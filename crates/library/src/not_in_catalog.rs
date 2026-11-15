//! "Not in catalog": folders on disk (below the root folders) whose photos the
//! library does not know yet – the counterpart to "missing photos".
//!
//! Why a folder ends up here ([`Reason`]):
//! - **Lightroom root folder:** the sync takes new photos in known folders, but in root folders that came from the
//!   Lightroom import it never adds folders on its own (no unrequested additions). A folder there that Lightroom did
//!   not know (never imported, removed in Lightroom, created later on disk) stays outside until it is added.
//! - **Own root folder:** there the sync adds new folders by itself; such a folder is only listed until the next sync.
//!
//! Folders removed from the library ("Aus der Bibliothek entfernen", remembered as excluded) are not listed again;
//! they are only counted. Folders whose names look like a trash, export, backup, Photo Booth or Picasa folder get a
//! [`Hint`]. Files with the same name and size as a photo of the library are counted as probably already there.
//!
//! The search is read-only (folder names, file names, sizes). "Add" creates catalog entries and
//! syncs; the photos themselves are not touched. Folders one does not want in the catalog
//! can be remembered ("Ignore", in the table `info`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::db::Db;

/// A folder with photos the catalog does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub root: i64,
    /// root folder, absolute, with trailing `/`
    pub root_path: String,
    /// path relative to the root folder (`a/b/`, never empty: the root folder itself is in the catalog)
    pub rel: String,
    pub count: usize,
    pub bytes: u64,
    /// formats with count, most frequent first
    pub formats: Vec<(String, usize)>,
    pub reason: Reason,
    pub hint: Option<Hint>,
    /// files with the same name and size as a photo of the library (probably duplicates)
    pub known: usize,
}

/// Why the folder is not in the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// root folder from the Lightroom import: the sync does not add unknown folders there
    LightroomRoot,
    /// own root folder: the next sync adds it
    NewInOwnRoot,
}

/// What the folder name suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Trash,
    Export,
    Backup,
    PhotoBooth,
    Picasa,
}

/// Hint from the folder path (relative to the root folder).
pub fn hint_of(rel: &str) -> Option<Hint> {
    let parts: Vec<String> = rel.split('/').filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect();
    let any = |words: &[&str]| parts.iter().any(|p| words.iter().any(|w| p.contains(w)));
    if any(&["photo booth"]) {
        Some(Hint::PhotoBooth)
    } else if any(&["picasa"]) {
        Some(Hint::Picasa)
    } else if any(&["trash", "papierkorb", "gelöscht", "geloescht", "deleted"]) {
        Some(Hint::Trash)
    } else if any(&["export"]) {
        Some(Hint::Export)
    } else if any(&["backup", "sicherung", "kopie", "copy"]) {
        Some(Hint::Backup)
    } else {
        None
    }
}

/// Result of a search with what was left out.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub entries: Vec<Entry>,
    /// root folders on other drives not searched this time
    pub skipped: Vec<i64>,
    /// folders hidden with "Nicht mehr anzeigen"
    pub ignored: usize,
    /// folders removed from the library earlier (not listed again)
    pub removed: usize,
}

impl Entry {
    pub fn path(&self) -> PathBuf {
        Path::new(&self.root_path).join(&self.rel)
    }

    /// Unique key (absolute path with `/`), also for "Ignore".
    pub fn cache_key(&self) -> String {
        format!("{}{}", self.root_path, self.rel)
    }
}

const IGNORED: &str = "nik_ignoriert";

pub fn ignored_list(db: &Db) -> HashSet<String> {
    db.info(IGNORED).map(|t| t.lines().filter(|l| !l.is_empty()).map(String::from).collect()).unwrap_or_default()
}

/// Remember folders that should no longer be shown as "not in catalog".
pub fn ignore(db: &Db, cache_key: &[String]) -> Result<(), String> {
    let mut all: BTreeSet<String> = ignored_list(db).into_iter().collect();
    all.extend(cache_key.iter().cloned());
    with_retry(|| db.set_info(IGNORED, &all.iter().cloned().collect::<Vec<_>>().join("\n")).map_err(|e| e.to_string()))
}

/// Show all remembered folders again.
pub fn clear_ignored(db: &Db) -> Result<(), String> {
    with_retry(|| db.set_info(IGNORED, "").map_err(|e| e.to_string()))
}

/// Retry library writes while a background run locks the database
/// (up to about a minute). Other errors apply immediately.
pub fn with_retry<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut last_item = String::new();
    for attempt in 0..30 {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let locked = e.contains("locked") || e.contains("busy");
                if !locked || attempt == 29 {
                    return Err(e);
                }
                last_item = e;
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
    Err(last_item)
}

/// Scan the root folders. Unreachable root folders (NAS not connected) are skipped.
pub fn search(db: &Db, progress: &dyn Fn(&str)) -> Result<Vec<Entry>, String> {
    search_in(db, progress, true).map(|(e, _)| e)
}

/// [`search`]; `other_drives`: also root folders on other drives (NAS, external disks) – otherwise they are left out
/// and returned as skipped (their earlier result stays valid). Files in folders already in the catalog are not
/// looked at one by one (only their subfolders matter).
pub fn search_in(db: &Db, progress: &dyn Fn(&str), other_drives: bool) -> Result<(Vec<Entry>, Vec<i64>), String> {
    search_report(db, progress, other_drives).map(|r| (r.entries, r.skipped))
}

/// [`search_in`] with the counts of what was left out. `progress` gets "root folder · folders read · photos found".
pub fn search_report(db: &Db, progress: &dyn Fn(&str), other_drives: bool) -> Result<Report, String> {
    let stems: Vec<_> = db.roots().map_err(|e| e.to_string())?.into_iter().filter(|s| s.origin != crate::apple_photos::ORIGIN).collect();
    let all_folders = db.folder().map_err(|e| e.to_string())?;
    let ignored = ignored_list(db);
    let mut known: BTreeMap<i64, HashSet<String>> = BTreeMap::new();
    for o in all_folders {
        known.entry(o.root).or_default().insert(o.path);
    }
    let empty = HashSet::new();
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    let (mut n_ignored, mut n_removed) = (0usize, 0usize);
    // name (lower case) and size of the library's photos: "probably already there"
    let library_files: HashSet<(String, i64)> = {
        let mut st = db.conn.prepare("SELECT lower(name), size FROM file WHERE size > 0").map_err(|e| e.to_string())?;
        let v = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(|e| e.to_string())?.flatten().collect();
        v
    };
    for s in stems {
        let root_dir = PathBuf::from(&s.path);
        if !other_drives && !crate::platform::on_startup_volume(&root_dir) {
            skipped.push(s.id);
            continue;
        }
        if !root_dir.is_dir() {
            continue;
        }
        // "root name · folders read · photos found so far" (the app shows it as the current step)
        let found = |out: &Vec<Entry>| out.iter().map(|e| e.count).sum::<usize>();
        progress(&format!("{} · 0 · {}", s.name, found(&out)));
        let known_set = known.get(&s.id).unwrap_or(&empty);
        let excluded = crate::manage::exclusions(&db.conn, s.id);
        let reason = if s.origin == "lightroom" { Reason::LightroomRoot } else { Reason::NewInOwnRoot };
        let mut batch = vec![String::new()];
        let mut read = 0usize;
        while let Some(rel) = batch.pop() {
            read += 1;
            if read % 50 == 0 {
                progress(&format!("{} · {read} · {}", s.name, found(&out)));
            }
            let Ok(rd) = std::fs::read_dir(root_dir.join(&rel)) else { continue };
            // a folder in the catalog: only its subfolders are of interest (no size of every file, which cost a
            // metadata query per photo of the whole library on every run)
            let known_here = rel.is_empty() || known_set.contains(&rel);
            let (mut count, mut bytes, mut known_files) = (0usize, 0u64, 0usize);
            let mut formats: BTreeMap<String, usize> = BTreeMap::new();
            for e in rd.flatten() {
                let name = crate::nfc(&e.file_name().to_string_lossy());
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if !crate::reconcile::skip(&name) {
                        batch.push(format!("{rel}{name}/"));
                    }
                } else if !known_here && ft.is_file() && !name.starts_with('.') {
                    if let Some(f) = crate::format_from_extension(&name) {
                        count += 1;
                        let len = e.metadata().map_or(0, |m| m.len());
                        bytes += len;
                        if library_files.contains(&(name.to_lowercase(), len as i64)) {
                            known_files += 1;
                        }
                        *formats.entry(f.to_string()).or_default() += 1;
                    }
                }
            }
            if count == 0 || rel.is_empty() || known_set.contains(&rel) {
                continue;
            }
            // removed from the library on purpose: not offered again
            if crate::manage::is_excluded(&excluded, &rel) {
                n_removed += 1;
                continue;
            }
            let entry = {
                let mut f: Vec<(String, usize)> = formats.into_iter().collect();
                f.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let hint = hint_of(&rel);
                Entry { root: s.id, root_path: s.path.clone(), rel, count, bytes, formats: f, reason, hint, known: known_files }
            };
            if ignored.contains(&entry.cache_key()) {
                n_ignored += 1;
            } else {
                out.push(entry);
            }
        }
    }
    out.sort_by(|a, b| a.cache_key().to_lowercase().cmp(&b.cache_key().to_lowercase()));
    Ok(Report { entries: out, skipped, ignored: n_ignored, removed: n_removed })
}

/// Add a folder to the catalog (create the entry, then sync). Returns: new images.
/// The photos themselves stay untouched.
pub fn add(db: &Db, selection: &[(i64, String)], progress: &dyn Fn(&str)) -> Result<usize, String> {
    let mut stems = BTreeSet::new();
    for (root, rel) in selection {
        with_retry(|| db.create_folder(*root, rel, None).map(|_| ()).map_err(|e| e.to_string()))?;
        stems.insert(*root);
    }
    let mut new = 0;
    for s in stems {
        let b = with_retry(|| crate::reconcile::reconcile(db, Some(s), progress))?;
        new += b.newly_added;
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_hints_removed_and_known_files() {
        let dir = PathBuf::from(format!("/private/tmp/graineo-nik-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let photos = dir.join("photos");
        for sub in ["2024", "Export/web", "Import/Trash", "Weg"] {
            std::fs::create_dir_all(photos.join(sub)).unwrap();
        }
        std::fs::write(photos.join("2024/A.JPG"), b"12345").unwrap();
        std::fs::write(photos.join("Export/web/B.JPG"), b"1").unwrap();
        std::fs::write(photos.join("Import/Trash/C.RAF"), b"1").unwrap();
        std::fs::write(photos.join("Weg/D.JPG"), b"1").unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        let root = db.create_root(&format!("{}/", photos.display()), "photos", "lightroom", None).unwrap();
        let o = db.create_folder(root, "", None).unwrap();
        // a photo of the library with the name and size of 2024/A.JPG
        db.conn.execute("INSERT INTO file(folder, name, format, size) VALUES(?1, 'a.jpg', 'JPG', 5)", [o]).unwrap();
        db.conn.execute("INSERT INTO excluded_path(root, rel, time) VALUES(?1, 'Weg/', 0)", [root]).unwrap();
        ignore(&db, &[format!("{}/Export/web/", photos.display())]).unwrap();
        let r = search_report(&db, &|_| {}, true).unwrap();
        let rels: Vec<&str> = r.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["2024/", "Import/Trash/"]);
        assert_eq!((r.ignored, r.removed), (1, 1));
        assert!(r.entries.iter().all(|e| e.reason == Reason::LightroomRoot));
        assert_eq!(r.entries[0].known, 1);
        assert_eq!(r.entries[1].hint, Some(Hint::Trash));
        assert_eq!(hint_of("Pictures/Photo Booth Library/Pictures/"), Some(Hint::PhotoBooth));
        assert_eq!(hint_of("2010/Picasa Exports/"), Some(Hint::Picasa));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
