//! Inputs and result of the Classic chain: camera data of the file and the linear working codes.

use x3f::{Camf, PropList};

/// Linear working codes (12-bit units, 4096 = 1.0) of the whole stored sensor area, right before
/// the output stage, plus the active area of the image.
pub struct Codes {
    pub width: usize,
    pub height: usize,
    /// left, top, right (exclusive), bottom (exclusive) of the active area within the stored image
    pub crop: [usize; 4],
    /// interleaved RGB
    pub data: Vec<i32>,
}

/// Camera data (CAMF), header properties and the colour space set in the camera.
pub struct Meta<'a> {
    pub camf: &'a Camf,
    pub props: &'a PropList,
    /// colour space set in the camera is Adobe RGB (EXIF of the embedded JPEG)
    pub adobe: bool,
}

impl Meta<'_> {
    /// All values of a camera-data entry, `None` when missing.
    pub fn values(&self, name: &str) -> Option<Vec<f64>> {
        self.camf.matrix(name).ok().map(|m| m.as_f64())
    }
    /// First value of an entry, `default` when missing.
    pub fn scalar(&self, name: &str, default: f64) -> f64 {
        self.values(name).and_then(|v| v.first().copied()).unwrap_or(default)
    }
    /// An entry with exactly `N` values.
    pub fn fixed<const N: usize>(&self, name: &str) -> Option<[f64; N]> {
        let v = self.values(name)?;
        (v.len() == N).then(|| std::array::from_fn(|i| v[i]))
    }
    /// Capture ISO: the camera data's value, else (or when it is 0) the header's ISO (default 100).
    pub fn capture_iso(&self) -> f64 {
        let header = self.props.get("ISO").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(100.0);
        let c = self.scalar("CaptureISO", header);
        if c == 0.0 { header } else { c }
    }
}
