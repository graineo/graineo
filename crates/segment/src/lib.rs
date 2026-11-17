//! Selection masks from images: which pixels belong to the subject, a person or a clicked object. The result is a
//! weight map 0..1 per instance in the size of the input image; it never changes pixels of the photo. No file or
//! database access: the caller develops the image, stores the result and decides what the mask does.
//!
//! - [`analyse`]: foreground objects or people with the system's image analysis (macOS 14+ Vision; people also on
//!   macOS 12/13 as one whole mask). Other systems: [`available`] says no.

pub mod depth;
pub mod embedded;
pub mod inpaint;
pub mod sam;
pub mod segments;
pub mod sky;
#[cfg(target_os = "macos")]
mod vision;

/// What to find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// the salient foreground objects (no classes)
    Subject,
    /// people
    Person,
}

/// Weight maps of the instances found, each `width × height` values 0..1 (row-major, top left first).
#[derive(Clone, Debug, Default)]
pub struct Instances {
    pub width: u32,
    pub height: u32,
    pub masks: Vec<Vec<f32>>,
}

impl Instances {
    /// Union (maximum) of the chosen instances (1-based indices; empty = all).
    pub fn union(&self, chosen: &[u8]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.width as usize * self.height as usize];
        for (i, m) in self.masks.iter().enumerate() {
            if chosen.is_empty() || chosen.contains(&((i + 1) as u8)) {
                for (o, v) in out.iter_mut().zip(m) {
                    *o = o.max(*v);
                }
            }
        }
        out
    }
}

/// How the system can answer a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// separate instances (macOS 14+)
    Instances,
    /// one mask for all people together (macOS 12/13, people only)
    Whole,
    /// not on this system
    None,
}

/// What this system can do for `kind`.
pub fn available(kind: Kind) -> Support {
    #[cfg(target_os = "macos")]
    {
        vision::support(kind)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        Support::None
    }
}

/// Step of a model run, reported to the caller before it begins (so it can name it, or stop the run between steps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// reading and preparing the model file
    Load,
    /// computing
    Run,
}

/// Error of a run that the caller stopped between two steps (its step callback returned `false`).
pub const CANCELLED: &str = "cancelled";

/// Find the instances of `kind` in an upright 8-bit sRGB image. Empty result: nothing found.
pub fn analyse(img: &image::RgbImage, kind: Kind) -> Result<Instances, String> {
    #[cfg(target_os = "macos")]
    {
        vision::analyse(img, kind)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (img, kind);
        Err("subject and people selection is only available on macOS".into())
    }
}

/// Bilinear resampling of a weight map (used for low-resolution results).
pub fn resize(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0.0; dw * dh];
    for y in 0..dh {
        let fy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (fy as usize, fy - fy.floor());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let fx = ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (fx as usize, fx - fx.floor());
            let x1 = (x0 + 1).min(sw - 1);
            let top = src[y0 * sw + x0] + (src[y0 * sw + x1] - src[y0 * sw + x0]) * tx;
            let bottom = src[y1 * sw + x0] + (src[y1 * sw + x1] - src[y1 * sw + x0]) * tx;
            out[y * dw + x] = top + (bottom - top) * ty;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_takes_the_maximum_of_the_chosen() {
        let i = Instances { width: 2, height: 1, masks: vec![vec![1.0, 0.0], vec![0.2, 0.7]] };
        assert_eq!(i.union(&[]), vec![1.0, 0.7]);
        assert_eq!(i.union(&[2]), vec![0.2, 0.7]);
    }

    #[test]
    fn resize_keeps_flat_and_edges() {
        let src = vec![0.0, 1.0, 0.0, 1.0];
        let out = resize(&src, 2, 2, 4, 4);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[3], 1.0);
        assert!(out.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    /// Real Vision run on a synthetic image (a bright disc on grey): only meaningful on macOS 14+.
    #[test]
    #[cfg(target_os = "macos")]
    fn subject_on_a_synthetic_disc() {
        if available(Kind::Subject) != Support::Instances {
            return;
        }
        let img = image::RgbImage::from_fn(512, 384, |x, y| {
            let d = ((x as f32 - 256.0).powi(2) + (y as f32 - 192.0).powi(2)).sqrt();
            if d < 90.0 { image::Rgb([230, 60, 40]) } else { image::Rgb([90, 110, 100]) }
        });
        let t = std::time::Instant::now();
        let r = analyse(&img, Kind::Subject).unwrap();
        eprintln!("Vision subject 512×384: {} instances in {:?}", r.masks.len(), t.elapsed());
        if let Some(m) = r.masks.first() {
            assert_eq!(m.len(), 512 * 384);
            let centre = m[192 * 512 + 256];
            let corner = m[10 * 512 + 10];
            eprintln!("centre {centre}, corner {corner}");
            assert!(centre > corner);
        }
    }
}

#[cfg(test)]
mod photo_tests {
    /// Real photo (PNG copy in a scratch folder): GRAINEO_TEST_SEGMENT_IMAGE=<file.png>; skipped otherwise.
    #[test]
    fn subject_on_a_photo() {
        let Some(p) = std::env::var_os("GRAINEO_TEST_SEGMENT_IMAGE") else { return };
        let img = image::open(&p).unwrap().to_rgb8();
        for kind in [super::Kind::Subject, super::Kind::Person] {
            let t = std::time::Instant::now();
            let r = super::analyse(&img, kind).unwrap();
            let cover: Vec<f32> = r.masks.iter().map(|m| m.iter().filter(|v| **v > 0.5).count() as f32 / m.len() as f32).collect();
            eprintln!("{kind:?} {}×{}: {} instances {cover:?} in {:?}", img.width(), img.height(), r.masks.len(), t.elapsed());
        }
    }
}
