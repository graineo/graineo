//! Reading the picture styles of the library's RAWs for "Kamera-Looks", ahead and in parallel.
//!
//! The window needs the picture style of a spread sample of each camera's RAWs (at most
//! [`learn::SAMPLE_PER_CAMERA`] per camera, see [`learn::sample`]). Reading it from a network drive took 0.2–0.7 s per
//! file, one after the other, and started only when the window opened. Now:
//! - only the head of a file is read ([`raw::picture_style::read`]), the result is kept in the cache file;
//! - several readers at once (network drive 6 while the window is open, 3 in idle time; local disk 3 / 2), so the waits
//!   of the drive overlap; quiet priority, nothing that computes;
//! - the first [`FIRST_PER_CAMERA`] photos of each camera's sample first (the sample is ordered so that its start is
//!   already spread over the shooting days), the rest after: the list fills camera by camera and shows the styles
//!   early; learning waits for the whole sample of its camera, so its photos are the same as before;
//! - in idle time (no input for a while, not in Low Power Mode or on battery) once per session ahead, so the window
//!   usually opens with everything known; the window takes the readings over and asks for more readers.
//!
//! The result in the end is the same list as reading all of the sample one by one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rawdev::learn::{self, Candidate, ScanCache, Scanned};

/// Photos of each camera's sample read first (the list shows the styles after these).
pub const FIRST_PER_CAMERA: usize = 120;
/// Readers at most (network drive, window open).
const MAX_READERS: usize = 6;
/// The app must have been running this long before idle-time reading starts (start-up work first).
const IDLE_START: Duration = Duration::from_secs(60);
/// Save the cache file after this many new readings (a cancelled run keeps what it read).
const SAVE_EVERY: usize = 50;

/// Who wants the readings now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// nobody: the readers wait
    Off,
    /// idle time ahead: only while the app is idle and not saving energy, few readers, lowest priority
    Ahead,
    /// the window is open: more readers, the user is waiting for the list
    Window,
}

/// What the window shows: readings so far and progress.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// read photos with a learnable style, in the order of the sample (as one reader would give them)
    pub scanned: Arc<Vec<Scanned>>,
    /// photos of the sample read / in total
    pub done: usize,
    pub total: usize,
    /// per camera (library name): photos read and in its sample
    pub cameras: HashMap<String, (usize, usize)>,
    /// remaining time (only after enough readings to tell)
    pub eta: Option<Duration>,
    /// readers are working right now (`false`: finished, or waiting for idle time)
    pub reading: bool,
    /// counts up with every change
    pub generation: u64,
}

impl Snapshot {
    /// Is the whole sample of this camera read?
    pub fn camera_done(&self, camera: &str) -> bool {
        self.cameras.get(camera).is_none_or(|(d, n)| d >= n)
    }

    pub fn complete(&self) -> bool {
        self.done >= self.total
    }
}

struct State {
    mode: Mode,
    /// sample in the order of [`learn::sample`] and the camera of each entry
    sample: Vec<Candidate>,
    /// identity of the candidates the plan was made from
    plan_key: u64,
    /// what was read for each sample entry: `None` not yet, `Some(None)` no learnable style / not there
    results: Vec<Option<Option<Scanned>>>,
    /// sample entries still to read, in reading order
    queue: VecDeque<usize>,
    /// sample entries being read right now
    busy: HashSet<usize>,
    /// end times of the last readings (remaining time)
    recent: VecDeque<Instant>,
    /// readings by the readers (not from the cache) since the last save
    unsaved: usize,
    snapshot: Snapshot,
    /// the result list must be built again
    dirty: bool,
    /// network drive: more readers help
    network: bool,
}

static GENERATION: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static (Mutex<State>, Condvar) {
    static S: OnceLock<(Mutex<State>, Condvar)> = OnceLock::new();
    S.get_or_init(|| {
        (
            Mutex::new(State {
                mode: Mode::Off,
                sample: Vec::new(),
                plan_key: 0,
                results: Vec::new(),
                queue: VecDeque::new(),
                busy: HashSet::new(),
                recent: VecDeque::new(),
                unsaved: 0,
                snapshot: Snapshot::default(),
                dirty: false,
                network: false,
            }),
            Condvar::new(),
        )
    })
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().0.lock().unwrap_or_else(|e| e.into_inner())
}

fn cache() -> &'static Mutex<ScanCache> {
    static C: OnceLock<Mutex<ScanCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ScanCache::load()))
}

fn key_of(cands: &[Candidate]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cands.len().hash(&mut h);
    for c in cands {
        c.path.hash(&mut h);
        c.day.hash(&mut h);
        c.camera.hash(&mut h);
    }
    h.finish()
}

/// Reading order of the sample: the first [`FIRST_PER_CAMERA`] of every camera (cameras with most photos first),
/// then the rest; `first` (a camera) before all others. Pure (tests).
fn reading_order(sample: &[Candidate], photos: &HashMap<String, usize>, first: Option<&str>) -> Vec<usize> {
    let mut cameras: Vec<&str> = Vec::new();
    for c in sample {
        if cameras.last() != Some(&c.camera.as_str()) && !cameras.contains(&c.camera.as_str()) {
            cameras.push(c.camera.as_str());
        }
    }
    cameras.sort_by_key(|c| (Some(*c) != first, std::cmp::Reverse(photos.get(*c).copied().unwrap_or(0)), c.to_string()));
    let mut by_camera: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, c) in sample.iter().enumerate() {
        by_camera.entry(c.camera.as_str()).or_default().push(i);
    }
    let mut out = Vec::with_capacity(sample.len());
    if let Some(f) = first.and_then(|f| by_camera.get(f)) {
        out.extend(f.iter().copied());
    }
    for stage in 0..2 {
        for c in &cameras {
            if Some(*c) == first {
                continue;
            }
            let v = &by_camera[c];
            let cut = v.len().min(FIRST_PER_CAMERA);
            out.extend(if stage == 0 { &v[..cut] } else { &v[cut..] }.iter().copied());
        }
    }
    out
}

/// Plan the readings for these candidates (keeps what was read before for the same files).
fn plan(s: &mut State, cands: &[Candidate]) {
    let key = key_of(cands);
    if key == s.plan_key && !s.sample.is_empty() {
        return;
    }
    let old: HashMap<PathBuf, Option<Scanned>> = s.sample.iter().zip(s.results.iter()).filter_map(|(c, r)| r.clone().map(|r| (c.path.clone(), r))).collect();
    let sample = learn::sample(cands, learn::SAMPLE_PER_CAMERA);
    let mut photos: HashMap<String, usize> = HashMap::new();
    for c in cands {
        *photos.entry(c.camera.clone()).or_default() += 1;
    }
    s.results = sample.iter().map(|c| old.get(&c.path).cloned()).collect();
    s.queue = reading_order(&sample, &photos, None).into_iter().filter(|&i| s.results[i].is_none()).collect();
    // a network drive: more readers (a few folders are enough; looked at by a helper thread, a drive that went away
    // must not hold up the window)
    let mut seen = HashSet::new();
    let probe: Vec<PathBuf> = sample.iter().filter(|c| seen.insert(c.folder.clone())).take(8).map(|c| c.path.clone()).collect();
    let _ = std::thread::Builder::new().name("bildstile-ort".into()).spawn(move || {
        let network = probe.iter().any(|p| library::network_trash::is_network(p));
        let mut s = lock();
        if s.plan_key == key {
            s.network = network;
            state().1.notify_all();
        }
    });
    s.sample = sample;
    s.plan_key = key;
    s.busy.clear();
    s.recent.clear();
    s.dirty = true;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Readers allowed now for the mode and the place of the photos.
fn readers(mode: Mode, network: bool) -> usize {
    match (mode, network) {
        (Mode::Off, _) => 0,
        (Mode::Ahead, true) => 3,
        (Mode::Ahead, false) => 2,
        (Mode::Window, true) => MAX_READERS,
        (Mode::Window, false) => 3,
    }
}

/// May the idle-time reading go on? (No input for a while, not saving energy.)
fn ahead_allowed() -> bool {
    since_start() >= IDLE_START && crate::jobs::idle() && crate::perf_mode::power() == crate::perf_mode::Power::Mains
}

fn start_readers() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    for n in 0..MAX_READERS {
        let _ = std::thread::Builder::new().name(format!("bildstile-{n}")).spawn(move || reader(n));
    }
}

/// One reader: takes the next photo of the queue while its number is allowed, reads it (cache or file head).
fn reader(n: usize) {
    let mut priority = None;
    loop {
        let (i, cand, mode, plan_key) = {
            let (m, cv) = state();
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let allowed = n < readers(s.mode, s.network) && (s.mode != Mode::Ahead || ahead_allowed());
                if allowed {
                    if let Some(i) = s.queue.pop_front() {
                        s.busy.insert(i);
                        break (i, s.sample[i].clone(), s.mode, s.plan_key);
                    }
                }
                // nothing to do, or idle time is over: look again in a moment
                s = cv.wait_timeout(s, Duration::from_secs(2)).unwrap_or_else(|e| e.into_inner()).0;
            }
        };
        // the window: the user waits (a network drive is slowed down at the lowest priority); ahead: lowest
        let want = if mode == Mode::Window { library::platform::Priority::Utility } else { library::platform::Priority::Background };
        if priority != Some(want) {
            library::platform::set_priority(want);
            priority = Some(want);
        }
        let before = cache().lock().unwrap_or_else(|e| e.into_inner()).knows(&cand.path);
        let r = learn::scan_cached(&cand, cache()).unwrap_or(None);
        let mut s = lock();
        if s.plan_key != plan_key {
            continue;
        }
        s.busy.remove(&i);
        s.results[i] = Some(r);
        s.recent.push_back(Instant::now());
        while s.recent.len() > 40 {
            s.recent.pop_front();
        }
        if !before {
            s.unsaved += 1;
        }
        let finished = s.queue.is_empty() && s.busy.is_empty();
        if s.unsaved >= SAVE_EVERY || (finished && s.unsaved > 0) {
            s.unsaved = 0;
            drop(s);
            cache().lock().unwrap_or_else(|e| e.into_inner()).save();
            s = lock();
        }
        s.dirty = true;
        GENERATION.fetch_add(1, Ordering::Relaxed);
        if finished {
            crate::log(&format!("picture styles: {} photos of the sample read", s.sample.len()));
        }
    }
}

/// Remaining time from the pace of the last readings (`None` until there are enough).
fn eta(recent: &VecDeque<Instant>, left: usize, readers_now: usize) -> Option<Duration> {
    if left == 0 || recent.len() < 10 || readers_now == 0 {
        return None;
    }
    let span = recent.back()?.duration_since(*recent.front()?);
    if span < Duration::from_millis(500) {
        return None;
    }
    let per = span.as_secs_f64() / (recent.len() - 1) as f64;
    Some(Duration::from_secs_f64(per * left as f64))
}

fn build(s: &mut State) {
    let mut cameras: HashMap<String, (usize, usize)> = HashMap::new();
    for (c, r) in s.sample.iter().zip(s.results.iter()) {
        let e = cameras.entry(c.camera.clone()).or_default();
        e.1 += 1;
        if r.is_some() {
            e.0 += 1;
        }
    }
    let done = s.results.iter().filter(|r| r.is_some()).count();
    let scanned: Vec<Scanned> = s.results.iter().filter_map(|r| r.clone().flatten()).collect();
    let reading = !(s.queue.is_empty() && s.busy.is_empty()) && readers(s.mode, s.network) > 0 && (s.mode != Mode::Ahead || ahead_allowed());
    let readers_now = if reading { readers(s.mode, s.network) } else { 0 };
    s.snapshot = Snapshot {
        scanned: Arc::new(scanned),
        done,
        total: s.sample.len(),
        cameras,
        eta: eta(&s.recent, s.sample.len() - done, readers_now),
        reading,
        generation: GENERATION.load(Ordering::Relaxed),
    };
    s.dirty = false;
}

/// The readings for the window (built again only when something changed).
pub fn snapshot() -> Snapshot {
    let mut s = lock();
    if s.dirty || s.snapshot.generation != GENERATION.load(Ordering::Relaxed) {
        build(&mut s);
    }
    s.snapshot.clone()
}

/// Counts up with every reading (cheap check whether [`snapshot`] changed).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The window opened with these candidates: read what is missing, with more readers.
pub fn open(cands: &[Candidate]) {
    start_readers();
    let mut s = lock();
    plan(&mut s, cands);
    s.mode = Mode::Window;
    s.dirty = true;
    state().1.notify_all();
}

/// The window closed: what is left is read in idle time only.
pub fn close() {
    let mut s = lock();
    if s.mode == Mode::Window {
        s.mode = Mode::Ahead;
        s.dirty = true;
    }
}

/// The window shows this camera (library name): its photos first.
pub fn prefer(camera: &str) {
    let mut s = lock();
    let mut photos: HashMap<String, usize> = HashMap::new();
    for c in &s.sample {
        *photos.entry(c.camera.clone()).or_default() += 1;
    }
    let order = reading_order(&s.sample, &photos, Some(camera));
    let left: HashSet<usize> = s.queue.iter().copied().collect();
    let queue: VecDeque<usize> = order.into_iter().filter(|i| left.contains(i)).collect();
    if queue != s.queue {
        s.queue = queue;
    }
}

/// Every frame from the library: plan the readings ahead for the library's RAWs, so the window opens with the styles
/// known. `photos` (number of rows) tells whether the library changed; `cands` is only called then. The readers start
/// once the app has been running a minute and is idle (and runs on mains power); the cache knows what was read before,
/// so after the first time only new photos are read.
pub fn ahead(photos: usize, cands: impl FnOnce() -> Vec<Candidate>) {
    static SEEN: AtomicU64 = AtomicU64::new(u64::MAX);
    if SEEN.swap(photos as u64, Ordering::Relaxed) == photos as u64 {
        return;
    }
    let _ = since_start();
    let c = cands();
    if c.is_empty() {
        return;
    }
    start_readers();
    let mut s = lock();
    if s.mode == Mode::Window {
        return;
    }
    plan(&mut s, &c);
    s.mode = Mode::Ahead;
    s.dirty = true;
    let left = s.queue.len();
    if left > 0 {
        crate::log(&format!("picture styles: {left} of {} photos to read ahead in idle time", s.sample.len()));
    }
    state().1.notify_all();
}

/// Time since the library first asked for readings ahead (about the start of the app).
fn since_start() -> Duration {
    static FIRST: OnceLock<Instant> = OnceLock::new();
    FIRST.get_or_init(Instant::now).elapsed()
}

/// Text for the activity list while reading (`None`: nothing to do or waiting for idle time).
pub fn status() -> Option<String> {
    let s = snapshot();
    (s.reading && !s.complete()).then(|| crate::trf!("Bildstile lesen: {d} von {n} Fotos", d = s.done, n = s.total))
}

/// "noch etwa 3 min" / "noch etwa 40 s".
pub fn eta_text(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        crate::trf!("noch etwa {n} s", n = s.max(5).div_ceil(5) * 5)
    } else {
        crate::trf!("noch etwa {n} min", n = s.div_ceil(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(camera: &str, i: usize) -> Candidate {
        Candidate { path: PathBuf::from(format!("/{camera}/{i}.RAF")), day: format!("2026-01-{:02}", i % 28 + 1), folder: format!("/{camera}"), camera: camera.into(), thumb: None }
    }

    #[test]
    fn reading_order_starts_every_camera_then_reads_the_rest() {
        let mut sample: Vec<Candidate> = (0..200).map(|i| cand("A", i)).collect();
        sample.extend((0..150).map(|i| cand("B", i)));
        let photos = HashMap::from([("A".to_string(), 900), ("B".to_string(), 2000)]);
        let o = reading_order(&sample, &photos, None);
        assert_eq!(o.len(), 350);
        // B has more photos: its first 120, then A's first 120, then the rest of B, then the rest of A
        assert_eq!(o[..120], (200..320).collect::<Vec<_>>()[..]);
        assert_eq!(o[120..240], (0..120).collect::<Vec<_>>()[..]);
        assert_eq!(o[240..270], (320..350).collect::<Vec<_>>()[..]);
        assert_eq!(o[270..], (120..200).collect::<Vec<_>>()[..]);
        // every entry exactly once
        let mut s = o.clone();
        s.sort_unstable();
        assert_eq!(s, (0..350).collect::<Vec<_>>());
        // a preferred camera goes first, whole
        let o = reading_order(&sample, &photos, Some("A"));
        assert_eq!(o[..200], (0..200).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn the_start_of_a_sample_is_the_smaller_sample() {
        // the first photos of a camera's sample are the sample one would take with fewer photos: the list after the
        // first stage shows a spread selection, not the oldest days
        let cands: Vec<Candidate> = (0..3000).map(|i| Candidate { day: format!("2025-{:02}-{:02}", (i / 28) % 12 + 1, i % 28 + 1), ..cand("A", i) }).collect();
        let big = learn::sample(&cands, learn::SAMPLE_PER_CAMERA);
        let small = learn::sample(&cands, FIRST_PER_CAMERA);
        assert_eq!(big.len(), learn::SAMPLE_PER_CAMERA);
        assert_eq!(big[..FIRST_PER_CAMERA].iter().map(|c| &c.path).collect::<Vec<_>>(), small.iter().map(|c| &c.path).collect::<Vec<_>>());
    }

    #[test]
    fn remaining_time_only_with_enough_readings() {
        let t = Instant::now();
        let few: VecDeque<Instant> = (0..5).map(|k| t + Duration::from_millis(k * 100)).collect();
        assert_eq!(eta(&few, 100, 3), None);
        let many: VecDeque<Instant> = (0..21).map(|k| t + Duration::from_millis(k * 100)).collect();
        let e = eta(&many, 300, 3).unwrap();
        assert!((e.as_secs_f64() - 30.0).abs() < 0.01, "{e:?}");
        assert_eq!(eta(&many, 0, 3), None);
    }
}
