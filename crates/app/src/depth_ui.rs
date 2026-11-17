//! Masks "Tiefe" (whole segments of the photo chosen by their distance: near, middle, far) and "Himmel" (sky found
//! automatically): computing them in the background, filling pending leaves, and the settings of such a part (depth
//! classes, segments clicked in or out, soft edge, quality, compute again). The masks only select; they never change
//! pixels.
//!
//! Depth: a depth map stored in the photo (phones, portrait mode) is used when there is one (measured); otherwise
//! the depth is estimated with the module "Tiefe" (Depth Anything V2 Small, Apache-2.0) or, if the user loaded and
//! confirmed it, the optional non-commercial module "Tiefe genauer" (Base). The photo is then cut into segments along
//! colour and depth edges (`segment::segments`); each segment gets the median of its depth and a class (nearest,
//! middle or farthest third). The segment map is stored once as a 16-bit raster; the classes and clicked segments act
//! when rendering, without computing again. A segment is in or out of the mask; the soft edge is spatial only.
//!
//! Sky: depth estimate + click selection (MobileSAM) + optionally a check with the search model SigLIP 2 Base when
//! it is installed (`segment::sky`, `library::sky_check`).
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use develop::Settings;
use egui::Ui;
use masks::{DepthClass, DepthData, Document, Layer, Mask, RasterData, RasterSource};

use crate::analysis_progress::{self as progress, Tool};
use crate::lr;

/// Long side of a stored segment map (16 bit) and of the sky analysis.
pub const DEPTH_SIDE: u32 = 1024;
/// Soft edge of a new depth mask (share of the short side; spatial only).
pub const NEW_FEATHER: f32 = 0.002;
/// Model rule of the standard depth (stored depth map if the photo has one, otherwise the open module).
pub const STANDARD: &str = "depth";

/// What is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Depth,
    Sky,
}

enum Out {
    /// segment map in the mask space, the number of segments, and what the depth came from ("embedded", module id)
    Depth(Arc<DepthData>, usize, &'static str),
    /// sky weights in the mask space; `None`: no sky found
    Sky(Option<Arc<RasterData>>, String),
}

struct Job {
    path: PathBuf,
    what: What,
    model: String,
    result: Option<Result<Out, String>>,
}

static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// What the depth of a photo came from, for the state line (path, raster name, source).
static USED: Mutex<Vec<(PathBuf, String, &'static str)>> = Mutex::new(Vec::new());
/// Photos whose computation was tried in this session (no endless retry of a failing one).
static TRIED: Mutex<Vec<(PathBuf, What)>> = Mutex::new(Vec::new());

/// The open depth module (Small).
pub fn depth_available() -> bool {
    library::module::usable(&library::module::DEPTH)
}

/// The optional non-commercial depth module (Base), loaded and confirmed.
pub fn better_depth_available() -> bool {
    library::module::usable(&library::module::DEPTH_BASE)
}

/// Can a depth mask be made on this system? (A stored depth map needs no module, but only macOS reads it.)
pub fn depth_possible() -> Result<(), &'static str> {
    if depth_available() || cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err("Braucht das Modul „Tiefe“ (Einstellungen → Module)")
    }
}

/// Can the sky be found: needs the click selection and the depth module.
pub fn sky_possible() -> Result<(), &'static str> {
    if !crate::subject_ui::click_available() || !depth_available() {
        Err("Braucht die Module „Klick-Auswahl“ und „Tiefe“ (Einstellungen → Module)")
    } else {
        Ok(())
    }
}

fn busy() -> bool {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.result.is_none())
}

/// Is a depth or sky job running for this photo?
pub fn running(path: &Path, what: What) -> bool {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.path == path && j.what == what && j.result.is_none())
}

/// Name of the tool for [`crate::subject_ui::redo`].
fn tool(what: What) -> &'static str {
    match what {
        What::Depth => "depth",
        What::Sky => "sky",
    }
}

fn what_of(source: &RasterSource) -> Option<What> {
    match source {
        RasterSource::Depth { .. } => Some(What::Depth),
        RasterSource::Scene { .. } => Some(What::Sky),
        _ => None,
    }
}

/// Is `m` a depth or scene leaf (handled here)?
pub fn handles(m: &Mask) -> bool {
    matches!(m, Mask::Raster { source, .. } if what_of(source).is_some())
}

/// A new depth leaf (pending): the near segments.
pub fn depth_leaf() -> Mask {
    Mask::Raster { raster: String::new(), source: RasterSource::Depth { model: STANDARD.into(), classes: vec![DepthClass::Near], toggled: Vec::new() }, feather: NEW_FEATHER, range: None, edge: 0.0 }
}

/// A new sky leaf (pending).
pub fn sky_leaf() -> Mask {
    Mask::Raster { raster: String::new(), source: RasterSource::Scene { part: "sky".into() }, feather: 0.002, range: None, edge: 0.0 }
}

/// New layer with a pending leaf; `false`: 64 layers already.
pub fn new_layer(local: &mut Option<Document>, what: What) -> bool {
    let d = local.get_or_insert(Document { version: 2, layers: vec![] });
    if d.layers.len() >= 64 {
        return false;
    }
    d.layers.push(Layer::new(if what == What::Depth { depth_leaf() } else { sky_leaf() }, masks::Adjustments::default()));
    d.settle_version();
    true
}

/// Start computing `what` for the photo (background). `model`: the depth rule of the leaf.
pub fn start(ctx: &egui::Context, what: What, model: &str, path: &Path, settings: &Settings) {
    {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.result.is_none()) {
            return;
        }
        *g = Some(Job { path: path.to_path_buf(), what, model: model.to_string(), result: None });
    }
    TRIED.lock().unwrap_or_else(|e| e.into_inner()).push((path.to_path_buf(), what));
    let ticket = progress::begin(if what == What::Depth { Tool::Depth } else { Tool::Sky }, path, progress::DEVELOP);
    let (path, settings, ctx, model) = (path.to_path_buf(), settings.clone(), ctx.clone(), model.to_string());
    let failed_path = path.clone();
    let work = move || {
        library::platform::set_priority(library::platform::Priority::UserInitiated);
        let r = crate::subject_ui::guarded("depth/sky mask", || compute(&path, &settings, what, &model, &ticket).and_then(|o| persist(&path, o)));
        if let Err(e) = &r {
            crate::log(&format!("depth/sky mask {}: {e}", path.display()));
        }
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.path == path) {
            j.result = Some(r);
        }
        drop(ticket);
        ctx.request_repaint();
    };
    crate::subject_ui::spawn_job("depth-sky-mask", work, |e| {
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.path == failed_path) {
            j.result = Some(Err(e));
        }
    });
}

/// The photo developed in the mask space (long side [`DEPTH_SIDE`]) and turned upright, with the turn.
struct Upright {
    mw: u32,
    mh: u32,
    uw: u32,
    uh: u32,
    quarter: develop::geometry::Frame,
    image: image::RgbImage,
    /// the same picture in the mask space (not turned)
    small: image::RgbImage,
}

fn upright(path: &Path, settings: &Settings) -> Result<Upright, String> {
    let s = Settings { local: None, ..settings.clone() };
    let (img, camera) = crate::large_preview::render_mask_space(path, &s)?;
    let (w, h) = (img.width, img.height);
    let scale = (DEPTH_SIDE as f32 / w.max(h) as f32).min(1.0);
    let (mw, mh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    let rgb = image::RgbImage::from_raw(w as u32, h as u32, img.to_rgb8()).ok_or("image size")?;
    let small = image::imageops::resize(&rgb, mw, mh, image::imageops::FilterType::Triangle);
    let quarter = develop::geometry::Frame::full(mw as usize, mh as usize, develop::geometry::quarters(&Settings { rotate: settings.rotate, ..Settings::default() }, camera));
    let (uw, uh) = (quarter.w.round() as u32, quarter.h.round() as u32);
    let image = image::RgbImage::from_fn(uw, uh, |x, y| {
        let p = quarter.to_src([(x as f32 + 0.5) / uw as f32, (y as f32 + 0.5) / uh as f32]);
        *small.get_pixel((p[0] as u32).min(mw - 1), (p[1] as u32).min(mh - 1))
    });
    Ok(Upright { mw, mh, uw, uh, quarter, image, small })
}

impl Upright {
    /// Values of the upright picture (any grid `gw × gh` over it) back into the mask space (`mw × mh`).
    fn back(&self, v: &[f32], gw: usize, gh: usize) -> Vec<f32> {
        let up = segment::resize(v, gw, gh, self.uw as usize, self.uh as usize);
        let mut out = vec![0.0f32; (self.mw * self.mh) as usize];
        for y in 0..self.mh {
            for x in 0..self.mw {
                let u = self.quarter.from_src([x as f32 + 0.5, y as f32 + 0.5]);
                let (ux, uy) = (((u[0] * self.uw as f32) as u32).min(self.uw - 1), ((u[1] * self.uh as f32) as u32).min(self.uh - 1));
                out[(y * self.mw + x) as usize] = up[(uy * self.uw + ux) as usize];
            }
        }
        out
    }
}

/// Depth of the upright picture with the chosen module (`None` when it is not loaded).
fn estimate(u: &Upright, model: &str, ticket: &progress::Ticket) -> Result<(segment::depth::Estimate, &'static str), String> {
    let d = if model == library::module::DEPTH_BASE.id && better_depth_available() { &library::module::DEPTH_BASE } else { &library::module::DEPTH };
    if !library::module::usable(d) {
        return Err(crate::tr("Kein Tiefenbild im Foto – Modul „Tiefe“ laden (Einstellungen → Module)").into());
    }
    let e = segment::depth::estimate_steps(&library::module::folder(d), &u.image, segment::depth::SHORT_SIDE, &mut ticket.model_step(progress::DEPTH))?;
    Ok((e, d.id))
}

/// Segment map of the picture in the mask space from its nearness ranks (same size).
fn segments(u: &Upright, ranks: &[u16], ticket: &progress::Ticket) -> Result<(Arc<DepthData>, usize), String> {
    ticket.step(progress::SEGMENTS)?;
    let s = segment::segments::segment(&u.small, ranks, &segment::segments::Params::default());
    Ok((Arc::new(DepthData { width: s.width, height: s.height, data: s.labels.clone() }), s.count()))
}

fn compute(path: &Path, settings: &Settings, what: What, model: &str, ticket: &progress::Ticket) -> Result<Out, String> {
    let t = std::time::Instant::now();
    match what {
        What::Depth => {
            // measured depth stored in the photo first (only with the standard rule: "genauer" asks for the model)
            if model == STANDARD {
                if let Some(e) = segment::embedded::read(path) {
                    // stored pixels = mask space of a photo that is not a RAW; only the size differs
                    let u = upright(path, settings)?;
                    let a = e.width as f32 / e.height as f32;
                    let b = u.mw as f32 / u.mh as f32;
                    if ((a - b) / b).abs() < 0.02 {
                        let v = segment::resize(&e.disparity, e.width as usize, e.height as usize, u.mw as usize, u.mh as usize);
                        let (data, n) = segments(&u, &segment::depth::nearness_ranks(&v), ticket)?;
                        crate::log(&format!("depth mask {}: stored depth map {}×{}, {n} segments in {:.2} s", path.display(), e.width, e.height, t.elapsed().as_secs_f32()));
                        return Ok(Out::Depth(data, n, "embedded"));
                    }
                }
            }
            let u = upright(path, settings)?;
            let developed = t.elapsed().as_secs_f32();
            let (e, used) = estimate(&u, model, ticket)?;
            let v = u.back(&e.disparity, e.width as usize, e.height as usize);
            let estimated = t.elapsed().as_secs_f32();
            let (data, n) = segments(&u, &segment::depth::nearness_ranks(&v), ticket)?;
            crate::log(&format!(
                "depth mask {}: {used}, develop {developed:.2} s, depth {:.2} s, {n} segments {:.2} s",
                path.display(),
                estimated - developed,
                t.elapsed().as_secs_f32() - estimated
            ));
            Ok(Out::Depth(data, n, used))
        }
        What::Sky => {
            let u = upright(path, settings)?;
            let (e, _) = estimate(&u, STANDARD, ticket)?;
            let depth = segment::sky::Depth::from_estimate(&e);
            ticket.step(progress::SKY)?;
            let sam = library::module::folder(&library::module::MOBILESAM);
            // the check only when the search model is installed (never downloaded for this)
            let scorer = library::content_keywords::model_folder().and_then(|d| library::sky_check::Scorer::load(&d).ok());
            let score = scorer.as_ref().map(|s| move |crop: &image::RgbImage| s.sky(crop));
            let found = segment::sky::find(&sam, &u.image, Some(&depth), score.as_ref().map(|f| f as &dyn Fn(&image::RgbImage) -> f32), &mut ticket.model_step(progress::SKY))?;
            crate::log(&format!("sky mask {}: {} in {:.2} s", path.display(), found.note, t.elapsed().as_secs_f32()));
            let checked = if scorer.is_some() { "checked" } else { "unchecked" };
            Ok(Out::Sky(
                found.weights.map(|w| Arc::new(RasterData::from_weights(u.mw, u.mh, &u.back(&w, u.uw as usize, u.uh as usize)))),
                checked.into(),
            ))
        }
    }
}

fn basis(path: &Path, rule: &str, used: &str, w: u32, h: u32) -> serde_json::Value {
    serde_json::json!({"rule": rule, "used": used, "file": path.file_name().map(|n| n.to_string_lossy().to_string()), "app": env!("CARGO_PKG_VERSION"), "aspect": w as f32 / h as f32})
}

/// Store the computed raster in the library (in the job thread): a leaf only names it once it is stored, so that a
/// failed write (library locked, closed meanwhile) never leaves a mask that points at nothing.
fn persist(path: &Path, out: Out) -> Result<Out, String> {
    match &out {
        Out::Depth(data, n, used) => {
            let mut b = basis(path, "depth", used, data.width, data.height);
            b["kind"] = "segments".into();
            b["segments"] = (*n).into();
            library::mask_raster::put_depth(data.width, data.height, &data.data, &b, path).map_err(|e| crate::trf!("Auswahl nicht gespeichert: {e}", e = e))?;
        }
        Out::Sky(Some(data), checked) => crate::subject_ui::store_now(data, path, &basis(path, "sky", checked, data.width, data.height))?,
        Out::Sky(None, _) => {}
    }
    Ok(out)
}

/// Every frame while the mask panel is shown: take a finished job of this photo into its pending leaves. Returns the
/// computation to start when the photo has pending or missing depth/sky leaves that can be computed here.
pub fn update(local: &mut Option<Document>, path: &Path) -> Option<crate::subject_ui::Request> {
    let finished = {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.path == path && j.result.is_some()) { g.take() } else { None }
    };
    if let Some(Job { what, model, result: Some(r), .. }) = finished {
        // leaves computed again ("Neu berechnen", other quality): replaced by a result, else they keep their raster
        let replace = take_redo(path, what);
        match r {
            Ok(Out::Depth(data, _, used)) => {
                let id = masks::raster::remember_depth(data);
                USED.lock().unwrap_or_else(|e| e.into_inner()).push((path.to_path_buf(), id.clone(), used));
                fill(local, What::Depth, Some(&model), &id, &replace);
                crate::subject_ui::set_message(path, crate::tr(if used == "embedded" { "Tiefe aus dem Foto gelesen" } else { "Tiefe geschätzt" }).to_string());
            }
            Ok(Out::Sky(Some(data), _)) => {
                let id = masks::raster::remember(data);
                fill(local, What::Sky, None, &id, &replace);
                crate::subject_ui::set_message(path, crate::tr("Himmel gefunden").to_string());
            }
            Ok(Out::Sky(None, _)) => {
                crate::subject_ui::set_message(path, crate::tr("Kein Himmel erkannt").to_string());
                end_without_result(local, what, &replace);
            }
            Err(e) => {
                let cancelled = progress::is_cancel(&e);
                crate::subject_ui::set_message(path, if cancelled { crate::tr(progress::CANCELLED).to_string() } else { crate::trf!("Berechnung fehlgeschlagen: {e}", e = e) });
                end_without_result(local, what, &replace);
            }
        }
        if local.as_ref().is_some_and(|d| d.layers.is_empty()) {
            *local = None;
        }
        return None;
    }
    let d = local.as_ref()?;
    if busy() {
        return None;
    }
    let tried = TRIED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut want = None;
    for l in &d.layers {
        l.mask.visit(&mut |m| {
            if let Mask::Raster { raster, source, .. } = m {
                if let Some(w) = what_of(source) {
                    let possible = if w == What::Depth { depth_possible().is_ok() } else { sky_possible().is_ok() };
                    if possible && !masks::raster::present(raster, source) && !tried.iter().any(|(p, x)| p == path && *x == w) {
                        let model = match source {
                            RasterSource::Depth { model, .. } => model.clone(),
                            _ => STANDARD.into(),
                        };
                        want.get_or_insert((w, model));
                    }
                }
            }
        });
    }
    want.map(|(w, m)| crate::subject_ui::Request::Scene(w, m))
}

/// A computation ended without a raster (nothing found, cancelled, failed – e.g. no depth map in the photo and no
/// module): a new, unchanged layer that is only a pending leaf of `what` goes away; leaves computed again keep their
/// raster and get back the quality they were computed with.
fn end_without_result(local: &mut Option<Document>, what: What, replace: &[(String, Option<String>)]) {
    let Some(d) = local.as_mut() else { return };
    d.layers.retain(|l| !(matches!(&l.mask, Mask::Raster { raster, source, .. } if raster.is_empty() && what_of(source) == Some(what)) && crate::subject_ui::untouched(l)));
    for l in &mut d.layers {
        l.mask.visit_mut(&mut |m| {
            if let Mask::Raster { raster, source: RasterSource::Depth { model, .. }, .. } = m {
                if let Some((_, Some(old))) = replace.iter().find(|(id, _)| id == raster) {
                    *model = old.clone();
                }
            }
        });
    }
    d.settle_version();
}

/// Put a computed raster into the pending or missing leaves of `what` (depth: only those with the same rule) and into
/// those computed again (`replace`).
fn fill(local: &mut Option<Document>, what: What, model: Option<&str>, id: &str, replace: &[(String, Option<String>)]) {
    let Some(d) = local.as_mut() else { return };
    for l in &mut d.layers {
        l.mask.visit_mut(&mut |m| {
            if let Mask::Raster { raster, source, .. } = m {
                let same = match source {
                    RasterSource::Depth { model: m, .. } => what == What::Depth && model.is_none_or(|x| x == m),
                    RasterSource::Scene { .. } => what == What::Sky,
                    _ => false,
                };
                if same && (!masks::raster::present(raster, source) || replace.iter().any(|(r, _)| r == raster)) {
                    *raster = id.to_string();
                    // clicked segments belonged to the former segment map
                    if let RasterSource::Depth { toggled, .. } = source {
                        toggled.clear();
                    }
                }
            }
        });
    }
    d.settle_version();
}

/// Leaves of depth computed again: raster → the quality (model rule) it was computed with. Shared with the other
/// tools in [`crate::subject_ui::redo`]; the rule is kept here.
static REDO_MODEL: Mutex<Vec<(PathBuf, String, String)>> = Mutex::new(Vec::new());

/// The leaf with `raster` (computed with `model`) of this photo is computed again.
fn redo(path: &Path, what: What, raster: &str, model: Option<&String>) {
    crate::subject_ui::redo(path, tool(what), raster);
    if let Some(m) = model.filter(|_| !raster.is_empty()) {
        let mut g = REDO_MODEL.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|(p, r, _)| !(p == path && r == raster));
        g.push((path.to_path_buf(), raster.to_string(), m.clone()));
    }
}

/// The rasters computed again for this photo, each with its former rule (depth), forgotten.
fn take_redo(path: &Path, what: What) -> Vec<(String, Option<String>)> {
    let ids = crate::subject_ui::take_redo(path, tool(what));
    let mut g = REDO_MODEL.lock().unwrap_or_else(|e| e.into_inner());
    let out = ids.into_iter().map(|id| {
        let m = g.iter().find(|(p, r, _)| p == path && *r == id).map(|e| e.2.clone());
        (id, m)
    }).collect();
    g.retain(|(p, _, _)| p != path);
    out
}

/// Forget that a photo was tried (a click on "Neu berechnen", a new mask).
pub fn retry(path: &Path) {
    TRIED.lock().unwrap_or_else(|e| e.into_inner()).retain(|(p, _)| p != path);
}

fn used_of(path: &Path, id: &str) -> Option<&'static str> {
    USED.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().find(|(p, i, _)| p == path && i == id).map(|e| e.2)
}

fn session_key() -> egui::Id {
    egui::Id::unique("depth_segments_shown")
}

/// Are the segment borders of a selected depth part shown in the image (for the session; on at first)?
pub fn segments_shown(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.get_temp::<bool>(session_key()).unwrap_or(true))
}

/// Toggle the segment under the normalised point `q` (mask space) of the segment map `id`: a segment the classes leave
/// out comes in, one they take goes out (a second click takes it back). `false`: no segment there (map missing).
pub fn toggle_segment(id: &str, toggled: &mut Vec<u16>, q: [f32; 2]) -> bool {
    let Some(map) = masks::raster::resolve_depth(id) else { return false };
    let x = ((q[0] * map.width as f32) as u32).min(map.width - 1);
    let y = ((q[1] * map.height as f32) as u32).min(map.height - 1);
    let Some((_, n)) = masks::raster::split_label(map.data[(y * map.width + x) as usize]) else { return false };
    if let Some(i) = toggled.iter().position(|t| *t == n) {
        toggled.remove(i);
    } else if toggled.len() < masks::raster::TOGGLED_MAX {
        toggled.push(n);
    } else {
        return false;
    }
    true
}

/// Settings of a depth or sky part: state line, depth classes (near, middle, far) and clicked segments or soft edge,
/// quality, compute again.
pub fn controls(ui: &mut Ui, part: &mut Mask, path: &Path) -> Option<crate::subject_ui::Request> {
    let Some(Mask::Raster { raster, source, feather, edge, .. }) = crate::subject_ui::first_raster(part) else { return None };
    let what = what_of(source)?;
    let ok = masks::raster::present(raster, source);
    let state = if running(path, what) {
        crate::tr("Wird berechnet …").to_string()
    } else if raster.is_empty() || !ok {
        match (what, if what == What::Depth { depth_possible() } else { sky_possible() }) {
            (_, Err(why)) => crate::trf!("Auswahl fehlt – {why}", why = crate::tr(why)),
            _ if raster.is_empty() => crate::tr("Auswahl steht aus").to_string(),
            _ => crate::tr("Auswahl fehlt in der Bibliothek").to_string(),
        }
    } else if what == What::Depth {
        match used_of(path, raster) {
            Some("embedded") => crate::tr("Tiefe aus dem Foto (gemessen)").to_string(),
            Some(_) => crate::tr("Tiefe geschätzt").to_string(),
            None => crate::tr("Tiefe berechnet").to_string(),
        }
    } else {
        crate::tr("Himmel berechnet").to_string()
    };
    lr::note(ui, &state);
    let mut request = None;
    if let RasterSource::Depth { model, classes, toggled } = source {
        // whole segments by depth class (several at once); a click on a tile switches it
        let w = lr::visible_width(ui) - lr::TEXT_INSET;
        let gap = ui.spacing().item_spacing.x;
        let bw = ((w - 2.0 * gap) / 3.0).floor();
        ui.horizontal(|ui| {
            ui.add_space(lr::TEXT_INSET);
            for (c, label, tip) in [
                (DepthClass::Near, "Vorne", "Ganze Segmente, deren mittlere Tiefe im nächsten Drittel des Fotos liegt"),
                (DepthClass::Middle, "Mitte", "Ganze Segmente, deren mittlere Tiefe im mittleren Drittel des Fotos liegt"),
                (DepthClass::Far, "Hinten", "Ganze Segmente, deren mittlere Tiefe im fernsten Drittel des Fotos liegt"),
            ] {
                let on = classes.contains(&c);
                let r = ui.add_sized([bw, 24.0], egui::Button::new(crate::tr(label)).selected(on));
                lr::hover_tip(ui, &r, crate::tr(tip));
                if r.clicked() {
                    if on {
                        classes.retain(|x| *x != c);
                    } else {
                        classes.push(c);
                        classes.sort_by_key(|x| *x as u8);
                    }
                }
            }
        });
        // clicked segments: how many, and back to the classes alone (fixed place, locked without clicks)
        ui.horizontal(|ui| {
            ui.add_space(lr::TEXT_INSET);
            let shown = segments_shown(ui.ctx());
            let s = ui.add(egui::Button::new(crate::tr("Segmente zeigen")).selected(shown));
            lr::hover_tip(ui, &s, crate::tr("Grenzen der Segmente im Bild zeigen; ein Klick ins Bild nimmt ein Segment dazu oder heraus"));
            if s.clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(session_key(), !shown));
            }
            let n = toggled.len();
            let r = ui.add_enabled(n > 0, egui::Button::new(crate::trf!("Klicks zurücksetzen ({n})", n = n)));
            lr::hover_tip(ui, &r, crate::tr("Angeklickte Segmente vergessen: nur die gewählten Bereiche zählen"));
            if r.clicked() {
                toggled.clear();
            }
        });
        let mut pct = *feather * 100.0;
        if lr::slider_plain_f32(ui, "Weiche Kante (%)", &mut pct, 0.0..=5.0, NEW_FEATHER * 100.0, 0.05) {
            *feather = (pct / 100.0).clamp(0.0, 0.25);
        }
        let mut pct = *edge * 100.0;
        let max = masks::EDGE_MAX * 100.0;
        if lr::slider_plain_f32(ui, "Kante (%)", &mut pct, -max..=max, 0.0, 0.05) {
            *edge = (pct / 100.0).clamp(-masks::EDGE_MAX, masks::EDGE_MAX);
        }
        // quality: the optional non-commercial module only when loaded and confirmed
        let better = better_depth_available();
        let mut base = *model == library::module::DEPTH_BASE.id;
        ui.horizontal(|ui| {
            ui.add_space(lr::TEXT_INSET);
            ui.label(crate::tr("Qualität"));
            let a = ui.add(egui::Button::new(crate::tr("Standard")).selected(!base));
            lr::hover_tip(ui, &a, crate::tr("Tiefenbild aus dem Foto, sonst geschätzt mit Depth Anything V2 Small (Apache-2.0)"));
            let b = ui.add_enabled(better, egui::Button::new(crate::tr("Genauer")).selected(base));
            lr::hover_tip(ui, &b, crate::tr(if better {
                "Geschätzt mit Depth Anything V2 Base: feinere Kanten, etwa viermal langsamer; nur für nicht-kommerzielle Nutzung"
            } else {
                "Braucht das Modul „Tiefe genauer“ (Einstellungen → Module; nur nicht-kommerziell)"
            }));
            // the segments stay until the new ones are there (cancel, nothing: back to the former quality)
            let next = if a.clicked() && base {
                Some(STANDARD)
            } else if b.clicked() && !base {
                Some(library::module::DEPTH_BASE.id)
            } else {
                None
            };
            if let Some(next) = next {
                base = next != STANDARD;
                if !raster.is_empty() && !busy() {
                    redo(path, What::Depth, raster, Some(model));
                    request = Some(crate::subject_ui::Request::Scene(What::Depth, next.to_string()));
                } else {
                    // computed later (another computation runs): the former segments no longer fit the rule
                    raster.clear();
                    toggled.clear();
                }
                *model = next.into();
            }
        });
        if raster.is_empty() {
            retry(path);
        }
    } else {
        let mut pct = *feather * 100.0;
        if lr::slider_plain_f32(ui, "Weiche Kante (%)", &mut pct, 0.0..=5.0, 0.2, 0.05) {
            *feather = (pct / 100.0).clamp(0.0, 0.25);
        }
        // edge shift of the sky selection (as for subject and people; double-click: 0)
        let mut pct = *edge * 100.0;
        let max = masks::EDGE_MAX * 100.0;
        if lr::slider_plain_f32(ui, "Kante (%)", &mut pct, -max..=max, 0.0, 0.05) {
            *edge = (pct / 100.0).clamp(-masks::EDGE_MAX, masks::EDGE_MAX);
        }
    }
    let possible = (if what == What::Depth { depth_possible() } else { sky_possible() }).is_ok() && !busy();
    ui.horizontal(|ui| {
        ui.add_space(lr::TEXT_INSET);
        let r = ui.add_enabled(possible, egui::Button::new(crate::tr("Neu berechnen")));
        lr::hover_tip(ui, &r, crate::tr("Auswahl neu berechnen (z. B. nach einer geänderten Objektivkorrektur)"));
        if r.clicked() {
            // the selection (and clicked segments) stay until the new one is there; cancel or nothing found: unchanged
            let model = match source {
                RasterSource::Depth { model, .. } => Some(model.clone()),
                _ => None,
            };
            redo(path, what, raster, model.as_ref());
            retry(path);
            request = Some(crate::subject_ui::Request::Scene(what, model.unwrap_or_else(|| STANDARD.into())));
        }
    });
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_layers_are_pending_and_version_4() {
        let mut local = None;
        assert!(new_layer(&mut local, What::Depth));
        assert!(new_layer(&mut local, What::Sky));
        let d = local.as_ref().unwrap();
        assert_eq!(d.version, 4);
        d.validate().unwrap();
        assert!(d.layers.iter().all(|l| handles(&l.mask)));
        assert!(masks::raster::missing(d) == 2);
    }

    #[test]
    fn a_finished_depth_fills_only_leaves_of_its_rule() {
        let mut local = None;
        new_layer(&mut local, What::Depth);
        new_layer(&mut local, What::Depth);
        if let Some(Mask::Raster { source: RasterSource::Depth { model, .. }, .. }) = local.as_mut().map(|d| &mut d.layers[1].mask) {
            *model = library::module::DEPTH_BASE.id.into();
        }
        let data = Arc::new(DepthData { width: 4, height: 2, data: (0..8).map(|i| masks::raster::segment_label(DepthClass::of_rank(i * 8000), i)).collect() });
        let id = masks::raster::remember_depth(data);
        fill(&mut local, What::Depth, Some(STANDARD), &id, &[]);
        let ids = masks::raster::ids(local.as_ref().unwrap());
        assert_eq!(ids, vec![id.clone(), String::new()]);
        // a click toggles the segment under the point, a second click takes it back
        let mut toggled = Vec::new();
        assert!(toggle_segment(&id, &mut toggled, [0.9, 0.9]));
        assert_eq!(toggled, vec![7]);
        assert!(toggle_segment(&id, &mut toggled, [0.9, 0.9]));
        assert!(toggled.is_empty());
        assert!(!toggle_segment("", &mut toggled, [0.5, 0.5]));
        // sky leaves are left alone
        new_layer(&mut local, What::Sky);
        end_without_result(&mut local, What::Sky, &[]);
        assert_eq!(local.as_ref().unwrap().layers.len(), 2);
    }

    /// Review 1.3.0 (logic M2, data safety N3, logic N6): a computation that is cancelled, finds nothing or fails
    /// removes only a new, unchanged layer. A pasted or edited pending layer keeps its settings; a layer computed
    /// again keeps its raster, its clicked segments and its quality; a result replaces it.
    #[test]
    fn ending_without_a_result_keeps_what_the_user_made() {
        let path = Path::new("/x/ending-without-result.jpg");
        let old = masks::raster::remember_depth(Arc::new(DepthData { width: 2, height: 1, data: vec![masks::raster::segment_label(DepthClass::Near, 0), masks::raster::segment_label(DepthClass::Far, 1)] }));
        let mut local = None;
        // 0: fresh pending depth, 1: pasted sky with corrections, 2: computed depth with a name, 3: fresh pending sky
        new_layer(&mut local, What::Depth);
        new_layer(&mut local, What::Sky);
        new_layer(&mut local, What::Depth);
        new_layer(&mut local, What::Sky);
        {
            let d = local.as_mut().unwrap();
            d.layers[1].adjustments.exposure = -1.0;
            d.layers[2].name = Some("Hintergrund".into());
            d.layers[2].amount = 0.8;
            d.layers[2].adjustments.exposure = -0.5;
            if let Mask::Raster { raster, source: RasterSource::Depth { toggled, .. }, .. } = &mut d.layers[2].mask {
                *raster = old.clone();
                toggled.push(1);
            }
        }
        // "Genauer", then cancelled: unchanged, quality back
        let base = library::module::DEPTH_BASE.id.to_string();
        redo(path, What::Depth, &old, Some(&STANDARD.to_string()));
        if let Mask::Raster { source: RasterSource::Depth { model, .. }, .. } = &mut local.as_mut().unwrap().layers[2].mask {
            *model = base.clone();
        }
        end_without_result(&mut local, What::Depth, &take_redo(path, What::Depth));
        let d = local.as_ref().unwrap();
        assert_eq!(d.layers.len(), 3, "only the fresh depth layer went");
        assert!(matches!(&d.layers[1].mask, Mask::Raster { raster, source: RasterSource::Depth { model, toggled, .. }, .. } if *raster == old && model == STANDARD && toggled == &[1]));
        assert_eq!(d.layers[1].name.as_deref(), Some("Hintergrund"));
        // no sky found: the pasted sky with corrections stays (no effect), the fresh one goes
        end_without_result(&mut local, What::Sky, &[]);
        let d = local.as_ref().unwrap();
        assert_eq!(d.layers.len(), 2);
        assert_eq!(d.layers[0].adjustments.exposure, -1.0);
        // computed again with a result: replaced, clicked segments of the former map forgotten
        redo(path, What::Depth, &old, Some(&STANDARD.to_string()));
        let new = masks::raster::remember_depth(Arc::new(DepthData { width: 2, height: 1, data: vec![masks::raster::segment_label(DepthClass::Far, 0); 2] }));
        fill(&mut local, What::Depth, Some(STANDARD), &new, &take_redo(path, What::Depth));
        assert!(matches!(&local.as_ref().unwrap().layers[1].mask, Mask::Raster { raster, source: RasterSource::Depth { toggled, .. }, .. } if *raster == new && toggled.is_empty()));
        assert!(take_redo(path, What::Depth).is_empty());
    }
}
