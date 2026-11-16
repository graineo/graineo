//! Building the column filters (`Facets`) without waiting: the keyword assignments are read with a read-only
//! connection of their own and the value lists are computed in a background thread (QoS UTILITY); the UI takes the
//! finished result in a later frame (`LibraryView::poll_facets`). Until then the filter bar shows its frame with a
//! "loading" note. With 65 000 photos and 340 000 keyword assignments the work takes a noticeable time and must
//! never run in the UI thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use library::db::{Db, Keyword, Row};

use crate::library_filter::{Facets, Remembered};

/// Where the keyword assignments (image → keyword IDs) come from.
pub enum Assignments {
    /// already read (a library load brings them)
    Known(HashMap<i64, Vec<i64>>),
    /// read in the job from this library file
    FromDb(PathBuf),
}

/// Result of [`FacetsJob::poll`].
pub enum Poll {
    Pending,
    Ready(Box<Facets>),
    /// the thread ended without a result
    Lost,
}

/// A running build of the column filters.
pub struct FacetsJob {
    rx: Receiver<Facets>,
    /// the selection to restore on the new filters (empty: first build)
    pub remembered: Remembered,
    /// number of rows the job builds for; a result for another number is useless
    pub rows_len: usize,
}

/// All keyword assignments of the library (one read, a few 10 ms for 340 000).
pub fn read_assignments(db: &Db) -> HashMap<i64, Vec<i64>> {
    let mut keywords: HashMap<i64, Vec<i64>> = HashMap::new();
    if let Ok(mut st) = db.conn.prepare("SELECT image, keyword FROM image_keyword") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
            for (b, k) in rows.flatten() {
                keywords.entry(b).or_default().push(k);
            }
        }
    }
    keywords
}

impl FacetsJob {
    /// Start the build in a thread; `None` if no thread could be started. `ctx` is asked for a frame when done.
    pub fn start(rows: Arc<Vec<Row>>, keywords: Vec<Keyword>, source: Assignments, remembered: Remembered, ctx: Option<egui::Context>) -> Option<Self> {
        let (tx, rx) = channel();
        let rows_len = rows.len();
        std::thread::Builder::new()
            .name("filter-spalten".into())
            .spawn(move || {
                // not BACKGROUND: someone is waiting for the lists, and a thread at the very back starves behind the
                // preview work of a freshly opened library (3.8 s for ~100 ms of computing); UTILITY still keeps
                // clear of the UI thread
                library::platform::set_priority(library::platform::Priority::Utility);
                let t = std::time::Instant::now();
                // test aid: makes the loading state of the filter bar visible (screenshots)
                if let Some(ms) = crate::env::var("GRAINEO_FACETS_DELAY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
                let assignments = match source {
                    Assignments::Known(k) => k,
                    // read-only: never waits for a write lock, never migrates
                    Assignments::FromDb(path) => Db::read_only(&path).map(|db| read_assignments(&db)).unwrap_or_default(),
                };
                let built = Facets::build(&rows, &keywords, &assignments);
                crate::log(&format!("library: column filters built in {:.0} ms (background, {} photos)", t.elapsed().as_secs_f64() * 1000.0, rows.len()));
                let _ = tx.send(built);
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            })
            .ok()?;
        Some(Self { rx, remembered, rows_len })
    }

    /// Is the result there? (every frame while the job runs; never blocks)
    pub fn poll(&mut self) -> Poll {
        match self.rx.try_recv() {
            Ok(f) => Poll::Ready(Box::new(f)),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }

    /// Does the job restore a selection (the grid must wait for it, otherwise it would flash unfiltered)?
    pub fn selection_active(&self) -> bool {
        !self.remembered.0.is_empty() || !self.remembered.1.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(job: &mut FacetsJob) -> Option<Box<Facets>> {
        let t = std::time::Instant::now();
        while t.elapsed().as_secs() < 20 {
            match job.poll() {
                Poll::Ready(f) => return Some(f),
                Poll::Lost => return None,
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        None
    }

    #[test]
    fn job_builds_in_background_and_restores_the_selection() {
        let rows = Arc::new(crate::library_filter::tests::test_rows(2000));
        let started = std::time::Instant::now();
        let mut job = FacetsJob::start(rows.clone(), Vec::new(), Assignments::Known(HashMap::new()), (vec![(0, "Canon".to_string())], Vec::new()), None).expect("thread");
        // starting returns at once (the build is not done in the caller)
        assert!(started.elapsed().as_millis() < 500);
        assert!(job.selection_active());
        assert_eq!(job.rows_len, 2000);
        let mut f = wait(&mut job).expect("result");
        f.restore(&job.remembered);
        assert!(f.active());
        assert_eq!(f.rows_len(), 2000);
        let basis: Vec<usize> = (0..rows.len()).collect();
        let out = f.apply(&rows, &basis);
        assert!(!out.is_empty() && out.len() < rows.len());
    }

    #[test]
    fn job_reads_assignments_from_the_database() {
        let dir = std::env::temp_dir().join(format!("graineo-facets-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");
        let db = Db::open(&path).unwrap();
        db.conn.execute("PRAGMA foreign_keys = OFF", []).ok();
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES (1, 'Rom', 0)", []).unwrap();
        db.conn.execute("INSERT INTO image_keyword VALUES (7, 1)", []).unwrap();
        drop(db);
        let read = Db::read_only(&path).map(|d| read_assignments(&d)).unwrap();
        assert_eq!(read.get(&7), Some(&vec![1]));
        let mut job = FacetsJob::start(Arc::new(Vec::new()), Vec::new(), Assignments::FromDb(path), Default::default(), None).expect("thread");
        assert!(!job.selection_active());
        assert!(wait(&mut job).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
