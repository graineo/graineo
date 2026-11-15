//! Measurement for the computation planning (gates P0 and P2): how long a visible film preview (2400 px) takes while
//! background film renders at full resolution run on the same machine – the background holds the film lock for a whole
//! image, the visible preview waits behind it. Prints isolated and contended times (p50/p95/max) and when the
//! background finished. Only with `GRAINEO_MEASURE_FILM_GATE=<X3F file>` (a copy, only read); skipped otherwise.
//! `GRAINEO_MEASURE_FILM_GATE_BG=<n>` background threads (default 2), each renders `GRAINEO_MEASURE_FILM_GATE_ROUNDS`
//! (default 2) whole images; `GRAINEO_MEASURE_FILM_GATE_OLD=1` leaves the background at `Visible` (before the gates).

use super::*;
use std::time::{Duration, Instant};

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn film_settings() -> Settings {
    let mut s = Settings::default();
    s.film.active = true;
    s.film.stock = "kodak_trix_400".into();
    s
}

#[test]
fn visible_film_preview_while_background_film_renders() {
    let Some(path) = crate::env::var_os("GRAINEO_MEASURE_FILM_GATE").map(PathBuf::from) else { return };
    let bg_threads: usize = crate::env::var("GRAINEO_MEASURE_FILM_GATE_BG").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    let rounds: usize = crate::env::var("GRAINEO_MEASURE_FILM_GATE_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    let film = film_settings();
    // visible: the preview of the loaded photo (own pipeline, no Sigma base needed)
    let loaded = load(&path, &Settings::default(), || true).expect("open").expect("loaded");
    let (previews, cal) = (loaded.previews.clone(), loaded.cal.clone());
    let visible = move || {
        let t = Instant::now();
        let _ = previews.developed.render(&cal, &film);
        t.elapsed().as_secs_f64() * 1e3
    };
    let _ = visible(); // warm up (film pipeline, GPU)
    let mut isolated: Vec<f64> = (0..5).map(|_| visible()).collect();
    eprintln!("measure film gate: isolated visible film preview p50 {:.0} ms, max {:.0} ms", pct(&mut isolated, 0.5), pct(&mut isolated, 1.0));

    // background: whole images with film, one session per thread (each needs &mut)
    let film_bg = film_settings();
    let start = Instant::now();
    let bg_done = Arc::new(Mutex::new(Vec::<f64>::new()));
    let mut handles = Vec::new();
    for n in 0..bg_threads {
        let (p, s, done) = (path.clone(), film_bg.clone(), bg_done.clone());
        handles.push(
            std::thread::Builder::new()
                .name(format!("hintergrund-film-{n}"))
                .spawn(move || {
                    background_intent();
                    let mut session = Session::open(&p).expect("session");
                    let _ = session.render_full(&Settings::default()); // base, disk cache
                    for _ in 0..rounds {
                        let t = Instant::now();
                        let _ = session.render_full(&s);
                        done.lock().unwrap().push(t.elapsed().as_secs_f64() * 1e3);
                    }
                })
                .unwrap(),
        );
    }
    // give the background time to open its sessions, then the visible previews 100 ms apart
    std::thread::sleep(Duration::from_secs(3));
    let visible_thread = std::thread::Builder::new()
        .name("job-vorne-messung".into())
        .spawn(move || {
            let mut contended = Vec::new();
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(40) {
                contended.push(visible());
                std::thread::sleep(Duration::from_millis(100));
            }
            contended
        })
        .unwrap();
    let mut contended = visible_thread.join().unwrap();
    for h in handles {
        h.join().unwrap();
    }
    let bg = bg_done.lock().unwrap().clone();
    let mut bg_sorted = bg.clone();
    eprintln!(
        "measure film gate: contended visible film preview n={} p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms; background whole images n={} p50 {:.0} ms, max {:.0} ms, all done after {:.1} s",
        contended.len(),
        pct(&mut contended, 0.5),
        pct(&mut contended, 0.95),
        pct(&mut contended, 1.0),
        bg.len(),
        pct(&mut bg_sorted, 0.5),
        pct(&mut bg_sorted, 1.0),
        start.elapsed().as_secs_f64()
    );
}

/// Marks the measurement's background threads as background at the gates; with `GRAINEO_MEASURE_FILM_GATE_OLD=1`
/// they stay `Visible` like every thread before the gates (A/B with the same binary).
fn background_intent() {
    if crate::env::var_os("GRAINEO_MEASURE_FILM_GATE_OLD").is_none() {
        pools::set_intent(pools::Intent::Idle);
    }
}

/// Measurement for gate P5: a full-resolution crop of a Sigma photo while its whole image (with film) is being
/// rendered – before the shared base the crop waited for the whole image under the session lock. Prints the crop
/// time alone and during the whole image. Only with `GRAINEO_MEASURE_REGION_DURING_FULL=<X3F file>`.
#[test]
fn region_during_full_sigma() {
    let Some(path) = crate::env::var_os("GRAINEO_MEASURE_REGION_DURING_FULL").map(PathBuf::from) else { return };
    let film = film_settings();
    let session = Arc::new(Mutex::new(Session::open(&path).unwrap()));
    session.lock().unwrap().ensure_spp(&film).unwrap();
    let current = Arc::new(AtomicUsize::new(0));
    let region_job = || Job::RenderRegion { index: 0, session: Some(session.clone()), foreign: None, settings: film.clone(), region: [1200, 800, 900, 600], factor: 1, generation: 0, current: current.clone(), edit: EditStamp::none() };
    let time = |job: Job| {
        let t = Instant::now();
        let ok = Mutex::new(false);
        run_inner(job, &|r| {
            if matches!(r, JobResult::RegionRendered { .. } | JobResult::FullRendered { .. }) {
                *ok.lock().unwrap() = true;
            }
        });
        assert!(ok.into_inner().unwrap(), "job failed");
        t.elapsed().as_secs_f64() * 1e3
    };
    let _ = time(region_job());
    let mut alone: Vec<f64> = (0..3).map(|_| time(region_job())).collect();
    // whole image with film on another thread; the crop starts 150 ms later (the whole image is then in its film step)
    let mut during = Vec::new();
    for _ in 0..3 {
        let full = Job::RenderFull { index: 0, session: Some(session.clone()), foreign: None, settings: film.clone(), generation: 0, current: current.clone(), edit: EditStamp::none() };
        let full_thread = std::thread::spawn(move || {
            let t = Instant::now();
            run_inner(full, &|_| {});
            t.elapsed().as_secs_f64() * 1e3
        });
        std::thread::sleep(Duration::from_millis(150));
        during.push(time(region_job()));
        let full_ms = full_thread.join().unwrap();
        eprintln!("measure region during full: whole image {full_ms:.0} ms");
    }
    eprintln!(
        "measure region during full: crop alone p50 {:.0} ms (n={}), crop during whole image p50 {:.0} ms, max {:.0} ms",
        pct(&mut alone, 0.5),
        alone.len(),
        pct(&mut during, 0.5),
        pct(&mut during, 1.0)
    );
}
