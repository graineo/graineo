//! Regression: results of background jobs (open, preload, render, thumbnails) must never land on another photo when the
//! image list changes (other folder) while they run. Two library folders with different photos, preloading in the first,
//! switching to the second, opening photos there: the image shown, the cache and the stored previews always belong to
//! the photo of their entry.
//!
//! Needs `GRAINEO_DATA_DIR` and free test RAWs (`testbilder/signature-edits` or `GRAINEO_TEST_RAW_DIR`); skipped
//! without them. Run single-threaded (`--test-threads=1`): it sets up its own library.

use crate::*;
use eframe::App as _;
use std::path::{Path, PathBuf};

fn raw_dir() -> Option<PathBuf> {
    let p = crate::env::var_os("GRAINEO_TEST_RAW_DIR").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/signature-edits"));
    p.is_dir().then_some(p)
}

/// Everything the app holds for an index must belong to the photo of that entry.
fn check(a: &App, step: &str) {
    if let Some(c) = &a.current {
        assert_eq!(c.loaded.path, a.entries[c.index].path, "{step}: shown image {} does not belong to entry {} ({})", c.loaded.path.display(), c.index, a.entries[c.index].path.display());
    }
    for (i, l) in &a.cache {
        assert_eq!(l.path, a.entries[*i].path, "{step}: cached image {} under entry {} ({})", l.path.display(), i, a.entries[*i].path.display());
    }
}

/// Mean colour of an RGB image (for "is this the same photo").
fn mean(img: &image::RgbImage) -> [f32; 3] {
    let n = img.pixels().len().max(1) as f32;
    let mut s = [0f32; 3];
    for p in img.pixels() {
        for c in 0..3 {
            s[c] += p.0[c] as f32;
        }
    }
    s.map(|v| v / n)
}

#[test]
fn results_never_land_on_another_photo_after_a_list_change() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let Some(src) = raw_dir() else {
        eprintln!("skipped: no test RAWs");
        return;
    };
    let root = std::env::temp_dir().join(format!("graineo-list-switch-{}", std::process::id()));
    let (dir_a, dir_b) = (root.join("A"), root.join("B"));
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    // different photos, same positions in both lists
    for (dir, names) in [(&dir_a, ["01-P1060804.dng", "05-DSC05627.ARW", "09-DSC_9514.NEF"]), (&dir_b, ["07-DSC03768.ARW", "08-DSC00866.ARW", "12-AM403348.ARW"])] {
        for n in names {
            if !dir.join(n).exists() {
                std::fs::copy(src.join(n), dir.join(n)).unwrap();
            }
        }
    }
    {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
    }
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let _app_lock = crate::ui_test::app_lock();
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let mut frame = |a: &mut App, step: &str| {
        time += 0.1;
        let mut input = egui::RawInput { time: Some(time), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        check(a, step);
        std::thread::sleep(std::time::Duration::from_millis(15));
    };
    let wait = |a: &mut App, frame: &mut dyn FnMut(&mut App, &str), step: &str, until: &dyn Fn(&App) -> bool| {
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while !until(a) {
            frame(a, step);
            assert!(std::time::Instant::now() < deadline, "{step}: timeout (loading {:?}, current {:?}, texture {}, not developable {:?}, status {}, tab grid {})", a.loading, a.current.as_ref().map(|c| c.index), a.current.as_ref().is_some_and(|c| c.texture.is_some()), a.not_developable, a.status, a.in_grid());
        }
    };
    let shown = |name: &'static str| move |a: &App| a.loading.is_none() && a.current.as_ref().is_some_and(|c| c.texture.is_some() && a.entries[c.index].path.ends_with(name));
    let go = |a: &mut App, name: &str| {
        let i = a.entries.iter().position(|e| e.path.ends_with(name)).unwrap_or_else(|| panic!("{name} not in the list"));
        a.lib.set_active(a.entries[i].image);
        a.select(i, false);
    };

    // folder A in the single view; page once, so that neighbours are preloaded
    a.open_folder_inner(dir_a.clone());
    assert_eq!(a.entries.len(), 3, "folder A in the library");
    a.to_single_image();
    for name in ["05-DSC05627.ARW", "01-P1060804.dng"] {
        go(&mut a, name);
        wait(&mut a, &mut frame, name, &shown(name));
    }
    // preloading of the neighbours starts after the dwell: switch the list while it runs
    for _ in 0..4 {
        frame(&mut a, "A dwell");
    }
    // folder B: the same indices, other photos
    a.open_folder_inner(dir_b.clone());
    assert!(a.entries.iter().all(|e| e.path.starts_with(&dir_b)), "folder B shown");
    a.to_single_image();
    for name in ["08-DSC00866.ARW", "07-DSC03768.ARW", "12-AM403348.ARW"] {
        go(&mut a, name);
        wait(&mut a, &mut frame, name, &shown(name));
        for _ in 0..10 {
            frame(&mut a, name);
        }
    }
    // back to A while B preloads
    a.open_folder_inner(dir_a.clone());
    a.to_single_image();
    go(&mut a, "09-DSC_9514.NEF");
    wait(&mut a, &mut frame, "A 09", &shown("09-DSC_9514.NEF"));
    for _ in 0..20 {
        frame(&mut a, "A rest");
    }
    // the stored developed views belong to their photos: compare with a fresh development of each photo
    let photos: Vec<(i64, PathBuf, Settings)> = a.entries.iter().map(|e| (e.image, e.path.clone(), e.settings.clone())).collect();
    for (image, path, settings) in photos {
        let Some(view) = developed_cache::load(image) else { continue };
        let stored = image::RgbImage::from_raw(view.size[0] as u32, view.size[1] as u32, view.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect()).unwrap();
        let fresh = crate::large_preview::render(&path, &settings, 512).unwrap();
        let (m1, m2) = (mean(&stored), mean(&fresh));
        let d = (0..3).map(|c| (m1[c] - m2[c]).abs()).fold(0.0, f32::max);
        assert!(d < 25.0, "developed view of {} differs from its photo: {m1:?} vs {m2:?}", path.display());
    }
}

/// Two viewed folders (not in the library): their photos must not share IDs, otherwise the second folder takes over
/// the loaded photo, settings and preview of the first one at the same place.
#[test]
fn viewed_folders_keep_their_photos_apart() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let Some(src) = raw_dir() else {
        eprintln!("skipped: no test RAWs");
        return;
    };
    let root = std::env::temp_dir().join(format!("graineo-viewed-{}", std::process::id()));
    let (dir_c, dir_d) = (root.join("C"), root.join("D"));
    for (dir, names) in [(&dir_c, ["05-DSC05627.ARW", "09-DSC_9514.NEF"]), (&dir_d, ["07-DSC03768.ARW", "12-AM403348.ARW"])] {
        std::fs::create_dir_all(dir).unwrap();
        for n in names {
            if !dir.join(n).exists() {
                std::fs::copy(src.join(n), dir.join(n)).unwrap();
            }
        }
    }
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let _app_lock = crate::ui_test::app_lock();
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let mut frame = |a: &mut App, step: &str| {
        time += 0.1;
        let mut input = egui::RawInput { time: Some(time), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        check(a, step);
        std::thread::sleep(std::time::Duration::from_millis(15));
    };
    a.open_folder_inner(dir_c.clone());
    a.to_single_image();
    let i = a.entries.iter().position(|e| e.path.ends_with("09-DSC_9514.NEF")).unwrap();
    a.lib.set_active(a.entries[i].image);
    a.select(i, false);
    let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
    while a.current.as_ref().is_none_or(|c| c.texture.is_none() || !a.entries[c.index].path.ends_with("09-DSC_9514.NEF")) {
        frame(&mut a, "C open");
        assert!(std::time::Instant::now() < deadline, "C: timeout");
    }
    let ids_c: Vec<i64> = a.entries.iter().map(|e| e.image).collect();
    a.open_folder_inner(dir_d.clone());
    check(&a, "D after switch");
    assert!(a.entries.iter().all(|e| !ids_c.contains(&e.image)), "viewed folders share photo IDs");
    for _ in 0..30 {
        frame(&mut a, "D");
    }
}
