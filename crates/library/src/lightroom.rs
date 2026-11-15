//! Import from a Lightroom Classic catalog (`.lrcat`, SQLite).
//!
//! The catalog is opened **strictly read-only** (`mode=ro` plus `PRAGMA query_only = 1`).
//! If the WAL file is empty (Lightroom closed), additionally `immutable=1`: SQLite then creates neither
//! locks nor side files. If Lightroom is running and changes are in the WAL, reading goes without `immutable`
//! through the normal locks and the WAL – otherwise those changes would be missing, and reading
//! during a checkpoint would yield "database disk image is malformed". If that fails (e.g. an
//! orphaned WAL without `-shm`), fall back to `immutable`: then only the most recent changes are missing.
//!
//! Repeatable: everything is matched via `lr_id` (= `id_local` in the catalog) or the path;
//! a second run syncs instead of duplicating.
//!
//! **Own changes stay.** A new import adds and updates what comes from
//! Lightroom and never takes back what was added or changed in graineo:
//! - Values (stars, flag, label each on its own; caption; names and places of collections and keywords;
//!   face boxes; places of root folders, folders and files): the Lightroom value is taken only if the
//!   graineo value is still the one Lightroom had at the last import (`lr_import_state`, for ratings
//!   `image.lr_state`). Changed on both sides: the own value stays, the report names the conflict. Without
//!   a stored state (first import after 0.107.0) the existing graineo value stays.
//! - Things removed in graineo (collections, keywords, virtual copies, faces, folders, assignments to
//!   collections) are not brought back. Lightroom's `id_local` comes from one counter for all tables:
//!   everything up to the highest id seen at the last import (`info.lightroom_horizon`) already existed
//!   then; missing in graineo means removed there. New in Lightroom means a higher id.
//! - Collections deleted in Lightroom stay in graineo (as own collections, with all their photos).
//!   Assignments removed in Lightroom go only if graineo still holds them unchanged.
//! - Keywords of a photo: replaced from Lightroom only while they were never changed in graineo
//!   (`image.keywords_changed`). Develop settings: the import does not touch them (the handover writes only the
//!   Lightroom columns, [`crate::lr_handover`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::json;

use crate::db::{orientation_from_lightroom, Db};
use crate::label;

/// Commit what is written so far and start the next block: other writers waiting for the library get a short
/// window (prepared statements stay valid across the commit).
fn breathe(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("COMMIT")?;
    std::thread::sleep(std::time::Duration::from_millis(30));
    conn.execute_batch("BEGIN IMMEDIATE")
}

/// Values of the catalog at the last import, per kind and Lightroom id: the base of the rule "Lightroom
/// value only if the graineo value is unchanged since the last import" (see module docs).
const STATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS lr_import_state (kind TEXT NOT NULL, lr_id INTEGER NOT NULL, value TEXT NOT NULL, PRIMARY KEY (kind, lr_id)) WITHOUT ROWID";

/// Stored catalog values of one kind (empty without a previous import).
pub(crate) fn state_load(conn: &Connection, kind: &str) -> rusqlite::Result<HashMap<i64, serde_json::Value>> {
    conn.execute_batch(STATE_SCHEMA)?;
    let mut st = conn.prepare("SELECT lr_id, value FROM lr_import_state WHERE kind = ?1")?;
    let v = st
        .query_map([kind], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|r| r.ok())
        .filter_map(|(id, t)| serde_json::from_str(&t).ok().map(|v| (id, v)))
        .collect();
    Ok(v)
}

/// Replace the stored catalog values of one kind by those of this import (only rows that differ are written: a
/// repeated import of an unchanged catalog writes nothing).
pub(crate) fn state_save(conn: &Connection, kind: &str, rows: &[(i64, serde_json::Value)]) -> rusqlite::Result<()> {
    conn.execute_batch(STATE_SCHEMA)?;
    let mut stored: HashMap<i64, String> = {
        let mut st = conn.prepare("SELECT lr_id, value FROM lr_import_state WHERE kind = ?1")?;
        let v = st.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let mut ins = conn.prepare_cached("INSERT OR REPLACE INTO lr_import_state(kind, lr_id, value) VALUES(?1, ?2, ?3)")?;
    for (id, v) in rows {
        let text = v.to_string();
        if stored.remove(id).as_deref() != Some(text.as_str()) {
            ins.execute(params![kind, id, text])?;
        }
    }
    let mut del = conn.prepare_cached("DELETE FROM lr_import_state WHERE kind = ?1 AND lr_id = ?2")?;
    for id in stored.keys() {
        del.execute(params![kind, id])?;
    }
    Ok(())
}

/// The three-way rule: the Lightroom value if the own value is still the one of the last import, otherwise the own
/// one. Without a stored value (first import after 0.107.0) the own one. Second: true if an own change kept a
/// different Lightroom value out.
pub(crate) fn merge<T: PartialEq>(own: T, old: Option<T>, lr: T) -> (T, bool) {
    match old {
        Some(o) if o == own => (lr, false),
        _ => {
            let kept = own != lr;
            (own, kept)
        }
    }
}

/// Highest Lightroom id that already existed at the last import (Lightroom numbers all tables with one counter).
/// Without a stored value: the highest Lightroom id the library knows (0 if nothing was imported yet).
pub(crate) fn horizon(conn: &Connection) -> i64 {
    if let Some(v) = conn.query_row("SELECT value FROM info WHERE name = 'lightroom_horizon'", [], |r| r.get::<_, String>(0)).optional().ok().flatten().and_then(|t| t.parse().ok()) {
        return v;
    }
    conn.query_row(
        "SELECT max(coalesce((SELECT max(lr_id) FROM image), 0), coalesce((SELECT max(lr_id) FROM file), 0), coalesce((SELECT max(lr_id) FROM keyword), 0),
                    coalesce((SELECT max(lr_id) FROM collection), 0), coalesce((SELECT max(lr_id) FROM face), 0), coalesce((SELECT max(lr_id) FROM folder), 0),
                    coalesce((SELECT max(lr_id) FROM root_folder), 0))",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Summary of an import.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub catalog: String,
    pub roots: usize,
    pub folder: usize,
    pub files: usize,
    pub images: usize,
    pub virtual_copies: usize,
    pub new_images: usize,
    pub per_format: BTreeMap<String, usize>,
    pub keywords: usize,
    pub keyword_assignments: usize,
    pub collections: usize,
    pub collection_sets: usize,
    pub smart_collections: usize,
    /// of these, translated into the own search syntax
    pub smart_translated: usize,
    pub collection_assignments: usize,
    pub with_stars: usize,
    pub flagged: usize,
    pub rejected: usize,
    pub with_label: usize,
    pub unknown_labels: BTreeMap<String, usize>,
    /// rating changed in Lightroom and in graineo – the own value stays
    pub conflicts: Vec<String>,
    /// own changes the import left as they were although Lightroom has something else (values, places, names)
    /// or did not bring back (removed in graineo)
    pub own_kept: usize,
    /// in our library but no longer in the catalog (left untouched)
    pub no_longer_in_catalog: usize,
    /// face boxes from the catalog and of these the ones with a confirmed name (the other names are unverified suggestions by Lightroom)
    pub faces: usize,
    pub faces_confirmed: usize,
    pub read_duration_ms: u128,
    pub write_duration_ms: u128,
    pub run: i64,
}

impl Report {
    pub fn json(&self) -> serde_json::Value {
        json!({
            "catalog": self.catalog,
            "roots": self.roots,
            "folder": self.folder,
            "files": self.files,
            "images": self.images,
            "virtual_copies": self.virtual_copies,
            "new_images": self.new_images,
            "per_format": self.per_format,
            "keywords": self.keywords,
            "keyword_assignments": self.keyword_assignments,
            "collections": self.collections,
            "collection_sets": self.collection_sets,
            "smart_collections": self.smart_collections,
            "smart_translated": self.smart_translated,
            "collection_assignments": self.collection_assignments,
            "with_stars": self.with_stars,
            "flagged": self.flagged,
            "rejected": self.rejected,
            "with_label": self.with_label,
            "unknown_labels": self.unknown_labels,
            "conflicts": self.conflicts,
            "own_kept": self.own_kept,
            "no_longer_in_catalog": self.no_longer_in_catalog,
            "faces": self.faces,
            "faces_confirmed": self.faces_confirmed,
            "read_duration_ms": self.read_duration_ms,
            "write_duration_ms": self.write_duration_ms,
        })
    }
}

/// SQLite `file:` URI of a path, percent-encoded (spaces, umlauts, '?', '#', '%' would otherwise cut or change the
/// path).
pub(crate) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for b in path.as_os_str().as_encoded_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => uri.push(*b as char),
            x => uri.push_str(&format!("%{x:02X}")),
        }
    }
    uri
}

/// Open the catalog read-only (see module docs).
pub fn open_catalog(path: &Path) -> rusqlite::Result<Connection> {
    let uri = file_uri(path);
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let wal_empty = std::fs::metadata(&wal).map_or(true, |m| m.len() == 0);
    let open = |suffix_part: &str| -> rusqlite::Result<Connection> {
        let conn = Connection::open_with_flags(format!("{uri}{suffix_part}"), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.pragma_update(None, "query_only", 1)?;
        // PRAGMA instead of `busy_timeout()`: briefly busy (Lightroom is writing) means wait, not fail
        conn.pragma_update(None, "busy_timeout", 60_000)?;
        // forces actually reading the file (and thus the fallback below if the WAL is not readable)
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))?;
        Ok(conn)
    };
    if wal_empty {
        return open("?mode=ro&immutable=1");
    }
    open("?mode=ro").or_else(|_| open("?mode=ro&immutable=1"))
}

struct LrImage {
    id: i64,
    file: i64,
    capture: Option<String>,
    stars: u8,
    flag: i8,
    label_text: String,
    orientation: Option<String>,
    master: Option<i64>,
    copy: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    camera: Option<String>,
    video: bool,
    lens: Option<String>,
    aperture: Option<f64>,
    focal: Option<f64>,
    iso: Option<i64>,
    time: Option<f64>,
    image_text: Option<String>,
    /// location, creator, copyright, alt text (for search only)
    extra_text: Option<String>,
    /// last change in Lightroom (`touchTime`, Unix seconds)
    touch: Option<i64>,
}

struct LrFile {
    id: i64,
    folder: i64,
    name: String,
    sidecar: Option<String>,
}

fn number(v: rusqlite::types::Value) -> Option<f64> {
    match v {
        rusqlite::types::Value::Integer(i) => Some(i as f64),
        rusqlite::types::Value::Real(r) => Some(r),
        rusqlite::types::Value::Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: rusqlite::types::Value) -> Option<String> {
    match v {
        rusqlite::types::Value::Text(t) => Some(t),
        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
        rusqlite::types::Value::Real(r) => Some(r.to_string()),
        _ => None,
    }
}

/// Lightroom time ("2019-05-04T12:34:56.12" or with time zone) → "2019-05-04T12:34:56".
fn time(t: Option<String>) -> Option<String> {
    let t = t?;
    let t = t.trim();
    if t.len() < 10 || !t.as_bytes()[0..4].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut s: String = t.chars().take(19).collect();
    if s.len() == 10 {
        s.push_str("T00:00:00");
    }
    Some(s.replace(' ', "T"))
}

/// Run the import. `progress` receives short status messages.
pub fn import(db: &Db, catalog: &Path, progress: &dyn Fn(&str)) -> Result<Report, String> {
    let error = |e: rusqlite::Error| e.to_string();
    let mut b = Report { catalog: catalog.display().to_string(), ..Default::default() };
    if crate::move_job::active() {
        return Err("Ein Verschieben läuft oder ist angehalten – Lightroom-Import erst, wenn es fertig oder beendet ist".into());
    }
    let t0 = Instant::now();
    progress("Lightroom-Katalog lesen …");
    let lr = open_catalog(catalog).map_err(|e| format!("Katalog nicht lesbar: {e}"))?;

    // ── Reading ──
    let root_rows: Vec<(i64, String, String)> = {
        let mut st = lr.prepare("SELECT id_local, absolutePath, name FROM AgLibraryRootFolder").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let folder: Vec<(i64, i64, String)> = {
        let mut st = lr.prepare("SELECT id_local, rootFolder, pathFromRoot FROM AgLibraryFolder").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let files: Vec<LrFile> = {
        let mut st = lr.prepare("SELECT id_local, folder, idx_filename, baseName, extension, sidecarExtensions FROM AgLibraryFile").map_err(error)?;
        let r = st
            .query_map([], |r| {
                let idx: Option<String> = r.get(2)?;
                let base: String = r.get(3)?;
                let ext: String = r.get(4)?;
                let name = idx.filter(|s| !s.is_empty()).unwrap_or_else(|| if ext.is_empty() { base.clone() } else { format!("{base}.{ext}") });
                Ok(LrFile { id: r.get(0)?, folder: r.get(1)?, name: crate::nfc(&name), sidecar: text(r.get(5)?).filter(|s| !s.is_empty()) })
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        r
    };
    let images: Vec<LrImage> = {
        // locations, creator, copyright: not present in every (older or trimmed) catalog
        let has = |t: &str| lr.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0;
        let place = if ["AgHarvestedIptcMetadata", "AgInternedIptcLocation", "AgInternedIptcCity", "AgInternedIptcState", "AgInternedIptcCountry"].iter().all(|t| has(t)) {
            "(SELECT trim(coalesce(loc.value,'') || ' ' || coalesce(city.value,'') || ' ' || coalesce(st.value,'') || ' ' || coalesce(co.value,''))
                           FROM AgHarvestedIptcMetadata h
                           LEFT JOIN AgInternedIptcLocation loc ON loc.id_local = h.locationRef
                           LEFT JOIN AgInternedIptcCity city ON city.id_local = h.cityRef
                           LEFT JOIN AgInternedIptcState st ON st.id_local = h.stateRef
                           LEFT JOIN AgInternedIptcCountry co ON co.id_local = h.countryRef
                          WHERE h.image = i.id_local LIMIT 1)"
        } else {
            "NULL"
        };
        let creator = if has("AgLibraryIPTC") {
            "(SELECT trim(coalesce(t.altTextAccessibility,'') || ' ' || coalesce(t.extDescrAccessibility,'')) FROM AgLibraryIPTC t WHERE t.image = i.id_local LIMIT 1)"
        } else {
            "NULL"
        };
        let mut st = lr
            .prepare(&format!(
                "SELECT i.id_local, i.rootFile, i.captureTime, i.rating, i.pick, i.colorLabels, i.orientation, i.masterImage, i.copyName,
                        i.fileWidth, i.fileHeight, cm.value, i.fileFormat,
                        e.aperture, e.focalLength, e.isoSpeedRating, e.shutterSpeed, l.value,
                        (SELECT caption FROM AgLibraryIPTC t WHERE t.image = i.id_local AND t.caption <> '' LIMIT 1), i.touchTime,
                        {place}, {creator}
                 FROM Adobe_images i
                 LEFT JOIN AgHarvestedExifMetadata e ON e.image = i.id_local
                 LEFT JOIN AgInternedExifCameraModel cm ON cm.id_local = e.cameraModelRef
                 LEFT JOIN AgInternedExifLens l ON l.id_local = e.lensRef"
            ))
            .map_err(error)?;
        let r = st
            .query_map([], |r| {
                Ok(LrImage {
                    id: r.get(0)?,
                    file: r.get(1)?,
                    capture: time(text(r.get(2)?)),
                    stars: number(r.get(3)?).unwrap_or(0.0).clamp(0.0, 5.0) as u8,
                    flag: number(r.get(4)?).unwrap_or(0.0).clamp(-1.0, 1.0) as i8,
                    label_text: text(r.get(5)?).unwrap_or_default(),
                    orientation: text(r.get(6)?),
                    master: r.get(7)?,
                    copy: text(r.get(8)?).filter(|s| !s.is_empty()),
                    width: number(r.get(9)?).map(|v| v as i64),
                    height: number(r.get(10)?).map(|v| v as i64),
                    camera: text(r.get(11)?).filter(|s| !s.is_empty()),
                    video: text(r.get(12)?).as_deref() == Some("VIDEO"),
                    // Lightroom stores aperture and shutter speed as APEX values: f = 2^(Av/2), t = 2^(−Tv)
                    aperture: number(r.get(13)?).filter(|v| v.is_finite()).map(|av| ((2f64.powf(av / 2.0)) * 10.0).round() / 10.0),
                    focal: number(r.get(14)?).filter(|v| *v > 0.0),
                    iso: number(r.get(15)?).filter(|v| *v > 0.0).map(|v| v.round() as i64),
                    time: number(r.get(16)?).filter(|v| v.is_finite()).map(|tv| 2f64.powf(-tv)),
                    lens: text(r.get(17)?).filter(|s| !s.is_empty()),
                    image_text: text(r.get(18)?).filter(|s| !s.trim().is_empty()),
                    // seconds since 2001-01-01 (Cocoa time)
                    touch: number(r.get(19)?).filter(|v| *v > 0.0).map(|v| v as i64 + 978_307_200),
                    extra_text: {
                        let a = text(r.get(20)?).unwrap_or_default();
                        let b = text(r.get(21)?).unwrap_or_default();
                        Some(format!("{a} {b}").trim().to_string()).filter(|s| !s.is_empty())
                    },
                })
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        r
    };
    let keywords: Vec<(i64, Option<String>, Option<i64>, String, Option<String>)> = {
        let mut st = lr.prepare("SELECT id_local, name, parent, genealogy, keywordType FROM AgLibraryKeyword ORDER BY length(genealogy), id_local").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, text(r.get(1)?), r.get(2)?, r.get(3)?, text(r.get(4)?)))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let keyword_image: Vec<(i64, i64)> = {
        let mut st = lr.prepare("SELECT image, tag FROM AgLibraryKeywordImage").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let collections: Vec<(i64, String, Option<i64>, String, String)> = {
        let mut st = lr
            .prepare(
                "SELECT id_local, name, parent, creationId, systemOnly FROM AgLibraryCollection
                 WHERE creationId IN ('com.adobe.ag.library.collection','com.adobe.ag.library.group','com.adobe.ag.library.smart_collection')
                 ORDER BY length(genealogy), id_local",
            )
            .map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, text(r.get(4)?).unwrap_or_default()))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    // (row id, collection, image, position)
    let collection_image: Vec<(i64, i64, i64, Option<f64>)> = {
        let mut st = lr.prepare("SELECT id_local, collection, image, positionInCollection FROM AgLibraryCollectionImage").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, number(r.get(3)?)))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let smart_rules: HashMap<i64, String> = {
        let mut st = lr.prepare("SELECT collection, content FROM AgLibraryCollectionContent WHERE owningModule = 'ag.library.smart_collection'").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, text(r.get(1)?).unwrap_or_default()))).map_err(error)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(error)?;
        r
    };
    let lr_faces = crate::people::read_lightroom(&lr).unwrap_or_default();
    drop(lr);
    b.read_duration_ms = t0.elapsed().as_millis();
    // highest id of this catalog: the horizon of the next import
    let new_horizon = [
        root_rows.iter().map(|r| r.0).max(),
        folder.iter().map(|r| r.0).max(),
        files.iter().map(|r| r.id).max(),
        images.iter().map(|r| r.id).max(),
        keywords.iter().map(|r| r.0).max(),
        collections.iter().map(|r| r.0).max(),
        collection_image.iter().map(|r| r.0).max(),
        lr_faces.iter().map(|r| r.0).max(),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0);

    // ── Writing (in blocks: the transaction is committed every few thousand rows and between the sections, so other
    // writers – develop settings, ratings, the sync – get their turn; the import is idempotent via `lr_id`) ──
    let t1 = Instant::now();
    progress(&format!("{} Bilder übernehmen …", images.len()));
    let tx = db.conn.unchecked_transaction().map_err(error)?;
    let mut written = 0usize;
    let mut pause_point = |n: usize| -> Result<(), String> {
        written += n;
        if written >= 2000 {
            written = 0;
            breathe(&db.conn).map_err(error)?;
        }
        Ok(())
    };
    let run = db.new_run("lightroom", &catalog.display().to_string()).map_err(error)?;
    b.run = run;
    let known = horizon(&tx);

    // root folders and folders: a place changed in graineo (relinked, moved) stays; removed in graineo stays removed
    let old_roots = state_load(&tx, "root").map_err(error)?;
    let mut new_roots: Vec<(i64, serde_json::Value)> = Vec::new();
    let mut root_id: HashMap<i64, i64> = HashMap::new();
    for (lid, path, name) in &root_rows {
        let mut p = crate::nfc(path);
        if !p.ends_with('/') {
            p.push('/');
        }
        new_roots.push((*lid, json!(p)));
        let found: Option<(i64, String)> = tx.query_row("SELECT id, path FROM root_folder WHERE lr_id=?1", [lid], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(error)?;
        let id = match found {
            Some((id, own)) => {
                let old = old_roots.get(lid).and_then(|v| v.as_str()).map(str::to_string);
                let (path_now, kept) = merge(own.clone(), old, p.clone());
                b.own_kept += usize::from(kept);
                if path_now != own {
                    tx.execute("UPDATE root_folder SET path=?2, name=?3 WHERE id=?1", params![id, path_now, name]).map_err(error)?;
                }
                id
            }
            None if *lid <= known => {
                b.own_kept += 1;
                continue;
            }
            None => db.create_root(&p, name, "lightroom", Some(*lid)).map_err(error)?,
        };
        root_id.insert(*lid, id);
    }
    b.roots = root_id.len();
    let old_folders = state_load(&tx, "folder").map_err(error)?;
    let mut new_folders: Vec<(i64, serde_json::Value)> = Vec::new();
    let mut folder_id: HashMap<i64, i64> = HashMap::new();
    for (lid, root, path) in &folder {
        let path = crate::nfc(path);
        new_folders.push((*lid, json!([root, path])));
        let Some(&sid) = root_id.get(root) else { continue };
        let found: Option<(i64, i64, String)> = tx.query_row("SELECT id, root, path FROM folder WHERE lr_id=?1", [lid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?;
        let id = match found {
            Some((id, own_root, own_path)) => {
                let old = old_folders.get(lid).and_then(|v| Some((*root_id.get(&v.get(0)?.as_i64()?)?, v.get(1)?.as_str()?.to_string())));
                let (now, kept) = merge((own_root, own_path.clone()), old, (sid, path.clone()));
                b.own_kept += usize::from(kept);
                if now != (own_root, own_path) {
                    // folder moved/renamed in Lightroom: update the path (if free)
                    let _ = tx.execute("UPDATE folder SET root=?2, path=?3 WHERE id=?1", params![id, now.0, now.1]);
                }
                id
            }
            None if *lid <= known => {
                b.own_kept += 1;
                continue;
            }
            None => db.create_folder(sid, &path, Some(*lid)).map_err(error)?,
        };
        folder_id.insert(*lid, id);
    }
    b.folder = folder_id.len();
    state_save(&tx, "root", &new_roots).map_err(error)?;
    state_save(&tx, "folder", &new_folders).map_err(error)?;

    // files: formats and capture dates come from the original image (not from virtual copies)
    let master_to_file: HashMap<i64, &LrImage> = images.iter().filter(|i| i.master.is_none()).map(|i| (i.file, i)).collect();
    let mut file_id: HashMap<i64, i64> = HashMap::new();
    // files new in this import (their images and faces are new too)
    let mut new_files: HashSet<i64> = HashSet::new();
    let old_files = state_load(&tx, "file").map_err(error)?;
    let mut state_files: Vec<(i64, serde_json::Value)> = Vec::with_capacity(files.len());
    {
        let mut find_lr = tx.prepare_cached("SELECT id, folder, name FROM file WHERE lr_id = ?1").map_err(error)?;
        let mut find_name = tx.prepare_cached("SELECT id, folder, name FROM file WHERE folder = ?1 AND name = ?2").map_err(error)?;
        // do not recreate entries removed in graineo (deleted or file was missing)
        let removed: std::collections::HashSet<i64> = {
            let mut st = tx.prepare("SELECT lr_id FROM removed_lr_file").map_err(error)?;
            let r = st.query_map([], |r| r.get(0)).map_err(error)?.flatten().collect();
            r
        };
        let mut new = tx
            .prepare_cached("INSERT INTO file(folder, name, format, camera, capture_time, width, height, orientation, sidecars, lr_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")
            .map_err(error)?;
        let mut upd = tx
            .prepare_cached(
                "UPDATE file SET folder=CASE WHEN id IN (SELECT file FROM moved_files) THEN folder ELSE ?2 END, name=CASE WHEN id IN (SELECT file FROM moved_files) THEN name ELSE ?3 END, format=?4, camera=coalesce(?5,camera), capture_time=CASE WHEN capture_time_original IS NULL THEN coalesce(?6,capture_time) ELSE capture_time END, width=coalesce(?7,width),
                 height=coalesce(?8,height), orientation=coalesce(?9,orientation), sidecars=?10, lr_id=?11 WHERE id=?1",
            )
            .map_err(error)?;
        let mut exif = tx
            .prepare_cached(
                "UPDATE file SET lens=coalesce(?2,lens), aperture=coalesce(?3,aperture), focal_length=coalesce(?4,focal_length), iso=coalesce(?5,iso),
                 time=coalesce(?6,time), exif=CASE WHEN ?7 THEN 1 ELSE exif END WHERE id=?1",
            )
            .map_err(error)?;
        for d in &files {
            state_files.push((d.id, json!([d.folder, d.name])));
            let Some(&fid) = folder_id.get(&d.folder) else { continue };
            if removed.contains(&d.id) {
                continue;
            }
            let m = master_to_file.get(&d.id);
            let format = if m.is_some_and(|m| m.video) { "VIDEO" } else { crate::format_from_extension(&d.name).unwrap_or("ANDERE") };
            *b.per_format.entry(format.to_string()).or_default() += 1;
            let (camera, capture, width, height, orient) =
                m.map_or((None, None, None, None, None), |m| (m.camera.clone(), m.capture.clone(), m.width, m.height, m.orientation.as_deref().and_then(orientation_from_lightroom)));
            let row = |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, String)> { Ok((r.get(0)?, r.get(1)?, r.get(2)?)) };
            let existing: Option<(i64, i64, String)> = match find_lr.query_row([d.id], row).optional().map_err(error)? {
                Some(v) => Some(v),
                None => find_name.query_row(params![fid, d.name], row).optional().map_err(error)?,
            };
            let id = match existing {
                Some((id, own_folder, own_name)) => {
                    // place: moved or renamed in graineo → stays (files moved by a job: `moved_files`, see the statement)
                    let old = old_files.get(&d.id).and_then(|v| Some((*folder_id.get(&v.get(0)?.as_i64()?)?, v.get(1)?.as_str()?.to_string())));
                    let ((folder_now, name_now), kept) = merge((own_folder, own_name), old, (fid, d.name.clone()));
                    b.own_kept += usize::from(kept);
                    upd.execute(params![id, folder_now, name_now, format, camera, capture, width, height, orient, d.sidecar, d.id]).map_err(error)?;
                    id
                }
                None => {
                    new.execute(params![fid, d.name, format, camera, capture, width, height, orient, d.sidecar, d.id]).map_err(error)?;
                    let id = tx.last_insert_rowid();
                    new_files.insert(id);
                    id
                }
            };
            if let Some(m) = m {
                let has = m.aperture.is_some() || m.focal.is_some() || m.iso.is_some() || m.time.is_some() || m.lens.is_some();
                exif.execute(params![id, m.lens, m.aperture, m.focal, m.iso, m.time, has]).map_err(error)?;
            }
            file_id.insert(d.id, id);
            pause_point(1)?;
        }
    }
    b.files = file_id.len();
    state_save(&tx, "file", &state_files).map_err(error)?;
    pause_point(usize::MAX / 2)?;

    // images (originals and virtual copies)
    let mut image_id: HashMap<i64, i64> = HashMap::new();
    // images new in this import (their faces are new too)
    let mut new_images: HashSet<i64> = HashSet::new();
    let old_captions = state_load(&tx, "caption").map_err(error)?;
    let mut state_captions: Vec<(i64, serde_json::Value)> = Vec::with_capacity(images.len());
    {
        type Found = (i64, i64, i64, i64, Option<String>, Option<String>);
        let found = |r: &rusqlite::Row| -> rusqlite::Result<Found> { Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)) };
        let mut find_lr = tx.prepare_cached("SELECT id, stars, flag, label, lr_state, caption FROM image WHERE lr_id = ?1").map_err(error)?;
        let mut find_orig = tx.prepare_cached("SELECT id, stars, flag, label, lr_state, caption FROM image WHERE file = ?1 AND copy IS NULL AND lr_id IS NULL").map_err(error)?;
        let mut new = tx
            .prepare_cached("INSERT INTO image(file, copy, stars, flag, label, label_lr, lr_id, lr_state, run) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")
            .map_err(error)?;
        let mut upd = tx.prepare_cached("UPDATE image SET file=?2, copy=?3, stars=?4, flag=?5, label=?6, label_lr=?7, lr_id=?8, lr_state=?9 WHERE id=?1").map_err(error)?;
        let mut set_text = tx.prepare_cached("UPDATE image SET caption=?2, lr_text=?3 WHERE id=?1").map_err(error)?;
        let mut set_time = tx
            .prepare_cached("UPDATE image SET lr_touch=?2, rating_time=CASE WHEN ?3 AND (rating_time IS NULL OR changed IS NULL) THEN ?2 ELSE rating_time END, keywords_time=CASE WHEN keywords_changed IS NULL THEN ?2 ELSE keywords_time END WHERE id=?1")
            .map_err(error)?;
        let mut path_of = tx
            .prepare_cached("SELECT s.path || o.path || d.name FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE d.id=?1")
            .map_err(error)?;
        let names = ["Sterne", "Markierung", "Etikett"];
        for i in &images {
            state_captions.push((i.id, json!(i.image_text)));
            let Some(&did) = file_id.get(&i.file) else { continue };
            let label = match label::from_lightroom(&i.label_text) {
                Some(e) => e,
                None => {
                    *b.unknown_labels.entry(i.label_text.clone()).or_default() += 1;
                    0
                }
            };
            let lr_values = [i64::from(i.stars), i64::from(i.flag), i64::from(label)];
            let state = format!("{},{},{}", lr_values[0], lr_values[1], lr_values[2]);
            let copy = if i.master.is_some() { Some(i.copy.clone().unwrap_or_else(|| "Kopie".into())) } else { None };
            let label_lr = (!i.label_text.is_empty()).then(|| i.label_text.clone());
            let existing = match find_lr.query_row([i.id], found).optional().map_err(error)? {
                Some(v) => Some(v),
                None if copy.is_none() => find_orig.query_row([did], found).optional().map_err(error)?,
                None => None,
            };
            // a virtual copy that existed at the last import and is missing now was removed in graineo: it stays removed
            if existing.is_none() && copy.is_some() && i.id <= known && !new_files.contains(&did) {
                b.own_kept += 1;
                continue;
            }
            if copy.is_some() {
                b.virtual_copies += 1;
            }
            let values = match &existing {
                None => lr_values,
                Some((_, s, m, e, old_state, _)) => {
                    let own = [*s, *m, *e];
                    let old = old_state.as_deref().and_then(|t| {
                        let v: Vec<i64> = t.split(',').filter_map(|x| x.parse().ok()).collect();
                        (v.len() == 3).then(|| [v[0], v[1], v[2]])
                    });
                    match old {
                        // first time from Lightroom (image was already known from disk): own values take precedence if set
                        None if own != [0, 0, 0] && own != lr_values => {
                            let p: Option<String> = path_of.query_row([did], |r| r.get(0)).optional().map_err(error)?;
                            b.conflicts.push(format!("{}: eigen {:?} ≠ Lightroom {:?} – eigener Wert bleibt", p.unwrap_or_default(), own, lr_values));
                            own
                        }
                        None => lr_values,
                        // each value on its own: Lightroom's only where graineo still has the value of the last import;
                        // changed on both sides (differently) → the own value stays, named as a conflict
                        Some(a) => {
                            let mut v = own;
                            let mut clash = Vec::new();
                            for k in 0..3 {
                                if a[k] == own[k] {
                                    v[k] = lr_values[k];
                                } else if a[k] != lr_values[k] && own[k] != lr_values[k] {
                                    clash.push(format!("{} eigen {} ≠ Lightroom {}", names[k], own[k], lr_values[k]));
                                }
                            }
                            if !clash.is_empty() {
                                let p: Option<String> = path_of.query_row([did], |r| r.get(0)).optional().map_err(error)?;
                                b.conflicts.push(format!("{}: {} – eigener Wert bleibt", p.unwrap_or_default(), clash.join(", ")));
                            }
                            v
                        }
                    }
                }
            };
            if existing.is_some() && values != lr_values {
                b.own_kept += 1;
            }
            // caption: Lightroom's only if graineo still has the one of the last import (or none)
            let caption = match &existing {
                None => i.image_text.clone(),
                Some((.., own)) => {
                    let own = own.clone().filter(|t| !t.trim().is_empty());
                    let old = old_captions.get(&i.id).map(|v| v.as_str().map(str::to_string));
                    match old {
                        None if own.is_none() => i.image_text.clone(),
                        old => {
                            let (t, kept) = merge(own, old, i.image_text.clone());
                            b.own_kept += usize::from(kept);
                            t
                        }
                    }
                }
            };
            let id = match existing {
                Some((id, ..)) => {
                    upd.execute(params![id, did, copy, values[0], values[1], values[2], label_lr, i.id, state]).map_err(error)?;
                    id
                }
                None => {
                    new.execute(params![did, copy, values[0], values[1], values[2], label_lr, i.id, state, run]).map_err(error)?;
                    b.new_images += 1;
                    let id = tx.last_insert_rowid();
                    new_images.insert(id);
                    id
                }
            };
            if values[0] > 0 {
                b.with_stars += 1;
            }
            match values[1] {
                1 => b.flagged += 1,
                -1 => b.rejected += 1,
                _ => {}
            }
            if values[2] > 0 {
                b.with_label += 1;
            }
            set_text.execute(params![id, caption, i.extra_text]).map_err(error)?;
            // timestamp of the values: if they come from Lightroom, Lightroom's modification time applies
            set_time.execute(params![id, i.touch, values == lr_values]).map_err(error)?;
            image_id.insert(i.id, id);
            pause_point(1)?;
        }
    }
    b.images = image_id.len();
    state_save(&tx, "caption", &state_captions).map_err(error)?;
    pause_point(usize::MAX / 2)?;
    {
        let lr_ids: HashSet<i64> = images.iter().map(|i| i.id).collect();
        let mut st = tx.prepare("SELECT lr_id FROM image WHERE lr_id IS NOT NULL").map_err(error)?;
        let all: Vec<i64> = st.query_map([], |r| r.get(0)).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        b.no_longer_in_catalog = all.iter().filter(|i| !lr_ids.contains(i)).count();
    }

    // keywords (hierarchy via parent; the invisible root without a name is dropped)
    progress("Stichwörter übernehmen …");
    let mut keyword_id: HashMap<i64, i64> = HashMap::new();
    let old_keywords = state_load(&tx, "keyword").map_err(error)?;
    let mut state_keywords: Vec<(i64, serde_json::Value)> = Vec::with_capacity(keywords.len());
    {
        let mut find = tx.prepare_cached("SELECT id, name, parent, person FROM keyword WHERE lr_id = ?1").map_err(error)?;
        let mut new = tx.prepare_cached("INSERT INTO keyword(name, parent, person, lr_id) VALUES(?1,?2,?3,?4)").map_err(error)?;
        let mut upd = tx.prepare_cached("UPDATE keyword SET name=?2, parent=?3, person=?4 WHERE id=?1").map_err(error)?;
        for (lid, name, parent_lr, _gen, type_id) in &keywords {
            let Some(name) = name.as_deref().filter(|n| !n.is_empty()) else { continue };
            let person = type_id.as_deref() == Some("person");
            state_keywords.push((*lid, json!([name, parent_lr, person])));
            let parent = parent_lr.and_then(|p| keyword_id.get(&p).copied());
            let found: Option<(i64, String, Option<i64>, bool)> = find.query_row([lid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(error)?;
            let id = match found {
                Some((id, own_name, own_parent, own_person)) => {
                    let old = old_keywords.get(lid);
                    let old_name = old.and_then(|v| Some(v.get(0)?.as_str()?.to_string()));
                    // the parent of the last import, as it is called in graineo now (a top-level keyword: None)
                    let old_parent = old.and_then(|v| v.get(1)).map(|p| p.as_i64().and_then(|p| keyword_id.get(&p).copied()));
                    let old_person = old.and_then(|v| v.get(2)?.as_bool());
                    let (n, k1) = merge(own_name, old_name, name.to_string());
                    let (p, k2) = merge(own_parent, old_parent, parent);
                    let (s, k3) = merge(own_person, old_person, person);
                    b.own_kept += usize::from(k1 || k2 || k3);
                    upd.execute(params![id, n, p, s]).map_err(error)?;
                    id
                }
                // existed at the last import and is gone: deleted or merged in graineo – stays so
                None if *lid <= known => {
                    b.own_kept += 1;
                    continue;
                }
                None => {
                    new.execute(params![name, parent, person, lid]).map_err(error)?;
                    tx.last_insert_rowid()
                }
            };
            keyword_id.insert(*lid, id);
        }
    }
    b.keywords = keyword_id.len();
    state_save(&tx, "keyword", &state_keywords).map_err(error)?;
    pause_point(usize::MAX / 2)?;
    // replace keywords from Lightroom – except for images whose keywords were changed
    // in graineo (the own version stays; it is also in the sidecar)
    let own_kw: HashSet<i64> = {
        let mut st = tx.prepare("SELECT id FROM image WHERE keywords_changed IS NOT NULL").map_err(error)?;
        let r = st.query_map([], |r| r.get(0)).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        r
    };
    tx.execute("DELETE FROM image_keyword WHERE keyword IN (SELECT id FROM keyword WHERE lr_id IS NOT NULL) AND image NOT IN (SELECT id FROM image WHERE keywords_changed IS NOT NULL)", []).map_err(error)?;
    {
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1,?2)").map_err(error)?;
        for (img, tag) in &keyword_image {
            if image_id.get(img).is_some_and(|b| own_kw.contains(b)) {
                continue;
            }
            if let (Some(bid), Some(sid)) = (image_id.get(img), keyword_id.get(tag)) {
                b.keyword_assignments += ins.execute(params![bid, sid]).map_err(error)?;
            }
        }
    }
    // remove keywords deleted in Lightroom (only those from Lightroom)
    {
        let valid: HashSet<i64> = keyword_id.keys().copied().collect();
        let mut st = tx.prepare("SELECT id, lr_id FROM keyword WHERE lr_id IS NOT NULL").map_err(error)?;
        let all: Vec<(i64, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        for (id, lid) in all {
            if !valid.contains(&lid) {
                // still in use in graineo (own assignments – the Lightroom ones were removed above – or a person
                // with faces): keep it as an own keyword, only the link to Lightroom goes
                let used: bool = tx
                    .query_row("SELECT EXISTS(SELECT 1 FROM image_keyword WHERE keyword=?1) OR EXISTS(SELECT 1 FROM face WHERE person=?1) OR EXISTS(SELECT 1 FROM keyword WHERE parent=?1)", [id], |r| r.get(0))
                    .map_err(error)?;
                // renamed or made a person in graineo since the last import: an own keyword now
                let changed = match old_keywords.get(&lid) {
                    Some(v) => {
                        let (n, p): (String, bool) = tx.query_row("SELECT name, person FROM keyword WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?;
                        v.get(0).and_then(|x| x.as_str()) != Some(n.as_str()) || v.get(2).and_then(|x| x.as_bool()) != Some(p)
                    }
                    None => false,
                };
                if used || changed {
                    tx.execute("UPDATE keyword SET lr_id=NULL WHERE id=?1", [id]).map_err(error)?;
                } else {
                    tx.execute("DELETE FROM keyword WHERE id=?1", [id]).map_err(error)?;
                }
            }
        }
    }

    pause_point(usize::MAX / 2)?;
    // collections, collection sets, smart collections (only name + rule text)
    progress("Sammlungen übernehmen …");
    let mut collection_id: HashMap<i64, i64> = HashMap::new();
    // collections new in this import (all Lightroom assignments go in)
    let mut new_collections: HashSet<i64> = HashSet::new();
    let old_collections = state_load(&tx, "collection").map_err(error)?;
    let mut state_collections: Vec<(i64, serde_json::Value)> = Vec::with_capacity(collections.len());
    {
        let mut find = tx.prepare_cached("SELECT id, name, parent FROM collection WHERE lr_id = ?1").map_err(error)?;
        let mut new = tx.prepare_cached("INSERT INTO collection(name, parent, kind, rules, lr_id) VALUES(?1,?2,?3,?4,?5)").map_err(error)?;
        let mut upd = tx.prepare_cached("UPDATE collection SET name=?2, parent=?3, kind=CASE WHEN kind=3 THEN 3 ELSE ?4 END, rules=?5 WHERE id=?1").map_err(error)?;
        let mut smart = tx.prepare_cached("UPDATE collection SET search=?2, translatable=?3 WHERE id=?1").map_err(error)?;
        for (lid, name, parent_lr, kind, system) in &collections {
            let kind_n: i64 = match kind.as_str() {
                "com.adobe.ag.library.group" => 1,
                "com.adobe.ag.library.smart_collection" => 2,
                _ => 0,
            };
            let name = if name == "quick collection" && system.starts_with('1') { "Schnellsammlung".to_string() } else { name.clone() };
            state_collections.push((*lid, json!([name, parent_lr])));
            let parent = parent_lr.and_then(|p| collection_id.get(&p).copied());
            let rules = smart_rules.get(lid).cloned();
            let found: Option<(i64, String, Option<i64>)> = find.query_row([lid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?;
            let id = match found {
                Some((id, own_name, own_parent)) => {
                    // renamed or moved in graineo since the last import: stays so
                    let old = old_collections.get(lid);
                    let old_name = old.and_then(|v| Some(v.get(0)?.as_str()?.to_string()));
                    let old_parent = old.and_then(|v| v.get(1)).map(|p| p.as_i64().and_then(|p| collection_id.get(&p).copied()));
                    let (n, k1) = merge(own_name, old_name, name.clone());
                    let (p, k2) = merge(own_parent, old_parent, parent);
                    b.own_kept += usize::from(k1 || k2);
                    upd.execute(params![id, n, p, kind_n, rules]).map_err(error)?;
                    id
                }
                // existed at the last import and is gone: deleted in graineo – stays deleted
                None if *lid <= known => {
                    b.own_kept += 1;
                    continue;
                }
                None => {
                    new.execute(params![name, parent, kind_n, rules, lid]).map_err(error)?;
                    let id = tx.last_insert_rowid();
                    new_collections.insert(id);
                    id
                }
            };
            if kind_n == 2 {
                let s = rules.as_deref().and_then(crate::search::lightroom_rules);
                if s.is_some() {
                    b.smart_translated += 1;
                }
                smart.execute(params![id, s, s.is_some()]).map_err(error)?;
            }
            match kind_n {
                1 => b.collection_sets += 1,
                2 => b.smart_collections += 1,
                _ => b.collections += 1,
            }
            collection_id.insert(*lid, id);
        }
    }
    // the quick collection (kind 3) belongs to the app after the import: a new import leaves it untouched
    let own_items: HashSet<i64> = {
        let mut st = tx.prepare("SELECT id FROM collection WHERE kind=3").map_err(error)?;
        let ids: HashSet<i64> = st.query_map([], |r| r.get(0)).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        ids
    };
    // assignments: nothing is taken out wholesale any more. New in Lightroom → added; removed in graineo → stays
    // removed; removed in Lightroom → goes only if graineo still holds it as imported (same position)
    let old_items = state_load(&tx, "collection_image").map_err(error)?;
    let mut state_items: Vec<(i64, serde_json::Value)> = Vec::with_capacity(collection_image.len());
    {
        let mut get = tx.prepare_cached("SELECT position FROM collection_image WHERE collection=?1 AND image=?2").map_err(error)?;
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO collection_image(collection, image, position) VALUES(?1,?2,?3)").map_err(error)?;
        let mut set_pos = tx.prepare_cached("UPDATE collection_image SET position=?3 WHERE collection=?1 AND image=?2").map_err(error)?;
        let mut del = tx.prepare_cached("DELETE FROM collection_image WHERE collection=?1 AND image=?2").map_err(error)?;
        let same = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-9,
            (None, None) => true,
            _ => false,
        };
        let mut seen: HashSet<i64> = HashSet::with_capacity(collection_image.len());
        for (row, c, img, pos) in &collection_image {
            state_items.push((*row, json!([c, img, pos])));
            seen.insert(*row);
            let (Some(&sid), Some(&bid)) = (collection_id.get(c).filter(|s| !own_items.contains(s)), image_id.get(img)) else { continue };
            // what graineo holds now: Some(position) or None (not in the collection)
            let now: Option<Option<f64>> = get.query_row(params![sid, bid], |r| r.get(0)).optional().map_err(error)?;
            match (now, old_items.get(row)) {
                (Some(own), Some(old)) => {
                    // position: Lightroom's only if graineo kept the one of the last import
                    let old_pos = old.get(2).and_then(|v| v.as_f64());
                    if same(own, old_pos) && !same(own, *pos) {
                        set_pos.execute(params![sid, bid, pos]).map_err(error)?;
                    }
                    b.collection_assignments += 1;
                }
                (Some(_), None) => b.collection_assignments += 1,
                // existed at the last import and graineo does not hold it: taken out in graineo – stays out
                (None, Some(_)) => b.own_kept += 1,
                (None, None) if *row <= known && !new_collections.contains(&sid) && !new_images.contains(&bid) => b.own_kept += 1,
                (None, None) => b.collection_assignments += ins.execute(params![sid, bid, pos]).map_err(error)?,
            }
        }
        // removed in Lightroom since the last import (in collections that are still in Lightroom)
        for (row, old) in &old_items {
            if seen.contains(row) {
                continue;
            }
            let (Some(c), Some(img)) = (old.get(0).and_then(|v| v.as_i64()), old.get(1).and_then(|v| v.as_i64())) else { continue };
            let (Some(&sid), Some(&bid)) = (collection_id.get(&c).filter(|s| !own_items.contains(s)), image_id.get(&img)) else { continue };
            let now: Option<Option<f64>> = get.query_row(params![sid, bid], |r| r.get(0)).optional().map_err(error)?;
            if let Some(own) = now {
                if same(own, old.get(2).and_then(|v| v.as_f64())) {
                    del.execute(params![sid, bid]).map_err(error)?;
                } else {
                    b.own_kept += 1;
                }
            }
        }
    }
    state_save(&tx, "collection", &state_collections).map_err(error)?;
    state_save(&tx, "collection_image", &state_items).map_err(error)?;
    // collections deleted in Lightroom stay in graineo with all their photos, as own collections
    {
        let valid: HashSet<i64> = collections.iter().map(|c| c.0).collect();
        let mut st = tx.prepare("SELECT id, lr_id FROM collection WHERE lr_id IS NOT NULL").map_err(error)?;
        let all: Vec<(i64, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        for (id, lid) in all {
            if !valid.contains(&lid) {
                tx.execute("UPDATE collection SET lr_id=NULL WHERE id=?1", [id]).map_err(error)?;
            }
        }
    }
    pause_point(usize::MAX / 2)?;
    // faces and people (only boxes and names; Lightroom's own face features stay untouched)
    progress("Gesichter übernehmen …");
    match crate::people::import_lightroom_with(&tx, &lr_faces, &image_id, &keyword_id, known, &new_images) {
        Ok((all, named, kept)) => {
            (b.faces, b.faces_confirmed) = (all, named);
            b.own_kept += kept;
            let _ = db.set_info("faces_lr", crate::people::LR_REVISION);
        }
        Err(e) => b.conflicts.push(format!("Gesichter nicht übernommen: {e}")),
    }
    // everything up to here existed in the catalog at this import (see `horizon`)
    tx.execute(
        "INSERT INTO info(name, value) VALUES('lightroom_horizon', ?1) ON CONFLICT(name) DO UPDATE SET value=excluded.value",
        [new_horizon.max(known).to_string()],
    )
    .map_err(error)?;
    tx.commit().map_err(error)?;
    b.write_duration_ms = t1.elapsed().as_millis();
    db.set_info("lightroom_catalog", &catalog.display().to_string()).map_err(error)?;
    db.finish_run(run, b.new_images as i64, &b.json().to_string()).map_err(error)?;
    Ok(b)
}
