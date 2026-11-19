//! Small previews of the photo in every film (and in Sigma's colour modes and the camera looks): the tiles of
//! "Film & Look" and the rows of the darkroom's film shelf share one source and one cache.
//!
//! - **Source:** once per photo a small whole image ([`EDGE`] px long side), derived from the preview the app already
//!   has (Sigma: the 2400 px preview of the exact base or the own chain; other formats: the binned preview stage).
//!   The original is never decoded again for a tile. The small image is kept on the worker side ([`SOURCE`]) as long
//!   as the preview behind it lives.
//! - **Per tile:** the film on these ≈ 37 000 pixels. Grain is left out (invisible at this size; its balance costs
//!   ≈ 100 ms per film), development, push and paper grade are the film's defaults ([`tile_settings`]), so that the
//!   calibrated pipelines are shared by all tiles of a film whatever the open film is set to.
//! - **Order and load:** the visible tiles are computed first (lane "front", "Visible" at the film gate); the
//!   darkroom then fills in the other films quietly (lane "back", "Idle"). One job at a time per kind; a job stops at the next film once its state is
//!   outdated ([`FilmPreviews::valid`]: other photo, other settings).
//! - **Cache:** the textures per photo and settings state; the last [`RECENT`] states stay in memory, so paging back
//!   to a photo shows its tiles at once. After a change of the settings the old tiles stay shown until the new ones
//!   arrive (no flicker, no layout jump).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use develop::Settings;
use eframe::egui;
use egui::TextureHandle;

use crate::jobs::{JobResult, Previews};

/// Long side of the small whole image (pixels): sharp enough for the tiles of "Film & Look" (≈ 95 points wide) at
/// 2× and for the shelf rows (64 × 40 points).
pub const EDGE: usize = 240;
/// Settings states (photo + settings) whose textures stay in memory (≈ 45 tiles of ≤ 150 kB each per state).
pub const RECENT: usize = 6;
/// Wait after the last change of the settings before computing (no work while a slider is dragged).
const REST: f64 = 0.25;
/// A job that never answered does not block the previews for ever.
const STUCK: f64 = 30.0;

/// Texture filter of the tiles: linear with mipmaps, so that the 240 px image stays clean in the small shelf field.
pub fn texture_options() -> egui::TextureOptions {
    egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear))
}

/// Settings of a film tile: the film's defaults for development, push, paper and grade, no grain. The rest (exposure,
/// white balance, film sliders such as halation) stays as set.
pub fn tile_settings(s: &Settings) -> Settings {
    let mut t = s.clone();
    t.film.paper = None;
    t.film.film_dev = 0.0;
    t.film.paper_grade = -1.0;
    t.film.push = 0.0;
    t.film.grain = 0.0;
    t
}

/// Settings state of the previews: photo, everything that changes a tile – not the chosen film, colour mode or look
/// (each tile sets its own), not what [`tile_settings`] overrides – plus the profile generation. The arrival of the
/// exact Sigma base does not count: at this size the tiles of the own chain do not differ visibly, and computing all
/// of them a second time doubled the wait (measured).
pub fn state_key(path: &std::path::Path, s: &Settings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut b = tile_settings(s);
    b.film.stock.clear();
    b.film.active = false;
    b.film.grain_size = 0.0;
    b.film.grain_model.clear();
    b.color_mode = None;
    b.raw.look.clear();
    b.raw.look_name.clear();
    b.raw.look_package.clear();
    b.profile_amount = 1.0;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    serde_json::to_string(&b).unwrap_or_default().hash(&mut h);
    raw::film_profile::generation().hash(&mut h);
    h.finish().max(1)
}

/// Request/receive state of the previews in the app.
pub struct FilmPreviews {
    /// image index and settings state shown now
    index: usize,
    key: u64,
    /// path of the photo (stale tiles are only kept for the same photo)
    path: std::path::PathBuf,
    fresh: HashMap<String, TextureHandle>,
    /// tiles of the previous state of the same photo, shown until the new ones are there
    stale: HashMap<String, TextureHandle>,
    /// failed for this state: not requested again, no placeholder
    failed: HashSet<String>,
    /// jobs on their way (visible tiles, background fill): state key and when sent
    front: Option<(u64, f64)>,
    back: Option<(u64, f64)>,
    /// candidate state and since when it is unchanged
    since: (u64, f64),
    /// the state jobs compute for; a job stops at the next tile once it differs
    valid: Arc<AtomicU64>,
    /// textures of earlier states (newest last)
    recent: VecDeque<(u64, HashMap<String, TextureHandle>)>,
}

impl Default for FilmPreviews {
    fn default() -> Self {
        Self {
            index: usize::MAX,
            key: 0,
            path: Default::default(),
            fresh: HashMap::new(),
            stale: HashMap::new(),
            failed: HashSet::new(),
            front: None,
            back: None,
            since: (0, 0.0),
            valid: Arc::new(AtomicU64::new(0)),
            recent: VecDeque::new(),
        }
    }
}

/// What [`FilmPreviews::plan`] asks to compute now.
#[derive(Debug, PartialEq)]
pub struct Plan {
    /// visible tiles to compute (lane "front")
    pub front: Vec<String>,
    /// other tiles to fill in quietly (lane "back")
    pub back: Vec<String>,
    /// still something on its way or waiting: repaint soon
    pub busy: bool,
}

impl FilmPreviews {
    /// The state jobs compute for (shared with them).
    pub fn valid(&self) -> Arc<AtomicU64> {
        self.valid.clone()
    }

    pub fn key(&self) -> u64 {
        self.key
    }

    /// Tiles shown now: new ones over old ones of the same photo.
    pub fn shown(&self) -> HashMap<String, TextureHandle> {
        let mut out = self.stale.clone();
        out.extend(self.fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
        out
    }

    /// Tiles that failed for the current state (no placeholder for them).
    pub fn failed(&self) -> &HashSet<String> {
        &self.failed
    }

    /// The image list was rebuilt: follow the photo to its new index (`None`: gone).
    pub fn remap(&mut self, map: impl Fn(usize) -> Option<usize>) {
        match map(self.index) {
            Some(n) => self.index = n,
            None => self.forget(),
        }
    }

    /// Film profiles changed (import, editing): everything is computed anew.
    pub fn forget(&mut self) {
        *self = Self { valid: self.valid.clone(), ..Self::default() };
        self.valid.store(0, Ordering::SeqCst);
    }

    /// Switch to the state `key` of photo `index` at `path` once it has rested (or at once for another photo or when
    /// nothing is shown yet), then say which of `visible` (in this order) and, with `fill`, which of `all` to compute.
    pub fn plan(&mut self, index: usize, path: &std::path::Path, key: u64, visible: &[String], fill: &[String], now: f64) -> Plan {
        if self.since.0 != key {
            self.since = (key, now);
        }
        let other_photo = index != self.index || path != self.path;
        if self.key != key && (other_photo || self.key == 0 || now - self.since.1 >= REST || self.fresh.is_empty() && self.stale.is_empty()) {
            self.switch(index, path, key, other_photo);
        }
        for p in [&mut self.front, &mut self.back] {
            if p.is_some_and(|(k, sent)| k != self.key || now - sent > STUCK) {
                *p = None;
            }
        }
        let open = |id: &&String| !self.fresh.contains_key(*id) && !self.failed.contains(*id);
        let mut front: Vec<String> = Vec::new();
        for id in visible.iter().filter(open) {
            if !front.contains(id) {
                front.push(id.clone());
            }
        }
        let mut back: Vec<String> = Vec::new();
        for id in fill.iter().filter(open) {
            if !front.contains(id) && !back.contains(id) {
                back.push(id.clone());
            }
        }
        let settled = self.key == key;
        let busy = self.front.is_some() || self.back.is_some() || !front.is_empty() || !settled;
        let front = if settled && self.front.is_none() { front } else { Vec::new() };
        // the quiet fill only when nothing visible waits
        let back = if settled && self.front.is_none() && front.is_empty() && self.back.is_none() { back } else { Vec::new() };
        if !front.is_empty() {
            self.front = Some((self.key, now));
        }
        if !back.is_empty() {
            self.back = Some((self.key, now));
        }
        Plan { front, back, busy }
    }

    fn switch(&mut self, index: usize, path: &std::path::Path, key: u64, other_photo: bool) {
        let old = std::mem::take(&mut self.fresh);
        if self.key != 0 && !old.is_empty() {
            self.recent.retain(|(k, _)| *k != self.key);
            self.recent.push_back((self.key, old.clone()));
            while self.recent.len() > RECENT {
                self.recent.pop_front();
            }
        }
        if other_photo {
            self.stale.clear();
        } else {
            self.stale.extend(old);
        }
        if let Some(i) = self.recent.iter().position(|(k, _)| *k == key) {
            self.fresh = self.recent.remove(i).map(|(_, m)| m).unwrap_or_default();
        }
        self.index = index;
        self.path = path.to_path_buf();
        self.key = key;
        self.failed.clear();
        self.front = None;
        self.back = None;
        self.valid.store(key, Ordering::SeqCst);
    }

    /// Result of [`crate::jobs::Job::FilmThumbs`]: tiles of the state `key` (others are dropped); `end`: the job is done.
    pub fn receive(&mut self, ctx: &egui::Context, index: usize, key: u64, thumbs: Vec<(String, Option<(usize, usize, Vec<u8>)>)>, end: bool, background: bool) {
        if end {
            let p = if background { &mut self.back } else { &mut self.front };
            if p.is_some_and(|(k, _)| k == key) {
                *p = None;
            }
        }
        if key != self.key || index != self.index {
            return;
        }
        for (id, img) in thumbs {
            match img {
                Some((w, h, rgb)) => {
                    let img = egui::ColorImage::from_rgb([w, h], &rgb);
                    match self.fresh.get_mut(&id) {
                        Some(t) => t.set(img, texture_options()),
                        None => {
                            let t = ctx.load_texture(format!("film-{id}"), img, texture_options());
                            self.fresh.insert(id, t);
                        }
                    }
                }
                None => {
                    self.failed.insert(id);
                }
            }
        }
    }
}

/// The small whole image of a photo on the worker side, kept while the preview behind it is the same.
enum Small {
    /// Sigma: the scaled preview (`None`: the preview itself is already small enough)
    Sigma(Option<Arc<develop::session::Preview>>),
    /// other formats: the scaled preview stage
    Foreign(Arc<rawdev::Stage>),
}

/// The preview a small image was derived from, held weakly: a weak reference keeps the allocation (not the pixels), so
/// its address cannot be handed to a later preview of another photo or state while the slot remembers it. A bare
/// address can be reused by the allocator; a new preview then got the small image of an old one (another photo), or
/// one of the other kind, which ended in a panic that repeated with every request.
type Origin = std::sync::Weak<dyn std::any::Any + Send + Sync>;
type Slot<V> = Mutex<Option<(Origin, V)>>;

/// Identity of the preview and the small image derived from it.
static SOURCE: Slot<Arc<Small>> = Mutex::new(None);
/// The same for the own pipeline's preview while the tiles also use Sigma's exact base (colour modes of a photo set to
/// black and white), so that the two do not push each other out.
static SOURCE_OWN: Slot<Arc<Small>> = Mutex::new(None);

/// The value cached in `slot` for exactly this `origin` (same allocation, still alive), or a new one from `make`.
fn cached<T: std::any::Any + Send + Sync, V: Clone>(slot: &Slot<V>, origin: &Arc<T>, make: impl FnOnce() -> V) -> V {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((o, v)) = slot.as_ref() {
        if o.strong_count() > 0 && std::ptr::addr_eq(o.as_ptr(), Arc::as_ptr(origin)) {
            return v.clone();
        }
    }
    let v = make();
    let any: Arc<dyn std::any::Any + Send + Sync> = origin.clone();
    *slot = Some((Arc::downgrade(&any), v.clone()));
    v
}

fn small_of<T: std::any::Any + Send + Sync>(origin: &Arc<T>, make: impl FnOnce() -> Small) -> Arc<Small> {
    cached(&SOURCE, origin, || Arc::new(make()))
}

/// Compute the tiles `modes` (film IDs, `sigma:<mode>`, `sigma-mono`, `look:<name>`) one after the other and send each at once;
/// stop at the next tile once `valid` != `key`. The last answer has `end` set.
#[allow(clippy::too_many_arguments)]
pub fn compute(index: usize, previews: &Previews, cal: &develop::Calibration, settings: &Settings, key: u64, modes: &[String], valid: &AtomicU64, background: bool, send: &dyn Fn(JobResult)) {
    let t0 = std::time::Instant::now();
    let tiles = tile_settings(settings);
    let live = || valid.load(Ordering::SeqCst) == key;
    let log = crate::env::var_os("GRAINEO_LOG").is_some();
    let out = |id: &str, img: Option<(usize, usize, Vec<u8>)>, q: u8| {
        if log {
            eprintln!("film previews: tile {id} after {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
        }
        let img = img.map(|(w, h, rgb)| crate::jobs::rotate_rgb(w, h, rgb, q));
        send(JobResult::FilmThumbs { index, key, thumbs: vec![(id.to_string(), img)], end: false, background });
    };
    let mut n = 0;
    if let Some(doc) = &previews.foreign {
        let mut base = tiles.clone();
        base.film.active = false;
        base.profile_amount = 1.0;
        let stage = doc.preview_stage(&base);
        let small = small_of(&stage, || Small::Foreign(Arc::new(doc.small(&base, EDGE))));
        let Small::Foreign(st) = &*small else { unreachable!() };
        let mut scene = None;
        for m in modes {
            if !live() {
                break;
            }
            let img = if let Some(look) = m.strip_prefix("look:") {
                let mut t = base.clone();
                t.raw.look = look.to_string();
                let img = doc.render_stage(st, &t);
                Some((img.width, img.height, img.to_rgb8()))
            } else {
                let (lin, geo) = scene.get_or_insert_with(|| doc.film_scene(st, &tiles));
                develop::film::thumbnails(lin, &tiles.film, *geo, Some(&[m.as_str()])).into_iter().next().and_then(|t| t.image.ok()).map(|img| (img.width, img.height, img.to_rgb8()))
            };
            out(m, img, doc.orientation);
            n += 1;
        }
    } else {
        // Sigma's colour modes of a photo set to black and white are shown in colour: they start from a colour preview
        // (the own pipeline while the base on screen is a grey one, see `developed_preview`); everything else from the
        // preview of the settings. The small image is derived once per preview.
        let colour = |mode: &str| {
            let mut s = tiles.clone();
            s.film.active = false;
            s.profile_amount = 1.0;
            s.monochrome = false;
            s.color_mode = if mode.is_empty() { None } else { Some(mode.to_string()) };
            s
        };
        let mono = || {
            let mut s = tiles.clone();
            s.film.active = false;
            s.profile_amount = 1.0;
            s.monochrome = true;
            s
        };
        for m in modes {
            if !live() {
                break;
            }
            let tile = match m.strip_prefix("sigma:") {
                Some(mode) => Some(colour(mode)),
                None if m == crate::film_look::SIGMA_MONO_KEY => Some(mono()),
                None => None,
            };
            let preview = crate::jobs::developed_preview(previews, tile.as_ref().unwrap_or(settings));
            let slot = if previews.spp.is_some() && Arc::ptr_eq(preview, &previews.developed) { &SOURCE_OWN } else { &SOURCE };
            let small = cached(slot, preview, || Arc::new(Small::Sigma(preview.scaled(cal, EDGE).map(Arc::new))));
            let Small::Sigma(small) = &*small else { unreachable!() };
            let p: &develop::session::Preview = small.as_deref().unwrap_or(preview);
            let img = if let Some(s) = &tile {
                // Sigma's colour modes and black and white: the small preview with these settings
                let img = p.render(cal, s);
                Some((img.width, img.height, img.to_rgb8()))
            } else {
                // as the main image with this film: layer exposure and grain balance per film (Sigma-Analog)
                develop::analog::thumbnails(cal, &tiles, &[m.as_str()], |s| p.film_scene(cal, s, EDGE))
                    .into_iter()
                    .next()
                    .and_then(|t| t.image.ok())
                    .map(|img| (img.width, img.height, img.to_rgb8()))
            };
            out(m, img, cal.orientation);
            n += 1;
        }
    }
    if n > 0 && log {
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!("film previews: {n} tiles in {ms:.0} ms ({:.0} ms each){}", ms / n as f64, if background { ", background" } else { "" });
    }
    send(JobResult::FilmThumbs { index, key, thumbs: Vec::new(), end: true, background });
}

/// Rectangle of a tile image fitted whole into `field` (aspect kept, centred, aligned to the pixel grid).
pub fn fit(field: egui::Rect, size: egui::Vec2, ppp: f32) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return field;
    }
    let k = (field.width() / size.x).min(field.height() / size.y);
    let snap = |v: f32| (v * ppp).round() / ppp;
    let (w, h) = (snap(size.x * k), snap(size.y * k));
    let min = egui::pos2(snap(field.center().x - w / 2.0), snap(field.center().y - h / 2.0));
    egui::Rect::from_min_size(min, egui::vec2(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The whole image fits into the field, aspect kept, centred.
    #[test]
    fn fit_keeps_the_whole_image() {
        let field = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(64.0, 40.0));
        // landscape 3:2: full height, 60 wide
        let r = fit(field, egui::vec2(240.0, 160.0), 2.0);
        assert_eq!(r.size(), egui::vec2(60.0, 40.0));
        assert_eq!(r.center(), field.center());
        // portrait 2:3: full height, narrow
        let r = fit(field, egui::vec2(160.0, 240.0), 2.0);
        assert!((r.height() - 40.0).abs() < 0.01 && (r.width() - 26.5).abs() < 0.3, "{r:?}");
        assert!(field.contains_rect(r));
        // panorama: full width
        let r = fit(field, egui::vec2(240.0, 80.0), 2.0);
        assert_eq!(r.width(), 64.0);
        assert!(field.contains_rect(r));
    }

    /// The state ignores the film choice and what the tiles set themselves, but not the photo or its development.
    #[test]
    fn state_key_follows_photo_and_settings_not_the_film() {
        let p = std::path::Path::new("/a/b.X3F");
        let s = Settings::default();
        let k = state_key(p, &s);
        let mut t = s.clone();
        t.film.active = true;
        t.film.stock = "kodak_portra_400".into();
        t.film.paper = Some("kodak_portra_endura".into());
        t.film.push = 1.0;
        t.film.film_dev = 0.5;
        t.film.paper_grade = 3.0;
        t.film.grain = 0.8;
        t.color_mode = Some("Neutral".into());
        assert_eq!(state_key(p, &t), k);
        t.exposure += 0.3;
        assert_ne!(state_key(p, &t), k, "exposure changes the tiles");
        let mut h = s.clone();
        h.film.halation = 0.7;
        assert_ne!(state_key(p, &h), k, "film sliders change the tiles");
        assert_ne!(state_key(std::path::Path::new("/a/c.X3F"), &s), k, "other photo");
    }

    /// Tile settings: the film's defaults, no grain; the rest stays.
    #[test]
    fn tile_settings_use_film_defaults_without_grain() {
        let mut s = Settings::default();
        s.film.push = 2.0;
        s.film.grain = 0.5;
        s.film.paper = Some("x".into());
        s.exposure = 0.7;
        let t = tile_settings(&s);
        assert_eq!((t.film.push, t.film.grain, t.film.paper.as_deref(), t.film.paper_grade, t.film.film_dev), (0.0, 0.0, None, -1.0, 0.0));
        assert_eq!(t.exposure, 0.7);
    }

    /// Visible tiles first, then the quiet fill; nothing twice; one job of each kind at a time.
    #[test]
    fn plan_visible_first_then_fill() {
        let mut f = FilmPreviews::default();
        let p = std::path::Path::new("/a.raf");
        let all = ids(&["a", "b", "c", "d"]);
        let plan = f.plan(0, p, 5, &ids(&["b", "c", "b"]), &all, 0.0);
        assert_eq!(plan.front, ids(&["b", "c"]));
        assert!(plan.back.is_empty(), "no fill while visible tiles wait");
        assert!(plan.busy);
        assert_eq!(f.valid().load(Ordering::SeqCst), 5);
        // the job is on its way: nothing new
        let plan = f.plan(0, p, 5, &ids(&["b", "c"]), &all, 0.1);
        assert!(plan.front.is_empty() && plan.back.is_empty() && plan.busy);
        // the job ended (results arrive in `receive`; here: b failed, c not delivered)
        let ctx = egui::Context::default();
        f.receive(&ctx, 0, 5, vec![("b".into(), None)], true, false);
        let plan = f.plan(0, p, 5, &ids(&["b", "c"]), &all, 0.2);
        assert_eq!(plan.front, ids(&["c"]), "failed tiles are not asked again");
        f.receive(&ctx, 0, 5, vec![("c".into(), Some((2, 1, vec![0; 6])))], true, false);
        let plan = f.plan(0, p, 5, &ids(&["b", "c"]), &all, 0.3);
        assert!(plan.front.is_empty());
        assert_eq!(plan.back, ids(&["a", "d"]));
        assert!(f.failed().contains("b"));
    }

    /// A change of the settings waits for rest, keeps the old tiles shown and stops the running job; paging back
    /// to a state shows its tiles from memory.
    #[test]
    fn change_rests_keeps_old_tiles_and_cancels() {
        let ctx = egui::Context::default();
        let mut f = FilmPreviews::default();
        let p = std::path::Path::new("/a.raf");
        let vis = ids(&["a"]);
        f.plan(0, p, 1, &vis, &[], 0.0);
        let valid = f.valid();
        f.receive(&ctx, 0, 1, vec![("a".into(), Some((2, 1, vec![0; 6])))], true, false);
        assert!(f.shown().contains_key("a"));
        // slider moves: no switch yet, the job of state 1 may still run
        let plan = f.plan(0, p, 2, &vis, &[], 1.0);
        assert!(plan.front.is_empty() && plan.busy);
        assert_eq!(valid.load(Ordering::SeqCst), 1);
        // rested: switch, the old tile stays shown, state 1 is outdated for running jobs
        let plan = f.plan(0, p, 2, &vis, &[], 1.0 + REST);
        assert_eq!(plan.front, vis);
        assert_eq!(valid.load(Ordering::SeqCst), 2);
        assert!(f.shown().contains_key("a"), "old tile shown until the new one is there");
        // a late answer of state 1 is dropped
        f.receive(&ctx, 0, 1, vec![("b".into(), Some((2, 1, vec![0; 6])))], true, false);
        assert!(!f.shown().contains_key("b"));
        // back to state 1 (after rest): from memory, nothing to compute
        f.plan(0, p, 1, &vis, &[], 5.0);
        let plan = f.plan(0, p, 1, &vis, &[], 5.0 + REST);
        assert_eq!(f.key(), 1);
        assert!(plan.front.is_empty() && f.fresh.contains_key("a"), "state 1 comes from memory: {plan:?}");
    }

    /// Another photo switches at once and never shows the tiles of the previous photo.
    #[test]
    fn other_photo_switches_at_once_without_old_tiles() {
        let ctx = egui::Context::default();
        let mut f = FilmPreviews::default();
        f.plan(0, std::path::Path::new("/a.raf"), 1, &ids(&["a"]), &[], 0.0);
        f.receive(&ctx, 0, 1, vec![("a".into(), Some((2, 1, vec![0; 6])))], true, false);
        let plan = f.plan(1, std::path::Path::new("/b.raf"), 9, &ids(&["a"]), &[], 0.01);
        assert_eq!(plan.front, ids(&["a"]));
        assert!(f.shown().is_empty());
        assert_eq!(f.valid().load(Ordering::SeqCst), 9);
    }

    /// The small image belongs to one preview, not to an address: after the preview is gone, a new one (another photo
    /// or state) never gets the old small image, even where the allocator hands out the same memory again.
    #[test]
    fn source_follows_the_preview_not_its_address() {
        static SLOT: Slot<u32> = Mutex::new(None);
        for i in 0..2000u32 {
            let a = Arc::new([i; 8]);
            assert_eq!(cached(&SLOT, &a, || i), i);
            assert_eq!(cached(&SLOT, &a, || u32::MAX), i, "same preview: from the slot");
            drop(a);
            let b = Arc::new([i + 1; 8]);
            assert_eq!(cached(&SLOT, &b, || i + 1_000_000), i + 1_000_000, "a new preview gets its own small image");
        }
        // another kind of preview never gets the value of the first kind
        let a = Arc::new(1u64);
        cached(&SLOT, &a, || 1);
        drop(a);
        let b = Arc::new(2u64 as f64);
        assert_eq!(cached(&SLOT, &b, || 2), 2);
    }

    /// Only the last RECENT states stay in memory.
    #[test]
    fn memory_is_bounded() {
        let ctx = egui::Context::default();
        let mut f = FilmPreviews::default();
        let p = std::path::Path::new("/a.raf");
        for k in 1..=(RECENT as u64 + 3) {
            f.plan(k as usize, p, k, &ids(&["a"]), &[], k as f64);
            f.receive(&ctx, k as usize, k, vec![("a".into(), Some((2, 1, vec![0; 6])))], true, false);
        }
        assert!(f.recent.len() <= RECENT);
    }
}
