//! Small single-channel image helpers: proxies, half-size pyramid, max filter, Lanczos-3 table.

use rayon::prelude::*;

/// Single-channel float image.
#[derive(Clone, Debug)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub p: Vec<f32>,
}

impl Img {
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.p[y * self.w + x]
    }

    /// Bilinear sample, `None` outside the valid area.
    #[inline]
    pub fn sample(&self, x: f64, y: f64) -> Option<f32> {
        if self.w < 2 || self.h < 2 || x < 0.0 || y < 0.0 || x > (self.w - 2) as f64 || y > (self.h - 2) as f64 {
            return None;
        }
        let (x0, y0) = (x as usize, y as usize);
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let a = self.at(x0, y0);
        let b = self.at(x0 + 1, y0);
        let c = self.at(x0, y0 + 1);
        let d = self.at(x0 + 1, y0 + 1);
        Some((a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy)
    }

    /// Maximum of the 4 neighbours, `None` outside.
    #[inline]
    pub fn sample_max(&self, x: f64, y: f64) -> Option<f32> {
        if self.w < 2 || self.h < 2 || x < 0.0 || y < 0.0 || x > (self.w - 2) as f64 || y > (self.h - 2) as f64 {
            return None;
        }
        let (x0, y0) = (x as usize, y as usize);
        Some(self.at(x0, y0).max(self.at(x0 + 1, y0)).max(self.at(x0, y0 + 1)).max(self.at(x0 + 1, y0 + 1)))
    }

    /// [`Self::sample_max`] that also covers the last column and row (their neighbours beyond the edge are the edge
    /// itself), `None` outside the image.
    #[inline]
    pub fn sample_max_edge(&self, x: f64, y: f64) -> Option<f32> {
        if self.w < 1 || self.h < 1 || !(x >= 0.0 && y >= 0.0 && x <= (self.w - 1) as f64 && y <= (self.h - 1) as f64) {
            return None;
        }
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        Some(self.at(x0, y0).max(self.at(x1, y0)).max(self.at(x0, y1)).max(self.at(x1, y1)))
    }

    /// 2×2 box downscale.
    pub fn half(&self) -> Img {
        let (w, h) = (self.w / 2, self.h / 2);
        let mut p = vec![0f32; w * h];
        p.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = 0.25 * (self.at(2 * x, 2 * y) + self.at(2 * x + 1, 2 * y) + self.at(2 * x, 2 * y + 1) + self.at(2 * x + 1, 2 * y + 1));
            }
        });
        Img { w, h, p }
    }

    /// `log2(clamp(v / k, lo, hi))` per pixel: frames of different exposure become comparable, clipped and noisy
    /// areas become flat.
    pub fn log_clamped(&self, k: f32, lo: f32, hi: f32) -> Img {
        Img { w: self.w, h: self.h, p: self.p.par_iter().map(|&v| (v / k).clamp(lo, hi).log2()).collect() }
    }
}

/// Separable max filter with radius `r` (clamped edges).
pub fn max_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            *o = s[a..=b].iter().copied().fold(f32::MIN, f32::max);
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let mut m = f32::MIN;
            for yy in a..=b {
                m = m.max(tmp[yy * w + x]);
            }
            *o = m;
        }
    });
    out
}

/// Lanczos-3 weights for 256 sub-pixel phases, each phase normalised to sum 1.
pub struct Lanczos {
    t: Vec<[f32; 6]>,
}

impl Lanczos {
    pub fn new() -> Lanczos {
        const N: usize = 256;
        let l = |d: f64| -> f64 {
            if d.abs() < 1e-9 {
                1.0
            } else if d.abs() < 3.0 {
                let pd = std::f64::consts::PI * d;
                3.0 * pd.sin() * (pd / 3.0).sin() / (pd * pd)
            } else {
                0.0
            }
        };
        let t = (0..=N)
            .map(|i| {
                let f = i as f64 / N as f64;
                let mut w = [0f64; 6];
                for (k, o) in w.iter_mut().enumerate() {
                    *o = l(k as f64 - 2.0 - f);
                }
                let s: f64 = w.iter().sum();
                w.map(|v| (v / s) as f32)
            })
            .collect();
        Lanczos { t }
    }

    /// Weights for the taps −2 … +3 around the integer position for the fraction `f` ∈ [0, 1).
    #[inline]
    pub fn w(&self, f: f64) -> &[f32; 6] {
        &self.t[(f * 256.0).round() as usize]
    }
}

impl Default for Lanczos {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanczos_phases_sum_to_one_and_interpolate() {
        let l = Lanczos::new();
        for i in 0..=256 {
            let s: f32 = l.w(i as f64 / 256.0).iter().sum();
            assert!((s - 1.0).abs() < 1e-5);
        }
        // phase 0 is the identity
        let w0 = l.w(0.0);
        assert!((w0[2] - 1.0).abs() < 1e-6 && w0[3].abs() < 1e-6);
    }

    #[test]
    fn max_filter_spreads_peaks() {
        let mut p = vec![0f32; 49];
        p[24] = 1.0;
        let m = max_filter(&p, 7, 7, 2);
        assert_eq!(m.iter().filter(|v| **v == 1.0).count(), 25);
    }
}
