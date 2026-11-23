//! Synthetic brackets with known truth: exposure factors, alignment, weights and the merge.

use super::*;
use std::sync::atomic::AtomicBool;

pub(crate) fn info(w: usize, h: usize, t: f32) -> raw::Info {
    raw::Info {
        format: "TEST".into(),
        maker: "Test".into(),
        model: "Cam".into(),
        mode: String::new(),
        width: w,
        height: h,
        channels: 1,
        bits: 14,
        sensor: raw::Sensor::Bayer,
        cfa: "RGGB".into(),
        cfa_size: (2, 2),
        black: Vec::new(),
        black_pattern: (1, 1, 1),
        white: vec![1.0],
        wb: Some([2.0, 1.0, 1.5]),
        matrix: None,
        matrix_illuminants: Vec::new(),
        orientation: 1,
        active: None,
        crop: Some(raw::Region { x: 8, y: 8, w: w - 16, h: h - 16 }),
        fuji_rotated: false,
        baseline_exposure: 0.0,
        dng_profile: Vec::new(),
        exif: raw::Exif { iso: Some(100), exposure_time: Some(t), aperture: Some(8.0), ..Default::default() },
        corrections: Vec::new(),
        calibration: Vec::new(),
        analog_balance: None,
        fuji_dr: None,
    }
}

pub(crate) const RGGB: [[u8; 6]; 6] = {
    let mut p = [[0u8; 6]; 6];
    let mut y = 0;
    while y < 6 {
        let mut x = 0;
        while x < 6 {
            p[y][x] = match (y % 2, x % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            x += 1;
        }
        y += 1;
    }
    p
};

/// Scene radiance (camera RGB before white balance, reference exposure): a gradient over 9 EV with texture and a
/// bright patch far above the reference white.
fn scene(x: f64, y: f64, w: usize) -> [f64; 3] {
    let t = x / w as f64;
    let base = (2f64).powf(-7.0 + 9.0 * t) * (1.0 + 0.25 * (x * 0.21).sin() * (y * 0.17).cos());
    let tint = [0.45, 1.0, 0.65];
    std::array::from_fn(|c| base * tint[c])
}

struct Rng(u64);
impl Rng {
    fn u(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn g(&mut self) -> f64 {
        (-2.0 * self.u().ln()).sqrt() * (std::f64::consts::TAU * self.u()).cos()
    }
}

/// Frame with exposure `k` relative to the reference, shifted by (dx, dy) px, Poisson-Gauss noise, clipped at 1.
fn frame(w: usize, h: usize, k: f64, dx: f64, dy: f64, seed: u64) -> frames::Frame {
    let mut rng = Rng(seed);
    let (a, b) = (2e-5, 1e-7);
    let mut m = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = RGGB[y % 6][x % 6] as usize;
            let v = scene(x as f64 + dx, y as f64 + dy, w)[c] * k;
            let n = (a * v + b).sqrt() * rng.g();
            m[y * w + x] = (v + n).clamp(0.0, 1.0) as f32;
        }
    }
    frames::from_mosaic(format!("f{k}.raw").into(), info(w, h, (k / 64.0) as f32), m, RGGB)
}

/// Like [`frame`] with a bright square object (side 40 px, five times the local scene) with its top-left corner at `obj`.
fn frame_obj(w: usize, h: usize, k: f64, obj: Option<(usize, usize)>, seed: u64) -> frames::Frame {
    let mut rng = Rng(seed);
    let (a, b) = (2e-5, 1e-7);
    let mut m = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = RGGB[y % 6][x % 6] as usize;
            let mut v = scene(x as f64, y as f64, w)[c] * k;
            if let Some((ox, oy)) = obj {
                if x >= ox && x < ox + 40 && y >= oy && y < oy + 40 {
                    v *= 5.0;
                }
            }
            let n = (a * v + b).sqrt() * rng.g();
            m[y * w + x] = (v + n).clamp(0.0, 1.0) as f32;
        }
    }
    frames::from_mosaic(format!("f{k}.raw").into(), info(w, h, (k / 64.0) as f32), m, RGGB)
}

fn ctl_run<T>(f: impl FnOnce(&Control) -> T) -> T {
    let cancel = AtomicBool::new(false);
    let p = |_: f32| {};
    f(&Control { cancel: &cancel, progress: &p })
}

#[test]
fn clip_level_from_histogram() {
    let fr = frame(256, 128, 8.0, 0.0, 0.0, 1);
    // bright frame clips at 1.0 in all channels → 0.97
    assert!((fr.clip[1] - 0.97).abs() < 1e-3, "{:?}", fr.clip);
    let dark = frame(256, 128, 1.0 / 64.0, 0.0, 0.0, 2);
    // no pile-up at the top → nominal white
    assert_eq!(dark.clip, [0.97; 3]);
}

#[test]
fn factors_alignment_and_merge_against_truth() {
    let (w, h) = (480, 320);
    let fs = vec![frame(w, h, 0.25, 3.0, -2.0, 11), frame(w, h, 1.0, 0.0, 0.0, 12), frame(w, h, 4.0, -4.5, 1.5, 13)];
    let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)).unwrap();
    assert_eq!(plan.reference, 1);
    // exposure factors within 0.02 EV
    for (i, kt) in [0.25f64, 1.0, 4.0].iter().enumerate() {
        assert!((plan.k[i] / kt).log2().abs() < 0.02, "k[{i}] = {} (truth {kt})", plan.k[i]);
    }
    // alignment: the warp maps reference → frame, the truth is a pure shift of (−dx, −dy)
    for (i, (dx, dy)) in [(3.0, -2.0), (0.0, 0.0), (-4.5, 1.5)].iter().enumerate() {
        let (tx, ty, rot, _) = plan.warps[i].describe(w, h);
        assert!((tx + dx).abs() < 0.15 && (ty + dy).abs() < 0.15 && rot.abs() < 0.02, "frame {i}: {tx:.3}, {ty:.3}, {rot:.4}°");
    }
    let preview = ctl_run(|c| merge::preview(&plan, 128, c)).unwrap();
    assert!(preview.w <= 128 && preview.h <= 128);
    let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
    assert_eq!((m.w, m.h), (w - 16, h - 16));
    // against the truth (white-balanced, reference units), away from the borders of the warped frames
    let wb = [2.0, 1.0, 1.5];
    let (mut err, mut n, mut peak) = (0.0, 0.0, 0f64);
    for y in 16..m.h - 16 {
        for x in 16..m.w - 16 {
            let t = scene((x + 8) as f64, (y + 8) as f64, w);
            let p = m.px[y * m.w + x];
            for c in 0..3 {
                let tv = t[c] * wb[c];
                if tv > 0.004 {
                    err += ((p[c] as f64 / tv).log2()).powi(2);
                    n += 1.0;
                }
            }
            peak = peak.max(p[1] as f64);
        }
    }
    let rms_ev = (err / n).sqrt();
    assert!(rms_ev < 0.08, "log error {rms_ev:.4} EV");
    // the bright end of the scene (≈ 4× above the reference white) is recovered from the dark frame
    assert!(peak > 2.5, "peak {peak}");
    // noise of the result lower than that of the reference alone in the shadows
    let nm = m.noise;
    let (a_r, b_r) = plan.noise[1];
    assert!(nm.a[1] * 0.002 + nm.b[1] < a_r * 0.002 + b_r, "{nm:?} vs ({a_r}, {b_r})");
}

/// Three-layer frame (Foveon): every layer carries the scene with its own tint, own noise, clipped per layer at 1.
fn planar_frame(w: usize, h: usize, k: f64, dx: f64, dy: f64, seed: u64) -> frames::Frame {
    let mut rng = Rng(seed);
    let (a, b) = (2e-5, 1e-7);
    let mut px = vec![[0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let t = scene(x as f64 + dx, y as f64 + dy, w);
            for c in 0..3 {
                let v = t[c] * k;
                px[y * w + x][c] = (v + (a * v + b).sqrt() * rng.g()).clamp(0.0, 1.0) as f32;
            }
        }
    }
    let mut i = info(w, h, (k / 64.0) as f32);
    i.sensor = raw::Sensor::Linear;
    i.wb = Some([1.0; 3]);
    let clip = frames::planar_clip(&[&px]);
    frames::from_planar(format!("p{k}.x3f").into(), i, px, w, h, clip)
}

/// Foveon path: the layers are merged without a demosaic; exposure factors, alignment, highlight recovery and the
/// noise gain work as for a mosaic, and the saturation of one layer excludes the pixel.
#[test]
fn planar_frames_merge_against_truth() {
    let (w, h) = (480, 320);
    let fs = vec![planar_frame(w, h, 0.25, 3.0, -2.0, 21), planar_frame(w, h, 1.0, 0.0, 0.0, 22), planar_frame(w, h, 4.0, -4.5, 1.5, 23)];
    // the brightest frame clips every layer at 1.0 → clip 0.97 of the 99.99 % value (≈ 1)
    assert!(fs[2].clip.iter().all(|c| (c - 0.97).abs() < 0.02), "{:?}", fs[2].clip);
    let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)).unwrap();
    for (i, kt) in [0.25f64, 1.0, 4.0].iter().enumerate() {
        assert!((plan.k[i] / kt).log2().abs() < 0.03, "k[{i}] = {} (truth {kt})", plan.k[i]);
    }
    let pv = ctl_run(|c| merge::preview(&plan, 128, c)).unwrap();
    assert!(pv.w <= 128);
    let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
    let (mut err, mut n, mut peak) = (0.0, 0.0, 0f64);
    for y in 16..m.h - 16 {
        for x in 16..m.w - 16 {
            let t = scene((x + 8) as f64, (y + 8) as f64, w);
            let p = m.px[y * m.w + x];
            for c in 0..3 {
                if t[c] > 0.004 {
                    err += ((p[c] as f64 / t[c]).log2()).powi(2);
                    n += 1.0;
                }
            }
            peak = peak.max(p[1] as f64);
        }
    }
    let rms_ev = (err / n).sqrt();
    assert!(rms_ev < 0.08, "log error {rms_ev:.4} EV");
    assert!(peak > 2.5, "peak {peak}");
}

#[test]
fn weights_follow_noise_model() {
    // read-noise limited: weight ∝ k²; shot-noise limited: ∝ k
    let (a, b) = (1e-4, 1e-8);
    let w1 = merge::weight(1.0, 1.0, a, b, 1e-6);
    let w4 = merge::weight(1.0, 4.0, a, b, 1e-6);
    assert!((w4 / w1 / 16.0 - 1.0).abs() < 0.05);
    let w1 = merge::weight(1.0, 1.0, a, 0.0, 0.1);
    let w4 = merge::weight(1.0, 4.0, a, 0.0, 0.1);
    assert!((w4 / w1 / 4.0 - 1.0).abs() < 1e-3);
    assert_eq!(merge::ramp(0.5), 1.0);
    assert_eq!(merge::ramp(0.95), 0.0);
}

#[test]
fn rejects_wrong_input() {
    let fs = vec![frame(64, 64, 1.0, 0.0, 0.0, 1)];
    assert_eq!(ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)).err(), Some(Error::TooFewFrames(1)));
    let mut b = frame(64, 64, 2.0, 0.0, 0.0, 2);
    b.info.model = "Other".into();
    let fs = vec![frame(64, 64, 1.0, 0.0, 0.0, 1), b];
    assert!(matches!(ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)), Err(Error::Mismatch { .. })));
}

#[test]
fn cancel_stops_the_merge() {
    let fs = vec![frame(128, 96, 0.5, 0.0, 0.0, 1), frame(128, 96, 1.0, 0.0, 0.0, 2)];
    let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)).unwrap();
    let cancel = AtomicBool::new(true);
    let p = |_: f32| {};
    assert_eq!(merge::full(&mut plan, &Control { cancel: &cancel, progress: &p }).err(), Some(Error::Cancelled));
}

#[test]
fn exif_deviation_falls_back_with_warning() {
    let (w, h) = (256, 192);
    // the bright frame claims 4× in EXIF but is really 2× brighter (light changed)
    let mut b = frame(w, h, 2.0, 0.0, 0.0, 5);
    b.exif = Some(4.0 / 64.0 / 64.0); // t = k/64 s at f/8
    let fs = vec![frame(w, h, 1.0, 0.0, 0.0, 4), b];
    let plan = ctl_run(|c| plan_from_frames(fs, &Settings { reference: Some(0), align: false, ..Default::default() }, c)).unwrap();
    assert!((plan.k[1] - 4.0).abs() < 1e-9, "{:?} {:?}", plan.k, plan.steps);
    assert!(plan.warnings.iter().any(|w| matches!(w, Warning::ExifDeviation { .. })));
}

/// Without a measurement (frames too small for one) and without an exposure time the step is unknown: factor 1, and
/// the warning says so instead of naming an EXIF fallback that did not happen. With EXIF the old warning stays.
#[test]
fn unknown_exposure_is_named_honestly() {
    let pair = |exif: bool| {
        let mut fs = vec![frame(36, 36, 1.0, 0.0, 0.0, 6), frame(36, 36, 2.0, 0.0, 0.0, 7)];
        if !exif {
            for f in &mut fs {
                f.info.exif.exposure_time = None;
                f.exif = None;
            }
        }
        ctl_run(|c| plan_from_frames(fs, &Settings { reference: Some(0), align: false, ..Default::default() }, c)).unwrap()
    };
    let plan = pair(false);
    assert_eq!(plan.steps.len(), 1);
    assert!(plan.steps[0].measured.is_none() && plan.steps[0].exif.is_none() && !plan.steps[0].fell_back, "{:?}", plan.steps);
    assert_eq!(plan.k, vec![1.0, 1.0]);
    assert!(plan.warnings.iter().any(|w| matches!(w, Warning::ExposureUnknown { dark: 0, bright: 1 })), "{:?}", plan.warnings);
    assert!(!plan.warnings.iter().any(|w| matches!(w, Warning::NotMeasured { .. })), "{:?}", plan.warnings);
    let plan = pair(true);
    assert!(plan.steps[0].fell_back && (plan.k[1] - 2.0).abs() < 1e-6, "{:?} {:?}", plan.steps, plan.k);
    assert!(plan.warnings.iter().any(|w| matches!(w, Warning::NotMeasured { dark: 0, bright: 1 })), "{:?}", plan.warnings);
    assert!(!plan.warnings.iter().any(|w| matches!(w, Warning::ExposureUnknown { .. })), "{:?}", plan.warnings);
}

/// The aperture is missing in one frame of the bracket only: counted as f/1 that frame would look 64× brighter in
/// EXIF and push the correct measurement aside. The aperture is left out for the whole bracket instead.
#[test]
fn aperture_missing_in_single_frames() {
    let (w, h) = (256, 192);
    let mut b = frame(w, h, 2.0, 0.0, 0.0, 9);
    b.info.exif.aperture = None;
    b.exif = frames::exif_exposure(&b.info); // as the loader sets it
    let a = frame(w, h, 1.0, 0.0, 0.0, 8);
    assert!((b.exif.unwrap() / a.exif.unwrap() - 128.0).abs() < 1e-6, "the distorted ratio the fix is about");
    let plan = ctl_run(|c| plan_from_frames(vec![a, b], &Settings { reference: Some(0), align: false, ..Default::default() }, c)).unwrap();
    let s = &plan.steps[0];
    assert!((s.exif.unwrap() - 0.5).abs() < 1e-9 && !s.fell_back, "{:?}", plan.steps);
    assert!((plan.k[1] - 2.0).abs() < 0.03, "{:?}", plan.k);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    // all frames with, or all without, an aperture stay as they are
    let mut fs = vec![frame(64, 64, 1.0, 0.0, 0.0, 1), frame(64, 64, 2.0, 0.0, 0.0, 2)];
    let before: Vec<_> = fs.iter().map(|f| f.exif).collect();
    assert!(!frames::harmonize_exif(&mut fs));
    assert_eq!(before, fs.iter().map(|f| f.exif).collect::<Vec<_>>());
    for f in &mut fs {
        f.info.exif.aperture = None;
    }
    assert!(!frames::harmonize_exif(&mut fs));
}

#[test]
fn store_roundtrip() {
    let px: Vec<[f32; 3]> = (0..1000).map(|i| [i as f32 * 0.01, 0.18, 40.0 / (1 + i) as f32]).collect();
    let dir = std::env::temp_dir().join(format!("hdr-store-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.hdrc");
    let nm = raw::noise::NoiseModel { a: [1e-4; 3], b: [1e-8; 3] };
    store::write(&p, &Merged { px: px.clone(), w: 50, h: 20, noise: nm }).unwrap();
    let back = store::read(&p).unwrap();
    assert_eq!((back.w, back.h), (50, 20));
    assert_eq!(back.noise, nm);
    for (a, b) in px.iter().zip(&back.px) {
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() <= a[c].abs() * 1e-3 + 1e-7, "{a:?} {b:?}");
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Values beyond the half-float range are stored as the largest finite value, never as `inf` or NaN.
#[test]
fn store_clamps_to_the_half_range() {
    let dir = std::env::temp_dir().join(format!("hdr-store-clamp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.hdrc");
    let nm = raw::noise::NoiseModel { a: [1e-4; 3], b: [1e-8; 3] };
    let px = vec![[65504.0, 65519.9, 65520.0], [70000.0, 1e9, f32::MAX], [f32::INFINITY, f32::NEG_INFINITY, -1e9], [f32::NAN, 60000.0, 0.18]];
    store::write(&p, &Merged { px, w: 4, h: 1, noise: nm }).unwrap();
    let back = store::read(&p).unwrap();
    assert!(back.px.iter().flatten().all(|v| v.is_finite()), "{:?}", back.px);
    assert_eq!(back.px[0], [store::MAX_VALUE; 3]);
    assert_eq!(back.px[1], [store::MAX_VALUE; 3]);
    assert_eq!(back.px[2], [store::MAX_VALUE, -store::MAX_VALUE, -store::MAX_VALUE]);
    assert_eq!(back.px[3][0], 0.0);
    assert!((back.px[3][1] - 60000.0).abs() < 32.0 && (back.px[3][2] - 0.18).abs() < 1e-4, "{:?}", back.px[3]);
    std::fs::remove_dir_all(&dir).ok();
}

/// Writers of the same file use temporary files of their own: written at the same time every result is complete, and
/// neither a success nor a failure leaves a temporary file behind.
#[test]
fn store_writers_do_not_share_a_temporary_file() {
    let dir = std::env::temp_dir().join(format!("hdr-store-tmp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("f1-0123456789abcdef-preview.hdrc");
    let nm = raw::noise::NoiseModel { a: [1e-4; 3], b: [1e-8; 3] };
    let m = Merged { px: (0..200_000).map(|i| [(i % 977) as f32 * 0.01, 0.18, (i % 31) as f32]).collect(), w: 500, h: 400, noise: nm };
    std::thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| {
                for _ in 0..4 {
                    store::write(&p, &m).unwrap();
                    let back = store::read(&p).unwrap();
                    assert_eq!((back.w, back.h, back.px.len()), (500, 400, 200_000));
                }
            });
        }
    });
    // a failing write (the target is a directory: the rename fails) removes its temporary file
    let blocked = dir.join("f2-0123456789abcdef.hdrc");
    std::fs::create_dir_all(blocked.join("x")).unwrap();
    assert!(store::write(&blocked, &Merged { px: vec![[0.5; 3]; 4], w: 2, h: 2, noise: nm }).is_err());
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    assert_eq!(names.len(), 2, "{names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Temporary files of interrupted writers go once they are old; young ones (a writer may be at work) and everything
/// that is no temporary file stay.
#[test]
fn stale_temporary_files_are_removed() {
    let dir = std::env::temp_dir().join(format!("hdr-store-stale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let make = |name: &str, aged: bool| {
        let p = dir.join(name);
        std::fs::write(&p, [0u8; 100]).unwrap();
        if aged {
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        }
        p
    };
    let gone = [make("f7-0123456789abcdef.4711-0.tmp", true), make("f7-0123456789abcdef-preview.tmp", true)];
    let kept = [make("f7-0123456789abcdef.4711-1.tmp", false), make("f7-0123456789abcdef.hdrc", true), make("f7-0123456789abcdef.json", true)];
    assert_eq!(store::remove_stale_tmp(&dir, store::STALE_TMP_AGE), 200);
    assert!(gone.iter().all(|p| !p.exists()));
    assert!(kept.iter().all(|p| p.exists()));
    assert_eq!(store::remove_stale_tmp(&dir, store::STALE_TMP_AGE), 0);
    assert_eq!(store::remove_stale_tmp(&dir.join("missing"), store::STALE_TMP_AGE), 0);
    std::fs::remove_dir_all(&dir).ok();
}

/// A damaged cache file never asks for more memory than its data: a header naming a huge image is refused or ends as
/// "truncated", a stream longer than the header says is refused; empty planes give the nominal clip.
#[test]
fn store_reader_is_bounded() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("hdr-store-bound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.hdrc");
    let nm = raw::noise::NoiseModel { a: [1e-4; 3], b: [1e-8; 3] };
    store::write(&p, &Merged { px: vec![[0.5; 3]; 200], w: 20, h: 10, noise: nm }).unwrap();
    let good = std::fs::read(&p).unwrap();
    let with_size = |w: u32, h: u32| {
        let mut d = good.clone();
        d[8..12].copy_from_slice(&w.to_le_bytes());
        d[12..16].copy_from_slice(&h.to_le_bytes());
        std::fs::File::create(&p).unwrap().write_all(&d).unwrap();
        store::read(&p).map(|m| (m.w, m.h))
    };
    assert!(with_size(65535, 65535).is_err(), "more than the reader accepts");
    assert!(with_size(u32::MAX, u32::MAX).is_err());
    assert!(with_size(16000, 16000).is_err(), "200 pixels of data for 256 MP: truncated, no reservation");
    assert!(with_size(10, 10).is_err(), "more data than the header names");
    assert_eq!(with_size(20, 10).unwrap(), (20, 10));
    // values below black survive (the mean of deep shadows), values that are no numbers become black
    store::write(&p, &Merged { px: vec![[-0.002, 0.0, f32::NAN], [0.004, -0.001, 0.25]], w: 2, h: 1, noise: nm }).unwrap();
    let back = store::read(&p).unwrap();
    assert!((back.px[0][0] + 0.002).abs() < 2e-6 && back.px[0][1] == 0.0 && back.px[0][2] == 0.0, "{:?}", back.px[0]);
    assert!((back.px[1][1] + 0.001).abs() < 1e-6);
    assert!(((back.px[0][0] + back.px[1][0]) / 2.0 - 0.001).abs() < 2e-6, "the mean is kept");
    assert_eq!(frames::planar_clip(&[&[]]), [0.97; 3]);
    assert_eq!(frames::planar_clip(&[]), [0.97; 3]);
    std::fs::remove_dir_all(&dir).ok();
}

/// A moving object: the dark and bright frames see it elsewhere than the reference. Without ghost handling it leaves
/// a trace in the merge; with it the pixels where the reference shows plain scene stay at the scene value.
#[test]
fn ghost_handling_removes_the_moving_object() {
    let (w, h) = (480, 320);
    let make = || vec![frame_obj(w, h, 0.25, Some((100, 120)), 21), frame_obj(w, h, 1.0, Some((300, 120)), 22), frame_obj(w, h, 4.0, Some((200, 120)), 23)];
    let truth_err = |m: &Merged, x0: usize, y0: usize| -> f64 {
        // the place of the object in the dark frame: the reference shows the plain scene there
        let wb = [2.0f64, 1.0, 1.5];
        let (mut e, mut n) = (0.0, 0.0);
        for y in y0..y0 + 40 {
            for x in x0..x0 + 40 {
                let p = m.px[(y - 8) * m.w + (x - 8)];
                let t = scene(x as f64, y as f64, w)[1] * wb[1];
                e += ((p[1] as f64 / t).log2()).abs();
                n += 1.0;
            }
        }
        e / n
    };
    let run = |level: ghost::Level| {
        let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { ghost: level, ..Default::default() }, c)).unwrap();
        assert_eq!(plan.reference, 1);
        let maps = ghost::detect(&plan);
        let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
        (m, maps)
    };
    let (off, _) = run(ghost::Level::Off);
    let (std_, maps) = run(ghost::Level::Standard);
    let (fine, _) = run(ghost::Level::Fine);
    assert!(maps.area > 0.0, "motion area {}", maps.area);
    let (e_off, e_std, e_fine) = (truth_err(&off, 100, 120), truth_err(&std_, 100, 120), truth_err(&fine, 100, 120));
    assert!(e_std < e_off * 0.5 && e_std < 0.35, "off {e_off:.3} standard {e_std:.3} fine {e_fine:.3} EV");
    assert!(e_fine < e_off * 0.5, "off {e_off:.3} fine {e_fine:.3} EV");
    // a still scene: the ghost stage changes nothing (no motion found)
    let still = |level| {
        let fs = vec![frame_obj(w, h, 0.25, None, 31), frame_obj(w, h, 1.0, None, 32), frame_obj(w, h, 4.0, None, 33)];
        let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings { ghost: level, ..Default::default() }, c)).unwrap();
        let area = ghost::detect(&plan).area;
        (ctl_run(|c| merge::full(&mut plan, c)).unwrap(), area)
    };
    let (a, area_std) = still(ghost::Level::Standard);
    let (b, _) = still(ghost::Level::Off);
    assert!(area_std < 0.08, "false motion {area_std}");
    let diff = a.px.iter().zip(&b.px).filter(|(x, y)| ((x[1].max(1e-4) / y[1].max(1e-4)).log2()).abs() > 0.1).count();
    assert!(diff * 100 < a.px.len(), "{diff} of {} pixels differ by more than 0.1 EV", a.px.len());
}

/// A stroke forces one source frame: painting the reference's frame index over an area gives the reference there.
#[test]
fn painted_source_wins() {
    let (w, h) = (480, 320);
    let make = || vec![frame_obj(w, h, 0.25, None, 41), frame_obj(w, h, 1.0, None, 42), frame_obj(w, h, 4.0, None, 43)];
    let stroke = ghost::Stroke { frame: 0, erase: false, radius: 0.08, points: vec![[0.5, 0.5], [0.52, 0.5]] };
    let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { paint: vec![stroke], ..Default::default() }, c)).unwrap();
    let maps = ghost::detect(&plan);
    assert!(maps.paint.iter().any(|p| *p == 0));
    assert!(maps.active());
    let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
    // at the painted centre the result is the dark frame's value (a clipped bright frame would differ in noise)
    let (cx, cy) = (m.w / 2, m.h / 2);
    let wb = [2.0f64, 1.0, 1.5];
    let t = scene((cx + 8) as f64, (cy + 8) as f64, w)[1] * wb[1];
    assert!((m.px[cy * m.w + cx][1] as f64 / t).log2().abs() < 0.3);
}

/// Bands change the memory, never the result (also with the pyramid blend of the ghost stage).
#[test]
fn bands_give_the_same_result() {
    let (w, h) = (480, 320);
    let make = || vec![frame_obj(w, h, 0.25, Some((100, 120)), 21), frame_obj(w, h, 1.0, Some((300, 120)), 22), frame_obj(w, h, 4.0, Some((200, 120)), 23)];
    for level in [ghost::Level::Off, ghost::Level::Standard, ghost::Level::Fine] {
        let run = |bands: usize| {
            let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { ghost: level, ..Default::default() }, c)).unwrap();
            ctl_run(|c| merge::full_banded(&mut plan, c, None, Some(bands))).unwrap()
        };
        let (one, three) = (run(1), run(3));
        assert_eq!((one.w, one.h), (three.w, three.h));
        assert!(one.px == three.px, "level {level:?}: bands change the result");
    }
}

/// More bands than the rounded-up rows per band need (80 rows in 64 bands: 2 rows each, 40 bands): the surplus bands
/// are dropped instead of starting below the image.
#[test]
fn surplus_bands_are_dropped() {
    let make = || vec![frame(128, 96, 0.5, 0.0, 0.0, 31), frame(128, 96, 2.0, 0.0, 0.0, 32)];
    let run = |bands: usize| {
        let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { ghost: ghost::Level::Off, ..Default::default() }, c)).unwrap();
        ctl_run(|c| merge::full_banded(&mut plan, c, None, Some(bands))).unwrap()
    };
    let (one, many) = (run(1), run(64));
    assert_eq!((one.w, one.h), (112, 80));
    assert_eq!(many.px.len(), 112 * 80);
    assert!(one.px == many.px);
}

#[test]
fn memory_estimate_falls_with_bands() {
    let e = |b| merge::estimate_memory(24_000_000, 5, 24_000_000, true, b);
    assert!(e(1) > e(2) && e(2) > e(3));
    // measured 3.7 GB in one band
    assert!((3.3e9..4.1e9).contains(&(e(1) as f64)), "{}", e(1));
}

/// Recipe round trip: a new merge stores both strengths and reads them back; an old recipe (without them) reads as
/// fusion at full strength and no automatic tone, and stores exactly as before (its cache key stays).
#[test]
fn strengths_in_the_recipe() {
    let new = Settings::new_merge();
    assert_eq!(new.fusion_strength, FUSION_STRENGTH_DEFAULT);
    assert_eq!(new.auto_tone, Some(AutoTone { strength: AUTO_TONE_DEFAULT, applied: None }));
    let mut s = Settings { output: fusion::Output::Fusion, fusion_strength: 0.35, ..new.clone() };
    s.auto_tone = Some(AutoTone { strength: 0.8, applied: Some([0.25, -0.4, 0.3, -0.1, 0.05]) });
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
    // old recipes
    let old = r#"{"reference":1,"align":true,"ghost":"standard","output":"fusion"}"#;
    let o: Settings = serde_json::from_str(old).unwrap();
    assert_eq!(o.fusion_strength, 1.0);
    assert_eq!(o.auto_tone, None);
    assert_eq!(serde_json::to_string(&o).unwrap(), old, "an old recipe stores as before");
    assert_eq!(serde_json::from_str::<Settings>("{}").unwrap(), Settings::default());
    // the linear merge does not depend on either strength
    assert_eq!(serde_json::to_string(&s.base()).unwrap(), serde_json::to_string(&Settings { output: fusion::Output::Fusion, ..Settings::default() }.base()).unwrap());
}

/// The last sensor column and row take every frame (a frame lying exactly on the reference was left out there: with a
/// clipped reference a 1 px line of its clip value ran along the right and bottom edge). Full resolution and preview.
#[test]
fn the_last_column_and_row_take_every_frame() {
    let (w, h) = (240, 160);
    let make = || {
        let mut fs = vec![frame(w, h, 1.0 / 16.0, 0.0, 0.0, 61), frame(w, h, 1.0, 0.0, 0.0, 62), frame(w, h, 4.0, 0.0, 0.0, 63)];
        for f in &mut fs {
            // the whole sensor is the picture
            f.info.crop = None;
        }
        fs
    };
    // the brightest frame is the reference: it clips at the bright (right) end of the scene
    let settings = Settings { reference: Some(2), align: false, ghost: ghost::Level::Off, ..Default::default() };
    let mut plan = ctl_run(|c| plan_from_frames(make(), &settings, c)).unwrap();
    assert_eq!((plan.crop.w, plan.crop.h), (w, h));
    let pv = ctl_run(|c| merge::preview(&plan, 4096, c)).unwrap();
    let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
    assert_eq!((m.w, m.h), (w, h));
    // mean green of a column / a row
    let col = |im: &Merged, x: usize| (0..im.h).map(|y| im.px[y * im.w + x][1] as f64).sum::<f64>() / im.h as f64;
    let row = |im: &Merged, y: usize, from: usize| (from..im.w).map(|x| im.px[y * im.w + x][1] as f64).sum::<f64>() / (im.w - from) as f64;
    for (name, im) in [("full", &m), ("preview", &pv)] {
        let (last, before) = (col(im, im.w - 1), col(im, im.w - 2));
        assert!((last / before).log2().abs() < 0.3, "{name}: last column {last}, the one before {before}");
        // the clipped part of the last row (the right fifth)
        let from = im.w * 4 / 5;
        let (last, before) = (row(im, im.h - 1, from), row(im, im.h - 2, from));
        assert!((last / before).log2().abs() < 0.3, "{name}: last row {last}, the one before {before}");
    }
}

/// An area taken from a frame that does not cover all of it: beyond the edge of the frame the reference shows – at
/// full resolution as in the preview (before, the full resolution kept the merge there).
#[test]
fn an_area_beyond_its_frame_shows_the_reference() {
    let (w, h) = (240, 160);
    // the dark frame is shifted by 30 px: its left edge covers nothing of the reference
    let fs = vec![frame(w, h, 0.25, 30.0, 0.0, 71), frame(w, h, 1.0, 0.0, 0.0, 72)];
    let plan = ctl_run(|c| plan_from_frames(fs, &Settings { reference: Some(1), ..Default::default() }, c)).unwrap();
    let (tx, _, _, _) = plan.warps[0].describe(w, h);
    assert!((tx + 30.0).abs() < 0.5, "shift {tx}");
    let geo = plan.geometry();
    // the left edge of the picture: the frame starts 30 px after it (22 px of the crop, which starts at 8)
    let rect = [0, 20, 48, 32];
    let mut patch = regions::full_patch(&plan.frames[0], &geo, 0, rect);
    let reference = regions::full_patch(&plan.frames[1], &geo, 1, rect);
    let uncovered: Vec<usize> = (0..patch.len()).filter(|&i| !patch[i][1].is_finite()).collect();
    assert!(uncovered.len() > 10 * 32 && uncovered.len() < patch.len(), "{} of {} uncovered", uncovered.len(), patch.len());
    assert!(reference.iter().all(|p| p[1].is_finite()));
    assert_eq!(regions::fill_uncovered(&mut patch, &reference), uncovered.len());
    assert!(patch.iter().all(|p| p.iter().all(|v| v.is_finite())));
    assert!(uncovered.iter().all(|&i| patch[i] == reference[i]));
    // the preview of that frame shows the reference there as well
    let frames = ctl_run(|c| merge::preview_frames(&plan, 4096, c)).unwrap();
    let pw = geo.crop[2] / 2;
    let (x, y) = (2, 20);
    assert_eq!(frames[0][y * pw + x], frames[1][y * pw + x]);
    assert_ne!(frames[0][y * pw + pw / 2], frames[1][y * pw + pw / 2], "where it covers, the frame shows itself");
}

/// A scene as [`frame`], seen by a camera turned by a quarter (the sensor is the same size, the picture lies across).
fn frame_turned(w: usize, h: usize, k: f64, seed: u64) -> frames::Frame {
    let mut rng = Rng(seed);
    let (a, b) = (2e-5, 1e-7);
    let mut m = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = RGGB[y % 6][x % 6] as usize;
            let v = scene(y as f64 * w as f64 / h as f64, (w - 1 - x) as f64, w)[c] * k;
            let n = (a * v + b).sqrt() * rng.g();
            m[y * w + x] = (v + n).clamp(0.0, 1.0) as f32;
        }
    }
    let mut f = frames::from_mosaic(format!("turned{k}.raw").into(), info(w, h, (k / 64.0) as f32), m, RGGB);
    f.info.orientation = 6;
    f
}

/// Portrait and landscape in one bracket (same sensor size) are refused – when the pictures confirm what the
/// orientation flag says. The flag alone is not trusted: a camera pointing down sets it at random.
#[test]
fn portrait_and_landscape_do_not_merge() {
    let (w, h) = (480, 320);
    for align in [true, false] {
        let s = Settings { align, ..Default::default() };
        let fs = vec![frame(w, h, 0.25, 0.0, 0.0, 81), frame(w, h, 1.0, 0.0, 0.0, 82), frame_turned(w, h, 4.0, 83)];
        match ctl_run(|c| plan_from_frames(fs, &s, c)) {
            Err(Error::Turned { path }) => assert_eq!(path, std::path::PathBuf::from("turned4.raw")),
            Err(e) => panic!("align {align}: {e}"),
            Ok(_) => panic!("align {align}: a turned frame was merged"),
        }
        // the flag differs, the pictures lie on each other: one bracket
        let mut fs = vec![frame(w, h, 0.25, 2.0, -1.0, 84), frame(w, h, 1.0, 0.0, 0.0, 85), frame(w, h, 4.0, -3.0, 1.0, 86)];
        fs[2].info.orientation = 8;
        let plan = ctl_run(|c| plan_from_frames(fs, &s, c)).unwrap();
        assert!(!plan.warnings.iter().any(|w| matches!(w, Warning::AlignmentRejected { .. })), "{:?}", plan.warnings);
    }
    assert_eq!(Error::Turned { path: "/x/IMG_4.CR2".into() }.to_string(), "IMG_4.CR2: other orientation (portrait and landscape mixed)");
}

/// The plausibility check of a measured alignment: what a bracket can move passes, the rest does not; frames that
/// show the same picture agree, frames of different pictures do not.
#[test]
fn alignment_is_checked_for_plausibility() {
    use align::Warp;
    let (w, h) = (6000, 4000);
    let s = w.max(h) as f64 / 2.0;
    let shift = |px: f64| Warp { p: [0.0, 0.0, px / s, 0.0, 0.0, 0.0, 0.0, 0.0] };
    assert!(Warp::default().plausible(w, h));
    assert!(shift(300.0).plausible(w, h), "a hand-held bracket");
    assert!(!shift(3000.0).plausible(w, h), "half the picture");
    assert!(!Warp { p: [f64::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0] }.plausible(w, h));
    assert!(!Warp { p: [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0] }.plausible(w, h), "scaled by half");
    let a = 30f64.to_radians();
    assert!(!Warp { p: [a.cos() - 1.0, -a.sin(), 0.0, a.sin(), a.cos() - 1.0, 0.0, 0.0, 0.0] }.plausible(w, h), "turned by 30°");
    // agreement of the proxies
    let (w, h) = (480, 320);
    let (r, same, other) = (frame(w, h, 1.0, 0.0, 0.0, 91), frame(w, h, 4.0, 0.0, 0.0, 92), frame_turned(w, h, 4.0, 93));
    let (lo, hi) = (0.004f32, 0.2f32);
    let rr = r.g.log_clamped(1.0, lo, hi);
    let agree = |f: &frames::Frame| align::agreement(&rr, &f.g.log_clamped(4.0, lo, hi), &Warp::default(), lo, hi);
    assert!(agree(&same).unwrap() > 0.9, "{:?}", agree(&same));
    assert!(agree(&other).is_none_or(|a| a < 0.35), "{:?}", agree(&other));
    // an ordinary bracket is aligned as before, nothing is rejected
    let fs = vec![frame(w, h, 0.25, 3.0, -2.0, 11), frame(w, h, 1.0, 0.0, 0.0, 12), frame(w, h, 4.0, -4.5, 1.5, 13)];
    let plan = ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)).unwrap();
    assert!(plan.warnings.iter().all(|w| !matches!(w, Warning::AlignmentRejected { .. })), "{:?}", plan.warnings);
    assert!(!plan.warps[0].is_identity() && !plan.warps[2].is_identity());
}
