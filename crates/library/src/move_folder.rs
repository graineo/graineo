//! "Move to …" for a folder of the library: to another place on the same volume (rename) or to another location
//! (another volume, the NAS, an external drive) and back.
//!
//! Sequence between locations:
//! 1. [`plan`]: the folder (with subfolders) is read once; checks: target name free, no folder of the same name in the
//!    library there, space, write permission, no links.
//! 2. [`prepare`]: small and large previews of all photos into the cache (and, through the app, the developed large
//!    preview of edited photos), so that they are visible right away later even if the new location is not reachable.
//! 3. [`execute`]: every file (originals, XMP, `.graineo`, everything else in the folder) is copied with a SHA-256
//!    check and the modification time kept ([`crate::relocate`], journal), the source stays. Then the library is
//!    switched over in one transaction (same file and image IDs: ratings, keywords, collections, development, faces and
//!    virtual copies stay), the previews move to their new key, and only then the source goes to the trash.
//!    Cancel or error before the switch: the copies are removed again, the source stays unchanged.
//! 4. Same volume: one rename of the folder (atomic), no trash.
//!
//! Library model: a whole root folder moved outside the other root folders keeps its root and gets the new path. A
//! folder that lands inside a root folder becomes part of it; a subfolder moved outside all root folders becomes a
//! root folder of its own. A root folder moved into another one is merged into it (its own entry is removed).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use rusqlite::{params, OptionalExtension};

use crate::db::Db;
use crate::preview::{self, Cache, Job};
use crate::relocate::{self, Change, Operation, Step};

/// `Operation::kind` of a folder move.
pub const KIND: &str = "move_folder";

fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Reserve on the target beyond the bytes of the folder: 1 % or at least 64 MB.
fn reserve(bytes: u64) -> u64 {
    (bytes / 100).max(64 << 20)
}

/// A photo of the folder (library entry).
#[derive(Clone, Debug)]
pub struct Photo {
    pub file: i64,
    pub format: String,
    /// path before the move, as the library forms it (root + folder + name)
    pub path: PathBuf,
    pub size: i64,
    pub mtime: i64,
    pub orientation: Option<i64>,
}

impl Photo {
    /// Key of its previews (same formula as the library view).
    pub fn key_at(&self, path: &str) -> u64 {
        preview::cache_key_with_orientation(preview::cache_key(path, self.size, self.mtime), self.orientation)
    }

    pub fn key(&self) -> u64 {
        self.key_at(&self.path.to_string_lossy())
    }

    pub fn job(&self, level: u32) -> Job {
        Job { file: self.file, path: self.path.clone(), format: self.format.clone(), key: self.key(), level, orientation: self.orientation }
    }
}

/// Result of [`plan`]: what will happen (for the confirmation).
#[derive(Clone, Debug)]
pub struct Plan {
    pub folder: i64,
    /// folder name (stays)
    pub name: String,
    pub source: PathBuf,
    /// root folder of the source (for the trash of network drives)
    pub source_root: PathBuf,
    /// chosen place
    pub dest: PathBuf,
    /// new folder: `dest/name`
    pub target: PathBuf,
    pub same_volume: bool,
    pub photos: Vec<Photo>,
    /// all files of the folder (relative path, bytes)
    pub files: Vec<(PathBuf, u64)>,
    pub bytes: u64,
    /// free space at the target (between locations)
    pub free: Option<u64>,
    pub changes: Vec<Change>,
}

/// Paths of moves running right now: the sync leaves root folders touching them alone ([`touches_busy`]).
static BUSY: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

struct BusyGuard(Vec<PathBuf>);

impl BusyGuard {
    fn new(paths: Vec<PathBuf>) -> BusyGuard {
        BUSY.lock().unwrap_or_else(|e| e.into_inner()).extend(paths.iter().cloned());
        BusyGuard(paths)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        // never panic in Drop (after a panic with the lock held this would abort the process)
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        for p in &self.0 {
            if let Some(i) = b.iter().position(|x| x == p) {
                b.remove(i);
            }
        }
    }
}

/// Is a folder move running right now?
pub fn busy() -> bool {
    !BUSY.lock().unwrap_or_else(|e| e.into_inner()).is_empty() || crate::move_job::running()
}

/// Does a running move touch this root folder (source or target inside it, or it inside them)?
pub fn touches_busy(root: &str) -> bool {
    let r = Path::new(root);
    BUSY.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p.starts_with(r) || r.starts_with(p)) || crate::move_job::touches(root)
}

fn with_slash(p: &Path) -> String {
    format!("{}/", crate::nfc(p.to_string_lossy().trim_end_matches('/')))
}

fn same_device(a: &Path, b: &Path) -> bool {
    if std::env::var_os("GRAINEO_MOVE_COPY").is_some() {
        return false;
    }
    match (crate::platform::device(a), crate::platform::device(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Own side files of an interrupted copy (see [`crate::files::partial_copy_stem`]).
fn is_side_copy(name: &str) -> bool {
    crate::files::partial_copy_stem(name).is_some()
}

/// All files below `dir` (relative path, bytes). `.DS_Store` stays out (the Finder makes it again); links are refused.
/// `skip`: folder that stays out (the trash folder of a network drive inside the folder).
fn walk(dir: &Path, skip: Option<&Path>) -> Result<Vec<(PathBuf, u64)>, String> {
    let mut out = Vec::new();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        let abs = dir.join(&rel);
        if skip.is_some_and(|s| abs == s) {
            continue;
        }
        let rd = std::fs::read_dir(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        for e in rd {
            let e = e.map_err(|e| format!("{}: {e}", abs.display()))?;
            let name = e.file_name();
            let n = name.to_string_lossy();
            let ft = e.file_type().map_err(|x| format!("{}: {x}", e.path().display()))?;
            if ft.is_symlink() {
                return Err(format!("Der Ordner enthält eine Verknüpfung ({}) – Ordner mit Verknüpfungen werden nicht verschoben", e.path().display()));
            }
            if ft.is_dir() {
                todo.push(rel.join(&name));
            } else if ft.is_file() {
                if n == ".DS_Store" || is_side_copy(&n) {
                    continue;
                }
                let m = e.metadata().map_err(|x| format!("{}: {x}", e.path().display()))?;
                out.push((rel.join(&name), m.len()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Photos of a folder with its subfolders: (file, format, path, size, mtime, orientation).
type Row = (i64, String, String, Option<i64>, Option<i64>, Option<i64>);

fn rows(db: &Db, root: i64, rel: &str) -> Result<Vec<Row>, String> {
    let mut st = db
        .conn
        .prepare(
            "SELECT d.id, d.format, s.path || o.path || d.name, d.size, d.mtime, d.orientation
             FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
             WHERE o.root = ?1 AND substr(o.path, 1, length(?2)) = ?2 ORDER BY d.id",
        )
        .map_err(f)?;
    let r = st
        .query_map(params![root, rel], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .map_err(f)?
        .collect::<rusqlite::Result<Vec<Row>>>()
        .map_err(f);
    r
}

/// Plan the move of a folder (or root folder) into `dest` (it lands there as `dest/<name>`). Only reads; the write
/// test creates and removes one empty own file in `dest`.
pub fn plan(db: &Db, folder: i64, dest: &Path) -> Result<Plan, String> {
    let (root, rel, root_path, root_name, origin): (i64, String, String, String, String) = db
        .conn
        .query_row("SELECT o.root, o.path, s.path, s.name, s.origin FROM folder o JOIN root_folder s ON s.id = o.root WHERE o.id = ?1", [folder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
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
    let target = dest.join(&name);
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (src_c, dest_c) = (canon(&source), canon(dest));
    if dest_c == src_c || dest_c.starts_with(&src_c) {
        return Err("Ein Ordner lässt sich nicht in sich selbst verschieben".into());
    }
    if source.parent().is_some_and(|p| canon(p) == dest_c) {
        return Err(format!("„{name}“ liegt schon in {}", dest.display()));
    }
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(format!("Am Ziel gibt es schon „{name}“ ({}) – nichts wird überschrieben", target.display()));
    }
    let list = rows(db, root, &rel)?;
    let changes = library_changes(db, root, &rel, &root_path, &root_name, &origin, &source, &target, &name)?;
    let same_volume = same_device(&source, dest);
    // the trash folder of a network drive inside the folder stays out
    let network_trash = (!crate::watch::local(&source)).then(|| crate::network_trash::base(Path::new(&root_path))).filter(|t| t.starts_with(&source));
    let files = walk(&source, network_trash.as_deref())?;
    let bytes: u64 = files.iter().map(|x| x.1).sum();
    let mut free = None;
    if !same_volume {
        let fr = crate::free(dest).ok_or_else(|| format!("Freier Platz in {} unbekannt", dest.display()))?;
        free = Some(fr);
        if fr < bytes + reserve(bytes) {
            return Err(format!("Am Ziel ist nicht genug Platz: {:.1} GB nötig, {:.1} GB frei", (bytes + reserve(bytes)) as f64 / 1e9, fr as f64 / 1e9));
        }
    }
    write_test(dest)?;
    let photos = list
        .into_iter()
        .map(|r| Photo { file: r.0, format: r.1, path: PathBuf::from(r.2), size: r.3.unwrap_or(0), mtime: r.4.unwrap_or(0), orientation: r.5 })
        .collect();
    Ok(Plan { folder, name, source, source_root: PathBuf::from(root_path.trim_end_matches('/')), dest: dest.to_path_buf(), target, same_volume, photos, files, bytes, free, changes })
}

/// Library switch for a folder (root `root`, relative path `rel`, at `source`) that now lies at `target` (with the
/// checks shared by "Move to …" and "Locate missing folder"): no other root folder inside the folder, no root folder of
/// the library at or below the target, no known folder at the target. Model see the module description.
#[allow(clippy::too_many_arguments)]
pub(crate) fn library_changes(db: &Db, root: i64, rel: &str, root_path: &str, root_name: &str, origin: &str, source: &Path, target: &Path, name: &str) -> Result<Vec<Change>, String> {
    let source_s = with_slash(source);
    let target_s = with_slash(target);
    let roots = db.roots().map_err(f)?;
    if let Some(r) = roots.iter().find(|r| r.id != root && r.path.starts_with(&source_s)) {
        return Err(format!("Im Ordner liegt ein weiterer Stammordner der Bibliothek ({}) – bitte zuerst diesen verschieben oder entfernen", r.path));
    }
    if let Some(r) = roots.iter().find(|r| r.path.starts_with(&target_s)) {
        return Err(format!("Die Bibliothek kennt am Ziel schon den Stammordner {} – nichts wird überschrieben", r.path));
    }
    let into = roots.iter().find(|r| target_s.starts_with(&r.path)).cloned();
    if let Some(b) = &into {
        if b.origin == crate::apple_photos::ORIGIN {
            return Err("Nie in die Bibliothek von Apple Fotos verschieben".into());
        }
        let rel_b = &target_s[b.path.len()..];
        let n: i64 = db.conn.query_row("SELECT count(*) FROM folder WHERE root = ?1 AND substr(path, 1, length(?2)) = ?2", params![b.id, rel_b], |r| r.get(0)).map_err(f)?;
        if n > 0 {
            return Err(format!("Die Bibliothek kennt am Ziel schon einen Ordner „{name}“ – nichts wird überschrieben"));
        }
    }
    Ok(match (rel.is_empty(), &into) {
        (true, None) => vec![Change::RootPath(root, root_path.to_string(), target_s.clone())],
        (true, Some(b)) => vec![Change::Subtree(root_path.to_string(), String::new(), b.path.clone(), target_s[b.path.len()..].to_string()), Change::DropRoot(root_path.to_string(), root_name.to_string(), origin.to_string())],
        (false, None) => vec![Change::CreateRoot(target_s.clone(), name.to_string()), Change::Subtree(root_path.to_string(), rel.to_string(), target_s.clone(), String::new())],
        (false, Some(b)) => vec![Change::Subtree(root_path.to_string(), rel.to_string(), b.path.clone(), target_s[b.path.len()..].to_string())],
    })
}

/// May we write into `dest`? Creates an empty own file there and removes it again.
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

/// Phase of a running move (for the progress display).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Prepare,
    Copy,
    Switch,
    Cleanup,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    pub bytes: u64,
    pub bytes_total: u64,
}

/// Result of [`prepare`].
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    /// previews made now (small + large)
    pub made: usize,
    /// already there
    pub present: usize,
    /// developed large previews rendered now
    pub developed: usize,
    /// edited photos whose developed large preview was left out (limit)
    pub developed_skipped: usize,
    pub errors: Vec<String>,
    pub secs: f64,
}

/// What the app's develop callback did for a photo.
pub enum Developed {
    /// not edited or already present
    Nothing,
    Rendered,
    /// edited and missing, but not rendered (beyond [`MAX_DEVELOP`])
    Skipped,
    Failed(String),
}

/// At most this many developed large previews per move (each costs ≈ 0.5–3 s; beyond that the developed grid
/// preview stands in, reduced).
pub const MAX_DEVELOP: usize = 200;

/// Make sure small and large previews of all photos are in the cache. `develop` (from the app) renders the developed
/// large preview of an edited photo if it is missing and returns what it did; it is called at most [`MAX_DEVELOP`]
/// times with `true` (beyond that with `false`: only count). Cancel between photos.
pub fn prepare(plan: &Plan, cache: &Cache, cancel: &AtomicBool, progress: &dyn Fn(Progress), develop: &dyn Fn(&Photo, bool) -> Developed) -> Result<Prepared, String> {
    let t = std::time::Instant::now();
    let mut out = Prepared::default();
    let levels = [preview::small(), preview::large()];
    let total = plan.photos.len();
    for (i, p) in plan.photos.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(relocate::CANCELLED.into());
        }
        progress(Progress { phase: Phase::Prepare, done: i, total, ..Default::default() });
        match develop(p, out.developed < MAX_DEVELOP) {
            Developed::Nothing => {}
            Developed::Rendered => out.developed += 1,
            Developed::Skipped => out.developed_skipped += 1,
            Developed::Failed(e) => out.errors.push(format!("{}: {e}", p.path.display())),
        }
        for &l in &levels {
            match preview::ensure(cache, &p.job(l)) {
                Ok(true) => out.made += 1,
                Ok(false) => out.present += 1,
                Err(e) => {
                    if out.errors.len() < 200 {
                        out.errors.push(format!("{}: {e}", p.path.display()));
                    }
                }
            }
        }
    }
    progress(Progress { phase: Phase::Prepare, done: total, total, ..Default::default() });
    out.secs = t.elapsed().as_secs_f64();
    Ok(out)
}

/// Puts files into the trash: a list of files or (between local volumes) the whole source folder; second argument: the
/// former root folder (trash folder of network drives).
pub type Dispose<'a> = &'a dyn Fn(&[PathBuf], &Path) -> Result<(), String>;

/// Result of [`execute`].
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub photos: usize,
    pub files: usize,
    pub bytes: u64,
    /// copy time (s)
    pub copy_secs: f64,
    /// preview entries moved to the new key
    pub previews_moved: usize,
    pub same_volume: bool,
    /// notes (e.g. files left at the old place)
    pub notes: Vec<String>,
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
            _ => {}
        }
    }
    let _ = std::fs::remove_dir(dir); // only if empty
}

/// The file paths of the library (id → root + folder + name) after the switch.
fn paths(db: &Db, files: &[i64]) -> std::collections::HashMap<i64, String> {
    let mut m = std::collections::HashMap::new();
    let Ok(mut st) = db.conn.prepare("SELECT s.path || o.path || d.name FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1") else { return m };
    for &id in files {
        if let Ok(p) = st.query_row([id], |r| r.get::<_, String>(0)) {
            m.insert(id, p);
        }
    }
    m
}

/// Move the previews of the photos to their new keys.
fn rekey(db: &Db, plan: &Plan, cache: &Cache) -> usize {
    let new = paths(db, &plan.photos.iter().map(|p| p.file).collect::<Vec<_>>());
    plan.photos.iter().filter_map(|p| new.get(&p.file).map(|n| cache.rekey(p.key(), p.key_at(n)))).sum()
}

/// Source into the trash, after the switch. Whole folder (local, nothing new in it), otherwise exactly the copied files
/// (network drive: its trash folder; files added meanwhile stay where they are, with a note).
fn cleanup(v: &Operation, dispose: Dispose, notes: &mut Vec<String>) -> Result<(), String> {
    let Some((source, root)) = v.cleanup.clone() else { return Ok(()) };
    if !source.exists() {
        return Ok(());
    }
    let copied: HashSet<PathBuf> = v.steps.iter().filter(|s| s.keep).map(|s| s.from.clone()).collect();
    let network = !crate::watch::local(&source);
    let skip = network.then(|| crate::network_trash::base(&root)).filter(|t| t.starts_with(&source));
    let now = walk(&source, skip.as_deref()).unwrap_or_default();
    let extra: Vec<PathBuf> = now.iter().map(|(r, _)| source.join(r)).filter(|p| !copied.contains(p)).collect();
    if !network && extra.is_empty() {
        return dispose(std::slice::from_ref(&source), &root);
    }
    let mut list: Vec<PathBuf> = copied.into_iter().filter(|p| p.exists()).collect();
    list.sort();
    dispose(&list, &root)?;
    remove_empty_tree(&source);
    if !extra.is_empty() {
        notes.push(format!("{} Datei(en) kamen während des Verschiebens hinzu und blieben in {}", extra.len(), source.display()));
    }
    Ok(())
}

/// Execute a planned move (see the module description). Cancel (`cancel`) works until the library is switched over;
/// everything copied so far is removed again then and the source stays.
pub fn execute(db: &Db, plan: &Plan, cache: Option<&Cache>, cancel: &AtomicBool, progress: &dyn Fn(Progress), dispose: Dispose) -> Result<Outcome, String> {
    let _busy = BusyGuard::new(vec![plan.source.clone(), plan.target.clone()]);
    // sidecar writes still waiting for the source are written first (they are copied along then)
    let root_id: Option<i64> = db.conn.query_row("SELECT root FROM folder WHERE id = ?1", [plan.folder], |r| r.get(0)).optional().map_err(f)?;
    if let Some(r) = root_id {
        let _ = r;
        crate::store::start_writer_round();
    }
    let steps: Vec<Step> = if plan.same_volume {
        vec![Step::new(plan.source.clone(), plan.target.clone())]
    } else {
        plan.files.iter().map(|(r, _)| Step::copy(plan.source.join(r), plan.target.join(r))).collect()
    };
    let mut v = Operation {
        id: relocate::new_id(),
        title: format!("Ordner „{}“ nach {}", plan.name, plan.dest.display()),
        steps,
        changes: plan.changes.clone(),
        status: "planned".into(),
        message: String::new(),
        kind: KIND.into(),
        cleanup: (!plan.same_volume).then(|| (plan.source.clone(), plan.source_root.clone())),
    };
    if std::fs::symlink_metadata(&plan.target).is_ok() {
        return Err(format!("Am Ziel gibt es schon „{}“ ({}) – nichts wird überschrieben", plan.name, plan.target.display()));
    }
    relocate::check(&v)?;
    let total = v.steps.len();
    let bytes_total = plan.bytes;
    let t = std::time::Instant::now();
    let r = relocate::execute_with(db, &mut v, Some(cancel), &mut |done, bytes| progress(Progress { phase: Phase::Copy, done, total, bytes, bytes_total }));
    if let Err(e) = r {
        if !plan.same_volume {
            remove_empty_tree(&plan.target);
        }
        return Err(e);
    }
    let copy_secs = t.elapsed().as_secs_f64();
    progress(Progress { phase: Phase::Switch, done: total, total, bytes: bytes_total, bytes_total });
    let mut out = Outcome { photos: plan.photos.len(), files: plan.files.len(), bytes: plan.bytes, copy_secs, same_volume: plan.same_volume, ..Default::default() };
    out.previews_moved = match cache {
        Some(c) => rekey(db, plan, c),
        None => rekey(db, plan, &Cache::standard()),
    };
    if v.status == "cleanup" {
        progress(Progress { phase: Phase::Cleanup, done: total, total, bytes: bytes_total, bytes_total });
        match cleanup(&v, dispose, &mut out.notes) {
            Ok(()) => {
                v.status = "done".into();
                v.save()?;
            }
            Err(e) => {
                v.message = e.clone();
                v.save()?;
                out.notes.push(format!("Alles ist kopiert, geprüft und die Bibliothek zeigt auf {}; der alte Ordner kam aber nicht in den Papierkorb: {e}", plan.target.display()));
            }
        }
    }
    Ok(out)
}

/// At startup: interrupted folder moves. Before the switch of the library: everything copied is removed again (the
/// source is unchanged; the move can simply be started again). After the switch: the source goes to the trash.
/// Returns: messages.
pub fn resume(db: &Db, dispose: Dispose) -> Vec<String> {
    let mut m = Vec::new();
    for mut v in relocate::journals().into_iter().filter(|v| v.kind == KIND) {
        if v.status == "running" && relocate::db_applied(db, &v.changes) == Some(true) {
            // the run ended between switching the library and saving the journal: the copies at the target are the
            // photos now – finish instead of reverting (which would delete them and leave every photo "missing")
            v.status = if v.cleanup.is_some() { "cleanup" } else { "done" }.into();
            let _ = v.save();
            m.push(format!("{}: Bibliothek war schon umgestellt", v.title));
        }
        match v.status.as_str() {
            "running" | "planned" | "rolling_back" => {
                v.status = "rolling_back".into();
                let target = v.steps.first().map(|s| s.by_key.clone());
                let r = relocate::execute_step(db, &mut v);
                if let (Some(t), true) = (target, v.cleanup.is_some()) {
                    // the folder itself: the first step's target is `target/<rel>`; remove what is empty from there up to the moved folder
                    if let Some(dir) = v.cleanup.as_ref().and_then(|(src, _)| v.steps.first().and_then(|s| s.from.strip_prefix(src).ok()).map(|rel| t.ancestors().nth(rel.components().count()).map(Path::to_path_buf))).flatten() {
                        remove_empty_tree(&dir);
                    }
                }
                m.push(match r {
                    Err(e) if v.status == "failed" => format!("{}: unterbrochen, zurückgenommen ({e})", v.title),
                    Err(e) => format!("{}: {e}", v.title),
                    Ok(()) => format!("{}: zurückgenommen", v.title),
                });
            }
            "cleanup" => {
                let mut notes = Vec::new();
                match cleanup(&v, dispose, &mut notes) {
                    Ok(()) => {
                        v.status = "done".into();
                        let _ = v.save();
                        m.push(format!("{}: alter Ordner in den Papierkorb", v.title));
                    }
                    Err(e) => m.push(format!("{}: {e}", v.title)),
                }
                m.extend(notes);
            }
            _ => {}
        }
    }
    m
}
