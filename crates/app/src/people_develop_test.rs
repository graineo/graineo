//! People → Develop: the photo of the face chosen in the people view is the current photo of the app. Stars (keys and
//! menu) go to it and not to the grid's photo, Develop opens exactly it, its sliders change it, and the way back leads
//! to the same person and face, with the grid's source and choice from before underneath.
//!
//! Needs `GRAINEO_DATA_DIR` (own library in the test run's data folder). Synthetic photos, a face entered by hand.

use crate::*;
use eframe::App as _;

#[test]
fn people_photo_is_current_and_develop_leads_back() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let root = std::env::temp_dir().join(format!("graineo-people-develop-{}", std::process::id()));
    let (dir_a, dir_b) = (root.join("A"), root.join("B"));
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    for (dir, name, c) in [(&dir_a, "a.png", [150u8, 90, 60]), (&dir_a, "b.png", [60, 150, 90]), (&dir_b, "face.png", [90, 60, 150])] {
        image::RgbImage::from_pixel(800, 600, image::Rgb(c)).save(dir.join(name)).unwrap();
    }
    let image_of = |db: &library::db::Db, name: &str| -> i64 { db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id = b.file WHERE d.name = ?1", [name], |r| r.get(0)).unwrap() };
    let (face_img, a_img, face) = {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
        let face_img = image_of(&db, "face.png");
        let a_img = image_of(&db, "a.png");
        db.conn.execute("INSERT INTO face(image, x, y, w, h, source, status, local) VALUES(?1, 0.4, 0.3, 0.2, 0.3, 'manual', 'unknown', 1)", [face_img]).unwrap();
        let face = db.conn.last_insert_rowid();
        library::people::apply_answer_with_name(&db, face, "Testperson").unwrap();
        (face_img, a_img, face)
    };

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let _app_lock = crate::ui_test::app_lock();
    let mut a = App::new(&cc);
    let mut time = 10.0;
    let mut frame = |a: &mut App, events: Vec<egui::Event>| {
        time += 0.1;
        let mut input = egui::RawInput { time: Some(time), events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        a.handle_results(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(15));
    };
    let wait = |a: &mut App, frame: &mut dyn FnMut(&mut App, Vec<egui::Event>), step: &str, until: &dyn Fn(&App) -> bool| {
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while !until(a) {
            frame(a, Vec::new());
            assert!(std::time::Instant::now() < deadline, "{step}: timeout (people open {}, chosen {:?}, focus {:?}, focus in people {:?})", crate::people_ui::is_open(), crate::people_ui::chosen_for_test(), a.lib.people_focus(), crate::people_ui::focus_image());
        }
    };
    let key = |k: egui::Key| vec![egui::Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers: Default::default() }];

    // the grid: folder A, photo a active
    a.open_folder_inner(dir_a.clone());
    assert_eq!(a.entries.len(), 2, "folder A in the library");
    a.lib.set_active(a_img);
    let source_before = a.lib.source_title();
    // people view, the face chosen
    crate::people_ui::open_at(crate::people_ui::ToolTab::People);
    crate::people_ui::choose_face_for_test(face);
    wait(&mut a, &mut frame, "face chosen", &|a| a.lib.people_focus() == Some(face_img));
    assert!(a.in_people() && a.has_photo());
    assert_eq!(a.tool_rail_active(), workspace::ToolTab::Info, "people view: the rail shows Info");

    // 1. stars by key and the flag by the menu command go to the face's photo, not to the grid's photo
    frame(&mut a, key(egui::Key::Num3));
    assert_eq!(a.lib.stars_of(face_img), Some(3), "key 3 on the chosen face's photo");
    assert_eq!(a.lib.stars_of(a_img), Some(0), "the grid's photo stays as it was");
    a.rate(crate::rating::Change::Stars(4), false);
    assert_eq!((a.lib.stars_of(face_img), a.lib.stars_of(a_img)), (Some(4), Some(0)));
    assert_eq!(crate::people_ui::chosen_for_test().0, Some(face), "rating keeps the face chosen");

    // 2. Develop (button, module bar, rail and D all go through `to_single_image`): exactly this photo
    frame(&mut a, key(egui::Key::D));
    assert!(!a.in_grid(), "D opens Develop");
    assert!(a.lib.people_pending(), "the way back to People is open");
    wait(&mut a, &mut frame, "photo loaded", &|a| a.loading.is_none() && a.current.as_ref().is_some_and(|c| c.texture.is_some()));
    let i = a.current.as_ref().unwrap().index;
    assert_eq!(a.entries[i].image, face_img, "Develop shows the face's photo");
    assert_eq!(a.active_entry(), Some(i), "the sliders belong to it");
    // a slider move: on the shown photo, saved for it
    a.entries[i].settings.exposure = 1.25;
    for _ in 0..3 {
        frame(&mut a, Vec::new());
    }
    let face_path = a.entries[i].path.clone();

    // 3. back (Esc, "← Personen"): the same person and face, the grid's source and choice from before underneath
    frame(&mut a, key(egui::Key::Escape));
    assert!(a.in_grid() && crate::people_ui::is_open(), "Esc leads back to People");
    assert!(!a.lib.people_pending());
    wait(&mut a, &mut frame, "face chosen again", &|a| crate::people_ui::chosen_for_test().0 == Some(face) && a.lib.people_focus() == Some(face_img));
    assert_eq!(a.lib.source_title(), source_before, "the grid's source is back");
    assert_eq!(a.lib.active_image(), Some(a_img), "the grid's photo is back");
    assert!(library::store::flush(std::time::Duration::from_secs(10)));
    assert_eq!(crate::raw_app::load_settings(&face_path, None).0.exposure, 1.25, "the slider changed the face's photo");
    assert_eq!(crate::raw_app::load_settings(&dir_a.join("a.png"), None).0.exposure, 0.0, "the grid's photo is unchanged");

    // 4. the editing tools of the rail open it in Develop as well
    a.choose_tool(workspace::ToolTab::Edit);
    assert!(!a.in_grid() && a.lib.people_pending());
    assert_eq!(a.tool_tab, workspace::ToolTab::Edit);
    a.to_grid();
    assert!(crate::people_ui::is_open());
    let _ = std::fs::remove_dir_all(&root);
}
