//! Version 4 of the mask document (name, on/off and strength per layer): written only when used, rendering without
//! the fields unchanged (fixed checksums from before the fields existed), older readers keep reading what is written
//! without them.
use masks::*;

/// FNV-1a over the bits of the values (exact: every changed bit changes the sum).
fn fnv(data: &[f32]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, v| {
        v.to_bits().to_le_bytes().iter().fold(h, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
    })
}

fn fnv_text(t: &str) -> u64 {
    t.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn picture(w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [0.05 + 0.9 * x * x, 0.1 + 0.6 * y, 0.3 + 0.2 * ((x * 17.0).sin() * (y * 11.0).cos())]
        })
        .collect()
}

fn samples() -> [(&'static str, Document); 3] {
    [
        ("v1", Document::from_json(include_str!("../samples/masks-example.json")).unwrap()),
        ("v2", Document::from_json(include_str!("../samples/masks-v2-example.json")).unwrap()),
        // filters of version 2, brush, combination and inversion
        ("v2-detail", Document::from_json(r#"{"version":2,"layers":[{"mask":{"type":"radial","center":[0.5,0.5],"radii":[0.3,0.25],"angle":0.4,"feather":0.5},"adjustments":{"exposure":0.5,"clarity":0.4}},{"mask":{"type":"combine","operation":"subtract","left":{"type":"brush","points":[[0.3,0.3],[0.5,0.45],[0.7,0.5]],"radius":0.05,"hardness":0.5,"flow":0.6},"right":{"type":"invert","mask":{"type":"linear","start":[0.2,0.2],"end":[0.8,0.6]}}},"adjustments":{"texture":0.5,"noise_reduction":0.3,"saturation":-0.4,"highlights":0.3}}]}"#).unwrap()),
    ]
}

/// Checksums of the CPU rendering, of each layer's weights and of the written JSON for the two sample documents,
/// taken with the program before version 4 (17.11.2026). Documents that use none of the new fields must come out
/// bit for bit the same.
#[test]
fn documents_without_the_new_fields_render_and_write_as_before() {
    const EXPECTED: [(&str, u64, u64, u64); 3] = [
        ("v1", 0xc24713f4101133b9, 0x2c6022a8d4efebc7, 0x02cf183a83720080),
        ("v2", 0xc24713f4101133b9, 0x2c6022a8d4efebc7, 0xb065e350a645f421),
        ("v2-detail", 0x66d8fdc7ac1d9c46, 0x815b923538f6798c, 0x8827d346393fd8f8),
    ];
    let (w, h) = (96, 64);
    let data = picture(w, h);
    let image = Image::new(w, h, &data).unwrap();
    for ((name, doc), (e_name, e_render, e_weights, e_json)) in samples().into_iter().zip(EXPECTED) {
        assert_eq!(name, e_name);
        let render = fnv(&apply(image, &doc).unwrap());
        let weights: Vec<f32> = doc.layers.iter().flat_map(|l| rasterize(image, &l.mask).unwrap()).collect();
        let weights = fnv(&weights);
        let json = fnv_text(&doc.to_json().unwrap());
        println!("{name}: render {render:#018x}, weights {weights:#018x}, json {json:#018x}");
        assert_eq!((render, weights, json), (e_render, e_weights, e_json), "{name}");
    }
}

/// What an older program reads: a layer with exactly `mask` and `adjustments`, unknown fields refused.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct OldLayer {
    mask: serde_json::Value,
    adjustments: serde_json::Value,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct OldDocument {
    version: u32,
    layers: Vec<OldLayer>,
}

#[test]
fn version_4_only_when_a_new_field_is_used() {
    let (_, mut doc) = samples().into_iter().nth(1).unwrap();
    doc.settle_version();
    assert_eq!(doc.version, 2);
    let json = doc.to_json().unwrap();
    assert!(!json.contains("\"name\"") && !json.contains("\"enabled\"") && !json.contains("\"amount\""));
    // an older program reads what is written without the new fields
    assert!(serde_json::from_str::<OldDocument>(&json).is_ok());
    for change in 0..3 {
        let mut d = doc.clone();
        match change {
            0 => d.layers[0].name = Some("Himmel".into()),
            1 => d.layers[0].enabled = false,
            _ => d.layers[0].amount = 0.5,
        }
        // without settling: refused (an older version number with fields of version 4)
        assert!(d.validate().is_err());
        d.settle_version();
        assert_eq!(d.version, 4);
        let json = d.to_json().unwrap();
        assert_eq!(Document::from_json(&json).unwrap(), d);
        // an older program refuses it instead of losing the field
        assert!(serde_json::from_str::<OldDocument>(&json).is_err());
        // back to the defaults: back to version 2
        d.layers[0] = Layer { name: None, enabled: true, amount: 1.0, ..d.layers[0].clone() };
        d.settle_version();
        assert_eq!(d.version, 2);
    }
    // version 1 never becomes 4 by itself: its filters would change
    let (_, mut v1) = samples().into_iter().next().unwrap();
    assert!(!v1.allows_layer_fields());
    v1.layers[0].amount = 0.5;
    v1.settle_version();
    assert_eq!(v1.version, 1);
    assert!(v1.validate().is_err());
}

#[test]
fn version_4_keeps_raster_leaves() {
    let leaf = Mask::Raster { raster: String::new(), source: RasterSource::Painted, feather: 0.0, range: None, edge: 0.0 };
    let mut d = Document { version: 2, layers: vec![Layer::new(leaf, Adjustments { exposure: 1.0, ..Default::default() })] };
    d.settle_version();
    assert_eq!(d.version, 3);
    d.layers[0].name = Some("Bitmap".into());
    d.settle_version();
    assert_eq!(d.version, 4);
    d.validate().unwrap();
    d.layers[0].name = None;
    d.settle_version();
    assert_eq!(d.version, 3);
}

#[test]
fn values_out_of_range_are_refused() {
    let base = r#"{"version":4,"layers":[{"mask":{"type":"luminance","low":0.0,"high":1.0,"feather":0.0},"adjustments":{"exposure":1.0}"#;
    assert!(Document::from_json(&format!("{base},\"amount\":0.25}}]}}")).is_ok());
    assert!(Document::from_json(&format!("{base},\"amount\":1.5}}]}}")).is_err());
    assert!(Document::from_json(&format!("{base},\"amount\":-0.1}}]}}")).is_err());
    assert!(Document::from_json(&format!("{base},\"name\":\"{}\"}}]}}", "x".repeat(64))).is_ok());
    assert!(Document::from_json(&format!("{base},\"name\":\"{}\"}}]}}", "x".repeat(65))).is_err());
    assert!(Document::from_json(&format!("{base},\"name\":\"a\\nb\"}}]}}")).is_err());
    assert!(Document::from_json(&format!("{base},\"colour\":1}}]}}")).is_err());
    // version 5 is unknown
    let plain = format!("{base}}}]}}");
    assert!(Document::from_json(&plain).is_ok());
    assert!(Document::from_json(&plain.replace("\"version\":4", "\"version\":5")).is_err());
}

#[test]
fn switched_off_and_strength_act_on_the_rendering() {
    let (w, h) = (48, 32);
    let data = picture(w, h);
    let image = Image::new(w, h, &data).unwrap();
    let full = Mask::Luminance { low: -10.0, high: 10.0, feather: 0.0 };
    let layer = Layer::new(full, Adjustments { exposure: 1.0, saturation: 0.3, ..Default::default() });
    let one = Document { version: 2, layers: vec![layer.clone()] };
    let corrected = apply(image, &one).unwrap();
    // switched off or strength 0: like no layer at all (bit for bit the input)
    for l in [Layer { enabled: false, ..layer.clone() }, Layer { amount: 0.0, ..layer.clone() }] {
        let mut d = Document { version: 2, layers: vec![l] };
        d.settle_version();
        assert_eq!(d.version, 4);
        assert!(!d.layers[0].active());
        assert_eq!(apply(image, &d).unwrap(), data);
        assert_eq!(margin(&d, [w, h]), 0);
    }
    // half strength: half way between input and full correction
    let mut d = Document { version: 2, layers: vec![Layer { amount: 0.5, ..layer.clone() }] };
    d.settle_version();
    let half = apply(image, &d).unwrap();
    let max = half.iter().zip(&data).zip(&corrected).map(|((h, i), c)| (h - (i + 0.5 * (c - i))).abs()).fold(0.0, f32::max);
    assert!(max < 1e-6, "{max}");
    // a switched-off layer between two others leaves the rest as it was
    let other = Layer::new(Mask::Radial { center: [0.5, 0.5], radii: [0.3, 0.2], angle: 0.0, feather: 0.5 }, Adjustments { contrast: 0.4, ..Default::default() });
    let two = Document { version: 2, layers: vec![layer.clone(), other.clone()] };
    let mut three = Document { version: 2, layers: vec![layer, Layer { enabled: false, ..other.clone() }, other] };
    three.settle_version();
    assert_eq!(apply(image, &two).unwrap(), apply(image, &three).unwrap());
}

#[test]
fn another_photo_keeps_name_switch_and_strength() {
    let leaf = Mask::Raster { raster: String::new(), source: RasterSource::Subject { instances: Instances::All }, feather: 0.0, range: None, edge: 0.0 };
    let mut d = Document { version: 2, layers: vec![Layer { name: Some("Motiv".into()), enabled: false, amount: 0.4, ..Layer::new(leaf, Adjustments { exposure: 0.5, ..Default::default() }) }] };
    d.settle_version();
    let (other, dropped) = d.for_other_photo();
    let other = other.unwrap();
    assert_eq!(dropped, 0);
    assert_eq!((other.layers[0].name.as_deref(), other.layers[0].enabled, other.layers[0].amount), (Some("Motiv"), false, 0.4));
    assert_eq!(other.version, 4);
}
