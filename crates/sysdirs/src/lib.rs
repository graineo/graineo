//! System folders in one place. macOS: `~/Library/Application Support`, `~/Library/Caches`,
//! `~/Library/Logs`, `/Library/Application Support`. Windows: `%APPDATA%` (Roaming), `%LOCALAPPDATA%`
//! (cache, `…\Cache`) and `%LOCALAPPDATA%\…\Logs`, `%PROGRAMDATA%`.
//! Linux: XDG folders (`$XDG_DATA_HOME` or `~/.local/share`, `$XDG_CACHE_HOME` or `~/.cache`, logs in
//! `$XDG_STATE_HOME` or `~/.local/state`, system data `/usr/share`).
//! The functions return the base; the caller appends the program name (`graineo`).

use std::path::{Path, PathBuf};

/// Name of the app's folders (Application Support, Caches, Logs) – in one place, so the app can be renamed.
pub const APP: &str = "graineo";

/// Linux: an XDG base folder (the variable, if it is an absolute path, otherwise `~/<fallback>`).
#[cfg(target_os = "linux")]
fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    env_path(var).filter(|p| p.is_absolute()).or_else(|| home().map(|h| h.join(fallback)))
}

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
    } else if cfg!(target_os = "linux") {
        linux_data()
    } else {
        home().map(|h| h.join("Library/Application Support"))
    }
}

/// Cache (macOS `~/Library/Caches`, Windows `%LOCALAPPDATA%`). On Windows the app's folder lives
/// inside it; the caller appends `graineo`, on Windows also `Cache` via [`app_cache_dir`].
pub fn cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("LOCALAPPDATA")
    } else if cfg!(target_os = "linux") {
        linux_cache()
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
/// Test and development runs never write into the real logs folder: with `GRAINEO_DATA_DIR` the logs go to
/// `<DATA>/logs`, a cargo test program without it gets `<temp>/graineo-test-<pid>/logs` (same folder as the
/// library's safety net for test programs).
pub fn app_logs_dir(app: &str) -> Option<PathBuf> {
    if let Some(d) = env_path("GRAINEO_DATA_DIR") {
        return Some(d.join("logs"));
    }
    if let Some(d) = test_program_dir() {
        return Some(d.join("logs"));
    }
    real_logs_dir(app)
}

/// Is this a cargo test program (`<target>/<profile>/deps/…`)? Then its own folder in the temp directory.
pub fn test_program_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let by_cargo = std::env::var_os("CARGO").is_some() || std::env::var_os("CARGO_MANIFEST_DIR").is_some();
    is_test_program(&exe, by_cargo).then(|| std::env::temp_dir().join(format!("graineo-test-{}", std::process::id())))
}

/// A folder named `deps` alone is no proof: the app may be installed in one (e.g. `/opt/deps/graineo`), and then
/// its data would land in the temp directory. A cargo test program lies in `deps/` **and** is either started by
/// cargo (`cargo test` sets `CARGO`/`CARGO_MANIFEST_DIR` for it) or carries cargo's hash suffix
/// (`name-<16 hex digits>`, when started by hand).
fn is_test_program(exe: &Path, by_cargo: bool) -> bool {
    let in_deps = exe.parent().and_then(Path::file_name).is_some_and(|n| n == "deps");
    let stem = exe.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let hashed = stem.rsplit_once('-').is_some_and(|(_, h)| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    in_deps && (by_cargo || hashed)
}

/// The user's real logs folder, without the test redirection of [`app_logs_dir`].
fn real_logs_dir(app: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("LOCALAPPDATA").map(|l| l.join(app).join("Logs"))
    } else if cfg!(target_os = "linux") {
        linux_state().map(|s| s.join(app).join("logs"))
    } else {
        home().map(|h| h.join("Library/Logs").join(app))
    }
}

/// Application data for all users (macOS `/Library/Application Support`, Windows `%PROGRAMDATA%`).
pub fn system_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("PROGRAMDATA")
    } else if cfg!(target_os = "linux") {
        Some(PathBuf::from("/usr/share"))
    } else {
        Some(PathBuf::from("/Library/Application Support"))
    }
}

#[cfg(target_os = "linux")]
fn linux_data() -> Option<PathBuf> { xdg("XDG_DATA_HOME", ".local/share") }
#[cfg(target_os = "linux")]
fn linux_cache() -> Option<PathBuf> { xdg("XDG_CACHE_HOME", ".cache") }
#[cfg(target_os = "linux")]
fn linux_state() -> Option<PathBuf> { xdg("XDG_STATE_HOME", ".local/state") }
#[cfg(not(target_os = "linux"))]
fn linux_data() -> Option<PathBuf> { None }
#[cfg(not(target_os = "linux"))]
fn linux_cache() -> Option<PathBuf> { None }
#[cfg(not(target_os = "linux"))]
fn linux_state() -> Option<PathBuf> { None }

// ───────────────────────────────────────────── Paths for display and comparison

/// Text with the Windows "verbatim" path prefixes removed: `\\?\C:\x` → `C:\x`, `\\?\UNC\server\share` →
/// `\\server\share`. Other `\\?\` forms (volume GUIDs, devices) stay. Only for display and comparison: a path
/// longer than 260 characters needs the prefix for file access.
pub fn strip_verbatim(text: &str) -> std::borrow::Cow<'_, str> {
    const UNC: &str = "\\\\?\\UNC\\";
    const VERBATIM: &str = "\\\\?\\";
    if !text.contains(VERBATIM) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(VERBATIM) {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        if let Some(tail) = after.strip_prefix(UNC) {
            out.push_str("\\\\");
            rest = tail;
        } else {
            let tail = &after[VERBATIM.len()..];
            let b = tail.as_bytes();
            if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
                rest = tail;
            } else {
                out.push_str(VERBATIM);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// Does the path text point to a network place (UNC `\\\\server\\share`, `\\\\?\\UNC\\…`, `//server/share`, macOS
/// automount `/net/host/…`)? Such a path is never touched without a decision of the user (`is_dir`, `exists`):
/// Windows would connect to the server and send the login there. A text from a foreign file (catalog of another
/// program) counts only as text. Pure text test, the same on every system.
pub fn is_network_path(text: &str) -> bool {
    let t = strip_verbatim(text);
    let t = t.trim_start();
    if t.starts_with("\\\\") {
        return true;
    }
    if t.starts_with("//") && !t.starts_with("///") {
        return true;
    }
    t == "/net" || t.starts_with("/net/")
}

/// The path without the verbatim prefix (see [`strip_verbatim`]); unchanged on other systems. For display and
/// for comparing paths that came from `canonicalize` with ones that did not – never for opening files.
pub fn simplify(p: &std::path::Path) -> PathBuf {
    if !cfg!(windows) {
        return p.to_path_buf();
    }
    match p.to_str() {
        Some(t) => PathBuf::from(strip_verbatim(t).into_owned()),
        None => p.to_path_buf(),
    }
}

/// Path as shown to the user.
pub fn display_path(p: &std::path::Path) -> String {
    simplify(p).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deps_folder_alone_is_no_test_program() {
        let t = Path::new("/w/target/release/deps/library-0123456789abcdef");
        assert!(is_test_program(t, false));
        assert!(is_test_program(Path::new("/w/target/release/deps/x"), true));
        // the app installed in a folder named deps, not started by cargo
        assert!(!is_test_program(Path::new("/opt/deps/graineo"), false));
        assert!(!is_test_program(Path::new("/opt/deps/sub/graineo-0123456789abcdef"), true));
    }

    #[test]
    fn test_programs_never_get_the_real_logs_folder() {
        let logs = app_logs_dir(APP).unwrap();
        assert_ne!(Some(logs.clone()), real_logs_dir(APP));
        assert!(logs.starts_with(std::env::temp_dir()) || std::env::var_os("GRAINEO_DATA_DIR").is_some(), "{}", logs.display());
    }

    #[test]
    fn network_paths_are_recognised_as_text() {
        for p in ["\\\\server\\share\\Fotos", "\\\\?\\UNC\\server\\share", "//server/share/x", "/net/host/x", " \\\\h\\s", "\\\\?\\Volume{1234}\\"] {
            assert!(is_network_path(p), "{p}");
        }
        for p in ["C:\\Fotos", "\\\\?\\C:\\Fotos", "/Volumes/Bilder/", "/Users/x", "/network/x", "/netz/x", "relative/a", "///x", ""] {
            assert!(!is_network_path(p), "{p}");
        }
    }

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

    #[test]
    fn verbatim_prefixes_are_removed_for_display() {
        assert_eq!(strip_verbatim(r"\\?\C:\x\a.X3F"), r"C:\x\a.X3F");
        assert_eq!(strip_verbatim(r"\\?\UNC\server\share\a"), r"\\server\share\a");
        assert_eq!(strip_verbatim(r"\\?\Volume{1234}\a"), r"\\?\Volume{1234}\a");
        assert_eq!(strip_verbatim("/Users/x/a.jpg"), "/Users/x/a.jpg");
        // inside a message
        assert_eq!(strip_verbatim(r"\\?\C:\Users\a\Pictures\x.X3F: Access denied"), r"C:\Users\a\Pictures\x.X3F: Access denied");
        assert_eq!(strip_verbatim(r"von \\?\D:\a nach \\?\UNC\nas\fotos"), r"von D:\a nach \\nas\fotos");
    }
}
