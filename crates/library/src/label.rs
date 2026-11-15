//! Color labels. Lightroom stores in `Adobe_images.colorLabels` only the **text** of the
//! label in the currently active label set (localized, sometimes custom sets). We store a
//! fixed color (0 = none, 1 red, 2 yellow, 3 green, 4 blue, 5 purple) plus the original text.

/// Fixed colors in Lightroom order (keys 6–9 = red, yellow, green, blue).
pub const NAMES: [&str; 6] = ["Keine", "Rot", "Gelb", "Grün", "Blau", "Lila"];

/// Display color (RGB) per label.
pub const COLORS: [[u8; 3]; 6] = [[0x60, 0x60, 0x60], [0xc8, 0x3c, 0x3c], [0xd2, 0xb4, 0x32], [0x46, 0xa0, 0x4a], [0x3c, 0x78, 0xc8], [0x96, 0x5a, 0xbe]];

/// Mapping Lightroom label text → fixed color. Source: the default label sets in German
/// and English ("Referenz-Standard", "Überprüfungsstatus"/"Review Status", "Bridge-Standard")
/// plus the values found in an actual catalog.
/// Unknown texts → `None` (the text is kept in `label_lr`; the report lists them).
pub fn from_lightroom(text: &str) -> Option<u8> {
    let t = text.trim();
    if t.is_empty() {
        return Some(0);
    }
    Some(match t.to_lowercase().as_str() {
        "none" | "keine" | "ohne" => 0,
        // Referenz-Standard
        "red" | "rot" => 1,
        "yellow" | "gelb" => 2,
        "green" | "grün" => 3,
        "blue" | "blau" => 4,
        "purple" | "lila" | "violett" => 5,
        // Review status (colors); German name: "Überprüfungsstatus"
        "to delete" | "zu löschen" => 1,
        "color correction needed" | "farbkorrektur erforderlich" => 2,
        "good to use" | "verwendbar" => 3,
        "retouching needed" | "retusche erforderlich" => 4,
        "to print" | "zu drucken" => 5,
        // Bridge-Standard
        "select" | "auswahl" => 1,
        "second" | "zweite wahl" => 2,
        "approved" | "genehmigt" => 3,
        "review" | "überprüfen" => 4,
        "to do" | "zu erledigen" => 5,
        // custom set: "Pink" – closest Lightroom color
        "pink" | "rosa" => 5,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_values() {
        for (t, f) in [("Rot", 1), ("Red", 1), ("Gelb", 2), ("Grün", 3), ("Green", 3), ("Blau", 4), ("Blue", 4), ("Good to Use", 3), ("To Delete", 1), ("Retouching Needed", 4), ("Color Correction Needed", 2), ("Pink", 5), ("None", 0), ("", 0), ("Yellow", 2)] {
            assert_eq!(from_lightroom(t), Some(f), "{t}");
        }
        assert_eq!(from_lightroom("Irgendwas"), None);
    }
}
