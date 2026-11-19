use masks::*;
#[test]
#[ignore = "Requires a copy of your own photo in a scratch folder"]
fn photographic_noise() {
    let path = std::path::PathBuf::from(std::env::var("MASKS_PHOTO").unwrap())
        .canonicalize()
        .unwrap();
    assert!(path.starts_with("/private/tmp/graineo-masks"));
    let photo = image::open(path).unwrap().to_rgb32f();
    let (w, h) = (photo.width() as usize, photo.height() as usize);
    let clean: Vec<f32> = photo
        .as_raw()
        .iter()
        .map(|v| {
            if *v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    let mut seed = 12345u32;
    let noisy: Vec<f32> = clean
        .iter()
        .map(|v| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            v + ((seed >> 8) as f32 / 16777215.0 - 0.5) * 0.04
        })
        .collect();
    let d = Document {
        version: 2,
        layers: vec![Layer {
            mask: Mask::Luminance {
                low: -1.0,
                high: 10.0,
                feather: 0.0,
            },
            adjustments: Adjustments {
                noise_reduction: 0.75,
                ..Default::default()
            },
            name: None,
            enabled: true,
            amount: 1.0,
            refine: 0.0,
        }],
    };
    let out = apply(Image::new(w, h, &noisy).unwrap(), &d).unwrap();
    let rmse = |data: &[f32]| {
        (data
            .iter()
            .zip(&clean)
            .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
            .sum::<f64>()
            / data.len() as f64)
            .sqrt()
    };
    let before = rmse(&noisy);
    let after = rmse(&out);
    println!("photo {w}×{h}: RMSE {before:.8} -> {after:.8}");
    assert!(after < before * 0.9);
    for (name, data) in [("photo-noisy.png", &noisy), ("photo-filtered.png", &out)] {
        let p = std::path::Path::new("/private/tmp/graineo-masks").join(format!("{w}-{h}-{name}"));
        assert!(!p.exists());
        let rgb = data
            .iter()
            .map(|v| {
                let v = v.max(0.0);
                let s = if v <= 0.0031308 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                };
                (s.clamp(0.0, 1.0) * 255.0).round() as u8
            })
            .collect();
        image::RgbImage::from_raw(w as u32, h as u32, rgb)
            .unwrap()
            .save(p)
            .unwrap();
    }
}

/// Auto mask on a photo: two strokes over tree branches against the sky, without and with auto mask (reference =
/// colour under the start of each stroke, 3 × 3 mean). Writes `<photo>-automask.png` next to the copy (left: without,
/// right: with; red = weight) and prints how much weight lands on the dark branches.
#[test]
#[ignore = "Requires a copy of your own photo in a scratch folder"]
fn auto_mask_on_a_photo() {
    let path = std::path::PathBuf::from(std::env::var("MASKS_PHOTO").unwrap()).canonicalize().unwrap();
    assert!(path.starts_with("/private/tmp/"));
    let photo = image::open(&path).unwrap().to_rgb32f();
    let (w, h) = (photo.width() as usize, photo.height() as usize);
    let lin: Vec<f32> = photo.as_raw().iter().map(|v| if *v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }).collect();
    let im = Image::new(w, h, &lin).unwrap();
    let mean = |p: [f32; 2]| {
        let (x, y) = ((p[0] * w as f32) as usize, (p[1] * h as f32) as usize);
        let mut acc = [0.0f32; 3];
        for yy in y - 1..=y + 1 {
            for xx in x - 1..=x + 1 {
                for c in 0..3 {
                    acc[c] += lin[(yy * w + xx) * 3 + c] / 9.0;
                }
            }
        }
        acc
    };
    let tolerance: f32 = std::env::var("MASKS_TOLERANCE").ok().and_then(|v| v.parse().ok()).unwrap_or(0.15);
    let path_at = |y: f32| -> Vec<[f32; 2]> { (0..80).map(|i| { let x = 0.05 + 0.9 * i as f32 / 79.0; [x, y + 0.04 * (x * 9.0).sin()] }).collect() };
    let strokes = [path_at(0.3), path_at(0.55)];
    let mask = |auto: bool| {
        let leaf = |pts: &Vec<[f32; 2]>| Mask::Brush { points: pts.clone(), radius: 0.06, hardness: 0.5, flow: 0.5, density: 1.0, auto: auto.then(|| AutoMask { reference: mean(pts[0]), tolerance }) };
        Mask::Combine { operation: Operation::Add, left: Box::new(leaf(&strokes[0])), right: Box::new(leaf(&strokes[1])) }
    };
    let plain = rasterize(im, &mask(false)).unwrap();
    let t = std::time::Instant::now();
    let edge = rasterize(im, &mask(true)).unwrap();
    println!("{w}×{h}, auto mask {:.1} ms, tolerance {tolerance}", t.elapsed().as_secs_f64() * 1000.0);
    // dark pixels (branches): encoded luma below 0.12
    let dark: Vec<bool> = (0..w * h).map(|i| { let l = 0.2126 * lin[i * 3] + 0.7152 * lin[i * 3 + 1] + 0.0722 * lin[i * 3 + 2]; l < 0.012 }).collect();
    for (name, wt) in [("without", &plain), ("with", &edge)] {
        let (mut on_dark, mut on_sky, mut n_dark, mut n_sky) = (0.0f64, 0.0f64, 0usize, 0usize);
        for i in 0..w * h {
            if plain[i] > 0.5 {
                if dark[i] { on_dark += wt[i] as f64; n_dark += 1 } else { on_sky += wt[i] as f64; n_sky += 1 }
            }
        }
        println!("{name} auto mask: mean weight on branches {:.3} ({n_dark} px), on sky {:.3} ({n_sky} px)", on_dark / n_dark.max(1) as f64, on_sky / n_sky.max(1) as f64);
    }
    let src = image::open(&path).unwrap().to_rgb8();
    let mut out = image::RgbImage::new(2 * w as u32 + 8, h as u32);
    for (k, wt) in [&plain, &edge].into_iter().enumerate() {
        for y in 0..h {
            for x in 0..w {
                let p = src.get_pixel(x as u32, y as u32).0;
                let a = wt[y * w + x].clamp(0.0, 1.0) * 0.6;
                let c = [p[0] as f32 * (1.0 - a) + 230.0 * a, p[1] as f32 * (1.0 - a) + 40.0 * a, p[2] as f32 * (1.0 - a) + 40.0 * a];
                out.put_pixel((k * (w + 8) + x) as u32, y as u32, image::Rgb(c.map(|v| v.round() as u8)));
            }
        }
    }
    let target = path.with_file_name(format!("{}-automask.png", path.file_stem().unwrap().to_string_lossy()));
    out.save(&target).unwrap();
    println!("written {}", target.display());
}
