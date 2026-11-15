//! Noise-aware brightening: [`Brake`] for Fill Light (setting "Rauschbremse"); [`lift`] only via `GRAINEO_LIFT`
//! for graineo-dev test series.
//!
//! Plain exposure multiplies signal and noise alike. Here the linear image is split into an edge-preserving base
//! (guided filter of each channel with the luminance as guide; the regularisation is the locally measured noise
//! variance, so edges above the noise stay sharp) and a detail layer. Only the base receives the full gain;
//! luminance detail is amplified by `gain^luma_exp` and colour detail (mostly colour noise) by `gain^chroma_exp`.
//! Ideas: Durand & Dorsey 2002 (base/detail), Eilertsen, Mantiuk & Unger 2015 (amplify detail only as far as it
//! is not noise).
//!
//! `GRAINEO_LIFT=ev[,luma_exp[,chroma_exp[,radius[,k]]]]`, defaults luma_exp 0.5, chroma_exp 0, radius 6, k 3.

use rayon::prelude::*;

pub struct LiftParams {
    pub ev: f32,
    pub luma_exp: f32,
    pub chroma_exp: f32,
    pub radius: usize,
    pub k: f32,
}

pub fn from_env() -> Option<LiftParams> {
    let v = std::env::var("GRAINEO_LIFT").ok()?;
    let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let ev = *p.first()?;
    Some(LiftParams {
        ev,
        luma_exp: p.get(1).copied().unwrap_or(0.5),
        chroma_exp: p.get(2).copied().unwrap_or(0.0),
        radius: p.get(3).map_or(6, |&r| r.max(1.0) as usize),
        k: p.get(4).copied().unwrap_or(3.0),
    })
}

fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        let mut cs = vec![0f64; w + 1];
        for x in 0..w {
            cs[x + 1] = cs[x] + s[x] as f64;
        }
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(w));
            row[x] = ((cs[b] - cs[a]) / (b - a) as f64) as f32;
        }
    });
    let mut out = vec![0f32; w * h];
    const CH: usize = 64;
    let parts: Vec<(usize, usize, Vec<f32>)> = (0..w.div_ceil(CH))
        .into_par_iter()
        .map(|c| {
            let (x0, x1) = (c * CH, ((c + 1) * CH).min(w));
            let cw = x1 - x0;
            let mut cs = vec![0f64; (h + 1) * cw];
            for y in 0..h {
                for i in 0..cw {
                    cs[(y + 1) * cw + i] = cs[y * cw + i] + tmp[y * w + x0 + i] as f64;
                }
            }
            let mut res = vec![0f32; h * cw];
            for y in 0..h {
                let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
                for i in 0..cw {
                    res[y * cw + i] = ((cs[b * cw + i] - cs[a * cw + i]) / (b - a) as f64) as f32;
                }
            }
            (x0, cw, res)
        })
        .collect();
    for (x0, cw, res) in parts {
        for y in 0..h {
            out[y * w + x0..y * w + x0 + cw].copy_from_slice(&res[y * cw..(y + 1) * cw]);
        }
    }
    out
}

/// Noise variance of the luminance as a function of brightness, var(Y) ≈ a·Y + b, estimated from the image:
/// variance of Y minus its 3×3 mean in 16×16 blocks, lowest decile per brightness bin (flat blocks).
/// Public for the GPU version of the base layer in rawdev (same estimate from the same luminance).
pub fn noise_model(y: &[f32], w: usize, h: usize) -> (f32, f32) {
    let m3 = box_mean(y, w, h, 1);
    let mut blocks: Vec<(f32, f32)> = Vec::new();
    for by in (0..h.saturating_sub(16)).step_by(16) {
        for bx in (0..w.saturating_sub(16)).step_by(16) {
            let (mut s, mut s2, mut sm) = (0f64, 0f64, 0f64);
            for yy in by..by + 16 {
                for xx in bx..bx + 16 {
                    let k = yy * w + xx;
                    let d = (y[k] - m3[k]) as f64;
                    s += d;
                    s2 += d * d;
                    sm += y[k] as f64;
                }
            }
            let n = 256.0;
            // residual of a 3×3 mean keeps 8/9 of white-noise variance
            let var = ((s2 / n - (s / n) * (s / n)) * 9.0 / 8.0) as f32;
            blocks.push(((sm / n) as f32, var));
        }
    }
    if blocks.len() < 32 {
        return (0.0, 1.0);
    }
    blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
    // 16 brightness bins, lowest decile of the variance in each
    let per = blocks.len() / 16;
    let mut pts = Vec::new();
    for i in 0..16 {
        let mut chunk: Vec<(f32, f32)> = blocks[i * per..(i + 1) * per].to_vec();
        chunk.sort_by(|a, b| a.1.total_cmp(&b.1));
        let p = chunk[chunk.len() / 10];
        pts.push(p);
    }
    // least squares var = a·Y + b, a, b ≥ 0
    let n = pts.len() as f64;
    let (sx, sy) = pts.iter().fold((0f64, 0f64), |(a, b), p| (a + p.0 as f64, b + p.1 as f64));
    let (mx, my) = (sx / n, sy / n);
    let (mut sxx, mut sxy) = (0f64, 0f64);
    for p in &pts {
        sxx += (p.0 as f64 - mx).powi(2);
        sxy += (p.0 as f64 - mx) * (p.1 as f64 - my);
    }
    let a = if sxx > 0.0 { (sxy / sxx).max(0.0) } else { 0.0 };
    let b = (my - a * mx).max(my * 0.05);
    (a as f32, b as f32)
}

/// Above this many pixels [`base_layer`] works at half size (the GPU version in rawdev follows the same rule).
pub const BASE_HALF_ABOVE: usize = 4_000_000;

/// Edge-preserving base layer (interleaved RGB): guided filter of each channel with the luminance as guide,
/// regularised by the measured luminance noise, so that edges above the noise stay in the base and the
/// remainder (`data − base`) is fine detail and noise.
pub fn base_layer(data: &[f32], w: usize, h: usize, radius: usize, k: f32) -> Vec<f32> {
    // Large images: the base layer is smooth, so compute it at half size (radius halved) and interpolate back;
    // about four times faster with a practically identical result.
    if w * h > BASE_HALF_ABOVE && radius >= 2 {
        let (sw, sh) = (w / 2, h / 2);
        let mut small = vec![0f32; sw * sh * 3];
        small.par_chunks_mut(sw * 3).enumerate().for_each(|(y, row)| {
            for x in 0..sw {
                for c in 0..3 {
                    let i = |xx: usize, yy: usize| data[(yy * w + xx) * 3 + c];
                    row[x * 3 + c] = 0.25 * (i(2 * x, 2 * y) + i(2 * x + 1, 2 * y) + i(2 * x, 2 * y + 1) + i(2 * x + 1, 2 * y + 1));
                }
            }
        });
        let base = base_layer_full(&small, sw, sh, radius / 2, k);
        let mut out = vec![0f32; w * h * 3];
        out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
            let fy = ((y as f32 - 0.5) / 2.0).clamp(0.0, (sh - 1) as f32);
            let (y0, ty) = (fy.floor() as usize, fy.fract());
            let y1 = (y0 + 1).min(sh - 1);
            for x in 0..w {
                let fx = ((x as f32 - 0.5) / 2.0).clamp(0.0, (sw - 1) as f32);
                let (x0, tx) = (fx.floor() as usize, fx.fract());
                let x1 = (x0 + 1).min(sw - 1);
                for c in 0..3 {
                    let g = |xx: usize, yy: usize| base[(yy * sw + xx) * 3 + c];
                    let top = g(x0, y0) * (1.0 - tx) + g(x1, y0) * tx;
                    let bot = g(x0, y1) * (1.0 - tx) + g(x1, y1) * tx;
                    row[x * 3 + c] = top * (1.0 - ty) + bot * ty;
                }
            }
        });
        return out;
    }
    base_layer_full(data, w, h, radius, k)
}

fn base_layer_full(data: &[f32], w: usize, h: usize, radius: usize, k: f32) -> Vec<f32> {
    let lw = [0.2126f32, 0.7152, 0.0722];
    let y: Vec<f32> = data.par_chunks(3).map(|q| lw[0] * q[0] + lw[1] * q[1] + lw[2] * q[2]).collect();
    let (na, nb) = noise_model(&y, w, h);
    if std::env::var_os("GRAINEO_LIFT_INFO").is_some() {
        eprintln!("base layer {w}×{h}: noise var(Y) = {na:.4e}·Y + {nb:.4e}");
    }
    let r = radius;
    let mu_y = box_mean(&y, w, h, r);
    let yy = box_mean(&y.par_iter().map(|v| v * v).collect::<Vec<_>>(), w, h, r);
    let var_y: Vec<f32> = yy.iter().zip(&mu_y).map(|(a, m)| (a - m * m).max(0.0)).collect();
    let eps: Vec<f32> = mu_y.iter().map(|&m| k * (na * m.max(0.0) + nb)).collect();
    let mut out = vec![0f32; w * h * 3];
    for c in 0..3 {
        let ch: Vec<f32> = data.par_chunks(3).map(|q| q[c]).collect();
        let mu_c = box_mean(&ch, w, h, r);
        let cy = box_mean(&ch.par_iter().zip(&y).map(|(a, b)| a * b).collect::<Vec<_>>(), w, h, r);
        let (a, b): (Vec<f32>, Vec<f32>) = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let cov = cy[i] - mu_c[i] * mu_y[i];
                let ai = cov / (var_y[i] + eps[i]);
                (ai, mu_c[i] - ai * mu_y[i])
            })
            .unzip();
        let am = box_mean(&a, w, h, r);
        let bm = box_mean(&b, w, h, r);
        out.par_chunks_mut(3).enumerate().for_each(|(i, o)| o[c] = am[i] * y[i] + bm[i]);
    }
    out
}

/// Noise brake for brightening gains (Fill Light): the base layer receives the full gain `f`, luminance detail
/// `f^luma_exp`, colour detail (mostly colour noise) `f^chroma_exp`. Darkening (`f ≤ 1`) is applied unchanged.
pub struct Brake {
    base: Vec<f32>,
    luma_exp: f32,
    chroma_exp: f32,
}

impl Brake {
    pub fn new(data: &[f32], w: usize, h: usize, luma_exp: f32, chroma_exp: f32) -> Self {
        Self { base: base_layer(data, w, h, 6, 3.0), luma_exp, chroma_exp }
    }

    /// With a precomputed base layer ([`base_layer`] with radius 6, k 3), e.g. cached per image.
    pub fn from_base(base: Vec<f32>, luma_exp: f32, chroma_exp: f32) -> Self {
        Self { base, luma_exp, chroma_exp }
    }

    /// Base value of pixel `k` (to be transformed like the pixel by affine steps such as dehaze).
    #[inline]
    pub fn base(&self, k: usize) -> [f32; 3] {
        [self.base[k * 3], self.base[k * 3 + 1], self.base[k * 3 + 2]]
    }

    /// Pixel `p` with base `b` (both after the same affine steps) brightened by `f`.
    #[inline]
    pub fn apply(&self, p: [f32; 3], b: [f32; 3], f: f32) -> [f32; 3] {
        if f <= 1.0 {
            return p.map(|v| v * f);
        }
        let lw = [0.2126f32, 0.7152, 0.0722];
        let d = [p[0] - b[0], p[1] - b[1], p[2] - b[2]];
        let dy = lw[0] * d[0] + lw[1] * d[1] + lw[2] * d[2];
        let (gl, gc) = (f.powf(self.luma_exp), f.powf(self.chroma_exp));
        std::array::from_fn(|c| f * b[c] + gl * dy + gc * (d[c] - dy))
    }
}

/// Returns the lifted interleaved RGB image (linear, same units as the input).
pub fn lift(data: &[f32], w: usize, h: usize, p: &LiftParams) -> Vec<f32> {
    let gain = 2f32.powf(p.ev);
    let lw = [0.2126f32, 0.7152, 0.0722];
    let y: Vec<f32> = data.par_chunks(3).map(|q| lw[0] * q[0] + lw[1] * q[1] + lw[2] * q[2]).collect();
    let (na, nb) = noise_model(&y, w, h);
    let r = p.radius;
    let mu_y = box_mean(&y, w, h, r);
    let yy = box_mean(&y.par_iter().map(|v| v * v).collect::<Vec<_>>(), w, h, r);
    let var_y: Vec<f32> = yy.iter().zip(&mu_y).map(|(a, m)| (a - m * m).max(0.0)).collect();
    let eps: Vec<f32> = mu_y.iter().map(|&m| p.k * (na * m.max(0.0) + nb)).collect();
    let mut base: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::new());
    for c in 0..3 {
        let ch: Vec<f32> = data.par_chunks(3).map(|q| q[c]).collect();
        let mu_c = box_mean(&ch, w, h, r);
        let cy = box_mean(&ch.par_iter().zip(&y).map(|(a, b)| a * b).collect::<Vec<_>>(), w, h, r);
        let (a, b): (Vec<f32>, Vec<f32>) = (0..w * h)
            .into_par_iter()
            .map(|k| {
                let cov = cy[k] - mu_c[k] * mu_y[k];
                let ak = cov / (var_y[k] + eps[k]);
                (ak, mu_c[k] - ak * mu_y[k])
            })
            .unzip();
        let am = box_mean(&a, w, h, r);
        let bm = box_mean(&b, w, h, r);
        base[c] = (0..w * h).into_par_iter().map(|k| am[k] * y[k] + bm[k]).collect();
    }
    let gl = gain.powf(p.luma_exp);
    let gc = gain.powf(p.chroma_exp);
    let mut out = vec![0f32; w * h * 3];
    out.par_chunks_mut(3).enumerate().for_each(|(k, o)| {
        let q = &data[k * 3..k * 3 + 3];
        let d = [q[0] - base[0][k], q[1] - base[1][k], q[2] - base[2][k]];
        let dy = lw[0] * d[0] + lw[1] * d[1] + lw[2] * d[2];
        for c in 0..3 {
            o[c] = gain * base[c][k] + gl * dy + gc * (d[c] - dy);
        }
    });
    if std::env::var_os("GRAINEO_LIFT_INFO").is_some() {
        eprintln!("lift: gain {gain:.2}, luma detail ×{gl:.2}, colour detail ×{gc:.2}, noise var(Y) = {na:.3}·Y + {nb:.2}");
    }
    out
}
