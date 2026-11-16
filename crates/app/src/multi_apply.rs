//! New settings for many photos in the background (multi-selection: "Apply to n photos", undo/redo of such a command,
//! paste/transfer onto many photos).
//!
//! The UI thread only hands over a request (photos + what to do) and never waits: one helper thread at background
//! priority loads each photo's settings, computes the new ones and saves them **in blocks** of [`BLOCK`] photos – one
//! short library transaction per block ([`library::store::batch`]), never the whole selection at once. After each block
//! the UI gets the changed photos (settings before and after) and shows the progress ("1 234 of 2 806 saved").
//!
//! - Cancelling stops before the next block: a block is either saved completely or not at all; what was saved stays
//!   and is undoable.
//! - Requests run one after the other, never two over the same photos at the same time. A request that has not started
//!   yet takes a following one for the same photos along (both steps applied in one pass, the last state counts).
//! - The previews of the changed photos are re-rendered afterwards, visible cells first (see `multi_edit.rs`).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use develop::{Settings, View};

use crate::settings_group::Change;
use crate::sidecar::Meta;

/// Photos per block (one library transaction each).
pub const BLOCK: usize = 250;

/// A step that turns a photo's settings into the new ones.
pub type Step = Arc<dyn Fn(&Settings) -> Settings + Send + Sync>;

/// One photo of a request. `set`: exactly these settings (undo/redo); otherwise the steps of the request.
#[derive(Clone)]
pub struct Target {
    pub image: i64,
    pub copy: Option<String>,
    pub path: PathBuf,
    pub set: Option<Settings>,
}

pub struct Request {
    pub id: u64,
    pub steps: Vec<Step>,
    pub targets: Vec<Target>,
    /// an undoable command of its own (not: undo/redo of one)
    pub record: bool,
    /// the develop history: steps of this command (written with the settings, in the same block)
    pub history: Option<HistoryJob>,
}

/// What the develop history needs of a request: the command, whether it is its undo, and the photos whose history the
/// app holds in memory (it records theirs itself when the block arrives).
#[derive(Clone)]
pub struct HistoryJob {
    pub cmd: crate::history::Command,
    pub undo: bool,
    pub memory: std::collections::HashSet<(i64, Option<String>)>,
}

/// From the helper thread to the UI.
pub enum Msg {
    /// One block saved: its photos that changed (with their rating, as loaded), progress of the request.
    Block { id: u64, changes: Vec<(Change, Meta)>, done: usize, total: usize },
    /// The request ended (completely or cancelled before a block); errors while saving.
    End { id: u64, cancelled: bool, errors: usize, error: Option<String> },
}

/// Settings as stored (the view is not part of them).
fn stored(s: &Settings) -> Settings {
    Settings { view: View::Developed, ..s.clone() }
}

/// Carry out one request: block by block, `cancelled` is asked before each block, `load` gives a photo's current
/// settings and rating, `save` stores one block (returns the error messages). Pure logic (tests with stand-ins).
pub fn run(
    req: &Request,
    cancelled: &dyn Fn() -> bool,
    load: &dyn Fn(&Target) -> Result<(Settings, Meta), String>,
    save: &mut dyn FnMut(&[(&Target, &Settings, &Settings, &Meta)]) -> Vec<String>,
    emit: &mut dyn FnMut(Msg),
) {
    let total = req.targets.len();
    let (mut done, mut errors, mut error) = (0, 0, None);
    for block in req.targets.chunks(BLOCK) {
        if cancelled() {
            emit(Msg::End { id: req.id, cancelled: true, errors, error });
            return;
        }
        let mut changes: Vec<(&Target, Change, Meta)> = Vec::new();
        for t in block {
            let (now, meta) = match load(t) {
                Ok(v) => v,
                Err(e) => {
                    errors += 1;
                    error = Some(e);
                    continue;
                }
            };
            let before = stored(&now);
            let after = match &t.set {
                Some(s) => stored(s),
                None => stored(&req.steps.iter().fold(now, |s, f| f(&s))),
            };
            if after != before {
                changes.push((t, Change { image: t.image, copy: t.copy.clone(), before, after }, meta));
            }
        }
        let list: Vec<(&Target, &Settings, &Settings, &Meta)> = changes.iter().map(|(t, c, m)| (*t, &c.before, &c.after, m)).collect();
        let failed = if list.is_empty() { Vec::new() } else { save(&list) };
        if let Some(e) = failed.last() {
            error = Some(e.clone());
        }
        errors += failed.len();
        done += block.len();
        emit(Msg::Block { id: req.id, changes: changes.into_iter().map(|(_, c, m)| (c, m)).collect(), done, total });
    }
    emit(Msg::End { id: req.id, cancelled: false, errors, error });
}

/// Load a photo's settings the way the UI does (library, sidecar, initial values from the camera file).
fn load_photo(t: &Target) -> Result<(Settings, Meta), String> {
    // the selection is chosen by file name; a file that is not really a developable photo stays as it is
    if !crate::image_kind::of(&t.path).editable() {
        return Err(crate::trf!("{a} lässt sich nicht bearbeiten", a = t.path.display()));
    }
    let (mut s, meta, _) = crate::sidecar::load_entry(&t.path, t.copy.as_deref());
    rawdev::film_profiles::remember_reference(&mut s);
    Ok((s, meta))
}

/// Save one block: every photo through the usual path, the library writes of the block in one transaction. The
/// rating is read again right before (a star set meanwhile must not be overwritten with the one loaded earlier). The
/// step of the develop history goes into the same transaction (photos whose history the app holds: it records them).
fn save_block(list: &[(&Target, &Settings, &Settings, &Meta)], history: Option<&HistoryJob>) -> Vec<String> {
    library::store::batch(|| {
        let mut errors = Vec::new();
        for (t, before, s, meta) in list {
            let loc = library::store::locate(&t.path);
            let step = history.filter(|h| !h.memory.contains(&(t.image, t.copy.clone()))).zip(loc);
            if let Some((h, (file, _))) = step {
                let (b, a) = (crate::history::value(before), crate::history::value(s));
                library::store::command_step(file, t.copy.as_deref(), &b, &a, h.cmd.hint.kind, &h.cmd.hint.label, h.cmd.id, h.undo);
            }
            let fresh = loc.and_then(|(file, _)| library::store::meta(file, t.copy.as_deref())).map(|m| Meta { rating: m.0, flag: crate::sidecar::Flag::from_i8(m.1), label: m.2 });
            let meta = fresh.unwrap_or_else(|| (*meta).clone());
            if let Err(e) = crate::sidecar::save_entry(&t.path, t.copy.as_deref(), s, &meta) {
                errors.push(format!("{}: {e}", t.path.display()));
            }
            // also when the settings needed no write (equal to the stored ones)
            if let Some((_, (file, _))) = step {
                library::store::send_history_of(file, t.copy.as_deref());
            }
        }
        errors
    })
}

struct Queue {
    waiting: VecDeque<Request>,
    /// requests up to this id are cancelled (also the one running)
    cancel_upto: u64,
    /// the request being carried out
    running: Option<u64>,
}

/// The helper thread and its queue (one per app).
pub struct Runner {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    started: bool,
    next: AtomicU64,
}

impl Default for Runner {
    fn default() -> Self {
        let (tx, rx) = channel();
        Runner { shared: Arc::new((Mutex::new(Queue { waiting: VecDeque::new(), cancel_upto: 0, running: None }), Condvar::new())), rx, tx, started: false, next: AtomicU64::new(1) }
    }
}

impl Runner {
    /// Hand over a request. Returns its id; when it was taken along by the request still waiting before it, that one's id.
    pub fn submit(&mut self, ctx: Option<&eframe::egui::Context>, steps: Vec<Step>, targets: Vec<Target>, record: bool) -> u64 {
        self.submit_with(ctx, steps, targets, record, None)
    }

    /// [`Self::submit`] with the steps of the develop history.
    pub fn submit_with(&mut self, ctx: Option<&eframe::egui::Context>, steps: Vec<Step>, targets: Vec<Target>, record: bool, history: Option<HistoryJob>) -> u64 {
        self.start(ctx.cloned());
        let (m, cv) = &*self.shared;
        let mut q = m.lock().unwrap_or_else(|e| e.into_inner());
        // a waiting request with the same photos takes the new step along (the last state counts, one pass)
        if let Some(last) = q.waiting.back_mut() {
            let same = last.record == record && targets.iter().all(|t| t.set.is_none()) && last.targets.iter().all(|t| t.set.is_none()) && last.targets.len() == targets.len() && last.targets.iter().zip(&targets).all(|(a, b)| a.image == b.image && a.copy == b.copy);
            if same && !steps.is_empty() {
                last.steps.extend(steps);
                return last.id;
            }
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        q.waiting.push_back(Request { id, steps, targets, record, history });
        cv.notify_all();
        id
    }

    /// Cancel everything handed over so far (the running request stops before its next block).
    pub fn cancel(&self) {
        let (m, cv) = &*self.shared;
        let mut q = m.lock().unwrap_or_else(|e| e.into_inner());
        q.cancel_upto = self.next.load(Ordering::SeqCst).saturating_sub(1);
        cv.notify_all();
    }

    /// Messages of the helper thread since the last call.
    pub fn take(&self) -> Vec<Msg> {
        self.rx.try_iter().collect()
    }

    fn start(&mut self, ctx: Option<eframe::egui::Context>) {
        if self.started {
            return;
        }
        self.started = true;
        let shared = self.shared.clone();
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("multi-apply".into())
            .spawn(move || {
                // the user waits for it (progress in view): not throttled like background work, but one thread only
                library::platform::set_priority(library::platform::Priority::Utility);
                loop {
                    let req = {
                        let (m, cv) = &*shared;
                        let mut q = m.lock().unwrap_or_else(|e| e.into_inner());
                        q.running = None;
                        loop {
                            if let Some(r) = q.waiting.pop_front() {
                                q.running = Some(r.id);
                                break r;
                            }
                            q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let id = req.id;
                    let cancelled = || shared.0.lock().map(|q| q.cancel_upto >= id).unwrap_or(false);
                    let mut emit = |m: Msg| {
                        let _ = tx.send(m);
                        if let Some(c) = &ctx {
                            c.request_repaint();
                        }
                    };
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&req, &cancelled, &load_photo, &mut |l| save_block(l, req.history.as_ref()), &mut emit)));
                    if r.is_err() {
                        emit(Msg::End { id, cancelled: true, errors: 1, error: Some(crate::tr("interner Fehler beim Speichern").to_string()) });
                    }
                }
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn targets(n: usize) -> Vec<Target> {
        (0..n as i64).map(|i| Target { image: i, copy: None, path: PathBuf::from(format!("/x/{i}.X3F")), set: None }).collect()
    }

    fn plus(ev: f64) -> Step {
        Arc::new(move |s: &Settings| Settings { exposure: s.exposure + ev, ..s.clone() })
    }

    /// A stand-in library: settings per photo, every block saved as one "transaction".
    struct Lib {
        s: RefCell<HashMap<i64, Settings>>,
        transactions: RefCell<Vec<usize>>,
    }

    impl Lib {
        fn new(n: usize) -> Lib {
            Lib { s: RefCell::new((0..n as i64).map(|i| (i, Settings { exposure: i as f64 * 0.01, ..Settings::default() })).collect()), transactions: RefCell::new(Vec::new()) }
        }
        fn go(&self, req: &Request, cancel_after_blocks: Option<usize>) -> Vec<Msg> {
            let mut out = Vec::new();
            let load = |t: &Target| -> Result<(Settings, Meta), String> { Ok((self.s.borrow()[&t.image].clone(), Meta::default())) };
            let mut save = |l: &[(&Target, &Settings, &Settings, &Meta)]| {
                for (t, _, s, _) in l {
                    self.s.borrow_mut().insert(t.image, (*s).clone());
                }
                self.transactions.borrow_mut().push(l.len());
                Vec::new()
            };
            let cancelled = || cancel_after_blocks.is_some_and(|k| self.transactions.borrow().len() >= k);
            run(req, &cancelled, &load, &mut save, &mut |m| out.push(m));
            out
        }
    }

    #[test]
    fn blocks_of_250_one_transaction_each_with_progress() {
        let lib = Lib::new(3000);
        let req = Request { id: 1, steps: vec![plus(0.5)], targets: targets(3000), record: true, history: None };
        let msgs = lib.go(&req, None);
        assert_eq!(*lib.transactions.borrow(), vec![250; 12], "12 blocks of 250, one transaction each");
        let progress: Vec<(usize, usize)> = msgs.iter().filter_map(|m| if let Msg::Block { done, total, .. } = m { Some((*done, *total)) } else { None }).collect();
        assert_eq!(progress.first(), Some(&(250, 3000)));
        assert_eq!(progress.last(), Some(&(3000, 3000)));
        assert!(matches!(msgs.last(), Some(Msg::End { cancelled: false, errors: 0, .. })));
        let changes: usize = msgs.iter().map(|m| if let Msg::Block { changes, .. } = m { changes.len() } else { 0 }).sum();
        assert_eq!(changes, 3000);
        assert!((lib.s.borrow()[&2999].exposure - (29.99 + 0.5)).abs() < 1e-9, "relative: own value + change");
    }

    #[test]
    fn last_block_may_be_short() {
        let lib = Lib::new(2806);
        lib.go(&Request { id: 1, steps: vec![plus(1.0)], targets: targets(2806), record: true, history: None }, None);
        let t = lib.transactions.borrow();
        assert_eq!(t.len(), 12);
        assert_eq!(*t.last().unwrap(), 2806 - 11 * 250);
    }

    #[test]
    fn cancel_stops_between_blocks_never_half_a_block() {
        let lib = Lib::new(3000);
        let msgs = lib.go(&Request { id: 1, steps: vec![plus(1.0)], targets: targets(3000), record: true, history: None }, Some(3));
        assert_eq!(*lib.transactions.borrow(), vec![250, 250, 250]);
        assert!(matches!(msgs.last(), Some(Msg::End { cancelled: true, .. })));
        let s = lib.s.borrow();
        let changed = (0..3000).filter(|&i| (s[&i].exposure - (i as f64 * 0.01 + 1.0)).abs() < 1e-9).count();
        let unchanged = (0..3000).filter(|&i| (s[&i].exposure - i as f64 * 0.01).abs() < 1e-9).count();
        assert_eq!((changed, unchanged), (750, 2250), "whole blocks only");
    }

    #[test]
    fn undo_restores_every_photo_in_one_request() {
        let lib = Lib::new(600);
        let start: HashMap<i64, Settings> = lib.s.borrow().clone();
        let msgs = lib.go(&Request { id: 1, steps: vec![plus(0.3)], targets: targets(600), record: true, history: None }, None);
        let changes: Vec<Change> = msgs.into_iter().flat_map(|m| if let Msg::Block { changes, .. } = m { changes.into_iter().map(|(c, _)| c).collect() } else { Vec::new() }).collect();
        // one undo step: all photos back (the request of ⌘Z sets "before")
        let back: Vec<Target> = changes.iter().map(|c| Target { image: c.image, copy: None, path: PathBuf::new(), set: Some(c.before.clone()) }).collect();
        lib.go(&Request { id: 2, steps: vec![], targets: back, record: false, history: None }, None);
        let s = lib.s.borrow();
        assert!((0..600).all(|i| s[&i] == stored(&start[&i])));
    }

    #[test]
    fn photos_that_do_not_change_are_not_saved() {
        let lib = Lib::new(10);
        let msgs = lib.go(&Request { id: 1, steps: vec![Arc::new(|s: &Settings| s.clone())], targets: targets(10), record: true, history: None }, None);
        assert!(lib.transactions.borrow().is_empty());
        assert!(matches!(msgs.last(), Some(Msg::End { cancelled: false, .. })));
    }

    #[test]
    fn a_waiting_request_takes_the_next_step_along() {
        let mut r = Runner { started: true, ..Runner::default() };
        let a = r.submit(None, vec![plus(0.1)], targets(5), true);
        let b = r.submit(None, vec![plus(0.2)], targets(5), true);
        assert_eq!(a, b, "same photos, not started yet: one request");
        let c = r.submit(None, vec![plus(0.2)], targets(4), true);
        assert_ne!(c, a, "other photos: a request of its own");
        let q = r.shared.0.lock().unwrap();
        assert_eq!(q.waiting.len(), 2);
        assert_eq!(q.waiting[0].steps.len(), 2);
        let s = q.waiting[0].steps.iter().fold(Settings::default(), |s, f| f(&s));
        assert!((s.exposure - 0.3).abs() < 1e-9);
    }

    /// The real path: photos of the library (synthetic PNGs), saved block by block in the background – one library
    /// transaction per block, the library has the new settings afterwards. Needs `GRAINEO_DATA_DIR`.
    #[test]
    fn library_gets_one_transaction_per_block() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let _app_lock = crate::ui_test::app_lock();
        let root = std::env::temp_dir().join(format!("graineo-multi-apply-{}", std::process::id()));
        let dir = root.join("A");
        std::fs::create_dir_all(&dir).unwrap();
        let n = 600;
        for k in 0..n {
            image::RgbImage::from_pixel(8, 6, image::Rgb([(k % 200) as u8, 90, 60])).save(dir.join(format!("p{k:04}.png"))).unwrap();
        }
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
        let files: Vec<(i64, PathBuf)> = (0..n).map(|k| dir.join(format!("p{k:04}.png"))).map(|p| (library::store::locate(&p).expect("in the library").0, p)).collect();
        let targets: Vec<Target> = files.iter().map(|(f, p)| Target { image: *f, copy: None, path: p.clone(), set: None }).collect();
        let before = library::store::commits();
        let mut r = Runner::default();
        let id = r.submit(None, vec![plus(0.5)], targets, true);
        let (mut blocks, mut changed, mut end) = (0, 0, None);
        let t = std::time::Instant::now();
        while end.is_none() {
            for m in r.take() {
                match m {
                    Msg::Block { id: i, changes, .. } if i == id => {
                        blocks += 1;
                        changed += changes.len();
                    }
                    Msg::End { cancelled, errors, .. } => end = Some((cancelled, errors)),
                    _ => {}
                }
            }
            assert!(t.elapsed() < crate::ui_test::WAIT, "background apply does not end");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(end, Some((false, 0)));
        assert_eq!((blocks, changed), (3, n));
        assert!(library::store::flush(std::time::Duration::from_secs(60)));
        let commits = library::store::commits() - before;
        assert!((3..=6).contains(&commits), "{commits} transactions for {n} photos (3 blocks)");
        for (f, _) in &files {
            let s: Settings = serde_json::from_str(&library::store::develop(*f, None).expect("saved")).unwrap();
            assert!((s.exposure - 0.5).abs() < 1e-9);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The develop history of a command for 200 photos: one step per photo with the command's id, written in the
    /// blocks of the settings; its undo moves every photo's cursor back (the step stays, dimmed), the redo forward.
    #[test]
    fn history_steps_for_200_photos_and_their_undo() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let _app_lock = crate::ui_test::app_lock();
        let root = std::env::temp_dir().join(format!("graineo-multi-history-{}", std::process::id()));
        let dir = root.join("H");
        std::fs::create_dir_all(&dir).unwrap();
        let n = 200;
        for k in 0..n {
            image::RgbImage::from_pixel(8, 6, image::Rgb([(k % 200) as u8, 70, 60])).save(dir.join(format!("h{k:04}.png"))).unwrap();
        }
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &root, &|_| {}).unwrap();
        let files: Vec<(i64, PathBuf)> = (0..n).map(|k| dir.join(format!("h{k:04}.png"))).map(|p| (library::store::locate(&p).expect("in the library").0, p)).collect();
        let cmd = crate::history::Command::new(crate::history::Hint::paste(Some("h0000.png")));
        let job = |undo: bool| Some(HistoryJob { cmd: cmd.clone(), undo, memory: Default::default() });
        let mut r = Runner::default();
        let wait = |r: &Runner, id: u64| -> Vec<Change> {
            let t = std::time::Instant::now();
            let mut changes = Vec::new();
            loop {
                for m in r.take() {
                    match m {
                        Msg::Block { id: i, changes: c, .. } if i == id => changes.extend(c.into_iter().map(|(c, _)| c)),
                        Msg::End { id: i, .. } if i == id => return changes,
                        _ => {}
                    }
                }
                assert!(t.elapsed() < crate::ui_test::WAIT, "background apply does not end");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let targets: Vec<Target> = files.iter().map(|(f, p)| Target { image: *f, copy: None, path: p.clone(), set: None }).collect();
        let id = r.submit_with(None, vec![plus(0.4)], targets, true, job(false));
        let changes = wait(&r, id);
        assert_eq!(changes.len(), n);
        assert!(library::store::flush(std::time::Duration::from_secs(60)));
        let conn = library::db::Db::open(&library::db_path()).unwrap();
        for (f, _) in &files {
            let (steps, cursor) = library::develop_history::load(&conn.conn, *f, None).unwrap();
            assert_eq!(steps.len(), 2, "start + paste");
            assert_eq!((steps[1].kind.as_str(), steps[1].command, cursor), ("paste", Some(cmd.id), None));
        }
        // undo of the command: every cursor back on the start, the paste step stays
        let back: Vec<Target> = changes.iter().map(|c| Target { image: c.image, copy: None, path: files.iter().find(|(f, _)| *f == c.image).unwrap().1.clone(), set: Some(c.before.clone()) }).collect();
        let id = r.submit_with(None, Vec::new(), back, false, job(true));
        wait(&r, id);
        assert!(library::store::flush(std::time::Duration::from_secs(60)));
        for (f, _) in &files {
            let (steps, cursor) = library::develop_history::load(&conn.conn, *f, None).unwrap();
            assert_eq!((steps.len(), cursor), (2, Some(0)));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
