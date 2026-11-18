//! White balance of real files: as-shot neutral → (Kelvin, tint) → neutral gives the as-shot neutral back (so "as
//! shot" and the same values set by hand look the same), and the tint has the sign of the system RAW engine.
//! Files from `GRAINEO_TEST_WB_FILES` (paths separated by `:`); skipped without the variable.

use std::path::Path;

#[test]
fn as_shot_round_trip_and_sign() {
    let Ok(list) = std::env::var("GRAINEO_TEST_WB_FILES") else {
        eprintln!("GRAINEO_TEST_WB_FILES not set, skipped");
        return;
    };
    for p in list.split(':').filter(|p| !p.is_empty()) {
        let f = raw::RawFile::open(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        let info = &f.info;
        let wb = info.wb.expect("as-shot white balance");
        let n = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
        let (k, t) = rawdev::wb::temp_from_neutral(info, n).expect("temperature");
        let back = rawdev::wb::neutral_for(info, k, t).expect("neutral");
        let err = (0..3).map(|i| (back[i] / n[i] - 1.0).abs()).fold(0.0f32, f32::max);
        #[cfg(target_os = "macos")]
        let apple = raw::apple::probe(Path::new(p)).ok().map(|a| a.as_shot);
        #[cfg(not(target_os = "macos"))]
        let apple: Option<(f32, f32)> = None;
        let name = Path::new(p).file_name().unwrap().to_string_lossy();
        println!("WB\t{name}\t{} {}\t{k:.0} K\t{t:+.1}\tround trip {err:.5}\tapple {apple:?}", info.maker, info.model);
        assert!(err < 2e-3, "{name}: round trip {back:?} vs {n:?}");
        // slider direction with this camera's matrices: + assumes greener light, so the green gain drops (magenta image)
        let up = rawdev::wb::neutral_for(info, k, t + 20.0).expect("neutral");
        let green = |n: [f32; 3]| 0.5 * (n[0] + n[2]) / n[1];
        assert!(green(up) < green(n), "{name}: tint + does not turn the image magenta ({up:?} vs {n:?})");
        // Where the system engine reaches the same temperature it uses the same matrices (DNG with its embedded
        // matrices): then the tint has to agree in value and sign. Other files (own matrices of either engine, unsuitable
        // embedded matrices of the Leica Q/Q2/Q3) differ in both and are only printed.
        if let Some((ak, at)) = apple.filter(|a| (a.0 - k).abs() < 0.01 * k) {
            assert!((at - t).abs() < 1.0, "{name}: {k} K / {t:+} vs system engine {ak} K / {at:+}");
        }
    }
}
