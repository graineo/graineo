//! "Locate missing folder": a folder (or root folder) of the library was moved or renamed in the Finder; its old path
//! no longer exists. The user picks the folder where it is now; the library is linked to it again. Nothing on disk
//! changes: no copy, no move, no write into originals – only the library (and the keys of the previews).
//!
//! Sequence:
//! 1. [`plan`]: reads the chosen folder once (with subfolders) and compares it with the photos the library knows in the
//!    missing folder: same relative path and name, same size and (within 2 s) the same modification time. Checks: the
//!    old folder is really gone (and its location connected), the chosen folder is not known to the library yet (no
//!    root folder at or below it, no known folder there), at least one photo found.
//! 2. [`execute`]: in one transaction: check again, switch the folder over (same file and image IDs: ratings, keywords,
//!    collections, development, faces and virtual copies stay; subfolders move along), mark the photos found as present
//!    (with size and modification time from the disk). Then the previews move to their new key. Afterwards the app runs
//!    a sync of the root folder (photos not found stay "missing", photos new in the folder are added).
//!
//! Library model as for "Move to …" ([`crate::move_folder`]): a root folder that is now outside all root folders keeps
//! its entry and gets the new path (and the new name, if it carried the folder name); a folder that is now inside a
//! root folder becomes part of it; a subfolder that is now outside all root folders becomes a root folder of its own.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, OptionalExtension};

use crate::db::Db;
use crate::preview::{self, Cache};

fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Modification times within this many seconds count as equal (FAT/exFAT and some network drives round to 2 s).
const MTIME_TOLERANCE: i64 = 2;

/// Volume root of a path: `/Volumes/<name>` on macOS, the drive (`C:\`) on Windows; `None`: the startup volume.
pub fn volume_root(p: &Path) -> Option<PathBuf> {
    let mut c = p.components();
    match c.next()? {
        Component::Prefix(px) => Some(PathBuf::from(format!("{}\\", px.as_os_str().to_string_lossy()))),
        Component::RootDir => match (c.next(), c.next()) {
            (Some(Component::Normal(v)), Some(Component::Normal(name))) if v == "Volumes" => Some(Path::new("/Volumes").join(name)),
            _ => None,
        },
        _ => None,
    }
}

/// Is the volume of this path connected? A leftover empty folder `/Volumes/<name>` without a mounted volume (same
/// device as the startup volume) does not count.
pub fn volume_present(p: &Path) -> bool {
    let Some(v) = volume_root(p) else { return true };
    if !v.is_dir() {
        return false;
    }
    if v.starts_with("/Volumes") {
        let link = std::fs::symlink_metadata(&v).is_ok_and(|m| m.file_type().is_symlink());
        if !link {
            if let (Some(a), Some(b)) = (crate::platform::device(&v), crate::platform::device(Path::new("/"))) {
                return a != b;
            }
        }
    }
    true
}

/// Is a missing folder of the library something to locate? Its path is gone, but its volume is connected (otherwise
/// the location is merely not reachable – nothing to relink).
pub fn locatable(folder: &Path) -> bool {
    !folder.exists() && volume_present(folder)
}

/// How a photo of the library was found in the chosen folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// same name, size and modification time
    Exact,
    /// same name and size, other modification time (e.g. a copy tool that did not keep it)
    OtherTime,
    /// same name, other size: not linked as found (stays "missing" until the sync decides)
    OtherSize,
    /// not there
    Absent,
}

/// A photo (file of the library) of the missing folder.
#[derive(Clone, Debug)]
pub struct Photo {
    pub file: i64,
    /// path relative to the missing folder (`sub/IMG_1.CR3`)
    pub rel: String,
    /// path before, as the library forms it (root + folder + name)
    pub old_path: String,
    pub size: Option<i64>,
    pub mtime: Option<i64>,
    pub orientation: Option<i64>,
    pub found: Found,
    /// size and modification time in the chosen folder (if there)
    pub disk: Option<(i64, i64)>,
}

impl Photo {
    pub fn is_found(&self) -> bool {
        matches!(self.found, Found::Exact | Found::OtherTime)
    }
}

/// Result of [`plan`] (for the confirmation).
#[derive(Clone, Debug)]
pub struct Plan {
    pub folder: i64,
    pub root: i64,
    /// root folder of Lightroom (a later Lightroom import takes the path from the catalog again)
    pub lightroom: bool,
    /// the whole root folder is relinked
    pub is_root: bool,
    /// old (missing) path, without '/' at the end
    pub old: PathBuf,
    /// chosen folder
    pub target: PathBuf,
    pub photos: Vec<Photo>,
    /// photo files in the chosen folder the library does not know yet (the sync adds them afterwards)
    pub extra: usize,
    pub changes: Vec<crate::relocate::Change>,
    /// new name of the root folder (it carried the old folder name)
    pub rename_root: Option<String>,
}

impl Plan {
    pub fn total(&self) -> usize {
        self.photos.len()
    }
    pub fn count(&self, k: Found) -> usize {
        self.photos.iter().filter(|p| p.found == k).count()
    }
    pub fn found(&self) -> usize {
        self.photos.iter().filter(|p| p.is_found()).count()
    }
    /// every photo found, same size and time
    pub fn complete(&self) -> bool {
        self.count(Found::Exact) == self.total()
    }
}

/// All files below `dir`: relative path (NFC, '/') → (bytes, modification time). Links are not followed, `.DS_Store`
/// stays out.
fn walk(dir: &Path) -> Result<HashMap<String, (i64, i64)>, String> {
    let mut out = HashMap::new();
    let mut todo = vec![String::new()];
    while let Some(rel) = todo.pop() {
        let abs = if rel.is_empty() { dir.to_path_buf() } else { dir.join(&rel) };
        let rd = std::fs::read_dir(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        for e in rd.flatten() {
            let name = crate::nfc(&e.file_name().to_string_lossy());
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                todo.push(format!("{rel}{name}/"));
            } else if ft.is_file() {
                if name == ".DS_Store" {
                    continue;
                }
                let Ok(m) = e.metadata() else { continue };
                let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
                out.insert(format!("{rel}{name}"), (m.len() as i64, mtime));
            }
        }
    }
    Ok(out)
}

/// Photos of a folder with its subfolders: (file, relative path of its folder in the root, name, size, mtime,
/// orientation).
type Row = (i64, String, String, Option<i64>, Option<i64>, Option<i64>);

fn rows(db: &Db, root: i64, rel: &str) -> Result<Vec<Row>, String> {
    let mut st = db
        .conn
        .prepare(
            "SELECT d.id, o.path, d.name, d.size, d.mtime, d.orientation
             FROM file d JOIN folder o ON o.id = d.folder
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

/// Folder of the library: (root, relative path, root path, root name, origin, Lightroom root).
fn folder_info(db: &Db, folder: i64) -> Result<(i64, String, String, String, String, bool), String> {
    db.conn
        .query_row(
            "SELECT o.root, o.path, s.path, s.name, s.origin, s.lr_id IS NOT NULL FROM folder o JOIN root_folder s ON s.id = o.root WHERE o.id = ?1",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(f)?
        .ok_or_else(|| "Ordner unbekannt".to_string())
}

/// Plan relinking the missing folder `folder` to `target`. Only reads (the library and the chosen folder).
pub fn plan(db: &Db, folder: i64, target: &Path) -> Result<Plan, String> {
    let (root, rel, root_path, root_name, origin, lr) = folder_info(db, folder)?;
    if origin == crate::apple_photos::ORIGIN || crate::apple_photos::in_photos_library(Path::new(&root_path)) {
        return Err("Ordner der Mediathek von Apple Fotos werden nicht neu verknüpft".into());
    }
    if crate::apple_photos::in_photos_library(target) {
        return Err("Die Mediathek von Apple Fotos lässt sich nicht als Ordner verknüpfen".into());
    }
    let old = PathBuf::from(format!("{root_path}{rel}").trim_end_matches('/'));
    if old.exists() {
        return Err(format!("{} gibt es noch – der Ordner fehlt nicht", old.display()));
    }
    if !volume_present(&old) {
        return Err(format!("Der Speicherort von {} ist nicht verbunden – bitte zuerst verbinden; die Fotos fehlen dann nicht", old.display()));
    }
    if !rel.is_empty() && !Path::new(&root_path).is_dir() {
        return Err(format!("Der Stammordner {} fehlt – bitte zuerst ihn suchen", root_path.trim_end_matches('/')));
    }
    if !target.is_absolute() || !target.is_dir() {
        return Err(format!("{} ist kein Ordner", target.display()));
    }
    let name = target.file_name().map(|n| crate::nfc(&n.to_string_lossy())).ok_or("Ordner ohne Namen")?;
    let list = rows(db, root, &rel)?;
    if list.is_empty() {
        return Err("Die Bibliothek kennt in diesem Ordner keine Fotos – nichts zu verknüpfen".into());
    }
    let changes = crate::move_folder::library_changes(db, root, &rel, &root_path, &root_name, &origin, &old, target, &name).map_err(|e| format!("{e}. Der gewählte Ordner ist schon in der Bibliothek – bitte dort die Einträge prüfen oder einen anderen Ordner wählen."))?;
    let disk = walk(target)?;
    let mut known: HashSet<String> = HashSet::new();
    let photos: Vec<Photo> = list
        .into_iter()
        .map(|r| {
            let rel_photo = format!("{}{}", &r.1[rel.len()..], crate::nfc(&r.2));
            let d = disk.get(&rel_photo).copied();
            let found = match d {
                None => Found::Absent,
                Some((sz, _)) if r.3.is_some_and(|s| s != sz) => Found::OtherSize,
                Some((_, mt)) if r.4.is_some_and(|m| (m - mt).abs() > MTIME_TOLERANCE) => Found::OtherTime,
                Some(_) => Found::Exact,
            };
            known.insert(rel_photo.clone());
            Photo { file: r.0, old_path: format!("{root_path}{}{}", r.1, r.2), rel: rel_photo, size: r.3, mtime: r.4, orientation: r.5, found, disk: d }
        })
        .collect();
    let extra = disk.keys().filter(|k| !known.contains(*k) && crate::format_from_extension(k).is_some() && !k.rsplit('/').next().is_some_and(|n| n.starts_with('.'))).count();
    let p = Plan {
        folder,
        root,
        lightroom: lr || origin == "lightroom",
        is_root: rel.is_empty(),
        rename_root: (rel.is_empty() && old.file_name().is_some_and(|n| crate::nfc(&n.to_string_lossy()) == root_name) && root_name != name).then(|| name.clone()),
        old,
        target: target.to_path_buf(),
        photos,
        extra,
        changes,
    };
    if p.found() == 0 {
        return Err(format!(
            "Im gewählten Ordner ist keines der {} Fotos aus „{}“ (gleicher Name und gleiche Größe) – wohl der falsche Ordner. Nichts wurde verknüpft.",
            p.total(),
            p.old.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        ));
    }
    Ok(p)
}

/// Result of [`execute`].
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// photos marked present at the new place
    pub found: usize,
    /// photos still missing (not in the chosen folder or with another size)
    pub not_found: usize,
    /// preview entries moved to the new key
    pub previews_moved: usize,
    /// root folder of the folder afterwards (for the sync)
    pub root: i64,
}

/// Relink as planned (see the module description). Checks again in the transaction: if the library changed meanwhile
/// (the folder was linked elsewhere, the target became known), nothing happens.
pub fn execute(db: &Db, plan: &Plan, cache: Option<&Cache>) -> Result<Outcome, String> {
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let (root, rel, root_path, root_name, origin, _) = folder_info(db, plan.folder)?;
    let old = PathBuf::from(format!("{root_path}{rel}").trim_end_matches('/'));
    if root != plan.root || old != plan.old {
        return Err("Die Bibliothek hat sich inzwischen geändert – bitte noch einmal suchen".into());
    }
    let name = plan.target.file_name().map(|n| crate::nfc(&n.to_string_lossy())).ok_or("Ordner ohne Namen")?;
    let changes = crate::move_folder::library_changes(db, root, &rel, &root_path, &root_name, &origin, &old, &plan.target, &name)?;
    if changes != plan.changes {
        return Err("Die Bibliothek hat sich inzwischen geändert – bitte noch einmal suchen".into());
    }
    crate::relocate::apply_tx(&tx, &changes, false)?;
    if let Some(n) = &plan.rename_root {
        tx.execute("UPDATE root_folder SET name = ?2 WHERE id = ?1", params![root, n]).map_err(f)?;
    }
    let mut out = Outcome::default();
    for p in &plan.photos {
        match (p.is_found(), p.disk) {
            (true, Some((sz, mt))) => {
                tx.execute("UPDATE file SET status = 0, size = ?2, mtime = ?3 WHERE id = ?1 AND status IN (0, 1, 2)", params![p.file, sz, mt]).map_err(f)?;
                out.found += 1;
            }
            _ => {
                // the sync decides (missing, or present with a changed file); the location is reachable now
                tx.execute("UPDATE file SET status = 1 WHERE id = ?1 AND status IN (0, 2)", [p.file]).map_err(f)?;
                out.not_found += 1;
            }
        }
    }
    let new_root: i64 = tx.query_row("SELECT o.root FROM folder o WHERE o.id = ?1", [plan.folder], |r| r.get(0)).map_err(f)?;
    tx.commit().map_err(f)?;
    out.root = new_root;
    // previews: old key (old path, old size/time) → new key (new path, size/time from the disk)
    let standard;
    let cache = match cache {
        Some(c) => c,
        None => {
            standard = Cache::standard();
            &standard
        }
    };
    if let Ok(mut st) = db.conn.prepare("SELECT s.path || o.path || d.name FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1") {
        for p in plan.photos.iter().filter(|p| p.is_found()) {
            let (Some((sz, mt)), Ok(new_path)) = (p.disk, st.query_row([p.file], |r| r.get::<_, String>(0))) else { continue };
            let old_key = preview::cache_key_with_orientation(preview::cache_key(&p.old_path, p.size.unwrap_or(0), p.mtime.unwrap_or(0)), p.orientation);
            let new_key = preview::cache_key_with_orientation(preview::cache_key(&new_path, sz, mt), p.orientation);
            out.previews_moved += cache.rekey(old_key, new_key);
        }
    }
    Ok(out)
}
