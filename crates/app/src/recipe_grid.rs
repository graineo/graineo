//! Image grid for film recipes; the same background renderer as for the contact sheet.
use crate::darkroom_recipes::Source;
use crate::{
    jobs::{Job, ContactTarget},
    lr,
    presets::Scope,
    App,
};
use develop::{film::FilmSettings, Settings};
use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Vec2};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

#[derive(Clone, PartialEq)]
pub struct Entry {
    pub source: Source,
    pub name: String,
    pub film_name: String,
    pub film: FilmSettings,
    pub group: u8,
    /// Presets: the finished settings for the preview (instead of base + film); `None` for film looks
    pub full: Option<Settings>,
    /// Order within the group (0 = sort by name)
    pub series: u32,
    key: u64,
}
struct Task {
    id: u64,
    keys: Vec<u64>,
}
#[derive(Default)]
pub struct Previews {
    pub entries: Vec<Entry>,
    basis: Option<(i64, Settings)>,
    pub aspect_ratio: f32,
    cache: HashMap<u64, TextureHandle>,
    order: VecDeque<u64>,
    wish: Option<(u64, f64)>,
    task: Option<Task>,
    counter: u64,
}
fn hash<T: Hash>(v: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}
fn type_name(f: &FilmSettings) -> u8 {
    develop::film::stock(&f.stock).map_or(3, |s| {
        if s.monochrome {
            0
        } else if s.positive {
            2
        } else {
            1
        }
    })
}
impl Previews {
    fn prepare(&mut self, image: i64, mut basis: Settings, mut items: Vec<Entry>, now: f64) {
        basis.film = FilmSettings::default();
        basis.view = develop::View::Developed;
        items.sort_by_key(|e| {
            (
                matches!(e.source, Source::BuiltIn(_)) && e.full.is_none(),
                e.group,
                e.series,
                e.name.to_lowercase(),
            )
        });
        let unchanged = self
            .basis
            .as_ref()
            .is_some_and(|(b, s)| *b == image && *s == basis)
            && self.entries.len() == items.len()
            && self
                .entries
                .iter()
                .zip(&items)
                .all(|(a, b)| a.source == b.source && a.name == b.name && a.film == b.film && a.full == b.full);
        if unchanged {
            return;
        }
        let base_key = hash(&(image, serde_json::to_string(&basis).unwrap()));
        for e in &mut items {
            e.key = hash(&(base_key, serde_json::to_string(&e.film).unwrap(), serde_json::to_string(&e.full).unwrap()));
        }
        let wanted = hash(&items.iter().map(|e| e.key).collect::<Vec<_>>());
        if self.wish.is_none_or(|w| w.0 != wanted) {
            self.wish = Some((wanted, now));
        }
        self.basis = Some((image, basis));
        self.entries = items;
    }
    /// Finished preview of an entry (`None` while it is still being rendered).
    pub fn texture(&self, e: &Entry) -> Option<TextureHandle> {
        self.cache.get(&e.key).cloned()
    }
    pub fn progress(&self) -> (usize, usize) {
        (
            self.entries
                .iter()
                .filter(|e| self.cache.contains_key(&e.key))
                .count(),
            self.entries.len(),
        )
    }
    fn missing(&self) -> Vec<(u64, Settings)> {
        let Some((_, basis)) = &self.basis else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        self.entries
            .iter()
            .filter(|e| !self.cache.contains_key(&e.key) && seen.insert(e.key))
            .map(|e| {
                if let Some(v) = &e.full {
                    return (e.key, v.clone());
                }
                let mut s = basis.clone();
                s.film = e.film.clone();
                (e.key, s)
            })
            .collect()
    }
    pub fn outcome(
        &mut self,
        ctx: &egui::Context,
        id: u64,
        fields: Vec<(usize, usize, usize, Vec<u8>)>,
        end: bool,
    ) {
        let Some(a) = self.task.as_ref().filter(|a| a.id == id) else {
            return;
        };
        for (sp, w, h, rgb) in fields {
            let Some(&key) = a.keys.get(sp) else { continue };
            self.cache.insert(
                key,
                ctx.load_texture(
                    format!("recipe-{key}"),
                    egui::ColorImage::from_rgb([w, h], &rgb),
                    egui::TextureOptions::LINEAR,
                ),
            );
            self.order.push_back(key);
        }
        if end {
            self.task = None;
            let keep: HashSet<_> = self.entries.iter().map(|e| e.key).collect();
            let limit = (self.entries.len() * 2).max(64);
            while self.cache.len() > limit {
                let Some(k) = self.order.pop_front() else {
                    break;
                };
                if keep.contains(&k) {
                    self.order.push_back(k);
                } else {
                    self.cache.remove(&k);
                }
            }
        }
    }
}
impl App {
    /// Only requested by the open recipe book or the visible preset list.
    pub(crate) fn update_recipe_previews(&mut self, ctx: &egui::Context) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else {
            return;
        };
        let mut items = Vec::new();
        for (i, p) in self
            .presets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.scope == Scope::Film)
        {
            let film = p.film_recipe();
            items.push(Entry {
                source: Source::Own(i),
                name: p.name.clone(),
                film_name: crate::darkroom_recipes::film_name(p),
                group: type_name(&film),
                film,
                full: None,
                series: 0,
                key: 0,
            });
        }
        for (i, p) in self.darkroom.recipes.built_in().iter().enumerate() {
            let film = p.film_recipe();
            items.push(Entry {
                source: Source::BuiltIn(i),
                // built-in names are translation keys, user names stay as entered
                name: crate::tr(&p.name).to_string(),
                film_name: crate::darkroom_recipes::film_name(p),
                group: type_name(&film),
                film,
                full: None,
                series: 0,
                key: 0,
            });
        }
        self.maintain_previews(ctx, items, ContactTarget::Recipes);
    }

    /// Preset grid: previews of all presets on the current photo (only with the panel open).
    pub(crate) fn update_preset_previews(&mut self, ctx: &egui::Context, sigma: bool) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else {
            return;
        };
        let mut cur = self.entries[index].settings.clone();
        cur.view = develop::View::Developed;
        let mut items = Vec::new();
        let mut series = 0u32;
        for (gi, cat) in crate::presets::presets_library().into_iter().enumerate() {
            let sigma_only = cat.name == "Foveon-Spezial";
            for p in &cat.presets {
                series += 1;
                if sigma_only && !sigma {
                    continue;
                }
                items.push(Entry {
                    source: Source::BuiltIn(series as usize - 1),
                    name: crate::tr(&p.name).to_string(),
                    film_name: crate::tr(&p.hint).to_string(),
                    film: FilmSettings::default(),
                    group: gi as u8,
                    full: Some(p.apply(&cur)),
                    series,
                    key: 0,
                });
            }
        }
        for (i, p) in self.presets.iter().enumerate().filter(|(_, p)| p.scope != Scope::Film) {
            items.push(Entry {
                source: Source::Own(i),
                name: p.name.clone(),
                film_name: String::new(),
                film: FilmSettings::default(),
                group: 99,
                full: Some(p.apply(&cur)),
                series: i as u32 + 1,
                key: 0,
            });
        }
        self.maintain_previews(ctx, items, ContactTarget::Presets);
    }

    fn maintain_previews(&mut self, ctx: &egui::Context, items: Vec<Entry>, target: ContactTarget) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let s = self.entries[index].settings.clone();
        let c = self.current.as_ref().unwrap();
        let frame = develop::geometry::Frame::from_settings(
            &s,
            c.loaded.full_size[0],
            c.loaded.full_size[1],
            c.loaded.cal.orientation,
        );
        let v = match target {
            ContactTarget::Presets => &mut self.preset_images,
            _ => &mut self.darkroom.recipes.images,
        };
        v.aspect_ratio = (frame.w / frame.h.max(1.0)).clamp(0.1, 10.0);
        // An exact Sigma base that has since finished also replaces the early preview images.
        let source = hash(&(
            self.entries[index].image,
            c.loaded.previews.spp.is_some(),
            c.loaded.cal.orientation,
            c.loaded.full_size,
        )) as i64;
        v.prepare(source, s, items, now);
        let (done, total) = v.progress();
        if done == total || v.task.is_some() {
            return;
        }
        if v.wish.is_none_or(|w| now - w.1 < 0.35) {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
            return;
        }
        let missing = v.missing();
        if missing.is_empty() {
            return;
        }
        v.counter = v.counter.wrapping_add(1);
        let id = v.counter;
        let keys = missing.iter().map(|(k, _)| *k).collect();
        let fields: Vec<_> = missing
            .into_iter()
            .enumerate()
            .map(|(i, (_, s))| (i, s))
            .collect();
        let rows = fields
            .chunks(4)
            .enumerate()
            .map(|(z, f)| (z, f.to_vec()))
            .collect();
        v.task = Some(Task { id, keys });
        self.worker.send(Job::Contact {
            index,
            previews: c.loaded.previews.clone(),
            cal: c.loaded.cal.clone(),
            rows: rows,
            key: id,
            target,
        });
    }
}

/// Equally sized image fields; names always take two lines, film names one.
/// Card grid of the darkroom recipe book and the presets (user ones first, marked as "OWN");
/// `show_card` selects the entries. "Film & Look" uses the plain tiles of [`lr::tile_grid`].
pub fn raster(
    ui: &mut egui::Ui,
    v: &Previews,
    show_card: &dyn Fn(&Entry) -> bool,
    selection: Option<Source>,
    target_width: f32,
    scroll: bool,
    hover: &mut Option<Source>,
) -> Option<(Source, bool)> {
    let items: Vec<_> = v.entries.iter().filter(|e| show_card(e)).collect();
    if items.is_empty() {
        return None;
    }
    let gap = 10.0;
    let cols = ((ui.available_width() + gap) / (target_width + gap))
        .floor()
        .clamp(1.0, 6.0) as usize;
    let width = ((ui.available_width() - gap * (cols - 1) as f32) / cols as f32).max(20.0);
    let image_h = width / v.aspect_ratio.max(0.1);
    let mut action = None;
    for row in items.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for e in row {
                let r = card(ui, v, e, selection, width, image_h, target_width, scroll);
                if r.hovered() {
                    *hover = Some(e.source);
                }
                if r.clicked() || r.double_clicked() {
                    action = Some((e.source, r.double_clicked()));
                }
            }
        });
        ui.add_space(gap);
    }
    action
}

/// One preview card.
#[allow(clippy::too_many_arguments)]
fn card(ui: &mut egui::Ui, v: &Previews, e: &Entry, selection: Option<Source>, width: f32, image_h: f32, target_width: f32, scroll: bool) -> egui::Response {
        let (rect, r) =
            ui.allocate_exact_size(Vec2::new(width, image_h + 54.0), egui::Sense::click());
        let id = egui::Id::unique(("recipe-card", e.source, (target_width as usize)));
        let r = ui.interact(rect, id, egui::Sense::click()).union(r);
        let p = ui.painter_at(rect);
        let selected = selection == Some(e.source);
        p.rect_filled(
            rect, 0.0,
            Color32::from_gray(if selected { 48 } else { 25 }),
        );
        let image = Rect::from_min_max(
            rect.min + Vec2::splat(4.0),
            Pos2::new(rect.right() - 4.0, rect.top() + image_h - 4.0),
        );
        if let Some(t) = v.cache.get(&e.key) {
            let size = t.size_vec2();
            let scale = (image.width() / size.x).min(image.height() / size.y);
            let fit = Rect::from_center_size(image.center(), size * scale);
            p.image(
                t.id(),
                fit,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            p.text(
                image.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                lr::semibold(12.0),
                lr::TEXT_DIM,
            );
        }
        let mut name = egui::text::LayoutJob::simple(
            e.name.clone(),
            lr::semibold(11.0),
            lr::TEXT,
            width - 12.0,
        );
        name.wrap.max_rows = 2;
        let g = p.layout_job(name);
        p.galley(
            Pos2::new(rect.left() + 6.0, rect.top() + image_h + 3.0),
            g,
            lr::TEXT,
        );
        let mut film = egui::text::LayoutJob::simple(
            sub_line(e),
            egui::FontId::monospace(9.5),
            lr::TEXT_DIM,
            width - 12.0,
        );
        film.wrap.max_rows = 1;
        let g = p.layout_job(film);
        p.galley(
            Pos2::new(rect.left() + 6.0, rect.bottom() - 15.0),
            g,
            lr::TEXT_DIM,
        );
        p.rect_stroke(
            rect, 0.0,
            Stroke::new(
                if selected { 2.0 } else { 1.0 },
                Color32::from_gray(if selected {
                    210
                } else if r.hovered() {
                    115
                } else {
                    46
                }),
            ),
            egui::StrokeKind::Inside,
        );
        if selected && scroll {
            r.scroll_to_me(None);
        }
    r
}

/// Size of a card in the look strip (image and one line of text).
pub fn line_card_size(v: &Previews, width: f32) -> (f32, f32) {
    (width, width / v.aspect_ratio.max(0.1) + 24.0)
}

/// One row of look cards with a single-line name. `filter`: 0 all, 1 black and white, 2 colour.
/// Returns the clicked look and the hovered one.
pub fn line(ui: &mut egui::Ui, v: &Previews, width: f32, filter: u8, current: &FilmSettings) -> (Option<Source>, Option<(Source, FilmSettings)>) {
    let (width, height) = line_card_size(v, width);
    let mut chosen = None;
    let mut hovered = None;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for e in v.entries.iter().filter(|e| filter == 0 || (filter == 1) == (e.group == 0)) {
            let (rect, r) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());
            let p = ui.painter_at(rect);
            let selected = e.film == *current;
            p.rect_filled(rect, 0.0, Color32::from_gray(if selected { 48 } else { 25 }));
            let image = Rect::from_min_max(rect.min + Vec2::splat(3.0), Pos2::new(rect.right() - 3.0, rect.bottom() - 22.0));
            if let Some(t) = v.cache.get(&e.key) {
                let size = t.size_vec2();
                let scale = (image.width() / size.x).min(image.height() / size.y);
                p.image(t.id(), Rect::from_center_size(image.center(), size * scale), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            } else {
                p.text(image.center(), egui::Align2::CENTER_CENTER, "…", lr::semibold(12.0), lr::TEXT_DIM);
            }
            let mut job = egui::text::LayoutJob::simple_singleline(e.name.clone(), egui::FontId::proportional(11.0), lr::TEXT);
            job.wrap = egui::text::TextWrapping { max_width: width - 10.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
            let g = p.layout_job(job);
            p.galley(Pos2::new(rect.left() + 5.0, rect.bottom() - 18.0), g, lr::TEXT);
            p.rect_stroke(rect, 0.0, Stroke::new(if selected { 1.5 } else { 1.0 }, Color32::from_gray(if selected { 210 } else if r.hovered() { 115 } else { 46 })), egui::StrokeKind::Inside);
            if r.hovered() {
                hovered = Some((e.source, e.film.clone()));
            }
            if r.clicked() {
                chosen = Some(e.source);
            }
        }
    });
    (chosen, hovered)
}

/// Second line of a film look: the film, own looks marked as "CUSTOM".
pub fn sub_line(e: &Entry) -> String {
    if matches!(e.source, Source::Own(_)) {
        if e.film_name.is_empty() { crate::tr("EIGEN").to_string() } else { crate::trf!("EIGEN · {a}", a = e.film_name) }
    } else {
        e.film_name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(i: usize, name: &str) -> Entry {
        let film = FilmSettings {
            active: true,
            stock: "kodak_trix_400".into(),
            ..Default::default()
        };
        Entry {
            source: Source::BuiltIn(i),
            name: name.into(),
            film_name: "Tri-X".into(),
            film,
            group: 0,
            full: None,
            series: 0,
            key: 0,
        }
    }
    #[test]
    fn loaded_recipe_keeps_preview_key_but_photo_and_sliders_change_it() {
        let mut v = Previews::default();
        let mut s = Settings::default();
        v.prepare(1, s.clone(), vec![entry(0, "A")], 0.0);
        let key = v.entries[0].key;
        s.film = FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            ..Default::default()
        };
        v.prepare(1, s.clone(), vec![entry(0, "A")], 1.0);
        assert_eq!(key, v.entries[0].key);
        v.prepare(1, s.clone(), vec![entry(0, "Anderer Name")], 2.0);
        assert_eq!(key, v.entries[0].key);
        s.exposure = 1.0;
        v.prepare(1, s.clone(), vec![entry(0, "A")], 3.0);
        assert_ne!(key, v.entries[0].key);
        let k = v.entries[0].key;
        v.prepare(2, s, vec![entry(0, "A")], 4.0);
        assert_ne!(k, v.entries[0].key);
    }
    #[test]
    fn old_job_fills_only_its_own_cache_images() {
        let ctx = egui::Context::default();
        let mut v = Previews::default();
        v.prepare(1, Settings::default(), vec![entry(0, "A")], 0.0);
        let old = v.entries[0].key;
        v.task = Some(Task {
            id: 7,
            keys: vec![old],
        });
        v.prepare(2, Settings::default(), vec![entry(0, "A")], 1.0);
        v.outcome(&ctx, 7, vec![(0, 2, 2, vec![128; 12])], true);
        assert_eq!(v.progress(), (0, 1));
        assert!(v.task.is_none());
        v.prepare(1, Settings::default(), vec![entry(0, "A")], 2.0);
        assert_eq!(v.progress(), (1, 1));
    }
}
