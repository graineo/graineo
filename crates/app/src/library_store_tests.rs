//! The library as the main store (08.11.2026): photos of the library develop the same as before from their sidecars,
//! also when the sidecars are gone; saving changes nothing next to the photos while writing sidecars is off; with
//! writing on, the sidecar carries the same fields as before.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use develop::Settings;
use library::xmp;

use crate::sidecar::{Flag, Meta};

fn hash(img: &image::RgbImage) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (img.width(), img.height()).hash(&mut h);
    img.as_raw().hash(&mut h);
    h.finish()
}

/// Tests that point `GRAINEO_LIBRARY_DB` at their own library run one after the other (the variable is process-wide).
static OWN_LIBRARY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn raw_dir() -> PathBuf {
    crate::env::var_os("GRAINEO_TEST_RAW_DIR").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/signature-edits"))
}

struct Photo {
    path: PathBuf,
    xmp: PathBuf,
}

/// Everything the app reads for a photo: settings, rating and own-settings flag of the original and the copy, Lightroom
/// data, and the standard development (pixel checksum).
#[derive(Debug, PartialEq)]
struct State {
    original: (Settings, Meta, bool),
    copy: Option<(Settings, Meta, bool)>,
    lightroom: xmp::Lightroom,
    pixels: u64,
}

fn state(p: &Photo, copy: bool) -> State {
    State {
        original: crate::raw_app::load_settings(&p.path, None),
        copy: copy.then(|| crate::raw_app::load_settings(&p.path, Some("Kopie 1"))),
        lightroom: library::store::lightroom_for(&p.path, None),
        pixels: hash(&crate::preview_repair::standard(&p.path, 512).expect("develops")),
    }
}

#[test]
fn library_develops_the_same_as_the_sidecars_did() {
    let src = raw_dir();
    let (Some(a_src), Some(b_src)) = (Some(src.join("01-P1060804.dng")).filter(|p| p.exists()), Some(src.join("02-DSC06683.dng")).filter(|p| p.exists())) else {
        eprintln!("test RAWs missing – skipped");
        return;
    };
    let _own = OWN_LIBRARY.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("graineo-store-test-{}-{}", std::process::id(), library::now()));
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    let lib = dir.join("library.sqlite");
    let before_db = std::env::var_os("GRAINEO_LIBRARY_DB");
    std::env::set_var("GRAINEO_LIBRARY_DB", &lib);

    let a = Photo { path: photos.join("A.dng"), xmp: photos.join("A.xmp") };
    let b = Photo { path: photos.join("B.dng"), xmp: photos.join("B.xmp") };
    std::fs::copy(&a_src, &a.path).unwrap();
    std::fs::copy(&b_src, &b.path).unwrap();
    // sidecars as graineo up to 0.83 wrote them: rating, keywords, a copy with own settings and Lightroom data,
    // own settings (A), Lightroom handover (A and B; B without own settings develops from it)
    let own = Settings { exposure: 0.4, contrast: 12.0, ..Settings::default() };
    let copy = Settings { exposure: -0.3, ..Settings::default() };
    let copies = serde_json::json!([{"name": "Kopie 1", "rating": 2, "flag": 0, "label": 3, "keywords": [], "settings": serde_json::to_value(&copy).unwrap(), "lightroomDevelop": "s = { Exposure2012 = -0.5 }", "lightroomOrientation": "AB"}]).to_string();
    xmp::write(&a.xmp, &xmp::Values { stars: 3, flag: 1, keywords: vec!["Orte|Rom".into()], copies_json: Some(copies), has_rating: true, has_keywords: true, rating_time: Some(1_000), ..Default::default() }).unwrap();
    xmp::write_develop(&a.xmp, Some(&serde_json::to_string(&Settings { view: develop::View::Developed, ..own.clone() }).unwrap())).unwrap();
    let lr = |name: &str| xmp::Lightroom { develop: Some("s = { Exposure2012 = 0.35,\nContrast2012 = 20,\nHasCrop = true,\nCropLeft = 0.1,\nCropRight = 0.9,\nCropTop = 0.05,\nCropBottom = 0.95 }".into()), orientation: Some("AB".into()), edited: true, file: Some(name.into()) };
    xmp::write_lightroom(&a.xmp, &lr("A.dng"), None).unwrap();
    xmp::write_lightroom(&b.xmp, &lr("B.dng"), None).unwrap();
    let sidecar_a = std::fs::read_to_string(&a.xmp).unwrap();

    // before: no library – everything from the sidecars
    assert!(!lib.exists());
    let a_before = state(&a, true);
    let b_before = state(&b, false);
    assert!(a_before.original.2 && a_before.copy.as_ref().unwrap().2 && !b_before.original.2);
    assert_eq!(a_before.original.1, Meta { rating: 3, flag: Flag::Pick, label: 0 });

    // the library: adding the folder reads the sidecars once
    let db = library::db::Db::open(&lib).unwrap();
    library::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
    assert_eq!(library::sidecar_import::unread_count(&db.conn), 0);
    // the sidecars go away: from now on everything comes from the library
    let away = dir.join("weg");
    std::fs::create_dir_all(&away).unwrap();
    std::fs::rename(&a.xmp, away.join("A.xmp")).unwrap();
    std::fs::rename(&b.xmp, away.join("B.xmp")).unwrap();
    assert!(library::store::locate(&a.path).is_some_and(|(_, read)| read));
    let a_after = state(&a, true);
    let b_after = state(&b, false);
    assert_eq!(a_after, a_before, "same settings, rating, copy, Lightroom data and pixels from the library");
    assert_eq!(b_after, b_before, "Lightroom starting values from the library develop the same");
    eprintln!("checksums before/after: A {:016x}/{:016x}, B {:016x}/{:016x}", a_before.pixels, a_after.pixels, b_before.pixels, b_after.pixels);

    // saving a change: into the library only – no sidecar appears (writing sidecars is off)
    let changed = Settings { exposure: 0.9, ..a_after.original.0.clone() };
    crate::raw_app::save_settings(&a.path, None, &changed, &Meta { rating: 5, ..a_after.original.1.clone() }).unwrap();
    assert_eq!(crate::raw_app::load_settings(&a.path, None).0.exposure, 0.9, "reads see it at once");
    assert!(library::store::flush(std::time::Duration::from_secs(10)));
    assert!(!a.xmp.exists(), "no new sidecar while writing them is off");
    let (s, m, _) = crate::raw_app::load_settings(&a.path, None);
    assert_eq!((s.exposure, m.rating), (0.9, 5));

    // writing sidecars on: the sidecar carries the same fields as the one written before
    std::fs::rename(away.join("A.xmp"), &a.xmp).unwrap();
    let file_a = library::store::locate(&a.path).unwrap().0;
    assert!(library::store::write_sidecar_from_library(&db, file_a, true).unwrap());
    let old_values = xmp::read_text(&sidecar_a).unwrap();
    let xmp::ReadResult::Ok(new_values) = xmp::read(&a.xmp) else { panic!("sidecar not readable") };
    assert_eq!((new_values.stars, new_values.flag, &new_values.keywords), (5, old_values.flag, &old_values.keywords));
    assert_eq!(xmp::lightroom(&a.xmp), xmp::lightroom_from_text(&sidecar_a));
    let dev: Settings = serde_json::from_str(&xmp::read_develop(&a.xmp).unwrap().unwrap()).unwrap();
    assert_eq!(dev.exposure, 0.9);
    let (k_old, k_new) = (library::sidecar::read_copies(old_values.copies_json.as_deref()), library::sidecar::read_copies(new_values.copies_json.as_deref()));
    assert_eq!(k_new, k_old, "copy with its settings and Lightroom data");
    // written again unchanged: the file is not touched
    assert!(!library::store::write_sidecar_from_library(&db, file_a, true).unwrap());

    drop(db);
    match before_db {
        Some(v) => std::env::set_var("GRAINEO_LIBRARY_DB", v),
        None => std::env::remove_var("GRAINEO_LIBRARY_DB"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A development of a newer version (unknown mask field: not readable as `Settings` here) in the library is never
/// overwritten or removed by saving: Sigma X3F original (saved on every photo switch), another format, virtual copy.
/// Ratings are still saved; the unchanged initial values are no error (review of 1.3.0, M1).
#[test]
fn library_keeps_a_development_of_a_newer_version() {
    let _own = OWN_LIBRARY.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("graineo-newer-lib-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    let lib = dir.join("library.sqlite");
    let before_db = std::env::var_os("GRAINEO_LIBRARY_DB");
    std::env::set_var("GRAINEO_LIBRARY_DB", &lib);

    let mut doc = masks::Document::from_json(include_str!("../../masks/samples/masks-example.json")).unwrap();
    doc.settle_version();
    let mut v = serde_json::to_value(Settings { exposure: 0.7, local: Some(doc), ..Default::default() }).unwrap();
    v["local"]["layers"][0]["futureField"] = serde_json::json!(1);
    let unknown = v.to_string();
    assert!(serde_json::from_str::<Settings>(&unknown).is_err());

    let jpg = photos.join("A.jpg");
    image::RgbImage::from_pixel(32, 24, image::Rgb([90, 90, 90])).save(&jpg).unwrap();
    let x3f = photos.join("SDIM0001.X3F");
    std::fs::write(&x3f, b"not a real X3F").unwrap();
    // the developments arrive with the sidecars (read once when the folder is added)
    let copies = serde_json::json!([{ "name": "Kopie 1", "rating": 0, "flag": 0, "label": 0, "keywords": [], "settings": v.clone() }]).to_string();
    xmp::write(&xmp::path_to(&jpg), &xmp::Values { copies_json: Some(copies), ..Default::default() }).unwrap();
    xmp::write_develop(&xmp::path_to(&jpg), Some(&unknown)).unwrap();
    xmp::write_develop(&crate::sidecar::sidecar_path(&x3f), Some(&unknown)).unwrap();
    let db = library::db::Db::open(&lib).unwrap();
    library::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
    // from now on the library only
    std::fs::remove_file(xmp::path_to(&jpg)).unwrap();
    std::fs::remove_file(crate::sidecar::sidecar_path(&x3f)).unwrap();
    let stored = |p: &Path, copy: Option<&str>| library::store::develop_at(p, library::store::locate(p), copy).unwrap();
    let originals = [(jpg.clone(), None), (jpg.clone(), Some("Kopie 1")), (x3f.clone(), None)];
    for (p, copy) in &originals {
        assert!(library::store::locate(p).is_some(), "{} in the library", p.display());
        assert!(stored(p, *copy).is_some_and(|j| serde_json::from_str::<Settings>(&j).is_err()), "{} {copy:?}: newer development stored", p.display());
    }
    let changed = Settings { exposure: 1.2, ..Settings::default() };
    let rated = Meta { rating: 4, ..Meta::default() };
    // what this version shows instead (initial values): saved without a word, nothing removed
    let (x3f_shown, _) = crate::sidecar::load(&x3f);
    crate::sidecar::save_entry(&x3f, None, &x3f_shown, &Meta::default()).unwrap();
    let (jpg_shown, _, _) = crate::raw_app::load_settings(&jpg, None);
    crate::sidecar::save_entry(&jpg, None, &jpg_shown, &Meta::default()).unwrap();
    crate::sidecar::save_entry(&jpg, Some("Kopie 1"), &Settings::default(), &Meta::default()).unwrap();
    // a change: refused, the rating is saved
    for (p, copy) in &originals {
        let e = crate::sidecar::save_entry(p, *copy, &changed, &rated).unwrap_err();
        assert_eq!(e, crate::develop_guard::newer_text(), "{} {copy:?}", p.display());
    }
    assert!(library::store::flush(std::time::Duration::from_secs(10)));
    for (p, copy) in &originals {
        assert_eq!(stored(p, *copy).as_deref(), Some(unknown.as_str()), "{} {copy:?}: development kept", p.display());
        let loc = library::store::locate(p).unwrap();
        assert_eq!(library::store::meta(loc.0, *copy).map(|m| m.0), Some(4), "{} {copy:?}: rating saved", p.display());
    }
    assert!(crate::photo_state::get(&x3f).iter().any(|n| n.kind == crate::photo_state::Kind::NewerDevelop));

    drop(db);
    match before_db {
        Some(v) => std::env::set_var("GRAINEO_LIBRARY_DB", v),
        None => std::env::remove_var("GRAINEO_LIBRARY_DB"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
