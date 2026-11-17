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
