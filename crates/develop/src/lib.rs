//! Development of Merrill raw data.
//!
//! Pipeline:
//! 1. [`linearize`]: subtract the black level, apply spatial gain and radial colour
//!    shading, crop to the image area, normalise to the sensor saturation
//!    → linear camera image (still without white balance). Defective pixels
//!    are replaced in the raw data beforehand ([`defects::replace_bad_pixels_raw`]),
//!    then optionally [`denoise::denoise`].
//! 2. [`render`]: white balance, Fill Light, colour matrix (incl. colour mode) or
//!    monochrome mix, highlight neutralisation, Sigma tone curve, sRGB gamma.
//!    Alternatively sensor views ([`View`]) for exploring the three layers.
//! 3. Output via [`export`].
//!
//! [`Session`] bundles this for an open image and keeps expensive
//! intermediate results.
//!
//! Not yet included: defect clusters, highlight reconstruction.
//! Sigma's sharpness slider acts on the SPP base ([`sharpness`]).

pub mod analysis;
pub mod auto;
pub mod auto_look;
pub mod calib;
pub mod classic_new;
pub mod curve;
pub mod output_sharpen;
pub mod foveon_texture;
pub mod damaged;
pub mod defects;
pub mod defringe;
pub mod spp_defringe;
pub mod dehaze;
pub mod ai_denoise;
pub mod denoise;
pub mod diskcache;
pub mod dng;
pub mod export;
pub mod exr;
pub mod filllight;
pub mod film;
pub mod grain_detail;
pub mod highlight_post;
pub mod highlight_rebuild;
pub mod merrill_highlights;
pub mod geometry;
pub mod inspect;
pub mod jxl;
pub mod analog;
pub mod layers;
pub mod lens;
pub mod lift;
pub mod look;
pub mod local;
pub mod spots;
pub mod matrix;
pub mod mono;
pub mod quattro_color;
pub mod quattro_nr;
pub mod quattro_noise;
pub mod quattro_grid;
pub mod quattro_hi;
pub mod quattro_lce;
pub mod quattro_speckle;
pub mod png16;
pub mod profile;
pub mod quattro;
pub mod rawlayers;
pub mod resize;
mod resize_gpu;
pub mod session;
pub mod settings;
pub mod sfd;
pub mod sharpness;
pub mod sigma_out;
pub mod spp;
pub mod sppa;
pub mod tiffw;
pub mod tone_eq;
pub mod upright;
pub mod xmp;

use rayon::prelude::*;
use x3f::RawImage;

pub use calib::Calibration;
/// Test switch for the alternative Merrill parts (see `x3f_core::merrill_new`).
pub use x3f_core::merrill_new;
pub use curve::ToneCurve;
use matrix::Mat3;
pub use session::Session;
pub use settings::{Binning, HighlightSliders, NoiseBrake, Settings, SppKey, View, NR_OFF, TEXTURE_NEUTRAL};

/// Linear camera image: three planes, black = 0, sensor saturation = 1.
#[derive(Clone)]
pub struct Linear {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// SPP counts only: Sigma's detail image "L" for the sharpness slider
    /// ([`sharpness`]), same size as the image.
    pub detail: Option<std::sync::Arc<sharpness::SharpDetail>>,
    /// Dehaze analysis of this image ([`dehaze::HazeField`]), computed once on the full image and cropped/downscaled
    /// with it so that every crop and preview uses the same haze colour and map
    pub haze: Option<std::sync::Arc<dehaze::HazeField>>,
    /// Tone equalizer mask of this image ([`tone_eq::Mask`]): a view into the analysis of the whole image, cropped and
    /// downscaled with it (like `haze`)
    pub toneq: Option<tone_eq::Mask>,
    /// Grey base of Sigma's monochrome processing: all three channels hold
    /// the grey value in counts, already sharpened; see [`spp::working_key`]
    pub mono: bool,
}

impl Linear {
    pub fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// Copy a rectangular crop.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Linear {
        let mut data = Vec::with_capacity(w * h * 3);
        for yy in y..y + h {
            data.extend_from_slice(&self.data[(yy * self.width + x) * 3..(yy * self.width + x + w) * 3]);
        }
        let detail = self.detail.as_ref().map(|d| std::sync::Arc::new(d.crop(self.width, x, y, w, h)));
        let haze = self.haze.as_ref().map(|f| std::sync::Arc::new(f.crop(x, y, w, h)));
        let toneq = self.toneq.as_ref().map(|m| m.crop(x, y, w, h));
        Linear { mono: self.mono, width: w, height: h, data, detail, haze, toneq }
    }

    /// Downscale by an integer factor (mean over blocks).
    pub fn downsample(&self, factor: usize) -> Linear {
        if factor <= 1 {
            return self.clone();
        }
        let (w, h) = (self.width / factor, self.height / factor);
        let n = (factor * factor) as f32;
        let mut data = vec![0f32; w * h * 3];
        data.par_chunks_mut(w * 3).enumerate().for_each(|(y, line)| {
            for x in 0..w {
                let mut s = [0f32; 3];
                for yy in y * factor..(y + 1) * factor {
                    let row = &self.data[(yy * self.width + x * factor) * 3..(yy * self.width + (x + 1) * factor) * 3];
                    for p in row.chunks_exact(3) {
                        s[0] += p[0];
                        s[1] += p[1];
                        s[2] += p[2];
                    }
                }
                line[x * 3..x * 3 + 3].copy_from_slice(&[s[0] / n, s[1] / n, s[2] / n]);
            }
        });
        let haze = self.haze.as_ref().map(|f| std::sync::Arc::new(f.downsample(factor, w, h)));
        let toneq = self.toneq.as_ref().map(|m| m.downsample(factor, w, h));
        Linear { mono: self.mono, width: w, height: h, data, detail: None, haze, toneq }
    }
}

pub fn linearize(raw: &RawImage, cal: &Calibration) -> Linear {
    let [x0, y0, x1, y1] = cal.active;
    let (width, height) = (x1 - x0 + 1, y1 - y0 + 1);
    let black: [f32; 3] = cal.black.map(|b| b as f32);
    let inv_sat = 1.0 / cal.saturation as f32;
    let mut data = vec![0f32; width * height * 3];
    data.par_chunks_mut(width * 3).enumerate().for_each(|(y, line)| {
        let sy = y0 + y;
        let rows: Option<[Vec<f32>; 3]> =
            cal.spatial.as_ref().map(|t| std::array::from_fn(|c| t[c].row(sy, raw.rows)));
        for x in 0..width {
            let sx = x0 + x;
            let p = raw.pixel(sy, sx);
            let (sr, sb) = cal.shading.map_or((1.0, 1.0), |s| s.factors(sx, sy));
            let shade = [sr, 1.0, sb];
            for c in 0..3 {
                let g = match (&cal.spatial, &rows) {
                    (Some(t), Some(r)) => t[c].at_row(&r[c], sx, raw.columns),
                    _ => 1.0,
                };
                line[x * 3 + c] = (p[c] as f32 - black[c]) * g * shade[c] * inv_sat;
            }
        }
    });
    Linear { mono: false, haze: None, toneq: None, width, height, data, detail: None }
}

/// Like [`linearize`], but directly downscaled by `factor` (mean over blocks of the raw data).
/// Spatial gain and colour shading are evaluated per block at its centre.
/// For the preview: fast and without the full floating-point image.
pub fn linearize_scaled(raw: &RawImage, cal: &Calibration, factor: usize) -> Linear {
    if factor <= 1 {
        return linearize(raw, cal);
    }
    let [x0, y0, x1, y1] = cal.active;
    let (width, height) = ((x1 - x0 + 1) / factor, (y1 - y0 + 1) / factor);
    let black: [f32; 3] = cal.black.map(|b| b as f32);
    let norm = 1.0 / (cal.saturation as f32 * (factor * factor) as f32);
    let mut data = vec![0f32; width * height * 3];
    data.par_chunks_mut(width * 3).enumerate().for_each(|(y, line)| {
        let sy0 = y0 + y * factor;
        let cy = sy0 + factor / 2;
        let rows: Option<[Vec<f32>; 3]> =
            cal.spatial.as_ref().map(|t| std::array::from_fn(|c| t[c].row(cy, raw.rows)));
        let mut acc = vec![[0u32; 3]; width];
        for sy in sy0..sy0 + factor {
            let base = sy * raw.columns + x0;
            for c in 0..3 {
                let plane = &raw.planes[c][base..base + width * factor];
                for (x, block) in plane.chunks_exact(factor).enumerate() {
                    acc[x][c] += block.iter().map(|&v| v as u32).sum::<u32>();
                }
            }
        }
        let n = (factor * factor) as f32;
        for x in 0..width {
            let cx = x0 + x * factor + factor / 2;
            let (sr, sb) = cal.shading.map_or((1.0, 1.0), |s| s.factors(cx, cy));
            let shade = [sr, 1.0, sb];
            for c in 0..3 {
                let g = match (&cal.spatial, &rows) {
                    (Some(t), Some(r)) => t[c].at_row(&r[c], cx, raw.columns),
                    _ => 1.0,
                };
                line[x * 3 + c] = (acc[x][c] as f32 - black[c] * n) * g * shade[c] * norm;
            }
        }
    });
    Linear { mono: false, haze: None, toneq: None, width, height, data, detail: None }
}

/// Fully developed image, sRGB-encoded, 16 bit per channel.
pub struct Rgb16 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u16>,
}

impl Rgb16 {
    pub fn to_rgb8(&self) -> Vec<u8> {
        self.data.par_iter().map(|&v| ((v as u32 * 255 + 32767) / 65535) as u8).collect()
    }

    /// [`Self::to_rgb8`] with triangular dither of ±1 step (fixed pattern from the pixel position): smooth, almost
    /// noise-free gradients (a sky merged from several exposures) show no contour steps on an 8-bit display. The mean
    /// stays the same.
    pub fn to_rgb8_dithered(&self) -> Vec<u8> {
        self.data
            .par_iter()
            .enumerate()
            .map(|(i, &v)| {
                let hash = |mut x: u32| {
                    x ^= x >> 16;
                    x = x.wrapping_mul(0x7feb_352d);
                    x ^= x >> 15;
                    x = x.wrapping_mul(0x846c_a68b);
                    x ^ (x >> 16)
                };
                let a = hash(i as u32 * 2 + 1) as f32 / u32::MAX as f32;
                let b = hash(i as u32 * 2 + 2) as f32 / u32::MAX as f32;
                (v as f32 * 255.0 / 65535.0 + a - b + 0.5).floor().clamp(0.0, 255.0) as u8
            })
            .collect()
    }
}

fn srgb_encode(x: f32) -> f32 {
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB encoding as a table over linear 0..1.
struct EncodeLut(Vec<u16>);

impl EncodeLut {
    const N: usize = 1 << 16;
    fn new() -> Self {
        Self((0..=Self::N).map(|i| (srgb_encode(i as f32 / Self::N as f32) * 65535.0 + 0.5) as u16).collect())
    }
    #[inline]
    fn get(&self, v: f32) -> u16 {
        self.0[(v.clamp(0.0, 1.0) * Self::N as f32) as usize]
    }
}

/// Develops `lin` with the settings `s`. `fill_map` is the log gain map from
/// [`filllight::log_gain_map`] for exactly this image (required if Fill Light ≠ 0).
pub fn render(lin: &Linear, cal: &Calibration, s: &Settings, fill_map: Option<&[f32]>) -> Rgb16 {
    if s.view != View::Developed {
        return render_sensor_view(lin, s);
    }
    render_impl(lin, cal, s, fill_map, Out::Tone).tone()
}

/// Like [`render`], but with true 16-bit gradations for export: the tone curve is interpolated
/// between its table entries and the sRGB gamma evaluated exactly (instead of Sigma's integer
/// tables). Own pipeline only; Sigma's exact path keeps its tables.
pub fn render_fine(lin: &Linear, cal: &Calibration, s: &Settings, fill_map: Option<&[f32]>) -> Rgb16 {
    if s.view != View::Developed {
        return render_sensor_view(lin, s);
    }
    render_impl(lin, cal, s, fill_map, Out::Fine).tone()
}

/// Like [`render`], but as extended (unclipped) sRGB-encoded floats for wide-gamut exports:
/// colours outside sRGB stay as values below 0 or above 1 (the tone curve is continued
/// point-symmetrically below 0 and linearly above its end). `fine` interpolates the tone curve.
pub fn render_wide(lin: &Linear, cal: &Calibration, s: &Settings, fill_map: Option<&[f32]>, fine: bool) -> Linear {
    render_impl(lin, cal, s, fill_map, Out::Wide { fine }).scene()
}

/// Like [`render`], but scene-linear right before Sigma's tone curve (input of the
/// film simulation, see [`film`]).
pub fn render_scene(lin: &Linear, cal: &Calibration, s: &Settings, fill_map: Option<&[f32]>) -> Linear {
    render_impl(lin, cal, s, fill_map, Out::Scene(film::scene_gain(cal, s))).scene()
}

fn render_impl(lin: &Linear, cal: &Calibration, s: &Settings, fill_map: Option<&[f32]>, mode_out: Out) -> RenderOut {
    let mode = cal.mode(s.color_mode.as_deref());
    // sd Quattro colour mode fine correction (preview of the own pipeline, on the gamma-encoded image as an approximation)
    let axes = mode.axes.as_deref().filter(|_| !s.is_linear() && std::env::var("GRAINEO_MULTIAXIS").map_or(true, |v| v != "0"));
    let tone = output_tone(cal, s, mode);
    let (max_count, mono_max) = (cal.max_count(), if cal.is_prepared() { 65535.0 } else { 32767.0 });
    let gamma = gamma_lut();
    // Monochrome: Sigma multiplies the luminance by the ISO factor (CaptureISO/SensorISO); in the colour path it is part of the matrix
    let iso = cal.iso_gain as f32;
    // SPP computes in sensor counts: (raw − black) · 2 · WB (DP Merrill, WB/0.5;
    // SD1: WB/0.55 → `cal.count_scale`). The tone curve is a table over these counts
    // (psat = 2^ImageDepth − 1); exposure shifts the curve (x3fuse-core).
    let sat = cal.saturation as f32;
    let ratio = cal.wb_ratio(s);
    let gain: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c] * ratio[c]) as f32 * sat);
    let out_m = output_q12(cal);
    // White balance is in `gain` (sensor channels); the output matrix contains temperature/tint
    // Layer mixer: our own layer→RGB matrix replaces Sigma's colour matrix here, ColorDQ
    // is dropped (see [`layers`]); otherwise Sigma's path unchanged.
    // Sigma-Analog: black-and-white film exposed from the layers (see [`analog`]); takes precedence
    let analog_l = analog::layer_matrix(cal, s).filter(|_| matches!(mode_out, Out::Scene(_)));
    // brightness reference of the exposure floor (see [`analog::exposure_floor`])
    let floor_ref = analog_l.map(|_| analog::floor_reference(cal, s).to_f32());
    let layer_m = match analog_l {
        Some(l) => Some(analog::cam_to_input(cal, s, &l)),
        None => layers::active(s).map(|l| layers::cam_to_rgb(cal, s, l)),
    };
    let m = match layer_m {
        Some(lm) => lm.to_f32(),
        None => Mat3::diag(calib::temp_tint_gains(s.wb_temp, s.wb_tint)).mul(&cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref())).to_f32(),
    };
    let mix = s.normalized_mix();
    let dq = cal.color_dq.as_ref().filter(|_| layer_m.is_none() && !s.is_linear());
    let fill = fill_map.filter(|_| s.fill_light != 0.0).map(|map| (map, filllight::FillLightCurve::new(s.fill_light)));
    // Optional highlight neutralisation (SPP: Highlight Control; default 1 = off).
    let k = s.highlight_neutralize.clamp(0.0, 1.0);
    let hc_own = k > 0.0 && x3f_core::merrill_new::active("hc");
    let presence = s.saturation != 0.0 || s.vibrance != 0.0;
    // SPP fringe correction (hue band desaturation, before colour adjustments)
    let spp_fringe = s.spp_defringe.prepare();
    // Merrill (data straight from the sensor, not prepared): layers at their clip level glide to neutral
    // ([`glide_clipped`]); Quattro and Classic handle their highlights while being prepared
    let clip_glide = !cal.is_prepared() && layer_m.is_none() && analog_l.is_none();

    let (mut data, mut scene_data) = out_buffers(lin.width * lin.height * 3, mode_out.float_buffer());
    // Dehaze on the linear sensor values (same as in [`render_counts`])
    let haze_own;
    let haze_field = match (&lin.haze, s.dehaze != 0.0) {
        (_, false) => None,
        (Some(f), true) => Some(&**f),
        (None, true) => {
            haze_own = dehaze::HazeField::analyze(&lin.data, lin.width, lin.height, 1.0);
            Some(&haze_own)
        }
    };
    let dz = haze_field.and_then(|f| f.at(s.dehaze));
    let brake = s.noise_brake.exponents().filter(|_| s.fill_light > 0.0 && fill.is_some()).map(|(l, c)| lift::Brake::new(&lin.data, lin.width, lin.height, l, c));
    // Tone equalizer: exposure per brightness zone, mask of the whole image (attached, else computed for this image);
    // the data are normalised to sensor saturation (white = 1)
    let teq_mask = tone_eq::needed(s).then(|| tone_eq::mask_for(lin.toneq.as_ref(), &lin.data, lin.width, lin.height, 1.0, &s.tone_eq));
    let teq = teq_mask.as_ref().map(|m| tone_eq::Apply::new(&s.tone_eq, m, (s.exposure + quattro_exposure_offset(cal, s)) as f32, s.tone_eq_mask));
    out_rows(&mut data, &mut scene_data, lin.width * 3, mode_out, cal).zip(lin.data.par_chunks(lin.width * 3)).enumerate().for_each(|(y, (mut out, src))| {
        for x in 0..lin.width {
            let px = y * lin.width + x;
            let mut p = [src[x * 3], src[x * 3 + 1], src[x * 3 + 2]];
            let mut bp = brake.as_ref().map(|b| b.base(px));
            if let Some(dz) = &dz {
                p = dz.pixel(px, p);
                bp = bp.map(|b| dz.pixel(px, b));
            }
            let te = match &teq {
                Some(t) if t.show => {
                    out.grey(x, t.mask_grey(x, y, p));
                    continue;
                }
                Some(t) => t.gain(x, y, p),
                None => 1.0,
            };
            let f = te * fill.as_ref().map_or(1.0, |(map, curve)| curve.factor(map[px]));
            let q = match (&brake, bp) {
                (Some(b), Some(bp)) => b.apply(p, bp, f),
                _ => [p[0] * f, p[1] * f, p[2] * f],
            };
            let mut v = [q[0] * gain[0], q[1] * gain[1], q[2] * gain[2]];
            if s.monochrome {
                let l = mono_luma(v, mix, iso, mono_max);
                out.mono(x, l, &tone, gamma);
                continue;
            }
            if clip_glide {
                v = glide_clipped([src[x * 3], src[x * 3 + 1], src[x * 3 + 2]], v);
            }
            let mut rgb = [
                m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
                m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
                m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
            ];
            if let Some(r) = &floor_ref {
                rgb = [analog::exposure_floor(rgb[0], r[0] * v[0] + r[1] * v[1] + r[2] * v[2]); 3];
            }
            if let Some(dq) = dq {
                rgb = dq.apply(rgb);
            }
            if let Some(fr) = &spp_fringe {
                rgb = fr.apply(rgb);
            }
            if presence {
                rgb = apply_presence(rgb, s.saturation, s.vibrance);
            }
            if k > 0.0 {
                let level = 0.03125 * p[0] + 0.03125 * p[1] + 0.9375 * p[2];
                let (t, gray) = match hc_own {
                    true => (hc_own_weight(p) * k, v[0].max(v[1]).max(v[2])),
                    false => (((level - 0.62) / 0.24).clamp(0.0, 1.0) * k, 0.25 * v[0] + 0.5 * v[1] + 0.25 * v[2]),
                };
                if t > 0.0 {
                    for c in &mut rgb {
                        *c += t * (gray - *c);
                    }
                }
            }
            out.rgb(x, rgb, &tone, gamma, out_m.as_ref(), max_count);
        }
        if let Some(a) = axes {
            out.encoded_row(lin.width, |p| a.apply_u16(p));
        }
    });
    RenderOut::new(lin.width, lin.height, data, scene_data, mode_out.float_buffer())
}

/// Output stage on SPP counts (see [`spp::working`]): Fill Light, Sigma tone curve
/// with all sliders, gamma. `mode_matrix` is the colour matrix the counts were
/// produced with (used to go back to camera space for monochrome), `remap`
/// an optional conversion to a different colour mode (preview, until recomputed).
pub fn render_counts(
    counts: &Linear,
    cal: &Calibration,
    s: &Settings,
    mode_matrix: &Mat3,
    fill_map: Option<&[f32]>,
) -> Rgb16 {
    render_counts_impl(counts, cal, s, mode_matrix, fill_map, None, None, Out::Tone).tone()
}

/// Like [`render_counts`], but scene-linear right before Sigma's tone curve (input of the
/// film simulation, see [`film`]): linear sRGB, middle grey 0.184.
pub fn render_counts_scene(counts: &Linear, cal: &Calibration, s: &Settings, mode_matrix: &Mat3, fill_map: Option<&[f32]>) -> Linear {
    render_counts_impl(counts, cal, s, mode_matrix, fill_map, None, None, Out::Scene(film::scene_gain(cal, s))).scene()
}

/// [`render_counts`] with the dehaze analysis and the tone equalizer mask handed in (`haze`, `toneq`: of this image or
/// crop; take precedence over `counts.haze`/`counts.toneq`) – for a shared base that is read by several threads and must
/// not be changed.
pub fn render_counts_with_haze(counts: &Linear, cal: &Calibration, s: &Settings, mode_matrix: &Mat3, fill_map: Option<&[f32]>, haze: Option<&dehaze::HazeField>, toneq: Option<&tone_eq::Mask>) -> Rgb16 {
    render_counts_impl(counts, cal, s, mode_matrix, fill_map, haze, toneq, Out::Tone).tone()
}

/// [`render_counts_scene`] with the dehaze analysis handed in (see [`render_counts_with_haze`]).
pub fn render_counts_scene_with_haze(counts: &Linear, cal: &Calibration, s: &Settings, mode_matrix: &Mat3, fill_map: Option<&[f32]>, haze: Option<&dehaze::HazeField>, toneq: Option<&tone_eq::Mask>) -> Linear {
    render_counts_impl(counts, cal, s, mode_matrix, fill_map, haze, toneq, Out::Scene(film::scene_gain(cal, s))).scene()
}

/// [`render_counts_with_haze`] as extended sRGB-encoded floats (see [`render_wide`]); Sigma's tone
/// table is used exactly as in the 16-bit output for values inside its range.
pub fn render_counts_wide_with_haze(counts: &Linear, cal: &Calibration, s: &Settings, mode_matrix: &Mat3, fill_map: Option<&[f32]>, haze: Option<&dehaze::HazeField>, toneq: Option<&tone_eq::Mask>) -> Linear {
    render_counts_impl(counts, cal, s, mode_matrix, fill_map, haze, toneq, Out::Wide { fine: false }).scene()
}

fn render_counts_impl(
    counts: &Linear,
    cal: &Calibration,
    s: &Settings,
    mode_matrix: &Mat3,
    fill_map: Option<&[f32]>,
    haze: Option<&dehaze::HazeField>,
    toneq: Option<&tone_eq::Mask>,
    mode_out: Out,
) -> RenderOut {
    let mode = cal.mode(s.color_mode.as_deref());
    // sd Quattro colour mode fine correction: already part of the counts (linear colour stage, see `quattro::to_counts`)
    let classic_sigma = mode_out.is_tone() && !s.monochrome && !s.is_linear();
    let classic_new_from = mode.classic.as_deref().zip(mode.classic_output.as_deref()).filter(|_| classic_sigma).map(|(c, basis)| {
        let k: [f64; 3] = std::array::from_fn(|i| c.wb_gain[i] / (c.k_raw[i] * cal.wb_gain[i]));
        let g: [f64; 3] = std::array::from_fn(|i| cal.count_scale * cal.wb_gain[i] * cal.saturation);
        let m: [f32; 9] = std::array::from_fn(|i| (f64::from(c.a[i]) / (k[i % 3] * g[i % 3]) * 4096.0) as f32);
        // colour adjustment: the gains behind temperature/tint (their scale does not matter, the step
        // normalises them to the image mean); saturation in the settings is half of Sigma's slider
        let slider = classic_new::output::Slider {
            exposure: s.exposure,
            contrast: s.contrast,
            shadows: s.shadow,
            highlights: s.highlight,
            saturation: f64::from(s.saturation) * 2.0,
            sharpness: f64::from(s.sigma_sharpness),
            fill_light: f64::from(s.fill_light),
            color_balance: calib::temp_tint_gains(s.wb_temp, s.wb_tint),
        };
        // sharpness: the chain ran with amount 1.0; its high-pass rides along with the counts and is
        // added again with the slider's amount (full resolution and crops; downscaled previews get a
        // high-pass of their own below)
        let hp = counts.detail.as_deref().filter(|d| d.is_classic_high_pass() && d.l.len() == counts.width * counts.height).map(|d| d.l.as_slice());
        let magnitude = classic_new::detail::magnitude(sharpness::quantize(s.sigma_sharpness));
        (basis.output(&slider, false), m, hp.filter(|_| magnitude != 1 << 14), magnitude)
    });
    let tone = output_tone(cal, s, mode);
    let (max_count, mono_max) = (cal.max_count(), if cal.is_prepared() { 65535.0 } else { 32767.0 });
    let gamma = gamma_lut();
    // Quattro: bright colours beyond the output range are brought into it by desaturating towards white instead of
    // clipping channels (`highlight_gamut`); the white is the count where the tone curve reaches its top
    // With the highlight rebuild on (`highlight_rebuild`), the channels clip as in the maker's output: the rebuilt
    // surfaces are brighter than white in a colour, and the roll-off towards white took exactly that colour (measured
    // on a blown cyan label against the maker's TIFF: ΔE00 where it stays coloured 12.4 → 3.2); `GRAINEO_QUATTRO_GAMUT=1`
    // keeps the roll-off
    let gamut_white = (cal.is_quattro() && !s.monochrome && !matches!(mode_out, Out::Scene(_)) && std::env::var("GRAINEO_QUATTRO_GAMUT").map_or_else(|_| !highlight_rebuild::enabled_quattro(), |v| v != "0"))
        .then(|| tone_white(&tone, max_count))
        .filter(|w| *w > 0.0);
    // Monochrome: Sigma multiplies the luminance by the ISO factor (CaptureISO/SensorISO); in the colour path it is part of the matrix
    let iso = cal.iso_gain as f32;
    let inv = mode_matrix.inverse().unwrap_or(Mat3::IDENTITY);
    let to_cam = inv.to_f32();
    // Different colour mode or white balance than when computing the counts: convert
    // linearly (back to camera space, channel gain, new matrix).
    let ratio = cal.wb_ratio(s);
    let target = cal.output_matrix(s).mul(&inv);
    let remap = (target.0.iter().zip(Mat3::IDENTITY.0.iter()).any(|(a, b)| (a - b).abs() > 1e-9)).then(|| target.to_f32());
    // Layer mixer: counts back to camera space as for monochrome (inverse of the
    // colour mode matrix, white balance ratio), then our own layer→RGB matrix instead of
    // Sigma's matrix, all in one matrix in place of `remap` (see [`layers`]).
    let analog_l = analog::layer_matrix(cal, s).filter(|_| matches!(mode_out, Out::Scene(_)));
    // brightness reference of the exposure floor (see [`analog::exposure_floor`]), on the same input as `remap`
    let floor_ref = analog_l.map(|_| analog::floor_reference(cal, s).mul(&Mat3::diag(ratio)).mul(&inv).to_f32());
    let remap = match (analog_l, layers::active(s)) {
        (Some(l), _) => Some(analog::cam_to_input(cal, s, &l).mul(&Mat3::diag(ratio)).mul(&inv).to_f32()),
        (None, Some(l)) => Some(layers::cam_to_rgb(cal, s, l).mul(&Mat3::diag(ratio)).mul(&inv).to_f32()),
        (None, None) => remap,
    };
    let ratio = ratio.map(|r| r as f32);
    let presence = s.saturation != 0.0 || s.vibrance != 0.0;
    // SPP fringe correction: in SPP after sharpening, before colour adjustments, Fill Light and tone
    // curve; it only changes saturation (not hue or V), so the Fill Light gain before it does not matter
    let spp_fringe = s.spp_defringe.prepare();
    let mix = s.normalized_mix();
    let fill = fill_map.filter(|_| s.fill_light != 0.0).map(|map| (map, filllight::FillLightCurve::new(s.fill_light)));
    let ev = 1.0f32; // exposure is in the tone curve
    let out_m = output_q12(cal);
    // Sigma's sharpness slider: on the counts before everything else
    // (sharpness 0 → nothing to do, bit-identical to the base).
    let sharp = counts.detail.as_deref().filter(|d| !counts.mono && d.l.len() == counts.width * counts.height).and_then(|d| Some((d, d.amounts(s.sigma_sharpness)?)));
    // alternative sharpening form (test switch GRAINEO_MERRILL_NEW=sharpness)
    let sharp_own = sharp.is_some() && x3f_core::merrill_new::active("sharpness");
    // Highlight neutralisation as in [`render`]: there the sensor level is taken from the normalised
    // raw values; here from the counts converted back to camera space, divided
    // by the same gain (count scale · white balance · saturation).
    let k = s.highlight_neutralize.clamp(0.0, 1.0);
    let hc_own = k > 0.0 && x3f_core::merrill_new::active("hc");
    let sat = cal.saturation as f32;
    let level_div: [f32; 3] = std::array::from_fn(|c| 1.0 / ((cal.count_scale * cal.wb_gain[c]) as f32 * sat).max(1e-6));
    // Grey base of Sigma's monochrome processing: grey image (mix, sharpening) computed first
    let grey = counts.mono.then(|| mono::grey(counts, s));
    let (mut data, mut scene_data) = out_buffers(counts.width * counts.height * 3, mode_out.float_buffer());
    // Experimental noise-aware brightening (GRAINEO_LIFT, only with GRAINEO_NO_DISK_CACHE, see lift.rs)
    let lifted = lift::from_env()
        .filter(|_| std::env::var_os("GRAINEO_NO_DISK_CACHE").is_some())
        .map(|p| lift::lift(&counts.data, counts.width, counts.height, &p));
    let src_data: &[f32] = lifted.as_deref().unwrap_or(&counts.data);
    // Classic sharpness in a downscaled preview (the counts carry only a marker with the scale, no
    // full-resolution high-pass): a high-pass of the preview's own linear codes
    let classic_preview_hp: Option<Vec<i16>> = classic_new_from.as_ref().filter(|(_, _, hp, magnitude)| hp.is_none() && *magnitude != 1 << 14).and_then(|(_, m_from, _, _)| {
        let d = counts.detail.as_deref().filter(|d| d.is_classic_high_pass() && d.l.is_empty())?;
        let m = &to_cam;
        let codes: Vec<[f32; 3]> = src_data
            .par_chunks(3)
            .map(|p| {
                let v = [
                    (m[0] * p[0] + m[1] * p[1] + m[2] * p[2]) * ratio[0],
                    (m[3] * p[0] + m[4] * p[1] + m[5] * p[2]) * ratio[1],
                    (m[6] * p[0] + m[7] * p[1] + m[8] * p[2]) * ratio[2],
                ];
                std::array::from_fn(|r| m_from[r * 3] * v[0] + m_from[r * 3 + 1] * v[1] + m_from[r * 3 + 2] * v[2])
            })
            .collect();
        Some(classic_new::detail::preview_high_pass(&codes, counts.width, counts.height, d.scale))
    });
    // Dehaze: analysis handed in or attached to the counts (full base and its crops), else computed for this whole image
    let haze_own;
    let haze_field = match (haze.or(counts.haze.as_deref()), s.dehaze != 0.0) {
        (_, false) => None,
        (Some(f), true) => Some(f),
        (None, true) => {
            haze_own = dehaze::HazeField::analyze(&counts.data, counts.width, counts.height, 1.0);
            Some(&haze_own)
        }
    };
    let dz = haze_field.and_then(|f| f.at(s.dehaze));
    // Noise brake for Fill Light: base/detail split of the input (see lift::Brake)
    let brake = s.noise_brake.exponents().filter(|_| s.fill_light > 0.0 && fill.is_some()).map(|(l, c)| lift::Brake::new(src_data, counts.width, counts.height, l, c));
    // Tone equalizer on the counts (white = highest count): mask handed in, attached, or computed for this image
    let teq_mask = tone_eq::needed(s).then(|| tone_eq::mask_for(toneq.or(counts.toneq.as_ref()), src_data, counts.width, counts.height, cal.max_count(), &s.tone_eq));
    let teq = teq_mask.as_ref().map(|m| tone_eq::Apply::new(&s.tone_eq, m, (s.exposure + quattro_exposure_offset(cal, s)) as f32, s.tone_eq_mask));
    out_rows(&mut data, &mut scene_data, counts.width * 3, mode_out, cal).zip(src_data.par_chunks(counts.width * 3)).enumerate().for_each(|(y, (mut out, src))| {
        for x in 0..counts.width {
            let mut p = [src[x * 3], src[x * 3 + 1], src[x * 3 + 2]];
            if let Some(g) = &grey {
                p = [g[y * counts.width + x]; 3];
            }
            if let Some((d, (amt0, amt))) = sharp {
                p = if sharp_own {
                    sharpness::own_sharpen_px(p, d.l[y * counts.width + x], amt - amt0)
                } else {
                    sharpness::resharpen_px(p, d.l[y * counts.width + x], amt0, amt)
                };
            }
            let px = y * counts.width + x;
            let mut bp = brake.as_ref().map(|b| b.base(px));
            if let Some(dz) = &dz {
                p = dz.pixel(px, p);
                bp = bp.map(|b| dz.pixel(px, b));
            }
            let te = match &teq {
                Some(t) if t.show => {
                    out.grey(x, t.mask_grey(x, y, p));
                    continue;
                }
                Some(t) => t.gain(x, y, p),
                None => 1.0,
            };
            let f = ev * te * fill.as_ref().map_or(1.0, |(map, curve)| curve.factor(map[px]));
            let mut rgb = match (&brake, bp) {
                (Some(b), Some(bp)) => b.apply(p, bp, f),
                _ => [p[0] * f, p[1] * f, p[2] * f],
            };
            if grey.is_some() {
                // grey base: the grey value is final (colour settings during a switch show it neutral)
                let l = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
                if s.monochrome {
                    out.mono(x, l.clamp(0.0, mono_max), &tone, gamma);
                } else {
                    out.rgb(x, [l; 3], &tone, gamma, out_m.as_ref(), max_count);
                }
                continue;
            }
            if s.monochrome {
                let m = &to_cam;
                let v = [
                    (m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2]) * ratio[0],
                    (m[3] * rgb[0] + m[4] * rgb[1] + m[5] * rgb[2]) * ratio[1],
                    (m[6] * rgb[0] + m[7] * rgb[1] + m[8] * rgb[2]) * ratio[2],
                ];
                let l = mono_luma(v, mix, iso, mono_max);
                out.mono(x, l, &tone, gamma);
                continue;
            }
            if let (Some((outp, m_from, hp, magnitude)), RowOut::Tone(o) | RowOut::Fine(o, _)) = (&classic_new_from, &mut out) {
                let m = &to_cam;
                let mut v = [
                    (m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2]) * ratio[0],
                    (m[3] * rgb[0] + m[4] * rgb[1] + m[5] * rgb[2]) * ratio[1],
                    (m[6] * rgb[0] + m[7] * rgb[1] + m[8] * rgb[2]) * ratio[2],
                ];
                if s.vibrance != 0.0 {
                    v = apply_presence(v, 0.0, s.vibrance);
                }
                let m = m_from;
                let lin: [f32; 3] = std::array::from_fn(|r| m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2]);
                let hp_px = hp.map(|hp| hp[px]).or_else(|| classic_preview_hp.as_ref().map(|v| v[px]));
                let from_px = match hp_px {
                    Some(h) => outp.pixel_with_sharpness(lin, h, *magnitude),
                    None => outp.pixel(lin.map(|l| l.round() as i32)),
                };
                o[x * 3..x * 3 + 3].copy_from_slice(&from_px);
                continue;
            }
            if let Some(m) = &remap {
                let input = rgb;
                rgb = [
                    m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2],
                    m[3] * rgb[0] + m[4] * rgb[1] + m[5] * rgb[2],
                    m[6] * rgb[0] + m[7] * rgb[1] + m[8] * rgb[2],
                ];
                if let Some(r) = &floor_ref {
                    rgb = [analog::exposure_floor(rgb[0], r[0] * input[0] + r[1] * input[1] + r[2] * input[2]); 3];
                }
            }
            if let Some(fr) = &spp_fringe {
                rgb = fr.apply(rgb);
            }
            if presence {
                rgb = apply_presence(rgb, s.saturation, s.vibrance);
            }
            if k > 0.0 {
                let m = &to_cam;
                let q = [p[0] * f, p[1] * f, p[2] * f];
                let cam = [
                    m[0] * q[0] + m[1] * q[1] + m[2] * q[2],
                    m[3] * q[0] + m[4] * q[1] + m[5] * q[2],
                    m[6] * q[0] + m[7] * q[1] + m[8] * q[2],
                ];
                let lv = [cam[0] * level_div[0], cam[1] * level_div[1], cam[2] * level_div[2]];
                let level = 0.03125 * lv[0] + 0.03125 * lv[1] + 0.9375 * lv[2];
                let t = if hc_own { hc_own_weight(lv) * k } else { ((level - 0.62) / 0.24).clamp(0.0, 1.0) * k };
                if t > 0.0 {
                    let v = [cam[0] * ratio[0], cam[1] * ratio[1], cam[2] * ratio[2]];
                    let gray = if hc_own { v[0].max(v[1]).max(v[2]) } else { 0.25 * v[0] + 0.5 * v[1] + 0.25 * v[2] };
                    for c in &mut rgb {
                        *c += t * (gray - *c);
                    }
                }
            }
            if let Some(w) = gamut_white {
                rgb = highlight_gamut(rgb, w);
            }
            out.rgb(x, rgb, &tone, gamma, out_m.as_ref(), max_count);
        }
    });
    RenderOut::new(counts.width, counts.height, data, scene_data, mode_out.float_buffer())
}

/// Alternative Highlight Control (test switch `GRAINEO_MERRILL_NEW=hc`), with the ramp of dcraw's
/// "adjust the brighter pixels" (D. Coffin, dcraw.c, GPL-2+): the weight rises with the level `hi` of the
/// brightest layer (relative to saturation) as `(1 − (1 − u)²)²`, `u = (hi − HC_START)/HC_WIDTH`, and the
/// pixel moves towards its brightest layer (neutral and bright, as in dcraw). Start and width measured
/// against the default form (`graineo-dev merrill-new-fit`, 7 Merrill files: rms of the weight 0.014).
#[inline]
fn hc_own_weight(levels: [f32; 3]) -> f32 {
    let hi = levels[0].max(levels[1]).max(levels[2]);
    let u = ((hi - HC_START) / HC_WIDTH).clamp(0.0, 1.0);
    let i = 1.0 - (1.0 - u) * (1.0 - u);
    i * i
}

/// Quick pipeline, Merrill: a layer standing at the sensor's clip level carries no colour information any more (the
/// top layer clips first; with its value too low the colour matrix turns bright foliage into flat full yellow without
/// any drawing). Such a pixel glides towards the bottom layer, i.e. towards neutral, as the highlight stage of the exact
/// pipeline does: weight = smoothstep of the brightest layer level `raw` (relative to sensor saturation) between
/// [`CLIP_GLIDE_START`] and 1, applied to the white-balanced layers `v`. Pixels below the start are returned unchanged.
#[inline]
fn glide_clipped(raw: [f32; 3], v: [f32; 3]) -> [f32; 3] {
    let hi = raw[0].max(raw[1]).max(raw[2]);
    if hi <= CLIP_GLIDE_START {
        return v;
    }
    let t = ((hi - CLIP_GLIDE_START) / (1.0 - CLIP_GLIDE_START)).min(1.0);
    let w = t * t * (3.0 - 2.0 * t);
    v.map(|c| c + w * (v[0] - c))
}

/// Start of [`glide_clipped`] (layer level relative to sensor saturation).
const CLIP_GLIDE_START: f32 = 0.85;

/// [`hc_own_weight`]: start and width of the ramp (levels relative to saturation).
pub const HC_START: f32 = 0.61;
pub const HC_WIDTH: f32 = 0.34;

/// Quattro: exposure correction for the chosen white balance ([`Calibration::sigma_wb_ev`]): the white balance gains
/// are normalised to the middle layer (green = 1, the usual convention of raw converters).
fn quattro_exposure_offset(cal: &Calibration, s: &Settings) -> f64 {
    if cal.sigma_wb_ev.is_empty() {
        return 0.0;
    }
    let find = |n: &str| cal.sigma_wb_ev.iter().find(|(k, _)| k == n).map(|(_, v)| *v);
    s.white_balance.as_deref().and_then(find).or_else(|| find(&cal.wb_name)).unwrap_or(0.0)
}

/// Grey value of the monochrome mix (counts): mix, ISO factor, clamped.
#[inline]
fn mono_luma(v: [f32; 3], mix: [f32; 3], iso: f32, max: f32) -> f32 {
    ((mix[0] * v[0].max(0.0) + mix[1] * v[1].max(0.0) + mix[2] * v[2].max(0.0)) * iso).clamp(0.0, max)
}

/// Result of the output stage: finished with Sigma's tone curve or scene-linear for the film.
enum RenderOut {
    Tone(Rgb16),
    Scene(Linear),
}

impl RenderOut {
    fn new(width: usize, height: usize, data: Vec<u16>, scene_data: Vec<f32>, scene: bool) -> Self {
        if scene {
            RenderOut::Scene(Linear { mono: false, haze: None, toneq: None, width, height, data: scene_data, detail: None })
        } else {
            RenderOut::Tone(Rgb16 { width, height, data })
        }
    }
    fn tone(self) -> Rgb16 {
        match self {
            RenderOut::Tone(img) => img,
            RenderOut::Scene(_) => unreachable!("Tonkurven-Ausgabe erwartet"),
        }
    }
    fn scene(self) -> Linear {
        match self {
            RenderOut::Scene(lin) => lin,
            RenderOut::Tone(_) => unreachable!("szenen-lineare Ausgabe erwartet"),
        }
    }
}

/// Output buffer: only the one needed is allocated.
fn out_buffers(n: usize, scene: bool) -> (Vec<u16>, Vec<f32>) {
    if scene { (Vec::new(), vec![0f32; n]) } else { (vec![0u16; n], Vec::new()) }
}

/// Rows of the output buffer as a parallel sequence of [`RowOut`].
fn out_rows<'a>(
    data: &'a mut [u16],
    scene_data: &'a mut [f32],
    row: usize,
    mode: Out,
    cal: &Calibration,
) -> impl IndexedParallelIterator<Item = RowOut<'a>> {
    use rayon::iter::Either;
    // Adobe RGB shots: the counts are interpreted as Adobe RGB (as SPP does) → linear to sRGB
    let adobe = cal.output_to_srgb.map(|m| m.to_f32());
    match mode {
        Out::Tone | Out::Fine => {
            let fine = matches!(mode, Out::Fine);
            Either::Left(data.par_chunks_mut(row).map(move |r| if fine { RowOut::Fine(r, adobe) } else { RowOut::Tone(r) }))
        }
        Out::Wide { .. } | Out::Scene(_) => Either::Right(scene_data.par_chunks_mut(row).map(move |r| match mode {
            Out::Wide { fine } => RowOut::Wide(r, fine, adobe),
            Out::Scene(gain) => RowOut::Scene(r, gain, adobe),
            _ => unreachable!(),
        })),
    }
}

/// Target of an image row: Sigma's tone curve + gamma (16 bit) or scene-linear (film simulation;
/// factor to middle grey 0.184 and optional Adobe RGB→sRGB matrix).
enum RowOut<'a> {
    Tone(&'a mut [u16]),
    /// 16 bit with interpolated tone curve and exact gamma; optional output matrix in float
    Fine(&'a mut [u16], Option<[f32; 9]>),
    /// extended sRGB-encoded floats (wide-gamut export); `bool`: interpolate the tone curve
    Wide(&'a mut [f32], bool, Option<[f32; 9]>),
    Scene(&'a mut [f32], f32, Option<[f32; 9]>),
}

/// Kind of output of the render stage.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Out {
    /// Sigma's tone curve and gamma tables, 16 bit
    Tone,
    /// as `Tone`, but interpolated tone curve and exact gamma (true 16-bit gradations)
    Fine,
    /// extended sRGB-encoded floats for wide-gamut exports
    Wide { fine: bool },
    /// scene-linear for the film simulation and float exports (gain to middle grey)
    Scene(f32),
}

impl Out {
    fn is_tone(self) -> bool {
        matches!(self, Out::Tone | Out::Fine)
    }
    fn float_buffer(self) -> bool {
        matches!(self, Out::Wide { .. } | Out::Scene(_))
    }
}

/// Tone curve between its table entries (linear interpolation over the counts).
#[inline]
fn tone_fine(v: f32, tone: &[u16]) -> f32 {
    let v = v.max(0.0);
    let i = v as usize;
    if i + 1 >= tone.len() {
        return tone[tone.len() - 1] as f32;
    }
    let f = v - i as f32;
    tone[i] as f32 + f * (tone[i + 1] as f32 - tone[i] as f32)
}

/// Tone curve over the whole real axis for wide-gamut output: inside `0..=max` as in the 16-bit
/// output (table or interpolated), above `max` continued proportionally, below 0 point-symmetric.
#[inline]
fn tone_wide(v: f32, tone: &[u16], max: f32, fine: bool) -> f32 {
    let at = |a: f32| if fine { tone_fine(a, tone) } else { tone[sigma_out::tone_index(a)] as f32 };
    let a = v.abs();
    let t = if a <= max { at(a) } else { at(max) * a / max.max(1.0) };
    if v < 0.0 { -t } else { t }
}

/// Exact sRGB gamma of a tone value (scale of Sigma's table: value / 65536), rounded to 16 bits.
#[inline]
fn gamma_fine(t: f32) -> u16 {
    (srgb_encode((t / 65536.0).clamp(0.0, 1.0)) * 65535.0 + 0.5) as u16
}

#[inline]
fn mat_px(m: &[f32; 9], v: [f32; 3]) -> [f32; 3] {
    [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
}

impl RowOut<'_> {
    #[inline]
    fn mono(&mut self, x: usize, l: f32, tone: &[u16], gamma: &[u16]) {
        match self {
            RowOut::Tone(out) => out[x * 3..x * 3 + 3].fill(gamma[tone[sigma_out::tone_index(l)] as usize]),
            RowOut::Fine(out, _) => out[x * 3..x * 3 + 3].fill(gamma_fine(tone_fine(l, tone))),
            RowOut::Wide(out, fine, _) => out[x * 3..x * 3 + 3].fill(graineo_color::srgb_encode(tone_wide(l, tone, (tone.len() - 1) as f32, *fine) / 65536.0)),
            RowOut::Scene(out, k, _) => out[x * 3..x * 3 + 3].fill(l * *k),
        }
    }

    /// A neutral grey `v` (display-encoded 0 … 1) at pixel `x`, past the tone curve (tone equalizer "show mask").
    fn grey(&mut self, x: usize, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match self {
            RowOut::Tone(out) | RowOut::Fine(out, _) => out[x * 3..x * 3 + 3].fill((v * 65535.0 + 0.5) as u16),
            RowOut::Wide(out, ..) => out[x * 3..x * 3 + 3].fill(v),
            RowOut::Scene(out, ..) => out[x * 3..x * 3 + 3].fill(v.powf(2.2)),
        }
    }

    /// A change of the finished (gamma-encoded) 16-bit row of `w` pixels; not for the film input.
    fn encoded_row(&mut self, w: usize, f: impl Fn(&mut [u16])) {
        match self {
            RowOut::Tone(out) | RowOut::Fine(out, _) => f(&mut out[..w * 3]),
            // on the clipped 16-bit pixels; the change is carried over to the extended values
            RowOut::Wide(out, ..) => {
                let p = &mut out[..w * 3];
                let q: Vec<u16> = p.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect();
                let mut r = q.clone();
                f(&mut r);
                for i in 0..p.len() {
                    p[i] += (r[i] as f32 - q[i] as f32) / 65535.0;
                }
            }
            RowOut::Scene(..) => {}
        }
    }

    #[inline]
    fn rgb(&mut self, x: usize, rgb: [f32; 3], tone: &[u16], gamma: &[u16], m: Option<&[i32; 9]>, max_count: f32) {
        match self {
            RowOut::Tone(out) => write_px(&mut out[x * 3..x * 3 + 3], rgb, tone, gamma, m, max_count),
            RowOut::Fine(out, adobe) => {
                let t = rgb.map(|v| tone_fine(v.clamp(0.0, max_count), tone));
                let t = match adobe {
                    Some(a) => mat_px(a, t),
                    None => t,
                };
                for c in 0..3 {
                    out[x * 3 + c] = gamma_fine(t[c]);
                }
            }
            RowOut::Wide(out, fine, adobe) => {
                let t = rgb.map(|v| tone_wide(v, tone, max_count, *fine));
                let t = match adobe {
                    Some(a) => mat_px(a, t),
                    None => t,
                };
                for c in 0..3 {
                    out[x * 3 + c] = graineo_color::srgb_encode(t[c] / 65536.0);
                }
            }
            RowOut::Scene(out, k, adobe) => {
                let v = match adobe {
                    Some(a) => [
                        a[0] * rgb[0] + a[1] * rgb[1] + a[2] * rgb[2],
                        a[3] * rgb[0] + a[4] * rgb[1] + a[5] * rgb[2],
                        a[6] * rgb[0] + a[7] * rgb[1] + a[8] * rgb[2],
                    ],
                    None => rgb,
                };
                out[x * 3..x * 3 + 3].copy_from_slice(&[v[0] * *k, v[1] * *k, v[2] * *k]);
            }
        }
    }
}

/// Smallest count at which the tone curve `tone` reaches its top value (output white), at most `max_count`.
fn tone_white(tone: &[u16], max_count: f32) -> f32 {
    let n = (max_count as usize + 1).min(tone.len());
    let Some(&top) = tone[..n].iter().max() else {
        return 0.0;
    };
    tone[..n].iter().position(|&t| t >= top).map_or(0.0, |i| i as f32)
}

/// [`highlight_gamut`]: chroma (relative to the largest that fits) from which it is compressed softly.
const GAMUT_SOFT: f32 = 0.8;
/// [`highlight_gamut`]: share of white from which the luminance of coloured pixels rolls off towards white.
const GAMUT_SHOULDER: f32 = 0.9;
/// From this share of the output white on, [`highlight_gamut`] starts.
const GAMUT_KNEE: f32 = 0.8;

/// Bright colours that leave the output range (a channel above `white` or below 0, linear RGB counts). Clipping
/// each channel turns a bright saturated colour into a flat full tone (0/255/255) without any drawing. Instead the
/// pixel is split into its luminance `y` and its chroma `rgb − y` (hue kept):
/// - the target luminance follows `y` up to white; for coloured pixels it rolls off softly above [`GAMUT_SHOULDER`]
///   · `white` towards white (never reaching it), so a colour brighter than white stays a light colour, distinct from
///   white text on it, and still gets lighter the brighter it is;
/// - the chroma is compressed softly from [`GAMUT_SOFT`] of the largest that fits on, and then only reduced as far as
///   the target luminance needs to fit into the output range: the output brightens monotonically with the input
///   (no plateau where different brightnesses map to one colour, no steps when the input varies slightly);
/// - the chroma never exceeds that of the clipped pixel (max − min after clipping), so areas that clipping shows
///   white stay white;
/// - pixels inside the output range with moderate chroma and neutral pixels up to white are unchanged.
/// The change fades in with smoothstep between [`GAMUT_KNEE`] · `white` and `white` of the largest channel; darker
/// pixels are unchanged.
#[inline]
fn highlight_gamut(rgb: [f32; 3], white: f32) -> [f32; 3] {
    let hi = rgb[0].max(rgb[1]).max(rgb[2]);
    let knee = GAMUT_KNEE * white;
    if hi <= knee {
        return rgb;
    }
    let soft = |x: f32, a: f32| if x <= a { x } else { a + (1.0 - a) * (1.0 - (-(x - a) / (1.0 - a)).exp()) };
    let y = (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]).max(0.0);
    let d = rgb.map(|v| v - y);
    let (d_hi, d_lo) = (d[0].max(d[1]).max(d[2]).max(0.0), d[0].min(d[1]).min(d[2]).min(0.0));
    let span = d_hi - d_lo;
    // chroma relative to the largest that fits (1 = touches black and white at once)
    let r = span / white;
    if r <= GAMUT_SOFT && rgb.iter().all(|v| (0.0..=white).contains(v)) {
        return rgb;
    }
    let mut k = if r <= GAMUT_SOFT { 1.0 } else { soft(r, GAMUT_SOFT) / r };
    // target luminance: as is for neutral pixels, with a soft shoulder for coloured ones
    let l = y / white;
    let c = (r / 0.25).min(1.0);
    let g = white * ((1.0 - c) * l.min(1.0) + c * soft(l, GAMUT_SHOULDER));
    if d_hi > 0.0 {
        k = k.min((white - g) / d_hi);
    }
    if d_lo < 0.0 {
        k = k.min(g / -d_lo);
    }
    // never more colourful than the clipped pixel
    let clipped = rgb.map(|v| v.clamp(0.0, white));
    let spread_clipped = clipped[0].max(clipped[1]).max(clipped[2]) - clipped[0].min(clipped[1]).min(clipped[2]);
    if k * span > spread_clipped {
        k = spread_clipped / span.max(1e-6);
    }
    let k = k.max(0.0);
    let fitted = d.map(|dc| g + k * dc);
    let t = ((hi - knee) / (white - knee)).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    std::array::from_fn(|c| rgb[c] + t * (fitted[c] - rgb[c]))
}

/// Saturation (uniform) and vibrance (stronger for pale colours) in the linear
/// output space, around the Rec. 709 luminance.
#[inline]
fn apply_presence(rgb: [f32; 3], saturation: f32, vibrance: f32) -> [f32; 3] {
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    let (mx, mn) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    let current = if mx > 1e-6 { ((mx - mn) / mx).clamp(0.0, 1.0) } else { 0.0 };
    let v = if vibrance > 0.0 { vibrance * (1.0 - current).powi(2) } else { vibrance * current };
    let k = ((1.0 + saturation) * (1.0 + v)).max(0.0);
    [y + (rgb[0] - y) * k, y + (rgb[1] - y) * k, y + (rgb[2] - y) * k]
}

/// Output matrix after the tone curve in Q12 as in SPP, only for Adobe RGB shots.
fn output_q12(cal: &Calibration) -> Option<[i32; 9]> {
    cal.output_to_srgb.map(|m| m.0.map(|v| v.mul_add(4096.0, 0.5) as i32))
}

/// Tone curve, optional output matrix (Q12), gamma for one pixel.
#[inline]
fn write_px(out: &mut [u16], rgb: [f32; 3], tone: &[u16], gamma: &[u16], m: Option<&[i32; 9]>, max_count: f32) {
    let t = rgb.map(|v| tone[sigma_out::tone_index(v.clamp(0.0, max_count))]);
    match m {
        None => {
            for c in 0..3 {
                out[c] = gamma[t[c] as usize];
            }
        }
        Some(m) => {
            for r in 0..3 {
                let mut sum = 2048_i32;
                for c in 0..3 {
                    sum = sum.wrapping_add(m[3 * r + c].wrapping_mul(t[c] as i32));
                }
                out[r] = gamma[(sum >> 12).clamp(0, 65535) as usize];
            }
        }
    }
}

pub(crate) fn gamma_lut() -> &'static [u16] {
    static LUT: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    LUT.get_or_init(sigma_out::srgb_gamma_lut)
}

/// Tone table of the output stage: Sigma's tone curve of the colour mode, or for the linear rendering
/// ([`Settings::is_linear`]) a straight line through the scene-linear values (middle grey where the film simulation puts
/// it, white = 1, clipped above), with the contrast slider as a slope around middle grey.
fn output_tone(cal: &Calibration, s: &Settings, mode: &calib::ColorMode) -> std::sync::Arc<Vec<u16>> {
    let shifted;
    let ts = match quattro_exposure_offset(cal, s) {
        0.0 => s,
        off => {
            shifted = Settings { exposure: s.exposure + off, ..s.clone() };
            &shifted
        }
    };
    if s.is_linear() {
        return linear_tone_lut(film::scene_gain(cal, ts), s.contrast as f32);
    }
    cached_tone_lut(&mode.tone, ts, if s.monochrome { 0.0 } else { mode.contrast }, cal.pixel_saturation, cal.is_prepared())
}

/// Straight tone table: count `i` → linear light `i · gain` (0 … 65535), contrast `c` (−1 … +1) as a slope in log
/// space around middle grey 0.184.
fn linear_tone_lut(gain: f32, c: f32) -> std::sync::Arc<Vec<u16>> {
    let k = 1.0 + 0.6 * c.clamp(-1.0, 1.0);
    std::sync::Arc::new(
        (0..65536)
            .map(|i| {
                let y = i as f32 * gain;
                let y = if k == 1.0 || y <= 0.0 { y } else { 0.184 * (y / 0.184).powf(k) };
                (y.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
            })
            .collect(),
    )
}

/// Tone curve LUT with a small cache (generating it costs a few ms).
pub(crate) fn cached_tone_lut(row: &calib::ToneRow, s: &Settings, mode_contrast: f64, psat: f64, quattro: bool) -> std::sync::Arc<Vec<u16>> {
    use std::sync::{Arc, Mutex};
    type Key = [u64; 13];
    static CACHE: Mutex<Vec<(Key, Arc<Vec<u16>>)>> = Mutex::new(Vec::new());
    let key: Key = [
        row.start_bits(), row.f_end.to_bits(), row.steep.to_bits(), row.steep2.to_bits(), row.brk.to_bits(),
        row.scale.to_bits(), s.exposure.to_bits(), (s.contrast + mode_contrast).to_bits(), s.shadow.to_bits(),
        s.highlight.to_bits(), s.monochrome as u64, psat.to_bits(), quattro as u64,
    ];
    if let Some(t) = row.bracket.or_else(sfd::bracket_tone).filter(|_| quattro && !s.monochrome) {
        // merged bracket: its own tone curve (`sfd`)
        return sfd::cached_lut(&t, s.exposure, s.contrast + mode_contrast, s.shadow, s.highlight);
    }
    if let Some((_, lut)) = CACHE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return lut.clone();
    }
    let shape = sigma_out::ToneShape {
        start: row.f_start,
        end: row.f_end,
        steepness: row.steep,
        steepness2: row.steep2,
        breakpoint: row.brk,
        scale: row.scale,
    };
    let settings = sigma_out::ToneSettings {
        pixel_saturation: psat,
        exposure: s.exposure,
        shadows: s.shadow,
        contrast: s.contrast + mode_contrast,
        highlights: s.highlight,
        mono: s.monochrome,
        quattro,
    };
    let lut = Arc::new(sigma_out::tone_lut(shape, settings).unwrap_or_else(|| (0..65536).map(|i| i.min(65535) as u16).collect()));
    let mut cache = CACHE.lock().unwrap();
    if cache.len() >= 16 {
        cache.remove(0);
    }
    cache.push((key, lut.clone()));
    lut
}

/// Sensor views: a single layer or all three as R/G/B, without white balance,
/// colour matrix and tone curve, only with a brightness factor and sRGB gamma for display.
fn render_sensor_view(lin: &Linear, s: &Settings) -> Rgb16 {
    let enc = EncodeLut::new();
    let gain = 2f32.powf(s.view_gain_ev);
    let mut data = vec![0u16; lin.width * lin.height * 3];
    data.par_chunks_mut(3).zip(lin.data.par_chunks(3)).for_each(|(o, p)| match s.view {
        View::Layer(c) => o.fill(enc.get(p[c.min(2)] * gain)),
        // Raw clipping: image in muted grey, saturated layers in colour (bottom red, middle green, top blue)
        View::Clipping => {
            let clip = [p[0] >= 0.97, p[1] >= 0.97, p[2] >= 0.97];
            if clip.iter().any(|&c| c) {
                for c in 0..3 {
                    o[c] = if clip[c] { 65535 } else { 6000 };
                }
            } else {
                o.fill(enc.get(p[2] * 0.35));
            }
        }
        // Colour share: each layer divided by the brightest; shows how the sensor
        // "sees" colour independent of brightness (dark areas masked out)
        View::Chroma => {
            let m = p[0].max(p[1]).max(p[2]);
            if m < 0.004 {
                o.fill(0);
            } else {
                for c in 0..3 {
                    o[c] = enc.get(p[c] / m);
                }
            }
        }
        // Top layer (B) as blue, bottom (R) as red; that is where the spectral centroids lie.
        _ => {
            o[0] = enc.get(p[0] * gain);
            o[1] = enc.get(p[1] * gain);
            o[2] = enc.get(p[2] * gain);
        }
    });
    Rgb16 { width: lin.width, height: lin.height, data }
}

/// Histogram per layer over 0 … saturation (linear), `bins` classes.
pub fn layer_histograms(lin: &Linear, bins: usize) -> [Vec<u32>; 3] {
    let mut h: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0; bins]);
    for p in lin.data.chunks_exact(3) {
        for c in 0..3 {
            let i = ((p[c].clamp(0.0, 1.0)) * (bins - 1) as f32) as usize;
            h[c][i] += 1;
        }
    }
    h
}

/// The system's sRGB ICC profile (macOS or Windows), if present.
pub fn srgb_icc() -> Option<Vec<u8>> {
    if cfg!(windows) {
        let win = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        std::fs::read(win.join("System32\\spool\\drivers\\color\\sRGB Color Space Profile.icm")).ok()
    } else {
        std::fs::read("/System/Library/ColorSync/Profiles/sRGB Profile.icc").ok()
    }
}

/// Camera (without white balance) → XYZ (D65) for the DNG colour description.
pub fn camera_to_xyz(cal: &Calibration, mode: Option<&str>) -> Mat3 {
    Mat3::SRGB_TO_XYZ.mul(&cal.mode(mode).cam_to_srgb).mul(&Mat3::diag(cal.wb_gain))
}

/// White balance eyedropper: channel gain (relative to the selected preset) that makes the
/// camera values `cam` (linear, without white balance, 0…1) neutral grey after the colour matrix.
/// `None` if the spot is too dark or blown out.
pub fn pick_white_balance(cal: &Calibration, s: &Settings, cam: [f64; 3]) -> Option<[f32; 3]> {
    if cam.iter().any(|&v| !(0.002..0.95).contains(&v)) {
        return None;
    }
    let preset = cal.wb(s.white_balance.as_deref());
    let m = cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref()).inverse()?;
    let want = m.apply([1.0, 1.0, 1.0]);
    let w: [f64; 3] = std::array::from_fn(|c| preset.gain[c] * cam[c]);
    let r: [f64; 3] = std::array::from_fn(|c| want[c] / w[c]);
    if r.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    // Green stays fixed so that brightness hardly changes
    Some(r.map(|v| (v / r[1]) as f32))
}

#[cfg(test)]
mod gamut_tests {
    use super::{highlight_gamut, tone_white};

    const W: f32 = 50_000.0;

    #[test]
    fn dark_and_fitting_pixels_unchanged() {
        assert_eq!(highlight_gamut([0.7 * W, 0.2 * W, -0.1 * W], W), [0.7 * W, 0.2 * W, -0.1 * W]);
        assert_eq!(highlight_gamut([0.9 * W, 0.85 * W, 0.8 * W], W), [0.9 * W, 0.85 * W, 0.8 * W]);
        // neutral beyond white stays neutral (clipped later as before)
        let n = highlight_gamut([1.3 * W; 3], W);
        assert!(n.iter().all(|v| (v - W).abs() < 1.0), "{n:?}");
    }

    #[test]
    fn clipped_layer_glides_to_bottom_layer() {
        let v = [5000.0, 7000.0, 3000.0];
        // below the start: unchanged (bit for bit)
        assert_eq!(super::glide_clipped([0.4, 0.85, 0.6], v), v);
        // at the clip level: all layers at the bottom layer (neutral)
        assert_eq!(super::glide_clipped([0.4, 0.8, 1.0], v), [5000.0; 3]);
        // in between: soft and monotone
        let a = super::glide_clipped([0.4, 0.8, 0.9], v);
        let b = super::glide_clipped([0.4, 0.8, 0.95], v);
        assert!(a[1] < v[1] && b[1] < a[1] && b[1] > 5000.0);
        assert!(a[2] > v[2] && b[2] > a[2] && b[2] < 5000.0);
        assert_eq!(a[0], 5000.0);
    }

    #[test]
    fn bright_saturated_colour_fits_keeps_hue_and_stays_graded() {
        // bright cyan beyond the output range (red below 0, green and blue above white)
        let a = highlight_gamut([-0.3 * W, 1.1 * W, 1.6 * W], W);
        let b = highlight_gamut([-0.3 * W, 1.0 * W, 1.45 * W], W);
        for p in [a, b] {
            assert!(p.iter().all(|v| (-1.0..=W + 1.0).contains(v)), "{p:?}");
            // still cyan: red lowest, blue highest
            assert!(p[0] < p[1] && p[1] < p[2], "{p:?}");
        }
        // different inputs stay different (no flat full tone)
        assert!((a[0] - b[0]).abs() > 100.0 || (a[1] - b[1]).abs() > 100.0, "{a:?} {b:?}");
        // far brighter than white: nearly white
        let c = highlight_gamut([1.0 * W, 3.0 * W, 4.0 * W], W);
        assert!(c.iter().all(|v| *v > 0.9 * W), "{c:?}");
    }

    #[test]
    fn brighter_input_never_gives_a_darker_or_equal_flat_output() {
        // a cyan and a light blue along increasing exposure: the output keeps getting lighter (no plateau, no steps)
        for base in [[-0.15, 0.55, 0.8], [0.1, 0.6, 0.75], [0.9, 0.8, -0.05]] {
            let mut last = -1.0f32;
            for i in 0..40 {
                let s = 1.0 + i as f32 * 0.05;
                let o = highlight_gamut(base.map(|v| v * s * W), W).map(|v| v.clamp(0.0, W));
                let sum = o[0] + o[1] + o[2];
                assert!(sum >= last - 1.0, "{base:?} s {s}: {o:?}");
                // until it is nearly white
                if s >= 1.4 && s <= 2.4 && sum < 2.95 * W {
                    assert!(sum > last + 50.0, "plateau at {base:?} s {s}: {o:?}");
                }
                last = sum;
            }
        }
    }

    #[test]
    fn white_of_the_tone_curve() {
        let tone: Vec<u16> = (0..1000u32).map(|i| (i * 100).min(60_000) as u16).collect();
        assert_eq!(tone_white(&tone, 999.0), 600.0);
    }
}

#[cfg(test)]
mod linear_tests {
    use super::*;

    /// The straight tone table: proportional below white, clipped above, contrast 0 leaves it straight, contrast
    /// keeps middle grey.
    #[test]
    fn linear_table_is_straight_and_keeps_middle_grey() {
        let g = 1.0 / 10000.0;
        let t = linear_tone_lut(g, 0.0);
        assert_eq!(t[0], 0);
        assert_eq!(t[5000], 32768);
        assert_eq!(t[10000], 65535);
        assert_eq!(t[40000], 65535);
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
        let c = linear_tone_lut(g, 0.5);
        let grey = (0.184 / g) as usize;
        assert!((c[grey] as i32 - t[grey] as i32).abs() <= 8);
        assert!(c[grey / 4] < t[grey / 4], "more contrast: shadows darker");
    }

    #[test]
    fn linear_mode_takes_the_neutral_matrix() {
        assert_eq!(settings::matrix_mode(Some(settings::LINEAR_MODE)), Some(profile::NEUTRAL_MODE));
        assert_eq!(settings::matrix_mode(Some("Vivid")), Some("Vivid"));
        let s = Settings { color_mode: Some(settings::LINEAR_MODE.into()), ..Settings::default() };
        assert!(s.is_linear());
        assert!(!Settings { monochrome: true, ..s.clone() }.is_linear());
        assert_eq!(s.spp_key_class(false).mode.as_deref(), Some(profile::NEUTRAL_MODE), "same base as the neutral mode");
    }
}

#[cfg(test)]
mod dither_tests {
    use super::Rgb16;

    /// A very flat gradient: without dither long plateaus of one code, with dither short runs and the same mean.
    #[test]
    fn dither_breaks_plateaus_and_keeps_the_mean() {
        let w = 4096;
        let data: Vec<u16> = (0..w).flat_map(|x| [(40000 + x * 256 / w) as u16; 3]).collect();
        let img = Rgb16 { width: w, height: 1, data };
        let runs = |v: &[u8]| {
            let g: Vec<u8> = v.iter().step_by(3).copied().collect();
            let mut n = 1usize;
            let mut cnt = 1usize;
            for k in 1..g.len() {
                if g[k] != g[k - 1] {
                    cnt += 1;
                }
                n += 1;
            }
            n as f64 / cnt as f64
        };
        let plain = img.to_rgb8();
        let dith = img.to_rgb8_dithered();
        assert!(runs(&plain) > 500.0, "{}", runs(&plain));
        assert!(runs(&dith) < 5.0, "{}", runs(&dith));
        let mean = |v: &[u8]| v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64;
        assert!((mean(&plain) - mean(&dith)).abs() < 0.05);
    }
}
