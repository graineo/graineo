//! Face frames with names over the image in single view.
//!
//! The frames are normalised (0 … 1) in the image as the camera orients it. If the user additionally rotates the image
//! (`rotate` quarter turns clockwise) and crops, the frames follow via [`Frame`].
use library::people::FaceInfo;
use develop::geometry::Frame;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use std::cell::RefCell;

/// Point (0 … 1) after `r` quarter turns clockwise.
pub(crate) fn rotate_quarters(mut p: [f32; 2], r: u8) -> [f32; 2] {
    for _ in 0..r % 4 {
        p = [1.0 - p[1], p[0]];
    }
    p
}

/// Frame of a face on screen; `rect` is the image (whole crop) on screen.
fn screen_rect(g: &FaceInfo, rect: Rect, frame: &Frame, r: u8) -> Rect {
    let (ow, oh) = frame.oriented();
    let corners = [[g.x, g.y], [g.x + g.b, g.y], [g.x + g.b, g.y + g.h], [g.x, g.y + g.h]].map(|p| {
        let d = rotate_quarters(p, r);
        let u = frame.from_ori(frame.ori_to_persp([d[0] * ow, d[1] * oh]));
        Pos2::new(rect.min.x + u[0] * rect.width(), rect.min.y + u[1] * rect.height())
    });
    Rect::from_points(&corners)
}

/// Does a face frame lie mostly on the image (`rect`: the whole crop on screen)? A frame outside the crop – found on
/// another image than the photo, or outside the area kept by the crop – would stand in the dark area beside the photo
/// with its name; it is not drawn.
pub fn on_image(box_: Rect, rect: Rect) -> bool {
    let i = box_.intersect(rect);
    let area = box_.width() * box_.height();
    area > 0.0 && i.width() > 0.0 && i.height() > 0.0 && i.width() * i.height() >= 0.5 * area
}

/// Open name field: face, input, anchor point, already focused?
struct NameField {
    face: i64,
    text: String,
    anchor: Pos2,
    focused: bool,
    has_name: bool,
    /// just drawn by hand: cancel discards the face again
    is_new: bool,
}

thread_local! {
    static NAME_FIELD: RefCell<Option<NameField>> = const { RefCell::new(None) };
    static RESULT: RefCell<Option<(i64, String)>> = const { RefCell::new(None) };
    /// Draw a frame by hand: on/off, start point of the drag
    static DRAW_BY_HAND: RefCell<(bool, Option<Pos2>)> = const { RefCell::new((false, None)) };
    /// finished drawn frame (normalised x, y, w, h) and anchor point for the name field
    static NEW_FRAME: RefCell<Option<([f32; 4], Pos2)>> = const { RefCell::new(None) };
    static DISCARDED: RefCell<Option<i64>> = const { RefCell::new(None) };
}

/// Screen point → point (0 … 1) in the camera-oriented image (inverse of [`screen_rect`]).
fn screen_to_image(p: Pos2, rect: Rect, frame: &Frame, r: u8) -> [f32; 2] {
    let u = [(p.x - rect.min.x) / rect.width(), (p.y - rect.min.y) / rect.height()];
    let o = frame.persp_to_ori(frame.to_ori(u));
    let (ow, oh) = frame.oriented();
    rotate_quarters([o[0] / ow, o[1] / oh], (4 - r % 4) % 4)
}

/// Finished drawn frame (x, y, width, height normalised) with anchor point for the name field.
pub fn fetch_frames() -> Option<([f32; 4], Pos2)> {
    NEW_FRAME.with(|e| e.borrow_mut().take())
}

/// Open the name field for a face just created by hand.
pub fn new_name_field(face: i64, anchor: Pos2) {
    NAME_FIELD.with(|f| *f.borrow_mut() = Some(NameField { face, text: String::new(), anchor, focused: false, has_name: false, is_new: true }));
}

/// A face created by hand whose name field was cancelled (to discard).
pub fn fetch_discarded() -> Option<i64> {
    DISCARDED.with(|e| e.borrow_mut().take())
}

/// "+ Face" toggle and drawing a frame. Only in the image: `image` is the visible part of the image on screen.
fn draw_by_hand(ui: &mut egui::Ui, avail: Rect, rect: Rect, frame: &Frame, r: u8) {
    let painter = ui.painter().with_clip_rect(avail);
    let (drawing_on, start) = DRAW_BY_HAND.with(|h| *h.borrow());
    let text = painter.layout_no_wrap(crate::tr("+ Gesicht").to_string(), egui::FontId::proportional(12.0), crate::lr::gray(235));
    let toggle_rect = Rect::from_min_size(avail.left_top() + egui::vec2(12.0, 12.0), text.size() + egui::vec2(16.0, 8.0));
    let resp = ui.interact(toggle_rect, egui::Id::new("gesicht-von-hand"), egui::Sense::click());
    painter.rect_filled(toggle_rect, 0.0, Color32::from_black_alpha(if drawing_on || resp.hovered() { 235 } else { 190 }));
    if drawing_on {
        painter.rect_stroke(toggle_rect, 0.0, Stroke::new(1.0, crate::lr::gray(220)), egui::StrokeKind::Inside);
    }
    painter.galley(toggle_rect.min + egui::vec2(8.0, 4.0), text, crate::lr::TEXT_STRONG());
    if resp.clicked() {
        DRAW_BY_HAND.with(|h| *h.borrow_mut() = (!drawing_on, None));
    }
    if !drawing_on {
        return;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        DRAW_BY_HAND.with(|h| *h.borrow_mut() = (false, None));
        return;
    }
    let drag_area = ui.interact(avail.intersect(rect), egui::Id::new("gesicht-ziehen"), egui::Sense::click_and_drag());
    if drag_area.hovered() || drag_area.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if drag_area.drag_started() {
        let p = drag_area.interact_pointer_pos();
        DRAW_BY_HAND.with(|h| h.borrow_mut().1 = p);
    }
    let (Some(a), Some(b)) = (start, drag_area.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()))) else { return };
    let dragged_rect = Rect::from_two_pos(a, b);
    if drag_area.dragged() {
        painter.rect_stroke(dragged_rect.expand(1.0), 0.0, Stroke::new(3.0, Color32::from_black_alpha(110)), egui::StrokeKind::Outside);
        painter.rect_stroke(dragged_rect, 0.0, Stroke::new(1.2, crate::lr::TEXT_STRONG()), egui::StrokeKind::Outside);
    }
    if drag_area.drag_stopped() {
        DRAW_BY_HAND.with(|h| *h.borrow_mut() = (false, None));
        if dragged_rect.width() >= 12.0 && dragged_rect.height() >= 12.0 {
            let corners = [dragged_rect.left_top(), dragged_rect.right_top(), dragged_rect.right_bottom(), dragged_rect.left_bottom()].map(|p| screen_to_image(p, rect, frame, r));
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for e in corners {
                (x0, y0, x1, y1) = (x0.min(e[0]), y0.min(e[1]), x1.max(e[0]), y1.max(e[1]));
            }
            let (x0, y0, x1, y1) = (x0.clamp(0.0, 1.0), y0.clamp(0.0, 1.0), x1.clamp(0.0, 1.0), y1.clamp(0.0, 1.0));
            if x1 > x0 && y1 > y0 {
                NEW_FRAME.with(|e| *e.borrow_mut() = Some(([x0, y0, x1 - x0, y1 - y0], Pos2::new(dragged_rect.left(), dragged_rect.bottom() + 6.0))));
            }
        }
    }
}

/// Pick up the user's input: (face, name); an empty name removes the name.
pub fn fetch() -> Option<(i64, String)> {
    RESULT.with(|e| e.borrow_mut().take())
}

/// Draw frames. Below each frame is the name (suggestions with "?"); a click on it opens the name field. Faces without
/// a name show "+ Name" only when the pointer is over them.
pub fn draw(ui: &mut egui::Ui, avail: Rect, rect: Rect, frame: &Frame, r: u8, faces: &[FaceInfo], names: &[(i64, String)]) {
    let painter = ui.painter().with_clip_rect(avail);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let hand_mode = DRAW_BY_HAND.with(|h| h.borrow().0);
    // Pointer over a face, its name tag or the open name field: clicks belong to the faces, not to the image zoom
    let mut above = NAME_FIELD.with(|f| f.borrow().is_some());
    for g in faces {
        let box_ = screen_rect(g, rect, frame, r);
        if !box_.intersects(avail) || !on_image(box_, rect) {
            continue;
        }
        let named = !g.name.is_empty() && g.status != "rejected";
        let (color, name) = match g.status.as_str() {
            "confirmed" if named => (crate::lr::overlay(230), g.name.clone()),
            "suggested" if named => (crate::lr::overlay(150), format!("{}?", g.name)),
            _ => (crate::lr::overlay(90), String::new()),
        };
        painter.rect_stroke(box_.expand(1.0), 0.0, Stroke::new(3.0, Color32::from_black_alpha(110)), egui::StrokeKind::Outside);
        painter.rect_stroke(box_, 0.0, Stroke::new(1.2, color), egui::StrokeKind::Outside);
        above |= pointer.is_some_and(|p| box_.expand(3.0).contains(p));
        let open = NAME_FIELD.with(|f| f.borrow().as_ref().is_some_and(|f| f.face == g.id));
        // The whole area of the face is clickable (not just the name tag); a click opens the name field
        if !hand_mode {
            let drag_area = ui.interact(box_.intersect(avail), egui::Id::new(("gesicht-flaeche", g.id)), egui::Sense::click());
            if drag_area.clicked() {
                NAME_FIELD.with(|f| *f.borrow_mut() = Some(NameField { face: g.id, text: g.name.clone(), anchor: Pos2::new(box_.left(), box_.bottom() + 30.0), focused: false, has_name: named, is_new: false }));
            }
        }
        let over = pointer.is_some_and(|p| box_.expand(4.0).contains(p) || (box_.expand2(egui::vec2(40.0, 0.0)).contains(p) && p.y < box_.bottom() + 28.0 && p.y > box_.top()));
        if name.is_empty() && !over && !open {
            continue;
        }
        let text = if name.is_empty() { crate::tr("+ Name").to_string() } else { name };
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), crate::lr::gray(235));
        let pos = Pos2::new(box_.center().x - galley.size().x / 2.0, box_.bottom() + 4.0);
        let tag_bg = Rect::from_min_size(pos - egui::vec2(5.0, 2.0), galley.size() + egui::vec2(10.0, 4.0));
        let resp = ui.interact(tag_bg.intersect(avail), egui::Id::new(("gesicht-name", g.id)), egui::Sense::click());
        above |= pointer.is_some_and(|p| tag_bg.expand(2.0).contains(p));
        let brighter = resp.hovered() || open;
        painter.rect_filled(tag_bg, 0.0, Color32::from_black_alpha(if brighter { 235 } else { 190 }));
        painter.galley(pos, galley, crate::lr::TEXT_STRONG());
        if resp.clicked() {
            NAME_FIELD.with(|f| *f.borrow_mut() = Some(NameField { face: g.id, text: g.name.clone(), anchor: Pos2::new(tag_bg.left(), tag_bg.bottom() + 4.0), focused: false, has_name: named, is_new: false }));
        }
    }
    draw_by_hand(ui, avail, rect, frame, r);
    above |= hand_mode;
    let now = ui.input(|i| i.time);
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("gesicht-darueber"), (now, above)));
    // Test hook: GRAINEO_FACE_FIELD=1 opens the name field at the first face (for screenshots)
    if crate::env::var_os("GRAINEO_FACE_FIELD").is_some() && ui.input(|i| i.time) > 4.0 {
        static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Some(g) = faces.get(2).filter(|_| !ONCE.swap(true, std::sync::atomic::Ordering::Relaxed)) {
            let b = screen_rect(g, rect, frame, r);
            NAME_FIELD.with(|f| *f.borrow_mut() = Some(NameField { face: g.id, text: g.name.clone(), anchor: Pos2::new(b.left(), b.bottom() + 30.0), focused: false, has_name: !g.name.is_empty(), is_new: false }));
        }
    }
    name_fields(ui.ctx(), names);
}

/// Is the pointer over a face, a name tag or the name field (state of the last frame, only valid while faces are shown)?
pub fn above(ctx: &egui::Context) -> bool {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.get_temp::<(f64, bool)>(egui::Id::new("gesicht-darueber"))).is_some_and(|(t, b)| b && now - t < 0.3)
}

fn name_fields(ctx: &egui::Context, names: &[(i64, String)]) {
    let Some(mut f) = NAME_FIELD.with(|f| f.borrow_mut().take()) else { return };
    let mut done: Option<String> = None;
    let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let area = egui::Area::new(egui::Id::new("gesicht-feld")).order(egui::Order::Foreground).fixed_pos(f.anchor).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(260.0);
            let r = crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut f.text).hint_text(crate::tr("Name …")).desired_width(240.0));
            if !f.focused {
                r.request_focus();
                f.focused = true;
            }
            let input_text = f.text.trim().to_string();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !input_text.is_empty() {
                done = Some(input_text.clone());
            }
            if !input_text.is_empty() {
                let k = library::people::person_brief(&input_text);
                for (_, n) in names.iter().filter(|(_, n)| library::people::person_brief(n).contains(&k)).take(5) {
                    if ui.selectable_label(false, n).clicked() {
                        done = Some(n.clone());
                    }
                }
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!input_text.is_empty(), egui::Button::new(crate::tr("Übernehmen"))).clicked() {
                    done = Some(input_text.clone());
                }
                if f.has_name && ui.button(crate::tr("Name entfernen")).clicked() {
                    done = Some(String::new());
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    close = true;
                }
            });
        });
    });
    // a click outside closes
    if area.response.clicked_elsewhere() && f.focused && ctx.input(|i| i.pointer.any_pressed()) {
        close = true;
    }
    if let Some(name) = done {
        RESULT.with(|e| *e.borrow_mut() = Some((f.face, name)));
    } else if !close {
        NAME_FIELD.with(|x| *x.borrow_mut() = Some(f));
    } else if f.is_new {
        DISCARDED.with(|e| *e.borrow_mut() = Some(f.face));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_follows_rotation() {
        // Landscape image 6000 × 4000, face top left; quarter clockwise → top right in portrait
        let frame = Frame::full(6000, 4000, 1);
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 600.0));
        let g = FaceInfo { id: 1, status: "confirmed".into(), name: "A".into(), confidence: 1.0, x: 0.0, y: 0.0, b: 0.1, h: 0.1 };
        let b = screen_rect(&g, rect, &frame, 0);
        assert!(b.min.x < 1.0 && b.min.y < 1.0, "without rotation top left: {b:?}");
        let g2 = FaceInfo { x: 0.0, y: 0.0, ..g };
        let b2 = screen_rect(&g2, rect, &Frame::full(6000, 4000, 1), 1);
        assert!(b2.max.x > 399.0 && b2.min.y < 1.0, "after a quarter turn top right: {b2:?}");
    }

    #[test]
    fn screen_point_maps_back_to_camera_image() {
        // the frame already carries the user's rotation in its quarters (camera 0)
        for r in 0..4u8 {
            let frame = Frame::full(6000, 4000, r);
            let rect = Rect::from_min_size(Pos2::new(30.0, 20.0), egui::vec2(300.0, 200.0));
            let g = FaceInfo { id: 1, status: "confirmed".into(), name: "A".into(), confidence: 1.0, x: 0.2, y: 0.3, b: 0.1, h: 0.15 };
            let s = screen_rect(&g, rect, &frame, r);
            let a = screen_to_image(s.left_top(), rect, &frame, r);
            let c = screen_to_image(s.right_bottom(), rect, &frame, r);
            let (x0, x1, y0, y1) = (a[0].min(c[0]), a[0].max(c[0]), a[1].min(c[1]), a[1].max(c[1]));
            assert!((x0 - 0.2).abs() < 1e-3 && (x1 - 0.3).abs() < 1e-3 && (y0 - 0.3).abs() < 1e-3 && (y1 - 0.45).abs() < 1e-3, "r={r}: {a:?} {c:?}");
        }
    }
}
