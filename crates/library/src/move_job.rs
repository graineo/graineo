//! "Move to …" for large moves between locations and for merging into an existing folder (storage model, after
//! stage 6): e.g. the rest of `~/Pictures` (tens of thousands of photos, hundreds of GB) into the year folders that
//! already exist on the NAS.
//!
//! Unlike [`crate::move_folder::execute`] (all or nothing, one journal step per file, one transaction) a job works
//! **folder by folder** (each directory on its own, without its subfolders):
//! 1. look at the directory: files at the source, files at the target, library rows of both;
//! 2. per *unit* (a photo with its sidecars – XMP, graineo sidecar, camera JPEG – or a single other file):
//!    - nothing of that name at the target → verified copy (SHA-256, modification time kept); same volume: rename;
//!    - same name, same size → SHA-256 of both sides; equal → counts as already there (nothing is copied);
//!    - same name, other content (or only a different upper/lower case) → never overwritten: the whole unit stays
//!      at the source and is named in the report;
//! 3. small preview and offline copy of each photo are made from the local original (developed for RAWs, in a second
//!    thread one or two folders ahead) – before the library points to the target;
//! 4. one short transaction per directory: rows move to the target folder (same file and image IDs); a row whose file
//!    already had a row at the target is merged into it (rating of the more recently changed one, keywords,
//!    collections, faces, virtual copies) – never two rows for one file;
//! 5. previews move to their new key, then the source files of the directory go to the trash.
//!
//! Journal: `<journal>/move-jobs/<id>.json` (the job: directories done, figures, report) is saved after each
//! directory; `<id>.dir.json` (the directory in progress: own copies, then the planned switch) after each file. Cancel
//! (between files), quit, crash, a disconnected NAS or the minimum free space only *pause* the job: finished
//! directories stay moved, own copies of the directory in progress are known and are not copied again. "Stop" removes
//! the own copies of the directory in progress; the rest stays at the old place. While a job is running or paused the
//! sync leaves both root folders alone and the Lightroom import waits ([`touches`], [`active`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::db::Db;
use crate::move_folder::Dispose;
use crate::preview::{self, Cache};

fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// At most this many conflicts are listed by name in the report (all are counted).
const MAX_LISTED: usize = 2000;
/// Photos whose previews are made at the same time (a RAW development uses its own thread pool on top).
const PREVIEW_THREADS: usize = 3;
/// The preview thread works at most this many directories ahead of the copy.
const AHEAD: usize = 2;
/// Message of a job paused by the user.
pub const PAUSED_BY_USER: &str = "angehalten";
/// Start of the message of a job paused for space.
pub const PAUSED_FOR_SPACE: &str = "zu wenig freier Platz";

// ───────────────────────────────────────────── running and paused jobs (sync and Lightroom import wait)

/// Paths (source and target folder) of jobs that are running or paused, per job.
static PENDING: Mutex<Vec<(String, Vec<PathBuf>)>> = Mutex::new(Vec::new());
/// Is a job running in this process right now (not only paused)?
static RUNNING: AtomicBool = AtomicBool::new(false);

fn set_pending(id: &str, paths: Option<Vec<PathBuf>>) {
    let mut p = PENDING.lock().unwrap();
    p.retain(|(i, _)| i != id);
    if let Some(v) = paths {
        p.push((id.to_string(), v));
    }
}

/// Is a job running or paused (not finished)? The Lightroom import waits meanwhile.
pub fn active() -> bool {
    !PENDING.lock().unwrap().is_empty()
}

/// Is a job running right now in this process?
pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Does a running or paused job touch this root folder (source or target inside it, or it inside them)?
pub fn touches(root: &str) -> bool {
    let r = Path::new(root);
    PENDING.lock().unwrap().iter().any(|(_, v)| v.iter().any(|p| p.starts_with(r) || r.starts_with(p)))
}

struct RunningGuard;

impl RunningGuard {
    fn new() -> Result<RunningGuard, String> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err("Ein anderes Verschieben läuft bereits".into());
        }
        Ok(RunningGuard)
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

// ───────────────────────────────────────────── helpers

fn with_slash(p: &Path) -> String {
    format!("{}/", crate::nfc(p.to_string_lossy().trim_end_matches('/')))
}

/// Resolved path for comparing (without the Windows `\\?\` prefix, so resolved and unresolved paths compare equal).
fn canon(p: &Path) -> PathBuf {
    sysdirs::simplify(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
}

/// Nearest existing folder (the target may not exist yet).
fn existing_ancestor(p: &Path) -> PathBuf {
    p.ancestors().find(|a| a.exists()).map(Path::to_path_buf).unwrap_or_else(|| p.to_path_buf())
}

fn same_device(a: &Path, b: &Path) -> bool {
    if std::env::var_os("GRAINEO_MOVE_COPY").is_some() {
        return false;
    }
    match (crate::platform::device(a), crate::platform::device(&existing_ancestor(b))) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Own side files of an interrupted copy (see [`crate::files::partial_copy_stem`]).
fn is_side_copy(name: &str) -> bool {
    crate::files::partial_copy_stem(name).is_some()
}

fn lower(s: &str) -> String {
    crate::nfc(s).to_lowercase()
}

fn secs(m: &std::fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64)
}

/// Folders that never move along: packages of other programs (Apple Photos library, Lightroom previews, apps …) and
/// folders holding a Lightroom catalog. They stay where they are and are named in the confirmation.
fn kept_folder(dir: &Path, name: &str) -> bool {
    if crate::write_target::is_package_name(name) || crate::apple_photos::in_photos_library(&dir.join(name)) {
        return true;
    }
    std::fs::read_dir(dir.join(name)).is_ok_and(|rd| rd.flatten().any(|e| catalog_file(&e.file_name().to_string_lossy())))
}

/// Files of a Lightroom catalog (catalog, its journal, lock): never moved.
fn catalog_file(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(".lrcat") || l.contains(".lrcat-") || l.contains(".lrcat.")
}

/// Directories below `dir` (relative, with '/' at the end; "" for `dir` itself), sorted, and the folders kept out
/// ([`kept_folder`]). Links are refused; `skip`: a folder that stays out (the trash folder of a network drive inside
/// the folder).
fn walk_dirs(dir: &Path, skip: Option<&Path>) -> Result<(Vec<String>, Vec<String>), String> {
    let mut out = Vec::new();
    let mut kept = Vec::new();
    let mut todo = vec![String::new()];
    while let Some(rel) = todo.pop() {
        let abs = dir.join(&rel);
        if skip.is_some_and(|s| abs == s || abs.starts_with(s)) {
            continue;
        }
        let rd = std::fs::read_dir(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        for e in rd {
            let e = e.map_err(|e| format!("{}: {e}", abs.display()))?;
            let ft = e.file_type().map_err(|x| format!("{}: {x}", e.path().display()))?;
            let name = e.file_name().to_string_lossy().to_string();
            if ft.is_symlink() {
                return Err(format!("Der Ordner enthält eine Verknüpfung ({}) – Ordner mit Verknüpfungen werden nicht verschoben", e.path().display()));
            }
            if ft.is_dir() {
                if kept_folder(&abs, &name) {
                    kept.push(format!("{rel}{name}"));
                } else {
                    todo.push(format!("{rel}{name}/"));
                }
            }
        }
        out.push(rel);
    }
    out.sort();
    kept.sort();
    Ok((out, kept))
}

/// Files directly in `dir` (name, size); `.DS_Store` and own side files stay out.
fn list_files(dir: &Path) -> std::io::Result<Vec<(String, u64)>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let ft = e.file_type()?;
        if !ft.is_file() {
            continue;
        }
        let n = e.file_name().to_string_lossy().to_string();
        if n == ".DS_Store" || is_side_copy(&n) || catalog_file(&n) {
            continue;
        }
        v.push((n, e.metadata()?.len()));
    }
    v.sort();
    Ok(v)
}

/// The target folder of a relative directory: existing folders with the same name in another Unicode form (NFC/NFD,
/// e.g. on a NAS) are used instead of making a second one.
fn resolve_target(target: &Path, rel: &str) -> PathBuf {
    let mut p = target.to_path_buf();
    for c in rel.split('/').filter(|c| !c.is_empty()) {
        let want = crate::nfc(c);
        let found = std::fs::read_dir(&p).ok().and_then(|rd| rd.flatten().find(|e| crate::nfc(&e.file_name().to_string_lossy()) == want).map(|e| e.file_name()));
        p = match found {
            Some(n) => p.join(n),
            None => p.join(&want),
        };
    }
    p
}

// ───────────────────────────────────────────── looking at one directory

/// A library row of the source directory.
#[derive(Clone, Debug)]
pub struct Row {
    pub file: i64,
    pub name: String,
    pub format: String,
    pub size: i64,
    pub mtime: i64,
    pub orientation: Option<i64>,
    pub state: i64,
}

impl Row {
    fn key_at(&self, path: &str, size: i64, mtime: i64) -> u64 {
        preview::cache_key_with_orientation(preview::cache_key(path, size, mtime), self.orientation)
    }
    fn job(&self, path: &Path, level: u32) -> preview::Job {
        preview::Job { file: self.file, path: path.to_path_buf(), format: self.format.clone(), key: self.key_at(&path.to_string_lossy(), self.size, self.mtime), level, orientation: self.orientation }
    }
}

fn rows_of_folder(conn: &rusqlite::Connection, folder: i64) -> Result<Vec<Row>, String> {
    let mut st = conn
        .prepare_cached(
            "SELECT d.id, d.name, d.format, d.size, d.mtime, d.orientation, d.status
             FROM file d WHERE d.folder = ?1 ORDER BY d.name",
        )
        .map_err(f)?;
    let r = st
        .query_map([folder], |r| {
            Ok(Row {
                file: r.get(0)?,
                name: r.get(1)?,
                format: r.get(2)?,
                size: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                mtime: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                orientation: r.get(5)?,
                state: r.get(6)?,
            })
        })
        .map_err(f)?
        .collect::<rusqlite::Result<Vec<Row>>>()
        .map_err(f);
    r
}

fn folder_id(conn: &rusqlite::Connection, root_path: &str, rel: &str) -> Option<i64> {
    conn.query_row("SELECT o.id FROM folder o JOIN root_folder s ON s.id = o.root WHERE s.path = ?1 AND o.path = ?2", params![root_path, rel], |r| r.get(0)).optional().ok().flatten()
}

/// What happens with one file.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// nothing of that name at the target
    Copy,
    /// same name and size at the target: compare checksums
    Same,
    /// already at the target (checked, or an own copy of an interrupted run)
    Present,
    /// stays at the source (reason)
    Conflict(String),
}

#[derive(Clone, Debug)]
struct FileWork {
    name: String,
    /// name at the target (an existing file in another Unicode form keeps its name)
    target_name: String,
    size: u64,
    kind: Kind,
}

#[derive(Clone, Debug, Default)]
struct Unit {
    files: Vec<FileWork>,
    photos: Vec<Row>,
}

impl Unit {
    fn conflict(&self) -> Option<String> {
        self.files.iter().find_map(|w| match &w.kind {
            Kind::Conflict(r) => Some(format!("{}: {r}", w.name)),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, Default)]
struct DirWork {
    units: Vec<Unit>,
    /// library rows whose file is missing (they stay at the old place)
    missing_rows: usize,
    /// target rows by NFC name (target folder of the library)
    target_rows: HashMap<String, i64>,
}

fn stem_of(n: &str) -> String {
    crate::nfc(&Path::new(n).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
}

/// Sidecars of the file `i` among the files of its directory (same rules as [`crate::relocate::companions`], from the
/// listing instead of reading the folder again): `NAME.EXT.xmp`, `NAME.xmp` (not for a JPEG with a RAW beside it), the
/// camera JPEG of a RAW.
fn companions_in(files: &[(String, u64)], i: usize) -> Vec<usize> {
    let name = crate::nfc(&files[i].0);
    let stem = stem_of(&name);
    let raw = crate::format_from_extension(&name).is_some_and(crate::is_raw);
    let raw_beside = !raw && files.iter().any(|(n, _)| stem_of(n) == stem && crate::format_from_extension(n).is_some_and(crate::is_raw));
    let (xmp, stem_xmp) = (format!("{name}.xmp").to_lowercase(), format!("{stem}.xmp").to_lowercase());
    let mut v = Vec::new();
    for (j, (n, _)) in files.iter().enumerate() {
        if j == i {
            continue;
        }
        let n = crate::nfc(n);
        let l = n.to_lowercase();
        if l == xmp || ((raw || !raw_beside) && l == stem_xmp) || (raw && stem_of(&n) == stem && crate::format_from_extension(&n) == Some("JPG")) {
            v.push(j);
        }
    }
    v
}

/// Look at one directory: `src_dir`/`tgt_dir` on disk, `rows` of the source folder, `target_rows` of the target
/// folder, `ours`: own copies of an interrupted run (count as present when their size matches).
fn look_at(src_dir: &Path, tgt_dir: &Path, rows: Vec<Row>, target_rows: HashMap<String, i64>, ours: &HashSet<String>, same_volume: bool) -> Result<DirWork, String> {
    let mut files = list_files(src_dir).map_err(|e| format!("{}: {e}", src_dir.display()))?;
    // same volume: own renames of an interrupted run are no longer at the source, but belong to the directory
    if same_volume && !ours.is_empty() {
        let here: HashSet<String> = files.iter().map(|(n, _)| crate::nfc(n)).collect();
        if let Ok(rd) = std::fs::read_dir(tgt_dir) {
            for e in rd.flatten() {
                let n = crate::nfc(&e.file_name().to_string_lossy());
                if ours.contains(&n) && !here.contains(&n) {
                    if let Ok(m) = e.metadata() {
                        files.push((n, m.len()));
                    }
                }
            }
        }
    }
    let by_nfc: HashMap<String, usize> = files.iter().enumerate().map(|(i, (n, _))| (crate::nfc(n), i)).collect();
    // units: a photo with its sidecars; overlapping ones (RAW + camera JPEG as an image of its own) become one
    let mut parent: Vec<usize> = (0..files.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut j = i;
        while p[j] != r {
            let n = p[j];
            p[j] = r;
            j = n;
        }
        r
    }
    let mut photo_of: HashMap<usize, Vec<Row>> = HashMap::new();
    let mut missing_rows = 0;
    for r in rows {
        let Some(&i) = by_nfc.get(&r.name) else {
            missing_rows += 1;
            continue;
        };
        for j in companions_in(&files, i) {
            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
            if a != b {
                parent[a] = b;
            }
        }
        photo_of.entry(i).or_default().push(r);
    }
    // the target directory: names by NFC and by lower case
    let mut tgt_nfc: HashMap<String, String> = HashMap::new();
    let mut tgt_lower: HashMap<String, String> = HashMap::new();
    match std::fs::read_dir(tgt_dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                tgt_lower.insert(lower(&n), n.clone());
                tgt_nfc.insert(crate::nfc(&n), n);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Ziel {}: {e}", tgt_dir.display())),
    }
    let mut groups: HashMap<usize, Unit> = HashMap::new();
    for (i, (n, size)) in files.iter().enumerate() {
        let key = crate::nfc(n);
        let (kind, target_name) = match tgt_nfc.get(&key) {
            Some(t) => {
                let k = match std::fs::symlink_metadata(tgt_dir.join(t)) {
                    Ok(m) if m.is_file() && m.len() == *size => {
                        if ours.contains(&key) {
                            Kind::Present
                        } else {
                            Kind::Same
                        }
                    }
                    Ok(m) if m.is_file() => Kind::Conflict("am Ziel liegt eine andere Datei gleichen Namens (andere Größe)".into()),
                    Ok(_) => Kind::Conflict("am Ziel liegt ein Ordner oder eine Verknüpfung gleichen Namens".into()),
                    Err(e) => Kind::Conflict(format!("Ziel nicht lesbar: {e}")),
                };
                (k, t.clone())
            }
            None => match tgt_lower.get(&lower(n)) {
                Some(t) => (Kind::Conflict(format!("am Ziel liegt „{t}“ (anders geschrieben)")), t.clone()),
                None => (Kind::Copy, key.clone()),
            },
        };
        let root = find(&mut parent, i);
        let u = groups.entry(root).or_default();
        u.files.push(FileWork { name: n.clone(), target_name, size: *size, kind });
        if let Some(p) = photo_of.remove(&i) {
            u.photos.extend(p);
        }
    }
    let mut units: Vec<Unit> = groups.into_values().collect();
    units.sort_by(|a, b| a.files[0].name.cmp(&b.files[0].name));
    Ok(DirWork { units, missing_rows, target_rows })
}

// ───────────────────────────────────────────── plan

/// Figures for the confirmation.
#[derive(Clone, Debug, Default)]
pub struct Figures {
    pub dirs: usize,
    /// photos (library rows) that move
    pub photos: usize,
    pub files: usize,
    pub bytes: u64,
    /// new at the target
    pub copy_files: usize,
    pub copy_bytes: u64,
    /// same name and size at the target (compared by checksum while running)
    pub same_files: usize,
    pub same_bytes: u64,
    /// same name with other content: stay (files of their units included)
    pub conflict_files: usize,
    pub conflicts: Vec<String>,
    /// library rows whose file is missing (stay)
    pub missing_rows: usize,
    /// photos whose file already has a row at the target (merged)
    pub merge_rows: usize,
    /// photos still without offline copy (only for a target on another drive)
    pub offline_missing: usize,
    /// RAWs among them (developed, ≈ 2–3 s each)
    pub offline_develop: usize,
    /// projected space of the missing offline copies on this Mac
    pub offline_bytes: u64,
    pub free_local: Option<u64>,
    pub free_target: Option<u64>,
    pub min_free: u64,
    /// folders that stay where they are (Apple Photos library, Lightroom catalog, packages of other programs)
    pub kept_folders: Vec<String>,
    /// Lightroom catalog files directly in a folder that moves (stay)
    pub kept_files: usize,
}

impl Figures {
    /// Space the offline copies lack beyond the minimum free space (`None`: they fit).
    pub fn offline_shortfall(&self) -> Option<u64> {
        preview::offline_shortfall(self.offline_bytes, 0, self.free_local?, self.min_free)
    }
}

/// Result of [`plan`].
#[derive(Clone, Debug)]
pub struct Plan {
    pub folder: i64,
    /// name of the source folder
    pub name: String,
    pub source: PathBuf,
    /// root folder of the source (path without '/')
    pub source_root: PathBuf,
    pub source_root_path: String,
    /// relative path of the source in its root ("" or "a/b/")
    pub source_rel: String,
    /// where the files land (exists: merge)
    pub target: PathBuf,
    pub merge: bool,
    /// the contents go into the chosen folder itself (not into a subfolder of the same name)
    pub contents: bool,
    pub same_volume: bool,
    /// target on another drive: offline copies are made
    pub offline: bool,
    /// root folder of the library containing the target (with '/'); `None`: a new root folder at the target
    pub target_root: Option<String>,
    /// directories (relative to the source, with '/'; "" = the folder itself) with (files, bytes)
    pub dirs: Vec<(String, usize, u64)>,
    pub figures: Figures,
}

/// Plan moving the folder `folder` into `dest` (as `dest/<name>`, merged if it exists) or – `contents` – its contents
/// into `dest` itself. Only reads (the write test creates and removes one empty own file).
pub fn plan(db: &Db, folder: i64, dest: &Path, contents: bool, cache: Option<&Cache>) -> Result<Plan, String> {
    let (root, rel, root_path, origin): (i64, String, String, String) = db
        .conn
        .query_row("SELECT o.root, o.path, s.path, s.origin FROM folder o JOIN root_folder s ON s.id = o.root WHERE o.id = ?1", [folder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()
        .map_err(f)?
        .ok_or("Ordner unbekannt")?;
    if origin == crate::apple_photos::ORIGIN || crate::apple_photos::in_photos_library(Path::new(&root_path)) {
        return Err("Ordner der Bibliothek von Apple Fotos werden nicht verschoben".into());
    }
    if crate::apple_photos::in_photos_library(dest) {
        return Err("Nie in die Bibliothek von Apple Fotos verschieben".into());
    }
    crate::write_target::check(dest)?;
    crate::write_target::probe(dest)?;
    let source = PathBuf::from(format!("{root_path}{rel}").trim_end_matches('/'));
    if !crate::location::root_reachable(&root_path) || !source.is_dir() {
        return Err(format!("{} ist nicht erreichbar – verschieben geht erst nach dem Verbinden", source.display()));
    }
    if !dest.is_dir() {
        return Err(format!("Ziel {} ist kein Ordner", dest.display()));
    }
    let name = source.file_name().map(|n| n.to_string_lossy().to_string()).ok_or("Ordner ohne Namen")?;
    let target = if contents { dest.to_path_buf() } else { resolve_target(dest, &name) };
    let (src_c, tgt_c) = (canon(&source), canon(&existing_ancestor(&target)));
    if tgt_c == src_c || tgt_c.starts_with(&src_c) {
        return Err("Ein Ordner lässt sich nicht in sich selbst verschieben".into());
    }
    if !contents && source.parent().is_some_and(|p| canon(p) == canon(dest)) {
        return Err(format!("„{name}“ liegt schon in {}", dest.display()));
    }
    let merge = match std::fs::symlink_metadata(&target) {
        Ok(m) if m.is_dir() => true,
        Ok(_) => return Err(format!("Am Ziel gibt es schon „{name}“ ({}), aber nicht als Ordner – nichts wird überschrieben", target.display())),
        Err(_) => false,
    };
    // root folders of the library
    let source_s = with_slash(&source);
    let target_s = with_slash(&target);
    let roots = db.roots().map_err(f)?;
    let network_trash = (!crate::watch::local(&source)).then(|| crate::network_trash::base(Path::new(&root_path))).filter(|t| t.starts_with(&source));
    let (dir_list, kept) = walk_dirs(&source, network_trash.as_deref())?;
    // another root folder inside: only inside a folder that stays anyway (e.g. the Apple Photos library)
    let inside_kept = |p: &str| kept.iter().any(|k| p.starts_with(&with_slash(&source.join(k))));
    if let Some(r) = roots.iter().find(|r| r.id != root && r.path.starts_with(&source_s) && !inside_kept(&r.path)) {
        return Err(format!("Im Ordner liegt ein weiterer Stammordner der Bibliothek ({}) – bitte zuerst diesen verschieben oder entfernen", r.path));
    }
    let target_root = roots.iter().filter(|r| target_s.starts_with(&r.path)).max_by_key(|r| r.path.len()).cloned();
    if let Some(b) = &target_root {
        if b.origin == crate::apple_photos::ORIGIN {
            return Err("Nie in die Bibliothek von Apple Fotos verschieben".into());
        }
    }
    if let Some(r) = roots.iter().find(|r| r.path.starts_with(&target_s) && r.path != target_s && target_root.as_ref().is_none_or(|b| b.id != r.id)) {
        return Err(format!("Am Ziel liegt der Stammordner {} der Bibliothek – bitte direkt in ihn verschieben", r.path));
    }
    let same_volume = same_device(&source, &target);
    let offline = !crate::platform::on_startup_volume(&existing_ancestor(&target));
    let large = preview::large();
    // target rows per target folder (relative path in the target root)
    let tgt_base = target_root.as_ref().map(|b| target_s[b.path.len()..].to_string());
    let mut fig = Figures { dirs: dir_list.len(), min_free: cache.map_or(preview::MIN_FREE, |c| c.min_free), kept_folders: kept, ..Default::default() };
    let mut dirs = Vec::new();
    for d in &dir_list {
        let src_dir = source.join(d);
        let tgt_dir = resolve_target(&target, d);
        fig.kept_files += std::fs::read_dir(&src_dir).map(|rd| rd.flatten().filter(|e| catalog_file(&e.file_name().to_string_lossy())).count()).unwrap_or(0);
        let rel_db = crate::nfc(&format!("{rel}{d}"));
        let rows = match folder_id(&db.conn, &root_path, &rel_db) {
            Some(id) => rows_of_folder(&db.conn, id)?,
            None => Vec::new(),
        };
        let target_rows = match (&target_root, &tgt_base) {
            (Some(b), Some(base)) => target_rows(&db.conn, &b.path, &crate::nfc(&format!("{base}{d}")))?,
            _ => HashMap::new(),
        };
        let w = look_at(&src_dir, &tgt_dir, rows, target_rows, &HashSet::new(), same_volume)?;
        let (mut n, mut b) = (0usize, 0u64);
        fig.missing_rows += w.missing_rows;
        for u in &w.units {
            n += u.files.len();
            b += u.files.iter().map(|x| x.size).sum::<u64>();
            if let Some(c) = u.conflict() {
                fig.conflict_files += u.files.len();
                if fig.conflicts.len() < MAX_LISTED {
                    fig.conflicts.push(format!("{d}{c}"));
                }
                continue;
            }
            for x in &u.files {
                match x.kind {
                    Kind::Copy => {
                        fig.copy_files += 1;
                        fig.copy_bytes += x.size;
                    }
                    _ => {
                        fig.same_files += 1;
                        fig.same_bytes += x.size;
                    }
                }
            }
            for p in &u.photos {
                fig.photos += 1;
                if w.target_rows.contains_key(&p.name) {
                    fig.merge_rows += 1;
                }
                if offline {
                    let path = src_dir.join(&p.name);
                    let key = p.job(&path, large).key;
                    let has = cache.is_some_and(|c| c.has(key, large) && (!preview::develops(&p.format) || c.is_developed(key, large)));
                    if !has {
                        fig.offline_missing += 1;
                        if crate::is_raw(&p.format) {
                            fig.offline_develop += 1;
                        }
                    }
                }
            }
        }
        fig.files += n;
        fig.bytes += b;
        dirs.push((d.clone(), n, b));
    }
    if fig.photos == 0 && fig.files == 0 {
        return Err("Im Ordner liegt nichts, was verschoben werden kann".into());
    }
    if offline {
        let usage = cache.map(|c| c.usage()).unwrap_or_default();
        fig.offline_bytes = preview::projected_offline(&usage, fig.offline_missing, large);
    }
    fig.free_local = cache.and_then(|c| c.free_space());
    if !same_volume {
        let fr = crate::free(&existing_ancestor(&target)).ok_or_else(|| format!("Freier Platz in {} unbekannt", dest.display()))?;
        fig.free_target = Some(fr);
        let need = fig.copy_bytes + (fig.copy_bytes / 100).max(64 << 20);
        if fr < need {
            return Err(format!("Am Ziel ist nicht genug Platz: {:.1} GB nötig, {:.1} GB frei", need as f64 / 1e9, fr as f64 / 1e9));
        }
    }
    write_test(&existing_ancestor(&target))?;
    Ok(Plan {
        folder,
        name,
        source,
        source_root: PathBuf::from(root_path.trim_end_matches('/')),
        source_root_path: root_path,
        source_rel: rel,
        target,
        merge,
        contents,
        same_volume,
        offline,
        target_root: target_root.map(|b| b.path),
        dirs,
        figures: fig,
    })
}

fn target_rows(conn: &rusqlite::Connection, root_path: &str, rel: &str) -> Result<HashMap<String, i64>, String> {
    let Some(id) = folder_id(conn, root_path, rel) else { return Ok(HashMap::new()) };
    let mut st = conn.prepare_cached("SELECT name, id FROM file WHERE folder = ?1").map_err(f)?;
    let r = st.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(f)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(f);
    r
}

fn write_test(dest: &Path) -> Result<(), String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let p = dest.join(format!(".graineo-write-test-{}-{nanos:x}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
        Ok(_) => {
            let _ = std::fs::remove_file(&p);
            Ok(())
        }
        Err(e) => Err(format!("In {} darf nicht geschrieben werden: {e}", dest.display())),
    }
}

/// Rough duration in seconds: copying at `bytes_per_s`, developing RAWs at `develop_s` each (parallel to the copy,
/// so the longer of both counts).
pub fn estimate_secs(fig: &Figures, bytes_per_s: f64, develop_s: f64, same_volume: bool) -> f64 {
    let copy = if same_volume { fig.files as f64 * 0.002 } else { fig.copy_bytes as f64 / bytes_per_s.max(1.0) + fig.same_bytes as f64 * 2.0 / bytes_per_s.max(1.0) };
    let prep = fig.offline_develop as f64 * develop_s + (fig.offline_missing - fig.offline_develop) as f64 * 0.2;
    copy.max(prep) + fig.dirs as f64 * 0.3
}

// ───────────────────────────────────────────── the job (journal)

/// Cumulated result of a job.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub photos_moved: usize,
    pub rows_merged: usize,
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub files_present: usize,
    pub bytes_present: u64,
    /// files counted so far (moved, present, conflicts) and their bytes – for the progress after a resume
    pub files_done: usize,
    pub bytes_done: u64,
    pub conflict_files: usize,
    pub conflicts: Vec<String>,
    pub missing_rows: usize,
    pub previews_made: usize,
    pub previews_moved: usize,
    pub without_preview: Vec<String>,
    pub notes: Vec<String>,
    /// time spent running (s)
    pub secs: f64,
}

impl Report {
    fn json(&self) -> Value {
        json!({
            "photosMoved": self.photos_moved, "rowsMerged": self.rows_merged, "filesCopied": self.files_copied,
            "bytesCopied": self.bytes_copied, "filesPresent": self.files_present, "bytesPresent": self.bytes_present,
            "filesDone": self.files_done, "bytesDone": self.bytes_done, "conflictFiles": self.conflict_files,
            "conflicts": self.conflicts, "missingRows": self.missing_rows,
            "previewsMade": self.previews_made, "previewsMoved": self.previews_moved,
            "withoutPreview": self.without_preview, "notes": self.notes, "secs": self.secs,
        })
    }
    fn from_json(v: &Value) -> Report {
        let n = |k: &str| v[k].as_u64().unwrap_or(0);
        let l = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        Report {
            photos_moved: n("photosMoved") as usize,
            rows_merged: n("rowsMerged") as usize,
            files_copied: n("filesCopied") as usize,
            bytes_copied: n("bytesCopied"),
            files_present: n("filesPresent") as usize,
            bytes_present: n("bytesPresent"),
            files_done: n("filesDone") as usize,
            bytes_done: n("bytesDone"),
            conflict_files: n("conflictFiles") as usize,
            conflicts: l("conflicts"),
            missing_rows: n("missingRows") as usize,
            previews_made: n("previewsMade") as usize,
            previews_moved: n("previewsMoved") as usize,
            without_preview: l("withoutPreview"),
            notes: l("notes"),
            secs: v["secs"].as_f64().unwrap_or(0.0),
        }
    }
}

/// A directory of the job.
#[derive(Clone, Debug, PartialEq)]
pub struct Dir {
    pub rel: String,
    pub files: usize,
    pub bytes: u64,
    pub done: bool,
}

/// A move job (saved in the journal).
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub name: String,
    pub source: PathBuf,
    pub source_root: PathBuf,
    pub source_root_path: String,
    pub source_rel: String,
    pub target: PathBuf,
    pub target_root: Option<String>,
    pub merge: bool,
    pub same_volume: bool,
    pub offline: bool,
    pub dirs: Vec<Dir>,
    /// "running", "paused", "finished", "stopped"
    pub status: String,
    /// why it is paused (or the last error)
    pub message: String,
    pub files_total: usize,
    pub bytes_total: u64,
    pub photos_total: usize,
    pub report: Report,
}

/// Folder of the job journals.
pub fn folder() -> PathBuf {
    crate::relocate::journal_folder().join("move-jobs")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_digit())
}

fn abs_ok(p: &Path) -> bool {
    use std::path::Component;
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

fn rel_ok(r: &str) -> bool {
    !r.starts_with('/') && !r.split('/').any(|c| c == ".." || c == ".") && !r.contains('\0')
}

impl Job {
    /// New job from a plan (not saved yet).
    pub fn new(plan: &Plan) -> Job {
        Job {
            id: crate::relocate::new_id(),
            title: format!("„{}“ nach {}", plan.name, plan.target.display()),
            name: plan.name.clone(),
            source: plan.source.clone(),
            source_root: plan.source_root.clone(),
            source_root_path: plan.source_root_path.clone(),
            source_rel: plan.source_rel.clone(),
            target: plan.target.clone(),
            target_root: plan.target_root.clone(),
            merge: plan.merge,
            same_volume: plan.same_volume,
            offline: plan.offline,
            dirs: plan.dirs.iter().map(|(r, n, b)| Dir { rel: r.clone(), files: *n, bytes: *b, done: false }).collect(),
            status: "paused".into(),
            message: String::new(),
            files_total: plan.figures.files,
            bytes_total: plan.figures.bytes,
            photos_total: plan.figures.photos,
            report: Report::default(),
        }
    }

    fn path(&self) -> PathBuf {
        folder().join(format!("{}.json", self.id))
    }

    fn dir_path(&self) -> PathBuf {
        folder().join(format!("{}.dir.json", self.id))
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id, "kind": "move_job", "title": self.title, "name": self.name, "source": self.source,
            "sourceRoot": self.source_root, "sourceRootPath": self.source_root_path, "sourceRel": self.source_rel,
            "target": self.target, "targetRoot": self.target_root, "merge": self.merge, "sameVolume": self.same_volume,
            "offline": self.offline, "status": self.status, "message": self.message,
            "filesTotal": self.files_total, "bytesTotal": self.bytes_total, "photosTotal": self.photos_total, "report": self.report.json(),
            "dirs": self.dirs.iter().map(|d| json!([d.rel, d.files, d.bytes, d.done])).collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &Value) -> Option<Job> {
        let id = v["id"].as_str().filter(|i| valid_id(i))?.to_string();
        let p = |k: &str| v[k].as_str().map(PathBuf::from).filter(|p| abs_ok(p));
        let dirs = v["dirs"]
            .as_array()?
            .iter()
            .map(|d| Some(Dir { rel: d[0].as_str().filter(|r| rel_ok(r))?.to_string(), files: d[1].as_u64()? as usize, bytes: d[2].as_u64()?, done: d[3].as_bool()? }))
            .collect::<Option<Vec<_>>>()?;
        Some(Job {
            id,
            title: v["title"].as_str().unwrap_or("").into(),
            name: v["name"].as_str().unwrap_or("").into(),
            source: p("source")?,
            source_root: p("sourceRoot")?,
            source_root_path: v["sourceRootPath"].as_str()?.into(),
            source_rel: v["sourceRel"].as_str().filter(|r| rel_ok(r))?.into(),
            target: p("target")?,
            target_root: v["targetRoot"].as_str().map(str::to_string),
            merge: v["merge"].as_bool().unwrap_or(false),
            same_volume: v["sameVolume"].as_bool().unwrap_or(false),
            offline: v["offline"].as_bool().unwrap_or(true),
            dirs,
            status: v["status"].as_str().unwrap_or("").into(),
            message: v["message"].as_str().unwrap_or("").into(),
            files_total: v["filesTotal"].as_u64().unwrap_or(0) as usize,
            bytes_total: v["bytesTotal"].as_u64().unwrap_or(0),
            photos_total: v["photosTotal"].as_u64().unwrap_or(0) as usize,
            report: Report::from_json(&v["report"]),
        })
    }

    /// Save the job (atomic) and register it for the sync lock.
    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(folder()).map_err(|e| e.to_string())?;
        crate::files::write_atomic(&self.path(), serde_json::to_string(&self.json()).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())?;
        set_pending(&self.id, self.open().then(|| vec![self.source.clone(), self.target.clone()]));
        Ok(())
    }

    /// Running or paused (not finished, not stopped)?
    pub fn open(&self) -> bool {
        matches!(self.status.as_str(), "running" | "paused")
    }

    pub fn dirs_done(&self) -> usize {
        self.dirs.iter().filter(|d| d.done).count()
    }
}

/// All jobs (newest first).
pub fn jobs() -> Vec<Job> {
    let mut v: Vec<Job> = std::fs::read_dir(folder())
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json") && !e.file_name().to_string_lossy().ends_with(".dir.json"))
                .filter_map(|e| Job::from_json(&serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

/// At startup: the open job (running at the last end = interrupted → paused), registered for the sync lock.
pub fn load_open() -> Option<Job> {
    let mut open = None;
    for mut j in jobs().into_iter().filter(Job::open) {
        if j.status == "running" {
            j.status = "paused".into();
            j.message = "unterbrochen (Programmende, Absturz oder Neustart)".into();
            let _ = j.save();
        } else {
            set_pending(&j.id, Some(vec![j.source.clone(), j.target.clone()]));
        }
        if open.is_none() {
            open = Some(j);
        }
    }
    open
}

/// The directory in progress (`<id>.dir.json`).
#[derive(Clone, Debug, Default)]
struct Current {
    rel: String,
    /// own copies (NFC names at the source) installed in this directory
    copied: Vec<String>,
    /// "copying" or "switching" (the library transaction is about to be committed or was committed)
    phase: String,
    /// switching: files whose rows move (to check whether the commit happened), preview keys (old, new) and the
    /// source files that go to the trash afterwards
    moved_rows: Vec<(i64, i64)>,
    rekey: Vec<(u64, u64)>,
    dispose: Vec<PathBuf>,
}

impl Current {
    fn load(job: &Job) -> Option<Current> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(job.dir_path()).ok()?).ok()?;
        let rel = v["rel"].as_str().filter(|r| rel_ok(r))?.to_string();
        Some(Current {
            rel,
            copied: v["copied"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            phase: v["phase"].as_str().unwrap_or("copying").into(),
            moved_rows: v["movedRows"].as_array().map(|a| a.iter().filter_map(|x| Some((x[0].as_i64()?, x[1].as_i64()?))).collect()).unwrap_or_default(),
            rekey: v["rekey"].as_array().map(|a| a.iter().filter_map(|x| Some((x[0].as_str()?.parse().ok()?, x[1].as_str()?.parse().ok()?))).collect()).unwrap_or_default(),
            dispose: v["dispose"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(PathBuf::from)).filter(|p| abs_ok(p)).collect()).unwrap_or_default(),
        })
    }
    fn save(&self, job: &Job) -> Result<(), String> {
        let v = json!({
            "rel": self.rel, "copied": self.copied, "phase": self.phase,
            "movedRows": self.moved_rows.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
            "rekey": self.rekey.iter().map(|(a, b)| json!([a.to_string(), b.to_string()])).collect::<Vec<_>>(),
            "dispose": self.dispose,
        });
        crate::files::write_atomic(&job.dir_path(), serde_json::to_string(&v).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())
    }
}

// ───────────────────────────────────────────── running

/// Phase of the directory in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Check,
    Copy,
    Previews,
    Switch,
    Trash,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub phase: Phase,
    pub dir: usize,
    pub dirs: usize,
    pub rel: String,
    pub files: usize,
    pub files_total: usize,
    pub bytes: u64,
    pub bytes_total: u64,
    /// photos with previews ready (the preview thread)
    pub prepared: usize,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    Finished,
    /// paused: by the user ([`PAUSED_BY_USER`]), for space (free bytes), or after an error (e.g. the NAS is gone)
    Paused(Pause),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pause {
    User,
    Space(u64),
    Error(String),
}

/// Free space on this Mac and the minimum that must stay free (bytes).
pub type Space<'a> = &'a (dyn Fn() -> (u64, u64) + Sync);

/// Standard space check: the volume of the preview store and its minimum free space.
pub fn space_of(cache: &Cache) -> impl Fn() -> (u64, u64) + Sync + '_ {
    move || (cache.free_space().unwrap_or(0), cache.min_free)
}

/// Make the previews of a photo: the small one, and – target on another drive – the offline copy (developed for
/// RAWs). Returns: (made something, complete).
fn prepare_photo(cache: &Cache, row: &Row, path: &Path, offline: bool) -> (bool, bool) {
    let (small, large) = (preview::small(), preview::large());
    let mut made = false;
    if offline {
        let j = row.job(path, large);
        if let Ok(true) = preview::ensure_offline(cache, &j) {
            made = true;
        }
    }
    let j = row.job(path, small);
    if let Ok(true) = preview::ensure(cache, &j) {
        made = true;
    }
    let complete = cache.has(j.key, small) && (!offline || cache.has(row.job(path, large).key, large));
    (made, complete)
}

/// Run (or continue) a job until it is finished or paused. `cancel` pauses between files; `space` is checked before
/// previews are written (below the minimum: paused). Never waits on the library longer than one short transaction per
/// directory.
pub fn run(db: &Db, job: &mut Job, cache: &Cache, cancel: &AtomicBool, progress: &(dyn Fn(&Progress) + Sync), dispose: Dispose, space: Space) -> Result<Stop, String> {
    crate::backup_view::refuse()?;
    let _g = RunningGuard::new()?;
    let t0 = std::time::Instant::now();
    job.status = "running".into();
    job.message.clear();
    job.save()?;
    if let Some(r) = db.conn.query_row("SELECT id FROM root_folder WHERE path = ?1", [&job.source_root_path], |r| r.get::<_, i64>(0)).optional().map_err(f)? {
        let _ = r;
        crate::store::start_writer_round();
    }
    // the target folder is made once at the start (its parent must be there); later a missing target means a
    // disconnected drive – never a folder made anew on this Mac in an empty mount point
    if !job.merge && job.dirs_done() == 0 && !job.target.exists() {
        std::fs::create_dir(&job.target).map_err(|e| format!("{}: {e}", job.target.display()))?;
    }
    let first = job.dirs.iter().position(|d| !d.done).unwrap_or(job.dirs.len());
    let at = AtomicUsize::new(first);
    let prepared_upto = AtomicUsize::new(first);
    let prepared = AtomicUsize::new(0);
    let made = AtomicUsize::new(0);
    let space_short = AtomicBool::new(false);
    let quit = AtomicBool::new(false);
    let pjob = job.clone();
    let stop = std::thread::scope(|scope| -> Result<Stop, String> {
        // the preview thread: the directory in progress and the next one, from the local originals
        let prep = {
            let (pjob, at, prepared_upto, prepared, made, space_short, quit) = (&pjob, &at, &prepared_upto, &prepared, &made, &space_short, &quit);
            let db_path = db.path.clone();
            scope.spawn(move || {
                let Ok(pdb) = Db::open(&db_path) else { return };
                // a few photos at a time (decoding, developing); low priority keeps the app smooth
                let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(PREVIEW_THREADS).thread_name(|i| format!("move-previews-{i}")).start_handler(|_| crate::platform::background_priority()).build() else { return };
                for i in first..pjob.dirs.len() {
                    while i >= at.load(Ordering::SeqCst) + AHEAD && !quit.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    if quit.load(Ordering::SeqCst) {
                        return;
                    }
                    let d = &pjob.dirs[i];
                    let src_dir = pjob.source.join(&d.rel);
                    let rel_db = crate::nfc(&format!("{}{}", pjob.source_rel, d.rel));
                    let rows = folder_id(&pdb.conn, &pjob.source_root_path, &rel_db).and_then(|id| rows_of_folder(&pdb.conn, id).ok()).unwrap_or_default();
                    let todo: Vec<&Row> = rows.iter().collect();
                    pool.install(|| {
                        use rayon::prelude::*;
                        todo.par_iter().for_each(|r| {
                            if quit.load(Ordering::SeqCst) || space_short.load(Ordering::SeqCst) {
                                return;
                            }
                            let (free, min) = space();
                            if free < min {
                                space_short.store(true, Ordering::SeqCst);
                                return;
                            }
                            let path = src_dir.join(&r.name);
                            if !path.is_file() {
                                return;
                            }
                            if prepare_photo(cache, r, &path, pjob.offline).0 {
                                made.fetch_add(1, Ordering::Relaxed);
                            }
                            prepared.fetch_add(1, Ordering::Relaxed);
                        });
                    });
                    if quit.load(Ordering::SeqCst) || space_short.load(Ordering::SeqCst) {
                        return;
                    }
                    prepared_upto.store(i + 1, Ordering::SeqCst);
                }
            })
        };
        let wait = |i: usize| -> Option<u64> {
            // wait for the preview thread (if it stopped, the directory makes its own)
            while prepared_upto.load(Ordering::SeqCst) <= i && !prep.is_finished() && !cancel.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let (free, min) = space();
            (space_short.load(Ordering::SeqCst) && free < min).then_some(free)
        };
        // also on a panic in the main loop: the preview thread ends (the scope waits for it)
        struct QuitOnDrop<'a>(&'a AtomicBool);
        impl Drop for QuitOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _q = QuitOnDrop(&quit);
        let result = (|| -> Result<Stop, String> {
            for i in first..job.dirs.len() {
                at.store(i, Ordering::SeqCst);
                if cancel.load(Ordering::SeqCst) {
                    return Ok(Stop::Paused(Pause::User));
                }
                if let Some(s) = do_dir(db, job, i, cache, cancel, progress, dispose, space, &|| wait(i), &prepared)? {
                    return Ok(Stop::Paused(s));
                }
            }
            Ok(Stop::Finished)
        })();
        drop(_q);
        result
    });
    job.report.previews_made += made.load(Ordering::Relaxed);
    job.report.secs += t0.elapsed().as_secs_f64();
    let stop = match stop {
        Ok(s) => s,
        Err(e) => Stop::Paused(Pause::Error(e)),
    };
    match &stop {
        Stop::Finished => {
            finish(db, job);
            job.status = "finished".into();
        }
        Stop::Paused(p) => {
            job.status = "paused".into();
            job.message = match p {
                Pause::User => PAUSED_BY_USER.into(),
                Pause::Space(free) => format!("{PAUSED_FOR_SPACE} auf diesem Mac ({:.1} GB frei)", *free as f64 / 1e9),
                Pause::Error(e) => e.clone(),
            };
        }
    }
    job.save()?;
    Ok(stop)
}

/// Was the library switch of this directory committed? (The first moved row lies in the target folder or was merged
/// away.)
fn committed(db: &Db, cur: &Current) -> bool {
    cur.moved_rows.first().is_none_or(|(file, folder)| match db.conn.query_row("SELECT folder FROM file WHERE id = ?1", [file], |r| r.get::<_, i64>(0)).optional() {
        Ok(Some(o)) => o == *folder,
        Ok(None) => true,
        Err(_) => false,
    })
}

/// Names (NFC) → paths of the files in a directory.
fn listing(dir: &Path) -> HashMap<String, PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| (crate::nfc(&e.file_name().to_string_lossy()), e.path())).collect()).unwrap_or_default()
}

/// One directory. Returns `Some(pause)` if the job has to pause before it is done.
#[allow(clippy::too_many_arguments)]
fn do_dir(db: &Db, job: &mut Job, i: usize, cache: &Cache, cancel: &AtomicBool, progress: &(dyn Fn(&Progress) + Sync), dispose: Dispose, space: Space, wait_previews: &dyn Fn() -> Option<u64>, prepared: &AtomicUsize) -> Result<Option<Pause>, String> {
    let rel = job.dirs[i].rel.clone();
    let src_dir = job.source.join(&rel);
    let tgt_dir = resolve_target(&job.target, &rel);
    let mut cur = Current::load(job).filter(|c| c.rel == rel).unwrap_or(Current { rel: rel.clone(), phase: "copying".into(), ..Default::default() });
    let base = Progress { dir: i, dirs: job.dirs.len(), rel: rel.clone(), files: job.report.files_done, files_total: job.files_total, bytes: job.report.bytes_done, bytes_total: job.bytes_total, ..Default::default() };
    let show = |p: Phase, files: usize, bytes: u64| progress(&Progress { phase: p, files, bytes, prepared: prepared.load(Ordering::Relaxed), ..base.clone() });
    // an interrupted switch: committed → only previews and trash are left
    if cur.phase == "switching" {
        if committed(db, &cur) {
            after_switch(job, i, cur, cache, dispose, &show)?;
            return Ok(None);
        }
        cur.phase = "copying".into();
        cur.moved_rows.clear();
        cur.rekey.clear();
        cur.dispose.clear();
    }
    show(Phase::Check, base.files, base.bytes);
    if !crate::location::root_reachable(&job.source_root_path) {
        // the source drive went away (network drive asleep, disk unplugged): pause, never count its folders as done
        return Ok(Some(Pause::Error(format!("Quelle {} ist nicht erreichbar – Laufwerk verbinden, dann fortsetzen", job.source_root_path))));
    }
    if !src_dir.is_dir() {
        // gone meanwhile: nothing left to do
        job.dirs[i].done = true;
        job.save()?;
        return Ok(None);
    }
    if !job.target.is_dir() {
        return Ok(Some(Pause::Error(format!("Ziel {} ist nicht erreichbar – Laufwerk verbinden, dann fortsetzen", job.target.display()))));
    }
    let rel_db = crate::nfc(&format!("{}{rel}", job.source_rel));
    let rows = match folder_id(&db.conn, &job.source_root_path, &rel_db) {
        Some(id) => rows_of_folder(&db.conn, id)?,
        None => Vec::new(),
    };
    let (target_root_path, tgt_rel) = target_place(job, &rel);
    let target_rows = match &target_root_path {
        Some(b) => target_rows(&db.conn, b, &tgt_rel)?,
        None => HashMap::new(),
    };
    let ours: HashSet<String> = cur.copied.iter().cloned().collect();
    let mut w = match look_at(&src_dir, &tgt_dir, rows, target_rows, &ours, job.same_volume) {
        Ok(w) => w,
        Err(e) => return Ok(Some(Pause::Error(e))),
    };
    let (mut files_done, mut bytes_done) = (base.files, base.bytes);
    // 1. same-named files: compare checksums; any difference keeps the whole unit at the source
    for u in w.units.iter_mut() {
        if u.conflict().is_some() {
            continue;
        }
        for k in 0..u.files.len() {
            if u.files[k].kind != Kind::Same {
                continue;
            }
            if cancel.load(Ordering::SeqCst) {
                return Ok(Some(Pause::User));
            }
            show(Phase::Check, files_done, bytes_done);
            let x = &u.files[k];
            match (crate::relocate::sha256(&src_dir.join(&x.name)), crate::relocate::sha256(&tgt_dir.join(&x.target_name))) {
                (Ok(a), Ok(b)) if a == b => u.files[k].kind = Kind::Present,
                (Ok(_), Ok(_)) => {
                    u.files[k].kind = Kind::Conflict("am Ziel liegt eine andere Datei gleichen Namens (gleiche Größe, anderer Inhalt)".into());
                    break;
                }
                (Err(e), _) => return Ok(Some(Pause::Error(e))),
                (_, Err(e)) => return Ok(Some(Pause::Error(format!("Ziel nicht lesbar: {e}")))),
            }
        }
    }
    let moving: Vec<usize> = (0..w.units.len()).filter(|&k| w.units[k].conflict().is_none()).collect();
    // 2. previews of the photos that move, from the local original – before anything leaves this place
    show(Phase::Previews, files_done, bytes_done);
    if let Some(free) = wait_previews() {
        return Ok(Some(Pause::Space(free)));
    }
    let mut without = Vec::new();
    for &k in &moving {
        for r in &w.units[k].photos {
            let path = src_dir.join(&r.name);
            if !path.is_file() {
                continue; // same volume, renamed by an interrupted run: its previews were made before
            }
            let (free, min) = space();
            if free < min {
                return Ok(Some(Pause::Space(free)));
            }
            let (made, complete) = prepare_photo(cache, r, &path, job.offline);
            if made {
                job.report.previews_made += 1;
            }
            if !complete {
                if cache.paused() {
                    return Ok(Some(Pause::Space(space().0)));
                }
                without.push(format!("{rel}{}", r.name));
            }
        }
    }
    // 3. copy (same volume: rename) every unit without a conflict
    for &k in &moving {
        for x in &w.units[k].files {
            if cancel.load(Ordering::SeqCst) {
                return Ok(Some(Pause::User));
            }
            if x.kind == Kind::Copy {
                let (from, to) = (src_dir.join(&x.name), tgt_dir.join(&x.target_name));
                std::fs::create_dir_all(&tgt_dir).map_err(|e| format!("{}: {e}", tgt_dir.display()))?;
                let b0 = bytes_done;
                let r = if job.same_volume {
                    // noted first: after a crash the renamed file is known as ours
                    cur.copied.push(crate::nfc(&x.name));
                    cur.save(job)?;
                    crate::files::rename_no_replace(&from, &to).map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))
                } else {
                    crate::relocate::verified_copy(&from, &to, &mut |b| show(Phase::Copy, files_done, b0 + b)).and_then(|tmp| {
                        crate::files::place_sibling_file(&tmp, &to).map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { format!("Ziel existiert schon: {}", to.display()) } else { format!("{}: {e}", to.display()) })
                    })
                };
                if let Err(e) = r {
                    // pause (e.g. the NAS is gone); own copies so far stay noted and count as present when resuming
                    return Ok(Some(Pause::Error(e)));
                }
                if !job.same_volume {
                    cur.copied.push(crate::nfc(&x.name));
                    cur.save(job)?;
                }
            }
            files_done += 1;
            bytes_done += x.size;
            show(Phase::Copy, files_done, bytes_done);
        }
    }
    // a source changed after its copy (e.g. a rating written into the XMP meanwhile): copy it once more
    if !job.same_volume {
        for &k in &moving {
            for x in w.units[k].files.iter().filter(|x| x.kind == Kind::Copy) {
                let (from, to) = (src_dir.join(&x.name), tgt_dir.join(&x.target_name));
                let (Ok(a), Ok(b)) = (std::fs::metadata(&from), std::fs::metadata(&to)) else { continue };
                if a.len() == b.len() && secs(&a) == secs(&b) {
                    continue;
                }
                // the new copy first (verified, next to the target), the old one goes only then: an interruption in
                // between never leaves the target without the file
                let tmp = crate::relocate::verified_copy(&from, &to, &mut |_| {})?;
                if let Err(e) = std::fs::remove_file(&to) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("{}: {e}", to.display()));
                }
                crate::files::place_sibling_file(&tmp, &to).map_err(|e| e.to_string())?;
            }
        }
    }
    // 4. the library: one short transaction for this directory
    show(Phase::Switch, files_done, bytes_done);
    let plan = switch_plan(job, &src_dir, &tgt_dir, &w, &moving);
    cur.phase = "switching".into();
    cur.dispose = plan.dispose.clone();
    cur.rekey.clear();
    cur.moved_rows.clear();
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let (root_path, folder) = ensure_target_folder(&tx, job, &rel)?;
    let folder_rel = tgt_rel_in(job, &rel, &root_path);
    cur.moved_rows = plan.moves.iter().map(|m| (m.row.file, folder)).collect();
    let mut merged = 0;
    for m in &plan.moves {
        let new_path = format!("{root_path}{folder_rel}{}", crate::nfc(&m.target_name));
        let old_key = m.row.key_at(&m.old_path, m.row.size, m.row.mtime);
        let existing: Option<(i64, Option<i64>)> = tx.query_row("SELECT id, orientation FROM file WHERE folder = ?1 AND name = ?2", params![folder, crate::nfc(&m.target_name)], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(f)?;
        let kept = match existing {
            Some((t, orient)) if t != m.row.file => {
                merge_rows(&tx, m.row.file, t)?;
                tx.execute("UPDATE file SET status = 0, size = ?2, mtime = ?3 WHERE id = ?1", params![t, m.size, m.mtime]).map_err(f)?;
                cur.rekey.push((old_key, preview::cache_key_with_orientation(preview::cache_key(&new_path, m.size, m.mtime), orient)));
                merged += 1;
                t
            }
            _ => {
                tx.execute("UPDATE file SET folder = ?2, name = ?3, status = 0, size = ?4, mtime = ?5 WHERE id = ?1", params![m.row.file, folder, crate::nfc(&m.target_name), m.size, m.mtime]).map_err(f)?;
                cur.rekey.push((old_key, m.row.key_at(&new_path, m.size, m.mtime)));
                m.row.file
            }
        };
        tx.execute("INSERT OR REPLACE INTO moved_files(file, job, old_path, moved_at) VALUES(?1, ?2, ?3, ?4)", params![kept, job.id, m.old_path, crate::now()]).map_err(f)?;
    }
    // noted before the commit: after a crash the next run sees whether it happened ([`committed`])
    cur.save(job)?;
    tx.commit().map_err(f)?;
    job.report.photos_moved += plan.moves.len();
    job.report.rows_merged += merged;
    for &k in &moving {
        for x in &w.units[k].files {
            if x.kind == Kind::Present {
                job.report.files_present += 1;
                job.report.bytes_present += x.size;
            } else {
                job.report.files_copied += 1;
                job.report.bytes_copied += x.size;
            }
        }
    }
    for u in w.units.iter().filter(|u| u.conflict().is_some()) {
        job.report.conflict_files += u.files.len();
        files_done += u.files.len();
        bytes_done += u.files.iter().map(|x| x.size).sum::<u64>();
        if job.report.conflicts.len() < MAX_LISTED {
            job.report.conflicts.push(format!("{rel}{}", u.conflict().unwrap_or_default()));
        }
    }
    job.report.without_preview.extend(without.into_iter().take(MAX_LISTED.saturating_sub(job.report.without_preview.len())));
    job.report.missing_rows += w.missing_rows;
    job.report.files_done = files_done;
    job.report.bytes_done = bytes_done;
    w.units.clear();
    after_switch(job, i, cur, cache, dispose, &show)?;
    Ok(None)
}

/// Root folder (with '/') and relative folder path of a directory at the target; `None` root: not created yet.
fn target_place(job: &Job, rel: &str) -> (Option<String>, String) {
    let t = with_slash(&job.target);
    match &job.target_root {
        Some(b) => (Some(b.clone()), crate::nfc(&format!("{}{rel}", &t[b.len().min(t.len())..]))),
        None => (None, crate::nfc(rel)),
    }
}

fn tgt_rel_in(job: &Job, rel: &str, root_path: &str) -> String {
    let t = with_slash(&job.target);
    crate::nfc(&format!("{}{rel}", &t[root_path.len().min(t.len())..]))
}

/// Target root folder (created at the target if it lies in none) and the target folder of `rel`.
fn ensure_target_folder(tx: &rusqlite::Transaction, job: &mut Job, rel: &str) -> Result<(String, i64), String> {
    let root_path = match &job.target_root {
        Some(b) => b.clone(),
        None => {
            let p = with_slash(&job.target);
            let name = job.target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
            tx.execute("INSERT OR IGNORE INTO root_folder(path, name, origin) VALUES(?1, ?2, 'own')", params![p, name]).map_err(f)?;
            job.target_root = Some(p.clone());
            p
        }
    };
    let root: i64 = tx.query_row("SELECT id FROM root_folder WHERE path = ?1", [&root_path], |r| r.get(0)).map_err(f)?;
    let frel = tgt_rel_in(job, rel, &root_path);
    tx.execute("INSERT OR IGNORE INTO folder(root, path) VALUES(?1, ?2)", params![root, frel]).map_err(f)?;
    let folder: i64 = tx.query_row("SELECT id FROM folder WHERE root = ?1 AND path = ?2", params![root, frel], |r| r.get(0)).map_err(f)?;
    Ok((root_path, folder))
}

struct Move {
    row: Row,
    old_path: String,
    target_name: String,
    size: i64,
    mtime: i64,
}

struct SwitchPlan {
    moves: Vec<Move>,
    dispose: Vec<PathBuf>,
}

fn switch_plan(job: &Job, src_dir: &Path, tgt_dir: &Path, w: &DirWork, moved_units: &[usize]) -> SwitchPlan {
    let mut moves = Vec::new();
    let mut dispose = Vec::new();
    for &ui in moved_units {
        let u = &w.units[ui];
        for x in &u.files {
            if !job.same_volume || x.kind == Kind::Present {
                dispose.push(src_dir.join(&x.name));
            }
        }
        for r in &u.photos {
            let x = u.files.iter().find(|x| crate::nfc(&x.name) == r.name);
            let target_name = x.map_or(r.name.clone(), |x| x.target_name.clone());
            let (size, mtime) = std::fs::metadata(tgt_dir.join(&target_name)).map_or((r.size, r.mtime), |m| (m.len() as i64, secs(&m)));
            moves.push(Move { row: r.clone(), old_path: src_dir.join(&r.name).to_string_lossy().to_string(), target_name, size, mtime });
        }
    }
    SwitchPlan { moves, dispose }
}

/// After the commit: previews to their new keys, the source files into the trash, the directory done.
fn after_switch(job: &mut Job, i: usize, cur: Current, cache: &Cache, dispose: Dispose, show: &dyn Fn(Phase, usize, u64)) -> Result<(), String> {
    for (a, b) in &cur.rekey {
        job.report.previews_moved += cache.rekey(*a, *b);
    }
    show(Phase::Trash, job.report.files_done, job.report.bytes_done);
    let rel = &job.dirs[i].rel;
    let tgt_dir = resolve_target(&job.target, rel);
    // only what really is at the target goes (paranoia: same size there)
    let there = listing(&tgt_dir);
    let list: Vec<PathBuf> = cur
        .dispose
        .iter()
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = crate::nfc(&p.file_name().unwrap_or_default().to_string_lossy());
            matches!((there.get(&n).and_then(|a| std::fs::metadata(a).ok()), std::fs::metadata(p)), (Some(a), Ok(b)) if a.len() == b.len())
        })
        .cloned()
        .collect();
    if !list.is_empty() {
        if let Err(e) = dispose(&list, &job.source_root) {
            job.report.notes.push(format!("{}: alte Dateien nicht in den Papierkorb ({e})", job.source.join(rel).display()));
        }
    }
    job.dirs[i].done = true;
    job.save()?;
    let _ = std::fs::remove_file(job.dir_path());
    Ok(())
}

/// Merge the row `s` (its file at the target already had the row `t`) into `t`, then remove `s`. Rules:
/// stars/flag/label of the more recently changed image, keywords and collections
/// united, caption filled if empty, faces: `t` keeps its own (if it has none, those of `s` move over, otherwise only
/// faces of persons `t` does not show yet), virtual copies, AI data and vectors move over if `t` has none. Lightroom
/// IDs move over if `t` has none (otherwise `s` is noted as removed, so that the import does not recreate it).
pub(crate) fn merge_rows(tx: &rusqlite::Transaction, s: i64, t: i64) -> Result<(), String> {
    let original = |d: i64| tx.query_row("SELECT id FROM image WHERE file = ?1 AND copy IS NULL", [d], |r| r.get::<_, i64>(0)).optional().map_err(f);
    let (si, ti) = (original(s)?, original(t)?);
    match (si, ti) {
        (Some(si), None) => {
            tx.execute("UPDATE image SET file = ?2 WHERE id = ?1", params![si, t]).map_err(f)?;
        }
        (Some(si), Some(ti)) => {
            tx.execute("INSERT OR IGNORE INTO image_keyword(image, keyword) SELECT ?2, keyword FROM image_keyword WHERE image = ?1", params![si, ti]).map_err(f)?;
            tx.execute("INSERT OR IGNORE INTO collection_image(collection, image, position) SELECT collection, ?2, position FROM collection_image WHERE image = ?1", params![si, ti]).map_err(f)?;
            let rating = |b: i64| {
                tx.query_row("SELECT stars, flag, label, max(coalesce(rating_time, 0), coalesce(changed, 0), coalesce(lr_touch, 0)) FROM image WHERE id = ?1", [b], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))).map_err(f)
            };
            let (a, b) = (rating(ti)?, rating(si)?);
            if b.3 > a.3 && (a.0, a.1, a.2) != (b.0, b.1, b.2) {
                tx.execute(
                    "UPDATE image SET stars = v.stars, flag = v.flag, label = v.label, label_lr = v.label_lr, rating_time = v.rating_time,
                            changed = max(coalesce(image.changed, 0), coalesce(v.changed, 0))
                     FROM (SELECT stars, flag, label, label_lr, rating_time, changed FROM image WHERE id = ?1) AS v WHERE image.id = ?2",
                    params![si, ti],
                )
                .map_err(f)?;
            }
            tx.execute("UPDATE image SET caption = (SELECT caption FROM image WHERE id = ?1) WHERE id = ?2 AND coalesce(caption, '') = ''", params![si, ti]).map_err(f)?;
            tx.execute("UPDATE image SET keywords_time = max(coalesce(keywords_time, 0), (SELECT coalesce(keywords_time, 0) FROM image WHERE id = ?1)) WHERE id = ?2", params![si, ti]).map_err(f)?;
            let t_faces: i64 = tx.query_row("SELECT count(*) FROM face WHERE image = ?1", [ti], |r| r.get(0)).map_err(f)?;
            if t_faces == 0 {
                tx.execute("UPDATE face SET image = ?2 WHERE image = ?1", params![si, ti]).map_err(f)?;
            } else {
                tx.execute("UPDATE face SET image = ?2 WHERE image = ?1 AND person IS NOT NULL AND person NOT IN (SELECT person FROM face WHERE image = ?2 AND person IS NOT NULL)", params![si, ti]).map_err(f)?;
            }
            let lr: (Option<i64>, Option<i64>) = (tx.query_row("SELECT lr_id FROM image WHERE id = ?1", [si], |r| r.get(0)).map_err(f)?, tx.query_row("SELECT lr_id FROM image WHERE id = ?1", [ti], |r| r.get(0)).map_err(f)?);
            if let (Some(l), None) = lr {
                tx.execute("UPDATE image SET lr_id = NULL WHERE id = ?1", [si]).map_err(f)?;
                tx.execute("UPDATE image SET lr_id = ?2, lr_state = (SELECT lr_state FROM image WHERE id = ?1) WHERE id = ?3", params![si, l, ti]).map_err(f)?;
            }
            tx.execute("DELETE FROM image WHERE id = ?1", [si]).map_err(f)?;
        }
        _ => {}
    }
    tx.execute("UPDATE image SET file = ?2 WHERE file = ?1 AND copy IS NOT NULL", params![s, t]).map_err(f)?;
    for table in ["ai", "image_vector", "text_vector", "text_vector_model", "image_vector_model"] {
        tx.execute(&format!("UPDATE OR IGNORE {table} SET file = ?2 WHERE file = ?1"), params![s, t]).map_err(f)?;
    }
    let lr: (Option<i64>, Option<i64>) = (tx.query_row("SELECT lr_id FROM file WHERE id = ?1", [s], |r| r.get(0)).map_err(f)?, tx.query_row("SELECT lr_id FROM file WHERE id = ?1", [t], |r| r.get(0)).map_err(f)?);
    match lr {
        (Some(l), None) => {
            tx.execute("UPDATE file SET lr_id = NULL WHERE id = ?1", [s]).map_err(f)?;
            tx.execute("UPDATE file SET lr_id = ?2 WHERE id = ?1", params![t, l]).map_err(f)?;
        }
        (Some(l), Some(_)) => {
            tx.execute(
                "INSERT OR REPLACE INTO removed_lr_file(lr_id, path, time) SELECT ?2, s.path || o.path || d.name, ?3 FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1",
                params![s, l, crate::now()],
            )
            .map_err(f)?;
        }
        _ => {}
    }
    // own develop settings live only in the library (sidecars are optional): they move over before the row of `s`
    // goes (its `develop_state` rows would go with it by ON DELETE CASCADE). Virtual copies: theirs move with them
    // (a copy of the same name at `t` keeps its own). Original: `t` takes them where it has none, or where those of
    // `s` are newer; Lightroom data only where `t` has none.
    tx.execute("UPDATE OR IGNORE develop_state SET file = ?2 WHERE file = ?1 AND copy <> ''", params![s, t]).map_err(f)?;
    let t_has: bool = tx.query_row("SELECT count(*) FROM develop_state WHERE file = ?1 AND copy = ''", [t], |r| r.get::<_, i64>(0)).map_err(f)? > 0;
    // the develop history goes with the develop state it belongs to
    let s_wins: bool = !t_has
        || tx
            .query_row(
                "SELECT count(*) FROM develop_state a JOIN develop_state b ON b.file = ?2 AND b.copy = ''
                 WHERE a.file = ?1 AND a.copy = '' AND a.settings IS NOT NULL
                   AND (b.settings IS NULL OR coalesce(a.settings_time, 0) > coalesce(b.settings_time, 0))",
                params![s, t],
                |r| r.get::<_, i64>(0),
            )
            .map_err(f)?
            > 0;
    crate::develop_history::merge_file(tx, s, t, s_wins).map_err(f)?;
    if !t_has {
        tx.execute("UPDATE develop_state SET file = ?2 WHERE file = ?1 AND copy = ''", params![s, t]).map_err(f)?;
    } else {
        tx.execute(
            "UPDATE develop_state SET settings = v.settings, settings_time = v.settings_time
             FROM (SELECT settings, settings_time FROM develop_state WHERE file = ?1 AND copy = '' AND settings IS NOT NULL) AS v
             WHERE develop_state.file = ?2 AND develop_state.copy = ''
               AND (develop_state.settings IS NULL OR coalesce(v.settings_time, 0) > coalesce(develop_state.settings_time, 0))",
            params![s, t],
        )
        .map_err(f)?;
        tx.execute(
            "UPDATE develop_state SET lr_develop = v.lr_develop, lr_orientation = v.lr_orientation, lr_edited = v.lr_edited, lr_file = v.lr_file
             FROM (SELECT lr_develop, lr_orientation, lr_edited, lr_file FROM develop_state WHERE file = ?1 AND copy = '' AND lr_develop IS NOT NULL) AS v
             WHERE develop_state.file = ?2 AND develop_state.copy = '' AND develop_state.lr_develop IS NULL",
            params![s, t],
        )
        .map_err(f)?;
    }
    // a sidecar write still waiting for `s` is due for `t` now
    tx.execute("INSERT OR IGNORE INTO pending_sidecar_write(file, since, baseline) SELECT ?2, since, baseline FROM pending_sidecar_write WHERE file = ?1", params![s, t]).map_err(f)?;
    tx.execute("DELETE FROM file WHERE id = ?1", [s]).map_err(f)?;
    Ok(())
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    /// Merging a moved photo into the row already at the target keeps its own develop settings (original and virtual
    /// copies) – they live only in the library; before 0.108 they went with the source row (ON DELETE CASCADE).
    #[test]
    fn merge_keeps_develop_settings() {
        let p = std::env::temp_dir().join(format!("graineo-merge-dev-{}-{}.sqlite", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let db = Db::open(&p).unwrap();
        let root = db.create_root("/tmp/graineo-merge/", "m", "own", None).unwrap();
        let a = db.create_folder(root, "a/", None).unwrap();
        let b = db.create_folder(root, "b/", None).unwrap();
        let file = |folder: i64| {
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'x.jpg', 'JPG')", [folder]).unwrap();
            let d = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file) VALUES(?1)", [d]).unwrap();
            d
        };
        let (s, t) = (file(a), file(b));
        db.conn.execute("INSERT INTO image(file, copy) VALUES(?1, 'Kopie 1')", [s]).unwrap();
        db.conn.execute("INSERT INTO develop_state(file, copy, settings, settings_time) VALUES(?1, '', '{\"own\":1}', 100), (?1, 'Kopie 1', '{\"copy\":1}', 100)", [s]).unwrap();
        db.conn.execute("INSERT INTO develop_state(file, copy, lr_develop) VALUES(?1, '', 'lr')", [t]).unwrap();
        db.conn.execute("INSERT INTO develop_step(file, copy, seq, time, kind, patch) VALUES(?1, '', 0, 1, 'start', '{}'), (?1, 'Kopie 1', 0, 1, 'start', '{}')", [s]).unwrap();
        let tx = db.conn.unchecked_transaction().unwrap();
        merge_rows(&tx, s, t).unwrap();
        tx.commit().unwrap();
        let get = |copy: &str| db.conn.query_row("SELECT settings, lr_develop FROM develop_state WHERE file = ?1 AND copy = ?2", params![t, copy], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))).unwrap();
        assert_eq!(get(""), (Some("{\"own\":1}".into()), Some("lr".into())));
        assert_eq!(get("Kopie 1").0.as_deref(), Some("{\"copy\":1}"));
        // the histories go with the develop states
        let steps: i64 = db.conn.query_row("SELECT count(*) FROM develop_step WHERE file = ?1", [t], |r| r.get(0)).unwrap();
        assert_eq!(steps, 2);
        drop(db);
        for x in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{x}", p.display()));
        }
    }
}

/// The job is finished: empty source folders go (only empty ones, bottom up), and so do their library entries; a
/// source root folder without anything left is removed from the library.
fn finish(db: &Db, job: &Job) {
    remove_empty_tree(&job.source);
    let Ok(root) = db.conn.query_row("SELECT id FROM root_folder WHERE path = ?1", [&job.source_root_path], |r| r.get::<_, i64>(0)) else { return };
    let folders: Vec<(i64, String)> = db
        .conn
        .prepare("SELECT id, path FROM folder WHERE root = ?1 AND substr(path, 1, length(?2)) = ?2 AND NOT EXISTS (SELECT 1 FROM file d WHERE d.folder = folder.id)")
        .and_then(|mut st| st.query_map(params![root, job.source_rel], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default();
    for (id, rel) in folders {
        if !Path::new(&format!("{}{rel}", job.source_root_path)).exists() {
            let _ = db.conn.execute("DELETE FROM folder WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM file d WHERE d.folder = ?1)", [id]);
        }
    }
    if !Path::new(&job.source_root_path).exists() {
        let _ = db.conn.execute("DELETE FROM root_folder WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM folder o WHERE o.root = ?1)", [root]);
    }
}

/// Remove empty folders below and including `dir` (bottom up) plus own side files of an interrupted copy.
fn remove_empty_tree(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => remove_empty_tree(&p),
            Ok(t) if t.is_file() && is_side_copy(&e.file_name().to_string_lossy()) => {
                let _ = std::fs::remove_file(&p);
            }
            Ok(t) if t.is_file() && e.file_name() == ".DS_Store" => {
                // the Finder's own file only: a folder holding nothing else counts as empty
                if std::fs::read_dir(dir).map(|r| r.count()).unwrap_or(0) == 1 {
                    let _ = std::fs::remove_file(&p);
                }
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_dir(dir); // only if empty
}

/// Stop a paused job for good: the own copies of the directory in progress are removed again (only where the source
/// is still there with the same size), a committed directory is finished. Finished directories stay moved; the rest
/// stays at the old place.
pub fn stop(db: &Db, job: &mut Job, cache: &Cache, dispose: Dispose) -> Result<(), String> {
    if running() {
        return Err("Das Verschieben läuft noch – zuerst anhalten".into());
    }
    if let Some(cur) = Current::load(job) {
        if let Some(i) = job.dirs.iter().position(|d| d.rel == cur.rel) {
            if cur.phase == "switching" && committed(db, &cur) {
                after_switch(job, i, cur, cache, dispose, &|_, _, _| {})?;
            } else {
                let src_dir = job.source.join(&cur.rel);
                let tgt_dir = resolve_target(&job.target, &cur.rel);
                let (there, here) = (listing(&tgt_dir), listing(&src_dir));
                for n in &cur.copied {
                    match (there.get(n).cloned(), here.get(n).cloned()) {
                        (Some(t), Some(s)) if !job.same_volume => {
                            if std::fs::metadata(&t).ok().map(|m| m.len()) == std::fs::metadata(&s).ok().map(|m| m.len()) {
                                std::fs::remove_file(&t).map_err(|e| format!("{}: {e}", t.display()))?;
                            }
                        }
                        (Some(t), None) if job.same_volume => {
                            let back = src_dir.join(t.file_name().unwrap_or_default());
                            crate::files::rename_no_replace(&t, &back).map_err(|e| format!("{}: {e}", t.display()))?;
                        }
                        _ => {}
                    }
                }
                let _ = std::fs::remove_file(job.dir_path());
                remove_empty_tree(&tgt_dir);
            }
        }
    }
    job.status = "stopped".into();
    job.save()
}
