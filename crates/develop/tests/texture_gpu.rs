//! Foveon detail bands (fine/structure gains of the luminance residual): the GPU noise reduction must match the
//! CPU path within the usual GPU/CPU tolerance of the chain (f32 rounding, measured at 100/100: mean |Δ| ≈ 0.0012,
//! max |Δ| ≈ 3.5–4.5 counts).
//!
//! Runs only when `GRAINEO_TEST_MERRILL_DIR` names a folder with `DP2M5418.X3F` and a GPU is used (otherwise skipped).

#[test]
fn texture_bands_gpu_matches_cpu() {
    let Some(dir) = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(std::path::PathBuf::from) else {
        eprintln!("GRAINEO_TEST_MERRILL_DIR not set, skipped");
        return;
    };
    let path = dir.join("DP2M5418.X3F");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let key = develop::SppKey { texture: [30, 120], ..Default::default() };
    let gpu = develop::spp::working_key(&path, &key, true, &cancel).expect("GPU base");
    if !develop::spp::last_used_gpu() {
        eprintln!("no GPU used, skipped");
        return;
    }
    let cpu = develop::spp::working_key(&path, &key, false, &cancel).expect("CPU base");
    let neutral = develop::spp::working_key(&path, &develop::SppKey::default(), false, &cancel).expect("CPU base 100/100");
    assert_eq!((cpu.width, cpu.height), (gpu.width, gpu.height));
    let (mut sum, mut max, mut effect) = (0f64, 0f64, 0f64);
    for ((a, b), n) in cpu.data.iter().zip(&gpu.data).zip(&neutral.data) {
        let d = f64::from((a - b).abs());
        sum += d;
        max = max.max(d);
        effect += f64::from((a - n).abs());
    }
    let len = cpu.data.len() as f64;
    let (mean, effect) = (sum / len, effect / len);
    eprintln!("GPU vs CPU at 30/120: mean |Δ| {mean:.4}, max |Δ| {max:.2}; band effect (CPU vs 100/100) mean {effect:.2}");
    // the gains act at all, and the GPU follows the CPU far more closely than the effect itself
    assert!(effect > 1.0, "band gains without effect: {effect}");
    assert!(mean < 0.01, "mean deviation {mean}");
    assert!(max < 16.0, "max deviation {max}");
}
