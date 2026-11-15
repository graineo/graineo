//! Comparing maker, model and lens names (moved unchanged from `raw::lcp`).

/// Letters and digits only, lower case.
pub fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

/// First word, normalised ("NIKON CORPORATION" → "nikon").
pub fn first_word(s: &str) -> String {
    norm(s.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| !w.is_empty()).unwrap_or(""))
}

/// Same camera model (one name may end with the other, e.g. "Canon EOS 400D" ↔ "EOS 400D").
pub fn model_matches(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.ends_with(&b) || b.ends_with(&a))
}
