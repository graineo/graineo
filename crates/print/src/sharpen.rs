//! Output sharpening: an unsharp mask on the brightness, with the radius given in millimetres on paper, so it looks
//! the same at 300 and at 720 ppi. Run after scaling to the print pixels. The ratio of sharpened to original
//! brightness is applied to all three channels (no colour fringes). Starting values, to be tuned on test prints.

use crate::template::{Media, Sharpen, SharpenAmount};
use crate::Image16;

/// Radius (Gaussian sigma) on paper in mm and strength per medium and level.
pub fn parameters(s: Sharpen) -> Option<(f64, f32)> {
    let radius_mm = match s.media {
        Media::Glossy => 0.08,
        Media::Matte => 0.12,
    };
    let amount = match (s.amount, s.media) {
        (SharpenAmount::Off, _) => return None,
        (SharpenAmount::Low, Media::Glossy) => 0.4,
        (SharpenAmount::Standard, Media::Glossy) => 0.7,
        (SharpenAmount::High, Media::Glossy) => 1.0,
        (SharpenAmount::Low, Media::Matte) => 0.6,
        (SharpenAmount::Standard, Media::Matte) => 1.0,
        (SharpenAmount::High, Media::Matte) => 1.4,
    };
    Some((radius_mm, amount))
}

/// Differences below this (of 1.0) are left alone: grain and noise are not blown up.
const THRESHOLD: f32 = 0.004;
/// Largest brightness change of one pixel (against halos at hard edges).
const LIMIT: f32 = 0.12;

/// Sharpen `img` (printed at `ppi`) in place.
pub fn apply(img: &mut Image16, ppi: f64, s: Sharpen) {
    let Some((radius_mm, amount)) = parameters(s) else { return };
    let sigma = radius_mm * ppi / 25.4;
    if img.width < 3 || img.height < 3 || sigma < 0.2 {
        return;
    }
    let (w, h) = (img.width, img.height);
    // brightness of the encoded values (Rec. 709 weights); the buffers are asked for, not assumed: without the memory
    // the page is printed unsharpened instead of ending the program (the job limits the page size before, see
    // `raster::check_size`)
    let Some(mut luma) = zeros(w * h) else { return };
    for (l, p) in luma.iter_mut().zip(img.data.chunks_exact(3)) {
        *l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 65535.0;
    }
    let Some(blur) = gaussian(&luma, w, h, sigma) else { return };
    for (i, p) in img.data.chunks_exact_mut(3).enumerate() {
        let y = luma[i];
        let d = y - blur[i];
        if d.abs() <= THRESHOLD {
            continue;
        }
        // soft threshold: the step below the threshold does not show as an edge of its own
        let d = d - THRESHOLD * d.signum();
        let target = (y + (amount * d).clamp(-LIMIT, LIMIT)).clamp(0.0, 1.0);
        if y > 1e-4 {
            let k = target / y;
            for c in p.iter_mut() {
                *c = (*c as f32 * k).round().clamp(0.0, 65535.0) as u16;
            }
        } else {
            let add = (target - y) * 65535.0;
            for c in p.iter_mut() {
                *c = (*c as f32 + add).round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

/// Unsharp mask on the brightness of an encoded RGB image (`data`, 16 bit, `w`×`h`) as used for export output
/// sharpening: the luminance is taken on the encoded values, its difference from the blur (Gaussian `sigma` in pixels)
/// moves it, and the result goes back as one ratio on the **linear** channels (`decode`/`encode` = transfer of the
/// image's colour space), so hue and saturation stay. Brightening stops where the largest linear channel reaches 1.
/// Fringe guard: next to blown highlights (largest channel ≥ 98 %) a dark halo cannot be balanced by a light one on the
/// clipped side, so darkening is faded out there (share of clipped pixels within about two radii).
pub fn unsharp_linear_ratio(data: &mut [u16], w: usize, h: usize, sigma: f64, amount: f32, decode: &(dyn Fn(f32) -> f32 + Sync), encode: &(dyn Fn(f32) -> f32 + Sync)) {
    if w < 3 || h < 3 || sigma < 0.2 || amount <= 0.0 || data.len() != w * h * 3 {
        return;
    }
    let Some(mut luma) = zeros(w * h) else { return };
    let Some(mut clipped) = zeros(w * h) else { return };
    for ((l, c), p) in luma.iter_mut().zip(clipped.iter_mut()).zip(data.chunks_exact(3)) {
        *l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 65535.0;
        *c = if p[0].max(p[1]).max(p[2]) as f32 >= 0.98 * 65535.0 { 1.0 } else { 0.0 };
    }
    let Some(blur) = gaussian(&luma, w, h, sigma) else { return };
    let near_clip = if clipped.iter().any(|&c| c > 0.0) { gaussian(&clipped, w, h, (2.0 * sigma).max(1.0)) } else { None };
    drop(clipped);
    let lut: Vec<f32> = (0..=65535u32).map(|v| decode(v as f32 / 65535.0)).collect();
    for (i, p) in data.chunks_exact_mut(3).enumerate() {
        let y = luma[i];
        let d = y - blur[i];
        if d.abs() <= THRESHOLD {
            continue;
        }
        let mut d = amount * (d - THRESHOLD * d.signum());
        if d < 0.0 {
            if let Some(n) = &near_clip {
                d *= (1.0 - 3.0 * n[i]).clamp(0.0, 1.0);
            }
        }
        let target = (y + d.clamp(-LIMIT, LIMIT)).clamp(0.0, 1.0);
        let (yl, tl) = (decode(y), decode(target));
        if yl <= 1e-6 {
            continue;
        }
        let lin = [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]];
        let mx = lin[0].max(lin[1]).max(lin[2]);
        let mut k = tl / yl;
        if k > 1.0 && mx > 0.0 {
            k = k.min((1.0 / mx).max(1.0));
        }
        for (c, l) in p.iter_mut().zip(lin) {
            *c = (encode(l * k).clamp(0.0, 1.0) * 65535.0).round() as u16;
        }
    }
}

/// `n` zeros, or `None` when the memory is not there.
fn zeros(n: usize) -> Option<Vec<f32>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0.0);
    Some(v)
}

/// Separable Gaussian blur (edges repeated); `None` without memory for its buffers.
fn gaussian(src: &[f32], w: usize, h: usize, sigma: f64) -> Option<Vec<f32>> {
    let r = (sigma * 3.0).ceil().max(1.0) as isize;
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i as f64 * i as f64) / (2.0 * sigma * sigma)).exp() as f32).collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    let mut tmp = zeros(w * h)?;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut a = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let xx = (x as isize + j as isize - r).clamp(0, w as isize - 1) as usize;
                a += row[xx] * kv;
            }
            tmp[y * w + x] = a;
        }
    }
    let mut out = zeros(w * h)?;
    for y in 0..h {
        for (j, kv) in k.iter().enumerate() {
            let yy = (y as isize + j as isize - r).clamp(0, h as isize - 1) as usize;
            let s = &tmp[yy * w..(yy + 1) * w];
            let o = &mut out[y * w..(y + 1) * w];
            for x in 0..w {
                o[x] += s[x] * kv;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srgb_decode(x: f32) -> f32 {
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    }
    fn srgb_encode(x: f32) -> f32 {
        if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
    }

    /// Image with a vertical edge between two colours (`a` left, `b` right), 40×8.
    fn colour_edge(a: [u16; 3], b: [u16; 3]) -> Vec<u16> {
        let (w, h) = (40, 8);
        let mut d = vec![0u16; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let c = if x < w / 2 { a } else { b };
                d[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        d
    }

    #[test]
    fn linear_ratio_sharpens_and_keeps_hue() {
        let mut d = colour_edge([12000, 8000, 4000], [40000, 26667, 13333]);
        let before = d.clone();
        unsharp_linear_ratio(&mut d, 40, 8, 1.0, 1.0, &srgb_decode, &srgb_encode);
        assert_ne!(d, before, "nothing sharpened");
        // the dark side next to the edge got darker, the light side lighter
        let px = |d: &[u16], x: usize| [d[(3 * 40 + x) * 3], d[(3 * 40 + x) * 3 + 1], d[(3 * 40 + x) * 3 + 2]];
        assert!(px(&d, 19)[0] < px(&before, 19)[0]);
        assert!(px(&d, 20)[0] > px(&before, 20)[0]);
        // ratios of the linear channels stay (hue and saturation)
        for x in [18, 19, 20, 21] {
            let (o, n) = (px(&before, x), px(&d, x));
            let r = |p: [u16; 3]| srgb_decode(p[0] as f32 / 65535.0) / srgb_decode(p[2] as f32 / 65535.0);
            assert!((r(o) - r(n)).abs() / r(o) < 0.01, "x {x}: {} vs {}", r(o), r(n));
        }
        // flat areas untouched
        assert_eq!(px(&d, 2), px(&before, 2));
    }

    #[test]
    fn no_dark_fringe_next_to_blown_highlights() {
        let mut guarded = colour_edge([30000; 3], [65535; 3]);
        unsharp_linear_ratio(&mut guarded, 40, 8, 1.0, 1.0, &srgb_decode, &srgb_encode);
        let mut unguarded = colour_edge([30000; 3], [64000; 3]);
        unsharp_linear_ratio(&mut unguarded, 40, 8, 1.0, 1.0, &srgb_decode, &srgb_encode);
        let at = |d: &[u16]| d[(3 * 40 + 19) * 3];
        assert!(at(&unguarded) < 29000, "control edge without clipping must get a dark halo: {}", at(&unguarded));
        assert!(at(&guarded) >= 29900, "dark halo next to the blown area: {}", at(&guarded));
    }

    /// A soft edge (a few pixels wide) across a test strip printed 30 mm wide.
    fn edge(ppi: f64) -> Image16 {
        let w = (30.0 / 25.4 * ppi).round() as usize;
        let h = 8;
        let mut img = Image16::new(w, h);
        let ramp_mm = 0.3;
        for y in 0..h {
            for x in 0..w {
                let mm = (x as f64 + 0.5) / ppi * 25.4 - 15.0;
                let t = (mm / ramp_mm + 0.5).clamp(0.0, 1.0);
                let v = (0.2 + 0.5 * t) * 65535.0;
                let i = (y * w + x) * 3;
                img.data[i..i + 3].copy_from_slice(&[v as u16; 3]);
            }
        }
        img
    }

    /// Width (mm) of the region the sharpening changed noticeably, around the edge.
    fn response_width_mm(ppi: f64, s: Sharpen) -> f64 {
        let before = edge(ppi);
        let mut after = before.clone();
        apply(&mut after, ppi, s);
        let w = before.width;
        let changed: Vec<usize> = (0..w).filter(|&x| (after.data[(3 * w + x) * 3] as i32 - before.data[(3 * w + x) * 3] as i32).abs() > 300).collect();
        assert!(!changed.is_empty(), "nothing sharpened at {ppi} ppi");
        (changed.last().unwrap() - changed.first().unwrap() + 1) as f64 / ppi * 25.4
    }

    #[test]
    fn same_width_on_paper_at_300_and_720_ppi() {
        for media in [Media::Glossy, Media::Matte] {
            let s = Sharpen { amount: SharpenAmount::Standard, media };
            let a = response_width_mm(300.0, s);
            let b = response_width_mm(720.0, s);
            assert!((a - b).abs() / b <= 0.10 + 1.0 / 300.0 * 25.4 / b, "{media:?}: {a:.3} mm at 300 ppi, {b:.3} mm at 720 ppi");
        }
    }

    #[test]
    fn off_changes_nothing_and_flat_areas_stay() {
        let mut img = edge(300.0);
        let orig = img.clone();
        apply(&mut img, 300.0, Sharpen { amount: SharpenAmount::Off, media: Media::Glossy });
        assert_eq!(img, orig);
        let mut flat = Image16 { width: 40, height: 40, data: vec![30000; 40 * 40 * 3] };
        apply(&mut flat, 300.0, Sharpen { amount: SharpenAmount::High, media: Media::Matte });
        assert!(flat.data.iter().all(|&v| v == 30000));
    }

    #[test]
    fn stronger_levels_change_more_and_matte_more_than_glossy() {
        let change = |s: Sharpen| {
            let mut img = edge(360.0);
            let orig = img.clone();
            apply(&mut img, 360.0, s);
            img.data.iter().zip(&orig.data).map(|(a, b)| (*a as i64 - *b as i64).abs()).sum::<i64>()
        };
        let g = |a| change(Sharpen { amount: a, media: Media::Glossy });
        assert!(g(SharpenAmount::Low) < g(SharpenAmount::Standard) && g(SharpenAmount::Standard) < g(SharpenAmount::High));
        assert!(change(Sharpen { amount: SharpenAmount::Standard, media: Media::Matte }) > g(SharpenAmount::Standard));
    }

    #[test]
    fn neutral_stays_neutral() {
        let mut img = edge(300.0);
        apply(&mut img, 300.0, Sharpen { amount: SharpenAmount::High, media: Media::Glossy });
        assert!(img.data.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
    }
}
