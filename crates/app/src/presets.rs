//! Develop presets: a categorised library
//! (Foveon specials; films live in the "Film" section) plus user
//! presets under `~/Library/Application Support/graineo/presets.json`.
//!
//! The values come from our own research (characteristics
//! of the films or the Sigma color modes, translated into our parameter space – estimated, not
//! measured). Filter and toning presets only affect their own part (layer mixing or
//! color grading) and can therefore be combined with a film preset.

use std::path::PathBuf;

use develop::Settings;
use serde::{Deserialize, Serialize};

/// Which settings a preset overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// The whole look (everything except noise reduction, white balance preset and view)
    #[default]
    Look,
    /// Only black & white + layer mixing
    Mix,
    /// Only color grading and matte shadows
    Toning,
    /// Only the film simulation (`settings.film`): darkroom recipes
    Film,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub settings: Settings,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub hint: String,
}

impl Preset {
    /// Applies the preset to `cur` (only the parts within its scope).
    pub fn apply(&self, cur: &Settings) -> Settings {
        let p = &self.settings;
        let mut s = cur.clone();
        match self.scope {
            Scope::Look => {
                s = Settings {
                    denoise: cur.denoise,
                    nr_luma: cur.nr_luma,
                    nr_chroma: cur.nr_chroma,
                    quattro_detail: cur.quattro_detail,
                    // binning belongs to the noise of the photo, not to a look
                    binning: cur.binning,
                    white_balance: cur.white_balance.clone(),
                    wb_mult: cur.wb_mult,
                    view: cur.view,
                    view_gain_ev: cur.view_gain_ev,
                    view_rescue: cur.view_rescue,
                    // Fringe removal is a lens correction, not a look
                    defringe_purple_amount: cur.defringe_purple_amount,
                    defringe_purple_hue: cur.defringe_purple_hue,
                    defringe_green_amount: cur.defringe_green_amount,
                    defringe_green_hue: cur.defringe_green_hue,
                    // objects removed and bitmap masks belong to the photo they were made on (as pasting): the photo
                    // keeps its own removals, masks come along only where their rule computes them anew
                    removals: cur.removals.clone(),
                    local: p.local.as_ref().and_then(|d| d.for_other_photo().0),
                    ..p.clone()
                };
            }
            Scope::Mix => {
                s.monochrome = true;
                s.mix = p.mix;
            }
            Scope::Toning => {
                s.grade_shadows = p.grade_shadows;
                s.grade_midtones = p.grade_midtones;
                s.grade_highlights = p.grade_highlights;
                s.grade_balance = p.grade_balance;
                s.fade = p.fade;
            }
            Scope::Film => {
                s.film = p.film.clone();
            }
        }
        s
    }

    /// The film settings of this recipe (for the darkroom recipe book): always enabled.
    pub fn film_recipe(&self) -> develop::film::FilmSettings {
        develop::film::FilmSettings { active: true, ..self.settings.film.clone() }
    }
}

/// The settings of a new own preset made from a photo: without what belongs only to that photo (objects removed,
/// bitmap masks that cannot be computed again on another photo), as pasting does.
pub fn own_preset_settings(s: Settings) -> Settings {
    Settings { removals: Vec::new(), local: s.local.as_ref().and_then(|d| d.for_other_photo().0), ..s }
}

/// New user recipe from the current film settings (scope film).
pub fn recipe(name: &str, film: &develop::film::FilmSettings) -> Preset {
    Preset {
        name: name.trim().to_string(),
        settings: Settings { film: develop::film::FilmSettings { active: true, ..film.clone() }, ..Settings::default() },
        scope: Scope::Film,
        hint: String::new(),
    }
}

/// Exchange format of a single recipe file (JSON, English keys).
const RECIPE_FORMAT: &str = "graineo-recipe";

#[derive(Serialize, Deserialize)]
struct RecipeFile {
    format: String,
    version: u32,
    name: String,
    film: develop::film::FilmSettings,
}

/// Recipe as JSON text for exchange.
pub fn recipe_json(p: &Preset) -> String {
    let d = RecipeFile { format: RECIPE_FORMAT.into(), version: 1, name: p.name.clone(), film: p.film_recipe() };
    serde_json::to_string_pretty(&d).unwrap_or_default()
}

/// Largest recipe file the import reads (bytes).
pub const RECIPE_MAX_BYTES: u64 = 1 << 20;

/// Read a recipe from an exchange file.
pub fn read_recipe(json: &str) -> Result<Preset, String> {
    let d: RecipeFile = serde_json::from_str(json).map_err(|e| crate::trf!("Keine Filmlook-Datei: {e}", e = e))?;
    if d.format != RECIPE_FORMAT {
        return Err(crate::tr("Keine graineo-Filmlook-Datei.").into());
    }
    if d.version > 1 {
        return Err(crate::trf!("Der Filmlook stammt aus einer anderen Version von graineo (Format {a}).", a = d.version));
    }
    if d.name.trim().is_empty() {
        return Err(crate::tr("Der Filmlook hat keinen Namen.").into());
    }
    if d.film.stock.is_empty() {
        return Err(crate::tr("Der Filmlook nennt keinen Film.").into());
    }
    Ok(recipe(&d.name, &d.film))
}

#[cfg(test)]
mod recipe_tests {
    use super::*;

    #[test]
    fn recipe_affects_only_the_film() {
        let mut film = develop::film::FilmSettings { stock: "kodak_trix_400".into(), film_dev: 11.0, paper_grade: 4.0, ..Default::default() };
        film.paper_matte = 1.5;
        let r = recipe("Hart", &film);
        assert_eq!(r.scope, Scope::Film);
        let mut cur = Settings::default();
        cur.exposure = 0.7;
        let applied = r.apply(&cur);
        assert_eq!(applied.exposure, 0.7, "other sliders stay");
        assert!(applied.film.active && applied.film.stock == "kodak_trix_400" && applied.film.film_dev == 11.0 && applied.film.paper_matte == 1.5);
    }

    /// An own preset never carries what belongs to the photo it was made on: removed objects stay with that photo,
    /// bitmap masks only come along as their rule (computed again on the other photo); also for older presets that
    /// still hold them.
    #[test]
    fn own_preset_leaves_photo_bound_parts_behind() {
        use masks::{Adjustments, Document, Instances, Layer, Mask, RasterSource};
        let id = format!("sha256:{}", "a".repeat(64));
        let removal = develop::spots::Removal { center: [0.5, 0.5], radius: 0.1, region: [0.3, 0.3, 0.4, 0.4], patch: format!("sha256:{}", "b".repeat(64)) };
        let layer = |source| Layer::new(Mask::Raster { raster: id.clone(), source, feather: 0.0, range: None, edge: 0.0 }, Adjustments { exposure: 1.0, ..Default::default() });
        let local = Document { version: 3, layers: vec![layer(RasterSource::Subject { instances: Instances::All }), layer(RasterSource::Painted)] };
        let photo = Settings { exposure: 0.4, removals: vec![removal.clone()], local: Some(local), ..Settings::default() };
        let check = |s: &Settings| {
            let text = serde_json::to_string(s).unwrap();
            assert!(!text.contains("sha256:"), "no raster or patch names: {text}");
            let d = s.local.as_ref().expect("subject mask comes along as its rule");
            assert_eq!(d.layers.len(), 1);
        };
        let made = own_preset_settings(photo.clone());
        assert!(made.removals.is_empty());
        assert_eq!(made.exposure, 0.4);
        check(&made);
        // an older preset that still holds them: applying leaves the target's own removals and drops the rest
        let old = Preset { name: "alt".into(), settings: photo, scope: Scope::Look, hint: String::new() };
        let own = develop::spots::Removal { center: [0.1, 0.1], patch: format!("sha256:{}", "c".repeat(64)), ..removal };
        let target = Settings { removals: vec![own.clone()], ..Settings::default() };
        let applied = old.apply(&target);
        assert_eq!(applied.removals, vec![own]);
        assert_eq!(applied.exposure, 0.4);
        check(&Settings { removals: Vec::new(), ..applied });
    }

    /// Own presets live in the app's data folder (`GRAINEO_DATA_DIR` for test apps and test runs), never in the
    /// real one of the installed app.
    #[test]
    fn presets_file_follows_data_dir() {
        let p = path().unwrap();
        assert_eq!(p, library::data_dir().join("presets.json"));
        if let Some(real) = sysdirs::app_data_dir() {
            assert!(!p.starts_with(real.join(sysdirs::APP)), "{p:?}");
        }
    }

    #[test]
    fn exchange_file_roundtrip_and_errors() {
        let film = develop::film::FilmSettings { stock: "ilford_hp5_plus".into(), paper_tone: 0.4, taking_filter: "orange21".into(), ..Default::default() };
        let r = recipe(" HP5 warm ", &film);
        assert_eq!(r.name, "HP5 warm");
        let json = recipe_json(&r);
        assert!(json.contains("\"format\": \"graineo-recipe\"") && json.contains("\"paper_tone\""));
        let read_back = read_recipe(&json).unwrap();
        assert_eq!(read_back.name, "HP5 warm");
        assert_eq!(read_back.film_recipe(), r.film_recipe());
        assert!(read_recipe("{}").is_err());
        assert!(read_recipe("kein json").is_err());
        let foreign = json.replace("graineo-recipe", "anderes");
        assert!(read_recipe(&foreign).unwrap_err().contains("graineo"));
    }

    #[test]
    fn bundled_film_recipes_carry_their_film_values() {
        let lib = library();
        let cat = lib.iter().find(|c| c.name == "Film-Rezepte").expect("category");
        assert!(cat.presets.len() >= 6);
        for p in &cat.presets {
            let f = p.film_recipe();
            assert!(f.active && !f.stock.is_empty(), "{}", p.name);
        }
    }
}

pub struct Category {
    pub name: &'static str,
    pub presets: Vec<Preset>,
}

fn path() -> Option<PathBuf> {
    // same location as the library (test runs: GRAINEO_DATA_DIR), otherwise ~/Library/Application Support/graineo
    Some(library::data_dir().join("presets.json"))
}

/// Shorthand for the library.
#[derive(Default, Clone)]
struct P {
    mode: Option<&'static str>,
    temp: f32,
    tint: f32,
    ev: f64,
    k: f64,
    li: f64,
    ti: f64,
    fl: f32,
    dy: f32,
    sa: f32,
    /// Layer mixing top / middle / bottom (as noted in the research)
    mix: Option<[f32; 3]>,
    s: [f32; 2],
    m: [f32; 2],
    l: [f32; 2],
    bal: f32,
    fade: f32,
    grain: [f32; 3],
    vig: [f32; 2],
    /// Tone curve (RGB), empty = none
    curve: Vec<[f32; 2]>,
}

impl P {
    fn build(self, name: &str, hint: &str, scope: Scope) -> Preset {
        let d = Settings::default();
        let mono = self.mix.is_some();
        let settings = Settings {
            monochrome: mono,
            color_mode: self.mode.map(String::from),
            exposure: self.ev,
            contrast: self.k,
            highlight: self.li,
            shadow: self.ti,
            fill_light: self.fl,
            vibrance: self.dy,
            saturation: self.sa,
            wb_temp: self.temp,
            wb_tint: self.tint,
            // The library notes top/middle/bottom; internally the order is bottom/middle/top
            mix: self.mix.map(|[o, m, u]| [u, m, o]).unwrap_or(d.mix),
            grade_shadows: if self.s[1] > 0.0 { self.s } else { d.grade_shadows },
            grade_midtones: if self.m[1] > 0.0 { self.m } else { d.grade_midtones },
            grade_highlights: if self.l[1] > 0.0 { self.l } else { d.grade_highlights },
            grade_balance: self.bal,
            fade: self.fade,
            grain_amount: self.grain[0],
            grain_size: if self.grain[0] > 0.0 { self.grain[1] } else { d.grain_size },
            grain_rough: if self.grain[0] > 0.0 { self.grain[2] } else { d.grain_rough },
            vignette: self.vig[0],
            vignette_mid: if self.vig[0] != 0.0 { self.vig[1] } else { d.vignette_mid },
            tone_curve: develop::ToneCurve { rgb: self.curve, ..Default::default() },
            ..d
        };
        Preset { name: name.into(), settings, scope, hint: hint.into() }
    }
}

/// Built-in library (cannot be deleted).
/// The presets shown in the "Presets" panel: without film looks (own section) and without the
/// black & white filters (shown as color dots at the black & white mix).
pub fn presets_library() -> Vec<Category> {
    library().into_iter().filter(|c| !matches!(c.name, "Film-Rezepte" | "Schwarzweiß-Filter")).collect()
}

pub fn library() -> Vec<Category> {
    use Scope::*;
    let look = |p: P, n: &str, h: &str| p.build(n, h, Look);
    let mix = |p: P, n: &str, h: &str| p.build(n, h, Mix);
    // Layer shares as with Sigma: 0 … 1, sum 1 (presets with small negative values are scaled back accordingly)
    let bw = |m: [f32; 3]| {
        let c = m.map(|v| v.max(0.0));
        let sum: f32 = c.iter().sum();
        Some(c.map(|v| (v / sum * 1000.0).round() / 1000.0))
    };
    vec![
        Category {
            name: "Film-Rezepte",
            presets: vec![
                film_recipe("ilford_hp5_plus", |f| { f.format_mm = 60.0; f.paper = Some("ilford_multigrade_fb_classic".into()); f.paper_grade = 3.0; f.taking_filter = "gelb15".into(); f.taking_filter_amount = 1.0; f.layer_exposure = true; f.grain_balance = true; }, P::default(), "Sigma-Analog", "Bei Sigma Merrill direkt aus den Foveon-Schichten belichtet, Korn statt Sensorrauschen; HP5 mit Gelbfilter auf Barytpapier"),
                film_recipe("kodak_portra_400", |f| { f.exposure = 0.7; f.print_warmth = 0.25; f.grain = 0.6; }, P::default(), "Portra · hell & warm", "Überbelichtetes Portra 400: luftig, weiche Haut, warme Lichter"),
                film_recipe("kodak_gold_200", |f| { f.print_exposure = -0.5; f.print_warmth = 0.4; f.paper_gamma = 1.15; f.grain = 0.5; }, P::default(), "Gold · warm & klar", "Gold 200, heller und warmer Abzug: satte Orange- und Brauntöne, warme Haut, sauberes Weiß, wenig Korn"),
                film_recipe("kodak_ektar_100", |f| { f.paper_gamma = 1.1; f.grain = 0.4; }, P::default(), "Ektar · kräftig", "Satte Farben, feines Korn – für Landschaft"),
                film_recipe("fujifilm_velvia_100", |f| { f.grain = 0.4; }, P::default(), "Velvia · Landschaft", "Diafilm mit sehr kräftigen Grün- und Blautönen"),
                film_recipe("kodak_kodachrome_64", |f| { f.exposure = -0.2; }, P::default(), "Kodachrome · klassisch", "Warmer, dichter Dia-Look"),
                film_recipe("kodak_vision3_500t", |f| { f.auto_balance = false; f.grain = 0.8; f.halation = 1.5; }, P::default(), "Vision3 500T · Nacht", "Kunstlicht-Kinofilm ungefiltert: kühl, Lichthöfe um Lichter"),
                film_recipe("kodak_trix_400", |f| { f.format_mm = 35.0; f.grain = 1.0; f.film_gamma = 1.15; }, P::default(), "Tri-X · Reportage", "Kleinbild-Korn und kräftiger Kontrast"),
                film_recipe("kodak_trix_400", |f| { f.format_mm = 35.0; f.film_dev = 11.0; f.paper_grade = 4.0; f.paper_matte = 1.7; f.grain = 1.2; }, P::default(), "Tri-X · hart & körnig", "Lange Entwicklung, hartes mattes Papier: tiefe Schatten, grobes Korn"),
                film_recipe("kodak_trix_400", |f| { f.format_mm = 35.0; f.film_dev = 6.0; f.paper_grade = 2.0; f.paper_matte = 1.5; }, P::default(), "Tri-X · weich & matt", "Kurze Entwicklung, weiches mattes Papier: lange Tonskala, angehobene Schwarztöne"),
                film_recipe("ilford_hp5_plus", |f| { f.format_mm = 35.0; f.paper_grade = 3.0; f.grain = 1.0; }, P::default(), "HP5 · Reportage", "Klassischer Kleinbild-Abzug auf Papier Grad 3"),
                film_recipe("ilford_delta_100", |f| { f.format_mm = 35.0; f.paper_grade = 3.0; f.grain = 0.8; }, P::default(), "Delta 100 · fein", "Feinkörnig und sauber, kräftiger Abzug"),
                film_recipe("fujifilm_acros_100_ii", |f| { f.format_mm = 35.0; f.paper_grade = 3.0; f.paper_gamma = 1.1; }, P::default(), "Acros · tief", "Sehr feines Korn, satte Schwarztöne"),
                film_recipe("kodak_tmax_400", |f| { f.format_mm = 35.0; f.paper_grade = 3.0; f.film_dev = 9.0; }, P::default(), "T-Max · klar", "Feines Korn, hohe Schärfe, kräftiger Kontrast"),
                film_recipe("agfa_scala_200x", |f| { f.format_mm = 35.0; f.film_dev = 400.0; f.paper_matte = 3.0; }, P::default(), "Scala · verblasst hart", "Hartes Schwarzweiß-Dia mit angehobenen Schwarztönen: klare Zeichnung, matter Schleier"),
                film_recipe("agfa_scala_200x", |f| { f.format_mm = 35.0; f.film_dev = 400.0; }, P::default(), "Scala · klar hart", "Push auf EI 400: sehr kontrastreiches, dichtes Schwarzweiß-Dia"),
                film_recipe("ilford_delta_100", |f| { f.format_mm = 35.0; f.paper_grade = 2.0; f.paper_matte = 2.5; }, P::default(), "Delta 100 · verblasst", "Weicher Abzug mit mattem, angehobenem Schwarz"),
                film_recipe("kodak_trix_400", |f| { f.format_mm = 35.0; f.paper = Some("kodak_portra_endura".into()); f.print_warmth = 0.28; f.paper_matte = 0.8; f.grain = 1.0; }, P::default(), "Warmton · fein", "Schwarzweißfilm auf Farbpapier mit einem Hauch Gelb: leicht warme Mitten und Lichter, die Tiefen bleiben neutral"),
                film_recipe("kodak_trix_400", |f| { f.format_mm = 35.0; f.paper = Some("kodak_portra_endura".into()); f.print_warmth = -0.28; f.paper_matte = 0.8; f.grain = 1.0; }, P::default(), "Kühlton · fein", "Schwarzweißfilm auf Farbpapier mit einem Hauch Kühle: leicht bläuliche Mitten und Lichter"),
                film_recipe("ilford_ortho_plus", |f| { f.format_mm=35.0; f.paper_grade=4.0; f.film_dev=10.0; }, P::default(), "Ortho · hart", "Orthochromatischer Film, lange Entwicklung und hartes Papier Grad 4"),
                film_recipe("ilford_delta_100", |f| { f.format_mm = 35.0; f.paper_grade = 4.0; }, P::default(), "Delta 100 · hart", "Fein und scharf auf hartem Papier: kräftige, klare Kontraste"),
                film_recipe("ilford_hp5_plus", |f| { f.format_mm = 35.0; f.paper_grade = 2.0; f.paper_matte = 2.5; }, P::default(), "HP5 · verblasst", "Weiches Korn, matter Abzug mit angehobenen Schwarztönen"),
                film_recipe("ilford_fp4_plus", |f| { f.format_mm = 35.0; f.paper_grade = 2.0; f.paper_matte = 2.5; }, P::default(), "FP4 · verblasst", "Fein und weich, matter Abzug mit angehobenen Schwarztönen"),
                film_recipe("ilford_delta_3200", |f| { f.format_mm = 35.0; f.film_dev = 12.0; f.paper_grade = 2.0; f.grain = 1.2; }, P::default(), "Delta 3200 · Nachtkorn", "Hohe Empfindlichkeit, grobes Korn, weicher Abzug"),
            ],
        },
        Category {
            name: "Schwarzweiß-Filter",
            presets: vec![
                mix(P { mix: bw([0.33, 0.33, 0.34]), ..P::default() }, "Ohne Filter (Summe)", "Alle drei Schichten gleich – Standard der Kamera"),
                mix(P { mix: bw([0.3, 0.45, 0.25]), ..P::default() }, "Gelbfilter", "Blau leicht gedämpft – Wolken werden sichtbar"),
                mix(P { mix: bw([0.15, 0.4, 0.45]), ..P::default() }, "Orangefilter", "Himmel dunkler, Haut glatter"),
                mix(P { mix: bw([-0.1, 0.25, 0.85]), ..P::default() }, "Rotfilter", "Dramatischer, fast schwarzer Himmel"),
                mix(P { mix: bw([0.25, 0.65, 0.1]), ..P::default() }, "Grünfilter", "Laub hell, Haut dunkler"),
                mix(P { mix: bw([0.95, 0.15, -0.1]), ..P::default() }, "Blaufilter", "Dunst und Atmosphäre betont"),
            ],
        },
        Category {
            name: "Foveon-Spezial",
            presets: vec![
                look(P { mix: bw([0.65, 0.45, -0.1]), k: 0.8, grain: [0.1, 0.8, 0.3], ..P::default() }, "Orthochromatisch", "Kein Rot: Lippen und Haut werden dunkel, Himmel hell"),
                look(
                    P { mix: bw([1.2, -0.2, 0.0]), k: 0.4, fade: 0.05, vig: [-0.4, 0.5], grain: [0.3, 1.2, 0.6], ..P::default() },
                    "Frühe Glasplatte",
                    "Nur blauempfindlich: Rot und Grün werden schwarz, Himmel weiß",
                ),
                look(
                    P { mix: bw([-0.2, 0.4, 0.8]), k: 1.0, fl: 0.5, vig: [-0.3, 0.5], grain: [0.6, 1.6, 0.7], ..P::default() },
                    "Infrarot-Look (HIE)",
                    "Simuliert: dunkler Himmel, untere Schicht betont (die Merrill hat einen IR-Sperrfilter)",
                ),
                look(P { mix: bw([1.0, 0.0, 0.0]), k: 0.3, ..P::default() }, "Top-Layer-Monochrom", "Nur die obere Schicht: rauschärmste, schärfste Schicht, fast panchromatisch – auch für hohe ISO"),
                look(
                    P { mix: bw([-0.2, 0.3, 0.9]), k: 0.8, ti: -0.3, vig: [-0.3, 0.5], grain: [0.2, 1.0, 0.4], ..P::default() },
                    "Untere Schicht (Rot/IR)",
                    "Die tiefste Schicht dominiert – Korn kaschiert ihr Rauschen",
                ),
                look(P { k: 0.3, fl: 0.4, li: -0.3, dy: 0.2, ..P::default() }, "Merrill-Pop", "Mikrokontrast und „3D“-Eindruck betont, Schatten mit Fill Light geöffnet"),
                look(P { mode: Some("FCBlue"), k: 0.4, sa: 0.1, grain: [0.1, 0.8, 0.3], ..P::default() }, "SD9-Klassik", "Die Anmutung der frühen Foveon-Kameras"),
            ],
        },
    ]
}

pub fn load() -> Vec<Preset> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(presets: &[Preset]) {
    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(presets) {
            let _ = std::fs::write(p, json);
        }
    }
}

/// Preset with film: profile ID, film fine settings and remaining sliders (scope look).
fn film_recipe(stock: &str, tune: impl Fn(&mut develop::film::FilmSettings), p: P, name: &str, hint: &str) -> Preset {
    let mut preset = p.build(name, hint, Scope::Look);
    let f = &mut preset.settings.film;
    f.active = true;
    f.stock = stock.to_string();
    tune(f);
    preset
}

#[cfg(test)] mod tool_recipe_tests {
 use super::*;
 #[test] fn new_darkroom_fields_survive_recipe_exchange(){
 let f=develop::film::FilmSettings{active:true,stock:"ilford_ortho_plus".into(),preflash:0.25,preflash_warmth:0.2,optics:"soft".into(),camera_blur_um:Some(4.),diffusion:"mist".into(),diffusion_amount:0.3,diffusion_enlarger:true,halation_type:"red".into(),scan_black:0.02,scan_white:0.9,scan_auto:true,..Default::default()};
 let p=recipe("Probe",&f);let text=recipe_json(&p);assert_eq!(read_recipe(&text).unwrap().film_recipe(),f);
 let old=read_recipe(r#"{"format":"graineo-recipe","version":1,"name":"Alt","film":{"active":true,"stock":"ilford_hp5_plus"}}"#).unwrap();assert_eq!(old.settings.film.preflash,0.);assert_eq!(old.settings.film.scan_white,1.);assert!(old.settings.film.optics.is_empty());
 }
}
