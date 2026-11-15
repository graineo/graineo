//! Program settings as a readable file `settings.json` in the data folder (English keys).
//!
//! Reading is lenient (missing or broken file = no settings). Writing is not:
//! an unreadable file is never overwritten but first backed up as `settings.json.broken-<time>`;
//! writes are atomic, changes from several threads are not lost.

use serde_json::{json, Map, Value};
use std::sync::Mutex;

pub fn path() -> std::path::PathBuf {
    crate::data_dir().join("settings.json")
}

/// File as object: `Ok(None)` = not present, `Err` = present but not a valid JSON object.
fn read_strict(p: &std::path::Path) -> Result<Option<Map<String, Value>>, String> {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(mut v @ Value::Object(_)) => {
            // keys written by graineo 1.0 (normally translated once at the start, see [`migrate_file`])
            legacy_names::rename_keys(&mut v, legacy_names::settings_keys::SETTINGS_FILE);
            Ok(v.as_object().cloned())
        }
        Ok(_) => Err(format!("{}: kein JSON-Objekt", p.display())),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// Translate keys of graineo 1.0 in `settings.json` once: a copy of the old file stays next to it
/// (`settings.json.before-1.0.1`). Returns whether the file was rewritten.
pub fn migrate_file(p: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(p) else { return false };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else { return false };
    if !legacy_names::rename_keys(&mut v, legacy_names::settings_keys::SETTINGS_FILE) {
        return false;
    }
    let backup = p.with_file_name("settings.json.before-1.0.1");
    if !backup.exists() && std::fs::copy(p, &backup).is_err() {
        return false;
    }
    crate::files::write_atomic(p, serde_json::to_string_pretty(&v).unwrap_or_default().as_bytes()).is_ok()
}

pub fn read() -> Value {
    read_strict(&path()).ok().flatten().map_or(json!({}), Value::Object)
}

pub fn value(cache_key: &str) -> Value {
    read().get(cache_key).cloned().unwrap_or(Value::Null)
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Set a value; `Err` with reason if it could not be written.
pub fn set_checked(cache_key: &str, w: Value) -> Result<(), String> {
    let _lock = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let p = path();
    let mut o = match read_strict(&p) {
        Ok(o) => o.unwrap_or_default(),
        Err(reason) => {
            // lose nothing: put the unreadable file aside, then start fresh
            let safe = p.with_file_name(format!("settings.json.defekt-{}", crate::now()));
            std::fs::rename(&p, &safe).map_err(|e| format!("{reason}; Sicherung: {e}"))?;
            Map::new()
        }
    };
    o.insert(cache_key.to_string(), w);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(o)).map_err(|e| e.to_string())?;
    crate::files::write_atomic(&p, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
}

/// Like [`set_checked`]; an error is only logged (settings are never more important than the program).
pub fn set(cache_key: &str, w: Value) {
    if let Err(e) = set_checked(cache_key, w) {
        eprintln!("setting {cache_key} not saved: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_read_distinguishes_missing_and_broken() {
        let d = std::env::temp_dir().join(format!("graineo-einst-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("settings.json");
        assert_eq!(read_strict(&p), Ok(None));
        for text in ["[]", "false", "42", "\"x\"", "null", "{kaputt"] {
            std::fs::write(&p, text).unwrap();
            assert!(read_strict(&p).is_err(), "{text}");
        }
        std::fs::write(&p, r#"{"a":1}"#).unwrap();
        assert_eq!(read_strict(&p).unwrap().unwrap().get("a"), Some(&json!(1)));
        std::fs::remove_dir_all(d).unwrap();
    }
}
