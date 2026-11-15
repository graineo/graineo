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
    /// Strength step 0 … 4 of the colour noise slider
    pub step: usize,
    /// White of the raw range in the units of the stage
    pub white: f32,
}

/// Noise reduction of one grid `img` (`w × h`, units see module) in place ([`crate::quattro_noise`]).
pub fn stage1(img: &mut [Px], w: usize, h: usize, p: &Params) {
    let image = crate::quattro_noise::Image { gains: p.gains, iso: p.capture_iso, binned: p.binned, white: p.white };
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

/// Put the denoised grid back into the full-resolution layers: colour from the denoised grid (enlarged 2× bicubic as
/// in the preparation), fine detail of the top layer (its difference to its own enlarged grid value) added as a
/// neutral change, i.e. on all layers in the ratio of the grey of the shot white balance (`neutral`: layer value of
/// grey per unit of the top layer). Thus the fine top-layer noise becomes brightness noise only.
/// `GRAINEO_QUATTRO_DETAIL=layers`: the previous way (detail on all layers equally).
pub fn layers_from_grid(x: &mut [Vec<f32>; 3], w: usize, h: usize, before: &[Px], after: &[Px], gw: usize, gh: usize, neutral: [f32; 3]) {
    let ch = |f: &dyn Fn(&Px, &Px) -> f32| -> Vec<f32> { before.iter().zip(after.iter()).map(|(b, a)| f(b, a)).collect() };
    if std::env::var("GRAINEO_QUATTRO_DETAIL").is_ok_and(|v| v == "layers") {
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
