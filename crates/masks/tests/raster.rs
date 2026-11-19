//! Raster (bitmap) masks: JSON, version rules, missing rasters, resolution independence, combination with a brush.
use std::sync::Arc;

use masks::raster::{self, RasterData};
use masks::{Adjustments, Document, Image, Instances, Layer, Mask, Operation, RasterSource};

/// A disc in the middle of a 400 × 300 raster (other contents per test, so that names never collide).
fn disc(seed: u8) -> Arc<RasterData> {
    let (w, h) = (400u32, 300u32);
    let data = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
            let d = ((x - 200.0).powi(2) + (y - 150.0).powi(2)).sqrt();
            if d < 80.0 { 255 } else if d < 81.0 { seed } else { 0 }
        })
        .collect();
    Arc::new(RasterData { width: w, height: h, data })
}

fn leaf(id: &str) -> Mask {
    Mask::Raster { raster: id.into(), source: RasterSource::Subject { instances: Instances::All }, feather: 0.0, range: None, edge: 0.0 }
}

#[test]
fn json_roundtrip_and_version() {
    let id = raster::remember(disc(1));
    let doc = Document { version: 3, layers: vec![Layer::new(leaf(&id), Adjustments { exposure: 1.0, ..Default::default() })] };
    let json = doc.to_json().unwrap();
    assert!(json.contains("\"type\": \"raster\"") && json.contains("\"kind\": \"subject\"") && json.contains("\"instances\": \"all\""), "{json}");
    assert_eq!(Document::from_json(&json).unwrap(), doc);
    // version 2 must not carry rasters, an unknown version is refused
    let mut v2 = doc.clone();
    v2.version = 2;
    assert!(v2.validate().is_err());
    v2.settle_version();
    assert_eq!(v2.version, 3);
    v2.layers.clear();
    v2.settle_version();
    assert_eq!(v2.version, 2);
    assert!(Document::from_json(&json.replace("\"version\": 3", "\"version\": 5")).is_err());
    // a path or other text is never a raster name
    let bad = json.replace(&id, "../../etc/passwd");
    assert!(Document::from_json(&bad).is_err());
    let some = r#"{"version":3,"layers":[{"mask":{"type":"raster","raster":"","source":{"kind":"person","instances":{"some":[1,2]}},"feather":0.01},"adjustments":{}}]}"#;
    Document::from_json(some).unwrap();
}

#[test]
fn missing_raster_weighs_nothing() {
    let data = vec![0.18f32; 40 * 30 * 3];
    let image = Image::new(40, 30, &data).unwrap();
    let pending = masks::rasterize(image, &leaf("")).unwrap();
    assert!(pending.iter().all(|&w| w == 0.0));
    let unknown = format!("sha256:{}", "0".repeat(64));
    assert!(masks::rasterize(image, &leaf(&unknown)).unwrap().iter().all(|&w| w == 0.0));
    // the inverted missing raster stays 1 everywhere: inversion is the user's choice, the raster itself never fills up
    let doc = Document { version: 3, layers: vec![Layer::new(leaf(""), Adjustments { exposure: 1.0, ..Default::default() })] };
    assert_eq!(raster::missing(&doc), 1);
    assert_eq!(masks::apply(image, &doc).unwrap(), data);
}

#[test]
fn same_mask_at_every_resolution() {
    let id = raster::remember(disc(2));
    let m = leaf(&id);
    let small = vec![0.18f32; 400 * 300 * 3];
    let big = vec![0.18f32; 1600 * 1200 * 3];
    let a = masks::rasterize(Image::new(400, 300, &small).unwrap(), &m).unwrap();
    let b = masks::rasterize(Image::new(1600, 1200, &big).unwrap(), &m).unwrap();
    // reduce the big one 4× and compare
    let mut err = 0.0f64;
    for y in 0..300 {
        for x in 0..400 {
            let mut s = 0.0;
            for dy in 0..4 {
                for dx in 0..4 {
                    s += b[(y * 4 + dy) * 1600 + x * 4 + dx];
                }
            }
            err += ((s / 16.0) - a[y * 400 + x]).powi(2) as f64;
        }
    }
    let rmse = (err / (400.0 * 300.0)).sqrt();
    assert!(rmse < 0.02, "RMSE {rmse}");
    // centre inside, corner outside
    assert_eq!(a[150 * 400 + 200], 1.0);
    assert_eq!(a[0], 0.0);
}

#[test]
fn brush_subtracts_from_raster() {
    let id = raster::remember(disc(3));
    let m = Mask::Combine {
        operation: Operation::Subtract,
        left: Box::new(leaf(&id)),
        right: Box::new(Mask::Brush { points: vec![[0.5, 0.5]], radius: 0.05, hardness: 1.0, flow: 1.0, density: 1.0, auto: None }),
    };
    m.validate().unwrap();
    let data = vec![0.18f32; 400 * 300 * 3];
    let w = masks::rasterize(Image::new(400, 300, &data).unwrap(), &m).unwrap();
    assert_eq!(w[150 * 400 + 200], 0.0, "brushed out");
    assert_eq!(w[150 * 400 + 260], 1.0, "rest of the subject kept");
}

#[test]
fn foreign_aspect_ratio_is_not_used() {
    let id = raster::remember(disc(4));
    let data = vec![0.18f32; 300 * 300 * 3];
    let w = masks::rasterize(Image::new(300, 300, &data).unwrap(), &leaf(&id)).unwrap();
    assert!(w.iter().all(|&v| v == 0.0));
}

#[test]
fn other_photo_gets_rules_not_rasters() {
    let id = raster::remember(disc(5));
    let click = Mask::Raster { raster: id.clone(), source: RasterSource::Click { points: vec![[0.5, 0.5, 1.0]], rect: None }, feather: 0.0, range: None, edge: 0.0 };
    let brush = Mask::Brush { points: vec![[0.5, 0.5]], radius: 0.05, hardness: 1.0, flow: 1.0, density: 1.0, auto: None };
    let doc = Document {
        version: 3,
        layers: vec![
            Layer::new(Mask::Combine { operation: Operation::Subtract, left: Box::new(leaf(&id)), right: Box::new(brush.clone()) }, Adjustments { exposure: 0.5, ..Default::default() }),
            Layer::new(Mask::Combine { operation: Operation::Subtract, left: Box::new(click.clone()), right: Box::new(brush.clone()) }, Adjustments::default()),
            Layer::new(Mask::Combine { operation: Operation::Add, left: Box::new(click), right: Box::new(brush.clone()) }, Adjustments::default()),
        ],
    };
    let (other, dropped) = doc.for_other_photo();
    let other = other.unwrap();
    assert_eq!(dropped, 2);
    assert_eq!(other.layers.len(), 2, "subtraction without its base is left out, the added brush stays");
    other.validate().unwrap();
    assert!(raster::ids(&other).iter().all(|i| i.is_empty()), "pending, computed again on the other photo");
    assert_eq!(other.layers[1].mask, brush);
    let (none, _) = Document { version: 3, layers: vec![Layer::new(Mask::Raster { raster: id, source: RasterSource::Painted, feather: 0.0, range: None, edge: 0.0 }, Adjustments::default())] }.for_other_photo();
    assert!(none.is_none());
}

/// Looking up a segment map is a name comparison, not a hash over every map in memory (review 1.3.0, logic M3: it ran
/// in the UI thread each frame). Measured with six maps of 1024 × 683; prints the time per lookup.
#[test]
#[ignore]
fn depth_lookup_time() {
    use masks::raster::{remember_depth, resolve_depth};
    use masks::DepthData;
    use std::sync::Arc;
    let ids: Vec<String> = (0..6u16)
        .map(|k| remember_depth(Arc::new(DepthData { width: 1024, height: 683, data: (0..1024 * 683u32).map(|i| (i as u16).wrapping_mul(k + 3)).collect() })))
        .collect();
    let time = |id: &str| {
        let t = std::time::Instant::now();
        for _ in 0..20 {
            let _ = resolve_depth(id);
        }
        t.elapsed().as_secs_f64() * 1e6 / 20.0
    };
    let d = DepthData { width: 1024, height: 683, data: vec![7; 1024 * 683] };
    let t = std::time::Instant::now();
    let _ = d.id();
    let name = t.elapsed().as_secs_f64() * 1e3;
    println!("lookup: oldest {:.2} µs, newest {:.2} µs, missing {:.2} µs (without library); name of one map {name:.2} ms", time(&ids[0]), time(&ids[5]), time(&format!("sha256:{}", "0".repeat(64))));
}

/// The name of a segment map stays what it was (stored maps are found by it): SHA-256 over "u16", width, height and
/// the values, little endian – also when the values span several hashing blocks.
#[test]
fn depth_names_are_unchanged() {
    use sha2::{Digest, Sha256};
    for (w, h) in [(3u32, 2u32), (300, 250)] {
        let d = masks::DepthData { width: w, height: h, data: (0..w * h).map(|i| (i * 7919) as u16).collect() };
        let mut s = Sha256::new();
        s.update(b"u16");
        s.update(w.to_le_bytes());
        s.update(h.to_le_bytes());
        for v in &d.data {
            s.update(v.to_le_bytes());
        }
        let want: String = "sha256:".chars().chain(s.finalize().iter().flat_map(|b| format!("{b:02x}").chars().collect::<Vec<_>>())).collect();
        assert_eq!(d.id(), want);
    }
}
