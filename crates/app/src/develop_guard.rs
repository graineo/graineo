//! Protection of developments written by a newer graineo.
//!
//! A newer version may store fields this version does not know: new mask fields (the mask document refuses unknown
//! fields, so the whole `develop::Settings` does not parse) or a newer `RawAdjust` value. Every load path then falls
//! back to the initial values. Saving them would replace the newer development by those values, and the work would be
//! lost when going back to an older version (seen with 1.2.0 and the masks of 1.3.0). So a stored development that
//! this version cannot read is **never overwritten or deleted**:
//!
//! - every save path (library and sidecar, original and virtual copy, Sigma and other formats, the browser, "Apply to
//!   the selection") asks [`check`] before it writes or removes a development; ratings are still saved;
//! - unchanged initial values are not a change of the user: saving them is a silent no-op (no message on every photo
//!   switch); a real change is refused with [`newer_text`] in the status line;
//! - the photo shows a condition ("!", [`crate::photo_state::newer_develop`]) while it is open.
//!
//! Decision (17.11.2026): refuse instead of passing unknown fields through. Passing them through would need a parser
//! that keeps unknown fields at every level (top level, `raw`, each mask layer and leaf). `Settings` has no list of its
//! own field names, many fields are left out while they hold their default, and old field names are translated on
//! reading, so "unknown" cannot be told from "known, left out, or renamed" without a list of every field; and the mask
//! crate refuses unknown fields on purpose (a mask with an unknown meaning must not be rendered half). Refusing is
//! small, safe and the same rule that already protected the masks of other formats.
//!
//! For later versions: `Settings` ignores unknown fields at the top level, so a newer version that adds such a field
//! whose loss matters writes [`READER_FORMAT_KEY`] with a number above [`READER_FORMAT`]; this version then protects
//! the development as well.

use develop::Settings;

/// Highest `readerFormat` of a stored development this version understands (see the module text).
pub const READER_FORMAT: u64 = 1;
/// Key of that number at the top level of a stored development (written only by newer versions that need it).
pub const READER_FORMAT_KEY: &str = "readerFormat";

/// A stored development as this version sees it.
#[derive(Debug, PartialEq)]
pub enum Stored {
    /// none stored
    None,
    /// readable
    Readable(Box<Settings>),
    /// stored, but not readable here (newer version or damaged); the text says why (for the log)
    Foreign(String),
}

/// Read a stored development (JSON of the library or the sidecar).
pub fn read(json: Option<&str>) -> Stored {
    let Some(json) = json else { return Stored::None };
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(v) => read_value(Some(&v)),
        Err(e) => Stored::Foreign(format!("not JSON: {e}")),
    }
}

/// [`read`] for a development stored as a JSON value (virtual copies in the sidecar). `null` counts as none.
pub fn read_value(v: Option<&serde_json::Value>) -> Stored {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Stored::None };
    if let Some(n) = v.get(READER_FORMAT_KEY) {
        if n.as_u64().is_none_or(|n| n > READER_FORMAT) {
            return Stored::Foreign(format!("{READER_FORMAT_KEY} {n} is newer than {READER_FORMAT}"));
        }
    }
    let s = match serde_json::from_value::<Settings>(v.clone()) {
        Ok(s) => s,
        Err(e) => return Stored::Foreign(format!("settings: {e}")),
    };
    // the mask document checks more than its fields (its version, the values); as when it is opened
    if let Some(local) = v.get("local").filter(|l| !l.is_null()) {
        if let Err(e) = masks::Document::from_json(&local.to_string()) {
            return Stored::Foreign(format!("masks: {e}"));
        }
    }
    Stored::Readable(Box::new(s))
}

/// The message when saving is refused.
pub fn newer_text() -> String {
    crate::tr("Diese Bearbeitung stammt aus einer neueren Version und wird nicht überschrieben").to_string()
}

/// May a development be written over the stored one? `Err` (with [`newer_text`]) when one is stored that this version
/// cannot read.
pub fn check(json: Option<&str>) -> Result<(), String> {
    verdict(read(json))
}

/// [`check`] for a development stored as a JSON value.
pub fn check_value(v: Option<&serde_json::Value>) -> Result<(), String> {
    verdict(read_value(v))
}

fn verdict(s: Stored) -> Result<(), String> {
    match s {
        Stored::Foreign(why) => {
            crate::log(&format!("stored development not readable ({why}): not overwritten"));
            Err(newer_text())
        }
        _ => Ok(()),
    }
}

/// The save paths: `None` = go on and save; `Some(result)` = stop here, the stored development is not readable and
/// must stay as it is: `Ok` (silently) when `unchanged` (the settings are the initial values shown instead of it:
/// nothing the user did, nothing to say), otherwise the error [`newer_text`].
pub fn stop(blocked: Result<(), String>, unchanged: bool) -> Option<Result<(), String>> {
    match blocked {
        Ok(()) => None,
        Err(_) if unchanged => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// On loading the development of a photo (original): remember or clear the condition "from a newer version".
pub fn note_loaded(path: &std::path::Path, json: Option<&str>) {
    if matches!(read(json), Stored::Foreign(_)) {
        crate::photo_state::set(path, crate::photo_state::newer_develop());
    } else {
        crate::photo_state::clear(path, crate::photo_state::Kind::NewerDevelop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_unknown_mask_field() -> String {
        let mut doc = masks::Document::from_json(include_str!("../../masks/samples/masks-example.json")).unwrap();
        doc.settle_version();
        let s = Settings { exposure: 0.7, local: Some(doc), ..Default::default() };
        let mut v = serde_json::to_value(&s).unwrap();
        // a field of a newer version in the first layer (the mask crate refuses unknown fields)
        v["local"]["layers"][0]["futureField"] = serde_json::json!(1);
        v.to_string()
    }

    #[test]
    fn developments_of_a_newer_version_are_recognised() {
        assert_eq!(read(None), Stored::None);
        assert_eq!(read_value(Some(&serde_json::Value::Null)), Stored::None);
        let plain = serde_json::to_string(&Settings { exposure: 0.3, ..Default::default() }).unwrap();
        assert!(matches!(read(Some(&plain)), Stored::Readable(s) if s.exposure == 0.3));
        assert!(check(Some(&plain)).is_ok() && check(None).is_ok());
        // unknown mask field, mask version above ours, damaged text, a newer reader format
        let unknown = with_unknown_mask_field();
        assert!(serde_json::from_str::<Settings>(&unknown).is_err(), "the load paths fall back to the initial values");
        assert!(matches!(read(Some(&unknown)), Stored::Foreign(_)));
        let mut v: serde_json::Value = serde_json::from_str(&plain).unwrap();
        v["local"] = serde_json::from_str(include_str!("../../masks/samples/masks-example.json")).unwrap();
        v["local"]["version"] = 999.into();
        assert!(matches!(read_value(Some(&v)), Stored::Foreign(_)), "mask document of a newer version");
        assert!(matches!(read(Some("{\"exposure\":")), Stored::Foreign(_)));
        let mut newer: serde_json::Value = serde_json::from_str(&plain).unwrap();
        newer[READER_FORMAT_KEY] = 2.into();
        assert!(serde_json::from_value::<Settings>(newer.clone()).is_ok(), "unknown top-level fields alone parse");
        assert!(matches!(read_value(Some(&newer)), Stored::Foreign(_)), "a newer reader format protects them");
        newer[READER_FORMAT_KEY] = READER_FORMAT.into();
        assert!(matches!(read_value(Some(&newer)), Stored::Readable(_)));
        assert_eq!(check(Some(&unknown)).unwrap_err(), newer_text());
        // unchanged initial values: no message, nothing written
        assert_eq!(stop(check(Some(&unknown)), true), Some(Ok(())), "stops without a word");
        assert_eq!(stop(check(Some(&unknown)), false), Some(Err(newer_text())));
        assert_eq!(stop(Ok(()), false), None, "readable: saving goes on");
        // an unknown engine value of a newer version is readable (the field is a free text)
        let engine = serde_json::to_string(&Settings { raw: develop::settings::RawAdjust { engine: Some("future:1".into()), ..Default::default() }, ..Default::default() }).unwrap();
        assert!(matches!(read(Some(&engine)), Stored::Readable(_)));
    }

    /// Every save path keeps a development it cannot read: sidecar of a photo outside the library (original and
    /// virtual copy), Sigma X3F outside the library. The library paths: `library_store_tests`.
    #[test]
    fn saving_never_overwrites_a_development_of_a_newer_version() {
        use crate::sidecar::Meta;
        let root = std::env::temp_dir().join(format!("graineo-newer-dev-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let unknown = with_unknown_mask_field();
        let changed = Settings { exposure: 1.2, ..Default::default() };

        // another format, original: the sidecar keeps the development; the rating is still saved
        let jpg = root.join("a.jpg");
        image::RgbImage::from_pixel(16, 16, image::Rgb([90, 90, 90])).save(&jpg).unwrap();
        let xp = library::xmp::path_to(&jpg);
        library::xmp::write_develop(&xp, Some(&unknown)).unwrap();
        let (shown, _, _) = crate::raw_app::load_settings(&jpg, None);
        assert!(crate::raw_app::save_settings(&jpg, None, &shown, &Meta::default()).is_ok(), "unchanged: no message");
        assert_eq!(crate::raw_app::save_settings(&jpg, None, &changed, &Meta { rating: 4, ..Default::default() }).unwrap_err(), newer_text());
        assert_eq!(library::xmp::read_develop(&xp).unwrap().as_deref(), Some(unknown.as_str()), "development kept");
        let library::xmp::ReadResult::Ok(w) = library::xmp::read(&xp) else { panic!("sidecar") };
        assert_eq!(w.stars, 4, "the rating is saved");
        assert!(crate::photo_state::get(&jpg).iter().any(|n| n.kind == crate::photo_state::Kind::NewerDevelop), "the photo says why");

        // another format, virtual copy in the sidecar
        let copies = serde_json::json!([{ "name": "Kopie 1", "rating": 0, "flag": 0, "label": 0, "keywords": [], "settings": serde_json::from_str::<serde_json::Value>(&unknown).unwrap() }]).to_string();
        let library::xmp::ReadResult::Ok(mut w) = library::xmp::read(&xp) else { panic!("sidecar") };
        w.copies_json = Some(copies);
        library::xmp::write(&xp, &w).unwrap();
        let copy_before = library::sidecar::read_copies(w.copies_json.as_deref());
        assert!(crate::raw_app::save_settings(&jpg, Some("Kopie 1"), &Settings::default(), &Meta::default()).is_ok());
        assert_eq!(crate::raw_app::save_settings(&jpg, Some("Kopie 1"), &changed, &Meta { rating: 2, ..Default::default() }).unwrap_err(), newer_text());
        let library::xmp::ReadResult::Ok(w) = library::xmp::read(&xp) else { panic!("sidecar") };
        let copy_after = library::sidecar::read_copies(w.copies_json.as_deref());
        assert_eq!(copy_after[0].settings, copy_before[0].settings, "the copy's development is kept");
        assert_eq!(copy_after[0].stars, 2, "its rating is saved");

        // Sigma X3F outside the library (sidecar `NAME.xmp`): neither replaced nor removed
        let x3f = root.join("SDIM0001.X3F");
        std::fs::write(&x3f, b"not a real X3F").unwrap();
        let sp = crate::sidecar::sidecar_path(&x3f);
        library::xmp::write_develop(&sp, Some(&unknown)).unwrap();
        let (shown, _) = crate::sidecar::load(&x3f);
        assert!(crate::sidecar::save(&x3f, &shown, &Meta::default()).is_ok(), "the initial values: nothing to say, nothing removed");
        assert_eq!(library::xmp::read_develop(&sp).unwrap().as_deref(), Some(unknown.as_str()), "not removed");
        assert_eq!(crate::sidecar::save(&x3f, &changed, &Meta::default()).unwrap_err(), newer_text());
        assert_eq!(library::xmp::read_develop(&sp).unwrap().as_deref(), Some(unknown.as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
