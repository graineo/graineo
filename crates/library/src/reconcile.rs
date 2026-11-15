//! Sync library ↔ disk. Read-only (`read_dir`, `stat`, small sidecars);
//! writes only into the own database.
//!
//! Per root folder:
//! - root folder not reachable (e.g. volume `/Volumes/Data` not mounted) → all its
//!   files "volume missing" (state 2), nothing is removed.
//! - known folders: files present (0) or missing (1); size and modification time are
//!   updated (the preview cache key is derived from them).
//! - new image files in known folders are added. Folders the library does not know are only
//!   counted and reported in Lightroom root folders (no unrequested additions); in own root
//!   folders (created via "add folder") they are added completely.
//! - X3F: as for any RAW, the sidecar is the XMP (`NAME.xmp`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::params;
use serde_json::json;

use crate::db::Db;

/// Folder names that are never scanned (packages of other programs, own sidecar folders).
pub(crate) fn skip(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    let lower = name.to_lowercase();
    [".photoslibrary", ".lrdata", ".lrcat-data", ".cocatalog", ".lrlibrary", ".app", ".aplibrary", ".migratedphotolibrary", ".photolibrary", ".fcpbundle", ".imovielibrary", ".bundle"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// Result of a sync.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub files_checked: usize,
    pub existing: usize,
    pub missing: usize,
    /// examples of missing files (at most 500)
    pub missing_list: Vec<String>,
    pub missing_per_folder: BTreeMap<String, usize>,
    /// root folders that are currently unreachable (path, files)
    pub offline: Vec<(String, usize)>,
    /// newly added files in known folders
    pub newly_added: usize,
    pub new_per_format: BTreeMap<String, usize>,
    /// folders with images the library does not know (only reported): path → images
    pub unknown_folders: BTreeMap<String, usize>,
    pub sidecar_imported: usize,
    /// sidecar files (RAW+JPEG) according to Lightroom – on disk, but not an image of their own
    pub sidecars: usize,
    /// sidecars rewritten from the library (were missing/broken)
    pub sidecar_new: usize,
    pub sidecar_broken: usize,
    /// field groups taken over from sidecars or written into them
    pub from_sidecar: usize,
    pub in_sidecar: usize,
    /// would have to be written, but writing is off
    pub sidecar_would: usize,
    pub sidecar_list: Vec<String>,
    /// files moved in the Finder, relinked
    pub relinked: usize,
    pub linked_list: Vec<String>,
    pub conflicts: Vec<String>,
    /// waiting sidecar writes (location was unreachable) done now: written / merged with a sidecar changed elsewhere
    pub pending_written: usize,
    pub pending_merged: usize,
    /// still waiting (location unreachable or original not there)
    pub pending_waiting: usize,
    pub duration_ms: u128,
    pub run: i64,
    /// quick sync ([`Depth::Quick`])
    pub quick: bool,
    /// folders listed (`read_dir`)
    pub folders_listed: usize,
    /// of those unchanged since the last run (no `stat` per file)
    pub folders_unchanged: usize,
    /// files whose size and time were read (`stat`)
    pub files_stat: usize,
}

impl Report {
    pub fn json(&self) -> serde_json::Value {
        json!({
            "files_checked": self.files_checked,
            "existing": self.existing,
            "missing": self.missing,
            "missing_list": self.missing_list,
            "missing_per_folder": self.missing_per_folder,
            "offline": self.offline.iter().map(|(p, n)| json!({"path": p, "files": n})).collect::<Vec<_>>(),
            "newly_added": self.newly_added,
            "new_per_format": self.new_per_format,
            "unknown_folders": self.unknown_folders,
            "unknown_folders_images": self.unknown_folders.values().sum::<usize>(),
            "sidecar_imported": self.sidecar_imported,
            "sidecars": self.sidecars,
            "sidecar_new": self.sidecar_new,
            "sidecar_broken": self.sidecar_broken,
            "from_sidecar": self.from_sidecar,
            "in_sidecar": self.in_sidecar,
            "sidecar_would": self.sidecar_would,
            "sidecar_list": self.sidecar_list,
            "relinked": self.relinked,
            "linked_list": self.linked_list,
            "conflicts": self.conflicts,
            "pending_written": self.pending_written,
            "pending_merged": self.pending_merged,
            "pending_waiting": self.pending_waiting,
            "duration_ms": self.duration_ms,
            "quick": self.quick,
            "folders_listed": self.folders_listed,
            "folders_unchanged": self.folders_unchanged,
            "files_stat": self.files_stat,
        })
    }
}

/// Stars from a foreign XMP file next to the photo (`NAME.xmp`); graineo's own XMP
/// do not count (they only mirror the sidecar).
pub fn foreign_xmp_stars(photo: &Path) -> Option<i64> {
    let s = std::fs::read_to_string(photo.with_extension("xmp")).ok()?;
    if crate::is_own_xmp(&s) {
        return None;
    }
    let i = s.find("xmp:Rating")?;
    let rest = &s[i + 10..];
    let rest = rest.trim_start_matches(['=', '"', '>', ' ']);
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    n.parse().ok()
}

#[derive(Clone)]
struct DbFile {
    id: i64,
    size: Option<i64>,
    mtime: Option<i64>,
    status: i64,
    /// Lightroom sidecar extensions (RAW+JPEG: "JPG"); such files are not an image of their own
    sidecar: Option<String>,
}

/// How thoroughly a sync looks at the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// Size and modification time of every file are read (`stat`): on request ("Bibliothek prüfen", Lightroom import,
    /// adding a folder) and once a day ([`full_due`]).
    Full,
    /// Every folder is listed and its names compared with the library (new, missing, moved files are always found).
    /// Size and time of the files are only read where the folder changed since the last run (modification time or
    /// number of entries differ, see [`folder_unchanged`]), and for files the library has as missing/offline.
    Quick,
}

/// Settings of a sync run ([`run`]).
pub struct Options<'a> {
    pub depth: Depth,
    /// overrides once the setting "write sidecars" (see [`reconcile_with`])
    pub write_now: Option<bool>,
    /// text of the current step
    pub progress: &'a dyn Fn(&str),
    /// folders done / expected (at most every 250 ms)
    pub count: &'a dyn Fn(usize, usize),
    /// called before each folder: a run in the background waits here while the user works
    pub pause: &'a dyn Fn(),
}

/// Sync all root folders (or only one), checking every file ([`Depth::Full`]).
pub fn reconcile(db: &Db, stem_only: Option<i64>, progress: &dyn Fn(&str)) -> Result<Report, String> {
    reconcile_with(db, stem_only, None, progress)
}

/// Like [`reconcile`]; `write` overrides once the setting "create missing
/// sidecars" (e.g. "Begleitdateien synchronisieren …" after confirmation).
pub fn reconcile_with(db: &Db, stem_only: Option<i64>, write_now: Option<bool>, progress: &dyn Fn(&str)) -> Result<Report, String> {
    run(db, stem_only, &Options { depth: Depth::Full, write_now, progress, count: &|_, _| {}, pause: &|| {} })
}

/// `info` key: Unix seconds of the last full sync of all root folders.
const FULL_KEY: &str = "sync_full";

/// Is a full sync due (none in the last 24 hours)? The quick sync does not notice files changed in place in an
/// otherwise unchanged folder; the daily full sync does.
pub fn full_due(db: &Db) -> bool {
    db.info(FULL_KEY).and_then(|t| t.parse::<i64>().ok()).is_none_or(|t| crate::now() - t >= 24 * 3600)
}

/// A folder modification time this close to now (or in the future: clock of a network drive ahead) is not trusted:
/// a change within the same clock tick (1–2 s on HFS+/FAT) would not change it.
const RACY_NS: i128 = 3_000_000_000;

/// Modification time of a folder in nanoseconds, if it can be trusted for the next quick sync.
fn folder_mtime(m: &std::fs::Metadata, now_ns: i128) -> Option<i64> {
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as i128;
    (now_ns - t >= RACY_NS).then_some(t as i64)
}

/// May a quick sync skip reading size and time of the files of this folder? Only if modification time and number
/// of entries are the same as at the last run. The modification time of a folder changes when an entry is added,
/// removed or renamed (also for programs that save via a temporary file and rename it); the number of entries in
/// addition guards against file systems/servers that report a coarse or stale time. A file overwritten in place
/// changes neither: that is found by the daily full sync.
pub fn folder_unchanged(last: Option<&(Option<i64>, i64)>, mtime_ns: Option<i64>, entries: i64) -> bool {
    matches!((last, mtime_ns), (Some((Some(a), n)), Some(b)) if *a == b && *n == entries)
}

/// Sync with the given [`Options`].
pub fn run(db: &Db, stem_only: Option<i64>, o: &Options) -> Result<Report, String> {
    let error = |e: rusqlite::Error| e.to_string();
    let progress = o.progress;
    let t0 = Instant::now();
    let mut b = Report { quick: o.depth == Depth::Quick, ..Report::default() };
    let run = db.new_run("sync", "").map_err(error)?;
    b.run = run;
    if let Ok(n @ 1..) = db.normalize_camera_names() {
        eprintln!("camera names normalized: {n}");
    }
    let stems = db.roots().map_err(error)?;
    let stems: Vec<&crate::db::Root> = stems.iter().filter(|s| stem_only.is_none_or(|n| n == s.id) && s.origin != crate::apple_photos::ORIGIN).collect();
    // Read first, then write briefly: the run over the disk (seconds to minutes, NAS) holds no
    // write lock. It only collects the changes; afterwards they are written in short blocks.
    let mut steps: Vec<Step> = Vec::new();
    let mut new_files: Vec<(i64, i64, String)> = Vec::new();
    let mut missing_items: Vec<(i64, Option<i64>, String)> = Vec::new();
    // folder states of the last run (per root) and the expected number of folders (for the progress)
    let mut last_scan: HashMap<i64, HashMap<String, (Option<i64>, i64)>> = HashMap::new();
    for s in &stems {
        let mut st = db.conn.prepare("SELECT rel, mtime_ns, entries FROM folder_scan WHERE root=?1").map_err(error)?;
        let m = st.query_map([s.id], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))).map_err(error)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(error)?;
        last_scan.insert(s.id, m);
    }
    let folders_known: HashMap<i64, usize> = {
        let mut st = db.conn.prepare("SELECT root, count(*) FROM folder GROUP BY root").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        r
    };
    let total: usize = stems.iter().map(|s| last_scan.get(&s.id).map_or(0, |m| m.len()).max(folders_known.get(&s.id).copied().unwrap_or(0))).sum::<usize>().max(1);
    let mut done = 0usize;
    let mut last_count = Instant::now();
    (o.count)(0, total);
    // the Photos library is synced by `apple_photos::reconcile` (many originals are only in iCloud)
    for s in stems {
        let s_expected = last_scan.get(&s.id).map_or(0, |m| m.len()).max(folders_known.get(&s.id).copied().unwrap_or(0));
        let done_before = done;
        // a folder move is running there: synced afterwards (otherwise its copies would count as new photos)
        if crate::move_folder::touches_busy(&s.path) {
            progress(&format!("Abgleich {}: wird gerade verschoben, später", s.path));
            done += s_expected;
            continue;
        }
        progress(&format!("Abgleich {} …", s.path));
        // folders and files of the root from the database
        let folder: HashMap<String, i64> = {
            let mut st = db.conn.prepare("SELECT path, id FROM folder WHERE root=?1").map_err(error)?;
            let r = st.query_map([s.id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
            r
        };
        let mut files: HashMap<i64, HashMap<String, DbFile>> = HashMap::new();
        {
            let mut st = db.conn.prepare("SELECT d.folder, d.name, d.id, d.size, d.mtime, d.status, d.sidecars FROM file d JOIN folder o ON o.id=d.folder WHERE o.root=?1").map_err(error)?;
            let mut rows = st.query([s.id]).map_err(error)?;
            while let Some(r) = rows.next().map_err(error)? {
                let o: i64 = r.get(0).map_err(error)?;
                let d = DbFile { id: r.get(2).map_err(error)?, size: r.get(3).map_err(error)?, mtime: r.get(4).map_err(error)?, status: r.get(5).map_err(error)?, sidecar: r.get(6).map_err(error)? };
                files.entry(o).or_default().insert(r.get(1).map_err(error)?, d);
            }
        }
        let count: usize = files.values().map(|m| m.len()).sum();
        b.files_checked += count;
        let root_dir = PathBuf::from(&s.path);
        // not only "is a folder": an empty mount point left on the system disk for a disconnected network drive is
        // one too – everything in it would count as missing instead of "drive not connected"
        if !crate::location::root_reachable(&s.path) {
            steps.push(Step::Offline(s.id));
            b.offline.push((s.path.clone(), count));
            done += s_expected;
            continue;
        }
        let own = s.origin == "own";
        // folders and files removed from the library by the user: not taken in again
        let excluded = crate::manage::exclusions(&db.conn, s.id);
        let last = last_scan.remove(&s.id).unwrap_or_default();
        let mut visited: HashSet<String> = HashSet::new();
        let mut scanned: HashSet<String> = HashSet::new();
        // folders that could not be listed this time (network hiccup, rights): what is below them is unknown, not missing
        let mut unreadable: Vec<String> = Vec::new();
        // recursive run over the disk
        let mut batch: Vec<String> = vec![String::new()];
        while let Some(rel) = batch.pop() {
            (o.pause)();
            done += 1;
            if last_count.elapsed().as_millis() >= 250 {
                last_count = Instant::now();
                (o.count)(done.min(total), total);
            }
            let abs = root_dir.join(&rel);
            // time of the folder before listing it: a change during the listing makes the next run look again
            let now_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i128);
            let mtime_ns = std::fs::metadata(&abs).ok().and_then(|m| folder_mtime(&m, now_ns));
            let Ok(rd) = std::fs::read_dir(&abs) else {
                if abs.exists() {
                    unreadable.push(rel.clone());
                }
                continue;
            };
            b.folders_listed += 1;
            let mut names: Vec<(String, Option<std::fs::DirEntry>)> = Vec::new();
            let mut all_names: Vec<String> = Vec::new();
            // network drives (SMB) sometimes return names several times for very large folders: look at each only once
            let mut seen: HashSet<String> = HashSet::new();
            for e in rd.flatten() {
                let name = crate::nfc(&e.file_name().to_string_lossy());
                if !seen.insert(name.clone()) {
                    continue;
                }
                all_names.push(name.clone());
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    let sub = format!("{rel}{name}/");
                    if !skip(&name) && !crate::manage::is_excluded(&excluded, &sub) {
                        batch.push(sub);
                    }
                } else if ft.is_file() && !name.starts_with('.') && crate::format_from_extension(&name).is_some() && !crate::manage::is_excluded(&excluded, &format!("{rel}{name}")) {
                    names.push((name, Some(e)));
                }
            }
            let entries = all_names.len() as i64;
            scanned.insert(rel.clone());
            // the folder state is written after the changes of its files (an aborted run never leaves a state that
            // claims files as checked that were not written)
            let scan = Step::Scan { root: s.id, rel: rel.clone(), mtime_ns, entries };
            // new folder of an own root: created when writing
            let folder_id = match folder.get(&rel) {
                Some(&id) => Some(id),
                None if own && !names.is_empty() => None,
                None => {
                    if !names.is_empty() {
                        b.unknown_folders.insert(format!("{}{rel}", s.path), names.len());
                    }
                    steps.push(scan);
                    continue;
                }
            };
            visited.insert(rel.clone());
            let known = folder_id.and_then(|o| files.remove(&o)).unwrap_or_default();
            let unchanged = o.depth == Depth::Quick && folder_unchanged(last.get(&rel), mtime_ns, entries);
            if unchanged {
                b.folders_unchanged += 1;
            }
            // The listing of a folder on a network drive can be incomplete (for very large folders SMB omits
            // files). Whatever the library knows and the listing does not name is queried individually before it
            // counts as "missing".
            let caught_up: Vec<(String, std::fs::Metadata)> = {
                let named: HashSet<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
                known
                    .iter()
                    .filter(|(n, _)| !named.contains(n.as_str()))
                    .filter_map(|(n, _)| {
                        b.files_stat += 1;
                        abs.join(n).metadata().ok().filter(|m| m.is_file()).map(|m| (n.clone(), m))
                    })
                    .collect()
            };
            let mut caught: HashMap<String, std::fs::Metadata> = HashMap::new();
            for (n, m) in caught_up {
                names.push((n.clone(), None));
                caught.insert(n, m);
            }
            let on_disk: HashSet<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
            // sidecar files according to Lightroom (e.g. IMG_1.JPG for IMG_1.CR3 with RAW+JPEG) – not an image of their own
            let companions: HashSet<String> = known
                .iter()
                .filter_map(|(n, d)| Some((n, d.sidecar.as_deref()?)))
                .flat_map(|(n, ext)| {
                    let base = n.rsplit_once('.').map_or(n.as_str(), |(b, _)| b).to_lowercase();
                    ext.split(',').map(move |e| format!("{base}.{}", e.trim().to_lowercase())).collect::<Vec<_>>()
                })
                .collect();
            for (name, d) in &known {
                if !on_disk.contains(name.as_str()) {
                    steps.push(Step::Missing { id: d.id, folder: folder_id.unwrap_or_default(), name: name.clone() });
                    missing_items.push((d.id, d.size, name.clone()));
                    b.missing += 1;
                    *b.missing_per_folder.entry(format!("{}{rel}", s.path)).or_default() += 1;
                    if b.missing_list.len() < 500 {
                        b.missing_list.push(format!("{}{rel}{name}", s.path));
                    }
                }
            }
            for (name, entry) in &names {
                let known_file = known.get(name);
                let is_companion = known_file.is_none() && companions.contains(&name.to_lowercase());
                if is_companion {
                    // sidecar JPEG according to Lightroom: not an image of its own
                    b.sidecars += 1;
                    continue;
                }
                // quick: a file present at the last run in an unchanged folder is still the same (size and time known)
                if known_file.is_some_and(|d| unchanged && d.status == 0 && d.size.is_some() && d.mtime.is_some()) {
                    b.existing += 1;
                    continue;
                }
                let meta = match (caught.remove(name), entry) {
                    (Some(m), _) => Some(m),
                    (None, Some(e)) => {
                        b.files_stat += 1;
                        e.metadata().ok()
                    }
                    (None, None) => None,
                };
                let Some(m) = meta else { continue };
                let size = m.len() as i64;
                let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
                match known_file {
                    Some(d) => {
                        b.existing += 1;
                        if d.status != 0 || d.size != Some(size) || d.mtime != mtime {
                            steps.push(Step::Present { id: d.id, size, mtime });
                        }
                    }
                    None => {
                        let format = crate::format_from_extension(name).unwrap_or("ANDERE");
                        steps.push(Step::New(Box::new(NewFile { root: s.id, rel: rel.clone(), folder: folder_id, name: name.clone(), format, size, mtime })));
                    }
                }
            }
            steps.push(scan);
        }
        // known folders that do not exist (any more) on disk
        for (rel, folder_id) in &folder {
            if visited.contains(rel) || unreadable.iter().any(|u| rel.starts_with(u.as_str())) {
                continue;
            }
            if let Some(list) = files.remove(folder_id) {
                for (name, d) in list {
                    steps.push(Step::Missing { id: d.id, folder: *folder_id, name: name.clone() });
                    b.missing += 1;
                    *b.missing_per_folder.entry(format!("{}{rel}", s.path)).or_default() += 1;
                    if b.missing_list.len() < 500 {
                        b.missing_list.push(format!("{}{rel}{name}", s.path));
                    }
                }
            }
        }
        // folders of the last run that are gone: forget their state
        for rel in last.keys().filter(|r| !scanned.contains(*r)) {
            steps.push(Step::Unscan { root: s.id, rel: rel.clone() });
        }
        // the expected number was an estimate: fewer folders than expected → continue after all of them
        done = done_before + s_expected.max(done - done_before);
    }
    (o.count)(total, total);
    progress("Änderungen eintragen …");
    write_steps(db, &steps, run, &mut b, &mut new_files).map_err(error)?;
    drop(steps);
    // relink files moved/renamed in the Finder (instead of "missing" + "new")
    progress("Verschobene Dateien suchen …");
    let relinked_ids = relink(db, &missing_items, &new_files, &mut b).map_err(error)?;
    // sidecars: the library is the main store (`crate::store`). Only the sidecars of photos new in this run are read
    // (once, `file.xmp_read`); later changes by other programs come in only through "Begleitdateien einlesen …"
    // (`crate::sidecar_import::run`). Writing sidecars happens in the background writer, if switched on.
    progress("Begleitdateien neuer Fotos lesen …");
    let fresh: Vec<i64> = new_files.iter().map(|(id, _, _)| *id).filter(|id| !relinked_ids.contains(id)).collect();
    for chunk in fresh.chunks(BLOCK) {
        match crate::sidecar_import::import_files(db, chunk) {
            Ok(r) => {
                b.from_sidecar += r.taken;
                b.sidecar_broken += r.broken;
                for l in r.log {
                    if b.sidecar_list.len() < 500 {
                        b.sidecar_list.push(l);
                    }
                }
            }
            Err(e) => b.conflicts.push(e),
        }
    }
    // root folder list as a file (rebuild without database)
    let _ = crate::backup::write_roots(db);
    b.duration_ms = t0.elapsed().as_millis();
    if o.depth == Depth::Full && stem_only.is_none() {
        let _ = db.set_info(FULL_KEY, &crate::now().to_string());
    }
    db.finish_run(run, b.newly_added as i64, &b.json().to_string()).map_err(error)?;
    Ok(b)
}

/// Write block: so many changes per transaction that the lock lasts only milliseconds.
const BLOCK: usize = 500;

/// Change from the run over the disk; written afterwards in short blocks ([`write_steps`]).
enum Step {
    /// root folder not reachable
    Offline(i64),
    /// file missing on disk (only if the entry has not been moved in the meantime)
    Missing { id: i64, folder: i64, name: String },
    /// file back or size/modification time changed
    Present { id: i64, size: i64, mtime: Option<i64> },
    New(Box<NewFile>),
    /// state of a listed folder for the next quick sync (`None`: time not trustworthy)
    Scan { root: i64, rel: String, mtime_ns: Option<i64>, entries: i64 },
    /// folder of the last run no longer there
    Unscan { root: i64, rel: String },
}

struct NewFile {
    root: i64,
    rel: String,
    /// `None`: new folder of an own root, created when writing
    folder: Option<i64>,
    name: String,
    format: &'static str,
    size: i64,
    mtime: Option<i64>,
}

/// Write the collected changes in blocks. Each transaction is short; in between the
/// UI and other work get their turn. Whatever was entered from elsewhere in the meantime
/// (e.g. an import into the same folder) is not created twice.
fn write_steps(db: &Db, steps: &[Step], run: i64, b: &mut Report, new_files: &mut Vec<(i64, i64, String)>) -> rusqlite::Result<()> {
    let mut new_folders: HashMap<(i64, &str), i64> = HashMap::new();
    for block in steps.chunks(BLOCK) {
        let tx = db.conn.unchecked_transaction()?;
        for s in block {
            match s {
                Step::Offline(root) => {
                    tx.execute("UPDATE file SET status=2 WHERE folder IN (SELECT id FROM folder WHERE root=?1)", [root])?;
                }
                Step::Missing { id, folder, name } => {
                    tx.execute("UPDATE file SET status=1 WHERE id=?1 AND folder=?2 AND name=?3", params![id, folder, name])?;
                }
                Step::Present { id, size, mtime } => {
                    // content changed (size or time): the stored checksums no longer apply
                    tx.execute("UPDATE file SET status=0, partial_hash = CASE WHEN size IS ?2 AND mtime IS ?3 THEN partial_hash END, sha256 = CASE WHEN size IS ?2 AND mtime IS ?3 THEN sha256 END, size=?2, mtime=?3 WHERE id=?1", params![id, size, mtime])?;
                }
                Step::Scan { root, rel, mtime_ns, entries } => {
                    tx.execute(
                        "INSERT INTO folder_scan(root, rel, mtime_ns, entries) VALUES(?1,?2,?3,?4) ON CONFLICT(root, rel) DO UPDATE SET mtime_ns=excluded.mtime_ns, entries=excluded.entries",
                        params![root, rel, mtime_ns, entries],
                    )?;
                }
                Step::Unscan { root, rel } => {
                    tx.execute("DELETE FROM folder_scan WHERE root=?1 AND rel=?2", params![root, rel])?;
                }
                Step::New(n) => {
                    let folder_id = match n.folder.or_else(|| new_folders.get(&(n.root, n.rel.as_str())).copied()) {
                        Some(o) => o,
                        None => {
                            let o = db.create_folder(n.root, &n.rel, None)?;
                            new_folders.insert((n.root, n.rel.as_str()), o);
                            o
                        }
                    };
                    match tx.execute(
                        "INSERT INTO file(folder, name, format, size, mtime, status) VALUES(?1,?2,?3,?4,?5,0) ON CONFLICT(folder, name) DO NOTHING",
                        params![folder_id, n.name, n.format, n.size, n.mtime],
                    ) {
                        Ok(0) => continue,
                        Ok(_) => {}
                        // folder removed in the meantime or similar: again at the next sync
                        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => continue,
                        Err(e) => return Err(e),
                    }
                    let did = tx.last_insert_rowid();
                    tx.execute("INSERT INTO image(file, run) VALUES(?1,?2)", params![did, run])?;
                    new_files.push((did, n.size, n.name.clone()));
                    b.newly_added += 1;
                    *b.new_per_format.entry(n.format.to_string()).or_default() += 1;
                }
            }
        }
        tx.commit()?;
    }
    Ok(())
}

/// Write sidecars next to the photos? (setting `writeSidecars`, see [`crate::store::write_sidecars`]).
pub fn write_allowed(_db: &Db) -> bool {
    crate::store::write_sidecars()
}


/// Partial checksum (FNV-1a over the first and last 64 KB).
pub fn partial_sum(p: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let mut buf = vec![0u8; 65536];
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: &[u8]| {
        for x in b {
            h ^= *x as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    let n = f.read(&mut buf).ok()?;
    feed(&buf[..n]);
    if len > 131072 {
        f.seek(SeekFrom::End(-65536)).ok()?;
        let n = f.read(&mut buf).ok()?;
        feed(&buf[..n]);
    }
    Some(format!("{h:016x}-{len}"))
}

/// [`partial_sum`] from bytes read elsewhere (a camera, a read past the cache): `first` = the first 64 KB (or the whole
/// file if shorter), `last` = the last 64 KB for files over 128 KB, `len` = file length.
pub fn partial_sum_of(first: &[u8], last: Option<&[u8]>, len: u64) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for x in first.iter().chain(last.unwrap_or_default()) {
        h ^= *x as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}-{len}")
}

/// Link newly found files with missing ones of the same size: same name (moved) or
/// same capture time (renamed), with a known partial checksum also that. The missing
/// file takes over the new location (including rating, keywords, collections); the new entry
/// is dropped. Returns: dropped new file IDs.
fn relink(db: &Db, missing_items: &[(i64, Option<i64>, String)], new: &[(i64, i64, String)], b: &mut Report) -> rusqlite::Result<HashSet<i64>> {
    let mut remove_fn = HashSet::new();
    if missing_items.is_empty() || new.is_empty() {
        return Ok(remove_fn);
    }
    let free: &[(i64, Option<i64>, String)] = missing_items;
    // missing files by size (before 0.108 every new file went through all missing ones: a renamed folder of 100 000
    // photos meant 10^10 comparisons)
    let mut by_size: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, f) in free.iter().enumerate() {
        if let Some(g) = f.1 {
            by_size.entry(g).or_default().push(i);
        }
    }
    let mut taken = vec![false; free.len()];
    for (nid, size, name) in new {
        let cand: Vec<usize> = by_size.get(size).map(|v| v.iter().copied().filter(|&i| !taken[i]).collect()).unwrap_or_default();
        if cand.is_empty() {
            continue;
        }
        let path_new = db.file_path(*nid);
        let matches = cand.iter().copied().find(|&i| free[i].2 == *name).or_else(|| {
            // renamed: compare capture time (read the header of the new file)
            let p = path_new.as_ref()?;
            let fm = crate::format_from_extension(name).unwrap_or("");
            let a = crate::exif::read(p, fm).and_then(|a| a.capture)?;
            cand.iter().copied().find(|&i| db.conn.query_row("SELECT capture_time FROM file WHERE id=?1", [free[i].0], |r| r.get::<_, Option<String>>(0)).ok().flatten().as_deref() == Some(a.as_str()))
        });
        let Some(i) = matches else { continue };
        taken[i] = true;
        let (old, _, old_name) = free[i].clone();
        // partial checksum, if known, must match
        let old_sum: Option<String> = db.conn.query_row("SELECT partial_hash FROM file WHERE id=?1", [old], |r| r.get(0)).ok().flatten();
        let new_sum = path_new.as_deref().and_then(partial_sum);
        if old_sum.is_some() && old_sum != new_sum {
            continue;
        }
        let (folder, gr, mt): (i64, i64, Option<i64>) = db.conn.query_row("SELECT folder, size, mtime FROM file WHERE id=?1", [nid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        // both or neither (short transaction; checksum and header read above without lock)
        let tx = db.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM file WHERE id=?1", [nid])?;
        tx.execute("UPDATE file SET folder=?2, name=?3, size=?4, mtime=?5, status=0, partial_hash=coalesce(?6, partial_hash) WHERE id=?1", params![old, folder, name, gr, mt, new_sum])?;
        tx.commit()?;
        remove_fn.insert(*nid);
        b.relinked += 1;
        b.newly_added = b.newly_added.saturating_sub(1);
        b.missing = b.missing.saturating_sub(1);
        if let Some(p) = &path_new {
            b.missing_list.retain(|x| !x.ends_with(&format!("/{old_name}")) || x.contains(&p.display().to_string()));
            if b.linked_list.len() < 500 {
                b.linked_list.push(format!("{old_name} → {}", p.display()));
            }
        }
    }
    Ok(remove_fn)
}

/// Add a folder as an own root folder and sync it. If it already lies inside a
/// root folder, nothing is created (returns: that root).
pub fn add_root(db: &Db, folder: &Path, progress: &dyn Fn(&str)) -> Result<(i64, Report), String> {
    if crate::apple_photos::in_photos_library(folder) {
        return Err("Die Mediathek von Apple Fotos wird über Einstellungen → Bibliothek eingebunden, nicht als Ordner".into());
    }
    if let Some(s) = db.root_to_path(folder) {
        let b = reconcile(db, Some(s.id), progress)?;
        db.mark_run(b.run, "folder").map_err(|e| e.to_string())?;
        return Ok((s.id, b));
    }
    let p = format!("{}/", crate::nfc(folder.to_string_lossy().trim_end_matches('/')));
    if let Some(s) = db.roots().map_err(|e| e.to_string())?.into_iter().find(|s| s.path.starts_with(&p)) {
        return Err(format!("{} enthält schon den Stammordner {} – bitte einen Ordner darin wählen", p, s.path));
    }
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
    let id = db.create_root(&p, &name, "own", None).map_err(|e| e.to_string())?;
    db.create_folder(id, "", None).map_err(|e| e.to_string())?;
    let b = reconcile(db, Some(id), progress)?;
    db.mark_run(b.run, "folder").map_err(|e| e.to_string())?;
    Ok((id, b))
}
