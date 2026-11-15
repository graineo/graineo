//! AI descriptions of photos (table `ai`: keywords, scene, quality, reason, "nonsense" marker) and their vectors
//! (`image_vector`, `text_vector`). The descriptions graineo 1.0 took over from an external program are kept with the
//! source [`SOURCE`]; a description made by graineo itself will fill the same fields.

/// Source of the descriptions and vectors taken over before 1.0.1.
pub const SOURCE: &str = "imported";

/// "gut"/"mittel"/"ausschuss" → 2/1/0.
/// Also the names the interface shows in English, French, Spanish and Japanese (folded spelling).
pub fn quality(t: &str) -> Option<i64> {
    match crate::search::fold_text(t.trim()).as_str() {
        "gut" | "good" | "bon" | "bonne" | "bueno" | "buena" | "良" => Some(2),
        "mittel" | "medium" | "fair" | "moyen" | "moyenne" | "medio" | "media" | "中" => Some(1),
        "ausschuss" | "schlecht" | "reject" | "rejected" | "bad" | "rebut" | "descarte" | "除外" => Some(0),
        _ => None,
    }
}
