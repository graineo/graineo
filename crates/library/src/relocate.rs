//! Moving and renaming on disk – including sidecars, with a log (journal) for
//! abort/resume and ⌘Z.
//!
//! Taken along: XMP (`NAME.xmp` or `NAME.EXT.xmp`) and the same-named camera JPEG of a RAW.
//!
//! Same drive: rename without overwriting (`RENAME_EXCL`, atomic). Other drive: copy into a
//! newly created side file → `fsync` → compare SHA-256 of both sides → install without overwriting
//! → only then delete the source. Nothing is ever overwritten: if something already exists at the target
//! (even only at the last moment), the step aborts.
//!
//! All or nothing: if a step fails, the steps already executed are reverted in reverse
//! order so that files and library match.
//!
//! Journal: `~/Library/Application Support/<APP>/journal/<id>.json` is written before the first step
//! and updated after each step (forwards and backwards). An interrupted operation –
//! including an interrupted revert – continues at the next start (steps are repeatable:
//! source gone + target present = done; with a known size only if the target matches it).

use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::db::Db;

fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Upper limit for the steps of a journal (protects against damaged or planted files).
const MAX_STEPS: usize = 200_000;

/// New file or folder name: exactly one path component.
pub fn single_name(n: &str) -> Result<&str, String> {
    let n = n.trim();
    if n.is_empty() || n == "." || n == ".." || n.contains('/') || n.contains('\0') || n.len() > 255 {
        return Err(format!("ungültiger Name: „{n}“"));
    }
    Ok(n)
}

/// Paths from journals: absolute and without `.`/`..`.
fn path_ok(p: &Path) -> Result<(), String> {
    use std::path::Component;
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) || p.to_string_lossy().contains('\0') {
        return Err(format!("Pfad nicht zulässig: {}", p.display()));
    }
    Ok(())
}

pub fn journal_folder() -> PathBuf {
    std::env::var_os("GRAINEO_LIBRARY_JOURNAL").map(PathBuf::from).unwrap_or_else(|| crate::data_dir().join("journal"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub from: PathBuf,
    pub by_key: PathBuf,
    pub finished: bool,
    /// size of the source file at planning time (files only): detects a foreign file at the target when resuming
    pub size: Option<u64>,
    /// copy only (verified), the source stays: for moving a folder between locations, where the source goes to the
    /// trash as a whole only after the library has been switched over ([`crate::move_folder`])
    pub keep: bool,
}

impl Step {
    pub(crate) fn new(from: PathBuf, by_key: PathBuf) -> Step {
        let size = std::fs::metadata(&from).ok().filter(|m| m.is_file()).map(|m| m.len());
        Step { from, by_key, finished: false, size, keep: false }
    }

    /// Verified copy that leaves the source in place (see [`Step::keep`]).
    pub(crate) fn copy(from: PathBuf, by_key: PathBuf) -> Step {
        Step { keep: true, ..Step::new(from, by_key) }
    }
}

/// Database change of an operation (for execute and undo).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// file: (ID, old folder, old name, new folder, new name)
    File(i64, i64, String, i64, String),
    /// folder (root, old relative path, new relative path) – also affects subfolders
    Folder(i64, String, String),
    /// root folder gets a new absolute path (root ID, old path, new path; with '/' at the end)
    RootPath(i64, String, String),
    /// folder with its subfolders into another (or the same) root folder: (source root path, source prefix,
    /// target root path, target prefix). Root folders are named by path, because a root created in the same operation
    /// gets its ID only when the change is applied.
    Subtree(String, String, String, String),
    /// new root folder (path, name); reverse: removed again if it has no folders left
    CreateRoot(String, String),
    /// root folder without folders is removed (path, name, origin); reverse: created again
    DropRoot(String, String, String),
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub id: String,
    pub title: String,
    pub steps: Vec<Step>,
    pub changes: Vec<Change>,
    /// "planned", "running", "done", "undone", "failed" (everything reverted) plus the interrupted
    /// states "rolling_back" (revert after an error) and "undoing" (⌘Z); a folder move between
    /// locations additionally "cleanup" (library switched over, the source still has to go to the trash)
    pub status: String,
    pub message: String,
    /// "" (move/rename) or [`crate::move_folder::KIND`]
    pub kind: String,
    /// folder move between locations: (source folder, its former root folder) – goes to the trash after the switch
    pub cleanup: Option<(PathBuf, PathBuf)>,
}

impl Operation {
    fn json(&self) -> Value {
        json!({
            "id": self.id, "title": self.title, "status": self.status, "message": self.message,
            "kind": self.kind,
            "cleanup": self.cleanup.as_ref().map(|(a, b)| json!({"source": a, "root": b})),
            "steps": self.steps.iter().map(|s| json!({"from": s.from, "to": s.by_key, "done": s.finished, "size": s.size, "keep": s.keep})).collect::<Vec<_>>(),
            "db": self.changes.iter().map(|a| match a {
                Change::File(id, ao, old_name, no, nn) => json!({"file": id, "oldFolder": ao, "oldName": old_name, "newFolder": no, "newName": nn}),
                Change::Folder(s, a, n) => json!({"root": s, "oldPath": a, "newPath": n}),
                Change::RootPath(id, a, n) => json!({"rootId": id, "oldRootPath": a, "newRootPath": n}),
                Change::Subtree(a, b, c, d) => json!({"subtree": [a, b, c, d]}),
                Change::CreateRoot(p, n) => json!({"createRoot": [p, n]}),
                Change::DropRoot(p, n, o) => json!({"dropRoot": [p, n, o]}),
            }).collect::<Vec<_>>(),
        })
    }
    fn from_json(v: &Value) -> Option<Operation> {
        // The identifier becomes the file name in the journal folder: digits only, never path components
        let id = v["id"].as_str().filter(|i| !i.is_empty() && i.len() <= 40 && i.bytes().all(|b| b.is_ascii_digit()))?;
        let steps = v["steps"].as_array().filter(|s| s.len() <= MAX_STEPS)?;
        let strings = |a: &Value, n: usize| -> Option<Vec<String>> {
            let v: Vec<String> = a.as_array()?.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
            (v.len() == n).then_some(v)
        };
        let cleanup = match (v["cleanup"]["source"].as_str(), v["cleanup"]["root"].as_str()) {
            (Some(a), Some(b)) => {
                let (a, b) = (PathBuf::from(a), PathBuf::from(b));
                path_ok(&a).ok()?;
                path_ok(&b).ok()?;
                Some((a, b))
            }
            _ => None,
        };
        Some(Operation {
            id: id.to_string(),
            title: v["title"].as_str().unwrap_or("").to_string(),
            // journals of graineo 1.0 name the states in German
            status: legacy_names::translate(legacy_names::files::JOURNAL_STATUS, v["status"].as_str().unwrap_or("")).to_string(),
            message: v["message"].as_str().unwrap_or("").to_string(),
            kind: v["kind"].as_str().unwrap_or("").to_string(),
            cleanup,
            steps: steps.iter().filter_map(|s| Some(Step { from: PathBuf::from(s["from"].as_str()?), by_key: PathBuf::from(s["to"].as_str()?), finished: s["done"].as_bool().unwrap_or(false), size: s["size"].as_u64(), keep: s["keep"].as_bool().unwrap_or(false) })).collect(),
            changes: v["db"]
                .as_array()?
                .iter()
                .filter_map(|a| {
                    if let Some(id) = a["file"].as_i64() {
                        Some(Change::File(id, a["oldFolder"].as_i64()?, a["oldName"].as_str()?.into(), a["newFolder"].as_i64()?, a["newName"].as_str()?.into()))
                    } else if let Some(id) = a["rootId"].as_i64() {
                        Some(Change::RootPath(id, a["oldRootPath"].as_str()?.into(), a["newRootPath"].as_str()?.into()))
                    } else if let Some(x) = strings(&a["subtree"], 4) {
                        Some(Change::Subtree(x[0].clone(), x[1].clone(), x[2].clone(), x[3].clone()))
                    } else if let Some(x) = strings(&a["createRoot"], 2) {
                        Some(Change::CreateRoot(x[0].clone(), x[1].clone()))
                    } else if let Some(x) = strings(&a["dropRoot"], 3) {
                        Some(Change::DropRoot(x[0].clone(), x[1].clone(), x[2].clone()))
                    } else {
                        Some(Change::Folder(a["root"].as_i64()?, a["oldPath"].as_str()?.into(), a["newPath"].as_str()?.into()))
                    }
                })
                .collect(),
        })
    }
    /// Write the journal entry (atomic).
    pub fn save(&self) -> Result<(), String> {
        let d = journal_folder();
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let p = d.join(format!("{}.json", self.id));
        crate::files::write_atomic(&p, serde_json::to_string_pretty(&self.json()).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())
    }
}

/// Sidecars of an image that move along: (old path, new path at the new location/root).
pub fn companions(image: &Path, target: &Path) -> Vec<(PathBuf, PathBuf)> {
    let (Some(dir), Some(name), Some(stem)) = (image.parent(), image.file_name().map(|n| n.to_string_lossy().to_string()), image.file_stem().map(|n| n.to_string_lossy().to_string())) else { return Vec::new() };
    let (Some(dst_dir), Some(dst_name), Some(dst_stem)) = (target.parent(), target.file_name().map(|n| n.to_string_lossy().to_string()), target.file_stem().map(|n| n.to_string_lossy().to_string())) else { return Vec::new() };
    let fm = crate::format_from_extension(&name).unwrap_or("");
    let raw = crate::is_raw(fm);
    let mut v: Vec<(PathBuf, PathBuf)> = Vec::new();
    let siblings: Vec<String> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
    let raw_beside = !raw && siblings.iter().any(|n| Path::new(n).file_stem().is_some_and(|s| s.to_string_lossy() == stem) && crate::format_from_extension(n).is_some_and(crate::is_raw));
    // XMP
    v.push((dir.join(format!("{name}.xmp")), dst_dir.join(format!("{dst_name}.xmp"))));
    if raw || !raw_beside {
        for n in &siblings {
            if n.eq_ignore_ascii_case(&format!("{stem}.xmp")) {
                v.push((dir.join(n), dst_dir.join(format!("{dst_stem}.xmp"))));
            }
        }
    }
    // camera JPEG of a RAW (same name)
    if raw {
        for n in &siblings {
            let p = Path::new(n);
            if p.file_stem().is_some_and(|s| s.to_string_lossy() == stem) && crate::format_from_extension(n) == Some("JPG") {
                let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
                v.push((dir.join(n), dst_dir.join(format!("{dst_stem}.{ext}"))));
                // the JPEG's own sidecar (`NAME.JPG.xmp`) moves with it
                v.push((dir.join(format!("{n}.xmp")), dst_dir.join(format!("{dst_stem}.{ext}.xmp"))));
            }
        }
    }
    v.retain(|(a, _)| a.exists());
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

/// Same drive?
fn same_volume(a: &Path, target_dir: &Path) -> bool {
    // Test hook: force the copy path (copy → verify → delete) even on the same drive
    if std::env::var_os("GRAINEO_MOVE_COPY").is_some() && a.is_file() {
        return false;
    }
    let mut z = target_dir.to_path_buf();
    while !z.exists() {
        match z.parent() {
            Some(p) => z = p.to_path_buf(),
            None => return false,
        }
    }
    match (crate::platform::device(a), crate::platform::device(&z)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

pub fn sha256(p: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Execute one step (repeatable). Never overwrites anything.
fn step(s: &Step) -> Result<(), String> {
    step_with(s, &mut |_| {})
}

/// Verified copy into a new side file next to the target: the source is read once (copy + SHA-256), the copy is
/// read back and compared; the modification time of the source is kept (the library and the preview key stay valid).
/// Returns: the side file (not yet installed).
pub(crate) fn verified_copy(from: &Path, by_key: &Path, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String> {
    let (tmp, a) = crate::files::side_copy_with_sum(from, by_key, bytes).map_err(|e| format!("Kopie {}: {e}", from.display()))?;
    if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
        let _ = std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
    }
    match sha256(&tmp) {
        Ok(b) if b == a => Ok(tmp),
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("Prüfsumme der Kopie stimmt nicht: {}", from.display()))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Like [`step`]; `bytes` receives the amount copied of this step so far.
pub(crate) fn step_with(s: &Step, bytes: &mut dyn FnMut(u64)) -> Result<(), String> {
    use std::io::ErrorKind::AlreadyExists;
    path_ok(&s.from)?;
    path_ok(&s.by_key)?;
    let occupied = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let taken = || format!("Ziel existiert schon: {}", s.by_key.display());
    if s.keep {
        // copy only: resume = the same file is already at the target (size and checksum)
        if occupied(&s.by_key) {
            let same = std::fs::symlink_metadata(&s.by_key).is_ok_and(|m| m.is_file() && Some(m.len()) == s.size) && sha256(&s.from).ok().is_some_and(|a| sha256(&s.by_key).ok().as_deref() == Some(a.as_str()));
            return if same { Ok(()) } else { Err(taken()) };
        }
        let dst_dir = s.by_key.parent().ok_or("kein Zielordner")?;
        std::fs::create_dir_all(dst_dir).map_err(|e| format!("{}: {e}", dst_dir.display()))?;
        let tmp = verified_copy(&s.from, &s.by_key, bytes)?;
        return crate::files::place_sibling_file(&tmp, &s.by_key).map_err(|e| if e.kind() == AlreadyExists { taken() } else { e.to_string() });
    }
    if !s.from.exists() && occupied(&s.by_key) {
        // resume: already arrived – but is it the same file, not a foreign one?
        if let (Some(g), Ok(m)) = (s.size, std::fs::metadata(&s.by_key)) {
            if m.is_file() && m.len() != g {
                return Err(format!("Am Ziel liegt eine andere Datei als erwartet (Größe): {}", s.by_key.display()));
            }
        }
        return Ok(());
    }
    if occupied(&s.by_key) {
        // resume of a move between locations whose source could not be removed after the verified copy: the copy at
        // the target is ours when it matches the source byte for byte – then only the source has to go
        let ours = std::fs::symlink_metadata(&s.by_key).is_ok_and(|m| m.is_file()) && std::fs::symlink_metadata(&s.from).is_ok_and(|m| m.is_file()) && sha256(&s.from).ok().is_some_and(|a| sha256(&s.by_key).ok().as_deref() == Some(a.as_str()));
        if ours {
            std::fs::remove_file(&s.from).map_err(|e| format!("Quelle nicht gelöscht {}: {e}", s.from.display()))?;
            return Ok(());
        }
        return Err(taken());
    }
    let dst_dir = s.by_key.parent().ok_or("kein Zielordner")?;
    std::fs::create_dir_all(dst_dir).map_err(|e| format!("{}: {e}", dst_dir.display()))?;
    if same_volume(&s.from, dst_dir) {
        crate::files::rename_no_replace(&s.from, &s.by_key).map_err(|e| if e.kind() == AlreadyExists { taken() } else { format!("{} → {}: {e}", s.from.display(), s.by_key.display()) })?;
    } else if s.from.is_dir() {
        return Err("Ordner über Laufwerke verschieben wird nicht unterstützt".into());
    } else {
        let tmp = verified_copy(&s.from, &s.by_key, bytes)?;
        crate::files::place_sibling_file(&tmp, &s.by_key).map_err(|e| if e.kind() == AlreadyExists { taken() } else { e.to_string() })?;
        std::fs::remove_file(&s.from).map_err(|e| format!("Quelle nicht gelöscht {}: {e}", s.from.display()))?;
    }
    Ok(())
}

/// Revert a finished copy step: remove the copy at the target – only while the source is still there and the copy
/// still has the size of the source (never a foreign file).
fn undo_copy(s: &Step) -> Result<(), String> {
    path_ok(&s.from)?;
    path_ok(&s.by_key)?;
    let Ok(m) = std::fs::symlink_metadata(&s.by_key) else { return Ok(()) };
    if !s.from.is_file() {
        return Err(format!("Quelle fehlt, die Kopie bleibt: {}", s.by_key.display()));
    }
    if !m.is_file() || Some(m.len()) != s.size {
        return Err(format!("Am Ziel liegt eine andere Datei als die Kopie: {}", s.by_key.display()));
    }
    std::fs::remove_file(&s.by_key).map_err(|e| format!("{}: {e}", s.by_key.display()))
}

pub(crate) fn new_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{n}")
}

/// Folder ID for an absolute target folder (must lie inside a root folder; is created).
fn target_folder(db: &Db, dir: &Path) -> Result<(i64, i64, String), String> {
    let s = db.root_to_path(dir).ok_or_else(|| format!("{} liegt in keinem Stammordner der Bibliothek", dir.display()))?;
    let abs = format!("{}/", crate::nfc(dir.to_string_lossy().trim_end_matches('/')));
    let rel = abs.strip_prefix(&s.path).unwrap_or("").to_string();
    let id = db.create_folder(s.id, &rel, None).map_err(f)?;
    Ok((id, s.id, rel))
}

/// Move files (including sidecars) into a folder or – with `new_name` – rename one file.
pub fn plan_files(db: &Db, files: &[i64], target_dir: &Path, new_name: Option<&str>) -> Result<Operation, String> {
    let (dst_id, _, _) = target_folder(db, target_dir)?;
    let mut v = Operation { id: new_id(), title: String::new(), steps: Vec::new(), changes: Vec::new(), status: "planned".into(), message: String::new(), kind: String::new(), cleanup: None };
    let mut seen = std::collections::HashSet::new();
    if crate::apple_photos::in_photos_library(target_dir) {
        return Err("Nie in die Mediathek von Apple Fotos verschieben".into());
    }
    crate::write_target::check(target_dir)?;
    for &d in files {
        let p = db.file_path(d).ok_or("Datei unbekannt")?;
        if crate::apple_photos::in_photos_library(&p) {
            return Err("Fotos aus Apple Fotos lassen sich hier nicht verschieben oder umbenennen".into());
        }
        let (ao, old_name): (i64, String) = db.conn.query_row("SELECT folder, name FROM file WHERE id=?1", [d], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?;
        let nn = match new_name {
            Some(n) if files.len() == 1 => {
                let n = single_name(n)?;
                // keep the extension
                let ext = Path::new(&old_name).extension().map(|e| e.to_string_lossy().to_string());
                match ext {
                    Some(e) if !n.to_lowercase().ends_with(&format!(".{}", e.to_lowercase())) => format!("{n}.{e}"),
                    _ => n.to_string(),
                }
            }
            _ => old_name.clone(),
        };
        let target = target_dir.join(&nn);
        if !seen.insert(p.clone()) {
            continue;
        }
        v.steps.push(Step::new(p.clone(), target.clone()));
        v.changes.push(Change::File(d, ao, old_name.clone(), dst_id, nn.clone()));
        for (a, b) in companions(&p, &target) {
            if seen.insert(a.clone()) {
                // camera JPEG as an image of its own in the library? Then update it there too.
                if let Some(bn) = b.file_name().map(|x| x.to_string_lossy().to_string()) {
                    let old_name2 = a.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    if let Ok(id2) = db.conn.query_row("SELECT id FROM file WHERE folder=?1 AND name=?2", params![ao, crate::nfc(&old_name2)], |r| r.get::<_, i64>(0)) {
                        v.changes.push(Change::File(id2, ao, old_name2, dst_id, bn));
                    }
                }
                v.steps.push(Step::new(a, b));
            }
        }
    }
    // a library row of the same name at the target (its file missing or not synced yet) would make the switch of the
    // library fail after the files were moved: refuse before anything moves
    for c in &v.changes {
        if let Change::File(id, _, _, dst, nn) = c {
            let taken: i64 = db.conn.query_row("SELECT count(*) FROM file WHERE folder = ?1 AND name = ?2 AND id <> ?3", params![dst, crate::nfc(nn), id], |r| r.get(0)).map_err(f)?;
            if taken > 0 {
                return Err(format!("In der Bibliothek gibt es am Ziel schon einen Eintrag „{nn}“ (Datei fehlt) – erst diesen Eintrag entfernen"));
            }
        }
    }
    v.title = match (files.len(), new_name) {
        (1, Some(n)) => format!("Umbenennen in {n}"),
        (n, _) => format!("{n} Foto(s) nach {}", target_dir.display()),
    };
    check(&v)?;
    Ok(v)
}

/// Rename a folder (same parent folder).
pub fn plan_folder(db: &Db, folder: i64, new_name: &str) -> Result<Operation, String> {
    let new_name = single_name(new_name).map_err(|_| "ungültiger Ordnername".to_string())?;
    let (root, rel, root_path): (i64, String, String) = db.conn.query_row("SELECT o.root, o.path, s.path FROM folder o JOIN root_folder s ON s.id=o.root WHERE o.id=?1", [folder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?;
    if rel.is_empty() {
        return Err("Stammordner werden nicht umbenannt".into());
    }
    if crate::apple_photos::in_photos_library(Path::new(&root_path)) {
        return Err("Ordner der Mediathek von Apple Fotos werden nicht umbenannt".into());
    }
    let old = PathBuf::from(format!("{root_path}{rel}"));
    let parent_rel = rel.trim_end_matches('/').rsplit_once('/').map_or(String::new(), |(a, _)| format!("{a}/"));
    let new_rel = crate::nfc(&format!("{parent_rel}{}/", new_name.trim()));
    let new = PathBuf::from(format!("{root_path}{new_rel}"));
    let v = Operation {
        id: new_id(),
        title: format!("Ordner umbenennen in {}", new_name.trim()),
        steps: vec![Step::new(old, new)],
        changes: vec![Change::Folder(root, rel, new_rel)],
        status: "planned".into(),
        message: String::new(),
        kind: String::new(),
        cleanup: None,
    };
    check(&v)?;
    Ok(v)
}

pub(crate) fn check(v: &Operation) -> Result<(), String> {
    let mut targets = std::collections::HashSet::new();
    for s in &v.steps {
        if !targets.insert(s.by_key.clone()) {
            return Err(format!("Zwei Dateien hätten denselben Zielnamen: {}", s.by_key.display()));
        }
        if std::fs::symlink_metadata(&s.by_key).is_ok() {
            return Err(format!("Am Ziel liegt schon {} – nichts wird überschrieben", s.by_key.display()));
        }
        if !s.from.exists() {
            return Err(format!("{} fehlt", s.from.display()));
        }
    }
    Ok(())
}

/// Root folder ID by path (for [`Change::Subtree`]).
fn root_id(tx: &rusqlite::Transaction, path: &str) -> Result<i64, String> {
    use rusqlite::OptionalExtension;
    tx.query_row("SELECT id FROM root_folder WHERE path=?1", [path], |r| r.get(0)).optional().map_err(f)?.ok_or_else(|| format!("Stammordner fehlt in der Bibliothek: {path}"))
}

fn create_root(tx: &rusqlite::Transaction, path: &str, name: &str, origin: &str) -> Result<(), String> {
    tx.execute("INSERT OR IGNORE INTO root_folder(path, name, origin) VALUES(?1, ?2, ?3)", params![path, name, origin]).map_err(f)?;
    Ok(())
}

/// Remove a root folder, but only if no folder belongs to it any more (nothing cascades).
fn drop_root(tx: &rusqlite::Transaction, path: &str) -> Result<(), String> {
    tx.execute("DELETE FROM root_folder WHERE path=?1 AND NOT EXISTS (SELECT 1 FROM folder o WHERE o.root = root_folder.id)", [path]).map_err(f)?;
    Ok(())
}

fn subtree(tx: &rusqlite::Transaction, from_root: &str, from: &str, to_root: &str, to: &str) -> Result<(), String> {
    let (a, b) = (root_id(tx, from_root)?, root_id(tx, to_root)?);
    tx.execute(
        "UPDATE folder SET root=?3, path = ?4 || substr(path, length(?2) + 1) WHERE root=?1 AND substr(path, 1, length(?2)) = ?2",
        params![a, from, b, to],
    )
    .map_err(f)?;
    Ok(())
}

/// Are these changes already in the library? `Some(true)`: every checkable change is applied (a run that ended
/// between the commit and saving the journal); `Some(false)`: at least one is not; `None`: nothing checkable.
pub(crate) fn db_applied(db: &Db, a: &[Change]) -> Option<bool> {
    let mut checked = false;
    for x in a {
        let applied = match x {
            Change::File(id, _, _, no, nn) => {
                let row: (i64, String) = db.conn.query_row("SELECT folder, name FROM file WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok()??;
                row == (*no, crate::nfc(nn))
            }
            Change::Folder(root, old, new) => {
                let has = |p: &str| db.conn.query_row("SELECT count(*) FROM folder WHERE root=?1 AND path=?2", params![root, p], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0;
                has(new) && !has(old)
            }
            Change::RootPath(id, _, new) => db.conn.query_row("SELECT path FROM root_folder WHERE id=?1", [id], |r| r.get::<_, String>(0)).ok()? == *new,
            _ => continue,
        };
        checked = true;
        if !applied {
            return Some(false);
        }
    }
    checked.then_some(true)
}

pub(crate) fn apply_db(db: &Db, a: &[Change], reverse: bool) -> Result<(), String> {
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    apply_tx(&tx, a, reverse)?;
    tx.commit().map_err(f)
}

/// Like [`apply_db`], inside a transaction of the caller (e.g. [`crate::relink`], which checks and switches in one).
pub(crate) fn apply_tx(tx: &rusqlite::Transaction, a: &[Change], reverse: bool) -> Result<(), String> {
    // backwards in reverse order (a root folder created first is removed last)
    let order: Vec<&Change> = if reverse { a.iter().rev().collect() } else { a.iter().collect() };
    for x in order {
        match x {
            Change::File(id, ao, old_name, no, nn) => {
                let (o, n) = if reverse { (ao, old_name) } else { (no, nn) };
                tx.execute("UPDATE file SET folder=?2, name=?3 WHERE id=?1", params![id, o, crate::nfc(n)]).map_err(f)?;
            }
            Change::Folder(root, old, new) => {
                let (from, by_key) = if reverse { (new, old) } else { (old, new) };
                tx.execute("UPDATE folder SET path = ?3 || substr(path, length(?2) + 1) WHERE root=?1 AND substr(path, 1, length(?2)) = ?2", params![root, from, by_key]).map_err(f)?;
            }
            Change::RootPath(id, old, new) => {
                let p = if reverse { old } else { new };
                tx.execute("UPDATE root_folder SET path=?2 WHERE id=?1", params![id, p]).map_err(f)?;
            }
            Change::Subtree(fr, fp, tr, tp) => {
                if reverse {
                    subtree(tx, tr, tp, fr, fp)?;
                } else {
                    subtree(tx, fr, fp, tr, tp)?;
                }
            }
            Change::CreateRoot(p, n) => {
                if reverse {
                    drop_root(tx, p)?;
                } else {
                    create_root(tx, p, n, "own")?;
                }
            }
            Change::DropRoot(p, n, o) => {
                if reverse {
                    create_root(tx, p, n, o)?;
                } else {
                    drop_root(tx, p)?;
                }
            }
        }
    }
    Ok(())
}

/// Execute (or continue) an operation. If a step fails, the steps already executed are
/// reverted (all or nothing); the library is changed only after the last step.
pub fn execute_step(db: &Db, v: &mut Operation) -> Result<(), String> {
    execute_with(db, v, None, &mut |_, _| {})
}

/// Like [`execute_step`], with cancelling (between files and before the switch of the library: everything is
/// reverted, message "abgebrochen") and progress (`progress(steps done, bytes done)`). A folder move between
/// locations stops after the switch with the status "aufraeumen"; [`crate::move_folder`] puts the source into the trash.
pub fn execute_with(db: &Db, v: &mut Operation, cancel: Option<&std::sync::atomic::AtomicBool>, progress: &mut dyn FnMut(usize, u64)) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    crate::backup_view::refuse()?;
    if v.status == "rolling_back" || v.status == "undoing" {
        return further_back(db, v);
    }
    if v.status == "cleanup" || v.status == "done" {
        return Ok(());
    }
    v.status = "running".into();
    v.save()?;
    let mut done_bytes: u64 = v.steps.iter().filter(|s| s.finished).filter_map(|s| s.size).sum();
    let mut done = v.steps.iter().filter(|s| s.finished).count();
    progress(done, done_bytes);
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let mut failure: Option<String> = None;
    for i in 0..v.steps.len() {
        if v.steps[i].finished {
            continue;
        }
        if cancelled() {
            failure = Some(CANCELLED.into());
            break;
        }
        let base = done_bytes;
        let r = step_with(&v.steps[i], &mut |b| progress(done, base + b));
        if let Err(e) = r {
            failure = Some(e);
            break;
        }
        v.steps[i].finished = true;
        done += 1;
        done_bytes += v.steps[i].size.unwrap_or(0);
        progress(done, done_bytes);
        v.save()?;
    }
    // copies whose source changed meanwhile (e.g. a rating written into the XMP during the copy): copy once more
    if failure.is_none() {
        failure = refresh_changed(v).err();
    }
    if failure.is_none() && cancelled() {
        failure = Some(CANCELLED.into());
    }
    if let Some(e) = failure {
        v.status = "rolling_back".into();
        v.message = e.clone();
        v.save()?;
        return match further_back(db, v) {
            Ok(()) if e == CANCELLED => Err(e),
            Ok(()) => Err(format!("{e} (bereits ausgeführte Schritte wurden zurückgenommen)")),
            Err(e2) => Err(format!("{e}; Zurücknehmen unterbrochen: {e2}")),
        };
    }
    // the library could not be switched (e.g. a row of the same name at the target appeared meanwhile): the files go
    // back, so that library and disk agree again (before 0.108 the files stayed moved and the photo counted as missing)
    if let Err(e) = apply_db(db, &v.changes, false) {
        v.status = "rolling_back".into();
        v.message = e.clone();
        v.save()?;
        return match further_back(db, v) {
            Ok(()) => Err(format!("Bibliothek: {e} (die Dateien wurden zurückgelegt)")),
            Err(e2) => Err(format!("Bibliothek: {e}; Zurücklegen unterbrochen: {e2}")),
        };
    }
    v.status = if v.cleanup.is_some() { "cleanup".into() } else { "done".into() };
    v.save()?;
    Ok(())
}

/// Copy steps whose source changed after its copy (size or modification time to the second): replace the own copy.
fn refresh_changed(v: &mut Operation) -> Result<(), String> {
    let secs = |m: &std::fs::Metadata| m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
    for i in 0..v.steps.len() {
        let s = &v.steps[i];
        if !s.keep || !s.finished {
            continue;
        }
        let (Ok(a), Ok(b)) = (std::fs::metadata(&s.from), std::fs::metadata(&s.by_key)) else { continue };
        if a.len() == b.len() && secs(&a) == secs(&b) {
            continue;
        }
        undo_copy(s)?;
        v.steps[i].finished = false;
        v.steps[i].size = Some(a.len());
        v.save()?;
        step(&v.steps[i])?;
        v.steps[i].finished = true;
        v.save()?;
    }
    Ok(())
}

/// Message of a cancelled operation (everything reverted).
pub const CANCELLED: &str = "abgebrochen";

/// Revert completed steps in reverse order; each step is recorded in the journal,
/// so that an interrupted revert can be continued as well.
fn roll_back(v: &mut Operation) -> Result<(), String> {
    for i in (0..v.steps.len()).rev() {
        if !v.steps[i].finished {
            // copy step interrupted between installing the copy and noting it in the journal: an identical copy at the
            // target is ours (the source stays) and goes again
            let s = &v.steps[i];
            if s.keep && std::fs::symlink_metadata(&s.by_key).is_ok_and(|m| m.is_file() && Some(m.len()) == s.size) && sha256(&s.from).ok().is_some_and(|a| sha256(&s.by_key).ok().as_deref() == Some(a.as_str())) {
                undo_copy(s)?;
            }
            continue;
        }
        let s = &v.steps[i];
        if s.keep {
            undo_copy(s)?;
        } else {
            step(&Step { from: s.by_key.clone(), by_key: s.from.clone(), finished: false, size: s.size, keep: false })?;
        }
        v.steps[i].finished = false;
        v.save()?;
    }
    Ok(())
}

fn further_back(db: &Db, v: &mut Operation) -> Result<(), String> {
    let undo = v.status == "undoing";
    if let Err(e) = roll_back(v) {
        v.message = e.clone();
        v.save()?;
        return Err(e);
    }
    if undo {
        apply_db(db, &v.changes, true)?;
        v.status = "undone".into();
    } else {
        v.status = "failed".into();
    }
    v.save()
}

/// Undo: execute all steps in reverse, reset the database. Not for copies between locations (the source is in the
/// trash then; "Move to …" back is the way).
pub fn undo(db: &Db, v: &mut Operation) -> Result<(), String> {
    crate::backup_view::refuse()?;
    if v.status != "done" {
        return Err("nur abgeschlossene Vorgänge lassen sich rückgängig machen".into());
    }
    if v.steps.iter().any(|s| s.keep) {
        return Err(format!("„{}“ lässt sich nicht rückgängig machen – bitte mit „Verschieben nach …“ zurückholen", v.title));
    }
    v.status = "undoing".into();
    v.save()?;
    further_back(db, v)
}

/// All journals (newest first).
pub fn journals() -> Vec<Operation> {
    let mut v: Vec<Operation> = std::fs::read_dir(journal_folder())
        .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).filter_map(|e| Operation::from_json(&serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?)).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

/// Continue interrupted operations (at startup). Returns: messages.
pub fn resume(db: &Db) -> Vec<String> {
    let mut m = Vec::new();
    // folder moves between locations need the trash of the app: [`crate::move_folder::resume`]
    for mut v in journals().into_iter().filter(|v| v.kind.is_empty() && matches!(v.status.as_str(), "running" | "planned" | "rolling_back" | "undoing")) {
        match execute_step(db, &mut v) {
            Ok(()) => m.push(format!("fortgesetzt: {}", v.title)),
            Err(e) => m.push(format!("{}: {e}", v.title)),
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The journal lives in a folder determined by an environment variable: the tests run one after another.
    static LOCK: Mutex<()> = Mutex::new(());

    struct Env {
        dir: PathBuf,
        db: Db,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    fn env() -> Env {
        let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("graineo-versch-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(dir.join("fotos")).unwrap();
        std::env::set_var("GRAINEO_LIBRARY_JOURNAL", dir.join("journal"));
        let db = Db::open(&dir.join("lib.sqlite")).unwrap();
        Env { dir, db, _g: g }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            std::env::remove_var("GRAINEO_LIBRARY_JOURNAL");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn operation(steps: Vec<Step>) -> Operation {
        Operation { id: new_id(), title: "Test".into(), steps, changes: Vec::new(), status: "planned".into(), message: String::new(), kind: String::new(), cleanup: None }
    }

    #[test]
    fn if_a_step_fails_everything_is_undone() {
        let u = env();
        let f = u.dir.join("fotos");
        std::fs::write(f.join("a.jpg"), b"A").unwrap();
        std::fs::write(f.join("b.jpg"), b"B").unwrap();
        std::fs::create_dir_all(f.join("neu")).unwrap();
        // The target of the second step only appears after planning (race)
        let mut v = operation(vec![Step::new(f.join("a.jpg"), f.join("neu/a.jpg")), Step::new(f.join("b.jpg"), f.join("neu/b.jpg"))]);
        std::fs::write(f.join("neu/b.jpg"), b"FREMD").unwrap();
        let e = execute_step(&u.db, &mut v).unwrap_err();
        assert!(e.contains("Ziel existiert schon"), "{e}");
        assert_eq!(std::fs::read(f.join("a.jpg")).unwrap(), b"A", "first step reverted");
        assert_eq!(std::fs::read(f.join("b.jpg")).unwrap(), b"B");
        assert_eq!(std::fs::read(f.join("neu/b.jpg")).unwrap(), b"FREMD", "foreign file untouched");
        assert!(!f.join("neu/a.jpg").exists());
        assert_eq!(v.status, "failed");
        let j = journals();
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].status, "failed");
        assert!(j[0].steps.iter().all(|s| !s.finished));
    }

    /// A library row of the same name at the target: planning refuses; should one appear after planning, the files
    /// go back when the library cannot be switched (before 0.108 they stayed moved and the photo counted as missing).
    #[test]
    fn library_row_at_target_moves_nothing() {
        let u = env();
        let f = u.dir.join("fotos");
        std::fs::create_dir_all(f.join("neu")).unwrap();
        std::fs::write(f.join("a.jpg"), b"A").unwrap();
        let root = u.db.create_root(&format!("{}/", crate::nfc(&f.to_string_lossy())), "fotos", "own", None).unwrap();
        let folder = u.db.create_folder(root, "", None).unwrap();
        let other = u.db.create_folder(root, "neu/", None).unwrap();
        u.db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'a.jpg', 'JPG')", [folder]).unwrap();
        let id = u.db.conn.last_insert_rowid();
        u.db.conn.execute("INSERT INTO file(folder, name, format, status) VALUES(?1, 'a.jpg', 'JPG', 1)", [other]).unwrap();
        let ghost = u.db.conn.last_insert_rowid();
        let e = plan_files(&u.db, &[id], &f.join("neu"), None).unwrap_err();
        assert!(e.contains("schon einen Eintrag"), "{e}");
        // the row appears only after planning: files go back
        u.db.conn.execute("DELETE FROM file WHERE id = ?1", [ghost]).unwrap();
        let mut v = plan_files(&u.db, &[id], &f.join("neu"), None).unwrap();
        u.db.conn.execute("INSERT INTO file(folder, name, format, status) VALUES(?1, 'a.jpg', 'JPG', 1)", [other]).unwrap();
        let e = execute_step(&u.db, &mut v).unwrap_err();
        assert!(e.contains("zurückgelegt"), "{e}");
        assert_eq!(std::fs::read(f.join("a.jpg")).unwrap(), b"A");
        assert!(!f.join("neu/a.jpg").exists());
        assert_eq!(u.db.file_path(id), Some(f.join("a.jpg")));
    }

    #[test]
    fn db_applied_tells_whether_the_library_was_switched() {
        let u = env();
        let root = u.db.create_root("/tmp/graineo-db-applied/", "t", "own", None).unwrap();
        let folder = u.db.create_folder(root, "", None).unwrap();
        let other = u.db.create_folder(root, "neu/", None).unwrap();
        u.db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'a.jpg', 'JPG')", [folder]).unwrap();
        let id: i64 = u.db.conn.query_row("SELECT id FROM file WHERE name='a.jpg'", [], |r| r.get(0)).unwrap();
        let changes = vec![Change::File(id, folder, "a.jpg".into(), other, "a.jpg".into())];
        assert_eq!(db_applied(&u.db, &changes), Some(false));
        apply_db(&u.db, &changes, false).unwrap();
        assert_eq!(db_applied(&u.db, &changes), Some(true));
        assert_eq!(db_applied(&u.db, &[Change::CreateRoot("/x/".into(), "x".into())]), None, "nothing checkable");
    }

    #[test]
    fn interrupted_operation_is_resumed() {
        let u = env();
        let f = u.dir.join("fotos");
        std::fs::write(f.join("a.jpg"), b"A").unwrap();
        std::fs::write(f.join("b.jpg"), b"B").unwrap();
        let mut v = operation(vec![Step::new(f.join("a.jpg"), f.join("x/a.jpg")), Step::new(f.join("b.jpg"), f.join("x/b.jpg"))]);
        // first step ran, then power failure: the journal shows "running", step 1 done
        step(&v.steps[0]).unwrap();
        v.steps[0].finished = true;
        v.status = "running".into();
        v.save().unwrap();
        let m = resume(&u.db);
        assert_eq!(m.len(), 1, "{m:?}");
        assert!(f.join("x/a.jpg").exists() && f.join("x/b.jpg").exists());
        assert_eq!(journals()[0].status, "done");
    }

    #[test]
    fn interrupted_undo_is_resumed() {
        let u = env();
        let f = u.dir.join("fotos");
        std::fs::write(f.join("a.jpg"), b"A").unwrap();
        std::fs::write(f.join("b.jpg"), b"B").unwrap();
        let mut v = operation(vec![Step::new(f.join("a.jpg"), f.join("x/a.jpg")), Step::new(f.join("b.jpg"), f.join("x/b.jpg"))]);
        execute_step(&u.db, &mut v).unwrap();
        // ⌘Z, but after the first backward step everything aborts
        v.status = "undoing".into();
        step(&Step { from: f.join("x/b.jpg"), by_key: f.join("b.jpg"), finished: false, size: None, keep: false }).unwrap();
        v.steps[1].finished = false;
        v.save().unwrap();
        let m = resume(&u.db);
        assert_eq!(m.len(), 1, "{m:?}");
        assert!(f.join("a.jpg").exists() && f.join("b.jpg").exists());
        assert!(!f.join("x/a.jpg").exists() && !f.join("x/b.jpg").exists());
        assert_eq!(journals()[0].status, "undone");
    }

    #[test]
    fn foreign_file_at_target_does_not_count_as_done_on_resume() {
        let u = env();
        let f = u.dir.join("fotos");
        std::fs::write(f.join("a.jpg"), b"lang genug").unwrap();
        let s = Step::new(f.join("a.jpg"), f.join("b.jpg"));
        std::fs::remove_file(f.join("a.jpg")).unwrap();
        std::fs::write(f.join("b.jpg"), b"x").unwrap();
        assert!(step(&s).unwrap_err().contains("andere Datei"));
        std::fs::write(f.join("b.jpg"), b"lang genug").unwrap();
        step(&s).unwrap();
        drop(u);
    }

    #[test]
    fn copy_path_overwrites_nothing_and_follows_no_link() {
        let u = env();
        let f = u.dir.join("fotos");
        std::env::set_var("GRAINEO_MOVE_COPY", "1");
        std::fs::write(f.join("a.jpg"), b"A").unwrap();
        let foreign = f.join("fremd.txt");
        std::fs::write(&foreign, b"fremd").unwrap();
        std::fs::create_dir_all(f.join("x")).unwrap();
        // predictable former names point to a foreign file
        std::os::unix::fs::symlink(&foreign, f.join("x/.a.jpg.copying")).unwrap();
        std::os::unix::fs::symlink(&foreign, f.join("x/.a.jpg.teil")).unwrap();
        step(&Step::new(f.join("a.jpg"), f.join("x/a.jpg"))).unwrap();
        assert_eq!(std::fs::read(&foreign).unwrap(), b"fremd");
        assert_eq!(std::fs::read(f.join("x/a.jpg")).unwrap(), b"A");
        // occupied target
        std::fs::write(f.join("b.jpg"), b"B").unwrap();
        std::fs::write(f.join("x/b.jpg"), b"andere").unwrap();
        assert!(step(&Step::new(f.join("b.jpg"), f.join("x/b.jpg"))).is_err());
        assert_eq!(std::fs::read(f.join("x/b.jpg")).unwrap(), b"andere");
        std::env::remove_var("GRAINEO_MOVE_COPY");
    }

    #[test]
    fn journal_with_foreign_entries_is_rejected() {
        let _u = env();
        let good = json!({"id": "123", "steps": [], "db": []});
        assert!(Operation::from_json(&good).is_some());
        for id in ["../x", "1/2", "", "abc"] {
            assert!(Operation::from_json(&json!({"id": id, "steps": [], "db": []})).is_none(), "{id}");
        }
        assert!(path_ok(Path::new("relativ/a.jpg")).is_err());
        assert!(path_ok(Path::new("/a/../etc/x")).is_err());
        assert!(path_ok(Path::new("/a/b.jpg")).is_ok());
        let s = Step { from: "/tmp/a".into(), by_key: "/tmp/../etc/b".into(), finished: false, size: None, keep: false };
        assert!(step(&s).is_err());
    }

    #[test]
    fn names_are_exactly_one_component() {
        for ok in ["IMG_1", "  Strand  ", "a.b"] {
            assert!(single_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "  ", ".", "..", "a/b", "../a", "a\0b"] {
            assert!(single_name(bad).is_err(), "{bad:?}");
        }
        assert!(single_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn two_sources_with_same_target_name_are_rejected_before_start() {
        let u = env();
        let f = u.dir.join("fotos");
        std::fs::create_dir_all(f.join("a")).unwrap();
        std::fs::create_dir_all(f.join("b")).unwrap();
        std::fs::write(f.join("a/x.jpg"), b"1").unwrap();
        std::fs::write(f.join("b/x.jpg"), b"2").unwrap();
        let v = operation(vec![Step::new(f.join("a/x.jpg"), f.join("z/x.jpg")), Step::new(f.join("b/x.jpg"), f.join("z/x.jpg"))]);
        assert!(check(&v).unwrap_err().contains("denselben Zielnamen"));
        assert!(!f.join("z").exists());
    }
}
