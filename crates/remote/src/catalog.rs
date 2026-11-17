//! Reading the library for the browser: sources (folders, collections), photo lists, previews.
//!
//! Every server thread has its own read-only connection (`Db::read_only`, `query_only`); the
//! rows of the library are kept as a snapshot for a few seconds. Paths never come from a
//! request: the browser names photos only by their image id, and files are found through the
//! library (folder path + file name) or in the preview cache.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use library::db::{Db, Row};
use library::preview;
use serde_json::{json, Value};

thread_local! {
    static CONN: RefCell<Option<Db>> = const { RefCell::new(None) };
}

/// Runs `f` with this thread's read-only connection to the library.
pub fn with_db<T>(f: impl FnOnce(&Db) -> Result<T, String>) -> Result<T, String> {
    CONN.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(Db::read_only(&library::db_path()).map_err(|e| format!("library: {e}"))?);
        }
        f(c.as_ref().unwrap())
    })
}

pub struct Snapshot {
    pub rows: Vec<Row>,
    pub by_image: HashMap<i64, usize>,
    pub folder_paths: HashMap<i64, String>,
    made: Instant,
    /// ratings changed through the server since the rows were read (image → stars, flag, label): a change never
    /// copies the rows of the whole library
    ratings: Mutex<HashMap<i64, (u8, i8, u8)>>,
}

impl Snapshot {
    /// Stars, flag and label of a row, with the changes made through the server.
    pub fn rating(&self, z: &Row) -> (u8, i8, u8) {
        self.ratings.lock().unwrap_or_else(|e| e.into_inner()).get(&z.image).copied().unwrap_or((z.stars, z.flag, z.label))
    }

    pub fn row(&self, image: i64) -> Option<&Row> {
        self.by_image.get(&image).map(|&i| &self.rows[i])
    }

    pub fn path_of(&self, z: &Row) -> PathBuf {
        PathBuf::from(format!("{}{}", self.folder_paths.get(&z.folder).map(String::as_str).unwrap_or(""), z.name))
    }

    /// Preview cache key as the grid of the app computes it.
    pub fn key_of(&self, z: &Row) -> u64 {
        let p = format!("{}{}", self.folder_paths.get(&z.folder).map(String::as_str).unwrap_or(""), z.name);
        preview::cache_key_with_orientation(preview::cache_key(&p, z.size.unwrap_or(0), z.mtime.unwrap_or(0)), z.orientation)
    }
}

static SNAPSHOT: Mutex<Option<Arc<Snapshot>>> = Mutex::new(None);
const MAX_AGE: Duration = Duration::from_secs(20);

/// Current rows of the library (re-read after 20 s or after [`invalidate`]).
pub fn snapshot() -> Result<Arc<Snapshot>, String> {
    if let Some(s) = SNAPSHOT.lock().unwrap().as_ref() {
        if s.made.elapsed() < MAX_AGE {
            return Ok(s.clone());
        }
    }
    let (rows, folder_paths) = with_db(|db| {
        let rows = db.rows().map_err(|e| e.to_string())?;
        let fp = db.folder_paths().map_err(|e| e.to_string())?;
        Ok((rows, fp))
    })?;
    let by_image = rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
    let s = Arc::new(Snapshot { rows, by_image, folder_paths, made: Instant::now(), ratings: Mutex::new(HashMap::new()) });
    *SNAPSHOT.lock().unwrap() = Some(s.clone());
    Ok(s)
}

pub fn invalidate() {
    *SNAPSHOT.lock().unwrap() = None;
}

/// Patch the rating of one row in the snapshot (after a change through the server).
pub fn patch_rating(image: i64, m: (u8, i8, u8)) {
    let s = SNAPSHOT.lock().unwrap().clone();
    if let Some(s) = s {
        if s.by_image.contains_key(&image) {
            s.ratings.lock().unwrap_or_else(|e| e.into_inner()).insert(image, m);
        }
    }
}

fn file_name_of(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

/// Folders (per root), collections and keywords with counts.
pub fn sources() -> Result<Value, String> {
    let snap = snapshot()?;
    let mut per_folder: HashMap<i64, usize> = HashMap::new();
    for z in snap.rows.iter().filter(|z| z.jpeg_for.is_none()) {
        *per_folder.entry(z.folder).or_default() += 1;
    }
    let total = snap.rows.iter().filter(|z| z.jpeg_for.is_none()).count();
    with_db(|db| {
        let roots = db.roots().map_err(|e| e.to_string())?;
        let folders = db.folder().map_err(|e| e.to_string())?;
        let mut out_roots = Vec::new();
        for r in &roots {
            let mut fs: Vec<&library::db::Folder> = folders.iter().filter(|f| f.root == r.id).collect();
            fs.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
            let list: Vec<Value> = fs
                .iter()
                .filter(|f| per_folder.get(&f.id).copied().unwrap_or(0) > 0 || folders.iter().any(|g| g.root == f.root && g.path.len() > f.path.len() && g.path.starts_with(&f.path)))
                .map(|f| {
                    let depth = f.path.trim_end_matches('/').matches('/').count() + usize::from(!f.path.trim_end_matches('/').is_empty());
                    json!({ "id": f.id, "name": if f.path.trim_end_matches('/').is_empty() { r.name.clone() } else { file_name_of(&f.path) }, "depth": depth, "count": per_folder.get(&f.id).copied().unwrap_or(0) })
                })
                .collect();
            out_roots.push(json!({ "id": r.id, "name": if r.name.is_empty() { file_name_of(&r.path) } else { r.name.clone() }, "folders": list }));
        }
        let cols = db.collections().map_err(|e| e.to_string())?;
        let collections: Vec<Value> = cols
            .iter()
            .filter(|c| !c.duplicates)
            .map(|c| json!({ "id": c.id, "name": c.name, "parent": c.parent, "count": c.count, "smart": c.search.as_deref().is_some_and(|s| !s.trim().is_empty()), "quick": c.kind == 3, "set": c.kind == 2 }))
            .collect();
        Ok(json!({ "total": total, "roots": out_roots, "collections": collections }))
    })
}

/// Which photos the browser asks for.
#[derive(Default, Debug)]
pub struct Query {
    pub source: String,
    pub text: String,
    pub min_stars: u8,
    /// "", "picked", "rejected", "unrejected"
    pub flag: String,
    /// 0 = any, 1..5 = this colour label
    pub label: u8,
    pub sort: String,
    pub offset: usize,
    pub limit: usize,
}

fn source_filter(snap: &Snapshot, source: &str) -> Result<Option<Vec<bool>>, String> {
    let (kind, id) = match source.split_once(':') {
        Some((k, v)) => (k, v.parse::<i64>().map_err(|_| "bad source".to_string())?),
        None if source.is_empty() || source == "all" => return Ok(None),
        None => return Err("bad source".into()),
    };
    let mask_of = |ids: Vec<i64>| {
        let set: std::collections::HashSet<i64> = ids.into_iter().collect();
        snap.rows.iter().map(|z| set.contains(&z.image)).collect::<Vec<bool>>()
    };
    match kind {
        "folder" => Ok(Some(snap.rows.iter().map(|z| z.folder == id).collect())),
        "collection" => with_db(|db| {
            let cols = db.collections().map_err(|e| e.to_string())?;
            let c = cols.iter().find(|c| c.id == id).ok_or("unknown collection")?;
            if let Some(q) = c.search.as_deref().filter(|s| !s.trim().is_empty()) {
                let ctx = library::search::Context { db, rows: &snap.rows, folder_paths: &snap.folder_paths };
                return Ok(Some(library::search::evaluate(&ctx, &library::search::parse(q))));
            }
            Ok(Some(mask_of(db.images_of_collection(id).map_err(|e| e.to_string())?)))
        }),
        "keyword" => with_db(|db| Ok(Some(mask_of(db.images_with_keyword(id).map_err(|e| e.to_string())?)))),
        _ => Err("bad source".into()),
    }
}

pub fn item(z: &Row, (stars, flag, label): (u8, i8, u8)) -> Value {
    json!({
        "id": z.image,
        "name": z.name,
        "copy": z.copy,
        "stars": stars,
        "flag": flag,
        "label": label,
        "w": z.width,
        "h": z.height,
        "o": z.orientation,
        "date": z.capture,
        "video": z.format == "VIDEO",
    })
}

pub fn images(q: &Query) -> Result<Value, String> {
    let snap = snapshot()?;
    let mut keep = source_filter(&snap, &q.source)?.unwrap_or_else(|| vec![true; snap.rows.len()]);
    let text = q.text.trim();
    if !text.is_empty() {
        let hits = with_db(|db| {
            let ctx = library::search::Context { db, rows: &snap.rows, folder_paths: &snap.folder_paths };
            Ok(library::search::evaluate(&ctx, &library::search::parse(text)))
        })?;
        for (k, h) in keep.iter_mut().zip(hits) {
            *k &= h;
        }
    }
    let changed = snap.ratings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let rating = |z: &Row| changed.get(&z.image).copied().unwrap_or((z.stars, z.flag, z.label));
    let mut list: Vec<(&Row, (u8, i8, u8))> = snap
        .rows
        .iter()
        .zip(&keep)
        // camera JPEGs of RAW+JPEG pairs are not shown on their own (as in the stacked grid of the app)
        .filter(|(z, k)| **k && z.jpeg_for.is_none())
        .map(|(z, _)| (z, rating(z)))
        .filter(|(_, (stars, flag, label))| {
            *stars >= q.min_stars
                && (q.label == 0 || *label == q.label)
                && match q.flag.as_str() {
                    "picked" => *flag == 1,
                    "rejected" => *flag == -1,
                    "unrejected" => *flag != -1,
                    _ => true,
                }
        })
        .collect();
    match q.sort.as_str() {
        "date_asc" => list.sort_by(|(a, _), (b, _)| a.capture.cmp(&b.capture).then(a.name.cmp(&b.name))),
        "name" => list.sort_by(|(a, _), (b, _)| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.copy.cmp(&b.copy))),
        "stars" => list.sort_by(|(a, ra), (b, rb)| rb.0.cmp(&ra.0).then(b.capture.cmp(&a.capture))),
        _ => list.sort_by(|(a, _), (b, _)| b.capture.cmp(&a.capture).then(a.name.cmp(&b.name))),
    }
    let total = list.len();
    let limit = q.limit.clamp(1, 500);
    let items: Vec<Value> = list.iter().skip(q.offset).take(limit).map(|(z, r)| item(z, *r)).collect();
    Ok(json!({ "total": total, "offset": q.offset, "items": items }))
}

/// Details of one photo.
pub fn details(image: i64) -> Result<Option<Value>, String> {
    let snap = snapshot()?;
    let Some(z) = snap.row(image) else { return Ok(None) };
    let (keywords, collections) = with_db(|db| Ok((db.keywords_of(image).unwrap_or_default(), db.collections_of(image).unwrap_or_default())))?;
    let meta = library::store::meta(z.file, z.copy.as_deref()).unwrap_or_else(|| snap.rating(z));
    let mut v = item(z, meta);
    v["stars"] = json!(meta.0);
    v["flag"] = json!(meta.1);
    v["label"] = json!(meta.2);
    v["camera"] = json!(z.camera);
    v["lens"] = json!(z.lens);
    v["iso"] = json!(z.iso);
    v["aperture"] = json!(z.aperture);
    v["focal"] = json!(z.focal);
    v["time"] = json!(z.time);
    v["place"] = json!(z.place);
    v["format"] = json!(z.format);
    v["keywords"] = json!(keywords);
    v["collections"] = json!(collections);
    v["reachable"] = json!(z.status != 4 && z.status != 2);
    Ok(Some(v))
}

/// A small store of JPEGs the server made itself (missing previews), at most `MAX_LRU` bytes.
struct Lru {
    map: HashMap<(i64, u32), Arc<Vec<u8>>>,
    order: VecDeque<(i64, u32)>,
    bytes: usize,
}

const MAX_LRU: usize = 96 * 1024 * 1024;
static MADE: Mutex<Option<Lru>> = Mutex::new(None);

fn lru_get(k: (i64, u32)) -> Option<Arc<Vec<u8>>> {
    MADE.lock().unwrap().as_ref().and_then(|l| l.map.get(&k).cloned())
}

fn lru_put(k: (i64, u32), v: Arc<Vec<u8>>) {
    let mut g = MADE.lock().unwrap();
    let l = g.get_or_insert_with(|| Lru { map: HashMap::new(), order: VecDeque::new(), bytes: 0 });
    l.bytes += v.len();
    if let Some(old) = l.map.insert(k, v) {
        l.bytes -= old.len();
    } else {
        l.order.push_back(k);
    }
    while l.bytes > MAX_LRU {
        let Some(o) = l.order.pop_front() else { break };
        if let Some(old) = l.map.remove(&o) {
            l.bytes -= old.len();
        }
    }
}

/// Forget what the server made for this photo (after a new development).
pub fn forget(image: i64) {
    let mut g = MADE.lock().unwrap();
    if let Some(l) = g.as_mut() {
        let keys: Vec<(i64, u32)> = l.map.keys().filter(|k| k.0 == image).copied().collect();
        for k in keys {
            if let Some(old) = l.map.remove(&k) {
                l.bytes -= old.len();
            }
            l.order.retain(|o| *o != k);
        }
    }
}

pub fn encode_jpeg(img: &image::RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(img).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Why a preview could not be delivered.
pub enum PreviewError {
    NotFound,
    /// the server is making other previews right now; try again shortly
    Busy,
    Failed(String),
}

/// Size class of a preview.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Thumb,
    Large,
}

fn existing(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

/// File of a finished preview: developed previews (own edits) first, then the preview cache.
fn cached_file(snap: &Snapshot, z: &Row, size: Size) -> Option<PathBuf> {
    let cache = library::cache_folder();
    let key = snap.key_of(z);
    let small: Vec<u32> = {
        let mut v = vec![preview::small()];
        for l in preview::SMALL_CHOICES {
            if !v.contains(&l) {
                v.push(l);
            }
        }
        v
    };
    let large: Vec<u32> = {
        let mut v = vec![preview::large()];
        for l in preview::LARGE_CHOICES {
            if !v.contains(&l) {
                v.push(l);
            }
        }
        v
    };
    let developed = |l: u32| -> Option<PathBuf> {
        if z.copy.is_some() {
            return None;
        }
        existing(preview::developed_folder().join(format!("{}-{l}.jpg", z.file)))
    };
    let cached = |l: u32| existing(preview::file_in(&cache, key, l));
    let app_developed = || existing(library::data_dir().join("previews").join("developed").join(format!("{}.jpg", z.image)));
    match size {
        Size::Thumb => small.iter().find_map(|&l| developed(l).or_else(|| cached(l))),
        Size::Large => large
            .iter()
            .find_map(|&l| developed(l).or_else(|| cached(l)))
            .or_else(app_developed)
            .or_else(|| small.iter().rev().find_map(|&l| developed(l).or_else(|| cached(l)))),
    }
}

/// JPEG of a photo in grid or full-screen size. Finished previews are delivered as they are;
/// a missing one is made from the original (read only) at low priority, at most `slots` at a time.
pub fn preview_jpeg(image: i64, size: Size, slots: &crate::limits::Slots) -> Result<Arc<Vec<u8>>, PreviewError> {
    let snap = snapshot().map_err(PreviewError::Failed)?;
    let Some(z) = snap.row(image) else { return Err(PreviewError::NotFound) };
    let level = if size == Size::Thumb { 512 } else { 2048 };
    if let Some(v) = lru_get((image, level)) {
        return Ok(v);
    }
    if let Some(f) = cached_file(&snap, z, size) {
        // the developed small level can be larger than wanted, but it is a finished JPEG: deliver as it is
        return std::fs::read(&f).map(Arc::new).map_err(|e| PreviewError::Failed(e.to_string()));
    }
    if z.format == "VIDEO" {
        return Err(PreviewError::NotFound);
    }
    let path = snap.path_of(z);
    if !path.is_file() {
        return Err(PreviewError::NotFound);
    }
    let Some(_slot) = slots.try_enter() else { return Err(PreviewError::Busy) };
    library::platform::background_priority();
    // as the preview generator of the app does it (orientation of the library, not only of the file)
    let (img, _) = preview::load(&path, &z.format, level).map_err(PreviewError::Failed)?;
    let img = preview::rotate(img, preview::extra_rotation(&path, &z.format, z.orientation));
    let jpeg = Arc::new(encode_jpeg(&preview::downscale(img, level), 85).map_err(PreviewError::Failed)?);
    lru_put((image, level), jpeg.clone());
    Ok(jpeg)
}
