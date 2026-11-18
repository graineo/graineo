//! Noise reduction for non-Sigma RAWs.
//!
//! Own implementation; ideas from darktable "denoise (profiled)" (`src/iop/denoiseprofile.c`,
//! GPL-3.0-or-later: variance stabilization with the generalized Anscombe transform,
//! wavelets in an opponent color space with separate strength for luminance and color) and
//! RawTherapee "Noise Reduction" (separate luminance/chrominance sliders, detail slider).
//! No code taken; the thresholds do not follow darktable but a locally adaptive
//! Wiener shrinkage per wavelet level.
//!
//! Chain:
//! 1. **Noise model from the image** ([`estimate`]): on the white-balanced mosaic (where
//!    the photosites are still independent) per color second differences of same-color neighbors in a
//!    row, averaged in 16×16 blocks, sorted by brightness; per brightness level the lower
//!    decile of the block variances (flat areas) → line `variance = a·value + b` (Poisson-Gaussian).
//! 2. **Noise amplitude per wavelet level by simulation** ([`simulate_profile`]): a flat
//!    mosaic with exactly this noise is developed with the *same* demosaic and transformed –
//!    so the correlations of the demosaic (RCD, Markesteijn) are included in the thresholds.
//! 3. **Denoising after the demosaic** ([`apply`]): one common Anscombe transform
//!    for all channels (noise ≈ 1, gray stays gray),
//!    opponent color space Y/U/V, à-trous wavelets (B3 spline, 5 levels luminance, 6 levels color),
//!    per coefficient Wiener gain `max(0, E − T²)/E` with the local energy `E`
//!    (5×5, stretched by the level spacing) and `T = k·σ_level`, back, unbiased inverse
//!    Anscombe (Mäkitalo & Foi 2013, closed-form approximation).
//!
//! Why after the demosaic: luminance and color can only be separated after the demosaic
//! (separate "Luminance"/"Color" sliders), color noise is low-frequency (several pixels in size)
//! and needs large levels, and the demosaic correlation is accounted for via the simulation (2.).
//! Denoising before the demosaic would only handle luminance well.

use rayon::prelude::*;

/// Sliders as in common photo apps ("noise reduction"): all 0…100.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseReduction {
    /// luminance (reference default 0 = off)
    pub luminance: f32,
    /// luminance detail (default 50): higher = spare the fine levels
    pub luminance_detail: f32,
    /// color (reference default 25)
    pub color: f32,
    /// color detail (default 50): higher = spare fine color edges
    pub color_detail: f32,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self { luminance: 0.0, luminance_detail: 50.0, color: 25.0, color_detail: 50.0 }
    }
}

impl NoiseReduction {
    pub const OFF: Self = Self { luminance: 0.0, luminance_detail: 50.0, color: 0.0, color_detail: 50.0 };

    pub fn active(&self) -> bool {
        self.luminance > 0.0 || self.color > 0.0
    }
}

/// Poisson-Gaussian noise model per channel: variance = a·value + b (normalized, white-balanced values).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseModel {
    pub a: [f32; 3],
    pub b: [f32; 3],
}

impl NoiseModel {
    /// Noise (σ) at value `v` for channel `c`.
    pub fn sigma(&self, c: usize, v: f32) -> f32 {
        (self.a[c] * v + self.b[c]).max(0.0).sqrt()
    }
}

const BLOCK: usize = 16;
const STEPS: usize = 24;
/// The lower decile of the block variances underestimates the true variance; factor from the simulation
/// with pure noise (test `estimator_is_unbiased`).
const DECILE_CORRECTION: f32 = 1.33;

/// Estimate the noise model from the white-balanced mosaic. `clip[c]`: from here on a value counts as
/// blown (not used); `wb`: white balance factors the mosaic was
/// multiplied with. None if too few flat areas are found.
pub fn estimate(m: &[f32], w: usize, h: usize, color_at: &(impl Fn(usize, usize) -> usize + Sync), clip: [f32; 3], wb: [f32; 3]) -> Option<NoiseModel> {
    // per pattern position (x mod 6, y mod 6): distance to the nearest same-color pixel left/right
    let mut nb = [[(0usize, 0usize); 6]; 6];
    for (py, row) in nb.iter_mut().enumerate() {
        for (px, o) in row.iter_mut().enumerate() {
            let c = color_at(px + 12, py + 12);
            let l = (1..=6).find(|&d| color_at(px + 12 - d, py + 12) == c).unwrap_or(6);
            let r = (1..=6).find(|&d| color_at(px + 12 + d, py + 12) == c).unwrap_or(6);
            *o = (l, r);
        }
    }
    let bw = w / BLOCK;
    let bh = h / BLOCK;
    if bw < 4 || bh < 4 {
        return None;
    }
    // per block and color: (mean, variance of the second difference / factor)
    let blocks: Vec<[(f32, f32, u32); 3]> = (0..bh)
        .into_par_iter()
        .flat_map_iter(|by| {
            let nb = &nb;
            (0..bw).map(move |bx| {
                let mut s = [0f64; 3];
                let mut s2 = [0f64; 3];
                let mut n = [0u32; 3];
                for y in by * BLOCK..(by + 1) * BLOCK {
                    for x in bx * BLOCK..(bx + 1) * BLOCK {
                        let (l, r) = nb[y % 6][x % 6];
                        if x < l || x + r >= w {
                            continue;
                        }
                        let c = color_at(x, y);
                        let (a, v, b) = (m[y * w + x - l], m[y * w + x], m[y * w + x + r]);
                        if v >= clip[c] || a >= clip[c] || b >= clip[c] {
                            continue;
                        }
                        // second difference with weights for unequal distances (linearly interpolated)
                        let (fl, fr) = (r as f32 / (l + r) as f32, l as f32 / (l + r) as f32);
                        let d = v - fl * a - fr * b;
                        s[c] += v as f64;
                        s2[c] += (d * d / (1.0 + fl * fl + fr * fr)) as f64;
                        n[c] += 1;
                    }
                }
                std::array::from_fn(|c| if n[c] >= 16 { ((s[c] / n[c] as f64) as f32, (s2[c] / n[c] as f64) as f32, n[c]) } else { (0.0, -1.0, 0) })
            })
        })
        .collect();
    // Physically, before white balance the same line holds for all colors (gain g,
    // read noise r); with the factor k_c: a_c = k_c·g, b_c = k_c²·r. Therefore fit all colors together
    // in raw value units (more robust than per color).
    let mut pts_acc: Vec<(f32, f32, f32)> = Vec::new();
    for c in 0..3 {
        let k = wb[c].max(1e-6);
        // levels by √value (dark areas resolved more finely)
        let mut push_stops: Vec<Vec<(f32, f32)>> = vec![Vec::new(); STEPS];
        let top = clip[c].max(1e-3);
        for bl in &blocks {
            let (mean, var, _) = bl[c];
            // skip empty/constant blocks (e.g. masked border, raw value 0) and values below black
            if var <= 1e-12 || mean <= 0.0 || mean > 0.95 * top {
                continue;
            }
            let s = ((mean.max(0.0) / top).sqrt() * STEPS as f32) as usize;
            push_stops[s.min(STEPS - 1)].push((mean, var));
        }
        // per level: lower decile (flat areas)
        for st in push_stops.iter_mut() {
            if st.len() < 24 {
                continue;
            }
            st.sort_by(|x, y| x.1.total_cmp(&y.1));
            let var = st[st.len() / 10].1 * DECILE_CORRECTION;
            let mean = st.iter().map(|p| p.0).sum::<f32>() / st.len() as f32;
            pts_acc.push((mean / k, var / (k * k), st.len() as f32));
        }
    }
    if pts_acc.is_empty() {
        return None;
    }
    // weighted line var = g·x + r; weight ~ √count / var² (relative errors)
    let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for &(x, y, n) in &pts_acc {
        let wgt = (n as f64).sqrt() / (y as f64 * y as f64).max(1e-30);
        sw += wgt;
        sx += wgt * x as f64;
        sy += wgt * y as f64;
        sxx += wgt * x as f64 * x as f64;
        sxy += wgt * x as f64 * y as f64;
    }
    let det = sw * sxx - sx * sx;
    let (mut g, mut r) = if pts_acc.len() >= 2 && det.abs() > 1e-40 { (((sw * sxy - sx * sy) / det) as f32, ((sxx * sy - sx * sxy) / det) as f32) } else { (0.0, (sy / sw) as f32) };
    if g < 0.0 {
        g = 0.0;
        r = (sy / sw) as f32;
    }
    // read noise never exactly 0 (Anscombe needs b > −3/8·a²)
    let min_var = pts_acc.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    r = r.max(0.05 * min_var).max(1e-14);
    let a = std::array::from_fn(|c| wb[c] * g);
    let b = std::array::from_fn(|c| wb[c] * wb[c] * r);
    Some(NoiseModel { a, b })
}

// ---------------------------------------------------------------------------------------------
// Variance stabilization

/// Generalized Anscombe transform per channel: values with variance a·v + b → variance ≈ 1.
#[derive(Clone, Copy)]
struct Anscombe {
    a: f32,
    /// σ² of the Gaussian part in Poisson units (b / a²)
    s2: f32,
    /// pure Gaussian noise (a ≈ 0): simple scaling with 1/√b
    gauss: Option<f32>,
}

impl Anscombe {
    fn new(a: f32, b: f32) -> Self {
        if a <= 1e-9 || b / (a * a) > 1e6 {
            Self { a, s2: 0.0, gauss: Some(1.0 / b.max(1e-20).sqrt()) }
        } else {
            Self { a, s2: b / (a * a), gauss: None }
        }
    }

    #[inline]
    fn forward(&self, v: f32) -> f32 {
        if let Some(g) = self.gauss {
            return v * g;
        }
        let x = v / self.a;
        2.0 * (x + 0.375 + self.s2).max(0.0).sqrt()
    }

    /// Unbiased inverse transform (closed-form approximation after Mäkitalo & Foi 2013), for
    /// small z the algebraic inverse.
    #[inline]
    fn inverse(&self, z: f32) -> f32 {
        if let Some(g) = self.gauss {
            return z / g;
        }
        let x = if z > 1.2 {
            const S: f32 = 1.224_744_9; // √(3/2)
            let zi = 1.0 / z;
            0.25 * z * z + 0.25 * S * zi - 1.375 * zi * zi + 0.625 * S * zi * zi * zi - 0.125 - self.s2
        } else {
            0.25 * z * z - 0.375 - self.s2
        };
        x * self.a
    }
}

// ---------------------------------------------------------------------------------------------
// Wavelets

const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
const BOX: [f32; 5] = [0.2; 5];

#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * (n - 1) - i;
    }
    i.clamp(0, n - 1) as usize
}

/// Separable 5-tap filter with hole spacing `step` (à trous), mirrored borders.
fn filter5(src: &[f32], dst: &mut [f32], tmp: &mut [f32], w: usize, h: usize, step: usize, k: &[f32; 5]) {
    let s = step as isize;
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let r = &src[y * w..(y + 1) * w];
        let inner = 2 * step..w.saturating_sub(2 * step);
        for (x, o) in row.iter_mut().enumerate() {
            *o = if inner.contains(&x) {
                k[0] * r[x - 2 * step] + k[1] * r[x - step] + k[2] * r[x] + k[3] * r[x + step] + k[4] * r[x + 2 * step]
            } else {
                let xi = x as isize;
                (0..5).map(|t| k[t] * r[mirror(xi + (t as isize - 2) * s, w)]).sum()
            };
        }
    });
    let tmp: &[f32] = tmp;
    dst.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let yi = y as isize;
        let rows: [&[f32]; 5] = std::array::from_fn(|t| {
            let yy = mirror(yi + (t as isize - 2) * s, h);
            &tmp[yy * w..(yy + 1) * w]
        });
        for (x, o) in row.iter_mut().enumerate() {
            *o = k[0] * rows[0][x] + k[1] * rows[1][x] + k[2] * rows[2][x] + k[3] * rows[3][x] + k[4] * rows[4][x];
        }
    });
}

/// Denoise one plane: `p` is replaced by the sum of the shrunk details plus residual.
/// `thresholds[l]` = T per level (0 = leave the level unchanged).
fn denoise_plane(p: &mut [f32], w: usize, h: usize, thresholds: &[f32]) {
    let n = w * h;
    let mut cur = p.to_vec();
    let mut next = vec![0f32; n];
    let mut en = vec![0f32; n];
    let mut tmp = vec![0f32; n];
    p.iter_mut().for_each(|v| *v = 0.0);
    for (l, &t) in thresholds.iter().enumerate() {
        let step = 1usize << l;
        filter5(&cur, &mut next, &mut tmp, w, h, step, &B3);
        // detail = cur − next (in cur)
        cur.par_iter_mut().zip(next.par_iter()).for_each(|(c, nx)| *c -= nx);
        if t > 0.0 {
            en.par_iter_mut().zip(cur.par_iter()).for_each(|(e, d)| *e = d * d);
            let en2 = en.clone();
            filter5(&en2, &mut en, &mut tmp, w, h, step, &BOX);
            let t2 = t * t;
            cur.par_iter_mut().zip(en.par_iter()).for_each(|(d, &e)| {
                let g = if e > t2 { (e - t2) / e } else { 0.0 };
                *d *= g;
            });
        }
        p.par_iter_mut().zip(cur.par_iter()).for_each(|(o, d)| *o += d);
        std::mem::swap(&mut cur, &mut next);
    }
    p.par_iter_mut().zip(cur.par_iter()).for_each(|(o, r)| *o += r);
}

pub const LAYERS_Y: usize = 5;
pub const LAYERS_C: usize = 6;

/// Noise amplitude (σ) per wavelet level in Y, U, V after Anscombe – valid for a specific
/// noise model and demosaic.
#[derive(Debug, Clone)]
pub struct Profile {
    pub model: NoiseModel,
    pub sigma: [[f32; LAYERS_C]; 3],
}

impl Profile {
    /// The same noise with its amplitude multiplied by `k` (variance by `k²`): the residual noise of an image mixed
    /// from a noise-free version and `k` parts of the original. The level thresholds are relative to the stabilized
    /// noise and stay.
    pub fn scaled(&self, k: f32) -> Profile {
        let k2 = k * k;
        Profile { model: NoiseModel { a: self.model.a.map(|v| v * k2), b: self.model.b.map(|v| v * k2) }, sigma: self.sigma }
    }
}

/// *One* transform for all channels (mean of the models): this way gray stays gray in the stabilized
/// space (U = V = 0), and shrinking the color levels does not shift hues. The residual variance that differs
/// per channel (white balance) is contained in the simulated level thresholds.
fn joint_anscombe(m: &NoiseModel) -> [Anscombe; 3] {
    let a = (m.a[0] + m.a[1] + m.a[2]) / 3.0;
    let b = (m.b[0] + m.b[1] + m.b[2]) / 3.0;
    [Anscombe::new(a, b); 3]
}

#[inline]
fn to_yuv(r: f32, g: f32, b: f32) -> [f32; 3] {
    [(r + g + b) / 3.0, 0.5 * (r - b), 0.5 * (r + b) - g]
}

#[inline]
fn from_yuv(y: f32, u: f32, v: f32) -> [f32; 3] {
    let g = y - v * (2.0 / 3.0);
    let rb = y + v / 3.0;
    [rb + u, g, rb - u]
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }

    /// Standard normal distributed (Box-Muller)
    fn gauss(&mut self) -> f32 {
        let (u1, u2) = (self.next(), self.next());
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

/// Generate a flat mosaic (gray `level`) with the noise model, develop it with `demosaic`,
/// transform it and measure the noise amplitude per level.
pub fn simulate_profile(
    model: NoiseModel,
    color_at: &(impl Fn(usize, usize) -> usize + Sync),
    demosaic: &dyn Fn(&[f32], usize, usize) -> Vec<[f32; 3]>,
) -> Profile {
    simulate_profile_with(model, color_at, 264, &|m, w, h| (demosaic(m, w, h), w, h))
}

/// Like [`simulate_profile`], but with arbitrary processing that may change the size
/// (e.g. binning + downscaling for the preview): mosaic `n_source`², result (rgb, w, h).
/// This way the profile matches the downscaled preview image exactly.
pub fn simulate_profile_with(
    model: NoiseModel,
    color_at: &(impl Fn(usize, usize) -> usize + Sync),
    n_source: usize,
    process: &dyn Fn(&[f32], usize, usize) -> (Vec<[f32; 3]>, usize, usize),
) -> Profile {
    const MARGIN: usize = 32;
    let level = 0.18f32;
    let nq = n_source.div_ceil(6) * 6;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let m: Vec<f32> = (0..nq * nq)
        .map(|i| {
            let c = color_at(i % nq, i / nq);
            level + model.sigma(c, level) * rng.gauss()
        })
        .collect();
    let (rgb, nw, nh) = process(&m, nq, nq);
    let anscombe = joint_anscombe(&model);
    let n = nw * nh;
    let mut layers: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; n]);
    for (i, p) in rgb.iter().enumerate().take(n) {
        let yuv = to_yuv(anscombe[0].forward(p[0]), anscombe[1].forward(p[1]), anscombe[2].forward(p[2]));
        for k in 0..3 {
            layers[k][i] = yuv[k];
        }
    }
    let border = MARGIN.min(nw.min(nh) / 4);
    let mut sigma = [[0f32; LAYERS_C]; 3];
    let mut tmp = vec![0f32; n];
    let mut next = vec![0f32; n];
    for k in 0..3 {
        let mut cur = layers[k].clone();
        for (l, s) in sigma[k].iter_mut().enumerate() {
            filter5(&cur, &mut next, &mut tmp, nw, nh, 1 << l, &B3);
            let (mut s2, mut cnt) = (0f64, 0f64);
            for y in border..nh - border {
                for x in border..nw - border {
                    let d = cur[y * nw + x] - next[y * nw + x];
                    s2 += (d * d) as f64;
                    cnt += 1.0;
                }
            }
            *s = (s2 / cnt.max(1.0)).sqrt() as f32;
            std::mem::swap(&mut cur, &mut next);
        }
    }
    Profile { model, sigma }
}

/// Strength factors per level from the sliders. Luminance 0…100 → k 0…3,
/// color 0…100 → k 0…12 (25 ≙ 3: on GR III/EOS RP ISO 6400 as clean as the default 25 of common photo apps). Detail shifts strength away from the fine levels.
fn thresholds(p: &Profile, r: &NoiseReduction) -> [[f32; LAYERS_C]; 3] {
    let mut t = [[0f32; LAYERS_C]; 3];
    let ky = (r.luminance / 100.0).clamp(0.0, 1.0) * 3.0;
    let kc = (r.color / 100.0).clamp(0.0, 1.0) * 12.0;
    let dy = (r.luminance_detail / 100.0).clamp(0.0, 1.0);
    let dc = (r.color_detail / 100.0).clamp(0.0, 1.0);
    for l in 0..LAYERS_Y {
        let f = match l {
            0 => 1.25 - dy,
            1 => 1.15 - 0.5 * dy,
            2 => 1.05 - 0.15 * dy,
            _ => 0.9,
        };
        t[0][l] = ky * f.max(0.0) * p.sigma[0][l];
    }
    for l in 0..LAYERS_C {
        let f = match l {
            0 => 1.3 - 0.6 * dc,
            1 => 1.2 - 0.4 * dc,
            _ => 1.0,
        };
        for k in 1..3 {
            t[k][l] = kc * f.max(0.0) * p.sigma[k][l];
        }
    }
    t
}

/// Everything another implementation (GPU, `rawdev::gpu`) needs for [`apply`].
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Anscombe: a, s2 (Poisson units) or pure Gaussian (factor 1/√b)
    pub a: f32,
    pub s2: f32,
    pub gauss: Option<f32>,
    /// thresholds per wavelet level; empty = do not denoise the level (channel)
    pub y: Vec<f32>,
    pub u: Vec<f32>,
    pub v: Vec<f32>,
}

/// Parameters for [`apply`] (None: nothing to do).
pub fn parameter(profile: &Profile, r: &NoiseReduction, w: usize, h: usize) -> Option<Parameter> {
    if !r.active() || w < 16 || h < 16 {
        return None;
    }
    let ans1 = joint_anscombe(&profile.model)[0];
    let t = thresholds(profile, r);
    Some(Parameter {
        a: ans1.a,
        s2: ans1.s2,
        gauss: ans1.gauss,
        y: if r.luminance > 0.0 { t[0][..LAYERS_Y].to_vec() } else { Vec::new() },
        u: if r.color > 0.0 { t[1].to_vec() } else { Vec::new() },
        v: if r.color > 0.0 { t[2].to_vec() } else { Vec::new() },
    })
}

/// Denoise (linear, white-balanced camera RGB, w×h). Unchanged when the sliders are inactive.
pub fn apply(px: &mut [[f32; 3]], w: usize, h: usize, profile: &Profile, r: &NoiseReduction) {
    if !r.active() || w < 16 || h < 16 {
        return;
    }
    let m = profile.model;
    let anscombe = joint_anscombe(&m);
    let n = w * h;
    let t = thresholds(profile, r);
    let mut y = vec![0f32; n];
    let mut u = vec![0f32; n];
    let mut v = vec![0f32; n];
    y.par_iter_mut().zip(u.par_iter_mut()).zip(v.par_iter_mut()).zip(px.par_iter()).for_each(|(((y, u), v), p)| {
        let q = to_yuv(anscombe[0].forward(p[0]), anscombe[1].forward(p[1]), anscombe[2].forward(p[2]));
        *y = q[0];
        *u = q[1];
        *v = q[2];
    });
    if r.luminance > 0.0 {
        denoise_plane(&mut y, w, h, &t[0][..LAYERS_Y]);
    }
    if r.color > 0.0 {
        denoise_plane(&mut u, w, h, &t[1]);
        denoise_plane(&mut v, w, h, &t[2]);
    }
    px.par_iter_mut().zip(y.par_iter()).zip(u.par_iter()).zip(v.par_iter()).for_each(|(((p, &y), &u), &v)| {
        let q = from_yuv(y, u, v);
        *p = [anscombe[0].inverse(q[0]), anscombe[1].inverse(q[1]), anscombe[2].inverse(q[2])];
    });
}

// ---- finished images: noise without a mosaic (JPEG/TIFF, and linear RAW such as Canon sRAW/mRAW or linear DNG) ----

/// Noise model of a finished image: per channel the variance of the second difference in the
/// quietest blocks (lowest tenth), as Gaussian noise (variance independent of the value).
pub fn image_noise_model(px: &[[f32; 3]], w: usize, h: usize) -> Option<NoiseModel> {
    const B: usize = 16;
    let (bw, bh) = (w / B, h / B);
    if bw < 4 || bh < 4 {
        return None;
    }
    let mut b = [0f32; 3];
    for c in 0..3 {
        let mut v: Vec<f32> = (0..bh)
            .into_par_iter()
            .flat_map_iter(|by| {
                (0..bw).map(move |bx| {
                    let (mut s, mut s2, mut n) = (0f64, 0f64, 0f64);
                    for y in by * B..(by + 1) * B {
                        for x in (bx * B + 1)..((bx + 1) * B - 1) {
                            let d = (2.0 * px[y * w + x][c] - px[y * w + x - 1][c] - px[y * w + x + 1][c]) as f64;
                            s += d;
                            s2 += d * d;
                            n += 1.0;
                        }
                    }
                    ((s2 / n - (s / n) * (s / n)) / 6.0) as f32
                })
            })
            .collect();
        v.sort_by(|a, b| a.total_cmp(b));
        b[c] = v[v.len() / 10].max(1e-10) * 1.33;
    }
    Some(NoiseModel { a: [0.0; 3], b })
}

/// Smallest scale the noise of a downscaled finished image is simulated at (see [`image_profile`]).
const PROFILE_MIN_SCALE: f32 = 0.2;

/// Noise profile for a finished image (channels independent), optionally downscaled by `f`.
///
/// The simulated square grows with 1/f² (≈ 800/f samples per edge): for a panorama with a 30 000 px long edge
/// that was 134 million samples, 1.5 GB and 3 s just to open it. Below [`PROFILE_MIN_SCALE`] the simulation runs at
/// that scale with the noise variance reduced by (f/min)² – downscaling by 1/f averages ≈ 1/f² pixels, so the
/// downscaled noise is the same. Images up to ≈ 10 000 px long edge compute exactly as before.
pub fn image_profile(mut m: NoiseModel, mut f: f32) -> Profile {
    if f < PROFILE_MIN_SCALE {
        let k = (f / PROFILE_MIN_SCALE).powi(2);
        m.b = m.b.map(|b| b * k);
        m.a = m.a.map(|a| a * k);
        f = PROFILE_MIN_SCALE;
    }
    image_profile_exact(m, f)
}

pub fn image_profile_exact(m: NoiseModel, f: f32) -> Profile {
    let color_at = |x: usize, _y: usize| x % 3;
    let nq = (264.0 * 3.0 / f).ceil() as usize;
    let v = move |m: &[f32], ww: usize, hh: usize| -> (Vec<[f32; 3]>, usize, usize) {
        let w3 = ww / 3;
        let px: Vec<[f32; 3]> = (0..hh).flat_map(|y| (0..w3).map(move |x| (y, x))).map(|(y, x)| [m[y * ww + 3 * x], m[y * ww + 3 * x + 1], m[y * ww + 3 * x + 2]]).collect();
        let (nw, nh) = (((w3 as f32 * f).round() as usize).max(1), ((hh as f32 * f / 3.0).round() as usize).max(1));
        // only a third of the rows needed (keep it square)
        let hh3 = hh / 3;
        let px: Vec<[f32; 3]> = px[..w3 * hh3].to_vec();
        let nh = nh.min(hh3).max(1);
        (crate::development::downscale(&px, w3, hh3, nw, nh), nw, nh)
    };
    simulate_profile_with(m, &color_at, nq, &v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bayer(x: usize, y: usize) -> usize {
        [[0, 1], [1, 2]][y & 1][x & 1]
    }

    /// Pure noise of known strength on a brightness gradient: the estimator hits a and b.
    #[test]
    fn estimator_is_unbiased() {
        let (w, h) = (1200, 800);
        let (a, b) = (2e-4f32, 4e-6f32);
        let mut rng = Rng(42);
        let m: Vec<f32> = (0..w * h)
            .map(|i| {
                let v = 0.02 + 0.8 * (i % w) as f32 / w as f32;
                v + (a * v + b).sqrt() * rng.gauss()
            })
            .collect();
        let r = estimate(&m, w, h, &bayer, [1.0; 3], [1.0; 3]).expect("model");
        for c in 0..3 {
            let rel_a = r.a[c] / a;
            eprintln!("c{c}: a {:.3e} ({rel_a:.3}), b {:.3e}", r.a[c], r.b[c]);
            assert!((0.85..1.15).contains(&rel_a), "a channel {c}: {rel_a}");
        }
    }

    #[test]
    fn anscombe_round_trip() {
        // Unbiased: the inverse transform of the *mean* of the transformed noisy values
        // hits the true value (Gaussian approximation of the Poisson noise, from ~20 counts).
        let (a, b) = (1e-3f32, 1e-6f32);
        let ans1 = Anscombe::new(a, b);
        let mut rng = Rng(7);
        for v in [0.02f32, 0.1, 0.5, 1.0] {
            let n = 200_000;
            let s = (a * v + b).sqrt();
            let mz: f64 = (0..n).map(|_| ans1.forward(v + s * rng.gauss()) as f64).sum::<f64>() / n as f64;
            let r = ans1.inverse(mz as f32);
            assert!((r - v).abs() / v < 0.01, "{v} → {r}");
        }
    }

    #[test]
    fn yuv_invertible() {
        let q = to_yuv(0.3, 0.5, 0.9);
        let p = from_yuv(q[0], q[1], q[2]);
        assert!((p[0] - 0.3).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6 && (p[2] - 0.9).abs() < 1e-6);
    }
}
