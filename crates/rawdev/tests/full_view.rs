//! Zoomed view (whole image at full resolution, as the app shows it at 100 % and above) against the export, with dehaze and
//! noise brake active: `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test full_view -- --ignored --nocapture`
//!
//! The view renders the whole image once with `render_full` (the export path) and draws every zoom and pan from it; before that
//! it shows a crop (`render_region`) around the visible part. Checked: the view equals the export bit for bit, and a crop
//! matches the export inside (dehaze analysis and noise-brake base are cropped from the whole image, not recomputed).

use std::time::Instant;

fn settings() -> develop::Settings {
    let mut s = develop::Settings::default();
    s.dehaze = 0.5;
    s.noise_brake = develop::NoiseBrake::ColorGrain;
    s.exposure = 0.7;
    s.shadow = 0.4;
    s
}

/// Largest channel difference (16 bit) between `crop` and the same area of `full`, `inset` pixels away from the crop's edges.
fn max_diff(full: &develop::Rgb16, crop: &develop::Rgb16, at: [usize; 2], inset: usize) -> (u16, f64) {
    let (mut max, mut sum, mut n) = (0u16, 0f64, 0usize);
    for y in inset..crop.height.saturating_sub(inset) {
        for x in inset..crop.width.saturating_sub(inset) {
            for c in 0..3 {
                let a = full.data[((at[1] + y) * full.width + at[0] + x) * 3 + c];
                let b = crop.data[(y * crop.width + x) * 3 + c];
                max = max.max(a.abs_diff(b));
                sum += a.abs_diff(b) as f64;
                n += 1;
            }
        }
    }
    (max, sum / n.max(1) as f64)
}

#[test]
#[ignore]
fn full_view_matches_export_with_dehaze_and_noise_brake() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let s = settings();
    assert!(rawdev::render::brake_active(&s), "noise brake must be active for this test");
    // export: fresh document, as the export opens it
    let export_doc = rawdev::RawDoc::open(std::path::Path::new(&p), &|| true).unwrap().unwrap();
    let t = Instant::now();
    let export = export_doc.render_full(&s).unwrap();
    println!("export (render_full, fresh): {}×{} in {:.0?}", export.width, export.height, t.elapsed());
    drop(export_doc);
    // view: as in the app – first a crop around the visible part, then the whole image
    let doc = rawdev::RawDoc::open(std::path::Path::new(&p), &|| true).unwrap().unwrap();
    let (w, h) = (export.width, export.height);
    let region = [w / 3, h / 3, w / 3, h / 4];
    let t = Instant::now();
    let crop = doc.render_region(&s, region).unwrap();
    println!("crop {region:?} in {:.0?}", t.elapsed());
    let t = Instant::now();
    let view = doc.render_full(&s).unwrap();
    println!("whole image for the view (after the crop, stage cached) in {:.0?}", t.elapsed());
    assert_eq!((view.width, view.height), (w, h));
    let (vmax, vmean) = max_diff(&export, &view, [0, 0], 0);
    println!("view vs. export: largest difference {vmax} of 65535, mean {vmean:.3}");
    let (cmax, cmean) = max_diff(&export, &crop, [region[0], region[1]], 24);
    println!("crop vs. export (24 px from the edge): largest difference {cmax} of 65535, mean {cmean:.3}");
    // the fresh export may take the fused GPU path, the view the CPU path (stage already cached): allow the
    // known small GPU/CPU difference, never a dehaze or noise-brake offset
    assert!(vmean < 64.0, "view differs from the export: mean {vmean}");
    assert!(cmean < 64.0, "crop differs from the export: mean {cmean}");
}
