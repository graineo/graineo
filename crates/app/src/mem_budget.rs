//! Reaction to memory shortage. The cache of preloaded photos keeps its fixed size
//! (`perf_mode::Plan::cache_limit`) in normal operation; only when the system reports memory pressure photos leave
//! the cache early. The photo on screen and its whole image at full resolution are never touched.

use library::platform::Pressure;

/// Memory left to the system and the photo on screen before anything is computed **ahead** (bytes). Provisional.
pub const RESERVE: u64 = 1_000_000_000;
/// Estimated peak memory of preparing the full resolution of another RAW, per pixel. Provisional, from the
/// performance report of 16.11.2026 (M1 Pro): 24–26-MP RAWs peaked at 5.3–5.9 GB with the GPU and the processor path
/// in one process; one path is taken as ≈ 2 GB above the opened app, ≈ 80 bytes per pixel.
pub const FULL_BYTES_PER_PIXEL: u64 = 80;
/// Same for Sigma's exact base (a Merrill base ≈ 1 GB at 14.8 MP).
pub const BASE_BYTES_PER_PIXEL: u64 = 70;
/// Opening a neighbour (raw data, fast preview).
pub const OPEN_BYTES: u64 = 600_000_000;

/// Is there room for work ahead that needs `need` bytes, with `free` bytes available (pure)? Unknown: yes.
pub fn room(free: Option<u64>, need: u64) -> bool {
    free.is_none_or(|f| f >= need.saturating_add(RESERVE))
}

/// Room for work ahead now? Logs when not. Work the user waits for never asks: it always runs, the result is the same.
pub fn room_ahead(what: &str, need: u64) -> bool {
    let free = library::platform::info::memory_available_now();
    let ok = room(free, need);
    if !ok {
        crate::log(&format!("memory: {what} skipped ({:.1} GB free, {:.1} GB needed with reserve)", free.unwrap_or(0) as f64 / 1e9, (need + RESERVE) as f64 / 1e9));
    }
    ok
}

/// Which cached photos (by index) leave the cache at this pressure? Farthest from `current` first.
/// `Normal`: none. `Warn`: only the nearest neighbour stays. `Critical`: none stays.
pub fn cache_to_drop(pressure: Pressure, cached: &[usize], current: usize) -> Vec<usize> {
    let keep = match pressure {
        Pressure::Normal => return Vec::new(),
        Pressure::Warn => 1,
        Pressure::Critical => 0,
    };
    let mut others: Vec<usize> = cached.iter().copied().filter(|&i| i != current).collect();
    others.sort_by_key(|&i| std::cmp::Reverse(i.abs_diff(current)));
    let drop_n = others.len().saturating_sub(keep);
    others.truncate(drop_n);
    others
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_keeps_the_reserve() {
        assert!(room(None, 10_000_000_000));
        assert!(room(Some(3_000_000_000), 2_000_000_000));
        assert!(!room(Some(2_900_000_000), 2_000_000_000));
        // 24 MP full resolution needs ≈ 1.9 GB + reserve
        assert!(!room(Some(2_500_000_000), 24_000_000 * FULL_BYTES_PER_PIXEL));
    }

    #[test]
    fn normal_pressure_keeps_everything() {
        assert!(cache_to_drop(Pressure::Normal, &[1, 2, 4, 5], 3).is_empty());
    }

    #[test]
    fn warn_keeps_only_the_nearest_neighbour() {
        let d = cache_to_drop(Pressure::Warn, &[1, 2, 4, 6], 3);
        assert_eq!(d.len(), 3);
        assert!(!d.contains(&2) || !d.contains(&4)); // one of the two nearest stays
        assert!(d.contains(&6) && d.contains(&1)); // farthest go first
    }

    #[test]
    fn critical_drops_all_cached_photos() {
        assert_eq!(cache_to_drop(Pressure::Critical, &[1, 2, 4], 3).len(), 3);
    }

    #[test]
    fn current_photo_never_evicted() {
        for p in [Pressure::Normal, Pressure::Warn, Pressure::Critical] {
            // even if the current photo is (wrongly) in the cache list it is not in the result
            assert!(!cache_to_drop(p, &[3, 2, 4], 3).contains(&3));
        }
    }
}
