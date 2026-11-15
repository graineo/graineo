use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
fn test_env() -> Storage {
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "graineo-film-references-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("source/Original package")).unwrap();
    Storage {
        data: root.join("data"),
        cache: root.join("cache/film-looks"),
        automatic: vec![root.join("source")],
    }
}
fn profile(id: &str, name: &str, group: &str, delta: i16) -> String {
    let mut raw = Vec::new();
    for w in [1u32, 1, 3, 2] {
        raw.extend(w.to_le_bytes());
    }
    for i in 0..8 {
        for c in 0..3 {
            let x = if i == 7 && c == 0 { delta as u16 } else { 0u16 };
            raw.extend(x.to_le_bytes());
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).unwrap();
    let mut b = (raw.len() as u32).to_le_bytes().to_vec();
    b.extend(z.finish().unwrap());
    while b.len() % 4 != 0 {
        b.push(0);
    }
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><rdf:RDF><rdf:Description crs:PresetType="Look" crs:UUID="{id}" crs:SupportsAmount="True" crs:RGBTable="Synthetic" crs:Table_Synthetic="{}"><crs:Name><rdf:Alt><rdf:li>{name}</rdf:li></rdf:Alt></crs:Name><crs:Group><rdf:Alt><rdf:li>{group}</rdf:li></rdf:Alt></crs:Group></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        super::super::base85_encode(&b)
    )
}
fn source_profile(a: &Storage, id: &str, delta: i16) -> PathBuf {
    let p = a.automatic[0]
        .join("Original package")
        .join(format!("{id}.xmp"));
    fs::write(&p, profile(id, "Synthetic film", "Test package", delta)).unwrap();
    p
}

#[test]
fn folder_reference_only_remove_and_keep_originals() {
    let a = test_env();
    let path = source_profile(&a, "SYNTHETIC-A", -6554);
    let bytes = fs::read(&path).unwrap();
    assert_eq!(a.search()[0].source, Source::Automatic);
    let mut o = a.options();
    o.discover = false;
    a.write_options(&o).unwrap();
    let root = a.automatic[0].canonicalize().unwrap();
    assert_eq!(a.add_folder(&[root.clone()]).imported, 1);
    assert_eq!(a.add_folder(&[root.clone()]).existing, 1);
    let profiles = a.search();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].source, Source::Referenced);
    assert_eq!(profiles[0].path, path.canonicalize().unwrap());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(!a.data.join("film-profiles").exists());
    assert!(!a.cache.exists());
    a.remove_folder(&root).unwrap();
    assert!(a.search().is_empty());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let json = fs::read_to_string(a.data.join("settings.json")).unwrap();
    assert!(json.contains("filmProfileFolders"));
}
#[test]
fn offline_folder_keeps_headers_and_returns_unchanged() {
    let a = test_env();
    let path = source_profile(&a, "OFFLINE", -6000);
    let root = a.automatic[0].canonicalize().unwrap();
    let mut o = a.options();
    o.discover = false;
    a.write_options(&o).unwrap();
    a.add_folder(&[root.clone()]);
    assert!(a.search()[0].available);
    let detached = root.with_file_name("detached-source");
    fs::rename(&root, &detached).unwrap();
    let offline = a.search();
    assert_eq!(offline.len(), 1);
    assert!(!offline[0].available);
    assert_eq!(offline[0].name, "Synthetic film");
    assert!(offline[0].look().is_none());
    assert_eq!(a.packages()[0].available, 0);
    let index = fs::read_to_string(a.data.join("film-profile-index.json")).unwrap();
    assert!(!index.contains("Table_Synthetic"));
    assert!(!index.contains("data"));
    assert!(index.contains("sourceFolder"));
    fs::rename(&detached, &root).unwrap();
    let again = a.search();
    assert!(again[0].available);
    assert!(again[0].look().is_some());
    assert!(path.exists());
}
#[test]
fn available_duplicate_wins_over_offline_reference() {
    let a = test_env();
    source_profile(&a, "DUP", 0);
    let second = a.data.parent().unwrap().join("extra");
    fs::create_dir_all(&second).unwrap();
    fs::write(
        second.join("dup.xmp"),
        profile("DUP", "Explicit", "Test package", -6000),
    )
    .unwrap();
    a.add_folder(&[second.clone()]);
    assert_eq!(a.search()[0].name, "Explicit");
    fs::rename(&second, second.with_file_name("offline-extra")).unwrap();
    let p = a.search();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].name, "Synthetic film");
    assert!(p[0].available);
}
#[test]
fn hiding_keeps_references_and_unknown_settings() {
    let a = test_env();
    source_profile(&a, "SYNTHETIC-B", 0);
    fs::create_dir_all(&a.data).unwrap();
    fs::write(a.data.join("settings.json"), r#"{"otherSetting":42}"#).unwrap();
    a.show_package("Test package", false).unwrap();
    a.show_profile("synthetic-b", false).unwrap();
    assert_eq!(a.search().len(), 1);
    assert!(!a.packages()[0].visible);
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(a.data.join("settings.json")).unwrap()).unwrap();
    assert_eq!(v["otherSetting"], 42);
    a.show_package("Test package", true).unwrap();
    a.show_profile("synthetic-b", true).unwrap();
    assert!(a.options().hidden_profiles.is_empty());
}
#[test]
fn no_table_disk_cache_and_content_change_without_timestamp() {
    let a = test_env();
    let p = source_profile(&a, "SYNTHETIC-C", -6554);
    let f = read_profile(&p).unwrap();
    let before = f.look().unwrap().pixel([1.; 3]);
    let stamp = fs::metadata(&p).unwrap().modified().unwrap();
    fs::write(
        &p,
        profile("SYNTHETIC-C", "Synthetic film", "Test package", -13107),
    )
    .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    let after = f.look().unwrap().pixel([1.; 3]);
    assert!((after[0] - before[0]).abs() > 0.05);
    assert!(!a.cache.exists());
}
#[test]
fn broken_oversized_and_adobe_profiles_count_as_skipped() {
    let a = test_env();
    let root = &a.automatic[0];
    fs::write(root.join("bad.xmp"), "<invalid/>").unwrap();
    fs::write(
        root.join("adobe.xmp"),
        profile("ADOBE", "Adobe Color", "Adobe", 0),
    )
    .unwrap();
    fs::File::create(root.join("huge.xmp"))
        .unwrap()
        .set_len(MAX_FILE + 1)
        .unwrap();
    fs::write(
        root.join("entity.xmp"),
        "<!DOCTYPE x [<!ENTITY y SYSTEM 'file:///etc/passwd'>]><x>&y;</x>",
    )
    .unwrap();
    let (profiles, b) = a.search_with_report();
    assert!(profiles.is_empty());
    assert_eq!(b.skipped, 4);
    let mut bomb = (u32::MAX).to_le_bytes().to_vec();
    bomb.extend([0; 4]);
    assert!(super::super::table_from_text(&super::super::base85_encode(&bomb)).is_none());
}
#[cfg(unix)]
#[test]
fn links_broken_settings_and_old_cache_are_limited() {
    let a = test_env();
    let p = source_profile(&a, "SYMLINK", 0);
    std::os::unix::fs::symlink(&p, a.automatic[0].join("linked.xmp")).unwrap();
    assert_eq!(a.search().len(), 1);
    fs::write(a.data.join("settings.json"), b"broken").unwrap();
    assert!(a.write_options(&Options::default()).is_err());
    assert_eq!(fs::read(a.data.join("settings.json")).unwrap(), b"broken");
    fs::create_dir_all(&a.cache).unwrap();
    fs::write(
        a.cache.join("look-1234567890abcdef.json"),
        b"obsolete derived cache",
    )
    .unwrap();
    fs::write(a.cache.join("preserve.xmp"), b"source").unwrap();
    std::os::unix::fs::symlink(&p, a.cache.join("look-abcdef1234567890.json")).unwrap();
    a.clear_old_cache().unwrap();
    assert!(!a.cache.join("look-1234567890abcdef.json").exists());
    assert_eq!(fs::read(a.cache.join("preserve.xmp")).unwrap(), b"source");
    assert!(p.exists());
    assert!(
        a.cache
            .join("look-abcdef1234567890.json")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let mut bad = a.clone();
    bad.data = a.data.parent().unwrap().join("Adobe/CameraRaw");
    assert!(bad.write_options(&Options::default()).is_err());
    assert!(!bad.data.exists());
}
#[test]
fn cube_stays_in_its_source_folder_and_keeps_id() {
    let a = test_env();
    let path = a.automatic[0].join("Synthetic swap.cube");
    let mut text = String::from("LUT_3D_SIZE 17\n");
    for b in 0..17 {
        for g in 0..17 {
            for r in 0..17 {
                text.push_str(&format!(
                    "{} {} {}\n",
                    b as f32 / 16.,
                    g as f32 / 16.,
                    r as f32 / 16.
                ));
            }
        }
    }
    fs::write(&path, &text).unwrap();
    let id = read_profile(&path).unwrap().uuid;
    let root = a.automatic[0].canonicalize().unwrap();
    a.add_folder(&[root.clone()]);
    let f = a.search().into_iter().find(|f| f.uuid == id).unwrap();
    assert_eq!(f.name, "Synthetic swap");
    assert_eq!(f.source, Source::Referenced);
    let rgb = f.look().unwrap().pixel([0.125, 0.5, 0.75]);
    for (v, e) in rgb.into_iter().zip([0.75, 0.5, 0.125]) {
        assert!((v - e).abs() < 1e-6);
    }
    a.remove_folder(&root).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}
#[test]
fn depth_file_count_and_folder_count_stay_limited() {
    let a = test_env();
    let mut dir = a.automatic[0].clone();
    for _ in 0..14 {
        dir = dir.join("deep");
        fs::create_dir_all(&dir).unwrap();
    }
    fs::write(
        dir.join("ignored.xmp"),
        profile("TOO-DEEP", "Synthetic", "Test", 0),
    )
    .unwrap();
    let (v, b) = a.search_with_report();
    assert!(v.is_empty());
    assert!(b.messages.iter().any(|m| m.contains("Unterordner")));
    let roots: Vec<_> = (0..65)
        .map(|i| {
            let p = a.data.parent().unwrap().join(format!("folder-{i}"));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();
    let b = a.add_folder(&roots);
    assert_eq!(b.imported, 64);
    assert_eq!(b.skipped, 1);
    let mut visited = MAX_FILES * 4;
    let (_, b) = files_found(&a.automatic, &mut visited);
    assert!(b.messages.iter().any(|m| m.contains("Suchgrenze")));
}

#[test]
fn opaque_table_id_and_adobe_origin_stay_checked() {
    let a = test_env();
    let root = &a.automatic[0];
    let text = profile("OPAQUE", "Synthetic", "Test", -6554).replace(
        "Synthetic\" crs:Table_Synthetic",
        "0123456789ABCDEF0123456789ABCDEF\" crs:Table_0123456789ABCDEF0123456789ABCDEF",
    );
    let p = root.join("opaque.xmp");
    fs::write(&p, text).unwrap();
    assert!(read_profile(&p).unwrap().look().is_some());
    let vendor = root.join("Settings/Adobe/Profiles/Camera");
    fs::create_dir_all(&vendor).unwrap();
    fs::write(
        vendor.join("camera.xmp"),
        profile("ADOBE-CAMERA", "Camera Synthetic", "Profiles", 0),
    )
    .unwrap();
    let disguised = root.join("copyright.xmp");
    fs::write(
        &disguised,
        profile("ADOBE-COPYRIGHT", "Camera Synthetic", "Profiles", 0).replace(
            "crs:PresetType=",
            "crs:Copyright=\"Copyright Adobe Systems\" crs:PresetType=",
        ),
    )
    .unwrap();
    assert!(read_profile(&disguised).is_none());
    let profiles = a.search();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].uuid, "OPAQUE");
}

#[test]
fn broken_field_types_and_new_index_stay_unchanged() {
    let a = test_env();
    source_profile(&a, "TYPE", 0);
    a.search();
    let index = a.data.join("film-profile-index.json");
    let before = fs::read(&index).unwrap();
    fs::write(
        a.data.join("settings.json"),
        br#"{"filmProfileFolders":42}"#,
    )
    .unwrap();
    assert!(a.write_options(&Options::default()).is_err());
    let (profiles, b) = a.search_with_report();
    assert!(profiles.is_empty());
    assert!(!b.messages.is_empty());
    assert_eq!(fs::read(&index).unwrap(), before);
    fs::write(a.data.join("settings.json"), b"{}").unwrap();
    let future = br#"{"version":99,"profiles":[],"future":42}"#;
    fs::write(&index, future).unwrap();
    a.search();
    assert_eq!(fs::read(&index).unwrap(), future);
}
#[test]
fn previous_newest_source_wins_on_uuid_duplicates() {
    let a = test_env();
    let root = &a.automatic[0];
    let old = root.join("z-old.xmp");
    let new = root.join("a-new.xmp");
    fs::write(&old, profile("SAME-ID", "Old", "Test", 0)).unwrap();
    fs::write(&new, profile("SAME-ID", "New", "Test", 0)).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(100))
        .unwrap();
    assert_eq!(a.search()[0].name, "New");
}
