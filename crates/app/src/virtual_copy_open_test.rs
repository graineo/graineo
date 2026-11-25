//! A virtual copy opens itself in single image view (forum report: after "Virtuelle Kopie anlegen" in the library,
//! a double click on the copy opened the next real photo). Copy made from the grid, opened by a double click on
//! its tile, at several display scales (HiDPI 200 % / 250 %).
//!
//! Needs `GRAINEO_DATA_DIR` (own library in the test run's data folder). Synthetic photos.

use crate::*;
use eframe::App as _;

/// Index of the photo the single view shows (loaded, loading or as a stored preview).
fn shown(a: &App) -> Option<usize> {
    a.loading.or(a.current.as_ref().map(|c| c.index)).or(a.not_developable)
}

/// `quick`: the photos are shown from a collection (the quick collection), not from their folder.
fn run_at(scale: f32, stacked: bool, quick: bool) {
    let _app_lock = crate::ui_test::app_lock();
    let root = std::env::temp_dir().join(format!("graineo-vcopy-{}-{}-{stacked}-{quick}", std::process::id(), (scale * 100.0) as u32));
    let dir = root.join("A");
    std::fs::create_dir_all(&dir).unwrap();
    for k in 0..4u8 {
        image::RgbImage::from_pixel(400, 300, image::Rgb([40 + k * 40, 90, 60])).save(dir.join(format!("p{k}.png"))).unwrap();
    }
    {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
        let dir_s = format!("{}/", dir.display());
        let ids: Vec<i64> = db.conn.prepare("SELECT image.id FROM image JOIN file ON file.id = image.file JOIN folder ON folder.id = file.folder JOIN root_folder root ON root.id = folder.root WHERE root.path || folder.path = ?1 ORDER BY file.name").unwrap().query_map([&dir_s], |r| r.get(0)).unwrap().flatten().collect();
        assert_eq!(ids.len(), 4, "photos of {dir_s}");
        if quick {
            let q = db.quick_collection().unwrap();
            db.conn.execute("DELETE FROM collection_image WHERE collection = ?1", [q]).unwrap();
            db.add_to_collection(q, &ids).unwrap();
        }
        if stacked {
            // the four photos as one expanded stack
            let (_, stack) = library::series::create(&db, &ids, ids[0]).unwrap();
            library::series::set_collapsed(&db, stack, false).unwrap();
        }
    }
    let ctx = egui::Context::default();
    ctx.set_pixels_per_point(scale);
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let size = egui::Vec2::new(1400.0, 1000.0);
    let mut frame = |a: &mut App, events: Vec<egui::Event>| {
        time += 0.05;
        let mut input = egui::RawInput { time: Some(time), events, focused: true, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(scale);
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let wait = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>), step: &str, until: &dyn Fn(&App) -> bool| {
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while !until(a) {
            frame(a, Vec::new());
            assert!(std::time::Instant::now() < deadline, "{scale}: {step}: timeout");
        }
    };
    a.open_folder_inner(dir.clone());
    if quick {
        a.lib.show_quick();
    }
    wait(&mut a, &mut frame, "grid", &|a| a.lib.test_cells.len() >= 4);
    assert!(a.in_grid());
    let original = a.lib.test_cells[1].0;
    a.create_virtual_copy(Some(original));
    wait(&mut a, &mut frame, "copy", &|a| a.entries.iter().any(|e| e.copy.is_some()) && a.lib.test_cells.len() >= 5);
    let copy = a.entries.iter().find(|e| e.copy.is_some()).unwrap().image;
    assert_ne!(copy, original);
    let pos = a.lib.test_cells.iter().find(|c| c.0 == copy).expect("copy tile in the grid").1.center();
    // double click as the window delivers it
    let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&mut a, vec![egui::Event::PointerMoved(pos)]);
    frame(&mut a, vec![button(true)]);
    frame(&mut a, vec![button(false)]);
    frame(&mut a, vec![button(true)]);
    frame(&mut a, vec![button(false)]);
    for _ in 0..10 {
        frame(&mut a, Vec::new());
    }
    assert!(!a.in_grid(), "{scale}: double click opens the single view");
    let i = shown(&a).expect("a photo is shown");
    assert_eq!(a.entries[i].image, copy, "{scale}: the single view shows {} instead of the virtual copy", a.entries[i].name);
    assert_eq!(a.lib.active_image(), Some(copy));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn virtual_copy_opens_itself() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    for scale in [1.0, 2.0, 2.5] {
        run_at(scale, false, false);
    }
    run_at(1.0, true, false);
    run_at(1.0, false, true);
}

/// The usual way: the original was developed, back to the grid, a copy made there, the copy double-clicked.
/// Developable RAWs (free test RAWs `testbilder/signature-edits` or `GRAINEO_TEST_RAW_DIR`; skipped without them).
#[test]
fn virtual_copy_of_a_developed_photo_opens_itself() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let src = crate::env::var_os("GRAINEO_TEST_RAW_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/signature-edits"));
    if !src.is_dir() {
        eprintln!("skipped: no test RAWs");
        return;
    }
    let _app_lock = crate::ui_test::app_lock();
    let root = std::env::temp_dir().join(format!("graineo-vcopy-raw-{}", std::process::id()));
    let dir = root.join("A");
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["05-DSC05627.ARW", "09-DSC_9514.NEF", "12-AM403348.ARW"] {
        std::fs::copy(src.join(n), dir.join(n)).unwrap();
    }
    {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
    }
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let mut frame = |a: &mut App, events: Vec<egui::Event>| {
        time += 0.05;
        let mut input = egui::RawInput { time: Some(time), events, focused: true, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let wait = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>), step: &str, until: &dyn Fn(&App) -> bool| {
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while !until(a) {
            frame(a, Vec::new());
            assert!(std::time::Instant::now() < deadline, "{step}: timeout (shown {:?})", shown(a).map(|i| a.entries[i].name.clone()));
        }
    };
    a.open_folder_inner(dir.clone());
    wait(&mut a, &mut frame, "grid", &|a| a.lib.test_cells.len() >= 3);
    let original = a.lib.test_cells[1].0;
    // develop the original, back to the grid
    a.lib.set_active(original);
    a.to_single_image();
    wait(&mut a, &mut frame, "original", &|a| a.loading.is_none() && a.current.as_ref().is_some_and(|c| a.entries[c.index].image == original));
    a.to_grid();
    for _ in 0..5 {
        frame(&mut a, Vec::new());
    }
    a.create_virtual_copy(Some(original));
    wait(&mut a, &mut frame, "copy", &|a| a.entries.iter().any(|e| e.copy.is_some()) && a.lib.test_cells.len() >= 4);
    let copy = a.entries.iter().find(|e| e.copy.is_some()).unwrap().image;
    let pos = a.lib.test_cells.iter().find(|c| c.0 == copy).expect("copy tile").1.center();
    let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&mut a, vec![egui::Event::PointerMoved(pos)]);
    frame(&mut a, vec![button(true)]);
    frame(&mut a, vec![button(false)]);
    frame(&mut a, vec![button(true)]);
    frame(&mut a, vec![button(false)]);
    for _ in 0..5 {
        frame(&mut a, Vec::new());
    }
    assert!(!a.in_grid());
    let i = shown(&a).expect("a photo is shown");
    assert_eq!(a.entries[i].image, copy, "the single view shows {} instead of the virtual copy", a.entries[i].name);
    wait(&mut a, &mut frame, "copy loaded", &|a| a.loading.is_none() && a.current.is_some());
    for _ in 0..20 {
        frame(&mut a, Vec::new());
    }
    let c = a.current.as_ref().unwrap().index;
    assert_eq!(a.entries[c].image, copy, "loaded: {} instead of the virtual copy", a.entries[c].name);
    let _ = std::fs::remove_dir_all(&root);
}
