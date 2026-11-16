//! Building the column filters (`Facets`) without waiting: the keyword assignments are read with a read-only
//! connection of their own and the value lists are computed in a background thread (QoS UTILITY, USER_INITIATED while
//! the filter panel waits for them); the UI takes the finished result in a later frame (`LibraryView::poll_facets`).
//! Until then the filter bar shows its frame with a "loading" note. A library load that changed nothing the filters
//! read (same rows, keywords and assignments: same [`inputs_fingerprint`]) keeps the previous filters.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use library::db::{Db, Keyword, Row};

use crate::library_filter::{inputs_fingerprint, Facets, Remembered};

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
    /// the inputs are those of [`FacetsJob::previous`]: keep it
    Unchanged,
    /// the thread ended without a result
    Lost,
}

enum Outcome {
    Built(Box<Facets>),
    Unchanged,
}

/// A running build of the column filters.
pub struct FacetsJob {
    rx: Receiver<Outcome>,
    /// the selection to restore on the new filters (empty: first build)
    pub remembered: Remembered,
    /// number of rows the job builds for; a result for another number is useless
    pub rows_len: usize,
    /// the filters before this build: taken again if nothing they were built from changed
    pub previous: Option<Box<Facets>>,
    /// someone waits for the result (filter panel open): the thread raises its priority
    urgent: Arc<AtomicBool>,
}

/// All keyword assignments of the library (one read, ~150 ms for 340 000).
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

/// Priority of the build thread: someone waits for it, or it runs ahead of time.
fn priority(urgent: bool) -> library::platform::Priority {
    // not BACKGROUND: a thread at the very back starves behind the preview work of a freshly opened library (3.8 s
    // for ~100 ms of computing); UTILITY still keeps clear of the UI thread
    if urgent {
        library::platform::Priority::UserInitiated
    } else {
        library::platform::Priority::Utility
    }
}

impl FacetsJob {
    /// Start the build in a thread; `None` if no thread could be started. `ctx` is asked for a frame when done.
    /// `previous`: the filters so far (kept if the inputs did not change); `urgent`: the filter panel waits.
    pub fn start(rows: Arc<Vec<Row>>, keywords: Vec<Keyword>, source: Assignments, remembered: Remembered, previous: Option<Box<Facets>>, urgent: bool, ctx: Option<egui::Context>) -> Option<Self> {
        let (tx, rx) = channel();
        let rows_len = rows.len();
        let previous_fingerprint = previous.as_ref().map(|f| f.fingerprint);
        let urgent = Arc::new(AtomicBool::new(urgent));
        let hurry = urgent.clone();
        std::thread::Builder::new()
            .name("filter-spalten".into())
            .spawn(move || {
                let mut raised = hurry.load(Ordering::Relaxed);
                library::platform::set_priority(priority(raised));
                // between the phases: the panel was opened meanwhile
                let mut check = || {
                    if !raised && hurry.load(Ordering::Relaxed) {
                        raised = true;
                        library::platform::set_priority(priority(true));
                    }
                };
                let t = std::time::Instant::now();
                // test aid: makes the loading state of the filter bar visible (screenshots)
                if let Some(ms) = crate::env::var("GRAINEO_FACETS_DELAY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
                check();
                let assignments = match source {
                    Assignments::Known(k) => k,
                    // read-only: never waits for a write lock, never migrates
                    Assignments::FromDb(path) => Db::read_only(&path).map(|db| read_assignments(&db)).unwrap_or_default(),
                };
                let reading = t.elapsed().as_secs_f64() * 1000.0;
                check();
                let t_check = std::time::Instant::now();
                let fingerprint = inputs_fingerprint(&rows, &keywords, &assignments);
                let checking = t_check.elapsed().as_secs_f64() * 1000.0;
                let outcome = if previous_fingerprint == Some(fingerprint) {
                    crate::log(&format!("library: column filters unchanged, kept ({:.0} ms: reading {reading:.0}, check {checking:.0}; {} photos)", t.elapsed().as_secs_f64() * 1000.0, rows.len()));
                    Outcome::Unchanged
                } else {
                    check();
                    let (mut built, p) = Facets::build_timed(&rows, &keywords, &assignments);
                    built.fingerprint = fingerprint;
                    crate::log(&format!(
                        "library: column filters built in {:.0} ms (reading {reading:.0}, check {checking:.0}, rows {:.0}, keywords {:.0}, numbers {:.0}; {} photos, priority {})",
                        t.elapsed().as_secs_f64() * 1000.0,
                        p.rows,
                        p.keywords,
                        p.numbers,
                        rows.len(),
                        if raised { "user-initiated" } else { "utility" }
                    ));
                    Outcome::Built(Box::new(built))
                };
                let _ = tx.send(outcome);
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            })
            .ok()?;
        Some(Self { rx, remembered, rows_len, previous, urgent })
    }

    /// Is the result there? (every frame while the job runs; never blocks)
    pub fn poll(&mut self) -> Poll {
        match self.rx.try_recv() {
            Ok(Outcome::Built(f)) => Poll::Ready(f),
            Ok(Outcome::Unchanged) => Poll::Unchanged,
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }

    /// Someone waits for the filters (the panel is open): the build continues at a higher priority.
    pub fn hurry(&self) {
        self.urgent.store(true, Ordering::Relaxed);
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
                Poll::Lost | Poll::Unchanged => return None,
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        None
    }

    #[test]
    fn job_builds_in_background_and_restores_the_selection() {
        let rows = Arc::new(crate::library_filter::tests::test_rows(2000));
        let started = std::time::Instant::now();
        let mut job = FacetsJob::start(rows.clone(), Vec::new(), Assignments::Known(HashMap::new()), (vec![(0, "Canon".to_string())], Vec::new()), None, false, None).expect("thread");
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
        let mut job = FacetsJob::start(Arc::new(Vec::new()), Vec::new(), Assignments::FromDb(path), Default::default(), None, false, None).expect("thread");
        assert!(!job.selection_active());
        assert!(wait(&mut job).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn result(job: &mut FacetsJob) -> Option<Option<Box<Facets>>> {
        let t = std::time::Instant::now();
        while t.elapsed().as_secs() < 20 {
            match job.poll() {
                Poll::Ready(f) => return Some(Some(f)),
                Poll::Unchanged => return Some(None),
                Poll::Lost => return None,
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        None
    }

    /// A load that changed nothing the filters read keeps them (no rebuild); a change of a star rating, a keyword
    /// assignment or a keyword name builds them again.
    #[test]
    fn unchanged_inputs_keep_the_filters_and_changes_rebuild() {
        let kw = |id: i64, name: &str, parent: Option<i64>| Keyword { id, name: name.into(), parent, count: 1, person: false };
        let keywords = vec![kw(1, "Orte", None), kw(2, "Rom", Some(1))];
        let rows = Arc::new(crate::library_filter::tests::test_rows(500));
        let assignments = || HashMap::from([(3, vec![2]), (4, vec![1])]);
        let mut job = FacetsJob::start(rows.clone(), keywords.clone(), Assignments::Known(assignments()), Default::default(), None, false, None).unwrap();
        let first = result(&mut job).expect("result").expect("first build");
        assert_ne!(first.fingerprint, 0);
        // same inputs: kept
        let mut job = FacetsJob::start(rows.clone(), keywords.clone(), Assignments::Known(assignments()), Default::default(), Some(first), true, None).unwrap();
        assert!(result(&mut job).expect("result").is_none(), "unchanged: no rebuild");
        let first = job.previous.take().unwrap();
        // a star rating changed: rebuilt
        let mut changed = (*rows).clone();
        changed[10].stars = 4;
        let mut job = FacetsJob::start(Arc::new(changed), keywords.clone(), Assignments::Known(assignments()), Default::default(), Some(first), false, None).unwrap();
        let rebuilt = result(&mut job).expect("result").expect("rebuilt after a rating");
        let first = job.previous.take().unwrap();
        assert_ne!(rebuilt.fingerprint, first.fingerprint);
        // an assignment changed: rebuilt
        let mut other = assignments();
        other.insert(5, vec![1]);
        let mut job = FacetsJob::start(rows.clone(), keywords.clone(), Assignments::Known(other), Default::default(), Some(first), false, None).unwrap();
        assert!(result(&mut job).expect("result").is_some(), "rebuilt after an assignment");
        let first = job.previous.take().unwrap();
        // a keyword renamed: rebuilt
        let renamed = vec![kw(1, "Orte", None), kw(2, "Roma", Some(1))];
        let mut job = FacetsJob::start(rows.clone(), renamed, Assignments::Known(assignments()), Default::default(), Some(first), false, None).unwrap();
        assert!(result(&mut job).expect("result").is_some(), "rebuilt after a rename");
    }
}
