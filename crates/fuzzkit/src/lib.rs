//! Small mutation fuzzer that runs on stable Rust inside the normal `cargo test`.
//!
//! Every target is a plain `#[test]` that calls [`run`] with a few seed inputs and a closure
//! that feeds the bytes to one of our readers of foreign data (XMP, Lightroom values, X3F,
//! makernotes, ONNX …). The fuzzer mutates the seeds (bit flips, byte replacements, interesting
//! integers, insertions, deletions, splices, truncation) and reports
//!
//! * panics (caught per input, the input is printed and saved),
//! * hangs (a watchdog aborts when one input runs longer than `GRAINEO_FUZZ_HANG_SECS`, default 10),
//! * huge allocations (with [`CapAlloc`] as global allocator: one allocation above
//!   `GRAINEO_FUZZ_MAX_ALLOC_MB`, default 1024 MiB, aborts).
//!
//! Environment:
//! * `GRAINEO_FUZZ_ITERS` – inputs per target (default 400, short enough for every test run)
//! * `GRAINEO_FUZZ_SECS` – run each target for this many seconds instead (0 = use the count)
//! * `GRAINEO_FUZZ_SEED` – start value of the random generator (default: fixed, reproducible)
//! * `GRAINEO_FUZZ_OUT` – folder for crashing inputs (default: the system temp folder)
//!
//! A saved input can be replayed with `GRAINEO_FUZZ_REPLAY=<file>`: the target then runs only
//! that input, without catching the panic.

use std::alloc::{GlobalAlloc, Layout, System};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

/// Deterministic random generator (splitmix64).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

const INTERESTING: [u32; 14] = [
    0, 1, 2, 0x7f, 0x80, 0xff, 0x100, 0x7fff, 0x8000, 0xffff, 0x10000, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff,
];

/// Returns a mutated copy of `input`; `pool` provides material for splicing.
pub fn mutate(rng: &mut Rng, input: &[u8], pool: &[Vec<u8>], max_len: usize) -> Vec<u8> {
    let mut d = input.to_vec();
    let rounds = 1 + rng.below(6);
    for _ in 0..rounds {
        let len = d.len();
        match rng.below(10) {
            0 if len > 0 => {
                let i = rng.below(len);
                d[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                d[i] = rng.next() as u8;
            }
            2 if len > 0 => {
                // interesting integer, 1/2/4 bytes, either byte order
                let v = INTERESTING[rng.below(INTERESTING.len())];
                let w = [1usize, 2, 4][rng.below(3)];
                let bytes = if rng.chance(2) { v.to_le_bytes() } else { v.to_be_bytes() };
                let i = rng.below(len);
                for k in 0..w {
                    if i + k < len {
                        d[i + k] = if rng.chance(2) { bytes[k] } else { bytes[4 - w + k] };
                    }
                }
            }
            3 if len > 0 => {
                // delete a range
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(64));
                d.drain(i..i + n);
            }
            4 => {
                // insert random bytes
                let i = rng.below(len + 1);
                let n = 1 + rng.below(16);
                let ins: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                d.splice(i..i, ins);
            }
            5 if len > 1 => {
                // duplicate a range
                let a = rng.below(len);
                let n = 1 + rng.below((len - a).min(256));
                let chunk = d[a..a + n].to_vec();
                let i = rng.below(len + 1);
                d.splice(i..i, chunk);
            }
            6 if !pool.is_empty() => {
                // splice with another seed
                let o = &pool[rng.below(pool.len())];
                if !o.is_empty() {
                    let a = rng.below(o.len());
                    let n = 1 + rng.below((o.len() - a).min(512));
                    let i = rng.below(len + 1);
                    let j = (i + rng.below(n + 1)).min(len);
                    d.splice(i..j, o[a..a + n].iter().copied());
                }
            }
            7 if len > 0 => {
                // truncate
                d.truncate(rng.below(len));
            }
            8 if len > 0 => {
                // ASCII-aware: replace a byte with a structural character
                const ASCII: &[u8] = b"<>/=\"' \n:;,.{}[]-0123456789eE+&#";
                let i = rng.below(len);
                d[i] = ASCII[rng.below(ASCII.len())];
            }
            _ if len > 0 => {
                // overwrite a range with copies of one byte
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(32));
                let b = [0u8, 0xff, b'9', b' '][rng.below(4)];
                d[i..i + n].fill(b);
            }
            _ => d.push(rng.next() as u8),
        }
    }
    d.truncate(max_len);
    d
}

// ---------------------------------------------------------------------------------------------
// global allocator with a cap on single allocations

static CURRENT: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);
static MAX_ALLOC: AtomicUsize = AtomicUsize::new(0);
static STARTED_MS: AtomicU64 = AtomicU64::new(0);
static EPOCH: Mutex<Option<Instant>> = Mutex::new(None);

/// Global allocator for fuzz test binaries: forwards to the system allocator, but aborts (after
/// saving the current input) when a single allocation exceeds the cap. Declare it with
/// `#[global_allocator] static A: fuzzkit::CapAlloc = fuzzkit::CapAlloc;`.
pub struct CapAlloc;

unsafe impl GlobalAlloc for CapAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        check_size(l.size());
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        check_size(l.size());
        System.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        check_size(n);
        System.realloc(p, l, n)
    }
}

thread_local! {
    /// set while the fuzz thread runs a target (other tests of the binary are not limited)
    static ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn check_size(size: usize) {
    if !ACTIVE.try_with(|a| a.get()).unwrap_or(false) {
        return;
    }
    let cap = MAX_ALLOC.load(Ordering::Relaxed);
    if cap != 0 && size > cap {
        MAX_ALLOC.store(0, Ordering::Relaxed); // no recursion while reporting
        report_and_abort(&format!("allocation of {} MiB", size >> 20));
    }
}

fn report_and_abort(what: &str) -> ! {
    if let Ok(g) = CURRENT.try_lock() {
        if let Some((name, data)) = g.as_ref() {
            let p = save(name, data);
            raw_err(&format!("fuzz target {name}: {what}; input saved to {}\n", p.display()));
        }
    }
    raw_err(&format!("fuzz: {what} - abort\n"));
    std::process::abort();
}

/// Writes to stderr directly (not through the test harness capture, which is lost on abort).
fn raw_err(s: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(s.as_bytes());
}

fn now_ms() -> u64 {
    let mut e = EPOCH.lock().unwrap_or_else(|e| e.into_inner());
    e.get_or_insert_with(Instant::now).elapsed().as_millis() as u64 + 1
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn save(name: &str, data: &[u8]) -> PathBuf {
    let dir = std::env::var_os("GRAINEO_FUZZ_OUT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h = (h ^ *b as u64).wrapping_mul(0x100_0000_01b3);
    }
    let p = dir.join(format!("fuzz-{name}-{h:016x}.bin"));
    let _ = std::fs::write(&p, data);
    p
}

fn start_watchdog() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let limit = env_u64("GRAINEO_FUZZ_HANG_SECS", 10) * 1000;
        let _ = std::thread::Builder::new().name("fuzz-watchdog".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            let s = STARTED_MS.load(Ordering::Relaxed);
            if s != 0 && now_ms().saturating_sub(s) > limit {
                report_and_abort(&format!("one input ran longer than {} s (hang)", limit / 1000));
            }
        });
    });
}

/// Fuzzes `target` with mutations of `seeds`. `max_len` bounds the input size.
/// Panics (failing the test) with the saved input path when the target panics.
pub fn run(name: &str, seeds: &[Vec<u8>], max_len: usize, target: impl Fn(&[u8])) {
    if let Some(p) = std::env::var_os("GRAINEO_FUZZ_REPLAY") {
        let data = std::fs::read(&p).expect("replay file");
        if PathBuf::from(&p).file_name().map(|n| n.to_string_lossy().contains(&format!("fuzz-{name}-"))) == Some(true)
        {
            target(&data);
        }
        return;
    }
    // one target at a time: the watchdog and the allocation cap report the current input
    static RUN: Mutex<()> = Mutex::new(());
    let _serial = RUN.lock().unwrap_or_else(|e| e.into_inner());
    let iters = env_u64("GRAINEO_FUZZ_ITERS", 400);
    let secs = env_u64("GRAINEO_FUZZ_SECS", 0);
    let mut rng = Rng::new(env_u64("GRAINEO_FUZZ_SEED", 0x6772_6169_6e65_6f00) ^ fnv(name));
    MAX_ALLOC.store((env_u64("GRAINEO_FUZZ_MAX_ALLOC_MB", 1024) as usize) << 20, Ordering::Relaxed);
    start_watchdog();
    let t0 = Instant::now();
    let mut corpus: Vec<Vec<u8>> = seeds.to_vec();
    if corpus.is_empty() {
        corpus.push(Vec::new());
    }
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut failure = None;
    let mut n = 0u64;
    // seeds first, unchanged
    let mut queue: Vec<Vec<u8>> = corpus.clone();
    loop {
        let input = match queue.pop() {
            Some(i) => i,
            None => {
                let done = if secs > 0 { t0.elapsed().as_secs() >= secs } else { n >= iters };
                if done {
                    break;
                }
                let base = &corpus[rng.below(corpus.len())];
                mutate(&mut rng, base, &corpus, max_len)
            }
        };
        n += 1;
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some((name.to_string(), input.clone()));
        STARTED_MS.store(now_ms(), Ordering::Relaxed);
        ACTIVE.with(|a| a.set(true));
        let r = panic::catch_unwind(AssertUnwindSafe(|| target(&input)));
        ACTIVE.with(|a| a.set(false));
        STARTED_MS.store(0, Ordering::Relaxed);
        if let Err(e) = r {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            failure = Some((save(name, &input), msg, input.len()));
            break;
        }
        // keep a few mutants as new bases so the search goes deeper than one step
        if corpus.len() < 64 && rng.chance(16) {
            corpus.push(input);
        }
    }
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    MAX_ALLOC.store(0, Ordering::Relaxed);
    panic::set_hook(prev_hook);
    if std::env::var_os("GRAINEO_FUZZ_VERBOSE").is_some() {
        eprintln!("fuzz {name}: {n} inputs in {:.1} s", t0.elapsed().as_secs_f64());
    }
    if let Some((p, msg, len)) = failure {
        panic!(
            "fuzz target {name} panicked on a {len}-byte input after {n} inputs: {msg}\n  saved: {}\n  replay: GRAINEO_FUZZ_REPLAY={} cargo test …",
            p.display(),
            p.display()
        );
    }
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Writes `data` to a fresh file in a per-process scratch folder and returns its path
/// (for readers that only take a path). The file is overwritten on the next call with the same `name`.
pub fn temp_file(name: &str, data: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("graineo-fuzz-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(name);
    std::fs::write(&p, data).expect("write fuzz input");
    p
}
