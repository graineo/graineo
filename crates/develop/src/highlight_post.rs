//! Highlight sliders "Lichter-Farbe" and "Lichter-Struktur" on a finished working image, for the paths whose rebuild
//! does not run on the Quattro colour grid: Merrill (the rebuild runs in the float frontend,
//! [`crate::merrill_highlights`]).
//!
//! The base is developed once with the sliders in the middle and kept ([`base`]); a slider move only redoes this
//! step on it: no new development. Both sliders act only by the weight of the rebuilt areas (1 inside, fading out
//! over their seam):
//! - colour: chroma around the brightness, as the Quattro grid does it ([`crate::quattro`], reach and smoothing in
//!   pixels instead of cells);
//! - structure: the fine brightness detail (luminance minus its 5 × 5 mean) scaled, one factor for all channels
//!   (hue and saturation stay).
//!
//! At 50/50 nothing runs: the image is the base, bit for bit.

use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Weight of the rebuilt areas of a photo in the coordinates of its working image.
pub struct Weight {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

/// Pixels of the image per Quattro grid cell for the colour slider's reach.
const SCALE: usize = 2;
/// Radius of the mean the structure slider's detail is measured against (pixels).
const DETAIL_RADIUS: usize = 2;

fn weights() -> &'static Mutex<Vec<(PathBuf, Option<Arc<Weight>>)>> {
    static W: Mutex<Vec<(PathBuf, Option<Arc<Weight>>)>> = Mutex::new(Vec::new());
    &W
}

/// Remembers the weight of `path` (`None`: developed with the rebuild on, nothing rebuilt).
pub fn remember(path: &Path, w: Option<Weight>) {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut m = weights().lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|(p, _)| *p != file);
    // a few photos (a weight is one float per pixel)
    if m.len() >= 4 {
        m.remove(0);
    }
    m.push((file, w.map(Arc::new)));
}

/// The weight of `path`: `Some(None)` = known, nothing rebuilt; `None` = unknown (not developed in this session).
pub fn weight(path: &Path) -> Option<Option<Arc<Weight>>> {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    weights().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(p, _)| *p == file).map(|(_, w)| w.clone())
}

/// Drops every remembered weight and base (the switch was turned).
pub fn forget_all() {
    weights().lock().unwrap_or_else(|e| e.into_inner()).clear();
    *bases().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

type BaseKey = (PathBuf, crate::SppKey);

fn bases() -> &'static Mutex<Option<(BaseKey, Arc<crate::Linear>)>> {
    static B: Mutex<Option<(BaseKey, Arc<crate::Linear>)>> = Mutex::new(None);
    &B
}

/// The base (sliders in the middle) of `path` under `key`, kept for the next slider move (one photo).
pub fn base(path: &Path, key: &crate::SppKey, compute: impl FnOnce() -> Result<crate::Linear, String>) -> Result<Arc<crate::Linear>, String> {
    let k = (std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()), key.clone());
    if let Some((bk, lin)) = bases().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *bk == k {
            return Ok(lin.clone());
        }
    }
    let lin = Arc::new(compute()?);
    *bases().lock().unwrap_or_else(|e| e.into_inner()) = Some((k, lin.clone()));
    Ok(lin)
}

/// Applies the sliders (`colour`, `structure`: 0 … 1, 0.5 = as rebuilt) to the RGB data of `lin` by `w` (same size).
pub fn apply(lin: &mut crate::Linear, w: &Weight, colour: f32, structure: f32) {
    let (iw, ih) = (lin.width, lin.height);
    if w.width != iw || w.height != ih || lin.data.len() != iw * ih * 3 {
        return;
    }
    let kc = crate::highlight_rebuild::colour_factor(colour);
    let ks = crate::highlight_rebuild::structure_factor(structure);
    if ks != 1.0 {
        let luma = |p: &[f32]| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
        let y: Vec<f32> = lin.data.par_chunks(3).map(luma).collect();
        let mean = crate::quattro::box_blur(&y, iw, ih, DETAIL_RADIUS);
        lin.data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
            let wt = w.data[i];
            if wt > 0.0 && y[i] > 0.0 {
                let f = ((y[i] + (ks - 1.0) * wt * (y[i] - mean[i])) / y[i]).max(0.0);
                for v in px.iter_mut() {
                    *v *= f;
                }
            }
        });
    }
    if kc < 1.0 {
        crate::quattro::highlight_colour_scaled(&mut lin.data, &w.data, iw, ih, kc, SCALE);
    } else if kc > 1.0 {
        more_colour(lin, w, kc);
    }
}

/// Relative smoothed chroma below which "Lichter-Farbe" adds no colour, and from which it adds the full amount (as on
/// the Quattro grid).
const COLOUR_GATE: (f32, f32) = (0.08, 0.2);
/// Reach of the colour slider around the rebuilt areas and radius of the smoothing of its chroma (pixels).
const REACH: usize = 24;
const SMOOTH: usize = 4;

/// "Lichter-Farbe" above the middle: the chroma around the brightness scaled up where it is clear (smoothed chroma
/// over [`COLOUR_GATE`]), by the softened weight; the same factor for all channels' distance from the brightness, so
/// the hue stays (the Quattro grid keeps its brightest channel instead; on the frontend's colours, before the output
/// matrix, that turned bright yellow into magenta). Nothing below 0.
fn more_colour(lin: &mut crate::Linear, w: &Weight, k: f32) {
    let (iw, ih) = (lin.width, lin.height);
    let wt = crate::quattro::box_blur(&crate::quattro::max_filter(&w.data, iw, ih, REACH), iw, ih, REACH / 2);
    let luma = |p: &[f32]| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
    let ys: Vec<f32> = lin.data.par_chunks(3).map(luma).collect();
    let d: [Vec<f32>; 3] = std::array::from_fn(|c| crate::quattro::box_blur(&lin.data.par_chunks(3).zip(ys.par_iter()).map(|(p, y)| p[c] - y).collect::<Vec<_>>(), iw, ih, SMOOTH));
    let yl = crate::quattro::box_blur(&ys, iw, ih, SMOOTH);
    lin.data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let t = wt[i].min(1.0);
        if t <= 0.0 || yl[i] <= 0.0 {
            return;
        }
        let chroma = (d[0][i] * d[0][i] + d[1][i] * d[1][i] + d[2][i] * d[2][i]).sqrt() / yl[i];
        let g = ((chroma - COLOUR_GATE.0) / (COLOUR_GATE.1 - COLOUR_GATE.0)).clamp(0.0, 1.0);
        let mut f = (k - 1.0) * t * g * g * (3.0 - 2.0 * g);
        let y = ys[i];
        for c in 0..3 {
            let dc = px[c] - y;
            if dc < 0.0 {
                f = f.min(px[c].max(0.0) / -dc);
            }
        }
        if f > 0.0 {
            for v in px.iter_mut() {
                *v = y + (*v - y) * (1.0 + f);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin(w: usize, h: usize) -> crate::Linear {
        let data = (0..w * h).flat_map(|i| [0.6 + 0.01 * (i % 5) as f32, 0.4, 0.2]).collect();
        crate::Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None }
    }

    #[test]
    fn middle_changes_nothing() {
        let (w, h) = (16, 12);
        let mut a = lin(w, h);
        let before = a.data.clone();
        apply(&mut a, &Weight { width: w, height: h, data: vec![1.0; w * h] }, 0.5, 0.5);
        assert_eq!(a.data, before);
    }

    #[test]
    fn colour_zero_is_neutral_where_rebuilt_only() {
        let (w, h) = (64, 8);
        let mut a = lin(w, h);
        let before = a.data.clone();
        let mut wt = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..4 {
                wt[y * w + x] = 1.0;
            }
        }
        apply(&mut a, &Weight { width: w, height: h, data: wt }, 0.0, 0.5);
        let p = &a.data[0..3];
        assert!((p[0] - p[2]).abs() < 0.05, "{p:?}");
        // far from the rebuilt area: unchanged
        let i = (h / 2 * w + w - 1) * 3;
        assert_eq!(&a.data[i..i + 3], &before[i..i + 3]);
    }
}
