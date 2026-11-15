//! Regression for lock-ups between the pools: several X3F offline copies at the same time (shared background pool,
//! Sigma noise reduction on the GPU) while an X3F is opened, its SPP base computed and its full resolution rendered in
//! front – like paging through Sigma photos while the library makes offline copies. Every part must finish; a lock held
//! over parallel work would leave them waiting for each other forever (then nothing computes any more). And no
//! preprocessing of the same file runs twice at the same time (`pools::recomputations`).
//!
//! Needs `GRAINEO_DATA_DIR` and X3F files (`GRAINEO_TEST_X3F`, separated by `;`, otherwise the files in
//! `GRAINEO_TEST_MERRILL_DIR` and `testbilder/`, only read); skipped without them.

use super::*;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn x3f_files() -> Vec<PathBuf> {
    if let Ok(v) = crate::env::var("GRAINEO_TEST_X3F") {
        return v.split(';').map(PathBuf::from).filter(|p| p.is_file()).collect();
    }
    let merrill = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(PathBuf::from).unwrap_or_default();
    // testbilder/: next to the free RAWs (GRAINEO_TEST_RAW_DIR=…/testbilder/signature-edits) or in the repository
    let pictures = crate::env::var_os("GRAINEO_TEST_RAW_DIR")
        .and_then(|d| PathBuf::from(d).parent().map(PathBuf::from))
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder"));
    [
        merrill.join("DP2M5418.X3F"),
        merrill.join("DP2M5412.X3F"),
        pictures.join("dp2q/imaging-resource/DP2QINBI0100.X3F"),
        pictures.join("dp2q/imaging-resource/DP2QOUTBAP1.X3F"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .collect()
}

/// Does a thread of the test's priority get processor time right now? A thread that only computes (utility) runs
/// for two seconds; if the process gains less than a tenth of a second meanwhile, the computer is so overloaded that
/// the scheduler gives this priority nothing – then a minute without processor time says nothing about a lock-up.
fn priority_starved() -> bool {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let before = library::platform::cpu_seconds().unwrap_or(0.0);
    // not joined: a starved thread ends whenever it gets its turn
    std::thread::spawn(move || {
        library::platform::set_priority(library::platform::Priority::Utility);
        while !s.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
    });
    std::thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
    library::platform::cpu_seconds().unwrap_or(0.0) - before < 0.1
}

#[test]
fn parallel_x3f_offline_copies_and_foreground_never_lock_up() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let files = x3f_files();
    if files.len() < 2 {
        eprintln!("skipped: fewer than two X3F files");
        return;
    }
    // uses all gates: not at the same time as the gate tests (they count the waiters)
    let _serial = global_gates_for_tests();
    // A lock-up is a matter of the waiting logic, not of priorities: the background work runs as "utility" here.
    // Kept on the efficiency cores, the system's own background work (malware scan of fresh builds, Spotlight) could
    // starve it for minutes; the check below then reported a lock-up although the workers were ready to compute.
    library::platform::background_as_utility(true);
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            // not after a failure: a locked-up pool would never run the reset
            if !std::thread::panicking() {
                library::platform::background_as_utility(false);
            }
        }
    }
    let _restore = Restore;
    let (done_tx, done_rx) = channel::<String>();
    let mut parts = 0;
    // offline copies: two rounds over all files, each from its own plain thread (like the preview generator)
    for round in 0..2 {
        for p in &files {
            let (p, tx) = (p.clone(), done_tx.clone());
            parts += 1;
            std::thread::spawn(move || {
                let r = crate::large_preview::render_offline(&p, 1024);
                let _ = tx.send(format!("offline copy {} (round {round}): {}", p.display(), r.map(|i| format!("{}×{}", i.width(), i.height())).unwrap_or_else(|e| e)));
            });
        }
    }
    // front: SPP base of the second file, and opening the first one with its full resolution (job workers)
    let front = files[0].clone();
    let spp_file = files[1].clone();
    {
        let tx = done_tx.clone();
        parts += 1;
        crate::threads::spawn(crate::threads::Lane::Front, move || {
            let mode = Settings::default().spp_key(&spp_file);
            run_inner(Job::Spp { index: 1, path: spp_file.clone(), session: None, mode, cancel: Arc::new(AtomicBool::new(false)), prefetch: false }, &|_| {});
            let _ = tx.send(format!("spp base {}", spp_file.display()));
        });
    }
    {
        let tx = done_tx.clone();
        parts += 1;
        crate::threads::spawn(crate::threads::Lane::Front, move || {
            let current = Arc::new(AtomicUsize::new(0));
            let opened: Mutex<Option<Loaded>> = Mutex::new(None);
            run_inner(Job::Open { index: 0, path: front.clone(), settings: Settings::default(), generation: 0, current: current.clone() }, &|r| {
                if let JobResult::Opened { loaded, .. } = r {
                    *opened.lock().unwrap() = Some(loaded);
                }
            });
            let Some(loaded) = opened.into_inner().unwrap() else {
                let _ = tx.send(format!("open {} failed", front.display()));
                return;
            };
            let full = Mutex::new(String::from("no result"));
            let job = Job::RenderFull { index: 0, session: loaded.session.clone(), foreign: loaded.previews.foreign.clone(), settings: Settings::default(), generation: 0, current, edit: EditStamp::none() };
            run_inner(job, &|r| {
                *full.lock().unwrap() = match r {
                    JobResult::FullRendered { width, height, .. } => format!("{width}×{height}"),
                    _ => "failed".into(),
                }
            });
            let _ = tx.send(format!("open + full resolution {}: {}", front.display(), full.into_inner().unwrap()));
        });
    }
    {
        // preloading the neighbour at the same time (its own gate, intent "soon")
        let (tx, next) = (done_tx.clone(), files[files.len() - 1].clone());
        parts += 1;
        crate::threads::spawn(crate::threads::Lane::Front, move || {
            let got = Mutex::new("no result");
            run_inner(Job::Prefetch { index: 2, path: next.clone(), settings: Settings::default(), cancel: Arc::new(AtomicBool::new(false)) }, &|r| {
                *got.lock().unwrap() = match r {
                    JobResult::Opened { prefetch: true, .. } => "preloaded",
                    _ => "failed",
                }
            });
            let _ = tx.send(format!("prefetch {}: {}", next.display(), got.into_inner().unwrap()));
        });
    }
    drop(done_tx);
    // A lock-up computes nothing any more. On a busy computer (the whole test suite, a build) the parts may take many
    // minutes – that is no lock-up. So: fail when no part finished and the process used (almost) no processor time
    // for a minute.
    let cpu = || library::platform::cpu_seconds().unwrap_or(0.0);
    let (start, cpu0, recomputed0) = (std::time::Instant::now(), cpu(), pools::recomputations());
    let mut window_cpu = cpu0;
    let mut k = 0;
    while k < parts {
        match done_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(m) => {
                k += 1;
                eprintln!("{k}/{parts} {m} ({:.0} s)", start.elapsed().as_secs_f32());
                window_cpu = cpu();
            }
            Err(_) => {
                // measured before the probe (its thread counts for the process as well)
                let used = cpu() - window_cpu;
                if used < 0.25 && priority_starved() {
                    eprintln!("no processor time for a minute, but a thread that only computes gets none either: the computer is overloaded, waiting on");
                } else {
                    assert!(used >= 0.25, "lock-up: {k} of {parts} parts finished, no processor time for a minute; gates: {}", pools::gates_overview());
                }
                window_cpu = cpu();
                assert!(start.elapsed() < Duration::from_secs(45 * 60), "only {k} of {parts} parts finished within 45 minutes");
            }
        }
    }
    eprintln!("all {parts} parts in {:.0} s, {:.0} s processor time", start.elapsed().as_secs_f32(), cpu() - cpu0);
    // the same file is never preprocessed twice at the same time: the offline copies take their turn in the
    // background pool one at a time, the front waits for a running preprocessing outside its own pool
    assert_eq!(pools::recomputations() - recomputed0, 0, "a shared computation ran twice");
}

/// Measurement (only with `GRAINEO_MEASURE_PAGING=<folder with X3F files>`): paging through about 50 Sigma photos
/// as in Develop – the base of the current photo in front, the next one ahead, most photos only glanced at (0.3 s),
/// every fifth looked at for 4 s – and the bytes written: into the disk cache of the Sigma base and by the whole
/// process. `GRAINEO_MEASURE_OLD=1` reports no current photo (every computed base is stored, as before 0.89).
#[test]
fn measure_write_load_while_paging() {
    let Some(dir) = crate::env::var_os("GRAINEO_MEASURE_PAGING").map(PathBuf::from) else { return };
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some() && std::env::var_os("GRAINEO_CACHE_DIR").is_some());
    let old = crate::env::var_os("GRAINEO_MEASURE_OLD").is_some();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f"))).collect();
    files.sort();
    let views: Vec<PathBuf> = files.iter().cycle().take(50).cloned().collect();
    let (cache0, _) = develop::diskcache::stats();
    let disk0 = library::platform::disk_bytes_written().unwrap_or(0);
    let t = std::time::Instant::now();
    let mode = Settings::default().spp_key_class(false);
    for (i, p) in views.iter().enumerate() {
        if !old {
            develop::diskcache::set_current(p);
        }
        run_inner(Job::Spp { index: i, path: p.clone(), session: None, mode: mode.clone(), cancel: Arc::new(AtomicBool::new(false)), prefetch: false }, &|_| {});
        if let Some(next) = views.get(i + 1).cloned() {
            let mode = mode.clone();
            crate::threads::spawn(crate::threads::Lane::Ahead, move || {
                run_inner(Job::Spp { index: i + 1, path: next, session: None, mode, cancel: Arc::new(AtomicBool::new(false)), prefetch: true }, &|_| {});
            });
        }
        std::thread::sleep(Duration::from_millis(if i % 5 == 4 { 4000 } else { 300 }));
    }
    // let the stores waiting in the background finish
    std::thread::sleep(Duration::from_secs(25));
    let (cache1, n) = develop::diskcache::stats();
    let disk1 = library::platform::disk_bytes_written().unwrap_or(0);
    eprintln!(
        "MEASUREMENT paging {} views ({} files, {}): {:.0} s, disk cache +{:.0} MB ({n} entries), process wrote {:.0} MB",
        views.len(),
        files.len(),
        if old { "store every base" } else { "store looked-at photos" },
        t.elapsed().as_secs_f32(),
        (cache1.saturating_sub(cache0)) as f64 / 1e6,
        (disk1.saturating_sub(disk0)) as f64 / 1e6
    );
}

/// Measurement of the priority inversion (only with `GRAINEO_MEASURE_INVERSION=<Quattro or Classic X3F>`): an offline
/// copy of the photo starts in the background pool; once it is inside the preprocessing it is starved there
/// (`GRAINEO_MEASURE_SPIN` threads, default 16, that only compute with the background priority, on top of what the
/// system runs), and the photo is opened in front. Reports the time to the first image, the time of the offline copy and
/// whether the front took over the shared preprocessing (`pools::takeovers`).
#[test]
fn measure_open_while_offline_copy_starves() {
    let Some(path) = crate::env::var_os("GRAINEO_MEASURE_INVERSION").map(PathBuf::from) else { return };
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let _serial = global_gates_for_tests();
    let spin: usize = crate::env::var("GRAINEO_MEASURE_SPIN").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
    let t0 = std::time::Instant::now();
    let (copy_tx, copy_rx) = channel::<(f32, String)>();
    {
        let p = path.clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let r = crate::large_preview::render_offline(&p, 1024);
            let _ = copy_tx.send((t.elapsed().as_secs_f32(), r.map(|i| format!("{}×{}", i.width(), i.height())).unwrap_or_else(|e| e)));
        });
    }
    // starve the offline copy only once it is inside the preprocessing (unstarved a moment after the start)
    while !develop::quattro::is_preparing(&path) {
        assert!(t0.elapsed() < Duration::from_secs(600), "offline copy did not reach the preprocessing: {}", pools::gates_overview());
        std::thread::sleep(Duration::from_millis(10));
    }
    let stop = Arc::new(AtomicBool::new(false));
    for _ in 0..spin {
        let s = stop.clone();
        std::thread::spawn(move || {
            library::platform::set_priority(library::platform::Priority::Background);
            while !s.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        });
    }
    std::thread::sleep(Duration::from_secs(1));
    let takeovers0 = pools::takeovers();
    let (open_tx, open_rx) = channel::<(f32, bool)>();
    {
        let p = path.clone();
        crate::threads::spawn(crate::threads::Lane::Front, move || {
            let t = std::time::Instant::now();
            let ok = Mutex::new(false);
            run_inner(Job::Open { index: 0, path: p, settings: Settings::default(), generation: 0, current: Arc::new(AtomicUsize::new(0)) }, &|r| {
                *ok.lock().unwrap() = matches!(r, JobResult::Opened { .. });
            });
            let _ = open_tx.send((t.elapsed().as_secs_f32(), ok.into_inner().unwrap()));
        });
    }
    let (open_s, ok) = open_rx.recv_timeout(Duration::from_secs(30 * 60)).expect("open within 30 minutes");
    let took_over = pools::takeovers() - takeovers0;
    eprintln!("inversion: first image after {open_s:.2} s (opened {ok}), takeovers {took_over}, background busy since {:.1} s, spinners {spin}", t0.elapsed().as_secs_f32() - open_s);
    stop.store(true, Ordering::Relaxed);
    let (copy_s, copy) = copy_rx.recv_timeout(Duration::from_secs(60 * 60)).expect("offline copy within an hour");
    eprintln!("inversion: offline copy {copy} in {copy_s:.1} s");
    assert!(ok);
}
