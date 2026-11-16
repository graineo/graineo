//! Test helper: detect faces in an image, draw boxes/points, save aligned crops,
//! compare feature vectors.  erkennen IMAGE [MODEL] [OUTPUT_DIR]
#[cfg(target_os = "macos")]
use image::{Rgb, RgbImage};

#[cfg(target_os = "macos")]
fn line(b: &mut RgbImage, x0: f32, y0: f32, x1: f32, y1: f32, f: Rgb<u8>) {
    let n = ((x1 - x0).abs().max((y1 - y0).abs()) as usize).max(1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let (x, y) = ((x0 + (x1 - x0) * t) as i64, (y0 + (y1 - y0) * t) as i64);
        for d in 0..2 {
            let (px, py) = ((x + d).clamp(0, b.width() as i64 - 1), (y).clamp(0, b.height() as i64 - 1));
            b.put_pixel(px as u32, py as u32, f);
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut img = image::open(&a[1]).unwrap().to_rgb8();
    let max = 1280.0 / img.width().max(img.height()) as f32;
    if max < 1.0 {
        img = image::imageops::resize(&img, (img.width() as f32 * max) as u32, (img.height() as f32 * max) as u32, image::imageops::FilterType::Triangle);
    }
    let t = std::time::Instant::now();
    let found = faces::vision::detect(&img).unwrap();
    println!("{} faces in {:?} ({}×{})", found.len(), t.elapsed(), img.width(), img.height());
    let model = a.get(2).map(|p| faces::sface::Sface::load(std::path::Path::new(p)).unwrap());
    let out_dir = a.get(3).cloned().unwrap_or_else(|| "/tmp/graineo-gesicht-aus".into());
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut canvas = img.clone();
    let mut vectors = Vec::new();
    for (i, g) in found.iter().enumerate() {
        println!("  #{i}: box {:.0},{:.0} {:.0}×{:.0} confidence {:.2} roll {:?} yaw {:?} points {}", g.x, g.y, g.b, g.h, g.confidence, g.roll, g.yaw, g.points.is_some());
        let (x0, y0, x1, y1) = (g.x, g.y, g.x + g.b, g.y + g.h);
        let f = Rgb([255, 200, 0]);
        line(&mut canvas, x0, y0, x1, y0, f);
        line(&mut canvas, x1, y0, x1, y1, f);
        line(&mut canvas, x1, y1, x0, y1, f);
        line(&mut canvas, x0, y1, x0, y0, f);
        let p = g.points.unwrap_or_else(|| faces::align::points_from_box(g.x, g.y, g.b, g.h));
        for q in p {
            line(&mut canvas, q.0 - 3.0, q.1, q.0 + 3.0, q.1, Rgb([0, 255, 255]));
            line(&mut canvas, q.0, q.1 - 3.0, q.0, q.1 + 3.0, Rgb([0, 255, 255]));
        }
        let crop = faces::align::crop_aligned(&img, &p);
        RgbImage::from_raw(112, 112, crop.clone()).unwrap().save(format!("{out_dir}/gesicht{i}.png")).unwrap();
        if let Some(m) = &model {
            let t = std::time::Instant::now();
            let v = m.embedding(&crop).unwrap();
            println!("     feature in {:?}, length {}, first {:?}", t.elapsed(), v.len(), &v[..4]);
            vectors.push(v);
        }
    }
    for i in 0..vectors.len() {
        for j in i + 1..vectors.len() {
            println!("  similarity #{i}–#{j}: {:.3}", faces::sface::similarity(&vectors[i], &vectors[j]));
        }
    }
    canvas.save(format!("{out_dir}/zeichnung.png")).unwrap();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This helper needs Apple Vision and is only available on macOS.");
}
