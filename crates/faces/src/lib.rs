//! Faces in photos: detect, recognize, assign, group.

pub mod matching;
pub mod align;
pub mod net;
pub mod onnx;
pub mod sface;
#[cfg(target_os = "macos")]
pub mod vision;

/// Face detection – on macOS with Apple's Vision. On other systems there is no detector (yet):
/// the calls report an error; recognition and assignment (SFace) run everywhere.
pub mod detector {
    #[cfg(not(target_os = "macos"))]
    use image::RgbImage;

    #[cfg(not(target_os = "macos"))]
    use crate::Detection;

    /// Is there a detector on this system?
    pub const AVAILABLE: bool = cfg!(target_os = "macos");

    #[cfg(target_os = "macos")]
    pub use crate::vision::{detect, detect_tile_grid, detect_with_tiles};

    #[cfg(not(target_os = "macos"))]
    const MISSING: &str = "Gesichter finden gibt es auf diesem System noch nicht";

    #[cfg(not(target_os = "macos"))]
    pub fn detect(_img: &RgbImage) -> Result<Vec<Detection>, String> {
        Err(MISSING.into())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn detect_with_tiles(_img: &RgbImage) -> Result<Vec<Detection>, String> {
        Err(MISSING.into())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn detect_tile_grid(_img: &RgbImage, _n: usize, _fraction: f32) -> Result<Vec<Detection>, String> {
        Err(MISSING.into())
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
