//! Quality hints for reviewing a series: sharpness, motion blur, clipped highlights/shadows (from the preview), faces
//! and closed eyes (from Apple Vision, macOS), and the suggestion "best photo" with its reason.
//!
//! All values are meant **relative within the series**: an absolute "unsharp" is not reliable across subjects (fog,
//! long exposures, deliberate blur), but "photo 3 is clearly less sharp than photo 2 of the same series" is. The
//! suggestion never sets anything in the library; the user confirms it.

use std::collections::HashMap;

use image::RgbImage;
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Db;

/// Version of the measurement (a new version measures again).
pub const VERSION: i64 = 2;

/// Measured hints of one photo.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quality {
    /// Tenengrad (mean squared Sobel gradient) of the three sharpest tiles of an 8×8 grid
    pub sharpness: f32,
    /// centre of the sharpest tile (0..1 of the upright image)
    pub sharp_x: f32,
    pub sharp_y: f32,
    /// 0..1: anisotropy of the gradients in the sharpest tiles (high = smeared in one direction)
    pub motion: f32,
    /// share of clipped pixels (any channel ≥ 254 / all channels ≤ 2)
    pub clip_high: f32,
    pub clip_low: f32,
    /// faces found (`None`: not looked for, e.g. on Windows)
    pub faces: Option<u32>,
    pub eyes_closed: Option<u32>,
    /// mean face capture quality 0..1
    pub face_score: Option<f32>,
}

/// Measure sharpness, motion and clipping on an upright preview (≤ 1024 px recommended; ~5–10 ms).
pub fn measure(img: &RgbImage) -> Quality {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut q = Quality::default();
    if w < 16 || h < 16 {
        return q;
    }
    let mut luma = vec![0f32; w * h];
    let (mut high, mut low) = (0usize, 0usize);
    for (i, p) in img.pixels().enumerate() {
        let [r, g, b] = p.0;
        luma[i] = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        if r >= 254 || g >= 254 || b >= 254 {
            high += 1;
        }
        if r <= 2 && g <= 2 && b <= 2 {
            low += 1;
        }
    }
    q.clip_high = high as f32 / (w * h) as f32;
    q.clip_low = low as f32 / (w * h) as f32;
    // per tile: gradient energy and structure tensor
    let mut energy = [0f64; 64];
    let mut jxx = [0f64; 64];
    let mut jyy = [0f64; 64];
    let mut jxy = [0f64; 64];
    let mut count = [0u32; 64];
    let l = |x: usize, y: usize| luma[y * w + x];
    for y in 1..h - 1 {
        let ty = y * 8 / h;
        for x in 1..w - 1 {
            let gx = (l(x + 1, y - 1) + 2.0 * l(x + 1, y) + l(x + 1, y + 1)) - (l(x - 1, y - 1) + 2.0 * l(x - 1, y) + l(x - 1, y + 1));
            let gy = (l(x - 1, y + 1) + 2.0 * l(x, y + 1) + l(x + 1, y + 1)) - (l(x - 1, y - 1) + 2.0 * l(x, y - 1) + l(x + 1, y - 1));
            let t = ty * 8 + x * 8 / w;
            let (gx, gy) = (gx as f64, gy as f64);
            energy[t] += gx * gx + gy * gy;
            jxx[t] += gx * gx;
            jyy[t] += gy * gy;
            jxy[t] += gx * gy;
            count[t] += 1;
        }
    }
    let mut tiles: Vec<(f64, usize)> = (0..64).map(|t| (energy[t] / count[t].max(1) as f64, t)).collect();
    tiles.sort_by(|a, b| b.0.total_cmp(&a.0));
    let best = &tiles[..3];
    q.sharpness = (best.iter().map(|t| t.0).sum::<f64>() / 3.0).sqrt() as f32;
    let t0 = best[0].1;
    q.sharp_x = ((t0 % 8) as f32 + 0.5) / 8.0;
    q.sharp_y = ((t0 / 8) as f32 + 0.5) / 8.0;
    let (sxx, syy, sxy) = best.iter().fold((0.0, 0.0, 0.0), |a, t| (a.0 + jxx[t.1], a.1 + jyy[t.1], a.2 + jxy[t.1]));
    let tr = sxx + syy;
    if tr > 0.0 {
        let d = ((sxx - syy).powi(2) + 4.0 * sxy * sxy).sqrt();
        q.motion = (d / tr) as f32;
    }
    q
}

/// Store the measured hints (the signature in the same row stays).
pub fn save(db: &Db, file: i64, q: &Quality) -> Result<(), String> {
    db.conn
        .execute(
            "INSERT INTO image_quality(file, version, sharpness, sharp_x, sharp_y, motion, clip_high, clip_low, faces, eyes_closed, face_score, computed)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(file) DO UPDATE SET version = excluded.version, sharpness = excluded.sharpness, sharp_x = excluded.sharp_x, sharp_y = excluded.sharp_y,
               motion = excluded.motion, clip_high = excluded.clip_high, clip_low = excluded.clip_low, faces = excluded.faces,
               eyes_closed = excluded.eyes_closed, face_score = excluded.face_score, computed = excluded.computed",
            params![file, VERSION, q.sharpness, q.sharp_x, q.sharp_y, q.motion, q.clip_high, q.clip_low, q.faces, q.eyes_closed, q.face_score, crate::now()],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Hints of these files (only current measurements).
pub fn load(conn: &Connection, files: &[i64]) -> HashMap<i64, Quality> {
    let mut out = HashMap::new();
    let Ok(mut st) = conn.prepare_cached("SELECT sharpness, sharp_x, sharp_y, motion, clip_high, clip_low, faces, eyes_closed, face_score FROM image_quality WHERE file = ?1 AND version = ?2 AND sharpness IS NOT NULL") else {
        return out;
    };
    for &f in files {
        let q = st
            .query_row(params![f, VERSION], |r| {
                Ok(Quality {
                    sharpness: r.get::<_, f64>(0)? as f32,
                    sharp_x: r.get::<_, f64>(1)? as f32,
                    sharp_y: r.get::<_, f64>(2)? as f32,
                    motion: r.get::<_, f64>(3)? as f32,
                    clip_high: r.get::<_, f64>(4)? as f32,
                    clip_low: r.get::<_, f64>(5)? as f32,
                    faces: r.get::<_, Option<i64>>(6)?.map(|x| x as u32),
                    eyes_closed: r.get::<_, Option<i64>>(7)?.map(|x| x as u32),
                    face_score: r.get::<_, Option<f64>>(8)?.map(|x| x as f32),
                })
            })
            .optional()
            .ok()
            .flatten();
        if let Some(q) = q {
            out.insert(f, q);
        }
    }
    out
}

/// Members of stacks without a current measurement: (file, path, size, mtime, orientation, format), newest first.
pub fn stack_files_without_quality(db: &Db) -> Vec<(i64, String, i64, i64, Option<i64>, String)> {
    let Ok(mut st) = db.conn.prepare(
        "SELECT d.id, s.path || o.path || d.name, coalesce(d.size, 0), coalesce(d.mtime, 0), d.orientation, d.format
         FROM stack_image si JOIN image b ON b.id = si.image JOIN file d ON d.id = b.file
         JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
         LEFT JOIN image_quality q ON q.file = d.id
         WHERE q.sharpness IS NULL OR q.version <> ?1
         ORDER BY d.capture_time DESC",
    ) else {
        return Vec::new();
    };
    st.query_map([VERSION], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).map(|it| it.flatten().collect()).unwrap_or_default()
}

// ───────────────────────────────────────────── suggestion

/// One photo of a series for the suggestion.
#[derive(Clone, Copy, Debug, Default)]
pub struct Candidate {
    pub quality: Option<Quality>,
    pub stars: u8,
    pub flag: i8,
}

/// Why a photo is suggested (at most two, shown translated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Sharpest,
    EyesOpen,
    NoMotionBlur,
    NoClipping,
    RatedBest,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Suggestion {
    /// index of the suggested photo; `None`: no clear best ("hardly any difference") or nothing measured
    pub best: Option<usize>,
    /// points per photo (0..1, comparable within the series)
    pub points: Vec<f32>,
    pub why: Vec<Why>,
    /// the best two are almost equal: decide yourself
    pub close: bool,
    /// normalized sharpness per photo (0..1 within the series), for the bars
    pub sharp: Vec<f32>,
}

fn norm(v: &[f32]) -> Vec<f32> {
    let lo = v.iter().copied().fold(f32::MAX, f32::min);
    let hi = v.iter().copied().fold(f32::MIN, f32::max);
    if !(hi - lo > 1e-6) {
        return vec![1.0; v.len()];
    }
    v.iter().map(|x| (x - lo) / (hi - lo)).collect()
}

/// Points per photo as in the concept: 0.40 sharpness, 0.25 faces (quality, −1 per face with closed eyes),
/// 0.15 (1 − motion blur), 0.10 exposure (little clipped), 0.10 the rating so far. Without faces their share goes to
/// the others. Needs a measurement for every photo; a gap below 0.05 to the second counts as "close".
pub fn suggest(c: &[Candidate]) -> Suggestion {
    let n = c.len();
    if n < 2 || c.iter().any(|x| x.quality.is_none()) {
        return Suggestion { points: vec![0.0; n], ..Suggestion::default() };
    }
    let q: Vec<Quality> = c.iter().map(|x| x.quality.unwrap()).collect();
    // sharpness as a share of the sharpest (a min–max scale would blow up tiny differences)
    let max_sharp = q.iter().map(|x| x.sharpness).fold(0.0f32, f32::max);
    let sharp: Vec<f32> = q.iter().map(|x| if max_sharp > 0.0 { x.sharpness / max_sharp } else { 1.0 }).collect();
    // less motion is better; equal motion everywhere: no difference
    let motion = norm(&q.iter().map(|x| -x.motion).collect::<Vec<_>>());
    let expo = norm(&q.iter().map(|x| -(x.clip_high + x.clip_low)).collect::<Vec<_>>());
    let rating = norm(&c.iter().map(|x| x.stars as f32 + if x.flag == 1 { 2.0 } else if x.flag == -1 { -3.0 } else { 0.0 }).collect::<Vec<_>>());
    let with_faces = q.iter().any(|x| x.faces.unwrap_or(0) > 0);
    let faces: Vec<f32> = if with_faces {
        let score = norm(&q.iter().map(|x| x.face_score.unwrap_or(0.0)).collect::<Vec<_>>());
        score.iter().zip(&q).map(|(s, x)| s - x.eyes_closed.unwrap_or(0) as f32).collect()
    } else {
        vec![0.0; n]
    };
    let (ws, wf, wm, we, wr) = if with_faces { (0.40, 0.25, 0.15, 0.10, 0.10) } else { (0.40 / 0.75, 0.0, 0.15 / 0.75, 0.10 / 0.75, 0.10 / 0.75) };
    let points: Vec<f32> = (0..n).map(|i| ws * sharp[i] + wf * faces[i] + wm * motion[i] + we * expo[i] + wr * rating[i]).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| points[*b].total_cmp(&points[*a]));
    let (b, second) = (order[0], order[1]);
    let close = points[b] - points[second] < 0.05;
    let mut why = Vec::new();
    if !close {
        // the features that made the difference: largest weighted lead over the mean of the others
        let lead = |v: &[f32], w: f32| {
            let others: f32 = (0..n).filter(|&i| i != b).map(|i| v[i]).sum::<f32>() / (n - 1) as f32;
            w * (v[b] - others)
        };
        let mut cands = vec![(lead(&sharp, ws), Why::Sharpest), (lead(&motion, wm), Why::NoMotionBlur), (lead(&expo, we), Why::NoClipping), (lead(&rating, wr), Why::RatedBest)];
        if with_faces {
            cands.push((lead(&faces, wf), Why::EyesOpen));
        }
        cands.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (v, w) in cands {
            let fits = match w {
                Why::Sharpest => sharp[b] >= 0.999,
                Why::EyesOpen => q[b].eyes_closed == Some(0) && q[b].faces.unwrap_or(0) > 0,
                Why::NoClipping => q[b].clip_high < 0.002,
                _ => true,
            };
            if v > 0.02 && fits && why.len() < 2 {
                why.push(w);
            }
        }
    }
    Suggestion { best: (!close).then_some(b), points, why, close, sharp }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn textured(blur: u32, shift: u32) -> RgbImage {
        let base = RgbImage::from_fn(256, 192, |x, y| {
            let v = if ((x + shift) / 8 + y / 8) % 2 == 0 { 40 } else { 210 };
            image::Rgb([v, v, v])
        });
        if blur == 0 {
            return base;
        }
        // horizontal box blur: motion in x
        RgbImage::from_fn(256, 192, |x, y| {
            let mut s = 0u32;
            let mut n = 0u32;
            for d in 0..=blur * 2 {
                let xx = (x + d).saturating_sub(blur).min(255);
                s += base.get_pixel(xx, y).0[0] as u32;
                n += 1;
            }
            let v = (s / n) as u8;
            image::Rgb([v, v, v])
        })
    }

    #[test]
    fn sharp_beats_blurred_and_motion_is_seen() {
        let sharp = measure(&textured(0, 0));
        let blurred = measure(&textured(4, 0));
        assert!(sharp.sharpness > blurred.sharpness * 1.3, "{} vs {}", sharp.sharpness, blurred.sharpness);
        assert!(blurred.motion > sharp.motion, "{} vs {}", blurred.motion, sharp.motion);
        assert!(sharp.clip_high == 0.0 && sharp.clip_low == 0.0);
        let white = measure(&RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255])));
        assert!(white.clip_high > 0.99);
    }

    #[test]
    fn suggestion_with_reason_or_honestly_close() {
        let q = |s: f32, m: f32| Some(Quality { sharpness: s, motion: m, ..Quality::default() });
        let c = vec![Candidate { quality: q(10.0, 0.5), ..Default::default() }, Candidate { quality: q(20.0, 0.1), ..Default::default() }, Candidate { quality: q(12.0, 0.4), ..Default::default() }];
        let s = suggest(&c);
        assert_eq!(s.best, Some(1));
        assert_eq!(s.why.first(), Some(&Why::Sharpest));
        // equal photos: no pretend suggestion
        let same = vec![Candidate { quality: q(10.0, 0.2), ..Default::default() }; 3];
        let s = suggest(&same);
        assert!(s.close && s.best.is_none());
        // missing measurement: nothing suggested
        let s = suggest(&[Candidate::default(), Candidate { quality: q(1.0, 0.0), ..Default::default() }]);
        assert!(s.best.is_none() && !s.close);
    }

    #[test]
    fn closed_eyes_lose() {
        let f = |s: f32, closed: u32| Some(Quality { sharpness: s, faces: Some(2), eyes_closed: Some(closed), face_score: Some(0.6), ..Quality::default() });
        let c = vec![Candidate { quality: f(20.0, 1), ..Default::default() }, Candidate { quality: f(18.0, 0), ..Default::default() }];
        let s = suggest(&c);
        assert_eq!(s.best, Some(1));
        assert!(s.why.contains(&Why::EyesOpen));
    }
}

/// A face of a reviewed photo (frame 0..1 of the upright image, origin top left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// eyes look closed (eye contour flatter than [`EYES_CLOSED_BELOW`])
    pub eyes_closed: bool,
    pub eye_ratio: Option<f32>,
    pub quality: Option<f32>,
}

/// Height/width of the eye contour below which the eyes count as closed.
pub const EYES_CLOSED_BELOW: f32 = 0.15;

/// Faces with eyes and capture quality (Apple Vision; other systems: none). Small faces (< 3 % of the width) are left
/// out: their eyes cannot be judged on a preview.
pub fn faces(img: &RgbImage) -> Option<Vec<Face>> {
    #[cfg(target_os = "macos")]
    {
        let v = ::faces::vision::review_faces(img).ok()?;
        Some(
            v.into_iter()
                .filter(|f| f.w >= 0.03)
                .map(|f| Face { x: f.x, y: f.y, w: f.w, h: f.h, eyes_closed: f.eye_ratio.is_some_and(|r| r < EYES_CLOSED_BELOW), eye_ratio: f.eye_ratio, quality: f.capture_quality })
                .collect(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = img;
        None
    }
}

/// Measure a photo completely: preview values plus faces (if the system can).
pub fn measure_all(img: &RgbImage) -> (Quality, Option<Vec<Face>>) {
    let mut q = measure(img);
    let f = faces(img);
    if let Some(f) = &f {
        q.faces = Some(f.len() as u32);
        q.eyes_closed = Some(f.iter().filter(|x| x.eyes_closed).count() as u32);
        let qs: Vec<f32> = f.iter().filter_map(|x| x.quality).collect();
        q.face_score = (!qs.is_empty()).then(|| qs.iter().sum::<f32>() / qs.len() as f32);
    }
    (q, f)
}
