//! Darkroom, phase 2: test strip. Like in the lab, a strip of fields with increasing
//! exposure: the same photo at 7 print brightnesses (−1 to +1 stop) or, if the paper has contrast grades,
//! at all grades. A click on a field applies the value. The small images are rendered by a
//! background job (`Job::Variants`), only once the settings have been idle for a moment.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use develop::Settings;
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Vec2};

use crate::jobs::Job;
use crate::{lr, App};

/// What distinguishes the fields.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Kind {
    #[default]
    Brightness,
    Grad,
    /// The photo on every matching paper
    Paper,
    Preflash,
}

/// Print brightnesses of the fields in stops.
pub const BRIGHTNESS: [f32; 7] = [-1.0, -2.0 / 3.0, -1.0 / 3.0, 0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0];

/// Idle time of the settings before the fields are re-rendered (seconds).
const QUIET_PERIOD: f64 = 0.35;

#[derive(Default)]
pub struct Strip {
    pub kind: Kind,
    /// requested key and since when (UI time)
    requested: Option<(u64, f64)>,
    /// key currently being rendered
    computing: Option<u64>,
    /// key of the finished fields
    finished: Option<u64>,
    tex: HashMap<String, TextureHandle>,
}

/// Label of a brightness: "−1", "−⅔" …
pub fn brightness_name(v: f32) -> String {
    let r = (v * 3.0).round() as i32;
    let (vz, a) = (if r < 0 { "−" } else if r > 0 { "+" } else { "" }, r.abs());
    match a {
        0 => "0".to_string(),
        1 => format!("{vz}⅓"),
        2 => format!("{vz}⅔"),
        3 => format!("{vz}1"),
        n => format!("{vz}{}", n as f32 / 3.0),
    }
}

/// Label of a paper grade (Multigrade: filter 00, 0, 1 … 5).
pub fn grade_name(multigrade: bool, v: f32) -> String {
    if multigrade && v == 0.0 {
        "00".into()
    } else if multigrade && v == 0.5 {
        "0".into()
    } else {
        format!("{v}")
    }
}

/// Short paper name for the label of a field.
pub fn paper_short(name: &str) -> String {
    let n = name.split(['(', ',']).next().unwrap_or(name).trim();
    let n = n.trim_start_matches("Ilford ").trim_start_matches("Kodak Professional ").trim_start_matches("Kodak ").trim_start_matches("Fujifilm ");
    n.chars().take(16).collect()
}

/// Settings and labels of the fields. `grade`: family values of the paper (for `Kind::Grad`),
/// `papers`: ID and name of the papers (for `Kind::Paper`).
pub fn variants(s: &Settings, kind: Kind, grade: &[f32], multigrade: bool, papers: &[(String, String)]) -> Vec<(String, Settings)> {
    match kind {
        Kind::Preflash => [0.0f32,0.25,1.0].into_iter().map(|v|{let mut t=s.clone();t.film.preflash=v;(format!("{v:.2}"),t)}).collect(),
        Kind::Paper => papers
            .iter()
            .map(|(id, name)| {
                let mut t = s.clone();
                t.film.paper = Some(id.clone());
                t.film.paper_grade = -1.0;
                (paper_short(name), t)
            })
            .collect(),
        Kind::Brightness => BRIGHTNESS
            .iter()
            .map(|&v| {
                let mut t = s.clone();
                t.film.print_exposure = v;
                (brightness_name(v), t)
            })
            .collect(),
        Kind::Grad => grade
            .iter()
            .map(|&g| {
                let mut t = s.clone();
                t.film.paper_grade = g;
                (grade_name(multigrade, g), t)
            })
            .collect(),
    }
}

/// Key of the fields: everything except the value that changes from field to field.
fn cache_key(index: usize, s: &Settings, kind: Kind) -> u64 {
    let mut n = s.clone();
    match kind {
        Kind::Preflash => n.film.preflash = 0.0,
        Kind::Brightness => n.film.print_exposure = 0.0,
        Kind::Grad => n.film.paper_grade = -1.0,
        Kind::Paper => {
            n.film.paper = None;
            n.film.paper_grade = -1.0;
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    index.hash(&mut h);
    (kind as u8).hash(&mut h);
    serde_json::to_string(&n).unwrap_or_default().hash(&mut h);
    h.finish()
}

impl Strip {
    /// Number of finished fields (tests).
    pub fn fields(&self) -> usize {
        self.tex.len()
    }

    /// Take over finished fields (only if they are still needed).
    pub fn outcome(&mut self, ctx: &egui::Context, key: u64, images: Vec<(String, usize, usize, Vec<u8>)>) {
        if self.computing == Some(key) {
            self.computing = None;
        }
        if self.requested.map(|w| w.0) != Some(key) {
            return;
        }
        self.tex.clear();
        for (name, w, h, rgb) in images {
            let img = egui::ColorImage::from_rgb([w, h], &rgb);
            self.tex.insert(name.clone(), ctx.load_texture(format!("dk-streifen-{name}"), img, egui::TextureOptions::LINEAR));
        }
        self.finished = Some(key);
    }
}

/// Paper of the current film for the test strip.
struct PaperInfo {
    grade: Vec<f32>,
    multi: bool,
    effective_paper: String,
    standard: String,
    list: Vec<(String, String)>,
}

impl App {
    /// Which paper applies, and is there a paper path?
    fn darkroom_strip_paper(&self, index: usize) -> Option<PaperInfo> {
        let s = &self.entries.get(index)?.settings;
        let film = s.film.is_active().then(|| develop::film::stock(&s.film.stock)).flatten()?;
        if film.positive || s.film.negative {
            return None;
        }
        let standard = film.default_paper.clone()?;
        let effective_paper = s.film.paper.clone().unwrap_or_else(|| standard.clone());
        let paper = develop::film::stock(&effective_paper)?;
        let list = develop::film::papers().iter().filter(|p| p.monochrome == film.monochrome).map(|p| (p.id.clone(), p.name.clone())).collect();
        Some(PaperInfo { grade: paper.family.clone(), multi: effective_paper.starts_with("ilford_multigrade"), effective_paper, standard, list })
    }

    /// Every frame: request fields when the settings have been idle briefly and something changed.
    pub(crate) fn darkroom_strip_update(&mut self, ctx: &egui::Context) {
        if !self.darkroom_active() || self.darkroom.image_only || self.darkroom.right_closed {
            return;
        }
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let Some(pi) = self.darkroom_strip_paper(index) else { return };
        let (grade, multi) = (pi.grade.clone(), pi.multi);
        let mut kind = self.darkroom.probe.kind;
        if kind == Kind::Grad && grade.len() < 2 {
            kind = Kind::Brightness;
            self.darkroom.probe.kind = kind;
        }
        let now = ctx.input(|i| i.time);
        let s = self.entries[index].settings.clone();
        let key = cache_key(index, &s, kind);
        // no strip from a fast preview too rough to be shown: asked again once the exact base is there
        let waits = self.derived_waits(&s);
        let st = &mut self.darkroom.probe;
        if st.finished == Some(key) || st.computing == Some(key) {
            st.requested = Some((key, st.requested.map_or(now, |w| if w.0 == key { w.1 } else { now })));
            return;
        }
        match st.requested {
            Some((k, since)) if k == key => {
                if now - since >= QUIET_PERIOD && st.computing.is_none() && !waits {
                    st.computing = Some(key);
                    let Some(c) = self.current.as_ref() else { return };
                    let list = variants(&s, kind, &grade, multi, &pi.list);
                    self.worker.send(Job::Variants { index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), list, key });
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(120));
                }
            }
            _ => {
                st.requested = Some((key, now));
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
    }

    /// Test strip in the left part of the bottom panel.
    pub(crate) fn darkroom_strip_draw(&mut self, ui: &mut egui::Ui, index: usize) {
        lr::subheading(ui, "Probestreifen");
        let Some(pi) = self.darkroom_strip_paper(index) else {
            let text = crate::darkroom::no_paper_text(&self.entries[index].settings.film);
            ui.add(egui::Label::new(egui::RichText::new(text).color(lr::TEXT_DIM())).wrap());
            return;
        };
        let (grade, multi) = (pi.grade.clone(), pi.multi);
        let has_grade = grade.len() > 1;
        let kind = self.darkroom.probe.kind;
        ui.horizontal_wrapped(|ui| {
            let mut a = kind;
            let mut opt: Vec<(Kind, &str)> = vec![(Kind::Brightness, crate::tr("Helligkeit")),(Kind::Preflash,"Vorbelichtung")];
            if has_grade {
                opt.push((Kind::Grad, "Papiergrad"));
            }
            if pi.list.len() > 1 {
                opt.push((Kind::Paper, crate::tr("Papier")));
            }
            for (k, name) in &opt {
                if ui.selectable_label(a == *k, crate::tr(name)).clicked() {
                    a = *k;
                }
            }
            self.darkroom.probe.kind = a;
        });
        let kind = self.darkroom.probe.kind;
        let s = &self.entries[index].settings;
        let fields: Vec<(String, f32)> = match kind {
            Kind::Preflash => [0.0f32,0.25,1.0].into_iter().map(|v|(format!("{v:.2}"),v)).collect(),
            Kind::Brightness => BRIGHTNESS.iter().map(|&v| (brightness_name(v), v)).collect(),
            Kind::Grad => grade.iter().map(|&g| (grade_name(multi, g), g)).collect(),
            Kind::Paper => pi.list.iter().enumerate().map(|(i, (_, n))| (paper_short(n), i as f32)).collect(),
        };
        // current field: the closest one
        let current = {
            let actual = match kind {
                Kind::Preflash => s.film.preflash,
                Kind::Brightness => s.film.print_exposure,
                Kind::Paper => pi.list.iter().position(|(id, _)| *id == pi.effective_paper).unwrap_or(0) as f32,
                Kind::Grad => {
                    if s.film.paper_grade >= 0.0 {
                        s.film.paper_grade
                    } else if multi {
                        3.0
                    } else {
                        grade.get((grade.len().max(1) - 1) / 2).copied().unwrap_or(0.0)
                    }
                }
            };
            fields.iter().enumerate().min_by(|a, b| (a.1 .1 - actual).abs().total_cmp(&(b.1 .1 - actual).abs())).map(|(i, _)| i)
        };
        let space = ui.available_width();
        let gap = 6.0;
        let columns = (((space + gap) / (66.0 + gap)).floor() as usize).clamp(2, 4);
        let b = (space - gap * (columns as f32 - 1.0)) / columns as f32;
        let h = b * 2.0 / 3.0;
        let mut pick: Option<f32> = None;
        for (row, chunk) in fields.chunks(columns).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (j, (name, value)) in chunk.iter().enumerate() {
                let i = row * columns + j;
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(b, h), egui::Sense::click());
                    let active = current == Some(i);
                    ui.painter().rect_filled(r, 0.0, crate::lr::gray(0x14));
                    if let Some(t) = self.darkroom.probe.tex.get(name) {
                        let ts = t.size_vec2();
                        let fit = (r.width() / ts.x).min(r.height() / ts.y);
                        let image = Rect::from_center_size(r.center(), ts * fit);
                        ui.painter().image(t.id(), image, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    } else {
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::monospace(12.0), lr::TEXT_DIM());
                    }
                    let margin = if active { Stroke::new(1.5, lr::ACCENT()) } else if resp.hovered() { Stroke::new(1.0, crate::lr::gray(0x70)) } else { Stroke::new(1.0, crate::lr::gray(0x2e)) };
                    ui.painter().rect_stroke(r, 0.0, margin, egui::StrokeKind::Inside);
                    let (lr_, _) = ui.allocate_exact_size(Vec2::new(b, 14.0), egui::Sense::hover());
                    ui.painter().text(lr_.center(), egui::Align2::CENTER_CENTER, name, egui::FontId::monospace(10.5), if active { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() });
                    if resp.clicked() {
                        pick = Some(*value);
                    }
                });
            }
        });
        }
        if let Some(w) = pick {
            let f = &mut self.entries[index].settings.film;
            match kind {
                Kind::Preflash => f.preflash = w,
                Kind::Brightness => f.print_exposure = w,
                Kind::Grad => f.paper_grade = w,
                Kind::Paper => {
                    if let Some((id, _)) = pi.list.get(w as usize) {
                        f.paper = (*id != pi.standard).then(|| id.clone());
                        f.paper_grade = -1.0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_names() {
        assert_eq!(brightness_name(0.0), "0");
        assert_eq!(brightness_name(1.0), "+1");
        assert_eq!(brightness_name(-1.0), "−1");
        assert_eq!(brightness_name(-2.0 / 3.0), "−⅔");
        assert_eq!(brightness_name(1.0 / 3.0), "+⅓");
    }

    #[test]
    fn grade_names() {
        assert_eq!(grade_name(true, 0.0), "00");
        assert_eq!(grade_name(true, 0.5), "0");
        assert_eq!(grade_name(true, 3.0), "3");
        assert_eq!(grade_name(false, 3.5), "3.5");
    }

    #[test]
    fn variants_change_only_the_value() {
        let mut s = Settings::default();
        s.film.active = true;
        s.film.stock = "kodak_trix_400".into();
        s.film.grain = 1.3;
        let v = variants(&s, Kind::Brightness, &[], true, &[]);
        assert_eq!(v.len(), 7);
        assert!(v.iter().all(|(_, t)| t.film.grain == 1.3 && t.film.stock == "kodak_trix_400"));
        assert_eq!(v[0].1.film.print_exposure, -1.0);
        assert_eq!(v[6].1.film.print_exposure, 1.0);
        let g = variants(&s, Kind::Grad, &[0.0, 0.5, 1.0, 2.0, 3.0, 4.0, 5.0], true, &[]);
        assert_eq!(g.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["00", "0", "1", "2", "3", "4", "5"]);
        assert_eq!(g[4].1.film.paper_grade, 3.0);
        let papers = vec![("ilford_multigrade_iv_rc".to_string(), "Ilford Multigrade IV RC (glossy), contrast grades".to_string()), ("kodak_2302".to_string(), "Kodak Print Film 2302".to_string())];
        let p = variants(&s, Kind::Paper, &[], false, &papers);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, "Multigrade IV RC");
        assert_eq!(p[1].1.film.paper.as_deref(), Some("kodak_2302"));
        assert_eq!(p[1].1.film.paper_grade, -1.0);
    }

    #[test]
    fn key_ignores_varied_value() {
        let mut s = Settings::default();
        s.film.active = true;
        let a = cache_key(0, &s, Kind::Brightness);
        s.film.print_exposure = 0.4;
        assert_eq!(a, cache_key(0, &s, Kind::Brightness), "print brightness is not part of the key");
        s.film.grain = 1.5;
        assert_ne!(a, cache_key(0, &s, Kind::Brightness), "different grain needs new fields");
        assert_ne!(a, cache_key(1, &s, Kind::Brightness));
        let g = cache_key(0, &s, Kind::Grad);
        s.film.paper_grade = 4.0;
        assert_eq!(g, cache_key(0, &s, Kind::Grad));
    }
}
