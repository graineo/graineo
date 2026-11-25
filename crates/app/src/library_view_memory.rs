//! Remembers the library's view across restarts: sort order and direction, thumbnail size and whether the
//! metadata columns are open. One global value `libraryView` in `settings.json` (not per folder or collection:
//! the order the user picked stays when switching sources, which is what one expects after a restart).
//!
//! Written only when the value changed and no mouse button is held (a slider drag writes once at the end),
//! on a background thread so the UI never waits for the file.

use super::{LibraryView, SortOrder};
use serde_json::{json, Value};
use std::sync::Mutex;

const KEY: &str = "libraryView";
const CELL_MIN: f32 = 110.0;
const CELL_MAX: f32 = 380.0;

/// The remembered part of the view.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct ViewState {
    pub sort: SortOrder,
    pub reversed: bool,
    pub cell_size: f32,
    pub columns_open: bool,
}

impl SortOrder {
    fn key(self) -> &'static str {
        match self {
            SortOrder::CaptureTime => "captureTime",
            SortOrder::Name => "name",
            SortOrder::Stars => "stars",
        }
    }
    fn from_key(k: &str) -> Option<Self> {
        Some(match k {
            "captureTime" => SortOrder::CaptureTime,
            "name" => SortOrder::Name,
            "stars" => SortOrder::Stars,
            _ => return None,
        })
    }
}

impl ViewState {
    pub fn to_json(self) -> Value {
        json!({ "sort": self.sort.key(), "reversed": self.reversed, "cellSize": self.cell_size.round(), "columnsOpen": self.columns_open })
    }

    /// Fills in what `v` holds; missing or broken parts keep the value of `self`.
    pub fn merged(self, v: &Value) -> Self {
        ViewState {
            sort: v.get("sort").and_then(Value::as_str).and_then(SortOrder::from_key).unwrap_or(self.sort),
            reversed: v.get("reversed").and_then(Value::as_bool).unwrap_or(self.reversed),
            cell_size: v.get("cellSize").and_then(Value::as_f64).map_or(self.cell_size, |c| (c as f32).clamp(CELL_MIN, CELL_MAX)),
            columns_open: v.get("columnsOpen").and_then(Value::as_bool).unwrap_or(self.columns_open),
        }
    }
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

impl LibraryView {
    pub(super) fn view_state(&self) -> ViewState {
        ViewState { sort: self.sort_order, reversed: self.sort_reversed, cell_size: self.cell_size, columns_open: self.columns_open }
    }

    /// At startup: take the remembered view (before the test aid, which may override it).
    /// Off in tests (one shared settings file, parallel tests), as `panel_memory.rs`.
    pub(super) fn restore_view(&mut self) {
        if cfg!(test) {
            return;
        }
        let s = self.view_state().merged(&library::settings::value(KEY));
        (self.sort_order, self.sort_reversed, self.cell_size, self.columns_open) = (s.sort, s.reversed, s.cell_size, s.columns_open);
        self.remembered_view = Some(s);
        self.refilter = true;
    }

    /// Once per frame: write the view when it changed (not while the mouse is down, e.g. during a slider drag).
    pub(super) fn remember_view(&mut self, ctx: &eframe::egui::Context) {
        let s = self.view_state();
        if self.remembered_view.is_none() || self.remembered_view == Some(s) || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.remembered_view = Some(s);
        save(s.to_json());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: ViewState = ViewState { sort: SortOrder::CaptureTime, reversed: false, cell_size: 180.0, columns_open: false };

    #[test]
    fn view_state_round_trip() {
        let s = ViewState { sort: SortOrder::Name, reversed: true, cell_size: 290.0, columns_open: true };
        assert_eq!(DEFAULT.merged(&s.to_json()), s);
        let s = ViewState { sort: SortOrder::Stars, ..DEFAULT };
        assert_eq!(DEFAULT.merged(&s.to_json()), s);
        // through text, as in settings.json
        let text = serde_json::to_string_pretty(&json!({ KEY: s.to_json() })).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(DEFAULT.merged(&back[KEY]), s);
    }

    #[test]
    fn view_state_missing_or_broken_keeps_defaults() {
        assert_eq!(DEFAULT.merged(&Value::Null), DEFAULT);
        assert_eq!(DEFAULT.merged(&json!({ "sort": "nonsense", "reversed": 3, "cellSize": "x" })), DEFAULT);
        assert_eq!(DEFAULT.merged(&json!({ "cellSize": 5000 })).cell_size, CELL_MAX);
    }
}
