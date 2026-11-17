//! "Kante verfeinern": the edge of a mask follows the edges of the picture, in a narrow band around the edge only.
//! Two methods, chosen by what the mask is made of:
//!
//! - **Drawn masks and ranges** (brush, gradient, ellipse, brightness and colour ranges): a guided filter (He, Sun &
//!   Tang, "Guided Image Filtering", ECCV 2010) with the brightness of the picture the layer sees as guide, built from
//!   box means only (as the guided filter of the tone equaliser). Where the mask is flat (all 0 or all 1 in the
//!   window) the filter changes nothing, so it only acts in the band around the edge.
//! - **Masks with a raster from an image analysis** (subject, people, click selection, depth segments, sky, person
//!   parts): no guided filter (patent review: automatic segmentation + guided filter + separate adjustment is claimed).
//!   **Not used by the program**: [`snap`] below was built as the replacement and measured (`graineo-dev
//!   mask-refine`, test photos with sky against a reference): it brought no gain (the strongest edge in the band is
//!   often a cloud or colour edge instead of the object's edge; boundary F-score and IoU fell), so such masks keep
//!   their edge and the slider is locked for them. The construction, kept for the measurement: sure inside and sure outside are the
//!   mask eroded by the band radius; the band between them is split by a marker-controlled watershed on the edge
//!   strength (Sobel magnitude of the guide's brightness), so the new edge lies on the strongest image edge in the
//!   band. How far the result replaces the mask depends on the edge strength found nearby (feathering by edge
//!   strength): without a clear image edge the mask stays as it was. No matting, no solver, no guided filter.
//!
//! Both work on the weights of the whole layer at the resolution it is rendered in; the processor computes them, the
//! GPU gets the refined weights (same numbers on both).
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Band radius at full strength: share of the short image side.
pub const RADIUS: f32 = 0.02;
/// Regularisation of the guided filter (guide: sRGB-encoded brightness 0..1).
const EPS: f32 = 2.5e-4;
/// Edge strength (Sobel magnitude of the encoded brightness) that counts as a clear edge.
const STRONG: f32 = 0.06;

/// Band radius in pixels for `amount` (0..1) of a picture whose short side is `short` pixels.
pub fn radius(amount: f32, short: usize) -> usize {
    ((amount.clamp(0.0, 1.0) * RADIUS * short as f32).round() as usize).max(2)
}

/// Reach in pixels outside a crop that [`refine`] reads (for the margin of crops).
pub fn reach(amount: f32, short: usize) -> usize {
    if amount > 0.0 { 4 * radius(amount, short) + 2 } else { 0 }
}

/// Brightness of the guide: sRGB-encoded Rec.709 luminance, clamped to 0..1.5.
fn guide(rgb: &[f32]) -> Vec<f32> {
    rgb.chunks_exact(3).map(|p| crate::correction::encode(crate::luma(p).max(0.0)).min(1.5)).collect()
}

/// Mean over a square of side `2r + 1` (clamped edges), separable box filter.
/// Running sums in f64, so that a flat window gives exactly its value (a crop and the whole picture agree, and the
/// flat parts are recognised exactly).
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    use rayon::prelude::*;
    let line = |src: &[f32], out: &mut [f32]| box_line(src, out, r);
    let mut rows = vec![0.0f32; v.len()];
    rows.par_chunks_mut(w).zip(v.par_chunks(w)).for_each(|(o, s)| line(s, o));
    // columns through the transposed picture
    let mut t = vec![0.0f32; v.len()];
    t.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, c) in col.iter_mut().enumerate() {
            *c = rows[y * w + x];
        }
    });
    let mut tc = vec![0.0f32; v.len()];
    tc.par_chunks_mut(h).zip(t.par_chunks(h)).for_each(|(o, s)| line(s, o));
    let mut out = vec![0.0f32; v.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, c) in row.iter_mut().enumerate() {
            *c = tc[x * h + y];
        }
    });
    out
}

/// Refine the weights `wt` (`w × h`) of a layer: `rgb` is the picture the layer sees (linear RGB, same size),
/// `short` the short side of the whole picture, `amount` 0..1, `analysis`: use the own edge snap instead of the
/// guided filter (measurement only, see the module text).
pub fn refine(wt: &mut [f32], rgb: &[f32], w: usize, h: usize, short: usize, amount: f32, analysis: bool) {
    if amount <= 0.0 || w < 3 || h < 3 {
        return;
    }
    let r = radius(amount, short);
    let g = guide(rgb);
    if analysis {
        snap(wt, &g, w, h, r);
    } else {
        guided(wt, &g, w, h, r);
    }
}

/// Core side of the tiles of the guided filter. Each tile is computed with a margin of `2r + 1` (all the filter
/// reads), so the result is that of the whole picture (up to the rounding of the running sums, below 1e-6), while the
/// memory no longer grows with the picture: about ten fields of one tile per thread instead of fourteen of the whole
/// picture (24 MP: +1.3 GB before). Tiles where the mask is flat (all 0 or all 1 within the reach) are skipped.
const TILE: usize = 512;

/// Guided filter with a grey guide: q = mean(a)·I + mean(b), a = cov(I, p) / (var(I) + ε), b = mean(p) − a·mean(I).
pub fn guided(p: &mut [f32], i: &[f32], w: usize, h: usize, r: usize) {
    use rayon::prelude::*;
    let mut tiles = Vec::new();
    for y0 in (0..h).step_by(TILE) {
        for x0 in (0..w).step_by(TILE) {
            tiles.push([x0, y0, (x0 + TILE).min(w), (y0 + TILE).min(h)]);
        }
    }
    let src: &[f32] = p;
    // tiles read the mask as it was: the results are written after all tiles are done
    let done: Vec<([usize; 4], Vec<f32>)> = tiles.par_iter().filter_map(|t| guided_tile(src, i, w, h, r, *t).map(|v| (*t, v))).collect();
    for ([x0, y0, x1, y1], v) in done {
        let cw = x1 - x0;
        for y in y0..y1 {
            p[y * w + x0..y * w + x1].copy_from_slice(&v[(y - y0) * cw..(y - y0 + 1) * cw]);
        }
    }
}

/// The guided filter for the core `[x0, y0, x1, y1]` of a tile: its new weights (row-major), `None` when the mask is
/// flat within the reach (nothing changes).
fn guided_tile(p: &[f32], i: &[f32], w: usize, h: usize, r: usize, [x0, y0, x1, y1]: [usize; 4]) -> Option<Vec<f32>> {
    // flat around the core (all 0 or all 1 within r): the mean of the mask is exactly 0 or 1 there
    let (fx0, fy0, fx1, fy1) = (x0.saturating_sub(r), y0.saturating_sub(r), (x1 + r).min(w), (y1 + r).min(h));
    let first = p[fy0 * w + fx0];
    if (first == 0.0 || first == 1.0) && (fy0..fy1).all(|y| p[y * w + fx0..y * w + fx1].iter().all(|v| *v == first)) {
        return None;
    }
    // the tile with everything the filter reads: means of means, 2r + 1 around the core
    let m = 2 * r + 1;
    let (ex0, ey0, ex1, ey1) = (x0.saturating_sub(m), y0.saturating_sub(m), (x1 + m).min(w), (y1 + m).min(h));
    let (tw, th) = (ex1 - ex0, ey1 - ey0);
    let cut = |v: &[f32]| -> Vec<f32> { (ey0..ey1).flat_map(|y| v[y * w + ex0..y * w + ex1].iter().copied()).collect() };
    let (pt, it) = (cut(p), cut(i));
    let mut boxes = Boxes::new(tw, th, r);
    let mi = boxes.mean(&it);
    let mp = boxes.mean(&pt);
    let ip: Vec<f32> = it.iter().zip(pt.iter()).map(|(a, b)| a * b).collect();
    let mut a = boxes.mean(&ip);
    drop(ip);
    let ii: Vec<f32> = it.iter().map(|a| a * a).collect();
    let mut b = boxes.mean(&ii);
    drop(ii);
    // a and b in place of the means of I·p and I·I (same arithmetic as before)
    for k in 0..a.len() {
        let var = (b[k] - mi[k] * mi[k]).max(0.0);
        let cov = a[k] - mi[k] * mp[k];
        a[k] = cov / (var + EPS);
        b[k] = mp[k] - a[k] * mi[k];
    }
    drop(mi);
    let ma = boxes.mean(&a);
    drop(a);
    let mb = boxes.mean(&b);
    drop(b);
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        for x in x0..x1 {
            let k = (y - ey0) * tw + (x - ex0);
            // where the window was flat the mask stays exactly as it was
            out.push(if mp[k] <= 0.0 || mp[k] >= 1.0 { p[y * w + x] } else { (ma[k] * it[k] + mb[k]).clamp(0.0, 1.0) });
        }
    }
    Some(out)
}

/// [`box_mean`] for one tile on one thread, with its scratch rows kept: the same arithmetic (the same running sums
/// along each row and each column), so a tile that is the whole picture gives exactly the same numbers.
struct Boxes {
    w: usize,
    h: usize,
    r: usize,
    rows: Vec<f32>,
    column: Vec<f32>,
    column_out: Vec<f32>,
}

impl Boxes {
    fn new(w: usize, h: usize, r: usize) -> Self {
        Self { w, h, r, rows: vec![0.0; w * h], column: vec![0.0; h], column_out: vec![0.0; h] }
    }

    fn mean(&mut self, v: &[f32]) -> Vec<f32> {
        let (w, h, r) = (self.w, self.h, self.r);
        for (o, s) in self.rows.chunks_mut(w).zip(v.chunks(w)) {
            box_line(s, o, r);
        }
        let mut out = vec![0.0f32; v.len()];
        for x in 0..w {
            for y in 0..h {
                self.column[y] = self.rows[y * w + x];
            }
            box_line(&self.column, &mut self.column_out, r);
            for y in 0..h {
                out[y * w + x] = self.column_out[y];
            }
        }
        out
    }
}

/// Mean over `2r + 1` values along one line (clamped ends), as a running sum in f64.
fn box_line(src: &[f32], out: &mut [f32], r: usize) {
    let n = src.len();
    let at = |i: isize| src[i.clamp(0, n as isize - 1) as usize] as f64;
    let mut sum: f64 = (-(r as isize)..=r as isize).map(at).sum();
    let k = (2 * r + 1) as f64;
    for i in 0..n {
        out[i] = (sum / k) as f32;
        sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
    }
}

/// Sobel magnitude of `v` (edges clamped), divided by 4 (a step of 1 gives about 1).
pub fn sobel(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let at = |x: isize, y: isize| v[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let mut out = vec![0.0; v.len()];
    for y in 0..h as isize {
        for x in 0..w as isize {
            let gx = at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1);
            let gy = at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1);
            out[y as usize * w + x as usize] = gx.hypot(gy) / 4.0;
        }
    }
    out
}

/// The own edge snap for masks of an image analysis (see the module text).
pub fn snap(wt: &mut [f32], g: &[f32], w: usize, h: usize, r: usize) {
    let n = w * h;
    // sure inside / outside: the two sides of the mask eroded by the band radius
    let mut inside: Vec<f32> = wt.iter().map(|v| if *v > 0.5 { 1.0 } else { 0.0 }).collect();
    let mut outside: Vec<f32> = inside.iter().map(|v| 1.0 - v).collect();
    if inside.iter().all(|v| *v == 0.0) || outside.iter().all(|v| *v == 0.0) {
        return;
    }
    crate::raster::morph(&mut inside, w, h, r, false);
    crate::raster::morph(&mut outside, w, h, r, false);
    // edge strength of the guide (after a 3 × 3 box mean against noise)
    let e = sobel(&box_mean(g, w, h, 1), w, h);
    // marker-controlled watershed in the band: both sides flood the edge strength in order of increasing value
    const FREE: u8 = 0;
    let mut side = vec![FREE; n];
    let mut heap = BinaryHeap::new();
    let key = |v: f32| (v.max(0.0) * 65536.0).min(u32::MAX as f32 / 2.0) as u32;
    let mut order = 0u64;
    for k in 0..n {
        let s = if inside[k] > 0.5 { 1 } else if outside[k] > 0.5 { 2 } else { continue };
        side[k] = s;
        // only markers next to the band need to flood
        let (x, y) = (k % w, k / w);
        let border = (x > 0 && inside[k - 1] + outside[k - 1] < 0.5)
            || (x + 1 < w && inside[k + 1] + outside[k + 1] < 0.5)
            || (y > 0 && inside[k - w] + outside[k - w] < 0.5)
            || (y + 1 < h && inside[k + w] + outside[k + w] < 0.5);
        if border {
            heap.push(Reverse((key(e[k]), order, k)));
            order += 1;
        }
    }
    while let Some(Reverse((_, _, k))) = heap.pop() {
        let s = side[k];
        let (x, y) = (k % w, k / w);
        for q in [(x > 0).then(|| k - 1), (x + 1 < w).then(|| k + 1), (y > 0).then(|| k - w), (y + 1 < h).then(|| k + w)].into_iter().flatten() {
            if side[q] == FREE {
                side[q] = s;
                heap.push(Reverse((key(e[q]), order, q)));
                order += 1;
            }
        }
    }
    // the split, smoothed by one pixel against stair steps
    let split: Vec<f32> = side.iter().map(|s| if *s == 1 { 1.0 } else { 0.0 }).collect();
    let split = box_mean(&split, w, h, 1);
    // how clear the image edge near each pixel is: largest edge strength within the band radius, 0..1
    let mut near = e.clone();
    crate::raster::morph(&mut near, w, h, r, true);
    for k in 0..n {
        // outside the band the sides are sure: nothing changes
        if inside[k] > 0.5 || outside[k] > 0.5 {
            continue;
        }
        // below a quarter of a clear edge: no edge at all (noise), the mask stays exactly as it was
        let c = ((near[k] / STRONG - 0.25) / 0.75).clamp(0.0, 1.0);
        if c > 0.0 {
            wt[k] += c * c * (3.0 - 2.0 * c) * (split[k] - wt[k]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with a vertical edge at x = 30 (dark left, bright right) and a mask whose edge sits at x = 25.
    fn step() -> (Vec<f32>, Vec<f32>, usize, usize) {
        let (w, h) = (64, 32);
        let rgb: Vec<f32> = (0..w * h).flat_map(|i| if i % w < 30 { [0.02; 3] } else { [0.6; 3] }).collect();
        let wt: Vec<f32> = (0..w * h).map(|i| if i % w < 25 { 0.0 } else { 1.0 }).collect();
        (rgb, wt, w, h)
    }

    #[test]
    fn the_snap_moves_the_edge_onto_the_image_edge() {
        let (rgb, mut wt, w, h) = step();
        refine(&mut wt, &rgb, w, h, 600, 1.0, true);
        let row = |x: usize| wt[16 * w + x];
        // r = 6 px: the band reaches the image edge, the new edge lies on it
        assert!(row(27) < 0.05 && row(28) < 0.4, "{} {}", row(27), row(28));
        assert!(row(31) > 0.95, "{}", row(31));
        // far from the band nothing changes
        assert_eq!((row(5), row(60)), (0.0, 1.0));
    }

    #[test]
    fn without_an_image_edge_the_snap_keeps_the_mask() {
        let (w, h) = (64, 32);
        let rgb = vec![0.3; w * h * 3];
        let mut wt: Vec<f32> = (0..w * h).map(|i| if i % w < 25 { 0.0 } else { 1.0 }).collect();
        let before = wt.clone();
        refine(&mut wt, &rgb, w, h, 600, 1.0, true);
        assert!(wt == before, "the mask changed without an image edge");
    }

    #[test]
    fn the_guided_filter_follows_the_image_edge_and_leaves_flat_parts() {
        let (rgb, mut wt, w, h) = step();
        // a soft mask edge around x = 25 (as a feathered brush)
        for k in 0..w * h {
            let x = (k % w) as f32;
            wt[k] = ((x - 21.0) / 8.0).clamp(0.0, 1.0);
        }
        let before = wt.clone();
        refine(&mut wt, &rgb, w, h, 600, 1.0, false);
        let row = |v: &[f32], x: usize| v[16 * w + x];
        // the dark side loses weight, the bright side gains: the transition moves towards the image edge
        assert!(row(&wt, 26) < row(&before, 26), "{} {}", row(&wt, 26), row(&before, 26));
        assert!(row(&wt, 31) > 0.95, "{}", row(&wt, 31));
        assert_eq!((row(&wt, 2), row(&wt, 62)), (0.0, 1.0), "flat parts stay");
        assert!(wt.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    /// The guided filter as it was computed before the tiles (whole picture at once), for comparison.
    fn guided_whole(p: &mut [f32], i: &[f32], w: usize, h: usize, r: usize) {
        let mi = box_mean(i, w, h, r);
        let mp = box_mean(p, w, h, r);
        let ip: Vec<f32> = i.iter().zip(p.iter()).map(|(a, b)| a * b).collect();
        let ii: Vec<f32> = i.iter().map(|a| a * a).collect();
        let mip = box_mean(&ip, w, h, r);
        let mii = box_mean(&ii, w, h, r);
        let mut a = vec![0.0; p.len()];
        let mut b = vec![0.0; p.len()];
        for k in 0..p.len() {
            let var = (mii[k] - mi[k] * mi[k]).max(0.0);
            let cov = mip[k] - mi[k] * mp[k];
            a[k] = cov / (var + EPS);
            b[k] = mp[k] - a[k] * mi[k];
        }
        let ma = box_mean(&a, w, h, r);
        let mb = box_mean(&b, w, h, r);
        for k in 0..p.len() {
            if mp[k] <= 0.0 || mp[k] >= 1.0 {
                continue;
            }
            p[k] = (ma[k] * i[k] + mb[k]).clamp(0.0, 1.0);
        }
    }

    /// The tiles give the result of the whole picture (review 1.3.0, logic M4: memory): bitwise for a picture of one
    /// tile, within 1e-6 (rounding of the running sums) across tiles, for soft and hard edges, gradients over the whole
    /// picture, flat tiles and odd sizes.
    #[test]
    fn tiles_give_the_whole_picture() {
        let noise = |k: usize| ((k as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 40) as f32 / (1u64 << 24) as f32;
        for (w, h, r) in [(300usize, 200usize, 6usize), (1300, 900, 20), (1031, 517, 3), (700, 1500, 40)] {
            let guide: Vec<f32> = (0..w * h).map(|k| {
                let (x, y) = ((k % w) as f32, (k / w) as f32);
                (0.3 + 0.4 * ((x / 37.0).sin() * (y / 23.0).cos()) + 0.2 * noise(k)).clamp(0.0, 1.5)
            }).collect();
            let masks: [Box<dyn Fn(usize, usize) -> f32>; 3] = [
                // soft disc in the middle (most tiles flat)
                Box::new(move |x, y| (1.0 - ((x as f32 - w as f32 / 2.0).hypot(y as f32 - h as f32 / 2.0) - 150.0) / 60.0).clamp(0.0, 1.0)),
                // gradient over the whole picture
                Box::new(move |x, _| x as f32 / (w - 1) as f32),
                // hard edges and noise
                Box::new(move |x, y| if (x / 97 + y / 61) % 2 == 0 { 1.0 } else if x % 13 == 0 { noise(x * 7 + y) } else { 0.0 }),
            ];
            for (n, mask) in masks.iter().enumerate() {
                let p0: Vec<f32> = (0..w * h).map(|k| mask(k % w, k / w)).collect();
                let mut whole = p0.clone();
                guided_whole(&mut whole, &guide, w, h, r);
                let mut tiled = p0.clone();
                guided(&mut tiled, &guide, w, h, r);
                let max = whole.iter().zip(&tiled).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                let same = whole.iter().zip(&tiled).filter(|(a, b)| a.to_bits() == b.to_bits()).count();
                println!("{w}×{h} r {r} mask {n}: max difference {max:e}, {} of {} bitwise equal", same, w * h);
                if w <= TILE && h <= TILE {
                    assert!(whole == tiled, "{w}×{h} mask {n}: one tile must be bitwise the same");
                }
                assert!(max < 1e-6, "{w}×{h} r {r} mask {n}: max difference {max:e}");
                assert!(same as f64 >= 0.9999 * (w * h) as f64, "{w}×{h} mask {n}: only {same} of {} bitwise equal", w * h);
                assert!(whole != p0, "mask {n} changed nothing");
            }
        }
    }

    /// Speed and memory of "Kante verfeinern" at 24 MP (run alone with `/usr/bin/time -l` for the peak memory).
    #[test]
    #[ignore]
    fn refine_24_mp() {
        let (w, h) = (6000usize, 4000usize);
        let rgb: Vec<f32> = (0..w * h).flat_map(|k| {
            let v = 0.3 + 0.3 * (((k % w) as f32 / 53.0).sin() * ((k / w) as f32 / 31.0).cos());
            [v, v * 0.9, v * 1.1]
        }).collect();
        let whole = std::env::var("GRAINEO_REFINE_WHOLE").is_ok();
        for (name, mask) in [("gradient", 0usize), ("disc", 1)] {
            let mut wt: Vec<f32> = (0..w * h).map(|k| {
                let (x, y) = ((k % w) as f32, (k / w) as f32);
                if mask == 0 { x / w as f32 } else { (1.0 - ((x - 3000.0).hypot(y - 2000.0) - 800.0) / 200.0).clamp(0.0, 1.0) }
            }).collect();
            let t = std::time::Instant::now();
            if std::env::var("GRAINEO_REFINE_NONE").is_ok() {
                // inputs only (the baseline of the memory measurement)
                std::hint::black_box(&mut wt);
            } else if whole {
                let g = guide(&rgb);
                guided_whole(&mut wt, &g, w, h, radius(1.0, h));
            } else {
                refine(&mut wt, &rgb, w, h, h, 1.0, false);
            }
            println!("refine 24 MP {name} ({}): {:.2} s", if whole { "whole picture" } else { "tiles" }, t.elapsed().as_secs_f32());
        }
    }

    #[test]
    fn zero_changes_nothing() {
        let (rgb, mut wt, w, h) = step();
        let before = wt.clone();
        refine(&mut wt, &rgb, w, h, 600, 0.0, true);
        refine(&mut wt, &rgb, w, h, 600, 0.0, false);
        assert_eq!(wt, before);
        assert_eq!(reach(0.0, 1000), 0);
        assert_eq!(radius(0.5, 4000), 40);
    }
}
