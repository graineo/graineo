//! Binning of the Quattro at high ISO (see `develop::quattro::binning_factor`): the switch per photo in "Details"
//! (noise), the default for new photos in the preferences (setting `binning`) and reopening the photo when the
//! switch changes the image size.
//!
//! A binned photo is a smaller image throughout (session, Sigma's base, display, 100 %, export). Crop, masks,
//! retouching and face frames are stored relative to the image (0 … 1), so switching moves nothing; only the
//! session is opened again with the other size (one new base, computed once).

use develop::{Binning, Calibration, Settings};
use eframe::egui;

use crate::jobs::Loaded;
use crate::lr;
use develop::quattro::BinningCamera;

/// Load the default at start and allow binning in the app.
pub fn init() {
    develop::quattro::enable_binning(true);
}

/// Default for new photos (setting `binning`). Without a stored choice: off (decision of 2026-11-22; until then
/// automatic as in the camera maker's software). Photos with a stored development keep what they have.
pub fn standard() -> Binning {
    standard_from(library::settings::value("binning"))
}

fn standard_from(stored: serde_json::Value) -> Binning {
    if stored.is_null() {
        return Binning::Off;
    }
    serde_json::from_value(stored).unwrap_or(Binning::Off)
}

fn set_standard(b: Binning) {
    library::settings::set("binning", serde_json::to_value(b).unwrap_or_default());
}

/// Settings of a photo without own development: the default binning applies to Quattro files only (other photos keep
/// the neutral value, so their settings stay unchanged).
pub fn apply_standard(path: &std::path::Path, s: &mut Settings) {
    if develop::quattro::is_prepared(path) {
        s.binning = standard();
    }
}

/// Neutral value of the switch for this photo (reset of the section): the default for cameras with binning.
pub fn default_for(cal: &Calibration) -> Binning {
    if develop::quattro::binning_camera(cal).is_some() { standard() } else { Binning::Auto }
}

/// Why the noise sliders (luminance, colour) of a Quattro photo are greyed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuattroNoiseLock {
    /// below capture ISO 400 (or a multi-shot file): the noise reduction of the preparation is fixed
    LowIso,
    /// the full chain is switched off (`GRAINEO_QUATTRO_FULL=0`)
    ChainOff,
}

/// Noise sliders (luminance, colour) greyed out? Only for Quattro: from capture ISO 400 on the full chain evaluates
/// them (five steps, the middle is the camera's setting, see `develop::quattro::noise_sliders_adjustable`). Below
/// that the noise reduction runs in the preparation (x3fuse-core) and `denoise::denoise` is a no-op (no noise model),
/// so the sliders could only switch that preparation step off. `None` = adjustable, or not a Quattro photo.
pub fn noise_sliders_locked(cal: &Calibration, path: &std::path::Path) -> Option<QuattroNoiseLock> {
    if !cal.model.is_quattro() || develop::quattro::noise_sliders_adjustable(cal, path) {
        return None;
    }
    Some(if develop::quattro::full_chain_enabled() { QuattroNoiseLock::LowIso } else { QuattroNoiseLock::ChainOff })
}

/// Capture ISO from which the older Foveon cameras (Classic) have an adjustable noise reduction: below it their
/// processing has no noise reduction the sliders could change (the maker's software greys them out there as well).
pub const CLASSIC_NOISE_MIN_ISO: f64 = 400.0;

/// Noise sliders greyed out because a Classic photo was taken below [`CLASSIC_NOISE_MIN_ISO`]?
pub fn classic_noise_locked(model: develop::calib::Model, iso: f64) -> bool {
    model == develop::calib::Model::Classic && iso < CLASSIC_NOISE_MIN_ISO
}

/// Tooltip of the noise sliders greyed out by [`classic_noise_locked`].
pub const CLASSIC_NOISE_TIP: &str = "Bei ISO unter 400 hat diese Kamera keine einstellbare Rauschminderung.";

/// Tooltip of the sliders greyed out by [`noise_sliders_locked`]; `away` = stored values differ from the middle.
/// Below ISO 400 such values switch the preparation's noise reduction off (as before); with the full chain off the
/// previous high-ISO path still applies them.
pub fn noise_locked_tip(lock: QuattroNoiseLock, away: bool) -> &'static str {
    match (lock, away) {
        (QuattroNoiseLock::LowIso, false) => "Unter ISO 400 läuft die Rauschminderung der Quattro fest in der Aufbereitung.",
        (QuattroNoiseLock::LowIso, true) => "Unter ISO 400 läuft die Rauschminderung der Quattro fest in der Aufbereitung; die gespeicherten Werte schalten sie ab.",
        (QuattroNoiseLock::ChainOff, false) => "Der volle Quattro-Weg ist ausgeschaltet; nur er wertet diese Regler aus.",
        (QuattroNoiseLock::ChainOff, true) => "Der volle Quattro-Weg ist ausgeschaltet; die gespeicherten Werte gelten weiter.",
    }
}

/// Tooltip of the greyed-out Foveon sharpness: it needs the detail image of the Merrill base (`prepared`: a Quattro
/// or Classic photo, which has none), and the Foveon detail switched on.
pub fn sigma_sharpness_locked_tip(prepared: bool) -> &'static str {
    if prepared {
        "Die Foveon-Schärfe gibt es nur für Merrill-Fotos. Hier schärft „Zusätzlich schärfen“; bei der Quattro ab ISO 400 auch „Detail“."
    } else {
        "Die Foveon-Schärfe wirkt nur mit eingeschaltetem Foveon-Detail."
    }
}

/// Tooltip of the Quattro detail slider.
pub const DETAIL_TIP: &str = "Wie viel Detail die Rauschminderung der Quattro ab ISO 400 stehen lässt: sanft (−3) bis stark (+3), 0 = Einstellung der Kamera. Feindetail und Struktur wirken danach auf das Ergebnis";

/// Tooltip of the detail slider greyed out for a Quattro photo (`multi_shot`: an X3I file).
pub fn detail_locked_tip(lock: QuattroNoiseLock, multi_shot: bool) -> &'static str {
    match (lock, multi_shot) {
        (QuattroNoiseLock::ChainOff, _) => "Der volle Quattro-Weg ist ausgeschaltet; nur er wertet den Detail-Regler aus.",
        (QuattroNoiseLock::LowIso, true) => "Bei Mehrfachaufnahmen (X3I) ist das Detail fest. Mehr Schärfe gibt „Zusätzlich schärfen“ oben.",
        (QuattroNoiseLock::LowIso, false) => "Unter ISO 400 ist das Detail der Quattro fest. Mehr Schärfe gibt „Zusätzlich schärfen“ oben.",
    }
}

/// Detail slider of the Quattro below the noise sliders (seven steps, the middle is the camera's setting; acts in the
/// full chain from capture ISO 400 on, `develop::Settings::quattro_detail`). Only shown for Quattro photos; greyed
/// out with the reason where the chain does not run. Returns: changed.
pub fn detail_slider(ui: &mut egui::Ui, cal: &Calibration, path: &std::path::Path, s: &mut Settings) -> bool {
    if !cal.model.is_quattro() {
        return false;
    }
    let locked = noise_sliders_locked(cal, path);
    let range = f64::from(develop::settings::QUATTRO_DETAIL_RANGE);
    let mut v = f64::from(s.quattro_detail);
    let row = ui.add_enabled_ui(s.denoise && locked.is_none(), |ui| lr::slider_hint(ui, "Detail", &mut v, -range..=range, 0.0, 1.0, DETAIL_TIP));
    if let Some(lock) = locked {
        let r = ui.interact(row.response.rect, ui.id().with("detail-locked"), egui::Sense::hover());
        lr::hover_tip(ui, &r, detail_locked_tip(lock, x3f::x3i::is_x3i(path)));
    }
    let step = v.round() as i8;
    if row.inner && step != s.quattro_detail {
        s.quattro_detail = step;
        return true;
    }
    false
}

/// Is the photo edited ("edited" mark in filmstrip and grid)? Like `s != Settings::default()`, but the binning counts
/// as unedited while it is the default for new photos ([`standard`]), so an untouched Quattro photo with the default
/// "Aus" carries no mark.
pub fn edited(s: &Settings) -> bool {
    edited_with(s, || standard())
}

fn edited_with(s: &Settings, standard: impl FnOnce() -> Binning) -> bool {
    if s.binning == Binning::Auto || s.binning != standard() {
        return *s != Settings::default();
    }
    Settings { binning: Binning::Auto, ..s.clone() } != Settings::default()
}

/// Preferences → Develop: default for new photos (right side of a row).
pub fn prefs_switch(ui: &mut egui::Ui) {
    let mut b = if standard() == Binning::Off { Binning::Off } else { Binning::Auto };
    if lr::segmented(ui, &mut b, &[(Binning::Auto, "Auto"), (Binning::Off, "Aus")]) {
        set_standard(b);
    }
}

/// Does the loaded session have the binning the settings call for? (Always for non-Sigma photos.)
pub fn fits(loaded: &Loaded, s: &Settings) -> bool {
    loaded.session.is_none() || develop::quattro::binning_camera(&loaded.cal).is_none() || loaded.cal.binning == develop::quattro::binning_for(&loaded.cal, s)
}

/// Size of the image at binning factor `n` (`cal` may belong to a binned session).
pub fn size_at(cal: &Calibration, n: usize) -> [usize; 2] {
    let [w, h] = cal.full_size();
    [w / n.max(1), h / n.max(1)]
}

/// Switch per photo in "Details" below the noise sliders, only for cameras with binning: sd Quattro (H) "Auto | Aus",
/// dp Quattro "Aus | An · graineo" (the camera maker's software never bins the dp Quattro; "Aus" is the automatic
/// setting there). Below ISO 800 it stays visible with the state "no effect". Returns: changed.
pub fn switch(ui: &mut egui::Ui, cal: &Calibration, s: &mut Settings) -> bool {
    let Some(camera) = develop::quattro::binning_camera(cal) else { return false };
    let iso = develop::quattro::binning_iso_factor(cal);
    let n = develop::quattro::binning_for(cal, s);
    let state = if iso <= 1 {
        crate::tr("ohne Wirkung unter ISO 800").to_string()
    } else if n > 1 {
        let [w, h] = size_at(cal, n);
        format!("×{n} · {w} × {h}")
    } else {
        let [w, h] = size_at(cal, 1);
        format!("{w} × {h}")
    };
    let mut v = match (camera, s.binning) {
        (BinningCamera::Sd, Binning::Off) => Binning::Off,
        (BinningCamera::Sd, _) => Binning::Auto,
        (BinningCamera::Dp, Binning::On) => Binning::On,
        (BinningCamera::Dp, _) => Binning::Auto,
    };
    let options: &[(Binning, &str)] = match camera {
        BinningCamera::Sd => &[(Binning::Auto, "Auto"), (Binning::Off, "Aus")],
        BinningCamera::Dp => &[(Binning::Auto, "Aus"), (Binning::On, "An · graineo")],
    };
    ui.add_space(4.0);
    if !lr::choice_state(ui, "Binning", &state, &mut v, options, "") {
        return false;
    }
    s.binning = v;
    true
}

/// Label of the binning state for the image info ("1356 × 904 · Binning ×4").
pub fn info_size(loaded: &Loaded) -> String {
    let [w, h] = loaded.full_size;
    match loaded.cal.binning {
        n if n > 1 && loaded.session.is_some() => crate::trf!("{w} × {h} · Binning ×{n}", w = w, h = h, n = n),
        _ => format!("{w} × {h}"),
    }
}

impl crate::App {
    /// Each frame: if the binning of the photo on screen changed (switch, paste, undo), open it again with the other
    /// size. Nothing is moved: all geometry is stored relative to the image.
    pub(crate) fn binning_check(&mut self) {
        if self.loading.is_some() {
            return;
        }
        self.binning_test_hook();
        let Some(c) = self.current.as_ref() else { return };
        let Some(e) = self.entries.get(c.index) else { return };
        if fits(&c.loaded, &e.settings) {
            return;
        }
        let index = c.index;
        // the picture on screen stays until the photo is rendered in its new size (no thumbnail in between, never
        // the rough fast preview, see `preview_hold.rs`)
        let kept = c.texture.clone().map(|t| (e.path.clone(), t));
        crate::log(&format!("binning {index}: ×{} → ×{}, opened again", c.loaded.cal.binning, develop::quattro::binning_for(&c.loaded.cal, &e.settings)));
        self.save_current();
        self.cache.retain(|(i, _)| *i != index);
        // the old session (another size) is dropped; jobs still holding it finish and are discarded by size
        self.current = None;
        self.select(index, true);
        self.kept_view = kept;
    }

    /// Test hook (only with `GRAINEO_DATA_DIR`): `GRAINEO_BINNING_TEST="off@8"` (or `auto@…`, `on@…`) switches the
    /// binning of the photo on screen once after that many seconds, as the switch in "Details" does.
    fn binning_test_hook(&mut self) {
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        if DONE.load(std::sync::atomic::Ordering::Relaxed) || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let Ok(spec) = crate::env::var("GRAINEO_BINNING_TEST") else { return };
        let Some((what, at)) = spec.split_once('@') else { return };
        let at = at.parse::<f64>().unwrap_or(f64::MAX);
        let since = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64();
        if since < at {
            self.worker.ctx().request_repaint_after(std::time::Duration::from_secs_f64((at - since).max(0.05)));
            return;
        }
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        let b = match what { "off" => Binning::Off, "on" => Binning::On, _ => Binning::Auto };
        crate::log(&format!("binning test: {index} → {b:?}"));
        self.entries[index].settings.binning = b;
    }
}

// ── Before/after: the same photo with the other binning ──

/// Settings of the other binning for the before/after comparison and its label ("Ohne Binning" / "Binning ×4"); `None`
/// when switching changes nothing for this photo (other camera, below ISO 800, non-Sigma photo).
pub fn alt_settings(loaded: &Loaded, s: &Settings) -> Option<(Settings, String)> {
    let camera = develop::quattro::binning_camera(&loaded.cal)?;
    if loaded.session.is_none() || develop::quattro::binning_iso_factor(&loaded.cal) <= 1 {
        return None;
    }
    let other = if develop::quattro::binning_for(&loaded.cal, s) > 1 {
        Binning::Off
    } else if camera == BinningCamera::Sd {
        Binning::Auto
    } else {
        Binning::On
    };
    let t = Settings { binning: other, ..s.clone() };
    let n = develop::quattro::binning_for(&loaded.cal, &t);
    let label = if n > 1 { crate::trf!("Binning ×{n}", n = n) } else { crate::tr("Ohne Binning").to_string() };
    Some((t, label))
}

/// A second session of the photo in the other size (only while the comparison shows it).
#[derive(Clone)]
pub struct AltView {
    pub previews: crate::jobs::Previews,
    pub cal: std::sync::Arc<Calibration>,
    pub session: std::sync::Arc<std::sync::Mutex<develop::Session>>,
    pub full_size: [usize; 2],
}

type AltKey = (std::path::PathBuf, usize, develop::SppKey, bool);

type AltSlot = std::sync::Arc<std::sync::Mutex<Option<Result<AltView, String>>>>;

/// The other-binning session of the comparison: opened once per photo and factor in a background thread (Sigma's
/// base from the disk cache or computed once), released when the comparison no longer shows it.
#[derive(Default)]
pub struct Old {
    /// (photo, factor, base of Sigma's noise reduction / colour mode / method, SPP base in use) and the result once there
    key: Option<AltKey>,
    slot: Option<AltSlot>,
}

impl Old {
    /// The session for `path` with the settings `s` (another binning than on screen): `Some(Ok)` when ready,
    /// `None` while it is being opened (started on the first call), `Some(Err)` when it failed.
    pub fn get(&mut self, ctx: &egui::Context, path: &std::path::Path, cal: &Calibration, s: &Settings) -> Option<Result<AltView, String>> {
        let key = (path.to_path_buf(), develop::quattro::binning_for(cal, s), s.spp_key(path), develop::Session::uses_spp(s));
        if self.key.as_ref() != Some(&key) {
            let slot: AltSlot = Default::default();
            self.key = Some(key);
            self.slot = Some(slot.clone());
            let (path, s, ctx) = (path.to_path_buf(), s.clone(), ctx.clone());
            let _ = std::thread::Builder::new().name("binning-compare".into()).spawn(move || {
                let t = std::time::Instant::now();
                let r = open_alt(&path, &s);
                crate::log(&format!("binning comparison: other size opened in {:.2} s ({})", t.elapsed().as_secs_f32(), r.as_ref().map_or_else(|e| e.clone(), |a| format!("{} × {}", a.full_size[0], a.full_size[1]))));
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                ctx.request_repaint();
            });
        }
        self.slot.as_ref()?.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Did a job with this origin work on the second session of photo `path` (its results belong to the comparison)?
    pub fn is_origin(&self, o: &crate::jobs::Origin, path: &std::path::Path) -> bool {
        if self.key.as_ref().is_none_or(|(p, ..)| p != path) {
            return false;
        }
        let Some(slot) = &self.slot else { return false };
        let g = slot.lock().unwrap_or_else(|e| e.into_inner());
        matches!(&*g, Some(Ok(a)) if crate::jobs::is_origin_of(o, &a.previews, &a.session))
    }

    /// Release the second session (comparison ended, other source, other photo).
    pub fn clear(&mut self) {
        self.key = None;
        self.slot = None;
    }
}

fn open_alt(path: &std::path::Path, s: &Settings) -> Result<AltView, String> {
    let session = develop::Session::open_for(path, s).map_err(|e| e.to_string())?;
    let mut previews = crate::jobs::previews(&session, s);
    if develop::Session::uses_spp(s) {
        let base = session.ensure_spp(s)?;
        let m = base.matrix(&session.cal);
        previews.spp = Some(std::sync::Arc::new(develop::session::Preview::from_spp(&base, m, crate::jobs::PREVIEW_EDGE)));
    }
    let (cal, full_size) = (session.cal.clone(), session.size());
    Ok(AltView { previews, cal, session: std::sync::Arc::new(std::sync::Mutex::new(session)), full_size })
}

/// Region `[x, y, w, h]` of the image on screen in pixels of the other binning size (`scale` = its pixels per pixel on
/// screen), rounded outwards and limited to `size`.
pub fn scale_region(r: [usize; 4], scale: [f32; 2], size: [usize; 2]) -> [usize; 4] {
    let x0 = ((r[0] as f32 * scale[0]).floor() as usize).min(size[0].saturating_sub(1));
    let y0 = ((r[1] as f32 * scale[1]).floor() as usize).min(size[1].saturating_sub(1));
    let x1 = (((r[0] + r[2]) as f32 * scale[0]).ceil() as usize).clamp(x0 + 1, size[0]);
    let y1 = (((r[1] + r[3]) as f32 * scale[1]).ceil() as usize).clamp(y0 + 1, size[1]);
    [x0, y0, x1 - x0, y1 - y0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_sliders_lock_rules() {
        use develop::calib::Model;
        // Quattro: adjustable from capture ISO 400 on with the full chain, never below, without it or for X3I
        let rule = develop::quattro::noise_sliders_rule;
        assert!(rule(Model::DpQuattro, 1600.0, true, false) && rule(Model::SdQuattro, 400.0, true, false));
        assert!(!rule(Model::DpQuattro, 100.0, true, false) && !rule(Model::SdQuattro, 320.0, true, false));
        assert!(!rule(Model::DpQuattro, 1600.0, false, false) && !rule(Model::DpQuattro, 1600.0, true, true));
        assert!(!rule(Model::Dp, 1600.0, true, false) && !rule(Model::Classic, 1600.0, true, false));
        assert!(classic_noise_locked(Model::Classic, 100.0) && classic_noise_locked(Model::Classic, 200.0));
        assert!(!classic_noise_locked(Model::Classic, 400.0) && !classic_noise_locked(Model::Classic, 1600.0));
        assert!(!classic_noise_locked(Model::Dp, 100.0) && !classic_noise_locked(Model::DpQuattro, 100.0));
    }

    #[test]
    fn without_a_stored_choice_the_default_is_off() {
        assert_eq!(standard_from(serde_json::Value::Null), Binning::Off);
        assert_eq!(standard_from(serde_json::json!("auto")), Binning::Auto, "a stored choice stays");
        assert_eq!(standard_from(serde_json::json!("off")), Binning::Off);
        assert_eq!(standard_from(serde_json::json!(17)), Binning::Off);
    }

    #[test]
    fn default_binning_is_not_an_edit() {
        let off = Settings { binning: Binning::Off, ..Settings::default() };
        // default "Aus": an untouched photo with binning Off is not edited, any other change is
        assert!(!edited_with(&off, || Binning::Off));
        assert!(edited_with(&Settings { exposure: 0.3, ..off.clone() }, || Binning::Off));
        // default Auto: Off is an edit, Auto is not
        assert!(edited_with(&off, || Binning::Auto));
        assert!(!edited_with(&Settings::default(), || Binning::Off));
        assert!(edited_with(&Settings { binning: Binning::On, ..Settings::default() }, || Binning::Off));
    }

    #[test]
    fn region_scales_outwards_and_stays_inside() {
        // binned 1356 × 904 on screen, full 5424 × 3616 in the comparison
        assert_eq!(scale_region([100, 50, 200, 100], [4.0, 4.0], [5424, 3616]), [400, 200, 800, 400]);
        // full on screen, binned ×3 in the comparison: outwards, inside the image
        assert_eq!(scale_region([10, 10, 20, 20], [1.0 / 3.0, 1.0 / 3.0], [1808, 1205]), [3, 3, 7, 7]);
        assert_eq!(scale_region([5400, 3600, 24, 16], [0.25, 0.25], [1356, 904]), [1350, 900, 6, 4]);
    }
}
