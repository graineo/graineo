//! Edits of graineo 1.0 name camera looks with old style ids (see `legacy_names::looks`). They load with the
//! current name and develop exactly like an edit that uses the current name.

use legacy_names::looks::old_look_names;
use develop::{Rgb16, Settings};

fn load(look: &str) -> Settings {
    serde_json::from_str(&format!(r#"{{"raw":{{"look":"{look}"}}}}"#)).unwrap()
}

/// Every bundled look that graineo 1.0 shipped under an old name is found again from that name.
#[test]
fn every_old_look_name_finds_its_bundled_look() {
    let mut n = 0;
    for name in raw::looks::Look::names() {
        for old in old_look_names(name) {
            assert_ne!(old, name);
            assert_eq!(load(&old).raw.look, name, "{old} does not load as {name}");
            n += 1;
        }
    }
    // 17 renamed looks (Ricoh, Canon Faithful, Fujifilm Monochrome) plus the personal user styles where bundled
    assert!(n >= 17, "only {n} old look names checked");
}

#[cfg(target_os = "macos")]
#[test]
fn old_look_name_develops_like_the_current_one() {
    let root = std::env::var_os("GRAINEO_DATA_DIR").expect("isolated tests only");
    let dir = std::path::PathBuf::from(root).join(format!("legacy-looks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("colors.tif");
    let (w, h) = (48usize, 32usize);
    let data: Vec<u16> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(x * 65535 / w) as u16, (y * 65535 / h) as u16, ((x + y) * 65535 / (w + h)) as u16]
        })
        .collect();
    develop::export::write_tiff16(&Rgb16 { width: w, height: h, data }, &path).unwrap();
    let doc = rawdev::RawDoc::open(&path, &|| true).unwrap().unwrap();
    let neutral = doc.render_preview(&load("neutral"));
    let mut n = 0;
    for name in ["gr3/positive-film", "gr2/bw-high-contrast", "x100/monochrome-ye", "eos-5d-mark-ii/faithful"] {
        let current = doc.render_preview(&load(name));
        assert_ne!(current.data, neutral.data, "{name}: look not applied");
        for old in old_look_names(name) {
            assert_eq!(doc.render_preview(&load(&old)).data, current.data, "{old} develops differently from {name}");
            n += 1;
        }
    }
    assert_eq!(n, 4);
    let _ = std::fs::remove_dir_all(&dir);
}
