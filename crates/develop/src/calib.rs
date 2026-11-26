//! Calibration from CAMF and PROP: black level, spatial gain, white balance,
//! colour matrices, colour mode and tone curve parameters.

use x3f::{Camf, RawImage, X3fFile};

use crate::matrix::Mat3;

fn vec3(camf: &Camf, name: &str) -> Option<[f64; 3]> {
    let v = camf.matrix(name).ok()?.as_f64();
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

fn mat3(camf: &Camf, name: &str) -> Option<Mat3> {
    let v = camf.matrix(name).ok()?.as_f64();
    (v.len() == 9).then(|| Mat3(std::array::from_fn(|i| v[i])))
}

fn rect(camf: &Camf, name: &str) -> Option<[usize; 4]> {
    let v = camf.matrix(name).ok()?.as_f64();
    (v.len() == 4).then(|| [v[0] as usize, v[1] as usize, v[2] as usize, v[3] as usize])
}

fn scalar(camf: &Camf, name: &str) -> Option<f64> {
    camf.matrix(name).ok()?.as_f64().first().copied()
}

/// DP Merrill (CAMERAID 78) at MED or LOW resolution with FPNSuppression other than 1: the black level comes from a
/// fixed rectangle of the top dark shield instead of the CAMF one, as the maker's software does (inclusive).
fn merrill_fixed_dark_shield(camf: &Camf, resolution: &str) -> Option<[usize; 4]> {
    let camera = scalar(camf, "CAMERAID")?;
    let fpn = scalar(camf, "FPNSuppression").unwrap_or(0.0);
    fixed_dark_shield_for(camera as u32, fpn as u32, resolution)
}

fn fixed_dark_shield_for(camera: u32, fpn: u32, resolution: &str) -> Option<[usize; 4]> {
    if camera != 78 || fpn == 1 {
        return None;
    }
    if resolution.contains("MED") {
        Some([64, 1608, 4766, 1626])
    } else if resolution.contains("LOW") {
        Some([64, 1608, 2374, 1626])
    } else {
        None
    }
}

/// Trimmed mean per plane (1 % discarded at top and bottom) over an inclusive rectangle.
fn black_level(raw: &RawImage, r: [usize; 4]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut v: Vec<u16> = Vec::new();
        for y in r[1]..=r[3].min(raw.rows - 1) {
            for x in r[0]..=r[2].min(raw.columns - 1) {
                v.push(raw.pixel(y, x)[c]);
            }
        }
        if v.is_empty() {
            continue;
        }
        v.sort_unstable();
        let cut = v.len() / 100;
        let s = &v[cut..v.len() - cut];
        *o = s.iter().map(|&x| x as f64).sum::<f64>() / s.len() as f64;
    }
    out
}

/// Position-dependent gain of one sensor plane (table covering the whole sensor area).
#[derive(Clone)]
pub struct GainTable {
    rows: usize,
    cols: usize,
    v: Vec<f32>,
}

impl GainTable {
    fn read(camf: &Camf, props: &str, ch: char) -> Option<Self> {
        let min: f32 = camf.property(props, &format!("MinGains{ch}"))?.parse().ok()?;
        let delta: f32 = camf.property(props, &format!("Delta{ch}"))?.parse().ok()?;
        if min == 0.0 || delta == 0.0 {
            return None;
        }
        let m = camf.matrix(camf.property(props, &format!("GainsTable{ch}"))?).ok()?;
        let shape = m.shape();
        if shape.len() != 2 {
            return None;
        }
        let v = m.as_f64().iter().map(|&t| min + delta * t as f32).collect();
        Some(Self { rows: shape[0], cols: shape[1], v })
    }

    fn blend(a: &Self, wa: f32, b: &Self) -> Self {
        let v = a.v.iter().zip(&b.v).map(|(x, y)| wa * x + (1.0 - wa) * y).collect();
        Self { rows: a.rows, cols: a.cols, v }
    }

    /// `wa·a + wb·b` with independent weights (CRA blend, also extrapolation).
    fn mix(a: &Self, wa: f32, b: &Self, wb: f32) -> Self {
        let v = a.v.iter().zip(&b.v).map(|(x, y)| wa * x + wb * y).collect();
        Self { rows: a.rows, cols: a.cols, v }
    }

    /// Interpolate one image row vertically in advance (sensor row `y` of `h`).
    pub fn row(&self, y: usize, h: usize) -> Vec<f32> {
        let fy = y as f32 / (h - 1) as f32 * (self.rows - 1) as f32;
        let r0 = fy as usize;
        let r1 = (r0 + 1).min(self.rows - 1);
        let t = fy - r0 as f32;
        (0..self.cols)
            .map(|c| {
                let a = self.v[r0 * self.cols + c];
                a + t * (self.v[r1 * self.cols + c] - a)
            })
            .collect()
    }

    /// Horizontal interpolation within a row produced by `row`.
    #[inline]
    pub fn at_row(&self, row: &[f32], x: usize, w: usize) -> f32 {
        let fx = x as f32 / (w - 1) as f32 * (self.cols - 1) as f32;
        let c0 = fx as usize;
        let c1 = (c0 + 1).min(self.cols - 1);
        row[c0] + (fx - c0 as f32) * (row[c1] - row[c0])
    }
}

/// Spatial gain tables for aperture `f` and focus encoder `focus` (0 = infinity … 31 = closest focus),
/// interpolated linearly in 1/f and linearly over focus.
fn spatial_gains(camf: &Camf, f: f64, focus: f64) -> Option<[GainTable; 3]> {
    // SD1 (interchangeable lenses): tables per chief ray angle of the lens instead of per focus.
    if let Some(cra) = scalar(camf, "ChiefRayAngle").filter(|&c| c > 0.0) {
        return spatial_gains_cra(camf, f, cra);
    }
    let fstops = camf.matrix("SpatialGain_Fstop").ok()?.as_f64();
    let n = fstops.len();
    let load = |i: usize, dist: &str| -> Option<[GainTable; 3]> {
        let props = format!("SpatialGainsProps_{i}_{dist}");
        Some([
            GainTable::read(camf, &props, 'R')?,
            GainTable::read(camf, &props, 'G')?,
            GainTable::read(camf, &props, 'B')?,
        ])
    };
    let for_dist = |dist: &str| -> Option<[GainTable; 3]> {
        if n == 0 {
            return None;
        }
        if f <= fstops[0] || n == 1 {
            return load(0, dist);
        }
        let i = fstops.iter().position(|&s| s >= f).unwrap_or(n - 1).max(1);
        let a = ((1.0 / f - 1.0 / fstops[i]) / (1.0 / fstops[i - 1] - 1.0 / fstops[i])) as f32;
        let (lo, hi) = (load(i - 1, dist)?, load(i, dist)?);
        Some(std::array::from_fn(|c| GainTable::blend(&lo[c], a, &hi[c])))
    };
    let inf = for_dist("INF")?;
    let w = ((31.0 - focus) / 31.0) as f32;
    if w >= 1.0 {
        return Some(inf);
    }
    let near = for_dist("MOD")?;
    Some(std::array::from_fn(|c| GainTable::blend(&inf[c], w, &near[c])))
}

/// Weights between the CRA table families "50" and "30" or "85" for aperture `f` and
/// chief ray angle `cra` (degrees). Reimplementation of x3fuse-core `merrill_cra_weights`
/// (constants as f64 bit patterns, as there).
fn cra_weights(f: f64, cra: f64) -> (&'static str, f32, f32) {
    let q = 0.5 / f;
    let phi = |a: f64, b: f64, low: f32| (((q + a).min(b) + f64::from(((a - q) as f32).max(low))) * 0.5).atan();
    let p50 = phi(f64::from_bits(0x3fc3_c50a_f08f_bd56), f64::from_bits(0x3fdd_7343_4719_bb36), f32::from_bits(0xbd28_cf0c));
    let t = (std::f64::consts::PI / p50) * f64::from(cra as f32 / 180.0);
    if f >= 2.0 && t >= 1.0 {
        let r = phi(f64::from_bits(0x3fcf_a730_ea29_9de5), f64::from_bits(0x3fdd_23b6_93fe_1ec4), f32::from_bits(0xbd34_fbe9)) / p50;
        ("85", ((r - t) / (r - 1.0)) as f32, ((t - 1.0) / (r - 1.0)) as f32)
    } else {
        let r = phi(f64::from_bits(0x3f8a_cf4e_4e15_9df0), f64::from_bits(0x3fd4_17db_677f_f305), f32::from_bits(0xbccc_8007)) / p50;
        ("30", ((t - r) / (1.0 - r)) as f32, ((1.0 - t) / (1.0 - r)) as f32)
    }
}

/// Spatial gain of the SD1 (Merrill): tables `SpatialGainsProps_<aperture>_<30|50|85>` for
/// apertures 1.4/2.0/2.8/8.0 (interpolated quadratically in f), blended by chief ray angle.
fn spatial_gains_cra(camf: &Camf, f: f64, cra: f64) -> Option<[GainTable; 3]> {
    const STOPS: [(f64, &str); 4] = [(1.4, "1.4"), (2.0, "2.0"), (2.8, "2.8"), (8.0, "8.0")];
    let load = |i: usize, fam: &str| -> Option<[GainTable; 3]> {
        let props = format!("SpatialGainsProps_{}_{fam}", STOPS[i].1);
        Some([GainTable::read(camf, &props, 'R')?, GainTable::read(camf, &props, 'G')?, GainTable::read(camf, &props, 'B')?])
    };
    let for_family = |fam: &str| -> Option<[GainTable; 3]> {
        if f <= STOPS[0].0 {
            return load(0, fam);
        }
        if f >= STOPS[3].0 {
            return load(3, fam);
        }
        let hi = STOPS.iter().position(|s| s.0 >= f).unwrap_or(3).max(1);
        let (a2, b2) = (STOPS[hi].0 * STOPS[hi].0, STOPS[hi - 1].0 * STOPS[hi - 1].0);
        let a = ((a2 - f * f) / (a2 - b2)) as f32;
        let (lo, up) = (load(hi - 1, fam)?, load(hi, fam)?);
        Some(std::array::from_fn(|c| GainTable::blend(&lo[c], a, &up[c])))
    };
    let (family, a, b) = cra_weights(f, cra);
    let (mid, other) = (for_family("50")?, for_family(family)?);
    Some(std::array::from_fn(|c| GainTable::mix(&mid[c], a, &other[c], b)))
}

/// Inverse of Sigma's Adobe RGB source matrix (from x3fuse-core):
/// shots taken with colour space Adobe RGB get their tone curve in this space and only then
/// are converted to sRGB.
const ADOBE_TO_SRGB: Mat3 = Mat3([
    1.3982606199511893, -0.39828280643279224, -5.6444684264386543e-5, //
    5.7053088178776595e-5, 1.0000033459087134, 3.4094961680870946e-5, //
    8.7362994382552355e-5, -0.04294260086663016, 1.0427079091680009,
]);

/// EXIF tag ColorSpace (0xA001) from a JPEG: 1 = sRGB, 0xFFFF = uncalibrated (Adobe RGB).
fn jpeg_color_space(jpg: &[u8]) -> Option<u16> {
    let mut i = 2;
    while i + 4 <= jpg.len() && jpg[i] == 0xFF {
        let marker = jpg[i + 1];
        let len = u16::from_be_bytes([jpg[i + 2], jpg[i + 3]]) as usize;
        let seg = jpg.get(i + 4..i + 2 + len)?;
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            let t = &seg[6..];
            let le = t.get(..2)? == b"II";
            let u16_at = |o: usize| t.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
            let u32_at = |o: usize| t.get(o..o + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
            let find = |ifd: usize, tag: u16| -> Option<usize> {
                let n = u16_at(ifd)? as usize;
                (0..n).map(|k| ifd + 2 + k * 12).find(|&e| u16_at(e) == Some(tag))
            };
            let exif = u32_at(find(u32_at(4)? as usize, 0x8769)? + 8)? as usize;
            return u16_at(find(exif, 0xA001)? + 8);
        }
        if marker == 0xDA {
            break;
        }
        i += 2 + len;
    }
    None
}

/// Fixed FCBlue matrix used when the CAMF contains none (older firmware);
/// DP and SD variant as in x3fuse-core `fcblue_matrix`.
fn fcblue_matrix(dp: bool) -> Mat3 {
    let bits: [u64; 9] = if dp {
        [
            0x3ff8d42c3c9eecc0, 0x3fd46edaa92e6213, 0xbfebdfce3150dae4, 0x3fd69d2bf5515055, 0x3ff6160956c0d6f5,
            0xbfe77aa8a82a5615, 0x3fc08ad688377186, 0x3fb73b13d244dbd2, 0x3fe8f5e84f09528f,
        ]
    } else {
        [
            0x3ff9b97785729b28, 0x3fd2a44417870542, 0xbfecc5197a24894c, 0x3fd8915bdd76683c, 0x3ff5d78811b1d92b,
            0xbfe7f7b9e060fe48, 0x3fc2fbe33acd5b68, 0x3fb4ca0c282c6ef4, 0x3fe8a7c7c5260f5e,
        ]
    };
    Mat3(bits.map(f64::from_bits))
}

/// Camera-model-dependent constants of the Merrill generation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    /// DP1/DP2/DP3 Merrill: counts = (raw − black) · 2 · WB
    Dp,
    /// SD1 / SD1 Merrill: counts = (raw − black) · WB / 0.55
    Sd1,
    /// dp0/dp1/dp2/dp3 Quattro (preprocessing via x3fuse-core, see [`crate::quattro`])
    DpQuattro,
    /// sd Quattro / sd Quattro H (no SPP colour calibration, substitute matrix)
    SdQuattro,
    /// Older generation ("Classic": SD9/SD10/SD14/SD15, DP1/DP2 incl. s/x); preprocessing via
    /// x3fuse-core as for Quattro, calibration via [`Calibration::read_classic`]
    Classic,
}

impl Model {
    pub fn from_name(name: &str) -> Self {
        if name.starts_with("SIGMA SD1") { Model::Sd1 } else { Model::Dp }
    }

    /// Factor from normalised WB gains to sensor counts
    /// (x3fuse-core `merrill_count_scale`).
    pub fn count_scale(self) -> f64 {
        match self {
            Model::Dp => 2.0,
            Model::Sd1 => 1.0 / 0.55,
            Model::DpQuattro | Model::SdQuattro | Model::Classic => 1.0,
        }
    }

    pub fn is_quattro(self) -> bool {
        matches!(self, Model::DpQuattro | Model::SdQuattro)
    }

    /// Preprocessing via x3fuse-core (synthetic raw image, counts up to 65535, own
    /// tone curve shape): Quattro and Classic.
    pub fn is_prepared(self) -> bool {
        matches!(self, Model::DpQuattro | Model::SdQuattro | Model::Classic)
    }
}

/// Tone curve shape of the Quattro colour modes (Quattro uses a fixed table instead of `TCColorModeSettings`;
/// values from x3fuse-core `quattro_tone`).
fn quattro_tone_row(mode: &str) -> ToneRow {
    let [f_start, f_end, steep, brk, steep2] = match mode {
        "Standard" => [-1.19, 2.85, 3.0, 0.078, 1.55],
        "Vivid" => [-1.20, 2.85, 3.55, 0.070, 1.60],
        "Neutral" => [-1.19, 2.85, 2.80, 0.080, 1.56],
        "Portrait" => [-1.05, 2.65, 3.10, 0.030, 1.97],
        "Cinema" => [-1.20, 2.85, 3.50, 0.070, 1.90],
        _ => [-1.17, 2.85, 3.0, 0.1, 1.4],
    };
    ToneRow { scale: 1.0, f_start, f_end, steep, brk, steep2, bracket: None }
}

/// Brightness reference of the sd Quattro (H) relative to the dp Quattro (empirical).
const QUATTRO_SD_EXPOSURE: f64 = 1.0;

/// Additional colour modes of the Quattro cameras (after the six Merrill modes).
pub const QUATTRO_EXTRA_MODES: [&str; 5] = ["SunsetRed", "ForestGreen", "FCYellow", "Cinema", "TealAndOrange"];

/// Teal-and-orange matrix of the Quattro (fixed, from x3fuse-core `resolve_color_mode`).
const QUATTRO_TEAL_ORANGE: Mat3 = Mat3([1.06673, 0.05757, -0.12415, 0.25086, 0.38293, 0.36634, -0.01669, -0.04191, 1.05873]);

/// Digital ISO factor `CaptureISO / SensorISO` (as for Merrill and in Kalpanika).
///
/// For Quattro, x3fuse-core divides by 100 instead of `SensorISO`; that is only the same at
/// ISO 100. At higher ISO the sensor works with analogue `SensorISO` (200/400),
/// and dividing by 100 made the images log2(SensorISO/100) stops too bright
/// (comparison with the camera JPEGs: ISO 400–6400 up to +2 EV).
pub(crate) fn quattro_iso_gain(camf: &Camf) -> f64 {
    let capture = scalar(camf, "CaptureISO").unwrap_or(100.0);
    let sensor = scalar(camf, "SensorISO").filter(|v| *v > 0.0).unwrap_or(100.0);
    let g = capture / sensor;
    if g.is_finite() && g > 0.0 { g } else { 1.0 }
}

/// Brightness reference of the Classic cameras: counts = `CLASSIC_EXPOSURE` · 16383 · (camera matrix
/// from x3fuse-core) · x. Matched against the embedded camera JPEGs.
pub const CLASSIC_EXPOSURE: f64 = 1.05;

/// Additional brightness reference for cameras without gain tables (SD9/SD10/SD14,
/// white balance via illuminant + correction): −1 EV, only roughly matched on three samples.
pub const CLASSIC_EXPOSURE_PRE_TRUE: f64 = 0.5;

/// Layer balance of the Classic cameras (bottom, middle, top layer): with Kalpanika's
/// matrices alone the images are slightly magenta compared to the camera JPEGs (grey areas
/// Δa* ≈ +2.4); 2 % less in the bottom layer brings them to ±2 (DP1s).
pub const CLASSIC_LAYER_BALANCE: [f64; 3] = [0.98, 1.0, 1.0];

/// Tone curve shape of the Classic cameras (matched against the camera JPEGs).
fn classic_tone_row() -> ToneRow {
    if let Some(v) = std::env::var("GRAINEO_CLASSIC_TONE").ok().map(|t| t.split(',').filter_map(|x| x.trim().parse::<f64>().ok()).collect::<Vec<_>>()).filter(|v| v.len() == 5) {
        return ToneRow { scale: 1.0, f_start: v[0], f_end: v[1], steep: v[2], brk: v[3], steep2: v[4], bracket: None };
    }
    ToneRow { f_start: -1.05, ..quattro_tone_row("Standard") }
}

/// White balance preset of a Classic camera as in Kalpanika (`x3f_get_gain`,
/// `x3f_get_bmt_to_xyz`): gain per layer and matrix camera (white-balanced) → sRGB.
fn classic_wb(camf: &Camf, name: &str) -> Option<([f64; 3], Mat3)> {
    let lookup = |lists: &[&str], name: &str| lists.iter().find_map(|l| camf.property(l, name).map(str::to_string));
    let find = |lists: &[&str]| lookup(lists, name).or_else(|| (name == "Daylight").then(|| lookup(lists, "Sunlight")).flatten());
    let gains = find(&["WhiteBalanceGains", "DP1_WhiteBalanceGains"]).and_then(|n| vec3(camf, &n));
    let cc = find(&["WhiteBalanceColorCorrections", "DP1_WhiteBalanceColorCorrections"]).and_then(|n| mat3(camf, &n));
    let (mut gain, ccm) = match (gains, cc) {
        (Some(g), Some(cc)) => (g, cc),
        _ => {
            // SD9/SD10: camera → XYZ via illuminant and correction, white point D65
            let illum = find(&["WhiteBalanceIlluminants"]).and_then(|n| mat3(camf, &n))?;
            let corr = find(&["WhiteBalanceCorrections"]).and_then(|n| mat3(camf, &n))?;
            let raw_to_xyz = corr.mul(&illum);
            let n = raw_to_xyz.inverse()?.apply([0.95047, 1.0, 1.08883]);
            if n.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
                return None;
            }
            let bmt_to_xyz = raw_to_xyz.mul(&Mat3::diag(n));
            (n.map(|v| 1.0 / v), Mat3::SRGB_TO_XYZ.inverse()?.mul(&bmt_to_xyz))
        }
    };
    for fact in ["SensorAdjustmentGainFact", "TempGainFact", "FNumberGainFact"] {
        if let Some(f) = vec3(camf, fact) {
            for c in 0..3 {
                gain[c] *= f[c];
            }
        }
    }
    Some((gain, ccm))
}

/// Parameters of one row of `TCColorModeSettings`.
#[derive(Clone, Copy, Debug)]
pub struct ToneRow {
    pub scale: f64,
    pub f_start: f64,
    pub f_end: f64,
    pub steep: f64,
    pub brk: f64,
    pub steep2: f64,
    /// merged bracket (X3I): its own tone curve instead of this row ([`crate::sfd::BracketTone`])
    pub bracket: Option<crate::sfd::BracketTone>,
}

impl ToneRow {
    pub fn start_bits(&self) -> u64 {
        self.f_start.to_bits()
    }
}

impl Default for ToneRow {
    /// Fallback row when the file contains no tone curve values.
    fn default() -> Self {
        Self { scale: 1.0, f_start: -1.17, f_end: 1.55, steep: 3.0, brk: 0.1, steep2: 1.85, bracket: None }
    }
}

pub const COLOR_MODES: [&str; 6] = ["Standard", "Vivid", "Neutral", "Portrait", "Landscape", "FCBlue"];

/// Radial fine correction of R and B per white balance:
/// `R ·= 1 + a·r⁴ + b·r²`, `B ·= 1 + c·r⁴ + d·r²`, r = normalised distance to the sensor centre.
#[derive(Clone, Copy, Debug)]
pub struct ColorShading {
    pub coef: [f64; 4],
    pub cx: f64,
    pub cy: f64,
    pub denom: f64,
    /// MED resolution: vertical distances count double (pixels are twice as high as wide)
    pub stretch_y: bool,
}

impl ColorShading {
    /// `resolution`: PROP `RESOLUTION` (HI, MED, LOW); the CAMF areas are already given in the pixels of
    /// that resolution.
    fn read(camf: &Camf, wb: &str, active: [usize; 4], resolution: &str) -> Option<Self> {
        let v = camf.matrix(camf.property("WhiteBalanceColorShadingFactor", wb)?).ok()?.as_f64();
        let cr = camf.matrix("DarkShieldColRange").ok()?.as_f64();
        if v.len() != 4 || cr.len() != 4 {
            return None;
        }
        // "Valid PixRect": margin 32 rows at HI, 16 at the reduced resolutions (MED/LOW of the SD1 Merrill)
        let margin = if resolution.contains("HI") || resolution.is_empty() { 32 } else { 16 };
        let stretch_y = resolution.contains("MED");
        let (left, right) = (cr[1] as i64 + 6, cr[2] as i64 - 8);
        let (top, bottom) = (active[1] as i64 - margin, active[3] as i64 + 1 + margin);
        let (hw, hh) = ((right - left) / 2, (bottom - top) / 2);
        let hy = if stretch_y { 2 * hh } else { hh };
        Some(Self {
            coef: [v[0], v[1], v[2], v[3]],
            cx: (left + hw) as f64,
            cy: (top + hh) as f64,
            denom: ((hy * hy + hw * hw) as f64).sqrt(),
            stretch_y,
        })
    }

    /// Factors for R and B at sensor position (x, y).
    #[inline]
    pub fn factors(&self, x: usize, y: usize) -> (f32, f32) {
        let (dx, dy) = (x as f64 - self.cx, (y as f64 - self.cy) * if self.stretch_y { 2.0 } else { 1.0 });
        let r2 = (dx * dx + dy * dy) / (self.denom * self.denom);
        let [a, b, c, d] = self.coef;
        ((1.0 + a * r2 * r2 + b * r2) as f32, (1.0 + c * r2 * r2 + d * r2) as f32)
    }
}

/// Known defective pixels of a list: (row, column, flags).
#[derive(Clone, Copy, Debug)]
pub struct BadPixel {
    pub row: u16,
    pub col: u16,
    pub flags: u16,
}

fn bad_pixel_list(camf: &Camf, name: &str) -> Vec<BadPixel> {
    let Ok(m) = camf.matrix(name) else { return Vec::new() };
    if m.shape().first() != Some(&3) {
        return Vec::new();
    }
    m.as_f64()
        .chunks_exact(3)
        .map(|e| BadPixel { row: e[0] as u16, col: e[1] as u16, flags: e[2] as u16 })
        .collect()
}

/// Minimum "badness" from which defective pixels are replaced: depends on exposure time
/// and sensor temperature, because dark current grows with both.
fn badness_threshold(camf: &Camf) -> u16 {
    let table = camf
        .matrix("BadnessTable")
        .ok()
        .map(|m| m.as_f64())
        .filter(|v| v.len() == 12)
        .unwrap_or_else(|| vec![30., 10000., 0., 30., 3000., 1., 30., 500., 2., 30., 200., 3.]);
    let k = std::f64::consts::LN_2 / 8.0;
    let d: Vec<f64> = (0..4).map(|i| table[i * 3 + 1] * (k * table[i * 3]).exp()).collect();
    let t = scalar(camf, "CaptureShutter").unwrap_or(0.0) / 1000.0;
    let x = t * (k * scalar(camf, "SensorTemperature").unwrap_or(20.0)).exp();
    let (dmin, dmax) = d.iter().fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    let bad = if x <= dmin {
        table[11]
    } else if x >= dmax {
        table[2]
    } else {
        let mut i = 3;
        while i > 0 && d[i] < x {
            i -= 1;
        }
        table[i * 3 + 2]
    } as i64;
    let offset = scalar(camf, "BPBadnessOffset").unwrap_or(1.0) as i64;
    if bad == 0 { 0 } else { (bad - offset).max(0) as u16 }
}

/// A Sigma colour mode: colour matrix (CMCM), contrast offset (CMCC) and tone curve row.
#[derive(Clone, Debug)]
pub struct ColorMode {
    pub name: String,
    /// Camera (white-balanced) → linear sRGB, incl. ISO factor and CMCM
    pub cam_to_srgb: Mat3,
    /// Colour mode compensation (CMCM); `cam_to_srgb = WB correction · ISO · cmcm`
    pub cmcm: Mat3,
    pub contrast: f64,
    pub tone: ToneRow,
    /// sd Quattro: colour mode fine correction from the CAMF for the colour stage ([`crate::quattro_color::Axes`])
    pub axes: Option<std::sync::Arc<crate::quattro_color::Axes>>,
    /// Classic: conversion between camera values and the chain's codes for this mode
    /// ([`crate::classic_new::conversion`]); `None` = none
    pub classic: Option<std::sync::Arc<crate::classic_new::conversion::Conversion>>,
    /// Classic: slider-independent part of the output step ([`crate::classic_new::output`]),
    /// made from the chain's codes; `None` = chain not available
    pub classic_output: Option<std::sync::Arc<crate::classic_new::output::OutputBasis>>,
}

/// White balance preset from the CAMF (Sunlight, Shade, … as in the camera menu).
#[derive(Clone, Debug)]
pub struct WbPreset {
    pub name: String,
    /// `WhiteBalanceGains` · sensor/temperature/aperture factors
    pub gain: [f64; 3],
    /// `WhiteBalanceColorCorrections`
    pub ccm: Mat3,
}

/// sd Quattro (H) at image quality LOW: per-layer ratio `SensorAdjustmentGainFact_LOW / _HI` (1 for HI files, for
/// dp Quattro, and when either entry is missing or implausible).
pub fn sd_low_adjustment(file: &X3fFile, camf: &Camf) -> [f64; 3] {
    if !file.is_quattro_low() {
        return [1.0; 3];
    }
    match (vec3(camf, "SensorAdjustmentGainFact_LOW"), vec3(camf, "SensorAdjustmentGainFact_HI")) {
        (Some(l), Some(h)) => {
            let r: [f64; 3] = std::array::from_fn(|c| l[c] / h[c]);
            if r.iter().all(|v| v.is_finite() && (0.5..=2.0).contains(v)) {
                r
            } else {
                [1.0; 3]
            }
        }
        _ => [1.0; 3],
    }
}

/// White balance presets in the order of the camera menu.
/// White balance name of a Quattro file's `WB_DESC` as in the CAMF tables (`Auto(Natural)` = `AutoLSP`, the
/// light-source-priority auto white balance).
pub fn quattro_wb_name(desc: &str) -> String {
    match desc.trim() {
        "Auto(Natural)" | "Auto (Natural)" => "AutoLSP".into(),
        other => other.to_string(),
    }
}

pub const WB_PRESETS: [&str; 7] = ["Auto", "Sunlight", "Shade", "Overcast", "Incandescent", "Fluorescent", "Flash"];

#[derive(Clone)]
pub struct Calibration {
    pub camera: String,
    /// Camera family (determines the count factor)
    pub model: Model,
    /// Normalised WB gain → sensor counts: DP 2, SD1 1/0.55
    pub count_scale: f64,
    /// psat = 2^ImageDepth − 1 (Merrill: 4095); reference of the tone curve in counts
    pub pixel_saturation: f64,
    /// Matrix to apply after the tone curve when the camera was set to Adobe RGB
    /// (the counts are then treated as Adobe RGB, as SPP does); `None` = sRGB
    pub output_to_srgb: Option<Mat3>,
    pub wb_name: String,
    /// All white balance presets of the camera (incl. the shot setting)
    pub wb_presets: Vec<WbPreset>,
    /// `CaptureISO / SensorISO`
    pub iso_gain: f64,
    /// Quattro: exposure correction (EV) per white balance preset: Sigma's output corresponds to white balance gains normalised to the middle layer, our
    /// calibration to the smallest gain (dp Quattro) or not at all (sd Quattro, sd Quattro H). Empty for
    /// the other cameras.
    pub sigma_wb_ev: Vec<(String, f64)>,
    /// Quattro: strength of our base sharpening still to apply on the counts (the preprocessing applies only part
    /// of it below ISO 800, see `quattro::prepare`); 0 = already applied or none
    pub pending_sharpen: f32,
    /// Quattro, merged bracket developed like its base exposure: factor already in the white balance gains that
    /// lifts the merge's units to the base exposure's (`quattro::prepare`); the colour stage takes its light level
    /// relative to the base exposure. 1 otherwise.
    pub exposure_gain: f64,
    /// Quattro: `SensorISO` of the shot (analogue sensor ISO, 100 when unknown); 0 = other cameras
    pub sensor_iso: f64,
    /// Colour mode set in the camera
    pub camera_mode: String,
    /// All colour modes in the order of [`COLOR_MODES`]
    pub modes: Vec<ColorMode>,
    /// White balance gain per plane (incl. sensor, temperature and aperture factor)
    pub wb_gain: [f64; 3],
    /// ColorDQ after the colour matrix (strength `ColorDQCamRGB · min(noise scale, ISO factor)`)
    pub color_dq: Option<crate::sigma_out::ColorDq>,
    pub black: [f64; 3],
    /// Value from which a plane counts as saturated (after black subtraction)
    pub saturation: f64,
    /// Image area (inclusive): left, top, right, bottom
    pub active: [usize; 4],
    pub spatial: Option<[GainTable; 3]>,
    pub shading: Option<ColorShading>,
    /// Defective pixels from `BadPixelsF20` and `Jpeg_BadClusters` (sensor coordinates)
    pub bad_pixels: Vec<BadPixel>,
    pub badness_threshold: u16,
    pub noise_model: Option<[[f64; 3]; 3]>,
    /// `ISONoiseSettings` (rows of 13 values, column 0 = ISO)
    pub iso_noise_settings: Vec<f64>,
    /// `DenoisingSlidersStep` (5 rows of 13 values)
    pub denoise_slider_steps: Vec<f64>,
    pub residual_epsilon: f64,
    pub edge_lin_shift: f64,
    pub iso: f64,
    /// Camera orientation: quarter turns clockwise to straighten (PROP `ROTATION`)
    pub orientation: u8,
    /// Binning factor of the image this calibration belongs to (1 = full size; Quattro at high ISO, see
    /// [`crate::quattro::binning_factor`] and [`Calibration::binned`])
    pub binning: usize,
    /// Image area before binning (`None` = not binned)
    pub unbinned: Option<[usize; 2]>,
}

impl Calibration {
    /// Calibration of the same image binned `n × n` to `w × h` (Quattro: no position-dependent tables, only the image
    /// area changes).
    pub fn binned(&self, n: usize, w: usize, h: usize) -> Self {
        Self { active: [0, 0, w.max(1) - 1, h.max(1) - 1], binning: n, unbinned: Some(self.full_size()), ..self.clone() }
    }

    /// Image area at full size (before binning).
    pub fn full_size(&self) -> [usize; 2] {
        self.unbinned.unwrap_or([self.active[2] - self.active[0] + 1, self.active[3] - self.active[1] + 1])
    }

    pub fn read(file: &X3fFile, raw: &RawImage) -> x3f::Result<Self> {
        let camf = file.camf()?;
        let props = file.properties()?;
        let camera = props.get("CAMMODEL").unwrap_or("SIGMA").to_string();
        let model = Model::from_name(&camera);
        let depth = scalar(&camf, "ImageDepth").filter(|d| (1.0..=16.0).contains(d)).unwrap_or(12.0);
        let pixel_saturation = (1u32 << depth as u32) as f64 - 1.0;
        let color_space = file.thumbnail_jpeg().and_then(jpeg_color_space).or_else(|| file.preview_jpeg().and_then(jpeg_color_space));
        let output_to_srgb = (color_space.is_some_and(|c| c != 1)).then_some(ADOBE_TO_SRGB);

        let mut wb = props.get("WB_DESC").unwrap_or("Auto").to_string();
        if camf.property("WhiteBalanceGains", &wb).is_none() {
            wb = "Auto".into();
        }
        // The SPP base (x3fuse-core) computes with the white balance written in the file header, where
        // SIGMA Photo Pro also stores its own choice; "as shot" must be the same white balance, otherwise a
        // preset is converted from the wrong one (applied twice).
        if let Some(h) = x3f::sppa::read(file).filter(|a| a.source == x3f::sppa::Source::Header).and_then(|a| a.white_balance) {
            let alias = match h.as_str() {
                "Sunlight" => "Daylight",
                "Daylight" => "Sunlight",
                other => other,
            };
            if let Some(name) = [h.as_str(), alias].into_iter().find(|n| camf.property("WhiteBalanceGains", n).is_some()) {
                wb = name.to_string();
            }
        }
        let preset = |wb: &str| -> Option<WbPreset> {
            let mut gain = vec3(&camf, camf.property("WhiteBalanceGains", wb)?)?;
            for fact in ["SensorAdjustmentGainFact", "TempGainFact", "FNumberGainFact"] {
                // The temperature factor is dropped entirely if one value lies outside 0.5…1.5 (same result as SPP).
                let valid = |f: &[f64; 3]| fact != "TempGainFact" || f.iter().all(|v| v.is_finite() && (0.5..=1.5).contains(v));
                if let Some(f) = vec3(&camf, fact).filter(valid) {
                    for c in 0..3 {
                        gain[c] *= f[c];
                    }
                }
            }
            let ccm = camf.property("WhiteBalanceColorCorrections", wb).and_then(|n| mat3(&camf, n)).unwrap_or(Mat3::IDENTITY);
            Some(WbPreset { name: wb.to_string(), gain, ccm })
        };
        let shot = preset(&wb).unwrap_or(WbPreset { name: wb.clone(), gain: [1.0; 3], ccm: Mat3::IDENTITY });
        let mut wb_presets: Vec<WbPreset> = WB_PRESETS.iter().filter_map(|n| preset(n)).collect();
        if !wb_presets.iter().any(|p| p.name == shot.name) {
            wb_presets.push(shot.clone());
        }
        let (gain, ccm) = (shot.gain, shot.ccm);
        let iso_gain = match (scalar(&camf, "CaptureISO"), scalar(&camf, "SensorISO")) {
            (Some(c), Some(s)) if s > 0.0 => c / s,
            _ => 1.0,
        };

        let base = ccm.scale(iso_gain);
        let tone_rows = camf.matrix("TCColorModeSettings").ok().map(|m| m.as_f64()).unwrap_or_default();
        let modes = COLOR_MODES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let cmcm = camf
                    .property("ColorModeCompensations", name)
                    .and_then(|n| mat3(&camf, n))
                    .filter(|m| m.0[0] != 0.0)
                    .or_else(|| (*name == "FCBlue").then(|| fcblue_matrix(model == Model::Dp)))
                    .unwrap_or(Mat3::IDENTITY);
                let mut contrast = camf
                    .property("ColorModeContrastCompensations", name)
                    .and_then(|n| scalar(&camf, n))
                    .unwrap_or(0.0);
                // FCBlue without CAMF contrast offset → 0.3
                if *name == "FCBlue" && contrast == 0.0 {
                    contrast = 0.3;
                }
                let tone = tone_rows
                    .get(i * 7..i * 7 + 7)
                    .map(|r| ToneRow { scale: r[0], f_start: r[1], f_end: r[2], steep: r[3], brk: r[5], steep2: r[6], bracket: None })
                    .unwrap_or_default();
                ColorMode { name: name.to_string(), cam_to_srgb: base.mul(&cmcm), cmcm, contrast, tone, axes: None, classic: None, classic_output: None }
            })
            .collect();
        let camera_mode = props
            .get("CM_DESC")
            .filter(|m| COLOR_MODES.contains(m))
            .unwrap_or("Standard")
            .to_string();

        let black = merrill_fixed_dark_shield(&camf, props.get("RESOLUTION").unwrap_or("HI"))
            .or_else(|| rect(&camf, "DarkShieldTop"))
            .map(|r| black_level(raw, r))
            .unwrap_or([0.0; 3]);
        let active = rect(&camf, "ActiveImageArea").unwrap_or([0, 0, raw.columns - 1, raw.rows - 1]);
        let saturation = pixel_saturation - black.iter().cloned().fold(0.0, f64::max);

        let fnumber = props.get("APERTURE").and_then(|s| s.parse().ok()).unwrap_or(2.8);
        let focus = scalar(&camf, "ScaleENC").unwrap_or(0.0);

        Ok(Self {
            camera,
            model,
            count_scale: model.count_scale(),
            pixel_saturation,
            output_to_srgb,
            wb_name: wb.clone(),
            wb_presets,
            iso_gain,
            sigma_wb_ev: Vec::new(),
            pending_sharpen: 0.0,
            exposure_gain: 1.0,
            sensor_iso: 0.0,
            camera_mode,
            modes,
            wb_gain: gain,
            color_dq: vec3(&camf, "ColorDQCamRGB").and_then(|st| {
                // ISONoiseSettings column 7 (constant 4 for Merrill) × slider step column 7 (1)
                let noise = camf
                    .matrix("ISONoiseSettings")
                    .ok()
                    .map(|m| m.as_f64())
                    .and_then(|v| v.get(7).copied())
                    .unwrap_or(4.0);
                let scale = noise.min(iso_gain);
                crate::sigma_out::ColorDq::new(st.map(|v| v * scale))
            }),
            black,
            saturation,
            active,
            spatial: spatial_gains(&camf, fnumber, focus),
            shading: ColorShading::read(&camf, &wb, active, props.get("RESOLUTION").unwrap_or("HI")),
            bad_pixels: {
                let mut v = bad_pixel_list(&camf, "BadPixelsF20");
                v.extend(bad_pixel_list(&camf, "Jpeg_BadClusters"));
                v
            },
            badness_threshold: badness_threshold(&camf),
            noise_model: camf
                .matrix("ColorNoiseModel")
                .ok()
                .map(|m| m.as_f64())
                .filter(|v| v.len() == 9)
                .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]),
            iso_noise_settings: camf
                .matrix("ISONoiseSettings")
                .ok()
                .map(|m| m.as_f64())
                .filter(|v| v.len() % 13 == 0)
                .unwrap_or_default(),
            denoise_slider_steps: camf
                .matrix("DenoisingSlidersStep")
                .ok()
                .map(|m| m.as_f64())
                .filter(|v| v.len() == 65)
                .unwrap_or_default(),
            residual_epsilon: scalar(&camf, "ResidualEpsilon").unwrap_or(5.0),
            edge_lin_shift: scalar(&camf, "EdgeMapLinShift").unwrap_or(10.0),
            iso: props.get("ISO").and_then(|s| s.parse().ok()).unwrap_or(100.0),
            orientation: crate::geometry::quarters_from_degrees(props.get("ROTATION").and_then(|s| s.parse().ok()).unwrap_or(0)),
            binning: 1,
            unbinned: None,
        })
    }

    /// Calibration of a Quattro file whose image was preprocessed by x3fuse-core
    /// ([`crate::quattro`]). The image is a synthetic raw image
    /// (black = `ENC_BLACK`, saturation = `ENC_SCALE`, already cropped to the image area).
    ///
    /// Counts (tone curve input) = ISO · CC(WB) · CMCM(mode) · diag(g) · x, x = normalised
    /// camera space. dp Quattro: g = WB gain · n2c from x3fuse-core's port of the SPP colour modes.
    /// sd Quattro (H): the SPP calibration is missing there; g is derived from the camera matrix of
    /// x3fuse-core (Kalpanika method incl. DigitalISOGain), brightness as for dp Quattro.
    pub fn read_quattro(
        file: &X3fFile,
        camf: &Camf,
        props: &x3f::PropList,
        scene: &x3f_core::SceneLinearImage,
        size: [usize; 2],
    ) -> Result<Self, String> {
        use crate::quattro::{ENC_BLACK, ENC_SCALE};
        let camera = props.get("CAMMODEL").unwrap_or("SIGMA Quattro").to_string();
        let camera_id = scalar(camf, "CAMERAID").unwrap_or(0.0) as u32;
        let sd = matches!(camera_id, 40 | 41) || camera.contains("sd Quattro");
        let model = if sd { Model::SdQuattro } else { Model::DpQuattro };
        let depth = scalar(camf, "ImageDepth").filter(|d| (1.0..=16.0).contains(d)).unwrap_or(14.0);
        let pixel_saturation = (1u32 << depth as u32) as f64 - 1.0;
        let iso_gain = quattro_iso_gain(camf);

        // White balance of the shot as SIGMA Photo Pro shows it ("Auto(Natural)" = the light-source-priority
        // auto white balance `AutoLSP`); x3fuse-core's camera matrix may use another one (its CAMF code), which is
        // taken out below (`decoded`).
        let shot_name = quattro_wb_name(props.get("WB_DESC").unwrap_or("Auto"));
        let mut wb = props.get("WB_DESC").unwrap_or("Auto").to_string();
        if camf.property("WhiteBalanceGains", &wb).is_none() {
            wb = "Auto".into();
        }
        // "ColorTemp" (code 11) has no table entry: gains and matrix from the colour temperature tables
        // (`quattro_wb`), at the shot's kelvin or, for "ColorTemp <K>K", at that value (SPP-saved setting).
        let color_temp = crate::quattro_wb::ColorTempTables::read(camf);
        let shot_kelvin = crate::quattro_wb::shot_kelvin(camf);
        let from_temp = |name: &str| -> Option<([f64; 3], Mat3)> {
            let k = if name == "ColorTemp" { shot_kelvin } else { crate::quattro_wb::kelvin_of(name)? };
            crate::quattro_wb::gains_and_matrix(color_temp.as_ref()?, k)
        };
        let raw_gain = |name: &str| vec3(camf, camf.property("WhiteBalanceGains", name)?).or_else(|| from_temp(name).map(|t| t.0));
        let ccm = |name: &str| camf.property("WhiteBalanceColorCorrections", name).and_then(|n| mat3(camf, n)).or_else(|| from_temp(name).map(|t| t.1));
        if shot_name.starts_with("ColorTemp") && from_temp(&shot_name).is_none() {
            eprintln!("Quattro: white balance ColorTemp without usable CAMF tables, using Auto");
        }
        let shot_raw = raw_gain(&wb).ok_or("Weißabgleich fehlt in der CAMF")?;
        let shot_ccm = ccm(&wb).ok_or("Farbkorrekturmatrix fehlt in der CAMF")?;
        let norm = |g: [f64; 3]| {
            let min = g.iter().cloned().fold(f64::INFINITY, f64::min);
            g.map(|v| v / min)
        };

        // Gain per layer in counts per unit x at the shot white balance
        let counts_gain: [f64; 3] = match (&scene.color_mode_calibration, sd) {
            (Some(c), false) => {
                // Cross-check: the same CC matrix as x3fuse-core?
                let diff = c.sensor_to_srgb.iter().zip(shot_ccm.0.iter()).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
                if diff > 1e-6 && std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                    eprintln!("Quattro: CC matrix differs from x3fuse-core ({diff:.2e}), WB {wb}");
                }
                // ISO factor not from x3fuse-core (see `quattro_iso_gain`) but our own
                std::array::from_fn(|i| c.wb_gain[i] * c.normalized_to_counts[i])
            }
            _ => {
                // camera_to_rgb = iso · CC · diag(gain) → diagonal of CC⁻¹ · camera_to_rgb
                let cam = Mat3(scene.white_balance.camera_to_rgb);
                let d = shot_ccm.inverse().ok_or("CC-Matrix singulär")?.mul(&cam);
                let off = [1, 2, 3, 5, 6, 7].iter().map(|&i| d.0[i].abs()).fold(0.0, f64::max);
                let on = [0, 4, 8].iter().map(|&i| d.0[i].abs()).fold(0.0, f64::max);
                if off > 1e-3 * on && std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                    eprintln!("Quattro: camera matrix not diagonal to the CC matrix ({:.2e})", off / on);
                }
                // Brightness reference as for dp Quattro: n2c = 2^14 − 1 − RawOffset
                let n2c = pixel_saturation - scalar(camf, "RawOffset").unwrap_or(2048.0).min(4096.0);
                // image quality LOW: the maker's software takes the sensor adjustment for binned top layers
                // (`SensorAdjustmentGainFact_LOW` instead of `_HI`); our matrix is calibrated at HI → their ratio
                let low = sd_low_adjustment(file, camf);
                std::array::from_fn(|i| d.0[i * 4] * n2c * QUATTRO_SD_EXPOSURE / iso_gain * low[i])
            }
        };
        let wb_gain: [f64; 3] = counts_gain.map(|g| g / ENC_SCALE);
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            let t = vec3(camf, "TempGainFact").unwrap_or([1.0; 3]);
            eprintln!("Quattro WB {wb}: counts gain {:?} (/green {:?}), CAMF·Temp /green {:?}", counts_gain,
                counts_gain.map(|g| g / counts_gain[1]), [0, 1, 2].map(|c| shot_raw[c] * t[c] / (shot_raw[1] * t[1])));
        }
        let shot_norm = norm(shot_raw);
        let preset = |name: &str| -> Option<WbPreset> {
            let g = norm(raw_gain(name)?);
            Some(WbPreset {
                name: name.to_string(),
                gain: std::array::from_fn(|c| wb_gain[c] * g[c] / shot_norm[c]),
                ccm: ccm(name)?,
            })
        };
        // Sigma's output corresponds to white balance gains normalised to the middle layer; ours are
        // normalised to the smallest gain (dp Quattro, x3fuse-core) or taken as stored (sd Quattro/H).
        let shot_min = shot_raw.iter().cloned().fold(f64::INFINITY, f64::min);
        let sigma_wb_ev: Vec<(String, f64)> = WB_PRESETS
            .iter()
            .map(|n| n.to_string())
            .chain(std::iter::once(wb.clone()))
            .chain(std::iter::once(shot_name.clone()))
            .filter_map(|name| {
                let g = raw_gain(&name)?;
                let min = g.iter().cloned().fold(f64::INFINITY, f64::min);
                let ratio = if sd { min / (shot_min * g[1]) } else { min / g[1] };
                (ratio.is_finite() && ratio > 0.0).then(|| (name, ratio.log2()))
            })
            .collect();
        let decoded = WbPreset { name: wb.clone(), gain: wb_gain, ccm: shot_ccm };
        // the shot's own white balance where x3fuse-core decoded with another one (gains relative as for the presets)
        let shot = if shot_name != wb && std::env::var("GRAINEO_QUATTRO_WB_LSP").map_or(true, |v| v != "0") {
            preset(&shot_name).unwrap_or_else(|| decoded.clone())
        } else {
            decoded.clone()
        };
        let (wb, wb_gain, shot_ccm) = (shot.name.clone(), shot.gain, shot.ccm);
        let mut wb_presets: Vec<WbPreset> = WB_PRESETS.iter().filter_map(|n| preset(n)).collect();
        // colour temperature saved by SIGMA Photo Pro: own preset at that kelvin (`develop::sppa` selects it)
        if let Some(k) = x3f::sppa::read(file).filter(|a| a.white_balance.as_deref() == Some("ColorTemp")).and_then(|a| a.wb_temperature) {
            if let Some(p) = preset(&crate::quattro_wb::preset_name(k)) {
                wb_presets.push(p);
            }
        }
        for p in [&decoded, &shot] {
            if !wb_presets.iter().any(|q| q.name == p.name) {
                wb_presets.push(p.clone());
            }
        }

        let base = shot_ccm.scale(iso_gain);
        let modes = COLOR_MODES
            .iter()
            .chain(QUATTRO_EXTRA_MODES.iter())
            .filter_map(|name| {
                let cmcm = if *name == "TealAndOrange" {
                    Some(QUATTRO_TEAL_ORANGE)
                } else {
                    camf.property("ColorModeCompensations", name).and_then(|n| mat3(camf, n)).filter(|m| m.0[0] != 0.0)
                };
                let cmcm = match (cmcm, *name) {
                    (Some(m), _) => m,
                    (None, "Standard") => Mat3::IDENTITY,
                    _ => return None,
                };
                let contrast = camf.property("ColorModeContrastCompensations", name).and_then(|n| scalar(camf, n)).unwrap_or(0.0);
                // sd Quattro: fine correction per colour mode (dp Quattro: not measured against Sigma, left out)
                let table = sd.then(|| camf.property("ColorModeMultiAxisTableCompensations", name)).flatten().and_then(|n| camf.matrix(n).ok()).map(|m| m.as_f64());
                let axes = table.as_deref().and_then(crate::quattro_color::Axes::from_table).map(std::sync::Arc::new);
                Some(ColorMode { name: name.to_string(), cam_to_srgb: base.mul(&cmcm), cmcm, contrast, tone: quattro_tone_row(name), axes, classic: None, classic_output: None })
            })
            .collect::<Vec<_>>();
        let camera_mode = props
            .get("CM_DESC")
            .filter(|m| modes.iter().any(|x| x.name == *m))
            .unwrap_or("Standard")
            .to_string();
        let [w, h] = size;
        Ok(Self {
            camera,
            model,
            count_scale: model.count_scale(),
            pixel_saturation,
            // Quattro: the tone curve is applied directly in the target space (no Adobe RGB conversion), as in SPP.
            output_to_srgb: None,
            wb_name: wb,
            wb_presets,
            iso_gain,
            sigma_wb_ev,
            pending_sharpen: 0.0,
            exposure_gain: 1.0,
            sensor_iso: scalar(camf, "SensorISO").filter(|v| *v > 0.0).unwrap_or(100.0),
            camera_mode,
            modes,
            wb_gain,
            color_dq: None,
            black: [ENC_BLACK; 3],
            saturation: ENC_SCALE,
            active: [0, 0, w - 1, h - 1],
            spatial: None,
            shading: None,
            bad_pixels: Vec::new(),
            badness_threshold: 0,
            noise_model: None,
            iso_noise_settings: Vec::new(),
            denoise_slider_steps: Vec::new(),
            residual_epsilon: 5.0,
            edge_lin_shift: 10.0,
            iso: props.get("ISO").and_then(|s| s.parse().ok()).unwrap_or(100.0),
            orientation: crate::geometry::quarters_from_degrees(props.get("ROTATION").and_then(|s| s.parse().ok()).unwrap_or(0)),
            binning: 1,
            unbinned: None,
        })
    }

    /// Calibration of a Classic file (SD9/SD10/SD14/SD15, DP1/DP2 incl. s/x) whose image
    /// was preprocessed by x3fuse-core ([`crate::quattro`], same path as Quattro).
    ///
    /// Classic CAMF has no SPP colour mode tables like Merrill. White balance and colour matrix
    /// per preset as in Kalpanika/x3fuse-core ([`classic_wb`]): `(DP1_)WhiteBalanceGains` +
    /// `(DP1_)WhiteBalanceColorCorrections` (TRUE cameras), otherwise from `WhiteBalanceIlluminants`
    /// and `WhiteBalanceCorrections` (SD9/SD10). Brightness follows the camera matrix of
    /// x3fuse-core (incl. ISO), scaled with [`CLASSIC_EXPOSURE`] to the count domain.
    pub fn read_classic(
        file: &X3fFile,
        camf: &Camf,
        props: &x3f::PropList,
        scene: &x3f_core::SceneLinearImage,
        size: [usize; 2],
    ) -> Result<Self, String> {
        use crate::quattro::{ENC_BLACK, ENC_SCALE};
        let camera = props.get("CAMMODEL").unwrap_or("SIGMA").trim().to_string();
        let model = Model::Classic;
        let pixel_saturation = 16383.0;
        let iso_gain = match (scalar(camf, "CaptureISO"), scalar(camf, "SensorISO")) {
            (Some(c), Some(s)) if s > 0.0 && c > 0.0 => c / s,
            _ => 1.0,
        };
        // colour space set in the camera (EXIF of the embedded JPEG), as the chain reads it
        let adobe_camera = file.exif().and_then(|e| e.color_space) == Some(0xFFFF);
        let mut wb = props.get("WB_DESC").unwrap_or("Auto").trim().to_string();
        if classic_wb(camf, &wb).is_none() {
            wb = "Auto".into();
        }
        // As with Merrill: SIGMA Photo Pro takes the white balance from the file header (the SD10 sample
        // has "Sunlight" there and "Auto" in the properties; SPP renders it as Sunlight).
        if std::env::var("GRAINEO_CLASSIC_HEADER_WB").map_or(true, |v| v != "0") {
            if let Some(h) = x3f::sppa::read(file).filter(|a| a.source == x3f::sppa::Source::Header).and_then(|a| a.white_balance) {
                let alias = match h.as_str() {
                    "Sunlight" => "Daylight",
                    "Daylight" => "Sunlight",
                    other => other,
                };
                if let Some(name) = [h.as_str(), alias].into_iter().find(|n| classic_wb(camf, n).is_some()) {
                    wb = name.to_string();
                }
            }
        }
        let (shot_raw, shot_ccm) = classic_wb(camf, &wb).ok_or("Weißabgleich fehlt in der CAMF")?;
        // camera_to_rgb = k · CC · diag(gain) → diagonal of CC⁻¹ · camera_to_rgb
        let cam = Mat3(scene.white_balance.camera_to_rgb);
        let d = shot_ccm.inverse().ok_or("CC-Matrix singulär")?.mul(&cam);
        let off = [1, 2, 3, 5, 6, 7].iter().map(|&i| d.0[i].abs()).fold(0.0, f64::max);
        let on = [0, 4, 8].iter().map(|&i| d.0[i].abs()).fold(0.0, f64::max);
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!(
                "Classic {camera}: WB {wb}, gain {shot_raw:?}, diagonal {:?} (residual {:.1e}), ISO factor {iso_gain}",
                [d.0[0], d.0[4], d.0[8]],
                off / on
            );
        }
        let exposure = std::env::var("GRAINEO_CLASSIC_EXPOSURE").ok().and_then(|v| v.parse().ok()).unwrap_or(CLASSIC_EXPOSURE);
        let tint: [f64; 3] = std::env::var("GRAINEO_CLASSIC_BALANCE")
            .ok()
            .map(|t| t.split(',').filter_map(|x| x.parse().ok()).collect::<Vec<f64>>())
            .filter(|v| v.len() == 3)
            .map_or(CLASSIC_LAYER_BALANCE, |v| [v[0], v[1], v[2]]);
        // SD9/SD10/SD14: the gain from illuminant + correction (Kalpanika) has no fixed
        // scale → normalise to a minimum of 1 like the `WhiteBalanceGains` tables.
        let has_gain_table = ["WhiteBalanceGains", "DP1_WhiteBalanceGains"].iter().any(|l| camf.entries.contains_key(*l));
        // Brightness reference there additionally empirical (SD14 JPEG, SD9/SD10 previews: +0.5…1.7 EV
        // too bright, mean ≈ 1 EV).
        let scale = if has_gain_table { 1.0 } else { CLASSIC_EXPOSURE_PRE_TRUE / shot_raw.iter().cloned().fold(f64::INFINITY, f64::min) };
        let counts_gain: [f64; 3] = std::array::from_fn(|i| d.0[i * 4] * pixel_saturation * exposure / iso_gain * tint[i] * scale);
        let wb_gain: [f64; 3] = counts_gain.map(|g| g / ENC_SCALE);
        let norm = |g: [f64; 3]| {
            let min = g.iter().cloned().fold(f64::INFINITY, f64::min);
            g.map(|v| v / min)
        };
        let shot_norm = norm(shot_raw);
        let preset = |name: &str| -> Option<WbPreset> {
            let (g, ccm) = classic_wb(camf, name)?;
            let g = norm(g);
            Some(WbPreset { name: name.to_string(), gain: std::array::from_fn(|c| wb_gain[c] * g[c] / shot_norm[c]), ccm })
        };
        let shot = WbPreset { name: wb.clone(), gain: wb_gain, ccm: shot_ccm };
        let mut wb_presets: Vec<WbPreset> = WB_PRESETS.iter().filter_map(|n| preset(n)).collect();
        if !wb_presets.iter().any(|p| p.name == shot.name) {
            wb_presets.push(shot.clone());
        }

        // Colour modes: only what the CAMF provides (later firmware); otherwise only Standard.
        let base = shot_ccm.scale(iso_gain);
        let tone = classic_tone_row();
        let modes = COLOR_MODES
            .iter()
            .filter_map(|name| {
                let cmcm = camf.property("ColorModeCompensations", name).and_then(|n| mat3(camf, n)).filter(|m| m.0[0] != 0.0);
                let cmcm = match (cmcm, *name) {
                    (Some(m), _) => m,
                    (None, "Standard") => Mat3::IDENTITY,
                    _ => return None,
                };
                let contrast = camf.property("ColorModeContrastCompensations", name).and_then(|n| scalar(camf, n)).unwrap_or(0.0);
                let classic = crate::classic_new::conversion::Conversion::new(camf, props, adobe_camera, &wb, iso_gain, wb_gain, &cmcm).map(std::sync::Arc::new);
                Some(ColorMode { name: name.to_string(), cam_to_srgb: base.mul(&cmcm), cmcm, contrast, tone, axes: None, classic, classic_output: None })
            })
            .collect::<Vec<_>>();
        let camera_mode = props
            .get("CM_DESC")
            .map(str::trim)
            .filter(|m| modes.iter().any(|x| x.name == *m))
            .unwrap_or("Standard")
            .to_string();
        let degrees = props.get("ROTATION").and_then(|s| s.trim().parse().ok()).or_else(|| file.header_rotation().map(|d| d as i32)).unwrap_or(0);
        let [w, h] = size;
        Ok(Self {
            camera,
            model,
            count_scale: model.count_scale(),
            pixel_saturation,
            output_to_srgb: None,
            wb_name: wb,
            wb_presets,
            iso_gain,
            sigma_wb_ev: Vec::new(),
            pending_sharpen: 0.0,
            exposure_gain: 1.0,
            sensor_iso: 0.0,
            camera_mode,
            modes,
            wb_gain,
            color_dq: None,
            black: [ENC_BLACK; 3],
            saturation: ENC_SCALE,
            active: [0, 0, w - 1, h - 1],
            spatial: None,
            shading: None,
            bad_pixels: Vec::new(),
            badness_threshold: 0,
            noise_model: None,
            iso_noise_settings: Vec::new(),
            denoise_slider_steps: Vec::new(),
            residual_epsilon: 5.0,
            edge_lin_shift: 10.0,
            iso: props.get("ISO").and_then(|s| s.trim().parse().ok()).unwrap_or(100.0),
            orientation: crate::geometry::quarters_from_degrees(degrees),
            binning: 1,
            unbinned: None,
        })
    }

    /// Quattro (own tone curve, preprocessing via x3fuse-core)?
    pub fn is_quattro(&self) -> bool {
        self.model.is_quattro()
    }

    /// The synthetic raw image was divided by `f` per layer (Classic: normalisation to the
    /// real saturation); adjust gains so that the counts stay the same.
    pub fn rescale_layers(&mut self, f: [f64; 3]) {
        for c in 0..3 {
            self.wb_gain[c] *= f[c];
            for p in &mut self.wb_presets {
                p.gain[c] *= f[c];
            }
        }
    }

    /// Older Foveon generation (Classic)?
    pub fn is_classic(&self) -> bool {
        self.model == Model::Classic
    }

    /// Preprocessing via x3fuse-core with counts up to 65535 (Quattro, Classic)?
    pub fn is_prepared(&self) -> bool {
        self.model.is_prepared()
    }

    /// Largest count that points into the tone curve table (Merrill 32766 as in SPP,
    /// Quattro/Classic 65535; there the curve reaches ≈ 65532 at −2 EV).
    pub fn max_count(&self) -> f32 {
        if self.is_prepared() { 65535.0 } else { 32766.0 }
    }

    /// Colour mode by name; unknown or `None` → the camera's mode.
    /// White balance preset; `None` or unknown = as shot.
    pub fn wb(&self, name: Option<&str>) -> &WbPreset {
        let want = name.unwrap_or(&self.wb_name);
        self.wb_presets
            .iter()
            .find(|p| p.name == want)
            .or_else(|| self.wb_presets.iter().find(|p| p.name == self.wb_name))
            .expect("shot white balance missing")
    }

    /// Camera (amplified with white balance `wb`) → linear sRGB for colour mode and white balance.
    pub fn cam_to_srgb(&self, mode: Option<&str>, wb: Option<&str>) -> Mat3 {
        self.wb(wb).ccm.scale(self.iso_gain).mul(&self.mode(mode).cmcm)
    }

    /// Gain per channel relative to the shot white balance: preset, eyedropper,
    /// temperature (red ↔ blue) and tint (green ↔ magenta).
    pub fn wb_ratio(&self, s: &crate::Settings) -> [f64; 3] {
        let preset = self.wb(s.white_balance.as_deref()).gain;
        std::array::from_fn(|c| preset[c] / self.wb_gain[c] * s.wb_mult[c] as f64)
    }

    /// Camera → linear sRGB including temperature/tint fine adjustment. The fine adjustment
    /// acts in the output space (not in the sensor channels, where the strong Merrill matrices
    /// would amplify even small shifts enormously) and keeps brightness constant,
    /// like SPP's colour adjustment.
    pub fn output_matrix(&self, s: &crate::Settings) -> Mat3 {
        self.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref())
            .mul(&Mat3::diag(self.wb_ratio(s)))
            .pipe(|m| Mat3::diag(temp_tint_gains(s.wb_temp, s.wb_tint)).mul(&m))
    }

    pub fn mode(&self, name: Option<&str>) -> &ColorMode {
        let want = crate::settings::matrix_mode(name).unwrap_or(&self.camera_mode);
        self.modes
            .iter()
            .find(|m| m.name == want)
            .or_else(|| self.modes.iter().find(|m| m.name == self.camera_mode))
            .unwrap_or(&self.modes[0])
    }
}

/// Temperature (−1 cooler … +1 warmer) and tint (−1 greener … +1 magenta) as gain
/// per sRGB channel, normalised to equal luminance. ±1 corresponds to about ±1500 K or a
/// strong but not garish tint.
pub fn temp_tint_gains(temp: f32, tint: f32) -> [f64; 3] {
    if temp == 0.0 && tint == 0.0 {
        return [1.0; 3]; // exactly neutral so the default development stays bit-identical
    }
    let (t, g) = (temp as f64 * 0.22, tint as f64 * 0.12);
    let gains = [(t + g).exp(), (-2.0 * g).exp(), (-t + g).exp()];
    let y = 0.2126 * gains[0] + 0.7152 * gains[1] + 0.0722 * gains[2];
    gains.map(|v| v / y)
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl Pipe for Mat3 {}

#[cfg(test)]
mod dark_shield_tests {
    use super::fixed_dark_shield_for;

    #[test]
    fn dp_merrill_reduced_resolution_takes_the_fixed_rectangle() {
        assert_eq!(fixed_dark_shield_for(78, 0, "LOW"), Some([64, 1608, 2374, 1626]));
        assert_eq!(fixed_dark_shield_for(78, 2, "MED"), Some([64, 1608, 4766, 1626]));
        assert_eq!(fixed_dark_shield_for(78, 1, "LOW"), None);
        assert_eq!(fixed_dark_shield_for(78, 0, "HI"), None);
        assert_eq!(fixed_dark_shield_for(77, 0, "LOW"), None);
    }
}
