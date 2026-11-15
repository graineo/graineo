//! Which view the centre of the library shows, and that the user can always see it and leave it:
//! - exactly one entry of the left panel is marked: the source, or one of the views that replace the grid (People,
//!   RAW+JPEG twice) – never several at once;
//! - restrictions that are not a normal source ("Similar photos", "Duplicates") appear as a chip with × in the filter
//!   bar, Esc and "Reset all filters" leave them;
//! - "+" in the header of the FOLDERS section adds a folder to the library;
//! - switching to Develop with nothing selected opens the photo in view, not one far away.

use eframe::egui::{self, Color32};
use std::cell::RefCell;

use super::{Filter, LibraryView, Source};
use crate::lr;

thread_local! {
    /// Source before "Similar photos" / "Duplicates" (× and Esc go back to it).
    static BEFORE: RefCell<Option<Source>> = const { RefCell::new(None) };
}

/// File dialog of "Add folder".
const ADD_FOLDER: &str = "bib-ordner-hinzufuegen";

/// Is this source a restriction of its own kind (shown as a chip)?
fn special(q: &Source) -> bool {
    matches!(q, Source::Similar(_) | Source::Duplicates)
}

impl LibraryView {
    /// A view that replaces the grid (People, RAW+JPEG twice) is open: no source row is marked then.
    pub(super) fn grid_replaced(&self) -> bool {
        self.pairs.open || crate::people_ui::is_open()
    }

    /// Is the row of this source the marked one?
    pub(super) fn source_marked(&self, q: &Source) -> bool {
        self.source == *q && !self.grid_replaced()
    }

    /// Open the People view: the other views close, the source stays (and is marked again afterwards).
    pub(super) fn open_people(&mut self, tab: crate::people_ui::ToolTab) {
        self.pairs.open = false;
        self.loupe = false;
        crate::people_ui::open_at(tab);
    }

    /// Before "Similar photos" or "Duplicates": remember where we came from, close the views that replace the grid.
    pub(super) fn enter_special_source(&mut self) {
        crate::people_ui::close();
        self.pairs.open = false;
        if !special(&self.source) {
            let before = self.source.clone();
            BEFORE.with(|b| *b.borrow_mut() = Some(before));
        }
    }

    /// Leave "Similar photos" / "Duplicates": back to the source before (or All photos). `false`: not in such a view.
    pub(super) fn leave_special_source(&mut self) -> bool {
        if !special(&self.source) {
            return false;
        }
        let back = BEFORE.with(|b| b.borrow_mut().take()).filter(|q| !special(q)).unwrap_or(Source::All);
        self.set_source(back);
        self.refilter = true;
        true
    }

    /// Chip for a special source: text (`None`: nothing to show).
    pub(super) fn source_chip(&self) -> Option<String> {
        match &self.source {
            Source::Similar(file) => {
                let name = self.rows.iter().find(|z| z.file == *file).map(|z| z.name.clone()).unwrap_or_default();
                Some(crate::trf!("Ähnlich zu: {a}", a = name))
            }
            Source::Duplicates => Some(crate::tr("Duplikate").to_string()),
            _ => None,
        }
    }

    /// "Reset all filters": search, flags, stars, labels, metadata and a special source.
    pub(super) fn reset_all_filters(&mut self) {
        self.filter = Filter::default();
        if let Some(f) = &mut self.facets {
            f.reset();
        }
        self.leave_special_source();
        self.refilter = true;
    }

    /// Draw the chip of the special source at `x` in the chip row; returns its right edge and whether × was clicked.
    pub(super) fn paint_source_chip(&self, ui: &mut egui::Ui, region: egui::Rect, x: f32, id: egui::Id) -> Option<(f32, bool)> {
        let text = self.source_chip()?;
        let clip = ui.painter().with_clip_rect(region);
        let g = clip.layout_no_wrap(text.clone(), egui::FontId::proportional(11.0), Color32::WHITE);
        let r = egui::Rect::from_min_size(egui::pos2(x, region.top() + 1.0), egui::vec2(g.size().x + 28.0, 20.0));
        clip.rect_filled(r, 0.0, Color32::from_rgb(0x3c, 0x4a, 0x3a));
        clip.galley(r.left_center() + egui::vec2(9.0, -g.size().y / 2.0), g, Color32::WHITE);
        let cross = egui::Rect::from_center_size(egui::pos2(r.right() - 10.0, r.center().y), egui::vec2(14.0, 14.0));
        let resp = ui.interact(cross.intersect(region), id.with("quelle-chip"), egui::Sense::click());
        clip.text(cross.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(12.0), if resp.hovered() { Color32::WHITE } else { Color32::from_gray(0xc4) });
        lr::tip_key(ui, &resp, "Ansicht verlassen", "grid_esc");
        Some((r.right() + 5.0, resp.clicked()))
    }

    // ───────────────────────────────────────────── Folders: "+"

    /// "+" in a section header at the right (Collections, Folders): returns the response.
    pub(super) fn header_plus(ui: &mut egui::Ui, header_top: f32, header_right: f32, id: &str, tip: &str) -> egui::Response {
        let plus = egui::Rect::from_center_size(egui::pos2(header_right - 14.0, header_top + 14.0), egui::vec2(22.0, 22.0));
        let resp = ui.interact(plus, egui::Id::unique(id), egui::Sense::click());
        let c = plus.center();
        if resp.hovered() {
            ui.painter().rect_filled(plus, 0.0, Color32::from_gray(0x36));
        }
        let stroke = egui::Stroke::new(1.4, if resp.hovered() { Color32::WHITE } else { lr::TEXT_DIM });
        ui.painter().line_segment([c - egui::vec2(5.0, 0.0), c + egui::vec2(5.0, 0.0)], stroke);
        ui.painter().line_segment([c - egui::vec2(0.0, 5.0), c + egui::vec2(0.0, 5.0)], stroke);
        lr::hover_tip(ui, &resp, tip);
        resp
    }

    /// "+" of the FOLDERS section: choose a folder (system dialog off the UI thread), then add it in the background.
    pub(super) fn add_folder_button(&mut self, ui: &mut egui::Ui, header_top: f32, header_right: f32) {
        if Self::header_plus(ui, header_top, header_right, "bib-ordner-plus", "Ordner zur Bibliothek hinzufügen").clicked() {
            crate::file_dialog::folder(ui.ctx(), ADD_FOLDER, |d| d.set_title(crate::tr("Ordner zur Bibliothek hinzufügen")));
        }
        if let Some(folder) = crate::file_dialog::fetch_one(ADD_FOLDER) {
            if self.work.lock().unwrap().running {
                self.manage_message = Some(crate::tr("Es läuft schon eine Aufgabe der Bibliothek. Bitte danach den Ordner noch einmal hinzufügen.").to_string());
            } else {
                let ctx = ui.ctx().clone();
                self.start_job(&ctx, "folder", Some(folder));
            }
        }
    }

    // ───────────────────────────────────────────── To Develop

    /// Before switching to Develop: with nothing selected and the active photo out of view (or filtered out), the first
    /// photo in view becomes the active one – Develop opens what the user is looking at.
    pub fn prepare_develop(&mut self) {
        let active_shown = self.active.is_some_and(|a| self.visible.iter().any(|&i| self.rows[i].image == a));
        if !self.nothing_selected && active_shown {
            return;
        }
        if active_shown && !self.active_outside_view() {
            return;
        }
        if let Some(image) = self.first_image_in_view() {
            self.active = Some(image);
        }
    }

    /// The photo in the top left of the visible part of the grid.
    fn first_image_in_view(&self) -> Option<i64> {
        if self.grid_cell <= 0.0 || self.grid_view_h <= 0.0 || self.columns == 0 {
            return self.visible.first().map(|&i| self.rows[i].image);
        }
        let first = self.sem_from.unwrap_or(self.visible.len());
        let layout = crate::grid_jump::Rows::new(first, self.columns, self.sem_from.is_some(), self.grid_cell, 2.0);
        let pos = crate::grid_jump::first_in_view(self.visible.len(), self.columns, first, &layout, self.grid_cell - 2.0, self.grid_y, self.grid_view_h)?;
        self.visible.get(pos).map(|&i| self.rows[i].image)
    }
}
