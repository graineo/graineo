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
