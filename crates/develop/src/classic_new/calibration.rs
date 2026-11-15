//! Classic chain, step 1: calibration. Everything the later steps need to know that does
//! not depend on pixel data: camera class, ISO and exposure factors, which colour matrix and
//! which neutral point belong to the chosen white balance, the gain vector of the three layers,
//! the matrix that maps layer codes to the working colour space, and the choice of the spatial gain
//! table.
//!
//! One function per parameter, inputs and outputs explicit. Matrix helpers use a fixed operation
//! order so that results agree to the last bit (all in `f64`, camera data read as `f32`).
//!
//! What is *not* in here (needs pixel data): automatic white balance of cameras without a stored
//! neutral point, dark drift, flare. Those are inputs of this step where needed
//! ([`Input::auto_neutral`]).

use x3f::{Camf, CamfEntry, PropList};

/// 3 × 3 matrix, row-major.
pub type Mat = [f64; 9];
/// Chromaticity of the D65 white point (x, y, 1 − x − y), CIE standard illuminant D65 rounded to
/// four digits; dcraw (`foveon_interpolate`) uses the same three numbers for the neutral point of
/// the layers (response of the white point through the inverse of the camera matrix).
pub const D65: [f64; 3] = [0.3127, 0.329, 0.3583];

/// XYZ → sRGB (PCS white point 1).
const CS_SRGB: Mat = [3.241, -1.5374, -0.4986, -0.9692, 1.876, 0.0416, 0.0556, -0.204, 1.057];
/// XYZ → Adobe RGB (PCS white point 1).
const CS_ADOBE: Mat = [2.0416, -0.565, -0.3447, -0.9692, 1.876, 0.0416, 0.0134, -0.1184, 1.0152];

// ---- matrix helpers (fixed operation order) ----

/// `a · b`.
pub fn mm(a: &Mat, b: &Mat) -> Mat {
    let mut r = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            let mut acc = 0.0;
            for k in 0..3 {
                acc = a[i * 3 + k] * b[k * 3 + j] + acc;
            }
            r[i * 3 + j] = acc;
        }
    }
    r
}

/// `a · v`.
pub fn mv(a: &Mat, v: &[f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i * 3] * v[0] + a[i * 3 + 1] * v[1] + a[i * 3 + 2] * v[2])
}

/// Matrix with `v` on the diagonal.
pub fn diag(v: &[f64; 3]) -> Mat {
    [v[0], 0.0, 0.0, 0.0, v[1], 0.0, 0.0, 0.0, v[2]]
}

pub fn det(m: &Mat) -> f64 {
    (m[0] * (m[4] * m[8] - m[7] * m[5]) - m[1] * (m[3] * m[8] - m[6] * m[5])) + m[2] * (m[3] * m[7] - m[6] * m[4])
}

pub fn inv(m: &Mat) -> Mat {
    let d = 1.0 / det(m);
    [
        (m[4] * m[8] - m[5] * m[7]) * d,
        -(m[1] * m[8] - m[7] * m[2]) * d,
        (m[1] * m[5] - m[4] * m[2]) * d,
        -(m[3] * m[8] - m[5] * m[6]) * d,
        (m[0] * m[8] - m[6] * m[2]) * d,
        -(m[0] * m[5] - m[3] * m[2]) * d,
        (m[3] * m[7] - m[6] * m[4]) * d,
        -(m[0] * m[7] - m[6] * m[1]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ]
}

pub(crate) fn max3(v: &[f64; 3]) -> f64 {
    // larger of the last two, then larger of that and the first
    let b = if v[1] <= v[2] { v[2] } else { v[1] };
    if v[0] <= b { b } else { v[0] }
}

/// XYZ → RIMM primaries (PCS white point 1); RIMM/ROMM RGB primaries and D50 white after
/// ISO 22028-2.
const XYZ_RIMM: Mat = [1.4032, -0.2231, -0.1016, -0.5263, 1.4816, 0.017, -0.0112, 0.0183, 0.9113];

/// Matrix from camera codes to the RIMM working space for a given neutral response `x`: the matrix
/// is first mapped to RIMM, then each output row is scaled so that the neutral maps to equal
/// channels (the channels are floored at a quarter of their weighted mean first); `p4` mixes in the
/// neighbouring rows (0 in all uses here), `p3` is the overall scale (the digital ISO factor).
pub fn cam2rimm(m: &Mat, x: &[f64; 3], p3: f64, p4: f64) -> Mat {
    let r = &XYZ_RIMM;
    // p = r · m, summed in this order
    let p = |i: usize, j: usize| ((r[i * 3 + 2] * m[6 + j] + r[i * 3 + 1] * m[3 + j]) + r[i * 3] * m[j]) + 0.0;
    let mut q: Mat = std::array::from_fn(|k| p(k / 3, k % 3));
    if x[0] >= 0.0 && x[1] >= 0.0 && x[2] >= 0.0 {
        let r0 = (x[2] * q[2] + q[1] * x[1]) + q[0] * x[0];
        let r1 = (x[2] * q[5] + q[3] * x[0]) + x[1] * q[4];
        let r2 = (x[2] * q[8] + x[1] * q[7]) + x[0] * q[6];
        let s = ((0.3 * r0 + r1 * 0.55) + 0.15 * r2) * 0.25;
        let e0 = if r0 <= s { s } else { r0 };
        let e1 = if r1 <= s { s } else { r1 };
        let e2 = if r2 <= s { s } else { r2 };
        let t = (0.15 * e2 + 0.55 * e1) + 0.3 * e0;
        let f = [t / e0, t / e1, t / e2];
        for i in 0..3 {
            for j in 0..3 {
                q[i * 3 + j] *= f[i];
            }
        }
    }
    let c = 1.0 - (p4 + p4);
    let a = |i: usize| q[i];
    let d2 = p4 * a(0) + 0.0;
    let d4 = a(1) * p4 + 0.0;
    let d6 = a(2) * p4 + 0.0;
    [
        (((c * a(0) + 0.0) + p4 * a(3)) + p4 * a(6)) * p3,
        (((c * a(1) + 0.0) + p4 * a(4)) + p4 * a(7)) * p3,
        (((a(8) * p4 + a(2) * c) + 0.0) + a(5) * p4) * p3,
        ((p4 * a(6) + d2) + a(3) * c) * p3,
        ((p4 * a(7) + a(4) * c) + d4) * p3,
        ((a(8) * p4 + a(5) * c) + d6) * p3,
        ((c * a(6) + d2) + p4 * a(3)) * p3,
        ((c * a(7) + d4) + p4 * a(4)) * p3,
        ((c * a(8) + d6) + a(5) * p4) * p3,
    ]
}

// ---- camera data access ----

/// The CAMF as the calibration sees it (values read as `f32`).
pub struct Data<'a> {
    pub camf: &'a Camf,
}

impl Data<'_> {
    pub fn present(&self, name: &str) -> bool {
        self.camf.matrix(name).is_ok()
    }
    pub fn values(&self, name: &str) -> Option<Vec<f64>> {
        self.camf.matrix(name).ok().map(|m| m.as_f64())
    }
    /// First value of a number entry as `f32`, 0 when missing.
    pub fn number(&self, name: &str) -> f32 {
        self.values(name).and_then(|v| v.first().copied()).unwrap_or(0.0) as f32
    }
    pub fn integer(&self, name: &str) -> u32 {
        let v = self.values(name).and_then(|v| v.first().copied()).unwrap_or(0.0) as i64 as u32;
        check_fallback(name, v)
    }
    /// Three numbers (a colour triple), `standard` when missing.
    pub fn triple(&self, name: &str, standard: [f64; 3]) -> [f64; 3] {
        match self.values(name) {
            Some(v) if v.len() >= 3 => [v[0] as f32 as f64, v[1] as f32 as f64, v[2] as f32 as f64],
            _ => standard,
        }
    }
    /// 3 × 3 matrix, all zero when missing.
    pub fn matrix(&self, name: &str) -> Mat {
        match self.values(name) {
            Some(v) if v.len() >= 9 => std::array::from_fn(|i| v[i] as f32 as f64),
            _ => [0.0; 9],
        }
    }
    pub(crate) fn list(&self, name: &str) -> Option<&[(String, String)]> {
        match self.camf.entries.get(name) {
            Some(CamfEntry::Properties(p)) if !p.is_empty() => Some(p),
            _ => None,
        }
    }
}

/// Test aid (no camera file in the measurement set uses these): `GRAINEO_CLASSIC_TEST_SKIP`
/// (number, `0x…` allowed) is added to `SkipStepFlags`, `GRAINEO_CLASSIC_TEST_BULB` replaces
/// `BulbFrameCount`. Only this chain reads them.
fn check_fallback(name: &str, v: u32) -> u32 {
    let number = |k: &str| {
        std::env::var(k).ok().and_then(|t| {
            let t = t.trim();
            match t.strip_prefix("0x") {
                Some(h) => u32::from_str_radix(h, 16).ok(),
                None => t.parse().ok(),
            }
        })
    };
    match name {
        "SkipStepFlags" => number("GRAINEO_CLASSIC_TEST_SKIP").map_or(v, |z| v | z),
        "BulbFrameCount" => number("GRAINEO_CLASSIC_TEST_BULB").unwrap_or(v),
        _ => v,
    }
}

pub(crate) fn entry(list: &[(String, String)], key: &str) -> String {
    list.iter().find(|(k, _)| k.trim().eq_ignore_ascii_case(key)).map(|(_, v)| v.trim().to_string()).unwrap_or_default()
}

// ---- camera class and white balance names ----

/// Camera classes of the calibration (some variants of a model family are folded into one
/// class).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Camera {
    Sd9,
    Sd10,
    Sd14,
    Sd15,
    /// DP1, DP1s, DP1x
    Dp1,
    /// DP2, DP2s, DP2x
    Dp2,
}

/// The raw model code of the camera (before folding), as far as the calibration needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub class: Camera,
    /// exactly the DP2 (not DP2s/DP2x): the only model with colour-mode compensation
    pub is_dp2: bool,
}

pub fn model(name: &str) -> Option<Model> {
    let n = name.trim().to_ascii_uppercase();
    let n = n.strip_prefix("SIGMA").unwrap_or(&n).trim().to_string();
    let (class, is_dp2) = match n.as_str() {
        "SD9" => (Camera::Sd9, false),
        "SD10" => (Camera::Sd10, false),
        "SD14" => (Camera::Sd14, false),
        "SD15" => (Camera::Sd15, false),
        "DP1" | "DP1S" | "DP1X" => (Camera::Dp1, false),
        "DP2" => (Camera::Dp2, true),
        "DP2S" | "DP2X" => (Camera::Dp2, false),
        _ => return None,
    };
    Some(Model { class, is_dp2 })
}

/// White balance of the request: code 1 … 9 and the name the camera lists use for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhiteBalance {
    pub code: u8,
    pub name: &'static str,
}

pub fn white_balance(wb: &str) -> WhiteBalance {
    let (code, name) = match wb.trim().to_ascii_lowercase().as_str() {
        "auto" => (1, "Auto"),
        "daylight" | "sunlight" => (2, "Daylight"),
        "shade" => (3, "Shade"),
        "overcast" | "cloudy" => (4, "Overcast"),
        "incandescent" | "tungsten" => (5, "Incandescent"),
        "fluorescent" => (6, "Fluorescent"),
        "flash" => (7, "Flash"),
        "custom" => (8, "Custom"),
        "monochrome" => (9, "Monochrome"),
        _ => (1, "Auto"),
    };
    WhiteBalance { code, name }
}

// ---- ISO and exposure factors ----

/// ISO, bulb and exposure-time factors.
#[derive(Clone, Copy, Debug)]
pub struct Iso {
    /// capture ISO
    pub iso: f32,
    /// ISO of the sensor's base sensitivity (default 100)
    pub sensor_iso: f32,
    /// `sensor_iso / 100`
    pub sensor_rel: f32,
    /// from this ISO on the range-based reconstruction runs
    pub high: bool,
    /// capture ISO / sensor ISO
    pub factor: f32,
    /// number of stacked frames (long exposure mode when > 1)
    pub bulb_images: u32,
    pub bulb: bool,
    /// ISO factor limited to 2 (not limited in long exposure mode)
    pub factor1: f32,
    /// the rest of the ISO factor (`factor / factor1`), applied digitally
    pub digital: f32,
    /// correction for the number of stacked frames (1 without)
    pub bulb_factor: f64,
}

pub fn iso(d: &Data, request_iso: f32) -> Iso {
    let mut iso = d.number("CaptureISO");
    if iso == 0.0 {
        iso = request_iso;
    }
    let mut sensor = d.number("SensorISO");
    if sensor == 0.0 {
        sensor = 100.0;
    }
    let sensor_rel = sensor / 100.0;
    let high = iso >= 400.0;
    let factor = iso / sensor;
    let bulb_images = d.integer("BulbFrameCount");
    let bulb = (bulb_images as i32) > 1;
    let factor1 = if 2.0 > factor || bulb { factor } else { 2.0 };
    let digital = factor / factor1;
    let n = f64::from(bulb_images);
    let mut bulb_factor = 1.0;
    if bulb {
        bulb_factor = (n + 1.0).sqrt() + n / 6.0;
        if !(2.0 <= f64::from(factor)) {
            bulb_factor = 2.0f64.sqrt() * bulb_factor;
        }
    }
    Iso { iso, sensor_iso: sensor, sensor_rel, high, factor, bulb_images, bulb, factor1, digital, bulb_factor }
}

// ---- matrix and neutral point ----

/// Everything the matrix/neutral choice decides.
#[derive(Clone, Debug)]
pub struct Color {
    /// camera-matrix ("cached") calibration was used (cameras that store matrices and gains per
    /// white balance)
    pub cached: bool,
    /// matrix: layer codes → XYZ (with the white-balance gain of the cached calibration folded in
    /// and the neutral point's scaling taken over); mixed with the daylight matrix at high ISO
    pub m: Mat,
    /// `m` before the mixing with the daylight matrix, and the daylight matrix itself
    pub m_single: Mat,
    pub m2: Option<Mat>,
    /// matrix handed to the colour step: `m` times the digital ISO factor (cached calibration) or
    /// the RIMM matrix of `m` (plain path); mixed with the daylight variant at high ISO
    pub camera: Mat,
    /// RIMM matrix of the daylight reference and its mixing weight (only at high ISO with automatic
    /// or incandescent white balance)
    pub alt: Option<(Mat, f64)>,
    /// neutral point response, normalised so that its largest component is 1
    pub neutral: [f64; 3],
    /// the matrix is (nearly) singular: monochrome
    pub degenerate: bool,
    /// the matrix before the neutral point's scaling (input of the automatic white balance)
    pub m_raw: Mat,
}

fn color_space(adobe: bool) -> Mat {
    if adobe { CS_ADOBE } else { CS_SRGB }
}

/// Cached calibration (DP1, DP2, SD15 family): the camera stores a correction matrix and a gain
/// triple per white balance. Returns `None` when a part is missing (then the plain path runs).
fn cached(d: &Data, wb: &WhiteBalance, adobe: bool, m2_needed: bool) -> Option<(Mat, Option<Mat>, [f64; 3])> {
    if d.integer("CCMatrix_Flag") != 1 {
        return None;
    }
    let corr = d.list("DP1_WhiteBalanceColorCorrections").or_else(|| d.list("WhiteBalanceColorCorrections"))?;
    let gains = d.list("DP1_WhiteBalanceGains").or_else(|| d.list("WhiteBalanceGains"))?;
    let mut name = wb.name.to_string();
    let mut corr_name = entry(corr, &name);
    if corr_name.is_empty() {
        name = match wb.code {
            2 => "Sunlight",
            4 => "Cloudy",
            5 => "Tungsten",
            7 => "Strobe",
            _ => wb.name,
        }
        .to_string();
        corr_name = entry(corr, &name);
    }
    let gain_name = entry(gains, &name);
    let m0 = d.matrix(&corr_name);
    let gain = d.triple(&gain_name, [0.0; 3]);
    if m0[0] == 0.0 || gain[0] == 0.0 {
        return None;
    }
    let inv_cs = inv(&color_space(adobe));
    let m = mm(&inv_cs, &m0);
    let m2 = if m2_needed {
        let mut n2 = entry(corr, "Daylight");
        if n2.is_empty() {
            n2 = entry(corr, "Sunlight");
        }
        Some(mm(&inv_cs, &d.matrix(&n2)))
    } else {
        None
    };
    Some((mm(&m, &diag(&gain)), m2, gain))
}

/// Plain path (SD9, SD10, SD14, and cameras whose cached calibration is incomplete): the matrix
/// comes from the camera-to-XYZ entries.
fn simple(d: &Data, wb: &WhiteBalance, class: Camera) -> Mat {
    let mut name = wb.name.to_string();
    if matches!(class, Camera::Sd9 | Camera::Sd10 | Camera::Sd14) && name == "Daylight" {
        name = "Sunlight".into();
    }
    if let Some(light) = d.list("WhiteBalanceIlluminants") {
        let ml = entry(light, &name);
        if ml.is_empty() {
            let m = d.matrix("CamToXYZ_Flash");
            if wb.code == 9 {
                return monochrome_matrix(&m);
            }
            return m;
        }
        let mut m = d.matrix(&ml);
        if let Some(k) = d.list("WhiteBalanceCorrections") {
            let kn = entry(k, &name);
            if !kn.is_empty() {
                m = mm(&d.matrix(&kn), &m);
            }
        }
        m
    } else {
        let m = d.matrix("CamToXYZ");
        if m[0] == 0.0 { d.matrix("CamToXYZ_Flash") } else { m }
    }
}

/// Monochrome white balance: every row of the matrix becomes `s · (1, 2, 1)`, with `s` a ninth of
/// the summed response to the white point through `m` (white point scaled so that its largest
/// layer response is 1). The matrix is singular, which switches the chain to its monochrome way.
fn monochrome_matrix(m: &Mat) -> Mat {
    let x = mv(&inv(m), &D65);
    let mx = if x[0] <= x[1].max(x[2]) { if x[1] <= x[2] { x[2] } else { x[1] } } else { x[0] };
    let x = [x[0] / mx, x[1] / mx, x[2] / mx];
    let v = mv(m, &x);
    let s = ((v[0] + v[1]) + v[2]) / 9.0;
    [s, s + s, s, s, s + s, s, s, s + s, s]
}

/// Matrix and neutral point for the chosen white balance.
///
/// `auto_neutral`: the neutral point estimated from the image (automatic white balance of cameras
/// without cached calibration); `None` uses the stored neutral point or the matrix.
/// Colour factor from the dark drift the camera stored (`RawDrift`): the deviation of each layer's
/// dark level from the mean of the other two, times a dark gain (default 1.25, scaled by the
/// exposure time with a camera default; not in long exposure mode), over 4096, plus one. Limited to
/// 0.98 … 1.01 (third layer 1.04) except in long exposure mode, where the dark level is corrected
/// for the accumulated offset of the stacked frames instead. Returns the factor and, in long
/// exposure mode, the three square terms of the post polynomial that replace the stored ones.
pub fn drift_color_factor(d: &Data, class: Camera, iso: &Iso, exposure_us: i32) -> ([f64; 3], Option<[f32; 3]>) {
    let p = d.values("RawDrift").filter(|p| p.len() >= 12).map(|p| p.iter().map(|v| *v as f32 as f64).collect::<Vec<_>>()).unwrap_or_else(|| vec![0.0; 12]);
    // file order: slope and level per layer, top then bottom
    let mut top_mean = [p[1], p[3], p[5]];
    let mut bottom_mean = [p[7], p[9], p[11]];
    let bottom_slope = [p[6], p[8], p[10]];
    if d.integer("UsePreComputedDrift") == 0 {
        for c in 0..3 {
            bottom_mean[c] += bottom_slope[c] / 2.0;
        }
        top_mean = bottom_mean;
    }
    let mut mean: [f64; 3] = std::array::from_fn(|c| (top_mean[c] + bottom_mean[c]) / 2.0);
    let n = iso.bulb_images as i32;
    if iso.bulb {
        let int3 = |name: &str| d.values(name).filter(|v| v.len() >= 3).map_or([0i32; 3], |v| [v[0] as i32, v[1] as i32, v[2] as i32]);
        let (acc, acc_n) = (int3("BulbAccumOffset"), int3("BulbAccumOffsetN"));
        for c in 0..3 {
            let k = (acc_n[c].wrapping_sub(0xfff)).wrapping_mul(n - 1).wrapping_sub(acc[c].wrapping_sub(0xfff));
            mean[c] = (mean[c] - f64::from(k)) / f64::from(n);
        }
    }
    let [r, g, b] = mean;
    let dev = [r - (g + b) / 2.0, g - (r + b) / 2.0, b - (r + g) / 2.0];
    let mut gain = d.triple("DarkGainFactor", [0.0; 3]);
    if gain == [0.0; 3] {
        gain = [1.25; 3];
    }
    if !iso.bulb {
        let standard = match class {
            Camera::Sd9 => [1.0 / 4_000_000.0; 3],
            _ => [0.0; 3],
        };
        let scale = d.triple("DarkGainScale", standard);
        for c in 0..3 {
            gain[c] *= scale[c] * f64::from(exposure_us);
        }
    }
    let normal = d.triple("DarkNormalOffset", [0.0; 3]);
    let mut f: [f64; 3] = std::array::from_fn(|c| (dev[c] - normal[c]) * gain[c] / 4096.0 + 1.0);
    if !iso.bulb {
        for v in f.iter_mut() {
            if !(0.98 <= *v) {
                *v = 0.98;
            }
        }
        for (c, limit) in [1.01, 1.01, 1.04].into_iter().enumerate() {
            if !(f[c] <= limit) {
                f[c] = limit;
            }
        }
        return (f, None);
    }
    // long exposure: square terms of the post polynomial from the factor and the curvature
    let mut offset = f64::from(d.number("BulbCurvatureOffset"));
    let mut curvature = d.triple("BulbCurvatureFactor", [0.0; 3]);
    if offset == 0.0 {
        offset = 1.0;
        curvature = [1.0; 3];
    }
    let divisor = f64::from(n) - offset;
    let q: [f32; 3] = std::array::from_fn(|c| ((f[c] * curvature[c]) / divisor) as f32);
    (f, Some(q))
}

/// Exposure time in microseconds (header value, else the camera data).
pub fn exposure_us(d: &Data, props: &x3f::PropList, class: Camera) -> i32 {
    if let Some(t) = props.get("EXPTIME").and_then(|v| v.trim().parse::<i64>().ok()) {
        return t as i32;
    }
    let t = f64::from(d.number("CaptureShutter"));
    if matches!(class, Camera::Sd9 | Camera::Sd10) { (t * 1_000_000.0) as i32 } else { t as i32 }
}

pub fn color(d: &Data, class: Camera, wb: &WhiteBalance, adobe: bool, iso: &Iso, skip: u32, auto_neutral: Option<[f64; 3]>, drift_factor: [f64; 3]) -> Color {
    let high_auto = iso.high && (wb.code == 1 || wb.code == 5);
    let mut zw = cached(d, wb, adobe, high_auto);
    let mut x = [0.0; 3];
    let (mut m, m2, between);
    if let Some((mm_, m2_, gain)) = zw.take() {
        m = mm_;
        m2 = m2_;
        between = true;
        // neutral point: response of the white point through the inverse of the matrix
        x = mv(&inv(&m), &D65);
        let _ = gain;
    } else {
        between = false;
        m2 = None;
        m = simple(d, wb, class);
    }
    if skip & 0x40000 != 0 {
        m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    }
    let degenerate = det(&m).abs() < 0.0001 || wb.code == 9;
    // SD9 / SD10: mix a little of the first two layers into the third and rescale the rows so that
    // the white point stays
    let mix = match class {
        Camera::Sd9 => 0.06,
        Camera::Sd10 => 0.02,
        _ => 0.0,
    };
    if !between && !(det(&m).abs() < 0.0001) && mix != 0.0 {
        let a: Mat = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, mix, mix, 1.0 - (mix + mix)];
        let white = mv(&inv(&m), &D65);
        m = mm(&a, &m);
        let target = mv(&m, &white);
        let scale = [D65[0] / target[0], D65[1] / target[1], D65[2] / target[2]];
        m = mm(&diag(&scale), &m);
    }
    m = mm(&m, &diag(&drift_factor));
    if !between {
        // neutral point of the plain path
        let from_matrix = |degenerate: bool, m: &Mat| if degenerate { [1.0; 3] } else { mv(&inv(m), &D65) };
        let stored = |n: &str| d.triple(n, [0.0; 3]);
        if d.list("WhiteBalanceIlluminants").is_some() {
            match wb.code {
                1 => x = stored("AutoRGBNeutral"),
                8 => x = stored("CustomRGBNeutral"),
                9 => x = [1.0; 3],
                _ => {}
            }
            if !(x[1] > 0.0) {
                x = from_matrix(degenerate, &m);
            }
        } else {
            x = stored("RGBNeutral");
            if !(x[0] > 0.0 && x[1] > 0.0 && x[2] > 0.0) {
                x = from_matrix(degenerate, &m);
            }
        }
        if wb.code == 1 && skip & 0x2000000 == 0 {
            if let Some(a) = auto_neutral.filter(|a| a.iter().all(|v| v.is_finite() && *v != 0.0)) {
                x = a;
            }
        }
    }
    let mx = max3(&x);
    let neutral = [x[0] / mx, x[1] / mx, x[2] / mx];
    // the matrix takes over the neutral point's scaling (the neutral point itself becomes 1)
    let m_raw = m;
    let mut m = mm(&m, &diag(&neutral));
    let m_single = m;
    let digital = f64::from(iso.digital);
    let one = [1.0; 3];
    let mut camera = if between { m.map(|v| v * digital) } else { cam2rimm(&m, &one, digital, 0.0) };
    let mut alt = None;
    if let (true, Some(m2)) = (high_auto, m2.as_ref().filter(|_| true)) {
        let w = if d.present("MatrixMixing") { f64::from(d.number("MatrixMixing")) } else { 0.5 };
        let a = cam2rimm(m2, &one, digital, 0.0);
        for i in 0..9 {
            camera[i] = w * camera[i] + (1.0 - w) * a[i];
            m[i] = w * m[i] + (1.0 - w) * m2[i];
        }
        alt = Some((a, w));
    }
    Color { cached: between, m, m_single, m2, camera, alt, neutral, degenerate, m_raw }
}

// ---- spatial gain table choice and layer gain vector ----

const LIGHT: [&str; 6] = ["Sunlight", "Shade", "Overcast", "Incandescent", "Fluorescent", "Flash"];

/// Result of the preset comparison for automatic / custom white balance of the cached cameras.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Preset {
    /// preset whose correction matrix is closest to the file's matrix (decides the shading
    /// compensation); `None` when no comparison ran
    pub after_matrix: Option<usize>,
    /// preset whose matrix times gain is closest (decides the spatial gain table)
    pub after_gain: Option<usize>,
}

fn sum_squares(a: &Mat, b: &Mat) -> f64 {
    let mut s = 0.0;
    for i in 0..9 {
        let e = a[i] - b[i];
        s = if i == 0 { e * e } else { s + e * e };
    }
    s
}

/// Which of the six light-source presets is closest to the automatic (or custom) white balance
/// stored in the file. Runs for the cached calibration with automatic or custom white balance.
pub fn preset_choose(d: &Data, class: Camera, wb: &WhiteBalance, skip: u32, cached: bool) -> Preset {
    if skip & 0x2000000 != 0 || !cached || !(wb.code == 1 || wb.code == 8) {
        return Preset::default();
    }
    let (cc1, cc2, g1, g2) = if wb.code == 1 {
        ("DP1_AutoCCMatrix", "AutoCCMatrix", "DP1_AutoWBGain", "AutoWBGain")
    } else {
        ("DP1_CustomCCMatrix", "CustomCCMatrix", "DP1_CustomWBGain", "CustomWBGain")
    };
    let mut gain_auto = d.triple(g1, [0.0; 3]);
    let mut m_auto = d.matrix(cc1);
    if m_auto[0] == 0.0 {
        m_auto = d.matrix(cc2);
        gain_auto = d.triple(g2, [0.0; 3]);
    }
    let a = mm(&m_auto, &diag(&gain_auto));
    let (mut best_m, mut best_g) = (100000.0f64, 100000.0f64);
    let (mut idx_m, mut idx_g) = (None, None);
    for (i, l) in LIGHT.iter().enumerate() {
        let mut gi = d.triple(&format!("DP1_{l}WBGain"), [0.0; 3]);
        let mut mi = d.matrix(&format!("DP1_{l}CCMatrix"));
        if mi[0] == 0.0 {
            mi = d.matrix(&format!("{l}CCMatrix"));
            gi = d.triple(&format!("{l}WBGain"), [0.0; 3]);
        }
        let d1 = sum_squares(&mi, &m_auto);
        if d1 < best_m {
            best_m = d1;
            idx_m = Some(i);
        }
        let b = mm(&mi, &diag(&gi));
        let d2 = sum_squares(&b, &a);
        if d2 < best_g {
            best_g = d2;
            idx_g = Some(i);
        }
    }
    if idx_m == Some(1) && class == Camera::Dp1 {
        idx_m = Some(2);
    }
    Preset { after_matrix: idx_m, after_gain: idx_g }
}

/// Shading compensation of the DP1 family by preset (three layer factors).
pub fn shading(d: &Data, class: Camera, v: &Preset) -> [f64; 3] {
    if class != Camera::Dp1 {
        return [1.0; 3];
    }
    let (name, std): (&str, [f64; 3]) = match v.after_matrix {
        Some(0) => ("ShadingCompensation_Sunlight", [0.98437, 1.0, 1.00676]),
        Some(1) => ("ShadingCompensation_Shade", [0.99381, 1.0, 0.9893]),
        Some(2) => ("ShadingCompensation_Overcast", [0.98253, 1.0, 0.98639]),
        Some(3) => ("ShadingCompensation_Incandescent", [0.985627, 1.0, 0.995364]),
        Some(4) => ("ShadingCompensation_Fluorescent", [1.00494, 1.0, 0.98753]),
        Some(5) => ("ShadingCompensation_Flash", [1.0, 1.0, 1.0]),
        _ => return [1.0; 3],
    };
    d.triple(name, std)
}

/// Name of the spatial gain table: the preset closest to the file's gains when that comparison
/// ran, else the camera's `SpatialGainTables` entry for the white balance (incandescent has a
/// built-in name), else the plain table. The caller falls back to `SpatialGain` when the named
/// table does not exist.
pub fn grid_name(d: &Data, wb: &WhiteBalance, v: &Preset) -> String {
    let mut name = "SpatialGain".to_string();
    if let Some(l) = d.list("SpatialGainTables") {
        name = entry(l, wb.name);
        if name.is_empty() && wb.code == 2 {
            // the lists of the table names are keyed "Sunlight" where the request says "Daylight"
            // (measured against the exports: the sunlight table applies)
            name = entry(l, "Sunlight");
        }
        if name.is_empty() && wb.name == "Incandescent" {
            name = "SpatialGain_Incandescent".into();
        }
    }
    match v.after_gain {
        Some(i) if i < 6 => format!("SpatialGain_{}", LIGHT[i]),
        _ => name,
    }
}

/// The layer gain vector and the quantities derived with it.
#[derive(Clone, Debug)]
pub struct Amplification {
    /// gain from the neutral point alone (smallest component 1)
    pub relative: [f64; 3],
    /// gain with aperture, shading and temperature factors
    pub gain: [f64; 3],
    /// gain times the (limited) ISO factor
    pub permanent: [f64; 3],
}

/// Layer gain vector: reciprocal of the normalised neutral point, times the camera's aperture
/// factor, the shading compensation and the sensor-temperature factor. `gray_mean`: mean of the
/// three layers (only needed for incandescent white balance at high ISO, where the gain is mixed
/// with the grey-world estimate).
pub fn amplification(d: &Data, f: &Color, iso: &Iso, wb: &WhiteBalance, shadows: [f64; 3], gray_mean: Option<[f64; 3]>) -> Amplification {
    let relative = [1.0 / f.neutral[0], 1.0 / f.neutral[1], 1.0 / f.neutral[2]];
    let mut g = relative;
    let fn_ = d.triple("FnumberGainFact", [1.0; 3]);
    let temp = d.triple("TempGainFact", [1.0; 3]);
    for i in 0..3 {
        g[i] *= fn_[i];
    }
    for i in 0..3 {
        g[i] *= shadows[i];
    }
    for i in 0..3 {
        g[i] *= temp[i];
    }
    if iso.high && wb.code == 5 {
        let w = f64::from(if d.present("WBMixing") { d.number("WBMixing") } else { 0.5 });
        if let Some(m) = gray_mean {
            let target = [m[1] / m[0], 1.0, m[1] / m[2]];
            for i in 0..3 {
                g[i] = g[i] * w + target[i] * (1.0 - w);
            }
        }
    }
    let permanent = [g[0] * f64::from(iso.factor1), g[1] * f64::from(iso.factor1), g[2] * f64::from(iso.factor1)];
    Amplification { relative, gain: g, permanent }
}

/// Parameters of one chroma smoothing variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromaPar {
    /// amplitude of the coring tables (same for the three layers)
    pub amp: [f64; 3],
    /// offset against division by zero in the large variant (twice that in the small one)
    pub spatial: i32,
    /// power of two the summed corrections are divided by
    pub shift: u32,
}

/// Which chroma variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaKind {
    Small,
    Large,
}

/// Parameters of the chroma smoothing, `None` when the variant does not run for this file
/// (no `ChromaBlur`, skipped by flag, or - for the large one - a degenerate matrix).
pub fn chroma_parameter(d: &Data, k: &Calibration, file_kind: ChromaKind) -> Option<ChromaPar> {
    let blur = d.number("ChromaBlur") as i32;
    let shift = if d.present("ChromaSumShift") { d.integer("ChromaSumShift") & 0xffff } else { 3 };
    let is_active = match file_kind {
        ChromaKind::Small => blur != 0 && k.skip & 0x8000 == 0,
        ChromaKind::Large => blur != 0 && !k.color.degenerate && k.skip & 0x10000 == 0,
    };
    if !is_active {
        return None;
    }
    let bulb = k.iso.bulb_factor;
    let mut dq = d.triple("ChromaDQ", [0.0; 3]);
    let mut spatial = d.number("SpatialDQ") as i32;
    if file_kind == ChromaKind::Small && k.color.degenerate {
        dq = dq.map(|v| v * 2.0);
    }
    spatial = (f64::from(spatial) * bulb) as i32;
    let g = k.amplification.gain;
    let factor = f64::from(k.iso.factor1) * k.statistics;
    for c in 0..3 {
        dq[c] *= bulb;
        dq[c] *= g[c];
        dq[c] *= factor;
        if file_kind == ChromaKind::Small {
            dq[c] *= 2.0;
        }
    }
    spatial *= ((k.iso.factor1 * 10.0) as f64 * k.statistics) as i32;
    if file_kind == ChromaKind::Small {
        spatial += spatial;
    }
    let mean = ((dq[0] + dq[1]) + dq[2]) / 3.0;
    Some(ChromaPar { amp: [mean; 3], spatial, shift })
}

/// All results of step 1 together.
#[derive(Clone, Debug)]
pub struct Input<'a> {
    pub camf: &'a Camf,
    pub props: &'a PropList,
    /// white balance name of the request ("Auto", "Sunlight", …)
    pub wb: &'a str,
    /// the camera's colour space is Adobe RGB
    pub adobe: bool,
    /// neutral point estimated from the image (automatic white balance without cached calibration)
    pub auto_neutral: Option<[f64; 3]>,
    /// mean of the layers (incandescent white balance at high ISO)
    pub gray_mean: Option<[f64; 3]>,
}

#[derive(Clone, Debug)]
pub struct Calibration {
    pub model: Model,
    pub wb: WhiteBalance,
    pub iso: Iso,
    pub skip: u32,
    pub color: Color,
    pub preset: Preset,
    pub shading: [f64; 3],
    pub grid: String,
    pub amplification: Amplification,
    /// factor applied to the colour statistics (1.3 with cached calibration)
    pub statistics: f64,
    /// colour factor from the stored dark drift (already folded into the matrix)
    pub drift_factor: [f64; 3],
    /// long exposure mode: the square terms of the post polynomial (first layer R², second G²,
    /// third B²) replaced by the drift factor and the curvature
    pub bulb_squares: Option<[f32; 3]>,
}

impl Calibration {
    /// Parameters of a chroma variant for the camera data this calibration was made from.
    pub fn chroma(&self, camf: &Camf, file_kind: ChromaKind) -> Option<ChromaPar> {
        chroma_parameter(&Data { camf }, self, file_kind)
    }
}

pub fn calc(e: &Input) -> Result<Calibration, String> {
    let d = Data { camf: e.camf };
    let name = e.props.get("CAMMODEL").unwrap_or("").to_string();
    let model = model(&name).ok_or_else(|| format!("Kamera {name:?} ist keine Classic-Kamera"))?;
    let wb = white_balance(e.wb);
    let request_iso = e.props.get("ISO").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(100.0);
    let iso = iso(&d, request_iso);
    let skip = d.integer("SkipStepFlags");
    let (drift_factor, bulb_squares) = drift_color_factor(&d, model.class, &iso, exposure_us(&d, e.props, model.class));
    let color = color(&d, model.class, &wb, e.adobe, &iso, skip, e.auto_neutral, drift_factor);
    let preset = preset_choose(&d, model.class, &wb, skip, color.cached);
    let shadows = shading(&d, model.class, &preset);
    let grid = grid_name(&d, &wb, &preset);
    let v = amplification(&d, &color, &iso, &wb, shadows, e.gray_mean);
    let statistics = if color.cached { 1.3 } else { 1.0 };
    Ok(Calibration { model, wb, iso, skip, color, preset, shading: shadows, grid, amplification: v, statistics, drift_factor, bulb_squares })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_inverse_and_determinant() {
        let m: Mat = [2.0, 0.5, -1.0, 0.25, 3.0, 0.75, -0.5, 1.0, 4.0];
        let p = mm(&m, &inv(&m));
        for i in 0..9 {
            let target = if i % 4 == 0 { 1.0 } else { 0.0 };
            assert!((p[i] - target).abs() < 1e-12, "{i}: {}", p[i]);
        }
        assert!((det(&diag(&[2.0, 3.0, 4.0])) - 24.0).abs() < 1e-12);
    }

    #[test]
    fn rimm_matrix_is_row_scaled_projection() {
        // with a neutral response of equal channels the scaled rows map the neutral to equal values
        let m: Mat = [0.9, 0.1, 0.0, 0.05, 0.9, 0.05, 0.0, 0.1, 0.9];
        let q = cam2rimm(&m, &[1.0; 3], 1.0, 0.0);
        let z = mv(&q, &[1.0; 3]);
        assert!((z[0] - z[1]).abs() < 1e-9 && (z[1] - z[2]).abs() < 1e-9, "{z:?}");
        // the digital factor scales everything
        let q2 = cam2rimm(&m, &[1.0; 3], 2.0, 0.0);
        for i in 0..9 {
            assert!((q2[i] - 2.0 * q[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn white_balance_names() {
        assert_eq!(white_balance("Sunlight"), WhiteBalance { code: 2, name: "Daylight" });
        assert_eq!(white_balance("auto").code, 1);
        assert_eq!(white_balance("Incandescent").code, 5);
        assert_eq!(model("SIGMA DP2x").map(|m| m.class), Some(Camera::Dp2));
        assert!(model("SIGMA DP2").unwrap().is_dp2 && !model("SIGMA DP2x").unwrap().is_dp2);
    }

    /// Amplitude of the small chroma variant, measured earlier by fitting against the former chain's
    /// intermediate results (tolerance of the fit about 0.01 to 0.1).
    #[test]
    fn chroma_amplitudes_match_the_fits() {
        let cases: [(&str, f64, f64); 5] = [
            ("pb_sigma_dp1_pb_01", 27.0724, 0.02),
            ("pb_sigma_dp1s_01", 13.89, 0.02),
            ("pb_sigma_dp2_01", 29.24, 0.02),
            ("sd14_pixls", 21.27, 0.02),
            ("sd10_pixls", 57.91, 0.1),
        ];
        let all_items = crate::classic_new::check::classic_files();
        for (name, target, tol) in cases {
            let Some(f) = all_items.iter().find(|p| p.to_string_lossy().contains(name)) else { continue };
            let x = x3f::X3fFile::open(f).unwrap();
            let (camf, props) = (x.camf().unwrap(), x.properties().unwrap());
            let adobe = x.exif().and_then(|e| e.color_space) == Some(0xFFFF);
            // SD10: the fit used the matrix white as neutral point; use the same for the check
            let wb = if name.starts_with("sd10") { "Sunlight" } else { props.get("WB_DESC").unwrap_or("Auto").trim() };
            let k = calc(&Input { camf: &camf, props: &props, wb, adobe, auto_neutral: None, gray_mean: None }).unwrap();
            let p = k.chroma(&camf, ChromaKind::Small).unwrap();
            let actual = p.amp[0];
            eprintln!("{name}: small {actual:.4} (fit {target})");
            if !name.starts_with("sd10") {
                assert!((actual - target).abs() < tol, "{name}: {actual} instead of {target}");
            }
            assert_eq!(p.amp[0] / 2.0, k.chroma(&camf, ChromaKind::Large).unwrap().amp[0]);
        }
    }

    /// Prints the new calibration for one file as one line (for comparisons)
    /// (`GRAINEO_CLASSIC_CALIB_FILE=<x3f>`, optional `GRAINEO_CLASSIC_CALIB_WB`); used by the comparison script.
    #[test]
    #[ignore]
    fn output() {
        let Ok(f) = std::env::var("GRAINEO_CLASSIC_CALIB_FILE") else { return };
        let x = x3f::X3fFile::open(&f).unwrap();
        let (camf, props) = (x.camf().unwrap(), x.properties().unwrap());
        let wb = std::env::var("GRAINEO_CLASSIC_CALIB_WB").unwrap_or_else(|_| props.get("WB_DESC").unwrap_or("Auto").trim().to_string());
        let adobe = x.exif().and_then(|e| e.color_space) == Some(0xFFFF);
        let k = calc(&Input { camf: &camf, props: &props, wb: &wb, adobe, auto_neutral: None, gray_mean: None }).unwrap();
        for a in [ChromaKind::Small, ChromaKind::Large] {
            println!("CHROMA {a:?} {:?}", k.chroma(&camf, a));
        }
        println!(
            "NEU einzeln={:?} m2={:?} camera={:?} final_gain={:?} relative_gain={:?} ratio={} digital_gain={} stat={} alt={:?} kamera={:?} vorgabe={:?} gitter={} neutral={:?}",
            k.color.m_single, k.color.m2, k.color.m, k.amplification.permanent, k.amplification.relative, k.iso.factor, k.iso.digital, k.statistics, k.color.alt, k.color.camera, k.preset, k.grid, k.color.neutral
        );
    }
}
