//! graineo addition: optional per-stage timing of the SPP noise reduction.
//!
//! Active only when the environment variable `X3F_SPP_TIMING` is set. The times of the
//! tile stages are summed over all tiles (and thus over all threads),
//! so they are CPU time, not wall-clock time. Results of the computation are unaffected.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Measuring points within `Pipeline::process_tile`.
pub(crate) const STAGES: [&str; 12] = [
    "copy/allocation",
    "KernelMap+Bilateral",
    "EdgeMap+Remap",
    "LocalMean (IIR)",
    "MatchColOffsets",
    "Luma+Diffusion+OSF+Ratio",
    "MinMaxMap",
    "RCPS",
    "residual mix",
    "Matrix+ColorDQ",
    "SharpL+apply",
    "copy back",
];

static TOTALS: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];

/// Is timing switched on?
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("X3F_SPP_TIMING").is_some())
}

/// Stopwatch for the stages of one tile.
pub(crate) struct Lap {
    last: Option<Instant>,
}

impl Lap {
    pub(crate) fn start() -> Self {
        Self {
            last: enabled().then(Instant::now),
        }
    }

    /// Finishes stage `stage` and starts the next one.
    pub(crate) fn lap(&mut self, stage: usize) {
        if let Some(last) = self.last {
            let now = Instant::now();
            TOTALS[stage].fetch_add((now - last).as_nanos() as u64, Ordering::Relaxed);
            self.last = Some(now);
        }
    }
}

/// Prints the summed stage times and resets them.
pub(crate) fn report() {
    if !enabled() {
        return;
    }
    let total: u64 = TOTALS.iter().map(|t| t.load(Ordering::Relaxed)).sum();
    eprintln!(
        "SPP Kachelstufen (CPU-Zeit über alle Threads, Summe {:.0} ms):",
        total as f64 / 1e6
    );
    for (name, value) in STAGES.iter().zip(&TOTALS) {
        let ns = value.swap(0, Ordering::Relaxed);
        eprintln!("  {name:<26} {:>7.1} ms", ns as f64 / 1e6);
    }
}
