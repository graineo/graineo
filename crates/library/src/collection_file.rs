//! Own smart collections as a readable file in the root folder: an optional, visible copy for other
//! programs and people. The library (`library.sqlite`) is the main store; the file is **never read**
//! and only written while the option "write sidecars"
//! (setting `writeSidecars`, [`crate::store::write_sidecars`]) is on.
//!
//! Location: `<root folder>/.graineo/collections.json` (English keys: `smartCollections`, `name`, `rule`)
//! (same subfolder as the Sigma sidecars, never next to an original), written into the root folder with
//! the most images. Lightroom collections stay in the Lightroom catalog and are imported from there.

use std::path::PathBuf;

use serde_json::json;

use crate::db::Db;

pub const FILE: &str = ".graineo/collections.json";

/// Root folder for the file: reachable, with the most images – never the Photos library.
fn main_root(db: &Db) -> Option<PathBuf> {
    let mut st = db
        .conn
        .prepare("SELECT s.path, count(*) n FROM root_folder s JOIN folder o ON o.root=s.id JOIN file d ON d.folder=o.id WHERE s.origin <> 'apple-photos' GROUP BY s.id ORDER BY n DESC")
        .ok()?;
    let v: Vec<String> = st.query_map([], |r| r.get(0)).ok()?.flatten().collect();
    v.into_iter().map(PathBuf::from).find(|p| p.is_dir())
}

/// Write the file if the option "write sidecars" is on (setting `writeSidecars`); otherwise nothing.
/// Returns: path written, `None` if the option is off.
pub fn write_if_enabled(db: &Db) -> Result<Option<PathBuf>, String> {
    write_if(db, crate::store::write_sidecars())
}

/// Write the file only if `enabled` (see [`write_if_enabled`]).
pub fn write_if(db: &Db, enabled: bool) -> Result<Option<PathBuf>, String> {
    if enabled { write(db).map(Some) } else { Ok(None) }
}

/// Write all own smart collections to the file (atomically), regardless of the option. Returns: path.
pub fn write(db: &Db) -> Result<PathBuf, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut st = db.conn.prepare("SELECT name, search FROM collection WHERE kind=2 AND lr_id IS NULL AND search IS NOT NULL ORDER BY name").map_err(f)?;
    let smart: Vec<serde_json::Value> = st.query_map([], |r| Ok(json!({"name": r.get::<_, String>(0)?, "rule": r.get::<_, String>(1)?}))).map_err(f)?.flatten().collect();
    let root = main_root(db).ok_or("kein erreichbarer Stammordner")?;
    let target = root.join(FILE);
    if crate::apple_photos::in_photos_library(&target) {
        return Err("nie in die Mediathek von Apple Fotos schreiben".into());
    }
    let content = json!({
        "app": crate::APP,
        "version": 1,
        "note": "Smart collections; rule = search syntax of the app.",
        "smartCollections": smart,
    });
    std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&content).unwrap_or_default()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
    Ok(target)
}
