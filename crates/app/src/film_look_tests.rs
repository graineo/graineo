use super::*;
fn group() -> Group {
    Group {
        name: "Synthetic package".into(),
        package: true,
        entries: (0..189)
            .map(|i| Entry {
                key: format!("look:film:SYNTHETIC-{i}"),
                name: format!("Look {i:03}"),
                choice: Choice::Look(format!("film:SYNTHETIC-{i}")),
                available: true,
            })
            .collect(),
    }
}
fn draw(state: &mut State, group: &Group) -> (usize, egui::FullOutput) {
    let ctx = egui::Context::default();
    let mut requested = Vec::new();
    let output = crate::ui_test::run(
        &ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300., 400.),
            )),
            ..Default::default()
        },
        |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                group_ui(
                    ui,
                    state,
                    group,
                    "look:film:SYNTHETIC-87",
                    &Settings::default(),
                    &HashMap::new(),
                    &HashSet::new(),
                    &mut requested,
                    &|_| None,
                );
            });
        },
    );
    (requested.len(), output)
}
#[test]
fn packs_collapsed_by_default_search_without_persistent_expansion() {
    let group = group();
    let mut state = State::default();
    state.area = Area::Look;
    assert_eq!(draw(&mut state, &group).0, 0);
    state.open_packages.insert(group.name.clone());
    let count = draw(&mut state, &group).0;
    assert!(
        count > 0 && count < 24,
        "{count} Vorschauen statt sichtbarer Teilmenge"
    );
    state.open_packages.clear();
    state.search = "Look 087".into();
    assert_eq!(draw(&mut state, &group).0, 1);
    assert!(state.open_packages.is_empty());
    state.search.clear();
    assert_eq!(draw(&mut state, &group).0, 0);
}
#[test]
fn offline_entries_request_no_thumbnails() {
    let mut group = group();
    for e in &mut group.entries {
        e.available = false;
    }
    let mut state = State::default();
    state.open_packages.insert(group.name.clone());
    let (count, output) = draw(&mut state, &group);
    assert_eq!(count, 0);
    assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.text().contains("Nicht verfügbar"))));
}
#[test]
fn film_shelf_only_materials_bw_first_and_sorted_by_name() {
    let groups = film_groups();
    assert!(!groups.is_empty());
    // under "Film & Look" the plain films are collapsed behind the film looks
    assert_eq!(groups[0].name, "Filme · Schwarzweiß");
    for g in groups {
        assert!(g.package);
        let names: Vec<_> = g.entries.iter().map(|e| e.name.to_lowercase()).collect();
        assert!(names.windows(2).all(|w| w[0] <= w[1]));
        for e in g.entries {
            assert!(matches!(e.choice, Choice::Film(_)));
            assert!(develop::film::stock(&e.key).is_some());
            assert!(!e.key.starts_with("look:"));
        }
    }
}
#[test]
fn selection_keeps_saved_look_and_darkroom_values() {
    let mut s = Settings::default();
    s.raw.look = "film:OLD".into();
    s.film.paper = Some("test-paper".into());
    s.film.grain = 0.42;
    s.exposure = 0.7;
    let film = Choice::Film("test-film".into()).apply(&s);
    assert!(film.film.active);
    assert_eq!(film.raw, s.raw);
    assert_eq!(film.exposure, s.exposure);
    assert_eq!(film.film.grain, 0.42);
    assert!(film.film.paper.is_none());
    let look = Choice::Look("film:NEW".into()).apply(&film);
    assert!(!look.film.active);
    assert_eq!(look.film.stock, "test-film");
    assert_eq!(look.film.grain, 0.42);
    assert_eq!(look.raw.look, "film:NEW");
    let sigma = Choice::Sigma("Neutral".into()).apply(&film);
    assert!(!sigma.film.active);
    assert_eq!(sigma.raw, s.raw);
    assert_eq!(sigma.color_mode.as_deref(), Some("Neutral"));
}
/// A Sigma colour mode switches black and white off; the black-and-white entry switches it on and keeps the mode.
#[test]
fn sigma_colour_mode_switches_black_and_white_off() {
    let mono = Settings { monochrome: true, color_mode: Some("Vivid".into()), ..Settings::default() };
    let colour = Choice::Sigma("Neutral".into()).apply(&mono);
    assert!(!colour.monochrome);
    assert_eq!(colour.color_mode.as_deref(), Some("Neutral"));
    let camera = Choice::Sigma(String::new()).apply(&mono);
    assert!(!camera.monochrome && camera.color_mode.is_none());
    let mut film = colour.clone();
    film.film.active = true;
    let back = Choice::SigmaMono.apply(&film);
    assert!(back.monochrome && !back.film.active);
    assert_eq!(back.color_mode.as_deref(), Some("Neutral"));
    // other choices leave black and white as it is
    assert!(Choice::Film("x".into()).apply(&mono).monochrome);
    assert!(Choice::Look("y".into()).apply(&mono).monochrome);
}
/// Sigma photo set to black and white: the camera group offers black and white (selected, own key) and all colour modes.
#[test]
fn sigma_black_and_white_offers_the_colour_modes() {
    let modes = vec!["Neutral".to_string(), "Vivid".to_string()];
    let mono = Settings { monochrome: true, ..Settings::default() };
    let groups = look_groups(&mono, None, "Standard", &modes);
    let keys: Vec<_> = groups[0].entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, [SIGMA_MONO_KEY, "sigma:", "sigma:Neutral", "sigma:Vivid"]);
    assert!(matches!(groups[0].entries[0].choice, Choice::SigmaMono));
    assert!(groups[0].entries[1..].iter().all(|e| matches!(e.choice, Choice::Sigma(_))));
    assert_eq!(look_card(&mono, None, "Standard").0, SIGMA_MONO_KEY, "black and white is the selected tile");
    // in colour: only the colour modes, as before
    let colour = look_groups(&Settings::default(), None, "Standard", &modes);
    let keys: Vec<_> = colour[0].entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, ["sigma:", "sigma:Neutral", "sigma:Vivid"]);
    assert_eq!(look_card(&Settings::default(), None, "Standard").0, "sigma:");
}
#[test]
fn fold_state_persisted_in_english_and_foreign_version_kept() {
    let root = std::path::PathBuf::from(crate::env::var_os("GRAINEO_DATA_DIR").unwrap())
        .join("profile-browser-state-test");
    std::fs::create_dir_all(&root).unwrap();
    let p = root.join("profile-browser.json");
    let mut state = State::default();
    state.area = Area::Look;
    state.open_packages.insert("Synthetic package".into());
    state.search = "transient".into();
    state.to_file(&p).unwrap();
    let bytes = std::fs::read(&p).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["area"], serde_json::json!("look"));
    assert!(json["openLookPackages"].is_array());
    assert!(json.get("suche").is_none());
    assert_eq!(
        json.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["area", "openLookPackages", "version"]
    );
    let again = State::from_file(&p);
    assert_eq!(again.area, Area::Look);
    assert!(again.open_packages.contains("Synthetic package"));
    assert!(again.search.is_empty());
    let future = br#"{"version":99,"future":42}"#;
    std::fs::write(&p, future).unwrap();
    let future_state = State::from_file(&p);
    future_state.to_file(&p).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), future);
}
#[test]
fn selected_missing_look_stays_visible_in_browser_header() {
    let root = std::path::PathBuf::from(crate::env::var_os("GRAINEO_DATA_DIR").unwrap())
        .join("profile-browser-photo-test");
    std::fs::create_dir_all(&root).unwrap();
    image::RgbImage::from_pixel(900, 600, image::Rgb([100, 80, 60]))
        .save(root.join("sample.png"))
        .unwrap();
    let ctx = egui::Context::default();
    let _app_lock = crate::ui_test::app_lock();
    let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
    app.open_folder_inner(root);
    app.to_single_image();
    let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
    while app.current.as_ref().is_none_or(|c| c.texture.is_none()) || app.loading.is_some() {
        app.handle_results(&ctx);
        app.schedule(0.);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let index = app.current.as_ref().unwrap().index;
    let s = &mut app.entries[index].settings;
    s.raw.look = "film:SYNTHETIC-NOT-INSTALLED".into();
    s.raw.look_name = "Selected hidden look".into();
    s.raw.look_package = "Synthetic package".into();
    app.profile_choice.area = Area::Look;
    app.profile_browser = true;
    let output = crate::ui_test::run(
        &ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300., 700.),
            )),
            ..Default::default()
        },
        |ui| app.profile_browser_ui(ui),
    );
    assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.text().contains("Selected hidden look"))));
    assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.text().contains("Nicht verfügbar"))));
}

#[test]
fn pack_header_click_toggles_without_jumping() {
    let ctx = egui::Context::default();
    let mut state = State::default();
    let group = group();
    let mut point = egui::Pos2::ZERO;
    let mut frame = |time: f64, events: Vec<egui::Event>| {
        let mut requested = Vec::new();
        let mut changed = false;
        let _ = crate::ui_test::run(
            &ctx,
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300., 400.),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                point = ui.cursor().min + egui::vec2(20., 16.);
                let result = group_ui(
                    ui,
                    &mut state,
                    &group,
                    "",
                    &Settings::default(),
                    &HashMap::new(),
                    &HashSet::new(),
                    &mut requested,
                    &|_| None,
                );
                changed = result.state_dirty;
            },
        );
        (
            point,
            changed,
            requested.len(),
            state.open_packages.contains(&group.name),
        )
    };
    let (p, _, count, open) = frame(1., vec![]);
    assert_eq!(count, 0);
    assert!(!open);
    let event = |pressed| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(2., vec![egui::Event::PointerMoved(p), event(true)]);
    let (p2, changed, count, open) = frame(2.05, vec![event(false)]);
    assert_eq!(p, p2);
    assert!(changed && open && count > 0 && count < 24);
    frame(3., vec![event(true)]);
    let (p3, changed, count, open) = frame(3.05, vec![event(false)]);
    assert_eq!(p, p3);
    assert!(changed && !open);
    assert_eq!(count, 0);
}
#[test]
fn offline_tile_is_not_selected_or_previewed() {
    for available in [false, true] {
        let ctx = egui::Context::default();
        let mut click = None;
        let p = egui::pos2(35., 35.);
        for (time, events) in [
            (1., vec![]),
            (
                2.,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            ),
            (
                2.05,
                vec![egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            ),
        ] {
            let _ = crate::ui_test::run(
                &ctx,
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300., 400.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let r = lr::thumb_grid_fav_available(
                        ui,
                        "availability-test",
                        &[(None, "Synthetic look")],
                        None,
                        3,
                        &[],
                        true,
                        &[available],
                    );
                    click = r.clicked;
                    if !available {
                        assert!(r.hovered.is_none());
                    }
                },
            );
        }
        assert_eq!(click, available.then_some(0));
    }
}
#[test]
fn favorites_read_legacy_and_write_only_new_english_file() {
    let root = std::path::PathBuf::from(crate::env::var_os("GRAINEO_DATA_DIR").unwrap())
        .join("profile-favorites-test");
    std::fs::create_dir_all(&root).unwrap();
    let old = root.join("favoriten.txt");
    let current = root.join("favorites.json");
    std::fs::write(&old, b"sigma:Neutral\nfilm-stock\n").unwrap();
    let saved_old = std::fs::read(&old).unwrap();
    let expected: HashSet<_> = ["sigma:Neutral".to_string(), "film-stock".to_string()]
        .into_iter()
        .collect();
    // A fresh alternative filename makes repeated test runs independent.
    let fresh = root.join(format!(
        "favorites-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert_eq!(favorites_from(&fresh, &old), expected);
    favorites_to_file(&fresh, &expected).unwrap();
    assert_eq!(favorites_from(&fresh, &old), expected);
    assert_eq!(std::fs::read(&old).unwrap(), saved_old);
    std::fs::write(&current, b"broken").unwrap();
    assert!(favorites_to_file(&current, &expected).is_err());
    assert_eq!(std::fs::read(&current).unwrap(), b"broken");
}

/// The head of "Film & Look" keeps its height whatever the name length (nothing below jumps), and
/// the symbol buttons sit inside it without moving the layout cursor.
#[test]
fn look_header_has_fixed_height() {
    let below = |title: &str| {
        let ctx = egui::Context::default();
        let mut y = 0.;
        crate::ui_test::run(
            &ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(330., 300.))),
                ..Default::default()
            },
            |ui| {
                let presentation = (String::new(), title.to_string(), "Schwarzweiß · Ilford Multigrade IV RC".to_string());
                look_header(ui, &presentation, None, &HashMap::new(), false);
                y = ui.cursor().top();
            },
        );
        y
    };
    let short = below("Neutral");
    let long = below("Ilford Delta 100 Professional Black and White, a very long name");
    assert_eq!(short, long);
    // two name rows, subline and spacing: clearly more than the 24 pt of the buttons
    assert!(short > 40., "{short}");
}
