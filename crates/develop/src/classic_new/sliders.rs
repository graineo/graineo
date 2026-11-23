//! Classic with the noise sliders away from the middle position. The sliders act inside the chain
//! (range-based reconstruction from ISO 400 on, see [`super::region::with_noise_sliders`]), so the
//! chain runs again on the file with them, and its codes are converted to the linear image of the
//! preparation exactly as the preparation converts the neutral result (inverse of the shot mode's
//! conversion matrix, gain relation of the calibration, fractional codes from the detail step).

use std::path::Path;
use std::sync::Arc;

use x3f::X3fFile;

use crate::{Calibration, Linear};

/// Linear image of the chain on `path` with slider steps `nr` (luminance, colour; −1 … 4, 2 =
/// middle), converted with the preparation's calibration `cal` for a `w × h` active area. `None`
/// where the chain or the conversion is not available.
pub fn linear(path: &Path, cal: &Calibration, w: usize, h: usize, nr: (i8, i8)) -> Option<Linear> {
    let (luma, chroma) = (f64::from(nr.0.clamp(-1, 4)) / 4.0, f64::from(nr.1.clamp(-1, 4)) / 4.0);
    let file = X3fFile::open(path).ok()?;
    let camf = file.camf().ok()?;
    let props = file.properties().ok()?;
    let mode = cal.camera_mode.clone();
    let run = || super::run(&file, &camf, &props, &cal.wb_name, &mode, file.header_rotation().unwrap_or(0) as u32);
    let (result, high_pass) = super::region::with_noise_sliders(luma, chroma, || super::detail::with_high_pass(run));
    let codes = result.ok()?;
    let [x0, y0, x1, y1] = codes.crop;
    let spp = cal.mode(Some(&mode)).classic.clone()?;
    if x1 - x0 != w || y1 - y0 != h {
        return None;
    }
    let a_inv = crate::matrix::Mat3(std::array::from_fn(|i| f64::from(spp.a[i]))).inverse()?.to_f32();
    let k: [f32; 3] = std::array::from_fn(|c| (spp.wb_gain[c] / (spp.k_raw[c] * cal.wb_gain[c])) as f32);
    let high_pass = high_pass.filter(|hp| hp.width == w && hp.height == h && hp.index.len() == w * h * 3);
    let position = high_pass.as_ref().map(|hp| hp.index.as_slice());
    let mut data = vec![0f32; w * h * 3];
    use rayon::prelude::*;
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let src = &codes.data[((y0 + y) * codes.width + x0) * 3..((y0 + y) * codes.width + x1) * 3];
        let idx = position.map(|l| &l[y * w * 3..(y + 1) * w * 3]);
        for (x, (o, c)) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)).enumerate() {
            let v: [f32; 3] = match idx {
                Some(l) => std::array::from_fn(|i| super::output::code_with_position(l[x * 3 + i]) / 4096.0),
                None => [c[0] as f32 / 4096.0, c[1] as f32 / 4096.0, c[2] as f32 / 4096.0],
            };
            for r in 0..3 {
                o[r] = (a_inv[r * 3] * v[0] + a_inv[r * 3 + 1] * v[1] + a_inv[r * 3 + 2] * v[2]) * k[r];
            }
        }
    });
    let detail = high_pass.map(|hp| Arc::new(crate::sharpness::SharpDetail { l: hp.values, base: crate::sharpness::CLASSIC_HIGH_PASS, scale: 1.0 }));
    Some(Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail })
}
