//! High-ISO chain, step 2b: the colour stage on the denoised colour grid.
//!
//! The colour matrix of a three-layer sensor multiplies the noise of the layers into chroma noise. The stage
//! therefore mixes two renderings of every grid cell: the matrix colour (with small values cored towards grey by a
//! soft threshold, Donoho 1995) and the white-balanced camera colour itself. The share of the camera colour is
//! high in the dark and falls with the brightness; it also depends on hue and saturation. Before that, cells near
//! the top of the range move towards their channel mean so that clipped layers give no colour cast; afterwards
//! the chroma (U and V of BT.601) of every cell is limited to the second smallest and second largest value of its
//! 7 × 7 neighbours (a rank-order clamp, Hardie and Boncelet 1993).
//!
//! Input: the grid in the units of the chain; output: values of the colour matrix times the ISO factor.

use rayon::prelude::*;

use super::Px;

/// One image's colour stage.
#[derive(Clone, Copy, Debug)]
pub struct Tint {
    /// white-balanced camera values → output colour (colour matrix of white balance and colour mode, row-major)
    pub matrix: [f64; 9],
    /// white balance gains on the grid values, middle layer = 1
    pub gains: [f32; 3],
    /// SensorISO / 100
    pub sensor: f32,
    /// capture ISO / 100
    pub iso_factor: f32,
    /// highlight setting: shifts the range in which bright cells turn neutral upwards (0 = camera setting)
    pub highlights: f32,
    /// compact camera (dp Quattro) rather than a system camera
    pub compact: bool,
    /// the grid holds block means (binned output)
    pub pooled: bool,
}

/// Rounds to the nearest whole number, halves upwards.
#[inline]
fn nearest(v: f64) -> i32 {
    (v + 1.5e-8).round_ties_even() as i32
}

/// Entries of the brightness curve and of the coring curve.
const BRIGHTNESS_STEPS: usize = 900;
const CORING_STEPS: usize = 1024;
/// Entries of the exponent curve and the value that maps to its end.
const EXPONENT_STEPS: usize = 4096;
const EXPONENT_RANGE: f32 = 14335.0;
/// Fixed-point factor of the coring.
const CORING_SCALE: f32 = 16.0;

/// Share of the matrix colour by hue: red, yellow, green, cyan, blue, magenta, red.
const HUE_SHARE: [f32; 7] = [0.0, 0.7, 1.0, 0.5, 0.0, 1.0, 0.0];

/// The curves of the blend.
pub(super) struct Curves {
    /// share of the camera colour by brightness step, 1 (dark) … 0
    brightness: Vec<f32>,
    /// exponent of the share by mean brightness
    exponent: Vec<f32>,
    /// coring: amount taken off a value of that many sixteenths
    coring: Vec<f32>,
    /// brightness at which the share has run out, and the factor on the brightness
    reach: f32,
    quick: bool,
}

impl Curves {
    pub fn new(t: &Tint) -> Self {
        let power = if t.compact { 4.0 } else { 8.0 };
        // the last entry stays 0: no camera colour from the top of the range on
        let mut brightness = vec![0f32; BRIGHTNESS_STEPS + 1];
        for (step, share) in brightness[..BRIGHTNESS_STEPS].iter_mut().enumerate() {
            let remaining = ((4.0f32 - step as f32 * 0.003_333_333_4) - 1.0) / 3.0;
            *share = ((f64::from(remaining).powf(power) * 3.0 + 1.0) as f32 - 1.0) / 3.0;
        }
        const NODES: [usize; 10] = [0, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
        const VALUES: [f64; 10] = [15.0, 15.0, 8.0, 5.0, 3.0, 2.5, 3.25, 4.0, 4.0, 4.0];
        let mut exponent = vec![0f32; EXPONENT_STEPS];
        for k in 0..9 {
            let (from, to) = (NODES[k], NODES[k + 1]);
            let (base, rise) = (VALUES[k] as f32, (VALUES[k + 1] - VALUES[k]) as f32);
            for (j, e) in exponent[from..to].iter_mut().enumerate() {
                *e = j as f32 / (to - from) as f32 * rise + base;
            }
        }
        exponent[EXPONENT_STEPS - 1] = VALUES[9] as f32;
        let coring = (0..CORING_STEPS)
            .map(|i| {
                let i = i as f64;
                let fade = if i / 320.0 < std::f64::consts::PI { ((i / 320.0).cos() + 1.0) * 0.5 } else { 0.0 };
                ((0.8 * i / 64.0).tanh() * (4.0 * fade)) as f32
            })
            .collect();
        Self { brightness, exponent, coring, reach: if t.compact { 200.0 } else { 150.0 }, quick: t.compact || t.pooled }
    }

    /// Coring amount for `n` sixteenths (sign kept).
    #[inline]
    fn core(&self, n: i32) -> f32 {
        let n = n.clamp(-(CORING_STEPS as i32 - 1), CORING_STEPS as i32 - 1);
        let v = self.coring[n.unsigned_abs() as usize];
        if n < 0 { -v } else { v }
    }

    /// Share of the camera colour for the white-balanced cell `p`.
    #[inline]
    fn camera_share(&self, p: &Px) -> (f32, f32) {
        let k = 4095.0 / EXPONENT_RANGE;
        let at = nearest(f64::from(((p[1] * k + p[0] * k) + p[2] * k) * 0.33));
        let exponent = self.exponent[at.clamp(0, EXPONENT_STEPS as i32 - 1) as usize];
        let peak = p[0].max(p[1].max(p[2]));
        let level = if self.quick { peak * 4.0 } else { peak };
        let step = nearest(f64::from(level - self.reach).clamp(1.0, BRIGHTNESS_STEPS as f64)) as usize;
        let a = p.map(f32::abs);
        let (high, low) = (a[0].max(a[1].max(a[2])), a[0].min(a[1].min(a[2])));
        let span = high - low;
        let (hue, saturation) = if span == 0.0 {
            (0.0, 0.0)
        } else {
            let sector = if a[0] == high {
                let h = (a[1] - a[2]) / span;
                if h < 0.0 { h + 6.0 } else { h }
            } else if a[1] == high {
                (a[2] - a[0]) / span + 2.0
            } else {
                (a[0] - a[1]) / span + 4.0
            };
            (sector * (1.0 / 6.0), span / high)
        };
        let (mut total, mut weighted) = (0f32, 0f32);
        for (k, share) in HUE_SHARE.iter().enumerate() {
            let w = 1.0 - (6.0 * (k as f32 / 6.0 - hue).abs()).min(1.0);
            total += w;
            weighted += w * share;
        }
        if total <= 0.0 {
            return (0.0, 0.0);
        }
        let q = level.min(self.reach) / self.reach;
        let q2 = q * q;
        let raw = (weighted / total).powf(q2).powf(saturation) * (self.brightness[step] * (1.0 - saturation * q2));
        let share = if raw < 0.0 { 0.0 } else { raw.min(1.0) }.powf(exponent);
        (share, share.powf(1.0 / exponent))
    }

    /// One white-balanced cell → output colour (before the ISO factor).
    #[inline]
    pub fn cell(&self, p: &Px, m: &[f64; 9]) -> Px {
        let (share, cored_share) = self.camera_share(p);
        let u = p.map(|v| f64::from(v.abs()));
        let matrixed: [f64; 3] = std::array::from_fn(|k| (u[0] * m[k * 3] + u[1] * m[k * 3 + 1]) + u[2] * m[k * 3 + 2]);
        let scale = f64::from(CORING_SCALE);
        let rest: [f32; 3] = std::array::from_fn(|k| (matrixed[k] * scale) as f32 - self.core(nearest(matrixed[k] * scale)) * CORING_SCALE);
        let grey = nearest(f64::from(((rest[1] + rest[1]) + rest[0]) + rest[2])) >> 2;
        std::array::from_fn(|k| {
            let cored = rest[k] / CORING_SCALE - self.core(nearest(f64::from(rest[k])) - grey);
            let colour = cored_share * cored + (1.0 - cored_share) * matrixed[k] as f32;
            (1.0 - share) * colour + share * u[k] as f32
        })
    }
}

/// Bright cells towards their channel mean (white-balanced result).
#[inline]
pub(super) fn neutralise(p: &Px, t: &Tint) -> Px {
    let (from, to) = (8000.0 + 2400.0 * t.highlights, 11000.0 + 3000.0 * t.highlights);
    let v: [f32; 3] = std::array::from_fn(|c| p[c] * t.gains[c] * t.sensor);
    let mean = v[2] * (1.0 / 3.0) + v[1] * (1.0 / 3.0) + v[0] * (1.0 / 3.0);
    let share = ((mean - from) / (to - from)).clamp(0.0, 1.0);
    v.map(|c| (mean * share + c * (1.0 - share)) / t.sensor)
}

/// BT.601 luma and colour differences.
#[inline]
fn to_yuv(p: &Px) -> [f32; 3] {
    let (r, g, b) = (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
    [((0.299 * r + 0.587 * g) + 0.114 * b) as f32, ((-0.28886 * g - 0.14713 * r) + 0.436 * b) as f32, (-0.10001 * b + (-0.51499 * g + 0.615 * r)) as f32]
}

#[inline]
fn from_yuv(y: f32, u: f32, v: f32) -> Px {
    let (y, u, v) = (f64::from(y), f64::from(u), f64::from(v));
    [(v * 1.13983 + y) as f32, ((v * -0.5806 + u * -0.39465) + y) as f32, (u * 2.03211 + y) as f32]
}

/// Radius of the chroma clamp.
const CLAMP_RADIUS: isize = 3;

/// The two largest and the two smallest values seen so far, for both colour differences.
#[derive(Clone, Copy)]
struct Extremes {
    high: [[f32; 2]; 2],
    low: [[f32; 2]; 2],
}

impl Extremes {
    const NONE: Self = Self { high: [[f32::NEG_INFINITY; 2]; 2], low: [[f32::INFINITY; 2]; 2] };

    #[inline]
    fn add(&mut self, c: usize, v: f32) {
        if v > self.high[c][1] {
            self.high[c] = if v > self.high[c][0] { [v, self.high[c][0]] } else { [self.high[c][0], v] };
        }
        if v < self.low[c][1] {
            self.low[c] = if v < self.low[c][0] { [v, self.low[c][0]] } else { [self.low[c][0], v] };
        }
    }
}

/// Chroma clamp: U and V of every cell limited to the second smallest and second largest of its 48 neighbours
/// (edge cells repeated); the luma stays. `yuv` in, RGB out; a cell whose result is not finite takes `fallback`.
/// The extremes of each cell's column of seven are formed once and merged along the rows.
fn clamp_chroma(yuv: &[[f32; 3]], fallback: &[Px], w: usize, h: usize) -> Vec<Px> {
    let row_at = |y: usize, dy: isize| (y as isize + dy).clamp(0, h as isize - 1) as usize * w;
    let mut columns = vec![Extremes::NONE; w * h];
    columns.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for dy in -CLAMP_RADIUS..=CLAMP_RADIUS {
            for (e, n) in row.iter_mut().zip(&yuv[row_at(y, dy)..row_at(y, dy) + w]) {
                e.add(0, n[1]);
                e.add(1, n[2]);
            }
        }
    });
    let mut out = vec![[0f32; 3]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut window = Extremes::NONE;
            for dx in (-CLAMP_RADIUS..=CLAMP_RADIUS).filter(|&dx| dx != 0) {
                let column = &columns[y * w + (x as isize + dx).clamp(0, w as isize - 1) as usize];
                for c in 0..2 {
                    window.add(c, column.high[c][0]);
                    window.add(c, column.high[c][1]);
                    window.add(c, column.low[c][0]);
                    window.add(c, column.low[c][1]);
                }
            }
            // the cell's own column without the cell itself
            for dy in (-CLAMP_RADIUS..=CLAMP_RADIUS).filter(|&dy| dy != 0) {
                let n = &yuv[row_at(y, dy) + x];
                window.add(0, n[1]);
                window.add(1, n[2]);
            }
            let centre = &yuv[y * w + x];
            let limit = |c: usize| if centre[c + 1] <= window.high[c][1] { centre[c + 1].max(window.low[c][1]) } else { window.high[c][1] };
            let rgb = from_yuv(centre[0], limit(0), limit(1));
            *o = if rgb.iter().all(|v| v.is_finite()) { rgb } else { fallback[y * w + x] };
        }
    });
    out
}

/// The colour stage on the processor: `grid` (`w × h`, units of the chain) → output colour per cell.
pub fn tint(grid: &[Px], w: usize, h: usize, t: &Tint) -> Vec<Px> {
    let mut clock = super::Clock::start();
    let balanced: Vec<Px> = grid.par_iter().map(|p| neutralise(p, t)).collect();
    let yuv = blend_to_yuv(&balanced, t);
    clock.lap("colour: blend");
    let out = clamp_chroma(&yuv, &balanced, w, h);
    clock.lap("colour: chroma clamp");
    out
}

/// Blend of matrix colour and camera colour of the white-balanced cells, as luma and colour differences.
pub(super) fn blend_to_yuv(balanced: &[Px], t: &Tint) -> Vec<[f32; 3]> {
    let curves = Curves::new(t);
    balanced.par_iter().map(|p| to_yuv(&curves.cell(p, &t.matrix).map(|v| v * t.iso_factor))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tint {
        Tint { matrix: [1.6, -0.4, -0.2, -0.3, 1.5, -0.2, -0.1, -0.4, 1.5], gains: [1.0; 3], sensor: 1.0, iso_factor: 1.0, highlights: 0.0, compact: true, pooled: false }
    }

    #[test]
    fn grey_stays_grey_and_bright_cells_take_the_matrix_colour() {
        let t = sample();
        let c = Curves::new(&t);
        let grey = c.cell(&[10.0, 10.0, 10.0], &t.matrix);
        assert!((grey[0] - grey[1]).abs() < 1e-3 && (grey[1] - grey[2]).abs() < 1e-3 && (grey[0] - 10.0).abs() < 0.5, "{grey:?}");
        let bright = c.cell(&[3000.0, 2000.0, 1500.0], &t.matrix);
        assert!((bright[0] - (1.6 * 3000.0 - 0.4 * 2000.0 - 0.2 * 1500.0)).abs() < 1.0, "{bright:?}");
        // in the dark the colour lies between camera and matrix colour, with less saturation than the matrix gives
        let dark = c.cell(&[30.0, 20.0, 15.0], &t.matrix);
        assert!(dark[0] - dark[2] < (1.6 * 30.0 - 0.4 * 20.0 - 0.2 * 15.0) - (-0.1 * 30.0 - 0.4 * 20.0 + 1.5 * 15.0), "{dark:?}");
    }

    #[test]
    fn highlights_turn_neutral() {
        let t = sample();
        assert_eq!(neutralise(&[100.0, 200.0, 300.0], &t), [100.0, 200.0, 300.0]);
        let p = neutralise(&[12000.0, 11000.0, 13000.0], &t);
        assert!((p[0] - 12000.0).abs() < 1e-2 && (p[1] - 12000.0).abs() < 1e-2 && (p[2] - 12000.0).abs() < 1e-2, "{p:?}");
    }

    #[test]
    fn single_chroma_outlier_is_clamped() {
        let (w, h) = (12, 10);
        let mut cells = vec![[500f32, 500.0, 500.0]; w * h];
        cells[4 * w + 5] = [900.0, 500.0, 300.0];
        let yuv: Vec<[f32; 3]> = cells.iter().map(to_yuv).collect();
        let out = clamp_chroma(&yuv, &cells, w, h);
        let p = out[4 * w + 5];
        assert!((p[0] - p[1]).abs() < 0.5 && (p[1] - p[2]).abs() < 0.5, "{p:?}");
        assert!((out[0][0] - 500.0).abs() < 0.1);
    }
}
