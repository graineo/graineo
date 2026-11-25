//! Sync database ↔ sidecar per file. Since 08.11.2026 the library is the main store ([`crate::store`]); this
//! comparison runs only when a sidecar is read into the library ([`crate::sidecar_import`]: once per file, or on
//! "Begleitdateien einlesen …") – never as part of the regular sync. Rules when it runs:
//! - sidecar missing or broken, the database knows values → rewrite the sidecar.
//! - both present and different → per field group (rating, keywords) the newer
//!   state wins (timestamp in the sidecar, else its modification time; in the database `rating_ts`
//!   or `keyword_ts`). The report lists every decision.
//! - virtual copies: union by name; if one is missing in the sidecar, it is added.
//! - database lost → during sync all values come back from the sidecars.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::sidecar::{self, VirtualCopy};
use crate::db::Db;
use crate::xmp::{ReadResult, Values};

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// What syncing a file did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    /// sidecar rewritten (was missing or broken)
    pub new: bool,
    pub broken: bool,
    /// groups taken over from the sidecar
    pub from_file: Vec<&'static str>,
    /// groups written into the sidecar (database newer)
    pub in_file: Vec<&'static str>,
    /// only checked, not written (writing off)
    pub would_write: bool,
}

/// Keyword paths ("A|B|C") of an image.
pub fn keyword_paths(db: &Db, image: i64) -> R<Vec<String>> {
    let mut st = db
        .conn
        .prepare_cached(
            "WITH RECURSIVE p(id, name, parent, depth) AS (
                SELECT s.id, s.name, s.parent, 0 FROM image_keyword bs JOIN keyword s ON s.id = bs.keyword WHERE bs.image = ?1
                UNION ALL SELECT p.id, e.name || '|' || p.name, e.parent, p.depth + 1 FROM p JOIN keyword e ON e.id = p.parent WHERE p.depth < 64)
             SELECT name FROM p WHERE parent IS NULL ORDER BY name",
        )
        .map_err(f)?;
    let r = st.query_map([image], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<String>>>().map_err(f)?;
    Ok(r)
}

/// Create or find a keyword chain given as stored path ("Orte|Italien|Rom", the sidecar and Lightroom form);
/// returns the ID of the leaf. Only `|` separates: typed input goes through [`crate::keywords::resolve`].
pub fn create_keyword(db: &Db, path: &str) -> R<Option<i64>> {
    let mut parent: Option<i64> = None;
    for part in path.split('|').map(str::trim).filter(|t| !t.is_empty()).take(crate::keywords::MAX_DEPTH) {
        parent = Some(crate::keywords::child(&db.conn, parent, part)?);
    }
    Ok(parent)
}

/// Set (replace) an image's keywords and remember it as an own change.
pub fn set_keywords(db: &Db, image: i64, paths: &[String], time: i64, own: bool) -> R<()> {
    // all or nothing (a savepoint works inside a caller's transaction too): an error after the DELETE left the photo
    // without any keyword before 0.108
    db.conn.execute_batch("SAVEPOINT set_keywords").map_err(f)?;
    let r = (|| -> R<()> {
        db.conn.execute("DELETE FROM image_keyword WHERE image = ?1", [image]).map_err(f)?;
        for p in paths {
            if let Some(s) = create_keyword(db, p)? {
                db.conn.execute("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1, ?2)", params![image, s]).map_err(f)?;
            }
        }
        db.conn.execute("UPDATE image SET keywords_time = ?2, keywords_changed = CASE WHEN ?3 THEN ?2 ELSE keywords_changed END WHERE id = ?1", params![image, time, own]).map_err(f)?;
        Ok(())
    })();
    match r {
        Ok(()) => db.conn.execute_batch("RELEASE set_keywords").map_err(f)?,
        Err(e) => {
            let _ = db.conn.execute_batch("ROLLBACK TO set_keywords; RELEASE set_keywords");
            return Err(e);
        }
    }
    // own changes (keywords, people): only this image's search entry is renewed, not the whole index
    if own {
        let _ = crate::search::update_image(db, image);
    }
    Ok(())
}

struct DbImage {
    id: i64,
    copy: Option<String>,
    stars: u8,
    flag: i8,
    label: u8,
    rating_ts: Option<i64>,
    keyword_ts: Option<i64>,
}

fn images_of_file(db: &Db, file: i64) -> R<Vec<DbImage>> {
    let mut st = db.conn.prepare_cached("SELECT id, copy, stars, flag, label, rating_time, keywords_time FROM image WHERE file = ?1 ORDER BY copy IS NOT NULL, id").map_err(f)?;
    let r = st
        .query_map([file], |r| {
            Ok(DbImage {
                id: r.get(0)?,
                copy: r.get(1)?,
                stars: r.get::<_, i64>(2)?.clamp(0, 5) as u8,
                flag: r.get::<_, i64>(3)?.clamp(-1, 1) as i8,
                label: r.get::<_, i64>(4)?.clamp(0, 5) as u8,
                rating_ts: r.get(5)?,
                keyword_ts: r.get(6)?,
            })
        })
        .map_err(f)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(f)?;
    Ok(r)
}

/// Database values for a file's sidecar (original + copies).
pub fn db_values(db: &Db, file: i64) -> R<Option<(i64, Values)>> {
    let images = images_of_file(db, file)?;
    let Some(o) = images.iter().find(|b| b.copy.is_none()) else { return Ok(None) };
    let mut copies = Vec::new();
    for b in images.iter().filter(|b| b.copy.is_some()) {
        copies.push(VirtualCopy { name: b.copy.clone().unwrap_or_default(), stars: b.stars, flag: b.flag, label: b.label, keywords: keyword_paths(db, b.id)?, ..Default::default() });
    }
    // keep the copies' own develop settings from the existing sidecar
    Ok(Some((
        o.id,
        Values {
            stars: o.stars,
            flag: o.flag,
            label: o.label,
            keywords: keyword_paths(db, o.id)?,
            rating_time: o.rating_ts,
            keyword_time: o.keyword_ts,
            copies_json: sidecar::copies_text(&copies),
            has_rating: true,
            has_keywords: true,
            regions: regions_of(db, o.id, file),
            region_dims: region_dims(db, file),
            clear_regions: db.conn.query_row("SELECT faces_scanned FROM image WHERE id=?1", [o.id], |r| r.get::<_, i64>(0)).map(|v| v == 1).unwrap_or(false),
            time_correction: time_correction_of(db, file),
            copyright: db.copyright_of(o.id),
        },
    )))
}

/// Corrected capture time of a file (new, original), if corrected.
fn time_correction_of(db: &Db, file: i64) -> Option<(String, String)> {
    db.conn
        .query_row("SELECT capture_time, capture_time_original FROM file WHERE id=?1 AND capture_time_original IS NOT NULL AND capture_time IS NOT NULL", [file], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok()
}

/// Named (confirmed) faces of an image for the sidecar.
fn regions_of(db: &Db, image: i64, _file: i64) -> Vec<crate::xmp::Region> {
    let Ok(mut st) = db.conn.prepare_cached("SELECT s.name, g.x, g.y, g.w, g.h FROM face g JOIN keyword s ON s.id = g.person WHERE g.image = ?1 AND g.status = 'confirmed' ORDER BY g.x") else { return Vec::new() };
    st.query_map([image], |r| Ok(crate::xmp::Region { name: r.get(0)?, x: r.get::<_, f64>(1)? as f32, y: r.get::<_, f64>(2)? as f32, b: r.get::<_, f64>(3)? as f32, h: r.get::<_, f64>(4)? as f32 }))
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
}

/// Dimensions of the upright image (swapped for EXIF rotation 5–8).
fn region_dims(db: &Db, file: i64) -> Option<(u32, u32)> {
    let (b, h, o): (Option<i64>, Option<i64>, Option<i64>) = db.conn.query_row("SELECT width, height, orientation FROM file WHERE id=?1", [file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()?;
    let (b, h) = (b.filter(|x| *x > 0)? as u32, h.filter(|x| *x > 0)? as u32);
    Some(if matches!(o, Some(5..=8)) { (h, b) } else { (b, h) })
}

/// Merge copies: keep the settings (own and those taken over from Lightroom) from the file.
fn merge_copies(db_k: &str, file_k: Option<&str>) -> Option<String> {
    let mut k = sidecar::read_copies(Some(db_k));
    let old = sidecar::read_copies(file_k);
    for x in &mut k {
        if let Some(a) = old.iter().find(|a| a.name == x.name) {
            x.settings = a.settings.clone();
            x.lightroom = a.lightroom.clone();
            x.lightroom_orientation = a.lightroom_orientation.clone();
        }
    }
    sidecar::copies_text(&k)
}

pub(crate) fn write_one(db: &Db, file: i64, path: &Path, format: &str, raw_beside: bool, old: Option<&Values>) -> R<()> {
    let Some((_, mut w)) = db_values(db, file)? else { return Ok(()) };
    if let (Some(k), Some(a)) = (w.copies_json.clone(), old) {
        w.copies_json = merge_copies(&k, a.copies_json.as_deref());
    }
    sidecar::write(path, format, raw_beside, &w)?;
    Ok(())
}

/// After a change in the library: the sidecar next to the photo follows in the background, if writing sidecars is
/// switched on ([`crate::store`]; the library is the main store). Off: nothing is written.
pub fn write_sidecar(db: &Db, file: i64) -> R<()> {
    crate::store::queue_sidecar(&db.conn, file)
}

pub(crate) fn file_info(db: &Db, file: i64) -> R<(std::path::PathBuf, String)> {
    let p = db.file_path(file).ok_or("Datei unbekannt")?;
    let fm: String = db.conn.query_row("SELECT format FROM file WHERE id=?1", [file], |r| r.get(0)).map_err(f)?;
    Ok((p, fm))
}

/// Is there a RAW with the same name in the folder (for the XMP name of a JPEG/HEIC)?
pub fn raw_beside(image: &Path, format: &str) -> bool {
    if crate::is_raw(format) {
        return false;
    }
    let Some(stem) = image.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else { return false };
    let Some(dir) = image.parent() else { return false };
    std::fs::read_dir(dir).map_or(false, |rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            let p = Path::new(&n);
            p.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase() == stem) && crate::format_from_extension(&n).is_some_and(crate::is_raw)
        })
    })
}

pub(crate) fn equal_kw(a: &[String], b: &[String]) -> bool {
    let x: BTreeSet<String> = a.iter().map(|s| crate::search::fold_text(s)).collect();
    let y: BTreeSet<String> = b.iter().map(|s| crate::search::fold_text(s)).collect();
    x == y
}

/// Sync one file. `write`: write missing/older sidecars (else only report).
pub fn reconcile_file(db: &Db, file: i64, path: &Path, format: &str, raw: bool, write: bool) -> R<Outcome> {
    reconcile_file_read(db, file, path, format, raw, write, sidecar::read(path, format, raw))
}

/// Like [`reconcile_file`] with a previously read sidecar (`sidecar::read`) – the sync reads
/// the files outside the write lock.
pub fn reconcile_file_read(db: &Db, file: i64, path: &Path, format: &str, raw: bool, write: bool, read_result: (ReadResult, Option<i64>)) -> R<Outcome> {
    let mut e = Outcome::default();
    let Some((orig, db_vals)) = db_values(db, file)? else { return Ok(e) };
    let (g, mtime) = read_result;
    // a file time in the future (clock of a NAS ahead) must not win over every later change in the library
    let mtime = mtime.map(|m| m.min(crate::now()));
    // the first reading of this photo's sidecar (after the update, a new photo, a rebuild): copies and a capture
    // time correction come from it. Later (the library is the main store) they are never taken back from a sidecar:
    // they carry no time of their own, an old sidecar would bring back a deleted copy or an undone correction.
    let first_read: bool = db.conn.query_row("SELECT xmp_read IS NULL FROM file WHERE id=?1", [file], |r| r.get(0)).optional().map_err(f)?.unwrap_or(true);
    let w = match g {
        ReadResult::Missing | ReadResult::Broken => {
            e.broken = g == ReadResult::Broken;
            if !sidecar::empty(&db_vals) {
                if write {
                    write_one(db, file, path, format, raw, None)?;
                    e.new = true;
                } else {
                    e.would_write = true;
                }
            }
            return Ok(e);
        }
        ReadResult::Ok(w) => w,
    };
    let mut file_outdated = false;
    // rating
    if !w.has_rating {
        if (db_vals.stars, db_vals.flag, db_vals.label) != (0, 0, 0) {
            file_outdated = true;
            e.in_file.push("Bewertung");
        }
    } else if (w.stars, w.flag, w.label) != (db_vals.stars, db_vals.flag, db_vals.label) {
        let t_file = w.rating_time.or(mtime).unwrap_or(0);
        let t_db = db_vals.rating_time.unwrap_or(0);
        if t_file >= t_db {
            db.conn
                .execute("UPDATE image SET stars=?2, flag=?3, label=?4, rating_time=?5 WHERE id=?1", params![orig, w.stars, w.flag, w.label, t_file])
                .map_err(f)?;
            e.from_file.push("Bewertung");
        } else {
            file_outdated = true;
            e.in_file.push("Bewertung");
        }
    }
    // keywords
    if !w.has_keywords {
        if !db_vals.keywords.is_empty() {
            file_outdated = true;
            e.in_file.push("Stichwörter");
        }
    } else if !equal_kw(&w.keywords, &db_vals.keywords) {
        let t_file = w.keyword_time.or(mtime).unwrap_or(0);
        let t_db = db_vals.keyword_time.unwrap_or(0);
        if t_file >= t_db {
            set_keywords(db, orig, &w.keywords, t_file, false)?;
            e.from_file.push("Stichwörter");
        } else {
            file_outdated = true;
            e.in_file.push("Stichwörter");
        }
    }
    // copies: union by name (only at the first reading, see above)
    let in_file = sidecar::read_copies(w.copies_json.as_deref());
    let in_db = sidecar::read_copies(db_vals.copies_json.as_deref());
    let mut new_db = false;
    for k in &in_file {
        if first_read && !in_db.iter().any(|x| x.name == k.name) {
            db.conn
                .execute(&format!("INSERT INTO image(id, file, copy, stars, flag, label, rating_time) VALUES({},?1,?2,?3,?4,?5,?6)", crate::merge::NEW_IMAGE_ID), params![file, k.name, k.stars, k.flag, k.label, mtime])
                .map_err(f)?;
            let id = db.conn.last_insert_rowid();
            set_keywords(db, id, &k.keywords, mtime.unwrap_or(0), false)?;
            new_db = true;
        }
    }
    if new_db {
        e.from_file.push("Kopien");
    }
    if in_db.iter().any(|k| !in_file.iter().any(|x| x.name == k.name)) {
        file_outdated = true;
        e.in_file.push("Kopien");
    }
    // capture time: at the first reading a correction in the sidecar applies; later the library's state stays (and
    // goes into the sidecar if it is written)
    if w.time_correction != db_vals.time_correction {
        match &w.time_correction {
            Some((new, original_time)) if first_read => {
                db.conn.execute("UPDATE file SET capture_time=?2, capture_time_original=?3 WHERE id=?1", params![file, new, original_time]).map_err(f)?;
                e.from_file.push("Aufnahmezeit");
            }
            _ => {
                file_outdated = true;
                e.in_file.push("Aufnahmezeit");
            }
        }
    }
    if in_file.iter().any(|k| !in_db.iter().any(|x| x.name == k.name)) && !first_read {
        file_outdated = true;
    }
    if file_outdated {
        if write {
            write_one(db, file, path, format, raw, Some(&w))?;
        } else {
            e.would_write = true;
        }
    }
    Ok(e)
}

/// Create a virtual copy (values from the original, name "Kopie N" or as given). Returns: image ID.
/// The copy belongs where its original belongs: the same import ("Zuletzt importiert") and the same collections
/// (also the quick collection), so it is shown in the view it was made in and can be opened there.
pub fn create_copy(db: &Db, image: i64, name: Option<&str>) -> R<i64> {
    let (file, stars, mark, et, run): (i64, i64, i64, i64, Option<i64>) =
        db.conn.query_row("SELECT file, stars, flag, label, run FROM image WHERE id=?1", [image], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(f)?;
    let names: Vec<String> = {
        let mut st = db.conn.prepare("SELECT copy FROM image WHERE file=?1 AND copy IS NOT NULL").map_err(f)?;
        let r = st.query_map([file], |r| r.get(0)).map_err(f)?.flatten().collect();
        r
    };
    let name = match name {
        Some(n) if !n.trim().is_empty() && !names.iter().any(|x| x == n.trim()) => n.trim().to_string(),
        _ => (1..).map(|i| format!("Kopie {i}")).find(|n| !names.contains(n)).unwrap_or_default(),
    };
    let t = crate::now();
    crate::location::note_before_change(db, &[file]);
    db.conn
        .execute(&format!("INSERT INTO image(id, file, copy, stars, flag, label, rating_time, changed, run) VALUES({},?1,?2,?3,?4,?5,?6,?6,?7)", crate::merge::NEW_IMAGE_ID), params![file, name, stars, mark, et, t, run])
        .map_err(f)?;
    let id = db.conn.last_insert_rowid();
    // right after the original in each of its collections (a manual order keeps the pair together)
    db.conn
        .execute(
            "INSERT OR IGNORE INTO collection_image(collection, image, position)
             SELECT collection, ?2, position + 0.5 FROM collection_image WHERE image = ?1",
            params![image, id],
        )
        .map_err(f)?;
    let ids = crate::keywords::ids_of(&db.conn, image)?;
    crate::keywords::set_ids(db, id, &ids, t, true)?;
    write_sidecar(db, file)?;
    Ok(id)
}

/// Change keywords of several images (add/remove typed texts, see [`crate::keywords::change_text`]) and write the
/// sidecars. Returns the number of files touched.
pub fn change_keywords(db: &Db, images: &[i64], added: &[String], remove_fn: &[String]) -> R<usize> {
    crate::keywords::change_text(db, images, added, remove_fn)
}
