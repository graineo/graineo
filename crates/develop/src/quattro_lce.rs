//! Quattro: local contrast stage of Sigma's processing (third stage, before the tone curve; Sigma way of the step
//! `quattroColor`).
//!
//! Sigma's processing raises the local contrast of Quattro images on the lightness only: the linear RGB image
//! (16383 = white) goes to CIE L*a*b*, the L* plane runs through a six-level wavelet transform (lifting 5/3, scale
//! √2 on the low band), the detail bands of each level are amplified with a gain that grows with the level's
//! spatial scale (a fifth-order polynomial in the logarithm of the scale), soft-limited around the original
//! magnitude (limit and slope per level), the smallest low band gets a mild unsharp mask, and after the inverse
//! transform the new L* is kept within ±35 % of the old one; a* and b* stay. This lifts contrast in mid tones and
//! darkens small dark areas, also visible in block means (shadows down by a few per cent).
//!
//! Own implementation from our reading of Sigma's processing (the gain polynomials per detail step; the unsharp mask of the smallest band keeps the amount of the standard step).

use rayon::prelude::*;

/// Gain polynomial (powers 0…5 of x = ln √(2^(l+2) − 2)) per detail step 0 (soft) … 6 (strong); 3 is the camera's
/// setting.
const POLY: [[f64; 6]; 7] = [
    [0.994647, 0.11335, -0.03233, 0.004454, -0.000282, 0.000007],
    [1.099294, 0.358327, -0.668527, 0.635983, -0.27939, 0.046668],
    [1.121631, 0.459364, -0.889268, 0.865613, -0.385829, 0.064953],
    [1.144235, 0.567012, -1.129763, 1.120466, -0.50573, 0.085779],
    [1.156549, 0.66235, -1.403245, 1.473315, -0.693201, 0.121302],
    [1.167312, 0.717967, -1.537551, 1.626538, -0.769131, 0.135002],
    [1.176575, 0.766115, -1.653808, 1.759424, -0.835123, 0.146939],
];
/// The camera's detail step.
pub const CAMERA_STEP: usize = 3;
/// Soft limit and slope per level (level 0 = coarsest band).
const LIMIT: [f32; 6] = [100.0, 100.0, 100.0, 100.0, 25.0, 25.0];
const SLOPE: [f32; 6] = [0.5, 0.55, 0.6, 0.6, 0.7, 0.8];
const MAX_GAIN: f64 = 10.0;
/// Unsharp mask of the smallest low band: base 0.15 × 0.3 (full resolution), × 0.8 for Portrait.
const LOWEST_AMOUNT: f32 = 0.045;
const RELATIVE_BOUND: f32 = 0.35;
const LEVELS: usize = 6;

const RGB2XYZ: [f64; 9] = [
    0.41239558896741424, 0.3575834307637148, 0.18049264738170157, 0.21258623078559555, 0.7151703037034108, 0.07220049864333623,
    0.019297215491746945, 0.11918386458084854, 0.9504971251315798,
];
const L_SCALE: f64 = 81.915;
const A_SCALE: f64 = 500.0 * 27.305;
const B_SCALE: f64 = 200.0 * 54.61;

fn f(t: f64) -> f64 {
    if t < 0.008856452 { t * 7.787037 + 0.137931 } else { t.max(0.0).cbrt() }
}
fn finv(v: f64) -> f64 {
    if v < 0.20689655 { (v - 0.137931) / 7.787037 } else { v * v * v }
}

fn gain(level: usize, step: usize) -> f32 {
    let s = 2f64.powi(level as i32 + 1);
    let x = (s - 1.0 + s - 1.0).sqrt().ln();
    let g: f64 = POLY[step.min(6)].iter().enumerate().map(|(k, c)| c * x.powi(k as i32)).sum();
    g.clamp(1.0, MAX_GAIN) as f32
}

/// Soft-limited amplification of one detail coefficient.
#[inline]
fn amplify(c: f32, g: f32, t: f32, s: f32) -> f32 {
    let a = c.abs();
    let mut y = g * a;
    let (lo, hi) = (a - t, a + t);
    if y < lo {
        y = lo - (lo - y) * s;
    }
    if y > hi {
        y = (y - hi) * s + hi;
    }
    if c < 0.0 { -y } else { y }
}

/// One 5/3 lifting step along a line of `n` (even) samples with stride `st`, in place: low band first, then high.
fn forward_line(buf: &mut [f32], tmp: &mut Vec<f32>, n: usize) {
    let h = n / 2;
    tmp.clear();
    tmp.resize(n, 0.0);
    let (lo, hi) = tmp.split_at_mut(h);
    for i in 0..h {
        let e = buf[2 * i];
        let en = buf[(2 * i + 2).min(n - 2)];
        hi[i] = buf[2 * i + 1] - 0.5 * (e + en);
    }
    for i in 0..h {
        let dp = if i == 0 { hi[0] } else { hi[i - 1] };
        lo[i] = (buf[2 * i] + 0.25 * (dp + hi[i])) * std::f32::consts::SQRT_2;
    }
    buf[..n].copy_from_slice(tmp);
}

fn inverse_line(buf: &mut [f32], tmp: &mut Vec<f32>, n: usize) {
    let h = n / 2;
    tmp.clear();
    tmp.resize(n, 0.0);
    for i in 0..h {
        let dp = if i == 0 { buf[h] } else { buf[h + i - 1] };
        tmp[2 * i] = buf[i] / std::f32::consts::SQRT_2 - 0.25 * (dp + buf[h + i]);
    }
    for i in 0..h {
        let e = tmp[2 * i];
        let en = tmp[(2 * i + 2).min(n - 2)];
        tmp[2 * i + 1] = buf[h + i] + 0.5 * (e + en);
    }
    buf[..n].copy_from_slice(tmp);
}

/// Transform of the top-left `w × h` area of a `stride`-wide plane: columns (vertical) then rows.
fn transform(p: &mut [f32], stride: usize, w: usize, h: usize, inverse: bool) {
    let rows = |p: &mut [f32]| {
        p.par_chunks_mut(stride).take(h).for_each_init(Vec::new, |tmp, line| if inverse { inverse_line(line, tmp, w) } else { forward_line(line, tmp, w) });
    };
    let cols = |p: &mut [f32]| {
        // gather columns into rows of a transposed buffer
        let mut t = vec![0f32; w * h];
        t.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
            for y in 0..h {
                col[y] = p[y * stride + x];
            }
        });
        t.par_chunks_mut(h).for_each_init(Vec::new, |tmp, col| if inverse { inverse_line(col, tmp, h) } else { forward_line(col, tmp, h) });
        p.par_chunks_mut(stride).take(h).enumerate().for_each(|(y, line)| {
            for x in 0..w {
                line[x] = t[x * h + y];
            }
        });
    };
    if inverse {
        rows(p);
        cols(p);
    } else {
        cols(p);
        rows(p);
    }
}

/// L* (Sigma's 16-bit scale, 100 = 8191.5) → contrast-raised L*.
fn lce_plane(l: &[f32], w: usize, h: usize, portrait: bool, step: usize) -> Vec<f32> {
    let wp = w.div_ceil(128) * 128;
    let hp = h.div_ceil(128) * 128;
    let mut p = vec![0f32; wp * hp];
    p.par_chunks_mut(wp).enumerate().for_each(|(y, line)| {
        let src = &l[y.min(h - 1) * w..y.min(h - 1) * w + w];
        line[..w].copy_from_slice(src);
        let last = src[w - 1];
        line[w..].fill(last);
    });
    let (mut cw, mut ch) = (wp, hp);
    for _ in 0..LEVELS {
        transform(&mut p, wp, cw, ch, false);
        cw /= 2;
        ch /= 2;
    }
    for lev in 0..LEVELS {
        let g = gain(lev, step);
        let (t, s) = (LIMIT[lev], SLOPE[lev]);
        let (bw, bh) = (wp >> (LEVELS - lev), hp >> (LEVELS - lev));
        p.par_chunks_mut(wp).take(2 * bh).enumerate().for_each(|(y, line)| {
            let x0 = if y < bh { bw } else { 0 };
            for v in &mut line[x0..2 * bw] {
                *v = amplify(*v, g, t, s);
            }
        });
    }
    // smallest low band: 3 × 3 Gaussian (σ = 3) unsharp mask
    let (bw, bh) = (wp >> LEVELS, hp >> LEVELS);
    let k1 = (-0.5f32 / 9.0).exp();
    let kern = [k1, 1.0, k1];
    let norm: f32 = kern.iter().map(|a| kern.iter().map(|b| a * b).sum::<f32>()).sum();
    let amount = if portrait { LOWEST_AMOUNT * 0.8 } else { LOWEST_AMOUNT };
    let ll: Vec<f32> = (0..bh).flat_map(|y| p[y * wp..y * wp + bw].to_vec()).collect();
    for y in 0..bh {
        for x in 0..bw {
            let mut b = 0.0;
            for (dy, ky) in kern.iter().enumerate() {
                for (dx, kx) in kern.iter().enumerate() {
                    let yy = (y + dy).saturating_sub(1).min(bh - 1);
                    let xx = (x + dx).saturating_sub(1).min(bw - 1);
                    b += ky * kx * ll[yy * bw + xx];
                }
            }
            let v = ll[y * bw + x];
            p[y * wp + x] = v + amount * (v - b / norm);
        }
    }
    let (mut cw, mut ch) = (bw, bh);
    for _ in 0..LEVELS {
        cw *= 2;
        ch *= 2;
        transform(&mut p, wp, cw, ch, true);
    }
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        for x in 0..w {
            let old = l[y * w + x];
            let (a, b) = (old * (1.0 - RELATIVE_BOUND), old * (1.0 + RELATIVE_BOUND));
            let v = p[y * wp + x];
            o[x] = if v < a.min(b) { a.min(b) } else if v > a.max(b) { a.max(b) } else { v };
        }
    });
    out
}

/// Applies the stage in place to interleaved linear RGB counts of size `w × h`; `scale` maps them to the units of
/// Sigma's processing (16383 = white).
pub fn apply(rgb: &mut [f32], w: usize, h: usize, portrait: bool, scale: f32) {
    apply_step(rgb, w, h, portrait, scale, CAMERA_STEP)
}

/// [`apply`] at the detail step `step` (0 = soft … 6 = strong, [`CAMERA_STEP`] = the camera's setting): the step
/// chooses the gain polynomial of the bands.
pub fn apply_step(rgb: &mut [f32], w: usize, h: usize, portrait: bool, scale: f32, step: usize) {
    if w < 2 || h < 2 {
        return;
    }
    let wn: [f64; 3] = std::array::from_fn(|r| RGB2XYZ[r * 3] + RGB2XYZ[r * 3 + 1] + RGB2XYZ[r * 3 + 2]);
    let lab: Vec<[f32; 3]> = rgb
        .par_chunks(3)
        .map(|p| {
            let k = scale as f64 / 16383.0;
            let c = [p[0] as f64 * k, p[1] as f64 * k, p[2] as f64 * k];
            let xyz: [f64; 3] = std::array::from_fn(|r| (RGB2XYZ[r * 3] * c[0] + RGB2XYZ[r * 3 + 1] * c[1] + RGB2XYZ[r * 3 + 2] * c[2]) / wn[r]);
            let (fx, fy, fz) = (f(xyz[0]), f(xyz[1]), f(xyz[2]));
            [((116.0 * fy - 16.0) * L_SCALE) as f32, ((fx - fy) * A_SCALE) as f32, ((fy - fz) * B_SCALE) as f32]
        })
        .collect();
    let l: Vec<f32> = lab.iter().map(|v| v[0]).collect();
    let nl = lce_plane(&l, w, h, portrait, step);
    let inv = crate::matrix::Mat3(RGB2XYZ).inverse().expect("sRGB matrix invertible").0;
    rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
        let fy = (nl[i] as f64 / L_SCALE + 16.0) / 116.0;
        let fx = fy + lab[i][1] as f64 / A_SCALE;
        let fz = fy - lab[i][2] as f64 / B_SCALE;
        let xyz = [finv(fx) * wn[0], finv(fy) * wn[1], finv(fz) * wn[2]];
        for r in 0..3 {
            p[r] = ((inv[r * 3] * xyz[0] + inv[r * 3 + 1] * xyz[1] + inv[r * 3 + 2] * xyz[2]) * 16383.0 / scale as f64) as f32;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wavelet_roundtrip() {
        let (w, h) = (256, 128);
        let mut p: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 101) as f32).collect();
        let orig = p.clone();
        let (mut cw, mut ch) = (w, h);
        for _ in 0..LEVELS {
            transform(&mut p, w, cw, ch, false);
            cw /= 2;
            ch /= 2;
        }
        for _ in 0..LEVELS {
            cw *= 2;
            ch *= 2;
            transform(&mut p, w, cw, ch, true);
        }
        let err = p.iter().zip(&orig).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-2, "{err}");
    }

    #[test]
    fn flat_image_unchanged() {
        let (w, h) = (200, 150);
        let mut rgb = vec![4000f32; w * h * 3];
        apply(&mut rgb, w, h, false, 1.0);
        assert!(rgb.iter().all(|v| (v - 4000.0).abs() < 1.0));
    }
}
