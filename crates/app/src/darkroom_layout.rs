//! Darkroom layout: a column with its own tool rail on each side (negative on the left, print on the right), the bar below
//! the image, and the film look handle above the image with its drawer.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Vec2};

use develop::film::FilmSettings;

use crate::darkroom_recipes::{Action, Source};
use crate::presets::Scope;
use crate::{dialog, lr, App};

/// Left rail: the film list or the sliders.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum LeftTab {
    #[default]
    Film,
    Controls,
}

/// Width of a column for a window width: rails and edge strips are subtracted, the photo keeps the rest.
pub fn column_width(window: f32) -> f32 {
    ((window - 2.0 * crate::tool_rail::WIDTH - 24.0) * 0.24).clamp(230.0, 320.0)
}

/// Height of the handle row above the image.
pub const HANDLE_H: f32 = 32.0;
const CHEVRON_W: f32 = 18.0;
const SAVE_W: f32 = 70.0;
/// How long the line under the handle runs.
const LINE_SECONDS: f32 = 0.21;
/// Look strip: fixed card width (names like "Delta 100 · verblasst" fit), height of its head.
const STRIP_CARD_W: f32 = 168.0;
/// Card width limits of the grid in "Manage …".
pub(crate) const CARD_RANGE: std::ops::RangeInclusive<f32> = 120.0..=320.0;
const STRIP_HEAD: f32 = 32.0;
/// Pause on a card before its preview is requested (seconds).
const HOVER_DELAY: f64 = 0.13;

/// How the current film settings relate to the saved film looks.
#[derive(Clone, PartialEq, Debug)]
pub enum LookState {
    NoFilm,
    Exact(String),
    Changed(String),
    Combination,
}

/// `looks`: (name, settings); `applied`: name of the look last applied to this photo.
pub fn look_state(current: &FilmSettings, looks: &[(String, FilmSettings)], applied: Option<&str>) -> LookState {
    if !current.is_active() {
        return LookState::NoFilm;
    }
    if let Some((name, _)) = looks.iter().find(|(n, f)| f == current && applied == Some(n.as_str())).or_else(|| looks.iter().find(|(_, f)| f == current)) {
        return LookState::Exact(name.clone());
    }
    match applied.filter(|a| looks.iter().any(|(n, _)| n == a)) {
        Some(a) => LookState::Changed(a.to_string()),
        None => LookState::Combination,
    }
}

/// A name no film look uses yet (saving never overwrites).
pub fn unique_name(wanted: &str, taken: &[String]) -> String {
    let wanted = wanted.trim();
    if !taken.iter().any(|t| t == wanted) {
        return wanted.to_string();
    }
    (2..).map(|n| format!("{wanted} {n}")).find(|c| !taken.contains(c)).unwrap_or_else(|| wanted.to_string())
}

/// Short film and paper name for the handle and the suggested look name.
fn combination_parts(f: &FilmSettings) -> (String, Option<String>, Option<String>) {
    let stock = develop::film::stock(&f.stock);
    let film = stock.map_or_else(|| f.stock.clone(), |s| crate::darkroom::short_name(&s.name));
    let paper_id = f.paper.clone().or_else(|| stock.and_then(|s| s.default_paper.clone()));
    let paper = paper_id.as_deref().and_then(develop::film::stock).map(|p| crate::darkroom_strip::paper_short(&p.name));
    let grade = (paper.is_some() && f.paper_grade >= 0.0).then(|| format!("{} {}", crate::tr("Grad"), f.paper_grade));
    (film, paper, grade)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| egui::lerp(x as f32..=y as f32, t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

impl App {
    fn look_list(&mut self) -> Vec<(String, FilmSettings)> {
        let mut looks: Vec<(String, FilmSettings)> = self.presets.iter().filter(|p| p.scope == Scope::Film).map(|p| (p.name.clone(), p.film_recipe())).collect();
        looks.extend(self.darkroom.recipes.built_in().iter().map(|p| (crate::tr(&p.name).to_string(), p.film_recipe())));
        looks
    }

    pub(crate) fn darkroom_look_state(&mut self, index: usize) -> LookState {
        let film = self.entries[index].settings.film.clone();
        let image = self.entries[index].image;
        let looks = self.look_list();
        let applied = self.darkroom.recipes.applied.get(&image).cloned();
        look_state(&film, &looks, applied.as_deref())
    }

    fn darkroom_handle_text(&self, index: usize, state: &LookState) -> String {
        match state {
            LookState::NoFilm => crate::tr("Ohne Film").to_string(),
            LookState::Exact(n) | LookState::Changed(n) => n.clone(),
            LookState::Combination => {
                let (film, paper, grade) = combination_parts(&self.entries[index].settings.film);
                [Some(film), paper, grade].into_iter().flatten().collect::<Vec<_>>().join(" · ")
            }
        }
    }

    /// Left rail: film list or sliders.
    pub(crate) fn darkroom_left_rail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let tabs = [(LeftTab::Film, lr::Icon::Film, crate::tr("Film")), (LeftTab::Controls, lr::Icon::Sliders, crate::tr("Regler"))];
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (tab, icon, tip) in tabs {
                if lr::tool_rail_button(ui, icon, self.darkroom.left_tab == tab, true, tip) {
                    self.darkroom.left_tab = tab;
                }
            }
        });
    }

    /// Width of the darkroom group in the bar below the image (measured from its parts).
    pub(crate) fn darkroom_tools_width(&self, ui: &egui::Ui) -> f32 {
        let comparing = self.darkroom.compare.mode != crate::darkroom_compare::Mode::Off;
        let mut w = 6.0 * (26.0 + 3.0);
        if comparing {
            w += 80.0;
        }
        if self.darkroom.grain_check && self.zoom > 1.001 {
            w += ui.painter().layout_no_wrap("Ausschnitt 00,00 mm".to_string(), egui::TextStyle::Small.resolve(ui.style()), lr::TEXT).size().x + 3.0;
        }
        w
    }

    /// Darkroom switches in the bar below the image (the common elements are those of Develop).
    pub(crate) fn darkroom_tools(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 3.0;
        if self.darkroom.compare.mode != crate::darkroom_compare::Mode::Off {
            let label = if self.darkroom.compare.mode == crate::darkroom_compare::Mode::AB { crate::tr("A | B") } else { crate::tr("OHNE FILM") };
            ui.menu_button(label, |ui| self.darkroom_compare_actions(ui));
        }
        if let Some(index) = self.current.as_ref().map(|c| c.index) {
            let f = &mut self.entries[index].settings.film;
            if lr::icon_button_hint(ui, lr::Icon::Film, f.negative, crate::tr("Negativ: den Film selbst zeigen statt des Abzugs")) {
                f.negative = !f.negative;
            }
        }
        if lr::icon_button_hint(ui, lr::Icon::ImageSquare, self.darkroom.on_paper, crate::tr("Auf Papier: das Foto mit Papierrand zeigen")) {
            self.darkroom.on_paper = !self.darkroom.on_paper;
        }
        if lr::icon_button_hint(ui, lr::Icon::CircleHalf, self.darkroom.zones.an, crate::tr("Zonen: Helligkeitsverteilung des Bildes (Zonensystem)")) {
            self.darkroom.zones.an = !self.darkroom.zones.an;
            self.rendered_for = None;
        }
        if lr::icon_button_hint(ui, lr::Icon::Target, self.darkroom.grain_check, crate::tr("Korn prüfen: Korn beim Vergrößern neu berechnen")) {
            self.darkroom.grain_check = !self.darkroom.grain_check;
        }
        if lr::icon_button_hint(ui, lr::Icon::CopySimple, false, &lr::with_shortcut("Neue virtuelle Kopie, um einen anderen Film auszuprobieren", "virtual_copy")) {
            self.create_virtual_copy(None);
        }
        self.darkroom_loupe_info(ui);
    }

    /// The darkroom switches as rows of the menu "MEHR".
    pub(crate) fn darkroom_tools_menu(&mut self, ui: &mut egui::Ui, item: &dyn Fn(&mut egui::Ui, Option<lr::Icon>, Option<Color32>, &str, bool) -> bool) {
        if let Some(index) = self.current.as_ref().map(|c| c.index) {
            let on = self.entries[index].settings.film.negative;
            if item(ui, Some(lr::Icon::Film), None, crate::tr("Negativ"), on) {
                self.entries[index].settings.film.negative = !on;
            }
        }
        if item(ui, Some(lr::Icon::ImageSquare), None, crate::tr("Auf Papier"), self.darkroom.on_paper) {
            self.darkroom.on_paper = !self.darkroom.on_paper;
        }
        if item(ui, Some(lr::Icon::CircleHalf), None, crate::tr("Zonen"), self.darkroom.zones.an) {
            self.darkroom.zones.an = !self.darkroom.zones.an;
            self.rendered_for = None;
        }
        if item(ui, Some(lr::Icon::Target), None, crate::tr("Korn prüfen"), self.darkroom.grain_check) {
            self.darkroom.grain_check = !self.darkroom.grain_check;
        }
        if item(ui, Some(lr::Icon::CopySimple), None, crate::tr("Neue Kopie"), false) {
            self.create_virtual_copy(None);
        }
    }

    /// Fixed row above the image: typographic handle of the look drawer, bookmark "save as film look", help.
    pub(crate) fn darkroom_handle(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let state = self.darkroom_look_state(index);
        let row = ui.max_rect();
        self.darkroom.handle_rect = Some(row);
        let resp_id = ui.id().with("dk-griff");
        let hovered_before = ui.ctx().read_response(resp_id).is_some_and(|r| r.hovered());
        let open = self.darkroom.recipes.drawer;
        let text_color = if hovered_before || open { Color32::from_gray(0xee) } else { lr::TEXT };

        // Text in the centre; both sides keep room for bookmark and help, so the text stays centred
        let side = 12.0 + CHEVRON_W + SAVE_W + 12.0;
        let room = (row.width() - 2.0 * side).max(60.0);
        let changed_suffix = matches!(state, LookState::Changed(_)).then(|| ui.painter().layout_no_wrap(crate::tr("geändert").to_string(), egui::FontId::proportional(10.5), lr::TEXT_DIM));
        let suffix_w = changed_suffix.as_ref().map_or(0.0, |g| g.size().x + 6.0);
        let mut job = egui::text::LayoutJob::simple_singleline(self.darkroom_handle_text(index, &state), egui::FontId::proportional(12.0), text_color);
        job.wrap = egui::text::TextWrapping { max_width: (room - suffix_w).max(40.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let name = ui.painter().layout_job(job);
        let text_w = name.size().x + suffix_w;
        let y = row.center().y - 3.0;
        let text_left = row.center().x - text_w / 2.0;
        ui.painter().galley(Pos2::new(text_left, y - name.size().y / 2.0), name.clone(), text_color);
        if let Some(g) = changed_suffix {
            ui.painter().galley(Pos2::new(text_left + name.size().x + 6.0, y - g.size().y / 2.0 + 1.0), g, lr::TEXT_DIM);
        }

        // Chevron beside the text; open it points upward
        let c = Pos2::new(text_left + text_w + 10.0, y);
        let d = if open { -2.0 } else { 2.0 };
        let stroke = egui::Stroke::new(1.0, if hovered_before || open { Color32::from_gray(0xcc) } else { lr::TEXT_DIM });
        ui.painter().line_segment([c + Vec2::new(-3.5, -d), c + Vec2::new(0.0, d)], stroke);
        ui.painter().line_segment([c + Vec2::new(0.0, d), c + Vec2::new(3.5, -d)], stroke);

        // The fine line grows from the text to the full image width and turns gold while the drawer is open
        let duration = if ui.ctx().global_style().animation_time > 0.0 { LINE_SECONDS } else { 0.0 };
        let t = ui.ctx().animate_bool_with_time(egui::Id::new("dk-griff-linie"), open, duration);
        let eased = 1.0 - (1.0 - t).powi(3);
        let (rest_l, rest_r) = (text_left, text_left + text_w);
        let (full_l, full_r) = (row.left(), row.right());
        let line_y = row.bottom() - 1.5;
        let rest = if hovered_before { Color32::from_gray(0x88) } else { Color32::from_gray(0x4a) };
        ui.painter().line_segment(
            [Pos2::new(egui::lerp(rest_l..=full_l, eased), line_y), Pos2::new(egui::lerp(rest_r..=full_r, eased), line_y)],
            egui::Stroke::new(1.0, lerp_color(rest, lr::GOLD, eased)),
        );
        if t > 0.0 && t < 1.0 {
            ui.ctx().request_repaint();
        }

        // Clickable area: text with chevron
        let area = Rect::from_min_max(Pos2::new(text_left - 6.0, row.top()), Pos2::new(text_left + text_w + CHEVRON_W + 2.0, row.bottom()));
        let resp = ui.interact(area, resp_id, egui::Sense::click());
        if resp.clicked() {
            self.darkroom.recipes.drawer = !open;
        }

        // "Speichern": fixed place to the right of the text, only with something unsaved
        let save = Rect::from_center_size(Pos2::new(area.right() + 8.0 + SAVE_W / 2.0, y), Vec2::new(SAVE_W, 22.0));
        if matches!(state, LookState::Combination | LookState::Changed(_)) {
            let save_resp = ui.interact(save, ui.id().with("dk-speichern"), egui::Sense::click());
            let color = if save_resp.hovered() { Color32::from_gray(0xee) } else { lr::TEXT_DIM };
            ui.painter().text(save.left_center(), egui::Align2::LEFT_CENTER, crate::tr("Speichern"), egui::FontId::proportional(12.0), color);
            lr::hover_tip(ui, &save_resp, crate::tr("Als neuen Filmlook speichern"));
            if save_resp.clicked() {
                self.darkroom.recipes.save_name = Some(self.darkroom_suggested_name(index, &state));
            }
        }
    }

    fn darkroom_suggested_name(&self, index: usize, state: &LookState) -> String {
        if let LookState::Changed(n) = state {
            return crate::trf!("{name} Kopie", name = n);
        }
        let (film, paper, _) = combination_parts(&self.entries[index].settings.film);
        [Some(film), paper].into_iter().flatten().collect::<Vec<_>>().join(" · ")
    }

    /// Height of the look strip: its head, one row of cards and the scroll bar.
    pub(crate) fn darkroom_strip_height(&self) -> f32 {
        let (w, h) = crate::recipe_grid::line_card_size(&self.darkroom.recipes.images, STRIP_CARD_W);
        let _ = w;
        STRIP_HEAD + h + 18.0
    }

    /// Height to reserve this frame: animated, so the photo moves gently (it is only fitted anew, nothing is rendered).
    pub(crate) fn darkroom_strip_animated(&self, ctx: &egui::Context) -> f32 {
        let open = self.darkroom.recipes.drawer && self.darkroom_active() && !self.darkroom.image_only;
        let duration = if ctx.global_style().animation_time > 0.0 { 0.18 } else { 0.0 };
        let t = ctx.animate_bool_with_time(egui::Id::new("dk-strip-anim"), open, duration);
        if t > 0.0 && t < 1.0 {
            ctx.request_repaint();
        }
        self.darkroom_strip_height() * (1.0 - (1.0 - t).powi(3))
    }

    /// The look strip above the image; stays open until it is closed via the same row or Esc.
    pub(crate) fn darkroom_look_strip(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let ctx = ui.ctx().clone();
        if self.darkroom.recipes.drawer {
            self.update_recipe_previews(&ctx);
        }
        let area = ui.max_rect();
        // Wheel moves the row sideways while the pointer is inside
        if ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| area.contains(p)) {
            ctx.input_mut(|i| {
                let y = i.smooth_scroll_delta.y;
                if y != 0.0 {
                    i.smooth_scroll_delta.x += y;
                    i.smooth_scroll_delta.y = 0.0;
                }
            });
        }
        let mut chosen: Option<Source> = None;
        let mut manage = false;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.set_height(STRIP_HEAD - 8.0);
            ui.label(egui::RichText::new(crate::tr("FILMLOOKS")).small().color(lr::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if ui.add(egui::Button::new(egui::RichText::new(crate::tr("Verwalten …"))).min_size(Vec2::new(0.0, 20.0))).clicked() {
                    manage = true;
                }
                let mut filter = self.darkroom.recipes.filter;
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    lr::segmented_pad(ui, &mut filter, &[(0u8, crate::tr("Alle")), (1, crate::tr("S/W")), (2, crate::tr("Farbe"))], 6.0);
                });
                self.darkroom.recipes.filter = filter;
            });
        });
        let current = self.entries[index].settings.film.clone();
        let filter = self.darkroom.recipes.filter;
        let width = STRIP_CARD_W;
        let images = &self.darkroom.recipes.images;
        let mut hovered = None;
        egui::ScrollArea::horizontal().id_salt("dk-looks-row").auto_shrink([false, false]).show(ui, |ui| {
            (chosen, hovered) = crate::recipe_grid::line(ui, images, width, filter, &current);
        });
        if let Some(q) = chosen {
            self.darkroom_run_recipe(Action::Load(q));
            self.darkroom_end_look_hover();
        } else {
            self.darkroom_look_hover(&ctx, index, hovered);
        }
        if manage {
            self.darkroom.recipes.opened = true;
        }
    }

    /// Hover preview in the large image: after a short delay and with at most one job on its way, so fast
    /// movements never pile up. Settings stay untouched until a click.
    fn darkroom_look_hover(&mut self, ctx: &egui::Context, index: usize, hovered: Option<(Source, FilmSettings)>) {
        let Some((source, film)) = hovered else {
            self.darkroom_end_look_hover();
            return;
        };
        let now = ctx.input(|i| i.time);
        let since = match self.darkroom.recipes.hover_since {
            Some((q, t)) if q == source => t,
            _ => {
                self.darkroom.recipes.hover_since = Some((source, now));
                now
            }
        };
        if now - since < HOVER_DELAY {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
            return;
        }
        let mut wanted = self.entries[index].settings.clone();
        wanted.film = film;
        // never a hover preview from a fast preview too rough to be shown
        if self.derived_waits(&wanted) {
            return;
        }
        self.hover_preset = Some(wanted.clone());
        self.darkroom.recipes.look_hover = true;
        // the job on its way counts as done when its image has arrived (or it has been lost for a while)
        let shown = self.hover_shown().map(|(s, _)| s.clone());
        let busy = self.darkroom.recipes.hover_sent.as_ref().is_some_and(|(s, t)| shown.as_ref() != Some(s) && now - t < 4.0);
        let already = self.darkroom.recipes.hover_sent.as_ref().is_some_and(|(s, _)| *s == wanted);
        if !busy && !already {
            if let Some(c) = &self.current {
                self.worker.send(crate::jobs::Job::HoverRender { index: c.index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: wanted.clone(), supersede: true });
            }
            self.darkroom.recipes.hover_sent = Some((wanted, now));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }

    /// Back to the current state at once.
    pub(crate) fn darkroom_end_look_hover(&mut self) {
        self.darkroom.recipes.hover_since = None;
        self.darkroom.recipes.hover_sent = None;
        if self.darkroom.recipes.look_hover {
            self.darkroom.recipes.look_hover = false;
            self.hover_preset = None;
        }
    }

    /// Name dialog of "Speichern" (shown from the frame end, above everything).
    pub(crate) fn darkroom_drawer(&mut self, ctx: &egui::Context) {
        if !self.darkroom.recipes.drawer {
            self.darkroom_end_look_hover();
        }
        self.darkroom_save_dialog(ctx);
    }

    fn darkroom_save_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.darkroom.recipes.save_name.take() else { return };
        let ok = !name.trim().is_empty();
        let answer = dialog::show(
            ctx,
            "dk-filmlook-speichern",
            crate::tr("Als Filmlook speichern"),
            420.0,
            |ui| {
                crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut name).desired_width(380.0).hint_text(crate::tr("Name des Filmlooks")));
            },
            dialog::Buttons::confirm(crate::tr("Speichern"), ok),
        );
        match answer {
            dialog::Answer::Primary if ok => self.darkroom_save_new(&name),
            dialog::Answer::Cancel => {}
            _ => self.darkroom.recipes.save_name = Some(name),
        }
    }

    /// Saves a new film look (never overwrites) and marks it as applied to the photo.
    pub(crate) fn darkroom_save_new(&mut self, wanted: &str) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let taken: Vec<String> = self.look_list().into_iter().map(|l| l.0).collect();
        let name = unique_name(wanted, &taken);
        self.darkroom_run_recipe(Action::Save(name.clone()));
        self.darkroom.recipes.applied.insert(self.entries[index].image, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(stock: &str) -> FilmSettings {
        FilmSettings { active: true, stock: stock.into(), ..Default::default() }
    }

    #[test]
    fn recognizes_exact_changed_and_none() {
        let looks = vec![("A".to_string(), look("kodak_trix_400")), ("B".to_string(), look("ilford_hp5_plus"))];
        assert_eq!(look_state(&look("kodak_trix_400"), &looks, None), LookState::Exact("A".into()));
        let mut changed = look("kodak_trix_400");
        changed.grain = 0.2;
        assert_eq!(look_state(&changed, &looks, Some("A")), LookState::Changed("A".into()));
        assert_eq!(look_state(&changed, &looks, None), LookState::Combination);
        assert_eq!(look_state(&FilmSettings::default(), &looks, None), LookState::NoFilm);
        // the same settings under two names: the applied one wins
        let two = vec![("A".to_string(), look("kodak_trix_400")), ("C".to_string(), look("kodak_trix_400"))];
        assert_eq!(look_state(&look("kodak_trix_400"), &two, Some("C")), LookState::Exact("C".into()));
    }

    #[test]
    fn saving_never_reuses_a_name() {
        let taken = vec!["X".to_string(), "X 2".to_string()];
        assert_eq!(unique_name("Y", &taken), "Y");
        assert_eq!(unique_name("X", &taken), "X 3");
    }

    #[test]
    fn columns_leave_room_for_the_photo() {
        assert_eq!(column_width(900.0), 230.0);
        assert_eq!(column_width(2200.0), 320.0);
    }
}
