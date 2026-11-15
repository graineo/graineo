//! The search runs in the background. Typing only changes the field and the suggestions; after a short
//! pause the query (full text and fields) is evaluated on a worker thread with its own read-only database connection,
//! an outdated evaluation stops between its terms. The grid keeps the last complete result until the new one is
//! there, then switches in one go (with the fade of `search_motion`).
//! Submodule of `library_ui` (accesses its fields).

use super::*;
use std::time::{Duration, Instant};

/// Pause after the last keystroke before the evaluation starts.
const DEBOUNCE: Duration = Duration::from_millis(110);

/// A finished evaluation.
pub(super) struct Done {
    pub text: String,
    pub query: search::Query,
    /// bit per row of the rows it was computed on (`None`: empty query)
    pub matches: Option<Vec<bool>>,
    /// identity of those rows (same `Arc`): a result for an older load is thrown away
    rows: usize,
    pub ms: f64,
}

#[derive(Default)]
pub(super) struct SearchJob {
    latest: Arc<AtomicU64>,
    slot: Arc<Mutex<Option<(u64, Done)>>>,
    /// text wished and since when (typing restarts the pause)
    wish: Option<(String, Instant)>,
    /// evaluation running for this text
    running: Option<(u64, String)>,
    /// since when the shown result is outdated (dimming, "sucht …")
    outdated: Option<Instant>,
    /// the next wish is evaluated at once in the frame (test hooks that read the list right after setting the text)
    pub sync_next: bool,
    /// rank of every row in the order "newest first" (for which rows: address and length), computed once per load:
    /// sorting a search result by it costs a few milliseconds instead of comparing texts
    capture_rank: Option<((usize, usize), Vec<u32>)>,
}

thread_local! {
    /// read-only connection of a worker thread (kept: prepared statements and the page cache stay warm)
    static CONN: std::cell::RefCell<Option<Db>> = const { std::cell::RefCell::new(None) };
}

impl SearchJob {
    /// The field now reads `text` (not empty): evaluate it after the pause; an older evaluation is outdated.
    pub fn wish(&mut self, text: &str) {
        if self.wish.as_ref().is_some_and(|w| w.0 == text) || (self.wish.is_none() && self.running.as_ref().is_some_and(|r| r.1 == text)) {
            return;
        }
        self.wish = Some((text.to_string(), Instant::now()));
        self.latest.fetch_add(1, Ordering::Relaxed);
        self.running = None;
        self.outdated.get_or_insert_with(Instant::now);
    }

    /// Nothing to search any more (field emptied): withdraw everything.
    pub fn cancel(&mut self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        self.wish = None;
        self.running = None;
        self.outdated = None;
    }

    /// Waiting for a result (pause or evaluation)?
    pub fn busy(&self) -> bool {
        self.wish.is_some() || self.running.is_some()
    }

    /// Since when the shown result is outdated.
    pub fn outdated_since(&self) -> Option<Instant> {
        self.outdated
    }

    /// Per frame: hand over a finished result, start the wished evaluation after the pause. Never computes or waits.
    pub fn poll(&mut self, ctx: &egui::Context, rows: &Arc<Vec<Row>>, folders: &Arc<HashMap<i64, String>>) -> Option<Done> {
        let mut out = None;
        if let Some((id, done)) = self.slot.lock().unwrap().take() {
            if id == self.latest.load(Ordering::Relaxed) && done.rows == Arc::as_ptr(rows) as usize {
                self.running = None;
                if self.wish.is_none() {
                    self.outdated = None;
                }
                out = Some(done);
            } else if self.running.as_ref().is_some_and(|r| r.0 == id) {
                // computed on an older load: once more on the current rows
                let text = self.running.take().map(|r| r.1).unwrap_or_default();
                self.wish = Some((text, Instant::now() - DEBOUNCE));
            }
        }
        let Some((text, since)) = self.wish.clone() else { return out };
        let left = DEBOUNCE.saturating_sub(since.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
            return out;
        }
        self.wish = None;
        let id = self.latest.fetch_add(1, Ordering::Relaxed) + 1;
        self.running = Some((id, text.clone()));
        let (latest, slot, rows, folders, ctx) = (self.latest.clone(), self.slot.clone(), rows.clone(), folders.clone(), ctx.clone());
        crate::threads::spawn(crate::threads::Lane::Front, move || {
            if latest.load(Ordering::Relaxed) != id {
                return;
            }
            let t = Instant::now();
            let query = search::parse(&text);
            let matches = CONN.with(|c| {
                let mut c = c.borrow_mut();
                if c.is_none() {
                    *c = Db::read_only(&library::db_path()).ok();
                }
                let db = c.as_ref()?;
                let k = search::Context { db, rows: &rows, folder_paths: &folders };
                search::evaluate_with(&k, &query, &|| latest.load(Ordering::Relaxed) != id)
            });
            let Some(matches) = matches else { return };
            if latest.load(Ordering::Relaxed) != id {
                return;
            }
            let done = Done { text, query, matches: Some(matches), rows: Arc::as_ptr(&rows) as usize, ms: t.elapsed().as_secs_f64() * 1000.0 };
            *slot.lock().unwrap() = Some((id, done));
            ctx.request_repaint();
        });
        out
    }

    /// Rank of each row in the order "newest first" (see `LibraryView::apply_filter`), cached per rows.
    pub fn capture_rank(&mut self, rows: &Arc<Vec<Row>>) -> &[u32] {
        let id = (Arc::as_ptr(rows) as usize, rows.len());
        if self.capture_rank.as_ref().is_none_or(|(k, _)| *k != id) {
            let mut order: Vec<usize> = (0..rows.len()).collect();
            order.sort_by(|&a, &b| {
                let (za, zb) = (&rows[a], &rows[b]);
                za.capture.is_none().cmp(&zb.capture.is_none()).then_with(|| zb.capture.cmp(&za.capture)).then_with(|| za.name.cmp(&zb.name)).then_with(|| za.copy.is_some().cmp(&zb.copy.is_some()))
            });
            let mut rank = vec![0u32; rows.len()];
            for (r, i) in order.into_iter().enumerate() {
                rank[i] = r as u32;
            }
            self.capture_rank = Some((id, rank));
        }
        &self.capture_rank.as_ref().unwrap().1
    }

    /// Evaluate right here (test hooks, a database without file): the same result as the background would give.
    pub fn now(db: &Db, text: &str, rows: &Arc<Vec<Row>>, folders: &Arc<HashMap<i64, String>>) -> Done {
        let t = Instant::now();
        let query = search::parse(text);
        let k = search::Context { db, rows, folder_paths: folders };
        let matches = Some(search::evaluate(&k, &query));
        Done { text: text.to_string(), query, matches, rows: Arc::as_ptr(rows) as usize, ms: t.elapsed().as_secs_f64() * 1000.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wish waits for the pause, a newer wish makes the older one outdated, emptying withdraws all.
    #[test]
    fn wishes_and_outdated_results() {
        let mut j = SearchJob::default();
        assert!(!j.busy());
        j.wish("rom");
        assert!(j.busy() && j.outdated_since().is_some());
        let first = j.latest.load(Ordering::Relaxed);
        j.wish("rom");
        assert_eq!(j.latest.load(Ordering::Relaxed), first, "the same text again changes nothing");
        j.wish("roma");
        assert!(j.latest.load(Ordering::Relaxed) > first);
        // a result of an outdated evaluation is not taken
        let rows = Arc::new(Vec::new());
        let folders = Arc::new(HashMap::new());
        *j.slot.lock().unwrap() = Some((first, Done { text: "rom".into(), query: Default::default(), matches: None, rows: Arc::as_ptr(&rows) as usize, ms: 0.0 }));
        let ctx = egui::Context::default();
        assert!(j.poll(&ctx, &rows, &folders).is_none());
        j.cancel();
        assert!(!j.busy() && j.outdated_since().is_none());
    }
}
