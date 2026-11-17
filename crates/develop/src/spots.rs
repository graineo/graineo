//! Spot retouching ("spot removal": heal and clone) and red-eye correction on the fully developed output.
//! Coordinates refer to the unrotated, uncropped image (0…1 in width and height), radii to the image width, as
//! Lightroom stores them. Computed with the `retouch` core (Poisson healing) in linear light.
use crate::{Rgb16, Settings, View};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// A spot: around `center` with `radius` (fraction of the image width), healed or copied from `source`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    /// Clone instead of heal
    pub clone: bool,
    pub center: [f32; 2],
    pub source: [f32; 2],
    pub radius: f32,
    pub opacity: f32,
}

/// A red pupil: ellipse around `center` with semi-axes `size` (fraction of the image width) and rotation `angle` (radians).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedEye {
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub angle: f32,
    /// Correction strength 0…1
    pub density: f32,
    /// Additional darkening of the pupil 0…1
    pub darken: f32,
}

/// A removed object: the area around `center` (radius as a fraction of the image width) is replaced by a patch an
/// inpainting model computed (LaMa, in the app); the patch covers `region` (x, y, w, h, 0…1 of the image) and is stored
/// in the library under its content name (`sha256:…`). Rendering blends it in with Poisson healing, so that its
/// brightness and colour follow the surroundings of this stage. The fill is invented content, not a photograph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Removal {
    pub center: [f32; 2],
    pub radius: f32,
    pub region: [f32; 4],
    /// content name of the stored patch (8-bit sRGB); empty while it is being computed
    pub patch: String,
}

/// A stored patch: width, height, 8-bit sRGB (row-major RGB).
pub struct Patch {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

type PatchProvider = Box<dyn Fn(&str) -> Option<std::sync::Arc<Patch>> + Send + Sync>;
static PATCHES: std::sync::RwLock<Option<PatchProvider>> = std::sync::RwLock::new(None);

/// Set the resolver for removal patches (the program: from the library).
pub fn set_patch_provider(f: impl Fn(&str) -> Option<std::sync::Arc<Patch>> + Send + Sync + 'static) {
    *PATCHES.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

fn patch(id: &str) -> Option<std::sync::Arc<Patch>> {
    if id.is_empty() {
        return None;
    }
    PATCHES.read().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|p| p(id)).filter(|p| p.width > 0 && p.height > 0 && p.rgb.len() == p.width as usize * p.height as usize * 3)
}

pub fn active(s: &Settings) -> bool {
    s.view == View::Developed && (!s.spots.is_empty() || !s.red_eyes.is_empty() || s.removals.iter().any(|r| !r.patch.is_empty()))
}

/// Blend one removal into `lin` (linear RGB, `w × h`): the patch, resampled to its region, is the source of a Poisson
/// heal of the circle. `false`: patch missing or region outside.
fn apply_removal(lin: &mut Vec<f32>, w: usize, h: usize, r: &Removal) -> bool {
    let Some(p) = patch(&r.patch) else { return false };
    let x0 = (r.region[0] * w as f32).round().max(0.0) as usize;
    let y0 = (r.region[1] * h as f32).round().max(0.0) as usize;
    let rw = ((r.region[2] * w as f32).round() as usize).min(w.saturating_sub(x0));
    let rh = ((r.region[3] * h as f32).round() as usize).min(h.saturating_sub(y0));
    if rw < 8 || rh < 8 {
        return false;
    }
    // working image: the region on the left, the resampled patch on the right
    let ww = 2 * rw;
    let mut work = vec![0.0f32; ww * rh * 3];
    let (pw, ph) = (p.width as usize, p.height as usize);
    for y in 0..rh {
        for x in 0..rw {
            let i = ((y0 + y) * w + x0 + x) * 3;
            let o = (y * ww + x) * 3;
            work[o..o + 3].copy_from_slice(&lin[i..i + 3]);
            let fx = ((x as f32 + 0.5) * pw as f32 / rw as f32 - 0.5).clamp(0.0, (pw - 1) as f32);
            let fy = ((y as f32 + 0.5) * ph as f32 / rh as f32 - 0.5).clamp(0.0, (ph - 1) as f32);
            let (ax, ay) = (fx as usize, fy as usize);
            let (bx, by) = ((ax + 1).min(pw - 1), (ay + 1).min(ph - 1));
            let (tx, ty) = (fx - ax as f32, fy - ay as f32);
            for c in 0..3 {
                let at = |x: usize, y: usize| to_linear((p.rgb[(y * pw + x) * 3 + c] as u16) * 257);
                let top = at(ax, ay) + (at(bx, ay) - at(ax, ay)) * tx;
                let bottom = at(ax, by) + (at(bx, by) - at(ax, by)) * tx;
                work[(y * ww + rw + x) * 3 + c] = top + (bottom - top) * ty;
            }
        }
    }
    let (cx, cy) = (r.center[0] * w as f32 - x0 as f32, r.center[1] * h as f32 - y0 as f32);
    let radius = (r.radius * w as f32).max(2.0);
    let Ok(mask) = retouch::Mask::circle([cx, cy], radius, 0.6) else { return false };
    let Ok(image) = retouch::Image::new(ww, rh, &work) else { return false };
    let options = retouch::Options { method: retouch::Method::Heal, ..Default::default() };
    let out = match retouch::compute(image, &mask, retouch::Source { dx: rw as i32, dy: 0 }, options).and_then(|q| q.apply(image)) {
        Ok(o) => o,
        // too close to the border of the image: left out like a spot
        Err(_) => return false,
    };
    for y in 0..rh {
        for x in 0..rw {
            let i = ((y0 + y) * w + x0 + x) * 3;
            let o = (y * ww + x) * 3;
            lin[i..i + 3].copy_from_slice(&out[o..o + 3]);
        }
    }
    true
}

fn to_linear(v: u16) -> f32 {
    let v = v as f32 / 65535.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb16(v: f32) -> u16 {
    let v = v.max(0.0);
    let s = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s.clamp(0.0, 1.0) * 65535.0).round() as u16
}

pub fn apply(img: &mut Rgb16, s: &Settings) {
    if !active(s) {
        return;
    }
    let (w, h) = (img.width, img.height);
    if w < 8 || h < 8 {
        return;
    }
    let mut lin: Vec<f32> = img.data.par_iter().map(|&v| to_linear(v)).collect();
    if apply_linear(&mut lin, w, h, s) {
        img.data.par_iter_mut().zip(lin.par_iter()).for_each(|(o, &v)| *o = to_srgb16(v));
    }
}

/// The same for encoded sRGB floats (wide-gamut display and export path; values may exceed 0…1).
pub fn apply_encoded(data: &mut [f32], w: usize, h: usize, s: &Settings) {
    if !active(s) || w < 8 || h < 8 || data.len() != w * h * 3 {
        return;
    }
    let dec = |v: f32| if v.abs() <= 0.04045 { v / 12.92 } else { v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4) };
    let enc = |v: f32| if v.abs() <= 0.0031308 { v * 12.92 } else { v.signum() * (1.055 * v.abs().powf(1.0 / 2.4) - 0.055) };
    let mut lin: Vec<f32> = data.par_iter().map(|&v| dec(v)).collect();
    if apply_linear(&mut lin, w, h, s) {
        data.par_iter_mut().zip(lin.par_iter()).for_each(|(o, &v)| *o = enc(v));
    }
}

/// Spots, removals and red eyes on linear RGB; `true` if anything changed.
fn apply_linear(lin_ref: &mut Vec<f32>, w: usize, h: usize, s: &Settings) -> bool {
    let mut lin = std::mem::take(lin_ref);
    let mut changed = false;
    for sp in &s.spots {
        let (cx, cy) = (sp.center[0] * w as f32, sp.center[1] * h as f32);
        let r = (sp.radius * w as f32).max(2.0);
        let (dx, dy) = ((sp.source[0] * w as f32 - cx).round() as i32, (sp.source[1] * h as f32 - cy).round() as i32);
        let Ok(mask) = retouch::Mask::circle([cx, cy], r, 0.6) else { continue };
        let Ok(image) = retouch::Image::new(w, h, &lin) else { continue };
        let options = retouch::Options { method: if sp.clone { retouch::Method::Clone } else { retouch::Method::Heal }, ..Default::default() };
        // Opacity < 1: scale the mask by the value
        let mask = if sp.opacity < 0.999 {
            let [x, y, mean, mh] = mask.bounds();
            let weights: Vec<f32> = mask.weights().iter().map(|v| v * sp.opacity.clamp(0.0, 1.0)).collect();
            match retouch::Mask::new(x, y, mean, mh, weights) {
                Ok(m) => m,
                Err(_) => continue,
            }
        } else {
            mask
        };
        match retouch::compute(image, &mask, retouch::Source { dx, dy }, options).and_then(|p| p.apply(image)) {
            Ok(out) => {
                lin = out;
                changed = true;
            }
            // Source or spot too close to the image border or outside: skip this spot
            Err(_) => {}
        }
    }
    for r in &s.removals {
        changed |= apply_removal(&mut lin, w, h, r);
    }
    for e in &s.red_eyes {
        let (cx, cy) = (e.center[0] * w as f32, e.center[1] * h as f32);
        let (a, b) = ((e.size[0] * w as f32).max(1.5), (e.size[1] * w as f32).max(1.5));
        let (sn, cs) = e.angle.sin_cos();
        let reach = (a.max(b) * 1.6).ceil() as isize;
        let (x0, x1) = ((cx as isize - reach).max(0), (cx as isize + reach).min(w as isize - 1));
        let (y0, y1) = ((cy as isize - reach).max(0), (cy as isize + reach).min(h as isize - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 - cx, y as f32 - cy);
                let (u, v) = ((px * cs + py * sn) / a, (-px * sn + py * cs) / b);
                let d = (u * u + v * v).sqrt();
                if d >= 1.3 {
                    continue;
                }
                // full inside, fading out softly outside
                let wgt = if d <= 1.0 { 1.0 } else { 1.0 - (d - 1.0) / 0.3 };
                let i = ((y as usize) * w + x as usize) * 3;
                let (r, g, bl) = (lin[i], lin[i + 1], lin[i + 2]);
                let target_size = ((g + bl) * 0.5) * (1.0 - 0.5 * e.darken.clamp(0.0, 1.0));
                if r > target_size {
                    lin[i] = r + (target_size - r) * wgt * e.density.clamp(0.0, 1.0);
                    changed = true;
                }
            }
        }
    }
    *lin_ref = lin;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image(w: usize, h: usize) -> Rgb16 {
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let v = (20000 + x * 60 + y * 30) as u16;
                data.extend_from_slice(&[v, v, v]);
            }
        }
        Rgb16 { width: w, height: h, data }
    }

    #[test]
    fn removal_blends_the_stored_patch() {
        let mut img = test_image(160, 100);
        for y in 40..60 {
            for x in 70..90 {
                img.data[(y * 160 + x) * 3..(y * 160 + x) * 3 + 3].copy_from_slice(&[2000, 2000, 2000]);
            }
        }
        // the "model" filled the region with a flat grey; Poisson healing adapts it to the gradient around
        set_patch_provider(|id| (id == "sha256:test-patch").then(|| std::sync::Arc::new(Patch { width: 8, height: 8, rgb: vec![150; 8 * 8 * 3] })));
        let s = Settings { removals: vec![Removal { center: [80.0 / 160.0, 50.0 / 100.0], radius: 16.0 / 160.0, region: [40.0 / 160.0, 10.0 / 100.0, 80.0 / 160.0, 80.0 / 100.0], patch: "sha256:test-patch".into() }], ..Settings::default() };
        assert!(active(&s));
        apply(&mut img, &s);
        let v = img.data[(50 * 160 + 80) * 3] as i32;
        let expect = (20000 + 80 * 60 + 50 * 30) as i32;
        assert!((v - expect).abs() < 3000, "{v} vs {expect}");
        // a missing patch changes nothing
        let mut other = test_image(160, 100);
        let before = other.data.clone();
        let s = Settings { removals: vec![Removal { patch: "sha256:missing".into(), ..s.removals[0].clone() }], ..Settings::default() };
        apply(&mut other, &s);
        assert_eq!(other.data, before);
    }

    #[test]
    fn single_spot_is_healed() {
        let mut img = test_image(120, 80);
        // dark spot at (40, 40), radius 5
        for y in 35..46 {
            for x in 35..46 {
                if ((x as f32 - 40.0).powi(2) + (y as f32 - 40.0).powi(2)).sqrt() < 5.0 {
                    let i = (y * 120 + x) * 3;
                    img.data[i..i + 3].fill(8000);
                }
            }
        }
        let s = Settings { spots: vec![Spot { clone: false, center: [40.0 / 120.0, 40.0 / 80.0], source: [60.0 / 120.0, 40.0 / 80.0], radius: 6.0 / 120.0, opacity: 1.0 }], ..Default::default() };
        assert!(active(&s));
        apply(&mut img, &s);
        let i = (40 * 120 + 40) * 3;
        let surround = (20000 + 40 * 60 + 40 * 30) as i32;
        assert!((img.data[i] as i32 - surround).abs() < 2500, "centre {} vs. surroundings {surround}", img.data[i]);
    }

    #[test]
    fn no_spots_unchanged_and_invalid_skipped() {
        let original_img = test_image(60, 40);
        let mut img = original_img.clone_for_test();
        apply(&mut img, &Settings::default());
        assert_eq!(img.data, original_img.data);
        // Spot at the border and source outside: no crash, image unchanged
        let s = Settings { spots: vec![Spot { clone: false, center: [0.0, 0.0], source: [2.0, 2.0], radius: 0.05, opacity: 1.0 }], ..Default::default() };
        apply(&mut img, &s);
        assert_eq!(img.data, original_img.data);
    }

    #[test]
    fn red_pupil_gets_darker_and_neutral() {
        let mut img = Rgb16 { width: 40, height: 40, data: vec![30000; 40 * 40 * 3] };
        for y in 17..24 {
            for x in 17..24 {
                let i = (y * 40 + x) * 3;
                img.data[i] = 60000;
            }
        }
        let s = Settings { red_eyes: vec![RedEye { center: [20.5 / 40.0, 20.5 / 40.0], size: [5.0 / 40.0, 5.0 / 40.0], angle: 0.0, density: 1.0, darken: 0.5 }], ..Default::default() };
        apply(&mut img, &s);
        let i = (20 * 40 + 20) * 3;
        assert!(img.data[i] < 40000, "red component {} stays too high", img.data[i]);
    }

    impl Rgb16 {
        fn clone_for_test(&self) -> Rgb16 {
            Rgb16 { width: self.width, height: self.height, data: self.data.clone() }
        }
    }
}
