//! Reading the shooting data of library photos again from their files ("Metadaten neu einlesen").
//!
//! Three ways in, one core ([`reread`]):
//! - The command for the selection, a folder or a collection ([`run`]): reads at once, counts read / changed /
//!   unreachable, can be cancelled, and returns an [`Undo`] that puts the previous state back (one ⌘Z).
//!   On request (asked only when a sidecar lies beside a photo) rating, label and keywords of the sidecar are taken
//!   over as well; never the develop settings, virtual copies or collections.
//! - The sync: a file whose size or modification time changed is marked ([`STALE`], `reconcile::write_steps`); the
//!   background step after the sync reads it again ([`refresh_stale`]).
//! - Once per library: photos without capture date or camera (e.g. imported under Windows/Linux before 1.4.2) and
//!   X3F without orientation are marked as well ([`mark_empty_once`]).
//!
//! What is read: capture date, camera, lens, exposure, focal length, ISO, GPS position (place), pixel size and
//! orientation. A value the file does not carry keeps what the library had (e.g. a lens from the Lightroom catalog).
//! A corrected capture time (`capture_time_original` set) stays as corrected. The orientation is only filled in where
//! the library has none (a value there may be a rotation from Lightroom). Nothing is ever written to a file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::types::Value;
use rusqlite::{params, OptionalExtension};

use crate::db::Db;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// `file.exif`: the file changed since its shooting data were read; read again after the sync ([`refresh_stale`]).
pub const STALE: i64 = -2;

/// `info` key of the one-time pass over photos with empty fields ([`mark_empty_once`]).
pub const EMPTY_ONCE_KEY: &str = "metadata_refill_empty_1";

/// Files read per block (one short write transaction each).
const BLOCK: usize = 32;
/// Reading threads (quiet: few; on a network drive the wait per file is the limit anyway).
const THREADS: usize = 4;

/// Shooting data columns of `file` as the library holds them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileValues {
    pub camera: Option<String>,
    pub capture: Option<String>,
    pub lens: Option<String>,
    pub aperture: Option<f64>,
    pub focal: Option<f64>,
    pub iso: Option<i64>,
    pub time: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
}

const FILE_COLUMNS: &str = "camera, capture_time, lens, aperture, focal_length, iso, time, width, height, orientation";

fn file_values(r: &rusqlite::Row, at: usize) -> rusqlite::Result<FileValues> {
    Ok(FileValues {
        camera: r.get(at)?,
        capture: r.get(at + 1)?,
        lens: r.get(at + 2)?,
        aperture: r.get(at + 3)?,
        focal: r.get(at + 4)?,
        iso: r.get(at + 5)?,
        time: r.get(at + 6)?,
        width: r.get(at + 7)?,
        height: r.get(at + 8)?,
        orientation: r.get(at + 9)?,
    })
}

fn write_values(c: &rusqlite::Connection, file: i64, v: &FileValues, exif: i64) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE file SET camera=?2, capture_time=?3, lens=?4, aperture=?5, focal_length=?6, iso=?7, time=?8, width=?9, height=?10, orientation=?11, exif=?12 WHERE id=?1",
        params![file, v.camera, v.capture, v.lens, v.aperture, v.focal, v.iso, v.time, v.width, v.height, v.orientation, exif],
    )?;
    Ok(())
}

/// What one file says (read outside the write lock).
#[derive(Clone, Debug, Default)]
pub struct FromFile {
    pub capture: crate::exif::Capture,
    pub gps: Option<(f64, f64)>,
}

/// Read the shooting data of one file. `None`: the file cannot be opened (not reachable right now).
pub fn read_file(path: &Path, format: &str) -> Option<FromFile> {
    std::fs::File::open(path).ok()?;
    let x3f = format == "X3F" || format == "X3I";
    let mut capture = crate::exif::read(path, format).unwrap_or_default();
    // pixel size: X3F from the file (above); other formats from the header, as the preview generation stores it
    if !x3f && capture.width.is_none() {
        if let Some(p) = crate::imageio::properties(path) {
            capture.width = p.width;
            capture.height = p.height;
            capture.orientation = capture.orientation.or(p.orientation.filter(|o| (1..=8).contains(o)));
        }
    }
    let gps = if x3f { None } else { crate::imageio::gps_position(path) };
    Some(FromFile { capture, gps })
}

/// The library's values after reading: what the file carries wins, what it lacks stays. A corrected capture time
/// stays; the orientation is only filled in.
pub fn merged(old: &FileValues, new: &crate::exif::Capture, time_corrected: bool) -> FileValues {
    FileValues {
        camera: new.camera.clone().or_else(|| old.camera.clone()),
        capture: if time_corrected { old.capture.clone() } else { new.capture.clone().or_else(|| old.capture.clone()) },
        lens: new.lens.clone().or_else(|| old.lens.clone()),
        aperture: new.aperture.or(old.aperture),
        focal: new.focal.or(old.focal),
        iso: new.iso.or(old.iso),
        time: new.time.or(old.time),
        width: new.width.map(i64::from).or(old.width),
        height: new.height.map(i64::from).or(old.height),
        orientation: old.orientation.or(new.orientation.map(i64::from)),
    }
}

/// State before a reading run, to put it back (⌘Z).
#[derive(Clone, Debug, Default)]
pub struct Undo {
    /// file, values, `file.exif`
    files: Vec<(i64, FileValues, i64)>,
    /// file, the whole `place` row before (`None`: there was none)
    places: Vec<(i64, Option<Vec<Value>>)>,
    /// image, stars, label
    ratings: Vec<(i64, i64, i64)>,
    /// image, keyword ids
    keywords: Vec<(i64, Vec<i64>)>,
}

impl Undo {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.places.is_empty() && self.ratings.is_empty() && self.keywords.is_empty()
    }
    /// Rows held (limit of the ⌘Z stack).
    pub fn size(&self) -> usize {
        self.files.len() + self.places.len() + self.ratings.len() + self.keywords.len()
    }
}

/// Result of [`run`].
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// files read
    pub read: usize,
    /// of these, files whose library entry changed
    pub changed: usize,
    /// files that could not be opened (drive missing, file gone): skipped
    pub unreachable: usize,
    pub cancelled: bool,
    pub undo: Undo,
    /// changed files (search index, reload)
    pub changed_files: Vec<i64>,
}

struct Item {
    id: i64,
    path: PathBuf,
    format: String,
    status: i64,
}

fn items(db: &Db, files: &[i64]) -> R<Vec<Item>> {
    let mut st = db
        .conn
        .prepare_cached("SELECT d.id, s.path || o.path || d.name, d.format, d.status FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1")
        .map_err(f)?;
    let mut v = Vec::with_capacity(files.len());
    let mut seen = std::collections::HashSet::new();
    for &id in files {
        if !seen.insert(id) {
            continue;
        }
        if let Some(it) = st.query_row([id], |r| Ok(Item { id: r.get(0)?, path: PathBuf::from(r.get::<_, String>(1)?), format: r.get(2)?, status: r.get(3)? })).optional().map_err(f)? {
            // merged photos (3) have no file of their own, Apple Photos (4 hidden) and videos carry no shooting data here
            if it.status == 3 || it.status == 4 || it.format == "VIDEO" {
                continue;
            }
            v.push(it);
        }
    }
    Ok(v)
}

/// Files with a sidecar beside them (the question "auch Bewertung, Stichwörter und Etikett übernehmen?" is only asked
/// when there is one). Reads one folder listing per folder.
pub fn with_sidecar(db: &Db, files: &[i64], cancel: &AtomicBool) -> R<usize> {
    let mut lists = crate::sidecar_import::Listings::default();
    let mut n = 0;
    for it in items(db, files)? {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if it.status == 0 && matches!(crate::sidecar_import::sidecar_values(&it.path, &it.format, &mut lists), Some(Some(_))) {
            n += 1;
        }
    }
    Ok(n)
}

/// The command "Metadaten neu einlesen" for these files. `sidecar`: also take over rating, label and keywords of the
/// sidecar beside each photo (original image; virtual copies keep theirs). `progress(done, total)` after each block.
pub fn run(db: &Db, files: &[i64], sidecar: bool, cancel: &AtomicBool, progress: &dyn Fn(usize, usize)) -> R<Outcome> {
    let list = items(db, files)?;
    let out = reread(db, &list, sidecar, true, cancel, progress)?;
    if !out.changed_files.is_empty() {
        let _ = crate::search::update_files(db, &out.changed_files);
    }
    Ok(out)
}

fn reread(db: &Db, list: &[Item], sidecar: bool, keep_undo: bool, cancel: &AtomicBool, progress: &dyn Fn(usize, usize)) -> R<Outcome> {
    let mut out = Outcome::default();
    let total = list.len();
    let mut lists = crate::sidecar_import::Listings::default();
    let mut done = 0;
    for block in list.chunks(BLOCK) {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        // read outside the write lock, a few threads at once
        let read: Vec<Option<FromFile>> = std::thread::scope(|sc| {
            let parts: Vec<_> = block
                .chunks(block.len().div_ceil(THREADS).max(1))
                .map(|part| {
                    sc.spawn(move || {
                        crate::platform::background_priority();
                        part.iter().map(|it| if it.status == 0 || it.status == 2 || it.status == 1 { read_file(&it.path, &it.format) } else { None }).collect::<Vec<_>>()
                    })
                })
                .collect();
            parts.into_iter().flat_map(|t| t.join().unwrap_or_default()).collect()
        });
        let side: Vec<Option<crate::xmp::Values>> = block
            .iter()
            .zip(&read)
            .map(|(it, r)| if sidecar && r.is_some() { crate::sidecar_import::sidecar_values(&it.path, &it.format, &mut lists).flatten() } else { None })
            .collect();
        let tx = db.conn.unchecked_transaction().map_err(f)?;
        for ((it, r), w) in block.iter().zip(read).zip(side) {
            let Some(r) = r else {
                out.unreachable += 1;
                continue;
            };
            out.read += 1;
            let mut changed = false;
            let (old, exif, corrected): (FileValues, i64, bool) = tx
                .query_row(&format!("SELECT {FILE_COLUMNS}, exif, capture_time_original IS NOT NULL FROM file WHERE id = ?1"), [it.id], |row| Ok((file_values(row, 0)?, row.get(10)?, row.get(11)?)))
                .map_err(f)?;
            let new = merged(&old, &r.capture, corrected);
            let done_mark = if it.format == "X3F" || it.format == "X3I" { 3 } else { 1 };
            let exif_new = if exif <= 0 { done_mark } else { exif };
            if new != old || exif_new != exif {
                if keep_undo {
                    out.undo.files.push((it.id, old.clone(), exif));
                }
                write_values(&tx, it.id, &new, exif_new).map_err(f)?;
                changed |= new != old;
            }
            // place: only a position the file carries replaces the stored one
            if let Some((lat, lon)) = r.gps {
                let before: Option<Vec<Value>> = tx
                    .query_row("SELECT file, state, latitude, longitude, city, region, country, country_code, name FROM place WHERE file = ?1", [it.id], |row| (0..9).map(|k| row.get::<_, Value>(k)).collect())
                    .optional()
                    .map_err(f)?;
                let same = before.as_ref().is_some_and(|b| matches!((&b[1], &b[2], &b[3]), (Value::Integer(1), Value::Real(a), Value::Real(o)) if *a == lat && *o == lon));
                if !same {
                    if keep_undo {
                        out.undo.places.push((it.id, before));
                    }
                    crate::places::store(&tx, it.id, 1, Some((lat, lon))).map_err(f)?;
                    changed = true;
                }
            }
            if let Some(w) = w {
                let image: Option<(i64, i64, i64)> = tx.query_row("SELECT id, stars, label FROM image WHERE file = ?1 AND copy IS NULL", [it.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(f)?;
                if let Some((image, stars, label)) = image {
                    if w.has_rating && (i64::from(w.stars), i64::from(w.label)) != (stars, label) {
                        out.undo.ratings.push((image, stars, label));
                        tx.execute("UPDATE image SET stars = ?2, label = ?3, changed = ?4, rating_time = ?4 WHERE id = ?1", params![image, w.stars, w.label, crate::now()]).map_err(f)?;
                        changed = true;
                    }
                    if w.has_keywords {
                        let current = crate::sync::keyword_paths(db, image)?;
                        if !crate::sync::equal_kw(&w.keywords, &current) {
                            out.undo.keywords.push((image, crate::keywords::ids_of(&db.conn, image)?));
                            crate::sync::set_keywords(db, image, &w.keywords, crate::now(), true)?;
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                out.changed += 1;
                out.changed_files.push(it.id);
            }
        }
        tx.commit().map_err(f)?;
        done += block.len();
        progress(done, total);
    }
    Ok(out)
}

/// ⌘Z of [`run`]: put the previous state back. Returns the files touched (search index, reload).
pub fn undo(db: &Db, u: &Undo) -> R<Vec<i64>> {
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let mut files: Vec<i64> = Vec::new();
    for (file, v, exif) in &u.files {
        write_values(&tx, *file, v, *exif).map_err(f)?;
        files.push(*file);
    }
    for (file, before) in &u.places {
        tx.execute("DELETE FROM place WHERE file = ?1", [file]).map_err(f)?;
        if let Some(row) = before {
            tx.execute("INSERT INTO place(file, state, latitude, longitude, city, region, country, country_code, name) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", rusqlite::params_from_iter(row.iter())).map_err(f)?;
        }
        files.push(*file);
    }
    for (image, stars, label) in &u.ratings {
        tx.execute("UPDATE image SET stars = ?2, label = ?3, changed = ?4, rating_time = ?4 WHERE id = ?1", params![image, stars, label, crate::now()]).map_err(f)?;
        if let Some(file) = db.file_of(*image) {
            files.push(file);
        }
    }
    for (image, ids) in &u.keywords {
        crate::keywords::set_ids(db, *image, ids, crate::now(), true)?;
        if let Some(file) = db.file_of(*image) {
            files.push(file);
        }
    }
    tx.commit().map_err(f)?;
    files.sort_unstable();
    files.dedup();
    let _ = crate::search::update_files(db, &files);
    Ok(files)
}

/// After the sync: read the files again whose size or modification time changed ([`STALE`]), without sidecars and
/// without undo, quietly. A file that cannot be opened stays marked. Returns the files read.
pub fn refresh_stale(db: &Db, max: usize, cancel: &AtomicBool) -> R<usize> {
    let ids: Vec<i64> = {
        let mut st = db.conn.prepare("SELECT id FROM file WHERE exif = ?1 AND status = 0 AND format <> 'VIDEO' LIMIT ?2").map_err(f)?;
        let r = st.query_map(params![STALE, max as i64], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<i64>>>().map_err(f)?;
        r
    };
    if ids.is_empty() {
        return Ok(0);
    }
    let list = items(db, &ids)?;
    let out = reread(db, &list, false, false, cancel, &|_, _| {})?;
    if !out.changed_files.is_empty() {
        let _ = crate::search::update_files(db, &out.changed_files);
    }
    Ok(out.read)
}

/// Once per library: mark photos without capture date or camera, and X3F without orientation, to be read again
/// ([`refresh_stale`]). Returns the number marked (0 when it ran before).
pub fn mark_empty_once(db: &Db) -> R<usize> {
    if db.info(EMPTY_ONCE_KEY).is_some() {
        return Ok(0);
    }
    let n = db
        .conn
        .execute(
            &format!(
                "UPDATE file SET exif = {STALE} WHERE exif > 0 AND status = 0 AND format <> 'VIDEO'
                 AND (capture_time IS NULL OR camera IS NULL OR (format IN ('X3F', 'X3I') AND orientation IS NULL))"
            ),
            [],
        )
        .map_err(f)?;
    db.set_info(EMPTY_ONCE_KEY, &crate::now().to_string()).map_err(f)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif_header::tests::{jpeg_with, sample};

    struct Lib {
        dir: PathBuf,
        db: Db,
        folder: i64,
    }

    fn lib(name: &str) -> Lib {
        let dir = std::env::temp_dir().join(format!("graineo-reread-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        let root = db.create_root(&format!("{}/photos/", dir.display()), "t", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        Lib { dir, db, folder }
    }

    impl Lib {
        fn add(&self, name: &str, bytes: &[u8]) -> (i64, i64) {
            std::fs::write(self.dir.join("photos").join(name), bytes).unwrap();
            self.db.conn.execute("INSERT INTO file(folder, name, format, status, exif, camera, capture_time) VALUES(?1, ?2, 'JPG', 0, 1, 'Alt', '2000-01-01T00:00:00')", params![self.folder, name]).unwrap();
            let file = self.db.conn.last_insert_rowid();
            self.db.conn.execute("INSERT INTO image(file) VALUES(?1)", [file]).unwrap();
            (file, self.db.conn.last_insert_rowid())
        }
        fn get<T: rusqlite::types::FromSql>(&self, sql: &str, id: i64) -> T {
            self.db.conn.query_row(sql, [id], |r| r.get(0)).unwrap()
        }
    }

    impl Drop for Lib {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:Rating="4" xmp:Label="Grün"><dc:subject><rdf:Bag><rdf:li>Berg</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#;

    /// The command reads the file's shooting data, ⌘Z puts the old values back; the sidecar is left alone without
    /// being asked.
    #[test]
    fn command_reads_and_undo_restores() {
        let l = lib("cmd");
        let (file, image) = l.add("a.jpg", &jpeg_with(Some(&sample(true))));
        std::fs::write(l.dir.join("photos/a.xmp"), XMP).unwrap();
        let out = run(&l.db, &[file], false, &never(), &|_, _| {}).unwrap();
        assert_eq!((out.read, out.changed, out.unreachable), (1, 1, 0));
        assert_eq!(l.get::<String>("SELECT camera FROM file WHERE id=?1", file), "Canon EOS R5");
        assert_eq!(l.get::<String>("SELECT capture_time FROM file WHERE id=?1", file), "2024-05-11T14:03:22");
        assert_eq!(l.get::<i64>("SELECT stars FROM image WHERE id=?1", image), 0, "no sidecar without asking");
        // nothing changed the second time
        let again = run(&l.db, &[file], false, &never(), &|_, _| {}).unwrap();
        assert_eq!((again.read, again.changed), (1, 0));
        undo(&l.db, &out.undo).unwrap();
        assert_eq!(l.get::<String>("SELECT camera FROM file WHERE id=?1", file), "Alt");
        assert_eq!(l.get::<String>("SELECT capture_time FROM file WHERE id=?1", file), "2000-01-01T00:00:00");
    }

    /// With the sidecar: rating, label and keywords are taken over; ⌘Z takes them back. Develop settings are not
    /// touched.
    #[test]
    fn command_with_sidecar() {
        let l = lib("xmp");
        let (file, image) = l.add("a.jpg", &jpeg_with(Some(&sample(true))));
        l.db.conn.execute("INSERT INTO develop_state(file, copy, settings, settings_time) VALUES(?1, '', '{\"exposure\":1}', 5)", [file]).ok();
        std::fs::write(l.dir.join("photos/a.xmp"), XMP).unwrap();
        assert_eq!(with_sidecar(&l.db, &[file], &never()).unwrap(), 1);
        let out = run(&l.db, &[file], true, &never(), &|_, _| {}).unwrap();
        assert_eq!(out.changed, 1);
        assert_eq!(l.get::<i64>("SELECT stars FROM image WHERE id=?1", image), 4);
        assert_eq!(l.get::<i64>("SELECT label FROM image WHERE id=?1", image), 3);
        assert_eq!(crate::sync::keyword_paths(&l.db, image).unwrap(), vec!["Berg".to_string()]);
        undo(&l.db, &out.undo).unwrap();
        assert_eq!(l.get::<i64>("SELECT stars FROM image WHERE id=?1", image), 0);
        assert_eq!(l.get::<i64>("SELECT label FROM image WHERE id=?1", image), 0);
        assert!(crate::sync::keyword_paths(&l.db, image).unwrap().is_empty());
    }

    /// A file that cannot be opened is skipped and counted, its entry stays.
    #[test]
    fn unreachable_is_counted() {
        let l = lib("gone");
        let (file, _) = l.add("a.jpg", &jpeg_with(Some(&sample(true))));
        let (gone, _) = l.add("b.jpg", b"x");
        std::fs::remove_file(l.dir.join("photos/b.jpg")).unwrap();
        let out = run(&l.db, &[file, gone], false, &never(), &|_, _| {}).unwrap();
        assert_eq!((out.read, out.changed, out.unreachable), (1, 1, 1));
        assert_eq!(l.get::<String>("SELECT camera FROM file WHERE id=?1", gone), "Alt");
    }

    /// A corrected capture time stays; a lens the file lacks stays; the orientation is only filled in.
    #[test]
    fn merge_rules() {
        let old = FileValues { capture: Some("2001-01-01T00:00:00".into()), lens: Some("LR-Objektiv".into()), orientation: Some(8), ..Default::default() };
        let new = crate::exif::Capture { capture: Some("2024-05-11T14:03:22".into()), camera: Some("X".into()), orientation: Some(6), ..Default::default() };
        let m = merged(&old, &new, true);
        assert_eq!(m.capture.as_deref(), Some("2001-01-01T00:00:00"));
        assert_eq!(m.lens.as_deref(), Some("LR-Objektiv"));
        assert_eq!(m.camera.as_deref(), Some("X"));
        assert_eq!(m.orientation, Some(8));
        let m = merged(&FileValues::default(), &new, false);
        assert_eq!((m.capture.as_deref(), m.orientation), (Some("2024-05-11T14:03:22"), Some(6)));
    }

    /// Cancelling before the first block reads nothing.
    #[test]
    fn cancel_stops() {
        let l = lib("cancel");
        let (file, _) = l.add("a.jpg", &jpeg_with(Some(&sample(true))));
        let out = run(&l.db, &[file], false, &AtomicBool::new(true), &|_, _| {}).unwrap();
        assert!(out.cancelled);
        assert_eq!(out.read, 0);
    }

    /// The sync notices a changed file (size/time) and reads its shooting data again; an unchanged one is not read.
    #[test]
    fn sync_rereads_changed_file() {
        let l = lib("sync");
        let path = l.dir.join("photos/a.jpg");
        std::fs::write(&path, jpeg_with(None)).unwrap();
        let o = crate::reconcile::Options { depth: crate::reconcile::Depth::Full, write_now: Some(false), progress: &|_| {}, count: &|_, _| {}, pause: &|| {} };
        crate::reconcile::run(&l.db, None, &o).unwrap();
        crate::exif::backfill(&l.db, 100, &never()).unwrap();
        let file: i64 = l.db.conn.query_row("SELECT id FROM file WHERE name='a.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(l.get::<Option<String>>("SELECT camera FROM file WHERE id=?1", file), None);
        assert_eq!(refresh_stale(&l.db, 100, &never()).unwrap(), 0, "unchanged: nothing to read");
        // overwritten in place with EXIF data (another size)
        std::fs::write(&path, jpeg_with(Some(&sample(true)))).unwrap();
        crate::reconcile::run(&l.db, None, &o).unwrap();
        assert_eq!(l.get::<i64>("SELECT exif FROM file WHERE id=?1", file), STALE);
        assert_eq!(refresh_stale(&l.db, 100, &never()).unwrap(), 1);
        assert_eq!(l.get::<String>("SELECT camera FROM file WHERE id=?1", file), "Canon EOS R5");
        assert_eq!(l.get::<i64>("SELECT exif FROM file WHERE id=?1", file), 1);
    }

    /// Empty fields are read once: marked at the first call only.
    #[test]
    fn empty_fields_once() {
        let l = lib("empty");
        let (file, _) = l.add("a.jpg", &jpeg_with(Some(&sample(true))));
        l.db.conn.execute("UPDATE file SET camera = NULL WHERE id = ?1", [file]).unwrap();
        assert_eq!(mark_empty_once(&l.db).unwrap(), 1);
        assert_eq!(refresh_stale(&l.db, 100, &never()).unwrap(), 1);
        assert_eq!(l.get::<String>("SELECT camera FROM file WHERE id=?1", file), "Canon EOS R5");
        l.db.conn.execute("UPDATE file SET camera = NULL WHERE id = ?1", [file]).unwrap();
        assert_eq!(mark_empty_once(&l.db).unwrap(), 0, "only once");
    }

    /// X3F (`GRAINEO_TEST_X3F_DIR`, all X3F there; skipped without it): the orientation comes from the embedded
    /// EXIF data, so a portrait X3F has its upright shape in the library (filmstrip cell) before its preview exists.
    #[test]
    fn x3f_orientation_from_the_file() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_X3F_DIR").map(PathBuf::from) else {
            eprintln!("GRAINEO_TEST_X3F_DIR not set, skipped");
            return;
        };
        let mut portrait = 0;
        for p in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f"))) {
            let r = read_file(&p, "X3F").expect("readable");
            let c = &r.capture;
            let aspect = match (c.width, c.height) {
                (Some(w), Some(h)) => if matches!(c.orientation, Some(5..=8)) { h as f32 / w as f32 } else { w as f32 / h as f32 },
                _ => 0.0,
            };
            eprintln!("{}: orientation {:?}, {:?}x{:?}, upright aspect {aspect:.3}", p.display(), c.orientation, c.width, c.height);
            // early Classic files (SD9) carry no EXIF block: no orientation there
            assert!(c.orientation.is_some() || c.camera.is_none(), "{}", p.display());
            portrait += usize::from(aspect > 0.0 && aspect < 1.0);
        }
        eprintln!("portrait: {portrait}");
    }
}
