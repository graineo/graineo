//! Language choice is one process-wide state: worker threads (loading, library, export) create texts that
//! end up in the UI, so they must see a runtime switch immediately. Only the unit tests override the
//! language per thread (`TestLanguage`), because tests run in parallel.
use super::Language;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    System,
    De,
    En,
    Ja,
    Fr,
    Es,
}
impl Choice {
    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::De => "de",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Fr => "fr",
            Self::Es => "es",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "system" => Some(Self::System),
            "de" => Some(Self::De),
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            "fr" => Some(Self::Fr),
            "es" => Some(Self::Es),
            _ => None,
        }
    }
    pub fn resolve(self, preferred: &[String]) -> Language {
        match self {
            Self::De => Language::De,
            Self::En => Language::En,
            Self::Ja => Language::Ja,
            Self::Fr => Language::Fr,
            Self::Es => Language::Es,
            Self::System => preferred
                .iter()
                .find_map(|s| {
                    match s
                        .split(['-', '_'])
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "de" => Some(Language::De),
                        "en" => Some(Language::En),
                        "ja" => Some(Language::Ja),
                        "fr" => Some(Language::Fr),
                        "es" => Some(Language::Es),
                        _ => None,
                    }
                })
                // Unsupported system language: English reaches more people than German.
                .unwrap_or(Language::En),
        }
    }
}
#[derive(Clone)]
struct State {
    choice: Choice,
    language: Language,
}
static STATE: RwLock<Option<State>> = RwLock::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Counts language switches. Caches that keep translated text compare it and rebuild when it changed.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}
#[cfg(test)]
thread_local! { static TEST_STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) }; }
fn current() -> State {
    #[cfg(test)]
    if let Some(s) = TEST_STATE.with(|s| s.borrow().clone()) {
        return s;
    }
    if let Some(s) = STATE.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return s;
    }
    // computed outside the lock: reading the settings must not block other readers
    let fresh = initial();
    STATE.write().unwrap_or_else(|e| e.into_inner()).get_or_insert(fresh).clone()
}

#[cfg(target_os = "macos")]
fn preferred_languages() -> Vec<String> {
    use objc2::{msg_send, rc::Retained, runtime::AnyClass};
    use objc2_foundation::{NSArray, NSString};
    let Some(class) = AnyClass::get(c"NSLocale") else {
        return Vec::new();
    };
    // Read-only Foundation API, no defaults write, shell command or process environment change.
    let languages: Retained<NSArray<NSString>> = unsafe { msg_send![class, preferredLanguages] };
    languages.iter().map(|s| s.to_string()).collect()
}
#[cfg(windows)]
fn preferred_languages() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    // Windows does not set LANG/LC_ALL; the display languages of the user come from the MUI API
    // ("de-DE\0en-US\0\0"). Read-only, no registry access.
    let mut count = 0u32;
    let mut len = 0u32;
    // SAFETY: a null buffer asks only for the required length (in UTF-16 units).
    if unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, std::ptr::null_mut(), &mut len) } == 0 || len == 0 {
        return posix_languages(|k| std::env::var(k).ok());
    }
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: the buffer holds exactly `len` UTF-16 units, as requested by the first call.
    if unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buffer.as_mut_ptr(), &mut len) } == 0 {
        return posix_languages(|k| std::env::var(k).ok());
    }
    buffer.truncate(len as usize);
    split_multi_string(&buffer)
}
#[cfg(not(any(target_os = "macos", windows)))]
fn preferred_languages() -> Vec<String> {
    posix_languages(|k| std::env::var(k).ok())
}
/// Splits a Windows multi-string (names separated by NUL, ended by two NULs) into its names.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn split_multi_string(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|c| *c == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| String::from_utf16(part).ok())
        .collect()
}
/// Language list after POSIX rules: `LANGUAGE` (list separated by ':') is only honored when a
/// locale is set and it is not "C"/"POSIX"; then `LC_ALL`, `LC_MESSAGES`, `LANG` (first one set).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn posix_languages(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(&var)
        .find(|v| !v.trim().is_empty());
    let plain = |v: &str| {
        let base = v.split(['.', '@']).next().unwrap_or("").trim();
        (!base.is_empty() && base != "C" && base != "POSIX").then(|| base.to_owned())
    };
    let mut out = Vec::new();
    if let Some(locale) = locale.as_deref().and_then(plain) {
        if let Some(list) = var("LANGUAGE") {
            out.extend(list.split(':').filter_map(plain));
        }
        out.push(locale);
    }
    out
}
fn saved_choice(saved: &serde_json::Value) -> Choice {
    if saved.is_null() {
        return Choice::System;
    }
    if let Some(choice) = saved.as_str().and_then(Choice::from_key) {
        return choice;
    }
    // An unreadable value is treated like no choice: follow the system language.
    super::diagnose("i18n: unknown language setting, following the system language");
    Choice::System
}
fn initial() -> State {
    let saved = library::settings::value("language");
    let choice = saved_choice(&saved);
    State {
        choice,
        language: choice.resolve(&preferred_languages()),
    }
}
pub fn language() -> Language {
    current().language
}
pub fn choice() -> Choice {
    current().choice
}
pub fn choose(choice: Choice) -> Result<(), String> {
    // Save only this app preference, using the existing atomic, merging settings writer.
    library::settings::set_checked("language", serde_json::json!(choice.key()))?;
    let state = State {
        choice,
        language: choice.resolve(&preferred_languages()),
    };
    GENERATION.fetch_add(1, Ordering::Relaxed);
    // tests run in parallel: their switch stays on their own thread
    #[cfg(test)]
    TEST_STATE.with(|s| *s.borrow_mut() = Some(state));
    #[cfg(not(test))]
    {
        *STATE.write().unwrap_or_else(|e| e.into_inner()) = Some(state);
    }
    Ok(())
}

#[cfg(test)]
pub struct TestLanguage(Option<State>);
#[cfg(test)]
impl TestLanguage {
    pub fn set(language: Language) -> Self {
        let old = TEST_STATE.with(|s| {
            s.replace(Some(State {
                choice: match language {
                    Language::De => Choice::De,
                    Language::En => Choice::En,
                    Language::Ja => Choice::Ja,
                    Language::Fr => Choice::Fr,
                    Language::Es => Choice::Es,
                },
                language,
            }))
        });
        Self(old)
    }
}
#[cfg(test)]
impl Drop for TestLanguage {
    fn drop(&mut self) {
        TEST_STATE.with(|s| s.replace(self.0.take()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Unsupported system languages fall back to English (DPReview, Windows user: German UI on an
    // English system and no way found to switch). An explicit choice stays as saved.
    fn language_resolution_is_pure_and_has_english_fallback() {
        let prefs = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(Choice::System.resolve(&prefs(&["de-AT"])), Language::De);
        assert_eq!(Choice::System.resolve(&prefs(&["en-GB"])), Language::En);
        assert_eq!(
            Choice::System.resolve(&prefs(&["fr-FR", "en-US"])),
            Language::Fr
        );
        assert_eq!(Choice::System.resolve(&prefs(&["ja-JP"])), Language::Ja);
        assert_eq!(Choice::System.resolve(&prefs(&["es-419"])), Language::Es);
        assert_eq!(Choice::System.resolve(&prefs(&["it-IT", "en-US"])), Language::En);
        assert_eq!(Choice::System.resolve(&prefs(&["it-IT"])), Language::En);
        assert_eq!(Choice::System.resolve(&prefs(&["it-IT", "de-CH"])), Language::De);
        assert_eq!(Choice::System.resolve(&[]), Language::En);
        assert_eq!(Choice::De.resolve(&prefs(&["en"])), Language::De);
        assert_eq!(Choice::En.resolve(&prefs(&["de"])), Language::En);
        assert!(Choice::from_key("../photo").is_none());
        assert_eq!(Choice::from_key("system"), Some(Choice::System));
    }
    #[test]
    // An unreadable saved value no longer forces German; it follows the system language like a
    // missing value (which itself falls back to English). Valid saved choices are kept.
    fn invalid_saved_language_follows_system() {
        assert_eq!(saved_choice(&serde_json::Value::Null), Choice::System);
        assert_eq!(saved_choice(&serde_json::json!("en")), Choice::En);
        assert_eq!(saved_choice(&serde_json::json!("de")), Choice::De);
        assert_eq!(saved_choice(&serde_json::json!(42)), Choice::System);
        assert_eq!(saved_choice(&serde_json::json!("unknown")), Choice::System);
    }
    #[test]
    fn windows_multi_string_is_split() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(split_multi_string(&w("de-DE\0en-US\0\0")), vec!["de-DE", "en-US"]);
        assert_eq!(split_multi_string(&w("en-GB\0\0")), vec!["en-GB"]);
        assert!(split_multi_string(&w("\0\0")).is_empty());
        assert!(split_multi_string(&[]).is_empty());
        let langs = split_multi_string(&w("it-IT\0fr-FR\0\0"));
        assert_eq!(Choice::System.resolve(&langs), Language::Fr);
    }
    #[test]
    fn posix_language_list_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        // LANGUAGE is a ':' list and comes first
        assert_eq!(
            posix_languages(env(&[("LANGUAGE", "fr:en_GB"), ("LANG", "de_DE.UTF-8")])),
            vec!["fr", "en_GB", "de_DE"]
        );
        // LC_ALL beats LC_MESSAGES beats LANG
        assert_eq!(
            posix_languages(env(&[("LC_ALL", "ja_JP.UTF-8"), ("LC_MESSAGES", "fr_FR"), ("LANG", "de_DE")])),
            vec!["ja_JP"]
        );
        assert_eq!(posix_languages(env(&[("LC_MESSAGES", "es_ES@euro"), ("LANG", "de_DE")])), vec!["es_ES"]);
        // C/POSIX locale: LANGUAGE is ignored, nothing is preferred -> English fallback
        assert!(posix_languages(env(&[("LANGUAGE", "de"), ("LANG", "C.UTF-8")])).is_empty());
        assert!(posix_languages(env(&[("LANGUAGE", "de")])).is_empty());
        assert!(posix_languages(env(&[])).is_empty());
        assert_eq!(Choice::System.resolve(&posix_languages(env(&[("LANG", "C")]))), Language::En);
    }
    #[test]
    fn ui_override_is_thread_local_and_restored() {
        let outer = TestLanguage::set(Language::De);
        {
            let _inner = TestLanguage::set(Language::En);
            assert_eq!(language(), Language::En);
        }
        assert_eq!(language(), Language::De);
        drop(outer);
    }
}

/// Internal native-window test aid; inert without an explicitly isolated data directory.
/// No OS input automation: changes only our own viewport and our own UI preference.
pub fn test_frame(ctx: &eframe::egui::Context) -> bool {
    use eframe::egui;
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return false;
    }
    let size_id = egui::Id::unique("i18n-test-window-size");
    if !ctx.data(|d| d.get_temp::<bool>(size_id).unwrap_or(false)) {
        if let Ok(value) = crate::env::var("GRAINEO_I18N_TEST_SIZE") {
            if let Some((w, h)) = value
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    w.clamp(900, 3000) as f32,
                    h.clamp(600, 2000) as f32,
                )));
                ctx.data_mut(|d| d.insert_temp(size_id, true));
            }
        }
    }
    if crate::env::var("GRAINEO_I18N_TEST_SWITCH").as_deref() != Ok("1") {
        return false;
    }
    let now = ctx.input(|i| i.time);
    let stage = if now < 1.0 {
        0u8
    } else if now < 3.0 {
        1
    } else {
        2
    };
    let id = egui::Id::unique("i18n-native-switch-stage");
    let previous = ctx.data(|d| d.get_temp::<u8>(id));
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    if previous == Some(stage) {
        return false;
    }
    let choice = if stage == 1 { Choice::En } else { Choice::De };
    match choose(choice) {
        Ok(()) => {
            ctx.data_mut(|d| d.insert_temp(id, stage));
            true
        }
        Err(_) => {
            eprintln!("i18n-native: language setting not writable");
            false
        }
    }
}

#[cfg(test)]
mod global_tests {
    /// The language must be one process-wide state (worker threads create UI texts; a switch at runtime
    /// must reach them). A per-thread language outside the tests would bring back the mixed-language UI.
    #[test]
    fn language_state_is_process_wide() {
        let src = include_str!("locale.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("static STATE: RwLock"));
        assert!(!production.contains("thread_local!"));
    }
}
