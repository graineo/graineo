//! Darkroom, phase 2: compare. A drag slider (wipe) shows a second state of the same
//! photo on the left, the current one on the right:
//!
//! - **Without film**: Sigma/camera development (film off) against the current film.
//! - **A | B**: state A (remembered film settings) against the current state B. All
//!   settings changed during the comparison belong to B; "Keep B" keeps
//!   them, "Compare off" restores the state the comparison started with.
//!
//! The left side is rendered in the background like the preview of a preset
//! (`Job::HoverRender`), there is no separate render path.

use develop::film::FilmSettings;
use develop::geometry::Frame;
use develop::Settings;
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Vec2};

use crate::jobs::Job;
use crate::{lr, App};

/// Kind of comparison.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Mode {
    #[default]
    Off,
    /// Film off against current
    NoFilm,
    /// State A against current (B)
    AB,
}

/// State of the comparison.
pub struct Compare {
    pub mode: Mode,
    /// State A (film settings), only in A | B mode
    pub a: Option<FilmSettings>,
    /// Settings at the start of the comparison (restored by "Compare off" in A | B mode)
    pub start: Option<Settings>,
    /// Settings for which the left side was requested
    target_film: Option<Settings>,
    /// left side (fully rendered) and the settings it belongs to
    tex: Option<(Settings, TextureHandle)>,
    /// Split line as a fraction of the image area from the left (0 … 1)
    pub split: f32,
    /// Split line is being dragged (the drag started near the line)
    pub dragging: bool,
    /// visible part of the image in the last frame (limits of the split line)
    view_rect: std::cell::Cell<Rect>,
    /// Test hook (GRAINEO_DARKROOM_COMPARE=A|B): on the first frame remember the current film as A and select this
    /// film as B
    pub test_b: Option<String>,
}

impl Default for Compare {
    fn default() -> Self {
        Self { mode: Mode::Off, a: None, start: None, target_film: None, tex: None, split: 0.5, dragging: false, view_rect: std::cell::Cell::new(Rect::NOTHING), test_b: None }
    }
}

impl Compare {
    pub fn active(&self) -> bool {
        self.mode != Mode::Off
    }

    /// Start the "without film" comparison.
    pub fn without_film(&mut self) {
        self.mode = Mode::NoFilm;
        self.a = None;
        self.start = None;
        self.target_film = None;
        self.tex = None;
    }

    /// Remember the current film state as A and switch to A | B mode.
    pub fn remember(&mut self, current: &Settings) {
        self.a = Some(current.film.clone());
        if self.start.is_none() {
            self.start = Some(current.clone());
        }
        self.mode = Mode::AB;
        self.target_film = None;
        self.tex = None;
    }

    /// Swap A and B (B is the current state of the settings).
    pub fn swap(&mut self, current: &mut Settings) {
        if let Some(a) = self.a.as_mut() {
            std::mem::swap(&mut current.film, a);
            self.target_film = None;
        }
    }

    /// Keep B and end the comparison.
    pub fn adopt(&mut self) {
        *self = Self::default();
    }

    /// End the comparison. In A | B mode the state it started with is restored.
    pub fn switch_off(&mut self, current: &mut Settings) {
        if self.mode == Mode::AB {
            if let Some(s) = self.start.take() {
                *current = s;
            }
        }
        *self = Self::default();
    }

    /// Settings of the left side for the current settings.
    pub fn left_settings(&self, current: &Settings) -> Option<Settings> {
        match self.mode {
            Mode::Off => None,
            Mode::NoFilm => {
                let mut s = current.clone();
                s.film.active = false;
                Some(s)
            }
            Mode::AB => self.a.as_ref().map(|a| Settings { film: a.clone(), ..current.clone() }),
        }
    }

    /// Labels (left, right).
    pub fn names(&self, current: &Settings) -> (String, String) {
        let film = |f: &FilmSettings| {
            if f.is_active() {
                develop::film::stock(&f.stock).map(|s| s.name.clone()).unwrap_or_else(|| f.stock.clone())
            } else {
                crate::tr("OHNE FILM").to_string()
            }
        };
        match self.mode {
            Mode::NoFilm => (crate::tr("OHNE FILM").into(), crate::tr("AKTUELL").into()),
            Mode::AB => (format!("A · {}", self.a.as_ref().map(film).unwrap_or_default()), format!("B · {}", film(&current.film))),
            Mode::Off => (String::new(), String::new()),
        }
    }

    /// Take over the finished left side (only if it is still needed).
    pub fn set_texture(&mut self, ctx: &egui::Context, settings: Settings, image: egui::ColorImage) {
        if self.target_film.as_ref() != Some(&settings) {
            return;
        }
        match &mut self.tex {
            Some((s, t)) => {
                t.set(image, egui::TextureOptions::LINEAR);
                *s = settings;
            }
            None => self.tex = Some((settings, ctx.load_texture("dk-vergleich", image, egui::TextureOptions::LINEAR))),
        }
    }

    /// The left side for these settings will not come (job crashed): request it again next frame.
    pub fn forget_request(&mut self, settings: &Settings) {
        if self.target_film.as_ref() == Some(settings) {
            self.target_film = None;
        }
    }

    /// Has the left side already been rendered for the currently requested settings?
    pub(crate) fn ready(&self, target_film: &Settings) -> bool {
        self.tex.as_ref().is_some_and(|(s, _)| s == target_film)
    }

    /// Area where the split line may lie: the visible part of the image (never into the black beside it).
    fn range(&self, area: Rect) -> Rect {
        let s = self.view_rect.get();
        if s.is_positive() && s.width() > 1.0 && s.height() > 1.0 { s.intersect(area) } else { area }
    }

    /// Does a drag start near the split line? Must run before panning the image so that the
    /// image stays still while the line is dragged.
    pub fn check_drag(&mut self, resp: &egui::Response, area: Rect) {
        if !self.active() {
            self.dragging = false;
            return;
        }
        if resp.drag_started() {
            let b = self.range(area);
            let x = b.left() + self.split * b.width();
            self.dragging = resp.interact_pointer_pos().is_some_and(|p| (p.x - x).abs() < 18.0);
        }
        if !resp.dragged() {
            self.dragging = false;
        }
        if self.dragging {
            if let Some(p) = resp.interact_pointer_pos() {
                let b = self.range(area);
                self.split = ((p.x - b.left()) / b.width().max(1.0)).clamp(0.0, 1.0);
            }
        }
    }

    /// Draw the left side over the main image (left of the split line), plus line, handle, names.
    /// `image` is the rectangle of the whole image, `area` the visible image area.
    pub fn draw(&self, painter: &egui::Painter, area: Rect, image: Rect, src: [f32; 4], frame: &Frame, current: &Settings, left_detail: Option<(egui::TextureId, [f32; 4])>, tint: Color32) {
        if !self.active() {
            return;
        }
        let view_rect = area.intersect(image);
        self.view_rect.set(view_rect);
        let b = self.range(area);
        let x = b.left() + self.split * b.width();
        let (left_label, right_label) = self.names(current);
        let ready = self.left_settings(current).is_some_and(|s| self.ready(&s));
        let clip = Rect::from_min_max(area.min, Pos2::new(x, area.max.y)).intersect(image);
        if let Some((id, region)) = left_detail {
            // 100 % view: left side at full resolution
            crate::paint_src_quad(&painter.with_clip_rect(clip), id, region, frame, image, tint);
        } else if let (true, Some((_, t))) = (ready, self.tex.as_ref()) {
            crate::paint_src_quad(&painter.with_clip_rect(clip), t.id(), src, frame, image, tint);
        }
        let line = Stroke::new(1.5, Color32::from_white_alpha(230));
        painter.line_segment([Pos2::new(x, b.top()), Pos2::new(x, b.bottom())], line);
        let handle = Rect::from_center_size(Pos2::new(x, b.center().y), Vec2::new(34.0, 20.0));
        painter.rect_filled(handle, 0.0, Color32::from_white_alpha(235));
        // Arrows as triangles (the font lacks the arrow glyphs)
        let m = handle.center();
        for r in [-1.0f32, 1.0] {
            let tip = Pos2::new(m.x + r * 12.0, m.y);
            let base = Pos2::new(m.x + r * 4.0, m.y);
            painter.add(egui::Shape::convex_polygon(vec![tip, Pos2::new(base.x, m.y - 5.0), Pos2::new(base.x, m.y + 5.0)], Color32::BLACK, Stroke::NONE));
        }
        let names = |p: Pos2, a: egui::Align2, t: &str, color: Color32| {
            if t.is_empty() {
                return;
            }
            let font = egui::FontId::monospace(11.0);
            let g = painter.layout_no_wrap(t.to_uppercase(), font, color);
            let r = a.anchor_size(p, g.size()).expand2(Vec2::new(7.0, 3.0));
            painter.rect_filled(r, 0.0, Color32::from_black_alpha(170));
            painter.galley(a.anchor_size(p, g.size()).min, g, color);
        };
        names(Pos2::new(b.left() + 12.0, b.top() + 12.0), egui::Align2::LEFT_TOP, &left_label, Color32::from_rgb(0x7a, 0xb8, 0xe8));
        names(Pos2::new(b.right() - 12.0, b.top() + 12.0), egui::Align2::RIGHT_TOP, &right_label, lr::ACCENT);
        if !ready {
            names(Pos2::new(b.left() + 12.0, b.bottom() - 28.0), egui::Align2::LEFT_TOP, crate::tr("wird berechnet …"), Color32::from_gray(0xaa));
        }
    }
}

impl App {
    /// Every frame: request the left side of the comparison if it is missing for the current settings
    /// (rendering runs in the background like the preview of a preset).
    pub(crate) fn darkroom_compare_update(&mut self) {
        if !self.darkroom_active() || !self.darkroom.compare.active() {
            return;
        }
        let Some(c) = self.current.as_ref() else { return };
        let index = c.index;
        if let (Some(b), true) = (self.darkroom.compare.test_b.clone(), self.entries.get(index).is_some_and(|e| e.settings.film.is_active())) {
            self.darkroom.compare.test_b = None;
            let s = self.entries[index].settings.clone();
            self.darkroom.compare.remember(&s);
            self.darkroom.test_film(&b);
        }
        let Some(c) = self.current.as_ref() else { return };
        let Some(e) = self.entries.get(c.index) else { return };
        let Some(target_film) = self.darkroom.compare.left_settings(&e.settings) else { return };
        if self.darkroom.compare.target_film.as_ref() != Some(&target_film) {
            self.worker.send(Job::HoverRender { index: c.index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: target_film.clone(), supersede: false });
            self.darkroom.compare.target_film = Some(target_film);
        }
    }

    /// Actions of the running comparison in the toolbar ("Compare" view).
    pub(crate) fn darkroom_compare_actions(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let toggle = |ui: &mut egui::Ui, on: bool, text: &str| {
            let color = if on { Color32::WHITE } else { lr::TEXT_DIM };
            ui.add(egui::Button::new(egui::RichText::new(text).color(color)).frame(on).fill(if on { Color32::from_gray(0x33) } else { Color32::TRANSPARENT })).clicked()
        };
        match self.darkroom.compare.mode {
            Mode::Off => {}
            Mode::NoFilm => {
                toggle(ui, true, crate::tr("OHNE FILM"));
                if toggle(ui, false, crate::tr("A | B")) {
                    let s = self.entries[index].settings.clone();
                    self.darkroom.compare.remember(&s);
                }
            }
            Mode::AB => {
                if toggle(ui, false, crate::tr("OHNE FILM")) {
                    self.darkroom.compare.without_film();
                }
                toggle(ui, true, crate::tr("A | B"));
                if toggle(ui, false, crate::tr("A MERKEN")) {
                    let s = self.entries[index].settings.clone();
                    self.darkroom.compare.remember(&s);
                }
                if toggle(ui, false, crate::tr("TAUSCHEN")) {
                    let mut s = self.entries[index].settings.clone();
                    self.darkroom.compare.swap(&mut s);
                    self.entries[index].settings = s;
                }
                if toggle(ui, false, crate::tr("B ÜBERNEHMEN")) {
                    self.darkroom.compare.adopt();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_film(id: &str) -> Settings {
        let mut s = Settings::default();
        s.film.active = true;
        s.film.stock = id.to_string();
        s
    }

    #[test]
    fn without_film_only_turns_off_the_film() {
        let mut v = Compare::default();
        v.without_film();
        let s = with_film("kodak_trix_400");
        let l = v.left_settings(&s).unwrap();
        assert!(!l.film.active && l.film.stock == "kodak_trix_400");
        assert_eq!(v.names(&s).0, "OHNE FILM");
    }

    #[test]
    fn a_b_remember_swap_and_reset() {
        let mut v = Compare::default();
        let mut s = with_film("kodak_trix_400");
        v.remember(&s);
        // B: another film
        s.film.stock = "ilford_hp5_plus".into();
        let l = v.left_settings(&s).unwrap();
        assert_eq!(l.film.stock, "kodak_trix_400");
        v.swap(&mut s);
        assert_eq!(s.film.stock, "kodak_trix_400");
        assert_eq!(v.a.as_ref().unwrap().stock, "ilford_hp5_plus");
        // "Compare off" restores the state from the start
        s.film.stock = "agfa_apx_100".into();
        v.switch_off(&mut s);
        assert_eq!(s.film.stock, "kodak_trix_400");
        assert_eq!(v.mode, Mode::Off);
    }

    #[test]
    fn apply_keeps_b() {
        let mut v = Compare::default();
        let mut s = with_film("kodak_trix_400");
        v.remember(&s);
        s.film.stock = "ilford_hp5_plus".into();
        v.adopt();
        assert_eq!(s.film.stock, "ilford_hp5_plus");
        assert!(!v.active() && v.start.is_none());
    }

    #[test]
    fn turning_off_without_film_changes_nothing() {
        let mut v = Compare::default();
        let mut s = with_film("kodak_trix_400");
        v.without_film();
        s.film.grain = 1.4;
        v.switch_off(&mut s);
        assert_eq!(s.film.grain, 1.4);
    }

    #[test]
    fn split_line_stays_in_visible_image() {
        let v = Compare::default();
        let area = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 600.0));
        let image = Rect::from_min_max(Pos2::new(200.0, 0.0), Pos2::new(800.0, 600.0));
        v.view_rect.set(area.intersect(image));
        let b = v.range(area);
        assert_eq!((b.left(), b.right()), (200.0, 800.0));
        // without a known image the whole area applies
        let empty = Compare::default();
        assert_eq!(empty.range(area), area);
    }
}
