//! High-ISO chain, step 1: outliers of the stored values.
//!
//! At high gain single values of a layer stand far outside the noise of their surroundings. They are found with
//! the sensor's noise model (a value beyond the range of its neighbours by more than its own noise deviation) and
//! replaced with order statistics of the same layer (Lee 1983; Bovik, Huang and Munson 1983).

use rayon::prelude::*;

use super::{Px, GRID_MODEL};

/// BT.601 luma and colour differences, and the way back.
const TO_YUV: [f64; 9] = [0.299, 0.587, 0.114, -0.14713, -0.28886, 0.436, 0.615, -0.51499, -0.10001];
const FROM_YUV: [f64; 9] = [1.0, 0.0, 1.13983, 1.0, -0.39465, -0.5806, 1.0, 2.03211, 0.0];

/// `left` applied after `right`, both 3 × 3 and stored row by row.
fn chained(left: &[f64; 9], right: &[f64; 9]) -> [f64; 9] {
    let mut out = [0f64; 9];
    for (line, target) in out.chunks_exact_mut(3).enumerate() {
        let factors = &left[3 * line..3 * line + 3];
        for (column, entry) in target.iter_mut().enumerate() {
            for (through, factor) in factors.iter().enumerate() {
                *entry += factor * right[3 * through + column];
            }
        }
    }
    out
}

/// The output colour of a grid cell as luma and colour differences, and back: the outlier step of the grid judges
/// the colour of a replacement there.
#[derive(Clone, Copy, Debug)]
pub struct ColourView {
    forward: [f64; 9],
    back: [f64; 9],
}

impl ColourView {
    /// `gains`: white balance on the grid values (middle layer = 1), `matrix`: white-balanced values → output
    /// colour, `iso_factor`: capture ISO / 100. `None` for a matrix that cannot be inverted.
    pub fn new(gains: [f64; 3], matrix: [f64; 9], iso_factor: f64) -> Option<Self> {
        let to_output: [f64; 9] = std::array::from_fn(|i| iso_factor * matrix[i] * gains[i % 3]);
        if !to_output.iter().all(|v| v.is_finite()) {
            return None;
        }
        let inverse = crate::matrix::Mat3(to_output).inverse()?.0;
        Some(Self { forward: chained(&TO_YUV, &to_output), back: chained(&inverse, &FROM_YUV) })
    }

    #[inline]
    fn yuv(&self, p: &Px) -> [f32; 3] {
        let m = &self.forward;
        std::array::from_fn(|i| (m[i * 3 + 2] * f64::from(p[2]) + m[i * 3 + 1] * f64::from(p[1]) + m[i * 3] * f64::from(p[0])) as f32)
    }

    #[inline]
    fn cell(&self, yuv: [f32; 3]) -> Px {
        let m = &self.back;
        std::array::from_fn(|i| (m[i * 3 + 2] * f64::from(yuv[2]) + m[i * 3 + 1] * f64::from(yuv[1]) + m[i * 3] * f64::from(yuv[0])) as f32)
    }
}

/// Noise deviation of grid value `v` of layer `c`.
#[inline]
fn grid_sigma(c: usize, v: f32) -> f32 {
    let [a, b, q] = GRID_MODEL[c];
    ((b * v.abs() + a) + (v * v) * q).sqrt()
}

/// Outliers of the colour grid. A cell counts as one when its three layers together exceed the largest (or fall
/// below the smallest) values of the 24 cells around it by more than the sum of their noise deviations. It takes
/// those extreme values; of the result the colour differences are limited to the range of the eight cells next to
/// it, the luma stays. Decisions are taken on the input; the outer two rows and columns stay. Returns the number
/// of replaced cells.
pub fn grid(cells: &mut [Px], w: usize, h: usize, view: &ColourView) -> usize {
    if w < 5 || h < 5 {
        return 0;
    }
    let source = cells.to_vec();
    cells
        .par_chunks_mut(w)
        .enumerate()
        .map(|(y, row)| {
            if y < 2 || y + 2 >= h {
                return 0;
            }
            let mut replaced = 0;
            for x in 2..w - 2 {
                let centre = source[y * w + x];
                let (mut high, mut low) = (source[(y - 2) * w + x - 2], source[(y - 2) * w + x - 2]);
                for yy in y - 2..=y + 2 {
                    for xx in x - 2..=x + 2 {
                        if yy == y && xx == x {
                            continue;
                        }
                        let n = &source[yy * w + xx];
                        for c in 0..3 {
                            high[c] = high[c].max(n[c]);
                            low[c] = low[c].min(n[c]);
                        }
                    }
                }
                let allowed = (grid_sigma(0, centre[0]) + grid_sigma(1, centre[1])) + grid_sigma(2, centre[2]);
                let above = (((centre[1] + (centre[0] - high[0])) - high[1]) + centre[2]) - high[2];
                let below = ((((low[0] - centre[0]) + low[1]) - centre[1]) + low[2]) - centre[2];
                let candidate = if above > allowed {
                    high
                } else if below > allowed {
                    low
                } else {
                    continue;
                };
                let mut yuv = view.yuv(&candidate);
                let mut range = [[f32::INFINITY, f32::NEG_INFINITY]; 2];
                for yy in y - 1..=y + 1 {
                    for xx in x - 1..=x + 1 {
                        if yy == y && xx == x {
                            continue;
                        }
                        let n = view.yuv(&source[yy * w + xx]);
                        for c in 0..2 {
                            range[c] = [range[c][0].min(n[c + 1]), range[c][1].max(n[c + 1])];
                        }
                    }
                }
                for c in 0..2 {
                    yuv[c + 1] = yuv[c + 1].clamp(range[c][0], range[c][1]);
                }
                row[x] = view.cell(yuv);
                replaced += 1;
            }
            replaced
        })
        .sum()
}

/// Noise model of the top layer as single-precision values.
const TOP: [f32; 3] = [super::TOP_MODEL[0] as f32, super::TOP_MODEL[1] as f32, super::TOP_MODEL[2] as f32];

/// A dark pair must lie this far below its surroundings, in units and in noise deviations.
const PAIR_DEPTH: f32 = 10.0;
const PAIR_SIGMAS: f32 = 1.0;

/// The `rank`-th smallest (0 = smallest) of `values`, repeated values counted singly.
fn smallest(values: impl Iterator<Item = f32>, rank: usize) -> f32 {
    let mut low = [f32::INFINITY; 3];
    for v in values {
        if v < low[rank] {
            let mut i = rank;
            while i > 0 && v < low[i - 1] {
                low[i] = low[i - 1];
                i -= 1;
            }
            low[i] = v;
        }
    }
    low[rank]
}

/// The third largest of the nine values around (`x`, `y`).
fn third_largest(at: &impl Fn(isize, isize) -> f32, x: isize, y: isize) -> f32 {
    -smallest((-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy))).map(|(dx, dy)| -at(x + dx, y + dy)), 2)
}

/// Dark vertical pairs of the top layer (two pixels above each other in an even and the following odd row): both
/// lie clearly below the ten pixels around them, close to each other, and (away from the border) below the second
/// smallest value of the 11 × 11 window around each. They take the mean of the two pixels two steps away, along
/// the direction in which the surroundings bend less, but never more than the third largest value of their 3 × 3
/// neighbourhood. Decisions are taken on the input. Returns the number of pairs.
pub fn top_pairs(top: &mut [f32], w: usize, h: usize) -> usize {
    if w < 5 || h < 5 {
        return 0;
    }
    let source: &[f32] = top;
    let at = |x: isize, y: isize| source[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let found: Vec<(usize, f32, f32)> = (1..(h - 1) / 2)
        .into_par_iter()
        .flat_map_iter(|half| {
            let y = 2 * half as isize;
            let at = &at;
            (2..w as isize - 1).filter_map(move |x| {
                let (upper, lower) = (at(x, y), at(x, y + 1));
                let around = [at(x - 1, y - 1), at(x, y - 1), at(x + 1, y - 1), at(x - 1, y), at(x + 1, y), at(x - 1, y + 1), at(x + 1, y + 1), at(x - 1, y + 2), at(x, y + 2), at(x + 1, y + 2)];
                let floor = around.iter().copied().fold(f32::INFINITY, f32::min);
                let sigma_upper = (((upper * upper) * TOP[2] + upper.abs() * TOP[1]) + TOP[0]).sqrt();
                let sigma_lower = ((((TOP[2] * lower) * lower) + TOP[0]) + TOP[1] * lower.abs()).sqrt();
                let (need_upper, need_lower) = (sigma_upper * PAIR_SIGMAS, sigma_lower * PAIR_SIGMAS);
                let deep = PAIR_DEPTH < floor - upper && need_upper < floor - upper && PAIR_DEPTH < floor - lower && need_lower < floor - lower;
                if !deep || (upper - lower).abs() >= sigma_upper * 3.0 {
                    return None;
                }
                if x > 4 && x + 5 <= w as isize && y > 4 && y + 5 <= h as isize {
                    let window = |cy: isize| {
                        let at = &at;
                        (cy - 5..=cy + 5).flat_map(move |yy| (x - 5..=x + 5).filter(move |&xx| !(xx == x && (yy == y || yy == y + 1))).map(move |xx| at(xx, yy)))
                    };
                    if !(need_upper < smallest(window(y), 1) - upper && need_lower < smallest(window(y + 1), 1) - lower) {
                        return None;
                    }
                }
                let bend_rows = (((2.0 * at(x + 2, y) - at(x + 2, y - 1)) - at(x + 2, y + 1)) + ((2.0 * at(x - 2, y) - at(x - 2, y - 1)) - at(x - 2, y + 1))).abs();
                let bend_columns = (((2.0 * at(x, y - 2) - at(x - 1, y - 2)) - at(x + 1, y - 2)) + ((2.0 * at(x, y + 2) - at(x - 1, y + 2)) - at(x + 1, y + 2))).abs();
                let (new_upper, new_lower) = if bend_rows <= bend_columns {
                    ((at(x, y + 2) + at(x, y - 2)) * 0.5, (at(x, y - 1) + at(x, y + 3)) * 0.5)
                } else {
                    ((at(x - 2, y) + at(x + 2, y)) * 0.5, (at(x - 2, y + 1) + at(x + 2, y + 1)) * 0.5)
                };
                Some((y as usize * w + x as usize, new_upper.min(third_largest(at, x, y)), new_lower.min(third_largest(at, x, y + 1))))
            })
        })
        .collect();
    for &(i, upper, lower) in &found {
        top[i] = upper;
        top[i + w] = lower;
    }
    found.len()
}

/// Below these 3 × 3 minima the limits of [`top_singles`] tighten to the second and to the third rank of the
/// 5 × 5 neighbours.
const DIM: f32 = 576.0;
const DARK: f32 = 288.0;
/// Largest magnitude a limit can take.
const RANGE: f32 = 16383.0;

/// Third rank of the 24 neighbours `v` from the top (`from_top`) or from the bottom. The first three values seed
/// the ranks with their extreme on the counted side, the last of them that differs from the other extreme, and
/// that other extreme, so a value can enter twice.
fn third_rank(v: &[f32; 24], from_top: bool) -> f32 {
    let sign = if from_top { 1.0 } else { -1.0 };
    let s = |i: usize| sign * v[i];
    let (most, least) = (s(0).max(s(1)).max(s(2)), s(0).min(s(1)).min(s(2)));
    let between = (0..3).rev().map(s).find(|&x| least < x && x <= most).unwrap_or(0.0);
    let mut rank = [most, between, least];
    for i in 3..24 {
        let x = s(i);
        if x >= rank[0] {
            rank = [x, rank[0], rank[1]];
        } else if x > rank[1] {
            rank = [rank[0], x, rank[1]];
        } else {
            rank[2] = rank[2].max(x);
        }
    }
    sign * rank[2]
}

/// Single outliers of the top layer: a pixel beyond the largest or smallest of its eight neighbours by more than
/// its noise deviation takes that value. In dark surroundings the limits tighten to the second (dim) or third
/// (dark) rank of the 24 pixels of the 5 × 5 window, which also catches small clusters. Decisions are taken on the
/// input; the outer three rows and columns stay. Returns the number of changed pixels.
pub fn top_singles(top: &mut [f32], w: usize, h: usize) -> usize {
    if w < 7 || h < 7 {
        return 0;
    }
    let source = top.to_vec();
    let (first, last) = (3usize, w - 3);
    top.par_chunks_mut(w)
        .enumerate()
        .map(|(y, row)| {
            if y < 3 || y > h - 3 {
                return 0;
            }
            let p = |x: usize, yy: usize| source[yy * w + x];
            let mut changed = 0;
            for x in first..=last {
                let (left, right) = ((x - 1).max(first), (x + 1).min(last));
                let centre = p(x, y);
                let ring = [p(left, y - 1), p(x, y - 1), p(right, y - 1), p(x - 1, y), p(x + 1, y), p(left, y + 1), p(x, y + 1), p(right, y + 1)];
                let (mut high, mut low) = (ring.iter().copied().fold(f32::MIN, f32::max), ring.iter().copied().fold(f32::MAX, f32::min));
                let darkest = low.min(centre);
                if darkest < DIM {
                    // a centre with enough larger and smaller values around it lies inside every rank limit (the
                    // third rank is only exact when its three seed values are not all equal, see `third_rank`)
                    let ranks = if darkest >= DARK { 2 } else { 3 };
                    if centre <= high && centre >= low && !(ranks == 3 && p(x - 2, y - 2) == p(x - 2, y - 1) && p(x - 2, y - 1) == p(x - 2, y)) {
                        let (mut larger, mut smaller) = (0u32, 0u32);
                        for yy in y - 2..=y + 2 {
                            for &v in &source[yy * w + x - 2..=yy * w + x + 2] {
                                larger += u32::from(v > centre);
                                smaller += u32::from(v < centre);
                            }
                        }
                        if larger >= ranks && smaller >= ranks {
                            continue;
                        }
                    }
                    let mut around = [0f32; 24];
                    let mut n = 0;
                    for xx in x - 2..=x + 2 {
                        for yy in y - 2..=y + 2 {
                            if xx != x || yy != y {
                                around[n] = p(xx, yy);
                                n += 1;
                            }
                        }
                    }
                    let (upper, lower) = if darkest >= DARK {
                        (-smallest(around.iter().map(|v| -v), 1), smallest(around.iter().copied(), 1))
                    } else {
                        (third_rank(&around, true), third_rank(&around, false))
                    };
                    high = high.min(upper);
                    low = low.max(lower);
                } else {
                    high = high.min(RANGE);
                    low = low.max(-RANGE);
                }
                let bound = if centre > high {
                    high
                } else if centre < low {
                    low
                } else {
                    continue;
                };
                let allowed = (((TOP[2] * bound) * centre + TOP[1] * bound).max(0.0) + TOP[0]).sqrt();
                let lifted = if allowed < low - centre { low } else { centre };
                let out = if allowed < lifted - high { high } else { lifted };
                if out != centre {
                    row[x] = out;
                    changed += 1;
                }
            }
            changed
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ColourView {
        ColourView::new([1.1, 1.0, 1.3], [1.6, -0.4, -0.2, -0.3, 1.5, -0.2, -0.1, -0.4, 1.5], 4.0).unwrap()
    }

    #[test]
    fn colour_view_goes_there_and_back() {
        let v = view();
        let p = [300.0f32, 250.0, 200.0];
        let q = v.cell(v.yuv(&p));
        assert!((0..3).all(|c| (p[c] - q[c]).abs() < 0.05), "{q:?}");
        assert!(ColourView::new([1.0; 3], [0.0; 9], 1.0).is_none());
    }

    #[test]
    fn grid_outlier_is_replaced_noise_is_not() {
        let (w, h) = (16, 12);
        let mut cells: Vec<Px> = (0..w * h).map(|i| [500.0 + (i % 3) as f32, 400.0 + (i % 2) as f32, 300.0]).collect();
        let before = cells.clone();
        assert_eq!(grid(&mut cells, w, h, &view()), 0);
        assert_eq!(cells, before);
        cells[5 * w + 7] = [900.0, 800.0, 700.0];
        assert_eq!(grid(&mut cells, w, h, &view()), 1);
        let p = cells[5 * w + 7];
        assert!((p[0] - 501.0).abs() < 3.0 && (p[1] - 400.5).abs() < 3.0 && (p[2] - 300.0).abs() < 3.0, "{p:?}");
    }

    #[test]
    fn dark_pair_is_filled_from_two_steps_away() {
        let (w, h) = (24, 20);
        let mut top = vec![1000f32; w * h];
        top[8 * w + 10] = 600.0;
        top[9 * w + 10] = 610.0;
        assert_eq!(top_pairs(&mut top, w, h), 1);
        assert!(top.iter().all(|&v| v == 1000.0));
        // a pair that starts in an odd row is left to the single-outlier step
        top[9 * w + 10] = 600.0;
        top[10 * w + 10] = 610.0;
        assert_eq!(top_pairs(&mut top, w, h), 0);
    }

    #[test]
    fn single_outliers_are_clamped_to_their_neighbours() {
        let (w, h) = (20, 16);
        let mut top: Vec<f32> = (0..w * h).map(|i| 2000.0 + (i % 5) as f32).collect();
        let before = top.clone();
        assert_eq!(top_singles(&mut top, w, h), 0);
        top[7 * w + 9] = 3000.0;
        top[9 * w + 12] = 900.0;
        assert_eq!(top_singles(&mut top, w, h), 2);
        assert!(top.iter().zip(&before).all(|(a, b)| (a - b).abs() <= 4.0));
        // in the dark a cluster of two bright pixels goes as well (third rank of the 5 × 5 window)
        let mut dark = vec![100f32; w * h];
        dark[7 * w + 9] = 400.0;
        dark[7 * w + 10] = 400.0;
        assert_eq!(top_singles(&mut dark, w, h), 2);
        assert!(dark.iter().all(|&v| v == 100.0));
    }

    #[test]
    fn third_rank_counts_from_either_end() {
        let v: [f32; 24] = std::array::from_fn(|i| ((i * 7) % 24) as f32);
        assert_eq!(third_rank(&v, true), 21.0);
        assert_eq!(third_rank(&v, false), 2.0);
    }
}
