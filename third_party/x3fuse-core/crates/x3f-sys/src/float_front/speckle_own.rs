//! graineo addition: own detection of single-sample outliers ("speckles") for the Merrill float
//! frontend, switched on with `GRAINEO_MERRILL_NEW=outliers` (see [`crate::merrill_new`]).
//!
//! Criterion, per layer on its own (no colour-space distances, no comparison between the neighbours):
//!
//! 1. Noise model of the image itself: for every layer the sample noise `σ(v)² = a + b·v` (Poisson
//!    shot noise plus read noise) is fitted to the image, from the residuals `v − mean(4 neighbours)`
//!    of a sparse grid, binned by level, with a robust spread (median absolute deviation) per bin and
//!    a weighted least-squares line through the bins; bins well above the line (texture) are dropped
//!    and the line refitted. Literature: Foi, Trimeche, Katkovnik, Egiazarian, "Practical
//!    Poissonian-Gaussian noise modeling and fitting for single-image raw-data", IEEE TIP 17 (2008).
//! 2. A sample is an outlier of its layer if it lies strictly outside the range of its eight
//!    neighbours in that layer **and** its distance to their median exceeds `T · s`, where
//!    `s = max(σ(median), spread of the neighbours)` (spread = inner quartile distance / 1.349, the
//!    Gaussian equivalent; it keeps texture and edges from being taken for noise) and
//!    `T = τ · DespAdjust/15` (camera calibration per layer, scaled by the request strength; τ per
//!    layer measured, [`TAU`]).
//! 3. Replacement: unchanged (`replace_own`): only the marked layers change, from their own
//!    neighbours; the other layers only steer how strongly (a peak in all layers is kept as structure).

use rayon::prelude::*;

/// Threshold per layer in robust standard deviations at `DespAdjust` = 15 (the threshold scales with
/// the camera's `DespAdjust`, default 15/15/5, i.e. 5.5/7/4.5 standard deviations). Measured: with
/// these values the number of marks per layer is of the same order as the default detection on DP2M5418,
/// SDIM0042 (SD1M), DP2M2453 (ISO 3200) and DP1M ISO 6400.
pub(super) const TAU: [f32; 3] = [5.5, 7.0, 13.5];

/// Fitted noise model and thresholds of one image.
#[derive(Clone, Debug)]
pub(super) struct OwnSpeckle {
    /// `σ² = a + b·v` per layer
    a: [f32; 3],
    b: [f32; 3],
    /// `T` per layer
    t: [f32; 3],
}

impl OwnSpeckle {
    /// Estimates the noise model on `data` (RGB, `row_stride` floats per row) inside `rect`
    /// (`[left, top, right, bottom]`, inclusive) and sets the thresholds from `adjust`.
    pub(super) fn estimate(
        data: &[f32],
        width: usize,
        height: usize,
        row_stride: usize,
        adjust: [f64; 3],
    ) -> Self {
        const BIN: f32 = 32.0;
        const BINS: usize = 512;
        let mut a = [1.0f32; 3];
        let mut b = [0.0f32; 3];
        for c in 0..3 {
            // residuals per level bin on every second row and column
            let per_row: Vec<Vec<(u16, f32)>> = (2..height.saturating_sub(2))
                .into_par_iter()
                .step_by(2)
                .map(|y| {
                    let mut out = Vec::with_capacity(width / 2);
                    for x in (2..width.saturating_sub(2)).step_by(2) {
                        let at = |dx: isize, dy: isize| {
                            data[(y as isize + dy) as usize * row_stride
                                + (x as isize + dx) as usize * 3
                                + c]
                        };
                        let mean = 0.25 * (at(-1, 0) + at(1, 0) + at(0, -1) + at(0, 1));
                        if !(mean > 0.0) {
                            continue;
                        }
                        let bin = ((mean / BIN) as usize).min(BINS - 1) as u16;
                        out.push((bin, (at(0, 0) - mean).abs()));
                    }
                    out
                })
                .collect();
            let mut bins: Vec<Vec<f32>> = vec![Vec::new(); BINS];
            for row in per_row {
                for (bin, e) in row {
                    bins[bin as usize].push(e);
                }
            }
            // robust σ per bin: residual = v − mean of 4 → variance 1.25·σ²
            let mut points: Vec<(f64, f64, f64)> = Vec::new(); // level, σ², weight
            for (i, v) in bins.iter_mut().enumerate() {
                if v.len() < 200 {
                    continue;
                }
                let mid = v.len() / 2;
                let (_, med, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
                let s = f64::from(*med) * 1.4826 / 1.25f64.sqrt();
                points.push(((i as f64 + 0.5) * f64::from(BIN), s * s, v.len() as f64));
            }
            let fit = |pts: &[(f64, f64, f64)]| -> Option<(f64, f64)> {
                let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for &(x, y, w) in pts {
                    sw += w;
                    sx += w * x;
                    sy += w * y;
                    sxx += w * x * x;
                    sxy += w * x * y;
                }
                let det = sw * sxx - sx * sx;
                if sw <= 0.0 || det.abs() < 1e-9 {
                    return None;
                }
                let slope = (sw * sxy - sx * sy) / det;
                Some(((sy - slope * sx) / sw, slope))
            };
            let mut line = fit(&points);
            for _ in 0..3 {
                let Some((a0, b0)) = line else { break };
                let kept: Vec<_> = points
                    .iter()
                    .copied()
                    .filter(|&(x, y, _)| y <= 1.5 * (a0 + b0 * x).max(1.0))
                    .collect();
                if kept.len() < 3 {
                    break;
                }
                line = fit(&kept);
            }
            if let Some((a0, b0)) = line {
                b[c] = b0.max(0.0) as f32;
                // offset at least the smallest measured variance, never below one count²
                let floor = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
                a[c] = a0.max(floor.min(4.0)).max(1.0) as f32;
            } else if !points.is_empty() {
                // a single level: constant noise (weighted mean variance)
                let w: f64 = points.iter().map(|p| p.2).sum();
                a[c] = (points.iter().map(|p| p.1 * p.2).sum::<f64>() / w).max(1.0) as f32;
            }
            if std::env::var_os("X3F_SPECKLE_DEBUG").is_some() {
                eprintln!(
                    "speckle own: layer {c}: sigma^2 = {:.2} + {:.4}·v ({} bins)",
                    a[c],
                    b[c],
                    points.len()
                );
            }
        }
        // test aid: X3F_SPECKLE_TAU=a,b,c replaces TAU (measuring only)
        let tau: [f32; 3] = std::env::var("X3F_SPECKLE_TAU")
            .ok()
            .and_then(|v| {
                let t: Vec<f32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                (t.len() == 3).then(|| [t[0], t[1], t[2]])
            })
            .unwrap_or(TAU);
        let t: [f32; 3] = std::array::from_fn(|c| tau[c] * (adjust[c] / 15.0) as f32);
        let own = Self { a, b, t };
        if let Ok(v) = std::env::var("X3F_SPECKLE_DEBUG") {
            // number of extrema whose excess exceeds z robust standard deviations, per layer
            let zs: Vec<f32> = if v.contains(',') {
                v.split(',').filter_map(|x| x.parse().ok()).collect()
            } else {
                vec![2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0]
            };
            let counts: Vec<[Vec<usize>; 3]> = (2..height.saturating_sub(2))
                .into_par_iter()
                .map(|y| {
                    let mut c: [Vec<usize>; 3] = std::array::from_fn(|_| vec![0; zs.len()]);
                    for x in 2..width.saturating_sub(2) {
                        for (k, ck) in c.iter_mut().enumerate() {
                            if let Some(z) = own.score(data, row_stride, x, y, k) {
                                for (i, &t) in zs.iter().enumerate() {
                                    ck[i] += usize::from(z > t);
                                }
                            }
                        }
                    }
                    c
                })
                .collect();
            for k in 0..3 {
                let tot: Vec<usize> = (0..zs.len())
                    .map(|i| counts.iter().map(|c| c[k][i]).sum())
                    .collect();
                eprintln!(
                    "speckle own: layer {k} adjust {:.1}: counts above z {zs:?}: {tot:?}",
                    adjust[k]
                );
            }
        }
        own
    }

    /// Excess of an extremum over the neighbour median in units of `max(σ(median), spread)`; `None` if
    /// the sample is not a strict extremum of its eight neighbours.
    fn score(&self, data: &[f32], row_stride: usize, x: usize, y: usize, c: usize) -> Option<f32> {
        let v = data[y * row_stride + x * 3 + c];
        let mut n = super::NEIGHBOR_OFFSETS.map(|(dx, dy)| {
            data[(y as i32 + dy) as usize * row_stride + (x as i32 + dx) as usize * 3 + c]
        });
        let up = n.iter().all(|&q| v > q);
        let down = n.iter().all(|&q| v < q);
        if !up && !down {
            return None;
        }
        n.sort_by(f32::total_cmp);
        let median = 0.5 * (n[3] + n[4]);
        let spread = (n[5] - n[2]) / 1.349;
        let s = self.sigma(c, median).max(spread);
        Some(if up { v - median } else { median - v } / s)
    }

    /// Noise standard deviation of layer `c` at level `v`.
    #[inline]
    pub(super) fn sigma(&self, c: usize, v: f32) -> f32 {
        (self.a[c] + self.b[c] * v.max(0.0)).sqrt()
    }

    /// Threshold of layer `c` at level `v` (for the evidence of the replacement).
    #[inline]
    pub(super) fn threshold(&self, c: usize, v: f32) -> f32 {
        self.t[c] * self.sigma(c, v)
    }

    /// Channel mask (C0 bit 4, C1 bit 2, C2 bit 1) of the sample at (x, y).
    #[inline]
    pub(super) fn predicate(&self, data: &[f32], row_stride: usize, x: usize, y: usize) -> u32 {
        let mut mask = 0u32;
        for c in 0..3 {
            if self
                .score(data, row_stride, x, y, c)
                .is_some_and(|z| z > self.t[c])
            {
                mask |= 4 >> c;
            }
        }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat image with deterministic pseudo-noise and one spike in layer 1.
    fn image(w: usize, h: usize) -> Vec<f32> {
        let mut state = 12345u32;
        let mut noise = || {
            // sum of four uniforms → roughly Gaussian, σ ≈ 10
            (0..4)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                })
                .sum::<f32>()
                * 17.3
        };
        // two brightness levels (left/right half), same noise
        (0..w * h * 3)
            .map(|i| if (i / 3) % w < w / 2 { 1000.0 } else { 2000.0 } + noise())
            .collect()
    }

    #[test]
    fn finds_spike_and_estimates_noise() {
        let (w, h) = (96, 64);
        let mut data = image(w, h);
        data[(30 * w + 20) * 3 + 1] += 400.0;
        let own = OwnSpeckle::estimate(&data, w, h, w * 3, [15.0; 3]);
        let s = own.sigma(1, 1000.0);
        assert!((7.0..14.0).contains(&s), "sigma {s}");
        assert_eq!(own.predicate(&data, w * 3, 20, 30), 2);
        let marks = (2..h - 2)
            .flat_map(|y| (2..w - 2).map(move |x| (x, y)))
            .filter(|&(x, y)| own.predicate(&data, w * 3, x, y) != 0)
            .count();
        assert_eq!(marks, 1);
    }
}
