//! Checks "auto upright" on real images: find lines in the preview (with
//! timing), estimate angle/perspective per mode and write before (with the lines found) and
//! after as JPEG.
//!
//! `cargo run --release -p develop --example upright_check -- OUTPUT_FOLDER IMAGE.X3F …`

use develop::geometry::{self, Frame};
use develop::session::Preview;
use develop::upright::{self, Mode};
use develop::{export, Rgb16, Session, Settings};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(out).unwrap();
    for path in &args[2..] {
        let session = Session::open(path).expect("open file");
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let [fw, fh] = session.size();
        let cam = session.cal.orientation;
        let (small, factor) = session.preview_linear(2400);
        let s0 = Settings::default();
        let preview = Preview::developed(small, factor, &session.cal, &s0);
        let base = Frame::full_settings(&s0, fw, fh, cam);
        let t = std::time::Instant::now();
        let segs = upright::detect(&preview.lin, &base);
        let t_detect = t.elapsed();
        let mut line = format!("{name:<28} {:>4} lines, search {:>5.1} ms", segs.len(), t_detect.as_secs_f64() * 1e3);
        // Before, with lines
        let img = preview.render(&session.cal, &s0);
        let mut before = geometry::apply(img, &s0, cam);
        let k = before.width as f32 / base.oriented().0;
        for sg in &segs {
            draw(&mut before, [sg.p0[0] * k, sg.p0[1] * k], [sg.p1[0] * k, sg.p1[1] * k]);
        }
        export::write_jpeg(&before, &out.join(format!("{name}_0_linien.jpg")), 88).unwrap();
        for (mode, tag) in [(Mode::Level, "1_waagerecht"), (Mode::Vertical, "2_senkrecht"), (Mode::Auto, "3_auto")] {
            let t = std::time::Instant::now();
            let r = upright::solve(&segs, &base, mode);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            match r {
                Ok(u) => {
                    line += &format!(" | {tag}: {:+.2}° v {:+.3} h {:+.3} ({} L, {ms:.1} ms)", u.angle, u.pv, u.ph, u.lines);
                    let mut s = s0.clone();
                    geometry::set_transform(&mut s, u.angle, u.pv, u.ph, fw, fh, cam);
                    let img = geometry::apply(preview.render(&session.cal, &s), &s, cam);
                    export::write_jpeg(&img, &out.join(format!("{name}_{tag}.jpg")), 88).unwrap();
                }
                Err(e) => line += &format!(" | {tag}: – ({e}, {ms:.1} ms)"),
            }
        }
        println!("{line}");
    }
}

fn draw(img: &mut Rgb16, a: [f32; 2], b: [f32; 2]) {
    let n = ((b[0] - a[0]).abs().max((b[1] - a[1]).abs()) as usize).max(1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let (x, y) = ((a[0] + (b[0] - a[0]) * t) as isize, (a[1] + (b[1] - a[1]) * t) as isize);
        for (dx, dy) in [(0, 0), (1, 0), (0, 1)] {
            let (x, y) = (x + dx, y + dy);
            if x >= 0 && y >= 0 && (x as usize) < img.width && (y as usize) < img.height {
                let i = (y as usize * img.width + x as usize) * 3;
                img.data[i..i + 3].copy_from_slice(&[65535, 0, 0]);
            }
        }
    }
}
