//! Timing of painted strokes: a wild back-and-forth stroke of 20 000 points rendered at 24 MP.
//! Run with `cargo test --release -p masks --test brush_perf -- --ignored --nocapture`.
use masks::*;

/// A wild stroke: a deterministic random walk of `n` points (steps of 0.001…0.009 of the short side, turning a lot,
/// bouncing off the edges), like scribbling back and forth over an area.
pub fn wild(n: usize, seed: u64) -> Vec<[f32; 2]> {
    let mut s = seed;
    let mut next = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((s >> 33) as f32) / (1u64 << 31) as f32
    };
    let mut p = [0.5f32, 0.5];
    let mut angle = 0.0f32;
    (0..n)
        .map(|_| {
            angle += (next() - 0.5) * 2.5;
            let step = 0.001 + 0.008 * next();
            p[0] += angle.cos() * step;
            p[1] += angle.sin() * step;
            for v in &mut p {
                if !(0.1..=0.9).contains(v) {
                    *v = v.clamp(0.1, 0.9);
                    angle += std::f32::consts::PI;
                }
            }
            p
        })
        .collect()
}

#[test]
#[ignore = "24 MP brush timing"]
fn wild_stroke_24mp() {
    let data = vec![0.18f32; 6000 * 4000 * 3];
    let image = Image::new(6000, 4000, &data).unwrap();
    for radius in [0.04f32, 0.01, 0.002] {
        let mask = Mask::Brush { points: wild(20_000, 7), radius, hardness: 0.6, flow: 0.2, density: 1.0, auto: None };
        let t = std::time::Instant::now();
        match rasterize(image, &mask) {
            Ok(w) => println!("wild stroke 20000 points, radius {radius}: {:.2?} (mean weight {:.4})", t.elapsed(), w.iter().sum::<f32>() / w.len() as f32),
            Err(e) => println!("wild stroke 20000 points, radius {radius}: error after {:.2?}: {e}", t.elapsed()),
        }
    }
}

/// Many strokes (a brush part of 200 strokes of 500 points each) at the size of the overlay (512 px long side) and of
/// the preview (≈ 2.8 MP): the cost of computing the weights again while painting.
#[test]
#[ignore = "brush timing"]
fn many_strokes_overlay_and_preview() {
    let strokes: Vec<Mask> = (0..200).map(|i| Mask::Brush { points: wild(500, 100 + i), radius: 0.04, hardness: 0.6, flow: 0.2, density: 1.0, auto: None }).collect();
    fn tree(v: &[Mask]) -> Mask {
        if v.len() == 1 {
            return v[0].clone();
        }
        let n = v.len() / 2;
        Mask::Combine { operation: Operation::Add, left: Box::new(tree(&v[..n])), right: Box::new(tree(&v[n..])) }
    }
    let mask = tree(&strokes);
    for (w, h) in [(512usize, 341usize), (2048, 1365)] {
        let data = vec![0.18f32; w * h * 3];
        let image = Image::new(w, h, &data).unwrap();
        let mut times = Vec::new();
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let _ = rasterize(image, &mask).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!("200 strokes × 500 points, {w}×{h}: median {:.1} ms (min {:.1}, max {:.1})", times[2], times[0], times[4]);
        // painting: the last stroke grows by one point per frame, the other strokes are summed once
        let mut growing = strokes.clone();
        let mut frames = Vec::new();
        for k in 0..20 {
            if let Some(Mask::Brush { points, .. }) = growing.last_mut() {
                let p = *points.last().unwrap();
                points.push([p[0] + 0.002 * (k as f32).cos(), p[1] + 0.002 * (k as f32).sin()]);
            }
            let m = tree(&growing);
            let t = std::time::Instant::now();
            let _ = rasterize(image, &m).unwrap();
            frames.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let first = frames[0];
        frames.sort_by(f64::total_cmp);
        println!("  painting (last stroke grows): first frame {first:.1} ms, then median {:.2} ms, max {:.2} ms", frames[10], frames[19]);
    }
}
