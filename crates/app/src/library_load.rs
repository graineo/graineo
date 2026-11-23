//! Loading the library without waiting: everything `reload` needs (rows, folders, collections, keywords and what is
//! derived from them) is read in a background thread with its own read-only connection; the UI adopts the finished
//! result in the next frame (`poll_load`). Selection, active photo, scroll position and filters stay as they are.
//!
//! At most one load runs at a time. A request while one runs is remembered; its result is still taken if it is newer
//! than the one shown, then the next load starts. Results older than the shown one are dropped.
//! Jobs that change the library in the background (move, undo, virtual copy, delete, import) load right after their
//! write in the same thread ([`Ticket::load`]) and hand over the result: it is correct the moment the job is done.
//!
//! Ratings change the rows immediately and reach the database later (`crate::rating`). A load that may have read the
//! database before such a write keeps the values shown for these photos.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use library::db::{Collection, Db, Folder, Keyword, Root, Row};

use super::{Action, DateMonth, DateYear, LibraryView, VIEW_FOLDER};

/// Callback after the next load (main thread).
type Then = Box<dyn FnOnce(&mut LibraryView, &mut Vec<Action>) + Send>;

/// Result of a load: database contents plus what is derived from the rows.
pub struct Loaded {
    /// order of the loads (a newer one replaces an older one, never the other way round)
    seq: u64,
    /// rating changes up to this number were certainly in the database when the load began
    settled: u64,
    /// the quick collection may be taken over (no write of it was pending or sent meanwhile)
    quick_sent: Option<u64>,
    rows: Vec<Row>,
    /// preview key per row (file, key)
    keys: Vec<(i64, u64)>,
    folder_paths: HashMap<i64, String>,
    roots: Vec<Root>,
    folder: Vec<Folder>,
    collections: Vec<Collection>,
    keywords: Vec<Keyword>,
    last_run: Option<i64>,
    quick: (Option<i64>, HashSet<i64>),
    /// keyword assignments per image, if the column filters are in use
    facet_keywords: Option<HashMap<i64, Vec<i64>>>,
    stats: Stats,
    dates: (Vec<DateYear>, usize),
    ms: f64,
}

/// Counts for the filters: cameras, formats, years with months.
#[derive(Default)]
struct Stats {
    cameras: HashMap<String, usize>,
    formats: HashMap<String, usize>,
    years: BTreeMap<u16, [usize; 13]>,
}

impl Stats {
    fn add(&mut self, rows: &[Row]) {
        for z in rows {
            if let Some(k) = &z.camera {
                match self.cameras.get_mut(k) {
                    Some(n) => *n += 1,
                    None => {
                        self.cameras.insert(k.clone(), 1);
                    }
                }
            }
            match self.formats.get_mut(z.format.as_str()) {
                Some(n) => *n += 1,
                None => {
                    self.formats.insert(z.format.as_str().to_string(), 1);
                }
            }
            if let Some(j) = z.year() {
                let e = self.years.entry(j).or_insert([0; 13]);
                e[0] += 1;
                if let Some(m) = z.month().filter(|m| (1..=12).contains(m)) {
                    e[m as usize] += 1;
                }
            }
        }
    }

    /// Cameras and formats by frequency.
    fn finish(self) -> (Vec<(String, usize)>, Vec<(String, usize)>, BTreeMap<u16, [usize; 13]>) {
        let mut k: Vec<(String, usize)> = self.cameras.into_iter().collect();
        k.sort_by(|a, b| b.1.cmp(&a.1));
        let mut f: Vec<(String, usize)> = self.formats.into_iter().collect();
        f.sort_by(|a, b| b.1.cmp(&a.1));
        (k, f, self.years)
    }
}

/// "By date" from the capture times (`2026-09-19T13:03:00`), newest first; plus the number without a date.
pub(super) fn dates_of(rows: &[Row]) -> (Vec<DateYear>, usize) {
    let mut m: BTreeMap<&str, BTreeMap<&str, BTreeMap<&str, usize>>> = Default::default();
    let mut without = 0;
    for z in rows.iter().filter(|z| z.folder != VIEW_FOLDER) {
        match z.capture.as_deref().filter(|a| a.len() >= 10 && a.is_char_boundary(10)) {
            Some(a) => *m.entry(&a[..4]).or_default().entry(&a[..7]).or_default().entry(&a[..10]).or_default() += 1,
            None => without += 1,
        }
    }
    let tree = m
        .into_iter()
        .rev()
        .map(|(j, months)| {
            let months: Vec<DateMonth> = months.into_iter().rev().map(|(mo, days)| DateMonth { key: mo.to_string(), count: days.values().sum(), days: days.into_iter().rev().map(|(t, n)| (t.to_string(), n)).collect() }).collect();
            DateYear { key: j.to_string(), count: months.iter().map(|x| x.count).sum(), months }
        })
        .collect();
    (tree, without)
}

/// What a load needs to know from the UI when it is started (it may run in any thread).
#[derive(Clone)]
pub struct Ticket {
    counter: Arc<AtomicU64>,
    facets: bool,
    settled: u64,
    quick_sent: Option<u64>,
}

impl Ticket {
    /// Read everything (any connection; in a background thread).
    pub fn load(&self, db: &Db) -> Result<Loaded, String> {
        let seq = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let t = std::time::Instant::now();
        fn e(e: impl ToString) -> String {
            e.to_string()
        }
        let rows = db.rows().map_err(e)?;
        let folder_paths = db.folder_paths().map_err(e)?;
        let roots = db.roots().map_err(e)?;
        let folder = db.folder().map_err(e)?;
        let collections = db.collections().map_err(e)?;
        let keywords = db.keywords().map_err(e)?;
        let last_run = db.last_import();
        let quick_id = db.quick_id();
        let quick = (quick_id, quick_id.map(|id| db.images_of_collection(id).unwrap_or_default().into_iter().collect()).unwrap_or_default());
        let facet_keywords = self.facets.then(|| super::library_facets_job::read_assignments(db));
        let keys = rows.iter().map(|z| (z.file, super::row_key(&folder_paths, z))).collect();
        let mut stats = Stats::default();
        stats.add(&rows);
        let dates = dates_of(&rows);
        Ok(Loaded {
            seq,
            settled: self.settled,
            quick_sent: self.quick_sent,
            rows,
            keys,
            folder_paths,
            roots,
            folder,
            collections,
            keywords,
            last_run,
            quick,
            facet_keywords,
            stats,
            dates,
            ms: t.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

/// State of the background loads.
#[derive(Default)]
pub(super) struct Loader {
    counter: Arc<AtomicU64>,
    /// a load newer than this number is wanted (None: nothing wanted)
    wanted: Option<u64>,
    /// number of the load shown
    applied: u64,
    running: Option<Receiver<Result<Loaded, String>>>,
    /// callbacks waiting for a load newer than the number
    pub(super) then: Vec<(u64, Then)>,
    /// first load shown (start)
    pub(super) first_done: bool,
    /// local rating changes: running number, image → number of its last change, number up to which all are written
    rating_epoch: u64,
    rated: HashMap<i64, u64>,
    rating_settled: u64,
    /// quick collection: jobs sent so far (counted in `quick_send`)
    pub(super) quick_sent: u64,
    /// test runs: load in the foreground (results are needed right away)
    pub(super) foreground: bool,
}

impl Loader {
    /// Is a load wanted or running (the list will still change)?
    pub(super) fn busy(&self) -> bool {
        self.running.is_some() || self.wanted.is_some()
    }

    /// A rating of these images changed in the rows (the database follows).
    pub(super) fn rated(&mut self, images: impl Iterator<Item = i64>) {
        self.rating_epoch += 1;
        for b in images {
            self.rated.insert(b, self.rating_epoch);
        }
    }

    /// All rating writes so far are in the database.
    pub(super) fn ratings_written(&mut self) {
        self.rating_settled = self.rating_epoch;
    }
}

impl LibraryView {
    /// Starting point for a load in another thread (job): see [`Ticket::load`], then [`Self::adopt_load`].
    pub fn load_ticket(&self) -> Ticket {
        let pending = self.quick_pending.load(Ordering::SeqCst) > 0;
        Ticket { counter: self.loader.counter.clone(), facets: self.facets.is_some() || self.facets_job.is_some(), settled: self.loader.rating_settled, quick_sent: (!pending).then_some(self.loader.quick_sent) }
    }

    /// Re-read everything from the database (after import/sync, …) – in the background; the result is adopted in a later
    /// frame. Test runs load right away.
    pub(super) fn reload(&mut self) {
        if self.loader.foreground {
            self.reload_now();
            return;
        }
        if self.db.is_none() {
            return;
        }
        self.loader.wanted = Some(self.loader.counter.load(Ordering::SeqCst));
        self.start_load();
    }

    /// Re-read and then run `then` with the new rows (e.g. go to the photos just imported).
    pub(super) fn reload_then(&mut self, then: impl FnOnce(&mut LibraryView, &mut Vec<Action>) + Send + 'static) {
        self.reload();
        let w = self.loader.wanted.unwrap_or(0);
        self.loader.then.push((w, Box::new(then)));
    }

    /// Load in the foreground (test runs and test hooks that need the rows at once).
    pub(super) fn reload_now(&mut self) {
        let ticket = self.load_ticket();
        let Some(db) = &self.db else { return };
        match ticket.load(db) {
            Ok(l) => self.adopt_load(l),
            Err(e) => crate::log(&format!("library: loading failed: {e}")),
        }
    }

    /// Start a load in the background unless one is running.
    fn start_load(&mut self) {
        if self.loader.running.is_some() {
            return;
        }
        let Some(path) = self.db.as_ref().map(|d| d.path.clone()) else { return };
        let ticket = self.load_ticket();
        let ctx = self.quick_ctx.clone();
        let (tx, rx) = channel();
        let started = std::thread::Builder::new()
            .name("bibliothek-laden".into())
            .spawn(move || {
                // read-only: never waits for a write lock, never migrates
                let r = Db::read_only(&path).map_err(|e| e.to_string()).and_then(|db| ticket.load(&db));
                let _ = tx.send(r);
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            })
            .is_ok();
        if started {
            self.loader.running = Some(rx);
        } else {
            self.loader.wanted = None;
        }
    }

    /// Every frame: adopt a finished load, start the next one if still wanted, run callbacks that waited for it.
    pub(super) fn poll_load(&mut self, actions: &mut Vec<Action>) {
        self.series_poll();
        self.series_first_hint(actions);
        // picture styles of the RAWs for "Kamera-Looks": read ahead in idle time (only when the library changed)
        if !self.first_loading() {
            crate::style_scan::ahead(self.rows.len(), || self.learn_candidates());
        }
        if self.writer.all_written() {
            self.loader.ratings_written();
        }
        if let Some(rx) = &self.loader.running {
            match rx.try_recv() {
                Ok(r) => {
                    self.loader.running = None;
                    match r {
                        Ok(l) => {
                            let _t = crate::frame_watch::section("library: adopt load");
                            self.adopt_load(l);
                        }
                        Err(e) => {
                            crate::log(&format!("library: loading failed: {e}"));
                            self.loader.wanted = None;
                            // never stay in the loading state: the first load then goes through the UI connection
                            if !self.loader.first_done {
                                self.reload_now();
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.loader.running = None;
                    self.loader.wanted = None;
                    if !self.loader.first_done {
                        self.reload_now();
                    }
                }
            }
        }
        if self.loader.wanted.is_some() {
            self.start_load();
        }
        if !self.loader.then.is_empty() {
            let (applied, waiting) = (self.loader.applied, self.loader.wanted.is_some());
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.loader.then).into_iter().partition(|(w, _)| applied > *w || !waiting);
            self.loader.then = rest;
            for (_, f) in due {
                f(self, actions);
            }
        }
    }

    /// Take over a finished load (from the background or from a job). Older than the one shown: dropped.
    pub fn adopt_load(&mut self, l: Loaded) {
        if l.seq <= self.loader.applied {
            return;
        }
        let t = std::time::Instant::now();
        self.loader.applied = l.seq;
        if self.loader.wanted.is_some_and(|w| l.seq > w) {
            self.loader.wanted = None;
        }
        // ratings that may not have been in the database yet: keep what is shown
        let keep: Vec<(i64, u8, i8, u8)> = self
            .loader
            .rated
            .iter()
            .filter(|(_, e)| **e > l.settled)
            .filter_map(|(b, _)| self.index.get(b).map(|&i| &self.rows[i]).map(|z| (*b, z.stars, z.flag, z.label)))
            .collect();
        self.loader.rated.retain(|_, e| *e > l.settled);
        // the visible list as images: keeps the active photo's place (see `apply_filter`)
        let before = self.visible_images();
        self.meta_comparison = None;
        // freeing 70 000 old rows (several strings each) takes tens of milliseconds: not in the frame
        let old = std::mem::replace(&mut self.rows, Arc::new(l.rows));
        let old_index = std::mem::take(&mut self.index);
        let _ = std::thread::Builder::new().name("bibliothek-alt".into()).spawn(move || drop((old, old_index)));
        self.row_keys = l.keys;
        self.folder_paths = Arc::new(l.folder_paths);
        let mut stats = l.stats;
        if let Some((dir, z)) = &self.viewed {
            Arc::make_mut(&mut self.rows).extend(z.iter().cloned());
            Arc::make_mut(&mut self.folder_paths).insert(VIEW_FOLDER, format!("{}/", dir.display()));
            stats.add(z);
        }
        self.index = self.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        for (b, s, m, e) in keep {
            if let Some(&i) = self.index.get(&b) {
                let z = &mut Arc::make_mut(&mut self.rows)[i];
                (z.stars, z.flag, z.label) = (s, m, e);
            }
        }
        self.remap_visible(before);
        self.roots = l.roots;
        self.folder = l.folder;
        self.set_collections(l.collections);
        self.keywords = l.keywords;
        self.last_run = l.last_run;
        // quick collection: only if nothing of it was written in the meantime (the display is ahead then)
        if l.quick_sent == Some(self.loader.quick_sent) && self.quick_pending.load(Ordering::SeqCst) == 0 {
            (self.quick_id, self.quick) = l.quick;
        }
        self.build_tree_with(Some(l.dates));
        (self.cameras, self.formats, self.years) = stats.finish();
        // rebuild the column filters; the selection is kept (via the names)
        if let Some(g) = self.take_facets_state() {
            let source = match l.facet_keywords {
                Some(kw) => super::library_facets_job::Assignments::Known(kw),
                None => super::library_facets_job::Assignments::FromDb(self.db.as_ref().map(|d| d.path.clone()).unwrap_or_default()),
            };
            self.start_facets(g, source);
        }
        self.query_for = None;
        self.smart_counts.clear();
        self.source_changed();
        self.refilter = true;
        self.background_set = false;
        self.update_protection();
        let first = !self.loader.first_done;
        self.loader.first_done = true;
        // photos may have come, gone or changed their time: the series job looks at the changed days
        crate::series_job::poke();
        crate::log(&format!("library: {} images loaded in {:.0} ms, taken over in {:.0} ms", self.rows.len(), l.ms, t.elapsed().as_secs_f64() * 1000.0));
        if first {
            self.after_first_load();
        }
    }

    /// Images of the shown list, in grid order (indices that no longer fit `rows` are skipped).
    pub(super) fn visible_images(&self) -> Vec<i64> {
        self.visible.iter().filter_map(|&i| self.rows.get(i)).map(|z| z.image).collect()
    }

    /// Re-point the shown list after `rows` was replaced: INVARIANT `visible` only holds indices below `rows.len()`
    /// (and `sem_from <= visible.len()`) at every moment the grid may draw. `rows` and `index` must already be new.
    /// `before`: the shown images before the change; photos that are gone drop out, the filter runs again afterwards.
    pub(super) fn remap_visible(&mut self, before: Vec<i64>) {
        self.visible = before.iter().filter_map(|b| self.index.get(b).copied()).collect();
        if self.sem_from.is_some_and(|n| n > self.visible.len()) {
            self.sem_from = None;
        }
        self.visible_before = Some(before);
        self.refilter = true;
    }

    /// Safety net for the invariant of [`Self::remap_visible`]: a path that replaced `rows` without re-pointing
    /// `visible` must not bring the UI thread down (index out of bounds in the grid). One cheap pass per frame;
    /// in a healthy state nothing happens. A repair is logged (it names a missed path) and filters again.
    pub(super) fn heal_visible(&mut self) {
        let n = self.rows.len();
        if self.visible.iter().all(|&i| i < n) && self.sem_from.is_none_or(|k| k <= self.visible.len()) {
            return;
        }
        crate::log(&format!("library: shown list pointed past the rows ({} rows, {} shown): repaired", n, self.visible.len()));
        self.visible.retain(|&i| i < n);
        if self.sem_from.is_some_and(|k| k > self.visible.len()) {
            self.sem_from = None;
        }
        self.refilter = true;
    }

    /// Change `rows` in place (viewed folder swapped, closed, refreshed) and keep `index` and `visible` consistent in the
    /// same step, so that no frame between the change and the next filter run can index past the end.
    pub(super) fn edit_rows(&mut self, f: impl FnOnce(&mut Vec<Row>)) {
        let before = self.visible_images();
        f(Arc::make_mut(&mut self.rows));
        self.index = self.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        self.remap_visible(before);
    }

    /// Take the list a job loaded after its write; without one (load failed) re-read in the background.
    pub(super) fn adopt_or_reload(&mut self, loaded: Option<Loaded>) {
        match loaded {
            Some(l) => self.adopt_load(l),
            None => self.reload(),
        }
    }

    /// The first load is still running (start): the grid shows a quiet loading state.
    pub(super) fn first_loading(&self) -> bool {
        !self.loader.first_done && self.db.is_some()
    }

    /// Reloads still to come (the list is not final yet)?
    pub fn reloading(&self) -> bool {
        self.loader.busy()
    }
}
