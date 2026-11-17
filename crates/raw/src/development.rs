//! Standard development for non-Sigma RAWs.
//!
//! Chain (all linear up to the tone curve):
//! raw value − black → /(white − black) → as-shot white balance (on the mosaic) →
//! highlight reconstruction "inpaint opposed" → demosaic (RCD or Markesteijn 3-pass) → crop →
//! capture sharpening (light) → camera → XYZ D50 (DNG specification, two illuminants) →
//! working space linear Rec. 2020 → exposure (camera base exposure + BaselineExposure +
//! Fuji DR) → sigmoid per channel with hue preservation → sRGB (gamut: desaturate towards luminance) → 8 bit.

use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::color::{self, M3};
use crate::lens::LensOptions;
use crate::noise::{self, NoiseReduction, NoiseModel};
use crate::sharpness::{self, Sharpness};
use crate::tone::{Sigmoid, ToneCurve};
use crate::{Error, RawFile, Result, RgbImage, Sensor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demosaic {
    /// RCD for Bayer, Markesteijn 3-pass for X-Trans
    Auto,
    /// mean of the same color in 3×3 (simple path)
    Bilinear,
    Rcd,
    Markesteijn1,
    Markesteijn3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingSpace {
    Rec2020,
    Srgb,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub demosaic: Demosaic,
    pub highlights: bool,
    pub sharpness: Option<Sharpness>,
    pub tone: Sigmoid,
    pub working_space: WorkingSpace,
    /// additional exposure in EV
    pub ev: f32,
    /// apply the camera base exposure (table)
    pub camera_ev: bool,
    /// hue/chroma table ("look") after the tone curve, None = off
    pub look: Option<Look>,
    /// use the camera look from [`camera_look`] if `look` is not set
    pub camera_look: bool,
    /// noise reduction (sliders; default color 25, luminance 0)
    pub noise: NoiseReduction,
    /// lens correction (embedded or lensfun), None = off
    pub lens: Option<LensOptions>,
}

/// Hue-dependent correction in Oklab after the tone curve: per 20° sector (centered at 10°, 30° …)
/// hue shift in degrees and chroma factor; linear in between, faded out for small chroma.
pub type Look = [[f32; 2]; 18];

/// Camera look (hue/chroma per sector) of the standard development, in Oklab after our tone curve and the
/// saturation factor of [`camera_saturation`]. FinePix X100, X100V, Ricoh GR and GR III (16.11.2026, `graineo-dev
/// camera-color --fit-look`): fitted on camera JPEGs/embedded previews of own shots in the camera's standard style
/// (Fujifilm Provia with color 0 and DR 100, Ricoh "Standard") of whole shooting days, checked on photos of other days
/// that were left out of the fit; ΔE00 median of the check photos X100 2.69 → 2.21, X100V 2.05 → 1.95, GR 2.47 → 2.33,
/// GR III 2.47 → 2.25 (against the previous development), the 90th percentile lower each time. GR IV: no hue
/// error worth a table; EOS RP: inconsistent. Own measurement, no Adobe profile data.
/// Test hook (measurements, `graineo-dev camera-color`): `GRAINEO_CAMERA_LOOK=off` switches the table off for every
/// camera, `GRAINEO_CAMERA_LOOK=<file>` uses the table in the file for every camera (18 lines "hue shift chroma factor").
pub fn camera_look(maker: &str, model: &str) -> Option<Look> {
    if let Some(v) = std::env::var_os("GRAINEO_CAMERA_LOOK") {
        return if v == "off" { None } else { std::fs::read_to_string(&v).ok().and_then(|t| parse_look(&t)) };
    }
    let m = model.to_ascii_uppercase();
    let h = maker.to_ascii_uppercase();
    if h.contains("FUJI") && m.contains("X100") && !m.contains("X100F") && !m.contains("X100V") && !m.contains("X100S") && !m.contains("X100T") {
        return Some([
            [2.4, 1.001], [1.0, 1.057], [-1.2, 1.103], [-3.4, 1.126], [-4.4, 1.126], [-4.0, 1.101],
            [-3.5, 1.065], [-2.9, 1.034], [-1.5, 1.013], [-0.7, 1.003], [-0.6, 1.010], [-0.2, 1.017],
            [0.5, 0.971], [0.5, 0.904], [-0.6, 0.890], [-0.9, 0.923], [0.2, 0.957], [1.9, 0.971],
        ]);
    }
    if h.contains("FUJI") && m.contains("X100V") && !m.contains("X100VI") {
        return Some([
            [1.6, 0.984], [0.8, 0.985], [0.1, 0.989], [0.6, 0.984], [3.0, 0.993], [6.9, 1.025],
            [7.7, 1.056], [4.3, 1.057], [1.1, 1.033], [0.1, 1.014], [0.5, 1.009], [2.2, 1.006],
            [3.7, 0.988], [3.2, 0.973], [1.6, 0.985], [0.7, 0.999], [0.9, 1.001], [1.4, 0.994],
        ]);
    }
    if h.contains("RICOH") && m.contains("GR III") && !m.contains("GR IIIX") {
        return Some([
            [0.3, 0.994], [-0.1, 0.990], [-0.1, 0.977], [0.5, 0.958], [1.3, 0.960], [1.9, 0.985],
            [1.5, 1.010], [-0.1, 1.024], [-1.5, 1.026], [-1.4, 1.021], [-0.8, 1.012], [-0.5, 1.008],
            [0.1, 1.015], [0.6, 1.008], [0.5, 0.997], [0.2, 0.996], [0.2, 0.997], [0.4, 0.997],
        ]);
    }
    if h.contains("RICOH") && m.trim().strip_prefix("RICOH ").unwrap_or(m.trim()) == "GR" {
        return Some([
            [1.3, 1.027], [0.6, 1.043], [-0.9, 1.050], [-1.5, 1.042], [-0.6, 1.034], [1.0, 1.021],
            [1.9, 1.005], [1.2, 1.002], [0.1, 1.006], [-0.1, 1.003], [1.8, 0.989], [5.6, 0.981],
            [8.0, 0.990], [6.5, 0.999], [3.3, 1.004], [1.4, 1.011], [0.9, 1.015], [0.9, 1.020],
        ]);
    }
    None
}

/// Table of 18 lines "hue shift chroma factor" (format of `GRAINEO_CAMERA_LOOK`, see [`camera_look`]).
pub fn parse_look(text: &str) -> Option<Look> {
    let rows: Vec<[f32; 2]> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let v: Vec<f32> = l.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            (v.len() == 2).then(|| [v[0], v[1]])
        })
        .collect::<Option<Vec<_>>>()?;
    rows.try_into().ok()
}

/// Even saturation factor after the tone curve (around the luminance, all hues alike) for the
/// standard development; 1.0 = unchanged. The per-channel tone curve raises the chroma of every
/// camera by roughly +10 C*. Most cameras' color matrices are slightly undersaturated on their own,
/// so that evens out; the matrices these cameras write into their files (and the X100's) are close
/// to colorimetric, so the curve leaves them clearly too saturated. Measured against ColorChecker
/// reference values (D50, pipette on the neutral patch) and against the cameras' own JPEGs in the
/// standard picture style; for GR III and X100 in addition to [`camera_look`].
/// Test hook: `GRAINEO_CAMERA_SATURATION=<factor>` replaces the value for every camera.
pub fn camera_saturation(maker: &str, model: &str) -> f32 {
    if let Some(v) = std::env::var("GRAINEO_CAMERA_SATURATION").ok().and_then(|v| v.parse::<f32>().ok()) {
        return v;
    }
    let h = maker.to_ascii_uppercase();
    if !h.contains("RICOH") && !h.contains("FUJI") {
        return 1.0;
    }
    model_saturation(model).unwrap_or(1.0)
}

/// Model names (upper case, without the maker prefix) and their factor for [`camera_saturation`].
const CAMERA_SATURATION: [(&str, f32); 6] = [("GR", 0.88), ("GR II", 0.85), ("GR III", 0.93), ("GR IIIX", 0.88), ("GR IV", 0.90), ("X100", 0.85)];

/// Factor of [`camera_saturation`] by the model name alone (as the library stores it, e.g. "GR II",
/// "RICOH GR IIIx", "FinePix X100"); `None` for every other camera.
pub fn model_saturation(model: &str) -> Option<f32> {
    let m = model.trim().to_ascii_uppercase();
    let m = m.strip_prefix("RICOH ").or_else(|| m.strip_prefix("FINEPIX ")).unwrap_or(&m).trim().to_string();
    CAMERA_SATURATION.iter().find(|(n, _)| *n == m).map(|(_, f)| *f)
}

/// Neutral table (starting point for `graineo-dev raw --look-file`). A look fitted to the reference previews
/// brought no gain in the cross-validation and is therefore off.
pub const STANDARD_LOOK: Look = [
    [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0],
    [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0],
    [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0],
];

pub fn srgb_lin_to_oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_46 * c[0] + 0.536_332_55 * c[1] + 0.051_445_995 * c[2];
    let m = 0.211_903_5 * c[0] + 0.680_699_5 * c[1] + 0.107_396_96 * c[2];
    let s = 0.088_302_46 * c[0] + 0.281_718_85 * c[1] + 0.629_978_7 * c[2];
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

pub fn oklab_to_srgb_lin(c: [f32; 3]) -> [f32; 3] {
    let l = c[0] + 0.396_337_78 * c[1] + 0.215_803_76 * c[2];
    let m = c[0] - 0.105_561_346 * c[1] - 0.063_854_17 * c[2];
    let s = c[0] - 0.089_484_18 * c[1] - 1.291_485_5 * c[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// Look as a 3D table (33³ nodes in the square-root space of linear sRGB, trilinear) –
/// saves atan2/sin/cos/cbrt per pixel (GR III: 400 → 140 ms at 24 MP).
pub struct LookLut {
    t: Vec<[f32; 3]>,
}

const LN: usize = 33;

impl LookLut {
    pub fn new(look: &Look) -> Self {
        let mut t = Vec::with_capacity(LN * LN * LN);
        for r in 0..LN {
            for g in 0..LN {
                for b in 0..LN {
                    let v = |k: usize| {
                        let x = k as f32 / (LN - 1) as f32;
                        x * x
                    };
                    t.push(apply_look(look, [v(r), v(g), v(b)]));
                }
            }
        }
        Self { t }
    }

    /// Table (33³, in square-root space, r slow) and edge length, for the GPU.
    pub fn table(&self) -> (usize, &[[f32; 3]]) {
        (LN, &self.t)
    }

    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        if c[0] < 0.0 || c[1] < 0.0 || c[2] < 0.0 || c[0] > 1.0 || c[1] > 1.0 || c[2] > 1.0 {
            return c;
        }
        let m = (LN - 1) as f32;
        let p = [c[0].sqrt() * m, c[1].sqrt() * m, c[2].sqrt() * m];
        let i = p.map(|v| (v as usize).min(LN - 2));
        let f = [p[0] - i[0] as f32, p[1] - i[1] as f32, p[2] - i[2] as f32];
        let at = |a: usize, b: usize, d: usize| self.t[((i[0] + a) * LN + i[1] + b) * LN + i[2] + d];
        let mut o = [0f32; 3];
        for (a, wa) in [(0, 1.0 - f[0]), (1, f[0])] {
            for (b, wb) in [(0, 1.0 - f[1]), (1, f[1])] {
                for (d, wd) in [(0, 1.0 - f[2]), (1, f[2])] {
                    let w = wa * wb * wd;
                    let v = at(a, b, d);
                    o[0] += w * v[0];
                    o[1] += w * v[1];
                    o[2] += w * v[2];
                }
            }
        }
        o
    }
}

/// Apply the look to a linear sRGB pixel.
#[inline]
pub fn apply_look(look: &Look, c: [f32; 3]) -> [f32; 3] {
    let lab = srgb_lin_to_oklab([c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]);
    let ch = lab[1].hypot(lab[2]);
    if ch < 1e-4 {
        return c;
    }
    let hue = lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0);
    let t = (hue - 10.0).rem_euclid(360.0) / 20.0;
    let i = t as usize % 18;
    let f = t - t.floor();
    let j = (i + 1) % 18;
    let dh = look[i][0] * (1.0 - f) + look[j][0] * f;
    let cs = look[i][1] * (1.0 - f) + look[j][1] * f;
    // fade out for almost neutral colors (chroma 0 … 0.04)
    let w = (ch / 0.04).min(1.0);
    let w = w * w * (3.0 - 2.0 * w);
    let h2 = (hue + w * dh).to_radians();
    let c2 = ch * (1.0 + w * (cs - 1.0));
    oklab_to_srgb_lin([lab[0], c2 * h2.cos(), c2 * h2.sin()])
}

impl Default for Options {
    fn default() -> Self {
        Self {
            demosaic: Demosaic::Auto,
            highlights: true,
            sharpness: Some(Sharpness::default()),
            tone: default_tone(),
            working_space: WorkingSpace::Rec2020,
            ev: 0.0,
            camera_ev: true,
            look: None,
            camera_look: true,
            noise: NoiseReduction::default(),
            lens: Some(LensOptions::default()),
        }
    }
}

/// Tone curve of the standard development (tuned on reference previews).
pub fn default_tone() -> Sigmoid {
    Sigmoid { contrast: 1.8, skew: 0.0, white: 1.0, black: 0.000152, hue: 0.2, saturation: 1.0 }
}

/// Base exposure in EV, relative to "white point = 1": the linear signal is raised by this much before the
/// tone curve. General value plus correction per camera (calibrated on the test bench).
pub const BASE_EV: f32 = 1.0;

/// Camera correction of the base exposure (EV): deviation of the midtones against reference exports, measured per model as the median over 45 to 190 unedited
/// photos of process version 2012 and newer (own measurement on own exports, `graineo-dev lr-comparison`). Models without
/// an entry get 0.
pub fn camera_ev(maker: &str, model: &str) -> f32 {
    // test hook for comparisons: GRAINEO_CAMERA_EV=<EV> replaces the correction
    if let Some(v) = std::env::var("GRAINEO_CAMERA_EV").ok().and_then(|v| v.parse::<f32>().ok()) {
        return v;
    }
    let m = model.to_ascii_uppercase();
    let m = m.trim();
    let h = maker.to_ascii_uppercase();
    match () {
        _ if h.contains("CANON") && m.contains("EOS RP") => 0.20,
        _ if h.contains("CANON") && m == "EOS 5D" => 0.14,
        // Ricoh: check GR IIIx before GR III (the name contains it)
        _ if h.contains("RICOH") && m.contains("GR IIIX") => -0.07,
        _ if h.contains("RICOH") && m.contains("GR III") => -0.03,
        _ if h.contains("RICOH") && m.contains("GR IV") => -0.06,
        _ if h.contains("RICOH") && m.contains("GR II") => 0.20,
        _ if h.contains("RICOH") && m == "GR" => -0.09,
        _ if h.contains("SONY") && m.contains("RX100M3") => -0.41,
        _ if h.contains("FUJI") && m.contains("X100") && !m.contains("X100F") && !m.contains("X100V") && !m.contains("X100S") && !m.contains("X100T") => -0.04,
        _ if h.contains("FUJI") && m.contains("X-PRO1") => 0.10,
        _ if h.contains("FUJI") && m.contains("X-E2") => 0.22,
        _ => 0.0,
    }
}

/// Exposure offset from the Fuji dynamic range setting: DR200 exposes the raw data
/// 1 EV shorter, DR400 2 EV (the camera JPEG brightens accordingly).
pub fn fuji_dr_ev(dr: Option<u16>) -> f32 {
    if std::env::var_os("GRAINEO_FUJI_DR_OFF").is_some() {
        return 0.0;
    }
    match dr {
        Some(d) if d > 100 => (d as f32 / 100.0).log2(),
        _ => 0.0,
    }
}

/// Compensation for Fuji shots (EV): the dynamic range modes DR200/DR400 and the extended ISO steps expose the raw data
/// shorter; the reference exports brighten them by different amounts depending on the model. The values are chosen so that the midtones of the development
/// match those of the reference exports for unedited photos (own measurement on 191 photos, midtones, process version 2012 and
/// newer; the value acts before the tone curve, so the output does not change 1:1):
/// - FinePix X100: from ISO 2500 +1.2 EV. Below that the dynamic range modes by their underexposure (+1 EV at DR200,
///   +2 EV at DR400): the reference exports (Lightroom) do not brighten them, but the camera's own rendering and the
///   system RAW engine of macOS do; against the embedded camera previews of 58 own DR200/DR400 shots (ISO 400–2000,
///   training and holdout on separate days) the full compensation lowered the median ΔE00 from 12.3 to 3.4 on the
///   holdout (own measurement, 17.11.2026). Photos with Lightroom start values keep the exports'
///   brightness ([`fuji_lightroom_offset`]).
/// - X-Pro1: from ISO 2500 +0.55 EV (28 photos); DR200/400 1.3 times the underexposure (only 8 photos).
/// - X-E2: DR200 +1.2 EV (36 photos); DR400 double (not measured); without DR mode from ISO 2500 +0.35 EV.
/// - X-Pro2: DR200 +0.5 EV (7 photos, few).
/// - other Fuji models (not measured): DR mode by its underexposure (+1 at DR200, +2 at DR400), otherwise from ISO 2500 +0.7 EV.
/// Fujifilm only; without information no compensation.
pub fn fuji_compensation(maker: &str, model: &str, iso: Option<u32>, dr: Option<u16>) -> f32 {
    if !maker.to_ascii_uppercase().contains("FUJI") {
        return 0.0;
    }
    let m = model.to_ascii_uppercase();
    let high = iso.is_some_and(|i| i >= 2500);
    let dr_ev = fuji_dr_ev(dr);
    if is_first_x100(&m) {
        return if high { 1.2 } else { dr_ev };
    }
    if m.contains("X-PRO1") {
        return if dr_ev > 0.0 { 1.3 * dr_ev } else if high { 0.55 } else { 0.0 };
    }
    if m.contains("X-E2") {
        return if dr_ev > 0.0 { 1.2 * dr_ev } else if high { 0.35 } else { 0.0 };
    }
    if m.contains("X-PRO2") {
        return 0.5 * dr_ev;
    }
    if dr_ev > 0.0 {
        dr_ev
    } else if high {
        0.7
    } else {
        0.0
    }
}

/// The original FinePix X100 (not X100S/T/F/V).
fn is_first_x100(model_upper: &str) -> bool {
    let m = model_upper;
    m.contains("X100") && !m.contains("X100F") && !m.contains("X100V") && !m.contains("X100S") && !m.contains("X100T") && !m.contains("X100VI")
}

/// Part of [`fuji_compensation`] that Lightroom does not apply (EV): the dynamic range modes of the FinePix X100 below
/// ISO 2500. Photos developed with Lightroom start values subtract it, so their brightness stays as in Lightroom.
pub fn fuji_lightroom_offset(maker: &str, model: &str, iso: Option<u32>, dr: Option<u16>) -> f32 {
    if !maker.to_ascii_uppercase().contains("FUJI") || !is_first_x100(&model.to_ascii_uppercase()) || iso.is_some_and(|i| i >= 2500) {
        return 0.0;
    }
    fuji_dr_ev(dr)
}

/// Hook for a demosaic outside the CPU version: (mosaic, width, height, pattern, method) → RGB.
pub type DemosaicHook<'a> = &'a (dyn Fn(&[f32], usize, usize, [[u8; 6]; 6], Demosaic) -> Option<Vec<[f32; 3]>> + Sync);

/// Size of the base image ([`RawFile::basis`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Full,
    /// longest edge at most this many pixels
    Preview(usize),
}

/// Base image: linear, white-balanced camera RGB (cropped), see [`RawFile::basis`].
/// Mosaic after normalization, white balance and highlights, not yet demosaiced (for the GPU path).
pub struct MosaicPart {
    pub norm: Vec<f32>,
    pub w: usize,
    pub h: usize,
    pub pat: [[u8; 6]; 6],
    pub crop: crate::Region,
    pub sensor: Sensor,
}

pub struct Basis {
    /// only with [`RawFile::base_mosaic`]: the mosaic instead of `px` (empty)
    pub mosaic: Option<MosaicPart>,
    pub px: Vec<[f32; 3]>,
    pub width: usize,
    pub height: usize,
    /// size at full resolution (cropped) – larger than `width`×`height` for the preview
    pub full: (usize, usize),
    pub profile: Option<noise::Profile>,
    pub clipped_frac: usize,
    pub demosaic: Demosaic,
    pub timings: Vec<(&'static str, Duration)>,
}

/// Average the mosaic per color in blocks of `k`×`k` (region `r` of the mosaic with width `w`). If a
/// color is missing in the block (X-Trans at some positions), the block is extended by one pixel.
pub fn bin_pixels(m: &[f32], w: usize, r: crate::Region, k: usize, color_at: &(impl Fn(usize, usize) -> usize + Sync)) -> (Vec<[f32; 3]>, usize, usize) {
    let (bw, bh) = (r.w / k, r.h / k);
    let hm = m.len() / w.max(1);
    let mut out = vec![[0f32; 3]; bw * bh];
    out.par_chunks_mut(bw.max(1)).enumerate().for_each(|(by, row)| {
        for (bx, o) in row.iter_mut().enumerate() {
            let (x0, y0) = (r.x + bx * k, r.y + by * k);
            let mut s = [0f32; 3];
            let mut n = [0u32; 3];
            for y in y0..y0 + k {
                for x in x0..x0 + k {
                    let c = color_at(x, y);
                    s[c] += m[y * w + x];
                    n[c] += 1;
                }
            }
            if n.contains(&0) {
                let (xa, ya) = (x0.saturating_sub(1), y0.saturating_sub(1));
                let (xe, ye) = ((x0 + k + 1).min(w), (y0 + k + 1).min(hm));
                for c in 0..3 {
                    if n[c] != 0 {
                        continue;
                    }
                    for y in ya..ye {
                        for x in xa..xe {
                            if color_at(x, y) == c {
                                s[c] += m[y * w + x];
                                n[c] += 1;
                            }
                        }
                    }
                }
            }
            *o = [s[0] / n[0].max(1) as f32, s[1] / n[1].max(1) as f32, s[2] / n[2].max(1) as f32];
        }
    });
    (out, bw, bh)
}

/// Area-preserving downscaling (mean over the covered source pixels, also with fractional factors).
pub fn downscale(px: &[[f32; 3]], w: usize, h: usize, nw: usize, nh: usize) -> Vec<[f32; 3]> {
    if (nw, nh) == (w, h) {
        return px.to_vec();
    }
    // weights per output position: (first source index, weights)
    let weights = |n_source: usize, n_target: usize| -> Vec<(usize, Vec<f32>)> {
        let f = n_source as f64 / n_target as f64;
        (0..n_target)
            .map(|i| {
                let (a, b) = (i as f64 * f, (i + 1) as f64 * f);
                let (ia, ib) = (a.floor() as usize, (b.ceil() as usize).min(n_source));
                let ws: Vec<f32> = (ia..ib).map(|j| ((b.min(j as f64 + 1.0) - a.max(j as f64)) / f) as f32).collect();
                (ia, ws)
            })
            .collect()
    };
    let gx = weights(w, nw);
    let gy = weights(h, nh);
    // horizontal
    let mut tmp = vec![[0f32; 3]; nw * h];
    tmp.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
        let src = &px[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let (a, ws) = &gx[x];
            let mut s = [0f32; 3];
            for (k, &wt) in ws.iter().enumerate() {
                let p = src[a + k];
                s[0] += wt * p[0];
                s[1] += wt * p[1];
                s[2] += wt * p[2];
            }
            *o = s;
        }
    });
    let mut out = vec![[0f32; 3]; nw * nh];
    out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
        let (a, ws) = &gy[y];
        for (k, &wt) in ws.iter().enumerate() {
            let src = &tmp[(a + k) * nw..(a + k + 1) * nw];
            for (o, p) in row.iter_mut().zip(src) {
                o[0] += wt * p[0];
                o[1] += wt * p[1];
                o[2] += wt * p[2];
            }
        }
    });
    out
}

pub struct Outcome {
    pub img: RgbImage,
    /// (step, duration)
    pub timings: Vec<(&'static str, Duration)>,
    /// actually applied exposure (EV) and its components
    pub ev: f32,
    pub ev_parts: Vec<(&'static str, f32)>,
    pub demosaic: Demosaic,
    pub clipped_frac: usize,
    pub color: Option<color::Color>,
    /// estimated noise model (only with active noise reduction)
    pub noise_model: Option<NoiseModel>,
    /// applied lens correction (sources, zoom)
    pub lens: Option<String>,
    /// short label of the profile source for the UI ("Adobe LCP", "Sicherung", "Kamera", "lensfun")
    pub lens_source: Option<&'static str>,
}

impl RawFile {
    /// Normalized mosaic values (black/white per pattern position) and 6×6 color pattern
    /// (`pattern[y % 6][x % 6]` = color 0/1/2 relative to the origin of the full sensor image).
    pub fn mosaic_normalized(&self) -> Result<(Vec<f32>, [[u8; 6]; 6])> {
        let i = &self.info;
        let (w, h) = (i.width, i.height);
        let rawler::rawimage::RawPhotometricInterpretation::Cfa(cfg) = &self.image.photometric else {
            return Err(Error::Unsupported("Mosaik ohne CFA-Angabe".into()));
        };
        let mut pat = [[0u8; 6]; 6];
        for (y, row) in pat.iter_mut().enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                *v = cfg.cfa.color_at(y, x) as u8;
            }
        }
        let data = self.image.data.as_f32();
        let (bw, bh, bc) = i.black_pattern;
        let (bw, bh, bc) = (bw.max(1), bh.max(1), bc.max(1));
        let white = i.white.first().copied().unwrap_or(65535.0);
        // written straight into uninitialised memory: every value is set below, filling it with zeros first cost a
        // noticeable share of each offline copy
        let mut norm: Vec<f32> = Vec::with_capacity(w * h);
        norm.spare_capacity_mut()[..w * h].par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let b = if i.black.is_empty() {
                    0.0
                } else {
                    let idx = ((y % bh) * bw + x % bw) * bc;
                    i.black.get(idx).copied().unwrap_or(i.black[0])
                };
                o.write((data[y * w + x] - b) / (white - b));
            }
        });
        // SAFETY: the loop above wrote all w × h values (h rows of w)
        unsafe { norm.set_len(w * h) };
        Ok((norm, pat))
    }

    /// Calibration (illuminants) and AnalogBalance → color according to the DNG specification.
    pub fn color_dng(&self, wb: [f32; 3]) -> Option<color::Color> {
        let neutral = [1.0 / wb[0], 1.0 / wb[1], 1.0 / wb[2]];
        let ab = self.info.analog_balance.map(color::diag).unwrap_or(color::diag([1.0; 3]));
        color::determine(neutral, &self.info.calibration, &ab)
    }

    /// Standard development up to the finished 8-bit sRGB image (sensor orientation).
    pub fn develop(&self, o: &Options) -> Result<Outcome> {
        let b = self.basis(o.demosaic, o.highlights, o.noise.active(), Size::Full)?;
        let Basis { px, width, height, profile, clipped_frac, demosaic, timings, .. } = b;
        self.ab_crop(px, width, height, o, timings, Instant::now(), clipped_frac, demosaic, profile)
    }

    /// Base image for further development: normalized, as-shot white balance, highlights
    /// reconstructed, demosaiced, cropped (linear, white-balanced
    /// camera RGB). `noise_model`: also deliver the noise profile for [`noise::apply`].
    /// [`Size::Preview`]: instead of demosaicing, binned per color (Bayer 2×2, X-Trans 3×3) and
    /// downscaled area-preserving to the edge length; the noise profile then matches exactly this image.
    pub fn basis(&self, demosaic: Demosaic, highlights: bool, noise_model: bool, size: Size) -> Result<Basis> {
        self.base_with(demosaic, highlights, noise_model, size, None)
    }

    /// Like [`RawFile::basis`], with a hook for the demosaic at full resolution (e.g. on the GPU): it
    /// receives the normalized, white-balanced mosaic with highlights, the size, the 6×6 pattern and the chosen
    /// method and returns RGB per pixel – or `None`, then the CPU computes.
    pub fn base_with(&self, demosaic: Demosaic, highlights: bool, noise_model: bool, size: Size, hook: Option<DemosaicHook>) -> Result<Basis> {
        self.base_inner(demosaic, highlights, noise_model, size, hook, false)
    }

    /// Base image up to just before the demosaic at full resolution: instead of `px` it delivers the mosaic (`Basis::mosaic`),
    /// so that the GPU can demosaic it and continue processing right away. Only for Bayer and X-Trans;
    /// otherwise (or if the mosaic does not reach the demosaic) `mosaic` is `None` and `px` is filled.
    pub fn base_mosaic(&self, noise_model: bool) -> Result<Basis> {
        self.base_inner(Demosaic::Auto, true, noise_model, Size::Full, None, true)
    }

    fn base_inner(&self, demosaic: Demosaic, highlights: bool, noise_model: bool, size: Size, hook: Option<DemosaicHook>, mosaic_only: bool) -> Result<Basis> {
        let i = &self.info;
        if i.fuji_rotated {
            return Err(Error::Unsupported("Fuji SuperCCD (gedrehter Sensor)".into()));
        }
        let mut timings = Vec::new();
        let (w, h) = (i.width, i.height);
        let wb = i.wb.unwrap_or([1.0; 3]);
        let mut t = Instant::now();
        let lap = |name: &'static str, t: &mut Instant, z: &mut Vec<(&'static str, Duration)>| {
            z.push((name, t.elapsed()));
            *t = Instant::now();
        };
        let max_edge = match size {
            Size::Full => None,
            Size::Preview(k) => Some(k.max(64)),
        };
        let r = i.crop.or(i.active).unwrap_or(crate::Region { x: 0, y: 0, w, h });
        let r = crate::Region { x: r.x.min(w), y: r.y.min(h), w: r.w.min(w - r.x.min(w)), h: r.h.min(h - r.y.min(h)) };
        match i.sensor {
            Sensor::Bayer | Sensor::XTrans => {
                let (mut norm, pat) = self.mosaic_normalized()?;
                let per = if i.sensor == Sensor::Bayer { 2 } else { 6 };
                norm.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    let pr = &pat[y % 6];
                    for (x, v) in row.iter_mut().enumerate() {
                        *v *= wb[pr[x % 6] as usize];
                    }
                });
                lap("normieren+WB", &mut t, &mut timings);
                let color_at = move |x: usize, y: usize| pat[y % 6][x % 6] as usize;
                let mut used_method = demosaic;
                if used_method == Demosaic::Auto {
                    used_method = if i.sensor == Sensor::Bayer { Demosaic::Rcd } else { Demosaic::Markesteijn3 };
                }
                let bin = if i.sensor == Sensor::Bayer { 2 } else { 3 };
                // preview: target size (area-preserving) after binning
                let target = max_edge.map(|k| {
                    let (bw, bh) = (r.w / bin, r.h / bin);
                    let f = (k as f32 / bw.max(bh) as f32).min(1.0);
                    (bw, bh, ((bw as f32 * f).round() as usize).max(1), ((bh as f32 * f).round() as usize).max(1))
                });
                let mut profile = None;
                if noise_model {
                    let clip = [0.98 * wb[0], 0.98 * wb[1], 0.98 * wb[2]];
                    if let Some(m) = noise::estimate(&norm, w, h, &color_at, clip, wb) {
                        profile = Some(match target {
                            None => {
                                let dm = |m: &[f32], ww: usize, hh: usize| -> Vec<[f32; 3]> { demosaic_mosaic(m, ww, hh, pat, used_method, i.sensor) };
                                noise::simulate_profile(m, &color_at, &dm)
                            }
                            Some((bw, _, nw, _)) => {
                                let f = nw as f32 / bw as f32;
                                let nq = (264.0 * bin as f32 / f).ceil() as usize;
                                let v = |m: &[f32], ww: usize, hh: usize| -> (Vec<[f32; 3]>, usize, usize) {
                                    let (px, bw2, bh2) = bin_pixels(m, ww, crate::Region { x: 0, y: 0, w: ww, h: hh }, bin, &color_at);
                                    let (nw2, nh2) = (((bw2 as f32 * f).round() as usize).max(1), ((bh2 as f32 * f).round() as usize).max(1));
                                    (downscale(&px, bw2, bh2, nw2, nh2), nw2, nh2)
                                };
                                noise::simulate_profile_with(m, &color_at, nq, &v)
                            }
                        });
                    }
                    lap("Rauschmodell", &mut t, &mut timings);
                }
                let mut clipped_frac = 0;
                if highlights {
                    clipped_frac = crate::highlights::opposed(&mut norm, w, h, wb, &color_at);
                    lap("Lichter", &mut t, &mut timings);
                }
                if let Some((bw, bh, nw, nh)) = target {
                    let (px, _, _) = bin_pixels(&norm, w, r, bin, &color_at);
                    drop(norm);
                    let px = downscale(&px, bw, bh, nw, nh);
                    lap("Vorschau", &mut t, &mut timings);
                    return Ok(Basis { mosaic: None, px, width: nw, height: nh, full: (r.w, r.h), profile, clipped_frac, demosaic: Demosaic::Bilinear, timings });
                }
                if mosaic_only && matches!(used_method, Demosaic::Rcd | Demosaic::Markesteijn3) {
                    return Ok(Basis { mosaic: Some(MosaicPart { norm, w, h, pat, crop: r, sensor: i.sensor }), px: Vec::new(), width: r.w, height: r.h, full: (r.w, r.h), profile, clipped_frac, demosaic: used_method, timings });
                }
                let from_hook = hook.and_then(|hk| hk(&norm, w, h, pat, used_method));
                let full = match (used_method, i.sensor) {
                    _ if from_hook.is_some() => from_hook.unwrap(),
                    (Demosaic::Rcd, Sensor::Bayer) => {
                        let p2 = [[pat[0][0] as usize, pat[0][1] as usize], [pat[1][0] as usize, pat[1][1] as usize]];
                        let mut out = crate::rcd::rcd(&norm, w, h, p2);
                        crate::demosaic_mean_in(&norm, w, h, per, &color_at, &mut out, Some(crate::rcd::RCD_BORDER))?;
                        out
                    }
                    (Demosaic::Markesteijn1 | Demosaic::Markesteijn3, Sensor::XTrans) => {
                        crate::markesteijn::markesteijn(&norm, w, h, pat, if used_method == Demosaic::Markesteijn1 { 1 } else { 3 })
                    }
                    _ => {
                        used_method = Demosaic::Bilinear;
                        let mut out = vec![[0f32; 3]; w * h];
                        crate::demosaic_mean_in(&norm, w, h, per, &color_at, &mut out, None)?;
                        out
                    }
                };
                lap("Demosaik", &mut t, &mut timings);
                drop(norm);
                let mut px = Vec::with_capacity(r.w * r.h);
                for y in r.y..r.y + r.h {
                    px.extend_from_slice(&full[y * w + r.x..y * w + r.x + r.w]);
                }
                Ok(Basis { mosaic: None, px, width: r.w, height: r.h, full: (r.w, r.h), profile, clipped_frac, demosaic: used_method, timings })
            }
            Sensor::Linear | Sensor::Mono => {
                let lin = self.linear()?;
                // linear() already crops
                let mut px = lin.px;
                px.par_iter_mut().for_each(|p| {
                    for c in 0..3 {
                        p[c] *= wb[c];
                    }
                });
                let (lw, lh) = (lin.width, lin.height);
                // no mosaic to measure the sensor noise on (Canon sRAW/mRAW, linear DNG): the data are already
                // interpolated, so the noise is estimated from the image itself, as for a finished image
                let model = if noise_model { noise::image_noise_model(&px, lw, lh) } else { None };
                if let Some(k) = max_edge {
                    let f = (k as f32 / lw.max(lh) as f32).min(1.0);
                    let (nw, nh) = (((lw as f32 * f).round() as usize).max(1), ((lh as f32 * f).round() as usize).max(1));
                    let px = downscale(&px, lw, lh, nw, nh);
                    let profile = model.map(|m| noise::image_profile(m, f));
                    return Ok(Basis { mosaic: None, px, width: nw, height: nh, full: (lw, lh), profile, clipped_frac: 0, demosaic: Demosaic::Bilinear, timings });
                }
                let profile = model.map(|m| noise::image_profile(m, 1.0));
                Ok(Basis { mosaic: None, px, width: lw, height: lh, full: (lw, lh), profile, clipped_frac: 0, demosaic: Demosaic::Bilinear, timings })
            }
            Sensor::OtherMosaic => Err(Error::Unsupported(format!("Farbfilter {}", i.cfa))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ab_crop(
        &self,
        mut px: Vec<[f32; 3]>,
        w: usize,
        h: usize,
        o: &Options,
        mut timings: Vec<(&'static str, Duration)>,
        mut t: Instant,
        clipped_frac: usize,
        demosaic: Demosaic,
        profile: Option<noise::Profile>,
    ) -> Result<Outcome> {
        let i = &self.info;
        let wb = i.wb.unwrap_or([1.0; 3]);
        timings.push(("Zuschnitt", t.elapsed()));
        t = Instant::now();
        // noise reduction before sharpening (otherwise noise would be sharpened too)
        if let Some(p) = &profile {
            noise::apply(&mut px, w, h, p, &o.noise);
            timings.push(("Rauschen", t.elapsed()));
            t = Instant::now();
        }
        if let Some(s) = &o.sharpness {
            sharpness::apply(&mut px, w, h, s, 0.95 * wb[1].min(wb[0]).min(wb[2]));
            timings.push(("Schärfe", t.elapsed()));
            t = Instant::now();
        }
        // lens correction after sharpening (deconvolution expects the sensor PSF), before color/tone
        let mut lens = None;
        let mut lens_source = None;
        if let Some(oo) = &o.lens {
            if let Some(k) = self.lens_correction(w, h, oo) {
                k.apply(&mut px, w, h);
                lens_source = k.geometry.as_ref().or(k.vignette.as_ref()).map(|t| t.kind.short());
                lens = Some(format!(
                    "Geometrie: {}, Vignette: {}, Zoom {:.4}",
                    k.geometry.as_ref().map(|g| g.source.as_str()).unwrap_or("–"),
                    k.vignette.as_ref().map(|g| g.source.as_str()).unwrap_or("–"),
                    k.zoom
                ));
                timings.push(("Objektiv", t.elapsed()));
                t = Instant::now();
            }
        }
        // color: camera → XYZ D50 → working space
        let fb = self.color_dng(wb);
        let cam_xyz: M3 = match &fb {
            Some(f) => f.camera_to_xyz_d50,
            None => return Err(Error::Unsupported("keine Kameramatrix".into())),
        };
        let ws_xyz = match o.working_space {
            WorkingSpace::Rec2020 => color::rec2020_to_xyz_d65(),
            WorkingSpace::Srgb => color::srgb_to_xyz_d65(),
        };
        let cam_ws = color::mul(&color::xyz_d50_to(&ws_xyz), &cam_xyz);
        let ws_srgb = color::mul(&color::inv(&color::srgb_to_xyz_d65()).expect("sRGB"), &ws_xyz);
        let lum = ws_xyz[1];
        // exposure
        let mut ev_parts = vec![("Grund", BASE_EV)];
        if o.camera_ev {
            let k = camera_ev(&i.maker, &i.model);
            if k != 0.0 {
                ev_parts.push(("Kamera", k));
            }
        }
        if i.baseline_exposure != 0.0 {
            ev_parts.push(("BaselineExposure", i.baseline_exposure));
        }
        let dr = fuji_compensation(&i.maker, &i.model, i.exif.iso, i.fuji_dr);
        if dr != 0.0 {
            ev_parts.push(("Fuji-DR", dr));
        }
        if o.ev != 0.0 {
            ev_parts.push(("Regler", o.ev));
        }
        let ev: f32 = ev_parts.iter().map(|(_, v)| v).sum();
        let gain = ev.exp2();
        let m = cam_ws.map(|r| r.map(|v| v * gain));
        let mut tone = o.tone;
        if o.camera_look && o.look.is_none() {
            tone.saturation *= camera_saturation(&i.maker, &i.model);
        }
        let curve = ToneCurve::new(tone);
        let look = o.look.or_else(|| if o.camera_look { camera_look(&i.maker, &i.model) } else { None }).map(|l| LookLut::new(&l));
        // sRGB output via LUT (12 bit linear → 8 bit)
        let lut: Vec<u8> = (0..=4096).map(|k| (crate::srgb_oetf(k as f32 / 4096.0) * 255.0 + 0.5) as u8).collect();
        let mut out = vec![0u8; w * h * 3];
        out.par_chunks_mut(w * 3).zip(px.par_chunks(w)).for_each(|(orow, prow)| {
            for (o3, p) in orow.chunks_exact_mut(3).zip(prow) {
                let v = color::mulv(&m, *p);
                let t = curve.pixel(v, lum);
                let mut s = color::mulv(&ws_srgb, t);
                if let Some(l) = &look {
                    s = l.apply(s);
                }
                // gamut: desaturate negative components towards the luminance
                let y = 0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2];
                let mn = s[0].min(s[1]).min(s[2]);
                if mn < 0.0 && y > 0.0 {
                    let f = y / (y - mn);
                    s = [y + f * (s[0] - y), y + f * (s[1] - y), y + f * (s[2] - y)];
                }
                for c in 0..3 {
                    o3[c] = lut[(s[c].clamp(0.0, 1.0) * 4096.0) as usize];
                }
            }
        });
        timings.push(("Farbe+Ton", t.elapsed()));
        let img = RgbImage::from_raw(w as u32, h as u32, out).expect("buffer size");
        Ok(Outcome { img, timings, ev, ev_parts, demosaic, clipped_frac, color: fb, noise_model: profile.map(|p| p.model), lens, lens_source })
    }
}

/// Demosaic of a mosaic (for the noise simulation; same choice as in [`RawFile::develop`]).
/// Public for analyses (e.g. demosaic comparison in `sensor_view`): `pat` is the 6×6 pattern
/// relative to the origin of `m`; RCD only for Bayer, Markesteijn only for X-Trans, otherwise bilinear.
pub fn demosaic_mosaic(m: &[f32], w: usize, h: usize, pat: [[u8; 6]; 6], d: Demosaic, sensor: Sensor) -> Vec<[f32; 3]> {
    let color_at = move |x: usize, y: usize| pat[y % 6][x % 6] as usize;
    let per = if sensor == Sensor::Bayer { 2 } else { 6 };
    match (d, sensor) {
        (Demosaic::Rcd, Sensor::Bayer) => {
            let p2 = [[pat[0][0] as usize, pat[0][1] as usize], [pat[1][0] as usize, pat[1][1] as usize]];
            let mut out = crate::rcd::rcd(m, w, h, p2);
            let _ = crate::demosaic_mean_in(m, w, h, per, &color_at, &mut out, Some(crate::rcd::RCD_BORDER));
            out
        }
        (Demosaic::Markesteijn1 | Demosaic::Markesteijn3, Sensor::XTrans) => {
            crate::markesteijn::markesteijn(m, w, h, pat, if d == Demosaic::Markesteijn1 { 1 } else { 3 })
        }
        _ => {
            let mut out = vec![[0f32; 3]; w * h];
            let _ = crate::demosaic_mean_in(m, w, h, per, &color_at, &mut out, None);
            out
        }
    }
}

#[cfg(test)]
mod camera_saturation_tests {
    use super::{camera_saturation, model_saturation};

    /// Maker and model as the files report them; GR IIIx must not take the GR III value and vice versa.
    #[test]
    fn by_maker_and_model() {
        let r = "RICOH IMAGING COMPANY, LTD.";
        assert_eq!(camera_saturation(r, "GR"), 0.88);
        assert_eq!(camera_saturation(r, "GR II"), 0.85);
        assert_eq!(camera_saturation(r, "RICOH GR III"), 0.93);
        assert_eq!(camera_saturation(r, "RICOH GR IIIx"), 0.88);
        assert_eq!(camera_saturation(r, "RICOH GR IV                     "), 0.90);
        assert_eq!(camera_saturation("FUJIFILM", "FinePix X100"), 0.85);
        assert_eq!(camera_saturation("FUJIFILM", "X100"), 0.85);
        for (h, m) in [(r, "GR DIGITAL 4"), ("FUJIFILM", "X100F"), ("FUJIFILM", "X100V"), ("FUJIFILM", "X100S"), ("FUJIFILM", "X-T30"), ("Canon", "GR"), ("SONY", "ILCE-7M3")] {
            assert_eq!(camera_saturation(h, m), 1.0, "{h} {m}");
        }
        assert_eq!(model_saturation("RICOH GR IIIx"), Some(0.88));
        assert_eq!(model_saturation("X100T"), None);
    }
}

#[cfg(test)]
mod iso_compensation_tests {
    use super::{fuji_compensation, fuji_lightroom_offset, camera_ev};

    #[test]
    fn only_fuji_gets_compensation() {
        for maker in ["Canon", "RICOH", "SONY", "SIGMA", "", "Unknown"] {
            for iso in [None, Some(100), Some(2500), Some(12800)] {
                for dr in [None, Some(100), Some(400)] {
                    assert_eq!(fuji_compensation(maker, "X100", iso, dr), 0.0, "{maker} {iso:?} {dr:?}");
                }
            }
        }
    }

    #[test]
    fn fuji_models_by_measurement() {
        // X100: dynamic range modes by their underexposure (camera rendering), high ISO as measured against the exports
        assert_eq!(fuji_compensation("FUJIFILM", "FinePix X100", Some(400), Some(400)), 2.0);
        assert_eq!(fuji_compensation("FUJIFILM", "FinePix X100", Some(400), Some(200)), 1.0);
        assert_eq!(fuji_compensation("FUJIFILM", "FinePix X100", Some(200), Some(100)), 0.0);
        assert_eq!(fuji_compensation("FUJIFILM", "FinePix X100", Some(6400), Some(100)), 1.2);
        assert_eq!(fuji_compensation("FUJIFILM", "FinePix X100", Some(3200), Some(400)), 1.2);
        // Lightroom start values keep the exports' brightness: only the X100's dynamic range part is taken back
        assert_eq!(fuji_lightroom_offset("FUJIFILM", "FinePix X100", Some(400), Some(200)), 1.0);
        assert_eq!(fuji_lightroom_offset("FUJIFILM", "FinePix X100", Some(3200), Some(400)), 0.0);
        assert_eq!(fuji_lightroom_offset("FUJIFILM", "X100V", Some(400), Some(200)), 0.0);
        assert_eq!(fuji_lightroom_offset("FUJIFILM", "X-E2", Some(400), Some(200)), 0.0);
        assert_eq!(fuji_lightroom_offset("Canon", "X100", Some(400), Some(200)), 0.0);
        // X-Pro1 and X-E2
        assert_eq!(fuji_compensation("FUJIFILM", "X-Pro1", Some(6400), Some(100)), 0.55);
        assert_eq!(fuji_compensation("FUJIFILM", "X-E2", Some(400), Some(200)), 1.2);
        assert_eq!(fuji_compensation("FUJIFILM", "X-E2", Some(400), Some(400)), 2.4);
        assert_eq!(fuji_compensation("FUJIFILM", "X-Pro2", Some(400), Some(200)), 0.5);
        assert_eq!(fuji_compensation("FUJIFILM", "X-E2", Some(200), Some(100)), 0.0);
        // other model: by the underexposure of the mode, otherwise from ISO 2500
        assert_eq!(fuji_compensation("FUJIFILM", "X-T2", Some(400), Some(200)), 1.0);
        assert_eq!(fuji_compensation("FUJIFILM", "X-T2", Some(400), Some(400)), 2.0);
        assert_eq!(fuji_compensation("FUJIFILM", "X-T2", Some(6400), None), 0.7);
        assert_eq!(fuji_compensation("FUJIFILM", "X-T2", Some(800), None), 0.0);
    }

    #[test]
    fn camera_correction_separates_ricoh_models() {
        assert_eq!(camera_ev("RICOH IMAGING COMPANY, LTD.", "RICOH GR III"), -0.03);
        assert_eq!(camera_ev("RICOH IMAGING COMPANY, LTD.", "RICOH GR IIIx"), -0.07);
        assert_eq!(camera_ev("RICOH IMAGING COMPANY, LTD.", "GR II"), 0.20);
        assert_eq!(camera_ev("RICOH IMAGING COMPANY, LTD.", "GR"), -0.09);
        assert_eq!(camera_ev("Canon", "Canon EOS RP"), 0.20);
        assert_eq!(camera_ev("Canon", "Canon EOS 5D Mark II"), 0.0);
        assert_eq!(camera_ev("SIGMA", "SIGMA DP2 Merrill"), 0.0);
    }
}

#[cfg(test)]
mod parse_look_tests {
    use super::parse_look;

    #[test]
    fn reads_eighteen_rows_and_rejects_others() {
        let ok: String = (0..18).map(|i| format!("{} 1.0{}\n", i as f32 * 0.5, i)).collect();
        let l = parse_look(&format!("# comment\n{ok}")).expect("18 rows");
        assert_eq!(l[2], [1.0, 1.02]);
        assert!(parse_look(&ok.lines().take(17).collect::<Vec<_>>().join("\n")).is_none());
        assert!(parse_look("1 2 3\n").is_none());
    }
}
