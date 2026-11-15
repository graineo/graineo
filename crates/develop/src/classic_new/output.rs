//! Classic chain, output step: from the linear working codes of the chain (after the
//! detail step) to the 16-bit export in sRGB (or Adobe RGB), matching Sigma's TIFF exports.
//!
//! Per pixel and layer (the working space has the primaries of ROMM RGB, linear, 4096 = 1.0):
//! 1. The linear code goes through the cube-root table of the detail step; the cube-root code
//!    indexes a **tone table** per layer. The tone table holds the four tone sliders
//!    (exposure, contrast, shadow, highlight; the colour mode adds its contrast compensation) and
//!    the colour adjustment (R/G/B gains) and returns a linear value (4096 = 1.0, at most 4.0):
//!    - exposure and the gains are slopes before the curve (gains normalised by the mean of their
//!      weighted arithmetic and harmonic mean 0.3/0.5/0.2),
//!    - the shadow slider sets a black offset,
//!    - above a threshold the curve is a power function (contrast as exponent around 18 % grey)
//!      with a soft shoulder whose sharpness the highlight slider sets,
//!    - below the threshold a straight line in the display-encoded domain (sRGB-type encoding)
//!      that meets the power section in value,
//!    - the table is evaluated every 32 codes and interpolated linearly in between.
//!
//!    The gains of the colour adjustment are first scaled so that the weighted mean
//!    (0.3/0.55/0.15) of the image mean stays the same.
//! 2. A 3 × 3 **saturation matrix** (12-bit fixed point): diagonal `1 − 2a`, elsewhere `a`,
//!    `a = −saturation / 6`.
//! 3. A **gamma table** (65536 entries, index = linear value, 4096 = 1.0) to the encoding of ROMM
//!    RGB with gamma 1/1.8 and a straight toe (slope 9, offset 0.01367; negative values continue
//!    the straight line), with a highlight shoulder above 0.4 of the encoded range
//!    (`1.5 y / ⁴√(y⁴ + 5.0625)`), sampled at growing steps and interpolated.
//! 4. A **colour management** step from ROMM RGB (gamma 1.8 as stored in its profile,
//!    D50 primaries) to the output space: sRGB (the standard curve sampled at 1024 points) or
//!    Adobe RGB (gamma 563/256), relative colorimetric, 16 bit.
//!
//! Published standards in this step: the ROMM RGB primaries, its D50 white and the gamma 1/1.8
//! encoding are ISO 22028-2 (ROMM RGB / RIMM RGB; the colorants and the gamma 461/256 are those of
//! the standard's ICC profile), the sRGB curve is IEC 61966-2-1, the Adobe RGB gamma 563/256 is
//! Adobe RGB (1998). The tone table, the toe (slope 9, offset 0.01367), the shoulder and the
//! saturation matrix are matched to the exports.
//!
//! Sharpness acts earlier, in the detail step: the high-pass is added with the amount
//! `1 + sharpness` (see [`super::detail::with_sharpness`]). Fill Light needs a local gain image of the
//! chain that is not built yet; the slider is accepted but has no effect (only 0 is exact).

use std::sync::OnceLock;

use super::detail;
use super::calibration::Data;

/// Sliders of the output in Sigma's units (neutral: 0, colour adjustment `[1; 3]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slider {
    pub exposure: f64,
    pub contrast: f64,
    pub shadows: f64,
    pub highlights: f64,
    pub saturation: f64,
    /// acts in the detail step ([`super::detail::with_sharpness`]), not here
    pub sharpness: f64,
    /// not built yet (no effect)
    pub fill_light: f64,
    pub color_balance: [f64; 3],
}

impl Default for Slider {
    fn default() -> Self {
        Slider { exposure: 0.0, contrast: 0.0, shadows: 0.0, highlights: 0.0, saturation: 0.0, sharpness: 0.0, fill_light: 0.0, color_balance: [1.0; 3] }
    }
}

impl Slider {
    /// Sliders stored in the file (colour mode values; missing values neutral).
    pub fn from_file(a: &x3f::sppa::SppAdjustments) -> Self {
        let z = |v: Option<f32>| v.map_or(0.0, f64::from);
        let t = &a.color;
        Slider {
            exposure: z(t.exposure),
            contrast: z(t.contrast),
            shadows: z(t.shadow),
            highlights: z(t.highlight),
            saturation: z(a.saturation),
            sharpness: z(t.sharpness),
            fill_light: z(t.fill_light),
            color_balance: a.color_adjust.map_or([1.0; 3], |c| c.map(f64::from)),
        }
    }
}

/// Variants of the tone table that depend on the request rather than on the image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// The newer form of the tone table: exposure +0.6, contrast +0.4, shadow −0.2, highlight
    /// +0.2 on top of the sliders (off for the colour mode "Neutral"). With the older form only
    /// the highlight slider gets +0.8 at ISO 50 and below.
    pub new_form: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { new_form: false }
    }
}

impl Options {
    /// The form for a camera: the newer form of the tone table belongs to SD15 and DP1x.
    /// Measured against the exports: SD15 newer form, DP1/DP1s/DP2/DP2x,
    /// SD10 and SD14 older form.
    pub fn for_camera(props: &x3f::PropList) -> Self {
        let model = props.get("CAMMODEL").unwrap_or("").trim().to_ascii_uppercase();
        let model = model.strip_prefix("SIGMA").unwrap_or(&model).trim().to_string();
        Options { new_form: matches!(model.as_str(), "SD15" | "DP1X") }
    }
}

/// The finished output step for one image.
pub struct Output {
    tone: [Vec<u16>; 3],
    matrix: [i32; 9],
    gamma: &'static [u16],
    space: &'static ColorSpace,
    /// colour adjustment after the normalisation to the image mean (as used in the tone table)
    pub amplification: [f64; 3],
    /// contrast compensation of the colour mode
    pub contrast_mode: f64,
}

/// The part of the output step that does not depend on the sliders: made once per image and colour
/// mode (image mean of the codes, contrast compensation of the mode, form of the tone table, ISO);
/// [`OutputBasis::output`] builds the output for a slider state and keeps the last one.
pub struct OutputBasis {
    mean: [f64; 3],
    contrast_mode: f64,
    low_iso: bool,
    new_form: bool,
    most_recent: std::sync::Mutex<Option<(Slider, bool, std::sync::Arc<Output>)>>,
}

impl std::fmt::Debug for OutputBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AusgabeBasis").field("mittel", &self.mean).field("kontrast_modus", &self.contrast_mode).field("niedrig_iso", &self.low_iso).field("neue_form", &self.new_form).finish()
    }
}

impl OutputBasis {
    /// `codes`: the linear working codes of the chain (whole image); their mean over the active
    /// area normalises the colour adjustment. `mode`: colour mode (contrast compensation; the
    /// mode "Neutral" always uses the older form of the tone table).
    pub fn new(camf: &x3f::Camf, props: &x3f::PropList, mode: &str, codes: &crate::classic_new::Codes, opt: Options) -> Self {
        let d = Data { camf };
        let iso = props.get("ISO").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(100.0);
        OutputBasis {
            mean: image_mean(&d, codes),
            contrast_mode: contrast_compensation(&d, mode),
            low_iso: iso <= 50.0,
            new_form: opt.new_form && !mode.trim().eq_ignore_ascii_case("Neutral"),
            most_recent: std::sync::Mutex::new(None),
        }
    }

    /// The output for a slider state (`adobe`: Adobe RGB instead of sRGB); the last one is cached.
    pub fn output(&self, slider: &Slider, adobe: bool) -> std::sync::Arc<Output> {
        if let Ok(z) = self.most_recent.lock() {
            if let Some((r, a, outp)) = z.as_ref() {
                if r == slider && *a == adobe {
                    return outp.clone();
                }
            }
        }
        let outp = std::sync::Arc::new(self.build(slider, adobe));
        if let Ok(mut z) = self.most_recent.lock() {
            *z = Some((*slider, adobe, outp.clone()));
        }
        outp
    }

    fn build(&self, slider: &Slider, adobe: bool) -> Output {
        let mut g = slider.color_balance;
        if g != [1.0; 3] {
            g = gray_balance(g, self.mean);
        }
        let tone = tone_curve(&Tone { exposure: slider.exposure, shadows: slider.shadows, contrast: slider.contrast + self.contrast_mode, highlights: slider.highlights, color: g }, self.low_iso, self.new_form);
        Output { tone, matrix: saturation_matrix(slider.saturation), gamma: gamma_table(), space: if adobe { adobe_rgb() } else { srgb() }, amplification: g, contrast_mode: self.contrast_mode }
    }
}

impl Output {
    /// `codes`: the linear working codes of the chain (whole image); their mean over the active
    /// area normalises the colour adjustment. `mode`: colour mode (contrast compensation),
    /// `adobe`: output in Adobe RGB instead of sRGB.
    pub fn new(camf: &x3f::Camf, props: &x3f::PropList, wb: &str, mode: &str, adobe: bool, slider: &Slider, codes: &crate::classic_new::Codes) -> Result<Self, String> {
        Self::with_options(camf, props, wb, mode, adobe, slider, codes, Options::for_camera(props))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_options(camf: &x3f::Camf, props: &x3f::PropList, _wb: &str, mode: &str, adobe: bool, slider: &Slider, codes: &crate::classic_new::Codes, opt: Options) -> Result<Self, String> {
        Ok(OutputBasis::new(camf, props, mode, codes, opt).build(slider, adobe))
    }

    /// One pixel with another sharpness than the chain ran with. `lin` are the chain's linear codes
    /// as fractional values that also carry the exact index into the inverse table
    /// ([`code_with_position`]); `hp` is the high-pass the detail step added with amount 1.0 (see
    /// [`super::detail::with_high_pass`]), `magnitude` the amount of the slider
    /// ([`super::detail::magnitude`]). The difference of the amounts is added to that index and the
    /// codes go back to linear, as the detail step would have computed them.
    #[inline]
    pub fn pixel_with_sharpness(&self, lin: [f32; 3], hp: i16, magnitude: i32) -> [u16; 3] {
        let h = i32::from(hp);
        let d = (h.wrapping_mul(magnitude) >> 14) - h;
        let rounded = lin.map(|v| v.round() as i32);
        if d == 0 {
            return self.pixel(rounded);
        }
        let ex = detail::exponent();
        let new = std::array::from_fn(|c| i32::from(ex[((index_from_position(lin[c]) + d) as u32 & 0xffff) as usize]));
        self.pixel(new)
    }

    /// One pixel: linear codes of the chain → 16-bit output.
    #[inline]
    pub fn pixel(&self, lin: [i32; 3]) -> [u16; 3] {
        let wz = detail::root();
        let t: [i32; 3] = std::array::from_fn(|c| {
            let l = lin[c].min(0x7fff);
            let w = wz[(l as u32 & 0xffff) as usize];
            i32::from(self.tone[c][w as u16 as usize])
        });
        let m = &self.matrix;
        let row = |r: usize| m[r * 3 + 2].wrapping_mul(t[2]).wrapping_add(m[r * 3 + 1].wrapping_mul(t[1])).wrapping_add(m[r * 3].wrapping_mul(t[0])).wrapping_add(0x800) >> 12;
        let e = [0, 1, 2].map(|r| self.gamma[(row(r) as u32 & 0xffff) as usize]);
        self.space.convert(e)
    }
}

// ---------------------------------------------------------------------------------------------
// exact index behind a linear code

/// Runs of equal values of the inverse table over the indices 0 … 0x7fff: for each linear code
/// (offset by [`RUN_OFFSET`]) the first index and the number of indices giving it.
const RUN_OFFSET: i32 = 128;
/// Longer runs (only the clipped top code) carry no position: single precision would not resolve it.
const MAX_RUN: u16 = 64;

fn runs() -> &'static [(u16, u16)] {
    static T: OnceLock<Vec<(u16, u16)>> = OnceLock::new();
    T.get_or_init(|| {
        let ex = detail::exponent();
        let mut t = vec![(0u16, 0u16); (0x7fff + RUN_OFFSET + 1) as usize];
        for i in 0..0x8000usize {
            let v = (i32::from(ex[i]) + RUN_OFFSET) as usize;
            if t[v].1 == 0 {
                t[v] = (i as u16, 1);
            } else if t[v].0 as usize + t[v].1 as usize == i {
                t[v].1 += 1;
            }
        }
        t
    })
}

/// The linear code of an index of the inverse table as a fractional value that also tells which
/// of the indices giving that code it was: code + (position + ½)/count − ½. Rounded it is the
/// code itself; [`index_from_position`] gets the index back (negative indices: the plain code).
pub fn code_with_position(index: i16) -> f32 {
    let ex = detail::exponent();
    let code = i32::from(ex[index as u16 as usize]);
    if index < 0 {
        return code as f32;
    }
    let (start, n) = runs()[(code + RUN_OFFSET) as usize];
    let pos = i32::from(index) - i32::from(start);
    if n <= 1 || n > MAX_RUN || pos < 0 || pos >= i32::from(n) {
        return code as f32;
    }
    code as f32 + ((pos as f32 + 0.5) / f32::from(n) - 0.5)
}

/// Inverse of [`code_with_position`]: the index of the inverse table for a (fractional) linear code;
/// without position information the cube-root code of the rounded code.
pub fn index_from_position(lin: f32) -> i32 {
    let code = (lin.round() as i32).min(0x7fff);
    if (-RUN_OFFSET..=0x7fff).contains(&code) {
        let (start, n) = runs()[(code + RUN_OFFSET) as usize];
        if n > 1 && n <= MAX_RUN {
            let pos = (((lin - code as f32) + 0.5) * f32::from(n)).floor().clamp(0.0, f32::from(n) - 1.0) as i32;
            return i32::from(start) + pos;
        }
        if n == 1 {
            return i32::from(start);
        }
    }
    i32::from(detail::root()[(code as u32 & 0xffff) as usize])
}

// ---------------------------------------------------------------------------------------------
// colour mode, image mean and colour adjustment

/// Contrast compensation of the colour mode: a list in the camera data names, for each mode, the
/// entry holding the value (0 when missing).
pub fn contrast_compensation(d: &Data, mode: &str) -> f64 {
    let Ok(list) = d.camf.properties("ColorModeContrastCompensations") else { return 0.0 };
    match list.iter().find(|(k, _)| k.trim().eq_ignore_ascii_case(mode.trim())) {
        Some((_, name)) => f64::from(d.number(name.trim())),
        None => 0.0,
    }
}

/// Mean of the three layers over the active area of the camera data (its coordinates taken as
/// they are, without the offset of the kept area; clipped to the image).
pub fn image_mean(d: &Data, codes: &crate::classic_new::Codes) -> [f64; 3] {
    let r = d.values("ActiveImageArea").filter(|v| v.len() >= 4).map(|v| [v[0] as usize, v[1] as usize, v[2] as usize + 1, v[3] as usize + 1]).unwrap_or([0, 0, codes.width, codes.height]);
    let (x0, y0, x1, y1) = (r[0].min(codes.width), r[1].min(codes.height), r[2].min(codes.width), r[3].min(codes.height));
    let mut s = [0.0f64; 3];
    let mut n = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = (y * codes.width + x) * 3;
            for c in 0..3 {
                s[c] += f64::from(codes.data[p + c] as i16);
            }
            n += 1;
        }
    }
    if n == 0 {
        return [1.0; 3];
    }
    s.map(|v| v / n as f64)
}

/// Scales the colour adjustment so that the weighted mean (0.3/0.55/0.15) of the image mean stays
/// the same (single precision for the ratio).
pub fn gray_balance(g: [f64; 3], m: [f64; 3]) -> [f64; 3] {
    if m.iter().any(|v| !(*v > 0.0)) {
        return g;
    }
    let top = (0.3 * m[0] + 0.55 * m[1] + 0.15 * m[2]) as f32;
    let bottom = (0.3 * g[0] * m[0] + 0.55 * g[1] * m[1] + 0.15 * g[2] * m[2]) as f32;
    let f = f64::from(top / bottom);
    g.map(|v| f * v)
}

// ---------------------------------------------------------------------------------------------
// tone table

/// Inputs of the tone table (slider units; `color` = gains R/G/B).
#[derive(Clone, Copy, Debug)]
pub struct Tone {
    pub exposure: f64,
    pub shadows: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub color: [f64; 3],
}

const ENC_SLOPE: f64 = 12.923054468333259;
const ENC_THRESHOLD: f64 = 0.00304;
const ENC_A: f64 = 0.9478668315473864;
const ENC_B: f64 = 0.05213316845261357;
const ENC_EXP: f64 = 0.4166666666666667;

/// Display decoding of the toe (sRGB-type curve), negative input clipped to 0.
fn decode(v: f64) -> f64 {
    let v = if 0.0 <= v { v } else { 0.0 };
    let d = v / ENC_SLOPE;
    if ENC_THRESHOLD < d { (v * ENC_A + ENC_B).powf(2.4) } else { d }
}

fn encode(v: f64) -> f64 {
    if v <= ENC_THRESHOLD { ENC_SLOPE * v } else { (v.powf(ENC_EXP) + -ENC_B) / ENC_A }
}

/// Tone table per layer: index = cube-root code (as 16-bit value), value = linear output
/// (4096 = 1.0, at most 0x4000); zero for negative cube-root codes. `low_iso`: ISO 50 or
/// less (older form: highlight +0.8), `new_form` see [`Options`].
pub fn tone_curve(t: &Tone, low_iso: bool, new_form: bool) -> [Vec<u16>; 3] {
    let expo = detail::exponent();
    let (mut exp_ev, mut con, mut hl) = (t.exposure, t.contrast, t.highlights);
    let sch;
    if new_form {
        exp_ev += 0.6;
        con += 0.4;
        sch = t.shadows + -0.2;
        hl += 0.2;
    } else {
        if low_iso {
            hl += 0.8;
        }
        sch = t.shadows;
    }
    let con = (con + 2.0) * 0.25 * 1.29125 + 0.64375;
    let hl = (hl + 2.0) * 0.25;
    let [r, g, b] = t.color;
    let norm = (1.0 / ((1.0 / b) * 0.2 + (1.0 / g) * 0.5 + (1.0 / r) * 0.3) + b * 0.2 + g * 0.5 + r * 0.3) * 0.5;
    let (rn, gn) = (r / norm, g / norm);
    let e = exp_ev.exp2();
    let bn = (b / norm) * e;
    let black = {
        let s = (2.0 - sch) * 0.25;
        (s + s + -1.0) * 0.025
    };
    let gain = 0.18f64.powf(1.0 - con);
    let k = ENC_EXP * con;
    let n = 1.07f64.powf((1.5 * hl + 1.0).powf(4.0));
    let s_par = 0.44 * hl + 0.66;
    let inv_n = 1.0 / n;
    // power section with shoulder; `assoc` selects one of two multiplication orders (bit-exact results)
    let power = |z: f64, assoc: bool| -> f64 {
        let y = z.powf(con) * gain;
        let m = if y <= 0.01 { 0.01 } else { y };
        let mm = m.powf(0.22);
        let a = (mm * s_par).powf(n);
        let bb = y.powf(n);
        let r = (1.0 / (bb + a)).powf(inv_n);
        if assoc { r * y * (mm * s_par) } else { r * y * mm * s_par }
    };
    // threshold and slope of the straight toe
    let (threshold, t0, slope) = if 1.0 <= k {
        (0.0, 0.0, 0.0)
    } else {
        let d = 1.0 - k;
        let t0 = 0.06 * d;
        let u = ENC_B + t0;
        let p = 0.18f64.powf((-1.0 + con) * ENC_EXP);
        let threshold = ((p * u) / (d * 1.0521331684526136)).powf(1.0 / k);
        let v = if 0.0 <= threshold { power(threshold, true) } else { decode(threshold * 0.0 + 0.0) };
        (threshold, t0, (encode(v) - t0) / threshold)
    };
    let value = |z: f64| -> f64 { if threshold <= z { power(z, false) } else { decode(z * slope + t0) } };
    let mut curve = [vec![0u16; 65536], vec![0u16; 65536], vec![0u16; 65536]];
    let mut before = [0.0f64; 3];
    let mut from_idx = 0usize;
    let mut last = [0u16; 3];
    let mut i = 0usize;
    while i < 0x2000 {
        let x = f64::from(expo[i]) * 0.000244140625 - black;
        let z = [e * rn * x + -0.005, e * gn * x + -0.005, x * bn + -0.005];
        let now = z.map(value);
        if from_idx < i {
            let nn = (i - (from_idx - 1)) as f64;
            for (a, idx) in (from_idx..i).enumerate() {
                let tt = (a + 1) as f64 / nn;
                let w = 1.0 - tt;
                for c in 0..3 {
                    let v = ((before[c] * w + now[c] * tt) * 4096.0 + 0.5) as i32;
                    curve[c][idx] = (if 0x3fff < v { 0x4000 } else { v }) as u16;
                }
            }
        }
        for c in 0..3 {
            let v = (now[c] * 4096.0 + 0.5) as i32;
            last[c] = (if 0x4000 < v { 0x4000 } else { v }) as u16;
            curve[c][i] = last[c];
        }
        before = now;
        from_idx = i | 1;
        i += 0x20;
    }
    for c in 0..3 {
        for v in &mut curve[c][0x1fe1..0x8000] {
            *v = last[c];
        }
    }
    curve
}

// ---------------------------------------------------------------------------------------------
// saturation, gamma table, colour management

/// Saturation matrix in 12-bit fixed point (rounded by adding 0.5 and truncating).
pub fn saturation_matrix(s: f64) -> [i32; 9] {
    let a = s / -6.0;
    let d = 1.0 - (a + a);
    let m = [d, a, a, a, d, a, a, a, d];
    m.map(|v| (v * 4096.0 + 0.5) as i32)
}

/// Encoding of the gamma table for a linear value (1.0 = white), shoulder included.
fn gamma_value(x: f64) -> f64 {
    const SLOPE: f64 = 9.0;
    const OFFSET: f64 = 0.013674885973673709;
    const THRESHOLD: f64 = 0.0018992897185657936;
    const EXPONENT: f64 = 0.5555555555555556;
    let y = if x <= THRESHOLD { x * SLOPE + OFFSET } else { (x.powf(EXPONENT) - 0.0) / (1.0 - 0.0) };
    if 0.4 < y {
        let t = y * 1.5;
        (1.0 / (y.powf(4.0) + 5.0625)).powf(0.25) * t
    } else {
        y
    }
}

#[inline]
fn round_out(v: f64) -> i32 {
    (v + if 0.0 <= v { 0.5 } else { -0.5 }) as i32
}

/// Gamma table (65536 entries, index = linear value as 16-bit code with 4096 = 1.0, value =
/// encoded 16-bit): sampled at indices 0, 8, 17, 27, … (the step grows by one) and interpolated
/// linearly until the maximum is reached, then constant; codes 0x8000 … 0xffff are negative
/// values (straight line of the toe, zero once it reaches zero).
pub fn gamma_table() -> &'static [u16] {
    static T: OnceLock<Vec<u16>> = OnceLock::new();
    T.get_or_init(|| {
        const MAX: f64 = 65535.0;
        let limit = (0.5 + MAX) as i32;
        let mut t = vec![0u16; 65536];
        let mut next: i64 = 0;
        let mut most_recent = 0i32;
        let mut position: i64 = 0;
        let mut step: i64 = 8;
        let mut before = 0.0f64;
        loop {
            let y = gamma_value(position as f64 * 0.000244140625);
            if next <= position {
                let basis = next - 1;
                let mut k = 1u32;
                let mut j = next;
                loop {
                    let tt = f64::from(k) / (position - basis) as f64;
                    most_recent = round_out(((1.0 - tt) * before + y * tt) * MAX);
                    if limit <= most_recent {
                        next = j;
                        break;
                    }
                    t[(j & 0xffff) as usize] = most_recent as u16;
                    next = j + 1;
                    k += 1;
                    let further = j < position;
                    j = next;
                    if !further {
                        break;
                    }
                }
            }
            position += step;
            step += 1;
            before = y;
            if limit <= most_recent {
                break;
            }
        }
        for j in next..0x8000 {
            t[j as usize] = limit as u16;
        }
        for j in (0x8001..=0xffffusize).rev() {
            let y = gamma_value((j as f64 - 65536.0) * 0.000244140625);
            if y <= 0.0 {
                break;
            }
            t[j] = round_out(y * MAX) as u16;
        }
        t
    })
}

/// Colour management from 16-bit ROMM RGB (gamma 1.8 as in its profile) to an output space with
/// matrix and tone curves (relative colorimetric, D50 colorants of the profiles).
pub struct ColorSpace {
    /// ROMM code → linear
    inp: Vec<f32>,
    /// output = matrix · ROMM (linear)
    m: [f64; 9],
    /// output encoding: linear (0…1) → 16-bit
    outp: OutputCurve,
}

enum OutputCurve {
    /// sampled curve (code k/(n−1) → linear value · 65535), inverted piecewise linearly
    Table(Vec<f64>),
    Gamma(f64),
}

impl ColorSpace {
    fn new(gamma_in: f64, in_xyz: [[i32; 3]; 3], from_xyz: [[i32; 3]; 3], outp: OutputCurve) -> Self {
        let columns = |p: [[i32; 3]; 3]| -> [f64; 9] { std::array::from_fn(|i| f64::from(p[i % 3][i / 3]) / 65536.0) };
        let a = columns(in_xyz);
        let b = super::calibration::inv(&columns(from_xyz));
        let m = super::calibration::mm(&b, &a);
        let inp = (0..65536).map(|i| (f64::from(i) / 65535.0).powf(gamma_in) as f32).collect();
        ColorSpace { inp, m, outp }
    }

    fn encode_px(&self, y: f64) -> u16 {
        let y = y.clamp(0.0, 1.0);
        let x = match &self.outp {
            OutputCurve::Gamma(g) => y.powf(1.0 / g),
            OutputCurve::Table(t) => {
                let v = y * 65535.0;
                let n = t.len();
                // first segment whose end is at least v
                let mut lo = 0usize;
                let mut hi = n - 1;
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    if t[mid] < v { lo = mid } else { hi = mid }
                }
                let (a, b) = (t[lo], t[hi]);
                let f = if b > a { (v - a) / (b - a) } else { 0.0 };
                (lo as f64 + f) / (n - 1) as f64
            }
        };
        (x * 65535.0 + 0.5).min(65535.0) as u16
    }

    #[inline]
    pub fn convert(&self, e: [u16; 3]) -> [u16; 3] {
        let l = e.map(|v| f64::from(self.inp[v as usize]));
        let m = &self.m;
        std::array::from_fn(|r| self.encode_px(m[r * 3] * l[0] + m[r * 3 + 1] * l[1] + m[r * 3 + 2] * l[2]))
    }
}

/// ROMM RGB colorants (D50, s15.16) and the gamma of its profile.
const ROMM_XYZ: [[i32; 3]; 3] = [[52276, 18877, 0], [8860, 46654, 0], [2054, 6, 54061]];
const ROMM_GAMMA: f64 = 461.0 / 256.0;

fn srgb() -> &'static ColorSpace {
    static F: OnceLock<ColorSpace> = OnceLock::new();
    F.get_or_init(|| {
        // sRGB colorants adapted to D50 (s15.16), standard curve sampled at 1024 points
        let xyz = [[28578, 14581, 912], [25241, 46981, 6362], [9376, 3972, 46799]];
        let curve = (0..1024)
            .map(|k| {
                let x = f64::from(k) / 1023.0;
                let y = if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
                (y * 65535.0).round()
            })
            .collect();
        ColorSpace::new(ROMM_GAMMA, ROMM_XYZ, xyz, OutputCurve::Table(curve))
    })
}

fn adobe_rgb() -> &'static ColorSpace {
    static F: OnceLock<ColorSpace> = OnceLock::new();
    F.get_or_init(|| {
        let xyz = [[39960, 20389, 1276], [13453, 41004, 3989], [9777, 4143, 48796]];
        ColorSpace::new(ROMM_GAMMA, ROMM_XYZ, xyz, OutputCurve::Gamma(563.0 / 256.0))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral() -> Tone {
        Tone { exposure: 0.0, shadows: 0.0, contrast: 0.0, highlights: 0.0, color: [1.0; 3] }
    }

    /// The neutral tone table is the same for all layers, starts near zero, rises monotonically
    /// and maps the linear code of white below 1.0 (the shoulder keeps headroom: about 0.83).
    #[test]
    fn tone_table_neutral() {
        let k = tone_curve(&neutral(), false, false);
        assert_eq!(k[0], k[1]);
        assert_eq!(k[1], k[2]);
        for i in 1..0x8000 {
            assert!(k[0][i] >= k[0][i - 1], "not monotone at {i:#x}: {} < {}", k[0][i], k[0][i - 1]);
        }
        assert!(k[0][0x8000..].iter().all(|v| *v == 0));
        // linear 1.0 (code 4096) → cube-root code
        let w = detail::root()[4096] as usize;
        let white = f64::from(k[0][w]) / 4096.0;
        assert!((0.75..1.0).contains(&white), "white → {white}");
        // 18 % grey stays a mid tone
        let g = detail::root()[(0.18 * 4096.0) as usize] as usize;
        let gray = f64::from(k[0][g]) / 4096.0;
        assert!((0.1..0.3).contains(&gray), "grey → {gray}");
        // black: only the small lift of the straight toe
        assert!(k[0][0] < 400, "black → {}", k[0][0]);
    }

    /// Whole chain plus output step on one file against Sigma's export (skipped when the files are
    /// missing): mean colour difference in sRGB-encoded 8-bit units must stay small. Folder with `x3f/` and
    /// `spp-tiff/`: `GRAINEO_TEST_CLASSIC_REF_DIR` (the files are not part of the repository).
    #[test]
    fn file_matches_reference_export() {
        let Some(basis) = std::env::var_os("GRAINEO_TEST_CLASSIC_REF_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_CLASSIC_REF_DIR not set, skipped");
            return;
        };
        let raw = basis.join("x3f/pb_sigma_dp1s_01.x3f");
        let tif = basis.join("spp-tiff/pb_sigma_dp1s_01.tif");
        if !raw.exists() || !tif.exists() {
            eprintln!("test files missing, skipped");
            return;
        }
        let x = x3f::X3fFile::open(&raw).unwrap();
        let (camf, props) = (x.camf().unwrap(), x.properties().unwrap());
        let wb = props.get("WB_DESC").unwrap_or("Auto").trim().to_string();
        let mode = props.get("CM_DESC").unwrap_or("Standard").trim().to_string();
        let codes = crate::classic_new::run(&x, &camf, &props, &wb, &mode, 0).unwrap();
        let a = Output::new(&camf, &props, &wb, &mode, false, &Slider::default(), &codes).unwrap();
        let mut dec = tiff::decoder::Decoder::new(std::fs::File::open(&tif).unwrap()).unwrap();
        let (tw, th) = dec.dimensions().unwrap();
        let tiff::decoder::DecodingResult::U16(spp) = dec.read_image().unwrap() else { panic!("not a 16-bit TIFF") };
        let [x0, y0, x1, y1] = codes.crop;
        assert_eq!(((x1 - x0) as u32, (y1 - y0) as u32), (tw, th), "size");
        let (mut s, mut n) = (0.0f64, 0usize);
        for y in (0..th as usize).step_by(7) {
            for xx in (0..tw as usize).step_by(7) {
                let p = ((y0 + y) * codes.width + x0 + xx) * 3;
                let o = a.pixel([codes.data[p], codes.data[p + 1], codes.data[p + 2]]);
                let q = (y * tw as usize + xx) * 3;
                for c in 0..3 {
                    s += (f64::from(o[c]) - f64::from(spp[q + c])).abs();
                    n += 1;
                }
            }
        }
        let mean = s / n as f64 / 257.0;
        eprintln!("mittlere Abweichung {mean:.3} (8 Bit)");
        assert!(mean < 0.5, "{mean}");
    }

    /// Exposure raises, the gains tilt the layers, contrast steepens around the middle.
    #[test]
    fn tone_table_sliders() {
        let n = tone_curve(&neutral(), false, false);
        let bright = tone_curve(&Tone { exposure: 1.0, ..neutral() }, false, false);
        let center = detail::root()[(0.18 * 4096.0) as usize] as usize;
        assert!(bright[1][center] > n[1][center]);
        let blue = tone_curve(&Tone { color: [1.0, 1.0, 0.9], ..neutral() }, false, false);
        assert!(blue[2][center] < blue[1][center]);
        let k = tone_curve(&Tone { contrast: 1.0, ..neutral() }, false, false);
        let dark = detail::root()[(0.02 * 4096.0) as usize] as usize;
        assert!(k[1][dark] < n[1][dark]);
        for c in 0..3 {
            for i in 1..0x8000 {
                assert!(bright[c][i] >= bright[c][i - 1] && k[c][i] >= k[c][i - 1]);
            }
        }
    }

    #[test]
    fn gamma_table_shape() {
        let t = gamma_table();
        for i in 1..0x8000 {
            assert!(t[i] >= t[i - 1], "{i:#x}");
        }
        assert_eq!(t[0x7fff], 65535);
        // toe offset at zero, straight line below zero
        assert_eq!(t[0], (0.013674885973673709f64 * 65535.0 + 0.5) as u16);
        assert!(t[0xffff] < t[0] && t[0xffff] > 0);
        // mid grey: 0.18^(1/1.8)
        let g = f64::from(t[(0.18 * 4096.0) as usize]) / 65535.0;
        assert!((g - 0.18f64.powf(1.0 / 1.8)).abs() < 2e-3, "{g}");
    }

    /// Every index of the inverse table survives the fractional code, rounding gives the code.
    #[test]
    fn index_round_trip() {
        let ex = detail::exponent();
        for i in 0..0x7000i16 {
            if ex[i as usize] == 0x7fff {
                continue;
            }
            let l = code_with_position(i);
            assert_eq!(l.round() as i32, i32::from(ex[i as usize]), "{i}");
            let j = index_from_position(l);
            assert_eq!(i32::from(ex[j as usize]), i32::from(ex[i as usize]));
            assert_eq!(j, i32::from(i), "index {i}");
        }
    }

    #[test]
    fn saturation_matrix_neutral_and_sum() {
        assert_eq!(saturation_matrix(0.0), [4096, 0, 0, 0, 4096, 0, 0, 0, 4096]);
        let m = saturation_matrix(0.6);
        assert!(m[0] > 4096 && m[1] < 0);
        assert!((m[0] + m[1] + m[2] - 4096).abs() <= 2);
    }

    /// ROMM white (and grey) maps to sRGB white (grey): both profiles share the D50 white.
    #[test]
    fn colour_management_keeps_neutral() {
        for v in [65535u16, 30000, 4000] {
            let o = srgb().convert([v; 3]);
            assert!(o[0].abs_diff(o[1]) <= 40 && o[1].abs_diff(o[2]) <= 40, "{v}: {o:?}");
        }
        assert!(srgb().convert([65535; 3]).iter().all(|v| *v >= 65400));
    }

    #[test]
    fn grey_balance_keeps_weighted_mean() {
        let m = [1000.0, 1200.0, 800.0];
        let g = gray_balance([1.0, 1.0, 0.912], m);
        let pre = 0.3 * m[0] + 0.55 * m[1] + 0.15 * m[2];
        let after = 0.3 * g[0] * m[0] + 0.55 * g[1] * m[1] + 0.15 * g[2] * m[2];
        assert!((pre - after).abs() / pre < 1e-6);
        assert!(g[0] > 1.0 && (g[2] / g[0] - 0.912).abs() < 1e-9);
    }
}
