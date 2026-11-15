//! Translation of the German UI texts: the German original is the key (`tr("Alle Fotos")`),
//! the English dictionary is in `locales/en-ui.tsv` (line: German, tab, English; `\n`, `\t` and `\\` escaped).
//! An unknown text stays unchanged (German) – which is why `tr` may also be used where user data occasionally passes through.
//! Templates with values are written as `trf!("{n} Fotos", n = n)`; the placeholders have the same names in both languages.
//! Display only: never apply to keys, file names, settings or sidecars.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::{language, Language};

/// Dictionary German → text of the current language (read once per language). Japanese, French and Spanish
/// are laid over the English one, so an entry missing there falls back to English, never to German.
fn dictionary() -> &'static HashMap<String, String> {
    dictionary_of(language())
}

fn dictionary_of(lang: Language) -> &'static HashMap<String, String> {
    static W: [OnceLock<HashMap<String, String>>; 5] = [const { OnceLock::new() }; 5];
    let slot = match lang {
        Language::De | Language::En => 0,
        Language::Ja => 1,
        Language::Fr => 2,
        Language::Es => 3,
    };
    W[slot].get_or_init(|| {
        let mut m = parse(include_str!("../../locales/en-ui.tsv"));
        let own = match lang {
            Language::Ja => include_str!("../../locales/ja-ui.tsv"),
            Language::Fr => include_str!("../../locales/fr-ui.tsv"),
            Language::Es => include_str!("../../locales/es-ui.tsv"),
            _ => "",
        };
        m.extend(parse(own));
        m
    })
}

/// Read lines `German<TAB>English`; skip empty lines and `#` comments (a line starting with `##` is an entry: help card text beginning with a heading).
pub fn parse(source: &str) -> HashMap<String, String> {
    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some(x) => {
                        out.push('\\');
                        out.push(x);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    }
    let mut m = HashMap::new();
    for l in source.lines() {
        if l.is_empty() || (l.starts_with('#') && !l.starts_with("##")) {
            continue;
        }
        if let Some((de, en)) = l.split_once('\t') {
            if !en.trim().is_empty() {
                m.insert(unescape(de), unescape(en));
            }
        }
    }
    m
}

/// Marks a German source text that is stored or passed on and translated only where it is shown
/// (`tr_msg`). Use it instead of `tr` when the text is kept in a structure that outlives a language switch.
/// The text needs an entry in `locales/en-ui.tsv` like every `tr` text (checked by the tests).
pub fn de(text: &str) -> &str {
    text
}

/// Decimal point of a displayed number: a comma in German, French and Spanish, a point otherwise (`1.5` → `1,5`).
/// Use on formatted numbers shown in the UI instead of `.replace('.', ",")`.
pub trait DeComma {
    fn de_comma(&self) -> String;
}
impl DeComma for str {
    fn de_comma(&self) -> String {
        if language().decimal_comma() {
            self.replace('.', ",")
        } else {
            self.to_string()
        }
    }
}

/// Text in the selected language. German: unchanged. English: the dictionary, otherwise the German text.
pub fn tr<'a>(de: &'a str) -> &'a str {
    let lang = language();
    if lang == Language::De {
        return de;
    }
    translated(de).map(|s| s.as_str()).unwrap_or(de)
}

#[allow(non_snake_case)]
fn translated(de: &str) -> Option<&'static String> {
    let t = dictionary().get(de);
    if t.is_none() && audit() {
        notify(de);
    }
    t
}

fn audit() -> bool {
    static A: OnceLock<bool> = OnceLock::new();
    *A.get_or_init(|| crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_I18N_AUDIT").as_deref() == Ok("1"))
}

/// Audit mode (`GRAINEO_I18N_AUDIT=1` with its own data folder): German-looking texts without translation are logged once.
fn notify(de: &str) {
    static REPORTED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let has_umlaut = de.chars().any(|c| "äöüÄÖÜß".contains(c));
    let is_word = de.chars().next().is_some_and(|c| c.is_uppercase()) && de.len() > 2;
    if !(has_umlaut || is_word) {
        return;
    }
    let mut g = REPORTED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if g.insert(de.to_string()) {
        super::diagnose(&format!("i18n-audit: keine englische Fassung für „{de}“"));
    }
}

/// Insert placeholder `{name}`; `{{` and `}}` yield `{` and `}` (like `format!`).
/// Inserted values that are themselves German messages (error texts from the library crates) are translated as well.
/// Only longer values with spaces: names and numbers from user data stay as they are.
fn value_translated(v: &str) -> std::borrow::Cow<'_, str> {
    if language() == Language::De || v.len() < 14 || !v.contains(' ') {
        return std::borrow::Cow::Borrowed(v);
    }
    tr_msg(v)
}

pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    fill_with(template, values, true)
}

/// Template already translated (`tr`), values inserted unchanged: names of the user (folders, files, collections,
/// keywords) are never translated – `fill` would turn a folder "Urlaub / Personen" into "Urlaub / people" through
/// the template "{n} Personen".
pub fn fill_names(template: &str, values: &[(&str, &str)]) -> String {
    fill_with(template, values, false)
}

/// Like `fill`, but the values are inserted unchanged: for German source texts that are translated later as a whole
/// (`de` + `tr_msg`).
pub fn fill_de(template: &str, values: &[(&str, &str)]) -> String {
    fill_with(template, values, false)
}

fn fill_with(template: &str, values: &[(&str, &str)], translate_values: bool) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let b = template.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' if b.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'}' if b.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            b'{' => match template[i + 1..].find('}') {
                Some(e) => {
                    let name = &template[i + 1..i + 1 + e];
                    match values.iter().find(|(k, _)| *k == name) {
                        Some((_, v)) if translate_values => out.push_str(&value_translated(v)),
                        Some((_, v)) => out.push_str(v),
                        None => out.push_str(&template[i..i + e + 2]),
                    }
                    i += e + 2;
                }
                None => {
                    out.push('{');
                    i += 1;
                }
            },
            _ => {
                // copy up to the next brace (UTF-8 safe)
                let rest = &template[i..];
                let n = rest.find(['{', '}']).unwrap_or(rest.len()).max(1);
                out.push_str(&rest[..n]);
                i += n;
            }
        }
    }
    out
}

/// Placeholders of a template (without `{{`): for checking that both languages have the same ones.
pub fn placeholder(template: &str) -> Vec<String> {
    let mut v = Vec::new();
    let b = template.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if b.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            if let Some(e) = template[i + 1..].find('}') {
                v.push(template[i + 1..i + 1 + e].to_string());
                i += e + 2;
                continue;
            }
        }
        i += 1;
    }
    v.sort();
    v
}

/// German quantity ("3 Fotos") – singular and plural are translated separately.
pub fn plural<'a>(n: usize, one: &'a str, more: &'a str) -> Cow<'a, str> {
    Cow::Borrowed(if n == 1 { tr(one) } else { tr(more) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::i18n::locale::TestLanguage;

    #[test]
    fn fill_like_format() {
        assert_eq!(fill("{n} Fotos · {x}", &[("n", "3"), ("x", "a")]), "3 Fotos · a");
        assert_eq!(fill("{{n}} {n}", &[("n", "3")]), "{n} 3");
        assert_eq!(fill("Ärger über {n}ß", &[("n", "1")]), "Ärger über 1ß");
        assert_eq!(fill("{unbekannt}", &[]), "{unbekannt}");
    }

    #[test]
    fn german_stays_unchanged_unknown_also_in_english() {
        let _de = TestLanguage::set(Language::De);
        assert_eq!(tr("Alle Fotos"), "Alle Fotos");
        let _en = TestLanguage::set(Language::En);
        assert_eq!(tr("Ein ganz anderer Text 123"), "Ein ganz anderer Text 123");
    }

    /// Japanese, French and Spanish have an entry for every German text of the English file, with the same placeholders.
    #[test]
    fn other_languages_are_complete_and_keep_placeholders() {
        let en = parse(include_str!("../../locales/en-ui.tsv"));
        for (name, src) in [
            ("ja", include_str!("../../locales/ja-ui.tsv")),
            ("fr", include_str!("../../locales/fr-ui.tsv")),
            ("es", include_str!("../../locales/es-ui.tsv")),
        ] {
            let own = parse(src);
            let mut bad = Vec::new();
            for key in en.keys() {
                match own.get(key) {
                    None => bad.push(format!("missing: {key:?}")),
                    Some(t) if placeholder(t) != placeholder(key) => bad.push(format!("placeholders: {key:?}")),
                    _ => {}
                }
            }
            assert!(bad.is_empty(), "{name}: {} problems:\n{}", bad.len(), bad.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
        }
    }

    #[test]
    fn missing_entry_falls_back_to_english_not_german() {
        let _ja = TestLanguage::set(Language::Ja);
        assert_eq!(tr("Alle Fotos"), dictionary_of(Language::Ja)["Alle Fotos"]);
        // an unknown text stays German as in every language
        assert_eq!(tr("Ein ganz anderer Text 123"), "Ein ganz anderer Text 123");
        assert_eq!(dictionary_of(Language::Fr).len(), dictionary_of(Language::En).len().max(dictionary_of(Language::Fr).len()));
    }

    #[test]
    fn parse_unescapes() {
        let m = parse("# x\na\\nb\tc\\td\n\nleer\t\n");
        assert_eq!(m.get("a\nb").map(String::as_str), Some("c\td"));
        assert!(!m.contains_key("leer"));
    }

    /// Every German display text has an English version: the texts in `tr("…")` / `trf!("…")` of the app's sources
    /// and all help card texts (help.rs). Same rules as `scripts/i18n-keys.py --missing` (texts identical in
    /// English need an entry too, e.g. `Film<TAB>Film`).
    #[test]
    fn every_german_text_has_an_english_version() {
        /// Rust string literal starting at `s` (after the opening quote): unescaped text and the rest after the closing quote.
        fn literal(s: &str) -> Option<(String, &str)> {
            let mut out = String::new();
            let mut it = s.char_indices().peekable();
            while let Some((i, c)) = it.next() {
                match c {
                    '"' => return Some((out, &s[i + 1..])),
                    '\\' => match it.next()?.1 {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'u' => {
                            let mut hex = String::new();
                            for (_, h) in it.by_ref() {
                                match h {
                                    '{' => {}
                                    '}' => break,
                                    h => hex.push(h),
                                }
                            }
                            out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        }
                        '\n' => {
                            while it.peek().is_some_and(|(_, c)| c.is_whitespace()) {
                                it.next();
                            }
                        }
                        x => out.push(x),
                    },
                    c => out.push(c),
                }
            }
            None
        }
        fn german(t: &str) -> bool {
            let letters = t.chars().filter(|c| c.is_alphabetic()).count();
            let ident = t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.:/-{}<>+*#%@[]|&!~".contains(c));
            // placeholders, punctuation and digits only
            let only_marks = {
                let mut rest = String::new();
                let mut depth = 0;
                for c in t.chars() {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        c if depth == 0 => rest.push(c),
                        _ => {}
                    }
                }
                !rest.chars().any(|c| c.is_alphabetic())
            };
            letters >= 2 && !ident && !only_marks && !["http", "/", "~", "."].iter().any(|p| t.starts_with(p))
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![];
        let mut dirs = vec![src.clone()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !p.ends_with("i18n") {
                        dirs.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
        let mut missing = std::collections::BTreeSet::new();
        for p in files {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with("tests.rs") || name.ends_with("_test.rs") || name == "prefs_i18n.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            // test modules at the end of a file are not part of the interface
            let text = text.split("#[cfg(test)]\nmod tests").next().unwrap_or(&text);
            let mut texts = vec![];
            for start in ["crate::tr(\"", " tr(\"", "(tr(\"", "trf!(\""] {
                for (i, _) in text.match_indices(start) {
                    if let Some((t, _)) = literal(&text[i + start.len()..]) {
                        texts.push(t);
                    }
                }
            }
            if name == "help.rs" {
                // card texts: the two literals after `card(` and the fields of hand-written cards
                for start in ["card(", "Card {short:", "text:"] {
                    for (i, _) in text.match_indices(start) {
                        let mut rest = &text[i + start.len()..];
                        for _ in 0..if start == "card(" { 2 } else { 1 } {
                            let Some(q) = rest.find('"').filter(|&q| rest[..q].trim_matches(|c: char| c.is_whitespace() || c == ',').is_empty()) else { break };
                            let Some((t, r)) = literal(&rest[q + 1..]) else { break };
                            texts.push(t);
                            rest = r;
                        }
                    }
                }
            }
            for t in texts {
                if german(&t) && !dictionary().contains_key(&t) {
                    missing.insert(format!("{name}: {t:?}"));
                }
            }
        }
        assert!(missing.is_empty(), "{} texts without English version in locales/en-ui.tsv:\n{}", missing.len(), missing.into_iter().collect::<Vec<_>>().join("\n"));
    }

    /// All string literals of a Rust source with their line (comments and char literals skipped).
    fn rust_literals(src: &str) -> Vec<(usize, String, String)> {
        let b: Vec<char> = src.chars().collect();
        let (mut i, mut line, mut out) = (0, 1, Vec::new());
        while i < b.len() {
            let c = b[i];
            if c == '\n' {
                line += 1;
                i += 1;
            } else if c == '/' && b.get(i + 1) == Some(&'/') {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            } else if c == '/' && b.get(i + 1) == Some(&'*') {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            } else if c == '\'' {
                // char literal ('x', '\n', '\u{..}') or lifetime
                if b.get(i + 1) == Some(&'\\') {
                    i += 2;
                    while i < b.len() && b[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                } else if b.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            } else if c == '"' || (c == 'r' && matches!(b.get(i + 1), Some('"') | Some('#')) && (i == 0 || !(b[i - 1].is_alphanumeric() || b[i - 1] == '_'))) {
                let before: String = b[i.saturating_sub(60)..i].iter().collect();
                let (mut hashes, mut j) = (0, i);
                let raw = c == 'r';
                if raw {
                    j += 1;
                    while b.get(j) == Some(&'#') {
                        hashes += 1;
                        j += 1;
                    }
                    if b.get(j) != Some(&'"') {
                        i += 1;
                        continue;
                    }
                }
                j += 1;
                let mut text = String::new();
                loop {
                    let Some(&d) = b.get(j) else { break };
                    if raw {
                        if d == '"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&'#')) {
                            j += 1 + hashes;
                            break;
                        }
                        text.push(d);
                        j += 1;
                    } else if d == '\\' {
                        match b.get(j + 1) {
                            Some('n') => text.push('\n'),
                            Some('t') => text.push('\t'),
                            Some('u') => {
                                let mut hex = String::new();
                                j += 2;
                                while let Some(&h) = b.get(j) {
                                    j += 1;
                                    if h == '}' {
                                        break;
                                    }
                                    if h != '{' {
                                        hex.push(h);
                                    }
                                }
                                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                    text.push(ch);
                                }
                                continue;
                            }
                            Some('\n') => {
                                line += 1;
                                j += 2;
                                while b.get(j).is_some_and(|c| c.is_whitespace()) {
                                    if b[j] == '\n' {
                                        line += 1;
                                    }
                                    j += 1;
                                }
                                continue;
                            }
                            Some(&x) => text.push(x),
                            None => {}
                        }
                        j += 2;
                    } else if d == '"' {
                        j += 1;
                        break;
                    } else {
                        if d == '\n' {
                            line += 1;
                        }
                        text.push(d);
                        j += 1;
                    }
                }
                out.push((line, text, before));
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The German UI texts that are NOT written as `tr("…")`: constants, tuples handed to translating helpers,
    /// `format!` pieces, return values. Every literal that looks German (umlaut, ß or typical German words) must have
    /// an English dictionary entry (then it is displayed through a translating helper or wrapped in `tr`) or be listed in
    /// `locales/i18n-ignore.txt` (logs, SQL, test data). Find candidates with `scripts/i18n-static.py`.
    #[test]
    fn every_german_literal_is_translated_or_listed() {
        const WORDS: &[&str] = &[
            "der", "die", "das", "den", "dem", "des", "ein", "eine", "einen", "einem", "einer", "und", "oder", "nicht", "kein", "keine", "mit", "von", "für", "auf", "aus", "bei", "nach", "über", "unter", "zum", "zur", "ist", "sind", "wird", "werden", "wurde", "kann", "können", "muss", "noch", "nur", "auch", "wenn", "als", "dass", "bild", "bilder", "foto", "fotos", "datei", "dateien", "ordner", "fehler", "öffnen", "speichern", "löschen", "wählen", "zurück", "abbrechen", "schließen", "fehlt", "fehlen", "gespeichert", "geladen", "läuft", "zeile", "regler", "stufe", "sammlung", "bibliothek", "einstellungen", "vorschau", "entwickeln", "gesichter", "kamera", "objektiv", "gut", "mittel", "klein", "groß", "alle", "ohne", "keins", "weich", "scharf", "stark", "heute", "gestern", "neu", "alt",
        ];
        let ignore: std::collections::HashSet<&str> = include_str!("../../locales/i18n-ignore.txt").lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        let allowed_calls = ["log", "println!", "eprintln!", "panic!", "expect", "diagnose", "unreachable!", "todo!", "trace!", "dbg!", "get", "remove", "insert", "contains_key", "set", "value", "setting", "key", "starts_with", "ends_with", "contains", "find", "split", "strip_prefix", "strip_suffix", "split_once", "var", "get_or", "extension", "join", "file_name", "assert!", "assert_eq!", "debug_assert!"];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![];
        let mut dirs = vec![src];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !p.ends_with("i18n") {
                        dirs.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
        let mut found = std::collections::BTreeSet::new();
        for p in files {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with("tests.rs") || name.ends_with("_test.rs") || name == "prefs_i18n.rs" || name == "ui_test.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let text = text.split("#[cfg(test)]\nmod ").next().unwrap_or(&text);
            for (line, lit, before) in rust_literals(text) {
                let letters = lit.chars().filter(|c| c.is_alphabetic()).count();
                if letters < 2 || dictionary().contains_key(&lit) || ignore.contains(format!("{name}:{lit}").as_str()) {
                    continue;
                }
                if lit.starts_with('\0') || lit.starts_with("http") || lit.starts_with('/') || lit.starts_with('.') || lit.starts_with('~') || lit.contains('_') && !lit.contains(' ') || lit.contains("::") || lit.contains(".rs") {
                    continue;
                }
                let head = before.trim_end().trim_end_matches(['&', '(']).trim_end();
                let callee: String = head.chars().rev().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '!' | ':')).collect::<Vec<_>>().into_iter().rev().collect();
                let callee = callee.rsplit("::").next().unwrap_or("").to_string();
                if before.trim_end().ends_with('(') && allowed_calls.contains(&callee.as_str()) {
                    continue;
                }
                // already translated at the site
                if before.trim_end().ends_with("tr(") || before.trim_end().ends_with("de(") || before.trim_end().ends_with("trf!(") {
                    continue;
                }
                // single lower-case words are keys and identifiers ("kamera", "bilder"), not display text
                let ident_like = !lit.contains(' ') && lit.chars().next().is_some_and(|c| c.is_lowercase()) && !lit.chars().any(|c| "äöüß".contains(c));
                let german = !ident_like && lit.chars().any(|c| "äöüÄÖÜß".contains(c)) || !ident_like && lit.split(|c: char| !c.is_alphabetic()).any(|w| WORDS.contains(&w.to_lowercase().as_str()));
                if german {
                    found.insert(format!("{name}:{line}: {lit:?}"));
                }
            }
        }
        assert!(found.is_empty(), "{} German-looking literals without English version (add them to locales/en-ui.tsv and make sure they go through tr / a translating helper, or list them in locales/i18n-ignore.txt as file.rs:text):\n{}", found.len(), found.into_iter().collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn dictionary_has_same_placeholders_in_both_languages() {
        for (de, en) in dictionary() {
            assert_eq!(placeholder(de), placeholder(en), "{de:?} → {en:?}");
        }
    }
}

/// Audit mode for the display (`GRAINEO_I18N_AUDIT=1`): reports every painted text that is a German key in the
/// dictionary (i.e. did not pass through `tr` at the drawing site) or looks German.
pub fn audit_image(ctx: &eframe::egui::Context) {
    use eframe::egui::{LayerId, Shape};
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    if !audit() {
        return;
    }
    static KEYS: OnceLock<HashSet<String>> = OnceLock::new();
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| dictionary().keys().map(|k| k.to_lowercase()).collect());
    static VALUES: OnceLock<HashSet<String>> = OnceLock::new();
    let values = VALUES.get_or_init(|| dictionary().values().map(|v| v.to_lowercase()).collect());
    let mut texts: Vec<String> = Vec::new();
    let mut layers: Vec<LayerId> = ctx.memory(|m| m.areas().visible_layer_ids().into_iter().collect());
    layers.push(LayerId::background());
    ctx.graphics(|g| {
        for l in &layers {
            if let Some(list) = g.get(*l) {
                for s in list.all_entries() {
                    if let Shape::Text(t) = &s.shape {
                        texts.push(t.galley.text().to_string());
                    }
                }
            }
        }
    });
    let mut reported = REPORTED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    for t in texts {
        let t = t.trim().to_string();
        if t.chars().filter(|c| c.is_alphabetic()).count() < 3 {
            continue;
        }
        let lower = t.to_lowercase();
        let is_german = keys.contains(&lower) || (!values.contains(&lower) && t.chars().any(|c| c.is_alphabetic()) && !t.is_ascii() || (!values.contains(&lower) && t.split_whitespace().any(|w| w.len() > 3 && w.chars().all(|c| c.is_alphabetic()))));
        if is_german && reported.insert(t.clone()) {
            super::diagnose(&format!("i18n-bild: {t}"));
        }
    }
}

/// Messages from the library crates are created there via `format!` and are German.
/// Templates with placeholders from the dictionary match too: `{}`/`{name}` stand for arbitrary text.
/// Without a matching entry the text stays unchanged; results are cached.
pub fn tr_msg(de: &str) -> std::borrow::Cow<'_, str> {
    match friendly(de) {
        std::borrow::Cow::Borrowed(_) => tr_msg_depth(de, 0),
        std::borrow::Cow::Owned(s) => std::borrow::Cow::Owned(tr_msg_depth(&s, 0).into_owned()),
    }
}

/// Technical error texts of SQLite and the operating system (English, with error numbers) and the plain phrase
/// shown instead: (technical text, German, English).
const FRIENDLY: &[(&str, &str, &str)] = &[
    ("database is locked", "die Bibliothek ist gerade belegt – bitte gleich noch einmal", "the library is busy right now – please try again in a moment"),
    ("database disk image is malformed", "die Bibliotheksdatei ist beschädigt", "the library file is damaged"),
    ("unable to open database file", "die Bibliotheksdatei lässt sich nicht öffnen", "the library file cannot be opened"),
    ("disk I/O error", "Lesefehler in der Bibliotheksdatei", "read error in the library file"),
    ("Permission denied (os error 13)", "keine Berechtigung", "no permission"),
    ("Operation not permitted (os error 1)", "vom System nicht erlaubt", "not permitted by the system"),
    ("No such file or directory (os error 2)", "Datei oder Ordner nicht gefunden", "file or folder not found"),
    ("No space left on device (os error 28)", "kein Platz mehr auf dem Laufwerk", "no space left on the drive"),
    ("Read-only file system (os error 30)", "das Laufwerk ist schreibgeschützt", "the drive is read-only"),
    ("Input/output error (os error 5)", "Lesefehler am Laufwerk", "read error on the drive"),
    ("Device not configured (os error 6)", "das Laufwerk ist nicht verbunden", "the drive is not connected"),
    ("File exists (os error 17)", "die Datei gibt es schon", "the file already exists"),
    ("Host is down (os error 64)", "das Netzlaufwerk ist nicht erreichbar", "the network drive is not reachable"),
    ("Socket is not connected (os error 57)", "das Netzlaufwerk ist nicht verbunden", "the network drive is not connected"),
    ("Operation timed out (os error 60)", "das Netzlaufwerk antwortet nicht", "the network drive does not respond"),
    ("Stale NFS file handle (os error 70)", "das Netzlaufwerk wurde neu verbunden – bitte noch einmal", "the network drive was reconnected – please try again"),
];

/// The same phrases in Japanese, French and Spanish (same technical text as key).
const FRIENDLY_OTHER: &[(&str, [&str; 3])] = &[
    ("database is locked", ["ライブラリが使用中です。少し待ってからもう一度お試しください", "la bibliothèque est occupée – réessayez dans un instant", "la biblioteca está ocupada: inténtalo de nuevo en un momento"]),
    ("database disk image is malformed", ["ライブラリファイルが破損しています", "le fichier de la bibliothèque est endommagé", "el archivo de la biblioteca está dañado"]),
    ("unable to open database file", ["ライブラリファイルを開けません", "le fichier de la bibliothèque ne peut pas être ouvert", "no se puede abrir el archivo de la biblioteca"]),
    ("disk I/O error", ["ライブラリファイルの読み込みエラー", "erreur de lecture du fichier de la bibliothèque", "error de lectura del archivo de la biblioteca"]),
    ("Permission denied (os error 13)", ["権限がありません", "autorisation refusée", "sin permiso"]),
    ("Operation not permitted (os error 1)", ["システムで許可されていません", "non autorisé par le système", "no permitido por el sistema"]),
    ("No such file or directory (os error 2)", ["ファイルまたはフォルダが見つかりません", "fichier ou dossier introuvable", "archivo o carpeta no encontrado"]),
    ("No space left on device (os error 28)", ["ドライブの空き容量がありません", "plus d'espace disponible sur le disque", "no queda espacio en la unidad"]),
    ("Read-only file system (os error 30)", ["ドライブは読み取り専用です", "le disque est en lecture seule", "la unidad es de solo lectura"]),
    ("Input/output error (os error 5)", ["ドライブの読み込みエラー", "erreur de lecture sur le disque", "error de lectura en la unidad"]),
    ("Device not configured (os error 6)", ["ドライブが接続されていません", "le disque n'est pas connecté", "la unidad no está conectada"]),
    ("File exists (os error 17)", ["ファイルはすでに存在します", "le fichier existe déjà", "el archivo ya existe"]),
    ("Host is down (os error 64)", ["ネットワークドライブに接続できません", "le disque réseau est inaccessible", "la unidad de red no está accesible"]),
    ("Socket is not connected (os error 57)", ["ネットワークドライブが接続されていません", "le disque réseau n'est pas connecté", "la unidad de red no está conectada"]),
    ("Operation timed out (os error 60)", ["ネットワークドライブが応答しません", "le disque réseau ne répond pas", "la unidad de red no responde"]),
    ("Stale NFS file handle (os error 70)", ["ネットワークドライブが再接続されました。もう一度お試しください", "le disque réseau a été reconnecté – réessayez", "la unidad de red se volvió a conectar: inténtalo de nuevo"]),
];

/// Replace technical error texts in a message by a plain phrase in the current language (see [`FRIENDLY`]).
/// Everything else stays as it is. Every message shown to the user passes through here via [`tr_msg`].
pub fn friendly(msg: &str) -> std::borrow::Cow<'_, str> {
    if !FRIENDLY.iter().any(|(t, _, _)| msg.contains(t)) {
        return std::borrow::Cow::Borrowed(msg);
    }
    let lang = language();
    let mut s = msg.to_string();
    for (t, de, e) in FRIENDLY {
        let phrase = match lang {
            Language::De => de,
            Language::En => e,
            other => {
                let i = match other { Language::Ja => 0, Language::Fr => 1, _ => 2 };
                FRIENDLY_OTHER.iter().find(|(k, _)| k == t).map(|(_, v)| v[i]).unwrap_or(e)
            }
        };
        s = s.replace(t, phrase);
    }
    std::borrow::Cow::Owned(s)
}

fn tr_msg_depth(de: &str, depth: u8) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    use std::sync::Mutex;
    if language() == Language::De || de.is_empty() {
        return Cow::Borrowed(de);
    }
    if let Some(t) = dictionary().get(de) {
        return Cow::Owned(t.clone());
    }
    static PATTERNS: [OnceLock<Vec<(Vec<String>, Vec<String>, String)>>; 4] = [const { OnceLock::new() }; 4];
    static MEMO: [OnceLock<Mutex<HashMap<String, String>>>; 4] = [const { OnceLock::new() }; 4];
    let slot = match language() { Language::Ja => 1, Language::Fr => 2, Language::Es => 3, _ => 0 };
    let patterns = PATTERNS[slot].get_or_init(|| {
        let mut v = Vec::new();
        for (k, en) in dictionary() {
            if !k.contains('{') || k.contains("{{") {
                continue;
            }
            // split k into parts (fixed text) and placeholder names
            let (mut parts, mut names, mut cur) = (Vec::new(), Vec::new(), String::new());
            let mut rest = k.as_str();
            while let Some(a) = rest.find('{') {
                let Some(b) = rest[a..].find('}') else { break };
                cur.push_str(&rest[..a]);
                parts.push(std::mem::take(&mut cur));
                let name = &rest[a + 1..a + b];
                names.push(name.split(':').next().unwrap_or("").to_string());
                rest = &rest[a + b + 1..];
            }
            parts.push(rest.to_string());
            v.push((parts, names, en.clone()));
        }
        // more specific ones (longer fixed text) first
        v.sort_by_key(|(t, _, _)| std::cmp::Reverse(t.iter().map(|s| s.len()).sum::<usize>()));
        v
    });
    let memo = MEMO[slot].get_or_init(Default::default);
    if let Some(t) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(de) {
        return Cow::Owned(t.clone());
    }
    let mut result = de.to_string();
    'pattern: for (parts, names, en) in patterns {
        if !de.starts_with(&parts[0]) || !de.ends_with(parts.last().unwrap().as_str()) || de.len() < parts.iter().map(|s| s.len()).sum::<usize>() {
            continue;
        }
        let mut pos = parts[0].len();
        let end = de.len() - parts.last().unwrap().len();
        let mut values: Vec<&str> = Vec::new();
        for i in 1..parts.len() {
            let t = &parts[i];
            if i == parts.len() - 1 {
                if pos > end {
                    continue 'pattern;
                }
                values.push(&de[pos..end]);
            } else {
                if t.is_empty() {
                    continue 'pattern;
                }
                let Some(p) = de[pos..end].find(t.as_str()) else { continue 'pattern };
                values.push(&de[pos..pos + p]);
                pos += p + t.len();
            }
        }
        // fill the English template: named placeholders by name, empty ones in order
        let mut out = String::new();
        let mut rest = en.as_str();
        let mut running = 0usize;
        while let Some(a) = rest.find('{') {
            let Some(b) = rest[a..].find('}') else { break };
            out.push_str(&rest[..a]);
            let name = rest[a + 1..a + b].split(':').next().unwrap_or("");
            let idx = if name.is_empty() {
                running += 1;
                Some(running - 1)
            } else {
                names.iter().position(|n| n == name)
            };
            let value = idx.and_then(|i| values.get(i)).copied().unwrap_or("");
            if depth < 2 { out.push_str(&tr_msg_depth(value, depth + 1)) } else { out.push_str(value) }
            rest = &rest[a + b + 1..];
        }
        out.push_str(rest);
        result = out;
        break;
    }
    // Error chains ("context: cause", as produced by `{e:#}` or `format!("…: {e}")`): if the whole text is unknown,
    // translate the context and the cause separately; unknown parts (paths, system messages) stay as they are.
    if result == de && depth < 3 {
        if let Some((head, tail)) = de.split_once(": ") {
            if !head.is_empty() && !tail.is_empty() {
                result = format!("{}: {}", tr_msg_depth(head, depth + 1), tr_msg_depth(tail, depth + 1));
            }
        }
    }
    let mut m = memo.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 4000 {
        m.clear();
    }
    m.insert(de.to_string(), result.clone());
    Cow::Owned(result)
}

#[cfg(test)]
mod tests_msg {
    use super::*;
    use crate::menu::i18n::locale::TestLanguage;

    #[test]
    fn technical_errors_become_plain_phrases() {
        let raw = "Bewertung nicht gespeichert: database is locked";
        let f = friendly(raw);
        assert!(!f.contains("database is locked"), "{f}");
        assert!(f.starts_with("Bewertung nicht gespeichert: "), "{f}");
        assert!(matches!(friendly("alles gut"), std::borrow::Cow::Borrowed(_)));
        let io = friendly("/x/y.xmp: No such file or directory (os error 2)");
        assert!(!io.contains("os error"), "{io}");
    }

    /// A folder of the user is shown as it is named. `fill` would translate the value through the
    /// template "{n} Personen" ("graineo-demo / Personen" → "graineo-demo / people").
    #[test]
    fn names_of_the_user_are_never_translated() {
        let _l = TestLanguage::set(Language::En);
        let template = tr("{a} · {b} Fotos");
        assert_eq!(fill_names(template, &[("a", "graineo-demo / Personen"), ("b", "12")]), "graineo-demo / Personen · 12 photos");
        assert_ne!(fill(template, &[("a", "graineo-demo / Personen"), ("b", "12")]), "graineo-demo / Personen · 12 photos");
    }

    #[test]
    fn messages_with_placeholders_are_translated() {
        let _l = TestLanguage::set(Language::En);
        assert_eq!(tr_msg("Ordner /tmp/x fehlt"), "Folder /tmp/x is missing");
        assert_eq!(tr_msg("Auf dem Mac sind 3 GB frei, nötig sind 5 GB plus 1 GB Reserve."), "This Mac has 3 GB free; 5 GB plus a 1 GB buffer is needed.");
        assert_eq!(tr_msg("etwas ganz anderes"), "etwas ganz anderes");
        // chains: context and cause are translated separately, unknown parts stay
        assert_eq!(tr_msg("Original nicht lesbar: kein Dateiname"), "Can’t read the original: no file name");
        // the unknown context stays, the technical cause becomes a plain phrase (`friendly`)
        assert_eq!(tr_msg("Unbekannter Kontext: Permission denied (os error 13)"), "Unbekannter Kontext: no permission");
        // library-crate errors with values
        assert_eq!(tr_msg("Ziel existiert schon: /tmp/a.jpg"), "Destination already exists: /tmp/a.jpg");
        assert_eq!(tr_msg("Prüfsumme von models/clip.bin stimmt nicht – Datei verworfen"), "Checksum mismatch for models/clip.bin – file discarded");
        let _de = TestLanguage::set(Language::De);
        assert_eq!(tr_msg("Original nicht lesbar: kein Dateiname"), "Original nicht lesbar: kein Dateiname");
    }
}
