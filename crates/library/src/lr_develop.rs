//! Read an image's Lightroom develop settings from the catalog (read-only,
//! see [`crate::lightroom::open_catalog`]): text from
//! `Adobe_imageDevelopSettings.text` (Lua table, interpreted in `rawdev::lightroom`).
//!
//! Matched via the path in the catalog (root folder + folder + name). If the image is now
//! somewhere else (e.g. moved to the NAS), via the name and the last folder names.

use std::path::Path;

use rusqlite::params;

/// Settings of an image or a virtual copy (`copy` = name of the copy).
#[derive(Clone, Debug)]
pub struct Entry {
    pub copy: Option<String>,
    pub text: String,
    /// path in the catalog
    pub catalog_path: String,
    /// orientation of the image in Lightroom (`AB`, `BC`, `CD`, `DA` …, including manual rotation)
    pub orientation: Option<String>,
}

// ------------------------------------------------------------------------------------------
// Lightroom's own as-shot white balance (reference for a changed white balance)

/// Name of the line graineo appends to a taken-over settings text when the catalog knows Lightroom's own as-shot
/// white balance of the photo: `graineoAsShot = { Temperature = 8000, Tint = 1 }`. It stands after the settings
/// table (which stays unchanged character for character), so every reader of the table ignores it;
/// `rawdev::lightroom::from_catalog` reads it as the key of the same name.
///
/// Why: Lightroom's temperature/tint are absolute numbers computed with its own camera profile. Where its matrices
/// differ from the ones graineo uses, the same numbers mean another colour. Read as a distance from Lightroom's
/// as-shot value they carry over independently of the camera (`rawdev::wb::from_lightroom_relative`).
pub const AS_SHOT_KEY: &str = "graineoAsShot";

/// `text` with the as-shot reference line (see [`AS_SHOT_KEY`]); a line already there is replaced.
pub fn with_as_shot(text: &str, kelvin: f64, tint: f64) -> String {
    format!("{}\n{AS_SHOT_KEY} = {{ Temperature = {kelvin}, Tint = {tint} }}", without_as_shot(text).trim_end())
}

/// `text` without the as-shot reference line.
pub fn without_as_shot(text: &str) -> &str {
    match text.find(&format!("\n{AS_SHOT_KEY} = ")) {
        Some(i) => &text[..i],
        None => text,
    }
}

/// Value of the key `key` at the top level of the settings table (`s = { key = value, … }`), as raw text without
/// quotes. Nested tables (masks, looks …) and strings are skipped.
fn top_level(text: &str, key: &str) -> Option<String> {
    let b = text.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' | b'\'' => quote = Some(c),
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return None;
                }
            }
            _ if depth == 1 && text.is_char_boundary(i) && text[i..].starts_with(key) && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) => {
                let rest = text[i + key.len()..].trim_start_matches([' ', '\t']);
                if let Some(v) = rest.strip_prefix('=') {
                    let v = v.trim_start();
                    let end = v.find([',', '\n', '}']).unwrap_or(v.len());
                    return Some(v[..end].trim().trim_matches('"').to_string());
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// White balance fields of a settings text: mode, temperature, tint, camera profile.
struct WhiteBalance {
    mode: Option<String>,
    kelvin: Option<f64>,
    tint: Option<f64>,
    profile: Option<String>,
}

fn white_balance(text: &str) -> WhiteBalance {
    let num = |k: &str| top_level(text, k).and_then(|v| v.trim_start_matches('+').parse::<f64>().ok()).filter(|v| v.is_finite());
    WhiteBalance { mode: top_level(text, "WhiteBalance"), kelvin: num("Temperature"), tint: num("Tint"), profile: top_level(text, "CameraProfile") }
}

/// Does the photo need Lightroom's as-shot value as a reference? Its white balance is not "As Shot" and carries numbers
/// (Custom, but also Daylight, Auto … – Lightroom stores their numbers too).
pub fn needs_as_shot(text: &str) -> bool {
    let w = white_balance(text);
    w.kelvin.is_some() && w.mode.as_deref() != Some("As Shot")
}

/// Settings text of a history step or before-settings column: plain text or a blob (4-byte big-endian length,
/// then zlib).
fn decode(v: rusqlite::types::Value) -> Option<String> {
    use std::io::Read;
    match v {
        rusqlite::types::Value::Text(t) => Some(t),
        rusqlite::types::Value::Blob(b) => {
            if b.len() > 4 {
                let mut s = String::new();
                if flate2::read::ZlibDecoder::new(&b[4..]).take(64 << 20).read_to_string(&mut s).is_ok() {
                    return Some(s);
                }
            }
            String::from_utf8(b).ok()
        }
        _ => None,
    }
}

/// Lightroom's own as-shot white balance (Kelvin, tint) of the catalog image `image`, for the camera profile `profile`
/// (that of its current settings): from the develop history (a step with "As Shot" and numbers, e.g. the import step)
/// or the before settings; for a virtual copy without such a step from its master. Preference: same profile before
/// another one (the as-shot numbers depend on the profile), the image before its master, the history before the before
/// settings, the latest step first. `None` if the catalog records none (photo never opened in Lightroom's develop
/// module …) or lacks the tables.
pub fn lr_as_shot(conn: &rusqlite::Connection, image: i64, master: Option<i64>, profile: Option<&str>) -> Option<(f64, f64)> {
    let of = |img: i64| -> Vec<WhiteBalance> {
        let mut found = Vec::new();
        let mut read = |sql: &str| {
            let Ok(mut st) = conn.prepare_cached(sql) else { return };
            let Ok(rows) = st.query_map([img], |r| r.get::<_, rusqlite::types::Value>(0)) else { return };
            found.extend(rows.filter_map(Result::ok).filter_map(decode).map(|t| white_balance(&t)).filter(|w| w.mode.as_deref() == Some("As Shot") && w.kelvin.is_some() && w.tint.is_some()));
        };
        // latest first
        read("SELECT text FROM Adobe_libraryImageDevelopHistoryStep WHERE image = ?1 ORDER BY dateCreated DESC, id_local DESC");
        read("SELECT b.beforeText FROM Adobe_imageDevelopBeforeSettings b JOIN Adobe_imageDevelopSettings d ON d.id_local = b.developSettings WHERE d.image = ?1 ORDER BY b.id_local DESC");
        found
    };
    let own = of(image);
    let from_master = master.filter(|m| *m != image).map(of).unwrap_or_default();
    let same = |l: &[WhiteBalance]| l.iter().find(|w| w.profile.as_deref() == profile).and_then(|w| Some((w.kelvin?, w.tint?)));
    let any = |l: &[WhiteBalance]| l.first().and_then(|w| Some((w.kelvin?, w.tint?)));
    same(&own).or_else(|| same(&from_master)).or_else(|| any(&own)).or_else(|| any(&from_master))
}

/// `text` with Lightroom's as-shot reference attached when the photo needs one and the catalog has it (see
/// [`AS_SHOT_KEY`]); otherwise `text` unchanged.
pub fn attach_as_shot(conn: &rusqlite::Connection, image: i64, master: Option<i64>, text: String) -> String {
    if !needs_as_shot(&text) {
        return text;
    }
    let profile = white_balance(&text).profile;
    match lr_as_shot(conn, image, master, profile.as_deref()) {
        Some((k, t)) => with_as_shot(&text, k, t),
        None => text,
    }
}

fn parts(p: &str) -> Vec<String> {
    p.split('/').filter(|t| !t.is_empty()).map(|t| crate::nfc(t).to_lowercase()).collect()
}

/// State of the catalog files (size and modification time of the catalog and its WAL): a cached
/// answer is only valid as long as this stays the same.
fn catalog_state(catalog: &Path) -> Option<[(u64, std::time::SystemTime); 2]> {
    let stamp = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
    let mut wal = catalog.as_os_str().to_owned();
    wal.push("-wal");
    Some([stamp(catalog)?, stamp(Path::new(&wal)).unwrap_or((0, std::time::UNIX_EPOCH))])
}

type CacheEntry = (std::path::PathBuf, std::path::PathBuf, [(u64, std::time::SystemTime); 2], Vec<Entry>);

/// Recent answers: opening an image asks twice (orientation, initial values), preloading and the
/// offline copy ask again – each query scans the file table of the catalog (≈ 30–50 ms).
static CACHE: std::sync::Mutex<Vec<CacheEntry>> = std::sync::Mutex::new(Vec::new());

/// All settings (original and virtual copies) for the file `image`.
pub fn read(catalog: &Path, image: &Path) -> Result<Vec<Entry>, String> {
    let state = catalog_state(catalog);
    if let Some(state) = state {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, _, e)) = cache.iter().find(|(c, b, s, _)| c == catalog && b == image && *s == state) {
            return Ok(e.clone());
        }
    }
    let entries = read_uncached(catalog, image)?;
    if let Some(state) = state {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(c, b, s, _)| !(c == catalog && b == image) && *s == state);
        cache.push((catalog.to_path_buf(), image.to_path_buf(), state, entries.clone()));
        if cache.len() > 64 {
            cache.remove(0);
        }
    }
    Ok(entries)
}

fn read_uncached(catalog: &Path, image: &Path) -> Result<Vec<Entry>, String> {
    let conn = crate::lightroom::open_catalog(catalog).map_err(|e| e.to_string())?;
    let name = image.file_name().and_then(|n| n.to_str()).ok_or("kein Dateiname")?;
    let (root, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let mut st = conn
        .prepare(
            "SELECT r.absolutePath || fo.pathFromRoot || f.baseName || '.' || f.extension, i.copyName, d.text, i.orientation, i.id_local, i.masterImage
             FROM AgLibraryFile f JOIN AgLibraryFolder fo ON fo.id_local = f.folder JOIN AgLibraryRootFolder r ON r.id_local = fo.rootFolder
             JOIN Adobe_images i ON i.rootFile = f.id_local LEFT JOIN Adobe_imageDevelopSettings d ON d.image = i.id_local
             WHERE f.baseName = ?1 COLLATE NOCASE AND f.extension = ?2 COLLATE NOCASE",
        )
        .map_err(|e| e.to_string())?;
    type Row = (String, Option<String>, Option<String>, Option<String>, i64, Option<i64>);
    let rows: Vec<Row> = st
        .query_map(params![root, ext], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    let target = parts(&image.to_string_lossy());
    // best path: exactly equal, else the longest common suffix (at least folder + name)
    let fits = |p: &str| -> usize {
        let t = parts(p);
        t.iter().rev().zip(target.iter().rev()).take_while(|(a, b)| a == b).count()
    };
    let best_match = rows.iter().map(|z| fits(&z.0)).max().unwrap_or(0);
    if best_match < 2 {
        return Ok(Vec::new());
    }
    Ok(rows
        .into_iter()
        .filter(|z| fits(&z.0) == best_match)
        // Lightroom sometimes keeps a second, empty settings row next to the real one (118 photos in the user's catalog):
        // skip it, otherwise the photo opens without its edits.
        .filter_map(|(p, copy, text, orientation, image, master)| {
            let text = text.filter(|t| !t.trim().is_empty())?;
            Some(Entry { copy, text: attach_as_shot(&conn, image, master, text), catalog_path: p, orientation })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUSTOM: &str = "s = {\n\tCameraProfile = \"Adobe Standard\",\n\tIncrementalTemperature = 3,\n\tLook = { Parameters = { Temperature = 100, WhiteBalance = \"As Shot\" } },\n\tTemperature = 9000,\n\tTint = -4,\n\tWhiteBalance = \"Custom\",\n}\n";

    #[test]
    fn white_balance_fields_from_the_top_level_only() {
        let w = white_balance(CUSTOM);
        assert_eq!((w.mode.as_deref(), w.kelvin, w.tint, w.profile.as_deref()), (Some("Custom"), Some(9000.0), Some(-4.0), Some("Adobe Standard")));
        assert!(needs_as_shot(CUSTOM));
        assert!(!needs_as_shot("s = { Temperature = 5000, Tint = 3, WhiteBalance = \"As Shot\" }"));
        assert!(!needs_as_shot("s = { WhiteBalance = \"Daylight\" }"), "no numbers: our preset");
        assert!(needs_as_shot("s = { Temperature = 5000, Tint = 3 }"), "numbers without a mode count as custom");
        assert!(top_level("s = { Name = \"Tint = 5\" }", "Tint").is_none(), "not inside strings");
    }

    #[test]
    fn as_shot_line_is_appended_and_replaced() {
        let t = with_as_shot(CUSTOM, 8000.0, 1.0);
        assert_eq!(t, format!("{}\ngraineoAsShot = {{ Temperature = 8000, Tint = 1 }}", CUSTOM.trim_end()));
        assert!(t.starts_with(CUSTOM.trim_end()), "the settings table stays unchanged");
        let t2 = with_as_shot(&t, 7950.0, -2.5);
        assert!(t2.ends_with("graineoAsShot = { Temperature = 7950, Tint = -2.5 }") && t2.matches(AS_SHOT_KEY).count() == 1, "{t2}");
        assert_eq!(without_as_shot(&t2), CUSTOM.trim_end());
        // the reference line does not change what the top level says
        assert_eq!(white_balance(&t2).kelvin, Some(9000.0));
    }
}
