//! Opt-in check using only one of your own photographs copied to a scratch folder beforehand.
use retouch::*;
#[test]
#[ignore = "Needs RETOUCH_SAMPLE_IMAGE and GRAINEO_DATA_DIR in a scratch folder"]
fn real_photo_with_injected_dust() {
    let root = std::path::PathBuf::from(std::env::var("GRAINEO_DATA_DIR").expect("scratch folder missing"));
    assert!(root.starts_with("/private/tmp/graineo-retouch"));
    let path = std::path::PathBuf::from(std::env::var("RETOUCH_SAMPLE_IMAGE").unwrap())
        .canonicalize()
        .unwrap();
    assert!(path.starts_with("/private/tmp/graineo-retouch"));
    std::fs::create_dir_all(&root).unwrap();
    let photo = image::open(path).unwrap().to_rgb8();
    let (w, h) = (photo.width() as usize, photo.height() as usize);
    let clean: Vec<f32> = photo
        .as_raw()
        .iter()
        .map(|&s| {
            let s = s as f32 / 255.;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    // Pick the smoothest nonblack patch, measuring every channel; deterministic grid.
    let r = 6usize;
    let mut best = (f64::INFINITY, 0, 0);
    for y in (r + 3..h - r - 3).step_by(8) {
        for x in (r + 3..w - r - 3).step_by(8) {
            let mut sum = [0.; 3];
            let mut square = [0.; 3];
            let mut n = 0.;
            for py in y - r..=y + r {
                for px in x - r..=x + r {
                    for c in 0..3 {
                        let v = clean[(py * w + px) * 3 + c] as f64;
                        sum[c] += v;
                        square[c] += v * v;
                    }
                    n += 1.;
                }
            }
            let mean = sum.iter().sum::<f64>() / (n * 3.);
            let variance = (0..3)
                .map(|c| square[c] / n - (sum[c] / n).powi(2))
                .sum::<f64>();
            if mean > 0.08 && variance < best.0 {
                best = (variance, x, y);
            }
        }
    }
    assert!(best.0.is_finite());
    let (_, x, y) = best;
    let mask = Mask::circle([x as f32, y as f32], r as f32, 0.7).unwrap();
    let mut dirty = clean.clone();
    for py in y - 3..=y + 3 {
        for px in x - 3..=x + 3 {
            if (px as f32 - x as f32).hypot(py as f32 - y as f32) < 3. {
                for c in 0..3 {
                    dirty[(py * w + px) * 3 + c] *= 0.15;
                }
            }
        }
    }
    let img = Image::new(w, h, &dirty).unwrap();
    let start = std::time::Instant::now();
    let source = find_source(
        img,
        &mask,
        Search {
            radius: 64,
            step: 2,
        },
    )
    .unwrap();
    let patch = compute(img, &mask, source.source, Options::default()).unwrap();
    let after = patch.apply(img).unwrap();
    let elapsed = start.elapsed();
    assert!(patch.report.converged);
    let baseline = compute(
        Image::new(w, h, &clean).unwrap(),
        &mask,
        source.source,
        Options::default(),
    )
    .unwrap();
    assert_eq!(patch.rgb(), baseline.rgb()); // Defect inside mask cannot influence the healed solution.
    let error = |data: &[f32]| {
        let mut sum = 0.;
        let mut n = 0.;
        for py in y - r..=y + r {
            for px in x - r..=x + r {
                for c in 0..3 {
                    let i = (py * w + px) * 3 + c;
                    sum += (data[i] as f64 - clean[i] as f64).powi(2);
                    n += 1.;
                }
            }
        }
        (sum / n).sqrt()
    };
    let before_error = error(&dirty);
    let after_error = error(&after);
    assert!(
        after_error < before_error * 0.25,
        "{before_error} -> {after_error}"
    );
    for (name, data) in [
        ("photo-before.png", &dirty),
        ("photo-after.png", &after),
        ("photo-reference.png", &clean),
    ] {
        let encoded: Vec<u8> = data
            .iter()
            .map(|&v| {
                let s = if v <= 0.0031308 {
                    12.92 * v
                } else {
                    1.055 * v.max(0.).powf(1. / 2.4) - 0.055
                };
                (s.clamp(0., 1.) * 255.).round() as u8
            })
            .collect();
        let path = root.join(name);
        assert!(!path.exists(), "own demo output already exists");
        image::RgbImage::from_raw(w as u32, h as u32, encoded)
            .unwrap()
            .save(path)
            .unwrap();
    }
    println!("photo {w}×{h}, mask {x},{y},{r}, source {:?}, border MSE {:.8}, RMSE before {before_error:.8} after {after_error:.8}, {:?}, {elapsed:.2?}",source.source,source.mean_squared_error,patch.report);
}
