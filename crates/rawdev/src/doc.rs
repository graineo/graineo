//! An opened non-Sigma RAW with cached stages.
//!
//! - On open only the **preview** (binned per color instead of demosaicing, ≈ 2 MP, see
//!   [`raw::RawFile::basis`] with [`raw::development::Size::Preview`]) together with the matching
//!   noise profile. Slider moves only compute [`crate::render`] on this image.
//! - Noise reduction and lens correction of the preview ([`RawDoc::preview_stage`]) are computed once
//!   per slider state and cached.
//! - Full resolution (demosaic RCD/Markesteijn, capture sharpening) only when zooming and
//!   exporting ([`RawDoc::full_stage`]); [`RawDoc::release_memory`] releases it when switching images.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use develop::look::Geo;
use develop::{Rgb16, Settings};
use raw::development::{Demosaic, Size};
use raw::color::{self, M3};
use raw::lens::{Correction, LensOptions};
use raw::noise::{self, image_noise_model, image_profile, NoiseReduction};
#[cfg(test)]
use raw::noise::image_profile_exact;
use rayon::prelude::*;

/// Longest edge of the preview (≈ 2.8 MP at 3:2).
pub const PREVIEW_EDGE: usize = 2048;

/// Detail stages of the preview kept per image (≈ 34 MB each at 2048 px).
const PREVIEW_STAGES: usize = 2;

/// Linear, white-balanced camera RGB of one processing stage.
pub struct Stage {
    pub px: Vec<[f32; 3]>,
    pub w: usize,
    pub h: usize,
    pub profile: Option<noise::Profile>,
    /// luminance map (log2, soft) for shadows/highlights – computed on demand
    mask: std::sync::OnceLock<Vec<f32>>,
    /// dehaze analysis of this stage – computed on demand
    haze: std::sync::OnceLock<develop::dehaze::HazeField>,
    /// base layer for the noise brake (interleaved RGB) – computed on demand
    brake_base: std::sync::OnceLock<Vec<f32>>,
    /// tone equalizer analysis of this stage for the last mask setting – computed on demand
    toneq: Mutex<Option<Arc<develop::tone_eq::Grid>>>,
    /// GPU output renders of this stage so far, and the stage kept on the GPU from the second one on (slider moves on
    /// the preview; a stage rendered only once – preloading, thumbnails – does not occupy GPU memory)
    gpu_renders: std::sync::atomic::AtomicU32,
    gpu_resident: Mutex<Option<Arc<crate::gpu::Resident>>>,
}

impl Stage {
    pub(crate) fn new(px: Vec<[f32; 3]>, w: usize, h: usize, profile: Option<noise::Profile>) -> Self {
        Self {
            px,
            w,
            h,
            profile,
            mask: std::sync::OnceLock::new(),
            haze: std::sync::OnceLock::new(),
            brake_base: std::sync::OnceLock::new(),
            toneq: Mutex::new(None),
            gpu_renders: std::sync::atomic::AtomicU32::new(0),
            gpu_resident: Mutex::new(None),
        }
    }

    /// Output stage on the GPU for this stage; from the second call on a preview-sized stage stays on the GPU (see
    /// `gpu_resident`).
    fn gpu_output(&self, out_data: &crate::gpu::OutputData) -> Option<crate::gpu::Developed> {
        let resident = {
            let mut g = self.gpu_resident.lock().unwrap_or_else(|e| e.into_inner());
            // preview sizes only: a full-resolution stage (≈ 300 MB at 24 MP) is not kept twice
            let small = self.w.max(self.h) <= PREVIEW_EDGE;
            if small && g.is_none() && self.gpu_renders.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1 {
                *g = crate::gpu::upload(&self.px, self.w, self.h).map(Arc::new);
            }
            g.clone()
        };
        match resident {
            Some(r) => crate::gpu::output_resident(&r, out_data),
            None => crate::gpu::output(&self.px, self.w, self.h, out_data),
        }
    }

    /// Release the copy on the GPU (switching images).
    fn release_gpu(&self) {
        *self.gpu_resident.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.gpu_renders.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Dehaze analysis of the whole stage (see `develop::dehaze`).
    pub fn haze(&self) -> &develop::dehaze::HazeField {
        self.haze.get_or_init(|| {
            let flat: Vec<f32> = self.px.iter().flatten().copied().collect();
            develop::dehaze::HazeField::analyze(&flat, self.w, self.h, 1.0)
        })
    }

    /// Tone equalizer analysis of the whole stage for the mask setting of `s` (see `develop::tone_eq`); the values are
    /// linear camera RGB, white = 1 before the exposure.
    pub fn toneq(&self, s: &Settings) -> Arc<develop::tone_eq::Grid> {
        let mut g = self.toneq.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(grid) = g.as_ref().filter(|g| g.fits(&s.tone_eq)) {
            return grid.clone();
        }
        let flat: Vec<f32> = self.px.iter().flatten().copied().collect();
        let grid = Arc::new(develop::tone_eq::Grid::analyze(&flat, self.w, self.h, 1.0, &s.tone_eq));
        *g = Some(grid.clone());
        grid
    }

    /// Base layer of the stage for the noise brake (see `develop::lift::base_layer`).
    pub fn brake_base(&self) -> &[f32] {
        self.brake_base.get_or_init(|| {
            let flat: Vec<f32> = self.px.iter().flatten().copied().collect();
            develop::lift::base_layer(&flat, self.w, self.h, 6, 3.0)
        })
    }

    /// Crop `[x, y, w, h]` of the brake base layer.
    pub fn brake_base_crop(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<f32> {
        let b = self.brake_base();
        (y..y + h).flat_map(|yy| b[(yy * self.w + x) * 3..(yy * self.w + x + w) * 3].iter().copied()).collect()
    }

    /// Soft luminance map (log2 of the luminance before exposure): heavily smoothed, with
    /// some pixel contribution, so that shadows/highlights act locally without wide halos.
    pub fn mask(&self, lum: [f32; 3]) -> &[f32] {
        self.mask.get_or_init(|| {
            let _t = crate::timings::Stage::new("CPU Helligkeitskarte");
            let (w, h) = (self.w, self.h);
            let l: Vec<f32> = self.px.par_iter().map(|p| (lum[0] * p[0] + lum[1] * p[1] + lum[2] * p[2]).max(1e-7).log2()).collect();
            // downscale to ≈ 128 px, smooth, bilinear back
            let k = (w.max(h) / 128).max(1);
            let (sw, sh) = (w.div_ceil(k), h.div_ceil(k));
            let mut small = vec![0f32; sw * sh];
            small.par_chunks_mut(sw).enumerate().for_each(|(by, row)| {
                for (bx, o) in row.iter_mut().enumerate() {
                    let (mut s, mut n) = (0f32, 0f32);
                    for y in by * k..((by + 1) * k).min(h) {
                        for x in bx * k..((bx + 1) * k).min(w) {
                            s += l[y * w + x];
                            n += 1.0;
                        }
                    }
                    *o = s / n.max(1.0);
                }
            });
            let mut b = small;
            for _ in 0..3 {
                b = crate::render::box_blur(&b, sw, sh, 2);
            }
            let mut out = vec![0f32; w * h];
            out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                let fy = ((y as f32 + 0.5) / k as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
                let (y0, ty) = (fy as usize, fy.fract());
                let y1 = (y0 + 1).min(sh - 1);
                for (x, o) in row.iter_mut().enumerate() {
                    let fx = ((x as f32 + 0.5) / k as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
                    let (x0, tx) = (fx as usize, fx.fract());
                    let x1 = (x0 + 1).min(sw - 1);
                    let a = b[y0 * sw + x0] * (1.0 - tx) + b[y0 * sw + x1] * tx;
                    let c = b[y1 * sw + x0] * (1.0 - tx) + b[y1 * sw + x1] * tx;
                    let smooth = a * (1.0 - ty) + c * ty;
                    let p = l[y * w + x];
                    *o = smooth + 0.35 * (p - smooth).clamp(-2.0, 2.0);
                }
            });
            out
        })
    }
}

/// Key of the detail stage (noise reduction, lens correction).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DetailKey {
    nr: [u32; 4],
    lens: Option<(u32, u32, bool)>,
    /// effective assignment of an own lens profile ([`pin_hash`]; 0 = automatic)
    pin: u64,
    /// Apple path: rendering of the engine (version, white balance; [`apple::AppleDev::key`]); 0 = graineo
    apple: u64,
    /// neural denoising in percent ([`crate::ai_denoise`]); 0 = off
    pub(crate) ai: u32,
    /// capture sharpening at full resolution (off for the linear rendering)
    sharpen: bool,
}

/// Registry that turns the assignment text (`"id@version"`, `""`) into a number for the `Copy` key and back.
fn pins() -> &'static Mutex<std::collections::HashMap<u64, String>> {
    static P: std::sync::OnceLock<Mutex<std::collections::HashMap<u64, String>>> = std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

fn pin_hash(pin: Option<&str>) -> u64 {
    let Some(t) = pin else { return 0 };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in t.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let h = h | 1; // never 0
    if let Ok(mut m) = pins().lock() {
        m.entry(h).or_insert_with(|| t.to_string());
    }
    h
}

fn pin_text(h: u64) -> Option<String> {
    if h == 0 {
        return None;
    }
    pins().lock().ok().and_then(|m| m.get(&h).cloned())
}

impl DetailKey {
    pub fn from_settings(s: &Settings) -> Self {
        let r = &s.raw;
        Self {
            nr: [r.nr_luma, r.nr_luma_detail, r.nr_color, r.nr_color_detail].map(|v| v.round().clamp(0.0, 100.0) as u32),
            lens: r.lens.then(|| ((r.lens_distortion * 100.0).round() as u32, (r.lens_vignette * 100.0).round() as u32, r.lens_ca)),
            pin: pin_hash(r.lens_profile.as_deref()),
            apple: 0,
            ai: crate::ai_denoise::percent(s),
            sharpen: s.raw.look != crate::learn::LINEAR_LOOK,
        }
    }
    fn noise(&self) -> NoiseReduction {
        NoiseReduction { luminance: self.nr[0] as f32, luminance_detail: self.nr[1] as f32, color: self.nr[2] as f32, color_detail: self.nr[3] as f32 }
    }
}

pub struct RawDoc {
    pub path: PathBuf,
    /// Path under which the image is listed in the library (Lightroom catalog, XMP): for an
    /// archived RAW opened from the cache the original location, otherwise equal to `path`
    pub logical_path: PathBuf,
    pub info: raw::Info,
    /// camera orientation in quarter turns clockwise (EXIF orientation; mirroring is dropped)
    pub orientation: u8,
    /// size at full resolution (cropped)
    pub full_size: [usize; 2],
    pub preview: Arc<Stage>,
    /// picture style of the shot and the matching bundled look (`camera/style`); the look is only offered
    /// ("As camera"), never applied by itself
    pub picture_style: Option<raw::picture_style::PictureStyle>,
    pub auto_look: Option<String>,
    /// key of a learned look for this shot (`camera/style`, see [`raw::learn_look::style_key`]); the learned look of
    /// that key, when switched on, is the standard development of an unedited (neutral) photo ([`crate::learn`])
    pub learn_key: Option<String>,
    /// plain development without any per-camera colour correction (no learned look, no hue table, no saturation
    /// factor): the base learned looks are fitted on ([`crate::learn`])
    pub plain: std::sync::atomic::AtomicBool,
    /// all bundled looks of this camera
    pub looks: Vec<String>,
    /// as-shot white balance as (Kelvin, tint)
    pub as_shot: (f32, f32),
    /// automatic white balance (camera neutral, from the preview)
    pub auto_neutral: [f32; 3],
    /// base exposure of the standard development in EV (without sliders); see [`Self::base_ev_for`]
    pub base_ev: f32,
    /// part of `base_ev` that Lightroom does not apply (FinePix X100 dynamic range modes): photos with Lightroom start
    /// values subtract it
    pub(crate) lr_offset: f32,
    pub(crate) cam_xyz_shot: M3,
    /// built-in neutral correction of the standard development (X100, GR III), linear sRGB
    pub(crate) std_look: Option<Arc<raw::development::LookLut>>,
    /// shooting data for the UI
    pub info_rows: Vec<(String, String)>,
    pub summary: String,
    /// finished image (JPEG/HEIC/TIFF/PNG): without demosaic, tone curve, lens profile, camera look
    pub finished: bool,
    /// HDR gain map (iPhone etc.): brightening per preview pixel relative to the SDR image
    pub hdr: Option<Arc<HdrMap>>,
    file: Mutex<Option<raw::RawFile>>,
    full: Mutex<Option<Arc<Stage>>>,
    full_detail: Mutex<Option<(DetailKey, Arc<Stage>)>>,
    /// held while the full resolution is being computed: a second request (zoom while it is prepared in the
    /// background) waits for the first instead of computing it again
    full_busy: Mutex<()>,
    /// the last [`PREVIEW_STAGES`] detail stages of the preview, newest first: a short look at another noise-reduction or
    /// lens state (hovering a preset, version strip) does not throw away the stage the sliders work on
    preview_detail: Mutex<Vec<(DetailKey, Arc<Stage>)>>,
    correction: Mutex<Option<(DetailKey, Option<Arc<Correction>>, Option<&'static str>)>>,
    looks_cache: Mutex<Vec<(String, Arc<raw::looks::Look>)>>,
    /// tone curve matched to the camera's rendering (`learn::MATCHED_LOOK`), once computed
    matched: Mutex<Option<Result<Arc<raw::looks::Look>, String>>>,
    /// Apple path: the system RAW engine decodes instead of graineo ([`apple`]); `None` = graineo
    pub apple: Option<Arc<apple::AppleDev>>,
    /// the value of `RawAdjust::engine` this document was opened for ([`Self::engine_fits`])
    pub engine_request: Option<String>,
    /// neural denoising of this photo ([`crate::ai_denoise`]), computed once at full resolution
    pub(crate) ai: crate::ai_denoise::Caches,
    /// merged photo (HDR): the pixels come from the merge, camera data from the reference frame ([`crate::merged`])
    pub merged: Option<Arc<Merged>>,
}

/// Version of [`CameraData`] (a file of another version is not read).
const CAMERA_DATA_VERSION: u32 = 1;

/// What the development of a merged photo takes from its reference frame beyond the pixels: sensor and colour data,
/// shooting data, looks. Stored beside every computed result (`<key>-camera.json`), so that the result opens and
/// exports when the reference file is gone. Not stored: what only the file itself can give (the lens correction
/// data of the camera, DNG profile tables).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CameraData {
    version: u32,
    format: String,
    maker: String,
    model: String,
    mode: String,
    width: usize,
    height: usize,
    channels: usize,
    bits: usize,
    xtrans: bool,
    cfa: String,
    cfa_size: (usize, usize),
    black: Vec<f32>,
    black_pattern: (usize, usize, usize),
    white: Vec<f32>,
    wb: Option<[f32; 3]>,
    matrix: Option<(String, [[f32; 3]; 3])>,
    matrix_illuminants: Vec<String>,
    orientation: u16,
    active: Option<[usize; 4]>,
    crop: Option<[usize; 4]>,
    baseline_exposure: f32,
    iso: Option<u32>,
    exposure_time: Option<f32>,
    aperture: Option<f32>,
    focal_length: Option<f32>,
    distance: Option<f32>,
    lens_maker: Option<String>,
    lens: Option<String>,
    lens_db: Option<String>,
    capture_date: Option<String>,
    serial_number: Option<String>,
    calibration: Vec<(f32, M3, Option<M3>)>,
    analog_balance: Option<[f32; 3]>,
    fuji_dr: Option<u16>,
    full_size: [usize; 2],
    style: Option<(String, String, String)>,
    auto_look: Option<String>,
    learn_key: Option<String>,
    as_shot: (f32, f32),
    base_ev: f32,
    lr_offset: f32,
    cam_xyz_shot: M3,
    info_rows: Vec<(String, String)>,
    summary: String,
}

impl CameraData {
    /// Is this a file of the version this build reads?
    pub(crate) fn readable(&self) -> bool {
        self.version == CAMERA_DATA_VERSION
    }
}

/// The merge behind a merged document.
#[derive(Debug)]
pub struct Merged {
    pub source: crate::merged::MergedSource,
    pub meta: crate::merged::Meta,
    /// the reference frame is not there: the camera data comes from the cache ([`CameraData`]) and only what the cache
    /// holds can be shown – nothing is merged anew, the lens correction of the file is not available
    pub offline: bool,
    /// the document was opened for an output the user asked for (export, print, copy): its full resolution is merged
    /// also when the user cancelled that result before. The document on screen is not: what it needs by itself (the
    /// sharp picture, previews) never starts a merge the user stopped.
    pub asked: std::sync::atomic::AtomicBool,
    /// the merge is not computed yet: the pixels are those of the reference frame until it is there (the app starts
    /// the computation and puts the result in place)
    pub pending: bool,
}

pub(crate) fn quarters(o: u16) -> u8 {
    match o {
        6 | 5 => 1,
        3 | 4 => 2,
        8 | 7 => 3,
        _ => 0,
    }
}

/// Non-Sigma RAW (extension)?
pub fn is_raw(p: &Path) -> bool {
    library::is_raw_extension(p.extension().and_then(|e| e.to_str()).unwrap_or(""))
}

/// Finished image (JPEG, HEIC, TIFF, PNG) that can be edited here?
pub fn is_image(p: &Path) -> bool {
    library::is_finished_image_extension(p.extension().and_then(|e| e.to_str()).unwrap_or(""))
}

/// lensfun for finished images: found profile and why it applies (or not) automatically.
#[derive(Clone, Debug)]
pub struct LensStatus {
    pub profile: Option<String>,
    pub reason: &'static str,
    pub default_on: bool,
}

/// HDR gain map as a factor map at preview size: log2(HDR/SDR) of the luminance per pixel. The
/// development acts on the SDR image; the display on an EDR screen multiplies the
/// developed (linear) image by 2^(map · k), k = available/maximum headroom (≤ 1).
#[derive(Debug)]
pub struct HdrMap {
    pub w: usize,
    pub h: usize,
    pub log2: Vec<f32>,
    /// largest brightening (linear, e.g. 3.8)
    pub headroom: f32,
    pub iso: bool,
    /// log2 headroom of base and alternate (for the weight per screen)
    pub base_headroom: f32,
    pub alt_headroom: f32,
}

impl HdrMap {
    fn read(path: &Path, _sdr: &[[f32; 3]], w: usize, h: usize) -> Option<HdrMap> {
        let info = library::imageio::hdr_info(path)?;
        let gm = library::imageio::gain_map(path)?;
        // to preview size (bilinear; the map is smooth)
        let mut log2 = Vec::with_capacity(w * h);
        for y in 0..h {
            let fy = ((y as f32 + 0.5) * gm.h as f32 / h as f32 - 0.5).clamp(0.0, (gm.h - 1) as f32);
            let (y0, ty) = (fy.floor() as usize, fy.fract());
            let y1 = (y0 + 1).min(gm.h - 1);
            for x in 0..w {
                let fx = ((x as f32 + 0.5) * gm.w as f32 / w as f32 - 0.5).clamp(0.0, (gm.w - 1) as f32);
                let (x0, tx) = (fx.floor() as usize, fx.fract());
                let x1 = (x0 + 1).min(gm.w - 1);
                let v = |xx: usize, yy: usize| gm.log2[yy * gm.w + xx];
                let a = v(x0, y0) + tx * (v(x1, y0) - v(x0, y0));
                let b = v(x0, y1) + tx * (v(x1, y1) - v(x0, y1));
                log2.push(a + ty * (b - a));
            }
        }
        Some(HdrMap { w, h, log2, headroom: info.headroom.max(gm.alternate_headroom.exp2()), iso: info.iso, base_headroom: gm.base_headroom, alt_headroom: gm.alternate_headroom })
    }

    /// Weight of the map for a screen with `display_headroom` (linear headroom, 1 = SDR):
    /// ISO 21496-1, linear in log2 between base and alternate headroom.
    pub fn weight(&self, display_headroom: f32) -> f32 {
        let d = (self.alt_headroom - self.base_headroom).max(1e-3);
        ((display_headroom.max(1.0).log2() - self.base_headroom) / d).clamp(0.0, 1.0)
    }
}

/// Editable with `rawdev` (RAW or finished image)?
pub fn is_editable(p: &Path) -> bool {
    is_raw(p) || is_image(p)
}

/// Info for a finished image: linear sRGB as "camera RGB" (matrix XYZ → sRGB, white balance
/// neutral), so that the same chain (white balance by the DNG method, working space) applies.
fn info_for_image(path: &Path, w: usize, h: usize, o: u32) -> raw::Info {
    let e = library::imageio::properties(path).unwrap_or_default();
    let xyz_srgb = color::inv(&color::srgb_to_xyz_d65()).expect("sRGB");
    let format = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_uppercase();
    raw::Info {
        format,
        maker: e.maker.clone().unwrap_or_default(),
        model: e.camera.clone().unwrap_or_default(),
        mode: String::new(),
        width: w,
        height: h,
        channels: 3,
        bits: 16,
        sensor: raw::Sensor::Linear,
        cfa: String::new(),
        cfa_size: (0, 0),
        black: Vec::new(),
        black_pattern: (1, 1, 1),
        white: vec![1.0],
        wb: Some([1.0; 3]),
        matrix: Some(("D65".into(), xyz_srgb)),
        matrix_illuminants: vec!["D65".into()],
        orientation: o as u16,
        active: None,
        crop: None,
        fuji_rotated: false,
        baseline_exposure: 0.0,
        dng_profile: Vec::new(),
        exif: raw::Exif {
            iso: e.iso.map(|v| v as u32),
            exposure_time: e.time.map(|v| v as f32),
            aperture: e.aperture.map(|v| v as f32),
            focal_length: e.focal.map(|v| v as f32),
            distance: None,
            lens_maker: None,
            lens: e.lens.clone(),
            lens_db: None,
            capture_date: e.capture.clone(),
            serial_number: None,
        },
        corrections: Vec::new(),
        calibration: vec![color::Calibration { kelvin: 6504.0, color: xyz_srgb, forward: None }],
        analog_balance: None,
        fuji_dr: None,
    }
}

/// Error text of an output of a merged photo whose result is not there and cannot be computed.
pub const MERGE_NOT_READY: &str = "HDR-Ergebnis fehlt im Cache";

/// The document for an output: `first` as opened; when it stands in for something still to be computed
/// (`pending` names it) that is computed and the document opened again. A document still pending after that is an
/// error, never the output.
fn settle_for_output<D, P>(first: Option<D>, pending: impl Fn(&D) -> Option<P>, compute: impl FnOnce(&P) -> Result<(), String>, reopen: impl FnOnce() -> Result<Option<D>, String>) -> Result<Option<D>, String> {
    let Some(doc) = first else { return Ok(None) };
    let Some(p) = pending(&doc) else { return Ok(Some(doc)) };
    drop(doc);
    compute(&p)?;
    match reopen()? {
        Some(d) if pending(&d).is_some() => Err(MERGE_NOT_READY.into()),
        other => Ok(other),
    }
}

impl RawDoc {
    /// Is this the document of a merged photo whose result is not there yet – the reference frame standing in while
    /// the app computes the merge? Its pixels are a single exposure: they may be shown while waiting, but are never
    /// stored or handed out as a picture of the merged photo (grid and large previews, copies, the remote view).
    pub fn stands_in(&self) -> bool {
        self.merged.as_ref().is_some_and(|m| m.pending)
    }

    /// Open: decode, preview, white balance values, picture style. `keep_going`: abort while browsing. Automatic
    /// engine (graineo, Apple only for files graineo cannot decode); see [`Self::open_for`].
    pub fn open(path: &Path, keep_going: &dyn Fn() -> bool) -> Result<Option<RawDoc>, String> {
        Self::open_for(path, &Settings::default(), keep_going)
    }

    /// Open with the engine of the settings (`RawAdjust::engine`, see [`apple::Choice`]); the white balance of the
    /// settings is the first rendering of the Apple path.
    pub fn open_for(path: &Path, s: &Settings, keep_going: &dyn Fn() -> bool) -> Result<Option<RawDoc>, String> {
        let request = s.raw.engine.clone();
        let mut doc = match Self::open_engine(path, s, keep_going) {
            Ok(Some(d)) => d,
            other => return other,
        };
        doc.engine_request = request;
        Ok(Some(doc))
    }

    /// Open for an output (export, print): like [`Self::open_for`], but a merged photo whose result is not in the cache
    /// is merged first (blocking, from its sources) – the reference frame that stands in while the app computes is
    /// never the output. When the merge cannot be computed the error says so.
    pub fn open_for_output(path: &Path, s: &Settings, keep_going: &dyn Fn() -> bool) -> Result<Option<RawDoc>, String> {
        let open = || Self::open_for(path, s, keep_going);
        let doc = settle_for_output(
            open()?,
            |d| d.merged.as_ref().filter(|m| m.pending).map(|m| m.source.clone()),
            // (through the app's one entrance for merges: never beside another merge)
            |src| crate::merged::compute_output(src, true).map(|_| ()),
            open,
        )?;
        if let Some(m) = doc.as_ref().and_then(|d| d.merged.as_ref()) {
            m.asked.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(doc)
    }

    fn open_engine(path: &Path, s: &Settings, keep_going: &dyn Fn() -> bool) -> Result<Option<RawDoc>, String> {
        // a merged photo has no file of its own (library file without a file on disk): the recipe decides, not the
        // disk – a file that got onto its path (an export into the folder of the sources) is never taken for it.
        // The library is asked only for a path that is not there or carries the name of a merged photo.
        if !path.exists() || library::merge::has_merge_name(path) {
            if let Some(src) = crate::merged::lookup(path) {
                return Self::open_merged(path, src, keep_going);
            }
        }
        if is_image(path) {
            return Self::open_image(path).map(Some);
        }
        let choice = apple::Choice::parse(s.raw.engine.as_deref());
        if let apple::Choice::Apple(v) = &choice {
            if apple::available() {
                // why graineo cannot decode the file (header only, no pixel data)
                let blocked = raw::check_decodable(path).err().map(|e| match e {
                    raw::Error::Unsupported(why) => why,
                    e => e.to_string(),
                });
                match apple::open(path, s, v.as_deref(), apple::Reason::Chosen, blocked) {
                    Ok(d) => return Ok(Some(d)),
                    // not supported (any more) or failed: graineo, the entry stays (the UI says so)
                    Err(e) => eprintln!("apple raw {}: {e} – graineo", path.display()),
                }
            }
        }
        let t = std::time::Instant::now();
        let f = match raw::RawFile::open(path) {
            Ok(f) => f,
            // a file graineo cannot decode (Nikon High Efficiency, implausible decoder result): the system engine, if it
            // knows the file
            Err(raw::Error::Unsupported(why)) if (why == raw::NIKON_HE || why == raw::DECODE_IMPLAUSIBLE) && apple::available() => {
                return match apple::open(path, s, None, apple::Reason::Fallback, Some(why.clone())) {
                    Ok(d) => Ok(Some(d)),
                    Err(e) => {
                        eprintln!("apple raw fallback {}: {e}", path.display());
                        Err(raw::Error::Unsupported(why).to_string())
                    }
                };
            }
            Err(e) => return Err(e.to_string()),
        };
        crate::timings::lap("CPU read RAW file", t.elapsed());
        if !keep_going() {
            return Ok(None);
        }
        let info = f.info.clone();
        let b = f.basis(Demosaic::Auto, true, true, Size::Preview(PREVIEW_EDGE)).map_err(|e| e.to_string())?;
        // stages of the binned preview for measurements (no-op unless `timings::collect` is on)
        for (n, d) in &b.timings {
            crate::timings::lap(&format!("CPU {n}"), *d);
        }
        if !keep_going() {
            return Ok(None);
        }
        let wb = info.wb.unwrap_or([1.0; 3]);
        let n_shot = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
        let as_shot = crate::wb::temp_from_neutral(&info, n_shot).unwrap_or((5500.0, 0.0));
        let cam_xyz_shot = f.color_dng(wb).map(|c| c.camera_to_xyz_d50).unwrap_or(color::diag([1.0; 3]));
        let auto_neutral = auto_neutral(&b.px, wb).unwrap_or(n_shot);
        let mut base_ev = raw::development::BASE_EV + raw::development::camera_ev(&info.maker, &info.model) + info.baseline_exposure;
        base_ev += raw::development::fuji_compensation(&info.maker, &info.model, info.exif.iso, info.fuji_dr);
        let lr_offset = raw::development::fuji_lightroom_offset(&info.maker, &info.model, info.exif.iso, info.fuji_dr);
        let std_look = raw::development::camera_look(&info.maker, &info.model).map(|l| Arc::new(raw::development::LookLut::new(&l)));
        let picture_style = raw::picture_style::read(path);
        let short = raw::picture_style::camera_short(&info.maker, &info.model);
        let looks: Vec<String> = crate::looks::names_for_camera(&short);
        let auto_look = picture_style.as_ref().map(|b| format!("{}/{}", b.camera_short(), b.style)).filter(|n| looks.contains(n));
        let learn_key = picture_style.as_ref().filter(|b| raw::learn_look::learnable_style(b)).map(|b| format!("{}/{}", b.camera_short(), raw::learn_look::style_key(b)));
        let (info_rows, summary) = info_texts(&info, picture_style.as_ref(), as_shot);
        let full_size = [b.full.0, b.full.1];
        let preview = Arc::new(Stage::new(b.px, b.width, b.height, b.profile));
        if std::env::var_os("GRAINEO_LOG").is_some() {
            eprintln!("raw open {}: {:.0?} ({}×{} preview, {:?})", path.display(), t.elapsed(), preview.w, preview.h, b.timings);
        }
        Ok(Some(RawDoc {
            path: path.to_path_buf(),
            logical_path: path.to_path_buf(),
            orientation: quarters(info.orientation),
            info,
            full_size,
            preview,
            picture_style,
            auto_look,
            learn_key,
            plain: Default::default(),
            looks,
            as_shot,
            auto_neutral,
            base_ev,
            lr_offset,
            cam_xyz_shot,
            std_look,
            info_rows,
            summary,
            finished: false,
            hdr: None,
            file: Mutex::new(Some(f)),
            full: Mutex::new(None),
            full_detail: Mutex::new(None),
            full_busy: Mutex::new(()),
            preview_detail: Mutex::new(Vec::new()),
            correction: Mutex::new(None),
            looks_cache: Mutex::new(Vec::new()),
            matched: Mutex::new(None),
            apple: None,
            engine_request: None,
            ai: Default::default(),
            merged: None,
        }))
    }

    /// Open a finished image (JPEG, HEIC, TIFF, PNG): linear, without tone curve and camera look.
    fn open_image(path: &Path) -> Result<RawDoc, String> {
        let (w, h, px, o) = library::imageio::linear_full(path).ok_or_else(|| format!("{}: Bild nicht lesbar", path.display()))?;
        let info = info_for_image(path, w, h, o);
        let noise_model = image_noise_model(&px, w, h);
        let f = (PREVIEW_EDGE as f32 / w.max(h) as f32).min(1.0);
        let (pw, ph) = (((w as f32 * f).round() as usize).max(1), ((h as f32 * f).round() as usize).max(1));
        let small = raw::development::downscale(&px, w, h, pw, ph);
        let profile = noise_model.map(|m| image_profile(m, f));
        let cam_xyz_shot = color::determine([1.0; 3], &info.calibration, &color::diag([1.0; 3])).map(|c| c.camera_to_xyz_d50).unwrap_or(color::diag([1.0; 3]));
        let (info_rows, summary) = info_texts(&info, None, crate::wb::image_white());
        let hdr = HdrMap::read(path, &small, pw, ph).map(Arc::new);
        let full = Arc::new(Stage::new(px, w, h, noise_model.map(|m| image_profile(m, 1.0))));
        Ok(RawDoc {
            path: path.to_path_buf(),
            logical_path: path.to_path_buf(),
            orientation: quarters(info.orientation),
            info,
            full_size: [w, h],
            preview: Arc::new(Stage::new(small, pw, ph, profile)),
            picture_style: None,
            auto_look: None,
            learn_key: None,
            plain: Default::default(),
            looks: Vec::new(),
            as_shot: crate::wb::image_white(),
            auto_neutral: [1.0; 3],
            base_ev: 0.0,
            lr_offset: 0.0,
            cam_xyz_shot,
            std_look: None,
            info_rows,
            summary,
            finished: true,
            hdr,
            file: Mutex::new(None),
            full: Mutex::new(Some(full)),
            full_detail: Mutex::new(None),
            full_busy: Mutex::new(()),
            preview_detail: Mutex::new(Vec::new()),
            correction: Mutex::new(None),
            looks_cache: Mutex::new(Vec::new()),
            matched: Mutex::new(None),
            apple: None,
            engine_request: None,
            ai: Default::default(),
            merged: None,
        })
    }

    /// Open a merged photo ([`crate::merged`]): the reference RAW for camera, colour, looks and lens; the pixels from the
    /// merge (cache, computed again from the sources when the cache is gone).
    fn open_merged(path: &Path, src: crate::merged::MergedSource, keep_going: &dyn Fn() -> bool) -> Result<Option<RawDoc>, String> {
        let reference = src.reference();
        let cached = crate::merged::read_meta(&src.key).and_then(|m| Some((m, crate::merged::read_preview(&src.key)?)));
        // the reference as an ordinary RAW: camera data, white balance, looks, lens. When it is not there (the source
        // was deleted, its drive is not connected) a result in the cache still opens, with the camera data stored
        // beside it – the finished picture does not depend on its sources.
        let opened = if !reference.is_file() {
            Err(format!("{}: Quelle fehlt", reference.display()))
        } else if crate::foveon_frames::is_foveon(&reference) {
            // Foveon: the reference's camera data and colour; its pixels never stand in (they are not developed here)
            Self::foveon_reference(&reference).map(Some)
        } else {
            Self::open_engine(&reference, &Settings::default(), keep_going)
        };
        let (mut doc, offline) = match opened {
            Ok(Some(d)) => (d, false),
            Ok(None) => return Ok(None),
            Err(e) => match cached.as_ref().and_then(|_| crate::merged::read_camera(&src.key)) {
                Some(c) => (Self::from_camera(c, &reference), true),
                None => return Err(e),
            },
        };
        if doc.apple.is_some() || doc.finished {
            return Err("HDR: Referenzbild nicht als Mosaik lesbar".into());
        }
        let Some((meta, pv)) = cached else {
            // not computed yet (a merge just started, or the cache is gone): the reference stands in, never a wait here;
            // the app computes the merge and puts it in place (`hdr_live.rs`)
            let meta = crate::merged::Meta { summary: hdr::Summary { reference: crate::merged::reference_index(src.settings.reference, || None, src.sources.len()), ev: Vec::new(), shift: Vec::new(), warnings: Vec::new() }, headroom: 1.0, algo: hdr::ALGO_VERSION, pattern: [[1; 6]; 6], base: None };
            doc.path = path.to_path_buf();
            doc.logical_path = path.to_path_buf();
            doc.summary = merged_summary(&doc.summary, src.sources.len());
            if doc.preview.w == 0 || doc.preview.h == 0 {
                // Foveon reference: no pixels of its own. A small neutral grey frame with the photo's aspect stands in
                // until the merge arrives (an empty stage would break the preview, histograms and tone map).
                let [fw, fh] = doc.full_size;
                let long = fw.max(fh).max(1) as f32;
                let (w, h) = (((fw as f32 / long) * 64.0).round().max(1.0) as usize, ((fh as f32 / long) * 64.0).round().max(1.0) as usize);
                doc.preview = Arc::new(Stage::new(vec![[0.18; 3]; w * h], w, h, None));
            }
            doc.merged = Some(Arc::new(Merged { source: src, meta, pending: true, offline: false, asked: Default::default() }));
            return Ok(Some(doc));
        };
        if !offline {
            // (kept beside the result for the day the reference is gone)
            crate::merged::write_camera(&src.key, &doc.camera_data());
        }
        let profile = crate::merged::profile(&meta, &doc.info, pv.noise, pv.w, doc.full_size[0]);
        let wb = doc.info.wb.unwrap_or([1.0; 3]);
        doc.auto_neutral = auto_neutral(&pv.px, wb).unwrap_or(doc.auto_neutral);
        doc.preview = Arc::new(Stage::new(pv.px, pv.w, pv.h, Some(profile)));
        doc.path = path.to_path_buf();
        doc.logical_path = path.to_path_buf();
        doc.summary = merged_summary(&doc.summary, src.sources.len());
        doc.merged = Some(Arc::new(Merged { source: src, meta, pending: false, offline, asked: Default::default() }));
        Ok(Some(doc))
    }

    /// The reference of a Foveon merge (X3F): camera data and colour as the general path reads them
    /// ([`crate::foveon_frames::info`]), no pixels (the merge supplies them).
    fn foveon_reference(reference: &Path) -> Result<RawDoc, String> {
        let s = develop::Session::open(reference).map_err(|e| format!("{}: {e}", reference.display()))?;
        let [w, h] = s.size();
        let info = crate::foveon_frames::info(reference, &s.cal, w, h).map_err(|e| e.to_string())?;
        drop(s);
        let wb = info.wb.unwrap_or([1.0; 3]);
        let n_shot = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
        let as_shot = crate::wb::temp_from_neutral(&info, n_shot).unwrap_or((5500.0, 0.0));
        let cam_xyz_shot = color::determine([1.0 / wb[0], 1.0 / wb[1], 1.0 / wb[2]], &info.calibration, &color::diag([1.0; 3])).map(|c| c.camera_to_xyz_d50).unwrap_or(color::diag([1.0; 3]));
        let (info_rows, summary) = info_texts(&info, None, as_shot);
        let i = &info;
        let c = CameraData {
            version: CAMERA_DATA_VERSION,
            format: i.format.clone(),
            maker: i.maker.clone(),
            model: i.model.clone(),
            mode: i.mode.clone(),
            width: i.width,
            height: i.height,
            channels: i.channels,
            bits: i.bits,
            xtrans: false,
            cfa: i.cfa.clone(),
            cfa_size: i.cfa_size,
            black: i.black.clone(),
            black_pattern: i.black_pattern,
            white: i.white.clone(),
            wb: i.wb,
            matrix: i.matrix.clone(),
            matrix_illuminants: i.matrix_illuminants.clone(),
            orientation: i.orientation,
            active: None,
            crop: None,
            baseline_exposure: 0.0,
            iso: i.exif.iso,
            exposure_time: i.exif.exposure_time,
            aperture: i.exif.aperture,
            focal_length: i.exif.focal_length,
            distance: None,
            lens_maker: None,
            lens: i.exif.lens.clone(),
            lens_db: None,
            capture_date: i.exif.capture_date.clone(),
            serial_number: i.exif.serial_number.clone(),
            calibration: i.calibration.iter().map(|c| (c.kelvin, c.color, c.forward)).collect(),
            analog_balance: None,
            fuji_dr: None,
            full_size: [w, h],
            style: None,
            auto_look: None,
            learn_key: None,
            as_shot,
            base_ev: raw::development::BASE_EV,
            lr_offset: 0.0,
            cam_xyz_shot,
            info_rows,
            summary,
        };
        Ok(Self::from_camera(c, reference))
    }

    /// What a merged photo needs of its reference frame beyond the pixels (see [`CameraData`]).
    pub(crate) fn camera_data(&self) -> CameraData {
        let i = &self.info;
        let region = |r: &raw::Region| [r.x, r.y, r.w, r.h];
        CameraData {
            version: CAMERA_DATA_VERSION,
            format: i.format.clone(),
            maker: i.maker.clone(),
            model: i.model.clone(),
            mode: i.mode.clone(),
            width: i.width,
            height: i.height,
            channels: i.channels,
            bits: i.bits,
            xtrans: i.sensor == raw::Sensor::XTrans,
            cfa: i.cfa.clone(),
            cfa_size: i.cfa_size,
            black: i.black.clone(),
            black_pattern: i.black_pattern,
            white: i.white.clone(),
            wb: i.wb,
            matrix: i.matrix.clone(),
            matrix_illuminants: i.matrix_illuminants.clone(),
            orientation: i.orientation,
            active: i.active.as_ref().map(region),
            crop: i.crop.as_ref().map(region),
            baseline_exposure: i.baseline_exposure,
            iso: i.exif.iso,
            exposure_time: i.exif.exposure_time,
            aperture: i.exif.aperture,
            focal_length: i.exif.focal_length,
            distance: i.exif.distance,
            lens_maker: i.exif.lens_maker.clone(),
            lens: i.exif.lens.clone(),
            lens_db: i.exif.lens_db.clone(),
            capture_date: i.exif.capture_date.clone(),
            serial_number: i.exif.serial_number.clone(),
            calibration: i.calibration.iter().map(|c| (c.kelvin, c.color, c.forward)).collect(),
            analog_balance: i.analog_balance,
            fuji_dr: i.fuji_dr,
            full_size: self.full_size,
            style: self.picture_style.as_ref().map(|p| (p.maker.clone(), p.model.clone(), p.style.clone())),
            auto_look: self.auto_look.clone(),
            learn_key: self.learn_key.clone(),
            as_shot: self.as_shot,
            base_ev: self.base_ev,
            lr_offset: self.lr_offset,
            cam_xyz_shot: self.cam_xyz_shot,
            info_rows: self.info_rows.clone(),
            summary: self.summary.clone(),
        }
    }

    /// A document without a file from stored camera data: for a merged photo whose reference frame is gone. The
    /// pixels are set by the caller.
    fn from_camera(c: CameraData, reference: &Path) -> RawDoc {
        let region = |r: [usize; 4]| raw::Region { x: r[0], y: r[1], w: r[2], h: r[3] };
        let foveon = c.format == "X3F";
        let info = raw::Info {
            format: c.format,
            maker: c.maker,
            model: c.model,
            mode: c.mode,
            width: c.width,
            height: c.height,
            channels: c.channels,
            bits: c.bits,
            // a Foveon reference (X3F) has no mosaic: three layers per pixel
            sensor: if foveon {
                raw::Sensor::Linear
            } else if c.xtrans {
                raw::Sensor::XTrans
            } else {
                raw::Sensor::Bayer
            },
            cfa: c.cfa,
            cfa_size: c.cfa_size,
            black: c.black,
            black_pattern: c.black_pattern,
            white: c.white,
            wb: c.wb,
            matrix: c.matrix,
            matrix_illuminants: c.matrix_illuminants,
            orientation: c.orientation,
            active: c.active.map(region),
            crop: c.crop.map(region),
            fuji_rotated: false,
            baseline_exposure: c.baseline_exposure,
            dng_profile: Vec::new(),
            exif: raw::Exif {
                iso: c.iso,
                exposure_time: c.exposure_time,
                aperture: c.aperture,
                focal_length: c.focal_length,
                distance: c.distance,
                lens_maker: c.lens_maker,
                lens: c.lens,
                lens_db: c.lens_db,
                capture_date: c.capture_date,
                serial_number: c.serial_number,
            },
            corrections: Vec::new(),
            calibration: c.calibration.into_iter().map(|(kelvin, color, forward)| color::Calibration { kelvin, color, forward }).collect(),
            analog_balance: c.analog_balance,
            fuji_dr: c.fuji_dr,
        };
        let wb = info.wb.unwrap_or([1.0; 3]);
        let std_look = raw::development::camera_look(&info.maker, &info.model).map(|l| Arc::new(raw::development::LookLut::new(&l)));
        let looks: Vec<String> = crate::looks::names_for_camera(&raw::picture_style::camera_short(&info.maker, &info.model));
        RawDoc {
            path: reference.to_path_buf(),
            logical_path: reference.to_path_buf(),
            orientation: quarters(info.orientation),
            full_size: c.full_size,
            preview: Arc::new(Stage::new(Vec::new(), 0, 0, None)),
            picture_style: c.style.map(|(maker, model, style)| raw::picture_style::PictureStyle { maker, model, style, details: Vec::new() }),
            auto_look: c.auto_look,
            learn_key: c.learn_key,
            plain: Default::default(),
            looks,
            as_shot: c.as_shot,
            auto_neutral: [wb[1] / wb[0], 1.0, wb[1] / wb[2]],
            base_ev: c.base_ev,
            lr_offset: c.lr_offset,
            cam_xyz_shot: c.cam_xyz_shot,
            std_look,
            info_rows: c.info_rows,
            summary: c.summary,
            finished: false,
            hdr: None,
            file: Mutex::new(None),
            full: Mutex::new(None),
            full_detail: Mutex::new(None),
            full_busy: Mutex::new(()),
            preview_detail: Mutex::new(Vec::new()),
            correction: Mutex::new(None),
            looks_cache: Mutex::new(Vec::new()),
            matched: Mutex::new(None),
            apple: None,
            engine_request: None,
            ai: Default::default(),
            merged: None,
            info,
        }
    }

    /// The same merged photo with other pixels (a change of its recipe): camera data, looks and orientation stay, the
    /// stages of the old pixels go. `full`: the full resolution, when it is there already (otherwise it is read from
    /// the cache or computed when needed).
    /// `preview`: `None` keeps the preview of this document (only the full resolution is new).
    pub fn with_merged(&self, preview: Option<&hdr::Merged>, full: Option<&hdr::Merged>, source: crate::merged::MergedSource, meta: crate::merged::Meta) -> RawDoc {
        let wb = self.info.wb.unwrap_or([1.0; 3]);
        let stage = preview.map(|p| {
            let prof = crate::merged::profile(&meta, &self.info, p.noise, p.w, self.full_size[0]);
            Arc::new(Stage::new(p.px.clone(), p.w, p.h, Some(prof)))
        });
        let full = full.map(|f| {
            let prof = crate::merged::profile(&meta, &self.info, f.noise, f.w, f.w);
            Arc::new(Stage::new(f.px.clone(), f.w, f.h, Some(prof)))
        });
        RawDoc {
            path: self.path.clone(),
            logical_path: self.logical_path.clone(),
            info: self.info.clone(),
            orientation: self.orientation,
            full_size: self.full_size,
            preview: stage.unwrap_or_else(|| self.preview.clone()),
            picture_style: self.picture_style.clone(),
            auto_look: self.auto_look.clone(),
            learn_key: self.learn_key.clone(),
            plain: std::sync::atomic::AtomicBool::new(self.plain.load(std::sync::atomic::Ordering::Relaxed)),
            looks: self.looks.clone(),
            as_shot: self.as_shot,
            auto_neutral: preview.and_then(|p| auto_neutral(&p.px, wb)).unwrap_or(self.auto_neutral),
            base_ev: self.base_ev,
            lr_offset: self.lr_offset,
            cam_xyz_shot: self.cam_xyz_shot,
            std_look: self.std_look.clone(),
            info_rows: self.info_rows.clone(),
            summary: merged_summary(&self.summary, source.sources.len()),
            finished: self.finished,
            hdr: None,
            file: Mutex::new(None),
            full: Mutex::new(full),
            full_detail: Mutex::new(None),
            full_busy: Mutex::new(()),
            preview_detail: Mutex::new(if preview.is_none() { self.preview_detail.lock().unwrap_or_else(|e| e.into_inner()).clone() } else { Vec::new() }),
            correction: Mutex::new(None),
            looks_cache: Mutex::new(self.looks_cache.lock().unwrap_or_else(|e| e.into_inner()).clone()),
            matched: Mutex::new(self.matched.lock().unwrap_or_else(|e| e.into_inner()).clone()),
            apple: None,
            engine_request: self.engine_request.clone(),
            ai: Default::default(),
            merged: Some(Arc::new(Merged { source, meta, pending: false, offline: self.merged.as_ref().is_some_and(|m| m.offline), asked: std::sync::atomic::AtomicBool::new(self.merged.as_ref().is_some_and(|m| m.asked.load(std::sync::atomic::Ordering::Relaxed))) })),
        }
    }

    /// Clip level of the stage for the capture sharpening (pixels at or above it are left alone): the white of the
    /// file, for a merged photo the white of its darkest frame.
    fn sharpen_clip(&self) -> f32 {
        let wb = self.info.wb.unwrap_or([1.0; 3]);
        0.95 * wb[1].min(wb[0]).min(wb[2]) * self.merged.as_ref().map_or(1.0, |m| m.meta.headroom.max(1.0))
    }

    /// Was this document opened for the engine the settings call for (`RawAdjust::engine`)? If not, the app opens the
    /// photo again. An automatic fallback whose version was pinned afterwards still fits.
    pub fn engine_fits(&self, s: &Settings) -> bool {
        if self.finished || s.raw.engine == self.engine_request {
            return true;
        }
        self.engine_request.is_none() && self.apple.as_ref().is_some_and(|a| a.reason == apple::Reason::Fallback && s.raw.engine.as_deref() == Some(a.pin_value().as_str()))
    }

    /// Release large intermediate states (full resolution, decoded file).
    pub fn release_memory(&self) {
        if let Some(a) = &self.apple {
            a.release();
        }
        for (_, st) in self.preview_detail.lock().unwrap().iter() {
            st.release_gpu();
        }
        *self.full.lock().unwrap() = None;
        *self.full_detail.lock().unwrap() = None;
        *self.file.lock().unwrap() = None;
        self.ai.release();
    }

    /// Base exposure for these settings: with Lightroom start values without the part Lightroom does not apply, so a
    /// photo edited in Lightroom keeps its brightness.
    pub fn base_ev_for(&self, s: &Settings) -> f32 {
        if s.raw.from_lightroom {
            self.base_ev - self.lr_offset
        } else {
            self.base_ev
        }
    }

    /// Camera neutral of the chosen white balance.
    pub fn target_neutral(&self, s: &Settings) -> Option<[f32; 3]> {
        // Apple path: the engine balanced already ([`apple::white_for`])
        if self.apple.is_some() {
            return None;
        }
        let r = &s.raw;
        match r.wb_mode.as_str() {
            "as_shot" | "" => None,
            "auto" => Some(self.auto_neutral),
            "custom" => crate::wb::neutral_for(&self.info, r.temperature, r.tint),
            v => crate::wb::preset(v).and_then(|(k, t)| crate::wb::neutral_for(&self.info, k, t)),
        }
    }

    /// (Kelvin, tint) of the chosen white balance (for the sliders).
    pub fn temp_tint(&self, s: &Settings) -> (f32, f32) {
        let r = &s.raw;
        match r.wb_mode.as_str() {
            "custom" => (r.temperature, r.tint),
            "auto" if self.apple.is_some() => self.as_shot,
            "auto" => crate::wb::temp_from_neutral(&self.info, self.auto_neutral).unwrap_or(self.as_shot),
            v => crate::wb::preset(v).unwrap_or(self.as_shot),
        }
    }

    /// Effective look name (or `None` = neutral). An unedited photo (`""`) starts neutral: the camera look
    /// ([`Self::auto_look`]) is applied only when it is chosen, then it is stored by name.
    pub fn look_name(&self, s: &Settings) -> Option<String> {
        match s.raw.look.as_str() {
            "" | "neutral" | crate::learn::LINEAR_LOOK => None,
            n => Some(n.to_string()),
        }
    }

    /// The learned look that is the standard of this shot's camera and style (switched on), with its strength.
    /// `None` for finished images, the plain development ([`Self::plain`]) and without a learned look.
    /// Linear scene rendering chosen ([`crate::learn::LINEAR_LOOK`]; RAWs only).
    pub fn is_linear(&self, s: &Settings) -> bool {
        !self.finished && s.raw.look == crate::learn::LINEAR_LOOK
    }

    pub fn learned_look(&self) -> Option<(Arc<raw::looks::Look>, f32)> {
        if self.finished || self.plain.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        crate::learn::active(self.learn_key.as_deref()?)
    }

    pub(crate) fn look_for(&self, s: &Settings) -> Option<Arc<raw::looks::Look>> {
        let name = self.look_name(s)?;
        if name == crate::learn::IDENTITY_LOOK {
            return Some(Arc::new(crate::learn::identity_look()));
        }
        if name == crate::learn::MATCHED_LOOK {
            return self.matched_look();
        }
        let cache_name = if name.starts_with("film:") { format!("{}:{name}",raw::film_profile::generation()) } else { name.clone() };
        let mut c = self.looks_cache.lock().unwrap();
        if let Some((_, l)) = c.iter().find(|(n, _)| *n == cache_name) {
            return Some(l.clone());
        }
        // `film:<UUID>`: film profile (Camera Raw look with RGB table) from the user's files
        // own looks in the data folder before bundled ones (`crate::looks`)
        let l = crate::looks::find(&name).or_else(|| raw::film_profile::look_for_names(&name).map(Arc::new))?;
        if name.starts_with("film:") {c.retain(|(n,_)| !n.contains(":film:"));}
        c.push((cache_name, l.clone()));
        Some(l)
    }

    /// The tone curve matched to the camera's rendering of this photo ([`crate::learn::MATCHED_LOOK`]), computed once
    /// per opened photo (no lock is held while it renders the plain development). `None` if it cannot be computed
    /// (no camera rendering, alignment too weak): the development stays neutral, [`Self::matched_error`] says why.
    pub fn matched_look(&self) -> Option<Arc<raw::looks::Look>> {
        if let Some(r) = self.matched.lock().unwrap().as_ref() {
            return r.as_ref().ok().cloned();
        }
        let r = crate::learn::matched_look(self).map(Arc::new);
        if let Err(e) = &r {
            eprintln!("matched curve {}: {e}", self.path.display());
        }
        let out = r.as_ref().ok().cloned();
        *self.matched.lock().unwrap() = Some(r);
        out
    }

    /// Why the matched tone curve could not be computed (after a try).
    pub fn matched_error(&self) -> Option<String> {
        self.matched.lock().unwrap().as_ref().and_then(|r| r.as_ref().err().cloned())
    }

    /// Lens correction for full size (cached) and its source ("Kamera", "lensfun" …).
    pub fn correction(&self, key: DetailKey) -> (Option<Arc<Correction>>, Option<&'static str>) {
        let mut g = self.correction.lock().unwrap();
        if let Some((k, corr, q)) = g.as_ref() {
            if k.lens == key.lens && k.pin == key.pin {
                return (corr.clone(), *q);
            }
        }
        let res = key.lens.and_then(|(d, v, ca)| {
            let o = LensOptions { distortion: d as f32 / 100.0, vignette: v as f32 / 100.0, lateral_ca: ca, backup_enabled: false, pinned: pin_text(key.pin), ..Default::default() };
            // (a merged photo whose reference file is gone: the lens data of the file is not there, lensfun stands in)
            if self.finished || self.apple.is_some() || self.merged.as_ref().is_some_and(|m| m.offline) {
                let i = &self.info;
                return raw::lens::lensfun_img(&i.maker, &i.model, i.exif.lens.as_deref(), i.exif.focal_length, i.exif.aperture, self.full_size[0], self.full_size[1], &o).map(|k| Arc::new(k.0));
            }
            let g = self.file_or_reopen()?;
            let k = g.as_ref()?.lens_correction(self.full_size[0], self.full_size[1], &o)?;
            Some(Arc::new(k))
        });
        let source = res.as_ref().and_then(|k| k.geometry.as_ref().or(k.vignette.as_ref()).map(|t| t.kind.short()));
        *g = Some((key, res.clone(), source));
        (res, source)
    }

    /// Effective assignment of an own lens profile: the photo's own before the rules (`""` = none, `None` = automatic).
    pub fn lens_pin(&self, s: &Settings) -> Option<String> {
        if self.finished {
            return None;
        }
        let i = &self.info;
        lensprof::assign::effective(&i.maker, &i.model, i.exif.lens.as_deref(), s.raw.lens_profile.as_deref())
    }

    /// Camera and lens name of the file as the lens assignment sees them.
    pub fn lens_identity(&self) -> (String, String, Option<String>) {
        let i = &self.info;
        (i.maker.clone(), i.model.clone(), i.exif.lens.clone())
    }

    /// Key of the detail stage; for finished images the lensfun decision applies
    /// ([`RawDoc::lens_status`]: automatically off until the user switches it on).
    pub fn detail_key(&self, s: &Settings) -> DetailKey {
        let mut k = DetailKey::from_settings(s);
        k.pin = pin_hash(self.lens_pin(s).as_deref());
        k.apple = self.apple.as_ref().map_or(0, |a| a.key(s));
        // neural denoising only where it can run (RAW decoded by graineo, module installed); otherwise off
        if k.ai > 0 && !(crate::ai_denoise::applicable(self) && crate::ai_denoise::module_ready()) {
            k.ai = 0;
        }
        if self.finished {
            let on = s.raw.lens_image.unwrap_or_else(|| self.lens_status().default_on);
            if !on {
                k.lens = None;
            } else if k.lens.is_none() {
                let r = &s.raw;
                k.lens = Some(((r.lens_distortion * 100.0).round() as u32, (r.lens_vignette * 100.0).round() as u32, r.lens_ca));
            }
        }
        k
    }

    /// lensfun for finished images: profile (if present), reason for the automatic decision.
    pub fn lens_status(&self) -> LensStatus {
        let i = &self.info;
        let profile = raw::lens::lensfun_profile(&i.maker, &i.model, i.exif.lens.as_deref(), i.exif.focal_length, i.exif.aperture).map(|p| p.1);
        let h = i.maker.to_ascii_lowercase();
        let phone = ["apple", "google", "samsung", "huawei", "xiaomi", "oneplus", "oppo", "vivo", "honor", "motorola", "nothing"].iter().any(|t| h.contains(t))
            || i.model.to_ascii_lowercase().contains("iphone");
        let corrected = ["fujifilm", "olympus", "om digital", "panasonic", "leica", "ricoh", "sony"].iter().any(|t| h.contains(t));
        let reason = if phone {
            "Aus: das Telefon hat das Bild schon korrigiert"
        } else if profile.is_none() {
            "Kein lensfun-Profil für Kamera und Objektiv"
        } else if corrected {
            "Aus: diese Kamera korrigiert ihre JPEGs selbst"
        } else if self.info.format.starts_with("TIF") {
            "Aus: TIFF meist aus einem RAW-Programm – nur einschalten, wenn Verzeichnung sichtbar ist"
        } else {
            "Aus: ob die Kamera schon korrigiert hat, steht nicht in der Datei – bei sichtbarer Verzeichnung einschalten"
        };
        LensStatus { profile, reason, default_on: false }
    }

    /// Source of the lens profile for the UI (without recomputing, if already known).
    pub fn lens_source(&self, s: &Settings) -> Option<&'static str> {
        let mut k = DetailKey::from_settings(s);
        k.pin = pin_hash(self.lens_pin(s).as_deref());
        if k.lens.is_none() {
            k.lens = Some((100, 100, true));
        }
        self.correction(k).1
    }

    /// Source of the lens profile, if already determined (without waiting or computing):
    /// `None` = not known yet, `Some(None)` = no profile.
    pub fn lens_source_known(&self) -> Option<Option<&'static str>> {
        let g = self.correction.try_lock().ok()?;
        g.as_ref().map(|(_, _, q)| *q)
    }

    fn file_or_reopen(&self) -> Option<std::sync::MutexGuard<'_, Option<raw::RawFile>>> {
        if self.finished || self.apple.is_some() {
            return None;
        }
        let mut g = self.file.lock().unwrap();
        if g.is_none() {
            // merged photo: the reference frame (camera data, lens profile)
            let p = self.merged.as_ref().map_or_else(|| self.path.clone(), |m| m.source.reference());
            *g = raw::RawFile::open(&p).ok();
        }
        Some(g)
    }

    fn detail(&self, base: &Stage, key: DetailKey, full: bool) -> Stage {
        let log = std::env::var_os("GRAINEO_LOG").is_some();
        let mut t = std::time::Instant::now();
        let mut lap_time = |name: &str| {
            if log {
                eprintln!("rawdev detail {}×{} {name}: {:.0?}", base.w, base.h, t.elapsed());
            }
            t = std::time::Instant::now();
        };
        let (w, h) = (base.w, base.h);
        // full resolution: all three steps on the GPU (CPU remains reference and fallback)
        if full {
            let sch = raw::sharpness::Sharpness::default();
            let corr_opt = if key.lens.is_some() { self.correction(key).0 } else { None };
            let fits = corr_opt.as_ref().is_none_or(|k| (k.width, k.height) == (w, h));
            let grid = corr_opt.as_ref().filter(|_| fits).map(|k| k.grid(lens_grid_step()));
            let job = crate::gpu::Job {
                noise: base.profile.as_ref().and_then(|p| raw::noise::parameter(p, &key.noise(), w, h)),
                sharpen: (!self.finished && key.sharpen).then(|| (sch.strength, raw::sharpness::kernel(sch.sigma), sch.iterations, sch.threshold, self.sharpen_clip())),
                lens: grid.as_ref(),
            };
            if let Some(px) = fits.then(|| crate::gpu::detail(&base.px, w, h, &job)).flatten() {
                lap_time("GPU: Rauschminderung, Grundschärfe, Objektiv");
                return Stage::new(px, w, h, None);
            }
        }
        let mut px = base.px.clone();
        lap_time("kopieren");
        if let Some(p) = &base.profile {
            let _t = crate::timings::Stage::new(if full { "CPU Rauschminderung" } else { "CPU Rauschminderung (Vorschau)" });
            noise::apply(&mut px, w, h, p, &key.noise());
            lap_time("Rauschminderung");
        }
        if full && !self.finished && key.sharpen {
            let _t = crate::timings::Stage::new("CPU Schärfe");
            raw::sharpness::apply(&mut px, w, h, &raw::sharpness::Sharpness::default(), self.sharpen_clip());
            lap_time("Grundschärfe");
        }
        if key.lens.is_some() {
            if let (Some(k), _) = self.correction(key) {
                let _t = crate::timings::Stage::new(if full { "CPU Objektiv" } else { "CPU Objektiv (Vorschau)" });
                k.apply_scaled(&mut px, w, h);
                lap_time("Objektiv");
            }
        }
        Stage::new(px, w, h, None)
    }

    /// Preview after noise reduction/lens correction (cached per slider state).
    /// Dehaze analysis for a rectangle of a stage of size `w × h`: always from the preview stage (one analysis for
    /// preview, crops, export and the GPU), resampled to the rectangle.
    pub fn haze_for(&self, s: &Settings, w: usize, h: usize, region: [usize; 4]) -> Option<develop::dehaze::HazeField> {
        (s.dehaze != 0.0).then(|| self.preview_stage(s).haze().resample(w, h, region))
    }

    /// Tone equalizer mask for a rectangle of a stage of size `w × h`: always from the preview stage (one analysis for
    /// preview, crops and export), resampled to the rectangle.
    pub fn toneq_for(&self, s: &Settings, w: usize, h: usize, region: [usize; 4]) -> Option<develop::tone_eq::Mask> {
        develop::tone_eq::needed(s).then(|| develop::tone_eq::Mask::resample(self.preview_stage(s).toneq(s), w, h, region))
    }

    /// Dehaze parameters for the GPU output stage (same analysis as [`Self::haze_for`]).
    fn gpu_haze(&self, s: &Settings) -> Option<crate::gpu::GpuHaze> {
        if s.dehaze == 0.0 {
            return None;
        }
        let pv = self.preview_stage(s);
        let f = pv.haze();
        let (omega, gain, remove, t0) = f.at(s.dehaze)?.parameters();
        Some(crate::gpu::GpuHaze { dark: f.dark().to_vec(), w: f.width, h: f.height, a: f.a, omega, gain, remove, t0 })
    }

    pub fn preview_stage(&self, s: &Settings) -> Arc<Stage> {
        let key = self.preview_key(s);
        {
            let mut c = self.preview_detail.lock().unwrap();
            if let Some(i) = c.iter().position(|(k, _)| *k == key) {
                let e = c.remove(i);
                let st = e.1.clone();
                c.insert(0, e);
                return st;
            }
        }
        let base = match &self.apple {
            Some(a) => a.preview_for(s).unwrap_or_else(|e| {
                eprintln!("apple raw preview {}: {e}", self.path.display());
                self.preview.clone()
            }),
            None if key.ai > 0 => crate::ai_denoise::preview(self, key.ai).unwrap_or_else(|| self.preview.clone()),
            None => self.preview.clone(),
        };
        let st = Arc::new(self.detail(&base, key, false));
        let mut c = self.preview_detail.lock().unwrap();
        c.retain(|(k, _)| *k != key);
        c.insert(0, (key, st.clone()));
        c.truncate(PREVIEW_STAGES);
        st
    }

    /// Key of the preview's detail stage: while the neural denoising is still being computed, the preview shows the
    /// photo without it ([`crate::ai_denoise`]).
    fn preview_key(&self, s: &Settings) -> DetailKey {
        let mut key = self.detail_key(s);
        if key.ai > 0 && !self.ai.ready() {
            key.ai = 0;
        }
        key
    }

    /// Does the preview detail stage already have this state?
    pub fn preview_ready(&self, s: &Settings) -> bool {
        let key = self.preview_key(s);
        self.preview_detail.lock().unwrap().iter().any(|(k, _)| *k == key)
    }

    /// Full resolution after demosaicing (cached).
    pub fn full_base(&self) -> Result<Arc<Stage>, String> {
        if self.apple.is_some() {
            return self.full_base_for(&Settings::default());
        }
        if let Some(v) = self.full.lock().unwrap().as_ref() {
            return Ok(v.clone());
        }
        let _busy = self.full_busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.full.lock().unwrap().as_ref() {
            return Ok(v.clone());
        }
        // (a merge not computed yet shows its reference: the full resolution is the reference's as well)
        if let Some(m) = self.merged.as_ref().filter(|m| !m.pending) {
            let key = &m.source.key;
            let asked = m.asked.load(std::sync::atomic::Ordering::Relaxed);
            if !crate::merged::cached(key) {
                crate::merged::compute_output(&m.source, asked)?;
            }
            let r = match crate::merged::read_full(key) {
                Some(r) => r,
                // the file is there and does not read (damaged): computed again, once
                None => {
                    crate::merged::forget_full(key);
                    crate::merged::compute_output(&m.source, asked)?;
                    crate::merged::read_full(key).ok_or(MERGE_NOT_READY)?
                }
            };
            let profile = crate::merged::profile(&m.meta, &self.info, r.noise, r.w, r.w);
            let st = Arc::new(Stage::new(r.px, r.w, r.h, Some(profile)));
            *self.full.lock().unwrap() = Some(st.clone());
            return Ok(st);
        }
        if self.finished {
            let (w, h, px, _) = library::imageio::linear_full(&self.path).ok_or("Bild nicht lesbar")?;
            let m = image_noise_model(&px, w, h);
            let st = Arc::new(Stage::new(px, w, h, m.map(|m| image_profile(m, 1.0))));
            *self.full.lock().unwrap() = Some(st.clone());
            return Ok(st);
        }
        let g = self.file_or_reopen().ok_or("Datei")?;
        let f = g.as_ref().ok_or("Datei nicht lesbar")?;
        // demosaic on the GPU where available (X-Trans: Markesteijn); CPU remains reference and fallback
        let gpu_demosaic = std::env::var("GRAINEO_GPU_DEMOSAIC").as_deref() != Ok("0") && crate::gpu::gpu().is_some();
        let hook = |m: &[f32], w: usize, h: usize, pat: [[u8; 6]; 6], d: Demosaic| -> Option<Vec<[f32; 3]>> {
            if gpu_demosaic && d == Demosaic::Markesteijn3 {
                crate::gpu::xtrans::markesteijn(m, w, h, pat)
            } else {
                None
            }
        };
        let b = f.base_with(Demosaic::Auto, true, true, Size::Full, Some(&hook)).map_err(|e| e.to_string())?;
        drop(g);
        for (n, t) in &b.timings {
            crate::timings::lap(&format!("CPU {n}"), *t);
        }
        let st = Arc::new(Stage::new(b.px, b.width, b.height, b.profile));
        *self.full.lock().unwrap() = Some(st.clone());
        Ok(st)
    }

    /// Full resolution of the raw stage for these settings: graineo's (independent of the settings) or the Apple
    /// engine's (rendered once per white balance).
    pub fn full_base_for(&self, s: &Settings) -> Result<Arc<Stage>, String> {
        match &self.apple {
            Some(a) => a.full_for(s),
            None => match self.detail_key(s).ai {
                0 => self.full_base(),
                // neural denoising: computed once at full resolution, mixed with the original
                p => crate::ai_denoise::base(self, p),
            },
        }
    }

    /// Full resolution after noise reduction, sharpening, lens correction (cached per slider state).
    pub fn full_stage(&self, s: &Settings) -> Result<Arc<Stage>, String> {
        let key = self.detail_key(s);
        if let Some((k, st)) = self.full_detail.lock().unwrap().as_ref() {
            if *k == key {
                return Ok(st.clone());
            }
        }
        let b = self.full_base_for(s)?;
        let _busy = self.full_busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, st)) = self.full_detail.lock().unwrap().as_ref() {
            if *k == key {
                return Ok(st.clone());
            }
        }
        let st = Arc::new(self.detail(&b, key, true));
        *self.full_detail.lock().unwrap() = Some((key, st.clone()));
        Ok(st)
    }

    /// Recompute the detail stage at full resolution, without cache (measurements, tests).
    pub fn full_stage_fresh(&self, s: &Settings) -> Result<Stage, String> {
        let b = self.full_base_for(s)?;
        Ok(self.detail(&b, self.detail_key(s), true))
    }

    /// Full resolution already available for this state?
    pub fn full_ready(&self, s: &Settings) -> bool {
        self.full_detail.lock().unwrap().as_ref().is_some_and(|(k, _)| *k == self.detail_key(s))
    }

    pub fn lum(&self) -> [f32; 3] {
        let ws = color::rec2020_to_xyz_d65();
        let m = color::mul(&color::xyz_d50_to(&ws), &self.cam_xyz_shot);
        // luminance of the working space as weights on the camera RGB
        let y = ws[1];
        [0, 1, 2].map(|c| y[0] * m[0][c] + y[1] * m[1][c] + y[2] * m[2][c])
    }

    /// Developed preview (with look stage), in sensor orientation. The output stage runs on the GPU (the same kernels as
    /// the export, see [`Self::render_stage_gpu`]); the CPU ([`Self::render_stage`]) computes without a GPU, with
    /// `GRAINEO_GPU_FUSION=0` and for slider states the GPU stage does not cover (finished images, black & white
    /// mixer, shadow tint).
    pub fn render_preview(&self, s: &Settings) -> Rgb16 {
        let st = self.preview_stage(s);
        if let Some(img) = self.render_stage_gpu(&st, s) {
            return img;
        }
        self.render_stage(&st, s)
    }

    /// Fully develop one stage (preview or full resolution).
    pub fn render_stage(&self, st: &Stage, s: &Settings) -> Rgb16 {
        let eff = self.effective_settings(s);
        let s = &*eff;
        let geo = Geo { full_w: self.full_size[0], full_h: self.full_size[1], off_x: 0, off_y: 0, scale: self.full_size[0] as f32 / st.w as f32, camera: self.orientation };
        let mask = st.mask(self.lum());
        // The noise brake acts on fine colour noise that a downscaled preview averages out anyway: only at
        // (nearly) full resolution, so slider changes in the fitted view stay fast.
        let no_brake;
        let s = if geo.scale > 1.5 && crate::render::brake_active(s) {
            no_brake = develop::Settings { noise_brake: develop::NoiseBrake::Off, ..s.clone() };
            &no_brake
        } else {
            s
        };
        let haze = self.haze_for(s, st.w, st.h, [0, 0, st.w, st.h]);
        let toneq = self.toneq_for(s, st.w, st.h, [0, 0, st.w, st.h]);
        let base = crate::render::brake_active(s).then(|| st.brake_base().to_vec());
        crate::render::render_finished_haze(self, &st.px, st.w, st.h, mask, s, geo, haze.as_ref(), toneq.as_ref(), base)
    }

    /// Downscaled detail stage (thumbnails in the profile browser).
    pub fn small(&self, s: &Settings, edge: usize) -> Stage {
        let st = self.preview_stage(s);
        let f = (edge as f32 / st.w.max(st.h) as f32).min(1.0);
        let (w, h) = (((st.w as f32 * f).round() as usize).max(1), ((st.h as f32 * f).round() as usize).max(1));
        Stage::new(raw::development::downscale(&st.px, st.w, st.h, w, h), w, h, None)
    }

    /// Scene-linear sRGB of a stage for the film simulation (thumbnails of all films).
    pub fn film_scene(&self, st: &Stage, s: &Settings) -> (develop::Linear, Geo) {
        let p = crate::render::Params::new(self, s);
        let mask = st.mask(self.lum());
        let geo = Geo { full_w: self.full_size[0], full_h: self.full_size[1], off_x: 0, off_y: 0, scale: self.full_size[0] as f32 / st.w as f32, camera: self.orientation };
        (crate::render::scene_linear(&p, &st.px, st.w, st.h, mask), geo)
    }

    /// Film grain on an enlarged, linear crop instead of enlarged output pixels.
    pub fn render_grain_region(&self,s:&Settings,region:[usize;4],factor:usize)->Result<Rgb16,String>{
        if factor<=1 || !s.film.is_active() || develop::local::active(s){return self.render_region(s,region)}
        let st=self.full_stage(s)?;let eff=self.effective_settings(s);let s=&*eff;
        let [x,y,w,h]=region;let x=x.min(st.w-1);let y=y.min(st.h-1);let w=w.max(1).min(st.w-x);let h=h.max(1).min(st.h-y);
        let m=develop::film::margin(&s.film,st.w,st.h);let ex=x.saturating_sub(m);let ey=y.saturating_sub(m);let ew=(x+w+m).min(st.w)-ex;let eh=(y+h+m).min(st.h)-ey;
        let map=st.mask(self.lum());let mut px=Vec::with_capacity(ew*eh*3);let mut mk=Vec::with_capacity(ew*eh*3);
        for yy in ey..ey+eh {for xx in ex..ex+ew{px.extend_from_slice(&st.px[yy*st.w+xx]);mk.extend_from_slice(&[map[yy*st.w+xx];3]);}}
        let make=|data|develop::Linear{ mono: false, haze: None, toneq: None,width:ew,height:eh,data,detail:None};
        let up=develop::grain_detail::upscale(&make(px),factor)?;let mask=develop::grain_detail::upscale(&make(mk),factor)?;
        let px:Vec<[f32;3]>=up.data.chunks_exact(3).map(|p|[p[0],p[1],p[2]]).collect();let mk:Vec<f32>=mask.data.chunks_exact(3).map(|p|p[0]).collect();
        let geo=develop::grain_detail::geometry([st.w,st.h],[ex,ey],self.orientation,factor);
        let img=crate::render::render_finished(self,&px,up.width,up.height,&mk,s,geo);
        Ok(crop_rgb(&img,(x-ex)*factor,(y-ey)*factor,w*factor,h*factor))
    }

    /// Crop at full resolution (x, y, w, h in image pixels).
    pub fn render_region(&self, s: &Settings, region: [usize; 4]) -> Result<Rgb16, String> {
        // local adjustments: crop plus their filter reach (`local::margin`); retouching needs the whole image
        if develop::local::active(s) && !develop::local::region_possible(s) {let full=self.render_full(s)?;return Ok(develop::local::crop(&full,region));}
        let st = self.full_stage(s)?;
        let eff = self.effective_settings(s);
        let s = &*eff;
        let [x, y, w, h] = region;
        let (x, y) = (x.min(st.w - 1), y.min(st.h - 1));
        let (w, h) = (w.min(st.w - x).max(1), h.min(st.h - y).max(1));
        // margin for film (halation) and texture
        let m = if s.film.is_active() { develop::film::margin(&s.film, st.w, st.h) } else { 8 };
        let m = m + develop::local::margin(s, [st.w, st.h]);
        let (ex, ey) = (x.saturating_sub(m), y.saturating_sub(m));
        let (ew, eh) = ((x + w + m).min(st.w) - ex, (y + h + m).min(st.h) - ey);
        let mask_full = st.mask(self.lum());
        let mut px = Vec::with_capacity(ew * eh);
        let mut mk = Vec::with_capacity(ew * eh);
        for yy in ey..ey + eh {
            px.extend_from_slice(&st.px[yy * st.w + ex..yy * st.w + ex + ew]);
            mk.extend_from_slice(&mask_full[yy * st.w + ex..yy * st.w + ex + ew]);
        }
        let geo = Geo { full_w: st.w, full_h: st.h, off_x: ex, off_y: ey, scale: 1.0, camera: self.orientation };
        let haze = self.haze_for(s, st.w, st.h, [ex, ey, ew, eh]);
        let toneq = self.toneq_for(s, st.w, st.h, [ex, ey, ew, eh]);
        let base = crate::render::brake_active(s).then(|| st.brake_base_crop(ex, ey, ew, eh));
        let img = crate::render::render_finished_haze(self, &px, ew, eh, &mk, s, geo, haze.as_ref(), toneq.as_ref(), base);
        Ok(if (ex, ey, ew, eh) == (x, y, w, h) { img } else { crop_rgb(&img, x - ex, y - ey, w, h) })
    }

    /// Full resolution, whole image (export).
    pub fn render_full(&self, s: &Settings) -> Result<Rgb16, String> {
        crate::film_profiles::check(s)?;
        if let Some(img) = self.render_full_gpu(s)? {
            return Ok(img);
        }
        let st = self.full_stage(s)?;
        // stage already computed (e.g. after a crop of the zoomed view): output stage on the GPU from it – the same
        // kernels as the fused path, so view and export match
        if let Some(img) = self.render_stage_gpu(&st, s) {
            return Ok(img);
        }
        Ok(self.render_stage(&st, s))
    }

    /// Output stage of a computed stage (preview or full resolution) on the GPU (as [`Self::render_stage`]). `None`: no
    /// GPU or a slider state only the CPU can do.
    pub fn render_stage_gpu(&self, st: &Stage, s: &Settings) -> Option<Rgb16> {
        if self.finished || std::env::var("GRAINEO_GPU_FUSION").as_deref() == Ok("0") || crate::gpu::gpu().is_none() {
            return None;
        }
        let eff = self.effective_settings(s);
        let se = &*eff;
        let geo = Geo { full_w: self.full_size[0], full_h: self.full_size[1], off_x: 0, off_y: 0, scale: self.full_size[0] as f32 / st.w as f32, camera: self.orientation };
        let t = std::time::Instant::now();
        let out_data = |se: &Settings, scene: bool| -> Option<crate::gpu::OutputData> {
            let mut o = crate::render::Params::new(self, se).gpu_data(self.lum())?;
            o.haze = self.gpu_haze(se);
            o.scene = scene;
            if geo.scale > 1.5 {
                o.brake = None;
            }
            Some(o)
        };
        let od = out_data(se, se.film.is_active())?;
        crate::timings::lap("Ausgabe: Parameter und Tabellen (CPU)", t.elapsed());
        let t = std::time::Instant::now();
        let developed = st.gpu_output(&od)?;
        crate::timings::lap("Ausgabe: GPU gesamt", t.elapsed());
        let mut img = match developed {
            crate::gpu::Developed::Encoded(data) => Rgb16 { width: st.w, height: st.h, data },
            // film: scene-linear sRGB from the GPU, then the film simulation (as render.rs `output`, blended against the
            // output without film below full intensity)
            crate::gpu::Developed::Scene(data) => {
                let base = || {
                    let mut n = se.clone();
                    n.film.active = false;
                    n.profile_amount = 1.0;
                    st.gpu_output(&out_data(&n, false)?)?.encoded()
                };
                self.film_from_scene(se, data, st.w, st.h, geo, base)?
            }
        };
        let t = std::time::Instant::now();
        if self.info.sensor == raw::Sensor::XTrans && se.raw.false_color > 0.0 && crate::render::full_resolution(geo) {
            crate::false_color::suppress(&mut img, se.raw.false_color);
        }
        if se.raw.texture != 0.0 {
            crate::render::structure(&mut img, se.raw.texture, geo.scale);
        }
        develop::look::apply(&mut img, se, geo);
        crate::timings::lap("Ausgabe: Nachstufe (CPU)", t.elapsed());
        Some(img)
    }

    /// Film simulation on the scene-linear sRGB the GPU delivered ([`crate::gpu::OutputData::scene`]), as render.rs
    /// `output` + `render_finished_haze`: below full intensity blended against `base` (the output without film).
    /// `None`: the film failed or the base is missing – the caller computes on the CPU (which reports the error).
    fn film_from_scene(&self, se: &Settings, data: Vec<f32>, w: usize, h: usize, geo: Geo, base: impl FnOnce() -> Option<Vec<u16>>) -> Option<Rgb16> {
        let scene = develop::Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
        let mut img = develop::film::develop(&scene, &se.film, geo).ok()?;
        if se.profile_amount < 1.0 {
            let b = Rgb16 { width: w, height: h, data: base()? };
            develop::profile::blend(&mut img, &b, se.profile_amount.clamp(0.0, 1.0));
        }
        Some(img)
    }

    /// Full resolution in one go on the GPU: noise reduction, base sharpening, lens and the whole
    /// output stage (luminance map, tone, color, look) without an intermediate stage on the CPU. `None`: not possible
    /// (no GPU, finished image, film below full intensity, stage already computed) – then the CPU computes as before.
    /// Texture and the look stage (curve, HSL, color grading, effects) run on the CPU afterwards.
    fn render_full_gpu(&self, s: &Settings) -> Result<Option<Rgb16>, String> {
        if self.finished || std::env::var("GRAINEO_GPU_FUSION").as_deref() == Ok("0") || crate::gpu::gpu().is_none() || self.full_ready(s) {
            return Ok(None);
        }
        let eff = self.effective_settings(s);
        let se = &*eff;
        // film below full intensity also needs the output without film: via the computed stage (render_stage_gpu)
        if se.film.is_active() && se.profile_amount < 1.0 {
            return Ok(None);
        }
        let key = self.detail_key(s);
        // X-Trans: mosaic directly to the GPU (demosaic, detail, output without a detour via the CPU)
        // (not for a merged photo: its pixels are not the file's mosaic)
        if self.info.sensor == raw::Sensor::XTrans && key.ai == 0 && self.merged.is_none() && std::env::var("GRAINEO_GPU_DEMOSAIC").as_deref() != Ok("0") {
            if let Some(img) = self.render_full_gpu_mosaic(s, se, key)? {
                return Ok(Some(img));
            }
        }
        let b = self.full_base_for(s)?;
        let (w, h) = (b.w, b.h);
        let sch = raw::sharpness::Sharpness::default();
        let corr_opt = if key.lens.is_some() { self.correction(key).0 } else { None };
        if !corr_opt.as_ref().is_none_or(|k| (k.width, k.height) == (w, h)) {
            return Ok(None);
        }
        let grid = corr_opt.as_ref().map(|k| k.grid(lens_grid_step()));
        let job = crate::gpu::Job {
            noise: b.profile.as_ref().and_then(|p| raw::noise::parameter(p, &key.noise(), w, h)),
            sharpen: key.sharpen.then(|| (sch.strength, raw::sharpness::kernel(sch.sigma), sch.iterations, sch.threshold, self.sharpen_clip())),
            lens: grid.as_ref(),
        };
        let Some(mut out_data) = crate::render::Params::new(self, se).gpu_data(self.lum()) else { return Ok(None) };
        out_data.haze = self.gpu_haze(se);
        out_data.scene = se.film.is_active();
        let geo = Geo { full_w: self.full_size[0], full_h: self.full_size[1], off_x: 0, off_y: 0, scale: self.full_size[0] as f32 / w as f32, camera: self.orientation };
        let Some(developed) = crate::gpu::full(&b.px, w, h, &job, &out_data) else { return Ok(None) };
        let Some(mut img) = self.encoded_or_film(se, developed, w, h, geo) else { return Ok(None) };
        // X-Trans without the mosaic path (pattern not usable on the GPU, GRAINEO_GPU_DEMOSAIC=0): false colors as everywhere else
        if self.info.sensor == raw::Sensor::XTrans && se.raw.false_color > 0.0 {
            crate::false_color::suppress(&mut img, se.raw.false_color);
        }
        if se.raw.texture != 0.0 {
            crate::render::structure(&mut img, se.raw.texture, geo.scale);
        }
        develop::look::apply(&mut img, se, geo);
        Ok(Some(img))
    }

    /// Finished image of the fused GPU path: the encoded output, or with film the film simulation of the delivered scene
    /// (full intensity only, see [`Self::render_full_gpu`]).
    fn encoded_or_film(&self, se: &Settings, developed: crate::gpu::Developed, w: usize, h: usize, geo: Geo) -> Option<Rgb16> {
        match developed {
            crate::gpu::Developed::Encoded(data) => Some(Rgb16 { width: w, height: h, data }),
            crate::gpu::Developed::Scene(data) => self.film_from_scene(se, data, w, h, geo, || None),
        }
    }

    /// The mosaic path of [`RawDoc::render_full_gpu`] (X-Trans).
    fn render_full_gpu_mosaic(&self, s: &Settings, se: &Settings, key: DetailKey) -> Result<Option<Rgb16>, String> {
        let _ = s;
        let g = self.file_or_reopen().ok_or("Datei")?;
        let f = g.as_ref().ok_or("Datei nicht lesbar")?;
        let b = f.base_mosaic(true).map_err(|e| e.to_string())?;
        drop(g);
        for (n, t) in &b.timings {
            crate::timings::lap(&format!("CPU {n}"), *t);
        }
        let Some(m) = b.mosaic.as_ref() else { return Ok(None) };
        let (w, h) = (b.width, b.height);
        let sch = raw::sharpness::Sharpness::default();
        let corr_opt = if key.lens.is_some() { self.correction(key).0 } else { None };
        if !corr_opt.as_ref().is_none_or(|k| (k.width, k.height) == (w, h)) {
            return Ok(None);
        }
        let grid = corr_opt.as_ref().map(|k| k.grid(lens_grid_step()));
        let job = crate::gpu::Job {
            noise: b.profile.as_ref().and_then(|p| raw::noise::parameter(p, &key.noise(), w, h)),
            sharpen: key.sharpen.then(|| (sch.strength, raw::sharpness::kernel(sch.sigma), sch.iterations, sch.threshold, self.sharpen_clip())),
            lens: grid.as_ref(),
        };
        let Some(mut out_data) = crate::render::Params::new(self, se).gpu_data(self.lum()) else { return Ok(None) };
        out_data.haze = self.gpu_haze(se);
        out_data.scene = se.film.is_active();
        let geo = Geo { full_w: self.full_size[0], full_h: self.full_size[1], off_x: 0, off_y: 0, scale: self.full_size[0] as f32 / w as f32, camera: self.orientation };
        let Some(developed) = crate::gpu::full_mosaic(m, &job, &out_data) else { return Ok(None) };
        let Some(mut img) = self.encoded_or_film(se, developed, w, h, geo) else { return Ok(None) };
        if se.raw.false_color > 0.0 {
            crate::false_color::suppress(&mut img, se.raw.false_color);
        }
        if se.raw.texture != 0.0 {
            crate::render::structure(&mut img, se.raw.texture, geo.scale);
        }
        develop::look::apply(&mut img, se, geo);
        Ok(Some(img))
    }

    /// Disabled sections set to default (like [`Settings::effective`], plus own fields).
    pub fn effective_settings<'a>(&self, s: &'a Settings) -> std::borrow::Cow<'a, Settings> {
        if !s.bypass.any() && !s.tone_eq_mask {
            return std::borrow::Cow::Borrowed(s);
        }
        let basic = s.bypass.basic;
        let mut e = s.effective().into_owned();
        if basic {
            let d = develop::settings::RawAdjust::default();
            e.raw.wb_mode = d.wb_mode;
            e.raw.whites = 0.0;
            e.raw.blacks = 0.0;
            e.raw.texture = 0.0;
        }
        std::borrow::Cow::Owned(e)
    }

    /// Image features for tuning the automatic tone (measurement command `auto-data`): log2 luminance at 19 quantiles of the
    /// preview (with base exposure), log mean of the middle 80 %, fractions blown/blocked, mean saturation,
    /// base exposure, log2 ISO.
    pub fn auto_features(&self) -> Option<Vec<f32>> {
        let st = &self.preview;
        let lum = self.lum();
        let g = self.base_ev.exp2();
        let mut y: Vec<f32> = st.px.iter().step_by(5).map(|p| (lum[0] * p[0] + lum[1] * p[1] + lum[2] * p[2]) * g).collect();
        if y.len() < 100 {
            return None;
        }
        let sat = {
            let (mut sum, mut n) = (0f64, 0f64);
            for p in st.px.iter().step_by(11) {
                let mx = p[0].max(p[1]).max(p[2]).max(1e-6);
                let mn = p[0].min(p[1]).min(p[2]);
                sum += ((mx - mn) / mx) as f64;
                n += 1.0;
            }
            (sum / n.max(1.0)) as f32
        };
        let n = y.len();
        let above = y.iter().filter(|v| **v > 1.0).count() as f32 / n as f32;
        let below = y.iter().filter(|v| **v < 0.002).count() as f32 / n as f32;
        let mean_lin = y.iter().sum::<f32>() / n as f32;
        y.sort_by(|a, b| a.total_cmp(b));
        let mut f: Vec<f32> = [0.001, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.95, 0.98, 0.99, 0.995, 0.999]
            .iter()
            .map(|q| y[((n - 1) as f32 * q) as usize].max(1e-6).log2())
            .collect();
        let (lo, hi) = (n / 10, n * 9 / 10);
        f.push(y[lo..hi].iter().map(|v| v.max(1e-6).ln()).sum::<f32>() / (hi - lo) as f32 / std::f32::consts::LN_2);
        f.extend([mean_lin.max(1e-6).log2(), above, below, sat, self.base_ev, (self.info.exif.iso.unwrap_or(100) as f32).max(1.0).log2()]);
        Some(f)
    }

    /// Automatic tone of a merged photo with linear output ([`hdr_auto_values`] at `strength`):
    /// `[exposure, highlights, shadows, whites, blacks]`, measured with the base exposure the photo is rendered with.
    pub fn auto_tone_hdr(&self, s: &Settings, strength: f32) -> Option<[f32; 5]> {
        let lum = self.lum();
        let g = self.base_ev_for(s).exp2();
        let mut y: Vec<f32> = self.preview.px.iter().step_by(7).map(|p| (lum[0] * p[0] + lum[1] * p[1] + lum[2] * p[2]) * g).collect();
        hdr_auto_values(&mut y).map(|v| hdr_auto_scaled(v, strength))
    }

    /// Automatic look ("Auto", ⌘U): all tone and colour sliders solved together (see [`develop::auto_look`]), measured
    /// on a small rendering of the preview with the remaining settings of `s`. The starting point carries the part of
    /// the base exposure that Lightroom start values leave out (see [`Self::base_ev_for`]), so such photos are measured
    /// as bright as without them. Two renderings of the analysis image. Returns: changed?
    pub fn auto_tone(&self, s: &mut Settings) -> bool {
        match self.auto_look(s) {
            Some(x) => {
                develop::auto_look::apply(&x, s);
                true
            }
            None => false,
        }
    }

    /// The solved slider vector of [`Self::auto_tone`] (exposure already including the Lightroom offset).
    pub fn auto_look(&self, s: &Settings) -> Option<develop::auto_look::Vector> {
        use develop::auto_look as al;
        let offset = (self.base_ev - self.base_ev_for(s)) as f64;
        let iso = self.info.exif.iso.unwrap_or(100) as f64;
        let free = al::curve_free(s);
        let mut x = self.with_look_render(s, |r| al::solve(al::Pipeline::Raw, iso, free, r))?;
        x[0] = ((x[0] + offset) * 20.0).round() / 20.0;
        Some(x)
    }

    /// Hand the analysis rendering of [`Self::auto_look`] (slider vector → features; exposure relative to the
    /// Lightroom offset) to `f` (also for the measurement of the sensitivities in the check tool).
    pub fn with_look_render<R>(&self, s: &Settings, f: impl FnOnce(&mut dyn FnMut(&develop::auto_look::Vector) -> Option<develop::auto_look::Features>) -> R) -> R {
        use develop::auto_look as al;
        let offset = (self.base_ev - self.base_ev_for(s)) as f64;
        let base = Settings { exposure: 0.0, ..s.clone() };
        let small = self.small(&base, 320);
        let mut render = |x: &al::Vector| -> Option<al::Features> {
            let mut v = *x;
            v[0] += offset;
            let st = al::probe_settings(&base, &v);
            let img = self.render_stage(&small, &st);
            let bp = x[8] as f32;
            al::features(img.data.chunks_exact(3).map(|p| [0, 1, 2].map(|c| al::black_point(p[c] as f32 / 65535.0, bp))))
        };
        f(&mut render)
    }
}

/// Automatic tone of a linear merge (HDR) at full strength, from the luminance `y` of its preview after the base
/// exposure (1 = white of the tone curve): `[exposure, highlights, shadows, whites, blacks]`.
/// - exposure: the log mean of the middle 80 % to a balanced middle grey (as "Auto");
/// - highlights: the 99.5 % highlight brought down to just above white (the slider lowers bright surroundings by up
///   to 1.5 EV), what is left over goes to whites (up to 1.1 EV more), beyond that to a lower exposure (up to 2 EV);
/// - shadows: the 2 % shadow lifted to about −4 EV under middle grey and the 10 % shadow to about −2.4 EV (up to
///   1.6 EV); blacks a little down by a tenth of the shadow lift (the blacks slider sets the black point firmly
///   since 1.5.1; a tenth now acts about like the former quarter), so the deepest blacks stay black and the picture does not turn grey.
///
/// `None` with too few pixels.
pub fn hdr_auto_values(y: &mut [f32]) -> Option<[f32; 5]> {
    if y.len() < 100 {
        return None;
    }
    y.sort_by(|a, b| a.total_cmp(b));
    let q = |f: f32| y[((y.len() - 1) as f32 * f) as usize].max(1e-6);
    let (lo, hi) = (y.len() / 10, y.len() * 9 / 10);
    let mean = (y[lo..hi].iter().map(|v| v.max(1e-6).ln()).sum::<f32>() / (hi - lo) as f32).exp();
    let mut ev = (0.16 / mean).log2().clamp(-2.5, 2.5);
    let bright = q(0.995) * ev.exp2();
    let need_hi = (bright.log2() - 0.5).max(0.0);
    let highlights = -(need_hi / 1.5).min(1.0);
    let rest = (need_hi - 1.5).max(0.0);
    let whites = -(rest / 1.1).min(1.0);
    // more range than highlights and whites can take (a linear merge can hold 3 EV and more over white): the rest
    // comes off the exposure (at most 2 EV), the shadows below take it back
    ev -= (rest - 1.1).clamp(0.0, 2.0);
    // the deepest 2 % to about −4 EV under middle grey, the darkest 10 % (a ground in shadow) to about −2.4 EV
    let need_lo = (0.01 / (q(0.02) * ev.exp2())).log2().max((0.03 / (q(0.1) * ev.exp2())).log2()).max(0.0);
    let shadows = (need_lo / 1.6).min(1.0);
    let blacks = -0.1 * shadows;
    Some([ev, highlights, shadows, whites, blacks])
}

/// [`hdr_auto_values`] at `strength` (0 … 1, 0 = all neutral): every value scaled, rounded to the slider steps
/// (exposure 0.05 EV, the others 1 %).
pub fn hdr_auto_scaled(full: [f32; 5], strength: f32) -> [f32; 5] {
    let k = strength.clamp(0.0, 1.0);
    let mut v = full.map(|x| x * k);
    v[0] = (v[0] * 20.0).round() / 20.0;
    for x in v.iter_mut().skip(1) {
        *x = (*x * 100.0).round() / 100.0;
    }
    // no "-0" in the sliders
    v.map(|x| if x == 0.0 { 0.0 } else { x })
}

/// Automatic white balance: gray world over the mid luminances (without blown and
/// very dark areas), weighted towards weakly saturated pixels. Returns: camera neutral.
fn auto_neutral(px: &[[f32; 3]], wb: [f32; 3]) -> Option<[f32; 3]> {
    let mut s = [0f64; 3];
    let mut n = 0f64;
    for p in px.iter().step_by(3) {
        let cam = [p[0] / wb[0], p[1] / wb[1], p[2] / wb[2]];
        let mx = p[0].max(p[1]).max(p[2]);
        let mn = p[0].min(p[1]).min(p[2]);
        if mx > 0.9 || mx < 0.01 {
            continue;
        }
        // strongly saturated colors count less (colorful areas would bias it otherwise)
        let sat = (mx - mn) / mx.max(1e-6);
        let wt = (1.0 - sat).max(0.05) as f64;
        for c in 0..3 {
            s[c] += cam[c] as f64 * wt;
        }
        n += wt;
    }
    (n > 10.0 && s[1] > 0.0).then(|| [(s[0] / s[1]) as f32, 1.0, (s[2] / s[1]) as f32])
}

fn info_texts(i: &raw::Info, style: Option<&raw::picture_style::PictureStyle>, as_shot: (f32, f32)) -> (Vec<(String, String)>, String) {
    let e = &i.exif;
    // a value of 0 is "unknown" (manual lenses report aperture 0): left out, never "f/0"
    let lap_time_v = e.exposure_time.filter(|z| *z > 0.0).map(|z| if z >= 1.0 { format!("{z:.1} s").replace(".0 s", " s") } else { format!("1/{:.0} s", 1.0 / z) });
    let aperture_v = e.aperture.filter(|b| *b > 0.0).map(|b| format!("f/{b:.1}").replace(".0", ""));
    let iso_v = e.iso.filter(|v| *v > 0).map(|v| format!("ISO {v}"));
    let bw_v = e.focal_length.filter(|b| *b > 0.0).map(|b| format!("{b:.0} mm"));
    let summary = [&lap_time_v, &aperture_v, &iso_v, &bw_v].iter().filter_map(|v| v.as_deref()).collect::<Vec<_>>().join(" · ");
    let exposure = [&lap_time_v, &aperture_v, &iso_v].iter().filter_map(|v| v.as_deref()).collect::<Vec<_>>().join(" · ");
    let exposure = if exposure.is_empty() { "–".to_string() } else { exposure };
    let bw = bw_v.unwrap_or_else(|| "–".into());
    let mut rows = vec![
        ("Kamera".to_string(), raw::camera_display(&i.maker, &i.model)),
        ("Aufnahme".into(), e.capture_date.clone().unwrap_or_else(|| "–".into())),
        ("Belichtung".into(), exposure),
        ("Brennweite".into(), bw.clone()),
        ("Objektiv".into(), e.lens.clone().or_else(|| e.lens_db.clone()).unwrap_or_else(|| "–".into())),
        ("Weißabgleich".into(), format!("{:.0} K · Tönung {:+.0}", as_shot.0, as_shot.1)),
        ("Format".into(), format!("{} · {} · {}×{}", i.format, match i.sensor {
            raw::Sensor::Bayer => "Bayer",
            raw::Sensor::XTrans => "X-Trans",
            raw::Sensor::Linear => "linear",
            raw::Sensor::Mono => "monochrom",
            raw::Sensor::OtherMosaic => "anderes Mosaik",
        }, i.width, i.height)),
    ];
    if let Some(b) = style {
        rows.push(("Bildstil".into(), b.style.clone()));
    }
    if let Some(s) = &e.serial_number {
        rows.push(("Seriennummer".into(), s.clone()));
    }
    (rows, summary)
}

/// Rectangle from a 16-bit image.
pub fn crop_rgb(img: &Rgb16, x: usize, y: usize, w: usize, h: usize) -> Rgb16 {
    let mut data = Vec::with_capacity(w * h * 3);
    for yy in y..y + h {
        data.extend_from_slice(&img.data[(yy * img.width + x) * 3..(yy * img.width + x + w) * 3]);
    }
    Rgb16 { width: w, height: h, data }
}

#[path="sharpen_study.rs"]
pub mod sharpen_study;

/// The system RAW engine of macOS as decoding stage (Apple path); a child of this module for the document's fields
#[path = "apple.rs"]
pub mod apple;

/// Spacing of the lens correction grid on the GPU in pixels (8; `GRAINEO_LENS_GRID=n` for measurements).
fn lens_grid_step() -> usize {
    std::env::var("GRAINEO_LENS_GRID").ok().and_then(|v| v.parse().ok()).filter(|v| *v >= 1).unwrap_or(8)
}

/// The summary line of a merged photo: "HDR · <frames> · <summary of the reference>". An existing "HDR · n · " prefix
/// is replaced (QA 1.5.0-test.5: after a frame was left out the line still said "HDR · 7").
fn merged_summary(summary: &str, frames: usize) -> String {
    let rest = summary.strip_prefix("HDR · ").and_then(|s| s.split_once(" · ")).filter(|(n, _)| n.parse::<usize>().is_ok()).map_or(summary, |(_, r)| r);
    format!("HDR · {frames} · {rest}")
}

#[cfg(test)]
mod merged_summary_tests {
    #[test]
    fn frame_count_follows_the_sources() {
        assert_eq!(super::merged_summary("1/20 s · f/18", 7), "HDR · 7 · 1/20 s · f/18");
        assert_eq!(super::merged_summary("HDR · 7 · 1/20 s · f/18", 6), "HDR · 6 · 1/20 s · f/18");
        assert_eq!(super::merged_summary("", 3), "HDR · 3 · ");
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use std::cell::Cell;

    /// An output never takes the stand-in of a merge that is not computed: the merge is computed and the photo opened
    /// again; when it still is not there the output fails with a reason. Ordinary photos are opened once.
    #[test]
    fn output_waits_for_the_merge_or_fails() {
        #[derive(Debug, PartialEq)]
        struct Doc(&'static str, bool);
        let pending = |d: &Doc| d.1.then_some(d.0);
        // an ordinary photo, a merge from the cache: taken as opened, nothing computed
        let computed = Cell::new(0);
        let compute = |_: &&str| {
            computed.set(computed.get() + 1);
            Ok(())
        };
        let r = settle_for_output(Some(Doc("photo", false)), pending, compute, || panic!("not opened twice"));
        assert_eq!((r, computed.get()), (Ok(Some(Doc("photo", false))), 0));
        // the stand-in: computed, then the merge itself
        let r = settle_for_output(Some(Doc("reference", true)), pending, compute, || Ok(Some(Doc("merge", false))));
        assert_eq!((r, computed.get()), (Ok(Some(Doc("merge", false))), 1));
        // the merge cannot be computed (a source is missing): the reason, no output
        let r = settle_for_output(Some(Doc("reference", true)), pending, |_| Err("Quelle fehlt: a.CR2".into()), || panic!("not opened again"));
        assert_eq!(r, Err("Quelle fehlt: a.CR2".to_string()));
        // computed, but still not there (the cache could not be written): an error, never the stand-in
        let r = settle_for_output(Some(Doc("reference", true)), pending, |_| Ok(()), || Ok(Some(Doc("reference", true))));
        assert_eq!(r, Err(MERGE_NOT_READY.to_string()));
        // opening was cancelled
        let r = settle_for_output(None::<Doc>, pending, |_| Ok(()), || panic!());
        assert_eq!(r, Ok(None));
    }
}

#[cfg(test)]
mod profile_scale_tests {
    use super::*;

    /// A strongly downscaled panorama: the reduced simulation gives (nearly) the same noise levels as the exact
    /// one, at a fraction of the size.
    #[test]
    fn small_scale_profile_matches_exact_simulation() {
        let m = noise::NoiseModel { a: [0.0; 3], b: [4e-4, 3e-4, 5e-4] };
        let f = 0.1;
        let exact = image_profile_exact(m, f);
        let fast = image_profile(m, f);
        // compare in image units: the stabilising transform divides by √b of the model
        for c in 0..3 {
            let bm = (m.b[0] + m.b[1] + m.b[2]) / 3.0;
            let bf = (fast.model.b[0] + fast.model.b[1] + fast.model.b[2]) / 3.0;
            for l in 0..2 {
                let e = exact.sigma[c][l] * bm.sqrt();
                let q = fast.sigma[c][l] * bf.sqrt();
                assert!((q / e - 1.0).abs() < 0.2, "channel {c} level {l}: exact {e}, reduced {q}");
            }
        }
    }
}

#[cfg(test)]
mod hdr_auto_tests {
    use super::*;

    /// A wide scene (shadows 8 EV under white, highlights 4 EV over it): full strength pulls the highlights down and
    /// lifts the shadows; the strength scales every value, 0 is neutral.
    #[test]
    fn hdr_automatic_scales_with_strength() {
        let mut y: Vec<f32> = (0..2000).map(|i| (-8.0 + 12.0 * i as f32 / 1999.0).exp2() * 0.5).collect();
        let full = hdr_auto_values(&mut y).unwrap();
        assert!(full[1] < -0.5, "highlights down: {full:?}");
        assert!(full[2] > 0.2, "shadows up: {full:?}");
        assert!(full[4] <= 0.0, "blacks never lifted: {full:?}");
        assert_eq!(hdr_auto_scaled(full, 0.0), [0.0; 5], "0 % is off");
        let half = hdr_auto_scaled(full, 0.5);
        let one = hdr_auto_scaled(full, 1.0);
        for i in 0..5 {
            assert!((half[i] - full[i] / 2.0).abs() <= 0.026, "{i}: {half:?} {full:?}");
            assert!((one[i] - full[i]).abs() <= 0.026, "{i}: {one:?} {full:?}");
        }
        // a scene that fits (no highlights over white, no deep shadows) gets no highlights/shadows
        let mut flat: Vec<f32> = (0..2000).map(|i| 0.05 + 0.4 * i as f32 / 1999.0).collect();
        let f = hdr_auto_values(&mut flat).unwrap();
        assert_eq!((f[1], f[2], f[3]), (0.0, 0.0, 0.0), "{f:?}");
        assert!(hdr_auto_values(&mut [0.5; 10]).is_none());
    }
}

#[cfg(test)]
mod auto_tone_tests {
    use super::*;

    /// "Auto" measures with the base exposure the photo is rendered with: with Lightroom start values of an X100 in
    /// DR200/DR400 (`lr_offset` 1 or 2 EV less) it must ask for that much more exposure, otherwise the result is 1–2 EV
    /// too dark. Photos without the offset, or without Lightroom start values, are unchanged.
    #[test]
    fn auto_tone_measures_with_the_rendered_base_exposure() {
        let dir = std::env::temp_dir().join(format!("graineo-auto-tone-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("grey.jpg");
        // a dark grey with some structure: the automatic exposure is well inside its ±2.5 EV range
        let img = image::RgbImage::from_fn(96, 64, |x, y| {
            let v = 100 + ((x * 3 + y * 5) % 24) as u8;
            image::Rgb([v, v, v])
        });
        img.save(&png).unwrap();
        let mut doc = RawDoc::open(&png, &|| true).unwrap().unwrap();
        let auto = |doc: &RawDoc, from_lightroom: bool| {
            let mut s = Settings::default();
            s.raw.from_lightroom = from_lightroom;
            assert!(doc.auto_tone(&mut s));
            s.exposure
        };
        let (plain, lr) = (auto(&doc, false), auto(&doc, true));
        assert_eq!(plain, lr, "no offset: Lightroom start values change nothing");
        assert!(plain.abs() < 2.0, "{plain}");
        for offset in [1.0f32, 2.0] {
            doc.lr_offset = offset;
            assert_eq!(auto(&doc, false), plain, "without Lightroom start values the offset does not apply");
            let with = auto(&doc, true);
            assert!((with - plain - offset as f64).abs() < 0.051, "offset {offset}: {plain} → {with}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod extension_tests {
    use std::path::Path;

    /// Nikon NEF and NRW (Coolpix) open in the RAW development, in any case.
    #[test]
    fn nikon_files_are_raw() {
        for name in ["a.NEF", "a.nef", "a.NRW", "a.nrw"] {
            assert!(super::is_raw(Path::new(name)), "{name}");
        }
        assert!(!super::is_raw(Path::new("a.jpg")));
    }
}

#[cfg(test)]
mod linear_tests {
    use super::*;

    /// The linear scene rendering leaves out the capture sharpening (own detail stage), other looks keep it.
    #[test]
    fn linear_look_has_no_capture_sharpening() {
        let mut s = Settings::default();
        assert!(DetailKey::from_settings(&s).sharpen);
        s.raw.look = crate::learn::LINEAR_LOOK.into();
        let k = DetailKey::from_settings(&s);
        assert!(!k.sharpen);
        assert_ne!(k, DetailKey::from_settings(&Settings::default()));
    }
}
