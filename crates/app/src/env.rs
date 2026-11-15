//! The app's `GRAINEO_*` switches (test hooks, measurements), read once from the environment.
//!
//! Many of them are asked in every frame; `getenv` takes a process-wide lock and searches the whole environment each
//! time. The app never changes these variables while it runs, so one snapshot at the first use is enough. Same
//! signatures as `std::env::var`/`var_os`, so call sites stay as they were.

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::OnceLock;

fn all() -> &'static HashMap<OsString, OsString> {
    static VARS: OnceLock<HashMap<OsString, OsString>> = OnceLock::new();
    VARS.get_or_init(|| std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(|k| k.starts_with("GRAINEO_"))).collect())
}

/// Like `std::env::var_os` for an `GRAINEO_*` variable (other names are read from the environment as before).
pub fn var_os(name: &str) -> Option<OsString> {
    if !name.starts_with("GRAINEO_") {
        return std::env::var_os(name);
    }
    all().get(std::ffi::OsStr::new(name)).cloned()
}

/// Like `std::env::var` for an `GRAINEO_*` variable.
pub fn var(name: &str) -> Result<String, std::env::VarError> {
    match var_os(name) {
        None => Err(std::env::VarError::NotPresent),
        Some(v) => v.into_string().map_err(std::env::VarError::NotUnicode),
    }
}

/// Test hook for screenshots (own data folder only): `GRAINEO_HELP_OPEN=<card title>` opens the help card with this
/// German or translated title once as soon as its "?" is drawn; `!` opens the condition card of the photo.
pub fn help_open_test(title: &str, tr: impl Fn(&str) -> String) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WANTED: OnceLock<Option<String>> = OnceLock::new();
    static DONE: AtomicBool = AtomicBool::new(false);
    let Some(w) = WANTED.get_or_init(|| var_os("GRAINEO_DATA_DIR").and_then(|_| var("GRAINEO_HELP_OPEN").ok())) else {
        return false;
    };
    if DONE.load(Ordering::Relaxed) || (title != w && title != tr(w)) {
        return false;
    }
    DONE.store(true, Ordering::Relaxed);
    true
}

/// Test hook for screenshots (own data folder only): `GRAINEO_HELP_CARD=<slider or section title>` shows that card.
pub fn help_card_test() -> Option<String> {
    var_os("GRAINEO_DATA_DIR").and_then(|_| var("GRAINEO_HELP_CARD").ok())
}
