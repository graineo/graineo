//! Develop history of the open photo: every finished change
//! is a step with a name, ⌘Z / ⇧⌘Z move the cursor through the steps, and for photos of the library the steps are kept
//! there ([`library::develop_history`], written with the settings through [`library::store`]).
//!
//! - The current settings of the photo stay the only truth; the history is a companion list. Errors in it never change
//!   the settings or an export.
//! - A step is recognised by its difference (as before: a changed state once no mouse button is down). Commands
//!   (preset, film, paste, auto, reset, rotate, eyedropper …) leave a name hint ([`hint`]) the next step takes over;
//!   other steps are named after the fields that changed.
//! - Steps on the same sliders within [`MERGE_SECONDS`] are merged ("Exposure (3×)"). A reset by double-click
//!   ([`reset_gesture`]) and every command step stay steps of their own: nothing merges into them or they into a slider step.
//! - After a jump back and a new change the later steps fall away; an immediate ⌘Z brings them back with the cursor.
//! - Photos outside the library, or with the setting "Verlauf speichern" off: the history lives only in memory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use develop::{Settings, View};
use library::develop_history::{self as dh, kind, Op, Step};
use serde_json::{json, Value};

/// Steps on the same sliders closer than this (seconds) become one step.
pub const MERGE_SECONDS: i64 = 120;
/// A name hint is taken over only by a step recognised this soon after it.
const HINT_LIFE: Duration = Duration::from_millis(2500);
/// Histories of library photos kept in memory besides the open one (neighbours, recently visited).
const KEEP: usize = 8;

/// Identity of a photo or virtual copy in the history (independent of its index in the list).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub path: PathBuf,
    pub copy: Option<String>,
}

/// Steps that fell away after a jump back and a new change: an immediate ⌘Z brings them back.
#[derive(Clone, Debug)]
struct Dropped {
    /// steps up to and including the cursor at that time
    keep: usize,
    steps: Vec<Step>,
    /// the step recognised right after (while it is the newest and the cursor is on it, ⌘Z restores)
    after: i64,
}

/// History of one photo or copy.
#[derive(Debug)]
pub struct Photo {
    /// file of the library (the history is kept there); `None`: in memory only
    lib: Option<i64>,
    pub steps: Vec<Step>,
    /// index of the step whose state applies
    pub cursor: usize,
    /// state at the cursor (computed on demand)
    state: Option<Settings>,
    /// the newest step may take a following slider step on the same fields
    mergeable: bool,
    /// checked against the photo's settings since it was loaded
    checked: bool,
    dropped: Option<Dropped>,
    used: Instant,
    /// last change in Lightroom (a mismatch found when loading is then named "Aus Lightroom übernommen")
    lr_touch: Option<i64>,
    /// while the history is in memory the library's maintenance never compacts it (dropped with the photo)
    _hold: Option<dh::Hold>,
    /// Lightroom's own develop history (name, time), only shown (read from the catalog in the background)
    pub lr_steps: Vec<(String, i64)>,
    /// time of the last action on each step (`seq` → [`crate::undo_order::tick`]) and of the last step back: ⌘Z / ⇧⌘Z
    /// take the most recent action of all kinds
    stamps: HashMap<i64, u64>,
    back_stamp: u64,
}

impl Photo {
    fn new(lib: Option<i64>, steps: Vec<Step>, cursor: Option<i64>) -> Self {
        let cursor = dh::cursor_index(&steps, cursor);
        Photo { lib, steps, cursor, state: None, mergeable: false, checked: false, dropped: None, used: Instant::now(), lr_touch: None, _hold: None, lr_steps: Vec::new(), stamps: HashMap::new(), back_stamp: 0 }
    }

    /// Is the history kept in the library (photo of the library, setting on)?
    pub fn in_library(&self) -> bool {
        self.lib.is_some()
    }

    /// Changes recorded (steps without the start).
    #[cfg(test)]
    pub fn changes(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }

    fn at_top(&self) -> bool {
        self.cursor + 1 >= self.steps.len()
    }

    fn state_at(&self, i: usize) -> Option<Settings> {
        dh::state_at(&self.steps, i).and_then(|v| serde_json::from_value(v).ok())
    }

    /// State at the cursor (`None`: no steps, or a damaged history).
    fn cursor_state(&mut self) -> Option<&Settings> {
        if self.state.is_none() && !self.steps.is_empty() {
            self.state = self.state_at(self.cursor);
        }
        self.state.as_ref()
    }

    fn cursor_op(&self) -> Op {
        Op::Cursor((!self.at_top()).then(|| self.steps[self.cursor].seq))
    }

    /// Note the time of an action that moved the cursor from `old` to where it stands now (forward: the action on the
    /// step now under the cursor; back: an undo).
    fn stamp_move(&mut self, old: usize) {
        let t = crate::undo_order::tick();
        if self.cursor < old {
            self.back_stamp = t;
        } else if let Some(s) = self.steps.get(self.cursor) {
            self.stamps.insert(s.seq, t);
        }
    }

    fn next_seq(&self) -> i64 {
        self.steps.last().map_or(0, |s| s.seq + 1)
    }
}

/// What a command tells the next step about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub kind: &'static str,
    pub label: Value,
}

impl Hint {
    pub fn preset(name: &str) -> Self {
        Hint { kind: kind::PRESET, label: json!({ "k": "preset", "name": name }) }
    }
    /// A film (`look = false`) or a look/colour mode (`look = true`) from "Film & Look".
    pub fn film(name: &str, look: bool) -> Self {
        Hint { kind: kind::FILM, label: json!({ "k": if look { "look" } else { "film" }, "name": name }) }
    }
    /// Pasted, copied from photo `from` (`None`: not known).
    pub fn paste(from: Option<&str>) -> Self {
        Hint { kind: kind::PASTE, label: json!({ "k": "paste", "from": from }) }
    }
    /// Transferred from photo `from` (the main photo; "Apply to n photos": the active one, `relative` or equal).
    pub fn sync(from: &str, relative: Option<bool>) -> Self {
        Hint { kind: kind::SYNC, label: json!({ "k": "sync", "from": from, "rel": relative }) }
    }
    pub fn auto() -> Self {
        Hint { kind: kind::AUTO, label: json!({ "k": "auto" }) }
    }
    /// Automatic tone of a merged photo set or taken back ([`crate::hdr_auto`]). The step carries what the recipe
    /// held before and after it – strength of the automatic tone and the output (`true` = fusion) – so that ⌘Z brings
    /// them back together with the sliders ([`History::merge_recipe_at_cursor`]).
    pub fn merge_tone(strength: [f32; 2], fusion: [bool; 2]) -> Self {
        Hint { kind: kind::AUTO, label: json!({ "k": "merge_tone", "s": strength, "o": fusion }) }
    }
    pub fn reset() -> Self {
        Hint { kind: kind::RESET, label: json!({ "k": "reset" }) }
    }
    pub fn rotate(clockwise: bool) -> Self {
        Hint { kind: kind::CROP, label: json!({ "k": "rotate", "cw": clockwise }) }
    }
    pub fn white_balance_picker() -> Self {
        Hint { kind: kind::SLIDER, label: json!({ "k": "wb_picker" }) }
    }
}

/// A command for several photos as the history sees it: one id for all its steps, kind and name.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub id: i64,
    pub hint: Hint,
}

static COMMAND: AtomicU64 = AtomicU64::new(0);

impl Command {
    pub fn new(hint: Hint) -> Self {
        // unique across sessions: milliseconds, raised if two commands come in the same one
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let prev = COMMAND.fetch_max(ms, Ordering::SeqCst);
        let id = if prev >= ms { COMMAND.fetch_add(1, Ordering::SeqCst) + 1 } else { ms };
        Command { id: id as i64, hint }
    }
}

static HINT: Mutex<Option<(usize, Hint, Instant)>> = Mutex::new(None);

/// When a control was last reset by double-click (see [`reset_gesture`]).
static RESET_GESTURE: Mutex<Option<Instant>> = Mutex::new(None);

/// A control was reset to its default by double-click: the step recognised next is a step of its own, never merged
/// with a slider step before or after it (the dragged value stays reachable with ⌘Z).
pub fn reset_gesture() {
    *RESET_GESTURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// Was there a reset by double-click since the last step? (Taken once; old marks expire.)
pub fn take_reset_gesture() -> bool {
    RESET_GESTURE.lock().unwrap_or_else(|e| e.into_inner()).take().is_some_and(|t| t.elapsed() < HINT_LIFE)
}

/// Name of the photo whose settings were copied last ("Eingefügt von …").
static CLIPBOARD: Mutex<Option<String>> = Mutex::new(None);

pub fn copied_from(name: Option<String>) {
    *CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner()) = name;
}

pub fn clipboard_source() -> Option<String> {
    CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A command changes photo `index` (entry index): the next step recognised on it takes this name.
pub fn hint(index: usize, h: Hint) {
    *HINT.lock().unwrap_or_else(|e| e.into_inner()) = Some((index, h, Instant::now()));
}

fn take_hint(index: usize) -> Option<Hint> {
    let mut g = HINT.lock().unwrap_or_else(|e| e.into_inner());
    match g.take() {
        Some((i, h, t)) if i == index && t.elapsed() < HINT_LIFE => Some(h),
        Some(other) if other.2.elapsed() < HINT_LIFE => {
            *g = Some(other);
            None
        }
        _ => None,
    }
}

/// Generation of "delete all histories": the app drops what it holds in memory when it changes.
static CLEARED: AtomicU64 = AtomicU64::new(0);

/// Delete every history (the user's command in the settings, after a question): in the library (in the background)
/// and in memory.
pub fn delete_all(done: impl FnOnce(Result<usize, String>) + Send + 'static) {
    CLEARED.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let r = library::db::Db::standard().map_err(|e| e.to_string()).and_then(|db| library::store::forget_all_histories(&db));
        done(r);
    });
}

/// A loaded history: key, library file, steps, cursor, last change in Lightroom, hold, Lightroom's own steps.
type LoadResult = (Key, Option<i64>, Vec<Step>, Option<i64>, Option<i64>, Option<dh::Hold>, Vec<(String, i64)>);

/// Read the history of a photo: library photos with "Verlauf speichern" on from the library, else nothing (memory only).
/// `catalog`: also Lightroom's steps from the catalog (background only: it reads the catalog file).
fn read(key: &Key, catalog: bool) -> LoadResult {
    let Some((file, _)) = library::store::locate(&key.path) else { return (key.clone(), None, Vec::new(), None, None, None, Vec::new()) };
    let lr_steps = if catalog { library::store::lr_history(file, key.copy.as_deref()).unwrap_or_default() } else { Vec::new() };
    if !library::store::save_history() {
        return (key.clone(), None, Vec::new(), None, None, None, lr_steps);
    }
    // held before reading: the maintenance never compacts what is in memory
    let hold = dh::hold(file, key.copy.as_deref());
    let (steps, cursor) = library::store::load_history(file, key.copy.as_deref()).unwrap_or_default();
    let lr = library::store::lr_touch(file, key.copy.as_deref());
    (key.clone(), Some(file), steps, cursor, lr, Some(hold), lr_steps)
}

fn photo_of(r: LoadResult) -> (Key, Photo) {
    let (key, lib, steps, cursor, lr, hold, lr_steps) = r;
    let mut p = Photo::new(lib, steps, cursor);
    p.lr_touch = lr;
    p._hold = hold;
    p.lr_steps = lr_steps;
    (key, p)
}

/// Histories in memory and the background loader.
pub struct History {
    photos: HashMap<Key, Photo>,
    /// state of the step under the mouse in the list (taken by the hover preview of the next frame)
    hover: Option<Settings>,
    /// last computed state of a hovered step (key, index, its `seq` and `time`: a merged step changes)
    hover_cache: Option<(Key, usize, (i64, i64), Settings)>,
    /// "Mit aktuellem Stand vergleichen": the photo, the step's state and the name shown over it
    compare: Option<(Key, Settings, String)>,
    /// "Als Version speichern": source image, the step's state, name of the source and the step number (until the copy
    /// is in the library)
    pub(crate) version: Option<(i64, Settings, String, usize)>,
    ask: Option<Sender<Key>>,
    answers: Option<Receiver<LoadResult>>,
    asked: Vec<Key>,
    cleared: u64,
}

impl Default for History {
    fn default() -> Self {
        History { photos: HashMap::new(), hover: None, hover_cache: None, compare: None, version: None, ask: None, answers: None, asked: Vec::new(), cleared: CLEARED.load(Ordering::SeqCst) }
    }
}

/// The JSON form of settings as stored (through text, so numbers compare equal with what the library holds).
pub fn value(s: &Settings) -> Value {
    let s = Settings { view: View::Developed, ..s.clone() };
    serde_json::to_string(&s).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

fn now() -> i64 {
    library::now()
}

static MAINTENANCE: std::sync::Once = std::sync::Once::new();

/// Quiet background run (once at start, then every six hours): keeps the library's history table under
/// [`dh::MAX_TOTAL_BYTES`] by compacting the histories touched longest ago (never one touched in the last hour).
fn start_maintenance() {
    MAINTENANCE.call_once(|| {
        let _ = std::thread::Builder::new().name("verlauf-pflege".into()).spawn(|| {
            library::platform::background_priority();
            std::thread::sleep(Duration::from_secs(90));
            loop {
                if library::store::save_history() {
                    match library::db::Db::standard().and_then(|db| dh::enforce_total(&db.conn, dh::MAX_TOTAL_BYTES, now() - 3600)) {
                        Ok(0) => {}
                        Ok(n) => crate::log(&format!("develop history: {n} histories compacted (total limit)")),
                        Err(e) => crate::log(&format!("develop history: {e}")),
                    }
                }
                std::thread::sleep(Duration::from_secs(6 * 3600));
            }
        });
    });
}

impl History {
    /// Load the histories of these photos in the background (the open photo and its neighbours).
    pub fn request(&mut self, keys: impl IntoIterator<Item = Key>) {
        self.poll();
        start_maintenance();
        for key in keys {
            if self.photos.contains_key(&key) || self.asked.contains(&key) {
                continue;
            }
            if self.ask.is_none() {
                let (tx, rx) = channel::<Key>();
                let (atx, arx) = channel::<LoadResult>();
                let spawned = std::thread::Builder::new().name("verlauf-laden".into()).spawn(move || {
                    for key in rx {
                        if atx.send(read(&key, true)).is_err() {
                            break;
                        }
                    }
                });
                if spawned.is_err() {
                    return;
                }
                self.ask = Some(tx);
                self.answers = Some(arx);
            }
            if self.ask.as_ref().is_some_and(|a| a.send(key.clone()).is_ok()) {
                self.asked.push(key);
            }
        }
    }

    /// Take over loaded histories; drop everything after "delete all histories".
    pub fn poll(&mut self) {
        let c = CLEARED.load(Ordering::SeqCst);
        if c != self.cleared {
            self.cleared = c;
            self.photos.clear();
        }
        let Some(rx) = &self.answers else { return };
        while let Ok(r) = rx.try_recv() {
            let (key, p) = photo_of(r);
            self.asked.retain(|k| *k != key);
            match self.photos.get_mut(&key) {
                // loaded meanwhile in the foreground (without the catalog): Lightroom's steps come from here
                Some(have) if have.lr_steps.is_empty() => have.lr_steps = p.lr_steps,
                Some(_) => {}
                None => {
                    self.photos.insert(key, p);
                }
            }
        }
    }

    /// History of `key`, loaded right now if the loader has not delivered it yet (a read of a few rows).
    fn photo(&mut self, key: &Key) -> &mut Photo {
        self.poll();
        if !self.photos.contains_key(key) {
            let (_, p) = photo_of(read(key, false));
            self.photos.insert(key.clone(), p);
            self.trim(key);
        }
        let p = self.photos.get_mut(key).expect("inserted above");
        p.used = Instant::now();
        p
    }

    /// Keep few library histories in memory (they are in the library); memory-only ones stay for the session.
    fn trim(&mut self, open: &Key) {
        let mut lib: Vec<(Key, Instant)> = self.photos.iter().filter(|(k, p)| p.lib.is_some() && *k != open && p.dropped.is_none()).map(|(k, p)| (k.clone(), p.used)).collect();
        if lib.len() <= KEEP {
            return;
        }
        lib.sort_by_key(|(_, t)| *t);
        for (k, _) in lib.iter().take(lib.len() - KEEP) {
            self.photos.remove(k);
        }
    }

    /// Changes recorded for `key` (for the panel), 0 if not loaded.
    #[cfg(test)]
    pub fn changes(&self, key: &Key) -> usize {
        self.photos.get(key).map_or(0, Photo::changes)
    }

    /// Every history in memory: key, number of steps, cursor (tests: nothing changed).
    #[cfg(test)]
    pub fn snapshot(&self) -> std::collections::BTreeMap<(PathBuf, Option<String>), (usize, usize)> {
        self.photos.iter().map(|(k, p)| ((k.path.clone(), k.copy.clone()), (p.steps.len(), p.cursor))).collect()
    }

    pub fn get(&self, key: &Key) -> Option<&Photo> {
        self.photos.get(key)
    }

    /// Hand operations to the library (photos of the library with the setting on); they are written with the next
    /// save of the photo's settings.
    fn store(p: &Photo, key: &Key, ops: Vec<Op>) {
        if let Some(file) = p.lib.filter(|_| library::store::save_history()) {
            library::store::queue_history(file, key.copy.as_deref(), ops);
        }
    }

    /// The open photo is shown with the state `current`: a history loaded from the library is checked against it
    /// once (an `external` step if they differ). Then, if its settings were changed by a command that does not go
    /// through [`Self::record`] (a command for several photos, its ⌘Z, a rotation), the change becomes a step – or
    /// moves the cursor when it returns to the state of the neighbouring step.
    pub fn follow(&mut self, key: &Key, index: usize, current: &Settings) {
        self.follow_with(key, index, current, true);
    }

    fn follow_with(&mut self, key: &Key, index: usize, current: &Settings, use_hint: bool) {
        self.poll();
        let Some(p) = self.photos.get_mut(key) else { return };
        if !p.checked {
            p.checked = true;
            let mut cursor = p.cursor;
            let last = p.steps.get(cursor).map(|s| s.time);
            let mut ops = dh::reconcile(&mut p.steps, &mut cursor, &value(current), now());
            p.cursor = cursor;
            // changed in Lightroom since the last step (a new import of the catalog): named so, not "changed elsewhere"
            if let (Some(t), Some(last), Some(top)) = (p.lr_touch, last, p.steps.last_mut()) {
                if top.kind == kind::EXTERNAL && t >= last {
                    top.kind = kind::LIGHTROOM.into();
                    top.label = Some(json!({ "k": "lightroom" }).to_string());
                    let fixed = top.clone();
                    for op in &mut ops {
                        if let Op::Put(s) = op {
                            if s.seq == fixed.seq {
                                *s = fixed.clone();
                            }
                        }
                    }
                }
            }
            if !ops.is_empty() {
                p.state = None;
                p.dropped = None;
                Self::store(p, key, ops);
            }
        }
        if p.steps.is_empty() || p.cursor_state().is_none_or(|s| s == current) {
            return;
        }
        // only fields that are never stored differ (display toggles): nothing to record
        if p.cursor_state().is_some_and(|s| value(s) == value(current)) {
            p.state = Some(current.clone());
            return;
        }
        let neighbour = |p: &Photo, i: Option<usize>| i.filter(|&i| i < p.steps.len()).filter(|&i| p.state_at(i).as_ref() == Some(current));
        if let Some(i) = neighbour(p, p.cursor.checked_sub(1)).or_else(|| neighbour(p, Some(p.cursor + 1))) {
            let old = p.cursor;
            p.cursor = i;
            p.stamp_move(old);
            p.state = Some(current.clone());
            p.mergeable = false;
            let op = p.cursor_op();
            Self::store(p, key, vec![op]);
            return;
        }
        let before = p.cursor_state().cloned().unwrap_or_default();
        let h = use_hint.then(|| take_hint(index)).flatten().unwrap_or(Hint { kind: kind::EXTERNAL, label: json!({ "k": "external" }) });
        self.add(key, &before, current, Some(h));
    }

    /// A change of the open photo from `before` to `after` (both as stored, view `Developed`): a new step, or merged
    /// into the newest one. Returns a status text when later steps fell away.
    pub fn record(&mut self, key: &Key, index: usize, before: &Settings, after: &Settings) -> Option<String> {
        self.record_with(key, index, before, after, false)
    }

    /// Like [`Self::record`]; `own`: the change is a step of its own that never merges (a reset by double-click).
    pub fn record_with(&mut self, key: &Key, index: usize, before: &Settings, after: &Settings, own: bool) -> Option<String> {
        if value(before) == value(after) {
            return None;
        }
        // the loaded history first fits the state before the change (its hint belongs to this change)
        self.photo(key);
        self.follow_with(key, index, before, false);
        let h = take_hint(index);
        self.add_with(key, before, after, h, own)
    }

    fn add(&mut self, key: &Key, before: &Settings, after: &Settings, h: Option<Hint>) -> Option<String> {
        self.add_with(key, before, after, h, false)
    }

    fn add_with(&mut self, key: &Key, before: &Settings, after: &Settings, h: Option<Hint>, own: bool) -> Option<String> {
        let p = self.photo(key);
        let (bv, av) = (value(before), value(after));
        let t = now();
        let mut ops = Vec::new();
        let mut status = None;
        // a jump back, then a new change: the later steps fall away (an immediate ⌘Z brings them back)
        let mut dropped = None;
        if !p.at_top() {
            let keep = p.cursor + 1;
            let gone = p.steps.split_off(keep);
            ops.push(Op::TruncateAfter(p.steps[p.cursor].seq));
            status = Some(crate::trf!("{n} spätere Schritte verworfen – ⌘Z holt sie zurück", n = gone.len()));
            dropped = Some((keep, gone));
        }
        p.dropped = None;
        if p.steps.is_empty() {
            let lr = before.raw.from_lightroom;
            let start = Step { seq: 0, time: t, kind: kind::START.into(), label: Some(json!({ "k": "start", "lr": lr }).to_string()), patch: bv.clone(), count: 1, command: None };
            ops.push(Op::Put(start.clone()));
            p.steps.push(start);
            p.cursor = 0;
        }
        let (k, label) = match h {
            Some(h) => (h.kind, h.label),
            None => describe(before, after),
        };
        let fields = label.get("f").cloned();
        let top = p.steps.len() - 1;
        let merge = k == kind::SLIDER
            && !own
            && p.mergeable
            && top > 0
            && p.steps[top].kind == kind::SLIDER
            && fields.is_some()
            && step_label(&p.steps[top]).and_then(|l| l.get("f").cloned()) == fields
            && t - p.steps[top].time <= MERGE_SECONDS;
        if merge {
            let prev = dh::state_at(&p.steps, top - 1).unwrap_or_else(|| bv.clone());
            let patch = dh::diff(&prev, &av);
            if patch.as_object().is_some_and(|o| o.is_empty()) {
                // back where the merged step began: no step left
                p.steps.pop();
                ops.push(Op::TruncateAfter(p.steps[top - 1].seq));
                p.mergeable = false;
            } else {
                let s = &mut p.steps[top];
                // the values of the merged parts (tooltip of "(3×)")
                let mut label = label;
                let old = step_label(s).unwrap_or(Value::Null);
                let mut parts: Vec<Value> = old.get("h").and_then(Value::as_array).cloned().unwrap_or_default();
                if let Some(v) = old.get("v").and_then(|v| v.get(0)) {
                    parts.push(v.clone());
                }
                if !parts.is_empty() {
                    label["h"] = Value::Array(parts);
                }
                s.patch = patch;
                s.count += 1;
                s.time = t;
                s.label = Some(label.to_string());
                ops.push(Op::Put(s.clone()));
            }
        } else {
            let step = Step { seq: p.next_seq(), time: t, kind: k.into(), label: Some(label.to_string()), patch: dh::diff(&bv, &av), count: 1, command: None };
            ops.push(Op::Put(step.clone()));
            p.steps.push(step);
            // a reset by double-click and command steps take no later slider step
            p.mergeable = k == kind::SLIDER && !own;
        }
        p.cursor = p.steps.len() - 1;
        p.state = Some(Settings { view: View::Developed, ..after.clone() });
        if let Some(st) = p.steps.last() {
            p.stamps.insert(st.seq, crate::undo_order::tick());
        }
        ops.push(Op::Cursor(None));
        let compacted = dh::compact(&p.steps, p.cursor, dh::MAX_STEPS, t);
        if let Some((c, ci)) = compacted.clone() {
            p.steps = c;
            p.cursor = ci;
            ops.push(Op::ReplaceAll(p.steps.clone()));
        }
        if let Some((keep, steps)) = dropped.filter(|_| compacted.is_none()) {
            p.dropped = Some(Dropped { keep, steps, after: p.steps[p.cursor].seq });
        }
        let snapshot: &Photo = p;
        Self::store(snapshot, key, ops);
        status
    }

    /// The step compared with the current state for `key` (left side of the comparison).
    pub fn compare_for(&self, key: &Key) -> Option<(Settings, String)> {
        self.compare.as_ref().filter(|(k, ..)| k == key).map(|(_, s, l)| (s.clone(), l.clone()))
    }

    pub fn set_compare(&mut self, key: &Key, state: Settings, label: String) {
        self.compare = Some((key.clone(), state, label));
    }

    /// When the action ⌘Z would take back on `key` was made (`None`: nothing to take back). Steps from an earlier
    /// session count with their own time.
    pub fn undo_stamp(&self, key: &Key) -> Option<u64> {
        let p = self.photos.get(key)?;
        let restorable = p.dropped.as_ref().is_some_and(|d| p.at_top() && p.steps.last().is_some_and(|s| s.seq == d.after));
        if p.cursor == 0 && !restorable {
            return None;
        }
        let s = p.steps.get(p.cursor)?;
        Some(p.stamps.get(&s.seq).copied().unwrap_or((s.time.max(0) as u64) * 1000))
    }

    /// Command id of the step under the cursor of `key` (the one ⌘Z would take back) and of the step after it.
    pub fn command_at_cursor(&self, key: &Key) -> (Option<i64>, Option<i64>) {
        let Some(p) = self.photos.get(key) else { return (None, None) };
        (p.steps.get(p.cursor).and_then(|s| s.command), p.steps.get(p.cursor + 1).and_then(|s| s.command))
    }

    /// When the step ⇧⌘Z would bring back on `key` was taken back (`None`: nothing to redo).
    pub fn redo_stamp(&self, key: &Key) -> Option<u64> {
        let p = self.photos.get(key)?;
        (p.cursor + 1 < p.steps.len()).then_some(p.back_stamp)
    }

    /// Is the history of `key` held in memory (open photo, neighbours, memory-only photos)? Commands for several
    /// photos record such photos here and all others directly in the library.
    pub fn in_memory(&self, key: &Key) -> bool {
        self.photos.contains_key(key)
    }

    /// A command for several photos changed `key` from `before` to `after` (or took it back, `undo`): its step in
    /// memory (`false`: not in memory, the caller records it in the library).
    pub fn command_step(&mut self, key: &Key, before: &Settings, after: &Settings, cmd: &Command, undo: bool) -> bool {
        self.poll();
        let Some(p) = self.photos.get_mut(key) else { return false };
        let mut cur = (!p.at_top()).then(|| p.steps[p.cursor].seq);
        let ops = dh::plan_command(&p.steps, cur, &value(before), &value(after), cmd.hint.kind, &cmd.hint.label, cmd.id, undo, now());
        let old = p.cursor;
        dh::apply_ops_mem(&mut p.steps, &mut cur, &ops);
        p.cursor = dh::cursor_index(&p.steps, cur);
        p.stamp_move(old);
        p.state = None;
        p.mergeable = false;
        p.dropped = None;
        p.checked = true;
        Self::store(p, key, ops);
        true
    }

    /// "Verlauf bis hier zusammenfassen": the steps 1 ..= `i` become one.
    pub fn compact_to(&mut self, key: &Key, i: usize) -> bool {
        let Some(p) = self.photos.get_mut(key) else { return false };
        let Some((c, ci)) = dh::compact_to(&p.steps, p.cursor, i) else { return false };
        p.steps = c;
        p.cursor = ci;
        p.state = None;
        p.mergeable = false;
        p.dropped = None;
        let ops = vec![Op::ReplaceAll(p.steps.clone()), p.cursor_op()];
        Self::store(p, key, ops);
        true
    }

    /// "Verlauf dieses Fotos löschen": the steps go (memory and library); the next change starts anew.
    pub fn clear_photo(&mut self, key: &Key) {
        if let Some(p) = self.photos.get_mut(key) {
            p.steps.clear();
            p.cursor = 0;
            p.state = None;
            p.mergeable = false;
            p.dropped = None;
            Self::store(p, key, vec![Op::Clear]);
        }
    }

    /// State at step `i` of `key` (for a comparison or a new version), fields never stored taken from `current`.
    pub fn state_of(&self, key: &Key, i: usize, current: &Settings) -> Option<Settings> {
        self.photos.get(key)?.state_at(i).map(|s| with_unsaved(s, current))
    }

    /// A new virtual copy made from a step: its history begins with that state ("Aus dem Verlauf von …, Schritt n").
    pub fn start_version(&mut self, key: &Key, state: &Settings, from: &str, step: usize) {
        let (_, mut p) = photo_of(read(key, false));
        let start = Step {
            seq: 0,
            time: now(),
            kind: kind::START.into(),
            label: Some(json!({ "k": "start", "from": from, "n": step }).to_string()),
            patch: value(state),
            count: 1,
            command: None,
        };
        let ops = vec![Op::Clear, Op::Put(start.clone())];
        p.steps = vec![start];
        p.cursor = 0;
        p.checked = true;
        Self::store(&p, key, ops);
        self.photos.insert(key.clone(), p);
    }

    /// Jump to step `target` (click in the list, "Original" = 0): the cursor moves, no step is added; the later
    /// steps stay (dimmed) until the next change. Returns the state to apply (`None`: nothing to do).
    pub fn jump(&mut self, key: &Key, index: usize, current: &Settings, target: usize) -> Option<Settings> {
        self.photo(key);
        self.follow_with(key, index, current, false);
        let p = self.photos.get_mut(key)?;
        if target >= p.steps.len() || target == p.cursor {
            return None;
        }
        let s = with_unsaved(p.state_at(target)?, current);
        let old = p.cursor;
        p.cursor = target;
        p.stamp_move(old);
        p.state = Some(s.clone());
        p.mergeable = false;
        p.dropped = None;
        let op = p.cursor_op();
        Self::store(p, key, vec![op]);
        self.hover = None;
        Some(s)
    }

    /// The list shows step `i` of `key` under the mouse: its state becomes the hover preview (`None`: no step hovered).
    pub fn set_hover(&mut self, key: &Key, i: Option<usize>, current: &Settings) {
        let Some(i) = i else {
            self.hover = None;
            return;
        };
        let Some(p) = self.photos.get(key) else { return };
        let Some(step) = p.steps.get(i) else { return };
        let tag = (step.seq, step.time);
        if let Some((k, j, t, s)) = &self.hover_cache {
            if k == key && *j == i && *t == tag {
                self.hover = Some(s.clone());
                return;
            }
        }
        let Some(s) = p.state_at(i).map(|s| with_unsaved(s, current)) else { return };
        self.hover_cache = Some((key.clone(), i, tag, s.clone()));
        self.hover = Some(s);
    }

    /// The hovered step's state for the preview (once; the list sets it again every frame while hovered).
    pub fn take_hover(&mut self) -> Option<Settings> {
        self.hover.take()
    }

    /// Merged photo: what its recipe held as of the step under the cursor – strength of the automatic tone, output
    /// (`true` = fusion) – and the state the automatic left (its five sliders are the "applied" values). From the
    /// newest step of the automatic ([`Hint::merge_tone`]) up to the cursor, else from before the first one after it.
    /// `from`: where the cursor stood before it moved. `None` unless the move crossed a step of the automatic (other
    /// steps leave the recipe alone: it may have changed since without a step, e.g. the output with sliders set by
    /// hand).
    pub fn merge_recipe_at_cursor(&self, key: &Key, from: usize) -> Option<(f32, bool, Settings)> {
        let p = self.photos.get(key)?;
        let fact = |i: usize| step_label(p.steps.get(i)?).filter(|l| l.get("k").and_then(Value::as_str) == Some("merge_tone")).map(|l| (l, i));
        (from.min(p.cursor) + 1..=from.max(p.cursor)).find_map(fact)?;
        let (label, side, state) = match (0..=p.cursor).rev().find_map(fact) {
            Some((l, i)) => (l, 1, i),
            None => {
                let (l, i) = (p.cursor + 1..p.steps.len()).find_map(fact)?;
                (l, 0, i.checked_sub(1)?)
            }
        };
        let strength = label.get("s")?.get(side)?.as_f64()? as f32;
        let fusion = label.get("o")?.get(side)?.as_bool()?;
        Some((strength, fusion, p.state_at(state)?))
    }

    /// Forget the history in memory of the photo at `path` and its copies (another photo has this path now).
    pub fn forget_path(&mut self, path: &std::path::Path) {
        self.photos.retain(|k, _| k.path != path);
    }

    /// Index of the step under the cursor (`None`: the history is not in memory).
    pub fn cursor_of(&self, key: &Key) -> Option<usize> {
        self.photos.get(key).map(|p| p.cursor)
    }

    /// ⌘Z (`redo = false`) / ⇧⌘Z: move the cursor one step. Returns the state to apply (`None`: nothing to do).
    pub fn undo(&mut self, key: &Key, index: usize, current: &Settings, redo: bool) -> Option<Settings> {
        self.photo(key);
        self.follow_with(key, index, current, false);
        let p = self.photos.get_mut(key)?;
        if !redo {
            // the later steps that just fell away come back, with the cursor where it stood
            if let Some(d) = p.dropped.take().filter(|d| p.at_top() && p.steps.last().is_some_and(|s| s.seq == d.after) && d.keep <= p.steps.len()) {
                p.steps.truncate(d.keep);
                let mut ops = vec![Op::TruncateAfter(p.steps[d.keep - 1].seq)];
                for s in &d.steps {
                    ops.push(Op::Put(s.clone()));
                }
                p.steps.extend(d.steps);
                p.cursor = d.keep - 1;
                p.back_stamp = crate::undo_order::tick();
                p.state = None;
                p.mergeable = false;
                ops.push(p.cursor_op());
                let s = p.cursor_state().cloned().map(|s| with_unsaved(s, current));
                Self::store(p, key, ops);
                return s;
            }
        }
        let target = if redo { (p.cursor + 1 < p.steps.len()).then_some(p.cursor + 1) } else { p.cursor.checked_sub(1) };
        let i = target?;
        let s = with_unsaved(p.state_at(i)?, current);
        let old = p.cursor;
        p.cursor = i;
        p.stamp_move(old);
        p.state = Some(s.clone());
        p.mergeable = false;
        p.dropped = None;
        let op = p.cursor_op();
        Self::store(p, key, vec![op]);
        Some(s)
    }
}

impl crate::App {
    /// A command for several photos changed entry `i` from `before` to `after` (`undo`: took it back): the step goes
    /// into the history in memory, or else straight into the library (written with the photo's settings).
    pub(crate) fn history_command(&mut self, i: usize, before: &Settings, after: &Settings, cmd: &Command, undo: bool) {
        let key = self.history_key(i);
        if self.history.command_step(&key, before, after, cmd, undo) {
            return;
        }
        if let Some((file, _)) = library::store::locate(&key.path) {
            library::store::command_step(file, key.copy.as_deref(), &value(before), &value(after), cmd.hint.kind, &cmd.hint.label, cmd.id, undo);
        }
    }
}

/// Test hook (own data folder only): `GRAINEO_HISTORY_TEST=edit` changes the open photo three times (exposure twice,
/// merged into one step, then contrast) and takes the last step back with ⌘Z; `=show` only reports. Both write the
/// history of the open photo to the log ("history test: …"). `=panel` makes five steps (one merged, one preset, a crop)
/// and jumps back to the third (for screenshots of the list; `GRAINEO_HISTORY_HOVER=<step>` hovers a step); `=compare`
/// compares step 0 with the current state; `=version` saves step 3 as a version and reports the copy's history.
impl crate::App {
    pub(crate) fn history_test_hook(&mut self) {
        use std::sync::atomic::AtomicU8;
        static STAGE: AtomicU8 = AtomicU8::new(0);
        static AT: Mutex<f64> = Mutex::new(0.0);
        let Some(spec) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_HISTORY_TEST").ok()) else { return };
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let now = self.now();
        self.worker.ctx().request_repaint_after(Duration::from_millis(200));
        let mut at = AT.lock().unwrap_or_else(|e| e.into_inner());
        let stage = STAGE.load(Ordering::Relaxed);
        if stage == 0 {
            if now - self.shown_at < 2.0 {
                return;
            }
            *at = now;
        } else if now - *at < 1.0 {
            return;
        }
        *at = now;
        let report = |app: &Self, what: &str| {
            let key = app.history_key(index);
            let line = match app.history.photos.get(&key) {
                Some(p) => format!(
                    "history test ({what}): {} steps, cursor {}, library {}: {}",
                    p.steps.len(),
                    p.cursor,
                    p.lib.is_some(),
                    p.steps.iter().map(|s| format!("{}{}", step_name(s), if s.count > 1 { format!(" ({}×)", s.count) } else { String::new() })).collect::<Vec<_>>().join(" | ")
                ),
                None => format!("history test ({what}): not loaded"),
            };
            crate::log(&line);
            eprintln!("{line}");
        };
        match (spec.as_str(), stage) {
            ("edit", 0) => self.entries[index].settings.exposure += 0.25,
            ("edit", 1) => self.entries[index].settings.exposure += 0.25,
            ("edit", 2) => self.entries[index].settings.contrast += 0.2,
            ("edit", 3) => {
                report(self, "before undo");
                self.undo(false);
            }
            ("panel", 0 | 1) => self.entries[index].settings.exposure += 0.25,
            ("panel", 2) => self.entries[index].settings.contrast += 0.2,
            ("panel", 3) => {
                hint(index, Hint::preset("Kontrastreich"));
                self.entries[index].settings.vibrance += 0.3;
                self.entries[index].settings.clarity += 0.2;
            }
            ("panel", 4) => self.entries[index].settings.crop = [0.5, 0.5, 0.85, 0.85],
            ("panel", 5) => {
                report(self, "before jump");
                let key = self.history_key(index);
                let current = Settings { view: View::Developed, ..self.entries[index].settings.clone() };
                if let Some(t) = self.history.jump(&key, index, &current, 2) {
                    let view = self.entries[index].settings.view;
                    self.entries[index].settings = Settings { view, ..t.clone() };
                    self.committed = Some((index, t));
                }
                report(self, "after jump");
                STAGE.store(9, Ordering::Relaxed);
                return;
            }
            ("compare", 0) => {
                let key = self.history_key(index);
                let current = Settings { view: View::Developed, ..self.entries[index].settings.clone() };
                if let Some(st) = self.history.state_of(&key, 0, &current) {
                    self.history.set_compare(&key, st, crate::trf!("Verlauf · {name}", name = crate::tr("Stand vor dem Verlauf")));
                    self.compare_with_history_step();
                }
                report(self, "compare");
                STAGE.store(9, Ordering::Relaxed);
                return;
            }
            ("version", 0) => {
                let current = Settings { view: View::Developed, ..self.entries[index].settings.clone() };
                self.save_as_version(index, 3, "Kontrastreich".into(), &current);
            }
            ("version", 1..=3) => {}
            (_, 4) | ("show", 0) => {
                report(self, if spec == "show" { "loaded" } else { "after undo" });
                STAGE.store(9, Ordering::Relaxed);
                return;
            }
            _ => return,
        }
        STAGE.store(stage + 1, Ordering::Relaxed);
    }
}

/// `s` with the fields that are never stored (display toggles, export size, Lightroom's upright mode) taken from
/// `current`: a step brings back what is stored, nothing else.
fn with_unsaved(mut s: Settings, current: &Settings) -> Settings {
    s.tone_eq_mask = current.tone_eq_mask;
    s.view_rescue = current.view_rescue;
    s.output_size = current.output_size;
    s.raw.upright_from_lr = current.raw.upright_from_lr;
    s
}

fn step_label(s: &Step) -> Option<Value> {
    serde_json::from_str(s.label.as_deref()?).ok()
}

// ─────────────────────────────────────────────── names of the steps

/// Fields (JSON keys of the settings, nested ones of `raw`, `film` and `bypass` with a dot) and the name of their
/// slider or section (German text = translation key). `true`: the value is shown with a sign.
const FIELDS: &[(&str, &str, bool)] = &[
    ("exposure", "Belichtung", true),
    ("contrast", "Kontrast", true),
    ("highlight", "Lichter", true),
    ("shadow", "Tiefen", true),
    ("fill_light", "Fill Light", true),
    ("dehaze", "Dunst entfernen", true),
    ("vibrance", "Dynamik", true),
    ("saturation", "Sättigung", true),
    ("clarity", "Klarheit", true),
    ("fade", "Matte Tiefen", false),
    ("vignette", "Vignette", true),
    ("vignette_mid", "Vignette", false),
    ("grain_amount", "Korn", false),
    ("grain_size", "Korngröße", false),
    ("grain_rough", "Korn", false),
    ("sharpen", "Schärfen", false),
    ("sharpen_radius", "Schärfen", false),
    ("sharpen_mask", "Schärfen", false),
    ("sigma_sharpness", "Schärfen", true),
    ("nr_luma", "Rauschreduzierung", false),
    ("nr_chroma", "Rauschreduzierung", false),
    ("quattro_detail", "Rauschreduzierung", false),
    ("denoise", "Rauschreduzierung", false),
    ("monochrome", "Schwarzweiß", false),
    ("mix", "Schwarzweiß", false),
    ("gray_mix", "Schwarzweiß", false),
    ("color_mode", "Profil", false),
    ("profile_amount", "Intensität", false),
    ("layer_matrix", "Schichtmischer", false),
    ("highlight_neutralize", "Lichter neutral", false),
    ("highlight_colour", "Lichter-Farbe", false),
    ("highlight_structure", "Lichter-Struktur", false),
    ("hsl", "Farbmischer", false),
    ("tone_curve", "Gradationskurve", false),
    ("toneEqualizer", "Tonwert-Entzerrer", false),
    ("grade_shadows", "Color Grading", false),
    ("grade_midtones", "Color Grading", false),
    ("grade_highlights", "Color Grading", false),
    ("grade_balance", "Color Grading", true),
    ("crop", "Freistellen", false),
    ("crop_angle", "Winkel", true),
    ("perspective_v", "Perspektive", true),
    ("perspective_h", "Perspektive", true),
    ("rotate", "Drehen", false),
    ("defringe_purple_amount", "Farbsäume", false),
    ("defringe_purple_hue", "Farbsäume", false),
    ("defringe_green_amount", "Farbsäume", false),
    ("defringe_green_hue", "Farbsäume", false),
    ("spp_defringe", "Farbsäume", false),
    ("ca_mode", "Objektivkorrekturen", false),
    ("ca_profile", "Objektivkorrekturen", false),
    ("ca_red", "Objektivkorrekturen", false),
    ("ca_blue", "Objektivkorrekturen", false),
    ("white_balance", "Weißabgleich", false),
    ("wb_mult", "Weißabgleich", false),
    ("wb_temp", "Temperatur", true),
    ("wb_tint", "Tönung", true),
    ("local", "Lokale Korrekturen", false),
    ("local.name", "Maske umbenannt", false),
    ("local.enabled", "Maske ein/aus", false),
    ("local.amount", "Stärke der Maske", false),
    ("local.refine", "Kante verfeinern", false),
    ("local.order", "Reihenfolge der Masken", false),
    ("spots", "Retusche", false),
    ("red_eyes", "Rote Augen", false),
    ("removals", "Objekte entfernen", false),
    ("raw.wb_mode", "Weißabgleich", false),
    ("raw.temperature", "Temperatur", false),
    ("raw.tint", "Tönung", true),
    ("raw.whites", "Weiß", true),
    ("raw.blacks", "Schwarz", true),
    ("raw.texture", "Struktur", true),
    ("raw.nr_luma", "Rauschreduzierung", false),
    ("raw.nr_luma_detail", "Rauschreduzierung", false),
    ("raw.nr_color", "Rauschreduzierung", false),
    ("raw.nr_color_detail", "Rauschreduzierung", false),
    ("raw.false_color", "Rauschreduzierung", false),
    ("raw.aiDenoise", "KI-Entrauschen", false),
    ("raw.calibration", "Kalibrierung", false),
    ("raw.lens", "Objektivkorrekturen", false),
    ("raw.lens_distortion", "Objektivkorrekturen", false),
    ("raw.lens_vignette", "Objektivkorrekturen", false),
    ("raw.lens_ca", "Objektivkorrekturen", false),
    ("raw.lens_image", "Objektivkorrekturen", false),
    ("raw.lens_profile", "Objektivkorrekturen", false),
    ("raw.engine", "Pipeline", false),
    ("raw.look", "Profil", false),
    ("raw.lookName", "Profil", false),
    ("raw.lookPackage", "Profil", false),
    ("film.active", "Film", false),
    ("film.stock", "Film", false),
    ("film.paper", "Papier", false),
    ("film.exposure", "Filmbelichtung", true),
    ("film.print_exposure", "Abzughelligkeit", true),
    ("film.grain", "Korn", false),
    ("film.grain_size", "Korngröße", false),
    ("film.halation", "Lichthof", false),
    ("film.film_gamma", "Filmkontrast", false),
];

fn field(f: &str) -> Option<(&'static str, bool)> {
    FIELDS.iter().find(|(k, ..)| *k == f).map(|(_, n, s)| (*n, *s))
}

/// Defaults of a nested section of the settings (`raw`, `film`, `bypass`) as stored.
fn section_defaults(k: &str) -> serde_json::Map<String, Value> {
    let text = match k {
        "raw" => serde_json::to_string(&develop::settings::RawAdjust::default()),
        "film" => serde_json::to_string(&develop::film::FilmSettings::default()),
        _ => serde_json::to_string(&develop::settings::Bypass::default()),
    };
    text.ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// What changed in the local corrections, when it is one thing of the mask list: the name, on/off, the strength or
/// the order of the masks (`local.name` …); otherwise `local`.
fn local_change(a: Option<&Value>, b: Option<&Value>) -> &'static str {
    let layers = |v: Option<&Value>| v.and_then(|v| v.get("layers")).and_then(Value::as_array).cloned().unwrap_or_default();
    let (la, lb) = (layers(a), layers(b));
    if la.len() != lb.len() || la.is_empty() {
        return "local";
    }
    // the same masks in another order
    if la.len() > 1 && la.iter().all(|l| lb.contains(l)) && lb.iter().all(|l| la.contains(l)) {
        return "local.order";
    }
    let strip = |l: &Value, key: &str| {
        let mut l = l.clone();
        if let Some(o) = l.as_object_mut() {
            o.remove(key);
        }
        l
    };
    let diff: Vec<(&Value, &Value)> = la.iter().zip(&lb).filter(|(x, y)| x != y).collect();
    if let [(x, y)] = diff.as_slice() {
        for (key, name) in [("name", "local.name"), ("enabled", "local.enabled"), ("amount", "local.amount"), ("refine", "local.refine")] {
            if strip(x, key) == strip(y, key) {
                return name;
            }
        }
    }
    "local"
}

/// Fields that differ between two stored states (`raw`, `film`, `bypass` one level deeper).
fn changed_fields(a: &Value, b: &Value) -> Vec<String> {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else { return Vec::new() };
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        let (x, y) = (a.get(k), b.get(k));
        if x == y {
            continue;
        }
        match (k.as_str(), x.and_then(Value::as_object), y.and_then(Value::as_object)) {
            ("local", _, _) => out.push(local_change(x, y).to_string()),
            ("raw" | "film" | "bypass", xo, yo) if xo.is_some() || yo.is_some() => {
                // a section left out of the stored JSON is at its defaults (`raw` while untouched)
                let defaults = section_defaults(k);
                let (xo, yo) = (xo.unwrap_or(&defaults), yo.unwrap_or(&defaults));
                let mut sub: Vec<&String> = xo.keys().chain(yo.keys()).collect();
                sub.sort();
                sub.dedup();
                for s in sub {
                    if xo.get(s) != yo.get(s) {
                        out.push(format!("{k}.{s}"));
                    }
                }
            }
            _ => out.push(k.clone()),
        }
    }
    // in the order of the panel
    out.sort_by_key(|f| FIELDS.iter().position(|(k, ..)| k == f).unwrap_or(usize::MAX));
    out
}

/// Kind and name (key and values, translated when shown) of a change recognised by its difference.
pub fn describe(before: &Settings, after: &Settings) -> (&'static str, Value) {
    let (a, b) = (value(before), value(after));
    let fields = changed_fields(&a, &b);
    let only = |set: &[&str]| !fields.is_empty() && fields.iter().all(|f| set.contains(&f.as_str()));
    let k = if fields.iter().any(|f| f == "local" || f.starts_with("local.")) {
        kind::MASK
    } else if only(&["spots", "red_eyes", "removals"]) {
        kind::RETOUCH
    } else if only(&["crop", "crop_angle", "perspective_v", "perspective_h", "rotate"]) {
        kind::CROP
    } else {
        kind::SLIDER
    };
    let mut label = json!({ "k": k, "f": fields });
    if let [f] = fields.as_slice() {
        let path: Vec<&str> = f.split('.').collect();
        let v = path.iter().try_fold(&b, |v, p| v.get(*p));
        if let Some(n) = v.and_then(Value::as_f64) {
            label["v"] = json!([n]);
        }
    }
    // fields without a name of their own: the sections that changed (as "Paste" knows them)
    if fields.iter().any(|f| field(f).is_none()) {
        let groups: Vec<&str> = crate::paste::GROUPS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| crate::paste::merge(before, after, &crate::paste::Groups::from_names([*n])) != *before)
            .collect();
        if !groups.is_empty() {
            label["g"] = json!(groups);
        }
    }
    (k, label)
}

fn number(v: f64, signed: bool) -> String {
    let lang = crate::menu::i18n::language();
    let places = if v.fract() == 0.0 && v.abs() >= 1.0 { 0 } else { 2 };
    let t = crate::menu::i18n::format::decimal(lang, v.abs(), places);
    match (signed, v < 0.0) {
        (_, true) => format!("−{t}"),
        (true, false) => format!("+{t}"),
        (false, false) => t,
    }
}

/// Displayed name of a step (in the language of the interface).
pub fn step_name(s: &Step) -> String {
    let l = step_label(s).unwrap_or(Value::Null);
    let str_of = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match l.get("k").and_then(Value::as_str).unwrap_or(&s.kind) {
        "start" if !str_of("from").is_empty() => crate::trf!("Aus dem Verlauf von {name}, Schritt {n}", name = str_of("from"), n = l.get("n").and_then(Value::as_i64).unwrap_or(0)),
        "start" if l.get("lr").and_then(Value::as_bool) == Some(true) => crate::tr("Ausgangszustand (Lightroom)").to_string(),
        "start" => crate::tr("Stand vor dem Verlauf").to_string(),
        "external" => crate::tr("Außerhalb geändert").to_string(),
        "compacted" => crate::tr("Ältere Schritte zusammengefasst").to_string(),
        "preset" => crate::trf!("Vorgabe: {name}", name = crate::tr(&str_of("name"))),
        "film" => crate::trf!("Film: {name}", name = str_of("name")),
        "look" => crate::trf!("Look: {name}", name = crate::tr(&str_of("name"))),
        "paste" if !str_of("from").is_empty() => crate::trf!("Eingefügt von {name}", name = str_of("from")),
        "paste" => crate::tr("Einstellungen eingefügt").to_string(),
        "sync" if l.get("rel").and_then(Value::as_bool) == Some(true) => crate::trf!("Übertragen von {name} (relativ)", name = str_of("from")),
        "sync" if !str_of("from").is_empty() => crate::trf!("Übertragen von {name}", name = str_of("from")),
        "sync" => crate::tr("Einstellungen übertragen").to_string(),
        "undo" => {
            let inner = Step { label: l.get("of").map(|v| v.to_string()), ..s.clone() };
            crate::trf!("Rückgängig: {name}", name = step_name(&inner))
        }
        "auto" => crate::tr("Auto").to_string(),
        "merge_tone" => {
            let pair = |k: &str| l.get(k).and_then(Value::as_array).filter(|a| a.len() == 2).cloned();
            match (pair("o"), pair("s")) {
                (Some(o), _) if o[0] != o[1] => crate::trf!("Ausgabe: {name}", name = crate::tr(if o[1].as_bool() == Some(true) { "Fusion" } else { "Linear" })),
                (_, Some(s)) if s[0] != s[1] => crate::trf!("Stärke {n} %", n = (s[1].as_f64().unwrap_or(0.0) * 100.0).round() as i64),
                _ => crate::tr("Auto").to_string(),
            }
        }
        "reset" => crate::tr("Zurücksetzen").to_string(),
        "rotate" => crate::tr("Drehen").to_string(),
        "wb_picker" => crate::tr("Weißabgleich-Pipette").to_string(),
        "lightroom" => crate::tr("Aus Lightroom übernommen").to_string(),
        "remote" => crate::trf!("Im Browser: {name}", name = fields_name(&l)),
        _ => fields_name(&l),
    }
}

/// Values of the parts of a merged slider step, oldest first ("+0,10 → +0,25 → +0,50"); `None` if not known.
pub fn merged_values(s: &Step) -> Option<String> {
    let l = step_label(s)?;
    let f = l.get("f")?.as_array()?.first()?.as_str()?.to_string();
    let signed = field(&f).is_some_and(|(_, s)| s);
    let mut v: Vec<f64> = l.get("h").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    v.push(l.get("v")?.get(0)?.as_f64()?);
    (v.len() > 1).then(|| v.iter().map(|x| number(*x, signed)).collect::<Vec<_>>().join(" → "))
}

/// Date of a time (Unix seconds) in the language of the interface.
pub fn date_of(t: i64) -> String {
    let a = library::platform::local_time(t);
    crate::menu::i18n::format::date(crate::menu::i18n::language(), a.year, a.month as u8, a.tag as u8).unwrap_or_default()
}

/// When a step was made: the time today, "gestern", else the date.
pub fn when(t: i64) -> String {
    let now = library::now();
    let a = library::platform::local_time(t);
    let b = library::platform::local_time(now);
    let y = library::platform::local_time(now - 86_400);
    if (a.year, a.month, a.tag) == (b.year, b.month, b.tag) {
        crate::menu::i18n::format::time(a.hour as u8, a.minute as u8, None).unwrap_or_default()
    } else if (a.year, a.month, a.tag) == (y.year, y.month, y.tag) {
        crate::tr("gestern").to_string()
    } else {
        crate::menu::i18n::format::date(crate::menu::i18n::language(), a.year, a.month as u8, a.tag as u8).unwrap_or_default()
    }
}

/// "Belichtung +0,30", "Belichtung, Kontrast", "Mehrere Einstellungen (Belichtung, Kontrast, +4)".
fn fields_name(l: &Value) -> String {
    let fields: Vec<&str> = l.get("f").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    let mut unknown = false;
    for f in &fields {
        match field(f) {
            Some((n, _)) => {
                let n = crate::tr(n).to_string();
                if !names.contains(&n) {
                    names.push(n);
                }
            }
            None => unknown = true,
        }
    }
    if unknown {
        for g in l.get("g").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            let n = crate::tr(g).to_string();
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    match names.len() {
        0 => crate::tr("Einstellungen").to_string(),
        1 => {
            let v = l.get("v").and_then(|v| v.get(0)).and_then(Value::as_f64);
            match (v, fields.as_slice()) {
                (Some(v), [f]) => format!("{} {}", names[0], number(v, field(f).is_some_and(|(_, s)| s))),
                _ => names[0].clone(),
            }
        }
        2 | 3 => names.join(", "),
        n => crate::trf!("Mehrere Einstellungen ({names}, +{n})", names = names[..2].join(", "), n = n - 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A history in memory only (tests never read a library).
    fn memory(name: &str) -> (History, Key) {
        let mut h = History::default();
        let k = Key { path: PathBuf::from(format!("/nirgends/verlauf-test/{name}.jpg")), copy: None };
        h.photos.insert(k.clone(), Photo::new(None, Vec::new(), None));
        (h, k)
    }

    fn named(before: &Settings, after: &Settings) -> String {
        let (k, l) = describe(before, after);
        step_name(&Step { seq: 1, time: 0, kind: k.into(), label: Some(l.to_string()), patch: json!({}), count: 1, command: None })
    }

    /// Steps of the mask list get their own names (rename, on/off, strength, order); everything else stays
    /// "Lokale Korrekturen".
    #[test]
    fn mask_list_steps_are_named() {
        let radial = |x: f32| masks::Layer::new(masks::Mask::Radial { center: [x, 0.5], radii: [0.2, 0.2], angle: 0.0, feather: 0.5 }, masks::Adjustments { exposure: 0.5, ..Default::default() });
        let with = |layers: Vec<masks::Layer>| {
            let mut d = masks::Document { version: 2, layers };
            d.settle_version();
            Settings { local: Some(d), ..Settings::default() }
        };
        let base = with(vec![radial(0.3), radial(0.6)]);
        let renamed = with(vec![masks::Layer { name: Some("Himmel".into()), ..radial(0.3) }, radial(0.6)]);
        let off = with(vec![radial(0.3), masks::Layer { enabled: false, ..radial(0.6) }]);
        let weaker = with(vec![masks::Layer { amount: 0.5, ..radial(0.3) }, radial(0.6)]);
        let order = with(vec![radial(0.6), radial(0.3)]);
        let moved = with(vec![radial(0.35), radial(0.6)]);
        assert_eq!(named(&base, &renamed), crate::tr("Maske umbenannt"));
        assert_eq!(named(&base, &off), crate::tr("Maske ein/aus"));
        assert_eq!(named(&base, &weaker), crate::tr("Stärke der Maske"));
        let mut finer = weaker.clone();
        if let Some(d) = finer.local.as_mut() {
            d.layers[0].refine = 0.4;
        }
        assert_eq!(named(&weaker, &finer), crate::tr("Kante verfeinern"));
        assert_eq!(named(&base, &order), crate::tr("Reihenfolge der Masken"));
        assert_eq!(named(&base, &moved), crate::tr("Lokale Korrekturen"));
        assert_eq!(describe(&base, &weaker).0, kind::MASK);
        assert_eq!(named(&Settings::default(), &base), crate::tr("Lokale Korrekturen"));
    }

    #[test]
    fn names_of_slider_steps() {
        let a = Settings::default();
        let b = Settings { exposure: 0.3, ..a.clone() };
        let name = named(&a, &b);
        assert!(name.starts_with("Belichtung +0") && name.ends_with("30"), "{name}");
        let c = Settings { contrast: 0.2, ..b.clone() };
        assert_eq!(named(&a, &c), "Belichtung, Kontrast");
        let d = Settings { vibrance: 0.1, clarity: 0.2, vignette: -0.3, ..c.clone() };
        assert_eq!(named(&a, &d), "Mehrere Einstellungen (Belichtung, Kontrast, +3)");
        let e = Settings { crop: [0.5, 0.5, 0.8, 0.8], ..a.clone() };
        assert_eq!(describe(&a, &e).0, kind::CROP);
        assert_eq!(named(&a, &e), "Freistellen");
        let mut f = a.clone();
        f.raw.whites = -0.2;
        let name = named(&a, &f);
        assert!(name.starts_with("Weiß −0"), "{name}");
        // unknown field: the section it belongs to
        let g = Settings { noise_brake: develop::NoiseBrake::Color, ..a.clone() };
        assert_eq!(named(&a, &g), "Fill Light");
        // commands name themselves
        let h = Hint::preset("Kontrastreich");
        let s = Step { seq: 1, time: 0, kind: h.kind.into(), label: Some(h.label.to_string()), patch: json!({}), count: 1, command: None };
        assert_eq!(step_name(&s), "Vorgabe: Kontrastreich");
        let start = Step { seq: 0, time: 0, kind: kind::START.into(), label: Some(json!({ "k": "start", "lr": true }).to_string()), patch: json!({}), count: 1, command: None };
        assert_eq!(step_name(&start), "Ausgangszustand (Lightroom)");
    }

    /// Merged photo: the steps of the automatic tone carry strength and output of the recipe; the cursor names what
    /// applied as of its step (before the first such step: what that step found).
    #[test]
    fn merge_recipe_follows_the_cursor() {
        let (mut h, k) = memory("hdr");
        let s0 = Settings::default();
        // new merge: tuned at 50 %, linear
        let s1 = Settings { exposure: -0.5, highlight: -0.4, ..s0.clone() };
        hint(0, Hint::merge_tone([0.5, 0.5], [false, false]));
        h.record(&k, 0, &s0, &s1);
        // strength to 80 %
        let s2 = Settings { exposure: -0.8, highlight: -0.6, ..s0.clone() };
        hint(0, Hint::merge_tone([0.5, 0.8], [false, false]));
        h.record(&k, 0, &s1, &s2);
        // a slider by hand, then the output to fusion: values taken back
        let s3 = Settings { contrast: 0.2, ..s2.clone() };
        h.record(&k, 0, &s2, &s3);
        let s4 = Settings { exposure: 0.0, highlight: 0.0, ..s3.clone() };
        hint(0, Hint::merge_tone([0.8, 0.8], [false, true]));
        h.record(&k, 0, &s3, &s4);
        let names: Vec<String> = h.get(&k).unwrap().steps.iter().map(step_name).collect();
        assert_eq!(names[1..], ["Auto".to_string(), "Stärke 80 %".into(), names[3].clone(), "Ausgabe: Fusion".into()]);
        let at = |h: &History, from: usize| h.merge_recipe_at_cursor(&k, from).map(|(s, o, st)| (s, o, st.exposure));
        assert_eq!(h.cursor_of(&k), Some(4));
        assert_eq!(at(&h, 0), Some((0.8, true, 0.0)));
        // ⌘Z: linear again with the values of 80 % (the state of the newest automatic step, not the hand change)
        assert_eq!(h.undo(&k, 0, &s4, false), Some(s3.clone()));
        assert_eq!(at(&h, 4), Some((0.8, false, -0.8)));
        // ⌘Z over the hand change: no step of the automatic crossed, the recipe is left alone
        h.undo(&k, 0, &s3, false);
        assert_eq!(at(&h, 3), None);
        assert_eq!(at(&h, 4), Some((0.8, false, -0.8)), "a jump over the fusion step");
        h.undo(&k, 0, &s2, false);
        assert_eq!(at(&h, 2), Some((0.5, false, -0.5)));
        // before the first tuning: what it found
        h.undo(&k, 0, &s1, false);
        assert_eq!(at(&h, 1), Some((0.5, false, 0.0)));
        // ⇧⌘Z
        h.undo(&k, 0, &s0, true);
        assert_eq!(at(&h, 0), Some((0.5, false, -0.5)));
        h.undo(&k, 0, &s1, true);
        assert_eq!(at(&h, 1), Some((0.8, false, -0.8)));
        // no automatic step: nothing to follow
        let (mut plain, kp) = memory("plain");
        plain.record(&kp, 0, &s0, &s3);
        assert_eq!(plain.merge_recipe_at_cursor(&kp, 0).map(|r| r.0), None);
    }

    /// ⌘Z / ⇧⌘Z move the cursor; slider steps on the same field merge; a command step does not.
    #[test]
    fn steps_merge_and_undo() {
        let (mut h, k) = memory("merge");
        let s0 = Settings::default();
        let s1 = Settings { exposure: 0.1, ..s0.clone() };
        let s2 = Settings { exposure: 0.2, ..s0.clone() };
        let s3 = Settings { exposure: 0.2, contrast: 0.3, ..s0.clone() };
        h.record(&k, 100, &s0, &s1);
        h.record(&k, 100, &s1, &s2);
        assert_eq!(h.get(&k).unwrap().steps.len(), 2, "start + one merged step");
        assert_eq!(h.get(&k).unwrap().steps[1].count, 2);
        h.record(&k, 100, &s2, &s3);
        assert_eq!(h.changes(&k), 2);
        hint(100, Hint::preset("X"));
        let s4 = Settings { exposure: 0.5, ..s3.clone() };
        h.record(&k, 100, &s3, &s4);
        assert_eq!(h.get(&k).unwrap().steps[3].kind, kind::PRESET);
        // undo all the way back to the start, then redo
        assert_eq!(h.undo(&k, 100, &s4, false), Some(s3.clone()));
        assert_eq!(h.undo(&k, 100, &s3, false), Some(s2.clone()));
        assert_eq!(h.undo(&k, 100, &s2, false), Some(s0.clone()));
        assert_eq!(h.undo(&k, 100, &s0, false), None);
        assert_eq!(h.undo(&k, 100, &s0, true), Some(s2.clone()));
        // a slider moved back to where the merged step began leaves no step
        let (mut g, k) = memory("back");
        g.record(&k, 101, &s0, &s1);
        g.record(&k, 101, &s1, &s0);
        assert_eq!(g.changes(&k), 0);
    }

    /// A Sigma colour mode clicked on a photo set to black and white is one step; ⌘Z brings black and white back.
    #[test]
    fn colour_mode_from_black_and_white_is_undone() {
        let (mut h, k) = memory("mono-to-colour");
        let mono = Settings { monochrome: true, ..Settings::default() };
        hint(102, Hint::film("Neutral", true));
        let colour = crate::film_look::Choice::Sigma("Neutral".into()).apply(&mono);
        assert!(!colour.monochrome);
        h.record(&k, 102, &mono, &colour);
        assert_eq!(h.changes(&k), 1);
        assert_eq!(h.undo(&k, 102, &colour, false), Some(mono.clone()));
        assert_eq!(h.undo(&k, 102, &mono, true), Some(colour));
    }

    /// A reset by double-click right after dragging the same slider stays a step of its own (before, the reset merged
    /// into the drag and the dragged value was lost); a drag after the reset does not merge into it either.
    #[test]
    fn reset_gesture_is_its_own_step() {
        let (mut h, k) = memory("reset");
        let s0 = Settings::default();
        let s1 = Settings { contrast: 0.2, ..s0.clone() };
        let s2 = Settings { exposure: 0.7, ..s1.clone() };
        let s3 = s1.clone();
        let s4 = Settings { exposure: 0.4, ..s1.clone() };
        h.record(&k, 107, &s0, &s1);
        h.record(&k, 107, &s1, &s2);
        h.record_with(&k, 107, &s2, &s3, true);
        assert_eq!(h.changes(&k), 3, "drag and reset are two steps");
        h.record(&k, 107, &s3, &s4);
        assert_eq!(h.changes(&k), 4, "a drag after the reset is a new step");
        assert_eq!(h.undo(&k, 107, &s4, false), Some(s3.clone()));
        assert_eq!(h.undo(&k, 107, &s3, false), Some(s2.clone()), "the dragged value comes back");
        assert_eq!(h.undo(&k, 107, &s2, false), Some(s1.clone()));
        // the mark of the gesture is taken once
        reset_gesture();
        assert!(take_reset_gesture());
        assert!(!take_reset_gesture());
    }

    /// After a jump back a new change drops the later steps; an immediate ⌘Z brings them back with the cursor.
    #[test]
    fn dropped_steps_come_back() {
        let (mut h, k) = memory("dropped");
        let s0 = Settings::default();
        let s1 = Settings { exposure: 0.1, ..s0.clone() };
        let s2 = Settings { exposure: 0.1, contrast: 0.2, ..s0.clone() };
        let s3 = Settings { exposure: 0.1, contrast: 0.2, vibrance: 0.3, ..s0.clone() };
        h.record(&k, 102, &s0, &s1);
        h.record(&k, 102, &s1, &s2);
        h.record(&k, 102, &s2, &s3);
        assert_eq!(h.undo(&k, 102, &s3, false), Some(s2.clone()));
        assert_eq!(h.undo(&k, 102, &s2, false), Some(s1.clone()));
        let other = Settings { clarity: 0.5, ..s1.clone() };
        let status = h.record(&k, 102, &s1, &other);
        assert!(status.is_some_and(|s| s.starts_with('2')));
        assert_eq!(h.changes(&k), 2);
        // immediate ⌘Z: back to s1 with the two steps after it
        assert_eq!(h.undo(&k, 102, &other, false), Some(s1.clone()));
        assert_eq!(h.changes(&k), 3);
        assert_eq!(h.undo(&k, 102, &s1, true), Some(s2.clone()));
        assert_eq!(h.undo(&k, 102, &s2, true), Some(s3.clone()));
    }

    /// A click jumps without a new step; "Original" is step 0; ⇧⌘Z goes forward again; merged values are kept.
    #[test]
    fn jump_and_original() {
        let (mut h, k) = memory("jump");
        let s0 = Settings::default();
        let s1 = Settings { exposure: 0.1, ..s0.clone() };
        let s2 = Settings { exposure: 0.3, ..s0.clone() };
        let s3 = Settings { exposure: 0.3, contrast: 0.2, ..s0.clone() };
        h.record(&k, 104, &s0, &s1);
        h.record(&k, 104, &s1, &s2);
        h.record(&k, 104, &s2, &s3);
        let merged = h.get(&k).unwrap().steps[1].clone();
        assert_eq!(merged.count, 2);
        let v = merged_values(&merged).unwrap();
        assert!(v.contains('→'), "{v}");
        assert_eq!(h.jump(&k, 104, &s3, 0), Some(s0.clone()));
        assert_eq!(h.get(&k).unwrap().steps.len(), 3, "a jump adds no step");
        assert_eq!(h.jump(&k, 104, &s0, 0), None);
        assert_eq!(h.undo(&k, 104, &s0, true), Some(s2.clone()));
        assert_eq!(h.jump(&k, 104, &s2, 2), Some(s3.clone()));
        // hover: the state of a step
        h.set_hover(&k, Some(0), &s3);
        assert_eq!(h.take_hover(), Some(s0.clone()));
        assert_eq!(h.take_hover(), None);
    }

    /// A command for several photos on a photo held in memory: a named step with the command id; the command's undo
    /// moves the cursor back; "Verlauf bis hier zusammenfassen" and "löschen" work on the list.
    #[test]
    fn command_steps_in_memory() {
        let (mut h, k) = memory("cmd");
        let s0 = Settings::default();
        let s1 = Settings { exposure: 0.1, ..s0.clone() };
        let s2 = Settings { saturation: 0.4, ..s1.clone() };
        h.record(&k, 105, &s0, &s1);
        let cmd = Command::new(Hint::paste(Some("A.jpg")));
        assert!(h.command_step(&k, &s1, &s2, &cmd, false));
        let p = h.get(&k).unwrap();
        assert_eq!((p.steps.len(), p.cursor, p.steps[2].command), (3, 2, Some(cmd.id)));
        assert_eq!(step_name(&p.steps[2]), "Eingefügt von A.jpg");
        assert!(h.command_step(&k, &s2, &s1, &cmd, true));
        assert_eq!(h.get(&k).unwrap().cursor, 1, "undo of the command: cursor back");
        assert!(h.command_step(&k, &s1, &s2, &cmd, false));
        let s3 = Settings { contrast: 0.2, ..s2.clone() };
        h.record(&k, 105, &s2, &s3);
        assert!(h.compact_to(&k, 2));
        assert_eq!(h.get(&k).unwrap().steps.len(), 3);
        assert_eq!(h.state_of(&k, 2, &s3), Some(s3.clone()));
        h.clear_photo(&k);
        assert_eq!(h.changes(&k), 0);
        let other = Key { path: PathBuf::from("/nirgends/verlauf-test/x.jpg"), copy: None };
        assert!(!h.command_step(&other, &s0, &s1, &cmd, false), "not in memory: the library records it");
    }

    /// Loaded history that does not fit the photo, changed in Lightroom since its last step: "Aus Lightroom übernommen".
    #[test]
    fn lightroom_change_is_named() {
        let (mut h, k) = memory("lr");
        let s0 = Settings::default();
        let s1 = Settings { exposure: 0.1, ..s0.clone() };
        h.record(&k, 106, &s0, &s1);
        let p = h.photos.get_mut(&k).unwrap();
        p.checked = false;
        p.lr_touch = Some(p.steps[1].time + 10);
        let from_lr = Settings { exposure: 0.8, ..s0.clone() };
        h.follow(&k, 106, &from_lr);
        let p = h.get(&k).unwrap();
        assert_eq!(p.steps.last().unwrap().kind, kind::LIGHTROOM);
        assert_eq!(step_name(p.steps.last().unwrap()), "Aus Lightroom übernommen");
    }

    /// A change that does not go through `record` (a command for several photos): a step with the command's name;
    /// its ⌘Z (back to the state before) only moves the cursor.
    #[test]
    fn command_changes_follow() {
        let (mut h, k) = memory("command");
        let s0 = Settings::default();
        let s1 = Settings { exposure: 0.1, ..s0.clone() };
        h.record(&k, 103, &s0, &s1);
        let pasted = Settings { saturation: 0.4, ..s1.clone() };
        hint(103, Hint::paste(None));
        h.follow(&k, 103, &pasted);
        assert_eq!(h.get(&k).unwrap().steps.last().unwrap().kind, kind::PASTE);
        h.follow(&k, 103, &s1);
        let p = h.get(&k).unwrap();
        assert_eq!((p.steps.len(), p.cursor), (3, 1), "the paste step stays, the cursor goes back");
    }

    /// Size of one step in the library for typical changes (patch text; the label adds 30–80 bytes).
    #[test]
    fn step_sizes() {
        let base = Settings { exposure: 0.35, contrast: 0.1, vibrance: 0.15, ..Settings::default() };
        let size = |a: &Settings, b: &Settings| dh::patch_text(&dh::diff(&value(a), &value(b))).len();
        let whole = dh::patch_text(&value(&base)).len();
        let slider = size(&base, &Settings { exposure: 0.6, ..base.clone() });
        let preset = crate::presets::presets_library().iter().flat_map(|c| c.presets.iter()).map(|p| size(&base, &p.apply(&base))).max().unwrap_or(0);
        let mut film = base.clone();
        film.film.active = true;
        film.film.stock = "kodak_portra_400".into();
        let film = size(&base, &film);
        let crop = size(&base, &Settings { crop: [0.48, 0.52, 0.8, 0.7], crop_angle: 1.5, ..base.clone() });
        let spot = develop::spots::Spot { clone: false, center: [0.4, 0.5], source: [0.45, 0.52], radius: 0.02, opacity: 1.0 };
        let mut spots = base.clone();
        spots.spots = vec![spot; 20];
        let mut more = spots.clone();
        more.spots.push(develop::spots::Spot { center: [0.7, 0.2], ..spot });
        let retouch = size(&spots, &more);
        let stroke = |i: usize| masks::Mask::Brush { points: (0..60).map(|j| [0.1 + i as f32 * 0.003 + j as f32 * 0.001, 0.3 + j as f32 * 0.002]).collect(), radius: 0.05, hardness: 0.5, flow: 1.0, density: 1.0, auto: None };
        fn tree(v: &[masks::Mask]) -> masks::Mask {
            if v.len() == 1 {
                return v[0].clone();
            }
            let n = v.len() / 2;
            masks::Mask::Combine { operation: masks::Operation::Add, left: Box::new(tree(&v[..n])), right: Box::new(tree(&v[n..])) }
        }
        let with = |n: usize| {
            let strokes: Vec<masks::Mask> = (0..n).map(stroke).collect();
            let adjustments: masks::Adjustments = serde_json::from_value(json!({ "exposure": 0.3 })).unwrap_or_default();
            Settings { local: Some(masks::Document { version: 2, layers: vec![masks::Layer::new(tree(&strokes), adjustments)] }), ..base.clone() }
        };
        let mask_whole = dh::patch_text(&value(&with(50))).len();
        let mask = size(&with(50), &with(51));
        eprintln!("step sizes (bytes): whole state {whole}, slider {slider}, preset (largest) {preset}, film {film}, crop {crop}, retouch spot {retouch}, mask stroke {mask} (whole state with 50 strokes {mask_whole})");
        assert!(slider < 60, "slider step {slider} B");
        assert!(preset < whole, "preset {preset} B, whole {whole} B");
        assert!(retouch < 200, "spot {retouch} B");
        assert!(mask < mask_whole / 5, "stroke {mask} B, whole {mask_whole} B");
    }
}
