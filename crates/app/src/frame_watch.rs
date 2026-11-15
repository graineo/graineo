//! Test hook `GRAINEO_FRAME_LOG=1` (only with `GRAINEO_DATA_DIR`): measures how smoothly the window runs. The app
//! then draws continuously; every frame more than 50 ms after the previous one is logged with the time spent in its
//! own frame code and the slow parts of it ([`section`]). Only for measurements – it costs nothing otherwise.

use std::sync::Mutex;
use std::time::Instant;

/// Frames further apart than this are reported (ms).
const REPORT_MS: f64 = 50.0;
/// Sections shorter than this are not listed (ms).
const SECTION_MS: f64 = 5.0;

#[derive(Default)]
struct State {
    begin: Option<Instant>,
    /// frame code of the previous frame (ms)
    code_ms: f64,
    /// slow sections of the previous and of the current frame
    prev_sections: Vec<(&'static str, f64)>,
    sections: Vec<(&'static str, f64)>,
    frames: u64,
    slow: u64,
    longest: f64,
    /// frame gaps since [`window_start`] (zoom guard), ms
    window: Option<Vec<f64>>,
    /// per-second summary (`GRAINEO_FRAME_LOG=seconds`): start, frame code times and gaps of this second
    second: Option<(Instant, Vec<f64>, Vec<f64>)>,
}

/// `GRAINEO_FRAME_LOG=seconds`: one log line per second with all frames (code time and gap), for typing measurements.
fn per_second() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::env::var("GRAINEO_FRAME_LOG").as_deref() == Ok("seconds"))
}

fn second_report(s: &mut State, now: Instant, gap: f64) {
    let (start, codes, gaps) = s.second.get_or_insert_with(|| (now, Vec::new(), Vec::new()));
    gaps.push(gap);
    if now.duration_since(*start).as_secs_f64() < 1.0 {
        return;
    }
    let max = |v: &[f64]| v.iter().copied().fold(0.0f64, f64::max);
    let p95 = |v: &[f64]| {
        let mut w = v.to_vec();
        w.sort_by(|a, b| a.total_cmp(b));
        w.get(((w.len().max(1) - 1) as f64 * 0.95).round() as usize).copied().unwrap_or(0.0)
    };
    crate::log(&format!(
        "frames/s: {} frames, code max {:.1} ms p95 {:.1} ms, {} over 16 ms; gap max {:.1} ms p95 {:.1} ms, {} over 25 ms",
        gaps.len(),
        max(codes),
        p95(codes),
        codes.iter().filter(|c| **c > 16.0).count(),
        max(gaps),
        p95(gaps),
        gaps.iter().filter(|g| **g > 25.0).count()
    ));
    s.second = None;
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::env::var_os("GRAINEO_FRAME_LOG").is_some() && crate::env::var_os("GRAINEO_DATA_DIR").is_some())
}

/// Start of a frame (beginning of the app's frame code).
pub fn begin(ctx: &eframe::egui::Context) {
    if !on() {
        return;
    }
    ctx.request_repaint();
    let now = Instant::now();
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(State::default);
    if let Some(prev) = s.begin {
        let gap = (now - prev).as_secs_f64() * 1000.0;
        s.frames += 1;
        if let Some(w) = s.window.as_mut() {
            w.push(gap);
        }
        if per_second() {
            second_report(s, now, gap);
        }
        if gap > s.longest {
            s.longest = gap;
        }
        if gap > REPORT_MS {
            s.slow += 1;
            let parts: Vec<String> = s.prev_sections.iter().map(|(n, ms)| format!("{n} {ms:.0}")).collect();
            crate::log(&format!(
                "frame: {gap:.0} ms since the previous frame (its frame code {:.0} ms{}{}) – {} of {} frames slow, longest {:.0} ms",
                s.code_ms,
                if parts.is_empty() { "" } else { ": " },
                parts.join(", "),
                s.slow,
                s.frames,
                s.longest
            ));
        }
    }
    s.begin = Some(now);
    s.prev_sections.clear();
    s.sections.clear();
}

/// End of the frame code (before painting).
pub fn end() {
    if !on() {
        return;
    }
    let mut g = STATE.lock().unwrap();
    if let Some(s) = g.as_mut() {
        if let Some(b) = s.begin {
            s.code_ms = b.elapsed().as_secs_f64() * 1000.0;
            if let Some((_, codes, _)) = s.second.as_mut() {
                codes.push(s.code_ms);
            }
        }
        s.prev_sections = std::mem::take(&mut s.sections);
    }
}

/// Times a part of the frame code until it is dropped (listed with a slow frame).
pub struct Section(Option<(&'static str, Instant)>);

pub fn section(name: &'static str) -> Section {
    Section(on().then(|| (name, Instant::now())))
}

impl Drop for Section {
    fn drop(&mut self) {
        if let Some((name, t)) = self.0.take() {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms >= SECTION_MS {
                if let Some(s) = STATE.lock().unwrap().as_mut() {
                    s.sections.push((name, ms));
                }
            }
        }
    }
}

/// Test hook `GRAINEO_REPAINT_LOG=1` (only with `GRAINEO_DATA_DIR`): why the window is drawn. Every 5 s the number of
/// frames and the places in the code that asked for them (egui's repaint causes) are logged – for finding redraw loops
/// while nothing happens.
pub fn repaint_causes(ctx: &eframe::egui::Context) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| crate::env::var_os("GRAINEO_REPAINT_LOG").is_some() && crate::env::var_os("GRAINEO_DATA_DIR").is_some()) {
        return;
    }
    type Tally = (Instant, u64, std::collections::BTreeMap<String, u64>);
    static TALLY: Mutex<Option<Tally>> = Mutex::new(None);
    let mut g = TALLY.lock().unwrap();
    let (since, frames, causes) = g.get_or_insert_with(|| (Instant::now(), 0, Default::default()));
    *frames += 1;
    for c in ctx.repaint_causes() {
        *causes.entry(format!("{}:{}", c.file.rsplit('/').next().unwrap_or(c.file), c.line)).or_default() += 1;
    }
    if since.elapsed().as_secs_f64() >= 5.0 {
        let mut list: Vec<_> = causes.iter().collect();
        list.sort_by(|a, b| b.1.cmp(a.1));
        let text: Vec<String> = list.iter().take(8).map(|(k, n)| format!("{k} ×{n}")).collect();
        crate::log(&format!("repaint: {frames} frames in {:.1} s – {}", since.elapsed().as_secs_f64(), text.join(", ")));
        *g = None;
    }
}

/// Zoom guard: from now on the frame gaps are collected (see [`window_p95`]).
pub fn window_start() {
    if on() {
        STATE.lock().unwrap().get_or_insert_with(State::default).window = Some(Vec::new());
    }
}

/// 95th percentile of the frame gaps since [`window_start`] (ms).
pub fn window_p95() -> Option<f64> {
    let g = STATE.lock().unwrap();
    let mut v = g.as_ref()?.window.clone()?;
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[((v.len() - 1) as f64 * 0.95).round() as usize])
}
