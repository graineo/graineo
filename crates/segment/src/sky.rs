//! Automatic sky selection without a class model of its own: a per-pixel guess (far in the depth estimate, upper part
//! of the picture, bright, bluish or evenly coloured) marks a region that touches the top edge; points inside and
//! outside it are given to the click selection (MobileSAM), whose mask follows the real edges; inside that mask,
//! pixels much darker than the sky (branches, wires) are taken out again. An optional check (`score`, e.g. a
//! zero-shot image model) confirms that the region is sky. The result is a weight map in the size of the input
//! picture; it never changes pixels.
//!
//! The depth estimate only chooses the points (far, upper part) and acts as a hard rule per segment: the picture is
//! cut into segments (`crate::segments`), and only pixels of segments whose median depth is in the farthest third
//! may be sky; there is no weight that falls off gradually with the depth. Where the segmentation cannot follow the
//! sky (seen through branches), the pixels of the far segments above the guessed horizon are taken and the dark ones
//! (branches) leave by their brightness.
//!
//! Strengths and limits (measured on test photos, see the measuring tool `graineo-dev scene-sky`): good with a clear
//! horizon, clouds and haze; weak where sky shows through foliage, with snow fields or water that mirror the sky, and
//! in pictures without sky that have a bright, even backdrop in the upper part (then the check has to say no).
use std::path::Path;

use image::RgbImage;

/// Smallest result of the check that counts as sky. Measured with SigLIP 2 Base on eleven test photos (17.11.2026):
/// pictures without sky 0.04–0.31 (one grey studio backdrop 0.91, a miss that no threshold fixes), sky 0.50–1.00.
pub const CHECK: f32 = 0.4;

/// Long side of the working grid of the guess.
const GRID: usize = 128;

/// Far-ness 0 (nearest) … 1 (farthest) from a depth estimate, robustly normalised (1st and 99th percentile), for the
/// choice of the points; and the nearness ranks (`crate::depth::nearness_ranks`) for the segments.
#[derive(Clone, Debug)]
pub struct Depth {
    pub width: u32,
    pub height: u32,
    pub far: Vec<f32>,
    pub ranks: Vec<u16>,
}

impl Depth {
    pub fn from_estimate(e: &crate::depth::Estimate) -> Self {
        let mut s = e.disparity.clone();
        s.sort_unstable_by(f32::total_cmp);
        let lo = s[s.len() / 100];
        let hi = s[(s.len() * 99) / 100];
        let span = (hi - lo).max(1e-6);
        let far = e.disparity.iter().map(|d| (1.0 - (d - lo) / span).clamp(0.0, 1.0)).collect();
        Self { width: e.width, height: e.height, far, ranks: crate::depth::nearness_ranks(&e.disparity) }
    }

    /// Per pixel of a picture of `w × h`: does it belong to a segment whose median depth is in the farthest third?
    pub fn far_segments(&self, img: &RgbImage) -> Vec<bool> {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let r: Vec<f32> = self.ranks.iter().map(|&v| v as f32).collect();
        let ranks: Vec<u16> = crate::resize(&r, self.width as usize, self.height as usize, w, h).iter().map(|v| v.round().clamp(0.0, 65535.0) as u16).collect();
        let s = crate::segments::segment(img, &ranks, &crate::segments::Params::default());
        s.labels.iter().map(|&l| masks::raster::split_label(l).is_some_and(|c| c.0 == masks::DepthClass::Far)).collect()
    }
}

/// Result: weights 0..1 in the size of the input (`None`: no sky found) and a short note for the measurement.
#[derive(Clone, Debug)]
pub struct Found {
    pub weights: Option<Vec<f32>>,
    pub note: String,
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn luma(p: &image::Rgb<u8>) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// Guess per grid cell (0..1) and the grid size.
pub fn guess(img: &RgbImage, depth: Option<&Depth>) -> (Vec<f32>, usize, usize) {
    let s = GRID as f32 / img.width().max(img.height()) as f32;
    let (gw, gh) = (((img.width() as f32 * s).round() as usize).max(1), ((img.height() as f32 * s).round() as usize).max(1));
    let small = image::imageops::resize(img, gw as u32, gh as u32, image::imageops::FilterType::Triangle);
    let mut l: Vec<f32> = small.pixels().map(luma).collect();
    l.sort_unstable_by(f32::total_cmp);
    let (p40, p85) = (l[l.len() * 40 / 100], l[l.len() * 85 / 100]);
    let far = depth.map(|d| crate::resize(&d.far, d.width as usize, d.height as usize, gw, gh));
    let lum: Vec<f32> = small.pixels().map(luma).collect();
    let mut out = vec![0.0; gw * gh];
    for y in 0..gh {
        let top = 1.0 - smooth(0.45, 0.92, (y as f32 + 0.5) / gh as f32);
        for x in 0..gw {
            let p = small.get_pixel(x as u32, y as u32);
            let bright = smooth(p40, p85.max(p40 + 1.0), luma(p));
            let blue = ((p[2] as f32 - p[0] as f32) / 40.0 + 0.5).clamp(0.0, 1.0);
            // sky is smooth at this scale: largest difference to the 8 neighbours (rock, snow and foliage are not)
            let i = y * gw + x;
            let mut rough = 0.0f32;
            for (dx, dy) in [(-1i32, -1i32), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx >= 0 && ny >= 0 && (nx as usize) < gw && (ny as usize) < gh {
                    rough = rough.max((lum[ny as usize * gw + nx as usize] - lum[i]).abs());
                }
            }
            let flat = 1.0 - smooth(6.0, 18.0, rough);
            out[i] = top
                * (0.5 + 0.5 * flat)
                * match &far {
                    Some(f) => smooth(0.85, 0.97, f[i]) * (0.6 + 0.4 * bright.max(blue)),
                    None => bright * (0.6 + 0.4 * blue),
                };
        }
    }
    (out, gw, gh)
}

/// Cells of `guess > 0.5` connected (4-neighbourhood) to the top row.
pub fn top_region(g: &[f32], w: usize, h: usize) -> Vec<bool> {
    top_region_at(g, w, h, 0.5)
}

/// [`top_region`] with another threshold.
pub fn top_region_at(g: &[f32], w: usize, h: usize, t: f32) -> Vec<bool> {
    let mut seen = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w).filter(|&x| g[x] > t).collect();
    for &i in &stack {
        seen[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut push = |j: usize| {
            if !seen[j] && g[j] > t {
                seen[j] = true;
                stack.push(j);
            }
        };
        if x > 0 {
            push(i - 1);
        }
        if x + 1 < w {
            push(i + 1);
        }
        if y > 0 {
            push(i - w);
        }
        if y + 1 < h {
            push(i + w);
        }
    }
    seen
}

/// Distance (in cells, chessboard metric) of every cell to the nearest cell outside `r` (or the picture edge counts
/// as inside for the top row: the sky goes on above the frame).
fn inner_distance(r: &[bool], w: usize, h: usize) -> Vec<u32> {
    let big = u32::MAX / 2;
    let mut d: Vec<u32> = r.iter().map(|&v| if v { big } else { 0 }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let mut m = d[i];
            // left and lower picture edges limit; the top edge does not
            m = m.min(if x == 0 { 1 } else { d[i - 1] + 1 });
            if y > 0 {
                m = m.min(d[i - w] + 1);
                m = m.min(if x == 0 { 1 } else { d[i - w - 1] + 1 });
                m = m.min(if x + 1 == w { 1 } else { d[i - w + 1] + 1 });
            }
            d[i] = m;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let mut m = d[i];
            m = m.min(if x + 1 == w { 1 } else { d[i + 1] + 1 });
            m = m.min(if y + 1 == h { 1 } else { d[i + w] + 1 });
            if y + 1 < h {
                m = m.min(if x == 0 { 1 } else { d[i + w - 1] + 1 });
                m = m.min(if x + 1 == w { 1 } else { d[i + w + 1] + 1 });
            }
            d[i] = m;
        }
    }
    d
}

/// Click points for the segmentation: up to four inside the region (deepest cell per quarter of the width), two
/// outside (deepest cell of the lower part that is surely not sky). Normalised x, y and +1 / −1.
pub fn points(region: &[bool], g: &[f32], w: usize, h: usize) -> Vec<[f32; 3]> {
    let d = inner_distance(region, w, h);
    let mut out = Vec::new();
    for q in 0..4 {
        let (x0, x1) = (q * w / 4, (q + 1) * w / 4);
        let best = (0..h).flat_map(|y| (x0..x1).map(move |x| (x, y))).max_by_key(|&(x, y)| (d[y * w + x], std::cmp::Reverse(y)));
        if let Some((x, y)) = best.filter(|&(x, y)| d[y * w + x] >= 3) {
            out.push([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32, 1.0]);
        }
    }
    let not: Vec<bool> = g.iter().zip(region).map(|(v, r)| !r && *v < 0.1).collect();
    let dn = inner_distance(&not, w, h);
    for half in 0..2 {
        let (x0, x1) = (half * w / 2, (half + 1) * w / 2);
        let best = (h / 2..h).flat_map(|y| (x0..x1).map(move |x| (x, y))).max_by_key(|&(x, y)| dn[y * w + x]);
        if let Some((x, y)) = best.filter(|&(x, y)| dn[y * w + x] >= 3) {
            out.push([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32, -1.0]);
        }
    }
    out
}

/// Find the sky in an upright 8-bit sRGB picture. `sam`: folder of the click selection module; `depth`: far-ness from
/// a depth estimate (much more reliable with it); `score`: a check 0..1 that a picture region is sky (region on grey),
/// below [`CHECK`] means "no sky". Steps are reported as for the other models.
pub fn find(sam: &Path, img: &RgbImage, depth: Option<&Depth>, score: Option<&dyn Fn(&RgbImage) -> f32>, step: &mut dyn FnMut(crate::Step) -> bool) -> Result<Found, String> {
    let (g, gw, gh) = guess(img, depth);
    let mut region = top_region(&g, gw, gh);
    let mut share = region.iter().filter(|v| **v).count() as f32 / region.len() as f32;
    if share < 0.02 {
        // sky seen through branches: the cells are mixed, the guess lower; one more try with a lower threshold
        region = top_region_at(&g, gw, gh, 0.3);
        share = region.iter().filter(|v| **v).count() as f32 / region.len() as f32;
    }
    if share < 0.02 {
        return Ok(Found { weights: None, note: format!("no region ({:.1} %)", share * 100.0) });
    }
    let pts = points(&region, &g, gw, gh);
    if !pts.iter().any(|p| p[2] > 0.0) {
        return Ok(Found { weights: None, note: "region too thin".into() });
    }
    let enc = crate::sam::encode_steps(sam, img, step)?;
    let m = crate::sam::decode(sam, &enc, &pts)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut weights = crate::resize(&m, enc.width as usize, enc.height as usize, w, h);
    // hard rule per segment: only far segments may be sky
    let far_seg = depth.map(|d| d.far_segments(img));
    // agreement with the guess: a segmentation that grabbed something else is not used
    let r_full: Vec<f32> = crate::resize(&region.iter().map(|&v| if v { 1.0 } else { 0.0 }).collect::<Vec<f32>>(), gw, gh, w, h);
    let (mut inter, mut uni) = (0usize, 0usize);
    for (a, b) in weights.iter().zip(&r_full) {
        inter += usize::from(*a > 0.5 && *b > 0.5);
        uni += usize::from(*a > 0.5 || *b > 0.5);
    }
    let agree = inter as f32 / uni.max(1) as f32;
    if agree < 0.4 {
        weights = match &far_seg {
            // sky through branches: the segmentation cannot follow them. The pixels down to the lowest row of the
            // guessed region (plus a margin, a spatial fall-off) count if they are about as bright as the sky there:
            // the dark branches leave by their brightness (here and in `refine_dark` below), near segments by the
            // segment rule below
            Some(_) => {
                let lowest = (0..gh).rev().find(|&y| (0..gw).any(|x| region[y * gw + x])).unwrap_or(0) as f32 / gh as f32;
                let lum: Vec<f32> = img.pixels().map(luma).collect();
                let inside: Vec<f32> = (0..w * h).filter(|&i| r_full[i] > 0.5).map(|i| lum[i]).collect();
                let bright = bright_sky(&inside);
                (0..w * h)
                    .map(|i| {
                        let below = smooth(lowest + 0.05, lowest + 0.15, (i / w) as f32 / h as f32);
                        if lum[i] >= bright { 1.0 - below } else { 0.0 }
                    })
                    .collect()
            }
            // fall back on the guess itself, softly upsampled
            None => r_full,
        };
    }
    refine_dark(img, &mut weights);
    // the check looks at the selection before the segment rule (is this region sky at all?)
    let checked = score.map(|f| f(&masked(img, &weights)));
    if let Some(far) = &far_seg {
        // near things inside the selection (branches, a roof, a person against the sky) leave it: whole segments that
        // are not in the farthest third
        for (v, f) in weights.iter_mut().zip(far) {
            if !f {
                *v = 0.0;
            }
        }
    }
    let note = format!("region {:.0} %, {} points, agreement {:.2}", share * 100.0, pts.len(), agree);
    if let Some(s) = checked {
        if s < CHECK {
            return Ok(Found { weights: None, note: format!("{note}, check {s:.2}: not sky") });
        }
        return Ok(Found { weights: Some(weights), note: format!("{note}, check {s:.2}") });
    }
    Ok(Found { weights: Some(weights), note })
}

/// Brightness a pixel of the sky seen through branches needs: 30 % of the upper quartile of the brightness in the guessed
/// sky (the branches are much darker, the sky between them about as bright as elsewhere).
fn bright_sky(lum: &[f32]) -> f32 {
    if lum.is_empty() {
        return 0.0;
    }
    let mut v = lum.to_vec();
    let k = v.len() * 3 / 4;
    0.3 * *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// Inside the mask, pixels much darker than the sky around them (branches, wires, roofs) are taken out: the sky's
/// brightness is the mean of the selected pixels in a window of 1/16 of the long side; a pixel below 55 % of it loses
/// its weight smoothly. A gradient (dusk sky darker at the top) stays, because the window follows it.
pub fn refine_dark(img: &RgbImage, w8: &mut [f32]) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let r = (w.max(h) / 32).max(2);
    let lum: Vec<f32> = img.pixels().map(luma).collect();
    let sel: Vec<f32> = w8.iter().map(|v| if *v > 0.5 { 1.0 } else { 0.0 }).collect();
    let weighted: Vec<f32> = lum.iter().zip(&sel).map(|(l, s)| l * s).collect();
    let (a, b) = (box_blur(&weighted, w, h, r), box_blur(&sel, w, h, r));
    for i in 0..w * h {
        if w8[i] > 0.0 && b[i] > 0.05 {
            let local = a[i] / b[i];
            w8[i] *= smooth(0.40 * local, 0.55 * local, lum[i]);
        }
    }
}

/// Box blur (mean over a square of side 2r+1, clamped at the edges), separable with running sums.
fn box_blur(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let line = |src: &[f32], n: usize, stride: usize, start: usize, out: &mut [f32]| {
        let at = |i: isize| src[start + i.clamp(0, n as isize - 1) as usize * stride];
        let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
        let k = (2 * r + 1) as f32;
        for i in 0..n {
            out[start + i * stride] = sum / k;
            sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
        }
    };
    let mut t = vec![0.0; w * h];
    for y in 0..h {
        line(v, w, 1, y * w, &mut t);
    }
    let mut out = vec![0.0; w * h];
    for x in 0..w {
        line(&t, h, w, x, &mut out);
    }
    out
}

/// The selected region on neutral grey, cut to its bounding box (input of the check).
pub fn masked(img: &RgbImage, w8: &[f32]) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if w8[(y * w + x) as usize] > 0.5 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x1 < x0 {
        return RgbImage::from_pixel(8, 8, image::Rgb([128, 128, 128]));
    }
    RgbImage::from_fn(x1 - x0 + 1, y1 - y0 + 1, |x, y| {
        let (sx, sy) = (x + x0, y + y0);
        let a = w8[(sy * w + sx) as usize];
        let p = img.get_pixel(sx, sy);
        image::Rgb([0, 1, 2].map(|c| (p[c] as f32 * a + 128.0 * (1.0 - a)) as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn landscape() -> (RgbImage, Depth) {
        // blue sky above a dark hill whose top line is a slope from 40 % to 60 % of the height
        let (w, h) = (256u32, 170u32);
        let line = |x: u32| (0.4 + 0.2 * x as f32 / w as f32) * h as f32;
        let img = RgbImage::from_fn(w, h, |x, y| if (y as f32) < line(x) { image::Rgb([110, 160, 235]) } else { image::Rgb([60, 70, 40]) });
        let far: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| if (y as f32) < line(x) { 1.0 } else { 0.6 * (1.0 - y as f32 / h as f32) })).collect();
        let ranks = far.iter().map(|f| (f * 65535.0) as u16).collect();
        (img, Depth { width: w, height: h, far, ranks })
    }

    #[test]
    fn guess_finds_the_sky_of_a_synthetic_landscape() {
        let (img, d) = landscape();
        for depth in [Some(&d), None] {
            let (g, gw, gh) = guess(&img, depth);
            let r = top_region(&g, gw, gh);
            // top left is sky, bottom right is not
            assert!(r[gw + 2], "{depth:?}");
            assert!(!r[(gh - 2) * gw + gw - 2]);
            let p = points(&r, &g, gw, gh);
            assert!(p.iter().filter(|p| p[2] > 0.0).count() >= 2, "{p:?}");
            assert!(p.iter().all(|p| (p[2] > 0.0) == (p[1] < 0.4 + 0.2 * p[0])), "{p:?}");
        }
    }

    #[test]
    fn no_sky_in_a_dark_picture_without_depth_cue() {
        let img = RgbImage::from_pixel(200, 140, image::Rgb([40, 40, 40]));
        let far = vec![0.2; 200 * 140];
        let (g, gw, gh) = guess(&img, Some(&Depth { width: 200, height: 140, far, ranks: vec![13107; 200 * 140] }));
        assert!(top_region(&g, gw, gh).iter().all(|v| !v));
    }

    #[test]
    fn dark_branches_leave_the_mask() {
        let img = RgbImage::from_fn(64, 64, |x, _| if x % 8 == 0 { image::Rgb([20, 20, 20]) } else { image::Rgb([200, 210, 230]) });
        let mut w = vec![1.0; 64 * 64];
        refine_dark(&img, &mut w);
        assert_eq!(w[0], 0.0);
        assert_eq!(w[1], 1.0);
    }

    /// Needs the click selection module: GRAINEO_TEST_MOBILESAM=<folder> (skipped otherwise).
    #[test]
    fn sky_of_a_synthetic_landscape_with_the_segmentation() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_MOBILESAM").map(std::path::PathBuf::from) else { return };
        let (img, d) = landscape();
        let f = find(&dir, &img, Some(&d), None, &mut |_| true).unwrap();
        let w = f.weights.expect("sky");
        let (iw, ih) = (img.width() as usize, img.height() as usize);
        let line = |x: usize| (0.4 + 0.2 * x as f32 / iw as f32) * ih as f32;
        let wrong = (0..ih).flat_map(|y| (0..iw).map(move |x| (x, y))).filter(|&(x, y)| (w[y * iw + x] > 0.5) != ((y as f32) < line(x))).count();
        assert!(wrong < iw * ih / 50, "{wrong} wrong pixels; {}", f.note);
    }
}
