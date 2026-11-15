//! Gentle colour care in the default look ("Tuned") of the colour films: green and dark blue.
//!
//! **Green:** with most films the spectral film computation lets grass and foliage drift slightly towards yellow
//! (own measurement). The tuned look therefore rotates the hue of yellows and greens
//! by a few degrees towards pure green, by a different amount per film (a quarter of the distance to the
//! reference looks, at most 8°).
//! **Blue:** dark blues lose saturation with the Kodak negatives and with Provia, Ektachrome and Kodachrome
//! and drift towards cyan. The tuned look raises their saturation slightly (at most +12 %) and rotates the
//! hue back by up to 5° towards blue-violet.
//!
//! Brightness stays; greys, skin, red, orange and black and white stay untouched (weighting via hue,
//! chroma and brightness). The "Green" and "Blue" sliders (−1 … +1, 0 = the film's default) shift the
//! values in both directions and also act in "Datasheet" mode (from 0 there); without sliders
//! "Datasheet" computes plainly.

use rayon::prelude::*;

/// Strength of the colour care: rotation of the green hue (degrees, positive = towards green/cyan), chroma gain of dark
/// blues (fraction, 0.1 = +10 %) and rotation of their hue (degrees, positive = towards violet).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorTune {
    pub green_deg: f32,
    pub blue_gain: f32,
    pub blue_deg: f32,
}

impl ColorTune {
    pub fn is_off(&self) -> bool {
        self.green_deg.abs() < 0.05 && self.blue_gain.abs() < 0.002 && self.blue_deg.abs() < 0.05
    }
}

/// Rotation of the green hue in degrees (positive = towards green/cyan) for a film in the tuned look.
pub fn green_for(stock: &str) -> f32 {
    match stock {
        // Kodak negatives: yellow-green, reference look clearly greener
        "kodak_portra_400" | "kodak_portra_800" | "kodak_ektar_100" => 8.0,
        "kodak_portra_160" | "kodak_portra_800_push1" => 6.0,
        "kodak_portra_800_push2" => 3.0,
        // Gold and Ultramax: reference look not greener, only a touch
        "kodak_gold_200" | "kodak_gold_800" | "kodak_ultramax_400" => 2.0,
        // Fuji negatives: actually rather cool green
        "fujifilm_pro_400h" => 8.0,
        "fujifilm_c200" | "fujifilm_xtra_400" | "fujifilm_superia_200" => 7.0,
        // slide
        "fujifilm_velvia_100" | "kodak_ektachrome_100" => 6.0,
        "kodak_kodachrome_64" | "kodak_kodachrome_25" => 7.0,
        "fujifilm_provia_100f" | "kodak_ektachrome_e200" => 5.0,
        // cinema
        "kodak_vision3_200t" | "kodak_vision3_500t" => 6.0,
        "kodak_vision3_250d" | "kodak_verita_200d" => 4.0,
        "kodak_vision3_50d" => 3.0,
        _ => 0.0,
    }
}

/// Blue care of a film in the tuned look: (chroma gain, rotation in degrees).
pub fn blue_for(stock: &str) -> (f32, f32) {
    match stock {
        "kodak_portra_160" => (0.08, 5.0),
        "kodak_portra_400" | "kodak_portra_800" | "kodak_ektar_100" => (0.06, 4.0),
        "kodak_portra_800_push1" => (0.05, 3.0),
        "kodak_portra_800_push2" => (0.03, 2.0),
        "kodak_gold_200" | "kodak_gold_800" => (0.05, 3.0),
        "kodak_ultramax_400" => (0.04, 3.0),
        "fujifilm_pro_400h" => (0.02, 1.0),
        "fujifilm_velvia_100" => (0.03, 0.0),
        "fujifilm_provia_100f" => (0.12, 3.0),
        "kodak_ektachrome_100" => (0.10, 4.0),
        "kodak_ektachrome_e200" => (0.08, 3.0),
        "kodak_kodachrome_64" => (0.12, 5.0),
        "kodak_kodachrome_25" => (0.10, 4.0),
        "kodak_vision3_200t" | "kodak_vision3_500t" => (0.05, 3.0),
        "kodak_vision3_250d" | "kodak_verita_200d" | "kodak_vision3_50d" => (0.04, 2.0),
        _ => (0.0, 0.0),
    }
}

/// Care for a film: default of the tuned look (not in "Datasheet" mode) plus sliders `green`/`blue` (−1 … +1).
pub fn tune_for(stock: &str, datasheet: bool, green: f32, blue: f32) -> ColorTune {
    let (g0, (bc0, bh0)) = if datasheet { (0.0, (0.0, 0.0)) } else { (green_for(stock), blue_for(stock)) };
    let (green, blue) = (green.clamp(-1.0, 1.0), blue.clamp(-1.0, 1.0));
    ColorTune {
        green_deg: g0 + green * 8.0,
        blue_gain: (bc0 + blue * 0.15).clamp(-0.3, 0.3),
        blue_deg: (bh0 + blue * 4.0).clamp(-10.0, 10.0),
    }
}

const D65_WHITE: [f32; 3] = [0.950_47, 1.0, 1.088_83];

fn decode_srgb(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn encode_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn f_lab(t: f32) -> f32 {
    if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 }
}

fn f_lab_um(f: f32) -> f32 {
    let f3 = f * f * f;
    if f3 > 216.0 / 24389.0 { f3 } else { (116.0 * f - 16.0) * 27.0 / 24389.0 }
}

/// sRGB (encoded, 0…1) → Lab (D65).
pub fn lab_from_srgb(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (decode_srgb(rgb[0]), decode_srgb(rgb[1]), decode_srgb(rgb[2]));
    let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / D65_WHITE[0];
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = (0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b) / D65_WHITE[2];
    let (fx, fy, fz) = (f_lab(x), f_lab(y), f_lab(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Lab (D65) → sRGB (encoded, clamped to 0…1).
pub fn srgb_from_lab(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + lab[1] / 500.0, fy - lab[2] / 200.0);
    let (x, y, z) = (f_lab_um(fx) * D65_WHITE[0], f_lab_um(fy), f_lab_um(fz) * D65_WHITE[2]);
    let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let g = -0.969_266_0 * x + 1.876_010_8 * y + 0.041_556_0 * z;
    let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
    [encode_srgb(r), encode_srgb(g), encode_srgb(b)]
}

fn smooth_ramp(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Is the Lab colour inside the sRGB gamut? (encoded components 0…1 without clamping)
fn im_gamut(lab: [f32; 3]) -> bool {
    let fy = (lab[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + lab[1] / 500.0, fy - lab[2] / 200.0);
    let (x, y, z) = (f_lab_um(fx) * D65_WHITE[0], f_lab_um(fy), f_lab_um(fz) * D65_WHITE[2]);
    let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let g = -0.969_266_0 * x + 1.876_010_8 * y + 0.041_556_0 * z;
    let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
    [r, g, b].iter().all(|v| (-0.002..=1.002).contains(v))
}

/// One pixel (encoded sRGB 0…1): `None` = unchanged.
pub fn pixel(rgb: [f32; 3], p: ColorTune) -> Option<[f32; 3]> {
    let [r, g, b] = rgb;
    let green = p.green_deg.abs() >= 0.05 && g > b + 0.02 && r <= g * 1.3;
    let blue = (p.blue_gain.abs() >= 0.002 || p.blue_deg.abs() >= 0.05) && b > r + 0.02 && b >= g * 0.95;
    if !green && !blue {
        return None;
    }
    let lab = lab_from_srgb(rgb);
    let chroma = lab[1].hypot(lab[2]);
    let h = lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0);
    let light_weight = smooth_ramp(8.0, 25.0, lab[0]) * (1.0 - smooth_ramp(92.0, 100.0, lab[0]));
    let mut dh = 0.0;
    let mut boost = 0.0;
    if green {
        let a = (h - 125.0) / 24.0;
        dh += p.green_deg * (-a * a).exp() * smooth_ramp(5.0, 22.0, chroma) * light_weight;
    }
    if blue {
        let a = (h - 275.0) / 32.0;
        let w = (-a * a).exp() * smooth_ramp(6.0, 28.0, chroma) * light_weight;
        dh += p.blue_deg * w;
        boost = p.blue_gain * w;
    }
    if dh.abs() < 0.01 && boost.abs() < 0.001 {
        return None;
    }
    let h2 = (h + dh).to_radians();
    let mut c2 = chroma * (1.0 + boost);
    // colours rotated or boosted out of the gamut: reduce chroma step by step
    for _ in 0..8 {
        if im_gamut([lab[0], c2 * h2.cos(), c2 * h2.sin()]) {
            break;
        }
        c2 *= 0.93;
    }
    Some(srgb_from_lab([lab[0], c2 * h2.cos(), c2 * h2.sin()]))
}

/// Care for the whole image (sRGB-encoded, 16 bit, R G B interleaved).
pub fn apply(data: &mut [u16], p: ColorTune) {
    if p.is_off() {
        return;
    }
    data.par_chunks_mut(3 * 8192).for_each(|piece| {
        for px in piece.chunks_exact_mut(3) {
            let rgb = [px[0] as f32 / 65535.0, px[1] as f32 / 65535.0, px[2] as f32 / 65535.0];
            if let Some(n) = pixel(rgb, p) {
                px[0] = (n[0] * 65535.0 + 0.5) as u16;
                px[1] = (n[1] * 65535.0 + 0.5) as u16;
                px[2] = (n[2] * 65535.0 + 0.5) as u16;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srgb8(r: u8, g: u8, b: u8) -> [f32; 3] {
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
    }

    fn lch(rgb: [f32; 3]) -> (f32, f32, f32) {
        let l = lab_from_srgb(rgb);
        (l[0], l[1].hypot(l[2]), l[2].atan2(l[1]).to_degrees().rem_euclid(360.0))
    }

    fn dh(a: [f32; 3], b: [f32; 3]) -> f32 {
        (lch(b).2 - lch(a).2 + 540.0) % 360.0 - 180.0
    }

    const GREEN8: ColorTune = ColorTune { green_deg: 8.0, blue_gain: 0.0, blue_deg: 0.0 };
    const BLUE: ColorTune = ColorTune { green_deg: 0.0, blue_gain: 0.12, blue_deg: 5.0 };

    #[test]
    fn lab_round_trip() {
        for rgb in [srgb8(157, 188, 64), srgb8(70, 148, 73), srgb8(10, 10, 10), srgb8(240, 240, 240), srgb8(200, 30, 90)] {
            let z = srgb_from_lab(lab_from_srgb(rgb));
            for k in 0..3 {
                assert!((z[k] - rgb[k]).abs() < 2e-3, "{rgb:?} → {z:?}");
            }
        }
    }

    #[test]
    fn yellow_green_gets_greener_lightness_and_chroma_stay() {
        let before = srgb8(120, 150, 60);
        let after = pixel(before, GREEN8).expect("is corrected");
        let (d, (lv, cv, _), (ln, cn, _)) = (dh(before, after), lch(before), lch(after));
        assert!(d > 5.0 && d <= 8.0, "Farbtondrehung {d}");
        assert!((lv - ln).abs() < 0.6 && (cv - cn).abs() < 1.0);
        // a negative angle rotates back
        let reverted = pixel(before, ColorTune { green_deg: -8.0, ..Default::default() }).unwrap();
        assert!(dh(before, reverted) < 0.0);
    }

    #[test]
    fn dark_blue_gets_more_saturated_and_violet_within_gamut() {
        for rgb in [srgb8(56, 61, 150), srgb8(80, 91, 166), srgb8(30, 40, 110), srgb8(98, 122, 157)] {
            let n = pixel(rgb, BLUE).expect("is cared for");
            let ((_, cv, hv), (_, cn, hn)) = (lch(rgb), lch(n));
            assert!(cn > cv, "{rgb:?}: Buntheit {cv} → {cn}");
            assert!(hn > hv - 0.01 || dh(rgb, n) >= 0.0, "{rgb:?}: Farbton {hv} → {hn}");
            assert!(n.iter().all(|v| (0.0..=1.0).contains(v)));
        }
        // the gain stays in the order of magnitude of the increase (not twice as saturated)
        let (v, n) = (srgb8(80, 91, 166), pixel(srgb8(80, 91, 166), BLUE).unwrap());
        assert!(lch(n).1 / lch(v).1 < 1.13);
    }

    #[test]
    fn gray_skin_red_orange_and_black_stay_untouched() {
        let both = ColorTune { green_deg: 8.0, blue_gain: 0.12, blue_deg: 5.0 };
        for rgb in [srgb8(128, 128, 128), srgb8(243, 243, 242), srgb8(52, 52, 52), srgb8(0, 0, 0), srgb8(224, 172, 138), srgb8(198, 134, 98), srgb8(200, 40, 40), srgb8(214, 126, 44), srgb8(175, 54, 60), srgb8(231, 199, 31)] {
            if let Some(n) = pixel(rgb, both) {
                // pure yellow (flowers) may follow only slightly
                assert!(dh(rgb, n).abs() < 2.5 && (lch(n).1 - lch(rgb).1).abs() < 1.5, "{rgb:?} changed: {n:?}");
            }
        }
        assert!(pixel(srgb8(128, 128, 128), both).is_none());
    }

    #[test]
    fn off_and_unknown_film_change_nothing() {
        let mut d: Vec<u16> = (0..3000u32).map(|i| (i * 21 % 65535) as u16).collect();
        let original = d.clone();
        apply(&mut d, ColorTune::default());
        assert_eq!(d, original);
        assert!(tune_for("ilford_hp5_plus", false, 0.0, 0.0).is_off());
        // no crash for arbitrary values
        let mut v: Vec<u16> = (0..3 * 5000u32).map(|i| ((i * 7919) % 65536) as u16).collect();
        apply(&mut v, ColorTune { green_deg: 16.0, blue_gain: 0.3, blue_deg: 10.0 });
    }

    #[test]
    fn tables_and_sliders_stay_in_range() {
        for f in ["kodak_portra_400", "kodak_portra_160", "kodak_gold_200", "fujifilm_pro_400h", "fujifilm_c200", "fujifilm_velvia_100", "kodak_vision3_500t", "kodak_kodachrome_64", "kodak_ektachrome_e200", "fujifilm_provia_100f"] {
            assert!((0.0..=8.0).contains(&green_for(f)), "{f}");
            let (c, h) = blue_for(f);
            assert!((0.0..=0.12).contains(&c) && (0.0..=5.0).contains(&h), "{f}: {c} {h}");
        }
        // Sliders: 0 = the film's default; Datasheet computes from 0
        assert_eq!(tune_for("kodak_portra_400", false, 0.0, 0.0), ColorTune { green_deg: 8.0, blue_gain: 0.06, blue_deg: 4.0 });
        assert!(tune_for("kodak_portra_400", true, 0.0, 0.0).is_off());
        let full_tune = tune_for("kodak_portra_400", false, 1.0, -1.0);
        assert_eq!((full_tune.green_deg, full_tune.blue_gain), (16.0, 0.06 - 0.15));
        assert_eq!(tune_for("kodak_portra_400", true, 0.5, 0.0).green_deg, 4.0);
    }
}
