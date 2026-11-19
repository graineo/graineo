use masks::*;
fn img(w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let v = (i % w) as f32 / w as f32;
            [v, 0.2, 0.5]
        })
        .collect()
}
fn full() -> Mask {
    Mask::Luminance {
        low: -10.,
        high: 10.,
        feather: 0.,
    }
}
fn doc(mask: Mask, a: Adjustments) -> Document {
    Document {
        version: 1,
        layers: vec![Layer {
            mask,
            adjustments: a,
            name: None,
            enabled: true,
            amount: 1.0,
            refine: 0.0,
        }],
    }
}
fn eval(mask: &Mask, p: [f32; 2]) -> f32 {
    // 101 pixels samples exact normalized center for p=.5, other p rounded to pixel centers.
    let data = vec![0.5; 101 * 101 * 3];
    let out = rasterize(Image::new(101, 101, &data).unwrap(), mask).unwrap();
    out[((p[1] * 100.).round() as usize) * 101 + (p[0] * 100.).round() as usize]
}
#[test]
fn gradients_geometry_and_composition() {
    let linear = Mask::Linear {
        start: [0., 0.],
        end: [1., 0.],
    };
    assert!((eval(&linear, [0.5, 0.5]) - 0.5).abs() < 1e-6);
    let ellipse = Mask::Radial {
        center: [0.5, 0.5],
        radii: [0.3, 0.1],
        angle: 0.,
        feather: 0.5,
    };
    assert_eq!(eval(&ellipse, [0.5, 0.5]), 1.);
    assert_eq!(eval(&ellipse, [0.5, 0.8]), 0.);
    let rotated = Mask::Radial {
        center: [0.5, 0.5],
        radii: [0.3, 0.1],
        angle: std::f32::consts::FRAC_PI_2,
        feather: 0.5,
    };
    assert!(eval(&rotated, [0.5, 0.7]) > eval(&ellipse, [0.5, 0.7]));
    for operation in [Operation::Add, Operation::Subtract, Operation::Intersect] {
        let mask = Mask::Combine {
            operation,
            left: Box::new(linear.clone()),
            right: Box::new(linear.clone()),
        };
        let expected = match operation {
            Operation::Add => 1.,
            Operation::Subtract => 0.,
            Operation::Intersect => 0.5,
        };
        assert!((eval(&mask, [0.5, 0.5]) - expected).abs() < 1e-6);
    }
    let inv = Mask::Invert {
        mask: Box::new(ellipse.clone()),
    };
    assert!((eval(&inv, [0.4, 0.5]) + eval(&ellipse, [0.4, 0.5]) - 1.).abs() < 1e-6);
}
#[test]
fn brush_flow_and_resolution() {
    let brush = Mask::Brush {
        points: vec![[0.5, 0.5]],
        radius: 0.2,
        hardness: 0.5,
        flow: 0.25,
        density: 1.0,
        auto: None,
    };
    assert_eq!(eval(&brush, [0.5, 0.5]), 0.25);
    assert_eq!(eval(&brush, [0., 0.]), 0.);
    assert!(eval(&brush, [0.65, 0.5]) > 0. && eval(&brush, [0.65, 0.5]) < 0.25);
    let once = Mask::Brush {
        points: vec![[0.2, 0.5], [0.8, 0.5]],
        radius: 0.1,
        hardness: 1.,
        flow: 0.25,
        density: 1.0,
        auto: None,
    };
    let split = Mask::Brush {
        points: vec![[0.2, 0.5], [0.5, 0.5], [0.8, 0.5]],
        radius: 0.1,
        hardness: 1.,
        flow: 0.25,
        density: 1.0,
        auto: None,
    };
    assert!((eval(&once, [0.5, 0.5]) - eval(&split, [0.5, 0.5])).abs() < 1e-6);
    assert!(eval(&once, [0.5, 0.5]) > 0.25);
    let low = vec![0.5; 31 * 21 * 3];
    let high = vec![0.5; 93 * 63 * 3];
    let a = rasterize(Image::new(31, 21, &low).unwrap(), &brush).unwrap();
    let b = rasterize(Image::new(93, 63, &high).unwrap(), &brush).unwrap();
    let error = (0..21)
        .flat_map(|y| (0..31).map(move |x| (x, y)))
        .map(|(x, y)| (a[y * 31 + x] - b[(3 * y + 1) * 93 + 3 * x + 1]).abs())
        .fold(0., f32::max);
    println!("resolution error brush 31×21/93×63: {error}");
    assert!(error < 1e-6);
}
#[test]
fn range_masks_soft_edges_hdr() {
    let data = [
        0., 0., 0., 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 1., 1., 1., 2., 2., 2.,
    ];
    let image = Image::new(5, 1, &data).unwrap();
    let lum = Mask::Luminance {
        low: 0.5,
        high: 1.,
        feather: 0.5,
    };
    let weights = rasterize(image, &lum).unwrap();
    assert_eq!(weights, [0., 0.5, 1., 1., 0.]);
    let color = Mask::Color {
        reference: [0.5; 3],
        radius: 0.,
        feather: 0.5,
    };
    let weights = rasterize(image, &color).unwrap();
    assert_eq!(weights[2], 1.);
    assert_eq!(weights[0], 0.);
    assert!(weights[1] > 0. && weights[1] < 1.);
}
#[test]
fn json_version_and_validation() {
    let original = doc(
        full(),
        Adjustments {
            exposure: 1.,
            ..Default::default()
        },
    );
    let json = original.to_json().unwrap();
    assert_eq!(Document::from_json(&json).unwrap(), original);
    assert!(Document::from_json(&json.replace("\"version\": 1", "\"version\": 99")).is_err());
    assert!(Document::from_json(&json.replace("\"exposure\"", "\"exposur\"")).is_err());
    assert!(Image::new(usize::MAX, 2, &[]).is_err());
    assert!(Image::new(1, 1, &[f32::NAN; 3]).is_err());
    assert!(Mask::Brush {
        points: vec![],
        radius: 1.,
        hardness: 1.,
        flow: 1.,
        density: 1.,
        auto: None
    }
    .validate()
    .is_err());
    let mut m = full();
    for _ in 0..34 {
        m = Mask::Invert { mask: Box::new(m) };
    }
    assert!(m.validate().is_err());
    assert!(doc(
        full(),
        Adjustments {
            contrast: f32::INFINITY,
            ..Default::default()
        }
    )
    .validate()
    .is_err());
}
#[test]
fn exposure_neutral_outside_and_layer_order() {
    let data = img(100, 60);
    let image = Image::new(100, 60, &data).unwrap();
    assert_eq!(
        apply(image, &doc(full(), Adjustments::default())).unwrap(),
        data
    );
    let out = apply(
        image,
        &doc(
            full(),
            Adjustments {
                exposure: 1.,
                ..Default::default()
            },
        ),
    )
    .unwrap();
    assert_eq!(out, data.iter().map(|v| v * 2.).collect::<Vec<_>>());
    let mask = Mask::Radial {
        center: [0.5, 0.5],
        radii: [0.1, 0.1],
        angle: 0.,
        feather: 0.5,
    };
    let out = apply(
        image,
        &doc(
            mask.clone(),
            Adjustments {
                exposure: 1.,
                ..Default::default()
            },
        ),
    )
    .unwrap();
    let weights = rasterize(image, &mask).unwrap();
    let mut maximum = 0.0f32;
    for (i, &w) in weights.iter().enumerate() {
        for c in 0..3 {
            if w == 0. {
                assert_eq!(out[i * 3 + c].to_bits(), data[i * 3 + c].to_bits());
            }
            let error = (out[i * 3 + c] - data[i * 3 + c] * (1. + w)).abs();
            maximum = maximum.max(error);
            assert!(error < 2e-7);
        }
    }
    println!("weighted +1 EV correction: maximum RGB error {maximum:.9}");
    let mut d = doc(
        full(),
        Adjustments {
            exposure: 1.,
            ..Default::default()
        },
    );
    d.layers.push(Layer {
        mask: Mask::Luminance {
            low: 0.7,
            high: 10.,
            feather: 0.,
        },
        adjustments: Adjustments {
            exposure: 1.,
            ..Default::default()
        },
        name: None,
        enabled: true,
        amount: 1.0,
        refine: 0.0,
    });
    let out = apply(image, &d).unwrap();
    assert_eq!(out[0], 0.);
    assert!(out[out.len() - 3] > data[data.len() - 3] * 2.);
}
#[test]
fn tonal_color_and_spatial_parameters() {
    let constant = vec![0.18; 32 * 32 * 3];
    let im = Image::new(32, 32, &constant).unwrap();
    let a = Adjustments {
        clarity: 1.,
        texture: 1.,
        sharpness: 1.,
        noise_reduction: 1.,
        ..Default::default()
    };
    let out = apply(im, &doc(full(), a)).unwrap();
    assert_eq!(out, constant);
    let data: Vec<f32> = (0..64 * 64)
        .flat_map(|i| {
            let x = i % 64;
            let noise = if i % 2 == 0 { 0.01 } else { -0.01 };
            let v = if x < 32 { 0.2 + noise } else { 0.7 + noise };
            [v, v * 0.8, v * 0.5]
        })
        .collect();
    let im = Image::new(64, 64, &data).unwrap();
    for a in [
        Adjustments {
            shadows: 1.,
            ..Default::default()
        },
        Adjustments {
            highlights: -1.,
            ..Default::default()
        },
        Adjustments {
            temperature: 1.,
            tint: 0.2,
            ..Default::default()
        },
        Adjustments {
            saturation: -1.,
            ..Default::default()
        },
        Adjustments {
            contrast: 0.5,
            ..Default::default()
        },
        Adjustments {
            clarity: 0.5,
            texture: 0.5,
            sharpness: 0.5,
            ..Default::default()
        },
        Adjustments {
            noise_reduction: 1.,
            ..Default::default()
        },
    ] {
        let out = apply(im, &doc(full(), a)).unwrap();
        assert_ne!(out, data);
        assert!(out.iter().all(|v| v.is_finite()));
    }
    let out = apply(
        im,
        &doc(
            full(),
            Adjustments {
                noise_reduction: 1.,
                ..Default::default()
            },
        ),
    )
    .unwrap();
    let rms = |v: &[f32]| {
        ((4..28)
            .map(|x| (v[(30 * 64 + x) * 3] - 0.2).powi(2))
            .sum::<f32>()
            / 24.)
            .sqrt()
    };
    println!("noise RMSE {} -> {}", rms(&data), rms(&out));
    assert!(rms(&out) < rms(&data));
    assert!(out[(30 * 64 + 32) * 3] - out[(30 * 64 + 31) * 3] > 0.45);
}
#[test]
fn thread_determinism() {
    let data = img(100, 80);
    let d = doc(
        full(),
        Adjustments {
            clarity: 0.2,
            exposure: 0.3,
            ..Default::default()
        },
    );
    let run = |n| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .unwrap()
            .install(|| apply(Image::new(100, 80, &data).unwrap(), &d).unwrap())
    };
    assert_eq!(run(1), run(4));
}
#[test]
#[ignore = "24 MP measurement, explicitly in a release build"]
fn benchmark_24mp() {
    let data = img(6000, 4000);
    let im = Image::new(6000, 4000, &data).unwrap();
    let mask = Mask::Radial {
        center: [0.5, 0.5],
        radii: [0.5, 0.3],
        angle: 0.3,
        feather: 0.5,
    };
    for (name, a) in [
        (
            "exposure",
            Adjustments {
                exposure: 0.5,
                ..Default::default()
            },
        ),
        (
            "all",
            Adjustments {
                exposure: 0.5,
                contrast: 0.2,
                highlights: -0.2,
                shadows: 0.2,
                temperature: 0.1,
                tint: 0.1,
                saturation: 0.1,
                clarity: 0.2,
                texture: 0.2,
                sharpness: 0.2,
                noise_reduction: 0.2,
                ..Default::default()
            },
        ),
    ] {
        let mut times = Vec::new();
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let out = apply(im, &doc(mask.clone(), a.clone())).unwrap();
            let elapsed = t.elapsed();
            assert!(out.iter().all(|v| v.is_finite()));
            times.push(elapsed.as_secs_f64());
            println!("24 MP {name}: {elapsed:.2?}");
        }
        times.sort_by(f64::total_cmp);
        println!("24 MP {name} Median {:.2} ms", times[1] * 1000.);
    }
    let t = std::time::Instant::now();
    let weights = rasterize(im, &mask).unwrap();
    println!(
        "24 MP Radialraster: {:.2?}, {} Werte",
        t.elapsed(),
        weights.len()
    );
}

#[test]
fn committed_json_fixture_and_extremes() {
    let d = Document::from_json(include_str!("../samples/masks-example.json")).unwrap();
    let data = img(40, 30);
    assert!(apply(Image::new(40, 30, &data).unwrap(), &d).is_ok());
    let hdr = [-0.1, 2.0, 4.0];
    let im = Image::new(1, 1, &hdr).unwrap();
    let out = apply(
        im,
        &doc(
            full(),
            Adjustments {
                exposure: 1.,
                ..Default::default()
            },
        ),
    )
    .unwrap();
    assert_eq!(out, [-0.2, 4., 8.]);
    assert!(apply(
        Image::new(1, 1, &[f32::MAX; 3]).unwrap(),
        &doc(
            Mask::Invert {
                mask: Box::new(full())
            },
            Adjustments {
                exposure: 10.,
                ..Default::default()
            }
        )
    )
    .is_err());
}

#[test]
fn v2_edges_have_limited_halos() {
    let (w, h) = (200, 80);
    let data: Vec<f32> = (0..w * h)
        .flat_map(|i| [if i % w < w / 2 { 0.1 } else { 0.8 }; 3])
        .collect();
    let d = Document {
        version: 2,
        layers: vec![Layer {
            mask: full(),
            adjustments: Adjustments {
                clarity: 1.0,
                texture: 1.0,
                sharpness: 1.0,
                ..Default::default()
            },
            name: None,
            enabled: true,
            amount: 1.0,
            refine: 0.0,
        }],
    };
    let out = apply(Image::new(w, h, &data).unwrap(), &d).unwrap();
    let min = out.iter().copied().fold(f32::INFINITY, f32::min);
    let max = out.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    println!("v2 step edge: {min:.6}..{max:.6}, expected 0.1..0.8");
    assert!(min >= 0.08 && max <= 0.82);
}
