//! Straighten by dragging with the right mouse button over the developed photo: the line of the tool
//! "Crop & straighten" (`align::line_angles`), released = angle set (one undo step like the tool). A right click
//! that moves less than [`MIN_DRAG`] keeps opening the context menu.

use crate::menu::i18n::DeComma as _;
use develop::geometry::Frame;
use eframe::egui::{self, Color32};

/// Movement (points) from which a right press becomes a line instead of a click.
pub const MIN_DRAG: f32 = 4.0;

#[derive(Default)]
pub struct RightLine {
    /// screen position of the right press and the same point in the uprighted image
    from: Option<(egui::Pos2, [f32; 2])>,
    /// the press has moved at least MIN_DRAG: a line, no context menu
    drawing: bool,
}

/// What a frame of [`RightLine::update`] yields.
pub struct Outcome {
    /// released with a usable line: the new crop angle (degrees)
    pub angle: Option<f32>,
    /// a line is (or was just) drawn: the context menu must not open on this release
    pub suppress_menu: bool,
}

impl RightLine {
    /// `rect`: the photo on screen, `frame`: its crop. `allowed`: no tool owns the image gestures.
    pub fn update(&mut self, ui: &egui::Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, frame: &Frame, allowed: bool) -> Outcome {
        let none = Outcome { angle: None, suppress_menu: false };
        let (pressed, down, released, pos) = ui.input(|i| {
            let b = egui::PointerButton::Secondary;
            (i.pointer.button_pressed(b), i.pointer.button_down(b), i.pointer.button_released(b), i.pointer.latest_pos())
        });
        let ori = |p: egui::Pos2| {
            let u = (p - rect.min) / rect.size();
            frame.to_ori([u.x, u.y])
        };
        if pressed {
            *self = Self::default();
            if let Some(p) = pos.filter(|p| allowed && resp.hovered() && rect.contains(*p)) {
                self.from = Some((p, ori(p)));
            }
        }
        let Some((p0, a)) = self.from else { return none };
        if !allowed {
            *self = Self::default();
            return none;
        }
        let p = pos.unwrap_or(p0);
        if !self.drawing && p.distance(p0) >= MIN_DRAG {
            self.drawing = true;
        }
        let b = ori(p);
        let (w, h) = frame.oriented();
        let long = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt() > 0.02 * w.max(h);
        if self.drawing {
            painter.line_segment([p0, p], egui::Stroke::new(3.5, Color32::from_black_alpha(120)));
            painter.line_segment([p0, p], egui::Stroke::new(1.5, crate::lr::TEXT_STRONG()));
            painter.circle_filled(p0, 3.0, crate::lr::TEXT_STRONG());
            if long {
                let deg = crate::align::line_angles(a, b);
                let center = p0.lerp(p, 0.5) + egui::vec2(0.0, -12.0);
                let g = painter.layout_no_wrap(format!("{deg:+.1}°").de_comma(), egui::FontId::proportional(12.0), crate::lr::TEXT_STRONG());
                painter.rect_filled(egui::Rect::from_center_size(center, g.size() + egui::vec2(8.0, 4.0)), 0.0, Color32::from_black_alpha(170));
                painter.galley(center - g.size() / 2.0, g, crate::lr::TEXT_STRONG());
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            ui.ctx().request_repaint();
        }
        let drawing = self.drawing;
        if released || !down {
            *self = Self::default();
            return Outcome { angle: (drawing && long).then(|| crate::align::line_angles(a, b)), suppress_menu: drawing };
        }
        Outcome { angle: None, suppress_menu: drawing }
    }
}
