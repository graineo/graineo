//! Reaction to memory shortage. The cache of preloaded photos keeps its fixed size
//! (`CACHE_LIMIT` in `main.rs`) in normal operation; only when the system reports memory pressure photos leave
//! the cache early. The photo on screen and its whole image at full resolution are never touched.

use library::platform::Pressure;

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
