//! The display at the top of the Edit and Masks trays (selection "Anzeige", histogram / parade / waveform /
//! vectorscope, line with shooting data or the values under the pointer) and its pin: pinned, the display stays at the
//! top while the tray below it scrolls. The choice is a program setting (`settings.json`, key [`KEY`], default off) and
//! applies to every tray with this display.
//!
//! Pinned and unpinned the display sits in the same place (the scroll area starts right below it), so switching does
//! not move anything while the tray is scrolled to the top. In a low window the pinned display gets lower; if even
//! then too little would be left for the tray, it scrolls along again for as long as the window stays this low (the
//! setting is kept).
//!
//! Size: three steps S/M/L ([`SIZES`], switch in the head row, double click on the display = next step), one setting
//! for every kind of display (key [`SIZE_KEY`]), so switching between histogram and vectorscope never changes the
//! height. Pinned, the step is the upper limit: a low window still shrinks the display as before.
//!
//! The parade, waveform and vectorscope images are computed off the frame by one worker ([`request`]) from the pixels
//! of the current view (`photo_copy`); a change of the kind or of the vectorscope zoom therefore needs no new render.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::{lr, vectorscope, App};

/// Key in `settings.json`.
pub const KEY: &str = "pinHistogram";
/// Id of the shortcut ([`crate::shortcuts`]).
pub const SHORTCUT: &str = "pin_histogram";

/// Key in `settings.json`: size step of the display ("s", "m", "l").
pub const SIZE_KEY: &str = "displaySize";
/// Heights of the display for the size steps S, M, L (the vectorscope uses them as its diameter).
pub const SIZES: [f32; 3] = [lr::HISTOGRAM_H, 180.0, 260.0];
const SIZE_NAMES: [&str; 3] = ["s", "m", "l"];

/// 0 = not read yet, otherwise step + 1.
static SIZE: AtomicU8 = AtomicU8::new(0);

/// Size step from a value of `settings.json` (missing or unknown: S).
pub fn size_from_value(v: &serde_json::Value) -> usize {
    v.as_str().and_then(|s| SIZE_NAMES.iter().position(|n| *n == s)).unwrap_or(0)
}

/// Current size step (0 S, 1 M, 2 L).
pub fn size_step() -> usize {
    match SIZE.load(Ordering::Relaxed) {
        0 => {
            let s = size_from_value(&library::settings::value(SIZE_KEY));
            SIZE.store(s as u8 + 1, Ordering::Relaxed);
            s
        }
        v => (v as usize - 1).min(2),
    }
}

/// Set and remember the size step.
pub fn set_size_step(step: usize) {
    let step = step.min(2);
    SIZE.store(step as u8 + 1, Ordering::Relaxed);
    library::settings::set(SIZE_KEY, serde_json::json!(SIZE_NAMES[step]));
}

/// The step after `step` (S → M → L → S).
pub fn next_size(step: usize) -> usize {
    (step + 1) % SIZES.len()
}

/// Height of the display for the current step (not pinned: inside the scroll area, always the full height).
pub fn display_height() -> f32 {
    SIZES[size_step()]
}

/// Lowest height of the pinned display (below that it is not pinned).
pub const MIN_H: f32 = 56.0;
/// The scroll area under the pinned display keeps at least this height.
pub const MIN_SCROLL: f32 = 240.0;
/// Height of the head row, the line under the display and the separator (with spacing).
pub const OVERHEAD: f32 = 58.0;

/// 0 = not read yet, 1 = off, 2 = on (read once from the settings, then kept here).
static PINNED: AtomicU8 = AtomicU8::new(0);

/// The setting from a value of `settings.json` (missing or not a boolean: off).
pub fn from_value(v: &serde_json::Value) -> bool {
    v.as_bool().unwrap_or(false)
}

/// Is the display pinned (setting)?
pub fn pinned() -> bool {
    match PINNED.load(Ordering::Relaxed) {
        0 => {
            let on = from_value(&library::settings::value(KEY));
            PINNED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        v => v == 2,
    }
}

/// Pin or release the display and remember it.
pub fn set_pinned(on: bool) {
    PINNED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    library::settings::set(KEY, serde_json::json!(on));
}

/// Height of the pinned display of the wanted height (size step) for a tray of `available` height, or `None`: not
/// pinned (too low a window).
pub fn pinned_height(available: f32, wanted: f32) -> Option<f32> {
    let h = (available - OVERHEAD - MIN_SCROLL).min(wanted);
    (h >= MIN_H).then_some(h)
}

// ── Computation of the parade / waveform / vectorscope image ──
//
// One worker at a time on the front lane (the user waits for the display; a computation takes a few milliseconds):
// requests go into a single slot, so while a slider is dragged only the newest view is computed next and nothing piles
// up. Every finished result newer than the one shown is shown (the display follows the drag at the pace of the
// computation), and the newest request is always computed last, so the display is exact once the slider stops.

/// Number of the latest request.
static JOB: AtomicU64 = AtomicU64::new(0);
/// Bookkeeping of the results, under one lock.
static STATE: Mutex<State> = Mutex::new(State { key: None, first: 0, done: 0, ready: None, next: None, working: false });

/// What a request is computed from (photo index, kind, zoom); a result for another key is never shown in its place.
type Key = (usize, u8, u8);

struct State {
    /// key of the latest request and the number of its first request with this key
    key: Option<Key>,
    first: u64,
    /// number of the newest request whose result arrived
    done: u64,
    /// result not yet picked up: photo index and image
    ready: Option<(usize, egui::ColorImage)>,
    /// request waiting for the worker (only the newest is kept)
    next: Option<Request>,
    /// a worker is running
    working: bool,
}

struct Request {
    job: u64,
    index: usize,
    kind: u8,
    zoom: u8,
    width: usize,
    height: usize,
    rgb: Arc<Vec<u8>>,
    ctx: egui::Context,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Image of a display kind (1 parade, 2 waveform, 3 vectorscope) from 8-bit RGB.
pub fn scope_image(kind: u8, rgb: &[u8], width: usize, height: usize, zoom: u8) -> egui::ColorImage {
    match kind {
        1 => lr::scope_image(rgb, width, height, lr::Scope::Parade),
        2 => lr::scope_image(rgb, width, height, lr::Scope::RgbWaveform),
        _ => vectorscope::image(rgb, zoom),
    }
}

/// Number a new request for `key`; a change of the key drops a result of the old one that was not picked up.
fn begin(st: &mut State, key: Key) -> u64 {
    let job = JOB.fetch_add(1, Ordering::SeqCst) + 1;
    if st.key != Some(key) {
        st.key = Some(key);
        st.first = job;
        st.ready = None;
    }
    job
}

/// Hand over the result of request `job`: kept when it is newer than the last one and has the key of the latest
/// request (an older result never replaces a newer one). Returns: kept.
fn publish(st: &mut State, job: u64, index: usize, img: egui::ColorImage) -> bool {
    if job <= st.done || job < st.first {
        return false;
    }
    st.done = job;
    st.ready = Some((index, img));
    true
}

/// Compute the image of display `kind` for photo `index` off the frame; the result is picked up by [`take`]. A newer
/// request supersedes an older one that has not started yet.
pub fn request(ctx: &egui::Context, index: usize, kind: u8, width: usize, height: usize, rgb: Arc<Vec<u8>>) {
    let zoom = vectorscope::zoom();
    let mut st = state();
    let job = begin(&mut st, (index, kind, zoom));
    st.next = Some(Request { job, index, kind, zoom, width, height, rgb, ctx: ctx.clone() });
    if !st.working {
        st.working = true;
        drop(st);
        crate::threads::spawn(crate::threads::Lane::Front, work);
    }
}

/// The worker: computes the waiting request until none is left.
fn work() {
    // should a computation panic, the next request starts a new worker
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            if std::thread::panicking() {
                state().working = false;
            }
        }
    }
    let _release = Release;
    loop {
        let r = {
            let mut st = state();
            match st.next.take() {
                Some(r) => r,
                None => {
                    st.working = false;
                    return;
                }
            }
        };
        let img = scope_image(r.kind, &r.rgb, r.width, r.height, r.zoom);
        if publish(&mut state(), r.job, r.index, img) {
            r.ctx.request_repaint();
        }
    }
}

/// Is a computation running whose result has not arrived yet?
pub fn pending() -> bool {
    JOB.load(Ordering::SeqCst) != state().done
}

/// The finished image for photo `index` (a result for another photo is dropped).
fn take(index: usize) -> Option<egui::ColorImage> {
    let (i, img) = state().ready.take()?;
    (i == index).then_some(img)
}

/// Frame state: the display was drawn above the scroll area (by [`App::pinned_histogram`]) with this drag of the
/// histogram, which the sliders below take over.
#[derive(Clone, Copy, Default)]
struct Drawn(Option<(lr::HistRegion, f32)>);

fn drawn_id() -> egui::Id {
    egui::Id::unique("histogram-pinned-drawn")
}

/// Pin button of the head row: Phosphor push pin, filled while pinned, tooltip with the shortcut.
fn pin_button(ui: &mut egui::Ui) {
    let on = pinned();
    let tip = lr::with_shortcut("Histogramm oben festhalten", SHORTCUT);
    // a little smaller than the other icon buttons: as high as the size switch next to it
    if lr::icon_button_sized(ui, lr::Icon::PushPin, on, &tip, egui::vec2(22.0, 18.0), 12.0) {
        set_pinned(!on);
    }
}

/// Size switch of the head row: three small fields S, M, L.
fn size_switch(ui: &mut egui::Ui) {
    let step = size_step();
    let (w, h) = (15.0, 18.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0 * w, h), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_gray(0x3a)), egui::StrokeKind::Inside);
    for (i, label) in ["S", "M", "L"].into_iter().enumerate() {
        let seg = egui::Rect::from_min_size(rect.min + egui::vec2(i as f32 * w, 0.0), egui::vec2(w, h));
        let resp = ui.interact(seg, ui.scope_id().with(("display-size", i)), egui::Sense::click());
        let on = step == i;
        if on {
            painter.rect_filled(seg.shrink(2.0), 0.0, egui::Color32::from_gray(0x4a));
        } else if resp.hovered() {
            painter.rect_filled(seg.shrink(2.0), 0.0, egui::Color32::from_gray(0x2c));
        }
        let color = if on { egui::Color32::WHITE } else if resp.hovered() { lr::TEXT() } else { lr::TEXT_DIM() };
        painter.text(seg.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.0), color);
        lr::hover_tip(ui, &resp, &crate::trf!("Größe der Anzeige: {s} (Doppelklick auf die Anzeige: nächste Größe)", s = label));
        if resp.clicked() && !on {
            set_size_step(i);
        }
    }
}

impl App {
    /// Before the scroll area of the trays: the shortcut, and the pinned display above the scroll area when it is on
    /// and fits. `with_display`: the tray shows the display (Edit, Masks), `enabled`: its photo is the active one.
    pub(crate) fn pinned_histogram(&mut self, ui: &mut egui::Ui, with_display: bool, enabled: bool) {
        if crate::shortcuts::pressed(ui.ctx(), SHORTCUT) {
            set_pinned(!pinned());
        }
        ui.ctx().data_mut(|d| d.remove_temp::<Drawn>(drawn_id()));
        if !with_display || self.current.is_none() || !pinned() {
            return;
        }
        let Some(h) = pinned_height(ui.available_height(), display_height()) else { return };
        // as wide as inside the scroll area, where the band of the scroll bar stays free: pinning moves nothing sideways
        let drag = ui
            .scope(|ui| {
                ui.set_max_width((ui.available_width() - lr::SCROLL_BAND).max(0.0));
                ui.add_enabled_ui(enabled, |ui| self.histogram_head(ui, h)).inner
            })
            .inner;
        ui.ctx().data_mut(|d| d.insert_temp(drawn_id(), Drawn(drag)));
    }

    /// The display at the top of the tray (inside the scroll area when it is not pinned). Returns the drag of the
    /// histogram (shadows / exposure / highlights).
    pub(crate) fn histogram_display(&mut self, ui: &mut egui::Ui) -> Option<(lr::HistRegion, f32)> {
        if let Some(Drawn(drag)) = ui.ctx().data_mut(|d| d.remove_temp::<Drawn>(drawn_id())) {
            return drag;
        }
        self.histogram_head(ui, display_height())
    }

    /// Compute the image of the selected display again from the current view (kind or zoom changed, or none yet);
    /// without a view (nothing rendered yet) the next render does it.
    fn refresh_scope(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.current else { return };
        let Some(e) = self.entries.get(c.index) else { return };
        match crate::photo_copy::view_of(e.image, e.copy.as_deref()) {
            Some((w, h, px)) => request(ctx, c.index, self.dev_scope, w, h, px),
            None => self.rendered_for = None,
        }
    }

    /// Head row (selection and pin), display of `h` points and the line with shooting data or values, separator.
    fn histogram_head(&mut self, ui: &mut egui::Ui, h: f32) -> Option<(lr::HistRegion, f32)> {
        let c = self.current.as_ref()?;
        let mut hist_drag = None;
        // histogram, RGB parade, waveform or vectorscope – selection on the right in the header (as in x3fuse), all
        // the same height so nothing jumps when switching
        const NAMES: [&str; 4] = ["Histogramm", "RGB-Parade", "Waveform", "Vektorskop"];
        let mut sel = self.dev_scope.min(3);
        ui.horizontal(|ui| {
            let g = lr::caps(ui.painter(), "Anzeige", egui::FontId::proportional(10.0), lr::TEXT_DIM());
            let (r, _) = ui.allocate_exact_size(egui::vec2(g.size().x, 18.0), egui::Sense::hover());
            ui.painter().galley(egui::pos2(r.left(), r.center().y - g.size().y / 2.0), g, lr::TEXT_DIM());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("dev-scope").selected_text(crate::tr(NAMES[sel as usize])).width(118.0).show_ui(ui, |ui| {
                    for (i, n) in NAMES.iter().enumerate() {
                        ui.selectable_value(&mut sel, i as u8, crate::tr(n));
                    }
                });
                pin_button(ui);
                size_switch(ui);
            });
        });
        let index = c.index;
        if let Some(img) = take(index) {
            if let Some(c) = &mut self.current {
                match &mut c.wave_tex {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => c.wave_tex = Some(ui.ctx().load_texture("wave", img, egui::TextureOptions::LINEAR)),
                }
            }
        }
        let c = self.current.as_ref()?;
        let tex = c.wave_tex.clone();
        let small = size_step() == 0;
        let mut recompute = false;
        let double = match sel {
            0 => match &c.out_hist {
                Some(hist) => {
                    let r = lr::histogram_h(ui, hist, self.show_clipping, h);
                    if r.toggle_clipping {
                        self.show_clipping = !self.show_clipping;
                    }
                    hist_drag = r.drag;
                    r.double_clicked
                }
                // not computed yet: keep its place
                None => ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click()).1.double_clicked(),
            },
            1 => lr::scope_view_h(ui, tex.as_ref(), lr::Scope::Parade, h, true),
            2 => lr::scope_view_h(ui, tex.as_ref(), lr::Scope::RgbWaveform, h, true),
            _ => {
                let r = vectorscope::view(ui, tex.as_ref(), h, small);
                recompute = r.zoom_changed;
                r.double_clicked
            }
        };
        if double {
            set_size_step(next_size(size_step()));
        }
        if sel != self.dev_scope {
            self.dev_scope = sel;
            if let Some(c) = &mut self.current {
                c.wave_tex = None;
            }
            if let Some(f) = crate::app_file("develop-view.txt") {
                let _ = std::fs::write(f, ["histogram", "parade", "waveform", "vectorscope"][sel as usize]);
            }
            if sel > 0 {
                self.refresh_scope(ui.ctx());
            }
        } else if sel > 0 && (recompute || (tex.is_none() && !pending() && !self.rendering)) {
            self.refresh_scope(ui.ctx());
        }
        let c = self.current.as_ref()?;
        // One fixed line: shooting data, and when hovering the image the sensor values in the same place – nothing
        // below shifts.
        let line = match &self.readout {
            Some(r) => egui::RichText::new(r).small().monospace().color(lr::TEXT()),
            None => egui::RichText::new(&c.loaded.summary).small().color(lr::TEXT_DIM()),
        };
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 16.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(line).truncate());
        });
        ui.separator();
        hist_drag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_is_off_unless_stored_as_true() {
        assert!(!from_value(&serde_json::Value::Null));
        assert!(!from_value(&serde_json::json!("ja")));
        assert!(!from_value(&serde_json::json!(false)));
        assert!(from_value(&serde_json::json!(true)));
        // what set_pinned writes is what pinned reads back
        for on in [false, true] {
            let mut file = serde_json::json!({});
            file[KEY] = serde_json::json!(on);
            assert_eq!(from_value(&file[KEY]), on);
        }
    }

    #[test]
    fn the_pinned_display_shrinks_in_low_windows_and_lets_go_when_too_low() {
        // plenty of room: full height
        let s = lr::HISTOGRAM_H;
        assert_eq!(pinned_height(900.0, s), Some(s));
        assert_eq!(pinned_height(OVERHEAD + MIN_SCROLL + s, s), Some(s));
        // lower: the display gives way, the scroll area keeps its minimum
        let h = pinned_height(OVERHEAD + MIN_SCROLL + 80.0, s).unwrap();
        assert_eq!(h, 80.0);
        assert!(h >= MIN_H);
        // too low: not pinned
        assert_eq!(pinned_height(OVERHEAD + MIN_SCROLL + MIN_H - 1.0, s), None);
        assert_eq!(pinned_height(100.0, s), None);
        // whenever pinned, the scroll area keeps at least its minimum, for every size step
        for wanted in SIZES {
            for a in (0..1400).step_by(7) {
                if let Some(h) = pinned_height(a as f32, wanted) {
                    assert!(a as f32 - h - OVERHEAD >= MIN_SCROLL - 0.01, "{a}");
                    assert!((MIN_H..=wanted).contains(&h));
                }
            }
        }
    }

    #[test]
    fn the_large_steps_shrink_in_a_low_window_but_never_below_the_small_one_without_need() {
        // a laptop window: the tray offers 520 pt; L gives way, M fits, S unchanged
        let a = 520.0;
        assert_eq!(pinned_height(a, SIZES[0]), Some(SIZES[0]));
        let l = pinned_height(a, SIZES[2]).unwrap();
        assert!(l < SIZES[2] && l >= SIZES[0], "{l}");
        assert_eq!(l, a - OVERHEAD - MIN_SCROLL);
        // the steps grow, S is the old height
        assert_eq!(SIZES[0], lr::HISTOGRAM_H);
        assert!(SIZES.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn the_size_step_is_read_back_and_cycles() {
        assert_eq!(size_from_value(&serde_json::Value::Null), 0);
        assert_eq!(size_from_value(&serde_json::json!("x")), 0);
        assert_eq!(size_from_value(&serde_json::json!(2)), 0);
        for (i, n) in SIZE_NAMES.iter().enumerate() {
            assert_eq!(size_from_value(&serde_json::json!(n)), i);
        }
        assert_eq!(next_size(0), 1);
        assert_eq!(next_size(1), 2);
        assert_eq!(next_size(2), 0);
    }

    #[test]
    fn the_largest_vectorscope_fits_the_narrow_tray() {
        // tray 330 pt wide minus margins and scroll band: the circle of L still fits
        let width = 330.0 - 2.0 * 10.0 - lr::SCROLL_BAND;
        assert!(SIZES[2] - 8.0 <= width, "{width}");
    }

    /// One test for the shared result slot (the statics must not be used by two tests at once).
    #[test]
    fn an_older_result_never_replaces_a_newer_one_and_the_newest_request_is_computed() {
        let img = |v: u8| egui::ColorImage::new([1, 1], vec![egui::Color32::from_gray(v)]);
        {
            let mut st = state();
            let a = begin(&mut st, (7, 3, 1));
            let b = begin(&mut st, (7, 3, 1));
            // the newer result arrives first: the older one is dropped after it
            assert!(publish(&mut st, b, 7, img(2)));
            assert!(!publish(&mut st, a, 7, img(1)));
            assert_eq!(st.ready.as_ref().map(|r| r.1.pixels[0]), Some(egui::Color32::from_gray(2)));
            // while a slider is dragged an older result of the same display is shown until the newest arrives …
            let c = begin(&mut st, (7, 3, 1));
            let d = begin(&mut st, (7, 3, 1));
            assert!(publish(&mut st, c, 7, img(3)));
            assert!(publish(&mut st, d, 7, img(4)));
            // … but never one of another kind, zoom or photo
            let e = begin(&mut st, (7, 1, 1));
            assert!(st.ready.is_none());
            let f = begin(&mut st, (7, 3, 1));
            assert!(!publish(&mut st, e, 7, img(5)));
            assert!(publish(&mut st, f, 7, img(6)));
            st.ready = None;
        }
        // through the worker: many requests in a row, the last one is what stays
        let ctx = egui::Context::default();
        let pictures: Vec<Arc<Vec<u8>>> = (0..12u8).map(|k| Arc::new(vec![k * 20, 100, 255 - k * 20].repeat(64 * 64))).collect();
        for p in &pictures {
            request(&ctx, 9, 3, 64, 64, p.clone());
        }
        let t = std::time::Instant::now();
        while pending() {
            assert!(t.elapsed() < std::time::Duration::from_secs(10), "the newest request was not computed");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let want = scope_image(3, &pictures[11], 64, 64, vectorscope::zoom());
        assert_eq!(take(9).map(|i| i.pixels), Some(want.pixels));
        // nothing is left waiting
        assert!(state().next.is_none());
    }
}
