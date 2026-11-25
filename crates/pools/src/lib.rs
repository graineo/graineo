//! Thread pools of the app and the rule that keeps them free of lock-ups.
//!
//! Rayon workers steal: a worker that waits for its parallel parts (`join`) takes any other job of its pool meanwhile,
//! also a new top-level job handed to the pool from outside (`install`). So a worker must never block on something
//! that a job of its own pool may have to finish first (a lock, a computation another caller shares): the finisher
//! may sit further down the worker's own stack, or two workers may wait for each other – seen as a lock-up of the
//! offline copies of the same Quattro files in the background pool. Plain threads may wait; a worker of pool A may
//! wait for work that runs in pool B (B gets along without A's threads).
//!
//! This crate names the pools ([`Pool`]; a pool marks its threads once in its `start_handler` with [`mark`]) and gives
//! two building blocks that follow the rule:
//! - [`Shared`]: a result computed once and shared; a caller waits for a running computation only outside its own pool,
//!   and a caller in front waits for a background computation only briefly, then computes itself.
//! - [`Gate`]: the lock where computations wait for each other, with intent (visible before soon before idle) and
//!   key (a newer waiter with the same key supersedes an older one); a gate in front of a pool lets the pool's own
//!   workers pass. See [`gate`].
//! - [`Turn`]: one top-level piece of work at a time in a pool, a [`Gate`] in front of the pool; callers from outside
//!   wait for their turn by intent. Several top-level jobs in a small pool stack on its workers (a worker waiting for
//!   its parallel parts takes the next photo on top), so the first one finishes last and every photo in progress
//!   holds its memory.

pub mod gate;
pub mod gpu;
pub use gpu::{gpu_allowed, gpu_error, gpu_errors, gpu_fits_memory, gpu_lost, physical_memory, set_gpu_allowed, take_gpu_lost_notice, GPU_BYTES_PER_PIXEL, GPU_MEMORY_SHARE};
pub use gate::{all, intent, set_intent, with_intent, Acquire, Gate, Guard, Intent, Snapshot};

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The app's thread pools. Threads not marked by any pool count as [`Pool::Global`]: a plain thread runs its
/// parallel parts in rayon's global pool, and a worker of an unnamed pool is treated the same way (on the safe side:
/// it waits for nothing that could run in the global pool).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pool {
    /// rayon's global pool: the work in front (one thread per core), also used by plain threads
    Global,
    /// the shared background pool (offline copies, large previews; efficiency cores, lowest priority)
    Background,
    /// the pool of the Sigma base ("SPP base")
    Spp,
    /// the parallel parts of a film computation started on a rayon worker
    Film,
}

/// Where the current thread runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thread {
    /// no rayon worker: may wait for anything
    Plain,
    /// a worker of this pool
    Worker(Pool),
}

thread_local! {
    static MARK: Cell<Option<Pool>> = const { Cell::new(None) };
}

/// Mark the current thread as a worker of `pool` (in the pool's `start_handler`).
pub fn mark(pool: Pool) {
    MARK.with(|m| m.set(Some(pool)));
}

/// The current thread: plain, or a worker of which pool.
pub fn current() -> Thread {
    match MARK.with(|m| m.get()) {
        Some(p) => Thread::Worker(p),
        None if rayon::current_thread_index().is_some() => Thread::Worker(Pool::Global),
        None => Thread::Plain,
    }
}

/// The pool whose threads a computation started on `t` needs for its parallel parts.
fn uses(t: Thread) -> Pool {
    match t {
        Thread::Plain => Pool::Global,
        Thread::Worker(p) => p,
    }
}

/// May `waiter` block until a computation started on `computer` is finished? A plain thread always; a worker only
/// when the computation runs in another pool (its own pool is not needed to finish it).
pub fn may_wait(waiter: Thread, computer: Thread) -> bool {
    waiter == Thread::Plain || uses(waiter) != uses(computer)
}

static RECOMPUTED: AtomicUsize = AtomicUsize::new(0);
static TAKEN_OVER: AtomicUsize = AtomicUsize::new(0);
static RUNS: AtomicU64 = AtomicU64::new(1);

/// How often a [`Shared`] computed a value once more because its caller was not allowed to wait for the running
/// computation (a worker of the same pool). For tests and measurements: with the pools gated by [`Turn`] this
/// should stay at zero.
pub fn recomputations() -> usize {
    RECOMPUTED.load(Ordering::SeqCst)
}

/// How often a caller with a higher [`Intent`] (visible or soon) stopped waiting for a background computation
/// ([`Intent::Idle`]) of a [`Shared`] and computed the value itself (see [`Shared::get_or_compute`]). For tests and
/// measurements.
pub fn takeovers() -> usize {
    TAKEN_OVER.load(Ordering::SeqCst)
}

/// How long a caller in front (intent above [`Intent::Idle`]) waits for a running background computation before it
/// computes the value itself. Background threads run only on the efficiency cores and can get no processor time
/// for minutes when the system keeps those busy (measured: an offline copy of a Quattro file 14 minutes for 19 s of
/// processor time); from outside nobody can tell a starved computation from one that is about to finish, so the
/// caller gives it this long and then computes for itself.
pub const BACKGROUND_GRACE: Duration = Duration::from_millis(500);

/// A computation in progress: on which thread, with which intent, and which run (so that only this run frees the
/// slot again when it panics).
#[derive(Clone, Copy)]
struct Run {
    thread: Thread,
    intent: Intent,
    id: u64,
}

enum State<T> {
    Empty,
    Running(Run),
    Done(T),
}

/// A value computed once and shared by all callers (e.g. the preprocessing of a Quattro file, which opening and the
/// Sigma base want at the same time). No lock is held while computing. While one caller computes, another one
/// waits for the result if [`may_wait`] allows it, otherwise it computes the value once more for itself (counted in
/// [`recomputations`]). A panic while computing leaves the slot empty for the next caller.
///
/// **Never in front behind the background:** a caller in front (its [`intent`] above [`Intent::Idle`]) waits for a
/// computation of the background ([`Intent::Idle`], e.g. an offline copy in the background pool) at most
/// [`BACKGROUND_GRACE`], then it takes over: it computes the value itself, and further callers wait for it instead
/// (counted in [`takeovers`]). The first computation to finish fills the slot, the later one is dropped and its
/// caller gets the stored value – the result is written once. The background keeps waiting for the front as before.
pub struct Shared<T> {
    state: Mutex<State<T>>,
    finished: Condvar,
}

impl<T> Default for Shared<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Shared<T> {
    pub const fn new() -> Self {
        Self { state: Mutex::new(State::Empty), finished: Condvar::new() }
    }

    /// Is the value there (computed, not running or empty)?
    pub fn is_done(&self) -> bool {
        matches!(*self.state.lock().unwrap_or_else(|e| e.into_inner()), State::Done(_))
    }

    /// Is a computation running right now (diagnosis, measurements)?
    pub fn is_running(&self) -> bool {
        matches!(*self.state.lock().unwrap_or_else(|e| e.into_inner()), State::Running(_))
    }
}

impl<T: Clone> Shared<T> {

    /// The value: from the slot, awaited, or computed by `compute` (here, without any lock held).
    pub fn get_or_compute(&self, compute: impl FnOnce() -> T) -> T {
        let me = current();
        let my_intent = intent();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // since when this caller waits for a background computation (for the grace period)
        let mut behind_background: Option<Instant> = None;
        loop {
            match &*st {
                State::Done(v) => return v.clone(),
                State::Running(r) if !may_wait(me, r.thread) => {
                    drop(st);
                    RECOMPUTED.fetch_add(1, Ordering::SeqCst);
                    return compute();
                }
                State::Running(r) if r.intent == Intent::Idle && my_intent > Intent::Idle => {
                    let since = *behind_background.get_or_insert_with(Instant::now);
                    match BACKGROUND_GRACE.checked_sub(since.elapsed()).filter(|d| !d.is_zero()) {
                        Some(left) => st = self.finished.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0,
                        None => {
                            TAKEN_OVER.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                    }
                }
                State::Running(_) => st = self.finished.wait(st).unwrap_or_else(|e| e.into_inner()),
                State::Empty => break,
            }
        }
        let id = RUNS.fetch_add(1, Ordering::SeqCst);
        *st = State::Running(Run { thread: me, intent: my_intent, id });
        drop(st);
        // a panic while computing leaves the slot empty again (if this run still owns it), so that nobody waits for
        // it forever
        struct Reset<'a, T>(&'a Shared<T>, u64, bool);
        impl<T> Drop for Reset<'_, T> {
            fn drop(&mut self) {
                if !self.2 {
                    let mut st = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    if matches!(&*st, State::Running(r) if r.id == self.1) {
                        *st = State::Empty;
                    }
                    drop(st);
                    self.0.finished.notify_all();
                }
            }
        }
        let mut reset = Reset(self, id, false);
        let v = compute();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        reset.2 = true;
        // the first computation to finish wins (the other one, taken over or overtaken, is dropped)
        let v = match &*st {
            State::Done(first) => first.clone(),
            _ => {
                *st = State::Done(v.clone());
                v
            }
        };
        drop(st);
        self.finished.notify_all();
        v
    }
}

/// One top-level piece of work at a time in a pool. Callers from outside the pool (plain threads, workers of other
/// pools) wait for their turn, served by their thread's [`intent`] (visible first); a worker of the pool itself runs
/// the work at once – nested, it belongs to the piece in progress, and waiting here would be waiting for itself.
pub struct Turn {
    gate: Gate,
}

impl Turn {
    pub const fn new(pool: Pool) -> Self {
        Self { gate: Gate::for_pool(pool_name(pool), pool) }
    }

    /// Run `f` in `in_pool` (the pool this turn guards) when it is this caller's turn. The caller's intent travels
    /// with `f` onto the pool's worker (thread-locals are not inherited), so gates inside see the same intent.
    pub fn run<R: Send>(&self, in_pool: &rayon::ThreadPool, f: impl FnOnce() -> R + Send) -> R {
        let i = intent();
        let _turn = self.gate.acquire(i, None);
        in_pool.install(move || with_intent(i, f))
    }

    /// Like [`Turn::run`] with an explicit intent and key: `None` when a newer caller with the same key arrived
    /// while this one waited (then `f` never runs).
    pub fn run_as<R: Send>(&self, intent: Intent, key: Option<u64>, in_pool: &rayon::ThreadPool, f: impl FnOnce() -> R + Send) -> Option<R> {
        let _turn = self.gate.acquire(intent, key).taken()?;
        Some(in_pool.install(move || with_intent(intent, f)))
    }

    /// The gate in front of the pool (diagnosis, hold-back hook).
    pub fn gate(&self) -> &Gate {
        &self.gate
    }
}

const fn pool_name(p: Pool) -> &'static str {
    match p {
        Pool::Global => "global",
        Pool::Background => "background",
        Pool::Spp => "spp",
        Pool::Film => "film",
    }
}

/// Gates outside [`gate::all`] (e.g. the background pool's turn) that the overview should list as well.
static REGISTERED: Mutex<Vec<&'static Gate>> = Mutex::new(Vec::new());

/// Add a gate to the overview ([`gates_overview`]); registering twice is harmless.
pub fn register(g: &'static Gate) {
    let mut r = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    if !r.iter().any(|x| std::ptr::eq(*x, g)) {
        r.push(g);
    }
}

/// Directory over the gates (diagnosis: "what runs, who waits, since when"): one entry per gate that is held or has
/// waiters, e.g. `open held 120 ms by job-vorne-2, waiting V1 S0 I0`; `idle` when nothing runs or waits.
pub fn gates_overview() -> String {
    let extra = REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let parts: Vec<String> = gate::all()
        .into_iter()
        .chain(extra)
        .map(|g| g.snapshot())
        .filter(|s| s.held || s.waiting.iter().any(|&n| n > 0))
        .map(|s| {
            let held = if s.held { format!("held {} ms by {}", s.held_for_ms, s.held_by.as_deref().unwrap_or("?")) } else { "free".to_string() };
            format!("{} {held}, waiting V{} S{} I{}", s.name, s.waiting[2], s.waiting[1], s.waiting[0])
        })
        .collect();
    if parts.is_empty() {
        "idle".to_string()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::time::Duration;

    fn pool(p: Pool, n: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new().num_threads(n).start_handler(move |_| mark(p)).build().unwrap()
    }

    #[test]
    fn plain_threads_wait_and_share_one_computation() {
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (s, n) = (slot.clone(), runs.clone());
        let first = std::thread::spawn(move || {
            s.get_or_compute(|| {
                n.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(200));
                7
            })
        });
        started_rx.recv().unwrap();
        let second = slot.get_or_compute(|| {
            runs.fetch_add(1, Ordering::SeqCst);
            8
        });
        assert_eq!((second, first.join().unwrap()), (7, 7));
        assert_eq!(runs.load(Ordering::SeqCst), 1, "computed twice");
        assert!(slot.is_done());
    }

    /// A worker that meets its own pool's running computation (further down its own stack, as when it picked up
    /// another job of the same file while waiting for parallel parts) computes again instead of waiting for itself.
    #[test]
    fn a_worker_never_waits_for_its_own_pool() {
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let pool = pool(Pool::Background, 1);
        let (s, n) = (slot.clone(), runs.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let before = recomputations();
        std::thread::spawn(move || {
            let r = pool.install(|| {
                s.get_or_compute(|| {
                    n.fetch_add(1, Ordering::SeqCst);
                    // nested on the same worker: the same slot again
                    s.get_or_compute(|| {
                        n.fetch_add(1, Ordering::SeqCst);
                        1
                    })
                })
            });
            tx.send(r).unwrap();
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(1), "the worker waited for itself");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(recomputations() - before, 1);
    }

    /// A worker of another pool waits for the running computation and gets the shared result.
    #[test]
    fn a_worker_waits_for_another_pool() {
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let background = pool(Pool::Background, 1);
        let spp = pool(Pool::Spp, 1);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (s, n) = (slot.clone(), runs.clone());
        let first = std::thread::spawn(move || {
            background.install(|| {
                s.get_or_compute(|| {
                    n.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(200));
                    3
                })
            })
        });
        started_rx.recv().unwrap();
        let second = spp.install(|| {
            slot.get_or_compute(|| {
                runs.fetch_add(1, Ordering::SeqCst);
                4
            })
        });
        assert_eq!((second, first.join().unwrap()), (3, 3));
        assert_eq!(runs.load(Ordering::SeqCst), 1, "computed twice");
    }

    /// Takeover tests count the global counter [`takeovers`]: one at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static ONE: Mutex<()> = Mutex::new(());
        ONE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A background computation (offline copy: background pool, intent idle) that gets no processor time: it holds
    /// the slot until `release` fires. Returns the thread with its result and a receiver that reports its start.
    fn starved_background(slot: Arc<Shared<u32>>, runs: Arc<AtomicUsize>, release: std::sync::mpsc::Receiver<()>, value: u32) -> (std::thread::JoinHandle<u32>, std::sync::mpsc::Receiver<()>) {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let background = pool(Pool::Background, 1);
        let h = std::thread::spawn(move || {
            background.install(move || {
                with_intent(Intent::Idle, || {
                    slot.get_or_compute(|| {
                        runs.fetch_add(1, Ordering::SeqCst);
                        started_tx.send(()).unwrap();
                        release.recv().unwrap();
                        value
                    })
                })
            })
        });
        (h, started_rx)
    }

    /// Priority inversion: opening a photo whose preprocessing an offline copy computes in the starved background.
    /// The visible caller waits only the grace period, computes itself and gets its own value; the background
    /// computation finishing later is dropped and its caller gets the stored value (written once).
    #[test]
    fn visible_caller_does_not_wait_for_a_starved_background() {
        let _one = serial();
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (bg, started) = starved_background(slot.clone(), runs.clone(), release_rx, 1);
        started.recv().unwrap();
        let before = takeovers();
        let t = std::time::Instant::now();
        let (s, n) = (slot.clone(), runs.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // a plain thread with the default intent (visible), as the job that opens the photo
            assert_eq!(intent(), Intent::Visible);
            let v = s.get_or_compute(|| {
                n.fetch_add(1, Ordering::SeqCst);
                2
            });
            tx.send(v).unwrap();
        });
        let v = rx.recv_timeout(Duration::from_secs(10)).expect("the visible caller waited for the background");
        let waited = t.elapsed();
        assert_eq!(v, 2, "computed itself");
        assert!(waited >= BACKGROUND_GRACE && waited < BACKGROUND_GRACE * 4, "waited {waited:?}");
        assert_eq!(takeovers() - before, 1);
        assert!(slot.is_done(), "the result is shared at once");
        // further callers (also the background) get the stored value without computing
        assert_eq!(slot.get_or_compute(|| 9), 2);
        release_tx.send(()).unwrap();
        assert_eq!(bg.join().unwrap(), 2, "the background's late result is dropped");
        assert_eq!(slot.get_or_compute(|| 9), 2, "written once");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// A background computation that finishes within the grace period is awaited (no double work).
    #[test]
    fn visible_caller_takes_a_background_result_within_the_grace() {
        let _one = serial();
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (bg, started) = starved_background(slot.clone(), runs.clone(), release_rx, 1);
        started.recv().unwrap();
        let before = takeovers();
        std::thread::spawn(move || {
            std::thread::sleep(BACKGROUND_GRACE / 5);
            release_tx.send(()).unwrap();
        });
        let v = slot.get_or_compute(|| {
            runs.fetch_add(1, Ordering::SeqCst);
            2
        });
        assert_eq!((v, bg.join().unwrap()), (1, 1));
        assert_eq!(runs.load(Ordering::SeqCst), 1, "computed twice");
        assert_eq!(takeovers(), before);
    }

    /// The background finishes first while the visible caller still computes: the first result wins, the visible
    /// caller gets it as well, nothing is written twice.
    #[test]
    fn first_finished_computation_wins() {
        let _one = serial();
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (bg, started) = starved_background(slot.clone(), runs.clone(), release_rx, 1);
        started.recv().unwrap();
        let (in_visible_tx, in_visible_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let s = slot.clone();
        let visible = std::thread::spawn(move || {
            s.get_or_compute(|| {
                in_visible_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                2
            })
        });
        in_visible_rx.recv_timeout(Duration::from_secs(10)).expect("no takeover");
        release_tx.send(()).unwrap();
        assert_eq!(bg.join().unwrap(), 1);
        go_tx.send(()).unwrap();
        assert_eq!(visible.join().unwrap(), 1, "the later result is dropped");
        assert_eq!(slot.get_or_compute(|| 9), 1);
    }

    /// While the visible caller computes after a takeover, more visible callers wait for it (no third computation),
    /// and a panic of the taken-over background computation does not empty the slot.
    #[test]
    fn after_a_takeover_others_wait_for_the_visible_computation() {
        let _one = serial();
        let slot = Arc::new(Shared::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        // background computation that panics when released
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let background = pool(Pool::Background, 1);
        let (s, n) = (slot.clone(), runs.clone());
        let bg = std::thread::spawn(move || {
            background.install(move || {
                with_intent(Intent::Idle, || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        s.get_or_compute(|| -> u32 {
                            n.fetch_add(1, Ordering::SeqCst);
                            started_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                            panic!("starved and gone")
                        })
                    }))
                    .is_err()
                })
            })
        });
        started_rx.recv().unwrap();
        let (in_visible_tx, in_visible_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let (s, n) = (slot.clone(), runs.clone());
        let first = std::thread::spawn(move || {
            s.get_or_compute(|| {
                n.fetch_add(1, Ordering::SeqCst);
                in_visible_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                2
            })
        });
        in_visible_rx.recv_timeout(Duration::from_secs(10)).expect("no takeover");
        release_tx.send(()).unwrap();
        assert!(bg.join().unwrap(), "background panicked");
        assert!(!slot.is_done());
        let (s, n) = (slot.clone(), runs.clone());
        let second = std::thread::spawn(move || {
            s.get_or_compute(|| {
                n.fetch_add(1, Ordering::SeqCst);
                3
            })
        });
        std::thread::sleep(BACKGROUND_GRACE * 2);
        go_tx.send(()).unwrap();
        assert_eq!((first.join().unwrap(), second.join().unwrap()), (2, 2));
        assert_eq!(runs.load(Ordering::SeqCst), 2, "computed a third time");
    }

    /// The background keeps waiting for the front, and the front keeps waiting for work ahead (intent soon): only a
    /// background computation is overtaken.
    #[test]
    fn background_and_soon_computations_are_still_awaited() {
        let _one = serial();
        let before = takeovers();
        for (computer, waiter) in [(Intent::Visible, Intent::Idle), (Intent::Soon, Intent::Visible), (Intent::Idle, Intent::Idle)] {
            let slot = Arc::new(Shared::<u32>::new());
            let runs = Arc::new(AtomicUsize::new(0));
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (s, n) = (slot.clone(), runs.clone());
            let first = std::thread::spawn(move || {
                with_intent(computer, || {
                    s.get_or_compute(|| {
                        n.fetch_add(1, Ordering::SeqCst);
                        started_tx.send(()).unwrap();
                        std::thread::sleep(BACKGROUND_GRACE * 2);
                        1
                    })
                })
            });
            started_rx.recv().unwrap();
            let v = with_intent(waiter, || {
                slot.get_or_compute(|| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    2
                })
            });
            assert_eq!((v, first.join().unwrap()), (1, 1), "{computer:?} computer, {waiter:?} waiter");
            assert_eq!(runs.load(Ordering::SeqCst), 1, "{computer:?} computer, {waiter:?} waiter: computed twice");
        }
        assert_eq!(takeovers(), before);
    }

    /// A plain thread computes in the global pool; a global worker must not wait for it (all workers could), a
    /// worker of a named pool may.
    #[test]
    fn waiting_rule() {
        assert!(may_wait(Thread::Plain, Thread::Plain));
        assert!(may_wait(Thread::Plain, Thread::Worker(Pool::Background)));
        assert!(!may_wait(Thread::Worker(Pool::Global), Thread::Plain));
        assert!(!may_wait(Thread::Worker(Pool::Background), Thread::Worker(Pool::Background)));
        assert!(may_wait(Thread::Worker(Pool::Background), Thread::Plain));
        assert!(may_wait(Thread::Worker(Pool::Spp), Thread::Worker(Pool::Background)));
        assert_eq!(current(), Thread::Plain);
        assert_eq!(pool(Pool::Film, 1).install(current), Thread::Worker(Pool::Film));
        let unnamed = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        assert_eq!(unnamed.install(current), Thread::Worker(Pool::Global));
    }

    #[test]
    fn panic_while_computing_frees_the_slot() {
        let slot = Shared::<u32>::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slot.get_or_compute(|| panic!("boom"))));
        assert!(r.is_err());
        assert!(!slot.is_done());
        assert_eq!(slot.get_or_compute(|| 5), 5);
    }

    /// Work from several threads runs one piece at a time in the pool (no photo nested on another); a worker of the
    /// pool itself never waits for the turn.
    #[test]
    fn turn_runs_one_piece_at_a_time() {
        static TURN: Turn = Turn::new(Pool::Background);
        let pool = Arc::new(pool(Pool::Background, 2));
        let (now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (now, most, pool) = (now.clone(), most.clone(), pool.clone());
                std::thread::spawn(move || {
                    TURN.run(&pool, || {
                        let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(n, Ordering::SeqCst);
                        // parallel parts, as the development has them; inside, the turn is not taken again
                        use rayon::prelude::*;
                        let s: u64 = (0..200_000u64).into_par_iter().map(|i| TURN.run(&pool, || i % 7)).sum();
                        std::thread::sleep(Duration::from_millis(30));
                        now.fetch_sub(1, Ordering::SeqCst);
                        s
                    })
                })
            })
            .collect();
        for t in threads {
            assert!(t.join().unwrap() > 0);
        }
        assert_eq!(most.load(Ordering::SeqCst), 1);
    }
}
