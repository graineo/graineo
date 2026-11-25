//! Rating without waiting: stars, flag and label apply immediately to the **active** image
//! (the one shown in the grid or film strip) or the selection – never to the image whose full
//! resolution finished last. The UI changes in the same frame; database and
//! sidecars are written by a dedicated thread in the order of the key presses.
//!
//! Keys are processed in the order of their events (←, X, ←, X … even within
//! one frame), see [`App::shortcuts_ordered`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use eframe::egui;

use crate::{sidecar, App};

/// Image ID, stars, flag, label.
pub type Value = (i64, u8, i8, u8);

/// A write job (in key order).
pub enum Task {
    /// Library images: database, then the sidecars of these files
    Library { values: Vec<Value>, sidecar: Vec<i64> },
    /// "Browse folder" (no database): sidecar directly (original or virtual copy)
    File { image: i64, path: PathBuf, format: String, copy: Option<String>, value: (u8, i8, u8) },
}

/// Background writer with its own database connection.
pub struct Writer {
    tx: Option<mpsc::Sender<Task>>,
    error: Arc<Mutex<Vec<String>>>,
    /// Image → still pending jobs (while any exist, the rating in memory applies, not the file)
    pending: Arc<Mutex<HashMap<i64, usize>>>,
    db_path: Option<PathBuf>,
}

impl Writer {
    pub fn new(db_path: Option<PathBuf>) -> Self {
        Writer { tx: None, error: Arc::default(), pending: Arc::default(), db_path }
    }

    pub fn send(&mut self, a: Task) {
        let images: Vec<i64> = match &a {
            Task::Library { values, .. } => values.iter().map(|w| w.0).collect(),
            Task::File { image, .. } => vec![*image],
        };
        {
            let mut o = self.pending.lock().unwrap();
            for b in &images {
                *o.entry(*b).or_default() += 1;
            }
        }
        if self.tx.is_none() {
            let (tx, rx) = mpsc::channel::<Task>();
            let (error, pending, path) = (self.error.clone(), self.pending.clone(), self.db_path.clone());
            let _ = std::thread::Builder::new().name("bewertung".into()).spawn(move || {
                let mut db: Option<library::db::Db> = None;
                for a in rx {
                    let images: Vec<i64> = match &a {
                        Task::Library { values, .. } => values.iter().map(|w| w.0).collect(),
                        Task::File { image, .. } => vec![*image],
                    };
                    // a panic in one job must not end the thread (all later ratings would be lost)
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(a, &mut db, path.as_deref()))).unwrap_or_else(|_| {
                        db = None;
                        Err(crate::tr("Bewertung nicht gespeichert (interner Fehler)").to_string())
                    });
                    if let Err(e) = r {
                        error.lock().unwrap_or_else(|x| x.into_inner()).push(e);
                    }
                    let mut o = pending.lock().unwrap_or_else(|x| x.into_inner());
                    for b in images {
                        if let Some(n) = o.get_mut(&b) {
                            *n -= 1;
                            if *n == 0 {
                                o.remove(&b);
                            }
                        }
                    }
                }
            });
            self.tx = Some(tx);
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(a);
        }
    }

    /// Is this image still being written?
    pub fn writing(&self, image: i64) -> bool {
        self.pending.lock().unwrap().contains_key(&image)
    }

    pub fn all_written(&self) -> bool {
        // the job currently running also still counts as pending
        self.pending.lock().unwrap().is_empty()
    }

    /// Error messages since the last pickup.
    pub fn error(&self) -> Vec<String> {
        std::mem::take(&mut *self.error.lock().unwrap())
    }
}

fn execute(a: Task, db: &mut Option<library::db::Db>, path: Option<&std::path::Path>) -> Result<(), String> {
    match a {
        Task::Library { values, sidecar } => {
            if db.is_none() {
                let p = path.ok_or(crate::tr("keine Bibliothek"))?;
                *db = Some(library::db::Db::open(p).map_err(|e| e.to_string())?);
            }
            let d = db.as_ref().unwrap();
            // files on an unreachable location: remember their sidecar state before the change (queue, merging later)
            library::location::note_before_change(d, &sidecar);
            d.set_ratings(&values).map_err(|e| crate::trf!("Bewertung nicht gespeichert: {e}", e = e))?;
            for file in sidecar {
                library::sync::write_sidecar(d, file).map_err(|e| crate::trf!("Begleitdatei nicht geschrieben: {e}", e = e))?;
            }
            Ok(())
        }
        Task::File { path, format, copy, value, .. } => {
            let raw = library::sync::raw_beside(&path, &format);
            let mut w = match library::sidecar::read(&path, &format, raw).0 {
                library::xmp::ReadResult::Ok(w) => w,
                _ => library::xmp::Values::default(),
            };
            match copy {
                None => (w.stars, w.flag, w.label) = value,
                Some(k) => {
                    let mut copies = library::sidecar::read_copies(w.copies_json.as_deref());
                    let Some(x) = copies.iter_mut().find(|x| x.name == k) else { return Err(crate::trf!("Kopie „{k}“ fehlt in der Begleitdatei", k = k)) };
                    (x.stars, x.flag, x.label) = value;
                    w.copies_json = library::sidecar::copies_text(&copies);
                }
            }
            w.rating_time = Some(library::now());
            library::sidecar::write(&path, &format, raw, &w).map(|_| ()).map_err(|e| crate::trf!("Begleitdatei nicht geschrieben: {e}", e = e))
        }
    }
}

/// Key → rating change (without ⌘).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    Stars(u8),
    Flag(i8),
    Label(u8),
}

fn change(k: egui::Key) -> Option<Change> {
    use egui::Key::*;
    Some(match k {
        Num0 => Change::Stars(0),
        Num1 => Change::Stars(1),
        Num2 => Change::Stars(2),
        Num3 => Change::Stars(3),
        Num4 => Change::Stars(4),
        Num5 => Change::Stars(5),
        Num6 => Change::Label(1),
        Num7 => Change::Label(2),
        Num8 => Change::Label(3),
        Num9 => Change::Label(4),
        P => Change::Flag(1),
        X => Change::Flag(-1),
        U => Change::Flag(0),
        _ => return None,
    })
}

impl App {
    /// Arrows and rating keys in event order (before all other
    /// shortcuts, which then no longer see these keys). Applies in the grid and single image.
    pub(crate) fn shortcuts_ordered(&mut self, ctx: &egui::Context) {
        if self.lib.import_open() || ctx.egui_wants_keyboard_input() || self.show_export || self.show_shortcuts {
            return;
        }
        // people view: only stars, flags, labels and B (on the chosen face's photo); arrows, ↩ … belong to the view
        let people = self.in_people();
        // comparison: arrows, ⇧X and ↩ belong to the view; rating keys apply to the focused photo
        let review = self.lib.compare_open();
        let raster = self.in_grid();
        let loupe = self.lib.loupe;
        // Crop: X rotates the aspect, digits stay free → only the old paths there
        let rating_allowed = self.crop_mode.is_none();
        let mut queue: Vec<(egui::Key, egui::Modifiers)> = Vec::new();
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                let egui::Event::Key { physical_key, key, pressed: true, modifiers, .. } = e else { return true };
                let k = physical_key.unwrap_or(*key);
                if modifiers.command || modifiers.ctrl {
                    return true;
                }
                // (the tone equalizer graph with keyboard focus keeps the arrow keys)
                let arrow = !crate::tone_eq_ui::keyboard_focus() && !people && !review && (matches!(k, egui::Key::ArrowLeft | egui::Key::ArrowRight) || (raster && !loupe && matches!(k, egui::Key::ArrowUp | egui::Key::ArrowDown)));
                let value = rating_allowed && !modifiers.alt && change(k).is_some() && !(review && modifiers.shift);
                // B: quick collection
                let quick = k == egui::Key::B && !modifiers.alt && !modifiers.shift;
                if arrow || value || quick {
                    queue.push((k, *modifiers));
                    return false;
                }
                true
            });
        });
        for (k, m) in queue {
            match k {
                egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown => {
                    let prefill = matches!(k, egui::Key::ArrowRight | egui::Key::ArrowDown);
                    let horizontal = matches!(k, egui::Key::ArrowLeft | egui::Key::ArrowRight);
                    // ⌥←/→: through the photos of the series too, a collapsed one opens for it (library_series.rs);
                    // in Develop with the zoom kept
                    if m.alt && !m.shift && horizontal && self.lib.series_step(prefill) {
                        if !raster {
                            self.strip_follow_lib = Some(true);
                        }
                        continue;
                    }
                    if raster {
                        let step = if matches!(k, egui::Key::ArrowUp | egui::Key::ArrowDown) { self.lib.column_count() as isize } else { 1 };
                        let d = if prefill { step } else { -step };
                        if m.shift && !m.alt {
                            self.lib.extend_external(d);
                        } else {
                            self.lib.step_external(d);
                        }
                    } else if m.shift && !m.alt && matches!(k, egui::Key::ArrowLeft | egui::Key::ArrowRight) {
                        self.extend_strip(prefill);
                    } else {
                        self.step(prefill, m.alt);
                    }
                }
                egui::Key::B => self.quick_key(),
                _ => {
                    if let Some(a) = change(k) {
                        self.rate(a, m.shift && !people && !review);
                    }
                }
            }
        }
    }

    /// Target images of a rating: the selection if the active image belongs to it, otherwise the active one.
    fn rating_target(&self) -> Vec<i64> {
        // people view: the chosen face's photo, nothing else (the grid's own choice stays as it is)
        if self.in_people() {
            return self.lib.people_focus().into_iter().collect();
        }
        // comparison (library and Develop): the focused field
        if let Some(f) = self.lib.compare_focus() {
            return vec![f];
        }
        let active = self.lib.active_image().or_else(|| self.active_entry().and_then(|i| self.entries.get(i)).map(|e| e.image));
        let Some(active) = active else { return Vec::new() };
        let mut selection: Vec<i64> = self.lib.selection();
        selection.extend(self.entries.iter().filter(|e| e.selected).map(|e| e.image));
        selection.sort();
        selection.dedup();
        if selection.len() > 1 && selection.contains(&active) {
            selection
        } else {
            vec![active]
        }
    }

    /// B: add target images to the quick collection or – if all are already in it – remove them.
    fn quick_key(&mut self) {
        let target = self.rating_target();
        if let Some(t) = self.lib.quick_toggle(&target) {
            self.status = t;
        }
    }

    /// Set the rating immediately (UI), write in the background. `advance`: ⇧ = advance afterwards.
    pub(crate) fn rate(&mut self, a: Change, advance: bool) {
        let target = self.rating_target();
        let (s, m, e) = match a {
            Change::Stars(n) => (Some(n), None, None),
            Change::Flag(n) => (None, Some(n), None),
            Change::Label(n) => (None, None, Some(n)),
        };
        let new = self.lib.rate_images(&target, s, m, e);
        self.turn_undo.other_action();
        crate::log(&format!("rate {a:?}: images {target:?}"));
        self.adopt_values(&new);
        if self.lib.take_rating_refused() {
            self.status = crate::tr(crate::library_ui::RATING_REFUSED).to_string();
            return;
        }
        if let Some(t) = feedback(a, &new) {
            self.status = t;
        }
        if advance {
            if self.in_grid() {
                self.lib.step_external(1);
            } else {
                self.step(true, false);
            }
        }
    }

    /// Apply new values to the film strip (entries by image ID).
    pub(crate) fn adopt_values(&mut self, values: &[Value]) {
        let w: HashMap<i64, (u8, i8, u8)> = values.iter().map(|&(b, s, m, e)| (b, (s, m, e))).collect();
        for e in self.entries.iter_mut() {
            if let Some(&(s, m, l)) = w.get(&e.image) {
                e.meta = sidecar::Meta { rating: s, flag: sidecar::Flag::from_i8(m), label: l };
            }
        }
    }

    /// ⌘Z after a rating: undo the last rating. `false` if the last action
    /// was not a rating (then the existing undo applies).
    pub(crate) fn undo_rating(&mut self) -> bool {
        let Some(alt) = self.lib.undo_rating_change() else { return false };
        self.adopt_values(&alt);
        self.status = crate::trf!("Rückgängig: Bewertung ({a} {b})", a = alt.len(), b = if alt.len() == 1 { crate::tr("Foto") } else { crate::tr("Fotos") });
        true
    }
}

/// Visible feedback in the status bar (e.g. "Rejected").
fn feedback(a: Change, new: &[Value]) -> Option<String> {
    let n = new.len();
    if n == 0 {
        return None;
    }
    let what = match a {
        Change::Flag(1) => crate::tr("Markiert").to_string(),
        Change::Flag(-1) => crate::tr("Abgelehnt").to_string(),
        Change::Flag(_) => crate::tr("Markierung entfernt").to_string(),
        Change::Stars(0) => crate::tr("Sterne entfernt").to_string(),
        Change::Stars(s) => format!("{s} {}", if s == 1 { crate::tr("Stern") } else { crate::tr("Sterne") }),
        Change::Label(_) if new[0].3 == 0 => crate::tr("Etikett entfernt").to_string(),
        Change::Label(e) => ["", crate::tr("Etikett Rot"), crate::tr("Etikett Gelb"), crate::tr("Etikett Grün"), crate::tr("Etikett Blau"), crate::tr("Etikett Lila")][e as usize % 6].to_string(),
    };
    Some(if n == 1 { what } else { crate::trf!("{what} ({n} Bilder)", what = what, n = n) })
}

// ───────────────────────────────────────────── Test hook

/// Test hook `GRAINEO_REJECT="<n>[@<ms>]"`: from 3 s after start, n times ←, X at intervals of
/// `ms` (default 20 ms) – due keys may also come several in one frame. Afterwards, once everything
/// is written, it checks memory and database/sidecars and writes the result to the
/// log ("ablehnen-test: …").
struct TestSequence {
    n: usize,
    interval: f64,
    start: Option<f64>,
    sent: usize,
    expected: Vec<(i64, PathBuf, String, Option<String>)>,
    /// Time at which the last key was injected
    last: Option<f64>,
    finished: bool,
}

static TEST_SEQUENCE: Mutex<Option<TestSequence>> = Mutex::new(None);

impl App {
    pub(crate) fn test_sequence(&mut self, ctx: &egui::Context) {
        let mut g = TEST_SEQUENCE.lock().unwrap();
        if g.is_none() {
            let Ok(v) = crate::env::var("GRAINEO_REJECT") else { return };
            let (n, ms) = v.split_once('@').map_or((v.as_str(), "20"), |(a, b)| (a, b));
            *g = Some(TestSequence { n: n.trim().parse().unwrap_or(10), interval: ms.trim().parse::<f64>().unwrap_or(20.0) / 1000.0, start: None, sent: 0, expected: Vec::new(), last: None, finished: false });
        }
        let Some(t) = g.as_mut() else { return };
        if t.finished {
            return;
        }
        let now = ctx.input(|i| i.time);
        ctx.request_repaint();
        let start = match t.start {
            Some(s) => s,
            None if now > 3.0 && !self.entries.is_empty() => {
                // from the last image of the list; expected: its n predecessors
                let a = self.entries.len() - 1;
                if self.lib.active_image() != Some(self.entries[a].image) {
                    let b = self.entries[a].image;
                    self.lib.set_active(b);
                    return;
                }
                t.expected = (1..=t.n).filter_map(|k| a.checked_sub(k)).map(|i| {
                    let e = &self.entries[i];
                    (e.image, e.path.clone(), e.format.clone(), e.copy.clone())
                }).collect();
                crate::log(&format!("ablehnen-test: start at image {a}, {} steps ← X every {:.0} ms, {}", t.expected.len(), t.interval * 1000.0, if self.in_grid() { crate::tr("Raster") } else { crate::tr("Einzelbild") }));
                t.start = Some(now);
                now
            }
            None => return,
        };
        // inject due keys (2 per step) in this order
        let due = (((now - start) / t.interval).floor() as usize + 1).min(2 * t.expected.len());
        let mut new = 0;
        while t.sent < due {
            let key = if t.sent % 2 == 0 { egui::Key::ArrowLeft } else { egui::Key::X };
            ctx.input_mut(|i| i.events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }));
            t.sent += 1;
            new += 1;
        }
        if new > 1 {
            crate::log(&format!("ablehnen-test: {new} keys in one frame"));
        }
        if t.sent < 2 * t.expected.len() {
            return;
        }
        let last = *t.last.get_or_insert(now);
        // check only in the next frame (the last key is processed then) and when everything is written
        if now - last < 0.1 || !self.lib.ratings_written() {
            return;
        }
        t.finished = true;
        let n = t.expected.len();
        let storage = t.expected.iter().filter(|(b, ..)| self.entries.iter().any(|e| e.image == *b && e.meta.flag == sidecar::Flag::Reject)).count();
        // persistent: database (library) or sidecar (browse folder)
        let db = library::db::Db::open(&library::db_path()).ok();
        let persistent = t
            .expected
            .iter()
            .filter(|(b, path, format, copy)| {
                let in_db = *b > 0 && db.as_ref().and_then(|d| d.conn.query_row("SELECT flag FROM image WHERE id=?1", [b], |r| r.get::<_, i64>(0)).ok()) == Some(-1);
                let raw = library::sync::raw_beside(path, format);
                let in_file = match library::sidecar::read(path, format, raw).0 {
                    library::xmp::ReadResult::Ok(w) => match copy {
                        None => w.flag == -1,
                        Some(k) => library::sidecar::read_copies(w.copies_json.as_deref()).iter().any(|x| &x.name == k && x.flag == -1),
                    },
                    _ => false,
                };
                in_db || in_file
            })
            .count();
        crate::log(&format!("ablehnen-test: {n} expected, {storage} rejected in memory, {persistent} persistent (database/sidecar) – {}", if storage == n && persistent == n { "OK" } else { crate::tr("FEHLER") }));
    }
}
