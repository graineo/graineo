//! Managing own profiles: duplicate, rename, export and import (`.graineo-lens`).
//!
//! Only own profiles (graineo measurements) go through here. Adobe, lensfun and camera data are never
//! copied or exported.

use std::path::Path;

use crate::own::{OwnProfile, ProfileRef, Revision, RevisionAction, FORMAT};
use crate::store;

/// Biggest colour-cast grid and number of entries an imported file may have.
const MAX_CELLS: usize = 4096;
const MAX_ENTRIES: usize = 2000;
const MAX_FILE: u64 = 32 * 1024 * 1024;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn current(id: &str) -> std::io::Result<OwnProfile> {
    let path = store::path_of(id).ok_or_else(|| std::io::Error::other("kein gültiger Profilname"))?;
    let text = std::fs::read_to_string(&path)?;
    OwnProfile::from_json(&text, id).map_err(std::io::Error::other)
}

/// A copy with a new id (version 1, history reset to "duplicated").
pub fn duplicate(id: &str, name: &str) -> std::io::Result<ProfileRef> {
    let mut p = current(id)?;
    let from = p.reference().to_string();
    p.name = name.trim().to_string();
    p.history.clear();
    store::save_new_as(p, RevisionAction::Duplicated { from })
}

/// Change only the display name (revision "renamed"; no new version).
pub fn rename(id: &str, name: &str) -> std::io::Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(std::io::Error::other("Name fehlt"));
    }
    let mut p = current(id)?;
    p.format = FORMAT;
    p.name = name.to_string();
    p.history.push(Revision { version: p.version, time: now(), action: RevisionAction::Renamed, shots: Vec::new(), note: String::new() });
    store::write_atomic(&store::path_of(id).ok_or_else(|| std::io::Error::other("kein gültiger Profilname"))?, &p)?;
    store::bump_generation();
    Ok(())
}

/// Write the current version with its revision list (without the snapshots of older versions).
pub fn export(id: &str, to: &Path) -> std::io::Result<()> {
    let p = current(id)?;
    store::write_atomic(to, &p)
}

/// Read an exported profile as a new profile (new id, revision "imported"). Error: reason for the user.
pub fn import(from: &Path) -> Result<ProfileRef, String> {
    let meta = std::fs::metadata(from).map_err(|e| e.to_string())?;
    if meta.len() > MAX_FILE {
        return Err("Die Datei ist zu groß für ein Objektivprofil".into());
    }
    let text = std::fs::read_to_string(from).map_err(|e| e.to_string())?;
    let mut p = OwnProfile::from_json(&text, "").map_err(|e| e.to_string())?;
    if p.profile.entries.len() > MAX_ENTRIES {
        return Err("Das Profil hat zu viele Einträge".into());
    }
    for c in &p.cast {
        let (cols, rows) = (c.grid.cols as usize, c.grid.rows as usize);
        if cols * rows == 0 || cols * rows > MAX_CELLS || c.grid.gain.len() != cols * rows {
            return Err("Die Farbstichkarte im Profil ist ungültig".into());
        }
    }
    if p.name.trim().is_empty() {
        p.name = from.file_stem().and_then(|s| s.to_str()).unwrap_or("Objektiv").to_string();
    }
    p.history.clear();
    p.id.clear();
    store::save_new_as(p, RevisionAction::Imported).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::own::tests::v1;
    use crate::store::tests::{lock, scratch};

    #[test]
    fn export_import_roundtrip_new_id() {
        let _l = lock();
        let root = scratch("exchange");
        let r1 = store::save_new(OwnProfile::from_json(&v1(), "").unwrap()).unwrap();
        let file = root.join("x.graineo-lens");
        export(&r1.id, &file).unwrap();
        let r2 = import(&file).unwrap();
        assert_ne!(r1.id, r2.id);
        let p = store::load(&r2).unwrap();
        assert_eq!(p.history.len(), 1);
        assert_eq!(p.history[0].action, RevisionAction::Imported);
        assert_eq!(p.profile.entries.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn import_rejects_format_3_and_huge_grids() {
        let _l = lock();
        let root = scratch("exchange-bad");
        let f3 = root.join("f3.json");
        std::fs::write(&f3, v1().replacen("\"format\": 1", "\"format\": 3", 1)).unwrap();
        assert!(import(&f3).is_err());
        let mut p = OwnProfile::from_json(&v1(), "a").unwrap();
        p.cast.push(crate::own::CastEntry { focal_length: 10.0, av: 4.0, grid: crate::cast::CastGrid { cols: 100, rows: 100, gain: vec![[1.0; 3]; 10000] }, quality: Default::default() });
        let big = root.join("big.json");
        std::fs::write(&big, serde_json::to_string(&p).unwrap()).unwrap();
        assert!(import(&big).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rename_and_duplicate() {
        let _l = lock();
        let root = scratch("exchange-rename");
        let r1 = store::save_new(OwnProfile::from_json(&v1(), "").unwrap()).unwrap();
        rename(&r1.id, "Helios 44-2").unwrap();
        let p = store::load(&r1).unwrap();
        assert_eq!((p.name.as_str(), p.version, p.history.last().unwrap().action.clone()), ("Helios 44-2", 1, RevisionAction::Renamed));
        let r2 = duplicate(&r1.id, "Kopie").unwrap();
        assert_ne!(r1.id, r2.id);
        assert!(matches!(store::load(&r2).unwrap().history[0].action, RevisionAction::Duplicated { .. }));
        assert!(rename(&r1.id, "  ").is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
