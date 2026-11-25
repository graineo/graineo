//! Search field of the settings: while it holds text, the right side shows every matching row of every
//! section instead of one section, grouped under a small section heading. The rows are the real controls.
//!
//! There is no separate register of the rows: in search mode each section is drawn once into an invisible
//! `Ui` (nothing visible, nothing clickable, so nothing is drawn twice and no work starts twice), and
//! [`super::row`] asks [`check`] first. A row that matches is drawn into a fresh `Ui` placed in the visible
//! result list; a row that does not match is skipped. Everything else of a section (hints, separators,
//! group frames) stays in the invisible pass.
//!
//! Searched text per row: label, hint line, the titles of its section and group, all in the shown language,
//! as German key and in English, plus the search words of [`KEYWORDS`]. Case, umlauts and accents do not
//! matter ("oberflache" finds "Oberfläche").
//!
//! Parts without rows (the sections Looks and Camera defaults with their own list layouts, the stage cards of the
//! search model) are searched as one block ([`block`]): by their title, section and search words. A matching block
//! is drawn once, whole, into the results; a block that does not match is not drawn at all in the search pass (its
//! drawing starts work such as reading the profile list or the model state, which must not run twice).

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// Search words per label (German key of a row, group or section label); search data only, never shown, so
/// German and English words are listed together.
const KEYWORDS: &[(&str, &str)] = &[
    ("Erscheinungsbild", "dark mode light mode dunkel hell dunkelmodus theme thema farben colors colours appearance"),
    ("Größe der Oberfläche", "zoom skalierung scale scaling schriftgröße font size groß klein bigger smaller"),
    ("Schrift", "font typeface schriftart sans mono"),
    ("Kontrast", "contrast lesbarkeit readability barrierefrei accessibility"),
    ("Sprache", "language deutsch english japanese français español"),
    ("Speicher", "cache zwischenspeicher speicherplatz disk storage space platz"),
    ("Betriebsart", "lüfter lüftung leise laut fan quiet noise akku battery strom energie sparsam power performance leistung"),
    ("Grafikkarte verwenden", "gpu metal graphics grafik"),
    ("Automatisch nach Updates suchen", "update aktualisierung aktualisieren version"),
    ("Sicherung", "backup sicherungskopie nas restore"),
    ("Begleitdateien (XMP) neben die Fotos schreiben", "xmp sidecar metadaten metadata"),
    ("Gesichter automatisch erkennen", "personen people faces gesicht face"),
    ("JPEG-Qualität", "kompression compression quality"),
    ("Objektivkorrektur standardmäßig an", "lens correction verzeichnung distortion vignettierung vignette"),
    ("Papierkorb auf Netzlaufwerken", "trash löschen delete nas netzwerk network smb"),
    ("Browser", "tablet ipad remote fernzugriff web"),
];

/// Lower case without umlauts and accents (ä → a, ß → ss, é → e), for comparisons.
pub(super) fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let base = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => "a",
            'æ' => "ae",
            'ç' | 'ć' | 'č' => "c",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ñ' | 'ń' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => "o",
            'œ' => "oe",
            'ù' | 'ú' | 'û' | 'ü' | 'ū' => "u",
            'ý' | 'ÿ' => "y",
            'ß' => "ss",
            'ł' => "l",
            'š' => "s",
            'ž' | 'ź' | 'ż' => "z",
            _ => {
                out.push(c);
                continue;
            }
        };
        out.push_str(base);
    }
    out
}

/// Words of a query, normalized; all must occur in a row's text.
pub(super) fn tokens(query: &str) -> Vec<String> {
    normalize(query).split_whitespace().map(str::to_string).collect()
}

/// Every form of a shown text: itself, its German key and its English text (see [`i18n::search_forms`]).
fn forms(shown: &str) -> Vec<String> {
    let mut v = crate::menu::i18n::search_forms(shown);
    if let std::borrow::Cow::Owned(t) = crate::menu::i18n::tr_msg(shown) {
        v.push(t);
    }
    v
}

/// Normalized searchable text of a row: all forms of the label, hint, group and section plus their search words.
pub(super) fn haystack(parts: &[&str]) -> String {
    let mut all: Vec<String> = Vec::new();
    for p in parts.iter().filter(|p| !p.is_empty()) {
        all.extend(forms(p));
    }
    let mut words = Vec::new();
    for (label, w) in KEYWORDS {
        if all.iter().any(|f| f == label) {
            words.push(*w);
        }
    }
    let mut s = String::new();
    for p in all.iter().map(String::as_str).chain(words) {
        s.push_str(&normalize(p));
        s.push('\n');
    }
    s
}

pub(super) fn matches(hay: &str, tokens: &[String]) -> bool {
    !tokens.is_empty() && tokens.iter().all(|t| hay.contains(t.as_str()))
}

/// One search pass over all sections (one frame).
struct Pass {
    tokens: Vec<String>,
    /// layer of the settings window: rows of other layers (dialogs opened from a section) are left alone
    layer: egui::LayerId,
    clip: egui::Rect,
    style: std::sync::Arc<egui::Style>,
    left: f32,
    width: f32,
    /// top of the next result
    y: f32,
    section: usize,
    group: String,
    /// last heading drawn (section, group)
    heading: Option<(usize, String)>,
    hits: [usize; 12],
    /// a matching row is being drawn: rows inside it are drawn normally
    inside: bool,
    rows: usize,
    /// shape slot reserved before a row, filled with its box once its height is known (box behind the controls)
    slot: Option<egui::layers::ShapeIdx>,
}

thread_local! {
    static PASS: RefCell<Option<Pass>> = const { RefCell::new(None) };
    /// haystack per (section, group, label, hint); emptied when the language changes
    static CACHE: RefCell<(u64, HashMap<(usize, String, String, String), String>)> = RefCell::new((u64::MAX, HashMap::new()));
}

/// Hits per section of the last pass (the list on the left is drawn before the pass).
static HITS: Mutex<[usize; 12]> = Mutex::new([0; 12]);

pub(super) fn hits() -> [usize; 12] {
    *HITS.lock().unwrap()
}

/// What [`super::row`] does with a row.
pub(super) enum Check {
    /// no search running: draw as usual
    Normal,
    /// search running, row does not match
    Skip,
    /// draw the row into this `Ui`, then call [`done`]
    Draw(egui::Ui),
}

/// Is a search pass drawing `ui` (the invisible pass of a section, not a matching row or a dialog)?
pub(super) fn active(ui: &egui::Ui) -> bool {
    PASS.with(|p| p.borrow().as_ref().is_some_and(|p| !p.inside && p.layer == ui.layer_id()))
}

/// Group title of the rows that follow (called by [`super::group`] in search mode).
pub(super) fn set_group(title: &str) {
    PASS.with(|p| {
        if let Some(p) = p.borrow_mut().as_mut() {
            p.group = title.to_string();
        }
    });
}

pub(super) fn check(ui: &egui::Ui, name: &str, hint: Option<&str>) -> Check {
    check_with(ui, name, hint, "")
}

/// [`check`] with extra search words (`words`, searched as given, not translated).
fn check_with(ui: &egui::Ui, name: &str, hint: Option<&str>, words: &str) -> Check {
    PASS.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(p) = b.as_mut() else { return Check::Normal };
        if p.inside || ui.layer_id() != p.layer {
            return Check::Normal;
        }
        let key = (p.section, p.group.clone(), name.to_string(), format!("{}\n{words}", hint.unwrap_or("")));
        let hay = CACHE.with(|c| {
            let mut c = c.borrow_mut();
            let gen = crate::menu::i18n::generation();
            if c.0 != gen {
                *c = (gen, HashMap::new());
            }
            c.1.entry(key)
                .or_insert_with(|| haystack(&[name, hint.unwrap_or(""), &p.group, localized::area_label(p.section), SECTIONS[p.section]]) + &normalize(words))
                .clone()
        });
        if !matches(&hay, &p.tokens) {
            return Check::Skip;
        }
        p.hits[p.section] += 1;
        let painter = ui.ctx().layer_painter(p.layer).with_clip_rect(p.clip);
        // small heading "SECTION · GROUP" once per group with hits
        let here = (p.section, p.group.clone());
        if p.heading.as_ref() != Some(&here) {
            let title = if p.group.is_empty() || p.group == name {
                // a block that is a whole group or section: its title is the heading
                if p.group.is_empty() { localized::area_label(p.section).to_string() } else { format!("{} · {}", localized::area_label(p.section), crate::menu::i18n::tr_msg(&p.group)) }
            } else if p.group.is_empty() {
                localized::area_label(p.section).to_string()
            } else {
                format!("{} · {}", localized::area_label(p.section), crate::menu::i18n::tr_msg(&p.group))
            };
            p.y += if p.heading.is_some() { 10.0 } else { 2.0 };
            let g = lr::caps(&painter, &title, lr::semibold(10.5), lr::TEXT_DIM());
            let h = g.size().y;
            painter.galley(egui::pos2(p.left + 4.0, p.y), g, lr::TEXT_DIM());
            p.y += h + 4.0;
            p.heading = Some(here);
        }
        p.rows += 1;
        p.inside = true;
        p.slot = Some(painter.add(egui::Shape::Noop));
        let rect = egui::Rect::from_min_size(egui::pos2(p.left + 12.0, p.y + 3.0), egui::vec2(p.width - 24.0, 4000.0));
        let id = egui::Id::unique(("prefs-search-row", p.section, &p.group, name, p.rows));
        let mut t = egui::Ui::new(ui.ctx().clone(), id, egui::UiBuilder::new().layer_id(p.layer).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)).style(p.style.clone()));
        t.set_clip_rect(p.clip);
        t.spacing_mut().item_spacing.y = 2.0;
        Check::Draw(t)
    })
}

/// A matching row is drawn: box behind it, move on.
pub(super) fn done(t: &egui::Ui) {
    PASS.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(p) = b.as_mut() else { return };
        p.inside = false;
        let bottom = t.min_rect().bottom() + 3.0;
        let r = egui::Rect::from_min_max(egui::pos2(p.left, p.y), egui::pos2(p.left + p.width, bottom));
        let painter = t.ctx().layer_painter(p.layer).with_clip_rect(p.clip);
        if let Some(slot) = p.slot.take() {
            painter.set(slot, egui::epaint::RectShape::new(r, 0.0, crate::lr::gray(0x1d), egui::Stroke::new(1.0, crate::lr::gray(0x2c)), egui::StrokeKind::Inside));
        }
        p.y = bottom + 2.0;
    });
}

/// Sections without rows of [`super::row`] (own list layouts): searched as one block each (see [`block`]).
pub(super) const BLOCK_SECTIONS: [usize; 2] = [9, 11];

/// Search words of the block sections (German and English together, search data only).
fn section_words(section: usize) -> &'static str {
    match section {
        9 => "film filme filmprofil filmprofile profile profiles look looks filmsimulation film simulation negativ negative papier paper schwarzweiss black white farbfilm color film import importieren eigene own",
        11 => "kamera kameras camera cameras standard standards default defaults startwerte start values vorgabe vorgaben preset presets objektiv lens rauschen noise schärfen sharpen profil profile",
        _ => "",
    }
}

/// A part without rows (`title`, e.g. a group of cards) with its search `words`: drawn as usual outside a search;
/// in a search pass drawn whole into the results when it matches, otherwise not drawn at all.
pub(super) fn block(ui: &mut egui::Ui, title: &str, words: &str, draw: impl FnOnce(&mut egui::Ui)) {
    if !active(ui) {
        draw(ui);
        return;
    }
    match check_with(ui, title, None, words) {
        Check::Normal => draw(ui),
        Check::Skip => {}
        Check::Draw(mut target) => {
            draw(&mut target);
            done(&target);
        }
    }
}

/// Draw the results of `query` into `ui` (inside the scroll area); `draw(ui, section)` draws one section.
/// Returns the number of hits.
pub(super) fn run(ui: &mut egui::Ui, query: &str, mut draw: impl FnMut(&mut egui::Ui, usize)) -> usize {
    let top = ui.cursor().top();
    let full = ui.max_rect();
    PASS.with(|p| {
        *p.borrow_mut() = Some(Pass {
            tokens: tokens(query),
            layer: ui.layer_id(),
            clip: ui.clip_rect(),
            style: ui.style().clone(),
            left: full.left(),
            width: ui.available_width(),
            y: top,
            section: 0,
            group: String::new(),
            heading: None,
            hits: [0; 12],
            inside: false,
            rows: 0,
            slot: None,
        })
    });
    for &i in ORDER.iter() {
        PASS.with(|p| {
            if let Some(p) = p.borrow_mut().as_mut() {
                p.section = i;
                p.group.clear();
            }
        });
        let mut hidden = ui.new_child(egui::UiBuilder::new().id_salt(("prefs-search-pass", i)).max_rect(full).layout(egui::Layout::top_down(egui::Align::Min)).invisible());
        if BLOCK_SECTIONS.contains(&i) {
            // the whole section is one block: shown under its heading when its name or search words match
            block(&mut hidden, "", section_words(i), |ui| draw(ui, i));
        } else {
            draw(&mut hidden, i);
        }
    }
    let pass = PASS.with(|p| p.borrow_mut().take()).expect("search pass");
    let total: usize = pass.hits.iter().sum();
    *HITS.lock().unwrap() = pass.hits;
    let height = (pass.y - top).max(0.0);
    ui.allocate_space(egui::vec2(ui.available_width(), height));
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_case_umlauts_and_accents() {
        assert_eq!(normalize("Oberfläche"), "oberflache");
        assert_eq!(normalize("GRÖSSE"), "grosse");
        assert_eq!(normalize("Größe"), "grosse");
        assert_eq!(normalize("Français Español"), "francais espanol");
        assert_eq!(normalize("表示"), "表示");
    }

    #[test]
    fn tokens_all_must_match() {
        let hay = haystack(&["Kontrast", "", "Darstellung", "Oberfläche"]);
        assert!(matches(&hay, &tokens("oberflache")));
        assert!(matches(&hay, &tokens("KONTRAST ober")));
        assert!(!matches(&hay, &tokens("kontrast xyz")));
        assert!(!matches(&hay, &tokens("   ")));
    }

    #[test]
    fn english_and_keywords_are_searched() {
        // German UI (tests run in German): the English text of the key is searched as well
        let hay = haystack(&["Schrift", "", "", ""]);
        assert!(matches(&hay, &tokens("font")));
        let hay = haystack(&["Erscheinungsbild", "", "", ""]);
        assert!(matches(&hay, &tokens("dark")));
        assert!(matches(&hay, &tokens("dunkel")));
        let hay = haystack(&["Betriebsart", "", "", ""]);
        assert!(matches(&hay, &tokens("lüfter")));
        assert!(matches(&hay, &tokens("leise")));
        // a row of the section "Speicher" is found with "cache"
        let hay = haystack(&["Belegt", "", "", "Speicher"]);
        assert!(matches(&hay, &tokens("cache")));
        // hint lines are searched too, also in English
        let hay = haystack(&["Verlauf speichern", "Schritte jeder Bearbeitung, nur in der Bibliothek auf diesem Rechner. Aus: nur bis zum Beenden.", "", ""]);
        assert!(matches(&hay, &tokens("bearbeitung")));
    }

    #[test]
    fn block_sections_and_cards_are_found_by_name_and_words() {
        // section Looks (9): by its name and its words, German and English
        let hay = haystack(&["", "", "", localized::area_label(9), SECTIONS[9]]) + &normalize(section_words(9));
        for q in ["looks", "filmprofil", "film profile", "papier"] {
            assert!(matches(&hay, &tokens(q)), "Looks: {q}");
        }
        // section Camera defaults (11)
        let hay = haystack(&["", "", "", localized::area_label(11), SECTIONS[11]]) + &normalize(section_words(11));
        for q in ["kamera-standards", "camera defaults", "startwerte"] {
            assert!(matches(&hay, &tokens(q)), "Kamera-Standards: {q}");
        }
        assert!(!matches(&hay, &tokens("xmp")));
        // the search model cards: group title, stage names and words
        let words = super::super::search_model_words();
        let hay = haystack(&["Suchmodell", "", "Suchmodell", localized::area_label(6), SECTIONS[6]]) + &normalize(&words);
        for q in ["suchmodell", "search model", "mehrsprachig", "beste treffer", "clip"] {
            assert!(matches(&hay, &tokens(q)), "Suchmodell: {q}");
        }
    }

    #[test]
    fn every_keyword_label_is_a_known_text() {
        // a label renamed in the code would silently lose its search words
        let en = crate::menu::i18n::search_forms;
        for (label, _) in KEYWORDS {
            let fluent = crate::menu::i18n::Text::ALL.iter().any(|&t| crate::menu::i18n::text_in(crate::menu::i18n::Language::De, t) == *label);
            assert!(fluent || en(label).len() > 1, "search words for unknown label „{label}“");
        }
    }
}
