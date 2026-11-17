//! Shared mask stage before display looks/geometry. The RGB16 transfer curve is decoded first.
use crate::{Rgb16, Settings, View};
use rayon::prelude::*;
pub fn active(s: &Settings) -> bool {
    masks_active(s) || crate::spots::active(s)
}
fn masks_active(s: &Settings) -> bool {
    s.view == View::Developed
        // a layer that is switched off or at strength 0 changes nothing (masks::Layer::active)
        && s.local.as_ref().is_some_and(|d| d.layers.iter().any(masks::Layer::active))
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
        keep_capture(img, view);
        return;
    }
    // retouching first (it changes image content), then the local adjustments
    crate::spots::apply(img, s);
    keep_capture(img, view);
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
/// The image the local adjustments see (after retouching, before the first mask), downscaled: linear RGB.
#[derive(Clone, Debug, PartialEq)]
pub struct Captured {
    pub width: usize,
    pub height: usize,
    /// linear sRGB/Rec.709, row-major RGB
    pub data: Vec<f32>,
}

std::thread_local! {
    /// Armed by [`capture`] on this thread: long side of the copy, and the copy once taken.
    static CAPTURE: std::cell::RefCell<Option<(usize, Option<Captured>)>> = const { std::cell::RefCell::new(None) };
}

/// Runs `render` (a whole-image development on this thread) and keeps a copy of the image the local adjustments see,
/// with a long side of at most `side` pixels (box average). For the colour samples and the overlay of colour and
/// brightness ranges: they compare against exactly this image. Reads only; the rendering is unchanged.
pub fn capture<R>(side: usize, render: impl FnOnce() -> R) -> (R, Option<Captured>) {
    CAPTURE.with(|c| *c.borrow_mut() = Some((side.max(1), None)));
    let r = render();
    let got = CAPTURE.with(|c| c.borrow_mut().take()).and_then(|c| c.1);
    (r, got)
}

fn keep_capture(img: &Rgb16, view: Option<masks::View>) {
    // only the whole image (a crop of the zoomed view is not the mask space)
    if view.is_some() || img.width == 0 || img.height == 0 {
        return;
    }
    CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        let Some((side, slot)) = c.as_mut() else { return };
        if slot.is_some() {
            return;
        }
        let f = (img.width.max(img.height) as f32 / *side as f32).ceil().max(1.0) as usize;
        let (w, h) = (img.width.div_ceil(f), img.height.div_ceil(f));
        let lin = |v: u16| {
            let v = v as f32 / 65535.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let data: Vec<f32> = (0..w * h)
            .into_par_iter()
            .flat_map_iter(|i| {
                let (x, y) = (i % w, i / w);
                let mut acc = [0.0f32; 3];
                let mut n = 0.0;
                for yy in y * f..((y + 1) * f).min(img.height) {
                    for xx in x * f..((x + 1) * f).min(img.width) {
                        let p = &img.data[(yy * img.width + xx) * 3..(yy * img.width + xx) * 3 + 3];
                        for k in 0..3 {
                            acc[k] += lin(p[k]);
                        }
                        n += 1.0;
                    }
                }
                acc.map(|a| a / n)
            })
            .collect();
        *slot = Some(Captured { width: w, height: h, data });
    });
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

    #[test]
    fn capture_keeps_what_the_masks_see_and_changes_nothing() {
        let data: Vec<u16> = (0..40 * 20 * 3).map(|i| (i * 37 % 65536) as u16).collect();
        let mut img = Rgb16 { width: 40, height: 20, data: data.clone() };
        let ((), got) = capture(10, || apply(&mut img, &Settings::default()));
        assert_eq!(img.data, data);
        let got = got.unwrap();
        assert_eq!((got.width, got.height), (10, 5));
        // nothing armed: nothing kept, and a crop is never kept
        let ((), none) = capture(10, || apply_view(&mut img, &Settings::default(), Some(masks::View { full: [80, 40], offset: [0, 0] })));
        assert!(none.is_none());
        apply(&mut img, &Settings::default());
        assert!(CAPTURE.with(|c| c.borrow().is_none()));
    }
}
