//! Navigator: a small picture of the whole photo at the bottom right of the single view while the photo is larger
//! than the view, with a frame around the part on screen. A click or drag in it moves that part.
//!
//! It only draws from what the view already shows (the same texture, the same crop frame) and changes nothing but
//! the pan. It lies over the image and takes no room; it fades in and out and asks for no repaint at rest.
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::atomic::{AtomicU8, Ordering};

/// Long edge of the small picture in points.
pub const LONG_EDGE: f32 = 165.0;
/// Distance from the bottom right corner of the view.
pub const MARGIN: f32 = 14.0;
/// Fade in/out in seconds.
pub const FADE: f32 = 0.18;
/// The photo must exceed the view by this factor on one axis before the navigator appears …
const SHOW_ABOVE: f32 = 1.03;
/// … and it stays until the photo is (nearly) fitted again: no flicker around "fit".
const HIDE_BELOW: f32 = 1.005;
/// Dark plate around the small picture.
const PLATE: f32 = 3.0;
/// Settings key (`settings.json`).
const KEY: &str = "navigatorOnZoom";

/// 0 = not read yet, 1 = on, 2 = off.
static ENABLED: AtomicU8 = AtomicU8::new(0);

/// "Navigator beim Zoomen" (menu View), default on.
pub fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        0 => {
            // unit tests never read the settings file (shared data folder)
            let on = cfg!(test) || library::settings::value(KEY).as_bool().unwrap_or(true);
            ENABLED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
        v => v == 1,
    }
}

pub fn toggle() {
    let on = !enabled();
    ENABLED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
    if !cfg!(test) {
        library::settings::set(KEY, serde_json::json!(on));
    }
}

/// Is the photo larger than the view? `was`: the answer of the last frame (hysteresis).
pub fn wanted(was: bool, image: Vec2, view: Vec2) -> bool {
    if image.min_elem() <= 0.0 || view.min_elem() <= 0.0 {
        return false;
    }
    let ratio = (image.x / view.x).max(image.y / view.y);
    ratio > if was { HIDE_BELOW } else { SHOW_ABOVE }
}

/// Place of the small picture: bottom right of `view`, `lift` points higher when something else sits in that corner.
/// The long edge shrinks in a small view so that the navigator never takes more than a third of it.
pub fn mini_rect(view: Rect, image: Vec2, lift: f32) -> Rect {
    let long = LONG_EDGE.min(view.width() / 3.0).min(view.height() / 3.0).max(1.0);
    let size = image * (long / image.max_elem().max(1e-6));
    let max = view.right_bottom() - Vec2::new(MARGIN + PLATE, MARGIN + PLATE + lift.max(0.0));
    Rect::from_min_max(max - size, max)
}

/// The part of `image` (its rectangle on screen) inside `view`, drawn into `mini`.
pub fn viewport(image: Rect, view: Rect, mini: Rect) -> Rect {
    let size = image.size().max(Vec2::splat(1e-6));
    let seen = image.intersect(view);
    let u0 = ((seen.min - image.min) / size).clamp(Vec2::ZERO, Vec2::splat(1.0));
    let u1 = ((seen.max - image.min) / size).clamp(u0, Vec2::splat(1.0));
    Rect::from_min_max(mini.min + u0 * mini.size(), mini.min + u1 * mini.size())
}

/// Pan limits of the view: no empty border next to the photo, an axis that fits is centred (as in the single view).
pub fn clamp_pan(pan: Vec2, image: Vec2, view: Vec2) -> Vec2 {
    let excess = (image - view) * 0.5;
    Vec2::new(
        if excess.x > 0.5 { pan.x.clamp(-excess.x, excess.x) } else { 0.0 },
        if excess.y > 0.5 { pan.y.clamp(-excess.y, excess.y) } else { 0.0 },
    )
}

/// Pan that brings the photo point under `p` (a place in `mini`) to the middle of the view. `image`: size of the
/// photo on screen, `offset`: what the view adds to the pan (darkroom sheet).
pub fn pan_for(p: Pos2, mini: Rect, image: Vec2, view: Vec2, offset: Vec2) -> Vec2 {
    let u = ((p - mini.min) / mini.size().max(Vec2::splat(1e-6))).clamp(Vec2::ZERO, Vec2::splat(1.0));
    clamp_pan((Vec2::splat(0.5) - u) * image - offset, image, view)
}

/// A rectangle another overlay drew in the corner (the text badges at the bottom right): the navigator moves above it.
pub fn note_corner(ctx: &egui::Context, rect: Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let e = d.get_temp_mut_or_insert_with(corner_id(), || (pass, rect));
        *e = if e.0 == pass { (pass, e.1.union(rect)) } else { (pass, rect) };
    });
}

fn corner_id() -> egui::Id {
    egui::Id::unique("navigator-corner")
}

/// How far the navigator has to move up so that it does not cover `taken`.
pub fn lift_for(view: Rect, image: Vec2, taken: Option<Rect>) -> f32 {
    match taken {
        Some(t) if t.intersects(mini_rect(view, image, 0.0).expand(PLATE)) => (view.bottom() - t.top() - MARGIN + 6.0).max(0.0),
        _ => 0.0,
    }
}

/// What the single view hands over each frame.
pub struct View {
    /// image area of the view
    pub view: Rect,
    /// the photo on screen (may be larger than the view)
    pub image: Rect,
    /// what the view adds to the pan when it places the photo
    pub offset: Vec2,
    /// the view allows the navigator at all (no crop mode, nothing covering the image)
    pub allowed: bool,
    /// clicks belong to the navigator (no tool is painting on the image)
    pub interactive: bool,
}

/// Draws the navigator (faded) and returns the new pan while the pointer moves the frame.
/// `paint_image(painter, rect, tint)` draws the whole photo into `rect`.
pub fn show(ui: &mut egui::Ui, v: &View, paint_image: impl FnOnce(&egui::Painter, Rect, Color32)) -> Option<Vec2> {
    let ctx = ui.ctx().clone();
    let id = egui::Id::unique("navigator");
    let was = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let want = v.allowed && enabled() && wanted(was, v.image.size(), v.view.size());
    if want != was {
        ctx.data_mut(|d| d.insert_temp(id, want));
    }
    // repaints only while the fade runs
    let alpha = ctx.animate_bool_with_time(id.with("fade"), want, FADE);
    if alpha <= 0.0 || v.image.width() <= 0.0 || v.image.height() <= 0.0 {
        return None;
    }
    let pass = ctx.cumulative_pass_nr();
    let taken = ctx.data(|d| d.get_temp::<(u64, Rect)>(corner_id())).filter(|(p, _)| *p == pass).map(|(_, r)| r);
    let mini = mini_rect(v.view, v.image.size(), lift_for(v.view, v.image.size(), taken));
    let plate = mini.expand(PLATE);
    let a = |value: u8| (value as f32 * alpha) as u8;
    let painter = ui.painter_at(v.view);
    painter.rect_filled(plate, 0.0, Color32::from_black_alpha(a(170)));
    paint_image(&painter.with_clip_rect(mini.intersect(v.view)), mini, Color32::WHITE.gamma_multiply(alpha));
    let seen = viewport(v.image, v.view, mini);
    // outside the part on screen: slightly darker
    let shade = Color32::from_black_alpha(a(110));
    for r in [
        Rect::from_min_max(mini.min, egui::pos2(mini.right(), seen.top())),
        Rect::from_min_max(egui::pos2(mini.left(), seen.bottom()), mini.max),
        Rect::from_min_max(egui::pos2(mini.left(), seen.top()), egui::pos2(seen.left(), seen.bottom())),
        Rect::from_min_max(egui::pos2(seen.right(), seen.top()), egui::pos2(mini.right(), seen.bottom())),
    ] {
        if r.width() > 0.0 && r.height() > 0.0 {
            painter.rect_filled(r, 0.0, shade);
        }
    }
    painter.rect_stroke(seen.expand(1.0), 0.0, egui::Stroke::new(1.0, Color32::from_black_alpha(a(120))), egui::StrokeKind::Outside);
    painter.rect_stroke(seen, 0.0, egui::Stroke::new(1.0, Color32::from_white_alpha(a(255))), egui::StrokeKind::Inside);
    painter.rect_stroke(plate, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x5a).gamma_multiply(alpha)), egui::StrokeKind::Inside);

    if !want || !v.interactive {
        return None;
    }
    // over the image: takes click and drag there (arrow pointer, the image below does not pan or zoom)
    let resp = ui.interact(plate, id.with("area"), egui::Sense::click_and_drag());
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::Default);
    }
    let grab_id = id.with("grab");
    let Some(pos) = resp.interact_pointer_pos() else {
        ctx.data_mut(|d| d.remove_temp::<Vec2>(grab_id));
        return None;
    };
    // grabbed inside the frame: it keeps its place under the pointer; outside: its middle jumps to the pointer
    let grab = ctx.data(|d| d.get_temp::<Vec2>(grab_id)).unwrap_or_else(|| if seen.contains(pos) { pos - seen.center() } else { Vec2::ZERO });
    ctx.data_mut(|d| d.insert_temp(grab_id, grab));
    Some(pan_for(pos - grab, mini, v.image.size(), v.view.size(), v.offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn view() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(1200.0, 800.0))
    }

    /// The photo on screen as the single view places it: centred, moved by the pan.
    fn placed(image: Vec2, pan: Vec2) -> Rect {
        Rect::from_center_size(view().center() + pan, image)
    }

    #[test]
    fn hidden_when_fitted_shown_when_larger() {
        let v = view().size();
        // fitted landscape and portrait: one axis fills the view exactly
        assert!(!wanted(false, vec2(1200.0, 800.0), v));
        assert!(!wanted(false, vec2(533.0, 800.0), v));
        assert!(!wanted(false, vec2(1200.0, 300.0), v));
        // smaller than the view (crop margin, darkroom sheet)
        assert!(!wanted(false, vec2(900.0, 600.0), v));
        // zoomed: larger on one axis is enough
        assert!(wanted(false, vec2(2400.0, 1600.0), v));
        assert!(wanted(false, vec2(1000.0, 1500.0), v));
        assert!(wanted(false, vec2(2400.0, 600.0), v));
        // nothing to show without a photo
        assert!(!wanted(true, Vec2::ZERO, v));
        assert!(!wanted(true, vec2(2400.0, 1600.0), Vec2::ZERO));
    }

    #[test]
    fn hysteresis_around_fit() {
        let v = view().size();
        // a gesture that ends a little above "fit" (settles back to fit) does not bring the navigator up …
        assert!(!wanted(false, vec2(1200.0, 800.0) * 1.02, v));
        // … but once shown it stays until the photo is fitted again
        assert!(wanted(true, vec2(1200.0, 800.0) * 1.02, v));
        assert!(!wanted(true, vec2(1200.0, 800.0) * 1.004, v));
        assert!(!wanted(true, vec2(1200.0, 800.0), v));
        // sweeping the zoom up and down switches exactly once each way
        let mut on = false;
        let mut switches = 0;
        let steps: Vec<f32> = (0..=100).map(|i| 1.0 + i as f32 * 0.001).chain((0..=100).rev().map(|i| 1.0 + i as f32 * 0.001)).collect();
        for z in steps {
            let now = wanted(on, vec2(1200.0, 800.0) * z, v);
            switches += (now != on) as u32;
            on = now;
        }
        assert_eq!(switches, 2);
        assert!(!on);
    }

    #[test]
    fn mini_keeps_shape_and_place() {
        for image in [vec2(6000.0, 4000.0), vec2(4000.0, 6000.0), vec2(3000.0, 3000.0), vec2(9000.0, 2000.0), vec2(2400.0, 1600.0)] {
            let m = mini_rect(view(), image, 0.0);
            assert!((m.size().max_elem() - LONG_EDGE).abs() < 1e-3, "{image:?}");
            assert!((m.aspect_ratio() - image.x / image.y).abs() < 1e-3, "{image:?}");
            // the plate around it keeps the distance from the corner
            let plate = m.expand(PLATE);
            assert!((view().right() - plate.right() - MARGIN).abs() < 1e-3);
            assert!((view().bottom() - plate.bottom() - MARGIN).abs() < 1e-3);
            assert!(view().contains_rect(plate));
        }
        // lifted above a badge
        let m = mini_rect(view(), vec2(6000.0, 4000.0), 40.0);
        assert!((view().bottom() - m.bottom() - MARGIN - PLATE - 40.0).abs() < 1e-3);
        // a small view: never more than a third of it
        let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 240.0));
        let m = mini_rect(small, vec2(6000.0, 4000.0), 0.0);
        assert!((m.width() - 80.0).abs() < 1e-3 && small.contains_rect(m));
    }

    #[test]
    fn viewport_matches_the_part_on_screen() {
        let image = vec2(6000.0, 4000.0);
        let mini = mini_rect(view(), image, 0.0);
        // centred: the middle fifth by width (1200 of 6000), a fifth by height (800 of 4000)
        let r = viewport(placed(image, Vec2::ZERO), view(), mini);
        assert!((r.center() - mini.center()).length() < 1e-3);
        assert!((r.width() / mini.width() - 0.2).abs() < 1e-4);
        assert!((r.height() / mini.height() - 0.2).abs() < 1e-4);
        // panned to the top left corner of the photo (pan positive = photo moves right/down)
        let pan = clamp_pan(vec2(1e6, 1e6), image, view().size());
        let r = viewport(placed(image, pan), view(), mini);
        assert!((r.min - mini.min).length() < 1e-3);
        // panned to the bottom right corner
        let r = viewport(placed(image, -pan), view(), mini);
        assert!((r.max - mini.max).length() < 1e-3);
        // portrait photo, taller than the view but narrower: the frame spans the whole width
        let image = vec2(700.0, 1050.0);
        let mini = mini_rect(view(), image, 0.0);
        let r = viewport(placed(image, Vec2::ZERO), view(), mini);
        assert!((r.width() - mini.width()).abs() < 1e-3);
        assert!((r.height() / mini.height() - 800.0 / 1050.0).abs() < 1e-4);
        // fitted: the frame is the whole picture
        let image = vec2(1200.0, 800.0);
        let mini = mini_rect(view(), image, 0.0);
        let r = viewport(placed(image, Vec2::ZERO), view(), mini);
        assert!((r.min - mini.min).length() < 1e-3 && (r.max - mini.max).length() < 1e-3);
    }

    #[test]
    fn click_centres_and_stays_inside_the_photo() {
        for (image, zooms) in [(vec2(1200.0, 800.0), [1.5f32, 2.0, 5.0, 12.0]), (vec2(533.0, 800.0), [1.5, 3.0, 5.0, 8.0]), (vec2(1200.0, 240.0), [2.0, 4.0, 6.0, 10.0])] {
            for z in zooms {
                let image = image * z;
                let mini = mini_rect(view(), image, 0.0);
                for u in [vec2(0.5, 0.5), vec2(0.3, 0.6), vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(1.0, 0.0), vec2(0.02, 0.97)] {
                    let p = mini.min + u * mini.size();
                    let pan = pan_for(p, mini, image, view().size(), Vec2::ZERO);
                    assert_eq!(pan, clamp_pan(pan, image, view().size()));
                    let r = viewport(placed(image, pan), view(), mini);
                    // the frame never leaves the picture …
                    assert!(mini.expand(1e-3).contains_rect(r), "{image:?} {u:?}");
                    // … and its middle is the clicked place, as far as the edges allow
                    let half = r.size() * 0.5;
                    let expect = pos2(p.x.clamp(mini.left() + half.x, (mini.right() - half.x).max(mini.left() + half.x)), p.y.clamp(mini.top() + half.y, (mini.bottom() - half.y).max(mini.top() + half.y)));
                    assert!((r.center() - expect).length() < 0.02, "{image:?} {u:?}: {:?} vs {expect:?}", r.center());
                }
                // outside the picture counts as its edge
                let far = pan_for(mini.min - vec2(50.0, 50.0), mini, image, view().size(), Vec2::ZERO);
                assert_eq!(far, pan_for(mini.min, mini, image, view().size(), Vec2::ZERO));
            }
        }
    }

    #[test]
    fn round_trip_frame_to_pan() {
        // the middle of the frame, clicked again, leaves the pan where it is
        let image = vec2(4800.0, 3200.0);
        let mini = mini_rect(view(), image, 0.0);
        for pan in [vec2(0.0, 0.0), vec2(700.0, -400.0), vec2(-1800.0, 1200.0), vec2(123.0, 45.0)] {
            let pan = clamp_pan(pan, image, view().size());
            let r = viewport(placed(image, pan), view(), mini);
            let back = pan_for(r.center(), mini, image, view().size(), Vec2::ZERO);
            assert!((back - pan).length() < 0.05, "{pan:?} → {back:?}");
        }
        // with an offset of the view (darkroom sheet) the pan compensates it
        let off = vec2(30.0, -20.0);
        let pan = pan_for(mini.center(), mini, image, view().size(), off);
        assert!((pan + off).length() < 1e-3);
    }

    #[test]
    fn pan_limits_match_the_view() {
        let v = view().size();
        assert_eq!(clamp_pan(vec2(50.0, 50.0), vec2(1200.0, 800.0), v), Vec2::ZERO);
        // only the axis that exceeds the view moves
        assert_eq!(clamp_pan(vec2(500.0, 500.0), vec2(1000.0, 1500.0), v), vec2(0.0, 350.0));
        assert_eq!(clamp_pan(vec2(-5000.0, 10.0), vec2(2400.0, 1600.0), v), vec2(-600.0, 10.0));
    }

    #[test]
    fn moves_above_a_badge_in_the_corner() {
        let image = vec2(2400.0, 1600.0);
        assert_eq!(lift_for(view(), image, None), 0.0);
        // a badge at the bottom right (12 pt from the corner, 28 pt high)
        let badge = Rect::from_min_max(view().right_bottom() - vec2(312.0, 40.0), view().right_bottom() - vec2(12.0, 12.0));
        let lift = lift_for(view(), image, Some(badge));
        let plate = mini_rect(view(), image, lift).expand(PLATE);
        assert!(!plate.intersects(badge) && plate.bottom() <= badge.top() - 5.0);
        // something elsewhere does not move it
        let far = Rect::from_min_size(view().left_top(), vec2(200.0, 30.0));
        assert_eq!(lift_for(view(), image, Some(far)), 0.0);
    }

    #[test]
    fn toggle_flips_the_switch() {
        let before = enabled();
        toggle();
        assert_eq!(enabled(), !before);
        toggle();
        assert_eq!(enabled(), before);
    }
}
