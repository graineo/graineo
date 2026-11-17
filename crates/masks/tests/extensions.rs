//! Mask extensions of document version 4 beyond name, on/off and strength: edge shift of raster leaves (more follow).
//! Each is written only when used; documents without them stay as they were.
use masks::*;
use std::sync::Arc;

fn picture(w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [0.05 + 0.9 * x * x, 0.1 + 0.6 * y, 0.3 + 0.2 * ((x * 17.0).sin() * (y * 11.0).cos())]
        })
        .collect()
}

/// A raster with a sharp disc (radius 60 of 240 px) in the middle.
fn disc() -> String {
    let data = (0..240 * 160)
        .map(|i| if (((i % 240) as f32 - 119.5).powi(2) + ((i / 240) as f32 - 79.5).powi(2)).sqrt() < 60.0 { 255 } else { 0 })
        .collect();
    raster::remember(Arc::new(RasterData { width: 240, height: 160, data }))
}

fn leaf(id: &str, edge: f32) -> Mask {
    Mask::Raster { raster: id.into(), source: RasterSource::Subject { instances: Instances::All }, feather: 0.0, range: None, edge }
}

#[test]
fn edge_is_version_4_and_written_only_when_used() {
    let id = disc();
    let mut d = Document { version: 2, layers: vec![Layer::new(leaf(&id, 0.0), Adjustments { exposure: 1.0, ..Default::default() })] };
    d.settle_version();
    assert_eq!(d.version, 3);
    assert!(!d.to_json().unwrap().contains("\"edge\""));
    if let Mask::Raster { edge, .. } = &mut d.layers[0].mask {
        *edge = 0.01;
    }
    // without settling: refused
    assert!(d.validate().is_err());
    d.settle_version();
    assert_eq!(d.version, 4);
    let json = d.to_json().unwrap();
    assert!(json.contains("\"edge\": 0.01"));
    assert_eq!(Document::from_json(&json).unwrap(), d);
    // a version-3 reader (the leaf without `edge`) refuses it instead of losing the shift
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct OldRaster {
        #[serde(rename = "type")]
        kind: String,
        raster: String,
        source: serde_json::Value,
        feather: f32,
        #[serde(default)]
        range: Option<[f32; 3]>,
    }
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(serde_json::from_value::<OldRaster>(v["layers"][0]["mask"].clone()).is_err());
    let plain: serde_json::Value = serde_json::from_str(&Document { version: 3, layers: vec![Layer::new(leaf(&id, 0.0), Adjustments::default())] }.to_json().unwrap()).unwrap();
    assert!(serde_json::from_value::<OldRaster>(plain["layers"][0]["mask"].clone()).is_ok());
    // back to 0: back to version 3
    if let Mask::Raster { edge, .. } = &mut d.layers[0].mask {
        *edge = 0.0;
    }
    d.settle_version();
    assert_eq!(d.version, 3);
    // out of range
    for bad in [0.051, -0.06, f32::NAN] {
        assert!(leaf(&id, bad).validate().is_err(), "{bad}");
    }
}

#[test]
fn edge_grows_and_shrinks_the_rendered_weights_without_new_raster() {
    let id = disc();
    let (w, h) = (480, 320);
    let data = picture(w, h);
    let image = Image::new(w, h, &data).unwrap();
    let count = |edge: f32| rasterize(image, &leaf(&id, edge)).unwrap().iter().filter(|v| **v > 0.5).count();
    let (plain, grown, shrunk) = (count(0.0), count(0.02), count(-0.02));
    // 2 % of the short raster side = 3 px of the raster = 6 px here: the disc radius 120 → 126 / 114
    let area = |r: f32| std::f32::consts::PI * r * r;
    assert!((plain as f32 / area(120.0) - 1.0).abs() < 0.02, "{plain}");
    assert!((grown as f32 / area(126.0) - 1.0).abs() < 0.03, "{grown}");
    assert!((shrunk as f32 / area(114.0) - 1.0).abs() < 0.03, "{shrunk}");
    // the same raster name: nothing was computed again
    assert_eq!(raster::ids(&Document { version: 4, layers: vec![Layer::new(leaf(&id, 0.02), Adjustments::default())] }), vec![id.clone()]);
}

#[test]
fn oklab_matches_the_reference_values() {
    // values from the Oklab description (Ottosson 2020) (linear sRGB → Oklab)
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3);
    assert!(close(oklab([1.0, 1.0, 1.0]), [1.0, 0.0, 0.0]));
    assert!(close(oklab([1.0, 0.0, 0.0]), [0.6280, 0.2249, 0.1258]));
    assert!(close(oklab([0.0, 1.0, 0.0]), [0.8664, -0.2339, 0.1795]));
    assert!(close(oklab([0.0, 0.0, 1.0]), [0.4520, -0.0325, -0.3115]));
    // finite for negative and HDR values
    assert!(oklab([-0.1, 2.0, 5.0]).iter().all(|v| v.is_finite()));
    // lightness counts half: the same hue darker is closer than another hue of the same lightness
    let red = oklab([0.5, 0.05, 0.05]);
    assert!(color_distance(red, oklab([0.25, 0.025, 0.025])) < color_distance(red, oklab([0.05, 0.25, 0.05])));
}

fn range(samples: Vec<[f32; 3]>) -> Mask {
    Mask::ColorRange { samples, radius: 0.05, feather: 0.05 }
}

#[test]
fn color_range_takes_the_nearest_of_several_samples() {
    // three flat patches: red, green, blue
    let (w, h) = (30, 10);
    let data: Vec<f32> = (0..w * h).flat_map(|i| match (i % w) / 10 { 0 => [0.6, 0.05, 0.04], 1 => [0.05, 0.5, 0.06], _ => [0.04, 0.06, 0.6] }).collect();
    let image = Image::new(w, h, &data).unwrap();
    let at = |m: &Mask, x: usize| rasterize(image, m).unwrap()[x];
    let red = [0.6, 0.05, 0.04];
    let blue = [0.04, 0.06, 0.6];
    let one = range(vec![red]);
    assert_eq!((at(&one, 5), at(&one, 15), at(&one, 25)), (1.0, 0.0, 0.0));
    let two = range(vec![red, blue]);
    assert_eq!((at(&two, 5), at(&two, 15), at(&two, 25)), (1.0, 0.0, 1.0));
    // a slightly different red still in, softly at the edge
    let near = range(vec![[0.55, 0.07, 0.05]]);
    assert!(at(&near, 5) > 0.5);
    // no sample yet: nothing selected
    assert!(rasterize(image, &range(vec![])).unwrap().iter().all(|v| *v == 0.0));
    // checks
    assert!(range(vec![[0.1; 3]; SAMPLES_MAX + 1]).validate().is_err());
    assert!(Mask::ColorRange { samples: vec![red], radius: -0.1, feather: 0.0 }.validate().is_err());
    assert!(Mask::ColorRange { samples: vec![[f32::NAN, 0.0, 0.0]], radius: 0.1, feather: 0.0 }.validate().is_err());
}

#[test]
fn color_range_is_version_4_and_the_old_colour_mask_stays() {
    let mut d = Document { version: 2, layers: vec![Layer::new(range(vec![[0.2, 0.3, 0.1]]), Adjustments { saturation: 0.3, ..Default::default() })] };
    assert!(d.validate().is_err());
    d.settle_version();
    assert_eq!(d.version, 4);
    let json = d.to_json().unwrap();
    assert!(json.contains("\"color_range\""));
    assert_eq!(Document::from_json(&json).unwrap(), d);
    // also a version-1 document switches (like a raster leaf): it is mask content, not a locked field
    let mut v1 = Document { version: 1, layers: d.layers.clone() };
    v1.settle_version();
    assert_eq!(v1.version, 4);
    // the old colour mask: version 2, unchanged weights (linear RGB distance)
    let old = Mask::Color { reference: [0.2, 0.3, 0.1], radius: 0.1, feather: 0.1 };
    let mut d2 = Document { version: 2, layers: vec![Layer::new(old.clone(), Adjustments { saturation: 0.3, ..Default::default() })] };
    d2.settle_version();
    assert_eq!(d2.version, 2);
    let (w, h) = (16, 8);
    let data = picture(w, h);
    let image = Image::new(w, h, &data).unwrap();
    let weights = rasterize(image, &old).unwrap();
    for (i, v) in weights.iter().enumerate() {
        let p = &data[i * 3..i * 3 + 3];
        let dist = (p[0] - 0.2).hypot(p[1] - 0.3).hypot(p[2] - 0.1);
        let expect = if dist <= 0.1 { 1.0 } else { 1.0 - { let t = ((dist - 0.1) / 0.1).clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) } };
        assert_eq!(*v, expect);
    }
}

fn all_new() -> Adjustments {
    Adjustments {
        exposure: 0.2,
        whites: 0.5,
        blacks: -0.4,
        dehaze: 0.6,
        grade_shadows: [220.0, 0.5],
        grade_midtones: [40.0, 0.2],
        grade_highlights: [45.0, 0.6],
        grade_balance: 0.3,
        curve: vec![[0.0, 0.05], [0.3, 0.25], [0.7, 0.8], [1.0, 0.95]],
        ..Default::default()
    }
}

#[test]
fn new_sliders_are_version_4_and_neutral_ones_change_nothing() {
    let neutral = Adjustments::default();
    assert!(!neutral.uses_version_4());
    let json = serde_json::to_string(&Adjustments { exposure: 0.5, ..Default::default() }).unwrap();
    for k in ["whites", "blacks", "dehaze", "grade_", "curve"] {
        assert!(!json.contains(k), "{k} written without being used");
    }
    let changes: Vec<Adjustments> = vec![
        Adjustments { whites: 0.3, ..Default::default() },
        Adjustments { blacks: 0.3, ..Default::default() },
        Adjustments { dehaze: -0.3, ..Default::default() },
        Adjustments { grade_midtones: [120.0, 0.4], ..Default::default() },
        Adjustments { grade_balance: 0.2, ..Default::default() },
        Adjustments { curve: vec![[0.0, 0.1], [1.0, 0.9]], ..Default::default() },
    ];
    let (w, h) = (24, 16);
    let data = picture(w, h);
    let image = Image::new(w, h, &data).unwrap();
    let full = Mask::Luminance { low: -10.0, high: 10.0, feather: 0.0 };
    for a in changes {
        assert!(a.uses_version_4(), "{a:?}");
        let mut d = Document { version: 2, layers: vec![Layer::new(full.clone(), a.clone())] };
        d.settle_version();
        assert_eq!(d.version, 4);
        assert_eq!(Document::from_json(&d.to_json().unwrap()).unwrap(), d);
        // a version-1 document keeps its filters: the sliders stay locked there (refused, not switched)
        let mut v1 = Document { version: 1, layers: vec![Layer::new(full.clone(), a.clone())] };
        v1.settle_version();
        assert_eq!(v1.version, 1);
        assert!(v1.validate().is_err());
        let out = apply(image, &d).unwrap();
        // balance alone has no effect without a wheel: then no change at all
        if a.grade_balance != 0.0 {
            assert_eq!(out, data);
        } else {
            assert_ne!(out, data, "{a:?}");
        }
    }
    for bad in [Adjustments { whites: 1.5, ..Default::default() }, Adjustments { grade_shadows: [400.0, 0.5], ..Default::default() }, Adjustments { curve: vec![[0.5, 1.2]], ..Default::default() }, Adjustments { curve: vec![[0.5, 0.5]; 17], ..Default::default() }] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}

#[test]
fn new_sliders_act_where_they_should() {
    let full = Mask::Luminance { low: -10.0, high: 10.0, feather: 0.0 };
    let run = |a: Adjustments, px: [f32; 3]| {
        let data = px.to_vec();
        let d = Document { version: 4, layers: vec![Layer::new(full.clone(), a)] };
        let out = apply(Image::new(1, 1, &data).unwrap(), &d).unwrap();
        [out[0], out[1], out[2]]
    };
    let dark = [0.01; 3];
    let bright = [0.9; 3];
    // whites: bright tones much more than dark ones
    let w = Adjustments { whites: 1.0, ..Default::default() };
    assert!(run(w.clone(), bright)[0] > 1.1 * bright[0]);
    assert!((run(w, dark)[0] - dark[0]).abs() < 1e-6);
    // blacks: the dark tones, not the bright ones
    let b = Adjustments { blacks: 1.0, ..Default::default() };
    assert!(run(b.clone(), dark)[0] > dark[0] + 0.01);
    assert_eq!(run(b, bright), bright);
    // dehaze darkens the mid tones (a veil taken away) and saturates
    let px = [0.3, 0.25, 0.2];
    let out = run(Adjustments { dehaze: 1.0, ..Default::default() }, px);
    assert!(out[1] < px[1] && (out[0] - out[2]) > (px[0] - px[2]));
    // grading: warm highlights make a bright grey warmer, a dark grey stays nearly as it was
    let g = Adjustments { grade_highlights: [30.0, 1.0], ..Default::default() };
    let warm = run(g.clone(), [0.7; 3]);
    assert!(warm[0] > warm[2] + 0.05);
    let cool = run(g, [0.005; 3]);
    assert!((cool[0] - cool[2]).abs() < 1e-3);
    // a curve that lifts the middle brightens middle grey
    assert!(run(Adjustments { curve: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]], ..Default::default() }, [0.2; 3])[0] > 0.25);
}

#[test]
#[ignore = "Explicit GPU check"]
fn gpu_matches_cpu_with_the_new_sliders() {
    let (w, h) = (128, 96);
    let data = picture(w, h);
    let image = Image::new(w, h, &data).unwrap();
    let mut a = all_new();
    a.clarity = 0.3;
    a.saturation = 0.1;
    let d = Document { version: 4, layers: vec![Layer::new(Mask::Radial { center: [0.5, 0.5], radii: [0.6, 0.4], angle: 0.2, feather: 0.5 }, a)] };
    let cpu = apply(image, &d).unwrap();
    let gpu = gpu::apply(image, &d).unwrap().expect("GPU must be available for this explicit check");
    let max = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    println!("GPU/CPU with the sliders of version 4: max {max:.7}");
    assert!(max < 2e-4, "{max}");
}

#[test]
fn click_box_is_version_4_and_checked() {
    let leaf = |points: Vec<[f32; 3]>, rect| Mask::Raster { raster: String::new(), source: RasterSource::Click { points, rect }, feather: 0.0, range: None, edge: 0.0 };
    // a box alone is a prompt
    leaf(vec![], Some([0.2, 0.2, 0.6, 0.7])).validate().unwrap();
    assert!(leaf(vec![], None).validate().is_err());
    assert!(leaf(vec![], Some([0.2, 0.2, 0.2, 0.7])).validate().is_err(), "empty box");
    assert!(leaf(vec![], Some([0.2, 0.2, 1.6, 0.7])).validate().is_err(), "outside");
    let mut d = Document { version: 2, layers: vec![Layer::new(leaf(vec![[0.5, 0.5, 1.0]], None), Adjustments::default())] };
    d.settle_version();
    assert_eq!(d.version, 3);
    assert!(!d.to_json().unwrap().contains("rect"));
    d.layers[0].mask = leaf(vec![[0.5, 0.5, 1.0]], Some([0.1, 0.1, 0.9, 0.9]));
    d.settle_version();
    assert_eq!(d.version, 4);
    assert_eq!(Document::from_json(&d.to_json().unwrap()).unwrap(), d);
    // a box does not travel to other photos (it belongs to this photo's subject, like the points)
    assert_eq!(d.for_other_photo(), (None, 1));
}

#[test]
fn person_parts_are_version_4_and_travel_to_other_photos() {
    for part in PersonPart::ALL {
        let leaf = Mask::Raster { raster: String::new(), source: RasterSource::PersonPart { part }, feather: 0.0, range: None, edge: 0.0 };
        let mut d = Document { version: 2, layers: vec![Layer::new(leaf, Adjustments { exposure: 0.3, ..Default::default() })] };
        d.settle_version();
        assert_eq!(d.version, 4);
        let json = d.to_json().unwrap();
        assert!(json.contains("\"person_part\""));
        assert_eq!(Document::from_json(&json).unwrap(), d);
        // the rule works on any photo: kept (pending) when pasted
        let (other, dropped) = d.for_other_photo();
        assert_eq!((other.map(|o| o.layers.len()), dropped), (Some(1), 0));
    }
    assert!(Document::from_json(r#"{"version":4,"layers":[{"mask":{"type":"raster","raster":"","source":{"kind":"person_part","part":"hair"},"feather":0.0},"adjustments":{}}]}"#).is_err(), "hair is not a part");
}

/// A depth leaf with a value range (a short-lived development state) is repaired on reading – range and segment map
/// go, the map is computed again – instead of making the whole document invalid (review 1.3.0, data safety N1).
#[test]
fn depth_leaf_with_a_range_is_repaired_on_reading() {
    let depth = Mask::Raster {
        raster: format!("sha256:{}", "a".repeat(64)),
        source: RasterSource::Depth { model: "depth".into(), classes: vec![DepthClass::Near], toggled: vec![3] },
        feather: 0.002,
        range: None,
        edge: 0.0,
    };
    let other = Mask::Raster { raster: format!("sha256:{}", "b".repeat(64)), source: RasterSource::Subject { instances: Instances::All }, feather: 0.0, range: Some([0.2, 0.8, 0.1]), edge: 0.0 };
    let doc = Document { version: 4, layers: vec![Layer::new(depth, Adjustments { exposure: -0.5, ..Default::default() }), Layer::new(other.clone(), Adjustments::default())] };
    let mut json = serde_json::to_value(&doc).unwrap();
    json["layers"][0]["mask"]["range"] = serde_json::json!([0.0, 0.4, 0.1]);
    let read: Document = serde_json::from_value(json).unwrap();
    read.validate().unwrap();
    match &read.layers[0].mask {
        Mask::Raster { raster, range, source: RasterSource::Depth { classes, .. }, .. } => {
            assert!(raster.is_empty() && range.is_none());
            assert_eq!(classes, &vec![DepthClass::Near]);
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(read.layers[0].adjustments.exposure, -0.5);
    // a range on an 8-bit leaf is a valid part of the document and stays
    assert_eq!(read.layers[1].mask, other);
}
