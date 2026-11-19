//! "Ordner ansehen": reading the viewed folder again in the whole app. ⌥⌘R (the shortcut table) brings a photo that
//! was copied in meanwhile into the grid and the filmstrip and says so; the window getting the focus back looks again
//! quietly, but not more often than every few seconds.
//!
//! Needs `GRAINEO_DATA_DIR`. Synthetic photos in a temporary folder, never added to the library.

use crate::*;
use eframe::App as _;

#[test]
fn refresh_by_shortcut_and_on_focus() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let _app_lock = crate::ui_test::app_lock();
    let dir = std::env::temp_dir().join(format!("graineo-view-refresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let png = |name: &str| image::RgbImage::from_pixel(64, 48, image::Rgb([90, 120, 70])).save(dir.join(name)).unwrap();
    png("a.png");
    png("b.png");

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let mut frame = |a: &mut App, events: Vec<egui::Event>, focused: bool| {
        time += 0.05;
        let mut input = egui::RawInput { time: Some(time), events, focused, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 900.0))), ..Default::default() };
        input.viewports.insert(egui::ViewportId::ROOT, egui::ViewportInfo { focused: Some(focused), ..Default::default() });
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let wait = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>, bool), n: usize, what: &str| {
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.entries.len() != n {
            frame(a, Vec::new(), true);
            assert!(std::time::Instant::now() < deadline, "{what}: {} photos instead of {n}", a.entries.len());
        }
    };

    a.open_folder_inner(dir.clone());
    wait(&mut a, &mut frame, 2, "viewed folder");
    // ⌥⌘R: the photo copied in meanwhile appears, the status says so
    png("c.png");
    let key = egui::Event::Key { key: egui::Key::R, physical_key: Some(egui::Key::R), pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND | egui::Modifiers::ALT };
    frame(&mut a, vec![key], true);
    wait(&mut a, &mut frame, 3, "after ⌥⌘R");
    // the message lands in the status of the title bar (a short success)
    let said = a.notes.done.as_ref().map(|d| d.0.clone()).unwrap_or_default();
    assert_eq!(said, crate::library_ui::view_folder_tree_text(1, 0));
    // back in the window after a while: it looks again by itself
    png("d.png");
    frame(&mut a, Vec::new(), false);
    a.lib.view_test_age();
    frame(&mut a, Vec::new(), true);
    wait(&mut a, &mut frame, 4, "focus back");
    // focus back again at once: no new look (no polling, not more often than every few seconds)
    png("e.png");
    frame(&mut a, Vec::new(), false);
    for _ in 0..10 {
        frame(&mut a, Vec::new(), true);
    }
    assert_eq!(a.entries.len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}
