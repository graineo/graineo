//! High-ISO chain, step 2a: colour noise reduction of the colour grid.
//!
//! The grid is filtered on two scales, at its own size and at half size, and the two results are mixed by
//! brightness (Burt and Adelson 1983; multi-scale range filtering after Zhang and Gunturk 2008). Each scale runs
//! the same passes on the white-balanced grid:
//!
//! 1. Chroma pre-filter: where a cell is dark or grey and no edge runs through its 3 × 3 neighbourhood, its colour
//!    differences (BT.601) become the neighbourhood mean; the luma stays.
//! 2. Range filter over a square window (Tomasi and Manduchi 1998): the weight of a neighbour falls exponentially
//!    with its squared distance to the centre in units of the noise variance, which follows from the sensor's
//!    noise model (variance a + b·v + c·v², raised towards black).
//! 3. Edge-aware recursive mean (Deriche 1987; Gastal and Oliveira 2011): a map of how many neighbours lie within
//!    the noise of each cell controls how far a forward and a backward running mean reach along rows and columns.
//!
//! Input and output: the grid in the units of the chain, before the white balance.

use rayon::prelude::*;

use super::Px;

/// Noise variance of a grid value per layer (bottom, middle, top mean): constant, linear and quadratic part.
const MODEL: [[f64; 3]; 3] = [[97.448, 0.1934, -5e-7], [60.657, 0.11, -2e-7], [5.21075, 0.10855, 2e-7]];

/// Strength of the range filter per colour step, for the full and the half scale.
const STRENGTH: [[f32; 5]; 2] = [[1.0, 0.5, 0.1, 0.05, 0.02], [1.5, 1.0, 0.5, 0.1, 0.06]];
/// The same for long exposures: fixed values for the full scale, factors on a polynomial of the ISO stops for the
/// half scale.
const NIGHT_STRENGTH: [f32; 5] = [0.5, 0.25, 0.05, 0.025, 0.01];
const NIGHT_HALF: [f64; 3] = [5.0, 2.5, 1.0];

/// Window radius and smallest counted weight of the range filter per scale.
const WINDOW: [(usize, f32); 2] = [(5, 0.3), (15, 0.75)];
/// Steps of the weight curve e^(−8·i/2047).
pub(super) const WEIGHT_STEPS: usize = 2048;
/// Entries of the curve that raises the variance towards black.
const DARK_STEPS: usize = 14384;
/// Radius of the edge map window and the reach of the recursive mean on a flat area.
const EDGE_RADIUS: usize = 5;
const REACH: f32 = 0.9;

/// In cells the pre-filter smoothed as dark, the variances of the three layers are mixed with this matrix.
const DARK_MIX: [[f64; 3]; 3] = [
    [0.1440004023245225, 0.3828545816329895, 0.014439858093753227],
    [0.10394208867656735, 0.35594662117735154, 0.01666031421879885],
    [0.09933340423738898, 0.3828538306536156, 0.10021776990222239],
];

/// Settings of the stage for one image.
#[derive(Clone, Copy, Debug)]
pub struct GridDenoise {
    /// daylight white balance on the grid values, middle layer = 1
    pub gains: [f64; 3],
    /// colour noise step 0 … 4 (2 = camera setting)
    pub step: usize,
    /// factor on the noise variance (block means: 0.25)
    pub variance: f64,
    /// long exposure, see [`night`]
    pub night: Option<Night>,
}

/// Long exposure: the half scale keeps at least `share` of the mix, and the filter strengths change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Night {
    pub share: f32,
    /// ISO stops above 100 (0 … 6)
    pub stops: f64,
}

/// Long-exposure settings from the capture ISO and the dark-current score (shutter time in ms times the doubling of
/// the dark current per 8 °C); `None` up to a score of 160 000.
pub fn night(capture_iso: f64, score: f32) -> Option<Night> {
    (score > 160_000.0).then(|| Night { share: (0.2 + 0.8 * (score - 160_000.0) / 320_000.0).clamp(0.0, 1.0), stops: (capture_iso.clamp(100.0, 6400.0) / 100.0).log2() })
}

impl GridDenoise {
    fn step(&self) -> usize {
        self.step.min(4)
    }

    /// Strength of the range filter on `scale` (0 = full, 1 = half).
    fn strength(&self, scale: usize) -> f32 {
        match (self.night, scale) {
            (None, _) => STRENGTH[scale][self.step()],
            (Some(_), 0) => NIGHT_STRENGTH[self.step()],
            (Some(n), _) => {
                let l = n.stops;
                let base = 0.000202 * l.powi(4) - 0.002618 * l.powi(3) + 0.009848 * l * l - 0.002074 * l + 0.007;
                (base * NIGHT_HALF[self.step().min(2)]) as f32
            }
        }
    }
}

/// Rounds to the nearest whole number, halves upwards.
#[inline]
fn nearest(v: f64) -> usize {
    (v + 1.5e-8).round_ties_even().max(0.0) as usize
}

/// Factor on the variance by brightness: 1 at white, `1 + rise` at black.
fn dark_curve(rise: f32) -> Vec<f32> {
    let mut curve: Vec<f32> = (0..DARK_STEPS).map(|i| (i as f32 / DARK_STEPS as f32).powf(4.0) * rise + 1.0).collect();
    curve.push(rise + 1.0);
    curve
}

/// The noise model of one scale.
struct Noise {
    /// per layer: constant, linear, quadratic
    model: [[f64; 3]; 3],
    dark: Vec<f32>,
}

impl Noise {
    /// Variance of value `v` of layer `c`, raised towards black.
    #[inline]
    fn raised(&self, c: usize, v: f32) -> f32 {
        let [a, b, q] = self.model[c];
        let u = f64::from(v.max(0.0));
        let at = 14382 - f64::from(v.clamp(1.0, 14382.0)).floor() as usize;
        ((q * u + b) * u + a) as f32 * self.dark[at]
    }

    /// Plain variance (magnitude) of value `v` of layer `c`.
    #[inline]
    fn plain(&self, c: usize, v: f32) -> f32 {
        let [a, b, q] = self.model[c];
        let v = f64::from(v);
        (q * v * v + b * v + a).abs() as f32
    }

    fn balanced(&self, gains: &[f64; 3]) -> Noise {
        Noise { model: std::array::from_fn(|c| [gains[c] * gains[c] * self.model[c][0], gains[c] * self.model[c][1], self.model[c][2]]), dark: self.dark.clone() }
    }
}

#[inline]
fn yuv(p: &Px) -> [f32; 3] {
    let (r, g, b) = (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
    [(0.299 * r + 0.587 * g + 0.114 * b) as f32, ((-0.28886 * g - 0.14713 * r) + 0.436 * b) as f32, ((-0.10001 * b - 0.51499 * g) + 0.615 * r) as f32]
}

#[inline]
fn rgb(y: f32, u: f32, v: f32) -> Px {
    let (y, u, v) = (f64::from(y), f64::from(u), f64::from(v));
    [(v * 1.13983 + y) as f32, ((v * -0.5806 + u * -0.39465) + y) as f32, (u * 2.03211 + y) as f32]
}

/// Chroma pre-filter of the white-balanced grid `img`. Returns the filtered grid and, per cell, whether it was
/// smoothed as a dark cell (the range filter mixes the variances there).
fn prefilter(img: &[Px], w: usize, h: usize, noise: &Noise, gains: &[f64; 3]) -> (Vec<Px>, Vec<bool>) {
    let top_gain = gains[0].max(gains[1]).max(gains[2]) as f32;
    let (full_until, none_from, dark_until) = (200.0 * top_gain, 225.0 * top_gain, 212.5 * top_gain);
    let black = (noise.model[0][0].sqrt() * 6.0) as f32;
    let mut out = img.to_vec();
    let mut dark = vec![false; w * h];
    if w < 3 || h < 3 {
        return (out, dark);
    }
    out.par_chunks_mut(w).zip(dark.par_chunks_mut(w)).enumerate().for_each(|(y, (row, dark_row))| {
        if y == 0 || y + 1 == h {
            return;
        }
        for x in 1..w - 1 {
            let centre = &img[y * w + x];
            let variance: [f32; 3] = std::array::from_fn(|c| noise.raised(c, centre[c]));
            let mut beyond = [0u32; 3];
            let (mut mean, mut mean_yuv) = ([0f32; 3], [0f32; 3]);
            for yy in y - 1..=y + 1 {
                for n in &img[yy * w + x - 1..=yy * w + x + 1] {
                    let t = yuv(n);
                    for c in 0..3 {
                        let d = n[c] - centre[c];
                        beyond[c] += u32::from(variance[c] < d * d);
                        mean[c] += n[c] * (1.0 / 9.0);
                        mean_yuv[c] += t[c] * (1.0 / 9.0);
                    }
                }
            }
            let is_dark = mean_yuv[0] <= black;
            let edge = (beyond[1] > 2 && beyond[0] > 2) || (beyond[2] > 2 && (beyond[1] > 2 || beyond[0] > 2)) || beyond.iter().any(|&n| n > 6);
            let grey = !is_dark && mean.iter().all(|&m| m >= 1.0) && {
                let (rb, gb) = (f64::from(mean[0]) / f64::from(mean[2]), f64::from(mean[1]) / f64::from(mean[2]));
                (0.95..=1.05).contains(&rb) && (0.95..=1.05).contains(&gb)
            };
            if !(is_dark || !edge || grey) {
                continue;
            }
            // grey cells take the smoothed colour fully, dark ones up to a luma limit with a short fade
            let level = mean_yuv[0].trunc();
            let share = if grey {
                1.0
            } else if mean_yuv[0] > dark_until || level >= none_from {
                0.0
            } else if level <= full_until {
                1.0
            } else {
                (none_from - level) / (none_from - full_until)
            };
            if share <= 0.0 {
                continue;
            }
            dark_row[x] = !grey;
            let smooth = rgb(yuv(centre)[0], mean_yuv[1], mean_yuv[2]);
            for c in 0..3 {
                row[x][c] = smooth[c] * share + (f64::from(centre[c]) * (1.0 - f64::from(share))) as f32;
            }
        }
    });
    (out, dark)
}

/// Per cell the three factors that turn a squared layer difference into steps of the weight curve.
fn range_factors(img: &[Px], dark: &[bool], noise: &Noise, strength: f32) -> Vec<Px> {
    let steps = (WEIGHT_STEPS - 1) as f32 / (8.0 / strength);
    img.par_iter()
        .zip(dark.par_iter())
        .map(|(p, &mixed)| {
            let mut v: [f32; 3] = std::array::from_fn(|c| noise.raised(c, p[c]));
            if mixed {
                let plain = v.map(f64::from);
                v = std::array::from_fn(|i| ((plain[2] * DARK_MIX[i][2] + plain[1] * DARK_MIX[i][1]) + plain[0] * DARK_MIX[i][0]) as f32);
            }
            v.map(|x| steps / x)
        })
        .collect()
}

/// The weight curve of a scale: e^(−8·i/2047), zero below `floor`.
pub(super) fn weight_curve(floor: f32) -> Vec<f32> {
    (0..WEIGHT_STEPS)
        .map(|i| {
            let w = (i as f32 * (-8.0 / (WEIGHT_STEPS - 1) as f32)).exp();
            if w >= floor { w } else { 0.0 }
        })
        .collect()
}

/// Range filter: every cell becomes the weighted mean of the `(2r+1)²` cells around it (edge cells repeated).
pub(super) fn range_filter(img: &[Px], factors: &[Px], w: usize, h: usize, r: usize, curve: &[f32]) -> Vec<Px> {
    let mut out = vec![[0f32; 3]; w * h];
    let r = r as isize;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let lines: Vec<usize> = (-r..=r).map(|dy| (y as isize + dy).clamp(0, h as isize - 1) as usize * w).collect();
        for (x, o) in row.iter_mut().enumerate() {
            let (centre, f) = (img[y * w + x], factors[y * w + x]);
            let (mut total, mut acc) = (0f32, [0f32; 3]);
            let inside = x as isize >= r && x as isize + r < w as isize;
            for &line in &lines {
                let mut take = |n: &Px| {
                    let (d0, d1, d2) = (n[0] - centre[0], n[1] - centre[1], n[2] - centre[2]);
                    let step = ((d2 * d2 * f[2] + d1 * d1 * f[1]) + d0 * d0 * f[0]) + 0.5;
                    if step < WEIGHT_STEPS as f32 {
                        let wt = curve[step as usize];
                        total += wt;
                        acc[0] += wt * n[0];
                        acc[1] += wt * n[1];
                        acc[2] += wt * n[2];
                    }
                };
                if inside {
                    img[line + x - r as usize..=line + x + r as usize].iter().for_each(&mut take);
                } else {
                    for dx in -r..=r {
                        take(&img[line + (x as isize + dx).clamp(0, w as isize - 1) as usize]);
                    }
                }
            }
            *o = if total > 0.0 { acc.map(|v| v / total) } else { centre };
        }
    });
    out
}

/// Per cell the squared distance per layer beyond which a neighbour counts as lying across an edge: three noise
/// variances.
fn edge_limits(img: &[Px], noise: &Noise) -> Vec<Px> {
    img.par_iter().map(|p| std::array::from_fn(|c| noise.plain(c, p[c]) * 3.0)).collect()
}

/// Edge map: 1 where all cells of the 11 × 11 window lie within the centre's `limits` in every layer, falling to 0
/// with the share of cells that do not. A border stays 0 (the recursive mean stops there).
fn edge_map(img: &[Px], limits: &[Px], w: usize, h: usize) -> Vec<f32> {
    let r = EDGE_RADIUS;
    let mut map = vec![0f32; w * h];
    if w < 2 * r + 3 || h < 2 * r + 1 {
        return map;
    }
    map.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < r || y + r > h {
            return;
        }
        for x in r..w - r - 1 {
            let (centre, limit) = (&img[y * w + x], &limits[y * w + x]);
            let mut beyond = 0u32;
            for yy in y - r..=(y + r).min(h - 1) {
                for n in &img[yy * w + x - r..=yy * w + x + r] {
                    let d = [n[0] - centre[0], n[1] - centre[1], n[2] - centre[2]];
                    beyond += u32::from(d[0] * d[0] > limit[0]) | u32::from(d[1] * d[1] > limit[1]) | u32::from(d[2] * d[2] > limit[2]);
                }
            }
            row[x] = (1.0 - beyond as f32 / 120.0).clamp(0.0, 1.0);
        }
    });
    map
}

/// Forward and backward running mean along one line; `reach(j)` (0 … 1) is how much of the sum so far cell `j`
/// takes over. `scratch` holds the forward pass.
fn running_mean(line: &mut [Px], reach: impl Fn(usize) -> f32, scratch: &mut Vec<(Px, f32)>) {
    let n = line.len();
    if n < 2 {
        return;
    }
    scratch.clear();
    scratch.push((line[0], 1.0));
    for j in 1..n {
        let k = reach(j) * REACH;
        let (sum, count) = scratch[j - 1];
        scratch.push((std::array::from_fn(|c| sum[c] * k + line[j][c]), count * k + 1.0));
    }
    let (mut sum, mut count) = (line[n - 1], 1.0f32);
    line[n - 1] = scratch[n - 1].0.map(|v| v / scratch[n - 1].1);
    for j in (0..n - 1).rev() {
        let k = reach(j) * REACH;
        let x = line[j];
        sum = std::array::from_fn(|c| sum[c] * k + x[c]);
        count = count * k + 1.0;
        line[j] = if j == 0 {
            sum.map(|v| v / count)
        } else {
            let (forward, forward_count) = scratch[j];
            std::array::from_fn(|c| ((forward[c] + sum[c]) - x[c]) / ((forward_count + count) - 1.0))
        };
    }
}

/// Edge-aware recursive mean of `img`: along the rows, then along the columns of the result.
fn smooth_within_edges(img: &mut [Px], map: &[f32], w: usize, h: usize) {
    img.par_chunks_mut(w).zip(map.par_chunks(w)).for_each_init(Vec::new, |scratch, (row, reach)| running_mean(row, |j| reach[j], scratch));
    let mut turned = vec![[0f32; 3]; w * h];
    let rows: &[Px] = img;
    turned.par_chunks_mut(h).enumerate().for_each_init(Vec::new, |scratch, (x, column)| {
        for (y, v) in column.iter_mut().enumerate() {
            *v = rows[y * w + x];
        }
        running_mean(column, |y| map[y * w + x], scratch);
    });
    img.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = turned[x * h + y];
        }
    });
}

/// All passes of one scale on `img` (`w × h`).
fn scale_pass(img: &[Px], w: usize, h: usize, p: &GridDenoise, scale: usize, clock: &mut super::Clock) -> Vec<Px> {
    let share = if scale == 0 { 1.0 } else { 0.25 } * p.variance;
    let rise = if scale == 1 && p.night.is_some() { 5.0 } else { 31.0 };
    let noise = Noise { model: MODEL.map(|m| m.map(|v| v * share)), dark: dark_curve(rise) };
    let gains = p.gains.map(|g| g as f32);
    let balanced: Vec<Px> = img.par_iter().map(|v| [v[0] * gains[0], v[1] * gains[1], v[2] * gains[2]]).collect();
    let balanced_noise = noise.balanced(&p.gains);
    let (pre, dark) = prefilter(&balanced, w, h, &balanced_noise, &p.gains);
    drop(balanced);
    let factors = range_factors(&pre, &dark, &balanced_noise, p.strength(scale));
    let (radius, floor) = WINDOW[scale];
    let curve = weight_curve(floor);
    clock.lap(if scale == 0 { "grid: pre-filter, full scale" } else { "grid: pre-filter, half scale" });
    let mut out = super::gpu::range_filter(&pre, &factors, w, h, radius, &curve).unwrap_or_else(|| range_filter(&pre, &factors, w, h, radius, &curve));
    out.par_iter_mut().for_each(|v| *v = [v[0] / gains[0], v[1] / gains[1], v[2] / gains[2]]);
    clock.lap(if scale == 0 { "grid: range filter 11 x 11" } else { "grid: range filter 31 x 31" });
    let limits = edge_limits(img, &noise);
    let map = edge_map(img, &limits, w, h);
    clock.lap(if scale == 0 { "grid: edge map, full scale" } else { "grid: edge map, half scale" });
    smooth_within_edges(&mut out, &map, w, h);
    clock.lap(if scale == 0 { "grid: edge-aware mean, full scale" } else { "grid: edge-aware mean, half scale" });
    out
}

/// Size of the half scale along an axis of `n` cells: the smallest even number of at least half.
fn half_size(n: usize) -> usize {
    (n.div_ceil(2) + 1) & !1
}

/// Resamples `src` (`w × h`) to `nw × nh` with the taps `taps(position, source length)`, rows first.
fn resample<const N: usize>(src: &[Px], w: usize, h: usize, nw: usize, nh: usize, taps: fn(usize, usize) -> [(usize, f32); N]) -> Vec<Px> {
    let mut wide = vec![[0f32; 3]; nw * h];
    wide.par_chunks_mut(nw).zip(src.par_chunks(w)).for_each(|(out, line)| {
        for (x, o) in out.iter_mut().enumerate() {
            for (at, wt) in taps(x, w) {
                *o = std::array::from_fn(|c| o[c] + wt * line[at][c]);
            }
        }
    });
    let mut out = vec![[0f32; 3]; nw * nh];
    out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
        for (at, wt) in taps(y, h) {
            for (o, v) in row.iter_mut().zip(&wide[at * nw..(at + 1) * nw]) {
                *o = std::array::from_fn(|c| o[c] + wt * v[c]);
            }
        }
    });
    out
}

/// Halving with the binomial weights (1 3 3 1)/8 around the pair of cells 2i, 2i + 1.
fn halve_taps(i: usize, n: usize) -> [(usize, f32); 4] {
    let at = |d: isize| (2 * i as isize + d).clamp(0, n as isize - 1) as usize;
    [(at(-1), 0.125), (at(0), 0.375), (at(1), 0.375), (at(2), 0.125)]
}

/// Doubling: linear between the two nearest cells (¾ and ¼).
fn double_taps(i: usize, n: usize) -> [(usize, f32); 2] {
    let cell = (i / 2) as isize;
    let other = if i % 2 == 0 { cell - 1 } else { cell + 1 };
    [(cell.clamp(0, n as isize - 1) as usize, 0.75), (other.clamp(0, n as isize - 1) as usize, 0.25)]
}

/// Brightness offset and steps of the curve that mixes the scales.
const MIX_OFFSET: f64 = 448.21875;
const MIX_STEPS: usize = 14343;

/// Mixes the half scale `coarse` (enlarged) into the full scale `fine` (in place): mostly in the dark, less where
/// the two scales disagree (an edge the half scale blurred); `least` is the smallest share of the half scale.
fn mix_scales(fine: &mut [Px], coarse: &[Px], least: f32) {
    let by_brightness: Vec<f32> = (0..MIX_STEPS)
        .map(|i| {
            let y = ((4.0f32 - i as f32 * 0.000_209_161_27) - 1.0) / 3.0;
            ((y.powf(64.0) * 3.0 + 1.0) - 1.0) / 3.0
        })
        .collect();
    let agreement: Vec<f32> = (0..1024).map(|i| f64::from(i as f32 / -1023.0 * 3.0).tanh().clamp(-1.0, 1.0) as f32 + 1.0).collect();
    fine.par_iter_mut().zip(coarse.par_iter()).for_each(|(f, c)| {
        let peak = f[0].max(f[1].max(f[2]));
        let mut share = by_brightness[nearest((f64::from(peak) - MIX_OFFSET).clamp(1.0, MIX_STEPS as f64)) - 1];
        if c.iter().all(|&v| v > 0.0) {
            let apart: [f32; 3] = std::array::from_fn(|k| (c[k] - f[k]).abs().powf(1.5) / c[k]);
            let bright = (c[0].max(c[1].max(c[2])).max(200.0) - 200.0).min(2800.0) / 2800.0;
            let t = ((bright * ((apart[1] + apart[0]) + apart[2])).min(3.0) / 3.0 * 1023.0).clamp(0.0, 1023.0);
            share *= agreement[nearest(f64::from(t))];
        }
        if least > 0.0 {
            share = share.max(least);
        }
        *f = std::array::from_fn(|k| c[k] * share + (f64::from(f[k]) * (1.0 - f64::from(share))) as f32);
    });
}

/// The stage: `grid` (`w × h`) → denoised grid.
pub fn denoise(grid: &[Px], w: usize, h: usize, p: &GridDenoise) -> Vec<Px> {
    assert_eq!(grid.len(), w * h, "grid size");
    let mut clock = super::Clock::start();
    let mut fine = scale_pass(grid, w, h, p, 0, &mut clock);
    let (hw, hh) = (half_size(w), half_size(h));
    let small = resample(grid, w, h, hw, hh, halve_taps);
    let coarse = scale_pass(&small, hw, hh, p, 1, &mut clock);
    let coarse = resample(&coarse, hw, hh, w, h, double_taps);
    mix_scales(&mut fine, &coarse, p.night.map_or(0.0, |n| n.share));
    clock.lap("grid: scales mixed");
    fine
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> GridDenoise {
        GridDenoise { gains: [1.2, 1.0, 1.1], step: 2, variance: 1.0, night: None }
    }

    use crate::quattro_hi::tests::noise;

    #[test]
    fn flat_grid_stays_and_noise_shrinks() {
        let (w, h) = (96, 64);
        let flat = vec![[800f32, 700.0, 600.0]; w * h];
        let out = denoise(&flat, w, h, &settings());
        assert!(out.iter().all(|p| (p[0] - 800.0).abs() < 0.05 && (p[1] - 700.0).abs() < 0.05 && (p[2] - 600.0).abs() < 0.05));
        let noisy: Vec<Px> = (0..w * h).map(|i| [800.0 + 12.0 * noise(3 * i), 700.0 + 10.0 * noise(3 * i + 1), 600.0 + 8.0 * noise(3 * i + 2)]).collect();
        let out = denoise(&noisy, w, h, &settings());
        let spread = |g: &[Px]| (g.iter().map(|p| (p[0] - 800.0).powi(2)).sum::<f32>() / g.len() as f32).sqrt();
        assert!(spread(&out) < 0.3 * spread(&noisy), "{} against {}", spread(&out), spread(&noisy));
    }

    #[test]
    fn a_strong_edge_survives() {
        let (w, h) = (96, 64);
        let img: Vec<Px> = (0..w * h).map(|i| if i % w < w / 2 { [400.0; 3] } else { [4000.0; 3] }).collect();
        let out = denoise(&img, w, h, &settings());
        // the half scale reaches a few cells across the edge on its dark side; further away both sides stay
        let (left, right) = (out[32 * w + w / 2 - 12], out[32 * w + w / 2 + 11]);
        assert!((left[1] - 400.0).abs() < 20.0 && (right[1] - 4000.0).abs() < 40.0, "{left:?} {right:?}");
        let (before, after) = (out[32 * w + w / 2 - 1], out[32 * w + w / 2]);
        assert!(after[1] - before[1] > 1000.0, "{before:?} {after:?}");
    }

    #[test]
    fn resampling_keeps_a_constant_and_sizes() {
        assert_eq!((half_size(5424), half_size(2713), half_size(7)), (2712, 1358, 4));
        let (w, h) = (10, 6);
        let img = vec![[5f32, 6.0, 7.0]; w * h];
        let small = resample(&img, w, h, half_size(w), half_size(h), halve_taps);
        let back = resample(&small, half_size(w), half_size(h), w, h, double_taps);
        assert!(back.iter().all(|p| (p[0] - 5.0).abs() < 1e-5 && (p[2] - 7.0).abs() < 1e-5));
    }

    #[test]
    fn long_exposures_start_above_the_score() {
        assert_eq!(night(1600.0, 100_000.0), None);
        let n = night(1600.0, 320_000.0).unwrap();
        assert!((n.share - 0.6).abs() < 1e-6 && (n.stops - 4.0).abs() < 1e-9);
        let p = GridDenoise { night: Some(n), ..settings() };
        assert_eq!(p.strength(0), 0.05);
        assert!(p.strength(1) > 0.0);
    }
}
