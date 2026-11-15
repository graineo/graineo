//! Classic chain, step "detail signal": the corrected linear codes are converted to
//! cube-root codes (which are close to perceptual lightness), the detail of a weighted
//! mono image (its high-pass) is added to all three layers equally, and the result is converted
//! back to linear codes.
//!
//! In the cube-root domain
//! - the middle layer gets a small correction where it exceeds both other layers (a smooth,
//!   tanh-shaped weight of both differences, scaled by a camera value; off for long
//!   exposure-sensitivities and when the step flag asks for it),
//! - the mono image is `0.35 R + 0.55 G + 0.10 B` of the *linear* codes (fixed-point 14 bit),
//!   converted to cube-root codes, and
//! - the high-pass of the mono image is the image minus its binomial 1-4-6-4-1 low-pass
//!   (12-bit fixed point, edges repeated), computed vertically first, then horizontally; for
//!   images that are rotated by a quarter turn in the output the order is swapped.
//!
//! The conversion tables have a continuation below zero (the codes are signed): the cube-root
//! table is a parabola down to zero for negative codes, the inverse a straight line below the
//! cube root of the offset.

use rayon::prelude::*;
use std::sync::OnceLock;

use super::calibration::Data;

/// Cube-root table, index = code as 16-bit value, value = root code (signed 16 bit).
pub fn root() -> &'static [i16] {
    static T: OnceLock<Vec<i16>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0i16; 65536];
        for (i, v) in t.iter_mut().enumerate() {
            let x = (i as f64 * 0.000244140625 + 0.008000000000000002).powf(0.3333333333333333);
            *v = (x * 4096.0 + 0.5) as i32 as i16;
        }
        // negative codes −4096 … −1: parabola down to zero
        for k in 0..4096usize {
            let n = k as i32 - 4096;
            let t1 = (((f64::from(n) * 0.000244140625 * 8.333333333333332 + 0.2) * 4096.0) + 819.2) * 0.5;
            let t1 = t1.max(0.0);
            let q = t1 * t1 / 819.2;
            let r = (q + 0.5) as i32;
            t[0xF000 + k] = r.max(0) as i16;
        }
        t
    })
}

/// Inverse table: index = root code as 16-bit value, value = linear code (0 … 32767).
pub fn exponent() -> &'static [i16] {
    static T: OnceLock<Vec<i16>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0i16; 65536];
        let round = |d: f64| -> i32 {
            let v = (d + if d >= 0.0 { 0.5 } else { -0.5 }) as i32;
            if v > 0x7ffe { 0x7fff } else { v }
        };
        for (i, v) in t.iter_mut().enumerate().take(0x333) {
            let d = ((i as f64 * 0.000244140625 + -0.2) * 0.12000000000000002) * 4096.0;
            *v = round(d) as i16;
        }
        for (i, v) in t.iter_mut().enumerate().skip(0x333) {
            let x = i as f64 * 0.000244140625;
            let d = (x * x * x + -0.008000000000000002) * 4096.0;
            *v = round(d) as i16;
        }
        for v in t.iter_mut().skip(0xF000) {
            *v = 0;
        }
        t
    })
}

/// Parameters of the middle-layer correction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GreenCorrection {
    /// Weight times 16384 (0 = off).
    pub strength: i32,
    /// Upper limit of the smooth weight function (the table is scaled with half of it).
    pub upper_limit: i32,
}

/// Reads the correction from the camera data (defaults when all three values are missing or zero).
pub fn green_parameter(d: &Data, iso: f64, skip: u32) -> GreenCorrection {
    let mut weight = f64::from(d.number("GreenWeight"));
    let mut high = d.number("GreenHighLimit") as i32;
    let low = d.number("GreenLowLimit") as i32;
    if high == 0 && low == 0 && weight == 0.0 {
        weight = 0.001;
        high = 200;
    }
    if skip & 2 != 0 || iso >= 3200.0 {
        weight = 0.0;
    }
    GreenCorrection { strength: (weight * 16384.0) as i32, upper_limit: high }
}

fn tanh_table(high: i32) -> Vec<f32> {
    let half = f64::from(high / 2);
    (0..1000).map(|i| (((f64::from(i) / 40.0 + -4.0).tanh() + 1.0) * half) as f32).collect()
}

/// Low-pass weights of the 5-tap filter for a scale factor (outer, inner, centre), sum 4096.
fn weights(scale: f64) -> (i32, i32, i32) {
    let r = (scale * 0.25).min(0.5);
    let r2 = -2.0 * r * r + r;
    let outer = (r * r * 4096.0 + 0.5) as i32;
    let inner = ((r2 + r2) * 4096.0 + 0.5) as i32;
    let center = (inner + outer) * -2 + 4096;
    (outer, inner, center)
}

#[inline]
fn tp(k: (i32, i32, i32), s: [i32; 5]) -> i16 {
    let sum = s[2].wrapping_mul(k.2).wrapping_add(0x800).wrapping_add((s[1] + s[3]).wrapping_mul(k.1)).wrapping_add((s[0] + s[4]).wrapping_mul(k.0));
    ((sum as u32) >> 12) as u16 as i16
}

/// Low-pass along one axis (`vertical` = along the rows' direction of travel from row to row), edges repeated.
fn low_pass(src: &[i16], w: usize, h: usize, k: (i32, i32, i32), vertical: bool) -> Vec<i16> {
    let mut out = vec![0i16; w * h];
    let at = |x: isize, y: isize| -> i32 {
        let xc = x.clamp(0, w as isize - 1) as usize;
        let yc = y.clamp(0, h as isize - 1) as usize;
        i32::from(src[yc * w + xc])
    };
    // rows are independent: in parallel
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (xi, yi) = (x as isize, y as isize);
            let s = if vertical {
                [at(xi, yi - 2), at(xi, yi - 1), at(xi, yi), at(xi, yi + 1), at(xi, yi + 2)]
            } else {
                [at(xi - 2, yi), at(xi - 1, yi), at(xi, yi), at(xi + 1, yi), at(xi + 2, yi)]
            };
            *o = tp(k, s);
        }
    });
    out
}

/// High-pass of the mono image: image minus low-pass; `cross` swaps the order of the two axes.
pub fn high_pass(mono: &mut [i16], w: usize, h: usize, scale: f64, cross: bool) {
    let k = weights(scale);
    let a = low_pass(mono, w, h, k, !cross);
    let b = low_pass(&a, w, h, k, cross);
    for (m, t) in mono.iter_mut().zip(&b) {
        *m = m.wrapping_sub(*t);
    }
}

thread_local! {
    /// Sharpness slider of the run on this thread (0 = neutral, amount 1.0).
    static SHARPNESS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// Recording of the high-pass of the run on this thread (`Some` = requested).
    static HIGH_PASS: std::cell::RefCell<Option<Option<HighPass>>> = const { std::cell::RefCell::new(None) };
}

/// High-pass of the mono image over the active area (`width` × `height`, cube-root domain), as
/// the step adds it with amount 1.0, and the index into the inverse table each layer ended up
/// with (cube-root code + detail, three per pixel); the output step adds the high-pass again with
/// another sharpness.
#[derive(Clone, Debug)]
pub struct HighPass {
    pub width: usize,
    pub height: usize,
    pub values: Vec<i16>,
    pub index: Vec<i16>,
}

/// Runs `f` (a chain run on this thread) and returns the high-pass of its detail step as well.
pub fn with_high_pass<R>(f: impl FnOnce() -> R) -> (R, Option<HighPass>) {
    let before = HIGH_PASS.replace(Some(None));
    let r = f();
    let hp = HIGH_PASS.replace(before).flatten();
    (r, hp)
}

/// Runs `f` (a chain run on this thread) with the sharpness slider `sharpness` (−2 … +2, 0 =
/// neutral): the high-pass is added with the amount `1 + sharpness` (14-bit fixed point,
/// truncated), not with 1.0.
pub fn with_sharpness<R>(sharpness: f64, f: impl FnOnce() -> R) -> R {
    let before = SHARPNESS.replace(sharpness);
    let r = f();
    SHARPNESS.set(before);
    r
}

/// Amount of the high-pass in 14-bit fixed point for a sharpness slider value.
pub fn magnitude(sharpness: f64) -> i32 {
    ((sharpness + 1.0) * 16384.0) as i32
}

/// High-pass for a downscaled preview: the full-resolution high-pass of the chain does not fit a
/// smaller image (averaged, it would almost vanish), so the preview gets one of its own, made the
/// same way from its linear codes (`codes`, three per pixel, `w` × `h`: mono image, cube root,
/// minus the 1-4-6-4-1 low-pass) and weighted with `strength` (the preview passes `√(1/factor)`, as
/// downscaled images are sharpened elsewhere). The output step adds it with the slider's amount
/// minus 1 ([`super::output::Output::pixel_with_sharpness`]); sharpness 0 leaves the preview as it is.
pub fn preview_high_pass(codes: &[[f32; 3]], w: usize, h: usize, strength: f32) -> Vec<i16> {
    if w < 5 || h < 5 || codes.len() != w * h {
        return vec![0; w * h];
    }
    let wz = root();
    let mut mono: Vec<i16> = codes
        .iter()
        .map(|c| {
            let [r, g, b] = c.map(|v| (v.round() as i32).clamp(-4096, 0x7fff));
            let s = (0x1666i32 * r + 0x666 * b + 0x2333 * g + 0x2000) as u32 >> 14;
            wz[(s & 0xffff) as usize]
        })
        .collect();
    high_pass(&mut mono, w, h, 1.0, false);
    mono.iter().map(|&v| (f32::from(v) * strength).round() as i16).collect()
}

/// The step: `img` are the linear codes (three layers per pixel, `w` × `h`), `crop` the active area
/// (left, top, right, bottom exclusive). Returns the linear codes with the detail added inside
/// the active area (outside: only the round trip through both tables). The high-pass is weighted
/// with the sharpness slider set by [`with_sharpness`] (neutral: amount 1.0).
pub fn run(img: &[i32], w: usize, h: usize, crop: [usize; 4], green: GreenCorrection, cross: bool) -> Vec<i32> {
    let k = magnitude(SHARPNESS.get());
    let wz = root();
    let ex = exponent();
    let idx = |v: i32| (v as i16 as u16) as usize;
    let mut out = vec![0i32; w * h * 3];
    // outside the active area: round trip
    out.par_iter_mut().zip(img.par_iter()).for_each(|(o, v)| {
        *o = i32::from(ex[(wz[idx(*v)] as u16) as usize]);
    });
    let (x0, y0, x1, y1) = (crop[0], crop[1], crop[2], crop[3]);
    let (cw, ch) = (x1 - x0, y1 - y0);
    let tanh = tanh_table(green.upper_limit);
    let tmax = tanh[999];
    let tt = |d: i32| -> f32 {
        if d < 0 {
            0.0
        } else if d < 1000 {
            tanh[d as usize]
        } else {
            tmax
        }
    };
    let mut roots = vec![0i32; cw * ch * 3];
    let mut mono = vec![0i16; cw * ch];
    roots.par_chunks_mut(cw * 3).zip(mono.par_chunks_mut(cw)).enumerate().for_each(|(y, (roots, mono))| {
        for x in 0..cw {
            let p = ((y + y0) * w + x + x0) * 3;
            let (r, g, b) = (img[p] as i16 as i32, img[p + 1] as i16 as i32, img[p + 2] as i16 as i32);
            let (rr, gg, bb) = (i32::from(wz[idx(r)]), i32::from(wz[idx(g)]), i32::from(wz[idx(b)]));
            let f = (tt(gg - rr) * tt(gg - bb)) as i32;
            let corr = ((f.wrapping_mul(green.strength)) as u32 >> 14) as i32;
            let q = x * 3;
            roots[q] = rr;
            roots[q + 1] = i32::from((gg + corr) as i16);
            roots[q + 2] = bb;
            let s = (0x1666i32 * r + 0x666 * b + 0x2333 * g + 0x2000) as u32 >> 14;
            mono[x] = wz[(s & 0xffff) as usize];
        }
    });
    high_pass(&mut mono, cw, ch, 1.0, cross);
    let capture = HIGH_PASS.with_borrow(|h| h.is_some());
    let mut index = if capture { vec![0i16; cw * ch * 3] } else { Vec::new() };
    // rows of the active area in parallel (`out` rows y0 … y1, columns x0 … x1)
    let target = |y: usize, o: &mut [i32], idx_row: Option<&mut [i16]>| {
        let mut idx_row = idx_row;
        for x in 0..cw {
            let d = i32::from(mono[y * cw + x]).wrapping_mul(k) >> 14; // neutral: amount 16384
            let q = (y * cw + x) * 3;
            for c in 0..3 {
                let i = (roots[q + c] + d) as u32 & 0xffff;
                o[(x + x0) * 3 + c] = i32::from(ex[i as usize]);
                if let Some(r) = idx_row.as_deref_mut() {
                    r[x * 3 + c] = i as u16 as i16;
                }
            }
        }
    };
    let rows = out[y0 * w * 3..y1 * w * 3].par_chunks_mut(w * 3).enumerate();
    if capture {
        rows.zip(index.par_chunks_mut(cw * 3)).for_each(|((y, o), r)| target(y, o, Some(r)));
    } else {
        rows.for_each(|(y, o)| target(y, o, None));
    }
    HIGH_PASS.with_borrow_mut(|h| {
        if let Some(place) = h.as_mut() {
            *place = Some(HighPass { width: cw, height: ch, values: mono, index });
        }
    });
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The preview high-pass: zero on an even area, a signal at an edge, scaled with the given strength.
    #[test]
    fn preview_high_pass_marks_edges_only() {
        let (w, h) = (16, 12);
        let codes: Vec<[f32; 3]> = (0..w * h).map(|i| if i % w < 8 { [800.0; 3] } else { [3000.0; 3] }).collect();
        let full = preview_high_pass(&codes, w, h, 1.0);
        let half = preview_high_pass(&codes, w, h, 0.5);
        assert_eq!(full[5 * w + 2], 0, "even area");
        assert!(full[5 * w + 7] < 0 && full[5 * w + 8] > 0, "edge: {} {}", full[5 * w + 7], full[5 * w + 8]);
        assert!((f32::from(half[5 * w + 8]) - f32::from(full[5 * w + 8]) * 0.5).abs() <= 1.0);
        assert!(preview_high_pass(&codes[..4], 2, 2, 1.0).iter().all(|v| *v == 0), "too small: nothing");
    }
}
