//! Sensor view for Bayer and X-Trans RAWs (X3F keeps its layer view).
//!
//! Computation core: `crates/sensor_view` ([`RawBild`] + [`analysieren`]); state and drawing
//! over the image here, the controls live in the "Sensor" tray of the tool rail (`sensor_ui.rs`):
//! - overlays over the image in the same mapping as the developed image (rotation, crop);
//! - mosaic (raw values in filter color, with pixel grid) from 400 % for the visible part;
//! - demosaic view of the visible crop from 100 %;
//! - a small status label in the image corner naming what is shown (no bar over the image);
//! - values at the mouse pointer (DN per filter color, EV below saturation, clipped, SNR, edge).
//!
//! All computation runs in the background (one thread per image, jobs in order); the UI
//! never waits.

use crate::menu::i18n::DeComma as _;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use eframe::egui;
use egui::{Color32, TextureHandle};
use sensor_view::{analyze, Kind, Region, Outcome, Options, RawImage, Role};
use serde_json::Value;

use crate::lr;

/// An overlay (toggle in the bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Overlay {
    Clipping,
    Reserve,
    Sharpness,
    Moire,
    Noise,
    Defects,
    Lens,
    Mosaic,
}

impl Overlay {
    pub const ALL: [Overlay; 8] = [Overlay::Clipping, Overlay::Reserve, Overlay::Sharpness, Overlay::Moire, Overlay::Noise, Overlay::Defects, Overlay::Lens, Overlay::Mosaic];

    pub fn name(self) -> &'static str {
        match self {
            Overlay::Clipping => crate::tr("Clipping"),
            Overlay::Reserve => crate::tr("JPEG-Reserve"),
            Overlay::Sharpness => crate::tr("Schärfe"),
            Overlay::Moire => crate::tr("Moiré"),
            Overlay::Noise => crate::tr("Rauschen"),
            Overlay::Defects => crate::tr("Defekte"),
            Overlay::Lens => crate::tr("Objektiv"),
            Overlay::Mosaic => crate::tr("Mosaik"),
        }
    }

    /// ID in the key table (⌥ + letter).
    pub fn key_id(self) -> &'static str {
        match self {
            Overlay::Clipping => "sensor_clipping",
            Overlay::Reserve => "sensor_reserve",
            Overlay::Sharpness => "sensor_sharpness",
            Overlay::Moire => "sensor_moire",
            Overlay::Noise => "sensor_noise",
            Overlay::Defects => "sensor_defects",
            Overlay::Lens => "sensor_lens",
            Overlay::Mosaic => "sensor_mosaic",
        }
    }

    fn icon(self) -> lr::Icon {
        match self {
            Overlay::Clipping => lr::Icon::Warning,
            Overlay::Reserve => lr::Icon::Sun,
            Overlay::Sharpness => lr::Icon::Crosshair,
            Overlay::Moire => lr::Icon::Checkerboard,
            Overlay::Noise => lr::Icon::Waveform,
            Overlay::Defects => lr::Icon::Target,
            Overlay::Lens => lr::Icon::Aperture,
            Overlay::Mosaic => lr::Icon::GridNine,
        }
    }

    fn kind(self) -> Kind {
        match self {
            Overlay::Clipping | Overlay::Reserve => Kind::Histogram,
            Overlay::Sharpness => Kind::Sharpness,
            Overlay::Moire => Kind::DemosaicComparison,
            Overlay::Noise => Kind::Noise,
            Overlay::Defects => Kind::Health,
            Overlay::Lens => Kind::Lens,
            Overlay::Mosaic => Kind::Mosaic,
        }
    }

    /// Legend: (color, meaning).
    fn legend(self, channel: usize) -> Vec<(Color32, &'static str)> {
        match self {
            Overlay::Clipping => vec![(Color32::RED, crate::tr("Rot geclippt")), (Color32::GREEN, crate::tr("Grün geclippt")), (Color32::from_rgb(60, 110, 255), crate::tr("Blau geclippt")), (Color32::WHITE, crate::tr("alle Kanäle"))],
            Overlay::Reserve => vec![(Color32::from_rgb(60, 200, 90), crate::tr("JPEG weiß, RAW ≥ ½ EV Reserve")), (Color32::from_rgb(230, 200, 50), crate::tr("Reserve < ½ EV")), (Color32::from_rgb(230, 60, 50), crate::tr("auch im RAW geclippt"))],
            Overlay::Sharpness => vec![(Color32::from_rgb(80, 255, 120), crate::tr("Fokus-Peaking: schärfste Kanten (Grün vor Demosaik)"))],
            Overlay::Moire => vec![(Color32::from_rgb(255, 90, 200), crate::tr("Falschfarben-/Moiré-Gefahr")), (Color32::from_rgb(60, 220, 230), crate::tr("Rahmen: geprüfter Ausschnitt"))],
            Overlay::Noise if channel == 3 => vec![(Color32::from_rgb(80, 120, 230), crate::tr("weniger Rauschen als das Modell")), (Color32::WHITE, crate::tr("wie erwartet")), (Color32::from_rgb(220, 70, 50), crate::tr("mehr (Textur/Banding)"))],
            Overlay::Noise => vec![(Color32::from_rgb(40, 20, 80), crate::tr("niedriger Rauschabstand")), (Color32::from_rgb(250, 170, 60), crate::tr("hoher Rauschabstand (0–45 dB)"))],
            Overlay::Defects => vec![(Color32::from_rgb(255, 60, 60), crate::tr("Hot-Pixel")), (Color32::from_rgb(60, 140, 255), crate::tr("Dead-Pixel")), (Color32::from_rgb(255, 200, 40), crate::tr("Stuck-Pixel"))],
            Overlay::Lens => vec![(Color32::from_rgb(255, 180, 60), crate::tr("Vignettierung (Profil)")), (Color32::from_rgb(120, 230, 140), crate::tr("Verzeichnung als Gitter"))],
            Overlay::Mosaic => vec![(Color32::from_rgb(230, 70, 60), "R"), (Color32::from_rgb(70, 200, 80), "G"), (Color32::from_rgb(70, 110, 240), "B")],
        }
    }

    fn layers(self, channel: usize) -> &'static [&'static str] {
        match self {
            Overlay::Clipping => &["clipping"],
            Overlay::Reserve => &["jpeg_reserve"],
            Overlay::Sharpness => &["peaking"],
            Overlay::Moire => &["false_color_map"],
            Overlay::Noise => match channel {
                0 => &["noise_r"],
                1 => &["noise_g"],
                2 => &["noise_b"],
                _ => &["noise_ratio"],
            },
            Overlay::Defects => &["defects"],
            Overlay::Lens => &["vignette_profile", "distortion_grid"],
            Overlay::Mosaic => &["mosaic"],
        }
    }
}

/// Row labels of [`SensorRaw::values`] (fixed order, fixed rows in the tray).
pub const VALUE_ROWS: [&str; 8] = ["Position", "Filter", "Rohwert (DN)", "unter Sättigung", "Geclippt", "Rauschabstand", "Kante", "Defekt"];

/// Demosaic view of the crop (0 = sensor default, neutral output).
pub const DEMOSAIC: [(&str, &str); 5] = [("", "Entwicklung"), ("rcd", "RCD"), ("markesteijn3", "Markesteijn 3"), ("markesteijn1", "Markesteijn 1"), ("bilinear", "bilinear")];

enum Task {
    Analysis(Kind, Options, String),
}

enum Answer {
    Opened(Result<Arc<RawImage>, String>),
    Done(String, Result<Arc<Outcome>, String>),
}

/// State of the sensor view (RAW).
pub struct SensorRaw {
    path: Option<PathBuf>,
    rb: Option<Arc<RawImage>>,
    pub error: Option<String>,
    /// The photo is developed by Apple's RAW engine because graineo cannot decode it (Nikon HE): no sensor view, and
    /// this says why (set every frame, see `sensor_raw_update`) instead of "sensor data not readable".
    pub apple_only: Option<String>,
    tx: Option<mpsc::Sender<Task>>,
    /// Crops of the visible area (mosaic, demosaic) go to their own lane, see [`crop_lane`]
    tx_crop: Option<mpsc::Sender<Task>>,
    rx: Option<mpsc::Receiver<Answer>>,
    /// Results per key ("art" or "art@ausschnitt")
    results: HashMap<String, Arc<Outcome>>,
    running: HashSet<String>,
    tex: HashMap<String, TextureHandle>,
    pub an: HashSet<Overlay>,
    /// Noise: 0 R, 1 G, 2 B, 3 ratio
    pub channel: usize,
    pub opacity: f32,
    /// Index into [`DEMOSAIC`]
    pub demosaic: usize,
    pub mosaic_previous: Option<(f32, egui::Vec2)>,
    pub view_center: Option<egui::Pos2>,
    /// Mouse pointer in image coordinates (crop, pixels)
    pub hover: Option<[f32; 2]>,
    /// visible crop (image coordinates) and zoom (screen pixels per image pixel)
    view_rect: Option<(Region, f32, f64)>,
    /// Crop result currently shown (key)
    crop_mosaic: Option<String>,
    crop_demosaic: Option<String>,
}

impl Default for SensorRaw {
    fn default() -> Self {
        Self {
            path: None,
            rb: None,
            error: None,
            apple_only: None,
            tx: None,
            tx_crop: None,
            rx: None,
            results: HashMap::new(),
            running: HashSet::new(),
            tex: HashMap::new(),
            an: HashSet::new(),
            channel: 3,
            opacity: 0.8,
            demosaic: 0,
            mosaic_previous: None,
            view_center: None,
            hover: None,
            view_rect: None,
            crop_mosaic: None,
            crop_demosaic: None,
        }
    }
}

fn defect_color(kind: &str) -> Color32 {
    match kind { "hot" => Color32::from_rgb(255,60,60), "dead" => Color32::from_rgb(60,140,255), _ => Color32::from_rgb(255,200,40) }
}

const MAP_SIZE: usize = 1600;

fn map_options() -> Options {
    Options { size: MAP_SIZE, detail: None, zoom: 8, grid: true, render_comparison: true, full_image: true, peaking_threshold: 0.35 }
}

/// Readable defect counts: "Hot 8 (R 1 · G 3 · B 4) · Stuck 0 · Dead 0", "none" when all are 0.
/// `counts`: `{"hot": {"r", "g", "b", "total"}, "stuck": {...}, "dead": {...}}`.
fn defects_text(counts: &Value) -> String {
    let n = |v: &Value| v.as_f64().unwrap_or(0.0).round() as i64;
    let mut parts = Vec::new();
    for (key, name) in [("hot", crate::tr("Heiß")), ("stuck", crate::tr("Hängend")), ("dead", crate::tr("Tot"))] {
        let c = &counts[key];
        let total = n(&c["total"]);
        if total > 0 {
            let split: Vec<String> = [("r", "R"), ("g", "G"), ("b", "B")].iter().filter(|(k, _)| n(&c[*k]) > 0).map(|(k, l)| format!("{l} {}", n(&c[*k]))).collect();
            parts.push(if split.is_empty() { format!("{name} {total}") } else { format!("{name} {total} ({})", split.join(" · ")) });
        } else {
            parts.push(format!("{name} 0"));
        }
    }
    if parts.iter().all(|p| p.ends_with(" 0")) {
        return crate::tr("keine").to_string();
    }
    parts.join(" · ")
}

/// Answer of the crop lane for a crop it skipped because a newer one of the same kind was waiting.
const SUPERSEDED: &str = "superseded";

/// Of waiting crops only the newest of each kind is computed (`true`); older ones are superseded.
fn newest_per_kind(kinds: &[Kind]) -> Vec<bool> {
    kinds.iter().enumerate().map(|(i, k)| !kinds[i + 1..].contains(k)).collect()
}

/// Two workers at utility priority for what the user waits for: reading the sensor data and the crops.
fn crop_pool() -> Option<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("sensorblick-vorn-{i}"))
        .start_handler(|_| library::platform::set_priority(library::platform::Priority::Utility))
        .build()
        .ok()
}

/// Lane for the crops of the visible area (mosaic, demosaic comparison): its own thread and two workers at utility
/// priority, so a crop never waits behind the whole-image analyses (histogram, sharpness, noise, defects; seconds on
/// the quiet background lane, far longer while the Mac is busy) and shows within a fraction of a second. Of several
/// waiting crops of one kind only the newest is computed (panning and zooming queue many).
fn crop_lane(pool: rayon::ThreadPool, rb: Arc<RawImage>, rx: mpsc::Receiver<Task>, tx: mpsc::Sender<Answer>, c: egui::Context) {
    let _ = std::thread::Builder::new().name("sensorblick-ausschnitt".into()).spawn(move || {
        library::platform::set_priority(library::platform::Priority::Utility);
        while let Ok(first) = rx.recv() {
            let mut waiting = vec![first];
            waiting.extend(rx.try_iter());
            let kinds: Vec<Kind> = waiting.iter().map(|Task::Analysis(k, _, _)| *k).collect();
            let compute = newest_per_kind(&kinds);
            for (i, Task::Analysis(kind, o, key)) in waiting.iter().enumerate() {
                let r = if !compute[i] {
                    Err(SUPERSEDED.to_string())
                } else {
                    let t = std::time::Instant::now();
                    let r = pool.install(|| analyze(&rb, *kind, o)).map(Arc::new).map_err(|e| e.to_string());
                    crate::log(&format!("sensor view {key}: done after {:.0} ms (region lane)", t.elapsed().as_secs_f64() * 1000.0));
                    r
                };
                if tx.send(Answer::Done(key.clone(), r)).is_err() {
                    return;
                }
                c.request_repaint();
            }
        }
    });
}

impl SensorRaw {
    /// Take care of this file (opens in the background if new). `None`: leave the view.
    pub fn for_stage(&mut self, path: Option<&std::path::Path>, ctx: &egui::Context) {
        if self.path.as_deref() == path {
            self.fetch(ctx);
            return;
        }
        // new image: discard everything (the old thread ends with its channel)
        let an = std::mem::take(&mut self.an);
        let (channel, opacity, demosaic) = (self.channel, self.opacity, if path.is_none() { 0 } else { self.demosaic });
        let mosaic_previous = if path.is_some() { self.mosaic_previous.take() } else { None };
        *self = SensorRaw { an, channel, opacity, demosaic, mosaic_previous, ..Default::default() };
        let Some(p) = path else { return };
        self.path = Some(p.to_path_buf());
        let (tx, rx_job) = mpsc::channel::<Task>();
        let (tx_crop, rx_crop) = mpsc::channel::<Task>();
        let (tx_answer, rx) = mpsc::channel::<Answer>();
        let p = p.to_path_buf();
        let c = ctx.clone();
        let _ = std::thread::Builder::new().name("sensorblick".into()).spawn(move || {
            let t_open = std::time::Instant::now();
            library::platform::background_priority();
            let pool = match rayon::ThreadPoolBuilder::new().num_threads(2).thread_name(|i| format!("sensorblick-{i}")).start_handler(|_| library::platform::background_priority()).build() {
                Ok(pool) => pool,
                Err(e) => {
                    let _ = tx_answer.send(Answer::Opened(Err(e.to_string())));
                    c.request_repaint();
                    return;
                }
            };
            // the user waits for the sensor data: read them on the crop lane's workers (utility priority); the
            // whole-image analyses below stay on the quiet background workers
            let Some(front) = crop_pool() else { return };
            let rb = match front.install(|| RawImage::open(&p).map(|rb| { rb.prepare(); Arc::new(rb) })) {
                Ok(rb) => rb,
                Err(e) => {
                    let _ = tx_answer.send(Answer::Opened(Err(e.to_string())));
                    c.request_repaint();
                    return;
                }
            };
            crate::log(&format!("sensor view: sensor data read after {:.0} ms", t_open.elapsed().as_secs_f64() * 1000.0));
            let _ = tx_answer.send(Answer::Opened(Ok(rb.clone())));
            c.request_repaint();
            crop_lane(front, rb.clone(), rx_crop, tx_answer.clone(), c.clone());
            for Task::Analysis(kind, o, key) in rx_job {
                let r = pool.install(|| analyze(&rb, kind, &o)).map(Arc::new).map_err(|e| e.to_string());
                if tx_answer.send(Answer::Done(key, r)).is_err() {
                    break;
                }
                c.request_repaint();
            }
        });
        self.tx = Some(tx);
        self.tx_crop = Some(tx_crop);
        self.rx = Some(rx);
        // request the basics right away (fast, or for the side panel)
        for a in [Kind::Internals, Kind::Histogram, Kind::Sharpness, Kind::Noise] {
            self.request(a);
        }
        // The pointer can report defects even when their overlay is switched off.
        if self.an.contains(&Overlay::Mosaic) { self.request(Kind::Health); }
    }

    fn fetch(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let mut added = Vec::new();
        while let Ok(a) = rx.try_recv() {
            added.push(a);
        }
        for a in added {
            match a {
                Answer::Opened(Ok(rb)) => self.rb = Some(rb),
                Answer::Opened(Err(e)) => self.error = Some(e),
                Answer::Done(k, r) => {
                    self.running.remove(&k);
                    // A newer view may have replaced this crop while it was still computing.
                    if k.contains('@') && self.crop_mosaic.as_ref() != Some(&k) && self.crop_demosaic.as_ref() != Some(&k) {
                        continue;
                    }
                    match r {
                        // a newer crop of the same kind replaced it before it started
                        Err(e) if e == SUPERSEDED => {}
                        Ok(e) => {
                            crate::log(&format!("sensorblick {k}: {:.0} ms, layers {:?}", e.duration.as_secs_f64() * 1000.0, e.layers.iter().map(|x| x.name.as_str()).collect::<Vec<_>>()));
                            // textures of this result anew
                            self.tex.retain(|t, _| !t.starts_with(&format!("{k}:")));
                            self.results.insert(k, e);
                        }
                        Err(e) => crate::log(&format!("sensorblick {k}: {e}")),
                    }
                }
            }
        }
        if !self.running.is_empty() || self.rb.is_none() && self.error.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    fn request(&mut self, kind: Kind) {
        let k = kind.name().to_string();
        if self.results.contains_key(&k) || self.running.contains(&k) {
            return;
        }
        if let Some(tx) = &self.tx {
            if tx.send(Task::Analysis(kind, map_options(), k.clone())).is_ok() {
                self.running.insert(k);
            }
        }
    }

    /// Crop analysis (mosaic or demosaic) for the visible area.
    fn request_crop(&mut self, kind: Kind, b: Region, zoom: usize) -> String {
        let k = format!("{}@{},{},{},{}@{zoom}", kind.name(), b.x, b.y, b.w, b.h);
        if !self.results.contains_key(&k) && !self.running.contains(&k) {
            // discard older crops of this kind (memory)
            let p = format!("{}@", kind.name());
            let keep: Vec<String> = [self.crop_mosaic.clone(), self.crop_demosaic.clone()].into_iter().flatten().collect();
            self.results.retain(|x, _| !x.starts_with(&p) || keep.contains(x));
            self.tex.retain(|id, _| !id.starts_with(&p) || keep.iter().any(|k| id.strip_prefix(k).is_some_and(|suffix| suffix.starts_with(':'))));
            let o = Options { size: MAP_SIZE, detail: Some(b), zoom, grid: true, render_comparison: false, full_image: false, peaking_threshold: 0.35 };
            if let Some(tx) = &self.tx_crop {
                if tx.send(Task::Analysis(kind, o, k.clone())).is_ok() {
                    self.running.insert(k.clone());
                }
            }
        }
        k
    }

    /// Mouse pointer: source pixel of the developed image (`fw`×`fh`) → image coordinates (crop).
    pub fn set_hover(&mut self, src: Option<[f32; 2]>, fw: f32, fh: f32) {
        if src.is_none() && crate::env::var_os("GRAINEO_SENSOR").is_some_and(|v| v.to_string_lossy().contains("pointer=")) {
            return; // the test hook holds an imaginary pointer
        }
        self.hover = match (src, &self.rb) {
            (Some(p), Some(rb)) => Some([p[0] * rb.crop.w as f32 / fw, p[1] * rb.crop.h as f32 / fh]),
            _ => None,
        };
    }

    pub fn ready(&self) -> bool {
        self.rb.is_some()
    }

    pub fn has_stage(&self) -> bool {
        self.path.is_some()
    }

    pub fn outcome(&self, a: Kind) -> Option<&Arc<Outcome>> {
        self.results.get(a.name())
    }

    /// Flip a toggle (requests the analysis).
    pub fn toggle_on(&mut self, o: Overlay) {
        if !self.an.remove(&o) {
            self.an.insert(o);
            if o == Overlay::Mosaic {
                crate::log("sensorblick: Mosaik eingeschaltet");
                self.request(Kind::Health);
            } else {
                self.request(o.kind());
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context, key: &str, layer: &str) -> Option<(TextureHandle, Option<Region>)> {
        let e = self.results.get(key)?.clone();
        let eb = e.layer(layer)?;
        let id = format!("{key}:{layer}");
        if !self.tex.contains_key(&id) {
            let img = egui::ColorImage::from_rgba_unmultiplied([eb.canvas.width, eb.canvas.height], &eb.canvas.rgba);
            let opt = if eb.role == Role::View { egui::TextureOptions::NEAREST } else { egui::TextureOptions::LINEAR };
            self.tex.insert(id.clone(), ctx.load_texture(format!("sensorblick-{id}"), img, opt));
        }
        Some((self.tex[&id].clone(), eb.reference))
    }

    /// Draw overlays over the image. `map_px(src)` maps source pixels (developed image, `fw`×`fh`)
    /// to the screen; `paint(tex, src, tint)` paints a texture over a source rectangle.
    /// `view_rect`: visible source area (x, y, w, h) and screen pixels per source pixel.
    pub fn overlay(&mut self, ctx: &egui::Context, fw: f32, fh: f32, view_rect: [f32; 4], px_per_px: f32, mut paint: impl FnMut(&TextureHandle, [f32; 4], Color32)) {
        let Some(rb) = self.rb.clone() else { return };
        let z = rb.crop;
        let (kx, ky) = (fw / z.w as f32, fh / z.h as f32);
        let src = |b: Region| [b.x as f32 * kx, b.y as f32 * ky, b.w as f32 * kx, b.h as f32 * ky];
        let now = ctx.input(|i| i.time);
        // visible area in image coordinates (crop)
        let x0 = (view_rect[0] / kx).floor().max(0.0).min((z.w-1) as f32) as usize;
        let y0 = (view_rect[1] / ky).floor().max(0.0).min((z.h-1) as f32) as usize;
        let x1 = ((view_rect[0] + view_rect[2]) / kx).ceil().max((x0+1) as f32).min(z.w as f32) as usize;
        let y1 = ((view_rect[1] + view_rect[3]) / ky).ceil().max((y0+1) as f32).min(z.h as f32) as usize;
        let vb = Region { x: x0, y: y0, w: x1-x0, h: y1-y0 };
        let zoom_image = px_per_px * kx.max(ky); // screen pixels per sensor pixel
        let settled = match &self.view_rect {
            Some((b, zz, since)) if *b == vb && (*zz - zoom_image).abs() < 0.01 => now - since > 0.3,
            _ => {
                self.view_rect = Some((vb, zoom_image, now));
                false
            }
        };
        if !settled {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        // demosaic crop (from 100 %) – opaque, below the overlays
        if zoom_image >= 0.99 && !(self.an.contains(&Overlay::Mosaic) && zoom_image >= 3.99) {
            if settled {
                let k = self.request_crop(Kind::DemosaicComparison, vb, (zoom_image.round() as usize).clamp(1, (2048 / vb.w.max(vb.h).max(1)).clamp(1, 16)));
                self.crop_demosaic = Some(k);
            }
            if let Some(k) = self.crop_demosaic.clone() {
                let method = if self.demosaic == 0 { if rb.period == 2 { 1 } else { 2 } } else { self.demosaic };
                if let Some((t, Some(b))) = self.texture(ctx, &k, DEMOSAIC[method].0) {
                    paint(&t, src(b), crate::lr::TEXT_STRONG());
                }
            }
        }
        // mosaic from 400 % (opaque, raw values in filter color with grid)
        if self.an.contains(&Overlay::Mosaic) && zoom_image >= 3.99 {
            if settled {
                let k = self.request_crop(Kind::Mosaic, vb, (zoom_image.round() as usize).clamp(4, (4096 / vb.w.max(vb.h).max(1)).clamp(4, 16)));
                self.crop_mosaic = Some(k);
            }
            if let Some(k) = self.crop_mosaic.clone() {
                if let Some((t, Some(b))) = self.texture(ctx, &k, "mosaic") {
                    paint(&t, src(b), crate::lr::TEXT_STRONG());
                }
            }
        }
        let tint = Color32::from_white_alpha((self.opacity * 255.0) as u8);
        for o in Overlay::ALL {
            if !self.an.contains(&o) || o == Overlay::Mosaic || (o == Overlay::Defects && self.an.contains(&Overlay::Mosaic) && zoom_image >= 3.99) {
                continue;
            }
            for eb in o.layers(self.channel) {
                if let Some((t, Some(b))) = self.texture(ctx, o.kind().name(), eb) {
                    paint(&t, src(b), tint);
                }
            }
        }
    }

    /// Values at the mouse pointer, one entry per row of [`VALUE_ROWS`]. `None`: no position.
    pub fn values(&self) -> Option<[String; 8]> {
        let rb = self.rb.as_ref()?;
        let [hx, hy] = self.hover?;
        let z = rb.crop;
        let (ix, iy) = (hx as usize, hy as usize);
        if ix >= z.w || iy >= z.h {
            return None;
        }
        let (sx, sy) = (z.x + ix, z.y + iy);
        let p = rb.period;
        // pattern cell around the point (Bayer 2×2, X-Trans 3×3 around the point)
        let (x0, y0, n) = if p == 2 { (sx & !1, sy & !1, 2) } else { (sx.saturating_sub(1), sy.saturating_sub(1), 3) };
        let mut sum = [0f32; 3];
        let mut count = [0u32; 3];
        for y in y0..(y0 + n).min(rb.height) {
            for x in x0..(x0 + n).min(rb.width) {
                let c = rb.color(x, y);
                sum[c] += rb.value(x, y);
                count[c] += 1;
            }
        }
        let sat = rb.saturation_color();
        let c = rb.color(sx, sy);
        let class_name = rb.classes.names.get(rb.class(sx, sy)).copied().unwrap_or("?");
        let mut ev = Vec::new();
        let mut clip = Vec::new();
        for f in 0..3 {
            if count[f] == 0 {
                continue;
            }
            let v = sum[f] / count[f] as f32;
            ev.push(format!("{:+.1}", (v.max(1e-6) / sat[f]).log2()).de_comma());
            if v >= sat[f] {
                clip.push(["R", "G", "B"][f]);
            }
        }
        // signal-to-noise ratio (model) for the filter color of the pixel
        let v = rb.value(sx, sy);
        let snr = rb
            .noise_model()
            .map(|m| m.sigma(c, v.max(0.0)))
            .filter(|&sig| sig > 0.0 && v > 0.0)
            .map_or("–".to_string(), |sig| format!("{:.0} dB", 20.0 * (v / sig).log10()));
        // local edge (green, gradient in a 7×7 window, relative to saturation)
        let mut g = 0f32;
        let mut k = 0u32;
        for y in sy.saturating_sub(3)..(sy + 4).min(rb.height - 1) {
            for x in sx.saturating_sub(3)..(sx + 4).min(rb.width - 2) {
                if rb.color(x, y) == 1 && rb.color(x + 2, y) == 1 {
                    let d = rb.value(x + 2, y) - rb.value(x, y);
                    g += d * d;
                    k += 1;
                }
            }
        }
        let edge = if k > 0 { format!("{:.1} %", (g / k as f32).sqrt() / sat[1] * 100.0).de_comma() } else { "–".into() };
        Some([
            format!("x {sx} · y {sy}"),
            class_name.to_string(),
            format!("{} {:.0}", ["R", "G", "B"][c], rb.value_dn(sx, sy)),
            format!("{} EV", ev.join(" / ")),
            if clip.is_empty() { crate::tr("nein").to_string() } else { clip.join(" ") },
            snr,
            edge,
            self.outcome(Kind::Health).and_then(|e| e.metrics["defects"].as_array()).and_then(|v| v.iter().find(|d| d["x"].as_u64() == Some(ix as u64) && d["y"].as_u64() == Some(iy as u64))).and_then(|d| d["type"].as_str()).map(|v| match v { "hot" => "Hot", "dead" => "Dead", "stuck" => "Stuck", _ => "–" }.to_string()).unwrap_or_else(|| "–".to_string()),
        ])
    }

    /// Key figures for the side panel (label, value).
    pub fn metrics(&self) -> Vec<(&'static str, String)> {
        let mut z = Vec::new();
        let num = |v: &Value| -> String {
            match v {
                Value::Null => "–".into(),
                Value::Number(n) => {
                    let f = n.as_f64().unwrap_or(0.0);
                    if f.fract() == 0.0 { format!("{f:.0}") } else { format!("{f:.2}").de_comma() }
                }
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().map(|x| match x {
                    Value::Number(n) => {
                        let f = n.as_f64().unwrap_or(0.0);
                        if f.fract().abs() < 1e-9 || f.abs() >= 100.0 { format!("{f:.0}") } else { format!("{f:.2}").de_comma() }
                    }
                    o => o.to_string(),
                }).collect::<Vec<_>>().join(" / "),
                o => o.to_string(),
            }
        };
        if let Some(e) = self.outcome(Kind::Internals) {
            let k = &e.metrics;
            z.push((crate::tr("Sensor"), format!("{} · {} Bit", k["sensor"]["type"].as_str().unwrap_or("–"), num(&k["bit_depth"]))));
            if let Some(m) = k["sensor"]["pattern_image"].as_array() {
                z.push((crate::tr("Muster"), m.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(" ")));
            }
            z.push((crate::tr("Schwarz / Weiß"), format!("{} / {} DN", num(&k["black_dn_rgb"]), num(&k["white_dn"]))));
            z.push((crate::tr("WB (Aufnahme)"), num(&k["wb_shot"])));
            if k["fuji_dr"].is_object() {
                z.push(("Fuji-DR", num(&k["fuji_dr"]["setting"])));
            }
        }
        if let Some(e) = self.outcome(Kind::Histogram) {
            let k = &e.metrics;
            let j = &k["render_compare"]["camera_jpeg"];
            if j.is_object() && j.get("error").is_none() {
                z.push((crate::tr("JPEG ausgefressen"), crate::trf!("{a} % (RAW-Reserve ≥ ½ EV: {b} %)", a = num(&j["blown_percent"]), b = num(&j["of_which_with_reserve_from_0_5ev_percent"]))));
            }
        }
        if let Some(e) = self.outcome(Kind::Noise) {
            let k = &e.metrics;
            z.push((crate::tr("Dynamikumfang"), crate::trf!("{a} EV nutzbar", a = num(&k["dynamic_range_usable"]["ev"]))));
            z.push((crate::tr("Szenenumfang"), format!("{} EV", num(&k["scene_range_ev"]))));
        }
        if let Some(e) = self.outcome(Kind::Sharpness) {
            let k = &e.metrics;
            z.push((crate::tr("Schärfe-Index"), num(&k["sharpness_index"])));
            z.push((crate::tr("Peaking-Anteil"), format!("{} %", num(&k["peaking_fraction_percent"]))));
        }
        if let Some(e) = self.outcome(Kind::Health) {
            let k = &e.metrics;
            z.push((crate::tr("Defekte"), defects_text(&k["count"])));
        }
        if let Some(e) = self.outcome(Kind::Lens) {
            let k = &e.metrics;
            z.push((crate::tr("Objektiv"), k["lens"].as_str().unwrap_or("–").to_string()));
        }
        if let Some(e) = self.outcome(Kind::DemosaicComparison) {
            z.push(("G1/G2-Abweichung", format!("{} %", num(&e.metrics["g1_g2_imbalance_percent"]))));
        }
        z
    }

    /// Screen pixels per sensor pixel of the last view.
    pub fn zoom_image(&self) -> f32 {
        self.view_rect.as_ref().map_or(0.0, |s| s.1)
    }

    pub fn sensor_type(&self) -> Option<usize> {
        self.rb.as_ref().map(|r| r.period)
    }

    /// Legends of the enabled overlays (fixed order).
    pub fn legends(&self) -> Vec<(Overlay, Vec<(Color32, &'static str)>)> {
        Overlay::ALL.iter().filter(|o| self.an.contains(o)).map(|&o| (o, o.legend(self.channel))).collect()
    }

    /// Is an analysis for this toggle still in flight?
    pub fn computing(&self, o: Overlay) -> bool {
        self.running.contains(o.kind().name()) || (o == Overlay::Mosaic && self.running.iter().any(|k| k.starts_with("mosaic@")))
    }

    pub fn demosaic_running(&self) -> bool {
        self.running.iter().any(|k| k.starts_with("demosaic@"))
    }

    /// Draw only visible sensor cells, independent of the resolution of the overview map.
    pub fn paint_pixels(&self, painter: &egui::Painter, visible: [f32; 4], fw: f32, fh: f32, map: impl Fn([f32; 2]) -> egui::Pos2) {
        let Some(rb) = &self.rb else { return };
        if !self.an.contains(&Overlay::Mosaic) || self.zoom_image() < 3.99 { return; }
        let Some(mosaic) = self.crop_mosaic.as_ref().and_then(|k| self.results.get(k)).and_then(|e| e.layer("mosaic")) else { return };
        let z = rb.crop;
        let kx = fw / z.w as f32;
        let ky = fh / z.h as f32;
        let Some(bounds) = mosaic.reference else { return };
        let x0 = ((visible[0] / kx).floor().max(0.0) as usize).max(bounds.x);
        let y0 = ((visible[1] / ky).floor().max(0.0) as usize).max(bounds.y);
        let x1 = (((visible[0] + visible[2]) / kx).ceil().max(0.0).min(z.w as f32) as usize).min(bounds.x + bounds.w);
        let y1 = (((visible[1] + visible[3]) / ky).ceil().max(0.0).min(z.h as f32) as usize).min(bounds.y + bounds.h);
        if x0 >= x1 || y0 >= y1 { return; }
        let defects = self.an.contains(&Overlay::Defects).then(|| self.outcome(Kind::Health)).flatten().and_then(|e| e.metrics["defects"].as_array());
        let defect_index: HashMap<(usize, usize), &Value> = defects.into_iter().flatten().filter_map(|v| {
            let x = v["x"].as_u64()? as usize;
            let y = v["y"].as_u64()? as usize;
            (x >= x0 && x < x1 && y >= y0 && y < y1).then_some(((x,y),v))
        }).collect();
        for (&(x,y), d) in &defect_index {
            let corners = [map([x as f32*kx,y as f32*ky]), map([(x+1) as f32*kx,y as f32*ky]), map([(x+1) as f32*kx,(y+1) as f32*ky]), map([x as f32*kx,(y+1) as f32*ky])];
            painter.add(egui::Shape::closed_line(corners.to_vec(), egui::Stroke::new(2.0, defect_color(d["type"].as_str().unwrap_or("")))));
        }
    }

    /// Mark the exact pixel beneath the pointer without covering a defect's outer frame.
    pub fn paint_hover_pixel(&self, painter: &egui::Painter, fw: f32, fh: f32, map: impl Fn([f32; 2]) -> egui::Pos2) {
        let Some(rb) = &self.rb else { return };
        let Some([x, y]) = self.hover else { return };
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x >= rb.crop.w as f32 || y >= rb.crop.h as f32 { return; }
        let (x, y) = (x.floor(), y.floor());
        let (kx, ky) = (fw / rb.crop.w as f32, fh / rb.crop.h as f32);
        let corners = [map([x*kx, y*ky]), map([(x+1.0)*kx, y*ky]), map([(x+1.0)*kx, (y+1.0)*ky]), map([x*kx, (y+1.0)*ky])];
        let edge = (corners[1]-corners[0]).length().min((corners[3]-corners[0]).length());
        if edge < 8.0 { return; }
        let center = corners[0] + (corners[2]-corners[0])*0.5;
        let inset = corners.map(|p| center + (p-center)*0.72);
        painter.add(egui::Shape::closed_line(inset.to_vec(), egui::Stroke::new(1.0, crate::lr::TEXT_STRONG())));
    }

    /// Concise, exact value for the sensor pixel beneath the pointer.
    pub fn pixel_tip(&self) -> Option<String> {
        let rb = self.rb.as_ref()?;
        if !self.an.contains(&Overlay::Mosaic) || self.zoom_image() < 3.99 { return None; }
        let [x, y] = self.hover?;
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 { return None; }
        let (x, y) = (x as usize, y as usize);
        let z = rb.crop;
        if x >= z.w || y >= z.h { return None; }
        let b = self.crop_mosaic.as_ref().and_then(|k| self.results.get(k)).and_then(|e| e.layer("mosaic"))?.reference?;
        if x < b.x || y < b.y || x >= b.x + b.w || y >= b.y + b.h { return None; }
        let (sx, sy) = (z.x + x, z.y + y);
        let class = rb.classes.names.get(rb.class(sx, sy)).copied().unwrap_or("?");
        let mut text = format!("x {sx} · y {sy} · {class} · {:.0} DN", rb.value_dn(sx, sy));
        let defect = self.outcome(Kind::Health).and_then(|e| e.metrics["defects"].as_array()).and_then(|v| v.iter().find(|d| d["x"].as_u64() == Some(x as u64) && d["y"].as_u64() == Some(y as u64))).and_then(|d| d["type"].as_str());
        if let Some(kind) = defect {
            let kind = match kind { "hot" => "Hot", "dead" => "Dead", "stuck" => "Stuck", _ => return Some(text) };
            text.push_str(" · ");
            text.push_str(&crate::trf!("Defekt: {a}", a = kind));
        }
        Some(text)
    }

    pub fn icon(o: Overlay) -> lr::Icon {
        o.icon()
    }

    /// Does the demosaic method suit the sensor (Bayer: RCD/bilinear, X-Trans: Markesteijn/bilinear)?
    pub fn demosaic_fits(&self, i: usize) -> bool {
        match (self.sensor_type(), i) {
            (_, 0) | (_, 4) => true,
            (Some(2), 1) => true,
            (Some(6), 2 | 3) => true,
            _ => false,
        }
    }

    /// Short state of the view for the label in the image corner: what is shown, and
    /// where it waits for a zoom level or a computation. Empty: nothing to show.
    pub fn status(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = Vec::new();
        if self.rb.is_none() {
            let t = if self.apple_only.is_some() {
                crate::tr("Sensoransicht nicht verfügbar – entwickelt über Apples RAW-Engine")
            } else if self.error.is_some() {
                crate::tr("Sensordaten nicht lesbar")
            } else {
                crate::tr("Sensordaten werden gelesen …")
            };
            out.push((t.to_string(), false));
            return out;
        }
        let z = self.zoom_image();
        if self.an.contains(&Overlay::Mosaic) {
            if z < 3.99 {
                out.push((crate::tr("Mosaik ab 400 %").to_string(), false));
            } else {
                out.push((crate::tr("Mosaik").to_string(), true));
            }
        }
        if !(self.an.contains(&Overlay::Mosaic) && z >= 3.99) {
            let name = if self.demosaic == 0 { if self.sensor_type() == Some(2) { "RCD" } else { "Markesteijn 3" } } else { DEMOSAIC[self.demosaic].1 };
            if !self.demosaic_fits(self.demosaic) {
                out.push((crate::trf!("{a} passt nicht zum Sensor", a = name), false));
            } else if z < 0.99 {
                out.push((crate::trf!("{a} ab 100 %", a = name), false));
            } else {
                out.push((crate::trf!("Neutrale Ansicht – {a}, ohne Look", a = name), true));
            }
        }
        for o in Overlay::ALL {
            if o != Overlay::Mosaic && self.an.contains(&o) {
                let name = if o == Overlay::Noise { format!("{} {}", o.name(), ["R", "G", "B", crate::tr("Verhältnis")][self.channel.min(3)]) } else { o.name().to_string() };
                out.push((name, true));
            }
        }
        if !out.is_empty() && !self.running.is_empty() {
            out.push((crate::tr("rechnet …").to_string(), false));
        }
        out
    }

    /// Status label in the lower left corner of the visible image (`area`), only when
    /// something is switched on or loading.
    pub fn paint_status(&self, painter: &egui::Painter, area: egui::Rect, reserve: f32) -> Option<egui::Rect> {
        let parts = self.status();
        if parts.is_empty() {
            return None;
        }
        let font = egui::FontId::proportional(10.5);
        let mut job = egui::text::LayoutJob::default();
        for (i, (t, on)) in parts.iter().enumerate() {
            if i > 0 {
                job.append("  ·  ", 0.0, egui::TextFormat::simple(font.clone(), lr::TEXT_DIM()));
            }
            job.append(t, 0.0, egui::TextFormat::simple(font.clone(), if *on { crate::lr::gray(0xe8) } else { lr::TEXT_DIM() }));
        }
        job.wrap.max_width = (area.width() - 40.0 - reserve).max(60.0);
        job.wrap.max_rows = 1;
        let g = painter.layout_job(job);
        let r = egui::Rect::from_min_size(egui::pos2(area.left() + 10.0, area.bottom() - 10.0 - g.size().y - 8.0), g.size() + egui::vec2(16.0, 8.0));
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x12, 0x12, 0x12, 0xd8));
        painter.galley(r.min + egui::vec2(8.0, 4.0), g, crate::lr::TEXT_STRONG());
        Some(r)
    }

    /// Histogram per filter color (R, G, B: sum of the classes of that color), 1/8 EV bins
    /// from `ev_min`, and the headroom per color.
    pub fn histogram_ev(&self) -> Option<(f32, [Vec<f64>; 3], [Option<f64>; 3], Option<f64>)> {
        let k = &self.outcome(Kind::Histogram)?.metrics;
        let ev_min = k["ev_levels"]["min"].as_f64().unwrap_or(-16.0) as f32;
        let mut h: [Vec<f64>; 3] = Default::default();
        for cl in k["classes"].as_array()? {
            let c = match cl["name"].as_str().and_then(|n| n.chars().next()) {
                Some('R') => 0,
                Some('G') => 1,
                Some('B') => 2,
                _ => continue,
            };
            let bins = cl["histogram_ev"].as_array()?;
            if h[c].len() < bins.len() {
                h[c].resize(bins.len(), 0.0);
            }
            for (i, b) in bins.iter().enumerate() {
                h[c][i] += b.as_f64().unwrap_or(0.0);
            }
        }
        let hr = |i: usize| k["headroom_ev_rgb"].get(i).and_then(|v| v.as_f64()).filter(|v| v.is_finite() && *v < 100.0);
        Some((ev_min, h, [hr(0), hr(1), hr(2)], k["clipped_percent"].as_f64()))
    }

    /// SNR curve: EV steps, model dB per color, measured points (EV, dB) per color.
    #[allow(clippy::type_complexity)]
    pub fn snr_curve(&self) -> Option<(Vec<f64>, Vec<Vec<f64>>, Vec<Vec<(f64, f64)>>)> {
        let k = &self.outcome(Kind::Noise)?.metrics["snr"];
        let f = |v: &Value| v.as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect::<Vec<_>>()).unwrap_or_default();
        let ev = f(&k["ev"]);
        let model: Vec<Vec<f64>> = k["model_db"].as_array()?.iter().map(f).collect();
        let points: Vec<Vec<(f64, f64)>> = k["measure"]
            .as_array()?
            .iter()
            .map(|c| c.as_array().map(|a| a.iter().filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))).collect()).unwrap_or_default())
            .collect();
        Some((ev, model, points))
    }
}

#[cfg(test)]
mod defect_tests {
    use super::*;

    #[test]
    fn defects_readable() {
        let c = serde_json::json!({"hot": {"b": 4, "g": 3, "total": 8, "r": 1}, "stuck": {"b": 0, "g": 0, "total": 0, "r": 0}, "dead": {"b": 0, "g": 0, "total": 0, "r": 0}});
        let t = defects_text(&c);
        assert!(t.contains("8 (R 1 · G 3 · B 4)") && !t.contains('{'), "{t}");
        let none = serde_json::json!({"hot": {"total": 0}, "stuck": {"total": 0}, "dead": {"total": 0}});
        assert!(!defects_text(&none).contains('0'));
    }
}

#[cfg(test)]
mod pixel_display_tests {
    use super::*;

    #[test]
    fn crop_lane_computes_only_the_newest_crop_of_each_kind() {
        let k = [Kind::Mosaic, Kind::DemosaicComparison, Kind::Mosaic, Kind::Mosaic];
        assert_eq!(newest_per_kind(&k), vec![false, true, false, true]);
        assert_eq!(newest_per_kind(&[Kind::Mosaic]), vec![true]);
    }
    #[test]
    fn changing_mosaic_crop_releases_older_textures() {
        let ctx = egui::Context::default();
        let texture = |name: &str| ctx.load_texture(name, egui::ColorImage::filled([1,1], Color32::WHITE), egui::TextureOptions::NEAREST);
        let mut state = SensorRaw::default();
        state.crop_mosaic = Some("mosaic@8,8,8,8@4".into());
        state.tex.insert("mosaic@0,0,8,8@4:mosaic".into(), texture("old"));
        state.tex.insert("mosaic@8,8,8,8@4:mosaic".into(), texture("current"));
        state.tex.insert("demosaic@0,0,8,8@1:rcd".into(), texture("other"));
        state.request_crop(Kind::Mosaic, Region { x:16, y:16, w:8, h:8 }, 4);
        assert!(!state.tex.contains_key("mosaic@0,0,8,8@4:mosaic"));
        assert!(state.tex.contains_key("mosaic@8,8,8,8@4:mosaic"));
        assert!(state.tex.contains_key("demosaic@0,0,8,8@1:rcd"));
    }

    #[test]
    fn enabling_mosaic_requests_defects_for_the_pointer() {
        let mut state = SensorRaw::default();
        let (tx, rx) = std::sync::mpsc::channel();
        state.tx = Some(tx);
        state.toggle_on(Overlay::Mosaic);
        assert!(matches!(rx.try_recv(), Ok(Task::Analysis(Kind::Health, _, _))));
        assert!(!state.an.contains(&Overlay::Defects));
    }

    #[test]
    fn mosaic_marks_only_visible_defect_at_its_pixel_boundary() {
        let pattern = std::array::from_fn(|y| std::array::from_fn(|x| if y%2 == 0 && x%2 == 0 { 0 } else if y%2 == 1 && x%2 == 1 { 2 } else { 1 }));
        let rb = RawImage::from_mosaic(vec![0.2;64], 8, 8, pattern, sensor_view::Sensor::Bayer, [1.0;3]);
        let mut state = SensorRaw::default();
        state.rb = Some(Arc::new(rb));
        state.an.insert(Overlay::Mosaic);
        state.an.insert(Overlay::Defects);
        state.view_rect = Some((Region { x:0, y:0, w:4, h:4 }, 8.0, 0.0));
        state.crop_mosaic = Some("mosaic-test".into());
        state.results.insert("mosaic-test".into(), Arc::new(Outcome {
            kind: Kind::Mosaic,
            layers: vec![sensor_view::Layer { name: "mosaic".into(), role: Role::View, reference: Some(Region { x:0, y:0, w:8, h:8 }), canvas: sensor_view::canvas::Canvas::new_at(8,8,[0,0,0,255]) }],
            metrics: serde_json::json!({}), duration: std::time::Duration::ZERO, cached: false,
        }));
        state.results.insert(Kind::Health.name().into(), Arc::new(Outcome {
            kind: Kind::Health, layers: Vec::new(),
            metrics: serde_json::json!({"defects":[{"x":2,"y":1,"type":"hot"},{"x":7,"y":7,"type":"dead"}]}),
            duration: std::time::Duration::ZERO, cached: false,
        }));
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0,100.0))), ..Default::default() };
        let output = crate::ui_test::run(&ctx, input, |ui| state.paint_pixels(ui.painter(), [0.0,0.0,4.0,4.0], 8.0, 8.0, |p| egui::pos2(p[0]*8.0,p[1]*8.0)));
        let frames: Vec<_> = output.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Path(p) if p.closed && p.stroke.width == 2.0 => Some(p), _ => None }).collect();
        assert_eq!(frames.len(), 1);
        assert!(output.shapes.iter().all(|s| !matches!(s.shape, egui::Shape::Text(_))), "no numbers are drawn into mosaic cells");
        assert_eq!(frames[0].points, vec![egui::pos2(16.0,8.0),egui::pos2(24.0,8.0),egui::pos2(24.0,16.0),egui::pos2(16.0,16.0)]);
        assert_eq!(frames[0].stroke.color, egui::epaint::ColorMode::Solid(defect_color("hot")));
        state.hover = Some([2.2, 1.1]);
        let marked = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0,100.0))), ..Default::default() }, |ui| state.paint_hover_pixel(ui.painter(), 8.0, 8.0, |p| egui::pos2(p[0]*8.0,p[1]*8.0)));
        let mark = marked.shapes.iter().find_map(|s| match &s.shape { egui::Shape::Path(p) if p.closed && p.stroke.width == 1.0 => Some(p), _ => None }).expect("hovered pixel marked");
        assert!(mark.points.iter().all(|p| p.x > 16.0 && p.x < 24.0 && p.y > 8.0 && p.y < 16.0));
        let tip = state.pixel_tip().unwrap();
        assert!(tip.contains("x 2 · y 1") && tip.contains("3277 DN") && tip.contains("Defekt: Hot"), "{tip}");
        state.an.remove(&Overlay::Defects);
        assert!(state.pixel_tip().unwrap().contains("Defekt: Hot"));
        state.hover = Some([1.1, 1.1]);
        assert!(!state.pixel_tip().unwrap().contains("Defekt"));
        state.an.remove(&Overlay::Mosaic);
        assert!(state.pixel_tip().is_none());
        state.an.insert(Overlay::Mosaic);
        state.hover = Some([8.0, 1.0]);
        assert!(state.pixel_tip().is_none());
    }

    #[test]
    fn defect_types_have_distinct_frame_colors() {
        assert_eq!(defect_color("hot"), Color32::from_rgb(255,60,60));
        assert_eq!(defect_color("dead"), Color32::from_rgb(60,140,255));
        assert_eq!(defect_color("stuck"), Color32::from_rgb(255,200,40));
    }
}
