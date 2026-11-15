//! Dehaze ("Dunst entfernen", −1 … +1).
//!
//! Haze model (Koschmieder): I = J·t + A·(1 − t) with scene radiance J, airlight A and transmission t, valid on linear
//! values. Estimation after He, Sun & Tang, "Single Image Haze Removal Using Dark Channel Prior" (CVPR 2009): in
//! haze-free regions some channel is close to zero, so the local minimum over channels and a neighbourhood
//! ("dark channel") of I/A measures the haze density d = 1 − t (at full strength). The map is refined with a guided
//! filter (He, Sun & Tang, "Guided Image Filtering", ECCV 2010) guided by the luminance, so it follows edges.
//!
//! The analysis ([`HazeField`]) depends only on the image, not on the slider: it is computed once per image (full
//! SPP base, own full image, or preview) and cropped together with the image for crops, like the Fill Light map, so
//! that preview, 100 % view and export agree. The slider only sets ω = 0.95·|amount| at render time.
//! amount > 0 removes haze, amount < 0 adds haze of the estimated colour. Half of the brightness loss of removing
//! haze is given back as a global gain (median luminance of the analysis grid).

use rayon::prelude::*;
use std::sync::Arc;

/// Lower bound of the transmission when removing haze (limits noise amplification in dense haze).
const T0: f32 = 0.25;
/// Share of the brightness loss that is compensated.
const COMPENSATE: f32 = 0.5;
/// Neighbourhood radius of the dark channel at full Merrill size (4704 px); scaled with the image size.
const RADIUS_FULL: f32 = 15.0;
const RADIUS_REF_EDGE: f32 = 4704.0;
/// Estimation grid (block size of the analysis copy).
const F: usize = 4;

/// Image analysis for dehaze: airlight and refined dark channel per pixel.
#[derive(Clone, Debug)]
pub struct HazeField {
    pub width: usize,
    pub height: usize,
    /// Airlight (haze colour) in the units of the analysed data
    pub a: [f32; 3],
    /// Refined dark channel of I/A per pixel (0 … 1)
    dark: Vec<f32>,
    /// Analysis grid of the whole image (block means and their dark value) for the brightness compensation;
    /// shared by crops so that every crop uses the gain of the full image
    sample: Arc<Vec<([f32; 3], f32)>>,
}

/// Prepared parameters for one slider position.
pub struct Dehaze<'a> {
    field: &'a HazeField,
    omega: f32,
    remove: bool,
    gain: f32,
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
    for x in 0..w {
        let mut cs = vec![0f64; h + 1];
        for y in 0..h {
            cs[y + 1] = cs[y] + tmp[y * w + x] as f64;
        }
        for y in 0..h {
            let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
            out[y * w + x] = ((cs[b] - cs[a]) / (b - a) as f64) as f32;
        }
    }
    out
}

/// Separable minimum filter with radius r.
fn min_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(w));
            row[x] = src[y * w + a..y * w + b].iter().copied().fold(f32::MAX, f32::min);
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let mut m = f32::MAX;
            for yy in a..b {
                m = m.min(tmp[yy * w + x]);
            }
            row[x] = m;
        }
    });
    out
}

fn lum(q: [f32; 3]) -> f32 {
    0.2126 * q[0] + 0.7152 * q[1] + 0.0722 * q[2]
}

impl HazeField {
    /// Analyses interleaved linear RGB `data` (any positive scale). `scale`: pixels of the full image per pixel of
    /// `data` (1 for full size, the factor for previews), so that the neighbourhood covers the same scene area.
    pub fn analyze(data: &[f32], w: usize, h: usize, scale: f32) -> Self {
        let (sw, sh) = (w.div_ceil(F).max(1), h.div_ceil(F).max(1));
        let small: Vec<[f32; 3]> = (0..sw * sh)
            .into_par_iter()
            .map(|k| {
                let (sx, sy) = (k % sw, k / sw);
                let mut acc = [0f64; 3];
                let mut n = 0f64;
                for y in sy * F..((sy + 1) * F).min(h) {
                    for x in sx * F..((sx + 1) * F).min(w) {
                        let q = &data[(y * w + x) * 3..(y * w + x) * 3 + 3];
                        for c in 0..3 {
                            acc[c] += q[c].max(0.0) as f64;
                        }
                        n += 1.0;
                    }
                }
                acc.map(|v| (v / n.max(1.0)) as f32)
            })
            .collect();
        // neighbourhood radius in grid cells, relative to the full image size
        let full_edge = w.max(h) as f32 * scale.max(1.0);
        let r = ((RADIUS_FULL * full_edge / RADIUS_REF_EDGE) / (F as f32 * scale.max(1.0))).round().max(1.0) as usize;
        // airlight: mean colour of the brightest 0.1 % of the dark channel (He et al.)
        let dark0 = min_filter(&small.iter().map(|q| q[0].min(q[1]).min(q[2])).collect::<Vec<_>>(), sw, sh, r);
        let mut idx: Vec<usize> = (0..sw * sh).collect();
        idx.sort_unstable_by(|&a, &b| dark0[b].total_cmp(&dark0[a]));
        let top = (sw * sh / 1000).max(16).min(sw * sh);
        let mut a = [0f64; 3];
        for &k in &idx[..top] {
            for c in 0..3 {
                a[c] += small[k][c] as f64;
            }
        }
        let a = a.map(|v| ((v / top as f64) as f32).max(1e-6));
        let dark = min_filter(&small.iter().map(|q| (q[0] / a[0]).min(q[1] / a[1]).min(q[2] / a[2]).clamp(0.0, 1.0)).collect::<Vec<_>>(), sw, sh, r);
        // guided filter refinement of the dark channel, guide = luminance relative to the airlight
        let la = lum(a);
        let g: Vec<f32> = small.iter().map(|&q| (lum(q) / la).min(2.0)).collect();
        let rg = (r * 4).max(4);
        let eps = 1e-3f32;
        let mg = box_mean(&g, sw, sh, rg);
        let md = box_mean(&dark, sw, sh, rg);
        let mgd = box_mean(&g.iter().zip(&dark).map(|(x, y)| x * y).collect::<Vec<_>>(), sw, sh, rg);
        let mgg = box_mean(&g.iter().map(|x| x * x).collect::<Vec<_>>(), sw, sh, rg);
        let (ca, cb): (Vec<f32>, Vec<f32>) = (0..sw * sh)
            .map(|k| {
                let var = mgg[k] - mg[k] * mg[k];
                let cov = mgd[k] - mg[k] * md[k];
                let ak = cov / (var + eps);
                (ak, md[k] - ak * mg[k])
            })
            .unzip();
        let ma = box_mean(&ca, sw, sh, rg);
        let mb = box_mean(&cb, sw, sh, rg);
        let d_small: Vec<f32> = (0..sw * sh).map(|k| (ma[k] * g[k] + mb[k]).clamp(0.0, 1.0)).collect();
        // bilinear to full size of `data`
        let half = (F as f32 - 1.0) / 2.0;
        let mut d_full = vec![0f32; w * h];
        d_full.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let fy = ((y as f32 - half) / F as f32).clamp(0.0, (sh - 1) as f32);
            let (y0, ty) = (fy.floor() as usize, fy.fract());
            let y1 = (y0 + 1).min(sh - 1);
            for (x, v) in row.iter_mut().enumerate() {
                let fx = ((x as f32 - half) / F as f32).clamp(0.0, (sw - 1) as f32);
                let (x0, tx) = (fx.floor() as usize, fx.fract());
                let x1 = (x0 + 1).min(sw - 1);
                *v = (d_small[y0 * sw + x0] * (1.0 - tx) + d_small[y0 * sw + x1] * tx) * (1.0 - ty)
                    + (d_small[y1 * sw + x0] * (1.0 - tx) + d_small[y1 * sw + x1] * tx) * ty;
            }
        });
        let sample = Arc::new(small.into_iter().zip(d_small).collect());
        Self { width: w, height: h, a, dark: d_full, sample }
    }

    /// Crop `[x, y, w, h]` (same rectangle as the image crop).
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Self {
        let dark = (y..y + h).flat_map(|yy| self.dark[yy * self.width + x..yy * self.width + x + w].iter().copied()).collect();
        Self { width: w, height: h, a: self.a, dark, sample: self.sample.clone() }
    }

    /// Downscaled by `factor` to `w × h` (pixel at the block's top-left sample, like the image's block mean origin).
    pub fn downsample(&self, factor: usize, w: usize, h: usize) -> Self {
        let dark = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                let (cx, cy) = ((x * factor + factor / 2).min(self.width - 1), (y * factor + factor / 2).min(self.height - 1));
                self.dark[cy * self.width + cx]
            })
            .collect();
        Self { width: w, height: h, a: self.a, dark, sample: self.sample.clone() }
    }

    /// Field for a rectangle `[x, y, w, h]` of an image of size `full_w × full_h` (this field covers the whole image at
    /// its own resolution): bilinear resampling, so every path (preview, crop, export) uses the same analysis.
    pub fn resample(&self, full_w: usize, full_h: usize, region: [usize; 4]) -> Self {
        let [x0, y0, w, h] = region;
        let (sx, sy) = (self.width as f32 / full_w as f32, self.height as f32 / full_h as f32);
        let dark = (0..h)
            .into_par_iter()
            .flat_map_iter(|yy| {
                let fy = (((y0 + yy) as f32 + 0.5) * sy - 0.5).clamp(0.0, (self.height - 1) as f32);
                let (r0, ty) = (fy.floor() as usize, fy.fract());
                let r1 = (r0 + 1).min(self.height - 1);
                (0..w).map(move |xx| {
                    let fx = (((x0 + xx) as f32 + 0.5) * sx - 0.5).clamp(0.0, (self.width - 1) as f32);
                    let (c0, tx) = (fx.floor() as usize, fx.fract());
                    let c1 = (c0 + 1).min(self.width - 1);
                    let g = |c: usize, r: usize| self.dark[r * self.width + c];
                    (g(c0, r0) * (1.0 - tx) + g(c1, r0) * tx) * (1.0 - ty) + (g(c0, r1) * (1.0 - tx) + g(c1, r1) * tx) * ty
                })
            })
            .collect();
        Self { width: w, height: h, a: self.a, dark, sample: self.sample.clone() }
    }

    /// Dark-channel map of the whole field (row-major, `width × height`), e.g. for the GPU.
    pub fn dark(&self) -> &[f32] {
        &self.dark
    }

    /// Parameters for slider position `amount` (−1 … +1); `None` at 0.
    pub fn at(&self, amount: f32) -> Option<Dehaze<'_>> {
        let amount = amount.clamp(-1.0, 1.0);
        if amount == 0.0 {
            return None;
        }
        let omega = 0.95 * amount.abs();
        let remove = amount > 0.0;
        let mut gain = 1.0;
        if remove {
            let mut before: Vec<f32> = self.sample.iter().map(|(q, _)| lum(*q)).collect();
            let mut after: Vec<f32> = self
                .sample
                .iter()
                .map(|&(q, d)| {
                    let t = (1.0 - omega * d).max(T0);
                    lum(std::array::from_fn(|c| ((q[c] - self.a[c]) / t + self.a[c]).max(0.0)))
                })
                .collect();
            let median = |v: &mut Vec<f32>| {
                let m = v.len() / 2;
                *v.select_nth_unstable_by(m, |x, y| x.total_cmp(y)).1
            };
            let (mb, ma) = (median(&mut before), median(&mut after));
            if mb > 0.0 && ma > 0.0 {
                gain = (mb / ma).clamp(1.0, 4.0).powf(COMPENSATE);
            }
        }
        Some(Dehaze { field: self, omega, remove, gain })
    }
}

impl Dehaze<'_> {
    /// (ω, gain, removes haze?, lower bound of t) – for the GPU output stage.
    pub fn parameters(&self) -> (f32, f32, bool, f32) {
        (self.omega, self.gain, self.remove, T0)
    }

    /// Applies to pixel number `k` (row-major in the analysed image) with linear value `p`.
    #[inline]
    pub fn pixel(&self, k: usize, p: [f32; 3]) -> [f32; 3] {
        let d = self.field.dark.get(k).copied().unwrap_or(0.0);
        let t = 1.0 - self.omega * d;
        let a = self.field.a;
        if self.remove {
            let t = t.max(T0);
            std::array::from_fn(|c| ((p[c] - a[c]) / t + a[c]) * self.gain)
        } else {
            std::array::from_fn(|c| p[c] * t + a[c] * (1.0 - t))
        }
    }
}
