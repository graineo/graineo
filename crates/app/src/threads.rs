//! Threads of the app: one compute pool for the work in front, one shared background pool (in `library::platform`)
//! and a few kept worker threads for the jobs of [`crate::jobs`].
//!
//! - Front compute pool (rayon's global pool): one thread per core ([`library::platform::foreground_threads`]) with
//!   priority "user initiated" – opening, rendering, zooming, export. Background work (offline copies, large previews,
//!   faces) has its own small pool ([`library::platform::background_pool`], lowest priority, efficiency cores) and
//!   pauses while the user works, so the two do not compete for the same cores.
//! - Job workers ([`spawn`]): kept threads per [`Lane`] instead of a new thread per job. Each lane has its own
//!   priority. A job never waits behind another one: when all workers of a lane are busy (a job can wait at a gate
//!   for seconds), another worker is started; workers above the kept number end after a short idle time.
//!
//! Rule for all pools: never hold a lock (Mutex, gate) while parallel work runs on a rayon worker thread – a worker
//! that waits for its parallel parts may meanwhile pick up another job of its pool, and if that one wants the same
//! lock, both wait forever. Job workers are plain threads (no rayon workers), so the gates in `jobs` stay safe.
//! Every pool marks its threads (`pools::mark`), so shared computations (`pools::Shared`) know whether a caller may
//! wait; work is handed to the background pool only through `library::platform::background_run` (one piece at a
//! time, `pools::Turn`).

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use library::platform::{set_priority, Priority};

/// Set up the front compute pool (once, at the very start, before anything uses rayon).
pub fn init() {
    let mut b = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("rechnen-{i}")).start_handler(|_| {
        pools::mark(pools::Pool::Global);
        set_priority(Priority::UserInitiated)
    });
    // RAYON_NUM_THREADS (measurements) still wins
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        b = b.num_threads(library::platform::foreground_threads());
    }
    if let Err(e) = b.build_global() {
        crate::log(&format!("threads: global pool already set up ({e})"));
    }
}

/// Which workers run a job (and with which priority).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// What the user is waiting for right now (opening, rendering, zoom, export on request): "user initiated".
    Front,
    /// Work for the next moments (preloading the neighbour, full resolution ahead, SPP base ahead): "utility".
    Ahead,
    /// Work nobody waits for: "background" (efficiency cores).
    Back,
}

impl Lane {
    fn priority(self) -> Priority {
        match self {
            Lane::Front => Priority::UserInitiated,
            Lane::Ahead => Priority::Utility,
            Lane::Back => Priority::Background,
        }
    }
    /// Intent at the gates (`pools::Gate`): what the user waits for goes first, the next moments after it, the
    /// background last.
    pub fn intent(self) -> pools::Intent {
        match self {
            Lane::Front => pools::Intent::Visible,
            Lane::Ahead => pools::Intent::Soon,
            Lane::Back => pools::Intent::Idle,
        }
    }
    /// Workers kept even when idle.
    fn keep(self) -> usize {
        match self {
            Lane::Front => 3,
            Lane::Ahead => 2,
            Lane::Back => 1,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Lane::Front => "job-vorne",
            Lane::Ahead => "job-voraus",
            Lane::Back => "job-hinten",
        }
    }
}

type Task = Box<dyn FnOnce() + Send + 'static>;

#[derive(Default)]
struct State {
    queue: VecDeque<Task>,
    /// workers waiting for a task
    idle: usize,
    /// workers alive
    threads: usize,
    /// numbering of the thread names
    started: usize,
}

struct Pool {
    lane: Lane,
    state: Mutex<State>,
    signal: Condvar,
}

/// Workers above the kept number end after this idle time.
const LINGER: Duration = Duration::from_secs(10);

fn pool(lane: Lane) -> &'static Pool {
    static POOLS: OnceLock<[Pool; 3]> = OnceLock::new();
    let p = POOLS.get_or_init(|| [Lane::Front, Lane::Ahead, Lane::Back].map(|lane| Pool { lane, state: Mutex::new(State::default()), signal: Condvar::new() }));
    &p[lane as usize]
}

/// Run `task` on a worker of `lane`; returns at once.
pub fn spawn(lane: Lane, task: impl FnOnce() + Send + 'static) {
    let p = pool(lane);
    let mut s = p.state.lock().unwrap_or_else(|e| e.into_inner());
    s.queue.push_back(Box::new(task));
    if s.idle >= s.queue.len() {
        // enough waiting workers for every queued task
        p.signal.notify_one();
        return;
    }
    s.threads += 1;
    s.started += 1;
    let name = format!("{}-{}", lane.name(), s.started);
    drop(s);
    if std::thread::Builder::new().name(name).spawn(move || worker(p)).is_err() {
        // no thread to be had: run the task here, as the old way would have failed too
        let task = {
            let mut s = p.state.lock().unwrap_or_else(|e| e.into_inner());
            s.threads -= 1;
            s.queue.pop_back()
        };
        if let Some(t) = task {
            t();
        }
    }
}

fn worker(p: &'static Pool) {
    loop {
        let task = {
            let mut s = p.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(t) = s.queue.pop_front() {
                    break t;
                }
                s.idle += 1;
                let (g, timeout) = p.signal.wait_timeout(s, LINGER).unwrap_or_else(|e| e.into_inner());
                s = g;
                s.idle -= 1;
                if timeout.timed_out() && s.queue.is_empty() && s.threads > p.lane.keep() {
                    s.threads -= 1;
                    return;
                }
            }
        };
        // a job may have changed the priority of its thread: back to the lane's one before each job; the gates
        // serve this job by the lane's intent
        set_priority(p.lane.priority());
        pools::set_intent(p.lane.intent());
        // jobs catch their own panics; a panic here must not shrink the pool unnoticed
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err() {
            crate::log("job worker: task panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Each lane's tasks run with the lane's intent at the gates.
    #[test]
    fn lane_sets_intent_for_its_tasks() {
        for (lane, want) in [(Lane::Front, pools::Intent::Visible), (Lane::Ahead, pools::Intent::Soon), (Lane::Back, pools::Intent::Idle)] {
            let (tx, rx) = std::sync::mpsc::channel();
            spawn(lane, move || tx.send(pools::intent()).unwrap());
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), want, "{lane:?}");
        }
    }

    /// Every task runs, also when many are sent at once and some block for a while (no task waits behind a
    /// blocked one), and the workers above the kept number end again.
    #[test]
    fn all_tasks_run_without_waiting_behind_blocked_ones() {
        let done = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        // five tasks block until released
        for _ in 0..5 {
            let (d, g) = (done.clone(), gate.clone());
            spawn(Lane::Back, move || {
                let (m, cv) = &*g;
                let mut open = m.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
                d.fetch_add(1, Ordering::SeqCst);
            });
        }
        // a sixth one finishes although all others are blocked
        let quick = Arc::new(AtomicUsize::new(0));
        let q = quick.clone();
        spawn(Lane::Back, move || {
            q.fetch_add(1, Ordering::SeqCst);
        });
        let t = std::time::Instant::now();
        while quick.load(Ordering::SeqCst) == 0 {
            assert!(t.elapsed() < Duration::from_secs(5), "task waited behind blocked ones");
            std::thread::sleep(Duration::from_millis(5));
        }
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        while done.load(Ordering::SeqCst) < 5 {
            assert!(t.elapsed() < Duration::from_secs(5), "blocked tasks did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        // many short tasks, a few milliseconds apart like jobs from the window, reuse the workers (no thread per task)
        let many = Arc::new(AtomicUsize::new(0));
        let before = pool(Lane::Ahead).state.lock().unwrap().started;
        for _ in 0..100 {
            let m = many.clone();
            spawn(Lane::Ahead, move || {
                m.fetch_add(1, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        while many.load(Ordering::SeqCst) < 100 {
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = pool(Lane::Ahead).state.lock().unwrap().started - before;
        assert!(started <= 10, "a thread per task: {started} threads for 100 tasks");
    }
}
