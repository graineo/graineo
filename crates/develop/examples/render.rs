//! Develops an X3F file with settings from a JSON file (format like the
//! app's sidecar files) and writes a JPEG, for tests without UI.
//!
//! `cargo run --release -p develop --example render -- IMAGE.X3F settings.json out.jpg [long_edge]`

use develop::{export, geometry, Session, Settings};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: render IMAGE.X3F settings.json out.jpg [long_edge]");
        std::process::exit(2);
    }
    let s: Settings = std::fs::read_to_string(&args[2]).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut session = Session::open(&args[1]).expect("open file");
    let t = std::time::Instant::now();
    let img = geometry::apply(session.render_full(&s), &s, session.cal.orientation);
    eprintln!("developed in {:.2?} ({}×{})", t.elapsed(), img.width, img.height);
    let img = match args.get(4).and_then(|v| v.parse::<usize>().ok()) {
        Some(edge) if edge < img.width.max(img.height) => {
            let f = img.width.max(img.height).div_ceil(edge);
            let (w, h) = (img.width / f, img.height / f);
            let mut data = vec![0u16; w * h * 3];
            for y in 0..h {
                for x in 0..w {
                    for c in 0..3 {
                        let mut acc = 0u32;
                        for yy in 0..f {
                            for xx in 0..f {
                                acc += img.data[((y * f + yy) * img.width + x * f + xx) * 3 + c] as u32;
                            }
                        }
                        data[(y * w + x) * 3 + c] = (acc / (f * f) as u32) as u16;
                    }
                }
            }
            develop::Rgb16 { width: w, height: h, data }
        }
        _ => img,
    };
    // .tif/.tiff → lossless 16 bit (e.g. for sharpness comparisons), otherwise JPEG
    let out = std::path::Path::new(&args[3]);
    if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff")) {
        export::write_tiff16(&img, out).expect("write TIFF");
    } else {
        export::write_jpeg(&img, out, 92).expect("write JPEG");
    }
}
