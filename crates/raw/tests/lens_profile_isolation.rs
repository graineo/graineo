//! Test isolation of the lens-profile folder: with `GRAINEO_DATA_DIR` set, private LCP
//! backups and own profiles land in that folder and nothing is written into the real data folder. Up to 0.94
//! `lcp::backup_dir` ignored `GRAINEO_DATA_DIR`.
//!
//! One test function: it changes environment variables of the process.

use std::path::{Path, PathBuf};

fn listing(d: &Path) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    let mut v = Vec::new();
    let mut todo = vec![d.to_path_buf()];
    while let Some(p) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let q = e.path();
            if q.is_dir() {
                todo.push(q.clone());
            }
            v.push((q, e.metadata().and_then(|m| m.modified()).ok()));
        }
    }
    v.sort();
    v
}

fn profile() -> raw::lcp::Profile {
    let vig = raw::lcp::Vignette { fx: 1.0, fy: 1.0, cx: 0.5, cy: 0.5, a: [-0.5, 0.1, 0.0] };
    raw::lcp::Profile { name: "Isolation".into(), crop: 1.0, entries: vec![raw::lcp::Entry { focal_length: 58.0, distance: 1000.0, av: 4.0, geo: None, ca: None, vig: Some(vig) }] }
}

#[test]
fn data_dir_isolates_lens_profiles() {
    let real = sysdirs::app_data_dir().map(|d| d.join(sysdirs::APP).join("lens-profiles"));
    let before = real.as_deref().map(listing);
    let tmp = std::env::temp_dir().join(format!("raw-lens-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::remove_var("GRAINEO_LENS_BACKUP");
    std::env::set_var("GRAINEO_DATA_DIR", &tmp);

    assert_eq!(raw::lcp::backup_dir(), Some(tmp.join("lens-profiles")));
    assert_eq!(raw::lcp::custom_folders(), Some(tmp.join("lens-profiles/own")));

    // private LCP backup
    let header = raw::lcp::Header { path: PathBuf::from("/nirgends/Test.lcp"), make: "Test".into(), model: "Kamera".into(), lens: "Objektiv".into(), lens_pretty: String::new(), raw_bytes: true, crop: 1.0 };
    let b = raw::lcp::write_backup("Testhersteller", "Testkamera", Some("Testobjektiv 58mm"), &header, &profile(), true).unwrap();
    assert!(b.starts_with(&tmp), "{b:?}");
    assert!(raw::lcp::load_backup("Testhersteller", "Testkamera", Some("Testobjektiv 58mm")).is_some());

    // own profile from the measurement workflow, twice for a named lens (second = version 2), once manual
    let outcome = |lens: Option<&str>| raw::measure_run::Outcome {
        maker: "Testhersteller".into(),
        model: "Testkamera".into(),
        lens: lens.map(str::to_string),
        crop: 1.0,
        crop_known: false,
        shots: Vec::new(),
        geo: Vec::new(),
        vig: Vec::new(),
        notes: Vec::new(),
        profile: Some(profile()),
        ..Default::default()
    };
    let p1 = raw::measure_run::save(&outcome(Some("Testobjektiv 58mm"))).unwrap();
    let p2 = raw::measure_run::save(&outcome(Some("Testobjektiv 58mm"))).unwrap();
    let p3 = raw::measure_run::save(&outcome(None)).unwrap();
    for p in [&p1, &p2, &p3] {
        assert!(p.starts_with(tmp.join("lens-profiles/own")), "{p:?}");
    }
    assert_eq!(p1, p2, "same named lens: new version of the same profile");
    assert_ne!(p1, p3);
    let own = lensprof::store::list();
    assert_eq!(own.len(), 2);
    let named = lensprof::store::find_auto("Testhersteller", "Testkamera", Some("Testobjektiv 58mm")).unwrap();
    assert_eq!(named.version, 2);
    assert!(lensprof::store::load(&lensprof::ProfileRef { id: named.id.clone(), version: 1 }).is_some());

    // the correction finds the own profile in the test folder
    assert!(lensprof::store::find_auto("Testhersteller", "Testkamera", None).is_some());

    // nothing changed in the real folder
    if let (Some(real), Some(before)) = (&real, before) {
        assert!(!real.starts_with(&tmp));
        assert_eq!(listing(real), before, "real folder {real:?} was changed");
    }

    // without a data folder, a test program gets its own temp folder
    std::env::remove_var("GRAINEO_DATA_DIR");
    let r = raw::lcp::backup_dir().unwrap();
    assert!(r.starts_with(std::env::temp_dir()), "{r:?}");
    if let Some(real) = &real {
        assert_ne!(&r, real);
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
