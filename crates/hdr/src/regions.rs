//! Areas taken from one frame ("Geister entfernen"): the user outlines a ghost with a brush and picks the frame that
//! shows the area best. Unlike the automatic ghost handling ([`crate::ghost`]) an area is not part of the merge: it is
//! pasted over the finished linear merge with a soft edge, from that frame warped onto the reference and divided by
//! its exposure factor. So an area can be added, changed or hidden locally without merging again.
//!
//! Method: plain alpha compositing of an aligned exposure with a feathered mask (manual ghost removal by choosing
//! the source exposure per area, as in the literature up to 2009 and in common tools).

use crate::align::Warp;
use crate::frames::Frame;
use crate::img::Lanczos;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// One brush stroke of an area. Coordinates are normalised to the crop of the reference in sensor orientation
/// (0 … 1 across the crop width and height); `radius` is a fraction of the longer crop edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaStroke {
    /// takes away from the area ("Verkleinern")
    #[serde(default)]
    pub erase: bool,
    pub radius: f32,
    pub points: Vec<[f32; 2]>,
}

/// An area taken from one frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    /// index of the source frame (position in the source list of the merge)
    pub frame: usize,
    pub strokes: Vec<AreaStroke>,
}

impl Area {
    /// Does the area cover anything (at least one stroke that adds)?
    pub fn is_empty(&self) -> bool {
        !self.strokes.iter().any(|s| !s.erase && !s.points.is_empty())
    }

    /// Bounding box in normalised crop coordinates (x0, y0, x1, y1) of the strokes that add, with their radius;
    /// `aspect` = crop width / crop height.
    pub fn bounds(&self, aspect: f32) -> Option<[f32; 4]> {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        let (ex, ey) = if aspect >= 1.0 { (1.0, aspect) } else { (1.0 / aspect, 1.0) };
        for s in self.strokes.iter().filter(|s| !s.erase) {
            for p in &s.points {
                b = [b[0].min(p[0] - s.radius * ex), b[1].min(p[1] - s.radius * ey), b[2].max(p[0] + s.radius * ex), b[3].max(p[1] + s.radius * ey)];
            }
        }
        (b[0] < b[2]).then(|| [b[0].max(0.0), b[1].max(0.0), b[2].min(1.0), b[3].min(1.0)])
    }
}

/// Areas from the brush strokes of older recipes: every stroke of frame `f` goes into the area of `f` and is taken
/// away from every other area, an erasing stroke is taken away from all – so the last stroke covering a place wins,
/// as it did before.
pub fn from_strokes(strokes: &[crate::ghost::Stroke]) -> Vec<Area> {
    let mut areas: Vec<Area> = Vec::new();
    for s in strokes.iter().filter(|s| !s.points.is_empty()) {
        let st = AreaStroke { erase: true, radius: s.radius, points: s.points.clone() };
        for a in areas.iter_mut().filter(|a| s.erase || a.frame != s.frame) {
            a.strokes.push(st.clone());
        }
        if !s.erase {
            let add = AreaStroke { erase: false, ..st };
            match areas.iter_mut().find(|a| a.frame == s.frame) {
                Some(a) => a.strokes.push(add),
                None => areas.push(Area { frame: s.frame, strokes: vec![add] }),
            }
        }
    }
    areas.retain(|a| !a.is_empty());
    areas
}

/// Rasterise an area onto a grid of `gw × gh` cells covering the crop (1 = inside). Strokes in order: adding sets,
/// erasing clears.
pub fn raster(area: &Area, gw: usize, gh: usize) -> Vec<u8> {
    let mut out = vec![0u8; gw * gh];
    raster_into(area, gw, gh, [0, 0, gw, gh], &mut out);
    out
}

/// [`raster`] restricted to the cells `rect` = (x, y, w, h) of the grid; `out` is `w × h`.
fn raster_into(area: &Area, gw: usize, gh: usize, rect: [usize; 4], out: &mut [u8]) {
    let [rx, ry, rw, rh] = rect;
    let edge = gw.max(gh) as f32;
    for s in &area.strokes {
        if s.points.is_empty() {
            continue;
        }
        let value = (!s.erase) as u8;
        let rad = (s.radius.max(0.0) * edge).max(0.5);
        let pts: Vec<[f32; 2]> = s.points.iter().map(|p| [p[0] * gw as f32, p[1] * gh as f32]).collect();
        for k in 0..pts.len() {
            let (a, b) = (pts[k], pts[(k + 1).min(pts.len() - 1)]);
            let x0 = ((a[0].min(b[0]) - rad).floor().max(rx as f32)) as usize;
            let x1 = ((a[0].max(b[0]) + rad).ceil().max(0.0) as usize).min(rx + rw);
            let y0 = ((a[1].min(b[1]) - rad).floor().max(ry as f32)) as usize;
            let y1 = ((a[1].max(b[1]) + rad).ceil().max(0.0) as usize).min(ry + rh);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = dx * dx + dy * dy;
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = [x as f32 + 0.5, y as f32 + 0.5];
                    let t = if l2 > 0.0 { (((c[0] - a[0]) * dx + (c[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                    let (px, py) = (a[0] + t * dx - c[0], a[1] + t * dy - c[1]);
                    if px * px + py * py <= rad * rad {
                        out[(y - ry) * rw + (x - rx)] = value;
                    }
                }
            }
        }
    }
}

/// Width of the soft edge as a fraction of the longer edge.
pub const FEATHER: f32 = 0.004;

/// Box blur of radius `r` along rows and columns.
fn box_blur(v: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let n = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).zip(v.par_chunks(w)).for_each(|(o, row)| {
        let mut s: f32 = (0..=r).map(|i| row[i.min(w - 1)]).sum::<f32>() + r as f32 * row[0];
        for x in 0..w {
            o[x] = s / n;
            let add = row[(x + r + 1).min(w - 1)];
            let sub = row[x.saturating_sub(r)];
            s += add - sub;
        }
    });
    let cols: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let col = |y: usize| tmp[y.min(h - 1) * w + x];
            let mut out = vec![0f32; h];
            let mut s: f32 = (0..=r).map(col).sum::<f32>() + r as f32 * col(0);
            for (y, o) in out.iter_mut().enumerate() {
                *o = s / n;
                s += col(y + r + 1) - col(y.saturating_sub(r));
            }
            out
        })
        .collect();
    for (x, c) in cols.iter().enumerate() {
        for (y, val) in c.iter().enumerate() {
            v[y * w + x] = *val;
        }
    }
}

/// Soft mask of an area on an image of `w × h` covering the crop: the cells `rect` = (x, y, w, h) it touches and
/// their weights 0 … 1. `None` when the area covers nothing.
pub fn mask(area: &Area, w: usize, h: usize) -> Option<([usize; 4], Vec<f32>)> {
    let b = area.bounds(w as f32 / h.max(1) as f32)?;
    let f = ((FEATHER * w.max(h) as f32).round() as usize).max(1);
    let m = 2 * f + 2;
    let x0 = ((b[0] * w as f32).floor() as usize).saturating_sub(m);
    let y0 = ((b[1] * h as f32).floor() as usize).saturating_sub(m);
    let x1 = (((b[2] * w as f32).ceil() as usize) + m).min(w);
    let y1 = (((b[3] * h as f32).ceil() as usize) + m).min(h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let rect = [x0, y0, x1 - x0, y1 - y0];
    let mut bin = vec![0u8; rect[2] * rect[3]];
    raster_into(area, w, h, rect, &mut bin);
    if !bin.contains(&1) {
        return None;
    }
    let mut v: Vec<f32> = bin.iter().map(|b| *b as f32).collect();
    // two box passes ≈ a smooth edge of about 2·f pixels, centred on the outline
    let r = f.div_ceil(2).max(1);
    box_blur(&mut v, rect[2], rect[3], r);
    box_blur(&mut v, rect[2], rect[3], r);
    Some((rect, v))
}

/// Paste the areas over a merged image `px` (`w × h`, covering the crop). `hidden[i]` leaves area `i` out (comparison);
/// `patch(frame, rect)` gives the pixels of that frame (aligned, reference units) for `rect` = (x, y, w, h), `None`
/// when it cannot. Non-finite patch values keep the merge.
pub fn apply(px: &mut [[f32; 3]], w: usize, h: usize, areas: &[Area], hidden: &[bool], patch: &mut dyn FnMut(usize, [usize; 4]) -> Option<Vec<[f32; 3]>>) {
    for (i, a) in areas.iter().enumerate() {
        if hidden.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some((rect, m)) = mask(a, w, h) else { continue };
        let Some(src) = patch(a.frame, rect) else { continue };
        if src.len() != rect[2] * rect[3] {
            continue;
        }
        let [x0, y0, rw, _] = rect;
        px.par_chunks_mut(w).enumerate().skip(y0).take(rect[3]).for_each(|(y, row)| {
            for x in 0..rw {
                let k = (y - y0) * rw + x;
                let (t, s) = (m[k], src[k]);
                if t <= 0.0 || !(s[0].is_finite() && s[1].is_finite() && s[2].is_finite()) {
                    continue;
                }
                let o = &mut row[x0 + x];
                for c in 0..3 {
                    o[c] += t * (s[c] - o[c]);
                }
            }
        });
    }
}

/// A patch of a frame ([`full_patch`]) with the places the frame does not cover (non-finite) taken from the same
/// patch of the reference: an area shows its frame, and beyond the edge of that frame the reference – at full
/// resolution as in the preview ([`crate::merge::preview_frames`] does the same). Returns the number of pixels filled.
pub fn fill_uncovered(patch: &mut [[f32; 3]], reference: &[[f32; 3]]) -> usize {
    let mut n = 0;
    for (o, r) in patch.iter_mut().zip(reference) {
        if !(o[0].is_finite() && o[1].is_finite() && o[2].is_finite()) {
            *o = *r;
            n += 1;
        }
    }
    n
}

/// Where the frames lie relative to the reference and how bright they are: what is needed to bring a frame onto the
/// merge without analysing again (stored with the merge).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub reference: usize,
    pub warps: Vec<Warp>,
    /// exposure factor of each frame relative to the reference
    pub k: Vec<f64>,
    /// white balance of the reference (applied to all frames)
    pub wb: [f32; 3],
    /// crop of the reference: x, y, w, h (sensor pixels)
    pub crop: [usize; 4],
    /// sensor size
    pub w: usize,
    pub h: usize,
}

impl crate::Plan {
    pub fn geometry(&self) -> Geometry {
        let f = &self.frames[self.reference];
        Geometry { reference: self.reference, warps: self.warps.clone(), k: self.k.clone(), wb: self.wb, crop: [self.crop.x, self.crop.y, self.crop.w, self.crop.h], w: f.w, h: f.h }
    }
}

/// The frame the merge falls back to where every frame is saturated: the darkest one (smallest exposure factor).
pub fn darkest(k: &[f64]) -> Option<usize> {
    (0..k.len()).min_by(|a, b| k[*a].total_cmp(&k[*b]))
}

/// The highlight handling of a single RAW on the mosaic of `frame` (blown colours rebuilt from the others, fully
/// blown places neutral; `wb`: the white balance of the merge). The merge does this to its darkest frame
/// ([`darkest`]) – where even that one is blown its values are what the merge shows. Whoever takes pixels of that
/// frame for the merge (the areas at full resolution, [`for_patches`]) has to do the same, or they differ from the
/// merge and its preview. `GRAINEO_HDR_BLOWN_LEGACY=1`: nothing, as before algorithm version 4 (comparisons only).
pub fn reconstruct_highlights(frame: &mut Frame, wb: [f32; 3]) -> usize {
    if std::env::var("GRAINEO_HDR_BLOWN_LEGACY").as_deref() == Ok("1") || frame.mosaic.is_empty() || frame.planar.is_some() {
        return 0;
    }
    let (pattern, clip, w, h) = (frame.pattern, frame.clip, frame.w, frame.h);
    let col = move |x: usize, y: usize| pattern[y % 6][x % 6] as usize;
    raw::highlights::reconstruct_mosaic(&mut frame.mosaic, w, h, wb, clip, &col)
}

/// Frame `i` of a merge as [`full_patch`] has to see it: the darkest frame with the highlight handling the merge gave
/// it ([`reconstruct_highlights`]), every other frame as it is. `None`: nothing to do, use the frame as loaded.
pub fn for_patches(frame: &Frame, geo: &Geometry, i: usize) -> Option<Frame> {
    if darkest(&geo.k) != Some(i) {
        return None;
    }
    let mut f = frame.clone();
    (reconstruct_highlights(&mut f, geo.wb) > 0).then_some(f)
}

/// Frame `i` at full resolution for the output rectangle `rect` = (x, y, w, h) of the crop: demosaiced on the part of the
/// sensor it needs, warped onto the reference (Lanczos 3, de-ringed as in the merge), divided by its exposure factor.
/// Pixels whose source lies outside the sensor are NaN.
pub fn full_patch(frame: &Frame, geo: &Geometry, i: usize, rect: [usize; 4]) -> Vec<[f32; 3]> {
    let (w, h) = (geo.w, geo.h);
    let [ox, oy, rw, rh] = rect;
    let (cx, cy, cs) = (w as f64 / 2.0, h as f64 / 2.0, w.max(h) as f64 / 2.0);
    let warp = geo.warps.get(i).copied().unwrap_or_default();
    let ident = i == geo.reference;
    let map = |x: usize, y: usize| -> (f64, f64) {
        let (x, y) = ((geo.crop[0] + ox + x) as f64, (geo.crop[1] + oy + y) as f64);
        if ident {
            (x, y)
        } else {
            warp.apply(x, y, cx, cy, cs)
        }
    };
    // source rectangle (corners of the similarity map, margin for the demosaic and the kernel), on the 6-pixel grid
    let corners = [map(0, 0), map(rw, 0), map(0, rh), map(rw, rh)];
    let (mut sx0, mut sy0, mut sx1, mut sy1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        sx0 = sx0.min(x);
        sy0 = sy0.min(y);
        sx1 = sx1.max(x);
        sy1 = sy1.max(y);
    }
    // (the demosaic needs some room: at least 96 pixels each way)
    let margin = 12.0;
    let grow = |a: f64, b: f64| {
        let extra = (96.0 - (b - a) - 2.0 * margin).max(0.0) / 2.0;
        (a - margin - extra, b + margin + extra)
    };
    let ((sx0, sx1), (sy0, sy1)) = (grow(sx0, sx1), grow(sy0, sy1));
    let sx0 = (sx0.floor().max(0.0) as usize) / 6 * 6;
    let sy0 = (sy0.floor().max(0.0) as usize) / 6 * 6;
    let sx1 = (sx1.ceil().max(0.0) as usize).min(w);
    let sy1 = (sy1.ceil().max(0.0) as usize).min(h);
    if sx1 <= sx0 + 8 || sy1 <= sy0 + 8 {
        return vec![[f32::NAN; 3]; rw * rh];
    }
    let (sw, sh) = (sx1 - sx0, sy1 - sy0);
    let wb = geo.wb;
    let rgb: Vec<[f32; 3]> = match &frame.planar {
        Some(pl) if !pl.is_empty() => (0..sw * sh).map(|k| {
            let p = pl[(sy0 + k / sw) * w + sx0 + k % sw];
            [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]]
        }).collect(),
        _ => {
            if frame.mosaic.len() < w * h {
                return vec![[f32::NAN; 3]; rw * rh];
            }
            let smax = wb[0].max(wb[1]).max(wb[2]).max(1e-6);
            let pat = frame.pattern;
            let mut m = vec![0f32; sw * sh];
            m.par_chunks_mut(sw).enumerate().for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    *v = frame.mosaic[(sy0 + y) * w + sx0 + x] * wb[pat[y % 6][x % 6] as usize] / smax;
                }
            });
            let dm = if frame.info.sensor == raw::Sensor::XTrans { raw::development::Demosaic::Markesteijn3 } else { raw::development::Demosaic::Rcd };
            let mut rgb = raw::development::demosaic_mosaic(&m, sw, sh, pat, dm, frame.info.sensor);
            rgb.par_iter_mut().for_each(|p| *p = p.map(|v| v * smax));
            rgb
        }
    };
    let k = geo.k.get(i).copied().unwrap_or(1.0) as f32;
    let lz = Lanczos::new();
    let mut out = vec![[f32::NAN; 3]; rw * rh];
    out.par_chunks_mut(rw).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (xs, ys) = map(x, y);
            let (xs, ys) = (xs - sx0 as f64, ys - sy0 as f64);
            // (the last column and row of the sensor count as covered, as in the merge)
            if !(xs >= 0.0 && ys >= 0.0 && xs <= (sw - 1) as f64 && ys <= (sh - 1) as f64) {
                continue;
            }
            let (x0, y0) = (xs as usize, ys as usize);
            let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
            let (fx, fy) = (xs - x0 as f64, ys - y0 as f64);
            let p00 = rgb[y0 * sw + x0];
            let p10 = rgb[y0 * sw + x1];
            let p01 = rgb[y1 * sw + x0];
            let p11 = rgb[y1 * sw + x1];
            let mut v = [0f32; 3];
            if fx < 1e-6 && fy < 1e-6 {
                v = p00;
            } else if x0 >= 2 && y0 >= 2 && x0 + 3 < sw && y0 + 3 < sh {
                let (wx, wy) = (lz.w(fx), lz.w(fy));
                for (jy, wyv) in wy.iter().enumerate() {
                    let r = (y0 + jy - 2) * sw;
                    for (jx, wxv) in wx.iter().enumerate() {
                        let p = rgb[r + x0 + jx - 2];
                        for c in 0..3 {
                            v[c] += wyv * wxv * p[c];
                        }
                    }
                }
                for c in 0..3 {
                    let lo = p00[c].min(p10[c]).min(p01[c]).min(p11[c]);
                    let hi = p00[c].max(p10[c]).max(p01[c]).max(p11[c]);
                    v[c] = v[c].clamp(lo, hi);
                }
            } else {
                let (fx, fy) = (fx as f32, fy as f32);
                for c in 0..3 {
                    v[c] = (p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy;
                }
            }
            *o = v.map(|c| c / k);
        }
    });
    out
}

/// Cut `rect` = (x, y, w, h) out of an image of width `w`.
pub fn cut(px: &[[f32; 3]], w: usize, rect: [usize; 4]) -> Vec<[f32; 3]> {
    let [x0, y0, rw, rh] = rect;
    let mut out = Vec::with_capacity(rw * rh);
    for y in y0..y0 + rh {
        out.extend_from_slice(&px[y * w + x0..y * w + x0 + rw]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghost::Stroke;

    fn stroke(frame: usize, erase: bool, x: f32) -> Stroke {
        Stroke { frame, erase, radius: 0.05, points: vec![[x, 0.5], [x + 0.1, 0.5]] }
    }

    /// Old recipes: the strokes become areas, and the last stroke covering a place still wins.
    #[test]
    fn strokes_become_areas() {
        let strokes = vec![stroke(0, false, 0.2), stroke(2, false, 0.25), stroke(0, true, 0.7), stroke(0, false, 0.6)];
        let areas = from_strokes(&strokes);
        assert_eq!(areas.iter().map(|a| a.frame).collect::<Vec<_>>(), vec![0, 2]);
        let (gw, gh) = (200, 100);
        let r0 = raster(&areas[0], gw, gh);
        let r2 = raster(&areas[1], gw, gh);
        let at = |x: f32| (gh / 2) * gw + (x * gw as f32) as usize;
        // 0.22: painted 0, then 2 over it → only area 2
        assert_eq!((r0[at(0.22)], r2[at(0.22)]), (0, 1));
        // 0.32: only the second stroke (2) reaches it
        assert_eq!(r2[at(0.32)], 1);
        // 0.65: the last stroke (0) wins
        assert_eq!(r0[at(0.65)], 1);
        // 0.78: erased and not painted again
        assert_eq!((r0[at(0.79)], r2[at(0.79)]), (0, 0));
        // migrating settings keeps nothing in `paint`
        let mut s = crate::Settings { paint: strokes, ..Default::default() };
        s.migrate();
        assert!(s.paint.is_empty());
        assert_eq!(s.regions.len(), 2);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("paint") && json.contains("regions"), "{json}");
        // a recipe without areas keeps its JSON (cached results stay valid)
        assert_eq!(serde_json::to_string(&crate::Settings::default()).unwrap(), r#"{"reference":null,"align":true,"ghost":"standard"}"#);
    }

    /// The area is pasted with a soft edge: inside the frame's values, far outside the merge, in between a mix.
    #[test]
    fn apply_pastes_with_a_soft_edge() {
        let (w, h) = (400, 200);
        let mut px = vec![[1.0f32; 3]; w * h];
        let a = Area { frame: 1, strokes: vec![AreaStroke { erase: false, radius: 0.05, points: vec![[0.5, 0.5]] }] };
        let mut asked = Vec::new();
        apply(&mut px, w, h, &[a.clone()], &[false], &mut |f, r| {
            asked.push((f, r));
            Some(vec![[4.0; 3]; r[2] * r[3]])
        });
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, 1);
        assert!((px[100 * w + 200][0] - 4.0).abs() < 1e-4, "centre {:?}", px[100 * w + 200]);
        assert_eq!(px[10 * w + 10][0], 1.0);
        let edge = (100 * w + 200 + 20..100 * w + 200 + 26).map(|k| px[k][0]).collect::<Vec<_>>();
        assert!(edge.iter().any(|v| *v > 1.01 && *v < 3.99), "soft edge {edge:?}");
        // hidden: nothing changes
        let mut px2 = vec![[1.0f32; 3]; w * h];
        apply(&mut px2, w, h, &[a.clone()], &[true], &mut |_, r| Some(vec![[4.0; 3]; r[2] * r[3]]));
        assert!(px2.iter().all(|p| p[0] == 1.0));
        // "Verkleinern" takes away
        let mut b = a.clone();
        b.strokes.push(AreaStroke { erase: true, radius: 0.08, points: vec![[0.5, 0.5]] });
        assert!(mask(&b, w, h).is_none());
    }

    /// The full-resolution patch of a frame equals the merge input: identity warp, factor 1 → the demosaiced frame.
    #[test]
    fn full_patch_of_the_reference_is_the_frame() {
        let (w, h) = (96, 72);
        let mut info = crate::tests::info(w, h, 0.01);
        info.crop = None;
        // flat grey mosaic: every channel 0.2 before white balance
        let f = crate::frames::from_mosaic("x".into(), info, vec![0.2; w * h], crate::tests::RGGB);
        let geo = Geometry { reference: 0, warps: vec![Warp::default()], k: vec![1.0], wb: [2.0, 1.0, 1.5], crop: [0, 0, w, h], w, h };
        let p = full_patch(&f, &geo, 0, [30, 24, 12, 12]);
        assert_eq!(p.len(), 144);
        for v in &p {
            assert!((v[0] - 0.4).abs() < 1e-3 && (v[1] - 0.2).abs() < 1e-3 && (v[2] - 0.3).abs() < 1e-3, "{v:?}");
        }
        // a factor of 2 halves it
        let geo2 = Geometry { k: vec![2.0], ..geo };
        let p2 = full_patch(&f, &geo2, 0, [30, 24, 4, 4]);
        assert!((p2[0][1] - 0.1).abs() < 1e-3);
    }
}
