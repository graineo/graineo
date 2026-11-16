//! Faces for the people view, kept in memory: one compact row per face (no feature vectors), loaded once in the
//! background (110 000 faces ≈ 0.2 s, 9 MB). Filtering, counting and sorting then happen without the database, so a
//! click never waits for it. Also: small face images from the preview cache (instead of opening the original), and
//! snapshots of faces for undo.

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{params, Connection};

/// What the person sees about a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    /// a person confirmed the name (or it came confirmed from Lightroom/Apple)
    Confirmed,
    /// suggestion with a clear lead (similarity ≥ 0.70, margin ≥ 0.10)
    SuggestSure,
    /// other suggestions
    SuggestMaybe,
    /// confirmed name, but the comparison says it looks more like someone else
    Doubtful,
    /// no name, in a group of similar faces
    UnnamedGroup,
    /// no name, alone
    Unnamed,
    /// "do not name"
    Rejected,
}

pub const ALL_STATUS: [Status; 7] = [Status::Confirmed, Status::SuggestSure, Status::SuggestMaybe, Status::Doubtful, Status::UnnamedGroup, Status::Unnamed, Status::Rejected];

impl Status {
    /// A question for the user (suggestion or doubt)?
    pub fn is_question(self) -> bool {
        matches!(self, Status::SuggestSure | Status::SuggestMaybe | Status::Doubtful)
    }
}

/// Where the face comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Apple,
    /// assigned automatically from an unnamed Apple group
    AppleGroup,
    Lightroom,
    Own,
    Hand,
}

pub const ALL_SOURCES: [Source; 5] = [Source::Apple, Source::AppleGroup, Source::Lightroom, Source::Own, Source::Hand];

/// One face.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub id: i64,
    pub image: i64,
    /// name carried (confirmed), suggested (suggestion) or entered (doubtful)
    pub person: Option<i64>,
    /// doubtful: the person it looks more like
    pub suspect: Option<i64>,
    pub status: Status,
    /// suggestion: similarity and second-best person; doubtful: own value and value of the suspect
    pub value: f32,
    pub second: f32,
    pub group: Option<i64>,
    pub source: Source,
    pub quality: f32,
    /// year of capture (0 = unknown)
    pub year: u16,
    /// capture time as text (sorting: newest first)
    pub capture: String,
    pub file_name: String,
}

impl Face {
    /// How clear a question is (higher first).
    pub fn certainty(&self) -> f32 {
        self.value - self.second
    }
}

/// A person (keyword marked as person).
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub hidden: bool,
}

/// All faces and people.
#[derive(Default, Clone)]
pub struct FaceIndex {
    pub faces: Vec<Face>,
    /// alphabetically
    pub people: Vec<Person>,
    pub by_id: HashMap<i64, usize>,
}

/// Similarity and margin from which a suggestion counts as "very likely" (the thresholds of the matching).
pub fn sure(value: f32, second: f32) -> bool {
    let s = crate::people::Thresholds::default();
    value >= s.safe && value - second >= s.distance
}

const SELECT: &str = "SELECT g.id, g.image, g.person, g.status, g.suspect, g.guessed, g.local, coalesce(g.confidence, 0), coalesce(g.score_own, 0),
        coalesce(g.score_guessed, 0), g.cluster, g.source, g.apple_id IS NOT NULL, coalesce(g.quality, 0), coalesce(d.capture_time, ''), coalesce(d.name, '')
     FROM face g JOIN image b ON b.id = g.image JOIN file d ON d.id = b.file";

fn row(r: &rusqlite::Row, hidden: &std::collections::HashSet<i64>) -> rusqlite::Result<Option<Face>> {
    let person: Option<i64> = r.get(2)?;
    let state: String = r.get(3)?;
    let local: i64 = r.get(6)?;
    // faces decided by the user are never doubtful; automatic ones (also from Apple groups) are
    let doubtful = r.get::<_, i64>(4)? == 1 && r.get::<_, Option<i64>>(5)?.is_some() && local != 1;
    let (safety, own, suspected) = (r.get::<_, f64>(7)? as f32, r.get::<_, f64>(8)? as f32, r.get::<_, f64>(9)? as f32);
    let group: Option<i64> = r.get(10)?;
    let (status, value, second) = match state.as_str() {
        "confirmed" if doubtful => (Status::Doubtful, suspected, own),
        "confirmed" => (Status::Confirmed, 0.0, 0.0),
        "suggested" => {
            // suggestions of hidden people are not shown (as before)
            if person.is_none_or(|p| hidden.contains(&p)) {
                return Ok(None);
            }
            (if sure(safety, own) { Status::SuggestSure } else { Status::SuggestMaybe }, safety, own)
        }
        "rejected" => (Status::Rejected, 0.0, 0.0),
        _ if group.is_some() => (Status::UnnamedGroup, 0.0, 0.0),
        _ => (Status::Unnamed, 0.0, 0.0),
    };
    let source_str: String = r.get(11)?;
    let source = if local == crate::apple_groups::LOCAL_APPLE_GROUP {
        Source::AppleGroup
    } else if r.get::<_, bool>(12)? || source_str == "apple" {
        Source::Apple
    } else {
        match source_str.as_str() {
            "lightroom" => Source::Lightroom,
            "manual" => Source::Hand,
            _ => Source::Own,
        }
    };
    let capture: String = r.get(14)?;
    Ok(Some(Face {
        id: r.get(0)?,
        image: r.get(1)?,
        person: if matches!(status, Status::Unnamed | Status::UnnamedGroup | Status::Rejected) { None } else { person },
        suspect: if status == Status::Doubtful { r.get(5)? } else { None },
        status,
        value,
        second,
        group: if status == Status::UnnamedGroup { group } else { None },
        source,
        quality: r.get::<_, f64>(13)? as f32,
        year: capture.get(..4).and_then(|y| y.parse().ok()).unwrap_or(0),
        capture,
        file_name: r.get(15)?,
    }))
}

fn hidden(c: &Connection) -> std::collections::HashSet<i64> {
    c.prepare("SELECT keyword FROM person_info WHERE hidden=1").and_then(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect())).unwrap_or_default()
}

/// Load all faces and people (in a background thread).
pub fn load(c: &Connection) -> Result<FaceIndex, String> {
    let hidden = hidden(c);
    let mut st = c.prepare(SELECT).map_err(|e| e.to_string())?;
    let faces: Vec<Face> = st.query_map([], |r| row(r, &hidden)).map_err(|e| e.to_string())?.flatten().flatten().collect();
    let people = load_people(c, &hidden);
    let by_id = faces.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    Ok(FaceIndex { faces, people, by_id })
}

fn load_people(c: &Connection, hidden: &std::collections::HashSet<i64>) -> Vec<Person> {
    let mut v: Vec<Person> = c
        .prepare("SELECT id, name FROM keyword WHERE person=1")
        .and_then(|mut s| s.query_map([], |r| Ok(Person { id: r.get(0)?, name: r.get(1)?, hidden: false })).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for p in v.iter_mut() {
        p.hidden = hidden.contains(&p.id);
    }
    v.sort_by_cached_key(|p| (crate::search::fold_text(&p.name), p.id));
    v
}

/// Read some faces again (after writing): the current rows, `None` for faces that no longer exist.
pub fn reload_faces(c: &Connection, ids: &[i64]) -> Vec<(i64, Option<Face>)> {
    let hidden = hidden(c);
    let Ok(mut st) = c.prepare(&format!("{SELECT} WHERE g.id = ?1")) else { return Vec::new() };
    ids.iter().map(|id| (*id, st.query_row([id], |r| row(r, &hidden)).ok().flatten())).collect()
}

/// Read the people again (after a rename or hiding).
pub fn reload_people(c: &Connection) -> Vec<Person> {
    load_people(c, &hidden(c))
}

impl FaceIndex {
    /// Replace or remove faces by id (after writing or optimistically).
    pub fn apply(&mut self, rows: Vec<(i64, Option<Face>)>) {
        for (id, f) in rows {
            match (self.by_id.get(&id).copied(), f) {
                (Some(i), Some(f)) => self.faces[i] = f,
                (Some(i), None) => {
                    self.faces[i].status = Status::Rejected;
                    self.faces[i].person = None;
                }
                (None, Some(f)) => {
                    self.by_id.insert(f.id, self.faces.len());
                    self.faces.push(f);
                }
                (None, None) => {}
            }
        }
    }

    pub fn face(&self, id: i64) -> Option<&Face> {
        self.by_id.get(&id).map(|&i| &self.faces[i])
    }

    pub fn face_mut(&mut self, id: i64) -> Option<&mut Face> {
        self.by_id.get(&id).copied().map(move |i| &mut self.faces[i])
    }

    pub fn name(&self, person: i64) -> &str {
        self.people.iter().find(|p| p.id == person).map_or("?", |p| p.name.as_str())
    }
}

// ───────────────────────────────────────────── Face images

/// Edge of a face image in the cache (px).
pub const THUMB: u32 = 192;

/// Folder of the face images (next to the preview cache).
pub fn thumb_folder() -> PathBuf {
    crate::cache_folder().with_file_name("faces")
}

/// Face image of a face on photo `image` with this box. The name carries a tag of photo and box: a face image made
/// for another place is never shown – the box moved (Lightroom or Apple Photos sync, repair), the face moved to
/// another photo, or a new face got the number of a removed one (SQLite gives the highest numbers again).
pub fn thumb_file(face: i64, image: i64, x: f64, y: f64, b: f64, h: f64) -> PathBuf {
    thumb_folder().join(format!("{face}-{:08x}.jpg", box_tag(image, x, y, b, h)))
}

/// Short tag (FNV-1a, 32 bits) of photo and box.
fn box_tag(image: i64, x: f64, y: f64, b: f64, h: f64) -> u32 {
    let mut v: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in image.to_le_bytes().into_iter().chain([x, y, b, h].into_iter().flat_map(|f| f.to_bits().to_le_bytes())) {
        v ^= byte as u64;
        v = v.wrapping_mul(0x0100_0000_01b3);
    }
    (v ^ (v >> 32)) as u32
}

/// Face image of a face as it is stored now (`None`: no such face).
pub fn thumb_path(c: &Connection, face: i64) -> Option<PathBuf> {
    c.query_row("SELECT image, x, y, w, h FROM face WHERE id=?1", [face], |r| Ok(thumb_file(face, r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).ok()
}

/// Revision of the face images. Before 2 a face image could come from another image than the one its box refers to
/// (a photo rotated in Lightroom only, a photo cropped in Apple Photos); before 3 it was named after the face number
/// only and stayed when the box or photo of the face changed. [`purge_outdated`] tidies up once.
pub const THUMB_REVISION: u32 = 3;

/// The whole photo, reduced into the square (dark edges): shown when a crop would not surely show the face.
fn whole(img: &image::RgbImage, side: u32) -> image::RgbImage {
    let s = side as f64 / img.width().max(img.height()).max(1) as f64;
    let (w, h) = (((img.width() as f64 * s).round() as u32).clamp(1, side), ((img.height() as f64 * s).round() as u32).clamp(1, side));
    let small = image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
    let mut out = image::RgbImage::from_pixel(side, side, image::Rgb([22, 22, 24]));
    image::imageops::replace(&mut out, &small, ((side - w) / 2) as i64, ((side - h) / 2) as i64);
    out
}

/// Square crop around the box (slightly enlarged), scaled to `side`.
fn crop(img: &image::RgbImage, x: f64, y: f64, b: f64, h: f64, side: u32) -> image::RgbImage {
    let (w, hh) = (img.width() as f64, img.height() as f64);
    let (cx, cy, s) = ((x + b / 2.0) * w, (y + h / 2.0) * hh, (b * w).max(h * hh) * 1.35);
    let (x0, y0) = ((cx - s / 2.0).max(0.0), (cy - s / 2.0).max(0.0));
    let a = image::imageops::crop_imm(img, x0 as u32, y0 as u32, (s as u32).min(img.width().saturating_sub(x0 as u32)).max(1), (s as u32).min(img.height().saturating_sub(y0 as u32)).max(1)).to_image();
    image::imageops::resize(&a, side, side, image::imageops::FilterType::Triangle)
}

/// Where a face image came from (for measuring).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbSource {
    Present,
    PreviewCache,
    Original,
    /// the crop would not surely show the face (the image has another shape than the photo): the whole photo
    Whole,
}

/// Make the face image if it is missing: from the camera preview in the preview cache (a few ms) when it is large enough
/// and has the shape of the photo, otherwise from the image the box refers to ([`crate::face_frame::load`], 50–150 ms,
/// more on a network drive). Developed previews are not used: a crop would move the box. If even that image does not
/// have the shape of the photo, the whole photo is shown reduced instead of a crop at a wrong place.
pub fn ensure_thumb(db: &crate::db::Db, face: i64) -> Result<ThumbSource, String> {
    let (image, x, y, b, h): (i64, f64, f64, f64, f64) = db.conn.query_row("SELECT image, x, y, w, h FROM face WHERE id=?1", [face], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(|e| e.to_string())?;
    let target = thumb_file(face, image, x, y, b, h);
    if target.exists() {
        return Ok(ThumbSource::Present);
    }
    let p = crate::face_frame::of(db, image)?;
    // a box outside the photo would only show an edge or a wrong place: the whole photo instead
    let inside = crate::face_frame::box_in_photo(x, y, b, h);
    let cache = crate::cache_folder();
    let mut from_cache = None;
    for level in [crate::preview::small(), crate::preview::large()] {
        let f = crate::preview::file_in(&cache, p.key, level);
        let Ok(bytes) = std::fs::read(&f) else { continue };
        if crate::preview::is_developed_bytes(&bytes) {
            continue;
        }
        let Ok(img) = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) else { continue };
        let img = img.to_rgb8();
        // shape like the photo (a camera preview with another crop would move the box; in the cache the photo is
        // already upright as the library shows it) and the face large enough
        let face_px = (b * img.width() as f64).max(h * img.height() as f64);
        if p.upright.is_some() && crate::face_frame::fits(&p, &img) && face_px >= 72.0 {
            from_cache = Some(img);
            break;
        }
    }
    let (small, source) = match from_cache {
        Some(i) if inside => (crop(&i, x, y, b, h, THUMB), ThumbSource::PreviewCache),
        Some(i) => (whole(&i, THUMB), ThumbSource::Whole),
        None => match crate::face_frame::load(&p, 1600) {
            Ok(i) if inside && crate::face_frame::fits(&p, &i) => (crop(&i, x, y, b, h, THUMB), ThumbSource::Original),
            Ok(i) => (whole(&i, THUMB), ThumbSource::Whole),
            Err(_) => (whole(&crate::face_frame::load_plain(&p, 1600)?, THUMB), ThumbSource::Whole),
        },
    };
    if let Some(d) = target.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let part = target.with_extension("teil");
    small.save_with_format(&part, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
    Ok(source)
}

/// The whole photo in the frame of its face boxes, at most `max` pixels edge length, for the large view and the
/// navigator of the people view: the camera preview from the preview cache when it has the photo's shape and is large
/// enough (a few ms), otherwise the image the boxes refer to ([`crate::face_frame::load`]). Second value: the boxes fit
/// this image (otherwise the photo is shown without boxes).
pub fn photo_for_boxes(db: &crate::db::Db, image: i64, max: u32) -> Result<(image::RgbImage, bool), String> {
    let p = crate::face_frame::of(db, image)?;
    let cache = crate::cache_folder();
    let f = crate::preview::file_in(&cache, p.key, crate::preview::large());
    if let Some(img) = std::fs::read(&f).ok().filter(|b| !crate::preview::is_developed_bytes(b)).and_then(|b| image::load_from_memory_with_format(&b, image::ImageFormat::Jpeg).ok()) {
        let img = img.to_rgb8();
        if p.upright.is_some() && crate::face_frame::fits(&p, &img) && img.width().max(img.height()) >= max.min(1024) {
            return Ok((img, true));
        }
    }
    match crate::face_frame::load(&p, max) {
        Ok(i) => {
            let fits = crate::face_frame::fits(&p, &i);
            Ok((i, fits))
        }
        Err(_) => Ok((crate::face_frame::load_plain(&p, max)?, false)),
    }
}

/// Faces worth having an image ready: open questions, examples of groups, one face per person.
pub fn prefill_list(c: &Connection) -> Vec<i64> {
    let q = "SELECT id FROM face WHERE status='suggested'
             UNION SELECT id FROM face WHERE suspect=1 AND status='confirmed' AND local<>1
             UNION SELECT id FROM face WHERE cluster IS NOT NULL AND person IS NULL AND status<>'rejected'";
    c.prepare(q).and_then(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect())).unwrap_or_default()
}

/// The faces of [`prefill_list`] whose face image is not made yet.
pub fn missing_thumbs(c: &Connection) -> Vec<i64> {
    let ids = prefill_list(c);
    let Ok(mut st) = c.prepare_cached("SELECT image, x, y, w, h FROM face WHERE id=?1") else { return ids };
    ids.into_iter().filter(|&id| st.query_row([id], |r| Ok(thumb_file(id, r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).is_ok_and(|p| !p.exists())).collect()
}

/// Make the missing face images of `ids` quietly (one thread, background priority); stops when `cancel` is set.
/// Returns: (made, of these from the preview cache).
pub fn prefill(db: &crate::db::Db, ids: &[i64], cancel: &std::sync::atomic::AtomicBool) -> (usize, usize) {
    let (mut made, mut cached) = (0, 0);
    for &id in ids {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match ensure_thumb(db, id) {
            Ok(ThumbSource::PreviewCache) => {
                made += 1;
                cached += 1;
            }
            Ok(ThumbSource::Original | ThumbSource::Whole) => made += 1,
            _ => {}
        }
    }
    (made, cached)
}

fn revision_file() -> PathBuf {
    thumb_folder().join(".revision")
}

/// Once per [`THUMB_REVISION`]: the face images of older revisions go; they are made again when shown (or quietly
/// by [`prefill`]), from the preview cache in a few ms each. Before 2 they could come from another image than the one
/// the box refers to; before 3 they were named after the face number only and stayed when the box or photo of the
/// face changed (Lightroom update with turned boxes, Apple Photos sync, a new face given the number of a removed one)
/// – which of them still fit cannot be told without looking. Stops when `cancel` is set (then it runs again next
/// time). Returns: images removed.
pub fn purge_outdated(cancel: &std::sync::atomic::AtomicBool) -> usize {
    if std::fs::read_to_string(revision_file()).ok().and_then(|s| s.trim().parse::<u32>().ok()).is_some_and(|r| r >= THUMB_REVISION) {
        return 0;
    }
    let old: Vec<PathBuf> = std::fs::read_dir(thumb_folder())
        .map(|rd| rd.flatten().filter(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".jpg")).is_some_and(|n| n.parse::<i64>().is_ok())).map(|e| e.path()).collect())
        .unwrap_or_default();
    let mut removed = 0;
    for p in old {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return removed;
        }
        removed += usize::from(std::fs::remove_file(p).is_ok());
    }
    let _ = std::fs::create_dir_all(thumb_folder());
    let _ = std::fs::write(revision_file(), THUMB_REVISION.to_string());
    removed
}

/// Face images left behind by faces whose box or photo changed, or that were removed: once a week, quietly.
/// Returns: images removed.
pub fn tidy(c: &Connection, cancel: &std::sync::atomic::AtomicBool) -> usize {
    let mark = thumb_folder().join(".tidy");
    let week = std::time::Duration::from_secs(7 * 24 * 3600);
    if std::fs::metadata(&mark).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e < week) {
        return 0;
    }
    let removed = tidy_folder(&thumb_folder(), c, cancel);
    if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::write(&mark, b"");
    }
    removed
}

fn tidy_folder(folder: &std::path::Path, c: &Connection, cancel: &std::sync::atomic::AtomicBool) -> usize {
    let Ok(rd) = std::fs::read_dir(folder) else { return 0 };
    let Ok(mut st) = c.prepare("SELECT image, x, y, w, h FROM face WHERE id=?1") else { return 0 };
    let mut removed = 0;
    for e in rd.flatten() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return removed;
        }
        let name = e.file_name();
        let Some((id, tag)) = name.to_str().and_then(|n| n.strip_suffix(".jpg")).and_then(|n| n.split_once('-')) else { continue };
        let Ok(id) = id.parse::<i64>() else { continue };
        let current = st.query_row([id], |r| Ok(box_tag(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).ok();
        if current.is_none_or(|t| format!("{t:08x}") != tag) {
            removed += usize::from(std::fs::remove_file(e.path()).is_ok());
        }
    }
    removed
}

// ───────────────────────────────────────────── Undo

/// State of some faces before a change: their rows, their excluded names and the keywords of their photos.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    faces: Vec<(i64, Option<i64>, String, i64, i64, Option<i64>, Option<f64>, Option<f64>, Option<f64>, Option<i64>)>,
    never: Vec<(i64, i64)>,
    /// keyword ids per photo before the change (by id: names are never split into paths)
    keywords: Vec<(i64, Vec<i64>)>,
    /// names of people (a rename changes them)
    names: Vec<(i64, String)>,
    pub face_ids: Vec<i64>,
}

/// Remember the faces before a change (and, for a rename, the person).
pub fn snapshot(db: &crate::db::Db, faces: &[i64], person: Option<i64>) -> Result<Snapshot, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let mut s = Snapshot::default();
    let mut ids: Vec<i64> = faces.to_vec();
    if let Some(p) = person {
        let mut st = db.conn.prepare("SELECT id FROM face WHERE person=?1 OR guessed=?1").map_err(fe)?;
        ids.extend(st.query_map([p], |r| r.get::<_, i64>(0)).map_err(fe)?.flatten());
        if let Ok(n) = db.conn.query_row("SELECT name FROM keyword WHERE id=?1", [p], |r| r.get::<_, String>(0)) {
            s.names.push((p, n));
        }
    }
    ids.sort_unstable();
    ids.dedup();
    let mut images: Vec<i64> = Vec::new();
    {
        let mut st = db.conn.prepare("SELECT image, person, status, local, suspect, guessed, confidence, score_own, score_guessed, cluster FROM face WHERE id=?1").map_err(fe)?;
        let mut never = db.conn.prepare("SELECT person FROM face_never WHERE face=?1").map_err(fe)?;
        for &id in &ids {
            let Ok((image, row)) = st.query_row([id], |r| Ok((r.get::<_, i64>(0)?, (id, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)))) else { continue };
            s.faces.push(row);
            images.push(image);
            for p in never.query_map([id], |r| r.get::<_, i64>(0)).map_err(fe)?.flatten() {
                s.never.push((id, p));
            }
        }
    }
    images.sort_unstable();
    images.dedup();
    for image in images {
        s.keywords.push((image, crate::keywords::ids_of(&db.conn, image)?));
    }
    s.face_ids = ids;
    Ok(s)
}

/// Put the faces back as they were (rows, excluded names, keywords of the photos; sidecars are written if switched on).
pub fn restore(db: &crate::db::Db, s: &Snapshot) -> Result<(), String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let tx = db.conn.unchecked_transaction().map_err(fe)?;
    for (p, n) in &s.names {
        tx.execute("UPDATE keyword SET name=?2 WHERE id=?1", params![p, n]).map_err(fe)?;
    }
    for (id, person, state, local, suspect, suspected, value, own, other, group) in &s.faces {
        tx.execute(
            "UPDATE face SET person=?2, status=?3, local=?4, suspect=?5, guessed=?6, confidence=?7, score_own=?8, score_guessed=?9, cluster=?10 WHERE id=?1",
            params![id, person, state, local, suspect, suspected, value, own, other, group],
        )
        .map_err(fe)?;
        tx.execute("DELETE FROM face_never WHERE face=?1", [id]).map_err(fe)?;
    }
    for (g, p) in &s.never {
        tx.execute("INSERT OR IGNORE INTO face_never(face, person) VALUES(?1, ?2)", params![g, p]).map_err(fe)?;
    }
    tx.commit().map_err(fe)?;
    for (image, ids) in &s.keywords {
        if crate::keywords::ids_of(&db.conn, *image)? != *ids {
            crate::keywords::set_ids(db, *image, ids, crate::now(), true)?;
            if let Ok(file) = db.conn.query_row("SELECT file FROM image WHERE id=?1", [image], |r| r.get::<_, i64>(0)) {
                let _ = crate::sync::write_sidecar(db, file);
            }
        }
    }
    Ok(())
}

/// A quick fingerprint of the suggestions and doubts: changes when the background run computed them anew.
pub fn fingerprint(c: &Connection) -> String {
    c.query_row(
        "SELECT count(*), total(person), total(confidence), total(cluster) FROM face WHERE status='suggested' OR cluster IS NOT NULL OR suspect=1",
        [],
        |r| Ok(format!("{}|{}|{:.4}|{}", r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?)),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn library() -> (Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("graineo-face-index-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1,'/x/','x','own'); INSERT INTO folder(id, root, path) VALUES(1,1,'');
                 INSERT INTO keyword(id, name, person) VALUES(10,'Anna',1),(11,'Ben',1);
                 INSERT INTO file(id, folder, name, format, capture_time) VALUES(1,1,'a.jpg','JPG','2024-05-01T10:00:00'),(2,1,'b.jpg','JPG','2023-01-01T10:00:00');
                 INSERT INTO image(id, file) VALUES(1,1),(2,2);
                 INSERT INTO face(id, image, x, y, w, h, source, person, status, local) VALUES(1,1,0.1,0.1,0.2,0.2,'lightroom',10,'confirmed',0);
                 INSERT INTO face(id, image, x, y, w, h, source, person, status, confidence, score_own) VALUES(2,2,0.1,0.1,0.2,0.2,'graineo',10,'suggested',0.8,0.4);
                 INSERT INTO face(id, image, x, y, w, h, source, apple_id, status, cluster) VALUES(3,2,0.5,0.1,0.2,0.2,'apple','A1','unknown',3);",
            )
            .unwrap();
        (db, dir)
    }

    #[test]
    fn load_maps_status_source_and_year() {
        let (db, dir) = library();
        let ix = load(&db.conn).unwrap();
        let f = |id| ix.face(id).unwrap().clone();
        assert_eq!((f(1).status, f(1).source, f(1).year), (Status::Confirmed, Source::Lightroom, 2024));
        assert_eq!((f(2).status, f(2).source, f(2).person), (Status::SuggestSure, Source::Own, Some(10)));
        assert_eq!((f(3).status, f(3).source, f(3).group), (Status::UnnamedGroup, Source::Apple, Some(3)));
        assert_eq!(ix.people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Anna", "Ben"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// JPEG with an optional EXIF orientation (minimal APP1 block).
    fn jpeg(img: &image::RgbImage, orientation: Option<u16>) -> Vec<u8> {
        let mut b = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut b, 95).encode_image(img).unwrap();
        let Some(o) = orientation else { return b };
        let mut tiff = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        tiff.extend(o.to_le_bytes());
        tiff.extend([0, 0, 0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend(((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        out.extend(&b[2..]);
        out
    }

    /// Grey photo `w × h` with a red block (x0, y0, side) – "the face".
    fn photo(w: u32, h: u32, x0: u32, y0: u32, side: u32) -> image::RgbImage {
        let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([128, 128, 128]));
        for y in y0..y0 + side {
            for x in x0..x0 + side {
                img.put_pixel(x, y, image::Rgb([230, 20, 20]));
            }
        }
        img
    }

    fn is_red(p: &image::Rgb<u8>) -> bool {
        p.0[0] > 180 && p.0[1] < 90 && p.0[2] < 90
    }

    /// Library with one photo (`dir`/`name`, stored size, orientation) and one face (`id`) with the given box.
    fn one_face(dir: &std::path::Path, root: &str, folder: &str, name: &str, size: (i64, i64), orientation: Option<i64>, id: i64, rect: (f64, f64, f64, f64)) -> Db {
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        db.conn.execute("INSERT INTO root_folder(id, path, name, origin) VALUES(1, ?1, 'x', 'own')", [root]).unwrap();
        db.conn.execute("INSERT INTO folder(id, root, path) VALUES(1, 1, ?1)", [folder]).unwrap();
        db.conn
            .execute("INSERT INTO file(id, folder, name, format, width, height, orientation, size, mtime) VALUES(1, 1, ?1, ?2, ?3, ?4, ?5, 1, 1)", params![name, crate::format_from_extension(name).unwrap(), size.0, size.1, orientation])
            .unwrap();
        db.conn.execute("INSERT INTO image(id, file) VALUES(1, 1)", []).unwrap();
        db.conn.execute("INSERT INTO face(id, image, x, y, w, h, source, status) VALUES(?1, 1, ?2, ?3, ?4, ?5, 'lightroom', 'unknown')", params![id, rect.0, rect.1, rect.2, rect.3]).unwrap();
        db
    }

    fn thumb_center(db: &Db, id: i64) -> image::Rgb<u8> {
        let t = image::open(thumb_path(&db.conn, id).unwrap()).unwrap().to_rgb8();
        *t.get_pixel(t.width() / 2, t.height() / 2)
    }

    /// A photo rotated in Lightroom only (file landscape without orientation, library: 90° clockwise): the box refers
    /// to the upright photo, the face image must be cut from the rotated image (formerly: from the file as it lies).
    #[cfg(target_os = "macos")]
    #[test]
    fn face_image_of_photo_rotated_in_lightroom_only() {
        let dir = std::env::temp_dir().join(format!("graineo-face-turn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // file 400 × 300, face at x 300..360, y 40..100; turned 90° clockwise (300 × 400): x = 300 - 1 - y, y = x
        std::fs::write(dir.join("t.jpg"), jpeg(&photo(400, 300, 300, 40, 60), None)).unwrap();
        let root = format!("{}/", dir.display());
        let (x, y) = ((300.0 - 100.0) / 300.0, 300.0 / 400.0);
        let db = one_face(&dir, &root, "", "t.jpg", (400, 300), Some(6), 9_100_001, (x, y, 60.0 / 300.0, 60.0 / 400.0));
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_001).unwrap());
        assert_eq!(ensure_thumb(&db, 9_100_001).unwrap(), ThumbSource::Original);
        assert!(is_red(&thumb_center(&db, 9_100_001)), "{:?}", thumb_center(&db, 9_100_001));
        // the face search sees the same image
        let f = crate::face_frame::of(&db, 1).unwrap();
        assert_eq!(crate::face_frame::misframed(&f), Some(1));
        let img = crate::face_frame::load(&f, 1600).unwrap();
        assert_eq!((img.width(), img.height()), (300, 400));
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_001).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A camera JPEG standing upright through its own EXIF orientation (Lightroom agrees): unchanged behaviour.
    #[cfg(target_os = "macos")]
    #[test]
    fn face_image_of_photo_with_exif_orientation() {
        let dir = std::env::temp_dir().join(format!("graineo-face-exif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("e.jpg"), jpeg(&photo(400, 300, 300, 40, 60), Some(6))).unwrap();
        let root = format!("{}/", dir.display());
        let (x, y) = ((300.0 - 100.0) / 300.0, 300.0 / 400.0);
        let db = one_face(&dir, &root, "", "e.jpg", (400, 300), Some(6), 9_100_002, (x, y, 60.0 / 300.0, 60.0 / 400.0));
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_002).unwrap());
        ensure_thumb(&db, 9_100_002).unwrap();
        assert!(is_red(&thumb_center(&db, 9_100_002)), "{:?}", thumb_center(&db, 9_100_002));
        assert_eq!(crate::face_frame::misframed(&crate::face_frame::of(&db, 1).unwrap()), None);
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_002).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Apple Photos, photo cropped there (screenshot 200 × 400 → upper part 200 × 200): Apple's boxes refer to the
    /// current version. The original is on this Mac – the face image comes from Photos' current version, not from the
    /// original (formerly: from the original, a wrong place). Without a current version: the whole photo.
    #[cfg(target_os = "macos")]
    #[test]
    fn face_image_of_photo_cropped_in_apple_photos() {
        let lib = std::env::temp_dir().join(format!("graineo-face-apple-{}.photoslibrary", std::process::id()));
        let uuid = "A1B2C3D4-0000-4000-8000-000000000001";
        std::fs::create_dir_all(lib.join("originals/A")).unwrap();
        std::fs::create_dir_all(lib.join("resources/derivatives/A")).unwrap();
        // original: face (red) in the lower part at y 300..360; in the cropped current version (upper half) the face
        // is at x 120..180, y 120..180 – where the original only shows grey
        let mut original = photo(200, 400, 20, 300, 60);
        for y in 120..180 {
            for x in 120..180 {
                original.put_pixel(x, y, image::Rgb([20, 20, 200]));
            }
        }
        original.save(lib.join(format!("originals/A/{uuid}.png"))).unwrap();
        std::fs::write(lib.join(format!("resources/derivatives/A/{uuid}_1_105_c.jpeg")), jpeg(&photo(200, 200, 120, 120, 60), None)).unwrap();
        let root = format!("{}/originals/", lib.display());
        let db = one_face(&lib, &root, "A/", &format!("{uuid}.png"), (200, 200), None, 9_100_003, (0.6, 0.6, 0.3, 0.3));
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_003).unwrap());
        assert_eq!(ensure_thumb(&db, 9_100_003).unwrap(), ThumbSource::Original);
        assert!(is_red(&thumb_center(&db, 9_100_003)), "{:?}", thumb_center(&db, 9_100_003));
        let f = crate::face_frame::of(&db, 1).unwrap();
        assert_eq!(crate::face_frame::misframed(&f), Some(0));
        // no current version on this Mac: rather the whole photo than a wrong place
        std::fs::remove_file(lib.join(format!("resources/derivatives/A/{uuid}_1_105_c.jpeg"))).unwrap();
        std::fs::remove_file(thumb_path(&db.conn, 9_100_003).unwrap()).unwrap();
        assert_eq!(ensure_thumb(&db, 9_100_003).unwrap(), ThumbSource::Whole);
        let t = image::open(thumb_path(&db.conn, 9_100_003).unwrap()).unwrap().to_rgb8();
        assert!(t.get_pixel(2, THUMB / 2).0.iter().all(|&c| c < 40), "dark edge left of the reduced photo");
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_003).unwrap());
        let _ = std::fs::remove_dir_all(lib);
    }

    /// The face search used to look at a photo rotated in Lightroom only as the file lies: once, its boxes without a
    /// decision go and the photo is searched again; a box decided in graineo is turned along. Runs only once.
    #[cfg(target_os = "macos")]
    #[test]
    fn repair_turns_own_boxes_of_photos_rotated_in_lightroom_only() {
        let dir = std::env::temp_dir().join(format!("graineo-face-repair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("r.jpg"), jpeg(&photo(400, 300, 300, 40, 60), None)).unwrap();
        let root = format!("{}/", dir.display());
        let db = one_face(&dir, &root, "", "r.jpg", (400, 300), Some(6), 1, (0.75, 40.0 / 300.0, 0.15, 0.2));
        // own finds in the frame of the file: one decided (lokal), one open suggestion
        db.conn
            .execute_batch(
                "INSERT INTO keyword(id, name, person) VALUES(10, 'Anna', 1);
                 UPDATE image SET faces_scanned=1;
                 INSERT INTO face(id, image, x, y, w, h, source, status, person, local, detected) VALUES(2, 1, 0.75, 0.1333, 0.15, 0.2, 'graineo', 'confirmed', 10, 1, 100);
                 INSERT INTO face(id, image, x, y, w, h, source, status, person, local, detected) VALUES(3, 1, 0.1, 0.1, 0.1, 0.1, 'graineo', 'suggested', 10, 0, 100);",
            )
            .unwrap();
        let r = crate::face_frame::repair(&db).unwrap().unwrap();
        assert_eq!((r.images, r.turned, r.removed), (1, 1, 1));
        let (x, y, b, h): (f64, f64, f64, f64) = db.conn.query_row("SELECT x, y, w, h FROM face WHERE id=2", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        // turned: the red block lies at x 200..260 of 300, y 300..360 of 400 in the upright photo
        assert!((x - (300.0 - 100.0) / 300.0).abs() < 1e-3 && (y - 0.75).abs() < 1e-3 && (b - 0.2).abs() < 1e-3 && (h - 0.15).abs() < 1e-3, "{x} {y} {b} {h}");
        assert_eq!(db.conn.query_row("SELECT count(*) FROM face WHERE id=3", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.conn.query_row("SELECT faces_scanned FROM image WHERE id=1", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // Lightroom's box is untouched; a second run changes nothing
        assert_eq!(db.conn.query_row("SELECT x FROM face WHERE id=1", [], |r| r.get::<_, f64>(0)).unwrap(), 0.75);
        assert_eq!(crate::face_frame::repair(&db).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A RAW cropped in Lightroom whose own faces were found while an exported JPEG represented it (before 08.11.2026):
    /// its boxes lie in the cropped frame. Boxes without a decision go and the photo is searched again; a decided box
    /// stays. The same photo without a crop is left alone.
    #[cfg(target_os = "macos")]
    #[test]
    fn repair_searches_again_photos_cropped_in_lightroom() {
        for cropped in [true, false] {
            let dir = std::env::temp_dir().join(format!("graineo-face-crop-{}-{cropped}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            photo(300, 200, 10, 10, 20).save(dir.join("c.png")).unwrap();
            let root = format!("{}/", dir.display());
            let db = one_face(&dir, &root, "", "c.png", (300, 200), Some(1), 1, (0.4, 0.3, 0.1, 0.1));
            let develop = if cropped { "s = { CropBottom = 0.99,\nCropLeft = 0.167161,\nCropRight = 0.93,\nCropTop = 0.22713,\nExposure2012 = 0 }" } else { "s = { CropBottom = 1,\nCropLeft = 0,\nCropRight = 1,\nCropTop = 0 }" };
            db.conn.execute("INSERT INTO develop_state(file, copy, lr_develop) VALUES(1, '', ?1)", [develop]).unwrap();
            db.conn
                .execute_batch(
                    "INSERT INTO keyword(id, name, person) VALUES(10, 'Anna', 1);
                     UPDATE image SET faces_scanned=1;
                     INSERT INTO face(id, image, x, y, w, h, source, status, person, local, detected) VALUES(2, 1, 0.3, 0.1, 0.05, 0.08, 'graineo', 'suggested', 10, 0, 1793444820);
                     INSERT INTO face(id, image, x, y, w, h, source, status, person, local, detected) VALUES(3, 1, 0.5, 0.1, 0.05, 0.08, 'graineo', 'confirmed', 10, 1, 1793444820);",
                )
                .unwrap();
            let r = crate::face_frame::repair(&db).unwrap().unwrap();
            let left: i64 = db.conn.query_row("SELECT count(*) FROM face WHERE source='graineo'", [], |r| r.get(0)).unwrap();
            let state: i64 = db.conn.query_row("SELECT faces_scanned FROM image WHERE id=1", [], |r| r.get(0)).unwrap();
            if cropped {
                assert_eq!((r.images, r.removed, r.kept, left, state), (1, 1, 1, 1, 0));
            } else {
                assert_eq!((r.images, left, state), (0, 2, 1));
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// ⌘Z: a confirmed suggestion goes back to being a suggestion, the keyword it added to the photo goes away again.
    #[test]
    fn snapshot_and_restore_undo_an_answer() {
        let (db, dir) = library();
        let before = snapshot(&db, &[2], None).unwrap();
        crate::people::answer_suggestion(&db, 2, true).unwrap();
        assert_eq!(crate::sync::keyword_paths(&db, 2).unwrap(), vec!["Anna".to_string()]);
        restore(&db, &before).unwrap();
        let ix = load(&db.conn).unwrap();
        assert_eq!(ix.face(2).unwrap().status, Status::SuggestSure);
        assert!(crate::sync::keyword_paths(&db, 2).unwrap().is_empty());
        // rejecting leaves an excluded name; undo removes it again
        let before = snapshot(&db, &[2], None).unwrap();
        crate::people::answer_suggestion(&db, 2, false).unwrap();
        restore(&db, &before).unwrap();
        let never: i64 = db.conn.query_row("SELECT count(*) FROM face_never WHERE face=2", [], |r| r.get(0)).unwrap();
        assert_eq!(never, 0);
        assert_eq!(reload_faces(&db.conn, &[2])[0].1.as_ref().unwrap().status, Status::SuggestSure);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The Lightroom update writes only the faces that change: with 55 000 faces, rewriting all of them took many
    /// minutes at background priority (0.105.0) and the one-time steps after it never came.
    #[test]
    fn lightroom_update_writes_only_changed_faces() {
        let (db, dir) = library();
        db.conn.execute_batch("UPDATE image SET lr_id=100 WHERE id=1; UPDATE keyword SET lr_id=500 WHERE id=10; UPDATE face SET lr_id=7 WHERE id=1;").unwrap();
        let images: HashMap<i64, i64> = HashMap::from([(100, 1)]);
        let keywords: HashMap<i64, i64> = HashMap::from([(500, 10)]);
        let mut rows: Vec<crate::people::LrFace> = vec![(7, 100, [Some(0.1), Some(0.1), Some(0.3), Some(0.3)], Some(500), true, false)];
        let written = |rows: &[crate::people::LrFace]| {
            let before = db.conn.total_changes();
            let tx = db.conn.unchecked_transaction().unwrap();
            crate::people::import_lightroom(&tx, rows, &images, &keywords).unwrap();
            tx.commit().unwrap();
            db.conn.total_changes() - before
        };
        // the first run only remembers the catalog's box (the base of "own changes stay")
        assert_eq!(written(&rows), 1, "first run: only the remembered state");
        assert_eq!(written(&rows), 0, "nothing changed: nothing written");
        // turned in Lightroom: the box is written (and remembered), the name stays
        rows[0].2 = [Some(0.6), Some(0.5), Some(0.8), Some(0.7)];
        assert_eq!(written(&rows), 2);
        let (x, p): (f64, i64) = db.conn.query_row("SELECT x, person FROM face WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!((x - 0.6).abs() < 1e-9 && p == 10);
        // the name taken back in Lightroom: only the name is written
        rows[0].4 = false;
        assert_eq!(written(&rows), 1);
        let state: String = db.conn.query_row("SELECT status FROM face WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(state, "unknown");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The one-time steps after an update are done once and then stay done (no catalog: the Lightroom step is not open).
    #[test]
    fn one_time_steps_run_once() {
        let (db, dir) = library();
        assert!(crate::people::once_pending(&db));
        let cancel = std::sync::atomic::AtomicBool::new(false);
        crate::people::once_after_update(&db, &cancel, &|_| {});
        assert!(!crate::people::once_pending(&db));
        assert_eq!(db.info("face_frames").as_deref(), Some(crate::face_frame::REVISION));
        assert!(crate::people::once_after_update(&db, &cancel, &|_| {}).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The face image belongs to photo and box, not only to the face number. Before, a box moved by a
    /// sync (Lightroom update with turned boxes, Apple Photos) kept the image of the old place, and a new face that got
    /// the number of a removed one (SQLite gives the highest numbers again, e.g. when a photo is searched again)
    /// showed the removed face's image – a wall or a screenshot's text under a confident suggestion.
    #[test]
    fn face_image_follows_box_and_photo_not_only_the_number() {
        let dir = std::env::temp_dir().join(format!("graineo-face-reuse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = photo(300, 200, 10, 10, 40);
        for y in 100..140 {
            for x in 200..240 {
                img.put_pixel(x, y, image::Rgb([20, 20, 230]));
            }
        }
        img.save(dir.join("a.png")).unwrap();
        image::RgbImage::from_pixel(300, 200, image::Rgb([128, 128, 128])).save(dir.join("b.png")).unwrap();
        let root = format!("{}/", dir.display());
        let id = 9_100_008;
        let db = one_face(&dir, &root, "", "a.png", (300, 200), None, id, (10.0 / 300.0, 10.0 / 200.0, 40.0 / 300.0, 40.0 / 200.0));
        assert_eq!(ensure_thumb(&db, id).unwrap(), ThumbSource::Original);
        let first = thumb_path(&db.conn, id).unwrap();
        assert!(is_red(&thumb_center(&db, id)));
        assert_eq!(ensure_thumb(&db, id).unwrap(), ThumbSource::Present);
        // the box moves (sync): a new image of the new place
        db.conn.execute("UPDATE face SET x=?2, y=?3 WHERE id=?1", params![id, 200.0 / 300.0, 100.0 / 200.0]).unwrap();
        assert_ne!(ensure_thumb(&db, id).unwrap(), ThumbSource::Present);
        let moved = thumb_path(&db.conn, id).unwrap();
        assert!(thumb_center(&db, id).0[2] > 180, "{:?}", thumb_center(&db, id));
        // the face goes, a face on another photo gets its number: not the old image
        db.conn.execute("DELETE FROM face WHERE id=?1", [id]).unwrap();
        db.conn.execute("INSERT INTO file(id, folder, name, format, width, height, size, mtime) VALUES(2, 1, 'b.png', 'PNG', 300, 200, 1, 1)", []).unwrap();
        db.conn.execute("INSERT INTO image(id, file) VALUES(2, 2)", []).unwrap();
        db.conn.execute("INSERT INTO face(id, image, x, y, w, h, source, status) VALUES(?1, 2, 0.6, 0.5, 0.1333, 0.2, 'graineo', 'unknown')", [id]).unwrap();
        assert_ne!(ensure_thumb(&db, id).unwrap(), ThumbSource::Present);
        let c = thumb_center(&db, id);
        assert!(!is_red(&c) && c.0[2] < 180, "{c:?}");
        // tidying removes the images of the earlier places, the current one stays (in a folder of its own: other
        // tests use the shared one at the same time)
        let own = dir.join("faces");
        std::fs::create_dir_all(&own).unwrap();
        let now = thumb_path(&db.conn, id).unwrap();
        for p in [&first, &moved, &now] {
            std::fs::copy(p, own.join(p.file_name().unwrap())).unwrap();
            let _ = std::fs::remove_file(p);
        }
        assert_eq!(tidy_folder(&own, &db.conn, &std::sync::atomic::AtomicBool::new(false)), 2);
        assert!(own.join(now.file_name().unwrap()).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A box outside the photo gets the whole photo as face image, not a crop of the edge.
    #[test]
    fn face_image_of_box_outside_the_photo_is_the_whole_photo() {
        let dir = std::env::temp_dir().join(format!("graineo-face-outside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        photo(300, 200, 10, 10, 20).save(dir.join("o.png")).unwrap();
        let root = format!("{}/", dir.display());
        let db = one_face(&dir, &root, "", "o.png", (300, 200), None, 9_100_007, (0.95, 0.3, 0.2, 0.1));
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_007).unwrap());
        assert_eq!(ensure_thumb(&db, 9_100_007).unwrap(), ThumbSource::Whole);
        let (img, fits) = photo_for_boxes(&db, 1, 512).unwrap();
        assert!(fits && img.width() == 300 && img.height() == 200);
        let _ = std::fs::remove_file(thumb_path(&db.conn, 9_100_007).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
