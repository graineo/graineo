//! Per-stage timing for measurements (`tests/bench.rs`, `GRAINEO_LOG`): a global collector that only
//! runs when switched on (otherwise [`lap`] costs one atomic read).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static COLLECTOR: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());

/// Switch collecting on/off (clears the previous entries).
pub fn collect(on: bool) {
    ACTIVE.store(on, Ordering::Relaxed);
    COLLECTOR.lock().unwrap().clear();
}

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Append an entry (stage, milliseconds).
pub fn lap(name: &str, d: std::time::Duration) {
    if active() {
        COLLECTOR.lock().unwrap().push((name.to_string(), d.as_secs_f64() * 1000.0));
    }
}

/// Fetch and clear the collected entries.
pub fn take_all() -> Vec<(String, f64)> {
    std::mem::take(&mut *COLLECTOR.lock().unwrap())
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
