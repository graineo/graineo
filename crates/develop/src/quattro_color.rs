//! Quattro: own colour stage.
//!
//! The plain colour matrix of a Foveon camera multiplies the noise of the three layers into strong chroma noise,
//! most visibly in dark and nearly grey areas. This stage forms the output colour in four steps:
//!
//! 1. **Matrix blend** ([`ColorLevel::pixel`]): each pixel is a mix of the matrixed colour and the white-balanced
//!    camera colour (both at the same brightness). The share of the camera colour is close to 1 for dark and little
//!    saturated pixels and falls off with the light level and the saturation (a logistic curve in the level whose
//!    threshold depends on saturation and hue). Small values of the matrixed colour are cored towards zero
//!    (soft `tanh` threshold that fades out above a few raw counts). Channel values below zero (noise around
//!    black) count by their magnitude.
//! 2. **Chroma outlier clamp** ([`chroma_limit`]): the chroma (U, V of BT.601 YUV) of each pixel is limited to the
//!    range of its 3 × 3 neighbours (neighbourhood clamp as used against impulse noise). Brightness stays.
//! 3. **Colour mode fine correction** ([`Axes`], sd Quattro only): the camera stores per colour mode a hue shift
//!    and a saturation factor at 21 hue nodes (CAMF `MultiAxisTable_<mode>`). The chroma (Cb, Cr of BT.601 YCbCr)
//!    is rotated and scaled by the values interpolated linearly between the nodes; luma and grey stay.
//! 4. **Local contrast** ([`local_contrast`], below capture ISO 800): the lightness L* is split into a Laplacian
//!    pyramid (Burt and Adelson 1983); the detail values of each level are amplified with a gain per level that fades
//!    out for strong edges; a* and b* stay.
//!
//! The parameters were fitted so that synthetic and real camera colours come out as in the camera maker's software,
//! and the developed results were checked against its TIFF exports. The fine correction takes its values from the
//! camera file.
//!
//! Units: white-balanced camera counts as for the colour matrix; light levels refer to the saturation of the middle
//! layer divided by SensorISO/100, the coring to the counts before the digital ISO gain.

use rayon::prelude::*;

/// BT.601 luma and colour differences (rows Y, U, V).
const YUV: [f32; 9] = [0.299, 0.587, 0.114, -0.14713, -0.28886, 0.436, 0.615, -0.51499, -0.10001];

/// Chroma outlier clamp: U and V of each pixel limited to the range of its `(2r+1)²` neighbours without the pixel
/// itself, optionally after dropping the `rank` most extreme neighbours on each side (`rank = 0`: plain
/// minimum/maximum, the own stage's choice).
/// `rgb` interleaved, in place; `r = 0` does nothing.
pub fn chroma_limit(rgb: &mut [f32], w: usize, h: usize, r: usize, rank: usize) {
    const MAX_RANK: usize = 3;
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let rank = rank.min(MAX_RANK).min((2 * r + 1) * (2 * r + 1) - 2);
    let inv = crate::matrix::Mat3(YUV.map(f64::from)).inverse().expect("YUV invertible").to_f32();
    let uv: Vec<[f32; 2]> = rgb.par_chunks(3).map(|p| [1, 2].map(|k| YUV[k * 3] * p[0] + YUV[k * 3 + 1] * p[1] + YUV[k * 3 + 2] * p[2])).collect();
    let r = r as isize;
    rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, line)| {
        for x in 0..w {
            let p = &mut line[x * 3..x * 3 + 3];
            let luma = YUV[0] * p[0] + YUV[1] * p[1] + YUV[2] * p[2];
            let mut q = uv[y * w + x];
            for (ch, v) in q.iter_mut().enumerate() {
                // the rang+1 smallest (ascending) and largest (descending) neighbour values
                let (mut lo, mut hi) = ([f32::MAX; MAX_RANK + 1], [f32::MIN; MAX_RANK + 1]);
                for dy in -r..=r {
                    let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    for dx in -r..=r {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let n = uv[yy * w + (x as isize + dx).clamp(0, w as isize - 1) as usize][ch];
                        if n < lo[rank] {
                            let mut i = rank;
                            while i > 0 && n < lo[i - 1] {
                                lo[i] = lo[i - 1];
                                i -= 1;
                            }
                            lo[i] = n;
                        }
                        if n > hi[rank] {
                            let mut i = rank;
                            while i > 0 && n > hi[i - 1] {
                                hi[i] = hi[i - 1];
                                i -= 1;
                            }
                            hi[i] = n;
                        }
                    }
                }
                *v = v.clamp(lo[rank], hi[rank]);
            }
            let yuv = [luma, q[0], q[1]];
            for k in 0..3 {
                p[k] = inv[k * 3] * yuv[0] + inv[k * 3 + 1] * yuv[1] + inv[k * 3 + 2] * yuv[2];
            }
        }
    });
}

/// Colour mode fine correction of the sd Quattro from the camera's table (see module): hue shift (degrees) and
/// saturation factor at the hue nodes 0°, 18°, …, 360° of the chroma plane (0° = +Cb, 90° = +Cr).
#[derive(Clone, Debug, PartialEq)]
pub struct Axes {
    shift: [f32; 21],
    sat: [f32; 21],
}

/// BT.601 YCbCr with 8-bit coefficients (as in JPEG files): rows Y, Cb, Cr.
const YCC: [f32; 9] = [77.0 / 256.0, 150.0 / 256.0, 29.0 / 256.0, -43.0 / 256.0, -85.0 / 256.0, 128.0 / 256.0, 128.0 / 256.0, -107.0 / 256.0, -21.0 / 256.0];

impl Axes {
    /// From the CAMF table (210 values: hue shifts of 5 rows × 21 nodes, then the saturation factors); the first row
    /// of each group is used (the rows are equal in the known files). `None` for an identity table or a wrong size.
    pub fn from_table(v: &[f64]) -> Option<Self> {
        if v.len() != 210 || !v.iter().all(|x| x.is_finite()) {
            return None;
        }
        let (shift, sat) = (&v[0..21], &v[105..126]);
        if shift.iter().all(|s| s.abs() < 1e-9) && sat.iter().all(|s| (s - 1.0).abs() < 1e-9) {
            return None;
        }
        Some(Self { shift: std::array::from_fn(|i| shift[i] as f32), sat: std::array::from_fn(|i| sat[i] as f32) })
    }

    /// Apply to one pixel (linear or encoded, any scale; luma and grey stay).
    #[inline]
    pub fn apply(&self, p: &mut [f32]) {
        let ycc: [f32; 3] = std::array::from_fn(|k| YCC[k * 3] * p[0] + YCC[k * 3 + 1] * p[1] + YCC[k * 3 + 2] * p[2]);
        let (cb, cr) = (ycc[1], ycc[2]);
        if cb == 0.0 && cr == 0.0 {
            return;
        }
        let deg = cr.atan2(cb).to_degrees().rem_euclid(360.0);
        let i = ((deg / 18.0) as usize).min(19);
        let t = (deg - 18.0 * i as f32) / 18.0;
        let shift = self.shift[i] + t * (self.shift[i + 1] - self.shift[i]);
        let sat = self.sat[i] + t * (self.sat[i + 1] - self.sat[i]);
        let (s, c) = shift.to_radians().sin_cos();
        let (cb2, cr2) = (sat * (c * cb - s * cr), sat * (s * cb + c * cr));
        let (dcb, dcr) = (cb2 - cb, cr2 - cr);
        // back to RGB: only the chroma changed (inverse of YCC applied to the difference)
        let d = ycc_inverse();
        for k in 0..3 {
            p[k] += d[k * 3 + 1] * dcb + d[k * 3 + 2] * dcr;
        }
    }

    /// Apply to gamma-encoded 16-bit pixels (interleaved RGB).
    pub fn apply_u16(&self, px: &mut [u16]) {
        for q in px.chunks_exact_mut(3) {
            let mut p = [q[0] as f32, q[1] as f32, q[2] as f32];
            self.apply(&mut p);
            for k in 0..3 {
                q[k] = p[k].round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

fn ycc_inverse() -> &'static [f32; 9] {
    static INV: std::sync::OnceLock<[f32; 9]> = std::sync::OnceLock::new();
    INV.get_or_init(|| crate::matrix::Mat3(YCC.map(f64::from)).inverse().expect("YCbCr invertible").to_f32())
}

/// Parameters of the matrix blend for one class of images (fitted, see module): share of the camera colour
/// `1 / (1 + (q / q50')^n)` with the light level `q` and `q50' = q50 · (s + s0)^(−e) · hue(hue)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mix {
    /// light level at which a fully saturated pixel keeps half of the camera colour
    pub q50: f32,
    /// saturation offset and exponent of the threshold: `q50 · (s + s0)^(−e)`
    pub s0: f32,
    pub e: f32,
    /// steepness of the transition
    pub n: f32,
    /// hue weight of the threshold at red, yellow, green, cyan, blue, magenta (linear in between)
    pub hue: [f32; 6],
}

/// Compact cameras (dp Quattro).
pub const MIX_DP: Mix = Mix { q50: 0.000525, s0: 0.0215, e: 0.757, n: 1.964, hue: [0.946, 1.047, 1.133, 1.169, 0.785, 0.972] };
/// Interchangeable-lens cameras (sd Quattro, sd Quattro H) at full size.
pub const MIX_SD: Mix = Mix { q50: 0.001326, s0: 0.0186, e: 0.621, n: 2.263, hue: [0.737, 1.114, 1.231, 1.065, 0.709, 1.309] };
/// sd Quattro (H) binned (high ISO, see [`crate::quattro::binning_factor`]): less chroma noise per pixel. Fitted again
/// after the own noise reduction of the colour grid ([`crate::quattro_noise`]): on real binned images (sd Quattro
/// and sd Quattro H at ISO 6400) the own colour stage was fitted to the camera maker's colour stage on the same
/// denoised grid (8 × 8 block colour); checked on two further binned sd Quattro images (ISO 800, 1600).
pub const MIX_SD_BINNED: Mix = Mix { q50: 0.000539, s0: 0.1877, e: 1.383, n: 1.611, hue: [0.695, 0.778, 0.713, 1.686, 0.605, 1.199] };

/// Coring of the matrixed colour (fitted, see module): each channel `t` (in raw signal units `u = t / gain`) moves
/// towards zero by `A · tanh(u / B)`, fading out linearly up to `|u| = U`.
pub const KERN_A: f32 = 9.07;
pub const KERN_B: f32 = 8.09;
pub const KERN_U: f32 = 53.7;

/// Own colour stage of one image (see module).
#[derive(Clone, Debug)]
pub struct ColorLevel {
    /// white-balanced camera counts → output counts (colour matrix with ISO factor)
    pub matrix: [f32; 9],
    /// ISO factor (camera colour at the brightness of the matrixed colour)
    pub iso: f32,
    /// counts → light level: 1 / (saturation of the middle layer in counts · SensorISO/100)
    pub level: f32,
    /// output counts → raw signal units of the coring: 1 / (ISO factor · SensorISO/100)
    pub signal: f32,
    pub mix: Mix,
}

impl ColorLevel {
    /// Share of the camera colour for the white-balanced camera colour `v` (counts).
    #[inline]
    pub fn fraction(&self, v: [f32; 3]) -> f32 {
        let c = v.map(f32::abs);
        let max = c[0].max(c[1]).max(c[2]);
        if max <= 0.0 {
            return 1.0;
        }
        let min = c[0].min(c[1]).min(c[2]);
        let d = max - min;
        let s = d / max;
        // hue 0 … 6 (red, yellow, green, cyan, blue, magenta)
        let hue = if d <= 0.0 {
            0.0
        } else if c[0] == max {
            ((c[1] - c[2]) / d).rem_euclid(6.0)
        } else if c[1] == max {
            (c[2] - c[0]) / d + 2.0
        } else {
            (c[0] - c[1]) / d + 4.0
        };
        let m = &self.mix;
        let i = (hue as usize).min(5);
        let f = hue - i as f32;
        // geometric interpolation between the hue nodes
        let wt = (m.hue[i].ln() * (1.0 - f) + m.hue[(i + 1) % 6].ln() * f).exp();
        let q50 = m.q50 * (s + m.s0).powf(-m.e) * wt;
        let q = max * self.level;
        1.0 / (1.0 + (q / q50).powf(m.n))
    }

    /// One pixel: white-balanced camera counts → output counts. Negative values (noise below black) count by their
    /// magnitude, as in the camera maker's output.
    #[inline]
    pub fn pixel(&self, v: [f32; 3]) -> [f32; 3] {
        let v = v.map(f32::abs);
        let m = &self.matrix;
        let t: [f32; 3] = std::array::from_fn(|r| m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2]);
        let a = self.fraction(v);
        std::array::from_fn(|k| {
            let u = t[k] * self.signal;
            let fade = (1.0 - u.abs() / KERN_U).max(0.0);
            let core = t[k] - KERN_A * (u / KERN_B).tanh() * fade / self.signal;
            core + a * (self.iso * v[k] - core)
        })
    }
}

/// Gains of the local contrast per pyramid level (finest first) and the detail amplitude (L*) above which the gain
/// fades out (fitted, see module).
pub const CONTRAST_GAINS: [f32; 7] = [1.443, 1.13, 1.214, 1.202, 1.083, 1.006, 0.993];
pub const CONTRAST_SOFT: f32 = 20.7;

/// sRGB (D65) → XYZ (IEC 61966-2-1).
const RGB2XYZ: [f64; 9] = [0.4124564, 0.3575761, 0.1804375, 0.2126729, 0.7151522, 0.0721750, 0.0193339, 0.1191920, 0.9503041];

fn lab_f(t: f64) -> f64 {
    const D: f64 = 6.0 / 29.0;
    if t > D * D * D { t.cbrt() } else { t / (3.0 * D * D) + 4.0 / 29.0 }
}

fn lab_finv(v: f64) -> f64 {
    const D: f64 = 6.0 / 29.0;
    if v > D { v * v * v } else { 3.0 * D * D * (v - 4.0 / 29.0) }
}

/// Local contrast on L* (see module) of linear RGB counts `rgb` (`w × h`, interleaved, in place); `white`: counts of
/// L* = 100.
pub fn local_contrast(rgb: &mut [f32], w: usize, h: usize, white: f32, gains: &[f32], soft: f32) {
    if w < 2 || h < 2 || gains.is_empty() {
        return;
    }
    let wn: [f64; 3] = std::array::from_fn(|r| RGB2XYZ[r * 3] + RGB2XYZ[r * 3 + 1] + RGB2XYZ[r * 3 + 2]);
    let k = 1.0 / white as f64;
    let lab: Vec<[f32; 3]> = rgb
        .par_chunks(3)
        .map(|p| {
            let f: [f64; 3] = std::array::from_fn(|r| lab_f((RGB2XYZ[r * 3] * p[0] as f64 + RGB2XYZ[r * 3 + 1] * p[1] as f64 + RGB2XYZ[r * 3 + 2] * p[2] as f64) * k / wn[r]));
            [(116.0 * f[1] - 16.0) as f32, (500.0 * (f[0] - f[1])) as f32, (200.0 * (f[1] - f[2])) as f32]
        })
        .collect();
    let l: Vec<f32> = lab.iter().map(|v| v[0]).collect();
    let nl = pyramid_amplify(&l, w, h, gains, soft);
    let inv = crate::matrix::Mat3(RGB2XYZ).inverse().expect("sRGB matrix invertible").0;
    rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
        let fy = (nl[i] as f64 + 16.0) / 116.0;
        let xyz = [lab_finv(fy + lab[i][1] as f64 / 500.0) * wn[0], lab_finv(fy) * wn[1], lab_finv(fy - lab[i][2] as f64 / 200.0) * wn[2]];
        for r in 0..3 {
            p[r] = ((inv[r * 3] * xyz[0] + inv[r * 3 + 1] * xyz[1] + inv[r * 3 + 2] * xyz[2]) / k) as f32;
        }
    });
}

/// 5-tap binomial blur (1 4 6 4 1)/16, separable, edges repeated.
fn binomial(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..5).map(|i| K[i] * s[(x + i).saturating_sub(2).min(w - 1)]).sum();
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..5).map(|i| K[i] * tmp[(y + i).saturating_sub(2).min(h - 1) * w + x]).sum();
        }
    });
    out
}

fn shrink(src: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let b = binomial(src, w, h);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let out = (0..w2 * h2).into_par_iter().map(|i| b[(i / w2) * 2 * w + (i % w2) * 2]).collect();
    (out, w2, h2)
}

/// 2× enlargement: values on the even positions, binomial interpolation in between (normalised by the weights that
/// fall on known values, so a constant stays constant up to the edges).
fn enlarge(src: &[f32], w2: usize, w: usize, h: usize) -> Vec<f32> {
    let mut z = vec![0f32; w * h];
    let mut m = vec![0f32; w * h];
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            z[y * w + x] = src[(y / 2) * w2 + x / 2];
            m[y * w + x] = 1.0;
        }
    }
    let (z, m) = (binomial(&z, w, h), binomial(&m, w, h));
    z.iter().zip(&m).map(|(a, b)| a / b).collect()
}

/// Laplacian pyramid of `l` with each detail value `d` of level `i` multiplied by `1 + (gains[i] − 1) / (1 + (d/soft)²)`
/// (finest level first): small details are amplified fully, strong edges less.
fn pyramid_amplify(l: &[f32], w: usize, h: usize, gains: &[f32], soft: f32) -> Vec<f32> {
    let mut levels: Vec<(Vec<f32>, usize, usize)> = vec![(l.to_vec(), w, h)];
    for _ in 0..gains.len() {
        let (a, aw, ah) = levels.last().unwrap();
        if *aw < 2 || *ah < 2 {
            break;
        }
        let next = shrink(a, *aw, *ah);
        levels.push(next);
    }
    let (mut acc, mut aw, _) = levels.pop().unwrap();
    while let Some((a, lw, lh)) = levels.pop() {
        let g = gains[levels.len()];
        let (lo, _, _) = shrink(&a, lw, lh);
        let up_lo = enlarge(&lo, aw, lw, lh);
        let up_acc = enlarge(&acc, aw, lw, lh);
        acc = (0..lw * lh)
            .into_par_iter()
            .map(|i| {
                let d = a[i] - up_lo[i];
                up_acc[i] + d * (1.0 + (g - 1.0) / (1.0 + (d / soft) * (d / soft)))
            })
            .collect();
        aw = lw;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyramid_with_unit_gains_is_identity() {
        let (w, h) = (97, 61);
        let l: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 101) as f32).collect();
        let out = pyramid_amplify(&l, w, h, &[1.0; 7], CONTRAST_SOFT);
        let err = out.iter().zip(&l).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-2, "{err}");
    }

    #[test]
    fn flat_image_keeps_contrast_stage_unchanged() {
        let (w, h) = (64, 48);
        let mut rgb = vec![4000f32; w * h * 3];
        local_contrast(&mut rgb, w, h, 16384.0, &CONTRAST_GAINS, CONTRAST_SOFT);
        assert!(rgb.iter().all(|v| (v - 4000.0).abs() < 0.5), "{:?}", &rgb[..3]);
    }

    #[test]
    fn blend_keeps_camera_colour_in_the_dark_only() {
        let st = ColorLevel { matrix: [1.6, -0.4, -0.2, -0.3, 1.5, -0.2, -0.1, -0.4, 1.5], iso: 1.0, level: 1.0 / 16384.0, signal: 1.0, mix: MIX_DP };
        assert!(st.fraction([3.0, 2.0, 1.5]) > 0.9);
        assert!(st.fraction([3000.0, 2000.0, 1500.0]) < 0.01);
        // grey stays grey either way
        let g = st.pixel([5.0, 5.0, 5.0]);
        assert!((g[0] - g[1]).abs() < 1e-3 && (g[1] - g[2]).abs() < 1e-3);
    }

    #[test]
    fn clamp_removes_single_chroma_outlier() {
        let (w, h) = (9, 9);
        let mut rgb = vec![100f32; w * h * 3];
        rgb[(4 * w + 4) * 3] = 160.0;
        chroma_limit(&mut rgb, w, h, 1, 0);
        let p = &rgb[(4 * w + 4) * 3..(4 * w + 4) * 3 + 3];
        assert!((p[0] - p[1]).abs() < 1e-2 && (p[1] - p[2]).abs() < 1e-2, "{p:?}");
    }

    #[test]
    fn fine_correction_keeps_grey_and_luma() {
        let mut t = vec![0.0; 210];
        t[105..].fill(1.0);
        for r in 0..5 {
            t[r * 21 + 7] = 5.0;
            t[105 + r * 21 + 12] = 1.2;
        }
        let a = Axes::from_table(&t).unwrap();
        let mut g = [300.0f32, 300.0, 300.0];
        a.apply(&mut g);
        assert!(g.iter().all(|v| (v - 300.0).abs() < 1e-3));
        let mut p = [400.0f32, 300.0, 100.0];
        let y = |p: &[f32; 3]| YCC[0] * p[0] + YCC[1] * p[1] + YCC[2] * p[2];
        let y0 = y(&p);
        a.apply(&mut p);
        assert!((y(&p) - y0).abs() < 1e-2);
    }
}
