//! SFace (MobileFaceNet, OpenCV Zoo, Apache-2.0): 112×112 face → 128 numbers. The network structure is in
//! the ONNX file; computation uses [`crate::net`].

use std::path::Path;

use crate::net::Net;
use crate::onnx;

/// SHA-256 of the expected model file `face_recognition_sface_2021dec.onnx` (OpenCV Zoo).
pub const SHA256: &str = "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79";
pub const SOURCE_URL: &str = "https://media.githubusercontent.com/media/opencv/opencv_zoo/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx";
/// Size of the model file in bytes (shown in the notice before the download).
pub const SIZE_BYTES: u64 = 38_696_353;
pub const FILE_NAME: &str = "face_recognition_sface_2021dec.onnx";
pub const EMBEDDING_LEN: usize = 128;

pub struct Sface {
    net: Net,
}

impl Sface {
    pub fn from_bytes(data: &[u8]) -> Result<Sface, String> {
        Ok(Sface { net: Net::new(onnx::parse(data)?)? })
    }

    pub fn load(path: &Path) -> Result<Sface, String> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes(&data)
    }

    /// Feature vector (normalized to length 1) for an aligned 112×112 RGB face.
    pub fn embedding(&self, rgb112: &[u8]) -> Result<Vec<f32>, String> {
        let mut v = self.net.infer(rgb112, crate::align::SIDE)?;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        v.iter_mut().for_each(|x| *x /= n);
        Ok(v)
    }
}

/// Cosine similarity of two normalized vectors.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
