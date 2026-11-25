//! Info lines below the photo of a grid cell (library and viewed folders): up to two lines, each showing one field
//! (file name, capture date, camera, lens, exposure, pixel size, file size, format). Off by default; the choice is kept
//! in `settings.json` (`gridInfoLine1`, `gridInfoLine2`). Each line that is on makes the cell [`LINE_H`] taller, with a
//! fixed height whether or not a photo has a value (the grid never changes its rows while scrolling).
//!
//! The text of a field is a pure function of the row ([`text`]); drawing and the loading of missing values for a
//! viewed folder live in the grid (`library_ui.rs`, `view_exif.rs`).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Once;

use library::db::Row;

use crate::menu::i18n::format::{date, decimal, time};
use crate::menu::i18n::Language;

/// Height of one info line.
pub const LINE_H: f32 = 14.0;
/// Font size of the info lines.
pub const SIZE: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Off,
    Name,
    Date,
    Camera,
    Lens,
    Exposure,
    Pixels,
    FileSize,
    Format,
}

impl Field {
    pub const ALL: [Field; 9] = [Field::Off, Field::Name, Field::Date, Field::Camera, Field::Lens, Field::Exposure, Field::Pixels, Field::FileSize, Field::Format];

    /// Value in `settings.json`.
    pub fn key(self) -> &'static str {
        match self {
            Field::Off => "off",
            Field::Name => "name",
            Field::Date => "date",
            Field::Camera => "camera",
            Field::Lens => "lens",
            Field::Exposure => "exposure",
            Field::Pixels => "pixels",
            Field::FileSize => "fileSize",
            Field::Format => "format",
        }
    }

    pub fn from_key(k: &str) -> Field {
        Field::ALL.into_iter().find(|f| f.key() == k).unwrap_or(Field::Off)
    }

    /// Name in the menu, translated.
    pub fn label(self) -> &'static str {
        match self {
            Field::Off => crate::tr("Aus"),
            Field::Name => crate::tr("Dateiname"),
            Field::Date => crate::tr("Aufnahmedatum"),
            Field::Camera => crate::tr("Kamera"),
            Field::Lens => crate::tr("Objektiv"),
            Field::Exposure => crate::tr("Belichtung"),
            Field::Pixels => crate::tr("Pixelmaße"),
            Field::FileSize => crate::tr("Dateigröße"),
            Field::Format => crate::tr("Format"),
        }
    }

    /// The field comes from the file header (a viewed folder has it only after reading it, see `view_exif.rs`).
    pub fn from_exif(self) -> bool {
        matches!(self, Field::Date | Field::Camera | Field::Lens | Field::Exposure | Field::Pixels)
    }
}

pub const KEYS: [&str; 2] = ["gridInfoLine1", "gridInfoLine2"];

static LINES: [AtomicU8; 2] = [AtomicU8::new(0), AtomicU8::new(0)];
static LOADED: Once = Once::new();

fn index(f: Field) -> u8 {
    Field::ALL.iter().position(|x| *x == f).unwrap_or(0) as u8
}

#[cfg(test)]
thread_local! {
    /// Tests choose the lines for their own thread only (other tests run in parallel with the default).
    static TEST_LINES: std::cell::Cell<Option<[Field; 2]>> = const { std::cell::Cell::new(None) };
}

/// Test aid: lines for this thread (`None`: as chosen).
#[cfg(test)]
pub fn set_for_test(lines: Option<[Field; 2]>) {
    TEST_LINES.with(|t| t.set(lines));
}

/// The two lines as chosen (read from the settings once, then kept in memory).
pub fn lines() -> [Field; 2] {
    #[cfg(test)]
    if let Some(l) = TEST_LINES.with(|t| t.get()) {
        return l;
    }
    LOADED.call_once(|| {
        for (i, k) in KEYS.iter().enumerate() {
            let f = Field::from_key(library::settings::value(k).as_str().unwrap_or("off"));
            LINES[i].store(index(f), Ordering::Relaxed);
        }
    });
    [0, 1].map(|i| Field::ALL[LINES[i].load(Ordering::Relaxed) as usize % Field::ALL.len()])
}

/// Choose line `i` (0 or 1) and keep the choice.
pub fn set(i: usize, f: Field) {
    lines();
    LINES[i].store(index(f), Ordering::Relaxed);
    library::settings::set(KEYS[i], serde_json::json!(f.key()));
}

/// Lines shown (an "Off" line takes no room, the other moves up).
pub fn shown() -> Vec<Field> {
    lines().into_iter().filter(|f| *f != Field::Off).collect()
}

/// Extra height of a cell for `n` shown lines (with a small gap above them).
pub fn extra_height(n: usize) -> f32 {
    if n == 0 { 0.0 } else { n as f32 * LINE_H + 2.0 }
}

/// A shown line needs values from the file header.
pub fn needs_exif() -> bool {
    lines().iter().any(|f| f.from_exif())
}

/// Text of `field` for row `z` (`name`: the file name as shown); `None` when the photo has no value for it.
pub fn text(field: Field, z: &Row, name: &str, lang: Language) -> Option<String> {
    match field {
        Field::Off => None,
        Field::Name => Some(name.to_string()).filter(|n| !n.is_empty()),
        Field::Date => capture_text(z.capture.as_deref()?, lang),
        Field::Camera => z.camera.clone().filter(|c| !c.trim().is_empty()),
        Field::Lens => z.lens.as_deref().filter(|c| !c.trim().is_empty()).map(lens_label),
        Field::Exposure => exposure(z.focal, z.aperture, z.time, z.iso, lang),
        Field::Pixels => pixels(z.width?, z.height?, lang),
        Field::FileSize => Some(file_size(z.size.filter(|s| *s >= 0)? as u64, lang)),
        Field::Format => Some(if z.format == "VIDEO" { crate::tr("Video").to_string() } else if z.sidecar_jpeg.is_some() { format!("{} + JPG", z.format) } else { z.format.clone() }),
    }
}

/// A lens name as shown: the stored suffix " (fixed)" of a built-in lens (see `library::exif::fixed_lens`)
/// in the UI language. The stored value stays as it is, so search and the lens filter keep matching.
pub(crate) fn lens_label(lens: &str) -> String {
    match lens.strip_suffix(" (fixed)") {
        Some(name) => format!("{name} ({})", crate::tr("fest")),
        None => lens.to_string(),
    }
}

/// "15.03.2013 15:19" from 'YYYY-MM-DDThh:mm:ss' (date only when the time is missing).
pub(crate) fn capture_text(a: &str, lang: Language) -> Option<String> {
    let num = |r: std::ops::Range<usize>| a.get(r).and_then(|s| s.parse::<u32>().ok());
    let d = date(lang, num(0..4)? as i32, num(5..7)? as u8, num(8..10)? as u8)?;
    match (num(11..13), num(14..16)).0.zip(num(14..16)).and_then(|(h, m)| time(h as u8, m as u8, None)) {
        Some(t) => Some(format!("{d} {t}")),
        None => Some(d),
    }
}

/// Number without trailing zeros after the point ("2,5", "13").
fn short(v: f64, places: usize, lang: Language) -> String {
    let s = decimal(lang, v, places);
    if s.contains(['.', ',']) { s.trim_end_matches('0').trim_end_matches(['.', ',']).to_string() } else { s }
}

/// "65 mm · f/4,0 · 1/125 s · ISO 400": the values present, in this order, never an empty part.
pub fn exposure(focal: Option<f64>, aperture: Option<f64>, time: Option<f64>, iso: Option<i64>, lang: Language) -> Option<String> {
    let ok = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
    let parts: Vec<String> = [
        ok(focal).map(|f| if f < 10.0 { format!("{} mm", short(f, 1, lang)) } else { format!("{} mm", f.round()) }),
        ok(aperture).map(|a| if a < 10.0 { format!("f/{}", decimal(lang, a, 1)) } else { format!("f/{}", short(a, 1, lang)) }),
        ok(time).map(|t| shutter(t, lang)),
        iso.filter(|i| *i > 0).map(|i| format!("ISO {i}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Exposure time: "1/125 s", "0,4 s", "2 s", "1,3 s".
fn shutter(t: f64, lang: Language) -> String {
    if t >= 1.0 {
        return format!("{} s", short(t, 1, lang));
    }
    let n = 1.0 / t;
    if n < 3.0 && (n - n.round()).abs() > 0.05 {
        format!("{} s", short(t, 1, lang))
    } else {
        format!("1/{} s", n.round())
    }
}

/// "6000 × 4000 · 24 MP" (below 10 MP with one decimal).
fn pixels(w: i64, h: i64, lang: Language) -> Option<String> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let mp = (w * h) as f64 / 1e6;
    let mp = if mp < 10.0 { decimal(lang, mp, 1) } else { format!("{}", mp.round()) };
    Some(format!("{w} × {h} · {mp} MP"))
}

/// "850 KB", "24,5 MB", "1,25 GB".
fn file_size(b: u64, lang: Language) -> String {
    match b {
        b if b < 1_000_000 => format!("{} KB", (b as f64 / 1e3).round().max(1.0)),
        b if b < 1_000_000_000 => format!("{} MB", decimal(lang, b as f64 / 1e6, 1)),
        b => format!("{} GB", decimal(lang, b as f64 / 1e9, 2)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            name: "IMG_0042.CR2".into(),
            format: "CR2".into(),
            camera: Some("Canon EOS RP".into()),
            lens: Some("RF24-105mm F4 L IS USM".into()),
            capture: Some("2024-05-11T14:03:22".into()),
            focal: Some(65.0),
            aperture: Some(4.0),
            time: Some(1.0 / 125.0),
            iso: Some(400),
            width: Some(6240),
            height: Some(4160),
            size: Some(24_512_000),
            ..Row::default()
        }
    }

    #[test]
    fn exposure_line_in_order_and_localised() {
        let z = row();
        assert_eq!(text(Field::Exposure, &z, "", Language::De).as_deref(), Some("65 mm · f/4,0 · 1/125 s · ISO 400"));
        assert_eq!(text(Field::Exposure, &z, "", Language::En).as_deref(), Some("65 mm · f/4.0 · 1/125 s · ISO 400"));
        assert_eq!(exposure(Some(4.3), Some(1.8), Some(0.4), Some(50), Language::De).as_deref(), Some("4,3 mm · f/1,8 · 0,4 s · ISO 50"));
        assert_eq!(exposure(Some(200.0), Some(11.0), Some(2.0), None, Language::En).as_deref(), Some("200 mm · f/11 · 2 s"));
        assert_eq!(exposure(None, None, Some(1.0 / 3.0), None, Language::De).as_deref(), Some("1/3 s"));
        assert_eq!(exposure(None, None, Some(1.3), None, Language::Fr).as_deref(), Some("1,3 s"));
    }

    #[test]
    fn missing_values_leave_no_empty_parts() {
        assert_eq!(exposure(None, Some(2.8), None, Some(100), Language::De).as_deref(), Some("f/2,8 · ISO 100"));
        assert_eq!(exposure(Some(35.0), None, None, None, Language::De).as_deref(), Some("35 mm"));
        assert_eq!(exposure(Some(0.0), Some(f64::NAN), None, Some(0), Language::De), None);
        assert_eq!(exposure(None, None, None, None, Language::De), None);
        let empty = Row { name: "a.png".into(), format: "PNG".into(), ..Row::default() };
        for f in [Field::Date, Field::Camera, Field::Lens, Field::Exposure, Field::Pixels, Field::FileSize, Field::Off] {
            assert_eq!(text(f, &empty, "a.png", Language::De), None, "{f:?}");
        }
        assert_eq!(text(Field::Camera, &Row { camera: Some("  ".into()), ..Row::default() }, "", Language::De), None);
    }

    #[test]
    fn other_fields() {
        let z = row();
        assert_eq!(text(Field::Name, &z, "IMG_0042.CR2", Language::De).as_deref(), Some("IMG_0042.CR2"));
        assert_eq!(text(Field::Date, &z, "", Language::De).as_deref(), Some("11.05.2024 14:03"));
        assert_eq!(capture_text("2024-05-11T14:03:22", Language::En).as_deref(), Some("2024-05-11 14:03"));
        assert_eq!(capture_text("2024-05-11T14:03:22", Language::Fr).as_deref(), Some("11/05/2024 14:03"));
        assert_eq!(capture_text("2024-05-11", Language::Ja).as_deref(), Some("2024/05/11"));
        assert_eq!(lens_label("SIGMA 30mm f/2.8"), "SIGMA 30mm f/2.8");
        assert!(lens_label("SIGMA 30mm f/2.8 (fixed)").starts_with("SIGMA 30mm f/2.8 ("));
        assert_eq!(text(Field::Date, &z, "", Language::En).as_deref(), Some("2024-05-11 14:03"));
        assert_eq!(text(Field::Date, &Row { capture: Some("2024-05-11".into()), ..Row::default() }, "", Language::De).as_deref(), Some("11.05.2024"));
        assert_eq!(text(Field::Pixels, &z, "", Language::De).as_deref(), Some("6240 × 4160 · 26 MP"));
        assert_eq!(pixels(3000, 2000, Language::De).as_deref(), Some("3000 × 2000 · 6,0 MP"));
        assert_eq!(text(Field::FileSize, &z, "", Language::De).as_deref(), Some("24,5 MB"));
        assert_eq!(text(Field::FileSize, &z, "", Language::En).as_deref(), Some("24.5 MB"));
        assert_eq!(file_size(850_000, Language::De), "850 KB");
        assert_eq!(file_size(1_250_000_000, Language::De), "1,25 GB");
        assert_eq!(text(Field::Format, &z, "", Language::De).as_deref(), Some("CR2"));
        assert_eq!(text(Field::Format, &Row { format: "X3F".into(), sidecar_jpeg: Some(3), ..Row::default() }, "", Language::De).as_deref(), Some("X3F + JPG"));
        assert_eq!(text(Field::Camera, &z, "", Language::De).as_deref(), Some("Canon EOS RP"));
    }

    #[test]
    fn keys_round_trip_and_heights() {
        for f in Field::ALL {
            assert_eq!(Field::from_key(f.key()), f);
        }
        assert_eq!(Field::from_key("unknown"), Field::Off);
        assert_eq!(extra_height(0), 0.0);
        assert_eq!(extra_height(1), LINE_H + 2.0);
        assert_eq!(extra_height(2), 2.0 * LINE_H + 2.0);
        assert!(Field::Exposure.from_exif() && !Field::FileSize.from_exif() && !Field::Name.from_exif());
    }
}
