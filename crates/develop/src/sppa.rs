//! Take over settings saved by SIGMA Photo Pro as initial values (read only).
//!
//! [`x3f::sppa`] reads the values in SPP's units; here they are mapped to [`Settings`].
//! Whatever cannot be mapped stays at the default value.

use std::path::Path;

use x3f::sppa::SppAdjustments;

use crate::Settings;

/// SPP settings of a file as [`Settings`]; `None` if SPP saved nothing
/// (camera settings only) or the file is unreadable. Reads only a few KB.
pub fn load(path: impl AsRef<Path>) -> Option<Settings> {
    to_settings(&x3f::sppa::read_path(path)?)
}

/// Initial values without our own settings file: SPP settings ([`load`]); otherwise shots
/// for which the camera was set to black and white or sepia open in black and white like the
/// camera preview (the RAW data stays in colour, colour is one click away).
/// The sepia toning is not taken over.
pub fn baseline(path: impl AsRef<Path>) -> Option<Settings> {
    let a = x3f::sppa::read_path(path)?;
    if let Some(s) = to_settings(&a) {
        return Some(s);
    }
    if let Some(s) = camera_settings(&a) {
        return Some(s);
    }
    camera_monochrome(&a).then(|| gray_wb(&a, Settings { monochrome: true, ..Settings::default() }))
}

/// White balance "Monochrome" in the header of an X3F 2.x/3.x file (written by older SPP versions for a black and
/// white conversion; the flag bit of the monochrome mode is not set): SIGMA Photo Pro renders such a file grey in its
/// colour mode, with the calibration of white balance "Auto" and every output channel the layers mixed 1 : 2 : 1.
fn header_gray_wb(a: &SppAdjustments) -> bool {
    a.source == x3f::sppa::Source::Header && !a.monochrome && a.white_balance.as_deref() == Some("Monochrome")
}

/// [`header_gray_wb`]: our monochrome mode with the layer mix 1 : 2 : 1 (white balance stays "as shot", which falls
/// back to "Auto" for this name).
fn gray_wb(a: &SppAdjustments, mut s: Settings) -> Settings {
    if header_gray_wb(a) {
        s.monochrome = true;
        s.mix = GRAY_WB_MIX;
    }
    s
}

/// Layer mix of the white balance "Monochrome" (bottom, middle, top).
pub const GRAY_WB_MIX: [f32; 3] = [0.25, 0.5, 0.25];

/// Tone values set in the camera (header of the X3F, e.g. contrast −0.2 in the monochrome mode):
/// SIGMA Photo Pro opens unedited files with them, so they are taken over as initial values like
/// SPP's own saved values. White balance and colour mode stay "as shot"; toning, grain and vignette
/// are not taken over. `None` if the camera values are all neutral.
fn camera_settings(a: &SppAdjustments) -> Option<Settings> {
    if a.saved_by_spp || a.source != x3f::sppa::Source::Header {
        return None;
    }
    let cam = SppAdjustments {
        saved_by_spp: true,
        white_balance: None,
        wb_temperature: None,
        color_mode: None,
        toning: None,
        grain: None,
        vignette: None,
        ..a.clone()
    };
    let mut s = to_settings(&cam)?;
    s.monochrome = camera_monochrome(a);
    let s = gray_wb(a, s);
    let neutral = gray_wb(a, Settings { monochrome: s.monochrome, ..Settings::default() });
    (s != neutral).then_some(s)
}

/// Was the camera set to black and white or sepia?
fn camera_monochrome(a: &SppAdjustments) -> bool {
    a.monochrome || header_gray_wb(a) || [&a.color_mode, &a.camera_color_mode].iter().any(|m| matches!(m.as_deref(), Some("Monochrome" | "Sepia")))
}

/// Mapping to our settings. `None` if the values do not come from SPP.
pub fn to_settings(a: &SppAdjustments) -> Option<Settings> {
    if !a.saved_by_spp {
        return None;
    }
    let mut s = Settings { monochrome: a.monochrome, ..Settings::default() };
    let d = Settings::default();

    // Tone: our sliders are SPP-compatible, take the values directly.
    // SPP keeps colour and monochrome separately, we have one set → that of the active mode.
    let t = if a.monochrome { &a.mono } else { &a.color };
    let f = |v: Option<f32>, lo: f32, hi: f32| v.filter(|v| v.is_finite()).map(|v| v.clamp(lo, hi));
    s.exposure = f(t.exposure, -3.0, 3.0).map_or(d.exposure, f64::from);
    s.contrast = f(t.contrast, -2.0, 2.0).map_or(d.contrast, f64::from);
    s.shadow = f(t.shadow, -2.0, 2.0).map_or(d.shadow, f64::from);
    s.highlight = f(t.highlight, -2.0, 2.0).map_or(d.highlight, f64::from);
    s.fill_light = f(t.fill_light, -2.0, 2.0).unwrap_or(d.fill_light);
    // Sharpness: SPP −2…+2; our "Sharpness (Sigma)" slider computes like SPP (crate::sharpness).
    s.sigma_sharpness = f(t.sharpness, -2.0, 2.0).unwrap_or(d.sigma_sharpness);

    // Noise sliders: SPP stores 0…1 in quarter steps, as we do.
    let step = |v: Option<f32>, def: f64| f(v, 0.0, 1.0).map_or(def, |v| (v as f64 * 4.0).round() / 4.0);
    s.nr_luma = step(if a.monochrome { a.mono_luma_noise } else { a.luma_noise }, d.nr_luma);
    s.nr_chroma = step(a.chroma_noise, d.nr_chroma);

    // Set colour mode and white balance only if they differ from the camera setting
    // (otherwise "as shot" is kept).
    let known_modes = crate::calib::COLOR_MODES.iter().chain(crate::calib::QUATTRO_EXTRA_MODES.iter());
    let known_modes: Vec<&str> = known_modes.copied().collect();
    s.color_mode = a
        .color_mode
        .clone()
        .filter(|m| known_modes.contains(&m.as_str()))
        .filter(|m| a.camera_color_mode.as_deref() != Some(m.as_str()));
    const WB: [&str; 9] = ["Auto", "Sunlight", "Shade", "Overcast", "Incandescent", "Fluorescent", "Flash", "Custom", "AutoLSP"];
    s.white_balance = a
        .white_balance
        .clone()
        .and_then(|w| match (w.as_str(), a.wb_temperature) {
            // colour temperature (Quattro): preset at the saved kelvin (`calib::read_quattro` provides it)
            ("ColorTemp", Some(k)) if k.is_finite() && k > 0.0 => Some(crate::quattro_wb::preset_name(k)),
            _ => WB.contains(&w.as_str()).then_some(w),
        })
        .filter(|w| a.camera_white_balance.as_deref() != Some(w.as_str()));

    if !a.monochrome {
        // CA correction saved by SPP (Merrill header; the cameras themselves write "off").
        // SPP applies it in colour only, as our base does (`Settings::ca_key`).
        match a.ca_switch {
            Some(1) => s.ca_mode = crate::lens::CaMode::Manual,
            Some(2) => s.ca_mode = crate::lens::CaMode::Profile,
            _ => {}
        }
        if let Some(v) = f(a.ca_profile, 0.0, 2.0) {
            s.ca_profile = v;
        }
        if let Some(v) = f(a.ca_red, -2.0, 2.0) {
            s.ca_red = v;
        }
        if let Some(v) = f(a.ca_blue, -2.0, 2.0) {
            s.ca_blue = v;
        }
        // SPP fringe correction (green/magenta hue band), only as saved by SPP
        let band = |b: Option<(f32, f32, f32)>| b.map(|(hue, range, amount)| crate::spp_defringe::Band { hue, range, amount });
        s.spp_defringe = crate::spp_defringe::SppDefringe { green: band(a.defringe_green), magenta: band(a.defringe_magenta) };
        // Saturation: SPP factor 1 + sat/2, ours 1 + saturation
        if let Some(v) = f(a.saturation, -2.0, 2.0) {
            s.saturation = (v / 2.0).clamp(-1.0, 1.0);
        }
        // Colour adjustment (gain R/G/B in the output space) → temperature/tint, inverse of
        // `calib::temp_tint_gains`
        if let Some([r, g, b]) = a.color_adjust.filter(|c| c.iter().all(|v| v.is_finite() && *v > 0.0)) {
            let (r, g, b) = ((r as f64).ln(), (g as f64).ln(), (b as f64).ln());
            s.wb_temp = (((r - b) / 2.0) / 0.22).clamp(-1.0, 1.0) as f32;
            s.wb_tint = (((r + b - 2.0 * g) / 6.0) / 0.12).clamp(-1.0, 1.0) as f32;
        }
    }

    // Layer mix (monochrome), SPP sum 1 like our defaults
    // (rounded to 0.001: SPP stores 0.33 as 10813/32768)
    if let Some(m) = a.mix.filter(|m| m.iter().all(|v| v.is_finite() && *v >= 0.0) && m.iter().sum::<f32>() > 0.01) {
        s.mix = m.map(|v| (v * 1000.0).round() / 1000.0);
    }

    if a.monochrome {
        if let Some(t) = a.toning {
            if let Some((hue, sat)) = toning_to_grade(t) {
                s.grade_shadows = [hue, sat];
                s.grade_midtones = [hue, sat];
                s.grade_highlights = [hue, sat];
            }
        }
        // Grain and vignette only act in monochrome mode in SPP
        if a.grain == Some(true) {
            s.grain_amount = 0.35;
            if let Some(size) = f(a.grain_size, 0.1, 1.0) {
                s.grain_size = 0.5 + (size - 0.1) / 0.9 * 2.0;
            }
            if let Some(r) = f(a.grain_roughness, 0.0, 1.0) {
                s.grain_rough = r;
            }
        }
        if a.vignette == Some(true) {
            if let Some((amount, mid)) = vignette_fit(a.vignette_amount.unwrap_or(0.5), a.vignette_midpoint.unwrap_or(0.5)) {
                s.vignette = amount;
                s.vignette_mid = mid;
            }
        }
    }
    // colour sliders of SPP (its colour mode), shown grey
    Some(gray_wb(a, s))
}

/// SPP toning (constant Cb/Cr offset on the gamma-encoded grey) →
/// hue and strength of our color grading (the same in shadows, midtones and highlights, where it is
/// also an offset `hue_offset · strength · 0.35`). `None` for neutral toning.
fn toning_to_grade(g: [f32; 3]) -> Option<(f32, f32)> {
    let [r, gg, b] = g.map(|v| v as f64);
    let cb = -0.169 * r - 0.331 * gg + 0.5 * b;
    let cr = 0.5 * r - 0.419 * gg - 0.081 * b;
    // 8 bit: c = 128 of 255 → factor ≈ 0.5 in the range 0…1
    let k = 128.0 / 255.0;
    let o = [
        k * (-0.0009267 * cb + 1.401686 * cr),
        k * (-0.3436 * cb - 0.71416 * cr),
        k * (1.7721 * cb + 0.0009902 * cr),
    ];
    let len = o.iter().map(|v| v * v).sum::<f64>().sqrt();
    if len < 1e-3 {
        return None;
    }
    let mut best = (0.0f32, f64::MIN, 0.0f64);
    for i in 0..720 {
        let h = i as f32 * 0.5;
        let v = crate::look::hue_offset(h).map(|c| c as f64 * 0.35);
        let vv: f64 = v.iter().map(|c| c * c).sum();
        let ov: f64 = (0..3).map(|c| o[c] * v[c]).sum();
        let score = ov / vv.sqrt();
        if score > best.1 {
            best = (h, score, ov / vv);
        }
    }
    Some((best.0, best.2.clamp(0.0, 1.0) as f32))
}

/// SPP vignette `F = exp(−0.8·(d·A)^p)` (A = strength, p = 2 + 10·midpoint, d = distance to the
/// centre, corner = 1) → our vignette (strength −1…0, midpoint 0…1) via
/// least-squares fit. `None` if SPP does not darken anything.
fn vignette_fit(amount: f32, midpoint: f32) -> Option<(f32, f32)> {
    let a = ((amount.clamp(0.0, 1.0) * 10.0).round() / 10.0) as f64;
    let p = 2.0 + (midpoint.clamp(0.0, 1.0) * 10.0).round() as f64;
    if a <= 0.0 {
        return None;
    }
    let spp = |d: f64| (-0.8 * (d * a).powf(p)).exp();
    let smooth = |e0: f64, e1: f64, x: f64| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let samples: Vec<f64> = (1..=50).map(|i| i as f64 / 50.0).collect();
    let mut best: Option<(f64, f64, f64)> = None; // (error, strength, midpoint)
    for m in 0..=100 {
        let mid = m as f64 / 100.0;
        let (mut num, mut den) = (0.0, 0.0);
        for &d in &samples {
            let sm = smooth(mid * 0.8, 1.05, d);
            num += d * (spp(d) - 1.0) * sm;
            den += d * sm * sm;
        }
        if den <= 0.0 {
            continue;
        }
        let v = (num / den).clamp(-1.0, 0.0);
        let err: f64 = samples.iter().map(|&d| d * (1.0 + v * smooth(mid * 0.8, 1.05, d) - spp(d)).powi(2)).sum();
        if best.is_none_or(|b| err < b.0) {
            best = Some((err, v, mid));
        }
    }
    let (_, v, mid) = best?;
    (v < -0.005).then_some((v as f32, mid as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sepia_is_orange() {
        let (h, s) = toning_to_grade([1.0, 0.8833, 0.74]).unwrap();
        assert!((20.0..45.0).contains(&h), "Farbton {h}");
        assert!(s > 0.2 && s < 0.6, "strength {s}");
        assert!(toning_to_grade([1.0, 1.0, 1.0]).is_none());
        // Blue (preset "Blue") must end up in the blue range
        let (h, _) = toning_to_grade([0.7518, 0.8606, 1.0]).unwrap();
        assert!((190.0..250.0).contains(&h), "Farbton {h}");
    }

    /// Real files (only with `GRAINEO_TEST_SD10_DIR`): header white balance "Monochrome" opens grey (1 : 2 : 1),
    /// "Sunlight"/"Auto" in colour.
    #[test]
    fn header_monochrome_white_balance_opens_grey() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_SD10_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_SD10_DIR not set, skipped");
            return;
        };
        let p = dir.join("IMG02675.X3F");
        if p.is_file() {
            let s = baseline(&p).expect("initial values");
            assert!(s.monochrome);
            assert_eq!(s.mix, GRAY_WB_MIX);
        }
        for name in ["IMG00237.X3F", "IMG00266.X3F"] {
            let p = dir.join(name);
            if p.is_file() {
                assert!(baseline(&p).is_none_or(|s| !s.monochrome), "{name}");
            }
        }
    }

    #[test]
    fn vignette_darkens() {
        let (v, m) = vignette_fit(1.0, 0.0).unwrap();
        assert!(v < -0.3 && (0.0..=1.0).contains(&m));
        assert!(vignette_fit(0.0, 0.5).is_none());
        // Default (0.5/0.5): corner exp(−0.8·0.5⁷) ≈ 0.994 → practically no vignette
        assert!(vignette_fit(0.5, 0.5).is_none_or(|(v, _)| v > -0.05));
    }
}
