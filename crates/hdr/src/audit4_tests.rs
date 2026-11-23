//! Audit round 4: targeted checks of the merge core. Tests marked `#[ignore = "audit4: …"]` assert the expected
//! (correct) behaviour and fail today; run them with `-- --ignored`. The others are sanity checks that pass.

use super::*;
use crate::tests::{info, RGGB};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

const XTRANS: [[u8; 6]; 6] = [[1, 2, 1, 1, 0, 1], [0, 1, 0, 2, 1, 2], [1, 2, 1, 1, 0, 1], [1, 0, 1, 1, 2, 1], [2, 1, 2, 0, 1, 0], [1, 0, 1, 1, 2, 1]];

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

/// Frame of `scene` (camera RGB before white balance at exposure 1) at exposure `k`, Poisson-Gauss noise, clipped at 1.
fn mk(w: usize, h: usize, k: f64, seed: u64, pattern: [[u8; 6]; 6], noisy: bool, scene: &dyn Fn(f64, f64) -> [f64; 3]) -> frames::Frame {
    let mut rng = Rng(seed);
    let (a, b) = (2e-5, 1e-7);
    let mut m = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = pattern[y % 6][x % 6] as usize;
            let v = scene(x as f64, y as f64)[c] * k;
            let n = if noisy { (a * v + b).sqrt() * rng.g() } else { 0.0 };
            m[y * w + x] = (v + n).clamp(0.0, 1.0) as f32;
        }
    }
    let mut i = info(w, h, (k / 64.0) as f32);
    if pattern == XTRANS {
        i.sensor = raw::Sensor::XTrans;
        i.cfa_size = (6, 6);
    }
    frames::from_mosaic(format!("a{k}.raw").into(), i, m, pattern)
}

/// Wide scene: 9 EV gradient with texture (as the scene of the main tests).
fn wide(w: usize) -> impl Fn(f64, f64) -> [f64; 3] {
    move |x, y| {
        let t = x / w as f64;
        let base = (2f64).powf(-7.0 + 9.0 * t) * (1.0 + 0.25 * (x * 0.21).sin() * (y * 0.17).cos());
        [0.45 * base, base, 0.65 * base]
    }
}

fn ctl_run<T>(f: impl FnOnce(&Control) -> T) -> T {
    let cancel = AtomicBool::new(false);
    let p = |_: f32| {};
    f(&Control { cancel: &cancel, progress: &p })
}

fn finite(px: &[[f32; 3]]) -> bool {
    px.iter().all(|p| p.iter().all(|v| v.is_finite()))
}

// ─────────────────────────────────────────────── findings (expected behaviour, red today)

/// A smooth bright area that is NOT clipped (peak 0.6 of white) must not be taken for the clip level: the nominal
/// white stays. Today the per-frame maximum counts as "pile-up" as soon as 1/10 000 of a channel lies within 1 % of
/// it – true for any frame whose brightest area is smooth.
#[test]
#[ignore = "audit4: clip level from an unclipped smooth highlight"]
fn unclipped_smooth_highlight_is_not_a_clip_level() {
    let (w, h) = (480, 320);
    // sky-like gradient 0.2 … 0.6 (green), far from clipping
    let sky = move |x: f64, _y: f64| {
        let v = 0.2 + 0.4 * x / w as f64;
        [0.8 * v, v, 0.9 * v]
    };
    let f = mk(w, h, 1.0, 1, RGGB, true, &sky);
    let top = f.mosaic.iter().copied().fold(0f32, f32::max);
    eprintln!("audit4 clip: max of the mosaic {top:.3}, clip {:?}", f.clip);
    assert!(top < 0.7);
    assert_eq!(f.clip, [0.97; 3], "an unclipped frame keeps the nominal white");
}

/// Consequence of the false clip level: the brightest (best exposed, unclipped) part of the bright frame gets no
/// weight, the merge takes it from the dark frame alone and is noisier there than with the true clip level.
#[test]
#[ignore = "audit4: false clip level makes unclipped highlights noisy"]
fn unclipped_highlights_use_the_bright_frame() {
    let (w, h) = (480, 320);
    let sky = move |x: f64, _y: f64| {
        let v = 0.2 + 0.4 * x / w as f64;
        [0.8 * v, v, 0.9 * v]
    };
    let make = || vec![mk(w, h, 1.0 / 16.0, 2, RGGB, true, &sky), mk(w, h, 1.0, 3, RGGB, true, &sky)];
    let settings = Settings { reference: Some(1), align: false, ghost: ghost::Level::Off, ..Default::default() };
    // relative noise of green in the brightest tenth (columns), against the noise-free truth
    let noise = |m: &Merged| {
        let (mut s2, mut n) = (0f64, 0f64);
        for y in 4..m.h - 4 {
            for x in m.w * 9 / 10..m.w - 4 {
                let t = sky((x + 8) as f64, 0.0)[1];
                s2 += (m.px[y * m.w + x][1] as f64 / t - 1.0).powi(2);
                n += 1.0;
            }
        }
        (s2 / n).sqrt()
    };
    let mut plan = ctl_run(|c| plan_from_frames(make(), &settings, c)).unwrap();
    eprintln!("audit4 clip levels as measured: {:?} / {:?}", plan.frames[0].clip, plan.frames[1].clip);
    let as_is = noise(&ctl_run(|c| merge::full(&mut plan, c)).unwrap());
    let mut plan = ctl_run(|c| plan_from_frames(make(), &settings, c)).unwrap();
    for f in &mut plan.frames {
        f.clip = [0.97; 3];
    }
    let nominal = noise(&ctl_run(|c| merge::full(&mut plan, c)).unwrap());
    eprintln!("audit4 relative noise in the brightest tenth: as measured {as_is:.5}, with the nominal clip {nominal:.5} (×{:.2})", as_is / nominal);
    assert!(as_is < 1.3 * nominal, "noise ×{:.2} of what the bright frame gives", as_is / nominal);
}

/// Pixels saturated in every frame (the sun, a lamp) are white: no colour is known there. Today they keep the clipped
/// raw values of the darkest frame times the white balance – red and blue above green, i.e. magenta/pink – while a
/// single frame gets its blown colours neutralised by the development.
#[test]
fn highlights_blown_in_every_frame_are_neutral() {
    let (w, h) = (360, 240);
    let scene = move |x: f64, y: f64| {
        let v = 0.05 + 0.1 * x / w as f64;
        if (150.0..210.0).contains(&x) && (90.0..150.0).contains(&y) {
            [1000.0; 3]
        } else {
            [0.5 * v, v, 0.7 * v]
        }
    };
    for level in [ghost::Level::Off, ghost::Level::Standard] {
        let fs = vec![mk(w, h, 0.25, 4, RGGB, true, &scene), mk(w, h, 1.0, 5, RGGB, true, &scene), mk(w, h, 4.0, 6, RGGB, true, &scene)];
        let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings { align: false, ghost: level, ..Default::default() }, c)).unwrap();
        let headroom = plan.headroom();
        let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
        let p = m.px[(120 - 8) * m.w + 180 - 8];
        let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
        eprintln!("audit4 blown everywhere ({level:?}): pixel {p:?}, headroom {headroom}, max/min {:.2}", hi / lo);
        assert!(hi / lo < 1.15, "{level:?}: blown pixel is coloured: {p:?}");
    }
}

/// Without exposure times the frames are ordered and measured from the pictures. A scene with a wide range (the case
/// HDR is for) must still give the right factors and the middle exposure as reference.
#[test]
fn brackets_without_exif_are_measured_from_the_pictures() {
    let (w, h) = (480, 320);
    let truth = [4.0f64, 1.0, 0.25];
    let fs: Vec<frames::Frame> = truth
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let mut f = mk(w, h, *k, 10 + i as u64, RGGB, true, &wide(w));
            f.info.exif.exposure_time = None;
            f.exif = None;
            f
        })
        .collect();
    let plan = ctl_run(|c| plan_from_frames(fs, &Settings { align: false, ..Default::default() }, c)).unwrap();
    eprintln!("audit4 no EXIF: order {:?}, reference {}, k {:?}, warnings {:?}", plan.order, plan.reference, plan.k, plan.warnings);
    assert_eq!(plan.reference, 1, "the middle exposure is the reference");
    let kr = truth[plan.reference];
    for (i, kt) in truth.iter().enumerate() {
        assert!((plan.k[i] / (kt / kr)).log2().abs() < 0.1, "k[{i}] = {} (truth {})", plan.k[i], kt / kr);
    }
}

/// The exposure fusion lifts by at most 2 EV over the reference white (its own rule, `MAX_LIFT`): a dark merge stays
/// dark. Today a merge whose 99.9 % value lies below 0.25 is lifted until that value is white.
#[test]
#[ignore = "audit4: fusion lifts a dark merge without bound"]
fn fusion_lift_is_bounded() {
    let (w, h) = (128, 96);
    let px: Vec<[f32; 3]> = (0..w * h).map(|i| [0.004 + 0.004 * ((i % w) as f32 / w as f32); 3]).collect();
    let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
    let f = ctl_run(|c| fusion::fuse(&m, c)).unwrap();
    let mean = |m: &Merged| m.px.iter().map(|p| p[1] as f64).sum::<f64>() / m.px.len() as f64;
    let lift = (mean(&f) / mean(&m)).log2();
    eprintln!("audit4 fusion: dark merge (mean {:.4}) lifted by {lift:.2} EV to {:.3}", mean(&m), mean(&f));
    assert!(lift <= 2.2, "lifted by {lift:.2} EV");
}

/// One non-finite pixel in the merge (a defect upstream) must not destroy the whole fused image.
#[test]
#[ignore = "audit4: one NaN pixel poisons the whole fusion"]
fn fusion_contains_a_non_finite_pixel() {
    let (w, h) = (128, 96);
    let mut px: Vec<[f32; 3]> = (0..w * h).map(|i| [0.05 + 2.0 * ((i % w) as f32 / w as f32); 3]).collect();
    px[40 * w + 60] = [f32::NAN; 3];
    let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
    let f = ctl_run(|c| fusion::fuse(&m, c)).unwrap();
    let bad = f.px.iter().filter(|p| !p.iter().all(|v| v.is_finite())).count();
    eprintln!("audit4 fusion: 1 NaN pixel in, {bad} of {} non-finite pixels out", w * h);
    assert!(bad <= 1, "{bad} pixels are not finite");
}

/// A fusion strength that is not a number (a slider value gone wrong) falls back to a valid strength.
#[test]
#[ignore = "audit4: NaN fusion strength gives a NaN image"]
fn fusion_strength_nan_is_harmless() {
    let (w, h) = (16, 16);
    let m = Merged { px: vec![[0.3; 3]; w * h], w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
    let mut f = Merged { px: vec![[0.5; 3]; w * h], w, h, noise: m.noise };
    fusion::blend(&m, &mut f, f32::NAN);
    assert!(finite(&f.px), "{:?}", f.px[0]);
}

/// The progress of one call never runs backwards. With the fusion as output, `merge::full` reports 1.0 and then
/// starts again at 0 for the fusion.
#[test]
#[ignore = "audit4: progress of merge::full with fusion runs backwards"]
fn progress_is_monotonic_with_fusion() {
    let (w, h) = (240, 160);
    let fs = vec![mk(w, h, 0.25, 20, RGGB, true, &wide(w)), mk(w, h, 1.0, 21, RGGB, true, &wide(w))];
    let seen = Mutex::new(Vec::new());
    let cancel = AtomicBool::new(false);
    let p = |f: f32| seen.lock().unwrap().push(f);
    let ctl = Control { cancel: &cancel, progress: &p };
    let mut plan = plan_from_frames(fs, &Settings { align: false, output: fusion::Output::Fusion, ..Default::default() }, &Control { cancel: &cancel, progress: &|_| {} }).unwrap();
    merge::full(&mut plan, &ctl).unwrap();
    let v = seen.into_inner().unwrap();
    let back = v.windows(2).filter(|p| p[1] < p[0] - 1e-6).map(|p| (p[0], p[1])).collect::<Vec<_>>();
    eprintln!("audit4 progress: {v:?}");
    assert!(back.is_empty(), "runs backwards: {back:?}");
}

/// A crop that leaves no pixels (damaged crop data of a file) is an error or an empty result, never a panic.
#[test]
fn an_empty_crop_does_not_panic() {
    let (w, h) = (96, 64);
    let make = |crop: raw::Region| {
        let mut fs = vec![mk(w, h, 0.25, 30, RGGB, true, &wide(w)), mk(w, h, 1.0, 31, RGGB, true, &wide(w))];
        for f in &mut fs {
            f.info.crop = Some(crop);
        }
        fs
    };
    let s = Settings { align: false, ghost: ghost::Level::Off, ..Default::default() };
    let mut failed = Vec::new();
    for (name, crop) in [("beyond the sensor", raw::Region { x: w + 4, y: 0, w: 10, h: 10 }), ("one pixel wide", raw::Region { x: 10, y: 10, w: 1, h: 20 })] {
        let full = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut plan = ctl_run(|c| plan_from_frames(make(crop), &s, c)).unwrap();
            let _ = ctl_run(|c| merge::full(&mut plan, c));
        }));
        let preview = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let plan = ctl_run(|c| plan_from_frames(make(crop), &s, c)).unwrap();
            let _ = ctl_run(|c| merge::preview(&plan, 512, c));
        }));
        eprintln!("audit4 crop {name}: full panics {}, preview panics {}", full.is_err(), preview.is_err());
        if full.is_err() {
            failed.push(format!("{name}: full"));
        }
        if preview.is_err() {
            failed.push(format!("{name}: preview"));
        }
    }
    assert!(failed.is_empty(), "panics: {failed:?}");
}

/// A plan made with the alignment of an earlier plan tells the same about the alignment as that plan did: a frame
/// whose alignment was rejected (used as shot) is still named.
#[test]
fn reused_alignment_keeps_its_warnings() {
    let (w, h) = (480, 320);
    // the third frame shows another picture (mirrored scene): its alignment cannot be right
    let other = move |x: f64, y: f64| wide(w)(w as f64 - 1.0 - x, 97.0 - y);
    let make = || vec![mk(w, h, 0.25, 40, RGGB, true, &wide(w)), mk(w, h, 1.0, 41, RGGB, true, &wide(w)), mk(w, h, 4.0, 42, RGGB, true, &other)];
    let s = Settings::default();
    let first = ctl_run(|c| plan_from_frames(make(), &s, c)).unwrap();
    let rejected = |p: &Plan| p.warnings.iter().filter(|w| matches!(w, Warning::AlignmentRejected { .. })).count();
    eprintln!("audit4 reuse: first plan {:?}", first.warnings);
    assert!(rejected(&first) > 0, "premise: the first plan rejects an alignment ({:?})", first.warnings);
    let again = ctl_run(|c| plan_with_alignment(make(), &Settings { ghost: ghost::Level::Fine, ..s.clone() }, c, Some((&first.warps, &first.rejected)))).unwrap();
    eprintln!("audit4 reuse: plan with the known warps {:?}", again.warnings);
    assert_eq!(rejected(&again), rejected(&first));
}

/// A painted source (older recipes, `Settings::paint`) that does not cover the painted place must not leave black
/// pixels there.
#[test]
#[ignore = "audit4: painted frame beyond its edge gives black pixels"]
fn a_painted_frame_beyond_its_edge_is_not_black() {
    let (w, h) = (240, 160);
    let shifted = move |x: f64, y: f64| wide(w)(x + 30.0, y);
    let fs = vec![mk(w, h, 0.25, 50, RGGB, true, &shifted), mk(w, h, 1.0, 51, RGGB, true, &wide(w))];
    let stroke = ghost::Stroke { frame: 0, erase: false, radius: 0.04, points: vec![[0.03, 0.5]] };
    let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings { reference: Some(1), paint: vec![stroke], ..Default::default() }, c)).unwrap();
    let (tx, _, _, _) = plan.warps[0].describe(w, h);
    eprintln!("audit4 paint: shift of the painted frame {tx:.1} px");
    let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
    let p = m.px[(m.h / 2) * m.w + 4];
    eprintln!("audit4 paint: pixel at the painted, uncovered place {p:?}");
    assert!(p[1] > 0.0, "black: {p:?}");
}

/// Largest difference between [`regions::full_patch`] of the reference and the frame demosaiced as a whole (as the
/// merge does), over `rects`.
fn patch_difference(name: &str, pattern: [[u8; 6]; 6], rects: &[[usize; 4]]) -> f32 {
    let (w, h) = (300, 240);
    let wb = [2.0f32, 1.0, 1.5];
    let (dm, sensor) = if pattern == XTRANS { (raw::development::Demosaic::Markesteijn3, raw::Sensor::XTrans) } else { (raw::development::Demosaic::Rcd, raw::Sensor::Bayer) };
    let scene = move |x: f64, y: f64| {
        let v = 0.2 + 0.15 * (x * 0.37).sin() * (y * 0.29).cos() + 0.05 * ((x + y) * 0.9).sin();
        [0.5 * v, v, 0.7 * v]
    };
    let mut f = mk(w, h, 1.0, 60, pattern, true, &scene);
    f.info.crop = None;
    let smax = 2.0f32;
    let m: Vec<f32> = (0..w * h).map(|i| f.mosaic[i] * wb[pattern[(i / w) % 6][(i % w) % 6] as usize] / smax).collect();
    let whole: Vec<[f32; 3]> = raw::development::demosaic_mosaic(&m, w, h, pattern, dm, sensor).into_iter().map(|p| p.map(|v| v * smax)).collect();
    let geo = regions::Geometry { reference: 0, warps: vec![align::Warp::default()], k: vec![1.0], wb, crop: [0, 0, w, h], w, h };
    let mut worst = 0f32;
    for rect in rects {
        let p = regions::full_patch(&f, &geo, 0, *rect);
        let (mut here, mut at, mut bad) = (0f32, (0, 0), 0);
        for y in 0..rect[3] {
            for x in 0..rect[2] {
                let (a, b) = (p[y * rect[2] + x], whole[(rect[1] + y) * w + rect[0] + x]);
                let d = (0..3).map(|c| (a[c] - b[c]).abs()).fold(0f32, f32::max);
                if !(d <= here) {
                    (here, at) = (d, (x, y));
                }
                bad += !(d < 1e-4) as usize;
            }
        }
        eprintln!("audit4 patch {name} {rect:?}: largest difference {here:.6} at {at:?} (patch {:?}), {bad} of {} pixels differ", p[at.1 * rect[2] + at.0], rect[2] * rect[3]);
        worst = worst.max(here);
    }
    worst
}

/// An area pasted at full resolution shows its frame as the merge sees it: the patch (demosaiced on a cut of the
/// sensor) equals the frame demosaiced as a whole. Holds inside the picture and for areas of ordinary size at its edge.
#[test]
fn full_patch_equals_the_whole_frame_demosaic() {
    let rects = [[100, 80, 40, 30], [300 - 40, 240 - 40, 40, 40], [0, 0, 40, 40], [131, 77, 7, 5]];
    assert!(patch_difference("bayer", RGGB, &rects) < 1e-4);
    assert!(patch_difference("x-trans", XTRANS, &rects) < 1e-4);
    assert!(patch_difference("x-trans corner", XTRANS, &[[0, 0, 20, 20], [0, 100, 9, 30]]) < 1e-4);
}

/// The same for a small area in a corner of a Bayer sensor. Today the cut of the sensor is narrower than the Bayer
/// demosaic accepts there (it is not widened towards the inside when the sensor edge cuts it): the inside of the patch
/// is black.
#[test]
#[ignore = "audit4: small Bayer patch at the sensor edge is black"]
fn a_small_patch_at_the_sensor_edge_is_demosaiced() {
    assert!(patch_difference("bayer corner", RGGB, &[[0, 0, 20, 20], [0, 100, 9, 30]]) < 1e-4);
}

/// The noise model handed on with a fused image describes the fused image: the noise measured in its lifted shadows
/// is about what the model says for their value. Today the model of the linear merge is passed through unchanged.
#[test]
#[ignore = "audit4: fusion keeps the noise model of the linear merge"]
fn fused_image_carries_a_matching_noise_model() {
    let (w, h) = (256, 192);
    let (a, b) = (1e-4f64, 1e-7f64);
    let mut rng = Rng(77);
    let px: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let v: f64 = if i % w < w / 2 { 2.0 } else { 0.01 };
            std::array::from_fn(|_| (v + (a * v + b).sqrt() * rng.g()) as f32)
        })
        .collect();
    let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [a as f32; 3], b: [b as f32; 3] } };
    let f = ctl_run(|c| fusion::fuse(&m, c)).unwrap();
    assert_eq!((f.noise.a, f.noise.b), (m.noise.a, m.noise.b), "premise: the model is passed through");
    // noise of green in the dark half (differences of neighbours, free of slow gradients)
    let measure = |im: &Merged| {
        let (mut s, mut s2, mut n) = (0f64, 0f64, 0f64);
        for y in 8..h - 8 {
            for x in (w * 5 / 8..w - 9).step_by(2) {
                let (p, q) = (im.px[y * w + x][1] as f64, im.px[y * w + x + 1][1] as f64);
                s += p;
                s2 += (p - q) * (p - q) / 2.0;
                n += 1.0;
            }
        }
        (s / n, (s2 / n).sqrt())
    };
    let ((v0, s0), (v1, s1)) = (measure(&m), measure(&f));
    let model = (f.noise.a[1] as f64 * v1 + f.noise.b[1] as f64).sqrt();
    eprintln!("audit4 fusion noise: shadows {v0:.4} ± {s0:.5} → {v1:.4} ± {s1:.5}; the model says ± {model:.5} (×{:.2})", s1 / model);
    assert!(s1 < 1.5 * model, "noise ×{:.2} of the model", s1 / model);
}

/// The fused preview shows what the fused full resolution will show: fusing a downscaled merge and downscaling the
/// fused merge agree (display values).
#[test]
fn fused_preview_matches_the_fused_full_resolution() {
    let (w, h) = (768, 510);
    let (pw, ph) = (256, 170);
    let px: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let tex = 1.0 + 0.2 * (x * 40.0).sin() * (y * 31.0).cos();
            // bright sky with a sun, dark ground with a window of light
            let v = if y < 0.45 { 3.0 + 9.0 * (-((x - 0.7).powi(2) + (y - 0.2).powi(2)) * 30.0).exp() } else if (0.2..0.35).contains(&x) && (0.6..0.8).contains(&y) { 0.8 } else { 0.01 + 0.03 * x };
            [v * tex, 0.9 * v * tex, 0.8 * v * tex]
        })
        .collect();
    let noise = raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] };
    let full = Merged { px, w, h, noise };
    let small = Merged { px: raw::development::downscale(&full.px, w, h, pw, ph), w: pw, h: ph, noise };
    let a = ctl_run(|c| fusion::fuse(&small, c)).unwrap();
    let b = raw::development::downscale(&ctl_run(|c| fusion::fuse(&full, c)).unwrap().px, w, h, pw, ph);
    let (mut sum, mut worst) = (0f64, 0f32);
    for (p, q) in a.px.iter().zip(&b) {
        let d = (p[1].max(0.0).powf(1.0 / 2.2) - q[1].max(0.0).powf(1.0 / 2.2)).abs();
        sum += d as f64;
        worst = worst.max(d);
    }
    let mean = sum / b.len() as f64;
    eprintln!("audit4 fusion preview against full resolution (display values 0 … 1): mean difference {mean:.4}, largest {worst:.4}");
    assert!(mean < 0.02 && worst < 0.12, "mean {mean}, largest {worst}");
}

// ─────────────────────────────────────────────── sanity checks (green)

fn run_all(make: &dyn Fn() -> Vec<frames::Frame>, name: &str) {
    for level in [ghost::Level::Off, ghost::Level::Standard, ghost::Level::Fine] {
        for output in [fusion::Output::Linear, fusion::Output::Fusion] {
            for align in [false, true] {
                let s = Settings { ghost: level, output, align, fusion_strength: 0.7, ..Default::default() };
                let mut plan = ctl_run(|c| plan_from_frames(make(), &s, c)).unwrap_or_else(|e| panic!("{name}: {e}"));
                let tag = format!("{name} {level:?} {output:?} align {align}");
                assert!(plan.k.iter().all(|k| k.is_finite() && *k > 0.0), "{tag}: k {:?}", plan.k);
                assert!(plan.headroom().is_finite() && plan.headroom() > 0.0, "{tag}");
                let n = plan.result_noise();
                assert!(n.a.iter().chain(n.b.iter()).all(|v| v.is_finite() && *v >= 0.0) && n.b.iter().all(|v| *v > 0.0), "{tag}: noise {:?} {:?}", n.a, n.b);
                let pv = ctl_run(|c| merge::preview(&plan, 512, c)).unwrap();
                assert!(finite(&pv.px), "{tag}: preview");
                for f in ctl_run(|c| merge::preview_frames(&plan, 512, c)).unwrap() {
                    assert!(finite(&f), "{tag}: preview frame");
                }
                let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
                assert!(finite(&m.px), "{tag}: full");
                assert_eq!((m.w, m.h), (plan.crop.w, plan.crop.h));
            }
        }
    }
}

/// Degenerate brackets stay finite: black, saturated, identical frames, a gap of 10 EV, with and without EXIF.
#[test]
fn degenerate_brackets_stay_finite() {
    let (w, h) = (192, 128);
    let flat = |v: f64| move |_x: f64, _y: f64| [v; 3];
    let no_exif = |mut fs: Vec<frames::Frame>| {
        for f in &mut fs {
            f.info.exif.exposure_time = None;
            f.exif = None;
        }
        fs
    };
    let ks = [0.25, 1.0, 4.0];
    run_all(&|| ks.iter().map(|k| mk(w, h, *k, 1, RGGB, false, &flat(0.0))).collect(), "black");
    run_all(&|| ks.iter().map(|k| mk(w, h, *k, 1, RGGB, false, &flat(1e6))).collect(), "saturated");
    run_all(&|| no_exif(ks.iter().map(|k| mk(w, h, *k, 1, RGGB, false, &flat(0.0))).collect()), "black, no EXIF");
    run_all(&|| no_exif(ks.iter().map(|k| mk(w, h, *k, 1, RGGB, false, &flat(1e6))).collect()), "saturated, no EXIF");
    run_all(&|| (0..3).map(|_| mk(w, h, 1.0, 7, RGGB, true, &wide(w))).collect(), "identical");
    run_all(&|| vec![mk(w, h, 1.0 / 1024.0, 8, RGGB, true, &wide(w)), mk(w, h, 1.0, 9, RGGB, true, &wide(w))], "10 EV apart");
    run_all(&|| no_exif(vec![mk(w, h, 1.0 / 1024.0, 8, RGGB, true, &wide(w)), mk(w, h, 1.0, 9, RGGB, true, &wide(w))]), "10 EV apart, no EXIF");
    // a noise model without a Poisson part (fit slope 0) and a reference outside the frames
    let mut plan = ctl_run(|c| plan_from_frames(ks.iter().map(|k| mk(w, h, *k, 3, RGGB, true, &wide(w))).collect(), &Settings { reference: Some(17), ..Default::default() }, c)).unwrap();
    assert_eq!(plan.reference, 1);
    plan.noise = vec![(0.0, 1e-14); 3];
    let n = plan.result_noise();
    assert!(n.a.iter().all(|v| v.is_finite() && *v >= 0.0) && n.b.iter().all(|v| v.is_finite() && *v > 0.0), "{:?} {:?}", n.a, n.b);
    assert!(finite(&ctl_run(|c| merge::full(&mut plan, c)).unwrap().px));
}

/// Odd sensor sizes and crops at odd offsets, Bayer and X-Trans: no panic, the full result has the size of the crop.
#[test]
fn odd_sizes_and_crops() {
    for (pattern, w, h) in [(RGGB, 243usize, 165usize), (XTRANS, 245, 163), (RGGB, 97, 61)] {
        for crop in [None, Some(raw::Region { x: 7, y: 5, w: w - 20, h: h - 12 }), Some(raw::Region { x: 3, y: 1, w: w - 3, h: h - 1 })] {
            let make = || {
                let mut fs = vec![mk(w, h, 0.25, 70, pattern, true, &wide(w)), mk(w, h, 1.0, 71, pattern, true, &wide(w)), mk(w, h, 4.0, 72, pattern, true, &wide(w))];
                for f in &mut fs {
                    f.info.crop = crop;
                }
                fs
            };
            for level in [ghost::Level::Off, ghost::Level::Standard, ghost::Level::Fine] {
                let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { ghost: level, ..Default::default() }, c)).unwrap();
                let pv = ctl_run(|c| merge::preview(&plan, 4096, c)).unwrap();
                let maps = ghost::detect(&plan);
                let ov = ghost::overlay(&plan, &maps, pv.w, pv.h);
                assert_eq!(ov.len(), pv.w * pv.h);
                assert!(finite(&pv.px) && pv.w > 0 && pv.h > 0);
                let one = ctl_run(|c| merge::full_banded(&mut plan, c, None, Some(1))).unwrap();
                let (cw, ch) = crop.map_or((w, h), |c| (c.w, c.h));
                assert_eq!((one.w, one.h), (cw, ch));
                assert!(finite(&one.px));
                let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { ghost: level, ..Default::default() }, c)).unwrap();
                let five = ctl_run(|c| merge::full_banded(&mut plan, c, None, Some(5))).unwrap();
                assert!(one.px == five.px, "bands change the result ({w}×{h}, {crop:?}, {level:?})");
            }
        }
    }
}

/// The same input gives the same bits (plan and pixels), run after run.
#[test]
fn merge_is_deterministic() {
    let (w, h) = (360, 240);
    let moved = move |x: f64, y: f64| wide(w)(x + 3.3, y - 1.7);
    let run = || {
        let fs = vec![mk(w, h, 0.25, 80, RGGB, true, &moved), mk(w, h, 1.0, 81, RGGB, true, &wide(w)), mk(w, h, 4.0, 82, RGGB, true, &wide(w))];
        let mut plan = ctl_run(|c| plan_from_frames(fs, &Settings::default(), c)).unwrap();
        let pv = ctl_run(|c| merge::preview(&plan, 300, c)).unwrap();
        let (k, warps, noise) = (plan.k.clone(), plan.warps.clone(), plan.noise.clone());
        let m = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
        (k, warps, noise, pv.px, m.px, (m.noise.a, m.noise.b))
    };
    let (a, b) = (run(), run());
    assert!(a.0 == b.0 && a.1 == b.1 && a.2 == b.2, "plan differs");
    assert!(a.3.iter().zip(&b.3).all(|(x, y)| x.map(f32::to_bits) == y.map(f32::to_bits)), "preview differs");
    assert!(a.4.iter().zip(&b.4).all(|(x, y)| x.map(f32::to_bits) == y.map(f32::to_bits)), "full differs");
    assert!(a.5 == b.5);
}

/// Strokes with coordinates outside the picture, not-a-number or with absurd radii never panic.
#[test]
fn wild_strokes_do_not_panic() {
    let pts: Vec<Vec<[f32; 2]>> = vec![vec![[-5.0, 7.0]], vec![[f32::NAN, f32::NAN], [0.5, 0.5]], vec![[1e30, -1e30], [0.2, 0.2]], vec![[0.0, 0.0], [1.0, 1.0]], vec![[f32::INFINITY, 0.5]], vec![[0.999, 0.999]]];
    for radius in [0.0f32, -1.0, 0.05, 3.0, f32::INFINITY, f32::NAN] {
        for p in &pts {
            let area = regions::Area { frame: 1, strokes: vec![regions::AreaStroke { erase: false, radius, points: p.clone() }, regions::AreaStroke { erase: true, radius, points: vec![[0.1, 0.1]] }] };
            for (w, h) in [(200usize, 100usize), (1, 1), (3, 500)] {
                let _ = regions::raster(&area, w, h);
                if let Some((rect, m)) = regions::mask(&area, w, h) {
                    assert!(rect[0] + rect[2] <= w && rect[1] + rect[3] <= h && m.len() == rect[2] * rect[3]);
                    assert!(m.iter().all(|v| v.is_finite() && (-1e-4..=1.0 + 1e-4).contains(v)), "mask out of range");
                }
                let mut px = vec![[1.0f32; 3]; w * h];
                regions::apply(&mut px, w, h, &[area.clone()], &[], &mut |_, r| Some(vec![[f32::NAN, 2.0, 2.0]; r[2] * r[3]]));
                assert!(px.iter().all(|p| *p == [1.0; 3]), "a non-finite patch keeps the merge");
            }
            let s = ghost::Stroke { frame: 1, erase: false, radius, points: p.clone() };
            let _ = ghost::rasterize(&[s.clone()], 50, 25, 4, raw::Region { x: 3, y: 5, w: 190, h: 90 }, 3);
            let _ = regions::from_strokes(&[s]);
        }
    }
}

/// M3 of the cross-check: an area taken from the darkest frame over a place that is blown in every frame. The merge
/// (and with it the preview) shows that place neutral, because the darkest frame gets the highlight handling of a
/// single RAW; the pixels the area takes at full resolution ([`regions::full_patch`]) have to come from the frame
/// treated the same way ([`regions::for_patches`]) – from the frame as loaded they are tinted.
#[test]
fn an_area_from_the_darkest_frame_over_a_blown_place_is_neutral_at_full_resolution() {
    let (w, h) = (360, 240);
    let scene = move |x: f64, y: f64| {
        let v = 0.05 + 0.1 * x / w as f64;
        if (150.0..210.0).contains(&x) && (90.0..150.0).contains(&y) {
            [1000.0; 3]
        } else {
            [0.5 * v, v, 0.7 * v]
        }
    };
    let make = || vec![mk(w, h, 0.25, 4, RGGB, true, &scene), mk(w, h, 1.0, 5, RGGB, true, &scene), mk(w, h, 4.0, 6, RGGB, true, &scene)];
    let mut plan = ctl_run(|c| plan_from_frames(make(), &Settings { align: false, ghost: ghost::Level::Off, ..Default::default() }, c)).unwrap();
    let geo = plan.geometry();
    let dark = regions::darkest(&geo.k).unwrap();
    let merged = ctl_run(|c| merge::full(&mut plan, c)).unwrap();
    // the frames as the app loads them for the areas (fresh, untreated)
    let loaded = make();
    let rect = [geo.crop[2] / 2 - 48, geo.crop[3] / 2 - 48, 96, 96];
    let at = |px: &[[f32; 3]]| px[48 * 96 + 48];
    let spread = |p: [f32; 3]| p[0].max(p[1]).max(p[2]) / p[0].min(p[1]).min(p[2]).max(1e-6);
    let raw_patch = at(&regions::full_patch(&loaded[dark], &geo, dark, rect));
    let treated = regions::for_patches(&loaded[dark], &geo, dark).expect("the darkest frame is blown: it is treated");
    let patch = at(&regions::full_patch(&treated, &geo, dark, rect));
    let want = merged.px[(rect[1] + 48) * merged.w + rect[0] + 48];
    eprintln!("audit5 area over a blown place: untreated {raw_patch:?}, treated {patch:?}, merge {want:?}");
    assert!(spread(raw_patch) > 1.3, "premise: the frame as loaded is tinted there ({raw_patch:?})");
    assert!(spread(patch) < 1.15, "the area is tinted at full resolution: {patch:?}");
    for c in 0..3 {
        assert!((patch[c] - want[c]).abs() <= 0.02 * want[c].abs().max(1e-3), "area {patch:?} against the merge {want:?}");
    }
    // every other frame stays as loaded
    assert!(regions::for_patches(&loaded[plan.reference], &geo, plan.reference).is_none() || plan.reference == dark);
}
