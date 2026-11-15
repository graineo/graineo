//! Grid: choosing several photos with the mouse. ⌘-click adds a photo, ⇧-click adds the run up to the clicked photo,
//! ⇧-drag (also when it starts on a photo) frames photos, a drag on free space frames them as well – with the
//! modifiers as the window delivers them (in the raw input and in the pointer events), through every filter of the app.
//!
//! Needs `GRAINEO_DATA_DIR` (own library in the test run's data folder). Synthetic photos.

use crate::*;
use eframe::App as _;
use std::collections::HashSet;

#[test]
fn grid_selects_several_photos_with_cmd_shift_and_band() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    // one app test at a time (also the library setup below: it writes to the shared test library)
    let _app_lock = crate::ui_test::app_lock();
    let root = std::env::temp_dir().join(format!("graineo-grid-select-{}", std::process::id()));
    let dir = root.join("A");
    std::fs::create_dir_all(&dir).unwrap();
    for k in 0..6u8 {
        image::RgbImage::from_pixel(400, 300, image::Rgb([40 + k * 30, 90, 60])).save(dir.join(format!("p{k}.png"))).unwrap();
    }
    {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
    }

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let mut frame = |a: &mut App, mut events: Vec<egui::Event>, modifiers: egui::Modifiers| {
        time += 0.05;
        // egui 0.37 takes the modifiers from this event (as egui-winit sends it), not from `RawInput::modifiers`
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let mut input = egui::RawInput { time: Some(time), events, focused: true, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let none = egui::Modifiers::NONE;
    let button = |pos: egui::Pos2, pressed: bool, modifiers: egui::Modifiers| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers };
    // a click as the window delivers it: move there, press, release (modifiers held throughout)
    let click = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>, egui::Modifiers), pos: egui::Pos2, m: egui::Modifiers| {
        frame(a, vec![egui::Event::PointerMoved(pos)], m);
        frame(a, vec![button(pos, true, m)], m);
        frame(a, vec![button(pos, false, m)], m);
        frame(a, Vec::new(), m);
        frame(a, Vec::new(), none);
    };
    let drag = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>, egui::Modifiers), from: egui::Pos2, to: egui::Pos2, m: egui::Modifiers| {
        frame(a, vec![egui::Event::PointerMoved(from)], m);
        frame(a, vec![button(from, true, m)], m);
        for k in 1..=10 {
            frame(a, vec![egui::Event::PointerMoved(from.lerp(to, k as f32 / 10.0))], m);
        }
        frame(a, vec![button(to, false, m)], m);
        frame(a, Vec::new(), m);
        frame(a, Vec::new(), none);
    };

    a.open_folder_inner(dir.clone());
    let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
    while a.lib.test_cells.len() < 6 {
        frame(&mut a, Vec::new(), none);
        assert!(std::time::Instant::now() < deadline, "grid with six photos: timeout ({} cells)", a.lib.test_cells.len());
    }
    assert!(a.in_grid());
    let cells: Vec<(i64, egui::Rect)> = a.lib.test_cells.clone();
    let id = |k: usize| cells[k].0;
    let at = |k: usize| cells[k].1.center();
    let set = |ks: &[usize]| ks.iter().map(|&k| id(k)).collect::<HashSet<i64>>();

    // a plain click: one photo
    click(&mut a, &mut frame, at(0), none);
    assert_eq!(a.lib.selected_for_test(), set(&[0]), "click selects one photo");
    // ⌘-click adds one
    click(&mut a, &mut frame, at(2), egui::Modifiers::COMMAND);
    assert_eq!(a.lib.selected_for_test(), set(&[0, 2]), "⌘-click adds a photo");
    // ⇧-click adds the run from the active photo to the clicked one
    click(&mut a, &mut frame, at(4), egui::Modifiers::SHIFT);
    assert_eq!(a.lib.selected_for_test(), set(&[0, 2, 3, 4]), "⇧-click adds a run");
    // ⌘-click on a selected photo takes it out again
    click(&mut a, &mut frame, at(3), egui::Modifiers::COMMAND);
    assert_eq!(a.lib.selected_for_test(), set(&[0, 2, 4]), "⌘-click takes a photo out");
    // a slow frame: the release of the button and of ⌘ arrive together – still a ⌘-click (the keys of the press count)
    let late = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>, egui::Modifiers), pos: egui::Pos2, m: egui::Modifiers| {
        frame(a, vec![egui::Event::PointerMoved(pos)], m);
        frame(a, vec![button(pos, true, m)], m);
        frame(a, vec![button(pos, false, m), egui::Event::ModifiersChanged(none)], m);
        frame(a, Vec::new(), none);
    };
    late(&mut a, &mut frame, at(1), egui::Modifiers::COMMAND);
    assert_eq!(a.lib.selected_for_test(), set(&[0, 1, 2, 4]), "⌘-click with ⌘ let go in the same frame as the button");
    click(&mut a, &mut frame, at(0), none);
    late(&mut a, &mut frame, at(2), egui::Modifiers::SHIFT);
    assert_eq!(a.lib.selected_for_test(), set(&[0, 1, 2]), "⇧-click with ⇧ let go in the same frame as the button");

    // ⇧-drag starting on a photo: frames photos, adds them to the selection
    click(&mut a, &mut frame, at(5), none);
    assert_eq!(a.lib.selected_for_test(), set(&[5]));
    drag(&mut a, &mut frame, at(0), at(2), egui::Modifiers::SHIFT);
    assert_eq!(a.lib.selected_for_test(), set(&[0, 1, 2, 5]), "⇧-drag frames photos");

    // a plain drag from free space below the photos up over them: a new selection
    let last = cells.iter().map(|c| c.1.bottom()).fold(0.0, f32::max);
    let free = egui::pos2(at(0).x, last + 40.0);
    drag(&mut a, &mut frame, free, at(1), none);
    let got = a.lib.selected_for_test();
    assert!(got.len() > 1 && got.contains(&id(0)) && got.contains(&id(1)), "drag on free space frames photos: {got:?}");

    // a plain click still selects just one photo
    click(&mut a, &mut frame, at(3), none);
    assert_eq!(a.lib.selected_for_test(), set(&[3]));
    let _ = std::fs::remove_dir_all(&root);
}
