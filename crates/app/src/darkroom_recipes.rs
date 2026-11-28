//! Darkroom, phase 3: recipe book. A recipe holds the complete film settings
//! (`settings.film`: film, development, push, paper, grade, tone, matte, grain, format, taking filter,
//! brightness, border) under a name. User recipes live in the same store as the other
//! presets (`presets.json`, scope `Film`), the film recipes of the library appear as
//! built-in ones (cannot be deleted). A single recipe file can be exported and imported.

use eframe::egui;

use crate::presets::{self, Preset, Scope};
use crate::{dialog, lr, App};

/// Where a recipe comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Source {
    Own(usize),
    BuiltIn(usize),
}

/// What the user triggered in the recipe book.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    Load(Source),
    Save(String),
    Rename(usize, String),
    Delete(usize),
    Export(Source),
    Import,
}

/// State of the recipe book.
#[derive(Default)]
pub struct Recipes {
    pub opened: bool,
    pub images: crate::recipe_grid::Previews,
    /// Input field "Recipe name" for saving
    pub name: String,
    pub selection: Option<Source>,
    /// Rename in progress: new name
    rename: Option<String>,
    /// Deletion waiting for confirmation (index of the user recipes)
    pub delete: Option<usize>,
    /// Last feedback (one line)
    pub message: String,
    /// Recipe file written after the save location has been chosen
    pub export_text: Option<String>,
    /// Built-in film recipes (fetched once from the library)
    bundled: Vec<Preset>,
    /// Look drawer below the handle open
    pub drawer: bool,
    /// Name of the look last applied, per photo (for "changed" in the handle)
    pub applied: std::collections::HashMap<i64, String>,
    /// "Save as film look": name in the dialog
    pub save_name: Option<String>,
    /// Look strip: 0 all, 1 black and white, 2 colour
    pub filter: u8,
    /// Hover preview of a card: look under the pointer and since when
    pub hover_since: Option<(Source, f64)>,
    /// Hover preview job on its way: only one at a time, the newest wish follows when it is back
    pub hover_sent: Option<(develop::Settings, f64)>,
    /// A hover preview of the strip is shown
    pub look_hover: bool,
    /// Card width of the look strip (remembered in the settings)
    card_width: Option<f32>,
}

impl Recipes {
    pub(crate) fn card_w(&self) -> f32 {
        self.card_width.unwrap_or_else(|| library::settings::value("filmLookCardWidth").as_f64().map_or(190.0, |v| v as f32))
    }

    pub(crate) fn set_card_w(&mut self, w: f32) {
        self.card_width = Some(w);
        library::settings::set("filmLookCardWidth", serde_json::json!(w));
    }

    pub(crate) fn built_in(&mut self) -> &[Preset] {
        if self.bundled.is_empty() {
            self.bundled = presets::library().into_iter().find(|c| c.name == "Film-Rezepte").map(|c| c.presets).unwrap_or_default();
        }
        &self.bundled
    }
}

/// Display name of a recipe's film (short).
pub(crate) fn film_name(p: &Preset) -> String {
    let id = &p.settings.film.stock;
    let Some(stock) = develop::film::stock(id) else { return id.clone() };
    let mut name = stock.name.split(',').next().unwrap_or(&stock.name).trim().to_string();
    for suffix in ["Black and White Negative Film", "Black and White Reversal", "Negative Film", "Professional"] {
        name = name.replace(suffix, "");
    }
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl App {
    /// Run a recipe book action (without file dialog; export/import report back to the caller).
    pub(crate) fn darkroom_run_recipe(&mut self, a: Action) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        match a {
            Action::Load(q) => {
                let film = match q {
                    Source::Own(i) => self.presets.get(i).map(|p| p.film_recipe()),
                    Source::BuiltIn(i) => self.darkroom.recipes.built_in().get(i).map(|p| p.film_recipe()),
                };
                let name = match q {
                    Source::Own(i) => self.presets.get(i).map(|p| p.name.clone()),
                    Source::BuiltIn(i) => self.darkroom.recipes.built_in().get(i).map(|p| crate::tr(&p.name).to_string()),
                };
                if let (Some(f), Some(name)) = (film, name) {
                    self.darkroom.recipes.applied.insert(self.entries[index].image, name);
                    self.entries[index].settings.film = f;
                    self.darkroom.recipes.message = crate::tr("Filmlook geladen.").into();
                }
            }
            Action::Save(name) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    self.darkroom.recipes.message = "Bitte einen Namen eingeben.".into();
                    return;
                }
                let film = self.entries[index].settings.film.clone();
                if !film.is_active() {
                    self.darkroom.recipes.message = crate::tr("Ohne Film gibt es nichts zu speichern.").into();
                    return;
                }
                let new_preset = presets::recipe(&name, &film);
                match self.presets.iter().position(|p| p.scope == Scope::Film && p.name == name) {
                    Some(i) => {
                        self.presets[i] = new_preset;
                        self.darkroom.recipes.selection = Some(Source::Own(i));
                    }
                    None => {
                        self.presets.push(new_preset);
                        self.darkroom.recipes.selection = Some(Source::Own(self.presets.len() - 1));
                    }
                }
                presets::save(&self.presets);
                self.darkroom.recipes.message = crate::trf!("Gespeichert: {name}", name = name);
            }
            Action::Rename(i, name) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                if let Some(p) = self.presets.get_mut(i).filter(|p| p.scope == Scope::Film) {
                    p.name = name;
                    presets::save(&self.presets);
                    self.darkroom.recipes.message = "Umbenannt.".into();
                }
            }
            Action::Delete(i) => {
                // user recipes only (scope film), after confirmation in the dialog
                if self.presets.get(i).is_some_and(|p| p.scope == Scope::Film) {
                    let p = self.presets.remove(i);
                    presets::save(&self.presets);
                    self.darkroom.recipes.selection = None;
                    self.darkroom.recipes.message = crate::trf!("Gelöscht: {a}", a = p.name);
                }
            }
            Action::Export(_) | Action::Import => {}
        }
    }

    /// The recipe for a source.
    pub(crate) fn darkroom_recipe_of(&mut self, q: Source) -> Option<Preset> {
        match q {
            Source::Own(i) => self.presets.get(i).cloned(),
            Source::BuiltIn(i) => self.darkroom.recipes.built_in().get(i).cloned(),
        }
    }

    /// Export a single recipe file (file dialog).
    fn darkroom_recipe_export(&mut self, ctx: &egui::Context, q: Source) {
        let Some(p) = self.darkroom_recipe_of(q) else { return };
        let file = format!("{}.graineo-recipe.json", p.name.replace(['/', ':'], "-"));
        self.darkroom.recipes.export_text = Some(presets::recipe_json(&p));
        crate::file_dialog::save(ctx, "dk-rezept-export", move |d| d.set_title(crate::tr("Filmlook exportieren")).set_file_name(file).add_filter(crate::tr("graineo-Filmlook"), &["json"]));
    }

    /// Pick up the results of the darkroom file dialogs (every frame; recipes, contact print).
    pub(crate) fn darkroom_file_dialogs(&mut self) {
        if let Some(path) = crate::file_dialog::fetch_one("dk-rezept-export") {
            if let Some(text) = self.darkroom.recipes.export_text.take() {
                self.darkroom.recipes.message = match std::fs::write(&path, text) {
                    Ok(()) => crate::tr("Filmlook exportiert.").into(),
                    Err(e) => crate::trf!("Export fehlgeschlagen: {e}", e = e),
                };
            }
        }
        if let Some(path) = crate::file_dialog::fetch_one("dk-rezept-import") {
            self.darkroom_recipe_import_from(path);
        }
        if let Some(path) = crate::file_dialog::fetch_one("dk-abzug-export") {
            self.darkroom_print_save(path);
        }
    }

    /// Import a single recipe file (file dialog). Recipes with the same name stay, the new one gets "(2)".
    fn darkroom_recipe_import(&mut self, ctx: &egui::Context) {
        crate::file_dialog::file(ctx, "dk-rezept-import", |d| d.set_title(crate::tr("Filmlook importieren")).add_filter(crate::tr("graineo-Filmlook"), &["json"]));
    }

    fn darkroom_recipe_import_from(&mut self, path: std::path::PathBuf) {
        // A recipe is a few KB; do not even read larger files
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() > presets::RECIPE_MAX_BYTES => Err(crate::tr("Die Datei ist zu groß für einen Filmlook.").to_string()),
            _ => std::fs::read_to_string(&path).map_err(|e| crate::trf!("Lesen fehlgeschlagen: {e}", e = e)),
        };
        match text.and_then(|t| presets::read_recipe(&t)) {
            Ok(mut p) => {
                let mut n = 2;
                let base = p.name.clone();
                while self.presets.iter().any(|q| q.scope == Scope::Film && q.name == p.name) {
                    p.name = format!("{base} ({n})");
                    n += 1;
                }
                if develop::film::stock(&p.settings.film.stock).is_none() {
                    self.darkroom.recipes.message = crate::tr("Der Film dieses Filmlooks ist hier nicht vorhanden.").into();
                    return;
                }
                self.darkroom.recipes.message = crate::trf!("Importiert: {a}", a = p.name);
                self.presets.push(p);
                presets::save(&self.presets);
                self.darkroom.recipes.selection = Some(Source::Own(self.presets.len() - 1));
            }
            Err(e) => self.darkroom.recipes.message = e,
        }
    }

    /// The recipe book (dialog) and the delete confirmation.
    pub(crate) fn darkroom_recipes_window(&mut self, ctx: &egui::Context) {
        if !self.darkroom.recipes.opened || !self.darkroom_active() {
            return;
        }
        self.update_recipe_previews(ctx);
        // Delete: confirmation above the recipe book
        if let Some(i) = self.darkroom.recipes.delete {
            let name = self.presets.get(i).map(|p| p.name.clone()).unwrap_or_default();
            match dialog::show(ctx, "dk-rezept-loeschen", crate::tr("Filmlook löschen?"), 420.0, |ui| dialog::text(ui, &crate::trf!("„{name}“ wird aus den Filmlooks gelöscht. Das lässt sich nicht rückgängig machen.", name = name)), dialog::Buttons::destructive(crate::tr("Löschen"), true)) {
                dialog::Answer::Primary => {
                    self.darkroom.recipes.delete = None;
                    self.darkroom_run_recipe(Action::Delete(i));
                }
                dialog::Answer::Cancel => self.darkroom.recipes.delete = None,
                dialog::Answer::Open => {}
            }
            return;
        }
        let own_presets: Vec<(usize, String, String)> = self.presets.iter().enumerate().filter(|(_, p)| p.scope == Scope::Film).map(|(i, p)| (i, p.name.clone(), film_name(p))).collect();
        let film_an = self.current.as_ref().and_then(|c| self.entries.get(c.index)).is_some_and(|e| e.settings.film.is_active());
        let mut action: Option<Action> = None;
        let has_selection = self.darkroom.recipes.selection.is_some();
        let st = &mut self.darkroom.recipes;
        let a = dialog::show(
            ctx,
            "dk-rezepte",
            crate::tr("Filmlooks"),
            (ctx.content_rect().width()-90.0).max(300.0),
            |ui| {
                // Save
                lr::subheading(ui, "Aktuelle Einstellungen speichern");
                ui.horizontal(|ui| {
                    crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut st.name).hint_text(crate::tr("Name des Filmlooks")).desired_width(300.0));
                    let ok = film_an && !st.name.trim().is_empty();
                    if ui.add_enabled(ok, egui::Button::new(crate::tr("FILMLOOK SPEICHERN"))).clicked() {
                        action = Some(Action::Save(st.name.clone()));
                    }
                });
                ui.add_space(8.0);
                let (finished, all) = st.images.progress();
                let mut card_w = st.card_w();
                ui.allocate_ui(egui::vec2(ui.available_width(), 20.0), |ui| {
                    ui.horizontal(|ui| {
                        if finished < all { ui.label(crate::trf!("VORSCHAUEN {a}/{b}", a = finished, b = all)); }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::lr::slider_add_sized(ui, [120.0, 18.0], egui::Slider::new(&mut card_w, crate::darkroom_layout::CARD_RANGE).show_value(false)).changed() {
                                st.set_card_w(card_w);
                            }
                            ui.label(egui::RichText::new(crate::tr("GRÖSSE")).color(lr::TEXT_DIM()));
                        });
                    });
                });
                let height = (ctx.content_rect().height() - 400.0).max(100.0);
                egui::ScrollArea::vertical().id_salt("recipe-grid").max_height(height).min_scrolled_height(height).auto_shrink([false,false]).show(ui, |ui| {
                    if let Some((q,duplicate)) = crate::recipe_grid::raster(ui,&st.images,&|_| true,st.selection,card_w,false,&mut None) {
                        st.selection=Some(q);st.rename=None;
                        if duplicate { action=Some(Action::Load(q)); }
                    }
                });
                ui.add_space(8.0);
                // Actions for the selected recipe
                let q = st.selection;
                let own_idx = if let Some(Source::Own(i)) = q { Some(i) } else { None };
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(own_idx.is_some(), egui::Button::new(crate::tr("UMBENENNEN"))).clicked() {
                        let old = own_idx.and_then(|i| own_presets.iter().find(|e| e.0 == i)).map(|e| e.1.clone()).unwrap_or_default();
                        st.rename = Some(old);
                    }
                    if ui.add_enabled(own_idx.is_some(), egui::Button::new(crate::tr("LÖSCHEN"))).clicked() {
                        st.delete = own_idx;
                    }
                    if ui.add_enabled(q.is_some(), egui::Button::new(crate::tr("EXPORTIEREN …"))).clicked() {
                        action = q.map(Action::Export);
                    }
                    if ui.button(crate::tr("IMPORTIEREN …")).clicked() {
                        action = Some(Action::Import);
                    }
                });
                ui.allocate_ui(egui::vec2(ui.available_width(), 28.0), |ui| {
                if let (Some(new_preset), Some(i)) = (st.rename.as_mut(), own_idx) {
                    ui.horizontal(|ui| {
                        crate::lr::edit_field(ui, egui::TextEdit::singleline(new_preset).desired_width(300.0));
                        if ui.button(crate::tr("ÜBERNEHMEN")).clicked() {
                            action = Some(Action::Rename(i, new_preset.clone()));
                        }
                        if ui.button(crate::tr("ABBRECHEN")).clicked() {
                            action = Some(Action::Rename(usize::MAX, String::new()));
                        }
                    });
                }
                }
                );
                ui.add_sized([ui.available_width(),18.0],egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(&st.message)).small().color(lr::TEXT_DIM())).truncate());
            },
            dialog::Buttons::confirm(crate::tr("Anwenden"), has_selection),
        );
        if a == dialog::Answer::Cancel {
            self.darkroom.recipes.opened = false;
        }
        // "Apply" (↩) and double click load the selected film look and close the window,
        // otherwise it covers the image and you cannot see that something happened.
        let action = match (a, action, self.darkroom.recipes.selection) {
            (dialog::Answer::Primary, None, Some(q)) => Some(Action::Load(q)),
            (_, action, _) => action,
        };
        match action {
            Some(Action::Export(q)) => self.darkroom_recipe_export(ctx, q),
            Some(Action::Import) => self.darkroom_recipe_import(ctx),
            Some(Action::Rename(usize::MAX, _)) => self.darkroom.recipes.rename = None,
            Some(Action::Rename(i, n)) => {
                self.darkroom.recipes.rename = None;
                self.darkroom_run_recipe(Action::Rename(i, n));
            }
            Some(Action::Save(n)) => {
                self.darkroom_run_recipe(Action::Save(n));
                if self.darkroom.recipes.message.starts_with("Gespeichert") {
                    self.darkroom.recipes.name.clear();
                }
            }
            Some(Action::Load(q)) => {
                self.darkroom_run_recipe(Action::Load(q));
                self.darkroom.recipes.opened = false;
            }
            Some(a) => self.darkroom_run_recipe(a),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tab;
    use eframe::App as _;
    use egui::{Pos2, Rect, Vec2};

    /// Whole window with a sample image; provides context and app.
    fn app_with_image(name: &str) -> (egui::Context, App) {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let mut folder = std::env::temp_dir().join(format!("graineo-dk-{name}-{}", std::process::id()));
        if name == "longtext" {
            // Long folder names as well (path rows, left panel)
            folder = folder.join("ein sehr langer Ordnername mit Leerzeichen ".repeat(4).trim()).join("Unterordner-".repeat(10));
        }
        std::fs::create_dir_all(&folder).unwrap();
        let filename = if name == "longtext" { format!("{}.png", "langer-dateiname-".repeat(12)) } else { "sample.png".to_string() };
        image::RgbImage::from_pixel(1800, 1200, image::Rgb([110, 80, 60])).save(folder.join(filename)).unwrap();
        if name == "strip-selection" {
            for i in 1..5 {
                image::RgbImage::from_pixel(48, 32, image::Rgb([110, 80, 60])).save(folder.join(format!("sample-{i}.png"))).unwrap();
            }
        }
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(folder);
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (ctx, a)
    }

    fn frame_step(ctx: &egui::Context, a: &mut App, clock: &mut f64) {
        *clock += 0.5;
        let mut input = egui::RawInput { time: Some(*clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(ctx, &mut input);
        let _ = crate::ui_test::run(ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
    }

    #[test]
    fn filmstrip_extension_keeps_main_photo_and_steps_from_head() {
        let (_ctx, mut app) = app_with_image("strip-selection");
        assert_eq!(app.entries.len(), 5);
        let anchor = app.current.as_ref().unwrap().index;
        let visible = app.visible();
        let pos = visible.iter().position(|&i| i == anchor).unwrap();
        assert!(pos + 3 < visible.len());
        app.extend_strip(true);
        app.extend_strip(true);
        assert_eq!(app.current.as_ref().unwrap().index, anchor);
        assert_eq!(app.strip_ext, Some((anchor, visible[pos + 2])));
        assert!(visible[pos..=pos + 2].iter().all(|&i| app.entries[i].selected));
        app.step(true, false);
        assert!(app.entries.iter().all(|e| !e.selected));
        assert_eq!(app.lib.active_image(), Some(app.entries[visible[pos + 3]].image));
        assert!(app.strip_ext.is_none());
    }

    #[test]
    fn long_filename_keeps_tool_panels_inside_the_window() {
        use crate::workspace::ToolTab;
        let (ctx, mut app) = app_with_image("longtext");
        ctx.global_style_mut(|s| s.animation_time = 0.0);
        app.show_right = true;
        let mut time = 0.0;
        for language in crate::menu::i18n::Language::ALL {
            let _guard = crate::menu::i18n::locale::TestLanguage::set(language);
            for tool in [ToolTab::Info, ToolTab::Edit, ToolTab::FilmLook, ToolTab::Crop, ToolTab::Masks, ToolTab::Retouch, ToolTab::Presets, ToolTab::Sensor, ToolTab::Versions] {
                app.tool_tab = tool;
                for _ in 0..2 {
                    time += 0.5;
                    let input = egui::RawInput { time: Some(time), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 900.0))), ..Default::default() };
                    let _ = crate::ui_test::run(&ctx, input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
                }
                let rect = app.panel_rects[3].expect("right panel visible");
                assert!(rect.width() <= 390.0, "panel grew for {tool:?} in {language:?}: {rect:?}");
                assert!(rect.left() >= 700.0, "panel extends into the photo for {tool:?} in {language:?}: {rect:?}");
            }
        }
        // The library (grid) with its info panel and the left panel, the case of the report
        app.to_grid();
        app.show_left = true;
        let mut checked = 0;
        for language in crate::menu::i18n::Language::ALL {
            let _guard = crate::menu::i18n::locale::TestLanguage::set(language);
            for width in [1600.0, 1100.0, 900.0] {
                // rects of panels hidden at a narrow width would stay from the frame before
                app.panel_rects = Default::default();
                for _ in 0..3 {
                    time += 0.5;
                    let input = egui::RawInput { time: Some(time), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 900.0))), ..Default::default() };
                    let _ = crate::ui_test::run(&ctx, input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
                }
                // a narrow window may close a panel; what is shown must fit
                let Some(right) = app.panel_rects[3] else { continue };
                checked += 1;
                assert!(right.width() <= 390.0 && right.right() <= width + 0.5, "info panel grew in the grid at {width} in {language:?}: {right:?}");
                if let Some(left) = app.panel_rects[2] {
                    assert!(left.width() <= 301.0 && left.left() >= -0.5, "left panel grew at {width} in {language:?}: {left:?}");
                    assert!(left.right() <= right.left() + 0.5, "panels overlap at {width} in {language:?}: {left:?} {right:?}");
                }
            }
        }
        assert!(checked >= 5, "info panel was shown in only {checked} cases");
    }

    #[test]
    fn toolbar_never_overlaps() {
        let (ctx, mut a) = app_with_image("toolbar");
        ctx.global_style_mut(|s| s.animation_time = 0.0);
        let mut clock = 0.0;
        for language in crate::menu::i18n::Language::ALL {
            let _guard = crate::menu::i18n::locale::TestLanguage::set(language);
            for darkroom in [false, true] {
                if darkroom { a.enter_darkroom(); } else { a.leave_darkroom(); }
                // the window is at least 900 pt wide (minimum size)
                let mut w = 900.0;
                while w <= 1600.0 {
                    for _ in 0..3 {
                        clock += 0.5;
                        let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w, 900.0))), ..Default::default() };
                        a.raw_input_hook(&ctx, &mut input);
                        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
                    }
                    assert!(a.toolbar_gap >= 0.0, "bar overlaps at {w} pt ({language:?}, darkroom {darkroom}): {} {:?} compact {} narrow {}", a.toolbar_gap, a.toolbar_widths, a.zoom_compact, a.zoom_narrow);
                    w += 20.0;
                }
            }
        }
    }

    #[test]
    fn drawer_recognizes_applies_and_closes() {
        use crate::darkroom_layout::LookState;
        let (ctx, mut a) = app_with_image("drawer");
        let mut clock = 0.0;
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame_step(&ctx, &mut a, &mut clock);
        frame_step(&ctx, &mut a, &mut clock);
        let index = a.current.as_ref().unwrap().index;
        assert!(matches!(a.darkroom_look_state(index), LookState::Combination));

        // open, apply: the drawer closes and the handle knows the look
        a.darkroom.recipes.drawer = true;
        frame_step(&ctx, &mut a, &mut clock);
        a.darkroom_run_recipe(Action::Load(Source::BuiltIn(0)));
        a.darkroom.recipes.drawer = false;
        let applied = a.entries[index].settings.film.clone();
        assert!(matches!(a.darkroom_look_state(index), LookState::Exact(_)));
        a.entries[index].settings.film.grain = applied.grain + 0.25;
        assert!(matches!(a.darkroom_look_state(index), LookState::Changed(_)));

        // Esc closes without a change
        let before = a.entries[index].settings.film.clone();
        a.darkroom.recipes.drawer = true;
        clock += 0.5;
        let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0))), ..Default::default() };
        input.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
        a.raw_input_hook(&ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        assert!(!a.darkroom.recipes.drawer && a.darkroom_active(), "Esc closes only the drawer");
        assert_eq!(a.entries[index].settings.film, before);

        // saving a changed look creates a new one and never overwrites
        let count = a.presets.len();
        a.darkroom_save_new("Mein Look");
        a.darkroom_save_new("Mein Look");
        assert_eq!(a.presets.len(), count + 2);
        assert!(a.presets.iter().any(|p| p.name == "Mein Look 2"));
        assert!(matches!(a.darkroom_look_state(index), LookState::Exact(n) if n == "Mein Look 2"));
    }

    #[test]
    fn recipe_book_save_load_rename_delete() {
        if develop::film::films().is_empty() {
            return;
        }
        let (ctx, mut a) = app_with_image("rezepte");
        let mut clock = 10.0;
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame_step(&ctx, &mut a, &mut clock);
        frame_step(&ctx, &mut a, &mut clock);
        let i = a.current.as_ref().unwrap().index;
        {
            let f = &mut a.entries[i].settings.film;
            f.film_dev = 11.0;
            f.paper_matte = 1.5;
            f.taking_filter = "orange21".into();
        }
        let name = format!("Testrezept {}", std::process::id());

        // nothing is saved without a name
        a.darkroom_run_recipe(Action::Save("  ".into()));
        assert!(a.darkroom.recipes.message.contains("Namen"));

        // save: user recipe with scope film, all film values included
        a.darkroom_run_recipe(Action::Save(name.clone()));
        let pos = a.presets.iter().position(|p| p.name == name && p.scope == Scope::Film).expect("saved");
        assert_eq!(a.darkroom.recipes.selection, Some(Source::Own(pos)));
        let f = a.presets[pos].film_recipe();
        assert!(f.stock == "kodak_trix_400" && f.film_dev == 11.0 && f.paper_matte == 1.5 && f.taking_filter == "orange21");
        // the same name overwrites instead of duplicating
        a.darkroom_run_recipe(Action::Save(name.clone()));
        assert_eq!(a.presets.iter().filter(|p| p.name == name).count(), 1);

        // change the photo, load the recipe: film values come back, other sliders stay
        a.entries[i].settings.exposure = 0.7;
        a.entries[i].settings.film = develop::film::FilmSettings { stock: "ilford_hp5_plus".into(), active: true, ..Default::default() };
        a.darkroom_run_recipe(Action::Load(Source::Own(pos)));
        let s = &a.entries[i].settings;
        assert!(s.film.stock == "kodak_trix_400" && s.film.film_dev == 11.0 && s.film.taking_filter == "orange21");
        assert_eq!(s.exposure, 0.7);

        // load built-in recipes
        a.darkroom_run_recipe(Action::Load(Source::BuiltIn(0)));
        assert!(a.entries[i].settings.film.is_active());
        assert!(a.darkroom.recipes.built_in().len() >= 6);

        // dialog draws (open, with delete confirmation) without crashing
        a.darkroom.recipes.opened = true;
        a.darkroom.recipes.selection = Some(Source::Own(pos));
        frame_step(&ctx, &mut a, &mut clock);
        a.darkroom.recipes.delete = Some(pos);
        frame_step(&ctx, &mut a, &mut clock);
        a.darkroom.recipes.delete = None;

        // rename
        let new_preset = format!("{name} neu");
        a.darkroom_run_recipe(Action::Rename(pos, new_preset.clone()));
        assert_eq!(a.presets[pos].name, new_preset);

        // Delete: only user film recipes; presets of other kinds stay untouched
        a.presets.push(Preset { name: "Look-Vorgabe".into(), settings: develop::Settings::default(), scope: Scope::Look, hint: String::new() });
        let look = a.presets.len() - 1;
        a.darkroom_run_recipe(Action::Delete(look));
        assert!(a.presets.iter().any(|p| p.name == "Look-Vorgabe"), "the recipe book does not delete look presets");
        a.presets.pop();
        a.darkroom_run_recipe(Action::Delete(pos));
        assert!(!a.presets.iter().any(|p| p.name == new_preset));
        assert_eq!(a.darkroom.recipes.selection, None);
        assert!(a.tab == Tab::Develop);
    }
}

#[cfg(test)]
mod grid_window_tests {
    use super::*;
    use eframe::App as _;
    #[test]
    fn grid_fills_click_marks_double_click_loads_and_reopen_uses_cache() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder=std::env::temp_dir().join(format!("graineo-recipe-grid-{}",std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_fn(300,200,|x,y| image::Rgb([(x/2) as u8,y as u8,90])).save(folder.join("sample.png")).unwrap();
        let _app_lock = crate::ui_test::app_lock();
        let ctx=egui::Context::default();let cc=eframe::CreationContext::_new_kittest(ctx.clone());let mut app=App::new(&cc);
        app.open_folder_inner(folder);app.to_single_image();
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(15);
        while app.current.as_ref().is_none_or(|c| c.texture.is_none()) || app.loading.is_some() {
            app.handle_results(&ctx);app.schedule(0.0);
            assert!(std::time::Instant::now()<deadline);std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.enter_darkroom();app.darkroom.test_film("kodak_trix_400");
        let own=app.presets.len();
        app.presets.push(presets::recipe("AAA Eigenes Rasterrezept",&develop::film::FilmSettings {active:true,stock:"ilford_hp5_plus".into(),..Default::default()}));
        let q=Source::Own(own);let mut time=10.0;
        let mut frame=|a:&mut App, events:Vec<egui::Event>| {
            time+=0.05;
            let mut input=egui::RawInput {time:Some(time),screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1400.0,1000.0))),events,..Default::default()};
            a.raw_input_hook(&ctx,&mut input);
            crate::ui_test::run(&ctx,input,|ui|a.ui(ui,&mut eframe::Frame::_new_kittest()))
        };
        frame(&mut app,vec![]);frame(&mut app,vec![]);app.darkroom.recipes.opened=true;
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(60);
        loop {
            app.handle_results(&ctx);frame(&mut app,vec![]);
            let (done,total)=app.darkroom.recipes.images.progress();
            if done==total && total>5 {break;}
            assert!(std::time::Instant::now()<deadline,"grid not finished: {done}/{total}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let id=egui::Id::unique(("recipe-card",q,190usize));
        let rect=ctx.read_response(id).expect("user recipe visible").rect;
        assert!(rect.left()>=0.0 && rect.right()<=1400.0 && rect.top()>=0.0 && rect.bottom()<=1000.0);
        let pos=rect.center();let click=|pressed|egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed,modifiers:egui::Modifiers::NONE};
        let i=app.current.as_ref().unwrap().index;let before=app.entries[i].settings.film.clone();
        frame(&mut app,vec![egui::Event::PointerMoved(pos),click(true)]);frame(&mut app,vec![click(false)]);
        assert_eq!(app.darkroom.recipes.selection,Some(q));assert_eq!(app.entries[i].settings.film,before,"a click only selects");
        frame(&mut app,vec![click(true)]);frame(&mut app,vec![click(false)]);
        assert_eq!(app.entries[i].settings.film.stock,"ilford_hp5_plus","double click loads");
        assert!(!app.darkroom.recipes.opened,"double click closes the window");
        app.darkroom.recipes.opened=true;frame(&mut app,vec![]);
        let finished=app.darkroom.recipes.images.progress();
        app.darkroom.recipes.opened=false;frame(&mut app,vec![]);
        app.darkroom.recipes.opened=true;frame(&mut app,vec![]);
        assert_eq!(app.darkroom.recipes.images.progress(),finished,"reopening is filled immediately");
    }
}
