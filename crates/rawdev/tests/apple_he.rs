//! Nikon "High Efficiency" NEFs through the Apple path (macOS system RAW engine, `rawdev::apple`). Runs only when
//! `GRAINEO_TEST_NEF_HE_DIR` names a folder with such files (e.g. the CC0 samples of raw.pixls.us); otherwise, and off
//! macOS, it is skipped. No sample is kept in the repository.
//!
//! Every HE file opens automatically with the engine (graineo cannot decode it), the version is the conservative
//! default (the newest below 9), a pinned version is honoured, a missing pin falls back to the newest and says so,
//! and the white balance renders the engine again while exposure does not.

use std::path::PathBuf;

use develop::Settings;
use rawdev::apple::{apple_value, Reason};

fn with_engine(engine: Option<&str>) -> Settings {
    let mut s = Settings::default();
    s.raw.engine = engine.map(str::to_string);
    s
}

#[test]
fn high_efficiency_nefs_develop_through_the_system_engine() {
    let Some(dir) = std::env::var_os("GRAINEO_TEST_NEF_HE_DIR").map(PathBuf::from) else {
        eprintln!("GRAINEO_TEST_NEF_HE_DIR not set, skipped");
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).expect("folder").filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("nef"))).collect();
    files.sort();
    let mut he = 0;
    for f in &files {
        let blocked = raw::check_decodable(f).err().map(|e| e.to_string());
        let Some(why) = blocked.filter(|w| w.contains(raw::NIKON_HE)) else { continue };
        he += 1;
        if !cfg!(target_os = "macos") {
            // elsewhere: the plain message, never a damaged file
            let e = rawdev::RawDoc::open(f, &|| true).err().expect("HE does not open off macOS");
            assert!(e.contains(raw::NIKON_HE), "{e}");
            continue;
        }
        let doc = rawdev::RawDoc::open(f, &|| true).expect("opens").expect("not cancelled");
        let a = doc.apple.clone().unwrap_or_else(|| panic!("{}: not developed by the engine", f.display()));
        assert_eq!(a.reason, Reason::Fallback);
        assert!(a.graineo_blocked.as_deref().is_some_and(|b| why.contains(b)), "{:?} / {why}", a.graineo_blocked);
        assert_eq!(Some(a.used.clone()), raw::apple::fallback_version(&a.probe.versions, &a.probe.engine_default, false));
        assert!(raw::apple::version_number(&a.used).is_some_and(|n| n < 9.0) || a.probe.versions.iter().all(|v| raw::apple::version_number(v).is_some_and(|n| n >= 9.0)));
        assert!(doc.full_size[0] > 4000 && doc.preview.w.max(doc.preview.h) == rawdev::doc::PREVIEW_EDGE, "{:?} {}×{}", doc.full_size, doc.preview.w, doc.preview.h);
        // automatic fits, and so does the pin of the version it used; another engine does not
        assert!(doc.engine_fits(&with_engine(None)));
        assert!(doc.engine_fits(&with_engine(Some(&a.pin_value()))));
        assert!(!doc.engine_fits(&with_engine(Some("graineo"))));
        // the development: a plausible image, exposure does not render the engine again (same stage)
        let s = Settings::default();
        let st = doc.preview_stage(&s);
        let mut brighter = s.clone();
        brighter.exposure = 0.5;
        assert!(std::sync::Arc::ptr_eq(&st, &doc.preview_stage(&brighter)), "exposure is graineo's: no new rendering");
        let img = doc.render_preview(&s);
        let mean = img.data.iter().map(|v| *v as f64).sum::<f64>() / img.data.len() as f64 / 65535.0;
        assert!(mean > 0.08 && mean < 0.9, "{}: mean {mean}", f.display());
        // white balance renders again
        let mut warm = s.clone();
        warm.raw.wb_mode = "custom".into();
        warm.raw.temperature = 7500.0;
        warm.raw.tint = 0.0;
        assert!(!std::sync::Arc::ptr_eq(&st, &doc.preview_stage(&warm)), "white balance goes to the engine");
        // full resolution ahead (the app's `PrepareFull`): the raw stage of these settings, rendered once; the stage
        // of the white balance as shot (`full_base`) would be a second full rendering
        let before = a.renders();
        let base = doc.full_base_for(&warm).expect("full resolution");
        let full = doc.full_stage(&warm).expect("full stage");
        assert_eq!(a.renders(), before + 1, "one full rendering for one white balance");
        assert_eq!((base.w, base.h), (full.w, full.h));
        eprintln!("{}: full stage {}×{}, native {:?}", f.display(), base.w, base.h, doc.full_size);
        drop((base, full));
        doc.release_memory();
        // pinned version, also through graineo's explicit choice (graineo cannot: the engine anyway)
        let pinned = rawdev::RawDoc::open_for(f, &with_engine(Some(&apple_value(&a.probe.versions[0]))), &|| true).unwrap().unwrap();
        assert_eq!(pinned.apple.as_ref().map(|a| a.used.clone()), Some(a.probe.versions[0].clone()));
        assert_eq!(pinned.apple.as_ref().map(|a| a.reason), Some(Reason::Chosen));
        assert!(pinned.apple.as_ref().is_some_and(|a| a.graineo_blocked.is_some()));
        // a version the system does not offer (after an update): the newest, and the missing pin is named
        let missing = rawdev::RawDoc::open_for(f, &with_engine(Some("apple:1")), &|| true).unwrap().unwrap();
        let m = missing.apple.as_ref().unwrap();
        assert_eq!(m.pinned_missing.as_deref(), Some("1"));
        assert_eq!(Some(m.used.clone()), raw::apple::newest(&m.probe.versions, &m.probe.engine_default));
        assert!(missing.engine_fits(&with_engine(Some("apple:1"))), "the entry is kept, nothing reopens");
        eprintln!("{}: Apple RAW {} of {:?}, {}×{}", f.display(), a.used, a.probe.versions, doc.full_size[0], doc.full_size[1]);
    }
    eprintln!("{he} High Efficiency files");
    // a file graineo decodes: automatic stays graineo; Apple only when chosen
    if let Some(f) = files.iter().find(|f| raw::check_decodable(f).is_ok()) {
        let g = rawdev::RawDoc::open(f, &|| true).unwrap().unwrap();
        assert!(g.apple.is_none(), "automatic = graineo for files graineo decodes");
        assert!(g.engine_fits(&with_engine(None)) && !g.engine_fits(&with_engine(Some("apple:8"))));
        if cfg!(target_os = "macos") {
            let a = rawdev::RawDoc::open_for(f, &with_engine(Some("apple")), &|| true).unwrap().unwrap();
            if let Some(dev) = a.apple.as_ref() {
                assert_eq!((dev.reason, dev.graineo_blocked.is_none()), (Reason::Chosen, true));
            }
        }
    }
}
