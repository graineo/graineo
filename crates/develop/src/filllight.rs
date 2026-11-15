//! Fill Light: Retinex brightening (standard path).
//!
//! Matches SPP's Fill Light output (Retinex-type local brightening).
//!
//! 1. Log luminance `L = 1024·ln(Y + 30)`.
//! 2. Very wide low-pass over an image pyramid → surround brightness `B`.
//! 3. Local contrast normalisation: target level = (L − B)·K/S around a middle
//!    level; log gain = target − actual. Dark surroundings → positive gain.
//! 4. The slider only shapes the gain via `exp(s·tanh(k·g))` (s = 1.33·slider)
//!    and is applied equally to R, G and B, so colours are preserved.
//!
//! The map does not depend on the slider and can be cached for the preview.
//!
//! Alternative form (test only, not the default; switch `GRAINEO_MERRILL_NEW=filllight`, [`own_gain_map`]): Multiscale
//! Retinex after Jobson, Rahman & Woodell ("A multiscale retinex for bridging the gap between colour
//! images and the human observation of scenes", IEEE TIP 6, 1997):
//! surround = mean of four Gaussian blurs of the log luminance (3 %, 10 %, 30 % and 100 % of the long
//! image side); reference level = mix of the mean log luminance and a fixed middle level; local contrast
//! stage after Wallis ("An approach to the space variant restoration and enhancement of images", 1976):
//! the deviation from the surround is scaled towards a desired spread. The gain is shaped by
//! `exp(A·slider·tanh(gain/(C·|slider|^q)))`. Parameters measured against exported reference images
//! ([`OWN`], local reference measurement).

use rayon::prelude::*;

use crate::calib::Calibration;
use crate::Linear;

struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.v[y * self.w + x]
    }

    fn downsample(&self) -> Plane {
        let (w, h) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let v = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % w) as isize * 2, (i / w) as isize * 2);
                0.25 * (self.at(x, y) + self.at(x + 1, y) + self.at(x, y + 1) + self.at(x + 1, y + 1))
            })
            .collect();
        Plane { w, h, v }
    }

    /// Binomial filter 3×3 (1 2 1)².
    fn binomial(&self) -> Plane {
        let v = (0..self.w * self.h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % self.w) as isize, (i / self.w) as isize);
                let row = |yy: isize| self.at(x - 1, yy) + 2.0 * self.at(x, yy) + self.at(x + 1, yy);
                (row(y - 1) + 2.0 * row(y) + row(y + 1)) / 16.0
            })
            .collect();
        Plane { w: self.w, h: self.h, v }
    }

    /// Bilinear upscaling to w × h (pixel centres aligned).
    fn upsample(&self, w: usize, h: usize) -> Plane {
        let (sx, sy) = (self.w as f32 / w as f32, self.h as f32 / h as f32);
        let v = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let fx = ((i % w) as f32 + 0.5) * sx - 0.5;
                let fy = ((i / w) as f32 + 0.5) * sy - 0.5;
                let (x0, y0) = (fx.floor(), fy.floor());
                let (tx, ty) = (fx - x0, fy - y0);
                let (x0, y0) = (x0 as isize, y0 as isize);
                let top = self.at(x0, y0) * (1.0 - tx) + self.at(x0 + 1, y0) * tx;
                let bot = self.at(x0, y0 + 1) * (1.0 - tx) + self.at(x0 + 1, y0 + 1) * tx;
                top * (1.0 - ty) + bot * ty
            })
            .collect();
        Plane { w, h, v }
    }
}

/// Very wide, roughly exponentially decaying low-pass: pyramid down to ≤ 2 pixels, then recombine from coarse to fine;
/// level i (0 = coarsest) enters with weight 0.9·0.65^i.
fn pyramid_filter(p: Plane) -> Plane {
    let mut levels = vec![p];
    while levels.last().unwrap().w > 2 && levels.last().unwrap().h > 2 {
        let next = levels.last().unwrap().downsample();
        levels.push(next);
    }
    levels.reverse();
    let mut acc = levels[0].binomial();
    for (i, lvl) in levels.iter().enumerate().skip(1) {
        let wi = 0.9 * 0.65f32.powi(i as i32);
        let up = acc.upsample(lvl.w, lvl.h);
        let b = lvl.binomial();
        let v = up.v.par_iter().zip(&b.v).map(|(u, l)| (1.0 - wi) * u + wi * l).collect();
        acc = Plane { w: lvl.w, h: lvl.h, v };
    }
    acc
}

/// Log gain map in units of ln/1024, per pixel of the linear image.
pub fn log_gain_map(lin: &Linear, cal: &Calibration) -> Vec<f32> {
    if x3f_core::merrill_new::active("filllight") {
        let g: [f32; 3] = cal.wb_gain.map(|v| (cal.count_scale * v) as f32 * cal.saturation as f32);
        let y = lin.data.par_chunks(3).map(|p| (p[0] * g[0] + p[1] * g[1] + p[2] * g[2]) / 3.0).collect();
        return own_gain_map(y, lin.width, lin.height);
    }
    let g: [f32; 3] = cal.wb_gain.map(|v| (cal.count_scale * v) as f32);
    let sat = cal.saturation as f32;
    let l: Vec<f32> = lin
        .data
        .par_chunks(3)
        .map(|p| {
            let y = sat * (0.25 * p[0] * g[0] + 0.65 * p[1] * g[1] + 0.10 * p[2] * g[2]);
            1024.0 * (y.clamp(0.0, 32767.0) + 30.0).ln()
        })
        .collect();
    gain_from_log_luma(l, lin.width, lin.height)
}

/// Like [`log_gain_map`], for SPP counts (already white-balanced and colour-converted).
pub fn log_gain_map_counts(counts: &Linear) -> Vec<f32> {
    if x3f_core::merrill_new::active("filllight") {
        let y = counts.data.par_chunks(3).map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
        return own_gain_map(y, counts.width, counts.height);
    }
    let l: Vec<f32> = counts
        .data
        .par_chunks(3)
        .map(|p| 1024.0 * ((0.25 * p[0] + 0.65 * p[1] + 0.10 * p[2]).clamp(0.0, 32767.0) + 30.0).ln())
        .collect();
    gain_from_log_luma(l, counts.width, counts.height)
}

fn gain_from_log_luma(l: Vec<f32>, w: usize, h: usize) -> Vec<f32> {
    let b = pyramid_filter(Plane { w, h, v: l.clone() });

    let mean = l.par_iter().map(|&x| x as f64).sum::<f64>() / l.len() as f64;
    let shift = (mean + ((6761.44 - mean) / 2.0 + 512.0).trunc()) as f32;
    let d: Vec<f32> = l.par_iter().zip(&b.v).map(|(&li, &bi)| li - bi).collect();
    let var = pyramid_filter(Plane { w, h, v: d.par_iter().map(|&x| (x * x / 1024.0).min(32767.0)).collect() });
    let s: Vec<f32> = var.v.par_iter().map(|&v| 32.0 * (v.max(0.0) + 64.0).sqrt()).collect();
    let k = 32.0 * (s.par_iter().map(|&x| x as f64).sum::<f64>() / s.len() as f64).sqrt() as f32;
    l.par_iter()
        .zip(&d)
        .zip(&s)
        .map(|((&li, &di), &si)| (di * k / si - (li - shift)).clamp(-32767.0, 32767.0))
        .collect()
}

/// Gain factor from log gain `g` for slider value `amount` (−2 … +2).
pub struct FillLightCurve {
    s: f32,
    k: f32,
}

impl FillLightCurve {
    pub fn new(amount: f32) -> Self {
        if x3f_core::merrill_new::active("filllight") {
            let p = own_params();
            let a = if amount > 0.0 { p.a_pos } else { p.a_neg };
            return Self { s: a * amount, k: 1.0 / (p.c * amount.abs().powf(p.q)).max(1e-3) };
        }
        let s = 1.33 * amount;
        Self { s, k: 1.0 / (1024.0 * (s.abs() + 1.0 / 16.0).powf(0.25)) }
    }

    #[inline]
    pub fn factor(&self, g: f32) -> f32 {
        (self.s * (self.k * g).tanh()).exp()
    }
}

/// Parameters of the alternative form, measured against exported reference images of the camera maker's
/// software at Fill Light −2 … +2 (local reference measurement, effective gain per pixel).
#[derive(Clone, Copy, Debug)]
pub struct OwnParams {
    /// small offset of the linear luminance before the logarithm (counts)
    pub eps: f32,
    /// reference level: share of the image's mean log luminance (rest: fixed middle level `ln_mid`)
    pub alpha: f32,
    pub ln_mid: f32,
    pub offset: f32,
    /// weights of the Gaussian surround scales [`OWN_SCALES`]
    pub weights: [f32; 4],
    /// Wallis stage: desired spread of the deviation from the surround, limit of the contrast gain and
    /// scale of the local spread (share of the long side)
    pub sd: f32,
    pub amax: f32,
    pub sig_scale: f32,
    /// curve: strength for brightening / darkening, knee C·|slider|^q
    pub a_pos: f32,
    pub a_neg: f32,
    pub c: f32,
    pub q: f32,
}

/// Measured 14.11.2026: SPP export of DP2M5437 (Fill Light −2/−1/+1/+2) plus five further Merrill files with
/// the default form as a weaker substitute target (weight 0.3), since only one complete reference set exists.
pub const OWN: OwnParams = OwnParams {
    eps: 16.0,
    alpha: 0.528,
    ln_mid: 5.007,
    offset: 1.151,
    weights: [1.0, 1.0, 1.0, 1.0],
    sd: 1.712,
    amax: 1.525,
    sig_scale: 0.030,
    a_pos: 1.268,
    a_neg: 1.295,
    c: 0.902,
    q: 0.245,
};

/// Parameters in use (test override `GRAINEO_FILL_OWN="key=value,…"` for measurements).
pub fn own_params() -> OwnParams {
    static P: std::sync::OnceLock<OwnParams> = std::sync::OnceLock::new();
    *P.get_or_init(|| {
        let mut p = OWN;
        if let Ok(v) = std::env::var("GRAINEO_FILL_OWN") {
            for kv in v.split(',') {
                let Some((k, x)) = kv.split_once('=') else { continue };
                let Ok(x) = x.trim().parse::<f32>() else { continue };
                match k.trim() {
                    "eps" => p.eps = x,
                    "alpha" => p.alpha = x,
                    "lnM" => p.ln_mid = x,
                    "off" => p.offset = x,
                    "w0" => p.weights[0] = x,
                    "w1" => p.weights[1] = x,
                    "w2" => p.weights[2] = x,
                    "w3" => p.weights[3] = x,
                    "sd" => p.sd = x,
                    "amax" => p.amax = x,
                    "sig_s" => p.sig_scale = x,
                    "A" => p.a_pos = x,
                    "An" => p.a_neg = x,
                    "C" => p.c = x,
                    "q" => p.q = x,
                    _ => {}
                }
            }
        }
        p
    })
}

/// Gaussian scales of the surround as a share of the long image side.
const OWN_SCALES: [f32; 4] = [0.03, 0.1, 0.3, 1.0];
/// Long side of the working grid: the map is computed on the luminance averaged down to about this size
/// (the effect is large-scale; averaging first keeps sensor noise out of the logarithm).
const OWN_GRID: usize = 1200;

/// Alternative Fill Light map from the linear luminance `y` (natural log units, positive in dark surroundings):
/// multiscale surround `S` of the log luminance, reference level `R` = mix of image mean and a fixed middle level,
/// map = `R − S + (l − S)·(k − 1)` with the Wallis gain `k = sd / (σ + sd/amax)` (σ: local spread of `l − S`).
pub fn own_gain_map(y: Vec<f32>, w: usize, h: usize) -> Vec<f32> {
    let p = own_params();
    // working grid (block mean of the linear luminance), as in the measurement
    let g = w.max(h).div_ceil(OWN_GRID).max(1);
    let (gw, gh) = (w.div_ceil(g), h.div_ceil(g));
    let lg: Vec<f32> = block_mean(&y, w, h, g).into_iter().map(|v| (v.max(0.0) + p.eps).ln()).collect();
    let mean = (lg.par_iter().map(|&x| f64::from(x)).sum::<f64>() / lg.len().max(1) as f64) as f32;
    // surround on a grid of at most 384 pixels
    let f = gw.max(gh).div_ceil(384).max(1);
    let (sw, sh) = (gw.div_ceil(f), gh.div_ceil(f));
    let small = block_mean(&lg, gw, gh, f);
    let long = sw.max(sh) as f32;
    let wsum: f32 = p.weights.iter().map(|x| x.max(0.0)).sum::<f32>().max(1e-6);
    let mut acc = vec![0.0f32; sw * sh];
    for (frac, wt) in OWN_SCALES.iter().zip(p.weights) {
        let wt = wt.max(0.0) / wsum;
        if wt == 0.0 {
            continue;
        }
        for (a, v) in acc.iter_mut().zip(gauss(&small, sw, sh, frac * long)) {
            *a += v * wt;
        }
    }
    let s_grid = Plane { w: sw, h: sh, v: acc }.upsample(gw, gh).v;
    let reference = p.alpha * mean + (1.0 - p.alpha) * p.ln_mid + p.offset;
    let map_grid: Vec<f32> = if p.sd > 0.0 {
        let dev2: Vec<f32> = lg.iter().zip(&s_grid).map(|(l, s)| (l - s) * (l - s)).collect();
        let d_small = block_mean(&dev2, gw, gh, f);
        let var = Plane { w: sw, h: sh, v: gauss(&d_small, sw, sh, p.sig_scale * long) }.upsample(gw, gh).v;
        lg.par_iter()
            .zip(&s_grid)
            .zip(&var)
            .map(|((&l, &s), &v)| {
                let k = p.sd / (v.max(0.0).sqrt() + p.sd / p.amax.max(0.05));
                reference - s + (l - s) * (k - 1.0)
            })
            .collect()
    } else {
        s_grid.iter().map(|&s| reference - s).collect()
    };
    if g == 1 {
        map_grid
    } else {
        Plane { w: gw, h: gh, v: map_grid }.upsample(w, h).v
    }
}

/// Block mean with factor `f` (partial blocks at the edge averaged over their pixels).
fn block_mean(v: &[f32], w: usize, h: usize, f: usize) -> Vec<f32> {
    if f == 1 {
        return v.to_vec();
    }
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    (0..sw * sh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % sw, i / sw);
            let (mut sum, mut n) = (0.0f32, 0u32);
            for yy in by * f..((by + 1) * f).min(h) {
                for xx in bx * f..((bx + 1) * f).min(w) {
                    sum += v[yy * w + xx];
                    n += 1;
                }
            }
            sum / n.max(1) as f32
        })
        .collect()
}

/// Gaussian blur with standard deviation `sigma` (pixels) as three box passes per axis (edges clamped).
fn gauss(v: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    // box width for three passes: sqrt(12σ²/3 + 1)
    let r = (((12.0 * sigma * sigma / 3.0 + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as isize;
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let (n, lines) = if horizontal { (w, h) } else { (h, w) };
        let at = |line: usize, i: isize| -> f32 {
            let i = i.clamp(0, n as isize - 1) as usize;
            if horizontal { src[line * w + i] } else { src[i * w + line] }
        };
        let out: Vec<Vec<f32>> = (0..lines)
            .into_par_iter()
            .map(|line| {
                let mut o = vec![0.0f32; n];
                let mut sum: f32 = (-r..=r).map(|i| at(line, i)).sum();
                for i in 0..n {
                    o[i] = sum / (2 * r + 1) as f32;
                    sum += at(line, i as isize + r + 1) - at(line, i as isize - r);
                }
                o
            })
            .collect();
        let mut res = vec![0.0f32; w * h];
        for (line, o) in out.into_iter().enumerate() {
            for (i, v) in o.into_iter().enumerate() {
                if horizontal { res[line * w + i] = v } else { res[i * w + line] = v }
            }
        }
        res
    };
    let mut cur = v.to_vec();
    for _ in 0..3 {
        cur = pass(&cur, true);
        cur = pass(&cur, false);
    }
    cur
}
