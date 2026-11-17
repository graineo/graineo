//! A learned look (`rawdev::learn`) is the standard of its camera and style: switched on, it replaces the built-in
//! hue table and the per-camera saturation (never both), switched off the standard development is unchanged.
//! Needs a non-Sigma RAW with a readable picture style in `GRAINEO_TEST_RAW_DIR` (skipped otherwise).

use develop::Settings;
use rawdev::{learn, RawDoc};

fn raw_with_style() -> Option<RawDoc> {
    let dir = std::env::var_os("GRAINEO_TEST_RAW_DIR")?;
    let mut files: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
    files.sort();
    // preferably a camera with a built-in hue table (then plain and standard development differ)
    let docs: Vec<RawDoc> = files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| !e.eq_ignore_ascii_case("x3f")))
        .filter_map(|p| RawDoc::open(&p, &|| true).ok().flatten())
        .filter(|d| !d.finished && d.learn_key.is_some())
        .collect();
    let i = docs.iter().position(|d| raw::development::camera_look(&d.info.maker, &d.info.model).is_some()).unwrap_or(0);
    docs.into_iter().nth(i)
}

fn render(doc: &RawDoc, s: &Settings) -> Vec<u16> {
    let st = doc.preview_stage(s);
    doc.render_stage(&st, s).data
}

#[test]
fn learned_look_replaces_the_standard_corrections_and_never_adds_to_them() {
    let Some(doc) = raw_with_style() else {
        eprintln!("GRAINEO_TEST_RAW_DIR not set or without a suitable RAW: skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("graineo-learned-look-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    learn::set_dir(Some(dir.clone()));
    let key = doc.learn_key.clone().unwrap();
    let s = Settings::default();
    let current = render(&doc, &s);
    doc.plain.store(true, std::sync::atomic::Ordering::Relaxed);
    let plain = render(&doc, &s);
    doc.plain.store(false, std::sync::atomic::Ordering::Relaxed);
    if raw::development::camera_look(&doc.info.maker, &doc.info.model).is_some() {
        assert_ne!(plain, current, "the hue table acts in the standard development");
    }
    // a learned look that changes nothing: the development equals the plain one (no hue table, no saturation factor)
    let n = 5;
    let zero = raw::looks::Look::from_table(&key, r#"{"learned":true,"interpolation":"tetrahedral"}"#, n, vec![[0.0; 3]; n * n * n]).unwrap();
    learn::install(&key, &zero).unwrap();
    assert!(doc.learned_look().is_some());
    let max_diff = |a: &[u16], b: &[u16]| a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
    assert!(max_diff(&render(&doc, &s), &plain) <= 1, "learned identity look differs from the plain development");
    // switched off: the standard development of before
    learn::set(&key, false, 1.0).unwrap();
    assert!(doc.learned_look().is_none());
    assert_eq!(render(&doc, &s), current);
    // "neutral" chosen by name is the unedited state as well
    learn::set(&key, true, 1.0).unwrap();
    let mut neutral = s.clone();
    neutral.raw.look = "neutral".into();
    assert!(max_diff(&render(&doc, &neutral), &plain) <= 1, "\"neutral\" is the unedited state as well");
    learn::reset(&key).unwrap();
    assert_eq!(render(&doc, &s), current);
    learn::set_dir(None);
    let _ = std::fs::remove_dir_all(&dir);
}
