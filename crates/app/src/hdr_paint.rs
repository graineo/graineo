//! Tool "Zusammenfügen" of the right rail: everything about the open merged photo in one place.
//!
//! - AUFNAHMEN: the frames as layers – tick = used, star = reference; further photos of the series or stack stand
//!   there unticked, others are dragged in from the filmstrip. Holding the mouse button on a frame shows that single
//!   exposure in the middle (same place, same zoom), the info box says so with a thin gold frame.
//! - BEWEGUNG, AUSGABE, Ausrichten: the big switches; a change merges again (a quick preview first, the full resolution
//!   behind it; the picture stays until the new one is there, `hdr_live.rs`).
//! - GEISTER ENTFERNEN: "Bereich malen" outlines an area (thin outline, no colour blobs; "Verkleinern" takes away);
//!   beside it the crops of this area from every frame, a click takes the area from that frame. Pasted locally
//!   ([`hdr::regions`]), never a new merge. The list works like the masks: eye = compare, ✕ = delete.
//! - "Erkannte Bewegung zeigen" lays the detected motion over the photo (steps back while a slider is dragged).
//!
//! State lives in the egui context (as in `retouch_ui`); it belongs to the merged photo that is open.

use eframe::egui;
use egui::{Color32, Context, Pos2};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::hdr_ui::clock;
use crate::lr;

/// A photo of the series or stack that is not a frame of the merge (yet).
#[derive(Clone)]
pub struct Candidate {
    pub file: i64,
    /// EV from the shooting data, relative to the reference
    pub ev: Option<f64>,
    /// EV from the shooting data (absolute; the order of the list)
    pub exif_ev: Option<f64>,
    pub thumb: Option<egui::TextureHandle>,
}

/// What the tool needs to know about the open merged photo.
#[derive(Clone)]
pub struct Info {
    pub file: i64,
    pub name: String,
    pub sources: Vec<PathBuf>,
    /// library files of the sources of the stored recipe (recipe order)
    pub source_files: Vec<i64>,
    /// library files of the frames listed (`sources`, same order): the recipe's while the lists are in step, those of
    /// the picture shown while a change of the frames is computed. EVs, numbers and the photos offered are looked up
    /// by these – never by the recipe's files against the picture's list (QA 11, M-1: a row too many, EVs shifted)
    pub list_files: Vec<i64>,
    /// the stored settings (older strokes already turned into areas)
    pub settings: hdr::Settings,
    /// EV of each source relative to the reference (measured; empty while not computed)
    pub ev: Vec<f64>,
    pub reference: usize,
    pub base_key: String,
    /// cache key of the result shown
    pub key: String,
    /// exposure, highlights, shadows, whites, blacks of the photo now (the sliders the automatic tone sets)
    pub tone_now: [f32; 5],
    pub warnings: Vec<hdr::Warning>,
    /// the merge is not there yet (the reference stands in)
    pub pending: bool,
    /// the full resolution of the result shown is computed
    pub full_ready: bool,
    /// EV of each source from the shooting data (absolute; the order of the list)
    pub source_exif_ev: Vec<Option<f64>>,
    /// number shown for each source: its place in the list of the series (frames and photos offered, by exposure),
    /// so a frame keeps its number when another is left out or taken in
    pub numbers: Vec<usize>,
    pub candidates: Vec<Candidate>,
    /// the frames listed (the photo shown) are the frames of the stored recipe: only then a tick, star or drop may
    /// change the list – while a change of the frames is being computed the two differ
    pub in_step: bool,
    /// the photo shown is not the result of the stored recipe and nothing computes it (a change was cancelled or
    /// failed)
    pub stale: bool,
    /// something is on its way for this photo: a job runs or is about to, a change is being stored
    pub busy: bool,
    /// the result the photo should show could not be computed: why (nothing is tried again by itself)
    pub failed: Option<String>,
    /// names of the sources that cannot be read now
    pub missing: Vec<String>,
    /// the photo was opened without its reference file (camera data from the cache): the lens correction of the file
    /// and its profile tables are not applied
    pub offline: bool,
    /// cache key of the stored recipe (`None`: no recipe)
    pub stored_key: Option<String>,
    /// when the recipe was created: with the file it names the merged photo (a file id alone may come again)
    pub created: i64,
}

/// A change of the recipe for the app (`merge_update_step`).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// settings to compute and show (hidden areas left out)
    pub view: hdr::Settings,
    /// settings to store (`None`: only a comparison, nothing is stored)
    pub save: Option<hdr::Settings>,
    /// new list of source files (frames taken in or left out)
    pub sources: Option<Vec<i64>>,
}

#[derive(Clone)]
struct State {
    open: bool,
    /// merged photo the fields below belong to: its file and when its recipe was created
    file: i64,
    created: i64,
    settings: hdr::Settings,
    /// "Bereich malen" is on
    tool: bool,
    /// "Verkleinern"
    erase: bool,
    /// brush radius in screen points (stays the same when zooming)
    brush: f32,
    /// stroke being drawn (normalised crop coordinates)
    pending: Vec<[f32; 2]>,
    /// area the brush works on (`None`: the next stroke starts a new one)
    selected: Option<usize>,
    hidden: Vec<bool>,
    show_motion: bool,
    /// frame held down in the list (shown instead of the merge)
    peek: Option<usize>,
    change: Option<Change>,
    /// the fusion strength was moved: stored and computed once the mouse button is up
    strength_moved: bool,
}

impl Default for State {
    fn default() -> Self {
        State { open: false, file: -1, created: 0, settings: hdr::Settings::default(), tool: false, erase: false, brush: 24.0, pending: Vec::new(), selected: None, hidden: Vec::new(), show_motion: false, peek: None, change: None, strength_moved: false }
    }
}

fn key() -> egui::Id {
    egui::Id::new("hdr-paint-state")
}

fn state(ctx: &Context) -> State {
    ctx.data_mut(|d| d.get_temp::<State>(key())).unwrap_or_default()
}

fn save(ctx: &Context, s: State) {
    ctx.data_mut(|d| d.insert_temp(key(), s));
}

/// The tool's panel is open (every frame from the rail).
pub fn set_open(ctx: &Context, open: bool) {
    let mut s = state(ctx);
    if s.open != open {
        s.open = open;
        s.pending.clear();
        s.peek = None;
        save(ctx, s);
    }
}

/// Does the brush take the pointer in the image (then a click or drag does not zoom or pan)?
pub fn active(ctx: &Context) -> bool {
    let s = state(ctx);
    s.open && s.tool
}

/// Take over the recipe of the open merged photo when it is another one than before.
fn sync(ctx: &Context, info: &Info) -> State {
    let mut s = state(ctx);
    // (`created` 0: the recipe could not be read just now – never a reason to drop what the tool holds)
    if s.file == info.file && s.created == 0 {
        s.created = info.created;
    }
    if s.file != info.file || (info.created != 0 && s.created != info.created) {
        s = State { open: s.open, brush: s.brush, show_motion: s.show_motion, file: info.file, created: info.created, settings: info.settings.clone(), hidden: vec![false; info.settings.regions.len()], ..Default::default() };
        // the automatic tone may be newer than the recipe the document was opened with
        if let Some(a) = crate::hdr_auto::known(info.file) {
            s.settings.auto_tone = a;
        }
    }
    s.hidden.resize(s.settings.regions.len(), false);
    s
}

/// The recipe settings the tool holds for `file` (`None` while it holds another photo's).
pub fn settings_of(ctx: &Context, file: i64) -> Option<hdr::Settings> {
    let s = state(ctx);
    (s.file == file).then_some(s.settings)
}

/// The automatic tone set by the app (`hdr_auto`): taken into the tool's recipe, so the next change stores it.
pub fn adopt_auto(ctx: &Context, file: i64, auto: Option<hdr::AutoTone>) {
    let mut s = state(ctx);
    if s.file == file {
        s.settings.auto_tone = auto;
        save(ctx, s);
    }
}

/// Slider "Stärke" under AUSGABE, the same place for both outputs: linear = strength of the automatic tone (0 = off),
/// fusion = how much of the fusion over the reference exposure. Returns whether the value moved.
fn strength_slider(ui: &mut egui::Ui, s: &mut State, info: &Info) -> bool {
    let fusion = s.settings.output == hdr::fusion::Output::Fusion;
    let (mut v, default) = if fusion {
        (s.settings.fusion_strength as f64 * 100.0, hdr::FUSION_STRENGTH_DEFAULT as f64 * 100.0)
    } else {
        (s.settings.auto_tone.map_or(0.0, |a| a.strength as f64 * 100.0), hdr::AUTO_TONE_DEFAULT as f64 * 100.0)
    };
    let top = ui.cursor().top();
    let moved = lr::slider_plain(ui, "Stärke", &mut v, 0.0..=100.0, default, 1.0);
    let rect = egui::Rect::from_min_max(egui::pos2(ui.min_rect().left(), top), egui::pos2(ui.min_rect().right(), ui.cursor().top()));
    let probe = ui.interact(rect, ui.id().with("hdr-strength-tip"), egui::Sense::hover());
    let by_hand = !fusion && crate::hdr_auto::moved_by_hand(s.settings.auto_tone.and_then(|a| a.applied), info.tone_now);
    let tip = if fusion {
        crate::tr("Wie stark Lichter und Tiefen ausgeglichen werden (0 % = Referenzaufnahme)")
    } else if by_hand {
        crate::tr("Tonwerte von Hand geändert: neue Aufnahmen stimmen nicht mehr ab, der Regler stimmt neu ab")
    } else {
        crate::tr("Wie stark Lichter und Tiefen ausgeglichen werden (setzt Belichtung, Lichter, Tiefen, Weiß, Schwarz)")
    };
    lr::hover_tip(ui, &probe, tip);
    if !moved {
        return false;
    }
    set_strength(s, info, v);
    true
}

/// The slider "Stärke" moved to `v` percent: the fusion strength, or the strength of the automatic tone (which tunes
/// again).
fn set_strength(s: &mut State, info: &Info, v: f64) {
    let k = (v / 100.0).clamp(0.0, 1.0) as f32;
    if s.settings.output == hdr::fusion::Output::Fusion {
        s.settings.fusion_strength = k;
    } else {
        s.settings.auto_tone = Some(hdr::AutoTone { strength: k, applied: s.settings.auto_tone.and_then(|a| a.applied) });
        crate::hdr_auto::remember(info.file, s.settings.auto_tone);
        crate::hdr_auto::want(info.file, true, None);
    }
}

/// The settings shown: hidden areas left out.
fn view_of(s: &State) -> hdr::Settings {
    let mut v = s.settings.clone();
    v.regions = s.settings.regions.iter().zip(&s.hidden).filter(|(_, h)| !**h).map(|(a, _)| a.clone()).collect();
    v
}

/// Remember a change: `save` = part of the recipe (not only a comparison).
fn changed(s: &mut State, save: bool, sources: Option<Vec<i64>>) {
    s.change = Some(Change { view: view_of(s), save: save.then(|| s.settings.clone()), sources });
}

/// A change for the app, once.
pub fn take_change(ctx: &Context, info: &Info) -> Option<Change> {
    let mut s = sync(ctx, info);
    let mut c = s.change.take();
    // A source is not reachable: nothing can be merged anew, so no change of the recipe is taken (it would be stored
    // and never computed, and the picture shown would not be the recipe's any more). The tool goes back to the
    // stored recipe and says why.
    if c.is_some() && !info.missing.is_empty() {
        c = None;
        // (the stored recipe, not the settings of the picture shown: that may be an older state of the photo)
        let stored = library::merge::recipe_of_file(info.file).and_then(|(r, _)| rawdev::merged::parse_settings(&r.settings).ok());
        s.settings = hdr::Settings { auto_tone: s.settings.auto_tone, ..stored.unwrap_or_else(|| info.settings.clone()) };
        s.hidden = vec![false; s.settings.regions.len()];
        s.selected = None;
        // (a comparison that was asked for – an area hidden – goes with it: the tool shows no area as hidden any
        // more, so the photo must not go on showing, or computing, the picture without that area; seed 905194)
        if crate::hdr_ui::wanted(info.file).is_some_and(|w| Some(&w.key) != info.stored_key.as_ref()) {
            crate::hdr_ui::unwant(info.file);
        }
        crate::hdr_live::say(&locked_text(info));
    }
    save(ctx, s);
    c
}

/// Why the recipe of a merged photo cannot be changed now: a source is not reachable.
pub(crate) fn locked_text(info: &Info) -> String {
    crate::trf!("Quelle nicht erreichbar: {a}. Das Bild bleibt, wie es ist – ändern und neu berechnen geht wieder, wenn sie da ist.", a = info.missing.join(", "))
}

/// Settings for a new list of sources: reference and the frames of the areas follow their files; an area whose frame
/// left takes the reference.
pub fn remap(settings: &hdr::Settings, old: &[i64], new: &[i64]) -> hdr::Settings {
    let map = |i: usize| old.get(i).and_then(|f| new.iter().position(|n| n == f));
    let mut s = settings.clone();
    s.reference = settings.reference.and_then(map);
    // (callers name a reference; the middle of the list only stands in when there is none at all)
    let fallback = s.reference.unwrap_or(new.len() / 2);
    for a in s.regions.iter_mut() {
        a.frame = map(a.frame).unwrap_or(fallback);
    }
    s
}

/// The frame that becomes the reference when the reference `removed` is left out: the one nearest in exposure
/// (`ev` per frame, the lower index on a tie), without exposure data the nearest in the list. Index in the old list.
pub fn next_reference(ev: &[Option<f64>], removed: usize, n: usize) -> usize {
    let at = ev.get(removed).copied().flatten();
    let distance = |i: usize| match (at, ev.get(i).copied().flatten()) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => f64::INFINITY,
    };
    (0..n)
        .filter(|i| *i != removed)
        .min_by(|a, b| distance(*a).total_cmp(&distance(*b)).then(a.abs_diff(removed).cmp(&b.abs_diff(removed))).then(a.cmp(b)))
        .unwrap_or(0)
}

/// The recipe without frame `i` of `files`: the new list and the settings for it (the reference and the areas follow
/// their files; a reference left out hands over to [`next_reference`], and so do its areas). `None` when the frame
/// cannot be left out: at least [`hdr::MIN_FRAMES`] stay, and `i` has to be a frame of `files`.
pub fn without_frame(settings: &hdr::Settings, files: &[i64], i: usize, reference: usize, ev: &[Option<f64>]) -> Option<(Vec<i64>, hdr::Settings)> {
    if files.len() <= hdr::MIN_FRAMES || i >= files.len() {
        return None;
    }
    let mut v = files.to_vec();
    v.remove(i);
    let mut st = settings.clone();
    st.reference = Some(if reference == i || reference >= files.len() { next_reference(ev, i, files.len()) } else { reference });
    Some((v.clone(), remap(&st, files, &v)))
}

/// Do the frames listed (`shown`: the sources of the photo on screen) belong to the stored recipe (`stored` paths,
/// `stored_ids` files)? Not while a new list is on its way into the recipe (`sent`) or being computed.
pub fn lists_in_step(shown: &[PathBuf], stored: &[PathBuf], stored_ids: &[i64], sent: Option<&[i64]>) -> bool {
    shown == stored && shown.len() == stored_ids.len() && sent.is_none_or(|s| s == stored_ids)
}

/// Library files of the frames listed (`shown`: the sources of the picture on screen). In step they are the recipe's
/// (`stored`); otherwise each shown path is looked up (`file_at`), so the list never pairs the picture's frames with
/// the recipe's files. A path the library does not know keeps no file (0): its row shows no EV and offers nothing.
pub fn list_files_of(in_step: bool, stored: &[i64], shown: &[PathBuf], file_at: impl Fn(&Path) -> Option<i64>) -> Vec<i64> {
    if in_step {
        return stored.to_vec();
    }
    shown.iter().map(|p| file_at(p).unwrap_or(0)).collect()
}

/// Index in the recipe (`stored`) of the frame listed in row `i` (`list`): by its file, never by its place.
pub fn recipe_index(list: &[i64], stored: &[i64], i: usize) -> Option<usize> {
    let f = *list.get(i)?;
    stored.iter().position(|s| *s == f && f != 0)
}

/// A row of the list of frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row {
    /// a frame of the merge (index in the sources)
    Source(usize),
    /// a photo offered (index in the candidates)
    Candidate(usize),
}

/// The fixed order of the list: by exposure (EV from the shooting data, darkest first), used or not – a frame left out
/// or taken in keeps its place. Without exposure data at the end, in their own order.
pub fn list_order(sources: &[Option<f64>], candidates: &[Option<f64>]) -> Vec<Row> {
    let mut rows: Vec<(Row, Option<f64>)> = sources.iter().enumerate().map(|(i, e)| (Row::Source(i), *e)).chain(candidates.iter().enumerate().map(|(j, e)| (Row::Candidate(j), *e))).collect();
    rows.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    rows.into_iter().map(|r| r.0).collect()
}

/// Number shown for source `i` (its place in the list; the index while the list is not known).
fn number_of(info: &Info, i: usize) -> String {
    number(info.numbers.get(i).copied().unwrap_or(i))
}

/// Number shown for frame `k` of the recipe (an area's frame): the number of the row with its file.
fn recipe_number(info: &Info, k: usize) -> String {
    match info.source_files.get(k).and_then(|f| info.list_files.iter().position(|l| l == f)) {
        Some(row) => number_of(info, row),
        None => number_of(info, k),
    }
}

/// Two-digit number of a place ("01").
fn number(i: usize) -> String {
    format!("{:02}", i + 1)
}

// ─────────────────────────────────────────────── textures of the frames (list, area chooser, single exposure)

/// Linear RGB (reference units) → 8-bit sRGB with a gain.
fn to_srgb(px: &[[f32; 3]], gain: f32) -> Vec<u8> {
    let srgb = |v: f32| -> u8 {
        let v = (v * gain).clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0 + 0.5) as u8
    };
    px.iter().flat_map(|p| [srgb(p[0]), srgb(p[1]), srgb(p[2])]).collect()
}

/// A frame "as shot": its aligned values times its exposure factor, clipped like a camera (where the frame saturates
/// the channels go to a neutral white instead of the pink of unequally clipped channels).
fn as_shot(px: &[[f32; 3]], k: f32) -> Vec<[f32; 3]> {
    px.iter()
        .map(|p| {
            let v = p.map(|c| c * k);
            let m = v[0].max(v[1]).max(v[2]);
            let t = ((v[1] - 0.85) / 0.12).clamp(0.0, 1.0);
            let t = t * t * (3.0 - 2.0 * t);
            v.map(|c| (c + t * (m - c)).min(1.0))
        })
        .collect()
}

/// Common gain: the median green of the merge at mid grey.
fn gain_of(m: &hdr::Merged) -> f32 {
    let mut g: Vec<f32> = m.px.iter().step_by(17).map(|p| p[1]).collect();
    if g.is_empty() {
        return 1.0;
    }
    let k = g.len() / 2;
    let med = *g.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1;
    0.18 / med.max(1e-4)
}

/// Cached textures of the frames of one merge: small thumbnails and, for the area chooser, the crops of one area.
#[derive(Default)]
struct Textures {
    base_key: String,
    thumbs: Vec<egui::TextureHandle>,
    /// (area bounds, frame textures) of the chooser
    crops: Option<([usize; 4], Vec<egui::TextureHandle>)>,
}

static TEXTURES: Mutex<Option<Textures>> = Mutex::new(None);

/// Small pictures of the frames as shot (each with its own exposure), in the orientation of the preview data.
fn frame_thumbs(ctx: &Context, info: &Info) -> Vec<egui::TextureHandle> {
    let mut g = TEXTURES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = g.as_ref().filter(|t| t.base_key == info.base_key && t.thumbs.len() == info.sources.len()) {
        return t.thumbs.clone();
    }
    let Some((meta, frames)) = rawdev::merged::frames(&info.base_key, info.sources.len()) else { return Vec::new() };
    let gain = frames.get(meta.summary.reference).map(|f| gain_of(f)).unwrap_or(1.0);
    let thumbs: Vec<egui::TextureHandle> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let k = meta.geometry.k.get(i).copied().unwrap_or(1.0) as f32;
            let step = (f.w.max(f.h) / 96).max(1);
            let (tw, th) = (f.w / step, f.h / step);
            let small: Vec<[f32; 3]> = (0..tw * th).map(|j| f.px[(j / tw) * step * f.w + (j % tw) * step]).collect();
            let img = egui::ColorImage::from_rgb([tw, th], &to_srgb(&as_shot(&small, k), gain));
            ctx.load_texture(format!("hdr-frame-{i}"), img, egui::TextureOptions::LINEAR)
        })
        .collect();
    *g = Some(Textures { base_key: info.base_key.clone(), thumbs: thumbs.clone(), crops: None });
    thumbs
}

/// The crops of one area (bounds in preview pixels) from every frame, as shot.
fn area_crops(ctx: &Context, info: &Info, bounds: [usize; 4]) -> Vec<egui::TextureHandle> {
    let mut g = TEXTURES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, c)) = g.as_ref().filter(|t| t.base_key == info.base_key).and_then(|t| t.crops.as_ref()).filter(|(b, _)| *b == bounds) {
        return c.clone();
    }
    let Some((meta, frames)) = rawdev::merged::frames(&info.base_key, info.sources.len()) else { return Vec::new() };
    let gain = frames.get(meta.summary.reference).map(|f| gain_of(f)).unwrap_or(1.0);
    let crops: Vec<egui::TextureHandle> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let k = meta.geometry.k.get(i).copied().unwrap_or(1.0) as f32;
            let px = hdr::regions::cut(&f.px, f.w, bounds);
            let img = egui::ColorImage::from_rgb([bounds[2], bounds[3]], &to_srgb(&as_shot(&px, k), gain));
            ctx.load_texture(format!("hdr-crop-{i}"), img, egui::TextureOptions::LINEAR)
        })
        .collect();
    if let Some(t) = g.as_mut().filter(|t| t.base_key == info.base_key) {
        t.crops = Some((bounds, crops.clone()));
    }
    crops
}

/// One exposure developed with the photo's settings, for "hold to look" (rendered in the background, once per base,
/// frame and settings).
struct Peek {
    key: (String, usize, u64),
    tex: Option<egui::TextureHandle>,
    image: Option<egui::ColorImage>,
    busy: bool,
}

static PEEK: Mutex<Option<Peek>> = Mutex::new(None);

fn settings_hash(s: &crate::Settings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{s:?}").hash(&mut h);
    h.finish()
}

/// The developed single exposure `i` of the open merged photo (`None` while it is rendered).
fn peek_texture_for(ctx: &Context, doc: &Arc<rawdev::RawDoc>, base_key: &str, n: usize, i: usize, settings: &crate::Settings) -> Option<egui::TextureId> {
    let k = (base_key.to_string(), i, settings_hash(settings));
    let mut g = PEEK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = g.as_mut().filter(|p| p.key == k) {
        if let Some(img) = p.image.take() {
            p.tex = Some(ctx.load_texture("hdr-peek", img, egui::TextureOptions::LINEAR));
        }
        if p.tex.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        return p.tex.as_ref().map(|t| t.id());
    }
    if g.as_ref().is_some_and(|p| p.busy) {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        return None;
    }
    *g = Some(Peek { key: k.clone(), tex: None, image: None, busy: true });
    let (doc, base_key, settings, ctx2) = (doc.clone(), base_key.to_string(), settings.clone(), ctx.clone());
    std::thread::spawn(move || {
        let img = (|| {
            let (meta, frames) = rawdev::merged::frames(&base_key, n)?;
            let f = frames.get(i)?;
            let kf = meta.geometry.k.get(i).copied().unwrap_or(1.0) as f32;
            // as shot: the frame's own exposure relative to the reference white
            let shot = hdr::Merged { px: as_shot(&f.px, kf), w: f.w, h: f.h, noise: f.noise };
            let m = doc.merged.as_ref()?;
            let d = doc.with_merged(Some(&shot), None, m.source.clone(), m.meta.clone());
            let img = d.render_preview(&settings);
            Some(egui::ColorImage::from_rgb([img.width, img.height], &img.to_rgb8()))
        })();
        let mut g = PEEK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = g.as_mut() {
            if p.key == k {
                p.image = img;
            }
            p.busy = false;
        }
        ctx2.request_repaint();
    });
    None
}

/// The single exposure held down in the list: texture to draw instead of the merge.
pub fn peek_texture(ctx: &Context, doc: &Arc<rawdev::RawDoc>, info: &Info, settings: &crate::Settings) -> Option<egui::TextureId> {
    let s = state(ctx);
    let i = s.peek.filter(|_| s.open && s.file == info.file)?;
    peek_texture_for(ctx, doc, &info.base_key, info.sources.len(), i, settings)
}

/// Label of the info box while a single exposure is shown ("AUFNAHME 03 · +1,3 EV · nicht das HDR").
pub fn peek_label(ctx: &Context, info: &Info) -> Option<String> {
    let s = state(ctx);
    let i = s.peek.filter(|_| s.open && s.file == info.file)?;
    let ev = info.ev.get(i).map(|e| format!(" · {} EV", crate::hdr_ui::ev_label(*e))).unwrap_or_default();
    Some(crate::trf!("AUFNAHME {n}{ev} · nicht das HDR", n = number_of(info, i), ev = ev))
}

/// Sharp crops of the single exposures for the part of the photo on screen (zoomed in): rendered ahead for every
/// frame while the tool is open and the view rests, so holding a frame shows it at once at full resolution, developed
/// and aligned like the merge.
struct PeekDetail {
    /// base, settings, region (x, y, w, h in source pixels, with a margin for small pans)
    key: (String, u64, [usize; 4]),
    images: Vec<Option<egui::ColorImage>>,
    tex: Vec<Option<egui::TextureHandle>>,
    busy: bool,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

static PEEK_DETAIL: Mutex<Option<PeekDetail>> = Mutex::new(None);
/// Frame held down now (−1: none), for the order of rendering ahead.
static HELD: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
/// When a frame was pressed (time until it is sharp, for the log).
static PEEK_SINCE: Mutex<Option<(usize, std::time::Instant)>> = Mutex::new(None);

fn contains(outer: [usize; 4], inner: [usize; 4]) -> bool {
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[0] + inner[2] <= outer[0] + outer[2] && inner[1] + inner[3] <= outer[1] + outer[3]
}

/// One frame at full resolution for `region`: the merge's full resolution with the frame (as shot) pasted over the
/// region and a margin, developed like the merge (lens correction, noise reduction and sharpening need the whole image).
fn render_frame_region(doc: &rawdev::RawDoc, i: usize, region: [usize; 4], settings: &crate::Settings, preview: &hdr::Merged, k: f32) -> Option<develop::Rgb16> {
    let m = doc.merged.as_ref()?;
    let base = doc.full_base().ok()?;
    let (w, h) = (base.w, base.h);
    let margin = 256;
    let x0 = region[0].saturating_sub(margin);
    let y0 = region[1].saturating_sub(margin);
    let x1 = (region[0] + region[2] + margin).min(w);
    let y1 = (region[1] + region[3] + margin).min(h);
    let rect = [x0, y0, x1 - x0, y1 - y0];
    let patch = as_shot(&rawdev::merged::frame_patch(&m.source, i, rect)?, k);
    let mut px = base.px.clone();
    for y in 0..rect[3] {
        for x in 0..rect[2] {
            let v = patch[y * rect[2] + x];
            if v[0].is_finite() && v[1].is_finite() && v[2].is_finite() {
                px[(y0 + y) * w + x0 + x] = v;
            }
        }
    }
    let full = hdr::Merged { px, w, h, noise: preview.noise };
    let d = doc.with_merged(Some(preview), Some(&full), m.source.clone(), m.meta.clone());
    d.render_region(settings, region).ok()
}

/// The part of the source on screen (x, y, w, h in source pixels).
pub fn visible_region(rect: egui::Rect, avail: egui::Rect, frame: &crate::Frame, full: [usize; 2]) -> [usize; 4] {
    let vis = rect.intersect(avail);
    let to_src = |p: egui::Pos2| {
        let u = (p - rect.min) / rect.size();
        frame.to_src([u.x, u.y])
    };
    let pts = [vis.left_top(), vis.right_top(), vis.right_bottom(), vis.left_bottom()].map(to_src);
    let (x0, x1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
    let (y0, y1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    crate::darkroom_loupe::source_region([x0, y0, x1, y1], full)
}

/// Each frame while zoomed in: the sharp crop of the held frame (texture and its region) when it is there; the crops of
/// all frames are rendered ahead once the view rests (`resting`).
#[allow(clippy::too_many_arguments)]
pub fn peek_detail(ctx: &Context, doc: &Arc<rawdev::RawDoc>, info: &Info, settings: &crate::Settings, region: [usize; 4], full: [usize; 2], resting: bool) -> Option<(egui::TextureId, [usize; 4])> {
    let st = state(ctx);
    HELD.store(st.peek.map_or(-1, |p| p as i64), std::sync::atomic::Ordering::Relaxed);
    if !st.open || st.file != info.file || info.pending {
        return None;
    }
    let sh = settings_hash(settings);
    let mut g = PEEK_DETAIL.lock().unwrap_or_else(|e| e.into_inner());
    let fits = g.as_ref().is_some_and(|p| p.key.0 == info.base_key && p.key.1 == sh && contains(p.key.2, region));
    if !fits && resting {
        if let Some(p) = g.as_ref() {
            p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // a quarter more on every side: small pans stay sharp
        let (mx, my) = (region[2] / 4, region[3] / 4);
        let x0 = region[0].saturating_sub(mx);
        let y0 = region[1].saturating_sub(my);
        let r = [x0, y0, (region[0] + region[2] + mx).min(full[0]) - x0, (region[1] + region[3] + my).min(full[1]) - y0];
        let n = info.sources.len();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *g = Some(PeekDetail { key: (info.base_key.clone(), sh, r), images: vec![None; n], tex: vec![None; n], busy: true, cancel: cancel.clone() });
        let (doc, base_key, settings, ctx2) = (doc.clone(), info.base_key.clone(), settings.clone(), ctx.clone());
        let first = st.peek;
        std::thread::spawn(move || {
            library::platform::set_priority(library::platform::Priority::Utility);
            let Some((meta, frames)) = rawdev::merged::frames(&base_key, n) else { return };
            // the frame held now first (also one pressed meanwhile), then the others in order
            let _ = first;
            let mut done = vec![false; n];
            while let Some(i) = {
                let held = HELD.load(std::sync::atomic::Ordering::Relaxed);
                if held >= 0 && (held as usize) < n && !done[held as usize] { Some(held as usize) } else { done.iter().position(|d| !d) }
            } {
                done[i] = true;
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let k = meta.geometry.k.get(i).copied().unwrap_or(1.0) as f32;
                let pv = hdr::Merged { px: as_shot(&frames[i].px, k), w: frames[i].w, h: frames[i].h, noise: frames[i].noise };
                let t = std::time::Instant::now();
                let img = render_frame_region(&doc, i, r, &settings, &pv, k);
                crate::log(&format!("hdr: frame {i} at full resolution {}×{} in {:.0} ms", r[2], r[3], t.elapsed().as_secs_f64() * 1000.0));
                let mut g = PEEK_DETAIL.lock().unwrap_or_else(|e| e.into_inner());
                let Some(p) = g.as_mut().filter(|p| Arc::ptr_eq(&p.cancel, &cancel)) else { return };
                p.images[i] = img.map(|im| egui::ColorImage::from_rgb([im.width, im.height], &im.to_rgb8_dithered()));
                ctx2.request_repaint();
            }
            if let Some(p) = PEEK_DETAIL.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|p| Arc::ptr_eq(&p.cancel, &cancel)) {
                p.busy = false;
            }
        });
    }
    let p = g.as_mut().filter(|p| p.key.0 == info.base_key && p.key.1 == sh)?;
    let i = st.peek?;
    if let Some(img) = p.images.get_mut(i).and_then(|o| o.take()) {
        p.tex[i] = Some(ctx.load_texture(format!("hdr-peek-detail-{i}"), img, egui::TextureOptions::LINEAR));
    }
    let r = p.key.2;
    match p.tex.get(i).and_then(|t| t.as_ref()) {
        Some(t) => {
            if let Some((f, t0)) = PEEK_SINCE.lock().unwrap_or_else(|e| e.into_inner()).take() {
                crate::log(&format!("hdr: held frame {f} sharp after {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0));
            }
            Some((t.id(), r))
        }
        None => {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            None
        }
    }
}

// ─────────────────────────────────────────────── the panel

const ROW_H: f32 = 34.0;

/// What is known about the reachability of the sources of the merged photo shown: (file, sources, names of the missing
/// ones, when looked up, a lookup is on its way).
#[allow(clippy::type_complexity)]
static MISSING: Mutex<Option<(i64, Vec<PathBuf>, Vec<String>, std::time::Instant, bool)>> = Mutex::new(None);

/// Names of the sources of a merged photo that are not reachable now. Asked while drawing, and a source on a network
/// drive can take long to answer: looked up at once only the first time for a photo, afterwards every two seconds
/// beside the drawing (the answer known is given meanwhile).
fn missing_names(file: i64, sources: &[PathBuf]) -> Vec<String> {
    let look = |sources: &[PathBuf]| -> Vec<String> { library::merge::missing_sources(sources).iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string()).collect() };
    // (tests move files and look at once)
    if cfg!(test) {
        return look(sources);
    }
    let mut g = MISSING.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_mut().filter(|m| m.0 == file && m.1 == sources) {
        None => {
            let names = look(sources);
            *g = Some((file, sources.to_vec(), names.clone(), std::time::Instant::now(), false));
            names
        }
        Some(m) => {
            if !m.4 && m.3.elapsed() > std::time::Duration::from_secs(2) {
                m.4 = true;
                let sources = sources.to_vec();
                std::thread::spawn(move || {
                    let names = library::merge::missing_sources(&sources).iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string()).collect();
                    if let Some(m) = MISSING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|m| m.0 == file && m.1 == sources) {
                        *m = (file, sources, names, std::time::Instant::now(), false);
                    }
                });
            }
            m.2.clone()
        }
    }
}

/// Status line of the tool: what is computed, how far.
fn status_line(info: &Info) -> (String, Option<i64>) {
    // cancelled: said at once, no progress of a job that is only winding down, no second "Abbrechen"
    if crate::hdr_ui::cancelling(info.file) {
        return (crate::tr("Wird abgebrochen …").to_string(), None);
    }
    let job = rawdev::merged::jobs().into_iter().find(|j| j.file == info.file);
    match job {
        Some(j) if j.phase == rawdev::merged::Phase::Full && !info.pending => (crate::trf!("Vorschau aktuell · volle Auflösung {p} %", p = (j.fraction * 100.0).floor() as u32), Some(j.file)),
        Some(j) => (crate::trf!("Wird zusammengefügt … {p} %", p = (j.overall() * 100.0).floor() as u32), Some(j.file)),
        // its job waits behind another merge (a batch, another photo): said so, and "Abbrechen" takes it out at once
        None if crate::hdr_ui::waits(info.file) => (crate::tr("Wartet – eine andere Berechnung läuft noch").to_string(), Some(info.file)),
        // a job is about to start (its change is stored, its thread not computing yet): never "Aktuell" meanwhile
        None if info.busy && crate::hdr_ui::job_alive(info.file) => (crate::trf!("Wird zusammengefügt … {p} %", p = 0), Some(info.file)),
        None if info.busy && !info.pending => (crate::trf!("Wird zusammengefügt … {p} %", p = 0), None),
        // the preview shown is the recipe's, only its full resolution was stopped: said so ("Neu berechnen" beside it)
        None if info.failed.is_some() && !info.pending && !info.stale && info.stored_key.as_ref() == Some(&info.key) => (crate::tr("Vorschau aktuell · volle Auflösung abgebrochen").to_string(), None),
        None if info.failed.is_some() => (crate::trf!("Nicht aktuell – {e}", e = info.failed.clone().unwrap_or_default()), None),
        // (before "out of date": with a source gone "Neu berechnen" cannot help)
        None if (info.pending || info.stale) && !info.missing.is_empty() => (crate::trf!("Quelle fehlt: {a}", a = info.missing.join(", ")), None),
        None if info.stale => (crate::tr("Nicht aktuell – die Berechnung wurde abgebrochen").to_string(), None),
        None if info.pending => (crate::tr("Wartet auf die Berechnung …").to_string(), None),
        None if !info.full_ready => (crate::tr("Vorschau aktuell · volle Auflösung beim Zoomen").to_string(), None),
        None => (crate::tr("Aktuell · volle Auflösung").to_string(), None),
    }
}

/// One row of the list of frames: tick, picture, number or name, EV, star. Returns (row, tick, star).
#[allow(clippy::too_many_arguments)]
fn frame_row(ui: &mut egui::Ui, w: f32, label: &str, ev: Option<f64>, thumb: Option<egui::TextureId>, used: bool, is_ref: bool, id: usize) -> (egui::Response, egui::Response, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click_and_drag());
    let p = ui.painter().clone();
    if resp.is_pointer_button_down_on() && used {
        p.rect_filled(rect, 0.0, crate::lr::gray(0x34));
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, crate::lr::gray(0x22));
    }
    let tick = egui::Rect::from_min_size(rect.min, egui::vec2(26.0, ROW_H));
    let tr = ui.interact(tick, ui.id().with(("hdr-tick", id)), egui::Sense::click());
    let b = egui::Rect::from_center_size(tick.center(), egui::vec2(13.0, 13.0));
    p.rect_stroke(b, 0.0, egui::Stroke::new(1.0, if tr.hovered() { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() }), egui::StrokeKind::Inside);
    if used {
        lr::paint_icon(&p, b.shrink(1.0), lr::Icon::Check, lr::TEXT());
    }
    let th = egui::Rect::from_min_size(egui::pos2(tick.right() + 2.0, rect.center().y - 12.0), egui::vec2(36.0, 24.0));
    p.rect_filled(th, 0.0, crate::lr::gray(0x0c));
    if let Some(t) = thumb {
        p.image(t, th, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), if used { Color32::WHITE } else { Color32::from_gray(120) });
    }
    let star = egui::Rect::from_min_size(egui::pos2(rect.right() - 28.0, rect.top()), egui::vec2(26.0, ROW_H));
    let sr = ui.interact(star, ui.id().with(("hdr-star", id)), if used { egui::Sense::click() } else { egui::Sense::hover() });
    if used {
        let sc = if is_ref { lr::GOLD() } else if sr.hovered() { crate::lr::TEXT_STRONG() } else { crate::lr::gray(0x5a) };
        lr::paint_icon(&p, egui::Rect::from_center_size(star.center(), egui::vec2(14.0, 14.0)), lr::Icon::Star, sc);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let tc = if used { lr::TEXT() } else { crate::lr::gray(0x78) };
    let room = (star.left() - 60.0 - th.right() - 8.0).max(20.0);
    let shown = lr::middle_ellipsis_font(&p, label, &font, room);
    p.text(egui::pos2(th.right() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, shown, font, tc);
    if let Some(e) = ev {
        p.text(egui::pos2(star.left() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, format!("{} EV", crate::hdr_ui::ev_label(e)), egui::TextStyle::Small.resolve(ui.style()), if used { lr::TEXT_DIM() } else { crate::lr::gray(0x66) });
    }
    (resp, tr, sr)
}

/// A switch with its label on one line; what it does is said in the tooltip (no explaining lines in the tray).
fn switch(ui: &mut egui::Ui, on: &mut bool, label: &str, tip: &str) -> bool {
    let mut changed = false;
    let r = ui.horizontal(|ui| {
        changed = crate::dialog::toggle(ui, on);
        if ui.add(egui::Label::new(egui::RichText::new(label).color(lr::TEXT())).selectable(false).sense(egui::Sense::click())).clicked() {
            *on = !*on;
            changed = true;
        }
    });
    let probe = ui.interact(r.response.rect, ui.id().with(("hdr-switch", label)), egui::Sense::hover());
    lr::hover_tip(ui, &probe, tip);
    changed
}

/// EV of frame `i` relative to the reference as the list shows it: from the shooting data, like the photos offered (a
/// frame reads the same ticked and unticked); the measured value only without shooting data.
fn source_ev(info: &Info, i: usize) -> Option<f64> {
    let exif = |k: usize| info.source_exif_ev.get(k).copied().flatten();
    exif(i).zip(exif(info.reference)).map(|(a, b)| a - b).or_else(|| info.ev.get(i).copied())
}

/// The panel. Returns a photo to take in as a frame (ticked candidate or dropped from the filmstrip).
pub fn panel(ui: &mut egui::Ui, info: &Info) -> Option<i64> {
    let ctx = ui.ctx().clone();
    let mut s = sync(&ctx, info);
    let mut take_in = None;
    let n = info.sources.len();
    let reference = s.settings.reference.unwrap_or(info.reference).min(n.saturating_sub(1));
    let w = lr::visible_width(ui);
    ui.add_space(4.0);
    // status: progress and cancelling, the same as in the title bar
    let (line, running) = status_line(info);
    let mut recompute = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(line).small().color(lr::TEXT_DIM()));
        if let Some(f) = running {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = ui.add(egui::Button::new(egui::RichText::new(crate::tr("Abbrechen")).small()).frame(false));
                // what cancelling does to the photo: a new merge that shows nothing yet goes again, everything else stays
                let goes = crate::hdr_ui::creating(f) && info.pending && crate::hdr_ui::wanted(f).is_none_or(|w| w.key == info.key);
                lr::hover_tip(ui, &b, crate::tr(if goes { "Bricht die Berechnung ab; das neue HDR wird wieder entfernt" } else { "Bricht die Berechnung ab; das Foto bleibt, wie es gezeigt wird" }));
                if b.clicked() {
                    crate::hdr_ui::cancel(f);
                }
            });
        }
    });
    if running.is_none() && (info.stale || info.failed.is_some()) && info.missing.is_empty() {
        // the recipe is ahead of the picture: one click computes it (on its own row: beside the status it covered
        // "volle Auflösung …", QA 11)
        if lr::secondary_button(ui, None, "Neu berechnen").clicked() {
            recompute = true;
        }
    }
    // opened without the reference file: the picture is rendered with less than the file gives – never silently
    if info.offline {
        ui.add(egui::Label::new(egui::RichText::new(crate::tr("Referenzaufnahme nicht erreichbar: Objektivkorrektur vereinfacht")).small().color(lr::TEXT_DIM())).wrap());
    }
    // a source is gone: the picture stays usable (from the cache), the controls below are locked – said here
    if !info.missing.is_empty() {
        if !(info.pending || info.stale) {
            ui.add(egui::Label::new(egui::RichText::new(locked_text(info)).small().color(lr::TEXT_DIM())).wrap());
        }
        ui.disable();
    }
    if let Some(wn) = info.warnings.first() {
        // (the numbers of the rows: with a frame left out its neighbours keep theirs – QA 11, N-3)
        let names: Vec<String> = (0..n).map(|i| number_of(info, i)).collect();
        ui.add(egui::Label::new(egui::RichText::new(crate::hdr_ui::warning_text(wn, &names)).small().color(lr::TEXT_DIM())).wrap());
    }

    // ── AUFNAHMEN
    // header with a "?" card: what tick, star, holding and dragging do (no tooltip over the rows: it hid the next one)
    lr::category_with(ui, crate::tr("Aufnahmen"), 20.0, |ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        crate::help::question_mark(ui, r.center(), ui.id().with("hdr-frames-help"), true, "Aufnahmen", "", &crate::help::HDR_FRAMES, "");
    });
    let thumbs = frame_thumbs(&ctx, info);
    // the way out of a change of the frames that was cancelled or failed: back to the frames of the picture shown
    let mut restore = false;
    if !info.in_step && info.busy {
        // why ticks and stars do nothing now
        ui.label(egui::RichText::new(crate::tr("Die Aufnahmen werden neu berechnet – ändern geht gleich wieder")).small().color(lr::TEXT_DIM()));
    } else if !info.in_step {
        // nothing computes: the list is the one of the picture shown, the recipe holds another one
        ui.add(egui::Label::new(egui::RichText::new(crate::tr("Die geänderte Auswahl der Aufnahmen ist nicht berechnet. Die Liste zeigt die Aufnahmen des Bildes.")).small().color(lr::TEXT_DIM())).wrap());
        let b = lr::secondary_button(ui, None, "Auswahl zurücknehmen");
        lr::hover_tip(ui, &b, crate::tr("Das Rezept nimmt wieder die Aufnahmen des gezeigten Bildes; danach lässt sich die Liste wieder ändern"));
        restore = b.clicked();
    }
    let mut leave_out: Option<usize> = None;
    let mut new_reference: Option<usize> = None;
    let mut held: Option<usize> = None;
    let src_ev: Vec<Option<f64>> = (0..n).map(|i| info.source_exif_ev.get(i).copied().flatten().or_else(|| info.ev.get(i).copied())).collect();
    let cand_ev: Vec<Option<f64>> = info.candidates.iter().map(|c| c.exif_ev).collect();
    let mut test_fine = false;
    // what the test hook does in the sections below (each like the click on its control)
    let mut test = TestClicks::default();
    for (what, arg) in test_actions() {
        match (what.as_str(), arg) {
            ("fine", _) => test_fine = true,
            // like the tick: only while the list and the recipe belong together
            ("leave", Some(i)) if info.in_step && i < n => {
                crate::log(&format!("hdr test: frame row {i} left out"));
                leave_out = Some(i);
            }
            ("leave", Some(i)) => crate::log(&format!("hdr test: frame row {i} not left out (frames locked: {})", !info.in_step)),
            // the star of a frame, the tick of a photo offered, a photo dropped on the list: same lock as the clicks
            ("reference", Some(i)) if info.in_step && i < n && i != reference => new_reference = Some(i),
            ("take", Some(j)) if info.in_step => take_in = info.candidates.get(j).map(|c| c.file),
            ("drop", Some(file)) if info.in_step => take_in = Some(file as i64),
            ("drop", Some(_)) => crate::hdr_live::say(crate::tr("Die Aufnahmen werden neu berechnet – ändern geht gleich wieder")),
            ("reference" | "take", _) => {}
            // "Abbrechen" and "Neu berechnen" exist only while a job runs / the photo is out of date
            ("cancel", _) => {
                if let Some(f) = running {
                    crate::hdr_ui::cancel(f);
                }
            }
            ("recompute", _) => recompute = running.is_none() && (info.stale || info.failed.is_some()) && info.missing.is_empty(),
            ("restore", _) => restore = !info.in_step && !info.busy,
            ("ghost", Some(k)) => test.ghost = [hdr::ghost::Level::Off, hdr::ghost::Level::Standard, hdr::ghost::Level::Fine].get(k).copied(),
            ("output", Some(k)) => test.output = [hdr::fusion::Output::Linear, hdr::fusion::Output::Fusion].get(k).copied(),
            ("align", _) => test.align = true,
            ("strength", Some(v)) => test.strength = Some(v as f64),
            ("paint", Some(seed)) => test.paint = Some(seed),
            ("area_frame", Some(v)) => test.area_frame = Some((v / 16, v % 16)),
            ("eye", Some(i)) => test.eye = Some(i),
            ("delete", Some(i)) => test.delete = Some(i),
            (other, _) => TEST_FOR_APP.lock().unwrap_or_else(|e| e.into_inner()).push(other.to_string()),
        }
        ctx.request_repaint();
    }
    if crate::env::var_os("GRAINEO_HDR_TEST").is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    for row in list_order(&src_ev, &cand_ev) {
        match row {
            Row::Source(i) => {
                let (resp, tick, star) = frame_row(ui, w, &number_of(info, i), source_ev(info, i), thumbs.get(i).map(|t| t.id()), true, i == reference, i);
                lr::hover_tip(ui, &tick, crate::tr("Aufnahme nicht benutzen"));
                lr::hover_tip(ui, &star, crate::tr(if i == reference { "Referenz" } else { "Als Referenz nehmen" }));
                if !info.in_step {
                    // the list is that of the photo shown, the recipe already another one: nothing is changed by index
                } else if tick.clicked() {
                    leave_out = Some(i);
                } else if star.clicked() && i != reference {
                    new_reference = Some(i);
                } else if resp.is_pointer_button_down_on() && !tick.contains_pointer() && !star.contains_pointer() && !thumbs.is_empty() {
                    held = Some(i);
                }
            }
            Row::Candidate(j) => {
                let c = &info.candidates[j];
                let place = list_order(&src_ev, &cand_ev).iter().position(|r| *r == Row::Candidate(j)).unwrap_or(n + j);
                let (resp, tick, _) = frame_row(ui, w, &number(place), c.ev, c.thumb.as_ref().map(|t| t.id()), false, false, 1000 + j);
                lr::hover_tip(ui, &tick, crate::tr("Aufnahme dazunehmen"));
                if info.in_step && (tick.clicked() || resp.double_clicked()) {
                    take_in = Some(c.file);
                }
            }
        }
    }
    // drop place: a photo from the filmstrip becomes a frame
    {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 32.0), egui::Sense::hover());
        let dragging = egui::DragAndDrop::has_payload_of_type::<crate::library_ui::PhotoDrag>(&ctx);
        let over = dragging && resp.contains_pointer();
        let stroke = egui::Stroke::new(1.0, if over { crate::lr::TEXT_STRONG() } else { crate::lr::gray(0x3a) });
        let p = ui.painter();
        let r = rect.shrink2(egui::vec2(0.0, 4.0));
        for (a, b) in [(r.left_top(), r.right_top()), (r.right_top(), r.right_bottom()), (r.right_bottom(), r.left_bottom()), (r.left_bottom(), r.left_top())] {
            p.extend(egui::Shape::dashed_line(&[a, b], stroke, 4.0, 3.0));
        }
        p.text(r.center(), egui::Align2::CENTER_CENTER, crate::tr("Foto aus dem Filmstreifen hierher ziehen"), egui::TextStyle::Small.resolve(ui.style()), if over { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() });
        if let Some(d) = resp.dnd_release_payload::<crate::library_ui::PhotoDrag>() {
            if info.in_step {
                take_in = Some(d.file);
            } else {
                crate::hdr_live::say(crate::tr("Die Aufnahmen werden neu berechnet – ändern geht gleich wieder"));
            }
        }
    }
    // holding a frame: that exposure is shown until the button is let go
    let down = ui.input(|i| i.pointer.primary_down());
    let before_peek = s.peek;
    s.peek = if down { held.or(s.peek) } else { None };
    if let (None, Some(i)) = (before_peek, s.peek) {
        *PEEK_SINCE.lock().unwrap_or_else(|e| e.into_inner()) = Some((i, std::time::Instant::now()));
    }

    // ── BEWEGUNG, AUSGABE, Ausrichten
    use hdr::ghost::Level;
    let before = s.settings.clone();
    let levels = [(Level::Off, "Aus"), (Level::Standard, "Standard"), (Level::Fine, "Fein")];
    let tips = ["Alle Aufnahmen überall verrechnen (für ruhige Motive)", "Bewegte Stellen aus der Referenz, weich überblendet", "Jede Aufnahme einzeln dort ausschließen, wo sie abweicht"];
    ui.add_space(6.0);
    lr::choice_tips(ui, "Bewegung", &mut s.settings.ghost, &levels, &tips, "");
    if test_fine {
        crate::log("hdr test: Bewegung → Fein");
        s.settings.ghost = Level::Fine;
    }
    if let Some(g) = test.ghost {
        s.settings.ghost = g;
    }
    let off = s.settings.ghost == Level::Off;
    ui.add_enabled_ui(!off, |ui| {
        switch(ui, &mut s.show_motion, crate::tr("Erkannte Bewegung zeigen"), crate::tr("Färbt die Stellen ein, an denen eine Aufnahme abweicht"));
    });
    let outputs = [(hdr::fusion::Output::Linear, "Linear"), (hdr::fusion::Output::Fusion, "Fusion")];
    let output_tips = ["Szenenlinear, die Lichter bleiben über Weiß erhalten (für die eigene Entwicklung)", "Natürlicher Look: die Belichtungen werden überblendet, der Umfang auf 0 bis 1 verdichtet"];
    lr::choice_tips(ui, "Ausgabe", &mut s.settings.output, &outputs, &output_tips, "");
    if let Some(o) = test.output {
        s.settings.output = o;
    }
    let output_before = before.output;
    let mut strength_moved = strength_slider(ui, &mut s, info);
    if let Some(v) = test.strength {
        set_strength(&mut s, info, v);
        strength_moved = true;
    }
    if strength_moved && s.settings.output == hdr::fusion::Output::Fusion {
        s.strength_moved = true;
    }
    ui.add_space(4.0);
    switch(ui, &mut s.settings.align, crate::tr("Ausrichten"), crate::tr("Gleicht Verschiebung, Drehung und Maßstab zwischen den Aufnahmen aus"));
    if test.align {
        s.settings.align = !s.settings.align;
    }
    if let Some(r) = new_reference.filter(|_| info.in_step).and_then(|i| recipe_index(&info.list_files, &info.source_files, i)) {
        s.settings.reference = Some(r);
        crate::hdr_auto::want(info.file, false, Some(info.key.clone()));
    }
    if s.settings.output != output_before {
        // to fusion: the values the automatic tone set are taken back (the fusion compresses the range itself); back to
        // linear: it is due again for the new pixels. Both only while the sliders are untouched (`hdr_auto`).
        crate::hdr_auto::want_output(info.file, Some(info.key.clone()));
    }
    // the strengths: the automatic tone changes no pixels (`hdr_auto` sets the sliders and stores it); the fusion
    // strength merges again once the slider is let go
    let pixels = |x: &hdr::Settings| hdr::Settings { auto_tone: None, fusion_strength: 1.0, ..x.clone() };
    let up = !ui.input(|i| i.pointer.primary_down());
    if pixels(&s.settings) != pixels(&before) || (s.strength_moved && up) || recompute {
        s.strength_moved = false;
        changed(&mut s, true, None);
    }
    // a frame left out: a new list of sources (at least two), by the files of the recipe the list was drawn from
    // (the row names a file of the list; its place in the recipe is looked up by that file)
    if let Some(i) = leave_out.filter(|_| info.in_step).and_then(|i| recipe_index(&info.list_files, &info.source_files, i)) {
        match without_frame(&s.settings, &info.source_files, i, reference, &src_ev) {
            Some((v, st)) => {
                s.settings = st;
                s.hidden = vec![false; s.settings.regions.len()];
                s.selected = None;
                sources_sent(info.file, &v);
                changed(&mut s, true, Some(v));
                crate::hdr_auto::want(info.file, false, Some(info.key.clone()));
            }
            None => crate::hdr_live::say(crate::tr("Mindestens zwei Aufnahmen.")),
        }
    }

    // ── GEISTER ENTFERNEN
    lr::category(ui, crate::tr("Geister entfernen"));
    let br = ui.add(egui::Button::new(crate::tr("Bereich malen")).selected(s.tool).min_size(egui::vec2(w, 26.0)));
    lr::hover_tip(ui, &br, crate::tr("Über einen Geist malen, dann die Aufnahme für den Bereich wählen"));
    if br.clicked() {
        s.tool = !s.tool;
        s.pending.clear();
    }
    if s.tool {
        ui.add_space(4.0);
        lr::choice(ui, "Werkzeug", &mut s.erase, &[(false, crate::tr("Malen")), (true, crate::tr("Verkleinern"))], "");
        ui.label(egui::RichText::new(crate::tr("Größe")).small().color(lr::TEXT_DIM()));
        let slider = ui.add(egui::Slider::new(&mut s.brush, 4.0..=150.0).logarithmic(true).show_value(false));
        lr::tip_key(ui, &slider, crate::tr("Pinselgröße, bleibt beim Zoomen auf dem Bildschirm gleich (größer: ])"), "brush_smaller");
        let nb = ui.add(egui::Button::new(crate::tr("Neuer Bereich")));
        if nb.clicked() {
            s.selected = None;
            s.erase = false;
        }
    }
    // the areas as a list: eye = compare, ✕ = delete, click = select
    let mut delete: Option<usize> = test.delete.filter(|i| *i < s.settings.regions.len());
    let mut eye: Option<usize> = test.eye.filter(|i| *i < s.settings.regions.len());
    // test hook: a stroke let go over the photo (a new area from the reference, as the brush makes it), and the click
    // on the crop of another frame for an area
    if let Some(seed) = test.paint {
        let at = [0.2 + 0.6 * ((seed % 7) as f32 / 6.0), 0.2 + 0.6 * ((seed / 7 % 5) as f32 / 4.0)];
        let stroke = hdr::regions::AreaStroke { erase: false, radius: 0.06, points: vec![at, [at[0] + 0.05, at[1]]] };
        let frame = s.settings.reference.unwrap_or(info.reference);
        s.settings.regions.push(hdr::regions::Area { frame, strokes: vec![stroke] });
        s.hidden.push(false);
        s.selected = Some(s.settings.regions.len() - 1);
        changed(&mut s, true, None);
    }
    if let Some((i, k)) = test.area_frame.filter(|(i, k)| info.in_step && *i < s.settings.regions.len() && *k < n && s.settings.regions[*i].frame != *k) {
        s.settings.regions[i].frame = k;
        changed(&mut s, true, None);
    }
    ui.add_space(4.0);
    for (i, a) in s.settings.regions.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
        let p = ui.painter().clone();
        if s.selected == Some(i) {
            p.rect_filled(rect, 0.0, crate::lr::gray(0x34));
        } else if resp.hovered() {
            p.rect_filled(rect, 0.0, crate::lr::gray(0x22));
        }
        let er = ui.interact(egui::Rect::from_min_size(rect.min, egui::vec2(24.0, 26.0)), ui.id().with(("hdr-area-eye", i)), egui::Sense::click());
        let hidden = s.hidden.get(i).copied().unwrap_or(false);
        lr::paint_icon(&p, egui::Rect::from_center_size(er.rect.center(), egui::vec2(14.0, 14.0)), if hidden { lr::Icon::EyeSlash } else { lr::Icon::Eye }, if er.hovered() { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() });
        lr::hover_tip(ui, &er, crate::tr(if hidden { "Bereich wieder einsetzen" } else { "Bereich ausblenden (vorher sehen)" }));
        let xr = ui.interact(egui::Rect::from_min_size(egui::pos2(rect.right() - 24.0, rect.top()), egui::vec2(24.0, 26.0)), ui.id().with(("hdr-area-x", i)), egui::Sense::click());
        lr::paint_icon(&p, egui::Rect::from_center_size(xr.rect.center(), egui::vec2(12.0, 12.0)), lr::Icon::X, if xr.hovered() { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() });
        lr::hover_tip(ui, &xr, crate::tr("Bereich löschen"));
        let text = crate::trf!("Bereich {k} · aus {f}", k = i + 1, f = recipe_number(info, a.frame));
        p.text(egui::pos2(rect.left() + 30.0, rect.center().y), egui::Align2::LEFT_CENTER, text, egui::TextStyle::Body.resolve(ui.style()), if hidden { crate::lr::gray(0x6a) } else { lr::TEXT() });
        if er.clicked() {
            eye = Some(i);
        } else if xr.clicked() {
            delete = Some(i);
        } else if resp.clicked() {
            s.selected = if s.selected == Some(i) { None } else { Some(i) };
            s.tool = true;
        }
    }
    if let Some(i) = eye {
        if let Some(h) = s.hidden.get_mut(i) {
            *h = !*h;
        }
        changed(&mut s, false, None);
    }
    if let Some(i) = delete {
        s.settings.regions.remove(i);
        s.hidden.remove(i);
        s.selected = None;
        changed(&mut s, true, None);
    }
    if restore && !info.in_step && !info.busy {
        // (the app knows the files of the picture's frames: carried out in `merge_update_step`)
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(info.file);
    }
    save(&ctx, s);
    take_in.filter(|f| !info.source_files.contains(f))
}

/// "Auswahl zurücknehmen" was clicked for this merged photo (carried out by the app in the same frame).
static RESTORE: Mutex<Option<i64>> = Mutex::new(None);

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info {
        Info {
            file: 7,
            name: "A-HDR.dng".into(),
            sources: vec!["a.cr2".into(), "b.cr2".into(), "c.cr2".into()],
            source_files: vec![1, 2, 3],
            list_files: vec![1, 2, 3],
            settings: hdr::Settings::default(),
            ev: vec![-2.0, 0.0, 2.0],
            reference: 1,
            base_key: "b".into(),
            key: "k".into(),
            tone_now: [0.0; 5],
            warnings: Vec::new(),
            pending: false,
            full_ready: true,
            source_exif_ev: Vec::new(),
            numbers: Vec::new(),
            candidates: Vec::new(),
            in_step: true,
            stale: false,
            busy: false,
            failed: None,
            stored_key: None,
            created: 0,
            missing: Vec::new(),
            offline: false,
        }
    }

    fn area(frame: usize) -> hdr::regions::Area {
        hdr::regions::Area { frame, strokes: vec![hdr::regions::AreaStroke { erase: false, radius: 0.05, points: vec![[0.5, 0.5]] }] }
    }

    /// A frame shows the EV of its shooting data relative to the reference (as the photos offered do), the measured
    /// value only without them.
    #[test]
    fn frame_ev_from_the_shooting_data() {
        let mut i = info();
        i.ev = vec![-2.07, 0.0, 3.1];
        assert_eq!(source_ev(&i, 2), Some(3.1));
        i.source_exif_ev = vec![Some(8.0), Some(10.0), Some(13.0)];
        assert_eq!(source_ev(&i, 2), Some(3.0));
        assert_eq!(source_ev(&i, 0), Some(-2.0));
        i.source_exif_ev[1] = None;
        assert_eq!(source_ev(&i, 2), Some(3.1));
    }

    /// A hidden area is left out of what is shown, but stays in the recipe; a change is handed out once.
    #[test]
    fn hidden_area_is_only_a_comparison() {
        let ctx = Context::default();
        let i = info();
        let mut s = sync(&ctx, &i);
        s.settings.regions = vec![area(0), area(2)];
        s.hidden = vec![false, true];
        changed(&mut s, false, None);
        save(&ctx, s);
        let c = take_change(&ctx, &i).expect("change");
        assert_eq!(c.view.regions.len(), 1);
        assert_eq!(c.view.regions[0].frame, 0);
        assert!(c.save.is_none());
        assert!(take_change(&ctx, &i).is_none());
    }

    /// Another photo starts with its own recipe, the brush size stays.
    #[test]
    fn other_photo_starts_with_its_recipe() {
        let ctx = Context::default();
        let mut i = info();
        let mut s = sync(&ctx, &i);
        s.settings.regions.push(area(2));
        s.brush = 40.0;
        save(&ctx, s);
        i.file = 8;
        let s = sync(&ctx, &i);
        assert!(s.settings.regions.is_empty());
        assert_eq!(s.brush, 40.0);
    }

    /// H1 of the second audit: a merged photo made after another one was removed may meet the tool with the same file –
    /// it is another photo (its recipe was created at another time) and starts with its own recipe. A recipe that
    /// cannot be read for a moment (`created` 0) drops nothing.
    #[test]
    fn a_new_photo_with_the_same_file_starts_with_its_recipe() {
        let ctx = Context::default();
        let mut i = info();
        i.created = 100;
        let mut s = sync(&ctx, &i);
        s.settings.regions.push(area(2));
        s.settings.reference = Some(2);
        save(&ctx, s);
        i.created = 0;
        assert_eq!(sync(&ctx, &i).settings.regions.len(), 1, "an unreadable moment keeps the tool's state");
        i.created = 101;
        i.settings.reference = Some(0);
        let s = sync(&ctx, &i);
        assert!(s.settings.regions.is_empty(), "the areas of the removed photo are not this photo's");
        assert_eq!(s.settings.reference, Some(0));
    }

    /// A frame left out: reference and areas follow their photos; an area of the frame gone takes the reference.
    /// Two frames left out in quick succession (the list on screen is still the old one, the recipe already the new
    /// one): never a wrong frame, never an index outside the list. The tool only acts while both belong together, and
    /// leaving out works on the files of the recipe.
    #[test]
    fn frames_are_left_out_by_file_never_by_a_stale_index() {
        let s = hdr::Settings { reference: Some(1), ..Default::default() };
        let ev = |n: usize| (0..n).map(|i| Some(i as f64 * 2.0)).collect::<Vec<_>>();
        // three frames, the first one left out: two stay
        let (v, st) = without_frame(&s, &[10, 20, 30], 0, 1, &ev(3)).unwrap();
        assert_eq!((v.as_slice(), st.reference), (&[20, 30][..], Some(0)));
        // the row of the old third frame clicked next: with the new recipe (two frames) nothing can be left out –
        // before, index 2 was removed from a list of two (crash)
        assert_eq!(without_frame(&st, &v, 2, 0, &ev(2)), None);
        assert_eq!(without_frame(&st, &v, 1, 0, &ev(2)), None, "at least two frames");
        // five frames: 10 left out, then the frame shown as third (30): with the lists in step its index in the
        // recipe is 1, and that file goes – not 40
        let (v, st) = without_frame(&s, &[10, 20, 30, 40, 50], 0, 1, &ev(5)).unwrap();
        let (v2, _) = without_frame(&st, &v, v.iter().position(|f| *f == 30).unwrap(), st.reference.unwrap(), &ev(4)).unwrap();
        assert_eq!(v2, vec![20, 40, 50]);
        // the lists: in step only when the photo shown has the frames of the recipe and no new list is on its way
        let p = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert!(lists_in_step(&p(&["a", "b", "c"]), &p(&["a", "b", "c"]), &[1, 2, 3], None));
        assert!(!lists_in_step(&p(&["a", "b", "c"]), &p(&["b", "c"]), &[2, 3], None), "the recipe is ahead of the picture");
        assert!(!lists_in_step(&p(&["a", "b", "c"]), &p(&["a", "b", "c"]), &[1, 2, 3], Some(&[2, 3])), "a new list is on its way");
        assert!(lists_in_step(&p(&["b", "c"]), &p(&["b", "c"]), &[2, 3], Some(&[2, 3])));
    }

    /// QA 11, M-1: while a change of the frames is computed the list is the picture's (six frames), the recipe already
    /// holds five. The rows are looked up by the picture's files – no row too many, no shifted EV – and a click names
    /// a file, whose place in the recipe is looked up (or nothing, while the lists differ).
    #[test]
    fn list_rows_follow_the_picture_shown_and_clicks_follow_files() {
        let shown: Vec<PathBuf> = (1..=6).map(|k| PathBuf::from(format!("g_0{k}.cr2"))).collect();
        let stored = vec![1, 3, 4, 5, 6];
        let at = |p: &Path| p.to_str().and_then(|s| s[3..4].parse::<i64>().ok());
        let list = list_files_of(false, &stored, &shown, at);
        assert_eq!(list, vec![1, 2, 3, 4, 5, 6], "the rows are the picture's frames");
        assert_eq!(list_files_of(true, &stored, &shown, at), stored);
        // the frame of row 4 (file 5) is frame 3 of the recipe, the dropped frame of row 1 is none
        assert_eq!(recipe_index(&list, &stored, 4), Some(3));
        assert_eq!(recipe_index(&list, &stored, 1), None);
        assert_eq!(recipe_index(&list, &stored, 9), None);
        // an area of the recipe's frame 2 (file 4) is named after its row: 04
        let mut i = info();
        i.source_files = stored.clone();
        i.list_files = list;
        i.numbers = (0..6).collect();
        assert_eq!(recipe_number(&i, 2), "04");
    }

    /// A change that was cancelled leaves the recipe ahead of the picture: the status says so instead of "Aktuell".
    #[test]
    fn a_cancelled_change_is_shown_as_out_of_date() {
        let mut i = info();
        i.file = 7_770_001;
        assert_eq!(status_line(&i).0, crate::tr("Aktuell · volle Auflösung"));
        i.stale = true;
        assert_eq!(status_line(&i), (crate::tr("Nicht aktuell – die Berechnung wurde abgebrochen").to_string(), None));
    }

    /// The reference left out: the frame nearest in exposure takes over (not the middle of the list), and the areas
    /// of the frame that left go to it.
    #[test]
    fn a_reference_left_out_hands_over_to_the_nearest_exposure() {
        // exposures −4, −2, 0, +1, +4 EV; the reference is +1 (index 3)
        let ev = [Some(-4.0), Some(-2.0), Some(0.0), Some(1.0), Some(4.0)];
        assert_eq!(next_reference(&ev, 3, 5), 2);
        assert_eq!(next_reference(&ev, 0, 5), 1);
        // a tie: the lower index; no exposure data: the neighbour
        assert_eq!(next_reference(&[Some(-1.0), Some(0.0), Some(1.0)], 1, 3), 0);
        assert_eq!(next_reference(&[None, None, None, None], 3, 4), 2);
        assert_eq!(next_reference(&[None, Some(0.0), None], 0, 3), 1);
        let s = hdr::Settings { reference: Some(3), regions: vec![area(3), area(4), area(0)], ..Default::default() };
        let (v, st) = without_frame(&s, &[10, 20, 30, 40, 50], 3, 3, &ev).unwrap();
        assert_eq!(v, vec![10, 20, 30, 50]);
        assert_eq!(st.reference, Some(2), "file 30, the nearest exposure – the middle of the new list would be the same here");
        assert_eq!(st.regions.iter().map(|a| a.frame).collect::<Vec<_>>(), vec![2, 3, 0]);
        // unsorted list (a frame taken in without exposure time lies first): still the nearest exposure, not len/2
        let ev = [None, Some(-2.0), Some(0.0), Some(2.0)];
        let s = hdr::Settings { reference: Some(3), regions: vec![area(3)], ..Default::default() };
        let (v, st) = without_frame(&s, &[5, 10, 20, 30], 3, 3, &ev).unwrap();
        assert_eq!((v, st.reference, st.regions[0].frame), (vec![5, 10, 20], Some(2), 2));
    }

    #[test]
    fn remap_follows_the_files() {
        let s = hdr::Settings { reference: Some(1), regions: vec![area(2), area(0)], ..Default::default() };
        let r = remap(&s, &[10, 20, 30], &[20, 30]);
        assert_eq!(r.reference, Some(0));
        assert_eq!(r.regions[0].frame, 1);
        assert_eq!(r.regions[1].frame, 0, "frame gone: the reference");
        let r = remap(&s, &[10, 20, 30], &[10, 20, 30, 40]);
        assert_eq!((r.reference, r.regions[0].frame), (Some(1), 2));
    }

    /// The list keeps one order by exposure: a frame left out stays in its place (dimmed), a photo taken in is sorted
    /// in by its EV, photos without exposure data come last.
    #[test]
    fn list_keeps_the_order_of_exposure() {
        use Row::*;
        // frames at −2, 0, +2 EV; offered: +1 and −1
        let order = list_order(&[Some(-2.0), Some(0.0), Some(2.0)], &[Some(1.0), Some(-1.0)]);
        assert_eq!(order, vec![Source(0), Candidate(1), Source(1), Candidate(0), Source(2)]);
        // frame 1 (0 EV) left out: it is a candidate now, at the same place
        let order = list_order(&[Some(-2.0), Some(2.0)], &[Some(1.0), Some(-1.0), Some(0.0)]);
        assert_eq!(order, vec![Source(0), Candidate(1), Candidate(2), Candidate(0), Source(1)]);
        // without exposure data: at the end
        assert_eq!(list_order(&[None, Some(1.0)], &[Some(0.0)]), vec![Candidate(0), Source(1), Source(0)]);
    }

    /// The outline of a round area is closed and lies on its edge.
    #[test]
    fn outline_follows_the_area() {
        let seg = outline(&area(0), 1.0);
        assert!(!seg.is_empty());
        for [a, b] in &seg {
            let m = [(a[0] + b[0]) / 2.0 - 0.5, (a[1] + b[1]) / 2.0 - 0.5];
            let r = (m[0] * m[0] + m[1] * m[1]).sqrt();
            assert!((r - 0.05).abs() < 0.01, "{r}");
        }
    }
}

// ─────────────────────────────────────────────── painting in the image

/// Outline of an area on a coarse grid, in normalised crop coordinates (segments).
fn outline(area: &hdr::regions::Area, aspect: f32) -> Vec<[[f32; 2]; 2]> {
    let (gw, gh) = if aspect >= 1.0 { (400, ((400.0 / aspect) as usize).max(1)) } else { (((400.0 * aspect) as usize).max(1), 400) };
    let r = hdr::regions::raster(area, gw, gh);
    let at = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < gw && (y as usize) < gh && r[y as usize * gw + x as usize] == 1;
    let mut seg = Vec::new();
    let (fx, fy) = (1.0 / gw as f32, 1.0 / gh as f32);
    for y in 0..gh as isize {
        for x in 0..gw as isize {
            if !at(x, y) {
                continue;
            }
            let (x0, y0, x1, y1) = (x as f32 * fx, y as f32 * fy, (x + 1) as f32 * fx, (y + 1) as f32 * fy);
            if !at(x - 1, y) {
                seg.push([[x0, y0], [x0, y1]]);
            }
            if !at(x + 1, y) {
                seg.push([[x1, y0], [x1, y1]]);
            }
            if !at(x, y - 1) {
                seg.push([[x0, y0], [x1, y0]]);
            }
            if !at(x, y + 1) {
                seg.push([[x0, y1], [x1, y1]]);
            }
        }
    }
    seg
}

/// Cached outlines (area JSON → segments).
static OUTLINES: Mutex<Vec<(String, Vec<[[f32; 2]; 2]>)>> = Mutex::new(Vec::new());

fn outline_cached(area: &hdr::regions::Area, aspect: f32) -> Vec<[[f32; 2]; 2]> {
    let k = format!("{}|{aspect:.4}", serde_json::to_string(area).unwrap_or_default());
    let mut g = OUTLINES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = g.iter().find(|(kk, _)| *kk == k) {
        return s.clone();
    }
    let s = outline(area, aspect);
    g.push((k, s.clone()));
    if g.len() > 32 {
        g.remove(0);
    }
    s
}

/// Motion plane of the open merge as a texture (magenta veil), once per base.
static MOTION: Mutex<Option<(String, Option<egui::TextureHandle>)>> = Mutex::new(None);

fn motion_texture(ctx: &Context, base_key: &str) -> Option<egui::TextureId> {
    let mut g = MOTION.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(k, _)| k != base_key) {
        let tex = rawdev::merged::motion(base_key).map(|(m, w, h)| {
            let px: Vec<Color32> = m.iter().map(|v| Color32::from_rgba_unmultiplied(0xe0, 0x30, 0xc0, (v.clamp(0.0, 1.0) * 120.0) as u8)).collect();
            ctx.load_texture("hdr-motion", egui::ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels: px }, egui::TextureOptions::LINEAR)
        });
        // not there yet (still computing): asked again later
        if tex.is_none() && rawdev::merged::read_base_meta(base_key).is_none() {
            return None;
        }
        *g = Some((base_key.to_string(), tex));
    }
    g.as_ref().and_then(|(_, t)| t.as_ref().map(|t| t.id()))
}

/// Number of places with detected motion of the open merge, for the label over the photo.
fn motion_label(base_key: &str) -> Option<String> {
    rawdev::merged::motion(base_key).map(|_| crate::tr("Erkannte Bewegung").to_string())
}

/// Painting and the overlays in the image: detected motion, the areas and the brush, the chooser of the selected area.
/// `full`: size of the source image in pixels (the crop of the merge; coordinates are normalised to it).
#[allow(clippy::too_many_arguments)]
pub fn overlay(ui: &egui::Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, frame: &crate::Frame, full: [f32; 2], info: &Info, avail: egui::Rect) {
    let ctx = ui.ctx().clone();
    let mut s = sync(&ctx, info);
    if !s.open {
        save(&ctx, s);
        return;
    }
    let to_screen = |p: [f32; 2]| -> Pos2 {
        let u = frame.from_src([p[0] * full[0], p[1] * full[1]]);
        rect.min + egui::vec2(u[0], u[1]) * rect.size()
    };
    let to_image = |p: Pos2| -> [f32; 2] {
        let u = (p - rect.min) / rect.size();
        let q = frame.to_src([u.x, u.y]);
        [(q[0] / full[0]).clamp(0.0, 1.0), (q[1] / full[1]).clamp(0.0, 1.0)]
    };
    // detected motion over the photo (steps back while a control outside the photo is held, as for the masks)
    if s.show_motion && s.settings.ghost != hdr::ghost::Level::Off && s.peek.is_none() && !crate::masks_ui::control_held(&ctx, avail) {
        if let Some(t) = motion_texture(&ctx, &info.base_key) {
            crate::paint_src_quad(painter, t, [0.0, 0.0, full[0], full[1]], frame, rect, Color32::WHITE);
            if let Some(l) = motion_label(&info.base_key) {
                let g = painter.layout_no_wrap(l, egui::FontId::proportional(11.5), crate::lr::TEXT_STRONG());
                let r = egui::Rect::from_min_size(avail.left_top() + egui::vec2(12.0, 12.0), g.size() + egui::vec2(12.0, 6.0));
                painter.rect_filled(r, 0.0, Color32::from_black_alpha(150));
                painter.rect_filled(egui::Rect::from_min_size(r.left_top(), egui::vec2(3.0, r.height())), 0.0, Color32::from_rgb(0xe0, 0x30, 0xc0));
                painter.galley(r.min + egui::vec2(8.0, 3.0), g, crate::lr::TEXT_STRONG());
            }
        }
    }
    if !s.tool || s.peek.is_some() {
        save(&ctx, s);
        return;
    }
    let aspect = full[0] / full[1].max(1.0);
    // the areas: thin outlines, the selected one light
    for (i, a) in s.settings.regions.iter().enumerate() {
        let sel = s.selected == Some(i);
        let c = if sel { crate::lr::overlay(235) } else { crate::lr::overlay(120) };
        for [a0, b0] in outline_cached(a, aspect) {
            painter.line_segment([to_screen(a0), to_screen(b0)], egui::Stroke::new(if sel { 1.5 } else { 1.0 }, c));
        }
    }
    // brush: a screen-constant radius; [ and ] change it
    if !ctx.egui_wants_keyboard_input() {
        if crate::shortcuts::pressed(&ctx, "brush_smaller") {
            s.brush = (s.brush / 1.2).max(4.0);
        }
        if crate::shortcuts::pressed(&ctx, "brush_larger") {
            s.brush = (s.brush * 1.2).min(150.0);
        }
    }
    let long_edge_screen = {
        let a = to_screen([0.0, 0.0]);
        let b = to_screen([1.0, 0.0]);
        let c = to_screen([0.0, 1.0]);
        (b - a).length().max((c - a).length()).max(1.0)
    };
    let radius_norm = s.brush / long_edge_screen;
    // the chooser of the selected area: its crop from every frame, a click takes the area from that frame
    let mut chooser_rect: Option<egui::Rect> = None;
    // (not over a dialog: the card lies above the photo and would cover the text of a question)
    let dialog_open = ctx.memory(|m| m.top_modal_layer().is_some());
    if let Some(i) = s.selected.filter(|i| *i < s.settings.regions.len() && !dialog_open) {
        let a = s.settings.regions[i].clone();
        if let (Some(b), Some((meta, _))) = (a.bounds(aspect), rawdev::merged::frames(&info.base_key, info.sources.len())) {
            let [pw, ph] = meta.preview;
            let px = |v: f32, n: usize| ((v * n as f32) as usize).min(n.saturating_sub(1));
            let (x0, y0) = (px(b[0], pw), px(b[1], ph));
            let (x1, y1) = (px(b[2], pw).max(x0 + 1), px(b[3], ph).max(y0 + 1));
            let bounds = [x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)];
            let crops = area_crops(&ctx, info, bounds);
            let screen_bb = egui::Rect::from_two_pos(to_screen([b[0], b[1]]), to_screen([b[2], b[3]]));
            let ar = bounds[2] as f32 / bounds[3] as f32;
            let tile = if ar >= 1.0 { egui::vec2(110.0, 110.0 / ar) } else { egui::vec2(80.0 * ar, 80.0) };
            let n = crops.len();
            let title = ctx.fonts_mut(|f| f.layout_no_wrap(crate::tr("Aus Aufnahme").to_string(), egui::FontId::proportional(10.5), crate::lr::TEXT_STRONG())).size().x;
            // wide enough for the title and the longest label below a tile
            let label_w = ctx.fonts_mut(|f| f.layout_no_wrap("00 · +0,0 ★".to_string(), egui::FontId::proportional(10.5), crate::lr::TEXT_STRONG())).size().x;
            let box_size = egui::vec2(tile.x.max(title).max(label_w) + 20.0, 26.0 + n as f32 * (tile.y + 22.0));
            let mut at = egui::pos2(screen_bb.right() + 14.0, screen_bb.top());
            if at.x + box_size.x > avail.right() - 8.0 {
                at.x = screen_bb.left() - 14.0 - box_size.x;
            }
            at.x = at.x.clamp(avail.left() + 8.0, (avail.right() - box_size.x - 8.0).max(avail.left() + 8.0));
            at.y = at.y.clamp(avail.top() + 8.0, (avail.bottom() - box_size.y - 8.0).max(avail.top() + 8.0));
            let r = egui::Rect::from_min_size(at, box_size);
            chooser_rect = Some(r);
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hdr-chooser"))).with_clip_rect(avail);
            p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x18, 0x18, 0x18, 238));
            p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0x44)), egui::StrokeKind::Inside);
            p.text(r.left_top() + egui::vec2(8.0, 7.0), egui::Align2::LEFT_TOP, crate::tr("Aus Aufnahme"), egui::FontId::proportional(10.5), lr::TEXT_DIM());
            let reference = s.settings.reference.unwrap_or(info.reference);
            let (pos, clicked) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_clicked()));
            for (k, t) in crops.iter().enumerate() {
                let tr = egui::Rect::from_min_size(r.left_top() + egui::vec2(8.0, 24.0 + k as f32 * (tile.y + 22.0)), tile);
                p.image(t.id(), tr, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                let chosen = a.frame == k;
                let hovered = pos.is_some_and(|q| tr.contains(q));
                if chosen || hovered {
                    p.rect_stroke(tr, 0.0, egui::Stroke::new(if chosen { 2.0 } else { 1.0 }, if chosen { crate::lr::TEXT_STRONG() } else { crate::lr::gray(0x99) }), egui::StrokeKind::Outside);
                }
                let ev = info.ev.get(k).map(|e| crate::hdr_ui::ev_label(*e)).unwrap_or_default();
                let label = format!("{} · {}{}", number_of(info, k), ev, if k == reference { " ★" } else { "" });
                p.text(egui::pos2(tr.left(), tr.bottom() + 3.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(10.5), if chosen { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() });
                // (the crops are the frames of the picture shown: while a change of the frames is computed they are not
                // the frames of the recipe any more – the index would name another frame, or none)
                if hovered && clicked && !chosen && info.in_step {
                    s.settings.regions[i].frame = k;
                    changed(&mut s, true, None);
                }
            }
        }
    }
    let over_chooser = |p: Pos2| chooser_rect.is_some_and(|r| r.contains(p));
    let hovering = resp.hover_pos().filter(|p| rect.contains(*p) && avail.contains(*p) && !over_chooser(*p));
    if let Some(pos) = hovering {
        painter.circle_stroke(pos, s.brush, egui::Stroke::new(1.5, crate::lr::overlay(210)));
        painter.circle_stroke(pos, s.brush + 1.5, egui::Stroke::new(1.0, Color32::from_black_alpha(150)));
        painter.line_segment([pos - egui::vec2(5.0, 0.0), pos + egui::vec2(5.0, 0.0)], egui::Stroke::new(1.5, crate::lr::TEXT_STRONG()));
        if !s.erase {
            painter.line_segment([pos - egui::vec2(0.0, 5.0), pos + egui::vec2(0.0, 5.0)], egui::Stroke::new(1.5, crate::lr::TEXT_STRONG()));
        }
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
    let down = ui.input(|i| i.pointer.primary_down());
    let start_ok = ui.input(|i| i.pointer.press_origin()).is_some_and(|p| rect.contains(p) && avail.contains(p) && !over_chooser(p));
    if down && start_ok {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()).filter(|p| rect.contains(*p)) {
            let q = to_image(pos);
            let step = 0.3 * radius_norm;
            if s.pending.last().is_none_or(|l| ((l[0] - q[0]) * aspect.max(1.0)).hypot((l[1] - q[1]) / aspect.min(1.0)) >= step) {
                s.pending.push(q);
            }
        }
    } else if !s.pending.is_empty() {
        let points = std::mem::take(&mut s.pending);
        let stroke = hdr::regions::AreaStroke { erase: s.erase, radius: radius_norm, points };
        match s.selected.filter(|i| *i < s.settings.regions.len()) {
            Some(i) => {
                s.settings.regions[i].strokes.push(stroke);
                if hdr::regions::mask(&s.settings.regions[i], 512, (512.0 / aspect).max(1.0) as usize).is_none() {
                    // taken away completely: the area goes
                    s.settings.regions.remove(i);
                    s.hidden.remove(i);
                    s.selected = None;
                }
                changed(&mut s, true, None);
            }
            None if !s.erase => {
                // a new area, from the reference until another frame is chosen
                let frame = s.settings.reference.unwrap_or(info.reference);
                s.settings.regions.push(hdr::regions::Area { frame, strokes: vec![stroke] });
                s.hidden.push(false);
                s.selected = Some(s.settings.regions.len() - 1);
                changed(&mut s, true, None);
            }
            None => {}
        }
    }
    if !s.pending.is_empty() {
        let c = crate::lr::overlay(if s.erase { 35 } else { 60 });
        for p in &s.pending {
            painter.circle_filled(to_screen(*p), s.brush, c);
        }
        ctx.request_repaint();
    }
    save(&ctx, s);
}

// ─────────────────────────────────────────────── the app's side

impl crate::App {
    /// The open photo as a merged photo (recipe, sources, measured EV), `None` for every other photo.
    pub(crate) fn merge_info(&self) -> Option<Info> {
        let mut info = self.merge_info_plain()?;
        // (looked up in the library twice a second at most: this is asked several times per frame)
        type Cached = (i64, Vec<i64>, Vec<PathBuf>, bool, std::time::Instant, Vec<i64>, Vec<Option<f64>>, Vec<usize>);
        static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
        if let Some((.., list, ev, nums)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|c| c.0 == info.file && c.1 == info.source_files && c.2 == info.sources && c.3 == info.in_step && clock::since(c.4) < std::time::Duration::from_millis(500)) {
            info.list_files = list.clone();
            info.source_exif_ev = ev.clone();
            info.numbers = nums.clone();
            return Some(info);
        }
        let image = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image)?;
        info.list_files = list_files_of(info.in_step, &info.source_files, &info.sources, |p| self.lib.file_at(p));
        let src_rows = self.lib.hdr_rows_of_files(&info.list_files);
        info.source_exif_ev = info.list_files.iter().map(|f| src_rows.iter().find(|r| r.file == *f).and_then(|r| r.exif_ev())).collect();
        let cand: Vec<Option<f64>> = self.lib.hdr_more_candidates(image, &info.list_files).iter().map(|r| r.exif_ev()).collect();
        let src_ev: Vec<Option<f64>> = (0..info.sources.len()).map(|i| info.source_exif_ev.get(i).copied().flatten().or_else(|| info.ev.get(i).copied())).collect();
        let order = list_order(&src_ev, &cand);
        info.numbers = (0..info.sources.len()).map(|i| order.iter().position(|r| *r == Row::Source(i)).unwrap_or(i)).collect();
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((info.file, info.source_files.clone(), info.sources.clone(), info.in_step, clock::now(), info.list_files.clone(), info.source_exif_ev.clone(), info.numbers.clone()));
        Some(info)
    }

    fn merge_info_plain(&self) -> Option<Info> {
        let c = self.current.as_ref()?;
        let m = c.loaded.previews.foreign.as_ref()?.merged.as_ref()?;
        let wanted = crate::hdr_ui::wanted(m.source.file);
        let busy = rawdev::merged::jobs().iter().any(|j| j.file == m.source.file) || crate::hdr_ui::job_alive(m.source.file) || wanted.as_ref().is_some_and(|w| w.key != m.source.key) || change_just_made();
        let base_key = wanted.map_or_else(|| m.source.base_key.clone(), |w| w.base_key);
        let stored = merge_files(m.source.file);
        let sent = sources_on_their_way(m.source.file, &stored.ids);
        // the picture shown is the result of the stored recipe: its frames are where the library has them now. The
        // paths of the document were read when it opened – a source renamed meanwhile (or renamed back before a start
        // and only then followed by the library) kept the photo locked as "not reachable" under its old name until
        // the photo was opened again (QA 11, M-3)
        let shown_sources = if stored.key.as_deref() == Some(m.source.key.as_str()) && stored.paths.len() == m.source.sources.len() { stored.paths.clone() } else { m.source.sources.clone() };
        Some(Info {
            file: m.source.file,
            name: self.entries.get(c.index).map(|e| e.name.clone()).unwrap_or_default(),
            sources: shown_sources.clone(),
            source_files: stored.ids.clone(),
            list_files: Vec::new(),
            settings: m.source.settings.clone(),
            ev: m.meta.summary.ev.clone(),
            reference: m.meta.summary.reference,
            base_key,
            key: m.source.key.clone(),
            tone_now: self.entries.get(c.index).map(|e| crate::hdr_auto::tone_of(&e.settings)).unwrap_or_default(),
            warnings: m.meta.summary.warnings.clone(),
            pending: m.pending,
            full_ready: rawdev::merged::cached(&m.source.key),
            source_exif_ev: Vec::new(),
            numbers: Vec::new(),
            candidates: Vec::new(),
            in_step: lists_in_step(&shown_sources, &stored.paths, &stored.ids, sent.as_deref()),
            // (a comparison shows other settings than stored on purpose: it is wanted, and so is every running change)
            stale: stored.key.as_ref().is_some_and(|k| *k != m.source.key) && crate::hdr_ui::wanted(m.source.file).is_none() && !busy,
            busy,
            // (a result the user cancelled counts as well: it is not started again by itself either)
            failed: stored.key.as_ref().filter(|_| !busy).and_then(|k| {
                // (also with the preview in place: the full resolution of a stopped result is not merged by itself)
                crate::hdr_ui::failed_reason(m.source.file, k).or_else(|| (crate::hdr_ui::stopped(m.source.file, k) && (m.pending || !rawdev::merged::cached(k))).then(|| crate::tr("die Berechnung wurde abgebrochen").to_string()))
            }),
            stored_key: stored.key.clone(),
            created: stored.created,
            offline: m.offline,
            // (the frames of the picture shown and those of the stored recipe: after a change of the frames that is
            // not computed yet they differ, and a source of either one that is gone locks the tool; seed 7260203)
            missing: {
                let mut all = shown_sources.clone();
                for p in &stored.paths {
                    if !all.contains(p) {
                        all.push(p.clone());
                    }
                }
                missing_names(m.source.file, &all)
            },
        })
    }

    /// [`Self::merge_info`] with the photos of the series or stack that are not frames (for the list).
    fn merge_info_full(&mut self) -> Option<Info> {
        let mut info = self.merge_info()?;
        let image = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image)?;
        let rows = self.lib.hdr_more_candidates(image, &info.list_files);
        let ref_ev = info.source_exif_ev.get(info.reference).copied().flatten();
        info.candidates = rows
            .into_iter()
            .map(|r| {
                let exif_ev = r.exif_ev();
                let ev = exif_ev.zip(ref_ev).map(|(a, b)| a - b);
                Candidate { file: r.file, ev, exif_ev, thumb: self.lib.thumbnail(r.file) }
            })
            .collect();
        Some(info)
    }

    /// Panel of the tool "Zusammenfügen".
    pub(crate) fn merge_panel(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.merge_info_full() else {
            ui.label(egui::RichText::new(crate::tr("Dieses Foto ist kein zusammengefügtes Foto.")).color(lr::TEXT_DIM()));
            return;
        };
        if let Some(file) = panel(ui, &info) {
            self.merge_take_in(&info, file);
        }
    }

    /// A photo ticked or dropped on the list: checked like the selection of a new merge, then taken in.
    fn merge_take_in(&mut self, info: &Info, file: i64) {
        if !info.in_step {
            return;
        }
        let mut files = info.source_files.clone();
        files.push(file);
        let rows = self.lib.hdr_rows_of_files(&files);
        // the frames stay in the order of their exposure (numbers and list do not jump)
        let mut files_ev: Vec<(i64, f64)> = files.iter().map(|f| (*f, rows.iter().find(|r| r.file == *f).and_then(|r| r.exif_ev()).unwrap_or(0.0))).collect();
        files_ev.sort_by(|a, b| a.1.total_cmp(&b.1));
        let files: Vec<i64> = files_ev.into_iter().map(|(f, _)| f).collect();
        if rows.len() != files.len() {
            crate::hdr_live::say(crate::tr("Dieses Foto kann keine Aufnahme sein."));
            return;
        }
        if let Some(why) = crate::hdr_ui::refusal(&rows) {
            crate::hdr_live::say(&why);
            return;
        }
        let ctx = self.worker.ctx().clone();
        let mut s = sync(&ctx, info);
        let mut st = s.settings.clone();
        st.reference = Some(st.reference.unwrap_or(info.reference));
        s.settings = remap(&st, &info.source_files, &files);
        sources_sent(info.file, &files);
        changed(&mut s, true, Some(files));
        save(&ctx, s);
        crate::hdr_auto::want(info.file, false, Some(info.key.clone()));
    }

    /// A step of the history brought back another output or strength of the automatic tone (`hdr_auto`): into the
    /// tool's recipe, stored and merged like a change made in the panel.
    pub(crate) fn merge_restore_recipe(&mut self, ctx: &egui::Context, output: hdr::fusion::Output, auto: Option<hdr::AutoTone>) {
        let Some(info) = self.merge_info() else { return };
        let mut s = sync(ctx, &info);
        s.settings.output = output;
        s.settings.auto_tone = auto;
        changed(&mut s, true, None);
        save(ctx, s);
    }

    /// Each frame: a change of the recipe is stored (unless only a comparison) and computed – the newest change cancels
    /// older ones.
    pub(crate) fn merge_update_step(&mut self, ctx: &egui::Context) {
        if let Some(t) = crate::hdr_live::take_said() {
            self.status = t;
        }
        let Some(info) = self.merge_info() else { return };
        for what in std::mem::take(&mut *TEST_FOR_APP.lock().unwrap_or_else(|e| e.into_inner())) {
            if what == "remove" {
                crate::log("hdr test: HDR entfernen (Quellen bleiben)");
                let mut actions = Vec::new();
                self.lib.remove_merge(info.file, &mut actions);
            }
        }
        // "Auswahl zurücknehmen": the recipe takes the frames and settings of the picture shown again
        if RESTORE.lock().unwrap_or_else(|e| e.into_inner()).take() == Some(info.file) && !info.in_step && !info.busy {
            let ids: Vec<i64> = info.sources.iter().filter_map(|p| self.lib.file_at(p)).collect();
            if ids.len() == info.sources.len() && ids.len() >= hdr::MIN_FRAMES {
                let mut s = sync(ctx, &info);
                s.settings = info.settings.clone();
                s.hidden = vec![false; s.settings.regions.len()];
                s.selected = None;
                sources_sent(info.file, &ids);
                changed(&mut s, true, Some(ids));
                save(ctx, s);
            } else {
                crate::hdr_live::say(crate::tr("Die Aufnahmen des gezeigten Bildes sind nicht mehr in der Bibliothek."));
            }
        }
        if let Some(c) = take_change(ctx, &info) {
            *CHANGED_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(clock::now());
            recipe_changed();
            match c.sources {
                Some(files) => self.lib.update_merge_sources(info.file, files, c.save.unwrap_or(c.view), info.name.clone()),
                None => self.lib.update_merge(info.file, c.view, c.save, info.name.clone()),
            }
        }
    }
}

/// Test hook (own data folder only): `GRAINEO_HDR_TEST="fine@1;leave=0@6;leave=2@6.3;remove@12"` does what a click
/// in the panel does, at the given seconds after the panel first showed: `fine` = "Bewegung: Fein", `leave=<row>` =
/// the tick of that frame (subject to the same lock as the click), `remove` = "HDR entfernen (Quellen bleiben)".
/// Returns the actions that are due now, each once.
fn test_actions() -> Vec<(String, Option<usize>)> {
    #[cfg(test)]
    {
        let queued = std::mem::take(&mut *TEST_QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
        if !queued.is_empty() {
            return queued;
        }
    }
    static PLAN: Mutex<Option<(std::time::Instant, Vec<(f32, String, Option<usize>)>)>> = Mutex::new(None);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return Vec::new();
    }
    let Ok(spec) = crate::env::var("GRAINEO_HDR_TEST") else { return Vec::new() };
    let mut g = PLAN.lock().unwrap_or_else(|e| e.into_inner());
    let (start, plan) = g.get_or_insert_with(|| {
        let plan = spec
            .split(';')
            .filter_map(|a| {
                let (what, at) = a.trim().split_once('@')?;
                let (name, arg) = what.split_once('=').map_or((what, None), |(n, v)| (n, v.parse().ok()));
                Some((at.parse().ok()?, name.to_string(), arg))
            })
            .collect();
        (std::time::Instant::now(), plan)
    });
    let now = start.elapsed().as_secs_f32();
    let (due, later): (Vec<_>, Vec<_>) = std::mem::take(plan).into_iter().partition(|a| a.0 <= now);
    *plan = later;
    due.into_iter().map(|(_, n, a)| (n, a)).collect()
}

/// What the test hook clicks in the sections of the panel below the list of frames.
#[derive(Default)]
struct TestClicks {
    ghost: Option<hdr::ghost::Level>,
    output: Option<hdr::fusion::Output>,
    align: bool,
    /// slider "Stärke" in percent
    strength: Option<f64>,
    /// a new area (the number places its stroke)
    paint: Option<usize>,
    /// (area, frame): the crop of that frame clicked for the area
    area_frame: Option<(usize, usize)>,
    eye: Option<usize>,
    delete: Option<usize>,
}

/// Clicks queued by a test in this process (`hdr_stress_test.rs`), carried out by the next panel drawn.
#[cfg(test)]
static TEST_QUEUE: Mutex<Vec<(String, Option<usize>)>> = Mutex::new(Vec::new());

/// Test: do what a click in the panel does (see [`test_actions`] for the names) when the panel is drawn next.
#[cfg(test)]
pub(crate) fn test_click(what: &str, arg: Option<usize>) {
    TEST_QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push((what.to_string(), arg));
}

/// Test: a new session (the short-lived memories of the tool are empty).
#[cfg(test)]
pub(crate) fn test_new_session() {
    TEST_QUEUE.lock().unwrap_or_else(|e| e.into_inner()).clear();
    TEST_FOR_APP.lock().unwrap_or_else(|e| e.into_inner()).clear();
    *CHANGED_AT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *STORED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Test: what the tool holds – (photo, recipe settings, an area is hidden, a change is waiting for the app).
#[cfg(test)]
pub(crate) fn test_state(ctx: &Context) -> (i64, hdr::Settings, bool, bool) {
    let s = state(ctx);
    (s.file, s.settings.clone(), s.hidden.iter().any(|h| *h), s.change.is_some())
}

/// Test: the status line of the panel and whether it offers "Abbrechen".
#[cfg(test)]
pub(crate) fn test_status(info: &Info) -> (String, bool) {
    let (line, running) = status_line(info);
    (line, running.is_some())
}

/// Actions of the test hook that the app carries out (the panel takes the others).
static TEST_FOR_APP: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// When the last change of a recipe went out (it is stored and its job started a moment later).
static CHANGED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// A change went out just now: its job is about to start (the photo is not "out of date" for that moment).
fn change_just_made() -> bool {
    CHANGED_AT.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| clock::since(t) < std::time::Duration::from_secs(3))
}

/// The stored recipe of a merged photo as the tool needs it.
#[derive(Clone, Default)]
struct Stored {
    /// library files of the sources (recipe order)
    ids: Vec<i64>,
    /// their paths now (same order)
    paths: Vec<PathBuf>,
    /// cache key of the stored recipe
    key: Option<String>,
    /// when the recipe was created
    created: i64,
}

static STORED: Mutex<Option<(i64, std::time::Instant, Stored)>> = Mutex::new(None);

/// The stored recipe of a merged photo (read at most twice a second; at once after a change of the frames).
fn merge_files(file: i64) -> Stored {
    let mut g = STORED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((f, t, v)) = g.as_ref() {
        if *f == file && clock::since(*t) < std::time::Duration::from_millis(500) {
            return v.clone();
        }
    }
    let v = library::merge::recipe_of_file(file)
        .map(|(r, paths)| Stored { ids: r.sources.iter().map(|s| s.file).collect(), paths, key: library::merge::keys_for(file, &[&r.settings]).and_then(|mut k| k.pop()), created: r.created })
        .unwrap_or_default();
    *g = Some((file, clock::now(), v.clone()));
    v
}

/// The change that went out arrived: its job took over, or it could not be stored. From here on the jobs and what is
/// wanted say whether something is on its way (the three seconds of [`change_just_made`] only bridge the way there).
pub(crate) fn change_arrived() {
    *CHANGED_AT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The stored recipe of a merged photo changed (or is about to): it is read anew, not taken from the half second kept.
/// Without this a change followed at once by its cancel left the tool comparing the picture with the recipe from
/// before the change.
pub(crate) fn recipe_changed() {
    *STORED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A new list of frames on its way into the recipe (written in the background): (photo, list, since).
static SENT: Mutex<Option<(i64, Vec<i64>, std::time::Instant)>> = Mutex::new(None);

/// A change of the frames goes out: until the recipe holds this list the frames cannot be changed again (the list on
/// screen and the recipe would not belong together), and the recipe is read anew at once.
fn sources_sent(file: i64, files: &[i64]) {
    *SENT.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, files.to_vec(), clock::now()));
    *STORED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The list still on its way for `file` (`None`: the recipe holds it, or the change did not arrive within a few
/// seconds – it failed and was reported).
fn sources_on_their_way(file: i64, stored: &[i64]) -> Option<Vec<i64>> {
    let mut g = SENT.lock().unwrap_or_else(|e| e.into_inner());
    let (f, list, since) = g.as_ref()?;
    if *f != file || list == stored || clock::since(*since) > std::time::Duration::from_secs(5) {
        *g = None;
        return None;
    }
    // the recipe is read again right away, not from the half second kept
    *STORED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Some(list.clone())
}
