//! Central settings example. Photo-related settings pages remain separate.
use super::*;

pub(super) fn general(ui: &mut egui::Ui) {
    group(ui, i18n::text(Text::Language), |ui| {
        row(
            ui,
            i18n::text(Text::Language),
            None,
            |ui| {
                let mut selected = i18n::locale::choice();
                let options = [
                    (i18n::Choice::System, i18n::text(Text::LanguageSystem)),
                    (i18n::Choice::De, "Deutsch"),
                    (i18n::Choice::En, "English"),
                    (i18n::Choice::Ja, "日本語"),
                    (i18n::Choice::Fr, "Français"),
                    (i18n::Choice::Es, "Español"),
                ];
                let shown = options.iter().find(|(c, _)| *c == selected).map(|(_, n)| *n).unwrap_or("");
                let before = selected;
                egui::ComboBox::from_id_salt("prefs-language").selected_text(shown).width(170.0).show_ui(ui, |ui| {
                    for (c, name) in &options {
                        ui.selectable_value(&mut selected, *c, *name);
                    }
                });
                if selected != before {
                    let result = i18n::locale::choose(selected);
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::unique("language-save-error"), result.err())
                    });
                    ui.ctx().request_repaint();
                }
            },
        );
        if let Some(error) = ui.ctx().data(|d| {
            d.get_temp::<Option<String>>(egui::Id::unique("language-save-error"))
                .flatten()
        }) {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(i18n::language_error(&error)).color(lr::TEXT_DIM()),
                )
                .wrap(),
            );
        }
    });
    group(ui, i18n::text(Text::PrefsStart), |ui| {
        let file = crate::app_file("module.txt");
        let mut value = file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| legacy_names::translate(legacy_names::app::START_MODULE, s.trim()).to_string())
            .unwrap_or_else(|| "library".into());
        row(
            ui,
            i18n::text(Text::PrefsOnStart),
            None,
            |ui| {
                // The stored values are the module names; the labels shown are translated.
                if lr::segmented(
                    ui,
                    &mut value,
                    &[
                        ("library".to_string(), i18n::text(Text::Grid)),
                        ("develop".to_string(), i18n::text(Text::SinglePhoto)),
                    ],
                ) {
                    if let Some(file) = &file {
                        let _ = std::fs::write(file, &value);
                    }
                }
            },
        );
    });
    group(ui, i18n::text(Text::Data), |ui| {
        row(ui, i18n::text(Text::DataFolder), None, |ui| {
            if ui.button(i18n::text(Text::MenuShowInFinder)).clicked() {
                crate::system::open(library::data_dir());
            }
            // long paths are shortened in the middle; the full path opens via "Show in Finder"
            path_text(ui, &library::data_dir().display().to_string(), 340.0);
        });
        separator(ui);
        row(ui, i18n::text(Text::Version), None, |ui| {
            ui.label(egui::RichText::new(env!("CARGO_PKG_VERSION")).color(lr::TEXT_DIM()));
        });
    });
    default_apps(ui);
    explorer_menu(ui);
    group(ui, crate::tr("Updates"), |ui| {
        row(ui, crate::tr("Automatisch nach Updates suchen"), Some(crate::tr("Einmal am Tag eine Anfrage an GitHub; es werden keine Daten gesendet.")), |ui| {
            crate::help::info(ui, "prefs-updates", "Updates", &crate::help::UPDATES);
            let mut on = library::update::enabled();
            if toggle(ui, &mut on) {
                library::update::set_enabled(on);
            }
        });
        separator(ui);
        let line = crate::update_ui::line();
        row(ui, crate::tr("Neue Version"), Some(&line), |ui| {
            if crate::update_ui::checking() {
                ui.spinner();
            } else if ui.button(crate::tr("Jetzt nach Updates suchen")).clicked() {
                crate::update_ui::check_now(ui.ctx());
            }
        });
    });
}

/// Default app per RAW format: which program opens the file in the Finder, "Use graineo" sets it.
/// The system is asked when the page opens and after setting (cached for a few seconds, never per frame).
fn default_apps(ui: &mut egui::Ui) {
    if !cfg!(target_os = "macos") {
        return;
    }
    type Cache = Option<(f64, Vec<Option<(String, String)>>)>;
    static CACHE: Mutex<Cache> = Mutex::new(None);
    static MESSAGE: Mutex<Option<String>> = Mutex::new(None);
    let now = ui.input(|i| i.time);
    let handlers = {
        let mut c = CACHE.lock().unwrap();
        if c.as_ref().is_none_or(|(t, _)| now - *t > 5.0 || now < *t) {
            *c = Some((now, crate::default_app::FORMATS.iter().map(|e| crate::default_app::handler(e)).collect()));
        }
        c.as_ref().map(|(_, v)| v.clone()).unwrap_or_default()
    };
    let own = crate::default_app::own_id();
    let more_id = egui::Id::unique("prefs-default-apps-more");
    let more = ui.ctx().data(|d| d.get_temp::<bool>(more_id).unwrap_or(false));
    group(ui, crate::tr("Standardprogramm"), |ui| {
        for (k, ext) in crate::default_app::FORMATS.iter().enumerate() {
            if k > 0 && !more {
                break;
            }
            if k > 0 {
                separator(ui);
            }
            let current = handlers.get(k).cloned().flatten();
            let is_own = matches!((&current, &own), (Some((id, _)), Some(o)) if crate::default_app::same(id, o));
            let name = crate::trf!("Standardprogramm für {a}", a = ext.to_uppercase());
            let shown = match &current {
                Some((_, n)) => n.clone(),
                None => crate::tr("keins").to_string(),
            };
            row(ui, &name, None, |ui| {
                if !is_own {
                    let enabled = own.is_some();
                    if ui.add_enabled(enabled, egui::Button::new(crate::tr("graineo verwenden"))).clicked() {
                        let r = crate::default_app::set_default(ext);
                        *MESSAGE.lock().unwrap() = r.err();
                        *CACHE.lock().unwrap() = None;
                    }
                }
                value(ui, &shown, if is_own { Signal::Green } else { Signal::Gray });
            });
        }
        let text = if more { crate::tr("Weniger Formate") } else { crate::tr("Weitere Formate (CR3, RAF, DNG, ARW, NEF, NRW)") };
        actions(ui, |ui| {
            if ui.button(text).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(more_id, !more));
            }
            if own.is_none() {
                ui.label(egui::RichText::new(crate::tr("Nur in der installierten App einstellbar.")).small().color(lr::TEXT_DIM()));
            }
        });
        if let Some(m) = MESSAGE.lock().unwrap().clone() {
            ui.label(egui::RichText::new(m).small().color(lr::TEXT_DIM()));
        }
    });
}

/// Windows: "Mit graineo ansehen" in the Explorer menu (folders, folder background, drives, photos). The state is read
/// from the registry when the page opens (cached for a few seconds, never per frame). Other systems: not shown.
fn explorer_menu(ui: &mut egui::Ui) {
    use library::platform::explorer_menu as menu;
    if !menu::AVAILABLE {
        return;
    }
    static CACHE: Mutex<Option<(f64, Option<bool>)>> = Mutex::new(None);
    static MESSAGE: Mutex<Option<String>> = Mutex::new(None);
    let now = ui.input(|i| i.time);
    let state = {
        let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if c.as_ref().is_none_or(|(t, _)| now - *t > 5.0 || now < *t) {
            *c = Some((now, menu::state()));
        }
        c.as_ref().and_then(|(_, s)| *s)
    };
    group(ui, crate::tr("Explorer"), |ui| {
        row(ui, crate::tr("Im Explorer-Menü anbieten"), Some(crate::tr("Rechtsklick auf Ordner, Laufwerke und Fotos: „Mit graineo ansehen“. Unter Windows 11 bei „Weitere Optionen anzeigen“.")), |ui| {
            let mut on = state.is_some();
            if toggle(ui, &mut on) {
                let r = menu::set(on.then(|| crate::tr(EXPLORER_MENU_LABEL)));
                *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = r.err();
                *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        });
        if let Some(m) = MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            ui.label(egui::RichText::new(m).small().color(lr::TEXT_DIM()));
        }
    });
}

pub(super) fn area_label(index: usize) -> &'static str {
    let keys = [
        Text::PrefsGeneral,
        Text::PrefsInterface,
        Text::PrefsLibrary,
        Text::PrefsDevelop,
        Text::PrefsExport,
        Text::PrefsLenses,
        Text::PrefsSearch,
        Text::PrefsModules,
        Text::PrefsStorage,
        Text::PrefsLooks,
        Text::PrefsBrowser,
    ];
    keys.get(index).map(|&key| i18n::text(key)).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn german_central_texts_match_the_existing_strings() {
        use i18n::{text_in, Language, Text};
        for (key, old) in [
            (Text::PrefsTitle, "Einstellungen"),
            (Text::PrefsStart, "Start"),
            (Text::PrefsOnStart, "Beim Start"),
            (
                Text::PrefsRemember,
                "Die App merkt sich außerdem die zuletzt gewählte Quelle und das aktive Bild.",
            ),
            (Text::Data, "Daten"),
            (Text::DataFolder, "Datenordner"),
            (Text::MenuShowInFinder, "Im Finder zeigen"),
            (Text::Version, "Version"),
            (Text::Appearance, "Darstellung"),
            (Text::InterfaceScale, "Größe der Oberfläche"),
            (
                Text::ScaleHelp,
                "Schrift, Symbole und Abstände gemeinsam; die Bildanzeige bleibt exakt.",
            ),
        ] {
            assert_eq!(text_in(Language::De, key).as_bytes(), old.as_bytes());
        }
        for (index, old) in SECTIONS.iter().enumerate() {
            let _guard = i18n::locale::TestLanguage::set(Language::De);
            assert_eq!(area_label(index).as_bytes(), old.as_bytes());
        }
    }
}
#[cfg(test)]
mod language_state_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let p = entry.path();
            if p.is_dir() {
                out.extend(files(&p));
            } else {
                out.insert(p.clone(), std::fs::read(p).unwrap());
            }
        }
        out
    }
    #[test]
    fn language_switch_preserves_photo_selection_render_and_sidecars() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let _guard = i18n::locale::TestLanguage::set(i18n::Language::De);
        let original_language = library::settings::value("language");
        struct Restore(serde_json::Value);
        impl Drop for Restore {
            fn drop(&mut self) {
                library::settings::set("language", self.0.clone());
            }
        }
        let _restore = Restore(original_language);
        let dir = std::env::temp_dir().join(format!("graineo-i18n-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("Übung 日本.png");
        image::RgbImage::from_fn(180, 120, |x, y| image::Rgb([x as u8, y as u8, 90]))
            .save(&photo)
            .unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut app = crate::App::new(&cc);
        app.open_folder_inner(dir.clone());
        app.to_single_image();
        let end = std::time::Instant::now() + crate::ui_test::WAIT;
        while app.current.as_ref().is_none_or(|c| c.texture.is_none()) || app.loading.is_some() {
            app.handle_results(&ctx);
            app.schedule(0.0);
            assert!(
                std::time::Instant::now() < end,
                "synthetisches Bild wird geladen"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.save_current();
        let current = app.current.as_ref().unwrap().index;
        let settings = app.entries[current].settings.clone();
        let doc = rawdev::RawDoc::open(&photo, &|| true).unwrap().unwrap();
        let render = doc.render_preview(&settings).data;
        let entries = app
            .entries
            .iter()
            .map(|e| {
                (
                    e.path.clone(),
                    e.selected,
                    serde_json::to_string(&e.settings).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let before = files(&dir);
        let history = app.history.snapshot();
        let tabs = (app.tab, app.tool_tab);
        for choice in [i18n::Choice::En, i18n::Choice::Ja, i18n::Choice::Fr, i18n::Choice::Es, i18n::Choice::De] {
            i18n::locale::choose(choice).unwrap();
            // Drawing the settings example must not run a photo action or alter selection.
            let output = crate::ui_test::run(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| general(ui),
            );
            assert!(!output.shapes.is_empty());
            assert_eq!(app.current.as_ref().unwrap().index, current);
            assert!((app.tab, app.tool_tab) == tabs);
            assert_eq!(
                app.entries
                    .iter()
                    .map(|e| (
                        e.path.clone(),
                        e.selected,
                        serde_json::to_string(&e.settings).unwrap()
                    ))
                    .collect::<Vec<_>>(),
                entries
            );
            assert_eq!(app.history.snapshot(), history);
            assert_eq!(doc.render_preview(&settings).data, render);
            app.save_current();
            assert_eq!(
                files(&dir),
                before,
                "Original und Begleitdateien bleiben bytegleich"
            );
            assert_eq!(library::settings::value("language"), choice.key());
        }
    }
}

#[cfg(test)]
mod window_geometry_tests {
    use super::*;
    use egui::{Pos2, Rect, Vec2};
    fn text_bounds(shapes: &[egui::epaint::ClippedShape], screen: Rect) -> usize {
        fn visit(shape: &egui::Shape, clip: Rect, screen: Rect, count: &mut usize) {
            match shape {
                egui::Shape::Text(t) => {
                    // the galley's own rect: right-aligned labels (paths in a right-to-left row) start left of `pos`
                    let bounds = t.visual_bounding_rect();
                    // Scrollable content can lie beyond its vertical clip. Visible text must
                    // never extend horizontally past the viewport; paths may use ellipsis.
                    if bounds.intersects(clip) {
                        assert!(
                            bounds.right() <= screen.right() + 2.0,
                            "text beyond viewport: {:?} {:?}",
                            t.galley.text(),
                            bounds
                        );
                        assert!(
                            bounds.left() >= screen.left() - 2.0,
                            "text left of viewport: {:?}",
                            t.galley.text()
                        );
                        // only the navigation band (10 pt); the same word can also be a heading of a page in a longer language
                        let nav_size = t.galley.job.sections.first().is_some_and(|s| (s.format.font_id.size - 10.0).abs() < 0.01);
                        if nav_size && (0..SECTIONS.len()).any(|i| area_label(i) == t.galley.text()) {
                            assert!(
                                bounds.width() <= (780.0 - 20.0) / 11.0,
                                "navigation label exceeds slot: {} {:?} {}",
                                t.galley.text(), bounds, t.galley.job.sections[0].format.font_id.size
                            );
                            assert!(
                                bounds.height() <= 40.0,
                                "navigation label exceeds reserved height: {}",
                                t.galley.text()
                            );
                        }
                        *count += 1;
                    }
                }
                egui::Shape::Vec(children) => {
                    for child in children {
                        visit(child, clip, screen, count);
                    }
                }
                _ => {}
            }
        }
        let mut count = 0;
        for shape in shapes {
            visit(&shape.shape, shape.clip_rect, screen, &mut count);
        }
        count
    }
    #[test]
    fn central_settings_fit_the_minimum_viewport_in_both_languages() {
        check(1.0);
        check(2.0);
    }
    fn check(ppp: f32) {
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        ctx.global_style_mut(|s| s.animation_time = 0.0);
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0));
        for language in i18n::Language::ALL {
            let _guard = i18n::locale::TestLanguage::set(language);
            // All pages without background work (Search, Modules, Storage and Looks start jobs)
            for page in [0usize, 1, 2, 3, 4, 5] {
                let mut open = true;
                let mut area = page;
                let mut output = None;
                // First frame lets egui measure the window before centering it.
                for n in 0..6 {
                    let mut input = egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(n as f64),
                        ..Default::default()
                    };
                    input
                        .viewports
                        .entry(egui::ViewportId::ROOT)
                        .or_default()
                        .native_pixels_per_point = Some(ppp);
                    output = Some(crate::ui_test::run(&ctx, input, |_| {
                        super::super::show_window(&ctx, &mut open, &mut area)
                    }));
                }
                assert!(text_bounds(&output.unwrap().shapes, screen) > 15);
                assert_eq!(area, page);
                assert!(open);
            }
        }
    }
}
