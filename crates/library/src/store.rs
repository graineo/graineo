//! The library is the main store: ratings, keywords, virtual copies, the own develop
//! settings and the Lightroom data taken over from the catalog live in `library.sqlite`. The app reads and writes them
//! there – fast, without touching the photos' folders (NAS).
//!
//! - Develop state per photo and virtual copy: table `develop_state` (file + copy name, `''` = original) with the own
//!   settings (JSON, as `graineo:DevelopSettings`) and the Lightroom data (as `graineo:LightroomDevelop`, …).
//! - Sidecars next to the photos (`NAME.xmp`) are an optional, visible copy (setting `writeSidecars`, default off;
//!   `GRAINEO_LIBRARY_SIDECARS=1/0` overrides it for tests). When on, every change only queues the file
//!   (`pending_sidecar_write`); a background writer ([`start_sidecar_writer`]) writes the queue in batches at the lowest
//!   priority, without `fsync` per file, and keeps files on unreachable locations for later. Off: no new sidecars;
//!   existing ones stay untouched.
//! - Existing sidecars are read once per file (`file.xmp_read`): after the update, for new photos and after a rebuild
//!   ([`crate::sidecar_import`]). Later changes by other programs only come in through "Begleitdateien einlesen …".
//! - Photos outside the library (opened from a folder) keep using their sidecar directly – there is no other place.
//!
//! The app accesses the state by path ([`locate`]); reads use one connection per thread, writes go through one writer
//! thread ([`set_develop`], [`set_meta`]) so that the UI never waits for the library's write lock. Until a write has
//! reached the library, reads see it from memory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Condvar, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Db;
use crate::xmp::Lightroom;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}


/// Develop state of a photo or a virtual copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    pub settings: Option<String>,
    pub settings_time: Option<i64>,
    pub lightroom: Lightroom,
}

fn copy_key(copy: Option<&str>) -> &str {
    copy.unwrap_or("")
}

/// Develop state of `file` (original: `copy = None`).
pub fn get(conn: &Connection, file: i64, copy: Option<&str>) -> rusqlite::Result<Record> {
    Ok(conn
        .prepare_cached("SELECT settings, settings_time, lr_develop, lr_orientation, lr_edited, lr_file FROM develop_state WHERE file = ?1 AND copy = ?2")?
        .query_row(params![file, copy_key(copy)], record)
        .optional()?
        .unwrap_or_default())
}

fn record(r: &rusqlite::Row) -> rusqlite::Result<Record> {
    Ok(Record {
        settings: r.get(0)?,
        settings_time: r.get(1)?,
        lightroom: Lightroom { develop: r.get(2)?, orientation: r.get(3)?, edited: r.get::<_, i64>(4)? != 0, file: r.get(5)? },
    })
}

/// Develop state of the original and all copies of `file` (key: copy name, `""` = original).
pub fn all_of_file(conn: &Connection, file: i64) -> rusqlite::Result<HashMap<String, Record>> {
    let mut st = conn.prepare_cached("SELECT copy, settings, settings_time, lr_develop, lr_orientation, lr_edited, lr_file FROM develop_state WHERE file = ?1")?;
    let rows = st.query_map([file], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Record {
                settings: r.get(1)?,
                settings_time: r.get(2)?,
                lightroom: Lightroom { develop: r.get(3)?, orientation: r.get(4)?, edited: r.get::<_, i64>(5)? != 0, file: r.get(6)? },
            },
        ))
    })?;
    rows.collect()
}

fn drop_empty(conn: &Connection, file: i64, copy: &str) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "DELETE FROM develop_state WHERE file = ?1 AND copy = ?2 AND settings IS NULL AND lr_develop IS NULL AND lr_orientation IS NULL AND lr_edited = 0 AND lr_file IS NULL",
    )?
    .execute(params![file, copy])?;
    Ok(())
}

/// Set the own develop settings (`None` = none, the starting values apply).
pub fn put_settings(conn: &Connection, file: i64, copy: Option<&str>, settings: Option<&str>, time: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO develop_state(file, copy, settings, settings_time) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(file, copy) DO UPDATE SET settings = excluded.settings, settings_time = excluded.settings_time",
    )?
    .execute(params![file, copy_key(copy), settings, time])?;
    drop_empty(conn, file, copy_key(copy))
}

/// Set the Lightroom data.
pub fn put_lightroom(conn: &Connection, file: i64, copy: Option<&str>, lr: &Lightroom) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO develop_state(file, copy, lr_develop, lr_orientation, lr_edited, lr_file) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(file, copy) DO UPDATE SET lr_develop = excluded.lr_develop, lr_orientation = excluded.lr_orientation,
            lr_edited = excluded.lr_edited, lr_file = excluded.lr_file",
    )?
    .execute(params![file, copy_key(copy), lr.develop, lr.orientation, lr.edited as i64, lr.file])?;
    drop_empty(conn, file, copy_key(copy))
}

/// Rating (stars, flag, label) of the original or a copy; `None`: no such image.
pub fn rating(conn: &Connection, file: i64, copy: Option<&str>) -> Option<(u8, i8, u8)> {
    conn.prepare_cached("SELECT stars, flag, label FROM image WHERE file = ?1 AND copy IS ?2")
        .ok()?
        .query_row(params![file, copy], |r| Ok((r.get::<_, i64>(0)?.clamp(0, 5) as u8, r.get::<_, i64>(1)?.clamp(-1, 1) as i8, r.get::<_, i64>(2)?.clamp(0, 5) as u8)))
        .optional()
        .ok()
        .flatten()
}

// ─────────────────────────────────────────────── setting "sidecars next to the photos"

static WRITE_CACHE: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);

/// Write sidecars (XMP) next to the photos? Setting `writeSidecars` (default off); `GRAINEO_LIBRARY_SIDECARS=1/0` overrides it.
pub fn write_sidecars() -> bool {
    if crate::backup_view::active().is_some() {
        return false;
    }
    match std::env::var("GRAINEO_LIBRARY_SIDECARS").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    let mut c = WRITE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = *c {
        if t.elapsed() < std::time::Duration::from_secs(2) {
            return v;
        }
    }
    let v = crate::settings::value("writeSidecars").as_bool() == Some(true);
    *c = Some((std::time::Instant::now(), v));
    v
}

/// Switch writing sidecars on or off (the setting `writeSidecars`).
pub fn set_write_sidecars(on: bool) -> R<()> {
    crate::settings::set_checked("writeSidecars", serde_json::Value::Bool(on))?;
    *WRITE_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), on));
    wake_writer();
    Ok(())
}

/// Note that the sidecar of `file` has to be written from the library (only if writing sidecars is on).
pub fn queue_sidecar(conn: &Connection, file: i64) -> R<()> {
    if !write_sidecars() {
        return Ok(());
    }
    enqueue(conn, file)
}

/// Queue the sidecar of `file` regardless of the setting.
pub fn enqueue(conn: &Connection, file: i64) -> R<()> {
    conn.execute("INSERT OR IGNORE INTO pending_sidecar_write(file, since) VALUES(?1, ?2)", params![file, crate::now()]).map_err(f)?;
    wake_writer();
    Ok(())
}

/// Queue the sidecars of all photos the library knows something about (switching writing on: write them all once).
/// Returns the number queued.
pub fn queue_all(db: &Db) -> R<usize> {
    let n = db
        .conn
        .execute(
            "INSERT OR IGNORE INTO pending_sidecar_write(file, since)
             SELECT id, ?1 FROM file WHERE format <> 'VIDEO' AND id IN (
                SELECT file FROM image WHERE stars > 0 OR flag <> 0 OR label > 0 OR copy IS NOT NULL OR faces_scanned = 1
                UNION SELECT b.file FROM image_keyword bs JOIN image b ON b.id = bs.image
                UNION SELECT file FROM develop_state
                UNION SELECT id FROM file WHERE capture_time_original IS NOT NULL)",
            [crate::now()],
        )
        .map_err(f)?;
    wake_writer();
    Ok(n)
}

/// Number of photos the library would write a sidecar for when writing is switched on (for the question beforehand).
pub fn count_all(conn: &Connection) -> usize {
    conn.query_row(
        "SELECT count(*) FROM file WHERE format <> 'VIDEO' AND id IN (
            SELECT file FROM image WHERE stars > 0 OR flag <> 0 OR label > 0 OR copy IS NOT NULL OR faces_scanned = 1
            UNION SELECT b.file FROM image_keyword bs JOIN image b ON b.id = bs.image
            UNION SELECT file FROM develop_state
            UNION SELECT id FROM file WHERE capture_time_original IS NOT NULL)",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
    .unwrap_or(0)
}

/// Sidecars still to write (status "Begleitdateien: noch N"); 0 while writing is off.
pub fn sidecars_waiting(conn: &Connection) -> usize {
    if !write_sidecars() {
        return 0;
    }
    crate::location::pending_count(conn)
}

// ─────────────────────────────────────────────── writing one sidecar from the library

/// Write the sidecar of `file` completely from the library: our fields, the copies with their settings and Lightroom
/// data, the own develop settings and the Lightroom data. Foreign fields of an existing file stay; a broken file is
/// first put aside (`.NAME.xmp.defekt-<time>`). Unchanged content is not written again. `sync`: force to disk (`fsync`).
/// `Ok(true)`: written.
pub fn write_sidecar_from_library(db: &Db, file: i64, sync: bool) -> R<bool> {
    let (path, format) = crate::sync::file_info(db, file)?;
    if format == "VIDEO" {
        return Ok(false);
    }
    let raw = crate::sync::raw_beside(&path, &format);
    if !raw && !crate::is_raw(&format) && raw_of_same_name(db, file, &path) {
        // a JPEG next to a RAW of the library that is not in the folder (e.g. on the NAS) would share its sidecar:
        // the RAW keeps it (as the Lightroom handover always did)
        return Ok(false);
    }
    let xp = crate::sidecar::path(&path, &format, raw);
    if !crate::xmp::writes_allowed(&xp) {
        return Ok(false);
    }
    if !xp.parent().is_some_and(Path::is_dir) {
        return Err(format!("{}: {UNREACHABLE}", xp.display()));
    }
    let Some((w, orig)) = sidecar_values(db, file)? else { return Ok(false) };
    let old = match std::fs::read_to_string(&xp) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", xp.display())),
    };
    let text = match crate::xmp::compose(old.as_deref(), &w, orig.settings.as_deref(), &orig.lightroom) {
        Ok(t) => t,
        Err(_) => {
            // not readable: lose nothing, put it aside and start anew
            let name = xp.file_name().unwrap_or_default().to_string_lossy();
            let safe = xp.with_file_name(format!(".{name}.defekt-{}", crate::now()));
            std::fs::rename(&xp, &safe).map_err(|e| format!("{}: {e}", xp.display()))?;
            crate::xmp::compose(None, &w, orig.settings.as_deref(), &orig.lightroom)?
        }
    };
    if old.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    let r = if sync { crate::files::write_atomic(&xp, text.as_bytes()) } else { crate::files::write_atomic_no_sync(&xp, text.as_bytes()) };
    r.map_err(|e| format!("{}: {e}", xp.display()))?;
    Ok(true)
}

/// What the sidecar of `file` holds, from the library: the field values with the copies (their settings and Lightroom
/// data), and the record of the original (own develop settings, Lightroom data). `None`: not in the library.
fn sidecar_values(db: &Db, file: i64) -> R<Option<(crate::xmp::Values, Record)>> {
    let Some((_, mut w)) = crate::sync::db_values(db, file)? else { return Ok(None) };
    let recs = all_of_file(&db.conn, file).map_err(f)?;
    let mut copies = crate::sidecar::read_copies(w.copies_json.as_deref());
    for k in &mut copies {
        if let Some(r) = recs.get(&k.name) {
            k.settings = r.settings.as_deref().and_then(|j| serde_json::from_str(j).ok());
            k.lightroom = r.lightroom.develop.clone();
            k.lightroom_orientation = r.lightroom.orientation.clone();
        }
    }
    w.copies_json = crate::sidecar::copies_text(&copies);
    let orig = recs.get("").cloned().unwrap_or_default();
    Ok(Some((w, orig)))
}

/// XMP sidecar text of `file` as it is next to the photo when sidecars are written (export "Original": goes along
/// with the copied file). Starts from `old` (an existing sidecar; its foreign fields stay), or anew if that is not
/// readable. `None`: no sidecar for this file (video, not in the library).
pub fn sidecar_text(db: &Db, file: i64, old: Option<&str>) -> R<Option<String>> {
    let Some((w, orig)) = sidecar_values(db, file)? else { return Ok(None) };
    let text = crate::xmp::compose(old, &w, orig.settings.as_deref(), &orig.lightroom).or_else(|_| crate::xmp::compose(None, &w, orig.settings.as_deref(), &orig.lightroom))?;
    Ok(Some(text))
}

/// Has the library a RAW with the same name in the same folder as `file`?
fn raw_of_same_name(db: &Db, file: i64, path: &Path) -> bool {
    let stem = path.file_stem().map(|s| crate::nfc(&s.to_string_lossy()).to_lowercase()).unwrap_or_default();
    let Ok(mut st) = db.conn.prepare_cached("SELECT name, format FROM file WHERE folder = (SELECT folder FROM file WHERE id = ?1) AND id <> ?1") else { return false };
    let rows: Vec<(String, String)> = st.query_map([file], |r| Ok((r.get(0)?, r.get(1)?))).map(|r| r.flatten().collect()).unwrap_or_default();
    rows.iter().any(|(n, f)| crate::is_raw(f) && Path::new(n).file_stem().is_some_and(|s| crate::nfc(&s.to_string_lossy()).to_lowercase() == stem))
}

// ─────────────────────────────────────────────── background writer of the sidecars

static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Let the background writer look at its queue now.
pub fn start_writer_round() {
    wake_writer();
}

fn wake_writer() {
    let (m, c) = &WAKE;
    *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
    c.notify_all();
}

fn wait_wake(d: std::time::Duration) {
    let (m, c) = &WAKE;
    let g = m.lock().unwrap_or_else(|e| e.into_inner());
    let (mut g, _) = c.wait_timeout_while(g, d, |w| !*w).unwrap_or_else(|e| e.into_inner());
    *g = false;
}

/// Outcome of one round of the sidecar writer.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct WriteRound {
    pub written: usize,
    pub unchanged: usize,
    pub errors: Vec<String>,
    /// still queued: its folder is not reachable right now
    pub waiting: usize,
    /// last entry looked at (`since`, `file`): the next round continues after it
    pub last: Option<(i64, i64)>,
}

/// Write up to `max` queued sidecars of reachable locations (files whose sidecar has not been read yet wait for that).
/// `pause` is called between files (background work steps back while the user works).
pub fn write_queued(db: &Db, max: usize, sync: bool, pause: &dyn Fn()) -> R<WriteRound> {
    write_queued_after(db, max, sync, pause, None)
}

/// [`write_queued`], continuing after the entry `after` (`since`, `file`) of an earlier round: entries left waiting
/// (folder renamed or gone while its root is reachable) do not block the queue – before 0.108 fifty of them at the
/// front stopped every further sidecar for good.
pub fn write_queued_after(db: &Db, max: usize, sync: bool, pause: &dyn Fn(), after: Option<(i64, i64)>) -> R<WriteRound> {
    let mut out = WriteRound::default();
    let roots: Vec<(i64, String)> = {
        let mut st = db.conn.prepare("SELECT id, path FROM root_folder").map_err(f)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.flatten().collect();
        r
    };
    let reachable: Vec<String> = roots.into_iter().filter(|(_, p)| crate::location::root_reachable(p)).map(|(id, _)| id.to_string()).collect();
    if reachable.is_empty() {
        return Ok(out);
    }
    let files: Vec<(i64, i64)> = {
        let sql = format!(
            "SELECT p.file, p.since FROM pending_sidecar_write p JOIN file d ON d.id = p.file JOIN folder o ON o.id = d.folder
             WHERE o.root IN ({}) AND d.xmp_read IS NOT NULL AND (p.since, p.file) > (?1, ?2) ORDER BY p.since, p.file LIMIT {max}",
            reachable.join(",")
        );
        let (a, b) = after.unwrap_or((i64::MIN, i64::MIN));
        let mut st = db.conn.prepare(&sql).map_err(f)?;
        let r = st.query_map([a, b], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.flatten().collect();
        r
    };
    for (file, since) in files {
        out.last = Some((since, file));
        pause();
        match write_sidecar_from_library(db, file, sync) {
            Ok(true) => out.written += 1,
            Ok(false) => out.unchanged += 1,
            Err(e) => {
                // the file stays queued while its root or its folder is unreachable (a network drive asleep, a
                // folder renamed for a moment); other errors are reported once
                if crate::location::file_reachable(db, file) && !e.ends_with(UNREACHABLE) {
                    out.errors.push(e);
                } else {
                    out.waiting += 1;
                    continue;
                }
            }
        }
        db.conn.execute("DELETE FROM pending_sidecar_write WHERE file = ?1", [file]).map_err(f)?;
    }
    Ok(out)
}

static WRITER_STARTED: std::sync::Once = std::sync::Once::new();

/// Start the background writer of the sidecars (once per process): QoS background, in batches, `pause` before every
/// file (the app passes its "wait while the user works"). It sleeps while writing is off or nothing is queued.
pub fn start_sidecar_writer(db_path: PathBuf, pause: fn()) {
    WRITER_STARTED.call_once(move || {
        let _ = std::thread::Builder::new().name("begleitdateien-schreiber".into()).spawn(move || {
            crate::platform::background_priority();
            let mut db: Option<Db> = None;
            loop {
                wait_wake(std::time::Duration::from_secs(30));
                if !write_sidecars() || !db_path.exists() {
                    continue;
                }
                if db.is_none() {
                    db = Db::open(&db_path).ok();
                }
                let Some(d) = db.as_ref() else { continue };
                let mut after = None;
                loop {
                    match write_queued_after(d, 50, false, &|| pause(), after) {
                        Ok(r) => {
                            for e in r.errors.iter().take(3) {
                                eprintln!("sidecar not written: {e}");
                            }
                            if r.last.is_none() || !write_sidecars() {
                                break;
                            }
                            after = r.last;
                        }
                        Err(e) => {
                            eprintln!("sidecar writer: {e}");
                            break;
                        }
                    }
                }
            }
        });
    });
}

// ─────────────────────────────────────────────── access by path (app)

thread_local! {
    static CONN: RefCell<Option<(PathBuf, Connection)>> = const { RefCell::new(None) };
}

/// Run `f` with this thread's read connection to the library (`None`: there is no library or it is too old).
pub(crate) fn with_conn<T>(f: impl FnOnce(&Connection) -> Option<T>) -> Option<T> {
    let p = crate::db_path();
    CONN.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_none_or(|(q, _)| *q != p) {
            *c = None;
            if !p.exists() {
                return None;
            }
            // never creates the file; reads only
            let conn = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
            let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
            let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).ok()?;
            if v < 15 {
                return None;
            }
            let _ = conn.pragma_update(None, "query_only", "ON");
            *c = Some((p.clone(), conn));
        }
        f(&c.as_ref()?.1)
    })
}

/// The library's file of a photo path: (file id, its sidecar has been read once). `None`: not in the library
/// (then the sidecar next to it is used, as before).
pub fn locate(image: &Path) -> Option<(i64, bool)> {
    let dir = format!("{}/", image.parent()?.to_string_lossy().trim_end_matches('/'));
    let name = crate::nfc(&image.file_name()?.to_string_lossy());
    let dir = crate::nfc(&dir);
    with_conn(|c| {
        c.prepare_cached(
            "SELECT d.id, d.xmp_read IS NOT NULL FROM root_folder s
             JOIN folder o ON o.root = s.id AND o.path = substr(?1, length(s.path) + 1)
             JOIN file d ON d.folder = o.id AND d.name = ?2
             WHERE substr(?1, 1, length(s.path)) = s.path LIMIT 1",
        )
        .ok()?
        .query_row(params![dir, name], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)))
        .optional()
        .ok()
        .flatten()
    })
}

#[derive(Clone, Debug, Default)]
struct Pending {
    /// `Some(x)`: settings to write (`x = None`: remove them)
    settings: Option<Option<String>>,
    meta: Option<(u8, i8, u8)>,
    generation: u64,
    /// changes of the develop history (numbered), written in the same transaction as the settings
    history: Vec<(u64, crate::develop_history::Op)>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.settings.is_none() && self.meta.is_none() && self.history.is_empty()
    }
}

static OVERLAY: Mutex<Option<HashMap<(i64, String), Pending>>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Number of the last queued history change.
static HISTORY_OP: AtomicU64 = AtomicU64::new(0);
static OPEN_JOBS: AtomicUsize = AtomicUsize::new(0);
static SENDER: Mutex<Option<Sender<(PathBuf, Vec<(i64, String, u64)>)>>> = Mutex::new(None);
/// Changes that could not be written after several tries (file, copy key); they stay in the overlay (reads see them)
/// and are tried again with the next change or [`retry_unsaved`].
static UNSAVED: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());
/// Last error of the writer (for a visible message), cleared after a successful write.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
/// Rounds a change failed on a constraint of the library (file, copy key → count). Such an error does not go away by
/// waiting: after [`CONSTRAINT_ROUNDS`] the queued history changes are dropped, after twice as many the whole change.
static CONSTRAINT_FAILS: Mutex<Option<HashMap<(i64, String), u32>>> = Mutex::new(None);
/// Rounds of constraint errors before queued history changes of a photo are dropped.
const CONSTRAINT_ROUNDS: u32 = 3;

/// Is `e` an error that a later try cannot fix (a foreign key or other constraint of the library)?
fn is_constraint_error(e: &str) -> bool {
    e.contains("constraint failed")
}

/// A change failed on a constraint once more: drop what cannot be written (history changes first, then the whole
/// change) after a bounded number of rounds. `true`: something is left to try again.
fn constraint_failed(file: i64, copy: &str, e: &str) -> bool {
    let n = {
        let mut g = CONSTRAINT_FAILS.lock().unwrap_or_else(|x| x.into_inner());
        let c = g.get_or_insert_with(HashMap::new).entry((file, copy.to_string())).or_insert(0);
        *c += 1;
        *c
    };
    let key = (file, copy.to_string());
    if n >= 2 * CONSTRAINT_ROUNDS {
        eprintln!("develop change of file {file} dropped after {n} rounds: {e}");
        overlay(|o| o.remove(&key));
        forget_constraint_fails(file, copy);
        return false;
    }
    if n >= CONSTRAINT_ROUNDS {
        let dropped = overlay(|o| {
            let Some(p) = o.get_mut(&key) else { return 0 };
            let k = p.history.len();
            p.history.clear();
            if p.is_empty() {
                o.remove(&key);
            }
            k
        });
        if dropped > 0 {
            eprintln!("develop history of file {file}: {dropped} queued changes dropped after {n} rounds: {e}");
        }
    }
    overlay(|o| o.contains_key(&key))
}

fn forget_constraint_fails(file: i64, copy: &str) {
    if let Some(m) = CONSTRAINT_FAILS.lock().unwrap_or_else(|x| x.into_inner()).as_mut() {
        m.remove(&(file, copy.to_string()));
    }
}

/// Error ending of a sidecar write whose folder is not reachable right now (the write stays queued).
const UNREACHABLE: &str = "Ordner nicht erreichbar";

fn overlay<T>(f: impl FnOnce(&mut HashMap<(i64, String), Pending>) -> T) -> T {
    let mut g = OVERLAY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Own develop settings (JSON) of a photo of the library (original: `copy = None`), including a change not yet written.
pub fn develop(file: i64, copy: Option<&str>) -> Option<String> {
    if let Some(s) = overlay(|o| o.get(&(file, copy_key(copy).to_string())).and_then(|p| p.settings.clone())) {
        return s;
    }
    with_conn(|c| get(c, file, copy).ok()).and_then(|r| r.settings)
}

/// Has the library its own entry for this photo/copy (develop state or a change on its way)?
fn has_entry(file: i64, copy: Option<&str>) -> bool {
    overlay(|o| o.get(&(file, copy_key(copy).to_string())).is_some_and(|p| p.settings.is_some()))
        || with_conn(|c| c.query_row("SELECT 1 FROM develop_state WHERE file = ?1 AND copy = ?2", params![file, copy_key(copy)], |_| Ok(())).optional().ok()).flatten().is_some()
}

/// Own develop settings by path. Photo of the library: from the library – while its sidecar has not been read yet
/// (first start after the update) and the library has nothing of its own, from the sidecar. Photo outside the
/// library: from the sidecar. `loc`: result of [`locate`]. `Err`: sidecar present but not readable.
pub fn develop_at(image: &Path, loc: Option<(i64, bool)>, copy: Option<&str>) -> R<Option<String>> {
    if let Some((file, read)) = loc {
        if read || has_entry(file, copy) {
            return Ok(develop(file, copy));
        }
    }
    let xp = crate::xmp::path_to(image);
    match copy {
        None => crate::xmp::read_develop(&xp),
        Some(k) => match crate::xmp::read(&xp) {
            crate::xmp::ReadResult::Ok(w) => Ok(crate::sidecar::read_copies(w.copies_json.as_deref()).into_iter().find(|x| x.name == k).and_then(|x| x.settings).map(|v| v.to_string())),
            crate::xmp::ReadResult::Missing => Ok(None),
            crate::xmp::ReadResult::Broken => Err(format!("{} ist nicht lesbar", xp.display())),
        },
    }
}

/// Rating of a photo or copy of the library, including a change not yet written; `None`: no such copy.
pub fn meta(file: i64, copy: Option<&str>) -> Option<(u8, i8, u8)> {
    if let Some(m) = overlay(|o| o.get(&(file, copy_key(copy).to_string())).and_then(|p| p.meta)) {
        return Some(m);
    }
    with_conn(|c| rating(c, file, copy))
}

/// Lightroom data of a photo of the library.
pub fn lightroom_of(file: i64, copy: Option<&str>) -> Lightroom {
    with_conn(|c| get(c, file, copy).ok()).map(|r| r.lightroom).unwrap_or_default()
}

/// Lightroom data of a photo by path: from the library, for photos outside it (or not read yet) from the sidecar.
pub fn lightroom_for(image: &Path, copy: Option<&str>) -> Lightroom {
    match locate(image) {
        Some((file, read)) if read || has_entry(file, copy) => lightroom_of(file, copy),
        _ => {
            let Some(t) = crate::xmp::text(&crate::xmp::path_to(image)) else { return Lightroom::default() };
            let lr = crate::xmp::lightroom_from_text(&t);
            if !lr.belongs_to(image) {
                return Lightroom::default();
            }
            match copy {
                None => lr,
                Some(k) => crate::xmp::read_text(&t)
                    .and_then(|w| crate::sidecar::read_copies(w.copies_json.as_deref()).into_iter().find(|x| x.name == k))
                    .map(|x| Lightroom { develop: x.lightroom, orientation: x.lightroom_orientation, ..Default::default() })
                    .unwrap_or_default(),
            }
        }
    }
}

/// Own develop settings of the original by path: from the library, for photos outside it (or not read yet) from the sidecar.
pub fn develop_for(image: &Path) -> Option<String> {
    develop_at(image, locate(image), None).ok().flatten()
}

fn send(file: i64, copy: Option<&str>, change: impl FnOnce(&mut Pending)) {
    let g = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let key = (file, copy_key(copy).to_string());
    overlay(|o| {
        let p = o.entry(key.clone()).or_default();
        change(p);
        p.generation = g;
    });
    to_writer(key.0, key.1, g);
    // changes that failed before go along (the library may be free again)
    retry_unsaved();
}

thread_local! {
    /// Changes collected by [`batch`] on this thread (written together in one transaction).
    static BATCH: RefCell<Option<Vec<(i64, String, u64)>>> = const { RefCell::new(None) };
}

/// Run `f`; the changes it saves ([`set_develop`], [`set_meta`]) go to the writer together and are written in **one**
/// transaction (many photos at once, e.g. a block of a multi-selection edit). Reads see them right away as usual.
pub fn batch<T>(f: impl FnOnce() -> T) -> T {
    if BATCH.with(|b| b.borrow().is_some()) {
        return f();
    }
    BATCH.with(|b| *b.borrow_mut() = Some(Vec::new()));
    struct Hand;
    impl Drop for Hand {
        fn drop(&mut self) {
            let items = BATCH.with(|b| b.borrow_mut().take()).unwrap_or_default();
            if !items.is_empty() {
                send_to_writer(items);
            }
        }
    }
    let _hand = Hand;
    f()
}

/// Hand one change to the writer thread (started, or started again if it ended).
fn to_writer(file: i64, copy: String, g: u64) {
    OPEN_JOBS.fetch_add(1, Ordering::SeqCst);
    let collected = BATCH.with(|b| match b.borrow_mut().as_mut() {
        Some(v) => {
            v.push((file, copy.clone(), g));
            true
        }
        None => false,
    });
    if !collected {
        send_to_writer(vec![(file, copy, g)]);
    }
}

/// Hand changes (already counted in `OPEN_JOBS`) to the writer thread.
fn send_to_writer(items: Vec<(i64, String, u64)>) {
    let mut s = SENDER.lock().unwrap_or_else(|e| e.into_inner());
    let mut job = (crate::db_path(), items);
    for _ in 0..2 {
        if s.is_none() {
            *s = start_writer();
        }
        let Some(tx) = s.as_ref() else { break };
        match tx.send(job) {
            Ok(()) => return,
            Err(e) => {
                // the thread is gone: start a new one
                job = e.0;
                *s = None;
            }
        }
    }
    OPEN_JOBS.fetch_sub(job.1.len(), Ordering::SeqCst);
    for (file, copy, _) in job.1 {
        remember_unsaved(file, copy, "Schreib-Thread nicht gestartet".into());
    }
}

fn remember_unsaved(file: i64, copy: String, e: String) {
    eprintln!("develop state not saved: {e}");
    *LAST_ERROR.lock().unwrap_or_else(|x| x.into_inner()) = Some(e);
    let mut u = UNSAVED.lock().unwrap_or_else(|x| x.into_inner());
    if !u.iter().any(|(f, c)| (*f, c) == (file, &copy)) {
        u.push((file, copy));
    }
}

/// The writer thread: one connection, every change tried a few times (the library may be locked for a while by a
/// sync or import); a panic in one write does not end the thread.
fn start_writer() -> Option<Sender<(PathBuf, Vec<(i64, String, u64)>)>> {
    let (tx, rx) = channel::<(PathBuf, Vec<(i64, String, u64)>)>();
    let spawned = std::thread::Builder::new().name("entwicklung-speichern".into()).spawn(move || {
        let mut db: Option<Db> = None;
        for (path, items) in rx {
            let mut last = String::new();
            for wait in [0u64, 1, 3, 8] {
                std::thread::sleep(std::time::Duration::from_secs(wait));
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> R<()> {
                    if db.as_ref().is_none_or(|d| d.path != path) {
                        db = Some(Db::open(&path).map_err(|e| e.to_string())?);
                    }
                    apply_all(db.as_ref().expect("opened above"), &items)
                }));
                match r {
                    Ok(Ok(())) => {
                        last.clear();
                        break;
                    }
                    Ok(Err(e)) if is_constraint_error(&e) => {
                        // waiting does not help: handled below without further tries
                        last = e;
                        break;
                    }
                    Ok(Err(e)) => last = e,
                    Err(_) => {
                        // the connection may be in an odd state after a panic: open a new one
                        db = None;
                        last = "interner Fehler beim Speichern".into();
                    }
                }
            }
            if last.is_empty() {
                *LAST_ERROR.lock().unwrap_or_else(|x| x.into_inner()) = None;
                for (file, copy, _) in &items {
                    forget_constraint_fails(*file, copy);
                }
            } else if is_constraint_error(&last) {
                for (file, copy, _) in &items {
                    if constraint_failed(*file, copy, &last) {
                        remember_unsaved(*file, copy.clone(), last.clone());
                    }
                }
            } else {
                for (file, copy, _) in &items {
                    remember_unsaved(*file, copy.clone(), last.clone());
                }
            }
            OPEN_JOBS.fetch_sub(items.len(), Ordering::SeqCst);
        }
    });
    spawned.ok().map(|_| tx)
}

/// Changes that could not be written yet (they are kept and tried again).
pub fn unsaved() -> usize {
    UNSAVED.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Last error while saving (cleared after the next successful write).
pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Try the changes that could not be written again (e.g. before quitting).
pub fn retry_unsaved() {
    let g = GENERATION.load(Ordering::SeqCst);
    let list = std::mem::take(&mut *UNSAVED.lock().unwrap_or_else(|e| e.into_inner()));
    for (file, copy) in list {
        // the overlay still holds the newest change; its generation decides whether it is removed after writing
        let gen = overlay(|o| o.get(&(file, copy.clone())).map(|p| p.generation)).unwrap_or(g);
        to_writer(file, copy, gen);
    }
}

/// Write changes: one alone as before, several in one transaction (if that fails, one by one).
fn apply_all(db: &Db, items: &[(i64, String, u64)]) -> R<()> {
    if let [(file, copy, g)] = items {
        return apply(db, *file, copy, *g).map(|_| ());
    }
    db.conn.execute_batch("BEGIN IMMEDIATE").map_err(f)?;
    match items.iter().map(|(file, copy, g)| apply(db, *file, copy, *g)).collect::<R<Vec<u64>>>() {
        Ok(ops) => {
            if let Err(e) = db.conn.execute_batch("COMMIT") {
                let _ = db.conn.execute_batch("ROLLBACK");
                return Err(f(e));
            }
            COMMITS.fetch_add(1, Ordering::Relaxed);
            for ((file, copy, g), op) in items.iter().zip(ops) {
                written(*file, copy, *g, op);
            }
            Ok(())
        }
        Err(_) => {
            // e.g. a sidecar still to be taken over first (needs a transaction of its own): one by one
            let _ = db.conn.execute_batch("ROLLBACK");
            items.iter().try_for_each(|(file, copy, g)| apply(db, *file, copy, *g).map(|_| ()))
        }
    }
}

/// A change reached the library: settings and rating leave the overlay unless newer ones came meanwhile, history
/// changes up to number `history_op` leave it in any case.
fn written(file: i64, copy: &str, generation: u64, history_op: u64) {
    overlay(|o| {
        let key = (file, copy.to_string());
        let Some(p) = o.get_mut(&key) else { return };
        if p.generation == generation {
            p.settings = None;
            p.meta = None;
        }
        p.history.retain(|(n, _)| *n > history_op);
        if p.is_empty() {
            o.remove(&key);
        }
    });
}

/// Write one change (inside a transaction of [`apply_all`], or in its own). Returns the number of the last history
/// change written (0: none).
fn apply(db: &Db, file: i64, copy: &str, generation: u64) -> R<u64> {
    let p = overlay(|o| o.get(&(file, copy.to_string())).cloned());
    let Some(p) = p else { return Ok(0) };
    // the photo left the library meanwhile (removed, or merged into another one by a move): nothing of it can be
    // written any more; dropping it keeps the writer from failing on the foreign key again and again
    let exists = db.conn.query_row("SELECT 1 FROM file WHERE id = ?1", [file], |_| Ok(())).optional().map_err(f)?.is_some();
    if !exists {
        eprintln!("develop change of file {file} dropped: no longer in the library");
        overlay(|o| o.remove(&(file, copy.to_string())));
        return Ok(0);
    }
    // a sidecar not read yet: take it over first (it must not overwrite this change later)
    let unread: bool = db.conn.query_row("SELECT xmp_read IS NULL FROM file WHERE id = ?1", [file], |r| r.get(0)).optional().map_err(f)?.unwrap_or(false);
    if unread {
        crate::sidecar_import::import_files(db, &[file]).map_err(|e| e.to_string())?;
    }
    let now = crate::now();
    let copy_opt = (!copy.is_empty()).then_some(copy);
    let own = db.conn.is_autocommit();
    let tx = if own { Some(db.conn.unchecked_transaction().map_err(f)?) } else { None };
    if let Some(s) = &p.settings {
        put_settings(&db.conn, file, copy_opt, s.as_deref(), now).map_err(f)?;
        db.conn.execute("UPDATE image SET changed = ?3 WHERE file = ?1 AND copy IS ?2", params![file, copy_opt, now]).map_err(f)?;
    }
    if let Some((s, m, e)) = p.meta {
        db.conn
            .execute("UPDATE image SET stars = ?3, flag = ?4, label = ?5, rating_time = ?6, changed = ?6 WHERE file = ?1 AND copy IS ?2", params![file, copy_opt, s, m, e, now])
            .map_err(f)?;
    }
    let last_op = p.history.last().map_or(0, |(n, _)| *n);
    if !p.history.is_empty() {
        // errors in the history never hold back the settings: its operations in a savepoint, on an error rolled back,
        // logged and dropped (the next load of the history finds the difference and records it as "changed elsewhere")
        let ops: Vec<crate::develop_history::Op> = p.history.iter().map(|(_, op)| op.clone()).collect();
        db.conn.execute_batch("SAVEPOINT history_ops").map_err(f)?;
        match crate::develop_history::apply_ops(&db.conn, file, copy_opt, &ops) {
            Ok(()) => db.conn.execute_batch("RELEASE history_ops").map_err(f)?,
            Err(e) => {
                eprintln!("develop history of file {file}: {} operations not written: {e}", ops.len());
                db.conn.execute_batch("ROLLBACK TO history_ops; RELEASE history_ops").map_err(f)?;
            }
        }
    }
    if p.settings.is_some() || p.meta.is_some() {
        queue_sidecar(&db.conn, file)?;
    }
    if let Some(tx) = tx {
        tx.commit().map_err(f)?;
        COMMITS.fetch_add(1, Ordering::Relaxed);
        written(file, copy, generation, last_op);
    }
    Ok(last_op)
}

/// Save the own develop settings of a photo of the library (in the background; reads see it right away).
pub fn set_develop(file: i64, copy: Option<&str>, settings: Option<String>) {
    send(file, copy, |p| p.settings = Some(settings));
}

/// Queue changes of the develop history of a photo of the library. They go to the library with the next save of
/// its settings ([`set_develop`]) in the same transaction, or with [`send_history`]; reads ([`load_history`]) see them
/// right away.
pub fn queue_history(file: i64, copy: Option<&str>, ops: Vec<crate::develop_history::Op>) {
    if ops.is_empty() {
        return;
    }
    overlay(|o| {
        let p = o.entry((file, copy_key(copy).to_string())).or_default();
        for op in ops {
            p.history.push((HISTORY_OP.fetch_add(1, Ordering::SeqCst) + 1, op));
        }
    });
}

/// Drop the history changes queued for a photo or copy that is removed (a new copy of the same name must not inherit
/// them). Settings and rating still queued stay (they are written or fail on their own).
pub(crate) fn drop_queued_history(file: i64, copy: Option<&str>) {
    overlay(|o| {
        let key = (file, copy_key(copy).to_string());
        if let Some(p) = o.get_mut(&key) {
            p.history.clear();
            if p.is_empty() {
                o.remove(&key);
            }
        }
    });
}

/// Hand every queued history change to the writer (after the settings were saved, or there was nothing to save).
pub fn send_history() {
    let keys: Vec<(i64, String, u64)> = overlay(|o| o.iter().filter(|(_, p)| !p.history.is_empty()).map(|((f, c), p)| (*f, c.clone(), p.generation)).collect());
    if keys.is_empty() {
        return;
    }
    batch(|| {
        for (file, copy, g) in keys {
            to_writer(file, copy, g);
        }
    });
}

/// Develop history of a photo of the library: steps and cursor (`seq`, `None`: on the newest step), including changes
/// not written yet. `None`: no library or the table cannot be read.
pub fn load_history(file: i64, copy: Option<&str>) -> Option<(Vec<crate::develop_history::Step>, Option<i64>)> {
    let (mut steps, mut cursor) = with_conn(|c| crate::develop_history::load(c, file, copy).ok())?;
    let ops: Vec<crate::develop_history::Op> = overlay(|o| o.get(&(file, copy_key(copy).to_string())).map(|p| p.history.iter().map(|(_, op)| op.clone()).collect()).unwrap_or_default());
    crate::develop_history::apply_ops_mem(&mut steps, &mut cursor, &ops);
    Some((steps, cursor))
}

/// A command for several photos changed photo `file`/`copy` from `before` to `after` (stored JSON of the settings):
/// queue its step ([`crate::develop_history::plan_command`]); it is written with the photo's settings. Nothing while
/// keeping the history is off.
#[allow(clippy::too_many_arguments)]
pub fn command_step(file: i64, copy: Option<&str>, before: &serde_json::Value, after: &serde_json::Value, kind: &str, label: &serde_json::Value, command: i64, undo: bool) {
    if !save_history() {
        return;
    }
    let (steps, cursor) = load_history(file, copy).unwrap_or_default();
    let ops = crate::develop_history::plan_command(&steps, cursor, before, after, kind, label, command, undo, crate::now());
    queue_history(file, copy, ops);
}

/// Hand the queued history changes of one photo to the writer (inside [`batch`]: in its transaction).
pub fn send_history_of(file: i64, copy: Option<&str>) {
    let g = overlay(|o| o.get(&(file, copy_key(copy).to_string())).filter(|p| !p.history.is_empty()).map(|p| p.generation));
    if let Some(g) = g {
        to_writer(file, copy_key(copy).to_string(), g);
    }
}

/// Lightroom's develop history of a photo of the library (name and time of every step, oldest first), read from the
/// chosen catalog strictly read-only ([`crate::lightroom::open_catalog`]) – never stored, only shown. `None`: not from
/// Lightroom, no catalog chosen or not readable. Reads the catalog file: for background threads.
pub fn lr_history(file: i64, copy: Option<&str>) -> Option<Vec<(String, i64)>> {
    let (lr_id, catalog) = with_conn(|c| {
        let id: i64 = c.prepare_cached("SELECT lr_id FROM image WHERE file = ?1 AND copy IS ?2").ok()?.query_row(params![file, copy], |r| r.get::<_, Option<i64>>(0)).optional().ok().flatten().flatten()?;
        let cat: String = c.query_row("SELECT value FROM info WHERE name = 'lightroom_catalog'", [], |r| r.get::<_, Option<String>>(0)).optional().ok().flatten().flatten()?;
        Some((id, cat))
    })?;
    let path = PathBuf::from(catalog.trim());
    if !path.is_file() {
        return None;
    }
    let conn = crate::lightroom::open_catalog(&path).ok()?;
    crate::lightroom::develop_history(&conn, lr_id).ok()
}

/// Last change of the photo in Lightroom (`image.lr_touch`, Unix seconds), if known.
pub fn lr_touch(file: i64, copy: Option<&str>) -> Option<i64> {
    with_conn(|c| c.prepare_cached("SELECT lr_touch FROM image WHERE file = ?1 AND copy IS ?2").ok()?.query_row(params![file, copy], |r| r.get::<_, Option<i64>>(0)).optional().ok().flatten().flatten())
}

static HISTORY_CACHE: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);

/// Keep the develop history in the library? Setting `saveHistory` (default on). Off: the history lives only for the
/// session, as before. Never while a backup is viewed.
pub fn save_history() -> bool {
    if crate::backup_view::active().is_some() {
        return false;
    }
    let mut c = HISTORY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = *c {
        if t.elapsed() < std::time::Duration::from_secs(2) {
            return v;
        }
    }
    let v = crate::settings::value("saveHistory").as_bool() != Some(false);
    *c = Some((std::time::Instant::now(), v));
    v
}

/// Switch keeping the develop history on or off (setting `saveHistory`).
pub fn set_save_history(on: bool) -> R<()> {
    crate::settings::set_checked("saveHistory", serde_json::Value::Bool(on))?;
    *HISTORY_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), on));
    Ok(())
}

/// Remove every develop history of the library (the user's "Delete all histories …", after a question): queued
/// changes are dropped first, then the tables are emptied. Returns the steps removed.
pub fn forget_all_histories(db: &Db) -> R<usize> {
    overlay(|o| {
        for p in o.values_mut() {
            p.history.clear();
        }
        o.retain(|_, p| !p.is_empty());
    });
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let n = crate::develop_history::forget_all(&tx).map_err(f)?;
    tx.commit().map_err(f)?;
    Ok(n)
}

/// Save the rating of a photo or copy of the library (in the background; reads see it right away).
pub fn set_meta(file: i64, copy: Option<&str>, m: (u8, i8, u8)) {
    send(file, copy, |p| p.meta = Some(m));
}

/// Wait until every saved change has reached the library (quitting, tests). At most `max` long; `true`: all written.
pub fn flush(max: std::time::Duration) -> bool {
    let t = std::time::Instant::now();
    while OPEN_JOBS.load(Ordering::SeqCst) > 0 {
        if t.elapsed() >= max {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    unsaved() == 0
}

/// Write transactions of the develop-state writer so far (measurements, load tests).
static COMMITS: AtomicU64 = AtomicU64::new(0);

pub fn commits() -> u64 {
    COMMITS.load(Ordering::Relaxed)
}

pub fn reset_commits() {
    COMMITS.store(0, Ordering::Relaxed);
}

/// Are changes still on their way into the library?
pub fn saving() -> bool {
    OPEN_JOBS.load(Ordering::SeqCst) > 0
}
