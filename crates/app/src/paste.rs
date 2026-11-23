//! "Copy settings" with selection:
//! When copying you choose the groups that are transferred on paste.

use develop::Settings;
use eframe::egui;

use crate::lr;

pub const GROUPS: [(&str, &str); N] = [
    ("Profil", "Farbmodus der Kamera bzw. Kamera-Look, Intensität, Kalibrierung"),
    ("Weißabgleich", "Vorgabe, Pipette, Temperatur, Tönung"),
    ("Tonwert", "Belichtung, Kontrast, Lichter, Tiefen, Weiß, Schwarz"),
    ("Fill Light", "Fill Light, Rauschbremse"),
    ("Präsenz", "Struktur, Klarheit, Dunst entfernen, Dynamik, Sättigung"),
    ("Farbstiche", "Lichter neutral, Lichter-Farbe, Lichter-Struktur"),
    ("Tonwert-Entzerrer", "Belichtung je Helligkeitszone, Maske"),
    ("Gradationskurve", "Punktkurve RGB, Rot, Grün, Blau"),
    ("Schwarzweiß", "Behandlung und Schichtmischung"),
    ("Farbmischer", ""),
    ("Schichtmischer", "Foveon-Schichten frei auf Rot, Grün, Blau verteilen"),
    ("Color Grading", ""),
    ("Effekte", "Vignette, Korn, matte Tiefen"),
    ("Film", "Filmsimulation: Film, Papier, Belichtung, Abzugsfarbe, Korn, Halation, Kontrast"),
    ("Objektivkorrekturen", "Objektivprofil bzw. chromatische Aberration"),
    ("Farbsäume", "Violett und Grün: Betrag, Farbton-Bereich"),
    ("Schärfen", "Foveon-Schärfe und zusätzliches Schärfen"),
    ("Rauschreduzierung", ""),
    ("Lokale Korrekturen", "Masken und lokale Regler"),
    ("Freistellen & Drehen", "Ausschnitt, Winkel, Perspektive, 90°-Drehung"),
];

/// Number of groups.
pub const N: usize = 20;

/// Position of a group in [`GROUPS`] (access by name so that new groups do not shift the
/// mapping).
fn index(name: &str) -> usize {
    GROUPS.iter().position(|(n, _)| *n == name).expect("unknown group")
}

/// Selected groups; crop is initially off, as in common photo apps.
#[derive(Clone, Copy)]
pub struct Groups(pub [bool; N]);

impl Groups {
    pub fn all() -> Self {
        Self([true; N])
    }
}

/// Groups that belong to a section of the Develop panel (for "Copy/paste section"
/// in the context menu of the section header).
pub fn for_section(title: &str) -> Option<Groups> {
    let names: &[&str] = match title {
        "Grundeinstellungen" => &["Profil", "Weißabgleich", "Tonwert", "Fill Light", "Präsenz"],
        "Farbstiche" => &["Farbstiche"],
        "Tonwert-Entzerrer" => &["Tonwert-Entzerrer"],
        "Gradationskurve" => &["Gradationskurve"],
        "S/W-Mischung (Sensorschichten)" => &["Schwarzweiß"],
        "Farbmischer" => &["Farbmischer"],
        "Schichtmischer" => &["Schichtmischer"],
        "Color Grading" => &["Color Grading"],
        "Kalibrierung" => &["Profil"],
        "Effekte" => &["Effekte"],
        "Film" | "Filmsimulation" => &["Film"],
        "Objektivkorrekturen" => &["Objektivkorrekturen", "Farbsäume"],
        "Lokale Korrekturen" => &["Lokale Korrekturen"],
        "Details" => &["Schärfen", "Rauschreduzierung"],
        "Freistellen & Ausrichten" => &["Freistellen & Drehen"],
        _ => return None,
    };
    let mut g = [false; N];
    for n in names {
        g[index(n)] = true;
    }
    Some(Groups(g))
}

impl Default for Groups {
    fn default() -> Self {
        let mut g = [true; N];
        g[index("Freistellen & Drehen")] = false;
        Self(g)
    }
}

/// Transfers the selected groups from `src` to `dst`.
pub fn merge(dst: &Settings, src: &Settings, g: &Groups) -> Settings {
    let mut s = dst.clone();
    let on = |name: &str| g.0[index(name)];
    // bitmap masks belong to one image: on the target they are computed again from their rule (subject, people),
    // click selections are left out (`masks::Document::for_other_photo`)
    if on("Lokale Korrekturen") { s.local = src.local.as_ref().and_then(|d| d.for_other_photo().0); }
    if on("Profil") {
        s.color_mode = src.color_mode.clone();
        s.profile_amount = src.profile_amount;
        s.raw.look = src.raw.look.clone();
        s.raw.look_name = src.raw.look_name.clone();
        s.raw.look_package = src.raw.look_package.clone();
        // Calibration changes the primaries of the profile
        s.raw.calibration = src.raw.calibration;
    }
    if on("Weißabgleich") {
        s.white_balance = src.white_balance.clone();
        s.wb_mult = src.wb_mult;
        s.wb_temp = src.wb_temp;
        s.wb_tint = src.wb_tint;
        s.raw.wb_mode = src.raw.wb_mode.clone();
        s.raw.temperature = src.raw.temperature;
        s.raw.tint = src.raw.tint;
    }
    if on("Tonwert") {
        s.exposure = src.exposure;
        s.contrast = src.contrast;
        s.highlight = src.highlight;
        s.shadow = src.shadow;
        s.raw.whites = src.raw.whites;
        s.raw.blacks = src.raw.blacks;
        s.bypass.basic = src.bypass.basic;
    }
    if on("Fill Light") {
        s.fill_light = src.fill_light;
        s.noise_brake = src.noise_brake;
    }
    if on("Präsenz") {
        s.clarity = src.clarity;
        s.dehaze = src.dehaze;
        s.vibrance = src.vibrance;
        s.saturation = src.saturation;
        s.raw.texture = src.raw.texture;
    }
    if on("Farbstiche") {
        s.highlight_neutralize = src.highlight_neutralize;
        s.highlight_colour = src.highlight_colour;
        s.highlight_structure = src.highlight_structure;
        s.bypass.casts = src.bypass.casts;
    }
    if on("Tonwert-Entzerrer") {
        s.tone_eq = src.tone_eq.clone();
    }
    if on("Gradationskurve") {
        s.tone_curve = src.tone_curve.clone();
        s.bypass.curve = src.bypass.curve;
    }
    if on("Schwarzweiß") {
        s.monochrome = src.monochrome;
        s.mix = src.mix;
    }
    if on("Farbmischer") {
        s.hsl = src.hsl;
        s.bypass.mixer = src.bypass.mixer;
    }
    if on("Schichtmischer") {
        s.layer_matrix = src.layer_matrix;
        s.bypass.layers = src.bypass.layers;
    }
    if on("Color Grading") {
        s.grade_shadows = src.grade_shadows;
        s.grade_midtones = src.grade_midtones;
        s.grade_highlights = src.grade_highlights;
        s.grade_balance = src.grade_balance;
        s.bypass.grading = src.bypass.grading;
    }
    if on("Effekte") {
        s.vignette = src.vignette;
        s.vignette_mid = src.vignette_mid;
        s.grain_amount = src.grain_amount;
        s.grain_size = src.grain_size;
        s.grain_rough = src.grain_rough;
        s.fade = src.fade;
        s.bypass.effects = src.bypass.effects;
    }
    if on("Film") {
        s.film = src.film.clone();
        s.bypass.film = src.bypass.film;
    }
    if on("Objektivkorrekturen") {
        s.ca_mode = src.ca_mode;
        s.ca_profile = src.ca_profile;
        s.ca_red = src.ca_red;
        s.ca_blue = src.ca_blue;
        s.raw.lens = src.raw.lens;
        s.raw.lens_distortion = src.raw.lens_distortion;
        s.raw.lens_vignette = src.raw.lens_vignette;
        s.raw.lens_ca = src.raw.lens_ca;
        s.raw.lens_image = src.raw.lens_image;
        s.raw.lens_profile = src.raw.lens_profile.clone();
    }
    if on("Farbsäume") {
        s.defringe_purple_amount = src.defringe_purple_amount;
        s.defringe_purple_hue = src.defringe_purple_hue;
        s.defringe_green_amount = src.defringe_green_amount;
        s.defringe_green_hue = src.defringe_green_hue;
    }
    if on("Schärfen") {
        s.sigma_sharpness = src.sigma_sharpness;
        s.sharpen = src.sharpen;
        s.sharpen_radius = src.sharpen_radius;
        s.sharpen_mask = src.sharpen_mask;
    }
    if on("Rauschreduzierung") {
        s.denoise = src.denoise;
        s.nr_luma = src.nr_luma;
        s.nr_chroma = src.nr_chroma;
        s.quattro_detail = src.quattro_detail;
        // binning (Quattro at high ISO) belongs to the noise
        s.binning = src.binning;
        s.raw.nr_luma = src.raw.nr_luma;
        s.raw.nr_luma_detail = src.raw.nr_luma_detail;
        s.raw.nr_color = src.raw.nr_color;
        s.raw.nr_color_detail = src.raw.nr_color_detail;
        s.raw.false_color = src.raw.false_color;
        s.raw.ai_denoise = src.raw.ai_denoise;
    }
    if on("Freistellen & Drehen") {
        s.crop = src.crop;
        s.crop_angle = src.crop_angle;
        s.perspective_v = src.perspective_v;
        s.perspective_h = src.perspective_h;
        s.rotate = src.rotate;
    }
    s
}

/// "Copy settings" dialog (shared dialog component). Returns: copy confirmed.
pub fn dialog(ctx: &egui::Context, open: &mut bool, groups: &mut Groups, warning: Option<&str>) -> bool {
    use crate::dialog;
    let n = groups.0.iter().filter(|g| **g).count();
    let title = crate::trf!("{n} von {N} Gruppen kopieren", n = n, N = N);
    let a = dialog::show(
        ctx,
        "kopieren",
        &title,
        700.0,
        |ui| {
            dialog::text(ui, crate::tr("Diese Gruppen überträgt „Einfügen“ (⇧⌘V) auf andere Bilder."));
            if groups.0[index("Profil")] { if let Some(w) = warning { dialog::text(ui, w); dialog::hint(ui, crate::tr("Der vollständige Profilverweis wird mitkopiert.")); } }
            group_choice(ui, "kopieren-gruppen", groups);
            dialog::hint(ui, crate::tr("⌥⇧⌘C kopiert alles ohne diesen Dialog."));
        },
        dialog::Buttons::confirm(crate::tr("Kopieren"), n > 0),
    );
    if a != dialog::Answer::Open {
        *open = false;
    }
    a == dialog::Answer::Primary
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value of "Basic" and "Colour casts" travels with copy/paste: with all groups and section by section.
    #[test]
    fn light_and_color_complete() {
        let mut src = Settings { exposure: 0.5, contrast: 0.3, highlight: -0.4, shadow: 0.6, fill_light: 0.7, noise_brake: develop::NoiseBrake::ColorGrain, ..Settings::default() };
        src.dehaze = 0.25;
        src.clarity = 0.2;
        src.vibrance = 0.15;
        src.saturation = -0.1;
        src.highlight_neutralize = 0.45;
        src.white_balance = Some("Sunlight".into());
        src.wb_temp = 0.1;
        src.wb_tint = -0.1;
        src.raw.whites = 0.2;
        src.raw.blacks = -0.2;
        src.raw.texture = 0.3;
        src.raw.wb_mode = "daylight".into();
        src.bypass.basic = true;
        src.bypass.casts = true;
        let fields = |s: &Settings| {
            (
                (s.exposure, s.contrast, s.highlight, s.shadow, s.fill_light, s.noise_brake, s.dehaze, s.clarity, s.vibrance, s.saturation),
                (s.highlight_neutralize, s.white_balance.clone(), s.wb_temp, s.wb_tint),
                (s.raw.whites, s.raw.blacks, s.raw.texture, s.raw.wb_mode.clone(), s.bypass.basic, s.bypass.casts),
            )
        };
        let d = Settings::default();
        assert_eq!(fields(&merge(&d, &src, &Groups::all())), fields(&src));
        let basic = merge(&d, &src, &for_section("Grundeinstellungen").unwrap());
        let both = merge(&basic, &src, &for_section("Farbstiche").unwrap());
        assert_eq!(fields(&both), fields(&src));
        // the cast section copies only its own sliders and switch
        let casts = merge(&d, &src, &for_section("Farbstiche").unwrap());
        assert_eq!((casts.highlight_neutralize, casts.bypass.casts), (0.45, true));
        assert_eq!((casts.dehaze, casts.exposure, casts.bypass.basic), (0.0, 0.0, false));
        // and "Basic" no longer takes the cast sliders
        assert_eq!(basic.highlight_neutralize, 0.0);
    }
}

/// Choice of the groups as two columns of checkboxes with "All" / "None" (copy dialog and "Transfer settings").
pub fn group_choice(ui: &mut egui::Ui, id: &str, groups: &mut Groups) {
    crate::dialog::options(ui, |ui| {
        egui::Grid::new(id).num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            for (i, (name, hint)) in GROUPS.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(310.0);
                    ui.checkbox(&mut groups.0[i], crate::tr(name));
                    ui.horizontal(|ui| {
                        ui.add_space(22.0);
                        ui.add(egui::Label::new(egui::RichText::new(if hint.is_empty() { " " } else { crate::tr(hint) }).small().color(lr::TEXT_DIM)).wrap());
                    });
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.small_button(crate::tr("Alle")).clicked() {
                groups.0 = [true; N];
            }
            if ui.small_button(crate::tr("Keine")).clicked() {
                groups.0 = [false; N];
            }
        });
    });
}

impl Groups {
    /// Names of the chosen groups (stored by name, so new groups do not shift a remembered choice).
    pub fn names(&self) -> Vec<&'static str> {
        GROUPS.iter().zip(self.0).filter(|(_, on)| *on).map(|((n, _), _)| *n).collect()
    }

    /// Groups from stored names; unknown names are ignored.
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut g = [false; N];
        for n in names {
            if let Some(i) = GROUPS.iter().position(|(x, _)| *x == n) {
                g[i] = true;
            }
        }
        Self(g)
    }

    pub fn count(&self) -> usize {
        self.0.iter().filter(|g| **g).count()
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn names_round_trip_and_ignore_unknown() {
        let g = Groups::default();
        let back = Groups::from_names(g.names().into_iter().chain(["gibt es nicht"]));
        assert_eq!(back.0, g.0);
        assert_eq!(g.count(), N - 1);
        assert!(!g.names().contains(&"Freistellen & Drehen"));
    }
}
