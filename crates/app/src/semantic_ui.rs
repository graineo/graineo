//! Semantic search ("similar in meaning"): relevance values, strictness slider, value in the cell.
//! The ranking itself (`library::similar::scored`/`keep`) lives in the library crate; here only
//! the state of the current query and the small controls.

use eframe::egui::{self, Color32};
use library::similar::{keep, limits, percent, Scored};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::lr;

/// Settings key of the strictness (0…1).
pub const KEY: &str = "semanticStrictness";

/// Pause after the last keystroke before the calculation starts.
const DEBOUNCE: Duration = Duration::from_millis(280);

/// Candidates of the semantic search per query (the relevance threshold decides how many stay).
const CANDIDATES: usize = 500;

/// Result of a background calculation.
pub struct JobResult {
    id: u64,
    pub text: String,
    pub list: Vec<Scored>,
    pub lib_size: usize,
    pub ms: f64,
}

/// State of the semantic part for the current query.
pub struct View {
    /// All candidates (descending by relevance), independent of the strictness.
    all: Vec<Scored>,
    /// Photos in the library with a vector (for the library-size dependent threshold).
    lib_size: usize,
    /// Strictness 0…1 (setting).
    pub strict: f32,
    /// Percent of the best hit for each kept hit.
    pub score: HashMap<i64, u32>,
    /// The separator row (with slider) is shown: there are hits, or a lenient setting would have some.
    pub row: bool,
    /// Number of hits kept at the moment.
    pub kept: usize,
    // background calculation: only the newest request (number) counts
    latest: Arc<AtomicU64>,
    slot: Arc<Mutex<Option<JobResult>>>,
    wish: String,
    wish_since: Option<Instant>,
    started: String,
}

impl View {
    pub fn new() -> View {
        let strict = library::settings::value(KEY).as_f64().map_or(library::similar::DEFAULT_STRICTNESS, |v| v as f32).clamp(0.0, 1.0);
        View { all: Vec::new(), lib_size: 0, strict, score: HashMap::new(), row: false, kept: 0, latest: Arc::new(AtomicU64::new(0)), slot: Arc::new(Mutex::new(None)), wish: String::new(), wish_since: None, started: String::new() }
    }

    pub fn clear(&mut self) {
        // withdraw the wish; a calculation still running is outdated from now on
        if !self.wish.is_empty() || !self.started.is_empty() {
            self.latest.fetch_add(1, Ordering::Relaxed);
        }
        self.wish.clear();
        self.started.clear();
        self.wish_since = None;
        self.all.clear();
        self.score.clear();
        self.row = false;
        self.kept = 0;
    }

    /// The query text that should have semantic hits (typing restarts the pause).
    pub fn wish(&mut self, text: &str) {
        if self.wish != text {
            self.wish = text.to_string();
            self.wish_since = Some(Instant::now());
            // an older calculation is outdated; the same text typed again must start anew
            self.started.clear();
            self.latest.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn wanted(&self) -> Option<&str> {
        (!self.wish.is_empty()).then_some(self.wish.as_str())
    }

    /// Is a calculation for exactly this text already running (or finished and waiting to be picked up)?
    pub fn started_for(&self, text: &str) -> bool {
        self.started == text
    }

    /// Time left of the typing pause (`None`: over).
    pub fn pause_left(&self) -> Option<Duration> {
        let left = DEBOUNCE.checked_sub(self.wish_since?.elapsed())?;
        (!left.is_zero()).then_some(left)
    }

    /// Show "running": a wish without a result yet (pause, model loading, calculation).
    pub fn busy(&self, done_for: &str) -> bool {
        !self.wish.is_empty() && self.wish != done_for
    }

    /// Start the calculation in a background thread (the UI thread never computes).
    pub fn start(&mut self, state: Arc<crate::library_ui::SemState>, text: String, ctx: egui::Context) {
        let id = self.latest.fetch_add(1, Ordering::Relaxed) + 1;
        self.started = text.clone();
        let text_log = text.clone();
        let (latest, slot) = (self.latest.clone(), self.slot.clone());
        let job = move || {
            let t = Instant::now();
            let q = state.enc.embed(&text);
            if latest.load(Ordering::Relaxed) != id {
                return;
            }
            let list = library::similar::scored(&state.image, state.text.as_ref(), &q, CANDIDATES);
            if latest.load(Ordering::Relaxed) != id {
                return;
            }
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(JobResult { id, text, list, lib_size: state.image.count(), ms: t.elapsed().as_secs_f64() * 1000.0 });
            crate::log(&format!("bedeutungssuche: „{}“ in {:.0} ms ({} Vektoren)", text_log, t.elapsed().as_secs_f64() * 1000.0, state.image.count()));
            ctx.request_repaint();
        };
        // test aid for before/after measurements: compute in the UI thread like the old code did
        if crate::env::var_os("GRAINEO_SEM_SYNC").is_some() {
            job();
        } else {
            std::thread::spawn(job);
        }
    }

    /// Result of the newest request, if it is there (older ones are dropped).
    pub fn take_result(&mut self) -> Option<JobResult> {
        let r = self.slot.try_lock().ok()?.take()?;
        (r.id == self.latest.load(Ordering::Relaxed) && r.text == self.wish).then_some(r)
    }

    /// New candidates for a query; returns file → rank of the kept hits.
    pub fn set(&mut self, all: Vec<Scored>, lib_size: usize) -> HashMap<i64, usize> {
        self.all = all;
        self.lib_size = lib_size;
        self.apply()
    }

    /// Filter the candidates with the current strictness (no recomputation); file → rank.
    pub fn apply(&mut self) -> HashMap<i64, usize> {
        let n = keep(&self.all, self.strict, self.lib_size);
        self.kept = n;
        let best = self.all.first().map_or(0.0, |s| s.z);
        self.score = self.all.iter().take(n).map(|s| (s.file, percent(s.z, best))).collect();
        self.row = n > 0 || keep(&self.all, 0.0, self.lib_size) > 0;
        self.all.iter().take(n).enumerate().map(|(r, s)| (s.file, r)).collect()
    }

    /// z threshold currently in force (for the tooltip of the slider).
    pub fn floor(&self) -> f32 {
        limits(self.strict, self.lib_size).0
    }
}

/// Strictness slider (thin track, small handle) with caps label to its left, right-aligned in `row`
/// at height `y`. Returns (changed, released). No layout jump: the space is fixed.
pub fn strictness_slider(ui: &mut egui::Ui, row: egui::Rect, y: f32, value: &mut f32) -> (bool, bool) {
    let track_w = 110.0;
    let track = egui::Rect::from_min_size(egui::pos2(row.right() - 20.0 - track_w, y - 8.0), egui::vec2(track_w, 16.0));
    let label = lr::caps(ui.painter(), crate::tr("Strenge"), lr::semibold(10.0), lr::SLIDER_LABEL);
    let label_pos = egui::pos2(track.left() - 10.0 - label.size().x, y - label.size().y / 2.0);
    ui.painter().galley(label_pos, label, lr::SLIDER_LABEL);
    let resp = ui.interact(track.expand2(egui::vec2(6.0, 0.0)), ui.id().with("sem-strenge"), egui::Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() || resp.clicked() || resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let v = (v * 20.0).round() / 20.0;
            if (v - *value).abs() > 1e-4 {
                *value = v;
                changed = true;
            }
        }
    }
    let active = resp.hovered() || resp.dragged();
    let line = if active { Color32::from_gray(0x8a) } else { Color32::from_gray(0x5a) };
    ui.painter().line_segment([egui::pos2(track.left(), y), egui::pos2(track.right(), y)], egui::Stroke::new(1.0, line));
    let hx = track.left() + track.width() * *value;
    ui.painter().circle_filled(egui::pos2(hx, y), if active { 5.0 } else { 4.0 }, Color32::from_gray(if active { 0xe0 } else { 0xb4 }));
    lr::hover_tip(ui, &resp, crate::tr("Strenge der Bedeutungssuche: weiter links zeigt mehr, weiter rechts nur die deutlichsten Treffer"));
    (changed, resp.drag_stopped() || (resp.clicked() && !resp.dragged()))
}

/// Remember the strictness (setting).
pub fn save(value: f32) {
    library::settings::set(KEY, serde_json::json!(value as f64));
}

/// Small dimmed value of a hit in the header of a cell (percent of the best hit), left of the format.
/// Only when the cell is big enough (`room` = free width right of the number).
pub fn cell_value(ui: &egui::Ui, header: egui::Rect, number_right: f32, percent: u32) {
    let x = number_right + 4.0;
    if header.right() - 25.0 - x < 74.0 {
        return;
    }
    let text = format!("{percent} %");
    let rect = egui::Rect::from_min_size(egui::pos2(x, header.top() + 3.0), egui::vec2(34.0, header.height() - 6.0));
    ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(10.0), Color32::from_gray(0x70));
    let resp = ui.interact(rect, ui.id().with(("sem-wert", header.min.x as i32, header.min.y as i32)), egui::Sense::hover());
    lr::hover_tip(ui, &resp, crate::tr("Übereinstimmung mit der Anfrage in Prozent des besten Treffers"));
}

/// Small calm spinner inside the search field (left of the clear cross) while the semantic search
/// works; tooltip names it. Fixed place, nothing moves.
pub fn busy_mark(ui: &mut egui::Ui, frame: egui::Rect, has_cross: bool) {
    let x = frame.right() - if has_cross { 36.0 } else { 18.0 };
    let r = egui::Rect::from_center_size(egui::pos2(x, frame.center().y), egui::vec2(11.0, 11.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
    crate::activity::calm_spinner(&mut child, 11.0, lr::TEXT_DIM);
    let resp = ui.interact(r.expand(3.0), ui.id().with("sem-laeuft"), egui::Sense::hover());
    lr::hover_tip(ui, &resp, crate::tr("Bedeutungssuche läuft …"));
}
