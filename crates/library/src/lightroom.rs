//! Import from a Lightroom Classic catalog (`.lrcat`, SQLite).
//!
//! The catalog is opened **strictly read-only** (`mode=ro` plus `PRAGMA query_only = 1`).
//! If the WAL file is empty or holds only its 32-byte header (Lightroom closed), additionally `immutable=1`: SQLite
//! then creates neither locks nor side files, and neither the catalog nor its folder is touched. If Lightroom is running and changes are in the WAL, reading goes without `immutable`
//! through the normal locks and the WAL – otherwise those changes would be missing, and reading
//! during a checkpoint would yield "database disk image is malformed". If that fails (e.g. an
//! orphaned WAL without `-shm`), fall back to `immutable`: then only the most recent changes are missing.
//!
//! Repeatable: everything is matched via `lr_id` (= `id_local` in the catalog) or the path;
//! a second run syncs instead of duplicating.
//!
//! **Own changes stay.** A new import adds and updates what comes from
//! Lightroom and never takes back what was added or changed in graineo:
//! - Values (stars, flag, label each on its own; caption, title, GPS position; names and places of collections and keywords;
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

/// Would giving `id` the parent `parent` close a loop in the hierarchy of `table` (`keyword` or `collection`)? Walks
/// up from `parent`; a loop already above it counts as one too (the walk never runs forever).
pub(crate) fn creates_cycle(conn: &Connection, table: &str, id: i64, parent: Option<i64>) -> rusqlite::Result<bool> {
    debug_assert!(table == "keyword" || table == "collection");
    let Some(mut p) = parent else { return Ok(false) };
    let mut st = conn.prepare_cached(&format!("SELECT parent FROM {table} WHERE id = ?1"))?;
    let mut seen = HashSet::new();
    loop {
        if p == id || !seen.insert(p) {
            return Ok(true);
        }
        match st.query_row([p], |r| r.get::<_, Option<i64>>(0)).optional()?.flatten() {
            Some(q) => p = q,
            None => return Ok(false),
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
    /// label texts without a known color (custom label sets of Lightroom; the catalog stores only the text): kept as
    /// text (`image.label_lr`, searchable), no color
    pub unknown_labels: BTreeMap<String, usize>,
    /// photos with a title / a GPS position after the import
    pub with_title: usize,
    pub with_position: usize,
    /// catalog format (`Adobe_DBVersion`)
    pub catalog_version: Option<i64>,
    /// tables or columns the catalog lacks (read as empty), and sections that could not be read
    pub catalog_gaps: Vec<String>,
    /// what the catalog has and graineo does not take over (synonyms, stacks)
    pub not_taken: NotTaken,
    /// rating changed in Lightroom and in graineo – the own value stays
    pub conflicts: Vec<String>,
    /// own changes the import left as they were although Lightroom has something else (values, places, names)
    /// or did not bring back (removed in graineo)
    pub own_kept: usize,
    /// parent changes from Lightroom left out because, together with own changes, they would have put a keyword or
    /// collection below itself (A under B in graineo, B under A in Lightroom); the own place stays (also in `own_kept`)
    pub cycles_avoided: usize,
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
    /// Newer catalog format than tested ([`TESTED_DB_VERSION`]): read all the same, with a note.
    pub fn newer_format(&self) -> bool {
        self.catalog_version.is_some_and(|v| v > TESTED_DB_VERSION)
    }

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
            "with_title": self.with_title,
            "with_position": self.with_position,
            "catalog_version": self.catalog_version,
            "newer_format": self.newer_format(),
            "catalog_gaps": self.catalog_gaps,
            "not_taken": {
                "synonyms": self.not_taken.synonyms,
                "stacks": self.not_taken.stacks,
                "stacked_photos": self.not_taken.stacked_photos,
                "unknown_labels": self.unknown_labels.values().sum::<usize>(),
            },
            "conflicts": self.conflicts,
            "own_kept": self.own_kept,
            "cycles_avoided": self.cycles_avoided,
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

/// Start of the error text when a catalog is not the one the library was imported from (see [`check_same_catalog`]).
pub const OTHER_CATALOG: &str = "Anderer Lightroom-Katalog";

/// Is `catalog` the catalog the library's Lightroom data came from? Lightroom numbers every catalog from the start,
/// so the ids (`lr_id`) of a second catalog collide with those of the first: importing it (or handing over its
/// edits) would relink root folders, files, ratings and settings of the first catalog to photos of the second.
/// Checked with what both know by id: a file, and the file of an image, has the name the catalog gives it (the
/// library's name or the one of the last import). The same catalog matches almost everywhere (also after an upgrade or a few renames), another one almost nowhere.
/// `files`: catalog file id → file name; `images`: catalog image id → file id. Reads only.
pub(crate) fn check_same_catalog(conn: &Connection, files: &HashMap<i64, String>, images: &HashMap<i64, i64>) -> Result<(), String> {
    let error = |e: rusqlite::Error| e.to_string();
    let fold = |t: &str| crate::nfc(t).to_lowercase();
    // names of the last import (the table is missing before the first import)
    let mut state_names: HashMap<i64, String> = HashMap::new();
    if let Ok(mut st) = conn.prepare("SELECT lr_id, value FROM lr_import_state WHERE kind = 'file'") {
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(error)?;
        for (id, v) in rows.flatten() {
            if let Some(n) = serde_json::from_str::<serde_json::Value>(&v).ok().and_then(|v| Some(v.get(1)?.as_str()?.to_string())) {
                state_names.insert(id, fold(&n));
            }
        }
    }
    let (mut overlap, mut matches) = (0usize, 0usize);
    {
        let mut st = conn.prepare("SELECT lr_id, name FROM file WHERE lr_id IS NOT NULL").map_err(error)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(error)?;
        for (id, name) in rows.flatten() {
            let Some(c) = files.get(&id).map(|c| fold(c)) else { continue };
            overlap += 1;
            matches += usize::from(fold(&name) == c || state_names.get(&id) == Some(&c));
        }
    }
    {
        // by name as well: the ids alone can line up in two catalogs built the same way
        let mut st = conn.prepare("SELECT i.lr_id, d.name, d.lr_id FROM image i JOIN file d ON d.id = i.file WHERE i.lr_id IS NOT NULL").map_err(error)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?))).map_err(error)?;
        for (image, name, file) in rows.flatten() {
            let Some(c) = images.get(&image).and_then(|f| files.get(f)).map(|c| fold(c)) else { continue };
            overlap += 1;
            matches += usize::from(fold(&name) == c || file.and_then(|f| state_names.get(&f)) == Some(&c));
        }
    }
    if overlap > 0 && matches * 5 < overlap {
        return Err(format!(
            "{OTHER_CATALOG}: die Bibliothek wurde aus einem anderen Katalog übernommen ({} von {overlap} Einträgen passen nicht) – ein zweiter Katalog würde Fotos vertauschen, nichts geändert",
            overlap - matches
        ));
    }
    Ok(())
}

/// [`check_same_catalog`] for an open catalog `lr` (file names and image files read from it; what its schema lacks
/// is not compared). Reads only.
pub(crate) fn check_catalog(conn: &Connection, lr: &Connection, schema: &CatalogSchema) -> Result<(), String> {
    let error = |e: rusqlite::Error| e.to_string();
    let name = if schema.has("AgLibraryFile", "idx_filename") { "coalesce(nullif(idx_filename, ''), baseName || '.' || extension)" } else { "baseName || '.' || coalesce(extension, '')" };
    let files: HashMap<i64, String> = if schema.has("AgLibraryFile", "baseName") && schema.has("AgLibraryFile", "extension") {
        let mut st = lr.prepare(&format!("SELECT id_local, {name} FROM AgLibraryFile")).map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, text(r.get(1)?).unwrap_or_default()))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        r
    } else {
        HashMap::new()
    };
    let images: HashMap<i64, i64> = if schema.has("Adobe_images", "rootFile") {
        let mut st = lr.prepare("SELECT id_local, rootFile FROM Adobe_images WHERE rootFile IS NOT NULL").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        r
    } else {
        HashMap::new()
    };
    check_same_catalog(conn, &files, &images)
}

/// Size of a WAL file header: a WAL of at most this size holds no frames (no change that is not in the catalog yet).
pub const WAL_HEADER: u64 = 32;

/// The catalog's `-wal` file holds no changes (missing, empty, or only its header): reading the main file alone is
/// complete, so it can be opened `immutable` (no locks, no side files).
pub fn wal_without_changes(catalog: &Path) -> bool {
    let mut wal = catalog.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(&wal).map_or(true, |m| m.len() <= WAL_HEADER)
}

/// Lightroom seems to have this catalog open: a lock file next to it, or changes waiting in the WAL. Only file
/// metadata is read.
pub fn catalog_in_use(catalog: &Path) -> bool {
    let mut lock = catalog.as_os_str().to_owned();
    lock.push(".lock");
    Path::new(&lock).exists() || !wal_without_changes(catalog)
}

/// Lightroom catalogs (`*.lrcat`) in `dir` and its direct subfolders, newest first. Only lists folders; nothing is
/// opened. Backups (`Backups` folders) and Lightroom's own packages (`.lrdata`) are skipped.
pub fn find_catalogs(dir: &Path) -> Vec<std::path::PathBuf> {
    let is_catalog = |p: &Path| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lrcat"));
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.starts_with('.') {
            continue;
        }
        if is_catalog(&p) {
            found.push((e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p));
        } else if p.is_dir() && name != "backups" && !name.ends_with(".lrdata") {
            for f in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let q = f.path();
                if is_catalog(&q) && !f.file_name().to_string_lossy().starts_with('.') {
                    found.push((f.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), q));
                }
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, p)| p).collect()
}

/// Open the catalog read-only (see module docs).
pub fn open_catalog(path: &Path) -> rusqlite::Result<Connection> {
    let uri = file_uri(path);
    let wal_empty = wal_without_changes(path);
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

/// Seconds between 2001-01-01 (Lightroom's time base) and 1970-01-01.
const COCOA_EPOCH: i64 = 978_307_200;

/// Lightroom's own develop history of an image of the catalog (`Adobe_images.id_local`): name and time (Unix seconds)
/// of every step, oldest first – shown beside graineo's history, never applied. Snapshots are not part of it. Empty if
/// the catalog has no such table.
pub fn develop_history(conn: &Connection, image: i64) -> rusqlite::Result<Vec<(String, i64)>> {
    let has: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'Adobe_libraryImageDevelopHistoryStep'", [], |r| r.get(0))?;
    if has == 0 {
        return Ok(Vec::new());
    }
    let mut st = conn.prepare_cached("SELECT coalesce(name, ''), dateCreated FROM Adobe_libraryImageDevelopHistoryStep WHERE image = ?1 ORDER BY dateCreated, id_local")?;
    let rows = st.query_map([image], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?.map_or(0, |d| d as i64 + COCOA_EPOCH))))?;
    rows.collect()
}

/// `Adobe_DBVersion` of the catalogs this import was tested with: Lightroom Classic 13 up to 15.1 all write 1500000
/// (no new or dropped table or column between them). A higher value is read all the same, with a note.
pub const TESTED_DB_VERSION: i64 = 1_500_000;

/// Catalog format (`Adobe_DBVersion` in `Adobe_variablesTable`; the highest value if it is listed twice, as in
/// catalogs upgraded over the years). `None` if the table or the value is missing.
pub fn catalog_version(conn: &Connection) -> Option<i64> {
    conn.query_row("SELECT max(CAST(value AS REAL)) FROM Adobe_variablesTable WHERE name = 'Adobe_DBVersion'", [], |r| r.get::<_, Option<f64>>(0))
        .ok()
        .flatten()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v as i64)
}

/// Tables and columns of a catalog (`PRAGMA table_info`). The import builds its queries from them: a table or column
/// that another Lightroom version lacks is read as NULL (or the section is skipped) instead of failing with "no such
/// column"; what was missing is collected for the report.
pub struct CatalogSchema {
    /// table (lower case) → columns (lower case)
    tables: HashMap<String, HashSet<String>>,
    missing: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

impl CatalogSchema {
    pub fn read(conn: &Connection) -> rusqlite::Result<CatalogSchema> {
        let names: Vec<String> = {
            let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            v
        };
        let mut tables = HashMap::new();
        let mut st = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        for t in names {
            let cols: HashSet<String> = st.query_map([&t], |r| r.get::<_, String>(0))?.filter_map(|c| c.ok()).map(|c| c.to_lowercase()).collect();
            tables.insert(t.to_lowercase(), cols);
        }
        Ok(CatalogSchema { tables, missing: Default::default() })
    }

    pub fn has_table(&self, table: &str) -> bool {
        self.tables.contains_key(&table.to_lowercase())
    }

    pub fn has(&self, table: &str, column: &str) -> bool {
        self.tables.get(&table.to_lowercase()).is_some_and(|c| c.contains(&column.to_lowercase()))
    }

    /// The table is there; otherwise it is noted as missing.
    pub fn table(&self, table: &str) -> bool {
        let ok = self.has_table(table);
        if !ok {
            self.missing.borrow_mut().insert(table.to_string());
        }
        ok
    }

    /// `alias.column` if the column is there, otherwise `NULL` (noted as missing).
    pub fn col(&self, table: &str, alias: &str, column: &str) -> String {
        if self.has(table, column) {
            format!("{alias}.{column}")
        } else {
            self.missing.borrow_mut().insert(if self.has_table(table) { format!("{table}.{column}") } else { table.to_string() });
            "NULL".to_string()
        }
    }

    /// Columns without which a section cannot be read: `Err` naming what is missing (also noted).
    pub fn require(&self, table: &str, columns: &[&str]) -> Result<(), String> {
        if !self.table(table) {
            return Err(table.to_string());
        }
        let gone: Vec<String> = columns.iter().filter(|c| !self.has(table, c)).map(|c| format!("{table}.{c}")).collect();
        if gone.is_empty() {
            return Ok(());
        }
        self.missing.borrow_mut().extend(gone.iter().cloned());
        Err(gone.join(", "))
    }

    /// Tables and columns found missing so far (sorted).
    pub fn missing(&self) -> Vec<String> {
        self.missing.borrow().iter().cloned().collect()
    }
}

/// Largest XMP packet of one image read from a catalog (unpacked). Lightroom's packets are a few kilobytes.
const CATALOG_XMP_MAX: u64 = 4 << 20;

/// The XMP packet Lightroom keeps per image (`Adobe_AdditionalMetadata.xmp`): a 4-byte length and zlib data, in old
/// catalogs plain text. A packet that unpacks to more than [`CATALOG_XMP_MAX`] bytes is skipped (a damaged or
/// crafted catalog must not exhaust the memory with a few kilobytes of zlib data).
fn catalog_xmp(v: rusqlite::types::Value) -> Option<String> {
    use std::io::Read;
    match v {
        rusqlite::types::Value::Text(t) => Some(t).filter(|t| t.len() as u64 <= CATALOG_XMP_MAX),
        rusqlite::types::Value::Blob(b) if b.first() == Some(&b'<') => String::from_utf8(b).ok().filter(|t| t.len() as u64 <= CATALOG_XMP_MAX),
        rusqlite::types::Value::Blob(b) if b.len() > 4 => {
            let mut raw = Vec::new();
            // one byte more than allowed: tells "exactly at the limit" from "too large"
            flate2::read::ZlibDecoder::new(&b[4..]).take(CATALOG_XMP_MAX + 1).read_to_end(&mut raw).ok()?;
            if raw.len() as u64 > CATALOG_XMP_MAX {
                return None;
            }
            String::from_utf8(raw).ok()
        }
        _ => None,
    }
}

/// Titles of the catalog's images (from their XMP packets; Lightroom keeps the title nowhere else).
fn read_titles(lr: &Connection, schema: &CatalogSchema) -> HashMap<i64, String> {
    if schema.require("Adobe_AdditionalMetadata", &["image", "xmp"]).is_err() {
        return HashMap::new();
    }
    let mut out = HashMap::new();
    let Ok(mut st) = lr.prepare("SELECT image, xmp FROM Adobe_AdditionalMetadata WHERE image IS NOT NULL") else { return out };
    let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, rusqlite::types::Value>(1)?))) else { return out };
    for (image, xmp) in rows.flatten() {
        if let Some(t) = catalog_xmp(xmp).filter(|t| t.contains("dc:title")).and_then(|t| crate::xmp::title(&t)) {
            out.insert(image, t);
        }
    }
    out
}

/// GPS position from Lightroom's EXIF values; `None` without a usable position.
fn position(has: Option<f64>, lat: Option<f64>, lon: Option<f64>) -> Option<(f64, f64)> {
    let (lat, lon) = (lat?, lon?);
    (has.is_none_or(|h| h > 0.0) && lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0)).then_some((lat, lon))
}

/// What a Lightroom catalog has that graineo does not take over (counted for the report).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NotTaken {
    /// keyword synonyms
    pub synonyms: usize,
    /// stacks of different photos (Lightroom also stacks virtual copies with their original: those are not counted)
    pub stacks: usize,
    pub stacked_photos: usize,
}

impl NotTaken {
    fn read(lr: &Connection, schema: &CatalogSchema) -> NotTaken {
        let count = |sql: &str| -> (usize, usize) { lr.query_row(sql, [], |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as usize, r.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as usize))).unwrap_or((0, 0)) };
        let mut n = NotTaken::default();
        if schema.has_table("AgLibraryKeywordSynonym") {
            n.synonyms = count("SELECT count(*), 0 FROM AgLibraryKeywordSynonym").0;
        }
        if schema.has("AgLibraryFolderStackImage", "stack") && schema.has("AgLibraryFolderStackImage", "image") && schema.has("Adobe_images", "rootFile") {
            (n.stacks, n.stacked_photos) = count(
                "SELECT count(*), sum(n) FROM (SELECT count(*) AS n, count(DISTINCT i.rootFile) AS files FROM AgLibraryFolderStackImage s
                 JOIN Adobe_images i ON i.id_local = s.image GROUP BY s.stack HAVING files > 1)",
            );
        }
        n
    }
}

/// Which parts the catalog could be read for (see `import_with`): a part it lacks leaves the library's values alone.
struct Known {
    /// stars, flag and label (`rating`, `pick`, `colorLabels`)
    ratings: bool,
    caption: bool,
    title: bool,
    position: bool,
    keywords: bool,
    keyword_images: bool,
    collections: bool,
    collection_images: bool,
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
    /// GPS position (latitude, longitude in degrees)
    position: Option<(f64, f64)>,
    title: Option<String>,
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

/// What an import would bring, read without writing anything ("Prüfen" in the Lightroom dialog).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Preview {
    /// photos in the catalog (with virtual copies)
    pub images: usize,
    /// of these, not in the library yet
    pub new_images: usize,
    pub virtual_copies: usize,
    pub collections: usize,
    pub collection_sets: usize,
    pub smart_collections: usize,
    pub keywords: usize,
    /// photos (originals and virtual copies) Lightroom marks as edited (`hasDevelopAdjustmentsEx`): their edits come
    /// along. Counts everything the catalog marks; the handover ([`crate::lr_handover::Report::edited`]) counts only
    /// the photos whose file is in the library and whose settings arrived, so it shows fewer when files are missing.
    pub edited: usize,
    pub roots: usize,
    /// root folders whose place cannot be reached right now (drive not connected): paths
    pub roots_unreachable: Vec<String>,
    /// root folders on a network path (`\\\\server\\share`): not checked, because looking at them would already
    /// connect to that server (a foreign catalog could name any server)
    pub roots_network: Vec<String>,
    /// catalog format (`Adobe_DBVersion`)
    pub catalog_version: Option<i64>,
    /// tables or columns the catalog lacks (read as empty)
    pub catalog_gaps: Vec<String>,
}

impl Preview {
    /// Newer catalog format than tested ([`TESTED_DB_VERSION`]): read all the same, with a note.
    pub fn newer_format(&self) -> bool {
        self.catalog_version.is_some_and(|v| v > TESTED_DB_VERSION)
    }

    pub fn json(&self) -> serde_json::Value {
        json!({
            "images": self.images,
            "new_images": self.new_images,
            "virtual_copies": self.virtual_copies,
            "collections": self.collections,
            "collection_sets": self.collection_sets,
            "smart_collections": self.smart_collections,
            "keywords": self.keywords,
            "edited": self.edited,
            "roots": self.roots,
            "roots_unreachable": self.roots_unreachable,
            "roots_network": self.roots_network,
            "catalog_version": self.catalog_version,
            "newer_format": self.newer_format(),
            "catalog_gaps": self.catalog_gaps,
        })
    }
}

/// Read the catalog (read-only, see [`open_catalog`]) and compare it with the library without writing to either.
pub fn preview(db: &Db, catalog: &Path) -> Result<Preview, String> {
    let lr = open_catalog(catalog).map_err(|e| format!("Katalog nicht lesbar: {e}"))?;
    let error = |e: rusqlite::Error| format!("Katalog nicht lesbar: {e}");
    let schema = CatalogSchema::read(&lr).map_err(error)?;
    schema.require("Adobe_images", &["id_local"]).map_err(|m| format!("Katalog nicht lesbar: {m} fehlt"))?;
    let count = |sql: &str| -> Result<usize, String> { lr.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|n| n.max(0) as usize).map_err(error) };
    // a section whose table or columns are missing counts 0 (named in `catalog_gaps`)
    let count_if = |table: &str, columns: &[&str], sql: &str| -> Result<usize, String> {
        match schema.require(table, columns) {
            Ok(()) => count(sql),
            Err(_) => Ok(0),
        }
    };
    let system_only = schema.col("AgLibraryCollection", "c", "systemOnly");
    let mut p = Preview {
        images: count("SELECT count(*) FROM Adobe_images")?,
        virtual_copies: count_if("Adobe_images", &["masterImage"], "SELECT count(*) FROM Adobe_images WHERE masterImage IS NOT NULL")?,
        collections: count_if(
            "AgLibraryCollection",
            &["creationId", "name"],
            &format!("SELECT count(*) FROM AgLibraryCollection c WHERE creationId = 'com.adobe.ag.library.collection' AND NOT (name = 'quick collection' AND coalesce({system_only}, '') LIKE '1%')"),
        )?,
        collection_sets: count_if("AgLibraryCollection", &["creationId"], "SELECT count(*) FROM AgLibraryCollection WHERE creationId = 'com.adobe.ag.library.group'")?,
        smart_collections: count_if("AgLibraryCollection", &["creationId"], "SELECT count(*) FROM AgLibraryCollection WHERE creationId = 'com.adobe.ag.library.smart_collection'")?,
        keywords: count_if("AgLibraryKeyword", &["parent"], "SELECT count(*) FROM AgLibraryKeyword WHERE parent IS NOT NULL")?,
        // older or trimmed catalogs may lack the column: then nothing counts as edited
        edited: count_if(
            "Adobe_imageDevelopSettings",
            &["image", "hasDevelopAdjustmentsEx"],
            "SELECT count(DISTINCT d.image) FROM Adobe_imageDevelopSettings d JOIN Adobe_images i ON i.id_local = d.image WHERE CAST(coalesce(d.hasDevelopAdjustmentsEx, 0) AS REAL) > 0",
        )
        .unwrap_or(0),
        catalog_version: catalog_version(&lr),
        ..Default::default()
    };
    let roots: Vec<String> = if schema.require("AgLibraryRootFolder", &["absolutePath"]).is_ok() {
        let mut st = lr.prepare("SELECT absolutePath FROM AgLibraryRootFolder WHERE absolutePath IS NOT NULL").map_err(error)?;
        let r = st.query_map([], |r| r.get::<_, String>(0)).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    } else {
        Vec::new()
    };
    p.roots = roots.len();
    // a network path is never touched here: on Windows a mere `is_dir` connects to the server and sends the login
    let (network, local): (Vec<String>, Vec<String>) = roots.into_iter().partition(|r| sysdirs::is_network_path(r));
    p.roots_network = network;
    p.roots_unreachable = local.into_iter().filter(|r| !Path::new(r).is_dir()).collect();
    // another catalog than the one already imported: the check says so (the import refuses it as well)
    check_catalog(&db.conn, &lr, &schema)?;
    // new: neither linked in the library nor removed there since the last import (those stay removed)
    let known: HashSet<i64> = {
        let mut st = db.conn.prepare("SELECT lr_id FROM image WHERE lr_id IS NOT NULL").map_err(|e| e.to_string())?;
        let r = st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?.collect::<rusqlite::Result<HashSet<i64>>>().map_err(|e| e.to_string())?;
        r
    };
    let horizon = horizon(&db.conn);
    let mut st = lr.prepare("SELECT id_local FROM Adobe_images").map_err(error)?;
    let ids = st.query_map([], |r| r.get::<_, i64>(0)).map_err(error)?;
    for id in ids {
        let id = id.map_err(error)?;
        if !known.contains(&id) && id > horizon {
            p.new_images += 1;
        }
    }
    p.catalog_gaps = schema.missing();
    Ok(p)
}

/// Choices of an import.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// collections new at the top level of the catalog go into a collection set of this name (found or created at
    /// the top level); `None`: at the top level as in Lightroom
    pub collection_set: Option<String>,
}

/// Run the import. `progress` receives short status messages.
pub fn import(db: &Db, catalog: &Path, progress: &dyn Fn(&str)) -> Result<Report, String> {
    import_with(db, catalog, &Options::default(), progress)
}

/// [`import`] with choices.
pub fn import_with(db: &Db, catalog: &Path, opts: &Options, progress: &dyn Fn(&str)) -> Result<Report, String> {
    let error = |e: rusqlite::Error| e.to_string();
    let mut b = Report { catalog: catalog.display().to_string(), ..Default::default() };
    if crate::move_job::active() {
        return Err("Ein Verschieben läuft oder ist angehalten – Lightroom-Import erst, wenn es fertig oder beendet ist".into());
    }
    let t0 = Instant::now();
    progress("Lightroom-Katalog lesen …");
    let lr = open_catalog(catalog).map_err(|e| format!("Katalog nicht lesbar: {e}"))?;

    // ── Reading ── (queries built from the catalog's own tables and columns: what another Lightroom version lacks
    // reads as NULL or the section stays empty, named in the report; only the core tables are required)
    let schema = CatalogSchema::read(&lr).map_err(error)?;
    b.catalog_version = catalog_version(&lr);
    let core = |table: &str, columns: &[&str]| schema.require(table, columns).map_err(|m| format!("Katalog nicht lesbar: {m} fehlt"));
    core("AgLibraryRootFolder", &["id_local", "absolutePath"])?;
    core("AgLibraryFolder", &["id_local", "rootFolder", "pathFromRoot"])?;
    core("AgLibraryFile", &["id_local", "folder", "baseName"])?;
    core("Adobe_images", &["id_local", "rootFile"])?;
    let rows = |sql: &str| lr.prepare(sql).map_err(error);
    let root_rows: Vec<(i64, String, String)> = {
        let mut st = rows(&format!("SELECT r.id_local, r.absolutePath, {} FROM AgLibraryRootFolder r WHERE r.absolutePath IS NOT NULL", schema.col("AgLibraryRootFolder", "r", "name")))?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, text(r.get(2)?).unwrap_or_default()))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let folder: Vec<(i64, i64, String)> = {
        let mut st = rows("SELECT id_local, rootFolder, coalesce(pathFromRoot, '') FROM AgLibraryFolder WHERE rootFolder IS NOT NULL")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    };
    let files: Vec<LrFile> = {
        let (idx, ext, side) = (schema.col("AgLibraryFile", "f", "idx_filename"), schema.col("AgLibraryFile", "f", "extension"), schema.col("AgLibraryFile", "f", "sidecarExtensions"));
        let mut st = rows(&format!("SELECT f.id_local, f.folder, {idx}, f.baseName, {ext}, {side} FROM AgLibraryFile f WHERE f.folder IS NOT NULL"))?;
        let r = st
            .query_map([], |r| {
                let idx = text(r.get(2)?);
                let base = text(r.get(3)?).unwrap_or_default();
                let ext = text(r.get(4)?).unwrap_or_default();
                let name = idx.filter(|s| !s.is_empty()).unwrap_or_else(|| if ext.is_empty() { base.clone() } else { format!("{base}.{ext}") });
                Ok(LrFile { id: r.get(0)?, folder: r.get(1)?, name: crate::nfc(&name), sidecar: text(r.get(5)?).filter(|s| !s.is_empty()) })
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        r
    };
    let titles = read_titles(&lr, &schema);
    let images: Vec<LrImage> = {
        let i = |c: &str| schema.col("Adobe_images", "i", c);
        // shooting data: without the table (or a column) the values stay empty
        let exif = schema.table("AgHarvestedExifMetadata") && schema.has("AgHarvestedExifMetadata", "image");
        let e = |c: &str| if exif { schema.col("AgHarvestedExifMetadata", "e", c) } else { "NULL".to_string() };
        let interned = |table: &str, reference: &str| exif && schema.has("AgHarvestedExifMetadata", reference) && schema.require(table, &["id_local", "value"]).is_ok();
        let (camera_join, camera) = if interned("AgInternedExifCameraModel", "cameraModelRef") { ("LEFT JOIN AgInternedExifCameraModel cm ON cm.id_local = e.cameraModelRef", "cm.value") } else { ("", "NULL") };
        let (lens_join, lens) = if interned("AgInternedExifLens", "lensRef") { ("LEFT JOIN AgInternedExifLens l ON l.id_local = e.lensRef", "l.value") } else { ("", "NULL") };
        let exif_join = if exif { "LEFT JOIN AgHarvestedExifMetadata e ON e.image = i.id_local" } else { "" };
        // locations, creator, copyright: not present in every (older or trimmed) catalog
        let has = |t: &str| schema.has_table(t);
        let place = if ["AgHarvestedIptcMetadata", "AgInternedIptcLocation", "AgInternedIptcCity", "AgInternedIptcState", "AgInternedIptcCountry"].iter().all(|t| has(t))
            && ["locationRef", "cityRef", "stateRef", "countryRef", "image"].iter().all(|c| schema.has("AgHarvestedIptcMetadata", c))
        {
            "(SELECT trim(coalesce(loc.value,'') || ' ' || coalesce(city.value,'') || ' ' || coalesce(st.value,'') || ' ' || coalesce(co.value,''))
                           FROM AgHarvestedIptcMetadata h
                           LEFT JOIN AgInternedIptcLocation loc ON loc.id_local = h.locationRef
                           LEFT JOIN AgInternedIptcCity city ON city.id_local = h.cityRef
                           LEFT JOIN AgInternedIptcState st ON st.id_local = h.stateRef
                           LEFT JOIN AgInternedIptcCountry co ON co.id_local = h.countryRef
                          WHERE h.image = i.id_local LIMIT 1)"
                .to_string()
        } else {
            "NULL".to_string()
        };
        let iptc = schema.table("AgLibraryIPTC") && schema.has("AgLibraryIPTC", "image");
        let creator = if iptc {
            format!(
                "(SELECT trim(coalesce({},'') || ' ' || coalesce({},'')) FROM AgLibraryIPTC t WHERE t.image = i.id_local LIMIT 1)",
                schema.col("AgLibraryIPTC", "t", "altTextAccessibility"),
                schema.col("AgLibraryIPTC", "t", "extDescrAccessibility")
            )
        } else {
            "NULL".to_string()
        };
        let caption = if iptc && schema.col("AgLibraryIPTC", "t", "caption") != "NULL" {
            "(SELECT caption FROM AgLibraryIPTC t WHERE t.image = i.id_local AND t.caption <> '' LIMIT 1)".to_string()
        } else {
            "NULL".to_string()
        };
        let mut st = rows(&format!(
            "SELECT i.id_local, i.rootFile, {}, {}, {}, {}, {}, {}, {},
                    {}, {}, {camera}, {},
                    {}, {}, {}, {}, {lens},
                    {caption}, {},
                    {place}, {creator},
                    {}, {}, {}
             FROM Adobe_images i
             {exif_join} {camera_join} {lens_join}",
            i("captureTime"),
            i("rating"),
            i("pick"),
            i("colorLabels"),
            i("orientation"),
            i("masterImage"),
            i("copyName"),
            i("fileWidth"),
            i("fileHeight"),
            i("fileFormat"),
            e("aperture"),
            e("focalLength"),
            e("isoSpeedRating"),
            e("shutterSpeed"),
            i("touchTime"),
            e("hasGPS"),
            e("gpsLatitude"),
            e("gpsLongitude"),
        ))?;
        let r = st
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                Ok(LrImage {
                    id,
                    file: r.get(1)?,
                    capture: time(text(r.get(2)?)),
                    stars: number(r.get(3)?).unwrap_or(0.0).clamp(0.0, 5.0) as u8,
                    flag: number(r.get(4)?).unwrap_or(0.0).clamp(-1.0, 1.0) as i8,
                    label_text: text(r.get(5)?).unwrap_or_default(),
                    orientation: text(r.get(6)?),
                    master: number(r.get(7)?).map(|v| v as i64),
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
                    position: position(number(r.get(22)?), number(r.get(23)?), number(r.get(24)?)),
                    title: titles.get(&id).cloned(),
                })
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        r
    };
    // (id, name, parent, genealogy, type, includeOnExport)
    #[allow(clippy::type_complexity)]
    let keywords: Vec<(i64, Option<String>, Option<i64>, Option<String>, Option<String>, Option<i64>)> = if schema.require("AgLibraryKeyword", &["id_local", "name"]).is_ok() {
        let k = |c: &str| schema.col("AgLibraryKeyword", "k", c);
        let (parent, genealogy) = (k("parent"), k("genealogy"));
        let include = if schema.has("AgLibraryKeyword", "includeOnExport") { "k.includeOnExport".to_string() } else { "NULL".to_string() };
        let mut st = rows(&format!("SELECT k.id_local, k.name, {parent}, {genealogy}, {}, {include} FROM AgLibraryKeyword k ORDER BY length({genealogy}), k.id_local", k("keywordType")))?;
        let r = st
            .query_map([], |r| Ok((r.get(0)?, text(r.get(1)?), number(r.get(2)?).map(|v| v as i64), text(r.get(3)?), text(r.get(4)?), number(r.get(5)?).map(|v| v as i64))))
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        r
    } else {
        Vec::new()
    };
    let keyword_image: Vec<(i64, i64)> = if schema.require("AgLibraryKeywordImage", &["image", "tag"]).is_ok() {
        let mut st = rows("SELECT image, tag FROM AgLibraryKeywordImage WHERE image IS NOT NULL AND tag IS NOT NULL")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    } else {
        Vec::new()
    };
    let collections: Vec<(i64, String, Option<i64>, String, String)> = if schema.require("AgLibraryCollection", &["id_local", "name", "creationId"]).is_ok() {
        let c = |col: &str| schema.col("AgLibraryCollection", "c", col);
        let mut st = rows(&format!(
            "SELECT c.id_local, c.name, {}, c.creationId, {} FROM AgLibraryCollection c
             WHERE c.creationId IN ('com.adobe.ag.library.collection','com.adobe.ag.library.group','com.adobe.ag.library.smart_collection')
             ORDER BY length({}), c.id_local",
            c("parent"),
            c("systemOnly"),
            c("genealogy")
        ))?;
        let r = st
            .query_map([], |r| Ok((r.get(0)?, text(r.get(1)?).unwrap_or_default(), number(r.get(2)?).map(|v| v as i64), r.get(3)?, text(r.get(4)?).unwrap_or_default())))
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        r
    } else {
        Vec::new()
    };
    // (row id, collection, image, position)
    let collection_image: Vec<(i64, i64, i64, Option<f64>)> = if schema.require("AgLibraryCollectionImage", &["id_local", "collection", "image"]).is_ok() {
        let mut st = rows(&format!(
            "SELECT c.id_local, c.collection, c.image, {} FROM AgLibraryCollectionImage c WHERE c.collection IS NOT NULL AND c.image IS NOT NULL",
            schema.col("AgLibraryCollectionImage", "c", "positionInCollection")
        ))?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, number(r.get(3)?)))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
        r
    } else {
        Vec::new()
    };
    let smart_rules: HashMap<i64, String> = if schema.require("AgLibraryCollectionContent", &["collection", "content", "owningModule"]).is_ok() {
        let mut st = rows("SELECT collection, content FROM AgLibraryCollectionContent WHERE owningModule = 'ag.library.smart_collection' AND collection IS NOT NULL")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, text(r.get(1)?).unwrap_or_default()))).map_err(error)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(error)?;
        r
    } else {
        HashMap::new()
    };
    // faces: only from a catalog that has both face tables (without them, names or boxes would read as removed)
    let lr_faces: Option<Vec<_>> = if schema.has_table("AgLibraryFace") && schema.has_table("AgLibraryKeywordFace") {
        match crate::people::read_lightroom(&lr) {
            Ok(f) => Some(f),
            Err(e) => {
                b.catalog_gaps.push(format!("Gesichter: {e}"));
                None
            }
        }
    } else {
        None
    };
    // What the catalog could not say (a table or column missing in another Lightroom version or a trimmed catalog) is
    // "not known", never "removed in Lightroom": those values, assignments and their stored state stay as they are.
    let readable = Known {
        ratings: ["rating", "pick", "colorLabels"].iter().all(|c| schema.has("Adobe_images", c)),
        caption: schema.has("AgLibraryIPTC", "caption") && schema.has("AgLibraryIPTC", "image"),
        title: schema.has("Adobe_AdditionalMetadata", "image") && schema.has("Adobe_AdditionalMetadata", "xmp"),
        position: schema.has("AgHarvestedExifMetadata", "image") && schema.has("AgHarvestedExifMetadata", "gpsLatitude") && schema.has("AgHarvestedExifMetadata", "gpsLongitude"),
        keywords: schema.has("AgLibraryKeyword", "id_local") && schema.has("AgLibraryKeyword", "name"),
        keyword_images: schema.has("AgLibraryKeywordImage", "image") && schema.has("AgLibraryKeywordImage", "tag"),
        collections: ["id_local", "name", "creationId"].iter().all(|c| schema.has("AgLibraryCollection", c)),
        collection_images: ["id_local", "collection", "image"].iter().all(|c| schema.has("AgLibraryCollectionImage", c)),
    };
    b.not_taken = NotTaken::read(&lr, &schema);
    b.catalog_gaps.extend(schema.missing());
    drop(lr);
    // never over the data of another catalog (its ids collide with the ones of the library)
    check_same_catalog(&db.conn, &files.iter().map(|f| (f.id, f.name.clone())).collect(), &images.iter().map(|i| (i.id, i.file)).collect())?;
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
        lr_faces.iter().flatten().map(|r| r.0).max(),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0);

    // ── Writing (in blocks: the transaction is committed every few thousand rows and between the sections, so other
    // writers – develop settings, ratings, the sync – get their turn; the import is idempotent via `lr_id`) ──
    let t1 = Instant::now();
    progress(&format!("{} Fotos übernehmen …", images.len()));
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
    // title and position: only values are remembered (none in Lightroom = no row; the rule is the same)
    let old_titles = state_load(&tx, "title").map_err(error)?;
    let old_positions = state_load(&tx, "position").map_err(error)?;
    let mut state_titles: Vec<(i64, serde_json::Value)> = Vec::new();
    let mut state_positions: Vec<(i64, serde_json::Value)> = Vec::new();
    {
        type Found = (i64, i64, i64, i64, Option<String>, Option<String>);
        let found = |r: &rusqlite::Row| -> rusqlite::Result<Found> { Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)) };
        let mut find_lr = tx.prepare_cached("SELECT id, stars, flag, label, lr_state, caption FROM image WHERE lr_id = ?1").map_err(error)?;
        let mut find_orig = tx.prepare_cached("SELECT id, stars, flag, label, lr_state, caption FROM image WHERE file = ?1 AND copy IS NULL AND lr_id IS NULL").map_err(error)?;
        let mut new = tx
            .prepare_cached("INSERT INTO image(file, copy, stars, flag, label, label_lr, lr_id, lr_state, run) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")
            .map_err(error)?;
        // ?10: the catalog has no label column – the label text of the last import stays
        let mut upd = tx
            .prepare_cached("UPDATE image SET file=?2, copy=?3, stars=?4, flag=?5, label=?6, label_lr=CASE WHEN ?10 THEN label_lr ELSE ?7 END, lr_id=?8, lr_state=?9 WHERE id=?1")
            .map_err(error)?;
        let mut set_text = tx.prepare_cached("UPDATE image SET caption=?2, lr_text=?3 WHERE id=?1").map_err(error)?;
        let mut get_meta = tx.prepare_cached("SELECT title, latitude, longitude FROM image_meta WHERE image=?1").map_err(error)?;
        let mut set_meta = tx
            .prepare_cached("INSERT INTO image_meta(image, title, latitude, longitude) VALUES(?1,?2,?3,?4) ON CONFLICT(image) DO UPDATE SET title=excluded.title, latitude=excluded.latitude, longitude=excluded.longitude")
            .map_err(error)?;
        let mut clear_meta = tx.prepare_cached("DELETE FROM image_meta WHERE image=?1").map_err(error)?;
        let mut set_time = tx
            .prepare_cached("UPDATE image SET lr_touch=?2, rating_time=CASE WHEN ?3 AND (rating_time IS NULL OR changed IS NULL) THEN ?2 ELSE rating_time END, keywords_time=CASE WHEN keywords_changed IS NULL THEN ?2 ELSE keywords_time END WHERE id=?1")
            .map_err(error)?;
        let mut path_of = tx
            .prepare_cached("SELECT s.path || o.path || d.name FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE d.id=?1")
            .map_err(error)?;
        let names = ["Sterne", "Markierung", "Etikett"];
        for i in &images {
            // a part the catalog cannot say keeps the state of the last import
            if readable.caption {
                state_captions.push((i.id, json!(i.image_text)));
            } else if let Some(v) = old_captions.get(&i.id) {
                state_captions.push((i.id, v.clone()));
            }
            if !readable.title {
                if let Some(v) = old_titles.get(&i.id) {
                    state_titles.push((i.id, v.clone()));
                }
            } else if let Some(t) = &i.title {
                state_titles.push((i.id, json!(t)));
            }
            if !readable.position {
                if let Some(v) = old_positions.get(&i.id) {
                    state_positions.push((i.id, v.clone()));
                }
            } else if let Some((lat, lon)) = i.position {
                state_positions.push((i.id, json!([lat, lon])));
            }
            let Some(&did) = file_id.get(&i.file) else { continue };
            // a label text of a custom label set: the catalog does not say which color it stands for, so no color is
            // guessed; the text stays (`label_lr`, shown in Info) and becomes searchable (`lr_text`)
            let (label, unknown_label) = match label::from_lightroom(&i.label_text) {
                Some(e) => (e, None),
                None => {
                    *b.unknown_labels.entry(i.label_text.clone()).or_default() += 1;
                    (0, Some(i.label_text.trim().to_string()))
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
                // the catalog cannot say: the own values stay
                Some((_, s, m, e, ..)) if !readable.ratings => [*s, *m, *e],
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
            if existing.is_some() && values != lr_values && readable.ratings {
                b.own_kept += 1;
            }
            // without the rating columns the state of the last import stays (it is what Lightroom had then)
            let state: Option<String> = match &existing {
                Some((.., old_state, _)) if !readable.ratings => old_state.clone(),
                _ => Some(state),
            };
            // caption: Lightroom's only if graineo still has the one of the last import (or none)
            let caption = match &existing {
                None => i.image_text.clone(),
                Some((.., own)) if !readable.caption => own.clone(),
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
            // title and GPS position: the same rule as the caption
            let existing_id = existing.as_ref().map(|e| e.0);
            type Meta = (Option<String>, Option<f64>, Option<f64>);
            let own_meta: Option<Meta> = match existing_id {
                Some(eid) => get_meta.query_row([eid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?,
                None => None,
            };
            let title = match existing_id {
                None => i.title.clone(),
                Some(_) if !readable.title => own_meta.as_ref().and_then(|m| m.0.clone()),
                Some(_) => {
                    let own = own_meta.as_ref().and_then(|m| m.0.clone()).filter(|t| !t.trim().is_empty());
                    let old = old_titles.get(&i.id).map(|v| v.as_str().map(str::to_string));
                    match old {
                        None if own.is_none() => i.title.clone(),
                        old => {
                            let (t, kept) = merge(own, old, i.title.clone());
                            b.own_kept += usize::from(kept);
                            t
                        }
                    }
                }
            };
            let position = match existing_id {
                None => i.position,
                Some(_) if !readable.position => own_meta.as_ref().and_then(|m| m.1.zip(m.2)),
                Some(_) => {
                    let own = own_meta.as_ref().and_then(|m| m.1.zip(m.2));
                    let old = old_positions.get(&i.id).map(|v| v.get(0).and_then(|a| a.as_f64()).zip(v.get(1).and_then(|o| o.as_f64())));
                    match old {
                        None if own.is_none() => i.position,
                        old => {
                            let (p, kept) = merge(own, old, i.position);
                            b.own_kept += usize::from(kept);
                            p
                        }
                    }
                }
            };
            let id = match existing {
                Some((id, ..)) => {
                    upd.execute(params![id, did, copy, values[0], values[1], values[2], label_lr, i.id, state, !readable.ratings]).map_err(error)?;
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
            let search_text: Vec<&str> = [i.extra_text.as_deref(), title.as_deref(), unknown_label.as_deref()].into_iter().flatten().filter(|t| !t.is_empty()).collect();
            let search_text = (!search_text.is_empty()).then(|| search_text.join(" "));
            set_text.execute(params![id, caption, search_text]).map_err(error)?;
            let meta_now = (title.clone(), position.map(|p| p.0), position.map(|p| p.1));
            if title.is_some() || position.is_some() {
                if own_meta.as_ref() != Some(&meta_now) {
                    set_meta.execute(params![id, meta_now.0, meta_now.1, meta_now.2]).map_err(error)?;
                }
                b.with_title += usize::from(title.is_some());
                b.with_position += usize::from(position.is_some());
            } else if own_meta.is_some() {
                clear_meta.execute([id]).map_err(error)?;
            }
            // timestamp of the values: if they come from Lightroom, Lightroom's modification time applies
            set_time.execute(params![id, i.touch, values == lr_values]).map_err(error)?;
            image_id.insert(i.id, id);
            pause_point(1)?;
        }
    }
    b.images = image_id.len();
    state_save(&tx, "caption", &state_captions).map_err(error)?;
    state_save(&tx, "title", &state_titles).map_err(error)?;
    state_save(&tx, "position", &state_positions).map_err(error)?;
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
        for (lid, name, parent_lr, _gen, type_id, _include) in &keywords {
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
                    let (mut p, mut k2) = merge(own_parent, old_parent, parent);
                    if p != own_parent && creates_cycle(&tx, "keyword", id, p).map_err(error)? {
                        p = own_parent;
                        k2 = true;
                        b.cycles_avoided += 1;
                    }
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
    // "include on export" = 0 in Lightroom (e.g. a branch of AI keywords): marked "never export" here too. Only
    // added, never taken away (a mark set or cleared in graineo stays)
    {
        let mut mark = tx.prepare_cached("INSERT OR IGNORE INTO keyword_option(keyword, export) VALUES(?1, 0)").map_err(error)?;
        for (lid, _, _, _, _, include) in &keywords {
            if *include == Some(0) {
                if let Some(id) = keyword_id.get(lid) {
                    mark.execute([id]).map_err(error)?;
                }
            }
        }
    }
    if readable.keywords {
        state_save(&tx, "keyword", &state_keywords).map_err(error)?;
    } else {
        // the catalog has no keywords to say: the library's keywords from Lightroom stay, with their assignments
        // (the faces below still find their people by them)
        let mut st = tx.prepare("SELECT lr_id, id FROM keyword WHERE lr_id IS NOT NULL").map_err(error)?;
        keyword_id = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
    }
    pause_point(usize::MAX / 2)?;
    // replace keywords from Lightroom – except for images whose keywords were changed
    // in graineo (the own version stays; it is also in the sidecar)
    let own_kw: HashSet<i64> = {
        let mut st = tx.prepare("SELECT id FROM image WHERE keywords_changed IS NOT NULL").map_err(error)?;
        let r = st.query_map([], |r| r.get(0)).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        r
    };
    if readable.keywords && readable.keyword_images {
        tx.execute("DELETE FROM image_keyword WHERE keyword IN (SELECT id FROM keyword WHERE lr_id IS NOT NULL) AND image NOT IN (SELECT id FROM image WHERE keywords_changed IS NOT NULL)", []).map_err(error)?;
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
    if readable.keywords {
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
    // the set for collections at the top level of the catalog: the one remembered for this name (also after moving or
    // renaming it in graineo), else one of this name, else created at the top level
    let top_set: Option<i64> = match opts.collection_set.as_deref().map(str::trim).filter(|n| !n.is_empty() && readable.collections) {
        Some(name) => Some(crate::collection_order::find_or_create_set(&tx, name).map_err(error)?),
        None => None,
    };
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
            // Lightroom's quick collection stays at the top level: there it becomes graineo's quick collection
            let quick = name == "quick collection" && system.starts_with('1');
            let name = if quick { "Schnellsammlung".to_string() } else { name.clone() };
            state_collections.push((*lid, json!([name, parent_lr])));
            let parent = parent_lr.and_then(|p| collection_id.get(&p).copied());
            let rules = smart_rules.get(lid).cloned();
            let found: Option<(i64, String, Option<i64>)> = find.query_row([lid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?;
            let id = match found {
                Some((id, own_name, own_parent)) => {
                    // renamed or moved in graineo since the last import: stays so
                    let old = old_collections.get(lid);
                    let old_name = old.and_then(|v| Some(v.get(0)?.as_str()?.to_string()));
                    // a top-level collection that sits in the chosen set counts as unmoved (the set stands for the top)
                    let top = |p: Option<i64>| p.or(top_set.filter(|t| own_parent == Some(*t)));
                    let parent = top(parent);
                    let old_parent = old.and_then(|v| v.get(1)).map(|p| top(p.as_i64().and_then(|p| collection_id.get(&p).copied())));
                    let (n, k1) = merge(own_name, old_name, name.clone());
                    let (mut p, mut k2) = merge(own_parent, old_parent, parent);
                    if p != own_parent && creates_cycle(&tx, "collection", id, p).map_err(error)? {
                        p = own_parent;
                        k2 = true;
                        b.cycles_avoided += 1;
                    }
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
                    new.execute(params![name, if quick { parent } else { parent.or(top_set) }, kind_n, rules, lid]).map_err(error)?;
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
    // only if the catalog can say which photos its collections hold (otherwise all would read as taken out)
    if readable.collections && readable.collection_images {
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
    if readable.collections {
        state_save(&tx, "collection", &state_collections).map_err(error)?;
    }
    if readable.collections && readable.collection_images {
        state_save(&tx, "collection_image", &state_items).map_err(error)?;
    }
    // collections deleted in Lightroom stay in graineo with all their photos, as own collections
    if readable.collections {
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
    // without readable face tables the faces of earlier imports stay as they are
    match lr_faces.as_ref().map(|f| crate::people::import_lightroom_with(&tx, f, &image_id, &keyword_id, known, &new_images)) {
        None => {}
        Some(Ok((all, named, kept))) => {
            (b.faces, b.faces_confirmed) = (all, named);
            b.own_kept += kept;
            let _ = db.set_info("faces_lr", crate::people::LR_REVISION);
        }
        Some(Err(e)) => b.conflicts.push(format!("Gesichter nicht übernommen: {e}")),
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

#[cfg(test)]
mod xmp_packet_tests {
    use super::*;
    use std::io::Write;

    fn packed(text: &[u8]) -> rusqlite::types::Value {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(text).unwrap();
        let mut b = (text.len() as u32).to_be_bytes().to_vec();
        b.extend(e.finish().unwrap());
        rusqlite::types::Value::Blob(b)
    }

    #[test]
    fn packed_packet_is_read() {
        let x = "<x:xmpmeta><dc:title><rdf:Alt><rdf:li>Bergsee</rdf:li></rdf:Alt></dc:title></x:xmpmeta>";
        assert_eq!(catalog_xmp(packed(x.as_bytes())).as_deref(), Some(x));
    }

    /// A zlib bomb (a few kilobytes that unpack to far more than any XMP packet) is skipped, not unpacked into memory.
    #[test]
    fn oversized_packet_is_skipped() {
        let bomb = packed(&vec![b' '; (CATALOG_XMP_MAX as usize) * 4]);
        if let rusqlite::types::Value::Blob(b) = &bomb {
            assert!(b.len() < 100_000, "test data should be small packed: {}", b.len());
        }
        assert_eq!(catalog_xmp(bomb), None);
        assert!(catalog_xmp(packed(&vec![b' '; CATALOG_XMP_MAX as usize])).is_some());
        assert_eq!(catalog_xmp(rusqlite::types::Value::Text(" ".repeat(CATALOG_XMP_MAX as usize + 1))), None);
    }
}
