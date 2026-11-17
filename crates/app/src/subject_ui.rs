//! Bitmap masks from an image analysis: "Motiv" (foreground objects) and "Personen" with the system's image analysis
//! (macOS Vision, `segment`), and the plumbing every raster source shares (computing in the background, storing the
//! raster in the library, filling pending leaves, choosing instances). The mask only selects; it never changes pixels.
//!
//! The analysis runs on a developed image of the mask space (sensor orientation, after lens correction, before crop
//! and rotation; without local corrections), turned upright for the analysis and turned back for the raster, so that
//! the raster follows crop, rotation and perspective like the vector masks. One job at a time, started by a click
//! (or when a photo with a pending subject is shown), at user-initiated priority: a short burst, never a run over the
//! library.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use develop::Settings;
use egui::Ui;
use masks::{Document, Instances, Layer, Mask, RasterData, RasterSource};

use crate::analysis_progress::{self as progress, Tool};
use crate::lr;

/// Long side of a stored raster (pixels).
pub const RASTER_SIDE: u32 = 2048;

/// What the panel asks the caller to start (the caller has the photo's path and settings).
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Analyse(segment::Kind),
    /// click selection with these points and box (MobileSAM)
    Click(Vec<[f32; 3]>, Option<[f32; 4]>),
    /// depth (with the model rule of the leaf) or sky ([`crate::depth_ui`])
    Scene(crate::depth_ui::What, String),
    /// the image colour and brightness ranges compare against ([`crate::mask_sample`])
    Sample,
    /// parts of people ([`crate::face_parts`])
    Parts,
}

/// Encoded photo for the click selection (path, encoding), kept while the user clicks.
static ENCODED: Mutex<Option<(PathBuf, Arc<segment::sam::Encoded>)>> = Mutex::new(None);
/// Result of a click selection: weights, width, height, seconds.
type ClickResult = Result<(Vec<f32>, u32, u32, f32), String>;
/// Prompt of a click selection: points and box.
type Prompt = (Vec<[f32; 3]>, Option<[f32; 4]>);
/// Running or finished click selection: path, prompt, result.
static CLICK: Mutex<Option<(PathBuf, Prompt, Option<ClickResult>)>> = Mutex::new(None);

/// Is the click selection installed (module „Klick-Auswahl“)?
pub fn click_available() -> bool {
    library::module::status(&library::module::MOBILESAM).0 == library::module::Status::Installed
}

/// Runs `f`; a panic inside (an unexpected model output, the inference library) becomes an error. The jobs of the
/// AI tools store their result only at the end: without this a panic left them "busy" until the next start.
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let text = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
        crate::log(&format!("{what}: panic {text}"));
        Err(crate::tr("interner Fehler").to_string())
    })
}

/// Starts `work` on its own thread; if no thread can be started, `failed` gets the error at once.
pub(crate) fn spawn_job(name: &str, work: impl FnOnce() + Send + 'static, failed: impl FnOnce(String)) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(work) {
        crate::log(&format!("{name}: no thread: {e}"));
        failed(crate::tr("interner Fehler").to_string());
    }
}

/// Leaves being computed again ("Neu berechnen", other quality): photo, tool, and the raster they keep until the new
/// one is there. A cancelled computation or one that finds nothing leaves them as they were.
static REDO: Mutex<Vec<(PathBuf, &'static str, String)>> = Mutex::new(Vec::new());

/// Remember that the leaf with `raster` of this photo is computed again by `tool`.
pub(crate) fn redo(path: &Path, tool: &'static str, raster: &str) {
    if !raster.is_empty() {
        let mut g = REDO.lock().unwrap_or_else(|e| e.into_inner());
        if !g.iter().any(|(p, t, r)| p == path && *t == tool && r == raster) {
            g.push((path.to_path_buf(), tool, raster.to_string()));
        }
    }
}

/// The rasters of this photo that `tool` replaces, forgotten (its computation has ended).
pub(crate) fn take_redo(path: &Path, tool: &'static str) -> Vec<String> {
    let mut g = REDO.lock().unwrap_or_else(|e| e.into_inner());
    let (mine, rest): (Vec<_>, Vec<_>) = g.drain(..).partition(|(p, t, _)| p == path && *t == tool);
    *g = rest;
    mine.into_iter().map(|e| e.2).collect()
}

/// Is this layer still as a new mask leaves it (no corrections, own name, switch, amount or refined edge)? Only such
/// a layer goes away when its computation is cancelled or finds nothing; a pasted, synchronised or edited one stays
/// with its settings (its leaf then has no effect until computed).
pub(crate) fn untouched(l: &Layer) -> bool {
    l.adjustments == masks::Adjustments::default() && l.name.is_none() && l.enabled && l.amount == 1.0 && l.refine == 0.0
}

/// Count of raster writes on their way into the library (closing waits for them).
static STORING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Are rasters still being written to the library?
pub fn storing() -> bool {
    STORING.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// Store a raster of `path` in the library now (job threads, never the UI thread): the leaf only names it once it is
/// stored.
pub(crate) fn store_now(data: &RasterData, path: &Path, basis: &serde_json::Value) -> Result<(), String> {
    let r = library::mask_raster::Raster { width: data.width, height: data.height, data: data.data.clone() };
    library::mask_raster::put(&r, basis, path).map(|_| ()).map_err(|e| crate::trf!("Auswahl nicht gespeichert: {e}", e = e))
}

fn click_busy() -> bool {
    CLICK.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.2.is_none())
}

/// Start the click selection for `points` (background; the photo is encoded once, every click only decodes).
pub fn start_click(ctx: &egui::Context, path: &Path, settings: &Settings, points: Vec<[f32; 3]>, rect: Option<[f32; 4]>) {
    {
        let mut g = CLICK.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|c| c.2.is_none()) {
            return;
        }
        *g = Some((path.to_path_buf(), (points.clone(), rect), None));
    }
    let cached = ENCODED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(p, _)| p == path).map(|e| e.1.clone());
    let ticket = progress::begin(Tool::Click, path, if cached.is_some() { progress::DECODE } else { progress::DEVELOP });
    let (path, settings, ctx) = (path.to_path_buf(), settings.clone(), ctx.clone());
    let failed_path = path.clone();
    let work = move || {
        library::platform::set_priority(library::platform::Priority::UserInitiated);
        let dir = library::module::folder(&library::module::MOBILESAM);
        let started = std::time::Instant::now();
        let r = guarded("click selection", || -> ClickResult {
            let enc = match cached {
                Some(e) => e,
                None => {
                    let t = std::time::Instant::now();
                    let s = Settings { local: None, ..settings.clone() };
                    let (img, _) = crate::large_preview::render_mask_space(&path, &s)?;
                    let rgb = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).ok_or("image size")?;
                    let developed = t.elapsed().as_secs_f32();
                    let t = std::time::Instant::now();
                    let e = Arc::new(segment::sam::encode_steps(&dir, &rgb, &mut ticket.model_step(progress::ENCODE))?);
                    crate::log(&format!("click selection {}: develop {developed:.2} s, encode {:.2} s", path.display(), t.elapsed().as_secs_f32()));
                    *ENCODED.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.clone(), e.clone()));
                    e
                }
            };
            ticket.step(progress::DECODE)?;
            let t = std::time::Instant::now();
            let w = segment::sam::decode_prompt(&dir, &enc, &points, rect)?;
            crate::log(&format!("click selection: {} points decoded in {:.2} s", points.len(), t.elapsed().as_secs_f32()));
            Ok((w, enc.width, enc.height, started.elapsed().as_secs_f32()))
        });
        if let Err(e) = &r {
            crate::log(&format!("click selection {}: {e}", path.display()));
        }
        if let Some(c) = CLICK.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|c| c.0 == path) {
            c.2 = Some(r);
        }
        drop(ticket);
        ctx.request_repaint();
    };
    spawn_job("click-selection", work, |e| {
        if let Some(c) = CLICK.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|c| c.0 == failed_path) {
            c.2 = Some(Err(e));
        }
    });
}

/// A click in the image with the click selection on: adds the point to the selected click mask or starts a new one.
/// Returns the index of the layer to select.
pub fn clicked_point(local: &mut Option<Document>, selected: usize, p: [f32; 2], exclude: bool) -> Option<usize> {
    let point = [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0), if exclude { -1.0 } else { 1.0 }];
    let d = local.get_or_insert(Document { version: 2, layers: vec![] });
    if let Some(l) = d.layers.get_mut(selected) {
        if let Some(Mask::Raster { raster, source: RasterSource::Click { points, .. }, .. }) = first_raster(&mut l.mask) {
            if points.len() < 64 {
                points.push(point);
                raster.clear();
            }
            return Some(selected);
        }
    }
    if exclude || d.layers.len() >= 64 {
        return None;
    }
    d.layers.push(Layer::new(Mask::Raster { raster: String::new(), source: RasterSource::Click { points: vec![point], rect: None }, feather: 0.0, range: None, edge: 0.0 }, masks::Adjustments::default()));
    d.settle_version();
    Some(d.layers.len() - 1)
}

/// Take a finished click selection into its leaf (same points); returns a request for a pending click leaf.
fn update_click(local: &mut Option<Document>, path: &Path) -> Option<Request> {
    let done = {
        let mut g = CLICK.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|c| c.0 == path && c.2.is_some()) { g.take() } else { None }
    };
    if let Some((_, (points, rect), Some(r))) = done {
        match r {
            Ok((w, width, height, seconds)) => {
                set_message(path, crate::trf!("Auswahl berechnet · {t} s", t = format!("{seconds:.1}")));
                let data = Arc::new(RasterData::from_weights(width, height, &w));
                let id = masks::raster::remember(data.clone());
                store(data, path, serde_json::json!({"rule": "click", "file": path.file_name().map(|n| n.to_string_lossy().to_string()), "app": env!("CARGO_PKG_VERSION")}));
                if let Some(d) = local.as_mut() {
                    for l in &mut d.layers {
                        l.mask.visit_mut(&mut |m| {
                            if let Mask::Raster { raster, source: RasterSource::Click { points: p, rect: b }, .. } = m {
                                if *p == points && *b == rect {
                                    *raster = id.clone();
                                }
                            }
                        });
                    }
                }
            }
            Err(e) if progress::is_cancel(&e) => set_message(path, crate::tr(progress::CANCELLED).to_string()),
            Err(e) => set_message(path, crate::trf!("Berechnung fehlgeschlagen: {e}", e = e)),
        }
        return None;
    }
    if click_busy() || !click_available() {
        return None;
    }
    let tried = MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|(p, _)| p == path);
    let mut want = None;
    for l in &local.as_ref()?.layers {
        l.mask.visit(&mut |m| {
            if let Mask::Raster { raster, source: RasterSource::Click { points, rect }, .. } = m {
                if raster.is_empty() || masks::raster::resolve(raster).is_none() {
                    want.get_or_insert_with(|| (points.clone(), *rect));
                }
            }
        });
    }
    (!tried).then_some(want.map(|(p, r)| Request::Click(p, r))).flatten()
}

/// Write a raster of `path` to the library in the background (a choice made in the panel; computed rasters are stored
/// in their job with [`store_now`]). Closing waits for it ([`storing`]); a failure is shown in the panel.
pub(crate) fn store(data: Arc<RasterData>, path: &Path, basis: serde_json::Value) {
    use std::sync::atomic::Ordering;
    let path = path.to_path_buf();
    STORING.fetch_add(1, Ordering::SeqCst);
    let spawned = std::thread::Builder::new().name("mask-raster-store".into()).spawn(move || {
        library::platform::set_priority(library::platform::Priority::Utility);
        if let Err(e) = store_now(&data, &path, &basis) {
            crate::log(&format!("mask raster not stored: {e}"));
            set_message(&path, e);
        }
        STORING.fetch_sub(1, Ordering::SeqCst);
    });
    if spawned.is_err() {
        STORING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Last message for this photo (found, nothing found, failed).
pub fn message(path: &Path) -> Option<String> {
    MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(p, _)| p == path).map(|m| m.1.clone())
}

/// Forget the message of a photo (a new click or analysis may try again).
pub fn clear_message() {
    *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Result of one analysis in the mask space: weights per instance, `width × height`.
pub struct Analysis {
    pub kind: segment::Kind,
    pub width: u32,
    pub height: u32,
    pub masks: Vec<Vec<f32>>,
    /// seconds for developing and analysing
    pub seconds: (f32, f32),
}

struct Job {
    path: PathBuf,
    kind: segment::Kind,
    result: Option<Result<Analysis, String>>,
}

static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// The latest analyses (path, kind), kept for choosing instances without analysing again.
static LAST: Mutex<Vec<(PathBuf, Arc<Analysis>)>> = Mutex::new(Vec::new());
/// Last message for the panel (path, text).
static MESSAGE: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

/// Rasters go to and come from the library; set once at start.
pub fn install_provider() {
    develop::spots::set_patch_provider(|id| library::mask_raster::get_rgb(id).map(|(width, height, rgb)| Arc::new(develop::spots::Patch { width, height, rgb })));
    masks::raster::set_provider(|id| library::mask_raster::get(id).map(|r| Arc::new(RasterData { width: r.width, height: r.height, data: r.data })));
    masks::raster::set_depth_provider(|id| library::mask_raster::get_depth(id).map(|(width, height, data)| Arc::new(masks::DepthData { width, height, data })));
}

fn kind_of(source: &RasterSource) -> Option<segment::Kind> {
    match source {
        RasterSource::Subject { .. } => Some(segment::Kind::Subject),
        RasterSource::Person { .. } => Some(segment::Kind::Person),
        _ => None,
    }
}

/// Is an analysis for this photo running? Which kind?
pub fn running(path: &Path) -> Option<segment::Kind> {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|j| j.path == path && j.result.is_none()).map(|j| j.kind)
}

/// Is an analysis running (for any photo)? Then no other can start.
pub fn analysing() -> bool {
    busy()
}

fn busy() -> bool {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.result.is_none())
}

pub(crate) fn set_message(path: &Path, text: String) {
    *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), text));
}

/// Start the analysis of `kind` for the photo (in the background). Nothing happens while another one runs.
pub fn start(ctx: &egui::Context, kind: segment::Kind, path: &Path, settings: &Settings) {
    {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.result.is_none()) {
            return;
        }
        *g = Some(Job { path: path.to_path_buf(), kind, result: None });
    }
    *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ticket = progress::begin(tool_of(kind), path, progress::DEVELOP);
    let (path, settings, ctx) = (path.to_path_buf(), settings.clone(), ctx.clone());
    let failed_path = path.clone();
    let work = move || {
        library::platform::set_priority(library::platform::Priority::UserInitiated);
        let r = guarded("subject mask", || analyse(&path, &settings, kind, Some(&ticket)));
        if let Err(e) = &r {
            crate::log(&format!("subject mask {}: {e}", path.display()));
        }
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.path == path) {
            j.result = Some(r);
        }
        drop(ticket);
        ctx.request_repaint();
    };
    spawn_job("subject-mask", work, |e| {
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.path == failed_path) {
            j.result = Some(Err(e));
        }
    });
}

fn tool_of(kind: segment::Kind) -> Tool {
    match kind {
        segment::Kind::Subject => Tool::Subject,
        segment::Kind::Person => Tool::Person,
    }
}

/// The system's image analysis has run once in this session (per kind): the first run also loads its model.
static WARM: [std::sync::atomic::AtomicBool; 2] = [std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false)];

/// Develop the photo in the mask space (without local corrections), analyse it upright and turn the result back.
/// `ticket`: names the steps and stops between them when cancelled.
pub fn analyse(path: &Path, settings: &Settings, kind: segment::Kind, ticket: Option<&progress::Ticket>) -> Result<Analysis, String> {
    let t = std::time::Instant::now();
    let s = Settings { local: None, ..settings.clone() };
    let (img, camera) = crate::large_preview::render_mask_space(path, &s)?;
    let (w, h) = (img.width, img.height);
    let scale = (RASTER_SIDE as f32 / w.max(h) as f32).min(1.0);
    let (mw, mh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    let rgb = image::RgbImage::from_raw(w as u32, h as u32, img.to_rgb8()).ok_or("image size")?;
    let small = image::imageops::resize(&rgb, mw, mh, image::imageops::FilterType::Triangle);
    // upright for the analysis: only the quarter turns (camera and own), no crop or fine angle
    let quarter = develop::geometry::Frame::full(mw as usize, mh as usize, develop::geometry::quarters(&Settings { rotate: settings.rotate, ..Settings::default() }, camera));
    let (uw, uh) = (quarter.w.round() as u32, quarter.h.round() as u32);
    let upright = image::RgbImage::from_fn(uw, uh, |x, y| {
        let p = quarter.to_src([(x as f32 + 0.5) / uw as f32, (y as f32 + 0.5) / uh as f32]);
        *small.get_pixel((p[0] as u32).min(mw - 1), (p[1] as u32).min(mh - 1))
    });
    let developed = t.elapsed().as_secs_f32();
    let warm = &WARM[kind as usize];
    if let Some(p) = ticket {
        let searching = match kind {
            segment::Kind::Subject => progress::SUBJECT,
            segment::Kind::Person => progress::PEOPLE,
        };
        p.step(if warm.load(std::sync::atomic::Ordering::Relaxed) { searching } else { progress::LOAD })?;
    }
    let t = std::time::Instant::now();
    let found = segment::analyse(&upright, kind)?;
    warm.store(true, std::sync::atomic::Ordering::Relaxed);
    let analysed = t.elapsed().as_secs_f32();
    // back into the mask space
    let masks = found
        .masks
        .iter()
        .map(|m| {
            let mut out = vec![0.0f32; (mw * mh) as usize];
            for y in 0..mh {
                for x in 0..mw {
                    let u = quarter.from_src([x as f32 + 0.5, y as f32 + 0.5]);
                    let (ux, uy) = (((u[0] * uw as f32) as u32).min(uw - 1), ((u[1] * uh as f32) as u32).min(uh - 1));
                    out[(y * mw + x) as usize] = m[(uy * uw + ux) as usize];
                }
            }
            out
        })
        .collect();
    crate::log(&format!("subject mask {}: {:?}, {} instances, develop {developed:.2} s, analysis {analysed:.2} s", path.display(), kind, found.masks.len()));
    Ok(Analysis { kind, width: mw, height: mh, masks, seconds: (developed, analysed) })
}

fn analysis_for(path: &Path, kind: segment::Kind) -> Option<Arc<Analysis>> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().find(|(p, a)| p == path && a.kind == kind).map(|e| e.1.clone())
}

/// Raster for a chosen set of instances: kept in memory at once, written to the library in the background.
fn raster_for(a: &Analysis, instances: &Instances, path: &Path) -> String {
    let chosen: Vec<u8> = match instances {
        Instances::All => Vec::new(),
        Instances::Some(v) => v.clone(),
    };
    let found = segment::Instances { width: a.width, height: a.height, masks: a.masks.clone() };
    let data = Arc::new(RasterData::from_weights(a.width, a.height, &found.union(&chosen)));
    let id = masks::raster::remember(data.clone());
    store(data, path, serde_json::json!({"rule": format!("{:?}", a.kind).to_lowercase(), "file": path.file_name().map(|n| n.to_string_lossy().to_string()), "app": env!("CARGO_PKG_VERSION"), "aspect": a.width as f32 / a.height as f32}));
    id
}

/// Every frame while the mask panel is shown: take a finished analysis of this photo into its pending leaves.
/// Returns a request to analyse when the photo has pending or missing leaves that can be computed here.
pub fn update(local: &mut Option<Document>, path: &Path) -> Option<Request> {
    if let Some(r) = crate::depth_ui::update(local, path) {
        return Some(r);
    }
    if let Some(r) = update_click(local, path) {
        return Some(r);
    }
    let finished = {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.path == path && j.result.is_some()) {
            g.take()
        } else {
            None
        }
    };
    if let Some(Job { kind, result: Some(r), .. }) = finished {
        match r {
            Ok(a) => {
                let a = Arc::new(a);
                {
                    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
                    last.retain(|(p, x)| !(p == path && x.kind == kind));
                    last.push((path.to_path_buf(), a.clone()));
                    if last.len() > 4 {
                        last.remove(0);
                    }
                }
                if a.masks.is_empty() {
                    set_message(path, match kind {
                        segment::Kind::Subject => crate::tr("Kein Motiv erkannt").into(),
                        segment::Kind::Person => crate::tr("Keine Person erkannt").into(),
                    });
                    // no empty new layer (also inverted: background); computed again: the previous raster stays
                    drop_untouched_pending(local, kind);
                    take_redo(path, tool_name(kind));
                } else {
                    set_message(path, crate::trf!("{n} gefunden · {t} s", n = a.masks.len(), t = format!("{:.1}", a.seconds.0 + a.seconds.1)));
                    let replace = take_redo(path, tool_name(kind));
                    fill(local, path, &a, true, &replace);
                }
            }
            Err(e) => {
                let cancelled = progress::is_cancel(&e);
                set_message(path, if cancelled { crate::tr(progress::CANCELLED).to_string() } else { crate::trf!("Berechnung fehlgeschlagen: {e}", e = e) });
                // as with nothing found: no empty new layer; computed again: the previous raster stays
                drop_untouched_pending(local, kind);
                take_redo(path, tool_name(kind));
            }
        }
        if local.as_ref().is_some_and(|d| d.layers.is_empty()) {
            *local = None;
        }
        return None;
    }
    // pending (pasted) or missing rasters with a rule that works here: compute once, when nothing else runs
    let d = local.as_ref()?;
    if busy() {
        return None;
    }
    let mut want = None;
    for l in &d.layers {
        l.mask.visit(&mut |m| {
            if let Mask::Raster { raster, source, .. } = m {
                if let Some(k) = kind_of(source) {
                    if (raster.is_empty() || masks::raster::resolve(raster).is_none()) && segment::available(k) != segment::Support::None && analysis_for(path, k).is_none() {
                        want.get_or_insert(k);
                    }
                }
            }
        });
    }
    let tried = MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|(p, _)| p == path);
    (!tried).then_some(want.map(Request::Analyse)).flatten()
}

fn tool_name(kind: segment::Kind) -> &'static str {
    match kind {
        segment::Kind::Subject => "subject",
        segment::Kind::Person => "person",
    }
}

/// A layer that is only a pending leaf of `kind` and still [`untouched`] goes away (nothing found, cancelled, failed).
fn drop_untouched_pending(local: &mut Option<Document>, kind: segment::Kind) {
    if let Some(d) = local.as_mut() {
        d.layers.retain(|l| !(only_pending(&l.mask, kind) && untouched(l)));
        d.settle_version();
    }
}

/// Is the mask only a pending leaf of `kind` (or that leaf inverted, the background)?
fn only_pending(m: &Mask, kind: segment::Kind) -> bool {
    match m {
        Mask::Raster { raster, source, .. } => raster.is_empty() && kind_of(source) == Some(kind),
        Mask::Invert { mask } => only_pending(mask, kind),
        _ => false,
    }
}

/// Put the rasters of an analysis into the leaves of its kind (`only_pending`: leave computed ones alone, except those
/// named in `replace`, which are computed again).
fn fill(local: &mut Option<Document>, path: &Path, a: &Analysis, only_pending: bool, replace: &[String]) {
    let Some(d) = local.as_mut() else { return };
    for l in &mut d.layers {
        l.mask.visit_mut(&mut |m| {
            if let Mask::Raster { raster, source, .. } = m {
                let instances = match source {
                    RasterSource::Subject { instances } | RasterSource::Person { instances } => instances.clone(),
                    _ => return,
                };
                if kind_of(source) == Some(a.kind) && (!only_pending || raster.is_empty() || replace.contains(raster) || masks::raster::resolve(raster).is_none()) {
                    *raster = raster_for(a, &instances, path);
                }
            }
        });
    }
    d.settle_version();
}

/// New layer with a pending leaf of `kind`, selected by the caller. `false`: 64 layers already.
pub fn new_layer(local: &mut Option<Document>, kind: segment::Kind) -> bool {
    let d = local.get_or_insert(Document { version: 2, layers: vec![] });
    if d.layers.len() >= 64 {
        return false;
    }
    let source = match kind {
        segment::Kind::Subject => RasterSource::Subject { instances: Instances::All },
        segment::Kind::Person => RasterSource::Person { instances: Instances::All },
    };
    d.layers.push(Layer::new(Mask::Raster { raster: String::new(), source, feather: 0.002, range: None, edge: 0.0 }, masks::Adjustments::default()));
    d.settle_version();
    true
}

/// Controls of a raster part: state, instances (chips), soft edge, compute again (adding or removing with the brush is
/// a part of its own, "+ Hinzufügen ▾ / − Abziehen ▾"). Returns a request to analyse again.
pub fn controls(ui: &mut Ui, part: &mut Mask, path: &Path) -> Option<Request> {
    let mut request = None;
    let Some(Mask::Raster { raster, source, feather, edge, .. }) = first_raster(part) else { return None };
    let kind = kind_of(source);
    let ok = masks::raster::resolve(raster).is_some();
    // state line
    let clicking = matches!(source, RasterSource::Click { .. });
    let parts = matches!(source, RasterSource::PersonPart { .. });
    let state = if (kind.is_some_and(|k| running(path) == Some(k))) || (clicking && click_busy()) || (parts && crate::face_parts::running(path)) {
        crate::tr("Wird berechnet …").to_string()
    } else if raster.is_empty() && parts && crate::face_parts::message(path).is_some() {
        crate::tr("Nicht gefunden").to_string()
    } else if raster.is_empty() {
        match kind.map(segment::available) {
            None if parts && crate::face_parts::available().is_err() => crate::tr("Auswahl fehlt – nur auf dem Mac berechenbar").to_string(),
            None if parts => crate::tr("Auswahl steht aus").to_string(),
            None if clicking && !click_available() => crate::tr("Auswahl fehlt – Modul „Klick-Auswahl“ nicht installiert").to_string(),
            None if clicking => crate::tr("Auswahl steht aus").to_string(),
            Some(segment::Support::None) | None => crate::tr("Auswahl fehlt – auf diesem System nicht berechenbar").to_string(),
            _ => crate::tr("Auswahl steht aus").to_string(),
        }
    } else if !ok {
        crate::tr("Auswahl fehlt in der Bibliothek").to_string()
    } else {
        crate::tr("Auswahl berechnet").to_string()
    };
    // always one line (the result itself is in the status line under the buttons), so nothing moves
    lr::note(ui, &state);
    // instances
    if let Some(k) = kind {
        if let Some(a) = analysis_for(path, k).filter(|a| a.masks.len() > 1) {
            let instances = match source {
                RasterSource::Subject { instances } | RasterSource::Person { instances } => instances,
                _ => unreachable!(),
            };
            let mut chosen: Vec<u8> = match instances {
                Instances::All => (1..=a.masks.len() as u8).collect(),
                Instances::Some(v) => v.clone(),
            };
            let mut changed = false;
            ui.horizontal_wrapped(|ui| {
                for i in 1..=a.masks.len().min(64) as u8 {
                    let on = chosen.contains(&i);
                    let name = match k {
                        segment::Kind::Subject => crate::trf!("Motiv {n}", n = i),
                        segment::Kind::Person => crate::trf!("Person {n}", n = i),
                    };
                    if ui.add(egui::Button::new(name).selected(on)).clicked() {
                        if on && chosen.len() > 1 {
                            chosen.retain(|x| *x != i);
                        } else if !on {
                            chosen.push(i);
                        }
                        changed = true;
                    }
                }
            });
            if changed {
                chosen.sort_unstable();
                *instances = if chosen.len() == a.masks.len() { Instances::All } else { Instances::Some(chosen) };
                *raster = raster_for(&a, instances, path);
            }
        }
    }
    // soft edge in per cent of the short side
    let mut pct = *feather * 100.0;
    // double-click resets to the soft edge of a new selection (0.2 %)
    if lr::slider_plain_f32(ui, "Weiche Kante (%)", &mut pct, 0.0..=5.0, 0.2, 0.05) {
        *feather = (pct / 100.0).clamp(0.0, 0.25);
    }
    // edge shift in per cent of the short side: + grows, − shrinks the selection (double-click: 0)
    let mut pct = *edge * 100.0;
    let max = masks::EDGE_MAX * 100.0;
    if lr::slider_plain_f32(ui, "Kante (%)", &mut pct, -max..=max, 0.0, 0.05) {
        *edge = (pct / 100.0).clamp(-masks::EDGE_MAX, masks::EDGE_MAX);
    }
    ui.horizontal(|ui| {
        if parts {
            let r = ui.add_enabled(crate::face_parts::available().is_ok() && !crate::face_parts::busy(), egui::Button::new(crate::tr("Neu berechnen")));
            lr::hover_tip(ui, &r, "Auswahl mit der Bildanalyse neu berechnen (z. B. nach einer geänderten Objektivkorrektur)");
            if r.clicked() {
                // the previous selection stays until the new one is there (cancel, nothing found: unchanged)
                redo(path, crate::face_parts::TOOL, raster);
                crate::face_parts::forget(path);
                request = Some(Request::Parts);
            }
        }
        if let Some(k) = kind {
            let possible = segment::available(k) != segment::Support::None && !busy();
            let r = ui.add_enabled(possible, egui::Button::new(crate::tr("Neu berechnen")));
            lr::hover_tip(ui, &r, "Auswahl mit der Bildanalyse neu berechnen (z. B. nach einer geänderten Objektivkorrektur)");
            if r.clicked() {
                // the previous selection stays until the new one is there (cancel, nothing found: unchanged)
                redo(path, tool_name(k), raster);
                *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
                LAST.lock().unwrap_or_else(|e| e.into_inner()).retain(|(p, a)| !(p == path && a.kind == k));
                request = Some(Request::Analyse(k));
            }
        }
    });
    request
}

/// The first raster leaf of a mask (left to right).
pub fn first_raster(m: &mut Mask) -> Option<&mut Mask> {
    match m {
        Mask::Raster { .. } => Some(m),
        Mask::Combine { left, right, .. } => {
            if left.has_raster() {
                first_raster(left)
            } else {
                first_raster(right)
            }
        }
        Mask::Invert { mask } => first_raster(mask),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_job_reports_an_error_and_frees_the_tool() {
        // the job state is set to a result also when the work panics (once "busy" until the restart)
        let path = PathBuf::from("/private/tmp/graineo-subject-test/panic.jpg");
        *JOB.lock().unwrap_or_else(|e| e.into_inner()) = Some(Job { path: path.clone(), kind: segment::Kind::Subject, result: None });
        assert!(busy());
        let r: Result<Analysis, String> = guarded("test", || panic!("model output of an unexpected shape"));
        assert!(r.is_err());
        JOB.lock().unwrap().as_mut().unwrap().result = Some(r);
        assert!(!busy());
        *JOB.lock().unwrap() = None;
        assert_eq!(guarded("test", || Ok::<_, String>(3)), Ok(3));
    }

    #[test]
    fn analysis_fills_pending_leaves_with_the_chosen_instances() {
        let path = PathBuf::from("/private/tmp/graineo-subject-test/a.jpg");
        let mut local = None;
        assert!(new_layer(&mut local, segment::Kind::Subject));
        assert_eq!(local.as_ref().unwrap().version, 3);
        let a = Analysis { kind: segment::Kind::Subject, width: 4, height: 2, masks: vec![vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0]], seconds: (0.0, 0.0) };
        fill(&mut local, &path, &a, true, &[]);
        let ids = masks::raster::ids(local.as_ref().unwrap());
        let r = masks::raster::resolve(&ids[0]).unwrap();
        assert_eq!(r.data, vec![255, 0, 255, 255, 255, 0, 255, 255]);
        // only the second instance
        if let Mask::Raster { source: RasterSource::Subject { instances }, raster, .. } = &mut local.as_mut().unwrap().layers[0].mask {
            *instances = Instances::Some(vec![2]);
            *raster = raster_for(&a, instances, &path);
        }
        let ids = masks::raster::ids(local.as_ref().unwrap());
        assert_eq!(masks::raster::resolve(&ids[0]).unwrap().data, vec![0, 0, 255, 255, 0, 0, 255, 255]);
        local.unwrap().validate().unwrap();
    }

    /// Review 1.3.0 (logic M2, data safety N3): "Neu berechnen" keeps the selection until the new one is there; a
    /// cancelled or empty analysis removes only a new, unchanged layer, never one with corrections or a computed one.
    #[test]
    fn ending_without_a_result_keeps_what_the_user_made() {
        let path = PathBuf::from("/private/tmp/graineo-subject-test/keep.jpg");
        let a = Analysis { kind: segment::Kind::Subject, width: 2, height: 1, masks: vec![vec![1.0, 0.0]], seconds: (0.0, 0.0) };
        let mut local = None;
        // 0: computed subject with corrections, 1: pasted pending subject with corrections, 2: fresh pending subject
        for _ in 0..3 {
            new_layer(&mut local, segment::Kind::Subject);
        }
        let old = {
            let d = local.as_mut().unwrap();
            d.layers[0].adjustments.exposure = -1.0;
            d.layers[1].adjustments.exposure = 0.5;
            let Mask::Raster { raster, .. } = &mut d.layers[0].mask else { unreachable!() };
            *raster = masks::raster::remember(Arc::new(RasterData { width: 2, height: 1, data: vec![9, 9] }));
            raster.clone()
        };
        // "Neu berechnen", then cancelled or nothing found
        redo(&path, tool_name(segment::Kind::Subject), &old);
        drop_untouched_pending(&mut local, segment::Kind::Subject);
        assert!(!take_redo(&path, tool_name(segment::Kind::Subject)).is_empty());
        let d = local.as_ref().unwrap();
        assert_eq!(d.layers.len(), 2, "only the fresh layer went");
        assert_eq!(masks::raster::ids(d), vec![old.clone(), String::new()]);
        assert_eq!((d.layers[0].adjustments.exposure, d.layers[1].adjustments.exposure), (-1.0, 0.5));
        // with a result: the layer computed again and the pending one get it
        redo(&path, tool_name(segment::Kind::Subject), &old);
        fill(&mut local, &path, &a, true, &take_redo(&path, tool_name(segment::Kind::Subject)));
        let ids = masks::raster::ids(local.as_ref().unwrap());
        assert!(ids.iter().all(|id| !id.is_empty() && *id != old), "{ids:?}");
    }
}
