//! Capture sharpening: Richardson-Lucy deconvolution of the luminance with a Gaussian PSF.
//!
//! Own implementation of the idea from RawTherapee ("Capture Sharpening", Ingo Weyrich, GPL-3):
//! blur from the AA filter/lens is assumed to be Gaussian and deconvolved in linear light before all tone
//! steps; the channels are scaled by the ratio of new/old luminance
//! (no color fringes). Damped in flat, noisy areas (contrast mask)
//! and in blown highlights. No code taken from RawTherapee.

use rayon::prelude::*;

#[derive(Debug, Clone, Copy)]
pub struct Sharpness {
    /// Strength 0…1 (0 = off)
    pub strength: f32,
    /// Radius of the Gaussian PSF in pixels
    pub sigma: f32,
    pub iterations: usize,
    /// Contrast threshold of the mask (relative gradient of the square-root luminance)
    pub threshold: f32,
}

impl Default for Sharpness {
    fn default() -> Self {
        Self { strength: 0.5, sigma: 0.7, iterations: 6, threshold: 0.012 }
    }
}

/// Gaussian kernel of the PSF (also for the GPU).
pub fn kernel(sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as isize;
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    k
}

fn blur(src: &[f32], w: usize, h: usize, k: &[f32], tmp: &mut [f32], dst: &mut [f32]) {
    let r = (k.len() / 2) as isize;
    let ru = r as usize;
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0f32;
            if x >= ru && x + ru < w {
                // interior without bounds checks
                for (kv, v) in k.iter().zip(&s[x - ru..=x + ru]) {
                    acc += kv * v;
                }
            } else {
                for (i, kv) in k.iter().enumerate() {
                    let xx = (x as isize + i as isize - r).clamp(0, w as isize - 1) as usize;
                    acc += kv * s[xx];
                }
            }
            *o = acc;
        }
    });
    let tmp: &[f32] = tmp;
    dst.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        row.iter_mut().for_each(|v| *v = 0.0);
        for (i, kv) in k.iter().enumerate() {
            let yy = (y as isize + i as isize - r).clamp(0, h as isize - 1) as usize;
            let s = &tmp[yy * w..(yy + 1) * w];
            for (o, v) in row.iter_mut().zip(s) {
                *o += kv * v;
            }
        }
    });
}

/// Sharpens `px` (linear, white-balanced camera RGB, w×h) in place.
/// `clip`: values from here on count as blown (no sharpening).
pub fn apply(px: &mut [[f32; 3]], w: usize, h: usize, p: &Sharpness, clip: f32) {
    if p.strength <= 0.0 || p.iterations == 0 || w < 8 || h < 8 {
        return;
    }
    let n = w * h;
    let y: Vec<f32> = px.par_iter().map(|c| ((c[0] + c[1] + c[2]) / 3.0).max(1e-7)).collect();
    let k = kernel(p.sigma);
    let mut est = y.clone();
    let mut tmp = vec![0f32; n];
    let mut b = vec![0f32; n];
    let mut b2 = vec![0f32; n];
    for _ in 0..p.iterations {
        blur(&est, w, h, &k, &mut tmp, &mut b);
        b.par_iter_mut().zip(y.par_iter()).for_each(|(bv, yv)| *bv = yv / bv.max(1e-7));
        blur(&b, w, h, &k, &mut tmp, &mut b2);
        est.par_iter_mut().zip(b2.par_iter()).for_each(|(e, r)| *e *= r.clamp(0.5, 2.0));
    }
    // contrast mask on square-root luminance (noise ~ √signal → gradient roughly noise-normalized)
    let s: Vec<f32> = y.par_iter().map(|v| v.sqrt()).collect();
    let thr = p.threshold;
    px.par_chunks_mut(w).enumerate().for_each(|(yy, row)| {
        for (xx, c) in row.iter_mut().enumerate() {
            let i = yy * w + xx;
            if c[0] >= clip || c[1] >= clip || c[2] >= clip {
                continue;
            }
            let x0 = xx.saturating_sub(1);
            let x1 = (xx + 1).min(w - 1);
            let y0 = yy.saturating_sub(1);
            let y1 = (yy + 1).min(h - 1);
            let gx = s[yy * w + x1] - s[yy * w + x0];
            let gy = s[y1 * w + xx] - s[y0 * w + xx];
            let g = (gx * gx + gy * gy).sqrt();
            let m = ((g - thr) / thr).clamp(0.0, 1.0);
            if m <= 0.0 {
                continue;
            }
            let f = (1.0 + p.strength * m * (est[i] / y[i] - 1.0)).clamp(0.6, 1.8);
            c[0] *= f;
            c[1] *= f;
            c[2] *= f;
        }
    });
}
