//! Test aid `GRAINEO_GRID_SCROLL_TEST="pts_per_s=3000;start=6;max_s=40"`: the library grid scrolls by itself
//! from the top to the bottom at a fixed speed (points per second, independent of the frame rate) and measures
//! frame times, tiles that are still empty when they come into view (and how long until they are filled) and the
//! memory of the grid textures. At the end the result goes into the trace log (`grid-scroll-test: …`) and the app
//! closes. Only for measurements, never active without the variable.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// speed in points per second
    pub pts_per_s: f32,
    /// start this many seconds after the library opened (warm-up: first tiles, layout)
    pub start_s: f32,
    /// stop after this many seconds of scrolling even if the bottom is not reached
    pub max_s: f32,
}

impl Config {
    pub fn parse(v: &str) -> Config {
        let mut c = Config { pts_per_s: 3000.0, start_s: 6.0, max_s: 60.0 };
        for part in v.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let Ok(x) = v.trim().parse::<f32>() else { continue };
            match k.trim() {
                "pts_per_s" => c.pts_per_s = x,
                "start" => c.start_s = x,
                "max_s" => c.max_s = x,
                _ => {}
            }
        }
        c
    }
}

pub struct ScrollTest {
    cfg: Config,
    started: Option<Instant>,
    last_frame: Option<Instant>,
    frames_ms: Vec<f64>,
    /// tiles in view in the previous frame
    previous: HashSet<i64>,
    /// tiles that came into view without their preview: since when
    pending: HashMap<i64, Instant>,
    appeared: usize,
    appeared_empty: usize,
    fill_ms: Vec<f64>,
    /// left the view again before their preview arrived
    left_empty: usize,
    texture_peak: u64,
    textures_peak: usize,
    at_bottom: usize,
}

impl ScrollTest {
    pub fn from_env() -> Option<ScrollTest> {
        let v = crate::env::var("GRAINEO_GRID_SCROLL_TEST").ok()?;
        Some(ScrollTest::new(Config::parse(&v)))
    }

    pub fn new(cfg: Config) -> ScrollTest {
        ScrollTest {
            cfg,
            started: None,
            last_frame: None,
            frames_ms: Vec::new(),
            previous: HashSet::new(),
            pending: HashMap::new(),
            appeared: 0,
            appeared_empty: 0,
            fill_ms: Vec::new(),
            left_empty: 0,
            texture_peak: 0,
            textures_peak: 0,
            at_bottom: 0,
        }
    }

    /// Scroll position for this frame (`None`: not started yet). `since_open`: seconds since the library opened,
    /// `ready`: the grid has photos.
    pub fn offset(&mut self, now: Instant, since_open: f32, ready: bool) -> Option<f32> {
        if self.started.is_none() {
            if !(ready && since_open >= self.cfg.start_s) {
                return None;
            }
            self.started = Some(now);
        }
        Some(now.duration_since(self.started?).as_secs_f32() * self.cfg.pts_per_s)
    }

    /// After drawing a frame: `shown` = (file, preview present) of the tiles in view, `texture_bytes`/`textures`
    /// = memory and number of the grid textures, `wanted`/`actual` = requested and clamped scroll position.
    /// Returns the report once the bottom (or `max_s`) is reached.
    pub fn frame(&mut self, now: Instant, shown: &[(i64, bool)], texture_bytes: u64, textures: usize, wanted: f32, actual: f32) -> Option<String> {
        let started = self.started?;
        if let Some(last) = self.last_frame {
            self.frames_ms.push(now.duration_since(last).as_secs_f64() * 1000.0);
        }
        self.last_frame = Some(now);
        let first = self.previous.is_empty() && self.appeared == 0;
        let current: HashSet<i64> = shown.iter().map(|s| s.0).collect();
        for &(file, has) in shown {
            if has {
                if let Some(t) = self.pending.remove(&file) {
                    self.fill_ms.push(now.duration_since(t).as_secs_f64() * 1000.0);
                }
            }
            // the tiles of the very first frame were there before scrolling started
            if first || self.previous.contains(&file) {
                continue;
            }
            self.appeared += 1;
            if !has {
                self.appeared_empty += 1;
                self.pending.insert(file, now);
            }
        }
        let gone: Vec<i64> = self.pending.keys().copied().filter(|f| !current.contains(f)).collect();
        for f in gone {
            self.pending.remove(&f);
            self.left_empty += 1;
        }
        self.previous = current;
        self.texture_peak = self.texture_peak.max(texture_bytes);
        self.textures_peak = self.textures_peak.max(textures);
        if wanted > actual + 1.0 {
            self.at_bottom += 1;
        }
        let elapsed = now.duration_since(started).as_secs_f32();
        (self.at_bottom >= 3 || elapsed >= self.cfg.max_s).then(|| self.report(elapsed, actual))
    }

    fn report(&self, elapsed: f32, y: f32) -> String {
        let n = self.frames_ms.len().max(1) as f64;
        let over = |ms: f64| self.frames_ms.iter().filter(|&&f| f > ms).count() as f64 / n * 100.0;
        let longest = self.frames_ms.iter().copied().fold(0.0, f64::max);
        let mut fill = self.fill_ms.clone();
        fill.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f64| if fill.is_empty() { 0.0 } else { fill[((fill.len() - 1) as f64 * p).round() as usize] };
        let mean_fill = if fill.is_empty() { 0.0 } else { fill.iter().sum::<f64>() / fill.len() as f64 };
        format!(
            "grid-scroll-test: {:.0} pt/s, {:.1} s, up to y {:.0}; {:.1} images/s; frames > 16.7 ms {:.1} %, > 33 ms {:.1} %, longest {:.1} ms; \
             tiles appeared {}, of which empty {} ({:.1} %), filled after mean {:.0} ms / median {:.0} ms / 95 % {:.0} ms / max {:.0} ms, empty ones gone again {}; \
             textures at most {} ({:.0} MB)",
            self.cfg.pts_per_s,
            elapsed,
            y,
            self.frames_ms.len() as f64 / elapsed.max(0.001) as f64,
            over(1000.0 / 60.0),
            over(1000.0 / 30.0),
            longest,
            self.appeared,
            self.appeared_empty,
            self.appeared_empty as f64 / self.appeared.max(1) as f64 * 100.0,
            mean_fill,
            pct(0.5),
            pct(0.95),
            fill.last().copied().unwrap_or(0.0),
            self.left_empty,
            self.textures_peak,
            self.texture_peak as f64 / 1e6,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_and_measures() {
        let c = Config::parse("pts_per_s=1500; start=2;max_s=9;x=1");
        assert_eq!((c.pts_per_s, c.start_s, c.max_s), (1500.0, 2.0, 9.0));
        let mut t = ScrollTest::new(c);
        let t0 = Instant::now();
        assert_eq!(t.offset(t0, 1.0, true), None, "warm-up");
        assert_eq!(t.offset(t0, 2.5, true), Some(0.0));
        assert!((t.offset(t0 + Duration::from_millis(100), 2.6, true).unwrap() - 150.0).abs() < 0.5);
        // frame 1: tiles 1, 2 were there already; frame 2: 3 appears empty, 4 with preview; frame 3: 3 filled
        assert!(t.frame(t0, &[(1, true), (2, true)], 100, 2, 0.0, 0.0).is_none());
        assert!(t.frame(t0 + Duration::from_millis(16), &[(2, true), (3, false), (4, true)], 200, 3, 10.0, 10.0).is_none());
        assert!(t.frame(t0 + Duration::from_millis(56), &[(3, true), (5, false)], 200, 3, 20.0, 20.0).is_none());
        // frame 4: 5 leaves the view still empty; the bottom is reached three times → report
        assert!(t.frame(t0 + Duration::from_millis(72), &[(6, true)], 300, 4, 40.0, 30.0).is_none());
        assert!(t.frame(t0 + Duration::from_millis(88), &[(6, true)], 300, 4, 50.0, 30.0).is_none());
        let r = t.frame(t0 + Duration::from_millis(104), &[(6, true)], 300, 4, 60.0, 30.0).unwrap();
        assert_eq!((t.appeared, t.appeared_empty, t.left_empty), (4, 2, 1));
        assert_eq!(t.fill_ms.len(), 1);
        assert!((t.fill_ms[0] - 40.0).abs() < 0.5);
        assert!(r.contains("of which empty 2 (50.0 %)"), "{r}");
        // frame times: 16, 40, 16, 16, 16 ms → one above 33 ms (20 %), longest 40
        assert!(r.contains("> 33 ms 20.0 %") && r.contains("longest 40.0 ms"), "{r}");
    }
}
