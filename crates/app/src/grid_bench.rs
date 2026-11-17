//! Test aid `GRAINEO_GRID_BENCH="out=<file.json>;start=6;speeds=5,10,20,40,80,160;secs=6;jumps=24;seed=1"` (only
//! with `GRAINEO_DATA_DIR`): measures whether the library grid shows its thumbnails while scrolling. The grid runs a
//! fixed program by itself and records every frame:
//!
//! 1. first screen at the top: time until every visible tile shows its thumbnail;
//! 2. steady scrolling at fixed speeds (tile rows per second, independent of the frame rate), each speed in its own
//!    part of the library (a jump there, wait until complete, then scroll for `secs` seconds);
//! 3. jumps: to the end, to the middle, to `jumps` random positions (seeded, spread over the part of the library
//!    the scrolling has not touched) and back to the top; after each jump it waits until every visible tile has its
//!    thumbnail (at most `timeout` seconds) and stays there for `dwell` seconds.
//!
//! Per frame: how many tiles are visible (wholly or partly inside the grid's view) and how many of them already show
//! their thumbnail, the frame interval (time between two frames of the grid) and the time spent building the grid.
//! At the end the result goes into `out` (JSON) and `out` with `.frames.tsv` (every frame), a summary line into the
//! trace log, and the app closes.
//!
//! The scroll position is set directly (like the scroll bar or Home/End), not through input events: the measurement
//! covers drawing and loading the thumbnails, not the kinetic scrolling of the trackpad. Never active without the
//! variable; the app does nothing extra otherwise.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Config {
    pub out: PathBuf,
    /// start this many seconds after the library opened
    pub start_s: f32,
    /// steady scrolling: speeds in tile rows per second
    pub speeds: Vec<f32>,
    /// seconds per speed
    pub secs: f32,
    /// random jumps
    pub jumps: usize,
    pub seed: u64,
    /// longest wait for a complete screen after a jump (s)
    pub timeout_s: f32,
    /// stay this long on a complete screen before the next step (s)
    pub dwell_s: f32,
}

impl Config {
    pub fn parse(v: &str) -> Config {
        let mut c = Config { out: PathBuf::from("grid-bench.json"), start_s: 6.0, speeds: vec![5.0, 10.0, 20.0, 40.0, 80.0, 160.0], secs: 6.0, jumps: 24, seed: 1, timeout_s: 10.0, dwell_s: 0.4 };
        for part in v.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let v = v.trim();
            let f = v.parse::<f32>().ok();
            match k.trim() {
                "out" => c.out = PathBuf::from(v),
                "start" => c.start_s = f.unwrap_or(c.start_s),
                "speeds" => c.speeds = v.split(',').filter_map(|s| s.trim().parse().ok()).filter(|s: &f32| *s > 0.0).collect(),
                "secs" => c.secs = f.unwrap_or(c.secs),
                "jumps" => c.jumps = v.parse().unwrap_or(c.jumps),
                "seed" => c.seed = v.parse().unwrap_or(c.seed),
                "timeout" => c.timeout_s = f.unwrap_or(c.timeout_s),
                "dwell" => c.dwell_s = f.unwrap_or(c.dwell_s),
                _ => {}
            }
        }
        c
    }
}

/// One step of the program.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// go to row `row` (clamped to the end) and wait until complete; `measured`: counts for the jump statistics
    Jump { label: String, row: f32, measured: bool },
    /// scroll from row `from` at `rows_per_s` for the configured time
    Scroll { rows_per_s: f32, from: f32 },
}

/// The program for a grid of `rows` tile rows (`view_rows` of them fit on the screen).
fn program(c: &Config, rows: f32, view_rows: f32) -> Vec<Step> {
    let last = (rows - view_rows).max(0.0);
    let mut v = vec![Step::Jump { label: "first screen".into(), row: 0.0, measured: false }];
    // steady scrolling in the first part of the library, one stretch per speed with a gap between them
    let mut at = view_rows * 2.0;
    for &s in &c.speeds {
        let len = s * c.secs + view_rows;
        if at + len > last * 0.4 && at > view_rows * 2.0 {
            // not enough room: start this speed again near the top (counted as a revisit)
            at = view_rows * 2.0;
        }
        v.push(Step::Jump { label: format!("before {s} rows/s"), row: at, measured: false });
        v.push(Step::Scroll { rows_per_s: s, from: at });
        at += len + view_rows * 2.0;
    }
    // jumps into the part the scrolling did not touch
    v.push(Step::Jump { label: "end".into(), row: last, measured: true });
    v.push(Step::Jump { label: "middle".into(), row: last * 0.7, measured: true });
    let mut rng = c.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for i in 0..c.jumps {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let f = 0.45 + (rng >> 11) as f32 / (1u64 << 53) as f32 * 0.5;
        v.push(Step::Jump { label: format!("random {}", i + 1), row: (last * f).floor(), measured: true });
    }
    v.push(Step::Jump { label: "top".into(), row: 0.0, measured: true });
    v
}

/// What the grid tells the bench before drawing.
pub struct Before {
    pub now: Instant,
    /// seconds since the library opened
    pub since_open: f32,
    /// the grid has photos
    pub ready: bool,
    /// row pitch (points)
    pub pitch: f32,
    /// tile rows
    pub rows: usize,
    /// height of the grid's view (points; of the previous frame)
    pub view_h: f32,
}

/// One frame as the grid drew it.
pub struct Drawn<'a> {
    pub now: Instant,
    /// (file, thumbnail shown) of the tiles inside the view
    pub tiles: &'a [(i64, bool)],
    /// first and last tile row inside the view
    pub row_span: (usize, usize),
    pub y: f32,
    /// time spent building the grid this frame (ms)
    pub build_ms: f64,
}

#[derive(Clone, Debug)]
struct Frame {
    t_ms: f64,
    step: usize,
    y: f32,
    visible: usize,
    with_image: usize,
    interval_ms: f64,
    build_ms: f64,
}

#[derive(Clone, Debug)]
struct JumpResult {
    label: String,
    row: f32,
    measured: bool,
    revisit: bool,
    ms: Option<f64>,
    first_placeholder_pct: f64,
    frames: usize,
}

#[derive(Clone, Debug, Default)]
struct ScrollResult {
    rows_per_s: f32,
    first: usize,
    last: usize,
    appeared: usize,
    appeared_empty: usize,
    fill_ms: Vec<f64>,
    left_empty: usize,
    seconds: f32,
}

pub struct GridBench {
    cfg: Config,
    plan: Vec<Step>,
    step: usize,
    step_t0: Option<Instant>,
    started: Option<Instant>,
    opened_offset_s: f32,
    last_frame: Option<Instant>,
    frames: Vec<Frame>,
    jumps: Vec<JumpResult>,
    scrolls: Vec<ScrollResult>,
    scroll_step: Option<usize>,
    /// jump: when complete (then dwell)
    complete_at: Option<Instant>,
    jump_frames: usize,
    first_placeholder: Option<f64>,
    /// scrolling: tiles of the previous frame, tiles that came in empty (since when)
    previous: HashSet<i64>,
    pending: HashMap<i64, Instant>,
    visited: Vec<(usize, usize)>,
    pitch: f32,
    rows: usize,
    view_rows: f32,
    pub info: String,
}

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        n as f64 * 100.0 / of as f64
    }
}

/// Percentile (nearest rank on the sorted values).
fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn spread(v: &[f64]) -> serde_json::Value {
    let mut s: Vec<f64> = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let mean = if s.is_empty() { 0.0 } else { s.iter().sum::<f64>() / s.len() as f64 };
    serde_json::json!({ "n": s.len(), "mean": r1(mean), "median": r1(quantile(&s, 0.5)), "p95": r1(quantile(&s, 0.95)), "p99": r1(quantile(&s, 0.99)), "max": r1(s.last().copied().unwrap_or(0.0)) })
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn frame_stats(f: &[&Frame]) -> serde_json::Value {
    let iv: Vec<f64> = f.iter().map(|f| f.interval_ms).filter(|v| *v > 0.0).collect();
    let n = iv.len().max(1) as f64;
    let over = |ms: f64| r1(iv.iter().filter(|&&v| v > ms).count() as f64 * 100.0 / n);
    let build: Vec<f64> = f.iter().map(|f| f.build_ms).collect();
    let secs = iv.iter().sum::<f64>() / 1000.0;
    let mut j = spread(&iv);
    j["over_16_7_pct"] = over(1000.0 / 60.0).into();
    j["over_33_pct"] = over(1000.0 / 30.0).into();
    j["fps"] = r1(if secs > 0.0 { iv.len() as f64 / secs } else { 0.0 }).into();
    serde_json::json!({ "interval_ms": j, "grid_build_ms": spread(&build) })
}

fn placeholder_stats(f: &[&Frame]) -> serde_json::Value {
    let (vis, with): (usize, usize) = f.iter().fold((0, 0), |a, f| (a.0 + f.visible, a.1 + f.with_image));
    let worst = f.iter().map(|f| pct(f.visible - f.with_image, f.visible)).fold(0.0, f64::max);
    let any = f.iter().filter(|f| f.with_image < f.visible).count();
    serde_json::json!({
        "mean_pct": r1(pct(vis - with, vis)),
        "worst_frame_pct": r1(worst),
        "frames_with_placeholder_pct": r1(pct(any, f.len())),
        "visible_tiles_mean": r1(vis as f64 / f.len().max(1) as f64),
    })
}

impl GridBench {
    pub fn from_env() -> Option<GridBench> {
        crate::env::var_os("GRAINEO_DATA_DIR")?;
        let v = crate::env::var("GRAINEO_GRID_BENCH").ok()?;
        Some(GridBench::new(Config::parse(&v)))
    }

    pub fn new(cfg: Config) -> GridBench {
        GridBench {
            cfg,
            plan: Vec::new(),
            step: 0,
            step_t0: None,
            started: None,
            opened_offset_s: 0.0,
            last_frame: None,
            frames: Vec::new(),
            jumps: Vec::new(),
            scrolls: Vec::new(),
            scroll_step: None,
            complete_at: None,
            jump_frames: 0,
            first_placeholder: None,
            previous: HashSet::new(),
            pending: HashMap::new(),
            visited: Vec::new(),
            pitch: 1.0,
            rows: 0,
            view_rows: 1.0,
            info: String::new(),
        }
    }

    /// Scroll position for this frame (`None`: leave it).
    pub fn offset(&mut self, b: Before) -> Option<f32> {
        if self.started.is_none() {
            if !(b.ready && b.since_open >= self.cfg.start_s && b.view_h > 0.0 && b.pitch > 0.0) {
                return None;
            }
            self.started = Some(b.now);
            self.opened_offset_s = b.since_open;
            self.pitch = b.pitch;
            self.rows = b.rows;
            self.view_rows = b.view_h / b.pitch;
            self.plan = program(&self.cfg, b.rows as f32, self.view_rows);
        }
        // a step starts with the frame that first applies its position
        let t0 = *self.step_t0.get_or_insert(b.now);
        let last_y = (b.rows as f32 * self.pitch - b.view_h).max(0.0);
        match self.plan.get(self.step)? {
            Step::Jump { row, .. } => Some((row * self.pitch).min(last_y)),
            Step::Scroll { rows_per_s, from } => {
                let t = b.now.duration_since(t0).as_secs_f32();
                Some(((from + t * rows_per_s) * self.pitch).min(last_y))
            }
        }
    }

    fn next_step(&mut self) {
        self.step += 1;
        self.step_t0 = None;
        self.complete_at = None;
        self.jump_frames = 0;
        self.first_placeholder = None;
        self.previous.clear();
        self.pending.clear();
    }

    fn overlaps(&self, a: usize, b: usize) -> bool {
        self.visited.iter().any(|&(x, y)| a <= y && x <= b)
    }

    /// After drawing a frame. Returns `true` when the program is done (the report is written).
    pub fn frame(&mut self, d: Drawn) -> bool {
        let Some(started) = self.started else { return false };
        let Some(step) = self.plan.get(self.step).cloned() else { return false };
        let interval = self.last_frame.map_or(0.0, |l| d.now.duration_since(l).as_secs_f64() * 1000.0);
        self.last_frame = Some(d.now);
        let with = d.tiles.iter().filter(|t| t.1).count();
        self.frames.push(Frame { t_ms: d.now.duration_since(started).as_secs_f64() * 1000.0, step: self.step, y: d.y, visible: d.tiles.len(), with_image: with, interval_ms: interval, build_ms: d.build_ms });
        let t0 = self.step_t0.unwrap_or(d.now);
        let complete = !d.tiles.is_empty() && with == d.tiles.len();
        match step {
            Step::Jump { label, row, measured } => {
                self.jump_frames += 1;
                if self.first_placeholder.is_none() {
                    self.first_placeholder = Some(pct(d.tiles.len() - with, d.tiles.len()));
                    // revisit: rows already seen before this jump (thumbnails may still be in memory)
                    let revisit = self.overlaps(d.row_span.0, d.row_span.1);
                    self.jumps.push(JumpResult { label, row, measured, revisit, ms: None, first_placeholder_pct: self.first_placeholder.unwrap_or(0.0), frames: 0 });
                    self.visited.push(d.row_span);
                }
                let waited = d.now.duration_since(t0).as_secs_f32();
                if complete && self.complete_at.is_none() {
                    self.complete_at = Some(d.now);
                    if let Some(j) = self.jumps.last_mut() {
                        j.ms = Some(d.now.duration_since(t0).as_secs_f64() * 1000.0);
                        j.frames = self.jump_frames;
                    }
                }
                let done = match self.complete_at {
                    Some(c) => d.now.duration_since(c).as_secs_f32() >= self.cfg.dwell_s,
                    None => waited >= self.cfg.timeout_s,
                };
                if done {
                    if self.complete_at.is_none() {
                        if let Some(j) = self.jumps.last_mut() {
                            j.frames = self.jump_frames;
                        }
                    }
                    self.next_step();
                }
            }
            Step::Scroll { rows_per_s, from } => {
                if self.scroll_step != Some(self.step) {
                    self.scroll_step = Some(self.step);
                    self.scrolls.push(ScrollResult { rows_per_s, first: self.frames.len() - 1, last: self.frames.len() - 1, ..Default::default() });
                }
                let first_frame = self.previous.is_empty();
                let current: HashSet<i64> = d.tiles.iter().map(|t| t.0).collect();
                let s = self.scrolls.last_mut().expect("pushed above");
                s.last = self.frames.len() - 1;
                for &(file, has) in d.tiles {
                    if has {
                        if let Some(t) = self.pending.remove(&file) {
                            s.fill_ms.push(d.now.duration_since(t).as_secs_f64() * 1000.0);
                        }
                    }
                    if first_frame || self.previous.contains(&file) {
                        continue;
                    }
                    s.appeared += 1;
                    if !has {
                        s.appeared_empty += 1;
                        self.pending.insert(file, d.now);
                    }
                }
                let gone: Vec<i64> = self.pending.keys().copied().filter(|f| !current.contains(f)).collect();
                for f in gone {
                    self.pending.remove(&f);
                    s.left_empty += 1;
                }
                self.previous = current;
                let t = d.now.duration_since(t0).as_secs_f32();
                s.seconds = t;
                let end = (from + t * rows_per_s) as usize;
                let at_end = d.row_span.1 + 1 >= self.rows;
                if t >= self.cfg.secs || at_end {
                    self.visited.push((from as usize, end + self.view_rows.ceil() as usize));
                    self.next_step();
                }
            }
        }
        if self.step < self.plan.len() {
            return false;
        }
        let (json, line) = self.report();
        let tsv = self.cfg.out.with_extension("frames.tsv");
        let mut t = String::from("t_ms\tstep\ty\tvisible\twith_image\tinterval_ms\tbuild_ms\n");
        for f in &self.frames {
            t.push_str(&format!("{:.1}\t{}\t{:.0}\t{}\t{}\t{:.2}\t{:.3}\n", f.t_ms, f.step, f.y, f.visible, f.with_image, f.interval_ms, f.build_ms));
        }
        if let Some(p) = self.cfg.out.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(&tsv, t);
        let _ = std::fs::write(&self.cfg.out, serde_json::to_string_pretty(&json).unwrap_or_default());
        crate::log(&line);
        true
    }

    fn report(&self) -> (serde_json::Value, String) {
        let all: Vec<&Frame> = self.frames.iter().collect();
        let scroll: Vec<serde_json::Value> = self
            .scrolls
            .iter()
            .map(|s| {
                let f: Vec<&Frame> = self.frames[s.first..=s.last].iter().collect();
                let mut j = serde_json::json!({
                    "rows_per_s": s.rows_per_s,
                    "seconds": r1(s.seconds as f64),
                    "frames": f.len(),
                    "placeholders": placeholder_stats(&f),
                    "tiles_appeared": s.appeared,
                    "tiles_appeared_empty_pct": r1(pct(s.appeared_empty, s.appeared)),
                    "fill_ms": spread(&s.fill_ms),
                    "tiles_left_empty": s.left_empty,
                });
                j["frames_stats"] = frame_stats(&f);
                j
            })
            .collect();
        let measured: Vec<&JumpResult> = self.jumps.iter().filter(|j| j.measured && !j.revisit).collect();
        let ms: Vec<f64> = measured.iter().filter_map(|j| j.ms).collect();
        let timeouts = measured.iter().filter(|j| j.ms.is_none()).count();
        let first = self.jumps.first();
        let jumps: Vec<serde_json::Value> = self
            .jumps
            .iter()
            .map(|j| serde_json::json!({ "label": j.label, "row": j.row, "measured": j.measured, "revisit": j.revisit, "ms_to_complete": j.ms.map(r1), "first_frame_placeholder_pct": r1(j.first_placeholder_pct), "frames": j.frames }))
            .collect();
        let jump_summary = serde_json::json!({ "n": measured.len(), "timeouts": timeouts, "ms_to_complete": spread(&ms), "first_frame_placeholder_pct": spread(&measured.iter().map(|j| j.first_placeholder_pct).collect::<Vec<_>>()) });
        let json = serde_json::json!({
            "tool": "grid-bench",
            "version": 1,
            "grid": self.info,
            "rows": self.rows,
            "view_rows": r1(self.view_rows as f64),
            "pitch_pt": self.pitch,
            "started_after_open_s": r1(self.opened_offset_s as f64),
            "config": { "speeds_rows_per_s": self.cfg.speeds, "secs": self.cfg.secs, "jumps": self.cfg.jumps, "seed": self.cfg.seed, "timeout_s": self.cfg.timeout_s, "dwell_s": self.cfg.dwell_s },
            "first_screen_ms": first.and_then(|j| j.ms).map(r1),
            "scroll": scroll,
            "jumps": jumps,
            "jump_summary": jump_summary,
            "all_frames": frame_stats(&all),
            "all_placeholders": placeholder_stats(&all),
            "process_memory_bytes": library::platform::info::process_memory(),
        });
        let line = format!(
            "grid-bench: {} frames; jumps n={} complete median {:.0} ms p95 {:.0} ms max {:.0} ms, timeouts {timeouts}; placeholders mean {:.1} %; {}",
            self.frames.len(),
            measured.len(),
            json["jump_summary"]["ms_to_complete"]["median"].as_f64().unwrap_or(0.0),
            json["jump_summary"]["ms_to_complete"]["p95"].as_f64().unwrap_or(0.0),
            json["jump_summary"]["ms_to_complete"]["max"].as_f64().unwrap_or(0.0),
            json["all_placeholders"]["mean_pct"].as_f64().unwrap_or(0.0),
            self.info
        );
        (json, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses() {
        let c = Config::parse("out=/x/r.json;speeds=5, 20;secs=2;jumps=3;seed=9;timeout=4;dwell=0.1");
        assert_eq!(c.out, PathBuf::from("/x/r.json"));
        assert_eq!(c.speeds, vec![5.0, 20.0]);
        assert_eq!((c.secs, c.jumps, c.seed, c.timeout_s, c.dwell_s), (2.0, 3, 9, 4.0, 0.1));
    }

    #[test]
    fn program_order_and_ranges() {
        let c = Config::parse("speeds=5,10;secs=2;jumps=4");
        let p = program(&c, 8000.0, 6.0);
        // first screen, (jump + scroll) per speed, end, middle, 4 random, top
        assert_eq!(p.len(), 1 + 2 * 2 + 2 + 4 + 1);
        assert!(matches!(&p[0], Step::Jump { measured: false, row, .. } if *row == 0.0));
        assert!(matches!(&p[2], Step::Scroll { rows_per_s, .. } if *rows_per_s == 5.0));
        for s in &p[5..p.len() - 1] {
            let Step::Jump { row, measured, .. } = s else { panic!("jump expected") };
            assert!(*measured && *row >= (8000.0 - 6.0) * 0.45 - 1.0 && *row <= 8000.0 - 6.0, "{s:?}");
        }
        assert_eq!(p, program(&c, 8000.0, 6.0), "seeded: same program every run");
    }

    #[test]
    fn measures_a_tiny_program() {
        let out = std::env::temp_dir().join(format!("grid-bench-test-{}.json", std::process::id()));
        let c = Config::parse(&format!("out={};start=0;speeds=10;secs=0.04;jumps=0;dwell=0.02;timeout=1", out.display()));
        let mut b = GridBench::new(c);
        let t0 = Instant::now();
        let before = |t: Instant| Before { now: t, since_open: 1.0, ready: true, pitch: 100.0, rows: 1000, view_h: 300.0 };
        // first screen: empty, then complete after 30 ms, dwell 20 ms
        assert_eq!(b.offset(before(t0)), Some(0.0));
        let ms = |n: u64| t0 + Duration::from_millis(n);
        assert!(!b.frame(Drawn { now: ms(0), tiles: &[(1, false), (2, false)], row_span: (0, 3), y: 0.0, build_ms: 1.0 }));
        assert!(!b.frame(Drawn { now: ms(30), tiles: &[(1, true), (2, true)], row_span: (0, 3), y: 0.0, build_ms: 1.0 }));
        assert!(!b.frame(Drawn { now: ms(55), tiles: &[(1, true), (2, true)], row_span: (0, 3), y: 0.0, build_ms: 1.0 }));
        assert_eq!(b.jumps[0].ms.map(|v| v.round()), Some(30.0));
        // jump before scrolling: row 6 (two screens)
        assert_eq!(b.offset(before(ms(56))), Some(600.0));
        assert!(!b.frame(Drawn { now: ms(60), tiles: &[(7, true)], row_span: (6, 9), y: 600.0, build_ms: 1.0 }));
        assert!(!b.frame(Drawn { now: ms(90), tiles: &[(7, true)], row_span: (6, 9), y: 600.0, build_ms: 1.0 }));
        // scrolling: 10 rows/s from row 6
        let y = b.offset(before(ms(100))).unwrap();
        assert!((y - 600.0).abs() < 15.0, "{y}");
        assert!(!b.frame(Drawn { now: ms(100), tiles: &[(7, true), (8, false)], row_span: (6, 9), y, build_ms: 1.0 }));
        assert!(!b.frame(Drawn { now: ms(120), tiles: &[(8, false), (9, false)], row_span: (6, 9), y, build_ms: 1.0 }));
        assert!(!b.frame(Drawn { now: ms(140), tiles: &[(8, true), (9, true)], row_span: (6, 9), y, build_ms: 1.0 }));
        // after 40 ms: end of scrolling (tile 9 came into view empty and was filled 20 ms later); then end, middle, top
        assert_eq!(b.step, 3, "scrolling finished");
        assert_eq!((b.scrolls[0].appeared, b.scrolls[0].appeared_empty), (1, 1));
        assert_eq!(b.scrolls[0].fill_ms.iter().map(|v| v.round()).collect::<Vec<_>>(), vec![20.0]);
        for k in 0..3 {
            let base = 200 + k * 100;
            assert!(b.offset(before(ms(base))).is_some());
            b.frame(Drawn { now: ms(base), tiles: &[(100 + k as i64, false)], row_span: (500, 503), y: 0.0, build_ms: 1.0 });
            b.frame(Drawn { now: ms(base + 10), tiles: &[(100 + k as i64, true)], row_span: (500, 503), y: 0.0, build_ms: 1.0 });
            let done = b.frame(Drawn { now: ms(base + 40), tiles: &[(100 + k as i64, true)], row_span: (500, 503), y: 0.0, build_ms: 1.0 });
            assert_eq!(done, k == 2);
        }
        // the middle and top jumps land on rows seen before (same row span in this test): revisits
        assert_eq!(b.jumps.iter().filter(|j| j.revisit).count(), 2);
        let (j, _) = b.report();
        assert_eq!(j["jump_summary"]["n"], 1);
        assert_eq!(j["jump_summary"]["ms_to_complete"]["median"], 10.0);
        assert_eq!(j["scroll"][0]["tiles_appeared_empty_pct"], 100.0);
        assert!(out.exists() && out.with_extension("frames.tsv").exists());
        let _ = std::fs::remove_file(out.with_extension("frames.tsv"));
        let _ = std::fs::remove_file(&out);
    }
}
