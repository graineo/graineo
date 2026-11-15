//! The rows of the Info panel, without repeating anything.
//!
//! - **METADATEN** (the most important first): file name, capture, camera, lens, exposure as one line
//!   ("1/50 s · f/2,8 · ISO 400 · 30 mm (45 mm KB)"), size in pixels, file size, format, folder; the condition only
//!   when it is not "present". Values the library does not know (a folder only browsed, a photo not yet indexed)
//!   come from the opened file.
//! - **AUFNAHMEDETAILS** (technique, nothing from above): program, exposure compensation, focus · flash, white
//!   balance, camera colour mode, picture style, aperture range; then the group "Sensor" (temperature, black level,
//!   defect pixels, sensor area …); then firmware and serial number.
//!
//! The details come from the opened file as (label, value) in German (`jobs.rs` for X3F, `rawdev` for other
//! formats); they are only sorted and filtered here. Empty values ("–") are left out.

/// Labels of the file details that METADATEN already shows (merged there), never repeated in the details.
const IN_METADATA: [&str; 8] = ["Kamera", "Aufnahme", "Datei", "Dateiname", "Maße", "Belichtung", "Brennweite", "Objektiv"];

/// Order of the details: first group (technique), second group "Sensor", third group (camera).
const TECHNIQUE: [&str; 8] = ["Programm", "Korrektur", "Fokus · Blitz", "Weißabgleich", "Farbmodus Kamera", "Bildstil", "Blendenbereich", "Binning"];
const SENSOR: [&str; 7] = ["Sensortemperatur", "Schwarzwert", "Aufbereitung", "Defektpixel", "Farbkalibrierung", "Sensorfläche", "Format"];
const CAMERA: [&str; 2] = ["Firmware", "Seriennummer"];

/// Is a value empty ("–", "– °C", "– s · f/– · ISO –" …: a dash and no digit)?
pub fn empty(v: &str) -> bool {
    let t = v.trim();
    t.is_empty() || t.contains('–') && !t.chars().any(|c| c.is_ascii_digit())
}

/// A detail value by its (German) label.
pub fn detail<'a>(details: &'a [(String, String)], key: &str) -> Option<&'a str> {
    details.iter().find(|(k, v)| k == key && !empty(v)).map(|(_, v)| v.as_str())
}

/// The details in three groups (label as shown, value), without what METADATEN shows and without empty values. The
/// label "Format" of other RAW formats (sensor kind and size) is shown as "Mosaik" in the group "Sensor".
pub fn groups(details: &[(String, String)]) -> [Vec<(String, String)>; 3] {
    let mut out: [Vec<(String, String)>; 3] = Default::default();
    let rank = |k: &str| -> (usize, usize) {
        for (g, list) in [&TECHNIQUE[..], &SENSOR[..], &CAMERA[..]].iter().enumerate() {
            if let Some(i) = list.iter().position(|x| *x == k) {
                return (g, i);
            }
        }
        (0, usize::MAX)
    };
    let mut rows: Vec<(usize, usize, usize, String, String)> = details
        .iter()
        .enumerate()
        .filter(|(_, (k, v))| !IN_METADATA.contains(&k.as_str()) && !empty(v))
        .map(|(n, (k, v))| {
            let (g, i) = rank(k);
            let shown = if k == "Format" { "Mosaik".to_string() } else { k.clone() };
            (g, i, n, shown, v.clone())
        })
        .collect();
    rows.sort_by_key(|r| (r.0, r.1, r.2));
    for (g, _, _, k, v) in rows {
        out[g].push((k, v));
    }
    out
}

/// Shutter speed with its unit ("1/640" → "1/640 s"; "0,8 s" stays).
pub fn with_seconds(t: &str) -> String {
    if t.trim_end().ends_with('s') { t.to_string() } else { format!("{t} s") }
}

/// The exposure line of METADATEN from its parts (missing parts left out); the focal length of the file is
/// preferred when it names the 35 mm equivalent ("30 mm (45 mm KB)").
pub fn exposure_line(time: Option<String>, aperture: Option<String>, iso: Option<String>, focal: Option<String>, file_focal: Option<&str>) -> Option<String> {
    let focal = match file_focal.filter(|f| f.contains("KB")) {
        Some(f) => Some(f.to_string()),
        None => focal.or_else(|| file_focal.map(str::to_string)),
    };
    let parts: Vec<String> = [time.map(|t| with_seconds(&t)), aperture, iso, focal].into_iter().flatten().filter(|p| !empty(p)).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn details_leave_out_metadata_and_empty_values_and_sort_in_groups() {
        let details = d(&[
            ("Kamera", "SIGMA DP2 Merrill"),
            ("Belichtung", "1/640 s · f/2,8 · ISO 100"),
            ("Seriennummer", "123"),
            ("Sensortemperatur", "– °C"),
            ("Defektpixel", "12 Einträge"),
            ("Weißabgleich", "Auto"),
            ("Programm", "Zeitautomatik (A)"),
            ("Format", "CR2 · Bayer · 5472×3648"),
            ("Neu", "x"),
        ]);
        let [a, b, c] = groups(&details);
        assert_eq!(a.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["Programm", "Weißabgleich", "Neu"]);
        assert_eq!(b.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["Defektpixel", "Mosaik"]);
        assert_eq!(c.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["Seriennummer"]);
    }

    #[test]
    fn exposure_in_one_line_with_the_35mm_equivalent() {
        let l = exposure_line(Some("1/50".into()), Some("f/2,8".into()), Some("ISO 400".into()), Some("30 mm".into()), Some("30 mm (45 mm KB)"));
        assert_eq!(l.as_deref(), Some("1/50 s · f/2,8 · ISO 400 · 30 mm (45 mm KB)"));
        assert_eq!(exposure_line(Some("0,8 s".into()), None, None, None, None).as_deref(), Some("0,8 s"));
        assert_eq!(exposure_line(None, None, None, None, Some("– mm (– mm KB)")), None);
        assert!(empty("– s · f/– · ISO –") && empty("–") && !empty("ISO 100") && !empty("f/2,8"));
    }
}
