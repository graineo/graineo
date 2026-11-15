//! Classic chain, step 2: sensor correction, part 1 and shared pieces. Works on the three layer codes
//! of the whole recorded area (including the dark-shielded border):
//!
//! 1. *Dark drift*: black level from the camera's stored drift or the shielded strips
//!    ([`super::dcraw::drift`], [`drift_from_camera`]); for a long exposure it is replaced by the
//!    black level estimated from the image.
//! 2. *Input factor*: when the camera lists a reduction factor for its first lookup table, the codes
//!    (minus the mean dark level) are scaled up by it in fixed point ([`input_factor`]); the mean of
//!    the scaled-off image is a by-product used by the incandescent white balance.
//!
//! Part 2 (row black level, flare, column filter, post polynomial, spatial gain) runs after dcraw in
//! [`super::dcraw::sensor`] and uses the gain tables ([`Grid`]) and the subsampled mean
//! ([`subsampling`]) from here.

use super::calibration::{Data, Calibration};

const Q14: f64 = 16384.0;

/// Rectangle with exclusive right and bottom edge (the camera lists inclusive values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub links: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A rectangle entry of the camera data (left, top, right, bottom inclusive → exclusive edges);
/// all zero when missing.
pub fn rect(d: &Data, name: &str) -> Rect {
    match d.values(name) {
        Some(v) if v.len() >= 4 => Rect { links: v[0] as i32, top: v[1] as i32, right: v[2] as i32 + 1, bottom: v[3] as i32 + 1 },
        _ => Rect::default(),
    }
}

/// Black level and slope along a line, per layer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Line {
    pub slope: [f64; 3],
    pub mean: [f64; 3],
}

/// Drift at the top and the bottom of the area.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Drift {
    pub top: Line,
    pub bottom: Line,
}

#[inline]
fn px(img: &[i32], i: usize) -> i32 {
    img[i] as i16 as i32
}

/// The camera's stored drift (12 numbers: slope and level per layer for the top, then the bottom).
pub fn drift_from_camera(d: &Data) -> Drift {
    let Some(p) = d.values("DarkDrift").filter(|p| p.len() >= 12) else { return Drift::default() };
    let f = |i: usize| f64::from(p[i] as f32);
    Drift {
        top: Line { slope: [f(0), f(2), f(4)], mean: [f(1), f(3), f(5)] },
        bottom: Line { slope: [f(6), f(8), f(10)], mean: [f(7), f(9), f(11)] },
    }
}

/// Result of the input-factor pass.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    /// mean of (code − dark level) over the whole area, per layer (before scaling)
    pub mean: [f64; 3],
}

/// Input factor: with a reduction factor ≠ 1 listed by the camera (`LinLUT_ReductionFactor`, times 1.3
/// with cached calibration) every code becomes `((code − dark level) · factor) >> 14` (fixed point,
/// 32-bit); the dark level is the mean of the two drift levels, cut to an integer. The pass also
/// returns the mean of the unscaled differences.
pub fn input_factor(img: &mut [i32], w: usize, h: usize, dr: &Drift, factor: f64, cached: bool) -> Input {
    let mut f = f64::from(factor as f32);
    if f == 0.0 {
        f = 1.0;
    }
    let change = f != 1.0;
    let dark: [i32; 3] = std::array::from_fn(|c| ((dr.top.mean[c] + dr.bottom.mean[c]) / 2.0) as i32);
    let k = if cached { 1.3 } else { 1.0 };
    let scale = ((k * f) * Q14) as i32;
    let mut sum = [0.0f64; 3];
    let width = f64::from(w as u32 as f32);
    for y in 0..h {
        let mut row = [0i32; 3];
        for x in 0..w {
            let i = (y * w + x) * 3;
            for c in 0..3 {
                let d = px(img, i + c).wrapping_sub(dark[c]);
                row[c] = row[c].wrapping_add(d);
                if change {
                    img[i + c] = (d.wrapping_mul(scale) >> 14) as i16 as i32;
                }
            }
        }
        for c in 0..3 {
            sum[c] += f64::from(row[c]) / width;
        }
    }
    let n = h as f64;
    Input { mean: [sum[0] / n, sum[1] / n, sum[2] / n] }
}

/// Mean of every fourth pixel (starting at the third row and column).
pub fn subsampling(img: &[i32], w: usize, h: usize, step: usize) -> [f64; 3] {
    let start = step.max(1) >> 1;
    let step = step.max(1);
    let (mut s, mut n) = ([0.0f64; 3], 0u32);
    if start < h && start < w {
        let mut y = start;
        while y < h {
            let mut x = start;
            loop {
                let i = (y * w + x) * 3;
                for c in 0..3 {
                    s[c] += f64::from(px(img, i + c));
                }
                n += 1;
                x += step;
                if x >= w {
                    break;
                }
            }
            y += step;
        }
    }
    if n == 0 {
        return [0.0; 3];
    }
    let n = f64::from(n);
    [s[0] / n, s[1] / n, s[2] / n]
}

/// A two-dimensional gain table (rows × columns × three layers, `f32`).
#[derive(Clone, Debug)]
pub struct Grid {
    pub columns: usize,
    pub rows: usize,
    pub data: Vec<f32>,
}

impl Grid {
    pub fn load(d: &Data, name: &str) -> Option<Grid> {
        let m = d.camf.matrix(name).ok()?;
        if m.dims.len() < 3 || m.dims[2].1 != 3 {
            return None;
        }
        let v: Vec<f32> = m.as_f64().iter().map(|x| *x as f32).collect();
        let (rows, columns) = (m.dims[0].1, m.dims[1].1);
        if v.len() < rows * columns * 3 {
            return None;
        }
        Some(Grid { columns, rows, data: v })
    }
}

/// Table row for image line `y`: linear blend of the two table rows around `(rows − 1) · y / (h − 1)`.
pub fn grid_row(g: &Grid, y: usize, h: usize, outp: &mut [f32]) {
    let pos = (g.rows as i32 - 1) as f64 * (y as f64 / f64::from(h as u32 - 1));
    let bottom = pos.floor();
    let top = pos.ceil();
    let t = (pos - f64::from(bottom as i32 as u32)) as f32;
    let a = bottom as i32 as usize * g.columns;
    let b = top as i32 as usize * g.columns;
    for i in 0..g.columns * 3 {
        let (lo, hi) = (g.data[a * 3 + i], g.data[b * 3 + i]);
        outp[i] = (hi - lo) * t + lo;
    }
}

/// Part 1 of the sensor correction (dark level, input factor); changes the codes when the camera
/// lists a reduction factor. Part 2 is [`super::dcraw::sensor`].
pub struct Prepared {
    pub drift: Drift,
    /// the drift before flag 64 of `SkipStepFlags` (input of the automatic white balance)
    pub drift_raw: Drift,
}

pub fn prepare(img: &mut [i32], w: usize, h: usize, d: &Data, cal: &Calibration, skip: u32) -> Result<(Prepared, Input), String> {
    let mut dr = super::dcraw::drift(img, w, h, d);
    if cal.iso.bulb {
        // long exposure: both dark levels become the black level estimated from the image around the
        // mean of the two strips (only three quarters of the estimate's deviation from its grey)
        let off: [i32; 3] = std::array::from_fn(|c| ((dr.top.mean[c] + dr.bottom.mean[c]) / 2.0) as i32);
        let mut b = super::color::black_level_with(img, w, h, [4.0; 3], off);
        let gray = (b[0] + 2 * b[1] + b[2]) >> 3;
        for v in b.iter_mut() {
            *v -= gray;
        }
        let new: [f64; 3] = std::array::from_fn(|c| f64::from(off[c] + (b[c] * 3) / 4));
        dr.top.mean = new;
        dr.bottom.mean = new;
    }
    let inp = input_factor(img, w, h, &dr, f64::from(d.number("LinLUT_ReductionFactor")), cal.color.cached);
    let drift_raw = dr;
    // flag 64: no drift in the row correction (the input factor above still used it)
    if skip & 64 != 0 {
        dr = Drift::default();
    }
    Ok((Prepared { drift: dr, drift_raw }, inp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_row_interpolates_between_table_rows() {
        let g = Grid { columns: 2, rows: 2, data: vec![1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 5.0, 5.0, 5.0] };
        let mut outp = vec![0.0f32; 6];
        grid_row(&g, 0, 11, &mut outp);
        assert_eq!(&outp[..3], &[1.0; 3]);
        grid_row(&g, 10, 11, &mut outp);
        assert_eq!(&outp[..3], &[3.0; 3]);
        grid_row(&g, 5, 11, &mut outp);
        assert!((outp[0] - 2.0).abs() < 1e-6 && (outp[3] - 3.5).abs() < 1e-6, "{outp:?}");
    }
}
