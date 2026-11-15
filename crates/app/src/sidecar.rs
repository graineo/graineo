//! Settings, rating and flag per image – the original file is never modified.
//!
//! - Sigma photos (X3F) have, since 0.39, like every RAW an XMP sidecar next to the photo (`BILD.xmp`): stars, flag,
//!   label, keywords, virtual copies and the develop settings (`graineo:DevelopSettings`). A foreign XMP is preserved
//!   character for character, only our fields change.
//! - Protection: an existing but unreadable sidecar (damaged or from a newer version) is never
//!   overwritten; saving then reports an error.
//! - Initial values: without own settings (no `DevelopSettings`), the settings SIGMA Photo Pro stored in
//!   the X3F file apply (read only, [`develop::sppa`]), otherwise the defaults.
//!   Nothing is saved until something differs from them. Images for which the camera had black & white
//!   or sepia set open in black & white ([`develop::sppa::baseline`]).

use std::path::{Path, PathBuf};

use develop::Settings;
use serde::{Deserialize, Serialize};

/// Flag: P = picked, X = rejected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Flag {
    #[default]
    None,
    Pick,
    Reject,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// 0 … 5 stars
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub flag: Flag,
    /// Color label as in the library: 0 none, 1 red, 2 yellow, 3 green, 4 blue, 5 purple
    #[serde(default, skip_serializing_if = "is_zero")]
    pub label: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

impl Flag {
    /// As in the library: 1 picked, 0 none, −1 rejected.
    pub fn as_i8(self) -> i8 {
        match self {
            Flag::Pick => 1,
            Flag::Reject => -1,
            Flag::None => 0,
        }
    }
    pub fn from_i8(v: i8) -> Flag {
        match v {
            1 => Flag::Pick,
            -1 => Flag::Reject,
            _ => Flag::None,
        }
    }
}

/// Program name in the XMP (`xmp:CreatorTool`).
const XMP_TOOL: &str = library::XMP_TOOL;

/// Sidecar (XMP) next to the photo.
pub fn sidecar_path(x3f: &Path) -> PathBuf {
    library::xmp::path(x3f, "X3F", false)
}

/// All sidecars of a photo created by graineo (for deleting along): the XMP, if it is ours.
pub fn companion_files(x3f: &Path) -> Vec<PathBuf> {
    let xp = sidecar_path(x3f);
    if is_our_xmp(&xp) { vec![xp] } else { Vec::new() }
}

/// Meta from the XMP (empty if there is none).
fn read_meta(xp: &Path) -> Meta {
    match library::xmp::read(xp) {
        library::xmp::ReadResult::Ok(w) => Meta { rating: w.stars, flag: Flag::from_i8(w.flag), label: w.label },
        _ => Meta::default(),
    }
}

/// Own settings from the XMP (`None`: none of our own).
fn read_settings(xp: &Path) -> Option<Settings> {
    library::xmp::read_develop(xp).ok().flatten().and_then(|j| serde_json::from_str::<Settings>(&j).ok())
}

/// Load the settings of an entry: X3F or non-Sigma RAW, original or virtual copy.
/// Returns: (settings, rating, own settings present?).
pub fn load_entry(path: &Path, copy: Option<&str>) -> (Settings, Meta, bool) {
    if crate::image_kind::of(path) != crate::image_kind::ImageKind::Foveon {
        return crate::raw_app::load_settings(path, copy);
    }
    match copy {
        None => {
            let (s, m) = load(path);
            (s, m, true)
        }
        Some(_) => {
            let (s, m, stored) = crate::raw_app::load_settings(path, copy);
            if stored {
                (s, m, true)
            } else {
                (baseline(path), m, false)
            }
        }
    }
}

/// Save an entry (see [`load_entry`]). Virtual copy of an X3F: entry in `graineo:VirtualCopies` of the original's
/// XMP (everything else stays).
pub fn save_entry(path: &Path, copy: Option<&str>, s: &Settings, meta: &Meta) -> Result<(), String> {
    if let Some(local)=&s.local {local.validate()?;}
    if crate::image_kind::of(path) != crate::image_kind::ImageKind::Foveon {
        return crate::raw_app::save_settings(path, copy, s, meta);
    }
    if copy.is_none() {
        return save(path, s, meta);
    }
    crate::raw_app::save_settings(path, copy, s, meta)
}

/// Own settings of an X3F (original): from the library for its photos, otherwise from the sidecar.
pub fn own_settings(x3f: &Path) -> Option<Settings> {
    library::store::develop_for(x3f).and_then(|j| serde_json::from_str::<Settings>(&j).ok())
}

pub fn load(x3f: &Path) -> (Settings, Meta) {
    if let Some(loc) = library::store::locate(x3f) {
        let meta = library::store::meta(loc.0, None).map(|m| Meta { rating: m.0, flag: Flag::from_i8(m.1), label: m.2 }).unwrap_or_default();
        let own = library::store::develop_at(x3f, Some(loc), None).ok().flatten().and_then(|j| serde_json::from_str::<Settings>(&j).ok());
        return (own.unwrap_or_else(|| baseline(x3f)), meta);
    }
    let xp = sidecar_path(x3f);
    (read_settings(&xp).unwrap_or_else(|| baseline(x3f)), read_meta(&xp))
}

/// Initial values without own settings: settings stored by SPP in the X3F,
/// otherwise the defaults. Reads only the header and small sections of the X3F (never writes).
fn baseline(x3f: &Path) -> Settings {
    let mut s = develop::sppa::baseline(x3f).unwrap_or_default();
    // Setting "Binning" (Quattro only, photos without own settings)
    crate::binning_ui::apply_standard(x3f, &mut s);
    s
}

/// Settings after "Reset": neutral values with the binning default from the preferences (as for photos without
/// own settings), keeping the view. Sigma photos get the colour mode "Neutral" explicitly (an empty one would be the
/// camera's colour mode, the start value on opening), also monochrome shots (they
/// come back in colour).
pub fn reset(view: develop::View, path: &Path) -> Settings {
    let mut s = Settings { view, ..Settings::default() };
    if crate::image_kind::of(path) == crate::image_kind::ImageKind::Foveon {
        s.color_mode = Some(NEUTRAL_MODE.to_string());
    }
    crate::binning_ui::apply_standard(path, &mut s);
    s
}

/// Colour mode of a Sigma photo after "Reset".
pub const NEUTRAL_MODE: &str = "Neutral";

/// Do settings from SIGMA Photo Pro currently apply to the image (none of our own, and SPP
/// stored something other than the defaults)?
pub fn from_spp(x3f: &Path) -> bool {
    if let Some(loc) = library::store::locate(x3f) {
        let own = library::store::develop_at(x3f, Some(loc), None).map_or(true, |e| e.is_some());
        return !own && develop::sppa::load(x3f).is_some_and(|s| s != Settings::default());
    }
    let own = library::xmp::read_develop(&sidecar_path(x3f)).map_or(true, |e| e.is_some());
    !own && develop::sppa::load(x3f).is_some_and(|s| s != Settings::default())
}

/// Saves the settings only if they differ from the initial values (default or from SPP, see [`load`]), and otherwise
/// removes them from the XMP again; rating and flag only on change (foreign XMP fields stay). Errors (e.g. protected,
/// unreadable file) as text.
pub fn save(x3f: &Path, s: &Settings, meta: &Meta) -> Result<(), String> {
    if let Some(loc) = library::store::locate(x3f) {
        return save_to_library(x3f, loc, s, meta);
    }
    let xp = sidecar_path(x3f);
    let mut w = match library::xmp::read(&xp) {
        library::xmp::ReadResult::Ok(w) => w,
        library::xmp::ReadResult::Missing => library::xmp::Values::default(),
        library::xmp::ReadResult::Broken => return Err(crate::trf!("{a} ist nicht lesbar und wird nicht überschrieben", a = xp.display())),
    };
    let old_rating = (w.stars, w.flag, w.label);
    let new_rating = (meta.rating, meta.flag.as_i8(), meta.label);
    if old_rating != new_rating {
        (w.stars, w.flag, w.label) = new_rating;
        w.rating_time = Some(library::now());
        w.has_rating = true;
        library::xmp::write(&xp, &w)?;
    }
    let settings = Settings { view: develop::View::Developed, ..s.clone() };
    let stored = library::xmp::read_develop(&xp)?;
    if settings == baseline(x3f) {
        if stored.is_some() {
            library::xmp::write_develop(&xp, None)?;
        }
        return Ok(());
    }
    if stored.as_deref().and_then(|j| serde_json::from_str::<Settings>(j).ok()).as_ref() == Some(&settings) {
        return Ok(()); // unchanged: do not touch the file
    }
    let json = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
    library::xmp::write_develop(&xp, Some(&json))
}

/// [`save`] for a photo of the library (the main store; in the background, [`library::store`]).
fn save_to_library(x3f: &Path, loc: (i64, bool), s: &Settings, meta: &Meta) -> Result<(), String> {
    let new_rating = (meta.rating, meta.flag.as_i8(), meta.label);
    if library::store::meta(loc.0, None).is_some_and(|m| m != new_rating) {
        library::store::set_meta(loc.0, None, new_rating);
    }
    let settings = Settings { view: develop::View::Developed, ..s.clone() };
    let stored = library::store::develop_at(x3f, Some(loc), None)?;
    // only with the file at hand: an unreachable X3F (NAS offline) has no initial values, and the defaults
    // standing in for them must not delete settings that happen to equal them
    if x3f.is_file() && settings == baseline(x3f) {
        if stored.is_some() {
            library::store::set_develop(loc.0, None, None);
        }
        return Ok(());
    }
    if stored.as_deref().and_then(|j| serde_json::from_str::<Settings>(j).ok()).as_ref() == Some(&settings) {
        return Ok(());
    }
    let json = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
    library::store::set_develop(loc.0, None, Some(json));
    Ok(())
}

fn is_our_xmp(p: &Path) -> bool {
    std::fs::read_to_string(p).is_ok_and(|s| library::is_own_xmp(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Reset" sets Sigma photos to the colour mode "Neutral" (not empty = the camera's mode) and in colour, other
    /// formats keep an empty colour mode; the view stays.
    #[test]
    fn reset_sets_sigma_to_neutral_colour() {
        for name in ["DP2M5418.X3F", "x.x3f"] {
            let s = reset(develop::View::Developed, Path::new(name));
            assert_eq!(s.color_mode.as_deref(), Some(NEUTRAL_MODE), "{name}");
            assert!(!s.monochrome);
            assert_eq!(s.view, develop::View::Developed);
        }
        for name in ["a.CR2", "b.RAF", "c.jpg"] {
            assert_eq!(reset(develop::View::Developed, Path::new(name)).color_mode, None, "{name}");
        }
        // "Neutral" is a colour mode every Merrill/Quattro calibration knows
        assert!(develop::calib::COLOR_MODES.contains(&NEUTRAL_MODE));
    }

    /// SPP initial values: they are read, but only written to the XMP on a change.
    #[test]
    fn spp_settings_are_baseline() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/dp2q/SDIM4388.X3F");
        if !src.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("graineo-sppa-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let x3f = dir.join("SDIM4388.X3F");
        let _ = std::fs::remove_file(&x3f);
        std::os::unix::fs::symlink(&src, &x3f).unwrap();
        let before = std::fs::metadata(&src).unwrap().modified().unwrap();

        let (spp, meta) = load(&x3f);
        assert_eq!(spp.color_mode.as_deref(), Some("Portrait"));
        assert_eq!((spp.nr_luma, spp.nr_chroma), (1.0, 1.0));
        assert!(from_spp(&x3f));
        // saving unchanged → no file
        save(&x3f, &spp, &meta).unwrap();
        assert!(!sidecar_path(&x3f).exists());
        // change → own settings in the XMP, SPP note goes away
        let edited = Settings { exposure: 0.3, ..spp.clone() };
        save(&x3f, &edited, &meta).unwrap();
        assert!(sidecar_path(&x3f).exists());
        assert!(!from_spp(&x3f));
        assert_eq!(load(&x3f).0, edited);
        // a reset to our defaults is kept (entry needed)
        save(&x3f, &Settings::default(), &meta).unwrap();
        assert_eq!(load(&x3f).0, Settings::default());
        // exactly the SPP values again → own settings superfluous
        save(&x3f, &spp, &meta).unwrap();
        assert!(library::xmp::read_develop(&sidecar_path(&x3f)).unwrap().is_none());
        assert!(from_spp(&x3f));

        assert_eq!(std::fs::metadata(&src).unwrap().modified().unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Settings, rating and a foreign XMP: stars and settings end up in `BILD.xmp`, foreign data stays; no JSON.
    #[test]
    fn settings_and_rating_live_in_the_xmp() {
        let dir = std::env::temp_dir().join(format!("graineo-sidecar-xmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let x3f = dir.join("A.X3F");
        std::fs::write(&x3f, b"FOVbkein echtes x3f").unwrap();
        let xp = sidecar_path(&x3f);
        assert_eq!(xp, dir.join("A.xmp"));
        // a foreign XMP (e.g. from Bridge) with its own field
        std::fs::write(&xp, "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" photoshop:City=\"Rom\"/></rdf:RDF></x:xmpmeta>").unwrap();
        let meta = Meta { rating: 3, flag: Flag::Pick, label: 2 };
        let edited = Settings { exposure: 0.4, ..Settings::default() };
        save(&x3f, &edited, &meta).unwrap();
        let t = std::fs::read_to_string(&xp).unwrap();
        assert!(t.contains("photoshop:City=\"Rom\""), "foreign data stays: {t}");
        let (s, m) = load(&x3f);
        assert_eq!((s.exposure, m), (0.4, meta.clone()));
        // saving again unchanged does not touch the file
        let before = std::fs::metadata(&xp).unwrap().modified().unwrap();
        save(&x3f, &edited, &meta).unwrap();
        assert_eq!(std::fs::metadata(&xp).unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read(&x3f).unwrap(), b"FOVbkein echtes x3f");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
