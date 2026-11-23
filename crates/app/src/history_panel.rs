//! Section "Verlauf" in the tool "Presets & History": the steps of the open photo, newest on top, in a list of fixed
//! height. A click jumps to a step (no new step; the later ones stay dimmed and clickable until the next change),
//! hovering shows the step's state in the image (the hover preview of the presets), "Original" jumps to the start
//! state. The history itself: `history.rs`.

use develop::{Settings, View};
use eframe::egui::{self, Color32};

use std::sync::Mutex;

use crate::{history, lr, App};

/// What the context menu of a step asks for.
enum MenuAction {
    Version(usize),
    Compare(usize),
    Compact(usize),
    Delete,
}

/// Open dialogs: "Als Version speichern" (photo, step, name being typed) and "Verlauf dieses Fotos löschen" (photo).
static VERSION_DIALOG: Mutex<Option<(history::Key, usize, String)>> = Mutex::new(None);
static DELETE_DIALOG: Mutex<Option<history::Key>> = Mutex::new(None);

/// Height of one row and number of rows the list always takes (it scrolls inside; nothing below moves).
const ROW_H: f32 = 20.0;
const ROWS: f32 = 10.0;

/// One row as drawn.
struct Row {
    index: usize,
    name: String,
    time: String,
    count: i64,
    parts: Option<String>,
}

const CURRENT: Color32 = Color32::from_gray(0xee);
const BEFORE: Color32 = Color32::from_gray(0xb4);
const AFTER: Color32 = Color32::from_gray(0x66);

impl App {
    /// The section "Verlauf" (drawn after the presets: a hovered step reaches their hover preview in the next frame).
    pub(crate) fn history_section(&mut self, ui: &mut egui::Ui) {
        let open = self.current.as_ref().map(|c| c.index);
        let key = open.map(|i| self.history_key(i));
        let in_library = key.as_ref().and_then(|k| self.history.get(k)).is_some_and(|p| p.in_library());
        let lr_steps: Vec<(String, i64)> = key.as_ref().and_then(|k| self.history.get(k)).map(|p| p.lr_steps.clone()).unwrap_or_default();
        let (rows, cursor) = match key.as_ref().and_then(|k| self.history.get(k)) {
            Some(p) => (
                p.steps
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(i, s)| Row { index: i, name: history::step_name(s), time: history::when(s.time), count: s.count, parts: (s.count > 1).then(|| history::merged_values(s)).flatten() })
                    .collect::<Vec<_>>(),
                p.cursor,
            ),
            None => (Vec::new(), 0),
        };
        let mut jump: Option<usize> = None;
        let mut hovered: Option<usize> = None;
        let mut action: Option<MenuAction> = None;
        lr::section(ui, "Verlauf", true, |ui| {
            ui.horizontal(|ui| {
                let n = rows.len().saturating_sub(1);
                ui.label(egui::RichText::new(crate::trf!("{n} Schritte", n = n)).small().color(lr::TEXT_DIM));
                crate::help::info(ui, "verlauf", crate::tr("Verlauf"), &crate::help::HISTORY);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(cursor > 0 && !rows.is_empty(), egui::Button::new(crate::tr("Original ⟲")).small());
                    lr::hover_tip(ui, &r, crate::tr("Zum Ausgangszustand springen"));
                    if r.clicked() {
                        jump = Some(0);
                    }
                });
            });
            let height = ROW_H * ROWS;
            egui::ScrollArea::vertical().id_salt("verlauf-liste").auto_shrink([false, false]).max_height(height).min_scrolled_height(height).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if rows.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(crate::tr("Noch keine Änderungen")).small().color(lr::TEXT_DIM));
                }
                for row in &rows {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
                    let painter = ui.painter_at(rect);
                    if resp.hovered() {
                        painter.rect_filled(rect, 0.0, Color32::from_gray(0x26));
                        if row.index != cursor {
                            hovered = Some(row.index);
                        }
                    }
                    let color = match row.index {
                        i if i == cursor => CURRENT,
                        i if i > cursor => AFTER,
                        _ => BEFORE,
                    };
                    let font = egui::FontId::proportional(lr::UI_SIZE);
                    let small = egui::FontId::proportional(lr::UI_SIZE - 1.5);
                    if row.index == cursor {
                        painter.text(egui::pos2(rect.left() + 2.0, rect.center().y), egui::Align2::LEFT_CENTER, "▶", small.clone(), CURRENT);
                    }
                    // right: "(3×)" and the time, then the name in the space left
                    let time = painter.layout_no_wrap(row.time.clone(), small.clone(), lr::TEXT_DIM);
                    let mut right = rect.right() - 4.0 - time.size().x;
                    painter.galley(egui::pos2(right, rect.center().y - time.size().y / 2.0), time, lr::TEXT_DIM);
                    if row.count > 1 {
                        let c = painter.layout_no_wrap(format!("({}×)", row.count), small.clone(), lr::TEXT_DIM);
                        right -= c.size().x + 8.0;
                        painter.galley(egui::pos2(right, rect.center().y - c.size().y / 2.0), c, lr::TEXT_DIM);
                    }
                    let name_rect = egui::Rect::from_min_max(egui::pos2(rect.left() + 16.0, rect.top()), egui::pos2(right - 6.0, rect.bottom()));
                    painter.with_clip_rect(name_rect).text(name_rect.left_center(), egui::Align2::LEFT_CENTER, &row.name, font, color);
                    if let Some(p) = &row.parts {
                        lr::hover_tip(ui, &resp, p);
                    }
                    if resp.clicked() {
                        jump = Some(row.index);
                    }
                    resp.context_menu(|ui| {
                        if lr::menu_item(ui, crate::tr("Als Version speichern …"), "", in_library) {
                            action = Some(MenuAction::Version(row.index));
                        }
                        if lr::menu_item(ui, crate::tr("Mit aktuellem Stand vergleichen"), "", row.index != cursor) {
                            action = Some(MenuAction::Compare(row.index));
                        }
                        if lr::menu_item(ui, crate::tr("Verlauf bis hier zusammenfassen"), "", row.index >= 2 && row.index <= cursor) {
                            action = Some(MenuAction::Compact(row.index));
                        }
                        ui.separator();
                        if lr::menu_item(ui, crate::tr("Verlauf dieses Fotos löschen …"), "", true) {
                            action = Some(MenuAction::Delete);
                        }
                    });
                }
                // below graineo's steps (older): Lightroom's own history, only shown
                lightroom_rows(ui, &lr_steps);
            });
        });
        let Some((index, key)) = open.zip(key) else {
            self.history.set_hover(&history::Key { path: Default::default(), copy: None }, None, &Settings::default());
            return;
        };
        let current = Settings { view: View::Developed, ..self.entries[index].settings.clone() };
        match action {
            Some(MenuAction::Version(i)) => {
                let name = rows.iter().find(|r| r.index == i).map(|r| r.name.clone()).unwrap_or_default();
                *VERSION_DIALOG.lock().unwrap_or_else(|e| e.into_inner()) = Some((key.clone(), i, name));
            }
            Some(MenuAction::Compare(i)) => {
                if let Some(s) = self.history.state_of(&key, i, &current) {
                    let name = rows.iter().find(|r| r.index == i).map(|r| r.name.clone()).unwrap_or_default();
                    self.history.set_compare(&key, s, crate::trf!("Verlauf · {name}", name = name));
                    self.compare_with_history_step();
                }
            }
            Some(MenuAction::Compact(i)) => {
                self.history.compact_to(&key, i);
            }
            Some(MenuAction::Delete) => *DELETE_DIALOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.clone()),
            None => {}
        }
        self.history_dialogs(ui.ctx(), index, &key, &current);
        // test hook (own data folder only): `GRAINEO_HISTORY_HOVER=<step>` hovers this step
        if hovered.is_none() {
            hovered = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_HISTORY_HOVER").ok()).and_then(|v| v.parse::<usize>().ok()).filter(|&i| i != cursor && i < rows.len());
        }
        if let Some(target) = jump {
            self.history_jump(index, &key, &current, target);
        } else {
            self.history.set_hover(&key, hovered, &current);
        }
    }

    /// Left side of the comparison = the chosen step, right side = the current state.
    pub(crate) fn compare_with_history_step(&mut self) {
        self.before.source = crate::compare_view::BeforeSource::HistoryStep;
        self.before.menu_open = false;
        if !matches!(self.compare_kind(), Some(crate::compare_view::Kind::BeforeAfter | crate::compare_view::Kind::Before)) {
            self.end_look_compare();
            self.compare = true;
        }
    }

    /// The dialogs of the context menu (only while open).
    fn history_dialogs(&mut self, ctx: &egui::Context, index: usize, key: &history::Key, current: &Settings) {
        let version = VERSION_DIALOG.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some((k, step, mut name)) = version.filter(|(k, ..)| k == key) {
            let ok = !name.trim().is_empty();
            let answer = crate::dialog::show(
                ctx,
                "verlauf-version",
                crate::tr("Als Version speichern"),
                crate::dialog::WIDTH,
                |ui| {
                    crate::dialog::input(ui, crate::tr("Name der Version"), &mut name);
                    crate::dialog::hint(ui, crate::tr("Eine virtuelle Kopie mit dem Stand dieses Schritts. Das Foto selbst bleibt, wie es ist."));
                },
                crate::dialog::Buttons::confirm(crate::tr("Speichern"), ok),
            );
            let mut slot = VERSION_DIALOG.lock().unwrap_or_else(|e| e.into_inner());
            match answer {
                crate::dialog::Answer::Open => *slot = Some((k, step, name)),
                crate::dialog::Answer::Cancel => *slot = None,
                crate::dialog::Answer::Primary => {
                    *slot = None;
                    drop(slot);
                    self.save_as_version(index, step, name.trim().to_string(), current);
                }
            }
        }
        let delete = DELETE_DIALOG.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if delete.as_ref() == Some(key) {
            let answer = crate::dialog::show(
                ctx,
                "verlauf-foto-loeschen",
                crate::tr("Verlauf dieses Fotos löschen?"),
                crate::dialog::WIDTH,
                |ui| {
                    crate::dialog::chip(ui, &self.entries[index].name);
                    crate::dialog::text(ui, crate::tr("Alle Schritte dieses Fotos werden gelöscht. Die Bearbeitung selbst bleibt, wie sie jetzt ist."));
                },
                crate::dialog::Buttons::destructive(crate::tr("Löschen"), true),
            );
            match answer {
                crate::dialog::Answer::Open => {}
                crate::dialog::Answer::Cancel => *DELETE_DIALOG.lock().unwrap_or_else(|e| e.into_inner()) = None,
                crate::dialog::Answer::Primary => {
                    *DELETE_DIALOG.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    self.history.clear_photo(key);
                    library::store::send_history();
                }
            }
        }
    }

    /// A virtual copy with the state of step `step` (the same way as "Virtuelle Kopie anlegen"; its history begins
    /// with that state, see [`Self::history_version_done`]).
    pub(crate) fn save_as_version(&mut self, index: usize, step: usize, name: String, current: &Settings) {
        let key = self.history_key(index);
        let Some(state) = self.history.state_of(&key, step, current) else { return };
        let source = self.entries[index].image;
        // versions are virtual copies in the library: refused for a photo of a viewed folder
        if crate::library_ui::needs_library(&[source]) {
            self.status = crate::tr(crate::library_ui::view_folder_guard::NEEDS_LIBRARY).to_string();
            return;
        }
        let from = match &self.entries[index].copy {
            Some(c) => format!("{} ({c})", self.entries[index].name),
            None => self.entries[index].name.clone(),
        };
        self.save_current();
        let meta = self.entries[index].meta.clone();
        self.history.version = Some((source, state.clone(), from, step));
        self.copy_pending = Some((source, !self.in_grid(), Some((state, meta))));
        if !self.lib.create_copy_named(source, Some(name)) {
            self.history.version = None;
            self.copy_done(source, Err(crate::tr("keine Bibliothek").into()));
        }
    }

    /// The copy made by "Als Version speichern" is in the library (entry `pos`): its history starts with the step.
    pub(crate) fn history_version_done(&mut self, source: i64, pos: usize) {
        let Some((src, state, from, step)) = self.history.version.take() else { return };
        if src != source {
            return;
        }
        let key = self.history_key(pos);
        self.history.start_version(&key, &state, &from, step);
        library::store::send_history();
    }

    /// Jump in the history of the open photo (no new step): its state applies.
    fn history_jump(&mut self, index: usize, key: &history::Key, current: &Settings, target: usize) {
        // a change not recorded yet becomes a step first (as before ⌘Z)
        if let Some((i, s)) = &self.committed {
            if *i == index && s != current {
                let before = s.clone();
                self.history.record(key, index, &before, current);
                self.committed = Some((index, current.clone()));
            }
        }
        let from = self.history.cursor_of(key);
        if let Some(target) = self.history.jump(key, index, current, target) {
            let view = self.entries[index].settings.view;
            self.entries[index].settings = Settings { view, ..target.clone() };
            self.committed = Some((index, target));
            self.hover_preset = None;
            crate::hdr_auto::history_moved(from);
        }
    }
}

/// Lightroom's develop history of the photo (from the catalog): one folded line "In Lightroom: n Schritte (from – to)",
/// unfolded the names and dates. Not clickable: Lightroom's states are not graineo's (only shown, see the concept).
fn lightroom_rows(ui: &mut egui::Ui, steps: &[(String, i64)]) {
    let Some(last) = steps.last() else { return };
    let id = ui.make_persistent_id("verlauf-lightroom");
    // test hook (own data folder only): `GRAINEO_HISTORY_LR_OPEN=1` shows the line unfolded
    let unfolded = crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_HISTORY_LR_OPEN").is_ok_and(|v| v == "1");
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(unfolded);
    // the date of the newest step on the right (as the time of graineo's steps; unfolded every step has its date)
    let span = history::date_of(last.1);
    let text = crate::trf!("In Lightroom: {n} Schritte", n = steps.len());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, Color32::from_gray(0x26));
    }
    let painter = ui.painter_at(rect);
    let small = egui::FontId::proportional(lr::UI_SIZE - 1.5);
    painter.text(egui::pos2(rect.left() + 2.0, rect.center().y), egui::Align2::LEFT_CENTER, if open { "▾" } else { "▸" }, small.clone(), lr::TEXT_DIM);
    let dates = painter.layout_no_wrap(span, small.clone(), lr::TEXT_DIM);
    let right = rect.right() - 4.0 - dates.size().x;
    painter.galley(egui::pos2(right, rect.center().y - dates.size().y / 2.0), dates, lr::TEXT_DIM);
    let r = egui::Rect::from_min_max(egui::pos2(rect.left() + 16.0, rect.top()), egui::pos2(right - 6.0, rect.bottom()));
    painter.with_clip_rect(r).text(r.left_center(), egui::Align2::LEFT_CENTER, text, small.clone(), lr::TEXT_DIM);
    if resp.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
    }
    if !open {
        return;
    }
    // newest on top, as graineo's steps
    for (name, t) in steps.iter().rev() {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H - 2.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let date = painter.layout_no_wrap(history::date_of(*t), small.clone(), AFTER);
        let right = rect.right() - 4.0 - date.size().x;
        painter.galley(egui::pos2(right, rect.center().y - date.size().y / 2.0), date, AFTER);
        let r = egui::Rect::from_min_max(egui::pos2(rect.left() + 16.0, rect.top()), egui::pos2(right - 6.0, rect.bottom()));
        painter.with_clip_rect(r).text(r.left_center(), egui::Align2::LEFT_CENTER, name, small.clone(), AFTER);
    }
}
