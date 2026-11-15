//! Tone curve as a point curve: one curve for all channels (RGB) and one each
//! for red, green and blue. Acts in the look stage on the finished, gamma-encoded
//! output values (0 … 1), i.e. on perceptually uniformly
//! distributed values: a point at ¼ hits the dark midtones, not the deep shadows.
//!
//! Interpolation: monotone cubic Hermite splines (Fritsch-Carlson); the curve does not overshoot
//! between the points, rising points give a rising curve. Before the
//! first and after the last point it stays horizontal (as in common photo apps when the
//! black or white point is dragged inwards).
//!
//! Applied via a precomputed table ([`CurveLut`], 4096 samples per channel,
//! linearly interpolated): RGB curve and channel curve are already chained in it.

use serde::{Deserialize, Serialize};

/// Points (input, output) each 0 … 1, sorted by ascending input, including the
/// two end points. Empty = diagonal (no effect).
pub type Points = Vec<[f32; 2]>;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneCurve {
    /// Brightness: acts equally on all three channels
    pub rgb: Points,
    pub red: Points,
    pub green: Points,
    pub blue: Points,
}

/// Channel names in the order of [`ToneCurve::channel`].
pub const CHANNELS: [&str; 4] = ["RGB", "Rot", "Grün", "Blau"];

impl ToneCurve {
    /// Curve 0 = RGB, 1 = red, 2 = green, 3 = blue.
    pub fn channel(&self, i: usize) -> &Points {
        match i {
            0 => &self.rgb,
            1 => &self.red,
            2 => &self.green,
            _ => &self.blue,
        }
    }

    pub fn channel_mut(&mut self, i: usize) -> &mut Points {
        match i {
            0 => &mut self.rgb,
            1 => &mut self.red,
            2 => &mut self.green,
            _ => &mut self.blue,
        }
    }

    /// No effect (all four curves empty or diagonal)? Then the step is skipped.
    pub fn is_identity(&self) -> bool {
        (0..4).all(|i| is_identity(self.channel(i)))
    }
}

/// Does the curve lie on the diagonal? (All points on y = x and end points at 0 and 1;
/// otherwise it would be horizontal outside the points.)
pub fn is_identity(p: &[[f32; 2]]) -> bool {
    p.is_empty()
        || (p.iter().all(|q| (q[0] - q[1]).abs() < 1e-5) && p[0][0] <= 1e-5 && p[p.len() - 1][0] >= 1.0 - 1e-5)
}

/// Fully prepared curve: samples and tangents (Fritsch-Carlson).
pub struct Spline {
    x: Vec<f32>,
    y: Vec<f32>,
    m: Vec<f32>,
}

impl Spline {
    /// `None` for an empty curve (= diagonal).
    pub fn new(p: &[[f32; 2]]) -> Option<Self> {
        if p.is_empty() {
            return None;
        }
        let mut pts: Vec<[f32; 2]> = p.iter().map(|q| [q[0].clamp(0.0, 1.0), q[1].clamp(0.0, 1.0)]).collect();
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let x: Vec<f32> = pts.iter().map(|q| q[0]).collect();
        let y: Vec<f32> = pts.iter().map(|q| q[1]).collect();
        let n = x.len();
        let mut m = vec![0f32; n];
        if n >= 2 {
            let d: Vec<f32> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / (x[k + 1] - x[k]).max(1e-6)).collect();
            m[0] = d[0];
            m[n - 1] = d[n - 2];
            for k in 1..n - 1 {
                m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { 0.5 * (d[k - 1] + d[k]) };
            }
            // Fritsch-Carlson: limit tangents so that no overshoot occurs
            for k in 0..n - 1 {
                if d[k] == 0.0 {
                    m[k] = 0.0;
                    m[k + 1] = 0.0;
                    continue;
                }
                let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
                let s = a * a + b * b;
                if s > 9.0 {
                    let t = 3.0 / s.sqrt();
                    m[k] = t * a * d[k];
                    m[k + 1] = t * b * d[k];
                }
            }
        }
        Some(Self { x, y, m })
    }

    pub fn eval(&self, v: f32) -> f32 {
        let n = self.x.len();
        if v <= self.x[0] || n == 1 {
            return self.y[0];
        }
        if v >= self.x[n - 1] {
            return self.y[n - 1];
        }
        let k = self.x.partition_point(|&xi| xi <= v).clamp(1, n - 1) - 1;
        let h = (self.x[k + 1] - self.x[k]).max(1e-6);
        let t = (v - self.x[k]) / h;
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * self.y[k] + h10 * h * self.m[k] + h01 * self.y[k + 1] + h11 * h * self.m[k + 1]).clamp(0.0, 1.0)
    }
}

/// Value of a single curve at position `v` (for display).
pub fn eval(p: &[[f32; 2]], v: f32) -> f32 {
    Spline::new(p).map_or(v, |s| s.eval(v))
}

/// Table per output channel: channel curve ∘ RGB curve.
pub struct CurveLut {
    lut: [Vec<f32>; 3],
}

impl CurveLut {
    pub const N: usize = 4096;

    /// `None` if the curve has no effect.
    pub fn new(c: &ToneCurve) -> Option<Self> {
        if c.is_identity() {
            return None;
        }
        let master = Spline::new(&c.rgb).filter(|_| !is_identity(&c.rgb));
        let lut = std::array::from_fn(|k| {
            let chan = Spline::new(c.channel(k + 1)).filter(|_| !is_identity(c.channel(k + 1)));
            (0..Self::N)
                .map(|i| {
                    let x = i as f32 / (Self::N - 1) as f32;
                    let x = master.as_ref().map_or(x, |s| s.eval(x));
                    chan.as_ref().map_or(x, |s| s.eval(x))
                })
                .collect()
        });
        Some(Self { lut })
    }

    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|k| {
            let f = c[k].clamp(0.0, 1.0) * (Self::N - 1) as f32;
            let i = (f as usize).min(Self::N - 2);
            let t = f - i as f32;
            let l = &self.lut[k];
            l[i] + (l[i + 1] - l[i]) * t
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonal_is_identity() {
        let mut c = ToneCurve::default();
        assert!(c.is_identity());
        c.rgb = vec![[0.0, 0.0], [0.4, 0.4], [1.0, 1.0]];
        assert!(c.is_identity());
        assert!(CurveLut::new(&c).is_none());
        // End point dragged inwards: no longer the identity
        c.rgb = vec![[0.1, 0.1], [1.0, 1.0]];
        assert!(!c.is_identity());
    }

    #[test]
    fn s_curve_monotonic_and_through_points() {
        let p = vec![[0.0, 0.0], [0.25, 0.18], [0.75, 0.84], [1.0, 1.0]];
        let s = Spline::new(&p).unwrap();
        for q in &p {
            assert!((s.eval(q[0]) - q[1]).abs() < 1e-5);
        }
        let mut last = -1.0;
        for i in 0..=1000 {
            let v = s.eval(i as f32 / 1000.0);
            assert!(v >= last - 1e-6, "not monotonic at {i}");
            last = v;
        }
    }

    #[test]
    fn no_overshoot() {
        // Steep step: cubic splines without limiting would overshoot above 1 or below 0
        let p = vec![[0.0, 0.0], [0.45, 0.02], [0.55, 0.98], [1.0, 1.0]];
        let s = Spline::new(&p).unwrap();
        for i in 0..=1000 {
            let x = i as f32 / 1000.0;
            let v = s.eval(x);
            if x < 0.45 {
                assert!(v <= 0.02 + 1e-6);
            }
            if x > 0.55 {
                assert!(v >= 0.98 - 1e-6);
            }
        }
    }

    #[test]
    fn diagonal_in_look_stage_bit_identical() {
        let (w, h) = (64, 48);
        let img = || crate::Rgb16 { width: w, height: h, data: (0..w * h * 3).map(|i| ((i * 7919) % 65536) as u16).collect() };
        let geo = crate::look::Geo::full(w, h);
        for base in [crate::Settings::default(), crate::Settings { vignette: -0.4, fade: 0.05, ..Default::default() }] {
            let mut a = img();
            crate::look::apply(&mut a, &base, geo);
            let mut b = img();
            let mut s = base.clone();
            s.tone_curve.rgb = vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];
            s.tone_curve.blue = vec![[0.0, 0.0], [1.0, 1.0]];
            crate::look::apply(&mut b, &s, geo);
            assert!(a.data == b.data);
        }
    }

    /// Timing (`cargo test --release -p develop curve::tests::speed -- --ignored --nocapture`)
    #[test]
    #[ignore]
    fn speed() {
        let (w, h) = (4704, 3136);
        let img = || crate::Rgb16 { width: w, height: h, data: (0..w * h * 3).map(|i| ((i * 7919) % 65536) as u16).collect() };
        let geo = crate::look::Geo::full(w, h);
        let s_curve = ToneCurve { rgb: vec![[0.0, 0.0], [0.25, 0.19], [0.75, 0.83], [1.0, 1.0]], ..Default::default() };
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(CurveLut::new(&s_curve));
        }
        eprintln!("build table: {:.1?}", t.elapsed() / 20);
        for (name, s) in [
            ("curve only", crate::Settings { tone_curve: s_curve.clone(), ..Default::default() }),
            ("Vignette", crate::Settings { vignette: -0.3, ..Default::default() }),
            ("vignette + curve", crate::Settings { vignette: -0.3, tone_curve: s_curve.clone(), ..Default::default() }),
        ] {
            let mut best = std::time::Duration::MAX;
            for _ in 0..5 {
                let mut a = img();
                let t = std::time::Instant::now();
                crate::look::apply(&mut a, &s, geo);
                best = best.min(t.elapsed());
            }
            eprintln!("{name}: {best:.1?} ({w}×{h})");
        }
    }

    #[test]
    fn lut_chains_rgb_and_channel() {
        let c = ToneCurve { rgb: vec![[0.0, 0.2], [1.0, 1.0]], red: vec![[0.0, 0.0], [1.0, 0.5]], ..Default::default() };
        let l = CurveLut::new(&c).unwrap();
        let o = l.apply([0.0, 0.0, 1.0]);
        assert!((o[0] - 0.1).abs() < 1e-4 && (o[1] - 0.2).abs() < 1e-4 && (o[2] - 1.0).abs() < 1e-4);
    }
}
