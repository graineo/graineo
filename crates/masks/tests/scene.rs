//! Depth and scene-part masks: segment maps (16 bit), whole segments chosen by depth class or by click, version 4 only
//! when used, paste.
use std::sync::Arc;

use masks::raster::{self, segment_label, DepthData};
use masks::{Adjustments, DepthClass, Document, Image, Layer, Mask, RasterSource};

/// Three segments side by side, 400 × 200: number 0 near (left third), 1 middle, 2 far (right third); `seed` keeps the
/// names apart.
fn thirds(seed: u16) -> Arc<DepthData> {
    let (w, h) = (400u32, 200u32);
    let data = (0..w * h)
        .map(|i| {
            let x = i % w;
            let (c, id) = if x < 133 { (DepthClass::Near, 0) } else if x < 266 { (DepthClass::Middle, 1) } else { (DepthClass::Far, 2) };
            segment_label(c, id + 3 * (seed & 1))
        })
        .collect();
    Arc::new(DepthData { width: w, height: h, data })
}

fn depth_leaf(id: &str, classes: &[DepthClass], toggled: &[u16]) -> Mask {
    Mask::Raster { raster: id.into(), source: RasterSource::Depth { model: "depth".into(), classes: classes.to_vec(), toggled: toggled.to_vec() }, feather: 0.0, range: None, edge: 0.0 }
}

fn weights(m: &Mask, w: usize, h: usize) -> Vec<f32> {
    let px = vec![0.2f32; w * h * 3];
    masks::rasterize(Image::new(w, h, &px).unwrap(), m).unwrap()
}

#[test]
fn classes_select_whole_segments_without_a_new_raster() {
    let id = raster::remember_depth(thirds(0));
    let w = weights(&depth_leaf(&id, &[DepthClass::Near], &[]), 200, 100);
    let at = |w: &[f32], x: f32| w[50 * 200 + (x * 200.0) as usize];
    // whole segments: full weight up to the border of the segment, nothing beyond (no depth-dependent fall-off)
    assert_eq!((at(&w, 0.05), at(&w, 0.3), at(&w, 0.4), at(&w, 0.9)), (1.0, 1.0, 0.0, 0.0));
    // middle and far: the same raster, only the selection changed
    let w = weights(&depth_leaf(&id, &[DepthClass::Middle, DepthClass::Far], &[]), 200, 100);
    assert_eq!((at(&w, 0.05), at(&w, 0.5), at(&w, 0.95)), (0.0, 1.0, 1.0));
    // a click takes the far segment out
    let w = weights(&depth_leaf(&id, &[DepthClass::Middle, DepthClass::Far], &[2]), 200, 100);
    assert_eq!((at(&w, 0.5), at(&w, 0.95)), (1.0, 0.0));
    // the only values are 0 and 1, apart from the one-pixel spatial transition at the segment borders
    let soft = w.iter().filter(|v| **v > 0.0 && **v < 1.0).count();
    assert!(soft <= 2 * 100, "{soft}");
}

#[test]
fn the_soft_edge_is_spatial() {
    let id = raster::remember_depth(thirds(0));
    let mut leaf = depth_leaf(&id, &[DepthClass::Near], &[]);
    if let Mask::Raster { feather, .. } = &mut leaf {
        *feather = 0.05;
    }
    let w = weights(&leaf, 400, 200);
    let row: Vec<f32> = (0..400).map(|x| w[100 * 400 + x]).collect();
    // falls off around the border of the near segment (x = 133 of 400), symmetrically in pixels
    assert!(row[60] > 0.99 && row[220] < 0.01, "{} {}", row[60], row[220]);
    assert!((row[133] - 0.5).abs() < 0.1, "{}", row[133]);
    assert!(row.windows(2).all(|p| p[1] <= p[0] + 1e-6));
}

#[test]
fn missing_depth_weighs_nothing() {
    let w = weights(&depth_leaf("", &DepthClass::ALL, &[]), 40, 20);
    assert!(w.iter().all(|v| *v == 0.0));
    // an 8-bit raster of the same name space does not stand in for a segment map
    let id8 = raster::remember(Arc::new(masks::RasterData { width: 4, height: 2, data: vec![255; 8] }));
    assert!(weights(&depth_leaf(&id8, &DepthClass::ALL, &[]), 40, 20).iter().all(|v| *v == 0.0));
}

#[test]
fn version_4_only_when_used_and_json_roundtrip() {
    let id = raster::remember_depth(thirds(1));
    let mut doc = Document { version: 2, layers: vec![Layer::new(depth_leaf(&id, &[DepthClass::Near], &[4]), Adjustments { exposure: -0.5, ..Default::default() })] };
    assert!(doc.validate().is_err(), "depth needs version 4");
    doc.settle_version();
    assert_eq!(doc.version, 4);
    let json = doc.to_json().unwrap();
    assert!(json.contains("\"kind\": \"depth\"") && json.contains("\"model\": \"depth\"") && json.contains("\"near\"") && json.contains("\"toggled\""), "{json}");
    assert!(!json.contains("\"range\""), "{json}");
    assert_eq!(Document::from_json(&json).unwrap(), doc);
    // version 3 with a depth leaf is refused (as an older program would)
    assert!(Document::from_json(&json.replace("\"version\": 4", "\"version\": 3")).is_err());
    // a depth leaf with a window over the depth (from/to/softness, never published) is refused
    if let Mask::Raster { range, .. } = &mut doc.layers[0].mask {
        *range = Some([0.0, 0.3, 0.1]);
    }
    assert!(doc.validate().is_err());
    // sky alone: version 4 as well; without new leaves back to 2
    let mut sky = Document { version: 2, layers: vec![Layer::new(Mask::Raster { raster: String::new(), source: RasterSource::Scene { part: "sky".into() }, feather: 0.002, range: None, edge: 0.0 }, Adjustments::default())] };
    sky.settle_version();
    assert_eq!(sky.version, 4);
    sky.layers.clear();
    sky.settle_version();
    assert_eq!(sky.version, 2);
    // an unknown scene part is refused
    let moon = Document { version: 4, layers: vec![Layer::new(Mask::Raster { raster: String::new(), source: RasterSource::Scene { part: "moon".into() }, feather: 0.0, range: None, edge: 0.0 }, Adjustments::default())] };
    assert!(moon.validate().is_err());
}

#[test]
fn paste_keeps_the_classes_but_not_the_raster_or_the_clicked_segments() {
    let id = raster::remember_depth(thirds(0));
    let doc = Document { version: 4, layers: vec![Layer::new(depth_leaf(&id, &[DepthClass::Far], &[1, 2]), Adjustments { exposure: -1.0, ..Default::default() })] };
    let (other, dropped) = doc.for_other_photo();
    assert_eq!(dropped, 0);
    let other = other.unwrap();
    assert_eq!(other.version, 4);
    match &other.layers[0].mask {
        Mask::Raster { raster, source: RasterSource::Depth { model, classes, toggled }, range, .. } => {
            assert!(raster.is_empty(), "pending on the other photo");
            assert_eq!(model, "depth");
            assert_eq!(classes, &vec![DepthClass::Far]);
            assert!(toggled.is_empty(), "segments of another photo");
            assert_eq!(*range, None);
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(raster::missing(&other), 1);
    assert_eq!(raster::missing(&doc), 0);
}
