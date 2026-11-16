//! Per-stage timing for measurements (`tests/bench.rs`, `GRAINEO_LOG`): a global collector that only
//! runs when switched on (otherwise [`lap`] costs one atomic read).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static COLLECTOR: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
/// Most entries kept between two [`take_all`] (a collector left on by mistake must not grow without end).
pub const MAX_ENTRIES: usize = 100_000;

/// Switch collecting on/off (clears the previous entries).
pub fn collect(on: bool) {
    ACTIVE.store(on, Ordering::Relaxed);
    COLLECTOR.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Append an entry (stage, milliseconds).
pub fn lap(name: &str, d: std::time::Duration) {
    if active() {
        let mut c = COLLECTOR.lock().unwrap_or_else(|e| e.into_inner());
        if c.len() < MAX_ENTRIES {
            c.push((name.to_string(), d.as_secs_f64() * 1000.0));
        }
    }
}

/// Fetch and clear the collected entries.
pub fn take_all() -> Vec<(String, f64)> {
    std::mem::take(&mut *COLLECTOR.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Measure a stage: `let _t = timings::Stage::new("…")` – it is recorded when the block is left.
pub struct Stage {
    name: &'static str,
    t: std::time::Instant,
}

impl Stage {
    pub fn new(name: &'static str) -> Self {
        Stage { name, t: std::time::Instant::now() }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        lap(self.name, self.t.elapsed());
    }
}
