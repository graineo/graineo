use masks::*;
fn document() -> Document {
    Document {
        version: 2,
        layers: vec![Layer {
            mask: Mask::Radial {
                center: [0.5, 0.5],
                radii: [0.6, 0.4],
                angle: 0.2,
                feather: 0.5,
            },
            adjustments: Adjustments {
                exposure: 0.3,
                contrast: 0.2,
                highlights: -0.2,
                shadows: 0.2,
                temperature: 0.1,
                tint: 0.1,
                saturation: 0.1,
                clarity: 0.4,
                texture: 0.3,
                sharpness: 0.2,
                noise_reduction: 0.4,
                ..Default::default()
            },
            name: None,
            enabled: true,
            amount: 1.0,
            refine: 0.0,
        }],
    }
}
fn compare(w: usize, h: usize) {
    let data: Vec<f32> = (0..w * h)
        .flat_map(|i| {
            let v = ((i * 13) % 101) as f32 / 100.0;
            [v, v * 0.6, 0.5]
        })
        .collect();
    let im = Image::new(w, h, &data).unwrap();
    let d = document();
    let t = std::time::Instant::now();
    let cpu = apply(im, &d).unwrap();
    println!("CPU v2 {w}×{h}: {:.2?}", t.elapsed());
    let t = std::time::Instant::now();
    let gpu = gpu::apply(im, &d)
        .unwrap()
        .expect("GPU must be available for this explicit check");
    println!(
        "GPU v2 {w}×{h}: {:.2?} inkl. Rasterung/Transfer",
        t.elapsed()
    );
    let max = cpu
        .iter()
        .zip(&gpu)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    let rmse = (cpu
        .iter()
        .zip(&gpu)
        .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
        .sum::<f64>()
        / cpu.len() as f64)
        .sqrt();
    println!("GPU/CPU max={max:.9}, RMSE={rmse:.9}");
    assert!(max < 0.00005);
}
#[test]
#[ignore = "Explicit GPU check"]
fn gpu_reference() {
    compare(128, 96);
}
#[test]
#[ignore = "24 MP GPU measurement"]
fn gpu_24mp() {
    compare(64, 64);
    for _ in 0..3 {
        compare(6000, 4000);
    }
}

#[test]
#[ignore = "GPU check of individual filter levels"]
fn gpu_selective() {
    let data: Vec<f32> = (0..64 * 48 * 3)
        .map(|i| ((i * 17) % 101) as f32 / 100.0)
        .collect();
    let image = Image::new(64, 48, &data).unwrap();
    for a in [
        Adjustments {
            exposure: 0.5,
            ..Default::default()
        },
        Adjustments {
            noise_reduction: 0.7,
            ..Default::default()
        },
        Adjustments {
            sharpness: 0.5,
            ..Default::default()
        },
        Adjustments {
            texture: 0.5,
            ..Default::default()
        },
        Adjustments {
            clarity: 0.5,
            ..Default::default()
        },
    ] {
        let mut d = document();
        d.layers[0].adjustments = a;
        let cpu = apply(image, &d).unwrap();
        let gpu = gpu::apply(image, &d).unwrap().unwrap();
        let max = cpu
            .iter()
            .zip(gpu)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        println!("single slider CPU/GPU max {max:.9}");
        assert!(max < 0.00005);
    }
}

/// Strength and on/off act the same on the graphics card as on the processor.
#[test]
#[ignore = "Explicit GPU check"]
fn gpu_strength_and_switch() {
    let (w, h) = (96, 64);
    let data: Vec<f32> = (0..w * h).flat_map(|i| {
        let v = ((i * 13) % 101) as f32 / 100.0;
        [v, v * 0.6, 0.5]
    }).collect();
    let im = Image::new(w, h, &data).unwrap();
    let mut d = document();
    let first = d.layers[0].clone();
    d.layers[0].amount = 0.4;
    d.layers.push(Layer { enabled: false, ..first });
    d.settle_version();
    assert_eq!(d.version, 4);
    let cpu = apply(im, &d).unwrap();
    let gpu = gpu::apply(im, &d).unwrap().expect("GPU must be available for this explicit check");
    let max = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    println!("GPU/CPU with strength 40 % and a switched-off layer: max={max:.9}");
    assert!(max < 0.00005);
}

/// Brush strokes with density and auto mask act the same on the graphics card as on the processor (the weights are
/// computed on the processor for both).
#[test]
#[ignore = "Explicit GPU check"]
fn gpu_brush_density_and_auto_mask() {
    let (w, h) = (160, 100);
    let data: Vec<f32> = (0..w * h).flat_map(|i| if (i / w) < h / 2 { [0.4, 0.5, 0.8] } else { [0.05, 0.08, 0.03] }).collect();
    let im = Image::new(w, h, &data).unwrap();
    let points: Vec<[f32; 2]> = (0..40).map(|i| [0.1 + 0.02 * i as f32, 0.5 + 0.1 * (i as f32 * 0.4).sin()]).collect();
    let stroke = |density: f32, auto: Option<AutoMask>| Mask::Brush { points: points.clone(), radius: 0.12, hardness: 0.5, flow: 0.4, density, auto };
    let mut d = document();
    d.layers[0].mask = Mask::Combine { operation: Operation::Add, left: Box::new(stroke(0.6, None)), right: Box::new(stroke(1.0, Some(AutoMask { reference: [0.4, 0.5, 0.8], tolerance: 0.15 }))) };
    d.settle_version();
    assert_eq!(d.version, 4);
    let cpu = apply(im, &d).unwrap();
    let gpu = gpu::apply(im, &d).unwrap().expect("GPU must be available for this explicit check");
    let max = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    println!("GPU/CPU brush with density and auto mask: max={max:.9}");
    assert!(max < 0.00005);
}
