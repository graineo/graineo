//! Check whether a path exists without a hanging network drive freezing the UI.
//! The check runs in its own thread; the UI waits only briefly. If the drive does not answer,
//! the path counts as unreachable for now, and the result is remembered for a few seconds (no new thread per frame).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a determined result stays valid.
const CACHE_FOR: Duration = Duration::from_secs(5);
/// How long the UI waits at most for a new answer.
const WAIT: Duration = Duration::from_millis(300);

#[derive(Default)]
struct Snapshot {
    outcome: HashMap<(PathBuf, bool), (Instant, bool)>,
    in_flight: HashSet<(PathBuf, bool)>,
}

fn snapshot() -> &'static Mutex<Snapshot> {
    static S: OnceLock<Mutex<Snapshot>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn check(p: &Path, want_dir: bool) -> bool {
    let key = (p.to_path_buf(), want_dir);
    let rx = {
        let mut s = snapshot().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((checked_at, found)) = s.outcome.get(&key) {
            if checked_at.elapsed() < CACHE_FOR {
                return *found;
            }
        }
        let (tx, rx) = mpsc::channel();
        if s.in_flight.insert(key.clone()) {
            let k = key.clone();
            std::thread::spawn(move || {
                let found = if k.1 { k.0.is_dir() } else { k.0.exists() };
                let mut s = snapshot().lock().unwrap_or_else(|e| e.into_inner());
                s.in_flight.remove(&k);
                s.outcome.insert(k, (Instant::now(), found));
                let _ = tx.send(found);
            });
        }
        rx
    };
    // Wait for the answer (only briefly); otherwise the last known result, without one: unreachable
    rx.recv_timeout(WAIT).unwrap_or_else(|_| snapshot().lock().unwrap_or_else(|e| e.into_inner()).outcome.get(&key).is_some_and(|(_, found)| *found))
}

/// Does this file or folder exist?
pub fn da(p: &Path) -> bool {
    check(p, false)
}

/// Is this an existing folder?
pub fn folder_exists(p: &Path) -> bool {
    check(p, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_existing_and_missing_paths() {
        let d = std::env::temp_dir();
        assert!(folder_exists(&d));
        assert!(da(&d));
        assert!(!da(&d.join("graineo-gibt-es-nicht-4711")));
        assert!(!folder_exists(&d.join("graineo-gibt-es-nicht-4711")));
    }
}
