//! Measurement helper: compare the face detectors on a set of images (macOS: Apple Vision against YuNet).
//! Prints one JSON line per image with the boxes of both detectors and the times.
//!
//!   detector_compare [--sface MODEL] [--edges 640,1600] IMAGE…
//!
//! `GRAINEO_FACE_PLAIN_GEMM=1` measures YuNet with the portable matrix product (as on Windows/Linux).
//! Images are scaled to a longest edge of 2400 px first, as the library does. With `--sface`, every face found by
//! both detectors is also aligned twice (Vision points, YuNet points) and the cosine similarity of the two SFace
//! vectors is printed (how well the YuNet landmarks fit the recognition). On other systems only YuNet runs.

use std::time::Instant;

fn boxes(v: &[faces::Detection]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|g| {
            let p = g.points.map(|p| p.iter().map(|q| format!("{:.1},{:.1}", q.0, q.1)).collect::<Vec<_>>().join(",")).unwrap_or_default();
            format!("[{:.1},{:.1},{:.1},{:.1},{:.3},[{p}]]", g.x, g.y, g.b, g.h, g.confidence)
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut sface = None;
    let mut edges: Vec<u32> = vec![faces::yunet::COARSE_EDGE, faces::yunet::FINE_EDGE];
    let mut files = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sface" => sface = Some(faces::sface::Sface::load(std::path::Path::new(&args.next().expect("model"))).expect("SFace")),
            "--edges" => edges = args.next().expect("edges").split(',').map(|e| e.parse().expect("number")).collect(),
            _ => files.push(a),
        }
    }
    // warm up (model parse, first allocation)
    let _ = faces::yunet::detect(&image::RgbImage::new(64, 64));
    for f in files {
        let Ok(img) = image::open(&f) else {
            eprintln!("{f}: not readable");
            continue;
        };
        let mut img = img.to_rgb8();
        let long = img.width().max(img.height());
        if long > 2400 {
            let s = 2400.0 / long as f32;
            img = image::imageops::resize(&img, (img.width() as f32 * s) as u32, (img.height() as f32 * s) as u32, image::imageops::FilterType::Triangle);
        }
        let t = Instant::now();
        let yunet = faces::yunet::detect_scales(&img, &edges).unwrap_or_default();
        let yunet_ms = t.elapsed().as_secs_f64() * 1000.0;
        #[cfg(target_os = "macos")]
        let (vision, vision_ms) = {
            let t = Instant::now();
            let v = faces::detector::tile_grid(&img, 2, 0.6, faces::vision::detect).unwrap_or_default();
            (v, t.elapsed().as_secs_f64() * 1000.0)
        };
        #[cfg(not(target_os = "macos"))]
        let (vision, vision_ms): (Vec<faces::Detection>, f64) = (Vec::new(), 0.0);
        let mut sims = Vec::new();
        let mut sface_ms = 0.0;
        if let Some(m) = &sface {
            for v in &vision {
                let Some(pv) = v.points else { continue };
                let (cx, cy) = (v.x + v.b / 2.0, v.y + v.h / 2.0);
                let Some(y) = yunet.iter().find(|y| (y.x + y.b / 2.0 - cx).abs() < v.b * 0.4 && (y.y + y.h / 2.0 - cy).abs() < v.h * 0.4) else { continue };
                let t = Instant::now();
                let a = m.embedding(&faces::align::crop_aligned(&img, &pv)).unwrap();
                sface_ms += t.elapsed().as_secs_f64() * 1000.0;
                let b = m.embedding(&faces::align::crop_aligned(&img, &y.points.unwrap())).unwrap();
                sims.push(format!("{:.3}", faces::sface::similarity(&a, &b)));
            }
        }
        println!(
            "{{\"file\":\"{f}\",\"w\":{},\"h\":{},\"yunet_ms\":{yunet_ms:.1},\"vision_ms\":{vision_ms:.1},\"sface_ms\":{sface_ms:.1},\"yunet\":{},\"vision\":{},\"sface_sim\":[{}]}}",
            img.width(),
            img.height(),
            boxes(&yunet),
            boxes(&vision),
            sims.join(",")
        );
    }
}
