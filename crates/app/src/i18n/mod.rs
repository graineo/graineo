//! UI-only localization. Never serialize a language into photo development settings.
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
// The display API is deliberately prepared for later authorized domain views.
#[allow(dead_code)]
pub mod format;
mod keys;
pub mod locale;
mod ui;
pub use ui::{audit_image, de, DeComma, fill, fill_de, fill_names, plural, tr, tr_msg};
use fluent_bundle::{concurrent::FluentBundle, FluentArgs, FluentResource};
pub use keys::Text;
pub use locale::{generation, language, Choice};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    De,
    En,
    Ja,
    Fr,
    Es,
}

impl Language {
    pub const ALL: [Language; 5] = [Language::De, Language::En, Language::Ja, Language::Fr, Language::Es];
    /// BCP 47 code, also the file prefix of the catalogs.
    pub fn code(self) -> &'static str {
        match self {
            Self::De => "de",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Fr => "fr",
            Self::Es => "es",
        }
    }
    /// Languages with a decimal comma (and a space as thousands separator).
    pub fn decimal_comma(self) -> bool {
        matches!(self, Self::De | Self::Fr | Self::Es)
    }
}

pub struct Catalogs {
    de: FluentBundle<FluentResource>,
    en: FluentBundle<FluentResource>,
    ja: FluentBundle<FluentResource>,
    fr: FluentBundle<FluentResource>,
    es: FluentBundle<FluentResource>,
    missing: Mutex<HashSet<String>>,
}

impl Catalogs {
    pub fn new() -> Result<Self, String> {
        Self::build(
            include_str!("../../locales/de.ftl"),
            include_str!("../../locales/en.ftl"),
            [
                include_str!("../../locales/ja.ftl"),
                include_str!("../../locales/fr.ftl"),
                include_str!("../../locales/es.ftl"),
            ],
            pseudo_enabled(),
        )
    }
    fn from_sources(de: &str, en: &str) -> Result<Self, String> {
        Self::build(de, en, ["", "", ""], false)
    }
    fn build(de: &str, en: &str, other: [&str; 3], pseudo: bool) -> Result<Self, String> {
        fn bundle(
            language: &str,
            source: &str,
            pseudo: bool,
        ) -> Result<FluentBundle<FluentResource>, String> {
            let id: unic_langid::LanguageIdentifier = language
                .parse()
                .map_err(|e| format!("Ungültige UI-Sprache: {e}"))?;
            let mut bundle = FluentBundle::new_concurrent(vec![id]);
            bundle.set_use_isolating(false);
            if pseudo {
                bundle.set_transform(Some(pseudo_text));
            }
            let resource = FluentResource::try_new(source.to_owned())
                .map_err(|(_, e)| format!("Ungültiger Sprachkatalog: {e:?}"))?;
            bundle
                .add_resource(resource)
                .map_err(|e| format!("Doppelte Übersetzung: {e:?}"))?;
            Ok(bundle)
        }
        Ok(Self {
            de: bundle("de", de, pseudo)?,
            en: bundle("en", en, pseudo)?,
            ja: bundle("ja", other[0], pseudo)?,
            fr: bundle("fr", other[1], pseudo)?,
            es: bundle("es", other[2], pseudo)?,
            missing: Mutex::new(HashSet::new()),
        })
    }
    fn resolve(
        &self,
        language: Language,
        key: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<String> {
        let bundle = match language {
            Language::De => &self.de,
            Language::En => &self.en,
            Language::Ja => &self.ja,
            Language::Fr => &self.fr,
            Language::Es => &self.es,
        };
        let pattern = bundle.get_message(key)?.value()?;
        let mut errors = Vec::new();
        let value = bundle.format_pattern(pattern, args, &mut errors);
        errors.is_empty().then(|| value.into_owned())
    }
    fn format(&self, language: Language, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        if let Some(value) = self.resolve(language, key, args) {
            return value;
        }
        // a missing entry of Japanese/French/Spanish falls back to English, not to German
        if language != Language::De && language != Language::En {
            if let Some(value) = self.resolve(Language::En, key, args) {
                return value;
            }
        }
        let fresh = self
            .missing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_owned());
        if fresh {
            diagnose(&format!(
                "i18n: fehlender oder ungültiger Text {key} ({language:?})"
            ));
        }
        self.resolve(Language::De, key, args)
            .unwrap_or_else(|| format!("⟦{key}⟧"))
    }
    // Typed plural API is exercised by the probe/tests; domain translations are gated.
    #[allow(dead_code)]
    pub fn photos(&self, language: Language, count: usize) -> String {
        let mut args = FluentArgs::new();
        args.set("count", count);
        self.format(language, "probe-photos", Some(&args))
    }
}

/// Logging is connected by the menu adapter; examples remain independent of the app.
static LOGGER: OnceLock<fn(&str)> = OnceLock::new();
pub fn set_logger(logger: fn(&str)) {
    let _ = LOGGER.set(logger);
}
fn diagnose(message: &str) {
    if let Some(logger) = LOGGER.get() {
        logger(message);
    } else {
        eprintln!("{message}");
    }
}
fn pseudo_enabled() -> bool {
    crate::env::var_os("GRAINEO_DATA_DIR").is_some()
        && crate::env::var("GRAINEO_I18N_PSEUDO").as_deref() == Ok("1")
}
fn pseudo_text(text: &str) -> Cow<'_, str> {
    // Fluent applies this only to literal text, not file names or other argument values.
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let raw = line.strip_suffix('\n').unwrap_or(line);
        out.push_str(&raw.replace('a', "ä").replace('o', "ö").replace('u', "ü"));
        out.push_str(&"ü".repeat((raw.chars().count() as f64 * 0.4).round() as usize));
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    Cow::Owned(out)
}
/// One parsed pair of embedded catalogs per process; used by the UI only.
pub fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        Catalogs::new().unwrap_or_else(|error| {
            diagnose(&format!("i18n: {error}"));
            // An invalid embedded catalog is a failed release check, but must not crash a window.
            Catalogs::from_sources("common-cancel = Abbrechen\ncommon-close = Schließen", "")
                .expect("fixed minimal fallback catalog")
        })
    })
}
/// Typed static labels are formatted once, retaining stable lifetimes for existing UI helpers.
pub fn text_in(language: Language, key: Text) -> &'static str {
    static TEXTS: OnceLock<HashMap<(Language, Text), String>> = OnceLock::new();
    let values = TEXTS.get_or_init(|| {
        let mut values = HashMap::new();
        for language in Language::ALL {
            for &key in Text::ALL {
                values.insert((language, key), catalogs().format(language, key.id(), None));
            }
        }
        values
    });
    values
        .get(&(language, key))
        .map(String::as_str)
        .unwrap_or(key.id())
}
pub fn text(key: Text) -> &'static str {
    text_in(language(), key)
}
#[allow(dead_code)]
pub fn about_details(version: &str) -> String {
    let mut args = FluentArgs::new();
    args.set("version", version);
    catalogs().format(language(), "about-details", Some(&args))
}
pub fn language_error(error: &str) -> String {
    let mut args = FluentArgs::new();
    args.set("error", error);
    catalogs().format(language(), "prefs-language-error", Some(&args))
}

/// Counts from a sync report. Missing older report fields retain the former "null" display.
pub fn sync_status(
    present: Option<u64>,
    missing: Option<u64>,
    added: Option<u64>,
) -> String {
    let count = |n: Option<u64>| n.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
    let mut args = FluentArgs::new();
    // Strings preserve the previous ungrouped decimal representation, even above 2^53.
    args.set("present", count(present));
    args.set("missing", count(missing));
    args.set("added", count(added));
    catalogs().format(language(), "library-sync-status", Some(&args))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_status_preserves_german_counts_and_translates_the_full_sentence() {
        for n in [0, 1, 2, 1000, u64::MAX] {
            let _de = locale::TestLanguage::set(Language::De);
            assert_eq!(
                sync_status(Some(n), Some(n), Some(n)),
                format!("Abgleich: {n} vorhanden, {n} fehlen, {n} neu")
            );
            let _en = locale::TestLanguage::set(Language::En);
            assert_eq!(
                sync_status(Some(n), Some(n), Some(n)),
                format!("Sync: {n} found, {n} missing, {n} new")
            );
        }
        let _de = locale::TestLanguage::set(Language::De);
        assert_eq!(
            sync_status(None, None, None),
            "Abgleich: null vorhanden, null fehlen, null neu"
        );
    }
    #[test]
    fn plurals_and_unchanged_german_unicode() {
        let c = Catalogs::new().unwrap();
        for (n, de, en) in [
            (0, "0 Fotos", "0 photos"),
            (1, "Ein Foto", "One photo"),
            (2, "2 Fotos", "2 photos"),
        ] {
            assert_eq!(c.photos(Language::De, n), de);
            assert_eq!(c.photos(Language::En, n), en);
        }
        assert_eq!(
            c.format(Language::De, "common-close", None).as_bytes(),
            "Schließen".as_bytes()
        );
    }
    #[test]
    fn missing_or_malformed_english_falls_back_once() {
        let c = Catalogs::from_sources("keep = Deutsch\nbroken = Deutsch", "broken = { $missing }")
            .unwrap();
        for _ in 0..3 {
            assert_eq!(c.format(Language::En, "keep", None), "Deutsch");
        }
        assert_eq!(c.missing.lock().unwrap().len(), 1);
        assert_eq!(c.format(Language::En, "broken", None), "Deutsch");
        assert_eq!(c.missing.lock().unwrap().len(), 2);
    }

    #[test]
    fn catalogs_have_the_same_keys_and_all_static_labels_are_valid() {
        fn keys(source: &str) -> HashSet<&str> {
            source
                .lines()
                .filter(|l| !l.starts_with(char::is_whitespace))
                .filter_map(|l| l.split_once(" = ").map(|(k, _)| k))
                .collect()
        }
        let de = include_str!("../../locales/de.ftl");
        let en = include_str!("../../locales/en.ftl");
        assert_eq!(keys(de), keys(en));
        let c = Catalogs::new().unwrap();
        for &key in Text::ALL {
            for language in Language::ALL {
                assert!(
                    c.resolve(language, key.id(), None).is_some(),
                    "{:?} {}",
                    language,
                    key.id()
                );
            }
        }
    }
    #[test]
    fn every_language_has_the_same_catalog_keys() {
        fn keys(source: &str) -> HashSet<&str> {
            source
                .lines()
                .filter(|l| !l.starts_with(char::is_whitespace))
                .filter_map(|l| l.split_once(" = ").map(|(k, _)| k))
                .collect()
        }
        let de = keys(include_str!("../../locales/de.ftl"));
        for (name, src) in [
            ("ja", include_str!("../../locales/ja.ftl")),
            ("fr", include_str!("../../locales/fr.ftl")),
            ("es", include_str!("../../locales/es.ftl")),
        ] {
            assert_eq!(de, keys(src), "{name}.ftl");
        }
    }
    #[test]
    fn pseudo_expands_literals_but_preserves_arguments() {
        let c = Catalogs::build("a = Datei { $path }", "a = File { $path }", ["", "", ""], true).unwrap();
        let mut args = FluentArgs::new();
        args.set("path", "/tmp/ä Japan 日本.jpg");
        let value = c.format(Language::En, "a", Some(&args));
        assert!(value.contains("/tmp/ä Japan 日本.jpg"));
        let original = "File /tmp/ä Japan 日本.jpg";
        assert!(value.chars().count() > original.chars().count());
    }

    #[test]
    fn about_matches_the_original_german_multiline_text() {
        let _language = locale::TestLanguage::set(Language::De);
        assert_eq!(
            about_details("0.25.0").as_bytes(),
            include_bytes!("../../tests/snapshots/about-de.txt")
        );
        let _english = locale::TestLanguage::set(Language::En);
        assert!(about_details("0.25.0").contains("Foveon is a trademark of Sigma Corporation"));
    }
}
