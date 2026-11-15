//! Turning photos by 90° from the library (grid and filmstrip): the buttons and ⌘R / ⇧⌘R act on all selected photos.
//!
//! The turn is the same as in Develop: `Settings::rotate` (quarter turns clockwise, on top of the camera
//! orientation, which stays the basis) and the crop turning with it. It is stored through the usual library path
//! (`sidecar::save_entry`, XMP only when "write sidecars" is on), originals are never touched.
//!
//! Display: the tile turns at once (the shown preview is drawn turned until the re-rendered preview arrives, which
//! already carries the turn – so nothing is developed again here, the normal thumbnail job does it); the filmstrip
//! cell changes its shape at once. ⌘Z / ⇧⌘Z undo and redo a turn of several photos together.

use eframe::egui::{self, Color32, Pos2, Rect, TextureId, Vec2};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::App;
use develop::Settings;

/// Settings after one more 90° turn: rotation counter and crop (centre and size swap axes).
pub fn turn_settings(s: &mut Settings, clockwise: bool) {
    s.rotate = (s.rotate + if clockwise { 1 } else { 3 }) % 4;
    let [cx, cy, w, h] = s.crop;
    s.crop = if clockwise { [1.0 - cy, cx, h, w] } else { [cy, 1.0 - cx, h, w] };
}

/// Rotation state of one photo, as far as undo needs it.
pub type State = (u8, [f32; 4]);

/// One photo of a turn: state before and after (virtual copies by name).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub image: i64,
    pub copy: Option<String>,
    pub before: State,
    pub after: State,
}

/// The steps of one turn command for the given photos (image, virtual copy, current settings): state before and
/// after, per photo, in the same order.
pub fn plan_steps(items: &[(i64, Option<String>, Settings)], clockwise: bool) -> Vec<Step> {
    items
        .iter()
        .map(|(image, copy, s)| {
            let mut t = s.clone();
            turn_settings(&mut t, clockwise);
            Step { image: *image, copy: copy.clone(), before: (s.rotate, s.crop), after: (t.rotate, t.crop) }
        })
        .collect()
}

/// One undoable command for several photos: a turn, or new develop settings (transfer, paste, reset – see
/// `settings_group.rs`).
#[derive(Clone, Debug, PartialEq)]
pub enum Undoable {
    Turn(Vec<Step>),
    Settings(Vec<crate::settings_group::Change>),
}

/// Photos kept in the undo stack of commands for several photos (memory: two settings per photo).
pub const PHOTOS_MAX: usize = 20_000;

impl Undoable {
    /// Number of photos of the command.
    pub fn photos(&self) -> usize {
        match self {
            Undoable::Turn(s) => s.len(),
            Undoable::Settings(c) => c.len(),
        }
    }
}

/// Undo and redo stack of turns and settings commands (one entry = one command for all photos it touched).
#[derive(Default)]
pub struct TurnUndo {
    undo: Vec<Undoable>,
    redo: Vec<Undoable>,
    /// the last undoable action was one of these (or its undo/redo); anything else resets it
    pub recent: bool,
}

impl TurnUndo {
    pub fn record(&mut self, steps: Vec<Step>) {
        if steps.is_empty() {
            return;
        }
        self.push(Undoable::Turn(steps));
    }

    /// New settings for several photos as one command (one ⌘Z for all of them).
    pub fn record_settings(&mut self, changes: Vec<crate::settings_group::Change>) {
        if changes.is_empty() {
            return;
        }
        self.push(Undoable::Settings(changes));
    }

    fn push(&mut self, u: Undoable) {
        self.undo.push(u);
        // at most 100 commands and – commands for thousands of photos keep two settings per photo – at most
        // [`PHOTOS_MAX`] photos in all (the newest command always stays)
        while self.undo.len() > 100 || (self.undo.len() > 1 && self.undo.iter().map(Undoable::photos).sum::<usize>() > PHOTOS_MAX) {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.recent = true;
    }

    /// Is the command ⌘Z would take back a turn? (turns are taken back only in the grid)
    pub fn next_undo_is_turn(&self) -> bool {
        self.recent && matches!(self.undo.last(), Some(Undoable::Turn(_)))
    }

    /// Is the command ⇧⌘Z would do again a turn?
    pub fn next_redo_is_turn(&self) -> bool {
        self.recent && matches!(self.redo.last(), Some(Undoable::Turn(_)))
    }

    /// ⌘Z: the command to take back (`None`: the last action was none of these).
    pub fn undo(&mut self) -> Option<Undoable> {
        if !self.recent {
            return None;
        }
        let s = self.undo.pop()?;
        self.redo.push(s.clone());
        self.recent = !self.undo.is_empty() || !self.redo.is_empty();
        Some(s)
    }

    /// ⇧⌘Z: the command to do again.
    pub fn redo(&mut self) -> Option<Undoable> {
        if !self.recent {
            return None;
        }
        let s = self.redo.pop()?;
        self.undo.push(s.clone());
        Some(s)
    }

    /// Another action came after the last command: ⌘Z belongs to that one.
    pub fn other_action(&mut self) {
        self.recent = false;
    }
}

/// Quarter turns still to be shown on tiles whose re-rendered preview has not arrived yet.
#[derive(Default)]
pub struct Turned {
    map: HashMap<i64, (u8, Instant)>,
}

/// A preview that does not arrive within this time (job lost) no longer counts as pending.
const PENDING_MAX: Duration = Duration::from_secs(90);

impl Turned {
    /// `quarters` more clockwise turns are visible on this photo's tile.
    pub fn add(&mut self, image: i64, quarters: u8) {
        let q = (self.map.get(&image).map_or(0, |v| v.0) + quarters) % 4;
        if q == 0 {
            self.map.remove(&image);
        } else {
            self.map.insert(image, (q, Instant::now()));
        }
    }

    /// The re-rendered preview arrived: it carries the turn itself.
    pub fn clear(&mut self, image: i64) {
        self.map.remove(&image);
    }

    pub fn quarters(&self, image: i64) -> u8 {
        match self.map.get(&image) {
            Some(&(q, t)) if t.elapsed() < PENDING_MAX => q,
            _ => 0,
        }
    }
}

/// Size of a texture as shown after `quarters` turns.
pub fn turned_size(size: Vec2, quarters: u8) -> Vec2 {
    if quarters % 2 == 1 {
        Vec2::new(size.y, size.x)
    } else {
        size
    }
}

/// Texture coordinates of the destination corners (top left, top right, bottom right, bottom left) for a picture
/// turned `quarters` times clockwise.
pub fn turned_uv(quarters: u8) -> [Pos2; 4] {
    let base = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
    let q = (quarters % 4) as usize;
    [base[(4 - q) % 4], base[(5 - q) % 4], base[(6 - q) % 4], base[(7 - q) % 4]]
}

/// Draw a texture into `rect` (already the turned shape), turned `quarters` times clockwise.
pub fn paint_image(painter: &egui::Painter, id: TextureId, rect: Rect, quarters: u8, tint: Color32) {
    if quarters % 4 == 0 {
        painter.image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
        return;
    }
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let uv = turned_uv(quarters);
    let mut mesh = egui::Mesh::with_texture(id);
    for k in 0..4 {
        mesh.vertices.push(egui::epaint::Vertex { pos: corners[k], uv: uv[k], color: tint });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

// ───────────────────────────────────────────── Saving (background, in order)

type Job = (std::path::PathBuf, Option<String>, Settings, crate::sidecar::Meta);

/// Turns handed to the writer and not yet saved (the app waits for them when closing).
static WRITING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Is a turn still being saved?
pub fn writing() -> bool {
    WRITING.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// One helper thread writes the turns in the order they were made, so the UI never waits for the database.
fn writer() -> &'static std::sync::mpsc::Sender<Job> {
    static W: std::sync::OnceLock<std::sync::mpsc::Sender<Job>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("turn-writer".into())
            .spawn(move || {
                for (path, copy, s, meta) in rx {
                    if let Err(e) = crate::sidecar::save_entry(&path, copy.as_deref(), &s, &meta) {
                        crate::log(&format!("turn not saved: {e}"));
                    }
                    WRITING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .ok();
        tx
    })
}

/// Save one photo's settings through the same ordered background writer (the UI never waits for the database).
pub(crate) fn save_background(path: std::path::PathBuf, copy: Option<String>, s: Settings, meta: crate::sidecar::Meta) {
    WRITING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if writer().send((path, copy, s, meta)).is_err() {
        WRITING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl App {
    /// ⌘R / ⇧⌘R and the buttons of the library: turn all selected photos (in Develop: the open photo).
    pub(crate) fn rotate_command(&mut self, clockwise: bool) {
        if self.in_grid() {
            self.rotate_selection(clockwise);
        } else {
            self.rotate_image(clockwise);
        }
    }

    /// Turn the selected photos of the library by 90°. Photos whose original is not reachable or that cannot be
    /// developed (video, damaged) are left out and named in the status line.
    pub(crate) fn rotate_selection(&mut self, clockwise: bool) {
        if self.lib.nothing_selected() {
            self.status = crate::tr("Kein Foto ausgewählt").to_string();
            return;
        }
        let images: std::collections::HashSet<i64> = self.lib.target_images().into_iter().collect();
        let (mut targets, mut skipped) = (Vec::new(), 0usize);
        for (i, e) in self.entries.iter().enumerate().filter(|(_, e)| images.contains(&e.image)) {
            if crate::image_kind::of(&e.path).editable() && e.state == 0 {
                targets.push(i);
            } else {
                skipped += 1;
            }
        }
        for &i in &targets {
            self.ensure_entry(i);
        }
        let items: Vec<(i64, Option<String>, Settings)> = targets.iter().map(|&i| (self.entries[i].image, self.entries[i].copy.clone(), self.entries[i].settings.clone())).collect();
        let steps = plan_steps(&items, clockwise);
        for (&i, st) in targets.iter().zip(&steps) {
            self.set_turn(i, st.after);
        }
        let n = steps.len();
        self.status = match (n, skipped) {
            (0, _) => crate::tr("Nicht gedreht: Original nicht erreichbar oder kein entwickelbares Foto").to_string(),
            (1, 0) => if clockwise { crate::tr("Nach rechts gedreht") } else { crate::tr("Nach links gedreht") }.to_string(),
            (_, 0) => crate::trf!("{n} Fotos gedreht", n = n),
            (_, k) => crate::trf!("{n} Fotos gedreht, {k} übersprungen", n = n, k = k),
        };
        self.after_other_edit();
        self.turn_undo.record(steps);
        self.compute_thumbnails();
    }

    /// Put photo `i` into the given rotation state: settings, save in the background, tile and strip cell follow.
    fn set_turn(&mut self, i: usize, to: State) {
        let from = (self.entries[i].settings.rotate, self.entries[i].settings.crop);
        let quarters = (to.0 + 4 - from.0) % 4;
        {
            let e = &mut self.entries[i];
            e.settings.rotate = to.0;
            e.settings.crop = to.1;
            e.stored = true;
        }
        let e = &self.entries[i];
        save_background(e.path.clone(), e.copy.clone(), e.settings.clone(), e.meta.clone());
        self.saved.insert(e.image, (crate::sidecar_settings(&e.settings), e.meta.clone()));
        // an open photo (Develop) must not count this as an edit of its own for the undo history
        if let Some((ci, c)) = self.committed.as_mut() {
            if *ci == i {
                *c = develop::Settings { view: develop::View::Developed, ..self.entries[i].settings.clone() };
            }
        }
        // only photos with their own preview job change on the tile (virtual copies share the file preview)
        let (image, shows) = (self.entries[i].image, self.entries[i].copy.is_none() && self.entries[i].file > 0);
        if shows && quarters != 0 {
            self.lib.turned.add(image, quarters);
            if quarters % 2 == 1 {
                let e = &mut self.entries[i];
                if e.aspect > 0.0 {
                    e.aspect = 1.0 / e.aspect;
                }
                self.strip_layout = None;
            }
        }
        self.queue_thumbnail(i);
    }

    /// Other undoable actions come after a turn: their ⌘Z first.
    pub(crate) fn after_other_edit(&mut self) {
        self.lib.rating_not_recent();
    }

    /// ⌘Z: take back the last turn or settings command for all its photos. `false`: the last action was neither
    /// (turns only in the grid, settings commands in every view).
    pub(crate) fn undo_turn(&mut self) -> bool {
        // a change for many photos is still being applied: stop it, then take back what was saved
        if self.multi_busy() {
            self.multi_cancel_then_undo();
            return true;
        }
        if self.turn_undo.next_undo_is_turn() && !self.in_grid() {
            return false;
        }
        match self.turn_undo.undo() {
            Some(Undoable::Turn(steps)) => {
                let n = self.apply_steps(&steps, false);
                self.status = crate::trf!("Rückgängig: Drehung ({n} Fotos)", n = n);
            }
            Some(Undoable::Settings(changes)) => {
                let n = self.apply_changes(&changes, false);
                self.status = crate::trf!("Rückgängig: Einstellungen ({n} Fotos)", n = n);
            }
            None => return false,
        }
        true
    }

    /// ⇧⌘Z: do the taken-back command again.
    pub(crate) fn redo_turn(&mut self) -> bool {
        if self.multi_busy() {
            self.status = crate::tr("Wird noch gespeichert …").to_string();
            return true;
        }
        if self.turn_undo.next_redo_is_turn() && !self.in_grid() {
            return false;
        }
        match self.turn_undo.redo() {
            Some(Undoable::Turn(steps)) => {
                let n = self.apply_steps(&steps, true);
                self.status = crate::trf!("Wiederholt: Drehung ({n} Fotos)", n = n);
            }
            Some(Undoable::Settings(changes)) => {
                let n = self.apply_changes(&changes, true);
                self.status = crate::trf!("Wiederholt: Einstellungen ({n} Fotos)", n = n);
            }
            None => return false,
        }
        true
    }

    fn apply_steps(&mut self, steps: &[Step], forward: bool) -> usize {
        let mut n = 0;
        for s in steps {
            let Some(i) = self.entries.iter().position(|e| e.image == s.image && e.copy == s.copy) else { continue };
            self.ensure_entry(i);
            self.set_turn(i, if forward { s.after } else { s.before });
            n += 1;
        }
        self.lib.rating_not_recent();
        self.compute_thumbnails();
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_turns_are_the_identity_and_left_undoes_right() {
        let mut s = Settings { crop: [0.4, 0.3, 0.5, 0.2], ..Settings::default() };
        let start = s.clone();
        for _ in 0..4 {
            turn_settings(&mut s, true);
        }
        let near = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
        assert_eq!(s.rotate, start.rotate);
        assert!(near(s.crop, start.crop));
        turn_settings(&mut s, true);
        turn_settings(&mut s, false);
        assert_eq!(s.rotate, start.rotate);
        assert!(near(s.crop, start.crop));
        turn_settings(&mut s, false);
        assert_eq!(s.rotate, 3);
        assert!(near(s.crop, [0.3, 0.6, 0.2, 0.5]));
    }

    #[test]
    fn plan_gives_each_photo_its_own_step_and_undo_restores_them() {
        let a = Settings { rotate: 1, crop: [0.5, 0.5, 1.0, 1.0], ..Settings::default() };
        let b = Settings { rotate: 3, crop: [0.4, 0.6, 0.5, 0.2], ..Settings::default() };
        let items = vec![(10, None, a.clone()), (11, Some("Kopie 1".to_string()), b.clone())];
        let steps = plan_steps(&items, true);
        assert_eq!(steps.len(), 2);
        assert_eq!((steps[0].image, steps[0].after.0), (10, 2));
        assert_eq!((steps[1].image, steps[1].copy.as_deref(), steps[1].after.0), (11, Some("Kopie 1"), 0));
        assert_eq!(steps[1].before, (3, b.crop));
        // one command, one undo step for both photos
        let mut u = TurnUndo::default();
        u.record(steps.clone());
        assert_eq!(u.undo().unwrap(), Undoable::Turn(steps.clone()));
        // counter-clockwise from the new state returns to the start
        let after: Vec<_> = steps.iter().zip(&items).map(|(st, (i, c, s))| (*i, c.clone(), Settings { rotate: st.after.0, crop: st.after.1, ..s.clone() })).collect();
        let back = plan_steps(&after, false);
        assert_eq!(back[0].after.0, 1);
        assert_eq!(back[1].after.0, 3);
    }

    #[test]
    fn turn_changes_only_rotation_and_crop() {
        let mut s = Settings { exposure: 0.7, ..Settings::default() };
        turn_settings(&mut s, true);
        assert_eq!(s.exposure, 0.7);
        assert_eq!(s.rotate, 1);
    }

    fn step(image: i64, before: u8, after: u8) -> Step {
        Step { image, copy: None, before: (before, [0.5, 0.5, 1.0, 1.0]), after: (after, [0.5, 0.5, 1.0, 1.0]) }
    }

    #[test]
    fn undo_takes_back_all_photos_of_one_command_together() {
        let mut u = TurnUndo::default();
        assert!(u.undo().is_none());
        u.record(vec![step(1, 0, 1), step(2, 3, 0), step(3, 1, 2)]);
        u.record(vec![step(1, 1, 2)]);
        let last = u.undo().unwrap();
        assert_eq!(last, Undoable::Turn(vec![step(1, 1, 2)]));
        let Undoable::Turn(before) = u.undo().unwrap() else { panic!("turn expected") };
        assert_eq!(before.len(), 3);
        assert!(u.undo().is_none());
        // redo returns the commands in the order they were taken back
        assert!(matches!(u.redo().unwrap(), Undoable::Turn(v) if v.len() == 3));
        assert_eq!(u.redo().unwrap(), Undoable::Turn(vec![step(1, 1, 2)]));
        assert!(u.redo().is_none());
    }

    #[test]
    fn another_action_hands_cmd_z_to_that_action_and_a_new_turn_clears_redo() {
        let mut u = TurnUndo::default();
        u.record(vec![step(1, 0, 1)]);
        u.other_action();
        assert!(u.undo().is_none());
        u.record(vec![step(1, 1, 2)]);
        assert!(u.undo().is_some());
        u.record(vec![step(1, 1, 0)]);
        assert!(u.redo().is_none());
    }

    #[test]
    fn undo_stack_keeps_at_most_photos_max_photos() {
        let mut u = TurnUndo::default();
        let big = |n: usize| (0..n as i64).map(|i| step(i, 0, 1)).collect::<Vec<_>>();
        u.record(big(8_000));
        u.record(big(8_000));
        u.record(big(8_000));
        assert_eq!(u.undo.iter().map(Undoable::photos).sum::<usize>(), 16_000, "the oldest command went");
        u.record(big(30_000));
        assert_eq!(u.undo.len(), 1, "the newest command always stays");
    }

    #[test]
    fn pending_quarters_add_up_and_clear() {
        let mut t = Turned::default();
        t.add(7, 1);
        t.add(7, 1);
        assert_eq!(t.quarters(7), 2);
        t.add(7, 2);
        assert_eq!(t.quarters(7), 0);
        t.add(8, 3);
        assert_eq!(t.quarters(8), 3);
        t.clear(8);
        assert_eq!(t.quarters(8), 0);
    }

    #[test]
    fn display_turn_swaps_the_shape_and_the_corners() {
        assert_eq!(turned_size(Vec2::new(300.0, 200.0), 0), Vec2::new(300.0, 200.0));
        assert_eq!(turned_size(Vec2::new(300.0, 200.0), 1), Vec2::new(200.0, 300.0));
        assert_eq!(turned_size(Vec2::new(300.0, 200.0), 2), Vec2::new(300.0, 200.0));
        let uv = turned_uv(0);
        assert_eq!(uv[0], Pos2::new(0.0, 0.0));
        // clockwise: the top left of the picture ends up at the top right
        let uv = turned_uv(1);
        assert_eq!(uv[1], Pos2::new(0.0, 0.0));
        assert_eq!(uv[0], Pos2::new(0.0, 1.0));
        // counter-clockwise (three turns): the top left ends up at the bottom left
        let uv = turned_uv(3);
        assert_eq!(uv[3], Pos2::new(0.0, 0.0));
        let uv = turned_uv(2);
        assert_eq!(uv[2], Pos2::new(0.0, 0.0));
    }
}
