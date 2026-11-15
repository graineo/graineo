//! Writing to the library without waiting: the UI (main thread) never waits for a write lock
//! and never for files (sidecars, moving to the NAS). Jobs run in order in a dedicated
//! thread with its own connection; if the library is currently busy (sync, faces …), it waits patiently
//! (up to two minutes, see `library::db`). The UI applies the result in the next frame
//! (`LibraryView::prepare`).

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use library::db::Db;
use eframe::egui;

use crate::library_ui::{Action, LibraryView};

/// Completion callback of a job; runs on the main thread.
pub type Done = Box<dyn FnOnce(&mut LibraryView, &mut Vec<Action>) + Send>;
type Task = Box<dyn FnOnce(Result<&Db, &str>) -> Done + Send>;

#[derive(Default)]
pub struct Writer {
    tx: Option<Sender<Task>>,
    rx: Option<Receiver<Done>>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    opened: usize,
    /// jobs sent but not yet carried out by the thread (for a clean quit)
    unfinished: Arc<std::sync::atomic::AtomicUsize>,
    /// results of a writer thread that ended, still to be picked up
    leftover: Vec<Done>,
}

impl Writer {
    /// Run `work` in the background, then `done` with its result on the main thread.
    pub fn task<R: Send + 'static>(
        &mut self,
        db_path: PathBuf,
        work: impl FnOnce(&Db) -> Result<R, String> + Send + 'static,
        done: impl FnOnce(&mut LibraryView, &mut Vec<Action>, Result<R, String>) + Send + 'static,
    ) {
        let a: Task = Box::new(move |db| {
            let r = match db {
                Ok(db) => work(db),
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = &r {
                crate::log(&format!("library: write failed: {e}"));
            }
            Box::new(move |b: &mut LibraryView, actions: &mut Vec<Action>| done(b, actions, r))
        });
        self.unfinished.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut a = a;
        for _ in 0..2 {
            if self.tx.is_none() {
                self.spawn(db_path.clone());
            }
            match self.tx.as_ref().map(|tx| tx.send(a)) {
                Some(Ok(())) => {
                    self.opened += 1;
                    return;
                }
                Some(Err(e)) => {
                    // the thread has ended: start a new one and hand the job to it
                    crate::log("library: writer thread gone, starting a new one");
                    a = e.0;
                    self.tx = None;
                }
                None => break,
            }
        }
        self.unfinished.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        crate::log("library: writer thread not started, job not carried out");
    }

    /// Start the writer thread (again).
    fn spawn(&mut self, db_path: PathBuf) {
        let (tx, rx) = channel::<Task>();
        let (tx_done, rx_done) = channel::<Done>();
        let ctx = self.ctx.clone();
        let unfinished = self.unfinished.clone();
        std::thread::Builder::new()
            .name("bibliothek-schreiber".into())
            .spawn(move || {
                let db = Db::open(&db_path).map_err(|e| e.to_string());
                for a in rx {
                    // a panic in one job ends neither the thread nor the count of open jobs; the view shows it
                    let f = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a(db.as_ref().map_err(|e| e.as_str())))).unwrap_or_else(|_| -> Done {
                        crate::log("library: write job crashed");
                        Box::new(|b: &mut LibraryView, _: &mut Vec<Action>| b.error = Some(crate::tr("Eine Änderung konnte nicht in die Bibliothek geschrieben werden (interner Fehler).").to_string()))
                    });
                    unfinished.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    if tx_done.send(f).is_err() {
                        break;
                    }
                    if let Some(c) = ctx.lock().unwrap().as_ref() {
                        c.request_repaint();
                    }
                }
            })
            .ok();
        self.tx = Some(tx);
        if let Some(old) = self.rx.replace(rx_done) {
            // results of the ended thread still to be picked up
            self.leftover.extend(old.try_iter());
        }
    }

    /// Pick up finished jobs (every frame); `ctx` for repainting after the next finished job.
    pub fn fetch(&mut self, ctx: &egui::Context) -> Vec<Done> {
        let mut c = self.ctx.lock().unwrap();
        if c.is_none() {
            *c = Some(ctx.clone());
        }
        drop(c);
        let mut v: Vec<Done> = std::mem::take(&mut self.leftover);
        v.extend(self.rx.as_ref().map(|rx| rx.try_iter().collect::<Vec<_>>()).unwrap_or_default());
        self.opened = self.opened.saturating_sub(v.len());
        v
    }

    /// Are jobs still running?
    pub fn busy(&self) -> bool {
        self.opened > 0
    }

    /// Has the thread carried out every job sent so far (written to the library)? Unlike [`Self::busy`]
    /// this does not wait for the main thread to pick up the results – for quitting.
    pub fn all_written(&self) -> bool {
        self.unfinished.load(std::sync::atomic::Ordering::SeqCst) == 0
    }
}
