//! Markesteijn on the GPU against the CPU (reference) on a small synthetic image – no files, runs
//! with every `cargo test` (skipped when there is no GPU or `GRAINEO_GPU=0`).

const XT: [[u8; 6]; 6] = [[1, 2, 1, 1, 0, 1], [0, 1, 0, 2, 1, 2], [1, 2, 1, 1, 0, 1], [1, 0, 1, 1, 2, 1], [2, 1, 2, 0, 1, 0], [1, 0, 1, 1, 2, 1]];

#[test]
fn markesteijn_gpu_matches_cpu_on_small_image() {
    if rawdev::gpu::gpu().is_none() {
        eprintln!("skipped: no GPU");
        return;
    }
    // Image with soft gradients, edges, fine lines and a color patch (no noise)
    let (w, h) = (300usize, 220usize);
    let mut rgb = vec![[0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let mut p = [0.2 + 0.5 * fx, 0.3 + 0.4 * fy, 0.6 - 0.3 * fx * fy];
            if (x / 20 + y / 20) % 2 == 0 && x > 150 {
                p = [0.8, 0.2, 0.1];
            }
            if y % 7 == 0 && x < 140 {
                p = [0.9, 0.9, 0.85];
            }
            if ((x as f32 - 60.0).powi(2) + (y as f32 - 150.0).powi(2)).sqrt() < 30.0 {
                p = [0.1, 0.6, 0.9];
            }
            rgb[y * w + x] = p;
        }
    }
    let mut m = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            m[y * w + x] = rgb[y * w + x][XT[y % 6][x % 6] as usize];
        }
    }
    let cpu = raw::development::demosaic_mosaic(&m, w, h, XT, raw::development::Demosaic::Markesteijn3, raw::Sensor::XTrans);
    let gpu = rawdev::gpu::xtrans::markesteijn(&m, w, h, XT).expect("GPU result");
    let (mut max, mut sum, mut n) = (0f32, 0f64, 0usize);
    let mut above = 0usize;
    for (a, b) in cpu.iter().zip(&gpu) {
        for c in 0..3 {
            let d = (a[c] - b[c]).abs();
            max = max.max(d);
            sum += d as f64;
            n += 1;
            if d > 0.01 {
                above += 1;
            }
        }
    }
    let mean = sum / n as f64;
    eprintln!("GPU ↔ CPU: largest deviation {max:.3e}, mean {mean:.3e}, values above 0.01: {above} of {n}");
    assert!(mean < 1e-4, "mean deviation {mean}");
    assert!(above * 1000 < n, "too many larger deviations: {above} of {n}");
}
