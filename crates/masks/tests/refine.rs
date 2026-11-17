//! "Kante verfeinern": version 4 only when used, the guided filter for drawn masks, the own edge snap for masks of an
//! image analysis, crops, and the GPU.
use std::sync::Arc;

use masks::*;

/// A picture with a vertical edge at x = 0.5 (dark left, bright right), 400 × 240 (band radius 5 px at full strength).
fn picture() -> (Vec<f32>, usize, usize) {
    let (w, h) = (400, 240);
    (((0..w * h).flat_map(|i| if i % w < 200 { [0.03, 0.03, 0.04] } else { [0.5, 0.55, 0.6] })).collect(), w, h)
}

fn layer(mask: Mask, refine: f32) -> Layer {
    Layer { refine, ..Layer::new(mask, Adjustments { exposure: 1.0, ..Default::default() }) }
}

#[test]
fn version_4_only_when_used() {
    let mut d = Document { version: 2, layers: vec![layer(Mask::Linear { start: [0.4, 0.5], end: [0.6, 0.5] }, 0.0)] };
    d.settle_version();
    assert_eq!(d.version, 2);
    assert!(!d.to_json().unwrap().contains("refine"));
    d.layers[0].refine = 0.5;
    assert!(d.validate().is_err(), "needs version 4");
    d.settle_version();
    assert_eq!(d.version, 4);
    let json = d.to_json().unwrap();
    assert!(json.contains("\"refine\": 0.5"), "{json}");
    assert_eq!(Document::from_json(&json).unwrap(), d);
    d.layers[0].refine = 1.5;
    assert!(d.validate().is_err());
    // version 1 keeps its filters: the field stays locked there
    let v1 = Document { version: 1, layers: vec![layer(Mask::Linear { start: [0.4, 0.5], end: [0.6, 0.5] }, 0.0)] };
    assert!(!v1.allows_layer_fields());
}

#[test]
fn a_gradient_follows_the_image_edge() {
    let (px, w, h) = (picture().0, picture().1, picture().2);
    let im = Image::new(w, h, &px).unwrap();
    let view = View::whole(&im);
    // a soft gradient across the image edge: plain and refined
    let g = Mask::Linear { start: [0.48, 0.5], end: [0.52, 0.5] };
    let plain = layer(g.clone(), 0.0).weights(im, view).unwrap();
    let fine = layer(g, 1.0).weights(im, view).unwrap();
    let at = |v: &[f32], x: usize| v[120 * w + x];
    // on the dark side less, on the bright side more: the transition moves onto the image edge
    assert!(at(&fine, 197) < at(&plain, 197) - 0.05, "{} {}", at(&fine, 197), at(&plain, 197));
    assert!(at(&fine, 202) > at(&plain, 202) + 0.05, "{} {}", at(&fine, 202), at(&plain, 202));
    assert_eq!((at(&fine, 10), at(&fine, 390)), (0.0, 1.0));
}

#[test]
fn a_raster_keeps_its_edge() {
    let (px, w, h) = picture();
    let im = Image::new(w, h, &px).unwrap();
    // a coarse selection of the bright half whose edge sits 4 px too far left (as an upsampled model mask)
    let data = (0..200 * 120).map(|i| if i % 200 < 98 { 0 } else { 255 }).collect();
    let id = raster::remember(Arc::new(RasterData { width: 200, height: 120, data }));
    let leaf = Mask::Raster { raster: id, source: RasterSource::Subject { instances: Instances::All }, feather: 0.0, range: None, edge: 0.0 };
    let plain = layer(leaf.clone(), 0.0).weights(im, View::whole(&im)).unwrap();
    let fine = layer(leaf, 1.0).weights(im, View::whole(&im)).unwrap();
    // masks of an image analysis: no guided filter, and the edge snap is not used (no measured gain)
    assert!(plain == fine);
    let l = layer(Mask::Raster { raster: String::new(), source: RasterSource::Painted, feather: 0.0, range: None, edge: 0.0 }, 1.0);
    assert!(!l.refines());
    assert!(layer(Mask::Linear { start: [0.4, 0.5], end: [0.6, 0.5] }, 1.0).refines());
}

#[test]
fn neutral_refine_is_bitwise_the_old_path_and_crops_match() {
    let (px, w, h) = picture();
    let im = Image::new(w, h, &px).unwrap();
    let mask = Mask::Radial { center: [0.5, 0.5], radii: [0.3, 0.3], angle: 0.0, feather: 0.6 };
    let old = Document { version: 2, layers: vec![layer(mask.clone(), 0.0)] };
    let a = apply(im, &old).unwrap();
    let mut d = old.clone();
    d.layers[0].refine = 1.0;
    d.settle_version();
    let b = apply(im, &d).unwrap();
    assert_ne!(a, b);
    // a crop with the margin gives the same inside
    let m = margin(&d, [w, h]);
    assert!(m >= 8, "{m}");
    let (x, y, cw, ch) = (170usize, 90usize, 60usize, 60usize);
    let (x0, y0) = (x - m.min(x), y - m.min(y));
    let (ew, eh) = ((x + cw + m).min(w) - x0, (y + ch + m).min(h) - y0);
    let crop: Vec<f32> = (y0..y0 + eh).flat_map(|yy| px[(yy * w + x0) * 3..(yy * w + x0 + ew) * 3].to_vec()).collect();
    let part = apply_view(Image::new(ew, eh, &crop).unwrap(), &d, View { full: [w, h], offset: [x0, y0] }).unwrap();
    for yy in 0..ch {
        for xx in 0..cw {
            for c in 0..3 {
                let p = b[((y + yy) * w + x + xx) * 3 + c];
                let q = part[((y - y0 + yy) * ew + x - x0 + xx) * 3 + c];
                assert!((p - q).abs() < 1e-5, "{p} {q} at {xx},{yy}");
            }
        }
    }
}

#[test]
#[ignore = "needs a GPU"]
fn gpu_matches_cpu_with_a_refined_edge() {
    let (px, w, h) = picture();
    let im = Image::new(w, h, &px).unwrap();
    for brightness in [false, true] {
        let mask = if brightness { Mask::Luminance { low: 0.2, high: 2.0, feather: 0.1 } } else { Mask::Linear { start: [0.48, 0.5], end: [0.52, 0.5] } };
        let mut d = Document { version: 2, layers: vec![layer(mask, 0.8)] };
        d.settle_version();
        let cpu = apply(im, &d).unwrap();
        let gpu = gpu::apply(im, &d).unwrap().expect("GPU");
        let max = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        println!("refine GPU vs CPU ({brightness}): {max:e}");
        assert!(max < 1e-5, "{max}");
    }
}
