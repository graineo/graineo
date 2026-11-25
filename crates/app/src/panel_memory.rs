//! Remembers across restarts whether the left and right side panels are open (F7/F8, the panel buttons). One global
//! value `sidePanels` in `settings.json`, as the library's view (`library_view_memory.rs`): written only when it
//! changed and no mouse button is held, on a background thread so the UI never waits for the file.
//!
//! Not remembered: the panels hidden by the full preview (F) and the automatic collapse of a narrow window; both
//! are temporary states, the file keeps the user's own choice. Unit tests build apps in parallel with one shared
//! settings file, so the memory is off in tests (the panels start open there, as before).

use serde_json::{json, Value};
use std::sync::Mutex;

const KEY: &str = "sidePanels";

/// (left open, right open)
pub type Panels = (bool, bool);

pub fn to_json(p: Panels) -> Value {
    json!({ "left": p.0, "right": p.1 })
}

/// Fills in what `v` holds; missing or broken parts keep the value of `base`.
pub fn merged(base: Panels, v: &Value) -> Panels {
    (v.get("left").and_then(Value::as_bool).unwrap_or(base.0), v.get("right").and_then(Value::as_bool).unwrap_or(base.1))
}

/// Latest value waiting to be written; the writer holds the lock while writing, so the last change always wins.
static PENDING: Mutex<Option<Value>> = Mutex::new(None);

fn save(v: Value) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
    std::thread::spawn(|| {
        let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = p.take() {
            library::settings::set(KEY, v);
        }
    });
}

impl crate::App {
    /// At startup: open or close the side panels as at the end of the last session (before the test hooks).
    pub(crate) fn restore_panels(&mut self) {
        if cfg!(test) {
            return;
        }
        let p = merged((self.show_left, self.show_right), &library::settings::value(KEY));
        (self.show_left, self.show_right) = p;
        self.panels_saved = Some(p);
    }

    /// The narrow window closed a panel by itself: not a choice of the user, so it is not written.
    pub(crate) fn panels_changed_by_the_app(&mut self) {
        if self.panels_saved.is_some() {
            self.panels_saved = Some((self.show_left, self.show_right));
        }
    }

    /// Once per frame: write the panels when they changed (not during the full preview, not while the mouse is down).
    pub(crate) fn remember_panels(&mut self, ctx: &eframe::egui::Context) {
        let p = (self.show_left, self.show_right);
        if self.panels_saved.is_none() || self.panels_saved == Some(p) || self.preview_full.is_some() || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.panels_saved = Some(p);
        save(to_json(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_round_trip_through_settings_text() {
        for p in [(true, true), (false, true), (true, false), (false, false)] {
            let text = serde_json::to_string_pretty(&json!({ KEY: to_json(p) })).unwrap();
            let back: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(merged((true, true), &back[KEY]), p);
        }
    }

    #[test]
    fn missing_or_broken_value_keeps_the_panels_open() {
        assert_eq!(merged((true, true), &Value::Null), (true, true));
        assert_eq!(merged((true, true), &json!({ "left": "no", "right": 0 })), (true, true));
        assert_eq!(merged((true, true), &json!({ "right": false })), (true, false));
    }
}
