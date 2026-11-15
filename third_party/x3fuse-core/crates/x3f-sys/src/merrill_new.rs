//! graineo addition: test switch for the alternative Merrill parts.
//!
//! `GRAINEO_MERRILL_NEW` is a comma-separated list of parts that use their alternative form instead
//! of the default one: `sharpness`, `filllight`, `hc`, `fringe`, `ca`, `noise` (together: `edge_parts`),
//! `outliers`, `highlights`, or `all`. Without the variable everything stays as today (bit-identical).
//! The switch exists for comparing the two forms; the default is unchanged.

/// Parts grouped under `edge_parts`.
pub const EDGE_PARTS: [&str; 6] = ["sharpness", "filllight", "hc", "fringe", "ca", "noise"];
/// All parts.
pub const ALL: [&str; 8] = [
    "sharpness",
    "filllight",
    "hc",
    "fringe",
    "ca",
    "noise",
    "outliers",
    "highlights",
];

/// Name of the environment variable.
pub const VAR: &str = "GRAINEO_MERRILL_NEW";

/// Is the new form of `part` switched on?
pub fn active(part: &str) -> bool {
    let Ok(v) = std::env::var(VAR) else {
        return false;
    };
    v.split(',')
        .map(str::trim)
        .any(|t| t == part || t == "all" || (t == "edge_parts" && EDGE_PARTS.contains(&part)))
}

/// The active parts in a fixed order (empty = default), e.g. for cache keys.
pub fn active_parts() -> Vec<&'static str> {
    ALL.into_iter().filter(|t| active(t)).collect()
}
