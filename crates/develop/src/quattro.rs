//! Quattro cameras (dp0/dp1/dp2/dp3 Quattro, sd Quattro, sd Quattro H) and the older
//! generation ("Classic": SD9/SD10/SD14/SD15, DP1/DP2 incl. s/x), both via the
//! preprocessing of x3fuse-core, with our own calibration ([`Calibration::read_quattro`] or
//! [`Calibration::read_classic`]).
//!
//! Architecture:
//! 1. x3fuse-core (Apache-2.0, third_party/x3fuse-core) decodes and preprocesses the file:
//!    TRUE decoding, CAMF type 5, black levels (top layer separately), defective pixels,
//!    AF pixel grid (sd Quattro/H), noise reduction and upsampling of the two lower
//!    layers to the resolution of the top one (Kalpanika method: top layer as
//!    luminance guide). Result via the public editor interface
//!    `prepare_scene_linear`: linear sRGB plus the camera matrix `camera_to_rgb`.
//! 2. We convert back to the normalised camera space with `camera_to_rgb⁻¹` (x = 0 black,
//!    1 = sensor saturation per layer) and store the image as a synthetic raw image
//!    (u16, `ENC_BLACK + x · ENC_SCALE`). Session, preview, sensor views,
//!    measuring spot and layer export thus work unchanged.
//! 3. The calibration (white balance presets, colour modes, ISO) comes from our own
//!    CAMF parsing, for dp Quattro matched against x3fuse-core's port of the SPP colour modes
//!    (`ColorModeCalibration`): counts = ISO · CC(WB) · CMCM(mode) · diag(WB · n2c) · x.
//!    Our output stage (`render_counts`) runs on that with the Quattro tone curve.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;
use x3f::{PropList, RawImage, X3fFile};

use crate::{Calibration, Linear};

/// Black level of the synthetic raw image (room for slightly negative values after
/// noise reduction).
pub const ENC_BLACK: f64 = 2048.0;
/// Code values per saturation unit (≈ 14-bit resolution of the sensor; headroom up to x ≈ 3.9).
pub const ENC_SCALE: f64 = 16384.0;

/// Preprocessed Quattro file: synthetic raw image, calibration, properties.
pub struct Prepared {
    pub raw: Arc<RawImage>,
    pub cal: Arc<Calibration>,
    pub props: PropList,
    /// Classic: result of Sigma's whole chain ([`crate::classic_new`]) as a linear image in the
    /// normalisation of [`crate::linearize`]; `None` where the chain is not available
    pub classic: Option<Arc<Linear>>,
    /// First noise reduction stage of the colour grid with binning possible (sd Quattro / sd Quattro H from capture
    /// ISO 800): the layers without the noise reduction of the colour grid, for the noise reduction after binning
    /// ([`counts`])
    pub unbinned_noise: Option<Arc<Stage1Source>>,
    /// First noise reduction stage: the denoised colour grid (colour stage on the grid, see [`grid_counts`])
    pub grid: Option<Arc<GridColour>>,
    /// High-ISO path: what the noise sliders start from ([`counts_with_sliders`])
    pub sliders: Option<Arc<SliderSource>>,
    /// Full chain of the maker's software ([`full_chain_enabled`]): its inputs, the whole development follows in
    /// [`counts`]
    pub full: Option<Arc<crate::quattro_hi::Inputs>>,
}

/// Are the noise sliders (luminance, colour) adjustable for this photo? Quattro only: from capture ISO 400 on the full
/// chain evaluates their steps ([`counts_with_sliders`]; the middle is the camera's setting). Below that ISO, with the
/// full chain off and for multi-shot files (X3I) the noise reduction is that of the preparation and the sliders have
/// no steps to choose from.
pub fn noise_sliders_adjustable(cal: &Calibration, path: &Path) -> bool {
    noise_sliders_rule(cal.model, cal.iso_gain * cal.sensor_iso, full_chain_enabled(), x3f::x3i::is_x3i(path))
}

/// The rule of [`noise_sliders_adjustable`]: `full_chain` = the full chain is on, `multi_shot` = an X3I file.
pub fn noise_sliders_rule(model: crate::calib::Model, capture_iso: f64, full_chain: bool, multi_shot: bool) -> bool {
    model.is_quattro() && full_chain && capture_iso >= STAGE1_MAX_ISO && !multi_shot
}

/// Default of the full chain of the maker's software for capture ISO 400 and above (on: measured closer to the maker's
/// output in colour, noise, blotches and edges on all test files; the previous high-ISO path stays reachable).
pub const FULL_CHAIN_DEFAULT: bool = true;

/// Setting of the full chain: 0 = default ([`FULL_CHAIN_DEFAULT`]), 1 = on, 2 = off.
static FULL_CHAIN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Switches the full chain on or off for this program (`None`: back to the default).
pub fn set_full_chain(on: Option<bool>) {
    FULL_CHAIN.store(
        match on {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Whether Quattro files from capture ISO 400 on run through the full chain of the maker's software
/// ([`crate::quattro_hi`]) instead of the previous high-ISO path. `GRAINEO_QUATTRO_FULL=1`/`0` overrides the setting.
pub fn full_chain_enabled() -> bool {
    if let Ok(v) = std::env::var("GRAINEO_QUATTRO_FULL") {
        return v != "0";
    }
    match FULL_CHAIN.load(std::sync::atomic::Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => FULL_CHAIN_DEFAULT,
    }
}

/// High-ISO path: the states before the noise reduction the sliders move towards ([`counts_with_sliders`]).
pub struct SliderSource {
    /// the colour grid before the first stage (normalised layer values, the size of [`GridColour`])
    pub grid: Vec<crate::quattro_nr::Px>,
    /// the full-resolution top layer before its brightness smoothing (repaired and sharpened like the result),
    /// encoded like the layers of [`Prepared::raw`]
    pub top: Vec<u16>,
    /// layer value of grey per unit of the top layer (normalised units)
    pub neutral: [f32; 3],
    /// normalised layer value → units of the stage
    pub unit: [f32; 3],
}

/// Layers before the first noise reduction stage of the colour grid and its parameters (binned path).
pub struct Stage1Source {
    pub raw: RawImage,
    pub params: crate::quattro_nr::Params,
    /// normalised layer value → units of the stage (counts above black / (SensorISO/100))
    pub unit: [f32; 3],
    /// high-ISO path (`raw` holds the denoised layers): the second pass of the first stage on the binned grid keeps
    /// the brightness of the denoised layers; layer value of grey per unit of the top layer (normalised units)
    pub luma_neutral: Option<[f32; 3]>,
    /// high-ISO path: base sharpening of the binned top layer ([`binned_top`])
    pub sharpen: f32,
    /// colour slider steps above the middle: further colour smoothing of the binned grid ([`counts_with_sliders`])
    pub chroma_step: usize,
}

/// Quattro file? (X3F header version 4.0 or later; reads only the first bytes)
pub fn is_quattro(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    // an X3I (SFD bracket of the sd Quattro H) counts through its first frame
    if x3f::x3i::is_x3i(path) {
        return std::fs::File::open(path)
            .and_then(|mut f| {
                use std::io::Seek;
                f.seek(std::io::SeekFrom::Start(x3f::x3i::HEADER_LEN))?;
                f.read_exact(&mut head)
            })
            .is_ok()
            && &head[0..4] == b"FOVb"
            && u32::from_le_bytes([head[4], head[5], head[6], head[7]]) >= 0x4_0000;
    }
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && &head[0..4] == b"FOVb"
        && u32::from_le_bytes([head[4], head[5], head[6], head[7]]) >= 0x4_0000
}

/// Classic file (RAW type 3: TRUE format 30 or Huffman 5/6; reads only header and directory)?
pub fn is_classic(path: &Path) -> bool {
    !is_quattro(path) && x3f::read_raw_header(path).is_some_and(|h| h.is_classic_raw())
}

/// Is the file preprocessed via x3fuse-core (Quattro or Classic)?
pub fn is_prepared(path: &Path) -> bool {
    is_quattro(path) || is_classic(path)
}

/// [`is_classic`], remembered per path (asked on the UI thread when a photo opens). Only files that could be read
/// are remembered.
pub fn is_classic_cached(path: &Path) -> bool {
    static KNOWN: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, c)) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, _)| q == path) {
        return c;
    }
    let c = is_classic(path);
    if path.is_file() {
        let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        if k.len() >= 256 {
            k.drain(..128);
        }
        k.push((path.to_path_buf(), c));
    }
    c
}

/// [`is_prepared`], remembered per path (the base key asks for it often, also on the UI thread). Only files that
/// could be read are remembered (an unreachable file is asked again later).
pub fn is_prepared_cached(path: &Path) -> bool {
    static KNOWN: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, p)) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, _)| q == path) {
        return p;
    }
    let p = is_prepared(path);
    if path.is_file() {
        let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        if k.len() >= 256 {
            k.drain(..128);
        }
        k.push((path.to_path_buf(), p));
    }
    p
}

type Done = Result<Arc<Prepared>, String>;

/// One cached preprocessing: computed once and shared – opening and "SPP base" run in parallel in the app.
type Slot = pools::Shared<Done>;

/// Most recently preprocessed files (opening and "SPP base" run in parallel in the app and
/// should run the preprocessing only once), full or quick ([`prepared_quick`]).
fn cache() -> &'static Mutex<Vec<((PathBuf, bool), Arc<Slot>)>> {
    static CACHE: OnceLock<Mutex<Vec<((PathBuf, bool), Arc<Slot>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Is the preprocessing of a file running right now (full or quick)? For measurements.
pub fn is_preparing(path: &Path) -> bool {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    cache().lock().unwrap().iter().any(|((p, ..), slot)| *p == file && slot.is_running())
}

/// Drop the cached preprocessing of a file (measuring tools: time the preprocessing again).
pub fn forget(path: &Path) {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    cache().lock().unwrap().retain(|((p, _), _)| *p != file);
    scene_cache().lock().unwrap().retain(|(p, _)| *p != file);
}

/// State of the own highlight rebuild ([`crate::highlight_rebuild`]) for a photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightState {
    /// Nothing rebuildable (or the rebuild does not run for this file).
    None,
    /// Rebuilt; the highlight sliders act only when `sliders` (the colour of the grid path, not the full chain of
    /// the maker's software at high ISO, not Classic).
    Rebuilt { sliders: bool },
}

impl HighlightState {
    fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Rebuilt { sliders: false } => 1,
            Self::Rebuilt { sliders: true } => 2,
        }
    }

    fn from_code(c: u8) -> Self {
        match c {
            0 => Self::None,
            1 => Self::Rebuilt { sliders: false },
            _ => Self::Rebuilt { sliders: true },
        }
    }

    /// The state of a finished preparation.
    pub fn of(p: &Prepared) -> Self {
        match p.grid.as_ref().and_then(|g| g.highlight.as_ref()) {
            Some(_) => Self::Rebuilt { sliders: p.classic.is_none() && p.full.is_none() },
            None => Self::None,
        }
    }
}

/// States learned from finished preparations of this session (the preparation itself may have left the cache).
fn highlight_memo() -> &'static Mutex<Vec<(PathBuf, String, HighlightState)>> {
    static MEMO: Mutex<Vec<(PathBuf, String, HighlightState)>> = Mutex::new(Vec::new());
    &MEMO
}

fn remember_highlights(file: &Path, p: &Prepared) {
    if !crate::highlight_rebuild::enabled_quattro() {
        return;
    }
    let st = HighlightState::of(p);
    let tag = crate::highlight_rebuild::tag();
    {
        let mut m = highlight_memo().lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|(q, ..)| q != file);
        if m.len() >= 256 {
            m.drain(..128);
        }
        m.push((file.to_path_buf(), tag, st));
    }
    crate::diskcache::store_highlight_note(file, st.code());
}

/// Whether the photo `path` holds rebuilt highlights ([`crate::highlight_rebuild`]) and whether the sliders act:
/// from the finished preparation in memory, else from this session's memory of an earlier one, else from the note
/// next to the disk cache (a base read from the cache runs no preparation). `None` while unknown (never computes or
/// waits); [`is_preparing`] tells whether it is being computed.
pub fn highlight_state(path: &Path) -> Option<HighlightState> {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let slot = cache().lock().unwrap().iter().find(|((p, quick), s)| *p == file && !*quick && s.is_done()).map(|(_, s)| s.clone());
    if let Some(slot) = slot {
        if let Ok(p) = slot.get_or_compute(|| Err(String::new())) {
            return Some(HighlightState::of(&p));
        }
    }
    let tag = crate::highlight_rebuild::tag();
    if let Some(st) = highlight_memo().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, t, _)| *q == file && *t == tag).map(|e| e.2) {
        return Some(st);
    }
    let st = HighlightState::from_code(crate::diskcache::highlight_note(&file)?);
    let mut m = highlight_memo().lock().unwrap_or_else(|e| e.into_inner());
    m.push((file, tag, st));
    Some(st)
}

/// Whether the finished preparation of `path` holds rebuilt highlights ([`highlight_state`]).
pub fn highlights_rebuilt(path: &Path) -> Option<bool> {
    highlight_state(path).map(|s| s != HighlightState::None)
}

/// Drop every cached preprocessing (a switch that changes the preparation, e.g. the highlight rebuild, was turned).
pub fn forget_all() {
    cache().lock().unwrap().clear();
}

/// Preprocessed file (≈ 3–6 s on the first call, afterwards from the cache): highlights without reconstruction, they
/// glide to neutral ([`own_highlights`]); the first noise reduction stage on the colour grid ([`crate::quattro_nr`],
/// [`crate::quattro_noise`]).
///
/// Computed once per file and shared ([`pools::Shared`]): while another thread computes the same file, a plain
/// thread or a worker of another pool waits for its result; a worker of the very pool the computation runs in never
/// waits (rule of the app's threads, see `pools`) and computes the file itself. Opening never waits behind the
/// background: if an offline copy (background pool, intent idle) computes the file, a caller in front waits at most
/// [`pools::BACKGROUND_GRACE`] and then computes it itself (the first result to finish is kept). The preprocessing
/// works in parallel in the caller's pool; no lock is held while computing.
pub fn prepared(path: &Path) -> Result<Arc<Prepared>, String> {
    prepared_full(path, false)
}

/// Quick preparation for the first picture: like [`prepared`], but without the first noise reduction stage
/// (the layers as x3fuse-core prepares them for the stage, i.e. without its colour noise reduction where the stage
/// applies). x3fuse-core runs only once for both: the full preparation takes over its result
/// ([`shared_scene`]). Where the stage does not apply, both are the same cache entry.
pub fn prepared_quick(path: &Path) -> Result<Arc<Prepared>, String> {
    prepared_full(path, true)
}

fn prepared_full(path: &Path, quick: bool) -> Result<Arc<Prepared>, String> {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // where the first stage does not apply (capture ISO, Classic), the quick and the full preparation are the same:
    // one cache entry for both (the quick opening and the Sigma base then share it)
    let quick = quick && stage1_applies(&file);
    let key = (file.clone(), quick);
    let slot = {
        let mut c = cache().lock().unwrap();
        if let Some(i) = c.iter().position(|(p, _)| *p == key) {
            let e = c.remove(i);
            let slot = e.1.clone();
            c.push(e);
            slot
        } else {
            let slot = Arc::new(Slot::default());
            c.push((key.clone(), slot.clone()));
            // Keep at most three images (current, preloaded, previous).
            while c.len() > 3 {
                c.remove(0);
            }
            slot
        }
    };
    let r = slot.get_or_compute(|| {
        let r = prepare(&file, quick).map(Arc::new);
        if let (false, Ok(p)) = (quick, &r) {
            remember_highlights(&file, p);
        }
        r
    });
    if r.is_err() {
        cache().lock().unwrap().retain(|(p, _)| *p != key);
    }
    r
}

/// Does the first noise reduction stage of the colour grid run for this file (Quattro below the
/// capture ISO limit [`STAGE1_MAX_ISO`], or from [`STAGE1_RAW_MIN_ISO`] on with the grid from the stored sensor values)?
/// Reads only the camera data; remembered per path.
pub fn stage1_applies(path: &Path) -> bool {
    static KNOWN: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, a)) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, _)| q == path) {
        return a;
    }
    let applies = (|| {
        let file = X3fFile::open(path).ok()?;
        if file.is_classic() || !is_quattro(path) {
            return Some(false);
        }
        let camf = file.camf().ok()?;
        Some(stage1_enabled(&camf) || mid_luma_applies(&camf))
    })();
    let Some(a) = applies else { return true };
    let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if k.len() >= 256 {
        k.drain(..128);
    }
    k.push((path.to_path_buf(), a));
    a
}

/// The first stage for a Quattro file with these camera data: below capture ISO [`STAGE1_MAX_ISO`] on the grid of the
/// prepared layers, from there on on the grid from the stored sensor values (400 to 3200 the high-ISO path,
/// [`high_path`]). `GRAINEO_QUATTRO_HIGHPATH=0` with `GRAINEO_QUATTRO_STAGE1_HIGH=1`: the stage on the stored values from
/// ISO 400 on without the high-ISO path (measurements), `GRAINEO_QUATTRO_RAWGRID=0`: the stage stays below ISO 400 only.
fn stage1_enabled(camf: &x3f::Camf) -> bool {
    let capture_iso = crate::calib::quattro_iso_gain(camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0);
    let raw_grid = raw_grid_enabled();
    crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_STAGE1").map_or(true, |v| v != "0")
        && (capture_iso < STAGE1_MAX_ISO
            || (raw_grid && capture_iso >= STAGE1_RAW_MIN_ISO)
            || (raw_grid && high_path(capture_iso))
            || crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_STAGE1_HIGH").is_ok_and(|v| v == "1"))
}

/// From capture ISO 400 on (binned sd output: below [`STAGE1_RAW_MIN_ISO`]) the high-ISO path works on the stored sensor values: outliers
/// repaired ([`crate::quattro_speckle`]), the first stage on their grid, the full-resolution top layer (the fine
/// detail) from the repaired stored values with its brightness smoothing and the base sharpening afterwards
/// (`GRAINEO_QUATTRO_HIGHPATH=0`: the previous way, measurements). From ISO 6400 on the top layer of x3fuse-core's
/// preparation stays the detail (the brightness smoothing of this path left coarse mottling there, measured).
fn high_path(capture_iso: f64) -> bool {
    // `GRAINEO_QUATTRO_HIGH6400=0`: from ISO 6400 on the previous way (grid from the stored values and stage only)
    let top = if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_HIGH6400").is_ok_and(|v| v == "0") { STAGE1_RAW_MIN_ISO } else { f64::INFINITY };
    (STAGE1_MAX_ISO..top).contains(&capture_iso) && crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_HIGHPATH").map_or(true, |v| v != "0")
}

type SceneDone = Result<Arc<x3f_core::SceneLinearImage>, String>;

/// x3fuse-core's result for the first noise reduction stage, shared by the quick and the full preparation of a file
/// (the most recent file only; the full preparation removes its entry once it has taken it over).
fn scene_cache() -> &'static Mutex<Vec<(PathBuf, Arc<pools::Shared<SceneDone>>)>> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Arc<pools::Shared<SceneDone>>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// x3fuse-core's preparation of `path`, computed once for the quick and the full preparation ([`prepared_quick`]);
/// `take`: remove the shared entry afterwards (the full preparation).
fn shared_scene(path: &Path, take: bool, compute: impl FnOnce() -> SceneDone) -> SceneDone {
    let key = path.to_path_buf();
    let slot = {
        let mut c = scene_cache().lock().unwrap();
        if let Some((_, s)) = c.iter().find(|(k, _)| *k == key) {
            s.clone()
        } else {
            let s = Arc::new(pools::Shared::default());
            c.push((key.clone(), s.clone()));
            while c.len() > 1 {
                c.remove(0);
            }
            s
        }
    };
    let r = slot.get_or_compute(compute);
    if take || r.is_err() {
        scene_cache().lock().unwrap().retain(|(k, s)| !(*k == key && Arc::ptr_eq(s, &slot)));
    }
    r
}

fn prepare(path: &Path, quick: bool) -> Result<Prepared, String> {
    let t = std::time::Instant::now();
    let file = X3fFile::open(path).map_err(|e| e.to_string())?;
    let camf = file.camf().map_err(|e| e.to_string())?;
    let sd_quattro_h = camf.matrix("CAMERAID").ok().and_then(|m| m.as_f64().first().copied()) == Some(41.0);
    // the first stage below capture ISO 400 on the prepared layers, from there on with the grid from the stored sensor
    // values (see `stage1_enabled`, 400 to 3200 the high-ISO path)
    let stage1 = !file.is_classic() && stage1_enabled(&camf);
    let capture_iso0 = crate::calib::quattro_iso_gain(&camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0);
    // the high-ISO path on the stored sensor values (see `high_path`); x3fuse-core's preparation then only serves as
    // the reference of the defect repair and needs no noise reduction of its own
    // binned output of sd Quattro / sd Quattro H (test program and export tools): keeps its previous treatment (fitted
    // on the maker's binned output); from ISO 6400 on the previous way altogether
    let binned_out = camf.matrix("CAMERAID").ok().and_then(|m| m.as_f64().first().copied()).is_some_and(|id| id == 40.0 || id == 41.0)
        && binning_enabled()
        && binning_for_iso(capture_iso0) > 1;
    let hi = stage1 && raw_grid_enabled() && high_path(capture_iso0) && !(binned_out && capture_iso0 >= STAGE1_RAW_MIN_ISO) && !x3f::x3i::is_x3i(path);
    // dp Quattro and sd Quattro (sd Quattro H with the first stage): the base sharpening is shared with the local
    // contrast of the colour stage below ISO 800 (`defer_sharpen`)
    let spp_neutralize = !file.is_classic() && (!sd_quattro_h || stage1) && crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_HL").map_or(true, |v| v != "rec");
    let mut opts = x3f_core::SceneLinearOptions {
        // No reconstruction of blown layers here: highlights glide to neutral below (`own_highlights`)
        highlight_recovery: false,
        apply_sgain: crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_SGAIN").ok().map(|v| v == "1"),
        denoise_intensity: crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_NR").ok().and_then(|v| v.parse().ok()).unwrap_or(10),
        // setting "Grafikkarte verwenden" off: the noise reduction of the preparation on the processor
        denoise_backend: if pools::gpu_allowed() { x3f_core::DenoiseBackend::Auto } else { x3f_core::DenoiseBackend::Cpu },
        ..Default::default()
    };
    // All Quattro: noise strength with the digital factor (sd Quattro H as well since part 6: at ISO 6400 without
    // it the luminance noise was 2.5 × that of the reference; `GRAINEO_QUATTRO_HNR=0` = previous preprocessing).
    if !file.is_classic() && (!sd_quattro_h || crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_HNR").map_or(true, |v| v != "0")) {
        opts.denoise_options = quattro_denoise_options(crate::calib::quattro_iso_gain(&camf));
    }
    // the colour noise of the grid is removed by the first stage below ; x3fuse-core
    // keeps only its luminance pass on the full-resolution top layer (from a digital factor > 1)
    if stage1 {
        opts.denoise_options.chroma = false;
        // without x3fuse-core's colour noise reduction the noise of dark regions reaches below the black of its
        // integer buffers at high ISO; a higher black keeps it from being clipped (which would brighten the darks)
        opts.denoise_options.min_intermediate_bias = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_BIAS").ok().and_then(|v| v.parse().ok()).unwrap_or(STAGE1_MIN_BIAS);
        if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_STAGE1_LUMA").is_ok_and(|v| v == "0") || hi {
            opts.denoise_options.luminance = false;
        }
    }
    // X3I (SFD bracket): the frames merged on the sensor grid; the preparation then works on the base exposure with
    // the merged values in place of its own (x3fuse-core's scene-linear merge as fallback)
    let grid_merge = if x3f::x3i::is_x3i(path) && crate::sfd::grid_merge() {
        match crate::sfd::merged_x3i(path) {
            Ok(r) => Some(r),
            Err(e) if e == crate::sfd::CANCELLED => return Err(e),
            Err(e) => {
                eprintln!("X3I grid merge failed, x3fuse-core merge instead: {e}");
                crate::sfd::note_fallback(path);
                None
            }
        }
    } else {
        None
    };
    if grid_merge.is_some() {
        opts.x3i_source = x3f_core::X3iSource::MiddleExposure;
    }
    // brightness smoothing on the preparation (capture ISO 400 to below 6400): after the first picture, like the stage
    let mid_luma = !file.is_classic() && !stage1 && mid_luma_applies(&camf);
    let run = || x3f_core::prepare_scene_linear(path, &opts, &AtomicBool::new(false), |_| {}).map(Arc::new).map_err(|e| e.to_string());
    let scene = if stage1 || mid_luma { shared_scene(path, !quick, run)? } else { run()? };
    // the quick preparation shows the layers before the first stage
    let stage1 = stage1 && !quick;
    let t_scene = t.elapsed();
    // x3fuse-core's scene-linear X3I merge (not the grid merge)
    let x3i = x3f::x3i::is_x3i(path) && grid_merge.is_none();
    // brightest scene value without isolated outliers (hot pixels, noise peaks of the merge): the 99.99th percentile
    // of the largest channel of every 7th pixel
    let scene_max = if x3i {
        let mut v: Vec<f32> = scene.data.chunks_exact(3).step_by(7).map(|p| p[0].max(p[1]).max(p[2])).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(((v.len() as f64 * 0.9999) as usize).min(v.len().saturating_sub(1))).copied().unwrap_or(1.0)
    } else {
        0.0
    };
    let mut scene_data = scene.data.clone();
    let props = file.properties().map_err(|e| e.to_string())?;
    // test bench: merged sensor values of a bracket in place of this frame's (`crate::sfd`)
    let replacement = grid_merge.clone().or_else(|| crate::sfd::replacement(path)).filter(|r| {
        let fits = r.tw == scene.width as usize && r.th == scene.height as usize;
        if !fits {
            eprintln!("replacement {}x{} does not fit the prepared {}x{}", r.tw, r.th, scene.width, scene.height);
        }
        fits
    });
    // A merged bracket holds values far below one exposure's (the merge is in units of its shortest exposure, scaled
    // down once more). Two factors bring it to the range the following steps are made for:
    // - `merge_level`: merged units → units of the base exposure. The first noise reduction stage runs at this level
    //   (its noise model is that of a single exposure; on the unscaled values it took all of the image for dark and
    //   noisy and smoothed the grid-scale brightness away).
    // - `store`: factor of the stored layers (16-bit codes), as large as the brightest merged value allows, at most
    //   `merge_level` (unscaled, half of the image lay within 13 to 26 code steps). The calibration gains carry the
    //   rest (`merge_level / store`).
    let merge_level = replacement.as_ref().and_then(|r| r.base_gain).filter(|g| g.is_finite() && *g > 0.0).unwrap_or(1.0);
    let mut store = 1.0f32;
    if let Some(r) = &replacement {
        let unit = stage1_params(&camf, &props, sd_quattro_h).unit;
        let rgb = crate::matrix::Mat3(scene.white_balance.camera_to_rgb).to_f32();
        if merge_level > 1.0 {
            let grid_max = r.grid.par_iter().map(|p| (p[0] / unit[0]).max(p[1] / unit[1]).max(p[2] / unit[2])).reduce(|| 0f32, f32::max);
            let max = r.top.par_iter().map(|v| v / unit[2]).reduce(|| 0f32, f32::max).max(grid_max);
            let fit = ((65535.0 - ENC_BLACK) / ENC_SCALE) as f32 * MERGE_STORE_HEADROOM / max.max(1e-6);
            store = merge_level.min(fit).max(1.0);
        }
        let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [scene.width as usize, scene.height as usize]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
        let neutral: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32);
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("merged bracket: level {merge_level:.3}, stored × {store:.3}, grey per top unit {neutral:?}");
        }
        scene_data = replaced_scene(r, unit.map(|u| u / store), &rgb, neutral);
    }
    // merged bracket: developed like its base exposure (calibration gains times the factor from the stored values to
    // the base exposure's), or with the bracket tone (`crate::sfd::bracket_tone_start`)
    let base_gain = replacement.as_ref().and_then(|r| r.base_gain).filter(|_| !crate::sfd::bracket_tone_start()).map(|g| g / store);
    // merged bracket with the bracket tone: its level from the merged values (scene units per count follow with the calibration)
    let bracket_probe = (grid_merge.is_some() && replacement.is_some() && base_gain.is_none()).then(|| {
        let t = crate::sfd::BracketTone::analyse(&scene_data, scene.width as usize, scene.height as usize, crate::sfd::SCENE_PER_PREPARED / store as f64, crate::sfd::BRACKET_WHITE, 0.0);
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            let mut v: Vec<f32> = scene_data.chunks_exact(3).step_by(97).map(|p| (p[0] + p[1] + p[2]) / 3.0).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            eprintln!("bracket tone probe {t:?}, channel mean p50 {} p90 {}", v[v.len() / 2], v[v.len() * 9 / 10]);
        }
        t
    });
    let iso = props.get("ISO").and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0);
    let amount = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_SHARPEN").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| capture_sharpen_amount(iso));
    // dp Quattro and sd Quattro below ISO 800: half the base sharpening here, the plain colour stage sharpens its
    // counts with the other half (`pending_sharpen`)
    let defer_sharpen = spp_neutralize && lce_applies(iso);
    let now = if defer_sharpen { amount * DEFERRED_SHARPEN } else { amount };
    // high-ISO path: the base sharpening acts on the repaired and smoothed top layer below
    if now > 0.0 && !hi {
        capture_sharpen(&mut scene_data, scene.width as usize, scene.height as usize, now as f32);
    }
    let cam_to_rgb = crate::matrix::Mat3(scene.white_balance.camera_to_rgb);
    let inv = cam_to_rgb.inverse().ok_or("Kameramatrix nicht invertierbar")?.to_f32();
    let (w, h) = (scene.width as usize, scene.height as usize);
    let n = w * h;
    // Back to the normalised camera space (planar like RawImage).
    let mut xs: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; n]);
    {
        let [p0, p1, p2] = &mut xs;
        p0.par_chunks_mut(w)
            .zip(p1.par_chunks_mut(w))
            .zip(p2.par_chunks_mut(w))
            .zip(scene_data.par_chunks(w * 3))
            .for_each(|(((r0, r1), r2), src)| {
                for (x, s) in src.chunks_exact(3).enumerate() {
                    let m = &inv;
                    r0[x] = m[0] * s[0] + m[1] * s[1] + m[2] * s[2];
                    r1[x] = m[3] * s[0] + m[4] * s[1] + m[5] * s[2];
                    r2[x] = m[6] * s[0] + m[7] * s[1] + m[8] * s[2];
                }
            });
    }
    let encode = |xs: &[Vec<f32>; 3]| -> [Vec<u16>; 3] {
        let (b, k) = (ENC_BLACK as f32, ENC_SCALE as f32);
        std::array::from_fn(|c| xs[c].par_iter().map(|&v| (b + v * k).round().clamp(0.0, 65535.0) as u16).collect())
    };
    // capture ISO 400 to below 6400 (x3fuse-core's preparation without the first stage): the brightness noise of the
    // top layer smoothed, as a neutral change of all three layers
    if mid_luma && !quick {
        let s1 = stage1_params(&camf, &props, sd_quattro_h);
        let capture_iso = (crate::calib::quattro_iso_gain(&camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0)) as f32;
        let u = s1.unit[2];
        let mut t: Vec<f32> = xs[2].par_iter().map(|v| v * u).collect();
        crate::quattro_nr::luma_smooth(&mut t, w, h, capture_iso);
        let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [w, h]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
        let neutral: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32);
        let [p0, p1, p2] = &mut xs;
        p0.par_iter_mut().zip(p1.par_iter_mut()).zip(p2.par_iter_mut()).zip(t.par_iter()).for_each(|(((b, m), top), &ts)| {
            let d = ts / u - *top;
            *b += d * neutral[0];
            *m += d * neutral[1];
            *top += d * neutral[2];
        });
    }
    // Level at which the top layer saturates where that is below the top of the stored range (sd Quattro H with its
    // digital gain per layer): cells with a clipped top pixel lose their colour below (`neutral_clipped_cells`)
    let top_clip = (!file.is_classic() && replacement.is_none() && !x3i && crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_CLIPCELLS").map_or(true, |v| v != "0"))
        .then(|| saturation_of_values(&xs[2], 1.0))
        .flatten();
    // the saturation of all layers before that treatment (it removes the pile-up the level is read from)
    let sat_before = top_clip.map(|t| [saturation_of_values(&xs[0], 1.0).unwrap_or(1.0), saturation_of_values(&xs[1], 1.0).unwrap_or(1.0), t]);
    // grids with peaks far above their surroundings are enlarged without over- and undershoot
    let peaks = replacement.is_some() || top_clip.is_some();
    // own rebuild of a clipped top layer from the valid lower layers (opt-in, `crate::highlight_rebuild`); where it
    // ran, the cells with a clipped top pixel keep their rebuilt colour (no neutral cells)
    let t_hl = std::time::Instant::now();
    let mut sat_before = sat_before;
    let rebuilt = (!file.is_classic() && replacement.is_none() && !x3i && crate::highlight_rebuild::enabled_quattro())
        .then(|| {
            let sat: [f32; 3] = std::array::from_fn(|c| saturation_of_values(&xs[c], 1.0).unwrap_or(1.0));
            if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                eprintln!("Quattro highlight rebuild: layer saturation {sat:?}");
            }
            let out = crate::highlight_rebuild::rebuild_layer(&mut xs, w, h, sat, 2, crate::highlight_rebuild::variant());
            // the rebuilt layer no longer piles up at its saturation: the white point of the later highlight
            // treatment is read here (as without the rebuild), so nothing outside the rebuilt areas changes
            if out.is_some() && sat_before.is_none() {
                sat_before = Some(sat);
            }
            out
        })
        .flatten();
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Quattro highlight rebuild: {:?}, {:.2?}", rebuilt.as_ref().map(|o| &o.stats), t_hl.elapsed());
    }
    // weight of the rebuilt areas per grid cell (largest of its 2 × 2 pixels), for the highlight sliders
    let hl_cells: Option<Arc<Vec<f32>>> = rebuilt.as_ref().map(|o| {
        let (gw, gh) = (w / 2, h / 2);
        let mut c = vec![0f32; gw * gh];
        for &(i, wt) in &o.weight {
            let (x, y) = ((i as usize % w) / 2, (i as usize / w) / 2);
            if x < gw && y < gh {
                let k = y * gw + x;
                c[k] = c[k].max(wt);
            }
        }
        Arc::new(c)
    });
    let top_clip = top_clip.filter(|_| rebuilt.is_none());
    let mut unbinned_noise = None;
    // high-ISO path, sd Quattro / sd Quattro H binned: the first stage runs on the full grid (like unbinned, so no
    // single outlier of the stored values survives as a colour point), then a second pass on the binned grid of the
    // denoised layers removes the colour blotches larger than the full grid's windows
    let mut hi_binned = None;
    let mut grid_colour = None;
    let mut sliders = None;
    let mut full_source: Option<Arc<crate::quattro_hi::Inputs>> = None;
    if stage1 {
        let t1 = std::time::Instant::now();
        let s1 = stage1_params(&camf, &props, sd_quattro_h);
        let capture_iso = (crate::calib::quattro_iso_gain(&camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0)) as f32;
        let params = crate::quattro_nr::Params { gains: s1.gains, capture_iso, binned: false, bin: 1, var_scale: if hi { crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_S1_VAR").ok().and_then(|v| v.parse().ok()).unwrap_or(HIGH_STAGE1_VAR) } else { 1.0 }, step: crate::quattro_nr::DEFAULT_STEP, white: s1.white };
        let (gw, gh, mut grid) = crate::quattro_nr::grid_from_layers([&xs[0], &xs[1], &xs[2]], w, h);
        let t_grid = t1.elapsed();
        for p in grid.iter_mut() {
            for c in 0..3 {
                p[c] *= s1.unit[c];
            }
        }
        // the grid of the prepared layers: reference for the defect repair and base of the top-layer detail
        let prepared_grid = grid.clone();
        let mut clock = crate::quattro_hi::Clock::start();
        // highlight-pixel lattice of the dp Quattro (positions checked: whole numbers, spacing limited)
        let lattice = {
            let values = |n: &str| camf.matrix(n).ok().map(|m| m.as_f64()).unwrap_or_default();
            crate::quattro_hi::lattice::Lattice::from_camera(&values("HighlightPixelsInfo"), &values("ActiveImageArea"))
        };
        // from capture ISO 400 on: the grid from the stored sensor values (matches Sigma's output)
        let mut raw = if let Some(r) = replacement.as_ref().filter(|r| r.gw == gw && r.gh == gh) {
            // merged bracket: its grid as it is (defects were handled per frame), in the stored scale
            Some(crate::quattro_grid::RawGrid { grid: r.grid.par_iter().map(|p| p.map(|v| v * store)).collect(), gw, gh, top: r.top.par_iter().map(|v| v * store).collect(), tw: w, th: h })
        } else {
            let raw = (capture_iso as f64 >= STAGE1_MAX_ISO && raw_grid_enabled())
                .then(|| crate::quattro_grid::read(path, s1.offset, s1.raw_unit))
                .and_then(|r| r.map_err(|e| eprintln!("Quattro grid from the sensor values: {e}")).ok())
                .filter(|r| r.gw == gw && r.gh == gh && r.tw == w && r.th == h);
            let chain = stage1 && full_chain_enabled() && !x3f::x3i::is_x3i(path) && (hi || binned_out);
            raw.map(|mut r| {
                let top_ref: Vec<f32> = xs[2].par_iter().map(|v| v * s1.unit[2]).collect();
                clock.lap("prepare: grid read");
                crate::quattro_grid::repair(&mut r, &prepared_grid, &top_ref);
                clock.lap("prepare: grid repair");
                // the high-ISO chain repairs outliers the maker's way also on the binned output
                // calibrated highlight pixels of the dp Quattro before the outlier repair (they would count as outliers there)
                if let Some(l) = lattice.as_ref().filter(|_| chain && crate::quattro_hi::lattice_replacement() == crate::quattro_hi::lattice::Replacement::Calibrated && r.tw == 2 * r.gw && r.th == 2 * r.gh) {
                    crate::quattro_hi::lattice::calibrate_cells(&mut r.grid, &mut r.top, r.gw, r.gh, l);
                    clock.lap("prepare: highlight cells");
                }
                if chain {
                    speckles(&mut r, outlier_colour_view(&camf).as_ref(), long_exposure(&camf));
                    clock.lap("prepare: outliers");
                } else if hi {
                    speckles(&mut r, outlier_colour_view(&camf).as_ref(), long_exposure(&camf) || binned_out);
                }
                r
            })
        };
        // high-ISO chain: its inputs from the repaired stored values
        if (hi || binned_out) && full_chain_enabled() && replacement.is_none() && !x3f::x3i::is_x3i(path) {
            if let Some(r) = raw.as_ref().filter(|r| r.tw == 2 * r.gw && r.th == 2 * r.gh && r.gw % 4 == 0 && r.gh % 4 == 0) {
                let v = |n: &str| camf.matrix(n).ok().and_then(|m| m.as_f64().first().copied());
                let score = ((0.0866434 * v("SensorTemperature").unwrap_or(25.0)).exp() * v("CaptureShutter").unwrap_or(0.0) / 1000.0) as f32;
                let lattice = lattice.as_ref().filter(|_| crate::quattro_hi::lattice_replacement() != crate::quattro_hi::lattice::Replacement::Off);
                let inputs = crate::quattro_hi::Inputs::assemble(&r.grid, &r.top, r.gw, r.gh, lattice, s1.gains.map(f64::from), capture_iso as f64, score, s1.unit);
                clock.lap("prepare: first stage");
                if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                    eprintln!("Quattro high-ISO chain: grid {}×{}, long exposure {:?}", r.gw, r.gh, inputs.stage.night);
                }
                full_source = Some(Arc::new(inputs));
            }
        }
        // sd Quattro / sd Quattro H: the binned output filters after binning; keep the layers for it
        let sd = camf.matrix("CAMERAID").ok().and_then(|m| m.as_f64().first().copied()).is_some_and(|id| id == 40.0 || id == 41.0);
        let binned_source = sd && binning_enabled() && binning_for_iso(capture_iso as f64) > 1;
        if binned_source && hi {
            hi_binned = Some((crate::quattro_nr::Params { binned: true, bin: binning_for_iso(capture_iso as f64), var_scale: 1.0, ..params }, s1.unit));
        }
        if binned_source && !hi {
            let planes = match &raw {
                // bottom/middle repeated over their 2 × 2 block, top at full resolution: binning averages areas
                Some(r) => {
                    let k = ENC_SCALE as f32;
                    let enc = |v: f32, c: usize| (ENC_BLACK as f32 + v / s1.unit[c] * k).round().clamp(0.0, 65535.0) as u16;
                    std::array::from_fn(|c| {
                        (0..w * h)
                            .into_par_iter()
                            .map(|i| {
                                let (x, y) = (i % w, i / w);
                                if c == 2 { enc(r.top[i], 2) } else { enc(r.grid[(y / 2).min(gh - 1) * gw + (x / 2).min(gw - 1)][c], c) }
                            })
                            .collect()
                    })
                }
                None => encode(&xs),
            };
            unbinned_noise = Some((RawImage { columns: w, rows: h, planes }, crate::quattro_nr::Params { binned: true, bin: binning_for_iso(capture_iso as f64), var_scale: 1.0, ..params }, s1.unit));
        }
        if let Some(r) = &raw {
            grid.clone_from(&r.grid);
        }
        // merged bracket: the top-layer detail stays relative to the merged grid itself. The grid of the prepared
        // layers carries the base sharpening, which at highlights far above their surroundings moves a cell's mean by
        // more than its darker pixels hold: against it they fell below zero (black pixels beside specular highlights)
        let merged_before = raw.as_ref().filter(|_| replacement.is_some()).map(|_| grid.clone());
        // high-ISO path: the repaired stored top layer becomes the fine detail
        // the full chain develops on its own: the previous high-ISO steps are skipped (the layers stay x3fuse-core's)
        let skip_old = full_source.is_some();
        let hi_top = raw.as_mut().filter(|_| hi && !skip_old).map(|r| std::mem::take(&mut r.top));
        // the grid before the first stage for the colour slider (normalised units)
        let mut slider_grid: Option<Vec<crate::quattro_nr::Px>> =
            raw.as_ref().filter(|_| hi).map(|r| r.grid.par_iter().map(|p| std::array::from_fn(|c| p[c] / s1.unit[c])).collect());
        drop(raw);
        if let Some(level) = top_clip.filter(|_| !skip_old) {
            // values of the stage → normalised: grey per top unit in the units of the stage
            let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [w, h]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
            let grey: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32 * s1.unit[c] / s1.unit[2]);
            let n = neutral_clipped_cells(&mut grid, &xs[2], w, gw, gh, level, grey);
            if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                eprintln!("Quattro top layer saturates at {level:.4}: {n} grid cells with clipped top pixels set to neutral");
            }
        }
        if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_STAGE1_ID").is_err() && !skip_old {
            // merged bracket: the stage at the level of the base exposure (see `merge_level`)
            let level = if replacement.is_some() { merge_level / store } else { 1.0 };
            if level != 1.0 {
                grid.par_iter_mut().for_each(|p| *p = p.map(|v| v * level));
            }
            crate::quattro_nr::stage1(&mut grid, gw, gh, &params);
            if level != 1.0 {
                grid.par_iter_mut().for_each(|p| *p = p.map(|v| v / level));
            }
        }
        let back = |g: &mut Vec<crate::quattro_nr::Px>| {
            for p in g.iter_mut() {
                for c in 0..3 {
                    p[c] /= s1.unit[c];
                }
            }
        };
        // the fine detail of the top layer stays relative to the grid of the prepared layers
        let (mut before, mut after) = (merged_before.unwrap_or(prepared_grid), grid);
        let mut hi_luma = false;
        let mut slider_top: Option<Vec<u16>> = None;
        // high-ISO path: the base sharpening on the smoothed top layer; below ISO 800 (where the colour stage would take
        // the other half, which the grid path leaves out) three quarters of the full amount (fitted on the edge
        // gradient against the maker's output at ISO 400)
        let hi_sharpen = (if defer_sharpen { amount * HIGH_SHARPEN_LOW_ISO } else { amount }) as f32;
        if let Some(mut t) = hi_top.filter(|t| t.len() == w * h) {
            hi_luma = true;
            // the top layer before its smoothing for the luminance slider (repaired and sharpened like the result)
            let mut unsmoothed = t.clone();
            sharpen_plane(&mut unsmoothed, w, h, hi_sharpen);
            let (b, k, u) = (ENC_BLACK as f32, ENC_SCALE as f32, s1.unit[2]);
            slider_top = Some(unsmoothed.par_iter().map(|&v| (b + v / u * k).round().clamp(0.0, 65535.0) as u16).collect::<Vec<u16>>());
            drop(unsmoothed);
            high_top(&mut t, w, h, hi_sharpen * if binned_source { 1.0 } else { HIGH_SHARPEN_FACTOR }, binned_source, capture_iso / 100.0);
            let (_, _, tg) = crate::quattro_nr::grid_from_layers([&t, &t, &t], w, h);
            for (b, g) in before.iter_mut().zip(tg.iter()) {
                b[2] = g[2];
            }
            let u = s1.unit[2];
            xs[2].par_iter_mut().zip(t.par_iter()).for_each(|(x, v)| *x = v / u);
        }
        back(&mut before);
        back(&mut after);
        let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [w, h]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
        let neutral: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32);
        // high-ISO path: the brightness of the grid follows the top layer of the high-ISO path (its own smoothing),
        // the first stage keeps the colour: the difference is a neutral change of all three layers
        let lumato = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_LUMATO").unwrap_or_default();
        if hi_luma && lumato != "old" && lumato != "0" && !binned_out {
            // the colour of the grid stays that of the first stage at its own brightness; of the grid-scale brightness
            // of the smoothed top layer a share falling with the capture ISO comes back as detail (the rest is the
            // first stage's top value): the maker's output keeps less of the grid-scale brightness noise at high ISO
            let share = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_TSHARE").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| grid_brightness_share(capture_iso));
            for (b, a) in before.iter_mut().zip(after.iter()) {
                b[2] = a[2] + (1.0 - share) * (b[2] - a[2]);
            }
        } else if hi_luma && lumato != "0" {
            let floor = DETAIL_T_FLOOR / s1.unit[2];
            after.par_iter_mut().zip(before.par_iter()).for_each(|(a, b)| luma_to(a, b[2], neutral, floor));
        }
        if let (Some(grid), Some(top)) = (slider_grid.take().filter(|g| g.len() == after.len()), slider_top.take()) {
            sliders = Some(Arc::new(SliderSource { grid, top, neutral, unit: s1.unit }));
        }
        let t_s1 = t1.elapsed();
        if peaks {
            crate::quattro_nr::layers_from_grid_limited(&mut xs, w, h, &before, &after, gw, gh, neutral);
        } else {
            crate::quattro_nr::layers_from_grid(&mut xs, w, h, &before, &after, gw, gh, neutral);
        }
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("Quattro stage 1 timing: grid {:.0} ms, up to layers {:.0} ms, layers back {:.0} ms", t_grid.as_secs_f64() * 1e3, t_s1.as_secs_f64() * 1e3, t1.elapsed().as_secs_f64() * 1e3);
        }
        if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_GRIDCOLOUR").map_or(true, |v| v != "0") {
            grid_colour = Some(GridColour { width: gw, height: gh, data: after, limited: peaks, highlight: hl_cells.clone() });
        }
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("Quattro stage 1: grid {gw}×{gh}, gains {:?}, unit {:?}, ISO {capture_iso}, {:.2?}", s1.gains, s1.unit, t1.elapsed());
        }
    }
    // without the first stage (the quick preparation): the same treatment of cells with a clipped top pixel
    if let Some(level) = top_clip.filter(|_| !stage1) {
        let (gw, gh, before) = crate::quattro_nr::grid_from_layers([&xs[0], &xs[1], &xs[2]], w, h);
        let mut after = before.clone();
        let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [w, h]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
        let neutral: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32);
        if neutral_clipped_cells(&mut after, &xs[2], w, gw, gh, level, neutral) > 0 {
            crate::quattro_nr::layers_from_grid_limited(&mut xs, w, h, &before, &after, gw, gh, neutral);
        }
    }
    // X3I merge: the range of the merged scene is far above one exposure. The layers are stored scaled down so that
    // the brightest value fits the 16-bit code range (headroom kept, nothing clipped); the calibration gains take the
    // factor back (`x3i_scale`).
    let mut x3i_scale = 1.0f64;
    if x3i {
        let max = xs.iter().map(|p| p.par_iter().copied().reduce(|| f32::MIN, f32::max)).fold(f32::MIN, f32::max);
        let k = ((65535.0 - ENC_BLACK) / ENC_SCALE * 0.97) as f32 / max.max(1e-3);
        if k < 1.0 {
            for p in xs.iter_mut() {
                p.par_iter_mut().for_each(|v| *v *= k);
            }
            x3i_scale = k as f64;
        }
    }
    let mut planes = encode(&xs);
    if let Some((params, unit)) = hi_binned {
        unbinned_noise = Some((RawImage { columns: w, rows: h, planes: planes.clone() }, params, unit));
    }
    drop(xs);
    let mut classic = None;
    let mut layer_sat = [1.0f32; 3];
    let mut cal = if file.is_classic() {
        Calibration::read_classic(&file, &camf, &props, &scene, [w, h])?
    } else {
        Calibration::read_quattro(&file, &camf, &props, &scene, [w, h])?
    };
    if cal.is_classic() {
        // Sigma's output stage works on raw values without the dark offset x3fuse-core subtracts
        if let Some((slope, _)) = classic_raw_map(&file, &camf, &planes, w, h, &cal.wb_name) {
            for m in &mut cal.modes {
                if let Some(c) = m.classic.as_mut() {
                    std::sync::Arc::make_mut(c).raw_reference(slope);
                }
            }
        }
        let f = classic_highlights(&file, &camf, &mut planes, w, h, &cal);
        cal.rescale_layers(f);
        classic = classic_chain(&file, &camf, &props, &mut cal, w, h);
    } else if !x3i {
        // a merged bracket has its own treatment of clipped highlights (`crate::sfd`); its range is not one exposure's
        if grid_merge.is_none() {
            layer_sat = sat_before.unwrap_or_else(|| layer_saturation(&planes));
        }
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("Quattro layer saturation {layer_sat:?}");
        }
        own_highlights(&mut planes, w, &cal, layer_sat);
        // the colour of the first noise reduction stage is formed on the grid: same treatment there where the layers
        // saturate below the top of the stored range (elsewhere the grid stays as measured against the maker's output)
        if let Some(g) = grid_colour.as_mut().filter(|_| layer_sat != [1.0; 3]) {
            own_highlights_grid(&mut g.data, &cal, layer_sat);
        }
    }
    if x3i {
        // start value of the development: the scene maximum at 55 % of the output range (adjustable with the sliders),
        // the stored scale factor taken back
        let f = if scene_max > 0.55 { 0.55 / scene_max as f64 } else { 1.0 } / x3i_scale;
        cal.wb_gain = cal.wb_gain.map(|g| g * f);
        for p in cal.wb_presets.iter_mut() {
            p.gain = p.gain.map(|g| g * f);
        }
    }
    if let Some(g) = base_gain {
        let g = g as f64;
        cal.wb_gain = cal.wb_gain.map(|v| v * g);
        for p in cal.wb_presets.iter_mut() {
            p.gain = p.gain.map(|v| v * g);
        }
        cal.exposure_gain = g;
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("merged bracket: levelled to the base exposure, gain {g:.3} ({:+.2} EV)", g.log2());
        }
    }
    if base_gain.is_none() && store != 1.0 {
        // bracket tone: the calibration gains only take the storage factor back
        let g = 1.0 / store as f64;
        cal.wb_gain = cal.wb_gain.map(|v| v * g);
        for p in cal.wb_presets.iter_mut() {
            p.gain = p.gain.map(|v| v * g);
        }
        cal.exposure_gain = g;
    }
    if let Some(mut t) = bracket_probe {
        t.scene_per_count = crate::film::scene_gain(&cal, &crate::Settings::default()) as f64;
        for m in cal.modes.iter_mut() {
            m.tone.bracket = Some(t);
        }
    }
    if defer_sharpen && amount > 0.0 {
        cal.pending_sharpen = (amount * (1.0 - DEFERRED_SHARPEN)) as f32;
    }
    // the binned path gets the same highlight treatment (per pixel, before binning)
    let unbinned_noise = unbinned_noise.map(|(mut raw, params, unit): (RawImage, crate::quattro_nr::Params, [f32; 3])| {
        own_highlights(&mut raw.planes, w, &cal, layer_sat);
        let luma_neutral = hi.then(|| std::array::from_fn(|c| (cal.wb_gain[2] / cal.wb_gain[c]) as f32));
        Arc::new(Stage1Source { raw, params, unit, luma_neutral, sharpen: if hi { amount as f32 } else { 0.0 }, chroma_step: 0 })
    });
    let raw = RawImage { columns: w, rows: h, planes };
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!(
            "preprocessing {}: preprocessing {:.2?} (x3fuse-core {:.2?}), {}×{}, orientation {}°",
            path.display(),
            t.elapsed(),
            t_scene,
            w,
            h,
            props.get("ROTATION").unwrap_or("?")
        );
    }
    // the sliders need the grid colour of the first stage
    let sliders = sliders.filter(|_| grid_colour.is_some());
    Ok(Prepared { raw: Arc::new(raw), cal: Arc::new(cal), props, classic, unbinned_noise, grid: grid_colour.map(Arc::new), sliders, full: full_source })
}

/// Outlier repair of the stored values on the high-ISO path ([`crate::quattro_speckle`]): top layer, then the
/// bottom and middle layer of the grid; the grid's top channel is formed again from the repaired top layer
/// (`GRAINEO_QUATTRO_SPECKLE=0`: off, measurements).
fn speckles(r: &mut crate::quattro_grid::RawGrid, view: Option<&crate::quattro_hi::outliers::ColourView>, long: bool) {
    if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_SPECKLE").is_ok_and(|v| v == "0") {
        return;
    }
    // long exposures (night sky: stars are single bright points) keep the previous repair
    if !long {
        let (tw, gw, gh) = (r.tw, r.gw, r.gh);
        let (cells, pairs, singles) = crate::quattro_hi::repair_outliers(&mut r.grid, &mut r.top, gw, gh, view);
        let t = &r.top;
        // the grid's top value follows the repaired layer only where the repair moved the cell mean by more than
        // `REGRID_K` noise deviations of a 2 × 2 mean (real outliers); the many small clamps of ordinary noise
        // values would otherwise shift the colour of the grid
        let n = crate::quattro_hi::TOP_MODEL.map(|v| v as f32);
        let updated: usize = r
            .grid
            .par_chunks_mut(gw)
            .enumerate()
            .map(|(y, row)| {
                let mut c = 0;
                for (x, p) in row.iter_mut().enumerate() {
                    let i = 2 * y * tw + 2 * x;
                    let m = 0.25 * (t[i] + t[i + 1] + t[i + tw] + t[i + tw + 1]);
                    let sd = 0.5 * (n[0] + n[1] * m.abs() + n[2] * m * m).sqrt();
                    if (m - p[2]).abs() > REGRID_K * sd {
                        p[2] = m;
                        c += 1;
                    }
                }
                c
            })
            .sum();
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("Quattro outliers: grid {cells}, top pairs {pairs}, top singles {singles} ({:.0} ppm), grid top values updated {updated}", (pairs + singles) as f64 * 1e6 / (tw * r.th) as f64);
        }
        return;
    }

    let th = crate::quattro_speckle::THRESHOLDS;
    let (tw, th_, gw, gh) = (r.tw, r.th, r.gw, r.gh);
    let nt = crate::quattro_speckle::top_layer(&mut r.top, tw, th_, &th, &r.grid, gw, gh);
    let ng = crate::quattro_speckle::grid(&mut r.grid, gw, gh, &th);
    let t = &r.top;
    r.grid.par_chunks_mut(gw).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let i = 2 * y * tw + 2 * x;
            p[2] = 0.25 * (t[i] + t[i + 1] + t[i + tw] + t[i + tw + 1]);
        }
    });
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Quattro speckles: top {nt} ({:.0} ppm), grid {ng}", nt as f64 * 1e6 / (tw * th_) as f64);
    }
}

/// Brings the top value of grid value `a` to `top` and changes the other layers in proportion, so the colour stays:
/// each layer changes by the top layer's change times its ratio to the top value; towards black (top value below
/// `floor`) the ratio glides to `neutral` (layer value of grey per unit of the top layer), so noise around black does
/// not divide by small values.
fn luma_to(a: &mut [f32; 3], top: f32, neutral: [f32; 3], floor: f32) {
    let d = top - a[2];
    let t = a[2].max(0.0);
    for c in 0..2 {
        a[c] += d * (a[c].max(0.0) + neutral[c] * floor) / (t + floor);
    }
    a[2] = top;
}

/// Full-resolution top layer of the high-ISO path (stage units, in place): brightness smoothing ([`HIGH_LUMA`]), the
/// remaining single outliers clamped ([`crate::quattro_speckle::top_layer_after`]) and the base sharpening of
/// `amount` ([`capture_sharpen`]). `GRAINEO_QUATTRO_LUMA=0`: without the smoothing (measurements).
fn high_top(t: &mut [f32], w: usize, h: usize, amount: f32, binned: bool, iso_gain: f32) {
    if luma_enabled() {
        let l = if binned { BINNED_LUMA } else { HIGH_LUMA };
        let k: f32 = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_HL_K").ok().and_then(|v| v.parse().ok()).unwrap_or(if binned { 1.0 } else { high_luma_strength(iso_gain * 100.0) });
        crate::quattro_noise::brightness_smooth_with(t, w, h, l.radius, (l.scatter.0 * k, l.scatter.1 * k), 1.0, l.guide);
        crate::quattro_speckle::top_layer_after(t, w, h, crate::quattro_speckle::THRESHOLDS.k_after);
    }
    sharpen_plane(t, w, h, amount);
}

/// Brightness smoothing of the high-ISO path ([`HIGH_LUMA`]) with its range spread times `strength`.
fn high_luma(t: &mut [f32], w: usize, h: usize, strength: f32) {
    let l = HIGH_LUMA;
    crate::quattro_noise::brightness_smooth_with(t, w, h, l.radius, (l.scatter.0 * strength, l.scatter.1 * strength), 1.0, l.guide);
}

/// Base sharpening ([`capture_sharpen`]) of one plane.
fn sharpen_plane(t: &mut [f32], w: usize, h: usize, amount: f32) {
    if amount > 0.0 {
        let mut rgb: Vec<f32> = t.par_iter().flat_map_iter(|&v| [v, v, v]).collect();
        capture_sharpen(&mut rgb, w, h, amount);
        t.par_iter_mut().zip(rgb.par_chunks(3)).for_each(|(v, p)| *v = p[0]);
    }
}

/// Brightness smoothing of the full-resolution top layer on the high-ISO path: range filter
/// ([`crate::quattro_noise::brightness_smooth_with`]) over a 7 × 7 window, full share, range spread
/// `s² = s₀ + s₁·v` (stage units), the range weights taken against a blend of 30 % of the 3 × 3 mean with the centre
/// (a centre far off its neighbours is smoothed too, so no isolated points stay). Fitted against the maker's output
/// (dp0/dp1/dp3 Quattro ISO 400–1600: fine brightness noise and edge gradient of flat and detailed sections).
struct HighLuma {
    radius: usize,
    scatter: (f32, f32),
    guide: f32,
}

/// Binned high-ISO path: the top layer of the binned denoised layers smoothed once more ([`HIGH_LUMA`] with the range
/// spread of a mean of n × n values times [`BINNED_SCATTER`], 5 × 5 window) and sharpened with [`BINNED_SHARPEN`] times
/// the base sharpening (the full-resolution sharpening is mostly averaged away by binning). Fitted against the
/// maker's binned output (sd Quattro H ISO 800 2 × 2, ISO 1600 3 × 3: fine brightness noise and edge gradient).
fn binned_top(t: &mut [f32], w: usize, h: usize, n: usize, sharpen: f32) {
    let l = BINNED_LUMA;
    let k = BINNED_SCATTER / (n * n) as f32;
    crate::quattro_noise::brightness_smooth_with(t, w, h, 2, (l.scatter.0 * k, l.scatter.1 * k), 1.0, l.guide);
    sharpen_plane(t, w, h, sharpen * BINNED_SHARPEN);
}

/// Share of the base sharpening on the high-ISO path below ISO 800 (see `prepare`).
const HIGH_SHARPEN_LOW_ISO: f64 = 0.75;

/// Range spread factor and sharpening share of [`binned_top`].
const BINNED_SCATTER: f32 = 0.7;
const BINNED_SHARPEN: f32 = 2.0;

/// Variance factor of the first stage on the high-ISO path (unbinned grid; fitted with [`HIGH_LUMA`]).
const HIGH_STAGE1_VAR: f32 = 2.0;

const HIGH_LUMA: HighLuma = HighLuma { radius: 3, scatter: (75.0, 0.792), guide: 0.45 };

/// The previous smoothing ([`HIGH_LUMA`] before the outlier steps of [`crate::quattro_hi::outliers`]); still the base of the
/// binned path ([`binned_top`]), which was fitted with it.
const BINNED_LUMA: HighLuma = HighLuma { radius: 3, scatter: (125.0, 1.32), guide: 0.3 };

/// Factor of the base sharpening on the unbinned high-ISO path (fitted with [`HIGH_LUMA`] on the edge gradient and
/// the fine brightness noise of the maker's output, dp0/dp1 Quattro and sd Quattro H, ISO 400–1600).
const HIGH_SHARPEN_FACTOR: f32 = 1.5;

/// Share of the grid-scale brightness of the smoothed top layer kept on the high-ISO path at capture ISO `iso`
/// (1 up to ISO 400, then falling by 0.2 per stop; fitted on the brightness blotches of the maker's output, dp0 Quattro
/// ISO 400–1600).
fn grid_brightness_share(iso: f32) -> f32 {
    (1.0 - 0.2 * (iso / 400.0).max(1.0).log2()).clamp(0.6, 1.0)
}

/// Strength of the top-layer brightness smoothing on the unbinned high-ISO path at capture ISO `iso` (range spread
/// factor: 1 up to ISO 1600, then falling by 0.35 per stop to 0.3 at ISO 6400; the maker's output keeps much more
/// fine brightness noise at ISO 6400, fitted on dp0/dp1/dp2 Quattro ISO 6400).
fn high_luma_strength(iso: f32) -> f32 {
    (1.0 - 0.35 * (iso / 1600.0).max(1.0).log2()).clamp(0.3, 1.0)
}

/// Threshold of the grid's top value update after the outlier repair (noise deviations of a 2 × 2 mean).
const REGRID_K: f32 = 8.0;

/// Long exposure for the outlier repair ([`crate::quattro_hi::long_exposure`]).
fn long_exposure(camf: &x3f::Camf) -> bool {
    let v = |n: &str| camf.matrix(n).ok().and_then(|m| m.as_f64().first().copied());
    crate::quattro_hi::long_exposure(v("CaptureShutter").unwrap_or(0.0), v("SensorTemperature").unwrap_or(25.0))
}

/// Colour of a grid cell for the grid outlier step, from the automatic white balance and colour matrix of the file.
fn outlier_colour_view(camf: &x3f::Camf) -> Option<crate::quattro_hi::outliers::ColourView> {
    let v = |name: &str, n: usize| camf.matrix(name).ok().map(|m| m.as_f64()).filter(|v| v.len() == n);
    let wb = v("AutoWBGain", 3)?;
    let temp = v("TempGainFact", 3).filter(|t| t.iter().all(|v| v.is_finite() && (0.5..=1.5).contains(v))).unwrap_or(vec![1.0; 3]);
    let g: [f64; 3] = std::array::from_fn(|c| wb[c] * temp[c]);
    let g = g.map(|x| x / g[1]);
    let m = v("AutoCCMatrix", 9)?;
    let iso = camf.matrix("CaptureISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0);
    crate::quattro_hi::outliers::ColourView::new(g, std::array::from_fn(|i| m[i]), iso / 100.0)
}

/// Grid from the stored sensor values from capture ISO 400 on (`GRAINEO_QUATTRO_RAWGRID=0`: off, measurements).
fn raw_grid_enabled() -> bool {
    crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_RAWGRID").map_or(true, |v| v != "0")
}

/// Brightness smoothing for capture ISO 400 to below [`STAGE1_RAW_MIN_ISO`] on x3fuse-core's preparation (measured
/// against Sigma's output: dp Quattro ISO 400–1600 ΔE00 1.7–2.7 → 1.5–2.3, see FAHRPLAN).
fn mid_luma_applies(camf: &x3f::Camf) -> bool {
    let capture_iso = crate::calib::quattro_iso_gain(camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0);
    (STAGE1_MAX_ISO..STAGE1_RAW_MIN_ISO).contains(&capture_iso) && luma_enabled() && !(raw_grid_enabled() && high_path(capture_iso))
}

/// Brightness smoothing of the top layer ([`crate::quattro_nr::luma_smooth`]; `GRAINEO_QUATTRO_LUMA=0`: off).
fn luma_enabled() -> bool {
    crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_LUMA").map_or(true, |v| v != "0")
}

/// Smallest black of x3fuse-core's integer buffers with the first noise reduction stage (intermediate units ≈ sensor
/// counts; leaves the full 14-bit range above it).
const STAGE1_MIN_BIAS: f32 = 2048.0;

/// The first noise reduction stage of the colour grid on the prepared layers up to this capture ISO (exclusive); from
/// here on the high-ISO path ([`high_path`]).
const STAGE1_MAX_ISO: f64 = 400.0;

/// From this capture ISO on, the first stage runs on the grid from the stored sensor values ([`crate::quattro_grid`])
/// with the top layer of x3fuse-core's preparation as the fine detail. Measured against Sigma's output: ISO 6400
/// better than x3fuse-core's preparation alone (dp Quattro ΔE00 3.4–3.8 → 3.0–3.2, block colour 1.9–2.3 → 1.4–1.8).
/// Below it (400 to 3200) the high-ISO path ([`high_path`]).
const STAGE1_RAW_MIN_ISO: f64 = 6400.0;

/// Luminance noise reduction per stop of digital gain (σ_Y = 300 · k · log2 g).
const QUATTRO_LUMA_NR_PER_STOP: f32 = 0.075;

/// Noise reduction of the Quattro preprocessing (x3fuse-core NLM) per digital ISO factor
/// `g = CaptureISO / SensorISO`. x3fuse-core's NLM strength is fixed in the raw value domain;
/// the digital factor (up to ×16 at ISO 6400) amplifies the residual noise afterwards. Hence
/// the strength grows with g (unchanged at g = 1, i.e. ISO 100–400 as before).
/// For experimenting: `GRAINEO_QUATTRO_NRSTR` (colour strength 1–2),
/// `GRAINEO_QUATTRO_NRLK` (luminance per stop of digital factor).
fn quattro_denoise_options(g: f64) -> x3f_core::DenoiseOptions {
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
    let mut o = x3f_core::DenoiseOptions::default();
    let g = g.max(1.0) as f32;
    // Colour: σ grows with √g (at most ×2)
    o.strength = env("GRAINEO_QUATTRO_NRSTR").unwrap_or(g.sqrt()).clamp(1.0, 2.0);
    // Luminance (top layer): only above a digital factor > 1, σ_Y = 300 · k · log2(g)
    let k = env("GRAINEO_QUATTRO_NRLK").unwrap_or(QUATTRO_LUMA_NR_PER_STOP);
    let luma = k * g.log2();
    if luma > 0.0 {
        o.luminance = true;
        o.luma_strength = (luma / o.strength).clamp(0.0, 3.0);
    }
    o
}

/// Saturation of the Classic sensors in raw values per layer: x3fuse-core normalises to
/// `RawSaturationLevel` (or `SaturationLevel`), but the TRUE sensors of the DP1/DP2/SD14/SD15
/// already saturate at ≈ 73–78 % of that (DP1s: ≈ 2320/2240/2230 instead of 2982/3065/3061).
/// Measured with our own TRUE decoder (black ≈ 0 as with x3fuse-core): value of the 20th brightest
/// pixel in the image area (defective pixels excluded); implausibly low (image without
/// blown areas) → [`CLASSIC_CLIP_FALLBACK`] · reference value.
fn classic_clip_fraction(file: &X3fFile, camf: &x3f::Camf) -> [f64; 3] {
    let max_raw: Option<Vec<f64>> = ["RawSaturationLevel", "SaturationLevel"]
        .iter()
        .find_map(|n| camf.matrix(n).ok().map(|m| m.as_f64()).filter(|v| v.len() == 3));
    let (Some(max_raw), Ok(raw)) = (max_raw, file.decode_raw()) else {
        return [1.0; 3];
    };
    let area = camf.matrix("ActiveImageArea").ok().map(|m| m.as_f64()).filter(|v| v.len() == 4);
    let [x0, y0, x1, y1] = area.map_or([0, 0, raw.columns - 1, raw.rows - 1], |v| v.iter().map(|&a| a as usize).collect::<Vec<_>>().try_into().unwrap());
    std::array::from_fn(|c| {
        let mut top: Vec<u16> = Vec::with_capacity((x1 - x0 + 1) * (y1 - y0 + 1));
        for y in y0..=y1.min(raw.rows - 1) {
            top.extend_from_slice(&raw.planes[c][y * raw.columns + x0..=y * raw.columns + x1.min(raw.columns - 1)]);
        }
        let k = top.len().saturating_sub(20);
        let v = *top.select_nth_unstable(k).1 as f64 / max_raw[c];
        if (0.55..=1.0).contains(&v) { v } else { CLASSIC_CLIP_FALLBACK }
    })
}

/// Relation of the preprocessed layers (before layer rescaling) to the raw values per layer:
/// `raw × spatial gain = RawSaturationLevel · layer + offset`, offset as mean over every 4th pixel between about 40
/// raw units and 85 % of `RawSaturationLevel` (the preprocessing removes a dark offset and normalises; Sigma's output
/// stage works on the raw values). `None` without own decoder (Huffman cameras).
fn classic_raw_map(file: &X3fFile, camf: &x3f::Camf, planes: &[Vec<u16>; 3], w: usize, h: usize, wb: &str) -> Option<([f64; 3], [f64; 3])> {
    let sat: Vec<f64> = camf.matrix("RawSaturationLevel").ok().map(|m| m.as_f64()).filter(|v| v.len() == 3)?;
    let raw = file.decode_raw().ok()?;
    let area = camf.matrix("ActiveImageArea").ok().map(|m| m.as_f64()).filter(|v| v.len() == 4)?;
    let (x0, y0) = (area[0] as usize, area[1] as usize);
    if x0 + w > raw.columns || y0 + h > raw.rows {
        return None;
    }
    let sg = classic_spatial_gain(camf, wb);
    let (b, k) = (ENC_BLACK, ENC_SCALE);
    // sums n, Σp, Σr per layer
    let mut acc = [[0f64; 3]; 3];
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let g: [f64; 3] = match &sg {
                Some((rows, cols, v)) => {
                    let fy = y as f64 / h as f64 * (*rows - 1) as f64;
                    let fx = x as f64 / w as f64 * (*cols - 1) as f64;
                    let (ri, ci) = ((fy as usize).min(rows - 2), (fx as usize).min(cols - 2));
                    let (ty, tx) = (fy - ri as f64, fx - ci as f64);
                    let at = |r: usize, c: usize, ch: usize| v[(r * cols + c) * 3 + ch];
                    std::array::from_fn(|ch| (1.0 - ty) * ((1.0 - tx) * at(ri, ci, ch) + tx * at(ri, ci + 1, ch)) + ty * ((1.0 - tx) * at(ri + 1, ci, ch) + tx * at(ri + 1, ci + 1, ch)))
                }
                None => [1.0; 3],
            };
            for c in 0..3 {
                let rv = raw.planes[c][(y0 + y) * raw.columns + x0 + x] as f64;
                if rv > 0.85 * sat[c] {
                    continue;
                }
                let r = rv * g[c];
                let p = (planes[c][y * w + x] as f64 - b) / k;
                // only clearly above black: raw values are cut at 0, which would bias the offset
                if p * sat[c] < 40.0 {
                    continue;
                }
                let a = &mut acc[c];
                a[0] += 1.0;
                a[1] += p;
                a[2] += r;
            }
        }
    }
    // Slope: the preprocessing normalises to `RawSaturationLevel` (checked on DP1, DP2, SD15); only a
    // clear dark offset (high ISO, e.g. DP2x ISO 800: ≈ 68) is taken over, small ones are noise.
    let slope: [f64; 3] = std::array::from_fn(|c| sat[c]);
    let mut off = [0f64; 3];
    for c in 0..3 {
        let [n, sp, sr] = acc[c];
        if n < 1000.0 {
            return None;
        }
        let b = (sr - sat[c] * sp) / n;
        off[c] = if b.abs() >= CLASSIC_DARK_MIN { b } else { 0.0 };
    }
    Some((slope, off))
}

/// Smallest dark offset (raw units) taken over by [`classic_raw_map`].
const CLASSIC_DARK_MIN: f64 = 16.0;

/// Saturation share when the image itself has no blown areas (DP1s measurement).
const CLASSIC_CLIP_FALLBACK: f64 = 0.75;

/// Spatial gain of the Classic cameras (`SpatialGainTables[WB]` or `SpatialGain`, rows ×
/// columns × 3 over the image area, bilinear), as in x3fuse-core `x3f_get_classic_spatial_gain`.
fn classic_spatial_gain(camf: &x3f::Camf, wb: &str) -> Option<(usize, usize, Vec<f64>)> {
    let name = camf.property("SpatialGainTables", wb).unwrap_or("SpatialGain");
    let m = camf.matrix(name).or_else(|_| camf.matrix("SpatialGain")).ok()?;
    let shape = m.shape();
    (shape.len() == 3 && shape[2] == 3 && shape[0] > 1 && shape[1] > 1).then(|| (shape[0], shape[1], m.as_f64()))
}

/// Highlights of the Classic cameras: normalise layers to their real saturation (1 = saturated,
/// so sensor views and raw clipping are correct too) and blend blown areas smoothly to
/// neutral. x3fuse-core's highlight reconstruction does not apply here because it assumes the
/// too-high normalisation; without treatment blown skies would turn magenta.
/// Returns the normalisation factors per layer (for [`Calibration::rescale_layers`]).
fn classic_highlights(file: &X3fFile, camf: &x3f::Camf, planes: &mut [Vec<u16>; 3], w: usize, h: usize, cal: &Calibration) -> [f64; 3] {
    let f = classic_clip_fraction(file, camf);
    let sg = classic_spatial_gain(camf, &cal.wb_name);
    // Gain per layer at the shot white balance (relative)
    let g = cal.wb_gain.map(|v| v / cal.wb_gain.iter().cloned().fold(f64::INFINITY, f64::min));
    let (start, k) = (CLASSIC_HL_START as f32, ENC_SCALE as f32);
    let inv_f = f.map(|v| (1.0 / v) as f32);
    let gf = g.map(|v| v as f32);
    let [p0, p1, p2] = planes;
    p0.par_chunks_mut(w).zip(p1.par_chunks_mut(w)).zip(p2.par_chunks_mut(w)).enumerate().for_each(|(y, ((r0, r1), r2))| {
        // Interpolate the spatial gain row vertically in advance
        let row: Option<Vec<[f32; 3]>> = sg.as_ref().map(|(rows, cols, v)| {
            let fy = y as f64 / h as f64 * (*rows - 1) as f64;
            let (ri, t) = ((fy as usize).min(rows - 2), fy - (fy as usize).min(rows - 2) as f64);
            (0..*cols).map(|c| std::array::from_fn(|ch| ((1.0 - t) * v[(ri * cols + c) * 3 + ch] + t * v[((ri + 1) * cols + c) * 3 + ch]) as f32)).collect()
        });
        for x in 0..w {
            let sgain: [f32; 3] = match (&row, &sg) {
                (Some(r), Some((_, cols, _))) => {
                    let fx = x as f32 / w as f32 * (*cols - 1) as f32;
                    let ci = (fx as usize).min(cols - 2);
                    let t = fx - ci as f32;
                    std::array::from_fn(|ch| (1.0 - t) * r[ci][ch] + t * r[ci + 1][ch])
                }
                _ => [1.0; 3],
            };
            let dec = |u: u16| (u as f32 - ENC_BLACK as f32) / k;
            // x' = x / f: 1 = saturation (without spatial gain), white-balanced v = x' · g
            let xs = [dec(r0[x]) * inv_f[0], dec(r1[x]) * inv_f[1], dec(r2[x]) * inv_f[2]];
            let v = [xs[0] * gf[0], xs[1] * gf[1], xs[2] * gf[2]];
            // Common white point: where the first layer saturates
            let white = (0..3).map(|c| sgain[c] * gf[c]).fold(f32::INFINITY, f32::min);
            let m = v[0].max(v[1]).max(v[2]) / white;
            let out = if m > start {
                let t = ((m - start) / (1.0 - start)).clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t);
                let target = v[0].max(v[1]).max(v[2]).min(white);
                [0, 1, 2].map(|c| ((1.0 - t) * v[c] + t * target).min(white) / gf[c])
            } else {
                xs
            };
            let enc = |x: f32| (ENC_BLACK as f32 + x * k).round().clamp(0.0, 65535.0) as u16;
            r0[x] = enc(out[0]);
            r1[x] = enc(out[1]);
            r2[x] = enc(out[2]);
        }
    });
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Classic: saturation {:.3}/{:.3}/{:.3} of the reference value, spatial gain {}", f[0], f[1], f[2], if sg.is_some() { "yes" } else { "no" });
    }
    f
}

/// From this share of the white point on, Classic highlights glide to neutral.
const CLASSIC_HL_START: f64 = 0.85;

/// Own highlight method for Quattro (without x3fuse-core's reconstruction): from [`CLASSIC_HL_START`] of the common
/// white point (where the first layer saturates, at the shot white balance) the pixel glides to neutral with a
/// smoothstep weight. The weight follows the lowest layer, not the highest: a bright coloured light (one layer near
/// saturation, the others well below) keeps its hue, as in the camera maker's output; only where every layer nears
/// saturation and the colour is no longer measurable does it turn white. Layers above the white point are capped.
/// No donor pixels, no colour models from the neighbourhood.
///
/// `sat`: saturation of each layer in normalised units ([`layer_saturation`]; 1 where the sensor saturates at the top
/// of the stored range). The common white point is the lowest white-balanced saturation, `min(sat[c] · g[c])`.
///
/// Measured against the maker's TIFFs (20 Quattro files, ΔE00): light source in SDQ_0612 7.86 → 0.54 (255/248/240 →
/// 255/231/216, maker 255/230/215); mean over all files 1.243 → 1.242, in highlights 0.935 → 0.917, no file worse.
fn own_highlights(planes: &mut [Vec<u16>; 3], w: usize, cal: &Calibration, sat: [f32; 3]) {
    let (gf, white) = highlight_white(cal, sat);
    let (start, k, b) = (CLASSIC_HL_START as f32, ENC_SCALE as f32, ENC_BLACK as f32);
    let [p0, p1, p2] = planes;
    p0.par_chunks_mut(w).zip(p1.par_chunks_mut(w)).zip(p2.par_chunks_mut(w)).for_each(|((r0, r1), r2)| {
        for x in 0..r0.len() {
            let dec = |u: u16| (u as f32 - b) / k;
            let v = [dec(r0[x]) * gf[0], dec(r1[x]) * gf[1], dec(r2[x]) * gf[2]];
            let Some(out) = highlight_pixel(v, white, start) else {
                continue;
            };
            let out = [0, 1, 2].map(|c| out[c] / gf[c]);
            let enc = |x: f32| (b + x * k).round().clamp(0.0, 65535.0) as u16;
            r0[x] = enc(out[0]);
            r1[x] = enc(out[1]);
            r2[x] = enc(out[2]);
        }
    });
}

/// [`own_highlights`] on the colour grid of the first noise reduction stage (normalised layer values).
fn own_highlights_grid(grid: &mut [crate::quattro_nr::Px], cal: &Calibration, sat: [f32; 3]) {
    let (gf, white) = highlight_white(cal, sat);
    grid.par_iter_mut().for_each(|p| {
        if let Some(out) = highlight_pixel([p[0] * gf[0], p[1] * gf[1], p[2] * gf[2]], white, CLASSIC_HL_START as f32) {
            for c in 0..3 {
                p[c] = out[c] / gf[c];
            }
        }
    });
}

/// Share of the top layer's saturation level from which a grid cell starts to lose its colour
/// ([`neutral_clipped_cells`]).
const CLIPPED_CELL_START: f32 = 0.95;

/// Excess of the lower layers over the grey of the top mean (as a share of that grey) at which a cell with a clipped
/// top pixel is fully neutral ([`neutral_clipped_cells`]).
const CLIPPED_CELL_EXCESS: f32 = 0.1;

/// Grid cells whose 2 × 2 top block holds a clipped pixel, where the top layer saturates below the other layers
/// (`level`: its saturation, normalised units; sd Quattro H): the block's top mean is too low there, so the cell's
/// colour (bottom and middle against the top mean) is wrong, and the unclipped pixels of the block take too much of the
/// lower layers: coloured 2 × 2 blocks at specular highlights. The lower layers of such a cell take the grey of its
/// top mean (`neutral`: layer value of grey per unit of the top layer), gliding in from [`CLIPPED_CELL_START`] of the
/// saturation, and so do the cells next to it; the top layer stays as measured, so every pixel keeps its own
/// brightness. Only cells whose lower layers lie above that grey change (gliding in over
/// [`CLIPPED_CELL_EXCESS`]): the measured top mean is a lower bound, so a cell that reads bluer than grey is at least
/// that blue (a blown coloured surface keeps its hue), while one that reads warmer may as well be neutral. No
/// neighbour's colour is consulted. Runs before the first noise reduction stage (it would spread the wrong colour to
/// the neighbours).
/// `grid`: layer values, `top`: the top layer at full resolution (width `w`), `level` in its units. Returns the cells
/// changed.
fn neutral_clipped_cells(grid: &mut [crate::quattro_nr::Px], top: &[f32], w: usize, gw: usize, gh: usize, level: f32, neutral: [f32; 3]) -> usize {
    if gw == 0 || gh == 0 || 2 * gw > w || top.len() < 2 * gh * w || grid.len() < gw * gh || level <= 0.0 {
        return 0;
    }
    // weight per cell from the brightest pixel of its top block
    let mut weight = vec![0f32; gw * gh];
    weight.par_chunks_mut(gw).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let i = 2 * y * w + 2 * x;
            let m = top[i].max(top[i + 1]).max(top[i + w]).max(top[i + w + 1]);
            let t = ((m / level - CLIPPED_CELL_START) / (1.0 - CLIPPED_CELL_START)).clamp(0.0, 1.0);
            *o = t * t * (3.0 - 2.0 * t);
        }
    });
    if !weight.par_iter().any(|t| *t > 0.0) {
        return 0;
    }
    // the cells around a clipped one as well: the enlargement of the grid in the preparation spreads the wrong
    // colour of a cell to its neighbours
    grid.par_chunks_mut(gw)
        .take(gh)
        .enumerate()
        .map(|(y, row)| {
            let mut n = 0;
            for (x, p) in row.iter_mut().enumerate() {
                let mut t = 0f32;
                for yy in y.saturating_sub(1)..=(y + 1).min(gh - 1) {
                    for xx in x.saturating_sub(1)..=(x + 1).min(gw - 1) {
                        t = t.max(weight[yy * gw + xx]);
                    }
                }
                if t <= 0.0 || p[2] <= 0.0 {
                    continue;
                }
                // how far the lower layers lie above the grey of the top mean (1 = grey)
                let excess = (p[0] / (p[2] * neutral[0])).max(p[1] / (p[2] * neutral[1]));
                let u = ((excess - 1.0) / CLIPPED_CELL_EXCESS).clamp(0.0, 1.0);
                let t = t * u * u * (3.0 - 2.0 * u);
                if t <= 0.0 {
                    continue;
                }
                for c in 0..2 {
                    p[c] = (1.0 - t) * p[c] + t * p[2] * neutral[c];
                }
                n += 1;
            }
            n
        })
        .sum()
}

/// White balance gains of [`own_highlights`] (normalised to the smallest) and the common white point `min(sat · g)`.
fn highlight_white(cal: &Calibration, sat: [f32; 3]) -> ([f32; 3], f32) {
    let g = cal.wb_gain.map(|v| v / cal.wb_gain.iter().cloned().fold(f64::INFINITY, f64::min));
    let gf = g.map(|v| v as f32);
    (gf, (0..3).map(|c| sat[c] * gf[c]).fold(f32::INFINITY, f32::min))
}

/// One white-balanced pixel of [`own_highlights`]: `None` below `start · white`, else the pixel glided to neutral.
fn highlight_pixel(v: [f32; 3], white: f32, start: f32) -> Option<[f32; 3]> {
    let hi = v[0].max(v[1]).max(v[2]);
    if hi <= start * white {
        return None;
    }
    let lo = v[0].min(v[1]).min(v[2]);
    let t = ((lo / white - start) / (1.0 - start)).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    let target = hi.min(white);
    Some([0, 1, 2].map(|c| ((1.0 - t) * v[c] + t * target).min(white)))
}

/// Histogram bin width of [`layer_saturation`] (normalised units).
const SAT_BIN: f32 = 0.0025;
/// Range searched by [`layer_saturation`]: a saturation below this is not plausible.
const SAT_MIN: f32 = 0.2;
/// A pile-up at or above this level counts as saturation at the top of the stored range (1, as assumed so far).
const SAT_FULL: f32 = 0.9;

/// Saturation level of each layer of the prepared layers (normalised units), from the layer's histogram.
///
/// The preparation normalises to the full stored range (2^ImageDepth − 1). Most Quattro files reach it where the
/// sensor saturates. Files with a digital gain per layer (sd Quattro H with `DigitalISOGain` 4/4/2 at ISO 100) store
/// the saturated sensor at about half of it, at a different level per layer (measured: bottom ≈ 0.455, middle ≈ 0.43,
/// top ≈ 0.535). Assuming 1 there leaves blown highlights untouched, with the top layer capped and the lower ones
/// rising further: yellow areas. A saturated layer shows as a sharp pile-up at the top of its histogram, which
/// is searched from the top down: a local peak with enough pixels, far above the density just below it, and with
/// little above it. The level returned is the lower half-maximum edge of the peak (most saturated pixels lie above
/// it). Layers without such a pile-up, or with it at [`SAT_FULL`] or higher, keep 1.
pub fn layer_saturation(planes: &[Vec<u16>; 3]) -> [f32; 3] {
    std::array::from_fn(|c| saturation_from_histogram(&layer_histogram(&planes[c]), planes[c].len()).unwrap_or(1.0))
}

/// [`layer_saturation`] for values that are not stored as a layer (`values`: normalised to the full stored range):
/// the level of the pile-up, `None` without one below [`SAT_FULL`].
pub(crate) fn saturation_of_values(values: &[f32], scale: f32) -> Option<f32> {
    let n = ((1.2 - SAT_MIN) / SAT_BIN) as usize;
    let h = values
        .par_chunks(1 << 16)
        .fold(
            || vec![0u32; n],
            |mut h, chunk| {
                for &v in chunk {
                    let x = v * scale;
                    if x >= SAT_MIN {
                        let i = ((x - SAT_MIN) / SAT_BIN) as usize;
                        if i < n {
                            h[i] += 1;
                        }
                    }
                }
                h
            },
        )
        .reduce(|| vec![0u32; n], |mut a, b| {
            a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
            a
        });
    saturation_from_histogram(&h, values.len())
}

/// Histogram of one encoded layer over [`SAT_MIN`] ..< 1.2 in bins of [`SAT_BIN`].
fn layer_histogram(plane: &[u16]) -> Vec<u32> {
    let n = ((1.2 - SAT_MIN) / SAT_BIN) as usize;
    let (b, k) = (ENC_BLACK as f32, ENC_SCALE as f32);
    plane
        .par_chunks(1 << 16)
        .fold(
            || vec![0u32; n],
            |mut h, chunk| {
                for &u in chunk {
                    let x = (u as f32 - b) / k;
                    if x >= SAT_MIN {
                        let i = ((x - SAT_MIN) / SAT_BIN) as usize;
                        if i < n {
                            h[i] += 1;
                        }
                    }
                }
                h
            },
        )
        .reduce(|| vec![0u32; n], |mut a, b| {
            a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
            a
        })
}

/// Saturation level from a layer histogram ([`layer_histogram`]) of `n` pixels; `None` without a clear pile-up below
/// [`SAT_FULL`].
fn saturation_from_histogram(h: &[u32], n: usize) -> Option<f32> {
    let min_count = (n as f64 / 50_000.0).max(200.0) as u32;
    let sum = |a: usize, b: usize| -> u64 { h[a.min(h.len())..b.min(h.len())].iter().map(|&v| v as u64).sum() };
    for i in (40..h.len() - 2).rev() {
        let c = h[i];
        if c < min_count || c < h[i - 1] || c < h[i + 1] || c < h[i - 2] || c < h[i + 2] {
            continue;
        }
        // density 0.04–0.10 below the peak
        let below = sum(i - 40, i - 16) as f64 / 24.0;
        if (c as f64) < 8.0 * below.max(1.0) {
            continue;
        }
        // little above the peak (beyond 0.025)
        let peak = sum(i - 4, i + 5);
        if sum(i + 10, h.len()) as f64 > 0.25 * peak as f64 {
            continue;
        }
        let mut lo = i;
        while lo > 0 && h[lo - 1] * 2 >= c {
            lo -= 1;
        }
        let level = SAT_MIN + lo as f32 * SAT_BIN;
        return (level < SAT_FULL).then_some(level);
    }
    None
}

/// Classic: Sigma's whole chain at the shot white balance and colour mode ([`crate::classic_new`]),
/// converted to the normalisation of [`crate::linearize`] through the inverse of the shot mode's
/// conversion matrix ([`crate::classic_new::conversion`]), so that the output step gets back exactly the
/// chain's codes. `None` (and the previous way) where the chain is not available.
fn classic_chain(file: &X3fFile, camf: &x3f::Camf, props: &PropList, cal: &mut Calibration, w: usize, h: usize) -> Option<Arc<Linear>> {
    // output stages that only work with the chain's result are dropped when it is not available
    let drop_chain_only = |cal: &mut Calibration| {
        for m in &mut cal.modes {
            if m.classic.as_ref().is_some_and(|c| c.only_chain) {
                m.classic = None;
            }
        }
    };
    if std::env::var("GRAINEO_CLASSIC_CHAIN").is_ok_and(|v| v == "0") {
        drop_chain_only(cal);
        return None;
    }
    let t = std::time::Instant::now();
    let mode = cal.camera_mode.clone();
    // the chain also hands out the high-pass of its detail step (sharpness slider)
    let run = || crate::classic_new::run(file, camf, props, &cal.wb_name, &mode, file.header_rotation().unwrap_or(0) as u32);
    let (result, high_pass) = crate::classic_new::detail::with_high_pass(run);
    let codes = match result {
        Ok(c) => c,
        Err(e) => {
            if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                eprintln!("Classic chain not available: {e}");
            }
            drop_chain_only(cal);
            return None;
        }
    };
    // grey-world share of the gains (incandescent light, high ISO): the chain's mean per layer
    if let Some(mean) = crate::classic_new::gray_mean_of_run() {
        for m in &mut cal.modes {
            if let Some(c) = m.classic.as_mut() {
                Arc::make_mut(c).gray_world(mean);
            }
        }
    }
    let [x0, y0, x1, y1] = codes.crop;
    let spp = cal.mode(Some(&mode)).classic.clone();
    let a_inv = spp.as_ref().and_then(|s| crate::matrix::Mat3(std::array::from_fn(|i| f64::from(s.a[i]))).inverse());
    let (Some(spp), Some(a_inv), true) = (spp, a_inv, x1 - x0 == w && y1 - y0 == h) else {
        drop_chain_only(cal);
        return None;
    };
    let a_inv = a_inv.to_f32();
    // Render: x = lin · count gain of the calibration / count gain of the output stage · k_raw
    let k: [f32; 3] = std::array::from_fn(|c| (spp.wb_gain[c] / (spp.k_raw[c] * cal.wb_gain[c])) as f32);
    let mut data = vec![0f32; w * h * 3];
    // new output step: the codes as fractional values that keep the exact index of the detail step
    // (rounded they are the codes; the sharpness slider needs the index, see `output::code_with_position`)
    let high_pass = high_pass.filter(|hp| hp.width == w && hp.height == h && hp.index.len() == w * h * 3);
    let position = high_pass.as_ref().map(|hp| hp.index.as_slice());
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let src = &codes.data[((y0 + y) * codes.width + x0) * 3..((y0 + y) * codes.width + x1) * 3];
        let idx = position.map(|l| &l[y * w * 3..(y + 1) * w * 3]);
        for (x, (o, c)) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)).enumerate() {
            let v = match idx {
                Some(l) => std::array::from_fn(|i| crate::classic_new::output::code_with_position(l[x * 3 + i]) / 4096.0),
                None => [c[0] as f32 / 4096.0, c[1] as f32 / 4096.0, c[2] as f32 / 4096.0],
            };
            for r in 0..3 {
                o[r] = (a_inv[r * 3] * v[0] + a_inv[r * 3 + 1] * v[1] + a_inv[r * 3 + 2] * v[2]) * k[r];
            }
        }
    });
    // the output step's slider-independent part per colour mode from the chain's codes (image mean,
    // mode contrast); used in `render_counts_impl`
    let opt = crate::classic_new::output::Options::for_camera(props);
    for m in &mut cal.modes {
        if m.classic.is_some() {
            m.classic_output = Some(Arc::new(crate::classic_new::output::OutputBasis::new(camf, props, &m.name, &codes, opt)));
        }
    }
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Classic-Kette: {:.2?}", t.elapsed());
    }
    // the high-pass travels with the counts (crops, disk cache) as a detail image of its own kind
    let detail = high_pass.map(|hp| Arc::new(crate::sharpness::SharpDetail { l: hp.values, base: crate::sharpness::CLASSIC_HIGH_PASS, scale: 1.0 }));
    Some(Arc::new(Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail }))
}

/// Working image for [`crate::render_counts`]: RGB counts of colour mode `mode` at the
/// shot white balance (before the tone curve), like the SPP base for Merrill.
/// Like [`counts`], but from a (e.g. denoised) linear image from
/// [`crate::linearize`] (Quattro: without positional corrections, `x = (u − black)/sat`).
pub fn counts_from_linear(p: &Prepared, lin: &Linear, mode: Option<&str>, binning: crate::Binning) -> Linear {
    let cal = &p.cal;
    let g: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c] * cal.saturation) as f32);
    let src = &lin.data;
    let mut c = binned_counts(cal, mode, lin.width, lin.height, binning_factor(cal, binning), |i| [src[i * 3] * g[0], src[i * 3 + 1] * g[1], src[i * 3 + 2] * g[2]]);
    // Classic: the high-pass of the new output step stays with the counts (same size only)
    if c.width == lin.width && c.height == lin.height {
        c.detail = lin.detail.clone().filter(|d| d.is_classic_high_pass());
    }
    c
}

/// [`to_counts`] after binning `n × n` ([`binning_factor`]); `n = 1` unchanged.
fn binned_counts(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, n: usize, v: impl Fn(usize) -> [f32; 3] + Sync) -> Linear {
    if n <= 1 {
        let data = to_counts(cal, mode, w, h, false, v);
        return Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
    }
    let (bw, bh, b) = bin_values(w, h, n, v);
    let data = to_counts(cal, mode, bw, bh, true, |i| b[i]);
    Linear { mono: false, haze: None, toneq: None, width: bw, height: bh, data, detail: None }
}

/// Working image for [`crate::render_counts`]: RGB counts of colour mode `mode` at the shot white balance (before
/// the tone curve). Quattro: the colour stage ([`crate::quattro_color`]); Classic: the result of Sigma's chain where
/// available; other cameras: the plain colour matrix.
pub fn counts(p: &Prepared, mode: Option<&str>, binning: crate::Binning) -> Linear {
    counts_hl(p, mode, binning, crate::HighlightSliders::default())
}

/// [`counts`] with the highlight sliders ([`crate::highlight_rebuild`]; they act only where the rebuild ran).
pub fn counts_hl(p: &Prepared, mode: Option<&str>, binning: crate::Binning, hl: crate::HighlightSliders) -> Linear {
    if let Some(lin) = p.classic.as_ref() {
        return counts_from_linear(p, lin, mode, binning);
    }
    if let Some(f) = p.full.as_deref() {
        return full_counts(&p.cal, f, mode, (2, 2), 0, binning_factor(&p.cal, binning));
    }
    let cal = &p.cal;
    // p = (u − black)/sat, counts = M · diag(count_scale · wb_gain · sat) · p
    let g: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c]) as f32);
    let black = cal.black.map(|b| b as f32);
    let n = binning_factor(cal, binning);
    if let Some(s) = p.unbinned_noise.as_deref().filter(|_| n > 1) {
        // the first noise reduction stage after binning (on the binned grid)
        let (w, h) = (s.raw.columns, s.raw.rows);
        let pl = &s.raw.planes;
        let k = ENC_SCALE as f32;
        let (bw, bh, mut b) = bin_values(w, h, n, |i| std::array::from_fn(|c| (pl[c][i] as f32 - black[c]) / k * s.unit[c]));
        if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_STAGE1_BIN").map_or(true, |v| v != "0") {
            // high-ISO path: the binned layers are already denoised; this pass removes larger colour blotches, the
            // brightness stays that of the denoised layers
            let top: Option<Vec<f32>> = s.luma_neutral.map(|_| {
                let mut t: Vec<f32> = b.par_iter().map(|p| p[2]).collect();
                binned_top(&mut t, bw, bh, n, s.sharpen);
                t
            });
            crate::quattro_nr::stage1(&mut b, bw, bh, &s.params);
            if let (Some(t), Some(nt)) = (top, s.luma_neutral) {
                let k: [f32; 3] = std::array::from_fn(|c| nt[c] * s.unit[c] / s.unit[2]);
                b.par_iter_mut().zip(t.par_iter()).for_each(|(p, &tv)| luma_to(p, tv, k, DETAIL_T_FLOOR));
                if s.chroma_step > 0 {
                    // values in units of the stage: grey per top unit as such, the binned value a mean of (n/2)² cells
                    chroma_smooth(&mut b, bw, bh, k, 1.0, (n as f32 / 2.0).powi(2).max(1.0), s.chroma_step);
                }
            }
        }
        let scale: [f32; 3] = std::array::from_fn(|c| k * g[c] / s.unit[c]);
        let data = to_counts(cal, mode, bw, bh, true, |i| std::array::from_fn(|c| b[i][c] * scale[c]));
        return Linear { mono: false, haze: None, toneq: None, width: bw, height: bh, data, detail: None };
    }
    let raw = &p.raw;
    let (w, h) = (raw.columns, raw.rows);
    let pl = &raw.planes;
    let colour_stage = cal.model.is_quattro() && cal.sensor_iso > 0.0 && crate::quattro_hi::developer_switch("GRAINEO_RAWBLEND").map_or(true, |v| v != "0");
    if let Some(grid) = p.grid.as_deref().filter(|g| n <= 1 && colour_stage && g.width * 2 <= w && g.height * 2 <= h) {
        let data = grid_counts(cal, mode, grid, w, h, hl, |i| (pl[2][i] as f32 - black[2]) * g[2]);
        return Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
    }
    binned_counts(cal, mode, w, h, n, |i| std::array::from_fn(|c| (pl[c][i] as f32 - black[c]) * g[c]))
}

/// Working image like [`counts`], with the noise sliders away from the middle position (high-ISO path only, `None`
/// otherwise): `nr` = steps (luminance, colour) −1 … 4 ([`crate::SppKey::nr`], 2 = middle = the result of the
/// preparation).
/// - Colour above the middle: the colour of the grid smoothed further, guided by the top layer
///   ([`chroma_smooth`], radius growing with the step); below: the colour of the grid moves back towards the grid
///   before the first stage ([`SLIDER_BACK`], "Off" = all of it), the brightness stays.
/// - Luminance above the middle: the brightness smoothing once more with its range spread times [`SLIDER_LUMA_MORE`] per
///   step above the middle; below: the top layer moves back towards its state before the smoothing ([`SLIDER_BACK`]). The change is
///   neutral on all three layers.
pub fn counts_with_sliders(p: &Prepared, mode: Option<&str>, binning: crate::Binning, nr: (i8, i8), detail: i8) -> Option<Linear> {
    if let Some(f) = p.full.as_deref() {
        return Some(full_counts(&p.cal, f, mode, nr, detail, binning_factor(&p.cal, binning)));
    }
    // the detail step acts in the full chain only: the other ways keep their middle result
    if nr == (2, 2) {
        return None;
    }
    let src = p.sliders.as_deref()?;
    let grid = p.grid.as_deref()?;
    let (w, h) = (p.raw.columns, p.raw.rows);
    let (dl, dc) = (nr.0.clamp(-1, 4) as i32 - 2, nr.1.clamp(-1, 4) as i32 - 2);
    let (b, k) = (ENC_BLACK as f32, ENC_SCALE as f32);
    let mut planes = p.raw.planes.clone();
    if dl != 0 && src.top.len() == w * h {
        let u = src.unit[2];
        let mut t: Vec<f32> = planes[2].par_iter().map(|&v| (v as f32 - b) / k * u).collect();
        let before = t.clone();
        if dl > 0 {
            high_luma(&mut t, w, h, SLIDER_LUMA_MORE * dl as f32);
        } else {
            let a = SLIDER_BACK[(-dl - 1) as usize];
            t.par_iter_mut().zip(src.top.par_iter()).for_each(|(v, &q)| *v += a * ((q as f32 - b) / k * u - *v));
        }
        let n = src.neutral;
        let enc = |v: f32| v.round().clamp(0.0, 65535.0) as u16;
        let [p0, p1, p2] = &mut planes;
        p0.par_iter_mut().zip(p1.par_iter_mut()).zip(p2.par_iter_mut()).zip(t.par_iter().zip(before.par_iter())).for_each(|(((q0, q1), q2), (&tv, &tb))| {
            let d = (tv - tb) / u * k;
            *q0 = enc(*q0 as f32 + d * n[0]);
            *q1 = enc(*q1 as f32 + d * n[1]);
            *q2 = enc(*q2 as f32 + d * n[2]);
        });
    }
    let mut colour = grid.data.clone();
    if dc > 0 {
        chroma_smooth(&mut colour, grid.width, grid.height, src.neutral, src.unit[2], 1.0, dc as usize);
    } else if dc < 0 && src.grid.len() == colour.len() {
        let a = SLIDER_BACK[(-dc - 1) as usize] * if dc > -3 { SLIDER_BACK_COLOUR } else { 1.0 };
        let floor = DETAIL_T_FLOOR / src.unit[2];
        colour.par_iter_mut().zip(src.grid.par_iter()).for_each(|(c, q)| {
            let mut q = *q;
            luma_to(&mut q, c[2], src.neutral, floor);
            for ch in 0..3 {
                c[ch] += a * (q[ch] - c[ch]);
            }
        });
    }
    // the layers follow the colour change of the grid (the binned path works on them)
    if dc != 0 {
        let (gw, gh) = (grid.width, grid.height);
        let [p0, p1, _] = &mut planes;
        for (c, pl) in [p0, p1].into_iter().enumerate() {
            let d: Vec<f32> = colour.iter().zip(grid.data.iter()).map(|(a, o)| (a[c] - o[c]) * k).collect();
            let up = crate::quattro_nr::enlarge(&d, gw, gh, w, h);
            pl.par_iter_mut().zip(up.par_iter()).for_each(|(q, &dv)| *q = (*q as f32 + dv).round().clamp(0.0, 65535.0) as u16);
        }
    }
    let unbinned_noise = p.unbinned_noise.as_ref().map(|s| {
        Arc::new(Stage1Source { raw: RawImage { columns: w, rows: h, planes: planes.clone() }, params: s.params, unit: s.unit, luma_neutral: s.luma_neutral, sharpen: s.sharpen, chroma_step: dc.max(0) as usize })
    });
    let q = Prepared {
        raw: Arc::new(RawImage { columns: w, rows: h, planes }),
        cal: p.cal.clone(),
        props: p.props.clone(),
        classic: None,
        unbinned_noise,
        grid: Some(Arc::new(GridColour { width: grid.width, height: grid.height, data: colour, limited: grid.limited, highlight: grid.highlight.clone() })),
        sliders: None,
        full: None,
    };
    Some(counts(&q, mode, binning))
}

/// Working image of the high-ISO chain ([`crate::quattro_hi`]) for colour mode `mode` and the slider steps `nr`
/// (luminance, colour; −1 = off, 2 = middle) and the detail step `detail` (−3 … 3, 0 = middle), converted to the
/// counts of [`counts`].
fn full_counts(cal: &Calibration, f: &crate::quattro_hi::Inputs, mode: Option<&str>, nr: (i8, i8), detail: i8, bin: usize) -> Linear {
    let t0 = std::time::Instant::now();
    let (luma, chroma) = (nr.0.clamp(-1, 4), nr.1.clamp(-1, 4));
    // white balance of the counts in units of the chain, normalised to the middle layer
    let gains: [f32; 3] = std::array::from_fn(|c| ((cal.wb_gain[c] / cal.wb_gain[1]) * f64::from(f.unit[1] / f.unit[c])) as f32);
    let m = cal.cam_to_srgb(mode, None);
    let look = crate::quattro_hi::Look {
        matrix: std::array::from_fn(|i| m.0[i] / cal.iso_gain),
        gains,
        sensor: (cal.sensor_iso / 100.0) as f32,
        compact: cal.model != crate::calib::Model::SdQuattro,
        fine: cal.mode(mode).axes.as_deref().filter(|_| crate::quattro_hi::developer_switch("GRAINEO_MULTIAXIS").map_or(true, |v| v != "0")),
        portrait: cal.mode(mode).name == "Portrait",
    };
    let out = f.develop(&look, luma, chroma, detail, bin);
    // output values → counts: counts = cam_to_srgb · v with v = b / s (b white-balanced values of the chain)
    let s = f64::from(f.unit[1]) / (cal.count_scale * cal.wb_gain[1] * ENC_SCALE);
    let k = (100.0 / (cal.sensor_iso * s)) as f32;
    let mut data = out.rgb;
    data.par_iter_mut().for_each(|v| *v *= k);
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Quattro high-ISO chain: steps luma {luma} colour {chroma}, gains {gains:?}, count factor {k:.4}, {:.2?}", t0.elapsed());
    }
    Linear { mono: false, haze: None, toneq: None, width: out.width, height: out.height, data, detail: None }
}

/// Shares by which a slider step below the middle moves back towards the state before the noise reduction (steps
/// 1, 0, "Off"); the colour moves by [`SLIDER_BACK_COLOUR`] of that on steps 1 and 0 (the unprocessed colour of the
/// grid is far noisier than the maker's lowest step).
const SLIDER_BACK: [f32; 3] = [0.15, 0.3, 1.0];
const SLIDER_BACK_COLOUR: f32 = 0.2;

/// Luminance slider above the middle: range spread of the further smoothing pass per step, as a share of the first
/// pass's (the smoothed layer has far less noise left than the model describes).
const SLIDER_LUMA_MORE: f32 = 0.12;

/// Further colour smoothing of the grid for the colour slider: the colour (each layer's ratio to the top layer)
/// averaged over a square window of radius `2 + 4·step` with Gaussian distance weights,
/// and range weights on the top layer (a neighbour across a brightness edge counts little); the top layer stays.
/// A joint (cross) range filter, a long-known method.
/// `neutral`: layer value of grey per unit of the top layer, `u`: value → units of the stage, `div`: the grid values are
/// means of that many grid cells (binned; their noise variance is the model's divided by it).
fn chroma_smooth(g: &mut [crate::quattro_nr::Px], gw: usize, gh: usize, neutral: [f32; 3], u: f32, div: f32, step: usize) {
    let r = 2 + 4 * step as isize;
    // range spread in noise deviations of the top value: 3 (step 1) and 4 (step 2, "more"), matched to the maker's
    // strongest colour step on flat areas
    let spread = 2.0 + step as f32;
    let sigma_s = r as f32 / 2.0;
    let n = neutral;
    let f = DETAIL_T_FLOOR / u;
    // colour as the ratio of each layer to the top layer (gliding to grey's ratio towards black, like `luma_to`)
    let ratio = |p: &crate::quattro_nr::Px| [(p[0].max(0.0) + n[0] * f) / (p[2].max(0.0) + f), (p[1].max(0.0) + n[1] * f) / (p[2].max(0.0) + f)];
    // difference form (default): colour as each layer's difference to grey at the pixel's top value; ratio form
    // (`GRAINEO_QUATTRO_CHROMA=ratio`, previous)
    let diff = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_CHROMA").map_or(true, |v| v != "ratio");
    let chroma: Vec<[f32; 2]> = if diff { g.par_iter().map(|p| [p[0] - n[0] * p[2], p[1] - n[1] * p[2]]).collect() } else { g.par_iter().map(ratio).collect() };
    let top: Vec<f32> = g.par_iter().map(|p| p[2]).collect();
    g.par_chunks_mut(gw).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let t = top[y * gw + x];
            // range spread: twice the noise deviation of the grid's top value
            let sd = spread * ((crate::quattro_nr::grid_variance([0.0, 0.0, t * u])[2] as f32 / div).sqrt()) / u;
            let inv = 1.0 / (2.0 * sd * sd).max(1e-12);
            let (mut acc, mut sw) = ([0f32; 2], 0f32);
            for dy in -r..=r {
                let yy = (y as isize + dy).clamp(0, gh as isize - 1) as usize;
                for dx in -r..=r {
                    let xx = (x as isize + dx).clamp(0, gw as isize - 1) as usize;
                    let i = yy * gw + xx;
                    let dt = top[i] - t;
                    let wt = (-((dx * dx + dy * dy) as f32) / (2.0 * sigma_s * sigma_s) - dt * dt * inv).exp();
                    acc[0] += wt * chroma[i][0];
                    acc[1] += wt * chroma[i][1];
                    sw += wt;
                }
            }
            if diff {
                p[0] = n[0] * t + acc[0] / sw;
                p[1] = n[1] * t + acc[1] / sw;
            } else {
                let tt = t.max(0.0) + f;
                p[0] = acc[0] / sw * tt - n[0] * f;
                p[1] = acc[1] / sw * tt - n[1] * f;
            }
        }
    });
}

/// Parameters of the first noise reduction stage from the camera data.
struct Stage1Camera {
    /// daylight white balance gains with temperature factor, normalised to the middle layer
    gains: [f32; 3],
    /// normalised layer value → counts above black / (SensorISO/100) (sd Quattro H: times the digital ISO gain)
    unit: [f32; 3],
    /// black of the stored sensor values (`RawOffset`)
    offset: f32,
    /// stored value above black → units of the stage: digital ISO gain / (SensorISO/100)
    raw_unit: [f32; 3],
    /// white of the raw range above black (14-bit values: 16383 − `RawOffset`)
    white: f32,
}

fn stage1_params(camf: &x3f::Camf, _props: &PropList, sd_quattro_h: bool) -> Stage1Camera {
    let v3 = |name: &str| camf.matrix(name).ok().map(|m| m.as_f64()).filter(|v| v.len() == 3).map(|v| [v[0], v[1], v[2]]);
    let sun = v3("SunlightWBGain").or_else(|| camf.property("WhiteBalanceGains", "Sunlight").and_then(|n| v3(&n))).unwrap_or([1.0; 3]);
    let temp = v3("TempGainFact").filter(|t| t.iter().all(|v| v.is_finite() && (0.5..=1.5).contains(v))).unwrap_or([1.0; 3]);
    let g: [f64; 3] = std::array::from_fn(|c| sun[c] * temp[c]);
    let gains = std::array::from_fn(|c| (g[c] / g[1]) as f32);
    let offset = camf.matrix("RawOffset").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(2048.0).min(4096.0);
    let sensor = camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).filter(|v| *v > 0.0).unwrap_or(100.0);
    let digital = if sd_quattro_h { v3("DigitalISOGain").unwrap_or([1.0; 3]) } else { [1.0; 3] };
    let unit = std::array::from_fn(|c| ((16383.0 - offset) / (sensor / 100.0) * digital[c]) as f32);
    let raw_unit = std::array::from_fn(|c| (digital[c] / (sensor / 100.0)) as f32);
    Stage1Camera { gains, unit, offset: offset as f32, raw_unit, white: (16383.0 - offset) as f32 }
}

/// White-balanced camera counts `v(i)` → output counts: Quattro the colour stage, other cameras the colour matrix.
/// `binned`: the image was binned (changes the brightness index of the colour stage).
fn to_counts(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, binned: bool, v: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    let m = cal.cam_to_srgb(mode, None);
    let stage = cal.model.is_quattro() && cal.sensor_iso > 0.0 && crate::quattro_hi::developer_switch("GRAINEO_RAWBLEND").map_or(true, |v| v != "0");
    if stage {
        return colour_stage(cal, mode, w, h, binned, v);
    }
    let mut data = vec![0f32; w * h * 3];
    {
        let m = m.to_f32();
        data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
            let v = v(i);
            for r in 0..3 {
                px[r] = m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2];
            }
        });
        if cal.pending_sharpen > 0.0 {
            capture_sharpen(&mut data, w, h, cal.pending_sharpen);
        }
    }
    data
}

/// Colour stage ([`crate::quattro_color`]) on white-balanced counts `v`: matrix blend,
/// chroma clamp, colour mode fine correction (sd Quattro); the local contrast follows in [`finish_own`].
fn own_colour(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, binned: bool, v: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    use crate::quattro_color as qf;
    let mix = match (cal.model == crate::calib::Model::SdQuattro, binned) {
        (true, false) => qf::MIX_SD,
        (true, true) => qf::MIX_SD_BINNED,
        _ => qf::MIX_DP,
    };
    let stage = qf::ColorLevel {
        matrix: cal.cam_to_srgb(mode, None).to_f32(),
        iso: cal.iso_gain as f32,
        level: (cal.exposure_gain / (ENC_SCALE * cal.count_scale * cal.wb_gain[1] * cal.sensor_iso / 100.0)) as f32,
        signal: (100.0 / (cal.iso_gain * cal.sensor_iso)) as f32,
        mix,
    };
    let mut data = vec![0f32; w * h * 3];
    data.par_chunks_mut(3).enumerate().for_each(|(i, px)| px.copy_from_slice(&stage.pixel(v(i))));
    qf::chroma_limit(&mut data, w, h, OWN_CLAMP_RADIUS, OWN_CLAMP_RANK);
    if let Some(a) = cal.mode(mode).axes.as_deref().filter(|_| crate::quattro_hi::developer_switch("GRAINEO_MULTIAXIS").map_or(true, |v| v != "0")) {
        data.par_chunks_mut(3).for_each(|px| a.apply(px));
    }
    data
}

/// Colour stage incl. its finishing step.
fn colour_stage(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, binned: bool, v: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    let mut data = own_colour(cal, mode, w, h, binned, v);
    finish_own(cal, &mut data, w, h);
    data
}

/// Neighbourhood radius and rank of the chroma clamp of the own colour stage (3 × 3 minimum/maximum; larger
/// neighbourhoods changed the result by less than 0.05 ΔE00 in the comparison).
const OWN_CLAMP_RADIUS: usize = 1;
const OWN_CLAMP_RANK: usize = 0;

/// After the colour stage: local contrast below capture ISO 800 ([`crate::quattro_color::local_contrast`]).
fn finish_own(cal: &Calibration, data: &mut [f32], w: usize, h: usize) {
    if !lce_applies(cal.iso_gain * cal.sensor_iso) {
        return;
    }
    let white = (ENC_SCALE * cal.count_scale * cal.sensor_iso / 100.0) as f32;
    crate::quattro_color::local_contrast(data, w, h, white, &crate::quattro_color::CONTRAST_GAINS, crate::quattro_color::CONTRAST_SOFT);
}

/// After the first noise reduction stage: the colour is formed on the denoised colour grid (before the
/// detail of the top layer comes back), enlarged 2× and the fine detail of the top
/// layer added to the brightness only (BT.601 Y): ΔY = ΔT · Y/T of the enlarged grid (T floor
/// [`DETAIL_T_FLOOR`]), so the top-layer noise does not become colour noise.
fn grid_counts(cal: &Calibration, mode: Option<&str>, grid: &GridColour, w: usize, h: usize, hl: crate::HighlightSliders, t_full: impl Fn(usize) -> f32 + Sync) -> Vec<f32> {
    let (gw, gh) = (grid.width, grid.height);
    let g: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c] * ENC_SCALE) as f32);
    let grid_v = |i: usize| std::array::from_fn(|c| grid.data[i][c] * g[c]);
    let mut cg = own_colour(cal, mode, gw, gh, false, grid_v);
    // highlight sliders in the rebuilt areas: chroma of the cells around their brightness (colour), top-layer
    // detail (structure); both 1 = as rebuilt
    let hw = grid.highlight.as_deref().filter(|c| c.len() == gw * gh);
    let (kc, ks) = (crate::highlight_rebuild::colour_factor(hl.colour()), crate::highlight_rebuild::structure_factor(hl.structure()));
    if let Some(cw) = hw.filter(|_| kc != 1.0) {
        highlight_colour(&mut cg, cw, gw, gh, kc);
    }
    let detail_k: Option<Vec<f32>> = hw.filter(|_| ks != 1.0).map(|cw| {
        let up = crate::quattro_nr::enlarge(cw, gw, gh, w, h);
        up.into_par_iter().map(|wt| 1.0 + wt.clamp(0.0, 1.0) * (ks - 1.0)).collect()
    });
    // counts → units of the detail floor: counts above black / (SensorISO/100)
    let k = (100.0 / cal.sensor_iso) as f32;
    let enlarge = if grid.limited { crate::quattro_nr::enlarge_limited } else { crate::quattro_nr::enlarge };
    let up: [Vec<f32>; 3] = std::array::from_fn(|c| enlarge(&cg.chunks_exact(3).map(|p| p[c]).collect::<Vec<_>>(), gw, gh, w, h));
    let t_up = enlarge(&grid.data.iter().map(|p| p[2] * g[2]).collect::<Vec<_>>(), gw, gh, w, h);
    let floor = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_TFLOOR").ok().and_then(|v| v.parse().ok()).unwrap_or(DETAIL_T_FLOOR) / k;
    let add = crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_DETAILY").is_ok_and(|v| v == "add");
    let mut data = vec![0f32; w * h * 3];
    data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let o = [up[0][i], up[1][i], up[2][i]];
        let y = 0.299 * o[0] + 0.587 * o[1] + 0.114 * o[2];
        let dy = (t_full(i) - t_up[i]) * y.max(0.0) / t_up[i].max(floor);
        let dy = detail_k.as_ref().map_or(dy, |k| dy * k[i]);
        if add {
            for c in 0..3 {
                px[c] = o[c] + dy;
            }
        } else {
            // same ratio on all three channels: brightness changes, hue and saturation stay
            let f = if y > 0.0 { ((y + dy) / y).max(0.0) } else { 1.0 };
            for c in 0..3 {
                px[c] = o[c] * f;
            }
        }
    });
    if cal.pending_sharpen > 0.0 && crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_GRIDSHARP").is_ok_and(|v| v == "1") {
        capture_sharpen(&mut data, w, h, cal.pending_sharpen);
    }
    finish_own(cal, &mut data, w, h);
    data
}

/// Slider "Lichter-Farbe" on the colour cells `cg` (RGB per cell, `gw × gh`) with the rebuilt-area weight `cw`:
/// chroma factor `k` (1 = as rebuilt). The weight is softened over a few cells, so the change fades out instead of
/// stopping at the cells of the mask (the patchy look at 0). Less colour (`k < 1`) takes the chroma of each cell
/// towards its brightness; more colour (`k > 1`) adds only the smoothed chroma (no amplified colour noise) and only
/// where it is clear (a nearly neutral rebuilt surface stays neutral instead of turning into yellow-green or cyan
/// blotches).
fn highlight_colour(cg: &mut [f32], cw: &[f32], gw: usize, gh: usize, k: f32) {
    // the weight reaches over the whole coloured surface around the rebuilt cells (a label is rebuilt only in parts:
    // acting on those parts alone left it patchy), softly
    let wt = box_blur(&max_filter(cw, gw, gh, COLOUR_REACH), gw, gh, COLOUR_REACH / 2);
    let luma = |p: &[f32]| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
    if k < 1.0 {
        cg.par_chunks_mut(3).zip(wt.par_iter()).for_each(|(px, &w)| {
            if w > 0.0 {
                let y = luma(px);
                let f = 1.0 - w.min(1.0) * (1.0 - k);
                for v in px.iter_mut() {
                    *v = y + f * (*v - y);
                }
            }
        });
        return;
    }
    // smoothed chroma per channel (5 × 5 cells) and the smoothed brightness it is measured against
    let ys: Vec<f32> = cg.chunks_exact(3).map(luma).collect();
    let d: [Vec<f32>; 3] = std::array::from_fn(|c| box_blur(&cg.chunks_exact(3).zip(&ys).map(|(p, y)| p[c] - y).collect::<Vec<_>>(), gw, gh, 2));
    let yl = box_blur(&ys, gw, gh, 2);
    cg.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let w = wt[i].min(1.0);
        if w <= 0.0 || yl[i] <= 0.0 {
            return;
        }
        let dl = [d[0][i], d[1][i], d[2][i]];
        let chroma = (dl[0] * dl[0] + dl[1] * dl[1] + dl[2] * dl[2]).sqrt() / yl[i];
        let t = ((chroma - COLOUR_GATE.0) / (COLOUR_GATE.1 - COLOUR_GATE.0)).clamp(0.0, 1.0);
        let mut g = (k - 1.0) * w * t * t * (3.0 - 2.0 * t);
        // more colour as a larger distance from the brightest channel: that channel stays (a light cyan does not
        // clip to white and lose its print), the others fall, never below 0
        let top = px[0].max(px[1]).max(px[2]);
        for c in 0..3 {
            let gap = top - px[c];
            if gap > 0.0 {
                g = g.min(px[c].max(0.0) / gap);
            }
        }
        if g <= 0.0 {
            return;
        }
        for v in px.iter_mut() {
            *v = top - (top - *v) * (1.0 + g);
        }
    });
}

/// Relative smoothed chroma below which "Lichter-Farbe" adds no colour, and from which it adds the full amount.
const COLOUR_GATE: (f32, f32) = (0.08, 0.2);

/// Cells around the rebuilt ones that "Lichter-Farbe" reaches (grid cells, 2 pixels each).
const COLOUR_REACH: usize = 12;

/// Separable maximum of radius `r` (edges clamped).
fn max_filter(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; v.len()];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            row[x] = v[y * w + a..=y * w + b].iter().copied().fold(0.0, f32::max);
        }
    });
    let mut out = vec![0f32; v.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            row[x] = (a..=b).map(|yy| tmp[yy * w + x]).fold(0.0, f32::max);
        }
    });
    out
}

/// Separable box mean of radius `r` (edges clamped).
fn box_blur(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; v.len()];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            row[x] = v[y * w + a..=y * w + b].iter().sum::<f32>() / (b - a + 1) as f32;
        }
    });
    let mut out = vec![0f32; v.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            row[x] = (a..=b).map(|yy| tmp[yy * w + x]).sum::<f32>() / (b - a + 1) as f32;
        }
    });
    out
}

/// Smallest top-layer value (grid units) for the brightness slope of the top-layer detail.
const DETAIL_T_FLOOR: f32 = 16.0;

/// Denoised colour grid (normalised layer values, half the size of the layers).
pub struct GridColour {
    pub width: usize,
    pub height: usize,
    pub data: Vec<crate::quattro_nr::Px>,
    /// enlarge without over- and undershoot ([`crate::quattro_nr::enlarge_limited`]): grids with peaks far above
    /// their surroundings (merged bracket, clipped cells set to neutral)
    pub limited: bool,
    /// Weight of the rebuilt highlight areas per cell (0 … 1, [`crate::highlight_rebuild`]), where the rebuild ran:
    /// the highlight sliders act there
    pub highlight: Option<Arc<Vec<f32>>>,
}

/// Binning factor (neighbouring pixels averaged, processed and output at 1/n of the width and height):
/// `Auto` as in the camera maker's software = sd Quattro / sd Quattro H from capture ISO 800 / 1600 / 6400 → 2 / 3 / 4,
/// dp Quattro never; `On` the same steps for the dp Quattro too (own addition); `Off` and other cameras: 1.
pub fn binning_factor(cal: &Calibration, setting: crate::Binning) -> usize {
    use crate::Binning;
    let camera = match cal.model {
        crate::calib::Model::SdQuattro => setting != Binning::Off,
        crate::calib::Model::DpQuattro => setting == Binning::On,
        _ => false,
    };
    if !camera || !binning_enabled() || std::env::var("GRAINEO_BINNING").is_ok_and(|v| v == "0") {
        return 1;
    }
    binning_for_iso(cal.iso_gain * cal.sensor_iso)
}

/// Binning factor by capture ISO (thresholds 800 / 1600 / 6400 as in the camera maker's software).
pub fn binning_for_iso(capture_iso: f64) -> usize {
    match capture_iso {
        i if i >= 6400.0 => 4,
        i if i >= 1600.0 => 3,
        i if i >= 800.0 => 2,
        _ => 1,
    }
}

static BINNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Allow binning ([`binning_factor`]) in this program. Off by default: the app's geometry (crop, masks, retouching,
/// zoom) still assumes the full image size; the test program and export tools switch it on. `GRAINEO_BINNING=1`
/// switches it on as well.
pub fn enable_binning(on: bool) {
    BINNING.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn binning_enabled() -> bool {
    BINNING.load(std::sync::atomic::Ordering::Relaxed) || std::env::var("GRAINEO_BINNING").is_ok_and(|v| v == "1")
}

/// Binning factor for these settings (see [`binning_factor`]).
pub fn binning_for(cal: &Calibration, s: &crate::Settings) -> usize {
    binning_factor(cal, s.binning)
}

/// Can this image be binned at all (sd Quattro / sd Quattro H, dp Quattro with `On`)? Whether it takes effect
/// also depends on the capture ISO ([`binning_iso_factor`]).
pub fn binning_camera(cal: &Calibration) -> Option<BinningCamera> {
    match cal.model {
        crate::calib::Model::SdQuattro => Some(BinningCamera::Sd),
        crate::calib::Model::DpQuattro => Some(BinningCamera::Dp),
        _ => None,
    }
}

/// Camera class for the binning switch: sd Quattro (H) bin in the camera maker's software, the dp Quattro never.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinningCamera {
    Sd,
    Dp,
}

/// Factor the capture ISO of this image calls for (1 below ISO 800), regardless of the setting.
pub fn binning_iso_factor(cal: &Calibration) -> usize {
    binning_for_iso(cal.iso_gain * cal.sensor_iso)
}

/// Raw image binned `n × n` (block mean, rounded; remainder rows/columns dropped) for the own pipeline and the
/// sensor views of a binned session (Sigma's base bins its counts itself, see [`counts`]).
pub fn bin_raw(raw: &x3f::RawImage, n: usize) -> x3f::RawImage {
    let (bw, bh) = (raw.columns / n, raw.rows / n);
    let w = raw.columns;
    let planes = std::array::from_fn(|c| {
        let src = &raw.planes[c];
        (0..bw * bh)
            .into_par_iter()
            .map(|i| {
                let (bx, by) = (i % bw, i / bw);
                let mut s = 0u32;
                for y in by * n..by * n + n {
                    for x in bx * n..bx * n + n {
                        s += src[y * w + x] as u32;
                    }
                }
                let k = (n * n) as u32;
                ((s + k / 2) / k) as u16
            })
            .collect()
    });
    x3f::RawImage { columns: bw, rows: bh, planes }
}

/// Mean of `n × n` neighbouring pixels of `v` (image `w × h`); remainder rows/columns are dropped.
fn bin_values(w: usize, h: usize, n: usize, v: impl Fn(usize) -> [f32; 3] + Sync) -> (usize, usize, Vec<[f32; 3]>) {
    let (bw, bh) = (w / n, h / n);
    let inv = 1.0 / (n * n) as f32;
    let out = (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % bw, i / bw);
            let mut s = [0f32; 3];
            for y in by * n..by * n + n {
                for x in bx * n..bx * n + n {
                    let p = v(y * w + x);
                    for c in 0..3 {
                        s[c] += p[c];
                    }
                }
            }
            s.map(|a| a * inv)
        })
        .collect();
    (bw, bh, out)
}

/// Share of the base sharpening applied in the preparation where the rest is deferred (measured: sd Quattro
/// SDI0004 edge gradient p99: reference 7.66, with 0.5 → 7.55, without → 5.72).
const DEFERRED_SHARPEN: f64 = 0.5;

/// Sigma's third stage uses the lightness wavelet only below an ISO factor of 8 (capture ISO 800); from there on it
/// uses another (box filter) way that is not rebuilt, so the previous base sharpening stays there.
fn lce_applies(capture_iso: f64) -> bool {
    capture_iso < 800.0 && crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_LCE").map_or(true, |v| v != "0")
}

/// Share of the 16-bit code range above black that the brightest value of a merged bracket may take when its layers
/// are stored scaled up (room for the base sharpening and the detail of the top layer).
const MERGE_STORE_HEADROOM: f32 = 0.8;

/// Scene-linear RGB (interleaved, `camera_to_rgb` applied) from replaced sensor values: top layer at full resolution,
/// bottom and middle as their grey share of the top layer (`neutral`: layer value of grey per unit of the top layer,
/// normalised units) plus the 2× enlargement of the grid's difference to that grey, without over- and undershoot
/// ([`crate::quattro_nr::enlarge_limited`]): the highlights of a merged bracket lie far above their surroundings, and
/// a neutral peak must not tint them. `unit`: normalised layer value → units of the replacement.
fn replaced_scene(r: &crate::sfd::Replacement, unit: [f32; 3], rgb: &[f32; 9], neutral: [f32; 3]) -> Vec<f32> {
    let (w, h) = (r.tw, r.th);
    // grey in the units of the replacement: layer value per unit of the top layer
    let grey: [f32; 3] = std::array::from_fn(|c| neutral[c] * unit[c] / unit[2]);
    let d: [Vec<f32>; 2] = std::array::from_fn(|c| {
        let g: Vec<f32> = r.grid.iter().map(|p| p[c] - grey[c] * p[2]).collect();
        crate::quattro_nr::enlarge_limited(&g, r.gw, r.gh, w, h)
    });
    let mut out = vec![0f32; w * h * 3];
    out.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let t = r.top[i];
        let x = [(grey[0] * t + d[0][i]) / unit[0], (grey[1] * t + d[1][i]) / unit[1], t / unit[2]];
        for k in 0..3 {
            px[k] = rgb[k * 3] * x[0] + rgb[k * 3 + 1] * x[1] + rgb[k * 3 + 2] * x[2];
        }
    });
    out
}

/// Base sharpening strength by ISO: SPP and the camera noticeably sharpen Quattro images even at
/// sharpness 0; less at high ISO so that noise is not emphasised.
fn capture_sharpen_amount(iso: f64) -> f64 {
    let stops = (iso.max(100.0) / 100.0).log2(); // 0 at ISO 100, 6 at ISO 6400
    (1.0 - 0.12 * stops).clamp(0.25, 1.0)
}

/// Base sharpening (unsharp mask, radius σ = 0.8 px) on the luminance of the linear
/// image; all three channels are scaled by the same factor, so no
/// colour fringes appear. Halos are tamed by limiting the factor (0.6 … 1.6).
fn capture_sharpen(rgb: &mut [f32], w: usize, h: usize, amount: f32) {
    let y: Vec<f32> = rgb.par_chunks(3).map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).max(0.0)).collect();
    let sigma = 0.8f32;
    let r = 3usize;
    let k: Vec<f32> = {
        let v: Vec<f32> = (0..=2 * r).map(|i| (-((i as f32 - r as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
        let s: f32 = v.iter().sum();
        v.into_iter().map(|x| x / s).collect()
    };
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
        let src = &y[row * w..(row + 1) * w];
        for x in 0..w {
            let mut acc = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = (x + i).saturating_sub(r).min(w - 1);
                acc += kv * src[xx];
            }
            out[x] = acc;
        }
    });
    let blur: Vec<f32> = (0..h)
        .into_par_iter()
        .flat_map_iter(|row| {
            let (tmp, k) = (&tmp, &k);
            (0..w).map(move |x| {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let yy = (row + i).saturating_sub(r).min(h - 1);
                    acc += kv * tmp[yy * w + x];
                }
                acc
            })
        })
        .collect();
    rgb.par_chunks_mut(3).zip(y.par_iter().zip(blur.par_iter())).for_each(|(p, (&l, &b))| {
        if l > 1e-5 {
            let f = (1.0 + amount * (l - b) / l).clamp(0.6, 1.6);
            p[0] *= f;
            p[1] *= f;
            p[2] *= f;
        }
    });
}

#[cfg(test)]
mod clipped_cell_tests {
    use super::*;

    #[test]
    fn cells_with_a_clipped_top_pixel_lose_their_colour() {
        let (gw, gh, w) = (6, 6, 12);
        let neutral = [0.5f32, 0.6, 1.0];
        // yellowish cells (lower layers above the grey of the top mean), top layer at 0.2 of the range
        let mut grid = vec![[0.3f32, 0.3, 0.2]; gw * gh];
        let mut top = vec![0.2f32; w * 2 * gh];
        // one top pixel of cell (2, 2) at the saturation level 0.5
        top[2 * 2 * w + 2 * 2 + 1] = 0.5;
        grid[2 * gw + 2][2] = 0.275;
        let n = neutral_clipped_cells(&mut grid, &top, w, gw, gh, 0.5, neutral);
        assert_eq!(n, 9, "the cell and its eight neighbours");
        // a cell that reads bluer than grey keeps its colour (the clipped top mean is a lower bound)
        let mut blue = vec![[0.05f32, 0.08, 0.2]; gw * gh];
        assert_eq!(neutral_clipped_cells(&mut blue, &top, w, gw, gh, 0.5, neutral), 0);
        assert!(blue.iter().all(|p| *p == [0.05, 0.08, 0.2]));
        // the clipped cell: grey of its own top mean, the top mean untouched
        let c = grid[2 * gw + 2];
        assert!((c[0] - 0.275 * 0.5).abs() < 1e-6 && (c[1] - 0.275 * 0.6).abs() < 1e-6 && c[2] == 0.275);
        // a neighbour: grey of its top mean
        let c = grid[gw + 1];
        assert!((c[0] - 0.1).abs() < 1e-6 && (c[1] - 0.12).abs() < 1e-6 && c[2] == 0.2);
        // two cells away: as before
        assert_eq!(grid[0], [0.3, 0.3, 0.2]);
        assert_eq!(grid[4 * gw + 4], [0.3, 0.3, 0.2]);
        // below the start of the glide nothing changes
        let mut grid = vec![[0.3f32, 0.3, 0.2]; gw * gh];
        top[2 * 2 * w + 2 * 2 + 1] = 0.5 * CLIPPED_CELL_START;
        assert_eq!(neutral_clipped_cells(&mut grid, &top, w, gw, gh, 0.5, neutral), 0);
        assert!(grid.iter().all(|p| *p == [0.3, 0.3, 0.2]));
    }
}

#[cfg(test)]
mod binning_tests {
    use super::*;

    #[test]
    fn factor_by_capture_iso() {
        for (iso, n) in [(100.0, 1), (500.0, 1), (799.0, 1), (800.0, 2), (1250.0, 2), (1600.0, 3), (3200.0, 3), (6400.0, 4), (12800.0, 4)] {
            assert_eq!(binning_for_iso(iso), n, "ISO {iso}");
        }
    }

    #[test]
    fn binning_averages_and_sizes() {
        // 5424 × 3616 (sd Quattro) at ×4 → 1356 × 904 as in the reference output; remainder dropped
        let (w, h) = (10, 7);
        let (bw, bh, v) = bin_values(w, h, 3, |i| [i as f32, 1.0, (i % w) as f32]);
        assert_eq!((bw, bh), (3, 2));
        assert_eq!(v.len(), 6);
        // first block: rows 0..3, columns 0..3 → mean index (0+1+2+10+11+12+20+21+22)/9 = 11
        assert!((v[0][0] - 11.0).abs() < 1e-4 && (v[0][1] - 1.0).abs() < 1e-6 && (v[0][2] - 1.0).abs() < 1e-6);
        assert_eq!((5424 / 4, 3616 / 4), (1356, 904));
    }

    #[test]
    fn raw_binning_rounds_the_block_mean() {
        // 5 × 3, n = 2 → 2 × 1; last column and row dropped
        let p: Vec<u16> = (0..15).collect();
        let raw = x3f::RawImage { columns: 5, rows: 3, planes: [p.clone(), p.clone(), vec![7; 15]] };
        let b = bin_raw(&raw, 2);
        assert_eq!((b.columns, b.rows), (2, 1));
        // (0+1+5+6)/4 = 3, (2+3+7+8)/4 = 5
        assert_eq!(b.planes[0], vec![3, 5]);
        assert_eq!(b.planes[2], vec![7, 7]);
    }
}

#[cfg(test)]
mod highlight_tests {
    use super::*;

    /// Encoded layer: `n` pixels, a share `clipped` piled up at `sat` (± 0.008), the rest spread over 0 … `top`.
    fn layer(n: usize, clipped: f32, sat: f32, top: f32) -> Vec<u16> {
        let enc = |x: f32| (ENC_BLACK as f32 + x * ENC_SCALE as f32).round() as u16;
        (0..n)
            .map(|i| {
                let r = ((i as u32).wrapping_mul(2_654_435_761) >> 8) as f32 / (1u32 << 24) as f32;
                if (i % 1000) as f32 / 1000.0 < clipped { enc(sat + (r - 0.5) * 0.016) } else { enc(r * top) }
            })
            .collect()
    }

    #[test]
    fn saturation_found_below_the_stored_range() {
        // sd Quattro H with digital gain 4/4/2: the layers saturate at ≈ 0.455 / 0.43 / 0.535
        let n = 400_000;
        let s = layer_saturation(&[layer(n, 0.6, 0.455, 0.4), layer(n, 0.6, 0.43, 0.38), layer(n, 0.6, 0.535, 0.5)]);
        for (got, want) in s.iter().zip([0.455, 0.43, 0.535]) {
            assert!((want - 0.012..=want).contains(got), "{s:?}");
        }
        // few clipped pixels (0.1 % of a 4 MP layer) are found as well
        let n = 4_000_000;
        let s = layer_saturation(&[layer(n, 0.001, 0.455, 0.3), layer(n, 0.0, 0.0, 0.3), layer(n, 0.0, 0.0, 0.3)]);
        assert!((0.443..=0.455).contains(&s[0]) && s[1] == 1.0 && s[2] == 1.0, "{s:?}");
    }

    #[test]
    fn no_saturation_without_pile_up_or_at_the_top_of_the_range() {
        let n = 400_000;
        // spread bright values without a pile-up
        assert_eq!(layer_saturation(&[layer(n, 0.0, 0.0, 0.7), layer(n, 0.0, 0.0, 1.1), layer(n, 0.0, 0.0, 0.5)]), [1.0; 3]);
        // saturation at the top of the stored range (dp Quattro, sd Quattro): unchanged 1
        assert_eq!(layer_saturation(&[layer(n, 0.3, 1.0, 0.8), layer(n, 0.3, 0.995, 0.8), layer(n, 0.3, 1.0, 0.8)]), [1.0; 3]);
    }

    #[test]
    fn blown_pixel_turns_neutral() {
        // white point at the top layer's saturation (0.535 white-balanced); lower layers above it, top capped
        let white = 0.535;
        let out = highlight_pixel([0.92, 0.88, 0.537], white, CLASSIC_HL_START as f32).unwrap();
        assert!(out.iter().all(|v| (v - white).abs() < 1e-6), "{out:?}");
        // bright coloured pixel (lowest layer far below the white point) keeps its colour
        let out = highlight_pixel([0.5, 0.2, 0.1], white, CLASSIC_HL_START as f32).unwrap();
        assert_eq!(out, [0.5, 0.2, 0.1]);
        // below the start nothing changes
        assert!(highlight_pixel([0.3, 0.3, 0.3], white, CLASSIC_HL_START as f32).is_none());
    }
}


#[cfg(test)]
mod highlight_slider_tests {
    use super::*;

    fn cells(gw: usize, gh: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Vec<f32> {
        (0..gw * gh).flat_map(|i| f(i % gw, i / gw)).collect()
    }

    fn chroma(p: &[f32]) -> f32 {
        let y = 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
        p.iter().map(|v| (v - y).abs()).fold(0.0, f32::max) / y
    }

    #[test]
    fn colour_zero_is_neutral_inside_the_area() {
        let (gw, gh) = (24, 24);
        let mut cg = cells(gw, gh, |x, y| if (x + y) % 2 == 0 { [1.2, 1.0, 0.9] } else { [0.9, 1.0, 1.1] });
        let cw: Vec<f32> = (0..gw * gh).map(|i| if (6..18).contains(&(i % gw)) && (6..18).contains(&(i / gw)) { 1.0 } else { 0.0 }).collect();
        highlight_colour(&mut cg, &cw, gw, gh, 0.0);
        // well inside the area every cell is neutral, whatever its noise was (no patches)
        for y in 9..15 {
            for x in 9..15 {
                let i = (y * gw + x) * 3;
                assert!(chroma(&cg[i..i + 3]) < 1e-4, "cell {x},{y}");
            }
        }
    }

    #[test]
    fn more_colour_leaves_near_neutral_noise_alone() {
        let (gw, gh) = (24, 24);
        // nearly neutral surface with small colour noise: unchanged at the largest factor
        let src = cells(gw, gh, |x, y| if (x * 7 + y * 3) % 5 == 0 { [1.03, 1.0, 0.98] } else { [0.99, 1.0, 1.01] });
        let cw = vec![1.0f32; gw * gh];
        let mut cg = src.clone();
        highlight_colour(&mut cg, &cw, gw, gh, crate::highlight_rebuild::COLOUR_MAX);
        assert_eq!(cg, src);
        // a clearly coloured surface gains colour
        let src = cells(gw, gh, |_, _| [1.4, 0.9, 0.8]);
        let mut cg = src.clone();
        highlight_colour(&mut cg, &cw, gw, gh, crate::highlight_rebuild::COLOUR_MAX);
        assert!(chroma(&cg[0..3]) > 1.4 * chroma(&src[0..3]));
    }
}
