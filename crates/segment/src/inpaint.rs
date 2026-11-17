//! Removing objects: Big-LaMa (Apache-2.0 code advimman/lama; ONNX export Carve/LaMa-ONNX, fixed 512 × 512), run with
//! tract on every system. The model INVENTS the content of the marked area from its surroundings; the caller labels it
//! as such. Input: 8-bit sRGB region and a mask (true = remove), both resized to 512 × 512 here; output: the filled
//! region at 512 × 512 (8-bit sRGB), equal to the input outside the mask.
use std::path::Path;

use image::RgbImage;
use tract_onnx::prelude::*;

/// File of the module.
pub const MODEL: &str = "lama_fp32.onnx";
/// Fixed model size.
pub const SIDE: u32 = 512;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Fill the masked area of `region` (`mask`: one weight per pixel of `region`, > 0.5 = remove). Returns 512 × 512 RGB.
pub fn fill(dir: &Path, region: &RgbImage, mask: &[f32]) -> Result<RgbImage, String> {
    fill_steps(dir, region, mask, &mut |_| true)
}

/// [`fill`], reporting each [`crate::Step`] before it begins; `step` returning `false` stops the run there with
/// [`crate::CANCELLED`].
pub fn fill_steps(dir: &Path, region: &RgbImage, mask: &[f32], step: &mut dyn FnMut(crate::Step) -> bool) -> Result<RgbImage, String> {
    let (rw, rh) = (region.width(), region.height());
    if mask.len() != (rw * rh) as usize {
        return Err("mask size".into());
    }
    let n = SIDE as usize;
    let img = image::imageops::resize(region, SIDE, SIDE, image::imageops::FilterType::Triangle);
    let mut input = vec![0f32; 3 * n * n];
    let mut m = vec![0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let (sx, sy) = ((x * rw as usize / n).min(rw as usize - 1), (y * rh as usize / n).min(rh as usize - 1));
            let on = mask[sy * rw as usize + sx] > 0.5;
            m[y * n + x] = if on { 1.0 } else { 0.0 };
            let p = img.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                input[c * n * n + y * n + x] = if on { 0.0 } else { p[c] as f32 / 255.0 };
            }
        }
    }
    if !step(crate::Step::Load) {
        return Err(crate::CANCELLED.into());
    }
    let plan = tract_onnx::onnx()
        .model_for_path(dir.join(MODEL))
        .map_err(err)?
        .with_input_fact(0, f32::fact([1, 3, n, n]).into())
        .map_err(err)?
        .with_input_fact(1, f32::fact([1, 1, n, n]).into())
        .map_err(err)?
        .into_optimized()
        .map_err(err)?
        .into_runnable()
        .map_err(err)?;
    if !step(crate::Step::Run) {
        return Err(crate::CANCELLED.into());
    }
    let a = tract_ndarray::Array4::from_shape_vec((1, 3, n, n), input).map_err(err)?;
    let b = tract_ndarray::Array4::from_shape_vec((1, 1, n, n), m).map_err(err)?;
    let res = plan.run(tvec!(a.into_tensor().into(), b.into_tensor().into())).map_err(err)?;
    let o = res[0].to_plain_array_view::<f32>().map_err(err)?;
    // output already in 0..255
    Ok(RgbImage::from_fn(SIDE, SIDE, |x, y| {
        let v = |c: usize| o[[0, c, y as usize, x as usize]].clamp(0.0, 255.0).round() as u8;
        image::Rgb([v(0), v(1), v(2)])
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the module file: GRAINEO_TEST_LAMA=<folder> (skipped otherwise).
    #[test]
    fn fills_a_gradient() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_LAMA").map(std::path::PathBuf::from) else { return };
        let region = RgbImage::from_fn(300, 200, |x, y| image::Rgb([(x * 255 / 300) as u8, (y * 255 / 200) as u8, 120]));
        let mask: Vec<f32> = (0..300 * 200).map(|i| if (120..180).contains(&(i % 300)) && (70..130).contains(&(i / 300)) { 1.0 } else { 0.0 }).collect();
        let t = std::time::Instant::now();
        let out = fill(&dir, &region, &mask).unwrap();
        eprintln!("LaMa 512: {:?}", t.elapsed());
        // centre of the hole close to the true gradient
        let p = out.get_pixel(256, 256);
        let expect = [128.0, 128.0, 120.0];
        let e: f32 = (0..3).map(|c| (p[c] as f32 - expect[c]).abs()).sum::<f32>() / 3.0;
        assert!(e < 25.0, "{p:?}");
    }
}
