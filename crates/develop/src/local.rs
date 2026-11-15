//! Shared mask stage before display looks/geometry. The RGB16 transfer curve is decoded first.
use crate::{Rgb16, Settings, View};
use rayon::prelude::*;
pub fn active(s: &Settings) -> bool {
    masks_active(s) || crate::spots::active(s)
}
fn masks_active(s: &Settings) -> bool {
    s.view == View::Developed
        && s.local.as_ref().is_some_and(|d| {
            d.layers
                .iter()
                .any(|l| l.adjustments != masks::Adjustments::default())
        })
}
/// Can a crop with local adjustments be computed on its own (with [`margin`] around it) instead of
/// from the whole image? Not with retouching: its sources can lie anywhere in the image.
pub fn region_possible(s: &Settings) -> bool {
    !crate::spots::active(s)
}

/// Margin in pixels around a crop of an image of `full` pixels so that the local adjustments inside
/// the crop come out exactly as in the whole image.
pub fn margin(s: &Settings, full: [usize; 2]) -> usize {
    s.local.as_ref().map_or(0, |d| masks::margin(d, full))
}

/// Crop `region` expanded by `margin` on every side, limited to the image.
pub fn expand(region: [usize; 4], margin: usize, full: [usize; 2]) -> [usize; 4] {
    let [x, y, w, h] = region;
    let x0 = x.min(full[0] - 1).saturating_sub(margin);
    let y0 = y.min(full[1] - 1).saturating_sub(margin);
    let x1 = (x + w + margin).min(full[0]);
    let y1 = (y + h + margin).min(full[1]);
    [x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)]
}

pub fn apply(img: &mut Rgb16, s: &Settings) {
    apply_view(img, s, None)
}

/// [`apply`] for a crop of the whole image (`view`, see [`masks::View`]); `None` = the whole image.
pub fn apply_view(img: &mut Rgb16, s: &Settings, view: Option<masks::View>) {
    if !active(s) {
        return;
    }
    // retouching first (it changes image content), then the local adjustments
    crate::spots::apply(img, s);
    if !masks_active(s) {
        return;
    }
    let Some(doc) = s.local.as_ref() else {
        return;
    };
    let linear: Vec<f32> = img
        .data
        .par_iter()
        .map(|&v| {
            let v = v as f32 / 65535.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    let result = masks::Image::new(img.width, img.height, &linear)
        .and_then(|i| match view {
            Some(v) => masks::gpu::apply_auto_view(i, doc, v),
            None => masks::gpu::apply_auto(i, doc),
        });
    match result {
        Ok(data) => img
            .data
            .par_iter_mut()
            .zip(data.par_iter())
            .for_each(|(out, &v)| {
                let v = v.max(0.0);
                let srgb = if v <= 0.0031308 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                };
                *out = (srgb.clamp(0.0, 1.0) * 65535.0).round() as u16;
            }),
        Err(e) => eprintln!("local adjustments: {e}"),
    }
}
pub fn crop(img: &Rgb16, region: [usize; 4]) -> Rgb16 {
    let [x, y, w, h] = region;
    let x = x.min(img.width - 1);
    let y = y.min(img.height - 1);
    let w = w.max(1).min(img.width - x);
    let h = h.max(1).min(img.height - y);
    let mut data = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        data.extend_from_slice(&img.data[(row * img.width + x) * 3..(row * img.width + x + w) * 3]);
    }
    Rgb16 {
        width: w,
        height: h,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_and_output_roundtrip() {
        let mut doc =
            masks::Document::from_json(include_str!("../../masks/samples/masks-example.json")).unwrap();
        doc.version = 2;
        let s = Settings {
            local: Some(doc),
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
        let data = vec![20000u16; 60 * 40 * 3];
        let mut image = Rgb16 {
            width: 60,
            height: 40,
            data: data.clone(),
        };
        apply(&mut image, &s);
        assert_ne!(image.data, data);
        let cropped = crop(&image, [5, 6, 10, 12]);
        assert_eq!(
            cropped.data[..30],
            image.data[(6 * 60 + 5) * 3..(6 * 60 + 15) * 3]
        );
        let mut neutral = Rgb16 {
            width: 60,
            height: 40,
            data: data.clone(),
        };
        apply(&mut neutral, &Settings::default());
        assert_eq!(neutral.data, data);
    }
}
