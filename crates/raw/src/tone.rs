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
}
