//! Automatic black and white point of the tone curve, from the histogram of the shown photo.
//!
//! "Auto (brightness)" moves the end points of the RGB curve so that [`CLIP`] of the pixels become pure black and
//! pure white; "Auto (RGB)" does the same per channel on the red, green and blue curves, which also removes a colour
//! cast in the shadows and highlights. The histogram is that of the output (after the curve), so the wanted output
//! values are first traced back through the curve that acted (the curve is monotone, see `develop::curve`). Inner
//! points between the new end points are kept, points outside them are dropped. One click is one change (one ⌘Z).

use develop::curve::{eval, Points};
use develop::ToneCurve;

/// Share of the pixels that may clip at each end.
pub const CLIP: f64 = 0.001;

/// Output values (0 … 1) below which `clip` and above which `clip` of the counts lie (`hist`: 256 bins).
pub fn percentiles(hist: &[u32], clip: f64) -> Option<(f32, f32)> {
    let total: u64 = hist.iter().map(|&n| n as u64).sum();
    if total == 0 || hist.len() < 2 {
        return None;
    }
    let last = (hist.len() - 1) as f32;
    let limit = (total as f64 * clip) as u64;
    let mut acc = 0u64;
    let lo = hist.iter().position(|&n| {
        acc += n as u64;
        acc > limit
    })?;
    acc = 0;
    let hi = hist.len() - 1 - hist.iter().rev().position(|&n| {
        acc += n as u64;
        acc > limit
    })?;
    (hi > lo).then(|| (lo as f32 / last, hi as f32 / last))
}

/// Input of curve `p` (empty = diagonal) whose output is `y`: the largest input still at or below `y` for the black
/// point (`high` false), the smallest at or above it for the white point (`high` true).
pub fn invert(p: &[[f32; 2]], y: f32, high: bool) -> f32 {
    let (mut a, mut b) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let m = 0.5 * (a + b);
        let v = eval(p, m);
        let go_right = if high { v < y } else { v <= y };
        if go_right {
            a = m;
        } else {
            b = m;
        }
    }
    if high { b } else { a }
}

/// Curve `p` with new end points at inputs `x0` (→ 0) and `x1` (→ 1); inner points between them stay.
pub fn with_end_points(p: &[[f32; 2]], x0: f32, x1: f32) -> Points {
    let x0 = x0.clamp(0.0, 0.98);
    let x1 = x1.clamp(x0 + 0.02, 1.0);
    let mut out = vec![[x0, 0.0]];
    out.extend(p.iter().filter(|q| q[0] > x0 + 0.01 && q[0] < x1 - 0.01).copied());
    out.push([x1, 1.0]);
    out
}

/// "Auto (brightness)": end points of the RGB curve from the mean of the three channel histograms. `acted`: the curve
/// that acted on the shown histogram (bypassed: none). `false` when the histogram says nothing.
pub fn brightness(curve: &mut ToneCurve, hist: &[Vec<u32>; 3], acted: bool) -> bool {
    let sum: Vec<u32> = (0..hist[0].len()).map(|i| hist[0][i] + hist[1][i] + hist[2][i]).collect();
    let Some((lo, hi)) = percentiles(&sum, CLIP) else { return false };
    let old: Points = if acted { curve.rgb.clone() } else { Vec::new() };
    curve.rgb = with_end_points(&curve.rgb, invert(&old, lo, false), invert(&old, hi, true));
    true
}

/// "Auto (RGB)": end points of the red, green and blue curves, each from its own histogram (traced back through the
/// channel curve; the RGB curve before it stays as it is).
pub fn per_channel(curve: &mut ToneCurve, hist: &[Vec<u32>; 3], acted: bool) -> bool {
    let mut any = false;
    for k in 0..3 {
        let Some((lo, hi)) = percentiles(&hist[k], CLIP) else { continue };
        let ch = curve.channel(k + 1).clone();
        let old: Points = if acted { ch.clone() } else { Vec::new() };
        *curve.channel_mut(k + 1) = with_end_points(&ch, invert(&old, lo, false), invert(&old, hi, true));
        any = true;
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(lo: usize, hi: usize) -> Vec<u32> {
        let mut h = vec![0u32; 256];
        for v in h.iter_mut().take(hi + 1).skip(lo) {
            *v = 100;
        }
        h
    }

    #[test]
    fn percentiles_ignore_a_few_outliers() {
        let mut h = hist(20, 230);
        h[0] = 1;
        h[255] = 1;
        let (lo, hi) = percentiles(&h, CLIP).unwrap();
        assert!((lo - 20.0 / 255.0).abs() < 1e-6 && (hi - 230.0 / 255.0).abs() < 1e-6, "{lo} {hi}");
        assert_eq!(percentiles(&[0; 256], CLIP), None);
    }

    #[test]
    fn inverting_the_diagonal_and_a_curve() {
        assert!((invert(&[], 0.25, false) - 0.25).abs() < 1e-4);
        let p = vec![[0.0, 0.0], [1.0, 0.5]];
        assert!((invert(&p, 0.25, true) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn brightness_stretches_and_is_stable() {
        let h = hist(20, 230);
        let mut c = ToneCurve::default();
        assert!(brightness(&mut c, &[h.clone(), h.clone(), h.clone()], true));
        assert!((c.rgb[0][0] - 20.0 / 255.0).abs() < 0.01 && c.rgb[0][1] == 0.0);
        assert!((c.rgb[1][0] - 230.0 / 255.0).abs() < 0.01 && c.rgb[1][1] == 1.0);
        // applied again on the stretched histogram (full range now): the end points stay
        let before = c.rgb.clone();
        let full = hist(0, 255);
        brightness(&mut c, &[full.clone(), full.clone(), full], true);
        for (a, b) in before.iter().zip(&c.rgb) {
            assert!((a[0] - b[0]).abs() < 0.01, "{before:?} {:?}", c.rgb);
        }
    }

    #[test]
    fn per_channel_removes_a_cast() {
        let mut c = ToneCurve::default();
        assert!(per_channel(&mut c, &[hist(40, 255), hist(0, 255), hist(0, 200)], true));
        assert!((c.red[0][0] - 40.0 / 255.0).abs() < 0.01);
        assert!(c.green[0][0] < 0.01 && c.green[1][0] > 0.99);
        assert!((c.blue[1][0] - 200.0 / 255.0).abs() < 0.01);
        assert!(c.rgb.is_empty());
    }

    #[test]
    fn inner_points_between_the_ends_stay() {
        let p = vec![[0.0, 0.0], [0.05, 0.1], [0.5, 0.6], [1.0, 1.0]];
        let q = with_end_points(&p, 0.1, 0.9);
        assert_eq!(q, vec![[0.1, 0.0], [0.5, 0.6], [0.9, 1.0]]);
    }
}
