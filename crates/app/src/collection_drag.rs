//! The collection list of the left panel and dragging in it: collections, sets and smart collections are dragged by
//! their whole row (no handle) into an own order within a level, into a set or out of it.
//!
//! - Own payload [`CollectionDrag`], separate from photos ([`PhotoDrag`]) and from files dropped from the Finder (those
//!   come as raw input, not as a payload), so none of them is ever taken for another.
//! - The list does not move while dragging: the source row is only dimmed, a grey insertion line (between rows) or a
//!   frame (middle of a set row: "into this set") shows the drop; blocked targets are dimmed with the "no drop" pointer.
//! - A click stays a click (egui's drag threshold); Esc cancels (egui drops the payload and takes the key); releasing
//!   on the same place or outside a target does nothing.
//! - Near the top edge (below the header that sticks there) or the bottom edge the panel scrolls; resting 0.7 s on a
//!   closed set opens it – both also while photos are dragged, so collections in closed sets can be reached.
//!
//! The hit test ([`hit`]) and the check of a drop ([`verdict`]) are pure functions on the rows drawn in this frame.

use super::*;
use library::collection_order::{self as order, Blocked};

/// Collection dragged in the left panel (egui drag and drop payload).
#[derive(Clone, Debug)]
pub struct CollectionDrag {
    pub id: i64,
    pub kind: u8,
    pub name: String,
}

/// One row of the collection list as drawn this frame.
#[derive(Clone, Copy, Debug)]
pub(super) struct Row {
    pub id: i64,
    pub kind: u8,
    pub parent: Option<i64>,
    pub depth: usize,
    /// a set whose content is shown below it
    pub open: bool,
    pub rect: egui::Rect,
}

/// What releasing at the pointer would do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Hit {
    /// into the level `parent` before `before` (`None`: at its end); the line is drawn at `y`, indented for `depth`
    Line { parent: Option<i64>, before: Option<i64>, y: f32, depth: usize },
    /// into this set, at its end (frame around the set's row)
    Into { set: i64 },
}

const SET: u8 = 1;
/// resting this long on a closed set opens it (seconds)
const REST: f64 = 0.7;
/// height of the zones at the top and bottom edge that scroll the panel
const EDGE: f32 = 24.0;
/// height of the section header that sticks at the top of the panel (`lr::section`, `sticky`)
const HEAD: f32 = 28.0;
/// how long a row stays highlighted after it was placed (seconds)
const FLASH: f32 = 0.6;

#[derive(Default)]
pub(super) struct DragState {
    rows: Vec<Row>,
    /// closed set the pointer rests on while dragging: (id, since)
    rest: Option<(i64, f64)>,
    /// row highlighted briefly after placing it: (id, since)
    flash: Option<(i64, std::time::Instant)>,
    /// row the panel scrolls to once (a new collection)
    pub scroll_to: Option<i64>,
    /// top of the panel's content this frame (below the top edge of the view: scrolled)
    pub content_top: f32,
    /// test aid (screenshots): a drag held still over a row – (dragged, target row, height in the row 0…1)
    pub test: Option<(i64, i64, f32)>,
}

/// Test aid `GRAINEO_LIBRARY_TEST="drag=Source>Target:0.5"`: the collections by name, the height within the target row.
pub(super) fn test_drag(list: &[Collection], spec: &str) -> Option<(i64, i64, f32)> {
    let (from, rest) = spec.split_once('>')?;
    let (to, f) = rest.rsplit_once(':').unwrap_or((rest, "0.5"));
    let id = |n: &str| list.iter().find(|c| c.name == n.trim()).map(|c| c.id);
    Some((id(from)?, id(to)?, f.trim().parse().ok()?))
}

impl DragState {
    /// Highlight a row briefly (placed, created).
    pub fn flash(&mut self, id: i64) {
        self.flash = Some((id, std::time::Instant::now()));
    }
}

/// The next member of the level of `rows[k]` below it (skipping what lies inside it), `None` at the end of the level.
fn next_sibling(rows: &[Row], k: usize) -> Option<i64> {
    let d = rows[k].depth;
    rows[k + 1..].iter().find(|n| n.depth <= d).filter(|n| n.depth == d).map(|n| n.id)
}

/// Which drop the pointer at height `y` means. Plain and smart collections: upper half = before, lower half = after.
/// Sets: upper quarter = before, middle half = into, lower quarter = after (an open set: first place inside it). Below
/// the last row down to `end`: end of the top level. Above the first row: nothing.
pub(super) fn hit(rows: &[Row], y: f32, end: f32) -> Option<Hit> {
    if y < rows.first()?.rect.top() {
        return None;
    }
    // the first row reaching below the pointer (a gap between rows counts as the top of the row after it)
    let Some(k) = rows.iter().position(|r| y < r.rect.bottom()) else {
        let last = rows.last()?;
        return (y < end).then_some(Hit::Line { parent: None, before: None, y: last.rect.bottom(), depth: 0 });
    };
    let r = rows[k];
    let f = ((y - r.rect.top()) / r.rect.height().max(1.0)).max(0.0);
    let before = Hit::Line { parent: r.parent, before: Some(r.id), y: r.rect.top(), depth: r.depth };
    let after = Hit::Line { parent: r.parent, before: next_sibling(rows, k), y: r.rect.bottom(), depth: r.depth };
    if r.kind != SET {
        return Some(if f < 0.5 { before } else { after });
    }
    Some(if f < 0.25 {
        before
    } else if f < 0.75 {
        Hit::Into { set: r.id }
    } else if r.open {
        let first = rows.get(k + 1).filter(|n| n.depth > r.depth).map(|n| n.id);
        Hit::Line { parent: Some(r.id), before: first, y: r.rect.bottom(), depth: r.depth + 1 }
    } else {
        after
    })
}

/// Is the drop allowed, and does it change anything? `Ok(false)`: the same place (nothing shown, nothing done).
pub(super) fn verdict(list: &[Collection], rows: &[Row], dragged: i64, h: Hit) -> Result<bool, Blocked> {
    let parent = match h {
        Hit::Line { parent, .. } => parent,
        Hit::Into { set } => Some(set),
    };
    order::check(list, dragged, parent)?;
    let current = list.iter().find(|c| c.id == dragged).and_then(|c| c.parent);
    if let Hit::Line { parent, before, .. } = h {
        if parent == current {
            let own = rows.iter().position(|r| r.id == dragged);
            if before == Some(dragged) || own.is_some_and(|k| next_sibling(rows, k) == before) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

impl LibraryView {
    /// The content of the section SAMMLUNGEN: the rows in their order, then what dragging over them means.
    pub(super) fn collection_list(&mut self, ui: &mut egui::Ui) {
        self.collection_drag.rows.clear();
        let top: Vec<usize> = self.collection_tree.children(None).to_vec();
        for i in top {
            if self.collections.get(i).is_some_and(|c| Some(c.id) != self.quick_id) {
                self.collection_node(ui, i, 0);
            }
        }
        let end = ui.clip_rect().bottom();
        self.collection_drag_frame(ui, end);
    }

    pub(super) fn collection_node(&mut self, ui: &mut egui::Ui, i: usize, depth: usize) {
        let s = self.collections[i].clone();
        let open = self.open_collections.contains(&s.id);
        let active = self.source_marked(&Source::Collection(s.id)) || self.source_marked(&Source::Smart(s.id));
        let count = match s.kind {
            1 => None,
            2 => self.smart_count(s.id),
            _ => Some(s.count),
        };
        let name = if s.duplicates { crate::trf!("{a} → Duplikate", a = s.name) } else if s.kind == 2 { format!("{} ⚙", s.name) } else { s.name.clone() };
        let (click, triangle, resp) =
            Self::list_row_sense(ui, egui::Sense::click_and_drag(), None, &name, count.map(thousands), active, depth, (s.kind == 1).then_some(open), s.kind == 2 && !s.translatable && !s.duplicates);
        self.collection_drag.rows.push(Row { id: s.id, kind: s.kind, parent: s.parent, depth, open: s.kind == 1 && open, rect: resp.rect });
        // the whole row drags (no handle); never while a backup is viewed
        if !Self::ordering_locked() {
            resp.dnd_set_drag_payload(CollectionDrag { id: s.id, kind: s.kind, name: name.clone() });
        }
        if let Some((id, since)) = self.collection_drag.flash {
            let t = since.elapsed().as_secs_f32();
            if id == s.id && t < FLASH {
                ui.painter().rect_filled(resp.rect, 0.0, crate::lr::overlay((28.0 * (1.0 - t / FLASH)) as u8));
                ui.ctx().request_repaint();
            }
        }
        if self.collection_drag.scroll_to == Some(s.id) {
            resp.scroll_to_me(Some(egui::Align::Center));
            self.collection_drag.scroll_to = None;
        }
        if !s.duplicates {
            resp.context_menu(|ui| self.collection_menu(ui, &s));
        }
        // photos dragged here: only plain collections take them (sets, smart collections and the albums of Apple Photos
        // visibly blocked: the next sync would overwrite them)
        if s.apple && resp.dnd_hover_payload::<PhotoDrag>().is_some() {
            self.manage_message = Some(crate::tr(library::manage::APPLE_ALBUM).to_string());
        }
        if let Some(images) = Self::drop_target(ui, &resp, s.kind == 0 && !s.duplicates && !s.apple) {
            self.drop_on_collection(s.id, images);
        }
        if triangle || (click && s.kind == 1) {
            if open {
                self.open_collections.remove(&s.id);
            } else {
                self.open_collections.insert(s.id);
            }
        } else if click && s.duplicates {
            self.show_duplicates();
        } else if click {
            self.set_source(if s.kind == 2 { Source::Smart(s.id) } else { Source::Collection(s.id) });
        }
        if s.kind == 1 && self.open_collections.contains(&s.id) {
            let children: Vec<usize> = self.collection_tree.children(Some(s.id)).to_vec();
            for c in children {
                self.collection_node(ui, c, depth + 1);
            }
        }
    }

    /// While something is dragged over the list: scroll at the edges, open a closed set after resting on it; for a
    /// dragged collection additionally the line/frame of the drop, blocked targets, the drag image and the drop itself.
    fn collection_drag_frame(&mut self, ui: &mut egui::Ui, end: f32) {
        let ctx = ui.ctx().clone();
        // test aid: the drag held still over a row (never released)
        let held = self.collection_drag.test.and_then(|(id, target, f)| {
            let c = self.collections.iter().find(|c| c.id == id)?;
            let r = self.collection_drag.rows.iter().find(|r| r.id == target)?;
            egui::DragAndDrop::set_payload(&ctx, CollectionDrag { id, kind: c.kind, name: c.name.clone() });
            Some(egui::pos2(r.rect.left() + r.rect.width() * 0.6, r.rect.top() + r.rect.height() * f.clamp(0.0, 0.99)))
        });
        let dragged = egui::DragAndDrop::payload::<CollectionDrag>(&ctx);
        if dragged.is_none() && !egui::DragAndDrop::has_payload_of_type::<PhotoDrag>(&ctx) {
            self.collection_drag.rest = None;
            return;
        }
        let Some(pos) = held.or_else(|| ctx.pointer_latest_pos()) else { return };
        let clip = ui.clip_rect();
        // scrolled: a section header sticks at the top and covers the rows below it
        let scrolled = self.collection_drag.content_top < clip.top() - 0.5;
        let top = clip.top() + if scrolled { HEAD } else { 0.0 };
        let over_panel = clip.x_range().contains(pos.x);
        // edges scroll (faster the closer to the edge); also from the sticky header and just below the panel
        if over_panel {
            let dt = ctx.input(|i| i.stable_dt).min(0.05);
            let speed = 700.0 * dt;
            let up = (top + EDGE - pos.y) / EDGE;
            let down = (pos.y - (clip.bottom() - EDGE)) / EDGE;
            if pos.y >= clip.top() - 4.0 && up > 0.0 && scrolled {
                ui.scroll_with_delta(egui::vec2(0.0, speed * up.min(1.5)));
                ctx.request_repaint();
            } else if pos.y <= clip.bottom() + 12.0 && down > 0.0 {
                ui.scroll_with_delta(egui::vec2(0.0, -speed * down.min(1.5)));
                ctx.request_repaint();
            }
        }
        let inside = over_panel && pos.y >= top && pos.y < clip.bottom();
        let rows = std::mem::take(&mut self.collection_drag.rows);
        let under = rows.iter().find(|r| inside && r.rect.contains(pos)).copied();
        // resting on a closed set opens it (not the set being dragged itself)
        match under.filter(|r| r.kind == SET && !r.open && dragged.as_ref().is_none_or(|d| d.id != r.id)) {
            Some(r) => {
                let now = ctx.input(|i| i.time);
                match self.collection_drag.rest {
                    Some((id, since)) if id == r.id => {
                        if now - since >= REST {
                            self.open_collections.insert(r.id);
                            self.collection_drag.rest = None;
                        } else {
                            ctx.request_repaint_after(std::time::Duration::from_secs_f64(REST - (now - since) + 0.01));
                        }
                    }
                    _ => {
                        self.collection_drag.rest = Some((r.id, now));
                        ctx.request_repaint_after(std::time::Duration::from_secs_f64(REST + 0.01));
                    }
                }
            }
            None => self.collection_drag.rest = None,
        }
        let Some(d) = dragged else {
            self.collection_drag.rows = rows;
            return;
        };
        let painter = ui.painter().clone();
        if let Some(src) = rows.iter().find(|r| r.id == d.id) {
            painter.rect_filled(src.rect, 0.0, Color32::from_black_alpha(120));
        }
        let h = if inside { hit(&rows, pos.y, end) } else { None };
        let judged = h.map(|h| (h, verdict(&self.collections, &rows, d.id, h)));
        let released = held.is_none() && ctx.input(|i| i.pointer.any_released());
        let left = rows.first().map_or(clip.left(), |r| r.rect.left());
        let right = rows.first().map_or(clip.right(), |r| r.rect.right());
        match judged {
            Some((h, Ok(true))) => {
                match h {
                    Hit::Line { y, depth, .. } => {
                        let x0 = left + 4.0 + depth as f32 * 12.0 + 8.0;
                        painter.line_segment([egui::pos2(x0, y), egui::pos2(right - 4.0, y)], egui::Stroke::new(2.0, crate::lr::gray(0xc8)));
                        painter.circle_stroke(egui::pos2(x0, y), 2.5, egui::Stroke::new(1.5, crate::lr::gray(0xc8)));
                    }
                    Hit::Into { set } => {
                        if let Some(r) = rows.iter().find(|r| r.id == set) {
                            painter.rect_stroke(r.rect, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0xe0)), egui::StrokeKind::Inside);
                        }
                    }
                }
                if released && egui::DragAndDrop::take_payload::<CollectionDrag>(&ctx).is_some() {
                    match h {
                        Hit::Line { parent, before, .. } => self.place_collection(d.id, parent, before),
                        Hit::Into { set } => self.place_collection(d.id, Some(set), None),
                    }
                }
            }
            Some((_, Err(blocked))) => {
                if let Some(r) = under {
                    painter.rect_filled(r.rect, 0.0, Color32::from_black_alpha(110));
                }
                ctx.set_cursor_icon(egui::CursorIcon::NoDrop);
                if released {
                    egui::DragAndDrop::clear_payload(&ctx);
                    self.manage_message = Some(crate::tr(blocked.message()).to_string());
                }
            }
            _ => {}
        }
        if egui::DragAndDrop::has_payload_of_type::<CollectionDrag>(&ctx) {
            paint_drag_image(&ctx, pos, &d);
        }
        self.collection_drag.rows = rows;
    }
}

/// Drag image at the pointer: small dark box with the name (smart collections with "⚙"), like the photo drag image.
fn paint_drag_image(ctx: &egui::Context, pos: egui::Pos2, d: &CollectionDrag) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("collection-drag")));
    let galley = painter.layout_no_wrap(d.name.clone(), egui::TextStyle::Body.resolve(&ctx.global_style()), lr::TEXT());
    let r = egui::Rect::from_min_size(pos + egui::vec2(14.0, 8.0), galley.size() + egui::vec2(16.0, 8.0));
    painter.rect_filled(r, 0.0, crate::lr::gray(0x26));
    painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0x60)), egui::StrokeKind::Outside);
    painter.galley(r.min + egui::vec2(8.0, 4.0), galley, lr::TEXT());
    if d.kind == SET {
        // a set takes its content along: a second edge behind it
        painter.rect_stroke(r.translate(egui::vec2(3.0, 3.0)), 0.0, egui::Stroke::new(1.0, crate::lr::gray(0x48)), egui::StrokeKind::Outside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, kind: u8, parent: Option<i64>, depth: usize, open: bool, top: f32) -> Row {
        Row { id, kind, parent, depth, open, rect: egui::Rect::from_min_size(egui::pos2(0.0, top), egui::vec2(200.0, 20.0)) }
    }

    /// Top level: open set 1 (with 2, 3), closed set 4, collection 5; panel ends at 400.
    fn rows() -> Vec<Row> {
        vec![row(1, 1, None, 0, true, 0.0), row(2, 0, Some(1), 1, false, 20.0), row(3, 0, Some(1), 1, false, 40.0), row(4, 1, None, 0, false, 60.0), row(5, 0, None, 0, false, 80.0)]
    }

    #[test]
    fn zones_of_rows() {
        let r = rows();
        let line = |parent, before, y, depth| Some(Hit::Line { parent, before, y, depth });
        // collection: upper half before it, lower half after it (inside its set)
        assert_eq!(hit(&r, 22.0, 400.0), line(Some(1), Some(2), 20.0, 1));
        assert_eq!(hit(&r, 35.0, 400.0), line(Some(1), Some(3), 40.0, 1));
        // the last one of a set: after it = end of the set (not before the next top-level row)
        assert_eq!(hit(&r, 55.0, 400.0), line(Some(1), None, 60.0, 1));
        // set: upper quarter before, middle into, lower quarter after; open: first place inside
        assert_eq!(hit(&r, 2.0, 400.0), line(None, Some(1), 0.0, 0));
        assert_eq!(hit(&r, 10.0, 400.0), Some(Hit::Into { set: 1 }));
        assert_eq!(hit(&r, 18.0, 400.0), line(Some(1), Some(2), 20.0, 1));
        assert_eq!(hit(&r, 64.0, 400.0), line(None, Some(4), 60.0, 0));
        assert_eq!(hit(&r, 70.0, 400.0), Some(Hit::Into { set: 4 }));
        assert_eq!(hit(&r, 78.0, 400.0), line(None, Some(5), 80.0, 0));
        // below the last row: end of the top level; above the first and past the panel: nothing
        assert_eq!(hit(&r, 150.0, 400.0), line(None, None, 100.0, 0));
        assert_eq!(hit(&r, -5.0, 400.0), None);
        assert_eq!(hit(&r, 450.0, 400.0), None);
        // an open empty set: its lower quarter is the (only) place inside it
        let empty = vec![row(7, 1, None, 0, true, 0.0)];
        assert_eq!(hit(&empty, 18.0, 400.0), line(Some(7), None, 20.0, 1));
    }

    fn coll(id: i64, kind: u8, parent: Option<i64>, apple: bool) -> Collection {
        Collection { id, name: format!("c{id}"), parent, kind, count: 0, search: None, translatable: true, duplicates: false, position: None, apple, }
    }

    /// Set "Reisen" (1, open) with 2 and 3, then 5 and 6 at the top level; no library (the order changes on screen,
    /// writing needs a library and is tested in `library::collection_order`).
    fn view(ctx: &egui::Context) -> LibraryView {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        lr::apply_theme(ctx);
        let mut b = LibraryView::new(ctx);
        let mut list = vec![coll(1, 1, None, false), coll(2, 0, Some(1), false), coll(3, 0, Some(1), false), coll(5, 0, None, false), coll(6, 0, None, false)];
        list[0].name = "Reisen".into();
        b.set_collections(list);
        b.open_collections.insert(1);
        b
    }

    fn frame(ctx: &egui::Context, b: &mut LibraryView, time: f64, events: Vec<egui::Event>) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(260.0, 400.0));
        let input = egui::RawInput { time: Some(time), screen_rect: Some(screen), events, ..Default::default() };
        let _ = crate::ui_test::run(ctx, input, |root| {
            egui::CentralPanel::default().show(root, |ui| b.collection_list(ui));
        });
    }

    fn rect_of(b: &LibraryView, id: i64) -> egui::Rect {
        b.collection_drag.rows.iter().find(|r| r.id == id).map(|r| r.rect).expect("row drawn")
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn dragging_a_row_orders_and_a_click_still_chooses() {
        let ctx = egui::Context::default();
        let mut b = view(&ctx);
        frame(&ctx, &mut b, 0.0, vec![]);
        assert_eq!(b.level_ids(None), [1, 5, 6]);
        // drag 6 into the upper half of 5: before it
        let (from, to) = (rect_of(&b, 6).center(), rect_of(&b, 5).center() - egui::vec2(0.0, 5.0));
        frame(&ctx, &mut b, 0.1, vec![egui::Event::PointerMoved(from), press(from, true)]);
        frame(&ctx, &mut b, 0.2, vec![egui::Event::PointerMoved(from - egui::vec2(0.0, 12.0))]);
        assert!(egui::DragAndDrop::has_payload_of_type::<CollectionDrag>(&ctx), "dragging a row starts a collection drag");
        assert!(!egui::DragAndDrop::has_payload_of_type::<PhotoDrag>(&ctx));
        frame(&ctx, &mut b, 0.3, vec![egui::Event::PointerMoved(to)]);
        assert_eq!(b.level_ids(None), [1, 5, 6], "the list does not move while dragging");
        frame(&ctx, &mut b, 0.4, vec![press(to, false)]);
        assert_eq!(b.level_ids(None), [1, 6, 5]);
        // into the set: middle of its row → at its end (one frame first: the rows are drawn in the new order)
        frame(&ctx, &mut b, 0.45, vec![]);
        let (from, to) = (rect_of(&b, 5).center(), rect_of(&b, 1).center());
        frame(&ctx, &mut b, 0.5, vec![egui::Event::PointerMoved(from), press(from, true)]);
        frame(&ctx, &mut b, 0.6, vec![egui::Event::PointerMoved(from - egui::vec2(0.0, 12.0))]);
        frame(&ctx, &mut b, 0.7, vec![egui::Event::PointerMoved(to)]);
        frame(&ctx, &mut b, 0.8, vec![press(to, false)]);
        assert_eq!(b.level_ids(Some(1)), [2, 3, 5]);
        assert_eq!(b.level_ids(None), [1, 6]);
        // a short click chooses the collection, it does not drag
        frame(&ctx, &mut b, 0.9, vec![]);
        let at = rect_of(&b, 6).center();
        frame(&ctx, &mut b, 1.0, vec![egui::Event::PointerMoved(at), press(at, true)]);
        frame(&ctx, &mut b, 1.05, vec![press(at, false)]);
        assert!(!egui::DragAndDrop::has_any_payload(&ctx));
        assert_eq!(b.source, Source::Collection(6));
    }

    #[test]
    fn esc_cancels_and_a_set_never_goes_into_itself() {
        let ctx = egui::Context::default();
        let mut b = view(&ctx);
        frame(&ctx, &mut b, 0.0, vec![]);
        let (from, to) = (rect_of(&b, 6).center(), rect_of(&b, 5).left_top() + egui::vec2(20.0, 2.0));
        frame(&ctx, &mut b, 0.1, vec![egui::Event::PointerMoved(from), press(from, true)]);
        frame(&ctx, &mut b, 0.2, vec![egui::Event::PointerMoved(from - egui::vec2(0.0, 12.0))]);
        frame(&ctx, &mut b, 0.3, vec![egui::Event::PointerMoved(to), egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        assert!(!egui::DragAndDrop::has_any_payload(&ctx), "Esc drops the payload");
        frame(&ctx, &mut b, 0.4, vec![press(to, false)]);
        assert_eq!(b.level_ids(None), [1, 5, 6], "nothing changed");
        // the open set dragged onto its own child: blocked, nothing changes
        let (from, to) = (rect_of(&b, 1).center(), rect_of(&b, 2).center() + egui::vec2(0.0, 6.0));
        frame(&ctx, &mut b, 0.5, vec![egui::Event::PointerMoved(from), press(from, true)]);
        frame(&ctx, &mut b, 0.6, vec![egui::Event::PointerMoved(from + egui::vec2(0.0, 12.0))]);
        frame(&ctx, &mut b, 0.7, vec![egui::Event::PointerMoved(to)]);
        frame(&ctx, &mut b, 0.8, vec![press(to, false)]);
        assert_eq!(b.level_ids(None), [1, 5, 6]);
        assert_eq!(b.level_ids(Some(1)), [2, 3]);
    }

    #[test]
    fn resting_on_a_closed_set_opens_it_also_for_photos() {
        let ctx = egui::Context::default();
        let mut b = view(&ctx);
        b.open_collections.clear();
        frame(&ctx, &mut b, 0.0, vec![]);
        let at = rect_of(&b, 1).center();
        egui::DragAndDrop::set_payload(&ctx, PhotoDrag { images: vec![7], file: 7 });
        frame(&ctx, &mut b, 0.1, vec![egui::Event::PointerMoved(at)]);
        frame(&ctx, &mut b, 0.5, vec![]);
        assert!(!b.open_collections.contains(&1), "not before 0.7 s");
        frame(&ctx, &mut b, 0.9, vec![]);
        assert!(b.open_collections.contains(&1));
        egui::DragAndDrop::clear_payload(&ctx);
        // photos never get an insertion line or change the order
        assert_eq!(b.level_ids(None), [1, 5, 6]);
    }

    #[test]
    fn same_place_and_blocked_drops() {
        let r = rows();
        let list = vec![coll(1, 1, None, false), coll(2, 0, Some(1), false), coll(3, 0, Some(1), false), coll(4, 1, None, true), coll(5, 0, None, false)];
        // the same place: before itself or before its own next sibling
        assert_eq!(verdict(&list, &r, 2, Hit::Line { parent: Some(1), before: Some(2), y: 0.0, depth: 1 }), Ok(false));
        assert_eq!(verdict(&list, &r, 2, Hit::Line { parent: Some(1), before: Some(3), y: 0.0, depth: 1 }), Ok(false));
        assert_eq!(verdict(&list, &r, 3, Hit::Line { parent: Some(1), before: None, y: 0.0, depth: 1 }), Ok(false));
        assert_eq!(verdict(&list, &r, 3, Hit::Line { parent: Some(1), before: Some(2), y: 0.0, depth: 1 }), Ok(true));
        assert_eq!(verdict(&list, &r, 2, Hit::Line { parent: None, before: None, y: 0.0, depth: 0 }), Ok(true));
        // a set never into itself; only sets take collections; the Apple Photos set (4) takes nothing
        assert_eq!(verdict(&list, &r, 1, Hit::Into { set: 1 }), Err(Blocked::IntoItself));
        assert_eq!(verdict(&list, &r, 1, Hit::Line { parent: Some(1), before: Some(2), y: 0.0, depth: 1 }), Err(Blocked::IntoItself));
        assert_eq!(verdict(&list, &r, 5, Hit::Into { set: 4 }), Err(Blocked::AppleSet));
        assert_eq!(verdict(&list, &r, 5, Hit::Into { set: 1 }), Ok(true));
    }
}
