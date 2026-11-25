//! Library side of the photo comparison (`crate::photo_compare`): which photos it shows (the selection, a series
//! chosen as a whole), the hints measured on the previews (sharpness, motion blur, clipping, faces and closed eyes
//! with Apple Vision, the camera's focus point), the suggestion "best photo" with its reason, and the decisions on a
//! series: the best photo gets the pick flag and becomes the cover, ⇧X rejects the rest in one command (one ⌘Z), ⌘↩
//! marks the series as reviewed and goes on to the next one. The view itself is drawn by `photo_compare_ui.rs` in
//! the centre of the library and of Develop.
//!
//! Submodule of `library_ui` (accesses its fields).

use super::*;
use crate::photo_compare::{Compare, Hint};
use library::series_quality::{self as sq, Face, Suggestion, Why};

/// ⇧X "lower" sets the rest to 0 stars, but only photos with at most this many stars: 4 and 5 stars stay.
pub(crate) const LOWER_KEEPS_FROM: u8 = 3;

/// ⇧X "lower": (photos set to 0 stars, photos that keep their 4 or 5 stars).
fn split_lowered(rest: Vec<i64>, stars: impl Fn(i64) -> u8) -> (Vec<i64>, Vec<i64>) {
    rest.into_iter().partition(|&b| stars(b) <= LOWER_KEEPS_FROM)
}

/// Order of the series for ↓/↑ and "next": not reviewed first, newest day first (as the source "Serien").
fn series_order(stacks: &[library::series::Stack]) -> Vec<i64> {
    let mut v: Vec<&library::series::Stack> = stacks.iter().filter(|s| s.is_series()).collect();
    v.sort_by_key(|s| (s.reviewed.is_some(), std::cmp::Reverse(s.day.clone()), s.id));
    v.iter().map(|s| s.id).collect()
}

/// A wish to open the comparison (button above the grid, N, context menu, series): the app opens it in the view
/// shown, the library or Develop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompareRequest {
    pub stack: Option<i64>,
    pub reference: Option<i64>,
    /// test aid: mode right after opening ("all", "duel", "zoom")
    pub test_mode: Option<String>,
}

impl LibraryView {
    /// Is the comparison open (keys and ratings go to it)?
    pub fn compare_open(&self) -> bool {
        self.compare.is_some()
    }

    /// The photo a rating key applies to in the comparison.
    pub fn compare_focus(&self) -> Option<i64> {
        self.compare.as_ref().and_then(|c| c.focus)
    }

    /// A wish to open the comparison, taken once by the app.
    pub fn take_compare_request(&mut self) -> Option<CompareRequest> {
        self.compare_request.take()
    }

    /// Ask the app to open the comparison (`stack`: the series as a whole).
    pub(super) fn request_compare(&mut self, stack: Option<i64>, reference: Option<i64>) {
        self.compare_request = Some(CompareRequest { stack, reference, test_mode: None });
    }

    fn capture_of(&self, b: &i64) -> String {
        self.index.get(b).and_then(|&i| self.rows[i].capture.clone()).unwrap_or_default()
    }

    /// Photos of a series for the comparison, in capture order (a camera JPEG folded into its RAW is no photo of its
    /// own: its rating follows the RAW).
    pub fn compare_series_images(&self, stack: i64) -> Option<(Vec<i64>, i64)> {
        let s = self.series.stacks.iter().find(|s| s.id == stack)?;
        let mut v = s.images.clone();
        v.retain(|b| self.index.contains_key(b) && !self.folded_jpeg(*b));
        v.sort_by_key(|b| self.capture_of(b));
        Some((v, s.top()))
    }

    /// Images in capture order.
    pub fn compare_sorted(&self, mut images: Vec<i64>) -> Vec<i64> {
        images.sort_by_key(|b| (self.capture_of(b), *b));
        images.dedup();
        images
    }

    /// Open: select the series (if one), then the comparison follows the selection.
    pub fn compare_start(&mut self, stack: Option<i64>, reference: Option<i64>) {
        let stack = stack.filter(|id| self.series.stacks.iter().any(|s| s.id == *id));
        if let Some((images, top)) = stack.and_then(|id| self.compare_series_images(id)) {
            let active = reference.filter(|r| images.contains(r)).unwrap_or(top);
            self.set_selection(active, &images);
        }
        self.compare = Some(Compare::new(stack, reference));
        self.compare_hints.clear();
        self.loupe = false;
        // the series of the compared photos stand open in the filmstrip (all members, to take others in)
        self.series.held.clear();
        self.series.held_photos.clear();
        let images = match stack.and_then(|id| self.compare_series_images(id)) {
            Some((images, _)) => images,
            None => self.target(),
        };
        self.compare_hold(&images);
        self.refilter = true;
    }

    /// While comparing: the stacks of these (compared) photos are held open in the grid and the filmstrip with all
    /// their members; a photo taken in from another stack opens that one too. The list is filtered again only when a
    /// stack is new.
    pub fn compare_hold(&mut self, images: &[i64]) {
        if self.compare.is_none() {
            return;
        }
        for &b in images {
            if self.series.held_photos.insert(b) {
                self.refilter = true;
            }
            if let Some(k) = self.series.of(b) {
                if self.series.held.insert(self.series.stacks[k].id) {
                    self.refilter = true;
                }
            }
        }
    }

    /// Images the comparison holds visible: all members of the held stacks (filters of marks, search and columns do
    /// not hide them while comparing). Empty when no comparison is open.
    pub(super) fn held_images(&self) -> HashSet<i64> {
        if self.compare.is_none() {
            return HashSet::new();
        }
        let mut v: HashSet<i64> = self.series.held_photos.clone();
        v.extend(self.series.stacks.iter().filter(|s| self.series.held.contains(&s.id)).flat_map(|s| s.images.iter().copied()));
        v
    }

    /// Leave the comparison: the focused photo becomes the active one, the selection stays.
    pub fn compare_close(&mut self) {
        let focus = self.compare_focus();
        self.compare = None;
        // the held stacks go back to their own state (expanded or collapsed as before the comparison)
        if !self.series.held.is_empty() || !self.series.held_photos.is_empty() {
            self.series.held.clear();
            self.series.held_photos.clear();
            self.refilter = true;
        }
        if let Some(f) = focus {
            self.active = Some(f);
            self.to_active = true;
        }
        // the large previews still queued for the comparison are dropped: after closing nothing keeps computing for
        // it (Quattro X3F take many seconds each; QA matrix 20.11.2026: 130 % processor for a minute after closing)
        if !self.compare_requested.is_empty() {
            if let Some(e) = &self.generator {
                e.ahead(Vec::new());
            }
        }
        self.compare_requested.clear();
    }

    /// Make `image` active without touching the selection (a click on a field).
    pub fn set_active_keep(&mut self, image: i64) {
        self.nothing_selected = false;
        self.active = Some(image);
    }

    /// Put a photo into the selection or take it out (⌘-click in the filmstrip while comparing).
    pub fn compare_select(&mut self, image: i64, on: bool) {
        let active = self.active;
        if self.selection.is_empty() {
            if let Some(a) = active {
                self.selection.insert(a);
            }
        }
        if on {
            self.selection.insert(image);
        } else {
            self.selection.remove(&image);
            if active == Some(image) {
                self.active = self.selection.iter().next().copied();
            }
        }
        self.nothing_selected = false;
    }

    /// Row of a photo (name, flag, stars, capture data).
    pub fn compare_row(&self, image: i64) -> Option<Row> {
        self.index.get(&image).map(|&i| self.rows[i].clone())
    }

    /// The images of the Sigma photos of the library whose camera and ISO `pred` picks and whose original is
    /// reachable right now. `None` while the library is still loading.
    pub fn reachable_sigma_images_where(&self, pred: impl Fn(&str, f64) -> bool) -> Option<Vec<i64>> {
        if self.db.is_none() || self.first_loading() || self.reloading() {
            return None;
        }
        Some(self.rows.iter().filter(|z| (z.format == "X3F" || z.format == "X3I") && z.image > 0 && z.status == 0 && pred(z.camera.as_deref().unwrap_or(""), z.iso.unwrap_or(0) as f64)).map(|z| z.image).collect())
    }

    /// Library preview of a photo (the large level; the small one while the large is missing), shown in a field until
    /// its development is there (`photo_compare_ui.rs`).
    pub fn compare_texture(&mut self, image: i64) -> Option<TextureHandle> {
        let file = self.rows.get(*self.index.get(&image)?)?.file;
        self.texture(file, preview::large())
    }

    /// Request the previews the comparison shows (large level for fields and faces), in the queue after the visible
    /// ones (the filmstrip keeps its own).
    pub fn compare_request_previews(&mut self, large: &[i64]) {
        let mut wanted = Vec::new();
        let mut jobs = Vec::new();
        for (list, level) in [(large, preview::large())] {
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
        if wanted != self.compare_requested {
            if let Some(e) = &self.generator {
                e.ahead(jobs);
            }
            self.compare_requested = wanted;
        }
    }

    /// Measure photos of the comparison in the background: stored hints first, missing ones from the previews (never
    /// the originals), faces with Vision (macOS). Results arrive one by one.
    fn compare_measure(&self, images: &[i64]) -> std::sync::mpsc::Receiver<(i64, Hint)> {
        let (tx, rx) = std::sync::mpsc::channel();
        let tasks: Vec<(i64, String, i64, i64, Option<i64>)> = images
            .iter()
            .filter_map(|b| self.index.get(b).map(|&i| &self.rows[i]))
            .map(|z| (z.file, row_path(&self.folder_paths, z), z.size.unwrap_or(0), z.mtime.unwrap_or(0), z.orientation))
            .collect();
        let path = self.db.as_ref().map(|d| d.path.clone());
        let cache = self.generator.as_ref().map(|g| g.cache().clone());
        let ctx = self.quick_ctx.clone();
        let _ = std::thread::Builder::new().name("compare-hints".into()).spawn(move || {
            library::platform::set_priority(library::platform::Priority::Utility);
            let db = path.and_then(|p| Db::open(&p).ok());
            let files: Vec<i64> = tasks.iter().map(|t| t.0).collect();
            let stored = db.as_ref().map(|d| sq::load(&d.conn, &files)).unwrap_or_default();
            for (file, p, size, mtime, orientation) in tasks {
                let signature = db.as_ref().and_then(|d| d.conn.query_row("SELECT signature FROM image_quality WHERE file = ?1", [file], |r| r.get::<_, Option<Vec<u8>>>(0)).ok().flatten());
                // focus area from the file header (a few milliseconds; nothing on an unreachable drive)
                let area = raw::focus::read(std::path::Path::new(&p)).and_then(|info| {
                    let g = info.fields.iter().find(|g| g.achieved).or_else(|| info.fields.iter().find(|g| g.selected))?;
                    Some(g.upright(info.orientation).rect)
                });
                let focus = area.map(|r| [r[0], r[1]]);
                let focus_area = area.filter(|r| r[2] > 0.0 && r[3] > 0.0).map(|r| [r[0] - r[2] / 2.0, r[1] - r[3] / 2.0, r[2], r[3]]);
                let mut hint = Hint { quality: stored.get(&file).copied(), faces: None, signature, focus, focus_area };
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

    /// Hint of a photo of the comparison (by image).
    pub fn compare_hint(&self, image: i64) -> Option<Hint> {
        let file = self.rows.get(*self.index.get(&image)?)?.file;
        self.compare_hints.get(&file).cloned()
    }

    /// Take the measured hints, start measuring photos new in the comparison, compute the suggestion when something
    /// new came. A series that is gone (stacks read again) leaves the comparison as a plain selection.
    pub fn compare_poll(&mut self) {
        let Some(mut c) = self.compare.take() else { return };
        if c.stack.is_some_and(|id| !self.series.stacks.iter().any(|s| s.id == id)) {
            c.stack = None;
        }
        let mut changed = false;
        if let Some(rx) = &c.hints_rx {
            while let Ok((file, h)) = rx.try_recv() {
                c.hints.insert(file, h);
                changed = true;
            }
        }
        // photos without a measurement: one background run for all of them (only when the last one is done)
        let missing: Vec<i64> = c.images.iter().copied().filter(|b| self.index.get(b).is_some_and(|&i| !c.measuring.contains(&self.rows[i].file))).collect();
        let idle = c.hints_rx.is_none() || c.measuring.iter().all(|f| c.hints.contains_key(f));
        if !missing.is_empty() && idle && missing.len() <= 200 {
            c.measuring.extend(missing.iter().filter_map(|b| self.index.get(b).map(|&i| self.rows[i].file)));
            c.hints_rx = Some(self.compare_measure(&missing));
        }
        if changed || c.suggestion_for != c.images {
            let bracket = self.series.stacks.iter().any(|s| Some(s.id) == c.stack && s.kind == library::series::Kind::Bracket);
            let cands: Vec<sq::Candidate> = c
                .images
                .iter()
                .map(|b| {
                    let z = self.index.get(b).map(|&i| &self.rows[i]);
                    sq::Candidate { quality: z.and_then(|z| c.hints.get(&z.file)).and_then(|h| h.quality), stars: z.map_or(0, |z| z.stars), flag: z.map_or(0, |z| z.flag) }
                })
                .collect();
            let s = sq::suggest(&cands);
            c.suggestion = if bracket { Suggestion { sharp: s.sharp, ..Suggestion::default() } } else { s };
            c.suggestion_for = c.images.clone();
        }
        if changed || self.compare_hints.len() != c.hints.len() {
            self.compare_hints = c.hints.clone();
        }
        self.compare = Some(c);
    }

    fn compare_stack(&self) -> Option<&library::series::Stack> {
        let id = self.compare.as_ref()?.stack?;
        self.series.stacks.iter().find(|s| s.id == id)
    }

    /// A series compared as a whole: its kind as a word for the filmstrip header ("Reihe · 8 Bilder"), the strip shows
    /// only its members then ([`Self::compare_series_only`]).
    pub fn compare_series_word(&self) -> Option<&'static str> {
        let s = self.compare_stack()?;
        Some(match s.kind {
            library::series::Kind::Bracket => crate::tr("Reihe"),
            library::series::Kind::Series => crate::tr("Serie"),
            library::series::Kind::Manual => crate::tr("Stapel"),
        })
    }

    /// The photos the list is cut down to while a series is compared as a whole (the
    /// filmstrip shows only its members – a photo of another series makes no sense in it; one leaves the comparison
    /// to go to another series). `None`: no series comparison (a free selection keeps the whole list).
    pub(super) fn compare_series_only(&self) -> Option<HashSet<i64>> {
        self.compare_stack().map(|s| s.images.iter().copied().collect())
    }

    /// Head of the view: what it is, where in the list of series, time span; with the reason the photos belong
    /// together (tooltip) when it is a series.
    pub fn compare_header(&self) -> (String, Option<String>) {
        let Some(c) = &self.compare else { return (String::new(), None) };
        let stack = self.compare_stack();
        let order = series_order(&self.series.stacks);
        let n = c.images.len();
        let t = |b: Option<&i64>| b.map(|b| self.capture_of(b)).and_then(|c| c.get(11..19).map(str::to_string)).unwrap_or_default();
        let kind = match stack {
            Some(s) if s.kind == library::series::Kind::Bracket => crate::tr("Belichtungsreihe"),
            Some(s) if s.is_series() => crate::tr("Serie"),
            Some(_) => crate::tr("Stapel"),
            None => crate::tr("Auswahl"),
        };
        // its place among the series (what "Weiter" walks through), in words: "6/7" next to "7 Fotos" read as a
        // count of photos (QA run 3)
        let place = stack
            .and_then(|s| order.iter().position(|&x| x == s.id))
            .map(|p| format!(" · {}", crate::trf!("Serie {a} von {b}", a = p + 1, b = order.len())))
            .unwrap_or_default();
        let (a, b) = (t(c.images.first()), t(c.images.last()));
        let count = if n == 1 { crate::tr("1 Foto").to_string() } else { crate::trf!("{n} Fotos", n = n) };
        let head = format!("{kind} · {count}{place}");
        // time span, reason and suggestion in the tooltip (the row stays short)
        let mut tip: Vec<String> = Vec::new();
        if !a.is_empty() {
            tip.push(format!("{a}–{b}"));
        }
        if let Some(s) = stack {
            tip.push(library_series::reason_text(s, self.stack_pictures(s)));
        }
        let sug = self.compare_suggestion_text();
        if !sug.is_empty() {
            tip.push(sug);
        }
        (head, (!tip.is_empty()).then(|| tip.join("\n")))
    }

    /// The suggestion as a short line ("Vorschlag: 3/8 – schärfstes Foto").
    pub fn compare_suggestion_text(&self) -> String {
        let Some(c) = &self.compare else { return String::new() };
        let n = c.images.len();
        if self.compare_stack().is_some_and(|s| s.kind == library::series::Kind::Bracket) {
            return crate::tr("Belichtungsreihe: kein Vorschlag (Material zum Zusammenfügen)").to_string();
        }
        let measured = c.images.iter().filter(|b| self.index.get(b).is_some_and(|&i| c.hints.contains_key(&self.rows[i].file))).count();
        if measured < n {
            return crate::trf!("Hinweise werden gemessen ({a}/{n}) …", a = measured, n = n);
        }
        if c.suggestion.close {
            return crate::tr("Kaum Unterschied – selbst entscheiden").to_string();
        }
        let Some(b) = c.suggestion.best else { return String::new() };
        let why: Vec<&str> = c
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

    /// Go to another series (`step` in the order of [`series_order`]; `unreviewed`: the next one not reviewed).
    /// `false`: none left (the comparison stays as it is).
    pub fn compare_go(&mut self, step: isize, unreviewed: bool, actions: &mut Vec<Action>) -> bool {
        let order = series_order(&self.series.stacks);
        let cur = self.compare.as_ref().and_then(|c| c.stack);
        let pos = cur.and_then(|c| order.iter().position(|&x| x == c));
        let next = if unreviewed {
            order.iter().copied().find(|&id| Some(id) != cur && self.series.stacks.iter().any(|s| s.id == id && s.reviewed.is_none()))
        } else if order.is_empty() {
            None
        } else {
            let p = pos.map_or(0, |p| (p as isize + step).clamp(0, order.len() as isize - 1) as usize);
            order.get(p).copied().filter(|&id| Some(id) != cur)
        };
        match next {
            Some(id) => {
                let keep = self.compare.take();
                self.compare_start(Some(id), None);
                // the view keeps its arrangement and aids
                if let (Some(old), Some(c)) = (keep, self.compare.as_mut()) {
                    c.arrange = old.arrange;
                    c.fit = old.fit;
                    c.coupled = old.coupled;
                    c.checks = old.checks;
                    c.faces_bar = old.faces_bar;
                }
                true
            }
            None => {
                actions.push(Action::Status(crate::tr("Keine weitere Serie zu sichten").into()));
                false
            }
        }
    }

    /// Choose a photo as the best one: pick flag (rating way, ⌘Z) and cover of the series (⌘Z).
    pub fn compare_choose(&mut self, best: i64, actions: &mut Vec<Action>) {
        let new = self.rate_images(&[best], None, Some(1), None);
        if !new.is_empty() {
            actions.push(Action::Rated(new));
        }
        self.compare_set_cover(best);
        actions.push(Action::Status(crate::tr("Als bestes Foto gewählt").into()));
    }

    /// Make `best` the cover of the stack `id` as part of the rating step made at `rated` (`None`: a step of its
    /// own): ⌘Z of either takes both back.
    pub(super) fn set_cover_with_rating(&mut self, id: i64, best: i64, rated: Option<u64>) {
        if let Some(k) = self.series.stacks.iter().position(|s| s.id == id) {
            self.series.stacks[k].cover = Some(best);
        }
        let label = crate::tr("Als bestes Foto gewählt").to_string();
        self.in_background(
            move |db| library::series::set_cover(db, id, best),
            move |b, actions, r| match r {
                Ok(change) => {
                    b.manage.push(library_manage::Undo::Stack { label, change });
                    b.manage.with_rating = rated.zip(b.manage.undo_at.last().copied());
                    crate::series_job::changed();
                }
                Err(e) => actions.push(Action::Status(crate::trf!("Stapel nicht geändert: {e}", e = e))),
            },
        );
    }

    fn compare_set_cover(&mut self, best: i64) {
        if let Some((id, top)) = self.compare_stack().map(|s| (s.id, s.top())) {
            if top != best {
                if let Some(k) = self.series.stacks.iter().position(|s| s.id == id) {
                    self.series.stacks[k].cover = Some(best);
                }
                self.series_change(move |db| library::series::set_cover(db, id, best), crate::tr("Als bestes Foto gewählt").to_string());
            }
        }
    }

    /// How the rest is treated after the best one (setting "stackRest"): stars and flag of each photo of `rest`, and
    /// the photos that keep their rating.
    fn rest_values(&self, rest: Vec<i64>) -> (Vec<(i64, Option<u8>, Option<i8>)>, Vec<i64>) {
        match library::settings::value("stackRest").as_str() {
            Some("lower") => {
                // lowering never takes away a high rating: photos with 4 or 5 stars keep them (a decision made
                // before, which ⌘Z after a restart could not bring back)
                let (low, kept) = split_lowered(rest, |b| self.index.get(&b).map_or(0, |&i| self.rows[i].stars));
                (low.into_iter().map(|b| (b, Some(0), None)).collect(), kept)
            }
            Some("none") => (Vec::new(), Vec::new()),
            _ => (rest.into_iter().map(|b| (b, None, Some(-1))).collect(), Vec::new()),
        }
    }

    /// ⇧X: the rest – all photos of the comparison except the picked ones – rejected (or lowered to 0 stars, setting
    /// "stackRest"; photos with more than [`LOWER_KEEPS_FROM`] stars keep their rating).
    pub fn compare_rest(&mut self, actions: &mut Vec<Action>) {
        let Some(c) = &self.compare else { return };
        let images = c.images.clone();
        let chosen: Vec<i64> = images.iter().copied().filter(|b| self.index.get(b).is_some_and(|&i| self.rows[i].flag == 1)).collect();
        if chosen.is_empty() {
            actions.push(Action::Status(crate::tr("Erst das beste Foto wählen (↩ oder „Als bestes“)").into()));
            return;
        }
        let rest: Vec<i64> = images.iter().copied().filter(|b| !chosen.contains(b)).collect();
        if rest.is_empty() {
            return;
        }
        let lower = library::settings::value("stackRest").as_str() == Some("lower");
        let (values, kept) = self.rest_values(rest);
        let new = self.rate_each(&values);
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

    /// Several ratings with values of their own in one ⌘Z step (`None` keeps the value).
    pub fn rate_each(&mut self, values: &[(i64, Option<u8>, Option<i8>)]) -> Vec<crate::rating::Value> {
        let before = self.rating_undo.len();
        let mut all = Vec::new();
        for &(b, s, f) in values {
            all.extend(self.rate_images(&[b], s, f, None));
        }
        // the single steps become one
        if self.rating_undo.len() > before + 1 {
            let merged: Vec<crate::rating::Value> = self.rating_undo.drain(before..).flatten().collect();
            self.rating_at.truncate(before);
            self.rating_undo.push(merged);
            self.rating_at.push(crate::undo_order::tick());
        }
        all
    }

    /// Tournament "Übernehmen": the winner gets the pick flag and becomes the cover, every other entrant is rejected –
    /// places 2 and 3 too, also with only two or three photos (what loses in the tournament is rejected; the setting
    /// "⇧X beim Sichten" does not apply here). The ratings are one ⌘Z step. No stars are given or taken.
    pub fn compare_adopt(&mut self, places: [Option<i64>; 3], entrants: &[i64], actions: &mut Vec<Action>) {
        let Some(winner) = places[0] else {
            actions.push(Action::Status(crate::tr("Turnier ohne Sieger – nichts gesetzt").into()));
            return;
        };
        let values = adopt_values(places, entrants);
        let new = self.rate_each(&values);
        let new_is_empty = new.is_empty();
        // the message names what was really set (a viewed folder without the sidecar switch refuses ratings)
        let status = adopt_status(winner, &new);
        if !new.is_empty() {
            actions.push(Action::Rated(new));
        }
        // the cover belongs to the same step: one ⌘Z takes ratings and cover back together
        let rated = (!new_is_empty).then(|| self.rating_stamp()).flatten();
        if let Some(a) = places[0] {
            // the series (or stack) compared as a whole, else the one the winner belongs to – also when not all of its
            // photos played in the tournament (QA 10, N-A5: the tooltip promises the cover)
            let stack = self.compare_stack().filter(|s| s.images.contains(&a)).or_else(|| self.series.stacks.iter().find(|s| s.images.contains(&a)));
            if let Some((id, top)) = stack.map(|s| (s.id, s.top())) {
                if top != a {
                    self.set_cover_with_rating(id, a, rated);
                }
            }
        }
        actions.push(Action::Status(status));
    }

    /// ⌘↩: the series is reviewed (collapses with the default setting), on to the next one. `false`: none left.
    pub fn compare_finish(&mut self, actions: &mut Vec<Action>) -> bool {
        let Some(id) = self.compare_stack().map(|s| s.id) else { return false };
        if let Some(k) = self.series.stacks.iter().position(|s| s.id == id) {
            self.series.stacks[k].reviewed = Some(library::now());
            if library::settings::value("stackCollapse").as_str() != Some("never") {
                self.series.stacks[k].collapsed = true;
            }
        }
        self.refilter = true;
        self.series_change(move |db| library::series::mark_reviewed(db, id), crate::tr("Serie gesichtet").to_string());
        self.compare_go(1, true, actions)
    }

    /// Persons across the photos: the faces of the photo with the most faces, left to right (found again in the
    /// other photos by the nearest frame centre – burst frames hardly move).
    pub fn compare_persons(&self, images: &[i64]) -> Vec<(f32, f32)> {
        let mut best: Option<&Vec<Face>> = None;
        for b in images {
            let Some(&i) = self.index.get(b) else { continue };
            if let Some(f) = self.compare_hints.get(&self.rows[i].file).and_then(|h| h.faces.as_ref()) {
                if best.is_none_or(|x| f.len() > x.len()) {
                    best = Some(f);
                }
            }
        }
        let mut v: Vec<(f32, f32)> = best.map(|f| f.iter().map(|f| (f.x + f.w / 2.0, f.y + f.h / 2.0)).collect()).unwrap_or_default();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }

    /// Test aid `GRAINEO_SERIES_TEST=review[:all|duel|zoom]`: open the comparison on the largest series.
    pub(super) fn compare_test_open(&mut self, mode: &str) {
        // the largest series; without one the largest exposure bracket
        let largest = |k: library::series::Kind| self.series.stacks.iter().filter(|s| s.kind == k).max_by_key(|s| s.images.len()).map(|s| s.id);
        let id = largest(library::series::Kind::Series).or_else(|| largest(library::series::Kind::Bracket));
        self.compare_request = Some(CompareRequest { stack: id, reference: None, test_mode: Some(mode.to_string()) });
    }
}

/// The ratings "Übernehmen" sets ([`LibraryView::compare_adopt`]): (photo, stars, flag), `None` keeps the value. The
/// winner is picked, every other entrant rejected; without a winner (every duel ended "both out") nothing is set.
fn adopt_values(places: [Option<i64>; 3], entrants: &[i64]) -> Vec<(i64, Option<u8>, Option<i8>)> {
    let Some(winner) = places[0] else { return Vec::new() };
    let mut values = vec![(winner, None, Some(1))];
    values.extend(entrants.iter().copied().filter(|b| *b != winner).map(|b| (b, None, Some(-1))));
    values
}

/// The message after "Übernehmen" from the ratings that were really set (`set`: what [`LibraryView::rate_each`]
/// returned): nothing set, only the winner picked, or the winner picked and n photos rejected.
fn adopt_status(winner: i64, set: &[crate::rating::Value]) -> String {
    let picked = set.iter().any(|v| v.0 == winner && v.2 == 1);
    let rejected = set.iter().filter(|v| v.0 != winner && v.2 == -1).count();
    match (picked, rejected) {
        (false, 0) => crate::tr("Turnier nicht übernommen: hier lassen sich keine Bewertungen setzen").to_string(),
        (true, 0) => crate::tr("Turnier übernommen: Sieger als Auswahl – ⌘Z nimmt es zurück").to_string(),
        _ => crate::trf!("Turnier übernommen: {a} – ⌘Z nimmt es zurück", a = crate::dialog::count(rejected, "Foto abgelehnt", "Fotos abgelehnt")),
    }
}

#[cfg(test)]
#[path = "library_compare_adopt_test.rs"]
mod adopt_tests;

#[cfg(test)]
mod lower_tests {
    use super::*;

    /// ⇧X "lower" never takes away 4 or 5 stars; 0–3 stars go to 0.
    #[test]
    fn lowering_keeps_high_ratings() {
        let stars = |b: i64| b as u8;
        assert_eq!(split_lowered(vec![0, 1, 2, 3, 4, 5], stars), (vec![0, 1, 2, 3], vec![4, 5]));
    }

    /// Tournament "Übernehmen" (decision of the user): the winner is picked, all others are rejected – places 2 and 3
    /// too – and nobody gets or loses stars.
    #[test]
    fn adopting_the_ranking_picks_the_winner_and_rejects_all_others() {
        let values = adopt_values([Some(1), Some(2), Some(3)], &[1, 2, 3, 4, 5]);
        assert_eq!(values.first(), Some(&(1, None, Some(1))), "place 1: pick flag, its stars stay");
        for b in 2..=5 {
            assert!(values.contains(&(b, None, Some(-1))), "photo {b} rejected: {values:?}");
        }
        assert_eq!(values.len(), 5);
        assert!(values.iter().all(|v| v.1.is_none()), "no stars given: {values:?}");
    }

    /// Also with two or three photos the losers are rejected (before, places 2 and 3 stayed untouched).
    #[test]
    fn adopting_rejects_the_loser_of_two_and_three() {
        assert_eq!(adopt_values([Some(7), Some(8), None], &[7, 8]), vec![(7, None, Some(1)), (8, None, Some(-1))]);
        assert_eq!(adopt_values([Some(2), Some(3), Some(1)], &[1, 2, 3]), vec![(2, None, Some(1)), (1, None, Some(-1)), (3, None, Some(-1))]);
    }

    /// The message names what was set, never "0 Fotos abgelehnt".
    #[test]
    fn the_status_names_what_was_really_set() {
        let de = |s: String| s;
        assert!(de(adopt_status(1, &[])).contains("nicht übernommen"));
        let only_winner = de(adopt_status(1, &[(1, 0, 1, 0)]));
        assert!(only_winner.contains("Sieger") && !only_winner.contains('0'), "{only_winner}");
        let two = de(adopt_status(1, &[(1, 0, 1, 0), (2, 3, -1, 0), (3, 0, -1, 0)]));
        assert!(two.contains('2'), "{two}");
    }

    /// Without a winner nothing is set.
    #[test]
    fn adopting_without_a_winner_sets_nothing() {
        assert!(adopt_values([None, None, None], &[1, 2]).is_empty());
    }
}

impl LibraryView {
    /// View switch "Raster | Vergleichen" at the right end of the first row above the grid and of the comparison (the
    /// same place in both). "Vergleichen" is greyed with fewer than 2 selected photos, the tooltip says why. Returns
    /// the view clicked (`true`: compare).
    pub fn view_switch(&mut self, ui: &mut egui::Ui, comparing: bool) -> Option<bool> {
        let n = self.target().len();
        // one photo of a series active (its tile, collapsed or not): the button compares the series, as N does
        let series = self.button_series().is_some();
        let can = comparing || n >= 2 || series;
        let compare_tip = if n < 2 && series {
            lr::with_shortcut("Die ganze Serie vergleichen", "stack_review")
        } else if can {
            lr::with_shortcut("Ausgewählte Fotos vergleichen", "stack_review")
        } else {
            crate::tr("Mindestens 2 Fotos auswählen (⌘-Klick)").to_string()
        };
        let grid_tip = lr::with_shortcut("Zurück zum Raster", if comparing { "cmp_back" } else { "" });
        let dim = if can { lr::TEXT() } else { crate::lr::gray(0x60) };
        let cells = [(lr::Cell::Text(crate::tr("Raster"), 12.0, lr::TEXT()), !comparing, grid_tip.as_str()), (lr::Cell::Text(crate::tr("Vergleichen"), 12.0, dim), comparing, compare_tip.as_str())];
        match lr::cell_group_w(ui, "bib-ansicht", &cells, &super::library_filter_bar::VIEW_CELLS) {
            Some(0) if comparing => Some(false),
            Some(1) if !comparing && can => Some(true),
            _ => None,
        }
    }
}

impl LibraryView {
    /// The series the button "Vergleichen" compares when fewer than two photos are selected: the one of the active
    /// photo (also behind a collapsed tile), if it has at least two pictures in the library (stack id).
    pub fn button_series(&self) -> Option<i64> {
        if self.target().len() >= 2 {
            return None;
        }
        let k = self.active.and_then(|a| self.series.of(a))?;
        let s = &self.series.stacks[k];
        (self.stack_pictures(s) >= 2).then_some(s.id)
    }

    /// Sharpness in a rectangle of a photo (`[x0, y0, x1, y1]`, 0..1 of the upright photo), on its preview (1024 px):
    /// mean squared Laplacian of the brightness. Only to compare photos of the same scene with each other.
    pub fn compare_sharpness(&self, image: i64, rect: [f32; 4]) -> Option<f32> {
        let z = &self.rows[*self.index.get(&image)?];
        let cache = self.generator.as_ref()?.cache().clone();
        let img = crate::series_job::preview_for(&cache, z.file, &row_path(&self.folder_paths, z), z.size.unwrap_or(0), z.mtime.unwrap_or(0), z.orientation)?;
        Some(laplacian_energy(&img, rect))
    }
}

/// Mean squared Laplacian of the brightness in `rect` (0..1).
fn laplacian_energy(img: &image::RgbImage, rect: [f32; 4]) -> f32 {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let x0 = ((rect[0] * w as f32) as i64).clamp(1, w - 2);
    let x1 = ((rect[2] * w as f32) as i64).clamp(x0 + 1, w - 1);
    let y0 = ((rect[1] * h as f32) as i64).clamp(1, h - 2);
    let y1 = ((rect[3] * h as f32) as i64).clamp(y0 + 1, h - 1);
    let luma = |x: i64, y: i64| {
        let p = img.get_pixel(x as u32, y as u32);
        0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
    };
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let l = 4.0 * luma(x, y) - luma(x - 1, y) - luma(x + 1, y) - luma(x, y - 1) - luma(x, y + 1);
            sum += (l * l) as f64;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { (sum / n as f64) as f32 }
}

#[cfg(test)]
mod sharpness_tests {
    #[test]
    fn edges_are_sharper_than_flat() {
        let flat = image::RgbImage::from_pixel(64, 64, image::Rgb([128, 128, 128]));
        let edges = image::RgbImage::from_fn(64, 64, |x, _| if x % 2 == 0 { image::Rgb([0, 0, 0]) } else { image::Rgb([255, 255, 255]) });
        assert_eq!(super::laplacian_energy(&flat, [0.0, 0.0, 1.0, 1.0]), 0.0);
        assert!(super::laplacian_energy(&edges, [0.0, 0.0, 1.0, 1.0]) > 1000.0);
    }
}

/// Where a background thread reads the preview of a photo (for the comparison's inspection aids).
#[derive(Clone)]
pub struct PreviewSource {
    pub cache: std::sync::Arc<preview::Cache>,
    pub file: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub orientation: Option<i64>,
}

impl PreviewSource {
    /// The developed preview (1024 px), upright and cropped like the photo in the grid.
    pub fn read(&self) -> Option<image::RgbImage> {
        crate::series_job::preview_for(&self.cache, self.file, &self.path, self.size, self.mtime, self.orientation)
    }
}

impl LibraryView {
    pub fn compare_preview_source(&self, image: i64) -> Option<PreviewSource> {
        let z = &self.rows[*self.index.get(&image)?];
        let cache = self.generator.as_ref()?.cache().clone();
        Some(PreviewSource { cache, file: z.file, path: row_path(&self.folder_paths, z), size: z.size.unwrap_or(0), mtime: z.mtime.unwrap_or(0), orientation: z.orientation })
    }
}
