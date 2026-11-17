//! Relative depth of a photo with Depth Anything V2 Small (Apache-2.0 code and Small weights; the Base and Large
//! weights carry a non-commercial licence and are not used), ONNX export of onnx-community/depth-anything-v2-small, run
//! with the pure-Rust ONNX runtime `tract` on every system. The model file is a downloadable module; this crate only
//! reads it.
//!
//! The model estimates *relative inverse depth* (disparity up to an unknown scale and shift): larger = nearer. There
//! is no metric distance. For a mask only the order matters, so [`nearness_ranks`] turns the estimate into ranks
//! 0 (nearest) … 65535 (farthest), spread evenly over the pixels of the photo: a range "0–30 %" then means the nearest
//! 30 % of the picture, whatever the scene.
use std::path::Path;

use image::RgbImage;
use tract_onnx::prelude::*;

/// File of the module (name in the module folder).
pub const MODEL: &str = "model.onnx";
/// Short side of the model input (the model's training size; a multiple of the patch size 14).
pub const SHORT_SIDE: u32 = 518;
const PATCH: u32 = 14;

/// Estimated relative depth: `width × height` disparity values (larger = nearer), row-major.
#[derive(Clone, Debug)]
pub struct Estimate {
    pub width: u32,
    pub height: u32,
    pub disparity: Vec<f32>,
}

fn err(e: impl std::fmt::Display) -> String {
    // with the chain of causes (tract reports which node and why)
    format!("{e:#}")
}

/// Size of the model input for an image of `w × h`: short side `short`, both sides multiples of 14.
pub fn input_size(w: u32, h: u32, short: u32) -> (u32, u32) {
    let s = short as f32 / w.min(h).max(1) as f32;
    let round = |v: f32| (((v / PATCH as f32).round() as u32).max(1)) * PATCH;
    (round(w as f32 * s), round(h as f32 * s))
}

/// Estimate the depth of an upright 8-bit sRGB image (any size; scaled to a short side of `short`, usually
/// [`SHORT_SIDE`]); reports each [`crate::Step`] before it begins, `step` returning `false` stops with
/// [`crate::CANCELLED`]. The result has the size of the model input.
pub fn estimate_steps(dir: &Path, img: &RgbImage, short: u32, step: &mut dyn FnMut(crate::Step) -> bool) -> Result<Estimate, String> {
    let (w, h) = input_size(img.width(), img.height(), short);
    let small = image::imageops::resize(img, w, h, image::imageops::FilterType::CatmullRom);
    if !step(crate::Step::Load) {
        return Err(crate::CANCELLED.into());
    }
    // the export annotates shapes with symbols (batch, height, width) that would not unify with the fixed input
    let plan = tract_onnx::onnx()
        .with_ignore_output_shapes(true)
        .with_ignore_value_info(true)
        .model_for_path(dir.join(MODEL))
        .map_err(err)?
        .with_input_fact(0, f32::fact([1, 3, h as usize, w as usize]).into())
        .map_err(err)?
        .into_optimized()
        .map_err(err)?
        .into_runnable()
        .map_err(err)?;
    if !step(crate::Step::Run) {
        return Err(crate::CANCELLED.into());
    }
    // ImageNet normalisation, NCHW (as the reference preprocessing)
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let (wu, hu) = (w as usize, h as usize);
    let mut data = vec![0.0f32; 3 * wu * hu];
    for (i, p) in small.pixels().enumerate() {
        for c in 0..3 {
            data[c * wu * hu + i] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, hu, wu), data).map_err(err)?;
    let out = plan.run(tvec!(input.into_tensor().into())).map_err(err)?;
    let v = out[0].to_plain_array_view::<f32>().map_err(err)?;
    let disparity: Vec<f32> = v.iter().copied().collect();
    if disparity.len() != wu * hu {
        return Err(format!("unexpected model output {:?}", v.shape()));
    }
    if disparity.iter().any(|x| !x.is_finite()) {
        return Err("model output not finite".into());
    }
    Ok(Estimate { width: w, height: h, disparity })
}

/// Ranks of nearness from disparity (larger = nearer): 0 = nearest … 65535 = farthest, spread evenly over the pixels.
/// Equal values share one rank (the middle of their run), so a flat area stays one value. Many pixels sharing the
/// smallest disparity (at least 1 %: a sky the estimate puts at infinity) take 65535, and the other pixels spread over
/// the whole scale alone; otherwise such a sky would sit in the middle of the scale and push the land into its nearest
/// part.
pub fn nearness_ranks(disparity: &[f32]) -> Vec<u16> {
    let n = disparity.len();
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<u32> = (0..n as u32).collect();
    // nearest (largest disparity) first
    order.sort_unstable_by(|&a, &b| disparity[b as usize].total_cmp(&disparity[a as usize]));
    let mut out = vec![0u16; n];
    // the run of the farthest value
    let far = disparity[order[n - 1] as usize];
    let far_run = order.iter().rev().take_while(|&&k| disparity[k as usize] == far).count();
    let spread = if far_run * 100 >= n && far_run < n { n - far_run } else { n };
    // the others end one below, so the far run stays the farthest
    let top = if spread < n { 65534.0 } else { 65535.0 };
    for &k in &order[spread..] {
        out[k as usize] = 65535;
    }
    let mut i = 0;
    while i < spread {
        let v = disparity[order[i] as usize];
        let mut j = i + 1;
        while j < spread && disparity[order[j] as usize] == v {
            j += 1;
        }
        // middle of the run, scaled to 0..65535
        let mid = (i + j - 1) as f64 / 2.0;
        let r = if spread > 1 { (mid / (spread - 1) as f64 * top).round() as u16 } else { 0 };
        for &k in &order[i..j] {
            out[k as usize] = r;
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_size_keeps_aspect_and_patch_multiples() {
        assert_eq!(input_size(6000, 4000, 518), (784, 518));
        assert_eq!(input_size(3000, 4000, 518), (518, 686));
        let (w, h) = input_size(1, 1, 518);
        assert_eq!((w % 14, h % 14), (0, 0));
    }

    #[test]
    fn ranks_order_near_to_far_and_share_ties() {
        // disparity: 5 = nearest, 0 = farthest (sky twice)
        let r = nearness_ranks(&[0.0, 5.0, 2.0, 0.0, 3.0]);
        assert_eq!(r[1], 0, "nearest first");
        assert!(r[4] < r[2] && r[2] < r[0]);
        assert_eq!(r[0], r[3], "ties share a rank");
        assert_eq!(r[0], 65535, "many pixels at the farthest value: the end of the scale");
        assert_eq!((r[1], r[4], r[2]), (0, 32767, 65534), "the others spread alone");
        // ties elsewhere: the middle of their run
        let r = nearness_ranks(&[5.0, 2.0, 2.0, 1.0, 0.0]);
        assert_eq!((r[0], r[1], r[2], r[3], r[4]), (0, 32767, 32767, 65534, 65535));
        // fewer than 1 % at the farthest value: all spread
        let v: Vec<f32> = (0..200).map(|i| i as f32).collect();
        let r = nearness_ranks(&v);
        assert_eq!((r[199], r[0]), (0, 65535));
        assert!(nearness_ranks(&[]).is_empty());
        assert_eq!(nearness_ranks(&[1.0]), vec![0]);
    }

    /// Needs the module file: GRAINEO_TEST_DEPTH=<folder with model.onnx> (skipped otherwise). A synthetic scene: a
    /// near disc in front of a floor that recedes towards the top.
    #[test]
    fn depth_of_a_synthetic_scene() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_DEPTH").map(std::path::PathBuf::from) else { return };
        let img = RgbImage::from_fn(768, 512, |x, y| {
            let d = ((x as f32 - 384.0).powi(2) + (y as f32 - 380.0).powi(2)).sqrt();
            if d < 90.0 {
                image::Rgb([200, 60, 40])
            } else if y < 200 {
                image::Rgb([120, 160, 220])
            } else {
                // checker floor, finer towards the horizon
                let f = 4.0 + (y as f32 - 200.0) * 0.25;
                let c = ((x as f32 / f) as i32 + (y as f32 / f * 0.5) as i32) % 2;
                if c == 0 { image::Rgb([90, 80, 60]) } else { image::Rgb([150, 140, 110]) }
            }
        });
        let t = std::time::Instant::now();
        let e = estimate_steps(&dir, &img, SHORT_SIDE, &mut |_| true).unwrap();
        eprintln!("depth {}×{} in {:?}", e.width, e.height, t.elapsed());
        let r = nearness_ranks(&e.disparity);
        let at = |x: f32, y: f32| r[(y * e.height as f32) as usize * e.width as usize + (x * e.width as f32) as usize];
        // the disc and the floor in front are nearer than the sky
        assert!(at(0.5, 0.74) < at(0.5, 0.1), "disc {} sky {}", at(0.5, 0.74), at(0.5, 0.1));
        assert!(at(0.1, 0.95) < at(0.1, 0.1));
    }
}
