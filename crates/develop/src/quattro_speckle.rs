//! Quattro: repair of isolated outliers ("speckles") before the first noise reduction stage, from capture ISO 400 on.
//!
//! At higher ISO the full-resolution top layer of the Quattro sensor carries single dark pixels and dark vertical
//! pairs (the two pixels of one 2-row cell of the top layer), plus some single bright pixels, far outside the noise of
//! their surroundings. Left in, the noise reduction cannot remove them (they are too far from their neighbours to be
//! averaged) and the sharpening and the colour stage turn them into visible black or white points ("pepper"). The
//! camera maker's software removes them before its noise reduction. This module does the same, in two passes on
//! the top layer and one on the colour grid:
//!
//! 1. **Dark pairs** (top layer): a vertical pair on an even row whose two values both lie more than `k_dark` noise
//!    deviations below the darkest of its ten neighbours (3 × 4 block around the pair) is lifted to that neighbour.
//! 2. **Single outliers** (top layer): a value more than `k_dark` deviations below the second-darkest of its eight
//!    neighbours, or more than `k_bright` deviations above the second-brightest, is clamped to that neighbour. Using
//!    the second rank keeps one-pixel lines (they have two neighbours along the line) and lets a remaining dark
//!    neighbour of a pair not shield its partner.
//! 3. **Colour grid** (bottom and middle layer): the same single-outlier clamp per layer on the grid.
//!
//! Each layer is judged and repaired from its own values: an outlier is replaced by a neighbour of the same layer.
//! The other layers only veto a repair where a point of light (a star, a reflection) stands out in every layer; they
//! never supply the repaired value. The noise deviation comes from the grid noise model ([`crate::quattro_noise::MODEL`],
//! a full-resolution top value having four times the variance of the 2 × 2 grid mean). Clamping a pixel to the range
//! of its neighbours is a long-known impulse-noise filter; the thresholds were chosen so that the share of dark points
//! in flat areas matches the maker's output.
//!
//! Units: those of the first stage ([`crate::quattro_nr`]).

use rayon::prelude::*;

use crate::quattro_nr::Px;

/// Share of a bottom/middle layer's relative excursion the top layer of the same grid cell must show for the cell to
/// count as a point of light (kept).
const LIGHT_SHARE: f32 = 0.25;

/// Thresholds in noise deviations.
#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    pub k_dark: f32,
    pub k_bright: f32,
    /// grid layers (bottom, middle)
    pub k_grid: f32,
    /// remaining single outliers of the smoothed top layer, both directions ([`top_layer_after`])
    pub k_after: f32,
}

/// Fitted thresholds (see the module text).
pub const THRESHOLDS: Thresholds = Thresholds { k_dark: 2.5, k_bright: 2.5, k_grid: 3.0, k_after: 1.5 };

/// Noise deviation of one full-resolution top-layer value `v`.
#[inline]
fn top_sigma(v: f32) -> f32 {
    let m = &crate::quattro_noise::MODEL;
    (4.0 * (m.a[2] + m.b[2] * v.max(0.0))).sqrt()
}

/// Dark pairs and single outliers of the full-resolution top layer `t` (`w × h`) in place. Returns the number of
/// repaired values (pairs count twice).
///
/// `grid` (`gw × gh`, bottom/middle/top): a bright top value whose grid cell is a bright outlier in the bottom or
/// middle layer as well is a point of light and stays (the grid only vetoes, it never supplies the repaired value).
pub fn top_layer(t: &mut [f32], w: usize, h: usize, th: &Thresholds, grid: &[Px], gw: usize, gh: usize) -> usize {
    if w < 8 || h < 8 {
        return 0;
    }
    let light = bright_cells(grid, gw, gh, th.k_grid);
    let mut n = dark_pairs(t, w, h, th.k_dark);
    n += clamp_singles(t, w, h, |v| top_sigma(v), th.k_dark, th.k_bright, |x, y| {
        let (gx, gy) = (x / 2, y / 2);
        gx < gw && gy < gh && light[gy * gw + gx]
    });
    n
}

/// Grid cells whose bottom or middle value lies more than `k` deviations above the second-brightest of its eight
/// neighbours (points of light; border: false).
fn bright_cells(g: &[Px], gw: usize, gh: usize, k: f32) -> Vec<bool> {
    let m = crate::quattro_noise::MODEL;
    let mut out = vec![false; g.len()];
    if gw < 3 || gh < 3 || g.len() < gw * gh {
        return out;
    }
    out.par_chunks_mut(gw).enumerate().for_each(|(y, row)| {
        if y == 0 || y + 1 >= gh {
            return;
        }
        for x in 1..gw - 1 {
            let c = g[y * gw + x];
            row[x] = (0..2).any(|ch| {
                let (mut h1, mut h2) = (f32::MIN, f32::MIN);
                for (dy, dx) in [(0usize, 0usize), (0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)] {
                    let v = g[(y + dy - 1) * gw + x + dx - 1][ch];
                    if v > h1 {
                        h2 = h1;
                        h1 = v;
                    } else if v > h2 {
                        h2 = v;
                    }
                }
                c[ch] - h2 > k * (m.a[ch] + m.b[ch] * h2.max(0.0)).sqrt()
            });
        }
    });
    out
}

/// Remaining single outliers of a smoothed full-resolution top layer: values beyond the second-darkest or
/// second-brightest of their eight neighbours by more than `k` deviations of the unsmoothed layer are clamped to
/// that neighbour (both directions alike, so the mean stays).
pub fn top_layer_after(t: &mut [f32], w: usize, h: usize, k: f32) -> usize {
    if w < 8 || h < 8 || k <= 0.0 {
        return 0;
    }
    clamp_singles(t, w, h, |v| top_sigma(v), k, k, |_, _| false)
}

/// Pass 1: dark vertical pairs on even rows.
fn dark_pairs(t: &mut [f32], w: usize, h: usize, k: f32) -> usize {
    let src = t.to_vec();
    // each pair writes rows y and y + 1: work on blocks of two rows
    let count: usize = t
        .par_chunks_mut(2 * w)
        .enumerate()
        .map(|(cell, rows)| {
            let y = 2 * cell;
            if y == 0 || y + 3 > h || rows.len() < 2 * w {
                return 0;
            }
            let mut n = 0;
            for x in 1..w - 1 {
                let (a, b) = (src[y * w + x], src[(y + 1) * w + x]);
                let mut lo = f32::MAX;
                for yy in y - 1..=y + 2 {
                    for xx in x - 1..=x + 1 {
                        if xx == x && (yy == y || yy == y + 1) {
                            continue;
                        }
                        lo = lo.min(src[yy * w + xx]);
                    }
                }
                let limit = k * top_sigma(lo);
                if lo - a > limit && lo - b > limit {
                    rows[x] = lo;
                    rows[w + x] = lo;
                    n += 2;
                }
            }
            n
        })
        .sum();
    count
}

/// Pass 2 / grid: values beyond the second-darkest or second-brightest of the 8 neighbours by more than
/// `k_dark` / `k_bright` deviations (`sigma` of the neighbour value) are clamped to that neighbour.
fn clamp_singles(t: &mut [f32], w: usize, h: usize, sigma: impl Fn(f32) -> f32 + Sync, k_dark: f32, k_bright: f32, keep_bright: impl Fn(usize, usize) -> bool + Sync) -> usize {
    let src = t.to_vec();
    t.par_chunks_mut(w)
        .enumerate()
        .map(|(y, row)| {
            if y == 0 || y + 1 >= h {
                return 0;
            }
            let mut n = 0;
            for x in 1..w - 1 {
                let c = src[y * w + x];
                // two smallest and two largest of the 8 neighbours
                let (mut l1, mut l2, mut h1, mut h2) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for (dy, dx) in [(0usize, 0usize), (0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)] {
                    let v = src[(y + dy - 1) * w + x + dx - 1];
                    if v < l1 {
                        l2 = l1;
                        l1 = v;
                    } else if v < l2 {
                        l2 = v;
                    }
                    if v > h1 {
                        h2 = h1;
                        h1 = v;
                    } else if v > h2 {
                        h2 = v;
                    }
                }
                if l2 - c > k_dark * sigma(l2) {
                    row[x] = l2;
                    n += 1;
                } else if c - h2 > k_bright * sigma(h2) && !keep_bright(x, y) {
                    row[x] = h2;
                    n += 1;
                }
            }
            n
        })
        .sum()
}

/// Single outliers of the bottom and middle layer of the colour grid (`gw × gh`) in place. A point of light (a star,
/// a reflection) is an outlier in every layer: a value is kept when the top layer of the same cell stands out of its
/// neighbours in the same direction by more than `k_grid` of its own deviations (the top layer only vetoes, it never
/// supplies the repaired value).
pub fn grid(g: &mut [Px], gw: usize, gh: usize, th: &Thresholds) -> usize {
    if gw < 8 || gh < 8 {
        return 0;
    }
    let m = crate::quattro_noise::MODEL;
    let src = g.to_vec();
    let k = th.k_grid;
    g.par_chunks_mut(gw)
        .enumerate()
        .map(|(y, row)| {
            if y == 0 || y + 1 >= gh {
                return 0;
            }
            let mut n = 0;
            for x in 1..gw - 1 {
                let c = src[y * gw + x];
                let mut lo = [f32::MAX; 3 * 2];
                let mut hi = [f32::MIN; 3 * 2];
                for (dy, dx) in [(0usize, 0usize), (0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)] {
                    let v = src[(y + dy - 1) * gw + x + dx - 1];
                    for ch in 0..3 {
                        let val = v[ch];
                        let (l1, l2) = (lo[2 * ch], lo[2 * ch + 1]);
                        if val < l1 {
                            lo[2 * ch + 1] = l1;
                            lo[2 * ch] = val;
                        } else if val < l2 {
                            lo[2 * ch + 1] = val;
                        }
                        let (h1, h2) = (hi[2 * ch], hi[2 * ch + 1]);
                        if val > h1 {
                            hi[2 * ch + 1] = h1;
                            hi[2 * ch] = val;
                        } else if val > h2 {
                            hi[2 * ch + 1] = val;
                        }
                    }
                }
                let sd = |ch: usize, v: f32| (m.a[ch] + m.b[ch] * v.max(0.0)).sqrt();
                let (t_lo, t_hi) = (lo[5], hi[5]);
                // relative excursion of the top layer of the cell (signed: above the second-brightest / below the
                // second-darkest neighbour, in units of the neighbour value plus its deviation)
                let top_up = (c[2] - t_hi) / (t_hi.abs() + sd(2, t_hi));
                let top_down = (t_lo - c[2]) / (t_lo.abs() + sd(2, t_lo));
                for ch in 0..2 {
                    let (l2, h2) = (lo[2 * ch + 1], hi[2 * ch + 1]);
                    let up = (c[ch] - h2) / (h2.abs() + sd(ch, h2));
                    let down = (l2 - c[ch]) / (l2.abs() + sd(ch, l2));
                    // a point of light: the top layer follows by at least a quarter of the relative excursion
                    let dark_light = top_down > LIGHT_SHARE * down;
                    let bright_light = top_up > LIGHT_SHARE * up;
                    if l2 - c[ch] > k * sd(ch, l2) && !dark_light {
                        row[x][ch] = l2;
                        n += 1;
                    } else if c[ch] - h2 > k * sd(ch, h2) && !bright_light {
                        row[x][ch] = h2;
                        n += 1;
                    }
                }
            }
            n
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32, level: f32, amp: f32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
                level + amp * ((s >> 8) as f32 / (1u32 << 24) as f32 - 0.5)
            })
            .collect()
    }

    #[test]
    fn removes_dark_points_and_pairs_keeps_lines() {
        let (w, h) = (64, 48);
        let mut t = noise(w * h, 7, 200.0, 10.0);
        let flat = t.clone();
        // single dark pixel, dark vertical pair on an even row, bright pixel
        t[10 * w + 10] = 20.0;
        t[20 * w + 30] = 30.0;
        t[21 * w + 30] = 25.0;
        t[30 * w + 40] = 900.0;
        // a one-pixel dark line (column 50) must stay
        for y in 5..40 {
            t[y * w + 50] = 40.0;
        }
        let g: Vec<Px> = vec![[300.0, 300.0, 200.0]; (w / 2) * (h / 2)];
        let n = top_layer(&mut t, w, h, &THRESHOLDS, &g, w / 2, h / 2);
        assert!(n >= 4, "{n}");
        for i in [10 * w + 10, 20 * w + 30, 21 * w + 30, 30 * w + 40] {
            assert!((t[i] - 200.0).abs() < 12.0, "{i}: {}", t[i]);
        }
        for y in 6..39 {
            assert_eq!(t[y * w + 50], 40.0, "line row {y}");
        }
        // plain noise stays untouched
        let changed = (0..w * h).filter(|&i| i % w != 50 && t[i] != flat[i] && ![10 * w + 10, 20 * w + 30, 21 * w + 30, 30 * w + 40].contains(&i)).count();
        assert!(changed <= 2, "{changed}");
    }

    #[test]
    fn grid_clamps_bottom_and_middle_only_and_keeps_lights() {
        let (gw, gh) = (32, 24);
        let a = noise(gw * gh, 3, 500.0, 20.0);
        let mut g: Vec<Px> = a.iter().map(|&v| [v, v * 0.9, v * 0.7]).collect();
        // outlier in the bottom and middle layer only: repaired
        g[12 * gw + 12] = [5000.0, 3000.0, 350.0];
        // a point of light (all layers): kept
        g[6 * gw + 20] = [5000.0, 3000.0, 4000.0];
        grid(&mut g, gw, gh, &THRESHOLDS);
        assert!(g[12 * gw + 12][0] < 600.0 && g[12 * gw + 12][1] < 600.0);
        assert_eq!(g[12 * gw + 12][2], 350.0);
        assert_eq!(g[6 * gw + 20], [5000.0, 3000.0, 4000.0]);
    }
}
