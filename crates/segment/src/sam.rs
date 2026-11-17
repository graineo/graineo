//! Click selection with MobileSAM (Apache-2.0 code, MIT weights; ONNX export of Acly/MobileSAM), run with the pure-Rust
//! ONNX runtime `tract` on every system. Two parts: the image encoder runs once per photo (≈ 1 s), the small mask
//! decoder once per click (≈ 0.1 s). The model files are a downloadable module; this crate only reads them.
//!
//! Pre- and post-processing as in the reference export: the image with a long side of 1024 px as raw RGB 0..255 (HWC;
//! the encoder normalises and pads by itself); click coordinates in pixels of that image plus one padding point
//! (0, 0) with label −1; the decoder gives mask logits in the size of the input image, > 0 = selected.
use std::path::Path;
use std::sync::Arc;

use image::RgbImage;
use tract_onnx::prelude::*;

type Plan = Arc<TypedSimplePlan>;

/// Files of the module (names in the module folder).
pub const ENCODER: &str = "mobile_sam_image_encoder.onnx";
pub const DECODER: &str = "sam_mask_decoder_multi.onnx";
/// Long side of the encoder input.
pub const SIDE: u32 = 1024;

/// An encoded photo: image embeddings and the size of the encoder input.
pub struct Encoded {
    embeddings: TValue,
    pub width: u32,
    pub height: u32,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Encode an image (any size; scaled to a long side of [`SIDE`]).
pub fn encode(dir: &Path, img: &RgbImage) -> Result<Encoded, String> {
    encode_steps(dir, img, &mut |_| true)
}

/// [`encode`], reporting each [`crate::Step`] before it begins; `step` returning `false` stops the run there with
/// [`crate::CANCELLED`].
pub fn encode_steps(dir: &Path, img: &RgbImage, step: &mut dyn FnMut(crate::Step) -> bool) -> Result<Encoded, String> {
    let scale = SIDE as f32 / img.width().max(img.height()) as f32;
    let (w, h) = (((img.width() as f32 * scale).round() as u32).max(1), ((img.height() as f32 * scale).round() as u32).max(1));
    let small = image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
    if !step(crate::Step::Load) {
        return Err(crate::CANCELLED.into());
    }
    let plan: Plan = tract_onnx::onnx()
        .model_for_path(dir.join(ENCODER))
        .map_err(err)?
        .with_input_fact(0, f32::fact([h as usize, w as usize, 3]).into())
        .map_err(err)?
        .into_optimized()
        .map_err(err)?
        .into_runnable()
        .map_err(err)?;
    if !step(crate::Step::Run) {
        return Err(crate::CANCELLED.into());
    }
    let data: Vec<f32> = small.as_raw().iter().map(|&v| v as f32).collect();
    let input = tract_ndarray::Array3::from_shape_vec((h as usize, w as usize, 3), data).map_err(err)?;
    let out = plan.run(tvec!(input.into_tensor().into())).map_err(err)?;
    Ok(Encoded { embeddings: out[0].clone(), width: w, height: h })
}

/// Selection for click points `(x, y, label)`: x, y 0..1 of the image, label +1 include / −1 exclude. Returns weights
/// 0..1 in the size of the encoder input (`Encoded::width × height`).
pub fn decode(dir: &Path, e: &Encoded, points: &[[f32; 3]]) -> Result<Vec<f32>, String> {
    decode_prompt(dir, e, points, None)
}

/// Prompt of the decoder in pixels of the encoder input: the click points (label 1 include, 0 exclude), then either
/// the box as two corners (labels 2 top left, 3 bottom right) or, without a box, the padding point (0, 0) with label
/// −1, as in the reference export.
pub fn prompt(points: &[[f32; 3]], rect: Option<[f32; 4]>, w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut coords = Vec::with_capacity(points.len() * 2 + 4);
    let mut labels = Vec::with_capacity(points.len() + 2);
    for p in points {
        coords.extend([p[0] * w as f32, p[1] * h as f32]);
        labels.push(if p[2] > 0.0 { 1.0f32 } else { 0.0 });
    }
    match rect {
        Some([x0, y0, x1, y1]) => {
            coords.extend([x0.min(x1) * w as f32, y0.min(y1) * h as f32, x0.max(x1) * w as f32, y0.max(y1) * h as f32]);
            labels.extend([2.0, 3.0]);
        }
        None => {
            coords.extend([0.0, 0.0]);
            labels.push(-1.0);
        }
    }
    (coords, labels)
}

/// [`decode`] with an optional box `[x0, y0, x1, y1]` (0..1 of the image) around the object; at least one point or
/// the box.
pub fn decode_prompt(dir: &Path, e: &Encoded, points: &[[f32; 3]], rect: Option<[f32; 4]>) -> Result<Vec<f32>, String> {
    if points.is_empty() && rect.is_none() {
        return Err("no click points".into());
    }
    let (w, h) = (e.width as usize, e.height as usize);
    let (coords, labels) = prompt(points, rect, w, h);
    let n = labels.len();
    let plan: Plan = tract_onnx::onnx()
        .model_for_path(dir.join(DECODER))
        .map_err(err)?
        .with_input_fact(0, f32::fact([1, 256, 64, 64]).into())
        .map_err(err)?
        .with_input_fact(1, f32::fact([1, n, 2]).into())
        .map_err(err)?
        .with_input_fact(2, f32::fact([1, n]).into())
        .map_err(err)?
        .with_input_fact(3, f32::fact([1, 1, 256, 256]).into())
        .map_err(err)?
        .with_input_fact(4, f32::fact([1]).into())
        .map_err(err)?
        // the output size must be a constant for tract
        .with_input_fact(5, InferenceFact::from(tract_ndarray::arr1(&[h as f32, w as f32]).into_tensor()))
        .map_err(err)?
        .into_optimized()
        .map_err(err)?
        .into_runnable()
        .map_err(err)?;
    let res = plan
        .run(tvec!(
            e.embeddings.clone(),
            tract_ndarray::Array3::from_shape_vec((1, n, 2), coords).map_err(err)?.into_tensor().into(),
            tract_ndarray::Array2::from_shape_vec((1, n), labels).map_err(err)?.into_tensor().into(),
            tract_ndarray::Array4::<f32>::zeros((1, 1, 256, 256)).into_tensor().into(),
            tract_ndarray::Array1::from_vec(vec![0.0f32]).into_tensor().into(),
            tract_ndarray::Array1::from_vec(vec![h as f32, w as f32]).into_tensor().into()
        ))
        .map_err(err)?;
    let masks = res[0].to_plain_array_view::<f32>().map_err(err)?;
    let iou = res[1].to_plain_array_view::<f32>().map_err(err)?;
    let k = masks.shape()[1];
    // one click: the best of the multi-mask candidates (1..k; 0 is meant for several points or a box); else the first
    let best = if points.len() == 1 && rect.is_none() && k > 1 {
        (1..k).max_by(|&a, &b| iou[[0, a]].total_cmp(&iou[[0, b]])).unwrap_or(0)
    } else {
        0
    };
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            // logits → soft weight with a narrow transition (about one pixel)
            let l = masks[[0, best, y, x]];
            out.push(1.0 / (1.0 + (-2.0 * l).exp()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_with_and_without_a_box() {
        let (c, l) = prompt(&[[0.5, 0.25, 1.0], [0.1, 0.1, -1.0]], None, 1000, 800);
        assert_eq!(c, vec![500.0, 200.0, 100.0, 80.0, 0.0, 0.0]);
        assert_eq!(l, vec![1.0, 0.0, -1.0]);
        // a box dragged from bottom right to top left: corners sorted, no padding point
        let (c, l) = prompt(&[], Some([0.6, 0.5, 0.2, 0.1]), 1000, 800);
        assert_eq!(c, vec![200.0, 80.0, 600.0, 400.0]);
        assert_eq!(l, vec![2.0, 3.0]);
        let (_, l) = prompt(&[[0.3, 0.3, 1.0]], Some([0.0, 0.0, 1.0, 1.0]), 10, 10);
        assert_eq!(l, vec![1.0, 2.0, 3.0]);
    }

    /// Needs the module files: GRAINEO_TEST_MOBILESAM=<folder> (skipped otherwise).
    #[test]
    fn click_selects_a_disc() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_MOBILESAM").map(std::path::PathBuf::from) else { return };
        let img = RgbImage::from_fn(800, 600, |x, y| {
            let d = ((x as f32 - 400.0).powi(2) + (y as f32 - 300.0).powi(2)).sqrt();
            if d < 120.0 { image::Rgb([235, 220, 200]) } else { image::Rgb([(x / 8) as u8, 40, (y / 6) as u8]) }
        });
        let t = std::time::Instant::now();
        let e = encode(&dir, &img).unwrap();
        let te = t.elapsed();
        let t = std::time::Instant::now();
        let m = decode(&dir, &e, &[[0.5, 0.5, 1.0]]).unwrap();
        eprintln!("MobileSAM encode {te:?}, decode {:?}", t.elapsed());
        let (w, h) = (e.width as usize, e.height as usize);
        let at = |x: f32, y: f32| m[(y * h as f32) as usize * w + (x * w as f32) as usize];
        assert!(at(0.5, 0.5) > 0.9);
        assert!(at(0.05, 0.05) < 0.1);
    }
}

#[cfg(test)]
mod box_tests {
    use super::*;

    /// A box around one of two discs selects that disc (needs the module files: GRAINEO_TEST_MOBILESAM=<folder>).
    #[test]
    fn a_box_selects_the_object_inside() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_MOBILESAM").map(std::path::PathBuf::from) else { return };
        let img = RgbImage::from_fn(800, 600, |x, y| {
            let d1 = ((x as f32 - 250.0).powi(2) + (y as f32 - 300.0).powi(2)).sqrt();
            let d2 = ((x as f32 - 560.0).powi(2) + (y as f32 - 300.0).powi(2)).sqrt();
            if d1 < 110.0 { image::Rgb([235, 210, 60]) } else if d2 < 110.0 { image::Rgb([60, 120, 230]) } else { image::Rgb([40, (x / 10) as u8, 50]) }
        });
        let e = encode(&dir, &img).unwrap();
        let t = std::time::Instant::now();
        let m = decode_prompt(&dir, &e, &[], Some([0.4625, 0.28, 0.8475, 0.72])).unwrap();
        eprintln!("MobileSAM box decode {:?}", t.elapsed());
        let (w, h) = (e.width as usize, e.height as usize);
        let at = |x: f32, y: f32| m[(y * h as f32) as usize * w + (x * w as f32) as usize];
        assert!(at(0.7, 0.5) > 0.9, "the disc in the box");
        assert!(at(0.3125, 0.5) < 0.1, "not the other disc");
        assert!(at(0.05, 0.05) < 0.1);
    }
}
