//! `graineo-dev ca <X3F> <output_folder> [out|profile[=strength]|manuell=red,blue]`:
//! compute the SPP base without and with chromatic aberration correction (one after the other),
//! measure the lateral color shift red→green and blue→green per radius ring (also in the
//! camera JPEG if `<stem>.JPG` lies next to it) and store corner crops before/after
//! (3× magnified) as JPEG. Times of the base with/without correction.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};

use develop::lens::{CaKey, CaMode};

struct Plane<'a> {
    w: usize,
    h: usize,
    data: &'a [f32],
    stride: usize,
    off: usize,
}

impl Plane<'_> {
    fn at(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w * self.stride + x * self.stride + self.off]
    }
    fn bilinear(&self, x: f64, y: f64) -> f64 {
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = (x - x0, y - y0);
        let (x0, y0) = (x0 as usize, y0 as usize);
        let p = |x: usize, y: usize| f64::from(self.at(x.min(self.w - 1), y.min(self.h - 1)));
        (p(x0, y0) * (1.0 - tx) + p(x0 + 1, y0) * tx) * (1.0 - ty) + (p(x0, y0 + 1) * (1.0 - tx) + p(x0 + 1, y0 + 1) * tx) * ty
    }
}

/// Radial shift (pixels, + = outward) of `c` relative to `g` in the field around (cx, cy).
fn radial_shift(g: &Plane, c: &Plane, cx: usize, cy: usize, half: usize) -> Option<(f64, f64)> {
    let (w, h) = (g.w as f64, g.h as f64);
    let (dx, dy) = (cx as f64 - (w - 1.0) / 2.0, cy as f64 - (h - 1.0) / 2.0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    // edge energy in the radial direction (only there is the shift measurable)
    let mut energy = 0.0;
    let (mut sg, mut sc) = (0.0, 0.0);
    for y in cy - half..cy + half {
        for x in cx - half..cx + half {
            let gx = f64::from(g.at(x + 1, y)) - f64::from(g.at(x - 1, y));
            let gy = f64::from(g.at(x, y + 1)) - f64::from(g.at(x, y - 1));
            energy += (gx * ux + gy * uy).powi(2);
            sg += f64::from(g.at(x, y));
            sc += f64::from(c.at(x, y));
        }
    }
    let n = (4 * half * half) as f64;
    let (mg, mc) = (sg / n, sc / n);
    if mg <= 1.0 || mc <= 1.0 || energy / n < (0.02 * mg).powi(2) {
        return None;
    }
    let err = |d: f64| -> f64 {
        let mut e = 0.0;
        for y in cy - half..cy + half {
            for x in cx - half..cx + half {
                let v = c.bilinear(x as f64 + d * ux, y as f64 + d * uy) / mc - f64::from(g.at(x, y)) / mg;
                e += v * v;
            }
        }
        e
    };
    let steps: Vec<f64> = (-40..=40).map(|i| i as f64 * 0.05).collect();
    let errs: Vec<f64> = steps.iter().map(|&d| err(d)).collect();
    let (i, _) = errs.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?;
    if i == 0 || i + 1 == errs.len() {
        return None;
    }
    let (a, b, cc) = (errs[i - 1], errs[i], errs[i + 1]);
    let den = a - 2.0 * b + cc;
    let frac = if den > 0.0 { 0.5 * (a - cc) / den } else { 0.0 };
    Some((steps[i] + frac * 0.05, len / ((w * w + h * h).sqrt() / 2.0)))
}

/// Mean shift per ring [0–0.4), [0.4–0.7), [0.7–1] of half the diagonal radius.
fn measure(w: usize, h: usize, data: &[f32], stride: usize) -> [[(f64, usize); 3]; 2] {
    let g = Plane { w, h, data, stride, off: 1 };
    let r = Plane { w, h, data, stride, off: 0 };
    let b = Plane { w, h, data, stride, off: 2 };
    let half = 40;
    let mut acc = [[(0.0, 0usize); 3]; 2];
    let (nx, ny) = (16, 11);
    for j in 0..ny {
        for i in 0..nx {
            let cx = half + 4 + i * (w - 2 * half - 8) / (nx - 1);
            let cy = half + 4 + j * (h - 2 * half - 8) / (ny - 1);
            for (k, c) in [&r, &b].into_iter().enumerate() {
                if let Some((d, rad)) = radial_shift(&g, c, cx, cy, half) {
                    let ring = if rad < 0.4 { 0 } else if rad < 0.7 { 1 } else { 2 };
                    acc[k][ring].0 += d;
                    acc[k][ring].1 += 1;
                }
            }
        }
    }
    acc
}

fn print(label: &str, m: &[[(f64, usize); 3]; 2]) {
    let f = |(s, n): (f64, usize)| if n == 0 { "     –     ".to_string() } else { format!("{:+.3} ({n:2})", s / n as f64) };
    println!(
        "{label:<22} red→green: center {}  middle {}  edge {}   blue→green: center {}  middle {}  edge {}",
        f(m[0][0]), f(m[0][1]), f(m[0][2]), f(m[1][0]), f(m[1][1]), f(m[1][2])
    );
}

fn crop_zoom(img: &develop::Rgb16, x0: usize, y0: usize, cw: usize, ch: usize, z: usize) -> develop::Rgb16 {
    let mut data = Vec::with_capacity(cw * ch * z * z * 3);
    for y in 0..ch * z {
        for x in 0..cw * z {
            let i = ((y0 + y / z) * img.width + x0 + x / z) * 3;
            data.extend_from_slice(&img.data[i..i + 3]);
        }
    }
    develop::Rgb16 { width: cw * z, height: ch * z, data }
}

pub fn run(file: &str, out: &str, spec: Option<&str>) -> Result<()> {
    let path = Path::new(file);
    std::fs::create_dir_all(out)?;
    let ca = match spec.unwrap_or("profile") {
        "off" => CaKey::default(),
        s if s.starts_with("profile") => {
            let v: f32 = s.split_once('=').map_or(Ok(1.0), |(_, v)| v.parse())?;
            CaKey { mode: CaMode::Profile, profile: (v * 10.0).round() as i8, red: 0, blue: 0 }
        }
        s if s.starts_with("manuell=") => {
            let v: Vec<f32> = s[8..].split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
            anyhow::ensure!(v.len() == 2, "manuell=rot,blau");
            CaKey { mode: CaMode::Manual, profile: 0, red: (v[0] * 10.0).round() as i8, blue: (v[1] * 10.0).round() as i8 }
        }
        s => anyhow::bail!("unknown correction {s}"),
    };
    if let Some(c) = develop::lens::correction(path, &ca) {
        println!("correction: {c:?}");
    } else if !ca.is_off() {
        println!("no lens profile found → no correction");
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let key_off = develop::SppKey { mode: None, nr: (2, 2), ca: CaKey::default(), ..Default::default() };
    let key_on = develop::SppKey { ca, ..key_off.clone() };
    let t = Instant::now();
    let off = develop::spp::working_key(path, &key_off, false, &cancel).map_err(anyhow::Error::msg)?;
    let t_off = t.elapsed();
    let t = Instant::now();
    let on = develop::spp::working_key(path, &key_on, false, &cancel).map_err(anyhow::Error::msg)?;
    let t_on = t.elapsed();
    println!("SPP base (CPU) without correction {t_off:.2?}, with {t_on:.2?}");
    let diff = off.data.iter().zip(&on.data).filter(|(a, b)| a != b).count();
    println!("changed values: {diff} of {}", off.data.len());
    print("without correction", &measure(off.width, off.height, &off.data, 3));
    print("with correction", &measure(on.width, on.height, &on.data, 3));
    // shift of the corrected vs. the uncorrected channel (effect of the correction)
    let mut pair = Vec::with_capacity(off.data.len());
    for (a, b) in off.data.chunks_exact(3).zip(on.data.chunks_exact(3)) {
        pair.extend_from_slice(&[b[0], a[0], b[2], a[2]]);
    }
    let effective = {
        let (w, h) = (off.width, off.height);
        let g_r = Plane { w, h, data: &pair, stride: 4, off: 1 };
        let c_r = Plane { w, h, data: &pair, stride: 4, off: 0 };
        let g_b = Plane { w, h, data: &pair, stride: 4, off: 3 };
        let c_b = Plane { w, h, data: &pair, stride: 4, off: 2 };
        let mut acc = [[(0.0, 0usize); 3]; 2];
        let half = 40;
        for j in 0..11 {
            for i in 0..16 {
                let cx = half + 4 + i * (w - 2 * half - 8) / 15;
                let cy = half + 4 + j * (h - 2 * half - 8) / 10;
                for (k, (g, c)) in [(&g_r, &c_r), (&g_b, &c_b)].into_iter().enumerate() {
                    if let Some((d, rad)) = radial_shift(g, c, cx, cy, half) {
                        let ring = if rad < 0.4 { 0 } else if rad < 0.7 { 1 } else { 2 };
                        acc[k][ring].0 += d;
                        acc[k][ring].1 += 1;
                    }
                }
            }
        }
        acc
    };
    print("effect (new→old)", &effective);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    for jpg in [path.with_extension("JPG"), path.with_extension("jpg")] {
        if let Ok(img) = image::open(&jpg) {
            let rgb = img.to_rgb8();
            let (w, h) = (rgb.width() as usize, rgb.height() as usize);
            let lin: Vec<f32> = rgb.as_raw().iter().map(|&v| (f32::from(v) / 255.0).powf(2.2) * 1000.0).collect();
            print("camera JPEG", &measure(w, h, &lin, 3));
            break;
        }
    }
    // crops: corners (top left, bottom right) and top edge center, 3×
    let session = develop::Session::open(path)?;
    let s = develop::Settings::default();
    let m = session.cal.mode(None).cam_to_srgb;
    let a = develop::render_counts(&off, &session.cal, &s, &m, None);
    let b = develop::render_counts(&on, &session.cal, &s, &m, None);
    let (cw, ch) = (240, 160);
    let spots = [("corner_tl", 40, 40), ("corner_br", a.width - cw - 40, a.height - ch - 40), ("border_top", a.width / 2 - cw / 2, 30)];
    for (name, x, y) in spots {
        for (tag, img) in [("vorher", &a), ("nachher", &b)] {
            let p = Path::new(out).join(format!("{stem}_{name}_{tag}.jpg"));
            develop::export::write_jpeg(&crop_zoom(img, x, y, cw, ch, 3), &p, 92).with_context(|| p.display().to_string())?;
        }
    }
    println!("crops in {out}");
    Ok(())
}
