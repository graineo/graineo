//! Quattro: the colour grid (bottom and middle layers at their own resolution, the top layer averaged 2 × 2 onto the
//! same grid) and its first noise reduction stage ([`crate::quattro_noise`]). This module keeps the grid
//! conversions around the stage.
//!
//! Units: sensor counts above black divided by SensorISO/100 (sd Quattro H: times the digital ISO gain of the
//! layer), before the white balance.

use rayon::prelude::*;

pub type Px = [f32; 3];

/// Middle strength step of the colour noise slider.
pub const DEFAULT_STEP: usize = 2;

/// Parameters of one image.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Daylight white balance gains (with temperature factor) normalised to the middle layer
    pub gains: [f32; 3],
    /// Capture ISO
    pub capture_iso: f32,
    /// The image was binned (factor ≥ 2)
    pub binned: bool,
    /// Binning factor n (1 = none): an n × n block of the full resolution covers (n/2)² grid cells, so the noise
    /// variance of a binned value is the grid model's divided by (n/2)² (n = 2: one grid cell, unchanged)
    pub bin: usize,
    /// Factor on the noise variance of the model (1 = as measured; the high-ISO path smooths the colour of its
    /// unprocessed stored values more strongly, see `crate::quattro`)
    pub var_scale: f32,
    /// Strength step 0 … 4 of the colour noise slider
    pub step: usize,
    /// White of the raw range in the units of the stage
    pub white: f32,
}

/// Noise reduction of one grid `img` (`w × h`, units see module) in place ([`crate::quattro_noise`]).
pub fn stage1(img: &mut [Px], w: usize, h: usize, p: &Params) {
    let noise_div = if p.binned { (p.bin as f32 / 2.0).powi(2).max(1.0) } else { 1.0 } / p.var_scale.max(1e-3);
    let image = crate::quattro_noise::Image { gains: p.gains, iso: p.capture_iso, noise_div, white: p.white };
    crate::quattro_noise::denoise(img, w, h, &image, &crate::quattro_noise::MODEL, &crate::quattro_noise::PARAMETER);
}

/// Brightness smoothing of the full-resolution top layer (`w × h`, units of the stage) in place
/// ([`crate::quattro_noise::brightness_smooth`]).
pub fn luma_smooth(top: &mut [f32], w: usize, h: usize, capture_iso: f32) {
    crate::quattro_noise::brightness_smooth(top, w, h, capture_iso);
}

/// Expected noise variance of a grid value (units of the stage, before the white balance, full grid)
/// ([`crate::quattro_noise::MODEL`]).
pub fn grid_variance(v: Px) -> [f64; 3] {
    crate::quattro_noise::MODEL.variance(v).map(f64::from)
}

/// Response of the 2 × 2 mean of the 2× bicubic enlargement (a = −0.75) on the coarse grid.
const UPDOWN: [f32; 5] = [-0.017_578_125, 0.078_125, 0.878_906_25, 0.078_125, -0.017_578_125];

fn filter_updown(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let cx = |x: isize, n: usize| x.clamp(0, n as isize - 1) as usize;
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..5).map(|k| UPDOWN[k] * src[y * w + cx(x as isize + k as isize - 2, w)]).sum();
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..5).map(|k| UPDOWN[k] * tmp[cx(y as isize + k as isize - 2, h) * w + x]).sum();
        }
    });
    out
}

/// Colour grid from the full-resolution layers of the preparation (bottom/middle = top + 2× bicubic enlargement of
/// the grid differences, top at full resolution): top averaged 2 × 2, differences averaged 2 × 2 and the response
/// of enlargement and averaging undone iteratively. `x`: layers (bottom, middle, top), `w × h` (even sizes used).
pub fn grid_from_layers(x: [&[f32]; 3], w: usize, h: usize) -> (usize, usize, Vec<Px>) {
    let (gw, gh) = (w / 2, h / 2);
    let box2 = |f: &(dyn Fn(usize) -> f32 + Sync)| -> Vec<f32> {
        (0..gw * gh)
            .into_par_iter()
            .map(|i| {
                let (gx, gy) = (i % gw, i / gw);
                let p = 2 * gy * w + 2 * gx;
                0.25 * (f(p) + f(p + 1) + f(p + w) + f(p + w + 1))
            })
            .collect()
    };
    let top = box2(&|p| x[2][p]);
    let diff: [Vec<f32>; 2] = std::array::from_fn(|c| {
        let g = box2(&|p| x[c][p] - x[2][p]);
        // Van Cittert: d ← d + (g − H·d)
        let mut d = g.clone();
        for _ in 0..8 {
            let hd = filter_updown(&d, gw, gh);
            d.par_iter_mut().zip(g.par_iter().zip(hd.par_iter())).for_each(|(d, (g, hd))| *d += g - hd);
        }
        d
    });
    let grid = (0..gw * gh).map(|i| [top[i] + diff[0][i], top[i] + diff[1][i], top[i]]).collect();
    (gw, gh, grid)
}

/// 2× bicubic enlargement (a = −0.75, as in the preparation) of one grid channel to `w × h`.
pub fn enlarge(src: &[f32], gw: usize, gh: usize, w: usize, h: usize) -> Vec<f32> {
    const A: f32 = -0.75;
    let k = |d: f32| {
        let d = d.abs();
        if d <= 1.0 {
            ((A + 2.0) * d - (A + 3.0)) * d * d + 1.0
        } else if d < 2.0 {
            ((A * d - 5.0 * A) * d + 8.0 * A) * d - 4.0 * A
        } else {
            0.0
        }
    };
    let taps = |x: usize, n: usize| -> ([usize; 4], [f32; 4]) {
        let s = (x as f32 + 0.5) * 0.5 - 0.5;
        let i = s.floor();
        let t = s - i;
        let i = i as isize;
        let c = |j: isize| j.clamp(0, n as isize - 1) as usize;
        ([c(i - 1), c(i), c(i + 1), c(i + 2)], [k(1.0 + t), k(t), k(1.0 - t), k(2.0 - t)])
    };
    let cols: Vec<([usize; 4], [f32; 4])> = (0..w).map(|x| taps(x, gw)).collect();
    let mut tmp = vec![0f32; gh * w];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (i, wt) = &cols[x];
            *o = (0..4).map(|j| wt[j] * src[y * gw + i[j]]).sum();
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (i, wt) = taps(y, gh);
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..4).map(|j| wt[j] * tmp[i[j] * w + x]).sum();
        }
    });
    out
}

/// [`enlarge`] without over- and undershoot: every value stays within the range of the 2 × 2 grid cells it lies
/// between. For grids with peaks far above their surroundings (the highlights of a merged bracket, clipped cells set
/// to neutral): the negative lobes of the bicubic kernel would otherwise turn the pixels around a peak dark or
/// coloured.
pub fn enlarge_limited(src: &[f32], gw: usize, gh: usize, w: usize, h: usize) -> Vec<f32> {
    let mut out = enlarge(src, gw, gh, w, h);
    if gw == 0 || gh == 0 {
        return out;
    }
    let cells = |x: usize, n: usize| -> (usize, usize) {
        let i = ((x as f32 + 0.5) * 0.5 - 0.5).floor() as isize;
        let c = |j: isize| j.clamp(0, n as isize - 1) as usize;
        (c(i), c(i + 1))
    };
    let cols: Vec<(usize, usize)> = (0..w).map(|x| cells(x, gw)).collect();
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (y0, y1) = cells(y, gh);
        for (x, o) in row.iter_mut().enumerate() {
            let (x0, x1) = cols[x];
            let v = [src[y0 * gw + x0], src[y0 * gw + x1], src[y1 * gw + x0], src[y1 * gw + x1]];
            let lo = v[0].min(v[1]).min(v[2]).min(v[3]);
            let hi = v[0].max(v[1]).max(v[2]).max(v[3]);
            *o = o.clamp(lo, hi);
        }
    });
    out
}

/// [`layers_from_grid`] for grids with peaks far above their surroundings (see [`enlarge_limited`]): the same
/// result written as the layer's grey share of the top layer plus the enlarged difference of the grid to that grey,
/// the difference enlarged without over- and undershoot. A neutral peak then leaves its surroundings untouched.
pub fn layers_from_grid_limited(x: &mut [Vec<f32>; 3], w: usize, h: usize, before: &[Px], after: &[Px], gw: usize, gh: usize, neutral: [f32; 3]) {
    let d: [Vec<f32>; 3] = std::array::from_fn(|c| {
        let g: Vec<f32> = before.iter().zip(after.iter()).map(|(b, a)| a[c] - neutral[c] * b[2]).collect();
        enlarge_limited(&g, gw, gh, w, h)
    });
    let [p0, p1, p2] = x;
    p0.par_iter_mut().zip(p1.par_iter_mut()).zip(p2.par_iter_mut()).enumerate().for_each(|(i, ((b, m), t))| {
        let top = *t;
        // a pixel much darker than its cell must not take the whole change of the cell's mean: no layer below zero,
        // the top layer at most one stop darker than measured
        *b = (top * neutral[0] + d[0][i]).max(0.0);
        *m = (top * neutral[1] + d[1][i]).max(0.0);
        *t = (top * neutral[2] + d[2][i]).max(0.5 * top * neutral[2]);
    });
}

/// Put the denoised grid back into the full-resolution layers: colour from the denoised grid (enlarged 2× bicubic as
/// in the preparation), fine detail of the top layer (its difference to its own enlarged grid value) added as a
/// neutral change, i.e. on all layers in the ratio of the grey of the shot white balance (`neutral`: layer value of
/// grey per unit of the top layer). Thus the fine top-layer noise becomes brightness noise only.
/// `GRAINEO_QUATTRO_DETAIL=layers`: the previous way (detail on all layers equally).
pub fn layers_from_grid(x: &mut [Vec<f32>; 3], w: usize, h: usize, before: &[Px], after: &[Px], gw: usize, gh: usize, neutral: [f32; 3]) {
    let ch = |f: &dyn Fn(&Px, &Px) -> f32| -> Vec<f32> { before.iter().zip(after.iter()).map(|(b, a)| f(b, a)).collect() };
    if crate::quattro_hi::developer_switch("GRAINEO_QUATTRO_DETAIL").is_ok_and(|v| v == "layers") {
        let d_top = enlarge(&ch(&|b, a| a[2] - b[2]), gw, gh, w, h);
        let d_b = enlarge(&ch(&|b, a| (a[0] - a[2]) - (b[0] - b[2])), gw, gh, w, h);
        let d_m = enlarge(&ch(&|b, a| (a[1] - a[2]) - (b[1] - b[2])), gw, gh, w, h);
        let [p0, p1, p2] = x;
        p0.par_iter_mut().zip(p1.par_iter_mut()).zip(p2.par_iter_mut()).enumerate().for_each(|(i, ((b, m), t))| {
            *t += d_top[i];
            *b += d_top[i] + d_b[i];
            *m += d_top[i] + d_m[i];
        });
        return;
    }
    let top_before = enlarge(&ch(&|b, _| b[2]), gw, gh, w, h);
    let col: [Vec<f32>; 3] = std::array::from_fn(|c| enlarge(&ch(&|_, a| a[c]), gw, gh, w, h));
    let [p0, p1, p2] = x;
    p0.par_iter_mut().zip(p1.par_iter_mut()).zip(p2.par_iter_mut()).enumerate().for_each(|(i, ((b, m), t))| {
        let d = *t - top_before[i];
        *b = col[0][i] + d * neutral[0];
        *m = col[1][i] + d * neutral[1];
        *t = col[2][i] + d * neutral[2];
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_enlargement_stays_within_its_cells() {
        // one cell far above its surroundings: the plain enlargement undershoots around it, the limited one does not
        let (gw, gh) = (12, 10);
        let mut g = vec![1.0f32; gw * gh];
        g[5 * gw + 6] = 100.0;
        let plain = enlarge(&g, gw, gh, 2 * gw, 2 * gh);
        let limited = enlarge_limited(&g, gw, gh, 2 * gw, 2 * gh);
        assert!(plain.iter().any(|v| *v < 0.0), "the plain enlargement undershoots");
        assert!(limited.iter().all(|v| (1.0..=100.0).contains(v)));
        // a smooth grid is not touched
        let smooth: Vec<f32> = (0..gw * gh).map(|i| 1.0 + 0.01 * (i % gw) as f32).collect();
        let (a, b) = (enlarge(&smooth, gw, gh, 2 * gw, 2 * gh), enlarge_limited(&smooth, gw, gh, 2 * gw, 2 * gh));
        for y in 4..2 * gh - 4 {
            for x in 4..2 * gw - 4 {
                assert!((a[y * 2 * gw + x] - b[y * 2 * gw + x]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn neutral_peak_leaves_its_surroundings_neutral() {
        // grey everywhere (layer values in the ratio `neutral`), one cell 50 × brighter: no pixel turns coloured or dark
        let (gw, gh) = (12, 10);
        let (w, h) = (2 * gw, 2 * gh);
        let neutral = [0.5f32, 0.6, 1.0];
        let level = |i: usize| if i == 5 * gw + 6 { 50.0f32 } else { 1.0 };
        let grid: Vec<Px> = (0..gw * gh).map(|i| neutral.map(|n| n * level(i))).collect();
        let top: Vec<f32> = (0..w * h).map(|i| level((i / w / 2) * gw + (i % w) / 2)).collect();
        let mut x = [top.clone(), top.clone(), top.clone()];
        layers_from_grid_limited(&mut x, w, h, &grid, &grid, gw, gh, neutral);
        for i in 0..w * h {
            assert!((x[0][i] - neutral[0] * top[i]).abs() < 1e-4 && (x[1][i] - neutral[1] * top[i]).abs() < 1e-4 && (x[2][i] - top[i]).abs() < 1e-4, "pixel {i}");
        }
        // a darkened cell does not take a pixel below half its measured top value
        let mut after = grid.clone();
        after[2 * gw + 2] = [0.0, 0.0, -5.0];
        let mut x = [top.clone(), top.clone(), top.clone()];
        layers_from_grid_limited(&mut x, w, h, &grid, &after, gw, gh, neutral);
        assert!(x[2].iter().zip(&top).all(|(a, t)| *a >= 0.5 * t) && x[0].iter().all(|v| *v >= 0.0));
    }

    #[test]
    fn grid_round_trip() {
        // layers built like the preparation: grid differences enlarged, top at full resolution
        let (gw, gh) = (40, 30);
        let grid: Vec<Px> = (0..gw * gh).map(|i| {
            let (x, y) = ((i % gw) as f32, (i / gw) as f32);
            [0.3 + 0.01 * (x * 0.7).sin(), 0.25 + 0.01 * (y * 0.5).cos(), 0.2]
        }).collect();
        let (w, h) = (2 * gw, 2 * gh);
        let d0 = enlarge(&grid.iter().map(|p| p[0] - p[2]).collect::<Vec<_>>(), gw, gh, w, h);
        let d1 = enlarge(&grid.iter().map(|p| p[1] - p[2]).collect::<Vec<_>>(), gw, gh, w, h);
        let top: Vec<f32> = (0..w * h).map(|i| 0.2 + 0.002 * ((i % w) as f32 * 1.3).sin()).collect();
        let b: Vec<f32> = (0..w * h).map(|i| top[i] + d0[i]).collect();
        let m: Vec<f32> = (0..w * h).map(|i| top[i] + d1[i]).collect();
        let (_, _, g) = grid_from_layers([&b, &m, &top], w, h);
        for y in 3..gh - 3 {
            for x in 3..gw - 3 {
                let (a, e) = (g[y * gw + x], grid[y * gw + x]);
                assert!(((a[0] - a[2]) - (e[0] - e[2])).abs() < 1e-4 && ((a[1] - a[2]) - (e[1] - e[2])).abs() < 1e-4);
            }
        }
    }
}
