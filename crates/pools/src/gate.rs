//! The gate: one lock type for the places where graineo's computations wait for each other. Waiters are served by
//! intent ([`Intent::Visible`] before `Soon` before `Idle`, then in arrival order); a waiter with the same key as a
//! newer waiter is superseded and returns without the lock ([`Acquire::Superseded`]). `Idle` waiters enter only when
//! no `Visible` waiter is there and the hold-back hook (see [`Gate::set_hold_back`]) does not pull.
//!
//! The waiting rule of this crate (see the crate documentation) holds by construction:
//! - A gate **for a pool** ([`Gate::for_pool`]) lets a worker of that pool pass at once (nested work belongs to the
//!   piece in progress); every other thread waits for its turn.
//! - A gate **without a pool** ([`Gate::new`], a leaf lock around a computation that may use any pool) is taken only
//!   on plain threads: a worker waiting here could meanwhile pick up another job of its pool that wants the same
//!   gate. A debug build asserts both rules.
//!
//! **Lock order** (left before right): `OPEN`/`PREFETCH` → `RENDER_REGION`/`RENDER_FULL`/`RENDER_WIDE`/`SPP_BASE` → Sigma
//! `Session` → `FILM` → GPU mutexes of the subsystems; the background pool's turn (`background_run`) is taken
//! around whole pieces of work and never inside one of the gates above. While a gate is held, only gates further
//! right may be taken.
//!
//! The thread-local intent ([`set_intent`], [`intent`]) defaults to `Visible` (today's behaviour). It is not
//! inherited by threads spawned from the current one or by pool workers; set it explicitly where background is meant.

use std::cell::Cell;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{current, Pool, Thread};

/// Why a computation waits: for what the user sees now, for the next moments, or for nobody.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Intent {
    Idle = 0,
    Soon = 1,
    Visible = 2,
}

thread_local! {
    static INTENT: Cell<Intent> = const { Cell::new(Intent::Visible) };
}

/// Thread-local intent of the current thread (default `Visible`).
pub fn set_intent(i: Intent) {
    INTENT.with(|c| c.set(i));
}

pub fn intent() -> Intent {
    INTENT.with(|c| c.get())
}

/// Runs `f` with this intent and restores the previous one afterwards.
pub fn with_intent<R>(i: Intent, f: impl FnOnce() -> R) -> R {
    let old = intent();
    set_intent(i);
    let r = f();
    set_intent(old);
    r
}

struct Waiter {
    seq: u64,
    intent: Intent,
    key: Option<u64>,
    superseded: bool,
}

struct Inner {
    held: bool,
    holder: Option<String>,
    since: Option<Instant>,
    seq: u64,
    waiters: Vec<Waiter>,
    /// the waiter the free lock was granted to (it takes it on its next wake-up)
    granted: Option<u64>,
    hold_back: Option<fn() -> bool>,
    /// tests: names of the threads the gate went to, in order (only while recording, see [`Gate::record_grants`])
    grants: Option<Vec<String>>,
}

pub struct Gate {
    name: &'static str,
    /// the pool this gate guards (its workers pass at once), or none for a leaf lock
    pool: Option<Pool>,
    inner: Mutex<Inner>,
    signal: Condvar,
}

/// Holds the gate; dropping it releases the gate and wakes the waiters. A pass-through guard (worker of the gate's
/// own pool, nested) releases nothing.
pub struct Guard<'a> {
    gate: Option<&'a Gate>,
}

pub enum Acquire<'a> {
    Taken(Guard<'a>),
    /// A newer waiter with the same key arrived while this one waited: it does not get the lock.
    Superseded,
}

impl<'a> Acquire<'a> {
    pub fn taken(self) -> Option<Guard<'a>> {
        match self {
            Acquire::Taken(g) => Some(g),
            Acquire::Superseded => None,
        }
    }
}

/// Diagnosis of a gate: who holds it for how long, who waits.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub name: &'static str,
    /// is the gate held right now?
    pub held: bool,
    /// name of the holding thread, if it has one
    pub held_by: Option<String>,
    pub held_for_ms: u64,
    /// waiters per intent: `[Idle, Soon, Visible]`
    pub waiting: [usize; 3],
}

/// The hold-back hook is re-checked this often while an `Idle` waiter is held back.
const POLL: Duration = Duration::from_millis(50);

/// Measurement (`GRAINEO_LOG`): a wait over 20 ms at a gate is reported on stderr with the thread's name (the app
/// names its workers by lane): `gate <name>: waited <ms> ms (lane <thread>)`.
struct WaitLog {
    name: &'static str,
    since: Instant,
}

impl Drop for WaitLog {
    fn drop(&mut self) {
        static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let waited = self.since.elapsed();
        if waited.as_millis() > 20 && *LOG.get_or_init(|| std::env::var_os("GRAINEO_LOG").is_some()) {
            let thread = std::thread::current();
            eprintln!("gate {}: waited {} ms (lane {})", self.name, waited.as_millis(), thread.name().unwrap_or("?"));
        }
    }
}

const fn inner() -> Mutex<Inner> {
    Mutex::new(Inner { held: false, holder: None, since: None, seq: 0, waiters: Vec::new(), granted: None, hold_back: None, grants: None })
}

impl Gate {
    /// A leaf lock (taken only on plain threads).
    pub const fn new(name: &'static str) -> Gate {
        Gate { name, pool: None, inner: inner(), signal: Condvar::new() }
    }

    /// A gate in front of `pool`: its workers pass at once, everybody else waits for the turn.
    pub const fn for_pool(name: &'static str, pool: Pool) -> Gate {
        Gate { name, pool: Some(pool), inner: inner(), signal: Condvar::new() }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    fn state(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Is the current thread a worker of this gate's own pool (passes without waiting)?
    fn passes(&self) -> bool {
        matches!((self.pool, current()), (Some(p), Thread::Worker(w)) if p == w)
    }

    fn check_rule(&self) {
        debug_assert!(
            self.pool.is_some() || current() == Thread::Plain,
            "gate {} (leaf lock) taken on a pool worker ({:?}); it may pick up work that wants the same gate",
            self.name,
            current()
        );
    }

    /// Waits until the gate is free and this waiter is the best one (highest intent, then oldest). `key`: a newer
    /// waiter with the same key supersedes this one (`Superseded`, no lock). `Idle` waiters enter only when no
    /// `Visible` waiter is there and the hold-back hook does not pull.
    pub fn acquire(&self, intent: Intent, key: Option<u64>) -> Acquire<'_> {
        if self.passes() {
            return Acquire::Taken(Guard { gate: None });
        }
        self.check_rule();
        let mut inner = self.state();
        inner.seq += 1;
        let seq = inner.seq;
        if let Some(k) = key {
            for w in inner.waiters.iter_mut() {
                if w.key == Some(k) {
                    w.superseded = true;
                }
            }
        }
        inner.waiters.push(Waiter { seq, intent, key, superseded: false });
        // the superseded waiters must wake up to leave
        self.signal.notify_all();
        let _log = WaitLog { name: self.name, since: Instant::now() };
        loop {
            let me = inner.waiters.iter().position(|w| w.seq == seq).expect("waiter is registered");
            if inner.waiters[me].superseded {
                inner.waiters.remove(me);
                if inner.granted == Some(seq) {
                    inner.granted = None;
                }
                self.signal.notify_all();
                return Acquire::Superseded;
            }
            if !inner.held && inner.granted.is_none() {
                if let Some(best) = Self::best(&inner) {
                    inner.granted = Some(best);
                    if best != seq {
                        self.signal.notify_all();
                    }
                }
            }
            if inner.granted == Some(seq) {
                inner.waiters.remove(me);
                inner.granted = None;
                inner.held = true;
                inner.holder = std::thread::current().name().map(str::to_owned);
                inner.since = Some(Instant::now());
                Self::note_grant(&mut inner);
                return Acquire::Taken(Guard { gate: Some(self) });
            }
            inner = self.signal.wait_timeout(inner, POLL).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The waiter the free lock goes to: highest intent, then oldest; `Idle` only when no `Visible` waits and the
    /// hold-back hook does not pull.
    fn best(inner: &Inner) -> Option<u64> {
        let live = inner.waiters.iter().filter(|w| !w.superseded);
        let best = live.max_by(|a, b| a.intent.cmp(&b.intent).then(b.seq.cmp(&a.seq)))?;
        if best.intent == Intent::Idle && inner.hold_back.is_some_and(|f| f()) {
            return None;
        }
        Some(best.seq)
    }

    /// `acquire(intent(), key)`.
    pub fn acquire_current(&self, key: Option<u64>) -> Acquire<'_> {
        self.acquire(intent(), key)
    }

    /// Replacement for `Mutex<()>::lock()`: `acquire(intent(), None)`, never superseded.
    pub fn lock(&self) -> Guard<'_> {
        match self.acquire(intent(), None) {
            Acquire::Taken(g) => g,
            Acquire::Superseded => unreachable!("a waiter without key is never superseded"),
        }
    }

    /// Takes the gate only if it is free and nobody waits for it.
    pub fn try_lock(&self) -> Option<Guard<'_>> {
        if self.passes() {
            return Some(Guard { gate: None });
        }
        let mut inner = self.state();
        if inner.held || inner.granted.is_some() || inner.waiters.iter().any(|w| !w.superseded) {
            return None;
        }
        inner.held = true;
        inner.holder = std::thread::current().name().map(str::to_owned);
        inner.since = Some(Instant::now());
        Self::note_grant(&mut inner);
        Some(Guard { gate: Some(self) })
    }

    fn note_grant(inner: &mut Inner) {
        if let (Some(log), Some(h)) = (inner.grants.as_mut(), inner.holder.clone()) {
            log.push(h);
        }
    }

    /// Tests: from now on remember to which (named) thread the gate goes, in order – at the moment it is taken, not
    /// when the holder is done (the order of later work of the threads says nothing about the gate).
    pub fn record_grants(&self) {
        self.state().grants = Some(Vec::new());
    }

    /// The order recorded since [`Gate::record_grants`]; recording ends.
    pub fn take_grants(&self) -> Vec<String> {
        self.state().grants.take().unwrap_or_default()
    }

    /// Hold-back hook for `Idle` waiters (e.g. "the user is working right now"); re-checked every 50 ms.
    pub fn set_hold_back(&self, f: fn() -> bool) {
        self.state().hold_back = Some(f);
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.state();
        let mut waiting = [0usize; 3];
        for w in inner.waiters.iter().filter(|w| !w.superseded) {
            waiting[w.intent as usize] += 1;
        }
        Snapshot {
            name: self.name,
            held: inner.held,
            held_by: inner.holder.clone().filter(|_| inner.held),
            held_for_ms: inner.since.filter(|_| inner.held).map_or(0, |t| t.elapsed().as_millis() as u64),
            waiting,
        }
    }

    /// Is a waiter with at least this intent waiting?
    pub fn waiting(&self, at_least: Intent) -> bool {
        self.state().waiters.iter().any(|w| !w.superseded && w.intent >= at_least)
    }

    fn release(&self) {
        let mut inner = self.state();
        inner.held = false;
        inner.holder = None;
        inner.since = None;
        inner.granted = None;
        self.signal.notify_all();
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if let Some(g) = self.gate {
            g.release();
        }
    }
}

/// The gates of the app, in lock order (see the module documentation).
pub static OPEN: Gate = Gate::new("open");
pub static PREFETCH: Gate = Gate::new("prefetch");
pub static RENDER_REGION: Gate = Gate::new("region");
pub static RENDER_FULL: Gate = Gate::new("full");
/// Wide-gamut version of the preview for the display (one at a time; a newer one of the same photo supersedes a waiting one).
pub static RENDER_WIDE: Gate = Gate::new("wide");
/// In front of the Sigma base computation (the `Spp` pool): a worker of that pool passes.
pub static SPP_BASE: Gate = Gate::for_pool("spp", Pool::Spp);
/// In front of every film computation (the `Film` pool): a worker of that pool passes.
pub static FILM: Gate = Gate::for_pool("film", Pool::Film);

/// All gates (status display, tests); the background pool's turn is in `library::platform`.
pub fn all() -> [&'static Gate; 7] {
    [&OPEN, &PREFETCH, &RENDER_REGION, &RENDER_FULL, &RENDER_WIDE, &SPP_BASE, &FILM]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mark;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// Waits until `n` waiters are registered at the gate (at most 2 s).
    fn wait_for_waiters(g: &Gate, n: usize) {
        let t = Instant::now();
        while g.snapshot().waiting.iter().sum::<usize>() < n {
            assert!(t.elapsed() < Duration::from_secs(2), "waiters did not line up");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Holder holds; waiters with these intents line up (in this order); the holder releases; returns the order in
    /// which they were served.
    fn serve_order(g: &'static Gate, intents: &[Intent]) -> Vec<Intent> {
        let order = Arc::new(Mutex::new(Vec::new()));
        let holder = g.lock();
        let mut handles = Vec::new();
        for (k, &i) in intents.iter().enumerate() {
            // register in order: each waiter is in line before the next one starts
            let lined_up = Arc::new(Barrier::new(2));
            let (order, lined_up2) = (order.clone(), lined_up.clone());
            handles.push(thread::spawn(move || {
                lined_up2.wait();
                let guard = g.acquire(i, None).taken().expect("no key, never superseded");
                order.lock().unwrap().push(i);
                drop(guard);
            }));
            lined_up.wait();
            wait_for_waiters(g, k + 1);
        }
        drop(holder);
        for h in handles {
            h.join().unwrap();
        }
        let v = order.lock().unwrap().clone();
        v
    }

    #[test]
    fn visible_waiter_is_served_before_idle_waiter() {
        static G: Gate = Gate::new("test-order-2");
        assert_eq!(serve_order(&G, &[Intent::Idle, Intent::Visible]), vec![Intent::Visible, Intent::Idle]);
    }

    #[test]
    fn soon_between_visible_and_idle() {
        static G: Gate = Gate::new("test-order-3");
        assert_eq!(serve_order(&G, &[Intent::Idle, Intent::Soon, Intent::Visible]), vec![Intent::Visible, Intent::Soon, Intent::Idle]);
    }

    #[test]
    fn same_key_supersedes_the_older_waiter() {
        static G: Gate = Gate::new("test-key");
        let holder = G.lock();
        let a_result = Arc::new(Mutex::new(None));
        let a = {
            let r = a_result.clone();
            thread::spawn(move || {
                let t = Instant::now();
                let outcome = matches!(G.acquire(Intent::Visible, Some(7)), Acquire::Superseded);
                *r.lock().unwrap() = Some((outcome, t.elapsed()));
            })
        };
        wait_for_waiters(&G, 1);
        let b = thread::spawn(move || G.acquire(Intent::Visible, Some(7)).taken().is_some());
        // A returns at once, although the holder still holds
        a.join().unwrap();
        let (superseded, took) = a_result.lock().unwrap().unwrap();
        assert!(superseded, "the older waiter must be superseded");
        assert!(took < Duration::from_millis(100), "superseded waiter took {took:?}");
        assert_eq!(G.snapshot().waiting, [0, 0, 1]);
        assert!(G.snapshot().held_by.is_some(), "holder undisturbed");
        drop(holder);
        assert!(b.join().unwrap(), "the newer waiter takes the gate");
    }

    #[test]
    fn key_does_not_supersede_the_holder() {
        static G: Gate = Gate::new("test-key-holder");
        let holder = G.acquire(Intent::Visible, Some(7)).taken().unwrap();
        let w = thread::spawn(move || G.acquire(Intent::Visible, Some(7)).taken().is_some());
        wait_for_waiters(&G, 1);
        thread::sleep(Duration::from_millis(60));
        assert!(G.snapshot().held_by.is_some());
        assert_eq!(G.snapshot().waiting, [0, 0, 1]);
        drop(holder);
        assert!(w.join().unwrap());
    }

    static HOLD: AtomicBool = AtomicBool::new(false);
    fn hold() -> bool {
        HOLD.load(Ordering::SeqCst)
    }

    #[test]
    fn idle_waits_while_hold_back_is_true_then_proceeds() {
        static G: Gate = Gate::new("test-hold-back");
        G.set_hold_back(hold);
        HOLD.store(true, Ordering::SeqCst);
        let t = Instant::now();
        let w = thread::spawn(move || {
            let g = G.acquire(Intent::Idle, None).taken().unwrap();
            let took = t.elapsed();
            drop(g);
            took
        });
        wait_for_waiters(&G, 1);
        thread::sleep(Duration::from_millis(150));
        assert_eq!(G.snapshot().waiting, [1, 0, 0], "idle must still wait");
        HOLD.store(false, Ordering::SeqCst);
        let took = w.join().unwrap();
        assert!(took >= Duration::from_millis(150) && took < Duration::from_millis(400), "took {took:?}");
    }

    #[test]
    fn idle_proceeds_when_no_visible_arrives() {
        static G: Gate = Gate::new("test-idle-only");
        let done = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let d = done.clone();
                thread::spawn(move || {
                    let _g = G.acquire(Intent::Idle, None).taken().unwrap();
                    d.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(done.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn fifo_within_same_intent() {
        static G: Gate = Gate::new("test-fifo");
        let holder = G.lock();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for k in 0..5 {
            let order = order.clone();
            handles.push(thread::spawn(move || {
                let g = G.acquire(Intent::Visible, None).taken().unwrap();
                order.lock().unwrap().push(k);
                drop(g);
            }));
            wait_for_waiters(&G, k + 1);
        }
        drop(holder);
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn poisoned_holder_releases() {
        static G: Gate = Gate::new("test-poison");
        let r = thread::spawn(|| {
            let _g = G.lock();
            panic!("holder dies");
        })
        .join();
        assert!(r.is_err());
        let t = Instant::now();
        let g = G.lock();
        assert!(t.elapsed() < Duration::from_millis(100));
        drop(g);
    }

    #[test]
    fn lock_is_a_drop_in_for_mutex() {
        static G: Gate = Gate::new("test-drop-in");
        let t = Instant::now();
        let g = G.lock();
        assert!(t.elapsed() < Duration::from_millis(50));
        assert!(G.try_lock().is_none());
        drop(g);
        assert!(G.try_lock().is_some());
    }

    #[test]
    fn thread_local_intent_default_is_visible() {
        thread::spawn(|| assert_eq!(intent(), Intent::Visible)).join().unwrap();
    }

    #[test]
    fn with_intent_restores() {
        thread::spawn(|| {
            set_intent(Intent::Soon);
            let inside = with_intent(Intent::Idle, intent);
            assert_eq!(inside, Intent::Idle);
            assert_eq!(intent(), Intent::Soon);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn snapshot_reports_waiters() {
        static G: Gate = Gate::new("test-snapshot");
        let holder = G.lock();
        let a = thread::spawn(|| drop(G.acquire(Intent::Soon, None)));
        wait_for_waiters(&G, 1);
        let b = thread::spawn(|| drop(G.acquire(Intent::Visible, None)));
        wait_for_waiters(&G, 2);
        let s = G.snapshot();
        assert_eq!(s.waiting, [0, 1, 1]);
        assert_eq!(s.name, "test-snapshot");
        assert!(s.held_by.is_some());
        assert!(G.waiting(Intent::Visible));
        drop(holder);
        a.join().unwrap();
        b.join().unwrap();
        assert_eq!(G.snapshot().waiting, [0, 0, 0]);
        assert!(G.snapshot().held_by.is_none());
    }

    /// A worker of the gate's own pool passes at once (nested work), even while a plain thread holds the gate; a
    /// worker of another pool waits.
    #[test]
    fn own_pool_passes_other_pool_waits() {
        static G: Gate = Gate::for_pool("test-pool-gate", Pool::Film);
        let film = rayon::ThreadPoolBuilder::new().num_threads(1).start_handler(|_| mark(Pool::Film)).build().unwrap();
        let spp = rayon::ThreadPoolBuilder::new().num_threads(1).start_handler(|_| mark(Pool::Spp)).build().unwrap();
        let holder = G.lock();
        let t = Instant::now();
        assert!(film.install(|| G.lock()).gate.is_none(), "own worker passes with a pass-through guard");
        assert!(t.elapsed() < Duration::from_millis(50));
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            spp.install(|| {
                let g = G.lock();
                tx.send(()).unwrap();
                drop(g);
            })
        });
        wait_for_waiters(&G, 1);
        assert!(rx.try_recv().is_err(), "another pool's worker must wait");
        drop(holder);
        waiter.join().unwrap();
        assert!(rx.recv().is_ok());
    }
}
