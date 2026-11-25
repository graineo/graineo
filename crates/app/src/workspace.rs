//! One workspace instead of modules: on the left the library sources,
//! in the middle grid or single image, at the bottom (in single image view) the film strip, on the right a
//! panel with the tabs "Info" (I) and "Edit" (E). Export is a dialog (⌘⇧E), the
//! sensor a view of the single image. Multi-selection: slider movements apply relatively to
//! all selected images ([`crate::multi_edit`]).

use crate::menu::i18n::DeComma as _;
use eframe::egui;
use egui::Color32;

use crate::{library_ui, lr, App, Tab};

/// Tools of the right panel (icon bar at the edge, see `tool_rail.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolTab {
    /// Metadata, keywords, rating of the library
    Info,
    /// Development sliders in categories
    Edit,
    /// Film, look, paper, grain
    FilmLook,
    /// Crop, straighten, perspective (only shown while crop mode is open)
    Crop,
    /// local corrections
    Masks,
    /// retouching: heal, clone, red eyes, remove objects
    Retouch,
    /// Presets, navigator, history
    Presets,
    /// Sensor view (measurements, calibration data)
    Sensor,
    /// virtual copies of the photo
    Versions,
    /// merged (HDR) photo: ghost level, "Quelle malen"
    Merge,
}

impl ToolTab {
    /// Belongs to editing the image (loads the active image, also in the grid)?
    pub fn editing(self) -> bool {
        self != ToolTab::Info
    }
}

/// At startup with "Single photo" as the last module: open the remembered photo as soon as the list is there.
pub(crate) static RESTORE_SINGLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl App {
    /// Start in the single view where the last session ended (once; the grid stays if the photo no longer exists).
    pub(crate) fn restore_view(&mut self) {
        use std::sync::atomic::Ordering;
        if !RESTORE_SINGLE.load(Ordering::Relaxed) || self.entries.is_empty() || !self.in_grid() {
            return;
        }
        RESTORE_SINGLE.store(false, Ordering::Relaxed);
        if self.lib.active_image().is_some_and(|b| self.entries.iter().any(|e| e.image == b)) {
            self.to_single_image();
        } else {
            crate::log("start: remembered photo not in the list, staying in the grid");
        }
    }

    /// Grid view active (otherwise single image or sensor view)?
    pub(crate) fn in_grid(&self) -> bool {
        self.tab == Tab::Library
    }

    /// The people view is open in the grid: the chosen face's photo is the current photo of the app.
    pub(crate) fn in_people(&self) -> bool {
        self.in_grid() && crate::people_ui::is_open()
    }

    /// Is there a photo for Develop and the editing tools? In the people view: a chosen face.
    pub(crate) fn has_photo(&self) -> bool {
        if self.in_people() {
            return self.lib.people_focus().is_some();
        }
        self.active_entry().is_some()
    }

    /// Switch to the grid (G, Esc).
    pub(crate) fn to_grid(&mut self) {
        // back to the people view: the photo's settings are saved now (the view shows it from the library)
        if self.lib.people_pending() {
            self.save_current();
        }
        // the photo just shown becomes the active one in the grid (and is scrolled to): the one selected in the
        // filmstrip, also while it is still loading (otherwise the grid went back to the photo loaded last)
        if let Some(b) = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable).and_then(|i| self.entries.get(i)).map(|e| e.image) {
            self.lib.set_active(b);
            // comparing: the filmstrip's selection becomes the grid's, the comparison goes on in the library
            if self.lib.compare_open() {
                let mut images: Vec<i64> = self.entries.iter().filter(|e| e.selected).map(|e| e.image).collect();
                if !images.contains(&b) {
                    images.push(b);
                }
                self.lib.set_selection(b, &images);
            }
        }
        self.leave_darkroom();
        if self.crop_mode.is_some() { self.toggle_crop(true); }
        self.wb_picker = false;
        self.measure = false;
        self.entries.iter_mut().for_each(|e| e.selected = false);
        self.grid = false;
        self.lib.loupe = false;
        self.tab = Tab::Library;
        self.lib.restore_grid();
        // left from the people view with "Foto öffnen": back to it as it was
        self.lib.resume_people();
        self.tool_tab = ToolTab::Info;
        self.show_right = true;
        self.navigation.finish();
    }

    /// Switch to single image view (double click, Return).
    pub(crate) fn to_single_image(&mut self) {
        // from the people view: the chosen face's photo, and the way back to the view (`to_grid`)
        if self.in_grid() && crate::people_ui::is_open() {
            self.lib.people_to_develop();
        }
        self.grid = false;
        // from the grid with nothing selected: the photo in view, not one far away
        if self.in_grid() {
            self.lib.prepare_develop();
        }
        // single image, Develop and darkroom always have an active photo (the last active one)
        self.lib.keep_active();
        if self.in_grid() {
            self.lib.remember_grid();
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
            self.compare = false;
            self.loupe = false;
            self.show_before = false;
            self.show_camera_jpeg = false;
            self.navigation.finish();
            // Whoever is already editing (Edit, Film & Look, Presets, Masks, Retouch, Versions) stays in their tool;
            // from Info (and everything that exists only in single image view) it goes to "Edit"
            if !matches!(self.tool_tab, ToolTab::Edit | ToolTab::FilmLook | ToolTab::Masks | ToolTab::Retouch | ToolTab::Presets | ToolTab::Versions | ToolTab::Merge) {
                self.tool_tab = ToolTab::Edit;
            }
            self.show_right = true;
        }
        if self.tab == Tab::Library || self.tab == Tab::Export {
            self.tab = Tab::Develop;
        }
        self.sync_source(false);
    }

    /// Workspace keys that apply in both views (I, E, ⌘⇧E); in the grid additionally
    /// G (stays) and Esc.
    pub(crate) fn workspace_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.lib.import_open() || ctx.memory(|m| m.top_modal_layer().is_some()) || (self.in_grid() && crate::people_ui::is_open()) {
            return;
        }
        let p = |k: egui::Key| ctx.input(|i| i.key_pressed(k) && !i.modifiers.command && !i.modifiers.alt);
        // Panels as in common photo apps (in the library and single image; the darkroom has its own Tab/F6):
        // Tab = side panels, ⇧Tab = all panels, F5–F8 individually
        let darkroom = self.darkroom_active();
        if let Some(shift) = self.tab_pressed.take_if(|_| !darkroom) {
            self.toggle_panels(shift);
        }
        if !darkroom && p(egui::Key::F5) {
            self.show_top = !self.show_top;
        }
        if !darkroom && p(egui::Key::F6) {
            self.show_filmstrip = !self.show_filmstrip;
        }
        if !darkroom && p(egui::Key::F7) {
            self.show_left = !self.show_left;
        }
        if !darkroom && p(egui::Key::F8) {
            self.show_right = !self.show_right;
        }
        // F: the whole app full screen (macOS full-screen mode) on/off – in every view, without switching the view
        if crate::shortcuts::pressed(ctx, "full_screen") {
            crate::panel_buttons::toggle_full_screen(ctx);
        }
        if crate::shortcuts::pressed(ctx, "refresh_folder") {
            self.lib.refresh_viewed(true);
        }
        if crate::shortcuts::pressed(ctx, "quick_show") {
            self.lib.show_quick();
            self.to_grid();
        }
        if crate::shortcuts::pressed(ctx, "quick_save") { self.lib.quick_ask(true); }
        if crate::shortcuts::pressed(ctx, "quick_clear") { self.lib.quick_ask(false); }
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let p = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        if crate::shortcuts::pressed(ctx, "tab_info") || (self.in_grid() && crate::shortcuts::pressed(ctx, "tab_info_grid")) {
            self.navigation.finish();
            self.tool_tab = ToolTab::Info;
            self.show_right = true;
        }
        if crate::shortcuts::pressed(ctx, "tab_edit") {
            self.tool_tab = ToolTab::Edit;
            self.show_right = true;
        }
        // ⇧⌘F / ⌥⌘F: Show in Finder / Show in library folder (active image)
        if crate::shortcuts::pressed(ctx, "show_in_finder") {
            self.show_in_finder();
        }
        if crate::shortcuts::pressed(ctx, "show_in_library_folder") {
            self.show_in_folder();
        }
        // Del / ⌫: delete selected photos (confirmation), in a collection only take them out of it (no question, ⌘Z);
        // not in crop, while drawing a mask, in the darkroom
        if crate::shortcuts::which(ctx, "delete_selected").is_some() && !self.confirm_delete && self.crop_mode.is_none() && !crate::masks_ui::painting(ctx) && !crate::masks_ui::takes_delete(ctx) && !crate::retouch_ui::has_selection(ctx) && !self.darkroom_active() {
            if self.lib.removable_collection().is_some() && !(self.in_grid() && self.lib.not_in_catalog_view()) {
                let ids = self.delete_targets();
                let a = self.lib.remove_from_collection_external(ids);
                self.library_actions(a);
            } else {
                self.request_delete();
            }
        }
        if crate::shortcuts::pressed(ctx, "spinner_info") {
            crate::work_step::toggle_always();
        }
        if !self.in_grid() && crate::shortcuts::pressed(ctx, "info_overlay") {
            self.info_next();
        }
        // ⌘Q / Ctrl+Q: a real menu shortcut (`menu.rs`, Windows through `shortcut_without_menu`), not handled here
        if self.in_grid() {
            if cmd && shift && p(egui::Key::E) {
                self.open_export();
            }
            if cmd && p(egui::Key::Slash) {
                self.show_shortcuts = !self.show_shortcuts;
            }
        }
    }

    /// Right panel: header (tool name, quick actions) and content of the selected tool; the
    /// tool is selected by the icon bar at the right edge ([`App::tool_rail`]).
    pub(crate) fn right_panel(&mut self, ui: &mut egui::Ui) -> Vec<library_ui::Action> {
        ui.add_space(4.0);
        let active = self.tool_rail_active();
        self.tool_header(ui, active);
        // Multi-selection: hint only when several images are selected
        // in single image view the sliders only affect the shown photo: no hint there
        // (editing tools: the band "Selection: n photos" below the histogram, see multi_edit.rs)
        let n = if self.in_grid() && !self.in_people() { self.selection_count() } else { 0 };
        let text = if n > 1 && !active.editing() { crate::trf!("{n} Bilder ausgewählt", n = n) } else { String::new() };
        if !text.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(&text)).small().color(crate::lr::ui_accent(Color32::from_rgb(0xe0, 0xc0, 0x70)))).truncate());
        }
        ui.add_space(2.0);
        ui.separator();
        match active {
            ToolTab::Info => {
                // details from the opened file go into the Info panel (formerly a second "Metadata" in Edit)
                self.lib.file_details = self.current.as_ref().and_then(|c| self.entries.get(c.index).map(|e| (e.image, c.loaded.info.clone())));
                // the library's own scroll bar
                let grid = self.in_grid();
                // viewing a backup: take over the edit of the shown photo
                if let Some(e) = self.active_entry().and_then(|i| self.entries.get(i)) {
                    crate::backup_view_ui::info_section(ui, &e.path, e.copy.as_deref());
                }
                self.lib.info(ui, grid)
            }
            _ => {
                self.edit_panel(ui);
                Vec::new()
            }
        }
    }

    /// Content of the editing tool panels (also in the grid): Edit, Film & Look, Masks, Crop,
    /// Presets (with navigator and history) – the tool rail selects the panel.
    fn edit_panel(&mut self, ui: &mut egui::Ui) {
        if self.tab == Tab::Sensor {
            self.controls(ui);
            return;
        }
        let w = ui.available_width();
        // the display at the top of Edit and Masks, pinned above the scroll area (histogram_pin.rs); while another
        // photo of the same kind loads it stays in place, locked like the sliders
        {
            let loaded = self.current.as_ref().map(|c| c.index);
            let active = self.active_entry();
            let kind = |i: Option<usize>| i.and_then(|i| self.entries.get(i)).map(|e| crate::image_kind::of(&e.path));
            let shown = matches!(self.tool_rail_active(), ToolTab::Edit | ToolTab::Masks | ToolTab::Retouch) && (active == loaded || kind(active) == kind(loaded));
            self.pinned_histogram(ui, shown, active == loaded);
        }
        let mut surface = egui::ScrollArea::vertical().id_salt("bearbeiten").auto_shrink([false; 2]).max_width(w);
        // Test hook: GRAINEO_SIDEBAR_SCROLL=<points> scrolls the Edit panel on the first draw (for screenshots)
        if let Some(px) = crate::scroll_hold::first_offset(ui, "edit", self.current.is_some() && self.loading.is_none()) {
            surface = surface.vertical_scroll_offset(px);
        }
        // placeholder frames (photo opened again, another photo loading) keep the scroll position (scroll_hold.rs)
        surface.show(ui, |ui| crate::scroll_hold::scope(ui, "edit", |ui| ((), self.edit_panel_content(ui))));
    }

    /// Scrolled content of the Edit panel; returns true while only a placeholder stands in for the controls.
    fn edit_panel_content(&mut self, ui: &mut egui::Ui) -> bool {
        let mut transient = false;
        crate::sticky::scope(ui, |ui| {
            if self.current.is_none() {
                transient = self.loading.is_some();
                let text = match self.not_developable.and_then(|i| self.entries.get(i)) {
                    Some(e) if e.state != 0 && self.lib.work_running() => crate::tr("Bibliothek wird gerade abgeglichen …").to_string(),
                    Some(e) if e.state == 2 => crate::tr("Original nicht erreichbar\nEntwickeln geht wieder, sobald der Speicherort verbunden ist.").to_string(),
                    Some(e) if e.state as i64 == library::merge::STATUS_VIRTUAL => crate::photo_state::merged_open_error(&e.path).unwrap_or_else(|| crate::state_message(e.state)),
                    Some(e) if e.state != 0 => crate::state_message(e.state),
                    Some(e) => crate::photo_state::unsupported_text(&e.path).unwrap_or_else(|| crate::trf!("{a} lässt sich nicht bearbeiten", a = e.format)),
                    // a heavily clipped Quattro spends seconds in the highlight rebuild: say so
                    None if self.loading.is_some() => match develop::highlight_rebuild::running_for() {
                        Some(t) => {
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                            crate::trf!("Lichter werden rekonstruiert … (seit {n} s)", n = t.as_secs())
                        }
                        None => crate::tr("Wird geladen …").to_string(),
                    },
                    None => crate::tr("Kein Bild gewählt").to_string(),
                };
                ui.label(egui::RichText::new(text).color(lr::TEXT_DIM()));
                return;
            }
            // Sliders always belong to the active image: while another one is loading, the old ones are only
            // visible disabled (same image kind) or not at all (other image kind – never Sigma sliders
            // for a RAF or vice versa)
            let loaded = self.current.as_ref().map(|c| c.index);
            let active = self.active_entry();
            let kind = |i: Option<usize>| i.and_then(|i| self.entries.get(i)).map(|e| crate::image_kind::of(&e.path));
            let pipeline = self.current.as_ref().map(|c| if c.loaded.previews.foreign.is_some() { "allgemein" } else { "Sigma" });
            log_path(active.and_then(|i| self.entries.get(i)).map(|e| e.name.as_str()).unwrap_or(""), kind(active), pipeline.unwrap_or("–"), active == loaded);
            if active != loaded {
                let name = active.and_then(|i| self.entries.get(i)).map(|e| e.name.clone()).unwrap_or_default();
                ui.label(egui::RichText::new(crate::trf!("{name} wird geladen …", name = name)).color(lr::TEXT_DIM()));
                if kind(active) != kind(loaded) {
                    transient = true;
                    return;
                }
                let part = self.tool_rail_active();
                ui.add_enabled_ui(false, |ui| self.develop_controls(ui, part));
                return;
            }
            // tools without the histogram: the band "Selection: n photos" at the top
            if matches!(self.tool_rail_active(), ToolTab::Crop | ToolTab::Versions | ToolTab::Presets | ToolTab::FilmLook) {
                self.multi_band_slot(ui);
            }
            match self.tool_rail_active() {
                ToolTab::Crop => self.crop_panel(ui),
                ToolTab::Versions => self.versions_panel(ui),
                ToolTab::Merge => self.merge_panel(ui),
                ToolTab::Presets => {
                    if !self.in_grid() {
                        lr::section(ui, "Navigator", true, |ui| self.navigator(ui));
                    }
                    // itself contains the "Presets" and "History" sections (no second frame around it)
                    self.left_panel_lists(ui);
                }
                part => self.develop_controls(ui, part),
            }
        });
        transient
    }

    /// Test hook: `GRAINEO_SET_EV=<EV>` changes the exposure of the active image by this value
    /// 3 s after opening (like a slider; checks saving and multi-selection).
    pub(crate) fn test_helpers(&mut self, ctx: &egui::Context) {
        // Test hook: GRAINEO_EXPORT_TEST=<folder>[@jpg|tif|png|jxl|jxl-lossless] exports all images there 3 s after start (default JPEG)
        static EXPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Ok(dir) = crate::env::var("GRAINEO_EXPORT_TEST") {
            if !EXPORT.load(std::sync::atomic::Ordering::Relaxed) && ctx.input(|i| i.time) > 3.0 && !self.entries.is_empty() {
                EXPORT.store(true, std::sync::atomic::Ordering::Relaxed);
                let (dir, kind) = dir.rsplit_once('@').unwrap_or((dir.as_str(), "jpg"));
                self.export_format = match kind {
                    "tif" => develop::export::Format::Tiff16,
                    "png" => develop::export::Format::Png16,
                    "jxl" => develop::export::Format::JXL_DEFAULT,
                    "jxl-lossless" => develop::export::Format::Jxl { quality: 92, lossless: true, bits16: true },
                    _ => develop::export::Format::Jpeg { quality: 92 },
                };
                self.export_dir = Some(std::path::PathBuf::from(dir));
                self.export_engine = develop::export::Engine::Own;
                let all: Vec<usize> = (0..self.entries.len()).collect();
                self.export(all);
            }
        }
        // Test hook: GRAINEO_DIALOG=<name> opens a dialog 4 s after start (screenshots)
        static DIALOG_SHOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Ok(name) = crate::env::var("GRAINEO_DIALOG") {
            if !DIALOG_SHOT.load(std::sync::atomic::Ordering::Relaxed) && ctx.input(|i| i.time) > 4.0 {
                DIALOG_SHOT.store(true, std::sync::atomic::Ordering::Relaxed);
                match name.as_str() {
                    "delete" => self.confirm_delete = true,
                    "delete-selection" => self.request_delete(),
                    "export" => self.show_export = true,
                    // all photos of the shown grid (at most nine) in the HDR dialog
                    "hdr" => crate::hdr_ui::ask(self.lib.hdr_test_candidates()),
                    // every bracket of the library in the dialog "Alle Reihen zusammenfügen"
                    "hdr-all" => crate::hdr_batch::open(self.lib.hdr_test_groups()),
                    "copy" => self.copy_dialog = true,
                    "transfer" => self.open_transfer_dialog(),
                    "shortcuts" => self.show_shortcuts = true,
                    "report-bug" => crate::bug_report::open(false),
                    "import" => self.lib.open_import(ctx),
                    "lens-wizard" => crate::lens_wizard::start_test(ctx),
                    "lens-assign" => crate::lens_assign::request(crate::lens_assign::Request::Assign),
                    "lenses" => crate::lens_assign::request(crate::lens_assign::Request::Room),
                    "names" => crate::people_ui::open(),
                    "suggestions" => crate::people_ui::open_at(crate::people_ui::ToolTab::Suggestions),
                    "untitled" => crate::people_ui::open_at(crate::people_ui::ToolTab::Untitled),
                    "people" => crate::people_ui::open_at(crate::people_ui::ToolTab::People),
                    others => self.lib.test_dialog(ctx, others),
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        // Test hook: GRAINEO_DIR_LATER=<folder> opens it 4 s after start like ⌘O
        static FOLDER_SHOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Ok(dir) = crate::env::var("GRAINEO_DIR_LATER") {
            if !FOLDER_SHOT.load(std::sync::atomic::Ordering::Relaxed) && ctx.input(|i| i.time) > 4.0 {
                FOLDER_SHOT.store(true, std::sync::atomic::Ordering::Relaxed);
                self.open_folder_inner(std::path::PathBuf::from(dir));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        // Test hook: GRAINEO_PEOPLE_TEST="…;back=<s>": opened from People, back to it after <s> seconds (screenshots)
        if let Some(t) = crate::env::var("GRAINEO_PEOPLE_TEST").ok().and_then(|v| v.split(';').find_map(|p| p.strip_prefix("back=")?.parse::<f64>().ok())) {
            if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && !self.in_grid() && self.lib.people_pending() && ctx.input(|i| i.time) > t {
                self.to_grid();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        // Test hook: GRAINEO_SET_MONO="0@1.5,1@6,mode=Vivid@7" sets "Treatment" (colour 0 / black & white 1) or the
        // Sigma colour mode of the open photo the given seconds after it is shown (outdated intermediate images after a
        // change, see `spp_accept.rs`); `nr=0.75@8` sets both noise sliders, `ev=0.5@9` adds exposure (`preview_hold.rs`)
        if let (Ok(spec), Some(i)) = (crate::env::var("GRAINEO_SET_MONO"), self.current.as_ref().map(|c| c.index).filter(|_| self.loading.is_none())) {
            static STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let steps: Vec<(String, f64)> = spec.split(',').filter_map(|p| p.rsplit_once('@').and_then(|(m, t)| Some((m.trim().to_string(), t.trim().parse().ok()?)))).collect();
            let n = STEP.load(std::sync::atomic::Ordering::Relaxed);
            if let Some((what, at)) = steps.get(n) {
                let since = ctx.input(|i| i.time) - self.shown_at;
                if since >= *at {
                    let s = &mut self.entries[i].settings;
                    if let Some(v) = what.strip_prefix("nr=").and_then(|v| v.parse::<f64>().ok()) {
                        // both noise sliders (0 … 1), as dragging them does
                        (s.nr_luma, s.nr_chroma) = (v, v);
                    } else if let Some(v) = what.strip_prefix("ev=").and_then(|v| v.parse::<f64>().ok()) {
                        s.exposure += v;
                    } else {
                        match what.strip_prefix("mode=") {
                            Some(m) => s.color_mode = Some(m.to_string()),
                            None => s.monochrome = what == "1",
                        }
                    }
                    crate::log(&format!("test: treatment {} colour mode {:?} after {since:.2} s", if s.monochrome { "black & white" } else { "colour" }, s.color_mode));
                    STEP.store(n + 1, std::sync::atomic::Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(20));
            }
        }
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let Some(ev) = crate::env::var("GRAINEO_SET_EV").ok().and_then(|v| v.parse::<f64>().ok()) else { return };
        if DONE.load(std::sync::atomic::Ordering::Relaxed) || self.loading.is_some() {
            return;
        }
        let Some(i) = self.current.as_ref().map(|c| c.index) else { return };
        if ctx.input(|i| i.time) - self.shown_at > 3.0 {
            self.entries[i].settings.exposure += ev;
            DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// Active image (index in the film strip), even if it cannot be developed.
    pub(crate) fn active_entry(&self) -> Option<usize> {
        // people view: the chosen face's photo is the current one (none without a chosen face)
        if self.in_people() {
            let b = self.lib.people_focus()?;
            return self.entries.iter().position(|e| e.image == b);
        }
        if self.in_grid() {
            let b = self.lib.active_image()?;
            return self.entries.iter().position(|e| e.image == b);
        }
        self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable).or_else(|| {
            let b = self.lib.active_image()?;
            self.entries.iter().position(|e| e.image == b)
        })
    }

    pub(crate) fn show_in_finder(&mut self) {
        if let Some(e) = self.active_entry().and_then(|i| self.entries.get(i)) {
            // a merged photo is no file: nothing to show (the grid menu greys the entry with the reason)
            if e.state as i64 == library::merge::STATUS_VIRTUAL {
                self.status = crate::tr(crate::library_ui::NOT_A_FILE).to_string();
                return;
            }
            crate::system::show(&e.path);
        }
    }

    pub(crate) fn show_in_folder(&mut self) {
        if let Some(b) = self.active_entry().and_then(|i| self.entries.get(i)).map(|e| e.image) {
            self.show_left = true;
            self.lib.show_folder(b);
        }
    }

    /// Create a virtual copy (grid, single image, film strip, menu, darkroom): `image` or the active
    /// photo. The copy takes over the current settings (for example to try another film
    /// without losing the first version) and is opened in single image view right away.
    pub(crate) fn create_virtual_copy(&mut self, image: Option<i64>) {
        let Some(source) = image.or_else(|| self.active_entry().and_then(|i| self.entries.get(i)).map(|e| e.image)) else { return };
        // a virtual copy lives in the library: refused for a photo of a viewed folder (before: "keine Bibliothek" error)
        if crate::library_ui::needs_library(&[source]) {
            self.status = crate::tr(crate::library_ui::view_folder_guard::NEEDS_LIBRARY).to_string();
            return;
        }
        let single = !self.in_grid();
        self.save_current();
        let of = self.entries.iter().position(|e| e.image == source);
        if let Some(v) = of {
            self.ensure_entry(v);
        }
        let settings = of.map(|v| (self.entries[v].settings.clone(), self.entries[v].meta.clone()));
        // the background writes to the library; it continues in `copy_done`
        self.copy_pending = Some((source, single, settings));
        if !self.lib.create_copy_for(source) {
            self.copy_done(source, Err(crate::tr("keine Bibliothek").into()));
        }
    }

    /// The virtual copy is in the library: take over the source's settings and show the copy.
    pub(crate) fn copy_done(&mut self, source: i64, r: Result<i64, String>) {
        let (single, settings) = match self.copy_pending.take() {
            Some((q, e, s)) if q == source => (e, s),
            others => {
                self.copy_pending = others;
                (false, None)
            }
        };
        match r {
            Ok(id) => {
                self.sync_source(true);
                if let Some(pos) = self.entries.iter().position(|e| e.image == id) {
                    self.ensure_entry(pos);
                    if let Some((s, m)) = settings {
                        self.entries[pos].settings = s;
                        self.entries[pos].meta = m;
                        if let Err(e) = crate::save_entry(&self.entries[pos]) {
                            self.status = crate::trf!("Kopie: Einstellungen nicht gespeichert ({e})", e = e);
                        }
                    }
                    // made from a step of the develop history: the copy's history starts there
                    self.history_version_done(source, pos);
                    self.lib.set_active(id);
                    if single {
                        self.select(pos, false);
                    }
                    self.status = crate::trf!("Virtuelle Kopie angelegt: {a}", a = self.entries[pos].name);
                }
            }
            Err(e) => self.status = crate::trf!("Kopie: {e}", e = e),
        }
    }

    /// Context menu on an image or thumbnail (single image, film strip).
    pub(crate) fn image_context_menu(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.current.as_ref().map(|c| c.index).or(self.loading) else { return };
        if let Some(m) = self.lib.photo_menu_for(ui, self.entries[i].image, self.entries[i].state == 0) {
            self.photo_command(ui.ctx(), i, m);
        }
    }

    /// Command from the context menu of a photo in the filmstrip or the single image (same menu as the grid): it applies
    /// to the selection if the photo belongs to it, otherwise to this photo alone.
    pub(crate) fn photo_command(&mut self, ctx: &egui::Context, i: usize, m: crate::library_ui::Menu) {
        let image = self.entries[i].image;
        let sel = self.selection();
        let images: Vec<i64> = if sel.contains(&i) { sel.iter().map(|&k| self.entries[k].image).collect() } else { vec![image] };
        self.lib.set_selection(image, &images);
        let a = self.lib.menu_external(m, ctx);
        self.library_actions(a);
    }

    /// Distance of the messages (and the update notice) from the bottom edge: above the filmstrip.
    pub(crate) fn message_bottom(&self) -> f32 {
        if self.in_grid() || !self.show_filmstrip { 16.0 } else { self.strip_height + 8.0 }
    }

    /// Messages (`status`) go to the one status in the title bar (activity.rs): errors stay there until closed,
    /// everything else is a short success. Taken once, so the same error a second time is reported again.
    pub(crate) fn take_status(&mut self, ctx: &egui::Context) {
        if self.status.is_empty() {
            return;
        }
        let s = std::mem::take(&mut self.status);
        let now = ctx.input(|i| i.time);
        let name = self.active_entry().and_then(|i| self.entries.get(i)).map(|e| e.name.clone()).unwrap_or_default();
        self.notes.message(&s, &name, now);
    }

    /// Key I: image info short → detailed → off → short.
    pub(crate) fn info_next(&mut self) {
        self.info_level = (self.info_level + 1) % 3;
    }

    /// Image info in single image view at the top right, semi-transparent (replaces the former status bar).
    /// Short: file and exposure. Detailed: plus camera, lens, capture time, dimensions, HDR.
    /// If a look is missing, that is shown in both levels.
    pub(crate) fn info_overlay(&self, painter: &egui::Painter, area: egui::Rect) {
        self.info_overlay_inner(painter, area)
    }

    fn info_overlay_inner(&self, painter: &egui::Painter, area: egui::Rect) {
        // own layer above the image (drawn last, the box stays over the photo)
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("info-overlay"));
        let painter = &painter.ctx().layer_painter(layer).with_clip_rect(painter.clip_rect());
        let Some((mut galleys, mut rect)) = self.info_layout(painter, area) else { return };
        // a single exposure of a merged photo is shown (held down in the tool): said in the box, thin gold frame
        let peek = self.merge_info().and_then(|mi| crate::hdr_paint::peek_label(painter.ctx(), &mi));
        if let Some(label) = &peek {
            let g = painter.layout_no_wrap(label.clone(), lr::semibold(12.0), crate::lr::TEXT_STRONG());
            let extra = g.size().y + 2.0;
            rect = egui::Rect::from_min_max(egui::pos2(rect.left().min(rect.right() - g.size().x - 16.0), rect.top()), egui::pos2(rect.right(), rect.bottom() + extra));
            galleys.insert(0, g);
        }
        let radius = if lr::theme() == lr::Theme::Dark { 0.0 } else { 3.0 };
        painter.rect_filled(rect, radius, lr::info_box_fill());
        if peek.is_some() {
            painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, lr::GOLD()), egui::StrokeKind::Inside);
        }
        let right = rect.right() - 8.0;
        let mut y = rect.top() + 8.0;
        for g in galleys {
            let hh = g.size().y;
            painter.galley(egui::pos2(right - g.size().x, y), g, crate::lr::TEXT_STRONG());
            y += hh + 2.0;
        }
    }

    /// Where the image info box lies (with its margin), `None` when it is not shown: the film strip of the darkroom
    /// leaves this place free (no perforation or edge print under the text).
    pub(crate) fn info_zone(&self, painter: &egui::Painter, area: egui::Rect) -> Option<egui::Rect> {
        if self.info_level == 0 || self.darkroom_covers_image() {
            return None;
        }
        self.info_layout(painter, area).map(|(_, r)| r)
    }

    /// Lines of the image info box and its place (top right of `area`, background included).
    fn info_layout(&self, painter: &egui::Painter, area: egui::Rect) -> Option<(Vec<std::sync::Arc<egui::Galley>>, egui::Rect)> {
        // a dialog above everything: the photo info must not shine through it
        if self.darkroom.card_open().is_some() || painter.ctx().memory(|m| m.top_modal_layer().is_some()) {
            return None;
        }
        let i = self.active_entry()?;
        let e = self.entries.get(i)?;
        let c = self.current.as_ref().filter(|c| c.index == i);
        let field = |k: &str| c.and_then(|c| c.loaded.info.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string())).filter(|v| !v.is_empty() && v != "–");
        // Order: camera, exposure, date, dimensions, file name last, unobtrusive
        let mut rows: Vec<String> = Vec::new();
        if self.info_level >= 2 {
            let camera_lens: Vec<String> = [field("Kamera"), field("Objektiv")].into_iter().flatten().collect();
            if !camera_lens.is_empty() {
                rows.push(camera_lens.join(" · "));
            }
        }
        if c.is_none() {
            // photo still loading: the exposure from the library, so the box does not appear only later
            if let Some(l) = self.lib.exposure_line(e.image) {
                rows.push(l);
            }
        }
        if let Some(c) = c {
            let exp_ev = c.loaded.summary.clone();
            if !exp_ev.is_empty() {
                rows.push(exp_ev);
            }
        }
        if self.info_level >= 2 {
            if let Some(d) = field("Aufnahme") {
                rows.push(d);
            }
            if let Some(c) = c {
                let [w, h] = c.loaded.full_size;
                if w > 0 && h > 0 {
                    rows.push(crate::binning_ui::info_size(&c.loaded));
                }
                if let Some(k) = c.loaded.previews.foreign.as_ref().and_then(|d| d.hdr.as_ref()) {
                    let an = self.display.hdr_on && self.display.hdr_possible != Some(false);
                    rows.push(format!("HDR {:.1}×{}", k.headroom, if an { String::new() } else { format!(" ({})", crate::tr("aus")) }).de_comma());
                }
            }
        }
        if let Some(note) = crate::library_ui::virtual_note(e.file, e.state as i64) {
            rows.push(note);
        }
        let name_row = rows.len();
        rows.push(e.name.clone());
        // conditions of this photo (damaged, Sigma-exact unavailable, look missing …): muted yellow below the name
        let notices = self.photo_notices(i);
        for n in &notices {
            rows.push(n.short.clone());
        }
        let galleys: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, z)| {
                let (main, name, notice) = crate::lr::info_box_text();
                let color = if i > name_row { notice } else if i == name_row { name } else { main };
                painter.layout_no_wrap(z.clone(), egui::FontId::proportional(11.5), color)
            })
            .collect();
        let w = galleys.iter().map(|g| g.size().x).fold(0.0f32, f32::max);
        let h: f32 = galleys.iter().map(|g| g.size().y + 2.0).sum();
        // top right, right-aligned
        let right = area.right() - 14.0;
        let at = egui::pos2(right - w, area.top() + 12.0);
        Some((galleys, egui::Rect::from_min_size(at, egui::vec2(w, h)).expand(8.0)))
    }

    /// Conditions of the photo at `i` (see `photo_state`): the remembered ones plus those that follow from the
    /// library state and the settings (original not reachable, look missing).
    pub(crate) fn photo_notices(&self, i: usize) -> Vec<crate::photo_state::Notice> {
        let Some(e) = self.entries.get(i) else { return Vec::new() };
        let mut v = crate::photo_state::get(&e.path);
        if matches!(e.state, 1 | 2) && !(self.lib.work_running()) {
            v.push(crate::photo_state::original(e.state));
        }
        if let Some(w) = rawdev::film_profiles::missing(&e.settings) {
            v.push(crate::photo_state::look(&w));
        }
        v.extend(self.engine_notices(i));
        v
    }

    /// Number of selected images (grid or film strip).
    pub(crate) fn selection_count(&self) -> usize {
        if self.in_grid() { return self.lib.target_images().len(); }
        let raster = self.lib.selection().len();
        let strip = self.entries.iter().filter(|e| e.selected).count();
        raster.max(strip)
    }

    /// Run library actions.
    pub(crate) fn library_actions(&mut self, actions: Vec<library_ui::Action>) {
        // In the grid the photo list follows only on demand – an action may act on it
        if self.in_grid() && !actions.is_empty() {
            self.sync_source(false);
        }
        for a in actions {
            match a {
                library_ui::Action::Develop => self.to_single_image(),
                library_ui::Action::Selection => { self.navigation.finish(); self.tool_tab = ToolTab::Info; self.show_right = true; }
                library_ui::Action::Export => self.open_export(),
                library_ui::Action::Print => self.open_print_room(None, None),
                library_ui::Action::Rated(w) => self.adopt_values(&w),
                library_ui::Action::Status(t) => self.status = t,
                library_ui::Action::VirtualCopy(b) => self.create_virtual_copy(Some(b)),
                library_ui::Action::Delete => self.request_delete(),
                library_ui::Action::CopyCreated(source, r) => self.copy_done(source, r),
                library_ui::Action::FacesChanged => self.faces_cache = None,
                library_ui::Action::CopySettings(image) => {
                    // the dialog takes the settings of this photo (it need not be open)
                    if let Some(i) = self.entries.iter().position(|e| e.image == image) {
                        self.ensure_entry(i);
                        self.copy_from = Some(i);
                        self.copy_dialog = true;
                    }
                }
                library_ui::Action::PasteSettings => self.paste_settings(),
                library_ui::Action::Turn(clockwise) => self.rotate_selection(clockwise),
                library_ui::Action::ShowFolder(image) => {
                    if !self.in_grid() {
                        self.to_grid();
                    }
                    self.show_left = true;
                    self.lib.show_folder(image);
                }
                // import with a preset: previews in the background, with their own key (a multi-edit does not cancel them)
                library_ui::Action::Redevelop(images) => self.worker.send(crate::jobs::Job::Thumbnails { images, key: 1, valid: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)), large: true }),
            }
        }
    }
}

/// Log (GRAINEO_LOG): which path is shown for the active image – only on change.
fn log_path(name: &str, kind: Option<crate::image_kind::ImageKind>, pipeline: &str, loaded: bool) {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let t = crate::trf!("{name}: {a} → Regler {route}{b}", name = name, a = kind.map_or("–", |a| a.name()), route = pipeline, b = if loaded { "" } else { crate::tr(" (lädt, gesperrt)") });
    let mut z = LAST.lock().unwrap();
    if *z != t {
        crate::log(&format!("edit: {t}"));
        *z = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;

    #[test]
    fn double_click_in_grid_keeps_selected_tool() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut app = App::new(&cc);
        for (before, after) in [
            (ToolTab::Presets, ToolTab::Presets),
            (ToolTab::FilmLook, ToolTab::FilmLook),
            (ToolTab::Masks, ToolTab::Masks),
            (ToolTab::Retouch, ToolTab::Retouch),
            (ToolTab::Versions, ToolTab::Versions),
            (ToolTab::Edit, ToolTab::Edit),
            (ToolTab::Info, ToolTab::Edit),
        ] {
            app.tab = Tab::Library;
            app.tool_tab = before;
            app.to_single_image();
            assert_eq!(app.tool_tab, after, "from {before:?}");
        }
    }
}
