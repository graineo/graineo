//! Pipeline comparison in the "Sensor" tray (macOS only): "Pipeline: graineo | Apple | Compare" for non-Sigma RAWs.
//!
//! - **graineo** – the normal sensor view (default).
//! - **Apple** – the photo developed by the system RAW engine of macOS (Core Image, [`raw::apple`]).
//! - **Compare** – graineo on the left, Apple on the right, zoom and pan coupled (drawn by `compare_view.rs`).
//!
//! Both sides get comparable settings: white balance and exposure of the photo; "neutral" switches noise reduction,
//! sharpening and lens correction off on both sides, "standard" lets each pipeline use its own defaults (then they do not
//! do the same steps). The Apple side is shown in the same display space as graineo's previews (sRGB, clipped).
//!
//! A pure view: nothing is stored in the photo's development; only the choice of the view (pipeline, steps, pinned
//! decoder version) is remembered in `settings.json` (`appleRawView`). A pinned decoder version the file does not offer
//! (after a macOS update) is replaced by the newest one, with a "!" notice.
//!
//! Rendering runs on one worker thread of its own (never in the UI thread; at most one view rendering at a time, plus
//! at most one warm-up rendering, see [`warmup`]). The engine compiles its GPU shaders once per program version and
//! camera family (3–5 s); the app warms them up quietly in the background after the start (once per app version, the
//! cameras done are kept in `settings.json`, key `appleRawWarmup`).

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use develop::Settings;
use eframe::egui::{self, Color32, Rect, TextureHandle, TextureOptions};
use raw::apple::{self, Params, Probe, Steps};

/// Which pipeline the image area of the sensor tray shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pipeline {
    #[default]
    Graineo,
    Apple,
    Compare,
}

impl Pipeline {
    fn key(self) -> &'static str {
        match self {
            Pipeline::Graineo => "graineo",
            Pipeline::Apple => "apple",
            Pipeline::Compare => "compare",
        }
    }
    fn from_key(k: &str) -> Pipeline {
        match k {
            "apple" => Pipeline::Apple,
            "compare" => Pipeline::Compare,
            _ => Pipeline::Graineo,
        }
    }
}

fn steps_key(s: Steps) -> &'static str {
    match s {
        Steps::Neutral => "neutral",
        Steps::Standard => "standard",
        Steps::Linear => "linear",
    }
}

/// Name of the steps in the pane labels.
fn steps_label(s: Steps) -> &'static str {
    match s {
        Steps::Neutral => crate::tr("neutral"),
        Steps::Standard => crate::tr("Standard"),
        Steps::Linear => crate::tr("linear"),
    }
}

/// Longest edge of the Apple preview (graineo's preview stage has the same size).
pub const PREVIEW_EDGE: usize = 2048;
/// Largest crop at full resolution (float buffer 16 bytes per pixel; larger views keep the preview).
const MAX_CROP_PIXELS: usize = 12_000_000;

/// graineo side of the comparison: the standard development with white balance, exposure and geometry of the photo;
/// "neutral" switches noise reduction, false colour suppression, sharpening and lens correction off (as on the Apple
/// side). Looks, film, masks and the other sliders stay out: Apple has nothing comparable.
pub fn graineo_settings(current: &Settings, steps: Steps) -> Settings {
    let mut s = Settings::default();
    s.exposure = current.exposure;
    s.raw.wb_mode = current.raw.wb_mode.clone();
    s.raw.temperature = current.raw.temperature;
    s.raw.tint = current.raw.tint;
    s.crop = current.crop;
    s.crop_angle = current.crop_angle;
    s.rotate = current.rotate;
    s.perspective_v = current.perspective_v;
    s.perspective_h = current.perspective_h;
    if steps == Steps::Neutral {
        s.raw.nr_luma = 0.0;
        s.raw.nr_color = 0.0;
        s.raw.false_color = 0.0;
        s.raw.lens = false;
        s.raw.lens_ca = false;
        s.sharpen = 0.0;
    }
    s
}

/// Apple side: the same white balance (as shot, or Kelvin/tint of the chosen balance) and exposure.
pub fn apple_params(current: &Settings, temp_tint: (f32, f32), steps: Steps, pin: Option<&str>) -> Params {
    let white = match current.raw.wb_mode.as_str() {
        "as_shot" | "" => None,
        _ => Some(temp_tint),
    };
    Params { decoder: pin.map(str::to_string), steps, exposure_ev: current.exposure as f32, white, max_edge: Some(PREVIEW_EDGE), region: None }
}

/// Region of graineo's full-resolution image (`full`) in pixels of Apple's (`apple`), rounded outward, and the part of
/// graineo's image it covers (for drawing).
pub fn map_region(r: [usize; 4], full: [usize; 2], apple: [usize; 2]) -> ([usize; 4], [f32; 4]) {
    let sx = apple[0] as f64 / full[0].max(1) as f64;
    let sy = apple[1] as f64 / full[1].max(1) as f64;
    let x0 = ((r[0] as f64 * sx).floor() as usize).min(apple[0].saturating_sub(1));
    let y0 = ((r[1] as f64 * sy).floor() as usize).min(apple[1].saturating_sub(1));
    let x1 = (((r[0] + r[2]) as f64 * sx).ceil() as usize).clamp(x0 + 1, apple[0].max(x0 + 1));
    let y1 = (((r[1] + r[3]) as f64 * sy).ceil() as usize).clamp(y0 + 1, apple[1].max(y0 + 1));
    let a = [x0, y0, x1 - x0, y1 - y0];
    (a, [(x0 as f64 / sx) as f32, (y0 as f64 / sy) as f32, ((x1 - x0) as f64 / sx) as f32, ((y1 - y0) as f64 / sy) as f32])
}

/// Text for the UI of an engine error. `camera`: model name for "not supported".
pub fn error_text(e: &apple::Error, camera: &str) -> String {
    match e {
        apple::Error::NotAvailable => crate::tr("Apple-RAW ist auf diesem System nicht verfügbar").to_string(),
        apple::Error::Missing(_) => crate::tr("Datei nicht gefunden").to_string(),
        apple::Error::Unsupported(_) if !camera.is_empty() => crate::trf!("Nicht unterstützt: Kamera {a} (macOS kennt das Modell nicht)", a = camera),
        apple::Error::Unsupported(r) => crate::trf!("Nicht unterstützt: {a}", a = r),
        apple::Error::Failed(r) => crate::trf!("Fehler: {a}", a = r),
    }
}

enum Job {
    Probe(PathBuf),
    Render(PathBuf, Params),
}

enum Reply {
    Probe(PathBuf, Result<Probe, apple::Error>),
    Render(PathBuf, Params, Result<(egui::ColorImage, String, [usize; 2], f64), apple::Error>),
}

struct Worker {
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Reply>,
}

/// The worker thread: one engine, newest request of each kind wins (a crop asked for while panning replaces the one
/// before it). Order: probe, preview, crop.
fn spawn_worker(ctx: egui::Context) -> Option<Worker> {
    let (tx, jobs) = mpsc::channel::<Job>();
    let (done, rx) = mpsc::channel::<Reply>();
    std::thread::Builder::new()
        .name("apple-raw".into())
        .spawn(move || {
            library::platform::set_priority(library::platform::Priority::UserInitiated);
            let mut engine: Option<apple::Engine> = None;
            while let Ok(first) = jobs.recv() {
                let (mut probe, mut preview, mut crop) = (None, None, None);
                for j in std::iter::once(first).chain(jobs.try_iter()) {
                    match j {
                        Job::Probe(p) => probe = Some(p),
                        Job::Render(p, params) if params.region.is_some() => crop = Some((p, params)),
                        Job::Render(p, params) => preview = Some((p, params)),
                    }
                }
                if engine.is_none() {
                    match apple::Engine::new() {
                        Ok(e) => engine = Some(e),
                        Err(e) => {
                            if let Some(p) = probe {
                                let _ = done.send(Reply::Probe(p, Err(e.clone())));
                            }
                            for (p, params) in preview.into_iter().chain(crop) {
                                let _ = done.send(Reply::Render(p, params, Err(e.clone())));
                            }
                            ctx.request_repaint();
                            continue;
                        }
                    }
                }
                let e = engine.as_mut().expect("engine set above");
                if let Some(p) = probe {
                    e.forget();
                    let r = e.probe(&p);
                    // remembered per kind of file (maker, model, size, format) for this system version
                    if let Some(key) = rawdev::apple::support_key_of(&p, r.as_ref().map(|x| x.native).unwrap_or([0, 0])) {
                        rawdev::apple::support_note(key, &r);
                    }
                    let _ = done.send(Reply::Probe(p, r));
                    ctx.request_repaint();
                }
                for (p, params) in preview.into_iter().chain(crop) {
                    let r = e.render(&p, &params).map(|r| {
                        let rgba = apple::to_srgb8(&r.rgba);
                        (egui::ColorImage::from_rgba_unmultiplied([r.width, r.height], &rgba), r.decoder, r.extent, r.millis)
                    });
                    if let Err(err) = &r {
                        crate::log(&format!("apple raw: {} – {err}", p.display()));
                    }
                    let _ = done.send(Reply::Render(p, params, r));
                    ctx.request_repaint();
                }
            }
        })
        .ok()?;
    Some(Worker { tx, rx })
}

/// State of the probe of the current photo.
#[derive(Default)]
enum ProbeState {
    #[default]
    None,
    Pending,
    Done(Result<Probe, apple::Error>),
}

/// State of the comparison view (one per app).
#[derive(Default)]
pub struct AppleView {
    pub pipeline: Pipeline,
    pub steps: Steps,
    /// pinned decoder version (display only); `None` = the newest
    pub pin: Option<String>,
    loaded: bool,
    worker: Option<Worker>,
    path: Option<PathBuf>,
    probe: ProbeState,
    preview: Option<(Params, TextureHandle)>,
    preview_pending: Option<Params>,
    /// crop at full resolution: parameters, texture, covered part of graineo's image
    detail: Option<(Params, TextureHandle, [f32; 4])>,
    detail_pending: Option<(Params, [f32; 4])>,
    /// the last preview and crop that failed: not asked again until the parameters (or the photo) change, otherwise a
    /// failing engine would render again every frame
    preview_failed: Option<Params>,
    detail_failed: Option<Params>,
    /// last rendering: decoder used, milliseconds, preview (false) or crop (true)
    last: Option<(String, f64, bool)>,
    /// the first rendering of this program run (may include the engine's shader compilation)
    rendered_once: bool,
    first_ms: Option<f64>,
    error: Option<String>,
    warm_started: bool,
    test_done: bool,
    /// test hook: engine of the photo on screen to set once (`engine=apple:8`)
    test_engine: Option<String>,
    /// version of the photo's own Apple development (the comparison uses it instead of the view's pin)
    dev_version: Option<String>,
}

impl AppleView {
    /// Remembered choice of the view (once).
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if !cfg!(target_os = "macos") {
            return;
        }
        let v = library::settings::value("appleRawView");
        // the Apple-only view became the development itself ("Entwicklung: Apple"); the view is photo or comparison
        self.pipeline = match Pipeline::from_key(v.get("pipeline").and_then(|x| x.as_str()).unwrap_or("")) {
            Pipeline::Apple => Pipeline::Graineo,
            p => p,
        };
        self.steps = if v.get("steps").and_then(|x| x.as_str()) == Some("standard") { Steps::Standard } else { Steps::Neutral };
        self.pin = v.get("decoder").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    }

    /// Remember the choice (in the background: never write files in the UI thread).
    fn save(&self) {
        let v = serde_json::json!({ "pipeline": self.pipeline.key(), "steps": steps_key(self.steps), "decoder": self.pin.clone().unwrap_or_default() });
        std::thread::spawn(move || library::settings::set("appleRawView", v));
    }

    /// Is the comparison drawn instead of the single image? `raw_view`: the sensor tray shows a non-Sigma RAW.
    pub fn showing(&self, raw_view: bool) -> bool {
        cfg!(target_os = "macos") && raw_view && self.pipeline != Pipeline::Graineo
    }

    /// Probe of the current photo, if done and successful.
    pub fn probe(&self) -> Option<&Probe> {
        match &self.probe {
            ProbeState::Done(Ok(p)) => Some(p),
            _ => None,
        }
    }

    /// Decoder version used for the current settings (`None` before the probe).
    pub fn decoder(&self) -> Option<apple::VersionChoice> {
        let p = self.probe()?;
        apple::choose_version(&p.versions, &p.engine_default, self.dev_version.as_deref().or(self.pin.as_deref()))
    }

    fn worker(&mut self, ctx: &egui::Context) -> Option<&Worker> {
        if self.worker.is_none() {
            self.worker = spawn_worker(ctx.clone());
        }
        self.worker.as_ref()
    }

    /// Every frame: pick up results; on another photo start over (probe in the background).
    pub fn update(&mut self, ctx: &egui::Context, path: Option<&Path>) {
        self.load();
        self.test_hook();
        if let Some(w) = &self.worker {
            while let Ok(r) = w.rx.try_recv() {
                match r {
                    Reply::Probe(p, res) if Some(&p) == self.path.as_ref() => {
                        if let Err(e) = &res {
                            crate::log(&format!("apple raw: {} – {e}", p.display()));
                        }
                        self.probe = ProbeState::Done(res);
                    }
                    Reply::Render(p, params, res) if Some(&p) == self.path.as_ref() => {
                        let crop = params.region.is_some();
                        let src = self.detail_pending.as_ref().filter(|(q, _)| *q == params).map(|(_, s)| *s);
                        if crop {
                            self.detail_pending = None;
                        } else if self.preview_pending.as_ref() == Some(&params) {
                            self.preview_pending = None;
                        }
                        // remembered per parameters: the pane asks again only when they change
                        let failed = res.is_err().then(|| params.clone());
                        if crop {
                            self.detail_failed = failed;
                        } else {
                            self.preview_failed = failed;
                        }
                        match res {
                            Ok((img, decoder, _extent, ms)) => {
                                crate::log(&format!("apple raw: {} {} × {} with decoder {decoder} in {ms:.0} ms", if crop { "crop" } else { "preview" }, img.size[0], img.size[1]));
                                if !self.rendered_once {
                                    self.rendered_once = true;
                                    self.first_ms = Some(ms);
                                }
                                self.last = Some((decoder, ms, crop));
                                self.error = None;
                                if crop {
                                    if let Some(src) = src {
                                        let tex = ctx.load_texture("apple-raw-detail", img, TextureOptions::NEAREST);
                                        self.detail = Some((params, tex, src));
                                    }
                                } else {
                                    match &mut self.preview {
                                        Some((q, t)) => {
                                            t.set(img, TextureOptions::LINEAR);
                                            *q = params;
                                        }
                                        None => self.preview = Some((params, ctx.load_texture("apple-raw-preview", img, TextureOptions::LINEAR))),
                                    }
                                }
                            }
                            Err(e) => self.error = Some(error_text(&e, "")),
                        }
                    }
                    _ => {}
                }
            }
        }
        // probed for every RAW in the sensor tray (in the background): the section greys out what the engine cannot do
        let wanted = path.map(Path::to_path_buf);
        if wanted.is_some() && wanted != self.path {
            self.path = wanted.clone();
            self.probe = ProbeState::Pending;
            self.preview = None;
            self.preview_pending = None;
            self.detail = None;
            self.detail_pending = None;
            self.preview_failed = None;
            self.detail_failed = None;
            self.last = None;
            self.error = None;
            let p = wanted.expect("checked");
            if let Some(w) = self.worker(ctx) {
                let _ = w.tx.send(Job::Probe(p));
            } else {
                self.probe = ProbeState::Done(Err(apple::Error::Failed("no worker thread".into())));
            }
        }
    }

    /// Draw the Apple pane (`r`: image rectangle, `rect`: pane) and request what is missing. `region`: visible part of
    /// graineo's full-resolution image (`full`) when zoomed beyond the preview; `settled`: the view has stopped moving.
    /// Returns: still loading.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_pane(&mut self, ctx: &egui::Context, painter: &egui::Painter, r: Rect, rect: Rect, frame: &develop::geometry::Frame, full: [usize; 2], base: &Params, region: Option<[usize; 4]>, settled: bool, now: f64, camera: &str) -> bool {
        let clip = painter.with_clip_rect(rect);
        let full_src = [0.0, 0.0, full[0] as f32, full[1] as f32];
        let probe = match &self.probe {
            ProbeState::Done(Ok(p)) => p.clone(),
            ProbeState::Done(Err(e)) => {
                let text = error_text(e, camera);
                pane_message(&clip, r.intersect(rect), &text);
                return false;
            }
            _ => {
                crate::spinner(painter, r.intersect(rect).center(), 6.5, now, 1.0);
                return true;
            }
        };
        let mut loading = false;
        let path = self.path.clone();
        let want = Params { max_edge: Some(PREVIEW_EDGE), region: None, ..base.clone() };
        let preview_failed = self.preview_failed.as_ref() == Some(&want);
        if should_request(self.preview.as_ref().map(|(p, _)| p), self.preview_pending.as_ref(), self.preview_failed.as_ref(), &want) {
            if let (Some(p), Some(w)) = (path.clone(), self.worker(ctx)) {
                let _ = w.tx.send(Job::Render(p, want.clone()));
                self.preview_pending = Some(want.clone());
            }
        }
        match &self.preview {
            Some((p, t)) => {
                crate::paint_src_quad(&clip, t.id(), full_src, frame, r, Color32::WHITE);
                if *p != want && !preview_failed {
                    loading = true;
                }
            }
            None if preview_failed => {
                // failed for these parameters: say so (the status row has the error) instead of waiting
                let text = self.error.clone().unwrap_or_else(|| crate::tr("Fehler").to_string());
                pane_message(&clip, r.intersect(rect), &text);
            }
            None => {
                clip.rect_filled(r.intersect(rect), 0.0, Color32::from_gray(24));
                crate::spinner(painter, r.intersect(rect).center(), 6.5, now, 1.0);
                loading = true;
            }
        }
        if let (Some(reg), Some((p, _))) = (region, &self.preview) {
            if p.filter_key() == want.filter_key() {
                let req_region = crate::region_with_margin(reg, 0.25, full);
                let (a, src) = map_region(req_region, full, probe.native);
                let crop = Params { max_edge: None, region: Some(a), ..base.clone() };
                if let Some((q, t, s)) = &self.detail {
                    if q.filter_key() == want.filter_key() {
                        crate::paint_src_quad(&clip, t.id(), *s, frame, r, Color32::WHITE);
                    }
                }
                let covered = self.detail.as_ref().is_some_and(|(q, _, s)| q.filter_key() == want.filter_key() && covers(*s, reg));
                let pending = self.detail_pending.as_ref().is_some_and(|(q, _)| q.filter_key() == want.filter_key());
                // this crop failed: the preview stays until the view or the parameters change
                let crop_failed = self.detail_failed.as_ref() == Some(&crop);
                if !covered && !crop_failed && a[2] * a[3] <= MAX_CROP_PIXELS {
                    loading = true;
                    if settled && !pending {
                        if let (Some(pp), Some(w)) = (path, self.worker(ctx)) {
                            let _ = w.tx.send(Job::Render(pp, crop.clone()));
                            self.detail_pending = Some((crop, src));
                        }
                    }
                }
            }
        }
        if loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        loading
    }

    /// Label of the Apple pane: "Apple RAW 8 · neutral".
    pub fn pane_label(&self) -> String {
        match self.decoder() {
            Some(c) => crate::trf!("Apple RAW {a} · {b}", a = apple::version_label(&c.used), b = steps_label(self.steps)),
            None => crate::tr("Apple RAW").to_string(),
        }
    }

    pub fn graineo_label(&self) -> String {
        crate::trf!("graineo · {a}", a = steps_label(self.steps))
    }

    /// Status line: what happens or why there is no picture.
    fn status(&self, camera: &str) -> (String, bool) {
        match &self.probe {
            ProbeState::None => ("–".to_string(), false),
            ProbeState::Pending => (crate::tr("wird geprüft …").to_string(), false),
            ProbeState::Done(Err(e)) => (error_text(e, camera), true),
            ProbeState::Done(Ok(_)) => {
                if let Some(e) = &self.error {
                    (e.clone(), true)
                } else if self.preview_pending.is_some() || self.detail_pending.is_some() {
                    (crate::tr("wird entwickelt …").to_string(), false)
                } else if self.preview.is_some() {
                    (crate::tr("fertig").to_string(), false)
                } else {
                    ("–".to_string(), false)
                }
            }
        }
    }

    /// Test hook (only with `GRAINEO_DATA_DIR`): `GRAINEO_APPLE_TEST="pipeline=compare;steps=standard;version=7"`;
    /// `engine=apple:5` sets the document field of the photo on screen once (`none` = automatic).
    fn test_hook(&mut self) {
        if self.test_done {
            return;
        }
        self.test_done = true;
        if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let Ok(spec) = crate::env::var("GRAINEO_APPLE_TEST") else { return };
        for part in spec.split(';') {
            match part.split_once('=') {
                Some(("pipeline", v)) => self.pipeline = Pipeline::from_key(v),
                Some(("steps", v)) => self.steps = if v == "standard" { Steps::Standard } else { Steps::Neutral },
                Some(("version", v)) => self.pin = Some(v.to_string()).filter(|v| !v.is_empty()),
                Some(("engine", v)) => self.test_engine = Some(v.to_string()),
                _ => {}
            }
        }
    }

    /// Start the shader warm-up once per program run, 20 s after the start (see [`warmup`]).
    pub fn warmup_tick(&mut self, now: f64) {
        if self.warm_started || now < 20.0 || !cfg!(target_os = "macos") {
            return;
        }
        self.warm_started = true;
        if crate::env::var("GRAINEO_APPLE_WARMUP").as_deref() == Ok("0") {
            return;
        }
        let _ = std::thread::Builder::new().name("apple-raw-warmup".into()).spawn(warmup);
    }
}

/// Should the rendering `want` be asked for? Not while it is shown or on its way, and not again after it failed for
/// exactly these parameters (asked again only when they change).
fn should_request(shown: Option<&Params>, pending: Option<&Params>, failed: Option<&Params>, want: &Params) -> bool {
    shown != Some(want) && pending != Some(want) && failed != Some(want)
}

/// Does the covered part `s` (graineo pixels) contain the visible region `r`?
fn covers(s: [f32; 4], r: [usize; 4]) -> bool {
    s[0] <= r[0] as f32 + 0.5 && s[1] <= r[1] as f32 + 0.5 && s[0] + s[2] + 0.5 >= (r[0] + r[2]) as f32 && s[1] + s[3] + 0.5 >= (r[1] + r[3]) as f32
}

/// A message in a pane without a picture (not supported, error).
fn pane_message(painter: &egui::Painter, area: Rect, text: &str) {
    painter.rect_filled(area, 0.0, Color32::from_gray(24));
    let g = painter.layout(text.to_string(), egui::FontId::proportional(13.0), crate::lr::TEXT, (area.width() - 40.0).max(120.0));
    painter.galley(area.center() - g.size() / 2.0, g, crate::lr::TEXT);
}

/// Shader warm-up: one small rendering (neutral and standard) per camera model of the library that the system engine
/// knows, quietly in the background (lowest priority), once per app version. Cameras done (or not supported) are kept in
/// `settings.json` (`appleRawWarmup`: app version and models); after an update everything is warmed again.
fn warmup() {
    library::platform::background_priority();
    let version = env!("CARGO_PKG_VERSION");
    let v = library::settings::value("appleRawWarmup");
    let mut done: Vec<String> = if v.get("version").and_then(|x| x.as_str()) == Some(version) {
        v.get("cameras").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let samples = match library::db::Db::standard().and_then(|db| db.raw_camera_samples(16)) {
        Ok(s) => s,
        Err(e) => {
            crate::log(&format!("apple raw warm-up: library not readable ({e})"));
            return;
        }
    };
    let todo: Vec<(String, PathBuf)> = samples.into_iter().filter(|(c, _)| !done.contains(c)).collect();
    if todo.is_empty() {
        return;
    }
    let mut engine = match apple::Engine::new() {
        Ok(e) => e,
        Err(e) => {
            crate::log(&format!("apple raw warm-up: {e}"));
            return;
        }
    };
    for (camera, path) in todo {
        if !path.is_file() {
            continue;
        }
        let t = std::time::Instant::now();
        match engine.probe(&path) {
            Ok(_) => {
                for steps in [Steps::Neutral, Steps::Standard] {
                    if let Err(e) = engine.render(&path, &Params { steps, max_edge: Some(256), ..Default::default() }) {
                        crate::log(&format!("apple raw warm-up: {camera}: {e}"));
                    }
                }
                engine.forget();
                crate::log(&format!("apple raw warm-up: {camera} in {:.1} s", t.elapsed().as_secs_f64()));
            }
            Err(apple::Error::Unsupported(_)) => crate::log(&format!("apple raw warm-up: {camera} not supported by the system engine")),
            Err(e) => {
                crate::log(&format!("apple raw warm-up: {camera}: {e}"));
                continue;
            }
        }
        done.push(camera);
        library::settings::set("appleRawWarmup", serde_json::json!({ "version": version, "cameras": done }));
    }
}

/// The automatic fallback pins the version it used (`pin`) in the photo's settings. It is not an edit of the user: the
/// committed state follows (no step in the develop history), and so does the base of "Apply to the selection"
/// (otherwise the pin counted as a change of the active photo and went onto every other selected photo, switching them
/// to the Apple engine).
pub(crate) fn pin_fallback(settings: &mut Settings, committed: Option<&mut Settings>, multi_base: Option<&mut Settings>, pin: &str) {
    for s in std::iter::once(settings).chain(committed).chain(multi_base) {
        s.raw.engine = Some(pin.to_string());
    }
}

/// Does the loaded photo have the engine its settings call for (`RawAdjust::engine`)? Always for Sigma and finished
/// images.
pub fn engine_fits(loaded: &crate::jobs::Loaded, s: &Settings) -> bool {
    loaded.previews.foreign.as_ref().is_none_or(|d| d.engine_fits(s))
}

impl crate::App {
    /// Each frame: an automatic fallback to the Apple engine pins the version it used in the document (first
    /// development); if the engine of the photo on screen changed (switch, paste, undo), open it again. Nothing is
    /// moved: all geometry is stored relative to the image.
    pub(crate) fn engine_check(&mut self) {
        if self.loading.is_some() {
            return;
        }
        let Some(c) = self.current.as_ref() else { return };
        let index = c.index;
        let Some(doc) = c.loaded.previews.foreign.clone() else { return };
        if let Some(e) = self.apple.test_engine.take() {
            self.entries[index].settings.raw.engine = Some(e).filter(|e| e != "none");
        }
        if let Some(a) = doc.apple.as_ref().filter(|a| a.reason == rawdev::apple::Reason::Fallback) {
            if doc.engine_request.is_none() && self.entries[index].settings.raw.engine.is_none() {
                let pin = a.pin_value();
                crate::log(&format!("engine of photo {index}: automatic fallback, {pin} pinned"));
                let image = self.entries[index].image;
                let committed = self.committed.as_mut().filter(|(ci, _)| *ci == index).map(|(_, cs)| cs);
                let base = self.multi_base.as_mut().filter(|(b, _)| *b == image).map(|(_, s)| s);
                pin_fallback(&mut self.entries[index].settings, committed, base, &pin);
            }
        }
        if doc.engine_fits(&self.entries[index].settings) {
            return;
        }
        crate::log(&format!("engine of photo {index}: {:?} → {:?}, opened again", doc.engine_request, self.entries[index].settings.raw.engine));
        self.save_current();
        self.cache.retain(|(i, _)| *i != index);
        self.current = None;
        self.select(index, true);
    }

    /// Conditions of the engine for the photo on screen (image info, toolbar mark): developed by the Apple engine
    /// because graineo cannot decode the file, a pinned version that is missing, Apple chosen but not available.
    pub(crate) fn engine_notices(&self, i: usize) -> Vec<crate::photo_state::Notice> {
        let Some(c) = self.current.as_ref().filter(|c| c.index == i) else { return Vec::new() };
        let Some(doc) = c.loaded.previews.foreign.as_ref() else { return Vec::new() };
        let choice = rawdev::apple::Choice::parse(self.entries[i].settings.raw.engine.as_deref());
        let mut v = Vec::new();
        if let Some(a) = doc.apple.as_ref() {
            if a.graineo_blocked.is_some() {
                v.push(crate::photo_state::engine_fallback(&apple::version_label(&a.used)));
            }
        }
        let blocked = if doc.apple.is_none() && choice.is_apple() { self.apple_blocked() } else { None };
        for item in engine_notice_items(doc.apple.as_deref(), &choice, blocked.as_deref()) {
            v.push(crate::photo_state::engine_missing(&item.title, &item.text));
        }
        v
    }

    /// Is the pipeline comparison drawn (sensor tray, non-Sigma RAW, "Apple" or "Compare")?
    pub(crate) fn apple_showing(&self) -> bool {
        self.apple.showing(self.sensor_raw_active())
    }

    /// Every frame: results of the worker, probe of a new photo, warm-up after the start.
    pub(crate) fn apple_update(&mut self, ctx: &egui::Context) {
        let path = self.sensor_raw_active().then(|| self.current.as_ref().map(|c| self.entries[c.index].path.clone())).flatten();
        self.apple.dev_version = self.current.as_ref().and_then(|c| c.loaded.previews.foreign.as_ref()).and_then(|d| d.apple.as_ref()).map(|a| a.used.clone());
        self.apple.update(ctx, path.as_deref());
        let now = ctx.input(|i| i.time);
        self.apple.warmup_tick(now);
    }

    /// Parameters of the Apple side for the current photo.
    pub(crate) fn apple_params(&self) -> Option<Params> {
        let c = self.current.as_ref()?;
        let doc = c.loaded.previews.foreign.as_ref()?;
        let s = &self.entries[c.index].settings;
        // the version actually used (a missing pin falls back to the newest, see `AppleView::decoder`)
        let used = self.apple.decoder().map(|d| d.used);
        Some(apple_params(s, doc.temp_tint(s), self.apple.steps, used.as_deref()))
    }

    /// Camera model of the current photo (for "not supported").
    pub(crate) fn apple_camera(&self) -> String {
        self.current.as_ref().and_then(|c| c.loaded.previews.foreign.as_ref()).map(|d| raw::camera_display(&d.info.maker, &d.info.model)).unwrap_or_default()
    }

    /// Why the Apple engine cannot develop the photo on screen (the choice is greyed out, the tooltip says this);
    /// `None` = possible. Sigma/Foveon, other systems, cameras the engine does not know, while the photo is checked.
    pub(crate) fn apple_blocked(&self) -> Option<String> {
        if !cfg!(target_os = "macos") {
            return Some(crate::tr("Apple RAW gibt es nur unter macOS – hier entwickelt graineo").to_string());
        }
        let c = self.current.as_ref()?;
        let path = &self.entries[c.index].path;
        if crate::image_kind::of(path) == crate::image_kind::ImageKind::Foveon {
            return Some(crate::tr("Für Sigma/Foveon nur graineo – Apple kennt das Format nicht").to_string());
        }
        if crate::image_kind::of(path) != crate::image_kind::ImageKind::Raw {
            return Some(crate::tr("Nur für RAW-Dateien").to_string());
        }
        if c.loaded.previews.foreign.as_ref().is_some_and(|d| d.apple.is_some()) {
            return None;
        }
        match &self.apple.probe {
            ProbeState::Done(Ok(_)) => None,
            ProbeState::Done(Err(apple::Error::Unsupported(_))) => Some(crate::tr("Für diese Kamera nur graineo – Apple kennt das Modell nicht").to_string()),
            ProbeState::Done(Err(e)) => Some(error_text(e, "")),
            _ => Some(crate::tr("wird geprüft …").to_string()),
        }
    }

    /// Section "Pipeline" at the top of the sensor tray, always shown (on Sigma/Foveon and off macOS greyed out with the
    /// reason): which engine develops this photo (document field `RawAdjust::engine`: graineo | Apple, back to
    /// automatic), the Apple decoder version, the comparison view. Fixed rows: switching never moves the controls below.
    pub(crate) fn apple_section(&mut self, ui: &mut egui::Ui) {
        let camera = self.apple_camera();
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let doc = self.current.as_ref().and_then(|c| c.loaded.previews.foreign.clone());
        let dev = doc.as_ref().and_then(|d| d.apple.clone());
        let blocked = self.apple_blocked();
        let graineo_blocked = dev.as_ref().and_then(|a| a.graineo_blocked.clone());
        let engine = self.entries[index].settings.raw.engine.clone();
        let choice = rawdev::apple::Choice::parse(engine.as_deref());
        // versions the engine offers for this photo: from the development, otherwise from the probe of the view
        let probe = dev.as_ref().map(|a| a.probe.clone()).or_else(|| self.apple.probe().cloned());
        let raw_view = self.sensor_raw_active();
        crate::lr::section(ui, "Pipeline", true, |ui| {
            let before = (self.apple.pipeline, self.apple.steps, self.apple.pin.clone());
            let mut new_engine: Option<Option<String>> = None;
            ui.horizontal(|ui| {
                crate::lr::field_label(ui, "Entwicklung");
                let current = if dev.is_some() { EngineSel::Apple } else { EngineSel::Graineo };
                let graineo_tip = match &graineo_blocked {
                    Some(_) => crate::tr("nicht möglich (HE): graineo kann Nikon „Hohe Effizienz“ noch nicht dekodieren").to_string(),
                    None => crate::tr("Dieses Foto mit graineos eigener Pipeline entwickeln").to_string(),
                };
                let apple_tip = match &blocked {
                    Some(why) => why.clone(),
                    None => crate::tr("Dieses Foto mit der RAW-Engine von macOS dekodieren; Farbe, Ton, Looks, Film und Masken rechnet graineo").to_string(),
                };
                let options = [(EngineSel::Graineo, "graineo", graineo_blocked.is_none(), graineo_tip.as_str()), (EngineSel::Apple, "Apple", blocked.is_none(), apple_tip.as_str())];
                if let Some(sel) = engine_switch(ui, current, &options) {
                    new_engine = Some(match sel {
                        EngineSel::Graineo => Some(rawdev::apple::GRAINEO.to_string()),
                        EngineSel::Apple => {
                            let v = self.apple.pin.clone().or_else(|| probe.as_ref().and_then(|p| apple::fallback_version(&p.versions, &p.engine_default, false)));
                            Some(v.map_or_else(|| "apple".to_string(), |v| rawdev::apple::apple_value(&v)))
                        }
                    });
                }
                ui.add_space(6.0);
                // automatic (no entry in the document): lit; a click on it when something is pinned goes back
                let auto_tip = if engine.is_some() { "Zurück auf Automatik: graineo, Apple nur für Dateien, die graineo nicht dekodieren kann" } else { "Automatik: graineo, Apple nur für Dateien, die graineo nicht dekodieren kann" };
                if crate::lr::icon_button_hint(ui, crate::lr::Icon::Auto, engine.is_none(), crate::tr(auto_tip)) && engine.is_some() {
                    new_engine = Some(None);
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                crate::lr::field_label(ui, "Apple-Version");
                ui.add_enabled_ui(blocked.is_none(), |ui| {
                    let newest = probe.as_ref().and_then(|p| apple::newest(&p.versions, &p.engine_default));
                    let newest_text = match &newest {
                        Some(n) => crate::trf!("Neueste ({a})", a = apple::version_label(n)),
                        None => crate::tr("Neueste").to_string(),
                    };
                    // developed with Apple: the version of the development (choosing one pins it); otherwise the version
                    // of the comparison view
                    let mut pin = match &dev {
                        Some(a) => Some(a.used.clone()),
                        None => self.apple.pin.clone(),
                    };
                    let shown = match &pin {
                        Some(p) => crate::trf!("Apple RAW {a}", a = apple::version_label(p)),
                        None => newest_text.clone(),
                    };
                    let old = pin.clone();
                    let resp = ui
                        .scope(|ui| {
                            crate::lr::field_style(ui);
                            egui::ComboBox::from_id_salt("apple-raw-version").width(130.0).selected_text(shown).show_ui(ui, |ui| {
                                if dev.is_none() {
                                    ui.selectable_value(&mut pin, None, newest_text);
                                }
                                for v in probe.as_ref().map(|p| p.versions.clone()).unwrap_or_default() {
                                    let label = crate::trf!("Apple RAW {a}", a = apple::version_label(&v));
                                    ui.selectable_value(&mut pin, Some(v), label);
                                }
                            })
                        })
                        .inner;
                    let tip = if dev.is_some() { "Decoder-Version für dieses Foto festhalten (ändert die Entwicklung)" } else { "Decoder-Version für den Vergleich; „Neueste“ folgt macOS-Updates" };
                    crate::lr::hover_tip(ui, &resp.response, crate::tr(tip));
                    if pin != old {
                        match (&dev, &pin) {
                            (Some(_), Some(v)) => new_engine = Some(Some(rawdev::apple::apple_value(v))),
                            (None, _) => self.apple.pin = pin.clone(),
                            _ => {}
                        }
                    }
                });
                // the pinned version is missing (macOS update) or Apple is not available here: computed otherwise, "!"
                let items = engine_notice_items(dev.as_deref(), &choice, blocked.as_deref());
                crate::help::notice_mark(ui, "apple-raw-pin", crate::tr("Apple-Version"), &items, crate::photo_state::YELLOW);
            });
            ui.add_space(2.0);
            let compare_possible = raw_view && blocked.is_none();
            ui.add_enabled_ui(compare_possible, |ui| {
                ui.horizontal(|ui| {
                    crate::lr::field_label(ui, "Ansicht");
                    let mut view = if self.apple.pipeline == Pipeline::Graineo { Pipeline::Graineo } else { Pipeline::Compare };
                    if crate::lr::segmented_tips(ui, &mut view, &[(Pipeline::Graineo, "Foto"), (Pipeline::Compare, "Vergleich")], &["Die Entwicklung dieses Fotos", "Links graineo, rechts Apple; Zoomen und Verschieben wirken auf beide"], 8.0) {
                        self.apple.pipeline = view;
                    }
                });
                ui.add_space(2.0);
                ui.add_enabled_ui(self.apple.pipeline != Pipeline::Graineo, |ui| {
                    ui.horizontal(|ui| {
                        crate::lr::field_label(ui, "Schritte");
                        let tip_n = "Rauschminderung, Schärfen und Objektivkorrektur auf beiden Seiten aus; Weißabgleich und Belichtung aus dem Foto";
                        let tip_s = "Jede Pipeline mit ihren eigenen Vorgaben – Apple und graineo machen dann nicht dieselben Schritte";
                        crate::lr::segmented_tips(ui, &mut self.apple.steps, &[(Steps::Neutral, "neutral"), (Steps::Standard, "Standard")], &[tip_n, tip_s], 8.0);
                    });
                });
            });
            ui.add_space(4.0);
            let on = compare_possible && self.apple.pipeline != Pipeline::Graineo;
            let developed = match (&dev, &choice) {
                (Some(a), _) => crate::trf!("Apple RAW {a}", a = apple::version_label(&a.used)),
                _ => "graineo".to_string(),
            };
            let dev_time = match &dev {
                Some(a) => {
                    let t = a.times();
                    let mut parts = Vec::new();
                    if let Some(ms) = t.preview {
                        parts.push(format!("{} {}", secs(ms), crate::tr("Vorschau")));
                    }
                    if let Some(ms) = t.full {
                        parts.push(format!("{} {}", secs(ms), crate::tr("voll")));
                    }
                    if parts.is_empty() { "–".to_string() } else { parts.join(" · ") }
                }
                None => {
                    let graineo_side = graineo_settings(&self.entries[index].settings, self.apple.steps);
                    match self.panes.last_ms.as_ref().filter(|(s, ..)| *s == graineo_side) {
                        Some((_, ms, crop)) if on => format!("{} {}", secs(*ms), if *crop { crate::tr("Ausschnitt") } else { crate::tr("Vorschau") }),
                        _ => "–".to_string(),
                    }
                }
            };
            let compare_text = match (&self.apple.last, on) {
                (Some((_, ms, crop)), true) => {
                    let what = if *crop { crate::tr("Ausschnitt") } else { crate::tr("Vorschau") };
                    let first = self.apple.first_ms.filter(|f| *f > 1500.0).map(|f| format!(" · {}", crate::trf!("erster Lauf {a} (Shader)", a = secs(f)))).unwrap_or_default();
                    let v = self.apple.decoder().map(|d| format!("{} · ", crate::trf!("Apple RAW {a}", a = apple::version_label(&d.used)))).unwrap_or_default();
                    format!("{v}{} {}{first}", secs(*ms), what)
                }
                _ => "–".to_string(),
            };
            let (status, bad) = engine_status(dev.as_deref(), &choice, blocked.as_deref(), on.then(|| self.apple.status(&camera)));
            egui::Grid::new("apple-raw-rows").num_columns(2).spacing([10.0, 3.0]).min_col_width(110.0).show(ui, |ui| {
                for (label, text, warn) in [(crate::tr("Entwickelt mit"), developed, false), (crate::tr("Zeit"), dev_time, false), (crate::tr("Vergleich"), compare_text, false), (crate::tr("Status"), status, bad)] {
                    ui.label(egui::RichText::new(label).color(crate::lr::TEXT_DIM));
                    let color = if warn { Color32::from_rgb(0xe8, 0x90, 0x70) } else { crate::lr::TEXT };
                    let r = ui.add(egui::Label::new(egui::RichText::new(&text).color(color)).truncate());
                    crate::lr::hover_tip(ui, &r, &text);
                    ui.end_row();
                }
            });
            if let Some(e) = new_engine {
                crate::log(&format!("engine of photo {index}: {:?} → {e:?}", self.entries[index].settings.raw.engine));
                self.entries[index].settings.raw.engine = e;
            }
            if before != (self.apple.pipeline, self.apple.steps, self.apple.pin.clone()) {
                self.apple.save();
                if before.0 != self.apple.pipeline {
                    // a new view starts fitted
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                }
            }
        });
    }
}

/// "0,42 s" / "85 ms".
fn secs(ms: f64) -> String {
    use crate::menu::i18n::DeComma as _;
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0).de_comma()
    } else {
        format!("{ms:.0} ms")
    }
}

/// The two engines of the switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineSel {
    Graineo,
    Apple,
}

/// Segmented switch "graineo | Apple" whose segments can be unavailable (drawn dim, not clickable, the tooltip says
/// why). Returns the newly chosen engine.
fn engine_switch(ui: &mut egui::Ui, current: EngineSel, options: &[(EngineSel, &str, bool, &str)]) -> Option<EngineSel> {
    use crate::lr::{TEXT, TEXT_DIM};
    let font = egui::TextStyle::Button.resolve(ui.style());
    let (h, pad) = (20.0, 8.0);
    let widths: Vec<f32> = options.iter().map(|(_, l, ..)| ui.painter().layout_no_wrap(crate::tr(l).to_string(), font.clone(), TEXT).size().x + 2.0 * pad).collect();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(widths.iter().sum(), h), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, crate::lr::BUTTON_FILL);
    let mut x = rect.left();
    let mut chosen = None;
    for (i, ((v, label, available, tip), w)) in options.iter().zip(&widths).enumerate() {
        let seg = Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*w, h));
        x += w;
        let enabled = *available && ui.is_enabled();
        let resp = ui.interact(seg, ui.scope_id().with(("engine-switch", i)), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
        if enabled && resp.clicked() && *v != current {
            chosen = Some(*v);
        }
        let active = *v == current;
        if active {
            painter.rect_filled(seg, 0.0, if enabled || !available { crate::lr::ACTIVE_FILL } else { crate::lr::ACTIVE_FILL.gamma_multiply(0.5) });
        } else if enabled && resp.hovered() {
            painter.rect_filled(seg, 0.0, Color32::from_gray(0x33));
        }
        if !tip.is_empty() {
            crate::lr::hover_tip(ui, &resp, tip);
        }
        let color = if active { Color32::WHITE } else if !enabled { TEXT_DIM.gamma_multiply(0.5) } else if resp.hovered() { TEXT } else { TEXT_DIM };
        let galley = painter.layout_no_wrap(crate::tr(label).to_string(), font.clone(), color);
        painter.galley(seg.center() - galley.size() / 2.0, galley, color);
    }
    chosen
}

/// Status row of the section: development (automatic, pinned, fallback, missing version), then the comparison.
fn engine_status(dev: Option<&rawdev::apple::AppleDev>, choice: &rawdev::apple::Choice, blocked: Option<&str>, compare: Option<(String, bool)>) -> (String, bool) {
    use rawdev::apple::Choice;
    if let Some(a) = dev {
        if let Some(m) = &a.pinned_missing {
            return (crate::trf!("Apple RAW {a} fehlt – mit {b} gerechnet", a = apple::version_label(m), b = apple::version_label(&a.used)), true);
        }
        if a.graineo_blocked.is_some() && matches!(choice, Choice::Auto) {
            return (crate::tr("Automatik · HE-Rückfall").to_string(), false);
        }
        if a.graineo_blocked.is_some() {
            return (crate::tr("festgehalten · HE").to_string(), false);
        }
        return (crate::tr("festgehalten").to_string(), false);
    }
    if let Choice::Apple(v) = choice {
        let v = v.as_deref().map(apple::version_label).unwrap_or_default();
        return (crate::trf!("Apple RAW {a} nicht verfügbar – graineo", a = v), true);
    }
    if let Some((text, bad)) = compare.filter(|(t, _)| t != "–") {
        return (text, bad);
    }
    match choice {
        Choice::Auto => (blocked.map_or_else(|| crate::tr("Automatik").to_string(), |_| crate::tr("Automatik · nur graineo").to_string()), false),
        Choice::Graineo => (crate::tr("festgehalten").to_string(), false),
        Choice::Unknown(v) => (crate::trf!("unbekannte Pipeline „{a}“ – graineo", a = v), true),
        Choice::Apple(_) => unreachable!("handled above"),
    }
}

/// Items of the "!" card of the Apple version: a pinned version macOS no longer offers, or Apple chosen where it is not
/// available (other system, unknown file).
pub(crate) fn engine_notice_items(dev: Option<&rawdev::apple::AppleDev>, choice: &rawdev::apple::Choice, blocked: Option<&str>) -> Vec<crate::help::NoticeItem> {
    if let Some(a) = dev {
        return a
            .pinned_missing
            .as_ref()
            .map(|m| crate::help::NoticeItem {
                title: crate::trf!("Apple RAW {a} nicht verfügbar", a = apple::version_label(m)),
                text: crate::trf!("Für dieses Foto bietet macOS die festgehaltene Version nicht mehr an; gerechnet mit Apple RAW {a}. Das Bild kann abweichen. Der Eintrag bleibt, bis du eine Version wählst.", a = apple::version_label(&a.used)),
                layers: None,
            })
            .into_iter()
            .collect();
    }
    match choice {
        rawdev::apple::Choice::Apple(v) => {
            let v = v.as_deref().map(apple::version_label).unwrap_or_default();
            vec![crate::help::NoticeItem {
                title: crate::trf!("Apple RAW {a} nicht verfügbar", a = v),
                text: crate::trf!("Dieses Foto ist für Apple RAW {a} eingestellt. {b}. Entwickelt mit graineo – das Bild kann abweichen. Der Eintrag bleibt erhalten.", a = v, b = blocked.map(str::to_string).unwrap_or_else(|| crate::tr("macOS hat die Datei nicht entwickelt").to_string())),
                layers: None,
            }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_graineo_side_switches_the_same_steps_off() {
        let mut cur = Settings::default();
        cur.exposure = 0.7;
        cur.raw.wb_mode = "custom".into();
        cur.raw.temperature = 4300.0;
        cur.raw.tint = 8.0;
        cur.contrast = 0.4;
        cur.raw.look = "x100/provia".into();
        cur.sharpen = 1.2;
        let n = graineo_settings(&cur, Steps::Neutral);
        assert_eq!(n.exposure, 0.7);
        assert_eq!((n.raw.wb_mode.as_str(), n.raw.temperature, n.raw.tint), ("custom", 4300.0, 8.0));
        assert_eq!(n.contrast, 0.0, "other sliders stay out");
        assert!(n.raw.look.is_empty(), "no look on either side");
        assert_eq!((n.raw.nr_luma, n.raw.nr_color, n.sharpen), (0.0, 0.0, 0.0));
        assert!(!n.raw.lens && !n.raw.lens_ca);
        let s = graineo_settings(&cur, Steps::Standard);
        let d = Settings::default();
        assert_eq!((s.raw.nr_color, s.raw.lens, s.sharpen), (d.raw.nr_color, d.raw.lens, d.sharpen), "standard = graineo's defaults");
    }

    #[test]
    fn apple_side_takes_white_balance_and_exposure_of_the_photo() {
        let mut cur = Settings::default();
        cur.exposure = -0.5;
        let p = apple_params(&cur, (5100.0, 3.0), Steps::Neutral, None);
        assert_eq!((p.white, p.exposure_ev, p.max_edge), (None, -0.5, Some(PREVIEW_EDGE)), "as shot stays as shot");
        cur.raw.wb_mode = "daylight".into();
        let p = apple_params(&cur, (5500.0, 10.0), Steps::Standard, Some("7"));
        assert_eq!((p.white, p.decoder.as_deref(), p.steps), (Some((5500.0, 10.0)), Some("7"), Steps::Standard));
    }

    #[test]
    fn regions_map_between_the_two_images() {
        // same size: identity
        let (a, s) = map_region([100, 50, 200, 100], [4288, 2848], [4288, 2848]);
        assert_eq!(a, [100, 50, 200, 100]);
        assert_eq!(s, [100.0, 50.0, 200.0, 100.0]);
        // Apple image slightly larger: outward rounding covers the region
        let (a, s) = map_region([1000, 1000, 333, 333], [6000, 4000], [6024, 4024]);
        assert!(covers(s, [1000, 1000, 333, 333]), "{a:?} {s:?}");
        assert!(a[0] + a[2] <= 6024 && a[1] + a[3] <= 4024);
        // at the edge
        let (a, _) = map_region([5990, 3990, 10, 10], [6000, 4000], [6000, 4000]);
        assert_eq!(a, [5990, 3990, 10, 10]);
    }

    #[test]
    fn view_choice_keys_round_trip() {
        for p in [Pipeline::Graineo, Pipeline::Apple, Pipeline::Compare] {
            assert_eq!(Pipeline::from_key(p.key()), p);
        }
        assert_eq!(Pipeline::from_key("unknown"), Pipeline::Graineo);
        let v = AppleView::default();
        assert!(!v.showing(true), "graineo is the default");
        let v = AppleView { pipeline: Pipeline::Compare, ..Default::default() };
        assert_eq!(v.showing(true), cfg!(target_os = "macos"));
        assert!(!v.showing(false), "only for non-Sigma RAWs in the sensor tray");
    }

    #[test]
    fn engine_notices_and_status_name_what_is_not_available() {
        use rawdev::apple::Choice;
        // Apple chosen, but not developed by it (other system, file unknown): a "!" item, the entry stays
        let items = engine_notice_items(None, &Choice::Apple(Some("8".into())), Some("Apple RAW gibt es nur unter macOS – hier entwickelt graineo"));
        assert_eq!(items.len(), 1);
        assert!(items[0].title.contains('8'), "{}", items[0].title);
        let (status, bad) = engine_status(None, &Choice::Apple(Some("8".into())), None, None);
        assert!(bad && status.contains('8'), "{status}");
        // graineo / automatic: nothing to report
        assert!(engine_notice_items(None, &Choice::Auto, None).is_empty());
        assert!(engine_notice_items(None, &Choice::Graineo, Some("x")).is_empty());
        assert!(!engine_status(None, &Choice::Auto, None, None).1);
        // a blocked engine (Sigma, unknown camera) shows "automatic, graineo only"
        let (t, bad) = engine_status(None, &Choice::Auto, Some("Für diese Kamera nur graineo – Apple kennt das Modell nicht"), None);
        assert!(!bad && t != engine_status(None, &Choice::Auto, None, None).0);
        // a value of a newer version is reported, never overwritten
        assert!(engine_status(None, &Choice::Unknown("other:1".into()), None, None).1);
    }

    /// A failed rendering is not asked again for the same parameters (N1 of the review of 1.3.0: the pane asked every
    /// 100 ms, with a log line each time); other parameters or another photo ask again.
    #[test]
    fn a_failed_rendering_is_asked_again_only_for_other_parameters() {
        let want = Params { max_edge: Some(PREVIEW_EDGE), exposure_ev: 0.3, ..Default::default() };
        assert!(should_request(None, None, None, &want));
        assert!(!should_request(Some(&want), None, None, &want), "shown");
        assert!(!should_request(None, Some(&want), None, &want), "on its way");
        assert!(!should_request(None, None, Some(&want), &want), "failed for exactly these parameters");
        let other = Params { exposure_ev: 0.5, ..want.clone() };
        assert!(should_request(None, None, Some(&want), &other), "changed parameters ask again");

        // the reply of the worker: the failure is remembered, the request is no longer pending
        let ctx = egui::Context::default();
        let (tx, _jobs) = mpsc::channel::<Job>();
        let (done, rx) = mpsc::channel::<Reply>();
        let path = PathBuf::from("/nonexistent/graineo-test/DSC_0001.NEF");
        let mut v = AppleView { loaded: true, test_done: true, worker: Some(Worker { tx, rx }), path: Some(path.clone()), probe: ProbeState::Pending, preview_pending: Some(want.clone()), ..Default::default() };
        done.send(Reply::Render(path.clone(), want.clone(), Err(apple::Error::Failed("no output image".into())))).unwrap();
        v.update(&ctx, Some(&path));
        assert_eq!((v.preview_pending.as_ref(), v.preview_failed.as_ref()), (None, Some(&want)));
        assert!(!should_request(None, v.preview_pending.as_ref(), v.preview_failed.as_ref(), &want), "no new request");
        assert!(v.error.is_some(), "the status row names the error");
        // a crop fails on its own
        let crop = Params { max_edge: None, region: Some([0, 0, 100, 100]), ..want.clone() };
        done.send(Reply::Render(path.clone(), crop.clone(), Err(apple::Error::Failed("x".into())))).unwrap();
        v.update(&ctx, Some(&path));
        assert_eq!(v.detail_failed.as_ref(), Some(&crop));
        // another photo starts over
        v.update(&ctx, Some(Path::new("/nonexistent/graineo-test/DSC_0002.NEF")));
        assert!(v.preview_failed.is_none() && v.detail_failed.is_none());
    }

    /// The pin of the automatic fallback is neither a step of the history nor a change that "Apply to the selection"
    /// carries to the other photos; a real change of the user still goes over, without the engine.
    #[test]
    fn the_automatic_pin_is_not_applied_to_the_selection() {
        let mut current = Settings::default();
        let mut committed = current.clone();
        let mut base = current.clone();
        pin_fallback(&mut current, Some(&mut committed), Some(&mut base), "apple:8");
        assert_eq!((current.raw.engine.as_deref(), committed.raw.engine.as_deref(), base.raw.engine.as_deref()), (Some("apple:8"), Some("apple:8"), Some("apple:8")));
        // the user then changes the exposure of the active photo and applies it to another (graineo) photo
        current.exposure = 0.5;
        let other = Settings { contrast: 10.0, ..Settings::default() };
        let step = crate::multi_edit::step(&base, &current, crate::multi_edit::Mode::Relative);
        let after = step(&other);
        assert_eq!(after.exposure, 0.5);
        assert_eq!(after.raw.engine, None, "the other photo keeps its engine");
        assert_eq!(after.contrast, 10.0);
    }

    #[test]
    fn errors_are_named() {
        assert!(error_text(&apple::Error::Unsupported("x".into()), "Sigma fp").contains("Sigma fp"));
        assert!(!error_text(&apple::Error::Failed("boom".into()), "").is_empty());
        assert!(!error_text(&apple::Error::Missing(PathBuf::from("/x")), "").is_empty());
    }
}
