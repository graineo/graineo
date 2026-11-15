//! RAW + JPEG twice in the library: the same shot was imported as two photos (RAW and camera JPEG, e.g. from
//! Lightroom, where both are images of their own). This module finds such pairs, grades how sure each one is and
//! removes the JPEGs of the sure ones – on request only; nothing here runs by itself.
//!
//! Only photos of the library count (files in `file`). Offline copies (small copies of RAWs on a NAS in the
//! preview cache) are not photos of the library: they are never read here, never counted and never touched.
//!
//! Levels ([`Level`]):
//! - **Safe:** same file name (without extension, upper/lower case ignored), same capture time (± 1 s), same camera,
//!   matching aspect ratio and size, the RAW is there and reachable, the JPEG has no development of its own.
//! - **Probable:** same capture time and camera, same folder, but another name (or the aspect ratio/size differs).
//!   Only when exactly one JPEG fits one RAW – bursts (same second, several photos) never end up here.
//! - **RAW not reachable:** the RAW file is missing or on a drive that is not connected: the JPEG stays for now.
//! - **Own data:** the JPEG has a development of its own or virtual copies: it stays.
//!
//! Ratings, flag, label, keywords, persons, caption and collections of a JPEG the RAW does not have are carried over
//! to the RAW before the JPEG goes ([`Transfer`]): only added, never overwritten.
//!
//! HEIC is left out on purpose: a camera HEIC next to a RAW is rare and Apple Photos (HEIC) has other names; only `JPG`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};

use crate::db::Db;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Capture times may differ by this many seconds.
pub const TIME_TOLERANCE: i64 = 1;
/// The aspect ratios (long side / short side) may differ by this share.
const ASPECT_TOLERANCE: f64 = 0.02;
/// A JPEG with fewer pixels than this share of the RAW looks like a resized export, not like the camera's JPEG.
const MIN_PIXEL_SHARE: f64 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Safe,
    Probable,
    RawUnreachable,
    OwnData,
}

/// One photo (original) as the detection needs it.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub image: i64,
    pub file: i64,
    pub folder: i64,
    pub path: PathBuf,
    pub name: String,
    pub format: String,
    pub size: i64,
    pub camera: String,
    /// seconds of the capture time (local time as seconds, only compared with each other)
    pub time: i64,
    pub capture: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    /// 0 present, 1 missing, 2 drive not connected
    pub status: u8,
    /// origin of its root folder: 'lightroom', 'own' (own folders), 'apple-photos'
    pub origin: String,
}

/// What of the JPEG the RAW lacks and gets before the JPEG goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transfer {
    pub stars: Option<u8>,
    pub flag: Option<i8>,
    pub label: Option<u8>,
    pub caption: Option<String>,
    /// keywords (including persons) the RAW does not have
    pub keywords: Vec<i64>,
    /// collections the RAW is not in: (collection, position)
    pub collections: Vec<(i64, Option<f64>)>,
}

impl Transfer {
    pub fn is_empty(&self) -> bool {
        self.stars.is_none() && self.flag.is_none() && self.label.is_none() && self.caption.is_none() && self.keywords.is_empty() && self.collections.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Pair {
    pub raw: Item,
    pub jpeg: Item,
    pub level: Level,
    /// same name (without extension)
    pub same_name: bool,
    pub transfer: Transfer,
    /// development of its own or virtual copies at the JPEG
    pub own_data: bool,
    /// why the JPEG has data of its own (bits of [`reason`]; 0 = none)
    pub reasons: u8,
    /// aspect ratio and pixel count of the JPEG fit the RAW (false: perhaps a resized export)
    pub shape_ok: bool,
}

/// Reasons why a JPEG keeps data of its own (bits in [`Pair::reasons`]).
pub mod reason {
    /// developed in graineo
    pub const DEVELOP: u8 = 1;
    /// marked as edited in Lightroom
    pub const LIGHTROOM: u8 = 2;
    /// has virtual copies
    pub const COPIES: u8 = 4;
}

/// Name without extension, lower case.
pub fn stem(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(s, _)| s).to_lowercase()
}

/// 'YYYY-MM-DDThh:mm:ss' (or with a space) as seconds; `None` if not readable.
pub fn seconds(capture: &str) -> Option<i64> {
    let b = capture.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |a: usize, z: usize| capture.get(a..z)?.parse::<i64>().ok();
    let (y, m, d, h, mi, s) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // days from the civil date (proleptic Gregorian)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

/// Pixel size as shown (orientation 5–8: width and height swapped).
fn shown(i: &Item) -> Option<(f64, f64)> {
    let (w, h) = (i.width? as f64, i.height? as f64);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(if matches!(i.orientation, Some(5..=8)) { (h, w) } else { (w, h) })
}

/// Do aspect ratio and size fit (unknown sizes do not speak against it)?
fn shape_fits(raw: &Item, jpeg: &Item) -> bool {
    let (Some((rw, rh)), Some((jw, jh))) = (shown(raw), shown(jpeg)) else { return true };
    // long/short side: a camera JPEG and its RAW are the same orientation or both are rotated alike
    let (ra, ja) = (rw.max(rh) / rw.min(rh), jw.max(jh) / jw.min(jh));
    if (ra - ja).abs() / ra > ASPECT_TOLERANCE {
        return false;
    }
    (jw * jh) >= (rw * rh) * MIN_PIXEL_SHARE
}

/// How far a pair departs from a camera's own RAW+JPEG (0 = fits exactly; larger = more to check): another name (1),
/// time difference (share of [`TIME_TOLERANCE`]), aspect ratio (share of its tolerance) and missing pixels of the
/// JPEG (resized export: the share below the RAW's pixel count, scaled to [`MIN_PIXEL_SHARE`]). Unknown sizes add nothing.
pub fn deviation(p: &Pair) -> f64 {
    let mut d = if p.same_name { 0.0 } else { 1.0 };
    d += (p.raw.time - p.jpeg.time).abs() as f64 / TIME_TOLERANCE.max(1) as f64;
    if let (Some((rw, rh)), Some((jw, jh))) = (shown(&p.raw), shown(&p.jpeg)) {
        let (ra, ja) = (rw.max(rh) / rw.min(rh), jw.max(jh) / jw.min(jh));
        d += (ra - ja).abs() / ra / ASPECT_TOLERANCE;
        d += (1.0 - (jw * jh) / (rw * rh)).max(0.0) / (1.0 - MIN_PIXEL_SHARE);
    }
    d
}

fn same_camera(a: &Item, b: &Item) -> bool {
    !a.camera.trim().is_empty() && a.camera.trim().eq_ignore_ascii_case(b.camera.trim())
}

/// The pure detection: pairs of RAW and JPEG with the level "safe", "probable" or "RAW not reachable".
/// (Own data is judged afterwards with the library, see [`find`].)
pub fn detect(raws: &[Item], jpegs: &[Item]) -> Vec<Pair> {
    // RAWs by name and by second
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_time: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, r) in raws.iter().enumerate() {
        by_name.entry(stem(&r.name)).or_default().push(i);
        by_time.entry(r.time).or_default().push(i);
    }
    let near = |t: i64| (t - TIME_TOLERANCE..=t + TIME_TOLERANCE).flat_map(|s| by_time.get(&s).into_iter().flatten().copied()).collect::<Vec<_>>();
    // 1. same name → safe
    let mut taken: HashSet<usize> = HashSet::new();
    let mut out: Vec<Pair> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (ji, j) in jpegs.iter().enumerate() {
        let mut fit: Vec<usize> = by_name.get(&stem(&j.name)).into_iter().flatten().copied().filter(|&ri| (raws[ri].time - j.time).abs() <= TIME_TOLERANCE && same_camera(&raws[ri], j)).collect();
        if fit.is_empty() {
            open.push(ji);
            continue;
        }
        // the best of several equal ones: the present one next to the JPEG
        fit.sort_by_key(|&ri| (raws[ri].status != 0, raws[ri].folder != j.folder, ri));
        let ri = fit[0];
        taken.insert(ri);
        let shape = shape_fits(&raws[ri], j);
        out.push(make(&raws[ri], j, true, shape));
    }
    // 2. another name, same folder, one to one → probable (bursts: several candidates → nothing)
    let mut cand: HashMap<usize, Vec<usize>> = HashMap::new(); // jpeg → raws
    let mut back: HashMap<usize, Vec<usize>> = HashMap::new(); // raw → jpegs
    for &ji in &open {
        let j = &jpegs[ji];
        for ri in near(j.time) {
            let r = &raws[ri];
            if r.folder == j.folder && same_camera(r, j) && stem(&r.name) != stem(&j.name) {
                cand.entry(ji).or_default().push(ri);
                back.entry(ri).or_default().push(ji);
            }
        }
    }
    // a RAW that already has a JPEG with its name is not a candidate for a differently named one
    for (ji, rs) in cand {
        if let [ri] = rs[..] {
            if back.get(&ri).is_some_and(|v| v.len() == 1) && !taken.contains(&ri) {
                out.push(make(&raws[ri], &jpegs[ji], false, shape_fits(&raws[ri], &jpegs[ji])));
            }
        }
    }
    out.sort_by(|a, b| (a.level, &a.jpeg.path).cmp(&(b.level, &b.jpeg.path)));
    out
}

fn make(raw: &Item, jpeg: &Item, same_name: bool, shape: bool) -> Pair {
    let level = if raw.status != 0 {
        Level::RawUnreachable
    } else if same_name && shape {
        Level::Safe
    } else {
        Level::Probable
    };
    Pair { raw: raw.clone(), jpeg: jpeg.clone(), level, same_name, transfer: Transfer::default(), own_data: false, reasons: 0, shape_ok: shape }
}

fn items(db: &Db) -> R<(Vec<Item>, Vec<Item>)> {
    let mut st = db
        .conn
        .prepare(
            "SELECT b.id, d.id, d.folder, s.path || o.path || d.name, d.name, d.format, coalesce(d.size, 0), d.camera, d.capture_time, d.width, d.height, d.orientation, d.status, s.origin
             FROM image b JOIN file d ON d.id = b.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
             WHERE b.copy IS NULL AND d.status IN (0, 1, 2) AND d.camera IS NOT NULL AND d.capture_time IS NOT NULL",
        )
        .map_err(f)?;
    let (mut raws, mut jpegs) = (Vec::new(), Vec::new());
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, i64>(12)?,
                r.get::<_, String>(13)?,
            ))
        })
        .map_err(f)?;
    for row in rows.flatten() {
        let (image, file, folder, path, name, format, size, camera, capture, width, height, orientation, status, origin) = row;
        let is_jpeg = format == "JPG";
        if !is_jpeg && !crate::is_raw(&format) {
            continue;
        }
        let Some(time) = seconds(&capture) else { continue };
        let item = Item { image, file, folder, path: PathBuf::from(path), name, format, size, camera, time, capture, width, height, orientation, status: status as u8, origin };
        // a JPEG that is not there or not reachable is nothing to remove
        if is_jpeg {
            if item.status == 0 {
                jpegs.push(item);
            }
        } else {
            raws.push(item);
        }
    }
    Ok((raws, jpegs))
}

/// Development of its own, Lightroom edits or virtual copies at this file (bits of [`reason`]).
fn own_reasons(db: &Db, file: i64) -> R<u8> {
    let copies: i64 = db.conn.prepare_cached("SELECT count(*) FROM image WHERE file = ?1 AND copy IS NOT NULL").and_then(|mut st| st.query_row([file], |r| r.get(0))).map_err(f)?;
    let (develop, lightroom): (i64, i64) = db
        .conn
        .prepare_cached("SELECT coalesce(sum(settings IS NOT NULL), 0), coalesce(sum(lr_edited <> 0), 0) FROM develop_state WHERE file = ?1")
        .and_then(|mut st| st.query_row([file], |r| Ok((r.get(0)?, r.get(1)?))))
        .map_err(f)?;
    let mut bits = 0;
    if develop > 0 {
        bits |= reason::DEVELOP;
    }
    if lightroom > 0 {
        bits |= reason::LIGHTROOM;
    }
    if copies > 0 {
        bits |= reason::COPIES;
    }
    Ok(bits)
}

/// What the JPEG has that the RAW does not.
pub fn transfer_of(db: &Db, jpeg_image: i64, raw_image: i64) -> R<Transfer> {
    type Meta = (i64, i64, i64, Option<String>);
    let get = |b: i64| -> R<Meta> { db.conn.query_row("SELECT stars, flag, label, caption FROM image WHERE id = ?1", [b], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(f) };
    let (j, r) = (get(jpeg_image)?, get(raw_image)?);
    let mut t = Transfer::default();
    if j.0 > 0 && r.0 == 0 {
        t.stars = Some(j.0.clamp(0, 5) as u8);
    }
    if j.1 != 0 && r.1 == 0 {
        t.flag = Some(j.1.clamp(-1, 1) as i8);
    }
    if j.2 > 0 && r.2 == 0 {
        t.label = Some(j.2.clamp(0, 5) as u8);
    }
    if j.3.as_deref().is_some_and(|s| !s.trim().is_empty()) && r.3.as_deref().is_none_or(|s| s.trim().is_empty()) {
        t.caption = j.3.clone();
    }
    let ids = |sql: &str, b: i64| -> R<HashSet<i64>> {
        let mut st = db.conn.prepare(sql).map_err(f)?;
        let v = st.query_map([b], |x| x.get::<_, i64>(0)).map_err(f)?.flatten().collect();
        Ok(v)
    };
    // keywords (persons are keywords as well) and persons confirmed at a face
    let mut jk = ids("SELECT keyword FROM image_keyword WHERE image = ?1", jpeg_image)?;
    jk.extend(ids("SELECT person FROM face WHERE image = ?1 AND status = 'confirmed' AND person IS NOT NULL", jpeg_image)?);
    let rk = ids("SELECT keyword FROM image_keyword WHERE image = ?1", raw_image)?;
    t.keywords = jk.difference(&rk).copied().collect();
    t.keywords.sort_unstable();
    let rc = ids("SELECT collection FROM collection_image WHERE image = ?1", raw_image)?;
    let mut st = db.conn.prepare("SELECT collection, position FROM collection_image WHERE image = ?1").map_err(f)?;
    let mut cols: Vec<(i64, Option<f64>)> = st.query_map([jpeg_image], |x| Ok((x.get(0)?, x.get(1)?))).map_err(f)?.flatten().filter(|(c, _)| !rc.contains(c)).collect();
    cols.sort_by_key(|c| c.0);
    t.collections = cols;
    Ok(t)
}

/// All pairs of the library (reads only), with their final level and what would be carried over.
pub fn find(db: &Db) -> R<Vec<Pair>> {
    let (raws, jpegs) = items(db)?;
    let mut pairs = detect(&raws, &jpegs);
    for p in &mut pairs {
        p.reasons = own_reasons(db, p.jpeg.file)?;
        p.own_data = p.reasons != 0;
        if p.own_data {
            p.level = Level::OwnData;
        }
        p.transfer = transfer_of(db, p.jpeg.image, p.raw.image)?;
    }
    pairs.sort_by(|a, b| (a.level, &a.jpeg.path).cmp(&(b.level, &b.jpeg.path)));
    Ok(pairs)
}

/// Counts for the summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub safe: usize,
    pub probable: usize,
    pub unreachable: usize,
    pub own_data: usize,
    /// of the sure ones: with ratings/keywords … to carry over
    pub safe_with_transfer: usize,
    /// size of the JPEGs of the sure ones
    pub safe_bytes: u64,
}

pub fn summarize(pairs: &[Pair]) -> Summary {
    let mut s = Summary::default();
    for p in pairs {
        match p.level {
            Level::Safe => {
                s.safe += 1;
                s.safe_bytes += p.jpeg.size.max(0) as u64;
                if !p.transfer.is_empty() {
                    s.safe_with_transfer += 1;
                }
            }
            Level::Probable => s.probable += 1,
            Level::RawUnreachable => s.unreachable += 1,
            Level::OwnData => s.own_data += 1,
        }
    }
    s
}

/// Carry the data of the JPEG over to the RAW (only adding).
pub fn apply_transfer(db: &Db, raw_image: i64, raw_file: i64, t: &Transfer) -> R<()> {
    if t.is_empty() {
        return Ok(());
    }
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    transfer_in(&tx, raw_image, t)?;
    tx.commit().map_err(f)?;
    // the RAW's sidecar (only if writing sidecars is on)
    let _ = crate::store::queue_sidecar(&db.conn, raw_file);
    Ok(())
}

/// The writes of [`apply_transfer`] inside a transaction the caller holds.
fn transfer_in(tx: &rusqlite::Connection, raw_image: i64, t: &Transfer) -> R<()> {
    let now = crate::now();
    if let Some(s) = t.stars {
        tx.execute("UPDATE image SET stars = ?2, rating_time = ?3, changed = ?3 WHERE id = ?1 AND stars = 0", params![raw_image, s as i64, now]).map_err(f)?;
    }
    if let Some(m) = t.flag {
        tx.execute("UPDATE image SET flag = ?2, rating_time = ?3, changed = ?3 WHERE id = ?1 AND flag = 0", params![raw_image, m as i64, now]).map_err(f)?;
    }
    if let Some(l) = t.label {
        tx.execute("UPDATE image SET label = ?2, rating_time = ?3, changed = ?3 WHERE id = ?1 AND label = 0", params![raw_image, l as i64, now]).map_err(f)?;
    }
    if let Some(c) = &t.caption {
        tx.execute("UPDATE image SET caption = ?2, changed = ?3 WHERE id = ?1 AND (caption IS NULL OR trim(caption) = '')", params![raw_image, c, now]).map_err(f)?;
    }
    if !t.keywords.is_empty() {
        for k in &t.keywords {
            tx.execute("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1, ?2)", params![raw_image, k]).map_err(f)?;
        }
        tx.execute("UPDATE image SET keywords_time = ?2, keywords_changed = ?2, changed = ?2 WHERE id = ?1", params![raw_image, now]).map_err(f)?;
    }
    for (c, pos) in &t.collections {
        tx.execute("INSERT OR IGNORE INTO collection_image(collection, image, position) VALUES(?1, ?2, ?3)", params![c, raw_image, pos]).map_err(f)?;
    }
    Ok(())
}

// ───────────────────────────────────────────── Removing

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposal {
    /// into the Trash (a network drive: into its trash folder), restorable
    Trash,
    /// deleted for good
    Permanent,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub disposal: Disposal,
    /// carry ratings/keywords … over to the RAW (off: JPEGs with such data stay)
    pub transfer: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub removed: usize,
    pub bytes: u64,
    pub transferred: usize,
    /// (JPEG name, reason): stayed
    pub skipped: Vec<(String, String)>,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

/// The files that may go for this pair, checked fresh against the library and the disk – or the reason why not.
/// Only a regular file that the library knows as JPEG, inside its root folder, whose RAW is a regular, non-empty file.
pub fn verify(db: &Db, p: &Pair) -> R<(PathBuf, Vec<PathBuf>)> {
    let path_of = |image: i64| -> R<(PathBuf, String, PathBuf, u8)> {
        let (path, format, root, status): (String, String, String, i64) = db
            .conn
            .query_row(
                "SELECT s.path || o.path || d.name, d.format, s.path, d.status FROM image b JOIN file d ON d.id = b.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE b.id = ?1 AND b.copy IS NULL",
                [image],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(f)?
            .ok_or_else(|| "nicht mehr in der Bibliothek".to_string())?;
        Ok((PathBuf::from(path), format, PathBuf::from(root), status as u8))
    };
    let (jpath, jformat, jroot, jstatus) = path_of(p.jpeg.image)?;
    let (rpath, rformat, _, rstatus) = path_of(p.raw.image)?;
    if jformat != "JPG" || !crate::is_raw(&rformat) {
        return Err("Formate haben sich geändert".into());
    }
    if jstatus != 0 || rstatus != 0 {
        return Err("Datei fehlt oder Laufwerk nicht verbunden".into());
    }
    if p.jpeg.file == p.raw.file {
        return Err("gleiche Datei".into());
    }
    if crate::apple_photos::in_photos_library(&jpath) {
        return Err("Foto aus Apple Fotos".into());
    }
    // the RAW: a real file with content
    let rm = std::fs::symlink_metadata(&rpath).map_err(|_| "RAW-Datei nicht erreichbar".to_string())?;
    if !rm.is_file() || rm.len() == 0 {
        return Err("RAW-Datei nicht lesbar".into());
    }
    // the JPEG: a real file (no link), inside its root folder
    let jm = std::fs::symlink_metadata(&jpath).map_err(|_| "JPEG-Datei nicht erreichbar".to_string())?;
    if !jm.is_file() {
        return Err("JPEG ist keine normale Datei".into());
    }
    let (cj, cr) = (jpath.canonicalize().map_err(|e| e.to_string())?, jroot.canonicalize().map_err(|e| e.to_string())?);
    if !cj.starts_with(&cr) {
        return Err("JPEG liegt außerhalb des Stammordners".into());
    }
    if rpath.canonicalize().ok().as_deref() == Some(cj.as_path()) {
        return Err("RAW und JPEG sind dieselbe Datei".into());
    }
    // sidecar of the JPEG: only "NAME.JPG.xmp" – never "NAME.xmp", that belongs to the RAW of the same name
    let mut files = vec![jpath.clone()];
    let name = jpath.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let xmp = jpath.with_file_name(format!("{name}.xmp"));
    if std::fs::symlink_metadata(&xmp).is_ok_and(|m| m.is_file()) {
        files.push(xmp);
    }
    Ok((jpath, files))
}

/// Pairs per block: the files of a block go in one call (one Trash request), the library entries in one transaction.
pub const BLOCK: usize = crate::trash_batch::BLOCK;

/// Remove the JPEGs of these pairs (the caller picks them; only the checks above decide per pair whether it really
/// goes). Works in blocks of [`BLOCK`] pairs: checks per pair, then the carried-over data in one transaction, then
/// `dispose` once with the files of all checked pairs (Trash or deleting; returns how many it took), then the library
/// entries in one transaction. If `dispose` fails, each JPEG still on disk counts as an error, each one gone as
/// removed. `progress(done, total)` comes per pair; `cancel` is polled per pair and ends the run after the current
/// block's checked pairs are finished cleanly.
pub fn remove(db: &Db, pairs: &[Pair], opt: Options, dispose: &dyn Fn(&Db, &[PathBuf]) -> R<usize>, progress: &dyn Fn(usize, usize), cancel: &dyn Fn() -> bool) -> Outcome {
    struct Ready<'a> {
        pair: &'a Pair,
        files: Vec<PathBuf>,
        transfer: Transfer,
        size: u64,
    }
    let mut out = Outcome::default();
    let mut done_images: HashSet<i64> = HashSet::new();
    let mut done = 0;
    for block in pairs.chunks(BLOCK) {
        // 1. checks, fresh against the library and the disk
        let mut ready: Vec<Ready> = Vec::with_capacity(block.len());
        for p in block {
            progress(done, pairs.len());
            if cancel() {
                out.cancelled = true;
                break;
            }
            done += 1;
            let name = p.jpeg.name.clone();
            if matches!(p.level, Level::OwnData | Level::RawUnreachable) || p.own_data {
                out.skipped.push((name, "hat eigene Daten oder RAW nicht erreichbar".into()));
                continue;
            }
            if !done_images.insert(p.jpeg.image) {
                continue;
            }
            let files = match verify(db, p) {
                Ok((_, files)) => files,
                Err(e) => {
                    out.skipped.push((name, e));
                    continue;
                }
            };
            // develop state may have changed since the list was made
            match own_reasons(db, p.jpeg.file) {
                Ok(0) => {}
                Ok(_) => {
                    out.skipped.push((name, "inzwischen bearbeitet".into()));
                    continue;
                }
                Err(e) => {
                    out.errors.push(format!("{name}: {e}"));
                    continue;
                }
            }
            let transfer = match transfer_of(db, p.jpeg.image, p.raw.image) {
                Ok(t) => t,
                Err(e) => {
                    out.errors.push(format!("{name}: {e}"));
                    continue;
                }
            };
            if !transfer.is_empty() && !opt.transfer {
                out.skipped.push((name, "hat Bewertung/Stichwörter, die das RAW nicht hat".into()));
                continue;
            }
            let size = std::fs::symlink_metadata(&files[0]).map(|m| m.len()).unwrap_or(0);
            ready.push(Ready { pair: p, files, transfer, size });
        }
        if !ready.is_empty() {
            // 2. carry data over to the RAWs (one transaction; only adding)
            if ready.iter().any(|r| !r.transfer.is_empty()) {
                let written = db.conn.unchecked_transaction().map_err(f).and_then(|tx| {
                    for r in ready.iter().filter(|r| !r.transfer.is_empty()) {
                        transfer_in(&tx, r.pair.raw.image, &r.transfer)?;
                    }
                    tx.commit().map_err(f)
                });
                match written {
                    Ok(()) => {
                        for r in ready.iter().filter(|r| !r.transfer.is_empty()) {
                            out.transferred += 1;
                            let _ = crate::store::queue_sidecar(&db.conn, r.pair.raw.file);
                        }
                    }
                    Err(e) => {
                        // nothing was carried over: these JPEGs stay
                        for r in ready.iter().filter(|r| !r.transfer.is_empty()) {
                            out.errors.push(format!("{}: {e}", r.pair.jpeg.name));
                        }
                        ready.retain(|r| r.transfer.is_empty());
                    }
                }
            }
            // 3. the files, all at once
            let files: Vec<PathBuf> = ready.iter().flat_map(|r| r.files.iter().cloned()).collect();
            let gone: Vec<&Ready> = match dispose(db, &files) {
                Ok(_) => ready.iter().collect(),
                Err(e) => {
                    let (gone, stayed): (Vec<&Ready>, Vec<&Ready>) = ready.iter().partition(|r| std::fs::symlink_metadata(&r.files[0]).is_err());
                    for r in stayed {
                        out.errors.push(format!("{}: {e}", r.pair.jpeg.name));
                    }
                    gone
                }
            };
            // 4. the library entries (one transaction)
            let images: Vec<i64> = gone.iter().map(|r| r.pair.jpeg.image).collect();
            if !images.is_empty() {
                match db.remove_images(&images, &[]) {
                    Ok(_) => {
                        out.removed += gone.len();
                        out.bytes += gone.iter().map(|r| r.size).sum::<u64>();
                    }
                    Err(e) => out.errors.extend(gone.iter().map(|r| format!("{}: {e}", r.pair.jpeg.name))),
                }
            }
        }
        if out.cancelled {
            break;
        }
    }
    progress(done, pairs.len());
    out
}

/// Deleting for good (only called after the user chose it in the confirmation).
pub fn delete_permanently(files: &[PathBuf]) -> R<usize> {
    crate::backup_view::refuse()?;
    let mut n = 0;
    for p in files {
        // once more: a regular file only, never a link or a folder
        if !std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()) {
            return Err(format!("{}: keine normale Datei", p.display()));
        }
        std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
        n += 1;
    }
    Ok(n)
}

/// Take files out of the way: Trash or deleting for good. The Trash goes through the shared block building block
/// [`crate::trash_batch`] (also used by cleaning up duplicates): all local files in ONE request to `local` (the
/// system Trash, passed in by the app; each request is slow, about a second), files on a network drive into the
/// trash folder of their drive, grouped by root folder.
pub fn dispose(db: &Db, files: &[PathBuf], disposal: Disposal, local: &dyn Fn(&[PathBuf]) -> R<()>) -> R<usize> {
    if disposal == Disposal::Permanent {
        return delete_permanently(files);
    }
    // the network check per folder, not per file
    let folder_is_network: std::cell::RefCell<HashMap<PathBuf, bool>> = Default::default();
    let is_network = |folder: &Path| *folder_is_network.borrow_mut().entry(folder.to_path_buf()).or_insert_with(|| crate::network_trash::is_network(folder));
    let root = |p: &Path| root_of(db, p);
    let t = crate::trash_batch::Targets { local, is_network: &is_network, root_of: &root, day: crate::card_import::today() };
    crate::trash_batch::dispose_all(files, &t)
}

/// Where the files of this pair live: root folder path (for the trash folder of a network drive).
pub fn root_of(db: &Db, path: &Path) -> Option<PathBuf> {
    path.parent().and_then(|d| db.root_to_path(d)).map(|r| PathBuf::from(r.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(image: i64, folder: i64, name: &str, format: &str, cam: &str, time: &str) -> Item {
        Item {
            image,
            file: image,
            folder,
            path: PathBuf::from(format!("/t/{folder}/{name}")),
            name: name.into(),
            format: format.into(),
            size: 1000,
            camera: cam.into(),
            time: seconds(time).unwrap(),
            capture: time.into(),
            width: Some(6000),
            height: Some(4000),
            orientation: Some(1),
            status: 0,
            origin: "own".into(),
        }
    }

    #[test]
    fn deviation_grows_with_name_time_and_shape() {
        let r = item(1, 1, "IMG_1.CR2", "CR2", "C", "2024-05-11T14:03:22");
        let exact = make(&r, &item(2, 1, "IMG_1.JPG", "JPG", "C", "2024-05-11T14:03:22"), true, true);
        assert_eq!(deviation(&exact), 0.0);
        let later = make(&r, &item(2, 1, "IMG_1.JPG", "JPG", "C", "2024-05-11T14:03:23"), true, true);
        assert_eq!(deviation(&later), 1.0);
        let renamed = make(&r, &item(2, 1, "X.JPG", "JPG", "C", "2024-05-11T14:03:22"), false, true);
        assert_eq!(deviation(&renamed), 1.0);
        let mut small = item(2, 1, "IMG_1.JPG", "JPG", "C", "2024-05-11T14:03:22");
        (small.width, small.height) = (Some(3000), Some(2000));
        let small = make(&r, &small, true, false);
        assert!(deviation(&small) > 2.0, "{}", deviation(&small));
        let mut square = item(2, 1, "IMG_1.JPG", "JPG", "C", "2024-05-11T14:03:22");
        square.width = Some(4000);
        assert!(deviation(&make(&r, &square, true, false)) > deviation(&later));
    }

    #[test]
    fn seconds_counts_calendar_correctly() {
        assert_eq!(seconds("1970-01-01T00:00:00"), Some(0));
        assert_eq!(seconds("2024-03-01T00:00:00").unwrap() - seconds("2024-02-28T00:00:00").unwrap(), 2 * 86400);
        assert_eq!(seconds("2024-05-11 14:03:23").unwrap() - seconds("2024-05-11T14:03:22").unwrap(), 1);
        assert_eq!(seconds("kaputt"), None);
    }

    #[test]
    fn same_name_time_camera_is_safe() {
        let raws = [item(1, 1, "IMG_0001.CR3", "CR3", "Canon EOS R", "2024-05-11T14:03:22")];
        let jpegs = [item(2, 1, "img_0001.JPG", "JPG", "Canon EOS R", "2024-05-11T14:03:23")];
        let p = detect(&raws, &jpegs);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].level, Level::Safe);
        assert!(p[0].same_name);
    }

    #[test]
    fn safe_also_in_another_folder() {
        let raws = [item(1, 1, "IMG_0001.CR3", "CR3", "Canon EOS R", "2024-05-11T14:03:22")];
        let jpegs = [item(2, 2, "IMG_0001.JPG", "JPG", "Canon EOS R", "2024-05-11T14:03:22")];
        assert_eq!(detect(&raws, &jpegs)[0].level, Level::Safe);
    }

    #[test]
    fn same_name_other_camera_or_time_is_nothing() {
        let raws = [item(1, 1, "IMG_0001.CR3", "CR3", "Canon EOS R", "2024-05-11T14:03:22")];
        assert!(detect(&raws, &[item(2, 1, "IMG_0001.JPG", "JPG", "Canon EOS RP", "2024-05-11T14:03:22")]).is_empty());
        assert!(detect(&raws, &[item(2, 1, "IMG_0001.JPG", "JPG", "Canon EOS R", "2024-05-11T14:03:25")]).is_empty());
        // another day, same counter (card rolled over)
        assert!(detect(&raws, &[item(2, 1, "IMG_0001.JPG", "JPG", "Canon EOS R", "2025-01-02T09:00:00")]).is_empty());
    }

    #[test]
    fn burst_with_other_names_is_not_sure() {
        // three RAWs and three JPEGs in the same second, same camera, other numbers
        let raws: Vec<Item> = (0..3).map(|i| item(10 + i, 1, &format!("A_{i}.CR3"), "CR3", "Canon", "2024-05-11T14:03:22")).collect();
        let jpegs: Vec<Item> = (0..3).map(|i| item(20 + i, 1, &format!("B_{i}.JPG"), "JPG", "Canon", "2024-05-11T14:03:22")).collect();
        assert!(detect(&raws, &jpegs).is_empty());
    }

    #[test]
    fn other_name_same_folder_one_to_one_is_probable() {
        let raws = [item(1, 1, "R0001.DNG", "DNG", "Ricoh GR", "2024-05-11T14:03:22")];
        let jpegs = [item(2, 1, "R0001_x.JPG", "JPG", "Ricoh GR", "2024-05-11T14:03:22")];
        let p = detect(&raws, &jpegs);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].level, Level::Probable);
        // other folder: nothing
        assert!(detect(&raws, &[item(2, 2, "R0001_x.JPG", "JPG", "Ricoh GR", "2024-05-11T14:03:22")]).is_empty());
    }

    #[test]
    fn raw_not_reachable_is_its_own_level() {
        let mut r = item(1, 1, "IMG_0001.CR3", "CR3", "Canon", "2024-05-11T14:03:22");
        r.status = 2;
        let p = detect(&[r], &[item(2, 1, "IMG_0001.JPG", "JPG", "Canon", "2024-05-11T14:03:22")]);
        assert_eq!(p[0].level, Level::RawUnreachable);
    }

    #[test]
    fn aspect_and_size_must_fit_for_safe() {
        let raw = item(1, 1, "IMG_0001.CR3", "CR3", "Canon", "2024-05-11T14:03:22");
        let mut square = item(2, 1, "IMG_0001.JPG", "JPG", "Canon", "2024-05-11T14:03:22");
        square.height = Some(6000);
        assert_eq!(detect(std::slice::from_ref(&raw), &[square])[0].level, Level::Probable);
        let mut small = item(2, 1, "IMG_0001.JPG", "JPG", "Canon", "2024-05-11T14:03:22");
        small.width = Some(2000);
        small.height = Some(1333);
        assert_eq!(detect(std::slice::from_ref(&raw), &[small])[0].level, Level::Probable);
        // portrait JPEG with rotation tag next to landscape-stored RAW with the same tag: fine
        let mut rot = item(2, 1, "IMG_0001.JPG", "JPG", "Canon", "2024-05-11T14:03:22");
        rot.width = Some(4000);
        rot.height = Some(6000);
        rot.orientation = Some(6);
        assert_eq!(detect(std::slice::from_ref(&raw), &[rot])[0].level, Level::Safe);
        // unknown size does not speak against it
        let mut unknown = item(2, 1, "IMG_0001.JPG", "JPG", "Canon", "2024-05-11T14:03:22");
        unknown.width = None;
        assert_eq!(detect(&[raw], &[unknown])[0].level, Level::Safe);
    }

    #[test]
    fn no_camera_no_pair() {
        let raws = [item(1, 1, "IMG_0001.CR3", "CR3", "", "2024-05-11T14:03:22")];
        let jpegs = [item(2, 1, "IMG_0001.JPG", "JPG", "", "2024-05-11T14:03:22")];
        assert!(detect(&raws, &jpegs).is_empty());
    }
}
