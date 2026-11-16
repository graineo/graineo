//! Test helper: compare two models on the same faces (time, vector length, similarities).
//!  vergleich MODEL IMAGE
#[cfg(target_os = "macos")]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let m = faces::sface::Sface::load(std::path::Path::new(&a[1])).unwrap();
    let img = image::open(&a[2]).unwrap().to_rgb8();
    let g = faces::vision::detect(&img).unwrap();
    let mut v = Vec::new();
    for (i, f) in g.iter().enumerate() {
        let p = f.points.unwrap_or_else(|| faces::align::points_from_box(f.x, f.y, f.b, f.h));
        let aligned = faces::align::crop_aligned(&img, &p);
        let t = std::time::Instant::now();
        let e = m.embedding(&aligned).unwrap();
        println!("#{i}: {} numbers, {:?}", e.len(), t.elapsed());
        v.push(e);
    }
    for i in 0..v.len() { for j in i + 1..v.len() { println!("  {i}–{j}: {:.3}", faces::sface::similarity(&v[i], &v[j])); } }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This helper needs Apple Vision and is only available on macOS.");
}
