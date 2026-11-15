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
    fn new(px: Vec<[f32; 3]>, w: usize, h: usize, profile: Option<noise::Profile>) -> Self {
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
    /// all bundled looks of this camera
    pub looks: Vec<String>,
    /// as-shot white balance as (Kelvin, tint)
    pub as_shot: (f32, f32),
    /// automatic white balance (camera neutral, from the preview)
    pub auto_neutral: [f32; 3],
    /// base exposure of the standard development in EV (without sliders)
    pub base_ev: f32,
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
    let e = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(e.as_str(), "cr2" | "cr3" | "raf" | "dng" | "arw" | "nef" | "orf" | "rw2" | "pef" | "srw")
}

/// Finished image (JPEG, HEIC, TIFF, PNG) that can be edited here?
pub fn is_image(p: &Path) -> bool {
    let e = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(e.as_str(), "jpg" | "jpeg" | "heic" | "heif" | "hif" | "tif" | "tiff" | "png")
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

impl RawDoc {
    /// Open: decode, preview, white balance values, picture style. `keep_going`: abort while browsing.
    pub fn open(path: &Path, keep_going: &dyn Fn() -> bool) -> Result<Option<RawDoc>, String> {
        if is_image(path) {
            return Self::open_image(path).map(Some);
        }
        let t = std::time::Instant::now();
        let f = raw::RawFile::open(path).map_err(|e| e.to_string())?;
        if !keep_going() {
            return Ok(None);
        }
        let info = f.info.clone();
        let b = f.basis(Demosaic::Auto, true, true, Size::Preview(PREVIEW_EDGE)).map_err(|e| e.to_string())?;
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
        let std_look = raw::development::camera_look(&info.maker, &info.model).map(|l| Arc::new(raw::development::LookLut::new(&l)));
        let picture_style = raw::picture_style::read(path);
        let short = raw::picture_style::camera_short(&info.maker, &info.model);
        let looks: Vec<String> = raw::looks::Look::names().into_iter().filter(|n| n.split('/').next() == Some(short.as_str())).map(str::to_string).collect();
        let auto_look = picture_style.as_ref().map(|b| format!("{}/{}", b.camera_short(), b.style)).filter(|n| looks.contains(n));
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
            looks,
            as_shot,
            auto_neutral,
            base_ev,
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
        let (info_rows, summary) = info_texts(&info, None, (6504.0, 0.0));
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
            looks: Vec::new(),
            as_shot: (6504.0, 0.0),
            auto_neutral: [1.0; 3],
            base_ev: 0.0,
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
        })
    }

    /// Release large intermediate states (full resolution, decoded file).
    pub fn release_memory(&self) {
        for (_, st) in self.preview_detail.lock().unwrap().iter() {
            st.release_gpu();
        }
        *self.full.lock().unwrap() = None;
        *self.full_detail.lock().unwrap() = None;
        *self.file.lock().unwrap() = None;
    }

    /// Camera neutral of the chosen white balance.
    pub fn target_neutral(&self, s: &Settings) -> Option<[f32; 3]> {
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
            "auto" => crate::wb::temp_from_neutral(&self.info, self.auto_neutral).unwrap_or(self.as_shot),
            v => crate::wb::preset(v).unwrap_or(self.as_shot),
        }
    }

    /// Effective look name (or `None` = neutral). An unedited photo (`""`) starts neutral: the camera look
    /// ([`Self::auto_look`]) is applied only when it is chosen, then it is stored by name.
    pub fn look_name(&self, s: &Settings) -> Option<String> {
        match s.raw.look.as_str() {
            "" | "neutral" => None,
            n => Some(n.to_string()),
        }
    }

    pub(crate) fn look_for(&self, s: &Settings) -> Option<Arc<raw::looks::Look>> {
        let name = self.look_name(s)?;
        let cache_name = if name.starts_with("film:") { format!("{}:{name}",raw::film_profile::generation()) } else { name.clone() };
        let mut c = self.looks_cache.lock().unwrap();
        if let Some((_, l)) = c.iter().find(|(n, _)| *n == cache_name) {
            return Some(l.clone());
        }
        // `film:<UUID>`: film profile (Camera Raw look with RGB table) from the user's files
        let l = Arc::new(raw::looks::Look::built_in(&name).or_else(|| raw::film_profile::look_for_names(&name))?);
        if name.starts_with("film:") {c.retain(|(n,_)| !n.contains(":film:"));}
        c.push((cache_name, l.clone()));
        Some(l)
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
            if self.finished {
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
        if self.finished {
            return None;
        }
        let mut g = self.file.lock().unwrap();
        if g.is_none() {
            *g = raw::RawFile::open(&self.path).ok();
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
            let wb = self.info.wb.unwrap_or([1.0; 3]);
            let sch = raw::sharpness::Sharpness::default();
            let corr_opt = if key.lens.is_some() { self.correction(key).0 } else { None };
            let fits = corr_opt.as_ref().is_none_or(|k| (k.width, k.height) == (w, h));
            let grid = corr_opt.as_ref().filter(|_| fits).map(|k| k.grid(lens_grid_step()));
            let job = crate::gpu::Job {
                noise: base.profile.as_ref().and_then(|p| raw::noise::parameter(p, &key.noise(), w, h)),
                sharpen: (!self.finished).then(|| (sch.strength, raw::sharpness::kernel(sch.sigma), sch.iterations, sch.threshold, 0.95 * wb[1].min(wb[0]).min(wb[2]))),
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
        if full && !self.finished {
            let _t = crate::timings::Stage::new("CPU Schärfe");
            let wb = self.info.wb.unwrap_or([1.0; 3]);
            raw::sharpness::apply(&mut px, w, h, &raw::sharpness::Sharpness::default(), 0.95 * wb[1].min(wb[0]).min(wb[2]));
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
        let key = self.detail_key(s);
        {
            let mut c = self.preview_detail.lock().unwrap();
            if let Some(i) = c.iter().position(|(k, _)| *k == key) {
                let e = c.remove(i);
                let st = e.1.clone();
                c.insert(0, e);
                return st;
            }
        }
        let st = Arc::new(self.detail(&self.preview, key, false));
        let mut c = self.preview_detail.lock().unwrap();
        c.retain(|(k, _)| *k != key);
        c.insert(0, (key, st.clone()));
        c.truncate(PREVIEW_STAGES);
        st
    }

    /// Does the preview detail stage already have this state?
    pub fn preview_ready(&self, s: &Settings) -> bool {
        let key = self.detail_key(s);
        self.preview_detail.lock().unwrap().iter().any(|(k, _)| *k == key)
    }

    /// Full resolution after demosaicing (cached).
    pub fn full_base(&self) -> Result<Arc<Stage>, String> {
        if let Some(v) = self.full.lock().unwrap().as_ref() {
            return Ok(v.clone());
        }
        let _busy = self.full_busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.full.lock().unwrap().as_ref() {
            return Ok(v.clone());
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

    /// Full resolution after noise reduction, sharpening, lens correction (cached per slider state).
    pub fn full_stage(&self, s: &Settings) -> Result<Arc<Stage>, String> {
        let key = self.detail_key(s);
        if let Some((k, st)) = self.full_detail.lock().unwrap().as_ref() {
            if *k == key {
                return Ok(st.clone());
            }
        }
        let b = self.full_base()?;
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
        let b = self.full_base()?;
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
        if self.info.sensor == raw::Sensor::XTrans && std::env::var("GRAINEO_GPU_DEMOSAIC").as_deref() != Ok("0") {
            if let Some(img) = self.render_full_gpu_mosaic(s, se, key)? {
                return Ok(Some(img));
            }
        }
        let b = self.full_base()?;
        let (w, h) = (b.w, b.h);
        let wb = self.info.wb.unwrap_or([1.0; 3]);
        let sch = raw::sharpness::Sharpness::default();
        let corr_opt = if key.lens.is_some() { self.correction(key).0 } else { None };
        if !corr_opt.as_ref().is_none_or(|k| (k.width, k.height) == (w, h)) {
            return Ok(None);
        }
        let grid = corr_opt.as_ref().map(|k| k.grid(lens_grid_step()));
        let job = crate::gpu::Job {
            noise: b.profile.as_ref().and_then(|p| raw::noise::parameter(p, &key.noise(), w, h)),
            sharpen: Some((sch.strength, raw::sharpness::kernel(sch.sigma), sch.iterations, sch.threshold, 0.95 * wb[1].min(wb[0]).min(wb[2]))),
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
        let wb = self.info.wb.unwrap_or([1.0; 3]);
        let sch = raw::sharpness::Sharpness::default();
        let corr_opt = if key.lens.is_some() { self.correction(key).0 } else { None };
        if !corr_opt.as_ref().is_none_or(|k| (k.width, k.height) == (w, h)) {
            return Ok(None);
        }
        let grid = corr_opt.as_ref().map(|k| k.grid(lens_grid_step()));
        let job = crate::gpu::Job {
            noise: b.profile.as_ref().and_then(|p| raw::noise::parameter(p, &key.noise(), w, h)),
            sharpen: Some((sch.strength, raw::sharpness::kernel(sch.sigma), sch.iterations, sch.threshold, 0.95 * wb[1].min(wb[0]).min(wb[2]))),
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

    /// Automatic tone: exposure such that the mean luminance lies as in a
    /// balanced shot; highlights/shadows with a lot of contrast. Returns: changed?
    pub fn auto_tone(&self, s: &mut Settings) -> bool {
        let st = &self.preview;
        let lum = self.lum();
        let g = (self.base_ev).exp2();
        let mut y: Vec<f32> = st.px.iter().step_by(7).map(|p| (lum[0] * p[0] + lum[1] * p[1] + lum[2] * p[2]) * g).collect();
        if y.len() < 100 {
            return false;
        }
        y.sort_by(|a, b| a.total_cmp(b));
        let q = |f: f32| y[((y.len() - 1) as f32 * f) as usize].max(1e-6);
        // mean in log space of the middle 80 %
        let lo = y.len() / 10;
        let hi = y.len() * 9 / 10;
        let mean = (y[lo..hi].iter().map(|v| v.max(1e-6).ln()).sum::<f32>() / (hi - lo) as f32).exp();
        let ev = (0.16 / mean).log2().clamp(-2.5, 2.5);
        let bright = q(0.995) * ev.exp2();
        let dark = q(0.02) * ev.exp2();
        s.exposure = ((ev * 20.0).round() / 20.0) as f64;
        s.highlight = if bright > 1.5 { (-(bright.log2() - 0.6) * 0.35).clamp(-0.7, 0.0) as f64 } else { 0.0 };
        s.shadow = if dark < 0.004 { ((0.004 / dark).log2() * 0.08).clamp(0.0, 0.5) as f64 } else { 0.0 };
        s.raw.whites = 0.0;
        s.raw.blacks = 0.0;
        true
    }
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
    let lap_time = e.exposure_time.map(|z| if z >= 1.0 { format!("{z:.1} s").replace(".0 s", " s") } else { format!("1/{:.0} s", 1.0 / z) }).unwrap_or_else(|| "–".into());
    let aperture = e.aperture.map(|b| format!("f/{b:.1}").replace(".0", "")).unwrap_or_else(|| "–".into());
    let iso = e.iso.map(|v| format!("ISO {v}")).unwrap_or_else(|| "–".into());
    let bw = e.focal_length.map(|b| format!("{b:.0} mm")).unwrap_or_else(|| "–".into());
    let mut rows = vec![
        ("Kamera".to_string(), raw::camera_display(&i.maker, &i.model)),
        ("Aufnahme".into(), e.capture_date.clone().unwrap_or_else(|| "–".into())),
        ("Belichtung".into(), format!("{lap_time} · {aperture} · {iso}")),
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
    (rows, format!("{lap_time} · {aperture} · {iso} · {bw}"))
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

/// Spacing of the lens correction grid on the GPU in pixels (8; `GRAINEO_LENS_GRID=n` for measurements).
fn lens_grid_step() -> usize {
    std::env::var("GRAINEO_LENS_GRID").ok().and_then(|v| v.parse().ok()).filter(|v| *v >= 1).unwrap_or(8)
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
