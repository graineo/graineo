//! High-ISO chain, step 3: from the colour grid back to the resolution of the top layer.
//!
//! The top layer is the only one stored at full resolution. This step first calms its brightness noise (single
//! spikes clamped to their neighbours, then a value-range filter over an 11 × 11 window whose weights come from the
//! sensor's noise model), enlarges the unfiltered grid 2× (cubic convolution, Keys 1981) and scales it with the
//! ratio of the top layer to its own enlarged copy (ratio pan-sharpening) to get a guide image. Every full-resolution
//! pixel then takes the colour of the colour stage as a weighted mean of the 5 × 5 grid cells around it, weighted by
//! distance and by the similarity of the denoised grid to the guide (joint bilateral upsampling, Kopf et al. 2007).
//! The brightness detail comes back as the ratio of the smoothed top layer to the same weighted mean of the grid's
//! top values; that ratio is damped for strongly coloured cells and scaled in the log domain (a power of the ratio,
//! Durand and Dorsey 2002). A high-pass of the result is kept as the detail signal for the sharpening that follows.
//!
//! Units: sensor values above black divided by SensorISO/100 (inputs), 16-bit output values of the colour stage.

use rayon::prelude::*;

use super::curve;
use super::{Px, TOP_MODEL};

/// Settings of one reconstruction.
#[derive(Clone, Copy, Debug)]
pub struct Upscale {
    /// capture ISO / 100
    pub iso_factor: f32,
    /// luminance noise step 0 … 4 (2 = camera setting)
    pub luma_step: usize,
    /// detail step 0 … 6 (3 = camera setting)
    pub detail_step: usize,
    /// white of the top layer in the units of the stage
    pub white: f32,
    /// the inputs are block means (binned output)
    pub pooled: bool,
}

/// Result of [`upscale`]: interleaved RGB (16-bit value range, `2gw × 2gh`) and the detail signal per pixel.
pub struct Upscaled {
    pub rgb: Vec<f32>,
    pub detail: Vec<f32>,
}

/// Window radius of the value-range filter on the top layer.
const RANGE_RADIUS: usize = 5;
/// Weights below this count as zero in the value-range filter.
const RANGE_CUTOFF: f32 = 0.25;
/// Number of brightness classes of the value-range filter and their width in stage units.
const CLASSES: usize = 1024;
const CLASS_WIDTH: f64 = 16.0;
/// Offset that keeps the brightness ratios finite near black.
const RATIO_OFFSET: f32 = 5.0;
/// Highlights: from here on a darkening ratio fades out over [`HIGHLIGHT_SPAN`] units.
const HIGHLIGHT_START: f32 = 11000.0;
const HIGHLIGHT_SPAN: f32 = 3000.0;
/// Largest share by which the colour gate damps the brightness ratio.
const GATE_DEPTH: f32 = 0.3;

/// Strength of the value-range filter per luminance step (larger = narrower range = less smoothing).
const RANGE_STRENGTH: [f64; 5] = [0.2, 0.125, 0.1, 0.075, 0.05];
/// The same for block means.
const RANGE_STRENGTH_POOLED: f64 = 0.1;

/// Share of the smoothed top layer by brightness (15 nodes, see [`curve::NODES`]) per luminance step.
const SMOOTH_SHARE: [[f64; 15]; 5] = [
    [0.0; 15],
    [0.33, 0.33, 0.33, 0.33, 0.33, 0.33, 0.33, 0.33, 0.34, 0.34, 0.36, 0.39, 0.43, 0.46, 0.46],
    [0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.51, 0.52, 0.53, 0.56, 0.6, 0.66, 0.7, 0.7],
    [0.62, 0.62, 0.62, 0.62, 0.62, 0.62, 0.62, 0.63, 0.64, 0.66, 0.69, 0.74, 0.82, 0.87, 0.87],
    [0.72, 0.72, 0.72, 0.72, 0.72, 0.72, 0.72, 0.73, 0.74, 0.76, 0.8, 0.86, 0.94, 1.0, 1.0],
];

/// Share of the smoothed top layer for block means: one value per ISO factor the cameras offer, 0.55 elsewhere.
pub fn pooled_smooth_share(iso_factor: f32) -> f32 {
    const BY_ISO: [(f32, f32); 12] =
        [(4.0, 0.75), (5.0, 0.733), (6.4, 0.717), (8.0, 0.70), (10.0, 0.683), (12.5, 0.667), (16.0, 0.65), (20.0, 0.633), (25.0, 0.617), (32.0, 0.60), (40.0, 0.583), (50.0, 0.567)];
    BY_ISO.iter().find(|(iso, _)| *iso == iso_factor).map_or(0.55, |(_, share)| *share)
}

impl Upscale {
    fn luma(&self) -> usize {
        self.luma_step.min(4)
    }

    /// Strength of the value-range filter on the top layer.
    fn range_strength(&self) -> f64 {
        if self.pooled { RANGE_STRENGTH_POOLED } else { RANGE_STRENGTH[self.luma()] }
    }

    /// Does the smoothed top layer come in with one share for all brightness values?
    fn constant_share(&self) -> bool {
        self.pooled || self.iso_factor < 1.0
    }

    /// Dead band of the detail signal (before the ISO factor) and the exponent applied to the brightness ratio.
    pub fn detail_tuning(&self) -> (f32, f32) {
        let iso = f64::from(self.iso_factor);
        let detail = self.detail_step.min(6);
        let dead_band = if self.pooled {
            [0.0, 5.0, 10.0, 15.0, 20.0][self.luma()]
        } else {
            let stops = iso.log2();
            let base = 16.0 - 15.0 * stops * stops / 36.0;
            let by_step = base * [1.0, 1.0, 1.2, 1.5, 2.0][self.luma()];
            if iso >= 8.0 { by_step * 1.5 } else { by_step }
        };
        let mut amount: f64 = if self.pooled { [0.0, 0.1, 0.3, 0.5, 0.9, 1.8, 2.8][detail] } else { [0.0, 0.0, 0.0, 0.0, 1.0, 1.25, 1.5][detail] };
        if !self.pooled && detail == 6 && iso > 8.0 {
            amount *= 0.75;
        }
        if !self.pooled && iso >= 8.0 && amount > 1.3 {
            amount /= 1.140_175_461_769_104;
        }
        let wide = ((1.0 - (iso - 1.0) / 15.0) * 0.6 + 1.2).max(1.2);
        let narrow = ((1.0 - (iso - 1.0) * 0.125) * 0.3 + 1.1).max(1.1);
        let reach = if self.pooled {
            wide
        } else if iso > 16.0 {
            1.0
        } else {
            narrow
        };
        (dead_band as f32, ((reach - 1.0) * amount + 1.0) as f32)
    }
}

/// Noise deviation of a top-layer value.
#[inline]
fn top_sigma(v: f32) -> f32 {
    let v = f64::from(v);
    ((TOP_MODEL[2] * v * v + TOP_MODEL[1] * v.abs() + TOP_MODEL[0]) as f32).sqrt()
}

/// Clamps single spikes of the top layer: a value that is the strict extreme of its 3 × 3 neighbourhood (ties with
/// the pixel above or below allowed) and lies more than one noise deviation beyond its neighbours takes the nearest
/// neighbour value. Decisions are taken on the input, the border stays.
fn clamp_spikes(top: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = top.to_vec();
    if w < 3 || h < 3 {
        return out;
    }
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y == 0 || y + 1 == h {
            return;
        }
        let (above, here, below) = (&top[(y - 1) * w..y * w], &top[y * w..(y + 1) * w], &top[(y + 1) * w..(y + 2) * w]);
        for x in 1..w - 1 {
            let v = here[x];
            let column = |i: usize, pick: fn(f32, f32) -> f32| pick(pick(above[i], here[i]), below[i]);
            if v >= above[x] && v >= below[x] && v > column(x - 1, f32::max) && v > column(x + 1, f32::max) {
                let nearest = above[x].max(below[x]).max(column(x - 1, f32::max)).max(column(x + 1, f32::max));
                if top_sigma(v) < v - nearest {
                    row[x] = nearest;
                }
            } else if v <= above[x] && v <= below[x] && v < column(x - 1, f32::min) && v < column(x + 1, f32::min) {
                let nearest = above[x].min(below[x]).min(column(x - 1, f32::min)).min(column(x + 1, f32::min));
                if top_sigma(v) < nearest - v {
                    row[x] = nearest;
                }
            }
        }
    });
    out
}

/// Weights of the value-range filter: for the brightness class of the centre a weight per class distance.
struct ClassWeights {
    /// largest class distance with a weight, per class
    pub reach: Vec<u16>,
    /// `span` weights per class
    pub weights: Vec<f32>,
    pub span: usize,
}

impl ClassWeights {
    /// `strength` divides the variance of the noise model (see [`RANGE_STRENGTH`]).
    fn new(strength: f64) -> Self {
        let variance = |class: usize| {
            let v = CLASS_WIDTH * class as f64;
            (TOP_MODEL[2] * v * v + TOP_MODEL[1] * v + TOP_MODEL[0]) / strength
        };
        let weight = |var: f64, distance: usize| {
            let d = CLASS_WIDTH * distance as f64;
            (-(d * d) / var).exp() as f32
        };
        let reach: Vec<u16> = (0..CLASSES)
            .map(|class| {
                let var = variance(class);
                let limit = ((var as i64) >> 4).max(0) as usize;
                (1..=limit.min(CLASSES - 1)).take_while(|&d| weight(var, d) >= RANGE_CUTOFF).count() as u16
            })
            .collect();
        let span = reach.iter().copied().max().unwrap_or(0) as usize + 1;
        let mut weights = vec![0f32; CLASSES * span];
        for class in 0..CLASSES {
            let var = variance(class);
            for d in 0..=reach[class] as usize {
                weights[class * span + d] = weight(var, d);
            }
        }
        Self { reach, weights, span }
    }
}

/// Brightness class of a top-layer value.
#[inline]
fn class_of(v: f32) -> usize {
    (v.clamp(0.0, 16383.0) as usize) >> 4
}

/// Value-range filter of the top layer: every pixel becomes the mean of the 11 × 11 values around it, weighted by
/// the distance of their brightness class to the centre's (no spatial weight). The window slides along each row
/// with a histogram of counts and sums per class. The border of [`RANGE_RADIUS`] pixels stays.
fn range_smooth(top: &[f32], w: usize, h: usize, cw: &ClassWeights) -> Vec<f32> {
    let r = RANGE_RADIUS;
    let mut out = top.to_vec();
    if w <= 2 * r || h <= 2 * r {
        return out;
    }
    let classes: Vec<u16> = top.par_iter().map(|&v| class_of(v) as u16).collect();
    out.par_chunks_mut(w).enumerate().for_each_init(
        || (vec![0u32; CLASSES], vec![0f64; CLASSES]),
        |(count, sum), (y, row)| {
            if y < r || y + r >= h {
                return;
            }
            count.fill(0);
            sum.fill(0.0);
            let column = |x: usize, count: &mut [u32], sum: &mut [f64], sign: f64| {
                for yy in y - r..=y + r {
                    let i = yy * w + x;
                    let k = classes[i] as usize;
                    count[k] = count[k].wrapping_add(sign as i32 as u32);
                    sum[k] += sign * f64::from(top[i]);
                }
            };
            for x in 0..2 * r {
                column(x, count, sum, 1.0);
            }
            for x in r..w - r {
                column(x + r, count, sum, 1.0);
                let k = classes[y * w + x] as usize;
                let reach = cw.reach[k] as usize;
                let table = &cw.weights[k * cw.span..(k + 1) * cw.span];
                let (mut total, mut weight) = (0f64, 0f64);
                for j in k.saturating_sub(reach)..=(k + reach).min(CLASSES - 1) {
                    if count[j] > 0 {
                        let wt = f64::from(table[j.abs_diff(k)]);
                        total += wt * sum[j];
                        weight += wt * f64::from(count[j]);
                    }
                }
                row[x] = (total / weight) as f32;
                column(x - r, count, sum, -1.0);
            }
        },
    );
    out
}

/// Blends the smoothed top layer `smooth` with the unsmoothed one by brightness (in place).
fn blend_smoothed(smooth: &mut [f32], plain: &[f32], p: &Upscale) {
    if p.constant_share() {
        let share = pooled_smooth_share(p.iso_factor);
        smooth.par_iter_mut().zip(plain.par_iter()).for_each(|(s, &t)| *s = share * *s + (1.0 - share) * t);
        return;
    }
    let table = curve::table(&SMOOTH_SHARE[p.luma()]);
    let scale = f64::from(16384.0 / p.white);
    smooth.par_iter_mut().zip(plain.par_iter()).for_each(|(s, &t)| {
        let share = table[curve::index(scale * f64::from(*s))].clamp(0.0, 1.0);
        *s = *s * share + t * (1.0 - share);
    });
}

/// Taps of the 2× cubic enlargement (a = −0.5) for output position `i` on an axis of `n` source values.
#[inline]
fn cubic_taps(i: usize, n: usize) -> ([usize; 4], [f32; 4]) {
    // source position (i + 0.5)/2 − 0.5: a quarter before or after cell i/2
    const BEFORE: [f32; 4] = [-0.0234375, 0.2265625, 0.8671875, -0.0703125];
    const AFTER: [f32; 4] = [-0.0703125, 0.8671875, 0.2265625, -0.0234375];
    let cell = (i / 2) as isize;
    let (first, weights) = if i % 2 == 0 { (cell - 2, BEFORE) } else { (cell - 1, AFTER) };
    (std::array::from_fn(|t| (first + t as isize).clamp(0, n as isize - 1) as usize), weights)
}

/// Spatial weights e^(−d) of the 5 × 5 grid cells around a full-resolution pixel, d in grid cells, for the four
/// positions of the pixel inside its cell (index `2·(y odd) + (x odd)`, then row, then column).
fn spatial_weights() -> [[f32; 25]; 4] {
    std::array::from_fn(|phase| {
        let (oy, ox) = (if phase >= 2 { 0.25f32 } else { -0.25 }, if phase % 2 == 1 { 0.25f32 } else { -0.25 });
        std::array::from_fn(|i| {
            let (dy, dx) = ((i / 5) as f32 - 2.0 - oy, (i % 5) as f32 - 2.0 - ox);
            (-(dy * dy + dx * dx).sqrt()).exp()
        })
    })
}

/// The colour gate: 1 for neutral and bluish cells, down to `1 − GATE_DEPTH` where bottom and middle layer dominate.
#[inline]
fn colour_gate(cell: &Px) -> f32 {
    let excess = ((cell[0] + cell[1]) - 2.0 * cell[2]) / cell[2].max(1e-4);
    1.0 - GATE_DEPTH / ((excess * -4.0 + 7.0).exp() + 1.0)
}

/// Step 3 on the processor. `denoised`: grid after the first stage, `plain`: grid before it, `colour`: result of
/// the colour stage on the grid (all `gw × gh`), `top`: top layer `2gw × 2gh`.
pub fn upscale(denoised: &[Px], plain: &[Px], colour: &[Px], gw: usize, gh: usize, top: &[f32], p: &Upscale) -> Upscaled {
    let (w, h) = (2 * gw, 2 * gh);
    assert!(denoised.len() == gw * gh && plain.len() == gw * gh && colour.len() == gw * gh && top.len() == w * h, "grid and top layer sizes");
    let mut clock = super::Clock::start();
    let sharp = clamp_spikes(top, w, h);
    clock.lap("upscale: spikes");
    if let Some(done) = on_card(denoised, plain, colour, gw, gh, &sharp, p) {
        clock.lap("upscale: graphics card");
        return done;
    }
    on_processor(denoised, plain, colour, gw, gh, sharp, p, &mut clock)
}

/// Step 3 on the processor, from the top layer with its spikes clamped (`sharp`) on. Working copies are freed as
/// soon as the next pass no longer needs them.
#[allow(clippy::too_many_arguments)]
fn on_processor(denoised: &[Px], plain: &[Px], colour: &[Px], gw: usize, gh: usize, sharp: Vec<f32>, p: &Upscale, clock: &mut super::Clock) -> Upscaled {
    let (w, h) = (2 * gw, 2 * gh);
    let mut smooth = range_smooth(&sharp, w, h, &ClassWeights::new(p.range_strength()));
    clock.lap("upscale: value-range filter");
    blend_smoothed(&mut smooth, &sharp, p);
    clock.lap("upscale: blend");

    // the unfiltered grid enlarged along the rows; the columns follow per output row
    let mut wide = vec![[0f32; 3]; gh * w];
    wide.par_chunks_mut(w).zip(plain.par_chunks(gw)).for_each(|(out, src)| {
        for (x, o) in out.iter_mut().enumerate() {
            let (at, wt) = cubic_taps(x, gw);
            *o = std::array::from_fn(|c| (0..4).map(|t| wt[t] * src[at[t]][c]).sum());
        }
    });
    clock.lap("upscale: enlarge rows");

    let spatial = spatial_weights();
    let mut rgb = vec![0f32; w * h * 3];
    let mut ratio = vec![1f32; w * h];
    rgb.par_chunks_mut(w * 3).zip(ratio.par_chunks_mut(w)).enumerate().for_each(|(y, (rgb_row, ratio_row))| {
        let (rows, row_wt) = cubic_taps(y, gh);
        let cy = (y / 2) as isize;
        let cells_y = (cy - 2).max(0) as usize..=((cy + 2) as usize).min(gh - 1);
        for x in 0..w {
            let i = y * w + x;
            // guide: enlarged grid times the ratio of the top layer to its enlarged grid value
            let enlarged: [f32; 3] = std::array::from_fn(|c| (0..4).map(|t| row_wt[t] * wide[rows[t] * w + x][c]).sum::<f32>().clamp(0.0, 32767.0).trunc());
            let lift = (sharp[i].max(0.0) + RATIO_OFFSET) / (enlarged[2] + RATIO_OFFSET);
            let guide: [f32; 3] = enlarged.map(|v| f32::from((v * lift + 0.5) as i32 as i16));
            let peak = guide[0].max(guide[1]).max(guide[2]);
            let spread = if peak > 1000.0 { peak * 0.025 } else { 25.0 };
            let cx = (x / 2) as isize;
            let near = &spatial[2 * (y % 2) + x % 2];
            let (mut weight, mut top_sum, mut acc) = (0f32, 0f32, [0f32; 3]);
            for gy in cells_y.clone() {
                let ky = (gy as isize - cy + 2) as usize;
                for gx in (cx - 2).max(0) as usize..=((cx + 2) as usize).min(gw - 1) {
                    let cell = &denoised[gy * gw + gx];
                    let apart = (cell[0] - guide[0]).abs() + (cell[1] - guide[1]).abs() + (cell[2] - guide[2]).abs();
                    let wt = near[ky * 5 + (gx as isize - cx + 2) as usize] * (1.0 / (apart / spread + 1.0));
                    let col = &colour[gy * gw + gx];
                    top_sum += cell[2] * wt;
                    acc[0] += wt * col[0];
                    acc[1] += wt * col[1];
                    acc[2] += wt * col[2];
                    weight += wt;
                }
            }
            let grid_top = top_sum / weight;
            let mut r = 1.0f32;
            if grid_top > 0.0 {
                let t = smooth[i];
                r = (t.max(0.0) + RATIO_OFFSET) / (grid_top + RATIO_OFFSET);
                if t >= HIGHLIGHT_START && r < 1.0 {
                    let fade = ((t - HIGHLIGHT_START) / HIGHLIGHT_SPAN).clamp(0.0, 1.0);
                    r = (r - 1.0) * (1.0 - fade) + 1.0;
                }
            }
            r = r.powf(colour_gate(&denoised[(y / 2) * gw + x / 2]));
            ratio_row[x] = r.clamp(0.001, 1000.0);
            for c in 0..3 {
                rgb_row[x * 3 + c] = ((acc[c] / weight).clamp(0.0, 65535.0) + 0.5).min(65535.0).trunc();
            }
        }
    });
    // free the working copies before the detail pass allocates
    drop((wide, smooth, sharp));
    clock.lap("upscale: joint filter");
    let detail = detail_and_ratio(&mut rgb, &ratio, w, h, p);
    clock.lap("upscale: detail signal, ratio applied");
    Upscaled { rgb, detail }
}

/// Limits of the detail signal: dead band, absolute limit, luma factor, limit relative to the luma.
fn detail_limits(p: &Upscale) -> [f32; 4] {
    let (dead_band, _) = p.detail_tuning();
    if p.pooled {
        [(dead_band * p.iso_factor).min(320.0), 6000.0, 3.0, 0.35]
    } else {
        [dead_band.min(40.0) * p.iso_factor, 2000.0 / p.iso_factor, 1.0, 0.2]
    }
}

/// Detail value of one pixel from its luma and the sum of the eight lumas around it (`limits` of [`detail_limits`]),
/// quantised to quarter steps.
#[inline]
fn detail_value(centre: f32, around: f32, limits: &[f32; 4]) -> f32 {
    let [dead_band, limit, factor, relative] = *limits;
    let luma = centre * factor;
    let difference = luma - factor * around * 0.125;
    let cored = difference.signum() * (difference.abs() - dead_band).max(0.0);
    let bound = relative * luma;
    let v = cored.clamp(-limit, limit).clamp(-bound, bound).clamp(-32760.0, 32760.0);
    (f64::from(v) * 4.0 + 0.5) as i32 as f32
}

/// Detail signal (luma of colour × ratio against the mean of its eight neighbours, see [`detail_value`]) and the
/// ratio applied to the colour (`rgb` in place). Returns the detail signal; its outermost pixels are 0.
fn detail_and_ratio(rgb: &mut [f32], ratio: &[f32], w: usize, h: usize, p: &Upscale) -> Vec<f32> {
    let luma: Vec<f32> = rgb.par_chunks(3).zip(ratio.par_iter()).map(|(c, &r)| 0.299 * (c[0] * r) + 0.587 * (c[1] * r) + 0.114 * (c[2] * r)).collect();
    let limits = detail_limits(p);
    let mut detail = vec![0f32; w * h];
    detail.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y == 0 || y + 1 >= h {
            return;
        }
        for x in 1..w - 1 {
            let i = y * w + x;
            let mut around = -luma[i];
            for j in [i - w, i, i + w] {
                around += luma[j - 1] + luma[j] + luma[j + 1];
            }
            row[x] = detail_value(luma[i], around, &limits);
        }
    });
    drop(luma);
    let (_, exponent) = p.detail_tuning();
    rgb.par_chunks_mut(3).zip(ratio.par_iter()).for_each(|(c, &r)| {
        let f = if exponent == 1.0 { r } else { r.powf(exponent) };
        for v in c {
            *v = (*v * f).clamp(0.0, 65535.0).trunc();
        }
    });
    detail
}

/// Step 3 on the graphics card (`upscale.wgsl`), from the top layer with its spikes clamped on; the detail signal
/// follows on the processor. `None`: the processor computes everything.
fn on_card(denoised: &[Px], plain: &[Px], colour: &[Px], gw: usize, gh: usize, sharp: &[f32], p: &Upscale) -> Option<Upscaled> {
    if !super::gpu::allowed() || gw < 8 || gh < 8 {
        return None;
    }
    let weights = ClassWeights::new(p.range_strength());
    let mut numbers = vec![0f32; 16];
    numbers[..4].copy_from_slice(&[gw as f32, gh as f32, weights.span as f32, if p.constant_share() { 1.0 } else { 0.0 }]);
    numbers[4..6].copy_from_slice(&[16384.0 / p.white, pooled_smooth_share(p.iso_factor)]);
    numbers.extend(weights.reach.iter().map(|&r| f32::from(r)));
    numbers.extend_from_slice(&weights.weights);
    numbers.extend(curve::table(&SMOOTH_SHARE[p.luma()]));
    numbers.extend(spatial_weights().iter().flatten());
    let (mut rgb, ratio) = super::gpu::upscale(denoised, plain, colour, gw, gh, sharp, &super::gpu::UpscaleNumbers(numbers))?;
    let detail = detail_and_ratio(&mut rgb, &ratio, 2 * gw, 2 * gh, p);
    Some(Upscaled { rgb, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quattro_hi::tests::noise;

    fn scene(gw: usize, gh: usize) -> (Vec<Px>, Vec<Px>, Vec<f32>) {
        let level = |x: usize, y: usize| 300.0 + 20.0 * x as f32 + if y > gh / 2 { 5000.0 } else { 0.0 };
        let top: Vec<f32> = (0..4 * gw * gh).map(|i| level(i % (2 * gw) / 2, i / (2 * gw) / 2) + 30.0 * noise(5 * i + 2)).collect();
        let grid: Vec<Px> = (0..gw * gh).map(|i| [level(i % gw, i / gw) * 1.3, level(i % gw, i / gw) * 1.1, level(i % gw, i / gw)]).collect();
        let colour: Vec<Px> = grid.iter().map(|p| [p[0] * 2.0, p[1] * 1.8, p[2] * 2.2]).collect();
        (grid, colour, top)
    }

    #[test]
    fn spikes_go_ordinary_values_stay() {
        let (w, h) = (12, 9);
        let mut top: Vec<f32> = (0..w * h).map(|i| 1000.0 + (i % 7) as f32).collect();
        assert_eq!(clamp_spikes(&top, w, h), top);
        top[4 * w + 5] = 2000.0;
        top[2 * w + 3] = 100.0;
        let out = clamp_spikes(&top, w, h);
        assert!((1000.0..=1006.0).contains(&out[4 * w + 5]) && (1000.0..=1006.0).contains(&out[2 * w + 3]));
    }

    #[test]
    fn flat_top_layer_reproduces_the_colour() {
        let (gw, gh) = (24, 20);
        let grid = vec![[900f32, 800.0, 700.0]; gw * gh];
        let colour = vec![[2000f32, 1800.0, 1500.0]; gw * gh];
        let top = vec![700f32; 4 * gw * gh];
        let p = Upscale { iso_factor: 16.0, luma_step: 2, detail_step: 3, white: 13000.0, pooled: false };
        let out = on_processor(&grid, &grid, &colour, gw, gh, top, &p, &mut crate::quattro_hi::Clock::start());
        // the ratio applied cuts off the fraction: one unit below at most
        assert!(out.rgb.chunks(3).all(|c| (0..3).all(|k| (c[k] - [2000.0, 1800.0, 1500.0][k]).abs() <= 1.0)));
        assert!(out.detail.iter().all(|&d| d == 0.0));
    }

    #[test]
    fn detail_tuning_follows_the_steps() {
        let p = Upscale { iso_factor: 64.0, luma_step: 2, detail_step: 3, white: 13000.0, pooled: false };
        // ISO 6400: base dead band 1, times 1.2 at the middle step, times 1.5 from ISO 800 on; no extra gain
        let (dead_band, exponent) = p.detail_tuning();
        assert!((dead_band - 1.8).abs() < 1e-5 && exponent == 1.0);
        let pooled = Upscale { pooled: true, iso_factor: 16.0, ..p };
        let (dead_band, exponent) = pooled.detail_tuning();
        assert!(dead_band == 10.0 && (exponent - 1.1).abs() < 1e-5);
        assert_eq!(pooled_smooth_share(16.0), 0.65);
        assert_eq!(pooled_smooth_share(7.0), 0.55);
    }

    /// Graphics card against processor: equal up to rounding (single values may differ by one where a sum lands
    /// on a whole number). Skipped without a card.
    #[test]
    fn graphics_card_matches_processor() {
        if !crate::quattro_hi::gpu::available() {
            eprintln!("no graphics card: comparison skipped");
            return;
        }
        let (gw, gh) = (72, 52);
        let (grid, colour, top) = scene(gw, gh);
        for (pooled, luma_step) in [(false, 2), (false, 4), (true, 2)] {
            let p = Upscale { iso_factor: 16.0, luma_step, detail_step: 3, white: 13000.0, pooled };
            let sharp = clamp_spikes(&top, 2 * gw, 2 * gh);
            let card = on_card(&grid, &grid, &colour, gw, gh, &sharp, &p).expect("upscale on the card");
            let cpu = on_processor(&grid, &grid, &colour, gw, gh, sharp, &p, &mut crate::quattro_hi::Clock::start());
            let mean = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32;
            let worst = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs() / (1.0 + y.abs())).fold(0f32, f32::max);
            assert!(mean(&card.rgb, &cpu.rgb) < 0.05 && worst(&card.rgb, &cpu.rgb) < 0.002, "colour, pooled {pooled}: mean {} worst {}", mean(&card.rgb, &cpu.rgb), worst(&card.rgb, &cpu.rgb));
            assert!(mean(&card.detail, &cpu.detail) < 0.05, "detail, pooled {pooled}: mean {}", mean(&card.detail, &cpu.detail));
        }
    }
}
