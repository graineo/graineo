//! Rasters of bitmap masks (table `mask_raster`): immutable 8-bit weight maps,
//! named by their content hash and referenced from the mask documents in the develop settings (`"raster":
//! "sha256:…"`). They live in the library – the main store – and so come into the daily backup; nothing is written
//! next to the photos. Sidecars only carry the name and the rule.
//!
//! Writes are single short transactions from a background job (never the UI thread); reads use the per-thread read
//! connection of [`crate::store`]. [`sweep`] removes a raster only when it is certain that nothing needs it any
//! more: no develop setting, history step (originals and virtual copies) or own preset names it, it was not stored
//! for a photo outside the library (whose settings live in the sidecar next to it, which the library cannot see), and
//! it was not stored or used for [`KEEP_UNUSED_SECS`]. In doubt it stays. Time alone never removes a raster.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::db::Db;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Grace time for unreferenced rasters (⌘Z within a session, edits not saved yet, exports running). Only a raster
/// that nothing names is removed after it; a referenced one is never removed.
pub const KEEP_UNUSED_SECS: i64 = 30 * 24 * 3600;
const FORMAT: &str = "u8-zstd";

/// A raster as stored: width, height, 8-bit weights (row-major).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Content name (`sha256:<hex>` over width, height as little-endian u32 and the weights): the same as
/// `masks::RasterData::id`, checked again on reading.
pub fn id_of(width: u32, height: u32, data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(width.to_le_bytes());
    h.update(height.to_le_bytes());
    h.update(data);
    let d = h.finalize();
    let mut s = String::from("sha256:");
    for b in d.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Key in `basis` that exempts a raster from the clean-up for good (stored for a photo outside the library).
const KEEP: &str = "keep";
/// Key in `basis` that marks a raster stored for a photo the library knew at that time. Only such rasters may ever be
/// removed by [`sweep`]; older rows without either mark stay (their photo can not be told apart from a folder photo
/// of the same name).
const LIBRARY: &str = "library";

/// Largest width or height of a stored raster; checked before anything is allocated for it.
pub const MAX_SIDE: u32 = 16384;

/// Width and height within `1..=MAX_SIDE`: pixel count, else `None` (damaged or foreign row).
fn checked_size(w: i64, h: i64) -> Option<usize> {
    let ok = |v: i64| (1..=MAX_SIDE as i64).contains(&v);
    (ok(w) && ok(h)).then(|| w as usize * h as usize)
}

/// `basis` for a raster of `photo`: marked [`KEEP`] when the photo is not in the library (connection `conn`). Its
/// settings then live only in the sidecar next to the photo, which the clean-up cannot see.
fn basis_for(conn: &Connection, basis: &Value, photo: &std::path::Path) -> Value {
    let mut b = if basis.is_object() { basis.clone() } else { serde_json::json!({}) };
    if crate::store::locate_in(conn, photo).is_none() {
        b[KEEP] = Value::Bool(true);
    } else {
        b[LIBRARY] = Value::Bool(true);
    }
    b
}

/// On a second store of the same content: refresh `last_used`; a [`KEEP`] mark is added, never taken away (it wins
/// over [`LIBRARY`]).
const ON_CONFLICT: &str = "ON CONFLICT(id) DO UPDATE SET last_used = excluded.last_used,
    basis = CASE WHEN json_extract(excluded.basis, '$.keep') = 1 THEN excluded.basis ELSE mask_raster.basis END";

/// Store a raster (nothing happens when it is there already). `basis`: what it was computed from. Returns its name.
pub fn put_in(conn: &Connection, r: &Raster, basis: &Value) -> R<String> {
    if r.width == 0 || r.height == 0 || r.data.len() != r.width as usize * r.height as usize {
        return Err("Rastergröße passt nicht".into());
    }
    let id = id_of(r.width, r.height, &r.data);
    let packed = zstd::bulk::compress(&r.data, 9).map_err(|e| e.to_string())?;
    let now = crate::now();
    conn.execute(
        &format!("INSERT INTO mask_raster (id, width, height, format, data, basis, created, last_used) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) {ON_CONFLICT}"),
        params![id, r.width, r.height, FORMAT, packed, basis.to_string(), now],
    )
    .map_err(f)?;
    Ok(id)
}

/// Store a raster of `photo` in the standard library (own short connection; background thread only). A photo
/// outside the library marks it [`KEEP`].
pub fn put(r: &Raster, basis: &Value, photo: &std::path::Path) -> R<String> {
    let p = crate::db_path();
    if !p.exists() {
        return Err("Keine Bibliothek: Bitmap-Masken werden in der Bibliothek gespeichert".into());
    }
    let conn = Connection::open(&p).map_err(f)?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    // older libraries got the table on their last opening; make sure it is there
    conn.execute_batch(TABLE).map_err(f)?;
    put_in(&conn, r, &basis_for(&conn, basis, photo))
}

const TABLE: &str = "CREATE TABLE IF NOT EXISTS mask_raster (id TEXT PRIMARY KEY, width INTEGER NOT NULL, height INTEGER NOT NULL, format TEXT NOT NULL, data BLOB NOT NULL, basis TEXT, created INTEGER NOT NULL, last_used INTEGER NOT NULL)";

/// Read a raster by name from `conn`; `None`: unknown, damaged or of another format.
pub fn get_in(conn: &Connection, id: &str) -> Option<Raster> {
    let (w, h, format, data): (i64, i64, String, Vec<u8>) = conn
        .prepare_cached("SELECT width, height, format, data FROM mask_raster WHERE id = ?1")
        .ok()?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()
        .ok()??;
    if format != FORMAT {
        return None;
    }
    let n = checked_size(w, h)?;
    let (w, h) = (w as u32, h as u32);
    let raw = zstd::bulk::decompress(&data, n).ok()?;
    (raw.len() == n && id_of(w, h, &raw) == id).then_some(Raster { width: w, height: h, data: raw })
}

/// Read a raster from the standard library (per-thread read connection).
pub fn get(id: &str) -> Option<Raster> {
    crate::store::with_conn(|c| get_in(c, id))
}

/// Mark rasters as used now (opening a photo that shows them), so that the clean-up keeps them.
pub fn touch_in(conn: &Connection, ids: &[String]) -> R<()> {
    let now = crate::now();
    for id in ids {
        conn.execute("UPDATE mask_raster SET last_used = ?2 WHERE id = ?1", params![id, now]).map_err(f)?;
    }
    Ok(())
}

/// Content name of a 16-bit raster (depth): SHA-256 over "u16", width, height (little-endian u32) and the values
/// (little-endian u16); the same as `masks::DepthData::id`.
pub fn depth_id_of(width: u32, height: u32, values: &[u16]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"u16");
    h.update(width.to_le_bytes());
    h.update(height.to_le_bytes());
    for v in values {
        h.update(v.to_le_bytes());
    }
    let mut s = String::from("sha256:");
    for b in h.finalize().iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Store a 16-bit raster (depth ranks; same table, format `u16-zstd`, values little endian). Returns its name.
pub fn put_depth_in(conn: &Connection, width: u32, height: u32, values: &[u16], basis: &Value) -> R<String> {
    if width == 0 || height == 0 || values.len() != width as usize * height as usize {
        return Err("Rastergröße passt nicht".into());
    }
    let id = depth_id_of(width, height, values);
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let packed = zstd::bulk::compress(&bytes, 9).map_err(|e| e.to_string())?;
    conn.execute(
        &format!("INSERT INTO mask_raster (id, width, height, format, data, basis, created, last_used) VALUES (?1, ?2, ?3, 'u16-zstd', ?4, ?5, ?6, ?6) {ON_CONFLICT}"),
        params![id, width, height, packed, basis.to_string(), crate::now()],
    )
    .map_err(f)?;
    Ok(id)
}

/// [`put_depth_in`] into the standard library for `photo` (own short connection; background thread only).
pub fn put_depth(width: u32, height: u32, values: &[u16], basis: &Value, photo: &std::path::Path) -> R<String> {
    let p = crate::db_path();
    if !p.exists() {
        return Err("Keine Bibliothek: Bitmap-Masken werden in der Bibliothek gespeichert".into());
    }
    let conn = Connection::open(&p).map_err(f)?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    conn.execute_batch(TABLE).map_err(f)?;
    put_depth_in(&conn, width, height, values, &basis_for(&conn, basis, photo))
}

/// Read a 16-bit raster by name: (width, height, values).
pub fn get_depth_in(conn: &Connection, id: &str) -> Option<(u32, u32, Vec<u16>)> {
    let (w, h, format, data): (i64, i64, String, Vec<u8>) = conn
        .prepare_cached("SELECT width, height, format, data FROM mask_raster WHERE id = ?1")
        .ok()?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()
        .ok()??;
    if format != "u16-zstd" {
        return None;
    }
    let n = checked_size(w, h)?;
    let (w, h) = (w as u32, h as u32);
    let raw = zstd::bulk::decompress(&data, n * 2).ok()?;
    if raw.len() != n * 2 {
        return None;
    }
    let values: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    (depth_id_of(w, h, &values) == id).then_some((w, h, values))
}

/// [`get_depth_in`] from the standard library (per-thread read connection).
pub fn get_depth(id: &str) -> Option<(u32, u32, Vec<u16>)> {
    crate::store::with_conn(|c| get_depth_in(c, id))
}

/// Content name of an RGB patch (removed objects): SHA-256 over "rgb8", width, height and the bytes.
pub fn rgb_id_of(width: u32, height: u32, rgb: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"rgb8");
    h.update(width.to_le_bytes());
    h.update(height.to_le_bytes());
    h.update(rgb);
    let mut s = String::from("sha256:");
    for b in h.finalize().iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Store an 8-bit sRGB patch of a removed object (same table, format `rgb8-zstd`). Returns its name.
pub fn put_rgb_in(conn: &Connection, width: u32, height: u32, rgb: &[u8], basis: &Value) -> R<String> {
    if width == 0 || height == 0 || rgb.len() != width as usize * height as usize * 3 {
        return Err("Patchgröße passt nicht".into());
    }
    let id = rgb_id_of(width, height, rgb);
    let packed = zstd::bulk::compress(rgb, 9).map_err(|e| e.to_string())?;
    conn.execute(
        &format!("INSERT INTO mask_raster (id, width, height, format, data, basis, created, last_used) VALUES (?1, ?2, ?3, 'rgb8-zstd', ?4, ?5, ?6, ?6) {ON_CONFLICT}"),
        params![id, width, height, packed, basis.to_string(), crate::now()],
    )
    .map_err(f)?;
    Ok(id)
}

/// [`put_rgb_in`] into the standard library for `photo` (own short connection; background thread only).
pub fn put_rgb(width: u32, height: u32, rgb: &[u8], basis: &Value, photo: &std::path::Path) -> R<String> {
    let p = crate::db_path();
    if !p.exists() {
        return Err("Keine Bibliothek".into());
    }
    let conn = Connection::open(&p).map_err(f)?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    conn.execute_batch(TABLE).map_err(f)?;
    put_rgb_in(&conn, width, height, rgb, &basis_for(&conn, basis, photo))
}

/// Read an RGB patch by name: (width, height, bytes).
pub fn get_rgb_in(conn: &Connection, id: &str) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, format, data): (i64, i64, String, Vec<u8>) = conn
        .prepare_cached("SELECT width, height, format, data FROM mask_raster WHERE id = ?1")
        .ok()?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()
        .ok()??;
    if format != "rgb8-zstd" {
        return None;
    }
    let n = checked_size(w, h)? * 3;
    let (w, h) = (w as u32, h as u32);
    let raw = zstd::bulk::decompress(&data, n).ok()?;
    (raw.len() == n && rgb_id_of(w, h, &raw) == id).then_some((w, h, raw))
}

/// [`get_rgb_in`] from the standard library (per-thread read connection).
pub fn get_rgb(id: &str) -> Option<(u32, u32, Vec<u8>)> {
    crate::store::with_conn(|c| get_rgb_in(c, id))
}

/// Number and bytes (packed) of the stored rasters, for the settings.
pub fn usage(conn: &Connection) -> (i64, i64) {
    conn.query_row("SELECT count(*), coalesce(sum(length(data)), 0) FROM mask_raster", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or((0, 0))
}

/// Raster and patch names (`sha256:` + 64 hex digits) in a text (develop settings, history patch, presets).
pub fn ids_in(text: &str, out: &mut HashSet<String>) {
    const P: &str = "sha256:";
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(k) = text[from..].find(P) {
        let start = from + k;
        let end = start + P.len() + 64;
        if end <= b.len() && b[start + P.len()..end].iter().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)) {
            out.insert(text[start..end].to_string());
            from = end;
        } else {
            from = start + P.len();
        }
    }
}

/// Copy the rasters named in `ids` from the library `src` into `dst` (backup take-over, rebuild). Existing ones get
/// a fresh `last_used`. A source without the table (older library) copies nothing. Returns: copied (new) rasters.
pub fn copy_from(src: &Connection, dst: &Connection, ids: &HashSet<String>) -> R<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let has: bool = src.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'mask_raster')", [], |r| r.get(0)).map_err(f)?;
    if !has {
        return Ok(0);
    }
    dst.execute_batch(TABLE).map_err(f)?;
    let now = crate::now();
    let mut n = 0;
    let mut get = src.prepare("SELECT width, height, format, data, basis, created FROM mask_raster WHERE id = ?1").map_err(f)?;
    for id in ids {
        let row: Option<(i64, i64, String, Vec<u8>, Option<String>, i64)> = get.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).optional().map_err(f)?;
        let Some((w, h, format, data, basis, created)) = row else { continue };
        // a damaged row stays behind (it could not be read anyway)
        if checked_size(w, h).is_none() {
            continue;
        }
        if dst.execute("UPDATE mask_raster SET last_used = ?2 WHERE id = ?1", params![id, now]).map_err(f)? > 0 {
            continue;
        }
        n += dst
            .execute(
                "INSERT INTO mask_raster (id, width, height, format, data, basis, created, last_used) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, w, h, format, data, basis, created, now],
            )
            .map_err(f)?;
    }
    Ok(n)
}

/// All raster names the library refers to: develop settings and history steps of every photo and virtual copy.
/// One pass over each table (`since`: only rows changed at or after this time).
fn referenced_in(conn: &Connection, since: Option<i64>, out: &mut HashSet<String>) -> R<()> {
    let t = since.unwrap_or(i64::MIN);
    for sql in [
        "SELECT settings FROM develop_state WHERE instr(settings, 'sha256:') > 0 AND coalesce(settings_time, ?1) >= ?1",
        "SELECT patch FROM develop_step WHERE instr(patch, 'sha256:') > 0 AND time >= ?1",
    ] {
        let mut st = conn.prepare(sql).map_err(f)?;
        let mut rows = st.query([t]).map_err(f)?;
        while let Some(r) = rows.next().map_err(f)? {
            if let Some(text) = r.get_ref(0).map_err(f)?.as_str_or_null().map_err(|e| e.to_string())? {
                ids_in(text, out);
            }
        }
    }
    Ok(())
}

/// Remove rasters nothing needs any more, at most `max` per run. A raster stays when
/// - a develop setting or history step of any photo or virtual copy names it,
/// - one of the files `extra` names it (own presets; a file that exists but cannot be read stops the whole run),
/// - it was stored for a photo outside the library ([`KEEP`]), or it carries no [`LIBRARY`] mark (stored before the
///   marks existed: whether its photo was a library photo or a folder photo of the same name can not be told),
/// - it was stored or used within [`KEEP_UNUSED_SECS`].
///
/// The names are collected in one pass (not one scan per raster); the removal itself is one short transaction that
/// checks again what changed meanwhile. Returns: removed.
pub fn sweep(db: &Db, now: i64, max: usize, extra: &[std::path::PathBuf]) -> R<usize> {
    let cutoff = now - KEEP_UNUSED_SECS;
    let candidates: Vec<(String, Option<String>)> = {
        let mut st = db.conn.prepare("SELECT id, basis FROM mask_raster WHERE last_used < ?1").map_err(f)?;
        let rows = st.query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?;
        rows.collect::<Result<_, _>>().map_err(f)?
    };
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut used = HashSet::new();
    for p in extra {
        match std::fs::read_to_string(p) {
            Ok(text) => ids_in(&text, &mut used),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    let scan_start = crate::now().min(now);
    referenced_in(&db.conn, None, &mut used)?;
    let mut doomed = Vec::new();
    let mut refresh = Vec::new();
    for (id, basis) in candidates {
        if used.contains(&id) {
            refresh.push(id);
            continue;
        }
        let basis: Value = basis.as_deref().and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
        if basis.get(KEEP).and_then(Value::as_bool) == Some(true) {
            continue;
        }
        // stored before the marks existed: in doubt it stays
        if basis.get(LIBRARY).and_then(Value::as_bool) != Some(true) {
            continue;
        }
        if doomed.len() < max {
            doomed.push(id);
        }
    }
    let t = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(f)?;
    for id in &refresh {
        t.execute("UPDATE mask_raster SET last_used = ?2 WHERE id = ?1", params![id, now]).map_err(f)?;
    }
    // what was written while collecting
    let mut late = HashSet::new();
    referenced_in(&t, Some(scan_start - 1), &mut late)?;
    let mut removed = 0;
    for id in doomed.iter().filter(|id| !late.contains(*id)) {
        removed += t.execute("DELETE FROM mask_raster WHERE id = ?1 AND last_used < ?2", params![id, cutoff]).map_err(f)?;
    }
    t.commit().map_err(f)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> Db {
        let d = std::env::temp_dir().join(format!("graineo-mask-raster-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        Db::open(&d.join("library.sqlite")).unwrap()
    }

    #[test]
    fn depth_rasters_are_stored_with_16_bit() {
        let db = tmp_db();
        let values: Vec<u16> = (0..64 * 48).map(|i| (i * 21) as u16).collect();
        let id = put_depth_in(&db.conn, 64, 48, &values, &serde_json::json!({"rule": "depth"})).unwrap();
        assert_eq!(get_depth_in(&db.conn, &id), Some((64, 48, values.clone())));
        // the 8-bit reader does not take it, and its name is that of the masks crate
        assert!(get_in(&db.conn, &id).is_none());
        assert!(id.starts_with("sha256:") && id.len() == 71);
        assert!(put_depth_in(&db.conn, 64, 47, &values, &Value::Null).is_err());
    }

    fn sample(v: u8) -> Raster {
        Raster { width: 64, height: 48, data: (0..64 * 48).map(|i| if i % 64 > 30 { 255 } else { v }).collect() }
    }

    #[test]
    fn store_read_and_dedupe() {
        let db = tmp_db();
        let r = sample(0);
        let id = put_in(&db.conn, &r, &serde_json::json!({"rule": "subject"})).unwrap();
        assert!(id.starts_with("sha256:") && id.len() == 71);
        assert_eq!(put_in(&db.conn, &r, &Value::Null).unwrap(), id, "same content, same name");
        assert_eq!(usage(&db.conn).0, 1);
        assert_eq!(get_in(&db.conn, &id), Some(r));
        assert_eq!(get_in(&db.conn, &format!("sha256:{}", "0".repeat(64))), None);
        // a damaged row (content does not match its name) is not used
        db.conn.execute("UPDATE mask_raster SET data = ?2 WHERE id = ?1", params![id, zstd::bulk::compress(&sample(7).data, 3).unwrap()]).unwrap();
        assert_eq!(get_in(&db.conn, &id), None);
        // RGB patches live in the same table and are never read as rasters
        let rgb = vec![7u8; 4 * 2 * 3];
        let pid = put_rgb_in(&db.conn, 4, 2, &rgb, &Value::Null).unwrap();
        assert_eq!(get_rgb_in(&db.conn, &pid), Some((4, 2, rgb)));
        assert_eq!(get_in(&db.conn, &pid), None);
    }

    #[test]
    fn sweep_keeps_referenced_and_recent() {
        let db = tmp_db();
        let now = 2_000_000_000;
        let used = put_in(&db.conn, &sample(1), &Value::Null).unwrap();
        let in_history = put_in(&db.conn, &sample(2), &Value::Null).unwrap();
        let orphan = put_in(&db.conn, &sample(3), &serde_json::json!({"file": "a.jpg", "library": true})).unwrap();
        let recent = put_in(&db.conn, &sample(4), &Value::Null).unwrap();
        db.conn.execute("UPDATE mask_raster SET last_used = ?1 WHERE id <> ?2", params![now - KEEP_UNUSED_SECS - 10, recent]).unwrap();
        db.conn.execute("UPDATE mask_raster SET last_used = ?1 WHERE id = ?2", params![now - 5, recent]).unwrap();
        db.conn.execute_batch("INSERT INTO root_folder (path, name, origin) VALUES ('/x/', 'x', 'own'); INSERT INTO folder (root, path) VALUES (1, '');").unwrap();
        let folder: i64 = db.conn.query_row("SELECT id FROM folder", [], |r| r.get(0)).unwrap();
        db.conn.execute("INSERT INTO file (folder, name, format) VALUES (?1, 'a.jpg', 'JPG')", [folder]).unwrap();
        let file: i64 = db.conn.query_row("SELECT id FROM file", [], |r| r.get(0)).unwrap();
        crate::store::put_settings(&db.conn, file, None, Some(&format!("{{\"local\":{{\"raster\":\"{used}\"}}}}")), now).unwrap();
        db.conn.execute("INSERT INTO develop_step (file, copy, seq, time, kind, patch) VALUES (?1, '', 1, ?2, 'mask', ?3)", params![file, now, format!("{{\"x\":\"{in_history}\"}}")]).unwrap();
        assert_eq!(sweep(&db, now, 100, &[]).unwrap(), 1);
        assert!(get_in(&db.conn, &orphan).is_none());
        for id in [&used, &in_history, &recent] {
            assert!(get_in(&db.conn, id).is_some(), "{id} kept");
        }
    }

    /// A damaged or foreign row with an absurd size is refused before anything is allocated for it (all three
    /// readers), and not carried over by `copy_from`.
    #[test]
    fn absurd_sizes_are_refused_before_allocating() {
        let db = tmp_db();
        let id = put_in(&db.conn, &sample(1), &Value::Null).unwrap();
        let did = put_depth_in(&db.conn, 4, 2, &[1u16; 8], &Value::Null).unwrap();
        let pid = put_rgb_in(&db.conn, 4, 2, &[1u8; 24], &Value::Null).unwrap();
        for (w, h) in [(4_000_000_000i64, 4_000_000_000i64), (0, 2), (16385, 1), (-1, 4)] {
            db.conn.execute("UPDATE mask_raster SET width = ?1, height = ?2", params![w, h]).unwrap();
            assert_eq!(get_in(&db.conn, &id), None, "{w}x{h}");
            assert_eq!(get_depth_in(&db.conn, &did), None, "{w}x{h}");
            assert_eq!(get_rgb_in(&db.conn, &pid), None, "{w}x{h}");
            let dst = tmp_db();
            assert_eq!(copy_from(&db.conn, &dst.conn, &HashSet::from([id.clone(), did.clone(), pid.clone()])).unwrap(), 0);
        }
    }

    #[test]
    fn names_in_text() {
        let a = format!("sha256:{}", "ab".repeat(32));
        let b = format!("sha256:{}", "0".repeat(64));
        let mut out = HashSet::new();
        ids_in(&format!(r#"{{"raster":"{a}","removals":[{{"patch":"{b}"}}],"x":"sha256:zz","y":"sha256:{}"}}"#, "1".repeat(10)), &mut out);
        assert_eq!(out, HashSet::from([a, b]));
    }

    /// A photo outside the library keeps its settings in the sidecar next to it: its rasters and patches are never
    /// removed, however long nobody used them. Rasters of library photos follow the references; presets count too.
    #[test]
    fn sweep_never_removes_what_may_still_be_needed() {
        let d = std::env::temp_dir().join(format!("graineo-mask-raster-sweep-{}-{}", std::process::id(), crate::now()));
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        let now = 2_000_000_000;
        let root = db.create_root("/x/", "x", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        db.conn.execute("INSERT INTO file (folder, name, format) VALUES (?1, 'in.jpg', 'JPG')", [folder]).unwrap();
        let basis = |name: &str| serde_json::json!({"rule": "remove", "file": name});
        db.conn.execute("INSERT INTO file (folder, name, format) VALUES (?1, 'same.jpg', 'JPG')", [folder]).unwrap();
        // stored for a photo opened from a folder (not in the library): marked to keep
        let outside = std::path::Path::new("/elsewhere/out.jpg");
        let b = basis_for(&db.conn, &basis("out.jpg"), outside);
        assert_eq!(b["keep"], true);
        let folder_patch = put_rgb_in(&db.conn, 2, 2, &[9u8; 12], &b).unwrap();
        // the same content stored again for a library photo does not lose the mark
        let b2 = basis_for(&db.conn, &basis("in.jpg"), std::path::Path::new("/x/in.jpg"));
        assert!(b2.get("keep").is_none(), "library photo: {b2}");
        assert_eq!(b2["library"], true);
        assert_eq!(put_rgb_in(&db.conn, 2, 2, &[9u8; 12], &b2).unwrap(), folder_patch);
        // stored before the mark existed, for a photo the library does not know: kept (in doubt)
        let legacy_outside = put_in(&db.conn, &sample(5), &basis("unknown.jpg")).unwrap();
        // stored before the marks existed for a folder photo whose name a library photo has too: kept (could be either)
        let legacy_same_name = put_in(&db.conn, &sample(9), &basis("same.jpg")).unwrap();
        // no basis at all: kept
        let no_basis = put_in(&db.conn, &sample(6), &Value::Null).unwrap();
        let lib = |name: &str| basis_for(&db.conn, &basis(name), &std::path::Path::new("/x/").join(name));
        // a library photo's raster only an own preset names: kept
        let in_preset = put_in(&db.conn, &sample(7), &lib("in.jpg")).unwrap();
        // a library photo's raster nothing names: removed
        let orphan = put_in(&db.conn, &sample(8), &lib("in.jpg")).unwrap();
        db.conn.execute("UPDATE mask_raster SET last_used = ?1", [now - KEEP_UNUSED_SECS - 10]).unwrap();
        let presets = d.join("presets.json");
        std::fs::write(&presets, format!(r#"[{{"name":"p","settings":{{"local":{{"raster":"{in_preset}"}}}}}}]"#)).unwrap();
        // a presets file that exists but cannot be read: nothing is removed
        let unreadable = d.join("unreadable.json");
        std::fs::create_dir_all(&unreadable).unwrap();
        assert!(sweep(&db, now, 100, &[presets.clone(), unreadable]).is_err());
        assert!(get_in(&db.conn, &orphan).is_some());
        // a missing file is fine
        assert_eq!(sweep(&db, now, 100, &[presets, d.join("missing.json")]).unwrap(), 1);
        assert!(get_in(&db.conn, &orphan).is_none());
        assert!(get_rgb_in(&db.conn, &folder_patch).is_some(), "photo outside the library");
        for id in [&legacy_outside, &legacy_same_name, &no_basis, &in_preset] {
            assert!(get_in(&db.conn, id).is_some(), "{id} kept");
        }
        // years later: still there
        assert_eq!(sweep(&db, now + 10 * 365 * 24 * 3600, 100, &[]).unwrap(), 1, "only the preset raster goes once the preset is gone");
        assert!(get_rgb_in(&db.conn, &folder_patch).is_some());
        drop(db);
        std::fs::remove_dir_all(d).unwrap();
    }
}
