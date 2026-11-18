//! Non-Sigma RAWs (CR2/CR3/RAF/DNG/ARW …) in the Develop module: opening (placeholders for the
//! Sigma fields of [`Loaded`]), sidecar (XMP with `graineo:DevelopSettings`, virtual
//! copies), initial values from Lightroom edits, profile thumbnails and the controls that exist only for
//! these formats. The computation itself lives in `crates/rawdev`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use develop::session::Preview;
use develop::Settings;
use eframe::egui;
use egui::Color32;
use rawdev::RawDoc;

use crate::jobs::{Loaded, Previews};
use crate::lr;
use crate::sidecar::{Flag, Meta};

/// EXIF orientation → quarter turns clockwise.
pub fn layout(o: u16) -> u8 {
    match o {
        6 | 5 => 1,
        3 | 4 => 2,
        8 | 7 => 3,
        _ => 0,
    }
}

/// Calibration as a placeholder (only position and size matter; the Sigma parts stay empty).
fn placeholder_cal(doc: &RawDoc) -> develop::Calibration {
    let [w, h] = doc.full_size;
    develop::Calibration {
        camera: format!("{} {}", doc.info.maker, doc.info.model),
        model: develop::calib::Model::Dp,
        count_scale: 1.0,
        pixel_saturation: 4095.0,
        output_to_srgb: None,
        wb_name: "As Shot".into(),
        wb_presets: Vec::new(),
        iso_gain: 1.0,
        sigma_wb_ev: Vec::new(),
        pending_sharpen: 0.0,
        sensor_iso: 0.0,
        camera_mode: String::new(),
        modes: Vec::new(),
        wb_gain: [1.0; 3],
        color_dq: None,
        black: [0.0; 3],
        saturation: 1.0,
        active: [0, 0, w.max(1) - 1, h.max(1) - 1],
        spatial: None,
        shading: None,
        bad_pixels: Vec::new(),
        badness_threshold: 0,
        noise_model: None,
        iso_noise_settings: Vec::new(),
        denoise_slider_steps: Vec::new(),
        residual_epsilon: 0.0,
        edge_lin_shift: 0.0,
        iso: doc.info.exif.iso.unwrap_or(100) as f64,
        orientation: doc.orientation,
        binning: 1,
        unbinned: None,
    }
}

/// RAWs opened from a working copy (Apple Photos: original only in iCloud): path of the copy → path of the photo
/// in the list (initial values, sidecar and list path belong to it).
static RAW_OPENING: Mutex<Vec<(std::path::PathBuf, std::path::PathBuf)>> = Mutex::new(Vec::new());

/// Before opening a RAW from a working copy: which photo it belongs to.
pub fn register_raw(copy: &Path, photo: &Path) {
    let mut v = RAW_OPENING.lock().unwrap();
    v.retain(|(c, _)| c != copy);
    v.push((copy.to_path_buf(), photo.to_path_buf()));
    if v.len() > 64 {
        v.remove(0);
    }
}

/// Opening for the app (background thread).
pub fn load(path: &Path, settings: &Settings, keep_going: &dyn Fn() -> bool) -> Result<Option<Loaded>, String> {
    let t0 = std::time::Instant::now();
    // Test hook (only with GRAINEO_DATA_DIR): GRAINEO_TEST_OPEN_DELAY_MS simulates a slow drive (e.g. the NAS)
    if let Some(ms) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_TEST_OPEN_DELAY_MS").ok()).and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    // engine of the raw stage from the settings (graineo, Apple, automatic fallback, see `rawdev::apple`)
    let Some(mut doc) = RawDoc::open_for(path, settings, keep_going)? else { return Ok(None) };
    let t_open = t0.elapsed();
    // from a working copy: initial values, XMP and list path belong to the photo
    let raw_entry = RAW_OPENING.lock().unwrap().iter().find(|(c, _)| c == path).cloned();
    if let Some((_, photo)) = &raw_entry {
        doc.logical_path = photo.clone();
    }
    // Orientation as in common photo apps (also rotated by hand), before initial values and crop are computed
    rawdev::lightroom::apply_orientation(&mut doc);
    let t_orient = t0.elapsed();
    let v = &doc.preview;
    let data: Vec<f32> = v.px.iter().flat_map(|p| *p).collect();
    let lin = develop::Linear { mono: false, haze: None, toneq: None, width: v.w, height: v.h, data, detail: None };
    let factor = (doc.full_size[0] as f32 / v.w as f32).round().max(1.0) as usize;
    let histograms = develop::layer_histograms(&lin, 256);
    let plain = Arc::new(Preview::plain(lin, factor));
    let t_hist = t0.elapsed();
    let lr_start = lightroom_start(&doc);
    let t_lr = t0.elapsed();
    let cal = Arc::new(placeholder_cal(&doc));
    // Compute noise reduction/lens correction of the preview right away (also when preloading the
    // neighbouring image) so that the first slider state appears immediately
    let stage_for = match (&lr_start, *settings == Settings::default()) {
        (Some(l), true) => l,
        _ => settings,
    };
    if keep_going() {
        let st = doc.preview_stage(stage_for);
        let _ = st.mask(doc.lum());
    }
    if crate::env::var_os("GRAINEO_LOG").is_some() {
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "raw load {}: open {:.0} · orientation {:.0} · histograms {:.0} · lightroom {:.0} · stage {:.0} ms",
            path.display(),
            ms(t_open),
            ms(t_orient - t_open),
            ms(t_hist - t_orient),
            ms(t_lr - t_hist),
            ms(t0.elapsed() - t_lr)
        );
    }
    let doc = Arc::new(doc);
    let previews = Previews {
        developed: plain.clone(),
        sensor: plain,
        denoise_key: settings.denoise_key(),
        spp: None,
        raw: Arc::new(x3f::RawImage { columns: 0, rows: 0, planes: [Vec::new(), Vec::new(), Vec::new()] }),
        foreign: Some(doc.clone()),
    };
    let summary = doc.summary.clone();
    Ok(Some(Loaded {
        session: None,
        cal,
        previews,
        histograms,
        info: doc.info_rows.clone(),
        full_size: doc.full_size,
        camera_mode: String::new(),
        saturation: 1.0,
        summary,
        start_values_from_spp: false,
        lr_start,
        prerendered: None,
        path: raw_entry.map_or_else(|| path.to_path_buf(), |(_, photo)| photo),
        kind: crate::image_kind::of(path),
        damage: None,
    }))
}

/// Initial values from Lightroom: the settings taken over into the sidecar, otherwise its `crs:` fields (never the catalog).
pub fn lightroom_start(doc: &RawDoc) -> Option<Settings> {
    rawdev::lightroom::initial_settings(doc)
}

// ---------------------------------------------------------------------------------------------
// Sidecar

/// Sidecar: `NAME.xmp`; a JPEG/HEIC/TIFF next to a RAW with the same name gets
/// `NAME.JPG.xmp` (as in the library, [`library::xmp::path`]).
fn xmp_path(p: &Path) -> std::path::PathBuf {
    library::xmp::path_to(p)
}

/// Initial values per image (remembered on load): only what differs from them is saved.
static INITIAL_VALUES: Mutex<Vec<(std::path::PathBuf, Option<String>, Settings)>> = Mutex::new(Vec::new());

fn remember_start(p: &Path, copy_name: Option<&str>, s: &Settings) {
    let mut v = INITIAL_VALUES.lock().unwrap();
    v.retain(|(q, k, _)| !(q == p && k.as_deref() == copy_name));
    v.push((p.to_path_buf(), copy_name.map(str::to_string), s.clone()));
    if v.len() > 4096 {
        v.drain(..1024);
    }
}

/// Apply initial values (e.g. from Lightroom) if the image has no own settings yet.
pub fn set_start_values(p: &Path, copy_name: Option<&str>, s: &Settings) {
    remember_start(p, copy_name, s);
}

pub fn start_value(p: &Path, copy_name: Option<&str>) -> Option<Settings> {
    INITIAL_VALUES.lock().unwrap().iter().find(|(q, k, _)| q == p && k.as_deref() == copy_name).map(|(_, _, s)| s.clone())
}

fn meta_from(w: &library::xmp::Values) -> Meta {
    Meta { rating: w.stars, flag: Flag::from_i8(w.flag), label: w.label }
}

/// Load: (settings, rating, own settings present?). Photo of the library: from the library (the main store,
/// [`library::store`]); otherwise from the sidecar next to it.
pub fn load_settings(p: &Path, copy_name: Option<&str>) -> (Settings, Meta, bool) {
    if let Some(loc) = library::store::locate(p) {
        return load_from_library(p, loc, copy_name);
    }
    load_from_sidecar(p, copy_name)
}

fn meta_of(m: (u8, i8, u8)) -> Meta {
    Meta { rating: m.0, flag: Flag::from_i8(m.1), label: m.2 }
}

fn load_from_library(p: &Path, loc: (i64, bool), copy_name: Option<&str>) -> (Settings, Meta, bool) {
    let file = loc.0;
    let Some(m) = library::store::meta(file, copy_name) else {
        // a copy the library does not know
        return (Settings::default(), Meta::default(), false);
    };
    let meta = meta_of(m);
    let json = library::store::develop_at(p, Some(loc), copy_name).ok().flatten();
    if copy_name.is_none() {
        crate::develop_guard::note_loaded(p, json.as_deref());
    }
    let own = json.and_then(|j| serde_json::from_str::<Settings>(&j).ok());
    match (own, copy_name) {
        (Some(s), None) => {
            remember_start(p, None, &s);
            (s, meta, true)
        }
        (Some(s), Some(_)) => (s, meta, true),
        (None, _) => (Settings::default(), meta, false),
    }
}

fn load_from_sidecar(p: &Path, copy_name: Option<&str>) -> (Settings, Meta, bool) {
    let xp = xmp_path(p);
    let (values, broken) = match library::xmp::read(&xp) {
        library::xmp::ReadResult::Ok(w) => (Some(w), false),
        library::xmp::ReadResult::Broken => (None, true),
        library::xmp::ReadResult::Missing => (None, false),
    };
    let _ = broken;
    if let Some(k) = copy_name {
        let copies = library::sidecar::read_copies(values.as_ref().and_then(|w| w.copies_json.as_deref()));
        let Some(copy) = copies.into_iter().find(|x| x.name == k) else {
            return (Settings::default(), Meta::default(), false);
        };
        let meta = Meta { rating: copy.stars, flag: Flag::from_i8(copy.flag), label: copy.label };
        return match copy.settings.and_then(|v| serde_json::from_value::<Settings>(v).ok()) {
            Some(s) => (s, meta, true),
            None => (Settings::default(), meta, false),
        };
    }
    let meta = values.as_ref().map(meta_from).unwrap_or_default();
    let json = library::xmp::read_develop(&xp).ok().flatten();
    crate::develop_guard::note_loaded(p, json.as_deref());
    match json.and_then(|j| serde_json::from_str::<Settings>(&j).ok()) {
        Some(s) => {
            remember_start(p, None, &s);
            (s, meta, true)
        }
        None => (Settings::default(), meta, false),
    }
}

/// Save: development only if it differs from the initial values; rating always
/// (foreign XMP fields stay). Virtual copy: entry in `graineo:VirtualCopies`.
pub fn save_settings(p: &Path, copy_name: Option<&str>, s: &Settings, meta: &Meta) -> Result<(), String> {
    if let Some(local)=&s.local {local.validate()?;}
    if let Some(loc) = library::store::locate(p) {
        return save_to_library(p, loc, copy_name, s, meta);
    }
    save_to_sidecar(p, copy_name, s, meta)
}

/// Save into the library (in the background, [`library::store`]): development only if it differs from the initial
/// values, rating only on change. The sidecar next to the photo follows only if writing sidecars is on.
fn save_to_library(p: &Path, loc: (i64, bool), copy_name: Option<&str>, s: &Settings, meta: &Meta) -> Result<(), String> {
    let file = loc.0;
    let stored_json = library::store::develop_at(p, Some(loc), copy_name)?;
    // a development this version cannot read (newer version: unknown fields, masks) is never overwritten or removed
    // (`develop_guard.rs`), for originals and virtual copies alike; ratings are still saved below
    let blocked = crate::develop_guard::check(stored_json.as_deref());
    let s = Settings { view: develop::View::Developed, ..s.clone() };
    let new_meta = (meta.rating, meta.flag.as_i8(), meta.label);
    let old_meta = library::store::meta(file, copy_name);
    if let Some(k) = copy_name {
        if old_meta.is_none() {
            return Err(crate::trf!("Kopie „{k}“ fehlt in der Bibliothek", k = k));
        }
        let old: Settings = stored_json.as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
        if old_meta != Some(new_meta) {
            library::store::set_meta(file, copy_name, new_meta);
        }
        if let Some(r) = crate::develop_guard::stop(blocked, copy_shown(p, &s)) {
            return r;
        }
        if old != s {
            let json = (s != Settings::default()).then(|| serde_json::to_string(&s)).transpose().map_err(|e| e.to_string())?;
            library::store::set_develop(file, copy_name, json);
        }
        return Ok(());
    }
    if old_meta.is_some_and(|m| m != new_meta) {
        library::store::set_meta(file, None, new_meta);
    }
    let start = start_value(p, None).unwrap_or_default();
    if let Some(r) = crate::develop_guard::stop(blocked, s == start || s == Settings::default()) {
        return r;
    }
    let stored = stored_json.as_deref().and_then(|j| serde_json::from_str::<Settings>(j).ok());
    if stored.as_ref() == Some(&s) || (stored.is_none() && s == start) {
        return Ok(());
    }
    let json = serde_json::to_string(&s).map_err(|e| e.to_string())?;
    library::store::set_develop(file, None, Some(json));
    Ok(())
}

/// Save into the sidecar next to a photo outside the library.
fn save_to_sidecar(p: &Path, copy_name: Option<&str>, s: &Settings, meta: &Meta) -> Result<(), String> {
    let xp = xmp_path(p);
    let s = Settings { view: develop::View::Developed, ..s.clone() };
    let mut w = match library::xmp::read(&xp) {
        library::xmp::ReadResult::Ok(w) => w,
        library::xmp::ReadResult::Missing => library::xmp::Values::default(),
        library::xmp::ReadResult::Broken => return Err(crate::trf!("{a} ist nicht lesbar und wird nicht überschrieben", a = xp.display())),
    };
    let now = library::now();
    if let Some(k) = copy_name {
        let mut copies = library::sidecar::read_copies(w.copies_json.as_deref());
        let Some(copy) = copies.iter_mut().find(|x| x.name == k) else { return Err(crate::trf!("Kopie „{k}“ fehlt in {a}", k = k, a = xp.display())) };
        // a development of a newer version stays (`develop_guard.rs`); the rating of the copy is still saved
        if let Err(e) = crate::develop_guard::check_value(copy.settings.as_ref()) {
            let new_meta = (meta.rating, meta.flag.as_i8(), meta.label);
            if (copy.stars, copy.flag, copy.label) != new_meta {
                (copy.stars, copy.flag, copy.label) = new_meta;
                w.copies_json = library::sidecar::copies_text(&copies);
                library::xmp::write(&xp, &w)?;
            }
            return crate::develop_guard::stop(Err(e), copy_shown(p, &s)).unwrap_or(Ok(()));
        }
        let old: Settings = copy.settings.clone().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        if old == s && (copy.stars, copy.flag, copy.label) == (meta.rating, meta.flag.as_i8(), meta.label) {
            return Ok(()); // unchanged: do not touch the file
        }
        copy.settings = (s != Settings::default()).then(|| serde_json::to_value(&s).unwrap_or_default());
        copy.stars = meta.rating;
        copy.flag = meta.flag.as_i8();
        copy.label = meta.label;
        w.copies_json = library::sidecar::copies_text(&copies);
        return library::xmp::write(&xp, &w);
    }
    // write the rating only if it has changed (otherwise the file stays untouched)
    let old_meta = (w.stars, w.flag, w.label);
    let new_meta = (meta.rating, meta.flag.as_i8(), meta.label);
    if old_meta != new_meta {
        (w.stars, w.flag, w.label) = new_meta;
        w.rating_time = Some(now);
        library::xmp::write(&xp, &w)?;
    }
    let stored_json = library::xmp::read_develop(&xp)?;
    let start = start_value(p, None).unwrap_or_default();
    // a development this version cannot read is never overwritten (`develop_guard.rs`); the rating is saved above
    if let Some(r) = crate::develop_guard::stop(crate::develop_guard::check(stored_json.as_deref()), s == start || s == Settings::default()) {
        return r;
    }
    let stored = stored_json.and_then(|j| serde_json::from_str::<Settings>(&j).ok());
    if stored.as_ref() == Some(&s) || (stored.is_none() && s == start) {
        return Ok(());
    }
    let json = serde_json::to_string(&s).map_err(|e| e.to_string())?;
    library::xmp::write_develop(&xp, Some(&json))
}

/// The settings a virtual copy shows when its own development is not readable: the defaults, for a Sigma X3F the
/// initial values of the file (see `sidecar::load_entry`). Saving them is no change of the user.
fn copy_shown(p: &Path, s: &Settings) -> bool {
    *s == Settings::default() || (crate::image_kind::of(p) == crate::image_kind::ImageKind::Foveon && *s == crate::sidecar::baseline(p))
}

// ---------------------------------------------------------------------------------------------
// Profiles (camera looks)

/// Title of a film profile pack (folder name of the Camera Raw settings → short German label).
pub fn package_title(group: &str) -> String {
    let g = group.trim_end_matches(" - profiles").trim_end_matches(" - Pro");
    let (brand, kind) = match g.strip_prefix("RNI FILMS 5 ") {
        Some(rest) => ("RNI Films 5", rest),
        None => return g.to_string(),
    };
    let kind = match kind {
        "BW" => crate::tr("Schwarzweiß"),
        "Negative" => crate::tr("Negativfilm"),
        "Slide" => crate::tr("Diafilm"),
        "Vintage" => "Vintage",
        "Instant" => crate::tr("Sofortbild"),
        a => a,
    };
    format!("{brand} · {kind}")
}

/// Installed film profiles (Camera Raw looks with an RGB table, e.g. RNI Films), grouped by pack:
/// (title of the pack, [(key, name, look value `film:<UUID>`)]).
pub fn film_profile_groups() -> Vec<(String, Vec<(String, String, String)>)> {
    let mut g: Vec<(String, String, Vec<(String, String, String)>)> = Vec::new();
    for p in raw::film_profile::visible_profiles().iter() {
        let look = crate::trf!("film:{a}", a = p.uuid);
        let entry = (format!("look:{look}"), p.name.clone(), look);
        match g.iter_mut().find(|(gr, _, _)| *gr == p.group) {
            Some((_, _, v)) => v.push(entry),
            None => g.push((p.group.clone(), package_title(&p.group), vec![entry])),
        }
    }
    g.into_iter().map(|(_, t, v)| (t, v)).collect()
}

/// Display name of a look (`camera/style`).
pub fn look_name(n: &str) -> String {
    if let Some(u) = n.strip_prefix("film:") {
        return raw::film_profile::by_uuid(u).map_or_else(|| crate::tr("Look (nicht verfügbar)").to_string(), |p| p.name);
    }
    if n == rawdev::learn::MATCHED_LOOK {
        return crate::tr("An Kamera-JPEG angleichen").to_string();
    }
    let style = n.rsplit('/').next().unwrap_or(n);
    let s = match style {
        "faithful" | "natural" => crate::tr("Natürlich").to_string(),
        "bw" => crate::tr("Schwarzweiß").to_string(),
        "bw-high-contrast" => crate::tr("S/W Hochkontrast").to_string(),
        "bw-toned" => crate::tr("S/W getont").to_string(),
        "monotone" => crate::tr("Monoton").to_string(),
        "monotone-hard" => crate::tr("Monoton hart").to_string(),
        "monotone-soft" => crate::tr("Monoton weich").to_string(),
        "monochrome" => crate::tr("Monochrom").to_string(),
        "monochrome-ye" => crate::tr("Monochrom + Ye").to_string(),
        "positive-film" => crate::tr("Positivfilm").to_string(),
        "negative-film" => crate::tr("Negativfilm").to_string(),
        "vivid" => crate::tr("Lebendig").to_string(),
        "landscape" => crate::tr("Landschaft").to_string(),
        "cross-processing" => crate::tr("Cross-Entwicklung").to_string(),
        "high-saturation" => crate::tr("Hohe Sättigung").to_string(),
        "low-saturation" => crate::tr("Niedrige Sättigung").to_string(),
        "fine-detail" => crate::tr("Feindetail").to_string(),
        "unknown" => crate::tr("Unbekannt").to_string(),
        "pro-neg-std" => "Pro Neg. Std".to_string(),
        "acros-ye" => crate::tr("Acros + Ye").to_string(),
        s if s.starts_with("user") => {
            let (b, rest) = s.split_once('-').unwrap_or((s, ""));
            crate::trf!("Benutzer {a}{b}", a = b.trim_start_matches("user"), b = if rest.is_empty() { String::new() } else { format!(" ({})", rest.to_uppercase()) })
        }
        s => s.split('-').map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }).collect::<Vec<_>>().join(" "),
    };
    s
}

/// Key, title and subline of the profile card for a RAW. An unedited photo (empty look) is neutral; the
/// camera's look (picture style of the shot) is only mentioned in the subline and applied when chosen.
pub fn profile_card(doc: &RawDoc, s: &Settings) -> (String, String, String) {
    match s.raw.look.as_str() {
        "" | "neutral" if !s.film.is_active() && doc.learned_look().is_some() => {
            let key = doc.learn_key.clone().unwrap_or_default();
            let meta = rawdev::learn::entries().iter().find(|(e, _)| e.key == key).and_then(|(_, m)| m.clone());
            let camera = doc.picture_style.as_ref().map(|b| b.model.trim().to_string()).unwrap_or_default();
            ("look:".into(), crate::learn_look_ui::learned_label(&key, &camera, meta.as_ref()), crate::tr("Standardentwicklung · gelernter Look dieser Kamera").into())
        }
        "" | "neutral" => (
            "look:".into(),
            "Neutral".into(),
            match (&doc.auto_look, &doc.picture_style) {
                (Some(n), _) => crate::trf!("Standardentwicklung · Kamera: {a}", a = look_name(n)),
                (None, Some(b)) => crate::trf!("Standardentwicklung · Kamera: {a} (kein Look)", a = look_name(&b.style)),
                (None, None) => crate::tr("Standardentwicklung").into(),
            },
        ),
        n if n.starts_with("film:") => {
            let package = raw::film_profile::by_uuid(&n[5..]).map(|p| package_title(&p.group));
            (format!("look:{n}"), if s.raw.look_name.is_empty() { look_name(n) } else { s.raw.look_name.clone() }, package.unwrap_or_else(|| s.raw.look_package.clone()))
        }
        n if n == rawdev::learn::MATCHED_LOOK => (
            format!("look:{n}"),
            look_name(n),
            if doc.matched_error().is_some() { crate::tr("Kein passendes Kamera-JPEG – neutral entwickelt") } else { crate::tr("Tonkurve dieses Fotos aus seinem Kamera-JPEG") }.into(),
        ),
        // a camera look stored by name that is neither in the data folder nor bundled (e.g. a personal style whose
        // file is missing): the photo develops neutral, say so instead of naming the look as if it acted
        n if n != rawdev::learn::IDENTITY_LOOK && rawdev::looks::source(n).is_none() => {
            (format!("look:{n}"), look_name(n), crate::tr("Kamera-Look nicht gefunden – neutral entwickelt").into())
        }
        n if doc.auto_look.as_deref() == Some(n) => (format!("look:{n}"), look_name(n), crate::tr("Wie Kamera · Kamera-Look").into()),
        n => (format!("look:{n}"), look_name(n), crate::tr("Kamera-Look").into()),
    }
}

/// Entries of the profile browser: (key, name, look value). "Neutral" is the default; the look matching the
/// shot's picture style comes right after it as "Wie Kamera: …" and is applied only on click.
pub fn profile_entries(doc: &RawDoc) -> Vec<(String, String, String)> {
    let mut v = vec![("look:".to_string(), "Neutral".to_string(), String::new())];
    if let Some(n) = &doc.auto_look {
        v.push((format!("look:{n}"), crate::trf!("Wie Kamera: {a}", a = look_name(n)), n.clone()));
    }
    for n in doc.looks.iter().filter(|n| doc.auto_look.as_ref() != Some(*n)) {
        v.push((format!("look:{n}"), look_name(n), n.clone()));
    }
    // tone curve matched per photo to the camera's rendering (every camera, also without a look of its own)
    if !doc.finished {
        let n = rawdev::learn::MATCHED_LOOK;
        v.push((format!("look:{n}"), look_name(n), n.to_string()));
    }
    v
}

/// Thumbnails for the profile browser: camera looks (`look:…`) and all films.
pub fn profile_thumbnails(doc: &RawDoc, settings: &Settings, modes: &[String]) -> Vec<(String, usize, usize, Vec<u8>)> {
    let mut out = Vec::new();
    let mut base = settings.clone();
    base.film.active = false;
    base.profile_amount = 1.0;
    let st = doc.small(&base, 240);
    for m in modes {
        let Some(look) = m.strip_prefix("look:") else { continue };
        let mut t = base.clone();
        t.raw.look = look.to_string();
        let img = doc.render_stage(&st, &t);
        out.push((m.clone(), img.width, img.height, img.to_rgb8()));
    }
    let ids: Vec<&str> = modes.iter().filter(|m| !m.starts_with("look:")).map(String::as_str).collect();
    if !ids.is_empty() {
        let (scene, geo) = doc.film_scene(&st, settings);
        out.extend(develop::film::thumbnails(&scene, &settings.film, geo, Some(&ids)).into_iter().filter_map(|t| t.image.ok().map(|img| (t.stock.id.clone(), img.width, img.height, img.to_rgb8()))));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Controls

/// Slider in `crs:` units (−100 … +100) for a field −1 … +1.
fn slider100(ui: &mut egui::Ui, label: &str, v: &mut f64, hint: &str, gradient: Option<[Color32; 2]>) -> bool {
    let mut x = (*v * 100.0).round();
    let r = match gradient {
        Some(g) => lr::slider_gradient(ui, label, &mut x, -100.0..=100.0, 0.0, 1.0, hint, g),
        None => lr::slider_hint(ui, label, &mut x, -100.0..=100.0, 0.0, 1.0, hint),
    };
    if r {
        *v = x / 100.0;
    }
    r
}

fn slider100_f32(ui: &mut egui::Ui, label: &str, v: &mut f32, hint: &str, gradient: Option<[Color32; 2]>) -> bool {
    let mut x = *v as f64;
    let r = slider100(ui, label, &mut x, hint, gradient);
    if r {
        *v = x as f32;
    }
    r
}

/// White balance presets: (value, icon, name).
const WB: [(&str, lr::Icon, &str); 8] = [
    ("as_shot", lr::Icon::Camera, "Wie Aufnahme"),
    ("auto", lr::Icon::Auto, "Automatisch"),
    ("daylight", lr::Icon::Sun, "Tageslicht"),
    ("cloudy", lr::Icon::Cloud, "Bewölkt"),
    ("shade", lr::Icon::Shade, "Schatten"),
    ("tungsten", lr::Icon::Bulb, "Kunstlicht"),
    ("fluorescent", lr::Icon::Tube, "Leuchtstofflampe"),
    ("flash", lr::Icon::Flash, "Blitz"),
];

/// Hint "Initial values from Lightroom" with reset (fixed row, only with Lightroom initial values).
pub fn lightroom_hint(ui: &mut egui::Ui, s: &mut Settings) {
    if !s.raw.from_lightroom {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::tr("Startwerte aus Lightroom")).small().color(lr::TEXT_DIM));
        crate::help::info(ui, "lr-start", "Startwerte aus Lightroom", &crate::help::RAW_LR_START);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(egui::Button::new(egui::RichText::new(crate::tr("Zurücksetzen")).small()).min_size(egui::vec2(0.0, 18.0))).clicked() {
                let view = s.view;
                *s = Settings { view, ..Settings::default() };
            }
        });
    });
}

/// "Basic" section for RAWs. `wb_picker`: color picker active.
pub fn defaults(ui: &mut egui::Ui, s: &mut Settings, doc: &RawDoc, wb_picker: &mut bool) {
    let d = Settings::default();
    let dr = develop::settings::RawAdjust::default();
    let changed = s.monochrome != d.monochrome
        || s.raw.wb_mode != dr.wb_mode
        || s.exposure != d.exposure
        || s.contrast != d.contrast
        || s.highlight != d.highlight
        || s.shadow != d.shadow
        || s.raw.whites != 0.0
        || s.raw.blacks != 0.0
        || s.raw.texture != 0.0
        || s.clarity != d.clarity
        || s.vibrance != d.vibrance
        || s.saturation != d.saturation
        || s.dehaze != d.dehaze
        || s.noise_brake != d.noise_brake;
    let mut bp = s.bypass;
    let mut reset_tone = false;
    let brake_key = egui::Id::unique(("noise-brake-shown", &doc.path));
    let reset = lr::section_switch(ui, "Grundeinstellungen", true, changed, &mut bp.basic, |ui| {
        lr::choice(ui, "Behandlung", &mut s.monochrome, &[(false, crate::tr("Farbe")), (true, crate::tr("Schwarzweiß"))], "");
        // White balance: one drop-down list of the presets with the picker beside it; temperature/tint absolute.
        // Finished images: only "As Shot" and "Auto" (illuminant presets make no sense there)
        let wb: Vec<&(&str, lr::Icon, &str)> = WB.iter().filter(|w| !doc.finished || matches!(w.0, "as_shot" | "auto")).collect();
        let items: Vec<(lr::Icon, &str)> = wb.iter().map(|(_, i, n)| (*i, *n)).collect();
        let selected = wb.iter().position(|(v, ..)| *v == s.raw.wb_mode);
        let (picked, picker) = lr::icon_select(ui, "wb-raw", "Weißabgleich", &items, selected, (lr::Icon::Custom, "Eigener"), lr::Icon::Picker, *wb_picker, &lr::with_shortcut("Pipette: neutrale Bildstelle anklicken", "eyedropper"));
        if picker {
            *wb_picker = !*wb_picker;
        }
        if let Some(i) = picked {
            s.raw.wb_mode = wb[i].0.to_string();
            *wb_picker = false;
        }
        let (t, g) = doc.temp_tint(s);
        // Moving one slider switches to "Eigener"; the other number keeps the exact current value (never the
        // rounded or range-limited value of its slider).
        if doc.finished {
            // relative as in common photo apps for JPEG: −100 … +100 (cooler/warmer, greener/magenta)
            let (rel_t, rel_g) = rawdev::wb::relative_from(t, g);
            let (mut rt, mut rg) = (rel_t.round() as f64, rel_g.round() as f64);
            if lr::slider_gradient(ui, "Temperatur", &mut rt, -100.0..=100.0, 0.0, 1.0, "Relativ: − kühler (blau), + wärmer (gelb)", lr::GRADIENT_TEMP) {
                s.raw.wb_mode = "custom".into();
                s.raw.temperature = rawdev::wb::relative_to(rt as f32, 0.0).0;
                s.raw.tint = g;
            }
            if lr::slider_gradient(ui, "Tönung", &mut rg, -100.0..=100.0, 0.0, 1.0, "− grüner, + magenta", lr::GRADIENT_TINT) {
                s.raw.wb_mode = "custom".into();
                s.raw.temperature = t;
                s.raw.tint = rawdev::wb::relative_to(0.0, rg as f32).1;
            }
        } else {
            // usual range of the track; values beyond it (as-shot and auto up to 50000 K) stay as they are: the knob
            // sits at the edge and the number shows the real value (the slider never clamps a value it only draws)
            let k_range = 2000.0..=12000.0;
            let mut tv = t as f64;
            if lr::slider_gradient(ui, "Temperatur", &mut tv, k_range, doc.as_shot.0 as f64, 50.0, "Farbtemperatur in Kelvin: kleiner kühler (blau), größer wärmer (gelb)", lr::GRADIENT_TEMP) {
                s.raw.wb_mode = "custom".into();
                s.raw.temperature = tv as f32;
                s.raw.tint = g;
            }
            let mut gv = g.round() as f64;
            if lr::slider_gradient(ui, "Tönung", &mut gv, -150.0..=150.0, doc.as_shot.1.round() as f64, 1.0, "− grüner, + magenta", lr::GRADIENT_TINT) {
                s.raw.wb_mode = "custom".into();
                s.raw.temperature = t;
                s.raw.tint = gv as f32;
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            reset_tone = lr::group_label(ui, "Tonwert");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = egui::Button::new(egui::RichText::new(crate::tr("Automatisch")).small()).min_size(egui::vec2(0.0, 18.0));
                if ui.add(b).clicked() {
                    doc.auto_tone(s);
                }
            });
        });
        lr::slider_gradient(ui, "Belichtung", &mut s.exposure, -5.0..=5.0, 0.0, 0.01, "In Blendenstufen, linear vor der Tonkurve", lr::GRADIENT_EXPOSURE);
        let film_on = s.film.is_active();
        ui.add_enabled_ui(!film_on, |ui| {
            slider100(ui, "Kontrast", &mut s.contrast, "Steilheit der Tonkurve in den Mitteltönen", None);
        });
        slider100(ui, "Lichter", &mut s.highlight, "Helle Bildbereiche örtlich abdunkeln (−) oder aufhellen (+)", None);
        slider100(ui, "Tiefen", &mut s.shadow, "Dunkle Bildbereiche örtlich aufhellen (+) oder abdunkeln (−)", None);
        // Noise brake right under Shadows; it acts when brightening with exposure, shadows or whites
        // (rawdev::render::brake_active)
        let lifted = s.exposure > 0.0 || s.shadow > 0.0 || s.raw.whites > 0.0;
        noise_brake(ui, &mut s.noise_brake, lifted, brake_key, "Wie stark Rauschen beim Aufhellen mitwächst.");
        slider100_f32(ui, "Weiß", &mut s.raw.whites, "Weißpunkt: hellste Töne heller (+) oder zurückgenommen (−)", None);
        slider100_f32(ui, "Schwarz", &mut s.raw.blacks, "Schwarzpunkt: tiefste Töne heller (+) oder satter (−)", None);
        ui.add_space(4.0);
        if lr::group_label(ui, "Präsenz") {
            s.raw.texture = 0.0;
            s.clarity = 0.0;
            s.dehaze = d.dehaze;
            s.vibrance = 0.0;
            s.saturation = 0.0;
        }
        slider100_f32(ui, "Struktur", &mut s.raw.texture, "Feine Details betonen (+) oder glätten (−)", None);
        slider100_f32(ui, "Klarheit", &mut s.clarity, "Lokaler Kontrast in den Mitteltönen", None);
        slider100_f32(ui, "Dunst entfernen", &mut s.dehaze, "Dunst und Schleier herausrechnen (Dark Channel), negativ: Dunst hinzufügen", None);
        ui.add_enabled_ui(!s.monochrome, |ui| {
            slider100_f32(ui, "Dynamik", &mut s.vibrance, "Hebt blasse Farben stärker an als bereits kräftige – schont Hauttöne und Himmel", Some(lr::GRADIENT_SAT));
            slider100_f32(ui, "Sättigung", &mut s.saturation, "Alle Farben gleichmäßig kräftiger oder blasser", Some(lr::GRADIENT_SAT));
        });
    });
    s.bypass = bp;
    if reset {
        s.monochrome = d.monochrome;
        s.raw.wb_mode = dr.wb_mode.clone();
        s.raw.texture = 0.0;
        s.clarity = 0.0;
        s.dehaze = d.dehaze;
        s.vibrance = 0.0;
        s.saturation = 0.0;
        reset_tone = true;
    }
    if reset_tone {
        s.exposure = 0.0;
        s.contrast = 0.0;
        s.highlight = 0.0;
        s.shadow = 0.0;
        s.raw.whites = 0.0;
        s.raw.blacks = 0.0;
        s.noise_brake = d.noise_brake;
    }
}

/// Noise brake (Off · Colour · Colour + grain) as a steps control right under the brightening sliders, Sigma and other
/// cameras. It appears once the image is brightened (`lifted`) and from then on stays for this image (`key`), so
/// dragging a slider across 0 does not make the rows below jump; while it has no effect it is dimmed but stays
/// adjustable. A brake that is switched on is always shown. `hint` selects the help card (empty: Sigma/Fill Light).
pub fn noise_brake(ui: &mut egui::Ui, value: &mut develop::NoiseBrake, lifted: bool, key: egui::Id, hint: &str) {
    let seen = ui.data_mut(|d| {
        let v = d.get_temp_mut_or_insert_with(key, || false);
        *v |= lifted;
        *v
    });
    if !seen && *value == develop::NoiseBrake::Off {
        return;
    }
    use develop::NoiseBrake as B;
    ui.scope(|ui| {
        if !lifted {
            ui.multiply_opacity(0.45);
        }
        lr::steps(ui, "Rauschbremse", value, &[(B::Off, "Aus"), (B::Color, "Farbe"), (B::ColorGrain, "Farbe + Korn")], B::Off, hint);
    });
}

/// Lens correction (profile) – part of the "Lens corrections" section. The source of the
/// profile is fixed as soon as the preview has been rendered once (no waiting here).
pub fn lens(ui: &mut egui::Ui, s: &mut Settings, doc: &RawDoc) {
    let known = if doc.finished { Some(None) } else { doc.lens_source_known() };
    if known.is_none() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    let source = known.unwrap_or(Some("…"));
    let dr = develop::settings::RawAdjust::default();
    if lr::group_label(ui, "Profilkorrektur") {
        s.raw.lens = dr.lens;
        s.raw.lens_image = dr.lens_image;
        s.raw.lens_distortion = dr.lens_distortion;
        s.raw.lens_vignette = dr.lens_vignette;
        s.raw.lens_ca = dr.lens_ca;
    }
    // finished images: lensfun, automatically off (camera/phone have usually corrected already)
    let status = doc.finished.then(|| doc.lens_status());
    let on = match &status {
        Some(b) => {
            let mut on = s.raw.lens_image.unwrap_or(b.default_on);
            ui.add_enabled_ui(b.profile.is_some(), |ui| {
                if lr::choice(ui, "Profil", &mut on, &[(false, "Aus"), (true, "An")], "") {
                    s.raw.lens_image = Some(on);
                }
            });
            on && b.profile.is_some()
        }
        None => {
            lr::choice(ui, "Profil", &mut s.raw.lens, &[(false, "Aus"), (true, "An")], "");
            s.raw.lens && source.is_some()
        }
    };
    ui.add_enabled_ui(on, |ui| {
        let mut v = (s.raw.lens_distortion * 100.0).round() as f64;
        if lr::slider_hint(ui, "Verzeichnung", &mut v, 0.0..=200.0, 100.0, 1.0, "Stärke der Verzeichnungskorrektur in Prozent des Profils") {
            s.raw.lens_distortion = v as f32 / 100.0;
        }
        let mut g = (s.raw.lens_vignette * 100.0).round() as f64;
        if lr::slider_hint(ui, "Vignettierung", &mut g, 0.0..=200.0, 100.0, 1.0, "Stärke der Aufhellung der Bildecken in Prozent des Profils") {
            s.raw.lens_vignette = g as f32 / 100.0;
        }
        ui.checkbox(&mut s.raw.lens_ca, crate::tr("Farbquerfehler entfernen"));
    });
    let note = match (&status, source) {
        (Some(b), _) => match (&b.profile, on) {
            (Some(p), true) => crate::trf!("Profil: {p}", p = p),
            (Some(p), false) => format!("{p} · {}", b.reason),
            (None, _) => b.reason.to_string(),
        },
        (None, Some(q)) => crate::trf!("Profil: {q}", q = q),
        (None, None) => crate::tr("Kein Profil für dieses Objektiv").to_string(),
    };
    // two fixed rows (profile, reason) – no jumping
    ui.allocate_ui(egui::vec2(ui.available_width(), 30.0), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(note).small().color(lr::TEXT_DIM)).wrap());
    });
    ui.add_space(4.0);
}

/// Noise reduction (`crs:` sliders) – part of the "Detail" section.
pub fn noise(ui: &mut egui::Ui, s: &mut Settings, possible: bool, xtrans: bool) {
    let dr = develop::settings::RawAdjust::default();
    if lr::group_label(ui, "Rauschminderung") {
        s.raw.nr_luma = dr.nr_luma;
        s.raw.nr_luma_detail = dr.nr_luma_detail;
        s.raw.nr_color = dr.nr_color;
        s.raw.nr_color_detail = dr.nr_color_detail;
        s.raw.false_color = dr.false_color;
    }
    ui.add_enabled_ui(possible, |ui| {
        let f = |ui: &mut egui::Ui, label: &str, v: &mut f32, default_value: f32, hint: &str, on: bool| {
            ui.add_enabled_ui(on, |ui| {
                let mut x = v.round() as f64;
                if lr::slider_hint(ui, label, &mut x, 0.0..=100.0, default_value as f64, 1.0, hint) {
                    *v = x as f32;
                }
            });
        };
        f(ui, "Luminanz", &mut s.raw.nr_luma, dr.nr_luma, "Glättet das Helligkeitsrauschen (Körnigkeit)", true);
        let l = s.raw.nr_luma > 0.0;
        f(ui, "Luminanz-Detail", &mut s.raw.nr_luma_detail, dr.nr_luma_detail, "Höher = feine Strukturen bleiben, etwas mehr Restrauschen", l);
        f(ui, "Farbe", &mut s.raw.nr_color, dr.nr_color, "Entfernt farbige Flecken im Rauschen", true);
        let c = s.raw.nr_color > 0.0;
        f(ui, "Farb-Detail", &mut s.raw.nr_color_detail, dr.nr_color_detail, "Höher = feine Farbkanten bleiben", c);
        if xtrans {
            let mut x = (s.raw.false_color * 100.0).round() as f64;
            if lr::slider_hint(ui, "Falschfarben", &mut x, 0.0..=100.0, (dr.false_color * 100.0) as f64, 1.0, "Entfernt einzelne bunte Pünktchen an feinen, hellen Kanten (X-Trans)") {
                s.raw.false_color = x as f32 / 100.0;
            }
        }
    });
    if !possible {
        ui.label(egui::RichText::new(crate::tr("Für dieses Format nicht verfügbar (bereits entwickelte Daten)")).small().color(lr::TEXT_DIM));
    }
}
