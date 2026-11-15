//! Classic chain, step "log ratio" (low ISO): the ratio between the matrix result and
//! the cored layers is smoothed in the logarithmic domain with an edge-aware recursive filter and
//! blended back, which removes colour noise without smearing edges.
//!
//! Outline (all float arithmetic in single precision, in a fixed order):
//!  1. Both code images are clamped to `1 … S-1` (S = `9000 / ISO gain 2 · 4`, at most 24000).
//!  2. `mu` = layers^exponent (table). The gradient magnitude of `mu` over a cross of neighbours
//!     (rows 1 … h-3 only, the border copies the neighbour) is turned into a soft edge weight by a
//!     sigmoid table whose width follows the standard deviation of the gradient.
//!  3. `mu` is smoothed with a two-pass (horizontal, vertical) recursive filter of constant weight;
//!     the gradient of the smoothed `mu` gives a second weight map, the two maps are merged with
//!     the maximum.
//!  4. The log ratio `ln(codes) − ln(layers)` (per layer) is smoothed by the same kind of filter
//!     but with the per-pixel weight map as coupling between neighbours.
//!  5. The weight map itself is smoothed (constant weight) and decides how much of the original
//!     codes stays: `result = (2·codes − E)·w + E·gain` with `E = exp(ln(layers) + ratio)`.
//!
//! The filters of the first and the last stage do not process the last line of their input; the
//! corresponding border values keep their previous
//! content. The result is written to the code image.

use rayon::prelude::*;

use super::calibration::{Data, Calibration, Camera};

/// Parameters of the step (named after the camera data entries they come from).
#[derive(Clone, Copy, Debug)]
pub struct Parameter {
    /// smoothing length of the layer image (`FIR_ProportionLocalMeanRef`)
    pub mean: f32,
    /// smoothing length of the weight map (`FIR_ProportionLocalMeanEdgeRef`)
    pub mean_edge: f32,
    /// smoothing length of the log ratio (`FIR_ProportionLocalMeanImgWithEdgeRef`)
    pub image_edge: f32,
    /// edge width factor (`FIR_Locality`)
    pub local: f32,
    /// `FIR_SigmoidOffset`
    pub offset: f32,
    /// `FIR_ScaleFactorEdgeRef`
    pub scale_edge: f32,
    /// `FIR_ScaleFactorLocalMeanEdgeRef`
    pub scale_mean_edge: f32,
    /// `FIR_EdgeMapExponent`
    pub exponent: f32,
    /// scale of the matrix (green row sum handed on by the colour step)
    pub matrix_gain: f32,
    /// the digital rest of the ISO factor
    pub iso_gain2: f32,
}

/// Parameters from the camera data (with fixed defaults by capture ISO and camera).
pub fn parameter(d: &Data, k: &Calibration, matrix_gain: f32) -> Parameter {
    let iso = k.iso.iso;
    let sd = matches!(k.model.class, Camera::Sd9 | Camera::Sd10);
    let number = |n: &str| d.number(n);
    let (mean, mean_edge, image_edge, local);
    if sd {
        mean = if iso <= 1200.0 { 0.00093 } else { 0.000846 };
        mean_edge = if iso <= 600.0 { 0.001606 } else { 0.000846 };
        image_edge = if iso <= 1200.0 {
            if iso <= 600.0 {
                0.016063999
            } else {
                0.012682
            }
        } else {
            0.008455
        };
        local = if iso <= 1200.0 {
            if iso <= 600.0 {
                1.0
            } else {
                1.5
            }
        } else {
            2.0
        };
    } else {
        let mut v = number("FIR_ProportionLocalMeanRef");
        if v == 0.0 {
            v = if iso <= 2400.0 {
                if iso <= 1200.0 {
                    0.0011
                } else {
                    0.001
                }
            } else {
                0.008
            };
        }
        mean = v;
        let mut v = number("FIR_ProportionLocalMeanEdgeRef");
        if v == 0.0 {
            v = if iso <= 600.0 { 0.0019 } else { 0.001 };
        }
        mean_edge = v;
        let mut v = number("FIR_ProportionLocalMeanImgWithEdgeRef");
        if v == 0.0 {
            v = if iso <= 2400.0 {
                if iso <= 1200.0 {
                    if iso <= 600.0 {
                        0.018999999
                    } else {
                        0.015
                    }
                } else {
                    0.01
                }
            } else {
                0.015
            };
        }
        image_edge = v;
        let mut v = number("FIR_Locality");
        if v == 0.0 {
            v = if iso <= 2400.0 {
                if iso <= 1200.0 {
                    if iso <= 600.0 {
                        1.0
                    } else {
                        1.5
                    }
                } else {
                    2.0
                }
            } else {
                3.0
            };
        }
        local = v;
    }
    let or = |n: &str, std: f32| {
        let v = number(n);
        if v == 0.0 {
            std
        } else {
            v
        }
    };
    Parameter {
        mean,
        mean_edge,
        image_edge,
        local,
        scale_edge: or("FIR_ScaleFactorEdgeRef", 5.0),
        scale_mean_edge: or("FIR_ScaleFactorLocalMeanEdgeRef", 7.0),
        offset: or("FIR_SigmoidOffset", 0.550000012),
        exponent: or("FIR_EdgeMapExponent", 0.699999988),
        matrix_gain: matrix_gain,
        iso_gain2: k.iso.digital,
    }
}

const TABLE_EDGE: usize = 0x3c000;
/// scale of the exponent / edge tables (entries per unit of the argument)
const EDGE_SCALE: f32 = 11818.5576171875;
const LOG_SCALE: f32 = -23637.115234375;

/// Gradient magnitude of the three-layer image `src` (`w × h`) on a cross of neighbours into the
/// mono image `dst`; only rows 1 … h-3 are written (the border copies its neighbour).
fn gradient(dst: &mut [f32], src: &[f32], w: usize, h: usize) {
    if h < 4 || w < 3 {
        return;
    }
    dst[w..(h - 2) * w].par_chunks_mut(w).enumerate().for_each(|(i, row_fn)| {
        let y = i + 1;
        for x in 1..w - 1 {
            let p = |yy: usize, xx: usize, c: usize| src[(yy * w + xx) * 3 + c];
            let f21 = p(y, x - 1, 0) - p(y, x + 1, 0);
            let f22 = p(y, x - 1, 1) - p(y, x + 1, 1);
            let f23 = p(y, x - 1, 2) - p(y, x + 1, 2);
            let f24 = p(y + 1, x, 0) - p(y - 1, x, 0);
            let f25 = p(y + 1, x, 1) - p(y - 1, x, 1);
            let f26 = p(y + 1, x, 2) - p(y - 1, x, 2);
            row_fn[x] = (f26 * f26 + f25 * f25 + f24 * f24 + f23 * f23 + f22 * f22 + f21 * f21).sqrt();
        }
        row_fn[0] = row_fn[1];
        row_fn[w - 1] = row_fn[w - 2];
    });
}

/// Edge map of the gradient image: gradient of `src` → `dst`, standardised by its spread and
/// passed through a sigmoid table.
fn edge_map(dst: &mut [f32], src: &[f32], w: usize, h: usize, local: f32, scale: f32, offset: f32) {
    gradient(dst, src, w, h);
    // spread of the gradient image (sums in double precision, two values per step)
    let (mut sq, mut sm) = (0.0f64, 0.0f64);
    for y in 0..h {
        let z = &dst[y * w..(y + 1) * w];
        let pairs = w - (w & 1);
        let mut i = 0;
        while i < pairs {
            let (a, b) = (z[i], z[i + 1]);
            sq = f64::from(b * b) + (f64::from(a * a) + sq);
            sm = f64::from(b) + (f64::from(a) + sm);
            i += 2;
        }
        if w & 1 == 1 {
            let a = z[w - 1];
            sq = f64::from(a * a) + sq;
            sm = f64::from(a) + sm;
        }
    }
    let n = (w as u32).wrapping_mul(h as u32);
    let var = (sq - (sm * sm) / f64::from(n)) / f64::from(n.wrapping_sub(1));
    let sigma = ((var as f32).sqrt() * local) * 0.5;
    let mut tab = vec![0.0f32; TABLE_EDGE];
    for (i, t) in tab.iter_mut().enumerate() {
        let x = (i as f32 / LOG_SCALE) / sigma;
        let v = ((x.tanh() + 1.0) - offset) * scale;
        *t = 1.0 / ((-v).exp() + 1.0);
    }
    dst.par_iter_mut().for_each(|v| {
        let i = (*v * EDGE_SCALE) as i32;
        let i = i.min(0x3bfff).max(0) as usize;
        *v = tab[i];
    });
}

/// Weights of the constant-weight filter for a line of `n` samples (decay `r`, `c = 1/r`).
fn weights(n: usize, r: f32, c: f32) -> Vec<f32> {
    let mut pre = vec![0.0f32; n];
    let mut back = vec![0.0f32; n];
    pre[0] = 1.0;
    back[n - 1] = 1.0;
    if n > 1 {
        pre[1] = r + 1.0;
        back[n - 2] = back[n - 1] * r + 1.0;
    }
    for i in 2..n.saturating_sub(1) {
        pre[i] = pre[i - 1] * r + 1.0;
        back[n - 1 - i] = back[n - i] * r + 1.0;
    }
    let mut w = vec![0.0f32; n];
    w[0] = 1.0 / (back[1] + c);
    w[n - 1] = 1.0 / (pre[n - 2] + c);
    for i in 1..n - 1 {
        w[i] = 1.0 / ((pre[i - 1] + back[i + 1]) + c);
    }
    w
}

/// Writes the rows `rows` (each `n` samples of `k` values, computed independently in parallel by
/// `row`) transposed into `dst` (`n × sh` samples of `k` values); rows not in the list keep `dst`.
fn transposed<const K: usize>(dst: &mut [f32], n: usize, sh: usize, rows: usize, row_fn: impl Fn(usize, &mut [f32]) + Sync) {
    let mut buffer = vec![0.0f32; rows * n * K];
    buffer.par_chunks_mut(n * K).enumerate().for_each(|(row, out)| row_fn(row, out));
    dst.par_chunks_mut(sh * K).enumerate().for_each(|(i, d)| {
        for row in 0..rows {
            d[row * K..row * K + K].copy_from_slice(&buffer[(row * n + i) * K..(row * n + i) * K + K]);
        }
    });
}

/// Constant-weight two-sided recursive filter along the rows of a three-layer image, written
/// transposed (`dst` is `sh × sw`). The last row of `src` is not processed. Rows in parallel.
fn smooth_rgb(dst: &mut [f32], src: &[f32], sw: usize, sh: usize, wts: &[f32], r: f32) {
    let c = 1.0f32 / r;
    let n = sw;
    transposed::<3>(dst, n, sh, sh.saturating_sub(1), |row, out| {
        let mut f = vec![[0.0f32; 3]; n];
        let mut b = vec![[0.0f32; 3]; n];
        let s = &src[row * sw * 3..(row + 1) * sw * 3];
        for ch in 0..3 {
            f[0][ch] = s[ch];
            for i in 1..n {
                f[i][ch] = s[i * 3 + ch] + f[i - 1][ch] * r;
            }
            b[n - 1][ch] = s[(n - 1) * 3 + ch];
            for i in (0..n - 1).rev() {
                b[i][ch] = s[i * 3 + ch] + b[i + 1][ch] * r;
            }
        }
        for ch in 0..3 {
            let o = |i: usize| i * 3 + ch;
            out[o(0)] = (s[ch] * c + b[1][ch]) * wts[0];
            out[o(n - 1)] = (s[(n - 1) * 3 + ch] * c + f[n - 2][ch]) * wts[n - 1];
            for i in 1..n - 1 {
                out[o(i)] = ((s[i * 3 + ch] * c + f[i - 1][ch]) + b[i + 1][ch]) * wts[i];
            }
        }
    });
}

/// Edge-coupled two-sided recursive filter (three layers); `edge` is the coupling weight per
/// sample (`sw × sh`). The result is written transposed; when `edge_t` is given, the transposed
/// coupling map is written there as well. Rows in parallel.
fn smooth_edge(dst: &mut [f32], src: &[f32], sw: usize, sh: usize, edge: &[f32], edge_t: Option<&mut [f32]>, r: f32) {
    let c = 1.0f32 / r;
    let n = sw;
    if let Some(t) = edge_t {
        t.par_chunks_mut(sh).enumerate().for_each(|(x, t)| {
            for row in 0..sh {
                t[row] = edge[row * sw + x];
            }
        });
    }
    transposed::<3>(dst, n, sh, sh, |row, out| {
        let mut f = vec![[0.0f32; 3]; n];
        let mut b = vec![[0.0f32; 3]; n];
        let mut nf = vec![0.0f32; n];
        let mut nb = vec![0.0f32; n];
        let s = &src[row * sw * 3..(row + 1) * sw * 3];
        let e = &edge[row * sw..(row + 1) * sw];
        for ch in 0..3 {
            f[0][ch] = s[ch];
        }
        nf[0] = 1.0;
        for i in 1..n {
            let a = e[i] * r;
            for ch in 0..3 {
                f[i][ch] = s[i * 3 + ch] + a * f[i - 1][ch];
            }
            nf[i] = a * nf[i - 1] + 1.0;
        }
        for ch in 0..3 {
            b[n - 1][ch] = s[(n - 1) * 3 + ch];
        }
        nb[n - 1] = 1.0;
        for i in (0..n - 1).rev() {
            let a = e[i] * r;
            for ch in 0..3 {
                b[i][ch] = s[i * 3 + ch] + a * b[i + 1][ch];
            }
            nb[i] = a * nb[i + 1] + 1.0;
        }
        let o = |i: usize, ch: usize| i * 3 + ch;
        let den_n = nb[1] + c;
        for ch in 0..3 {
            out[o(0, ch)] = (s[ch] * c + b[1][ch]) / den_n;
        }
        let den_n = nf[n - 2] + c;
        for ch in 0..3 {
            out[o(n - 1, ch)] = (s[(n - 1) * 3 + ch] * c + f[n - 2][ch]) / den_n;
        }
        for i in 1..n - 1 {
            let den_n = (nf[i - 1] + nb[i + 1]) + c;
            for ch in 0..3 {
                out[o(i, ch)] = ((s[i * 3 + ch] * c + f[i - 1][ch]) + b[i + 1][ch]) / den_n;
            }
        }
    });
}

/// Constant-weight filter for a mono image, written transposed (`dst` is `sh × sw`). Rows in parallel.
fn smooth_mono(dst: &mut [f32], src: &[f32], sw: usize, sh: usize, wts: &[f32], r: f32) {
    let c = 1.0f32 / r;
    let n = sw;
    // the vector body covers positions 1 … l, the scalar rest follows
    let l = if n >= 2 && n - 2 > 3 { (n - 2) - ((n + 2) & 3) } else { 0 };
    transposed::<1>(dst, n, sh, sh, |row, out| {
        let mut f = vec![0.0f32; n];
        let mut b = vec![0.0f32; n];
        let s = &src[row * sw..(row + 1) * sw];
        f[0] = s[0];
        for i in 1..n {
            f[i] = f[i - 1] * r + s[i];
        }
        b[n - 1] = s[n - 1];
        for i in (0..n - 1).rev() {
            b[i] = b[i + 1] * r + s[i];
        }
        out[0] = (s[0] * c + b[1]) * wts[0];
        out[n - 1] = (s[n - 1] * c + f[n - 2]) * wts[n - 1];
        for i in 1..n - 1 {
            let v = if i <= l { (s[i] * c + b[i + 1]) + f[i - 1] } else { (s[i] * c + f[i - 1]) + b[i + 1] };
            out[i] = v * wts[i];
        }
    });
}

fn rate(w: usize, h: usize, fraction: f32) -> f32 {
    let l = ((w as u32 + h as u32) >> 2) as f32;
    let e = (std::f32::consts::LN_2 / (l * fraction)).exp();
    1.0 / e
}

/// Runs the step: `codes` are the matrix result, `layers` the cored layers (both `w × h × 3`);
/// the result replaces `codes`.
pub fn run(codes: &mut [i32], layers: &[i32], w: usize, h: usize, p: &Parameter) {
    let scale = ((9000.0f32 / p.iso_gain2) * 4.0) as i32;
    let scale = if scale < 24001 { scale } else { 24000 };
    let sf = scale as f32;
    let upper_limit = scale - 1;
    let k1 = |v: i32| i32::from(v as i16).min(upper_limit).max(1);
    let c1: Vec<i32> = codes.iter().map(|&v| k1(v)).collect();
    let c2: Vec<i32> = layers.iter().map(|&v| k1(v)).collect();

    // layers^exponent
    let pow: Vec<f32> = (0..scale as usize).map(|i| (i as f32 / sf).powf(p.exponent)).collect();
    let n = w * h;
    let mut mu: Vec<f32> = c2.iter().map(|&v| pow[v as usize]).collect();
    let mut scratch = vec![0.0f32; n * 3];
    let mut intr = vec![0.0f32; n];
    let mut edge = vec![0.0f32; n];
    let mut scratch_mono = vec![0.0f32; n];

    edge_map(&mut intr, &mu, w, h, p.local, p.scale_edge, p.offset);

    // smooth the layer image (constant weight, horizontal then vertical)
    let r1 = rate(w, h, p.mean);
    let wt = weights(w, r1, 1.0 / r1);
    smooth_rgb(&mut scratch, &mu, w, h, &wt, r1);
    let wt = weights(h, r1, 1.0 / r1);
    smooth_rgb(&mut mu, &scratch, h, w, &wt, r1);
    edge_map(&mut edge, &mu, w, h, p.local, p.scale_mean_edge, p.offset);
    for (e, i) in edge.iter_mut().zip(&intr) {
        if !(*e > *i) {
            *e = *i;
        }
    }

    // log ratio, smoothed with the edge map as coupling
    let ln: Vec<f32> = (0..scale as usize).map(|i| ((f64::from(i as f32 / sf)) + 1e-12).ln() as f32).collect();
    mu.par_iter_mut().enumerate().for_each(|(i, m)| *m = ln[c1[i] as usize] - ln[c2[i] as usize]);
    let r2 = rate(w, h, p.image_edge);
    smooth_edge(&mut scratch, &mu, w, h, &edge, Some(&mut scratch_mono), r2);
    smooth_edge(&mut mu, &scratch, h, w, &scratch_mono, None, r2);

    // smooth the weight map
    let r3 = rate(h, w, p.mean_edge);
    let wt = weights(w, r3, 1.0 / r3);
    smooth_mono(&mut scratch_mono, &edge, w, h, &wt, r3);
    let wt = weights(h, r3, 1.0 / r3);
    smooth_mono(&mut intr, &scratch_mono, h, w, &wt, r3);

    finish(codes, &c1, &c2, &mu, &intr, w, h, scale, p);
}

/// Last stage: back from the log domain and blend.
fn finish(out: &mut [i32], c1: &[i32], c2: &[i32], mu: &[f32], weight: &[f32], w: usize, h: usize, scale: i32, p: &Parameter) {
    let sf = scale as f32;
    let pow: Vec<f32> = (0..scale as usize).map(|i| (i as f32 / sf).powf(0.2)).collect();
    let ln: Vec<f32> = (0..scale as usize).map(|i| ((f64::from(i as f32 / sf)) + 1e-12).ln() as f32).collect();
    let exp: Vec<i32> = (0..TABLE_EDGE)
        .map(|j| {
            let e = ((-(j as f32)) / 23637.115234375).exp() * sf;
            (e + e) as i32
        })
        .collect();
    let f21 = (p.matrix_gain.min(7.0) * p.iso_gain2 * 4096.0) as i32;
    let _ = (w, h);
    out.par_chunks_mut(3).enumerate().for_each(|(i, o)| {
        {
            let s = [c2[i * 3], c2[i * 3 + 1], c2[i * 3 + 2]];
            let e: [i32; 3] = std::array::from_fn(|c| {
                let v = ((ln[s[c] as usize] + mu[i * 3 + c]) * LOG_SCALE) as i32;
                exp[if v < 0 { 0 } else { v.min(0x3bfff) as usize }]
            });
            let g = weight[i].min(1.0);
            let f22 = (((1.0 - g) * pow[((s[2] + s[0] + s[1] * 2) >> 2) as usize]) * f21 as f32) as i32;
            for c in 0..3 {
                let v = ((c1[i * 3 + c] * 2 - e[c]).wrapping_mul(f22).wrapping_add(e[c].wrapping_mul(f21))) >> 13;
                let v = v.min(24000);
                o[c] = if v < -499 { -500 } else { v };
            }
        }
    });
}
