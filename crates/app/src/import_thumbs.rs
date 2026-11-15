//! Thumbnails of the import window: a few helper threads with low priority, only the visible cells are requested.
//! The queue is a stack: the most recently requested (i.e. currently visible) cells come first; when scrolling fast the
//! oldest requests drop out instead of being processed in order.
//!
//! Each thumbnail is read once in the size of the library's small preview level (from the embedded JPEG, never a RAW
//! development) and kept as a JPEG in a staging folder of the cache; after the import the library takes it from there
//! instead of reading the card a second time ([`staged`]). The window shows a smaller texture of it.
//!
//! The large view (space bar) has its own thread: the embedded preview at about 1 600 px for the shown photo and its
//! neighbours.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, TextureHandle};

/// at most this many textures are kept (oldest go first)
const MAX_TEXTURES: usize = 500;
/// at most this many jobs wait (about two screens full)
const MAX_PENDING: usize = 64;
/// edge of the textures shown in the cells
const TEXTURE_EDGE: u32 = 320;
/// edge of the large view
pub(super) const BIG_EDGE: u32 = 1600;

type Done = Arc<Mutex<Vec<(PathBuf, Option<egui::ColorImage>)>>>;
type Stack = Arc<(Mutex<(Vec<(PathBuf, String)>, bool)>, std::sync::Condvar)>;

pub(super) struct Thumbs {
    pub tex: HashMap<PathBuf, TextureHandle>,
    order: VecDeque<PathBuf>,
    in_flight: HashSet<PathBuf>,
    finished: Done,
    stack: Stack,
    /// large view: wanted paths (shown first), finished images, textures (at most 3)
    big_stack: Stack,
    big_finished: Done,
    pub big: HashMap<PathBuf, TextureHandle>,
    big_order: VecDeque<PathBuf>,
    big_in_flight: HashSet<PathBuf>,
}

/// Folder of the staged thumbnails (in the cache; emptied when a new import window opens).
pub(super) fn staging_folder() -> PathBuf {
    library::cache_folder().join("import-staging")
}

/// File of the staged thumbnail of a source file (small preview level, upright).
pub(super) fn staged(source: &Path) -> Option<PathBuf> {
    let p = staging_folder().join(format!("{:016x}-{}.jpg", key(source), library::preview::small()));
    p.is_file().then_some(p)
}

fn key(source: &Path) -> u64 {
    // stable within a run of the app (the staged files are used right after the import)
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let m = std::fs::metadata(source).ok();
    m.as_ref().map(|m| m.len()).hash(&mut h);
    m.and_then(|m| m.modified().ok()).hash(&mut h);
    h.finish()
}

/// Empty the staging folder (own files of the app; only when no import is running).
pub(super) fn clear_staging() {
    if let Ok(rd) = std::fs::read_dir(staging_folder()) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "jpg") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn to_color_image(img: &image::RgbImage) -> egui::ColorImage {
    egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], img.as_raw())
}

fn worker(name: String, stack: Stack, finished: Done, ctx: egui::Context, job: fn(&Path, &str) -> Option<egui::ColorImage>) {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            library::preview::low_priority_thread();
            loop {
                let (path, format) = {
                    let (m, cv) = &*stack;
                    let mut g = m.lock().unwrap();
                    loop {
                        if g.1 {
                            return;
                        }
                        if let Some(j) = g.0.pop() {
                            break j;
                        }
                        g = cv.wait(g).unwrap();
                    }
                };
                let image = std::panic::catch_unwind(|| job(&path, &format)).ok().flatten();
                finished.lock().unwrap().push((path, image));
                ctx.request_repaint();
            }
        })
        .ok();
}

/// Thumbnail: the small library level, staged as JPEG for the library, shown smaller.
fn thumb_job(path: &Path, format: &str) -> Option<egui::ColorImage> {
    let level = library::preview::small();
    let img = if library::import_source::is_camera(path) {
        // the camera delivers its thumbnail itself
        let (src, rel) = library::import_source::of_path(path)?;
        src.thumbnail(&rel, level)?
    } else {
        library::preview::load(path, format, level).ok()?.0
    };
    if level <= 1024 {
        let dir = staging_folder();
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join(format!("{:016x}-{level}.jpg", key(path)));
        if !target.exists() {
            let part = target.with_extension("teil");
            if let Ok(f) = std::fs::File::create(&part) {
                let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), 88);
                if enc.encode_image(&img).is_ok() {
                    let _ = std::fs::rename(&part, &target);
                } else {
                    let _ = std::fs::remove_file(&part);
                }
            }
        }
    }
    Some(to_color_image(&library::preview::downscale(img, TEXTURE_EDGE)))
}

fn big_job(path: &Path, format: &str) -> Option<egui::ColorImage> {
    if library::import_source::is_camera(path) {
        let (src, rel) = library::import_source::of_path(path)?;
        return src.thumbnail(&rel, BIG_EDGE).map(|img| to_color_image(&img));
    }
    library::preview::load(path, format, BIG_EDGE).ok().map(|(img, _)| to_color_image(&img))
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        let stack: Stack = Arc::default();
        let finished: Done = Arc::default();
        // four helpers in all (quiet): three for the cells, one for the large view
        for n in 0..3 {
            worker(format!("import-vorschau-{n}"), stack.clone(), finished.clone(), ctx.clone(), thumb_job);
        }
        let big_stack: Stack = Arc::default();
        let big_finished: Done = Arc::default();
        worker("import-gross".into(), big_stack.clone(), big_finished.clone(), ctx.clone(), big_job);
        Thumbs { tex: HashMap::new(), order: VecDeque::new(), in_flight: HashSet::new(), finished, stack, big_stack, big_finished, big: HashMap::new(), big_order: VecDeque::new(), big_in_flight: HashSet::new() }
    }

    /// Take over finished thumbnails (create textures, discard old ones).
    pub fn poll(&mut self, ctx: &egui::Context) {
        let new: Vec<_> = std::mem::take(&mut *self.finished.lock().unwrap());
        for (path, image) in new {
            self.in_flight.remove(&path);
            if let Some(b) = image {
                let t = ctx.load_texture(format!("import:{}", path.display()), b, Default::default());
                self.tex.insert(path.clone(), t);
                self.order.push_back(path);
            }
        }
        while self.tex.len() > MAX_TEXTURES {
            match self.order.pop_front() {
                Some(p) => {
                    self.tex.remove(&p);
                }
                None => break,
            }
        }
        let new: Vec<_> = std::mem::take(&mut *self.big_finished.lock().unwrap());
        for (path, image) in new {
            self.big_in_flight.remove(&path);
            if let Some(b) = image {
                let t = ctx.load_texture(format!("import-gross:{}", path.display()), b, Default::default());
                self.big.insert(path.clone(), t);
                self.big_order.push_back(path);
            }
        }
        while self.big.len() > 3 {
            match self.big_order.pop_front() {
                Some(p) => {
                    self.big.remove(&p);
                }
                None => break,
            }
        }
    }

    pub fn request(&mut self, path: &Path, format: &str) {
        if self.tex.contains_key(path) || self.in_flight.contains(path) {
            return;
        }
        let (m, cv) = &*self.stack;
        let mut g = m.lock().unwrap();
        g.0.push((path.to_path_buf(), format.to_string()));
        self.in_flight.insert(path.to_path_buf());
        // discard the oldest requests (scrolled away long ago); they are requested again once visible again
        while g.0.len() > MAX_PENDING {
            let (old, _) = g.0.remove(0);
            self.in_flight.remove(&old);
        }
        cv.notify_one();
    }

    /// Large view: `wanted` first (last in the list = first done), the others ahead of time.
    pub fn request_big(&mut self, wanted: &[(PathBuf, String)]) {
        let (m, cv) = &*self.big_stack;
        let mut g = m.lock().unwrap();
        // requests no longer wanted (paged on) drop out; they are no longer under way
        let dropped: Vec<PathBuf> = g.0.iter().filter(|(p, _)| !wanted.iter().any(|(w, _)| w == p)).map(|(p, _)| p.clone()).collect();
        g.0.retain(|(p, _)| wanted.iter().any(|(w, _)| w == p));
        for d in dropped {
            self.big_in_flight.remove(&d);
        }
        for (p, f) in wanted.iter().rev() {
            if self.big.contains_key(p) || self.big_in_flight.contains(p) {
                continue;
            }
            g.0.push((p.clone(), f.clone()));
            self.big_in_flight.insert(p.clone());
        }
        cv.notify_one();
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        for s in [&self.stack, &self.big_stack] {
            let (m, cv) = &**s;
            m.lock().unwrap().1 = true;
            cv.notify_all();
        }
    }
}

