//! Automatic look ("Auto", ⌘U): all tone and colour sliders are determined together so that the rendered photo
//! reaches a target appearance.
//!
//! 1. The current rendering (tone sliders neutral) is measured once: brightness statistics in L* (mean, spread,
//!    percentiles) plus mean chroma and clipping ([`Features`]).
//! 2. From these measurements a target vector is derived for all features at once ([`target`]): a little brighter
//!    and more open for dark photos, a deeper black point, more separation, noticeably livelier colour. The model
//!    is a small regression fitted to a reference set of photos and the same on every platform.
//! 3. One linear least-squares problem over all sliders together (box bounds, mild priors on the slider shape)
//!    with a fixed sensitivity matrix per pipeline ([`Pipeline`]).
//! 4. One confirmation rendering; the sensitivity matrix is corrected along the step taken (secant update) and the
//!    whole vector is solved again from the first measurement.
//!
//! No slider is computed from the result of another one: every solve covers the whole vector. Two renderings of a
//! small analysis image (≈ 320 px) per run.
//!
//! The black point is set with the blacks slider for other RAWs. Sigma has no blacks slider, so there the black point
//! is set with the point curve ([`apply`] sets a two-point RGB curve only if the photo has no curve of its own).

use crate::Settings;

/// Number of features.
pub const NF: usize = 8;
/// Number of sliders.
pub const NS: usize = 9;

/// Feature names (order of [`Features`]).
pub const FEATURES: [&str; NF] = ["L", "p10", "p90", "sd", "p005", "p995", "C", "clip"];
/// Slider names (order of [`Vector`]).
pub const SLIDERS: [&str; NS] = ["exposure", "contrast", "highlight", "shadow", "whites", "blacks", "vibrance", "saturation", "black_point"];

/// Slider values in the order of [`SLIDERS`]; `black_point` is the input value of the curve that maps to black.
pub type Vector = [f64; NS];
/// Measurements in the order of [`FEATURES`]: mean L*, L* at 10 % and 90 %, spread of L*, L* at 0.5 % and 99.5 %,
/// mean chroma C*, share of pixels with a channel at white (%).
pub type Features = [f64; NF];

/// The rendering path the sliders act in: sensitivities and available sliders differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipeline {
    /// Sigma (Foveon): no whites/blacks sliders
    Sigma,
    /// Other RAWs and images (rawdev)
    Raw,
}

/// Feature sensitivity per slider unit (columns in the order of [`SLIDERS`]), measured as the mean of probe
/// renderings over the reference set (`graineo-dev auto-look --jacobian`).
const J_RAW: [[f64; NS]; NF] = [
    // exposure contrast highlight shadow whites blacks vibrance saturation black_point
    [14.2, -1.3, 6.4, 7.9, 1.5, 3.0, 0.0, 0.0, -58.9],
    [13.6, -6.3, 0.0, 16.0, 0.0, 6.5, 0.0, 0.0, -91.7],
    [11.5, 5.0, 18.3, 0.0, 3.5, 0.0, 0.0, 0.0, -15.0],
    [-2.7, 4.6, 5.8, -5.7, 1.9, -2.2, 0.0, 0.0, 25.2],
    [4.3, -3.3, 0.0, 6.7, 0.0, 5.0, 0.0, 0.0, -71.7],
    [7.9, 7.0, 15.8, 0.0, 7.8, 0.0, 0.0, 0.0, -10.0],
    [2.0, 2.0, -0.6, 2.9, -0.5, 1.1, 2.1, 14.2, 13.3],
    [0.5, 0.2, 0.5, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0],
];
const J_SIGMA: [[f64; NS]; NF] = [
    [10.7, -3.3, 1.6, 2.4, 0.0, 0.0, 0.0, 0.0, -75.4],
    [4.4, -3.5, 0.5, 1.3, 0.0, 0.0, 0.0, 0.0, -73.4],
    [13.6, 2.0, 2.5, 2.2, 0.0, 0.0, 0.0, -0.7, -33.4],
    [3.0, 2.8, 0.7, 0.2, 0.0, 0.0, -0.1, -0.4, 21.3],
    [1.2, -1.3, 0.2, 0.5, 0.0, 0.0, 0.0, 0.0, -68.4],
    [10.1, 2.8, 1.8, 1.2, 0.0, 0.0, -0.2, -0.7, -16.7],
    [2.0, 0.2, 0.1, 0.1, 0.0, 0.0, 3.5, 15.7, 4.3],
    [0.5, 0.2, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
];

/// Box bounds per slider (lower, upper).
const LO: Vector = [-1.0, -0.2, -0.8, -0.3, -0.5, -1.0, 0.0, 0.0, 0.0];
const HI: Vector = [1.0, 0.5, 0.2, 0.8, 0.5, 0.3, 1.0, 0.4, 0.06];
/// Priors (centre, weight): the shape of the result (contrast slightly up, highlights slightly down),
/// otherwise small sliders preferred.
const PRIOR: [(f64, f64); NS] = [(0.0, 1.0), (0.12, 4.0), (-0.15, 2.0), (0.0, 2.0), (0.0, 3.0), (0.0, 1.5), (0.0, 0.5), (0.0, 2.0), (0.0, 20.0)];
/// Tolerance per feature (1 / weight in the least-squares problem).
const TOL: Features = [1.0, 2.0, 1.5, 1.5, 1.5, 2.0, 1.0, 0.15];

/// Measure features of an sRGB image (values 0 … 1 per channel, `px` yields RGB triples).
pub fn features(px: impl Iterator<Item = [f32; 3]>) -> Option<Features> {
    const BINS: usize = 2000;
    let mut hist = vec![0u32; BINS];
    let (mut n, mut sum, mut sum2, mut sc, mut clip) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for p in px {
        let [l, a, b] = lab(p);
        let l = l.clamp(0.0, 100.0);
        hist[((l / 100.0 * (BINS - 1) as f64) + 0.5) as usize] += 1;
        n += 1.0;
        sum += l;
        sum2 += l * l;
        sc += (a * a + b * b).sqrt();
        if p.iter().any(|&c| c >= 0.995) {
            clip += 1.0;
        }
    }
    if n < 100.0 {
        return None;
    }
    let pct = |q: f64| -> f64 {
        let limit = q / 100.0 * n;
        let mut acc = 0.0;
        for (i, &v) in hist.iter().enumerate() {
            acc += v as f64;
            if acc >= limit {
                return i as f64 / (BINS - 1) as f64 * 100.0;
            }
        }
        100.0
    };
    let mean = sum / n;
    Some([mean, pct(10.0), pct(90.0), (sum2 / n - mean * mean).max(0.0).sqrt(), pct(0.5), pct(99.5), sc / n, clip / n * 100.0])
}

/// CIE L*a*b* (D65) of an sRGB triple.
fn lab(p: [f32; 3]) -> [f64; 3] {
    let lin = p.map(|c| {
        let c = (c as f64).clamp(0.0, 1.0);
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = (0.4124 * lin[0] + 0.3576 * lin[1] + 0.1805 * lin[2]) / 0.95047;
    let y = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    let z = (0.0193 * lin[0] + 0.1192 * lin[1] + 0.9505 * lin[2]) / 1.08883;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// The two-point black-point curve applied to one display value (input `bp` → 0, 1 → 1, linear between: a
/// monotone spline through two points is a straight line).
pub fn black_point(v: f32, bp: f32) -> f32 {
    if bp <= 0.0 {
        v
    } else {
        ((v - bp) / (1.0 - bp)).clamp(0.0, 1.0)
    }
}

/// Target vector for all features from the measurement of the starting rendering. Regression over the reference
/// set: a change per feature from the brightness and spread of the photo (the darker the photo, the more it opens;
/// bright, flat photos get a little darker and more separated), black point at about half its value, chroma ×1.3,
/// no new clipping.
pub fn target(f0: &Features) -> Features {
    let r = [1.0, (f0[0] - 50.0) / 25.0, (f0[3] - 22.0) / 8.0, (f0[0] - 60.0).max(0.0) / 10.0];
    let lin = |b: [f64; 4]| b.iter().zip(r).map(|(b, r)| b * r).sum::<f64>();
    let mut t = *f0;
    t[0] += lin([1.77, -1.14, 0.48, -2.03]);
    t[1] += lin([0.63, 0.63, 0.64, -2.30]);
    t[2] += lin([0.45, -4.12, -1.05, 0.21]);
    t[3] += lin([0.04, -1.65, -0.78, 1.18]);
    t[4] = f0[4] * 0.52;
    t[5] += lin([0.92, -3.20, -1.37, -0.44]);
    t[6] = f0[6] * lin([0.267, 0.0, -0.086, -0.051]).exp();
    t[1] = t[1].max(0.0);
    t[5] = t[5].min(100.0);
    t
}

/// Slider bounds for a pipeline and the photo's ISO (lifting shadows is limited by noise); `curve_free`: the photo
/// has no point curve of its own, so the black point may be set.
fn bounds(pipe: Pipeline, iso: f64, curve_free: bool) -> (Vector, Vector) {
    let (lo, mut hi) = (LO, HI);
    let stops = (iso.max(100.0) / 100.0).log2();
    hi[3] = (0.8 - 0.15 * stops).clamp(0.2, 0.8);
    let mut lo = lo;
    if pipe == Pipeline::Sigma {
        for k in [4, 5] {
            lo[k] = 0.0;
            hi[k] = 0.0;
        }
    }
    // other RAWs set the black point with the blacks slider; the point curve only for Sigma (no blacks slider)
    if !curve_free || pipe == Pipeline::Raw {
        hi[8] = 0.0;
    }
    (lo, hi)
}

/// Bounded, regularised linear least squares: minimise ‖W (f0 + J x − t)‖² + Σ wᵢ² (xᵢ − μᵢ)² with lo ≤ x ≤ hi
/// (cyclic coordinate descent; the problem is convex and tiny).
fn solve_linear(f0: &Features, j: &[[f64; NS]; NF], t: &Features, lo: &Vector, hi: &Vector) -> Vector {
    let w: Features = TOL.map(|v| 1.0 / v);
    // normal equations H x = g of the quadratic
    let mut h = [[0f64; NS]; NS];
    let mut g = [0f64; NS];
    for k in 0..NF {
        let r = t[k] - f0[k];
        for a in 0..NS {
            g[a] += w[k] * w[k] * j[k][a] * r;
            for b in 0..NS {
                h[a][b] += w[k] * w[k] * j[k][a] * j[k][b];
            }
        }
    }
    for a in 0..NS {
        let (mu, pw) = PRIOR[a];
        h[a][a] += pw * pw;
        g[a] += pw * pw * mu;
    }
    let mut x = [0f64; NS];
    for a in 0..NS {
        x[a] = 0f64.clamp(lo[a], hi[a]);
    }
    for _ in 0..400 {
        let mut moved = 0f64;
        for a in 0..NS {
            if h[a][a] <= 0.0 || lo[a] == hi[a] {
                continue;
            }
            let rest: f64 = (0..NS).filter(|&b| b != a).map(|b| h[a][b] * x[b]).sum();
            let v = ((g[a] - rest) / h[a][a]).clamp(lo[a], hi[a]);
            moved = moved.max((v - x[a]).abs());
            x[a] = v;
        }
        if moved < 1e-7 {
            break;
        }
    }
    x
}

/// Fixed sensitivity matrix of a pipeline.
pub fn jacobian(pipe: Pipeline) -> [[f64; NS]; NF] {
    match pipe {
        Pipeline::Sigma => J_SIGMA,
        Pipeline::Raw => J_RAW,
    }
}

/// Solve the slider vector. `render` develops the analysis image with a slider vector (black point excluded: it
/// is applied by [`features`]' caller through [`black_point`]) and measures it; it is called twice (start and
/// confirmation). `iso` limits the shadows, `curve_free` allows the black point.
pub fn solve(pipe: Pipeline, iso: f64, curve_free: bool, render: &mut dyn FnMut(&Vector) -> Option<Features>) -> Option<Vector> {
    let f0 = render(&[0.0; NS])?;
    let t = target(&f0);
    let (lo, hi) = bounds(pipe, iso, curve_free);
    let mut j = jacobian(pipe);
    let x1 = snapped(&solve_linear(&f0, &j, &t, &lo, &hi));
    let Some(f1) = render(&x1) else { return Some(x1) };
    // secant correction of the sensitivities along the step (scaled by the slider ranges so that small sliders
    // like the black point are not drowned)
    let scale: Vector = std::array::from_fn(|a| (HI[a] - LO[a]).max(1e-6));
    let xs: Vector = std::array::from_fn(|a| x1[a] / scale[a]);
    let norm: f64 = xs.iter().map(|v| v * v).sum();
    if norm > 1e-9 {
        for k in 0..NF {
            let pred: f64 = (0..NS).map(|a| j[k][a] * x1[a]).sum();
            let miss = (f1[k] - f0[k]) - pred;
            for a in 0..NS {
                j[k][a] += miss * xs[a] / norm / scale[a];
            }
        }
    }
    Some(snapped(&solve_linear(&f0, &j, &t, &lo, &hi)))
}

/// Rounded to the slider steps (exposure 0.05 EV, black point 0.005, the others 0.01); no "-0".
pub fn snapped(x: &Vector) -> Vector {
    std::array::from_fn(|a| {
        let step = match a {
            0 => 0.05,
            8 => 0.005,
            _ => 0.01,
        };
        let v = (x[a] / step).round() * step;
        if v == 0.0 { 0.0 } else { v }
    })
}

/// Whether the photo's point curve is free for the automatic black point: no curve, or only the black-point curve
/// an earlier "Auto" set.
pub fn curve_free(s: &Settings) -> bool {
    let c = &s.tone_curve;
    let own = |p: &crate::curve::Points| crate::curve::is_identity(p);
    own(&c.red) && own(&c.green) && own(&c.blue) && (own(&c.rgb) || is_black_point_curve(&c.rgb))
}

fn is_black_point_curve(p: &crate::curve::Points) -> bool {
    p.len() == 2 && p[0][1] == 0.0 && p[0][0] < 0.2 && p[1] == [1.0, 1.0]
}

/// Settings for one analysis rendering: the slider vector applied, black point left out (it acts on the
/// rendered values, see [`black_point`]), Fill Light off.
pub fn probe_settings(base: &Settings, x: &Vector) -> Settings {
    let mut s = base.clone();
    set_sliders(&mut s, x);
    if curve_free(base) {
        s.tone_curve.rgb.clear();
    }
    s
}

fn set_sliders(s: &mut Settings, x: &Vector) {
    s.exposure = x[0];
    s.contrast = x[1];
    s.highlight = x[2];
    s.shadow = x[3];
    s.fill_light = 0.0;
    s.raw.whites = x[4] as f32;
    s.raw.blacks = x[5] as f32;
    s.vibrance = x[6] as f32;
    s.saturation = x[7] as f32;
}

/// Apply a solved vector to the settings (the black point as a two-point RGB curve when the curve is free).
pub fn apply(x: &Vector, s: &mut Settings) {
    let free = curve_free(s);
    set_sliders(s, x);
    if free {
        s.tone_curve.rgb = if x[8] > 0.0 { vec![[x[8] as f32, 0.0], [1.0, 1.0]] } else { Vec::new() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The linear model reaches its target when it can: with the true sensitivities the second solve lands on the
    /// target within the tolerances, and every value stays inside its bounds.
    #[test]
    fn solve_reaches_a_linear_target_inside_the_bounds() {
        let j = jacobian(Pipeline::Raw);
        let f0: Features = [40.0, 10.0, 75.0, 22.0, 2.0, 85.0, 15.0, 0.0];
        let mut render = |x: &Vector| -> Option<Features> { Some(std::array::from_fn(|k| f0[k] + (0..NS).map(|a| j[k][a] * x[a]).sum::<f64>())) };
        let x = solve(Pipeline::Raw, 100.0, true, &mut render).unwrap();
        let (lo, hi) = bounds(Pipeline::Raw, 100.0, true);
        for a in 0..NS {
            assert!(x[a] >= lo[a] - 1e-9 && x[a] <= hi[a] + 1e-9, "{} {}", SLIDERS[a], x[a]);
        }
        let f = render(&x).unwrap();
        let t = target(&f0);
        assert!((f[0] - t[0]).abs() < 1.5, "mean L* {} vs {}", f[0], t[0]);
        assert!(f[6] > f0[6], "more colour");
    }

    /// Sigma has no whites/blacks; a photo's own curve keeps the black point at 0.
    #[test]
    fn bounds_follow_pipeline_and_curve() {
        let (lo, hi) = bounds(Pipeline::Sigma, 100.0, false);
        assert_eq!((lo[4], hi[4], lo[5], hi[5], hi[8]), (0.0, 0.0, 0.0, 0.0, 0.0));
        let mut s = Settings::default();
        assert!(curve_free(&s));
        apply(&[0.0, 0.1, 0.0, 0.0, 0.0, 0.0, 0.2, 0.0, 0.02], &mut s);
        assert_eq!(s.tone_curve.rgb, vec![[0.02, 0.0], [1.0, 1.0]]);
        assert!(curve_free(&s), "the automatic black point may be replaced by the next Auto");
        s.tone_curve.rgb = vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]];
        assert!(!curve_free(&s));
    }

    #[test]
    fn features_of_grey_and_black_point() {
        let f = features(std::iter::repeat_n([0.5f32; 3], 1000)).unwrap();
        assert!((f[0] - 53.4).abs() < 0.3 && f[3] < 0.1 && f[6] < 0.1, "{f:?}");
        assert_eq!(black_point(0.02, 0.04), 0.0);
        assert!((black_point(0.52, 0.04) - 0.5).abs() < 1e-6);
    }
}
