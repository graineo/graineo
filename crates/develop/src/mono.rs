//! Black and white matching Sigma's monochrome output (Merrill).
//!
//! The grey base is computed by x3fuse-core (`spp_mono_working_image`, after x3fuse-core PR #34):
//! the preprocessed layers are mixed to one grey plane before any colour noise reduction (mixer
//! weighted by the white balance), the plane is
//! smoothed with a histogram bilateral filter, single-pixel extremes are clamped, the result is
//! multiplied by the ISO factor and sharpened with a high-pass of the grey plane at the base amount
//! 1.25 (sharpness slider 0). That matches Sigma's monochrome exports within rounding.
//!
//! The mixer is part of the base key ([`crate::SppKey::mono_mix`]); the sharpness slider is applied
//! here from the high-pass plane kept with the base.

use crate::{Linear, Settings};

/// Base amount of the grey image sharpening at sharpness 0.
pub const SHARPEN_BASE: f32 = 1.25;

/// Mixer of the base key: normalised mix in thousandths.
pub fn key_mix(mix: [f32; 3]) -> [u16; 3] {
    mix.map(|m| (m.clamp(0.0, 1.0) * 1000.0).round() as u16)
}

/// Grey base from x3fuse-core's monochrome working image: grey counts (ISO factor applied,
/// sharpened at [`SHARPEN_BASE`]) in all three channels, marked as monochrome base
/// ([`Linear::mono`]), with the high-pass plane for the sharpness slider.
pub(crate) fn base(w: x3f_core::SppWorking) -> Linear {
    let detail = crate::sharpness::SharpDetail { l: w.sharp_l, base: w.sharpen_base, scale: 1.0 };
    Linear { mono: true, width: w.width, height: w.height, data: w.data, detail: Some(std::sync::Arc::new(detail)), haze: None, toneq: None }
}

/// Grey image in counts (one value per pixel) of a monochrome base `lin` (full image, crop or
/// downscaled preview) for the settings `s`: the stored grey value, changed by the sharpness
/// slider relative to sharpness 0 (`Δ = L/4 · (amount(s) − amount(0))`, clamped to 0…32767 as
/// Sigma's sharpening of the grey image).
pub(crate) fn grey(lin: &Linear, s: &Settings) -> Vec<f32> {
    let mut l: Vec<f32> = lin.data.chunks_exact(3).map(|p| p[0]).collect();
    let Some(d) = lin.detail.as_deref().filter(|d| d.l.len() == l.len()) else {
        return l;
    };
    let Some((amt0, amt)) = d.amounts(s.sigma_sharpness) else {
        return l;
    };
    // Downscaled previews carry the detail of the downscaled image (base amount scaled with it)
    for (v, &hp) in l.iter_mut().zip(&d.l) {
        let q = f64::from(f32::from(hp) * 0.25);
        let delta = (q * amt - q * amt0) as f32;
        *v = (*v + delta).clamp(0.0, 32767.0);
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_follows_sharpness_slider() {
        let detail = crate::sharpness::SharpDetail { l: vec![0, 400, -400, 0], base: SHARPEN_BASE, scale: 1.0 };
        let lin = Linear { mono: true, width: 4, height: 1, data: vec![10.0, 10.0, 10.0, 500.0, 500.0, 500.0, 50.0, 50.0, 50.0, 32767.0, 32767.0, 32767.0], detail: Some(std::sync::Arc::new(detail)), haze: None, toneq: None };
        let s0 = Settings::default();
        assert_eq!(grey(&lin, &s0), vec![10.0, 500.0, 50.0, 32767.0]);
        let s1 = Settings { sigma_sharpness: 1.0, ..Settings::default() };
        let g = grey(&lin, &s1);
        assert_eq!(g[0], 10.0);
        assert!((g[1] - 600.0).abs() < 1e-3);
        assert_eq!(g[2], 0.0);
        assert_eq!(g[3], 32767.0);
        assert_eq!(key_mix([0.33, 0.33, 0.34]), [330, 330, 340]);
    }
}
