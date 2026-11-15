//! Classic chain: automatic white balance from the image (cameras without a stored
//! per-white-balance calibration: SD9, SD10, SD14 with white balance "Auto"). The image is reduced
//! to block means, converted to a working colour space and tone-adjusted; the neutral point is
//! estimated from the chroma-weighted near-neutral pixels relative to a few candidate light sources,
//! refined iteratively and constrained to a plausible region depending on the scene brightness.
//!
//! Outline:
//! 1. Reduction: for wide images a 1 % border is skipped and blocks of 4 columns × 8 rows are
//!    averaged; the dark offset is subtracted and the post polynomial applied (single precision).
//! 2. The reduced image is converted to 16-bit working-space codes (RIMM matrix of the camera
//!    matrix with a neutral of 1) and tone-adjusted with an automatic tone curve. The tone image
//!    only serves to group similar pixels in a 32³ histogram.
//! 3. Six candidate light sources (from very blue to incandescent): for each the mean chroma of
//!    the first quarter of the pixels in Lab relative to that white; the best one (with a few
//!    rules for close calls) starts the estimate.
//! 4. Estimate: the camera-space mean of the pixels that are below the per-layer maximum, not too
//!    dark (L* > 15) and close to neutral (chroma below 45, later 30), each weighted with a chroma
//!    weight and with one over the square root of the number of pixels in its tone bin; then
//!    normalised to its largest component.
//! 5. Constraint: in the working space the ratios R/G and B/G (× 10000) are kept inside a polygon
//!    that depends on the brightness value (and flash); between two ranges the result is blended.
//! 6. Up to five refinements with a narrower chroma window, a final constraint and a
//!    brightness-dependent shift of red and blue (bright scenes slightly warmer).

use super::calibration::{cam2rimm, inv, Mat};

/// Inputs of the estimate (all at this point of the chain).
#[derive(Clone, Debug)]
pub struct Input<'a> {
    /// sensor codes (`w × h × 3`, as 16-bit values) after the input factor, before the row correction
    pub image: &'a [i32],
    pub w: usize,
    pub h: usize,
    /// saturation level per layer (integer, long-exposure factor applied)
    pub saturation: [i32; 3],
    /// post polynomial (27 numbers, per layer R², G², B², RG, RB, GB, R³, G³, B³)
    pub post_polynomial: [f32; 27],
    /// scene brightness value (APEX)
    pub bv: f64,
    /// flash fired (and its compensation not below −0.5)
    pub flash: bool,
    /// matrix layer codes → XYZ before the neutral-point scaling
    pub matrix: Mat,
    /// capture ISO / sensor ISO
    pub iso_factor: f64,
    /// dark level to subtract per layer (flare plus mean drift)
    pub offset: [f64; 3],
    /// exposure not longer than `MaxConstrainTime` (illuminant constraint applies)
    pub short: bool,
}

/// Neutral point (layer response of the estimated white, largest component 1). Non-finite or zero
/// components mean "no estimate"; the caller then keeps the stored neutral point.
pub fn neutral_point(e: &Input) -> [f64; 3] {
    if e.w == 0 || e.h == 0 || e.image.len() < e.w * e.h * 3 {
        return [f64::NAN; 3];
    }
    let mut s = Estimator::new(e);
    if s.w == 0 || s.h == 0 {
        return [f64::NAN; 3];
    }
    s.shrink(e);
    s.working_space(e);
    s.tone = tone_image(&s.rimm, s.w, s.h, e.iso_factor);
    s.flow(e)
}

// ---------------------------------------------------------------------------------------------
// constants

/// Candidate light sources (XYZ, Y = 100): bluish sky, shade, daylight, warm daylight, warm light,
/// incandescent.
const HIGHLIGHTS: [[f64; 3]; 6] = [
    [94.9466, 100.0, 138.4398],
    [94.38, 100.0, 117.898],
    [95.0456, 100.0, 108.9058],
    [95.5475, 100.0, 77.986],
    [102.8524, 100.0, 49.5528],
    [108.854, 100.0, 33.4394],
];

/// Chroma weights of the refinement passes (index = rounded chroma, 0 … 29).
const CHROMA_WEIGHT_2: [f64; 30] = [
    1.0,
    0.9998888930040644,
    0.9991113744595842,
    0.997002999,
    0.9929057315376721,
    0.9861753122936036,
    0.9761914879999999,
    0.9623709909766297,
    0.9441830740370879,
    0.9211673169999999,
    0.8929533099629123,
    0.8592816894431236,
    0.8200258559999999,
    0.775213536027384,
    0.7250471660425748,
    0.669921875,
    0.6104396189231317,
    0.5474177824774172,
    0.4818903040000001,
    0.4150991039587969,
    0.34847330183407005,
    0.283593393,
    0.2221372253319109,
    0.1658042649767311,
    0.11621427199999991,
    0.07477611946222622,
    0.042522083880709195,
    0.01990251099999998,
    0.006535318325052068,
    0.0009043349657572533,
];

/// Polygons of plausible illuminants in the working space, vertices (R/G, B/G) × 10000, closed
/// (last vertex = first). Dim scenes (BV ≤ 4.9): wide region.
pub(super) const POLY_DARK: [(i32, i32); 12] = [
    (7759, 8388),
    (8545, 4847),
    (8899, 3834),
    (9557, 3276),
    (13000, 3200),
    (14700, 5000),
    (13700, 11500),
    (9952, 16300),
    (8626, 16000),
    (7766, 13209),
    (7662, 8899),
    (7759, 8388),
];
/// Medium brightness (5.7 < BV ≤ 5.8, and the blend ranges).
pub(super) const POLY_MEAN: [(i32, i32); 10] = [
    (9500, 9700),
    (10200, 7800),
    (10500, 7700),
    (11200, 8000),
    (11500, 9200),
    (11200, 12500),
    (10500, 14000),
    (9500, 14100),
    (9000, 12500),
    (9500, 9700),
];
/// Bright scenes (BV > 8.3): narrow region around daylight.
pub(super) const POLY_BRIGHT: [(i32, i32); 10] = [
    (9700, 9700),
    (10200, 8200),
    (10500, 8100),
    (10700, 8200),
    (10800, 9600),
    (10600, 11000),
    (10100, 11400),
    (9500, 11300),
    (9400, 10700),
    (9700, 9700),
];
/// Flash.
const POLY_FLASH: [(i32, i32); 12] = [
    (9600, 10200),
    (9900, 10000),
    (10100, 10000),
    (10400, 10300),
    (10500, 11000),
    (10500, 12500),
    (10100, 14100),
    (9850, 14300),
    (9100, 14200),
    (9000, 13700),
    (9000, 12700),
    (9600, 10200),
];

const THIRD: f64 = 0.3333333333333333;

// ---------------------------------------------------------------------------------------------
// numeric helpers (bit-exact results)

/// Truncating conversion to a 32-bit integer (out of range or NaN gives the "integer indefinite"
/// value `i32::MIN`).
#[inline]
pub(super) fn tr32(x: f64) -> i32 {
    if x.is_nan() || x >= 2147483648.0 || x < -2147483648.0 {
        i32::MIN
    } else {
        x as i32
    }
}

#[inline]
fn tr32f(x: f32) -> i32 {
    tr32(f64::from(x))
}

/// Larger of two (second operand when the first is not larger, also for NaN).
#[inline]
fn maxsd(a: f64, b: f64) -> f64 {
    if a > b { a } else { b }
}

/// Normalisation to the largest component (larger of the last two, then larger of that and the
/// first).
pub(super) fn to_max(v: [f64; 3]) -> [f64; 3] {
    let m = maxsd(v[0], maxsd(v[1], v[2]));
    [v[0] / m, v[1] / m, v[2] / m]
}

/// Signed cube root.
#[inline]
fn cube_root(x: f64) -> f64 {
    if x < 0.0 { -(-x).powf(THIRD) } else { x.powf(THIRD) }
}

/// Lab lightness function of a ratio to the white: linear near zero, a coarse table (step 0.0016)
/// in the usual range, the exact root outside.
#[inline]
fn lab_f(t: f64, tab: &[f64]) -> f64 {
    if t <= 0.008856 {
        t * 7.787 + 0.13793103448275862
    } else if -1.0 > t || t >= 15.0 {
        cube_root(t)
    } else {
        let i = ((t + 1.0) * 625.0) as i64 as u32 as usize;
        tab[i]
    }
}

/// `m · v` summed as `(m0 v0 + m1 v1) + m2 v2` per row.
#[inline]
pub(super) fn mul_mv(m: &Mat, v: &[f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (m[i * 3] * v[0] + m[i * 3 + 1] * v[1]) + m[i * 3 + 2] * v[2])
}

/// Post polynomial in 14-bit fixed point, applied to a single colour.
pub(super) fn polynomial(p: &[f32; 27], v: [f64; 3]) -> [f64; 3] {
    let k: [i32; 27] = std::array::from_fn(|i| tr32f(p[i] * 16384.0f32));
    let x = tr32(v[0] + 0.5);
    let y = tr32(v[1] + 0.5);
    let z = tr32(v[2] + 0.5);
    let xx = (x.wrapping_mul(x) as u32 >> 14) as i32;
    let yy = (y.wrapping_mul(y) as u32 >> 14) as i32;
    let zz = (z.wrapping_mul(z) as u32 >> 14) as i32;
    let xy = y.wrapping_mul(x) >> 14;
    let xz = z.wrapping_mul(x) >> 14;
    let yz = z.wrapping_mul(y) >> 14;
    let xxx = xx.wrapping_mul(x) >> 14;
    let yyy = yy.wrapping_mul(y) >> 14;
    let zzz = zz.wrapping_mul(z) >> 14;
    let terms = [xx, yy, zz, xy, xz, yz, xxx, yyy, zzz];
    let basis = [x, y, z];
    std::array::from_fn(|c| {
        let mut s = 0i32;
        for (j, t) in terms.iter().enumerate() {
            s = s.wrapping_add(k[c * 9 + j].wrapping_mul(*t));
        }
        f64::from((s >> 14).wrapping_add(basis[c]))
    })
}

/// Border (multiple of 4) and reduced size of wide images.
fn reduction(w: usize, h: usize) -> (usize, usize, usize, usize) {
    let border_x = (((w as u32 as f64) * 0.01) as i32 + 3) & !3;
    let border_y = (((h as u32 as f64) * 0.01) as i32 + 3) & !3;
    let w2 = (w as u32).wrapping_sub(2 * border_x as u32) >> 2;
    let h2 = (h as u32).wrapping_sub(2 * border_y as u32) >> 3;
    (border_x as usize, border_y as usize, w2 as usize, h2 as usize)
}

// ---------------------------------------------------------------------------------------------
// the estimator

struct Estimator {
    w: usize,
    h: usize,
    /// reduced image in camera codes
    image: Vec<[f32; 3]>,
    /// the same in 16-bit working-space codes
    rimm: Vec<[i16; 3]>,
    /// tone-adjusted working-space codes
    tone: Vec<[i16; 3]>,
    /// per-layer maximum of the reduced image, capped at the saturation
    max: [f64; 3],
    /// chroma² per pixel of the last pass
    chroma: Vec<f32>,
    /// coarse cube-root table of the Lab conversion
    root: Vec<f64>,
    /// chroma weights (index = rounded chroma)
    weight: Vec<f64>,
    /// camera matrix scaled for the Lab conversion (ISO factor × 0.0156)
    m_lab: Mat,
}

impl Estimator {
    fn new(e: &Input) -> Self {
        let (w, h) = if e.w > 500 {
            let (_, _, w2, h2) = reduction(e.w, e.h);
            (w2, h2)
        } else {
            (e.w, e.h)
        };
        // cube roots of −1 … 15 in steps of 0.0016 (the step is accumulated)
        let mut root = Vec::with_capacity(10000);
        let mut x = -1.0f64;
        for _ in 0..10000 {
            root.push(cube_root(x));
            x += 0.0016;
        }
        // tricube chroma weights for the first pass; the two entries after them are a stored
        // value and the first cube root (the lookup reaches them at the end of the table)
        let mut weight: Vec<f64> = (0..44)
            .map(|i| {
                let q = f64::from(i) / 45.0;
                let a = 1.0 - q * q * q;
                a * a * a
            })
            .collect();
        weight.push(f64::from_bits(0x3f3229484d575903));
        weight.push(root[0]);
        let s = e.iso_factor * 0.0156;
        let m_lab: Mat = std::array::from_fn(|i| e.matrix[i] * s);
        Estimator { w, h, image: Vec::new(), rimm: Vec::new(), tone: Vec::new(), max: [0.0; 3], chroma: vec![0.0; w * h], root, weight, m_lab }
    }

    /// Step 1: block means (or the plain codes for small images).
    fn shrink(&mut self, e: &Input) {
        let (w, h) = (self.w, self.h);
        let code = |x: usize, y: usize, c: usize| -> i32 { i32::from(e.image[(y * e.w + x) * 3 + c] as i16) };
        self.image = vec![[0.0f32; 3]; w * h];
        if e.w <= 500 {
            for y in 0..h {
                for x in 0..w {
                    self.image[y * w + x] = std::array::from_fn(|c| code(x, y, c) as f32);
                }
            }
            return;
        }
        let (rx, ry, _, _) = reduction(e.w, e.h);
        let offset: [f32; 3] = e.offset.map(|v| v as f32);
        let poly = e.post_polynomial.iter().any(|&p| p != 0.0);
        for j in 0..h {
            let row = &mut self.image[j * w..(j + 1) * w];
            for k in 0..8 {
                let y = ry + 8 * j + k;
                for (i, p) in row.iter_mut().enumerate() {
                    let x = rx + 4 * i;
                    for c in 0..3 {
                        let s = ((code(x, y, c) + code(x + 1, y, c)) + code(x + 2, y, c)) + code(x + 3, y, c);
                        p[c] += s as f32;
                    }
                }
            }
            for p in row.iter_mut() {
                let v: [f64; 3] = std::array::from_fn(|c| f64::from(p[c] * 0.03125f32 - offset[c]));
                let v = if poly { polynomial(&e.post_polynomial, v) } else { v };
                *p = v.map(|x| x as f32);
            }
        }
    }

    /// Step 2 (first half): working-space codes and the per-layer maxima.
    fn working_space(&mut self, e: &Input) {
        let r = cam2rimm(&e.matrix, &[1.0, 1.0, 1.0], e.iso_factor, 0.0);
        let mut max = [0.0f64; 3];
        self.rimm = self
            .image
            .iter()
            .map(|p| {
                let v = p.map(f64::from);
                for c in 0..3 {
                    if v[c] > max[c] {
                        max[c] = v[c];
                    }
                }
                mul_mv(&r, &v).map(|x| tr32(x) as i16)
            })
            .collect();
        for c in 0..3 {
            let s = f64::from(e.saturation[c]);
            self.max[c] = if max[c] < s { max[c] } else { s };
        }
    }

    /// Lab lightness functions of a reduced pixel relative to a white.
    #[inline]
    fn lab_f3(&self, p: &[f32; 3], white: &[f64; 3]) -> [f64; 3] {
        let xyz = mul_mv(&self.m_lab, &p.map(f64::from));
        std::array::from_fn(|i| lab_f(xyz[i] / white[i], &self.root))
    }

    /// Step 3: mean chroma of the first quarter of the pixels relative to each candidate.
    fn candidate(&mut self) -> usize {
        let n = self.w * self.h;
        let quarter = ((n as u32) >> 2) as usize;
        let mut mean = [0.0f64; 6];
        let mut best = 0usize;
        let mut smallest = 1000000.0f64;
        for (k, white) in HIGHLIGHTS.iter().enumerate() {
            for i in 0..quarter {
                let f = self.lab_f3(&self.image[i], white);
                let a = (f[0] - f[1]) * 500.0;
                let b = (f[1] - f[2]) * 200.0;
                self.chroma[i] = (b * b + a * a) as f32;
            }
            let mut s = 0.0f64;
            for &c in &self.chroma[..quarter] {
                s += f64::from(c.sqrt());
            }
            let m = s / quarter as f64;
            mean[k] = m;
            if smallest > m {
                best = k;
            }
            smallest = if m < smallest { m } else { smallest };
        }
        if !(smallest > 0.0) || best > 3 {
            return best;
        }
        // candidates within 50 % of the best
        let list: Vec<usize> = (0..6).filter(|&k| 0.5 > (mean[k] - smallest) / smallest).collect();
        let preset = match list.len() {
            n if n > 4 => 2,
            3 | 4 => list[1],
            _ => list[0],
        };
        if preset > 4 {
            return preset;
        }
        // the two warm candidates decide by their ratio
        let q = mean[4] / mean[5];
        if q > 1.25 {
            5
        } else if q > 1.075 {
            4
        } else {
            preset
        }
    }

    /// Step 4: neutral estimate relative to a white (XYZ, Y = 100) with a chroma limit.
    fn estimate(&mut self, white: &[f64; 3], limit: f32) -> [f64; 3] {
        let n = self.w * self.h;
        let g2 = limit * limit;
        let g2d = f64::from(g2);
        let outp = g2 + g2;
        let mut min = [10000.0f32; 3];
        let mut max = [0.0f32; 3];
        for i in 0..n {
            self.chroma[i] = outp;
            let p = &self.image[i];
            let v = p.map(f64::from);
            if !(self.max[0] > v[0]) || !(self.max[1] > v[1]) || !(self.max[2] > v[2]) {
                continue;
            }
            let f = self.lab_f3(p, white);
            let l = f[1] * 116.0 + -16.0;
            if !(l > 15.0) {
                continue;
            }
            let a = (f[0] - f[1]) * 500.0;
            let b = (f[1] - f[2]) * 200.0;
            let c2 = b * b + a * a;
            if !(g2d > c2) {
                continue;
            }
            self.chroma[i] = c2 as f32;
            let t = self.tone[i].map(f32::from);
            for c in 0..3 {
                max[c] = if t[c] > max[c] { t[c] } else { max[c] };
                min[c] = if t[c] < min[c] { t[c] } else { min[c] };
            }
        }
        let region: [f32; 3] = std::array::from_fn(|c| (max[c] - min[c]) + 1.0f32);
        let slot = |t: &[i16; 3]| -> usize {
            let k: [usize; 3] = std::array::from_fn(|c| (tr32f(((f32::from(t[c]) - min[c]) * 32.0f32) / region[c]) as u32 & 31) as usize);
            (k[0] << 10) | (k[1] << 5) | k[2]
        };
        let mut hist = vec![0u32; 32 * 32 * 32];
        for i in 0..n {
            if g2 > self.chroma[i] {
                hist[slot(&self.tone[i])] += 1;
            }
        }
        let mut sum = [0.0f64; 3];
        let mut count = 0u32;
        for i in 0..n {
            let c2 = self.chroma[i];
            if !(g2 > c2) {
                continue;
            }
            let z = hist[slot(&self.tone[i])];
            if z == 0 {
                continue;
            }
            let idx = (f64::from(c2.sqrt()) + 0.5) as i64 as u32 as usize;
            let g = (1.0 / f64::from(z).sqrt()) * self.weight.get(idx).copied().unwrap_or(0.0);
            let p = &self.image[i];
            for c in 0..3 {
                sum[c] += f64::from((g * f64::from(p[c])) as f32);
            }
            count += 1;
        }
        let n = f64::from(count);
        to_max([sum[0] / n, sum[1] / n, sum[2] / n])
    }

    /// Step 5: keep the estimate inside the plausible illuminant region.
    fn clamp_limit(&self, e: &Input, estimate: [f64; 3]) -> [f64; 3] {
        let r = cam2rimm(&e.matrix, &[-1.0, -1.0, -1.0], e.iso_factor, 0.0);
        let v = mul_mv(&r, &estimate);
        let c = if !e.short {
            // long exposure: only clamp the ratios (relative to the sum)
            let v01 = [if v[0] <= 0.1 { 0.1 } else { v[0] }, if v[1] <= 0.1 { 0.1 } else { v[1] }];
            let v2 = maxsd(0.1, v[2]);
            let k = 3.0 / ((v01[1] + v01[0]) + v2);
            let a = [k * v01[0], k * v01[1]];
            let a2 = k * v2;
            let a = [if a[0] <= 0.6 { 0.6 } else { a[0] }, if a[1] <= 0.9 { 0.9 } else { a[1] }];
            let a2 = maxsd(0.5, a2);
            let k2 = 1.0 / a[1];
            [k2 * a[0], k2 * a[1], k2 * a2]
        } else {
            let p = (tr32((v[0] / v[1]) * 10000.0), tr32((v[2] / v[1]) * 10000.0));
            let q = if e.flash { in_polygon(&POLY_FLASH, p) } else { after_brightness(e.bv, p) };
            [f64::from(q.0) / 10000.0, 1.0, f64::from(q.1) / 10000.0]
        };
        to_max(mul_mv(&inv(&r), &c))
    }

    fn flow(&mut self, e: &Input) -> [f64; 3] {
        let k = if e.flash { 1 } else { self.candidate() };
        let mut x = self.estimate(&HIGHLIGHTS[k], 45.0);
        x = self.clamp_limit(e, x);
        // refinement: narrower chroma window and the smooth weight table
        self.weight[..30].copy_from_slice(&CHROMA_WEIGHT_2);
        let m = &e.matrix;
        let mut round = 1;
        loop {
            let before = x;
            let xyz = mul_mv(m, &x);
            let white = [(xyz[0] / xyz[1]) * 100.0, (xyz[1] / xyz[1]) * 100.0, (xyz[2] / xyz[1]) * 100.0];
            x = self.estimate(&white, 30.0);
            if round > 4 {
                break;
            }
            let d: [f32; 3] = std::array::from_fn(|c| (((before[c] - x[c]) / x[c]) as f32).abs());
            let mean = ((d[1] + d[0]) + d[2]) / 3.0f32;
            round += 1;
            if !(f64::from(mean) > 0.005) {
                break;
            }
        }
        x = self.clamp_limit(e, x);
        // brighter scenes: red slightly down, blue up in the working space
        if e.bv > 3.2 {
            let f = if 5.7 > e.bv { ((e.bv + -3.2) / 2.5) as f32 } else { 1.0f32 };
            let r = cam2rimm(&e.matrix, &[-1.0, -1.0, -1.0], e.iso_factor, 0.0);
            let s = mul_mv(&r, &x);
            let fr = f * -0.02f32 + 1.0f32;
            let fb = f * 0.08f32 + 1.0f32;
            x = mul_mv(&inv(&r), &[f64::from(fr) * s[0], s[1], f64::from(fb) * s[2]]);
        }
        to_max(x)
    }
}

// ---------------------------------------------------------------------------------------------
// illuminant region

/// Position of a point relative to a closed polygon: 'v' vertex, 'e' edge, 'i' inside, 'o'
/// outside (crossing counts to the right and to the left of the point).
pub(super) fn position(poly: &[(i32, i32)], p: (i32, i32)) -> u8 {
    let n = poly.len();
    let q: Vec<(i32, i32)> = poly.iter().map(|v| (v.0.wrapping_sub(p.0), v.1.wrapping_sub(p.1))).collect();
    let mut right = 0i32;
    let mut links = 0i32;
    for i in 0..n {
        let (x, y) = q[i];
        if x == 0 && y == 0 {
            return b'v';
        }
        let (x1, y1) = q[(i + n - 1) % n];
        if (y > 0) != (y1 > 0) {
            let t = (f64::from(x) * f64::from(y1) - f64::from(x1) * f64::from(y)) / f64::from(y1.wrapping_sub(y));
            if t > 0.0 {
                right += 1;
            }
        }
        if (y1 ^ y) < 0 {
            let z = x.wrapping_mul(y1).wrapping_sub(y.wrapping_mul(x1));
            let t = f64::from(z) / f64::from(y1.wrapping_sub(y));
            if 0.0 > t {
                links += 1;
            }
        }
    }
    let (r, l) = (right % 2, links % 2);
    if r != l {
        b'e'
    } else if r == 1 {
        b'i'
    } else {
        b'o'
    }
}

/// Nearest point on the polygon's outline (edges first, then the vertices; rounded to integers).
fn to_border(poly: &[(i32, i32)], p: (i32, i32)) -> (i32, i32) {
    let n = poly.len();
    if n <= 1 {
        return p;
    }
    let mut best = 1e20f64;
    let mut res = p;
    let px = f64::from(p.0);
    for i in 0..n - 1 {
        let (x0, y0) = poly[i];
        let (x1, y1) = poly[i + 1];
        let dx = x1.wrapping_sub(x0);
        let dy = y1.wrapping_sub(y0);
        let t = (f64::from(p.1.wrapping_sub(y0).wrapping_mul(dy)) + (px - f64::from(x0)) * f64::from(dx)) / f64::from(dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)));
        if !(t >= 0.0) || !(1.0 >= t) {
            continue;
        }
        let qx = tr32(f64::from(dx) * t + 0.5).wrapping_add(x0);
        let qy = tr32(f64::from(dy) * t + 0.5).wrapping_add(y0);
        let ex = f64::from(p.0.wrapping_sub(qx));
        let ey = p.1.wrapping_sub(qy);
        let d = (f64::from(ey.wrapping_mul(ey)) + ex * ex).sqrt();
        if best > d {
            res = (qx, qy);
            best = d;
        }
    }
    for v in &poly[..n - 1] {
        let ex = f64::from(p.0.wrapping_sub(v.0));
        let ey = p.1.wrapping_sub(v.1);
        let d = (f64::from(ey.wrapping_mul(ey)) + ex * ex).sqrt();
        if best > d {
            res = *v;
            best = d;
        }
    }
    res
}

/// Inside or on the polygon: unchanged; outside: nearest point of the outline.
fn in_polygon(poly: &[(i32, i32)], p: (i32, i32)) -> (i32, i32) {
    if position(poly, p) == b'o' { to_border(poly, p) } else { p }
}

/// Between two brightness ranges: inside the narrower region the point stays; outside it the
/// point moves from the wide region's outline towards the narrow one's with the brightness
/// fraction `t`, unless its projection on that path already lies beyond `t` (the projection is an
/// integer division).
fn crossfade(wide: &[(i32, i32)], narrow: &[(i32, i32)], p: (i32, i32), t: f64) -> (i32, i32) {
    let a = to_border(wide, p);
    let position_narrow = position(narrow, p);
    let b = to_border(narrow, p);
    if position_narrow != b'o' {
        return p;
    }
    let q = in_polygon(wide, p);
    let dx = b.0.wrapping_sub(a.0);
    let dy = b.1.wrapping_sub(a.1);
    let counter = q.0.wrapping_sub(a.0).wrapping_mul(dx).wrapping_add(q.1.wrapping_sub(a.1).wrapping_mul(dy));
    let denominator = dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy));
    let s = if denominator == 0 { 0 } else { counter.wrapping_div(denominator) };
    if !(t > f64::from(s)) {
        return p;
    }
    let u = 1.0 - t;
    (
        tr32(f64::from(a.0) * u + 0.5).wrapping_add(tr32(f64::from(b.0) * t + 0.5)),
        tr32(f64::from(a.1) * u + 0.5).wrapping_add(tr32(f64::from(b.1) * t + 0.5)),
    )
}

/// Region by scene brightness.
fn after_brightness(bv: f64, p: (i32, i32)) -> (i32, i32) {
    if 4.9 >= bv {
        in_polygon(&POLY_DARK, p)
    } else if 5.7 >= bv {
        crossfade(&POLY_DARK, &POLY_MEAN, p, (bv + -4.9) / 0.7999999999999998)
    } else if 5.8 >= bv {
        in_polygon(&POLY_MEAN, p)
    } else if 8.3 >= bv {
        crossfade(&POLY_MEAN, &POLY_BRIGHT, p, (bv + -5.8) / 2.500000000000001)
    } else {
        in_polygon(&POLY_BRIGHT, p)
    }
}

// ---------------------------------------------------------------------------------------------
// tone-adjusted image

/// Tone-adjusted copy of the working-space codes (only used to group pixels): cube-root codes
/// through an automatic tone curve per layer.
fn tone_image(rimm: &[[i16; 3]], w: usize, h: usize, iso_factor: f64) -> Vec<[i16; 3]> {
    let root = super::detail::root();
    let expo = super::detail::exponent();
    let a = auto_setting(rimm, w, h, root, expo);
    let curve = tone_curve(&a, expo, 0.5 >= iso_factor);
    rimm.iter().map(|p| std::array::from_fn(|c| curve[c][root[p[c] as u16 as usize] as u16 as usize] as i16)).collect()
}

/// Settings of the automatic tone adjustment (slider values in steps of 0.1 and a colour balance).
#[derive(Clone, Copy, Debug)]
struct AutoTone {
    exposure: f64,
    contrast: f64,
    shadows: f64,
    highlights: f64,
    color: [f64; 3],
}

/// Percentiles of the lightness histogram, their targets (8-bit cube-root scale) and the weights
/// with which the target deviations make up the four sliders.
const CRITERIA: [f64; 7] = [0.005, 0.02, 0.1, 0.5, 0.9, 0.98, 0.995];
const TARGETS: [f64; 7] = [60.0, 75.0, 95.0, 150.0, 205.0, 235.0, 255.0];
const G_BALANCE: [f64; 7] = [0.0, 16.0, 16.0, 8.0, 4.0, 2.0, 1.0];
const G_EXPOSURE: [f64; 7] = [0.0; 7];
const G_CONTRAST: [f64; 7] = [0.0, -1.0, -1.0, 0.0, 1.0, 1.0, 0.0];
const G_SHADOWS: [f64; 7] = [6.0, -1.0, -1.0, 0.0, 1.0, 1.0, 0.0];
const G_HIGHLIGHTS: [f64; 7] = [0.0, 1.0, 1.0, 0.0, -1.0, -1.0, 6.0];

/// Rounds half away from zero to an integer (table index).
#[inline]
fn round_i(x: f64) -> i32 {
    tr32(x + if x >= 0.0 { 0.5 } else { -0.5 })
}

/// Weighted sum `Σ g_j · v_j`, accumulated from the first term on.
fn scalar(g: &[f64; 7], v: &[f64; 7]) -> f64 {
    let mut s = g[0] * v[0];
    for j in 1..7 {
        s = g[j] * v[j] + s;
    }
    s
}

/// A slider value: below −1.5 it is −1.5, else at most 1.5, rounded down to 0.1 after adding 0.05.
fn slider(v: f64) -> f64 {
    let x = if v < -1.5 { -14.5 } else { (if 1.5 < v { 1.5 } else { v }) * 10.0 + 0.5 };
    x.floor() / 10.0
}

/// Automatic tone settings of a 16-bit working-space image: lightness percentiles of the central
/// part of a 4×4-sampled copy against fixed targets, and a colour balance from the weighted
/// centroid of a 32³ colour histogram.
fn auto_setting(image: &[[i16; 3]], w: usize, h: usize, root: &[i16], expo: &[i16]) -> AutoTone {
    let neutral = AutoTone { exposure: 0.0, contrast: 0.0, shadows: 0.0, highlights: 0.0, color: [1.0; 3] };
    if w < 31 || h < 39 {
        return neutral;
    }
    // every 4th row from 19, mean of 4 neighbouring pixels from column 13 on, every 4th column
    let bw = ((w - 31) >> 2) + 1;
    let bh = ((h - 39) >> 2) + 1;
    let mut small = vec![[0i16; 3]; bw * bh];
    let mut max = [1i32; 3];
    if h - 20 >= 20 && w - 18 > 13 {
        let mut j = 0;
        let mut y = 19;
        while y < h - 20 {
            let mut i = 0;
            let mut x = 13;
            while x < w - 18 {
                let mut v = [0i32; 3];
                for c in 0..3 {
                    let s: i32 = (0..4).map(|k| i32::from(image[y * w + x + k][c])).sum();
                    v[c] = s / 4;
                    if v[c] > max[c] {
                        max[c] = v[c];
                    }
                }
                if j < bh && i < bw {
                    small[j * bw + i] = v.map(|x| x as i16);
                }
                i += 1;
                x += 4;
            }
            j += 1;
            y += 4;
        }
    }
    // lightness histogram (cube-root code / 16) and colour histogram of the central 80 %
    let fan = (root[0xefff] / 16).wrapping_add(1) as u16 as usize;
    if fan == 0 {
        return neutral;
    }
    let mut hist = vec![0u32; fan];
    let mut hist3 = vec![0i32; 32 * 32 * 32];
    let k3: [i32; 3] = std::array::from_fn(|c| if max[c] == 0 { 0 } else { 0x1fffffff / max[c] });
    let y0 = (bh as f64 * 0.1) as usize;
    let x0 = (bw as f64 * 0.1) as usize;
    for y in y0..bh.saturating_sub(y0) {
        for x in x0..bw.saturating_sub(x0) {
            let p = small[y * bw + x];
            let l = (f64::from(p[1]) * 0.5 + f64::from(p[0]) * 0.3) + f64::from(p[2]) * 0.2;
            let code = root[(tr32(l.floor()) as u32 & 0xffff) as usize];
            let slot = (code / 16) as u16 as usize;
            if slot < fan {
                hist[slot] += 1;
            }
            let q: [i32; 3] = std::array::from_fn(|c| (i32::from(p[c]).max(0).wrapping_mul(k3[c])) >> 24);
            let idx = ((q[2] << 10) + (q[1] << 5) + q[0]) as usize & 0x7fff;
            hist3[idx] += 1;
        }
    }
    // cumulative distribution, normalised
    let mut cdf = vec![0.0f64; fan];
    let mut s = f64::from(hist[0]);
    cdf[0] = s;
    for k in 1..fan {
        s += f64::from(hist[k]);
        cdf[k] = s;
    }
    let total = cdf[fan - 1];
    for v in cdf.iter_mut() {
        *v /= total;
    }
    // percentile bins (each search continues where the previous one stopped)
    let mut bin = [0.0f64; 7];
    let mut k = 0usize;
    for (j, crit) in CRITERIA.iter().enumerate() {
        while k + 1 < fan && *crit > cdf[k] {
            k += 1;
        }
        bin[j] = k as f64;
    }
    // targets and percentiles as linear codes; the exposure gain matches them in a weighted sum
    let lin = |code: f64| -> f64 { f64::from(expo[(round_i(code) as u32 & 0xffff) as usize]) };
    let target_lin: [f64; 7] = std::array::from_fn(|j| lin(TARGETS[j] * 16.0));
    let actual_lin: [f64; 7] = std::array::from_fn(|j| lin(bin[j] * 16.0));
    let gain_factor = scalar(&G_BALANCE, &target_lin) / scalar(&G_BALANCE, &actual_lin);
    let gain_factor = if gain_factor < 0.0 { 7.245789314111254 } else if 7.245789314111254 < gain_factor { 7.245789314111254 } else { gain_factor };
    let e = gain_factor.powf(0.7);
    // remaining deviations after the exposure, on the 8-bit cube-root scale
    let dev: [f64; 7] = std::array::from_fn(|j| TARGETS[j] - f64::from(root[(round_i(actual_lin[j] * e) as u32 & 0xffff) as usize]) * 0.0625);
    let preset = slider(0.0);
    let exposure = slider(e.ln() / 0.6931471805599453 + scalar(&G_EXPOSURE, &dev));
    let contrast = slider(preset * 0.5 + scalar(&G_CONTRAST, &dev) * 0.005);
    let shadows = slider(scalar(&G_SHADOWS, &dev) * 0.004);
    let hl = scalar(&G_HIGHLIGHTS, &dev) * 0.003;
    let q = if hl < -1.9 {
        0.9
    } else if hl < -1.5 {
        0.8
    } else if hl < -1.0 {
        0.7
    } else {
        0.6
    };
    let highlights = slider(preset * q + hl);
    AutoTone { exposure, contrast, shadows, highlights, color: color_balance(&mut hist3, max) }
}

/// Colour balance from the colour histogram: 2×2×2 box sums, a weighted centroid (weight
/// `(n+1)^(1/8) − 1`), per-layer factors one over the square root of the centroid relative to its
/// smallest component, quantised to 0.01 decades.
fn color_balance(h: &mut [i32], max: [i32; 3]) -> [f64; 3] {
    let at = |b: usize, g: usize, r: usize| (b << 10) | (g << 5) | r;
    for r in 0..31 {
        for g in 0..31 {
            for b in 0..31 {
                h[at(b, g, r)] = h[at(b, g, r)].wrapping_add(h[at(b, g, r + 1)]);
            }
        }
    }
    for r in 0..31 {
        for g in 0..31 {
            for b in 0..31 {
                h[at(b, g, r)] = h[at(b, g, r)].wrapping_add(h[at(b, g + 1, r)]);
            }
        }
    }
    for r in 0..31 {
        for g in 0..31 {
            for b in 0..31 {
                h[at(b, g, r)] = h[at(b, g, r)].wrapping_add(h[at(b + 1, g, r)]);
            }
        }
    }
    let (mut sw, mut si, mut sj, mut sk) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 1..=32usize {
        for j in 1..=32usize {
            for k in 1..=32usize {
                let n = h[at(k - 1, j - 1, i - 1)];
                if n == 0 {
                    continue;
                }
                let g = f64::from((n as u32).wrapping_add(1)).powf(0.125) + -1.0;
                sw += g;
                sk += g * k as f64;
                si += g * i as f64;
                sj += g * j as f64;
            }
        }
    }
    let m = [(si / sw) * 0.03125, (sj / sw) * 0.03125, (sk / sw) * 0.03125];
    let c = [(f64::from(max[0]) * m[0] + 100.0) * 0.96, (f64::from(max[1]) * m[1] + 100.0) * 1.0, (f64::from(max[2]) * m[2] + 100.0) * 1.04];
    let kl = {
        let a = if c[0] < c[1] { c[0] } else { c[1] };
        if a < c[2] { a } else { c[2] }
    };
    let c = c.map(|v| v / kl);
    let f: [f64; 3] = std::array::from_fn(|i| if c[i] == f64::NEG_INFINITY { 0.0 } else { 1.0 / c[i].sqrt().abs() });
    let fm = maxsd(f[2], maxsd(f[0], f[1]));
    f.map(|v| {
        let level = tr32(0.3 - (v / fm).log10() * 100.0);
        10f64.powf(f64::from(level.wrapping_neg()) / 100.0)
    })
}

/// Tone curve per layer over the cube-root codes (index) to 14-bit linear output: exposure and
/// colour balance as slopes, a straight toe in the display-encoded domain, a power section with
/// the contrast as exponent and a smooth shoulder (highlights) above; sampled every 32 codes and
/// interpolated, constant above code 0x1fe0, zero for the negative half.
fn tone_curve(a: &AutoTone, expo: &[i16], low_iso: bool) -> [Vec<u16>; 3] {
    let hl = if low_iso { a.highlights + 0.8 } else { a.highlights };
    let con = ((a.contrast + 2.0) * 0.25) * 1.29125 + 0.64375;
    let hl = (hl + 2.0) * 0.25;
    let f = a.color;
    let mean = (f[1] * 0.5 + f[0] * 0.3) + f[2] * 0.2;
    let harm = ((1.0 / f[1]) * 0.5 + (1.0 / f[0]) * 0.3) + (1.0 / f[2]) * 0.2;
    let norm = (1.0 / harm + mean) * 0.5;
    let e = a.exposure.exp2();
    let stg = [e * (f[0] / norm), e * (f[1] / norm), (f[2] / norm) * e];
    let black = (((2.0 - a.shadows) * 0.25) * 2.0 + -1.0) * 0.025;
    let gain = 0.18f64.powf(1.0 - con);
    let k = 0.4166666666666667 * con;
    let s_par = hl * 0.44 + 0.66;
    let encode = |v: f64| if 0.00304 >= v { v * 12.923054468333259 } else { (v.powf(0.4166666666666667) + -0.05213316845261357) / 0.9478668315473864 };
    let decode = |v: f64| {
        let d = v / 12.923054468333259;
        if 0.00304 >= d { d } else { (v * 0.9478668315473864 + 0.05213316845261357).powf(2.4) }
    };
    let shoulder_n = 1.07f64.powf((hl * 1.5 + 1.0).powf(4.0));
    let power = |x: f64| -> f64 {
        let y = x.powf(con) * gain;
        let q = y.max(0.01).powf(0.22) * s_par;
        let z = y * q;
        let a1 = q.powf(shoulder_n);
        let a2 = y.powf(shoulder_n);
        (1.0 / (a2 + a1)).powf(1.0 / shoulder_n) * z
    };
    let (threshold, t0, slope) = if 1.0 > k {
        let d = 1.0 - k;
        let t0 = 0.06 * d;
        let u = 0.05213316845261357 + t0;
        let p = 0.18f64.powf((-1.0 + con) * 0.4166666666666667) * u / (d * 1.0521331684526136);
        let threshold = p.powf(1.0 / k);
        let yv = if 0.0 > threshold { decode(0.0f64.max(threshold * 0.0 + 0.0)) } else { power(threshold) };
        (threshold, t0, (encode(yv) - t0) / threshold)
    } else {
        (0.0, 0.0, 0.0)
    };
    let value = |x: f64| -> f64 {
        if threshold > x {
            let v = x * slope + t0;
            decode(if 0.0 > v { 0.0 } else { v })
        } else {
            power(x)
        }
    };
    let mut curve = [vec![0u16; 65536], vec![0u16; 65536], vec![0u16; 65536]];
    let mut before = [0.0f64; 3];
    let mut from_idx = 0usize;
    let mut i = 0usize;
    let mut last = [0i32; 3];
    while i < 0x2000 {
        let x = f64::from(expo[i]) * 0.000244140625 - black;
        let now: [f64; 3] = std::array::from_fn(|c| value(stg[c] * x + -0.005));
        if from_idx < i {
            let n = (i - (from_idx - 1)) as f64;
            for (a, idx) in (from_idx..i).enumerate() {
                let t = (a + 1) as f64 / n;
                for c in 0..3 {
                    let v = tr32((before[c] * (1.0 - t) + now[c] * t) * 4096.0 + 0.5);
                    curve[c][idx] = (if v >= 0x4000 { 0x4000 } else { v }) as u16;
                }
            }
        }
        for c in 0..3 {
            let v = tr32(now[c] * 4096.0 + 0.5);
            last[c] = if v >= 0x4001 { 0x4000 } else { v };
            curve[c][i] = last[c] as u16;
        }
        before = now;
        from_idx = i | 1;
        i += 0x20;
    }
    for c in 0..3 {
        for v in &mut curve[c][0x1fe1..0x8000] {
            *v = last[c] as u16;
        }
    }
    curve
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_wb_grey_image() {
        let (w, h) = (640usize, 480usize);
        let mut image = vec![0i32; w * h * 3];
        // a grey ramp with a little texture
        for y in 0..h {
            for x in 0..w {
                let v = 800 + ((x * 7 + y * 3) % 2000) as i32;
                for c in 0..3 {
                    image[(y * w + x) * 3 + c] = v;
                }
            }
        }
        let m: Mat = [0.4124, 0.3576, 0.1805, 0.2126, 0.7152, 0.0722, 0.0193, 0.1192, 0.9505];
        for (bv, short, flash) in [(8.0, true, false), (4.0, true, false), (5.0, true, false), (7.0, true, false), (9.0, true, false), (6.0, false, false), (6.0, true, true)] {
            let e = Input { image: &image, w, h, saturation: [16000; 3], post_polynomial: [0.0; 27], bv, flash, matrix: m, iso_factor: 1.0, offset: [0.0; 3], short };
            let n = neutral_point(&e);
            eprintln!("grey bv={bv} kurz={short} blitz={flash}: {n:?}");
            assert!(n.iter().all(|v| v.is_finite() && *v > 0.0), "{n:?}");
            let m = n.iter().cloned().fold(0.0, f64::max);
            assert!((m - 1.0).abs() < 1e-12);
        }
    }

    /// Real file (only when present): a daylight scene of the SD10; the estimate should be near
    /// the daylight neutral of that camera (about 0.88 / 1 / 0.92).
    #[test]
    fn auto_wb_sd10_sample() {
        use super::super::calibration;
        let f = crate::classic_new::check::classic_dir().join("sd10_pixls_RAW_SIGMA_SD10.X3F");
        if !f.exists() {
            eprintln!("skipped: no sample file");
            return;
        }
        let x = x3f::X3fFile::open(&f).unwrap();
        let (camf, props) = (x.camf().unwrap(), x.properties().unwrap());
        let (img, w, h) = crate::classic_new::chain::raw_values(&x).unwrap();
        let d = calibration::Data { camf: &camf };
        let k = calibration::calc(&calibration::Input { camf: &camf, props: &props, wb: "Auto", adobe: false, auto_neutral: None, gray_mean: None }).unwrap();
        let sat = d.triple("SaturationLevel", [16383.0; 3]).map(|v| v as i32);
        let mut pp = [0.0f32; 27];
        if let Some(v) = d.values("PostPolyMatrix").filter(|v| v.len() >= 27) {
            for i in 0..27 {
                pp[i] = v[i] as f32;
            }
        }
        let drift = d.values("RawDrift").unwrap_or_default();
        let offset: [f64; 3] = std::array::from_fn(|c| if drift.len() >= 12 { (drift[1 + 2 * c] + drift[7 + 2 * c]) / 2.0 } else { 0.0 });
        eprintln!("SD10 {w}×{h} sat={sat:?} offset={offset:?} stored neutral={:?}", k.color.neutral);
        let e = Input { image: &img, w, h, saturation: sat, post_polynomial: pp, bv: 8.0, flash: false, matrix: k.color.m_raw, iso_factor: 1.0, offset, short: true };
        let t = std::time::Instant::now();
        let n = neutral_point(&e);
        eprintln!("SD10 auto neutral: {n:?} ({:.1} ms)", t.elapsed().as_secs_f64() * 1000.0);
        assert!(n.iter().all(|v| v.is_finite() && (0.7..=1.1).contains(v)), "{n:?}");
    }

    #[test]
    fn auto_wb_polygon_position() {
        assert_eq!(position(&POLY_BRIGHT, (10000, 10000)), b'i');
        assert_eq!(position(&POLY_BRIGHT, (20000, 10000)), b'o');
        assert_eq!(position(&POLY_BRIGHT, (9700, 9700)), b'v');
        let q = to_border(&POLY_BRIGHT, (20000, 10000));
        assert_ne!(position(&POLY_BRIGHT, q), b'o', "{q:?}");
    }
}
