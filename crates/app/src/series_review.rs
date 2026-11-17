//! Series review: a view in the centre (like "Duplikate"), opened with N, from "Serien" on the left or from the
//! context menu. Three modes:
//!
//! - **Vergleich** (default): 2–4 photos of the series side by side, coupled zoom (Z: 1:1 at the sharpest region,
//!   slightly shifted burst frames aligned by their image signature), hints below each photo as short bars.
//! - **Leuchttisch**: all photos of the series as large tiles; a click puts one into the comparison (up to 4),
//!   ⌘-click takes it out.
//! - **Duell**: two photos; ← keeps the left one, → the right one, the other leaves, the next one moves up.
//!
//! Hints (sharpness, motion blur, clipping, faces and closed eyes with Apple Vision) are measured on the previews when
//! the series opens; the suggestion "best photo" comes with its reason and never sets anything by itself. Decisions use
//! the usual ways: the best photo gets the pick flag and becomes the cover, ⇧X rejects the rest in one command
//! (one ⌘Z), ⌘↩ marks the series as reviewed and goes on to the next one.
//!
//! Submodule of `library_ui` (accesses its fields).

use std::sync::mpsc::Receiver;

use super::*;
use library::series_quality::{self as sq, Face, Quality, Suggestion, Why};

/// ⇧X "lower" sets the rest to 0 stars, but only photos with at most this many stars: 4 and 5 stars stay.
pub(crate) const LOWER_KEEPS_FROM: u8 = 3;

/// ⇧X "lower": (photos set to 0 stars, photos that keep their 4 or 5 stars).
fn split_lowered(rest: Vec<i64>, stars: impl Fn(i64) -> u8) -> (Vec<i64>, Vec<i64>) {
    rest.into_iter().partition(|&b| stars(b) <= LOWER_KEEPS_FROM)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Compare,
    LightTable,
    Duel,
}

/// A command from a button, carried out after drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Choose,
    Rest,
    Finish,
    Close,
}

/// Measured hints of one photo of the series.
#[derive(Clone, Default)]
struct Hint {
    quality: Option<Quality>,
    faces: Option<Vec<Face>>,
    signature: Option<Vec<u8>>,
    /// focus point of the camera (centre of the main area, 0..1 of the upright image)
    focus: Option<[f32; 2]>,
}

impl Hint {
    /// The camera's focus point lies beside every face found (only a hint, no rating).
    fn focus_beside_faces(&self) -> bool {
        let (Some([x, y]), Some(faces)) = (self.focus, &self.faces) else { return false };
        !faces.is_empty() && !faces.iter().any(|f| x >= f.x - f.w * 0.25 && x <= f.x + f.w * 1.25 && y >= f.y - f.h * 0.25 && y <= f.y + f.h * 1.25)
    }

    /// Where the coupled zoom goes: the camera's focus point, else the sharpest (largest, best) face, else the
    /// sharpest region.
    fn zoom_target(&self) -> Option<egui::Pos2> {
        if let Some([x, y]) = self.focus {
            return Some(egui::pos2(x, y));
        }
        if let Some(f) = self.faces.as_ref().and_then(|f| f.iter().max_by(|a, b| (a.w * a.quality.unwrap_or(0.5)).total_cmp(&(b.w * b.quality.unwrap_or(0.5))))) {
            return Some(egui::pos2(f.x + f.w / 2.0, f.y + f.h * 0.4));
        }
        self.quality.map(|q| egui::pos2(q.sharp_x, q.sharp_y))
    }
}

pub(super) struct Review {
    /// the stack (`None`: a selection without a stack)
    pub stack: Option<i64>,
    /// images in capture order
    images: Vec<i64>,
    mode: Mode,
    /// places in `images` shown in the comparison (2–4)
    compare: Vec<usize>,
    /// focused field (index into `compare`; in the duel 0 = left, 1 = right)
    focus: usize,
    /// duel: places still waiting, places out
    queue: Vec<usize>,
    out: Vec<usize>,
    zoom: bool,
    /// zoom centre (0..1 of the first photo); `None`: the sharpest region
    zoom_at: Option<egui::Pos2>,
    faces_bar: bool,
    hints: HashMap<i64, Hint>,
    hints_rx: Option<Receiver<(i64, Hint)>>,
    suggestion: Suggestion,
    pending: Option<Pending>,
}

impl Review {
    fn new(stack: Option<i64>, images: Vec<i64>, cover: Option<i64>) -> Review {
        let n = images.len();
        // the cover and the photos after it first
        let start = cover.and_then(|c| images.iter().position(|&i| i == c)).unwrap_or(0);
        let mut compare: Vec<usize> = (0..n.min(4)).map(|k| (start + k) % n.max(1)).collect();
        compare.sort_unstable();
        let mut r = Review {
            stack,
            images,
            mode: Mode::Compare,
            compare,
            focus: 0,
            queue: Vec::new(),
            out: Vec::new(),
            zoom: false,
            zoom_at: None,
            faces_bar: true,
            hints: HashMap::new(),
            hints_rx: None,
            suggestion: Suggestion::default(),
            pending: None,
        };
        // large series: the duel is quicker than comparing four at once
        if n > 10 {
            r.start_duel();
        }
        r
    }

    fn start_duel(&mut self) {
        self.mode = Mode::Duel;
        self.out.clear();
        self.queue = (2..self.images.len()).collect();
        self.compare = (0..self.images.len().min(2)).collect();
        self.focus = 0;
    }

    /// The focused photo.
    fn focused(&self) -> Option<i64> {
        self.compare.get(self.focus).and_then(|&k| self.images.get(k)).copied()
    }
}

/// Order of the series for ↓/↑ and "next": not reviewed first, newest day first (as the source "Serien").
fn series_order(stacks: &[library::series::Stack]) -> Vec<i64> {
    let mut v: Vec<&library::series::Stack> = stacks.iter().filter(|s| s.is_series()).collect();
    v.sort_by_key(|s| (s.reviewed.is_some(), std::cmp::Reverse(s.day.clone()), s.id));
    v.iter().map(|s| s.id).collect()
}

fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// Image rectangle of a photo fitted into `area`.
fn fit(tex: &TextureHandle, area: egui::Rect) -> egui::Rect {
    let s = tex.size_vec2();
    let k = (area.width() / s.x).min(area.height() / s.y);
    egui::Rect::from_center_size(area.center(), s * k)
}

impl LibraryView {
    /// Test aid: mode of the review just opened ("light", "duel", "zoom").
    pub(super) fn series_review_test_mode(&mut self, mode: &str) {
        let Some(r) = self.series.review.as_mut() else { return };
        match mode {
            "light" => r.mode = Mode::LightTable,
            "duel" => r.start_duel(),
            "zoom" => r.zoom = true,
            _ => {}
        }
    }

    /// Is the review open (keys and rating go to it)?
    pub fn review_open(&self) -> bool {
        self.series.review.is_some()
    }

    /// The photo a rating key applies to in the review.
    pub fn review_focus(&self) -> Option<i64> {
        self.series.review.as_ref().and_then(|r| r.focused())
    }

    fn capture_of(&self, b: &i64) -> String {
        self.index.get(b).and_then(|&i| self.rows[i].capture.clone()).unwrap_or_default()
    }

    /// Open the review for a stack (`None`: the selection, if it is 2–60 photos).
    pub(super) fn series_review_open(&mut self, stack: Option<i64>, actions: &mut Vec<Action>) {
        let (images, cover) = match stack.and_then(|id| self.series.stacks.iter().find(|s| s.id == id)) {
            Some(s) => {
                let mut v = s.images.clone();
                v.retain(|b| self.index.contains_key(b));
                v.sort_by_key(|b| self.capture_of(b));
                (v, Some(s.top()))
            }
            None => {
                let mut sel: Vec<i64> = self.target();
                sel.retain(|b| self.index.get(b).is_some_and(|&i| self.rows[i].copy.is_none()));
                if !(2..=60).contains(&sel.len()) {
                    actions.push(Action::Status(crate::tr("Zum Sichten eine Serie oder 2–60 Fotos auswählen").into()));
                    return;
                }
                sel.sort_by_key(|b| self.capture_of(b));
                (sel, None)
            }
        };
        if images.len() < 2 {
            return;
        }
        let stack = stack.filter(|id| self.series.stacks.iter().any(|s| s.id == *id));
        let mut r = Review::new(stack, images, cover);
        r.hints_rx = Some(self.review_measure(&r.images));
        self.series.review = Some(r);
        self.loupe = false;
    }

    /// Measure the photos of the review in the background: stored hints first, missing ones from the previews (never the
    /// originals), faces with Vision (macOS). Results arrive one by one.
    fn review_measure(&self, images: &[i64]) -> Receiver<(i64, Hint)> {
        let (tx, rx) = std::sync::mpsc::channel();
        let tasks: Vec<(i64, String, i64, i64, Option<i64>)> = images
            .iter()
            .filter_map(|b| self.index.get(b).map(|&i| &self.rows[i]))
            .map(|z| (z.file, row_path(&self.folder_paths, z), z.size.unwrap_or(0), z.mtime.unwrap_or(0), z.orientation))
            .collect();
        let path = self.db.as_ref().map(|d| d.path.clone());
        let cache = self.generator.as_ref().map(|g| g.cache().clone());
        let ctx = self.quick_ctx.clone();
        let _ = std::thread::Builder::new().name("serie-sichten".into()).spawn(move || {
            library::platform::set_priority(library::platform::Priority::Utility);
            let db = path.and_then(|p| Db::open(&p).ok());
            let files: Vec<i64> = tasks.iter().map(|t| t.0).collect();
            let stored = db.as_ref().map(|d| sq::load(&d.conn, &files)).unwrap_or_default();
            for (file, p, size, mtime, orientation) in tasks {
                let signature = db.as_ref().and_then(|d| d.conn.query_row("SELECT signature FROM image_quality WHERE file = ?1", [file], |r| r.get::<_, Option<Vec<u8>>>(0)).ok().flatten());
                // focus area from the file header (a few milliseconds; nothing on an unreachable drive)
                let focus = raw::focus::read(std::path::Path::new(&p)).and_then(|info| {
                    let g = info.fields.iter().find(|g| g.achieved).or_else(|| info.fields.iter().find(|g| g.selected))?;
                    Some(raw::focus::upright([g.rect[0], g.rect[1]], info.orientation))
                });
                let mut hint = Hint { quality: stored.get(&file).copied(), faces: None, signature, focus };
                // the preview may still be on its way: try for a while
                let mut img = None;
                for _ in 0..40 {
                    img = cache.as_ref().and_then(|c| crate::series_job::preview_for(c, file, &p, size, mtime, orientation));
                    if img.is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                if let Some(img) = img {
                    let (q, faces) = sq::measure_all(&img);
                    if let Some(d) = &db {
                        let _ = sq::save(d, file, &q);
                    }
                    hint.quality = Some(q);
                    hint.faces = faces;
                    if hint.signature.is_none() {
                        hint.signature = Some(library::series::signature(&img));
                    }
                }
                if tx.send((file, hint)).is_err() {
                    return;
                }
                if let Some(c) = &ctx {
                    c.request_repaint();
                }
            }
        });
        rx
    }

    /// The stacks were read again: the open review follows its stack (closes when it is gone).
    pub(super) fn series_review_refresh(&mut self) {
        let Some(r) = &self.series.review else { return };
        let Some(id) = r.stack else { return };
        if !self.series.stacks.iter().any(|s| s.id == id) {
            self.series.review = None;
        }
    }

    fn review_stack(&self) -> Option<&library::series::Stack> {
        let id = self.series.review.as_ref()?.stack?;
        self.series.stacks.iter().find(|s| s.id == id)
    }

    fn review_close(&mut self) {
        let focus = self.review_focus();
        self.series.review = None;
        if let Some(f) = focus {
            self.active = Some(f);
            self.to_active = true;
        }
        self.requested.clear();
    }

    /// Go to another series (`step` in the order of [`series_order`]; `unreviewed`: the next one not reviewed).
    fn review_go(&mut self, step: isize, unreviewed: bool, actions: &mut Vec<Action>) {
        let order = series_order(&self.series.stacks);
        if order.is_empty() {
            return;
        }
        let cur = self.series.review.as_ref().and_then(|r| r.stack);
        let pos = cur.and_then(|c| order.iter().position(|&x| x == c));
        let next = if unreviewed {
            order.iter().copied().find(|&id| Some(id) != cur && self.series.stacks.iter().any(|s| s.id == id && s.reviewed.is_none()))
        } else {
            let p = pos.map_or(0, |p| (p as isize + step).clamp(0, order.len() as isize - 1) as usize);
            order.get(p).copied().filter(|&id| Some(id) != cur)
        };
        match next {
            Some(id) => self.series_review_open(Some(id), actions),
            None => {
                actions.push(Action::Status(crate::tr("Keine weitere Serie zu sichten").into()));
                if unreviewed {
                    self.review_close();
                }
            }
        }
    }

    /// Choose the focused photo as the best one: pick flag (rating way, ⌘Z) and cover of the stack (⌘Z).
    fn review_choose(&mut self, actions: &mut Vec<Action>) {
        let Some(best) = self.review_focus() else { return };
        let new = self.rate_images(&[best], None, Some(1), None);
        if !new.is_empty() {
            actions.push(Action::Rated(new));
        }
        if let Some((id, top)) = self.review_stack().map(|s| (s.id, s.top())) {
            if top != best {
                if let Some(k) = self.series.stacks.iter().position(|s| s.id == id) {
                    self.series.stacks[k].cover = Some(best);
                }
                self.series_change(move |db| library::series::set_cover(db, id, best), crate::tr("Als bestes Foto gewählt").to_string());
            }
        }
        actions.push(Action::Status(crate::tr("Als bestes Foto gewählt").into()));
    }

    /// ⇧X: the rest – all photos of the series except the picked ones – rejected (or lowered to 0 stars, setting
    /// "stackRest"; photos with more than [`LOWER_KEEPS_FROM`] stars keep their rating).
    fn review_rest(&mut self, actions: &mut Vec<Action>) {
        let Some(r) = &self.series.review else { return };
        let chosen: Vec<i64> = r.images.iter().copied().filter(|b| self.index.get(b).is_some_and(|&i| self.rows[i].flag == 1)).collect();
        if chosen.is_empty() {
            actions.push(Action::Status(crate::tr("Erst das beste Foto wählen (Leertaste oder ↩)").into()));
            return;
        }
        let rest: Vec<i64> = r.images.iter().copied().filter(|b| !chosen.contains(b)).collect();
        if rest.is_empty() {
            return;
        }
        let mode = library::settings::value("stackRest");
        let lower = mode.as_str() == Some("lower");
        // lowering never takes away a high rating: photos with 4 or 5 stars keep them (a decision made before, which
        // ⌘Z after a restart could not bring back)
        let (rest, kept) = if lower { split_lowered(rest, |b| self.index.get(&b).map_or(0, |&i| self.rows[i].stars)) } else { (rest, Vec::new()) };
        let new = match mode.as_str() {
            Some("lower") => self.rate_images(&rest, Some(0), None, None),
            Some("none") => Vec::new(),
            _ => self.rate_images(&rest, None, Some(-1), None),
        };
        if !new.is_empty() || !kept.is_empty() {
            let n = new.len();
            if !new.is_empty() {
                actions.push(Action::Rated(new));
            }
            let mut what = if lower { crate::trf!("Rest abgestuft ({n} Fotos) – ⌘Z macht es rückgängig", n = n) } else { crate::trf!("Rest abgelehnt ({n} Fotos) – ⌘Z macht es rückgängig", n = n) };
            if !kept.is_empty() {
                what = format!("{what} · {}", crate::trf!("{n} mit 4–5 Sternen unverändert", n = kept.len()));
            }
            actions.push(Action::Status(what));
        }
    }

    /// ⌘↩: the series is reviewed (collapses with the default setting), on to the next one.
    fn review_finish(&mut self, actions: &mut Vec<Action>) {
        let Some(id) = self.review_stack().map(|s| s.id) else {
            self.review_close();
            return;
        };
        if let Some(k) = self.series.stacks.iter().position(|s| s.id == id) {
            self.series.stacks[k].reviewed = Some(library::now());
            if library::settings::value("stackCollapse").as_str() != Some("never") {
                self.series.stacks[k].collapsed = true;
            }
        }
        self.refilter = true;
        self.series_change(move |db| library::series::mark_reviewed(db, id), crate::tr("Serie gesichtet").to_string());
        self.review_go(1, true, actions);
    }

    /// Keys of the review (before the grid's keys). `true`: handled, the grid sees none of them.
    pub(super) fn review_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
        let pressed = |id: &str| crate::shortcuts::pressed(ctx, id);
        if pressed("review_finish") {
            self.review_finish(actions);
            return true;
        }
        if ctx.input(|i| i.modifiers.command) {
            // ⌘Z and the other ⌘ keys: as in the grid
            return false;
        }
        if pressed("review_back") {
            self.review_close();
            return true;
        }
        if pressed("review_rest") {
            self.review_rest(actions);
            return true;
        }
        if pressed("review_choose") || pressed("review_choose_space") {
            self.review_choose(actions);
            return true;
        }
        if pressed("review_next") {
            self.review_go(1, false, actions);
            return true;
        }
        if pressed("review_prev") {
            self.review_go(-1, false, actions);
            return true;
        }
        let Some(r) = self.series.review.as_mut() else { return false };
        if pressed("review_zoom") {
            r.zoom = !r.zoom;
            r.zoom_at = None;
        }
        if pressed("review_faces") {
            r.faces_bar = !r.faces_bar;
        }
        if let Some(n) = crate::shortcuts::which(ctx, "review_field") {
            if n < r.compare.len() {
                r.focus = n;
            }
        }
        let p = |k: egui::Key| ctx.input(|i| i.key_pressed(k) && !i.modifiers.shift && !i.modifiers.alt);
        let (left, right) = (p(egui::Key::ArrowLeft), p(egui::Key::ArrowRight));
        if r.mode == Mode::Duel && r.compare.len() == 2 {
            // ← keeps the left one, → the right one
            if left || right {
                let lose = if left { 1 } else { 0 };
                r.out.push(r.compare[lose]);
                match r.queue.first().copied() {
                    Some(next) => {
                        r.queue.remove(0);
                        r.compare[lose] = next;
                    }
                    // decided: the winner stays alone in focus
                    None => {
                        r.compare.remove(lose);
                    }
                }
                r.focus = if left { 0 } else { r.compare.len() - 1 };
            }
        } else if left {
            r.focus = r.focus.saturating_sub(1);
        } else if right {
            r.focus = (r.focus + 1).min(r.compare.len().saturating_sub(1));
        }
        true
    }

    /// Take the measured hints and compute the suggestion when something new came.
    fn review_poll(&mut self) {
        let Some(r) = self.series.review.as_mut() else { return };
        let mut changed = false;
        if let Some(rx) = &r.hints_rx {
            while let Ok((file, h)) = rx.try_recv() {
                r.hints.insert(file, h);
                changed = true;
            }
        }
        if !changed {
            return;
        }
        let bracket = self.series.stacks.iter().any(|s| Some(s.id) == r.stack && s.kind == library::series::Kind::Bracket);
        let cands: Vec<sq::Candidate> = r
            .images
            .iter()
            .map(|b| {
                let z = self.index.get(b).map(|&i| &self.rows[i]);
                sq::Candidate { quality: z.and_then(|z| r.hints.get(&z.file)).and_then(|h| h.quality), stars: z.map_or(0, |z| z.stars), flag: z.map_or(0, |z| z.flag) }
            })
            .collect();
        let s = sq::suggest(&cands);
        r.suggestion = if bracket { Suggestion { sharp: s.sharp, ..Suggestion::default() } } else { s };
    }

    /// Request the previews the review shows (large level for the fields and faces, small for strip and light table).
    fn review_request(&mut self, large: &[i64], small: &[i64]) {
        let (lv, sv) = (preview::large(), preview::small());
        let mut wanted = Vec::new();
        let mut jobs = Vec::new();
        for (list, level) in [(large, lv), (small, sv)] {
            for b in list {
                let Some(&i) = self.index.get(b) else { continue };
                let z = &self.rows[i];
                if self.textures.contains_key(&(z.file, level)) || wanted.contains(&(z.file, level)) {
                    continue;
                }
                wanted.push((z.file, level));
                jobs.push(self.task(z, level));
            }
        }
        if wanted != self.requested {
            if let Some(e) = &self.generator {
                e.visible(jobs);
            }
            self.requested = wanted;
        }
    }

    /// Centre: header, fields (or light table), strip, faces.
    pub(super) fn review_center(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        self.review_poll();
        let Some(mut r) = self.series.review.take() else { return };
        // the info panel on the right shows the focused photo
        if let Some(f) = r.focused() {
            self.active = Some(f);
        }
        self.review_ui(ui, &mut r);
        let pending = r.pending.take();
        self.series.review = Some(r);
        match pending {
            Some(Pending::Choose) => self.review_choose(actions),
            Some(Pending::Rest) => self.review_rest(actions),
            Some(Pending::Finish) => self.review_finish(actions),
            Some(Pending::Close) => self.review_close(),
            None => {}
        }
    }

    fn review_ui(&mut self, ui: &mut egui::Ui, r: &mut Review) {
        let stack = r.stack.and_then(|id| self.series.stacks.iter().find(|s| s.id == id)).cloned();
        let order = series_order(&self.series.stacks);
        let n = r.images.len();
        // ── header: what it is, where in the list, time span, why
        let head = {
            let t = |b: Option<&i64>| b.map(|b| self.capture_of(b)).and_then(|c| c.get(11..19).map(str::to_string)).unwrap_or_default();
            let kind = match &stack {
                Some(s) if s.kind == library::series::Kind::Bracket => crate::tr("Belichtungsreihe"),
                Some(s) if s.is_series() => crate::tr("Serie"),
                Some(_) => crate::tr("Stapel"),
                None => crate::tr("Auswahl"),
            };
            let place = stack.as_ref().and_then(|s| order.iter().position(|&x| x == s.id)).map(|p| format!(" {} / {}", p + 1, order.len())).unwrap_or_default();
            format!("{}{place} · {} · {}–{}", kind.to_uppercase(), crate::trf!("{n} Fotos", n = n), t(r.images.first()), t(r.images.last()))
        };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let h = ui.label(egui::RichText::new(head).font(lr::semibold(12.0)).color(Color32::from_gray(0xe0)));
            // why the photos belong together: on the header (the row keeps its width)
            if let Some(s) = &stack {
                lr::hover_tip(ui, &h, &library_series::reason_text(s));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                let back = ui.add(egui::Button::new(crate::tr("Zum Raster")).corner_radius(0.0));
                lr::tip_key(ui, &back, "Zurück zum Raster", "review_back");
                if back.clicked() {
                    r.pending = Some(Pending::Close);
                }
                if stack.is_some() {
                    let next = lr::main_button(ui, crate::tr("Gesichtet, weiter"), true);
                    lr::tip_key(ui, &next, "Serie als gesichtet abschließen und zur nächsten", "review_finish");
                    if next.clicked() {
                        r.pending = Some(Pending::Finish);
                    }
                }
                crate::help::info(ui, "serien", "Serien sichten", &crate::help::SERIES_REVIEW);
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            for (m, text, tip) in [
                (Mode::Compare, "Vergleich", "2–4 Fotos groß nebeneinander"),
                (Mode::LightTable, "Leuchttisch", "Alle Fotos der Serie; Klick legt eins in den Vergleich"),
                (Mode::Duel, "Duell", "Zwei Fotos: ← behält das linke, → das rechte"),
            ] {
                let b = ui.selectable_label(r.mode == m, crate::tr(text));
                lr::hover_tip(ui, &b, crate::tr(tip));
                if b.clicked() && r.mode != m {
                    if m == Mode::Duel {
                        r.start_duel();
                    } else {
                        r.mode = m;
                        if r.compare.len() < 2 {
                            r.compare = (0..n.min(4)).collect();
                        }
                    }
                }
            }
            ui.add_space(16.0);
            let best = ui.add(egui::Button::new(crate::tr("Als bestes wählen")).corner_radius(0.0));
            lr::tip_key(ui, &best, "Fokussiertes Foto als bestes wählen: Titelfoto und Markierung „Auswahl“", "review_choose");
            if best.clicked() {
                r.pending = Some(Pending::Choose);
            }
            let rest = ui.add(egui::Button::new(crate::tr("Rest ablehnen")).corner_radius(0.0));
            lr::tip_key(ui, &rest, "Alle Fotos außer den markierten ablehnen (ein ⌘Z)", "review_rest");
            if rest.clicked() {
                r.pending = Some(Pending::Rest);
            }
            let zoom = ui.selectable_label(r.zoom, "1:1");
            lr::tip_key(ui, &zoom, "Gekoppelter Zoom auf die schärfste Stelle (Doppelklick: an dieser Stelle)", "review_zoom");
            if zoom.clicked() {
                r.zoom = !r.zoom;
                r.zoom_at = None;
            }
            ui.add_space(12.0);
            let text = self.suggestion_text(r, stack.as_ref());
            ui.label(egui::RichText::new(text).color(Color32::from_gray(0xc8)));
        });
        ui.add_space(4.0);
        // ── areas
        let avail = ui.available_rect_before_wrap().intersect(ui.clip_rect().shrink2(egui::vec2(8.0, 0.0)));
        let persons = if r.faces_bar { self.review_persons(r).len().min(3) } else { 0 };
        let strip_h = 86.0;
        let faces_h = if persons > 0 { persons as f32 * 64.0 + 22.0 } else { 0.0 };
        let fields = egui::Rect::from_min_max(avail.min, egui::pos2(avail.max.x, (avail.max.y - strip_h - faces_h).max(avail.min.y + 120.0)));
        let strip = egui::Rect::from_min_size(egui::pos2(avail.min.x, fields.max.y + 4.0), egui::vec2(avail.width(), strip_h - 4.0));
        let faces_area = egui::Rect::from_min_max(egui::pos2(avail.min.x, strip.max.y + 4.0), avail.max);
        ui.allocate_rect(avail, egui::Sense::hover());
        let mut large: Vec<i64> = Vec::new();
        match r.mode {
            Mode::LightTable => self.review_light_table(ui, r, fields),
            _ => {
                large = r.compare.iter().map(|&k| r.images[k]).collect();
                self.review_fields(ui, r, fields);
            }
        }
        self.review_strip(ui, r, strip);
        if persons > 0 {
            large.extend(r.images.iter().copied());
            self.review_faces_bar(ui, r, faces_area);
        }
        large.dedup();
        let small = r.images.clone();
        self.review_request(&large, &small);
    }

    fn suggestion_text(&self, r: &Review, stack: Option<&library::series::Stack>) -> String {
        let n = r.images.len();
        if stack.is_some_and(|s| s.kind == library::series::Kind::Bracket) {
            return crate::tr("Belichtungsreihe: kein Vorschlag (Material zum Zusammenfügen)").to_string();
        }
        if r.hints.len() < n {
            return crate::trf!("Hinweise werden gemessen ({a}/{n}) …", a = r.hints.len(), n = n);
        }
        if r.suggestion.close {
            return crate::tr("Kaum Unterschied – selbst entscheiden").to_string();
        }
        let Some(b) = r.suggestion.best else { return String::new() };
        let why: Vec<&str> = r
            .suggestion
            .why
            .iter()
            .map(|w| match w {
                Why::Sharpest => crate::tr("schärfstes Foto"),
                Why::EyesOpen => crate::tr("alle Augen offen"),
                Why::NoMotionBlur => crate::tr("ohne Bewegungsunschärfe"),
                Why::NoClipping => crate::tr("ohne abgeschnittene Lichter"),
                Why::RatedBest => crate::tr("von dir am besten bewertet"),
            })
            .collect();
        let why = if why.is_empty() { String::new() } else { format!(" – {}", why.join(", ")) };
        crate::trf!("Vorschlag: {k}/{n}{w}", k = b + 1, n = n, w = why)
    }

    fn review_fields(&mut self, ui: &mut egui::Ui, r: &mut Review, area: egui::Rect) {
        let count = r.compare.len().max(1);
        let gap = 6.0;
        let w = (area.width() - gap * (count as f32 - 1.0)) / count as f32;
        let hints_h = 30.0;
        let ppp = ui.ctx().pixels_per_point();
        let file_of = |s: &LibraryView, b: i64| s.index.get(&b).map(|&i| s.rows[i].file);
        // alignment of slightly shifted frames: offset against the first photo of the comparison
        let first = r.compare.first().map(|&k| r.images[k]);
        let base_sig = first.and_then(|b| file_of(self, b)).and_then(|f| r.hints.get(&f)).and_then(|h| h.signature.clone());
        let base_target = first.and_then(|b| file_of(self, b)).and_then(|f| r.hints.get(&f)).and_then(|h| h.zoom_target());
        // every photo with its own focus point (tracking follows the subject): each field zooms to its own
        let own_focus = r.compare.iter().all(|&k| file_of(self, r.images[k]).and_then(|f| r.hints.get(&f)).is_some_and(|h| h.focus.is_some()));
        for (slot, k) in r.compare.clone().into_iter().enumerate() {
            let img = r.images[k];
            let Some(&ri) = self.index.get(&img) else { continue };
            let z = self.rows[ri].clone();
            let cell = egui::Rect::from_min_size(egui::pos2(area.left() + slot as f32 * (w + gap), area.top()), egui::vec2(w, area.height()));
            let pic = egui::Rect::from_min_max(cell.min, egui::pos2(cell.max.x, cell.max.y - hints_h));
            let resp = ui.interact(pic, egui::Id::unique(("sichten-feld", img)), egui::Sense::click());
            let p = ui.painter_at(pic);
            p.rect_filled(pic, 0.0, lr::IMAGE_BG);
            let hint = r.hints.get(&z.file).cloned().unwrap_or_default();
            if let Some(t) = self.texture(z.file, preview::large()) {
                if r.zoom {
                    // 1:1 preview pixels around the zoom centre (sharpest region of the first photo, else the click)
                    let own = (own_focus && r.zoom_at.is_none()).then(|| hint.focus.map(|[x, y]| egui::pos2(x, y))).flatten();
                    let base = own.or(r.zoom_at).or(base_target).unwrap_or(egui::pos2(0.5, 0.5));
                    let shift = match (&base_sig, &hint.signature) {
                        (Some(a), Some(b)) if slot > 0 && own.is_none() => library::series::signature_shift(a, b).unwrap_or((0.0, 0.0)),
                        _ => (0.0, 0.0),
                    };
                    let ts = t.size_vec2();
                    let size = egui::vec2((pic.width() * ppp / ts.x).min(1.0), (pic.height() * ppp / ts.y).min(1.0));
                    let uv = egui::Rect::from_center_size(egui::pos2(base.x + shift.0, base.y + shift.1), size);
                    // keep the crop inside the photo
                    let dx = (-uv.min.x).max(0.0) - (uv.max.x - 1.0).max(0.0);
                    let dy = (-uv.min.y).max(0.0) - (uv.max.y - 1.0).max(0.0);
                    p.image(t.id(), pic, uv.translate(egui::vec2(dx, dy)), Color32::WHITE);
                } else {
                    let fr = fit(&t, pic.shrink(4.0));
                    let tint = if z.flag == -1 { Color32::WHITE.gamma_multiply(0.45) } else { Color32::WHITE };
                    p.image(t.id(), fr, full_uv(), tint);
                    // faces with closed eyes: a thin frame
                    for f in hint.faces.iter().flatten().filter(|f| f.eyes_closed) {
                        let fr2 = egui::Rect::from_min_size(fr.min + egui::vec2(f.x * fr.width(), f.y * fr.height()), egui::vec2(f.w * fr.width(), f.h * fr.height()));
                        p.rect_stroke(fr2, 0.0, egui::Stroke::new(1.0, Color32::from_rgb(220, 120, 90)), egui::StrokeKind::Outside);
                    }
                }
            } else {
                p.text(pic.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(14.0), Color32::from_gray(0x80));
            }
            if resp.clicked() {
                r.focus = slot;
            }
            if resp.double_clicked() {
                r.focus = slot;
                r.zoom = !r.zoom;
                r.zoom_at = resp.interact_pointer_pos().map(|q| egui::pos2(((q.x - pic.left()) / pic.width()).clamp(0.0, 1.0), ((q.y - pic.top()) / pic.height()).clamp(0.0, 1.0)));
            }
            // marks: place, suggestion, decision
            let mut tag = format!("{}/{}", k + 1, r.images.len());
            if r.suggestion.best == Some(k) {
                tag = format!("{tag} · {}", crate::tr("VORSCHLAG"));
            }
            let g = p.layout_no_wrap(tag, lr::semibold(10.5), Color32::WHITE);
            let tr = egui::Rect::from_min_size(pic.min + egui::vec2(6.0, 6.0), g.size() + egui::vec2(10.0, 6.0));
            p.rect_filled(tr, 0.0, Color32::from_black_alpha(180));
            p.galley(tr.min + egui::vec2(5.0, 3.0), g, Color32::WHITE);
            if z.flag != 0 {
                lr::paint_flag(&p, pic.right_top() + egui::vec2(-16.0, 16.0), 18.0, z.flag, true, Color32::from_gray(0xf0));
            }
            if slot == r.focus {
                ui.painter().rect_stroke(pic, 0.0, egui::Stroke::new(2.0, Color32::from_gray(0xe8)), egui::StrokeKind::Inside);
            }
            let hr = egui::Rect::from_min_max(egui::pos2(cell.left(), pic.bottom() + 2.0), cell.max);
            self.review_hint_line(ui, hr, &hint, r.suggestion.sharp.get(k).copied(), img);
        }
    }

    /// Hints as short bars and words: sharpness (relative within the series), eyes, highlights, motion.
    fn review_hint_line(&self, ui: &egui::Ui, area: egui::Rect, hint: &Hint, sharp: Option<f32>, image: i64) {
        let p = ui.painter();
        let y = area.top() + 10.0;
        let Some(q) = hint.quality else {
            p.text(egui::pos2(area.left() + 4.0, y), egui::Align2::LEFT_CENTER, crate::tr("wird gemessen …"), egui::FontId::proportional(10.5), lr::TEXT_DIM);
            return;
        };
        let mut x = area.left() + 4.0;
        let label = p.text(egui::pos2(x, y), egui::Align2::LEFT_CENTER, crate::tr("Schärfe"), egui::FontId::proportional(10.5), Color32::from_gray(0xb0));
        x = label.right() + 6.0;
        let filled = sharp.map_or(0, |s| ((s * 4.0).round() as usize).clamp(1, 4));
        let bars = egui::Rect::from_min_size(egui::pos2(x, y - 5.0), egui::vec2(4.0 * 9.0, 10.0));
        for b in 0..4 {
            let r = egui::Rect::from_min_size(egui::pos2(x + b as f32 * 9.0, y - 5.0), egui::vec2(7.0, 10.0));
            if b < filled {
                p.rect_filled(r, 0.0, Color32::from_gray(0xd8));
            } else {
                p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x60)), egui::StrokeKind::Inside);
            }
        }
        let tip = ui.interact(bars.union(label), egui::Id::unique(("sichten-schaerfe", image)), egui::Sense::hover());
        lr::hover_tip(ui, &tip, &crate::trf!("Schärfe: {a} % des schärfsten Fotos der Serie (gemessen an der schärfsten Stelle)", a = (sharp.unwrap_or(0.0) * 100.0).round() as u32));
        x = bars.right() + 12.0;
        let mut words: Vec<(String, Color32, &str)> = Vec::new();
        if let (Some(faces), Some(closed)) = (q.faces, q.eyes_closed) {
            if faces > 0 {
                let tip = "Gesichter und Augen (Apple Vision); geschlossene Augen sind im Foto umrandet";
                if closed == 0 {
                    words.push((crate::trf!("Augen {n}/{n} offen", n = faces), Color32::from_gray(0xb0), tip));
                } else {
                    words.push((crate::trf!("Augen {a}/{n} zu", a = closed, n = faces), Color32::from_rgb(220, 140, 110), tip));
                }
            }
        }
        if q.clip_high > 0.005 {
            words.push((crate::trf!("Lichter {a} %", a = format!("{:.0}", (q.clip_high * 100.0).max(1.0))), Color32::from_rgb(220, 180, 110), "Anteil abgeschnittener Lichter in der Vorschau"));
        }
        if hint.focus_beside_faces() {
            words.push((crate::tr("Fokus neben dem Gesicht").to_string(), Color32::from_rgb(220, 180, 110), "Das Fokusfeld der Kamera berührt keines der gefundenen Gesichter (nur ein Hinweis)"));
        }
        if q.motion > 0.6 {
            words.push((crate::tr("verwischt").to_string(), Color32::from_rgb(220, 180, 110), "Die Kanten sind in einer Richtung verwischt (Bewegungsunschärfe)"));
        }
        for (k, (t, c, tip)) in words.into_iter().enumerate() {
            let r = p.text(egui::pos2(x, y), egui::Align2::LEFT_CENTER, t, egui::FontId::proportional(10.5), c);
            let h = ui.interact(r, egui::Id::unique(("sichten-wort", image, k)), egui::Sense::hover());
            lr::hover_tip(ui, &h, crate::tr(tip));
            x = r.right() + 10.0;
        }
    }

    fn review_light_table(&mut self, ui: &mut egui::Ui, r: &mut Review, area: egui::Rect) {
        let n = r.images.len();
        let cols = match n {
            0..=4 => 2,
            5..=9 => 3,
            _ => 4,
        }
        .min(n.max(1));
        let rows = n.div_ceil(cols).max(1);
        let size = egui::vec2(area.width() / cols as f32, (area.height() / rows as f32).max(80.0));
        let cmd = ui.input(|i| i.modifiers.command);
        for (k, img) in r.images.clone().into_iter().enumerate() {
            let cell = egui::Rect::from_min_size(area.min + egui::vec2((k % cols) as f32 * size.x, (k / cols) as f32 * size.y), size).shrink(3.0);
            if cell.top() > area.bottom() {
                break;
            }
            let Some(&ri) = self.index.get(&img) else { continue };
            let z = self.rows[ri].clone();
            let resp = ui.interact(cell, egui::Id::unique(("leuchttisch", img)), egui::Sense::click());
            let p = ui.painter_at(cell);
            p.rect_filled(cell, 0.0, if resp.hovered() { Color32::from_gray(0x26) } else { Color32::from_gray(0x1a) });
            let inner = cell.shrink(6.0);
            let pic = egui::Rect::from_min_max(inner.min, egui::pos2(inner.max.x, inner.max.y - 20.0));
            if let Some(t) = self.texture(z.file, preview::small()) {
                let tint = if z.flag == -1 { Color32::WHITE.gamma_multiply(0.45) } else { Color32::WHITE };
                p.image(t.id(), fit(&t, pic), full_uv(), tint);
            }
            if r.compare.contains(&k) {
                p.rect_stroke(cell, 0.0, egui::Stroke::new(2.0, Color32::from_gray(0xe8)), egui::StrokeKind::Inside);
            }
            let hint = r.hints.get(&z.file).cloned().unwrap_or_default();
            self.review_hint_line(ui, egui::Rect::from_min_max(egui::pos2(inner.left(), pic.bottom()), inner.max), &hint, r.suggestion.sharp.get(k).copied(), img);
            if z.flag != 0 {
                lr::paint_flag(&p, pic.right_top() + egui::vec2(-12.0, 12.0), 15.0, z.flag, true, Color32::from_gray(0xf0));
            }
            lr::hover_tip(ui, &resp, crate::tr("Klick: in den Vergleich legen, ⌘-Klick: herausnehmen"));
            if resp.clicked() {
                if cmd {
                    if r.compare.len() > 1 {
                        r.compare.retain(|&x| x != k);
                    }
                } else if !r.compare.contains(&k) {
                    if r.compare.len() >= 4 {
                        r.compare.remove(0);
                    }
                    r.compare.push(k);
                    r.compare.sort_unstable();
                }
                r.focus = r.compare.iter().position(|&x| x == k).unwrap_or(0);
            }
        }
    }

    /// Strip of the series: ✓ picked, ✗ rejected, frame = in the comparison, dimmed = out of the duel, ★ = cover.
    fn review_strip(&mut self, ui: &mut egui::Ui, r: &mut Review, area: egui::Rect) {
        let n = r.images.len();
        let h = area.height() - 6.0;
        let w = (h * 1.4).min(area.width() / n.max(1) as f32);
        let cmd = ui.input(|i| i.modifiers.command);
        ui.painter().rect_filled(area, 0.0, Color32::from_gray(0x14));
        let top = self.review_stack_top(r);
        for (k, img) in r.images.clone().into_iter().enumerate() {
            let cell = egui::Rect::from_min_size(area.min + egui::vec2(k as f32 * w + 3.0, 3.0), egui::vec2(w - 4.0, h));
            let Some(&ri) = self.index.get(&img) else { continue };
            let z = self.rows[ri].clone();
            let resp = ui.interact(cell, egui::Id::unique(("sichten-streifen", img)), egui::Sense::click());
            let p = ui.painter_at(cell);
            if let Some(t) = self.texture(z.file, preview::small()) {
                let dim = z.flag == -1 || r.out.contains(&k);
                p.image(t.id(), fit(&t, cell.shrink(2.0)), full_uv(), if dim { Color32::WHITE.gamma_multiply(0.35) } else { Color32::WHITE });
            }
            if z.flag != 0 {
                lr::paint_flag(&p, cell.right_top() + egui::vec2(-9.0, 9.0), 12.0, z.flag, true, Color32::from_gray(0xf0));
            }
            if top == Some(img) {
                p.text(cell.left_top() + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, "★", egui::FontId::proportional(11.0), Color32::from_gray(0xf0));
            }
            if r.compare.contains(&k) {
                p.rect_stroke(cell, 0.0, egui::Stroke::new(1.5, Color32::from_gray(0xd0)), egui::StrokeKind::Inside);
            }
            lr::hover_tip(ui, &resp, &crate::trf!("{k}/{n} – Klick: in den Vergleich, ⌘-Klick: heraus", k = k + 1, n = n));
            if resp.clicked() && r.mode != Mode::Duel {
                if cmd {
                    if r.compare.len() > 1 {
                        r.compare.retain(|&x| x != k);
                        r.focus = r.focus.min(r.compare.len() - 1);
                    }
                } else if !r.compare.contains(&k) {
                    if r.compare.len() < 4 && r.mode == Mode::LightTable {
                        r.compare.push(k);
                    } else {
                        // replaces the focused field
                        let f = r.focus.min(r.compare.len().saturating_sub(1));
                        if let Some(slot) = r.compare.get_mut(f) {
                            *slot = k;
                        }
                    }
                    r.compare.sort_unstable();
                    r.compare.dedup();
                    r.focus = r.compare.iter().position(|&x| x == k).unwrap_or(0);
                }
            }
        }
    }

    fn review_stack_top(&self, r: &Review) -> Option<i64> {
        let id = r.stack?;
        self.series.stacks.iter().find(|s| s.id == id).map(|s| s.top())
    }

    /// Persons across the series: the faces of the photo with the most faces, left to right (found again in the
    /// other photos by the nearest frame centre – burst frames hardly move).
    fn review_persons(&self, r: &Review) -> Vec<(f32, f32)> {
        let mut best: Option<&Vec<Face>> = None;
        for b in &r.images {
            let Some(&i) = self.index.get(b) else { continue };
            if let Some(f) = r.hints.get(&self.rows[i].file).and_then(|h| h.faces.as_ref()) {
                if best.is_none_or(|x| f.len() > x.len()) {
                    best = Some(f);
                }
            }
        }
        let mut v: Vec<(f32, f32)> = best.map(|f| f.iter().map(|f| (f.x + f.w / 2.0, f.y + f.h / 2.0)).collect()).unwrap_or_default();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }

    /// Faces bar: one row per person with its face in every photo – where someone blinks or looks away.
    fn review_faces_bar(&mut self, ui: &mut egui::Ui, r: &Review, area: egui::Rect) {
        let persons = self.review_persons(r);
        let p = ui.painter_at(area);
        let g = lr::caps(&p, crate::tr("Gesichter"), lr::semibold(10.5), Color32::from_gray(0xb0));
        p.galley(area.min + egui::vec2(8.0, 3.0), g, Color32::from_gray(0xb0));
        let size = 58.0;
        let n = r.images.len();
        for (row, &(cx, cy)) in persons.iter().take(3).enumerate() {
            let y = area.top() + 20.0 + row as f32 * (size + 6.0);
            for (k, b) in r.images.iter().enumerate() {
                let Some(&i) = self.index.get(b) else { continue };
                let file = self.rows[i].file;
                let x = area.left() + 8.0 + k as f32 * (size + 4.0);
                if x + size > area.right() {
                    break;
                }
                let cell = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(size, size));
                p.rect_filled(cell, 0.0, Color32::from_gray(0x18));
                let dist = |q: &Face| (q.x + q.w / 2.0 - cx).powi(2) + (q.y + q.h / 2.0 - cy).powi(2);
                let face = r.hints.get(&file).and_then(|h| h.faces.as_ref()).and_then(|f| f.iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))).filter(|f| dist(f) < 0.15 * 0.15).copied();
                let Some(face) = face else { continue };
                let Some(t) = self.texture(file, preview::large()) else { continue };
                // square crop around the face, a little larger than its frame
                let ts = t.size_vec2();
                let side = (face.w * ts.x).max(face.h * ts.y) * 1.3;
                let c = egui::pos2(face.x + face.w / 2.0, face.y + face.h / 2.0);
                let uv = egui::Rect::from_center_size(c, egui::vec2(side / ts.x, side / ts.y));
                p.image(t.id(), cell, uv, Color32::WHITE);
                if face.eyes_closed {
                    p.rect_stroke(cell, 0.0, egui::Stroke::new(1.5, Color32::from_rgb(220, 120, 90)), egui::StrokeKind::Inside);
                }
                let h = ui.interact(cell, egui::Id::unique(("sichten-gesicht", row, k)), egui::Sense::hover());
                let tip = match (face.eyes_closed, face.quality) {
                    (true, _) => crate::trf!("{k}/{n}: Augen zu", k = k + 1, n = n),
                    (false, Some(q)) => crate::trf!("{k}/{n}: Gesichtsqualität {q}", k = k + 1, n = n, q = format!("{q:.2}")),
                    _ => format!("{}/{}", k + 1, n),
                };
                lr::hover_tip(ui, &h, &tip);
            }
        }
    }
}

#[cfg(test)]
mod lower_tests {
    use super::*;

    /// ⇧X "lower" never takes away 4 or 5 stars; 0–3 stars go to 0.
    #[test]
    fn lowering_keeps_high_ratings() {
        let stars = |b: i64| b as u8;
        assert_eq!(split_lowered(vec![0, 1, 2, 3, 4, 5], stars), (vec![0, 1, 2, 3], vec![4, 5]));
    }
}
