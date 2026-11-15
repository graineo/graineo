//! Snapshots of earlier versions: `own/history/<id>/<version>.json`, written before a profile file is
//! replaced. The history is never rewritten; going back creates a new version with the old content.
//! Older app versions read only the top level of `own/` and do not see these files.

use std::path::PathBuf;

use crate::own::OwnProfile;

pub fn dir(id: &str) -> Option<PathBuf> {
    crate::store::valid_id(id).then_some(())?;
    Some(crate::store::own_dir()?.join("history").join(id))
}

pub fn path(id: &str, version: u32) -> Option<PathBuf> {
    Some(dir(id)?.join(format!("{version}.json")))
}

/// Keep `p` (already normalised) as snapshot of its version. An existing snapshot is left alone.
pub fn write(p: &OwnProfile) -> std::io::Result<PathBuf> {
    let path = path(&p.id, p.version).ok_or_else(|| std::io::Error::other("kein gültiger Profilname"))?;
    if path.exists() {
        return Ok(path);
    }
    crate::store::write_atomic(&path, p)?;
    Ok(path)
}

pub fn read(id: &str, version: u32) -> Option<OwnProfile> {
    let t = std::fs::read_to_string(path(id, version)?).ok()?;
    let mut p = OwnProfile::from_json(&t, id).ok()?;
    p.version = version;
    Some(p)
}

/// Versions with a snapshot, ascending.
pub fn versions(id: &str) -> Vec<u32> {
    let mut v: Vec<u32> = dir(id)
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|rd| rd.flatten().filter_map(|e| e.path().file_stem()?.to_str()?.parse().ok()).collect())
        .unwrap_or_default();
    v.sort_unstable();
    v
}
