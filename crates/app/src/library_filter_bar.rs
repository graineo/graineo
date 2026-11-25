//! Filter bar above the library grid (decided layout "search first"):
//!
//! - Row 1: search field and the FILTER button share the width (about half each, with minimum widths). The
//!   button carries the funnel, the word FILTER, the number of filtering metadata columns and, while there is
//!   room, small chips with the names of those columns ("+N" for the rest); beside it a reset for the metadata
//!   filters alone. The button is blue while metadata filters are active, so the state stays visible with the
//!   panel closed.
//! - The metadata panel (columns, unchanged) opens directly below row 1 over the full width.
//! - Row 2: the quick filters on the left (flags, stars, labels, kind, RAW+JPEG), sort order and direction on
//!   the right. No captions; the groups are separated by gaps only.
//! - Below that, only while something filters: the active search words and quick filters as chips.
//! - The actions on the selection (turn left/right, jump to the selection) sit in the status row right of the
//!   size slider ([`LibraryView::selection_actions`]).
//!
//! All heights are fixed and every decision depends on the width alone, so nothing jumps while working.

use super::*;

/// Active metadata filters: one quiet treatment in the app's blue ([`lr::BLUE`]) – a muted wash over the group
/// surface, no extra frame; the chips are a slightly stronger wash of the same blue, the badge the full blue.
const WASH: f32 = 0.22;
const WASH_HOVER: f32 = 0.30;
const CHIP_WASH: f32 = 0.30;
/// Minimum widths of the two halves of row 1.
const SEARCH_MIN: f32 = 150.0;
const FILTER_MIN: f32 = 130.0;
/// Fixed parts of row 1 besides the two halves: gap, gap, reset button (the "?" sits inside the search field), gap
/// and the view switch "Raster | Vergleichen" at the right end.
const ROW1_FIXED: f32 = 8.0 + 4.0 + RESET_W + 8.0 + VIEW_W;
/// Width of the view switch (two text cells, see [`LibraryView::view_switch`]).
pub(crate) const VIEW_W: f32 = VIEW_CELLS[0] + VIEW_CELLS[1];
pub(crate) const VIEW_CELLS: [f32; 2] = [70.0, 104.0];
const RESET_W: f32 = 30.0;
/// Width reserved for the sort order (dropdown and direction) at the right of row 2.
const SORT_W: f32 = 160.0;
/// Widths of the quick-filter groups in row 2: flags, stars, labels, kind, RAW+JPEG.
const QUICK_GROUPS: [f32; 5] = [3.0 * 30.0, 5.0 * 22.0, 5.0 * 24.0, 3.0 * 30.0, 30.0 + 38.0];
/// Gap between the lines of row 2 when it wraps.
const LINE_GAP: f32 = 6.0;
/// Gap between the groups of row 2.
const GROUP_GAP: f32 = 10.0;
/// Cells of the selection actions in the status row (height of the status row).
const ACTION_W: f32 = 26.0;
const ACTION_H: f32 = 20.0;

/// Widths of the search field and the FILTER button in row 1 of `width`: about half each, neither below its
/// minimum (the search field wins when both do not fit).
pub(super) fn split_row1(width: f32) -> (f32, f32) {
    let free = (width - ROW1_FIXED).max(0.0);
    let search = (free - (free / 2.0).max(FILTER_MIN)).max(SEARCH_MIN);
    (search, (free - search).max(48.0))
}

/// Places of the row-2 groups for `width` (relative to the row's top left), the place of the sort order and the
/// row's height. Groups flow left to right and start a new line when the next one does not fit; the sort order
/// goes right-aligned at the end of the last line, or onto a line of its own.
pub(super) fn quick_layout(width: f32) -> (Vec<egui::Pos2>, egui::Pos2, f32) {
    let line = lr::BAR_H + LINE_GAP;
    let (mut x, mut y) = (0.0, 0.0);
    let mut places = Vec::with_capacity(QUICK_GROUPS.len());
    for w in QUICK_GROUPS {
        if x > 0.0 && x + w > width {
            x = 0.0;
            y += line;
        }
        places.push(egui::pos2(x, y));
        x += w + GROUP_GAP;
    }
    if x - GROUP_GAP + 16.0 + SORT_W > width {
        y += line;
    }
    (places, egui::pos2((width - SORT_W).max(0.0), y), y + lr::BAR_H)
}

/// Quick filters as rule terms: flags and labels as alternatives (`flag:picked | flag:none`), stars as a minimum.
/// Search text as one self-contained part of a rule: an "or" word at the start or end (typed half-way, "foo |") would
/// join the neighbouring part of the rule to an "or" and change what the rule finds, and an open quotation mark would
/// swallow the parts after it.
pub(super) fn rule_text(text: &str) -> String {
    let is_or = |w: &str| matches!(w, "|" | "OR" | "ODER");
    let mut t = text.trim();
    loop {
        let before = t;
        if let Some(w) = t.split_whitespace().next_back().filter(|w| is_or(w)) {
            t = t[..t.len() - w.len()].trim_end();
        }
        if let Some(w) = t.split_whitespace().next().filter(|w| is_or(w)) {
            t = t[w.len()..].trim_start();
        }
        if t == before {
            break;
        }
    }
    let mut out = t.to_string();
    if out.matches('"').count() % 2 == 1 {
        out.push('"');
    }
    out
}

pub(super) fn quick_rule(flags: u8, stars_min: u8, labels: u8) -> Vec<String> {
    let mut out = Vec::new();
    let f: Vec<&str> = [(FLAG_PICKED, "flag:picked"), (FLAG_NONE, "flag:none"), (FLAG_REJECTED, "flag:rejected")].iter().filter(|(b, _)| flags & b != 0).map(|(_, t)| *t).collect();
    if !f.is_empty() {
        out.push(f.join(" | "));
    }
    if stars_min != 0 {
        out.push(format!("rating:>={stars_min}"));
    }
    let l: Vec<String> = ["red", "yellow", "green", "blue", "purple"].iter().enumerate().filter(|(i, _)| labels & (1 << (i + 1)) != 0).map(|(_, n)| format!("label:{n}")).collect();
    if !l.is_empty() {
        out.push(l.join(" | "));
    }
    out
}

/// Which column chips fit into `room` (widths of the chips in order, gap 5, "+N" needs `more_w`): the number
/// of chips shown; the rest is counted in "+N".
pub(super) fn chips_fitting(widths: &[f32], room: f32, more_w: f32) -> usize {
    let mut used = 0.0;
    for (i, w) in widths.iter().enumerate() {
        let reserve = if i + 1 < widths.len() { more_w } else { 0.0 };
        if used + w + reserve > room {
            return i;
        }
        used += w + 5.0;
    }
    widths.len()
}

impl LibraryView {
    pub(super) fn filter_bar(&mut self, ui: &mut egui::Ui) {
        let before = self.filter.clone();
        let sort_before = self.sort_order;
        let id = egui::Id::unique("bib-suchfeld");
        let width = self.centre_width.min(ui.available_width());
        // ⌘F: into the search field
        if crate::shortcuts::pressed(ui.ctx(), "search") || search_suggest::TEST_FOCUS.swap(false, Ordering::Relaxed) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let (search_w, filter_w) = split_row1(width);
        ui.allocate_ui_with_layout(egui::vec2(width, lr::BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.x = 0.0;
            // in its own child: the text edit placed inside the frame must not move the row's cursor back
            ui.allocate_ui_with_layout(egui::vec2(search_w, lr::BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| self.search_field(ui, id, search_w));
            ui.add_space(8.0);
            self.filter_button(ui, filter_w);
            ui.add_space(4.0);
            self.filter_reset(ui);
            ui.add_space(8.0);
            if self.view_switch(ui, false) == Some(true) {
                // one photo of a series: the whole series (as N); a selection: as it is
                self.request_compare(self.button_series(), None);
            }
        });
        if self.columns_open {
            self.facets_panel(ui);
        }
        ui.add_space(6.0);
        self.quick_row(ui, width);
        self.active_chips(ui, width, id);
        if self.filter != before || self.sort_order != sort_before {
            // Typing alone does not rebuild the grid, the search runs in the background
            let only_text = Filter { text: self.filter.text.clone(), ..before.clone() } == self.filter && self.sort_order == sort_before;
            if only_text {
                self.search_typed();
            } else {
                self.refilter = true;
            }
        }
        ui.add_space(6.0);
    }

    /// Search field `field_w` wide: magnifier on the left; on the right, inside the frame, the "?" for the syntax
    /// (8 pt from the edge) and left of it the × to clear; the text never runs under them. Placeholder just
    /// "Search", suggestions for the token under the cursor as an overlay below it.
    fn search_field(&mut self, ui: &mut egui::Ui, id: egui::Id, field_w: f32) {
        let suggestions = if ui.memory(|m| m.has_focus(id)) {
            let cursor = search_suggest::cursor_byte(ui.ctx(), id, &self.filter.text);
            self.suggestions(&ui.ctx().clone(), cursor)
        } else {
            Vec::new()
        };
        if !suggestions.is_empty() {
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.suggestion = (self.suggestion + 1).min(suggestions.len());
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.suggestion = self.suggestion.saturating_sub(1);
                }
            });
        } else {
            self.suggestion = 0;
        }
        let (frame, frame_resp) = ui.allocate_exact_size(egui::vec2(field_w, lr::BAR_H), egui::Sense::hover());
        let focus = ui.memory(|m| m.has_focus(id));
        ui.painter().rect(frame, 0.0, crate::lr::gray(0x1c), egui::Stroke::new(1.0, if focus { crate::lr::gray(0x9a) } else { crate::lr::gray(0x4a) }), egui::StrokeKind::Inside);
        let loupe = egui::pos2(frame.left() + 14.0, frame.center().y - 1.0);
        let lf = egui::Stroke::new(1.4, if focus { lr::TEXT() } else { lr::TEXT_DIM() });
        ui.painter().circle_stroke(loupe, 4.5, lf);
        ui.painter().line_segment([loupe + egui::vec2(3.4, 3.4), loupe + egui::vec2(7.5, 7.5)], lf);
        let empty = self.filter.text.is_empty();
        // "?" right-aligned inside the field: syntax and suggestions explained in a card
        let help = egui::Rect::from_center_size(egui::pos2(frame.right() - 8.0 - 8.0, frame.center().y), egui::vec2(16.0, 16.0));
        let mut help_ui = ui.new_child(egui::UiBuilder::new().max_rect(help).layout(egui::Layout::left_to_right(egui::Align::Center)));
        crate::help::info(&mut help_ui, "suche", "Suche", &crate::help::SEARCH);
        let input = egui::Rect::from_min_max(frame.min + egui::vec2(28.0, 2.0), frame.max - egui::vec2(if empty { 32.0 } else { 50.0 }, 2.0));
        let r = ui.put(
            input,
            egui::TextEdit::singleline(&mut self.filter.text)
                .id(id)
                .frame(egui::Frame::NONE)
                .font(egui::FontId::proportional(12.5))
                .text_color(lr::TEXT())
                .vertical_align(egui::Align::Center)
                .hint_text(egui::RichText::new(crate::tr("Suche")).color(lr::TEXT_DIM()))
                // Tab takes a suggestion instead of moving the focus on
                .lock_focus(!suggestions.is_empty())
                .desired_width(input.width()),
        );
        if !empty && self.sem_view.busy(&self.sem_for) && library::settings::value("semanticSearch").as_bool().unwrap_or(true) {
            crate::semantic_ui::busy_mark(ui, frame, true);
        }
        // The full-text search is running (thin strip at the lower edge)
        if !empty && self.search_job.busy() {
            search_motion::busy_strip(ui.painter(), frame, ui.input(|i| i.time));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        if !empty {
            let cross = egui::Rect::from_center_size(egui::pos2(help.left() - 14.0, frame.center().y), egui::vec2(18.0, 18.0));
            let resp = ui.interact(cross, id.with("leeren"), egui::Sense::click());
            let kf = egui::Stroke::new(1.4, if resp.hovered() { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() });
            let c = cross.center();
            ui.painter().line_segment([c + egui::vec2(-3.5, -3.5), c + egui::vec2(3.5, 3.5)], kf);
            ui.painter().line_segment([c + egui::vec2(-3.5, 3.5), c + egui::vec2(3.5, -3.5)], kf);
            if resp.clicked() {
                self.filter.text.clear();
                ui.memory_mut(|m| m.request_focus(id));
            }
        }
        let mut take: Option<Suggestion> = None;
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && self.suggestion > 0 {
            take = suggestions.get(self.suggestion - 1).cloned();
        }
        // Tab: the highlighted suggestion, otherwise the first
        if r.has_focus() && !suggestions.is_empty() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            take = suggestions.get(self.suggestion.max(1) - 1).cloned();
        }
        if !suggestions.is_empty() {
            let pos = frame.left_bottom() + egui::vec2(0.0, 2.0);
            egui::Area::new(id.with("vorschlaege")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
                // exactly as wide as the field (never over the "?" beside it); height follows the rows, at most
                // `ROWS` – an overlay, so nothing in the layout moves
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).stroke(egui::Stroke::new(1.0, crate::lr::gray(0x4a))).show(ui, |ui| {
                    ui.set_width((field_w - 10.0).max(0.0));
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (n, s) in suggestions.iter().enumerate() {
                        if search_suggest::row(ui, s, self.suggestion == n + 1).clicked() {
                            take = Some(s.clone());
                        }
                    }
                });
            });
        }
        if let Some(s) = take {
            let cursor = search_suggest::apply(&mut self.filter.text, &s);
            search_suggest::set_cursor(ui.ctx(), id, cursor);
            self.suggestion = 0;
            // the field keeps the focus (Tab must not move on to the next control)
            ui.memory_mut(|m| {
                m.request_focus(id);
                m.move_focus(egui::FocusDirection::None);
            });
        }
        if self.filter.text.is_empty() && !focus {
            lr::tip_key(ui, &frame_resp, "Suchen, auch nach Feldern wie iso:200", "search");
        }
    }

    /// The metadata columns that filter right now: (title key, values). Also while the columns are being
    /// rebuilt in the background (then from the remembered selection), so the button never flickers.
    pub(super) fn filter_columns(&self) -> Vec<(&'static str, String)> {
        match (&self.facets, &self.facets_job) {
            (Some(f), _) => f.active_columns(),
            (None, Some(job)) => crate::library_filter::remembered_columns(&job.remembered),
            (None, None) => Vec::new(),
        }
    }

    /// Button FILTER (`w` wide): opens and closes the metadata panel. Funnel, word, count badge and the
    /// names of the filtering columns as chips; blue while metadata filters are active, a mark at the lower
    /// edge while the panel is open.
    fn filter_button(&mut self, ui: &mut egui::Ui, w: f32) {
        let columns = self.filter_columns();
        let n = columns.len();
        let open = self.columns_open;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, lr::BAR_H), egui::Sense::click());
        let hovered = resp.hovered();
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        // same surface as the other groups; active filters lay the blue wash over it, the open panel the mark
        // at the lower edge like an active cell
        painter.rect_filled(rect, 0.0, if hovered && n == 0 { crate::lr::gray(0x2e) } else { lr::GROUP_FILL() });
        if n > 0 {
            painter.rect_filled(rect, 0.0, lr::BLUE().gamma_multiply(if hovered { WASH_HOVER } else { WASH }));
        }
        if open {
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.max), 0.0, lr::ACTIVE_MARK());
        }
        let fg = if hovered || open { crate::lr::TEXT_STRONG() } else { lr::TEXT() };
        let icon = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(16.0, 16.0));
        lr::paint_icon(&painter, icon, lr::Icon::Funnel, fg);
        let word = lr::caps(&painter, crate::tr("Filter"), egui::FontId::proportional(11.5), fg);
        let mut x = icon.right() + 8.0;
        painter.galley(egui::pos2(x, rect.center().y - word.size().y / 2.0), word.clone(), fg);
        x += word.size().x + 8.0;
        if n > 0 {
            let g = painter.layout_no_wrap(n.to_string(), egui::FontId::proportional(10.5), crate::lr::TEXT_STRONG());
            let badge = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 8.5), egui::vec2((g.size().x + 8.0).max(17.0), 17.0));
            painter.rect_filled(badge, 0.0, lr::BLUE());
            painter.galley(badge.center() - g.size() / 2.0, g, crate::lr::TEXT_STRONG());
            x = badge.right() + 8.0;
            // chips with the column names, as many as fit; "+N" for the rest
            let room = rect.right() - 8.0 - x;
            let font = egui::FontId::proportional(11.0);
            let galleys: Vec<_> = columns.iter().map(|(t, _)| painter.layout_no_wrap(crate::tr(t).to_string(), font.clone(), lr::TEXT())).collect();
            let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 14.0).collect();
            let shown = chips_fitting(&widths, room, 36.0);
            for (g, cw) in galleys.into_iter().zip(&widths).take(shown) {
                let chip = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 10.0), egui::vec2(*cw, 20.0));
                painter.rect_filled(chip, 0.0, lr::BLUE().gamma_multiply(CHIP_WASH));
                painter.galley(egui::pos2(chip.left() + 7.0, chip.center().y - g.size().y / 2.0), g, lr::TEXT());
                x = chip.right() + 5.0;
            }
            if shown < n && room > 0.0 {
                let g = painter.layout_no_wrap(format!("+{}", n - shown), font, lr::TEXT_DIM());
                let chip = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 10.0), egui::vec2(g.size().x + 14.0, 20.0));
                painter.galley(egui::pos2(chip.left() + 7.0, chip.center().y - g.size().y / 2.0), g, lr::TEXT_DIM());
            }
        }
        // tooltip: what the button does, with active filters the columns with their values
        let tip = if columns.is_empty() {
            crate::tr("Metadaten-Filter: Kamera, Objektiv, ISO …").to_string()
        } else {
            let mut t = crate::tr("Metadaten-Filter").to_string();
            for (title, values) in &columns {
                t.push('\n');
                t.push_str(&format!("{}: {values}", crate::tr(title)));
            }
            t
        };
        lr::hover_tip(ui, &resp, &tip);
        if resp.clicked() {
            self.columns_open = !open;
            self.refilter = true;
        }
    }

    /// Reset of the metadata filters only (search and quick filters stay); dimmed while none is active.
    fn filter_reset(&mut self, ui: &mut egui::Ui) {
        let usable = !self.filter_columns().is_empty();
        let tip = crate::tr("Metadaten-Filter zurücksetzen");
        let cells = [(lr::Cell::Symbol(lr::Icon::X, lr::TEXT()), false, tip)];
        let r = ui.allocate_ui_with_layout(egui::vec2(RESET_W, lr::BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_enabled_ui(usable, |ui| {
                if !usable {
                    ui.multiply_opacity(0.35);
                }
                lr::cell_group(ui, "bib-meta-reset", &cells, RESET_W)
            })
            .inner
        });
        if r.inner.is_some() && usable {
            self.reset_metadata_filters();
        }
    }

    /// Clears the selection in all metadata columns (also one waiting for its rebuilt columns).
    pub(super) fn reset_metadata_filters(&mut self) {
        if let Some(f) = &mut self.facets {
            f.reset();
        }
        if let Some(job) = &mut self.facets_job {
            job.remembered = Default::default();
        }
        self.refilter = true;
    }

    /// The metadata panel below row 1 over the full width (columns as before; built in the background).
    fn facets_panel(&mut self, ui: &mut egui::Ui) {
        self.facets_ready();
        if let (None, Some(job)) = (&self.facets, &self.facets_job) {
            // someone is waiting for the lists now
            job.hurry();
        }
        if self.facets.is_none() {
            // being built in the background: the frame stays, nothing jumps when the lists appear
            ui.add_space(6.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), crate::library_filter::HEIGHT + 22.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 0.0, crate::lr::gray(0x16));
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, crate::tr("Filter werden geladen …"), egui::FontId::proportional(10.5), lr::TEXT_DIM());
        }
        if let Some(f) = &mut self.facets {
            ui.add_space(6.0);
            let t_ui = crate::frame_watch::section("facets: ui");
            let changed = ui.allocate_ui(egui::vec2(ui.available_width(), crate::library_filter::HEIGHT + 26.0), |ui| f.ui(ui)).inner;
            drop(t_ui);
            let request = f.take_keyword_request();
            if changed {
                self.refilter = true;
            }
            // Keywords are managed in the keyword column (formerly the left panel)
            if let Some((id, action)) = request {
                self.keyword_action(id, action);
            }
        }
    }

    /// Row 2 over `width`: the quick-filter groups from the left (flags, stars, labels, kind, RAW+JPEG), sort order
    /// and direction right-aligned at the end of the last line. Too narrow for everything on one line: the groups
    /// flow onto further lines of the same height ([`quick_layout`]) – decided by the width alone, so nothing
    /// jumps while working.
    pub(super) fn quick_row(&mut self, ui: &mut egui::Ui, width: f32) {
        let top = ui.cursor().min;
        let (places, sort_at, height) = quick_layout(width);
        for (i, at) in places.iter().enumerate() {
            let r = egui::Rect::from_min_size(top + at.to_vec2(), egui::vec2(QUICK_GROUPS[i], lr::BAR_H));
            ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| match i {
                0 => self.flag_group(ui),
                1 => self.star_group(ui),
                2 => self.label_group(ui),
                3 => self.kind_group(ui),
                _ => self.stack_group(ui),
            });
        }
        let sort = egui::Rect::from_min_size(top + sort_at.to_vec2(), egui::vec2(SORT_W, lr::BAR_H));
        ui.scope_builder(egui::UiBuilder::new().max_rect(sort).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| self.sort_controls(ui));
        ui.allocate_rect(egui::Rect::from_min_size(top, egui::vec2(width, height)), egui::Sense::hover());
    }

    /// Sort order with direction (right part of row 2; laid out right to left).
    fn sort_controls(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (arrow, hint) = if self.sort_reversed { (lr::Icon::SortUp, "Aufsteigend sortiert · Klick kehrt um") } else { (lr::Icon::SortDown, "Absteigend sortiert · Klick kehrt um") };
        if lr::cell_group(ui, "bib-sort-richtung", &[(lr::Cell::Symbol(arrow, lr::TEXT()), false, hint)], 30.0).is_some() {
            self.sort_reversed = !self.sort_reversed;
            self.refilter = true;
        }
        let name = |m: SortOrder| match m {
            SortOrder::CaptureTime => crate::tr("Aufnahme"),
            SortOrder::Name => crate::tr("Dateiname"),
            SortOrder::Stars => crate::tr("Sterne"),
        };
        ui.add_space(1.0);
        ui.scope(|ui| {
            lr::field_style(ui);
            // a viewed folder has no capture times until "Aufnahme" is chosen: until then it is ordered by name, and
            // the field says so
            let shown = if self.viewed_folder().is_some() && !self.view_sort_chosen { SortOrder::Name } else { self.sort_order };
            egui::ComboBox::from_id_salt("bib-sort").selected_text(name(shown)).width(124.0).height(240.0).show_ui(ui, |ui| {
                for m in [SortOrder::CaptureTime, SortOrder::Name, SortOrder::Stars] {
                    if ui.selectable_label(m == shown, name(m)).clicked() {
                        self.sort_order = m;
                        self.view_sort_chosen = true;
                    }
                }
            });
        });
    }

    /// Flags (several can be combined).
    fn flag_group(&mut self, ui: &mut egui::Ui) {
        let f = self.filter.flags;
        let cells = [
            (lr::Cell::Flag(1), f & FLAG_PICKED != 0, "Nur markierte"),
            (lr::Cell::Flag(0), f & FLAG_NONE != 0, "Nur ohne Markierung"),
            (lr::Cell::Flag(-1), f & FLAG_REJECTED != 0, "Nur abgelehnte"),
        ];
        if let Some(i) = lr::cell_group(ui, "bib-flaggen", &cells, 30.0) {
            self.filter.flags ^= [FLAG_PICKED, FLAG_NONE, FLAG_REJECTED][i];
        }
    }

    /// At least n stars (five outline stars; a second click on the same star clears).
    fn star_group(&mut self, ui: &mut egui::Ui) {
        let n = self.filter.stars_min;
        let tips: Vec<String> = (1..=5).map(|k| if k == 1 { crate::tr("Mindestens 1 Stern").to_string() } else { crate::trf!("Mindestens {n} Sterne", n = k) }).collect();
        let cells: Vec<_> = (1..=5u8)
            .map(|k| {
                let on = n >= k;
                (lr::Cell::Text(if on { "★" } else { "☆" }, 14.0, if on { crate::lr::gray(0xee) } else { crate::lr::gray(0x8a) }), false, tips[k as usize - 1].as_str())
            })
            .collect();
        if let Some(i) = lr::cell_group(ui, "bib-sterne", &cells, 22.0) {
            let k = i as u8 + 1;
            self.filter.stars_min = if n == k { 0 } else { k };
        }
    }

    /// Color labels (several).
    fn label_group(&mut self, ui: &mut egui::Ui) {
        let names = [crate::tr("Etikett Rot"), crate::tr("Etikett Gelb"), crate::tr("Etikett Grün"), crate::tr("Etikett Blau"), crate::tr("Etikett Lila")];
        let cells: Vec<_> = (1..=5u8)
            .map(|e| {
                let c = library::label::COLORS[e as usize];
                (lr::Cell::Color(Color32::from_rgb(c[0], c[1], c[2])), self.filter.labels & (1 << e) != 0, names[e as usize - 1])
            })
            .collect();
        if let Some(i) = lr::cell_group(ui, "bib-etiketten", &cells, 24.0) {
            self.filter.labels ^= 1 << (i + 1);
        }
    }

    /// Photos, videos or both (exactly one).
    fn kind_group(&mut self, ui: &mut egui::Ui) {
        let cells = [
            (lr::Cell::Symbol(lr::Icon::Image, lr::TEXT()), self.kind == 0, "Nur Fotos"),
            (lr::Cell::Symbol(lr::Icon::Video, lr::TEXT()), self.kind == 1, "Nur Videos"),
            (lr::Cell::Symbol(lr::Icon::Images, lr::TEXT()), self.kind == 2, "Fotos und Videos"),
        ];
        if let Some(i) = lr::cell_group(ui, "bib-art", &cells, 30.0) {
            if i as u8 != self.kind {
                self.kind = i as u8;
                library::settings::set("libraryKind", serde_json::json!(self.kind));
                self.refilter = true;
            }
        }
    }

    /// RAW+JPEG as one photo or as two.
    fn stack_group(&mut self, ui: &mut egui::Ui) {
        let hint = if self.stack { "RAW+JPEG zusammen (ein Foto)" } else { "RAW+JPEG einzeln (zwei Fotos)" };
        // second cell: only merged (HDR) photos – no stacks, no sources
        let cells = [(lr::Cell::Symbol(lr::Icon::Stack, lr::TEXT()), self.stack, hint), (lr::Cell::Text("HDR", 11.0, lr::TEXT()), self.filter.hdr, "Nur zusammengefügte Fotos (HDR)")];
        match lr::cell_group_w(ui, "bib-raw-jpeg", &cells, &[30.0, 38.0]) {
            Some(0) => {
                self.stack = !self.stack;
                library::settings::set("rawJpegStacks", serde_json::json!(self.stack));
                self.refilter = true;
            }
            Some(_) => {
                self.filter.hdr = !self.filter.hdr;
                self.refilter = true;
            }
            None => {}
        }
    }

    /// Is the chip row there (search words, quick filters or a special source)?
    fn chips_shown(&self) -> bool {
        self.filter != Filter::default() || self.source_chip().is_some()
    }

    /// Row of the active search words and quick filters as chips (× removes one); exists only while something
    /// filters. The metadata filters show on the FILTER button, not here;
    /// "Save as smart collection" sits in the status row ([`Self::status_left`]).
    fn active_chips(&mut self, ui: &mut egui::Ui, width: f32, id: egui::Id) {
        if !self.chips_shown() {
            return;
        }
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(egui::vec2(width, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.x = 8.0;
            let buttons_w = 0.0;
            // Where is searched? Only visible when a folder/collection is selected
            if !self.filter.text.trim().is_empty() && matches!(self.source, Source::Folder(..) | Source::Collection(_) | Source::Keyword(_)) {
                let name = self.source_name();
                let short: String = if name.chars().count() > 26 { format!("{}…", name.chars().take(25).collect::<String>()) } else { name };
                let here = crate::menu::i18n::fill_names(crate::tr("Nur in „{a}“"), &[("a", &short)]);
                if lr::segmented(ui, &mut self.search_in_source, &[(false, crate::tr("Gesamte Bibliothek")), (true, here.as_str())]) {
                    self.refilter = true;
                }
            }
            let (region, _) = ui.allocate_exact_size(egui::vec2((ui.available_width() - buttons_w - 8.0).max(80.0), 22.0), egui::Sense::hover());
            // chip sources: search components and the quick filters
            enum ChipSource {
                Word(usize),
                Flags,
                Stars,
                Labels,
                Hdr,
            }
            let toks = raw_tokens(&self.filter.text);
            let mut chips: Vec<(ChipSource, String, Color32)> = Vec::new();
            for (n, t) in toks.iter().enumerate() {
                // a field still without value (`camera:""` while typing) is no filter yet
                if t == "|" || t == "ODER" || t.trim_start_matches('-').split_once([':', '：']).is_some_and(|(k, v)| v.trim_matches('"').is_empty() && library::search_fields::field_id(k).is_some()) {
                    continue;
                }
                let (text, color) = chip_text(t);
                chips.push((ChipSource::Word(n), text, color));
            }
            if self.filter.flags != 0 {
                chips.push((ChipSource::Flags, flags_text(self.filter.flags), Color32::from_rgb(0x4a, 0x42, 0x2c)));
            }
            if self.filter.stars_min != 0 {
                chips.push((ChipSource::Stars, format!("≥ {}", "★".repeat(self.filter.stars_min as usize)), Color32::from_rgb(0x4a, 0x42, 0x2c)));
            }
            if self.filter.labels != 0 {
                chips.push((ChipSource::Labels, format!("{} {}", crate::tr("Etikett:"), labels_text(self.filter.labels)), Color32::from_rgb(0x4a, 0x42, 0x2c)));
            }
            if self.filter.hdr {
                chips.push((ChipSource::Hdr, crate::tr("Nur HDR").to_string(), Color32::from_rgb(0x4a, 0x42, 0x2c)));
            }
            let clip = ui.painter().with_clip_rect(region);
            let mut x = region.left();
            // first the special source ("Similar to …", "Duplicates"): × leaves it (library_views.rs)
            let mut leave = false;
            if let Some((right, clicked)) = self.paint_source_chip(ui, region, x, id) {
                x = right;
                leave = clicked;
            }
            let mut remove: Option<usize> = None;
            for (n, (_, text, color)) in chips.iter().enumerate() {
                let g = clip.layout_no_wrap(text.clone(), egui::FontId::proportional(11.0), crate::lr::TEXT_STRONG());
                let r = egui::Rect::from_min_size(egui::pos2(x, region.top() + 1.0), egui::vec2(g.size().x + 28.0, 20.0));
                if r.left() > region.right() {
                    break;
                }
                clip.rect_filled(r, 0.0, *color);
                clip.galley(r.left_center() + egui::vec2(9.0, -g.size().y / 2.0), g, crate::lr::TEXT_STRONG());
                let cross = egui::Rect::from_center_size(egui::pos2(r.right() - 10.0, r.center().y), egui::vec2(14.0, 14.0));
                let resp = ui.interact(cross.intersect(region), id.with(("chip", n)), egui::Sense::click());
                clip.text(cross.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(12.0), if resp.hovered() { crate::lr::TEXT_STRONG() } else { crate::lr::gray(0xc4) });
                if resp.clicked() {
                    remove = Some(n);
                }
                x = r.right() + 5.0;
            }
            if leave {
                self.leave_special_source();
            }
            if let Some(n) = remove {
                match chips[n].0 {
                    ChipSource::Word(i) => {
                        let mut t = toks.clone();
                        t.remove(i);
                        self.filter.text = t.iter().map(|x| if x.contains(' ') && !x.contains('"') { format!("\"{x}\"") } else { x.clone() }).collect::<Vec<_>>().join(" ");
                    }
                    ChipSource::Flags => self.filter.flags = 0,
                    ChipSource::Stars => self.filter.stars_min = 0,
                    ChipSource::Labels => self.filter.labels = 0,
                    ChipSource::Hdr => self.filter.hdr = false,
                }
            }
        });
    }

    /// Does a filter hide every photo of the source (then [`Self::no_matches`] stands in the empty grid)?
    pub(super) fn no_matches_shown(&self) -> bool {
        self.visible.is_empty() && !self.refilter && !self.first_loading() && self.db.is_some() && (self.filter != Filter::default() || !self.filter_columns().is_empty())
    }

    /// Empty state with an active filter, centred in the empty grid: heading, one dimmed line with what filters,
    /// the way out as buttons of the bar's height (spacing as in the empty library).
    pub(super) fn no_matches(&mut self, ui: &mut egui::Ui) {
        let mut reasons: Vec<String> = Vec::new();
        if !self.filter.text.trim().is_empty() {
            let where_text = if self.search_global() || matches!(self.source, Source::All) { crate::tr("in der gesamten Bibliothek").to_string() } else { crate::menu::i18n::fill_names(crate::tr("in „{a}“"), &[("a", &self.source_name())]) };
            reasons.push(crate::trf!("Suche „{a}“ {b}", a = self.filter.text.trim(), b = where_text));
        }
        if self.filter.flags != 0 {
            reasons.push(flags_text(self.filter.flags));
        }
        if self.filter.stars_min != 0 {
            reasons.push(format!("≥ {}", "★".repeat(self.filter.stars_min as usize)));
        }
        if self.filter.labels != 0 {
            reasons.push(crate::trf!("Etikett: {a}", a = labels_text(self.filter.labels)));
        }
        let columns = self.filter_columns();
        if !columns.is_empty() {
            let names: Vec<&str> = columns.iter().map(|(t, _)| crate::tr(t)).collect();
            reasons.push(crate::trf!("Metadaten-Filter: {a}", a = names.join(", ")));
        }
        // heading, gap, reason, gap, buttons – the block sits a little above the middle of the free area
        let block = 18.0 + 6.0 + 16.0 + 18.0 + lr::BAR_H;
        ui.add_space(((ui.available_height() - block) * 0.42).max(24.0));
        let width = ui.available_width();
        ui.vertical_centered(|ui| {
            // without a search index (being built in the background) the full text finds nothing yet: say so
            let indexing = !self.filter.text.trim().is_empty() && super::INDEX_MISSING.load(Ordering::Relaxed);
            let heading = if indexing { crate::tr("Suchindex wird aufgebaut …") } else { crate::tr("Keine Treffer") };
            ui.label(egui::RichText::new(heading).font(lr::semibold(14.0)).color(lr::TEXT()));
            ui.add_space(6.0);
            ui.add(egui::Label::new(egui::RichText::new(reasons.join("  ·  ")).size(lr::UI_SIZE).color(lr::TEXT_DIM())).truncate());
            ui.add_space(18.0);
        });
        // buttons centred as one row (their widths from the text, so the row is centred exactly)
        let font = egui::FontId::proportional(lr::UI_SIZE);
        let mut labels = vec![crate::tr("Filter zurücksetzen")];
        if self.search_in_source {
            labels.push(crate::tr("In der gesamten Bibliothek suchen"));
        }
        let widths: Vec<f32> = labels.iter().map(|t| ui.painter().layout_no_wrap(t.to_string(), font.clone(), lr::TEXT()).size().x + 28.0).collect();
        let total = widths.iter().sum::<f32>() + 8.0 * (widths.len() as f32 - 1.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(((width - total) / 2.0).max(0.0));
            for (i, (t, w)) in labels.iter().zip(&widths).enumerate() {
                if ui.add(egui::Button::new(egui::RichText::new(*t).font(font.clone())).min_size(egui::vec2(*w, lr::BAR_H))).clicked() {
                    if i == 0 {
                        self.reset_all_filters();
                    } else {
                        self.search_in_source = false;
                        self.refilter = true;
                    }
                }
            }
        });
    }

    /// Is anything filtering the view (search, metadata columns, quick filters)?
    pub(super) fn filtering(&self) -> bool {
        self.filter != Filter::default() || !self.filter_columns().is_empty()
    }

    /// All filters of the view as a smart-collection rule in the app's search syntax: search text, metadata columns,
    /// flags, stars, labels (space = and, `|` = or). `Err`: why the view cannot be saved as a rule (the source on
    /// the left is no rule, or the columns are still being built).
    pub(super) fn smart_rule(&self) -> Result<String, String> {
        let mut parts: Vec<String> = Vec::new();
        match &self.source {
            Source::All => {}
            // a smart collection narrowed further: its own rule and the filters
            Source::Smart(id) => {
                if let Some(r) = self.collections.iter().find(|c| c.id == *id).and_then(|c| c.search.clone()) {
                    parts.push(r);
                }
            }
            // the search runs in the whole library: the folder or collection on the left does not count
            _ if self.search_global() => {}
            _ => return Err(crate::trf!("„{a}“ lässt sich nicht als Regel sichern: wähle links „Alle Fotos“", a = self.source_name())),
        }
        if self.filter.hdr {
            return Err(crate::tr("Der HDR-Filter lässt sich nicht als Regel sichern").to_string());
        }
        let text = rule_text(&self.filter.text);
        if !text.is_empty() {
            parts.push(text);
        }
        match &self.facets {
            Some(f) => {
                let t = f.as_search()?;
                if !t.is_empty() {
                    parts.push(t);
                }
            }
            None if !self.filter_columns().is_empty() => return Err(crate::tr("Metadaten-Filter werden noch geladen").to_string()),
            None => {}
        }
        parts.extend(quick_rule(self.filter.flags, self.filter.stars_min, self.filter.labels));
        Ok(parts.join(" "))
    }

    /// Name proposed for the smart collection: what filters, short ("Kamera: SIGMA DP3 Merrill · ≥ ★★★").
    pub(super) fn smart_name_proposal(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.filter.text.trim().is_empty() {
            parts.push(self.filter.text.trim().to_string());
        }
        for (title, values) in self.filter_columns() {
            parts.push(format!("{}: {values}", crate::tr(title)));
        }
        if self.filter.flags != 0 {
            parts.push(flags_text(self.filter.flags));
        }
        if self.filter.stars_min != 0 {
            parts.push(format!("≥ {}", "★".repeat(self.filter.stars_min as usize)));
        }
        if self.filter.labels != 0 {
            parts.push(labels_text(self.filter.labels));
        }
        let name = parts.join(" · ");
        if name.chars().count() > 60 { format!("{}…", name.chars().take(59).collect::<String>()) } else { name }
    }

    /// Left part of the status row: the count, and while something filters a quiet text button "Save these photos
    /// as a smart collection" right behind it (shorter when room is short). Dimmed with the reason as tooltip when
    /// the view is no rule. The row keeps its height either way.
    pub(super) fn status_left(&mut self, ui: &mut egui::Ui, left_text: String) {
        let font = egui::FontId::proportional(lr::UI_SIZE);
        if !self.filtering() {
            ui.add(egui::Label::new(egui::RichText::new(left_text).font(font).color(lr::TEXT_DIM())).truncate());
            return;
        }
        let rule = self.smart_rule();
        let count_w = ui.painter().layout_no_wrap(left_text.clone(), font.clone(), lr::TEXT_DIM()).size().x;
        // Only the symbol, never a sentence; the text lives in the tooltip.
        let text_w = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), lr::TEXT()).size().x + 16.0;
        let label: Option<&str> = None;
        let button_w = label.map_or(ACTION_W, text_w);
        let count_room = (ui.available_width() - button_w - 12.0).max(0.0);
        ui.allocate_ui_with_layout(egui::vec2(count_room.min(count_w + 1.0), ACTION_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(left_text).font(font.clone()).color(lr::TEXT_DIM())).truncate());
        });
        ui.add_space(12.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(button_w, ACTION_H), egui::Sense::click());
        let usable = rule.is_ok();
        let hot = usable && resp.hovered();
        ui.painter().rect_filled(rect, 0.0, if hot { crate::lr::gray(0x36) } else { lr::BUTTON_FILL() });
        let color = if !usable { crate::lr::gray(0x6a) } else if hot { crate::lr::TEXT_STRONG() } else { lr::TEXT() };
        match label {
            Some(t) => {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, t, font, color);
            }
            None => lr::paint_icon(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(15.0, 15.0)), lr::Icon::Stack, color),
        }
        let tip = match &rule {
            Ok(_) => {
                let mut t = if label.is_none() { format!("{}\n", crate::tr("Diese Fotos als Smart-Sammlung sichern")) } else { String::new() };
                t.push_str(crate::tr("Sichert die Filter als Regel: neue passende Fotos kommen von selbst hinzu"));
                if self.kind != 0 {
                    t.push('\n');
                    t.push_str(crate::tr("Fotos/Videos bleibt eine Einstellung der Ansicht, keine Regel"));
                }
                t
            }
            Err(why) => why.clone(),
        };
        lr::hover_tip(ui, &resp, &tip);
        if resp.clicked() && usable {
            self.smart_name = self.smart_name_proposal();
            self.smart_window = true;
        }
    }

    /// Menu "Zur Auswahl springen": the active photo goes to the middle of the grid.
    pub fn request_jump(&mut self) {
        self.jump_request = true;
    }

    /// Is the active photo outside the visible part of the grid? (Only known once the grid has been drawn.)
    pub(super) fn active_outside_view(&self) -> bool {
        let Some(active) = self.active else { return false };
        if self.grid_cell <= 0.0 || self.grid_view_h <= 0.0 || self.columns == 0 {
            return false;
        }
        let Some(pos) = self.visible.iter().position(|&i| self.rows[i].image == active) else { return false };
        let first = self.sem_from.unwrap_or(self.visible.len());
        let row = crate::grid_jump::row_of(pos, self.columns, first, self.sem_from.is_some());
        let layout = crate::grid_jump::Rows::new(first, self.columns, self.sem_from.is_some(), self.grid_cell, 2.0);
        !crate::grid_jump::in_view(layout.top(row), self.grid_cell - 2.0, self.grid_y, self.grid_view_h)
    }

    /// The actions on the selection at the right end of the status row (call in a right-to-left layout):
    /// turn left, turn right, jump to the selection. Dimmed while nothing is selected; the jump is highlighted
    /// while the active photo is out of view.
    pub(super) fn selection_actions(&mut self, ui: &mut egui::Ui) {
        let usable = !self.nothing_selected && self.active.is_some();
        // right to left: the jump first, so it ends up at the far right
        let outside = usable && self.active_outside_view();
        let tip = lr::with_shortcut("Zur Auswahl springen", "jump_to_selection");
        let cells = [(lr::Cell::Symbol(lr::Icon::Crosshair, lr::TEXT()), outside, tip.as_str())];
        let jump = ui
            .allocate_ui_with_layout(egui::vec2(ACTION_W, ACTION_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_enabled_ui(usable, |ui| {
                    if !outside {
                        ui.multiply_opacity(0.35);
                    }
                    lr::cell_group_sized(ui, "bib-zur-auswahl", &cells, ACTION_W, ACTION_H).0
                })
                .inner
            })
            .inner;
        if jump.is_some() && usable {
            self.jump_request = true;
        }
        ui.add_space(4.0);
        let (t_l, t_r) = (lr::with_shortcut("Nach links drehen", "rotate_left"), lr::with_shortcut("Nach rechts drehen", "rotate_right"));
        let cells = [(lr::Cell::Symbol(lr::Icon::RotCcw, lr::TEXT()), false, t_l.as_str()), (lr::Cell::Symbol(lr::Icon::RotCw, lr::TEXT()), false, t_r.as_str())];
        let turn = ui
            .allocate_ui_with_layout(egui::vec2(2.0 * ACTION_W, ACTION_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_enabled_ui(usable, |ui| {
                    if !usable {
                        ui.multiply_opacity(0.35);
                    }
                    lr::cell_group_sized(ui, "bib-drehen", &cells, ACTION_W, ACTION_H).0
                })
                .inner
            })
            .inner;
        if let Some(k) = turn.filter(|_| usable) {
            self.turn_request = Some(k == 1);
        }
    }

    /// The button "expand / collapse all series" of the status row (left of turn and jump).
    pub(super) fn series_all_cell(&mut self, ui: &mut egui::Ui) {
        if super::series_all_button(ui, self.series_all(), ACTION_W, ACTION_H) {
            self.series_all_click();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review N3: a half-typed "or" or an open quotation mark must not change the neighbouring parts of the rule.
    #[test]
    fn rule_text_is_self_contained() {
        assert_eq!(rule_text("foo |"), "foo");
        assert_eq!(rule_text("| foo"), "foo");
        assert_eq!(rule_text("  OR foo ODER "), "foo");
        assert_eq!(rule_text("a | b"), "a | b");
        assert_eq!(rule_text("\"x OR\""), "\"x OR\"");
        assert_eq!(rule_text("\"abc"), "\"abc\"");
        let joined = format!("{} rating:>=3", rule_text("foo |"));
        let q = library::search::parse(&joined);
        assert_eq!(q.clauses.len(), 2, "the rule must stay an AND: {q:?}");
    }

    #[test]
    fn row1_halves_share_the_width() {
        for width in (300..=2400).step_by(10) {
            let w = width as f32;
            let (s, f) = split_row1(w);
            assert!(s >= SEARCH_MIN, "search too narrow at {w}: {s}");
            if w >= ROW1_FIXED + SEARCH_MIN + FILTER_MIN {
                assert!(f >= FILTER_MIN, "filter too narrow at {w}: {f}");
                assert!((s + f + ROW1_FIXED - w).abs() < 0.5, "row 1 does not fill {w}: {s} + {f}");
            }
            if w >= ROW1_FIXED + 2.0 * SEARCH_MIN.max(FILTER_MIN) {
                assert!((s - f).abs() < 0.5, "not half and half at {w}: {s} {f}");
            }
        }
    }

    #[test]
    fn row2_flows_without_overlap() {
        for width in (200..=1600).step_by(10) {
            let w = width as f32;
            let (places, sort, height) = quick_layout(w);
            let mut rects: Vec<egui::Rect> = places.iter().zip(QUICK_GROUPS).map(|(p, gw)| egui::Rect::from_min_size(*p, egui::vec2(gw, lr::BAR_H))).collect();
            rects.push(egui::Rect::from_min_size(sort, egui::vec2(SORT_W, lr::BAR_H)));
            for (i, a) in rects.iter().enumerate() {
                assert!(a.right() <= w.max(SORT_W) + 0.1 && a.bottom() <= height + 0.1, "outside at {w}: {a:?}");
                for b in &rects[i + 1..] {
                    assert!(!a.shrink(0.1).intersects(b.shrink(0.1)), "overlap at {w}: {a:?} {b:?}");
                }
            }
        }
        // wide: one line; the narrow centre of a small window: two lines
        assert_eq!(quick_layout(900.0).2, lr::BAR_H);
        assert_eq!(quick_layout(400.0).2, 2.0 * lr::BAR_H + LINE_GAP);
    }

    /// The HDR filter shows merged photos only, combines with the other attributes and clears with them.
    #[test]
    fn hdr_filter_keeps_merged_photos_only() {
        let mut f = Filter::default();
        assert!(!f.attributes_active() && f.attributes_match(0, 0, 0, false));
        f.hdr = true;
        assert!(f.attributes_active());
        assert!(!f.attributes_match(0, 5, 0, false), "an ordinary photo is hidden");
        assert!(f.attributes_match(0, 0, 0, true), "a merged photo stays");
        f.stars_min = 3;
        assert!(!f.attributes_match(0, 2, 0, true), "the other attributes still count");
        f.clear_attributes();
        assert!(!f.hdr && !f.attributes_active());
    }

    /// The quick filters become rule terms the search understands, with the same meaning.
    #[test]
    fn quick_filters_as_rule() {
        assert!(quick_rule(0, 0, 0).is_empty());
        let r = quick_rule(FLAG_PICKED | FLAG_NONE, 3, (1 << 1) | (1 << 4));
        assert_eq!(r, vec!["flag:picked | flag:none".to_string(), "rating:>=3".into(), "label:red | label:blue".into()]);
        assert_eq!(library::search::flag_value("picked"), Some(1));
        assert_eq!(library::search::flag_value("none"), Some(0));
        assert_eq!(library::search::flag_value("rejected"), Some(-1));
        for (i, n) in ["red", "yellow", "green", "blue", "purple"].iter().enumerate() {
            assert_eq!(library::search::label_value(n), Some(i as u8 + 1));
        }
        let q = format!("{:?}", library::search::parse("rating:>=3"));
        assert!(q.contains("rating") && q.contains(">="), "{q}");
    }

    #[test]
    fn column_chips_overflow_into_plus_n() {
        // all fit
        assert_eq!(chips_fitting(&[60.0, 40.0], 200.0, 36.0), 2);
        // the last one needs no room for "+N"
        assert_eq!(chips_fitting(&[60.0, 40.0], 105.0, 36.0), 2);
        // second does not fit with the reserve for "+N" → one chip, "+2"
        assert_eq!(chips_fitting(&[60.0, 40.0, 40.0], 120.0, 36.0), 1);
        // not even the first: only "+N"
        assert_eq!(chips_fitting(&[60.0, 40.0], 50.0, 36.0), 0);
        assert_eq!(chips_fitting(&[], 50.0, 36.0), 0);
    }

    /// Row 2 keeps its five quick-filter groups and the sort direction inside the width, never overlapping,
    /// from narrow to wide windows.
    #[test]
    fn quick_row_groups_do_not_overlap() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut library = LibraryView::new(&ctx);
        for width in (360..=1600).step_by(20) {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1800.0, 600.0))), ..Default::default() };
            let output = crate::ui_test::run(&ctx, input, |ui| {
                ui.allocate_ui(egui::vec2(width as f32, 400.0), |ui| {
                    ui.set_width(width as f32);
                    library.quick_row(ui, width as f32);
                });
            });
            let groups: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.fill == lr::GROUP_FILL() && rect.rect.height() >= lr::BAR_H - 0.5 && rect.rect.width() >= 29.0 => Some(rect.rect),
                    _ => None,
                })
                .collect();
            // flags, stars, labels, kind, RAW+JPEG, sort direction (the dropdown may add its own frame)
            assert!(groups.len() >= 6, "missing groups at width {width}: {groups:?}");
            for (i, first) in groups.iter().enumerate() {
                assert!(first.right() <= width as f32 + 0.1, "outside at width {width}: {first:?}");
                for second in &groups[i + 1..] {
                    assert!(!first.shrink(0.1).intersects(second.shrink(0.1)), "overlap at width {width}: {first:?} {second:?}");
                }
            }
        }
    }
}
