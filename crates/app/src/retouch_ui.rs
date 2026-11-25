//! Retouching controls in the "Retouch" tool: heal or clone spots, correct red eyes.
//! The data lives in the photo's settings (`spots`, `red_eyes`, computation core see `develop::spots`); coordinates are, as with
//! the masks, relative to the unrotated, uncropped image. The source of a spot is chosen by a simple method (to the side,
//! into the image).
//!
//! Behaviour in the image:
//! - The circles are visible while the "Retouch" tool of the right rail is open or a retouching tool is active, hidden
//!   otherwise; H (or the switch in the panel) inverts that everywhere (hide in the tool, show outside of it).
//! - A click on a circle selects the spot (highlighted, source with a line), dragging moves the spot or its source.
//!   Radius and opacity act on the selected spot, ⌫/Del deletes it, Esc ends the tool and the selection.
//! - Only with an active tool does a click on a free area place something new; dragging while placing sets the source.
//!
//! The circles are drawn by egui over the photo (also over its wide-gamut/HDR version, which egui draws as well).

use develop::spots::{RedEye, Spot};
use eframe::egui::{self, Color32, Context, Pos2, Ui};

use crate::lr;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Tool {
    #[default]
    Off,
    Heal,
    CloneStamp,
    RedEyes,
}

/// A selected element in the image.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Sel {
    Spot(usize),
    Eye(usize),
}

#[derive(Clone, Copy)]
struct State {
    image: usize,
    tool: Tool,
    /// Radius for new spots as a fraction of the image width
    radius: f32,
    /// running drag: element, its source (true) or the element itself (false), newly placed (true: source is dragged while placing)
    dragging: Option<(Sel, bool, bool)>,
    selected: Option<Sel>,
    /// "Delete all …" asks first
    confirm_all: bool,
}

impl Default for State {
    fn default() -> Self {
        Self { image: usize::MAX, tool: Tool::Off, radius: 0.01, dragging: None, selected: None, confirm_all: false }
    }
}

/// Visibility of the overlays (independent of the photo).
#[derive(Clone, Copy, Default)]
struct View {
    /// the "Retouch" tool of the right rail is open
    panel_open: bool,
    /// the "Masks" tool of the right rail is open (H then shows or hides the mask outlines, not the circles)
    masks_open: bool,
    /// H / switch: inverts the default (hide in the tool, show outside)
    toggled: bool,
    /// the pointer was over a circle in the last frame (then the image does not zoom or pan on click/drag)
    over_handle: bool,
}

fn key() -> egui::Id {
    egui::Id::unique("retusche_werkzeug")
}

fn view_key() -> egui::Id {
    egui::Id::unique("retouch_overlay_view")
}

fn state(ctx: &Context, image: usize) -> State {
    let mut z = ctx.data_mut(|d| d.get_temp::<State>(key()).unwrap_or_default());
    if z.image != image {
        z = State { image, radius: z.radius, ..Default::default() };
    }
    z
}

fn save(ctx: &Context, z: State) {
    ctx.data_mut(|d| d.insert_temp(key(), z));
}

fn view(ctx: &Context) -> View {
    ctx.data_mut(|d| d.get_temp::<View>(view_key()).unwrap_or_default())
}

fn save_view(ctx: &Context, v: View) {
    ctx.data_mut(|d| d.insert_temp(view_key(), v));
}

/// Every frame before the image is drawn: is the "Retouch" or the "Masks" tool open? Opening or closing one resets H.
pub fn set_panel_open(ctx: &Context, open: bool, masks: bool) {
    let mut v = view(ctx);
    if v.panel_open != open || v.masks_open != masks {
        v.panel_open = open;
        v.masks_open = masks;
        v.toggled = false;
    }
    save_view(ctx, v);
}

/// Is the "Masks" tool open (see [`set_panel_open`])?
pub fn masks_open(ctx: &Context) -> bool {
    view(ctx).masks_open
}

/// Are the outlines of the selected mask shown? In the "Masks" tool unless hidden with H; in the "Retouch" tool never;
/// elsewhere only when shown with H.
pub fn mask_outlines_visible(ctx: &Context) -> bool {
    let v = view(ctx);
    if v.masks_open {
        !v.toggled
    } else {
        !v.panel_open && v.toggled
    }
}

/// Key H / switch: show or hide retouch spots and mask outlines.
pub fn toggle_overlays(ctx: &Context) {
    let mut v = view(ctx);
    v.toggled = !v.toggled;
    save_view(ctx, v);
}

/// Are retouch spots and mask outlines shown right now?
pub fn overlays_visible(ctx: &Context) -> bool {
    let v = view(ctx);
    active(ctx) || (!v.masks_open && v.panel_open != v.toggled)
}

/// Is a retouching tool active (then clicks in the image belong to it)?
pub fn active(ctx: &Context) -> bool {
    ctx.data_mut(|d| d.get_temp::<State>(key()).is_some_and(|z| z.tool != Tool::Off))
}

/// Is a retouch spot or red eye selected (then ⌫ and Esc belong to it, not to the photo)?
pub fn has_selection(ctx: &Context) -> bool {
    overlays_visible(ctx) && ctx.data_mut(|d| d.get_temp::<State>(key()).is_some_and(|z| z.selected.is_some()))
}

/// Does a circle take the pointer (hovered in the last frame or being dragged)? Then the image neither zooms nor pans.
pub fn grabs_pointer(ctx: &Context) -> bool {
    overlays_visible(ctx) && (view(ctx).over_handle || ctx.data_mut(|d| d.get_temp::<State>(key()).is_some_and(|z| z.dragging.is_some())))
}

/// Source for a new spot at `p` (normalised): beside it at a distance of three radii, towards the side with more room.
fn source_for(p: [f32; 2], radius: f32) -> [f32; 2] {
    let d = 3.0 * radius;
    let x = if p[0] + d < 0.97 { p[0] + d } else { p[0] - d };
    [x.clamp(0.03, 0.97), p[1].clamp(0.03, 0.97)]
}

/// Selection still valid for these lists?
fn valid(sel: Option<Sel>, spots: &[Spot], red_eyes: &[RedEye]) -> Option<Sel> {
    sel.filter(|s| match *s {
        Sel::Spot(i) => i < spots.len(),
        Sel::Eye(i) => i < red_eyes.len(),
    })
}

fn delete(sel: Sel, spots: &mut Vec<Spot>, red_eyes: &mut Vec<RedEye>) {
    match sel {
        Sel::Spot(i) if i < spots.len() => {
            spots.remove(i);
        }
        Sel::Eye(i) if i < red_eyes.len() => {
            red_eyes.remove(i);
        }
        _ => {}
    }
}

/// Section "Retouch" of the "Retouch" tool.
pub fn panel(ui: &mut Ui, spots: &mut Vec<Spot>, red_eyes: &mut Vec<RedEye>, index: usize) {
    let mut z = state(ui.ctx(), index);
    // test hook GRAINEO_RETOUCH_TEST=1 (only with GRAINEO_DATA_DIR): three spots, the second selected
    if spots.is_empty() && crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_RETOUCH_TEST").as_deref() == Ok("1") {
        for (x, y) in [(0.3, 0.4), (0.5, 0.5), (0.7, 0.3)] {
            spots.push(Spot { clone: false, center: [x, y], source: [x + 0.08, y + 0.05], radius: 0.02, opacity: 1.0 });
        }
        z.selected = Some(Sel::Spot(1));
    }
    z.selected = valid(z.selected, spots, red_eyes);
    {
        // heading with the help card on the right (fixed place)
        lr::category_with(ui, "Flecken & rote Augen", 20.0, |ui| crate::help::info(ui, "retusche", "Retusche", &crate::help::RETOUCH));
        // the three tools in one row of equal buttons
        ui.columns(3, |cols| {
            for (c, (w, name, tip)) in [
                (Tool::Heal, "Heilen", "Fleck anklicken: Struktur von einer Quelle daneben, an die Umgebung angepasst"),
                (Tool::CloneStamp, "Klonen", "Fleck anklicken: kopiert die Quelle genau"),
                (Tool::RedEyes, "Rote Augen", "Auf die Pupille klicken"),
            ]
            .into_iter()
            .enumerate()
            {
                let on = z.tool == w;
                let r = cols[c].add_sized([cols[c].available_width(), 24.0], egui::Button::new(crate::tr(name)).selected(on));
                lr::hover_tip(&cols[c], &r, crate::tr(tip));
                if r.clicked() {
                    z.tool = if on { Tool::Off } else { w };
                }
            }
        });
        ui.add_space(4.0);
        // switch with a clear state; with an active tool the circles are always visible; the key is in the tooltip
        ui.horizontal(|ui| {
            let visible = overlays_visible(ui.ctx());
            let label = if visible { crate::tr("Punkte sichtbar") } else { crate::tr("Punkte verborgen") };
            let r = ui.add_enabled(z.tool == Tool::Off, egui::Button::new(label).selected(visible));
            lr::tip_key(ui, &r, crate::tr("Retusche-Punkte im Bild zeigen oder verbergen"), "overlays");
            if r.clicked() {
                toggle_overlays(ui.ctx());
            }
        });
        // what is there and what is selected (fixed line)
        let mut line = if spots.is_empty() && red_eyes.is_empty() {
            crate::tr("Keine Retusche-Punkte").to_string()
        } else if red_eyes.is_empty() {
            crate::trf!("{a} Retusche-Punkte", a = spots.len())
        } else {
            crate::trf!("{a} Retusche-Punkte · {b} rote Augen", a = spots.len(), b = red_eyes.len())
        };
        match z.selected {
            Some(Sel::Spot(i)) => line.push_str(&crate::trf!(" · Nr. {a} gewählt", a = i + 1)),
            Some(Sel::Eye(i)) => line.push_str(&crate::trf!(" · Auge {a} gewählt", a = i + 1)),
            None => {}
        }
        lr::note(ui, &line);
        // Radius: of the selected element, otherwise of new spots (with an active tool); always in the same place
        let editable = z.selected.is_some() || z.tool != Tool::Off;
        ui.add_enabled_ui(editable, |ui| {
            let mut r = match z.selected {
                Some(Sel::Spot(i)) => spots[i].radius,
                Some(Sel::Eye(i)) => red_eyes[i].size[0].max(red_eyes[i].size[1]),
                None => z.radius,
            } as f64;
            if lr::slider_plain(ui, "Radius", &mut r, 0.002..=0.06, 0.01, 0.001) {
                let r = r as f32;
                match z.selected {
                    Some(Sel::Spot(i)) => spots[i].radius = r,
                    Some(Sel::Eye(i)) => red_eyes[i].size = [r, r],
                    None => {}
                }
                z.radius = r;
            }
        });
        let spot = match z.selected {
            Some(Sel::Spot(i)) => Some(i),
            _ => None,
        };
        ui.add_enabled_ui(spot.is_some(), |ui| {
            let mut o = spot.map_or(1.0, |i| spots[i].opacity) as f64 * 100.0;
            if lr::slider_plain(ui, "Deckkraft", &mut o, 0.0..=100.0, 100.0, 1.0) {
                if let Some(i) = spot {
                    spots[i].opacity = (o / 100.0).clamp(0.0, 1.0) as f32;
                }
            }
        });
        ui.horizontal(|ui| {
            if z.confirm_all {
                ui.add_space(lr::TEXT_INSET);
                ui.label(egui::RichText::new(crate::trf!("Alle {a} löschen?", a = spots.len() + red_eyes.len())).small());
                if ui.button(crate::tr("Löschen")).clicked() {
                    spots.clear();
                    red_eyes.clear();
                    z.selected = None;
                    z.confirm_all = false;
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    z.confirm_all = false;
                }
            } else {
                let r = ui.add_enabled(z.selected.is_some(), egui::Button::new(crate::tr("Gewählten löschen")));
                lr::hover_tip(ui, &r, &format!("{} (⌫)", crate::tr("Gewählten Punkt löschen")));
                if r.clicked() {
                    if let Some(s) = z.selected.take() {
                        delete(s, spots, red_eyes);
                    }
                }
                if ui.add_enabled(!spots.is_empty() || !red_eyes.is_empty(), egui::Button::new(crate::tr("Alle löschen …"))).clicked() {
                    z.confirm_all = true;
                }
            }
        });
    }
    save(ui.ctx(), z);
}

/// In the image: show the circles, select, move and delete them; with an active tool place new spots/pupils.
pub fn overlay(ui: &Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, frame: &crate::Frame, full: [f32; 2], spots: &mut Vec<Spot>, red_eyes: &mut Vec<RedEye>, index: usize) {
    let ctx = ui.ctx().clone();
    let mut z = state(&ctx, index);
    let mut v = view(&ctx);
    z.selected = valid(z.selected, spots, red_eyes);
    if !overlays_visible(&ctx) {
        // hidden circles cannot stay selected (⌫ would otherwise not reach the photo, the panel would name an invisible spot)
        z.selected = None;
        z.confirm_all = false;
        z.dragging = None;
        v.over_handle = false;
        save(&ctx, z);
        save_view(&ctx, v);
        return;
    }
    let to_screen = |p: [f32; 2]| -> Pos2 {
        let u = frame.from_src([p[0] * full[0], p[1] * full[1]]);
        rect.min + egui::vec2(u[0], u[1]) * rect.size()
    };
    let to_image = |p: Pos2| -> [f32; 2] {
        let u = (p - rect.min) / rect.size();
        let q = frame.to_src([u.x, u.y]);
        [(q[0] / full[0]).clamp(0.0, 1.0), (q[1] / full[1]).clamp(0.0, 1.0)]
    };
    // Scale: image width in screen points (unrotated image: width corresponds to full[0])
    let points_per_width = (to_screen([1.0, 0.5]) - to_screen([0.0, 0.5])).length();
    // Keys: Esc ends tool and selection, ⌫/Del deletes the selected element
    if !ctx.egui_wants_keyboard_input() {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            z.tool = Tool::Off;
            z.selected = None;
            z.dragging = None;
        }
        if ui.input(|i| (i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)) && !i.modifiers.command) {
            if let Some(s) = z.selected.take() {
                delete(s, spots, red_eyes);
                z.dragging = None;
            }
        }
    }
    // Element under the pointer: sources first, then spots, then pupils (the last placed is on top)
    let handle_at = |p: Pos2, spots: &[Spot], red_eyes: &[RedEye]| -> Option<(Sel, bool)> {
        let near = |m: Pos2, r: f32| m.distance(p) <= r.max(6.0) + 3.0;
        let r = |sp: &Spot| sp.radius * points_per_width;
        (0..spots.len())
            .rev()
            .find(|&i| near(to_screen(spots[i].source), r(&spots[i])))
            .map(|i| (Sel::Spot(i), true))
            .or_else(|| (0..spots.len()).rev().find(|&i| near(to_screen(spots[i].center), r(&spots[i]))).map(|i| (Sel::Spot(i), false)))
            .or_else(|| (0..red_eyes.len()).rev().find(|&i| near(to_screen(red_eyes[i].center), red_eyes[i].size[0].max(red_eyes[i].size[1]) * points_per_width)).map(|i| (Sel::Eye(i), false)))
    };
    let hover = resp.hover_pos().filter(|p| rect.contains(*p) && ui.clip_rect().contains(*p));
    // Drag: move an element or its source; with a heal/clone tool on a free area place a new spot and drag its source
    if resp.drag_started() {
        // where the button went down (the drag only starts after a few points of movement)
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        if let Some(pos) = origin.filter(|p| rect.contains(*p)) {
            z.dragging = match handle_at(pos, spots, red_eyes) {
                Some((s, is_source)) => Some((s, is_source, false)),
                None if matches!(z.tool, Tool::Heal | Tool::CloneStamp) && spots.len() < 256 => {
                    let p = to_image(pos);
                    spots.push(Spot { clone: z.tool == Tool::CloneStamp, center: p, source: p, radius: z.radius, opacity: 1.0 });
                    Some((Sel::Spot(spots.len() - 1), true, true))
                }
                None => None,
            };
            if let Some((s, _, _)) = z.dragging {
                z.selected = Some(s);
            }
        }
    }
    if let (Some((s, is_source, _)), Some(pos)) = (z.dragging, resp.interact_pointer_pos()) {
        let p = to_image(pos);
        match s {
            Sel::Spot(i) => {
                if let Some(sp) = spots.get_mut(i) {
                    if is_source {
                        sp.source = p;
                    } else {
                        // move the spot, the source moves along
                        let d = [p[0] - sp.center[0], p[1] - sp.center[1]];
                        sp.center = p;
                        sp.source = [(sp.source[0] + d[0]).clamp(0.0, 1.0), (sp.source[1] + d[1]).clamp(0.0, 1.0)];
                    }
                }
            }
            Sel::Eye(i) => {
                if let Some(e) = red_eyes.get_mut(i) {
                    e.center = p;
                }
            }
        }
    }
    if resp.drag_stopped() {
        // newly placed but barely dragged: source automatically beside it
        if let Some((Sel::Spot(i), _, true)) = z.dragging {
            if let Some(sp) = spots.get_mut(i) {
                let d = ((sp.source[0] - sp.center[0]) * points_per_width).hypot((sp.source[1] - sp.center[1]) * points_per_width * full[1] / full[0]);
                if d < sp.radius * points_per_width {
                    sp.source = source_for(sp.center, sp.radius);
                }
            }
        }
        z.dragging = None;
    }
    // Click: select the element under the pointer; with a tool on a free area place something new; otherwise deselect
    if resp.clicked() && z.dragging.is_none() {
        if let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p) && ui.clip_rect().contains(*p)) {
            match handle_at(pos, spots, red_eyes) {
                Some((s, _)) => z.selected = Some(s),
                None => {
                    let p = to_image(pos);
                    match z.tool {
                        Tool::RedEyes if red_eyes.len() < 64 => {
                            red_eyes.push(RedEye { center: p, size: [z.radius, z.radius], angle: 0.0, density: 0.9, darken: 0.5 });
                            z.selected = Some(Sel::Eye(red_eyes.len() - 1));
                        }
                        Tool::Heal | Tool::CloneStamp if spots.len() < 256 => {
                            spots.push(Spot { clone: z.tool == Tool::CloneStamp, center: p, source: source_for(p, z.radius), radius: z.radius, opacity: 1.0 });
                            z.selected = Some(Sel::Spot(spots.len() - 1));
                        }
                        _ => z.selected = None,
                    }
                }
            }
        }
    }
    let hovered = hover.and_then(|p| handle_at(p, spots, red_eyes)).map(|(s, _)| s);
    // Drawing: selected element white and thicker (source with a line), the hovered one brighter
    let gold = Color32::from_rgb(255, 230, 120);
    for (i, sp) in spots.iter().enumerate() {
        let c = to_screen(sp.center);
        let q = to_screen(sp.source);
        let r = (sp.radius * points_per_width).max(3.0);
        let sel = z.selected == Some(Sel::Spot(i));
        let hot = hovered == Some(Sel::Spot(i));
        let (main, width) = if sel { (crate::lr::TEXT_STRONG(), 2.5) } else if hot { (crate::lr::TEXT_STRONG(), 1.5) } else { (gold, 1.5) };
        painter.circle_stroke(c, r + 1.0, egui::Stroke::new(width + 2.0, Color32::from_black_alpha(90)));
        painter.circle_stroke(c, r, egui::Stroke::new(width, main));
        let faint = if sel { crate::lr::overlay(200) } else { Color32::from_rgba_unmultiplied(255, 230, 120, 140) };
        painter.circle_stroke(q, r, egui::Stroke::new(if sel { 1.5 } else { 1.0 }, faint));
        painter.line_segment([c, q], egui::Stroke::new(1.0, if sel { crate::lr::overlay(170) } else { Color32::from_rgba_unmultiplied(255, 230, 120, 110) }));
    }
    for (i, e) in red_eyes.iter().enumerate() {
        let c = to_screen(e.center);
        let r = (e.size[0].max(e.size[1]) * points_per_width).max(4.0);
        let sel = z.selected == Some(Sel::Eye(i));
        let hot = hovered == Some(Sel::Eye(i));
        let (col, width) = if sel { (crate::lr::TEXT_STRONG(), 2.5) } else if hot { (Color32::from_rgb(255, 170, 170), 1.5) } else { (Color32::from_rgb(255, 120, 120), 1.5) };
        painter.circle_stroke(c, r + 1.0, egui::Stroke::new(width + 2.0, Color32::from_black_alpha(90)));
        painter.circle_stroke(c, r, egui::Stroke::new(width, col));
    }
    // Pointer: crosshair and outline of the new spot only while placing; over a circle the normal arrow
    if z.dragging.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if z.tool != Tool::Off && hovered.is_none() {
        if let Some(pos) = hover {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            painter.circle_stroke(pos, (z.radius * points_per_width).max(3.0), egui::Stroke::new(1.0, crate::lr::TEXT_STRONG()));
        }
    }
    v.over_handle = hovered.is_some() || z.dragging.is_some();
    save(&ctx, z);
    save_view(&ctx, v);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ctx: &Context, events: Vec<egui::Event>, spots: &mut Vec<Spot>, eyes: &mut Vec<RedEye>) {
        let frame = crate::Frame::from_settings(&develop::Settings::default(), 200, 100, 0);
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), events, ..Default::default() };
        let _ = crate::ui_test::run(ctx, raw, |ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                overlay(ui, &resp, ui.painter(), rect, &frame, [200.0, 100.0], spots, eyes, 0);
            });
        });
    }

    fn click(ctx: &Context, p: Pos2, spots: &mut Vec<Spot>, eyes: &mut Vec<RedEye>) {
        let b = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        run(ctx, vec![egui::Event::PointerMoved(p), b(true)], spots, eyes);
        run(ctx, vec![b(false)], spots, eyes);
    }

    fn press(ctx: &Context, k: egui::Key, spots: &mut Vec<Spot>, eyes: &mut Vec<RedEye>) {
        run(ctx, vec![egui::Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], spots, eyes);
    }

    fn spot(x: f32, y: f32) -> Spot {
        Spot { clone: false, center: [x, y], source: [x + 0.2, y], radius: 0.02, opacity: 1.0 }
    }

    #[test]
    fn visibility_follows_panel_and_h() {
        let ctx = Context::default();
        assert!(!overlays_visible(&ctx), "outside the Retouch tool hidden");
        assert!(!mask_outlines_visible(&ctx));
        toggle_overlays(&ctx);
        assert!(overlays_visible(&ctx), "H shows them outside");
        assert!(mask_outlines_visible(&ctx), "H shows the mask outlines outside, too");
        set_panel_open(&ctx, true, false);
        assert!(overlays_visible(&ctx), "opening the tool: visible, H reset");
        assert!(!mask_outlines_visible(&ctx), "no mask outlines in the Retouch tool");
        toggle_overlays(&ctx);
        assert!(!overlays_visible(&ctx), "H hides them in the tool");
        set_panel_open(&ctx, false, false);
        assert!(!overlays_visible(&ctx));
        // the Masks tool: outlines, no retouch spots; H hides the outlines
        set_panel_open(&ctx, false, true);
        assert!(mask_outlines_visible(&ctx) && !overlays_visible(&ctx));
        toggle_overlays(&ctx);
        assert!(!mask_outlines_visible(&ctx) && !overlays_visible(&ctx));
    }

    #[test]
    fn select_move_delete_without_tool() {
        let ctx = Context::default();
        set_panel_open(&ctx, true, false);
        let (mut spots, mut eyes) = (vec![spot(0.25, 0.5), spot(0.5, 0.2)], Vec::new());
        run(&ctx, Vec::new(), &mut spots, &mut eyes);
        // a click on a free area places nothing without a tool
        click(&ctx, egui::pos2(190.0, 100.0), &mut spots, &mut eyes);
        assert_eq!(spots.len(), 2);
        assert!(!has_selection(&ctx));
        // click on the first spot (image 0.25/0.5 → screen 60/60): selected
        click(&ctx, egui::pos2(60.0, 60.0), &mut spots, &mut eyes);
        assert_eq!(state(&ctx, 0).selected, Some(Sel::Spot(0)));
        assert!(has_selection(&ctx) && grabs_pointer(&ctx));
        // drag the spot: the source moves along
        let b = |p, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        run(&ctx, vec![egui::Event::PointerMoved(egui::pos2(60.0, 60.0)), b(egui::pos2(60.0, 60.0), true)], &mut spots, &mut eyes);
        run(&ctx, vec![egui::Event::PointerMoved(egui::pos2(70.0, 70.0))], &mut spots, &mut eyes);
        run(&ctx, vec![egui::Event::PointerMoved(egui::pos2(80.0, 80.0))], &mut spots, &mut eyes);
        run(&ctx, vec![b(egui::pos2(80.0, 80.0), false)], &mut spots, &mut eyes);
        assert!((spots[0].center[0] - 0.35).abs() < 0.01 && (spots[0].center[1] - 0.7).abs() < 0.01, "{:?}", spots[0].center);
        assert!((spots[0].source[0] - 0.55).abs() < 0.01, "{:?}", spots[0].source);
        // ⌫ deletes only the selected one
        press(&ctx, egui::Key::Backspace, &mut spots, &mut eyes);
        assert_eq!(spots.len(), 1);
        assert_eq!(spots[0].center, [0.5, 0.2]);
        assert!(!has_selection(&ctx));
    }

    #[test]
    fn tool_places_and_esc_ends() {
        let ctx = Context::default();
        save(&ctx, State { image: 0, tool: Tool::Heal, ..Default::default() });
        let (mut spots, mut eyes) = (Vec::new(), Vec::new());
        run(&ctx, Vec::new(), &mut spots, &mut eyes);
        assert!(overlays_visible(&ctx), "an active tool always shows the circles");
        click(&ctx, egui::pos2(60.0, 60.0), &mut spots, &mut eyes);
        assert_eq!(spots.len(), 1);
        assert_eq!(state(&ctx, 0).selected, Some(Sel::Spot(0)));
        press(&ctx, egui::Key::Escape, &mut spots, &mut eyes);
        assert!(!active(&ctx));
        assert_eq!(state(&ctx, 0).selected, None);
        assert!(!overlays_visible(&ctx), "outside the tool hidden again");
        // hidden circles cannot be hit
        click(&ctx, egui::pos2(60.0, 60.0), &mut spots, &mut eyes);
        assert_eq!(state(&ctx, 0).selected, None);
    }
}
