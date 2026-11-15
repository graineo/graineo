//! Load test of the multi-selection edit (several thousand selected photos): the UI must stay responsive while a
//! change on the active photo goes onto all selected photos.
//!
//! Not part of the normal run (`#[ignore]`): it needs a folder of photos, e.g. 3 000 APFS clones of a Merrill X3F
//! (`cp -c`), in `GRAINEO_MULTI_DIR`, and an own `GRAINEO_DATA_DIR`. Run:
//! `GRAINEO_DATA_DIR=… GRAINEO_NO_XMP=1 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_NO_PHOTOKIT=1 GRAINEO_MULTI_DIR=…
//!  cargo test --release -p graineo multi_edit_load -- --ignored --nocapture`
//! Prints the measurements (`multi-load: …`): longest frame, frames over 100 ms, time until everything is saved,
//! number of library transactions, previews rendered, peak memory.

use crate::*;
use eframe::App as _;
use std::time::{Duration, Instant};

fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    out.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
}

struct Meter {
    ctx: egui::Context,
    time: f64,
    frames: Vec<f64>,
    peak_mb: f64,
    n: usize,
}

impl Meter {
    /// One frame; `events` as the window delivers them. Returns the time the UI thread needed (ms).
    fn frame(&mut self, a: &mut App, events: Vec<egui::Event>) -> f64 {
        self.time += 0.03;
        let mut input = egui::RawInput { time: Some(self.time), events, focused: true, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        let t = Instant::now();
        a.raw_input_hook(&self.ctx, &mut input);
        let _ = crate::ui_test::run(&self.ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        let t_ui = t.elapsed().as_secs_f64() * 1000.0;
        a.handle_results(&self.ctx);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > 100.0 {
            eprintln!("multi-load: slow frame {} : {ms:.0} ms (frame {t_ui:.0} ms, results {:.0} ms), applying {:?}, previews waiting {}", self.n, ms - t_ui, a.multi.progress(), a.thumbnails_waiting());
        }
        self.frames.push(ms);
        self.n += 1;
        if self.n % 30 == 0 {
            self.peak_mb = self.peak_mb.max(rss_mb());
        }
        std::thread::sleep(Duration::from_millis(5));
        ms
    }

    fn press(&mut self, a: &mut App, down: bool) {
        let p = egui::Pos2::new(5.0, 990.0);
        self.frame(a, vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: down, modifiers: Default::default() }]);
    }

    fn report(&mut self, name: &str, since: Instant, from: usize, a: &App) {
        let f = &self.frames[from..];
        let max = f.iter().cloned().fold(0.0, f64::max);
        let over = f.iter().filter(|&&m| m > 100.0).count();
        let sum: f64 = f.iter().sum();
        eprintln!(
            "multi-load: {name}: {:.1} s until saved, frames {} (max {max:.0} ms, {over} over 100 ms, UI busy {:.1} s), transactions {}, previews rendered {}, previews waiting {}, peak memory {:.0} MB",
            since.elapsed().as_secs_f64(),
            f.len(),
            sum / 1000.0,
            library::store::commits(),
            crate::jobs::THUMBS_RENDERED.load(std::sync::atomic::Ordering::Relaxed),
            a.thumbnails_waiting(),
            self.peak_mb.max(rss_mb())
        );
    }
}

/// Everything handed over is saved (library writer, ordered writer, multi-edit job).
fn saved(a: &App) -> bool {
    !library::store::saving() && !crate::library_turn::writing() && !a.multi_busy()
}

#[test]
#[ignore]
fn multi_edit_load() {
    let Some(dir) = crate::env::var_os("GRAINEO_MULTI_DIR").map(std::path::PathBuf::from) else { return };
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let _app_lock = crate::ui_test::app_lock();
    let count = std::fs::read_dir(&dir).unwrap().count();
    {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &dir, &|_| {}).unwrap();
    }
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut a = App::new(&cc);
    let mut m = Meter { ctx, time: 10.0, frames: Vec::new(), peak_mb: rss_mb(), n: 0 };
    a.open_folder_inner(dir.clone());
    let deadline = Instant::now() + crate::ui_test::WAIT;
    while a.entries.len() < count.min(3000) {
        m.frame(&mut a, vec![]);
        assert!(Instant::now() < deadline, "library: {} of {count} photos", a.entries.len());
    }
    assert!(a.in_grid());
    let first = a.entries.iter().find(|e| e.path.extension().is_some_and(|x| x == "X3F")).unwrap().image;
    a.lib.set_active(first);
    a.lib.select_all();
    a.choose_tool(crate::workspace::ToolTab::Edit);
    let deadline = Instant::now() + crate::ui_test::WAIT;
    while a.current.as_ref().is_none_or(|c| c.texture.is_none()) {
        m.frame(&mut a, vec![]);
        assert!(Instant::now() < deadline, "active photo is not loaded");
    }
    // let the start settle (grid previews of the first screen)
    for _ in 0..60 {
        m.frame(&mut a, vec![]);
    }
    a.lib.select_all();
    let cur = a.current.as_ref().unwrap().index;
    let active = a.entries[cur].image;
    // the list may be rebuilt meanwhile (other order): the active photo by its ID
    let at = |a: &App| a.entries.iter().position(|e| e.image == active).expect("active photo in the list");
    let selected = a.lib.selection().len();
    eprintln!("multi-load: {} photos in the library, {selected} selected, active {}", a.entries.len(), a.entries[cur].name);
    library::store::reset_commits();
    crate::jobs::THUMBS_RENDERED.store(0, std::sync::atomic::Ordering::Relaxed);
    m.peak_mb = rss_mb();
    let limit = Duration::from_secs(crate::env::var("GRAINEO_MULTI_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(300));

    let images: Vec<(i64, std::path::PathBuf)> = a.entries.iter().enumerate().filter(|(i, e)| *i != cur && e.copy.is_none() && e.path.extension().is_some_and(|x| x == "X3F")).map(|(_, e)| (e.image, e.path.clone())).collect();
    // the settings in effect (own ones from the library, otherwise the initial values from the camera file)
    let load = |p: &std::path::Path| Some(crate::sidecar::load_entry(p, None).0);
    let start: Vec<Option<develop::Settings>> = images.iter().map(|(_, p)| load(p)).collect();

    // 1. adjust on the active photo: colour mode switched, sliders dragged quickly one after another (exposure,
    //    white balance tint, exposure). Nothing happens to the other photos.
    let from = m.frames.len();
    let t = Instant::now();
    let cur = at(&a);
    a.entries[cur].settings.monochrome = !a.entries[cur].settings.monochrome;
    let mono = a.entries[cur].settings.monochrome;
    m.frame(&mut a, vec![]);
    for round in 0..3 {
        m.press(&mut a, true);
        for _ in 0..10 {
            let cur = at(&a);
            if round == 1 {
                a.entries[cur].settings.wb_tint += 1.0;
            } else {
                a.entries[cur].settings.exposure += 0.05;
            }
            m.frame(&mut a, vec![]);
        }
        m.press(&mut a, false);
        m.frame(&mut a, vec![]);
    }
    for _ in 0..30 {
        m.frame(&mut a, vec![]);
    }
    m.report("adjusting the active photo (3 000 selected)", t, from, &a);
    let touched = (0..images.len()).filter(|&k| load(&images[k].1) != start[k]).count();
    assert_eq!(touched, 0, "{touched} other photos changed before \"Apply\"");
    let list = a.multi_change().map(|(_, b, n)| crate::multi_edit::describe(&b, &n)).unwrap_or_default();
    eprintln!("multi-load: not applied yet: {}", list.join(", "));
    eprintln!(
        "multi-load: base {:?}, current {:?}, active entry now mono {} ev {:.2}",
        a.multi_base.as_ref().map(|(i, b)| (*i, b.monochrome, b.exposure)),
        a.current.as_ref().map(|c| c.index),
        a.entries[at(&a)].settings.monochrome,
        a.entries[at(&a)].settings.exposure
    );
    assert!(!list.is_empty());

    // 2. "Apply to n photos" (relative): in the background, block by block
    let from = m.frames.len();
    let t = Instant::now();
    let mut until_back = None;
    a.multi_apply_now();
    let ms = m.frame(&mut a, vec![]);
    if ms < 100.0 {
        until_back = Some(t.elapsed());
    }
    let mut seen_progress = 0;
    while !saved(&a) && t.elapsed() < limit {
        m.frame(&mut a, vec![]);
        if a.multi.progress().is_some() {
            seen_progress += 1;
        }
    }
    m.report("apply (relative)", t, from, &a);
    eprintln!("multi-load: UI usable again after {:?}, frames with progress shown {seen_progress}", until_back);
    let s_of = |p: &std::path::Path| load(p).unwrap_or_default();
    let wrong = images.iter().filter(|(_, p)| s_of(p).monochrome != mono).count();
    assert_eq!(wrong, 0, "{wrong} of {} photos do not have the new colour mode", images.len());
    let exp0 = |k: usize| start[k].as_ref().map_or(0.0, |s| s.exposure);
    let wrong = (0..images.len()).filter(|&k| (s_of(&images[k].1).exposure - (exp0(k) + 1.0)).abs() > 1e-6).count();
    assert_eq!(wrong, 0, "{wrong} photos do not have exposure +1.0");

    // 3. ⌘Z: one step takes it back on every photo
    let from = m.frames.len();
    let t = Instant::now();
    assert!(a.undo_turn(), "⌘Z: no command for several photos");
    m.frame(&mut a, vec![]);
    while !saved(&a) && t.elapsed() < limit {
        m.frame(&mut a, vec![]);
    }
    m.report("undo (one step)", t, from, &a);
    let wrong = (0..images.len()).filter(|&k| {
        let s = s_of(&images[k].1);
        let s0 = start[k].clone().unwrap_or_default();
        s.monochrome != s0.monochrome || (s.exposure - s0.exposure).abs() > 1e-6
    }).count();
    assert_eq!(wrong, 0, "{wrong} photos not back after ⌘Z");

    // 4. apply again and cancel after the first blocks: whole blocks only, undoable
    let cur = at(&a);
    a.multi_base = Some((active, develop::Settings { exposure: a.entries[cur].settings.exposure - 0.5, ..a.entries[cur].settings.clone() }));
    a.multi_apply_now();
    while a.multi.progress().is_none_or(|(d, _)| d < 500) {
        m.frame(&mut a, vec![]);
    }
    a.multi_cancel();
    let t = Instant::now();
    while !saved(&a) && t.elapsed() < limit {
        m.frame(&mut a, vec![]);
    }
    let changed = (0..images.len()).filter(|&k| (s_of(&images[k].1).exposure - (exp0(k) + 0.5)).abs() < 1e-6).count();
    eprintln!("multi-load: cancelled: {changed} photos changed ({} blocks of {})", changed / crate::multi_apply::BLOCK, crate::multi_apply::BLOCK);
    // whole blocks of 250 targets (the few RAF/JPEG among them are not counted here)
    let block = crate::multi_apply::BLOCK;
    assert!(changed > 0 && changed < images.len() && changed.div_ceil(block) * block - changed <= 4, "cancel: {changed} photos changed");
    assert!(a.undo_turn());
    while !saved(&a) {
        m.frame(&mut a, vec![]);
    }

    // 5. previews: they come in the background for a while (nothing over 100 ms meanwhile)
    let from = m.frames.len();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(20) {
        m.frame(&mut a, vec![]);
    }
    m.report("previews in the background (20 s)", t, from, &a);
}
