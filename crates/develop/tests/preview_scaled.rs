//! `Preview::scaled`: smaller scale levels of a finished preview.
//! Skips silently when the test file is missing.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use develop::session::Preview;
use develop::{Session, Settings};

fn file() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("GRAINEO_TEST_MERRILL_DIR")?).join("DP2M5418.X3F");
    p.exists().then_some(p)
}

fn developed(path: &PathBuf) -> (Session, Preview) {
    let session = Session::open(path).expect("open file");
    let (small, factor) = session.preview_linear(2400);
    let p = Preview::developed(small, factor, &session.cal, &Settings::default());
    (session, p)
}

fn k_for(p: &Preview, edge: usize) -> usize {
    p.lin.width.max(p.lin.height).div_ceil(edge)
}

#[test]
fn scaled_divides_size_and_keeps_factor() {
    let Some(path) = file() else { return };
    let (s, p) = developed(&path);
    let k = k_for(&p, 600);
    let q = p.scaled(&s.cal, 600).expect("scaled");
    assert_eq!(q.lin.width, p.lin.width / k);
    assert_eq!(q.lin.height, p.lin.height / k);
    assert_eq!(q.factor, p.factor * k);
}

#[test]
fn scaled_none_when_small_enough() {
    let Some(path) = file() else { return };
    let (s, p) = developed(&path);
    assert!(p.scaled(&s.cal, 100_000).is_none());
}

#[test]
fn scaled_keeps_mean_light() {
    let Some(path) = file() else { return };
    let (s, p) = developed(&path);
    let q = p.scaled(&s.cal, 600).unwrap();
    let mean = |d: &[f32], c: usize| d.chunks_exact(3).map(|x| x[c] as f64).sum::<f64>() / (d.len() / 3) as f64;
    for c in 0..3 {
        let (a, b) = (mean(&p.lin.data, c), mean(&q.lin.data, c));
        assert!((a - b).abs() / a.abs().max(1e-9) < 0.005, "Kanal {c}: {a} gegen {b}");
    }
}

#[test]
fn scaled_spp_keeps_key_and_detail() {
    let Some(path) = file() else { return };
    let session = Session::open(&path).expect("open file");
    let key = Settings::default().spp_key(&path);
    let counts = develop::spp::working_key(&path, &key, false, &AtomicBool::new(false)).expect("spp base");
    let matrix = session.cal.mode(key.mode.as_deref()).cam_to_srgb;
    let base = develop::session::SppBase::new(key.clone(), counts);
    let p = Preview::from_spp(&base, matrix, 2400);
    let q = p.scaled(&session.cal, 600).expect("scaled");
    assert_eq!(q.spp.as_ref().map(|s| &s.0), Some(&key));
    assert!(q.lin.detail.is_some());
}

#[test]
fn scaled_renders_at_new_size() {
    let Some(path) = file() else { return };
    let (s, p) = developed(&path);
    let q = p.scaled(&s.cal, 600).unwrap();
    let img = q.render(&s.cal, &Settings::default());
    assert_eq!((img.width, img.height), (q.lin.width, q.lin.height));
}
