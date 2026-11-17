//! Workspace navigation. Single clicks wait for the end of the
//! egui double-click window so that a double click does not trigger a zoom request.
use crate::{lr, masks_ui, App};
use eframe::egui::{self, Context, Pos2, Response};

pub struct Navigation {
    ctx: Context,
    pub click: ImageClick,
    pub zoom: crate::zoom::Transition,
}
impl Navigation {
    pub fn new(ctx: Context) -> Self {
        Self {
            ctx,
            click: ImageClick::default(),
            zoom: crate::zoom::Transition::default(),
        }
    }
    pub fn finish(&mut self) {
        self.click.cancel();
        self.zoom.cancel();
        masks_ui::finish(&self.ctx);
    }
}
#[derive(Default)]
pub struct ImageClick {
    pending: Option<(f64, Pos2, usize)>,
}
#[derive(Debug, PartialEq)]
pub enum Geste {
    Zoom(Pos2),
    Raster,
}
impl ImageClick {
    pub fn cancel(&mut self) {
        self.pending = None;
    }
    fn step(
        &mut self,
        now: f64,
        delay: f64,
        generation: usize,
        enabled: bool,
        down: bool,
        single: Option<Pos2>,
        double: bool,
    ) -> Option<Geste> {
        if !enabled {
            self.cancel();
            return None;
        }
        if self.pending.is_some_and(|(_, _, g)| g != generation) {
            self.cancel();
        }
        if double {
            self.cancel();
            return Some(Geste::Raster);
        }
        if let Some(pos) = single {
            self.pending = Some((now + delay, pos, generation));
        }
        if !down && self.pending.is_some_and(|(until, _, _)| now >= until) {
            return self.pending.take().map(|(_, p, _)| Geste::Zoom(p));
        }
        None
    }
    pub fn answer(
        &mut self,
        resp: &Response,
        generation: usize,
        enabled: bool,
        image: egui::Rect,
    ) -> Option<Geste> {
        let ctx = &resp.ctx;
        let (now, down, outside, moving) = ctx.input(|i| {
            (
                i.time,
                i.pointer.primary_down(),
                i.pointer.any_pressed()
                    && (!i.pointer.primary_pressed()
                        || !resp.contains_pointer()
                        || i.pointer.interact_pos().is_none_or(|p| !image.contains(p))),
                i.zoom_delta() != 1.0
                    || i.smooth_scroll_delta != egui::Vec2::ZERO
                    || i.events
                        .iter()
                        .any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
            )
        });
        let pos = resp.interact_pointer_pos().filter(|p| image.contains(*p));
        let delay = ctx.options(|o| o.input_options.max_double_click_delay);
        let result = self.step(
            now,
            delay,
            generation,
            enabled && !resp.dragged() && !outside && !moving,
            down,
            resp.clicked_by(egui::PointerButton::Primary)
                .then_some(pos)
                .flatten(),
            resp.double_clicked_by(egui::PointerButton::Primary) && pos.is_some(),
        );
        if let Some((until, _, _)) = self.pending {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((until - now).max(0.01)));
        }
        result
    }
}
impl App {
    pub(crate) fn image_click_free(&self, ctx: &Context) -> bool {
        !masks_ui::painting(ctx)
            && !crate::retouch_ui::grabs_pointer(ctx)
            && !crate::face_frames::above(ctx)
            && self.crop_mode.is_none()
            && !self.wb_picker
            && !self.measure
            && !self.loupe
            && !self.show_export
            && !ctx.memory(|m| m.top_modal_layer().is_some())
            && !ctx.any_popup_open()
            && !egui::Popup::is_any_open(ctx)
            && !crate::help::card_open(ctx)
    }
    /// Module bar top right, in the same place in every state: Library · Develop · Darkroom
    /// (in workflow order). The active area is highlighted.
    pub(crate) fn module_bar(&mut self, ui: &mut egui::Ui) {
        let has_image = self.has_photo();
        // original on an unreachable location: the darkroom needs it (Develop shows the preview with a notice)
        let unreachable = self.active_unreachable();
        let darkroom = self.darkroom_active();
        let media_library = self.in_grid();
        let (t_lib, t_dev, t_dark) = (lr::with_shortcut("Bibliothek: Fotos finden und auswählen", "grid"), lr::with_shortcut("Entwickeln: das Foto digital entwickeln", "single_photo_d"), lr::with_shortcut("Dunkelkammer: Film entwickeln und auf Papier abziehen", "darkroom"));
        let bar_entries = [
            lr::BarEntry { text: crate::tr("BIBLIOTHEK"), active: media_library, enabled: true, hover_tip: t_lib.as_str() },
            lr::BarEntry { text: crate::tr("ENTWICKELN"), active: !media_library && !darkroom, enabled: has_image, hover_tip: t_dev.as_str() },
            lr::BarEntry {
                text: crate::tr("DUNKELKAMMER"),
                active: darkroom,
                enabled: has_image && !unreachable,
                hover_tip: if unreachable { crate::tr("Dunkelkammer: Original nicht erreichbar") } else { t_dark.as_str() },
            },
        ];
        let accent = lr::GOLD;
        match lr::bar_with(ui, &bar_entries, accent) {
            Some(0) if !media_library => self.to_grid(),
            Some(1) if media_library => self.to_single_image(),
            Some(1) if darkroom => self.leave_darkroom(),
            Some(2) if !darkroom => self.enter_darkroom(),
            _ => {}
        }
    }
    /// Is the original of the active photo on a location that is not reachable right now?
    pub(crate) fn active_unreachable(&self) -> bool {
        self.active_entry().and_then(|i| self.entries.get(i)).is_some_and(|e| e.state == 2)
    }

    pub(crate) fn open_export(&mut self) {
        // a collapsed series among the photos: ask first (only the top photo or all), the answer comes back here
        if self.in_grid() && self.lib.series_ask_scope(crate::library_ui::ScopeKind::Export) {
            return;
        }
        if self.in_grid() {
            self.sync_source(true);
        }
        self.export_scope = 0;
        self.show_export = !self.entries.is_empty();
        self.navigation.click.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_click_waits_double_click_never_zooms() {
        let mut k = ImageClick::default();
        let p = Pos2::new(70.0, 20.0);
        assert_eq!(k.step(1.0, 0.3, 1, true, false, Some(p), false), None);
        assert_eq!(k.step(1.29, 0.3, 1, true, false, None, false), None);
        assert_eq!(
            k.step(1.3, 0.3, 1, true, false, None, false),
            Some(Geste::Zoom(p))
        );
        assert_eq!(k.step(1.4, 0.3, 1, true, false, None, false), None);
        k.step(2.0, 0.3, 1, true, false, Some(p), false);
        assert_eq!(
            k.step(2.2, 0.3, 1, true, false, Some(p), true),
            Some(Geste::Raster)
        );
        assert_eq!(k.step(2.6, 0.3, 1, true, false, None, false), None);
    }
    #[test]
    fn tool_image_change_drag_and_second_press_cancel() {
        let mut k = ImageClick::default();
        let p = Pos2::ZERO;
        k.step(0.0, 0.3, 4, true, false, Some(p), false);
        assert_eq!(k.step(0.4, 0.3, 4, false, false, None, true), None);
        k.step(1.0, 0.3, 4, true, false, Some(p), false);
        assert_eq!(k.step(1.5, 0.3, 5, true, false, None, false), None);
        k.step(2.0, 0.3, 5, true, false, Some(p), false);
        assert_eq!(k.step(2.4, 0.3, 5, true, true, None, false), None);
        assert_eq!(
            k.step(2.5, 0.3, 5, true, false, Some(p), true),
            Some(Geste::Raster)
        );
    }
    #[test]
    fn egui_real_click_sequence_and_tool_priority() {
        let ctx = Context::default();
        let mut k = ImageClick::default();
        let rect = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 200.0));
        let mut run = |time, events, enabled| {
            let mut out = None;
            let _ = crate::ui_test::run(&ctx,
                egui::RawInput {
                    screen_rect: Some(rect),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        let r = ui.allocate_rect(rect.shrink(10.0), egui::Sense::click_and_drag());
                        out = k.answer(&r, 1, enabled, rect.shrink(20.0));
                    });
                },
            );
            out
        };
        let p = Pos2::new(100.0, 100.0);
        let event = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run(0.0, vec![], true);
        run(1.0, vec![egui::Event::PointerMoved(p), event(true)], true);
        assert_eq!(run(1.02, vec![event(false)], true), None);
        run(1.1, vec![event(true)], true);
        assert_eq!(run(1.12, vec![event(false)], true), Some(Geste::Raster));
        assert_eq!(run(1.7, vec![], true), None);
        run(2.0, vec![event(true)], false);
        run(2.02, vec![event(false)], false);
        assert_eq!(run(2.5, vec![], true), None);
        run(3.0, vec![event(true)], true);
        run(3.02, vec![event(false)], true);
        assert_eq!(run(3.5, vec![], true), Some(Geste::Zoom(p)));
        run(4.0, vec![event(true)], true);
        run(4.02, vec![event(false)], true);
        let secondary = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Default::default(),
        };
        assert_eq!(run(4.321, vec![secondary(true)], true), None);
        assert_eq!(run(4.5, vec![secondary(false)], true), None);
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    use crate::workspace::ToolTab;
    #[test]
    fn automatic_view_export_and_quick_collection_in_both_views() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-navigation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            image::RgbImage::from_pixel(1600, 1200, image::Rgb([120, 70, 90]))
                .save(dir.join(format!("image-{i}.png")))
                .unwrap();
        }
        let ctx = Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(dir.clone());
        assert!(a.in_grid());
        assert_eq!(a.tool_tab, ToolTab::Info);
        a.lib.set_active(a.entries[1].image);
        assert_eq!(a.selection(), vec![1]);
        a.zoom = 4.0;
        a.pan = egui::vec2(20.0, 30.0);
        a.to_single_image();
        assert_eq!(a.zoom, 1.0);
        assert_eq!(a.pan, egui::Vec2::ZERO);
        assert_eq!(a.tool_tab, ToolTab::Edit);
        // Test the real viewer module with a synthetic photo, including
        // coordinate zoom and return. The events stay in the egui test context.
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(
                std::time::Instant::now() < deadline,
                "synthetic photo does not load"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let frame = |a: &mut App, time, events| {
            let _ = crate::ui_test::run(&ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        Pos2::ZERO,
                        egui::vec2(700.0, 500.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| a.viewer(ui));
                },
            );
        };
        frame(&mut a, 0.0, vec![]);
        let p = a.image_rect.unwrap().center() + egui::vec2(35.0, -20.0);
        let event = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(&mut a, 1.0, vec![egui::Event::PointerMoved(p), event(true)]);
        frame(&mut a, 1.02, vec![event(false)]);
        frame(&mut a, 1.319, vec![]);
        let fitted = a.image_rect.unwrap();
        let uv_before = (p - fitted.min) / fitted.size();
        assert_eq!(a.zoom, 1.0);
        frame(&mut a, 1.321, vec![]);
        if a.navigation.zoom.active() {
            assert_eq!(a.zoom, 1.0);
            frame(&mut a, 1.411, vec![]);
            assert!(a.zoom > 1.1 && a.zoom < a.one_to_one);
            let size = a.fit_size * a.zoom;
            let uv_mid = (p - (fitted.center() + a.pan - size * 0.5)) / size;
            assert!((uv_mid-uv_before).length()<1e-4);
            frame(&mut a, 1.502, vec![]);
            assert!(!a.navigation.zoom.active());
        }
        assert!(a.zoom > 1.1);
        assert_ne!(a.pan, egui::Vec2::ZERO);
        let size = a.fit_size * a.zoom;
        let uv_after = (p - (fitted.center() + a.pan - size * 0.5)) / size;
        assert!(
            (uv_after - uv_before).length() < 1e-6,
            "Angeklickter Bildpunkt muss an seiner Bildschirmstelle bleiben"
        );
        frame(&mut a, 2.0, vec![event(true)]);
        frame(&mut a, 2.02, vec![event(false)]);
        frame(&mut a, 2.321, vec![]);
        if a.navigation.zoom.active() {
            assert!(a.zoom > 1.1);
            frame(&mut a, 2.411, vec![]);
            assert!(a.zoom > 1.0 && a.zoom < a.one_to_one);
            frame(&mut a, 2.502, vec![]);
        }
        assert_eq!(a.zoom, 1.0);
        assert_eq!(a.pan, egui::Vec2::ZERO);
        frame(&mut a, 3.0, vec![event(true)]);
        frame(&mut a, 3.02, vec![event(false)]);
        assert_eq!(a.zoom, 1.0);
        frame(&mut a, 3.1, vec![event(true)]);
        frame(&mut a, 3.12, vec![event(false)]);
        assert!(a.in_grid());
        assert!(!a.navigation.zoom.active());
        assert_eq!(a.zoom, 1.0);
        assert_eq!(a.tool_tab, ToolTab::Info);
        a.to_single_image();
        // All active tools block image navigation.
        assert!(a.image_click_free(&ctx));
        a.wb_picker = true;
        assert!(!a.image_click_free(&ctx));
        a.wb_picker = false;
        a.measure = true;
        assert!(!a.image_click_free(&ctx));
        a.measure = false;
        a.crop_mode = Some(([0.5, 0.5, 1.0, 1.0], 0.0, [0.0, 0.0]));
        assert!(!a.image_click_free(&ctx));
        a.crop_mode = None;
        a.loupe = true;
        assert!(!a.image_click_free(&ctx));
        a.loupe = false;
        a.to_grid();
        assert!(a.in_grid());
        assert_eq!(a.tool_tab, ToolTab::Info);
        assert_eq!(a.lib.active_image(), Some(a.entries[1].image));
        // A previously loaded/loading photo must not be added to the grid selection.
        a.lib.set_active(a.entries[2].image);
        assert_eq!(a.selection(), vec![2]);
        a.export_scope = 2;
        a.open_export();
        assert!(a.show_export);
        assert_eq!(a.export_scope, 0);
        assert!(!a.image_click_free(&ctx));
        a.show_export = false;
        // Real egui key events, the same handlers as in the app; no OS input.
        for detail in [false, true] {
            if detail {
                a.to_single_image();
            }
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key: egui::Key::B,
                physical_key: Some(egui::Key::B),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            });
            a.navigation
                .click
                .step(10.0, 0.3, 1, true, false, Some(Pos2::ZERO), false);
            eframe::App::raw_input_hook(&mut a, &ctx, &mut input);
            assert!(
                a.navigation.click.pending.is_none(),
                "Tasten verwerfen wartende Zoomklicks vor der geordneten Verarbeitung"
            );
            let _ = crate::ui_test::run(&ctx, input, |_| a.workspace_shortcuts(&ctx));
            assert!(
                a.in_grid(),
                "⌘B must also lead from single image view to the grid"
            );
            assert_eq!(a.lib.source_title(), "Schnellsammlung");
            let mut release = egui::RawInput::default();
            release.events.push(egui::Event::Key {
                key: egui::Key::B,
                physical_key: Some(egui::Key::B),
                pressed: false,
                repeat: false,
                modifiers: Default::default(),
            });
            let _ = crate::ui_test::run(&ctx, release, |_| {});
            a.open_folder_inner(dir.clone());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod unreachable_tests {
    use super::*;

    /// Original on an unreachable location (state 2): the darkroom refuses with a notice, export and pasting
    /// leave the photo out and say so, the preview zooms at most to its own resolution.
    #[test]
    fn unreachable_original_locks_darkroom_export_and_zooms_only_the_preview() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-unreachable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..2 {
            image::RgbImage::from_pixel(640, 480, image::Rgb([90, 110, 120])).save(dir.join(format!("photo-{i}.png"))).unwrap();
        }
        let ctx = Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(dir.clone());
        assert_eq!(a.entries.len(), 2);
        a.entries[1].state = 2;
        a.lib.set_active(a.entries[1].image);
        assert!(a.active_unreachable());
        // darkroom: refused, notice in the status line
        a.enter_darkroom();
        assert!(!a.darkroom.active);
        assert!(a.status.contains("Original nicht erreichbar"), "{}", a.status);
        // pasting/syncing: only the reachable ones
        assert_eq!(a.without_unreachable(vec![0, 1]), (vec![0], 1));
        // export: "all" leaves the unreachable photo out
        a.export_scope = 2;
        let mut items = Vec::new();
        let _ = crate::ui_test::run(&ctx, egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| items = a.export_controls(ui, true));
        });
        assert_eq!(items, vec![0]);
        // single image without original: preview (here 4000 × 3000) fitted, a click zooms to one preview pixel per screen pixel
        // the GPU's limit as on a Mac (Metal allows at least 8192 px per side; headless egui assumes 2048)
        let _ = crate::ui_test::run(&ctx, egui::RawInput { max_texture_side: Some(8192), ..Default::default() }, |_| {});
        let big = egui::ColorImage::filled([4000, 3000], egui::Color32::GRAY);
        a.entries[1].thumb = Some(ctx.load_texture("test-preview", big, Default::default()));
        a.not_developable = Some(1);
        a.tab = crate::Tab::Develop;
        let screen = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 500.0));
        let mut frame = |a: &mut App, time: f64, events: Vec<egui::Event>| {
            let _ = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), time: Some(time), events, ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| a.viewer(ui));
            });
        };
        frame(&mut a, 0.0, vec![]);
        let p = Pos2::new(350.0, 250.0);
        let button = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&mut a, 1.0, vec![egui::Event::PointerMoved(p), button(true)]);
        frame(&mut a, 1.02, vec![button(false)]);
        frame(&mut a, 1.5, vec![]);
        let (image, zoom, _) = a.offline_zoom;
        assert_eq!(image, a.entries[1].image);
        // the area is about 700 × 500 minus the panel margins: fitted ≈ 0.16, so the limit is ≈ 6×
        assert!(zoom > 5.0 && zoom < 7.5, "zoom {zoom}");
        // a second click goes back to fitted
        frame(&mut a, 2.0, vec![button(true)]);
        frame(&mut a, 2.02, vec![button(false)]);
        frame(&mut a, 2.5, vec![]);
        assert_eq!(a.offline_zoom.1, 1.0);
        // a small preview does not zoom at all
        let small = egui::ColorImage::filled([400, 300], egui::Color32::GRAY);
        a.entries[1].thumb = Some(ctx.load_texture("test-small", small, Default::default()));
        frame(&mut a, 3.0, vec![button(true)]);
        frame(&mut a, 3.02, vec![button(false)]);
        frame(&mut a, 3.5, vec![]);
        assert_eq!(a.offline_zoom.1, 1.0);
        assert!(!a.in_grid());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
