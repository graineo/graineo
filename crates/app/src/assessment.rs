//! Assessment frame (after ISO 12646 and darktable's "color assessment" mode): in Develop the single photo is shown
//! smaller on a neutral mid grey, with a white frame around it, so colours and tones are judged against a fixed
//! neutral reference instead of the dark panel. Switched in the toolbar under the photo (and with [`SHORTCUT`]); the
//! width of the margin is a program setting (Settings → Develop). Only the view changes, never the photo.

use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{self, Color32};

/// Key in `settings.json`: frame on.
pub const KEY: &str = "assessmentFrame";
/// Key in `settings.json`: margin in percent of the shorter side of the view.
pub const WIDTH_KEY: &str = "assessmentFrameWidth";
/// Id of the shortcut ([`crate::shortcuts`]).
pub const SHORTCUT: &str = "assessment_frame";
/// Choosable margins in percent.
pub const WIDTHS: [u8; 4] = [2, 5, 10, 15];
const DEFAULT_WIDTH: u8 = 5;
/// Neutral mid grey of the surround (sRGB 118 ≈ L* 50).
pub const GREY: Color32 = Color32::from_gray(118);

/// 0 = not read yet, 1 = off, 2 = on.
static ON: AtomicU8 = AtomicU8::new(0);
/// 0 = not read yet, otherwise the margin in percent.
static WIDTH: AtomicU8 = AtomicU8::new(0);

/// Width from a value of `settings.json` (missing or not one of [`WIDTHS`]: the default).
pub fn width_from_value(v: &serde_json::Value) -> u8 {
    v.as_u64().and_then(|w| WIDTHS.iter().copied().find(|&x| x as u64 == w)).unwrap_or(DEFAULT_WIDTH)
}

/// Is the frame switched on?
pub fn on() -> bool {
    match ON.load(Ordering::Relaxed) {
        0 => {
            let on = library::settings::value(KEY).as_bool().unwrap_or(false);
            ON.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        v => v == 2,
    }
}

/// Switch the frame and remember it.
pub fn set_on(on: bool) {
    ON.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    library::settings::set(KEY, serde_json::json!(on));
}

/// Margin in percent.
pub fn width() -> u8 {
    match WIDTH.load(Ordering::Relaxed) {
        0 => {
            let w = width_from_value(&library::settings::value(WIDTH_KEY));
            WIDTH.store(w, Ordering::Relaxed);
            w
        }
        w => w,
    }
}

/// Set and remember the margin.
pub fn set_width(w: u8) {
    let w = width_from_value(&serde_json::json!(w));
    WIDTH.store(w, Ordering::Relaxed);
    library::settings::set(WIDTH_KEY, serde_json::json!(w));
}

/// Area left for the photo inside `view` with a margin of `percent` of its shorter side, and the thickness of the
/// white frame (a quarter of the margin, at least 2 points).
pub fn inner(view: egui::Rect, percent: u8) -> (egui::Rect, f32) {
    let pad = (view.width().min(view.height()) * percent as f32 / 100.0).round().max(4.0);
    (view.shrink(pad), (pad / 4.0).round().max(2.0))
}

/// Paint the grey surround of `view` (before the photo).
pub fn paint_ground(painter: &egui::Painter, view: egui::Rect) {
    painter.rect_filled(view, 0.0, GREY);
}

/// Paint the white frame just outside `photo` (the visible part of the photo), `line` points thick.
pub fn paint_frame(painter: &egui::Painter, photo: egui::Rect, line: f32) {
    painter.rect_stroke(photo, 0.0, egui::Stroke::new(line, Color32::WHITE), egui::StrokeKind::Outside);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_values() {
        assert_eq!(width_from_value(&serde_json::json!(10)), 10);
        assert_eq!(width_from_value(&serde_json::json!(15)), 15);
        assert_eq!(width_from_value(&serde_json::json!(7)), DEFAULT_WIDTH);
        assert_eq!(width_from_value(&serde_json::Value::Null), DEFAULT_WIDTH);
    }

    #[test]
    fn margin_follows_the_shorter_side_and_leaves_room_for_the_frame() {
        let view = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
        let (r, line) = inner(view, 5);
        assert_eq!(r, view.shrink(30.0));
        assert!(line < 30.0 && line >= 2.0);
        let (r2, _) = inner(view, 10);
        assert!(r2.width() < r.width());
    }
}
