//! When the fast preview of a Sigma photo is withheld.
//!
//! A Sigma photo opens in two steps: a fast preview from the own quick pipeline (block means of the prepared sensor
//! data), then the exact base, which takes seconds. At high ISO the fast preview is no picture to look at:
//!
//! - Quattro from capture ISO [`ROUGH_MIN_ISO`] on: the whole noise reduction and the repair of single cells belong
//!   to the exact base (below that ISO the preparation already denoises), so the fast preview shows heavy colour
//!   noise over the whole frame and the regular pattern of the unrepaired cells.
//! - Merrill (DP Merrill, SD1) from capture ISO [`MERRILL_ROUGH_MIN_ISO`] on: the fast preview has no noise
//!   reduction at all, its colour noise grows in proportion to the ISO. Measured on 30 files (RMS of the chroma
//!   against its 5 × 5 mean in the 2.8 MP preview, 8 bit; `jobs::gate_tests::measure_fast_preview_roughness`):
//!   ISO 100–400 1–10 (exact view 1–9), ISO 800 13–16 (exact 2–3; still a picture, as the Quattro at ISO 250
//!   with 15), ISO 1600 28–34 (exact 2–3; colour blotches over the whole frame, the level of a Quattro at ISO 800
//!   with 32, which is withheld), ISO 3200 60–93, ISO 6400 102–117 (exact 10–13; a coloured noise picture).
//!
//! Such a preview is never shown. The older cameras open with the exact view anyway; Quattro and Merrill below
//! their limits keep the quick first picture (it differs from the exact view only in nuances). Until the exact
//! base is there, the picture on screen stays: the stored preview of the library or the camera's embedded picture while the photo opens, the last
//! finished picture after a change that needs another base (binning, colour ↔ black & white), each with the spinner.
//! Sliders that are converted from the base on screen (tone, colour, noise steps) stay live: they render from that
//! base, never from the rough data.
//!
//! The same holds for every picture derived from the opened photo outside the main view (fields of the photo
//! comparison and the tournament, panes of the look comparison, film and preset tiles, hover previews, contact rows
//! and test strips of the darkroom): [`waits`] says whether such a picture must wait for the base.
//!
//! The fast preview comes back only when the exact base cannot be had (failed, or given up after the watchdog's
//! retries): then it is the best picture there is.

/// Capture ISO from which the fast preview of a Quattro photo is too rough to be shown (from here on the noise
/// reduction is part of the exact base, see `develop::quattro::noise_sliders_rule`).
pub(crate) const ROUGH_MIN_ISO: f64 = 400.0;

/// Capture ISO from which the fast preview of a Merrill photo (DP Merrill, SD1) is too rough to be shown.
pub(crate) const MERRILL_ROUGH_MIN_ISO: f64 = 1600.0;

/// Switch for comparisons: `GRAINEO_PREVIEW_HOLD=0` shows the fast preview at every ISO as before.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::env::var("GRAINEO_PREVIEW_HOLD").map_or(true, |v| v != "0"))
}

/// The state of the photo on screen the decision is made from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct State {
    /// the camera
    pub model: develop::calib::Model,
    /// capture ISO
    pub iso: f64,
    /// the developed view with the exact base in use (not the sensor views, not with the noise reduction off)
    pub wants_base: bool,
    /// an exact base that can stand in for the settings is there (the render starts from it)
    pub has_base: bool,
    /// the exact base failed or was given up for this photo
    pub gave_up: bool,
}

/// Is the fast preview of a photo of this camera and capture ISO too rough to be shown?
pub(crate) fn rough(model: develop::calib::Model, iso: f64) -> bool {
    use develop::calib::Model;
    match model {
        Model::DpQuattro | Model::SdQuattro => iso >= ROUGH_MIN_ISO,
        Model::Dp | Model::Sd1 => iso >= MERRILL_ROUGH_MIN_ISO,
        // the exact view is ready when the photo opens
        Model::Classic => false,
    }
}

/// Capture ISO of a Sigma photo.
pub(crate) fn capture_iso(cal: &develop::Calibration) -> f64 {
    if cal.model.is_quattro() { cal.iso_gain * cal.sensor_iso } else { cal.iso }
}

/// [`rough`] from the camera name and ISO of the library (for photos not opened), `false` for unknown names.
pub(crate) fn rough_by_name(camera: &str, iso: f64) -> bool {
    let name = camera.to_lowercase();
    if name.contains("quattro") {
        iso >= ROUGH_MIN_ISO
    } else if name.contains("merrill") || name.split_whitespace().any(|w| w == "sd1") {
        iso >= MERRILL_ROUGH_MIN_ISO
    } else {
        false
    }
}

/// [`rough`] for the camera data of a Sigma photo.
pub(crate) fn rough_for(cal: &develop::Calibration) -> bool {
    rough(cal.model, capture_iso(cal))
}

/// Withhold the render from the fast preview (the picture on screen stays until the exact base is there)?
pub(crate) fn hold(s: State) -> bool {
    s.wants_base && !s.has_base && !s.gave_up && rough(s.model, s.iso)
}

/// May a picture rendered in this state be kept (grid tile, developed preview, placeholder for the next opening)?
/// Never one made from a fast preview too rough to be shown – also not when the exact base failed or was given up
/// and the fast preview is what Develop shows: the tile then keeps what it had (the camera's picture).
pub(crate) fn keepable(s: State) -> bool {
    !(s.wants_base && !s.has_base && rough(s.model, s.iso))
}

/// [`keepable`] for a render of an opened photo with these settings (other formats: always).
pub(crate) fn render_keepable(loaded: &crate::jobs::Loaded, s: &develop::Settings) -> bool {
    loaded.session.is_none() || loaded.previews.foreign.is_some() || keepable(state(&loaded.previews, &loaded.cal, s, false))
}

/// [`State`] of an opened Sigma photo with these settings.
pub(crate) fn state(previews: &crate::jobs::Previews, cal: &develop::Calibration, s: &develop::Settings, gave_up: bool) -> State {
    State {
        model: cal.model,
        iso: capture_iso(cal),
        wants_base: s.view == develop::View::Developed && develop::Session::uses_spp(s),
        has_base: previews.spp.as_ref().is_some_and(|p| p.spp.as_ref().is_none_or(|(k, _)| crate::spp_accept::stands_in(k, &s.spp_key_class(k.prepared)))),
        gave_up,
    }
}

/// Must a picture rendered from these previews with these settings wait for the exact base (the fast preview is too
/// rough to be shown and no base that can stand in is there)? Never for other formats.
pub(crate) fn waits(previews: &crate::jobs::Previews, cal: &develop::Calibration, s: &develop::Settings) -> bool {
    enabled() && previews.foreign.is_none() && hold(state(previews, cal, s, false))
}

/// [`waits`] for an opened photo.
pub(crate) fn waits_for_base(loaded: &crate::jobs::Loaded, s: &develop::Settings) -> bool {
    loaded.session.is_some() && waits(&loaded.previews, &loaded.cal, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use develop::calib::Model;

    const OPENING: State = State { model: Model::DpQuattro, iso: 6400.0, wants_base: true, has_base: false, gave_up: false };

    /// The case reported: a Quattro photo at ISO 6400 opens, the base is still being computed.
    #[test]
    fn rough_preview_is_withheld_until_the_base_is_there() {
        assert!(hold(OPENING));
        assert!(hold(State { iso: 400.0, ..OPENING }), "from ISO 400 on");
        assert!(hold(State { model: Model::SdQuattro, ..OPENING }));
        // the base arrived: rendering from it, tone and colour sliders stay live
        assert!(!hold(State { has_base: true, ..OPENING }));
    }

    /// Merrill at high ISO (QA 9: DP1 Merrill and DP3 Merrill at ISO 6400 opened with a coloured noise picture).
    #[test]
    fn merrill_is_withheld_from_iso_1600() {
        for model in [Model::Dp, Model::Sd1] {
            assert!(hold(State { model, ..OPENING }), "ISO 6400");
            assert!(hold(State { model, iso: 1600.0, ..OPENING }));
            assert!(!hold(State { model, iso: 800.0, ..OPENING }), "ISO 800 keeps the quick first picture");
            assert!(!hold(State { model, iso: 100.0, ..OPENING }));
        }
    }

    /// The rule from the library's camera names matches the rule from the camera data.
    #[test]
    fn rough_by_name_follows_the_cameras() {
        assert!(rough_by_name("SIGMA dp2 Quattro", 6400.0) && rough_by_name("sd Quattro H", 400.0));
        assert!(!rough_by_name("sd Quattro H", 250.0));
        assert!(rough_by_name("SIGMA DP1 Merrill", 1600.0) && rough_by_name("SIGMA SD1", 3200.0));
        assert!(!rough_by_name("SIGMA DP2 Merrill", 800.0));
        assert!(!rough_by_name("SIGMA SD15", 3200.0) && !rough_by_name("SIGMA DP1", 800.0) && !rough_by_name("", 6400.0));
    }

    /// Low ISO and the older cameras keep the quick first picture.
    #[test]
    fn low_iso_and_older_cameras_keep_the_fast_preview() {
        assert!(!hold(State { iso: 100.0, ..OPENING }));
        assert!(!hold(State { iso: 320.0, ..OPENING }));
        assert!(!hold(State { model: Model::Classic, ..OPENING }), "the exact view is there when the photo opens");
    }

    /// A stored tile or developed preview never comes from the rough fast preview: not while the base is on its way,
    /// not after it failed or was given up (Develop shows the fast preview then, the tile keeps the camera's picture),
    /// and not with the comparison switch that shows the fast preview at once.
    #[test]
    fn a_picture_from_the_rough_preview_is_never_kept() {
        assert!(!keepable(OPENING), "Quattro ISO 6400 without its base");
        assert!(!keepable(State { gave_up: true, ..OPENING }), "base failed or given up");
        assert!(!keepable(State { model: Model::Dp, iso: 1600.0, ..OPENING }), "Merrill ISO 1600");
        // rendered from the exact base: kept
        assert!(keepable(State { has_base: true, ..OPENING }));
        // not rough, or the own pipeline was asked for (noise reduction off, sensor views): kept as before
        assert!(keepable(State { iso: 100.0, ..OPENING }));
        assert!(keepable(State { model: Model::Dp, iso: 800.0, ..OPENING }));
        assert!(keepable(State { model: Model::Classic, ..OPENING }));
        assert!(keepable(State { wants_base: false, ..OPENING }));
    }

    /// Without a base to wait for, the fast preview is the picture: noise reduction off, sensor views, a base that
    /// failed or was given up.
    #[test]
    fn nothing_is_withheld_without_a_base_to_wait_for() {
        assert!(!hold(State { wants_base: false, ..OPENING }));
        assert!(!hold(State { gave_up: true, ..OPENING }));
    }
}
