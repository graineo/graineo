//! Input frames: the normalised mosaic of each RAW (black level per pattern position subtracted, divided by white),
//! saturation per channel from the data, and a half-resolution luma proxy for alignment and exposure measurement.

use crate::img::Img;
use crate::Error;
use raw::{RawFile, Sensor};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// One loaded frame of the bracket.
#[derive(Clone)]
pub struct Frame {
    pub path: PathBuf,
    pub info: raw::Info,
    /// normalised mosaic (full sensor, `info.width × info.height`); taken while merging at full resolution
    pub mosaic: Vec<f32>,
    /// 6×6 colour pattern relative to the sensor origin
    pub pattern: [[u8; 6]; 6],
    pub w: usize,
    pub h: usize,
    /// exposure from EXIF, `t · ISO/100 / N²` (relative units)
    pub exif: Option<f64>,
    /// proxy block size (Bayer 2, X-Trans 3)
    pub pf: usize,
    /// mean green per proxy block (normalised units, before white balance)
    pub g: Img,
    /// per proxy block: maximum over the block of value / clip of its channel
    pub mx: Img,
    /// clip level per channel (normalised units)
    pub clip: [f32; 3],
    /// Foveon (three stacked layers, no mosaic): the linear value of every layer at every pixel (black subtracted,
    /// normalised to the saturation count). Then `mosaic` holds the mean of the three layers (for the noise model) and
    /// the pattern is "all green"; the merge uses these planes instead of a demosaic.
    pub planar: Option<Vec<[f32; 3]>>,
}

impl Frame {
    #[inline]
    pub fn color_at(&self, x: usize, y: usize) -> usize {
        self.pattern[y % 6][x % 6] as usize
    }
}

/// Relative exposure from EXIF: `t · ISO/100 / N²`; a missing aperture (manual lenses) counts as f/1. That is only
/// comparable within a bracket whose frames all have or all lack the aperture: see [`harmonize_exif`].
pub fn exif_exposure(i: &raw::Info) -> Option<f64> {
    exif_exposure_with(i, true)
}

/// [`exif_exposure`]; without `use_aperture` the aperture is left out (`t · ISO/100`).
pub fn exif_exposure_with(i: &raw::Info, use_aperture: bool) -> Option<f64> {
    let t = i.exif.exposure_time.filter(|t| *t > 0.0)? as f64;
    let iso = i.exif.iso.filter(|v| *v > 0).unwrap_or(100) as f64;
    let n = if use_aperture { known_aperture(i).unwrap_or(1.0) } else { 1.0 };
    Some(t * iso / 100.0 / (n * n))
}

fn known_aperture(i: &raw::Info) -> Option<f64> {
    i.exif.aperture.map(|a| a as f64).filter(|a| *a > 0.0)
}

/// Make the EXIF exposures of one bracket comparable: when only some of its frames carry an aperture (a contact
/// that failed for single shots, an adapter), the others would count as f/1 and be off by N². Then the aperture is
/// left out for all frames, as for a manual lens. Returns whether that happened.
pub fn harmonize_exif(frames: &mut [Frame]) -> bool {
    let with = frames.iter().filter(|f| known_aperture(&f.info).is_some()).count();
    if with == 0 || with == frames.len() {
        return false;
    }
    for f in frames {
        f.exif = exif_exposure_with(&f.info, false);
    }
    true
}

/// Clip level per channel: the per-channel maximum of the mosaic counts as saturation when at least 1/10 000 of that
/// channel's samples lie within 1 % below it (a pile-up at the top, idea from HDRMerge), ×0.97; otherwise the nominal
/// white (0.97).
pub fn clip_levels(norm: &[f32], w: usize, color_at: &(dyn Fn(usize, usize) -> usize + Sync)) -> [f32; 3] {
    let tops: [f32; 3] = norm
        .par_chunks(w)
        .enumerate()
        .map(|(y, row)| {
            let mut t = [0f32; 3];
            for (x, v) in row.iter().enumerate() {
                let c = color_at(x, y);
                t[c] = t[c].max(*v);
            }
            t
        })
        .reduce(|| [0.0; 3], |a, b| [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])]);
    let cnt: [usize; 6] = norm
        .par_chunks(w)
        .enumerate()
        .map(|(y, row)| {
            let mut n = [0usize; 6];
            for (x, v) in row.iter().enumerate() {
                let c = color_at(x, y);
                n[c + 3] += 1;
                if *v >= 0.99 * tops[c] {
                    n[c] += 1;
                }
            }
            n
        })
        .reduce(|| [0; 6], |a, b| std::array::from_fn(|i| a[i] + b[i]));
    std::array::from_fn(|c| if cnt[c] * 10000 >= cnt[c + 3] && tops[c] > 0.3 { 0.97 * tops[c].min(1.0) } else { 0.97 })
}

/// Load one RAW frame. Only mosaic sensors (Bayer, X-Trans) are supported; other files fail with a clear error.
pub fn load(path: &Path) -> Result<Frame, Error> {
    let f = RawFile::open(path).map_err(|e| match e {
        raw::Error::Unsupported(why) => Error::Unsupported { path: path.to_path_buf(), why },
        e => Error::Read { path: path.to_path_buf(), why: e.to_string() },
    })?;
    if f.info.fuji_rotated || !matches!(f.info.sensor, Sensor::Bayer | Sensor::XTrans) {
        return Err(Error::Unsupported { path: path.to_path_buf(), why: "no Bayer or X-Trans mosaic".into() });
    }
    let (norm, pattern) = f.mosaic_normalized().map_err(|e| Error::Read { path: path.to_path_buf(), why: e.to_string() })?;
    let info = f.info.clone();
    drop(f);
    Ok(from_mosaic(path.to_path_buf(), info, norm, pattern))
}

/// Build a frame from an already normalised mosaic (tests, synthetic brackets).
pub fn from_mosaic(path: PathBuf, info: raw::Info, norm: Vec<f32>, pattern: [[u8; 6]; 6]) -> Frame {
    let (w, h) = (info.width, info.height);
    let pf = if info.sensor == Sensor::XTrans { 3 } else { 2 };
    let col = |x: usize, y: usize| pattern[y % 6][x % 6] as usize;
    let clip = clip_levels(&norm, w, &col);
    let (pw, ph) = (w / pf, h / pf);
    let mut g = vec![0f32; pw * ph];
    let mut mx = vec![0f32; pw * ph];
    g.par_chunks_mut(pw.max(1)).zip(mx.par_chunks_mut(pw.max(1))).enumerate().for_each(|(y, (gr, mr))| {
        for x in 0..pw {
            let (mut s, mut n, mut m) = (0f32, 0f32, 0f32);
            for dy in 0..pf {
                for dx in 0..pf {
                    let (xx, yy) = (pf * x + dx, pf * y + dy);
                    let v = norm[yy * w + xx];
                    let c = col(xx, yy);
                    m = m.max(v / clip[c]);
                    if c == 1 {
                        s += v;
                        n += 1.0;
                    }
                }
            }
            gr[x] = s / n.max(1.0);
            mr[x] = m;
        }
    });
    let exif = exif_exposure(&info);
    Frame { path, info, mosaic: norm, pattern, w, h, exif, pf, g: Img { w: pw, h: ph, p: g }, mx: Img { w: pw, h: ph, p: mx }, clip, planar: None }
}

/// Clip level per layer of a Foveon bracket, shared by all its frames: the layers saturate at a physical level of their
/// own (in developed units about 0.48 / 0.47 / 0.60 for the sd Quattro H, far below the nominal count; different
/// from layer to layer and over the sensor), the same in every frame. The level is the 99.99 % value of the
/// brightest frames (any frame that clips shows it), counted as clip when at least 0.05 % of a frame's samples lie
/// within 5 % below it (a pile-up); otherwise the nominal 0.97. The clip is 0.97 of that top.
pub fn planar_clip(planes: &[&[[f32; 3]]]) -> [f32; 3] {
    let mut clip = [0.97f32; 3];
    for (c, cl) in clip.iter_mut().enumerate() {
        let tops: Vec<(f32, Vec<f32>)> = planes
            .iter()
            .filter(|pl| !pl.is_empty())
            .map(|pl| {
                let mut v: Vec<f32> = pl.iter().step_by(7).map(|p| p[c]).collect();
                v.sort_by(|a, b| a.total_cmp(b));
                (v[((v.len() - 1) as f64 * 0.9999) as usize], v)
            })
            .collect();
        let top = tops.iter().map(|t| t.0).fold(0f32, f32::max);
        let piled = tops.iter().any(|(_, v)| top > 0.3 && v.iter().rev().take_while(|x| **x >= 0.95 * top).count() * 2000 >= v.len());
        if piled {
            *cl = 0.97 * top;
        }
    }
    clip
}

/// Build a frame from three full-resolution layers (Foveon): `rgb` is `w × h` pixels of linear layer values (black
/// subtracted, 1.0 = nominal saturation count); `clip` the clip level per layer ([`planar_clip`] of the bracket).
/// The proxies use the mean of the layers.
pub fn from_planar(path: PathBuf, info: raw::Info, rgb: Vec<[f32; 3]>, w: usize, h: usize, clip: [f32; 3]) -> Frame {
    let pattern = [[1u8; 6]; 6];
    let mean: Vec<f32> = rgb.par_iter().map(|p| (p[0] + p[1] + p[2]) / 3.0).collect();
    let pf = 2;
    let (pw, ph) = (w / pf, h / pf);
    let mut g = vec![0f32; pw * ph];
    let mut mx = vec![0f32; pw * ph];
    g.par_chunks_mut(pw.max(1)).zip(mx.par_chunks_mut(pw.max(1))).enumerate().for_each(|(y, (gr, mr))| {
        for x in 0..pw {
            let (mut s, mut m) = (0f32, 0f32);
            for dy in 0..pf {
                for dx in 0..pf {
                    let i = (pf * y + dy) * w + pf * x + dx;
                    s += mean[i];
                    let p = rgb[i];
                    m = m.max((p[0] / clip[0]).max(p[1] / clip[1]).max(p[2] / clip[2]));
                }
            }
            gr[x] = s / (pf * pf) as f32;
            mr[x] = m;
        }
    });
    let exif = exif_exposure(&info);
    Frame { path, info, mosaic: mean, pattern, w, h, exif, pf, g: Img { w: pw, h: ph, p: g }, mx: Img { w: pw, h: ph, p: mx }, clip, planar: Some(rgb) }
}

/// Check that the frames belong together (same camera, same sensor size and pattern).
pub fn check_compatible(frames: &[Frame]) -> Result<(), Error> {
    let Some(a) = frames.first() else { return Err(Error::TooFewFrames(0)) };
    for f in &frames[1..] {
        if f.info.sensor != a.info.sensor || (f.w, f.h) != (a.w, a.h) || f.pattern != a.pattern || f.info.model != a.info.model {
            return Err(Error::Mismatch { path: f.path.clone() });
        }
    }
    Ok(())
}
