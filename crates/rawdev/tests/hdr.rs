//! Read an HDR gain map: `GRAINEO_HDR_IMAGE=<heic> cargo test -p rawdev --release --test hdr -- --ignored --nocapture`

#[test]
#[ignore]
fn gain_map() {
    let Ok(p) = std::env::var("GRAINEO_HDR_IMAGE") else { return };
    let t = std::time::Instant::now();
    let doc = rawdev::RawDoc::open(std::path::Path::new(&p), &|| true).unwrap().unwrap();
    println!("open with HDR: {:.0?}", t.elapsed());
    let k = doc.hdr.as_ref().expect("no gain map");
    let max = k.log2.iter().cloned().fold(f32::MIN, f32::max);
    let above = k.log2.iter().filter(|v| **v > 0.5).count();
    println!("{}×{}, headroom {:.2} ({}), largest brightening {:.2} EV, pixels > ½ EV: {:.1} %", k.w, k.h, k.headroom, if k.iso { "ISO" } else { "Apple" }, max, above as f64 * 100.0 / k.log2.len() as f64);
    let mut v = k.log2.clone();
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |f: f32| v[((v.len() - 1) as f32 * f) as usize];
    println!("percentiles log2: 1 % {:.2}, 10 % {:.2}, 50 % {:.2}, 90 % {:.2}, 99 % {:.2}", q(0.01), q(0.1), q(0.5), q(0.9), q(0.99));
    assert!(max > 0.5);
}

/// Export with crop/rotation/fine angle: the gain map must have the same geometry as the image.
/// `GRAINEO_HDR_IMAGE=<heic> GRAINEO_TEST_SCRATCH=<folder> cargo test -p rawdev --release --test hdr -- --ignored --nocapture export_gain_map`
#[test]
#[ignore]
fn export_gain_map_geometry() {
    let (Ok(p), Ok(scratch)) = (std::env::var("GRAINEO_HDR_IMAGE"), std::env::var("GRAINEO_TEST_SCRATCH")) else { return };
    let p = std::path::PathBuf::from(p);
    let scratch = std::path::PathBuf::from(scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let doc = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    assert!(doc.hdr.is_some(), "no gain map");
    let orig = library::imageio::gain_map(&p).expect("original map");
    println!("original {}×{}, map {}×{}, orientation {}", doc.full_size[0], doc.full_size[1], orig.w, orig.h, doc.orientation);
    let cases: [(&str, [f32; 4], f32, u8); 5] = [
        ("unveraendert", [0.5, 0.5, 1.0, 1.0], 0.0, 0),
        ("zuschnitt", [0.3, 0.4, 0.4, 0.5], 0.0, 0),
        ("gedreht", [0.5, 0.5, 1.0, 1.0], 0.0, 1),
        ("gedreht_zuschnitt", [0.5, 0.5, 0.6, 0.7], 0.0, 1),
        ("winkel", [0.5, 0.5, 0.7, 0.7], 4.0, 0),
    ];
    for (name, crop, angle_deg, rotation) in cases {
        let mut s = develop::Settings::default();
        s.crop = crop;
        s.crop_angle = angle_deg;
        s.rotate = rotation;
        let out = scratch.join(format!("hdr-{name}.jpg"));
        let _ = std::fs::remove_file(&out);
        rawdev::export::export(&doc, &s, develop::export::Format::Jpeg { quality: 90 }, &out).unwrap();
        let f = rawdev::export::export_frame(&doc, &s);
        assert!(rawdev::export::append_gain_map(&f, &p, &out, 90).unwrap(), "{name}: no map attached");
        let e = library::imageio::properties(&out).unwrap();
        let (w, h) = (e.width.unwrap() as usize, e.height.unwrap() as usize);
        let gm = library::imageio::gain_map(&out).unwrap_or_else(|| panic!("{name}: map in the export not readable"));
        // image size as computed, aspect ratio of the map same as the image
        let (ew, eh) = if f.is_identity() { (f.sw as usize, f.sh as usize) } else { (f.w.round() as usize, f.h.round() as usize) };
        assert_eq!((w, h), (ew, eh), "{name}: image size");
        let (rb, rk) = (w as f32 / h as f32, gm.w as f32 / gm.h as f32);
        assert!((rb / rk - 1.0).abs() < 0.04, "{name}: aspect ratio image {rb:.3} ↔ map {rk:.3}");
        // content: mean of the map in the export against mean of the original map at the same positions
        let mut expected = 0.0f64;
        let n = 48;
        for j in 0..n {
            for i in 0..n {
                let q = f.to_src([(i as f32 + 0.5) / n as f32, (j as f32 + 0.5) / n as f32]);
                let x = ((q[0] / f.sw * orig.w as f32) as usize).min(orig.w - 1);
                let y = ((q[1] / f.sh * orig.h as f32) as usize).min(orig.h - 1);
                expected += orig.log2[y * orig.w + x] as f64;
            }
        }
        expected /= (n * n) as f64;
        let actual = gm.log2.iter().map(|v| *v as f64).sum::<f64>() / gm.log2.len() as f64;
        println!("{name}: image {w}×{h}, map {}×{}, mean log2 expected {expected:.3} actual {actual:.3}", gm.w, gm.h);
        assert!((expected - actual).abs() < 0.06, "{name}: map does not match the image ({expected:.3} ↔ {actual:.3})");
    }
}
