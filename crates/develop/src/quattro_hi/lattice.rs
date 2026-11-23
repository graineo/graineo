//! Highlight pixels of the dp Quattro: a regular lattice of grid cells whose pixels are less sensitive than their
//! neighbours. Their stored values do not fit the surrounding ones, so the chain replaces them before it filters.
//!
//! Default: every lattice value becomes the mean of the nearest values of the same layer around it. By switch
//! ([`Replacement::Calibrated`]): the lattice values are first brought to the level of their surroundings with a
//! response curve estimated from the image and then kept wherever they fit the range of their neighbours.

use rayon::prelude::*;

use super::{Px, GRID_MODEL, TOP_MODEL};

/// Largest lattice spacing accepted from a file (grid cells).
pub const MAX_SPACING: usize = 4096;

/// Positions of the highlight-pixel cells in the colour grid of the active image area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lattice {
    /// first lattice column and row inside the active area
    first: (usize, usize),
    /// spacing along the rows and the columns
    step: (usize, usize),
}

impl Lattice {
    /// From the camera's values: `info` = first column, first row, column spacing, row spacing on the sensor's grid,
    /// `area` = active image area in full-resolution pixels (left, top first). `None` unless every value is a whole
    /// number in range and the spacings lie in 1 ..= [`MAX_SPACING`].
    pub fn from_camera(info: &[f64], area: &[f64]) -> Option<Self> {
        let whole = |v: f64, max: usize| (v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v <= max as f64).then_some(v as usize);
        if info.len() != 4 || area.len() != 4 {
            return None;
        }
        let (x0, y0) = (whole(info[0], 1 << 20)?, whole(info[1], 1 << 20)?);
        let (dx, dy) = (whole(info[2], MAX_SPACING)?, whole(info[3], MAX_SPACING)?);
        let (left, upper) = (whole(area[0], 1 << 20)? / 2, whole(area[1], 1 << 20)? / 2);
        if dx == 0 || dy == 0 {
            return None;
        }
        // first lattice position at or after the edge of the active area
        let start = |edge: usize, origin: usize, step: usize| if origin >= edge { origin - edge } else { (step - (edge - origin) % step) % step };
        Some(Self { first: (start(left, x0, dx), start(upper, y0, dy)), step: (dx, dy) })
    }

    /// Is grid cell (`x`, `y`) a lattice cell?
    #[inline]
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.first.0 && y >= self.first.1 && (x - self.first.0) % self.step.0 == 0 && (y - self.first.1) % self.step.1 == 0
    }

    /// Lattice columns of a grid `gw` wide.
    fn columns(&self, gw: usize) -> impl Iterator<Item = usize> + Clone {
        (self.first.0..gw).step_by(self.step.0)
    }

    /// Lattice rows of a grid `gh` high.
    fn rows(&self, gh: usize) -> impl Iterator<Item = usize> + Clone {
        (self.first.1..gh).step_by(self.step.1)
    }

    /// Number of lattice cells in a `gw × gh` grid.
    pub fn count(&self, gw: usize, gh: usize) -> usize {
        self.columns(gw).count() * self.rows(gh).count()
    }
}

/// How the lattice values are replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replacement {
    /// leave the stored values
    Off,
    /// mean of the neighbours of the same layer (default)
    Neighbours,
    /// own value through an estimated response curve, limited to the neighbours' range
    Calibrated,
}

/// Mean of the values `at(x ± d, y)` and `at(x, y ± d)` that lie inside `w × h`.
fn cross_mean(at: impl Fn(usize, usize) -> f32, x: usize, y: usize, d: usize, w: usize, h: usize) -> Option<f32> {
    let mut sum = 0f32;
    let mut n = 0u32;
    let mut take = |ok: bool, xx: usize, yy: usize| {
        if ok {
            sum += at(xx, yy);
            n += 1;
        }
    };
    take(x >= d, x.wrapping_sub(d), y);
    take(x + d < w, x + d, y);
    take(y >= d, x, y.wrapping_sub(d));
    take(y + d < h, x, y + d);
    (n > 0).then(|| sum / n as f32)
}

/// Bottom and middle layer of every lattice cell of the grid: mean of the four cells next to it (same layer).
/// Returns the number of cells.
pub fn fill_grid(grid: &mut [Px], gw: usize, gh: usize, lattice: &Lattice) -> usize {
    let rows: Vec<usize> = lattice.rows(gh).collect();
    // lattice cells are never neighbours of each other along a row unless the spacing is 1: read from a copy then
    let source = (lattice.step.0 == 1 || lattice.step.1 == 1).then(|| grid.to_vec());
    for &y in &rows {
        for x in lattice.columns(gw) {
            for c in 0..2 {
                let read = |xx: usize, yy: usize| source.as_deref().unwrap_or(grid)[yy * gw + xx][c];
                if let Some(m) = cross_mean(read, x, y, 1, gw, gh) {
                    grid[y * gw + x][c] = m;
                }
            }
        }
    }
    lattice.count(gw, gh)
}

/// The four top-layer pixels of every lattice cell (`top`: `2gw × 2gh`): each the mean of the pixels two columns
/// and two rows away (the same position inside the neighbouring cells).
pub fn fill_top(top: &mut [f32], gw: usize, gh: usize, lattice: &Lattice) {
    let (w, h) = (2 * gw, 2 * gh);
    if lattice.step.0 == 1 || lattice.step.1 == 1 {
        return;
    }
    for gy in lattice.rows(gh) {
        for gx in lattice.columns(gw) {
            for (x, y) in [(2 * gx, 2 * gy), (2 * gx + 1, 2 * gy), (2 * gx, 2 * gy + 1), (2 * gx + 1, 2 * gy + 1)] {
                if let Some(m) = cross_mean(|xx, yy| top[yy * w + xx], x, y, 2, w, h) {
                    top[y * w + x] = m;
                }
            }
        }
    }
}

/// Number of brightness bands of the response estimate (half stops from 16 units on).
const BANDS: usize = 20;

fn band(v: f32) -> usize {
    ((v.max(16.0) / 16.0).log2() * 2.0) as usize
}

/// Response of the lattice values of one kind: per brightness band the median factor that brings a lattice value to
/// the mean of its neighbours, from the cells whose neighbours agree within their noise.
struct Response {
    gain: [f32; BANDS],
}

impl Response {
    /// `samples`: (lattice value, mean of the neighbours, spread of the neighbours, noise deviation at the mean).
    fn estimate(samples: &[(f32, f32, f32, f32)]) -> Self {
        let mut ratios: Vec<Vec<f32>> = vec![Vec::new(); BANDS];
        for &(own, mean, spread, sigma) in samples {
            if own > 1.0 && mean > 1.0 && spread < 2.0 * sigma {
                ratios[band(own).min(BANDS - 1)].push(mean / own);
            }
        }
        let mut gain = [f32::NAN; BANDS];
        for (g, r) in gain.iter_mut().zip(ratios.iter_mut()) {
            if r.len() >= 24 {
                let mid = r.len() / 2;
                *g = *r.select_nth_unstable_by(mid, f32::total_cmp).1;
            }
        }
        // bands without enough flat cells take the nearest estimated band, 1 if there is none
        let known: Vec<usize> = (0..BANDS).filter(|&i| gain[i].is_finite()).collect();
        for i in 0..BANDS {
            if !gain[i].is_finite() {
                gain[i] = known.iter().min_by_key(|&&k| k.abs_diff(i)).map_or(1.0, |&k| gain[k]);
            }
        }
        Self { gain }
    }

    fn apply(&self, v: f32) -> f32 {
        v * self.gain[band(v).min(BANDS - 1)]
    }
}

fn sigma(model: &[f32; 3], v: f32) -> f32 {
    (model[2] * v * v + model[1] * v.abs() + model[0]).abs().sqrt()
}

/// One kind of lattice value (a grid layer or one of the four top-layer positions of the cell) on a plane where the
/// same kind of the neighbouring cells lies `d` pixels away.
fn calibrate(values: &mut [f32], w: usize, h: usize, cells: &[(usize, usize)], d: usize, model: &[f32; 3]) {
    let inside: Vec<(usize, usize)> = cells.iter().copied().filter(|&(x, y)| x >= d && y >= d && x + d < w && y + d < h).collect();
    let ring = |x: usize, y: usize| -> [f32; 8] {
        let at = |dx: isize, dy: isize| values[(y as isize + dy * d as isize) as usize * w + (x as isize + dx * d as isize) as usize];
        [at(-1, -1), at(0, -1), at(1, -1), at(-1, 0), at(1, 0), at(-1, 1), at(0, 1), at(1, 1)]
    };
    let samples: Vec<(f32, f32, f32, f32)> = inside
        .par_iter()
        .map(|&(x, y)| {
            let n = ring(x, y);
            let cross = [n[1], n[3], n[4], n[6]];
            let mean = cross.iter().sum::<f32>() / 4.0;
            let spread = cross.iter().copied().fold(f32::MIN, f32::max) - cross.iter().copied().fold(f32::MAX, f32::min);
            (values[y * w + x], mean, spread, sigma(model, mean))
        })
        .collect();
    let response = Response::estimate(&samples);
    let fixed: Vec<f32> = inside
        .par_iter()
        .map(|&(x, y)| {
            let n = ring(x, y);
            let (low, high) = (n.iter().copied().fold(f32::MAX, f32::min), n.iter().copied().fold(f32::MIN, f32::max));
            let v = response.apply(values[y * w + x]);
            let s = sigma(model, v);
            if v - high > s {
                high
            } else if low - v > s {
                low
            } else {
                v
            }
        })
        .collect();
    for (&(x, y), v) in inside.iter().zip(fixed) {
        values[y * w + x] = v;
    }
}

/// [`Replacement::Calibrated`]: bottom and middle layer of the lattice cells and their four top-layer pixels keep
/// their own values, corrected by the estimated response of their kind and limited to the range of the eight
/// nearest values of the same kind (± one noise deviation). The grid's top value becomes the mean of the four
/// corrected top-layer pixels. Returns the number of cells.
pub fn calibrate_cells(grid: &mut [Px], top: &mut [f32], gw: usize, gh: usize, lattice: &Lattice) -> usize {
    if lattice.step.0 == 1 || lattice.step.1 == 1 {
        return 0;
    }
    let cells: Vec<(usize, usize)> = lattice.rows(gh).flat_map(|y| lattice.columns(gw).map(move |x| (x, y))).collect();
    for c in 0..2 {
        let mut plane: Vec<f32> = grid.iter().map(|p| p[c]).collect();
        calibrate(&mut plane, gw, gh, &cells, 1, &GRID_MODEL[c]);
        for &(x, y) in &cells {
            grid[y * gw + x][c] = plane[y * gw + x];
        }
    }
    let (w, h) = (2 * gw, 2 * gh);
    let model = TOP_MODEL.map(|v| v as f32);
    for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let pixels: Vec<(usize, usize)> = cells.iter().map(|&(x, y)| (2 * x + ox, 2 * y + oy)).collect();
        calibrate(top, w, h, &pixels, 2, &model);
    }
    for &(x, y) in &cells {
        let i = 2 * y * w + 2 * x;
        grid[y * gw + x][2] = 0.25 * (top[i] + top[i + 1] + top[i + w] + top[i + w + 1]);
    }
    cells.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_values_are_checked() {
        let area = [8.0, 4.0, 100.0, 100.0];
        let l = Lattice::from_camera(&[0.0, 0.0, 16.0, 4.0], &area).unwrap();
        // the active area starts at grid cell (4, 2): first lattice column 16 − 4, first row 4 − 2
        assert!(l.contains(12, 2) && l.contains(28, 6) && !l.contains(13, 2) && !l.contains(12, 3) && !l.contains(0, 0));
        assert_eq!(l.count(40, 12), 2 * 3);
        for bad in [[0.0, 0.0, 0.0, 4.0], [0.0, 0.0, 16.0, 0.0], [0.0, 0.0, 0.5, 4.0], [0.0, 0.0, 16.0, 5000.0], [0.0, 0.0, f64::INFINITY, 4.0], [0.0, 0.0, 1.9e19, 4.0], [-1.0, 0.0, 16.0, 4.0], [0.0, f64::NAN, 16.0, 4.0]] {
            assert_eq!(Lattice::from_camera(&bad, &area), None, "{bad:?}");
        }
        assert_eq!(Lattice::from_camera(&[0.0, 0.0, 16.0], &area), None);
        assert_eq!(Lattice::from_camera(&[0.0, 0.0, 16.0, 4.0], &[0.5, 0.0, 1.0, 1.0]), None);
    }

    #[test]
    fn lattice_cells_take_the_mean_of_their_layer() {
        let (gw, gh) = (40, 12);
        let l = Lattice::from_camera(&[0.0, 0.0, 16.0, 4.0], &[0.0; 4]).unwrap();
        let mut grid: Vec<Px> = (0..gw * gh).map(|i| [100.0 + (i % gw) as f32, 50.0, 30.0]).collect();
        let mut top = vec![30f32; 4 * gw * gh];
        for y in (0..gh).step_by(4) {
            for x in (0..gw).step_by(16) {
                grid[y * gw + x] = [900.0, 900.0, 30.0];
                top[2 * y * 2 * gw + 2 * x] = 5.0;
            }
        }
        assert_eq!(fill_grid(&mut grid, gw, gh, &l), 9);
        fill_top(&mut top, gw, gh, &l);
        assert_eq!(grid[4 * gw + 16], [116.0, 50.0, 30.0]);
        // at the left edge only the neighbours that exist count
        assert_eq!(grid[4 * gw], [(101.0 + 100.0 + 100.0) / 3.0, 50.0, 30.0]);
        assert!(top.iter().all(|&v| v == 30.0));
        assert_eq!(grid[4 * gw + 17][0], 117.0);
    }

    #[test]
    fn calibrated_cells_follow_their_surroundings() {
        let (gw, gh) = (160, 80);
        let l = Lattice::from_camera(&[0.0, 0.0, 16.0, 4.0], &[0.0; 4]).unwrap();
        let level = |x: usize| 400.0 + 10.0 * (x / 64) as f32;
        let mut grid: Vec<Px> = (0..gw * gh).map(|i| [level(i % gw), level(i % gw), level(i % gw)]).collect();
        let mut top: Vec<f32> = (0..4 * gw * gh).map(|i| level(i % (2 * gw) / 2)).collect();
        for y in (0..gh).step_by(4) {
            for x in (0..gw).step_by(16) {
                for c in 0..2 {
                    grid[y * gw + x][c] *= 0.8;
                }
                for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    top[(2 * y + oy) * 2 * gw + 2 * x + ox] *= 0.8;
                }
            }
        }
        assert_eq!(calibrate_cells(&mut grid, &mut top, gw, gh, &l), 10 * 20);
        let p = grid[40 * gw + 80];
        assert!((p[0] - 410.0).abs() < 1.0 && (p[1] - 410.0).abs() < 1.0 && (p[2] - 410.0).abs() < 1.0, "{p:?}");
    }
}
