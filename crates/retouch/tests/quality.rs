use retouch::*;
fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Vec<f32> {
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            data.extend(f(x, y));
        }
    }
    data
}
fn heal(data: &[f32], w: usize, h: usize, mask: &Mask, s: Source) -> Patch {
    compute(Image::new(w, h, data).unwrap(), mask, s, Options::default()).unwrap()
}
fn max_error(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0., f32::max)
}
#[test]
fn clone_feather_overlap_and_untouched() {
    let data = image(100, 80, |x, y| [x as f32 / 100., y as f32 / 80., -0.0]);
    let m = Mask::circle([50., 40.], 10., 0.4).unwrap();
    let img = Image::new(100, 80, &data).unwrap();
    let patch = compute(
        img,
        &m,
        Source { dx: 5, dy: 0 },
        Options {
            method: Method::Clone,
            ..Options::default()
        },
    )
    .unwrap();
    let out = patch.apply(img).unwrap();
    let [x, y, w, _] = m.bounds();
    for py in 0..80 {
        for px in 0..100 {
            let a = if px >= x && px < x + w && py >= y && py < y + m.bounds()[3] {
                m.weights()[(py - y) * w + px - x]
            } else {
                0.
            };
            for c in 0..3 {
                let i = (py * 100 + px) * 3 + c;
                if a == 0. {
                    assert_eq!(out[i].to_bits(), data[i].to_bits());
                } else {
                    assert!(
                        (out[i] - (data[i] * (1. - a) + data[(py * 100 + px + 5) * 3 + c] * a))
                            .abs()
                            < 1e-6
                    );
                }
            }
        }
    }
}
#[test]
fn heals_dust_on_colored_ramp_without_seam() {
    let clean = image(160, 120, |x, y| {
        [
            0.1 + x as f32 * 0.003,
            0.2 + y as f32 * 0.002,
            0.6 - x as f32 * 0.001,
        ]
    });
    let mut dirty = clean.clone();
    for y in 51..70 {
        for x in 71..90 {
            if (x as f32 - 80.).hypot(y as f32 - 60.) < 8. {
                for c in 0..3 {
                    dirty[(y * 160 + x) * 3 + c] *= 0.2;
                }
            }
        }
    }
    let mask = Mask::circle([80., 60.], 14., 0.65).unwrap();
    let patch = heal(&dirty, 160, 120, &mask, Source { dx: 40, dy: 0 });
    let out = patch.apply(Image::new(160, 120, &dirty).unwrap()).unwrap();
    let error = max_error(&out, &clean);
    println!(
        "ramp/dust: max. RGB error {error:.9}, {:?}",
        patch.report
    );
    assert!(patch.report.converged);
    assert!(error < 2e-6);
    // Also measures gradient across the feather/boundary, not just the patch interior.
    let gradient = (1..160)
        .flat_map(|x| (0..120).map(move |y| (x, y)))
        .map(|(x, y)| {
            let i = (y * 160 + x) * 3;
            ((out[i] - out[i - 3]) - (clean[i] - clean[i - 3])).abs()
        })
        .fold(0., f32::max);
    println!("seam: max. gradient error {gradient:.9}");
    assert!(gradient < 3e-6);
}
#[test]
fn source_texture_and_edge_survive_color_adaptation() {
    let mut data = image(200, 100, |x, y| {
        let t = if x % 40 < 20 { 0.03 } else { -0.03 };
        let texture = (((x % 40) * 13 + y * 7) % 11) as f32 * 0.001;
        let offset = if x < 100 { 0.15 } else { 0.55 };
        [
            offset + t + texture,
            offset * 0.8 + t + texture,
            offset * 0.5 + t + texture,
        ]
    });
    let expected = data.clone();
    for y in 43..58 {
        for x in 53..68 {
            data[(y * 200 + x) * 3..(y * 200 + x) * 3 + 3].fill(0.);
        }
    }
    let mask = Mask::circle([60., 50.], 20., 0.9).unwrap();
    let patch = heal(&data, 200, 100, &mask, Source { dx: 80, dy: 0 });
    let out = patch.apply(Image::new(200, 100, &data).unwrap()).unwrap();
    let e = max_error(&out, &expected);
    println!("texture/edge/color offset: max. error {e:.9}");
    assert!(patch.report.converged);
    assert!(e < 2e-6);
}
#[test]
fn brush_and_holes_disconnected_masks() {
    let m = Mask::brush(&[[30., 30.], [60., 30.], [60., 60.]], 5., 0.5).unwrap();
    let [x, y, w, _] = m.bounds();
    for px in 30..61 {
        assert_eq!(m.weights()[(30 - y) * w + px - x], 1.);
    }
    let mut weights = vec![1.; 15 * 15];
    for y in 0..15 {
        weights[y * 15 + 7] = 0.;
    }
    let mask = Mask::new(30, 30, 15, 15, weights).unwrap();
    let data = image(100, 100, |x, y| [x as f32 * 0.01, y as f32 * 0.01, 2.]);
    let patch = heal(&data, 100, 100, &mask, Source { dx: 25, dy: 0 });
    let out = patch.apply(Image::new(100, 100, &data).unwrap()).unwrap();
    assert!(patch.report.converged);
    assert!(max_error(&out, &data) < 2e-6);
}
#[test]
fn automatic_source_is_deterministic_and_finds_matching_edge() {
    let data = image(160, 120, |x, y| {
        let v = if x % 40 < 20 { 0.1 } else { 0.7 };
        [v + y as f32 * 0.001, v, v]
    });
    let mask = Mask::circle([60., 60.], 8., 0.7).unwrap();
    let img = Image::new(160, 120, &data).unwrap();
    let a = find_source(
        img,
        &mask,
        Search {
            radius: 40,
            step: 8,
        },
    )
    .unwrap();
    let b = find_source(
        img,
        &mask,
        Search {
            radius: 40,
            step: 8,
        },
    )
    .unwrap();
    assert_eq!(a.source, b.source);
    assert_eq!(a.mean_squared_error, 0.);
    assert_eq!(a.source.dy, 0);
    assert_eq!(a.source.dx.abs(), 40);
    println!(
        "Automatic source: {:?}, MSE {}",
        a.source, a.mean_squared_error
    );
}
#[test]
fn invalid_inputs_empty_mask_hdr_and_iteration_limit() {
    assert!(Image::new(usize::MAX, 2, &[]).is_err());
    assert!(Image::new(1, 1, &[0., f32::NAN, 0.]).is_err());
    assert!(Mask::new(1, 1, 1, 1, vec![1.1]).is_err());
    assert!(Mask::circle([2., 3.], f32::INFINITY, 0.5).is_err());
    let data = image(60, 60, |x, y| {
        [-0.5 + x as f32 * 0.02, 2. + y as f32 * 0.01, 4.]
    });
    let img = Image::new(60, 60, &data).unwrap();
    let mask = Mask::circle([25., 25.], 6., 0.5).unwrap();
    assert!(compute(
        img,
        &mask,
        Source {
            dx: i32::MAX,
            dy: 0
        },
        Options::default()
    )
    .is_err());
    let edge = Mask::circle([1., 1.], 3., 0.5).unwrap();
    assert!(compute(img, &edge, Source { dx: 0, dy: 0 }, Options::default()).is_err());
    assert!(find_source(img, &mask, Search { radius: 1, step: 1 }).is_err());
    let empty = Mask::new(10, 10, 2, 2, vec![0.; 4]).unwrap();
    let p = heal(&data, 60, 60, &empty, Source { dx: 2, dy: 0 });
    assert_eq!(p.apply(img).unwrap(), data);
    let p = compute(
        img,
        &mask,
        Source { dx: 15, dy: 0 },
        Options {
            max_iterations: 1,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(!p.report.converged);
    let p = heal(&data, 60, 60, &mask, Source { dx: 15, dy: 0 });
    assert!(max_error(&p.apply(img).unwrap(), &data) < 2e-6);
}
#[test]
fn cpu_thread_counts_agree() {
    // >32768 active pixels exercises the parallel matrix multiplication as well.
    let data = image(600, 400, |x, y| [x as f32 * 0.001, y as f32 * 0.002, 0.8]);
    let mask = Mask::circle([200., 200.], 110., 0.5).unwrap();
    let run = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                let patch = heal(&data, 600, 400, &mask, Source { dx: 250, dy: 0 });
                assert!(patch.report.converged);
                patch.rgb().to_vec()
            })
    };
    assert_eq!(run(1), run(4));
}
#[test]
#[ignore = "24 MP timing, run explicitly in a release build"]
fn benchmark_24mp() {
    let data = image(6000, 4000, |x, y| {
        [0.1 + x as f32 / 12000., 0.2 + y as f32 / 10000., 0.4]
    });
    let mask = Mask::circle([3000., 2000.], 40., 0.7).unwrap();
    let img = Image::new(6000, 4000, &data).unwrap();
    let start = std::time::Instant::now();
    let found = find_source(img, &mask, Search::default()).unwrap();
    println!(
        "24 MP Quellsuche: {:.2?}, {} Kandidaten, {:?}",
        start.elapsed(),
        found.candidates,
        found.source
    );
    for method in [Method::Clone, Method::Heal] {
        let mut times = Vec::new();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let img = Image::new(6000, 4000, &data).unwrap();
            let validated = start.elapsed();
            let t = std::time::Instant::now();
            let patch = compute(
                img,
                &mask,
                Source { dx: 200, dy: 0 },
                Options {
                    method,
                    ..Options::default()
                },
            )
            .unwrap();
            let kernel = t.elapsed();
            let out = patch.apply(img).unwrap();
            let total = start.elapsed();
            assert!(patch.report.converged);
            if method == Method::Heal {
                assert!(max_error(&out, &data) < 2e-6);
            }
            println!("24 MP {method:?}: validation {validated:.2?}, kernel {kernel:.2?}, total {total:.2?}, {:?}",patch.report);
            times.push(total.as_secs_f64());
        }
        times.sort_by(f64::total_cmp);
        println!(
            "24 MP {method:?} median {:.3} ms (5 runs, incl. output allocation)",
            times[2] * 1000.
        );
    }
}
