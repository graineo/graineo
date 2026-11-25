//! Comparison views of the single image, drawn by one pane renderer:
//!
//! - **Default development | Current** (Y) and the single "Default development" view (\): the left side is selectable –
//!   graineo's base development (default settings), the Lightroom start values, the camera JPEG, a version (virtual
//!   copy) of the same file, the current state with the other binning (Quattro at high ISO) or a
//!   snapshot (the state held on a click; kept in memory per photo for the session, never written anywhere). The
//!   choice is remembered for the session.
//! - **Look comparison** (switch "Compare" in "Film & Look", or ⌘-click on a look tile): the current state plus up to
//!   three looks in 2–4 equal fields. A click on a field adopts its look, Esc ends without a change.
//!
//! All panes show the same crop of the image with the geometry (crop, rotation) of the current settings; zoom and pan
//! act on all of them. Each pane gets its preview in the background (`Job::HoverRender`) and, when zoomed beyond the
//! preview, a crop at full resolution (`Job::RenderRegion`). Only the current pane may use the whole image at full
//! resolution (it is the normal detail image); the other panes only keep crops of the visible part, so that several
//! fields do not multiply the texture memory.

use develop::geometry::Frame;
use develop::Settings;
use eframe::egui::{self, Color32, Pos2, Rect, TextureHandle, TextureOptions, Vec2};

use crate::film_look::Choice;
use crate::jobs::Job;
use crate::{darkroom_loupe, lr, App};

/// What the left side of "Unedited | Current" shows.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum BeforeSource {
    /// graineo's base development (default settings)
    #[default]
    Unedited,
    /// the start values from Lightroom (non-Sigma RAWs from a Lightroom catalog)
    LightroomStart,
    /// the JPEG embedded by the camera
    CameraJpeg,
    /// a version (virtual copy) of the same file; `None` is the original
    Version(Option<String>),
    /// the current settings with the other binning (Quattro at high ISO, see `binning_ui.rs`)
    Binning,
    /// the settings held by "Jetzigen Stand festhalten" (this photo / version, this session)
    Snapshot,
    /// a step of the develop history ("Mit aktuellem Stand vergleichen", `history_panel.rs`)
    HistoryStep,
}

/// The photo (path and virtual copy) a snapshot belongs to.
pub type SnapKey = (std::path::PathBuf, Option<String>);

/// Snapshots of the current state, per photo and version, in memory only: (key, settings, time in seconds).
#[derive(Default)]
pub struct Snapshots(Vec<(SnapKey, Settings, i64)>);

impl Snapshots {
    /// Hold `settings` for `key` (replaces an earlier snapshot of the same photo).
    pub fn set(&mut self, key: SnapKey, settings: Settings, at: i64) {
        self.0.retain(|(k, ..)| *k != key);
        self.0.push((key, settings, at));
    }
    pub fn get(&self, key: &SnapKey) -> Option<(&Settings, i64)> {
        self.0.iter().find(|(k, ..)| k == key).map(|(_, s, t)| (s, *t))
    }
}

/// Does a pane with settings `ps` need a session of its own (the one on screen has another binning or another base
/// of Sigma's noise reduction / colour mode)? Its previews and crops then come from a second session.
pub fn needs_own_session(loaded: &crate::jobs::Loaded, ps: &Settings, current: &Settings, path: &std::path::Path) -> bool {
    if loaded.session.is_none() {
        return false;
    }
    !crate::binning_ui::fits(loaded, ps) || (develop::Session::uses_spp(ps) && ps.spp_key(path) != current.spp_key(path))
}

/// Time of day of a snapshot for the tag ("14:32").
fn clock(t: i64) -> String {
    use crate::menu::i18n;
    let l = library::platform::local_time(t);
    i18n::format::time(l.hour as u8, l.minute as u8, None).unwrap_or_else(|| format!("{:02}:{:02}", l.hour, l.minute))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Remembered choice of the left side and its list.
#[derive(Default)]
pub struct BeforeChoice {
    pub source: BeforeSource,
    pub menu_open: bool,
    /// Lightroom start values with Upright resolved, per image (detecting lines takes a moment)
    lr_cache: Option<(usize, Settings)>,
    /// camera JPEG requested for this image (no repeated requests if there is none)
    jpeg_requested: Option<usize>,
    test_done: bool,
    /// second session in the other binning size for [`BeforeSource::Binning`]
    pub binning_alt: crate::binning_ui::Old,
    pub snapshots: Snapshots,
}

/// One look in the look comparison.
#[derive(Clone)]
pub struct Pick {
    pub key: String,
    pub name: String,
    pub choice: Choice,
}

/// State of the look comparison.
#[derive(Default)]
pub struct LookCompare {
    /// switch "Compare" in "Film & Look": a click on a tile picks instead of applying
    pub on: bool,
    pub picks: Vec<Pick>,
    /// last frame in which the "Film & Look" panel was drawn (the comparison ends with the panel)
    pub panel_frame: u64,
    pub test_done: bool,
}

/// At most this many looks besides the current state.
pub const MAX_PICKS: usize = 3;

impl LookCompare {
    /// Fields are shown (switch on and at least one look picked).
    pub fn showing(&self) -> bool {
        self.on && !self.picks.is_empty()
    }
    /// Number of the tile in the comparison (1 … 3).
    pub fn number(&self, key: &str) -> Option<usize> {
        self.picks.iter().position(|p| p.key == key).map(|i| i + 1)
    }
    /// Add a look or take it out again. `false`: already three looks.
    pub fn toggle(&mut self, pick: Pick) -> bool {
        self.on = true;
        if let Some(i) = self.picks.iter().position(|p| p.key == pick.key) {
            self.picks.remove(i);
            true
        } else if self.picks.len() < MAX_PICKS {
            self.picks.push(pick);
            true
        } else {
            false
        }
    }
    pub fn end(&mut self) {
        self.on = false;
        self.picks.clear();
    }
}

/// Settings of a look applied to the current settings (only look/film are exchanged, as when applying it).
pub fn with_look(choice: &Choice, current: &Settings) -> Settings {
    let mut t = choice.apply(current);
    rawdev::film_profiles::remember_reference(&mut t);
    t
}

/// What a pane shows.
#[derive(Clone, PartialEq)]
pub enum PaneImage {
    Current,
    Settings(Settings),
    CameraJpeg,
    /// the photo developed by the system RAW engine of macOS (apple_raw.rs)
    Apple,
}

pub struct Pane {
    pub label: String,
    pub image: PaneImage,
}

/// Previews and full-resolution crops of the panes that do not show the current settings.
#[derive(Default)]
pub struct PaneTextures {
    previews: Vec<(Settings, TextureHandle)>,
    /// requested previews with the time of the request (for the log)
    requested: Vec<(Settings, f64)>,
    /// crops at full resolution: settings, region in the source image, texture pixels per image pixel, texture,
    /// region as drawn (in pixels of the image on screen; differs from the region for another binning size)
    details: Vec<(Settings, [usize; 4], usize, TextureHandle, [f32; 4])>,
    detail_requested: Option<(Settings, f64)>,
    /// pixels of the requested crop's image per pixel of the image on screen (x, y): 1, or the size ratio of the other
    /// binning ([`BeforeSource::Binning`])
    detail_scale: [f32; 2],
    /// previews / crops that predate a new base of the current image ([`Self::invalidate`]): still painted until the
    /// new picture is there (never a dark gap, never a flash), but requested again
    stale_previews: Vec<Settings>,
    stale_details: Vec<Settings>,
    /// per pane: the settings of the picture last painted there (kept until the next picture is there, so that a moving
    /// slider never turns the pane dark)
    shown: Vec<Option<Settings>>,
    /// time of the last preview or crop: settings, milliseconds, crop (shown in the pipeline comparison)
    pub last_ms: Option<(Settings, f64, bool)>,
}

impl PaneTextures {
    /// Forget everything (other image, new base of the image).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// The base of the current image changed (exact Sigma base arrived, previews rebuilt): what is painted stays until
    /// the new picture arrives; results still on their way are dropped (they were made from the old base).
    pub fn invalidate(&mut self) {
        self.stale_previews = self.previews.iter().map(|(s, _)| s.clone()).collect();
        self.stale_details = self.details.iter().map(|(s, ..)| s.clone()).collect();
        self.requested.clear();
        self.detail_requested = None;
    }
    /// The picture of a pane that has a session of its own does not depend on the base of the current image: it stays valid.
    fn revalidate(&mut self, s: &Settings) {
        self.stale_previews.retain(|p| p != s);
        self.stale_details.retain(|p| p != s);
    }
    /// The picture to paint for pane `k` with settings `s`: its own, else the one painted there before.
    fn picture(&mut self, k: usize, s: &Settings) -> Option<&TextureHandle> {
        if self.shown.len() <= k {
            self.shown.resize(k + 1, None);
        }
        if self.preview(s).is_some() {
            self.shown[k] = Some(s.clone());
            return self.preview(s);
        }
        let old = self.shown[k].clone()?;
        self.preview(&old)
    }
    fn detail_stale(&self, s: &Settings) -> bool {
        self.stale_details.contains(s)
    }
    pub fn preview(&self, s: &Settings) -> Option<&TextureHandle> {
        self.previews.iter().find(|(p, _)| p == s).map(|(_, t)| t)
    }
    /// Request a preview unless it is there or on its way. `true`: send the job.
    fn want(&mut self, s: &Settings, now: f64) -> bool {
        if (self.preview(s).is_some() && !self.stale_previews.contains(s)) || self.requested.iter().any(|(p, _)| p == s) {
            return false;
        }
        self.requested.push((s.clone(), now));
        true
    }
    /// Finished preview (from `JobResult::HoverRendered`); only taken if it was requested here.
    pub fn accept_preview(&mut self, ctx: &egui::Context, settings: &Settings, image: egui::ColorImage) -> bool {
        let Some(pos) = self.requested.iter().position(|(p, _)| p == settings) else { return false };
        let (_, at) = self.requested.remove(pos);
        self.stale_previews.retain(|p| p != settings);
        let ms = (ctx.input(|i| i.time) - at) * 1000.0;
        crate::log(&format!("compare pane: preview {} × {} in {ms:.0} ms", image.size[0], image.size[1]));
        self.last_ms = Some((settings.clone(), ms, false));
        match self.previews.iter_mut().find(|(p, _)| p == settings) {
            Some((_, t)) => t.set(image, TextureOptions::LINEAR),
            None => self.previews.push((settings.clone(), ctx.load_texture("compare-pane", image, TextureOptions::LINEAR))),
        }
        true
    }
    /// The preview for these settings will not come (`JobResult::HoverSuperseded`): allow a new request.
    pub fn forget_request(&mut self, settings: &Settings) {
        self.requested.retain(|(p, _)| p != settings);
    }
    /// Is a crop for these settings on its way (`JobResult::RegionRendered` belongs here)?
    pub fn wants_detail(&self, settings: &Settings) -> bool {
        self.detail_requested.as_ref().is_some_and(|(s, _)| s == settings)
    }
    pub fn accept_detail(&mut self, ctx: &egui::Context, settings: Settings, region: [usize; 4], factor: usize, image: egui::ColorImage) {
        let at = self.detail_requested.take().map_or(0.0, |(_, t)| t);
        // a crop of the other binning size: drawn and checked in pixels of the image on screen
        let [sx, sy] = std::mem::replace(&mut self.detail_scale, [1.0, 1.0]);
        let [sx, sy] = [sx.max(1e-6), sy.max(1e-6)];
        let src = [region[0] as f32 / sx, region[1] as f32 / sy, region[2] as f32 / sx, region[3] as f32 / sy];
        let (x0, y0) = ((src[0]).ceil() as usize, (src[1]).ceil() as usize);
        let (x1, y1) = ((src[0] + src[2]).floor() as usize, (src[1] + src[3]).floor() as usize);
        let shown = [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)];
        let ms = (ctx.input(|i| i.time) - at) * 1000.0;
        crate::log(&format!("compare pane: crop {} × {} in {ms:.0} ms ({:.1} MB of pane crops)", image.size[0], image.size[1], (self.bytes() + image.size[0] * image.size[1] * 4) as f64 / 1e6));
        self.last_ms = Some((settings.clone(), ms, true));
        let tex = ctx.load_texture("compare-pane-detail", image, TextureOptions::NEAREST);
        self.details.retain(|(s, ..)| *s != settings);
        self.stale_details.retain(|p| *p != settings);
        self.details.push((settings, shown, factor, tex, src));
    }
    /// The detail request failed: allow a new one.
    pub fn detail_failed(&mut self) {
        self.detail_requested = None;
    }
    /// Drop textures no pane needs any more.
    fn keep(&mut self, needed: &[Settings]) {
        self.previews.retain(|(s, _)| needed.contains(s));
        self.details.retain(|(s, ..)| needed.contains(s));
    }
    /// Texture bytes of the crops.
    pub fn bytes(&self) -> usize {
        self.details.iter().map(|(_, _, _, t, _)| t.size()[0] * t.size()[1] * 4).sum::<usize>()
    }
}

/// Columns and rows for `n` panes: side by side, or the arrangement with the largest image
/// (portrait images side by side, landscape images one above the other; four panes 2 × 2).
pub fn grid(n: usize, area: Vec2, image: Vec2, side_by_side: bool, gap: f32) -> (usize, usize) {
    let n = n.max(1);
    let options: Vec<(usize, usize)> = if side_by_side || n == 1 {
        vec![(n, 1)]
    } else if n == 4 {
        vec![(2, 2)]
    } else {
        vec![(n, 1), (1, n)]
    };
    let fit = |(c, r): (usize, usize)| {
        let w = (area.x - gap * (c as f32 - 1.0)) / c as f32;
        let h = (area.y - gap * (r as f32 - 1.0)) / r as f32;
        (w / image.x.max(1.0)).min(h / image.y.max(1.0))
    };
    let mut best = options[0];
    for o in options.into_iter().skip(1) {
        if fit(o) > fit(best) {
            best = o;
        }
    }
    best
}

/// Rectangles of the panes (row by row).
pub fn pane_rects(n: usize, area: Rect, cols: usize, rows: usize, gap: f32) -> Vec<Rect> {
    let cell = Vec2::new((area.width() - gap * (cols as f32 - 1.0)) / cols as f32, (area.height() - gap * (rows as f32 - 1.0)) / rows as f32);
    (0..n).map(|k| Rect::from_min_size(area.min + Vec2::new((k % cols) as f32 * (cell.x + gap), (k / cols) as f32 * (cell.y + gap)), cell)).collect()
}

/// Name tag at the top left of a pane; with `menu` a small arrow (the tag opens the list of sources).
pub(crate) fn tag(painter: &egui::Painter, at: Pos2, text: &str, menu: bool, hot: bool, max_width: f32) -> Rect {
    let t = if menu { format!("{}  ▾", text.to_uppercase()) } else { text.to_uppercase() };
    // one line, shortened with "…" if the pane is narrower than the name
    let mut job = egui::text::LayoutJob::simple(t, egui::FontId::monospace(11.0), crate::lr::TEXT_STRONG(), (max_width - 14.0).max(40.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let g = painter.layout_job(job);
    let pad = Vec2::new(7.0, 4.0);
    let r = Rect::from_min_size(at, g.size() + pad * 2.0);
    painter.rect_filled(r, 0.0, Color32::from_black_alpha(if hot { 220 } else { 170 }));
    if menu {
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::overlay(if hot { 170 } else { 80 })), egui::StrokeKind::Inside);
    }
    painter.galley(r.min + pad, g, crate::lr::TEXT_STRONG());
    r
}

/// Number mark of a look tile in the comparison (top left of the tile image).
pub fn pick_mark(painter: &egui::Painter, corner: Pos2, n: usize) {
    let c = corner + Vec2::new(11.0, 11.0);
    painter.circle_filled(c, 9.0, crate::lr::gray(0xf0));
    painter.circle_stroke(c, 9.5, egui::Stroke::new(1.0, Color32::from_black_alpha(160)));
    painter.text(c, egui::Align2::CENTER_CENTER, n.to_string(), lr::semibold(11.0), Color32::BLACK);
}

/// Which comparison is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// left side and "Current" side by side (Y)
    BeforeAfter,
    /// only the left side (\)
    Before,
    /// current state and the picked looks
    Looks,
    /// pipeline comparison in the sensor tray: graineo | Apple, or Apple alone (apple_raw.rs)
    Pipeline,
}

impl App {
    /// Kind of comparison to draw instead of the single image (none while cropping).
    pub(crate) fn compare_kind(&self) -> Option<Kind> {
        if self.crop_mode.is_some() || self.current.is_none() {
            None
        } else if self.apple_showing() {
            Some(Kind::Pipeline)
        } else if self.look_compare.showing() {
            Some(Kind::Looks)
        } else if self.compare {
            Some(Kind::BeforeAfter)
        } else if self.show_before {
            Some(Kind::Before)
        } else {
            None
        }
    }

    /// Start "Unedited | Current" or the single "Unedited" view: ends the look comparison.
    pub(crate) fn end_look_compare(&mut self) {
        self.look_compare.end();
    }

    /// Switch on the look comparison: ends "Unedited | Current", "Unedited" and the camera JPEG view.
    pub(crate) fn start_look_compare(&mut self) {
        self.look_compare.on = true;
        self.compare = false;
        self.show_before = false;
        self.show_camera_jpeg = false;
        self.before.menu_open = false;
    }

    /// Add a look to the comparison or take it out (click with the switch on, or ⌘-click).
    pub(crate) fn toggle_compare_pick(&mut self, pick: Pick) {
        if !self.look_compare.on {
            self.start_look_compare();
        }
        if !self.look_compare.toggle(pick) {
            self.status = crate::tr("Höchstens drei Looks im Vergleich – erst einen herausnehmen").to_string();
        }
    }

    fn snap_key(&self, index: usize) -> SnapKey {
        (self.entries[index].path.clone(), self.entries[index].copy.clone())
    }

    /// Hold the current state as the left side ("Jetzigen Stand festhalten"): from now on the right side changes, the
    /// left stays. In memory only (per photo and version, until the program ends); nothing goes to the library or XMP.
    pub(crate) fn take_snapshot(&mut self) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let (key, s) = (self.snap_key(index), self.entries[index].settings.clone());
        self.before.snapshots.set(key, s, now_secs());
        self.before.source = BeforeSource::Snapshot;
        self.before.menu_open = false;
        // the key works without a comparison on screen, too: show "Snapshot | Current"
        if !matches!(self.compare_kind(), Some(Kind::BeforeAfter | Kind::Before)) {
            self.end_look_compare();
            self.compare = true;
        }
    }

    /// Swap left and right: the left source becomes the state of the sliders (undoable like any change), the state so far
    /// is held as the left side. Not possible for the camera JPEG (no settings behind it).
    pub(crate) fn swap_sides(&mut self) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let PaneImage::Settings(left) = self.before_pane().1.image else {
            self.status = crate::tr("Tauschen nicht möglich: die Kamera-JPEG lässt sich nicht als Stand der Regler übernehmen").to_string();
            return;
        };
        let (key, now) = (self.snap_key(index), self.entries[index].settings.clone());
        if left == now {
            return;
        }
        self.before.snapshots.set(key, now, now_secs());
        self.before.source = BeforeSource::Snapshot;
        self.entries[index].settings = left;
    }

    /// Lightroom start values of the current image (Upright resolved as when opening).
    fn lr_start_settings(&mut self) -> Option<Settings> {
        let c = self.current.as_ref()?;
        if let Some((i, s)) = &self.before.lr_cache {
            if *i == c.index {
                return Some(s.clone());
            }
        }
        let copy = self.entries.get(c.index).and_then(|e| e.copy.clone());
        let s = crate::resolve_lr_start(&c.loaded, copy.as_deref())?;
        self.before.lr_cache = Some((c.index, s.clone()));
        Some(s)
    }

    /// Choices for the left side of the current image: (source, label). Only what exists for this image.
    pub(crate) fn before_options(&mut self) -> Vec<(BeforeSource, String)> {
        let Some(c) = self.current.as_ref() else { return Vec::new() };
        let (index, has_lr, has_jpeg) = (c.index, c.loaded.lr_start.is_some(), c.camera_jpeg.is_some());
        let mut v = vec![(BeforeSource::Unedited, crate::tr("Standardentwicklung").to_string())];
        if has_lr {
            v.push((BeforeSource::LightroomStart, crate::tr("Lightroom-Start").to_string()));
        }
        if has_jpeg {
            v.push((BeforeSource::CameraJpeg, crate::tr("Kamera-JPEG").to_string()));
        }
        if let Some((_, at)) = self.before.snapshots.get(&self.snap_key(index)) {
            v.push((BeforeSource::Snapshot, crate::trf!("Momentaufnahme · {a}", a = clock(at))));
        }
        if let Some((_, label)) = self.history.compare_for(&self.history_key(index)) {
            v.push((BeforeSource::HistoryStep, label));
        }
        // Quattro at high ISO: the same photo with the other binning
        if let Some((_, label)) = crate::binning_ui::alt_settings(&c.loaded, &self.entries[index].settings) {
            v.push((BeforeSource::Binning, label));
        }
        let file = self.entries[index].file;
        let others: Vec<usize> = (0..self.entries.len()).filter(|&i| i != index && self.entries[i].file == file).collect();
        for i in others {
            self.ensure_entry(i);
            let copy = self.entries[i].copy.clone();
            let name = copy.clone().unwrap_or_else(|| crate::tr("Original").to_string());
            v.push((BeforeSource::Version(copy), crate::trf!("Version · {a}", a = name)));
        }
        v
    }

    /// The left side actually shown for the current image (falls back to "Unedited" if the choice does not exist here).
    pub(crate) fn before_pane(&mut self) -> (BeforeSource, Pane) {
        let options = self.before_options();
        let source = if options.iter().any(|(s, _)| *s == self.before.source) { self.before.source.clone() } else { BeforeSource::Unedited };
        let label = options.iter().find(|(s, _)| *s == source).map(|(_, l)| l.clone()).unwrap_or_default();
        let image = match &source {
            BeforeSource::Unedited => PaneImage::Settings(Settings::default()),
            BeforeSource::LightroomStart => self.lr_start_settings().map_or(PaneImage::Settings(Settings::default()), PaneImage::Settings),
            BeforeSource::CameraJpeg => PaneImage::CameraJpeg,
            BeforeSource::Snapshot => {
                let key = self.current.as_ref().map(|c| self.snap_key(c.index));
                key.and_then(|k| self.before.snapshots.get(&k).map(|(s, _)| s.clone())).map_or(PaneImage::Settings(Settings::default()), PaneImage::Settings)
            }
            BeforeSource::HistoryStep => {
                let key = self.current.as_ref().map(|c| self.history_key(c.index));
                key.and_then(|k| self.history.compare_for(&k)).map_or(PaneImage::Settings(Settings::default()), |(s, _)| PaneImage::Settings(s))
            }
            BeforeSource::Binning => {
                let alt = self.current.as_ref().and_then(|c| crate::binning_ui::alt_settings(&c.loaded, &self.entries[c.index].settings));
                alt.map_or(PaneImage::Settings(Settings::default()), |(s, _)| PaneImage::Settings(s))
            }
            BeforeSource::Version(copy) => {
                let index = self.current.as_ref().map_or(0, |c| c.index);
                let file = self.entries[index].file;
                self.entries.iter().enumerate().find(|(i, e)| *i != index && e.file == file && e.copy == *copy).map_or(PaneImage::Settings(Settings::default()), |(_, e)| PaneImage::Settings(e.settings.clone()))
            }
        };
        (source, Pane { label, image })
    }

    /// Panes of the current comparison.
    fn compare_panes(&mut self, kind: Kind) -> Vec<Pane> {
        let current = Pane { label: crate::tr("Aktuell").to_string(), image: PaneImage::Current };
        match kind {
            Kind::BeforeAfter => vec![self.before_pane().1, current],
            Kind::Before => vec![self.before_pane().1],
            Kind::Looks => {
                let Some(c) = self.current.as_ref() else { return Vec::new() };
                let s = self.entries[c.index].settings.clone();
                let name = crate::film_look::current_look_name(&s, c.loaded.previews.foreign.as_deref(), &c.loaded.camera_mode);
                let mut v = vec![Pane { label: crate::trf!("Aktuell · {a}", a = name), image: PaneImage::Current }];
                v.extend(self.look_compare.picks.iter().map(|p| Pane { label: p.name.clone(), image: PaneImage::Settings(with_look(&p.choice, &s)) }));
                v
            }
            Kind::Pipeline => {
                let Some(c) = self.current.as_ref() else { return Vec::new() };
                let apple = Pane { label: self.apple.pane_label(), image: PaneImage::Apple };
                if self.apple.pipeline == crate::apple_raw::Pipeline::Apple {
                    return vec![apple];
                }
                let g = crate::apple_raw::graineo_settings(&self.entries[c.index].settings, self.apple.steps);
                // developed by the Apple engine: the left pane is Apple's decoding through graineo's chain, say so
                let dev = self.current.as_ref().and_then(|c| c.loaded.previews.foreign.as_ref()).and_then(|d| d.apple.as_ref()).map(|a| raw::apple::version_label(&a.used));
                let label = match dev {
                    Some(v) => format!("{} · {}", crate::trf!("Apple RAW {a}", a = v), self.apple.graineo_label()),
                    None => self.apple.graineo_label(),
                };
                vec![Pane { label, image: PaneImage::Settings(g) }, apple]
            }
        }
    }

    /// Test hooks (only with `GRAINEO_DATA_DIR`): `GRAINEO_BEFORE_TEST="source=lr|jpeg|unedited|original|binning|method|snapshot|<copy>;menu=1;snap=1;exposure=0.8;view=compare|before;copy=1"`
    /// selects the left side, opens its list, starts the view or makes a virtual copy.
    fn compare_test_hooks(&mut self) {
        if self.before.test_done || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        self.before.test_done = true;
        let Ok(spec) = crate::env::var("GRAINEO_BEFORE_TEST") else { return };
        for part in spec.split(';') {
            match part.split_once('=') {
                Some(("source", v)) => {
                    self.before.source = match v {
                        "lr" => BeforeSource::LightroomStart,
                        "jpeg" => BeforeSource::CameraJpeg,
                        "unedited" => BeforeSource::Unedited,
                        "original" => BeforeSource::Version(None),
                        "binning" => BeforeSource::Binning,
                        "snapshot" => BeforeSource::Snapshot,
                        other => BeforeSource::Version(Some(other.to_string())),
                    }
                }
                Some(("menu", "1")) => self.before.menu_open = true,
                // hold the current state, then change the exposure (test: the left side must stay)
                Some(("snap", "1")) => self.take_snapshot(),
                Some(("exposure", v)) => {
                    if let (Some(c), Ok(v)) = (self.current.as_ref().map(|c| c.index), v.parse::<f64>()) {
                        self.entries[c].settings.exposure = v;
                    }
                }
                // a virtual copy of the photo (for the "Version" entries; the copy is shown afterwards)
                Some(("copy", "1")) => self.create_virtual_copy(None),
                Some(("view", "compare")) => {
                    self.end_look_compare();
                    self.compare = true;
                }
                Some(("view", "before")) => {
                    self.end_look_compare();
                    self.show_before = true;
                }
                _ => {}
            }
        }
    }

    /// Draw the comparison in the image area. `false`: no comparison, draw the single image.
    pub(crate) fn compare_view(&mut self, ui: &mut egui::Ui, avail: Rect, resp: &egui::Response, painter: &egui::Painter, generation: usize, click_free: bool) -> bool {
        self.compare_test_hooks();
        // the look comparison ends with its panel
        if self.look_compare.on && ui.ctx().cumulative_frame_nr() > self.look_compare.panel_frame + 2 {
            self.look_compare.end();
        }
        let Some(kind) = self.compare_kind() else {
            self.before.menu_open = false;
            self.before.binning_alt.clear();
            if !self.look_compare.on {
                self.panes.keep(&[]);
            }
            return false;
        };
        let panes = self.compare_panes(kind);
        if panes.is_empty() {
            return false;
        }
        let now = ui.input(|i| i.time);
        let ppp = ui.ctx().pixels_per_point();
        let Some(c) = self.current.as_ref() else { return false };
        let index = c.index;
        let settings = self.entries[index].settings.clone();
        let [fw, fh] = c.loaded.full_size;
        let frame = Frame::from_settings(&settings, fw, fh, c.loaded.cal.orientation);
        let full_src = [0.0, 0.0, fw as f32, fh as f32];
        let s = Vec2::new(frame.w, frame.h);
        let gap = 8.0;
        let (cols, rows) = grid(panes.len(), avail.size(), s, matches!(kind, Kind::BeforeAfter | Kind::Pipeline), gap);
        let rects = pane_rects(panes.len(), avail, cols, rows, gap);
        let cell = rects[0].size();
        let pane_fit = (cell.x / s.x).min(cell.y / s.y);
        let one = 1.0 / (pane_fit * ppp);
        self.one_to_one = one;
        self.fit_size = s * pane_fit;
        self.view_size = cell;
        let clicks = click_free && !self.before.menu_open;
        // zoom: gestures and steps as in the single view; zoom is a multiple of the fitted pane
        let zoom_frame = (generation, self.fit_size, cell, true);
        if let Some((z, p)) = self.navigation.zoom.step(now, zoom_frame, crate::zoom::direct_gesture(ui.ctx(), clicks)) {
            self.zoom = z;
            self.pan = p;
            self.view_changed_at = now;
            crate::zoom::log_lines(ui.ctx(), now, (z, p));
        }
        if let Some((z, p)) = crate::zoom_test_step(self.zoom, self.pan, one) {
            (self.zoom, self.pan) = (z, p);
            self.view_changed_at = now;
        }
        let max_zoom = one.max(1.0) * crate::zoom::maximum_factor(self.tab == crate::Tab::Sensor, self.darkroom.active);
        let mut center_u = Vec2::new(0.5, 0.5) - self.pan / (self.fit_size * self.zoom).max(Vec2::splat(1.0));
        let mut pan_swipe = Vec2::ZERO;
        if resp.hovered() && !self.before.menu_open {
            let (pinch, swipe, alt) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
            if let Some(t) = crate::zoom::scroll_source(ui.ctx()) {
                self.scroll_trackpad = t;
            }
            // as in Develop: a trackpad swipe pans a zoomed view and does nothing on a fitted one; the mouse wheel
            // (and ⌥ + swipe) zooms
            let scroll = if self.scroll_trackpad && !alt {
                if self.zoom > 1.01 {
                    pan_swipe = swipe;
                }
                pinch
            } else {
                pinch * (1.0 + swipe.y * 0.002)
            };
            if scroll != 1.0 {
                self.zoom = (self.zoom * scroll).clamp(1.0, max_zoom);
                self.view_changed_at = now;
            }
        }
        let pane_size = s * pane_fit * self.zoom;
        if pan_swipe != Vec2::ZERO {
            center_u -= pan_swipe / pane_size;
            self.view_changed_at = now;
        }
        if resp.dragged() && clicks {
            center_u -= resp.drag_delta() / pane_size;
            self.view_changed_at = now;
        }
        let image_rect = |pane: Rect, center_u: Vec2, size: Vec2| Rect::from_center_size(pane.center() + (Vec2::new(0.5, 0.5) - center_u) * size, size);
        let pointer_pane = resp.interact_pointer_pos().or(resp.hover_pos()).and_then(|p| rects.iter().position(|r| image_rect(*r, center_u, pane_size).intersect(*r).contains(p)));
        let mut adopt = None;
        if kind == Kind::Looks {
            // a click (without dragging) on a field adopts its look
            self.navigation.click.cancel();
            if clicks && resp.clicked_by(egui::PointerButton::Primary) {
                adopt = pointer_pane;
            }
            if self.zoom > 1.01 && clicks && resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if self.zoom > 1.01 && clicks && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        } else {
            let hit = pointer_pane.map_or(Rect::NOTHING, |k| image_rect(rects[k], center_u, pane_size).intersect(rects[k]));
            match self.navigation.click.answer(resp, generation, clicks, hit) {
                Some(crate::media_library::Geste::Raster) => {
                    self.to_grid();
                    return true;
                }
                Some(crate::media_library::Geste::Zoom(p)) => {
                    let (z, target_center) = if self.zoom > 1.01 {
                        (1.0, Vec2::new(0.5, 0.5))
                    } else {
                        let pane = rects.iter().copied().find(|r| r.contains(p)).unwrap_or(rects[0]);
                        let uv = center_u + (p - pane.center()) / pane_size;
                        let z = one.max(1.0);
                        (z, uv - (p - pane.center()) / (s * pane_fit * z))
                    };
                    let target_pan = (Vec2::new(0.5, 0.5) - target_center) * self.fit_size * z;
                    (self.zoom, self.pan) = self.navigation.zoom.start(now, (self.zoom, self.pan), (z, target_pan), zoom_frame, crate::zoom::reduced_motion());
                    center_u = Vec2::new(0.5, 0.5) - self.pan / (self.fit_size * self.zoom).max(Vec2::splat(1.0));
                    ui.ctx().request_repaint();
                    self.view_changed_at = now;
                }
                None => {}
            }
            if self.zoom > 1.01 && clicks && resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if self.zoom > 1.01 && clicks && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        // fitted stays fitted, as in the single view
        if crate::zoom::settle_fit(&mut self.zoom, &mut self.pan, crate::zoom::view_busy(ui.ctx(), &self.navigation.zoom)) {
            center_u = Vec2::new(0.5, 0.5);
            self.view_changed_at = now;
        }
        let pane_size = s * pane_fit * self.zoom;
        // no empty border next to the image, as in the single view
        for k in 0..2 {
            let (sz, vis) = (pane_size[k], cell[k]);
            center_u[k] = if sz <= vis + 1.0 { 0.5 } else { center_u[k].clamp(0.5 * vis / sz, 1.0 - 0.5 * vis / sz) };
        }
        self.pan = (Vec2::new(0.5, 0.5) - center_u) * self.fit_size * self.zoom;

        // full resolution: visible part of the image (the same in every pane)
        let Some(c) = self.current.as_ref() else { return true };
        let px_per_img = pane_size.x * ppp / fw as f32;
        let preview_res = c.loaded.previews.developed.lin.width as f32 / fw as f32;
        let needs_detail = !settings.view.is_stage() && self.zoom > 1.05 && px_per_img > preview_res * 1.1;
        let r0 = image_rect(rects[0], center_u, pane_size);
        let region = {
            let vis = r0.intersect(rects[0]);
            let to_src = |p: Pos2| {
                let u = (p - r0.min) / r0.size();
                frame.to_src([u.x, u.y])
            };
            let pts = [vis.left_top(), vis.right_top(), vis.right_bottom(), vis.left_bottom()].map(to_src);
            let (x0, x1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            darkroom_loupe::source_region([x0.max(0.0).floor(), y0.max(0.0).floor(), x1.min(fw as f32).ceil(), y1.min(fh as f32).ceil()], [fw, fh])
        };
        let gave_up = self.refine_retries >= 2;
        let detail_ready = |s: &Settings, session: &Option<std::sync::Arc<std::sync::Mutex<develop::Session>>>| match session {
            None => true,
            Some(sess) => !develop::Session::uses_spp(s) || gave_up || sess.try_lock().is_ok_and(|x| x.has_spp_for(s)),
        };
        // a pane with another binning (Quattro at high ISO) needs a second session of the other size
        let mut uses_alt = false;
        let path = self.entries[index].path.clone();
        let settled = now - self.view_changed_at > 0.25 && !self.detail_pending && now >= self.detail_blocked_until;
        let mut request: Option<(Settings, usize, [usize; 4], bool, Option<crate::binning_ui::AltView>)> = None;
        let mut jobs: Vec<Job> = Vec::new();
        let mut loading = false;
        let mut needed: Vec<Settings> = Vec::new();
        let mut tag_rects: Vec<Rect> = Vec::new();
        let apple_params = if kind == Kind::Pipeline { self.apple_params() } else { None };
        let apple_camera = if kind == Kind::Pipeline { self.apple_camera() } else { String::new() };
        let Some(c) = self.current.as_ref() else { return true };
        let hovered_pane = if kind == Kind::Looks && clicks && !resp.dragged() { resp.hover_pos().and_then(|p| rects.iter().position(|r| image_rect(*r, center_u, pane_size).intersect(*r).contains(p))) } else { None };
        if kind == Kind::Looks && hovered_pane.is_some() && self.zoom <= 1.01 {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        }
        for (k, pane) in panes.iter().enumerate() {
            let rect = rects[k];
            let r = image_rect(rect, center_u, pane_size);
            let clip = painter.with_clip_rect(rect);
            let image = match &pane.image {
                PaneImage::Settings(ps) if *ps == settings => PaneImage::Current,
                other => other.clone(),
            };
            match &image {
                PaneImage::Current => {
                    if let Some(t) = &c.texture {
                        // the version with preserved gamut / in HDR of the render shown, if there is one
                        let rendered = self.rendered_for.as_ref().filter(|(i, _)| *i == c.index).map(|(_, s)| s);
                        let id = self.display.preview_id(c.index, rendered).map_or(t.id(), |(id, _)| id);
                        crate::paint_src_quad(&clip, id, full_src, &frame, r, Color32::WHITE);
                    } else {
                        // the fast preview is withheld (`preview_hold.rs`): the spinner until the exact base is there –
                        // never a picture from the rough data
                        crate::spinner(painter, r.intersect(rect).center(), 6.5, now, 1.0);
                        loading = true;
                    }
                    if needs_detail {
                        let factor = darkroom_loupe::factor(px_per_img, 1.0, self.darkroom.active && self.darkroom.grain_check && settings.film.is_active(), develop::local::active(&settings));
                        if let Some((dtex, _, ds)) = c.detail_for(factor) {
                            if *ds == settings {
                                dtex.paint(&clip, &frame, r, Color32::WHITE, px_per_img);
                            }
                        }
                        let stale = c.detail_for(factor).is_none_or(|(_, dr, ds)| *ds != settings || !crate::contains(*dr, region));
                        loading |= stale;
                        if stale && settled && request.is_none() && detail_ready(&settings, &c.loaded.session) {
                            request = Some((settings.clone(), factor, region, true, None));
                        }
                    }
                }
                PaneImage::Settings(ps) => {
                    needed.push(ps.clone());
                    // other binning: previews and crops from the second session; until it is open the pane waits
                    let alt = if !needs_own_session(&c.loaded, ps, &settings, &path) {
                        Some(None)
                    } else {
                        uses_alt = true;
                        self.before.binning_alt.get(ui.ctx(), &path, &c.loaded.cal, ps).and_then(|r| r.ok()).map(Some)
                    };
                    let session = match &alt {
                        Some(Some(a)) => Some(a.session.clone()),
                        _ => c.loaded.session.clone(),
                    };
                    if matches!(alt, Some(Some(_))) {
                        self.panes.revalidate(ps);
                    }
                    if let Some(a) = &alt {
                        let (previews, cal) = a.as_ref().map_or((&c.loaded.previews, &c.loaded.cal), |a| (&a.previews, &a.cal));
                        // no pane from a fast preview too rough to be shown: asked again once the exact base is there
                        if !gave_up && crate::preview_hold::waits(previews, cal, ps) {
                            loading = true;
                        } else if self.panes.want(ps, now) {
                            let (previews, cal) = (previews.clone(), cal.clone());
                            jobs.push(Job::HoverRender { index, previews, cal, settings: ps.clone(), supersede: false });
                        }
                    }
                    let have_own = self.panes.preview(ps).is_some();
                    match self.panes.picture(k, ps).map(|t| t.id()) {
                        Some(id) => {
                            crate::paint_src_quad(&clip, id, full_src, &frame, r, Color32::WHITE);
                            if !have_own {
                                loading = true;
                            }
                        }
                        None => {
                            // until the preview is there: the current image dimmed, with a spinner
                            if let Some(t) = c.texture.as_ref() {
                                crate::paint_src_quad(&clip, t.id(), full_src, &frame, r, Color32::from_gray(70));
                            }
                            crate::spinner(painter, r.intersect(rect).center(), 6.5, now, 1.0);
                            loading = true;
                        }
                    }
                    if needs_detail && self.panes.preview(ps).is_some() {
                        let factor = darkroom_loupe::factor(px_per_img, 1.0, self.darkroom.active && self.darkroom.grain_check && ps.film.is_active(), develop::local::active(ps));
                        let have = self.panes.details.iter().find(|(d, ..)| d == ps);
                        if let Some((_, _, _, t, src)) = have {
                            crate::paint_src_quad(&clip, t.id(), *src, &frame, r, Color32::WHITE);
                        }
                        let stale = have.is_none_or(|(_, dr, f, _, _)| *f != factor || !crate::contains(*dr, region)) || self.panes.detail_stale(ps);
                        let ready = alt.is_some() && detail_ready(ps, &session);
                        loading |= stale && ready;
                        if stale && settled && request.is_none() && ready {
                            request = Some((ps.clone(), factor, region, false, alt.flatten()));
                        }
                    }
                }
                PaneImage::CameraJpeg => match &c.camera_jpeg {
                    Some(t) => crate::paint_src_quad(&clip, t.id(), full_src, &frame, r, Color32::WHITE),
                    None => {
                        crate::spinner(painter, r.intersect(rect).center(), 6.5, now, 1.0);
                        loading = true;
                    }
                },
                PaneImage::Apple => match &apple_params {
                    Some(p) => loading |= self.apple.paint_pane(ui.ctx(), painter, r, rect, &frame, [fw, fh], p, needs_detail.then_some(region), settled, now, &apple_camera),
                    None => crate::spinner(painter, r.intersect(rect).center(), 6.5, now, 1.0),
                },
            }
            if kind == Kind::Looks && hovered_pane == Some(k) {
                painter.rect_stroke(r.intersect(rect), 0.0, egui::Stroke::new(2.0, crate::lr::overlay(200)), egui::StrokeKind::Inside);
            }
            let menu = matches!(kind, Kind::BeforeAfter | Kind::Before) && k == 0;
            let at = r.intersect(rect).left_top() + Vec2::new(10.0, 10.0);
            let hot = menu && (self.before.menu_open || ui.rect_contains_pointer(Rect::from_min_size(at, Vec2::new(10.0, 10.0))));
            let tr = tag(painter, at, &pane.label, menu, hot, rect.right() - at.x - 10.0);
            tag_rects.push(tr);
        }
        // request the camera JPEG once per image (it may not exist)
        // (always with "Unedited | Current": the list offers it only once it exists, a remembered choice applies then)
        let jpeg_needed = matches!(kind, Kind::BeforeAfter | Kind::Before);
        if jpeg_needed && c.camera_jpeg.is_none() && self.before.jpeg_requested != Some(index) {
            self.before.jpeg_requested = Some(index);
            jobs.push(Job::CameraJpeg { index, path: self.entries[index].path.clone() });
        }
        // at most one crop at a time (the same queue as the single view)
        if let Some((rs, factor, region, current, alt)) = request {
            let req = if factor == 1 { crate::region_with_margin(region, 0.25, [fw, fh]) } else { region };
            // other binning size: the crop in pixels of that image (drawn back in pixels of the image on screen)
            let (req, session) = match &alt {
                Some(a) => {
                    let s = [a.full_size[0] as f32 / fw as f32, a.full_size[1] as f32 / fh as f32];
                    self.panes.detail_scale = s;
                    (crate::binning_ui::scale_region(req, s, a.full_size), Some(a.session.clone()))
                }
                None => {
                    self.panes.detail_scale = [1.0, 1.0];
                    (req, c.loaded.session.clone())
                }
            };
            // memory: crops of the other panes stay below 48 MP each (otherwise they keep the preview)
            if current || req[2] * req[3] <= 48_000_000 {
                if !current {
                    self.panes.detail_requested = Some((rs.clone(), now));
                }
                self.detail_pending = true;
                jobs.push(Job::RenderRegion { factor, index, session, foreign: c.loaded.previews.foreign.clone(), settings: rs, region: req, generation: self.open_generation.load(std::sync::atomic::Ordering::SeqCst), current: self.open_generation.clone(), edit: crate::jobs::EditStamp::none() });
            }
        }
        for j in jobs {
            self.worker.send(j);
        }
        if !uses_alt {
            self.before.binning_alt.clear();
        }
        needed.extend(self.panes.shown.iter().flatten().cloned());
        self.panes.keep(&needed);
        if loading || self.detail_pending {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        // the name tag of the left side opens the list of sources
        if matches!(kind, Kind::BeforeAfter | Kind::Before) {
            let tag_resp = ui.interact(tag_rects[0], egui::Id::new("compare-before-tag"), egui::Sense::click());
            if tag_resp.clicked() {
                self.before.menu_open = !self.before.menu_open;
            }
            // next to it: one click holds the current state as the left side
            let plus_at = tag_rects[0].right_top() + Vec2::new(4.0, 0.0);
            let plus_rect = tag(painter, plus_at, "+", false, false, 40.0);
            let plus_resp = ui.interact(plus_rect, egui::Id::new("compare-snapshot-button"), egui::Sense::click());
            if plus_resp.hovered() {
                tag(painter, plus_at, "+", false, true, 40.0);
            }
            lr::hover_tip(ui, &plus_resp, &lr::with_shortcut("Jetzigen Stand festhalten", "snapshot"));
            if plus_resp.clicked() {
                self.take_snapshot();
            }
            // and: swap left and right (not for the camera JPEG: it cannot become the state of the sliders)
            let can_swap = matches!(self.before_pane().1.image, PaneImage::Settings(_));
            let swap_at = plus_rect.right_top() + Vec2::new(4.0, 0.0);
            let swap_rect = tag(painter, swap_at, "↔", false, false, 40.0);
            let swap_resp = ui.interact(swap_rect, egui::Id::new("compare-swap-button"), egui::Sense::click());
            if !can_swap {
                painter.rect_filled(swap_rect, 0.0, Color32::from_black_alpha(120));
            } else if swap_resp.hovered() {
                tag(painter, swap_at, "↔", false, true, 40.0);
            }
            let swap_tip = if can_swap { lr::with_shortcut("Links und Rechts tauschen", "swap_sides") } else { crate::tr("Tauschen nicht möglich: die Kamera-JPEG lässt sich nicht als Stand der Regler übernehmen").to_string() };
            lr::hover_tip(ui, &swap_resp, &swap_tip);
            if swap_resp.clicked() && can_swap {
                self.swap_sides();
            }
            self.before_menu(ui.ctx(), tag_rects[0]);
        }
        // hint at the bottom right
        let hint = match kind {
            Kind::BeforeAfter => crate::tr("Y beendet den Vergleich").to_string(),
            Kind::Before => crate::tr("\\ zum Umschalten").to_string(),
            Kind::Looks => crate::tr("Klick auf ein Feld übernimmt den Look · Esc beendet").to_string(),
            Kind::Pipeline => crate::tr("Nur zum Vergleichen – ändert die Entwicklung nicht").to_string(),
        };
        crate::badge(painter, avail, &hint, None);
        if let Some(k) = adopt.or_else(|| self.look_compare_test_end(kind)) {
            self.adopt_pane(k);
        }
        true
    }

    /// Test hook `GRAINEO_LOOK_COMPARE_END="<field>@<s>"` or `"esc@<s>"` (only with `GRAINEO_DATA_DIR`): after `s` seconds
    /// of the look comparison, act as a click on field 1 … 4 or as Esc. `Some(k)`: adopt field `k` (0-based).
    fn look_compare_test_end(&mut self, kind: Kind) -> Option<usize> {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        if kind != Kind::Looks || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return None;
        }
        let spec = crate::env::var("GRAINEO_LOOK_COMPARE_END").ok()?;
        let (what, at) = spec.split_once('@')?;
        if START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() < at.parse::<f64>().ok()? {
            return None;
        }
        if what == "esc" {
            crate::log("look comparison: ended with Esc (test hook)");
            self.end_look_compare();
            return None;
        }
        what.parse::<usize>().ok().and_then(|k| k.checked_sub(1))
    }

    /// Look comparison: adopt the look of field `k` (field 0 is the current state) and end the comparison.
    fn adopt_pane(&mut self, k: usize) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        if let Some(p) = k.checked_sub(1).and_then(|i| self.look_compare.picks.get(i)) {
            let t = with_look(&p.choice, &self.entries[index].settings);
            crate::log(&format!("look comparison: field {} adopted ({})", k + 1, p.name));
            self.entries[index].settings = t;
        } else {
            crate::log("look comparison: current state kept");
        }
        self.look_compare.end();
    }

    /// Small list below the name tag of the left side.
    fn before_menu(&mut self, ctx: &egui::Context, anchor: Rect) {
        if !self.before.menu_open {
            return;
        }
        let options = self.before_options();
        let shown = self.before_pane().0;
        let mut pick = None;
        let mut snap = false;
        let mut swap = false;
        let can_swap = matches!(self.before_pane().1.image, PaneImage::Settings(_));
        let area = egui::Area::new(egui::Id::new("compare-before-menu")).order(egui::Order::Foreground).fixed_pos(anchor.left_bottom() + Vec2::new(0.0, 4.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let w = anchor.width().max(190.0);
                ui.set_min_width(w);
                ui.set_max_width(w);
                for (source, label) in &options {
                    if lr::list_item(ui, label, *source == shown).clicked() {
                        pick = Some(source.clone());
                    }
                }
                ui.separator();
                if lr::list_item(ui, crate::tr("Jetzigen Stand festhalten"), false).clicked() {
                    snap = true;
                }
                if can_swap && lr::list_item(ui, crate::tr("Links und Rechts tauschen"), false).clicked() {
                    swap = true;
                }
            });
        });
        if snap {
            self.take_snapshot();
        } else if swap {
            self.swap_sides();
            self.before.menu_open = false;
        } else if let Some(p) = pick {
            self.before.source = p;
            self.before.menu_open = false;
        } else if ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !anchor.contains(p))) {
            self.before.menu_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_one_above_the_other_portrait_side_by_side() {
        let area = Vec2::new(1300.0, 1000.0);
        assert_eq!(grid(2, area, Vec2::new(3000.0, 2000.0), false, 8.0), (1, 2));
        assert_eq!(grid(2, area, Vec2::new(2000.0, 3000.0), false, 8.0), (2, 1));
        assert_eq!(grid(3, area, Vec2::new(2000.0, 3000.0), false, 8.0), (3, 1));
        assert_eq!(grid(4, area, Vec2::new(3000.0, 2000.0), false, 8.0), (2, 2));
        // a very wide area: landscape images side by side are larger
        assert_eq!(grid(2, Vec2::new(3000.0, 600.0), Vec2::new(3000.0, 2000.0), false, 8.0), (2, 1));
        // "Unedited | Current" always side by side
        assert_eq!(grid(2, area, Vec2::new(3000.0, 2000.0), true, 8.0), (2, 1));
    }

    #[test]
    fn panes_are_equal_and_inside_the_area() {
        let area = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(1000.0, 600.0));
        let r = pane_rects(4, area, 2, 2, 8.0);
        assert_eq!(r.len(), 4);
        assert!(r.iter().all(|p| area.contains_rect(*p) && (p.width() - r[0].width()).abs() < 0.01));
        assert!((r[3].right() - area.right()).abs() < 0.01 && (r[3].bottom() - area.bottom()).abs() < 0.01);
    }

    #[test]
    fn at_most_three_looks_and_a_second_click_removes() {
        let mut v = LookCompare::default();
        let p = |k: &str| Pick { key: k.into(), name: k.into(), choice: Choice::Film(k.into()) };
        for k in ["a", "b", "c"] {
            assert!(v.toggle(p(k)));
        }
        assert!(!v.toggle(p("d")));
        assert_eq!(v.number("c"), Some(3));
        assert!(v.toggle(p("b")));
        assert_eq!(v.number("c"), Some(2));
        assert!(v.showing());
        v.end();
        assert!(!v.showing() && !v.on);
    }

    #[test]
    fn a_look_only_exchanges_the_film() {
        let mut s = Settings::default();
        s.exposure = 0.7;
        let t = with_look(&Choice::Film("kodak_portra_400".into()), &s);
        assert!(t.film.active && t.film.stock == "kodak_portra_400" && t.exposure == 0.7);
    }

    #[test]
    fn snapshot_stays_while_sliders_move_and_is_per_photo() {
        let mut snaps = Snapshots::default();
        let a: SnapKey = ("/a.X3F".into(), None);
        let b: SnapKey = ("/a.X3F".into(), Some("Kopie 1".into()));
        let mut s = Settings { exposure: 0.3, ..Settings::default() };
        snaps.set(a.clone(), s.clone(), 100);
        s.exposure = 1.5;
        assert_eq!(snaps.get(&a).unwrap().0.exposure, 0.3);
        assert!(snaps.get(&b).is_none());
        // holding again replaces it
        snaps.set(a.clone(), s.clone(), 200);
        assert_eq!((snaps.get(&a).unwrap().0.exposure, snaps.get(&a).unwrap().1), (1.5, 200));
    }

    #[test]
    fn a_new_base_keeps_the_old_picture_until_the_new_one_is_there() {
        let ctx = egui::Context::default();
        let img = || egui::ColorImage::new([4, 4], vec![Color32::WHITE; 16]);
        let (a, b) = (Settings::default(), Settings { exposure: 0.5, ..Settings::default() });
        let mut p = PaneTextures::default();
        for s in [&a, &b] {
            assert!(p.want(s, 0.0));
            assert!(p.accept_preview(&ctx, s, img()));
            assert!(!p.want(s, 0.0), "a fresh picture is not requested again");
        }
        p.invalidate();
        // nothing goes dark: both pictures are still there ...
        assert!(p.preview(&a).is_some() && p.preview(&b).is_some());
        // ... and are requested again; a pane with a session of its own keeps its picture
        p.revalidate(&a);
        assert!(!p.want(&a, 1.0));
        assert!(p.want(&b, 1.0));
        // an answer made from the old base (requested before the change) is dropped
        p.invalidate();
        assert!(!p.accept_preview(&ctx, &b, img()), "late result of the old base");
        // the new answer replaces the picture and makes it fresh
        assert!(p.want(&b, 2.0));
        assert!(p.accept_preview(&ctx, &b, img()));
        assert!(!p.want(&b, 2.0));
    }
}
