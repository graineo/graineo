//! Segments of a photo for the depth mask. The picture is cut into regions along colour and depth edges, each region
//! gets the median of its nearness ranks, and a depth mask then takes or leaves whole regions. There is no function
//! over the depth with a gradual transition between two depths: a region is in or out, and a soft edge only comes
//! later and only in space (feather of the raster leaf).
//!
//! Construction (classic building blocks only):
//! 1. Gradient image: Sobel magnitude of the colours (largest of R, G, B) plus the Sobel magnitude of the nearness
//!    ranks (weighted, so depth steps count as edges even between equal colours), after a 3 × 3 box blur.
//! 2. Marker-controlled watershed (Beucher & Lantuéjoul 1979, flooding as in Meyer 1991): markers on a regular grid,
//!    each moved to the lowest gradient in its neighbourhood, flood the gradient image in order of increasing value;
//!    regions meet on the ridges, i.e. on the edges of the picture.
//! 3. Region merging (textbook region adjacency graph): neighbouring regions merge, cheapest first, while their mean
//!    depth and mean colour are close and the pass between them (lowest gradient on their common border, as in
//!    hierarchical watersheds) is low. A merged region may not span more
//!    than [`DEPTH_SPAN`] of the depth ranks, so a floor or a landscape receding into the distance stays cut into
//!    bands and "near" can take its near part. Tiny regions merge into their cheapest neighbour at the end.
//! 4. Per region the median nearness rank and its class (nearest, middle or farthest third of the pixels, see
//!    `masks::DepthClass::of_rank`); the label encodes class and number (`masks::raster::segment_label`).
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use image::RgbImage;
use masks::raster::{segment_label, SEGMENTS_MAX};
use masks::DepthClass;

/// Largest span of the mean depth ranks (0..1) of the parts of one merged region.
pub const DEPTH_SPAN: f32 = 0.12;

/// A segment map: one label per pixel (class and number, see `masks::raster::segment_label`) and per segment its
/// median nearness rank and pixel count.
#[derive(Clone, Debug)]
pub struct Segments {
    pub width: u32,
    pub height: u32,
    pub labels: Vec<u16>,
    pub median: Vec<u16>,
    pub size: Vec<u32>,
}

impl Segments {
    pub fn count(&self) -> usize {
        self.median.len()
    }
}

/// Settings of the segmentation (the defaults are the ones the app uses).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// distance of the markers in pixels
    pub step: usize,
    /// weight of the depth gradient against the colour gradient
    pub depth_weight: f32,
    /// merge while the cost is below 1; cost = (Δdepth/d)² + (Δcolour/c)² + (border/b)² with these scales
    pub depth_scale: f32,
    pub color_scale: f32,
    pub border_scale: f32,
    /// regions below this share of the pixels merge into a neighbour in any case
    pub min_share: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self { step: 14, depth_weight: 3.0, depth_scale: 0.045, color_scale: 0.10, border_scale: 0.12, min_share: 0.0015 }
    }
}

fn box3(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut t = vec![0.0; v.len()];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(1), (x + 1).min(w - 1));
            t[y * w + x] = (v[y * w + a] + v[y * w + x] + v[y * w + b]) / 3.0;
        }
    }
    let mut out = vec![0.0; v.len()];
    for y in 0..h {
        let (a, b) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            out[y * w + x] = (t[a * w + x] + t[y * w + x] + t[b * w + x]) / 3.0;
        }
    }
    out
}

/// Sobel magnitude (normalised so that a step of 1 gives about 1), edges clamped.
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

/// Marker-controlled watershed: `markers` (pixel indices) flood `g` in order of increasing value (ties: first come);
/// returns a region number (0-based, the marker's index) per pixel.
pub fn watershed(g: &[f32], w: usize, h: usize, markers: &[usize]) -> Vec<u32> {
    const NONE: u32 = u32::MAX;
    let mut label = vec![NONE; w * h];
    let mut heap = BinaryHeap::new();
    let key = |v: f32| (v.max(0.0) * 65536.0).min(u32::MAX as f32 / 2.0) as u32;
    let mut order = 0u64;
    for (i, &m) in markers.iter().enumerate() {
        if label[m] == NONE {
            label[m] = i as u32;
            heap.push(Reverse((key(g[m]), order, m)));
            order += 1;
        }
    }
    while let Some(Reverse((_, _, p))) = heap.pop() {
        let l = label[p];
        let (x, y) = (p % w, p / w);
        let mut visit = |q: usize| {
            if label[q] == NONE {
                label[q] = l;
                heap.push(Reverse((key(g[q]), order, q)));
                order += 1;
            }
        };
        if x > 0 {
            visit(p - 1);
        }
        if x + 1 < w {
            visit(p + 1);
        }
        if y > 0 {
            visit(p - w);
        }
        if y + 1 < h {
            visit(p + w);
        }
    }
    label
}

struct Region {
    size: f64,
    depth: f64,
    color: [f64; 3],
    /// lowest and highest mean depth of the merged parts
    span: [f32; 2],
    /// neighbours: border length and pass value (lowest gradient on the border, the saddle between the two basins)
    next: HashMap<u32, (f64, f64)>,
    alive: bool,
    version: u32,
}

impl Region {
    fn mean_depth(&self) -> f32 {
        (self.depth / self.size) as f32
    }
    fn mean_color(&self) -> [f32; 3] {
        self.color.map(|c| (c / self.size) as f32)
    }
}

fn cost(a: &Region, b: &Region, border: (f64, f64), p: &Params) -> f32 {
    let dd = (a.mean_depth() - b.mean_depth()) / p.depth_scale;
    let (ca, cb) = (a.mean_color(), b.mean_color());
    let dc = ((ca[0] - cb[0]).powi(2) + (ca[1] - cb[1]).powi(2) + (ca[2] - cb[2]).powi(2)).sqrt() / p.color_scale;
    let db = border.1 as f32 / p.border_scale;
    dd * dd + dc * dc + db * db
}

/// Cut an image (8-bit sRGB) and its nearness ranks (0 = nearest … 65535 = farthest, same size) into segments.
pub fn segment(img: &RgbImage, ranks: &[u16], p: &Params) -> Segments {
    let (w, h) = (img.width() as usize, img.height() as usize);
    assert_eq!(ranks.len(), w * h, "ranks in the size of the image");
    let chan = |c: usize| -> Vec<f32> { img.pixels().map(|q| q[c] as f32 / 255.0).collect() };
    // plain colours for the region means (a blur would mix the colours of both sides into regions along an edge),
    // blurred ones for the gradient
    let plain = [chan(0), chan(1), chan(2)];
    let colors = [box3(&plain[0], w, h), box3(&plain[1], w, h), box3(&plain[2], w, h)];
    let depth: Vec<f32> = ranks.iter().map(|&r| r as f32 / 65535.0).collect();
    let depth_s = box3(&depth, w, h);
    // 1. gradient image
    let gc: Vec<Vec<f32>> = colors.iter().map(|c| sobel(c, w, h)).collect();
    let gd = sobel(&depth_s, w, h);
    let g: Vec<f32> = (0..w * h).map(|i| gc[0][i].max(gc[1][i]).max(gc[2][i]) + p.depth_weight * gd[i]).collect();
    // 2. markers on a grid, each at the lowest gradient of its cell's middle, then the watershed
    let step = p.step.max(2);
    let mut markers = Vec::new();
    let r = step / 4;
    for cy in (step / 2..h).step_by(step) {
        for cx in (step / 2..w).step_by(step) {
            let mut best = cy * w + cx;
            for y in cy.saturating_sub(r)..=(cy + r).min(h - 1) {
                for x in cx.saturating_sub(r)..=(cx + r).min(w - 1) {
                    if g[y * w + x] < g[best] {
                        best = y * w + x;
                    }
                }
            }
            markers.push(best);
        }
    }
    if markers.is_empty() {
        markers.push(0);
    }
    let first = watershed(&g, w, h, &markers);
    // 3. region adjacency graph
    let n = markers.len();
    let mut regions: Vec<Region> = (0..n).map(|_| Region { size: 0.0, depth: 0.0, color: [0.0; 3], span: [0.0; 2], next: HashMap::new(), alive: true, version: 0 }).collect();
    for i in 0..w * h {
        let r = &mut regions[first[i] as usize];
        r.size += 1.0;
        r.depth += depth[i] as f64;
        for c in 0..3 {
            r.color[c] += plain[c][i] as f64;
        }
    }
    for r in &mut regions {
        let m = r.mean_depth();
        r.span = [m, m];
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            for j in [(x + 1 < w).then(|| i + 1), (y + 1 < h).then(|| i + w)].into_iter().flatten() {
                let (a, b) = (first[i], first[j]);
                if a != b {
                    let edge = g[i].max(g[j]) as f64;
                    for (u, v) in [(a, b), (b, a)] {
                        let e = regions[u as usize].next.entry(v).or_insert((0.0, f64::INFINITY));
                        e.0 += 1.0;
                        e.1 = e.1.min(edge);
                    }
                }
            }
        }
    }
    // union-find over the first regions
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            parent[i as usize] = parent[parent[i as usize] as usize];
            i = parent[i as usize];
        }
        i
    }
    let merge = |regions: &mut Vec<Region>, parent: &mut Vec<u32>, a: u32, b: u32| {
        // b goes into a
        let rb = std::mem::replace(&mut regions[b as usize].next, HashMap::new());
        let (size, depth, color, span) = { let r = &regions[b as usize]; (r.size, r.depth, r.color, r.span) };
        regions[b as usize].alive = false;
        {
            let ra = &mut regions[a as usize];
            ra.size += size;
            ra.depth += depth;
            for c in 0..3 {
                ra.color[c] += color[c];
            }
            ra.span = [ra.span[0].min(span[0]), ra.span[1].max(span[1])];
            ra.next.remove(&b);
            ra.version += 1;
        }
        for (nb, e) in rb {
            if nb == a {
                continue;
            }
            let other = &mut regions[nb as usize];
            other.next.remove(&b);
            let f = other.next.entry(a).or_insert((0.0, f64::INFINITY));
            f.0 += e.0;
            f.1 = f.1.min(e.1);
            let f = *f;
            regions[a as usize].next.insert(nb, f);
        }
        parent[b as usize] = a;
    };
    let total = (w * h) as f64;
    // cheapest merges first; entries carry the versions of both regions, stale ones are skipped
    let mut heap: BinaryHeap<Reverse<(u32, u32, u32, u32, u32)>> = BinaryHeap::new();
    let push = |heap: &mut BinaryHeap<Reverse<(u32, u32, u32, u32, u32)>>, regions: &[Region], a: u32, b: u32| {
        let (ra, rb) = (&regions[a as usize], &regions[b as usize]);
        let Some(&border) = ra.next.get(&b) else { return };
        let c = cost(ra, rb, border, p);
        if c < 1.0 && ra.span[0].min(rb.span[0]) + DEPTH_SPAN >= ra.span[1].max(rb.span[1]) {
            heap.push(Reverse(((c * 1e6) as u32, a, b, ra.version, rb.version)));
        }
    };
    for a in 0..n as u32 {
        let nb: Vec<u32> = regions[a as usize].next.keys().copied().filter(|&b| b > a).collect();
        for b in nb {
            push(&mut heap, &regions, a, b);
        }
    }
    while let Some(Reverse((_, a, b, va, vb))) = heap.pop() {
        if !regions[a as usize].alive || !regions[b as usize].alive || regions[a as usize].version != va || regions[b as usize].version != vb {
            continue;
        }
        let (keep, gone) = if regions[a as usize].size >= regions[b as usize].size { (a, b) } else { (b, a) };
        merge(&mut regions, &mut parent, keep, gone);
        let nb: Vec<u32> = regions[keep as usize].next.keys().copied().collect();
        for o in nb {
            push(&mut heap, &regions, keep, o);
        }
    }
    // tiny regions into their cheapest neighbour (smallest first)
    let min = (p.min_share as f64 * total).max(4.0);
    loop {
        let small = (0..n).filter(|&i| regions[i].alive && regions[i].size < min && !regions[i].next.is_empty()).min_by(|&a, &b| regions[a].size.total_cmp(&regions[b].size));
        let Some(s) = small else { break };
        let best = regions[s].next.iter().map(|(&o, &e)| (o, cost(&regions[s], &regions[o as usize], e, p))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        let Some(o) = best else { break };
        merge(&mut regions, &mut parent, o, s as u32);
    }
    // 4. final numbering, median rank and class per segment
    let mut number = vec![u32::MAX; n];
    let mut count = 0u32;
    for i in 0..n {
        if regions[i].alive {
            number[i] = count;
            count += 1;
        }
    }
    let mut seg = vec![0u32; w * h];
    let mut cache = vec![u32::MAX; n];
    for i in 0..w * h {
        let f = first[i] as usize;
        if cache[f] == u32::MAX {
            cache[f] = number[find(&mut parent, f as u32) as usize];
        }
        seg[i] = cache[f];
    }
    let count = count as usize;
    let mut per: Vec<Vec<u16>> = vec![Vec::new(); count];
    for i in 0..w * h {
        per[seg[i] as usize].push(ranks[i]);
    }
    let median: Vec<u16> = per
        .iter_mut()
        .map(|v| {
            if v.is_empty() {
                return 0;
            }
            let k = v.len() / 2;
            *v.select_nth_unstable(k).1
        })
        .collect();
    let size: Vec<u32> = per.iter().map(|v| v.len() as u32).collect();
    // more segments than the label has room for cannot happen with the grid (at most one per marker), but the last
    // ones would share a number then
    let labels = seg.iter().map(|&s| segment_label(DepthClass::of_rank(median[s as usize]), (s as usize).min(SEGMENTS_MAX - 1) as u16)).collect();
    Segments { width: w as u32, height: h as u32, labels, median, size }
}

#[cfg(test)]
mod tests {
    use super::*;
    use masks::raster::split_label;

    /// A red disc in front of a grey wall, the wall in front of a blue sky: three depths, three colours.
    fn scene() -> (RgbImage, Vec<u16>) {
        let (w, h) = (240u32, 160u32);
        let img = RgbImage::from_fn(w, h, |x, y| {
            let d = ((x as f32 - 120.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
            if d < 35.0 {
                image::Rgb([200, 50, 40])
            } else if y < 50 {
                image::Rgb([120, 160, 230])
            } else {
                image::Rgb([128, 128, 120])
            }
        });
        let ranks = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let d = ((x as f32 - 120.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
                if d < 35.0 { 5000 } else if y < 50 { 60000 } else { 33000 }
            })
            .collect();
        (img, ranks)
    }

    #[test]
    fn watershed_meets_on_the_ridge() {
        // a ridge at x = 10 between two markers
        let (w, h) = (21, 5);
        let g: Vec<f32> = (0..w * h).map(|i| if i % w == 10 { 1.0 } else { 0.0 }).collect();
        let l = watershed(&g, w, h, &[2 * w + 2, 2 * w + 18]);
        assert!((0..h).all(|y| l[y * w + 9] == 0 && l[y * w + 11] == 1));
    }

    #[test]
    fn a_scene_falls_into_its_objects_with_their_classes() {
        let (img, ranks) = scene();
        let s = segment(&img, &ranks, &Params::default());
        assert!(s.count() >= 3 && s.count() <= 12, "{} segments", s.count());
        let at = |x: usize, y: usize| split_label(s.labels[y * 240 + x]).unwrap();
        let (disc, wall, sky) = (at(120, 100), at(30, 140), at(30, 10));
        assert_eq!((disc.0, wall.0, sky.0), (DepthClass::Near, DepthClass::Middle, DepthClass::Far));
        // each object is one segment, different from the others
        assert!(disc.1 != wall.1 && wall.1 != sky.1 && disc.1 != sky.1);
        assert_eq!(at(140, 110).1, disc.1);
        assert_eq!(at(220, 150).1, wall.1);
        assert_eq!(at(200, 5).1, sky.1);
        // whole segments: every pixel of the disc (away from its edge) carries the disc's label
        let inside = (0..240 * 160).filter(|&i| ((i % 240) as f32 - 120.0).hypot((i / 240) as f32 - 100.0) < 30.0);
        let bad: Vec<_> = inside.clone().filter(|&i| split_label(s.labels[i]).unwrap().1 != disc.1).map(|i| (i % 240, i / 240, split_label(s.labels[i]))).take(10).collect();
        assert!(bad.is_empty(), "{bad:?} sizes {:?} medians {:?}", s.size, s.median);
        assert_eq!(s.size.iter().sum::<u32>(), 240 * 160);
    }

    #[test]
    fn a_receding_floor_is_cut_into_depth_bands() {
        // one colour, depth rising from the bottom (near) to the top (far): no single segment may span it all
        let (w, h) = (200u32, 200u32);
        let img = RgbImage::from_pixel(w, h, image::Rgb([110, 100, 90]));
        let ranks: Vec<u16> = (0..w * h).map(|i| (65535.0 * (1.0 - (i / w) as f32 / (h - 1) as f32)) as u16).collect();
        let s = segment(&img, &ranks, &Params::default());
        let classes: std::collections::HashSet<_> = s.labels.iter().map(|&l| split_label(l).unwrap().0).collect();
        assert_eq!(classes.len(), 3, "near, middle and far bands");
        assert!(s.count() >= 3);
    }
}
