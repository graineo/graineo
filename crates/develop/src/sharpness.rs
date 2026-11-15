//! Sigma's sharpness slider (SPP "Sharpness", −2 … +2) with the same effect as in SPP.
//!
//! The development stores a detail image "L" (high-pass of the luminance, fixed point ×4). The SPP base
//! from x3fuse-core is already sharpened with the amount for sharpness 0. A different slider value
//! acts incrementally, matching SPP's output: `Sharpen(Δ)` with `Δ = amt(s) − amt(0)` on the already
//! sharpened image, which gives the same result as SPP when the slider is moved from 0 to `s`.
//! Sharpness 0 changes nothing (Δ = 0 → no computation, bit-identical to the base).
//!
//! Alternative form (test switch `GRAINEO_MERRILL_NEW=sharpness`, see [`own_amount`]): an exponential
//! amount law (every slider step doubles or halves the sharpening of the base) and a luminance unsharp
//! mask with a soft limit, applied incrementally on the base. Unsharp masking itself is common
//! knowledge (e.g. Gonzalez & Woods, "Digital Image Processing", high-boost filtering); the detail
//! image and the base amount come from the base (CAMF `SharpenAmount`).

/// Detail image "L" for an image ([`crate::Linear`]): one value per pixel.
#[derive(Clone, Debug)]
pub struct SharpDetail {
    /// High-pass of the luminance (fixed point ×4) (detail image of the base)
    pub l: Vec<i16>,
    /// Base of the sharpening strength (CAMF noise parameter `SharpenAmount`); the SPP base
    /// is already sharpened with this amount
    pub base: f32,
    /// Size factor `√((sx + sy)/2)` of the sharpening strength (1 = full resolution)
    pub scale: f32,
}

/// `base` of a [`SharpDetail`] that holds the high-pass of Sigma's Classic output step instead of the
/// detail image "L" (cube-root domain, see `classic_new::output`); the sharpness slider then acts
/// there and the paths of this module leave it alone.
pub const CLASSIC_HIGH_PASS: f32 = -1.0;

impl SharpDetail {
    /// High-pass of the Classic output step (not the detail image "L").
    pub fn is_classic_high_pass(&self) -> bool {
        self.base == CLASSIC_HIGH_PASS
    }

    /// Crop (image width `width`).
    pub fn crop(&self, width: usize, x: usize, y: usize, w: usize, h: usize) -> SharpDetail {
        if self.l.is_empty() {
            return SharpDetail { l: Vec::new(), base: self.base, scale: self.scale };
        }
        let mut l = Vec::with_capacity(w * h);
        for yy in y..y + h {
            l.extend_from_slice(&self.l[yy * width + x..yy * width + x + w]);
        }
        SharpDetail { l, base: self.base, scale: self.scale }
    }

    /// Amount by which to sharpen further relative to the base (sharpness 0); 0 = nothing to do.
    pub fn delta(&self, slider: f32) -> f64 {
        let s = quantize(slider);
        if s == 0.0 || self.is_classic_high_pass() {
            return 0.0;
        }
        // Two f32 amounts, difference as f64 (matches SPP's rounding)
        f64::from(amount(s, self.base, self.scale) - amount(0.0, self.base, self.scale))
    }

    /// Apply the sharpness slider to one pixel of the base: `None` if there is nothing to do
    /// (sharpness 0 → bit-identical to the base), otherwise the amounts (amt(0), amt(s)) for [`resharpen_px`].
    pub fn amounts(&self, slider: f32) -> Option<(f64, f64)> {
        let s = quantize(slider);
        if x3f_core::merrill_new::active("sharpness") {
            return (s != 0.0 && !self.is_classic_high_pass()).then(|| (f64::from(own_amount(0.0, self.base, self.scale)), f64::from(own_amount(s, self.base, self.scale))));
        }
        (s != 0.0 && !self.is_classic_high_pass()).then(|| (f64::from(amount(0.0, self.base, self.scale)), f64::from(amount(s, self.base, self.scale))))
    }
}

/// Slider value as in SPP: −2 … +2 in tenths (stored as SRATIONAL in the file).
pub fn quantize(slider: f32) -> f64 {
    if !slider.is_finite() {
        return 0.0;
    }
    (f64::from(slider.clamp(-2.0, 2.0)) * 10.0).round() / 10.0
}

/// Sharpening strength as in SPP: `s ≥ 0` adds `s` to the base, `s < 0` reduces the base
/// proportionally (−2 → 0). Result as f32, matching SPP. Reduced quality levels
/// are not offered here (full quality only).
pub fn amount(s: f64, base: f32, scale: f32) -> f32 {
    if s <= -999.0 {
        return 0.0;
    }
    let base = f64::from(base);
    let d = if s < 0.0 { s / 2.0 * base } else { s };
    (base + d) as f32 * scale
}

/// Alternative amount law: `base · 2^s` (one slider step doubles or halves the sharpening, −2 → a
/// quarter, +2 → four times), times the size factor.
pub fn own_amount(s: f64, base: f32, scale: f32) -> f32 {
    (f64::from(base) * s.exp2()) as f32 * scale
}

/// Alternative sharpening step of one pixel by `delta` (amount difference to the base): the
/// luminance `Y = R⁺ + G⁺ + B⁺` changes by `d = 0.25·delta·L`, softly limited to ±35 % of `Y` with
/// `0.35·Y·tanh(d/(0.35·Y))`; the colour ratios stay.
#[inline]
pub fn own_sharpen_px(p: [f32; 3], l: i16, delta: f64) -> [f32; 3] {
    let c = p.map(|v| f64::from(v).max(0.0));
    let y = c[0] + c[1] + c[2];
    if y <= 0.0 {
        return p;
    }
    let d = 0.25 * f64::from(l) * delta;
    let lim = 0.35 * y;
    let f = (y + lim * (d / lim).tanh()) / y;
    c.map(|v| (v * f).min(32767.0) as f32)
}

/// Sharpening of one pixel, computed like
/// `residual::sharpen` in x3fuse-core: only the luminance `Y = R⁺ + G⁺ + B⁺` changes by
/// `d = 0.25·Δ·L` (at most ±1000 and ±35 % of Y), the colour ratios stay.
#[inline]
pub fn sharpen_px(p: [f32; 3], l: i16, amount: f64) -> [f32; 3] {
    let c = p.map(|v| f64::from(v).max(0.0));
    let sum = (c[0] + c[1] + c[2]) as f32;
    let chroma = if sum == 0.0 { [0.0; 3] } else { c.map(|v| v / f64::from(sum)) };
    let delta = f32::from(l) * 0.25;
    let delta = ((f64::from(delta) * amount) as f32).clamp(-1000.0, 1000.0);
    let lower = f32::from_bits(0xbeb3_3333) * sum;
    let upper = f32::from_bits(0x3eb3_3333) * sum;
    let delta = if delta > lower { delta } else { lower };
    let delta = if upper < delta { upper } else { delta };
    let lum = f64::from(sum + delta);
    chroma.map(|v| (v * lum).min(32767.0) as f32)
}

/// Same result as SPP developing with sharpness `s` (a single `Sharpen` step with amt(s) on the
/// unsharpened image), starting from the base `p` sharpened with amt(0): the
/// sharpening step of x3fuse-core is first inverted (the luminance change is known from L and
/// amt(0), only the ±35 % limit depends on the starting value), then sharpened again with amt(s).
/// Deviation from the single-step computation: rounding (a few ulp); where the base is clipped at
/// 32767, it is instead sharpened incrementally by Δ.
#[inline]
pub fn resharpen_px(p: [f32; 3], l: i16, amt0: f64, amt: f64) -> [f32; 3] {
    if p.iter().any(|&v| v >= 32767.0) {
        return sharpen_px(p, l, amt - amt0);
    }
    let c = p.map(|v| f64::from(v).max(0.0));
    let yq = c[0] + c[1] + c[2];
    if yq <= 0.0 {
        return sharpen_px(p, l, amt);
    }
    let d = f64::from(((f64::from(f32::from(l) * 0.25) * amt0) as f32).clamp(-1000.0, 1000.0));
    // Starting luminance Y with yq = Y + clamp(d, ±0.35·Y)
    let k = f64::from(f32::from_bits(0x3eb3_3333));
    let y = if d > k * (yq - d) {
        yq / (1.0 + k)
    } else if d < -k * (yq - d) {
        yq / (1.0 - k)
    } else {
        yq - d
    };
    let f = y / yq;
    sharpen_px(c.map(|v| (v * f) as f32), l, amt)
}

/// Approximation of the detail image for a downscaled preview (factor `factor`): high-pass of the
/// luminance (`Y − mean3×3(Y)`) like SPP's detail image, but without residual weighting.
/// Downscaled images are likewise sharpened on their own detail image, scaled by
/// `√(1/factor)` (as in SPP).
pub fn preview_detail(lin: &crate::Linear, base: f32, factor: usize) -> SharpDetail {
    use rayon::prelude::*;
    let (w, h) = (lin.width, lin.height);
    let y: Vec<f32> = lin.data.par_chunks(3).map(|p| (p[0].max(0.0) + p[1].max(0.0) + p[2].max(0.0)).min(32767.0)).collect();
    let mut l = vec![0i16; w * h];
    if w >= 5 && h >= 5 {
        l.par_chunks_mut(w).enumerate().skip(2).take(h - 4).for_each(|(yy, row)| {
            for x in 2..w - 2 {
                let mut sum = 0.0f32;
                for r in yy - 1..=yy + 1 {
                    sum += y[r * w + x - 1] + y[r * w + x] + y[r * w + x + 1];
                }
                let hp = (y[yy * w + x] - sum / 9.0).min(8191.0);
                row[x] = (f64::from(hp) * 4.0 + 0.5) as i64 as i16;
            }
        });
    }
    SharpDetail { l, base, scale: (1.0 / factor.max(1) as f64).sqrt() as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_curve() {
        // Base 1: −2 → 0, −1 → 0.5, 0 → 1, +1 → 2, +2 → 3
        for (s, a) in [(-2.0, 0.0), (-1.0, 0.5), (0.0, 1.0), (1.0, 2.0), (2.0, 3.0)] {
            assert_eq!(amount(s, 1.0, 1.0), a);
        }
        assert_eq!(amount(-999.0, 1.0, 1.0), 0.0);
        let d = SharpDetail { l: vec![], base: 0.75, scale: 1.0 };
        assert_eq!(d.delta(0.0), 0.0);
        assert_eq!(d.delta(0.04), 0.0);
        assert_eq!(d.delta(-2.0), -0.75);
        assert_eq!(d.delta(1.0), 1.0);
    }

    #[test]
    fn own_amount_doubles_per_step() {
        assert_eq!(own_amount(0.0, 0.8, 1.0), 0.8);
        assert!((own_amount(1.0, 0.8, 1.0) - 1.6).abs() < 1e-6);
        assert!((own_amount(-2.0, 0.8, 1.0) - 0.2).abs() < 1e-6);
        // soft limit: never beyond ±35 % of the luminance, colour ratios stay
        let q = own_sharpen_px([1000.0, 2000.0, 1000.0], 32767, 10.0);
        let y = q[0] + q[1] + q[2];
        assert!(y > 4000.0 && y <= 4000.0 * 1.35 + 0.01, "{y}");
        assert!((q[1] / q[0] - 2.0).abs() < 1e-4);
        assert_eq!(own_sharpen_px([0.0; 3], 100, 1.0), [0.0; 3]);
    }

    #[test]
    fn sharpen_keeps_ratios_and_limits() {
        let p = [1000.0, 2000.0, 1000.0];
        let q = sharpen_px(p, 400, 1.0); // d = 100
        assert!((q[0] + q[1] + q[2] - 4100.0).abs() < 0.01);
        assert!((q[1] / q[0] - 2.0).abs() < 1e-5);
        let q = sharpen_px(p, -32768, 5.0); // strongly negative → at most −35 % or −1000
        assert!((q[0] + q[1] + q[2] - 3000.0).abs() < 0.01);
        assert_eq!(sharpen_px([0.0; 3], 1000, 1.0), [0.0; 3]);
    }
}
