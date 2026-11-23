//! Brightness-dependent amounts of the high-ISO chain: 15 values at doubling brightness nodes, linear in between,
//! looked up with a brightness rounded to the nearest (even) integer.

/// Brightness nodes of every 15-value curve.
pub(super) const NODES: [usize; 15] = [0, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384];
/// Entries of a tabulated curve.
pub(super) const ENTRIES: usize = 16384;

/// Tabulates the curve through `values` at [`NODES`] for the brightness values 0 … 16383 (the last entry takes the
/// last node's value).
pub(super) fn table(values: &[f64; 15]) -> Vec<f32> {
    let mut out = vec![0f32; ENTRIES];
    for (segment, pair) in NODES.windows(2).enumerate() {
        let (from, to) = (pair[0], pair[1]);
        let rise = (values[segment + 1] - values[segment]) as f32;
        for (offset, entry) in out[from..to.min(ENTRIES)].iter_mut().enumerate() {
            *entry = offset as f32 / (to - from) as f32 * rise + values[segment] as f32;
        }
    }
    out[ENTRIES - 1] = values[14] as f32;
    out
}

/// Table index of brightness `v`: rounded half to even, limited to the table.
#[inline]
pub(super) fn index(v: f64) -> usize {
    let rounded = v.round_ties_even();
    if rounded >= ENTRIES as f64 {
        ENTRIES - 1
    } else if rounded > 0.0 {
        rounded as usize
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_passes_through_its_nodes() {
        let values: [f64; 15] = std::array::from_fn(|i| 1.0 + i as f64 * 0.1);
        let t = table(&values);
        for (i, &n) in NODES.iter().enumerate().take(14) {
            assert!((t[n] - values[i] as f32).abs() < 1e-6);
        }
        assert!((t[3] - 1.15).abs() < 1e-6, "half way between the nodes 2 and 4");
        assert_eq!(t[ENTRIES - 1], values[14] as f32);
    }

    #[test]
    fn index_rounds_half_to_even_and_stays_inside() {
        assert_eq!(index(2.5), 2);
        assert_eq!(index(3.5), 4);
        assert_eq!(index(-7.0), 0);
        assert_eq!(index(1e9), ENTRIES - 1);
        assert_eq!(index(f64::NAN), 0);
    }
}
