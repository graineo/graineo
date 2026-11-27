//! HDR rendition of a photo (gain map on export, see `gainmap` and `rawdev::export::hdr_scene`):
//! - the slider row "HDR-Leuchtkraft" under "Grundeinstellungen" (headroom in stops, switch right after the name);
//! - the map of the preview, computed in the background from the rendered preview like the export does: shown in EDR
//!   on an HDR display (`photo_display`), otherwise as the overlay "Leuchtende Bereiche" (steps +0.5/+1/+2 EV).
//!
//! Sigma photos get the gain map on export; their preview has no HDR rendition yet.

use std::sync::{mpsc, Arc};

use develop::{HdrOutput, Settings};
use eframe::egui;
use rawdev::doc::HdrMap;

use crate::lr;

/// Hint of the slider (chooses its help card in `help::for_slider`).
pub const GLOW_HINT: &str = "Lichter über Weiß auf HDR-Bildschirmen, in Blenden";

/// Shortcut: glowing areas / HDR display on and off.
pub const SHORTCUT: &str = "hdr_glow";

/// The "HDR" group of the Edit panel. `auto_source`: the photo is HDR by nature (merged HDR, gain map of its own).
pub fn section(ui: &mut egui::Ui, s: &mut Settings, auto_source: bool) {
    let film = s.film.is_active();
    // Test hook (own data folder only): `GRAINEO_HDR_ON=1` turns the HDR rendition on at first sight (screenshots)
    if s.hdr.on.is_none() && crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_HDR_ON").as_deref() == Ok("1") {
        s.hdr.on = Some(true);
    }
    let mut on = s.hdr.is_on(auto_source, film);
    // automatic and off because of the film look: the switch is shown greyed, the reason in its tooltip
    let film_off = s.hdr.on.is_none() && auto_source && film;
    // one slider row "HDR-Leuchtkraft", track and value field like every slider; the switch has its own cell right
    // after the name and its "?" (lr::slider_switch_cell), clear of the value field at the right end
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    let cell = lr::slider_switch_cell(ui.painter(), left, top, "HDR-Leuchtkraft", true);
    ui.add_enabled_ui(on, |ui| {
        let mut v = s.hdr.headroom as f64;
        if lr::slider_hint_switch(ui, "HDR-Leuchtkraft", &mut v, 0.0..=HdrOutput::MAX_HEADROOM as f64, HdrOutput::DEFAULT_HEADROOM as f64, 0.1, GLOW_HINT, cell) {
            s.hdr.headroom = v as f32;
            if ui.input(|i| i.pointer.any_down()) {
                ui.ctx().data_mut(|d| d.insert_temp(drag_id(), true));
            }
        }
    });
    let resp = lr::switch_cell(ui, cell, ui.scope_id().with("hdr-switch"), on, !film_off);
    if resp.clicked() {
        on = !on;
        s.hdr.on = Some(on);
    }
    let tip = if film_off { crate::tr("HDR aus bei Film-Look – klicken zum Einschalten") } else if on { crate::tr("HDR ausschalten") } else { crate::tr("HDR einschalten") };
    lr::hover_tip(ui, &resp, tip);
}

fn drag_id() -> egui::Id {
    egui::Id::unique("hdr-glow-drag")
}

/// Is the slider "HDR-Leuchtkraft" being dragged? (The overlay shows itself meanwhile and goes on release.)
pub fn dragging(ctx: &egui::Context) -> bool {
    let down = ctx.input(|i| i.pointer.any_down());
    let flag = ctx.data(|d| d.get_temp::<bool>(drag_id())).unwrap_or(false);
    if flag && !down {
        ctx.data_mut(|d| d.remove::<bool>(drag_id()));
    }
    flag && down
}

/// Overlay image "Leuchtende Bereiche" at the size of the preview (`w × h`, same frame as the map): the steps of the
/// HDR gain, transparent below +0.5 EV.
pub fn glow_image(map: &HdrMap, w: usize, h: usize) -> egui::ColorImage {
    let [c0, c1, c2] = lr::GLOW_LEVELS();
    let tint = |c: egui::Color32| egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 200);
    let (t0, t1, t2) = (tint(c0), tint(c1), tint(c2));
    let mut pixels = vec![egui::Color32::TRANSPARENT; w * h];
    for y in 0..h {
        let my = (y * map.h / h.max(1)).min(map.h.saturating_sub(1));
        for x in 0..w {
            let mx = (x * map.w / w.max(1)).min(map.w.saturating_sub(1));
            let g = map.log2[my * map.w + mx];
            pixels[y * w + x] = if g >= 2.0 {
                t2
            } else if g >= 1.0 {
                t1
            } else if g >= 0.5 {
                t0
            } else {
                egui::Color32::TRANSPARENT
            };
        }
    }
    egui::ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels }
}

/// Histogram of the HDR rendition of the preview (`rgb` 8-bit sRGB, `w × h`) with its map: per channel the linear
/// SDR value with the gain applied the way a reader does ((c + o) · 2^G − o). Up to SDR white in the 256 sRGB bins,
/// above in stops up to `headroom` (everything beyond lands in the last bin).
pub fn hdr_histogram(rgb: &[u8], w: usize, h: usize, map: &HdrMap, headroom: f32) -> lr::HdrHistogram {
    let lin: [f32; 256] = std::array::from_fn(|i| {
        let c = i as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let k = ((headroom * lr::HDR_BINS_PER_EV as f32).ceil() as usize).max(1);
    let mut sdr: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0; 256]);
    let mut above: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0; k]);
    let o = gainmap::OFFSET;
    for y in 0..h {
        let my = (y * map.h / h.max(1)).min(map.h.saturating_sub(1));
        for x in 0..w {
            let mx = (x * map.w / w.max(1)).min(map.w.saturating_sub(1));
            let g = map.log2[my * map.w + mx];
            let p = &rgb[(y * w + x) * 3..(y * w + x) * 3 + 3];
            for c in 0..3 {
                let code = p[c] as usize;
                if g <= 0.0 {
                    sdr[c][code] += 1;
                    continue;
                }
                let v = (lin[code] + o) * g.exp2() - o;
                if v <= 1.0 {
                    // back to the sRGB scale of the SDR part
                    let e = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
                    sdr[c][((e * 255.0).round() as usize).min(255)] += 1;
                } else {
                    let ev = v.log2() / headroom.max(1e-3);
                    above[c][((ev * k as f32) as usize).min(k - 1)] += 1;
                }
            }
        }
    }
    lr::HdrHistogram { sdr, above, headroom }
}

struct Job {
    index: usize,
    settings: Settings,
    doc: Arc<rawdev::RawDoc>,
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

/// A finished preview map: the app shows it in EDR (`photo_display::State::new_hdr_preview`) with the same 8-bit
/// preview it was computed from.
pub struct Ready {
    pub index: usize,
    pub settings: Settings,
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
    pub map: Arc<HdrMap>,
    /// full-resolution image (crop or whole) of the same photo and settings that arrived before this map
    pub detail: Option<DetailSrc>,
}

/// 8-bit image at full resolution (a crop or the whole image of the zoomed view) whose HDR version needs the map of
/// the same settings. The map is computed once per settings state; zoom and pan only cut and scale it.
pub struct DetailSrc {
    pub index: usize,
    pub settings: Settings,
    pub region: [usize; 4],
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
    /// size of the whole image at full resolution
    pub full: [usize; 2],
}

struct Done {
    index: usize,
    settings: Settings,
    w: usize,
    h: usize,
    rgb: Vec<u8>,
    map: Option<Arc<HdrMap>>,
    glow: Option<egui::ColorImage>,
    hist: Option<Arc<lr::HdrHistogram>>,
}

/// Preview maps of the open photo: one computation at a time, the newest request waits.
pub struct Preview {
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    busy: bool,
    pending: Option<Job>,
    current: Option<(usize, Settings, Arc<HdrMap>)>,
    glow: Option<(usize, Settings, egui::TextureHandle)>,
    /// histogram of the HDR rendition of photo `index` (the latest one, like the overlay)
    hist: Option<(usize, Arc<lr::HdrHistogram>)>,
    /// full-resolution image waiting for its map (the map was still being computed when it arrived)
    detail_waiting: Option<DetailSrc>,
    /// "Leuchtende Bereiche" switched on (sun button / shortcut without an HDR display)
    pub show_glow: bool,
}

impl Default for Preview {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Preview { tx, rx, busy: false, pending: None, current: None, glow: None, hist: None, detail_waiting: None, show_glow: crate::env::var_os("GRAINEO_HDR_GLOW").is_some() }
    }
}

impl Preview {
    /// A new 8-bit preview of photo `index` arrived: compute its HDR map (if HDR is on) in the background.
    pub fn request(&mut self, ctx: &egui::Context, index: usize, settings: &Settings, doc: &Arc<rawdev::RawDoc>, w: usize, h: usize, rgb: &[u8]) {
        if settings.hdr.effective(rawdev::export::hdr_auto(doc), settings.film.is_active()).is_none() {
            self.pending = None;
            self.current = None;
            self.glow = None;
            self.hist = None;
            self.detail_waiting = None;
            return;
        }
        let job = Job { index, settings: settings.clone(), doc: doc.clone(), w, h, rgb: rgb.to_vec() };
        if self.busy {
            self.pending = Some(job);
        } else {
            self.start(ctx, job);
        }
    }

    fn start(&mut self, ctx: &egui::Context, j: Job) {
        self.busy = true;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let map = rawdev::export::hdr_preview_map(&j.doc, &j.settings, &j.rgb, j.w, j.h).map(Arc::new);
            let glow = map.as_ref().map(|m| glow_image(m, j.w, j.h));
            // nothing above white (no map): the HDR histogram still shows the empty stops up to the headroom
            let headroom = j.settings.hdr.effective(rawdev::export::hdr_auto(&j.doc), j.settings.film.is_active());
            let hist = headroom.map(|hr| {
                let flat = HdrMap { w: 1, h: 1, log2: vec![0.0], headroom: 1.0, iso: true, base_headroom: 0.0, alt_headroom: 0.0 };
                Arc::new(hdr_histogram(&j.rgb, j.w, j.h, map.as_deref().unwrap_or(&flat), hr))
            });
            let _ = tx.send(Done { index: j.index, settings: j.settings, w: j.w, h: j.h, rgb: j.rgb, map, glow, hist });
            ctx.request_repaint();
        });
    }

    /// Every frame: take a finished map; returns it for the EDR display.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Ready> {
        let mut out = None;
        while let Ok(d) = self.rx.try_recv() {
            self.busy = false;
            self.hist = d.hist.map(|h| (d.index, h));
            match (d.map, d.glow) {
                (Some(map), Some(glow)) => {
                    let tex = ctx.load_texture("hdr-glow", glow, egui::TextureOptions::NEAREST);
                    self.glow = Some((d.index, d.settings.clone(), tex));
                    self.current = Some((d.index, d.settings.clone(), map.clone()));
                    let detail = self.detail_waiting.take_if(|s| s.index == d.index && s.settings == d.settings);
                    out = Some(Ready { index: d.index, settings: d.settings, w: d.w, h: d.h, rgb: d.rgb, map, detail });
                }
                _ => {
                    self.current = None;
                    self.glow = None;
                }
            }
        }
        if !self.busy {
            if let Some(j) = self.pending.take() {
                self.start(ctx, j);
            }
        }
        out
    }

    /// Map of photo `index` for these settings (crops at full resolution use it too).
    pub fn map_for(&self, index: usize, settings: &Settings) -> Option<Arc<HdrMap>> {
        self.current.as_ref().filter(|(i, s, _)| *i == index && s == settings).map(|(_, _, m)| m.clone())
    }

    /// A full-resolution image of photo `src.index` arrived: with the map of its settings at hand it is returned for
    /// the HDR conversion right away; while that map is still being computed it waits and comes with [`Ready`].
    pub fn detail(&mut self, src: DetailSrc) -> Option<(DetailSrc, Arc<HdrMap>)> {
        if let Some(map) = self.map_for(src.index, &src.settings) {
            return Some((src, map));
        }
        if self.busy || self.pending.is_some() {
            self.detail_waiting = Some(src);
        }
        None
    }

    /// Overlay texture of photo `index`: the latest one (while a slider moves the previous map stays until the next).
    pub fn glow_texture(&self, index: usize) -> Option<egui::TextureId> {
        self.glow.as_ref().filter(|(i, ..)| *i == index).map(|(.., t)| t.id())
    }

    /// HDR histogram of photo `index`: the latest one (while a slider moves the previous one stays until the next).
    pub fn histogram(&self, index: usize) -> Option<&lr::HdrHistogram> {
        self.hist.as_ref().filter(|(i, _)| *i == index).map(|(_, h)| &**h)
    }

    /// Another photo: nothing of the previous one may show.
    pub fn clear(&mut self) {
        self.detail_waiting = None;
        self.pending = None;
        self.current = None;
        self.glow = None;
        self.hist = None;
    }
}
