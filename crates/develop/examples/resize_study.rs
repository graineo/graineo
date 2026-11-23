//! Measurement tool for the export resampling filters (`develop::resize::ResizeFilter`).
//!
//! `cargo run --release -p develop --example resize_study -- synthetic OUT_DIR`
//!   slanted-edge study (10–90 % rise, overshoot, MTF50, MTF at Nyquist) and a zone plate mosaic.
//! `cargo run --release -p develop --example resize_study -- photo IMAGE.jpg OUT_DIR [factor …]`
//!   100 % crops of the same detailed region for every filter (mosaic) and a sharpness figure (mean |Laplacian|).

use std::path::Path;

use develop::resize::{resize_cpu, ResizeFilter};
use develop::Rgb16;

const ALL: [(ResizeFilter, &str); 4] = [(ResizeFilter::Sharp, "Sharp"), (ResizeFilter::Balanced, "Balanced"), (ResizeFilter::Soft, "Soft"), (ResizeFilter::Smooth, "Smooth")];

fn luma(img: &Rgb16, x: usize, y: usize) -> f32 {
    let p = &img.data[(y * img.width + x) * 3..][..3];
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 65535.0
}

fn save_png(img: &Rgb16, path: &Path) {
    let buf: Vec<u8> = img.data.iter().map(|&v| (v >> 8) as u8).collect();
    image::save_buffer(path, &buf, img.width as u32, img.height as u32, image::ColorType::Rgb8).unwrap();
}

/// Side by side mosaic of equally sized images (2 columns).
fn mosaic(imgs: &[Rgb16]) -> Rgb16 {
    let (w, h) = (imgs[0].width, imgs[0].height);
    let cols = 2;
    let rows = imgs.len().div_ceil(cols);
    let (gw, gh) = (cols * w + (cols - 1) * 4, rows * h + (rows - 1) * 4);
    let mut data = vec![0u16; gw * gh * 3];
    for (k, im) in imgs.iter().enumerate() {
        let (ox, oy) = ((k % cols) * (w + 4), (k / cols) * (h + 4));
        for y in 0..h {
            data[((oy + y) * gw + ox) * 3..][..w * 3].copy_from_slice(&im.data[y * w * 3..][..w * 3]);
        }
    }
    Rgb16 { width: gw, height: gh, data }
}

fn crop(img: &Rgb16, x0: usize, y0: usize, w: usize, h: usize) -> Rgb16 {
    let mut data = Vec::with_capacity(w * h * 3);
    for y in y0..y0 + h {
        data.extend_from_slice(&img.data[(y * img.width + x0) * 3..][..w * 3]);
    }
    Rgb16 { width: w, height: h, data }
}

fn mean_laplacian(img: &Rgb16) -> f32 {
    let mut s = 0.0f64;
    for y in 1..img.height - 1 {
        for x in 1..img.width - 1 {
            let l = 4.0 * luma(img, x, y) - luma(img, x - 1, y) - luma(img, x + 1, y) - luma(img, x, y - 1) - luma(img, x, y + 1);
            s += l.abs() as f64;
        }
    }
    (s / ((img.width - 2) * (img.height - 2)) as f64) as f32
}

fn synthetic(out: &Path) {
    // slanted edge 5 degrees, hard (aliased) 0.15 / 0.85 step, reduced by 4
    let (n, f) = (1600usize, 4usize);
    let ang = 5.0f32.to_radians();
    let (lo, hi) = (0.15f32, 0.85f32);
    let mut data = vec![0u16; n * n * 3];
    for y in 0..n {
        for x in 0..n {
            let d = (x as f32 + 0.5 - n as f32 / 2.0) * ang.cos() + (y as f32 + 0.5 - n as f32 / 2.0) * ang.sin();
            let v = (if d > 0.0 { hi } else { lo } * 65535.0) as u16;
            data[(y * n + x) * 3..][..3].fill(v);
        }
    }
    let src = Rgb16 { width: n, height: n, data };
    println!("slanted edge {n}x{n} -> {}x{} (1/{f}), levels {lo}/{hi}", n / f, n / f);
    println!("{:9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8}", "filter", "rise10-90", "overshoot", "undershoot", "MTF50", "MTF@0.5", "@0.25", "@0.4", "@0.6");
    for (flt, name) in ALL {
        let o = resize_cpu(&src, n / f, n / f, flt);
        let m = n / f;
        // ESF: bin luma by signed distance to the edge (output pixels), bins of 0.1 px
        let (bw, half) = (0.1f32, 12.0f32);
        let nb = (2.0 * half / bw) as usize;
        let (mut sum, mut cnt) = (vec![0f64; nb], vec![0u32; nb]);
        for y in 0..m {
            for x in 0..m {
                let d = (x as f32 + 0.5 - m as f32 / 2.0) * ang.cos() + (y as f32 + 0.5 - m as f32 / 2.0) * ang.sin();
                if d.abs() < half {
                    let b = ((d + half) / bw) as usize;
                    sum[b] += luma(&o, x, y) as f64;
                    cnt[b] += 1;
                }
            }
        }
        let esf: Vec<f32> = (0..nb).map(|b| if cnt[b] > 0 { (sum[b] / cnt[b] as f64) as f32 } else { f32::NAN }).collect();
        let esf: Vec<f32> = esf.iter().enumerate().map(|(i, &v)| if v.is_nan() { esf[..i].iter().rev().find(|x| !x.is_nan()).copied().unwrap_or(lo) } else { v }).collect();
        let over = esf.iter().cloned().fold(f32::MIN, f32::max) - hi;
        let under = lo - esf.iter().cloned().fold(f32::MAX, f32::min);
        let at = |lvl: f32| esf.iter().position(|&v| v >= lo + lvl * (hi - lo)).unwrap() as f32 * bw;
        let rise = at(0.9) - at(0.1);
        // LSF = derivative, Hann window, DFT magnitude
        let lsf: Vec<f32> = esf.windows(2).map(|w| w[1] - w[0]).collect();
        let l = lsf.len();
        let mtf = |fq: f32| {
            let (mut re, mut im) = (0f64, 0f64);
            for (i, &v) in lsf.iter().enumerate() {
                let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (l - 1) as f32).cos();
                let ph = -2.0 * std::f64::consts::PI * fq as f64 * i as f64 * bw as f64;
                re += (v * w) as f64 * ph.cos();
                im += (v * w) as f64 * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        let dc = mtf(0.0);
        let mut mtf50 = 0.0;
        let mut fq = 0.0;
        while fq < 1.0 {
            if mtf(fq) / dc < 0.5 {
                mtf50 = fq;
                break;
            }
            fq += 0.005;
        }
        println!("{name:9} {rise:9.2} {:8.1}% {:8.1}% {mtf50:9.3} {:9.3} {:8.3} {:8.3} {:8.3}", over / (hi - lo) * 100.0, under / (hi - lo) * 100.0, mtf(0.5) / dc, mtf(0.25) / dc, mtf(0.4) / dc, mtf(0.6) / dc);
    }
    // zone plate 2048 -> 512 (hard sampling of a chirp: aliasing at the source)
    let n = 2048usize;
    let mut data = vec![0u16; n * n * 3];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 - n as f32 / 2.0, y as f32 - n as f32 / 2.0);
            let r2 = dx * dx + dy * dy;
            let v = 0.5 + 0.5 * (std::f32::consts::PI * r2 / (n as f32 * 0.55)).cos();
            data[(y * n + x) * 3..][..3].fill((v * 65535.0) as u16);
        }
    }
    let zp = Rgb16 { width: n, height: n, data };
    let outs: Vec<Rgb16> = ALL.iter().map(|(f, _)| crop(&resize_cpu(&zp, 512, 512, *f), 0, 0, 512, 512)).collect();
    save_png(&mosaic(&outs), &out.join("zoneplate-4x-Sharp-Balanced-Soft-Smooth.png"));
}

fn photo(file: &str, out: &Path, factors: &[usize]) {
    let im = image::open(file).unwrap().to_rgb8();
    let src = Rgb16 { width: im.width() as usize, height: im.height() as usize, data: im.as_raw().iter().map(|&v| v as u16 * 257).collect() };
    let stem = Path::new(file).file_stem().unwrap().to_string_lossy().to_string();
    for &f in factors {
        let (nw, nh) = (src.width / f, src.height / f);
        let outs: Vec<Rgb16> = ALL.iter().map(|(fl, _)| resize_cpu(&src, nw, nh, *fl)).collect();
        // most detailed 320 x 240 window of the Sharp result (same place for all filters)
        let (cw, ch) = (320.min(nw), 240.min(nh));
        let mut best = (0.0f32, 0usize, 0usize);
        for y0 in (0..nh - ch).step_by(40) {
            for x0 in (0..nw - cw).step_by(40) {
                let v = mean_laplacian(&crop(&outs[0], x0, y0, cw, ch));
                if v > best.0 {
                    best = (v, x0, y0);
                }
            }
        }
        let crops: Vec<Rgb16> = outs.iter().map(|o| crop(o, best.1, best.2, cw, ch)).collect();
        println!("{stem} 1/{f}: {nw}x{nh}, crop at {},{}", best.1, best.2);
        for ((_, name), o) in ALL.iter().zip(&outs) {
            println!("   {name:9} mean|Laplacian| whole image {:.5}", mean_laplacian(o));
        }
        save_png(&mosaic(&crops), &out.join(format!("{stem}-1-{f}-Sharp-Balanced-Soft-Smooth.png")));
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("synthetic") => synthetic(Path::new(&a[2])),
        Some("photo") => {
            let factors: Vec<usize> = a[4..].iter().filter_map(|s| s.parse().ok()).collect();
            photo(&a[2], Path::new(&a[3]), if factors.is_empty() { &[2, 4] } else { &factors });
        }
        _ => eprintln!("usage: resize_study synthetic OUT | photo IMAGE OUT [factor …]"),
    }
}
