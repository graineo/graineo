//! Reading the sidecars next to the photos into the library ([`crate::store`]: the library is the main store).
//!
//! - Once per file (`file.xmp_read`): after the update to schema 15 for every photo, later for new photos and after a
//!   rebuild. Runs in the background at the lowest priority, in small blocks, and continues where it stopped; photos on
//!   a location that is not reachable right now come when it is back ([`start_background_import`]).
//! - On request ("Begleitdateien einlesen …", [`run`]): all reachable sidecars again – changes by other programs
//!   come in where the sidecar is newer than the library's state.
//!
//! Taken over: rating, flag, label, keywords, virtual copies, corrected capture time (as the sync always did: per
//! group the newer state wins), the own develop settings (original and copies; newer than the library's state) and the
//! Lightroom data (`graineo:Lightroom…`, also of copies). Nothing is written to the photos' folders.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rusqlite::params;

use crate::db::Db;
use crate::store;
use crate::xmp::{Lightroom, ReadResult};

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// A sidecar as read from disk (outside the write lock).
struct Content {
    result: (ReadResult, Option<i64>),
    develop: Option<String>,
    lightroom: Lightroom,
}

/// File of the library to read.
struct Item {
    id: i64,
    path: PathBuf,
    format: String,
}

/// Lower-case names of a folder's files (one directory listing per folder instead of one access per photo – NAS), and
/// the stems of its RAWs (for "a RAW beside it": before 0.108 every photo cloned the whole listing and went through
/// all names – quadratic per folder, minutes for a flat folder of 20 000 photos).
#[derive(Default)]
pub(crate) struct Listings(HashMap<PathBuf, Option<Listing>>);

struct Listing {
    names: HashSet<String>,
    raw_stems: HashSet<String>,
}

impl Listings {
    fn of(&mut self, dir: &Path) -> Option<&Listing> {
        self.0
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let names: HashSet<String> = std::fs::read_dir(dir).ok()?.flatten().map(|e| crate::nfc(&e.file_name().to_string_lossy()).to_lowercase()).collect();
                let raw_stems = names.iter().filter(|x| crate::format_from_extension(x).is_some_and(crate::is_raw)).filter_map(|x| Path::new(x).file_stem().map(|s| s.to_string_lossy().into_owned())).collect();
                Some(Listing { names, raw_stems })
            })
            .as_ref()
    }
}

fn raw_beside(path: &Path, format: &str, listing: Option<&Listing>) -> bool {
    if crate::is_raw(format) {
        return false;
    }
    let stem = path.file_stem().map(|s| crate::nfc(&s.to_string_lossy()).to_lowercase()).unwrap_or_default();
    listing.is_some_and(|l| l.raw_stems.contains(&stem))
}

/// Read the sidecar of a photo. Inner `None`: there is none. Outer `None`: the photo's folder could not be listed
/// right now (network drop, permissions) – nothing is known, the photo must not count as read.
fn read_one(it: &Item, lists: &mut Listings) -> Option<(bool, Option<Content>)> {
    let dir = it.path.parent().map(Path::to_path_buf).unwrap_or_default();
    // photos of Apple Photos: their sidecar lives in graineo's data folder – never list the Photos library
    let apple = crate::apple_photos::uuid_from_path(&it.path).is_some();
    let listing = if apple { None } else { Some(lists.of(&dir)?) };
    Some(read_listed(it, &dir, listing))
}

fn read_listed(it: &Item, dir: &Path, listing: Option<&Listing>) -> (bool, Option<Content>) {
    let raw = raw_beside(&it.path, &it.format, listing);
    // a time in the future (clock of a NAS ahead) is capped to now: it must not win over later changes in the library
    let mtime = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| (d.as_secs() as i64).min(crate::now()));
    let xp = crate::sidecar::path(&it.path, &it.format, raw);
    // in the photo's folder: from the listing (no access per photo); elsewhere (Apple Photos) ask the disk
    let present = if xp.parent() == Some(dir) {
        let n = xp.file_name().map(|n| crate::nfc(&n.to_string_lossy()).to_lowercase()).unwrap_or_default();
        listing.is_some_and(|l| l.names.contains(&n))
    } else {
        xp.exists()
    };
    if !present {
        return (raw, None);
    }
    let text = match std::fs::read_to_string(&xp) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (raw, None),
        Err(_) => return (raw, Some(Content { result: (ReadResult::Broken, mtime(&xp)), develop: None, lightroom: Lightroom::default() })),
    };
    let m = mtime(&xp);
    match crate::xmp::read_text(&text) {
        None => (raw, Some(Content { result: (ReadResult::Broken, m), develop: None, lightroom: Lightroom::default() })),
        Some(w) => (
            raw,
            Some(Content { result: (ReadResult::Ok(w), m), develop: crate::xmp::develop_from_text(&text), lightroom: crate::xmp::lightroom_from_text(&text).for_image(&it.path) }),
        ),
    }
}

/// Rating, label and keywords of the sidecar beside a photo, for "Metadaten neu einlesen" (`crate::metadata_reread`).
/// Outer `None`: the folder could not be listed; inner `None`: no readable sidecar.
pub(crate) fn sidecar_values(path: &Path, format: &str, lists: &mut Listings) -> Option<Option<crate::xmp::Values>> {
    let it = Item { id: 0, path: path.to_path_buf(), format: format.to_string() };
    let (_, c) = read_one(&it, lists)?;
    Some(c.and_then(|c| match c.result.0 {
        ReadResult::Ok(w) => Some(w),
        _ => None,
    }))
}

/// What was taken over from one sidecar (field groups).
fn apply(db: &Db, it: &Item, raw: bool, c: Content) -> R<Vec<&'static str>> {
    let mut taken = Vec::new();
    // ratings, keywords, copies, capture time: as the sync always did (newer group wins); nothing is written to disk
    let copies_json = match &c.result.0 {
        ReadResult::Ok(w) => Some(w.copies_json.clone()),
        _ => None,
    };
    let t_file = c.result.1.unwrap_or(0);
    let e = crate::sync::reconcile_file_read(db, it.id, &it.path, &it.format, raw, false, c.result)?;
    taken.extend(e.from_file);
    let Some(copies_json) = copies_json else { return Ok(taken) };
    let conn = &db.conn;
    let cur = store::get(conn, it.id, None).map_err(f)?;
    if let Some(d) = &c.develop {
        if Some(d) != cur.settings.as_ref() && t_file >= cur.settings_time.unwrap_or(0) {
            store::put_settings(conn, it.id, None, Some(d), t_file).map_err(f)?;
            taken.push("Entwicklung");
        }
    }
    if !c.lightroom.is_empty() && c.lightroom != cur.lightroom {
        store::put_lightroom(conn, it.id, None, &c.lightroom).map_err(f)?;
        taken.push("Lightroom");
    }
    for k in crate::sidecar::read_copies(copies_json.as_deref()) {
        // only copies the library has (a copy deleted in the library is not brought back by an old sidecar)
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM image WHERE file=?1 AND copy=?2)", params![it.id, k.name], |r| r.get(0)).map_err(f)?;
        if !exists {
            continue;
        }
        let cur = store::get(conn, it.id, Some(&k.name)).map_err(f)?;
        if let Some(sv) = &k.settings {
            let j = sv.to_string();
            let same = cur.settings.as_deref().and_then(|x| serde_json::from_str::<serde_json::Value>(x).ok()).as_ref() == Some(sv);
            if !same && t_file >= cur.settings_time.unwrap_or(0) {
                store::put_settings(conn, it.id, Some(&k.name), Some(&j), t_file).map_err(f)?;
                taken.push("Entwicklung (Kopie)");
            }
        }
        let lr = Lightroom { develop: k.lightroom.clone(), orientation: k.lightroom_orientation.clone(), ..Default::default() };
        if !lr.is_empty() && lr != cur.lightroom {
            store::put_lightroom(conn, it.id, Some(&k.name), &lr).map_err(f)?;
            taken.push("Lightroom (Kopie)");
        }
    }
    Ok(taken)
}

fn items(db: &Db, sql: &str, p: impl rusqlite::Params) -> R<Vec<Item>> {
    let mut st = db.conn.prepare(sql).map_err(f)?;
    let r = st.query_map(p, |r| Ok(Item { id: r.get(0)?, path: PathBuf::from(r.get::<_, String>(1)?), format: r.get(2)? })).map_err(f)?.flatten().collect();
    Ok(r)
}

const ITEM_SQL: &str = "SELECT d.id, s.path || o.path || d.name, d.format FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root";

/// Root folders reachable right now (ids as SQL list).
fn reachable_roots(db: &Db) -> R<String> {
    let mut st = db.conn.prepare("SELECT id, path FROM root_folder").map_err(f)?;
    let v: Vec<(i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.flatten().collect();
    Ok(v.into_iter().filter(|(_, p)| crate::location::root_reachable(p)).map(|(i, _)| i.to_string()).collect::<Vec<_>>().join(","))
}

/// Counts of a reading run.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// photos looked at
    pub files: usize,
    /// of these with a sidecar
    pub sidecars: usize,
    /// sidecars from which something was taken over
    pub taken: usize,
    pub develop: usize,
    pub lightroom: usize,
    pub ratings: usize,
    pub broken: usize,
    /// location not reachable: comes later
    pub unreachable: usize,
    pub errors: usize,
    pub cancelled: bool,
    pub duration_ms: u128,
    pub log: Vec<String>,
}

impl Report {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "photos": self.files,
            "sidecars": self.sidecars,
            "taken": self.taken,
            "develop": self.develop,
            "lightroom": self.lightroom,
            "ratings": self.ratings,
            "broken": self.broken,
            "unreachable": self.unreachable,
            "errors": self.errors,
            "cancelled": self.cancelled,
            "duration_ms": self.duration_ms,
        })
    }
}

/// Read and take over a block of files (disk first, without lock; then one short transaction). `mark`: remember the
/// files as read (`file.xmp_read`).
fn block(db: &Db, list: &[Item], mark: bool, lists: &mut Listings, r: &mut Report) -> R<()> {
    let read: Vec<Option<(bool, Option<Content>)>> = list.iter().map(|it| read_one(it, lists)).collect();
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    for (it, got) in list.iter().zip(read) {
        let Some((raw, c)) = got else {
            // folder not readable right now: not marked, comes again when it is
            r.unreachable += 1;
            continue;
        };
        r.files += 1;
        if let Some(c) = c {
            r.sidecars += 1;
            if matches!(c.result.0, ReadResult::Broken) {
                r.broken += 1;
                if r.log.len() < 2000 {
                    r.log.push(format!("nicht lesbar: {}", it.path.display()));
                }
            }
            match apply(db, it, raw, c) {
                Ok(t) if !t.is_empty() => {
                    r.taken += 1;
                    r.develop += t.iter().any(|g| g.starts_with("Entwicklung")) as usize;
                    r.lightroom += t.iter().any(|g| g.starts_with("Lightroom")) as usize;
                    r.ratings += t.iter().any(|g| !g.starts_with("Entwicklung") && !g.starts_with("Lightroom")) as usize;
                    if r.log.len() < 2000 {
                        r.log.push(format!("{}: {}", it.path.display(), t.join(", ")));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    r.errors += 1;
                    r.log.push(format!("Fehler: {}: {e}", it.path.display()));
                }
            }
        }
        if mark {
            db.conn.execute("UPDATE file SET xmp_read = 1 WHERE id = ?1", [it.id]).map_err(f)?;
        }
    }
    tx.commit().map_err(f)
}

/// Read the sidecars of these files now (also the ones read before), if their location is reachable, and mark them read.
pub fn import_files(db: &Db, files: &[i64]) -> R<Report> {
    let mut r = Report::default();
    let roots = reachable_roots(db)?;
    if roots.is_empty() || files.is_empty() {
        r.unreachable = files.len();
        return Ok(r);
    }
    let ids = files.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let list = items(db, &format!("{ITEM_SQL} WHERE d.id IN ({ids}) AND o.root IN ({roots}) AND d.format <> 'VIDEO'"), [])?;
    r.unreachable = files.len() - list.len();
    let mut lists = Listings::default();
    block(db, &list, true, &mut lists, &mut r)?;
    // videos have no sidecar of ours: nothing to read
    db.conn.execute(&format!("UPDATE file SET xmp_read = 1 WHERE id IN ({ids}) AND format = 'VIDEO'"), []).map_err(f)?;
    Ok(r)
}

/// Photos whose sidecar has not been read yet (all locations).
pub fn unread_count(conn: &rusqlite::Connection) -> usize {
    conn.query_row("SELECT count(*) FROM file WHERE xmp_read IS NULL", [], |r| r.get::<_, i64>(0)).map(|n| n as usize).unwrap_or(0)
}

/// Read up to `max` unread sidecars of reachable locations (from the start, see [`import_unread_after`]).
pub fn import_unread(db: &Db, max: usize, pause: &dyn Fn()) -> R<Report> {
    import_unread_after(db, max, &mut 0, pause)
}

/// Read up to `max` unread sidecars of reachable locations, files after the id `*after` (moves `*after` on; folders that
/// cannot be listed right now are skipped and stay unread). Returns the report of this step (`files + unreachable == 0`:
/// nothing left that is reachable now). `pause` is called before every block.
pub fn import_unread_after(db: &Db, max: usize, after: &mut i64, pause: &dyn Fn()) -> R<Report> {
    let mut r = Report::default();
    let t0 = std::time::Instant::now();
    let roots = reachable_roots(db)?;
    // videos: nothing to read
    db.conn.execute("UPDATE file SET xmp_read = 1 WHERE xmp_read IS NULL AND format = 'VIDEO'", []).map_err(f)?;
    if roots.is_empty() {
        return Ok(r);
    }
    // one listing per folder serves its photos (the listings are kept for the whole step)
    let list = items(db, &format!("{ITEM_SQL} WHERE d.xmp_read IS NULL AND o.root IN ({roots}) AND d.id > ?1 ORDER BY d.id LIMIT {max}"), [*after])?;
    if let Some(last) = list.last() {
        *after = last.id;
    }
    let mut lists = Listings::default();
    for chunk in list.chunks(100) {
        pause();
        block(db, chunk, true, &mut lists, &mut r)?;
    }
    r.duration_ms = t0.elapsed().as_millis();
    Ok(r)
}

/// State of the background reading for the status line: (done, total) while it runs.
pub static PROGRESS_DONE: AtomicUsize = AtomicUsize::new(0);
pub static PROGRESS_TOTAL: AtomicUsize = AtomicUsize::new(0);
static WAKE_IMPORT: AtomicBool = AtomicBool::new(false);

/// Ask the background reading to look for unread sidecars now (after an import, a sync or a rebuild).
pub fn wake() {
    WAKE_IMPORT.store(true, Ordering::SeqCst);
}

/// Background reading progress for the status line (`Some((done, total))` while sidecars are being read).
pub fn progress() -> Option<(usize, usize)> {
    let t = PROGRESS_TOTAL.load(Ordering::Relaxed);
    (t > 0).then(|| (PROGRESS_DONE.load(Ordering::Relaxed).min(t), t))
}

static IMPORT_STARTED: std::sync::Once = std::sync::Once::new();

/// Start reading unread sidecars in the background (once per process): lowest priority, blocks of 100 photos,
/// `pause` before every block (the app passes its "wait while the user works"). Looks again every minute (a location
/// may be back) or after [`wake`].
pub fn start_background_import(db_path: PathBuf, pause: fn()) {
    IMPORT_STARTED.call_once(move || {
        let _ = std::thread::Builder::new().name("begleitdateien-einlesen".into()).spawn(move || {
            crate::platform::background_priority();
            let mut last_look = None::<std::time::Instant>;
            let mut db: Option<Db> = None;
            loop {
                let due = WAKE_IMPORT.swap(false, Ordering::SeqCst) || last_look.is_none_or(|t: std::time::Instant| t.elapsed() >= std::time::Duration::from_secs(60));
                if !due || !db_path.exists() {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                last_look = Some(std::time::Instant::now());
                if db.is_none() {
                    db = Db::open(&db_path).ok();
                }
                let Some(d) = db.as_ref() else { continue };
                let open = unread_count(&d.conn);
                if open == 0 {
                    continue;
                }
                PROGRESS_DONE.store(0, Ordering::Relaxed);
                PROGRESS_TOTAL.store(open, Ordering::Relaxed);
                let t0 = std::time::Instant::now();
                let mut total = Report::default();
                let mut after = 0;
                loop {
                    match import_unread_after(d, 500, &mut after, &|| pause()) {
                        Ok(r) if r.files + r.unreachable > 0 => {
                            PROGRESS_DONE.fetch_add(r.files, Ordering::Relaxed);
                            total.files += r.files;
                            total.sidecars += r.sidecars;
                            total.taken += r.taken;
                            total.develop += r.develop;
                            total.lightroom += r.lightroom;
                            total.errors += r.errors;
                        }
                        Ok(_) => break,
                        Err(e) => {
                            eprintln!("reading sidecars: {e}");
                            break;
                        }
                    }
                }
                PROGRESS_TOTAL.store(0, Ordering::Relaxed);
                if total.files > 0 {
                    total.duration_ms = t0.elapsed().as_millis();
                    eprintln!("sidecars read into the library: {}", total.json());
                    // the queued sidecar writes of these files may go now
                    crate::store::start_writer_round();
                }
            }
        });
    });
}

/// "Begleitdateien einlesen …": read every sidecar of the reachable locations again and take over what is newer
/// than the library's state (changes by other programs). `progress(done, total)` now and then; `cancel` stops after
/// the current block.
pub fn run(db: &Db, progress: &dyn Fn(usize, usize), cancel: &AtomicBool) -> R<Report> {
    let t0 = std::time::Instant::now();
    let mut r = Report::default();
    let roots = reachable_roots(db)?;
    let all: i64 = db.conn.query_row("SELECT count(*) FROM file WHERE format <> 'VIDEO'", [], |r| r.get(0)).map_err(f)?;
    let list = if roots.is_empty() { Vec::new() } else { items(db, &format!("{ITEM_SQL} WHERE o.root IN ({roots}) AND d.format <> 'VIDEO' ORDER BY d.folder, d.id"), [])? };
    r.unreachable = (all as usize).saturating_sub(list.len());
    let total = list.len();
    let mut lists = Listings::default();
    for (i, chunk) in list.chunks(200).enumerate() {
        progress(i * 200, total);
        if cancel.load(Ordering::Relaxed) {
            r.cancelled = true;
            break;
        }
        block(db, chunk, true, &mut lists, &mut r)?;
        // folder listings are only needed while their photos come (ordered by folder)
        if lists.0.len() > 64 {
            lists.0.clear();
        }
    }
    progress(total, total);
    r.duration_ms = t0.elapsed().as_millis();
    Ok(r)
}

/// Write the protocol of a run to the reports folder (`sidecar-import-<time>.txt`). Returns its path.
pub fn save_protocol(r: &Report) -> Option<PathBuf> {
    let dir = crate::data_dir().join("reports");
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(format!("sidecar-import-{}.txt", crate::now()));
    let mut t = serde_json::to_string_pretty(&r.json()).unwrap_or_default();
    t.push('\n');
    for l in &r.log {
        t.push_str(l);
        t.push('\n');
    }
    crate::files::write_atomic(&p, t.as_bytes()).ok()?;
    Some(p)
}

/// Mark files as read without reading (e.g. files created by the app itself that cannot have a sidecar yet).
pub fn mark_read(db: &Db, files: &[i64]) -> R<()> {
    for f_ in files {
        db.conn.execute("UPDATE file SET xmp_read = 1 WHERE id = ?1", params![f_]).map_err(f)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that cannot be listed right now (network drop, permissions) must not mark its photos as read.
    #[test]
    fn unreadable_folder_stays_unread() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("graineo-xmpread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("fotos").join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.jpg"), b"x").unwrap();
        std::fs::write(sub.join("a.xmp"), b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>").unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        let root = format!("{}/", dir.join("fotos").display());
        db.conn.execute("INSERT INTO root_folder(path, name, origin) VALUES(?1, 'fotos', 'own')", [&root]).unwrap();
        let s: i64 = db.conn.last_insert_rowid();
        db.conn.execute("INSERT INTO folder(root, path) VALUES(?1, 'sub/')", [s]).unwrap();
        let o: i64 = db.conn.last_insert_rowid();
        db.conn.execute("INSERT INTO file(folder, name, format, status) VALUES(?1, 'a.jpg', 'JPG', 0)", [o]).unwrap();
        db.conn.execute("UPDATE file SET xmp_read = NULL", []).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
        let listable = std::fs::read_dir(&sub).is_ok(); // running as root: permissions do not apply
        let r = import_unread(&db, 100, &|| {}).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !listable {
            assert_eq!((r.files, r.unreachable), (0, 1));
            assert_eq!(unread_count(&db.conn), 1, "not marked while the folder was unreadable");
        }
        let r = import_unread(&db, 100, &|| {}).unwrap();
        assert_eq!(r.files, 1);
        assert_eq!(unread_count(&db.conn), 0);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
