//! Own library database (SQLite, WAL).
//!
//! Principles:
//! - The database is an index and store for ratings; the photos stay where they are
//!   (path = root folder + folder path + file name, no copies, no renaming).
//! - Lightroom origin is remembered via `lr_id` (= `id_local` in the catalog) so that a repeated
//!   import syncs instead of duplicating.
//! - The file lives locally (not on the NAS): `library.sqlite` in Application Support.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

pub type Result<T> = rusqlite::Result<T>;

/// Is the call running on the main thread (UI)?
fn main_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: pure query without side effects
        unsafe { libc::pthread_main_np() == 1 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::thread::current().name() == Some("main")
    }
}

/// Current schema version (`PRAGMA user_version`). 20: English names (graineo 1.0 had German ones up to 19, see
/// `legacy_names::sql`).
pub const VERSION: i32 = 20;
/// Oldest schema version that may open this library safely (stored in `info.schema_compatible`). 20: versions
/// before the switch to English names must not open it (graineo 1.0 refuses it, it cannot read the marker).
pub const SCHEMA_COMPATIBLE: i32 = 20;

/// The whole schema. Every statement is `IF NOT EXISTS`: a new library gets everything at once, an existing one
/// whatever a newer version added without a version step (older versions ignore it).
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS root_folder (
    id        INTEGER PRIMARY KEY,
    path      TEXT NOT NULL UNIQUE,      -- absolute, with '/' at the end
    name      TEXT NOT NULL,
    origin    TEXT NOT NULL,             -- 'lightroom' | 'own' | 'apple-photos'
    lr_id     INTEGER UNIQUE
);
CREATE TABLE IF NOT EXISTS folder (
    id        INTEGER PRIMARY KEY,
    root      INTEGER NOT NULL REFERENCES root_folder(id) ON DELETE CASCADE,
    path      TEXT NOT NULL,             -- relative to the root: '' or 'a/b/'
    lr_id     INTEGER UNIQUE,
    UNIQUE (root, path)
);
CREATE TABLE IF NOT EXISTS file (
    id           INTEGER PRIMARY KEY,
    folder       INTEGER NOT NULL REFERENCES folder(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,          -- file name with extension (NFC)
    format       TEXT NOT NULL,          -- X3F, CR3, CR2, RAF, DNG, ARW, JPG, HEIC, TIFF, PNG, PSD, VIDEO …
    size         INTEGER,                -- bytes (last sync)
    mtime        INTEGER,                -- Unix seconds (last sync)
    camera       TEXT,
    capture_time TEXT,                   -- 'YYYY-MM-DDThh:mm:ss' (local time as in EXIF)
    width        INTEGER,
    height       INTEGER,
    orientation  INTEGER,                -- EXIF 1–8
    status       INTEGER NOT NULL DEFAULT 0, -- 0 present, 1 missing, 2 volume missing, 4 deleted/hidden in Apple Photos (not shown)
    sidecars     TEXT,                   -- Lightroom sidecarExtensions, e.g. 'JPG,xmp'
    lr_id        INTEGER UNIQUE,
    lens         TEXT,
    aperture     REAL,                   -- f-number, e.g. 2.8
    focal_length REAL,                   -- mm
    iso          INTEGER,
    time         REAL,                   -- exposure time in seconds
    exif         INTEGER NOT NULL DEFAULT 0, -- 1 = shooting data read (catalog or file)
    partial_hash TEXT,                   -- FNV over the first and last 64 KB
    text_read    INTEGER NOT NULL DEFAULT 0, -- 1 = text fields of the file read
    textmeta     TEXT,                   -- title, description, keywords, location, creator from the file/XMP
    capture_time_original TEXT,          -- camera time before a correction (for restoring)
    xmp_read     INTEGER,                -- 1 = sidecar read once (NULL: not yet)
    sha256       TEXT,                   -- of the whole file (computed by a verified copy)
    UNIQUE (folder, name)
);
CREATE INDEX IF NOT EXISTS file_folder ON file(folder);
-- import: "already in the library?" (size + name or capture time) without reading the whole table
CREATE INDEX IF NOT EXISTS file_size ON file(size);
CREATE INDEX IF NOT EXISTS file_size_partial_hash ON file(size, partial_hash) WHERE partial_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS file_xmp_unread ON file(id) WHERE xmp_read IS NULL;
CREATE TABLE IF NOT EXISTS run (
    id        INTEGER PRIMARY KEY,
    kind      TEXT NOT NULL,             -- 'lightroom' | 'import' | 'folder' | 'root_folder' | 'sync'
    source    TEXT,
    started   INTEGER NOT NULL,
    finished  INTEGER,
    added     INTEGER NOT NULL DEFAULT 0,
    report    TEXT                       -- JSON
);
CREATE TABLE IF NOT EXISTS image (
    id         INTEGER PRIMARY KEY,
    file       INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
    copy       TEXT,                     -- NULL = original, else name of the virtual copy
    stars      INTEGER NOT NULL DEFAULT 0,   -- 0–5
    flag       INTEGER NOT NULL DEFAULT 0,   -- 1 picked, 0 none, -1 rejected
    label      INTEGER NOT NULL DEFAULT 0,   -- 0 none, 1 red, 2 yellow, 3 green, 4 blue, 5 purple
    label_lr   TEXT,                     -- original text of the Lightroom label
    lr_id      INTEGER UNIQUE,
    lr_state   TEXT,                     -- 'stars,flag,label' at the last Lightroom import
    run        INTEGER REFERENCES run(id),
    changed    INTEGER,                  -- Unix seconds of the last change in graineo
    caption    TEXT,                     -- caption (Lightroom IPTC caption)
    rating_time INTEGER,                 -- time of the rating (Unix s): Lightroom change, own change or sidecar
    keywords_time INTEGER,               -- time of the keywords
    keywords_changed INTEGER,            -- keywords changed in graineo (a Lightroom import does not replace them)
    lr_touch   INTEGER,                  -- last change in Lightroom (touchTime)
    lr_text    TEXT,                     -- location, creator, copyright, alt text from Lightroom
    faces_scanned INTEGER NOT NULL DEFAULT 0, -- 1 = searched for faces
    copyright  TEXT                      -- copyright given at the import
);
CREATE INDEX IF NOT EXISTS image_file ON image(file);
CREATE UNIQUE INDEX IF NOT EXISTS image_original ON image(file) WHERE copy IS NULL;
CREATE INDEX IF NOT EXISTS image_run ON image(run) WHERE run IS NOT NULL;
-- descriptive values of an image beside the rating (added without a version step: older versions ignore the table)
CREATE TABLE IF NOT EXISTS image_meta (
    image      INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
    title      TEXT,                     -- title (IPTC/XMP dc:title, from Lightroom)
    latitude   REAL,                     -- GPS position in degrees, north positive (from Lightroom)
    longitude  REAL                      -- east positive
);
CREATE TABLE IF NOT EXISTS keyword (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    parent  INTEGER REFERENCES keyword(id) ON DELETE CASCADE,
    person  INTEGER NOT NULL DEFAULT 0,
    lr_id   INTEGER UNIQUE
);
CREATE INDEX IF NOT EXISTS keyword_parent ON keyword(parent);
-- per-keyword options beside the tree (added without a version step: older versions ignore the table); no row =
-- defaults. export = 0: the keyword and everything below it are never written into exported files (Lightroom
-- "includeOnExport" = 0)
CREATE TABLE IF NOT EXISTS keyword_option (
    keyword   INTEGER PRIMARY KEY REFERENCES keyword(id) ON DELETE CASCADE,
    export    INTEGER NOT NULL DEFAULT 1,
    synonyms  TEXT
);
CREATE TABLE IF NOT EXISTS image_keyword (
    image     INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    keyword   INTEGER NOT NULL REFERENCES keyword(id) ON DELETE CASCADE,
    PRIMARY KEY (image, keyword)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS image_keyword_k ON image_keyword(keyword, image);
CREATE TABLE IF NOT EXISTS collection (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    parent  INTEGER REFERENCES collection(id) ON DELETE CASCADE,
    kind    INTEGER NOT NULL,            -- 0 collection, 1 collection set, 2 smart collection, 3 quick collection
    rules   TEXT,                        -- smart collection: Lightroom rules (Lua text), not evaluated yet
    lr_id   INTEGER UNIQUE,
    search  TEXT,                        -- smart collection: search query (own syntax)
    translatable INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS collection_image (
    collection INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    image      INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    position   REAL,
    PRIMARY KEY (collection, image)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS collection_image_i ON collection_image(image);
-- own order of the collections in the left panel (`crate::collection_order`; added without a version step: older
-- versions ignore the table). A position counts only while `parent` equals the collection's parent.
CREATE TABLE IF NOT EXISTS collection_order (
    collection INTEGER PRIMARY KEY REFERENCES collection(id) ON DELETE CASCADE,
    parent     INTEGER,                  -- parent the position was given in (NULL = top level)
    position   INTEGER NOT NULL          -- 1, 2, 3 … within that parent
);
CREATE TABLE IF NOT EXISTS info (
    name    TEXT PRIMARY KEY,
    value   TEXT
);
CREATE TABLE IF NOT EXISTS ai (
    id          INTEGER PRIMARY KEY,
    file        INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
    source      TEXT NOT NULL,            -- 'imported' (taken over before 1.0.1)
    model       TEXT,
    keywords    TEXT,                     -- JSON list
    scene       TEXT,
    quality     INTEGER,                  -- 0 reject, 1 medium, 2 good
    technical   REAL,
    discard     REAL,
    reason      TEXT,
    nonsense    INTEGER NOT NULL DEFAULT 0,
    created     TEXT,
    UNIQUE (file, source)
);
-- image vectors (CLIP) as a separate, regenerable cache
CREATE TABLE IF NOT EXISTS image_vector (file INTEGER PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE, model TEXT NOT NULL, source TEXT NOT NULL, vec BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS text_vector (file INTEGER PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE, model TEXT NOT NULL, source TEXT NOT NULL, vec BLOB NOT NULL);
-- description vectors of further text models of the semantic search (the standard model keeps `text_vector`)
CREATE TABLE IF NOT EXISTS text_vector_model (file INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE, model TEXT NOT NULL, vec BLOB NOT NULL, PRIMARY KEY(file, model)) WITHOUT ROWID;
-- image vectors of further search models (SigLIP 2, `crate::search_model`), per model so a switch keeps them
CREATE TABLE IF NOT EXISTS image_vector_model (file INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE, model TEXT NOT NULL, vec BLOB NOT NULL, PRIMARY KEY(file, model)) WITHOUT ROWID;
-- Lightroom files removed in graineo (the import does not recreate them)
CREATE TABLE IF NOT EXISTS removed_lr_file (lr_id INTEGER PRIMARY KEY, path TEXT, time INTEGER);
-- people and faces (regenerable except for names and confirmations, which are also in the sidecars)
CREATE TABLE IF NOT EXISTS face (
    id        INTEGER PRIMARY KEY,
    image     INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL, -- normalized 0..1 on the upright image, origin top left
    landmarks BLOB,                  -- 5 face landmarks as 10 × f32 (normalized), if known
    embedding BLOB,                  -- feature vector (f32), model see `model`
    model     TEXT,
    quality   REAL,                  -- 0..1 (size, sharpness, gaze direction)
    source    TEXT NOT NULL,         -- 'lightroom' | 'graineo' | 'apple' | 'manual'
    lr_id     INTEGER UNIQUE,
    person    INTEGER REFERENCES keyword(id) ON DELETE SET NULL,
    status    TEXT NOT NULL DEFAULT 'unknown', -- 'confirmed' | 'suggested' | 'rejected' | 'unknown'
    confidence REAL,                 -- suggestion: similarity to the person
    cluster   INTEGER,               -- group of unnamed, similar faces
    local     INTEGER NOT NULL DEFAULT 0, -- 1 = confirmed/changed in graineo (a Lightroom import does not overwrite it)
    suspect   INTEGER NOT NULL DEFAULT 0, -- 1 = the name does not fit according to the face comparison (no example then)
    detected  INTEGER,               -- Unix seconds
    guessed   INTEGER REFERENCES keyword(id) ON DELETE SET NULL, -- suspicious name: whom the face resembles more
    score_own REAL,
    score_guessed REAL,
    apple_id  TEXT                   -- identifier of the face in Apple Photos (ZDETECTEDFACE.ZUUID)
);
CREATE INDEX IF NOT EXISTS face_image ON face(image);
CREATE INDEX IF NOT EXISTS face_person ON face(person, status);
CREATE UNIQUE INDEX IF NOT EXISTS face_apple ON face(apple_id);
-- names to review (`people::check_count`): only the few suspicious faces, without reading the whole table
CREATE INDEX IF NOT EXISTS face_suspect ON face(suspect, local) WHERE suspect=1;
-- faces of a group of unnamed faces without reading all unnamed faces per group
CREATE INDEX IF NOT EXISTS face_cluster ON face(cluster) WHERE cluster IS NOT NULL;
CREATE TABLE IF NOT EXISTS face_never (face INTEGER NOT NULL REFERENCES face(id) ON DELETE CASCADE, person INTEGER NOT NULL REFERENCES keyword(id) ON DELETE CASCADE, PRIMARY KEY (face, person)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS person_info (keyword INTEGER PRIMARY KEY REFERENCES keyword(id) ON DELETE CASCADE, hidden INTEGER NOT NULL DEFAULT 0, preview INTEGER);
-- sidecar writes waiting for an unreachable location (see `crate::location`)
CREATE TABLE IF NOT EXISTS pending_sidecar_write (
    file     INTEGER PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE,
    since    INTEGER NOT NULL, -- Unix seconds of the first waiting change
    baseline TEXT              -- JSON: sidecar values before the first waiting change (stars, flag, label, keywords, copies, time), if known
);
CREATE INDEX IF NOT EXISTS pending_sidecar_since ON pending_sidecar_write(since, file);
-- the library is the main store: develop settings and Lightroom data per photo/copy
CREATE TABLE IF NOT EXISTS develop_state (
    id             INTEGER PRIMARY KEY,
    file           INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
    copy           TEXT NOT NULL DEFAULT '', -- '' = original, else name of the virtual copy
    settings       TEXT,                     -- own develop settings (JSON, as graineo:DevelopSettings)
    settings_time  INTEGER,                  -- Unix seconds of the last change of `settings`
    lr_develop     TEXT,                     -- Lightroom settings text (as graineo:LightroomDevelop)
    lr_orientation TEXT,                     -- orientation in Lightroom (AB, BC …)
    lr_edited      INTEGER NOT NULL DEFAULT 0, -- Lightroom marks the image as edited
    lr_file        TEXT,                     -- name of the photo the Lightroom data belongs to
    UNIQUE (file, copy)
);
-- develop history per photo / virtual copy, one row per step (`crate::develop_history`; added without a version step:
-- older versions ignore the tables, the check on loading catches states they wrote without a step)
CREATE TABLE IF NOT EXISTS develop_step (
    id       INTEGER PRIMARY KEY,
    file     INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
    copy     TEXT NOT NULL DEFAULT '',  -- '' = original, else name of the virtual copy (as develop_state)
    seq      INTEGER NOT NULL,          -- 0 = start state, then ascending
    time     INTEGER NOT NULL,          -- Unix seconds (last change of a merged step)
    kind     TEXT NOT NULL,             -- 'start' | 'slider' | 'preset' | 'film' | 'paste' | 'sync' | 'auto' | 'reset' | 'crop'
                                        -- | 'mask' | 'retouch' | 'lightroom' | 'external' | 'version' | 'compacted'
    label    TEXT,                      -- JSON: key and values of the displayed name (translated when shown)
    patch    TEXT NOT NULL,             -- start/compacted: whole settings JSON; else patch from the step before
    count    INTEGER NOT NULL DEFAULT 1,-- merged steps
    command  INTEGER,                   -- id of a command for several photos
    UNIQUE (file, copy, seq)
);
-- where the cursor stands (only stored when it is not on the newest step)
CREATE TABLE IF NOT EXISTS develop_cursor (
    file   INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
    copy   TEXT NOT NULL DEFAULT '',
    seq    INTEGER NOT NULL,
    PRIMARY KEY (file, copy)
) WITHOUT ROWID;
-- folders and files removed from the library by the user (files untouched): the sync does not take them in
-- again; importing or adding them explicitly lifts it (`crate::manage`)
CREATE TABLE IF NOT EXISTS excluded_path (
    root INTEGER NOT NULL REFERENCES root_folder(id) ON DELETE CASCADE,
    rel  TEXT NOT NULL,      -- relative to the root: folder 'a/b/' or file 'a/b/x.jpg'
    time INTEGER NOT NULL,
    PRIMARY KEY (root, rel)
) WITHOUT ROWID;
-- files moved by "Move to …" in jobs (`crate::move_job`): the Lightroom import keeps their folder
CREATE TABLE IF NOT EXISTS moved_files (
    file     INTEGER PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE,
    job      TEXT NOT NULL,
    old_path TEXT NOT NULL,
    moved_at INTEGER NOT NULL
);
-- state of each folder at the last sync (`crate::reconcile`): modification time and number of entries; a quick
-- sync skips reading size and time of every file where both are unchanged. A pure cache.
CREATE TABLE IF NOT EXISTS folder_scan (
    root     INTEGER NOT NULL REFERENCES root_folder(id) ON DELETE CASCADE,
    rel      TEXT NOT NULL,
    mtime_ns INTEGER,
    entries  INTEGER NOT NULL,
    PRIMARY KEY (root, rel)
) WITHOUT ROWID;
-- search index (regenerable; missing e.g. in a backup)
CREATE VIRTUAL TABLE IF NOT EXISTS search_word USING fts5(text, tokenize = 'unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE IF NOT EXISTS search_trigram USING fts5(text, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS search_vocab USING fts5vocab(search_word, 'row');
"#;

/// Opened library.
pub struct Db {
    pub conn: Connection,
    pub path: PathBuf,
}

/// One row of the grid (everything grid and filters need; ~44,000 of them in memory).
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub image: i64,
    pub file: i64,
    pub folder: i64,
    pub name: String,
    pub format: String,
    pub copy: Option<String>,
    pub stars: u8,
    pub flag: i8,
    pub label: u8,
    pub status: u8,
    pub camera: Option<String>,
    /// 'YYYY-MM-DDThh:mm:ss'
    pub capture: Option<String>,
    pub run: Option<i64>,
    pub size: Option<i64>,
    pub mtime: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub lens: Option<String>,
    pub aperture: Option<f64>,
    pub focal: Option<f64>,
    pub iso: Option<i64>,
    pub time: Option<f64>,
    /// AI quality 0 reject, 1 medium, 2 good
    pub ai_quality: Option<u8>,
    /// last change (own or in Lightroom), Unix seconds
    pub changed: Option<i64>,
    /// time the image was added to the library (import run), Unix seconds
    pub imported: Option<i64>,
    /// file comes from the Lightroom catalog (`lr_id`)
    pub from_lightroom: bool,
    /// RAW with a camera JPEG next to it (image ID of the JPEG), see [`mark_pairs`]
    pub sidecar_jpeg: Option<i64>,
    /// camera JPEG of a RAW (image ID of the RAW) – not shown separately in the stack
    pub jpeg_for: Option<i64>,
}

/// Detect RAW+JPEG pairs: same folder, same name (without extension, case-insensitive); the JPEG
/// was never an image of its own in the Lightroom catalog (so e.g. every X3F pair). Pairs that Lightroom
/// already showed separately (both with `lr_id`) stay separate. Originals only, no copies.
pub fn mark_pairs(rows: &mut [Row]) {
    let root = |n: &str| n.rsplit_once('.').map_or(n, |(s, _)| s).to_lowercase();
    let mut raws: HashMap<(i64, String), usize> = HashMap::new();
    for (i, z) in rows.iter().enumerate() {
        if z.copy.is_none() && crate::is_raw(&z.format) {
            raws.insert((z.folder, root(&z.name)), i);
        }
    }
    let mut pairs = Vec::new();
    for (i, z) in rows.iter().enumerate() {
        if z.copy.is_none() && z.format == "JPG" && !z.from_lightroom {
            if let Some(&r) = raws.get(&(z.folder, root(&z.name))) {
                pairs.push((r, i));
            }
        }
    }
    for (r, j) in pairs {
        let (rb, jb) = (rows[r].image, rows[j].image);
        rows[r].sidecar_jpeg = Some(jb);
        rows[j].jpeg_for = Some(rb);
    }
}

impl Row {
    pub fn year(&self) -> Option<u16> {
        self.capture.as_deref().and_then(|a| a.get(0..4)).and_then(|j| j.parse().ok())
    }
    pub fn month(&self) -> Option<u8> {
        self.capture.as_deref().and_then(|a| a.get(5..7)).and_then(|j| j.parse().ok())
    }
    pub fn day(&self) -> Option<u8> {
        self.capture.as_deref().and_then(|a| a.get(8..10)).and_then(|j| j.parse().ok()).filter(|d| (1..=31).contains(d))
    }
}

#[derive(Clone, Debug)]
pub struct Root {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub origin: String,
}

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: i64,
    pub root: i64,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
    pub kind: u8,
    pub count: usize,
    /// smart collection: search query in own syntax (translated or saved by the app)
    pub search: Option<String>,
    pub translatable: bool,
    /// Lightroom smart collection of the plug-in "Find Duplicates 2" – replaced by "Duplikate"
    pub duplicates: bool,
    /// own place within its level (`crate::collection_order`), `None` = not placed (behind the placed ones)
    pub position: Option<i64>,
    /// mirrors Apple Photos (the set "Apple Fotos" and its albums, `crate::apple_photos`)
    pub apple: bool,
}

#[derive(Clone, Debug)]
pub struct Keyword {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
    /// images with exactly this keyword
    pub count: usize,
    /// name of a person (face recognition)
    pub person: bool,
}

/// AI description (table `ai`).
#[derive(Clone, Debug, Default)]
pub struct Ai {
    pub source: String,
    pub model: Option<String>,
    pub keywords: Vec<String>,
    pub scene: Option<String>,
    pub quality: Option<u8>,
    pub technical: Option<f64>,
    pub delete: Option<f64>,
    pub reason: Option<String>,
    pub nonsense: bool,
}

/// Result of a run (import/sync) – short, for the UI.
#[derive(Clone, Debug)]
pub struct Run {
    pub id: i64,
    pub kind: String,
    pub begin: i64,
    pub end: Option<i64>,
    pub new: i64,
    pub report: Option<String>,
}

static MIGRATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Step of a running schema migration, for the start screen (the migration runs on a helper thread).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStep {
    /// consistent copy of the library (`VACUUM INTO`)
    Backup,
    /// tables, columns and values get their new names
    Convert,
    /// integrity check and row counts before the commit
    Check,
}

/// Current step and rough progress (0 … 1) of a running migration; `None` when none is running.
static PROGRESS: std::sync::Mutex<Option<(MigrationStep, f32, std::time::Instant)>> = std::sync::Mutex::new(None);

fn progress(step: MigrationStep, fraction: f32) {
    let mut g = PROGRESS.lock().unwrap_or_else(|e| e.into_inner());
    let t0 = g.map(|(_, _, t)| t).unwrap_or_else(std::time::Instant::now);
    eprintln!("library migration: {step:?} {:.0} % after {} ms", fraction * 100.0, t0.elapsed().as_millis());
    *g = Some((step, fraction, t0));
}

fn progress_end() {
    *PROGRESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Step and progress (0 … 1) of the migration running right now (see [`MigrationStep`]).
pub fn migration_progress() -> Option<(MigrationStep, f32)> {
    PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).map(|(s, f, _)| (s, f))
}

/// Does opening this library start a migration (an existing library with an older schema)? Reads only; the schema
/// it has now is returned. Not a file yet, unreadable or current: `None` (then [`Db::open`] has nothing slow to do).
pub fn migration_needed(path: &Path) -> Option<i32> {
    if !path.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).ok()?;
    (1..VERSION).contains(&v).then_some(v)
}

/// Where [`backup_before_migration`] puts the backup of this library.
pub fn migration_backup_path(path: &Path) -> PathBuf {
    path.parent().unwrap_or(Path::new(".")).join("backups").join(format!("library-before-schema-{VERSION}.sqlite"))
}

/// Back the library up before a schema migration (consistent snapshot via `VACUUM INTO`), once per target version:
/// `backups/library-before-schema-<VERSION>.sqlite` next to the database. An existing backup is kept.
fn backup_before_migration(conn: &Connection, path: &Path, from: i32) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
    let target = dir.join(format!("library-before-schema-{VERSION}.sqlite"));
    progress(MigrationStep::Backup, 0.02);
    if target.exists() {
        return Ok(());
    }
    let fail = |e: String| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN), Some(format!("Sicherung vor der Umstellung der Bibliothek fehlgeschlagen – nichts verändert: {e}")));
    std::fs::create_dir_all(&dir).map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    let tmp = dir.join(format!(".library-before-schema-{VERSION}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let t = std::time::Instant::now();
    conn.execute("VACUUM INTO ?1", [tmp.to_string_lossy()]).map_err(|e| fail(e.to_string()))?;
    std::fs::rename(&tmp, &target).map_err(|e| fail(e.to_string()))?;
    progress(MigrationStep::Backup, 0.2);
    eprintln!("library backed up before migration {from} → {VERSION}: {} ({} ms)", target.display(), t.elapsed().as_millis());
    Ok(())
}

#[cfg(test)]
static FAIL_MIGRATION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Rows per table (tables without rows count 0); names as given.
fn row_counts(conn: &Connection, tables: &[&str]) -> Result<Vec<i64>> {
    tables.iter().map(|t| conn.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get::<_, i64>(0))).collect()
}

fn migration_error(msg: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT), Some(msg))
}

/// Schema 1 … 19 (graineo 1.0 and older, German names) → 20 (English names), in one transaction:
/// 1. bring the library to schema 19 exactly as graineo 1.0 did ([`legacy_names::sql::upgrade_to_19`]),
/// 2. rename tables and columns, drop the three columns unused since schema 16, translate stored values and `info`
///    keys ([`legacy_names::sql::rename_to_english`]),
/// 3. rebuild `face` (its default status was German), translate the run reports,
/// 4. create the current schema (indexes under their new names, the search vocabulary view),
/// 5. check: `integrity_check`, `foreign_key_check` and the number of rows of every table before and after.
///
/// Any error rolls everything back: the library stays as it was (backed up before, see [`backup_before_migration`]).
/// Foreign keys are off during the migration (dropping the old face table must not cascade); they are checked
/// before the commit.
fn migrate_from_legacy(conn: Connection, path: &Path, from: i32) -> Result<Db> {
    use legacy_names::sql as old;
    let t = std::time::Instant::now();
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let r = (|| -> Result<Vec<(String, i64)>> {
        progress(MigrationStep::Convert, 0.25);
        let tx = conn.unchecked_transaction()?;
        old::upgrade_to_19(&conn, from)?;
        progress(MigrationStep::Convert, 0.35);
        let same: Vec<&str> = ["info", "person_info", "develop_state", "pending_sidecar_write", "moved_files", "folder_scan", "excluded_path"].to_vec();
        let old_names: Vec<&str> = old::TABLES.iter().map(|(o, _)| *o).chain(same.iter().copied()).collect();
        let new_names: Vec<&str> = old::TABLES.iter().map(|(_, n)| *n).chain(same.iter().copied()).collect();
        let before = row_counts(&conn, &old_names)?;
        progress(MigrationStep::Convert, 0.45);
        old::rename_to_english(&conn)?;
        progress(MigrationStep::Convert, 0.6);
        rebuild_face(&conn)?;
        progress(MigrationStep::Convert, 0.75);
        // run reports: keys of the sync and Lightroom reports
        {
            let mut st = conn.prepare("SELECT id, report FROM run WHERE report IS NOT NULL")?;
            let rows: Vec<(i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_>>()?;
            for (id, report) in rows {
                if let std::borrow::Cow::Owned(new) = legacy_names::rename_keys_in_text(&report, old::REPORT_KEYS) {
                    conn.execute("UPDATE run SET report = ?2 WHERE id = ?1", params![id, new])?;
                }
            }
        }
        #[cfg(test)]
        if FAIL_MIGRATION.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(migration_error("test: migration interrupted".into()));
        }
        progress(MigrationStep::Convert, 0.82);
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", VERSION)?;
        conn.execute("INSERT INTO info(name, value) VALUES('schema_compatible', ?1) ON CONFLICT(name) DO UPDATE SET value=excluded.value", [SCHEMA_COMPATIBLE.to_string()])?;
        progress(MigrationStep::Check, 0.88);
        let after = row_counts(&conn, &new_names)?;
        let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if ok != "ok" {
            return Err(migration_error(format!("integrity_check: {ok}")));
        }
        progress(MigrationStep::Check, 0.95);
        let fk: i64 = conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
        if fk != 0 {
            return Err(migration_error(format!("foreign_key_check: {fk} rows")));
        }
        let mut counts = Vec::new();
        for ((name, b), a) in new_names.iter().zip(&before).zip(&after) {
            if b != a {
                return Err(migration_error(format!("{name}: {b} rows before, {a} after")));
            }
            counts.push((name.to_string(), *a));
        }
        tx.commit()?;
        Ok(counts)
    })();
    progress_end();
    conn.pragma_update(None, "foreign_keys", "ON")?;
    match r {
        Ok(counts) => {
            let total: i64 = counts.iter().map(|(_, n)| n).sum();
            eprintln!("library migrated {from} → {VERSION} (English names): {} tables, {total} rows unchanged, {} ms", counts.len(), t.elapsed().as_millis());
            let mut conn = conn;
            conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
            Ok(Db { conn, path: path.to_path_buf() })
        }
        Err(e) => {
            eprintln!("library migration {from} → {VERSION} failed, nothing changed: {e}");
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(format!("Umstellung der Bibliothek fehlgeschlagen – nichts verändert (Sicherung in „backups“): {e}")),
            ))
        }
    }
}

/// Rebuild `face` with the current definition (its default status was German up to schema 19). Foreign keys must
/// be off (dropping the old table must not cascade to `face_never`).
fn rebuild_face(conn: &Connection) -> Result<()> {
    let start = SCHEMA.find("CREATE TABLE IF NOT EXISTS face (").expect("face table in SCHEMA");
    let end = start + SCHEMA[start..].find(");").expect("end of the face table") + 2;
    let ddl = SCHEMA[start..end].replace("CREATE TABLE IF NOT EXISTS face (", "CREATE TABLE face_rebuilt (");
    let cols: Vec<String> = conn.prepare("SELECT name FROM pragma_table_info('face')")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
    let cols = cols.join(", ");
    conn.execute_batch(&format!("{ddl}; INSERT INTO face_rebuilt({cols}) SELECT {cols} FROM face; DROP TABLE face; ALTER TABLE face_rebuilt RENAME TO face;"))
}

impl Db {
    /// Opens an existing library read-only: no migration, no writing (e.g. for an export
    /// running next to the open app). An older library is rejected instead of rebuilt.
    pub fn read_only(path: &Path) -> Result<Db> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.pragma_update(None, "query_only", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v < VERSION {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(format!("Bibliothek hat ein älteres Schema ({v}) – erst mit graineo öffnen")),
            ));
        }
        Ok(Db { conn, path: path.to_path_buf() })
    }

    /// Opens (or creates) the database and brings the schema up to date.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let mut conn = Connection::open(path)?;
        // start write transactions immediately with a write lock: a deferred transaction that
        // first reads and then writes fails in WAL mode without waiting with "database is locked"
        // if another part (faces, sync, UI) has written in the meantime.
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // If another task is currently occupying the library: background work (deletion, import, sync, faces …)
        // waits patiently for up to two minutes instead of giving up with "database is locked". Only the UI (main thread)
        // waits briefly at most – it must never hang; its writes belong in the background.
        conn.busy_timeout(std::time::Duration::from_secs(if main_thread() { 3 } else { 120 }))?;
        let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > VERSION {
            // Newer library: usable if its changes only add (tables/columns).
            // For this it carries `info.schema_compatible` = oldest version that may read and write it.
            let compatible: Option<i32> = conn.query_row("SELECT value FROM info WHERE name='schema_compatible'", [], |r| r.get::<_, String>(0)).ok().and_then(|w| w.parse().ok());
            if compatible.is_none_or(|k| k > VERSION) {
                // own error message (formerly wrongly wrapped as "Invalid parameter name")
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("Bibliothek stammt aus einer neueren Version (Schema {v}) – wird nicht verändert")),
                ));
            }
            return Ok(Db { conn, path: path.to_path_buf() });
        }
        // An existing library is first backed up as it is (`backups/library-before-schema-<VERSION>.sqlite` next to
        // it), only then migrated; one migration at a time within the app (several threads open the library at start).
        let _one = MIGRATION.lock().unwrap_or_else(|e| e.into_inner());
        let v: i32 = if v < VERSION { conn.query_row("PRAGMA user_version", [], |r| r.get(0))? } else { v };
        if (1..VERSION).contains(&v) {
            progress(MigrationStep::Backup, 0.0);
            if let Err(e) = backup_before_migration(&conn, path, v) {
                progress_end();
                return Err(e);
            }
            return migrate_from_legacy(conn, path, v);
        }
        // Schema already current (every open after the first): a deferred transaction – the CREATE … IF NOT EXISTS
        // statements then only read, no write lock and no WAL frame per open (a counter opens the library often);
        // should something be missing, the transaction takes the write lock when it creates it.
        let current = v == VERSION;
        if current {
            conn.set_transaction_behavior(rusqlite::TransactionBehavior::Deferred);
        }
        let migration = conn.unchecked_transaction()?;
        conn.execute_batch(SCHEMA)?;
        if v < VERSION {
            conn.pragma_update(None, "user_version", VERSION)?;
        }
        let marker: Option<String> = conn.query_row("SELECT value FROM info WHERE name='schema_compatible'", [], |r| r.get(0)).optional()?;
        if !current || marker.as_deref() != Some(SCHEMA_COMPATIBLE.to_string().as_str()) {
            conn.execute("INSERT INTO info(name, value) VALUES('schema_compatible', ?1) ON CONFLICT(name) DO UPDATE SET value=excluded.value", [SCHEMA_COMPATIBLE.to_string()])?;
        }
        migration.commit()?;
        // later transactions of this connection: write lock right away (see above)
        let mut conn = conn;
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        Ok(Db { conn, path: path.to_path_buf() })
    }

    /// Default database (see [`crate::db_path`]).
    pub fn standard() -> Result<Db> {
        Self::open(&crate::db_path())
    }

    pub fn info(&self, cache_key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM info WHERE name=?1", [cache_key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_info(&self, cache_key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO info(name,value) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value", params![cache_key, value])?;
        Ok(())
    }

    pub fn image_count(&self) -> i64 {
        self.conn.query_row("SELECT count(*) FROM image", [], |r| r.get(0)).unwrap_or(0)
    }

    /// All images for grid and filters, sorted by capture time (like the default "capture time" in common
    /// photo apps), images without a date at the end, then by file name.
    pub fn rows(&self) -> Result<Vec<Row>> {
        let mut st = self.conn.prepare(
            "SELECT b.id, d.id, d.folder, d.name, d.format, b.copy, b.stars, b.flag, b.label, d.status,
                    d.camera, d.capture_time, b.run, d.size, d.mtime, d.width, d.height, d.orientation,
                    d.lens, d.aperture, d.focal_length, d.iso, d.time, k.quality,
                    max(coalesce(b.changed, 0), coalesce(b.keywords_changed, 0), coalesce(b.lr_touch, 0)), l.started,
                    d.lr_id IS NOT NULL
             FROM image b JOIN file d ON d.id = b.file
             LEFT JOIN ai k ON k.file = d.id AND k.source = 'imported' LEFT JOIN run l ON l.id = b.run
             WHERE d.status <> 4",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Row {
                image: r.get(0)?,
                file: r.get(1)?,
                folder: r.get(2)?,
                name: r.get(3)?,
                format: r.get(4)?,
                copy: r.get(5)?,
                stars: r.get::<_, i64>(6)?.clamp(0, 5) as u8,
                flag: r.get::<_, i64>(7)?.clamp(-1, 1) as i8,
                label: r.get::<_, i64>(8)?.clamp(0, 5) as u8,
                status: r.get::<_, i64>(9)? as u8,
                camera: r.get(10)?,
                capture: r.get(11)?,
                run: r.get(12)?,
                size: r.get(13)?,
                mtime: r.get(14)?,
                width: r.get(15)?,
                height: r.get(16)?,
                orientation: r.get(17)?,
                lens: r.get(18)?,
                aperture: r.get(19)?,
                focal: r.get(20)?,
                iso: r.get(21)?,
                time: r.get(22)?,
                ai_quality: r.get::<_, Option<i64>>(23)?.map(|q| q.clamp(0, 2) as u8),
                changed: r.get::<_, Option<i64>>(24)?.filter(|t| *t > 0),
                imported: r.get(25)?,
                from_lightroom: r.get(26)?,
                sidecar_jpeg: None,
                jpeg_for: None,
            })
        })?;
        let mut v: Vec<Row> = rows.collect::<Result<_>>()?;
        // sorted here instead of `ORDER BY`: SQLite sorted the wide rows in a temporary B-tree that spilled to disk
        // (0.5–0.7 s instead of 0.1 s for 71 000 photos). Same order: without a date at the end, then capture time, name,
        // original before its virtual copies (byte order, like SQLite's BINARY collation).
        v.sort_by(|a, b| {
            a.capture.is_none().cmp(&b.capture.is_none()).then_with(|| a.capture.cmp(&b.capture)).then_with(|| a.name.cmp(&b.name)).then_with(|| a.copy.cmp(&b.copy))
        });
        mark_pairs(&mut v);
        Ok(v)
    }

    pub fn roots(&self) -> Result<Vec<Root>> {
        let mut st = self.conn.prepare("SELECT id, path, name, origin FROM root_folder ORDER BY name COLLATE NOCASE")?;
        let rows = st.query_map([], |r| Ok(Root { id: r.get(0)?, path: r.get(1)?, name: r.get(2)?, origin: r.get(3)? }))?;
        rows.collect()
    }

    pub fn folder(&self) -> Result<Vec<Folder>> {
        let mut st = self.conn.prepare("SELECT id, root, path FROM folder ORDER BY root, path")?;
        let rows = st.query_map([], |r| Ok(Folder { id: r.get(0)?, root: r.get(1)?, path: r.get(2)? }))?;
        rows.collect()
    }

    pub fn collections(&self) -> Result<Vec<Collection>> {
        let mut st = self.conn.prepare(
            "SELECT s.id, s.name, s.parent, s.kind, (SELECT count(*) FROM collection_image sb JOIN image b ON b.id = sb.image JOIN file d ON d.id = b.file WHERE sb.collection = s.id AND d.status <> 4), s.search, s.translatable,
                    coalesce(s.rules, '') LIKE '%finddupes2%', o.position, coalesce(s.rules, '') = ?1
             FROM collection s LEFT JOIN collection_order o ON o.collection = s.id AND o.parent IS s.parent
             ORDER BY s.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([crate::apple_photos::COLLECTION_MARKER], |r| {
            Ok(Collection {
                id: r.get(0)?,
                name: r.get(1)?,
                parent: r.get(2)?,
                kind: r.get::<_, i64>(3)? as u8,
                count: r.get::<_, i64>(4)? as usize,
                search: r.get(5)?,
                translatable: r.get::<_, i64>(6)? != 0,
                duplicates: r.get(7)?,
                position: r.get(8)?,
                apple: r.get(9)?,
            })
        })?;
        rows.collect()
    }

    pub fn keywords(&self) -> Result<Vec<Keyword>> {
        // count per keyword: images whose file is not marked missing (status 4). Counted here in one pass over the
        // assignments (~60 ms for 340 000) instead of a correlated subquery per keyword (two lookups per assignment,
        // ~0.3 s for 11 000 keywords)
        let mut live: HashSet<i64> = HashSet::new();
        {
            let mut st = self.conn.prepare("SELECT b.id FROM image b JOIN file d ON d.id = b.file WHERE d.status <> 4")?;
            for id in st.query_map([], |r| r.get::<_, i64>(0))? {
                live.insert(id?);
            }
        }
        let mut count: HashMap<i64, usize> = HashMap::new();
        {
            let mut st = self.conn.prepare("SELECT image, keyword FROM image_keyword")?;
            for pair in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
                let (image, keyword) = pair?;
                if live.contains(&image) {
                    *count.entry(keyword).or_default() += 1;
                }
            }
        }
        let mut st = self.conn.prepare("SELECT s.id, s.name, s.parent, s.person FROM keyword s ORDER BY s.name COLLATE NOCASE")?;
        let rows = st.query_map([], |r| {
            let id: i64 = r.get(0)?;
            Ok(Keyword { id, name: r.get(1)?, parent: r.get(2)?, count: count.get(&id).copied().unwrap_or(0), person: r.get::<_, i64>(3)? != 0 })
        })?;
        rows.collect()
    }

    /// Create an own smart collection (search query) or replace it under the same name.
    pub fn save_smart(&self, name: &str, search: &str) -> Result<i64> {
        // Always the English base forms of the field names (older rules are rewritten on saving)
        let search = crate::search_fields::canonical_query(search);
        let search = search.as_str();
        if let Some(id) = self.conn.query_row("SELECT id FROM collection WHERE name=?1 AND kind=2 AND lr_id IS NULL", [name], |r| r.get::<_, i64>(0)).optional()? {
            self.conn.execute("UPDATE collection SET search=?2, translatable=1 WHERE id=?1", params![id, search])?;
            return Ok(id);
        }
        self.conn.execute("INSERT INTO collection(name, kind, search, translatable) VALUES(?1, 2, ?2, 1)", params![name, search])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Name for a new own smart collection that no other own smart collection carries yet: `name`, else
    /// `name (2)`, `name (3)` …
    pub fn unique_smart_name(&self, name: &str) -> Result<String> {
        let taken = |n: &str| -> Result<bool> {
            Ok(self.conn.query_row("SELECT 1 FROM collection WHERE name=?1 COLLATE NOCASE AND kind=2 AND lr_id IS NULL", [n], |_| Ok(())).optional()?.is_some())
        };
        if !taken(name)? {
            return Ok(name.to_string());
        }
        for i in 2.. {
            let candidate = format!("{name} ({i})");
            if !taken(&candidate)? {
                return Ok(candidate);
            }
        }
        unreachable!()
    }

    /// Save a view as a NEW smart collection; never replaces an existing one (a taken name gets " (2)" etc.).
    /// Returns the id and the name actually given.
    pub fn save_smart_new(&self, name: &str, search: &str) -> Result<(i64, String)> {
        let search = crate::search_fields::canonical_query(search);
        let name = self.unique_smart_name(name)?;
        self.conn.execute("INSERT INTO collection(name, kind, search, translatable) VALUES(?1, 2, ?2, 1)", params![name, search])?;
        Ok((self.conn.last_insert_rowid(), name))
    }

    /// AI description of an image (via its file).
    pub fn ai_of(&self, file: i64) -> Option<Ai> {
        self.conn
            .query_row(
                "SELECT source, model, keywords, scene, quality, technical, discard, reason, nonsense FROM ai WHERE file=?1 ORDER BY id DESC LIMIT 1",
                [file],
                |r| {
                    let sw: Option<String> = r.get(2)?;
                    Ok(Ai {
                        source: r.get(0)?,
                        model: r.get(1)?,
                        keywords: sw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                        scene: r.get(3)?,
                        quality: r.get::<_, Option<i64>>(4)?.map(|q| q as u8),
                        technical: r.get(5)?,
                        delete: r.get(6)?,
                        reason: r.get(7)?,
                        nonsense: r.get::<_, i64>(8)? != 0,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Caption (Lightroom) of an image.
    /// Copyright given to a photo (at import), if any.
    pub fn copyright_of(&self, image: i64) -> Option<String> {
        self.conn.query_row("SELECT copyright FROM image WHERE id=?1", [image], |r| r.get::<_, Option<String>>(0)).optional().ok().flatten().flatten().filter(|c| !c.trim().is_empty())
    }

    pub fn image_text_of(&self, image: i64) -> Option<String> {
        self.conn.query_row("SELECT caption FROM image WHERE id=?1", [image], |r| r.get(0)).optional().ok().flatten().flatten()
    }

    /// Title of an image (from Lightroom), if any.
    pub fn title_of(&self, image: i64) -> Option<String> {
        self.conn.query_row("SELECT title FROM image_meta WHERE image=?1", [image], |r| r.get::<_, Option<String>>(0)).optional().ok().flatten().flatten().filter(|t| !t.trim().is_empty())
    }

    /// GPS position (latitude, longitude in degrees) of an image (from Lightroom), if any.
    pub fn position_of(&self, image: i64) -> Option<(f64, f64)> {
        self.conn
            .query_row("SELECT latitude, longitude FROM image_meta WHERE image=?1 AND latitude IS NOT NULL AND longitude IS NOT NULL", [image], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten()
    }

    /// Image IDs of a collection in Lightroom order.
    pub fn images_of_collection(&self, collection: i64) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare("SELECT image FROM collection_image WHERE collection=?1 ORDER BY position, image")?;
        let rows = st.query_map([collection], |r| r.get(0))?;
        rows.collect()
    }

    /// Quick collection (kind 3: fill quickly with B, show, empty, save as collection).
    /// If needed, the quick collection imported from Lightroom ("quick collection") is taken over,
    /// otherwise a new one is created. Returns: its ID.
    pub fn quick_collection(&self) -> Result<i64> {
        if let Some(id) = self.conn.query_row("SELECT id FROM collection WHERE kind=3 LIMIT 1", [], |r| r.get(0)).optional()? {
            return Ok(id);
        }
        let old: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM collection WHERE kind=0 AND parent IS NULL AND lr_id IS NOT NULL AND lower(name) IN ('schnellsammlung','quick collection') ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = old {
            self.conn.execute("UPDATE collection SET kind=3 WHERE id=?1", [id])?;
            return Ok(id);
        }
        self.conn.execute("INSERT INTO collection(name, kind) VALUES('Schnellsammlung', 3)", [])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Read only (for display, without writing): ID of the quick collection, if there already is one
    /// (kind 3 or the "Schnellsammlung" imported from Lightroom).
    pub fn quick_id(&self) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT id FROM collection WHERE kind=3
                 UNION ALL
                 SELECT id FROM collection WHERE kind=0 AND parent IS NULL AND lr_id IS NOT NULL AND lower(name) IN ('schnellsammlung','quick collection')
                 LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Put images into the quick collection (`into_dir`) or take them out. Repeatable: whoever is already in
    /// stays; whoever is not in is not reported. Returns: number of changed assignments.
    pub fn quick_set(&self, images: &[i64], into_dir: bool) -> Result<usize> {
        let sq = self.quick_collection()?;
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        if into_dir {
            let mut pos: f64 = tx.query_row("SELECT coalesce(max(position), 0) + 1 FROM collection_image WHERE collection=?1", [sq], |r| r.get(0))?;
            for b in images {
                let k = tx.execute("INSERT OR IGNORE INTO collection_image(collection, image, position) VALUES(?1, ?2, ?3)", params![sq, b, pos])?;
                if k > 0 {
                    pos += 1.0;
                }
                n += k;
            }
        } else {
            for b in images {
                n += tx.execute("DELETE FROM collection_image WHERE collection=?1 AND image=?2", params![sq, b])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Like B in common photo apps: if at least one image is missing from the quick collection, all go in, otherwise
    /// all go out. Returns: (added, removed).
    pub fn quick_toggle(&self, images: &[i64]) -> Result<(usize, usize)> {
        let sq = self.quick_collection()?;
        let mut inside = 0usize;
        for b in images {
            inside += self.conn.query_row("SELECT count(*) FROM collection_image WHERE collection=?1 AND image=?2", params![sq, b], |r| r.get::<_, i64>(0))? as usize;
        }
        if !images.is_empty() && inside == images.len() {
            Ok((0, self.quick_set(images, false)?))
        } else {
            Ok((self.quick_set(images, true)?, 0))
        }
    }

    /// Empty the quick collection (only the assignment, never images or files). Returns: removed assignments.
    pub fn quick_clear(&self) -> Result<usize> {
        let sq = self.quick_collection()?;
        Ok(self.conn.execute("DELETE FROM collection_image WHERE collection=?1", [sq])?)
    }

    /// New plain collection at the top level (the name is made unique if needed: "Name 2"). Returns: ID.
    pub fn create_collection(&self, name: &str) -> Result<i64> {
        self.create_collection_in(name, None)
    }

    /// New plain collection, optionally in a collection set (the name is made unique there if needed). Returns: ID.
    pub fn create_collection_in(&self, name: &str, parent: Option<i64>) -> Result<i64> {
        let base = name.trim();
        let base = if base.is_empty() { "Neue Sammlung" } else { base };
        let mut new = base.to_string();
        let mut n = 2;
        while self.conn.query_row("SELECT count(*) FROM collection WHERE name=?1 AND parent IS ?2", params![&new, parent], |r| r.get::<_, i64>(0))? > 0 {
            new = format!("{base} {n}");
            n += 1;
        }
        self.conn.execute("INSERT INTO collection(name, kind, parent) VALUES(?1, 0, ?2)", params![&new, parent])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Collection set ("folder" for collections) for this name: the one remembered for it (also after moving or
    /// renaming), else one of this name, else a new one at the top level (`crate::collection_order::find_or_create_set`).
    pub fn find_or_create_collection_set(&self, name: &str) -> Result<i64> {
        crate::collection_order::find_or_create_set(&self.conn, name)
    }

    /// Append images to a plain collection (already contained ones stay). Returns: newly added.
    pub fn add_to_collection(&self, collection: i64, images: &[i64]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut pos: f64 = tx.query_row("SELECT coalesce(max(position), 0) FROM collection_image WHERE collection=?1", [collection], |r| r.get(0))?;
        let mut n = 0;
        for &b in images {
            pos += 1.0;
            n += tx.execute("INSERT OR IGNORE INTO collection_image(collection, image, position) VALUES(?1, ?2, ?3)", params![collection, b, pos])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Save the content of the quick collection as a new plain collection (the name is made unique if needed).
    /// Returns: ID of the new collection.
    pub fn quick_as_collection(&self, name: &str) -> Result<i64> {
        let sq = self.quick_collection()?;
        let base = name.trim();
        let base = if base.is_empty() { "Aus Schnellsammlung" } else { base };
        let mut new = base.to_string();
        let mut n = 2;
        while self.conn.query_row("SELECT count(*) FROM collection WHERE name=?1 AND parent IS NULL", [&new], |r| r.get::<_, i64>(0))? > 0 {
            new = format!("{base} {n}");
            n += 1;
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO collection(name, kind) VALUES(?1, 0)", [&new])?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO collection_image(collection, image, position) SELECT ?1, image, position FROM collection_image WHERE collection=?2", params![id, sq])?;
        tx.commit()?;
        Ok(id)
    }

    /// Image IDs with this keyword or one below it.
    pub fn images_with_keyword(&self, keyword: i64) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare(
            "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM keyword s JOIN t ON s.parent = t.id)
             SELECT DISTINCT image FROM image_keyword WHERE keyword IN (SELECT id FROM t)",
        )?;
        let rows = st.query_map([keyword], |r| r.get(0))?;
        rows.collect()
    }

    /// Full-text search (substring, case-insensitive) in file name, copy name and keywords.
    pub fn search(&self, text: &str) -> Result<Vec<i64>> {
        let pattern = format!("%{}%", text.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let mut st = self.conn.prepare(
            "SELECT b.id FROM image b JOIN file d ON d.id = b.file
               WHERE d.name LIKE ?1 ESCAPE '\\' OR b.copy LIKE ?1 ESCAPE '\\'
             UNION
             SELECT bs.image FROM image_keyword bs JOIN keyword s ON s.id = bs.keyword WHERE s.name LIKE ?1 ESCAPE '\\'",
        )?;
        let rows = st.query_map([pattern], |r| r.get(0))?;
        rows.collect()
    }

    /// Keywords of an image (name, with parent path "A › B").
    pub fn keywords_of(&self, image: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "WITH RECURSIVE p(id, name, parent, depth) AS (
                SELECT s.id, s.name, s.parent, 0 FROM image_keyword bs JOIN keyword s ON s.id = bs.keyword WHERE bs.image = ?1
                UNION ALL SELECT p.id, e.name || ' › ' || p.name, e.parent, p.depth + 1 FROM p JOIN keyword e ON e.id = p.parent WHERE p.depth < 64)
             SELECT name FROM p WHERE parent IS NULL ORDER BY name COLLATE NOCASE",
        )?;
        let rows = st.query_map([image], |r| r.get(0))?;
        rows.collect()
    }

    /// Keywords of the image (leaf names) without names of people (face recognition), for exported files.
    pub fn keywords_without_people(&self, image: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT s.name FROM image_keyword bs JOIN keyword s ON s.id = bs.keyword
             WHERE bs.image = ?1 AND s.person = 0
               AND NOT EXISTS (SELECT 1 FROM face f WHERE f.person = s.id)
             ORDER BY s.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([image], |r| r.get(0))?;
        rows.collect()
    }

    /// Names of the collections that contain the image.
    pub fn collections_of(&self, image: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT s.name FROM collection_image sb JOIN collection s ON s.id = sb.collection WHERE sb.image = ?1 ORDER BY s.name COLLATE NOCASE")?;
        let rows = st.query_map([image], |r| r.get(0))?;
        rows.collect()
    }

    /// Absolute path of a file.
    pub fn file_path(&self, file: i64) -> Option<PathBuf> {
        self.conn
            .query_row(
                "SELECT s.path || o.path || d.name FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1",
                [file],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .map(PathBuf::from)
    }

    /// Absolute paths of all folders (id → path with trailing '/').
    pub fn folder_paths(&self) -> Result<HashMap<i64, String>> {
        let mut st = self.conn.prepare("SELECT o.id, s.path || o.path FROM folder o JOIN root_folder s ON s.id = o.root")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }


    /// Set stars/flag/label (`None` = unchanged) and remember it as an own change.
    /// Remove images from the library – **database only, never files**. Original: the file
    /// including all images (copies) and camera JPEG rows belonging to it;
    /// virtual copy: only that. Lightroom files go into `removed` so that the next import
    /// does not recreate them. Returns: removed images.
    pub fn remove_images(&self, images: &[i64], also_sidecar_jpeg: &[i64]) -> Result<usize> {
        let t = crate::now();
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for &b in images.iter().chain(also_sidecar_jpeg) {
            let Some((file, copy)): Option<(i64, Option<String>)> = tx.query_row("SELECT file, copy FROM image WHERE id=?1", [b], |r| Ok((r.get(0)?, r.get(1)?))).optional()? else { continue };
            if copy.is_some() {
                n += tx.execute("DELETE FROM image WHERE id=?1", [b])?;
                // the copy's develop state goes with it (a new copy of the same name starts fresh)
                tx.execute("DELETE FROM develop_state WHERE file=?1 AND copy=?2", rusqlite::params![file, copy])?;
                crate::develop_history::forget(&tx, file, copy.as_deref())?;
                crate::store::drop_queued_history(file, copy.as_deref());
                continue;
            }
            n += tx.query_row("SELECT count(*) FROM image WHERE file=?1", [file], |r| r.get::<_, i64>(0))? as usize;
            let lr: Option<(Option<i64>, String)> = tx
                .query_row("SELECT d.lr_id, s.path || o.path || d.name FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE d.id=?1", [file], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            if let Some((Some(id), path)) = lr {
                tx.execute("INSERT OR REPLACE INTO removed_lr_file(lr_id, path, time) VALUES(?1, ?2, ?3)", params![id, path, t])?;
            }
            tx.execute("DELETE FROM file WHERE id=?1", [file])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// File ID of an image.
    pub fn file_of(&self, image: i64) -> Option<i64> {
        self.conn.query_row("SELECT file FROM image WHERE id=?1", [image], |r| r.get(0)).ok()
    }


    /// Set the orientation of a file (EXIF 1–8, e.g. after a rotation elsewhere).
    pub fn set_orientation(&self, file: i64, orientation: i64) -> Result<()> {
        self.conn.execute("UPDATE file SET orientation = ?2 WHERE id = ?1", params![file, orientation])?;
        Ok(())
    }

    /// Set ratings per image absolutely (image, stars, flag, label) – one transaction.
    pub fn set_ratings(&self, values: &[(i64, u8, i8, u8)]) -> Result<()> {
        let t = crate::now();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare("UPDATE image SET stars = ?2, flag = ?3, label = ?4, changed = ?5, rating_time = ?5 WHERE id = ?1")?;
            for &(b, s, m, e) in values {
                st.execute(params![b, s as i64, m as i64, e as i64, t])?;
            }
        }
        tx.commit()
    }

    pub fn set_rating(&self, images: &[i64], stars: Option<u8>, flag: Option<i8>, label: Option<u8>) -> Result<()> {
        let t = crate::now();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare(
                "UPDATE image SET stars = coalesce(?2, stars), flag = coalesce(?3, flag), label = coalesce(?4, label), changed = ?5, rating_time = ?5 WHERE id = ?1",
            )?;
            for &b in images {
                st.execute(params![b, stars.map(|v| v as i64), flag.map(|v| v as i64), label.map(|v| v as i64), t])?;
            }
        }
        tx.commit()
    }

    /// Find an image by absolute path (original, not a virtual copy).
    pub fn image_to_path(&self, path: &Path) -> Option<i64> {
        let p = crate::nfc(&path.to_string_lossy());
        let (dir, name) = p.rsplit_once('/')?;
        let dir = format!("{dir}/");
        self.conn
            .query_row(
                "SELECT b.id FROM image b JOIN file d ON d.id = b.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
                 WHERE b.copy IS NULL AND d.name = ?2 AND s.path || o.path = ?1",
                params![dir, name],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Fill in size, date, dimensions, camera (from preview generation), only empty fields.
    pub fn complete_file(&self, file: i64, width: Option<u32>, height: Option<u32>, capture: Option<&str>, camera: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE file SET width = coalesce(width, ?2), height = coalesce(height, ?3), capture_time = coalesce(capture_time, ?4), camera = coalesce(camera, ?5) WHERE id = ?1",
            params![file, width, height, capture, camera],
        )?;
        Ok(())
    }

    /// Normalize camera names: an earlier code path stored the cleaned-up model name without the
    /// manufacturer for RAWs ("EOS RP" instead of "Canon EOS RP" as in the EXIF data). If the same name also exists
    /// with exactly one word in front, that one wins. Returns: changed entries.
    pub fn normalize_camera_names(&self) -> Result<usize> {
        let names: Vec<String> = self.conn.prepare("SELECT DISTINCT camera FROM file WHERE camera IS NOT NULL")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
        let mut n = 0;
        for k in &names {
            let long: Vec<&String> = names.iter().filter(|l| l.len() > k.len() + 1 && l.ends_with(k.as_str()) && l[..l.len() - k.len()].ends_with(' ') && !l[..l.len() - k.len() - 1].contains(' ')).collect();
            if let [l] = long[..] {
                n += self.conn.execute("UPDATE file SET camera = ?1 WHERE camera = ?2", params![l, k])?;
            }
        }
        Ok(n)
    }

    pub fn runs(&self) -> Result<Vec<Run>> {
        let mut st = self.conn.prepare("SELECT id, kind, started, finished, added, report FROM run ORDER BY id DESC LIMIT 20")?;
        let rows = st.query_map([], |r| Ok(Run { id: r.get(0)?, kind: r.get(1)?, begin: r.get(2)?, end: r.get(3)?, new: r.get(4)?, report: r.get(5)? }))?;
        rows.collect()
    }

    /// Run that most recently brought new images through a real import ("Zuletzt importiert"): only the import
    /// window (card, camera, folder: 'import', 'folder') – never the takeover of a Lightroom catalog (tens of
    /// thousands of photos), a folder added with "+" in FOLDERS ('root_folder') or the silent sync, which also finds
    /// e.g. new screenshots. None: no real import yet (the row then counts 0).
    pub fn last_import(&self) -> Option<i64> {
        self.conn
            .query_row("SELECT max(b.run) FROM image b JOIN run l ON l.id = b.run WHERE l.kind IN ('import', 'folder')", [], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
            .flatten()
    }

    /// A sync run that served an import counts as such ('ordner' or 'import').
    pub fn mark_run(&self, run: i64, kind: &str) -> Result<()> {
        self.conn.execute("UPDATE run SET kind = ?2 WHERE id = ?1", params![run, kind])?;
        Ok(())
    }

    /// Begin a new run.
    pub fn new_run(&self, kind: &str, source: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO run(kind, source, started) VALUES(?1, ?2, ?3)", params![kind, source, crate::now()])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(&self, id: i64, new: i64, report: &str) -> Result<()> {
        self.conn.execute("UPDATE run SET finished=?2, added=?3, report=?4 WHERE id=?1", params![id, crate::now(), new, report])?;
        Ok(())
    }

    /// Create or find a root folder (absolute path, with trailing '/').
    pub fn create_root(&self, path: &str, name: &str, origin: &str, lr_id: Option<i64>) -> Result<i64> {
        if let Some(id) = lr_id.and_then(|l| self.conn.query_row("SELECT id FROM root_folder WHERE lr_id=?1", [l], |r| r.get::<_, i64>(0)).optional().ok().flatten()) {
            self.conn.execute("UPDATE root_folder SET path=?2, name=?3 WHERE id=?1", params![id, path, name])?;
            return Ok(id);
        }
        if let Some(id) = self.conn.query_row("SELECT id FROM root_folder WHERE path=?1", [path], |r| r.get::<_, i64>(0)).optional()? {
            if lr_id.is_some() {
                self.conn.execute("UPDATE root_folder SET lr_id=?2 WHERE id=?1", params![id, lr_id])?;
            }
            return Ok(id);
        }
        self.conn.execute("INSERT INTO root_folder(path, name, origin, lr_id) VALUES(?1,?2,?3,?4)", params![path, name, origin, lr_id])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Create or find a folder.
    pub fn create_folder(&self, root: i64, path: &str, lr_id: Option<i64>) -> Result<i64> {
        // adding a folder explicitly (import, "add folder", Lightroom) takes it back if it was removed from the library
        let _ = crate::manage::lift_exclusions(&self.conn, root, path);
        if let Some(id) = lr_id.and_then(|l| self.conn.query_row("SELECT id FROM folder WHERE lr_id=?1", [l], |r| r.get::<_, i64>(0)).optional().ok().flatten()) {
            // folder moved/renamed in Lightroom: update the path (if free)
            let _ = self.conn.execute("UPDATE folder SET root=?2, path=?3 WHERE id=?1", params![id, root, path]);
            return Ok(id);
        }
        if let Some(id) = self.conn.query_row("SELECT id FROM folder WHERE root=?1 AND path=?2", params![root, path], |r| r.get::<_, i64>(0)).optional()? {
            if lr_id.is_some() {
                self.conn.execute("UPDATE folder SET lr_id=?2 WHERE id=?1", params![id, lr_id])?;
            }
            return Ok(id);
        }
        self.conn.execute("INSERT INTO folder(root, path, lr_id) VALUES(?1,?2,?3)", params![root, path, lr_id])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Root folder for an absolute path (the path lies inside it or is it).
    pub fn root_to_path(&self, path: &Path) -> Option<Root> {
        let p = format!("{}/", crate::nfc(path.to_string_lossy().trim_end_matches('/')));
        self.roots().ok()?.into_iter().find(|s| p.starts_with(&s.path))
    }
}

/// EXIF orientation from Lightroom's text (`AB` = normal, `BC` = 90° right, `CD` = 180°,
/// `DA` = 90° left; mirrored: `BA`, `CB`, `DC`, `AD`).
pub fn orientation_from_lightroom(o: &str) -> Option<i64> {
    Some(match o {
        "AB" => 1,
        "BA" => 2,
        "CD" => 3,
        "DC" => 4,
        "CB" => 5,
        "BC" => 6,
        "AD" => 7,
        "DA" => 8,
        _ => return None,
    })
}

#[cfg(test)]
mod migration_guard_tests {
    use super::*;

    /// The keyword counts (images whose file is not marked missing) are those of the earlier correlated subquery.
    #[test]
    fn keyword_counts_match_the_subquery() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO file(id, folder, name, format, status) VALUES (1, 1, 'a.jpg', 'JPG', 0), (2, 1, 'b.jpg', 'JPG', 4), (3, 1, 'c.jpg', 'JPG', 1), (4, 1, 'd.jpg', 'JPG', 0);
             INSERT INTO image(id, file) VALUES (10, 1), (11, 4), (20, 2), (30, 3), (40, 99);
             INSERT INTO keyword(id, name, person) VALUES (1, 'Rom', 0), (2, 'italien', 0), (3, 'Eva', 1), (4, 'leer', 0);
             INSERT INTO image_keyword VALUES (10, 1), (11, 1), (20, 1), (30, 2), (40, 2), (10, 3), (77, 3);",
        )
        .unwrap();
        let mut st = db
            .conn
            .prepare("SELECT s.id, (SELECT count(*) FROM image_keyword bs JOIN image b ON b.id = bs.image JOIN file d ON d.id = b.file WHERE bs.keyword = s.id AND d.status <> 4) FROM keyword s ORDER BY s.name COLLATE NOCASE")
            .unwrap();
        let old: Vec<(i64, usize)> = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(|x| x.unwrap()).collect();
        let new: Vec<(i64, usize)> = db.keywords().unwrap().iter().map(|k| (k.id, k.count)).collect();
        assert_eq!(new, old);
        assert_eq!(new, vec![(3, 1), (2, 1), (4, 0), (1, 2)]);
    }

    /// Opening a library with the current schema writes nothing: it works while another connection holds the write
    /// lock, and the WAL does not grow.
    #[test]
    fn open_with_current_schema_does_not_write() {
        let p = std::env::temp_dir().join(format!("graineo-open-ro-{}-{}.sqlite", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        drop(Db::open(&p).unwrap());
        let holder = Db::open(&p).unwrap();
        holder.conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO info(name, value) VALUES('test', '1');").unwrap();
        let wal = || std::fs::metadata(p.with_extension("sqlite-wal")).map(|m| m.len()).unwrap_or(0);
        let before = wal();
        let (tx, rx) = std::sync::mpsc::channel();
        let q = p.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Db::open(&q).map(|_| ()).map_err(|e| e.to_string()));
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("open waited for the write lock");
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(wal(), before);
        holder.conn.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        for x in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{x}", p.display()));
        }
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("graineo-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Columns (name, type, not null, default, primary key) of every table, sorted: the schema without its SQL text.
    fn shape(c: &Connection) -> Vec<String> {
        let tables: Vec<String> = c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        let mut v = Vec::new();
        for t in tables {
            let mut cols: Vec<String> = c
                .prepare(&format!("SELECT name, type, \"notnull\", coalesce(dflt_value, ''), pk FROM pragma_table_info('{t}')"))
                .unwrap()
                .query_map([], |r| Ok(format!("{t}.{}:{}:{}:{}:{}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?)))
                .unwrap()
                .flatten()
                .collect();
            cols.sort();
            v.extend(cols);
            let fks: Vec<String> = c
                .prepare(&format!("SELECT \"table\", \"from\", \"to\", on_delete FROM pragma_foreign_key_list('{t}')"))
                .unwrap()
                .query_map([], |r| Ok(format!("{t} fk {}.{}→{} {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default(), r.get::<_, String>(3)?)))
                .unwrap()
                .flatten()
                .collect();
            v.extend(fks);
        }
        let mut idx: Vec<String> = c.prepare("SELECT name || ' ON ' || tbl_name FROM sqlite_master WHERE type='index' AND sql IS NOT NULL ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        v.append(&mut idx);
        v
    }

    /// Schema 20: a library of graineo 1.0 (schema 19, German names, rows in every table) is backed up, then migrated in
    /// one go. Afterwards it has exactly the schema of a new library (tables, columns, defaults, foreign keys, indexes),
    /// the same number of rows everywhere, translated values and keys, a working search index, and the old names are gone.
    #[test]
    fn schema_20_migrates_a_library_of_graineo_1_0() {
        let dir = tmp_dir("schema20");
        let p = dir.join("library.sqlite");
        let before = legacy_names::sql::create_schema_19_with_rows(&Connection::open(&p).unwrap()).unwrap();
        // the start screen asks before opening: an old library needs the conversion, a missing file does not
        assert_eq!(migration_needed(&p), Some(19));
        assert_eq!(migration_needed(&dir.join("missing.sqlite")), None);
        let db = Db::open(&p).unwrap();
        assert_eq!(migration_needed(&p), None, "converted: nothing slow left");
        assert_eq!(migration_backup_path(&p), dir.join(format!("backups/library-before-schema-{VERSION}.sqlite")));
        assert_eq!(db.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0)).unwrap(), VERSION);
        assert!(dir.join(format!("backups/library-before-schema-{VERSION}.sqlite")).exists(), "backup before the migration");
        // the backup is the untouched old library
        let old = Connection::open_with_flags(dir.join(format!("backups/library-before-schema-{VERSION}.sqlite")), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(old.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0)).unwrap(), 19);
        // same rows in every table
        for ((old_name, n), (_, new_name)) in before.iter().zip(legacy_names::sql::TABLES) {
            let after: i64 = db.conn.query_row(&format!("SELECT count(*) FROM {new_name}"), [], |r| r.get(0)).unwrap();
            assert_eq!(after, *n, "{old_name} → {new_name}");
        }
        // same shape as a new library
        let fresh = tmp_dir("schema20-fresh").join("library.sqlite");
        let new = Db::open(&fresh).unwrap();
        assert_eq!(shape(&db.conn), shape(&new.conn));
        // no table, column or index keeps an old name
        let old_words: Vec<&str> = legacy_names::sql::TABLES.iter().map(|(o, _)| *o).chain(legacy_names::sql::COLUMNS.iter().filter(|(o, _)| o.len() > 1).map(|(o, _)| *o)).collect();
        for item in shape(&db.conn) {
            for w in item.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                assert!(!old_words.contains(&w), "old name {w} in {item}");
            }
        }
        // values and keys
        let one = |q: &str| db.conn.query_row(q, [], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(one("SELECT group_concat(status, ',') FROM (SELECT status FROM face ORDER BY id)"), "confirmed,suggested,unknown,rejected");
        assert_eq!(one("SELECT source FROM face WHERE id = 3"), "manual");
        assert_eq!(one("SELECT group_concat(origin, ',') FROM (SELECT origin FROM root_folder ORDER BY id)"), "own,apple-photos");
        assert_eq!(one("SELECT group_concat(kind, ',') FROM (SELECT kind FROM run ORDER BY id)"), "sync,folder,root_folder");
        assert_eq!(one("SELECT source FROM image_vector"), "own");
        assert_eq!(one("SELECT source FROM ai"), "imported");
        assert_eq!(one("SELECT source FROM text_vector"), "imported");
        assert_eq!(one("SELECT value FROM info WHERE name = 'lightroom_catalog'"), "/x/Katalog.lrcat");
        assert_eq!(one("SELECT value FROM info WHERE name = 'schema_compatible'"), SCHEMA_COMPATIBLE.to_string());
        assert_eq!(one("SELECT report FROM run WHERE id = 1"), r#"{"missing":0,"newly_added":3,"sidecar_new":1}"#);
        assert_eq!(one("SELECT dflt_value FROM pragma_table_info('face') WHERE name = 'status'"), "'unknown'");
        // a face inserted without a status gets the English default; deleting a face still cascades
        db.conn.execute("INSERT INTO face(image, x, y, w, h, source) VALUES(1, 0, 0, 0.1, 0.1, 'graineo')", []).unwrap();
        assert_eq!(one("SELECT status FROM face ORDER BY id DESC LIMIT 1"), "unknown");
        db.conn.execute("DELETE FROM face WHERE id = 3", []).unwrap();
        assert_eq!(db.conn.query_row("SELECT count(*) FROM face_never", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // search index kept and usable, vocabulary rebuilt
        assert_eq!(db.conn.query_row("SELECT count(*) FROM search_word WHERE search_word MATCH 'strand'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(db.conn.query_row("SELECT count(*) FROM search_vocab", [], |r| r.get::<_, i64>(0)).unwrap() >= 3);
        // the grid works
        let rows = db.rows().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|z| z.copy.as_deref() == Some("Kopie 1") && z.stars == 5));
        // partial indexes are used under their new names
        let plan: Vec<String> = db
            .conn
            .prepare("EXPLAIN QUERY PLAN SELECT id FROM face x WHERE x.cluster = 5 AND +x.person IS NULL ORDER BY x.quality DESC LIMIT 6")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .flatten()
            .collect();
        assert!(plan.iter().any(|l| l.contains("face_cluster")), "{plan:?}");
        // opening again changes nothing (idempotent)
        drop(db);
        let db = Db::open(&p).unwrap();
        assert_eq!(shape(&db.conn), shape(&new.conn));
        drop((db, new));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(fresh.parent().unwrap());
    }

    /// An error in the middle of the migration (after the renames) rolls everything back: the library stays a valid
    /// schema-19 library with its old names and rows; the next start migrates it.
    #[test]
    fn interrupted_migration_leaves_the_old_library() {
        let dir = tmp_dir("schema20-fail");
        let p = dir.join("library.sqlite");
        let before = legacy_names::sql::create_schema_19_with_rows(&Connection::open(&p).unwrap()).unwrap();
        let schema_before: Vec<String> = Connection::open(&p).unwrap().prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        FAIL_MIGRATION.store(true, std::sync::atomic::Ordering::SeqCst);
        let e = Db::open(&p).err().expect("the interrupted migration must fail");
        FAIL_MIGRATION.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(e.to_string().contains("nichts verändert"), "{e}");
        {
            let c = Connection::open(&p).unwrap();
            assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0)).unwrap(), 19);
            let now: Vec<String> = c.prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
            assert_eq!(now, schema_before, "schema unchanged");
            for (old, n) in &before {
                assert_eq!(c.query_row(&format!("SELECT count(*) FROM {old}"), [], |r| r.get::<_, i64>(0)).unwrap(), *n, "{old}");
            }
            assert_eq!(c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
        }
        let db = Db::open(&p).unwrap();
        assert_eq!(db.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0)).unwrap(), VERSION);
        assert_eq!(db.conn.query_row("SELECT count(*) FROM face", [], |r| r.get::<_, i64>(0)).unwrap(), 4);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Older libraries (schema 1 … 18) go through the steps of graineo 1.0 first, then get the English names.
    #[test]
    fn very_old_library_reaches_schema_20() {
        let dir = tmp_dir("schema20-old");
        let p = dir.join("library.sqlite");
        legacy_names::sql::create_schema_15_offloading(&Connection::open(&p).unwrap()).unwrap();
        let db = Db::open(&p).unwrap();
        let fresh = tmp_dir("schema20-old-fresh").join("library.sqlite");
        let new = Db::open(&fresh).unwrap();
        assert_eq!(shape(&db.conn), shape(&new.conn));
        drop((db, new));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(fresh.parent().unwrap());
    }

    /// The count of names to review reads only the suspicious faces (partial index), not the whole face table.
    #[test]
    fn review_count_uses_the_partial_index() {
        let p = std::env::temp_dir().join(format!("graineo-review-index-{}-{}.sqlite", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let db = Db::open(&p).unwrap();
        let plan: Vec<String> = db
            .conn
            .prepare("EXPLAIN QUERY PLAN SELECT count(*) FROM face WHERE suspect=1 AND guessed IS NOT NULL AND local=0")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .flatten()
            .collect();
        assert!(plan.iter().any(|l| l.contains("face_suspect")), "{plan:?}");
        assert_eq!(crate::people::check_count(&db.conn), 0);
        drop(db);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn recent_import_ignores_the_silent_sync() {
        let p = std::env::temp_dir().join(format!("graineo-recent-{}-{}.sqlite", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let db = Db::open(&p).unwrap();
        let root = db.create_root("/tmp/graineo-recent/", "recent", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        let add = |name: &str, run: i64| {
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, ?2, ?3)", params![folder, name, "JPG"]).unwrap();
            let file = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file, run) VALUES(?1, ?2)", params![file, run]).unwrap();
        };
        // a Lightroom takeover alone is no import: the row counts 0
        let lr_run = db.new_run("lightroom", "/x/Katalog.lrcat").unwrap();
        add("lr.jpg", lr_run);
        assert_eq!(db.last_import(), None, "the Lightroom takeover must not become „Zuletzt importiert“");
        let folder_run = db.new_run("sync", "").unwrap();
        db.mark_run(folder_run, "folder").unwrap();
        add("a.jpg", folder_run);
        let sync_run = db.new_run("sync", "").unwrap();
        add("b.png", sync_run);
        assert_eq!(db.last_import(), Some(folder_run), "the sync's find must not become „Zuletzt importiert“");
        let card_run = db.new_run("sync", "").unwrap();
        db.mark_run(card_run, "import").unwrap();
        add("c.jpg", card_run);
        let later_lr = db.new_run("lightroom", "/x/Katalog.lrcat").unwrap();
        add("lr2.jpg", later_lr);
        let plus_run = db.new_run("sync", "").unwrap();
        db.mark_run(plus_run, "root_folder").unwrap();
        add("d.jpg", plus_run);
        assert_eq!(db.last_import(), Some(card_run), "a later Lightroom takeover or „+“ folder does not replace the card import");
        drop(db);
        let _ = std::fs::remove_file(&p);
    }

    /// Migration of a copy of a real library: `GRAINEO_TEST_LIBRARY_COPY=<copy of library.sqlite>` (only with `--ignored`;
    /// never the library itself – the copy is changed). Prints rows per table before and after, the time and the checks.
    #[test]
    #[ignore]
    fn migration_of_a_library_copy() {
        let Some(p) = std::env::var_os("GRAINEO_TEST_LIBRARY_COPY").map(PathBuf::from) else { return };
        let count = |c: &Connection, q: &str| c.query_row(q, [], |r| r.get::<_, i64>(0)).unwrap();
        let before: Vec<(String, i64)> = {
            let c = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(count(&c, "PRAGMA user_version"), 19);
            legacy_names::sql::TABLES.iter().map(|(o, _)| (o.to_string(), count(&c, &format!("SELECT count(*) FROM {o}")))).collect()
        };
        let t = std::time::Instant::now();
        let db = Db::open(&p).unwrap();
        let ms = t.elapsed().as_millis();
        for ((old, n), (_, new)) in before.iter().zip(legacy_names::sql::TABLES) {
            let after = count(&db.conn, &format!("SELECT count(*) FROM {new}"));
            eprintln!("{old:>20} → {new:<20} {n:>8} → {after:>8}");
            assert_eq!(after, *n);
        }
        let ok: String = db.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        let fk = count(&db.conn, "SELECT count(*) FROM pragma_foreign_key_check");
        eprintln!("schema 19 → {}: {ms} ms including backup, integrity {ok}, foreign key problems {fk}", count(&db.conn, "PRAGMA user_version"));
        assert_eq!((ok.as_str(), fk), ("ok", 0));
        assert!(p.parent().unwrap().join(format!("backups/library-before-schema-{VERSION}.sqlite")).exists());
    }

    /// Forward/backward: a library of a newer version is opened only if it is marked compatible with this one; an
    /// incompatible one is refused with a clear message and left byte for byte as it was (also its WAL).
    #[test]
    fn newer_library_is_refused_unless_compatible() {
        let dir = std::env::temp_dir().join(format!("graineo-newer-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.sqlite");
        drop(Db::open(&p).unwrap());
        let set = |v: i32, compatible: &str| {
            let c = Connection::open(&p).unwrap();
            c.execute("UPDATE info SET value=?1 WHERE name='schema_compatible'", [compatible]).unwrap();
            c.pragma_update(None, "user_version", v).unwrap();
            c.pragma_update(None, "wal_checkpoint", "TRUNCATE").unwrap();
        };
        // newer and only adding (compatible from 1): usable, nothing written
        set(VERSION + 1, "1");
        let db = Db::open(&p).unwrap();
        assert_eq!(db.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0)).unwrap(), VERSION + 1, "not set back");
        drop(db);
        // newer and incompatible: refused, file unchanged
        set(VERSION + 1, &(VERSION + 1).to_string());
        let before = std::fs::read(&p).unwrap();
        let e = Db::open(&p).err().expect("an incompatible newer library must be refused").to_string();
        assert!(e.contains("neueren Version"), "{e}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(!dir.join("backups").exists(), "no migration backup for a newer library");
        // marker missing (damaged info table): refused too
        Connection::open(&p).unwrap().execute("DELETE FROM info WHERE name='schema_compatible'", []).unwrap();
        assert!(Db::open(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is no SQLite database (damaged, foreign) is refused, never overwritten by a new empty library.
    #[test]
    fn damaged_library_is_not_replaced() {
        let dir = std::env::temp_dir().join(format!("graineo-damaged-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.sqlite");
        let junk = b"this is not a database, but it must stay as it is ......................................................".repeat(50);
        std::fs::write(&p, &junk).unwrap();
        assert!(Db::open(&p).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), junk);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod smart_name_tests {
    use super::*;

    /// Saving a view as a smart collection never replaces an existing one: a taken name gets " (2)", " (3)" …
    #[test]
    fn saving_a_view_never_replaces_a_smart_collection() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let (a, na) = db.save_smart_new("Sterne", "rating:>=3").unwrap();
        assert_eq!(na, "Sterne");
        let (b, nb) = db.save_smart_new("sterne", "rating:>=5").unwrap();
        assert_eq!(nb, "sterne (2)");
        let (c, nc) = db.save_smart_new("Sterne", "rating:>=1").unwrap();
        assert_eq!(nc, "Sterne (3)");
        assert!(a != b && b != c);
        let first: String = db.conn.query_row("SELECT search FROM collection WHERE id=?1", [a], |r| r.get(0)).unwrap();
        assert_eq!(first, "rating:>=3", "the first rule is untouched");
        assert_eq!(db.unique_smart_name("Neu").unwrap(), "Neu");
    }
}
