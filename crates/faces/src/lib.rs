//! Faces in photos: detect, recognize, assign, group.

pub mod matching;
pub mod align;
pub mod net;
pub mod onnx;
pub mod sface;
#[cfg(target_os = "macos")]
pub mod vision;

pub mod gemm;
pub mod graph;
pub mod yunet;

/// Face detection. Two sources: Apple's Vision (macOS, Neural Engine) and YuNet, computed in-house (every
/// system, see [`crate::yunet`]). macOS uses Vision; `GRAINEO_FACE_BACKEND=yunet` forces YuNet there
/// (measurements). Other systems always use YuNet.
pub mod detector {
    use image::RgbImage;

    use crate::Detection;

    /// Is there a detector on this system? (Since YuNet: always.)
    pub const AVAILABLE: bool = true;

    /// Should the automatic face search be on by default? On macOS yes (Vision is fast and quiet); elsewhere the
    /// search computes on the CPU and is switched on by the user.
    pub const AUTOMATIC_DEFAULT: bool = cfg!(target_os = "macos");

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Backend {
        Vision,
        YuNet,
    }

    /// The detector in use: on macOS Vision unless `GRAINEO_FACE_BACKEND=yunet`; elsewhere YuNet.
    pub fn backend() -> Backend {
        #[cfg(target_os = "macos")]
        {
            static B: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();
            *B.get_or_init(|| if std::env::var("GRAINEO_FACE_BACKEND").is_ok_and(|v| v.eq_ignore_ascii_case("yunet")) { Backend::YuNet } else { Backend::Vision })
        }
        #[cfg(not(target_os = "macos"))]
        Backend::YuNet
    }

    /// Does the detector run on the CPU of this program (slower, louder than the system's Vision)?
    pub fn computes_itself() -> bool {
        backend() == Backend::YuNet
    }

    /// Faces in the whole image.
    pub fn detect(img: &RgbImage) -> Result<Vec<Detection>, String> {
        match backend() {
            #[cfg(target_os = "macos")]
            Backend::Vision => crate::vision::detect(img),
            _ => crate::yunet::detect(img),
        }
    }

    /// Faces in the whole image and in overlapping tiles (small faces are relatively larger in a tile and
    /// are found that way); duplicate detections are merged (the larger box wins). YuNet searches small faces
    /// with its fine pass instead of tiles.
    pub fn detect_with_tiles(img: &RgbImage) -> Result<Vec<Detection>, String> {
        match backend() {
            Backend::Vision => detect_tile_grid(img, 2, 0.6),
            Backend::YuNet => crate::yunet::detect(img),
        }
    }

    /// Like [`detect_with_tiles`] with a selectable grid: `n × n` tiles, each `fraction` of the edge length (the rest overlaps).
    pub fn detect_tile_grid(img: &RgbImage, n: usize, fraction: f32) -> Result<Vec<Detection>, String> {
        tile_grid(img, n, fraction, detect)
    }

    fn overlap_fraction(a: &Detection, b: &Detection) -> f32 {
        let iw = (a.x + a.b).min(b.x + b.b) - a.x.max(b.x);
        let ih = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        if iw <= 0.0 || ih <= 0.0 {
            return 0.0;
        }
        let s = iw * ih;
        s / (a.b * a.h + b.b * b.h - s)
    }

    /// Whole image plus `n × n` tiles with any detector.
    pub fn tile_grid(img: &RgbImage, n: usize, fraction: f32, detect: fn(&RgbImage) -> Result<Vec<Detection>, String>) -> Result<Vec<Detection>, String> {
        let mut all = detect(img)?;
        let (w, h) = (img.width(), img.height());
        let (kw, kh) = ((w as f32 * fraction) as u32, (h as f32 * fraction) as u32);
        let step = if n > 1 { (1.0 - fraction) / (n - 1) as f32 } else { 0.0 };
        let mut origins: Vec<(f32, f32)> = Vec::new();
        for j in 0..n {
            for i in 0..n {
                origins.push((i as f32 * step, j as f32 * step));
            }
        }
        for &(fx, fy) in &origins {
            let (ox, oy) = ((w as f32 * fx) as u32, (h as f32 * fy) as u32);
            let tile = image::imageops::crop_imm(img, ox, oy, kw.min(w - ox), kh.min(h - oy)).to_image();
            for mut g in detect(&tile).unwrap_or_default() {
                g.x += ox as f32;
                g.y += oy as f32;
                if let Some(p) = &mut g.points {
                    for q in p.iter_mut() {
                        q.0 += ox as f32;
                        q.1 += oy as f32;
                    }
                }
                // faces at the tile border are often cut off: only accept them if they do not touch the border
                // that lies inside the image
                let (edge_left, edge_top) = (g.x - ox as f32 <= 2.0 && ox > 0, g.y - oy as f32 <= 2.0 && oy > 0);
                let (edge_right, edge_bottom) = (g.x + g.b >= (ox + tile.width()) as f32 - 2.0 && ox + tile.width() < w, g.y + g.h >= (oy + tile.height()) as f32 - 2.0 && oy + tile.height() < h);
                if edge_left || edge_top || edge_right || edge_bottom {
                    continue;
                }
                match all.iter().position(|a| overlap_fraction(a, &g) > 0.35) {
                    Some(i) => {
                        if g.b * g.h > all[i].b * all[i].h {
                            all[i] = g;
                        }
                    }
                    None => all.push(g),
                }
            }
        }
        Ok(all)
    }
}

/// A detected face (pixel coordinates, origin top left).
#[derive(Clone, Debug)]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub b: f32,
    pub h: f32,
    /// left eye, right eye, nose tip, left, right mouth corner ("left" as seen in the image)
    pub points: Option<[(f32, f32); 5]>,
    /// Detector confidence 0…1
    pub confidence: f32,
    /// Head tilt (roll) and turn (yaw) in radians, if known
    pub roll: Option<f32>,
    pub yaw: Option<f32>,
}
