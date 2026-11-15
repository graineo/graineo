//! Profile amount.
//!
//! The profile is either a Sigma colour mode ([`Settings::color_mode`]) or a film
//! ([`Settings::film`]). [`Settings::profile_amount`] blends continuously between a neutral
//! reference (0) and the profile (1):
//!
//! - Sigma colour mode: the reference is Sigma's "Standard", the mode that SPP and the camera
//!   fall back to and from which the other modes (matrix, contrast offset, tone curve) deviate.
//!   If the effective mode is itself "Standard" or black and white is selected, the slider has no effect.
//!   With the SPP base the reference comes from the same base, converted linearly to "Standard" (like
//!   the preview when switching colour modes): matrix and tone curve exact, Sigma's ColorDQ and the
//!   noise reduction stay those of the selected mode; no new base (seconds) is needed.
//! - Film: the reference is Sigma's output with the current colour mode, exactly the image shown
//!   with the film switched off (the film builds on the same colour matrix). So
//!   "0 % = film off" holds, and colour mode and film remain separate decisions.
//!
//! Blending happens at the end of the output stage (after Sigma's tone curve or the film, before the
//! look stage), linearly in the sRGB-encoded 16-bit values: the encoding is approximately
//! perceptually uniform, so 50 % is also visibly in the middle (contrast, colour), and no
//! decoding and re-encoding is needed. Film grain is attenuated as well but stays
//! fixed in place; grain of the look stage acts unchanged afterwards.
//!
//! At 1 (default) nothing extra is computed; at 0 only the reference.

use rayon::prelude::*;

use crate::{Calibration, Rgb16, Settings, View};

/// Name of Sigma's neutral colour mode.
pub const NEUTRAL_MODE: &str = "Standard";

/// Does the amount slider act for these settings (film active or colour mode ≠ Standard)?
/// Black and white without film is a separate profile without colour mode: there it has no effect.
pub fn adjustable(_cal: &Calibration, s: &Settings) -> bool {
    // Films only: Sigma's colour modes are camera profiles, without an amount slider as in common photo apps.
    // A value set earlier for a Sigma mode thus does not keep acting invisibly.
    s.view == View::Developed && s.film.is_active()
}

/// Like [`adjustable`], but from the names only (UI): `camera_mode` is the shot colour mode.
pub fn adjustable_names(s: &Settings, _camera_mode: &str) -> bool {
    s.film.is_active()
}

/// Settings of the neutral reference, if blending is needed (amount < 1 and effective).
pub fn neutral(cal: &Calibration, s: &Settings) -> Option<Settings> {
    if !(s.profile_amount < 1.0) || !adjustable(cal, s) {
        return None;
    }
    let mut n = s.clone();
    n.profile_amount = 1.0;
    if s.film.is_active() {
        n.film.active = false;
    } else {
        n.color_mode = Some(NEUTRAL_MODE.to_string());
    }
    Some(n)
}

/// Output with amount: `render` returns the image for given settings (without the look stage);
/// `neutral` is the result of [`neutral`]. Without a reference exactly one call with `s`.
pub fn mix(s: &Settings, neutral: Option<&Settings>, mut render: impl FnMut(&Settings) -> Rgb16) -> Rgb16 {
    let Some(n) = neutral else {
        return render(s);
    };
    let a = s.profile_amount.clamp(0.0, 1.0);
    if a <= 0.0 {
        return render(n);
    }
    let mut img = render(s);
    let base = render(n);
    blend(&mut img, &base, a);
    img
}

/// [`mix`] for extended floating-point output (wide-gamut export).
pub fn mix_f32(s: &Settings, neutral: Option<&Settings>, mut render: impl FnMut(&Settings) -> crate::Linear) -> crate::Linear {
    let Some(n) = neutral else {
        return render(s);
    };
    let a = s.profile_amount.clamp(0.0, 1.0);
    if a <= 0.0 {
        return render(n);
    }
    let mut img = render(s);
    let base = render(n);
    if img.data.len() == base.data.len() {
        img.data.par_iter_mut().zip(base.data.par_iter()).for_each(|(o, &b)| *o = b + a * (*o - b));
    }
    img
}

/// `img` ← `base` + `a` · (`img` − `base`) in the encoded values.
pub fn blend(img: &mut Rgb16, base: &Rgb16, a: f32) {
    if img.width != base.width || img.height != base.height {
        return;
    }
    img.data.par_chunks_mut(1 << 14).zip(base.data.par_chunks(1 << 14)).for_each(|(o, b)| {
        for (o, &b) in o.iter_mut().zip(b) {
            let (p, q) = (*o as f32, b as f32);
            *o = (q + a * (p - q) + 0.5) as u16;
        }
    });
}
