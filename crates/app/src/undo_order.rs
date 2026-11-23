//! ⌘Z / ⇧⌘Z by time, not by kind: every undoable action carries the time it was made ([`tick`]), and ⌘Z takes back
//! the most recent one of all stacks – commands for several photos and turns (`library_turn.rs`), ratings
//! (`library_ui.rs`), managing the library and moves of this session (collections, keywords, quick collection, faces
//! named outside the people view, order …; in every view), deleted own presets and the develop history of the open
//! photo (`history.rs`). Moves of earlier sessions are never taken back by ⌘Z. ⇧⌘Z does again what was taken back last (commands and the develop history; the
//! other stacks have no redo). The people view keeps its own ⌘Z; text fields are not affected (they take ⌘Z before
//! the menu).

use std::sync::atomic::{AtomicU64, Ordering};

static LAST: AtomicU64 = AtomicU64::new(0);

/// Time of an action: milliseconds since 1970, strictly increasing within the session (two actions in the same
/// millisecond keep their order). Steps of the develop history from an earlier session compare by their own time.
pub fn tick() -> u64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mut prev = LAST.load(Ordering::SeqCst);
    loop {
        let next = ms.max(prev + 1);
        match LAST.compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(p) => prev = p,
        }
    }
}

/// The stacks ⌘Z can take an action from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stack {
    /// commands for several photos and turns
    Commands,
    Rating,
    /// managing the library and moves (grid)
    Library,
    /// develop history of the open photo
    Photo,
    /// own presets deleted ("VORGABE LÖSCHEN")
    Preset,
}

/// The stack with the most recent action (`None`: nothing to take back). Equal times keep the order of the list.
pub fn pick(candidates: &[(Stack, u64)]) -> Option<Stack> {
    candidates.iter().copied().fold(None, |best: Option<(Stack, u64)>, c| match best {
        Some(b) if b.1 >= c.1 => Some(b),
        _ => Some(c),
    }).map(|(s, _)| s)
}

impl crate::App {
    /// What ⌘Z could take back now, with the time it was done.
    pub(crate) fn undo_candidates(&self) -> Vec<(Stack, u64)> {
        let mut v = Vec::new();
        // turns are taken back only in the grid (in Develop ⌘Z belongs to the photo)
        if let Some((t, turn)) = self.turn_undo.undo_stamp() {
            if !turn || self.in_grid() {
                v.push((Stack::Commands, t));
            }
        }
        if let Some(t) = self.lib.rating_stamp() {
            v.push((Stack::Rating, t));
        }
        // managing the library and moves of this session count in every view (a keyword assigned in Develop is
        // taken back there too); journals of earlier sessions never count
        if let Some(t) = self.lib.external_stamp() {
            v.push((Stack::Library, t));
        }
        if let Some(t) = self.preset_undo.last().map(|p| p.0) {
            v.push((Stack::Preset, t));
        }
        if let Some(i) = self.current.as_ref().map(|c| c.index) {
            let key = self.history_key(i);
            // the step of a command for several photos is taken back with the command (it moves the cursor too)
            let own = self.history.command_at_cursor(&key).0.is_some_and(|c| Some(c) == self.turn_undo.undo_command());
            if let Some(t) = self.history.undo_stamp(&key).filter(|_| !own) {
                v.push((Stack::Photo, t));
            }
        }
        v
    }

    /// ⌘Z: the most recent undoable action of all kinds.
    pub(crate) fn undo_latest(&mut self) {
        // a change for many photos is still being applied: stop it, then take back what was saved
        if self.multi_busy() {
            self.multi_cancel_then_undo();
            return;
        }
        // the comparison's tournament: ⌘Z takes back the last duel – from the menu too (the menu's key equivalent
        // reaches the app before the comparison's own keys; QA matrix 20.11.2026)
        if self.compare_undo_duel() {
            return;
        }
        match pick(&self.undo_candidates()) {
            Some(Stack::Commands) => {
                self.turn_undo.recent = true;
                self.undo_turn();
            }
            Some(Stack::Rating) => {
                self.lib.rating_recent();
                self.undo_rating();
            }
            Some(Stack::Library) => {
                // (the step may take a merged photo out: the develop settings of the open photo go with it)
                self.save_current();
                let a = self.lib.undo_external();
                self.library_actions(a);
            }
            Some(Stack::Photo) => self.undo(false),
            Some(Stack::Preset) => self.undo_preset_delete(),
            None => {}
        }
    }

    /// "VORGABE LÖSCHEN": take the own preset out of the list; ⌘Z puts it back.
    pub(crate) fn delete_own_preset(&mut self, i: usize) {
        if i >= self.presets.len() {
            return;
        }
        let p = self.presets.remove(i);
        crate::presets::save(&self.presets);
        self.status = crate::trf!("Vorgabe „{a}“ gelöscht – ⌘Z holt sie zurück", a = p.name);
        self.preset_undo.push((tick(), i, p));
        self.lib.rating_not_recent();
    }

    /// ⌘Z of a deleted own preset: back to its place in the list.
    pub(crate) fn undo_preset_delete(&mut self) {
        let Some((_, i, p)) = self.preset_undo.pop() else { return };
        self.status = crate::trf!("Rückgängig: Vorgabe „{a}“ gelöscht", a = p.name);
        self.presets.insert(i.min(self.presets.len()), p);
        crate::presets::save(&self.presets);
    }

    /// ⇧⌘Z: do again what was taken back last (a command for several photos, or a step of the open photo).
    pub(crate) fn redo_latest(&mut self) {
        let mut v = Vec::new();
        if let Some((t, turn)) = self.turn_undo.redo_stamp() {
            if !turn || self.in_grid() {
                v.push((Stack::Commands, t));
            }
        }
        if let Some(i) = self.current.as_ref().map(|c| c.index) {
            let key = self.history_key(i);
            let own = self.history.command_at_cursor(&key).1.is_some_and(|c| Some(c) == self.turn_undo.redo_command());
            if let Some(t) = self.history.redo_stamp(&key).filter(|_| !own) {
                v.push((Stack::Photo, t));
            }
        }
        match pick(&v) {
            Some(Stack::Commands) => {
                self.turn_undo.recent = true;
                self.redo_turn();
            }
            Some(_) => self.undo(true),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_increase() {
        let a = tick();
        let b = tick();
        assert!(b > a);
    }

    #[test]
    fn the_most_recent_action_wins() {
        assert_eq!(pick(&[]), None);
        assert_eq!(pick(&[(Stack::Commands, 5), (Stack::Library, 9), (Stack::Photo, 7)]), Some(Stack::Library));
        assert_eq!(pick(&[(Stack::Rating, 3), (Stack::Photo, 3)]), Some(Stack::Rating), "equal times: the order of the list");
    }

    /// The chain of mixed actions as the stacks record them: turn, then reordering collections, then a rating, then a
    /// slider step: ⌘Z takes them back newest first, whatever their kind.
    #[test]
    fn mixed_actions_newest_first() {
        let mut stamps: Vec<(Stack, u64)> = Vec::new();
        for s in [Stack::Commands, Stack::Library, Stack::Rating, Stack::Photo] {
            stamps.push((s, tick()));
        }
        let mut order = Vec::new();
        while let Some(s) = pick(&stamps) {
            order.push(s);
            stamps.retain(|(x, _)| *x != s);
        }
        assert_eq!(order, [Stack::Photo, Stack::Rating, Stack::Library, Stack::Commands]);
    }

    /// ⌘Z outside the grid, by time over all stacks: a slider step, the quick collection (B), a move of this session,
    /// a deleted own preset are taken back newest first in Develop; a management step older than a move waits for it.
    /// A finished move journal of an earlier session is never touched. Needs `GRAINEO_DATA_DIR`.
    #[test]
    fn session_steps_in_develop_and_old_journals_untouched() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-undo-session-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(64, 48, image::Rgb([90, 100, 120])).save(folder.join("b.png")).unwrap();
        // a finished move of an earlier session
        let journals = library::relocate::journal_folder();
        std::fs::create_dir_all(&journals).unwrap();
        let old = journals.join("1000.json");
        std::fs::write(&old, r#"{"id":"1000","title":"alt","status":"done","message":"","kind":"","steps":[],"db":[]}"#).unwrap();
        assert!(library::relocate::journals().iter().any(|v| v.id == "1000" && v.status == "done"));
        let ctx = eframe::egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        // the quick collection holds library photos only (a photo of a folder only being viewed has no entry, B is
        // refused there): the folder goes into the test library
        {
            let db = library::db::Db::open(&library::db_path()).unwrap();
            library::reconcile::add_root(&db, &folder, &|_| {}).unwrap();
        }
        let mut a = crate::App::new(&cc);
        assert!(a.undo_candidates().iter().all(|c| c.0 != Stack::Library), "journals of earlier sessions do not count");
        a.open_folder_inner(folder.clone());
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.is_none() || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline, "photo does not open");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!a.in_grid());
        let cur = a.current.as_ref().unwrap().index;
        let image = a.entries[cur].image;
        a.track_history(&ctx);
        a.entries[cur].settings.exposure = 0.3;
        a.track_history(&ctx);
        assert!(a.lib.quick_toggle(&[image]).is_some());
        assert!(a.lib.in_quick(image));
        a.lib.moves.push((tick(), "999".into()));
        a.presets.push(crate::presets::Preset { name: "Test".into(), settings: develop::Settings::default(), scope: crate::presets::Scope::Look, hint: String::new() });
        let n = a.presets.len();
        a.delete_own_preset(n - 1);
        assert_eq!(a.presets.len(), n - 1);
        let mut undo = |a: &mut crate::App| {
            a.undo_latest();
            a.track_history(&ctx);
        };
        undo(&mut a);
        assert_eq!(a.presets.len(), n, "the deleted preset is back");
        undo(&mut a);
        assert!(a.lib.moves.is_empty() && a.lib.in_quick(image), "the move is newer than the quick collection");
        undo(&mut a);
        assert!(!a.lib.in_quick(image), "B taken back in Develop");
        assert_eq!(a.entries[cur].settings.exposure, 0.3, "the slider step is older");
        undo(&mut a);
        assert_eq!(a.entries[cur].settings.exposure, 0.0);
        undo(&mut a);
        let _ = a.lib.undo_external();
        assert!(a.undo_candidates().is_empty());
        assert!(library::relocate::journals().iter().any(|v| v.id == "1000" && v.status == "done"), "the old journal stays done");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// The whole app: a slider step, then a command for several photos (reset), then another slider step. ⌘Z takes
    /// them back newest first – the command through its own stack, which also moves the photo's cursor – and ⇧⌘Z
    /// does them again in the order they were taken back. Needs `GRAINEO_DATA_DIR`.
    #[test]
    fn chain_of_slider_command_slider() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-undo-order-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(64, 48, image::Rgb([110, 80, 60])).save(folder.join("a.png")).unwrap();
        let ctx = eframe::egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = crate::App::new(&cc);
        a.open_folder_inner(folder.clone());
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.is_none() || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline, "photo does not open");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let cur = a.current.as_ref().unwrap().index;
        let ev = |a: &crate::App| a.entries[cur].settings.exposure;
        a.track_history(&ctx);
        a.entries[cur].settings.exposure = 0.3;
        a.track_history(&ctx);
        a.reset_command();
        a.track_history(&ctx);
        a.entries[cur].settings.exposure = 0.7;
        a.track_history(&ctx);
        let mut step = |a: &mut crate::App, redo: bool| {
            if redo {
                a.redo_latest();
            } else {
                a.undo_latest();
            }
            a.track_history(&ctx);
            ev(a)
        };
        assert_eq!(step(&mut a, false), 0.0, "the last slider step");
        assert_eq!(step(&mut a, false), 0.3, "the reset (command stack)");
        assert_eq!(step(&mut a, false), 0.0, "the first slider step");
        assert_eq!(step(&mut a, true), 0.3);
        assert_eq!(step(&mut a, true), 0.0, "the reset again");
        assert_eq!(step(&mut a, true), 0.7);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
