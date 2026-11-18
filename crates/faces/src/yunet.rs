//! Face detection with YuNet (OpenCV Zoo, MIT, Shiqi Yu), computed in-house with [`crate::graph`]: runs on every
//! system. The model (`face_detection_yunet_2023mar.onnx`, 232 KB) is built into the program; nothing is
//! downloaded. Output per face: box, five landmarks, confidence – the same [`Detection`] as Apple Vision.
//!
//! Decoding follows the published reference (OpenCV `FaceDetectorYN`): three strides (8, 16, 32), one anchor
//! per cell, score = √(class · objectness), box centre = (cell + offset) · stride, size = e^v · stride,
//! landmarks = (cell + offset) · stride; then non-maximum suppression.
//!
//! The network sees faces of about 10 to 300 pixels; photos are therefore searched at two scales (see [`detect`]).

use std::sync::OnceLock;

use image::RgbImage;

use crate::graph::{Graph, Nd};
use crate::Detection;

/// The built-in model file (unchanged from OpenCV Zoo).
pub static MODEL: &[u8] = include_bytes!("../models/face_detection_yunet_2023mar.onnx");
/// SHA-256 of [`MODEL`].
pub const SHA256: &str = "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4";
pub const SOURCE_URL: &str = "https://github.com/opencv/opencv_zoo/raw/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx";

/// Minimum score of a candidate before suppression (OpenCV's default is 0.9). Measured against Apple Vision on 113 free
/// test photos (docs, internal): 0.75 keeps every face both find and drops most false hits (backs of heads, hands, textures).
pub const SCORE_MIN: f32 = 0.75;
/// Boxes overlapping more than this (IoU) with a better one are dropped.
pub const NMS_IOU: f32 = 0.3;
const STRIDES: [usize; 3] = [8, 16, 32];

fn graph() -> Result<&'static Graph, String> {
    static G: OnceLock<Result<Graph, String>> = OnceLock::new();
    G.get_or_init(|| Graph::new(crate::onnx::parse(MODEL)?)).as_ref().map_err(|e| format!("YuNet: {e}"))
}

/// Raw network outputs for an image, as OpenCV names them (`cls_8`, `obj_8`, `bbox_8`, `kps_8`, …). The image
/// is padded with black on the right and bottom to a multiple of 32.
pub fn raw_outputs(img: &RgbImage) -> Result<std::collections::HashMap<String, Nd>, String> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return Err("leeres Bild".into());
    }
    let (pw, ph) = (w.div_ceil(32) * 32, h.div_ceil(32) * 32);
    // network input: BGR, 0…255, planar
    let mut d = vec![0f32; 3 * ph * pw];
    for (i, p) in img.as_raw().chunks_exact(3).enumerate() {
        let (y, x) = (i / w, i % w);
        for c in 0..3 {
            d[(c * ph + y) * pw + x] = p[2 - c] as f32;
        }
    }
    graph()?.run(Nd::new(vec![1, 3, ph, pw], d))
}

/// Faces in the image at its own size (faces of about 10 to 300 pixels), after non-maximum suppression.
pub fn detect_at_size(img: &RgbImage) -> Result<Vec<Detection>, String> {
    let out = raw_outputs(img)?;
    let (pw, ph) = (img.width().div_ceil(32) as usize * 32, img.height().div_ceil(32) as usize * 32);
    let mut cand = Vec::new();
    for s in STRIDES {
        let get = |n: &str| out.get(&format!("{n}_{s}")).ok_or_else(|| format!("YuNet: Ausgang {n}_{s} fehlt"));
        let (cls, obj, bbox, kps) = (get("cls")?, get("obj")?, get("bbox")?, get("kps")?);
        let (cols, rows) = (pw / s, ph / s);
        let cells = cols * rows;
        if cls.data.len() != cells || obj.data.len() != cells || bbox.data.len() != 4 * cells || kps.data.len() != 10 * cells {
            return Err(format!("YuNet: unerwartete Ausgabegröße bei Schritt {s}"));
        }
        let sf = s as f32;
        for idx in 0..cells {
            let score = (cls.data[idx].clamp(0.0, 1.0) * obj.data[idx].clamp(0.0, 1.0)).sqrt();
            if score < SCORE_MIN {
                continue;
            }
            let (r, c) = ((idx / cols) as f32, (idx % cols) as f32);
            let b = &bbox.data[idx * 4..idx * 4 + 4];
            let (cx, cy) = ((c + b[0]) * sf, (r + b[1]) * sf);
            let (bw, bh) = (b[2].exp() * sf, b[3].exp() * sf);
            let k = &kps.data[idx * 10..idx * 10 + 10];
            let mut pts = [(0f32, 0f32); 5];
            for (n, p) in pts.iter_mut().enumerate() {
                *p = ((k[2 * n] + c) * sf, (k[2 * n + 1] + r) * sf);
            }
            if !(cx.is_finite() && cy.is_finite() && bw.is_finite() && bh.is_finite()) {
                continue;
            }
            cand.push(to_detection(cx - bw / 2.0, cy - bh / 2.0, bw, bh, pts, score));
        }
    }
    let mut kept = nms(cand, NMS_IOU);
    // boxes may reach into the black padding or beyond the image: clip to the image
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    for g in &mut kept {
        let (x1, y1) = ((g.x + g.b).min(iw), (g.y + g.h).min(ih));
        g.x = g.x.max(0.0);
        g.y = g.y.max(0.0);
        g.b = (x1 - g.x).max(0.0);
        g.h = (y1 - g.y).max(0.0);
    }
    kept.retain(|g| g.b >= 2.0 && g.h >= 2.0);
    Ok(kept)
}

/// Build a [`Detection`]: the network gives the eyes and mouth corners in the person's order (right eye first,
/// which is on the left in the image); sorted here by image position. Roll from the eye line, yaw estimated
/// from the nose position between the eyes (rough).
fn to_detection(x: f32, y: f32, b: f32, h: f32, p: [(f32, f32); 5], score: f32) -> Detection {
    let (e1, e2) = if p[0].0 <= p[1].0 { (p[0], p[1]) } else { (p[1], p[0]) };
    let (m1, m2) = if p[3].0 <= p[4].0 { (p[3], p[4]) } else { (p[4], p[3]) };
    let points = [e1, e2, p[2], m1, m2];
    let roll = (e2.1 - e1.1).atan2(e2.0 - e1.0);
    let half = ((e2.0 - e1.0).hypot(e2.1 - e1.1) / 2.0).max(1e-3);
    // offset of the nose from the middle of the eyes, along the eye line, relative to half the eye distance
    let (mx, my) = ((e1.0 + e2.0) / 2.0, (e1.1 + e2.1) / 2.0);
    let along = ((p[2].0 - mx) * roll.cos() + (p[2].1 - my) * roll.sin()) / half;
    let yaw = along.clamp(-1.0, 1.0).asin();
    Detection { x, y, b, h, points: Some(points), confidence: score, roll: Some(roll), yaw: Some(yaw) }
}

fn iou(a: &Detection, b: &Detection) -> f32 {
    let iw = (a.x + a.b).min(b.x + b.b) - a.x.max(b.x);
    let ih = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if iw <= 0.0 || ih <= 0.0 {
        return 0.0;
    }
    let s = iw * ih;
    s / (a.b * a.h + b.b * b.h - s)
}

/// Non-maximum suppression: best score first, drop everything overlapping it by more than `limit`.
pub fn nms(mut v: Vec<Detection>, limit: f32) -> Vec<Detection> {
    v.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut kept: Vec<Detection> = Vec::new();
    for d in v {
        if kept.iter().all(|k| iou(k, &d) <= limit) {
            kept.push(d);
        }
    }
    kept
}

/// Longest edge of the coarse pass (large faces up to about the full image height).
pub const COARSE_EDGE: u32 = 640;
/// Longest edge of the fine pass (small faces); larger images are scaled down to this.
pub const FINE_EDGE: u32 = 1600;

fn scaled(img: &RgbImage, edge: u32) -> (RgbImage, f32) {
    let long = img.width().max(img.height());
    if long <= edge {
        return (img.clone(), 1.0);
    }
    let f = edge as f32 / long as f32;
    let (w, h) = (((img.width() as f32 * f).round() as u32).max(1), ((img.height() as f32 * f).round() as u32).max(1));
    (image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle), long as f32 / edge as f32)
}

fn rescale(mut v: Vec<Detection>, f: f32) -> Vec<Detection> {
    for g in &mut v {
        g.x *= f;
        g.y *= f;
        g.b *= f;
        g.h *= f;
        if let Some(p) = &mut g.points {
            for q in p.iter_mut() {
                q.0 *= f;
                q.1 *= f;
            }
        }
    }
    v
}

/// Faces in the whole image: a coarse pass (longest edge [`COARSE_EDGE`]) for large faces and – for larger
/// images – a fine pass (longest edge up to [`FINE_EDGE`]) for small ones; merged by suppression. Coordinates
/// refer to `img`.
pub fn detect(img: &RgbImage) -> Result<Vec<Detection>, String> {
    detect_scales(img, &[COARSE_EDGE, FINE_EDGE])
}

/// Like [`detect`] with selectable passes (longest edges); used for measurements.
pub fn detect_scales(img: &RgbImage, edges: &[u32]) -> Result<Vec<Detection>, String> {
    let long = img.width().max(img.height());
    let mut all = Vec::new();
    let mut done_full = false;
    for &e in edges {
        if done_full {
            break;
        }
        done_full = long <= e;
        let (s, f) = scaled(img, e);
        all.extend(rescale(detect_at_size(&s)?, f));
    }
    Ok(nms(all, NMS_IOU))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_is_the_documented_file() {
        assert_eq!(MODEL.len(), 232_589);
        assert!(graph().is_ok());
    }

    #[test]
    fn detection_points_in_image_order() {
        // the network reports the person's right eye (image left) first; mirrored input must not swap the order
        let g = to_detection(0.0, 0.0, 10.0, 10.0, [(7.0, 4.0), (3.0, 4.0), (5.0, 6.0), (6.5, 8.0), (3.5, 8.0)], 0.9);
        let p = g.points.unwrap();
        assert!(p[0].0 < p[1].0 && p[3].0 < p[4].0);
        assert!(g.roll.unwrap().abs() < 1e-6 && g.yaw.unwrap().abs() < 1e-6);
    }

    #[test]
    fn suppression_keeps_best() {
        let d = |x: f32, s: f32| Detection { x, y: 0.0, b: 10.0, h: 10.0, points: None, confidence: s, roll: None, yaw: None };
        let k = nms(vec![d(0.0, 0.7), d(1.0, 0.9), d(30.0, 0.8)], 0.3);
        assert_eq!(k.len(), 2);
        assert_eq!(k[0].confidence, 0.9);
    }

    #[test]
    fn empty_image_has_no_faces() {
        let img = RgbImage::from_pixel(200, 150, image::Rgb([128, 128, 128]));
        assert!(detect(&img).unwrap().is_empty());
    }
}
