//! Darkroom, phase 2: live markers in a film's info card. Above the characteristic curve lie
//! bars showing how much of the photo falls on the film at which exposure (distribution of scene brightness
//! in stops relative to middle grey, shifted by film exposure and push). This shows whether
//! shadows land in the toe of the curve or highlights in the shoulder.
//!
//! The distribution is computed by a background job (`Job::SceneDistribution`) on the small
//! scene-linear version of the photo, as also used by the film thumbnails.

use std::hash::{Hash, Hasher};

use develop::Settings;
use eframe::egui;
use egui::{Color32, Pos2, Rect};

use crate::jobs::Job;
use crate::App;

/// The first bin starts at −8 stops, each bin is half a stop wide.
const EV_START: f32 = -8.0;
const EV_STEP: f32 = 0.5;

#[derive(Default)]
pub struct Marker {
    key: Option<u64>,
    computing: Option<u64>,
    bins: Vec<u32>,
}

/// Key: everything that changes the scene (before the film), i.e. the settings without film.
fn scene_key(index: usize, s: &Settings) -> u64 {
    let mut n = s.clone();
    n.film = Default::default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    index.hash(&mut h);
    serde_json::to_string(&n).unwrap_or_default().hash(&mut h);
    h.finish()
}

impl Marker {
    pub fn outcome(&mut self, key: u64, bins: Vec<u32>) {
        if self.computing == Some(key) {
            self.computing = None;
        }
        if self.key == Some(key) || self.computing.is_none() {
            self.key = Some(key);
            self.bins = bins;
        }
    }

    /// Number of bins with pixels (tests).
    pub fn filled(&self) -> usize {
        self.bins.iter().filter(|&&b| b > 0).count()
    }

    /// Lay bars over the characteristic curve. `r`: drawing area of the curve, `x`: displayed log range,
    /// `shift_ev`: film exposure minus push in stops.
    pub fn draw(&self, painter: &egui::Painter, r: Rect, x: (f32, f32), shift_ev: f32) {
        let max = self.bins.iter().copied().max().unwrap_or(0);
        if max == 0 {
            return;
        }
        let height = r.height() * 0.32;
        for (i, &b) in self.bins.iter().enumerate() {
            if b == 0 {
                continue;
            }
            let ev0 = EV_START + i as f32 * EV_STEP + shift_ev;
            let (a, e) = (ev0 * 0.30103, (ev0 + EV_STEP) * 0.30103);
            if e < x.0 || a > x.1 {
                continue;
            }
            let px = |lg: f32| r.left() + (lg - x.0) / (x.1 - x.0) * r.width();
            let (xa, xe) = (px(a.max(x.0)), px(e.min(x.1)));
            let h = (b as f32 / max as f32).sqrt() * height;
            painter.rect_filled(Rect::from_min_max(Pos2::new(xa + 0.5, r.bottom() - h), Pos2::new(xe - 0.5, r.bottom())), 0.0, Color32::from_rgba_unmultiplied(0xc8, 0xaa, 0x62, 110));
        }
    }
}

impl App {
    /// Every frame: request the distribution while a film's info card is open.
    pub(crate) fn darkroom_marker_update(&mut self) {
        if !self.darkroom_active() {
            return;
        }
        let Some(id) = self.darkroom.card_open() else { return };
        if develop::film::stock(&id).is_some_and(|i| matches!(i.category, develop::film::Category::PrintFilm | develop::film::Category::Paper)) {
            return;
        }
        let Some(c) = self.current.as_ref() else { return };
        let Some(e) = self.entries.get(c.index) else { return };
        let key = scene_key(c.index, &e.settings);
        let m = &mut self.darkroom.marker;
        if m.key == Some(key) || m.computing == Some(key) {
            return;
        }
        m.computing = Some(key);
        self.worker.send(Job::SceneDistribution { index: c.index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: e.settings.clone(), key });
    }

    /// Shift of the curve by film exposure and push in stops (for the live marker).
    pub(crate) fn darkroom_marker_shift(&self) -> f32 {
        self.current.as_ref().and_then(|c| self.entries.get(c.index)).map_or(0.0, |e| e.settings.film.exposure - e.settings.film.push)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;

    #[test]
    fn scene_key_ignores_the_film() {
        let mut s = Settings::default();
        let a = scene_key(0, &s);
        s.film.active = true;
        s.film.stock = "kodak_trix_400".into();
        s.film.print_exposure = 0.5;
        assert_eq!(a, scene_key(0, &s), "film and print do not change the scene");
        s.exposure = 1.0;
        assert_ne!(a, scene_key(0, &s), "exposure changes it");
    }

    #[test]
    fn result_is_taken_and_counted() {
        let mut m = Marker::default();
        m.computing = Some(7);
        let mut bins = vec![0u32; 28];
        bins[16] = 100;
        bins[10] = 5;
        m.outcome(7, bins);
        assert_eq!(m.filled(), 2);
        assert_eq!(m.key, Some(7));
    }

    #[test]
    fn draw_without_crash() {
        let ctx = egui::Context::default();
        let mut m = Marker::default();
        m.bins = (0..28).map(|i| (i * 10) as u32).collect();
        let out = crate::ui_test::run(&ctx, Default::default(), |ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(300.0, 120.0), egui::Sense::hover());
            m.draw(ui.painter(), r, (-3.0, 3.0), 0.5);
        });
        assert!(!out.shapes.is_empty());
    }
}
