//! Assigned lens profile (`LensOptions.pinned`): the reference wins over the automatic choice by name; a missing
//! profile or "none" leaves the own profiles out (camera/lensfun/none follow). Needs a free RAW of the test pictures
//! (`GRAINEO_TEST_ARW`, or `testbilder/signature-edits`); skipped without one. One test: it sets environment variables.

use std::path::PathBuf;

fn sample() -> Option<PathBuf> {
    let c = [
        std::env::var("GRAINEO_TEST_ARW").unwrap_or_default(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../testbilder/signature-edits/04-DSC0135.ARW").into(),
    ];
    c.iter().map(PathBuf::from).find(|p| p.is_file())
}

#[test]
fn pinned_profile_wins_missing_and_none_leave_own_out() {
    let Some(file) = sample() else {
        eprintln!("test image not present – skipped");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("raw-lens-pinned-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("GRAINEO_LENS_BACKUP", &tmp);
    let rf = raw::RawFile::open(&file).expect("open");
    let i = &rf.info;
    let (w, h) = (i.crop.map_or(i.width, |c| c.w), i.crop.map_or(i.height, |c| c.h));
    let vig = raw::lcp::Vignette { fx: 1.0, fy: 1.0, cx: 0.5, cy: 0.5, a: [-0.5, 0.1, 0.0] };
    let profile = raw::lcp::Profile { name: "Pin".into(), crop: 1.0, entries: vec![raw::lcp::Entry { focal_length: i.exif.focal_length.unwrap_or(35.0) as f64, distance: 1000.0, av: 4.0, geo: None, ca: None, vig: Some(vig) }] };
    // manual lens: the profile has no lens name and the photo is not found by name
    let mut p = lensprof::OwnProfile::new(&i.maker, &i.model, "Handobjektiv ohne Namen", "Test", profile);
    p.name = "Handobjektiv".into();
    let r = lensprof::store::save_new(p).unwrap();
    let source = |pinned: Option<&str>| {
        let o = raw::lens::LensOptions { adobe: false, lensfun: false, backup_enabled: false, pinned: pinned.map(str::to_string), ..Default::default() };
        rf.lens_correction(w, h, &o).and_then(|c| c.vignette.map(|v| v.kind))
    };
    // without an assignment the profile of another lens name is not used
    assert_ne!(source(None), Some(raw::lens::Source::Custom));
    // assigned: used
    let pinned = r.to_string();
    assert_eq!(source(Some(&pinned)), Some(raw::lens::Source::Custom));
    // missing profile and "none": never an own profile
    assert_ne!(source(Some("dead@1")), Some(raw::lens::Source::Custom));
    assert_ne!(source(Some("")), Some(raw::lens::Source::Custom));
    let _ = std::fs::remove_dir_all(&tmp);
}
