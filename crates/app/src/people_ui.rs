//! People: one view in the main area of the library, built like the library itself – a name search, filter
//! columns (person, status, source, year) and a grid of faces in sections. A chosen name shows its confirmed faces as
//! one stack ("Anna · 23 bestätigt", opens with ↩ or a double click) and below it the suggestions for this name.
//! Unnamed faces come as stacks of similar faces.
//!
//! Commands are always the same buttons in the same place (Ja ↩, Nein ⌫, Anderer Name … N, ⌘Z); they act on the
//! selection. The photo of the one chosen face is the current photo of the app: ratings, the tool rail and Develop
//! (D, double click on the large photo) act on it, and Develop leads back here (`suspend` / `resume`). All faces live in memory ([`library::face_index`]); a click never waits for the
//! database: answers change the index at once and are written in the background, the face images come from the
//! preview cache. Nothing a person decides is changed again by a Lightroom import or the automatic recognition.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui;
use egui::Color32;
use library::apple_groups::Hint as AppleHint;
use library::db::Db;
use library::face_index::{self as fx, FaceIndex, Source, Status, ALL_SOURCES, ALL_STATUS};
use library::people::{self, Answer};

use crate::library_filter::{list_click, list_rows_labeled, ListRow};
use crate::lr;
use crate::people_model::{self as model, Built, Filter, Kind, SectionKind, Stack, Tile, NO_PERSON};

/// Face images the background workers make at the same time (on demand, for visible tiles)
const WORKERS: usize = 2;
/// Textures uploaded at most per frame (keeps scrolling smooth)
const TEXTURES_PER_FRAME: usize = 16;
/// Face textures kept at most (the least recently shown go first)
const TEXTURE_LIMIT: usize = 1500;
const TEXT_HEIGHT: f32 = 38.0;
const SECTION_H: f32 = 28.0;
/// Height of the filter columns (as in the library)
const COLUMN_H: f32 = 150.0;
/// Person column value of "(ohne Namen)" in list rows
const NONE_ROW: u32 = u32::MAX;

/// Where the view opens (the sidebar opens it where there is something to do).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolTab {
    Check,
    Suggestions,
    Untitled,
    People,
}

/// Commands from the menu (⌘Z, ⇧⌘Z, ⌘A, ⌘D) while the view is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    Undo,
    Redo,
    SelectAll,
    SelectNone,
}

/// Name field (N): text, just opened (focus), highlighted suggestion (0 = the typed text).
struct NameField {
    text: String,
    focus: bool,
    pick: usize,
}

/// A write job: steps, the faces it touches (snapshot for ⌘Z, read again afterwards), a person whose faces all
/// change (rename), whether the people list changes.
struct WriteJob {
    steps: Vec<Step>,
    faces: Vec<i64>,
    person: Option<i64>,
    people_changed: bool,
    undo: UndoMode,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UndoMode {
    /// a new change: the state before goes on the undo stack
    Record,
    /// restoring for ⌘Z: the state before goes on the redo stack
    Undo,
    /// restoring for ⇧⌘Z
    Redo,
    /// not undoable (releasing Apple groups removes the boxes graineo had added from Apple)
    Never,
}

/// Answer of the writer.
struct WriteDone {
    rows: Vec<(i64, Option<fx::Face>)>,
    people: Option<Vec<fx::Person>>,
    snapshot: Option<fx::Snapshot>,
    undo: UndoMode,
    fingerprint: String,
    error: Option<String>,
}

type Step = Box<dyn FnOnce(&Db) -> Result<(), String> + Send>;

thread_local! {
    /// Copy of the currently running step for a second attempt (stored by the step itself)
    static RETRIER: RefCell<Option<Step>> = const { RefCell::new(None) };
    static Z: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Step that can repeat itself on a lock (the data is copyable).
fn step<T: Clone + Send + 'static>(data: T, f: fn(&Db, T) -> Result<(), String>) -> Step {
    Box::new(move |db: &Db| {
        let copy = data.clone();
        RETRIER.with(|w| *w.borrow_mut() = Some(step(copy, f)));
        let r = f(db, data);
        if r.is_ok() {
            RETRIER.with(|w| *w.borrow_mut() = None);
        }
        r
    })
}

fn is_busy(e: &str) -> bool {
    let e = e.to_lowercase();
    e.contains("locked") || e.contains("busy")
}

/// Run a step; on "busy" return the step (rebuilt via the retrier).
fn run_with_retry(db: &Db, f: Step) -> Result<(), (String, Option<Step>)> {
    RETRIER.with(|w| match f(db) {
        Ok(()) => Ok(()),
        Err(e) if is_busy(&e) => Err((e, w.borrow_mut().take())),
        Err(e) => Err((e, None)),
    })
}

/// Background writer: runs jobs in order; if the library is busy with another task it waits and retries (up to two
/// minutes per step). Before a change it notes the faces (⌘Z), afterwards it reads them again for the index.
/// Never silent: if the library cannot be opened or a job panics, every job is answered with an error (the view shows
/// "Nicht gespeichert" instead of a name that is only in the view).
fn writer(path: PathBuf, rx: Receiver<(WriteJob, Option<fx::Snapshot>)>, tx: Sender<WriteDone>, ctx: egui::Context) {
    let failed = |undo: UndoMode, e: String| WriteDone { rows: Vec::new(), people: None, snapshot: None, undo, fingerprint: String::new(), error: Some(e) };
    let db = match Db::open(&path) {
        Ok(db) => db,
        Err(e) => {
            crate::log(&format!("people: library not opened for writing: {e}"));
            for (job, _) in rx {
                let _ = tx.send(failed(job.undo, e.to_string()));
                ctx.request_repaint();
            }
            return;
        }
    };
    for (job, restore) in rx {
        let undo = job.undo;
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_job(&db, job, restore))).unwrap_or_else(|_| failed(undo, crate::tr("interner Fehler beim Speichern").to_string()));
        let _ = tx.send(done);
        ctx.request_repaint();
    }
}

/// One job of the [`writer`].
fn write_job(db: &Db, job: WriteJob, restore: Option<fx::Snapshot>) -> WriteDone {
    {
        let t0 = std::time::Instant::now();
        let mut error = None;
        let before = fx::snapshot(&db, &job.faces, job.person).ok();
        let t_snap = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(s) = &restore {
            if let Err(e) = fx::restore(&db, s) {
                error = Some(e);
            }
        }
        for st in job.steps {
            let mut st = Some(st);
            let start = std::time::Instant::now();
            loop {
                let f = st.take().unwrap();
                match run_with_retry(&db, f) {
                    Ok(()) => break,
                    Err((_, Some(again))) if start.elapsed().as_secs() < 120 => {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        st = Some(again);
                    }
                    Err((e, _)) => {
                        crate::log(&format!("people: write failed: {e}"));
                        error.get_or_insert(e);
                        break;
                    }
                }
            }
        }
        let mut ids: Vec<i64> = job.faces.clone();
        if let Some(s) = before.as_ref() {
            ids.extend(s.face_ids.iter().copied());
        }
        if let Some(s) = restore.as_ref() {
            ids.extend(s.face_ids.iter().copied());
        }
        ids.sort_unstable();
        ids.dedup();
        let t_steps = t0.elapsed().as_secs_f64() * 1000.0;
        let rows = fx::reload_faces(&db.conn, &ids);
        let people = (job.people_changed || restore.is_some()).then(|| fx::reload_people(&db.conn));
        let fingerprint = fx::fingerprint(&db.conn);
        if measuring() {
            crate::log(&format!("people-measure: write {} faces: snapshot {t_snap:.0} ms, steps until {t_steps:.0} ms, total {:.0} ms", ids.len(), t0.elapsed().as_secs_f64() * 1000.0));
        }
        WriteDone { rows, people, snapshot: before, undo: job.undo, fingerprint, error }
    }
}

struct State {
    is_open: bool,
    db_path: PathBuf,
    index: Option<FaceIndex>,
    from_loader: Option<Receiver<Result<(FaceIndex, String), String>>>,
    built: Built,
    dirty: bool,
    filter: Filter,
    stack: Option<Stack>,
    /// search field (names) and the highlighted hit of its list
    search: String,
    search_pick: usize,
    search_focus: bool,
    anchors: [Option<u32>; 4],
    selection: HashSet<usize>,
    anchor: Option<usize>,
    to_start: bool,
    /// bring this tile into view once (after arrow keys)
    scroll_to: Option<usize>,
    size: f32,
    name_field: Option<NameField>,
    /// rename onto an existing name: (person, existing name, faces)
    merge_question: Option<(i64, String, usize)>,
    message: String,
    textures: HashMap<i64, (egui::TextureHandle, u64)>,
    texture_clock: u64,
    requested: HashSet<i64>,
    jobs_queue: Option<Arc<(Mutex<Vec<i64>>, Condvar)>>,
    from_worker: Option<Receiver<(i64, Option<egui::ColorImage>)>>,
    decoded: Vec<(i64, egui::ColorImage)>,
    prefill_cancel: Arc<AtomicBool>,
    writer: Option<Sender<(WriteJob, Option<fx::Snapshot>)>>,
    from_writer: Option<Receiver<WriteDone>>,
    pending_writes: usize,
    undo: Vec<fx::Snapshot>,
    redo: Vec<fx::Snapshot>,
    /// names changed → the library re-reads its photos when the view is left
    changed: bool,
    /// fingerprint of the suggestions when loaded / after the own writes; a different one means the background run
    /// computed them anew (the view does not take them over unasked)
    fingerprint: String,
    newer: Arc<AtomicBool>,
    checking: Arc<AtomicBool>,
    last_check: f64,
    /// Apple groups that seem to be one of our people (background, read-only) and the ones set aside
    hints: Option<Vec<AppleHint>>,
    hints_in: Arc<Mutex<Option<Vec<AppleHint>>>>,
    hints_busy: Arc<AtomicBool>,
    hints_dismissed: HashSet<(i64, i64)>,
    requests: Vec<Request>,
    columns: usize,
    /// photo of the chosen face (large view, navigator) and its loading
    photo: Option<Photo>,
    photo_rx: Option<Receiver<PhotoLoaded>>,
    photo_loading: Option<i64>,
    /// the chosen face's photo large instead of the grid (double click, space; Esc and double click go back)
    big: bool,
    /// scroll position of the grid (last drawn) and the one to restore on the next draw
    grid_y: f32,
    restore_y: Option<f32>,
    /// left for "Foto öffnen": the view comes back as it was on the return to the grid ([`resume`])
    resume_pending: bool,
    /// after reading the faces again: choose this face's tile again
    keep_face: Option<i64>,
    /// face boxes on the large photo (setting `peopleFaceFrames`, on by default); off: the chosen face is marked
    /// shortly after choosing it ([`FLASH`])
    frames: bool,
    /// the chosen face and when it was chosen (for the short mark with the boxes off)
    flash: Option<(i64, f64)>,
    /// the large photo was shown when the view was left for Develop: it comes back so
    resume_big: bool,
}

/// Settings key of the face boxes on the large photo.
const FRAMES_KEY: &str = "peopleFaceFrames";
/// With the boxes off: the chosen face stays marked this long (s), then fades out within [`FADE`].
const FLASH: f64 = 0.8;
const FADE: f64 = 0.4;

/// The photo of the chosen face, in the frame of its boxes.
struct Photo {
    image: i64,
    texture: Option<egui::TextureHandle>,
    /// the boxes fit the image (otherwise it is shown without boxes)
    fits: bool,
    faces: Vec<people::FaceInfo>,
    error: Option<String>,
}

/// (image, picture + boxes fit + faces of the photo, or an error)
type PhotoLoaded = (i64, Result<(egui::ColorImage, bool, Vec<people::FaceInfo>), String>);

/// Longest edge of the photo in the large view (also the navigator's source).
const PHOTO_EDGE: u32 = 2048;

static LAST_ACTION: Mutex<Option<std::time::Instant>> = Mutex::new(None);

fn touch() {
    *LAST_ACTION.lock().unwrap() = Some(std::time::Instant::now());
}

/// Was something done in the view within the last `secs` seconds? (The follow-up computation of the suggestions
/// waits then, so they do not change under the person's hands.)
pub fn active_within(secs: u64) -> bool {
    is_open() && LAST_ACTION.lock().unwrap().is_some_and(|t| t.elapsed().as_secs() < secs)
}

fn preset(f: &mut Filter, tab: Option<ToolTab>) {
    *f = Filter::default();
    match tab {
        Some(ToolTab::Check) => {
            f.status.insert(Status::Doubtful);
        }
        Some(ToolTab::Suggestions) => {
            f.status.extend([Status::SuggestSure, Status::SuggestMaybe]);
        }
        Some(ToolTab::Untitled) => {
            f.status.insert(Status::UnnamedGroup);
        }
        Some(ToolTab::People) => {
            f.status.insert(Status::Confirmed);
        }
        None => {}
    }
}

fn new_state(tab: Option<ToolTab>) -> State {
    let mut filter = Filter::default();
    preset(&mut filter, tab);
    State {
        is_open: true,
        db_path: library::db_path(),
        index: None,
        from_loader: None,
        built: Built::default(),
        dirty: true,
        filter,
        stack: None,
        search: String::new(),
        search_pick: 0,
        search_focus: false,
        anchors: [None; 4],
        selection: HashSet::new(),
        anchor: None,
        to_start: true,
        scroll_to: None,
        size: 150.0,
        name_field: None,
        merge_question: None,
        message: String::new(),
        textures: HashMap::new(),
        texture_clock: 0,
        requested: HashSet::new(),
        jobs_queue: None,
        from_worker: None,
        decoded: Vec::new(),
        prefill_cancel: Arc::new(AtomicBool::new(false)),
        writer: None,
        from_writer: None,
        pending_writes: 0,
        undo: Vec::new(),
        redo: Vec::new(),
        changed: false,
        fingerprint: String::new(),
        newer: Arc::default(),
        checking: Arc::default(),
        last_check: 0.0,
        hints: None,
        hints_in: Arc::default(),
        hints_busy: Arc::default(),
        hints_dismissed: HashSet::new(),
        requests: Vec::new(),
        columns: 1,
        photo: None,
        photo_rx: None,
        photo_loading: None,
        big: false,
        grid_y: 0.0,
        restore_y: None,
        resume_pending: false,
        keep_face: None,
        frames: library::settings::value(FRAMES_KEY).as_bool().unwrap_or(true),
        flash: None,
        resume_big: false,
    }
}

/// Open the view (standard: people, suggestions, doubts and groups).
pub fn open() {
    open_with(None);
}

/// Open the view with the status column preset for this kind of work.
pub fn open_at(tab: ToolTab) {
    open_with(Some(tab));
}

fn open_with(tab: Option<ToolTab>) {
    *LAST_ACTION.lock().unwrap() = None;
    Z.with(|z| {
        let mut g = z.borrow_mut();
        match g.as_mut() {
            Some(s) => {
                if !s.is_open {
                    // reopened: read the faces again (other parts of the app may have changed names)
                    s.index = None;
                    s.from_loader = None;
                    s.prefill_cancel = Arc::new(AtomicBool::new(false));
                }
                preset(&mut s.filter, tab);
                s.search.clear();
                s.stack = None;
                s.selection.clear();
                s.to_start = true;
                s.dirty = true;
                s.is_open = true;
                s.big = false;
                s.resume_pending = false;
                s.keep_face = None;
                s.restore_y = None;
            }
            None => *g = Some(new_state(tab)),
        }
    });
}

/// Close the view (another source was chosen).
pub fn close() {
    Z.with(|z| {
        if let Some(s) = z.borrow_mut().as_mut() {
            close_inner(s);
            s.resume_pending = false;
        }
    });
}

/// Develop (D, the button, the module bar, the tool rail) left the view: on the return to the grid ([`resume`]) it comes back as it was – the same
/// person, filters, selection and scroll position. Choosing another source in between forgets it ([`close`]).
pub fn suspend() {
    Z.with(|z| {
        if let Some(s) = z.borrow_mut().as_mut() {
            if s.is_open {
                s.resume_big = s.big;
                close_inner(s);
            }
            s.resume_pending = true;
        }
    });
}

/// Left for Develop and not yet back ([`suspend`]): the way back leads here.
pub fn resume_pending() -> bool {
    Z.with(|z| z.borrow().as_ref().is_some_and(|s| s.resume_pending && !s.is_open))
}

/// Back to the grid after [`suspend`]: open the view again as it was. Returns whether it did.
pub fn resume() -> bool {
    Z.with(|z| {
        let mut g = z.borrow_mut();
        let Some(s) = g.as_mut().filter(|s| s.resume_pending && !s.is_open) else { return false };
        s.resume_pending = false;
        s.is_open = true;
        *LAST_ACTION.lock().unwrap() = None;
        // names may have changed in Develop: read the faces again, then choose the same face and scroll back
        s.keep_face = chosen_face(s);
        s.index = None;
        s.from_loader = None;
        s.prefill_cancel = Arc::new(AtomicBool::new(false));
        s.restore_y = Some(s.grid_y);
        s.to_start = false;
        s.big = std::mem::take(&mut s.resume_big) && s.keep_face.is_some();
        s.dirty = true;
        true
    })
}

/// The one chosen face (exactly one face tile selected).
fn chosen_face(z: &State) -> Option<i64> {
    if z.selection.len() != 1 {
        return None;
    }
    let i = *z.selection.iter().next()?;
    match z.built.tiles.get(i)?.kind {
        Kind::Face(id) => Some(id),
        _ => None,
    }
}

/// Photo of the chosen face while the view is open (the Info panel shows it).
pub fn focus_image() -> Option<i64> {
    Z.with(|z| {
        let g = z.borrow();
        let s = g.as_ref().filter(|s| s.is_open)?;
        let id = chosen_face(s)?;
        s.index.as_ref()?.face(id).map(|f| f.image)
    })
}

fn close_inner(z: &mut State) {
    z.is_open = false;
    z.big = false;
    z.photo = None;
    z.name_field = None;
    z.prefill_cancel.store(true, Ordering::Relaxed);
    // textures are made again on the next opening
    z.textures.clear();
    z.requested.clear();
    z.decoded.clear();
}

pub fn is_open() -> bool {
    Z.with(|z| z.borrow().as_ref().is_some_and(|s| s.is_open))
}

/// A menu command for the view (⌘Z, ⇧⌘Z, ⌘A, ⌘D).
pub fn request(r: Request) {
    Z.with(|z| {
        if let Some(s) = z.borrow_mut().as_mut().filter(|s| s.is_open) {
            s.requests.push(r);
        }
    });
}

/// What the library should do after drawing.
#[derive(Default)]
pub struct Outcome {
    /// Names/keywords changed → re-read the library (the library does it when the view is left)
    pub changed: bool,
    /// something was written → refresh the counts in the sidebar (cheap, in the background)
    pub counts_changed: bool,
    /// "Fotos zeigen": set the search text (the view is then closed)
    pub search: Option<String>,
    /// open this photo
    pub open: Option<i64>,
}

/// Load the face index in a thread of its own (read-only connection).
fn start_load(z: &mut State, ctx: &egui::Context) {
    let (tx, rx) = channel();
    z.from_loader = Some(rx);
    let (path, c) = (z.db_path.clone(), ctx.clone());
    // normal priority: the person waits for it
    let _ = std::thread::Builder::new().name("people-index".into()).spawn(move || {
        let t = std::time::Instant::now();
        let r = Db::read_only(&path).or_else(|_| Db::open(&path)).map_err(|e| e.to_string()).and_then(|db| fx::load(&db.conn).map(|ix| (ix, fx::fingerprint(&db.conn))));
        if measuring() {
            crate::log(&format!("people-measure: face index loaded in {:.0} ms ({} faces)", t.elapsed().as_secs_f64() * 1000.0, r.as_ref().map_or(0, |x| x.0.faces.len())));
        }
        let _ = tx.send(r);
        c.request_repaint();
    });
}

/// Face images ready before they are shown: open questions and groups (one quiet thread, stops when the view closes).
fn start_prefill(z: &State) {
    let (path, cancel) = (z.db_path.clone(), z.prefill_cancel.clone());
    let _ = std::thread::Builder::new().name("people-prefill".into()).spawn(move || {
        library::preview::low_priority_thread();
        let Ok(db) = Db::open(&path) else { return };
        // once: face images that may show a wrong place go (made again when shown)
        let purged = fx::purge_outdated(&cancel) + fx::tidy(&db.conn, &cancel);
        if purged > 0 {
            crate::log(&format!("people: {purged} outdated face images removed"));
        }
        let ids: Vec<i64> = fx::missing_thumbs(&db.conn);
        let t = std::time::Instant::now();
        let (made, cached) = fx::prefill(&db, &ids, &cancel);
        if made > 0 {
            crate::log(&format!("people: {made} face images made ({cached} from the preview cache) in {:.1} s", t.elapsed().as_secs_f64()));
        }
    });
}

/// Background worker: makes face images (preview cache, else original) and decodes them, the visible ones first.
fn worker(path: PathBuf, jobs_queue: Arc<(Mutex<Vec<i64>>, Condvar)>, tx: Sender<(i64, Option<egui::ColorImage>)>, ctx: egui::Context) {
    library::people::low_priority();
    let Ok(db) = Db::open(&path) else { return };
    loop {
        let id = {
            let (m, cv) = &*jobs_queue;
            let mut list = m.lock().unwrap();
            while list.is_empty() {
                list = cv.wait(list).unwrap();
            }
            list.pop().unwrap()
        };
        let _ = fx::ensure_thumb(&db, id);
        let img = fx::thumb_path(&db.conn, id).and_then(|p| image::open(p).ok()).map(|img| {
            let rgb = img.to_rgb8();
            egui::ColorImage::from_rgb([rgb.width() as usize, rgb.height() as usize], rgb.as_raw())
        });
        if tx.send((id, img)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// Upload decoded images and ask for the visible ones (the queue only keeps what is visible).
fn fetch_images(z: &mut State, ctx: &egui::Context, visible: &[i64]) {
    let mut loaded = 0;
    while loaded < TEXTURES_PER_FRAME && !z.decoded.is_empty() {
        let (id, ci) = z.decoded.remove(0);
        z.texture_clock += 1;
        z.textures.insert(id, (ctx.load_texture(format!("gesicht-{id}"), ci, egui::TextureOptions::LINEAR), z.texture_clock));
        loaded += 1;
    }
    if !z.decoded.is_empty() {
        ctx.request_repaint();
    }
    // keep the number of textures bounded: the least recently shown go
    if z.textures.len() > TEXTURE_LIMIT {
        let mut v: Vec<(i64, u64)> = z.textures.iter().map(|(id, (_, t))| (*id, *t)).collect();
        v.sort_by_key(|x| x.1);
        let n = z.textures.len() - TEXTURE_LIMIT * 9 / 10;
        for (id, _) in v.into_iter().take(n) {
            z.textures.remove(&id);
            z.requested.remove(&id);
        }
    }
    z.texture_clock += 1;
    let mut new_ones = Vec::new();
    for &id in visible {
        if let Some(t) = z.textures.get_mut(&id) {
            t.1 = z.texture_clock;
        } else if z.requested.insert(id) {
            new_ones.push(id);
        }
    }
    let Some(a) = &z.jobs_queue else { return };
    let (m, cv) = &**a;
    let mut q = m.lock().unwrap();
    let seen: HashSet<i64> = visible.iter().copied().collect();
    let mut dropped = Vec::new();
    q.retain(|id| {
        let keep = seen.contains(id);
        if !keep {
            dropped.push(*id);
        }
        keep
    });
    for id in dropped {
        z.requested.remove(&id);
    }
    if !new_ones.is_empty() {
        // in reverse order so that the topmost tiles come first
        q.extend(new_ones.into_iter().rev());
        cv.notify_all();
    }
}

/// Text in one line, overlong text ends with "…".
fn single_line(text: String, font: egui::FontId, color: Color32, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Confirmed => crate::tr("Bestätigt"),
        Status::SuggestSure => crate::tr("Vorschlag · sehr wahrscheinlich"),
        Status::SuggestMaybe => crate::tr("Vorschlag · möglich"),
        Status::Doubtful => crate::tr("Name zweifelhaft"),
        Status::UnnamedGroup => crate::tr("Ohne Namen · Gruppen"),
        Status::Unnamed => crate::tr("Ohne Namen · einzeln"),
        Status::Rejected => crate::tr("Nicht benennen"),
    }
}

fn source_name(s: Source) -> &'static str {
    match s {
        Source::Apple => crate::tr("Apple Fotos"),
        Source::AppleGroup => crate::tr("Apple-Gruppe"),
        Source::Lightroom => "Lightroom",
        Source::Own => crate::tr("Eigene Erkennung"),
        Source::Hand => crate::tr("Von Hand"),
    }
}

/// Title, second line and "is a question" of a tile (made only for visible tiles).
fn tile_text(ix: &FaceIndex, t: &Tile) -> (String, String, bool) {
    match t.kind {
        Kind::Person(p) => (ix.name(p).to_string(), crate::trf!("{n} bestätigt", n = crate::library_ui::thousands(t.count)), false),
        Kind::Group(_) => (crate::trf!("{n} Gesichter", n = crate::library_ui::thousands(t.count)), crate::tr("ohne Namen").to_string(), false),
        Kind::Face(id) => {
            let Some(f) = ix.face(id) else { return (String::new(), String::new(), false) };
            let name = |p: Option<i64>| p.map_or("?", |p| ix.name(p)).to_string();
            match f.status {
                Status::SuggestSure => (format!("{}?", name(f.person)), crate::tr("sehr wahrscheinlich").to_string(), true),
                Status::SuggestMaybe => (format!("{}?", name(f.person)), crate::tr("möglich").to_string(), true),
                Status::Doubtful => (format!("{}?", name(f.suspect)), crate::trf!("eingetragen: {a}", a = name(f.person)), true),
                Status::Confirmed => (name(f.person), f.file_name.clone(), false),
                Status::Rejected => (crate::tr("Nicht benennen").to_string(), f.file_name.clone(), false),
                _ => (crate::tr("Ohne Namen").to_string(), f.file_name.clone(), false),
            }
        }
    }
}

fn section_title(ix: &FaceIndex, filter: &Filter, stack: Option<Stack>, kind: SectionKind) -> String {
    let one = filter.one_person().map(|p| ix.name(p).to_string());
    match (kind, one) {
        (SectionKind::People, Some(n)) => n,
        (SectionKind::People, None) => crate::tr("Personen").into(),
        (SectionKind::Questions, Some(n)) => crate::trf!("Vorschläge für {a}", a = n),
        (SectionKind::Questions, None) => crate::tr("Vorschläge").into(),
        (SectionKind::Doubtful, Some(n)) => crate::trf!("Name zweifelhaft – {a}?", a = n),
        (SectionKind::Doubtful, None) => crate::tr("Name zweifelhaft").into(),
        (SectionKind::Groups, _) => crate::tr("Ohne Namen · Gruppen").into(),
        (SectionKind::Unnamed, _) => crate::tr("Ohne Namen · einzeln").into(),
        (SectionKind::Rejected, _) => crate::tr("Nicht benennen").into(),
        (SectionKind::StackFaces, _) => match stack {
            Some(Stack::Person(p)) => crate::trf!("{a} · bestätigt", a = ix.name(p)),
            _ => crate::tr("Gruppe ohne Namen").into(),
        },
    }
}

/// What the person decided in this frame.
enum Command {
    Yes,
    No,
    Name(String),
    NoName,
    Open(usize),
    Back,
    OpenPhoto(usize),
    /// show this face tile's photo large in the view
    Big(usize),
    /// back from the large photo to the grid (same place)
    CloseBig,
    /// face boxes on the large photo on/off (stays in the settings)
    ToggleFrames,
    /// choose this tile (a click on a face in the large photo)
    Choose(usize),
    ShowPhotos(i64),
    Hide(i64, bool),
    ConfirmSure,
    AcceptGroup(AppleHint),
    DismissGroup(i64, i64),
    /// release the assignment from Apple groups: of a person (all) or of the groups of these faces
    ReleaseApple(Option<i64>, Vec<i64>),
    Undo,
    Redo,
    Refresh,
}

fn measuring() -> bool {
    crate::env::var_os("GRAINEO_PEOPLE_CYCLE").is_some() && crate::env::var_os("GRAINEO_DATA_DIR").is_some()
}

/// Draw the view in the centre of the library.
pub fn view(ui: &mut egui::Ui, db: Option<&Db>) -> Outcome {
    let mut result = Outcome::default();
    let Some(db) = db else { return result };
    let ctx = ui.ctx().clone();
    Z.with(|zz| {
        let mut guard = zz.borrow_mut();
        let Some(z) = guard.as_mut().filter(|z| z.is_open) else { return };
        z.db_path = db.path.clone();
        let _t = crate::frame_watch::section("people: view");
        receive(z, &ctx, &mut result);
        refresh_hints(z, &ctx);
        check_newer(z, &ctx);
        let mut command: Option<Command> = None;
        if measuring() {
            command = measuring_cycle(z, &ctx);
        }
        for r in std::mem::take(&mut z.requests) {
            match r {
                Request::Undo => command = Some(Command::Undo),
                Request::Redo => command = Some(Command::Redo),
                Request::SelectAll => z.selection = (0..z.built.tiles.len()).collect(),
                Request::SelectNone => z.selection.clear(),
            }
        }
        rebuild(z);
        if let Some(c) = test_hook(z) {
            command = Some(c);
        }

        // ── Row 1: name search, view (grid / large photo), size
        if let Some(c) = search_row(ui, z, &ctx) {
            command = Some(c);
        }
        ui.add_space(6.0);
        // ── Row 2: filter columns
        if columns_row(ui, z) {
            z.dirty = true;
            z.stack = None;
            z.selection.clear();
            z.to_start = true;
            z.big = false;
            rebuild(z);
        }
        ui.add_space(6.0);
        // ── Row 3: commands (always the same buttons in the same place)
        if let Some(c) = command_bar(ui, z) {
            command = Some(c);
        }
        // name field and merge question float below the command bar: the grid never moves
        let anchor = ui.cursor().min;
        if let Some(c) = name_field(z, &ctx, anchor) {
            command = Some(c);
        }
        merge_question(z, &ctx, anchor);
        // ── Row 4: fixed line: back from a stack, the person's commands, messages
        if let Some(c) = context_row(ui, z) {
            command = Some(c);
        }
        ui.add_space(4.0);
        // ── keys (not while typing)
        if command.is_none() {
            command = keys(z, &ctx);
        }
        // ── grid, or the chosen face's photo large in its place
        update_photo(z, &ctx);
        if z.big {
            if let Some(c) = big_view(ui, z) {
                command = Some(c);
            }
        } else if let Some(c) = grid(ui, z, &ctx) {
            command = Some(c);
        }
        if let Some(c) = command {
            touch();
            execute(z, &ctx, c, &mut result);
        }
        // no face chosen any more (the last one answered, filters changed): back to the grid
        if z.big && z.index.is_some() && !z.dirty && z.keep_face.is_none() && chosen_face(z).is_none() {
            close_big(z);
        }
        // the moment a face was chosen (with the boxes off it is marked shortly)
        let chosen = chosen_face(z);
        if z.flash.map(|f| f.0) != chosen {
            z.flash = chosen.map(|c| (c, ctx.input(|i| i.time)));
        }
        update_photo(z, &ctx);
        result.changed = std::mem::take(&mut z.changed);
        if result.search.is_some() {
            close_inner(z);
        }
    });
    result
}

/// Filter, count and arrange again if something changed (in memory, a few ms).
fn rebuild(z: &mut State) {
    if !z.dirty {
        return;
    }
    let Some(ix) = z.index.as_ref() else { return };
    let t = std::time::Instant::now();
    z.built = model::build(ix, &z.filter, z.stack);
    if measuring() {
        crate::log(&format!("people-measure: view rebuilt in {:.1} ms ({} tiles)", t.elapsed().as_secs_f64() * 1000.0, z.built.tiles.len()));
    }
    // an opened stack that became empty closes
    if z.stack.is_some() && z.built.tiles.is_empty() {
        z.stack = None;
        z.built = model::build(ix, &z.filter, None);
    }
    z.selection.retain(|&i| i < z.built.tiles.len());
    // back from Develop: the same face chosen again (its place may have moved when names changed there)
    if let Some(f) = z.keep_face.take() {
        // the face only inside its person's stack (names changed meanwhile): open that stack
        if !z.built.tiles.iter().any(|t| t.kind == Kind::Face(f)) && z.stack.is_none() {
            if let Some(p) = ix.face(f).filter(|x| x.status == Status::Confirmed).and_then(|x| x.person) {
                z.stack = Some(Stack::Person(p));
                z.built = model::build(ix, &z.filter, z.stack);
            }
        }
        if let Some(i) = z.built.tiles.iter().position(|t| t.kind == Kind::Face(f)) {
            z.selection = HashSet::from([i]);
            z.anchor = Some(i);
            z.scroll_to = Some(i);
        }
    }
    z.dirty = false;
}

/// Take over what the threads delivered: the index, decoded images, finished writes.
fn receive(z: &mut State, ctx: &egui::Context, result: &mut Outcome) {
    if z.index.is_none() && z.from_loader.is_none() && z.pending_writes == 0 {
        start_load(z, ctx);
    }
    if let Some(rx) = &z.from_loader {
        if let Ok(r) = rx.try_recv() {
            z.from_loader = None;
            match r {
                Ok((ix, fp)) => {
                    // a face number now on another photo (removed face, number given again): its image goes
                    if let Some(old) = &z.index {
                        let moved: Vec<i64> = z.textures.keys().copied().filter(|id| old.face(*id).map(|f| f.image) != ix.face(*id).map(|f| f.image)).collect();
                        for id in moved {
                            z.textures.remove(&id);
                            z.requested.remove(&id);
                        }
                    }
                    z.index = Some(ix);
                    z.fingerprint = fp;
                    z.newer.store(false, Ordering::Relaxed);
                    z.dirty = true;
                    start_prefill(z);
                }
                Err(e) => z.message = crate::trf!("Gesichter nicht lesbar: {e}", e = e),
            }
        }
    }
    if z.jobs_queue.is_none() {
        let a = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let (tx, rx) = channel();
        for _ in 0..WORKERS {
            let (p, a2, tx2, c2) = (z.db_path.clone(), a.clone(), tx.clone(), ctx.clone());
            let _ = std::thread::Builder::new().name("people-faces".into()).spawn(move || worker(p, a2, tx2, c2));
        }
        z.jobs_queue = Some(a);
        z.from_worker = Some(rx);
    }
    if let Some(rx) = &z.from_worker {
        while let Ok((id, img)) = rx.try_recv() {
            if let Some(ci) = img {
                z.decoded.push((id, ci));
            }
        }
    }
    let mut done = Vec::new();
    if let Some(rx) = &z.from_writer {
        while let Ok(d) = rx.try_recv() {
            done.push(d);
        }
    }
    for d in done {
        z.pending_writes = z.pending_writes.saturating_sub(1);
        result.counts_changed = true;
        z.changed = true;
        if let Some(ix) = z.index.as_mut() {
            ix.apply(d.rows);
            if let Some(p) = d.people {
                ix.people = p;
            }
        }
        z.dirty = true;
        if z.pending_writes == 0 {
            // own writes are not "newer" suggestions
            z.fingerprint = d.fingerprint;
        }
        match (d.error, d.snapshot) {
            (Some(e), _) => z.message = crate::trf!("Nicht gespeichert: {e}", e = e),
            (None, Some(s)) => match d.undo {
                UndoMode::Record => {
                    z.undo.push(s);
                    z.redo.clear();
                }
                UndoMode::Undo => z.redo.push(s),
                UndoMode::Redo => z.undo.push(s),
                UndoMode::Never => {}
            },
            _ => {}
        }
        if z.undo.len() > 100 {
            z.undo.remove(0);
        }
    }
}

/// Every 15 s (in a thread): did the background run compute the suggestions anew? Then the view offers to refresh
/// instead of changing under the person's hands.
fn check_newer(z: &mut State, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    if z.index.is_none() || z.pending_writes > 0 || now - z.last_check < 15.0 {
        return;
    }
    if z.checking.swap(true, Ordering::SeqCst) {
        return;
    }
    z.last_check = now;
    ctx.request_repaint_after(std::time::Duration::from_secs(16));
    let (path, base, newer, busy) = (z.db_path.clone(), z.fingerprint.clone(), z.newer.clone(), z.checking.clone());
    let c = ctx.clone();
    let spawned = std::thread::Builder::new().name("people-check".into()).spawn(move || {
        library::preview::low_priority_thread();
        if let Ok(db) = Db::read_only(&path) {
            if !base.is_empty() && fx::fingerprint(&db.conn) != base {
                newer.store(true, Ordering::Relaxed);
                c.request_repaint();
            }
        }
        busy.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        z.checking.store(false, Ordering::SeqCst);
    }
}

/// Row 1: name search with suggestions (alphabetically, ↑ ↓ ↩); on the right the view (grid or the chosen face's
/// photo large) and the tile size.
fn search_row(ui: &mut egui::Ui, z: &mut State, ctx: &egui::Context) -> Option<Command> {
    let mut chosen: Option<i64> = None;
    let mut command = None;
    let photo = possible(z).photo;
    ui.horizontal(|ui| {
        ui.set_height(24.0);
        let before = z.search.clone();
        let edit = egui::TextEdit::singleline(&mut z.search).hint_text(crate::tr("Name suchen …")).desired_width(300.0);
        let r = crate::lr::edit_field(ui, edit);
        lr::tip_key(ui, &r, "Person suchen", "search");
        if std::mem::take(&mut z.search_focus) {
            r.request_focus();
        }
        if z.search != before {
            z.search_pick = 0;
        }
        let hits: Vec<(i64, String)> = z.index.as_ref().map(|ix| model::matching_people(ix, &z.search).into_iter().take(12).map(|p| (p.id, p.name.clone())).collect()).unwrap_or_default();
        let typing = r.has_focus() && !z.search.trim().is_empty();
        if typing {
            let (up, down) = ctx.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
            if down {
                z.search_pick = (z.search_pick + 1).min(hits.len().saturating_sub(1));
            }
            if up {
                z.search_pick = z.search_pick.saturating_sub(1);
            }
        }
        if r.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            chosen = hits.get(z.search_pick).map(|h| h.0);
        }
        // hits below the field (floating, the grid does not move)
        if typing && !hits.is_empty() && chosen.is_none() {
            let counts: HashMap<i64, (usize, usize)> = z.built.people.iter().map(|c| (c.value, (c.n, c.open))).collect();
            egui::Area::new(egui::Id::NULL.with("personen-suche")).order(egui::Order::Foreground).fixed_pos(r.rect.left_bottom() + egui::vec2(0.0, 2.0)).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(r.rect.width());
                    for (i, (id, name)) in hits.iter().enumerate() {
                        let (n, open) = counts.get(id).copied().unwrap_or((0, 0));
                        let right = if open > 0 { crate::trf!("{a} · {b} offen", a = crate::library_ui::thousands(n), b = crate::library_ui::thousands(open)) } else { crate::library_ui::thousands(n) };
                        let resp = ui.add(egui::Button::selectable(i == z.search_pick, name.as_str()).right_text(right).min_size(egui::vec2(r.rect.width() - 12.0, 0.0)));
                        if resp.clicked() {
                            chosen = Some(*id);
                        }
                    }
                });
            });
        }
        if chosen.is_some() {
            r.surrender_focus();
        }
        if !z.search.is_empty() || !z.filter.people.is_empty() {
            let x = ui.add(egui::Button::new("×").small());
            lr::hover_tip(ui, &x, crate::tr("Suche und Personenauswahl leeren"));
            if x.clicked() {
                z.search.clear();
                z.filter.people.clear();
                z.stack = None;
                z.dirty = true;
                z.to_start = true;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().slider_width = 110.0;
            let r = ui.add(egui::Slider::new(&mut z.size, 100.0..=260.0).show_value(false));
            lr::hover_tip(ui, &r, crate::tr("Kachelgröße"));
            ui.label(egui::RichText::new(crate::tr("Größe")).small().color(lr::TEXT_DIM));
            ui.add_space(14.0);
            // view: fixed place, both always visible ("Groß" needs one chosen face)
            ui.spacing_mut().item_spacing.x = 0.0;
            let big = ui.add_enabled(photo || z.big, egui::Button::new(crate::tr("Groß")).selected(z.big).corner_radius(0.0).min_size(egui::vec2(64.0, 22.0)));
            lr::tip_key(ui, &big, "Foto des gewählten Gesichts groß zeigen", "people_large");
            if big.clicked() && !z.big {
                command = z.selection.iter().next().map(|&i| Command::Big(i));
            }
            let grid = ui.add(egui::Button::new(crate::tr("Raster")).selected(!z.big).corner_radius(0.0).min_size(egui::vec2(64.0, 22.0)));
            lr::tip_key(ui, &grid, "Gesichter als Raster zeigen", "people_back");
            if grid.clicked() && z.big {
                command = Some(Command::CloseBig);
            }
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(14.0);
            if crate::apple_photos_ui::an() && crate::apple_photos_ui::media_library().is_some() {
                let r = ui.add_enabled(!crate::apple_photos_ui::running(), egui::Button::new(crate::tr("Apple Fotos abgleichen")));
                lr::hover_tip(ui, &r, crate::tr("Liest Namen und Gesichter aus Apple Fotos jetzt ein"));
                if r.clicked() {
                    crate::apple_photos_ui::kick(ctx);
                    z.hints = None;
                }
            }
        });
    });
    if let Some(id) = chosen {
        choose_person(z, id);
    }
    command
}

fn choose_person(z: &mut State, id: i64) {
    z.filter.people = BTreeSet::from([id]);
    z.search = z.index.as_ref().map(|ix| ix.name(id).to_string()).unwrap_or_default();
    z.stack = None;
    z.selection.clear();
    z.dirty = true;
    z.to_start = true;
}

/// Row 2: four filter columns like in the library. Returns: selection changed.
fn columns_row(ui: &mut egui::Ui, z: &mut State) -> bool {
    let Some(ix) = z.index.as_ref() else {
        ui.allocate_space(egui::vec2(ui.available_width(), COLUMN_H));
        return false;
    };
    let gap = 6.0;
    let total = ui.available_width();
    let widths = [0.3f32, 0.3, 0.2, 0.2].map(|f| ((total - 3.0 * gap) * f).floor());
    // person column: positions in the alphabetical list, "(ohne Namen)" last; the search text narrows it
    let people_pos: HashMap<i64, u32> = ix.people.iter().enumerate().map(|(i, p)| (p.id, i as u32)).collect();
    let pattern = library::people::person_brief(&z.search);
    let narrow = !pattern.is_empty() && z.filter.one_person().is_none_or(|p| library::people::person_brief(ix.name(p)) != pattern);
    let person_rows: Vec<ListRow> = z
        .built
        .people
        .iter()
        .filter(|c| c.value == NO_PERSON || !narrow || z.filter.people.contains(&c.value) || library::people::person_brief(ix.name(c.value)).contains(&pattern))
        .map(|c| {
            let hidden = ix.people.iter().any(|p| p.id == c.value && p.hidden);
            let name = if c.value == NO_PERSON {
                crate::tr("(ohne Namen)").to_string()
            } else if hidden {
                crate::trf!("{a} (nicht angezeigt)", a = ix.name(c.value))
            } else {
                ix.name(c.value).to_string()
            };
            let count = if c.open > 0 { format!("{} · {}", crate::library_ui::thousands(c.n), crate::library_ui::thousands(c.open)) } else { crate::library_ui::thousands(c.n) };
            ListRow { value: if c.value == NO_PERSON { NONE_ROW } else { people_pos.get(&c.value).copied().unwrap_or(0) }, name, count, dim: c.n + c.open == 0 }
        })
        .collect();
    let person_sel: BTreeSet<u32> = z.filter.people.iter().filter_map(|p| if *p == NO_PERSON { Some(NONE_ROW) } else { people_pos.get(p).copied() }).collect();
    let status_rows: Vec<ListRow> = z.built.status.iter().map(|c| ListRow { value: c.value as u32, name: status_name(ALL_STATUS[c.value as usize]).to_string(), count: crate::library_ui::thousands(c.n), dim: c.n == 0 }).collect();
    let status_sel: BTreeSet<u32> = z.filter.status.iter().map(|s| *s as u32).collect();
    let source_rows: Vec<ListRow> = z.built.sources.iter().map(|c| ListRow { value: c.value as u32, name: source_name(ALL_SOURCES[c.value as usize]).to_string(), count: crate::library_ui::thousands(c.n), dim: c.n == 0 }).collect();
    let source_sel: BTreeSet<u32> = z.filter.sources.iter().map(|s| *s as u32).collect();
    let year_rows: Vec<ListRow> = z.built.years.iter().map(|c| ListRow { value: c.value as u32, name: if c.value == 0 { crate::tr("(ohne Datum)").to_string() } else { c.value.to_string() }, count: crate::library_ui::thousands(c.n), dim: c.n == 0 }).collect();
    let year_sel: BTreeSet<u32> = z.filter.years.iter().map(|y| *y as u32).collect();
    let people_ids: Vec<i64> = ix.people.iter().map(|p| p.id).collect();
    let mut clicks: [Option<BTreeSet<u32>>; 4] = [None, None, None, None];
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let columns: [(&str, &Vec<ListRow>, &BTreeSet<u32>, &str); 4] = [
            ("PERSON", &person_rows, &person_sel, "bestätigt · offen"),
            ("STATUS", &status_rows, &status_sel, ""),
            ("QUELLE", &source_rows, &source_sel, ""),
            ("JAHR", &year_rows, &year_sel, ""),
        ];
        for (i, (title, rows, sel, note)) in columns.into_iter().enumerate() {
            let (frame, _) = ui.allocate_exact_size(egui::vec2(widths[i], COLUMN_H), egui::Sense::hover());
            ui.painter().rect_filled(frame, 0.0, Color32::from_gray(0x16));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(frame.shrink2(egui::vec2(4.0, 3.0))).layout(egui::Layout::top_down(egui::Align::Min)));
            let c = &mut child;
            c.horizontal(|ui| {
                ui.set_height(18.0);
                ui.label(egui::RichText::new(crate::tr(title)).font(lr::semibold(10.5)).color(lr::TEXT_DIM));
                if !note.is_empty() {
                    ui.label(egui::RichText::new(crate::tr(note)).size(9.5).color(Color32::from_gray(0x80)));
                }
                if !sel.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(12.0), if resp.hovered() { Color32::WHITE } else { lr::TEXT_DIM });
                        lr::hover_tip(ui, &resp, crate::tr("Auswahl dieser Spalte aufheben"));
                        if resp.clicked() {
                            clicks[i] = Some(BTreeSet::new());
                        }
                    });
                }
            });
            if let Some((value, mods)) = list_rows_labeled(c, ("personen-spalte", i), rows, sel, None) {
                let order: Vec<u32> = rows.iter().map(|r| r.value).collect();
                let mut s = (*sel).clone();
                list_click(&mut s, &order, &mut z.anchors[i], value, mods);
                clicks[i] = Some(s);
            }
        }
    });
    let mut changed = false;
    for (i, c) in clicks.into_iter().enumerate() {
        let Some(s) = c else { continue };
        changed = true;
        match i {
            0 => {
                z.filter.people = s.iter().map(|v| if *v == NONE_ROW { NO_PERSON } else { people_ids.get(*v as usize).copied().unwrap_or(NO_PERSON) }).collect();
                // the search field shows the chosen name (or nothing)
                z.search = z.filter.one_person().and_then(|p| z.index.as_ref().map(|ix| ix.name(p).to_string())).unwrap_or_default();
            }
            1 => z.filter.status = s.iter().filter_map(|v| ALL_STATUS.get(*v as usize).copied()).collect(),
            2 => z.filter.sources = s.iter().filter_map(|v| ALL_SOURCES.get(*v as usize).copied()).collect(),
            _ => z.filter.years = s.iter().map(|v| *v as u16).collect(),
        }
    }
    changed
}

/// Tiles of the selection, in order.
fn selected(z: &State) -> Vec<(usize, Tile)> {
    let mut v: Vec<usize> = z.selection.iter().copied().collect();
    v.sort_unstable();
    v.into_iter().filter_map(|i| z.built.tiles.get(i).map(|t| (i, *t))).collect()
}

/// What "Ja", "Nein" and "Anderer Name" do for the current selection (enables the buttons, texts of their hints).
#[derive(Default)]
struct Possible {
    yes: Option<&'static str>,
    no: Option<&'static str>,
    name: Option<&'static str>,
    photo: bool,
}

fn possible(z: &State) -> Possible {
    let sel = selected(z);
    let mut p = Possible::default();
    let Some(ix) = z.index.as_ref() else { return p };
    if sel.is_empty() {
        return p;
    }
    let status = |t: &Tile| if let Kind::Face(f) = t.kind { ix.face(f).map(|f| f.status) } else { None };
    let all = |pred: &dyn Fn(&Tile, Option<Status>) -> bool| sel.iter().all(|(_, t)| pred(t, status(t)));
    if all(&|t, s| matches!(t.kind, Kind::Face(_)) && s.is_some_and(|s| s.is_question())) {
        p.yes = Some("Ja: Vorschlag annehmen");
        p.no = Some("Nein: Vorschlag ablehnen (wird nicht wieder vorgeschlagen)");
        p.name = Some("Anderen Namen eintragen");
    } else if all(&|t, _| matches!(t.kind, Kind::Person(_))) {
        p.yes = (sel.len() == 1).then_some("Alle bestätigten Gesichter zeigen");
        p.name = (sel.len() == 1).then_some("Person umbenennen oder zusammenführen");
    } else if all(&|t, _| matches!(t.kind, Kind::Group(_))) {
        p.yes = (sel.len() == 1).then_some("Gesichter der Gruppe zeigen");
        p.no = Some("Gruppe nicht benennen");
        p.name = Some("Gruppe benennen");
    } else if all(&|t, _| matches!(t.kind, Kind::Face(_))) {
        p.name = Some("Namen eintragen");
        if all(&|_, s| s == Some(Status::Confirmed)) {
            p.no = Some("Nein: Name entfernen");
        } else if all(&|_, s| matches!(s, Some(Status::Unnamed | Status::UnnamedGroup))) {
            p.no = Some("Nicht benennen");
        }
    }
    p.photo = sel.len() == 1 && matches!(sel[0].1.kind, Kind::Face(_));
    p
}

/// Row 3: count, Ja / Nein / Anderer Name / ↶ ↷, on the right "confirm all very likely". Fixed places: the right
/// button is placed first, the left part is clipped before it (never drawn over it).
fn command_bar(ui: &mut egui::Ui, z: &mut State) -> Option<Command> {
    let mut command = None;
    let p = possible(z);
    let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 24.0));
    ui.allocate_rect(row, egui::Sense::hover());
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
    {
        let ui = &mut right;
        let n = z.built.sure.len();
        let b = ui.add_enabled(n > 0 && z.stack.is_none(), egui::Button::new(crate::trf!("Alle sicheren bestätigen ({n})", n = crate::library_ui::thousands(n))).min_size(egui::vec2(0.0, 24.0)));
        lr::hover_tip(ui, &b, crate::tr("Bestätigt die Vorschläge „sehr wahrscheinlich“ der aktuellen Auswahl in den Spalten (⌘Z nimmt es zurück)"));
        if b.clicked() {
            command = Some(Command::ConfirmSure);
        }
    }
    let left_rect = row.with_max_x(right.min_rect().left() - 16.0);
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(left_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
    {
        let ui = &mut left;
        let n = z.selection.len();
        let count_text = if n > 0 { crate::trf!("{a} von {b} ausgewählt", a = n, b = crate::library_ui::thousands(z.built.tiles.len())) } else if z.built.tiles.is_empty() {
            crate::tr("Nichts angezeigt").to_string()
        } else {
            crate::trf!("{n} angezeigt", n = crate::library_ui::thousands(z.built.tiles.len()))
        };
        let (r, _) = ui.allocate_exact_size(egui::vec2(160.0, 20.0), egui::Sense::hover());
        let g = ui.painter().layout_no_wrap(count_text, egui::FontId::proportional(11.5), lr::TEXT_DIM);
        ui.painter().with_clip_rect(r).galley(egui::pos2(r.left(), r.center().y - g.size().y / 2.0), g, lr::TEXT_DIM);
        let yes = lr::main_button_min(ui, "Ja", p.yes.is_some(), 56.0);
        lr::tip_key(ui, &yes, p.yes.unwrap_or("Ja: Vorschlag annehmen bzw. Stapel öffnen"), "people_yes");
        if yes.clicked() && p.yes.is_some() {
            command = Some(Command::Yes);
        }
        let no = ui.add_enabled(p.no.is_some(), egui::Button::new(crate::tr("Nein")).min_size(egui::vec2(56.0, 24.0)));
        lr::tip_key(ui, &no, p.no.unwrap_or("Nein: Vorschlag ablehnen bzw. Name entfernen"), "people_no");
        if no.clicked() {
            command = Some(Command::No);
        }
        let name = ui.add_enabled(p.name.is_some(), egui::Button::new(crate::tr("Anderer Name …")).min_size(egui::vec2(0.0, 24.0)));
        lr::tip_key(ui, &name, p.name.unwrap_or("Namen eintragen"), "people_name");
        if name.clicked() {
            open_name_field(z);
        }
        ui.add_space(8.0);
        let u = ui.add_enabled(!z.undo.is_empty() && z.pending_writes == 0, egui::Button::new("↶").min_size(egui::vec2(28.0, 24.0)));
        lr::tip_key(ui, &u, "Rückgängig", "undo");
        if u.clicked() {
            command = Some(Command::Undo);
        }
        let r = ui.add_enabled(!z.redo.is_empty() && z.pending_writes == 0, egui::Button::new("↷").min_size(egui::vec2(28.0, 24.0)));
        lr::tip_key(ui, &r, "Wiederholen", "redo");
        if r.clicked() {
            command = Some(Command::Redo);
        }
    }
    command
}

fn open_name_field(z: &mut State) {
    let sel = selected(z);
    let prefill = match (sel.len(), sel.first().map(|x| x.1.kind)) {
        (1, Some(Kind::Person(p))) => z.index.as_ref().map(|ix| ix.name(p).to_string()).unwrap_or_default(),
        _ => String::new(),
    };
    if !sel.is_empty() {
        z.name_field = Some(NameField { text: prefill, focus: true, pick: 0 });
    }
}

/// Name field (N): floating below the command bar, with the matching people (alphabetically, ↑ ↓ ↩).
fn name_field(z: &mut State, ctx: &egui::Context, anchor: egui::Pos2) -> Option<Command> {
    let mut f = z.name_field.take()?;
    let mut command = None;
    let mut close = false;
    let hits: Vec<String> = z.index.as_ref().map(|ix| model::matching_people(ix, &f.text).into_iter().take(10).map(|p| p.name.clone()).collect()).unwrap_or_default();
    egui::Area::new(egui::Id::NULL.with("personen-namensfeld")).order(egui::Order::Foreground).fixed_pos(anchor + egui::vec2(160.0, 0.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                let r = crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut f.text).hint_text(crate::tr("Name …")).desired_width(260.0));
                if std::mem::take(&mut f.focus) {
                    r.request_focus();
                }
                let (up, down) = ctx.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
                if down && !hits.is_empty() {
                    f.pick = (f.pick + 1).min(hits.len());
                }
                if up {
                    f.pick = f.pick.saturating_sub(1);
                }
                let input = f.text.trim().to_string();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                // ↩ takes the highlighted person (pick 1…), otherwise the typed name
                let take = if f.pick > 0 { hits.get(f.pick - 1).cloned() } else { Some(input.clone()).filter(|s| !s.is_empty()) };
                if (ui.add_enabled(take.is_some(), egui::Button::new(crate::tr("Übernehmen"))).clicked() || enter) && take.is_some() {
                    command = take.map(Command::Name);
                    close = true;
                }
                let k = ui.button(crate::tr("Kein Name"));
                lr::hover_tip(ui, &k, crate::tr("Name entfernen, das Gesicht bleibt ohne Namen"));
                if k.clicked() {
                    command = Some(Command::NoName);
                    close = true;
                }
                if ui.button(crate::tr("Abbrechen")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
            for (i, n) in hits.iter().enumerate() {
                if ui.add(egui::Button::selectable(f.pick == i + 1, n.as_str())).clicked() {
                    command = Some(Command::Name(n.clone()));
                    close = true;
                }
            }
        });
    });
    if !close {
        z.name_field = Some(f);
    }
    command
}

fn merge_question(z: &mut State, ctx: &egui::Context, anchor: egui::Pos2) {
    let Some((from, name, n)) = z.merge_question.clone() else { return };
    egui::Area::new(egui::Id::NULL.with("personen-zusammenfuehren")).order(egui::Order::Foreground).fixed_pos(anchor + egui::vec2(160.0, 0.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::trf!("„{a}“ gibt es schon. {b} Gesichter dazunehmen?", a = name, b = n)).color(lr::TEXT));
                if ui.button(crate::tr("Ja, zusammenführen")).clicked() {
                    let st = step((from, name.clone()), |db, (v, n)| people::rename_person(db, v, &n).map(|_| ()));
                    write(z, ctx, WriteJob { steps: vec![st], faces: Vec::new(), person: Some(from), people_changed: true, undo: UndoMode::Record });
                    z.merge_question = None;
                    z.stack = None;
                    z.dirty = true;
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    z.merge_question = None;
                }
            });
        });
    });
}

/// Row 4 (always the same height). With the large photo only "← Raster · file name · Rahmen · Entwickeln". Otherwise:
/// back from a stack and its title, the commands of a chosen person; on the right, in the room the commands leave
/// (never over them), messages and the offer for an Apple group.
fn context_row(ui: &mut egui::Ui, z: &mut State) -> Option<Command> {
    let mut command = None;
    let h = 26.0;
    let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), h));
    ui.allocate_rect(row, egui::Sense::hover());
    let Some(ix) = z.index.as_ref() else { return None };
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    left.set_clip_rect(row.intersect(ui.clip_rect()));
    if z.big {
        let ui = &mut left;
        let b = ui.button(crate::tr("← Raster"));
        lr::tip_key(ui, &b, "Zurück zum Raster", "people_back");
        if b.clicked() {
            command = Some(Command::CloseBig);
        }
        let name = chosen_face(z).and_then(|id| ix.face(id)).map(|f| f.file_name.clone()).unwrap_or_default();
        // the file name in a fixed width: the buttons after it never move
        let (r, _) = ui.allocate_exact_size(egui::vec2(220.0, 20.0), egui::Sense::hover());
        let g = ui.painter().layout_job(single_line(name, lr::semibold(13.0), lr::TEXT, r.width()));
        ui.painter().galley(egui::pos2(r.left() + 4.0, r.center().y - g.size().y / 2.0), g, lr::TEXT);
        let f = ui.add(egui::Button::new(crate::tr("Rahmen")).selected(z.frames));
        lr::tip_key(ui, &f, "Gesichtsrahmen zeigen", "people_frames");
        if f.clicked() {
            command = Some(Command::ToggleFrames);
        }
        let d = ui.add_enabled(possible(z).photo, egui::Button::new(crate::tr("Entwickeln")));
        lr::tip_key(ui, &d, "Dieses Foto in Entwickeln öffnen", "people_photo");
        if d.clicked() {
            command = z.selection.iter().next().map(|&i| Command::OpenPhoto(i));
        }
        return command;
    }
    let person = match z.stack {
        Some(Stack::Person(p)) => Some(p),
        _ => z.filter.one_person().or_else(|| {
            let s = selected(z);
            match (s.len(), s.first().map(|x| x.1.kind)) {
                (1, Some(Kind::Person(p))) => Some(p),
                _ => None,
            }
        }),
    };
    {
        let ui = &mut left;
        if let Some(st) = z.stack {
            let b = ui.button(crate::tr("← Zurück"));
            lr::tip_key(ui, &b, "Zurück zur Übersicht", "people_back");
            if b.clicked() {
                command = Some(Command::Back);
            }
            let title = match st {
                Stack::Person(p) => ix.name(p).to_string(),
                Stack::Group(_) => crate::tr("Gruppe ohne Namen").to_string(),
            };
            ui.label(egui::RichText::new(title).font(lr::semibold(13.0)).color(lr::TEXT));
        }
        // the commands of one person: chosen by search/column, opened as a stack, or one person stack selected
        if let Some(p) = person {
            let name = ix.name(p).to_string();
            if z.stack.is_none() {
                ui.label(egui::RichText::new(&name).font(lr::semibold(13.0)).color(lr::TEXT));
            }
            ui.add_space(4.0);
            let b = ui.button(crate::tr("In der Bibliothek zeigen"));
            lr::hover_tip(ui, &b, &crate::trf!("Alle Fotos von {a} in der Bibliothek zeigen", a = name));
            if b.clicked() {
                command = Some(Command::ShowPhotos(p));
            }
            let hidden = ix.people.iter().any(|x| x.id == p && x.hidden);
            let b = ui.button(if hidden { crate::tr("Einblenden") } else { crate::tr("Ausblenden") });
            let tip = if hidden {
                crate::trf!("{a} wieder einblenden: bekommt wieder Vorschläge", a = name)
            } else {
                crate::trf!("{a} ausblenden: keine Vorschläge mehr, nur noch in der Spalte PERSON zu finden", a = name)
            };
            lr::hover_tip(ui, &b, &tip);
            if b.clicked() {
                command = Some(Command::Hide(p, !hidden));
            }
        }
        // faces assigned automatically from Apple groups: release them (the chosen faces' groups, or all of the person)
        let chosen: Vec<i64> = selected(z)
            .iter()
            .filter_map(|(_, t)| match t.kind {
                Kind::Face(id) => ix.face(id).filter(|f| f.source == Source::AppleGroup && f.person.is_some()).map(|f| f.id),
                _ => None,
            })
            .collect();
        let of_person = person.map_or(0, |p| ix.faces.iter().filter(|f| f.person == Some(p) && f.source == Source::AppleGroup).count());
        if !chosen.is_empty() || of_person > 0 {
            let b = ui.button(crate::tr("Zuordnung aus Apple-Gruppe lösen"));
            let tip = if chosen.is_empty() {
                crate::trf!("Die {n} Gesichter wieder lösen, die graineo aus Apple-Gruppen {a} zugeordnet hat. Sie tragen dann keinen Namen und werden zu Vorschlägen; die Gruppen werden nicht wieder von selbst zugeordnet.", n = of_person, a = person.map_or("?", |p| ix.name(p)))
            } else {
                crate::trf!("Die Apple-Gruppen der {n} gewählten Gesichter wieder lösen. Ihre Gesichter tragen dann keinen Namen und werden zu Vorschlägen; die Gruppen werden nicht wieder von selbst zugeordnet.", n = chosen.len())
            };
            lr::hover_tip(ui, &b, &tip);
            if b.clicked() {
                command = Some(Command::ReleaseApple(if chosen.is_empty() { person } else { None }, chosen));
            }
        }
    }
    // messages on the right: errors, newer suggestions, Apple groups – only in the room the commands left
    let right_rect = row.with_min_x((left.min_rect().right() + 24.0).min(row.right()));
    if right_rect.width() < 120.0 {
        return command;
    }
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(right_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    right.set_clip_rect(right_rect.intersect(ui.clip_rect()));
    let ui = &mut right;
    if !z.message.is_empty() {
        if ui.small_button("×").clicked() {
            z.message.clear();
        }
        let text = crate::menu::i18n::tr_msg(&z.message).to_string();
        one_line(ui, &text, Color32::from_rgb(230, 120, 100));
    } else if z.newer.load(Ordering::Relaxed) && z.pending_writes == 0 {
        let b = ui.button(crate::tr("Aktualisieren"));
        lr::hover_tip(ui, &b, crate::tr("Die neuen Vorschläge übernehmen (die Ansicht ändert sich nicht von allein)"));
        if b.clicked() {
            command = Some(Command::Refresh);
        }
        one_line(ui, crate::tr("Die Vorschläge wurden im Hintergrund neu berechnet."), lr::TEXT_DIM);
    } else if let Some(c) = apple_hint(ui, z, person) {
        command = Some(c);
    }
    command
}

/// Keys of the view (not while typing). ⌘Z, ⇧⌘Z, ⌘A and ⌘D come from the menu ([`request`]).
fn keys(z: &mut State, ctx: &egui::Context) -> Option<Command> {
    if z.name_field.is_some() || z.merge_question.is_some() {
        return None;
    }
    if crate::shortcuts::pressed(ctx, "search") {
        z.search_focus = true;
        return None;
    }
    if ctx.egui_wants_keyboard_input() {
        return None;
    }
    let plain = |k: egui::Key| ctx.input(|i| i.key_pressed(k) && !i.modifiers.command && !i.modifiers.alt);
    let has = !z.selection.is_empty();
    let p = possible(z);
    if plain(egui::Key::Escape) {
        if z.big {
            return Some(Command::CloseBig);
        }
        if z.stack.is_some() {
            return Some(Command::Back);
        }
        z.selection.clear();
        return None;
    }
    if has && plain(egui::Key::Enter) && p.yes.is_some() {
        return Some(Command::Yes);
    }
    if has && (plain(egui::Key::Backspace) || plain(egui::Key::Delete)) && p.no.is_some() {
        return Some(Command::No);
    }
    if has && plain(egui::Key::N) && p.name.is_some() {
        open_name_field(z);
        return None;
    }
    // D: the chosen face's photo in Develop (as D in the grid); H: face boxes on the large photo
    if crate::shortcuts::pressed(ctx, "single_photo_d") && p.photo {
        return z.selection.iter().next().map(|&i| Command::OpenPhoto(i));
    }
    if crate::shortcuts::pressed(ctx, "people_frames") {
        return Some(Command::ToggleFrames);
    }
    if plain(egui::Key::Space) {
        if z.big {
            return Some(Command::CloseBig);
        }
        if p.photo {
            return z.selection.iter().next().map(|&i| Command::Big(i));
        }
    }
    // arrows: move the selection
    let cols = z.columns.max(1) as isize;
    let d: Option<isize> = if plain(egui::Key::ArrowRight) {
        Some(1)
    } else if plain(egui::Key::ArrowLeft) {
        Some(-1)
    } else if plain(egui::Key::ArrowDown) {
        Some(cols)
    } else if plain(egui::Key::ArrowUp) {
        Some(-cols)
    } else {
        None
    };
    if let (Some(d), false) = (d, z.built.tiles.is_empty()) {
        let from = z.anchor.unwrap_or(0) as isize;
        let i = (from + d).clamp(0, z.built.tiles.len() as isize - 1) as usize;
        z.selection = HashSet::from([i]);
        z.anchor = Some(i);
        z.scroll_to = Some(i);
    }
    None
}

/// Layout of the grid: section headers and rows of tiles, one below the other (only the visible part is drawn).
struct Layout {
    columns: usize,
    tile_w: f32,
    tile_h: f32,
    /// (top, height, section, first tile or None for the header)
    rows: Vec<(f32, f32, usize, Option<usize>)>,
    total: f32,
}

fn layout(built: &Built, width: f32, size: f32) -> Layout {
    let columns = ((width / size).floor() as usize).max(1);
    let tile_w = width / columns as f32;
    let tile_h = tile_w - 8.0 + TEXT_HEIGHT;
    let mut rows = Vec::new();
    let mut y = 0.0;
    for (si, s) in built.sections.iter().enumerate() {
        rows.push((y, SECTION_H, si, None));
        y += SECTION_H;
        let mut i = s.range.start;
        while i < s.range.end {
            rows.push((y, tile_h, si, Some(i)));
            y += tile_h;
            i += columns;
        }
        y += 6.0;
    }
    Layout { columns, tile_w, tile_h, rows, total: y }
}

/// The grid with its sections (only visible rows are drawn).
fn grid(ui: &mut egui::Ui, z: &mut State, ctx: &egui::Context) -> Option<Command> {
    if z.index.is_none() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.label(egui::RichText::new(crate::tr("Lädt …")).size(15.0).color(lr::TEXT_DIM)));
        return None;
    }
    if z.built.tiles.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(crate::tr("Keine Gesichter für diese Auswahl")).size(15.0).color(lr::TEXT));
        });
        return None;
    }
    let lay = layout(&z.built, ui.available_width(), z.size);
    z.columns = lay.columns;
    let (cmd_mod, shift) = { let m = crate::click_mods::at_press(ctx); (m.command, m.shift) };
    let mut visible: Vec<i64> = Vec::new();
    let mut click: Option<usize> = None;
    let mut double: Option<usize> = None;
    let offset_id = egui::Id::NULL.with("personen-raster-y");
    let mut sa = egui::ScrollArea::vertical().id_salt("personen-raster").auto_shrink([false; 2]);
    let restore = z.restore_y.take();
    let mut placed = false;
    if std::mem::take(&mut z.to_start) {
        sa = sa.vertical_scroll_offset(0.0);
        placed = true;
    } else if let Some(y) = restore {
        placed = true;
        // back from the large photo or from Develop: the place the grid had
        sa = sa.vertical_scroll_offset(y);
        ctx.data_mut(|d| d.insert_temp(offset_id, y));
    }
    if let Some(i) = z.scroll_to.take() {
        if let Some(row) = lay.rows.iter().find(|r| r.3.is_some_and(|s| i >= s && i < s + lay.columns)) {
            let off = ctx.data(|d| d.get_temp::<f32>(offset_id)).unwrap_or(0.0);
            let view_h = ui.available_height();
            if row.0 < off {
                sa = sa.vertical_scroll_offset(row.0);
                placed = true;
            } else if row.0 + row.1 > off + view_h {
                sa = sa.vertical_scroll_offset(row.0 + row.1 - view_h);
                placed = true;
            }
        }
    }
    if let Some(y) = measuring_scroll() {
        sa = sa.vertical_scroll_offset(y);
        placed = true;
    }
    let z_ref: &State = z;
    let ix = z_ref.index.as_ref().unwrap();
    let out = lr::with_placed_scroll(ui, placed, |ui| sa.show_viewport(ui, |ui, viewport| {
        ui.set_height(lay.total);
        let origin = ui.max_rect().min;
        let width = ui.max_rect().width();
        let first = lay.rows.partition_point(|r| r.0 + r.1 < viewport.min.y);
        for &(top, h, si, start) in lay.rows[first..].iter().take_while(|r| r.0 < viewport.max.y) {
            let y = origin.y + top;
            match start {
                None => {
                    let s = &z_ref.built.sections[si];
                    let title = section_title(ix, &z_ref.filter, z_ref.stack, s.kind).to_uppercase();
                    let p = ui.painter();
                    let g = p.layout_no_wrap(title, lr::semibold(11.0), lr::TEXT);
                    let base = egui::pos2(origin.x + 4.0, y + h - 8.0 - g.size().y);
                    let gw = g.size().x;
                    p.galley(base, g, lr::TEXT);
                    let n = s.range.len();
                    let sub = match s.kind {
                        SectionKind::People if z_ref.filter.one_person().is_some() => String::new(),
                        SectionKind::People => crate::trf!("{n} Personen", n = crate::library_ui::thousands(n)),
                        SectionKind::Groups => crate::trf!("{n} Gruppen", n = crate::library_ui::thousands(n)),
                        SectionKind::Questions | SectionKind::Doubtful => crate::trf!("{n} · sicherste zuerst", n = crate::library_ui::thousands(n)),
                        SectionKind::StackFaces => crate::trf!("{n} · neueste zuerst", n = crate::library_ui::thousands(n)),
                        _ => crate::library_ui::thousands(n),
                    };
                    p.text(egui::pos2(base.x + gw + 10.0, base.y + 1.0), egui::Align2::LEFT_TOP, sub, egui::FontId::proportional(10.5), lr::TEXT_DIM);
                    p.hline(origin.x..=origin.x + width, y + h - 3.0, egui::Stroke::new(1.0, Color32::from_gray(0x2a)));
                }
                Some(start) => {
                    let end = z_ref.built.sections[si].range.end.min(start + lay.columns);
                    for i in start..end {
                        let t = z_ref.built.tiles[i];
                        let rect = egui::Rect::from_min_size(egui::pos2(origin.x + (i - start) as f32 * lay.tile_w, y), egui::vec2(lay.tile_w, lay.tile_h));
                        let resp = ui.interact(rect, egui::Id::NULL.with(("personen-kachel", i)), egui::Sense::click());
                        draw_tile(ui, z_ref, ix, &t, rect, z_ref.selection.contains(&i), resp.hovered());
                        if let Some(f) = t.face {
                            visible.push(f);
                        }
                        if resp.double_clicked() {
                            double = Some(i);
                        } else if resp.clicked() {
                            click = Some(i);
                        }
                    }
                }
            }
        }
    }));
    ctx.data_mut(|d| d.insert_temp(offset_id, out.state.clamped_offset().y));
    z.grid_y = out.state.clamped_offset().y;
    fetch_images(z, ctx, &visible);
    if let Some(i) = click {
        if cmd_mod {
            if !z.selection.remove(&i) {
                z.selection.insert(i);
            }
            z.anchor = Some(i);
        } else if shift {
            let a = z.anchor.unwrap_or(i);
            z.selection.extend(a.min(i)..=a.max(i));
        } else {
            z.selection = HashSet::from([i]);
            z.anchor = Some(i);
        }
        z.name_field = None;
    }
    double.map(|i| match z.built.tiles[i].kind {
        Kind::Face(_) => Command::Big(i),
        _ => Command::Open(i),
    })
}

/// Back from the large photo: the grid at the place it had, the chosen tile in view.
fn close_big(z: &mut State) {
    if !z.big {
        return;
    }
    z.big = false;
    z.restore_y = Some(z.grid_y);
    z.scroll_to = z.selection.iter().min().copied();
}

/// Load the photo of the chosen face in the background when the choice changes (one load at a time, the newest
/// choice wins); take over a finished load.
fn update_photo(z: &mut State, ctx: &egui::Context) {
    if let Some(rx) = &z.photo_rx {
        if let Ok((image, r)) = rx.try_recv() {
            z.photo_rx = None;
            z.photo_loading = None;
            let photo = match r {
                Ok((ci, fits, faces)) => Photo { image, texture: Some(ctx.load_texture(format!("personen-foto-{image}"), ci, egui::TextureOptions::LINEAR)), fits, faces, error: None },
                Err(e) => Photo { image, texture: None, fits: false, faces: Vec::new(), error: Some(e) },
            };
            z.photo = Some(photo);
        }
    }
    let want = chosen_face(z).and_then(|id| z.index.as_ref()?.face(id).map(|f| f.image));
    let Some(image) = want else { return };
    if z.photo.as_ref().is_some_and(|p| p.image == image) || z.photo_loading.is_some() {
        return;
    }
    let (tx, rx) = channel();
    z.photo_rx = Some(rx);
    z.photo_loading = Some(image);
    let (path, c) = (z.db_path.clone(), ctx.clone());
    // normal priority: the person waits for it
    let _ = std::thread::Builder::new().name("people-photo".into()).spawn(move || {
        let r = Db::read_only(&path).or_else(|_| Db::open(&path)).map_err(|e| e.to_string()).and_then(|db| {
            let (img, fits) = fx::photo_for_boxes(&db, image, PHOTO_EDGE)?;
            let faces = people::faces_to_image(&db.conn, image);
            Ok((egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], img.as_raw()), fits, faces))
        });
        let _ = tx.send((image, r));
        c.request_repaint();
    });
}

/// The photo of the chosen face, if it is the one loaded.
fn shown_photo(z: &State) -> Option<&Photo> {
    let id = chosen_face(z)?;
    let image = z.index.as_ref()?.face(id)?.image;
    z.photo.as_ref().filter(|p| p.image == image)
}

/// Name and look of a face box: from the index (answers change it at once), else as read with the photo.
fn box_label(z: &State, f: &people::FaceInfo) -> (String, bool) {
    let ix = z.index.as_ref();
    match ix.and_then(|ix| ix.face(f.id).map(|g| (ix, g))) {
        Some((ix, g)) => {
            let name = |p: Option<i64>| p.map_or("?", |p| ix.name(p)).to_string();
            match g.status {
                Status::Confirmed => (name(g.person), true),
                Status::SuggestSure | Status::SuggestMaybe => (format!("{}?", name(g.person)), false),
                Status::Doubtful => (format!("{}?", name(g.suspect)), false),
                _ => (String::new(), false),
            }
        }
        None => match f.status.as_str() {
            "confirmed" if !f.name.is_empty() => (f.name.clone(), true),
            "suggested" if !f.name.is_empty() => (format!("{}?", f.name), false),
            _ => (String::new(), false),
        },
    }
}

/// Screen rectangle of a face box on the photo `img` (on screen); `None` for a box outside the photo.
fn box_on(img: egui::Rect, f: &people::FaceInfo) -> Option<egui::Rect> {
    if !library::face_frame::box_in_photo(f.x as f64, f.y as f64, f.b as f64, f.h as f64) {
        return None;
    }
    let r = egui::Rect::from_min_size(img.min + egui::vec2(f.x, f.y) * img.size(), egui::vec2(f.b, f.h) * img.size());
    Some(r.intersect(img))
}

/// How strongly the boxes on the large photo show: 1 with the boxes on; with them off the chosen face is marked for
/// [`FLASH`] s after it was chosen and fades out within [`FADE`] s, the others only show under the pointer.
fn frame_alpha(z: &State, now: f64) -> f32 {
    if z.frames {
        return 1.0;
    }
    let Some((_, t)) = z.flash else { return 0.0 };
    let d = now - t;
    if d <= FLASH {
        1.0
    } else {
        (1.0 - (d - FLASH) / FADE).clamp(0.0, 1.0) as f32
    }
}

/// The chosen face's photo large in place of the grid, with the face boxes and names ("Rahmen" switches them off). A
/// click on a face chooses it (if it is shown in the grid); a double click opens the photo in Develop.
fn big_view(ui: &mut egui::Ui, z: &State) -> Option<Command> {
    let area = ui.available_rect_before_wrap();
    let (area, resp) = ui.allocate_exact_size(area.size(), egui::Sense::click());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_gray(0x16));
    let mut command = None;
    let open = || z.selection.iter().next().map(|&i| Command::OpenPhoto(i));
    if resp.double_clicked() {
        command = open();
    }
    let chosen = chosen_face(z);
    let Some(photo) = shown_photo(z) else {
        painter.text(area.center(), egui::Align2::CENTER_CENTER, crate::tr("Lädt …"), egui::FontId::proportional(15.0), lr::TEXT_DIM);
        return command;
    };
    let Some(tex) = &photo.texture else {
        let text = photo.error.as_deref().map_or_else(|| crate::tr("Foto nicht lesbar").to_string(), |e| crate::trf!("Foto nicht lesbar: {e}", e = e));
        painter.text(area.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), lr::TEXT_DIM);
        return command;
    };
    let s = tex.size_vec2();
    let inner = area.shrink(12.0);
    let img = egui::Rect::from_center_size(inner.center(), s * (inner.width() / s.x).min(inner.height() / s.y));
    painter.image(tex.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    if !photo.fits {
        return command;
    }
    let now = ui.input(|i| i.time);
    let alpha = frame_alpha(z, now);
    if !z.frames && alpha > 0.0 {
        ui.ctx().request_repaint();
    }
    let pointer = ui.input(|i| i.pointer.hover_pos());
    for f in &photo.faces {
        let Some(b) = box_on(img, f) else { continue };
        let is_chosen = chosen == Some(f.id);
        let over = pointer.is_some_and(|p| b.expand(4.0).contains(p));
        // a face shown in the grid can be chosen with a click (also with the boxes off)
        let tile = z.built.tiles.iter().position(|t| t.kind == Kind::Face(f.id));
        if let Some(i) = tile {
            let r = ui.interact(b, egui::Id::NULL.with(("personen-gross-gesicht", f.id)), egui::Sense::click());
            if r.clicked() && !is_chosen {
                command = Some(Command::Choose(i));
            }
            if r.double_clicked() {
                command = open();
            }
        }
        // with the boxes off: the chosen one fades after choosing, the others show only under the pointer
        let a = if z.frames || over { 1.0 } else if is_chosen { alpha } else { 0.0 };
        if a <= 0.0 {
            continue;
        }
        let fade = |c: Color32| c.gamma_multiply(a);
        let (name, confirmed) = box_label(z, f);
        let color = if is_chosen { Color32::WHITE } else if confirmed { Color32::from_white_alpha(200) } else if name.is_empty() { Color32::from_white_alpha(90) } else { Color32::from_white_alpha(150) };
        painter.rect_stroke(b.expand(1.0), 0.0, egui::Stroke::new(3.0, fade(Color32::from_black_alpha(110))), egui::StrokeKind::Outside);
        painter.rect_stroke(b, 0.0, egui::Stroke::new(if is_chosen { 2.5 } else { 1.2 }, fade(color)), egui::StrokeKind::Outside);
        if name.is_empty() && !is_chosen && !over {
            continue;
        }
        let text = if name.is_empty() { crate::tr("Ohne Namen").to_string() } else { name };
        let g = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), fade(Color32::from_gray(235)));
        let pos = egui::pos2(b.center().x - g.size().x / 2.0, b.bottom() + 4.0);
        let bg = egui::Rect::from_min_size(pos - egui::vec2(5.0, 2.0), g.size() + egui::vec2(10.0, 4.0));
        painter.rect_filled(bg, 0.0, fade(Color32::from_black_alpha(if is_chosen { 235 } else { 190 })));
        if is_chosen {
            painter.rect_stroke(bg, 0.0, egui::Stroke::new(1.0, fade(Color32::from_gray(0xd8))), egui::StrokeKind::Inside);
        }
        painter.galley(pos, g, fade(Color32::WHITE));
    }
    command
}

/// Navigator in the Info panel (above the photo's details): the whole photo of the chosen face, the face marked with
/// a frame; follows the selection. Fixed height, an empty field without a chosen face.
pub fn navigator(ui: &mut egui::Ui) {
    const H: f32 = 150.0;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, H), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, lr::PANEL_DARK);
    Z.with(|zz| {
        let g = zz.borrow();
        let Some(z) = g.as_ref().filter(|z| z.is_open) else { return };
        let chosen = chosen_face(z);
        let text = |t: &str| {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, t, egui::FontId::proportional(11.0), lr::TEXT_DIM);
        };
        if chosen.is_none() {
            text(crate::tr("Kein Gesicht gewählt"));
            return;
        }
        let Some(photo) = shown_photo(z) else {
            text(crate::tr("Lädt …"));
            return;
        };
        let Some(tex) = &photo.texture else {
            text(crate::tr("Foto nicht lesbar"));
            return;
        };
        let s = tex.size_vec2();
        let img = egui::Rect::from_center_size(rect.center(), s * (w / s.x).min(H / s.y));
        painter.image(tex.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        if !photo.fits {
            return;
        }
        if let Some(b) = photo.faces.iter().find(|f| Some(f.id) == chosen).and_then(|f| box_on(img, f)) {
            // at least a few points large, so that a small face stays visible in the small photo
            let b = egui::Rect::from_center_size(b.center(), b.size().max(egui::vec2(8.0, 8.0)));
            painter.rect_stroke(b.expand(1.0), 0.0, egui::Stroke::new(3.0, Color32::from_black_alpha(140)), egui::StrokeKind::Outside);
            painter.rect_stroke(b, 0.0, egui::Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
        }
    });
}

fn draw_tile(ui: &egui::Ui, z: &State, ix: &FaceIndex, t: &Tile, rect: egui::Rect, chosen: bool, hovered: bool) {
    let inner = rect.shrink(4.0);
    let p = ui.painter();
    let stack = !matches!(t.kind, Kind::Face(_));
    let fill = if chosen { Color32::from_gray(0x3a) } else if hovered { Color32::from_gray(0x2a) } else { Color32::from_gray(0x22) };
    let card = if stack { egui::Rect::from_min_max(inner.min + egui::vec2(0.0, 6.0), inner.max - egui::vec2(6.0, 0.0)) } else { inner };
    if stack {
        // a stack: two cards behind it, offset to the upper right
        p.rect_filled(card.translate(egui::vec2(6.0, -6.0)), 0.0, Color32::from_gray(0x1b));
        p.rect_filled(card.translate(egui::vec2(3.0, -3.0)), 0.0, Color32::from_gray(0x1f));
    }
    p.rect_filled(card, 0.0, fill);
    if chosen {
        p.rect_stroke(card, 0.0, egui::Stroke::new(2.0, Color32::from_gray(0xd8)), egui::StrokeKind::Inside);
    }
    let side = (card.width() - 12.0).min(card.height() - TEXT_HEIGHT - 6.0);
    let image_rect = egui::Rect::from_min_size(card.min + egui::vec2(6.0, 6.0), egui::vec2(side, side));
    let hidden = matches!(t.kind, Kind::Person(pp) if ix.people.iter().any(|x| x.id == pp && x.hidden));
    match t.face.and_then(|g| z.textures.get(&g)) {
        Some((tex, _)) => {
            p.image(tex.id(), image_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), if hidden { Color32::from_gray(0x80) } else { Color32::WHITE });
        }
        None => {
            p.rect_filled(image_rect, 0.0, Color32::from_gray(0x2c));
        }
    }
    if stack {
        let g = p.layout_no_wrap(crate::library_ui::thousands(t.count), egui::FontId::proportional(10.5), Color32::WHITE);
        let r = egui::Rect::from_min_size(egui::pos2(image_rect.right() - g.size().x - 10.0, image_rect.top() + 4.0), g.size() + egui::vec2(6.0, 2.0));
        p.rect_filled(r, 2.0, Color32::from_black_alpha(170));
        p.galley(r.min + egui::vec2(3.0, 1.0), g, Color32::WHITE);
    }
    let (title, sub, question) = tile_text(ix, t);
    let w = card.width() - 12.0;
    let color = if question || hidden { lr::TEXT_DIM } else { lr::TEXT };
    let g1 = ui.fonts_mut(|f| f.layout_job(single_line(title, lr::semibold(12.5), color, w)));
    let h1 = g1.size().y;
    p.galley(egui::pos2(image_rect.left(), image_rect.bottom() + 5.0), g1, color);
    if !sub.is_empty() {
        let g2 = ui.fonts_mut(|f| f.layout_job(single_line(sub, egui::FontId::proportional(10.5), lr::TEXT_DIM, w)));
        p.galley(egui::pos2(image_rect.left(), image_rect.bottom() + 6.0 + h1), g2, lr::TEXT_DIM);
    }
}

/// Hand a job to the background writer (starts it the first time).
fn write(z: &mut State, ctx: &egui::Context, job: WriteJob) {
    write_with(z, ctx, job, None);
}

fn write_with(z: &mut State, ctx: &egui::Context, job: WriteJob, restore: Option<fx::Snapshot>) {
    if z.writer.is_none() {
        let (tx, rx) = channel();
        let (tx2, rx2) = channel();
        let (c, path) = (ctx.clone(), z.db_path.clone());
        let _ = std::thread::Builder::new().name("people-write".into()).spawn(move || writer(path, rx, tx2, c));
        z.writer = Some(tx);
        z.from_writer = Some(rx2);
    }
    z.message.clear();
    if let Some(tx) = &z.writer {
        match tx.send((job, restore)) {
            Ok(()) => z.pending_writes += 1,
            Err(std::sync::mpsc::SendError((job, restore))) => {
                // the writer is gone (it never should be): start a new one and hand it the job
                // (writes still waiting in the old one are lost: say so)
                let lost = std::mem::take(&mut z.pending_writes);
                z.writer = None;
                write_with(z, ctx, job, restore);
                if lost > 0 {
                    z.message = crate::tr("Nicht gespeichert: Schreiber beendet").to_string();
                }
            }
        }
    }
}

/// Change a face in the index right away (the writer delivers the exact row afterwards).
fn set_face(z: &mut State, id: i64, status: Status, person: Option<i64>) {
    if let Some(f) = z.index.as_mut().and_then(|ix| ix.face_mut(id)) {
        f.status = status;
        f.person = person;
        f.suspect = None;
        if status != Status::UnnamedGroup {
            f.group = None;
        }
    }
}

fn execute(z: &mut State, ctx: &egui::Context, c: Command, result: &mut Outcome) {
    let sel = selected(z);
    let first = sel.first().map(|x| x.0);
    // "Ja" on a stack opens it
    let c = match (c, sel.first()) {
        (Command::Yes, Some((i, t))) if !matches!(t.kind, Kind::Face(_)) => Command::Open(*i),
        (c, _) => c,
    };
    let Some(ix) = z.index.as_ref() else { return };
    let mut job = WriteJob { steps: Vec::new(), faces: Vec::new(), person: None, people_changed: false, undo: UndoMode::Record };
    let mut changes: Vec<(i64, Status, Option<i64>)> = Vec::new();
    match c {
        Command::Big(i) => {
            if matches!(z.built.tiles.get(i).map(|t| t.kind), Some(Kind::Face(_))) {
                z.selection = HashSet::from([i]);
                z.anchor = Some(i);
                z.name_field = None;
                z.big = true;
            }
            return;
        }
        Command::CloseBig => {
            close_big(z);
            return;
        }
        Command::ToggleFrames => {
            z.frames = !z.frames;
            library::settings::set(FRAMES_KEY, serde_json::json!(z.frames));
            // switched off: the chosen face is marked once more, then the boxes go
            z.flash = chosen_face(z).map(|c| (c, ctx.input(|i| i.time)));
            return;
        }
        Command::Choose(i) => {
            z.selection = HashSet::from([i]);
            z.anchor = Some(i);
            z.name_field = None;
            return;
        }
        Command::Back => {
            z.stack = None;
            z.selection.clear();
            z.dirty = true;
            z.to_start = true;
            return;
        }
        Command::Open(i) => {
            match z.built.tiles.get(i).map(|t| t.kind) {
                Some(Kind::Person(p)) => z.stack = Some(Stack::Person(p)),
                Some(Kind::Group(g)) => z.stack = Some(Stack::Group(g)),
                _ => return,
            }
            z.selection.clear();
            z.dirty = true;
            z.to_start = true;
            return;
        }
        Command::OpenPhoto(i) => {
            if let Some(Kind::Face(f)) = z.built.tiles.get(i).map(|t| t.kind) {
                result.open = ix.face(f).map(|f| f.image);
            }
            return;
        }
        Command::ShowPhotos(p) => {
            result.search = Some(format!("person:\"{}\"", ix.name(p)));
            return;
        }
        Command::Hide(p, v) => {
            job.steps.push(step((p, v), |db, (id, v)| people::hide_person(&db.conn, id, v)));
            job.people_changed = true;
            write(z, ctx, job);
            // hiding changes which suggestions count: read the whole index again once written
            z.index = None;
            z.from_loader = None;
            return;
        }
        Command::Refresh => {
            z.index = None;
            z.from_loader = None;
            z.newer.store(false, Ordering::Relaxed);
            return;
        }
        Command::AcceptGroup(hint) => {
            z.hints_dismissed.insert((hint.group, hint.person));
            job.steps.push(step(hint, |db, h| people::apple_accept(db, &h)));
            job.people_changed = true;
            write(z, ctx, job);
            // Apple's faces may be new rows: read the index again once written
            z.index = None;
            z.from_loader = None;
            return;
        }
        Command::ReleaseApple(person, faces) => {
            job.steps.push(step((person, faces), |db, (p, f)| library::apple_groups::release(db, p, &f).map(|_| ())));
            job.people_changed = true;
            job.undo = UndoMode::Never;
            write(z, ctx, job);
            // faces were released and boxes removed: read the index again once written
            z.index = None;
            z.from_loader = None;
            z.selection.clear();
            return;
        }
        Command::DismissGroup(group, person) => {
            z.hints_dismissed.insert((group, person));
            return;
        }
        Command::Undo | Command::Redo => {
            let undo = matches!(c, Command::Undo);
            if z.pending_writes > 0 {
                return;
            }
            let Some(s) = (if undo { z.undo.pop() } else { z.redo.pop() }) else { return };
            let faces = s.face_ids.clone();
            write_with(z, ctx, WriteJob { steps: Vec::new(), faces, person: None, people_changed: true, undo: if undo { UndoMode::Undo } else { UndoMode::Redo } }, Some(s));
            return;
        }
        Command::ConfirmSure => {
            let ids = z.built.sure.clone();
            for id in &ids {
                if let Some(f) = ix.face(*id) {
                    changes.push((*id, Status::Confirmed, f.person));
                }
            }
            job.steps.push(step(ids.clone(), |db, ids| {
                for id in ids {
                    people::answer_suggestion(db, id, true)?;
                }
                Ok(())
            }));
            job.faces = ids;
        }
        Command::Yes => {
            for (_, t) in &sel {
                match t.kind {
                    Kind::Person(_) | Kind::Group(_) => {}
                    Kind::Face(id) => {
                        let Some(f) = ix.face(id) else { continue };
                        match f.status {
                            Status::SuggestSure | Status::SuggestMaybe => {
                                job.steps.push(step(id, |db, id| people::answer_suggestion(db, id, true)));
                                changes.push((id, Status::Confirmed, f.person));
                            }
                            Status::Doubtful => {
                                job.steps.push(step(id, |db, id| people::apply_answer(db, id, Answer::Correct)));
                                changes.push((id, Status::Confirmed, f.suspect));
                            }
                            _ => continue,
                        }
                        job.faces.push(id);
                    }
                }
            }
        }
        Command::No | Command::NoName => {
            let no_name = matches!(c, Command::NoName);
            for (_, t) in &sel {
                match t.kind {
                    Kind::Person(_) => {}
                    Kind::Group(g) => {
                        let members: Vec<i64> = ix.faces.iter().filter(|f| f.group == Some(g) && f.status == Status::UnnamedGroup).map(|f| f.id).collect();
                        job.steps.push(step(g, |db, g| people::discard_group(db, g)));
                        changes.extend(members.iter().map(|id| (*id, Status::Rejected, None)));
                        job.faces.extend(members);
                    }
                    Kind::Face(id) => {
                        let Some(f) = ix.face(id) else { continue };
                        match (f.status, no_name) {
                            (Status::SuggestSure | Status::SuggestMaybe, false) => {
                                job.steps.push(step(id, |db, id| people::answer_suggestion(db, id, false)));
                                changes.push((id, Status::Unnamed, None));
                            }
                            // doubtful, "Nein": the entered name is right
                            (Status::Doubtful, false) => {
                                job.steps.push(step(id, |db, id| people::apply_answer(db, id, Answer::NameCorrect)));
                                changes.push((id, Status::Confirmed, f.person));
                            }
                            (Status::Confirmed | Status::Doubtful | Status::SuggestSure | Status::SuggestMaybe, _) => {
                                job.steps.push(step(id, |db, id| people::apply_answer(db, id, Answer::RemoveName)));
                                changes.push((id, Status::Unnamed, None));
                            }
                            (Status::Unnamed | Status::UnnamedGroup, _) => {
                                job.steps.push(step(id, |db, id| people::reject_face(db, id)));
                                changes.push((id, Status::Rejected, None));
                            }
                            (Status::Rejected, _) => continue,
                        }
                        job.faces.push(id);
                    }
                }
            }
        }
        Command::Name(name) => {
            let existing = ix.people.iter().find(|p| library::people::person_brief(&p.name) == library::people::person_brief(&name)).map(|p| p.id);
            if existing.is_none() {
                job.people_changed = true;
            }
            for (_, t) in &sel {
                match t.kind {
                    Kind::Person(p) => {
                        // rename; onto an existing other name: ask first (merge)
                        if let Some(e) = existing.filter(|e| *e != p) {
                            let n = ix.faces.iter().filter(|f| f.person == Some(p) && f.status == Status::Confirmed).count();
                            z.merge_question = Some((p, ix.name(e).to_string(), n));
                            return;
                        }
                        job.steps.push(step((p, name.clone()), |db, (id, n)| people::rename_person(db, id, &n).map(|_| ())));
                        job.person = Some(p);
                        job.people_changed = true;
                    }
                    Kind::Group(g) => {
                        let members: Vec<i64> = ix.faces.iter().filter(|f| f.group == Some(g) && f.status == Status::UnnamedGroup).map(|f| f.id).collect();
                        job.steps.push(step((g, name.clone()), |db, (id, n)| people::name_group(db, id, &n).map(|_| ())));
                        changes.extend(members.iter().map(|id| (*id, Status::Confirmed, existing)));
                        job.faces.extend(members);
                    }
                    Kind::Face(id) => {
                        job.steps.push(step((id, name.clone()), |db, (id, n)| people::apply_answer_with_name(db, id, &n)));
                        changes.push((id, Status::Confirmed, existing));
                        job.faces.push(id);
                    }
                }
            }
        }
    }
    if job.steps.is_empty() {
        return;
    }
    for (id, s, p) in changes {
        set_face(z, id, s, p);
    }
    write(z, ctx, job);
    // the next tile moves to the position of the first answered one
    z.dirty = true;
    rebuild(z);
    z.selection.clear();
    if let Some(i) = first.filter(|_| !z.built.tiles.is_empty()) {
        let i = i.min(z.built.tiles.len() - 1);
        z.selection.insert(i);
        z.anchor = Some(i);
    }
}

/// Start the comparison of Apple's unnamed groups with our people (once per opening and after changes): read-only on
/// Apple's database, in the background.
fn refresh_hints(z: &mut State, ctx: &egui::Context) {
    if let Some(h) = z.hints_in.lock().unwrap().take() {
        z.hints = Some(h);
    }
    if z.hints.is_some() || !crate::apple_photos_ui::an() || z.index.is_none() {
        return;
    }
    let Some(media) = crate::apple_photos_ui::media_library() else { return };
    if z.hints_busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let (out, busy, c, path) = (z.hints_in.clone(), z.hints_busy.clone(), ctx.clone(), z.db_path.clone());
    let spawned = std::thread::Builder::new().name("apple-groups".into()).spawn(move || {
        library::preview::low_priority_thread();
        let result = Db::read_only(&path).or_else(|_| Db::open(&path)).map_err(|e| e.to_string()).and_then(|db| library::apple_groups::read_groups(&media).and_then(|g| library::apple_groups::hints(&db.conn, &g)));
        match result {
            Ok(h) => *out.lock().unwrap() = Some(h),
            Err(e) => {
                crate::log(&format!("apple groups: {e}"));
                *out.lock().unwrap() = Some(Vec::new());
            }
        }
        busy.store(false, Ordering::SeqCst);
        c.request_repaint();
    });
    if spawned.is_err() {
        z.hints_busy.store(false, Ordering::SeqCst);
    }
}

/// Offer for an Apple group ("this Apple group seems to be X – assign all?") in the fixed line (right to left).
/// Nothing happens without a click.
fn apple_hint(ui: &mut egui::Ui, z: &State, person: Option<i64>) -> Option<Command> {
    let shown: Vec<&AppleHint> = z.hints.iter().flatten().filter(|h| !z.hints_dismissed.contains(&(h.group, h.person))).filter(|h| person.is_none_or(|p| p == h.person)).collect();
    let h = *shown.first()?;
    // too little room next to the commands of the person: no offer (it comes back with more room or another person)
    if ui.available_width() < 260.0 {
        return None;
    }
    let mut out = None;
    let later = ui.button(crate::tr("Später"));
    lr::hover_tip(ui, &later, crate::tr("Diesen Hinweis bis zum nächsten Öffnen ausblenden"));
    if later.clicked() {
        out = Some(Command::DismissGroup(h.group, h.person));
    }
    let r = ui.button(crate::trf!("{name} zuordnen", name = h.name));
    lr::hover_tip(ui, &r, &crate::trf!("{c} bestätigt, {s} als Vorschlag – oder in Apple Fotos benennen: graineo übernimmt den Namen beim nächsten Abgleich.", c = h.confirmed, s = h.suggested));
    if r.clicked() {
        out = Some(Command::AcceptGroup(h.clone()));
    }
    let more = if shown.len() > 1 { format!(" {}", crate::trf!("(+{n} weitere)", n = shown.len() - 1)) } else { String::new() };
    let text = format!("{}{more}", crate::trf!("Apple Fotos: Gruppe ohne Namen ({n} Fotos) passt zu {name}, {o} noch offen.", n = h.faces.len(), name = h.name, o = h.open));
    one_line(ui, &text, lr::TEXT);
    out
}

/// A small one-line text in the remaining width, shortened with "…"; the full text as tooltip.
fn one_line(ui: &mut egui::Ui, text: &str, color: Color32) {
    if ui.available_width() < 40.0 {
        return;
    }
    let r = ui.add(egui::Label::new(egui::RichText::new(text).small().color(color)).truncate().show_tooltip_when_elided(false).selectable(false));
    if r.intrinsic_size().is_some_and(|s| s.x > r.rect.width() + 0.5) {
        lr::hover_tip(ui, &r, text);
    }
}

// ───────────────────────────────────────────── Measuring aid

/// Tests: choose this face (once the faces are read), as a click on its tile.
#[cfg(test)]
pub fn choose_face_for_test(face: i64) {
    Z.with(|z| {
        if let Some(s) = z.borrow_mut().as_mut() {
            s.keep_face = Some(face);
            s.dirty = true;
        }
    });
}

/// Tests: the chosen face and whether its photo is shown large.
#[cfg(test)]
pub fn chosen_for_test() -> (Option<i64>, bool) {
    Z.with(|z| z.borrow().as_ref().filter(|s| s.is_open).map_or((None, false), |s| (chosen_face(s), s.big)))
}

/// Test hook `GRAINEO_PEOPLE_TEST="select=<n>;large=1;develop=1"` (only with `GRAINEO_DATA_DIR`, for screenshots):
/// once the faces are there, choose the n-th face tile (counted from 0, default the first), with `large=1` show its
/// photo large, with `develop=1` open it in Develop (`back=<s>` in `App::test_helpers` leads back).
fn test_hook(z: &mut State) -> Option<Command> {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.with(|d| d.get()) || z.index.is_none() || z.built.tiles.is_empty() || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return None;
    }
    let Ok(spec) = crate::env::var("GRAINEO_PEOPLE_TEST") else { return None };
    DONE.with(|d| d.set(true));
    let value = |k: &str| spec.split(';').find_map(|p| p.strip_prefix(k)?.strip_prefix('=').map(str::to_string));
    let n: usize = value("select").and_then(|v| v.parse().ok()).unwrap_or(0);
    let faces: Vec<usize> = z.built.tiles.iter().enumerate().filter(|(_, t)| matches!(t.kind, Kind::Face(_))).map(|(i, _)| i).collect();
    let Some(&i) = faces.get(n).or(faces.first()) else { return None };
    z.selection = HashSet::from([i]);
    z.anchor = Some(i);
    z.scroll_to = Some(i);
    z.big = value("large").as_deref() == Some("1");
    (value("develop").as_deref() == Some("1")).then_some(Command::OpenPhoto(i))
}

thread_local! {
    static CYCLE: std::cell::Cell<(f64, usize)> = const { std::cell::Cell::new((0.0, 0)) };
    static SCROLL: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

fn measuring_scroll() -> Option<f32> {
    SCROLL.with(|s| s.take())
}

/// Test hook `GRAINEO_PEOPLE_CYCLE=1` (only with `GRAINEO_DATA_DIR`, for measurements on a test library): goes through
/// the typical actions every 2 s – choose a status, search a name, answer, undo, open a stack, scroll, name a group –
/// and logs each step; the frame times come from `GRAINEO_FRAME_LOG`.
fn measuring_cycle(z: &mut State, ctx: &egui::Context) -> Option<Command> {
    let now = ctx.input(|i| i.time);
    ctx.request_repaint();
    let (next, k) = CYCLE.with(|c| c.get());
    if z.index.is_none() || now < next.max(6.0) || z.pending_writes > 0 {
        return None;
    }
    CYCLE.with(|c| c.set((now + 2.0, k + 1)));
    let most_questions = z.built.people.iter().filter(|c| c.value != NO_PERSON && c.open > 0).max_by_key(|c| c.open).map(|c| c.value);
    let mut cmd = None;
    let what = match k {
        0 => {
            z.filter = Filter::default();
            z.dirty = true;
            "standard view"
        }
        1 => {
            z.filter.status = BTreeSet::from([Status::SuggestSure, Status::SuggestMaybe]);
            z.dirty = true;
            "status: suggestions"
        }
        2 => {
            z.filter.status = BTreeSet::from([Status::UnnamedGroup]);
            z.dirty = true;
            "status: unnamed groups"
        }
        3 => {
            z.filter = Filter::default();
            if let Some(p) = most_questions {
                choose_person(z, p);
            }
            "name searched (person with most suggestions)"
        }
        4 => {
            z.selection = HashSet::from([1]);
            cmd = Some(Command::Yes);
            "yes on the first suggestion"
        }
        5 => {
            z.selection = HashSet::from([1, 2, 3]);
            cmd = Some(Command::No);
            "no on three suggestions"
        }
        6 => {
            cmd = Some(Command::Undo);
            "undo"
        }
        7 => {
            cmd = Some(Command::Open(0));
            "open the person stack"
        }
        8 => {
            SCROLL.with(|s| s.set(Some(20_000.0)));
            "scroll in the stack"
        }
        9 => {
            cmd = Some(Command::Back);
            "back"
        }
        10 => {
            z.filter = Filter::default();
            z.search.clear();
            z.filter.status = BTreeSet::from([Status::UnnamedGroup]);
            z.dirty = true;
            "groups"
        }
        11 => {
            z.selection = HashSet::from([0]);
            cmd = Some(Command::Name("Messperson".into()));
            "name a group"
        }
        12 => {
            z.filter = Filter::default();
            z.dirty = true;
            SCROLL.with(|s| s.set(Some(60_000.0)));
            "standard view, scroll"
        }
        13 => {
            z.filter.status = BTreeSet::from([Status::Unnamed]);
            z.dirty = true;
            "status: unnamed single"
        }
        _ => return None,
    };
    crate::log(&format!("people-measure: step {k}: {what}"));
    cmd
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    /// A list change in the People view: the short old list is still the content height egui knows in the frame of
    /// the jump, the target lies behind it. Without `with_placed_scroll` the rubber band pulls the view back and it
    /// coasts on; with it the target stays.
    fn jump_drift(protect: bool) -> f32 {
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 440.0));
        let mut time = 0.0;
        let mut content = 700.0_f32;
        let mut target: Option<f32> = None;
        let mut ys = Vec::new();
        for frame in 0..120 {
            time += 1.0 / 60.0;
            if frame == 6 {
                // new, much longer list and a target far behind the old end
                content = 9000.0;
                target = Some(5000.0);
            }
            let _ = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(rect), time: Some(time), ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let mut sa = egui::ScrollArea::vertical().id_salt("personen-raster").auto_shrink([false; 2]);
                    let placed = target.is_some();
                    if let Some(y) = target.take() {
                        sa = sa.vertical_scroll_offset(y);
                    }
                    // the content grows only after the jump frame, as with the rebuilt layout
                    let h = if frame <= 6 { 700.0 } else { content };
                    let out = if protect {
                        lr::with_placed_scroll(ui, placed, |ui| sa.show(ui, |ui| ui.set_height(h)))
                    } else {
                        sa.show(ui, |ui| ui.set_height(h))
                    };
                    ys.push(out.state.clamped_offset().y);
                });
            });
        }
        let settled = ys[9];
        ys[9..].iter().map(|y| (y - settled).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn list_change_target_behind_old_content_stays() {
        assert!(jump_drift(true) < 1.0, "view kept moving after the jump");
        eprintln!("unprotected drift: {}", jump_drift(false));
    }
}
