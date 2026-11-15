//! Checks that the detail crop (RenderRegion + mapping via `Frame`, like the display at
//! 100 %/loupe) and the export (`geometry::apply`) show the same image location with perspective.
//!
//! `cargo run --release -p develop --example persp_detail_check -- IMAGE.X3F OUTPUT_FOLDER [pv ph angle]`

use develop::geometry::{self, Frame};
use develop::{export, Rgb16, Session, Settings};

fn bilinear(img: &Rgb16, x: f32, y: f32, c: usize) -> f32 {
    let (px, py) = ((x - 0.5).clamp(0.0, (img.width - 1) as f32), (y - 0.5).clamp(0.0, (img.height - 1) as f32));
    let (x0, y0) = (px.floor() as usize, py.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(img.width - 1), (y0 + 1).min(img.height - 1));
    let (tx, ty) = (px - x0 as f32, py - y0 as f32);
    let at = |xx: usize, yy: usize| img.data[(yy * img.width + xx) * 3 + c] as f32;
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
    let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
    top + (bot - top) * ty
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(out).unwrap();
    let num = |i: usize, d: f32| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(d);
    let mut session = Session::open(&args[1]).expect("open file");
    let [fw, fh] = session.size();
    let cam = session.cal.orientation;
    let mut s = Settings { vignette: -0.5, ..Settings::default() };
    geometry::set_transform(&mut s, num(5, -0.08), num(3, -0.375), num(4, 0.0), fw, fh, cam);
    let t = std::time::Instant::now();
    let full = session.render_full_own(&s);
    let t_dev = t.elapsed();
    let t = std::time::Instant::now();
    let exp = geometry::apply(full, &s, cam);
    println!("Export: developed {:.2?}, perspective+Lanczos {:.2?} → {}×{}", t_dev, t.elapsed(), exp.width, exp.height);
    let frame = Frame::from_settings(&s, fw, fh, cam);
    let (ow, oh) = (exp.width as f32, exp.height as f32);
    const P: usize = 64;
    for (k, u) in [[0.2, 0.2], [0.8, 0.3], [0.5, 0.5], [0.15, 0.85], [0.9, 0.9]].iter().enumerate() {
        let (cx, cy) = ((u[0] * ow) as usize, (u[1] * oh) as usize);
        let (x0, y0) = (cx.saturating_sub(P / 2).min(exp.width - P), cy.saturating_sub(P / 2).min(exp.height - P));
        let srcs: Vec<[f32; 2]> = (0..P * P).map(|i| frame.to_src([(x0 + i % P) as f32 / ow + 0.5 / ow, (y0 + i / P) as f32 / oh + 0.5 / oh])).collect();
        let (mut a0, mut a1, mut b0, mut b1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &srcs {
            a0 = a0.min(p[0]);
            a1 = a1.max(p[0]);
            b0 = b0.min(p[1]);
            b1 = b1.max(p[1]);
        }
        let rx = (a0 - 8.0).max(0.0) as usize;
        let ry = (b0 - 8.0).max(0.0) as usize;
        let region = [rx, ry, ((a1 + 8.0) as usize).min(fw) - rx, ((b1 + 8.0) as usize).min(fh) - ry];
        let det = session.render_region(&s, region);
        let mut diff = [0.0f64; 3];
        let mut shifted = 0.0f64;
        let mut patch = Rgb16 { width: P * 2 + 4, height: P, data: vec![0; (P * 2 + 4) * P * 3] };
        for (i, p) in srcs.iter().enumerate() {
            let (x, y) = (i % P, i / P);
            for c in 0..3 {
                let e = exp.data[((y0 + y) * exp.width + x0 + x) * 3 + c] as f32;
                let d = bilinear(&det, p[0] - rx as f32, p[1] - ry as f32, c);
                let d1 = bilinear(&det, p[0] - rx as f32 + 1.0, p[1] - ry as f32, c);
                diff[c] += ((e - d).abs() / 65535.0) as f64;
                shifted += ((e - d1).abs() / 65535.0) as f64 / 3.0;
                patch.data[(y * patch.width + x) * 3 + c] = e as u16;
                patch.data[(y * patch.width + x + P + 4) * 3 + c] = d.round() as u16;
            }
        }
        let n = (P * P) as f64;
        println!(
            "Spot {k} u={u:?}: |export − detail| = {:.4} {:.4} {:.4} (shifted by 1 px: {:.4})",
            diff[0] / n,
            diff[1] / n,
            diff[2] / n,
            shifted / n
        );
        // magnified 4× (nearest neighbour) for viewing: left export, right detail
        let z = 4;
        let mut big = Rgb16 { width: patch.width * z, height: patch.height * z, data: vec![0; patch.width * z * patch.height * z * 3] };
        for y in 0..big.height {
            for x in 0..big.width {
                for c in 0..3 {
                    big.data[(y * big.width + x) * 3 + c] = patch.data[((y / z) * patch.width + x / z) * 3 + c];
                }
            }
        }
        export::write_jpeg(&big, &out.join(format!("detail_vs_export_{k}.jpg")), 92).unwrap();
    }
    // Full image downscaled
    let f = exp.width.max(exp.height).div_ceil(1600);
    let small = Rgb16 {
        width: exp.width / f,
        height: exp.height / f,
        data: (0..(exp.height / f) * (exp.width / f) * 3).map(|i| {
            let (p, c) = (i / 3, i % 3);
            let (x, y) = (p % (exp.width / f), p / (exp.width / f));
            exp.data[((y * f) * exp.width + x * f) * 3 + c]
        }).collect(),
    };
    export::write_jpeg(&small, &out.join("export_gesamt.jpg"), 90).unwrap();
}
