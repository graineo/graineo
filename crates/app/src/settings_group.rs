//! Develop settings for several photos in one step:
//! - "Transfer settings from the main photo" (⌥⇧⌘V, button in the filmstrip header with a multiple selection): the chosen
//!   groups of the main photo (the active one) go onto all other selected photos;
//! - "Paste" (⇧⌘V) onto every selected photo;
//! - "Reset all settings" (⌥⇧⌘R).
//!
//! Each command is one entry of the shared undo stack (`library_turn::TurnUndo`): one ⌘Z takes it back for all photos,
//! in the grid and in Develop. Saving runs through the ordered background writer of the turns, the UI never waits for
//! the database.
//!
//! The groups of "Transfer" are remembered (`transferGroups`, by name). The shortcut applies them at once to fewer than
//! [`CONFIRM_FROM`] photos; from that many on, and always from the button, a dialog shows the groups and asks first.

use develop::{Settings, View};
use eframe::egui;
use std::sync::Mutex;

use crate::{paste, App};

/// From this number of target photos "Transfer" asks first.
pub const CONFIRM_FROM: usize = 25;

/// Settings key of the remembered groups.
const KEY: &str = "transferGroups";

/// One photo of a settings command: settings before and after (virtual copies by name).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub image: i64,
    pub copy: Option<String>,
    pub before: Settings,
    pub after: Settings,
}

/// Open "Transfer" dialog: the groups being chosen.
static DIALOG: Mutex<Option<paste::Groups>> = Mutex::new(None);

/// "Reset all settings" asked for from a place that holds the settings borrowed (button of the Edit panel).
static RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask for "Reset all settings" of the open photo (done in the next frame, undoable).
pub fn request_reset() {
    RESET.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Remembered groups of "Transfer" (first time: everything except crop, as for copying).
pub fn remembered() -> paste::Groups {
    match library::settings::value(KEY) {
        serde_json::Value::Array(v) => paste::Groups::from_names(v.iter().filter_map(|x| x.as_str())),
        _ => paste::Groups::default(),
    }
}

fn remember(g: &paste::Groups) {
    library::settings::set(KEY, serde_json::json!(g.names()));
}

/// Does the shortcut have to ask first?
pub fn needs_confirmation(targets: usize) -> bool {
    targets >= CONFIRM_FROM
}

/// Settings as stored (the view is not part of them).
fn stored(s: &Settings) -> Settings {
    Settings { view: View::Developed, ..s.clone() }
}

/// The changes of one command: per target its settings before and after; photos that do not change are left out.
pub fn plan(items: &[(i64, Option<String>, Settings)], f: impl Fn(&Settings) -> Settings) -> Vec<Change> {
    items
        .iter()
        .filter_map(|(image, copy, s)| {
            let before = stored(s);
            let after = stored(&f(s));
            (after != before).then(|| Change { image: *image, copy: copy.clone(), before, after })
        })
        .collect()
}

impl App {
    /// Index of the main photo: the open one in the single view, the active one in the grid.
    fn main_photo(&self) -> Option<usize> {
        if self.in_grid() {
            let a = self.lib.active_image()?;
            return self.entries.iter().position(|e| e.image == a);
        }
        self.current.as_ref().map(|c| c.index)
    }

    /// The selected photos that can take settings (without the main photo): (targets, left out).
    fn transfer_targets(&self, main: usize) -> (Vec<usize>, usize) {
        let mut skipped = 0;
        let targets = self
            .selection()
            .into_iter()
            .filter(|&i| i != main)
            .filter(|&i| {
                let ok = self.entries.get(i).is_some_and(|e| crate::image_kind::of(&e.path).editable() && crate::developable_state(e.state));
                if !ok {
                    skipped += 1;
                }
                ok
            })
            .collect();
        (targets, skipped)
    }

    /// ⌥⇧⌘V and the menu: transfer at once with the remembered groups, from [`CONFIRM_FROM`] photos on ask first.
    pub(crate) fn transfer_command(&mut self) {
        let Some(main) = self.main_photo() else { return };
        let (targets, _) = self.transfer_targets(main);
        if targets.is_empty() {
            self.status = crate::tr("Zum Übertragen weitere Fotos auswählen (Haupt-Foto + Auswahl)").to_string();
            return;
        }
        if needs_confirmation(targets.len()) {
            self.open_transfer_dialog();
        } else {
            self.transfer(remembered());
        }
    }

    /// Button "Transfer …": always with the choice of groups.
    pub(crate) fn open_transfer_dialog(&mut self) {
        *DIALOG.lock().unwrap() = Some(remembered());
    }

    /// Settings of the main photo (chosen groups) onto the other selected photos, one ⌘Z for all.
    fn transfer(&mut self, groups: paste::Groups) {
        let Some(main) = self.main_photo() else { return };
        self.ensure_entry(main);
        let src = self.entries[main].settings.clone();
        let name = self.entries[main].name.clone();
        let (targets, skipped) = self.transfer_targets(main);
        let cmd = crate::history::Command::new(crate::history::Hint::sync(&name, None));
        let n = self.settings_command(&targets, cmd, move |s| paste::merge(s, &src, &groups));
        let mut t = crate::trf!("Einstellungen von {a} auf {n} Fotos übertragen (⌘Z nimmt es zurück)", a = name, n = n);
        if skipped > 0 {
            t = crate::trf!("{t}; {k} übersprungen (Original nicht erreichbar oder nicht entwickelbar)", t = t, k = skipped);
        }
        self.status = t;
    }

    /// Every frame: a requested reset, the "Transfer" dialog while it is open.
    pub(crate) fn settings_group_frame(&mut self, ctx: &egui::Context) {
        if RESET.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.reset_command();
        }
        self.transfer_dialog(ctx);
    }

    /// Dialog of "Transfer" (only while open).
    fn transfer_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut groups) = *DIALOG.lock().unwrap() else { return };
        let Some(main) = self.main_photo() else {
            *DIALOG.lock().unwrap() = None;
            return;
        };
        let (targets, skipped) = self.transfer_targets(main);
        let name = self.entries[main].name.clone();
        let n = targets.len();
        let title = crate::trf!("Einstellungen von {a} übertragen", a = name);
        let ok = groups.count() > 0 && n > 0;
        let a = crate::dialog::show(
            ctx,
            "uebertragen",
            &title,
            700.0,
            |ui| {
                crate::dialog::text(ui, &crate::trf!("Die gewählten Bereiche des Haupt-Fotos ersetzen die Einstellungen von {n} weiteren ausgewählten Fotos. ⌘Z nimmt alles in einem Schritt zurück.", n = n));
                paste::group_choice(ui, "uebertragen-gruppen", &mut groups);
                if skipped > 0 {
                    crate::dialog::hint(ui, &crate::trf!("{k} ausgewählte Fotos bleiben unverändert (Original nicht erreichbar oder nicht entwickelbar).", k = skipped));
                }
                crate::dialog::hint(ui, &crate::trf!("{k} überträgt die zuletzt gewählten Bereiche ohne diesen Dialog (ab {m} Fotos mit Rückfrage).", k = crate::shortcuts::display("sync_settings"), m = CONFIRM_FROM));
            },
            crate::dialog::Buttons::confirm(crate::tr("Übertragen"), ok),
        );
        match a {
            crate::dialog::Answer::Open => *DIALOG.lock().unwrap() = Some(groups),
            crate::dialog::Answer::Primary if ok => {
                *DIALOG.lock().unwrap() = None;
                remember(&groups);
                self.transfer(groups);
            }
            _ => *DIALOG.lock().unwrap() = None,
        }
    }

    /// ⌥⇧⌘R and the button "Reset all settings": the open photo (in the grid the active one) back to its defaults,
    /// undoable with ⌘Z in every view.
    pub(crate) fn reset_command(&mut self) {
        let Some(i) = self.main_photo().or(self.current.as_ref().map(|c| c.index)) else { return };
        if !self.entries.get(i).is_some_and(|e| crate::image_kind::of(&e.path).editable() && crate::developable_state(e.state)) {
            return;
        }
        let path = self.entries[i].path.clone();
        // a camera without the colour mode "Neutral" (older Foveon cameras with Standard only) keeps its own
        let no_neutral = self.current.as_ref().filter(|c| c.index == i).is_some_and(|c| c.loaded.kind == crate::image_kind::ImageKind::Foveon && !c.loaded.cal.modes.iter().any(|m| m.name == crate::sidecar::NEUTRAL_MODE));
        let reset = move |s: &Settings| {
            let mut r = crate::sidecar::reset(s.view, &path);
            if no_neutral {
                r.color_mode = None;
            }
            r
        };
        if self.settings_command(&[i], crate::history::Command::new(crate::history::Hint::reset()), reset) > 0 {
            self.status = crate::tr("Alle Einstellungen zurückgesetzt (⌘Z nimmt es zurück)").to_string();
        }
    }

    /// "Reset to camera default": neutral values, then the global start-value preferences and the camera default
    /// (camera_defaults.rs), as one undoable command.
    pub(crate) fn reset_to_camera_default(&mut self) {
        let Some(c) = self.current.as_ref() else { return };
        let i = c.index;
        let Some(camera) = crate::camera_defaults::camera_of(&c.loaded) else { return };
        let foreign_raw = c.loaded.previews.foreign.as_ref().is_some_and(|d| !d.finished);
        let no_neutral = c.loaded.kind == crate::image_kind::ImageKind::Foveon && !c.loaded.cal.modes.iter().any(|m| m.name == crate::sidecar::NEUTRAL_MODE);
        let path = self.entries[i].path.clone();
        let f = move |s: &Settings| {
            let mut r = crate::sidecar::reset(s.view, &path);
            if no_neutral {
                r.color_mode = None;
            }
            crate::camera_defaults::start_values(&r, Some(&camera), foreign_raw)
        };
        if self.settings_command(&[i], crate::history::Command::new(crate::history::Hint::reset()), f) > 0 {
            self.status = crate::tr("Auf Kamera-Standard zurückgesetzt (⌘Z nimmt es zurück)").to_string();
        }
    }

    /// New settings for the given photos as one undoable command. Returns the number of photos that changed.
    /// Many photos (more than [`crate::multi_edit::RIGHT_AWAY_MAX`]) go through the background: loaded, changed and saved
    /// there block by block; the undo entry follows when it is done.
    pub(crate) fn settings_command(&mut self, targets: &[usize], cmd: crate::history::Command, f: impl Fn(&Settings) -> Settings + Send + Sync + 'static) -> usize {
        if targets.len() > crate::multi_edit::RIGHT_AWAY_MAX {
            let list = targets.iter().map(|&i| crate::multi_apply::Target { image: self.entries[i].image, copy: self.entries[i].copy.clone(), path: self.entries[i].path.clone(), set: None }).collect();
            self.multi_submit(vec![std::sync::Arc::new(f) as crate::multi_apply::Step], list, true, Some((cmd, false)));
            self.lib.rating_not_recent();
            return targets.len();
        }
        for &i in targets {
            self.ensure_entry(i);
        }
        let items: Vec<(i64, Option<String>, Settings)> = targets.iter().map(|&i| (self.entries[i].image, self.entries[i].copy.clone(), self.entries[i].settings.clone())).collect();
        let changes = plan(&items, &f);
        let n = self.apply_changes(&changes, true, &cmd);
        // whatever was undoable before (rating, move) now comes after this command
        self.lib.rating_not_recent();
        self.turn_undo.record_settings(changes, cmd);
        n
    }

    /// Put the photos of a command into the state after (`forward`) or before it.
    /// The develop history gets a step per photo (or its cursor moves back / forward for undo / redo), see `history.rs`.
    pub(crate) fn apply_changes(&mut self, changes: &[Change], forward: bool, cmd: &crate::history::Command) -> usize {
        if changes.len() > crate::multi_edit::RIGHT_AWAY_MAX {
            // many photos: in the background, block by block (undo/redo of a command for many photos)
            let pos: std::collections::HashMap<(i64, Option<&str>), usize> = self.entries.iter().enumerate().map(|(i, e)| ((e.image, e.copy.as_deref()), i)).collect();
            let list: Vec<crate::multi_apply::Target> = changes
                .iter()
                .filter_map(|c| pos.get(&(c.image, c.copy.as_deref())).map(|&i| (c, i)))
                .map(|(c, i)| crate::multi_apply::Target { image: c.image, copy: c.copy.clone(), path: self.entries[i].path.clone(), set: Some(if forward { c.after.clone() } else { c.before.clone() }) })
                .collect();
            let n = list.len();
            self.multi_submit(Vec::new(), list, false, Some((cmd.clone(), !forward)));
            return n;
        }
        let mut n = 0;
        for c in changes {
            let Some(i) = self.entries.iter().position(|e| e.image == c.image && e.copy == c.copy) else { continue };
            self.ensure_entry(i);
            let (from, to) = if forward { (&c.before, &c.after) } else { (&c.after, &c.before) };
            self.history_command(i, from, to, cmd, !forward);
            self.set_settings(i, to);
            n += 1;
        }
        self.compute_thumbnails();
        n
    }

    /// New settings for one photo: entry, background save, preview; the open photo does not count this as an edit of
    /// its own (its undo history stays as it is, the command has its own ⌘Z).
    fn set_settings(&mut self, i: usize, to: &Settings) {
        let view = self.entries[i].settings.view;
        {
            let e = &mut self.entries[i];
            e.settings = Settings { view, ..to.clone() };
            e.stored = true;
        }
        let e = &self.entries[i];
        crate::library_turn::save_background(e.path.clone(), e.copy.clone(), stored(&e.settings), e.meta.clone());
        self.saved.insert(e.image, (stored(&e.settings), e.meta.clone()));
        if let Some((ci, c)) = self.committed.as_mut() {
            if *ci == i {
                *c = stored(&self.entries[i].settings);
            }
        }
        if self.current.as_ref().is_none_or(|c| c.index != i) {
            self.cache.retain(|(ci, _)| *ci != i);
        }
        self.queue_thumbnail(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_leaves_out_unchanged_photos_and_keeps_before_and_after() {
        let a = Settings { exposure: 0.5, ..Settings::default() };
        let b = Settings { exposure: 1.0, ..Settings::default() };
        let items = vec![(1, None, a.clone()), (2, Some("Kopie 1".to_string()), b.clone())];
        let changes = plan(&items, |s| Settings { exposure: 1.0, ..s.clone() });
        assert_eq!(changes.len(), 1, "photo 2 already has the value");
        assert_eq!(changes[0].image, 1);
        assert_eq!(changes[0].before.exposure, 0.5);
        assert_eq!(changes[0].after.exposure, 1.0);
    }

    #[test]
    fn plan_ignores_the_view() {
        let s = Settings { view: View::Developed, ..Settings::default() };
        let changes = plan(&[(1, None, s)], |s| Settings { view: View::RawComposite, ..s.clone() });
        assert!(changes.is_empty());
    }

    #[test]
    fn confirmation_from_25_photos() {
        assert!(!needs_confirmation(24));
        assert!(needs_confirmation(25));
    }

    #[test]
    fn one_undo_entry_for_all_photos_of_a_transfer() {
        let mut u = crate::library_turn::TurnUndo::default();
        let items: Vec<_> = (0..30).map(|i| (i, None, Settings::default())).collect();
        let changes = plan(&items, |s| Settings { exposure: 2.0, ..s.clone() });
        u.record_settings(changes.clone(), crate::history::Command::new(crate::history::Hint::reset()));
        match u.undo() {
            Some(crate::library_turn::Undoable::Settings(c, _)) => assert_eq!(c.len(), 30),
            other => panic!("settings command expected, got {other:?}"),
        }
        assert!(u.undo().is_none());
    }
}
