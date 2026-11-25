//! The print room's filmstrip: the normal filmstrip of the library's current view (all photos of the grid one came
//! from, in its sort order), and the library's selection is the set of photos to print. Click, ⇧-click, ⌘-click,
//! ←/→, ⇧←/⇧→ and ⌘A change that selection (written back to the library at once), ⌫ takes the main photo out of it.
//! A switch "Auswahl | Alle" shows only the selected photos or all of the view. The cell layout is the one of the main
//! filmstrip ([`crate::filmstrip_layout`]: prefix sums, only the visible cells are drawn, thumbnails are requested for
//! them and arrive in the background), the ⇧+arrow rules those of [`crate::library_select`] and the ⌘-click rule that
//! of the single views (`cmd_click_on_active`). The pure parts are here and tested; the room draws them.

use std::collections::HashSet;

use eframe::egui;
use egui::{Color32, Rect, Vec2};
use print::layout::Layout;

use crate::filmstrip_layout;
use crate::library_select::{next_head, span, start_state};
use crate::{lr, App};

/// Height of the strip panel (header row, thumbnails, the band with the page marks and the scroll bar).
pub(super) const STRIP_H: f32 = 132.0;
/// Band below the cells for the mark "on the page shown".
const MARK_H: f32 = 5.0;
/// Key in `settings.json`: the strip shows all photos of the view (true, default) or only the selected ones.
pub(super) const SHOW_ALL_KEY: &str = "printStripAll";

/// Selection of the view, by image id. The set of photos to print is `selected`; `active` is the main photo (white
/// ring). With an empty selection `active` only remembers where the arrows go on from.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Selection {
    pub active: Option<i64>,
    pub selected: HashSet<i64>,
    /// ⇧+arrow: anchor and moving end
    ext: Option<(i64, i64)>,
}

impl Selection {
    pub fn single(id: i64) -> Self {
        Selection { active: Some(id), selected: HashSet::from([id]), ext: None }
    }

    /// The library's selection: `chosen` (in view order) with the main photo `active` (if it is one of them, else the
    /// first chosen one).
    pub fn of_library(chosen: &[i64], active: Option<i64>) -> Self {
        let selected: HashSet<i64> = chosen.iter().copied().collect();
        let active = active.filter(|a| selected.contains(a)).or_else(|| chosen.first().copied());
        Selection { active, selected, ext: None }
    }

    /// The main photo, if it is selected.
    pub fn main(&self) -> Option<i64> {
        self.active.filter(|a| self.selected.contains(a))
    }

    pub fn active_pos(&self, order: &[i64]) -> Option<usize> {
        self.active.and_then(|a| order.iter().position(|&o| o == a))
    }

    /// Selected positions in view order.
    pub fn positions(&self, order: &[i64]) -> Vec<usize> {
        if self.selected.is_empty() {
            return Vec::new();
        }
        order.iter().enumerate().filter(|(_, o)| self.selected.contains(o)).map(|(p, _)| p).collect()
    }

    /// Click on position `pos` ("Alle"): plain = this photo alone, ⌘ = add / take out (the rule of the single views;
    /// the only selected photo leaves too, the selection may be empty), ⇧ = the range from the main photo on joins.
    pub fn click(&mut self, order: &[i64], pos: usize, cmd: bool, shift: bool) {
        let Some(&id) = order.get(pos) else { return };
        let current = self.main().and_then(|a| order.iter().position(|&o| o == a));
        if cmd {
            match crate::cmd_click_on_active(pos, current, &self.positions(order)) {
                crate::CmdClick::Toggle => {
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                        if current.is_none() {
                            self.active = Some(id);
                        }
                    }
                }
                crate::CmdClick::Next(n) => {
                    self.selected.remove(&id);
                    self.active = Some(order[n]);
                }
                // the only selected photo: out; the selection is empty, the arrows go on from here
                crate::CmdClick::Nothing => {
                    self.selected.remove(&id);
                }
            }
            self.ext = None;
        } else if let (true, Some(c)) = (shift, current) {
            for p in span(c, pos) {
                self.selected.insert(order[p]);
            }
            self.ext = Some((order[c], id));
        } else {
            *self = Selection::single(id);
        }
    }

    /// "Auswahl": a click makes the photo the main one, the selection stays.
    pub fn focus(&mut self, order: &[i64], pos: usize) {
        if let Some(&id) = order.get(pos) {
            self.active = Some(id);
            self.selected.insert(id);
            self.ext = None;
        }
    }

    /// ←/→ ("Alle"): the next / previous photo alone. Returns its position.
    pub fn step(&mut self, order: &[i64], forward: bool) -> Option<usize> {
        if order.is_empty() {
            return None;
        }
        let p = match self.active_pos(order) {
            Some(c) => next_head(c, if forward { 1 } else { -1 }, order.len()),
            None if forward => 0,
            None => order.len() - 1,
        };
        *self = Selection::single(order[p]);
        Some(p)
    }

    /// ←/→ in "Auswahl": the main photo moves to the next / previous selected one, the selection stays.
    pub fn step_within(&mut self, order: &[i64], forward: bool) -> Option<usize> {
        let shown = self.positions(order);
        let cur = self.main().and_then(|a| order.iter().position(|&o| o == a));
        let p = match cur.and_then(|c| shown.iter().position(|&s| s == c)) {
            Some(k) => shown[next_head(k, if forward { 1 } else { -1 }, shown.len())],
            None => *if forward { shown.first() } else { shown.last() }?,
        };
        self.active = Some(order[p]);
        self.ext = None;
        Some(p)
    }

    /// ⇧←/⇧→: extend the selection from the main photo one photo at a time; the opposite arrow shrinks it back.
    /// Returns the moving end (to keep it in view).
    pub fn extend(&mut self, order: &[i64], forward: bool) -> Option<usize> {
        let Some(cur) = self.main().and_then(|a| order.iter().position(|&o| o == a)) else { return self.step(order, forward) };
        let all = self.positions(order);
        let others: Vec<usize> = all.iter().copied().filter(|&p| p != cur).collect();
        let now: HashSet<usize> = all.into_iter().chain(std::iter::once(cur)).collect();
        let kept = self.ext.filter(|&(a, _)| Some(a) == self.active).and_then(|(a, h)| {
            let (pa, ph) = (order.iter().position(|&o| o == a)?, order.iter().position(|&o| o == h)?);
            let expected: HashSet<usize> = span(pa, ph).collect();
            (expected == now).then_some((pa, ph))
        });
        let (anchor, head) = kept.unwrap_or_else(|| start_state(cur, &others));
        let head = next_head(head, if forward { 1 } else { -1 }, order.len());
        self.selected = span(anchor, head).map(|p| order[p]).collect();
        self.ext = Some((order[anchor], order[head]));
        Some(head)
    }

    /// ⌘A: all photos of the view (the main photo stays).
    pub fn select_all(&mut self, order: &[i64]) {
        self.selected = order.iter().copied().collect();
        if self.active.is_none_or(|a| !self.selected.contains(&a)) {
            self.active = order.first().copied();
        }
        self.ext = None;
    }

    /// ⌫ / Entf: take the main photo out of the selection (nothing is deleted); the next selected photo becomes the
    /// main one. Returns whether the selection changed.
    pub fn deselect_active(&mut self, order: &[i64]) -> bool {
        let Some(pos) = self.main().and_then(|a| order.iter().position(|&o| o == a)) else { return false };
        self.click(order, pos, true, false);
        true
    }

    /// After a change of the view: forget photos that left it; a main photo that left is replaced by the first selected.
    pub fn tidy(&mut self, order: &[i64]) {
        let present: HashSet<i64> = order.iter().copied().collect();
        self.selected.retain(|s| present.contains(s));
        if self.active.is_some_and(|a| !present.contains(&a)) {
            self.active = order.iter().copied().find(|o| self.selected.contains(o));
        }
        if self.ext.is_some_and(|(a, h)| !present.contains(&a) || !present.contains(&h)) {
            self.ext = None;
        }
    }
}

/// The photos to print: the selected positions of the view that can be printed, in view order (the order fills the
/// cells).
pub(super) fn print_positions(order: &[i64], sel: &Selection, printable: impl Fn(usize) -> bool) -> Vec<usize> {
    sel.positions(order).into_iter().filter(|&p| printable(p)).collect()
}

/// Index in the print list (sorted view positions) of the photo at view position `pos`.
pub(super) fn print_index(positions: &[usize], pos: usize) -> Option<usize> {
    positions.binary_search(&pos).ok()
}

/// Page of the layout that shows print photo `k` (its first page).
pub(super) fn page_of(l: &Layout, k: usize) -> Option<usize> {
    l.pages.iter().position(|p| p.photos.iter().any(|q| q.photo == k))
}

/// Print photos (indices of the print list) on page `page`.
pub(super) fn on_page(l: &Layout, page: usize) -> HashSet<usize> {
    l.pages.get(page).map(|p| p.photos.iter().map(|q| q.photo).collect()).unwrap_or_default()
}

/// Scroll position that shows the main photo (cell `anchor`) about in the middle. With several selected cells
/// (`selected`, sorted) that fit into the view together, their span is centred; otherwise the main photo sits a little
/// left of the middle, so the selected photos after it are in view as well. Never past either end.
pub(super) fn center_offset(layout: &filmstrip_layout::Layout, anchor: usize, selected: &[usize], view_w: f32) -> f32 {
    let max = (layout.total() - view_w).max(0.0);
    let mid = layout.x(anchor) + layout.width(anchor) / 2.0;
    let goal = match (selected.first(), selected.last()) {
        (Some(&first), Some(&last)) if first != last => {
            let (a, b) = (layout.x(first), layout.x(last) + layout.width(last));
            if b - a <= view_w * 0.9 && (a..=b).contains(&mid) { (a + b - view_w) / 2.0 } else { mid - view_w * 0.4 }
        }
        _ => mid - view_w / 2.0,
    };
    goal.clamp(0.0, max)
}

/// One frame of the gentle scroll towards `goal`: the new offset and whether it arrived.
pub(super) fn ease(offset: f32, goal: f32) -> (f32, bool) {
    let d = goal - offset;
    if d.abs() < 0.5 {
        (goal, true)
    } else {
        (offset + d * 0.3, false)
    }
}

/// Where the strip scrolls to (next frame, when it knows its width).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Follow {
    /// the main photo about in the middle (opening, switching "Auswahl | Alle"); `instant`: without easing
    Center { instant: bool },
    /// this view position into view, only if it is outside (arrows, clicks)
    Keep(usize),
}

/// Cells of the strip: the layout of the shown photos and, in "Auswahl", which view positions they are.
pub(super) struct Cells {
    key: (u64, bool, u64, u32, usize),
    layout: filmstrip_layout::Layout,
    /// view positions of the cells ("Auswahl"); `None` = all of the view, cell = position
    shown: Option<Vec<usize>>,
}

impl Cells {
    fn pos(&self, cell: usize) -> usize {
        self.shown.as_ref().map_or(cell, |s| s[cell])
    }

    fn cell(&self, pos: usize) -> Option<usize> {
        match &self.shown {
            None => (pos < self.layout.len()).then_some(pos),
            Some(s) => s.binary_search(&pos).ok(),
        }
    }
}

/// What the strip asks for; applied after the frame is drawn.
pub(super) enum Action {
    Click(usize),
    ShowAll(bool),
}

impl App {
    /// Test hooks (screenshots): GRAINEO_PRINT_ALL=1 selects all photos of the view, GRAINEO_PRINT_SELECT="2,4-6"
    /// these positions of the view (1-based, the first one is the main photo), GRAINEO_PRINT_STRIP=selection|all the
    /// switch of the strip.
    pub(crate) fn print_room_test_hooks(&mut self) {
        if !self.print_room.open {
            return;
        }
        let order = self.print_room.order.clone();
        // GRAINEO_PRINT_ORIENTATION=portrait|landscape: as if switched by hand (no suggestion afterwards)
        match crate::env::var("GRAINEO_PRINT_ORIENTATION").as_deref() {
            Ok("portrait") => {
                self.print_room.t.set_orientation(print::template::Orientation::Portrait);
                self.print_room.orientation_manual = true;
            }
            Ok("landscape") => {
                self.print_room.t.set_orientation(print::template::Orientation::Landscape);
                self.print_room.orientation_manual = true;
            }
            _ => {}
        }
        if crate::env::var("GRAINEO_PRINT_ALL").is_ok_and(|v| v == "1") {
            self.print_room.sel.select_all(&order);
        }
        if let Ok(v) = crate::env::var("GRAINEO_PRINT_SELECT") {
            let mut pos: Vec<usize> = Vec::new();
            for part in v.split(',') {
                let (a, b) = part.split_once('-').unwrap_or((part, part));
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    pos.extend((a.max(1)..=b).map(|p| p - 1));
                }
            }
            let ids: Vec<i64> = pos.into_iter().filter_map(|p| order.get(p).copied()).collect();
            self.print_room.sel = Selection::of_library(&ids, ids.first().copied());
        }
        match crate::env::var("GRAINEO_PRINT_STRIP").as_deref() {
            Ok("selection") => self.print_room.show_all = false,
            Ok("all") => self.print_room.show_all = true,
            _ => {}
        }
        self.print_selection_changed(Some(Follow::Center { instant: true }));
    }

    /// The selection changed in the room: it is the library's selection (grid and filmstrip of the single views), the
    /// print list and the layout follow, the preview goes to the page of the main photo, the strip scrolls if `follow`.
    pub(super) fn print_selection_changed(&mut self, follow: Option<Follow>) {
        self.print_mark_changed(follow);
        let sel = &self.print_room.sel;
        let mut ids: Vec<i64> = Vec::with_capacity(sel.selected.len());
        for e in self.entries.iter_mut() {
            e.selected = sel.selected.contains(&e.image);
            if e.selected {
                ids.push(e.image);
            }
        }
        match sel.main() {
            Some(a) => self.lib.set_selection(a, &ids),
            None => self.lib.clear_selection(),
        }
    }

    /// The room's part of a change (also on opening, where the selection is the library's already): the print list is
    /// built again, the preview goes to the page of the main photo, the strip scrolls if `follow`.
    pub(super) fn print_mark_changed(&mut self, follow: Option<Follow>) {
        let room = &mut self.print_room;
        room.sel_gen += 1;
        room.jump = true;
        if follow.is_some() {
            room.follow = follow;
        }
    }

    /// ⌘A in the print room (menu command or key): all photos of the view.
    pub(crate) fn print_select_all(&mut self) {
        let room = &mut self.print_room;
        room.sel.select_all(&room.order);
        self.print_selection_changed(None);
    }

    /// "Auswahl aufheben" in the print room: nothing to print (the main photo stays where the arrows go on from).
    pub(crate) fn print_select_none(&mut self) {
        self.print_room.sel.selected.clear();
        self.print_selection_changed(None);
    }

    /// Keys of the strip (only while the room has the keyboard: no text field, no dialog).
    pub(super) fn print_strip_keys(&mut self, ctx: &egui::Context) {
        use crate::shortcuts::{pressed, which};
        let all = self.print_room.show_all;
        let room = &mut self.print_room;
        let order = &room.order;
        let step = if pressed(ctx, "print_prev") {
            Some(false)
        } else if pressed(ctx, "print_next") {
            Some(true)
        } else {
            None
        };
        let mut moved = None;
        if let Some(forward) = step {
            moved = if all { room.sel.step(order, forward) } else { room.sel.step_within(order, forward) };
        } else if let Some(k) = which(ctx, "print_extend") {
            // "Auswahl" shows only the selection: ⇧+arrow moves like the arrow (nothing to extend into)
            moved = if all { room.sel.extend(order, k == 1) } else { room.sel.step_within(order, k == 1) };
        }
        let mut changed = moved.is_some();
        if which(ctx, "print_deselect").is_some() && room.sel.deselect_active(order) {
            moved = room.sel.main().and_then(|a| order.iter().position(|&o| o == a));
            changed = true;
        }
        if pressed(ctx, "print_select_all") {
            room.sel.select_all(order);
            changed = true;
        }
        if changed {
            self.print_selection_changed(moved.map(Follow::Keep));
        }
    }

    /// Apply an action of the strip.
    pub(super) fn print_apply(&mut self, a: Action) {
        let (cmd, shift) = {
            let m = crate::click_mods::at_press(&self.egui_ctx);
            (m.command, m.shift)
        };
        let room = &mut self.print_room;
        match a {
            Action::Click(pos) => {
                if room.show_all || cmd {
                    room.sel.click(&room.order, pos, cmd, shift);
                } else {
                    room.sel.focus(&room.order, pos);
                }
                self.print_selection_changed(Some(Follow::Keep(pos)));
            }
            Action::ShowAll(all) => {
                room.show_all = all;
                library::settings::set(SHOW_ALL_KEY, serde_json::json!(all));
                room.follow = Some(Follow::Center { instant: false });
            }
        }
    }

    /// Left part of the header: source, count, selection and main photo (as in the main filmstrip).
    fn print_strip_header(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let n = self.entries.len();
        let sel_n = self.print_room.sel.selected.len();
        let header_w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(header_w, filmstrip_layout::HEADER_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_size(egui::vec2(header_w, filmstrip_layout::HEADER_H));
            ui.set_max_height(filmstrip_layout::HEADER_H);
            let mut all = self.print_room.show_all;
            if lr::segmented_tips(ui, &mut all, &[(false, "Auswahl"), (true, "Alle")], &["Nur die ausgewählten Fotos zeigen", "Alle Fotos der Ansicht zeigen"], 6.0) {
                action = Some(Action::ShowAll(all));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let source = self.lib.source_title();
                let count = if n == 1 {
                    crate::menu::i18n::fill_names(crate::tr("{a} · 1 Bild"), &[("a", &source)])
                } else {
                    crate::menu::i18n::fill_names(crate::tr("{a} · {b} Bilder"), &[("a", &source), ("b", &n.to_string())])
                };
                ui.add(egui::Label::new(egui::RichText::new(count).small()).truncate());
                if sel_n > 1 {
                    ui.label(egui::RichText::new(crate::trf!("· {a} ausgewählt", a = sel_n)).small());
                }
                let main = self.print_room.sel.main();
                if let Some(e) = main.and_then(|a| self.print_room.order.iter().position(|&o| o == a)).and_then(|p| self.entries.get(p)) {
                    ui.add(egui::Label::new(egui::RichText::new(format!("/ {}", e.name)).small().color(lr::TEXT_DIM())).truncate());
                }
            });
        });
        action
    }

    /// The strip at the bottom of the room: the view's photos (or only the selected ones), the selection marked as in
    /// the grid, a gold mark below the photos on the page shown. Only the visible cells are drawn and asked for
    /// thumbnails (collected in `room.wanted`).
    pub(super) fn print_strip(&mut self, ui: &mut egui::Ui, l: &Layout) -> Option<Action> {
        let mut action = self.print_strip_header(ui);
        ui.add_space(2.0);
        let cells_h = (ui.available_height() - filmstrip_layout::SCROLLBAR - MARK_H).max(1.0);
        // cells: rebuilt only when the view, the switch, the selection ("Auswahl") or the height changes
        let room = &self.print_room;
        let key = (room.list_version, room.show_all, if room.show_all { 0 } else { room.sel_gen }, cells_h.to_bits(), self.entries.len());
        if room.cells.as_ref().is_none_or(|c| c.key != key) {
            let shown = (!room.show_all).then(|| room.sel.positions(&room.order));
            let layout = match &shown {
                None => filmstrip_layout::Layout::new(self.entries.iter().map(|e| e.aspect), cells_h),
                Some(s) => filmstrip_layout::Layout::new(s.iter().map(|&p| self.entries[p].aspect), cells_h),
            };
            self.print_room.cells = Some(Cells { key, layout, shown });
        }
        let Some(cells) = self.print_room.cells.take() else { return action };
        let layout = &cells.layout;
        let pad = filmstrip_layout::PAD;
        let cell_h = layout.thumb_h + 2.0 * pad + layout.meta_h;
        if layout.len() == 0 {
            let r = ui.available_rect_before_wrap();
            let text = if self.entries.is_empty() {
                crate::tr("Keine Fotos in der Ansicht")
            } else {
                crate::tr("Keine Fotos ausgewählt: „Alle“ zeigt alle Fotos der Ansicht")
            };
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(12.0), lr::TEXT_DIM());
            ui.allocate_rect(r, egui::Sense::hover());
            self.print_room.follow = None;
            self.print_room.cells = Some(cells);
            return action;
        }
        // mouse wheel scrolls sideways (as in the main filmstrip)
        let wheel = if ui.rect_contains_pointer(ui.available_rect_before_wrap()) {
            ui.input_mut(|i| {
                let d = i.smooth_scroll_delta;
                if d.y != 0.0 && d.x.abs() < d.y.abs() {
                    i.smooth_scroll_delta = Vec2::ZERO;
                    d.y
                } else {
                    0.0
                }
            })
        } else {
            0.0
        };
        let view_w = ui.available_width();
        let max = (layout.total() - view_w).max(0.0);
        let mut offset = self.print_room.strip_scroll.clamp(0.0, max);
        let mut jumped = false;
        if wheel != 0.0 {
            offset = (offset - wheel).clamp(0.0, max);
            self.print_room.strip_goal = None;
            jumped = true;
        }
        // where to go: computed once per request, then eased there frame by frame (no jump every frame)
        if let Some(f) = self.print_room.follow.take() {
            let room = &self.print_room;
            let goal = match f {
                Follow::Center { .. } => {
                    let selected: Vec<usize> = room.sel.positions(&room.order).into_iter().filter_map(|p| cells.cell(p)).collect();
                    let anchor = room.sel.main().and_then(|a| room.order.iter().position(|&o| o == a)).and_then(|p| cells.cell(p)).or_else(|| selected.first().copied());
                    anchor.map(|a| center_offset(layout, a, &selected, view_w))
                }
                Follow::Keep(pos) => cells.cell(pos).map(|c| layout.bring_into_view(c, offset, view_w)),
            };
            match (goal, f) {
                (Some(g), Follow::Center { instant: true }) => {
                    offset = g;
                    self.print_room.strip_goal = None;
                    jumped = true;
                }
                (Some(g), _) if (g - offset).abs() >= 0.5 => self.print_room.strip_goal = Some(g),
                _ => {}
            }
        }
        if let Some(goal) = self.print_room.strip_goal {
            let (o, done) = ease(offset, goal.clamp(0.0, max));
            offset = o;
            jumped = true;
            if done {
                self.print_room.strip_goal = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        if jumped {
            if let Some(id) = self.print_room.strip_area {
                egui::scroll_area::State::default().store(ui.ctx(), id);
            }
        }
        let main = self.print_room.sel.main();
        let multi = self.print_room.sel.selected.len() >= 2;
        // the photos on the page shown, as view positions
        let marked: HashSet<usize> = on_page(l, self.print_room.page).into_iter().filter_map(|k| self.print_room.photos.get(k).map(|p| p.pos)).collect();
        let mut wanted: Vec<i64> = Vec::new();
        ui.spacing_mut().scroll.floating_allocated_width = 0.0;
        let area = egui::ScrollArea::horizontal().id_salt("print-strip").auto_shrink([false; 2]).horizontal_scroll_offset(offset);
        let strip = area.show_viewport(ui, |ui, vp| {
            let h = ui.available_height();
            ui.set_min_size(egui::vec2(layout.total(), h));
            let origin = ui.min_rect().min;
            for c in layout.visible(vp.min.x, vp.max.x) {
                let pos = cells.pos(c);
                let Some(e) = self.entries.get(pos) else { continue };
                let (image, file) = (e.image, e.file);
                let printable = crate::developable_state(e.state) && crate::image_kind::of(&e.path).editable();
                let r = Rect::from_min_size(origin + egui::vec2(layout.x(c), 0.0), egui::vec2(layout.width(c), cell_h));
                let resp = ui.interact(r, ui.scope_id().with(("print-strip", image)), egui::Sense::click());
                let is_sel = self.print_room.sel.selected.contains(&image);
                let is_main = main == Some(image);
                if resp.hovered() && !is_sel {
                    ui.painter().rect_filled(r, 0.0, crate::lr::gray(0x3c));
                }
                if is_main {
                    ui.painter().rect_stroke(r.expand(1.0), 0.0, egui::Stroke::new(2.0, crate::lr::TEXT_STRONG()), egui::StrokeKind::Inside);
                } else if is_sel {
                    ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0xc6)), egui::StrokeKind::Inside);
                }
                let img_area = Rect::from_min_size(r.min + egui::vec2(pad, pad), egui::vec2(r.width() - 2.0 * pad, layout.thumb_h));
                match self.lib.thumbnail(file) {
                    Some(t) => {
                        let q = self.lib.turned.quarters(image);
                        let sz = crate::library_turn::turned_size(t.size_vec2(), q);
                        let k = (img_area.width() / sz.x.max(1.0)).min(img_area.height() / sz.y.max(1.0));
                        let recede = multi && !is_sel && !resp.hovered();
                        let tint = if !printable { Color32::from_gray(80) } else if recede { Color32::from_gray(150) } else { Color32::WHITE };
                        crate::library_turn::paint_image(ui.painter(), t.id(), Rect::from_center_size(img_area.center(), sz * k), q, tint);
                    }
                    None => {
                        wanted.push(image);
                        ui.painter().rect_filled(img_area, 0.0, crate::lr::gray(0x16));
                    }
                }
                // thin mark below the photos on the page shown
                if marked.contains(&pos) {
                    let y = r.bottom() + MARK_H / 2.0 + 0.5;
                    ui.painter().line_segment([egui::pos2(r.left() + 3.0, y), egui::pos2(r.right() - 3.0, y)], egui::Stroke::new(2.0, lr::GOLD()));
                }
                if resp.hovered() {
                    let name = e.name.clone();
                    let tip = if !printable {
                        crate::trf!("{a} (lässt sich nicht drucken)", a = name)
                    } else {
                        let positions: Vec<usize> = self.print_room.photos.iter().map(|p| p.pos).collect();
                        match print_index(&positions, pos).and_then(|k| page_of(l, k)) {
                            Some(p) => format!("{name} · {}", crate::trf!("Seite {a}", a = p + 1)),
                            None => name,
                        }
                    };
                    lr::hover_tip(ui, &resp, &tip);
                }
                if resp.clicked() {
                    action = Some(Action::Click(pos));
                }
            }
        });
        self.print_room.strip_area = Some(strip.id);
        self.print_room.strip_scroll = strip.state.clamped_offset().x;
        self.print_room.wanted.extend(wanted);
        self.print_room.cells = Some(cells);
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: [i64; 6] = [10, 11, 12, 13, 14, 15];

    fn sel(s: &Selection) -> Vec<i64> {
        let mut v: Vec<i64> = s.selected.iter().copied().collect();
        v.sort();
        v
    }

    #[test]
    fn the_library_selection_is_the_print_set() {
        // several chosen, the main photo among them
        let s = Selection::of_library(&[11, 13, 14], Some(13));
        assert_eq!(s.main(), Some(13));
        assert_eq!(print_positions(&ORDER, &s, |_| true), vec![1, 3, 4]);
        // main photo not chosen: the first chosen one
        assert_eq!(Selection::of_library(&[12, 14], Some(10)).main(), Some(12));
        // nothing selected but an active photo: the library hands over the active one (target_images)
        let s = Selection::of_library(&[15], Some(15));
        assert_eq!(print_positions(&ORDER, &s, |_| true), vec![5]);
        // neither: empty print set
        let s = Selection::of_library(&[], None);
        assert_eq!(s.main(), None);
        assert!(print_positions(&ORDER, &s, |_| true).is_empty());
        // photos that cannot be printed are not in the print, the order is the view's
        let s = Selection::of_library(&[15, 10, 12], Some(15));
        assert_eq!(print_positions(&ORDER, &s, |p| p != 2), vec![0, 5]);
    }

    #[test]
    fn plain_click_selects_one() {
        let mut s = Selection::single(10);
        s.click(&ORDER, 3, false, false);
        assert_eq!(s.active, Some(13));
        assert_eq!(sel(&s), vec![13]);
    }

    #[test]
    fn shift_click_selects_the_range_from_the_main_photo() {
        let mut s = Selection::single(11);
        s.click(&ORDER, 4, false, true);
        assert_eq!(s.active, Some(11));
        assert_eq!(sel(&s), vec![11, 12, 13, 14]);
        let mut s = Selection::single(14);
        s.click(&ORDER, 1, false, true);
        assert_eq!(sel(&s), vec![11, 12, 13, 14]);
        // without a selection ⇧-click is a plain click
        let mut s = Selection::default();
        s.click(&ORDER, 2, false, true);
        assert_eq!(sel(&s), vec![12]);
    }

    #[test]
    fn cmd_click_adds_and_takes_out_down_to_nothing() {
        let mut s = Selection::single(10);
        s.click(&ORDER, 2, true, false);
        s.click(&ORDER, 4, true, false);
        assert_eq!(sel(&s), vec![10, 12, 14]);
        assert_eq!(s.active, Some(10));
        s.click(&ORDER, 2, true, false);
        assert_eq!(sel(&s), vec![10, 14]);
        // the main photo of several leaves, the next selected becomes the main one
        s.click(&ORDER, 0, true, false);
        assert_eq!(s.main(), Some(14));
        assert_eq!(sel(&s), vec![14]);
        // the only one leaves too: empty print set, the arrows go on from there
        s.click(&ORDER, 4, true, false);
        assert!(s.selected.is_empty());
        assert_eq!(s.main(), None);
        assert_eq!(s.step(&ORDER, true), Some(5));
        // ⌘-click into an empty selection: this photo, as the main one
        let mut e = Selection::default();
        e.click(&ORDER, 3, true, false);
        assert_eq!((e.main(), sel(&e)), (Some(13), vec![13]));
    }

    #[test]
    fn delete_key_only_takes_out_of_the_selection() {
        let mut s = Selection::of_library(&[11, 12, 14], Some(12));
        assert!(s.deselect_active(&ORDER));
        assert_eq!(sel(&s), vec![11, 14]);
        assert_eq!(s.main(), Some(14));
        assert!(s.deselect_active(&ORDER));
        assert!(s.deselect_active(&ORDER));
        assert!(s.selected.is_empty());
        assert!(!s.deselect_active(&ORDER));
    }

    #[test]
    fn arrows_move_and_shift_arrows_extend() {
        let mut s = Selection::single(12);
        assert_eq!(s.step(&ORDER, true), Some(3));
        assert_eq!(s.active, Some(13));
        assert_eq!(s.step(&ORDER, false), Some(2));
        let mut e = Selection::single(15);
        assert_eq!(e.step(&ORDER, true), Some(5));
        assert_eq!(s.extend(&ORDER, true), Some(3));
        assert_eq!(s.extend(&ORDER, true), Some(4));
        assert_eq!(sel(&s), vec![12, 13, 14]);
        assert_eq!(s.extend(&ORDER, false), Some(3));
        assert_eq!(sel(&s), vec![12, 13]);
        assert_eq!(s.active, Some(12));
        s.extend(&ORDER, false);
        s.extend(&ORDER, false);
        assert_eq!(sel(&s), vec![11, 12]);
        let mut t = Selection::single(11);
        t.click(&ORDER, 3, false, true);
        assert_eq!(t.extend(&ORDER, true), Some(4));
        assert_eq!(sel(&t), vec![11, 12, 13, 14]);
    }

    /// "Auswahl": clicks and arrows move the main photo among the selected ones, the selection stays.
    #[test]
    fn selection_mode_moves_within_the_selection() {
        let mut s = Selection::of_library(&[11, 13, 15], Some(11));
        assert_eq!(s.step_within(&ORDER, true), Some(3));
        assert_eq!(s.step_within(&ORDER, true), Some(5));
        assert_eq!(s.step_within(&ORDER, true), Some(5));
        assert_eq!(s.step_within(&ORDER, false), Some(3));
        assert_eq!(sel(&s), vec![11, 13, 15]);
        s.focus(&ORDER, 1);
        assert_eq!((s.main(), sel(&s)), (Some(11), vec![11, 13, 15]));
        assert_eq!(Selection::default().step_within(&ORDER, true), None);
    }

    #[test]
    fn select_all_and_tidy() {
        let mut s = Selection::single(12);
        s.select_all(&ORDER);
        assert_eq!(sel(&s), ORDER.to_vec());
        assert_eq!(s.active, Some(12));
        s.tidy(&[10, 11]);
        assert_eq!(s.active, Some(10));
        assert_eq!(sel(&s), vec![10, 11]);
        let mut empty = Selection::default();
        empty.tidy(&[]);
        assert_eq!(empty, Selection::default());
    }

    /// Opening and switching: the main photo about in the middle; several photos that fit are centred as a block,
    /// a long selection starts a little left of the middle.
    #[test]
    fn scroll_target_centres_the_selection() {
        let l = filmstrip_layout::Layout::new(std::iter::repeat_n(1.5, 200), 60.0);
        let view = 800.0;
        let mid = |o: f32, c: usize| l.x(c) + l.width(c) / 2.0 - o;
        // one photo: its middle in the middle of the view
        let o = center_offset(&l, 100, &[100], view);
        assert!((mid(o, 100) - view / 2.0).abs() < 0.01);
        // three neighbours: the block is centred, the main photo (the first) a little left of the middle
        let o = center_offset(&l, 100, &[100, 101, 102], view);
        let block = (l.x(100) + l.x(102) + l.width(102)) / 2.0 - o;
        assert!((block - view / 2.0).abs() < 0.01);
        assert!(mid(o, 100) < view / 2.0 && mid(o, 100) > 0.0);
        // a selection wider than the view: the main photo at 40 %, the next selected ones in view to its right
        let wide: Vec<usize> = (100..140).collect();
        let o = center_offset(&l, 100, &wide, view);
        assert!((mid(o, 100) - view * 0.4).abs() < 0.01);
        // never past the ends
        assert_eq!(center_offset(&l, 0, &[0], view), 0.0);
        assert_eq!(center_offset(&l, 199, &[199], view), l.total() - view);
        // easing reaches the goal in a few frames and never overshoots
        let (mut off, mut n) = (0.0, 0);
        loop {
            let (o, done) = ease(off, 500.0);
            assert!(o <= 500.0);
            off = o;
            n += 1;
            if done {
                break;
            }
        }
        assert_eq!(off, 500.0);
        assert!(n < 30);
    }

    /// The selection changes the layout at once: the print list fills the cells in view order, the page of a photo
    /// and the marks of a page follow; an empty selection has no pages.
    #[test]
    fn layout_follows_the_selection() {
        use print::layout::{layout, PhotoSpec};
        use print::template::Template;
        let mut t = Template::default();
        t.grid.columns = 2;
        t.grid.rows = 2;
        t.image.repeat_one_per_page = false;
        let order: Vec<i64> = (1..=8).collect();
        let spec = |p: usize| PhotoSpec { width: 2000 + p as u32, height: 2000, caption: String::new(), offset: [0.0; 2] };
        let lay = |s: &Selection| {
            let pos = print_positions(&order, s, |_| true);
            (layout(&t, &pos.iter().map(|&p| spec(p)).collect::<Vec<_>>(), None, ""), pos)
        };
        let mut s = Selection::single(1);
        s.select_all(&order);
        let (l, pos) = lay(&s);
        assert_eq!(l.pages.len(), 2);
        assert_eq!(page_of(&l, print_index(&pos, 5).unwrap()), Some(1));
        // ⌘-click two out: one page; the view order fills the cells
        s.click(&order, 1, true, false);
        s.click(&order, 6, true, false);
        let (l, pos) = lay(&s);
        assert_eq!(pos, vec![0, 2, 3, 4, 5, 7]);
        assert_eq!(l.pages.len(), 2);
        assert_eq!(on_page(&l, 0), HashSet::from([0, 1, 2, 3]));
        assert_eq!(print_index(&pos, 7).and_then(|k| page_of(&l, k)), Some(1));
        assert_eq!(print_index(&pos, 1), None);
        // a single click: one photo, one page
        s.click(&order, 4, false, false);
        let (l, pos) = lay(&s);
        assert_eq!((pos, l.pages.len()), (vec![4], 1));
        // empty selection: no pages (the main button is off), no panic
        s.deselect_active(&order);
        let (l, pos) = lay(&s);
        assert!(pos.is_empty() && l.pages.is_empty());
        assert_eq!(page_of(&l, 0), None);
    }

    /// 50 000 photos in the view: taking over the selection, the print list, the scroll target and the visible range
    /// stay far below 100 ms, and only a few cells are drawn.
    #[test]
    fn large_view_is_cheap_and_virtualised() {
        let n = 50_000usize;
        let order: Vec<i64> = (0..n as i64).map(|i| i * 3 + 7).collect();
        let t0 = std::time::Instant::now();
        let chosen: Vec<i64> = order[30_000..30_005].to_vec();
        let s = Selection::of_library(&chosen, Some(order[30_002]));
        let pos = print_positions(&order, &s, |_| true);
        let l = filmstrip_layout::Layout::new((0..n).map(|i| if i % 3 == 0 { 0.667 } else { 1.5 }), 90.0);
        let view = 1400.0;
        let anchor = s.active_pos(&order).unwrap();
        let o = center_offset(&l, anchor, &pos, view);
        let visible = l.visible(o, o + view);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(pos, (30_000..30_005).collect::<Vec<_>>());
        assert!(visible.contains(&anchor) && visible.contains(&30_004), "{visible:?}");
        assert!(visible.len() < 40, "only the visible cells: {}", visible.len());
        assert!(ms < 100.0, "{ms:.1} ms");
        // ⌘A on all 50 000 and the print list from it: still cheap
        let t1 = std::time::Instant::now();
        let mut all = s.clone();
        all.select_all(&order);
        assert_eq!(print_positions(&order, &all, |_| true).len(), n);
        assert!(t1.elapsed().as_millis() < 100);
    }
}
