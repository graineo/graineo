//! Camera defaults: own start values per camera (maker + model) and the two global start-value preferences
//! ("lens correction on by default", default of "Lichter neutral").
//!
//! Order on the first opening of an unedited photo: graineo's start values → global preferences → camera default
//! (only the groups it contains, chosen as for "Copy settings", see [`crate::paste`]). Photos with own settings,
//! Lightroom handovers, SIGMA Photo Pro start values, merged (HDR) photos, virtual copies and finished images
//! (JPEG/HEIC/TIFF: the camera already rendered them, lens correction and noise reduction would act twice) stay as
//! they are. "Reset" stays neutral; "Reset to camera default" applies the chain on top of the neutral values.
//!
//! Stored in `camera-defaults.json` in the data folder. Reading happens once (small file), writing in a helper
//! thread, so the UI thread never waits for the disk.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use develop::Settings;
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::{lr, paste};

const FILE: &str = "camera-defaults.json";

/// Key of the global setting "lens correction on by default" (Bayer/X-Trans RAWs with a profile found).
const LENS_KEY: &str = "lensCorrectionDefault";
/// Key of the global default of "Lichter neutral" (0 … 1).
const HN_KEY: &str = "highlightNeutralDefault";

/// One camera default.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Display name, normalized like the library's camera column ("Canon EOS RP", "SIGMA DP2 Merrill")
    pub camera: String,
    /// Names of the chosen groups ([`paste::GROUPS`])
    pub groups: Vec<String>,
    /// Settings the groups are taken from
    pub settings: Settings,
}

impl Entry {
    pub fn groups(&self) -> paste::Groups {
        paste::Groups::from_names(self.groups.iter().map(String::as_str))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    cameras: Vec<Entry>,
}

static STORE: Mutex<Option<Vec<Entry>>> = Mutex::new(None);
static GLOBALS: Mutex<Option<(bool, f32)>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn path() -> Option<std::path::PathBuf> {
    crate::app_file(FILE)
}

fn read_file(p: &std::path::Path) -> Vec<Entry> {
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Store>(&t).ok()).map(|s| s.cameras).unwrap_or_default()
}

fn with_store<R>(f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let v = g.get_or_insert_with(|| path().map(|p| read_file(&p)).unwrap_or_default());
    f(v)
}

/// Write the current list in a helper thread; an older write that comes late is skipped.
fn save(list: Vec<Entry>) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(p) = path() else { return };
    std::thread::spawn(move || {
        static WRITE: Mutex<()> = Mutex::new(());
        let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let text = serde_json::to_string_pretty(&Store { cameras: list }).unwrap_or_default();
        if let Err(e) = library::files::write_atomic(&p, text.as_bytes()) {
            crate::log(&format!("camera defaults not saved: {e}"));
        }
    });
}

/// Camera name from maker and model, as the library stores it: the model alone if it already starts with the maker.
pub fn camera_key(maker: &str, model: &str) -> String {
    let (maker, model) = (maker.trim(), model.trim());
    // "NIKON CORPORATION" + "NIKON D300" → "NIKON D300": the model already names the maker (first word)
    let first = maker.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    let upper = model.to_ascii_uppercase();
    let name = if maker.is_empty() || upper.starts_with(&maker.to_ascii_uppercase()) || (!first.is_empty() && upper.starts_with(&format!("{first} "))) { model.to_string() } else { format!("{maker} {model}") };
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Camera of a loaded photo for the defaults; `None` for finished images (JPEG, HEIC, TIFF …) and unknown cameras.
pub fn camera_of(loaded: &crate::jobs::Loaded) -> Option<String> {
    let name = match &loaded.previews.foreign {
        Some(doc) if doc.finished => return None,
        Some(doc) => camera_key(&doc.info.maker, &doc.info.model),
        None if loaded.kind == crate::image_kind::ImageKind::Foveon => camera_key("SIGMA", &loaded.cal.camera),
        None => return None,
    };
    (!name.is_empty()).then_some(name)
}

pub fn all() -> Vec<Entry> {
    with_store(|v| v.clone())
}

pub fn find(camera: &str) -> Option<Entry> {
    with_store(|v| v.iter().find(|e| same(&e.camera, camera)).cloned())
}

pub fn set(entry: Entry) {
    let list = with_store(|v| {
        match v.iter_mut().find(|e| same(&e.camera, &entry.camera)) {
            Some(e) => *e = entry,
            None => {
                v.push(entry);
                v.sort_by_key(|e| e.camera.to_lowercase());
            }
        }
        v.clone()
    });
    save(list);
}

pub fn remove(camera: &str) {
    let list = with_store(|v| {
        v.retain(|e| !same(&e.camera, camera));
        v.clone()
    });
    save(list);
}

// ─────────────────────────────────────────────── global start-value preferences

fn globals() -> (bool, f32) {
    let mut g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(|| {
        let lens = library::settings::value(LENS_KEY).as_bool().unwrap_or(true);
        let hn = library::settings::value(HN_KEY).as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32;
        (lens, hn)
    })
}

/// Lens correction on by default for Bayer/X-Trans RAWs (today's behaviour: on).
pub fn lens_default() -> bool {
    globals().0
}

/// Default of "Lichter neutral" for new photos, 0 … 1 (today's value: 0).
pub fn highlight_neutral_default() -> f32 {
    globals().1
}

pub fn set_lens_default(on: bool) {
    let mut g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let hn = g.map_or_else(|| library::settings::value(HN_KEY).as_f64().unwrap_or(0.0) as f32, |x| x.1);
    *g = Some((on, hn));
    std::thread::spawn(move || library::settings::set(LENS_KEY, serde_json::json!(on)));
}

pub fn set_highlight_neutral_default(v: f32) {
    let v = v.clamp(0.0, 1.0);
    let mut g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let lens = g.map_or_else(|| library::settings::value(LENS_KEY).as_bool().unwrap_or(true), |x| x.0);
    *g = Some((lens, v));
    std::thread::spawn(move || library::settings::set(HN_KEY, serde_json::json!(v)));
}

/// Start values of a photo: `base` (graineo's start values or the neutral reset) → global preferences → camera
/// default of `camera`. `foreign_raw`: Bayer/X-Trans RAW (the lens switch only applies there).
pub fn start_values(base: &Settings, camera: Option<&str>, foreign_raw: bool) -> Settings {
    chain(base, camera.and_then(find).as_ref(), foreign_raw, lens_default(), highlight_neutral_default())
}

fn chain(base: &Settings, entry: Option<&Entry>, foreign_raw: bool, lens: bool, hn: f32) -> Settings {
    let mut s = base.clone();
    if foreign_raw {
        s.raw.lens = lens;
    }
    if hn > 0.0 && s.highlight_neutralize == 0.0 {
        s.highlight_neutralize = hn;
    }
    if let Some(e) = entry {
        s = paste::merge(&s, &e.settings, &e.groups());
    }
    Settings { view: base.view, ..s }
}

/// Start values of this photo without a camera default: graineo's values (binning standard of the photo) and the
/// global preferences. "Unchanged" in the dialog is measured against these.
pub fn photo_base(loaded: &crate::jobs::Loaded) -> Settings {
    let mut s = Settings::default();
    crate::binning_ui::apply_standard(&loaded.path, &mut s);
    let foreign_raw = loaded.previews.foreign.as_ref().is_some_and(|d| !d.finished);
    chain(&s, None, foreign_raw, lens_default(), highlight_neutral_default())
}

/// Start values without a camera default for the format class of `camera` (no photo at hand, "Change" in the
/// preferences): Sigma's Foveon cameras develop as Sigma photos, all others as Bayer/X-Trans RAWs (what a camera
/// default is mostly used for).
fn class_base(camera: &str) -> Settings {
    chain(&Settings::default(), None, !foveon_camera(camera), lens_default(), highlight_neutral_default())
}

/// Sigma camera with a Foveon sensor (every Sigma camera with its own raw format except the fp series).
fn foveon_camera(camera: &str) -> bool {
    let c = camera.to_ascii_lowercase();
    c.starts_with("sigma") && !c.split_whitespace().any(|w| w == "fp" || w == "bf")
}

/// Should the start values be applied when the photo is shown? Only for a photo opened for the first time
/// without own settings, Lightroom or SIGMA Photo Pro start values, not for merged photos and virtual copies.
pub fn eligible(loaded: &crate::jobs::Loaded, stored: bool, entry_loaded: bool, copy: bool, path: &std::path::Path) -> bool {
    applies(stored, entry_loaded, copy, loaded.start_values_from_spp, loaded.lr_start.is_some(), library::merge::has_merge_name(path), camera_of(loaded).is_some())
}

/// The rule of [`eligible`] on its own (tested).
fn applies(stored: bool, entry_loaded: bool, copy: bool, from_spp: bool, from_lightroom: bool, merged: bool, camera_known: bool) -> bool {
    entry_loaded && !stored && !copy && !from_spp && !from_lightroom && !merged && camera_known
}

/// Readable values of a default ("Exposure +0.30", "Lens corrections on"), for chips and tooltips.
pub fn chips(e: &Entry) -> Vec<String> {
    let d = Settings::default();
    let only = paste::merge(&d, &e.settings, &e.groups());
    let mut out = crate::history::chips(&d, &only);
    // chosen groups that equal graineo's values still belong to the default
    for (name, _) in paste::GROUPS.iter().filter(|(n, _)| e.groups.iter().any(|g| g == n)) {
        let g = paste::Groups::from_names([*name]);
        if paste::merge(&d, &e.settings, &g) == d {
            out.push(crate::trf!("{g}: Standard", g = crate::tr(name)));
        }
    }
    out
}

// ─────────────────────────────────────────────── dialog "Set as default for …"

/// What the dialog does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// no default yet: take the chosen groups of the open photo
    New,
    /// a default exists: the chosen groups are replaced by the photo's values, the others stay as they were
    Replace,
    /// "Change" in the preferences: only the choice of groups of the stored values
    Change,
}

struct Edit {
    camera: String,
    mode: Mode,
    /// values of the open photo (New/Replace) or the stored values (Change)
    settings: Settings,
    /// the existing default (Replace)
    old: Option<Entry>,
    /// start values without camera default ("unchanged" is measured against them)
    base: Settings,
    groups: paste::Groups,
    /// groups of the existing default that stay (Replace; × takes one out)
    kept: paste::Groups,
}

static EDIT: Mutex<Option<Edit>> = Mutex::new(None);

/// Groups that are usually photo-dependent and are never preselected.
const PHOTO_DEPENDENT: [&str; 2] = ["Weißabgleich", "Freistellen & Drehen"];

/// Group with index `i` only.
fn one(i: usize) -> paste::Groups {
    paste::Groups::from_names([paste::GROUPS[i].0])
}

/// Values of group `i` in `s` against the start values `d`; empty when the group equals them.
fn group_values(d: &Settings, s: &Settings, i: usize) -> Vec<String> {
    let only = paste::merge(d, s, &one(i));
    if only == *d {
        return Vec::new();
    }
    let c = crate::history::chips(d, &only);
    if c.is_empty() { vec![crate::tr("verändert").to_string()] } else { c }
}

fn changed(d: &Settings, s: &Settings, i: usize) -> bool {
    paste::merge(d, s, &one(i)) != *d
}

/// Word for a group without changes: camera-dependent groups start from the camera's values.
fn unchanged_word(i: usize) -> &'static str {
    if matches!(paste::GROUPS[i].0, "Profil" | "Weißabgleich" | "Objektivkorrekturen") { crate::tr("Startwert") } else { crate::tr("neutral") }
}

/// Groups preselected for a new default: the changed groups of the photo, without the photo-dependent ones.
fn preselect(base: &Settings, s: &Settings) -> paste::Groups {
    let mut g = paste::Groups::default();
    for (i, (n, _)) in paste::GROUPS.iter().enumerate() {
        g.0[i] = changed(base, s, i) && !PHOTO_DEPENDENT.contains(n);
    }
    g
}

/// Groups preselected when no photo values are known: everything but white balance and crop.
pub fn new_groups() -> paste::Groups {
    let mut g = paste::Groups::default();
    for (i, (n, _)) in paste::GROUPS.iter().enumerate() {
        if PHOTO_DEPENDENT.contains(n) {
            g.0[i] = false;
        }
    }
    g
}

/// Open the dialog for the current photo's settings (new or replacing an existing default); `loaded` is the photo.
pub fn open_set(camera: &str, settings: &Settings, loaded: &crate::jobs::Loaded) {
    open_set_with(camera, settings, photo_base(loaded));
}

fn open_set_with(camera: &str, settings: &Settings, base: Settings) {
    let old = find(camera);
    let (mode, groups, kept) = match &old {
        // replacing: preselect the existing groups whose values differ from the photo
        Some(e) => {
            let mut g = paste::Groups::default();
            let kept = e.groups();
            for i in 0..paste::GROUPS.len() {
                g.0[i] = kept.0[i] && paste::merge(&base, &e.settings, &one(i)) != paste::merge(&base, settings, &one(i));
            }
            (Mode::Replace, g, kept)
        }
        None => (Mode::New, preselect(&base, settings), paste::Groups::default()),
    };
    *EDIT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Edit { camera: camera.to_string(), mode, settings: settings.clone(), old, groups, kept, base });
}

/// Open the dialog for an existing default ("Change" in the preferences): only the choice of groups.
pub fn open_change(camera: &str) {
    if let Some(e) = find(camera) {
        *EDIT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Edit { base: class_base(&e.camera), camera: e.camera.clone(), mode: Mode::Change, groups: e.groups(), kept: paste::Groups::default(), settings: e.settings, old: None });
    }
}

pub fn dialog_open() -> bool {
    EDIT.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Result of the dialog: groups and settings of the new entry.
fn result(e: &Edit) -> (paste::Groups, Settings) {
    match (&e.mode, &e.old) {
        (Mode::Replace, Some(old)) => {
            let mut g = paste::Groups::default();
            for i in 0..paste::GROUPS.len() {
                g.0[i] = e.groups.0[i] || e.kept.0[i];
            }
            (g, paste::merge(&old.settings, &e.settings, &e.groups))
        }
        _ => (e.groups, e.settings.clone()),
    }
}

/// One row: checkbox, (× for a kept group), values; `dim` for unchanged groups.
fn row(ui: &mut egui::Ui, e: &mut Edit, i: usize, dim: bool) {
    let name = paste::GROUPS[i].0;
    let d = e.base.clone();
    let text = match (&e.mode, &e.old) {
        (Mode::Replace, Some(old)) => {
            let was = if e.kept.0[i] { group_values(&d, &old.settings, i) } else { Vec::new() };
            let now = group_values(&d, &e.settings, i);
            let before = if !e.kept.0[i] { crate::tr("nicht im Standard").to_string() } else if was.is_empty() { unchanged_word(i).to_string() } else { was.join(" · ") };
            let after = if now.is_empty() { unchanged_word(i).to_string() } else { now.join(" · ") };
            if !e.kept.0[i] && now.is_empty() {
                unchanged_word(i).to_string()
            } else if e.kept.0[i] && paste::merge(&d, &old.settings, &one(i)) == paste::merge(&d, &e.settings, &one(i)) {
                format!("{before} ({})", crate::tr("unverändert"))
            } else {
                format!("{before} → {after}")
            }
        }
        _ => {
            let v = group_values(&d, &e.settings, i);
            if v.is_empty() { unchanged_word(i).to_string() } else { v.join(" · ") }
        }
    };
    let color = if dim { lr::TEXT_DIM() } else { lr::TEXT() };
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(220.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(220.0);
            ui.checkbox(&mut e.groups.0[i], egui::RichText::new(crate::tr(name)).color(color));
            if e.mode == Mode::Replace && e.kept.0[i] {
                let x = ui.add(egui::Button::new(egui::RichText::new("×").color(lr::TEXT_DIM())).frame(false));
                lr::hover_tip(ui, &x, crate::tr("Gruppe aus dem Standard nehmen"));
                if x.clicked() {
                    e.kept.0[i] = false;
                    e.groups.0[i] = false;
                }
            }
        });
        ui.vertical(|ui| {
            ui.add_space(2.0);
            ui.add(egui::Label::new(egui::RichText::new(text).color(if dim { lr::TEXT_DIM() } else { crate::lr::gray(0xc8) })).wrap());
            if PHOTO_DEPENDENT.contains(&name) && !dim && e.mode != Mode::Change {
                ui.label(egui::RichText::new(crate::tr("meist fotoabhängig")).small().color(lr::TEXT_DIM()));
            }
        });
    });
    ui.add_space(2.0);
}

/// Is group `i` worth showing at the top (changed, or part of the existing default)?
fn relevant(e: &Edit, i: usize) -> bool {
    match (&e.mode, &e.old) {
        (Mode::Replace, Some(old)) => changed(&e.base, &e.settings, i) || e.kept.0[i] || changed(&e.base, &old.settings, i),
        (Mode::Change, _) => changed(&e.base, &e.settings, i) || e.groups.0[i],
        _ => changed(&e.base, &e.settings, i),
    }
}

/// Draw the dialog if open (every frame, after the preferences window so it lies above it).
pub fn dialog_ui(ctx: &egui::Context) {
    let mut g = EDIT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(edit) = g.as_mut() else { return };
    use crate::dialog;
    let title = crate::trf!("Standard für {c}", c = edit.camera);
    let n = edit.groups.count() + edit.kept.count();
    let (top, rest): (Vec<usize>, Vec<usize>) = (0..paste::GROUPS.len()).partition(|&i| relevant(edit, i));
    let head = match edit.mode {
        Mode::New => crate::trf!("Übernimmt die angehakten Einstellungen dieses Fotos als Startwerte für neue, unbearbeitete Fotos der {c}.", c = edit.camera),
        Mode::Replace => crate::trf!("Für die {c} gibt es schon einen Standard. Angehakte Gruppen werden ersetzt, nicht angehakte bleiben wie bisher.", c = edit.camera),
        Mode::Change => crate::trf!("Die angehakten Gruppen gelten als Startwerte für neue Fotos der {c}.", c = edit.camera),
    };
    let primary = match edit.mode {
        Mode::New => "Festlegen",
        Mode::Replace => "Ersetzen",
        Mode::Change => "Speichern",
    };
    let a = dialog::show_scrolling(
        ctx,
        "camera-default",
        &title,
        700.0,
        24.0,
        |ui, part| match part {
            dialog::Part::Header => {}
            dialog::Part::Body => {
                dialog::text(ui, &head);
                if edit.mode == Mode::Change {
                    dialog::hint(ui, crate::tr("Werte ändern: ein Foto der Kamera öffnen, Regler stellen, erneut festlegen."));
                }
                ui.add_space(8.0);
                for &i in &top {
                    row(ui, edit, i, false);
                }
                if !rest.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(crate::tr("Unverändert")).small().color(lr::TEXT_DIM()));
                        let r = ui.available_rect_before_wrap();
                        let y = r.center().y;
                        ui.painter().hline(r.left() + 4.0..=r.right(), y, egui::Stroke::new(1.0, crate::lr::gray(0x33)));
                    });
                    ui.add_space(2.0);
                    for &i in &rest {
                        row(ui, edit, i, true);
                    }
                }
            }
            dialog::Part::Footer => {
                ui.horizontal(|ui| {
                    if lr::secondary_button(ui, None, "Alle").clicked() {
                        edit.groups.0 = [true; paste::GROUPS.len()];
                    }
                    if lr::secondary_button(ui, None, "Keine").clicked() {
                        edit.groups.0 = [false; paste::GROUPS.len()];
                    }
                });
            }
        },
        dialog::Buttons::confirm(primary, n > 0),
    );
    match a {
        dialog::Answer::Open => {}
        dialog::Answer::Primary => {
            let e = g.take().unwrap();
            let (groups, settings) = result(&e);
            set(Entry { camera: e.camera, groups: groups.names().into_iter().map(str::to_string).collect(), settings: Settings { view: develop::View::Developed, ..settings } });
        }
        dialog::Answer::Cancel => *g = None,
    }
}

// ─────────────────────────────────────────────── preferences page

/// Section "Camera defaults" of the preferences: one tile per camera with its values as chips.
pub fn settings_page(ui: &mut egui::Ui) {
    let list = all();
    crate::prefs::group(ui, &crate::trf!("Eigene Standardwerte · {n} Kameras", n = list.len()), |ui| {
        if list.is_empty() {
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(crate::tr("Noch keine Kamera eingetragen. In Bearbeiten über „Zurücksetzen ▾“ → „Als Standard für … festlegen …“ die Werte des geöffneten Fotos als Standard seiner Kamera übernehmen.")).color(lr::TEXT_DIM())).wrap());
            ui.add_space(4.0);
        }
        let mut remove_cam = None;
        for (i, e) in list.iter().enumerate() {
            if i > 0 {
                crate::prefs::separator(ui);
            }
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(&e.camera).font(lr::semibold(lr::UI_SIZE)).color(lr::TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if lr::secondary_button(ui, None, "Entfernen").clicked() {
                        remove_cam = Some(e.camera.clone());
                    }
                    ui.add_space(6.0);
                    if lr::secondary_button(ui, None, "Ändern").clicked() {
                        open_change(&e.camera);
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
                for c in chips(e) {
                    egui::Frame::new().fill(crate::lr::gray(0x2a)).inner_margin(egui::Margin::symmetric(7, 2)).show(ui, |ui| {
                        ui.label(egui::RichText::new(c).small().color(crate::lr::gray(0xc8)));
                    });
                }
            });
            ui.add_space(4.0);
        }
        if let Some(c) = remove_cam {
            remove(&c);
        }
    });
    ui.add_space(4.0);
    ui.add(egui::Label::new(egui::RichText::new(crate::tr("Gilt für neu geöffnete Fotos dieser Kamera und übersteuert für die enthaltenen Werte die Voreinstellungen unter „Entwickeln“. Bereits bearbeitete Fotos, Lightroom-Übernahmen und zusammengefügte Fotos bleiben, wie sie sind; JPEG, HEIC und TIFF hat die Kamera schon entwickelt.")).small().color(lr::TEXT_DIM())).wrap());
}

/// Tooltip text of the mark "Camera default" in the Edit panel.
pub fn tooltip(e: &Entry) -> String {
    let mut t = crate::trf!("Kamera-Standard für {c}:", c = e.camera);
    for c in chips(e) {
        t.push_str("\n• ");
        t.push_str(&c);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(groups: &[&str], s: Settings) -> Entry {
        Entry { camera: "Canon EOS RP".into(), groups: groups.iter().map(|g| g.to_string()).collect(), settings: s }
    }

    #[test]
    fn key_follows_the_library() {
        assert_eq!(camera_key("Canon", "Canon EOS RP"), "Canon EOS RP");
        assert_eq!(camera_key("FUJIFILM", "X-T2"), "FUJIFILM X-T2");
        assert_eq!(camera_key("SIGMA", "SIGMA DP2 Merrill"), "SIGMA DP2 Merrill");
        assert_eq!(camera_key(" Canon ", " EOS  RP "), "Canon EOS RP");
        assert_eq!(camera_key("NIKON CORPORATION", "NIKON D300"), "NIKON D300");
        assert!(same("canon eos rp", "Canon EOS RP"));
    }

    #[test]
    fn chain_order_and_only_chosen_groups() {
        let base = Settings::default();
        let src = Settings { exposure: 0.7, highlight_neutralize: 0.9, sharpen: 0.4, ..Settings::default() };
        // global: lens off, Lichter neutral 0.5; the camera default contains only "Farbstiche" → wins for that field
        let e = entry(&["Farbstiche"], src.clone());
        let s = chain(&base, Some(&e), true, false, 0.5);
        assert!(!s.raw.lens);
        assert_eq!(s.highlight_neutralize, 0.9);
        assert_eq!(s.exposure, 0.0, "group not chosen");
        assert_eq!(s.sharpen, base.sharpen);
        // without camera default: the global value
        assert_eq!(chain(&base, None, true, true, 0.5).highlight_neutralize, 0.5);
        // Sigma: the lens switch is for Bayer/X-Trans only
        assert_eq!(chain(&base, None, false, false, 0.0).raw.lens, base.raw.lens);
        // today's defaults change nothing
        assert_eq!(chain(&base, None, true, true, 0.0), base);
    }

    #[test]
    fn only_new_unedited_photos() {
        // stored, entry_loaded, copy, from_spp, from_lightroom, merged, camera_known
        assert!(applies(false, true, false, false, false, false, true));
        assert!(!applies(true, true, false, false, false, false, true), "already edited");
        assert!(!applies(false, false, false, false, false, false, true), "settings not read yet");
        assert!(!applies(false, true, true, false, false, false, true), "virtual copy");
        assert!(!applies(false, true, false, true, false, false, true), "SIGMA Photo Pro start values");
        assert!(!applies(false, true, false, false, true, false, true), "Lightroom handover");
        assert!(!applies(false, true, false, false, false, true, true), "merged (HDR) photo");
        assert!(!applies(false, true, false, false, false, false, false), "finished image / unknown camera");
        assert!(library::merge::has_merge_name(std::path::Path::new("/x/IMG_1-HDR.dng")));
    }

    #[test]
    fn store_round_trip() {
        let e = entry(&["Tonwert", "Objektivkorrekturen"], Settings { exposure: 0.3, ..Settings::default() });
        let text = serde_json::to_string(&Store { cameras: vec![e.clone()] }).unwrap();
        let back: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(back.cameras, vec![e]);
        let d = std::env::temp_dir().join(format!("graineo-camdef-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(FILE);
        assert!(read_file(&p).is_empty());
        std::fs::write(&p, text).unwrap();
        assert_eq!(read_file(&p).len(), 1);
        std::fs::write(&p, "{broken").unwrap();
        assert!(read_file(&p).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_dialog_leaves_white_balance_and_crop_out() {
        let n = new_groups().names();
        assert!(!n.contains(&"Weißabgleich") && !n.contains(&"Freistellen & Drehen"));
        assert!(n.contains(&"Tonwert") && n.contains(&"Objektivkorrekturen"));
    }

    #[test]
    fn replace_keeps_unchecked_groups() {
        let old = entry(&["Tonwert", "Präsenz"], Settings { exposure: 0.3, clarity: 0.1, ..Settings::default() });
        let photo = Settings { exposure: 0.8, clarity: 0.2, ..Settings::default() };
        let e = Edit { camera: old.camera.clone(), mode: Mode::Replace, settings: photo, groups: paste::Groups::from_names(["Präsenz"]), kept: old.groups(), old: Some(old), base: Settings::default() };
        let (g, s) = result(&e);
        assert_eq!(g.names(), vec!["Tonwert", "Präsenz"]);
        assert_eq!(s.exposure, 0.3, "unchecked group stays");
        assert_eq!(s.clarity, 0.2, "checked group replaced");
        // preselection of a new default: changed groups without the photo-dependent ones
        let p = preselect(&Settings::default(), &Settings { exposure: 0.5, ..Settings::default() });
        assert_eq!(p.names(), vec!["Tonwert"]);
    }

    #[test]
    fn chips_name_the_values() {
        let e = entry(&["Tonwert", "Farbstiche"], Settings { exposure: 0.3, highlight_neutralize: 1.0, ..Settings::default() });
        let c = chips(&e);
        assert!(c.iter().any(|x| x.starts_with("Belichtung")), "{c:?}");
        assert!(c.iter().any(|x| x.starts_with("Lichter neutral")), "{c:?}");
    }

    /// "Unchanged" is measured against the photo's start values: lens corrections switched on by the global
    /// preference are not a change of the photo, the exposure is.
    #[test]
    fn unchanged_against_start_values() {
        // global preferences: lens corrections off, "Lichter neutral" 0.5
        let base = chain(&Settings::default(), None, true, false, 0.5);
        let mut photo = base.clone();
        photo.exposure = 0.4;
        let idx = |g: &str| paste::GROUPS.iter().position(|(n, _)| *n == g).unwrap();
        let (lens, cast, tone) = (idx("Objektivkorrekturen"), idx("Farbstiche"), idx("Tonwert"));
        assert!(changed(&Settings::default(), &photo, lens) && changed(&Settings::default(), &photo, cast), "against neutral values they differ");
        assert!(!changed(&base, &photo, lens) && !changed(&base, &photo, cast));
        assert!(group_values(&base, &photo, lens).is_empty());
        assert!(changed(&base, &photo, tone));
        assert_eq!(preselect(&base, &photo).names(), vec!["Tonwert"]);
    }

    #[test]
    fn format_class_of_a_camera() {
        assert!(foveon_camera("SIGMA DP2 Merrill") && foveon_camera("SIGMA sd Quattro H"));
        assert!(!foveon_camera("SIGMA fp L") && !foveon_camera("Canon EOS RP"));
        assert!(class_base("Canon EOS RP").raw.lens == lens_default());
    }
}
