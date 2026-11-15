//! graineo: parity of the sub-layer grain port (`apply_grain_to_density_layers`) with upstream
//! Python. Needs reference files from upstream (`<dir>/<stock>.in`, `<dir>/<stock>.out`, raw
//! little-endian f64, 64×48×3, pixel 6 µm, upstream default grain parameters); skipped without
//! `SPEKTRAFILM_PARITY_DIR`.

use spektrafilm_core::profile::load_profile;
use spektrafilm_gpu::cpu_backend::CpuBackend;
use spektrafilm_math::image::ImageBuf;
use spektrafilm_math::precision::{from_f64, to_f64};
use std::path::Path;

fn read_f64(p: &Path) -> Vec<f64> {
    std::fs::read(p).unwrap().chunks_exact(8).map(|b| f64::from_le_bytes(b.try_into().unwrap())).collect()
}

#[test]
fn sublayer_grain_matches_upstream() {
    let Some(dir) = std::env::var_os("SPEKTRAFILM_PARITY_DIR") else {
        eprintln!("SPEKTRAFILM_PARITY_DIR not set – skipped");
        return;
    };
    let dir = Path::new(&dir);
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/profiles");
    for stock in ["kodak_portra_400", "fujifilm_velvia_100"] {
        let film = load_profile(&data.join(format!("{stock}.json"))).unwrap();
        let input = read_f64(&dir.join(format!("{stock}.in")));
        let want = read_f64(&dir.join(format!("{stock}.out")));
        let (w, h) = (64u32, 48u32);
        let img = ImageBuf::from_data(w, h, input.iter().map(|&v| from_f64(v)).collect());
        let mut curves = film.density_curves_f64();
        let mut min = [f64::INFINITY; 3];
        for r in &curves {
            for c in 0..3 {
                if r[c].is_finite() {
                    min[c] = min[c].min(r[c]);
                }
            }
        }
        for r in &mut curves {
            for c in 0..3 {
                r[c] -= min[c];
            }
        }
        let out = spektrafilm_model::grain::apply_grain_to_density_layers(
            &img,
            &curves,
            &film.data.density_curves_layers,
            3,
            film.is_positive(),
            6.0,
            0.2,
            [1.6, 1.6, 3.2],
            &[2.0, 1.0, 0.5],
            [0.03; 3],
            [0.97, 0.99, 0.97],
            0.65,
            1.0,
            [0.2, 30.0],
            &CpuBackend,
        );
        let got: Vec<f64> = out.data.iter().map(|&v| to_f64(v)).collect();
        let max_diff = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        let mean_abs: f64 = want.iter().map(|v| v.abs()).sum::<f64>() / want.len() as f64;
        let mean_diff: f64 = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).sum::<f64>() / want.len() as f64;
        let imax = got.iter().zip(&want).enumerate().max_by(|a, b| (a.1 .0 - a.1 .1).abs().total_cmp(&(b.1 .0 - b.1 .1).abs())).map(|x| x.0).unwrap();
        eprintln!("{stock}: max |Δ| = {max_diff:.3e} at px {} ch {}, mean |Δ| {mean_diff:.3e} (mean |D| {mean_abs:.3})", imax / 3, imax % 3);
        if let Some(p) = std::env::var_os("SPEKTRAFILM_PARITY_DUMP") {
            let b: Vec<u8> = got.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(Path::new(&p).join(format!("{stock}.rust")), b).unwrap();
        }
        // same random streams (a different stream would give |Δ| ≈ 0.05 everywhere); single pixels
        // differ by up to ~1.5e-3 (upstream's numba fastmath interpolation/blur)
        assert!(max_diff < 5e-3 && mean_diff < 1e-4, "{stock}: max |Δ| = {max_diff}, mean {mean_diff}");
    }
}

/// The hashed/GPU form of the sub-layer grain (`GrainClasses::upstream_layers`) has the same noise
/// statistics as the exact port: compare mean and σ on flat patches.
#[test]
fn hashed_sublayer_grain_has_upstream_statistics() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/profiles");
    for stock in ["kodak_portra_400", "fujifilm_velvia_100"] {
        let film = load_profile(&data.join(format!("{stock}.json"))).unwrap();
        let mut curves = film.density_curves_f64();
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for r in &curves {
            for c in 0..3 {
                if r[c].is_finite() {
                    min[c] = min[c].min(r[c]);
                    max[c] = max[c].max(r[c]);
                }
            }
        }
        for r in &mut curves {
            for c in 0..3 {
                r[c] -= min[c];
            }
        }
        let (w, h) = (256u32, 256u32);
        let pix = 6.0f32;
        let classes = spektrafilm_gpu::GrainClasses::upstream_layers(
            &curves, &film.data.density_curves_layers, 3, &[2.0, 1.0, 0.5], [0.03; 3], pix as f64, 0.2,
            [1.6, 1.6, 3.2], 1.0, 0.65, film.is_positive(),
        );
        for frac in [0.1, 0.3, 0.5, 0.7, 0.9] {
            let d: Vec<f64> = (0..3).map(|c| frac * (max[c] - min[c])).collect();
            let img = ImageBuf::from_data(w, h, (0..w * h).flat_map(|_| [from_f64(d[0]), from_f64(d[1]), from_f64(d[2])]).collect());
            let exact = spektrafilm_model::grain::apply_grain_to_density_layers(
                &img, &curves, &film.data.density_curves_layers, 3, film.is_positive(), pix, 0.2, [1.6, 1.6, 3.2],
                &[2.0, 1.0, 0.5], [0.03; 3], [0.97, 0.99, 0.97], 0.65, 1.0, [0.2, 30.0], &CpuBackend,
            );
            let hashed = spektrafilm_model::grain::apply_grain_classes(
                &img, pix, 0.2, [1.6, 1.6, 3.2], [0.03; 3], [max[0] - min[0], max[1] - min[1], max[2] - min[2]],
                [0.97, 0.99, 0.97], 0.65, false, [0, 0, w], &classes, &CpuBackend,
            );
            for ch in 0..3 {
                let stat = |im: &ImageBuf| {
                    let v: Vec<f64> = im.pixels().skip(w as usize * 8).take(w as usize * (h as usize - 16)).map(|p| to_f64(p[ch])).collect();
                    let m = v.iter().sum::<f64>() / v.len() as f64;
                    (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt())
                };
                let (me, se) = stat(&exact);
                let (mh, sh) = stat(&hashed);
                eprintln!("{stock} D/Dmax {frac} ch {ch}: mean {me:.4}/{mh:.4} σ {se:.5}/{sh:.5}");
                assert!((me - mh).abs() < 0.02, "{stock} {frac} ch {ch}: mean {me} vs {mh}");
                assert!((se / sh - 1.0).abs() < 0.12, "{stock} {frac} ch {ch}: σ {se} vs {sh}");
            }
        }
    }
}
