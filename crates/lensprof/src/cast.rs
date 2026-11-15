//! Colour-cast map per channel (Bayer: R/G/B of the camera; Foveon: top/middle/bottom layer).
//!
//! The map holds what remains of a flat field after the radial vignetting: `C_c = F_c / (V · F_c(centre))`
//! on a coarse grid (cell centres evenly over the active image, centre = 1). A value of 1.04 means the lens
//! lets 4 % more of this channel through at that point than in the image centre; the correction divides by it.
//! White balance in the image centre therefore stays unchanged.

use serde::{Deserialize, Serialize};

/// Map size: 33 columns, rows by the aspect ratio, always odd (one cell sits on the centre).
pub const COLS: u16 = 33;

pub fn rows_for(width: usize, height: usize) -> u16 {
    let r = ((COLS as f64 * height as f64 / width.max(1) as f64).ceil() as u16).max(1);
    r | 1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CastGrid {
    pub cols: u16,
    pub rows: u16,
    /// relative response per cell and channel, row by row (`rows · cols` entries)
    pub gain: Vec<[f32; 3]>,
}

/// Quality of a flat-field measurement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CastQuality {
    /// RMS of the smooth fit in log units per channel (≈ relative residual)
    pub fit_rms: [f32; 3],
    /// consistency of the two shots of a pair (RMS of log(A/B) after removing a plane); `None` = single shot
    pub pair_rms: Option<f32>,
    /// blocks rejected as dust, hot pixels or other outliers
    pub dust_cells: u32,
    /// colour cast in the corner per channel (value farthest from 1, see [`CastGrid::corner_extremes`])
    pub corner: [f32; 3],
    pub score: u8,
}

/// Colour-cast map for one focal length and aperture (`av` = 2·log₂ N).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CastEntry {
    pub focal_length: f64,
    pub av: f64,
    pub grid: CastGrid,
    pub quality: CastQuality,
}

impl CastGrid {
    pub fn neutral(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        CastGrid { cols, rows, gain: vec![[1.0; 3]; cols as usize * rows as usize] }
    }

    /// Is the grid well formed (size matches, values finite and positive)?
    pub fn is_valid(&self) -> bool {
        self.cols > 0 && self.rows > 0 && self.gain.len() == self.cols as usize * self.rows as usize && self.gain.iter().all(|g| g.iter().all(|v| v.is_finite() && *v > 0.0))
    }

    fn cell(&self, i: usize, j: usize) -> [f32; 3] {
        self.gain[j * self.cols as usize + i]
    }

    /// Value at image position `u`, `v` ∈ [0, 1] (0 = left/top edge of the active image), bilinear between
    /// the cell centres, constant beyond the outer centres.
    pub fn at(&self, u: f32, v: f32) -> [f32; 3] {
        let (c, r) = (self.cols as usize, self.rows as usize);
        let axis = |t: f32, n: usize| -> (usize, usize, f32) {
            let x = (t * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
            let i0 = (x.floor() as usize).min(n - 1);
            let i1 = (i0 + 1).min(n - 1);
            (i0, i1, x - i0 as f32)
        };
        let (i0, i1, fx) = axis(u, c);
        let (j0, j1, fy) = axis(v, r);
        let (a, b, cc, d) = (self.cell(i0, j0), self.cell(i1, j0), self.cell(i0, j1), self.cell(i1, j1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = cc[k] + (d[k] - cc[k]) * fx;
            top + (bottom - top) * fy
        })
    }

    /// Mix two maps in the logarithm (`t` = 0 → `a`, 1 → `b`). A map of another size is sampled at the
    /// cell centres of `a`.
    pub fn blend(a: &Self, b: &Self, t: f32) -> Self {
        let (c, r) = (a.cols as usize, a.rows as usize);
        let same = a.cols == b.cols && a.rows == b.rows;
        let gain = (0..r)
            .flat_map(|j| (0..c).map(move |i| (i, j)))
            .map(|(i, j)| {
                let ga = a.cell(i, j);
                let gb = if same { b.cell(i, j) } else { b.at((i as f32 + 0.5) / c as f32, (j as f32 + 0.5) / r as f32) };
                std::array::from_fn(|k| (ga[k].max(1e-6).ln() * (1.0 - t) + gb[k].max(1e-6).ln() * t).exp())
            })
            .collect();
        CastGrid { cols: a.cols, rows: a.rows, gain }
    }

    /// Per channel the corner value farthest from 1 (four corner cells).
    pub fn corner_extremes(&self) -> [f32; 3] {
        let (c, r) = (self.cols as usize, self.rows as usize);
        let corners = [self.cell(0, 0), self.cell(c - 1, 0), self.cell(0, r - 1), self.cell(c - 1, r - 1)];
        std::array::from_fn(|k| corners.iter().map(|g| g[k]).fold(1.0f32, |best, v| if (v - 1.0).abs() > (best - 1.0).abs() { v } else { best }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_odd() {
        assert_eq!(rows_for(6000, 4000), 23);
        assert_eq!(rows_for(4000, 4000), 33);
        assert_eq!(rows_for(10, 1), 5);
        assert_eq!(rows_for(1000, 1) % 2, 1);
    }

    #[test]
    fn at_interpolates_and_clamps() {
        let mut g = CastGrid::neutral(3, 3);
        g.gain[4] = [2.0, 1.0, 0.5]; // centre
        assert_eq!(g.at(0.5, 0.5), [2.0, 1.0, 0.5]);
        assert_eq!(g.at(0.0, 0.0), [1.0; 3]);
        let h = g.at(1.0 / 3.0, 0.5); // halfway between left and centre cell centre
        assert!((h[0] - 1.5).abs() < 1e-5 && (h[2] - 0.75).abs() < 1e-5, "{h:?}");
    }

    #[test]
    fn blend_in_log_and_corners() {
        let a = CastGrid { cols: 1, rows: 1, gain: vec![[1.0, 2.0, 4.0]] };
        let b = CastGrid { cols: 1, rows: 1, gain: vec![[4.0, 2.0, 1.0]] };
        let m = CastGrid::blend(&a, &b, 0.5);
        assert!(m.gain[0].iter().all(|v| (v - 2.0).abs() < 1e-5), "{:?}", m.gain);
        let mut g = CastGrid::neutral(3, 3);
        g.gain[0] = [1.03, 1.0, 0.96];
        g.gain[8] = [0.99, 1.0, 1.01];
        assert_eq!(g.corner_extremes(), [1.03, 1.0, 0.96]);
    }
}
