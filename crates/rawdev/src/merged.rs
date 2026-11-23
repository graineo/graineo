//! Merged photos (HDR from an exposure bracket) as development documents.
//!
//! A merged photo is a library file without a file on disk (`library::merge`). Its pixels are the result of
//! [`hdr`] – linear, white-balanced camera RGB of the reference frame – kept in the app cache (`<cache>/merge/`,
//! half floats). Opening such a photo opens the reference RAW for everything that belongs to the camera (colour,
//! looks, lens profile, EXIF) and replaces its pixels by the merge. When the cache is gone and the sources are
//! reachable, the merge is computed again (deterministic). The reference frame has to be reachable to open the photo
//! at all (the camera data come from it, the cached result alone is not enough); with the reference there and other
//! sources missing, the cached result stays usable but cannot be changed.
//!
//! Two levels are cached:
//! - the **base**: the linear merge of the settings without areas and delivery ([`hdr::Settings::base`]), its
//!   preview, every frame aligned at preview size, the detected motion and where the frames lie
//!   ([`hdr::regions::Geometry`]) – key `base_key`;
//! - the **result** shown: the base with the areas pasted over it ([`hdr::regions`]) and, for "Fusion", fused – key
//!   `key`. An area changes only this level: no merge, only the pasting (and the fusion).
//!
//! The photo being worked on ([`set_live`]) keeps its loaded frames and the base in memory, so that a change of the
//! big switches (reference, motion, alignment) merges again without reading the RAWs, and an area is pasted at once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Edge of the stored preview of a merge (same as the document preview).
pub const PREVIEW_EDGE: usize = crate::doc::PREVIEW_EDGE;

/// What a document needs to know about its merge.
#[derive(Debug, Clone)]
pub struct MergedSource {
    /// library file of the result
    pub file: i64,
    /// current paths of the sources, in source order
    pub sources: Vec<PathBuf>,
    /// settings of the result shown (older brush strokes already turned into areas)
    pub settings: hdr::Settings,
    /// cache key of the result shown (sources, settings, algorithm)
    pub key: String,
    /// cache key of the merge without areas and delivery
    pub base_key: String,
    /// why the stored recipe cannot be used (settings not readable, fewer than two sources): nothing is computed and
    /// nothing may be stored over it
    pub unreadable: Option<String>,
}

impl MergedSource {
    /// Path of the reference frame (the settings' reference or the middle exposure as stored in the summary).
    pub fn reference(&self) -> PathBuf {
        let i = reference_index(self.settings.reference, || read_meta(&self.key).map(|m| m.summary.reference), self.sources.len());
        self.sources.get(i).cloned().unwrap_or_default()
    }

    /// A source built by hand (a merge just created: the settings are the ones just stored).
    pub fn new(file: i64, sources: Vec<PathBuf>, settings: hdr::Settings, key: String, base_key: String) -> Self {
        MergedSource { file, sources, settings, key, base_key, unreadable: None }
    }

    /// Sources that cannot be read now.
    pub fn missing(&self) -> Vec<PathBuf> {
        library::merge::missing_sources(&self.sources)
    }
}

/// Index of the reference frame among `n` sources: the one the settings name while it is one of them, otherwise the
/// one the merge chose (`computed`, from its summary), otherwise the middle one – the rule of the merge itself
/// (`hdr::plan_with_warps`: an index outside the frames counts as "none", the middle exposure is taken). Before, an
/// index outside gave the first source here: camera data and pixels came from different frames.
pub(crate) fn reference_index(settings: Option<usize>, computed: impl FnOnce() -> Option<usize>, n: usize) -> usize {
    settings.filter(|r| *r < n).or_else(|| computed().filter(|r| *r < n)).unwrap_or(n / 2)
}

/// JSON of the settings for the cache keys.
fn json(s: &hdr::Settings) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Error text of a recipe that cannot be read (start of the message; the app shows its own text for it).
pub const UNREADABLE: &str = "HDR-Rezept nicht lesbar";

/// The settings of a stored recipe. A text that does not read as settings (damaged, or written by a newer version
/// with values this one does not know) is an error – never the default settings: storing those would overwrite the
/// areas and switches of the recipe.
pub fn parse_settings(stored: &str) -> Result<hdr::Settings, String> {
    let mut settings: hdr::Settings = serde_json::from_str(stored).map_err(|e| format!("{UNREADABLE}: {e}"))?;
    settings.migrate();
    Ok(settings)
}

/// Is the stored recipe of `file` usable (readable settings, at least two sources)? Asked before new settings are
/// stored over it.
pub fn check_recipe(db: &library::db::Db, file: i64) -> Result<(), String> {
    let r = library::merge::recipe(&db.conn, file).ok_or_else(|| format!("{UNREADABLE}: sources"))?;
    if r.sources.len() < hdr::MIN_FRAMES {
        return Err(format!("{UNREADABLE}: {} sources", r.sources.len()));
    }
    parse_settings(&r.settings).map(|_| ())
}

fn usable(sources: &[PathBuf], settings: Result<hdr::Settings, String>) -> (hdr::Settings, Option<String>) {
    match settings {
        Ok(s) if sources.len() >= hdr::MIN_FRAMES => (s, None),
        Ok(s) => (s, Some(format!("{UNREADABLE}: {} sources", sources.len()))),
        Err(e) => (hdr::Settings::default(), Some(e)),
    }
}

/// The merged photo at `path` (a library file without a file on disk), if it is one.
pub fn lookup(path: &Path) -> Option<MergedSource> {
    let (r, sources, key) = library::merge::recipe_for_path(path)?;
    let (settings, unreadable) = usable(&sources, parse_settings(&r.settings));
    let base_key = library::merge::keys_for(r.file, &[&json(&settings.base())])?.pop()?;
    Some(MergedSource { file: r.file, sources, settings, key, base_key, unreadable })
}

/// The merged file `file` with other settings than stored (a change being tried, an area hidden for a comparison):
/// its sources and the keys of these settings. The stored settings keep their stored key.
pub fn source_for(file: i64, settings: &hdr::Settings) -> Option<MergedSource> {
    let (r, sources) = library::merge::recipe_of_file(file)?;
    let (stored, unreadable) = usable(&sources, parse_settings(&r.settings));
    let shown = if unreadable.is_none() && stored == *settings { r.settings.clone() } else { json(settings) };
    let mut keys = library::merge::keys_for(file, &[&shown, &json(&settings.base())])?;
    let base_key = keys.pop()?;
    let key = keys.pop()?;
    Some(MergedSource { file, sources, settings: settings.clone(), key, base_key, unreadable })
}

/// Folder of the computed merges in the app cache.
pub fn cache_dir() -> PathBuf {
    sysdirs::app_cache_dir(sysdirs::APP).unwrap_or_else(std::env::temp_dir).join("merge")
}

fn full_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.hdrc"))
}

fn preview_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}-preview.hdrc"))
}

fn meta_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.json"))
}

fn camera_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}-camera.json"))
}

/// The camera data stored beside the result `key` (see [`crate::doc::CameraData`]).
pub(crate) fn read_camera(key: &str) -> Option<crate::doc::CameraData> {
    serde_json::from_slice::<crate::doc::CameraData>(&std::fs::read(camera_path(key)).ok()?).ok().filter(|c| c.readable())
}

/// Is the camera data of the reference stored beside the result `key` (it opens without the reference file)?
pub fn camera_cached(key: &str) -> bool {
    camera_path(key).is_file()
}

/// Store the camera data beside the result `key` unless it is there (atomic; never an error).
pub(crate) fn write_camera(key: &str, c: &crate::doc::CameraData) {
    let path = camera_path(key);
    if path.is_file() {
        return;
    }
    let Ok(json) = serde_json::to_vec(c) else { return };
    let tmp = path.with_extension(format!("{}.{}", std::process::id(), hdr::store::TMP_EXTENSION));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Make sure the camera data of the reference lies beside the result of `src` (after it was computed): the
/// reference is opened once for it; a result that has it already costs nothing.
pub fn keep_camera(src: &MergedSource) {
    if camera_path(&src.key).is_file() || !preview_cached(&src.key) {
        return;
    }
    let reference = src.reference();
    // (the same reference as for the last result: a change of the recipe does not open the RAW again)
    static LAST: Mutex<Option<(PathBuf, Option<std::time::SystemTime>, crate::doc::CameraData)>> = Mutex::new(None);
    let t = mtime(&reference);
    let known = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|l| l.0 == reference && l.1 == t && t.is_some()).map(|l| l.2.clone());
    let data = match known {
        Some(d) => d,
        None => {
            let Ok(Some(doc)) = crate::RawDoc::open(&reference, &|| true) else { return };
            if doc.apple.is_some() || doc.finished {
                return;
            }
            let d = doc.camera_data();
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((reference.clone(), t, d.clone()));
            d
        }
    };
    write_camera(&src.key, &data);
}

fn base_path(key: &str, part: &str) -> PathBuf {
    cache_dir().join(format!("{key}-base{part}"))
}

/// Stored facts about a computed merge.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub summary: hdr::Summary,
    /// brightest value the merge can hold relative to the reference white (1 / smallest exposure factor)
    pub headroom: f32,
    pub algo: u32,
    /// colour pattern of the sensor (for the noise profile)
    pub pattern: [[u8; 6]; 6],
    /// key of the merge without areas this result was made from
    #[serde(default)]
    pub base: Option<String>,
}

/// Stored facts about the merge without areas.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BaseMeta {
    pub summary: hdr::Summary,
    /// headroom of the linear merge
    pub headroom: f32,
    pub algo: u32,
    pub pattern: [[u8; 6]; 6],
    pub geometry: hdr::regions::Geometry,
    /// a plane of the detected motion is stored
    pub motion: bool,
    /// size of the preview
    pub preview: [usize; 2],
}

pub fn read_meta(key: &str) -> Option<Meta> {
    serde_json::from_slice(&std::fs::read(meta_path(key)).ok()?).ok()
}

pub fn read_base_meta(base_key: &str) -> Option<BaseMeta> {
    serde_json::from_slice(&std::fs::read(base_path(base_key, ".json")).ok()?).ok()
}

/// Is the full-resolution result of this merge in the cache?
pub fn cached(key: &str) -> bool {
    full_path(key).is_file() && meta_path(key).is_file()
}

/// Is a preview of this merge in the cache?
pub fn preview_cached(key: &str) -> bool {
    preview_path(key).is_file() && meta_path(key).is_file()
}

pub fn read_preview(key: &str) -> Option<hdr::Merged> {
    hdr::store::read(&preview_path(key)).ok()
}

pub fn read_full(key: &str) -> Option<hdr::Merged> {
    hdr::store::read(&full_path(key)).ok()
}

/// Running computations per key: a document opening while the job computes waits for it instead of computing twice.
fn key_lock(key: &str) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<String, Arc<Mutex<()>>>>> = Mutex::new(None);
    let mut g = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new).entry(key.to_string()).or_default().clone()
}

/// Work on the cache files that runs beside a computation (storing a base, tidying older states) and is not done yet.
static BESIDE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Run `f` beside the work (the result does not wait for the disk).
fn beside(f: impl FnOnce() + Send + 'static) {
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            BESIDE.fetch_sub(1, Ordering::SeqCst);
        }
    }
    BESIDE.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let _done = Done;
        f()
    });
}

/// Is the cache still being written or tidied beside a computation? (Tests wait for it before they look at the
/// cache files.)
pub fn cache_busy() -> bool {
    BESIDE.load(Ordering::SeqCst) > 0
}

/// Phase of a running merge (for the progress shown to the user).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Analyze,
    Preview,
    Full,
}

// ─────────────────────────────────────────────── the photo being worked on (frames and base in memory)

/// File of the merged photo the user works on (−1: none).
static LIVE_FILE: AtomicI64 = AtomicI64::new(-1);

/// Base of the live photo in memory.
struct Live {
    base_key: String,
    meta: BaseMeta,
    preview: Arc<hdr::Merged>,
    frames: Vec<Arc<hdr::Merged>>,
    motion: Option<Arc<Vec<f32>>>,
    full: Option<Arc<hdr::Merged>>,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);
/// Loaded frames of the live photo: path, modification time, frame.
static FRAMES: Mutex<Vec<(PathBuf, Option<std::time::SystemTime>, Arc<hdr::frames::Frame>)>> = Mutex::new(Vec::new());

/// Alignment of the last base of the live photo: (frames, reference, alignment on) → warps.
static WARPS: Mutex<Option<(String, Vec<hdr::align::Warp>, Vec<usize>)>> = Mutex::new(None);

/// Memory the live photo may keep (bytes): loaded frames up to this, the full-resolution base only when there is room
/// for it as well. Set by the app from the operating mode.
static KEEP_LIMIT: AtomicU64 = AtomicU64::new(2 << 30);

pub fn set_memory_limit(bytes: u64) {
    KEEP_LIMIT.store(bytes, Ordering::Relaxed);
}

fn frame_bytes(f: &hdr::frames::Frame) -> u64 {
    (f.mosaic.len() * 4 + f.planar.as_ref().map_or(0, |p| p.len() * 12) + (f.g.p.len() + f.mx.p.len()) * 4) as u64
}

/// Bytes the live photo holds now (frames and base), for the log and the tests.
pub fn live_bytes() -> u64 {
    let frames: u64 = FRAMES.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, _, f)| frame_bytes(f)).sum();
    let base: u64 = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |l| {
        let px = |m: &hdr::Merged| (m.px.len() * 12) as u64;
        px(&l.preview) + l.frames.iter().map(|f| px(f)).sum::<u64>() + l.full.as_ref().map_or(0, |f| px(f))
    });
    frames + base
}

static WARMING: AtomicI64 = AtomicI64::new(-1);

/// Load the frames of the live photo ahead (the tool is open: the first change then merges without reading the RAWs).
/// Quiet, once per photo; nothing happens when they are in memory already.
pub fn warm(src: &MergedSource) {
    if !is_live(src.file) || src.missing().len() > 0 {
        return;
    }
    if WARMING.swap(src.file, Ordering::Relaxed) == src.file {
        return;
    }
    let sources = src.sources.clone();
    std::thread::spawn(move || {
        library::platform::set_priority(library::platform::Priority::Utility);
        let cancel = AtomicBool::new(false);
        let p = |_: f32| {};
        let _ = load_frames(&sources, true, &hdr::Control { cancel: &cancel, progress: &p });
    });
}

/// The user works on this merged photo (`None`: on none): its frames and base stay in memory; switching to another
/// photo releases them.
pub fn set_live(file: Option<i64>) {
    let f = file.unwrap_or(-1);
    let before = LIVE_FILE.swap(f, Ordering::Relaxed);
    if before != f {
        if before >= 0 {
            // the photo is left: only the states of its stored recipe stay in the cache
            beside(move || {
                tidy(before, true);
            });
        }
        *LIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        FRAMES.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *WARPS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        WARMING.store(-1, Ordering::Relaxed);
    }
}

fn is_live(file: i64) -> bool {
    LIVE_FILE.load(Ordering::Relaxed) == file
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Load the frames of `paths` (from memory for the live photo), side by side; progress 0 … 0.7 of the analysis.
fn load_frames(paths: &[PathBuf], keep: bool, ctl: &hdr::Control) -> Result<Vec<hdr::frames::Frame>, hdr::Error> {
    let done = std::sync::atomic::AtomicUsize::new(0);
    let loaded: Vec<Result<hdr::frames::Frame, hdr::Error>> = {
        use rayon::prelude::*;
        paths
            .par_iter()
            .map(|p| {
                ctl.check()?;
                let t = mtime(p);
                let have = keep.then(|| FRAMES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, m, _)| q == p && *m == t).map(|(_, _, f)| f.clone())).flatten();
                let f = match have {
                    Some(f) => (*f).clone(),
                    None => {
                        let f = hdr::profile("load", || hdr::frames::load(p))?;
                        if keep {
                            let mut g = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
                            g.retain(|(q, _, _)| q != p);
                            // only as much as the operating mode allows (oldest frames go first)
                            let limit = KEEP_LIMIT.load(Ordering::Relaxed);
                            if frame_bytes(&f) <= limit {
                                g.push((p.clone(), t, Arc::new(f.clone())));
                            }
                            while g.len() > hdr::MAX_FRAMES || g.iter().map(|(_, _, f)| frame_bytes(f)).sum::<u64>() > limit {
                                g.remove(0);
                            }
                        }
                        f
                    }
                };
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                ctl.report(n as f32 / paths.len() as f32 * 0.7);
                Ok(f)
            })
            .collect()
    };
    loaded.into_iter().collect()
}

/// One frame for pasting areas at full resolution (memory of the live photo, otherwise read).
fn frame_for(path: &Path) -> Option<Arc<hdr::frames::Frame>> {
    let t = mtime(path);
    if let Some(f) = FRAMES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, m, _)| q == path && *m == t).map(|(_, _, f)| f.clone()) {
        return Some(f);
    }
    hdr::frames::load(path).ok().map(Arc::new)
}

fn plane(m: &hdr::Merged) -> Vec<f32> {
    m.px.iter().map(|p| p[1]).collect()
}

/// The base of a merge: from memory, the cache or computed (analysis, preview, the frames, the motion). Returns the plan
/// when it was computed here (the full resolution then merges without analysing again).
fn base_parts(src: &MergedSource, cancel: &AtomicBool, progress: &(dyn Fn(Phase, f32) + Sync)) -> Result<(BaseMeta, Arc<hdr::Merged>, Vec<Arc<hdr::Merged>>, Option<hdr::Plan>), String> {
    let live = is_live(src.file);
    if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|l| l.base_key == src.base_key) {
        return Ok((l.meta.clone(), l.preview.clone(), l.frames.clone(), None));
    }
    let n = src.sources.len();
    let stored = read_base_meta(&src.base_key).and_then(|m| {
        let pv = hdr::store::read(&base_path(&src.base_key, "-preview.hdrc")).ok()?;
        let frames: Option<Vec<Arc<hdr::Merged>>> = (0..n).map(|i| hdr::store::read(&base_path(&src.base_key, &format!("-frame{i}.hdrc"))).ok().map(Arc::new)).collect();
        Some((m, Arc::new(pv), frames?))
    });
    if let Some((m, pv, frames)) = stored {
        if live {
            let motion = m.motion.then(|| hdr::store::read(&base_path(&src.base_key, "-motion.hdrc")).ok()).flatten().map(|p| Arc::new(plane(&p)));
            *LIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Live { base_key: src.base_key.clone(), meta: m.clone(), preview: pv.clone(), frames: frames.clone(), motion, full: None });
        }
        return Ok((m, pv, frames, None));
    }
    let missing = src.missing();
    if !missing.is_empty() {
        return Err(format!("{MISSING}{}", names(&missing)));
    }
    std::fs::create_dir_all(cache_dir()).map_err(|e| e.to_string())?;
    let base = src.settings.base();
    let pa = |f: f32| progress(Phase::Analyze, f);
    let ctl = hdr::Control { cancel, progress: &pa };
    let fs = load_frames(&src.sources, live, &ctl).map_err(|e| e.to_string())?;
    // the alignment of the last base of the same frames, reference and alignment (only motion changed)
    let align_key = format!("{:?}|{:?}|{}", src.sources, base.reference, base.align);
    let known = live.then(|| WARPS.lock().unwrap_or_else(|e| e.into_inner()).clone()).flatten().filter(|(k, _, _)| *k == align_key).map(|(_, w, r)| (w, r));
    let plan = hdr::profile("plan", || hdr::plan_with_alignment(fs, &base, &ctl, known.as_ref().map(|(w, r)| (w.as_slice(), r.as_slice())))).map_err(|e| e.to_string())?;
    if live {
        *WARPS.lock().unwrap_or_else(|e| e.into_inner()) = Some((align_key, plan.warps.clone(), plan.rejected.clone()));
    }
    let pp = |f: f32| progress(Phase::Preview, f);
    let ctl = hdr::Control { cancel, progress: &pp };
    let mut pv = hdr::profile("preview", || hdr::merge::preview(&plan, PREVIEW_EDGE, &ctl)).map_err(|e| e.to_string())?;
    // (as it is stored: results made from this base are the same whether it comes from memory or from the cache)
    hdr::store::quantize(&mut pv);
    let frames: Vec<Arc<hdr::Merged>> = hdr::profile("preview-frames", || hdr::merge::preview_frames(&plan, PREVIEW_EDGE, &ctl))
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|px| {
            let mut m = hdr::Merged { px, w: pv.w, h: pv.h, noise: pv.noise };
            hdr::store::quantize(&mut m);
            Arc::new(m)
        })
        .collect();
    let motion: Option<Vec<f32>> = (base.ghost != hdr::ghost::Level::Off).then(|| {
        let maps = hdr::ghost::detect(&plan);
        (maps.area > 0.0 || maps.holes > 0.0).then(|| hdr::ghost::overlay(&plan, &maps, pv.w, pv.h))
    }).flatten();
    let meta = BaseMeta { summary: plan.summary(), headroom: plan.headroom(), algo: hdr::ALGO_VERSION, pattern: plan.frames[plan.reference].pattern, geometry: plan.geometry(), motion: motion.is_some(), preview: [pv.w, pv.h] };
    let pv = Arc::new(pv);
    let motion = motion.map(Arc::new);
    if live {
        *LIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Live { base_key: src.base_key.clone(), meta: meta.clone(), preview: pv.clone(), frames: frames.clone(), motion: motion.clone(), full: None });
    }
    // stored beside the work (the result does not wait for the disk)
    {
        let (key, pv, frames, motion, meta) = (src.base_key.clone(), pv.clone(), frames.clone(), motion.clone(), meta.clone());
        beside(move || {
            let _ = hdr::store::write(&base_path(&key, "-preview.hdrc"), &pv);
            for (i, f) in frames.iter().enumerate() {
                let _ = hdr::store::write(&base_path(&key, &format!("-frame{i}.hdrc")), f);
            }
            if let Some(m) = &motion {
                let _ = hdr::store::write(&base_path(&key, "-motion.hdrc"), &hdr::Merged { px: m.iter().map(|v| [*v; 3]).collect(), w: pv.w, h: pv.h, noise: pv.noise });
            }
            if let Ok(j) = serde_json::to_vec_pretty(&meta) {
                let _ = std::fs::write(base_path(&key, ".json"), j);
            }
        });
    }
    Ok((meta, pv, frames, Some(plan)))
}

/// The full-resolution base: from memory, the cache or merged (with `plan` when the analysis was just done).
fn base_full(src: &MergedSource, plan: Option<hdr::Plan>, cancel: &AtomicBool, progress: &(dyn Fn(Phase, f32) + Sync)) -> Result<Arc<hdr::Merged>, String> {
    if let Some(f) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|l| l.base_key == src.base_key).and_then(|l| l.full.clone()) {
        return Ok(f);
    }
    let path = base_path(&src.base_key, ".hdrc");
    let full = match hdr::store::read(&path).ok() {
        Some(m) => Arc::new(m),
        None => {
            let mut plan = match plan {
                Some(p) => p,
                None => {
                    let pa = |f: f32| progress(Phase::Analyze, f);
                    let ctl = hdr::Control { cancel, progress: &pa };
                    let fs = load_frames(&src.sources, is_live(src.file), &ctl).map_err(|e| e.to_string())?;
                    hdr::plan_from_frames(fs, &src.settings.base(), &ctl).map_err(|e| e.to_string())?
                }
            };
            let pf = |f: f32| progress(Phase::Full, f);
            let ctl = hdr::Control { cancel, progress: &pf };
            // X-Trans: Markesteijn on the GPU where available (as for single photos); the CPU stays the fallback
            let gpu = std::env::var("GRAINEO_GPU_DEMOSAIC").as_deref() != Ok("0") && crate::gpu::gpu().is_some();
            let hook = move |m: &[f32], w: usize, h: usize, pat: [[u8; 6]; 6], sensor: raw::Sensor| -> Option<Vec<[f32; 3]>> {
                if gpu && sensor == raw::Sensor::XTrans {
                    crate::gpu::xtrans::markesteijn(m, w, h, pat)
                } else {
                    None
                }
            };
            let mut m = hdr::merge::full_with(&mut plan, &ctl, Some(&hook)).map_err(|e| e.to_string())?;
            hdr::store::quantize(&mut m);
            let m = Arc::new(m);
            drop(plan);
            let (m2, p2) = (m.clone(), path.clone());
            beside(move || {
                let _ = hdr::store::write(&p2, &m2);
            });
            m
        }
    };
    // the full base stays in memory only when the frames leave room for it (else it is read from the cache again)
    let held: u64 = FRAMES.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, _, f)| frame_bytes(f)).sum();
    if held + (full.px.len() * 12) as u64 <= KEEP_LIMIT.load(Ordering::Relaxed) {
        if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|l| l.base_key == src.base_key) {
            l.full = Some(full.clone());
        }
    }
    Ok(full)
}

/// The result shown from a base image: the areas pasted, fused for "Fusion".
fn finish(base: &hdr::Merged, settings: &hdr::Settings, patch: &mut dyn FnMut(usize, [usize; 4]) -> Option<Vec<[f32; 3]>>, cancel: &AtomicBool) -> Result<hdr::Merged, String> {
    let mut px = base.px.clone();
    hdr::regions::apply(&mut px, base.w, base.h, &settings.regions, &[], patch);
    let m = hdr::Merged { px, w: base.w, h: base.h, noise: base.noise };
    match settings.output {
        hdr::fusion::Output::Linear => Ok(m),
        hdr::fusion::Output::Fusion => {
            let p = |_: f32| {};
            let mut f = hdr::fusion::fuse(&m, &hdr::Control { cancel, progress: &p }).map_err(|e| e.to_string())?;
            hdr::fusion::blend(&m, &mut f, settings.fusion_strength);
            Ok(f)
        }
    }
}

/// Preview of the result for these settings from the base (areas from the aligned frames, fusion).
fn finish_preview(meta: &BaseMeta, base: &hdr::Merged, frames: &[Arc<hdr::Merged>], settings: &hdr::Settings, cancel: &AtomicBool) -> Result<hdr::Merged, String> {
    let _ = meta;
    finish(base, settings, &mut |f, rect| frames.get(f).map(|m| hdr::regions::cut(&m.px, m.w, rect)), cancel)
}

fn result_meta(b: &BaseMeta, base_key: &str, settings: &hdr::Settings) -> Meta {
    let headroom = if settings.output == hdr::fusion::Output::Fusion { 1.0 } else { b.headroom };
    Meta { summary: b.summary.clone(), headroom, algo: b.algo, pattern: b.pattern, base: Some(base_key.to_string()) }
}

/// Start of the error of a merge that needs a source file that is not there (the names follow).
pub const MISSING: &str = "Quelle fehlt: ";
/// Start of the error of a merge that needs a source file that is there and cannot be read (the names follow).
pub const UNREADABLE_SOURCE: &str = "Quelle nicht lesbar: ";

fn names(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>().join(", ")
}

/// The source files the areas of `src` take their pixels from at full resolution: the frame of every area, and the
/// reference (it fills what a frame does not cover) when an area takes another frame.
fn area_sources(src: &MergedSource) -> Vec<PathBuf> {
    let n = src.sources.len();
    let reference = reference_index(src.settings.reference, || read_base_meta(&src.base_key).map(|m| m.summary.reference), n);
    let mut frames: Vec<usize> = Vec::new();
    for a in src.settings.regions.iter().filter(|a| !a.is_empty() && a.frame < n) {
        for f in [a.frame, reference] {
            if !frames.contains(&f) {
                frames.push(f);
            }
        }
    }
    frames.into_iter().filter_map(|f| src.sources.get(f).cloned()).collect()
}

/// Compute a merge and store it in the cache: analysis, quick preview (handed to `on_preview` right away), full
/// resolution. `progress(phase, fraction)`; `cancel` stops it (nothing is stored then). Does nothing if the cache
/// already holds the result.
pub fn compute(src: &MergedSource, cancel: &AtomicBool, progress: &(dyn Fn(Phase, f32) + Sync), on_preview: &mut dyn FnMut(&hdr::Merged, &Meta)) -> Result<Meta, String> {
    compute_with(src, cancel, progress, on_preview, &mut |_, _| {})
}

/// [`compute`] that also hands the full resolution to `on_full` before it is stored.
pub fn compute_with(src: &MergedSource, cancel: &AtomicBool, progress: &(dyn Fn(Phase, f32) + Sync), on_preview: &mut dyn FnMut(&hdr::Merged, &Meta), on_full: &mut dyn FnMut(&hdr::Merged, &Meta)) -> Result<Meta, String> {
    if let Some(e) = &src.unreadable {
        return Err(e.clone());
    }
    if src.sources.len() < hdr::MIN_FRAMES {
        return Err(hdr::Error::TooFewFrames(src.sources.len()).to_string());
    }
    let lock = key_lock(&src.key);
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    used(src);
    if cached(&src.key) {
        // computed before (e.g. a switch turned back): handed over from the cache – when all of it reads. A result
        // whose files are damaged is computed again (before, it counted as done and never got its full resolution).
        if let (Some(m), Some(pv), Some(f)) = (read_meta(&src.key), read_preview(&src.key), read_full(&src.key)) {
            on_preview(&pv, &m);
            on_full(&f, &m);
            return Ok(m);
        }
        let _ = std::fs::remove_file(full_path(&src.key));
    }
    // Areas take their pixels from the RAW files at full resolution: with one of them gone the result could only be
    // stored without that area (the preview, made from cached frames, would show it). Nothing is computed then.
    let gone: Vec<PathBuf> = area_sources(src).into_iter().filter(|p| !p.is_file()).collect();
    if !gone.is_empty() {
        return Err(format!("{MISSING}{}", names(&gone)));
    }
    let check = || if cancel.load(Ordering::Relaxed) { Err(hdr::Error::Cancelled.to_string()) } else { Ok(()) };
    let base_lock = key_lock(&format!("base-{}", src.base_key));
    let bg = base_lock.lock().unwrap_or_else(|e| e.into_inner());
    let (bm, bpv, frames, plan) = base_parts(src, cancel, progress)?;
    check()?;
    std::fs::create_dir_all(cache_dir()).map_err(|e| e.to_string())?;
    let meta = result_meta(&bm, &src.base_key, &src.settings);
    let pv = hdr::profile("finish-preview", || finish_preview(&bm, &bpv, &frames, &src.settings, cancel))?;
    check()?;
    on_preview(&pv, &meta);
    trace(|| format!("job writes preview of {}", src.key));
    std::fs::write(meta_path(&src.key), serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    hdr::store::write(&preview_path(&src.key), &pv).map_err(|e| e.to_string())?;
    drop(pv);
    let full = base_full(src, plan, cancel, progress)?;
    drop(bg);
    check()?;
    let full = if src.settings.regions.is_empty() && src.settings.output == hdr::fusion::Output::Linear {
        full
    } else {
        let geo = bm.geometry.clone();
        let sources = src.sources.clone();
        let mut loaded: HashMap<usize, Option<Arc<hdr::frames::Frame>>> = HashMap::new();
        // (the darkest frame with the highlight handling the merge gave it: an area over a blown place shows at full
        // resolution what its preview shows)
        let load = |f: usize| -> Option<Arc<hdr::frames::Frame>> {
            let fr = sources.get(f).and_then(|p| frame_for(p))?;
            Some(hdr::regions::for_patches(&fr, &geo, f).map_or(fr, Arc::new))
        };
        // frames an area needs that could not be read: the result is not stored without them
        let mut unreadable: Vec<usize> = Vec::new();
        let finished = {
            let mut patch = |f: usize, rect: [usize; 4]| -> Option<Vec<[f32; 3]>> {
                let Some(fr) = loaded.entry(f).or_insert_with(|| load(f)).clone() else {
                    if !unreadable.contains(&f) {
                        unreadable.push(f);
                    }
                    return None;
                };
                let mut px = hdr::regions::full_patch(&fr, &geo, f, rect);
                // where the frame does not cover the area the reference shows, as in the preview (before, the full
                // resolution kept the merge there: the two differed)
                if f != geo.reference && px.iter().any(|p| !(p[0].is_finite() && p[1].is_finite() && p[2].is_finite())) {
                    let r = geo.reference;
                    match loaded.entry(r).or_insert_with(|| load(r)).clone() {
                        Some(rf) => {
                            hdr::regions::fill_uncovered(&mut px, &hdr::regions::full_patch(&rf, &geo, r, rect));
                        }
                        None if !unreadable.contains(&r) => unreadable.push(r),
                        None => {}
                    }
                }
                Some(px)
            };
            hdr::profile("finish-full", || finish(&full, &src.settings, &mut patch, cancel))?
        };
        if !unreadable.is_empty() {
            let paths: Vec<PathBuf> = unreadable.iter().filter_map(|f| src.sources.get(*f).cloned()).collect();
            let head = if paths.iter().any(|p| !p.is_file()) { MISSING } else { UNREADABLE_SOURCE };
            return Err(format!("{head}{}", names(&paths)));
        }
        Arc::new(finished)
    };
    check()?;
    on_full(&full, &meta);
    hdr::store::write(&full_path(&src.key), &full).map_err(|e| e.to_string())?;
    // older states of this photo go (see [`tidy`]); beside the work, never an error. A photo that was left while
    // this was computed keeps only the states of its stored recipe (the tidying on leaving it waited for this job).
    // Part of the job, not beside it: while this runs the job is still listed, under its own number – the tidying
    // knows exactly which computation is its own. (Beside the job, after it had left the list, "one job of this photo
    // is mine" took a follow-up job for the own one and could remove the files that one was writing.)
    keep_camera(src);
    tidy_as(src.file, !is_live(src.file), OWN_JOB.with(|j| j.get()));
    Ok(meta)
}

/// The aligned frames of a merge at preview size (memory or cache), for looking at one exposure and for choosing the
/// frame of an area.
pub fn frames(base_key: &str, n: usize) -> Option<(BaseMeta, Vec<Arc<hdr::Merged>>)> {
    if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|l| l.base_key == base_key) {
        return Some((l.meta.clone(), l.frames.clone()));
    }
    let m = read_base_meta(base_key)?;
    let frames: Option<Vec<Arc<hdr::Merged>>> = (0..n).map(|i| hdr::store::read(&base_path(base_key, &format!("-frame{i}.hdrc"))).ok().map(Arc::new)).collect();
    Some((m, frames?))
}

/// Frame `i` of a merge at full resolution for the rectangle `rect` = (x, y, w, h) of the crop, aligned and in reference
/// units like the merge (memory of the live photo, otherwise read from its RAW). For looking at one exposure closely.
pub fn frame_patch(src: &MergedSource, i: usize, rect: [usize; 4]) -> Option<Vec<[f32; 3]>> {
    let meta = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|l| l.base_key == src.base_key).map(|l| l.meta.clone()).or_else(|| read_base_meta(&src.base_key))?;
    let f = frame_for(src.sources.get(i)?)?;
    Some(hdr::regions::full_patch(&f, &meta.geometry, i, rect))
}

/// The detected motion of a merge at preview size (0 … 1, sensor orientation, covering the crop), `None` without any.
pub fn motion(base_key: &str) -> Option<(Arc<Vec<f32>>, usize, usize)> {
    if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|l| l.base_key == base_key) {
        return l.motion.clone().map(|m| (m, l.meta.preview[0], l.meta.preview[1]));
    }
    let m = read_base_meta(base_key).filter(|m| m.motion)?;
    let p = hdr::store::read(&base_path(base_key, "-motion.hdrc")).ok()?;
    Some((Arc::new(plane(&p)), m.preview[0], m.preview[1]))
}

// ─────────────────────────────────────────────── keeping the cache small

/// States of a merged photo kept while it is worked on, beside the stored one: results (a change taken back shows at
/// once) and bases (a big switch turned back does not merge again).
const KEEP_RESULTS: usize = 3;
const KEEP_BASES: usize = 2;

/// Keys used in this session per merged file, newest first: (results, bases).
static USED: Mutex<Option<HashMap<i64, (Vec<String>, Vec<String>)>>> = Mutex::new(None);

fn used(src: &MergedSource) {
    let mut g = USED.lock().unwrap_or_else(|e| e.into_inner());
    let (results, bases) = g.get_or_insert_with(HashMap::new).entry(src.file).or_default();
    for (list, key, keep) in [(results, &src.key, KEEP_RESULTS + 1), (bases, &src.base_key, KEEP_BASES + 1)] {
        list.retain(|k| k != key);
        list.insert(0, key.clone());
        list.truncate(keep);
    }
}

/// The key a cache file belongs to (`None`: not a file of this cache, or one written before keys named their photo).
fn key_of(name: &str) -> Option<(i64, &str)> {
    let rest = name.strip_prefix('f')?;
    let (id, tail) = rest.split_once('-')?;
    let file: i64 = id.parse().ok()?;
    let hash = tail.get(..16).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let _ = hash;
    Some((file, &name[..1 + id.len() + 1 + 16]))
}

/// Cache files in `dir` with the photo and key they belong to (`None` for files without one), and their size.
fn inventory(dir: &Path) -> Vec<(PathBuf, Option<(i64, String)>, u64, std::time::SystemTime)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let m = e.metadata().ok()?;
            Some((e.path(), key_of(&name).map(|(f, k)| (f, k.to_string())), m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect()
}

/// Set by tests: the library at hand counts as "not the real one" ([`other_library`]).
static OTHER_LIBRARY: AtomicBool = AtomicBool::new(false);

/// Test: act as the process that looks at a backup does.
#[doc(hidden)]
pub fn test_other_library(on: bool) {
    OTHER_LIBRARY.store(on, Ordering::SeqCst);
}

/// Is the library this process works on another one than the real library ("Sicherung ansehen" works on a copy of
/// a backup)? The cache of the merges belongs to the real library: a process on another library reads it and adds
/// to it, but never removes anything from it – what that library does not know is not unused.
pub fn other_library() -> bool {
    OTHER_LIBRARY.load(Ordering::SeqCst) || library::backup_view::active().is_some() || library::db_path() != library::real_db_path()
}

/// Remove the cache files of `file` in `dir` whose key is none of `keep`. Returns the bytes freed.
fn prune_in(dir: &Path, file: i64, keep: &[String]) -> u64 {
    if other_library() {
        return 0;
    }
    let mut freed = 0;
    for (path, key, bytes, _) in inventory(dir) {
        if key.is_some_and(|(f, k)| f == file && !keep.contains(&k)) && std::fs::remove_file(&path).is_ok() {
            trace(|| format!("pruned {} (kept {keep:?})", path.file_name().unwrap_or_default().to_string_lossy()));
            freed += bytes;
        }
    }
    freed
}

/// Log of what happens to the cache files (`GRAINEO_HDR_CACHE_LOG=1`, stderr): for finding out who removed what.
fn trace(text: impl FnOnce() -> String) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("GRAINEO_HDR_CACHE_LOG").is_some()) {
        eprintln!("hdr-cache [{:?}] {}", std::thread::current().id(), text());
    }
}

/// Are all source files of the merged photo `file` there now?
fn sources_reachable(file: i64) -> bool {
    library::merge::recipe_of_file(file).is_some_and(|(_, paths)| !paths.is_empty() && paths.iter().all(|p| p.is_file()))
}

/// The keys the stored recipe of `file` needs (result and base). `None`: no recipe, or one that cannot be read –
/// then nothing of the photo is removed.
fn stored_keys(file: i64) -> Option<Vec<String>> {
    let (r, _) = library::merge::recipe_of_file(file)?;
    let settings = parse_settings(&r.settings).ok()?;
    library::merge::keys_for(file, &[&r.settings, &json(&settings.base())])
}

/// Remove the cached states of the merged photo `file` that nothing needs any more: every key that is neither the
/// stored recipe's (result and base), nor being computed, nor – unless `strict` – one of the newest states used in this
/// session (changes that ⌘Z or the history can return to at once). `strict` when the photo is left. Returns the bytes
/// freed.
pub fn tidy(file: i64, strict: bool) -> u64 {
    tidy_as(file, strict, None)
}

thread_local! {
    /// The number of the registered job ([`run_job_flag`]) this thread computes now.
    static OWN_JOB: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Does another computation of `file` run or wait than the job `own` (`jobs`: number and file of the listed jobs,
/// `waiting`: files with a job waiting for its turn)? While one does, nothing of the photo is removed.
fn others_busy(jobs: &[(u64, i64)], waiting: &[i64], file: i64, own: Option<u64>) -> bool {
    waiting.contains(&file) || jobs.iter().any(|(id, f)| *f == file && Some(*id) != own)
}

/// [`tidy`] called by the job `own` at its end (`None`: by nobody's job).
fn tidy_as(file: i64, strict: bool, own: Option<u64>) -> u64 {
    // sources that are not reachable now: whatever the cache holds of the photo cannot be made again – it all stays
    // (as "Leeren" and the size limit keep it), also the states the stored recipe does not name. (Asked before the
    // gate is taken: a drive that answers slowly must not hold up the start of a computation.)
    if !sources_reachable(file) {
        return 0;
    }
    // (the stored keys are read, the busy list is looked at and the files are removed in one go, see [`CACHE_GATE`])
    let _gate = cache_gate();
    let Some(mut keep) = stored_keys(file) else { return 0 };
    trace(|| format!("tidy file {file} strict {strict} own {own:?} stored {keep:?} busy {:?}", busy_files()));
    if !strict {
        if let Some((results, bases)) = USED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&file)) {
            keep.extend(results.iter().chain(bases.iter()).cloned());
        }
    }
    // a computation of this photo still running writes its files later: nothing of the photo goes while one runs
    // (not strict as well: the keys kept were read before a job that started just now stored its recipe's)
    let jobs: Vec<(u64, i64)> = jobs().iter().map(|j| (j.id, j.file)).collect();
    let waiting: Vec<i64> = WAITING.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if others_busy(&jobs, &waiting, file, own) {
        return 0;
    }
    let freed = prune_in(&cache_dir(), file, &keep);
    if strict {
        if let Some(m) = USED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            m.remove(&file);
        }
    }
    freed
}

/// Tidy the whole cache once (after the start, in the background): every merged photo keeps only the states of its
/// stored recipe; files of photos that are no longer in the library, files without a photo in their name (written
/// before the keys named it) and temporary files left behind by interrupted writers go. Does nothing when the library
/// cannot be read. Returns the bytes freed.
pub fn sweep() -> u64 {
    if other_library() {
        return 0;
    }
    let Some(all) = library::merge::all_virtual() else { return 0 };
    let known: std::collections::HashSet<i64> = all.iter().map(|(f, _)| *f).collect();
    let dir = cache_dir();
    // temporary files of writers that were interrupted (also those of a stored key, which nothing below removes);
    // only old ones, so that no writer at work is hit
    let mut freed = hdr::store::remove_stale_tmp(&dir, hdr::store::STALE_TMP_AGE);
    for (path, key, bytes, _) in inventory(&dir) {
        let orphan = match &key {
            Some((f, _)) => !known.contains(f),
            None => path.extension().is_some_and(|e| e == "hdrc" || e == "json" || e == "tmp"),
        };
        if orphan && !key.as_ref().is_some_and(|(f, _)| busy_files().contains(f)) && std::fs::remove_file(&path).is_ok() {
            freed += bytes;
        }
    }
    for (file, _) in &all {
        freed += tidy(*file, true);
    }
    freed
}

/// Bytes the cache of the merges holds.
pub fn cache_bytes() -> u64 {
    inventory(&cache_dir()).iter().map(|e| e.2).sum()
}

/// Files of the cache that may be trimmed when the cache is over its limit, with size and modification time: the
/// full-resolution results and bases of merged photos whose sources are all reachable now (they can be merged
/// again), and every file of a photo that is gone. Photos being computed are left out.
pub fn trimmable() -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    if other_library() {
        return Vec::new();
    }
    let Some(all) = library::merge::all_virtual() else { return Vec::new() };
    let busy = busy_files();
    let live = LIVE_FILE.load(Ordering::Relaxed);
    let reachable: HashMap<i64, bool> = all.iter().map(|(f, paths)| (*f, !paths.is_empty() && paths.iter().all(|p| p.is_file()))).collect();
    inventory(&cache_dir())
        .into_iter()
        .filter(|(path, key, _, _)| match key {
            Some((f, _)) if busy.contains(f) || *f == live => false,
            Some((f, _)) => match reachable.get(f) {
                Some(ok) => *ok && path.extension().is_some_and(|e| e == "hdrc") && !path.to_string_lossy().ends_with("-preview.hdrc"),
                None => true,
            },
            None => false,
        })
        .map(|(p, _, b, t)| (p, b, t))
        .collect()
}

/// Empty the cache of the merges (settings): everything but the files of photos being computed or worked on and of
/// photos whose sources are not reachable now. The photos stay; their pixels are merged again from the sources when
/// they are opened. Returns the bytes freed.
pub fn clear_cache() -> u64 {
    if other_library() {
        return 0;
    }
    trace(|| "clear_cache".to_string());
    // photos whose sources cannot all be read now keep their result: it could not be merged again (their picture
    // would be the single reference frame, or nothing, until the sources are back). (Asked before the gate is taken:
    // a drive that answers slowly must not hold up the start of a computation.)
    let unreachable: Vec<i64> = library::merge::all_virtual().unwrap_or_default().into_iter().filter(|(_, paths)| paths.is_empty() || !paths.iter().all(|p| p.is_file())).map(|(f, _)| f).collect();
    let _gate = cache_gate();
    let busy = busy_files();
    let live = LIVE_FILE.load(Ordering::Relaxed);
    let mut freed = 0;
    for (path, key, bytes, _) in inventory(&cache_dir()) {
        if key.as_ref().is_some_and(|(f, _)| busy.contains(f) || *f == live || unreachable.contains(f)) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += bytes;
        }
    }
    freed
}

/// Does the cache hold anything computed for the merged photo `file` (a result, a preview, a base)?
pub fn has_cached(file: i64) -> bool {
    inventory(&cache_dir()).iter().any(|e| e.1.as_ref().is_some_and(|(f, _)| *f == file))
}

/// Remove everything the cache holds for the merged photo `file` (the photo is gone for good).
pub fn forget_file(file: i64) {
    trace(|| format!("forget_file {file}"));
    prune_in(&cache_dir(), file, &[]);
    if let Some(m) = USED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(&file);
    }
}

/// Remove the full-resolution result of a merge from the cache (it does not read: it is computed again).
pub fn forget_full(key: &str) {
    let _ = std::fs::remove_file(full_path(key));
}

/// Path of the full-resolution result in the cache (tests damage it).
pub fn full_result_path(key: &str) -> PathBuf {
    full_path(key)
}

/// Remove the cached result of a merge (undo of creating it), with its base.
pub fn forget(key: &str) {
    if let Some(b) = read_meta(key).and_then(|m| m.base) {
        if let Some(m) = read_base_meta(&b) {
            for i in 0..m.summary.ev.len() {
                let _ = std::fs::remove_file(base_path(&b, &format!("-frame{i}.hdrc")));
            }
        }
        for part in [".hdrc", "-preview.hdrc", "-motion.hdrc", ".json"] {
            let _ = std::fs::remove_file(base_path(&b, part));
        }
    }
    for p in [full_path(key), preview_path(key), meta_path(key)] {
        let _ = std::fs::remove_file(p);
    }
}

// ─────────────────────────────────────────────── running merge jobs (title bar)

/// State of a merge being computed in the background.
#[derive(Debug, Clone)]
pub struct Status {
    pub id: u64,
    pub file: i64,
    pub name: String,
    pub phase: Phase,
    pub fraction: f32,
    pub started: std::time::Instant,
    pub cancel: Arc<AtomicBool>,
}

impl Status {
    /// Overall fraction (analysis 15 %, preview 5 %, full resolution 80 %).
    pub fn overall(&self) -> f32 {
        match self.phase {
            Phase::Analyze => 0.15 * self.fraction,
            Phase::Preview => 0.15 + 0.05 * self.fraction,
            Phase::Full => 0.2 + 0.8 * self.fraction,
        }
    }

    /// Remaining seconds, extrapolated from the elapsed time (after the first 5 %).
    pub fn remaining_secs(&self) -> Option<f32> {
        let f = self.overall();
        (f > 0.05).then(|| self.started.elapsed().as_secs_f32() * (1.0 - f) / f)
    }
}

static JOBS: Mutex<Vec<Status>> = Mutex::new(Vec::new());
static GENERATION: AtomicU32 = AtomicU32::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Merges running now.
pub fn jobs() -> Vec<Status> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Changes whenever a job starts, progresses visibly, gets its preview or ends.
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
}

/// Report a change of a merge job to the title bar (a batch moved on to its next merge).
pub fn touch() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Held while cache files are removed by a rule ("nothing computes for this photo, so whatever the recipe does not
/// name can go") and while a computation enters the list of the busy ones. Without it a job could start between the
/// look at the list and the removal – and the files it wrote in that moment were removed as "unused" (stress seed
/// 512287: the result shown lost its preview). With it a job is either seen by the removal, which then leaves the
/// photo alone, or it starts after the removal is through.
static CACHE_GATE: Mutex<()> = Mutex::new(());

fn cache_gate() -> std::sync::MutexGuard<'static, ()> {
    CACHE_GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Files with a job that waits for its turn (one merge at a time, see the app's `hdr_ui`): not computing yet, but
/// about to write cache files – they count as busy wherever cache files are removed.
static WAITING: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// A job of `file` waits for its turn until the returned guard is dropped.
pub fn waiting(file: i64) -> Waiting {
    let _gate = cache_gate();
    WAITING.lock().unwrap_or_else(|e| e.into_inner()).push(file);
    Waiting(file)
}

/// See [`waiting`].
pub struct Waiting(i64);

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut w = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = w.iter().position(|f| *f == self.0) {
            w.remove(i);
        }
    }
}

/// Files a merge is computed for now or is about to be (running and waiting jobs).
pub fn busy_files() -> Vec<i64> {
    let mut v: Vec<i64> = jobs().iter().map(|j| j.file).collect();
    v.extend(WAITING.lock().unwrap_or_else(|e| e.into_inner()).iter().copied());
    v
}

/// How a merge needed for an output (export, print, the full resolution on zooming in) is computed: set by the app
/// to its one entrance for merges (one at a time, its pool and memory budget, shown in the title bar, cancellable).
/// Without it (tools, tests of this crate) the merge is computed where it is asked for.
type Runner = Box<dyn Fn(&MergedSource, bool) -> Result<Meta, String> + Send + Sync>;
static RUNNER: std::sync::OnceLock<Runner> = std::sync::OnceLock::new();

/// Set the entrance for merges of outputs (once; see [`compute_output`]).
pub fn set_output_runner(f: impl Fn(&MergedSource, bool) -> Result<Meta, String> + Send + Sync + 'static) {
    let _ = RUNNER.set(Box::new(f));
}

/// Compute the merge `src` for an output, blocking: through the app's entrance when it set one, so that it never runs
/// beside another merge (two merges at once would each take the whole memory budget).
///
/// `asked`: the user asked for this output (export, print, copy). Without it – the sharp picture of the photo on screen,
/// previews – the app does not merge a result the user cancelled or that failed (it says so and offers to compute).
pub fn compute_output(src: &MergedSource, asked: bool) -> Result<Meta, String> {
    if let Some(m) = read_meta(&src.key).filter(|_| cached(&src.key)) {
        return Ok(m);
    }
    match RUNNER.get() {
        Some(run) => run(src, asked),
        None => {
            let cancel = AtomicBool::new(false);
            compute(src, &cancel, &|_, _| {}, &mut |_, _| {})
        }
    }
}

/// Cancel the merge of `file`.
pub fn cancel(file: i64) {
    for j in JOBS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        if j.file == file {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Run [`compute`] as a registered job (title bar progress, cancellable). Blocking; call on a background thread.
pub fn run_job(src: &MergedSource, name: &str, on_preview: &mut dyn FnMut(&hdr::Merged, &Meta)) -> Result<Meta, String> {
    run_job_with(src, name, on_preview, &mut |_, _| {})
}

/// [`run_job`] with the full resolution handed to `on_full` as well.
pub fn run_job_with(src: &MergedSource, name: &str, on_preview: &mut dyn FnMut(&hdr::Merged, &Meta), on_full: &mut dyn FnMut(&hdr::Merged, &Meta)) -> Result<Meta, String> {
    run_job_flag(src, name, Arc::new(AtomicBool::new(false)), on_preview, on_full)
}

/// [`run_job_with`] with the cancel flag of the caller: a job told to stop before it was registered here (its thread
/// had not started yet) stops as well.
pub fn run_job_flag(src: &MergedSource, name: &str, cancel: Arc<AtomicBool>, on_preview: &mut dyn FnMut(&hdr::Merged, &Meta), on_full: &mut dyn FnMut(&hdr::Merged, &Meta)) -> Result<Meta, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let gate = cache_gate();
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).push(Status { id, file: src.file, name: name.to_string(), phase: Phase::Analyze, fraction: 0.0, started: std::time::Instant::now(), cancel: cancel.clone() });
    drop(gate);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    // the job leaves the list whatever happens to the computation (a panic must not leave it "running" for ever)
    struct Listed(u64);
    impl Drop for Listed {
        fn drop(&mut self) {
            JOBS.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| j.id != self.0);
            GENERATION.fetch_add(1, Ordering::Relaxed);
        }
    }
    let listed = Listed(id);
    let progress = move |phase: Phase, f: f32| {
        let mut g = JOBS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = g.iter_mut().find(|j| j.id == id) {
            let step = (j.overall() * 100.0) as u32;
            j.phase = phase;
            j.fraction = f;
            if (j.overall() * 100.0) as u32 != step {
                GENERATION.fetch_add(1, Ordering::Relaxed);
            }
        }
    };
    struct Own;
    impl Drop for Own {
        fn drop(&mut self) {
            OWN_JOB.with(|j| j.set(None));
        }
    }
    OWN_JOB.with(|j| j.set(Some(id)));
    let _own = Own;
    let r = compute_with(
        src,
        &cancel,
        &progress,
        &mut |m, meta| {
            GENERATION.fetch_add(1, Ordering::Relaxed);
            on_preview(m, meta)
        },
        on_full,
    );
    trace(|| format!("job of {} ends: {:?}; in the cache now: {:?}", src.key, r.as_ref().map(|_| ()), inventory(&cache_dir()).iter().filter(|e| e.1.as_ref().is_some_and(|k| k.0 == src.file)).map(|e| e.0.file_name().unwrap_or_default().to_string_lossy().to_string()).collect::<Vec<_>>()));
    drop(listed);
    r
}

/// Noise profile of a merged stage of size `w` (full width `full_w`).
pub(crate) fn profile(meta: &Meta, info: &raw::Info, model: raw::noise::NoiseModel, w: usize, full_w: usize) -> raw::noise::Profile {
    if w >= full_w {
        hdr::profile_full(model, info, meta.pattern)
    } else {
        hdr::profile_scaled(model, info, meta.pattern, w as f32 / full_w.max(1) as f32)
    }
}

/// Developed image of a merge preview with the standard development of the reference (grid thumbnail), upright.
/// Opens the reference RAW for the camera data.
pub fn render_preview(reference: &Path, pv: &hdr::Merged, meta: &Meta) -> Result<develop::Rgb16, String> {
    render_preview_with(reference, pv, meta, &develop::Settings::default())
}

/// [`render_preview`] with the develop settings of the merged photo.
pub fn render_preview_with(reference: &Path, pv: &hdr::Merged, meta: &Meta, s: &develop::Settings) -> Result<develop::Rgb16, String> {
    let mut doc = crate::RawDoc::open(reference, &|| true)?.ok_or("abgebrochen")?;
    if doc.apple.is_some() || doc.finished {
        return Err("HDR: Referenzbild nicht als Mosaik lesbar".into());
    }
    let profile = profile(meta, &doc.info, pv.noise, pv.w, doc.full_size[0]);
    doc.preview = Arc::new(crate::Stage::new(pv.px.clone(), pv.w, pv.h, Some(profile)));
    let img = doc.render_preview(s);
    Ok(develop::geometry::apply(img, s, doc.orientation))
}

#[cfg(test)]
mod tests {
    /// M4 of the cross-check: the tidying at the end of a job knows its own job by its number. With the own job
    /// already off the list and a follow-up job of the photo running (the order that removed the follow-up's files
    /// when one listed job counted as "mine"), nothing may be removed; with only the own job listed it tidies.
    #[test]
    fn tidying_never_takes_a_follow_up_job_for_its_own() {
        let file = 7;
        // the own job (number 1) left the list, the follow-up (number 2) computes
        assert!(super::others_busy(&[(2, file)], &[], file, Some(1)), "a follow-up job is another job");
        assert!(super::others_busy(&[(2, file)], &[], file, None));
        // only the own job is listed: nobody else
        assert!(!super::others_busy(&[(1, file)], &[], file, Some(1)));
        // own job listed and a follow-up waiting for its turn, or listed
        assert!(super::others_busy(&[(1, file)], &[file], file, Some(1)));
        assert!(super::others_busy(&[(1, file), (2, file)], &[], file, Some(1)));
        // jobs of other photos do not count
        assert!(!super::others_busy(&[(1, file), (3, 8)], &[9], file, Some(1)));
        // through the job itself: the number is set while a registered job computes, and gone afterwards
        assert_eq!(super::OWN_JOB.with(|j| j.get()), None);
    }

    use super::*;

    /// The reference of a merged photo by the rule of the merge itself: the frame the settings name while it is one
    /// of the sources; an index outside (a recipe of more frames once) counts as none – then the frame the merge
    /// chose, otherwise the middle one. Never the first source for an index outside (camera data and pixels came from
    /// different frames).
    #[test]
    fn reference_outside_the_sources_follows_the_merge() {
        assert_eq!(reference_index(Some(1), || Some(2), 3), 1);
        assert_eq!(reference_index(None, || Some(2), 3), 2);
        assert_eq!(reference_index(Some(5), || Some(2), 3), 2, "outside: what the merge chose");
        assert_eq!(reference_index(Some(5), || None, 3), 1, "outside, nothing computed: the middle one");
        assert_eq!(reference_index(Some(5), || Some(7), 3), 1);
        assert_eq!(reference_index(None, || None, 2), 1);
        let paths: Vec<PathBuf> = ["a", "b", "c"].iter().map(PathBuf::from).collect();
        let src = MergedSource::new(4_712_001, paths, hdr::Settings { reference: Some(6), ..Default::default() }, "f4712001-00000000000000aa".into(), "f4712001-00000000000000ab".into());
        assert_eq!(src.reference(), PathBuf::from("b"));
    }

    /// Older states of a merged photo leave the cache: only the keys named stay, files of other photos and files
    /// without a photo in their name are not touched.
    #[test]
    fn stale_states_of_a_photo_are_pruned() {
        let dir = std::env::temp_dir().join(format!("graineo-merge-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = |file: i64, n: u8| format!("{}{:016x}", library::merge::key_prefix(file), n as u64 * 0x1111_1111_1111_1111);
        let write = |name: String| std::fs::write(dir.join(name), [0u8; 10]).unwrap();
        // photo 7: three results on two bases; photo 71 and an old file without a photo in its name
        for n in [1, 2, 3] {
            for part in [".hdrc", "-preview.hdrc", ".json"] {
                write(format!("{}{part}", key(7, n)));
            }
        }
        for n in [8, 9] {
            for part in ["-base.hdrc", "-base-preview.hdrc", "-base-frame0.hdrc", "-base-frame1.hdrc", "-base-motion.hdrc", "-base.json"] {
                write(format!("{}{part}", key(7, n)));
            }
        }
        write(format!("{}.hdrc", key(71, 1)));
        write("0123456789abcdef.hdrc".into());
        assert_eq!(key_of(&format!("{}-base-frame1.hdrc", key(7, 8))), Some((7, key(7, 8).as_str())));
        assert_eq!(key_of("0123456789abcdef.hdrc"), None);
        assert_eq!(key_of("f7-short.hdrc"), None);
        assert_eq!(inventory(&dir).len(), 9 + 12 + 2);
        // the stored recipe needs result 3 on base 9
        let freed = prune_in(&dir, 7, &[key(7, 3), key(7, 9)]);
        assert_eq!(freed, (6 + 6) * 10);
        let left: Vec<String> = {
            let mut v: Vec<String> = inventory(&dir).into_iter().map(|e| e.0.file_name().unwrap().to_string_lossy().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(left.len(), 3 + 6 + 2, "{left:?}");
        assert!(left.iter().all(|n| n.starts_with(&key(7, 3)) || n.starts_with(&key(7, 9)) || n.starts_with("f71-") || n.starts_with("0123")));
        // the photo is gone for good: everything of it, nothing of the others
        assert_eq!(prune_in(&dir, 7, &[]), 9 * 10);
        assert_eq!(inventory(&dir).len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The newest states of the photo being worked on are remembered, newest first, a bounded number.
    #[test]
    fn used_states_are_bounded() {
        let file = 987_654;
        for n in 0..10 {
            used(&MergedSource::new(file, Vec::new(), hdr::Settings::default(), format!("r{n}"), format!("b{}", n / 4)));
        }
        let g = USED.lock().unwrap();
        let (results, bases) = g.as_ref().unwrap().get(&file).unwrap();
        assert_eq!(results, &["r9", "r8", "r7", "r6"]);
        assert_eq!(bases, &["b2", "b1", "b0"]);
    }

    /// A recipe without sources (a damaged row) is an error, never a panic: the reference is empty and nothing is
    /// computed.
    #[test]
    fn a_recipe_without_sources_is_an_error() {
        let src = MergedSource::new(1, Vec::new(), hdr::Settings::default(), "f1-0000000000000000".into(), "f1-0000000000000001".into());
        assert_eq!(src.reference(), PathBuf::new());
        let cancel = AtomicBool::new(false);
        let r = compute(&src, &cancel, &|_, _| {}, &mut |_, _| {});
        assert!(r.is_err(), "{r:?}");
        let one = MergedSource::new(1, vec![PathBuf::from("/nowhere/a.CR2")], hdr::Settings { reference: Some(5), ..Default::default() }, "f1-0000000000000000".into(), "f1-0000000000000001".into());
        assert_eq!(one.reference(), PathBuf::from("/nowhere/a.CR2"));
        assert!(compute(&one, &cancel, &|_, _| {}, &mut |_, _| {}).is_err());
        let (_, why) = usable(&[], Ok(hdr::Settings::default()));
        assert!(why.is_some_and(|w| w.starts_with(UNREADABLE)));
    }

    /// Settings that do not read are an error – never the default settings (storing those would overwrite the areas and
    /// switches of the recipe); a source with such a recipe computes nothing.
    #[test]
    fn unreadable_settings_are_not_defaults() {
        assert!(parse_settings(r#"{"reference":1}"#).is_ok());
        assert!(parse_settings("{}").is_ok());
        for bad in ["", "{", r#"{"ghost":"a level of a newer version"}"#, r#"{"regions":7}"#] {
            let e = parse_settings(bad).unwrap_err();
            assert!(e.starts_with(UNREADABLE), "{bad}: {e}");
        }
        let paths = vec![PathBuf::from("/nowhere/a.CR2"), PathBuf::from("/nowhere/b.CR2")];
        let (settings, why) = usable(&paths, parse_settings("{"));
        assert!(why.is_some());
        let src = MergedSource { file: 1, sources: paths, settings, key: "f1-0000000000000000".into(), base_key: "f1-0000000000000001".into(), unreadable: why };
        let cancel = AtomicBool::new(false);
        let e = compute(&src, &cancel, &|_, _| {}, &mut |_, _| {}).unwrap_err();
        assert!(e.starts_with(UNREADABLE), "{e}");
    }

    /// A base with one frame brighter than the merge: the area is pasted from that frame, the rest stays.
    #[test]
    fn preview_pastes_areas_from_the_frames() {
        let (w, h) = (100, 50);
        let noise = raw::noise::NoiseModel { a: [1e-4; 3], b: [1e-7; 3] };
        let base = hdr::Merged { px: vec![[0.2; 3]; w * h], w, h, noise };
        let frames = vec![Arc::new(hdr::Merged { px: vec![[0.2; 3]; w * h], w, h, noise }), Arc::new(hdr::Merged { px: vec![[0.5; 3]; w * h], w, h, noise })];
        let geo = hdr::regions::Geometry { reference: 0, warps: vec![Default::default(); 2], k: vec![1.0, 1.0], wb: [1.0; 3], crop: [0, 0, w, h], w, h };
        let meta = BaseMeta { summary: hdr::Summary { reference: 0, ev: vec![0.0, 0.0], shift: vec![0.0, 0.0], warnings: Vec::new() }, headroom: 1.0, algo: 2, pattern: [[1; 6]; 6], geometry: geo, motion: false, preview: [w, h] };
        let area = hdr::regions::Area { frame: 1, strokes: vec![hdr::regions::AreaStroke { erase: false, radius: 0.1, points: vec![[0.5, 0.5]] }] };
        let s = hdr::Settings { regions: vec![area], ..Default::default() };
        let cancel = AtomicBool::new(false);
        let t = std::time::Instant::now();
        let pv = finish_preview(&meta, &base, &frames, &s, &cancel).unwrap();
        assert!(t.elapsed().as_millis() < 500);
        assert!((pv.px[25 * w + 50][1] - 0.5).abs() < 1e-4);
        assert_eq!(pv.px[0][1], 0.2);
        // without areas: the base itself
        let pv = finish_preview(&meta, &base, &frames, &hdr::Settings::default(), &cancel).unwrap();
        assert!(pv.px.iter().all(|p| p[1] == 0.2));
        assert_eq!(result_meta(&meta, "b", &hdr::Settings { output: hdr::fusion::Output::Fusion, ..Default::default() }).headroom, 1.0);
    }
}
