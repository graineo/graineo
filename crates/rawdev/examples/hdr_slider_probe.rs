//! `cargo run --example hdr_slider_probe -- FILE`: develops the preview of FILE with a few slider states and prints the
//! mean of the SDR preview and of its HDR rendition (SDR × preview gain map, as the EDR display shows it). A slider
//! change must change both.

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("FILE"));
    let base = develop::Settings::default();
    let doc = rawdev::RawDoc::open_for(&path, &base, &|| true).expect("open").expect("cancelled");
    println!("own gain map: {} · hdr auto: {}", doc.hdr.is_some(), rawdev::export::hdr_auto(&doc));
    let lin = |v: u16| {
        let c = v as f32 / 65535.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let states: Vec<(&str, Box<dyn Fn(&mut develop::Settings)>)> = vec![
        ("default", Box::new(|_| {})),
        ("exposure -5", Box::new(|s| s.exposure = -5.0)),
        ("exposure -1", Box::new(|s| s.exposure = -1.0)),
        ("contrast +41", Box::new(|s| s.contrast = 0.41)),
        ("highlights -100", Box::new(|s| s.highlight = -1.0)),
    ];
    for (name, f) in states {
        let mut s = base.clone();
        f(&mut s);
        let img = doc.render_preview(&s);
        let (w, h) = (img.width, img.height);
        let rgb: Vec<u8> = img.data.iter().map(|v| (v >> 8) as u8).collect();
        let sdr: f64 = img.data.iter().map(|v| lin(*v) as f64).sum::<f64>() / img.data.len() as f64;
        let map = rawdev::export::hdr_preview_map(&doc, &s, &rgb, w, h);
        let hdr = match &map {
            Some(m) => {
                let mut sum = 0f64;
                for y in 0..h {
                    let ky = (y * m.h / h).min(m.h - 1);
                    for x in 0..w {
                        let kx = (x * m.w / w).min(m.w - 1);
                        let g = m.log2[ky * m.w + kx].exp2();
                        let i = (y * w + x) * 3;
                        for c in 0..3 {
                            sum += (lin(img.data[i + c]) * g) as f64;
                        }
                    }
                }
                format!("{:.4} (map max {:.2} EV)", sum / (w * h * 3) as f64, m.alt_headroom)
            }
            None => "no map".into(),
        };
        println!("{name:16} {w}×{h}  SDR mean {sdr:.4}  HDR mean {hdr}");
    }
}
