//! Visible switches for what Tab, ⇧Tab and L do: panels on/off and "lights out". The buttons sit at a fixed place
//! at the right end of the top bar and drive the same state as the keys (`show_*`, `darkroom.image_only`, `lights`),
//! so there is no second state. When the top bar is hidden (⇧Tab, fullscreen preview) or the lights are dimmed, one
//! pale button stays in the bottom right corner of the window and brings everything back: no way is ever hidden.

use eframe::egui;
use egui::{Color32, Rect, Stroke};

use crate::{lr, App};

/// Which symbol a switch shows.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Symbol {
    /// Frame with a strip on each side: the panels.
    Panels,
    /// Frame with a bright centre: the image stays lit, the surroundings go dark.
    Lights,
}

/// Size of the switches (like the other icon buttons).
const SIZE: egui::Vec2 = egui::vec2(26.0, 24.0);
/// Distance of the way-back button from the window corner: above the bar below the image (zoom buttons), so it covers nothing.
const CORNER_GAP: egui::Vec2 = egui::vec2(14.0, 56.0);
/// Layer order of the way-back button: above everything painted in `Foreground`, including the lights-out shade.
const WAY_BACK_ORDER: egui::Order = egui::Order::Tooltip;
/// Pointer zone around the corner in which the way-back button gets stronger.
const CORNER_ZONE: egui::Vec2 = egui::vec2(120.0, 130.0);

fn paint_symbol(painter: &egui::Painter, rect: Rect, symbol: Symbol, color: Color32) {
    let stroke = Stroke::new(1.2, color);
    let frame = Rect::from_center_size(rect.center(), egui::vec2(15.0, 11.0));
    painter.rect_stroke(frame, 1.5, stroke, egui::StrokeKind::Inside);
    match symbol {
        Symbol::Panels => {
            let (l, r) = (frame.left() + frame.width() * 0.3, frame.right() - frame.width() * 0.3);
            painter.line_segment([egui::pos2(l, frame.top()), egui::pos2(l, frame.bottom())], stroke);
            painter.line_segment([egui::pos2(r, frame.top()), egui::pos2(r, frame.bottom())], stroke);
        }
        Symbol::Lights => {
            painter.rect_filled(Rect::from_center_size(frame.center(), egui::vec2(7.0, 5.0)), 0.5, color);
        }
    }
}

/// One switch: fill and white symbol when on (the one treatment for "active" in the bars), grey when it does nothing here.
fn switch(ui: &mut egui::Ui, symbol: Symbol, on: bool, enabled: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(SIZE, egui::Sense::click());
    let (fill, fg) = if !enabled {
        (Color32::TRANSPARENT, Color32::from_gray(0x5c))
    } else if on {
        (Color32::from_gray(0x38), Color32::WHITE)
    } else if resp.hovered() {
        (Color32::from_gray(0x2a), Color32::from_gray(0xe8))
    } else {
        (Color32::TRANSPARENT, lr::TEXT)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    if resp.has_focus() {
        painter.rect_stroke(rect, 0.0, Stroke::new(1.5, lr::ACCENT), egui::StrokeKind::Inside);
    }
    paint_symbol(painter, rect, symbol, fg);
    lr::hover_tip(ui, &resp, tip);
    resp.clicked() && enabled
}

/// Tooltip texts of the two switches (with the shortcut from the central table in brackets).
fn panels_tip(hidden: bool) -> String {
    if hidden {
        lr::with_shortcut("Bedienfelder einblenden", "side_panels")
    } else {
        lr::with_shortcut("Bedienfelder ausblenden", "side_panels")
    }
}

fn lights_tip(lights: u8, usable: bool) -> String {
    match (usable, lights) {
        (false, _) => lr::with_shortcut("Licht aus: nur in der Einzelansicht", "lights_out"),
        (true, 0) => lr::with_shortcut("Licht dimmen", "lights_out"),
        (true, 1) => lr::with_shortcut("Licht ganz aus", "lights_out"),
        (true, _) => lr::with_shortcut("Licht wieder an", "lights_out"),
    }
}

impl App {
    /// Tab (`all` = false) and ⇧Tab (`all` = true), also for the button. Darkroom: both toggle "image only".
    pub(crate) fn toggle_panels(&mut self, all: bool) {
        if self.darkroom_active() {
            self.darkroom.image_only = !self.darkroom.image_only;
        } else if all {
            let any = self.show_top || self.show_filmstrip || self.show_left || self.show_right;
            self.show_top = !any;
            self.show_filmstrip = !any;
            self.show_left = !any;
            self.show_right = !any;
        } else {
            let any = self.show_left || self.show_right;
            self.show_left = !any;
            self.show_right = !any;
        }
    }

    /// L, also for the button: normal → dimmed → off → normal.
    pub(crate) fn cycle_lights(&mut self) {
        self.lights = (self.lights + 1) % 3;
    }

    /// The side panels are hidden (darkroom: "image only"), the state shown by the panels switch.
    pub(crate) fn panels_hidden(&self) -> bool {
        if self.darkroom_active() {
            self.darkroom.image_only
        } else {
            !(self.show_left || self.show_right)
        }
    }

    /// The lights switch works where L works: in the single view and the darkroom; an active dimming can always be undone.
    fn lights_usable(&self) -> bool {
        !self.in_grid() || self.lights > 0
    }

    /// The top bar is not visible, or the lights are dimmed: the corner button must offer the way back.
    pub(crate) fn needs_way_back(&self) -> bool {
        self.preview_full.is_some() || self.lights > 0 || (!self.darkroom_active() && !self.show_top)
    }

    /// Everything visible again: leave the fullscreen preview, show all panels, lights on.
    pub(crate) fn show_everything(&mut self, ctx: &egui::Context) {
        if self.preview_full.is_some() {
            self.toggle_preview_full(ctx);
        }
        self.show_top = true;
        self.show_filmstrip = true;
        self.show_left = true;
        self.show_right = true;
        self.darkroom.image_only = false;
        self.lights = 0;
    }

    /// The two switches at the right end of the top bar (call first in a right-to-left row: the lights switch ends up
    /// at the very right, the panels switch to its left).
    pub(crate) fn panel_buttons(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 2.0;
        let usable = self.lights_usable();
        if switch(ui, Symbol::Lights, self.lights > 0, usable, &lights_tip(self.lights, usable)) {
            self.cycle_lights();
        }
        let hidden = self.panels_hidden();
        if switch(ui, Symbol::Panels, hidden, true, &panels_tip(hidden)) {
            self.toggle_panels(false);
        }
        ui.add_space(12.0);
    }

    /// Pale button in the bottom right corner while the top bar is hidden or the lights are dimmed; stronger when the
    /// pointer is near the corner. One click shows everything again.
    pub(crate) fn way_back_button(&mut self, ctx: &egui::Context) {
        if !self.needs_way_back() {
            return;
        }
        let screen = ctx.content_rect();
        let pos = screen.right_bottom() - SIZE - CORNER_GAP;
        let zone = Rect::from_min_max(screen.right_bottom() - CORNER_ZONE, screen.right_bottom());
        let near = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| zone.contains(p));
        let t = ctx.animate_bool_with_time(egui::Id::new("way-back-near"), near, 0.15);
        let strength = 0.4 + 0.6 * t;
        let hidden = self.panels_hidden() || !self.show_top || self.preview_full.is_some();
        let (symbol, tip) = match (hidden, self.lights > 0) {
            (true, true) => (Symbol::Panels, lr::with_shortcut("Alles wieder einblenden", "all_panels")),
            (true, false) => (Symbol::Panels, lr::with_shortcut("Bedienfelder einblenden", "all_panels")),
            _ => (Symbol::Lights, lr::with_shortcut("Licht wieder an", "lights_out")),
        };
        // above the whole Foreground order: the lights-out shade is painted on an unregistered Foreground layer, and
        // egui draws such layers after all registered ones, so in Foreground the shade covered the button (with the
        // lights fully out it was black on black, the way back invisible)
        let area = egui::Area::new(egui::Id::new("way-back"))
            .order(WAY_BACK_ORDER)
            .fixed_pos(pos)
            .constrain(false)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(SIZE, egui::Sense::click());
                let painter = ui.painter();
                let bg = if resp.hovered() { Color32::from_gray(0x38) } else { Color32::from_gray(0x20) };
                painter.rect_filled(rect, 0.0, bg.gamma_multiply(strength));
                paint_symbol(painter, rect, symbol, Color32::from_gray(0xe0).gamma_multiply(strength));
                lr::hover_tip(ui, &resp, &tip);
                resp.clicked()
            });
        ctx.move_to_top(area.response.layer_id);
        if area.inner {
            self.show_everything(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;

    #[test]
    fn tips_name_the_shortcut_and_the_next_step() {
        assert!(panels_tip(false).contains("Tab"));
        assert_ne!(panels_tip(false), panels_tip(true));
        assert_ne!(lights_tip(0, true), lights_tip(1, true));
        assert_ne!(lights_tip(1, true), lights_tip(2, true));
        assert!(lights_tip(0, true).contains('L'));
        assert_ne!(lights_tip(0, false), lights_tip(0, true));
    }

    fn app() -> (egui::Context, App, std::sync::MutexGuard<'static, ()>) {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let lock = crate::ui_test::app_lock();
        let a = App::new(&cc);
        (ctx, a, lock)
    }

    const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

    fn frame(ctx: &egui::Context, a: &mut App) -> egui::FullOutput {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, SCREEN)), ..Default::default() };
        crate::ui_test::run(ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()))
    }

    /// Position in the paint order (later = on top) of the way-back button's fill and of the last opaque shade
    /// over its place.
    fn way_back_and_shade(out: &egui::FullOutput) -> (Option<usize>, Option<usize>) {
        let button = Rect::from_min_size((SCREEN - SIZE - CORNER_GAP).to_pos2(), SIZE);
        let (mut b, mut shade) = (None, None);
        for (i, c) in out.shapes.iter().enumerate() {
            if let egui::Shape::Rect(r) = &c.shape {
                if r.rect == button {
                    b = Some(i);
                } else if r.fill == Color32::from_black_alpha(255) && r.rect.contains_rect(button) {
                    shade = Some(i);
                }
            }
        }
        (b, shade)
    }

    #[test]
    fn modes_toggle_and_always_have_a_way_back() {
        let (ctx, mut a, _lock) = app();
        let folder = std::env::temp_dir().join(format!("graineo-panelknoepfe-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(900, 600, image::Rgb([110, 80, 60])).save(folder.join("sample.png")).unwrap();
        a.open_folder_inner(folder.clone());
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.is_none() || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        frame(&ctx, &mut a);
        assert!(a.tab == crate::Tab::Develop);
        assert!(!a.panels_hidden() && !a.needs_way_back());

        // Tab: side panels away, the top bar (with the switches) stays, so no extra button
        a.toggle_panels(false);
        assert!(a.panels_hidden() && !a.needs_way_back());
        a.toggle_panels(false);
        assert!(!a.panels_hidden());

        // ⇧Tab: everything away, the corner button takes over; it restores everything at once
        a.toggle_panels(true);
        assert!(!a.show_top && !a.show_filmstrip && a.panels_hidden() && a.needs_way_back());
        frame(&ctx, &mut a);
        a.show_everything(&ctx);
        assert!(a.show_top && a.show_filmstrip && a.show_left && a.show_right && !a.needs_way_back());

        // lights: normal → dimmed → off → normal; while dimmed the corner button is there
        a.cycle_lights();
        assert_eq!(a.lights, 1);
        assert!(a.needs_way_back());
        a.cycle_lights();
        assert_eq!(a.lights, 2);
        frame(&ctx, &mut a);
        // lights fully out: the way-back button is painted above the black shade, not hidden under it
        let (button, shade) = way_back_and_shade(&frame(&ctx, &mut a));
        let (button, shade) = (button.expect("way-back button painted"), shade.expect("shade over the corner"));
        assert!(button > shade, "way-back button (shape {button}) under the lights-out shade (shape {shade})");
        a.cycle_lights();
        assert_eq!(a.lights, 0);
        assert!(!a.needs_way_back());

        // both at once: one click brings both back
        a.toggle_panels(true);
        a.cycle_lights();
        assert!(a.needs_way_back());
        a.show_everything(&ctx);
        assert!(!a.needs_way_back() && !a.panels_hidden() && a.lights == 0);

        // darkroom: Tab = image only (same switch), way back by the top bar, restore clears it as well
        a.darkroom.active = true;
        a.toggle_panels(false);
        assert!(a.darkroom.image_only && a.panels_hidden() && !a.needs_way_back());
        a.toggle_panels(true);
        assert!(!a.darkroom.image_only);
        a.toggle_panels(false);
        a.show_everything(&ctx);
        assert!(!a.darkroom.image_only);
        a.darkroom.active = false;
        let _ = std::fs::remove_dir_all(&folder);

        // library: the lights switch does nothing (L does not either) unless dimming is still on
        a.tab = crate::Tab::Library;
        assert!(!a.lights_usable());
        a.lights = 1;
        assert!(a.lights_usable());
        frame(&ctx, &mut a);
    }
}
