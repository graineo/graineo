//! Skin tones: two global sliders that shift hue and saturation only inside a fixed skin-tone region.
//!
//! The region is a constant band around the skin-tone axis known from video engineering (the "flesh line" of the
//! vectorscope), expressed in Oklab LCh: a hue window wide enough for very light to very dark skin, a chroma
//! window that excludes greys and strongly saturated colours, and a soft fade near black and white. It is never
//! measured from the image (no colour picker, no reference sample, no skin-type classification).
//!
//! Weights are smooth (smoothstep ramps, a classic soft qualifier) and have compact support: every pixel outside
//! the region keeps its exact value – the rest of the image is not adjusted at all. With both sliders at 0 the
//! stage is skipped, so results stay bit-identical.
//!
//! JPEG sources (4:2:0 chroma, 8×8 blocks, colour noise) make the per-pixel weight jump from pixel to pixel, so the
//! weight map – never the image – is smoothed with a guided filter (He, Sun & Tang, "Guided Image Filtering", ECCV
//! 2010) guided by the brightness: the weight follows brightness edges (skin against hair, clothing, background) and
//! evens out where only the colour flickers. The filter keeps every pixel whose window holds no skin weight at all at
//! exactly 0, so pixels far from any skin tone stay exactly unchanged; only transition pixels take part of the
//! weight of their skin-tone neighbours. The radius is a fixed fraction of the image's short side, so the preview and
//! the full-size export look the same.
//!
//! Oklab: Ottosson 2020, public domain / MIT.

use crate::Settings;

/// Full-weight hue window (degrees, Oklab hue).
const HUE_LO: f32 = 42.0;
const HUE_HI: f32 = 80.0;
/// Width of the soft hue ramp on each side (degrees); weight is 0 below `HUE_LO − HUE_FADE` and above
/// `HUE_HI + HUE_FADE`.
const HUE_FADE: f32 = 20.0;
/// Chroma: fade in from grey, full weight, fade out towards strongly saturated colours.
const C_GREY: f32 = 0.0;
const C_FULL: f32 = 0.03;
const C_HIGH: f32 = 0.10;
const C_ZERO: f32 = 0.20;
/// Lightness: fade out near black and white.
const L_BLACK: f32 = 0.06;
const L_DARK: f32 = 0.18;
const L_LIGHT: f32 = 0.95;
const L_WHITE: f32 = 0.995;
/// Hue rotation at slider ±1 (degrees). Small enough that the mapping stays monotonic across the soft ramp.
const MAX_HUE_SHIFT: f32 = 10.0;
/// Relative chroma change at slider ±1 (±50 %).
const MAX_SAT: f32 = 0.5;

/// Are the skin-tone sliders active?
pub fn active(s: &Settings) -> bool {
    !s.monochrome && (s.skin_hue != 0.0 || s.skin_sat != 0.0)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn decode(x: f32) -> f32 {
    let a = x.abs();
    let v = if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    v.copysign(x)
}

fn encode(x: f32) -> f32 {
    let a = x.abs();
    let v = if a <= 0.0031308 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    v.copysign(x)
}

/// Encoded sRGB to Oklab, via the shared conversion of the masks crate.
fn to_oklab(c: [f32; 3]) -> [f32; 3] {
    masks::oklab(c.map(decode))
}

fn from_oklab(lab: [f32; 3]) -> [f32; 3] {
    let [ll, a, b] = lab;
    let l = ll + 0.396_337_78 * a + 0.215_803_76 * b;
    let m = ll - 0.105_561_346 * a - 0.063_854_17 * b;
    let s = ll - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
    .map(encode)
}

/// Weight of the fixed skin-tone region for an Oklab colour (L, chroma, hue in degrees): 0 outside, 1 in the core.
pub fn weight(l: f32, c: f32, h: f32) -> f32 {
    let h = h.rem_euclid(360.0);
    let wh = smoothstep(HUE_LO - HUE_FADE, HUE_LO, h) * (1.0 - smoothstep(HUE_HI, HUE_HI + HUE_FADE, h));
    if wh <= 0.0 {
        return 0.0;
    }
    let wc = smoothstep(C_GREY, C_FULL, c) * (1.0 - smoothstep(C_HIGH, C_ZERO, c));
    let wl = smoothstep(L_BLACK, L_DARK, l) * (1.0 - smoothstep(L_LIGHT, L_WHITE, l));
    wh * wc * wl
}

/// Radius of the weight smoothing as a fraction of the full image's short side (4000 px: 20 px, about one JPEG
/// chroma block on each side).
const SMOOTH_RADIUS: f32 = 0.005;

/// Smoothing radius in output pixels for a full image of `full_w × full_h` rendered at `scale` full-image pixels per
/// output pixel.
pub fn smooth_radius(full_w: usize, full_h: usize, scale: f32) -> usize {
    ((full_w.min(full_h) as f32 * SMOOTH_RADIUS) / scale.max(1.0)).round().max(1.0) as usize
}

/// Skin weight of one encoded sRGB colour and the guide value (encoded luminance) for the smoothing.
#[inline]
pub fn weight_and_guide(c: [f32; 3]) -> (f32, f32) {
    let g = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.5);
    let lab = to_oklab(c);
    let ch = lab[1].hypot(lab[2]);
    if ch <= 1e-6 {
        return (0.0, g);
    }
    (weight(lab[0], ch, lab[2].atan2(lab[1]).to_degrees()), g)
}

/// Edge-preserving smoothing of a weight map (row-major, `w × h`) with the guide `guide` (see the module notes).
/// Windows without any weight stay exactly 0; tiles without weight are skipped.
pub fn smooth_weights(wt: &mut [f32], guide: &[f32], w: usize, h: usize, r: usize) {
    if w < 2 || h < 2 || wt.iter().all(|&v| v == 0.0) {
        return;
    }
    masks::refine::guided(wt, guide, w, h, r);
}

/// Per-pixel weight map of an image (interleaved encoded sRGB via `color(i)`), smoothed. Pixels: `w × h`.
pub fn weight_map(w: usize, h: usize, scale_radius: usize, color: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    use rayon::prelude::*;
    let (mut wt, guide): (Vec<f32>, Vec<f32>) = (0..w * h).into_par_iter().map(|i| weight_and_guide(color(i))).unzip();
    smooth_weights(&mut wt, &guide, w, h, scale_radius);
    wt
}

/// Skin-tone adjustment with a given (smoothed) weight `w`. Weight 0 returns the colour exactly unchanged.
#[inline]
pub fn apply_weighted(c: [f32; 3], hue: f32, sat: f32, w: f32) -> [f32; 3] {
    if w <= 0.0 {
        return c;
    }
    let lab = to_oklab(c);
    let ch = lab[1].hypot(lab[2]);
    if ch <= 1e-6 {
        return c;
    }
    let h = lab[2].atan2(lab[1]).to_degrees();
    let h2 = (h + hue.clamp(-1.0, 1.0) * MAX_HUE_SHIFT * w).to_radians();
    let c2 = ch * (1.0 + sat.clamp(-1.0, 1.0) * MAX_SAT * w).max(0.0);
    from_oklab([lab[0], c2 * h2.cos(), c2 * h2.sin()])
}

/// Skin-tone adjustment of one encoded sRGB colour (extended values allowed). `hue`: −1 (rosier) … +1 (more
/// golden); `sat`: −1 … +1 relative chroma change. Colours outside the region are returned unchanged.
#[inline]
pub fn apply_pixel(c: [f32; 3], hue: f32, sat: f32) -> [f32; 3] {
    let lab = to_oklab(c);
    let ch = lab[1].hypot(lab[2]);
    if ch <= 1e-6 {
        return c;
    }
    let h = lab[2].atan2(lab[1]).to_degrees();
    let w = weight(lab[0], ch, h);
    if w <= 0.0 {
        return c;
    }
    let h2 = (h + hue.clamp(-1.0, 1.0) * MAX_HUE_SHIFT * w).to_radians();
    let c2 = ch * (1.0 + sat.clamp(-1.0, 1.0) * MAX_SAT * w).max(0.0);
    from_oklab([lab[0], c2 * h2.cos(), c2 * h2.sin()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(v: [u8; 3]) -> [f32; 3] {
        v.map(|x| x as f32 / 255.0)
    }

    fn lch(c: [f32; 3]) -> (f32, f32, f32) {
        let lab = to_oklab(c);
        (lab[0], lab[1].hypot(lab[2]), lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0))
    }

    #[test]
    fn roundtrip() {
        for c in [[0.2, 0.5, 0.9], [0.9, 0.6, 0.4], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]] {
            let b = from_oklab(to_oklab(c));
            for k in 0..3 {
                assert!((b[k] - c[k]).abs() < 1e-4, "{c:?} {b:?}");
            }
        }
    }

    #[test]
    fn skin_tones_get_full_weight() {
        // very light to very dark skin
        for v in [[255, 224, 189], [241, 194, 125], [224, 172, 105], [198, 134, 66], [141, 85, 36], [92, 56, 38], [238, 200, 175], [210, 160, 130], [115, 82, 68], [194, 150, 130]] {
            let (l, c, h) = lch(rgb(v));
            assert!(weight(l, c, h) > 0.75, "{v:?}: {}", weight(l, c, h));
        }
    }

    #[test]
    fn other_colours_untouched() {
        // greys, blue, green, cyan, magenta, saturated red: exactly unchanged
        for v in [[128, 128, 128], [250, 250, 250], [98, 122, 157], [87, 108, 67], [70, 148, 73], [8, 133, 161], [187, 86, 149], [175, 54, 60], [193, 90, 99], [56, 61, 150]] {
            let c = rgb(v);
            assert_eq!(apply_pixel(c, 1.0, 1.0), c, "{v:?}");
            assert_eq!(apply_pixel(c, -1.0, -1.0), c, "{v:?}");
        }
    }

    #[test]
    fn direction() {
        let c = rgb([210, 160, 130]);
        let (_, c0, h0) = lch(c);
        let (_, _, hg) = lch(apply_pixel(c, 1.0, 0.0));
        let (_, _, hr) = lch(apply_pixel(c, -1.0, 0.0));
        assert!(hg > h0 + 5.0 && hr < h0 - 5.0, "{hr} {h0} {hg}");
        let (_, cs, _) = lch(apply_pixel(c, 0.0, 0.5));
        assert!(cs > c0 * 1.2);
        let (_, cd, _) = lch(apply_pixel(c, 0.0, -1.0));
        assert!(cd < c0 * 0.6);
    }

    #[test]
    fn smoothing_keeps_far_pixels_and_evens_flicker() {
        // left half: flickering skin weight (JPEG-like), right half: no skin; flat brightness
        let (w, h) = (64usize, 32usize);
        let mut wt: Vec<f32> = (0..w * h).map(|i| if i % w < 32 { if (i / w + i % w) % 2 == 0 { 1.0 } else { 0.3 } } else { 0.0 }).collect();
        let guide = vec![0.5f32; w * h];
        let r = 3;
        smooth_weights(&mut wt, &guide, w, h, r);
        for y in 0..h {
            // pixels with no weight within reach (2r+1) of the skin half stay exactly 0
            for x in 32 + 2 * r + 2..w {
                assert_eq!(wt[y * w + x], 0.0, "{x},{y}");
            }
            // inside, the flicker is evened out
            for x in 2 * r + 2..32 - 2 * r - 2 {
                assert!((wt[y * w + x] - 0.65).abs() < 0.05, "{}", wt[y * w + x]);
            }
        }
        // a weight 0 never changes the colour
        let c = rgb([210, 160, 130]);
        assert_eq!(apply_weighted(c, 1.0, 1.0, 0.0), c);
        let (wc, _) = weight_and_guide(c);
        assert_eq!(apply_weighted(c, 0.7, -0.4, wc), apply_pixel(c, 0.7, -0.4));
    }

    #[test]
    fn smoothing_keeps_brightness_edges() {
        // skin (bright) next to dark hair without skin weight: the weight stays on the skin side
        let (w, h) = (64usize, 16usize);
        let mut wt: Vec<f32> = (0..w * h).map(|i| if i % w < 32 { 1.0 } else { 0.0 }).collect();
        let guide: Vec<f32> = (0..w * h).map(|i| if i % w < 32 { 0.7 } else { 0.15 }).collect();
        smooth_weights(&mut wt, &guide, w, h, 4);
        for y in 0..h {
            assert!(wt[y * w + 31] > 0.9 && wt[y * w + 32] < 0.1, "{} {}", wt[y * w + 31], wt[y * w + 32]);
        }
    }

    #[test]
    fn radius_matches_preview_and_full() {
        assert_eq!(smooth_radius(6000, 4000, 1.0), 20);
        assert_eq!(smooth_radius(6000, 4000, 3.0), 7);
        assert_eq!(smooth_radius(600, 400, 8.0), 1);
    }

    #[test]
    fn monotonic_hue_and_chroma() {
        // the mapping keeps the order of hues and chromas (no folding at the soft edges)
        for &(hue, sat) in &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (1.0, 1.0)] {
            let mut prev_h = f32::MIN;
            for i in 0..=1200 {
                let h = 0.0 + i as f32 * 0.1;
                let w = weight(0.6, 0.07, h);
                let h2 = h + hue * MAX_HUE_SHIFT * w;
                assert!(h2 >= prev_h - 1e-4, "hue fold at {h}");
                prev_h = h2;
            }
            let mut prev_c = f32::MIN;
            for i in 0..=400 {
                let c = i as f32 * 0.001;
                let w = weight(0.6, c, 60.0);
                let c2 = c * (1.0 + sat * MAX_SAT * w).max(0.0);
                assert!(c2 >= prev_c - 1e-5, "chroma fold at {c}");
                prev_c = c2;
            }
        }
    }
}
