//! Quattro: own noise reduction of the colour grid.
//!
//! The Quattro sensor records the bottom and middle layers at a quarter of the resolution of the top layer. The
//! colour is formed on that coarse grid ([`crate::quattro_nr::grid_from_layers`]: bottom and middle at their own
//! resolution, top averaged 2 × 2); its noise turns into blotchy colour noise once the colour matrix has amplified
//! the layer differences. This module removes it on the grid, before the colour stage and before the fine detail of
//! the top layer comes back. Steps:
//!
//! 1. **Noise model** ([`Model`]): expected variance of a grid value per layer, `a + b·v` (read noise plus shot
//!    noise), measured on flat areas of base-ISO Quattro images. Dark values get a higher variance by a smooth
//!    factor `1 + (K − 1)·(1 − v/white)^p`, so dark regions are smoothed more strongly than bright ones (in the
//!    shadows the colour matrix amplifies noise most, and there is little colour detail to keep).
//! 2. **Chroma pre-averaging** ([`chroma_prefilter`]): on the white-balanced grid, the colour differences U, V
//!    (BT.601) of a pixel are replaced by their 3 × 3 mean where the neighbourhood is nearly grey or dark and shows
//!    no edge (few neighbours outside the expected noise). Brightness stays. The variance of such a pixel changes
//!    accordingly; the factors per layer are derived from the BT.601 matrix ([`averaged_variance`]).
//! 3. **Range filter** ([`region_filter`]) at two scales (the grid and its half-size 2 × 2 mean): each neighbour
//!    in a square window weighs `exp(−k · Σ Δ²/σ²)` (σ² of the centre pixel, per layer) and is ignored below a
//!    smallest weight. The half scale uses a larger window and removes low-frequency colour blotches.
//! 4. **High ISO** (from capture ISO [`HIGH_ISO`]): both scales are smoothed further by a two-way recursive
//!    (exponential) filter whose coefficient follows an edge map (share of neighbours within the expected noise), and
//!    the half scale is blended into the grid where the image is dark and both scales agree within the noise.
//! 5. **Low ISO**: the half scale fills only flat, dark regions; at local brightness changes and in bright regions
//!    the grid result stays.
//!
//! A second, separate step smooths the brightness of the full-resolution top layer at medium ISO
//! ([`brightness_smooth`]): a 5 × 5 range filter whose range spread grows linearly with the value, blended with the
//! unfiltered layer by a share that falls linearly with log₂ of the capture ISO.
//!
//! Range filtering and recursive edge-preserving smoothing are long-known methods. The noise model was measured on
//! base-ISO images; the other parameters were fitted on grids of real Quattro images so that the result matches the
//! output of the camera maker's software, and the developed results were checked against its TIFF exports.
//!
//! Units: sensor counts above black divided by SensorISO/100 (sd Quattro H: times the digital ISO gain of the layer),
//! before the white balance; the same as [`crate::quattro_nr`].

use rayon::prelude::*;

pub type Px = [f32; 3];

/// From this capture ISO on the high-ISO chain runs (edge-guided recursive smoothing, blend by agreement).
pub const HIGH_ISO: f32 = 400.0;

/// Noise model of one grid value (bottom, middle, top layer): variance = `a + b·v`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    pub a: [f32; 3],
    pub b: [f32; 3],
}

/// Noise model of the Quattro grid at base ISO, measured on base-ISO images of dp0–dp3 Quattro, sd Quattro and
/// sd Quattro H: per layer the variance of the residual against the 3 × 3 mean (× 9/8) in 8 × 8 blocks, the lower
/// tenth of the blocks per brightness class (flat areas), straight line fitted (units of the stage).
pub const MODEL: Model = Model { a: [123.0, 170.0, 16.7], b: [0.32, 0.35, 0.25] };

impl Model {
    /// Variance of the unbalanced value `v` per layer.
    #[inline]
    pub fn variance(&self, v: Px) -> [f32; 3] {
        std::array::from_fn(|c| self.a[c] + self.b[c] * v[c].max(0.0))
    }

    /// Model of a mean of `n` independent values (binning, half scale): variance / n.
    pub fn averaged(&self, n: f32) -> Self {
        Model { a: self.a.map(|v| v / n), b: self.b.map(|v| v / n) }
    }
}

/// Parameters of the grid noise reduction (fitted, see the module text).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parameter {
    /// dark boost of the variance: factor `1 + (k − 1)·(1 − v/white)^p` per layer (white-balanced value)
    pub dark_k: f32,
    pub dark_p: f32,
    /// chroma pre-averaging: near grey when the 3 × 3 means of bottom and middle lie within this share of the top
    pub gray_tolerance: f32,
    /// chroma pre-averaging in dark regions: full below `dark_full`, fading out over `dark_ramp` (3 × 3 mean of
    /// the luma, white-balanced units, times the largest white balance gain)
    pub dark_full: f32,
    pub dark_ramp: f32,
    /// edge in the pre-averaging: more than `edge_two` neighbours outside the noise in two layers, or more than
    /// `edge_one` in one layer
    pub edge_two: u32,
    pub edge_one: u32,
    /// range filter per scale (grid, half size): window radius, range scale `k`, smallest weight
    pub radius: [usize; 2],
    pub region: [f32; 2],
    pub min_weight: [f32; 2],
    /// high ISO: edge map radius and threshold (squared difference > `edges_threshold` · σ²), recursive coefficient
    pub edges_radius: usize,
    pub edges_threshold: f32,
    pub recursive: f32,
    /// high ISO: half-scale share `min(1, exp(−(v − v0)/v1))` by the largest layer of the grid result, reduced by
    /// `exp(−agreement · r · Σ Δ²/σ²)` where both scales differ; `r` rises from 0 to 1 between the brightness
    /// `agreement_ab` and `agreement_ab + agreement_ramp` of the half scale (in dark regions the
    /// half scale replaces the grid also at single outliers)
    pub mix_v0: f32,
    pub mix_v1: f32,
    pub agreement: f32,
    pub agreement_ab: f32,
    pub agreement_ramp: f32,
    /// low ISO: the grid result stays where the local brightness changes by more than `contrast` (relative), where
    /// the brightness curve `1 − min(1, exp(−(v − v0)/v1))` (`low_v0`, `low_v1`) says so, and above the
    /// brightness `bright_ab` (fading in over `bright_ramp`)
    pub contrast: f32,
    pub low_v0: f32,
    pub low_v1: f32,
    pub bright_ab: f32,
    pub bright_ramp: f32,
}

impl Default for Parameter {
    fn default() -> Self {
        PARAMETER
    }
}

/// The fitted parameters (Nelder–Mead on 65 grid sections of 12 images at ISO 100 and 6400, error in units of the
/// noise variance; window radii and the edge map radius chosen by the smallest error over a grid of sizes).
pub const PARAMETER: Parameter = Parameter {
    dark_k: 28.0,
    dark_p: 4.0,
    gray_tolerance: 0.05,
    dark_full: 135.0,
    dark_ramp: 145.0,
    edge_two: 2,
    edge_one: 5,
    radius: [5, 12],
    region: [0.236, 1.0],
    min_weight: [0.18, 0.55],
    edges_radius: 3,
    edges_threshold: 34.0,
    recursive: 0.244,
    mix_v0: 67.0,
    mix_v1: 1080.0,
    agreement: 0.0062,
    agreement_ab: 204.0,
    agreement_ramp: 168.0,
    contrast: 0.104,
    low_v0: 45.0,
    low_v1: 210.0,
    bright_ab: 8300.0,
    bright_ramp: 6000.0,
};

/// One image.
#[derive(Clone, Copy, Debug)]
pub struct Image {
    /// white balance gains of the grid (daylight, normalised to the middle layer)
    pub gains: [f32; 3],
    /// capture ISO
    pub iso: f32,
    /// the grid was binned: the noise variance of a value is the model's divided by this (1 = not binned)
    pub noise_div: f32,
    /// white of the raw range in the units of the stage (2^bits − black)
    pub white: f32,
}

/// BT.601 luma and colour differences (rows Y, U, V).
const YUV: [[f32; 3]; 3] = [[0.299, 0.587, 0.114], [-0.14713, -0.28886, 0.436], [0.615, -0.51499, -0.10001]];

fn yuv_inverse() -> [[f32; 3]; 3] {
    let m = crate::matrix::Mat3(std::array::from_fn(|i| YUV[i / 3][i % 3] as f64)).inverse().expect("YUV invertible").to_f32();
    [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]]
}

#[inline]
fn mul(m: &[[f32; 3]; 3], v: Px) -> Px {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// Variance factors of a pixel whose U, V are replaced by the 3 × 3 mean while its Y stays: `out = A·c + B·m`, with
/// `A = M⁻¹·e_Y·M` (luma of the pixel `c`) and `B = M⁻¹·(e_U + e_V)·M` (chroma of the window mean `m`, variance 1/9 of
/// a single value). Treating the luma of the pixel and the chroma of the mean as independent:
/// `var(out_i) = Σ_j (A_ij² + B_ij²/9)·var_j`. Row `i`: weights of the input variances.
pub fn averaged_variance() -> [[f32; 3]; 3] {
    let inv = yuv_inverse();
    std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            let a = inv[i][0] * YUV[0][j];
            let b = inv[i][1] * YUV[1][j] + inv[i][2] * YUV[2][j];
            a * a + b * b / 9.0
        })
    })
}

/// Expected variance of a white-balanced value (`model`: unbalanced model, `g`: gains), with the dark boost.
#[inline]
fn variance_wb(model: &Model, g: [f32; 3], v: Px, par: &Parameter, white: f32) -> [f32; 3] {
    std::array::from_fn(|c| {
        let x = (v[c] / g[c]).max(0.0);
        let base = (model.a[c] + model.b[c] * x) * g[c] * g[c];
        let t = (1.0 - v[c].max(0.0) / white).clamp(0.0, 1.0);
        base * (1.0 + (par.dark_k - 1.0) * t.powf(par.dark_p))
    })
}

/// Noise reduction of the colour grid `img` (`w × h`, units see module) in place.
pub fn denoise(img: &mut [Px], w: usize, h: usize, image: &Image, model: &Model, par: &Parameter) {
    if w < 16 || h < 16 {
        return;
    }
    let model = if image.noise_div != 1.0 { model.averaged(image.noise_div) } else { *model };
    let g = image.gains;
    let high = image.iso >= HIGH_ISO;
    let orig: Vec<Px> = img.to_vec();
    let scale = |src: &[Px], sw: usize, sh: usize, level: usize, m: &Model| -> Vec<Px> {
        let wb: Vec<Px> = src.par_iter().map(|p| std::array::from_fn(|c| p[c] * g[c])).collect();
        let (pre, fraction) = chroma_prefilter(&wb, sw, sh, m, g, par, image.white);
        let mut res = region_filter(&pre, &fraction, sw, sh, m, g, par, image.white, level);
        res.par_iter_mut().for_each(|p| *p = std::array::from_fn(|c| p[c] / g[c]));
        if high {
            let e = edge_map(src, sw, sh, m, par);
            recursive_smooth(&mut res, &e, sw, sh, par.recursive);
        }
        res
    };
    let full = scale(&orig, w, h, 0, &model);
    let (hw, hh, half) = halve(&orig, w, h);
    let half = scale(&half, hw, hh, 1, &model.averaged(4.0));
    let upsampled = enlarge(&half, hw, hh, w, h);
    if high {
        mix_high(img, &full, &upsampled, &model, par);
    } else {
        mix_low(img, &full, &upsampled, &orig, w, h, par);
    }
}

/// 2 × 2 mean (odd last row/column repeated).
fn halve(src: &[Px], w: usize, h: usize) -> (usize, usize, Vec<Px>) {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let out = (0..nw * nh)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (2 * (i % nw), 2 * (i / nw));
            let (x1, y1) = ((x + 1).min(w - 1), (y + 1).min(h - 1));
            let (a, b, c, d) = (src[y * w + x], src[y * w + x1], src[y1 * w + x], src[y1 * w + x1]);
            std::array::from_fn(|k| 0.25 * (a[k] + b[k] + c[k] + d[k]))
        })
        .collect();
    (nw, nh, out)
}

/// Bilinear enlargement of the half-size grid to `w × h` (pixel centres aligned).
fn enlarge(src: &[Px], sw: usize, sh: usize, w: usize, h: usize) -> Vec<Px> {
    let tap = |x: usize, n: usize| {
        let s = (x as f32 + 0.5) * 0.5 - 0.5;
        let i = s.floor();
        let t = s - i;
        let i = i as isize;
        ((i.clamp(0, n as isize - 1)) as usize, ((i + 1).clamp(0, n as isize - 1)) as usize, t)
    };
    let cols: Vec<(usize, usize, f32)> = (0..w).map(|x| tap(x, sw)).collect();
    let mut out = vec![[0f32; 3]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ya, yb, ty) = tap(y, sh);
        for (x, o) in row.iter_mut().enumerate() {
            let (xa, xb, tx) = cols[x];
            let (p00, p01, p10, p11) = (src[ya * sw + xa], src[ya * sw + xb], src[yb * sw + xa], src[yb * sw + xb]);
            *o = std::array::from_fn(|c| {
                let a = p00[c] + (p01[c] - p00[c]) * tx;
                let b = p10[c] + (p11[c] - p10[c]) * tx;
                a + (b - a) * ty
            });
        }
    });
    out
}

/// Chroma pre-averaging on the white-balanced image: returns the image and per pixel the share of the averaged
/// chroma (0 = unchanged).
pub fn chroma_prefilter(img: &[Px], w: usize, h: usize, model: &Model, g: [f32; 3], par: &Parameter, white: f32) -> (Vec<Px>, Vec<f32>) {
    let inv = yuv_inverse();
    let gmax = g[0].max(g[1]).max(g[2]);
    let (full, ramp) = (par.dark_full * gmax, par.dark_ramp * gmax);
    let mut out = img.to_vec();
    let mut fraction = vec![0f32; w * h];
    out.par_chunks_mut(w).zip(fraction.par_chunks_mut(w)).enumerate().for_each(|(y, (row, a))| {
        if y == 0 || y + 1 >= h {
            return;
        }
        for x in 1..w - 1 {
            let c = img[y * w + x];
            let var = variance_wb(model, g, c, par, white);
            let mut outside = [0u32; 3];
            let mut sum = [0f32; 3];
            for dy in 0..3 {
                for dx in 0..3 {
                    let n = img[(y + dy - 1) * w + x + dx - 1];
                    for k in 0..3 {
                        let d = n[k] - c[k];
                        outside[k] += u32::from(d * d > var[k]);
                        sum[k] += n[k];
                    }
                }
            }
            let mean = sum.map(|s| s / 9.0);
            let gray = mean[2] >= 1.0 && (0..2).all(|k| (mean[k] / mean[2] - 1.0).abs() <= par.gray_tolerance);
            let two = outside.iter().filter(|&&n| n > par.edge_two).count() >= 2;
            let edge = two || outside.iter().any(|&n| n > par.edge_one);
            let luma = mul(&YUV, mean)[0];
            let dark = ((full + ramp - luma) / ramp).clamp(0.0, 1.0);
            let wgt = if gray { 1.0 } else if edge { 0.0 } else { dark };
            if wgt <= 0.0 {
                continue;
            }
            let ym = mul(&YUV, mean);
            let f = mul(&inv, [mul(&YUV, c)[0], ym[1], ym[2]]);
            row[x] = std::array::from_fn(|k| f[k] * wgt + c[k] * (1.0 - wgt));
            a[x] = wgt;
        }
    });
    (out, fraction)
}

/// Range filter of one scale (`level` 0 = grid, 1 = half size) on the pre-averaged, white-balanced image.
#[allow(clippy::too_many_arguments)]
pub fn region_filter(pre: &[Px], fraction: &[f32], w: usize, h: usize, model: &Model, g: [f32; 3], par: &Parameter, white: f32, level: usize) -> Vec<Px> {
    let r = par.radius[level];
    let k = par.region[level];
    // weights below the smallest one are ignored: exp(−k·D) ≥ min ⇔ D ≤ −ln(min)/k
    let d_max = -par.min_weight[level].max(1e-6).ln() / k.max(1e-9);
    let tv = averaged_variance();
    let pw = w + 2 * r;
    let pad = |c: usize| -> Vec<f32> {
        (0..(h + 2 * r) * pw)
            .into_par_iter()
            .map(|i| {
                let sx = ((i % pw) as isize - r as isize).clamp(0, w as isize - 1) as usize;
                let sy = ((i / pw) as isize - r as isize).clamp(0, h as isize - 1) as usize;
                pre[sy * w + sx][c]
            })
            .collect()
    };
    let q = [pad(0), pad(1), pad(2)];
    let mut out = vec![[0f32; 3]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let c = pre[y * w + x];
            let mut var = variance_wb(model, g, c, par, white);
            let s = fraction[y * w + x];
            if s > 0.0 {
                let v = var;
                var = std::array::from_fn(|i| (1.0 - s) * v[i] + s * (tv[i][0] * v[0] + tv[i][1] * v[1] + tv[i][2] * v[2]));
            }
            let inv: [f32; 3] = var.map(|v| 1.0 / v.max(1e-6));
            let (mut acc, mut sw) = ([0f32; 3], 0f32);
            for dy in 0..=2 * r {
                let base = (y + dy) * pw + x;
                let (l0, l1, l2) = (&q[0][base..base + 2 * r + 1], &q[1][base..base + 2 * r + 1], &q[2][base..base + 2 * r + 1]);
                for i in 0..=2 * r {
                    let (d0, d1, d2) = (l0[i] - c[0], l1[i] - c[1], l2[i] - c[2]);
                    let d = d0 * d0 * inv[0] + d1 * d1 * inv[1] + d2 * d2 * inv[2];
                    if d <= d_max {
                        let wt = (-k * d).exp();
                        sw += wt;
                        acc[0] += wt * l0[i];
                        acc[1] += wt * l1[i];
                        acc[2] += wt * l2[i];
                    }
                }
            }
            *o = if sw > 0.0 { acc.map(|a| a / sw) } else { c };
        }
    });
    out
}

/// Edge map: 1 − share of the neighbours in a `(2r+1)²` window that differ by more than `edges_threshold` noise
/// variances in any layer (unbalanced values, model without dark boost; border: 0).
pub fn edge_map(img: &[Px], w: usize, h: usize, model: &Model, par: &Parameter) -> Vec<f32> {
    let r = par.edges_radius;
    let mut out = vec![0f32; w * h];
    if w <= 2 * r + 1 || h <= 2 * r + 1 {
        return out;
    }
    let total = ((2 * r + 1) * (2 * r + 1) - 1) as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < r || y + r >= h {
            return;
        }
        for x in r..w - r {
            let c = img[y * w + x];
            let thr = model.variance(c).map(|v| v * par.edges_threshold);
            let mut n = 0u32;
            for yy in y - r..=y + r {
                for p in &img[yy * w + x - r..=yy * w + x + r] {
                    let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
                    n += u32::from(d[0] * d[0] > thr[0] || d[1] * d[1] > thr[1] || d[2] * d[2] > thr[2]);
                }
            }
            row[x] = (1.0 - n as f32 / total).clamp(0.0, 1.0);
        }
    });
    out
}

/// Two-way recursive smoothing of one line: forward and backward exponential sums with coefficient `k·e[x]`,
/// normalised by the sums of the weights (the centre counted once).
fn line(src: &[Px], e: &[f32], k: f32, out: &mut [Px]) {
    let n = src.len();
    let (mut f, mut nf) = (vec![[0f32; 3]; n], vec![0f32; n]);
    f[0] = src[0];
    nf[0] = 1.0;
    for x in 1..n {
        let a = e[x] * k;
        f[x] = std::array::from_fn(|c| f[x - 1][c] * a + src[x][c]);
        nf[x] = nf[x - 1] * a + 1.0;
    }
    let (mut b, mut nb) = ([0f32; 3], 0f32);
    for x in (0..n).rev() {
        let a = if x + 1 < n { e[x] * k } else { 0.0 };
        b = std::array::from_fn(|c| b[c] * a + src[x][c]);
        nb = nb * a + 1.0;
        out[x] = std::array::from_fn(|c| (f[x][c] + b[c] - src[x][c]) / (nf[x] + nb - 1.0));
    }
}

/// Edge-guided recursive smoothing, rows then columns.
pub fn recursive_smooth(img: &mut [Px], e: &[f32], w: usize, h: usize, k: f32) {
    if w < 3 || h < 3 {
        return;
    }
    let src = img.to_vec();
    img.par_chunks_mut(w).enumerate().for_each(|(y, row)| line(&src[y * w..(y + 1) * w], &e[y * w..(y + 1) * w], k, row));
    let cols: Vec<Vec<Px>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let s: Vec<Px> = (0..h).map(|y| img[y * w + x]).collect();
            let ec: Vec<f32> = (0..h).map(|y| e[y * w + x]).collect();
            let mut o = vec![[0f32; 3]; h];
            line(&s, &ec, k, &mut o);
            o
        })
        .collect();
    for (x, c) in cols.iter().enumerate() {
        for (y, v) in c.iter().enumerate() {
            img[y * w + x] = *v;
        }
    }
}

/// High ISO: the half scale `half` (enlarged) blended into the grid result `full` by brightness and agreement.
fn mix_high(out: &mut [Px], full: &[Px], half: &[Px], model: &Model, par: &Parameter) {
    out.par_iter_mut().zip(full.par_iter().zip(half.par_iter())).for_each(|(o, (v, u))| {
        let m = v[0].max(v[1]).max(v[2]);
        let mut a = (-(m - par.mix_v0) / par.mix_v1).exp().min(1.0);
        let var = model.variance(*u);
        let d: f32 = (0..3).map(|c| (u[c] - v[c]).powi(2) / var[c].max(1e-6)).sum();
        let r = ((u[0].max(u[1]).max(u[2]) - par.agreement_ab) / par.agreement_ramp.max(1e-3)).clamp(0.0, 1.0);
        a *= (-par.agreement * r * d).exp();
        *o = std::array::from_fn(|c| u[c] * a + v[c] * (1.0 - a));
    });
}

/// Low ISO: the half scale fills flat dark regions; the grid result stays at local brightness changes (relative
/// difference of the layer mean to its 8 neighbours, smoothed 3 × 3) and in bright regions.
fn mix_low(out: &mut [Px], full: &[Px], half: &[Px], orig: &[Px], w: usize, h: usize, par: &Parameter) {
    let mean = |p: &Px| (p[0] + p[1] + p[2]) / 3.0;
    let mut contrast = vec![0f32; w * h];
    contrast.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y == 0 || y + 1 >= h {
            return;
        }
        for x in 1..w - 1 {
            let c = mean(&full[y * w + x]);
            let mut f = 0.0;
            for (dy, dx) in [(0, 0), (0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)] {
                f += mean(&full[(y + dy - 1) * w + x + dx - 1]) / 8.0;
            }
            row[x] = if c <= 0.0 || f <= 0.0 { 0.0 } else { ((f / c).max(c / f) - 1.0) / par.contrast };
        }
    });
    const K: [[f32; 3]; 3] = [[1.0, 2.0, 1.0], [2.0, 4.0, 2.0], [1.0, 2.0, 1.0]];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = y * w + x;
            let (v, u) = (full[i], half[i]);
            let mut s = 0.0;
            if y > 0 && y + 1 < h && x > 0 && x + 1 < w {
                for dy in 0..3 {
                    for dx in 0..3 {
                        s += K[dy][dx] / 4.0 * contrast[(y + dy - 1) * w + x + dx - 1];
                    }
                }
            } else {
                row[x] = v;
                continue;
            }
            // share of the grid result: local change, brightness curve, bright regions
            let m = mean(&v);
            let bright_curve = 1.0 - (-(m - par.low_v0) / par.low_v1).exp().min(1.0);
            let bright = ((mean(&orig[i]) - par.bright_ab) / par.bright_ramp).clamp(0.0, 1.0);
            let keep = (s.clamp(0.0, 1.0).min(bright_curve) + bright).clamp(0.0, 1.0);
            row[x] = std::array::from_fn(|c| v[c] * keep + u[c] * (1.0 - keep));
        }
    });
}

/// Brightness smoothing of the full-resolution top layer (`w × h`, units of the stage) in place: 5 × 5 range filter
/// with weight `exp(−Δ²/s²)`, `s² = s₀ + s₁·v` of the centre value `v` ([`BRIGHTNESS_SCATTER`]), blended with the
/// unfiltered layer by [`brightness_fraction`].
pub fn brightness_smooth(top: &mut [f32], w: usize, h: usize, iso: f32) {
    brightness_smooth_with(top, w, h, 2, BRIGHTNESS_SCATTER, brightness_fraction(iso), 0.0);
}

/// [`brightness_smooth`] with window radius `r`, range spread `(s₀, s₁)` and share `fraction` of the filtered layer.
/// `guide`: share of the 3 × 3 mean in the reference the range weights compare the neighbours with (0 = the centre
/// value itself); with a share, a centre far outside the noise of its neighbours is smoothed as well (cross range
/// filter with a partly pre-smoothed guide).
#[allow(clippy::too_many_arguments)]
pub fn brightness_smooth_with(top: &mut [f32], w: usize, h: usize, r: usize, scatter: (f32, f32), fraction: f32, guide: f32) {
    if fraction <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let fraction = fraction.min(1.0);
    #[allow(non_snake_case)]
    let R = r as isize;
    let (s0, s1) = scatter;
    let src = top.to_vec();
    let at = |x: isize, y: isize| src[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize];
    top.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let c0 = src[y * w + x];
            let c = if guide > 0.0 {
                let (xi, yi) = (x as isize, y as isize);
                let mut m = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        m += at(xi + dx, yi + dy);
                    }
                }
                c0 + guide * (m / 9.0 - c0)
            } else {
                c0
            };
            let inv = 1.0 / (s0 + s1 * c.max(0.0));
            let (mut s, mut n) = (0f32, 0f32);
            for dy in -R..=R {
                let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                for dx in -R..=R {
                    let v = src[yy * w + (x as isize + dx).clamp(0, w as isize - 1) as usize];
                    let wt = (-(v - c) * (v - c) * inv).exp();
                    s += wt * v;
                    n += wt;
                }
            }
            *o = fraction * (s / n) + (1.0 - fraction) * c0;
        }
    });
}

/// Range spread of the brightness smoothing, `s² = s₀ + s₁·v` (fitted on single-pixel probes against the
/// camera maker's output, deviation ≤ 1.2 % for v = 30 … 12000).
pub const BRIGHTNESS_SCATTER: (f32, f32) = (208.0, 4.4);

/// Share of the filtered top layer by capture ISO: `0.75 − 0.05·log₂(ISO/400)`, between 0 and 1 (fitted on
/// small single-pixel probes at the third-stop ISO values 400 … 5000, deviation ≤ 0.0012).
pub fn brightness_fraction(iso: f32) -> f32 {
    (0.75 - 0.05 * (iso / 400.0).max(1e-3).log2()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variance_of_averaging() {
        let t = averaged_variance();
        // the averaged chroma lowers the variance of every layer
        for row in t {
            assert!(row.iter().sum::<f32>() < 1.0, "{row:?}");
        }
    }

    #[test]
    fn flat_stays_flat_and_noise_drops() {
        let (w, h) = (96, 80);
        let mut s = 12345u32;
        let mut rnd = || {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let before: Vec<Px> = (0..w * h).map(|_| [300.0 + 40.0 * rnd(), 280.0 + 30.0 * rnd(), 260.0 + 10.0 * rnd()]).collect();
        for iso in [100.0, 6400.0] {
            let mut im = before.clone();
            let image = Image { gains: [1.2, 1.0, 0.9], iso, noise_div: 1.0, white: 14336.0 };
            denoise(&mut im, w, h, &image, &MODEL, &PARAMETER);
            let mean = |v: &[Px], c: usize| v.iter().map(|p| p[c] as f64).sum::<f64>() / v.len() as f64;
            let sd = |v: &[Px], c: usize| {
                let m = mean(v, c);
                (v.iter().map(|p| (p[c] as f64 - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
            };
            for c in 0..3 {
                assert!((mean(&im, c) - mean(&before, c)).abs() < 2.0, "mean {c} ISO {iso}");
                assert!(sd(&im, c) < 0.6 * sd(&before, c), "noise {c} ISO {iso}: {} → {}", sd(&before, c), sd(&im, c));
            }
        }
    }

    #[test]
    fn brightness_edges_stay() {
        let (w, h) = (64, 48);
        let mut img: Vec<f32> = (0..w * h).map(|i| if i % w < w / 2 { 50.0 } else { 3000.0 } + ((i * 7919) % 13) as f32 - 6.0).collect();
        brightness_smooth(&mut img, w, h, 800.0);
        for y in 0..h {
            assert!(img[y * w + w / 2 - 1] < 200.0 && img[y * w + w / 2] > 2800.0);
        }
    }
}
