//! System folders in one place. macOS: `~/Library/Application Support`, `~/Library/Caches`,
//! `~/Library/Logs`, `/Library/Application Support`. Windows: `%APPDATA%` (Roaming), `%LOCALAPPDATA%`
//! (cache, `…\Cache`) and `%LOCALAPPDATA%\…\Logs`, `%PROGRAMDATA%`.
//! The functions return the base; the caller appends the program name (`graineo`).

use std::path::PathBuf;

/// Name of the app's folders (Application Support, Caches, Logs) – in one place, so the app can be renamed.
pub const APP: &str = "graineo";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The user's home folder (`HOME`, on Windows `USERPROFILE`).
pub fn home() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

/// Per-user application data (macOS `~/Library/Application Support`, Windows `%APPDATA%`).
pub fn app_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("APPDATA")
    } else {
        home().map(|h| h.join("Library/Application Support"))
    }
}

/// Cache (macOS `~/Library/Caches`, Windows `%LOCALAPPDATA%`). On Windows the app's folder lives
/// inside it; the caller appends `graineo`, on Windows also `Cache` via [`app_cache_dir`].
pub fn cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("LOCALAPPDATA")
    } else {
        home().map(|h| h.join("Library/Caches"))
    }
}

/// Cache folder of an app (macOS `~/Library/Caches/<app>`, Windows `%LOCALAPPDATA%\<app>\Cache`).
/// Test and development runs with their own data folder (`GRAINEO_DATA_DIR`) keep every cache inside it
/// (`<DATA>/cache`), never in the user's real cache.
pub fn app_cache_dir(app: &str) -> Option<PathBuf> {
    if let Some(d) = env_path("GRAINEO_DATA_DIR") {
        return Some(d.join("cache"));
    }
    let b = cache_dir()?.join(app);
    Some(if cfg!(windows) { b.join("Cache") } else { b })
}

/// Log folder of an app (macOS `~/Library/Logs/<app>`, Windows `%LOCALAPPDATA%\<app>\Logs`).
pub fn app_logs_dir(app: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("LOCALAPPDATA").map(|l| l.join(app).join("Logs"))
    } else {
        home().map(|h| h.join("Library/Logs").join(app))
    }
}

/// Application data for all users (macOS `/Library/Application Support`, Windows `%PROGRAMDATA%`).
pub fn system_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("PROGRAMDATA")
    } else {
        Some(PathBuf::from("/Library/Application Support"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run with its own data folder keeps every cache in it, never in the user's real cache.
    #[test]
    fn data_dir_run_keeps_caches_inside_it() {
        let data = std::env::temp_dir().join(format!("graineo-sysdirs-{}", std::process::id()));
        // only this test changes the variable in this crate's test binary
        std::env::set_var("GRAINEO_DATA_DIR", &data);
        let c = app_cache_dir(APP).unwrap();
        std::env::remove_var("GRAINEO_DATA_DIR");
        assert_eq!(c, data.join("cache"));
        if let Some(real) = cache_dir() {
            assert!(!c.starts_with(real), "{c:?}");
        }
        // without it, the normal place
        assert_ne!(app_cache_dir(APP), Some(data.join("cache")));
    }
}
