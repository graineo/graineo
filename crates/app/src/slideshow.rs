//! Slideshow: the photos of the selection (from two on) or of the view, full screen on the window's display.
//!
//! - Only the library's developed previews are shown (never a new render): the next and the previous slide are
//!   requested ahead, a slide's time only starts once its photo is there, so nothing waits or stutters.
//! - Calm Ken Burns: at most 7 % zoom over the whole time, aimed at the faces of the photo (face detection of the
//!   library), else at the brightest region of the preview, else the middle. The view never leaves the photo.
//!   Off with the system's "reduce motion".
//! - Two portraits in a row stand side by side on a landscape screen (pairs).
//! - Background: a strongly blurred, darkened version of the photo (a 24 px copy stretched) or black.
//! - Rating with the usual keys (0–5, P/X/U, 6–9) with a small fading mark; in a pair on the photo under the pointer.
//! - Moving the mouse shows a small bar (previous, pause, next, options, end); the pointer hides after 2 s.
//!
//! Test hooks: `GRAINEO_SLIDESHOW=<s>` starts the slideshow s seconds after the photos of the view are there;
//! `GRAINEO_SLIDESHOW_SHARP=0` shows the previews only (comparison).
//! `GRAINEO_SLIDESHOW_TEST="info@2;next@4;rate=3@5;bar@6;options@7;black@9;pause@10"` (seconds after the start).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use eframe::egui;
use egui::{Color32, Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::{lr, App};

/// Key of the options in `settings.json`.
const SETTINGS_KEY: &str = "slideshow";
/// Length of the dissolve in seconds.
const DISSOLVE: f64 = 0.9;
/// Largest zoom of the Ken Burns motion.
const KB_ZOOM: f32 = 1.07;
/// Hide the pointer and the bar after this idle time (seconds).
const IDLE_HIDE: f64 = 2.0;
/// The rating mark stays this long (seconds).
const TOAST: f64 = 1.4;
/// Sharp versions kept (current and next slide, also pairs, and the one before for paging back).
const SHARP_KEEP: usize = 6;
/// The sharp version fades in over the preview in this time (seconds).
const SHARP_FADE: f64 = 0.5;
/// A slide waits at most this long for its sharp version before its time runs anyway (seconds).
const SHARP_WAIT: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    Dissolve,
    Cut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backdrop {
    /// blurred, darkened colours of the photo
    Photo,
    Black,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub seconds: f32,
    pub transition: Transition,
    pub ken_burns: bool,
    pub shuffle: bool,
    pub pairs: bool,
    pub backdrop: Backdrop,
    pub info: bool,
    /// fill the screen (or each half of a pair) instead of fitting the whole photo
    pub fill: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { seconds: 5.0, transition: Transition::Dissolve, ken_burns: true, shuffle: false, pairs: true, backdrop: Backdrop::Photo, info: false, fill: false }
    }
}

impl Options {
    fn load() -> Options {
        let v = library::settings::value(SETTINGS_KEY);
        if v.is_null() {
            return Options::default();
        }
        serde_json::from_value(v).unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(v) = serde_json::to_value(self) {
            library::settings::set(SETTINGS_KEY, v);
        }
    }
}

/// Asked for by a button or menu of the library; started by the app in the next frame.
static REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request() {
    REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

// ───────────────────────────────────────────── Pure logic (tested)

/// One slide: one photo or two portraits side by side (indices into the show's photo list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slide {
    One(usize),
    Pair(usize, usize),
}

impl Slide {
    pub fn photos(self) -> Vec<usize> {
        match self {
            Slide::One(a) => vec![a],
            Slide::Pair(a, b) => vec![a, b],
        }
    }
}

/// Order 0..n, shuffled when `shuffle` (deterministic for a seed; xorshift + Fisher–Yates). `first` stays in front.
pub fn order(n: usize, shuffle: bool, seed: u64, first: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    if !shuffle || n < 2 {
        if first < n {
            v.rotate_left(first);
        }
        return v;
    }
    let mut s = seed | 1;
    for i in (1..n).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let j = (s % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    if let Some(p) = v.iter().position(|&x| x == first) {
        v.swap(0, p);
    }
    v
}

/// A portrait: clearly taller than wide.
pub fn is_portrait(aspect: f32) -> bool {
    aspect > 0.0 && aspect < 0.9
}

/// Slides from the order: with `pairs` on a landscape screen (aspect ≥ 1.2) two portraits in a row share a slide,
/// if both fit side by side at least as large as alone. A single portrait between landscapes stays alone.
pub fn slides(order: &[usize], aspect: impl Fn(usize) -> f32, pairs: bool, screen_aspect: f32) -> Vec<Slide> {
    let mut out = Vec::new();
    let pairing = pairs && screen_aspect >= 1.2;
    let mut i = 0;
    while i < order.len() {
        let a = order[i];
        if pairing && i + 1 < order.len() {
            let b = order[i + 1];
            let (x, y) = (aspect(a), aspect(b));
            if is_portrait(x) && is_portrait(y) && x + y <= screen_aspect * 1.05 {
                out.push(Slide::Pair(a, b));
                i += 2;
                continue;
            }
        }
        out.push(Slide::One(a));
        i += 1;
    }
    out
}

/// Slides to load ahead of slide `cur`: the current one, the next, the previous, the one after next.
pub fn ahead(n: usize, cur: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let mut v = Vec::new();
    for d in [0isize, 1, -1, 2] {
        let s = (cur as isize + d).rem_euclid(n as isize) as usize;
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v
}

/// Ken Burns motion of one slide: zoom and centre (normalised in the photo) at the start and at the end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub zoom: [f32; 2],
    pub from: [f32; 2],
    pub to: [f32; 2],
}

/// Calm motion towards `target` (or the middle): alternately in (from the whole photo to the target) and out
/// (from the target to the whole photo), so two slides in a row do not feel the same.
pub fn motion(target: Option<[f32; 2]>, slide_number: usize) -> Motion {
    let t = target.unwrap_or([0.5, 0.5]);
    if slide_number % 2 == 0 {
        Motion { zoom: [1.0, KB_ZOOM], from: [0.5, 0.5], to: t }
    } else {
        Motion { zoom: [KB_ZOOM, 1.0], from: t, to: [0.5, 0.5] }
    }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Part of the photo shown at time `t` (0…1) as texture coordinates; always inside the photo.
pub fn uv_at(m: &Motion, t: f32) -> Rect {
    let s = smooth(t);
    let z = egui::lerp(m.zoom[0]..=m.zoom[1], s).max(1.0);
    let half = 0.5 / z;
    let c = [egui::lerp(m.from[0]..=m.to[0], s), egui::lerp(m.from[1]..=m.to[1], s)].map(|v| v.clamp(half, 1.0 - half));
    Rect::from_min_max(Pos2::new(c[0] - half, c[1] - half), Pos2::new(c[0] + half, c[1] + half))
}

/// Target of the faces (normalised frames x, y, w, h): their centre, weighted by area (the larger faces count more).
pub fn face_target(faces: &[[f32; 4]]) -> Option<[f32; 2]> {
    let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
    for f in faces {
        let w = (f[2] * f[3]).max(0.0);
        sx += (f[0] + f[2] / 2.0) * w;
        sy += (f[1] + f[3] / 2.0) * w;
        sw += w;
    }
    (sw > 1e-6).then(|| [(sx / sw).clamp(0.0, 1.0), (sy / sw).clamp(0.0, 1.0)])
}

/// Centre of the brightest 3 % of a small image (luma); `None` for a flat image.
pub fn brightest(img: &image::RgbImage) -> Option<[f32; 2]> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let luma: Vec<f32> = img.pixels().map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).collect();
    let mut sorted = luma.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let (lo, hi) = (sorted[sorted.len() / 2], sorted[sorted.len() * 97 / 100]);
    if hi - lo < 24.0 {
        return None;
    }
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for (i, &l) in luma.iter().enumerate() {
        if l >= hi {
            sx += (i as u32 % w) as f32 + 0.5;
            sy += (i as u32 / w) as f32 + 0.5;
            n += 1.0;
        }
    }
    (n > 0.0).then(|| [sx / n / w as f32, sy / n / h as f32])
}

/// Background from a photo: about 24 px wide, blurred twice, a little desaturated and darkened.
pub fn ambient(img: &image::RgbImage) -> image::RgbImage {
    let (w, h) = img.dimensions();
    let tw = 24u32;
    let th = ((h as f32 / w.max(1) as f32) * tw as f32).round().clamp(4.0, 48.0) as u32;
    let mut small = image::imageops::resize(img, tw, th, image::imageops::FilterType::Triangle);
    for _ in 0..2 {
        small = image::imageops::blur(&small, 1.6);
    }
    for p in small.pixels_mut() {
        let l = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
        for c in 0..3 {
            p[c] = ((l + (p[c] as f32 - l) * 0.8) * 0.55).round().clamp(0.0, 255.0) as u8;
        }
    }
    small
}

/// Rectangles of the photos of a slide on `screen` (aspects width/height), with `margin` around and a gap between pairs.
pub fn layout(screen: Rect, aspects: &[f32], margin: f32) -> Vec<Rect> {
    let inner = screen.shrink(margin);
    match aspects {
        [a] => vec![fit(inner, *a)],
        [a, b] => {
            let gap = (inner.width() * 0.012).max(8.0);
            // both the same height, as large as fits
            let h = inner.height().min((inner.width() - gap) / (a + b).max(0.01));
            let (wa, wb) = (h * a, h * b);
            let x0 = inner.center().x - (wa + wb + gap) / 2.0;
            let y0 = inner.center().y - h / 2.0;
            vec![Rect::from_min_size(Pos2::new(x0, y0), Vec2::new(wa, h)), Rect::from_min_size(Pos2::new(x0 + wa + gap, y0), Vec2::new(wb, h))]
        }
        _ => Vec::new(),
    }
}

fn fit(r: Rect, aspect: f32) -> Rect {
    let aspect = if aspect > 0.0 { aspect } else { 1.5 };
    let (w, h) = if r.width() / r.height() > aspect { (r.height() * aspect, r.height()) } else { (r.width(), r.width() / aspect) };
    Rect::from_center_size(r.center(), Vec2::new(w, h))
}

/// Texture coordinates that fill `screen` with an image of `aspect` (cover).
fn cover_uv(screen: Rect, aspect: f32) -> Rect {
    let s = screen.width() / screen.height();
    if aspect > s {
        let w = s / aspect;
        Rect::from_min_max(Pos2::new(0.5 - w / 2.0, 0.0), Pos2::new(0.5 + w / 2.0, 1.0))
    } else {
        let h = aspect / s;
        Rect::from_min_max(Pos2::new(0.0, 0.5 - h / 2.0), Pos2::new(1.0, 0.5 + h / 2.0))
    }
}

// ───────────────────────────────────────────── State

#[derive(Clone)]
struct Photo {
    image: i64,
    file: i64,
    /// entry index (for the single view at the end)
    pos: usize,
    aspect: f32,
    name: String,
    rotate: u8,
}

/// Background data of a photo, computed once from its small preview (off the UI thread).
struct Extra {
    ambient: Option<egui::TextureHandle>,
    bright: Option<[f32; 2]>,
}

struct Computed {
    file: i64,
    ambient: Option<image::RgbImage>,
    bright: Option<[f32; 2]>,
}

/// A photo developed in the screen's size (all settings, the same way as the copy for the clipboard).
struct Sharp {
    tex: egui::TextureHandle,
    level: u32,
    fingerprint: u64,
    /// arrival (fade-in over the preview)
    at: f64,
}

struct SharpDone {
    image: i64,
    level: u32,
    fingerprint: u64,
    result: Result<image::RgbImage, String>,
    ms: u128,
    name: String,
}

/// Long edge in physical pixels a photo shown in `rect` (points) needs at `ppp`: with the Ken Burns zoom when it
/// moves, never more than the screen's long edge.
pub fn sharp_level(rect: Rect, ppp: f32, ken_burns: bool, screen: Rect) -> u32 {
    sharp_level_crop(rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), ppp, ken_burns, screen)
}

/// [`sharp_level`] for a photo of which only `crop` (texture coordinates) fills `rect`: the whole photo is larger
/// by the part cut away (filling the screen), at most by that much beyond the screen.
pub fn sharp_level_crop(rect: Rect, crop: Rect, ppp: f32, ken_burns: bool, screen: Rect) -> u32 {
    let zoom = if ken_burns { KB_ZOOM } else { 1.0 };
    let (cw, ch) = (crop.width().max(0.01), crop.height().max(0.01));
    // the whole photo on screen would be this large (points)
    let (pw, ph) = (rect.width() / cw, rect.height() / ch);
    let need = pw.max(ph) * ppp * zoom;
    let cap = screen.width().max(screen.height()) * ppp / cw.min(ch);
    need.min(cap).ceil().max(64.0) as u32
}

/// Most of a photo that filling may cut away; beyond it the photo is fitted.
pub const FILL_MAX_CUT: f32 = 0.30;

/// Part of a photo (texture coordinates) that fills a cell of `cell_aspect`: as large as possible, placed so the
/// target lies as central as it can and the face frame (`faces`, union of all faces) stays whole where it fits.
/// `None` when more than [`FILL_MAX_CUT`] of the photo would be cut away.
pub fn fill_uv(photo_aspect: f32, cell_aspect: f32, target: Option<[f32; 2]>, faces: Option<[f32; 4]>) -> Option<Rect> {
    if photo_aspect <= 0.0 || cell_aspect <= 0.0 {
        return None;
    }
    // fraction of the photo kept in each direction
    let (w, h) = if photo_aspect > cell_aspect { (cell_aspect / photo_aspect, 1.0) } else { (1.0, photo_aspect / cell_aspect) };
    if w * h < 1.0 - FILL_MAX_CUT - 1e-4 {
        return None;
    }
    let t = target.unwrap_or([0.5, 0.5]);
    let place = |len: f32, centre: f32, lo: Option<f32>, hi: Option<f32>| -> f32 {
        let mut start = (centre - len / 2.0).clamp(0.0, 1.0 - len);
        // keep the faces whole: their far edge inside, then their near edge (heads first)
        if let (Some(lo), Some(hi)) = (lo, hi) {
            if hi - lo <= len {
                if hi > start + len {
                    start = hi - len;
                }
                if lo < start {
                    start = lo;
                }
            } else {
                start = lo;
            }
        }
        start.clamp(0.0, 1.0 - len)
    };
    let (fx0, fx1, fy0, fy1) = match faces {
        Some(f) => (Some(f[0]), Some(f[0] + f[2]), Some(f[1]), Some(f[1] + f[3])),
        None => (None, None, None, None),
    };
    let x = place(w, t[0], fx0, fx1);
    let y = place(h, t[1], fy0, fy1);
    Some(Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)))
}

/// Union of face frames (x, y, w, h).
pub fn face_union(faces: &[[f32; 4]]) -> Option<[f32; 4]> {
    let mut it = faces.iter();
    let f = it.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (f[0], f[1], f[0] + f[2], f[1] + f[3]);
    for f in it {
        x0 = x0.min(f[0]);
        y0 = y0.min(f[1]);
        x1 = x1.max(f[0] + f[2]);
        y1 = y1.max(f[1] + f[3]);
    }
    Some([x0, y0, x1 - x0, y1 - y0])
}

/// Where the photos of a slide go: screen rectangle and the part of the photo shown there (texture coordinates).
/// Fit: the whole photo in [`layout`]. Fill: each photo fills the screen or its half of a pair (with the gap),
/// cropped by [`fill_uv`]; a photo that would lose too much is fitted into its cell instead.
pub fn place(screen: Rect, aspects: &[f32], fill: bool, margin: f32, aims: &[(Option<[f32; 2]>, Option<[f32; 4]>)]) -> Vec<(Rect, Rect)> {
    let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    if !fill {
        return layout(screen, aspects, margin).into_iter().map(|r| (r, full)).collect();
    }
    let cells: Vec<Rect> = match aspects.len() {
        1 => vec![screen],
        2 => {
            let gap = (screen.width() * 0.006).max(4.0);
            let w = (screen.width() - gap) / 2.0;
            vec![Rect::from_min_size(screen.min, Vec2::new(w, screen.height())), Rect::from_min_size(Pos2::new(screen.min.x + w + gap, screen.min.y), Vec2::new(w, screen.height()))]
        }
        _ => return Vec::new(),
    };
    cells
        .into_iter()
        .zip(aspects)
        .enumerate()
        .map(|(i, (cell, &a))| {
            let (target, faces) = aims.get(i).copied().unwrap_or((None, None));
            match fill_uv(a, cell.width() / cell.height().max(1.0), target, faces) {
                Some(uv) => (cell, uv),
                None => (fit(cell.shrink(margin), a), full),
            }
        })
        .collect()
}

/// `inner` (texture coordinates relative to `crop`) mapped into the photo.
pub fn within(crop: Rect, inner: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(crop.min.x + inner.min.x * crop.width(), crop.min.y + inner.min.y * crop.height()),
        Pos2::new(crop.min.x + inner.max.x * crop.width(), crop.min.y + inner.max.y * crop.height()),
    )
}

/// Least recently used order: `image` to the front; returns the images that fall out beyond `keep`.
pub fn lru_touch(order: &mut Vec<i64>, image: i64, keep: usize) -> Vec<i64> {
    order.retain(|&b| b != image);
    order.insert(0, image);
    if order.len() > keep {
        order.split_off(keep)
    } else {
        Vec::new()
    }
}

#[derive(Default)]
pub(crate) struct Slideshow {
    pub open: bool,
    opts: Options,
    photos: Vec<Photo>,
    slides: Vec<Slide>,
    order_seed: u64,
    start_photo: usize,
    screen_aspect: f32,
    /// slide shown and since when (its time runs from there; `None` until its photo is there)
    cur: usize,
    shown_at: Option<f64>,
    /// slide wanted next (timer or arrows): changes over once its photo is there
    want: Option<usize>,
    /// the slide before and when the change started (dissolve)
    prev: Option<(usize, f64, f64)>,
    paused: bool,
    /// time spent paused for the current slide (its time stands still)
    pause_at: Option<f64>,
    last_move: f64,
    last_pointer: Option<Pos2>,
    toast: Option<(String, f64)>,
    options_open: bool,
    was_fullscreen: bool,
    targets: HashMap<i64, Option<[f32; 2]>>,
    /// union of the faces of a photo (filling never cuts them when they fit)
    face_boxes: HashMap<i64, Option<[f32; 4]>>,
    extras: HashMap<i64, Extra>,
    asked: HashSet<i64>,
    rx: Option<mpsc::Receiver<Computed>>,
    tx: Option<mpsc::Sender<Computed>>,
    /// photo rectangles of the current slide on screen (rating in a pair: the one under the pointer)
    rects: Vec<(i64, Rect)>,
    test: Vec<(String, f64)>,
    test_start: Option<f64>,
    /// the slide's time runs from here: once its sharp versions are there (or failed, or took too long)
    timer_at: Option<f64>,
    /// the change to `want` was asked for by a key or the bar (changes as soon as the preview is there)
    want_manual: bool,
    sharp: HashMap<i64, Sharp>,
    sharp_order: Vec<i64>,
    sharp_failed: HashSet<i64>,
    /// one photo is developed at a time: (image, level)
    sharp_busy: Option<(i64, u32)>,
    sharp_rx: Option<mpsc::Receiver<SharpDone>>,
    sharp_tx: Option<mpsc::Sender<SharpDone>>,
    /// level each photo needs (from its rectangle on the screen, last frame)
    sharp_need: HashMap<i64, u32>,
    opened_at: f64,
    /// screen size of the last frame: the sizes needed are measured again when it changes
    last_screen: Vec2,
}

impl App {
    /// Start the slideshow: the selection from two photos on, else all photos of the view from the active one.
    pub(crate) fn open_slideshow(&mut self, ctx: &egui::Context) {
        self.sync_source(false);
        if self.entries.is_empty() {
            self.status = crate::tr("Keine Fotos für die Diashow").to_string();
            return;
        }
        let chosen = self.selection();
        let active_image = if self.in_grid() { self.lib.active_image() } else { self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image) };
        let indices: Vec<usize> = if chosen.len() >= 2 { chosen } else { (0..self.entries.len()).collect() };
        let start = active_image.and_then(|a| indices.iter().position(|&i| self.entries[i].image == a)).unwrap_or(0);
        let mut photos = Vec::with_capacity(indices.len());
        for &i in &indices {
            let e = &self.entries[i];
            // the shape of the thumbnail when it is there (the library's dimensions are only a first guess)
            let aspect = self.lib.thumbnail(e.file).map(|t| t.size_vec2()).filter(|s| s.y > 0.0).map(|s| s.x / s.y).unwrap_or(e.aspect);
            photos.push(Photo { image: e.image, file: e.file, pos: i, aspect, name: e.name.clone(), rotate: if e.loaded { e.settings.rotate % 4 } else { 0 } });
        }
        let s = &mut self.slideshow;
        s.opts = Options::load();
        s.photos = photos;
        s.start_photo = start;
        s.order_seed = (ctx.input(|i| i.time) * 1000.0) as u64 ^ 0x9e37_79b9_7f4a_7c15;
        let r = ctx.content_rect();
        s.screen_aspect = r.width() / r.height().max(1.0);
        s.rebuild();
        s.cur = 0;
        s.shown_at = None;
        s.want = None;
        s.prev = None;
        s.paused = false;
        s.pause_at = None;
        s.toast = None;
        s.options_open = false;
        s.rects.clear();
        let now = ctx.input(|i| i.time);
        s.last_move = now;
        s.opened_at = now;
        s.last_pointer = None;
        let (tx, rx) = mpsc::channel();
        s.tx = Some(tx);
        s.rx = Some(rx);
        let (tx, rx) = mpsc::channel();
        s.sharp_tx = Some(tx);
        s.sharp_rx = Some(rx);
        s.timer_at = None;
        s.want_manual = false;
        s.sharp_failed.clear();
        s.sharp_need.clear();
        s.was_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        s.open = true;
        if !s.was_fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        crate::log(&format!("slideshow: {} photos, {} slides", self.slideshow.photos.len(), self.slideshow.slides.len()));
    }

    /// End the slideshow; the single view (or the grid) shows the last photo seen.
    pub(crate) fn close_slideshow(&mut self, ctx: &egui::Context) {
        let s = &mut self.slideshow;
        if !s.open {
            return;
        }
        s.open = false;
        if !s.was_fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        let last = s.slides.get(s.cur).map(|sl| sl.photos()[0]).and_then(|p| s.photos.get(p)).map(|p| (p.image, p.pos));
        s.extras.clear();
        s.asked.clear();
        s.targets.clear();
        s.face_boxes.clear();
        s.rx = None;
        s.tx = None;
        s.sharp.clear();
        s.sharp_order.clear();
        s.sharp_rx = None;
        s.sharp_tx = None;
        s.sharp_busy = None;
        s.photos.clear();
        s.slides.clear();
        if let Some((image, pos)) = last {
            if self.in_grid() {
                self.lib.set_active(image);
            } else if self.entries.get(pos).is_some_and(|e| e.image == image) {
                self.select(pos, false);
            }
        }
    }

    /// Every frame while open: keys, timer, loading ahead, drawing. Takes the whole window.
    pub(crate) fn slideshow_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.slideshow_test(&ctx, now);
        self.slideshow_receive(&ctx);
        if !self.slideshow_keys(&ctx, now) {
            self.close_slideshow(&ctx);
            return;
        }
        if !self.slideshow.open {
            return;
        }
        // screen shape changed (fullscreen arrived, other display): pairs follow
        let screen = ctx.content_rect();
        let aspect = screen.width() / screen.height().max(1.0);
        if screen.size() != self.slideshow.last_screen {
            self.slideshow.last_screen = screen.size();
            self.slideshow.sharp_need.clear();
        }
        if (aspect - self.slideshow.screen_aspect).abs() > 0.05 && !self.slideshow.photos.is_empty() {
            let current = self.slideshow.slides.get(self.slideshow.cur).map(|s| s.photos()[0]);
            self.slideshow.screen_aspect = aspect;
            self.slideshow.rebuild();
            self.slideshow.cur = current.and_then(|p| self.slideshow.slides.iter().position(|s| s.photos().contains(&p))).unwrap_or(0);
            self.slideshow.want = None;
        }
        self.slideshow_ahead();
        self.slideshow_sharp(&ctx, now);
        self.slideshow_step(now);
        let rm = crate::zoom::reduced_motion();
        egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(Color32::BLACK)).show(ui, |ui| {
            let painter = ui.painter().clone();
            let screen = ui.max_rect();
            let s = &self.slideshow;
            let dur = s.opts.seconds.max(1.0) as f64;
            let fade = if s.opts.transition == Transition::Dissolve && !rm { DISSOLVE } else { 0.0 };
            // the slide before, under the dissolve
            let mut fading = None;
            if let Some((p, since, shown)) = s.prev {
                let a = ((now - since) / fade.max(1e-6)) as f32;
                if fade > 0.0 && a < 1.0 {
                    fading = Some((p, shown, crate::crossfade::ease(a)));
                }
            }
            let cur = s.cur;
            let shown = s.shown_at;
            let paused_for = s.pause_at.map(|p| now - p).unwrap_or(0.0);
            if let Some((p, shown_p, _)) = fading {
                let t = ((now - shown_p) / (dur + fade)) as f32;
                self.slideshow_draw(&painter, screen, p, t, 1.0, false, now);
            }
            if let Some(sh) = shown {
                let t = ((now - sh - paused_for) / (dur + fade)) as f32;
                let alpha = fading.map(|f| f.2).unwrap_or(1.0);
                self.slideshow_draw(&painter, screen, cur, t, alpha, true, now);
            }
            self.slideshow_overlay(ui, screen, now);
        });
        // keep moving: Ken Burns, dissolve, timer
        if !self.slideshow.paused || self.slideshow.prev.is_some() || self.slideshow.toast.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Request the previews of the slides around the current one and their background data.
    fn slideshow_ahead(&mut self) {
        let s = &self.slideshow;
        let mut images = Vec::new();
        let mut want: Vec<usize> = ahead(s.slides.len(), s.cur);
        if let Some(w) = s.want {
            want.insert(0, w);
        }
        for sl in want {
            for p in s.slides[sl].photos() {
                images.push(s.photos[p].clone());
            }
        }
        let ids: Vec<i64> = images.iter().map(|p| p.image).collect();
        self.lib.request_thumbnails(&ids);
        self.lib.prefetch_large(&ids);
        for p in &images {
            // faces: a small query of the library, once per photo
            if !self.slideshow.targets.contains_key(&p.image) {
                let faces: Vec<[f32; 4]> = self
                    .lib
                    .faces_of(p.image)
                    .iter()
                    .map(|f| {
                        let a = crate::face_frames::rotate_quarters([f.x, f.y], p.rotate);
                        let b = crate::face_frames::rotate_quarters([f.x + f.b, f.y + f.h], p.rotate);
                        [a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()]
                    })
                    .collect();
                self.slideshow.targets.insert(p.image, face_target(&faces));
                self.slideshow.face_boxes.insert(p.image, face_union(&faces));
            }
            if self.slideshow.asked.insert(p.file) {
                let Some(tx) = self.slideshow.tx.clone() else { continue };
                let source = self.lib.preview_source(&[p.file]);
                let file = p.file;
                crate::threads::spawn(crate::threads::Lane::Ahead, move || {
                    let img = library::preview::read_developed(file, library::preview::small()).or_else(|| {
                        let (cache, keys) = source?;
                        cache.read(keys.first().copied().flatten()?, library::preview::small())
                    });
                    let (ambient, bright) = match img {
                        Some(img) => {
                            let tiny = image::imageops::resize(&img, 32, (32 * img.height() / img.width().max(1)).max(1), image::imageops::FilterType::Triangle);
                            (Some(ambient(&img)), brightest(&tiny))
                        }
                        None => (None, None),
                    };
                    let _ = tx.send(Computed { file, ambient, bright });
                });
            }
        }
    }

    fn slideshow_receive(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.slideshow.rx else { return };
        let got: Vec<Computed> = rx.try_iter().collect();
        for c in got {
            let ambient = c.ambient.map(|a| {
                let size = [a.width() as usize, a.height() as usize];
                ctx.load_texture(format!("slideshow-ambient-{}", c.file), egui::ColorImage::from_rgb(size, a.as_raw()), egui::TextureOptions::LINEAR)
            });
            self.slideshow.extras.insert(c.file, Extra { ambient, bright: c.bright });
        }
        // keep the memory small: only the backgrounds of photos near the current slide
        if self.slideshow.extras.len() > 24 {
            let keep: HashSet<i64> = ahead(self.slideshow.slides.len(), self.slideshow.cur).into_iter().flat_map(|s| self.slideshow.slides[s].photos()).map(|p| self.slideshow.photos[p].file).collect();
            let s = &mut self.slideshow;
            s.extras.retain(|f, _| keep.contains(f));
            s.asked.retain(|f| keep.contains(f));
        }
    }

    /// Are the sharp versions of slide `sl` there (or did they fail)?
    fn slideshow_sharp_ready(&self, sl: usize) -> bool {
        let s = &self.slideshow;
        let Some(slide) = s.slides.get(sl) else { return false };
        slide.photos().into_iter().all(|p| {
            let image = s.photos[p].image;
            s.sharp.contains_key(&image) || s.sharp_failed.contains(&image)
        })
    }

    /// Develop the photos of the current and the next slide in the screen's size, one at a time, in the background
    /// (current first). A large preview that is already large enough serves as it is.
    fn slideshow_sharp(&mut self, ctx: &egui::Context, now: f64) {
        let done: Vec<SharpDone> = self.slideshow.sharp_rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for d in done {
            self.slideshow.sharp_busy = None;
            match d.result {
                Ok(img) => {
                    crate::log(&format!("slideshow sharp: {} {}x{} (level {}) in {} ms", d.name, img.width(), img.height(), d.level, d.ms));
                    let size = [img.width() as usize, img.height() as usize];
                    let tex = ctx.load_texture(format!("slideshow-sharp-{}", d.image), egui::ColorImage::from_rgb(size, img.as_raw()), egui::TextureOptions::LINEAR);
                    let s = &mut self.slideshow;
                    s.sharp.insert(d.image, Sharp { tex, level: d.level, fingerprint: d.fingerprint, at: now });
                    for old in lru_touch(&mut s.sharp_order, d.image, SHARP_KEEP) {
                        s.sharp.remove(&old);
                    }
                }
                Err(e) => {
                    crate::log(&format!("slideshow sharp: {} failed: {e}", d.name));
                    self.slideshow.sharp_failed.insert(d.image);
                }
            }
            ctx.request_repaint();
        }
        // not before the window has gone full screen: the size would be the window's, and the photo would be
        // developed twice
        let settling = !self.slideshow.was_fullscreen && !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) && now - self.slideshow.opened_at < 2.0;
        if settling {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        // test hook GRAINEO_SLIDESHOW_SHARP=0: previews only (comparison screenshots)
        if self.slideshow.sharp_busy.is_some() || self.slideshow.slides.is_empty() || crate::env::var("GRAINEO_SLIDESHOW_SHARP").is_ok_and(|v| v == "0") {
            return;
        }
        // in order: the current slide, the one asked for, the next
        let s = &self.slideshow;
        let n = s.slides.len();
        let mut order = vec![s.cur];
        if let Some(w) = s.want {
            order.push(w);
        }
        order.push((s.cur + 1) % n);
        // the size each photo needs: from its rectangle drawn last frame, else from the layout of its slide
        let screen = ctx.content_rect();
        let ppp = ctx.pixels_per_point();
        let kb = s.opts.ken_burns && !crate::zoom::reduced_motion();
        let margin = if s.opts.backdrop == Backdrop::Photo { (screen.height() * 0.035).round() } else { 0.0 };
        let margin = if s.opts.fill { 0.0 } else { margin };
        let max_side = (ctx.input(|i| i.max_texture_side) as u32).max(64);
        let mut wanted: Vec<(Photo, u32)> = Vec::new();
        for sl in order {
            let photos: Vec<Photo> = s.slides[sl].photos().into_iter().map(|p| s.photos[p].clone()).collect();
            let aspects: Vec<f32> = photos.iter().map(|p| p.aspect).collect();
            let aims: Vec<_> = photos.iter().map(|p| self.slideshow_aim(p)).collect();
            let placed = place(screen, &aspects, s.opts.fill, margin, &aims);
            let moving = kb && photos.len() == 1;
            for (p, (r, crop)) in photos.into_iter().zip(placed) {
                // never beyond what the GPU takes as one texture (a 6K screen filled with a cropped photo would ask more)
                let need = s.sharp_need.get(&p.image).copied().unwrap_or_else(|| sharp_level_crop(r, crop, ppp, moving, screen)).min(max_side);
                wanted.push((p, need));
            }
        }
        for (p, need) in wanted {
            if self.slideshow.sharp_failed.contains(&p.image) {
                continue;
            }
            if !self.entries.get(p.pos).is_some_and(|e| e.image == p.image) {
                self.slideshow.sharp_failed.insert(p.image);
                continue;
            }
            self.ensure_entry(p.pos);
            let e = &self.entries[p.pos];
            let fingerprint = crate::browse::settings_fp(&e.settings);
            // a version a little smaller than measured (within the Ken Burns reserve) is kept: no second render
            if let Some(x) = self.slideshow.sharp.get(&p.image) {
                if x.level * 100 >= need * 92 && x.fingerprint == fingerprint {
                    lru_touch(&mut self.slideshow.sharp_order, p.image, SHARP_KEEP);
                    continue;
                }
            }
            // the large preview (offline copy, developed large level) when it is large enough
            if let Some((tex, true)) = self.lib.large_preview(p.image) {
                let sz = tex.size_vec2();
                if sz.x.max(sz.y) as u32 * 100 >= need * 98 {
                    let s = &mut self.slideshow;
                    s.sharp.insert(p.image, Sharp { tex, level: need, fingerprint, at: now });
                    for old in lru_touch(&mut s.sharp_order, p.image, SHARP_KEEP) {
                        s.sharp.remove(&old);
                    }
                    continue;
                }
            }
            let e = &self.entries[p.pos];
            let (path, settings, stored) = (e.path.clone(), e.settings.clone(), e.stored);
            let Some(tx) = self.slideshow.sharp_tx.clone() else { return };
            let current = self.slideshow.slides.get(self.slideshow.cur).is_some_and(|sl| sl.photos().iter().any(|&q| self.slideshow.photos[q].image == p.image));
            self.slideshow.sharp_busy = Some((p.image, need));
            let (image, name, ctx2) = (p.image, p.name.clone(), ctx.clone());
            // the photo on screen as a render the user waits for, the next one as work for the next moments
            let lane = if current { crate::threads::Lane::Front } else { crate::threads::Lane::Ahead };
            crate::threads::spawn(lane, move || {
                let t0 = std::time::Instant::now();
                let result = crate::photo_copy::develop_level(&path, &settings, stored, need);
                let _ = tx.send(SharpDone { image, level: need, fingerprint, result, ms: t0.elapsed().as_millis(), name });
                ctx2.request_repaint();
            });
            return;
        }
    }

    /// Is the first photo of slide `sl` there to be shown?
    fn slideshow_ready(&mut self, sl: usize) -> bool {
        let Some(slide) = self.slideshow.slides.get(sl).copied() else { return false };
        slide.photos().into_iter().all(|p| {
            let image = self.slideshow.photos[p].image;
            self.lib.large_preview(image).is_some()
        })
    }

    /// Timer and change of slides.
    fn slideshow_step(&mut self, now: f64) {
        let n = self.slideshow.slides.len();
        if n == 0 {
            return;
        }
        // the first slide: shown once its preview is there
        if self.slideshow.shown_at.is_none() {
            if self.slideshow_ready(self.slideshow.cur) {
                self.slideshow.shown_at = Some(now);
            }
            return;
        }
        // its time starts with the sharp version (or when that failed or took too long)
        if self.slideshow.timer_at.is_none() && (self.slideshow_sharp_ready(self.slideshow.cur) || now - self.slideshow.shown_at.unwrap_or(now) > SHARP_WAIT) {
            self.slideshow.timer_at = Some(now);
        }
        let s = &self.slideshow;
        let fade = if s.opts.transition == Transition::Dissolve && !crate::zoom::reduced_motion() { DISSOLVE } else { 0.0 };
        if let Some((_, since, _)) = s.prev {
            if now - since >= fade {
                self.slideshow.prev = None;
            }
        }
        let s = &self.slideshow;
        if s.want.is_none() && !s.paused && s.timer_at.is_some_and(|t| now - t >= s.opts.seconds.max(1.0) as f64) {
            self.slideshow.want = Some((s.cur + 1) % n);
            self.slideshow.want_manual = false;
            self.slideshow.timer_at = Some(now);
        }
        if let Some(w) = self.slideshow.want {
            // by the timer: wait for the next slide's sharp version (prepared during this one, normally there);
            // by a key: as soon as its preview is there, the sharp one fades in
            let sharp_ok = self.slideshow.want_manual || self.slideshow_sharp_ready(w) || now - self.slideshow.timer_at.unwrap_or(now) > SHARP_WAIT;
            if w == self.slideshow.cur {
                self.slideshow.want = None;
            } else if sharp_ok && self.slideshow_ready(w) {
                let s = &mut self.slideshow;
                s.prev = Some((s.cur, now, s.shown_at.unwrap_or(now)));
                s.cur = w;
                s.want = None;
                s.shown_at = Some(now);
                s.timer_at = None;
                s.want_manual = false;
                s.pause_at = if s.paused { Some(now) } else { None };
            }
        }
    }

    /// Draw slide `sl` (background and photos) with opacity `alpha`; `t` = progress of its time (Ken Burns).
    /// Target (faces, else the brightest region) and face frame of a photo, as far as known.
    fn slideshow_aim(&self, p: &Photo) -> (Option<[f32; 2]>, Option<[f32; 4]>) {
        let s = &self.slideshow;
        let target = s.targets.get(&p.image).copied().flatten().or_else(|| s.extras.get(&p.file).and_then(|e| e.bright));
        (target, s.face_boxes.get(&p.image).copied().flatten())
    }

    #[allow(clippy::too_many_arguments)]
    fn slideshow_draw(&mut self, painter: &egui::Painter, screen: Rect, sl: usize, t: f32, alpha: f32, current: bool, now: f64) {
        let Some(slide) = self.slideshow.slides.get(sl).copied() else { return };
        let photos: Vec<Photo> = slide.photos().into_iter().map(|p| self.slideshow.photos[p].clone()).collect();
        let mut tex = Vec::new();
        for p in &photos {
            match self.lib.large_preview(p.image) {
                Some((t, _)) => tex.push(t),
                None => return,
            }
        }
        let opts = self.slideshow.opts.clone();
        let tint = Color32::from_white_alpha((alpha * 255.0).round() as u8);
        // background
        let ambient = (opts.backdrop == Backdrop::Photo).then(|| self.slideshow.extras.get(&photos[0].file).and_then(|e| e.ambient.clone())).flatten();
        match ambient {
            Some(a) => {
                let sz = a.size_vec2();
                painter.image(a.id(), screen, cover_uv(screen, sz.x / sz.y.max(1.0)), tint);
            }
            None => {
                painter.rect_filled(screen, 0.0, Color32::from_black_alpha((alpha * 255.0) as u8));
            }
        }
        // the sharp version's shape when it is there (the preview may differ by a pixel or two)
        let sharp: Vec<Option<(egui::TextureHandle, f64)>> = photos.iter().map(|p| self.slideshow.sharp.get(&p.image).map(|x| (x.tex.clone(), x.at))).collect();
        let aspects: Vec<f32> = tex.iter().zip(&sharp).map(|(t, sh)| sh.as_ref().map(|s| s.0.size_vec2()).unwrap_or(t.size_vec2())).map(|s| s.x / s.y.max(1.0)).collect();
        let margin = if opts.backdrop == Backdrop::Photo && !opts.fill { (screen.height() * 0.035).round() } else { 0.0 };
        let aims: Vec<_> = photos.iter().map(|p| self.slideshow_aim(p)).collect();
        let placed = place(screen, &aspects, opts.fill, margin, &aims);
        let rects: Vec<Rect> = placed.iter().map(|x| x.0).collect();
        let crops: Vec<Rect> = placed.iter().map(|x| x.1).collect();
        let kb = opts.ken_burns && !crate::zoom::reduced_motion() && photos.len() == 1;
        if current {
            self.slideshow.rects.clear();
        }
        let ppp = painter.ctx().pixels_per_point();
        for ((((p, t_), r), sh), crop) in photos.iter().zip(&tex).zip(&rects).zip(&sharp).zip(&crops) {
            if current {
                self.slideshow.sharp_need.insert(p.image, sharp_level_crop(*r, *crop, ppp, kb, screen));
            }
            let uv = if kb {
                // the motion inside the part shown (filling: the crop), aimed at the same target
                let target = self.slideshow_aim(p).0.map(|t| [((t[0] - crop.min.x) / crop.width()).clamp(0.0, 1.0), ((t[1] - crop.min.y) / crop.height()).clamp(0.0, 1.0)]);
                let number = self.slideshow.slides.iter().position(|s| *s == slide).unwrap_or(0);
                within(*crop, uv_at(&motion(target, number), t))
            } else {
                *crop
            };
            // the shadow only around a photo that does not reach the screen's edges
            if opts.backdrop == Backdrop::Photo && *crop == Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)) {
                // a soft shadow lifts the photo off the background
                painter.add(egui::Shape::Rect(egui::epaint::RectShape::filled(*r, 0.0, Color32::from_black_alpha((alpha * 110.0) as u8)).with_blur_width(28.0)));
            }
            // the preview until the sharp version is there, then that one fades in over it
            let fade_in = sh.as_ref().map(|(_, at)| crate::crossfade::ease(((now - at) / SHARP_FADE) as f32)).unwrap_or(0.0);
            if fade_in < 1.0 {
                painter.image(t_.id(), *r, uv, tint);
            }
            if let Some((st, _)) = sh {
                painter.image(st.id(), *r, uv, Color32::from_white_alpha((alpha * fade_in * 255.0).round() as u8));
            }
            if current {
                self.slideshow.rects.push((p.image, *r));
            }
        }
    }

    /// Info line, rating mark, pause sign, bar with the options.
    fn slideshow_overlay(&mut self, ui: &mut egui::Ui, screen: Rect, now: f64) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        // pointer: shows the bar, hides after a moment of rest
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        if pointer.is_some() && pointer != self.slideshow.last_pointer {
            if self.slideshow.last_pointer.is_some() {
                self.slideshow.last_move = now;
            }
            self.slideshow.last_pointer = pointer;
        }
        let bar_rect = Rect::from_center_size(Pos2::new(screen.center().x, screen.bottom() - 46.0), Vec2::new(196.0, 36.0));
        let over_ui = pointer.is_some_and(|p| bar_rect.expand(12.0).contains(p)) || self.slideshow.options_open;
        let active = now - self.slideshow.last_move < IDLE_HIDE || over_ui;
        if !active {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        let s = &self.slideshow;
        let font = egui::FontId::monospace(lr::UI_SIZE);
        // info line (I)
        if s.opts.info {
            if let Some(slide) = s.slides.get(s.cur) {
                let mut parts = Vec::new();
                for p in slide.photos() {
                    let ph = &s.photos[p];
                    let mut t = ph.name.clone();
                    if let Some(r) = self.lib.row_of_image(ph.image) {
                        if let Some(c) = r.capture.as_deref() {
                            t += &format!("  ·  {}", c.split('T').next().unwrap_or(c));
                        }
                        if let Some(c) = r.camera.as_deref() {
                            t += &format!("  ·  {c}");
                        }
                        if r.stars > 0 {
                            t += &format!("  ·  {}", "★".repeat(r.stars as usize));
                        }
                    }
                    parts.push(t);
                }
                let text = format!("{}      {} / {}", parts.join("   |   "), s.cur + 1, s.slides.len());
                let pos = Pos2::new(screen.left() + 22.0, screen.bottom() - 18.0);
                painter.text(pos + Vec2::new(1.0, 1.0), egui::Align2::LEFT_BOTTOM, &text, font.clone(), Color32::from_black_alpha(160));
                painter.text(pos, egui::Align2::LEFT_BOTTOM, &text, font.clone(), Color32::from_white_alpha(170));
            }
        }
        // rating mark: small, in the middle at the bottom, fades
        if let Some((text, at)) = s.toast.clone() {
            let age = now - at;
            if age > TOAST {
                self.slideshow.toast = None;
            } else {
                let a = (1.0 - ((age - (TOAST - 0.4)) / 0.4).clamp(0.0, 1.0)) as f32;
                let center = Pos2::new(screen.center().x, screen.bottom() - 96.0);
                let galley = painter.layout_no_wrap(text, font.clone(), Color32::from_white_alpha((230.0 * a) as u8));
                let r = Rect::from_center_size(center, galley.size() + Vec2::new(28.0, 14.0));
                painter.rect_filled(r, 4.0, Color32::from_black_alpha((150.0 * a) as u8));
                painter.galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
            }
        }
        let s = &self.slideshow;
        // pause: two small bars at the top right while the bar is hidden
        if s.paused && !active {
            let c = Pos2::new(screen.right() - 34.0, screen.top() + 30.0);
            for dx in [-4.0, 4.0] {
                painter.rect_filled(Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(4.0, 14.0)), 1.0, Color32::from_white_alpha(150));
            }
        }
        if !active {
            self.slideshow.options_open = false;
            return;
        }
        // the bar: previous, pause/play, next, options, end
        let bar_painter = ui.painter();
        bar_painter.rect_filled(bar_rect, 6.0, Color32::from_black_alpha(170));
        let mut cmd = None;
        let icons = [(lr::Icon::CaretLeft, "Voriges Foto", "slideshow_prev"), (if s.paused { lr::Icon::Play } else { lr::Icon::Pause }, if s.paused { "Weiter abspielen" } else { "Anhalten" }, "slideshow_pause"), (lr::Icon::Chevron, "Nächstes Foto", "slideshow_next"), (lr::Icon::Sliders, "Optionen der Diashow", ""), (lr::Icon::X, "Diashow beenden", "slideshow_end")];
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect.shrink2(Vec2::new(8.0, 6.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        child.spacing_mut().item_spacing.x = 10.0;
        let mut options_button = None;
        for (n, (icon, tip, key)) in icons.into_iter().enumerate() {
            let on = n == 3 && self.slideshow.options_open;
            let r = child.allocate_ui(Vec2::new(26.0, 24.0), |ui| lr::icon_button_hint(ui, icon, on, &lr::with_shortcut(tip, key)));
            if n == 3 {
                options_button = Some(r.response.rect);
            }
            if r.inner {
                cmd = Some(n);
            }
        }
        match cmd {
            Some(0) => self.slideshow_go(-1),
            Some(1) => self.slideshow_pause(now),
            Some(2) => self.slideshow_go(1),
            Some(3) => self.slideshow.options_open = !self.slideshow.options_open,
            Some(4) => {
                self.close_slideshow(&ctx);
                return;
            }
            _ => {}
        }
        if self.slideshow.options_open {
            let anchor = options_button.map(|r| r.center_top()).unwrap_or(bar_rect.center_top());
            self.slideshow_options(&ctx, anchor);
        }
    }

    /// The small options panel above the bar; changes apply at once and are saved.
    fn slideshow_options(&mut self, ctx: &egui::Context, anchor: Pos2) {
        let before = self.slideshow.opts.clone();
        let mut o = before.clone();
        let w = 290.0;
        egui::Area::new(egui::Id::unique("slideshow-options")).order(egui::Order::Foreground).fixed_pos(Pos2::new(anchor.x - w / 2.0, anchor.y - 12.0)).pivot(egui::Align2::LEFT_BOTTOM).show(ctx, |ui| {
            egui::Frame::new().fill(lr::PANEL()).stroke(egui::Stroke::new(1.0, lr::gray(0x3a))).corner_radius(4.0).inner_margin(12.0).show(ui, |ui| {
                ui.set_width(w - 24.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                lr::subheading(ui, crate::tr("Diashow"));
                let mut secs = o.seconds.round() as u32;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(crate::tr("Dauer")).color(lr::TEXT_DIM()));
                    ui.add_space(8.0);
                    lr::segmented(ui, &mut secs, &[(3, "3 s"), (5, "5 s"), (8, "8 s"), (12, "12 s")]);
                });
                o.seconds = secs as f32;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(crate::tr("Übergang")).color(lr::TEXT_DIM()));
                    ui.add_space(8.0);
                    lr::segmented(ui, &mut o.transition, &[(Transition::Dissolve, crate::tr("Überblenden")), (Transition::Cut, crate::tr("Schnitt"))]);
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(crate::tr("Größe")).color(lr::TEXT_DIM()));
                    ui.add_space(8.0);
                    lr::segmented(ui, &mut o.fill, &[(false, crate::tr("Einpassen")), (true, crate::tr("Füllen"))]);
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(crate::tr("Hintergrund")).color(lr::TEXT_DIM()));
                    ui.add_space(8.0);
                    lr::segmented(ui, &mut o.backdrop, &[(Backdrop::Photo, crate::tr("Aus dem Foto")), (Backdrop::Black, crate::tr("Nur schwarz"))]);
                });
                let r = lr::switch_label(ui, &mut o.ken_burns, crate::tr("Ken-Burns-Effekt"));
                lr::hover_tip(ui, &r, crate::tr("Langsamer Zoom auf Gesichter oder die hellste Stelle"));
                let r = lr::switch_label(ui, &mut o.pairs, crate::tr("Hochformate paarweise"));
                lr::hover_tip(ui, &r, crate::tr("Zwei Hochformate nebeneinander auf einem Querformat-Bildschirm"));
                lr::switch_label(ui, &mut o.shuffle, crate::tr("Zufällige Reihenfolge"));
                let r = lr::switch_label(ui, &mut o.info, crate::tr("Infozeile"));
                lr::tip_key(ui, &r, crate::tr("Infozeile"), &crate::shortcuts::display("slideshow_info"));
            });
        });
        if o != before {
            let rebuild = o.pairs != before.pairs || o.shuffle != before.shuffle;
            if o.fill != before.fill {
                // the sizes needed change with the mode
                self.slideshow.sharp_need.clear();
            }
            o.save();
            self.slideshow.opts = o;
            if rebuild {
                self.slideshow_rebuild_keep();
            }
        }
    }

    /// Order or pairs changed: rebuild, the photo on screen stays.
    fn slideshow_rebuild_keep(&mut self) {
        let s = &mut self.slideshow;
        let current = s.slides.get(s.cur).map(|sl| sl.photos()[0]);
        if let Some(c) = current {
            s.start_photo = c;
        }
        s.rebuild();
        s.cur = current.and_then(|p| s.slides.iter().position(|x| x.photos().contains(&p))).unwrap_or(0);
        s.want = None;
        s.prev = None;
    }

    fn slideshow_go(&mut self, d: isize) {
        let s = &mut self.slideshow;
        let n = s.slides.len();
        if n == 0 {
            return;
        }
        let from = s.want.unwrap_or(s.cur);
        s.want = Some((from as isize + d).rem_euclid(n as isize) as usize);
        s.want_manual = true;
    }

    fn slideshow_pause(&mut self, now: f64) {
        let s = &mut self.slideshow;
        s.paused = !s.paused;
        if s.paused {
            s.pause_at = Some(now);
        } else if let (Some(p), Some(sh)) = (s.pause_at.take(), s.shown_at) {
            // the time of the slide stood still while paused
            s.shown_at = Some(sh + (now - p));
            s.timer_at = s.timer_at.map(|t| t + (now - p));
        }
    }

    /// Keys of the slideshow (it takes them all). Returns `false` when it should end.
    fn slideshow_keys(&mut self, ctx: &egui::Context, now: f64) -> bool {
        use crate::shortcuts::pressed;
        if pressed(ctx, "slideshow_end") {
            if self.slideshow.options_open {
                self.slideshow.options_open = false;
                return true;
            }
            return false;
        }
        if pressed(ctx, "slideshow_pause") {
            self.slideshow_pause(now);
        }
        if pressed(ctx, "slideshow_next") {
            self.slideshow_go(1);
        }
        if pressed(ctx, "slideshow_prev") {
            self.slideshow_go(-1);
        }
        if pressed(ctx, "slideshow_info") {
            let s = &mut self.slideshow;
            s.opts.info = !s.opts.info;
            s.opts.save();
        }
        // rating keys as everywhere (0–5, 6–9, P, X, U)
        let mut changes = Vec::new();
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Key { physical_key, key, pressed: true, modifiers, repeat: false } = e {
                    if modifiers.command || modifiers.ctrl || modifiers.alt {
                        continue;
                    }
                    if let Some(c) = crate::rating::change(physical_key.unwrap_or(*key)) {
                        changes.push(c);
                    }
                }
            }
        });
        for c in changes {
            self.slideshow_rate(c, now);
        }
        true
    }

    /// Rate the photo of the slide (in a pair: the one under the pointer when the mouse was moved, else both).
    pub(crate) fn slideshow_rate(&mut self, c: crate::rating::Change, now: f64) {
        let s = &self.slideshow;
        let pointer = s.last_pointer.filter(|_| now - s.last_move < IDLE_HIDE);
        let under: Vec<i64> = s.rects.iter().filter(|(_, r)| pointer.is_some_and(|p| r.contains(p))).map(|(b, _)| *b).collect();
        let target: Vec<i64> = if !under.is_empty() { under } else { s.rects.iter().map(|(b, _)| *b).collect() };
        if target.is_empty() {
            return;
        }
        let (st, fl, la) = match c {
            crate::rating::Change::Stars(n) => (Some(n), None, None),
            crate::rating::Change::Flag(n) => (None, Some(n), None),
            crate::rating::Change::Label(n) => (None, None, Some(n)),
        };
        let new = self.lib.rate_images(&target, st, fl, la);
        self.adopt_values(&new);
        let text = if self.lib.take_rating_refused() { Some(crate::tr(crate::library_ui::RATING_REFUSED).to_string()) } else { crate::rating::feedback(c, &new) };
        if let Some(t) = text {
            self.slideshow.toast = Some((t, now));
        }
    }

    /// Test hook (see the module text).
    fn slideshow_test(&mut self, ctx: &egui::Context, now: f64) {
        let s = &mut self.slideshow;
        if s.test_start.is_none() {
            s.test_start = Some(now);
            s.test = crate::env::var("GRAINEO_SLIDESHOW_TEST")
                .ok()
                .map(|v| v.split(';').filter_map(|p| p.split_once('@').and_then(|(c, t)| Some((c.trim().to_string(), t.trim().parse::<f64>().ok()?)))).collect())
                .unwrap_or_default();
        }
        let start = s.test_start.unwrap_or(now);
        let due: Vec<String> = s.test.iter().filter(|(_, t)| now - start >= *t).map(|(c, _)| c.clone()).collect();
        s.test.retain(|(_, t)| now - start < *t);
        for c in due {
            crate::log(&format!("slideshow test: {c}"));
            match c.as_str() {
                "info" => self.slideshow.opts.info = !self.slideshow.opts.info,
                "next" => self.slideshow_go(1),
                "prev" => self.slideshow_go(-1),
                "pause" => self.slideshow_pause(now),
                "bar" => self.slideshow.last_move = now + 30.0,
                "options" => {
                    self.slideshow.last_move = now + 30.0;
                    self.slideshow.options_open = true;
                }
                "black" => self.slideshow.opts.backdrop = Backdrop::Black,
                "fill" => {
                    self.slideshow.opts.fill = true;
                    self.slideshow.sharp_need.clear();
                }
                "photo" => self.slideshow.opts.backdrop = Backdrop::Photo,
                "end" => self.close_slideshow(ctx),
                c if c.starts_with("rate=") => {
                    if let Ok(n) = c[5..].parse::<u8>() {
                        self.slideshow_rate(crate::rating::Change::Stars(n.min(5)), now);
                    }
                }
                _ => {}
            }
        }
    }

    /// Test hook `GRAINEO_SLIDESHOW=<s>`: start once the photos of the view are there.
    pub(crate) fn slideshow_autostart(&mut self, ctx: &egui::Context) {
        if REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed) && !self.slideshow.open {
            self.open_slideshow(ctx);
            return;
        }
        thread_local! {
            static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if DONE.with(|d| d.get()) {
            return;
        }
        let Some(after) = crate::env::var("GRAINEO_SLIDESHOW").ok().and_then(|v| v.trim().parse::<f64>().ok()) else {
            DONE.with(|d| d.set(true));
            return;
        };
        if ctx.input(|i| i.time) >= after {
            // in the grid the photo list follows only on demand
            self.sync_source(false);
        }
        if ctx.input(|i| i.time) < after || self.entries.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        DONE.with(|d| d.set(true));
        self.open_slideshow(ctx);
    }
}

impl Slideshow {
    fn rebuild(&mut self) {
        let o = order(self.photos.len(), self.opts.shuffle, self.order_seed, self.start_photo);
        let photos = &self.photos;
        self.slides = slides(&o, |i| photos[i].aspect, self.opts.pairs, self.screen_aspect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_keeps_all_and_starts_with_first() {
        let o = order(10, false, 1, 3);
        assert_eq!(o, vec![3, 4, 5, 6, 7, 8, 9, 0, 1, 2]);
        for seed in [1, 2, 99, 12345] {
            let mut s = order(50, true, seed, 7);
            assert_eq!(s[0], 7);
            s.sort();
            assert_eq!(s, (0..50).collect::<Vec<_>>());
        }
        assert_ne!(order(50, true, 1, 0), order(50, true, 2, 0), "different seeds, different order");
        assert_eq!(order(50, true, 5, 0), order(50, true, 5, 0), "deterministic");
        assert!(order(0, true, 1, 0).is_empty());
    }

    #[test]
    fn portraits_pair_on_landscape_screens_only() {
        let aspects = [1.5, 0.66, 0.75, 0.66, 1.5, 0.66];
        let a = |i: usize| aspects[i];
        let o: Vec<usize> = (0..6).collect();
        assert_eq!(slides(&o, a, true, 16.0 / 9.0), vec![Slide::One(0), Slide::Pair(1, 2), Slide::One(3), Slide::One(4), Slide::One(5)]);
        // off, or a portrait screen: every photo alone
        assert_eq!(slides(&o, a, false, 16.0 / 9.0).len(), 6);
        assert_eq!(slides(&o, a, true, 9.0 / 16.0).len(), 6);
        // squares and landscapes never pair
        assert_eq!(slides(&[0, 1], |_| 1.0, true, 1.78), vec![Slide::One(0), Slide::One(1)]);
        // two portraits too wide together for a 4:3 screen stay alone
        assert_eq!(slides(&[0, 1], |_| 0.85, true, 1.33).len(), 2);
        assert_eq!(slides(&[0, 1], |_| 0.85, true, 1.2), vec![Slide::One(0), Slide::One(1)]);
    }

    #[test]
    fn loading_ahead_covers_neighbours() {
        assert_eq!(ahead(10, 0), vec![0, 1, 9, 2]);
        assert_eq!(ahead(10, 9), vec![9, 0, 8, 1]);
        assert_eq!(ahead(2, 0), vec![0, 1]);
        assert_eq!(ahead(1, 0), vec![0]);
        assert!(ahead(0, 0).is_empty());
    }

    #[test]
    fn ken_burns_stays_inside_and_aims_at_target() {
        for target in [None, Some([0.0, 0.0]), Some([1.0, 1.0]), Some([0.3, 0.7])] {
            for n in 0..2 {
                let m = motion(target, n);
                for k in 0..=20 {
                    let r = uv_at(&m, k as f32 / 20.0);
                    assert!(r.min.x >= -1e-6 && r.min.y >= -1e-6 && r.max.x <= 1.0 + 1e-6 && r.max.y <= 1.0 + 1e-6, "{r:?}");
                    assert!(r.width() >= 1.0 / KB_ZOOM - 1e-4);
                }
            }
        }
        // zooming in on a face at the upper left: the end view lies towards it
        let end = uv_at(&motion(Some([0.2, 0.2]), 0), 1.0);
        assert!(end.center().x < 0.5 && end.center().y < 0.5);
        let start = uv_at(&motion(Some([0.2, 0.2]), 0), 0.0);
        assert_eq!(start, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)));
        // the next slide zooms out
        assert_eq!(uv_at(&motion(Some([0.2, 0.2]), 1), 1.0), start);
    }

    #[test]
    fn face_target_weights_larger_faces() {
        assert_eq!(face_target(&[]), None);
        // synthetic: a large face on the left, a small one on the right
        let t = face_target(&[[0.1, 0.3, 0.2, 0.2], [0.8, 0.3, 0.05, 0.05]]).unwrap();
        assert!(t[0] < 0.3, "{t:?}");
        let one = face_target(&[[0.6, 0.2, 0.1, 0.2]]).unwrap();
        assert!((one[0] - 0.65).abs() < 1e-5 && (one[1] - 0.3).abs() < 1e-5);
    }

    #[test]
    fn brightest_finds_light_and_ignores_flat() {
        let mut img = image::RgbImage::from_pixel(32, 20, image::Rgb([30, 30, 30]));
        for y in 2..6 {
            for x in 24..30 {
                img.put_pixel(x, y, image::Rgb([250, 240, 230]));
            }
        }
        let b = brightest(&img).unwrap();
        assert!(b[0] > 0.7 && b[1] < 0.35, "{b:?}");
        assert_eq!(brightest(&image::RgbImage::from_pixel(32, 20, image::Rgb([128, 128, 128]))), None);
    }

    #[test]
    fn ambient_is_small_and_dark() {
        let img = image::RgbImage::from_pixel(1280, 853, image::Rgb([240, 120, 40]));
        let a = ambient(&img);
        assert_eq!(a.width(), 24);
        let p = a.get_pixel(12, 8);
        assert!(p[0] < 140 && p[0] > p[2], "{p:?}");
    }

    #[test]
    fn sharp_level_follows_the_screen_never_beyond() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1512.0, 982.0));
        // a landscape filling the height on a Retina screen (2 px per point), with and without the zoom
        let r = fit(screen, 1.5);
        assert_eq!(sharp_level(r, 2.0, false, screen), (r.width() * 2.0).ceil() as u32);
        assert!(sharp_level(r, 2.0, true, screen) > sharp_level(r, 2.0, false, screen));
        assert!(sharp_level(screen, 2.0, true, screen) <= 3024);
        // a portrait of a pair needs only its height
        let pair = layout(screen, &[0.66, 0.66], 0.0);
        assert_eq!(sharp_level(pair[0], 2.0, false, screen), (pair[0].height() * 2.0).ceil() as u32);
    }

    #[test]
    fn filling_crops_towards_the_target_keeps_faces_and_limits_the_cut() {
        // a 3:2 landscape on a 16:10 screen: only the sides go, centred
        let uv = fill_uv(1.5, 1.6, None, None).unwrap();
        assert!((uv.width() - 1.0).abs() < 1e-5 && (uv.height() - 0.9375).abs() < 1e-4);
        assert!((uv.center().y - 0.5).abs() < 1e-4);
        // the target near the top: the crop moves up as far as it can
        let up = fill_uv(1.5, 1.6, Some([0.5, 0.0]), None).unwrap();
        assert!(up.min.y.abs() < 1e-5);
        // a face low at the bottom but the target in the middle: the face stays whole (synthetic frames)
        let face = [0.4, 0.92, 0.1, 0.07];
        let f = fill_uv(1.5, 1.6, Some([0.5, 0.5]), Some(face)).unwrap();
        assert!(f.max.y >= face[1] + face[3] - 1e-5, "{f:?}");
        // a portrait on a landscape screen would lose more than 30 %: fitted instead
        assert!(fill_uv(0.66, 1.6, None, None).is_none());
        // a portrait in its half of a pair (aspect 0.8) loses less: filled
        assert!(fill_uv(0.66, 0.8, None, None).is_some());
        // more faces than fit: the top of the frame (the heads) wins
        let tall = fill_uv(1.5, 3.0, None, Some([0.2, 0.1, 0.1, 0.8]));
        assert!(tall.is_none(), "cut beyond 30 % fits instead");
        let g = fill_uv(1.0, 1.25, None, Some([0.3, 0.05, 0.4, 0.9])).unwrap();
        assert!((g.min.y - 0.05).abs() < 1e-5, "{g:?}");
        let u = face_union(&[[0.1, 0.1, 0.1, 0.1], [0.5, 0.4, 0.2, 0.2]]).unwrap();
        assert!(u.iter().zip([0.1, 0.1, 0.6, 0.5]).all(|(a, b)| (a - b).abs() < 1e-5), "{u:?}");
    }

    #[test]
    fn place_fills_cells_and_falls_back_to_fit() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0));
        let one = place(screen, &[1.5], true, 0.0, &[]);
        assert_eq!(one[0].0, screen);
        // a single portrait would lose too much: fitted, whole photo
        let p = place(screen, &[0.66], true, 0.0, &[]);
        assert!(p[0].0.width() < 700.0 && p[0].1.width() == 1.0 && p[0].1.height() == 1.0);
        // a pair: each fills its half
        let two = place(screen, &[0.66, 0.75], true, 0.0, &[]);
        assert!((two[0].0.height() - 1000.0).abs() < 0.01 && two[0].0.right() < two[1].0.left());
        assert!(two[0].1.height() < 1.0 || two[0].1.width() < 1.0);
        // fit mode is the old layout
        assert_eq!(place(screen, &[1.5], false, 0.0, &[])[0].0, layout(screen, &[1.5], 0.0)[0]);
        // the sharp version of a filled photo is larger than the screen by the part cut away, never beyond
        let crop = fill_uv(0.66, 0.8, None, None).unwrap();
        let lv = sharp_level_crop(two[0].0, crop, 2.0, false, screen);
        assert!(lv as f32 > two[0].0.height() * 2.0);
        assert!(lv as f32 <= 1600.0 * 2.0 / crop.width().min(crop.height()) + 1.0);
    }

    #[test]
    fn lru_keeps_the_newest() {
        let mut o = Vec::new();
        for b in 1..=4 {
            assert!(lru_touch(&mut o, b, 4).is_empty());
        }
        // paging back to 2: it moves to the front, nothing falls out
        assert!(lru_touch(&mut o, 2, 4).is_empty());
        assert_eq!(o, vec![2, 4, 3, 1]);
        assert_eq!(lru_touch(&mut o, 5, 4), vec![1]);
        assert_eq!(o, vec![5, 2, 4, 3]);
    }

    #[test]
    fn layout_fits_and_pairs_share_height() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0));
        let one = layout(screen, &[1.5], 0.0);
        assert_eq!(one[0].height(), 900.0);
        assert!((one[0].width() - 1350.0).abs() < 0.01);
        let two = layout(screen, &[0.66, 0.75], 30.0);
        assert_eq!(two.len(), 2);
        assert!((two[0].height() - two[1].height()).abs() < 0.01);
        assert!(two[0].right() < two[1].left());
        assert!(screen.shrink(30.0).contains_rect(two[0]) && screen.shrink(30.0).contains_rect(two[1]));
    }
}
