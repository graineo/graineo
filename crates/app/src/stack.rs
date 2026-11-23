//! Batch "Re-render all previews": grid and preview show graineo's own development of JPEG, HEIC, TIFF and PNG
//! originals edited in Lightroom (with the Lightroom sliders) instead of the plain file, so that there is no jump when
//! editing; reference exports themselves are never edited again.
//!
//! Per photo: develop it with the Lightroom initial values (as when opening), store the small level and – only if the
//! file lies on another drive – the offline copy in the chosen level (full development) as developed grid previews
//! ([`library::preview::write_developed`]) and remove the old cache entry; the generator then takes the developed
//! preview. The files themselves stay untouched, nothing is ever written into an original. Photos you edited in
//! graineo yourself (settings in the library) are skipped. Version 3: retouching; version 4: calibration.
//! Runs in the background at low priority, can be cancelled and resumes (progress in `developed-grid/snapshot.json`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use library::preview;
use develop::Settings;

/// Development version for the batch: increase on an improvement, then the next run starts from the beginning.
pub const VERSION: u32 = 4;

#[derive(Clone, Default)]
pub struct Snapshot {
    pub running: bool,
    pub total: usize,
    pub done: usize,
    /// already finished before this run (from `snapshot.json`)
    pub before_done: usize,
    pub error: usize,
    pub skipped: usize,
    pub start: Option<Instant>,
    pub message: String,
}

static STATE: Mutex<Snapshot> = Mutex::new(Snapshot {
    running: false,
    total: 0,
    done: 0,
    before_done: 0,
    error: 0,
    skipped: 0,
    start: None,
    message: String::new(),
});
static CANCEL: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);

pub fn snapshot() -> Snapshot {
    STATE.lock().unwrap().clone()
}

pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// True once after a run (or partial run): the grid should reload its previews.
pub fn reload_needed() -> bool {
    RELOAD.swap(false, Ordering::SeqCst)
}

fn progress_file() -> PathBuf {
    preview::developed_folder().join(legacy_names::app::STACK_STATE_FILE.1)
}

fn load_done() -> HashSet<i64> {
    let file = progress_file();
    // a run of an earlier version left its progress under the old name (nothing is deleted)
    let text = std::fs::read_to_string(&file).or_else(|_| std::fs::read_to_string(preview::developed_folder().join(legacy_names::app::STACK_STATE_FILE.0)));
    let Ok(t) = text else { return HashSet::new() };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&t) else { return HashSet::new() };
    legacy_names::rename_keys(&mut v, legacy_names::app::STACK_STATE_KEYS);
    if v["version"].as_u64() != Some(VERSION as u64) {
        return HashSet::new();
    }
    v["done"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default()
}

fn save_done(f: &HashSet<i64>) {
    let mut ids: Vec<i64> = f.iter().copied().collect();
    ids.sort_unstable();
    let _ = std::fs::create_dir_all(preview::developed_folder());
    let _ = library::files::write_atomic(&progress_file(), serde_json::json!({"version": VERSION, "done": ids}).to_string().as_bytes());
}

/// A photo of the batch.
struct RerenderTask {
    file: i64,
    path: PathBuf,
    /// Grid key (to remove the old entry)
    key: u64,
}

fn tasks(db: &library::db::Db) -> Result<Vec<RerenderTask>, String> {
    let paths = db.folder_paths().map_err(|e| e.to_string())?;
    let mut v = Vec::new();
    // JPEG, HEIC, TIFF and PNG originals that were edited in Lightroom
    if library::settings::value("lightroomJpegEdits").as_bool() != Some(false) && library::settings::value("lightroomStartValues").as_bool() != Some(false) {
        let mut raw_stems: HashMap<String, HashSet<String>> = HashMap::new();
        for z in db.rows().map_err(|e| e.to_string())? {
            if z.copy.is_some() || z.status != 0 || !matches!(z.format.as_str(), "JPG" | "HEIC" | "TIFF" | "PNG") {
                continue;
            }
            let folder = paths.get(&z.folder).map(String::as_str).unwrap_or("");
            let p = format!("{folder}{}", z.name);
            if !lightroom_edited(Path::new(&p), &z.format, raw_stems.entry(folder.to_string()).or_insert_with(|| raw_stems_in(Path::new(folder)))) {
                continue;
            }
            let key = preview::cache_key_with_orientation(preview::cache_key(&p, z.size.unwrap_or(0), z.mtime.unwrap_or(0)), z.orientation);
            v.push(RerenderTask { file: z.file, path: PathBuf::from(&p), key });
        }
    }
    Ok(v)
}

/// Stems (lower case) of the RAW files in a folder (for the sidecar name of a JPEG next to a RAW).
fn raw_stems_in(dir: &Path) -> HashSet<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    library::format_from_extension(&n).filter(|f| library::is_raw(f))?;
                    Path::new(&n).file_stem().map(|s| s.to_string_lossy().to_lowercase())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Was the finished image (JPEG, HEIC, TIFF, PNG) edited in Lightroom? Read from its sidecar (`graineo:LightroomEdited`,
/// taken over from the catalog), never from the catalog itself.
fn lightroom_edited(p: &Path, format: &str, raw_stems: &HashSet<String>) -> bool {
    // photo of the library: from the library (the main store); otherwise from the sidecar next to it
    let lr = match library::store::locate(p) {
        Some(_) => library::store::lightroom_for(p, None),
        None => {
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            library::xmp::lightroom(&library::xmp::path(p, format, raw_stems.contains(&stem))).for_image(p)
        }
    };
    lr.edited && lr.develop.is_some()
}

fn downscale(img: &image::RgbImage, max: u32) -> image::RgbImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max {
        return img.clone();
    }
    let k = max as f32 / w.max(h) as f32;
    image::imageops::resize(img, ((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1), image::imageops::FilterType::Triangle)
}

/// Render and store one photo. `Ok(false)`: skipped (file unreachable or settings from graineo).
fn render_one(a: &RerenderTask, cache: &preview::Cache) -> Result<bool, String> {
    if !a.path.exists() {
        return Ok(false);
    }
    if library::store::develop_for(&a.path).is_some() {
        return Ok(false);
    }
    let mut doc = rawdev::RawDoc::open(&a.path, &|| true)?.ok_or("abgebrochen")?;
    // a merged photo that is not computed yet shows its reference frame: never stored as its preview
    if doc.stands_in() {
        return Ok(false);
    }
    rawdev::lightroom::apply_orientation(&mut doc);
    let s = match rawdev::lightroom::initial_settings(&doc) {
        Some(s) => s,
        // finished image without (applicable) Lightroom edits, e.g. a reference export: nothing to do
        None if doc.finished => return Ok(false),
        None => Settings::default(),
    };
    let img = develop::geometry::apply(doc.render_preview(&s), &s, doc.orientation);
    let rgb = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).ok_or("Bildgröße passt nicht")?;
    let (small_level, large_level) = (preview::small(), preview::large());
    let small = downscale(&rgb, small_level);
    // the offline copy (large level) only for photos whose file in the library (the key) lies on another drive, and
    // then in the chosen level from the full development (the render above has only ≈ 2.8 MP)
    let offline = !library::platform::on_startup_volume(&a.path);
    let ok = (!offline || preview::write_developed(a.file, large_level, &crate::large_preview::render_doc(&doc, &s, large_level)?)) && preview::write_developed(a.file, small_level, &small);
    if !ok {
        return Err("Vorschau konnte nicht geschrieben werden".into());
    }
    cache.remove_all(a.key);
    // the developed view for the single image (placeholder on open) exists separately; the grid suffices here
    Ok(true)
}

/// Start (one run at a time). `from_scratch`: discard the previous progress.
pub fn start(from_scratch: bool) {
    {
        let mut st = STATE.lock().unwrap();
        if st.running {
            return;
        }
        *st = Snapshot { running: true, start: Some(Instant::now()), message: crate::tr("Aufgaben werden ermittelt …").to_string(), ..Default::default() };
    }
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        lower_priority();
        let finish = |message: String| {
            let mut st = STATE.lock().unwrap();
            st.running = false;
            st.message = message;
            RELOAD.store(true, Ordering::SeqCst);
        };
        let db = match library::db::Db::standard() {
            Ok(d) => d,
            Err(e) => return finish(format!("Bibliothek: {e}")),
        };
        let all = match tasks(&db) {
            Ok(a) => a,
            Err(e) => return finish(e),
        };
        drop(db);
        let mut done = if from_scratch { HashSet::new() } else { load_done() };
        let mut pending: Vec<&RerenderTask> = all.iter().filter(|a| !done.contains(&a.file)).collect();
        // Test hook (only with GRAINEO_DATA_DIR): GRAINEO_STACK_N=<count> renders only that many photos
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
            if let Some(n) = crate::env::var("GRAINEO_STACK_N").ok().and_then(|v| v.parse::<usize>().ok()) {
                pending.truncate(n);
            }
        }
        {
            let mut st = STATE.lock().unwrap();
            st.total = all.len();
            st.before_done = all.len() - pending.len();
            st.done = st.before_done;
            st.message = String::new();
        }
        let cache = preview::Cache::standard();
        let next_index = std::sync::atomic::AtomicUsize::new(0);
        let done_mutex = Mutex::new(done);
        // three workers: reading (e.g. from the NAS) and developing overlap this way
        std::thread::scope(|sc| {
            for _ in 0..3 {
                sc.spawn(|| {
                    lower_priority();
                    loop {
                        if CANCEL.load(Ordering::SeqCst) {
                            break;
                        }
                        let i = next_index.fetch_add(1, Ordering::SeqCst);
                        let Some(a) = pending.get(i) else { break };
                        match render_one(a, &cache) {
                            Ok(true) => {
                                let mut f = done_mutex.lock().unwrap();
                                f.insert(a.file);
                                let n = {
                                    let mut st = STATE.lock().unwrap();
                                    st.done += 1;
                                    st.done
                                };
                                if n % 25 == 0 {
                                    save_done(&f);
                                    RELOAD.store(true, Ordering::SeqCst);
                                }
                            }
                            Ok(false) => STATE.lock().unwrap().skipped += 1,
                            Err(e) => {
                                crate::log(&format!("stapel: {} – {e}", a.path.display()));
                                STATE.lock().unwrap().error += 1;
                            }
                        }
                    }
                });
            }
        });
        let done = done_mutex.into_inner().unwrap();
        save_done(&done);
        let st = snapshot();
        let t = if CANCEL.load(Ordering::SeqCst) {
            crate::trf!("Abgebrochen bei {a} von {b}.", a = st.done, b = st.total)
        } else {
            crate::trf!("Fertig: {a} von {b} Vorschauen neu gerechnet, {c} übersprungen, {d} Fehler.", a = st.done, b = st.total, c = st.skipped, d = st.error)
        };
        finish(t);
    });
}

#[cfg(target_os = "macos")]
fn lower_priority() {
    // QOS_CLASS_UTILITY: the work runs on the efficiency cores, the UI stays smooth
    extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, rel: i32) -> i32;
    }
    unsafe {
        pthread_set_qos_class_self_np(0x11, 0);
    }
}
#[cfg(not(target_os = "macos"))]
fn lower_priority() {}

/// Test hook: with `GRAINEO_STACK_N=<count>` and `GRAINEO_DATA_DIR` the batch starts once by itself (from the beginning).
pub fn test_helper() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if crate::env::var_os("GRAINEO_STACK_N").is_some() && crate::env::var_os("GRAINEO_DATA_DIR").is_some() && !STARTED.swap(true, Ordering::SeqCst) {
        start(true);
    }
}
