//! Stress test for background processing (checking only, `GRAINEO_STRESS=<seed>`):
//! browses, zooms and pans quickly at random like an impatient person and then checks
//! whether "Refining …" and detail loading settle down. Result on stderr, then exit
//! with code 0 (fine) or 1 (hangs).

pub struct Stress {
    rng: u64,
    next_at: f64,
    pub actions: u32,
    total: u32,
    settle_from: Option<f64>,
    /// Fault simulation (`GRAINEO_STRESS_DROP=n`): discard every n-th SPP result
    drop_every: u32,
    seen: u32,
    /// Slider changes (`GRAINEO_STRESS_EDIT=1`): without it the test behaves as before
    edit: bool,
    /// Steps still to do of a running exposure series
    burst: u32,
    /// Zoom guard (`GRAINEO_STRESS_ZOOM=1`, with `=mask` the photo gets a mask first)
    zoom: Option<ZoomRun>,
    /// Hover storm (`GRAINEO_STRESS_HOVER=1`): after the start phase 30 hovered films 100 ms apart, then the check
    hover: bool,
}

/// Hover storm: how many hover previews were sent, rendered and superseded (final line `stress: hover sent=… rendered=…
/// superseded=…`).
pub static HOVER_SENT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static HOVER_RENDERED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static HOVER_SUPERSEDED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Number of hover actions of the hover storm.
pub const HOVER_STORM: u32 = 30;

pub fn hover_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!("stress: hover sent={} rendered={} superseded={}", HOVER_SENT.load(Relaxed), HOVER_RENDERED.load(Relaxed), HOVER_SUPERSEDED.load(Relaxed))
}

/// State of the zoom guard run: open the photo, zoom, wait for the whole image at full resolution, then only
/// zoom and pan. Nothing may be rendered after that (counted in [`zoom_report`]).
struct ZoomRun {
    mask: bool,
    mask_done: bool,
    zoomed: bool,
    /// the whole image is in place (reported by the app every frame)
    ready: bool,
    /// since when `ready` has held without a break (s)
    ready_since: Option<f64>,
    armed: bool,
    wait_since: Option<f64>,
}

static COUNT_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REGION_JOBS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FULL_JOBS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Counts `Job::RenderRegion` / `Job::RenderFull` while the zoom guard is armed (called when a job is sent).
pub fn count_job(region: bool, full: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if COUNT_ON.load(Relaxed) {
        if region {
            REGION_JOBS.fetch_add(1, Relaxed);
        }
        if full {
            FULL_JOBS.fetch_add(1, Relaxed);
        }
    }
}

/// Final line of the zoom guard: `stress: zoom jobs region=<n> full=<n> frame_p95=<ms>`.
pub fn zoom_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let p95 = crate::frame_watch::window_p95().map_or("n/a".to_string(), |v| format!("{v:.1}"));
    format!("stress: zoom jobs region={} full={} frame_p95={p95}", REGION_JOBS.load(Relaxed), FULL_JOBS.load(Relaxed))
}

/// What the test should do next.
pub enum Action {
    Select(usize),
    Zoom(f32, [f32; 2]),
    Pan([f32; 2]),
    Fit,
    /// Zoom guard with a mask: give the current photo a radial mask (exposure +0.5), nothing is saved
    ApplyMask,
    /// Slider change on the current photo (only with `GRAINEO_STRESS_EDIT=1`, see [`EditKind`])
    Edit(EditKind),
    /// Hover storm (only with `GRAINEO_STRESS_HOVER=1`): hover the film with this index of the film list
    Hover(usize),
    /// Idle phase: check whether everything is finished (`timeout` = waited too long)
    Check { waited: f64, timeout: bool },
    Wait,
}

/// Kind of slider change of the stress test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditKind {
    /// Exposure of the current photo set to this value (series of 20 steps, 33 ms apart, from −1 to +1 EV)
    Exposure(f32),
    /// Luminance noise slider one step (±0.25) further
    NoiseStep(i8),
}

const BURST_STEPS: u32 = 20;
const BURST_GAP: f64 = 0.033;

impl Stress {
    pub fn from_env() -> Option<Self> {
        let seed: u64 = crate::env::var("GRAINEO_STRESS").ok()?.parse().ok()?;
        let total = crate::env::var("GRAINEO_STRESS_N").ok().and_then(|s| s.parse().ok()).unwrap_or(150);
        let drop_every = crate::env::var("GRAINEO_STRESS_DROP").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let edit = crate::env::var_os("GRAINEO_STRESS_EDIT").is_some();
        let mut st = Self::new(seed, total, drop_every, edit);
        if crate::env::var_os("GRAINEO_STRESS_HOVER").is_some() {
            st.hover = true;
            if crate::env::var("GRAINEO_STRESS_N").is_err() {
                st.total = HOVER_STORM;
            }
        }
        if let Ok(v) = crate::env::var("GRAINEO_STRESS_ZOOM") {
            // the zoom guard defaults to 200 actions
            if crate::env::var("GRAINEO_STRESS_N").is_err() {
                st.total = 200;
            }
            st.zoom = Some(ZoomRun { mask: v == "mask", mask_done: false, zoomed: false, ready: false, ready_since: None, armed: false, wait_since: None });
        }
        Some(st)
    }

    fn new(seed: u64, total: u32, drop_every: u32, edit: bool) -> Self {
        Self { rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1, next_at: 3.0, actions: 0, total, settle_from: None, drop_every, seen: 0, edit, burst: 0, zoom: None, hover: false }
    }

    #[cfg(test)]
    fn new_hover(seed: u64, hover: bool) -> Self {
        let mut st = Self::new(seed, 5000, 0, false);
        st.hover = hover;
        st
    }

    /// Does this run hover films? (The app then sets the hover preview like the pointer does.)
    pub fn hover_enabled(&self) -> bool {
        self.hover
    }

    #[cfg(test)]
    fn new_zoom(seed: u64, total: u32) -> Self {
        let mut st = Self::new(seed, total, 0, false);
        st.zoom = Some(ZoomRun { mask: false, mask_done: true, zoomed: true, ready: true, ready_since: None, armed: false, wait_since: None });
        st
    }

    /// Does this run change settings? (Then nothing may be saved.)
    pub fn edits_enabled(&self) -> bool {
        self.edit || self.zoom.as_ref().is_some_and(|z| z.mask)
    }

    pub fn zoom_enabled(&self) -> bool {
        self.zoom.is_some()
    }

    /// Zoom guard: the app reports every frame whether the whole image at full resolution is in place and nothing is busy.
    pub fn set_full_ready(&mut self, ready: bool) {
        if let Some(z) = self.zoom.as_mut() {
            z.ready = ready;
        }
    }

    /// Zoom guard: the start phase (open, zoom, wait for the whole image). `None` once it is over.
    fn zoom_start(&mut self, now: f64) -> Option<Action> {
        let z = self.zoom.as_mut()?;
        if z.armed {
            return None;
        }
        if now < self.next_at {
            return Some(Action::Wait);
        }
        if z.mask && !z.mask_done {
            z.mask_done = true;
            self.next_at = now + 0.5;
            return Some(Action::ApplyMask);
        }
        if !z.zoomed {
            z.zoomed = true;
            self.next_at = now + 0.5;
            return Some(Action::Zoom(8.0, [0.0, 0.0]));
        }
        let since = *z.wait_since.get_or_insert(now);
        // the whole image counts as in place when it stayed in place for 3 s (Sigma: the exact base replaces the first one)
        if !z.ready {
            z.ready_since = None;
        }
        let steady = z.ready && now - *z.ready_since.get_or_insert(now) >= 3.0;
        if steady || now - since > 90.0 {
            // from here on nothing may be rendered: counters and frame window start at zero
            z.armed = true;
            self.next_at = now + 0.3;
            REGION_JOBS.store(0, std::sync::atomic::Ordering::Relaxed);
            FULL_JOBS.store(0, std::sync::atomic::Ordering::Relaxed);
            COUNT_ON.store(true, std::sync::atomic::Ordering::Relaxed);
            crate::frame_watch::window_start();
            crate::log(&format!("stress: zoom guard armed (whole image steady in place: {steady})"));
        }
        Some(Action::Wait)
    }

    /// Next step of the exposure series.
    fn burst_step(&mut self, now: f64) -> Action {
        self.burst -= 1;
        self.next_at = now + BURST_GAP;
        let k = (BURST_STEPS - 1 - self.burst) as f32 / (BURST_STEPS - 1) as f32;
        Action::Edit(EditKind::Exposure(-1.0 + 2.0 * k))
    }

    /// Should this SPP result be discarded (fault simulation)?
    pub fn drop_result(&mut self) -> bool {
        self.seen += 1;
        self.drop_every > 0 && self.seen % self.drop_every == 0
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }

    pub fn step(&mut self, now: f64, current: Option<usize>, count: usize) -> Action {
        if self.actions >= self.total && self.zoom.as_ref().is_none_or(|z| z.armed) {
            let from = *self.settle_from.get_or_insert(now);
            return Action::Check { waited: now - from, timeout: now - from > 90.0 };
        }
        if let Some(a) = self.zoom_start(now) {
            return a;
        }
        if now < self.next_at {
            return Action::Wait;
        }
        if self.burst > 0 {
            return self.burst_step(now);
        }
        self.actions += 1;
        if self.hover {
            // hover storm: one film after another, 100 ms apart, like a quick pass over the film list
            self.next_at = now + 0.1;
            return Action::Hover(self.actions as usize - 1);
        }
        if self.zoom.is_some() {
            // zoom guard: only zoom, pan and fit, at a steady pace
            self.next_at = now + 0.05 + 0.2 * self.unit() as f64;
            return match self.next() % 10 {
                0..=4 => Action::Zoom([2.0, 4.0, 8.0][(self.next() % 3) as usize], [self.unit() - 0.5, self.unit() - 0.5]),
                5..=7 => Action::Pan([self.unit() - 0.5, self.unit() - 0.5]),
                _ => Action::Fit,
            };
        }
        // Browsing (`GRAINEO_BROWSE=<seconds>`): one image forward at a steady pace
        if let Some(dt) = crate::env::var("GRAINEO_BROWSE").ok().and_then(|s| s.parse::<f64>().ok()) {
            self.next_at = now + dt;
            return Action::Select((current.unwrap_or(0) + 1) % count.max(1));
        }
        // mostly fast (50–250 ms), now and then a short pause (up to 3 s) – then the base starts
        let pause = if self.next() % 7 == 0 { 0.8 + 2.2 * self.unit() } else { 0.05 + 0.2 * self.unit() };
        self.next_at = now + pause as f64;
        let cur = current.unwrap_or(0);
        if self.edit && self.next() % 4 == 0 {
            if self.next() % 2 == 0 {
                self.burst = BURST_STEPS;
                return self.burst_step(now);
            }
            self.next_at = now + 0.5;
            return Action::Edit(EditKind::NoiseStep(if self.next() % 2 == 0 { 1 } else { -1 }));
        }
        let r = self.next() % 10;
        match r {
            0..=3 => {
                let step = if self.next() % 2 == 0 { 1 } else { count - 1 };
                Action::Select((cur + step) % count.max(1))
            }
            4 => Action::Select((self.next() as usize) % count.max(1)),
            5 | 6 => {
                let z = [2.0, 4.0, 8.0][(self.next() % 3) as usize];
                Action::Zoom(z, [self.unit() - 0.5, self.unit() - 0.5])
            }
            7 | 8 => Action::Pan([self.unit() - 0.5, self.unit() - 0.5]),
            _ => Action::Fit,
        }
    }
}

/// Time from a slider change to the finished image (`GRAINEO_EDIT_LOG=1` or `GRAINEO_STRESS_EDIT=1`, needs
/// `GRAINEO_DATA_DIR`): every frame the current photo's settings are compared with the previous frame; a change
/// is remembered and its time logged when the render for exactly these settings arrives
/// (`time: edit <kind> rendered after <ms> ms`) and, for noise, when the exact Sigma base is in place
/// (`time: edit noise exact after <ms> ms`). Works for the stress test and for real mouse input alike.
pub struct EditLog {
    on: bool,
    last: Option<(usize, develop::Settings)>,
    marks: Vec<(&'static str, std::time::Instant, develop::Settings)>,
    exact: Vec<(std::time::Instant, develop::SppKey)>,
}

impl EditLog {
    pub fn from_env() -> Self {
        let on = (crate::env::var_os("GRAINEO_EDIT_LOG").is_some() || crate::env::var_os("GRAINEO_STRESS_EDIT").is_some())
            && crate::env::var_os("GRAINEO_DATA_DIR").is_some();
        Self::new(on)
    }

    fn new(on: bool) -> Self {
        Self { on, last: None, marks: Vec::new(), exact: Vec::new() }
    }

    pub fn observe(&mut self, index: usize, path: &std::path::Path, s: &develop::Settings) {
        if !self.on {
            return;
        }
        if let Some((i, old)) = &self.last {
            if *i == index && old != s {
                let file_kind = if old.exposure != s.exposure {
                    "exposure"
                } else if old.nr_luma != s.nr_luma || old.nr_chroma != s.nr_chroma {
                    "noise"
                } else {
                    "other"
                };
                let now = std::time::Instant::now();
                self.marks.push((file_kind, now, s.clone()));
                if file_kind == "noise" {
                    self.exact.push((now, s.spp_key(path)));
                }
                // bounded: superseded changes are dropped
                if self.marks.len() > 64 {
                    self.marks.remove(0);
                }
                if self.exact.len() > 16 {
                    self.exact.remove(0);
                }
            }
        }
        if self.last.as_ref().is_none_or(|(i, old)| *i != index || old != s) {
            self.last = Some((index, s.clone()));
        }
    }

    /// A render for `s` arrived: kind and milliseconds of the last change with these settings.
    pub fn rendered(&mut self, s: &develop::Settings) -> Option<(&'static str, f64)> {
        let pos = self.marks.iter().rposition(|m| m.2 == *s)?;
        let (file_kind, at, _) = self.marks[pos].clone();
        self.marks.drain(..=pos);
        Some((file_kind, at.elapsed().as_secs_f64() * 1000.0))
    }

    /// The Sigma base for `key` arrived: milliseconds since the noise change with this key.
    pub fn exact(&mut self, key: &develop::SppKey) -> Option<f64> {
        let pos = self.exact.iter().rposition(|m| m.1 == *key)?;
        let at = self.exact[pos].0;
        self.exact.drain(..=pos);
        Some(at.elapsed().as_secs_f64() * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_actions_only_when_enabled() {
        let run = |edit: bool| {
            let mut st = Stress::new(7, 5000, 0, edit);
            let mut edits = 0;
            for k in 0..1000 {
                if let Action::Edit(_) = st.step(10.0 + k as f64, Some(0), 20) {
                    edits += 1;
                }
            }
            edits
        };
        assert_eq!(run(false), 0);
        assert!(run(true) >= 1);
    }

    #[test]
    fn hover_actions_only_when_enabled() {
        let run = |hover: bool| {
            let mut st = Stress::new_hover(7, hover);
            let mut hovers = 0;
            for k in 0..1000 {
                if let Action::Hover(_) = st.step(10.0 + k as f64, Some(0), 20) {
                    hovers += 1;
                }
            }
            hovers
        };
        assert_eq!(run(false), 0);
        assert!(run(true) >= HOVER_STORM as usize);
    }

    #[test]
    fn zoom_mode_only_zooms_and_pans() {
        let mut st = Stress::new_zoom(7, 200);
        // start phase: ready at once, so it is armed after the first waiting step
        let mut now = 10.0;
        let mut n = 0;
        let mut guard = 0;
        while n < 200 && guard < 100_000 {
            guard += 1;
            now += 0.3;
            match st.step(now, Some(0), 20) {
                Action::Zoom(..) | Action::Pan(_) | Action::Fit => n += 1,
                Action::Wait => {}
                _ => panic!("zoom mode must not select, edit or check before the end"),
            }
        }
        assert_eq!(n, 200);
        assert!(matches!(st.step(now + 1.0, Some(0), 20), Action::Check { .. }));
    }

    #[test]
    fn exposure_series_has_twenty_steps_from_minus_one_to_plus_one() {
        let mut st = Stress::new(1, 5000, 0, true);
        st.burst = BURST_STEPS;
        let mut v = Vec::new();
        let mut now = 10.0;
        while st.burst > 0 {
            if let Action::Edit(EditKind::Exposure(x)) = st.burst_step(now) {
                v.push(x);
            }
            now += 0.04;
        }
        assert_eq!(v.len(), 20);
        assert_eq!((v[0], v[19]), (-1.0, 1.0));
    }

    #[test]
    fn edit_log_matches_render_to_change() {
        let mut log = EditLog::new(true);
        let mut s = develop::Settings::default();
        log.observe(0, std::path::Path::new("/nirgends.X3F"), &s);
        s.exposure = 0.5;
        log.observe(0, std::path::Path::new("/nirgends.X3F"), &s);
        assert_eq!(log.rendered(&s).map(|r| r.0), Some("exposure"));
        assert!(log.rendered(&s).is_none());
    }
}
