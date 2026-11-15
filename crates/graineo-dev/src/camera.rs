//! `graineo-dev camera [--load] [--auto]`: diagnosis of the camera import with a real camera (ImageCaptureCore, macOS).
//! Lists the cameras, opens the first one, waits for its catalog, counts the files and asks for three of them a
//! thumbnail, the first 64 KB and the partial checksum, each with its time. Only reads; `--load` also downloads one
//! JPEG into `/private/tmp/graineo-kamera-diagnose` (nothing is ever deleted or written on the camera). `--auto` lets
//! the camera fetch all thumbnails itself (comparison). Every step of the camera module goes to stderr.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use library::import_source::{self as isrc, CamState, Source};

pub fn run(args: &[String]) -> Result<()> {
    // set before any other thread runs
    std::env::set_var("GRAINEO_CAMERA_LOG", "1");
    if args.iter().any(|a| a == "--auto") {
        std::env::set_var("GRAINEO_CAMERA_AUTOTHUMBS", "1");
    }
    let download = args.iter().any(|a| a == "--load");
    let Some(browser) = library::camera_mac::Browser::start(Box::new(|| {})) else { bail!("not on the main thread") };
    println!("Searching for cameras (10 s) …");
    let t = Instant::now();
    while browser.rows().is_empty() && t.elapsed() < Duration::from_secs(10) {
        library::camera_mac::pump(0.2);
    }
    let rows = browser.rows();
    if rows.is_empty() {
        bail!("no camera found (PTP/\"PC-Verbindung\", switched on, cable?)");
    }
    for r in &rows {
        println!("Camera: {} · {} · {:?}", r.name, r.id, r.state);
    }
    let id = rows[0].id.clone();
    let src = browser.open(&id);
    println!("Session open requested, waiting for the catalog (180 s at most) …");
    let t = Instant::now();
    let mut last = None;
    while !src.connected() && t.elapsed() < Duration::from_secs(180) {
        library::camera_mac::pump(0.2);
        let st = browser.rows().into_iter().find(|r| r.id == id).map(|r| r.state);
        if st != last {
            println!("  {:6.2} s  {:?}", t.elapsed().as_secs_f64(), st);
            last = st.clone();
        }
        if matches!(st, Some(CamState::Failed(_)) | Some(CamState::Restricted)) {
            bail!("Kamera: {st:?}");
        }
    }
    if !src.connected() {
        bail!("catalog not ready after 180 s");
    }
    println!("Catalog ready after {:.1} s", t.elapsed().as_secs_f64());
    for l in browser.describe(&id) {
        println!("  {l}");
    }
    // the storages the camera offers (memory card, internal memory, info storage …), PTP read only
    if let Some(job) = browser.storages_job(&id) {
        let done = Arc::new(AtomicBool::new(false));
        let d2 = done.clone();
        let h = std::thread::spawn(move || {
            let r = job();
            d2.store(true, Ordering::SeqCst);
            r
        });
        while !done.load(Ordering::SeqCst) {
            library::camera_mac::pump(0.1);
        }
        match h.join() {
            Ok(Ok(list)) => {
                println!("  Storages: {}", list.len());
                for s in list {
                    println!("    {:08x} · kind {} ({}) · \"{}\" · name \"{}\" · {} / {} bytes free", s.id, s.kind, if s.holds_photos() { "for photos" } else { "read only/info" }, s.description, s.label, s.free, s.capacity);
                }
            }
            Ok(Err(e)) => println!("  Storages: ERROR {e}"),
            Err(_) => println!("  Storages: thread crashed"),
        }
    }
    // later announcements (files that come after the catalog), and the app's re-reading of an empty list
    println!("listening for another 20 s …");
    let t2 = Instant::now();
    while t2.elapsed() < Duration::from_secs(20) {
        library::camera_mac::pump(0.2);
        let _ = browser.rows();
    }
    println!("afterwards: {:?}", browser.rows().into_iter().find(|r| r.id == id).map(|r| r.state));
    for l in browser.describe(&id) {
        println!("  {l}");
    }
    // the requests run in a helper thread; the main thread keeps the run loop going (it starts them)
    let done = Arc::new(AtomicBool::new(false));
    let (d2, s2) = (done.clone(), src.clone());
    let worker = std::thread::Builder::new().name("diagnosis".into()).spawn(move || {
        let r = probe(&*s2, download);
        d2.store(true, Ordering::SeqCst);
        r
    })?;
    while !done.load(Ordering::SeqCst) {
        library::camera_mac::pump(0.1);
    }
    let r = worker.join().map_err(|_| anyhow::anyhow!("diagnostic thread crashed"))?;
    drop(browser);
    library::camera_mac::pump(0.5);
    r
}

fn probe(src: &dyn Source, download: bool) -> Result<()> {
    let files = src.list().map_err(anyhow::Error::msg)?;
    let mut ext: BTreeMap<String, usize> = BTreeMap::new();
    let mut folders: BTreeMap<String, usize> = BTreeMap::new();
    for f in &files {
        *ext.entry(f.rel.rsplit('.').next().unwrap_or("").to_uppercase()).or_default() += 1;
        *folders.entry(f.rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()).or_default() += 1;
    }
    println!("{} files in the catalog", files.len());
    println!("  by extension: {ext:?}");
    println!("  by folder: {folders:?}");
    let cands = isrc::list(src).map_err(anyhow::Error::msg)?;
    println!("  as import candidates: {} · photos (RAW+JPEG = one): {}", cands.len(), library::card_import::photos(&cands).len());
    let mut picks: Vec<_> = files.iter().filter(|f| f.rel.to_uppercase().ends_with(".JPG")).take(2).collect();
    picks.extend(files.iter().filter(|f| !f.rel.to_uppercase().ends_with(".JPG")).take(1));
    for f in &picks {
        println!("— {} ({} bytes, {:?})", f.rel, f.size, f.created);
        let t = Instant::now();
        match src.thumbnail(&f.rel, 512) {
            Some(img) => println!("  Thumbnail: {}×{} in {:.2} s", img.width(), img.height(), t.elapsed().as_secs_f64()),
            None => println!("  Thumbnail: NONE after {:.2} s", t.elapsed().as_secs_f64()),
        }
        let t = Instant::now();
        match src.read_range(&f.rel, 0, 65536.min(f.size)) {
            Ok(b) => println!("  first 64 KB: {} bytes in {:.2} s, start {:02x?}", b.len(), t.elapsed().as_secs_f64(), &b[..b.len().min(4)]),
            Err(e) => println!("  first 64 KB: ERROR after {:.2} s: {e}", t.elapsed().as_secs_f64()),
        }
        let t = Instant::now();
        match isrc::partial_of(src, &f.rel, f.size) {
            Ok(s) => println!("  Partial checksum: {s} in {:.2} s", t.elapsed().as_secs_f64()),
            Err(e) => println!("  Partial checksum: ERROR: {e}"),
        }
    }
    if download {
        if let Some(f) = files.iter().filter(|f| f.rel.to_uppercase().ends_with(".JPG")).min_by_key(|f| f.size) {
            let dir = std::path::Path::new("/private/tmp/graineo-kamera-diagnose");
            std::fs::create_dir_all(dir)?;
            let tmp = dir.join(format!("lauf-{}", std::process::id()));
            std::fs::create_dir_all(&tmp)?;
            let c = library::card_import::Candidate { path: isrc::camera_path(&src.id(), &f.rel), size: f.size, format: "JPG".into(), ..Default::default() };
            let t = Instant::now();
            let name = f.rel.rsplit('/').next().unwrap_or("x.jpg").to_string();
            match isrc::fetch_verified(src, &c, &tmp, &name, &mut |_| {}) {
                Ok((p, sum)) => println!("Load + verify: {} in {:.2} s, SHA-256 {sum}", p.display(), t.elapsed().as_secs_f64()),
                Err(e) => println!("Load: ERROR after {:.2} s: {e}", t.elapsed().as_secs_f64()),
            }
        }
    }
    Ok(())
}
