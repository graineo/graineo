//! Data for the darkroom: info cards for films and papers (characteristic curves, spectral sensitivity,
//! character text), taking filters and push/pull.
//!
//! The characteristic curves and spectra come from the profile data (manufacturer datasheets), the text and the
//! key data from the profile field `info.card`.

use super::{load_profile, stock};

/// A curve for display: label and points (x, y).
#[derive(Clone, Debug)]
pub struct Series {
    pub name: String,
    pub points: Vec<(f32, f32)>,
}

/// Info card of a film or paper.
#[derive(Clone, Debug)]
pub struct InfoCard {
    pub id: String,
    pub name: String,
    pub maker: String,
    /// Type, e.g. "black-and-white negative film (T-grain)"
    pub type_name: String,
    /// Nominal speed (ISO); `None` for paper
    pub iso: Option<f32>,
    /// RMS granularity according to the datasheet (density 1.0, 48 µm aperture), if given
    pub rms: Option<f32>,
    /// Additional information (e.g. source of the resolution or contrast grades)
    pub additional: String,
    pub text: String,
    pub source: String,
    pub is_bw: bool,
    pub is_paper: bool,
    /// Characteristic curves: film per development time (label "9 min"), paper per contrast grade
    /// ("Grad 3", "Grad 00"); x = log exposure relative to middle grey, y = density (net)
    pub characteristic_curves: Vec<Series>,
    /// Spectral sensitivity: x = wavelength in nm, y = relative log sensitivity (maximum 0)
    pub spectrum: Vec<Series>,
    /// largest density of all characteristic curves
    pub dmax: f32,
    /// Note on spectral sensitivity (computed from the profile data): ortho-/panchromatic, red cut-off
    pub spectral_note: String,
    /// Built from datasheets (instead of a profile from spektrafilm)
    pub custom: bool,
    /// Development process ("C-41", "E-6" …)
    pub process: String,
    /// Introduction/end ("eingeführt 2010", "eingestellt 2012")
    pub period: String,
    /// typical uses
    pub suitable: Vec<String>,
    /// less suitable for
    pub less_suitable: Vec<String>,
    /// Hints on use in graineo
    pub tips: Vec<String>,
    /// Facts (text, source), only documented ones
    pub facts: Vec<(String, String)>,
}

/// Built from datasheets: profile with a card that is not explicitly marked as "spektrafilm".
pub fn is_custom_built(info: &spektrafilm_core::profile::ProfileInfo) -> bool {
    info.card.as_ref().is_some_and(|c| {
        let mut c = c.clone();
        legacy_names::settings_keys::upgrade_film_card(&mut c);
        c.get("origin").and_then(|v| v.as_str()) != Some("spektrafilm")
    })
}

fn grade_name(g: f64) -> String {
    match g {
        x if x == 0.0 => "Grad 00".into(),
        x if x == 0.5 => "Grad 0".into(),
        x => format!("Grad {x}"),
    }
}

/// Info card for a profile ID (film or paper); `None` if the profile is missing.
pub fn card(id: &str) -> Option<InfoCard> {
    let info = stock(id);
    let p = load_profile(id).ok()?;
    // cards of graineo 1.0 (own profiles in the data folder) have old keys
    let mut c = p.info.card.clone().unwrap_or_default();
    legacy_names::settings_keys::upgrade_film_card(&mut c);
    let s = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| c.get(k).and_then(|v| v.as_f64()).map(|v| v as f32);
    let str_list = |k: &str| -> Vec<String> { c.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    let is_paper = p.is_paper() || p.info.stage == "printing";
    let bw = p.is_bw();
    let le = &p.data.log_exposure;
    let advance = 4usize;
    let points = |col: usize| -> Vec<(f32, f32)> {
        le.iter().enumerate().step_by(advance).filter_map(|(i, &x)| p.data.density_curves.get(i).and_then(|r| r.get(col)).map(|&d| (x as f32, d as f32))).collect()
    };
    let mut characteristic_curves = Vec::new();
    if bw {
        for (k, &t) in p.data.development_time.iter().enumerate() {
            let name = if is_paper { grade_name(t) } else { format!("{t} min") };
            characteristic_curves.push(Series { name, points: points(k) });
        }
        if characteristic_curves.is_empty() {
            characteristic_curves.push(Series { name: "Kennlinie".into(), points: points(0) });
        }
    } else {
        for (k, n) in ["Rot", "Grün", "Blau"].iter().enumerate() {
            characteristic_curves.push(Series { name: (*n).into(), points: points(k) });
        }
    }
    let mut spectrum = Vec::new();
    let channels = if bw { 1 } else { 3 };
    let labels = if bw { vec!["Empfindlichkeit"] } else { vec!["Rot", "Grün", "Blau"] };
    // Outside the measured range the profile contains zeros (missing values) that do not count
    let is_valid = |v: &f64| v.is_finite() && *v != 0.0;
    let max = p.data.log_sensitivity.iter().flat_map(|r| r.iter().take(channels)).copied().filter(is_valid).fold(f64::MIN, f64::max);
    for k in 0..channels {
        let pts: Vec<(f32, f32)> = p
            .data
            .wavelengths
            .iter()
            .zip(&p.data.log_sensitivity)
            .filter_map(|(&w, r)| r.get(k).copied().filter(is_valid).map(|v| (w as f32, (v - max) as f32)))
            .collect();
        if !pts.is_empty() {
            spectrum.push(Series { name: labels[k].into(), points: pts });
        }
    }
    let spectral_note = spectral_note(&p.data.wavelengths, &p.data.log_sensitivity, bw && !is_paper);
    let dmax = characteristic_curves.iter().flat_map(|r| r.points.iter().map(|p| p.1)).fold(0.0f32, f32::max);
    Some(InfoCard {
        id: id.to_string(),
        name: info.map(|i| i.name.clone()).or_else(|| p.info.name.clone()).unwrap_or_else(|| id.to_string()),
        maker: s("maker"),
        type_name: s("type"),
        iso: f("iso"),
        rms: f("rms"),
        additional: s("additional"),
        text: s("text"),
        source: s("source"),
        is_bw: bw,
        is_paper,
        characteristic_curves,
        spectrum,
        dmax,
        spectral_note,
        custom: is_custom_built(&p.info),
        process: s("process"),
        period: s("period"),
        suitable: str_list("suitable"),
        less_suitable: str_list("less_suitable"),
        tips: str_list("tips"),
        facts: c.get("facts").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| Some((x.get("text")?.as_str()?.to_string(), x.get("source").and_then(|q| q.as_str()).unwrap_or("").to_string()))).collect()).unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------------------------
// Taking filters

/// A filter in front of the lens.
#[derive(Clone, Copy, Debug)]
pub struct FilterInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// short effect description for the help
    pub effect: &'static str,
}

const FILTER: &[FilterInfo] = &[
    FilterInfo { id: "gelb8", name: "Gelb (8)", effect: "Himmel und Wolken leicht abgesetzt, Haut kaum verändert" },
    FilterInfo { id: "gelb15", name: "Tiefgelb (15)", effect: "Himmel deutlich dunkler, mehr Wolkenzeichnung" },
    FilterInfo { id: "orange21", name: "Orange (21)", effect: "Himmel dunkel, Haut glatt und hell, Rot und Gelb heller" },
    FilterInfo { id: "rot25", name: "Rot (25)", effect: "Dramatischer, fast schwarzer Himmel; Haut sehr hell, Blau sehr dunkel" },
    FilterInfo { id: "rot29", name: "Tiefrot (29)", effect: "Extrem; Himmel schwarz, Laub hell wie Infrarot" },
    FilterInfo { id: "gruen11", name: "Grün (11)", effect: "Laub hell, Haut etwas dunkler und plastischer" },
    FilterInfo { id: "blau47", name: "Blau (47)", effect: "Dunst und Atmosphäre betont, Rot sehr dunkel" },
];

/// All taking filters (the curves are approximations based on the usual position of the edges).
pub fn filter_list() -> &'static [FilterInfo] {
    FILTER
}

fn long_pass(w: f64, edge: f64, width: f64) -> f64 {
    1.0 / (1.0 + (-(w - edge) / width).exp())
}

/// Transmittance of a taking filter at `nm` (0…1); unknown ID = 1.
pub fn filter_transmission(id: &str, nm: f64) -> f64 {
    match id {
        "gelb8" => 0.92 * long_pass(nm, 482.0, 7.0),
        "gelb15" => 0.92 * long_pass(nm, 515.0, 7.0),
        "orange21" => 0.92 * long_pass(nm, 560.0, 8.0),
        "rot25" => 0.92 * long_pass(nm, 598.0, 8.0),
        "rot29" => 0.92 * long_pass(nm, 618.0, 8.0),
        "gruen11" => 0.72 * long_pass(nm, 490.0, 12.0) * (1.0 - long_pass(nm, 590.0, 14.0)),
        "blau47" => 0.62 * (-((nm - 440.0) / 32.0).powi(2)).exp(),
        _ => 1.0,
    }
}

/// Contrast grade a film gets on this paper while the paper grade is set to "Std"
/// (default look of the film; Multigrade papers otherwise grade 3). `None`: paper without a grade preset
/// (then the middle step of the paper's family applies).
pub fn default_paper_grade(film: &str, paper: &str) -> Option<f32> {
    let look = load_profile(film).ok().and_then(|p| p.info.default_look);
    let multigrade = paper.starts_with("ilford_multigrade");
    let fits_look = multigrade || look.as_ref().and_then(|l| l.paper.as_deref()) == Some(paper);
    if !fits_look {
        return None;
    }
    look.and_then(|l| l.paper_grade).map(|g| g as f32).or(multigrade.then_some(3.0))
}

// ---------------------------------------------------------------------------------------------
// Push/Pull

/// Development time in minutes for `push_stops` stops push (+) or pull (−): about 30 % longer or
/// shorter per stop, starting from the film's standard time (`standard`, otherwise the middle one of the
/// profile's time series). The result is only a request; the film computation takes the nearest
/// time of the datasheet series.
pub fn push_time(film: &str, standard: Option<f64>, push_stops: f32) -> Option<f64> {
    let p = load_profile(film).ok()?;
    let t = &p.data.development_time;
    if t.is_empty() {
        return None;
    }
    let basis = standard.unwrap_or(t[(t.len() - 1) / 2]);
    Some(basis * (1.0 + 0.3 * push_stops as f64).max(0.3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_of_bw_film() {
        let Some(k) = card("fujifilm_acros_100_ii") else { return }; // nothing to check without film data
        assert!(k.is_bw && !k.is_paper);
        assert_eq!(k.iso, Some(100.0));
        assert_eq!(k.rms, Some(7.0));
        assert!(!k.text.is_empty());
        assert!(!k.characteristic_curves.is_empty() && k.characteristic_curves.iter().all(|r| r.points.len() > 20));
        assert!(!k.spectrum.is_empty() && k.spectrum[0].points.iter().any(|p| p.1 == 0.0));
        assert!(k.dmax > 1.5);
    }

    #[test]
    fn card_of_paper_with_grades() {
        let Some(k) = card("ilford_multigrade_fb_classic") else { return };
        assert!(k.is_paper && k.iso.is_none());
        assert_eq!(k.characteristic_curves.len(), 7);
        assert_eq!(k.characteristic_curves[0].name, "Grad 00");
        assert_eq!(k.characteristic_curves[1].name, "Grad 0");
    }

    #[test]
    fn filters_pass_expected_light() {
        for (id, red_bright) in [("orange21", true), ("rot25", true), ("gelb8", true)] {
            assert!(filter_transmission(id, 650.0) > 0.8, "{id}");
            assert!(filter_transmission(id, 420.0) < 0.05, "{id}");
            assert!(red_bright);
        }
        assert!(filter_transmission("blau47", 440.0) > 0.5 && filter_transmission("blau47", 640.0) < 0.01);
        assert!(filter_transmission("gruen11", 540.0) > 0.6 && filter_transmission("gruen11", 650.0) < 0.05);
        assert_eq!(filter_transmission("", 500.0), 1.0);
        assert_eq!(filter_list().len(), 7);
    }

    #[test]
    fn default_grade_per_film_and_paper() {
        // Multigrade without its own preset: grade 3; without profile data there is no value (not a failure)
        if card("kodak_trix_400").is_some() {
            assert_eq!(default_paper_grade("kodak_trix_400", "ilford_multigrade_iv_rc"), Some(3.0));
            assert_eq!(default_paper_grade("kodak_trix_400", "kodak_2302"), None);
        }
        if card("ilford_delta_400").is_some() {
            assert_eq!(default_paper_grade("ilford_delta_400", "ilford_multigrade_fb_classic"), Some(4.0));
        }
    }

    #[test]
    fn push_extends_development() {
        if let (Some(a), Some(b)) = (push_time("kodak_trix_400", Some(9.0), 1.0), push_time("kodak_trix_400", Some(9.0), -1.0)) {
            assert!(a > 9.0 && b < 9.0);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Speed and development (display in the darkroom)

/// Effective speed (EI) and development time of a film with the current settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevelopmentInfo {
    /// Exposure index: nominal value × 2^push (for reversal film the selected EI of the series)
    pub ei: Option<f32>,
    /// Development time in minutes (nearest time of the datasheet series); `None` for reversal film
    pub minutes: Option<f64>,
}

/// Effective EI and development time for the settings (only meaningful for films with a time series).
pub fn development_info(fs: &super::FilmSettings) -> Option<DevelopmentInfo> {
    if !fs.is_active() {
        return None;
    }
    let p = load_profile(&fs.stock).ok()?;
    let series = &p.data.development_time;
    let nominal = p.info.card.as_ref().and_then(|c| c.get("iso")).and_then(|v| v.as_f64()).map(|v| v as f32);
    let look_dev = if fs.look_datasheet { None } else { p.info.default_look.as_ref().and_then(|l| l.film_dev) };
    let desired = if fs.film_dev > 0.0 {
        Some(fs.film_dev as f64)
    } else if fs.push != 0.0 {
        push_time(&fs.stock, look_dev, fs.push.clamp(-2.0, 3.0))
    } else {
        look_dev.or_else(|| (!series.is_empty()).then(|| series[(series.len() - 1) / 2]))
    };
    let nearest = |w: f64| series.iter().copied().min_by(|a, b| (a - w).abs().total_cmp(&(b - w).abs())).unwrap_or(w);
    let picked = desired.map(|w| if series.is_empty() { w } else { nearest(w) });
    if p.is_positive() {
        // Reversal film: the series are the speeds
        return Some(DevelopmentInfo { ei: picked.map(|w| w as f32), minutes: None });
    }
    Some(DevelopmentInfo { ei: nominal.map(|n| n * 2f32.powf(fs.push.clamp(-2.0, 3.0))), minutes: picked })
}

#[cfg(test)]
mod development_tests {
    use super::*;
    use crate::film::FilmSettings;

    #[test]
    fn ei_follows_push() {
        let mut fs = FilmSettings { active: true, stock: "kodak_trix_400".into(), ..FilmSettings::default() };
        let Some(a) = development_info(&fs) else { return };
        assert_eq!(a.ei, Some(400.0));
        fs.push = 1.0;
        let b = development_info(&fs).unwrap();
        assert_eq!(b.ei, Some(800.0));
        assert!(b.minutes.unwrap() > a.minutes.unwrap(), "push lengthens development");
        fs.push = 0.0;
        fs.film_dev = 6.0;
        assert_eq!(development_info(&fs).unwrap().minutes, Some(6.0));
        assert_eq!(development_info(&FilmSettings::default()), None);
    }

    #[test]
    fn reversal_film_shows_series_ei() {
        let fs = FilmSettings { active: true, stock: "agfa_scala_200x".into(), film_dev: 400.0, ..FilmSettings::default() };
        let Some(i) = development_info(&fs) else { return };
        assert_eq!(i.ei, Some(400.0));
        assert_eq!(i.minutes, None);
    }

    #[test]
    fn spectral_note_distinguishes_ortho_and_pan() {
        let wl: Vec<f64> = (380..=700).step_by(5).map(|v| v as f64).collect();
        // flat up to 650 nm, then dropping steeply
        let pan: Vec<Vec<f64>> = wl.iter().map(|&w| vec![if w <= 650.0 { 0.0 } else { -3.0 }]).collect();
        // ortho: drop already at 580 nm
        let ortho: Vec<Vec<f64>> = wl.iter().map(|&w| vec![if w <= 580.0 { 0.0 } else { -3.0 }]).collect();
        assert!(spectral_note(&wl, &pan, true).starts_with("Panchromatisch"));
        assert!(spectral_note(&wl, &ortho, true).starts_with("Orthochromatisch"));
        assert_eq!(spectral_note(&wl, &pan, false), "");
    }
}

/// Ortho- or panchromatic? From the spectral sensitivity (first layer): red cut-off = longest
/// wavelength at which the sensitivity is at most one order of magnitude below the maximum;
/// plus red (600–650 nm) compared to green (500–560 nm) in stops.
fn spectral_note(wl: &[f64], ls: &[Vec<f64>], bw: bool) -> String {
    if !bw || wl.is_empty() {
        return String::new();
    }
    let points: Vec<(f64, f64)> = wl.iter().zip(ls).filter_map(|(&w, r)| r.first().copied().filter(|v| v.is_finite()).map(|v| (w, v))).collect();
    let Some(max) = points.iter().map(|p| p.1).reduce(f64::max) else { return String::new() };
    let cutoff = points.iter().filter(|p| p.1 >= max - 1.0).map(|p| p.0).fold(0.0, f64::max);
    let avg = |a: f64, b: f64| {
        let v: Vec<f64> = points.iter().filter(|p| p.0 >= a && p.0 <= b).map(|p| p.1).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    let red_vs_green = match (avg(600.0, 650.0), avg(500.0, 560.0)) {
        (Some(r), Some(g)) => Some((r - g) * 3.32),
        _ => None,
    };
    let kind = if cutoff < 610.0 { "Orthochromatisch: nicht rot-empfindlich" } else if cutoff > 690.0 { "Panchromatisch, bis ins nahe Infrarot empfindlich" } else { "Panchromatisch" };
    let red_note = red_vs_green.map_or(String::new(), |b| format!("; Rot gegenüber Grün {b:+.1} Blenden"));
    format!("{kind} (Empfindlichkeit reicht bis etwa {cutoff:.0} nm{red_note})")
}
