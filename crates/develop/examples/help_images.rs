//! Generates the before/after sample images of the context help (crates/app/assets/help/)
//! from the DP2 Merrill test images; reproducible, one image after another.
//!
//! `cargo run --release -p develop --features spp-gpu --example help_images -- FOLDER [TARGET] [name …]`
//!
//! FOLDER: folder with the test images, TARGET: output (default
//! crates/app/assets/help). With names, only these samples are recomputed.
//! Each sample yields `<name>_before.jpg` and `<name>_after.jpg` (before/after), 280 × 190 pixels each
//! (140 × 95 points in the app, so pixel-exact on Retina).

use develop::{geometry, Rgb16, Session, Settings};

const W: usize = 280;
const H: usize = 190;
const QUALITY: u8 = 80;

/// Crop of a sample.
#[derive(Clone, Copy)]
enum Area {
    /// Crop (centre x, centre y, width relative to the upright image), downscaled to 280 × 190;
    /// the height follows from the aspect ratio.
    Fit(f32, f32, f32),
    /// Pixel-exact crop at full resolution around (x, y) relative, magnified `zoom` times
    /// (1 = 100 %, 2 = 200 %).
    Pixel(f32, f32, usize),
}

struct Example {
    name: &'static str,
    file: &'static str,
    area: Area,
    /// Common base setting of both images
    base: fn(&mut Settings),
    /// The change shown
    after: fn(&mut Settings),
}

fn none(_: &mut Settings) {}

fn examples() -> Vec<Example> {
    vec![
        // ── Basic settings ──
        Example { name: "temperature", file: "DP2M5423", area: Area::Fit(0.42, 0.55, 0.55), base: none, after: |s| s.wb_temp = 0.7 },
        Example { name: "tint", file: "DP2M5412", area: Area::Fit(0.24, 0.62, 0.42), base: none, after: |s| s.wb_tint = 0.5 },
        Example { name: "exposure", file: "DP2M5428", area: Area::Fit(0.68, 0.55, 0.4), base: none, after: |s| s.exposure = 0.7 },
        Example { name: "contrast", file: "DP2M5427", area: Area::Fit(0.82, 0.62, 0.34), base: none, after: |s| s.contrast = 1.0 },
        Example { name: "highlights", file: "DP2M5439", area: Area::Fit(0.5, 0.45, 0.55), base: none, after: |s| s.highlight = -1.0 },
        Example { name: "shadows", file: "DP2M5430", area: Area::Fit(0.2, 0.78, 0.4), base: none, after: |s| s.shadow = 1.5 },
        Example { name: "fill_light", file: "DP2M5441", area: Area::Fit(0.5, 0.5, 0.98), base: none, after: |s| s.fill_light = 1.0 },
        Example { name: "clarity", file: "DP2M5420", area: Area::Fit(0.72, 0.1, 0.18), base: none, after: |s| s.clarity = 0.8 },
        Example { name: "vibrance", file: "DP2M5412", area: Area::Fit(0.2, 0.45, 0.4), base: none, after: |s| s.vibrance = 0.6 },
        Example { name: "saturation", file: "DP2M5412", area: Area::Fit(0.25, 0.75, 0.45), base: none, after: |s| s.saturation = 0.4 },
        // ── Tone curve, colour mixer, black and white, color grading ──
        Example {
            name: "curve",
            file: "DP2M5412",
            area: Area::Fit(0.2, 0.45, 0.4),
            base: none,
            after: |s| s.tone_curve.rgb = vec![[0.0, 0.0], [0.25, 0.18], [0.75, 0.84], [1.0, 1.0]],
        },
        Example { name: "color_mixer", file: "DP2M5415", area: Area::Fit(0.85, 0.22, 0.3), base: none, after: |s| s.hsl[5][2] = -0.8 },
        Example { name: "bw_mix", file: "DP2M5412", area: Area::Fit(0.5, 0.17, 0.33), base: |s| s.monochrome = true, after: |s| s.mix = [0.85, 0.25, -0.1] },
        Example {
            name: "color_grading",
            file: "DP2M5412",
            area: Area::Fit(0.2, 0.45, 0.4),
            base: none,
            after: |s| {
                s.grade_shadows = [220.0, 0.5];
                s.grade_highlights = [45.0, 0.45];
            },
        },
        // ── Effects ──
        Example { name: "vignette", file: "DP2M5440", area: Area::Fit(0.5, 0.5, 0.98), base: none, after: |s| s.vignette = -0.6 },
        Example { name: "grain", file: "DP2M5415", area: Area::Pixel(0.85, 0.15, 1), base: none, after: |s| s.grain_amount = 0.5 },
        Example { name: "matte_shadows", file: "DP2M5439", area: Area::Fit(0.5, 0.7, 0.6), base: none, after: |s| s.fade = 0.12 },
        // ── Details ──
        Example { name: "sigma_sharpness", file: "DP2M5412", area: Area::Pixel(0.5, 0.15, 2), base: none, after: |s| s.sigma_sharpness = 2.0 },
        Example { name: "sharpening", file: "DP2M5412", area: Area::Pixel(0.5, 0.15, 2), base: none, after: |s| s.sharpen = 1.0 },
    ]
}

/// Area-preserving downscale (box filter with fractions) to `w` × `h`.
fn resize(img: &Rgb16, w: usize, h: usize) -> Rgb16 {
    let (sx, sy) = (img.width as f64 / w as f64, img.height as f64 / h as f64);
    let mut data = vec![0u16; w * h * 3];
    for y in 0..h {
        let (y0, y1) = (y as f64 * sy, (y + 1) as f64 * sy);
        for x in 0..w {
            let (x0, x1) = (x as f64 * sx, (x + 1) as f64 * sx);
            let mut acc = [0f64; 3];
            let mut wsum = 0f64;
            let mut yy = y0.floor() as usize;
            while (yy as f64) < y1 && yy < img.height {
                let wy = (y1.min(yy as f64 + 1.0) - y0.max(yy as f64)).max(0.0);
                let mut xx = x0.floor() as usize;
                while (xx as f64) < x1 && xx < img.width {
                    let wx = (x1.min(xx as f64 + 1.0) - x0.max(xx as f64)).max(0.0);
                    let k = wx * wy;
                    let p = (yy * img.width + xx) * 3;
                    for c in 0..3 {
                        acc[c] += img.data[p + c] as f64 * k;
                    }
                    wsum += k;
                    xx += 1;
                }
                yy += 1;
            }
            for c in 0..3 {
                data[(y * w + x) * 3 + c] = (acc[c] / wsum.max(1e-9)).round() as u16;
            }
        }
    }
    Rgb16 { width: w, height: h, data }
}

/// Upscale without smoothing (each image pixel as a `zoom` × `zoom` block).
fn enlarge(img: &Rgb16, zoom: usize) -> Rgb16 {
    let (w, h) = (img.width * zoom, img.height * zoom);
    let mut data = vec![0u16; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let s = ((y / zoom) * img.width + x / zoom) * 3;
            data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&img.data[s..s + 3]);
        }
    }
    Rgb16 { width: w, height: h, data }
}

fn render(session: &mut Session, s: &Settings, area: Area) -> Rgb16 {
    let [fw, fh] = session.size();
    match area {
        Area::Fit(cx, cy, w) => {
            let q = geometry::quarters(s, session.cal.orientation);
            let (ow, oh) = if q % 2 == 1 { (fh as f32, fw as f32) } else { (fw as f32, fh as f32) };
            let mut w = w;
            let mut h = w * ow / oh * H as f32 / W as f32;
            if h > 1.0 {
                w /= h;
                h = 1.0;
            }
            let mut s = s.clone();
            s.crop = [cx.clamp(w / 2.0, 1.0 - w / 2.0), cy.clamp(h / 2.0, 1.0 - h / 2.0), w, h];
            let img = geometry::apply(session.render_full(&s), &s, session.cal.orientation);
            resize(&img, W, H)
        }
        Area::Pixel(cx, cy, zoom) => {
            let (rw, rh) = (W / zoom, H / zoom);
            let x = ((cx * fw as f32) as usize).saturating_sub(rw / 2).min(fw - rw);
            let y = ((cy * fh as f32) as usize).saturating_sub(rh / 2).min(fh - rh);
            // Crops do not compute the SPP base themselves (see Session::render_spp);
            // ensure it beforehand, otherwise our own pipeline would be used
            if Session::uses_spp(s) {
                session.ensure_spp(s).expect("SPP-Basis");
            }
            let img = session.render_region(s, [x, y, rw, rh]);
            enlarge(&img, zoom)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let src = args.first().cloned().expect("specify the folder with the test images");
    let dst = args.get(1).cloned().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../app/assets/help").to_string());
    let only: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();
    std::fs::create_dir_all(&dst).expect("Zielordner anlegen");
    let mut list: Vec<Example> = examples().into_iter().filter(|e| only.is_empty() || only.contains(&e.name)).collect();
    // group by file: each file is opened only once, the images computed one after another
    list.sort_by_key(|e| e.file);
    let mut i = 0;
    while i < list.len() {
        let file = list[i].file;
        let path = format!("{src}/{file}.X3F");
        let t = std::time::Instant::now();
        let mut session = Session::open(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        while i < list.len() && list[i].file == file {
            let e = &list[i];
            let mut before = Settings::default();
            (e.base)(&mut before);
            let mut after = before.clone();
            (e.after)(&mut after);
            for (suffix, s) in [("before", &before), ("after", &after)] {
                let img = render(&mut session, s, e.area);
                let out = std::path::PathBuf::from(&dst).join(format!("{}_{suffix}.jpg", e.name));
                // without embedded ICC profile (saves ≈ 3 KB per image; the app shows sRGB)
                let enc = jpeg_encoder::Encoder::new_file(&out, QUALITY).expect("JPEG anlegen");
                enc.encode(&img.to_rgb8(), img.width as u16, img.height as u16, jpeg_encoder::ColorType::Rgb).expect("JPEG schreiben");
            }
            eprintln!("{:<16} {file}  {:.1?}", e.name, t.elapsed());
            i += 1;
        }
    }
}
