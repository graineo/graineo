//! Scene-linear tone mapper: log-logistic sigmoid curve per channel with hue preservation.
//!
//! Origin: curve shape and parameter fitting (`_generalized_loglogistic_sigmoid`,
//! `_compute_curve`) as well as hue/energy preservation (`_preserve_hue_and_energy`,
//! `_desaturate_negative_values`) are ported from **darktable** `src/iop/sigmoid.c`
//! (master, retrieved 2026-10-27), license **GPL-3.0-or-later**,
//! Copyright (C) 2020-2026 darktable developers ("sigmoid" module by Jakob Gripenstedt).
//!
//! Changes: Rust, curve as a lookup table in log space (accuracy < 1e-4), fixed
//! parameters instead of a GUI, no primaries rework ("base primaries"), additional
//! saturation adjustment after the curve (own addition, see [`Sigmoid::saturation`]).

/// Middle gray of the scene/output as in darktable (18.45 %).
pub const MID_GRAY: f32 = 0.1845;

/// Parameters of the sigmoid curve (defaults: tuned on reference images).
#[derive(Debug, Clone, Copy)]
pub struct Sigmoid {
    /// Slope at middle gray (darktable "contrast", default there 1.5)
    pub contrast: f32,
    /// Skew: > 0 more contrast in the highlights/softer shadows (darktable "skew")
    pub skew: f32,
    /// Target white, relative (1 = output white)
    pub white: f32,
    /// Target black, relative (darktable 0.0152 % → 0.000152)
    pub black: f32,
    /// Hue preservation 0…1 (darktable default 1.0)
    pub hue: f32,
    /// Saturation factor after the curve (1 = unchanged), luminance-preserving
    pub saturation: f32,
}

impl Default for Sigmoid {
    fn default() -> Self {
        Self { contrast: 1.5, skew: 0.0, white: 1.0, black: 0.000152, hue: 1.0, saturation: 1.0 }
    }
}

#[derive(Debug, Clone, Copy)]
struct Curve {
    white: f32,
    paper_exp: f32,
    film_fog: f32,
    film_power: f32,
    paper_power: f32,
}

#[inline]
fn loglogistic(x: f32, magnitude: f32, paper_exp: f32, film_fog: f32, film_power: f32, paper_power: f32) -> f32 {
    let v = x.max(0.0);
    let film = (film_fog + v).powf(film_power);
    let paper = magnitude * (film / (paper_exp + film)).powf(paper_power);
    if paper.is_nan() {
        magnitude
    } else {
        paper
    }
}

fn curve(p: &Sigmoid) -> Curve {
    let g = MID_GRAY;
    let ref_film_power = p.contrast;
    let ref_paper_exposure = g.powf(ref_film_power) * (1.0 / g - 1.0);
    let delta = 1e-4f32;
    let ref_slope = (loglogistic(g + delta, 1.0, ref_paper_exposure, 0.0, ref_film_power, 1.0) - loglogistic(g - delta, 1.0, ref_paper_exposure, 0.0, ref_film_power, 1.0)) / 2.0 / delta;
    let paper_power = 5f32.powf(-p.skew);
    let wt = p.white;
    let temp_rel = (wt / g).powf(1.0 / paper_power) - 1.0;
    let temp_pe = g * temp_rel;
    let temp_slope = (loglogistic(g + delta, wt, temp_pe, 0.0, 1.0, paper_power) - loglogistic(g - delta, wt, temp_pe, 0.0, 1.0, paper_power)) / 2.0 / delta;
    let film_power = ref_slope / temp_slope;
    let wg = (wt / g).powf(1.0 / paper_power) - 1.0;
    let wb = (p.black / wt).powf(-1.0 / paper_power) - 1.0;
    let film_fog = g * wg.powf(1.0 / film_power) / (wb.powf(1.0 / film_power) - wg.powf(1.0 / film_power));
    let paper_exp = (film_fog + g).powf(film_power) * wg;
    Curve { white: wt, paper_exp, film_fog, film_power, paper_power }
}

/// Tabulated curve over log2(x) from 2^-16 to 2^+8.
pub struct ToneCurve {
    lut: Vec<f32>,
    p: Sigmoid,
}

const LOG_MIN: f32 = -16.0;
const LOG_MAX: f32 = 8.0;
const N: usize = 8192;

impl ToneCurve {
    pub fn new(p: Sigmoid) -> Self {
        let k = curve(&p);
        let lut = (0..=N)
            .map(|i| {
                let l = LOG_MIN + (LOG_MAX - LOG_MIN) * i as f32 / N as f32;
                loglogistic(l.exp2(), k.white, k.paper_exp, k.film_fog, k.film_power, k.paper_power)
            })
            .collect();
        Self { lut, p }
    }

    /// Table of the curve (8193 values over log2(x) from −16 to +8), for the GPU.
    pub fn table(&self) -> &[f32] {
        &self.lut
    }

    pub fn parameter(&self) -> Sigmoid {
        self.p
    }

    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return self.lut[0] * (x.max(0.0) / LOG_MIN.exp2()).min(1.0);
        }
        let l = x.log2();
        if l <= LOG_MIN {
            return self.lut[0] * x / LOG_MIN.exp2();
        }
        let t = ((l - LOG_MIN) / (LOG_MAX - LOG_MIN) * N as f32).min(N as f32 - 1e-3);
        let i = t as usize;
        let f = t - i as f32;
        self.lut[i] * (1.0 - f) + self.lut[i + 1] * f
    }

    /// Map per channel, preserve hue (darktable "per channel" with hue preservation),
    /// then scale saturation around the luminance `lum` (weights of the working space).
    #[inline]
    pub fn pixel(&self, pin: [f32; 3], lum: [f32; 3]) -> [f32; 3] {
        let p = desaturate_negative(pin);
        let per = [self.eval(p[0]), self.eval(p[1]), self.eval(p[2])];
        let order = channel_order(&p);
        let mut o = preserve_hue_and_energy(&p, &per, order, self.p.hue);
        if self.p.saturation != 1.0 {
            let y = lum[0] * o[0] + lum[1] * o[1] + lum[2] * o[2];
            for v in o.iter_mut() {
                *v = y + self.p.saturation * (*v - y);
            }
        }
        o
    }
}

impl ToneCurve {
    /// Scene value (one channel) at which the curve reaches `level` of its white (display linear), found by bisection.
    /// Used as "white" for the highlight roll of a lowered Whites slider ([`whites_roll_pull`]).
    pub fn scene_white(&self, level: f32) -> f32 {
        let target = level * self.p.white;
        let (mut lo, mut hi) = (MID_GRAY, 256.0f32);
        if self.eval(hi) < target {
            return hi;
        }
        for _ in 0..40 {
            let m = (lo * hi).sqrt();
            if self.eval(m) < target {
                lo = m;
            } else {
                hi = m;
            }
        }
        hi
    }
}

/// Display level (linear, fraction of white) that counts as "white" for [`whites_roll_pull`]: 0.995 encodes to about 254.4 of 255.
pub const ROLL_WHITE_LEVEL: f32 = 0.995;

/// Stops over white that a Whites slider at −1 brings back under white (the roll's reach at full strength).
pub const ROLL_REACH_STOPS: f32 = 2.0;

/// Reach of the roll in stops for a Whites slider value `whites` (−1 … +1). Zero for `whites >= 0`; grows like `d³/(d²+0.1²)`
/// so it starts with zero value and zero slope (no jump when the slider leaves 0).
pub fn whites_roll_reach(whites: f32) -> f32 {
    let d = (-whites).clamp(0.0, 1.0);
    if d == 0.0 {
        return 0.0;
    }
    ROLL_REACH_STOPS * d * d * d / (d * d + 0.01)
}

/// Highlight roll of a lowered Whites slider (idea from greycard's "Whites down reaches over white"; own implementation).
/// `t` = stops of the pixel's brightest channel relative to white ([`ToneCurve::scene_white`]), `reach` from
/// [`whites_roll_reach`]. Returns the pull in stops (≥ 0); the caller applies one gain `2^-pull` to all three channels, so the
/// hue stays. Shape: identity far below white; a band of width `reach·s/(1−s)` under white is compressed to slope `s` so that
/// the light at `reach` stops over white lands on white; past the reach the slope is one again (pull = reach). The corners
/// are rounded quadratically (width 2·`ROLL_CORNER`), so the mapping is C¹ and strictly increasing (slope ≥ `s`).
#[inline]
pub fn whites_roll_pull(t: f32, reach: f32) -> f32 {
    const S: f32 = 0.2;
    const W: f32 = 0.1;
    if reach <= 0.0 || !t.is_finite() {
        return 0.0;
    }
    let knee = reach * S / (1.0 - S);
    let ramp = |z: f32| {
        if z <= 0.0 {
            0.0
        } else if z < 2.0 * W {
            z * z / (4.0 * W)
        } else {
            z - W
        }
    };
    (1.0 - S) * (ramp(t + knee + W) - ramp(t - reach + W))
}

/// Applies [`whites_roll_pull`] to a scene-linear pixel: one gain on all channels; `white` = scene white of the curve.
#[inline]
pub fn whites_roll(v: [f32; 3], reach: f32, white: f32) -> [f32; 3] {
    let mx = v[0].max(v[1]).max(v[2]);
    if reach <= 0.0 || mx <= 0.0 || !mx.is_finite() {
        return v;
    }
    let pull = whites_roll_pull((mx / white).log2(), reach);
    if pull <= 0.0 {
        return v;
    }
    let g = (-pull).exp2();
    [v[0] * g, v[1] * g, v[2] * g]
}

fn desaturate_negative(p: [f32; 3]) -> [f32; 3] {
    let avg = ((p[0] + p[1] + p[2]) / 3.0).max(0.0);
    let min = p[0].min(p[1]).min(p[2]);
    let f = if min < 0.0 { -avg / (min - avg) } else { 1.0 };
    [avg + f * (p[0] - avg), avg + f * (p[1] - avg), avg + f * (p[2] - avg)]
}

/// (min, mid, max)
fn channel_order(p: &[f32; 3]) -> (usize, usize, usize) {
    if p[0] >= p[1] {
        if p[1] > p[2] {
            (2, 1, 0)
        } else if p[2] > p[0] {
            (1, 0, 2)
        } else if p[2] > p[1] {
            (1, 2, 0)
        } else {
            (2, 1, 0)
        }
    } else if p[0] >= p[2] {
        (2, 0, 1)
    } else if p[2] > p[1] {
        (0, 1, 2)
    } else {
        (0, 2, 1)
    }
}

fn preserve_hue_and_energy(pin: &[f32; 3], per: &[f32; 3], (mn, md, mx): (usize, usize, usize), hp: f32) -> [f32; 3] {
    let chroma = pin[mx] - pin[mn];
    let midscale = if chroma != 0.0 { (pin[md] - pin[mn]) / chroma } else { 0.0 };
    let full = per[mn] + (per[mx] - per[mn]) * midscale;
    let naive_mid = (1.0 - hp) * per[md] + hp * full;
    let per_energy = per[0] + per[1] + per[2];
    let naive_energy = per[mn] + naive_mid + per[mx];
    let mm = pin[mn] + pin[md];
    let blend = if mm != 0.0 { 2.0 * pin[mn] / mm } else { 0.0 };
    let target = blend * per_energy + (1.0 - blend) * naive_energy;
    let mut o = [0f32; 3];
    if naive_mid <= per[md] {
        let cm = ((1.0 - hp) * per[md] + hp * (midscale * per[mx] + (1.0 - midscale) * (target - per[mx]))) / (1.0 + hp * (1.0 - midscale));
        o[mn] = target - per[mx] - cm;
        o[md] = cm;
        o[mx] = per[mx];
    } else {
        let cm = ((1.0 - hp) * per[md] + hp * (per[mn] * (1.0 - midscale) + midscale * (target - per[mn]))) / (1.0 + hp * midscale);
        o[mn] = per[mn];
        o[md] = cm;
        o[mx] = target - per[mn] - cm;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mid_gray_stays() {
        let t = ToneCurve::new(Sigmoid::default());
        assert!((t.eval(MID_GRAY) - MID_GRAY).abs() < 2e-3, "{}", t.eval(MID_GRAY));
        assert!(t.eval(100.0) <= 1.0 && t.eval(100.0) > 0.98);
    }

    #[test]
    fn whites_roll_zero_is_identity_and_lands_reach_on_white() {
        assert_eq!(whites_roll_reach(0.0), 0.0);
        assert_eq!(whites_roll_reach(0.3), 0.0);
        let v = [3.0, 2.0, 1.0];
        assert_eq!(whites_roll(v, 0.0, 1.0), v);
        let r = whites_roll_reach(-1.0);
        assert!((r - 2.0 * 1.0 / 1.01).abs() < 1e-5);
        // far below the band: untouched to the bit
        assert_eq!(whites_roll_pull(-2.0, r), 0.0);
        // the light at the reach lands within 0.03 stop of white
        let at = r - whites_roll_pull(r, r);
        assert!(at.abs() < 0.03, "{at}");
        // past the reach: pull = reach
        assert!((whites_roll_pull(r + 3.0, r) - r).abs() < 1e-5);
    }

    #[test]
    fn whites_roll_is_monotone_and_keeps_hue() {
        for &w in &[-0.05f32, -0.3, -0.7, -1.0] {
            let r = whites_roll_reach(w);
            let mut last = f32::NEG_INFINITY;
            let mut t = -4.0;
            while t < 5.0 {
                let o = t - whites_roll_pull(t, r);
                assert!(o > last, "not increasing at {t} (whites {w})");
                last = o;
                t += 0.01;
            }
        }
        let v = [4.0, 2.0, 1.0];
        let o = whites_roll(v, 1.5, 1.0);
        assert!(o[0] < v[0]);
        assert!((o[0] / o[1] - 2.0).abs() < 1e-5 && (o[1] / o[2] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn scene_white_reaches_level() {
        let c = ToneCurve::new(crate::development::default_tone());
        let w = c.scene_white(ROLL_WHITE_LEVEL);
        assert!(w > 1.0 && w < 256.0, "{w}");
        assert!((c.eval(w) - ROLL_WHITE_LEVEL).abs() < 1e-3);
    }
}
