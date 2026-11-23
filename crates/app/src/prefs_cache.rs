//! Settings → Library → "Cache limit": upper limit for the preview cache (setting `previewCacheLimitGB`, default
//! unlimited), measuring in the background, quiet automatic trimming and "Trim now …". The rules (what is trimmed,
//! what never, the guard around the cache folder) live in `library::cache_limit`.
//!
//! Only two ways remove anything: the limit chosen by the user (automatic, in the background, only when
//! `jobs::idle()`, QoS background, one thread) and the button "Jetzt bereinigen …" after a confirmation.
use super::*;
use library::cache_limit as limit;
use library::preview;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

/// Measured sizes and plans (bytes).
#[derive(Clone, Default)]
struct Meas {
    /// previews of photos on this Mac (trimmed)
    previews: u64,
    /// offline copies and previews of photos on other drives or not reachable (never trimmed)
    offline: u64,
    /// developed previews (never trimmed)
    developed: u64,
    /// stored Sigma bases (trimmed)
    bases: u64,
    /// computed merges (HDR; trimmed where the sources are reachable)
    merged: u64,
    /// fingerprints, faces, …
    other: u64,
    /// what the automatic trimming would remove now / what "Trim now" would remove (0 = nothing)
    plan_auto: u64,
    plan_manual: u64,
    /// the target is reachable
    reaches: bool,
}

impl Meas {
    fn total(&self) -> u64 {
        self.previews + self.offline + self.developed + self.bases + self.merged + self.other
    }
}

static MEAS: Mutex<Option<(Meas, Instant)>> = Mutex::new(None);
static MEASURING: AtomicBool = AtomicBool::new(false);
static CONFIRM: AtomicBool = AtomicBool::new(false);
/// Running trim: (by hand, start)
static RUNNING: Mutex<Option<(bool, Instant)>> = Mutex::new(None);
static FREED: AtomicU64 = AtomicU64::new(0);
static PLANNED: AtomicU64 = AtomicU64::new(0);
/// Result of the last run (text for the settings)
static LAST: Mutex<String> = Mutex::new(String::new());
static STARTED: std::sync::Once = std::sync::Once::new();

/// Where the Sigma bases lie. Test runs (`GRAINEO_DATA_DIR`) get `<DATA>/cache/spp` (sysdirs), never the real cache.
fn bases_folder() -> Option<std::path::PathBuf> {
    develop::diskcache::dir()
}

fn roots(cache: &preview::Cache) -> limit::Roots {
    let spp = bases_folder();
    let merged = rawdev::merged::cache_dir();
    let mut v: Vec<&std::path::Path> = vec![cache.folder(), &merged];
    if let Some(s) = &spp {
        v.push(s);
    }
    limit::Roots::new(&v)
}

/// All candidates for trimming (stored bases, previews of photos on this Mac).
fn candidates(cache: &preview::Cache) -> Vec<limit::Item> {
    let mut v = bases_folder().map(|d| limit::scan_bases(&d, "sppc", std::time::SystemTime::now())).unwrap_or_default();
    v.extend(merged_items(std::time::SystemTime::now()));
    v.extend(limit::preview_items(&cache.inventory(), cache.protection_known(), &|k| cache.is_protected(k), &|k, l| cache.path(k, l)));
    v
}

/// Computed merges (HDR) that can be made again from their sources, as bases (oldest first, young ones left out).
fn merged_items(now: std::time::SystemTime) -> Vec<limit::Item> {
    rawdev::merged::trimmable()
        .into_iter()
        .filter(|(_, _, t)| now.duration_since(*t).is_ok_and(|d| d.as_secs() >= limit::KEEP_YOUNG_SECS))
        .map(|(path, bytes, t)| limit::Item { path, bytes, age: t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()), kind: limit::Kind::Base })
        .collect()
}

/// Measure (sizes of the groups, plans). Runs in the calling thread (always a background thread).
fn measure(cache: &preview::Cache) -> Meas {
    let mut m = Meas::default();
    for (k, _, b, _) in cache.inventory() {
        if cache.is_protected(k) {
            m.offline += b;
        } else {
            m.previews += b;
        }
    }
    m.developed = limit::folder_bytes(&preview::developed_folder());
    let base_dir = bases_folder();
    m.bases = base_dir.as_deref().map_or(0, |d| limit::scan_bases(d, "sppc", std::time::UNIX_EPOCH + Duration::from_secs(u32::MAX as u64)).iter().map(|i| i.bytes).sum());
    m.merged = rawdev::merged::cache_bytes();
    let folder = cache.folder();
    m.other = limit::folder_bytes(&folder.join("fingerprints")) + folder.parent().map_or(0, |p| limit::folder_bytes(&p.join("faces")));
    if let Some(l) = limit::limit_bytes() {
        let total = m.total();
        let c = candidates(cache);
        let a = limit::plan(c.clone(), total, l, 100);
        let h = limit::plan(c, total, l, limit::TARGET_PERCENT);
        (m.plan_auto, m.plan_manual, m.reaches) = (a.bytes, h.bytes, h.reaches_target);
    }
    m
}

/// Measured values; starts measuring in the background (at most every 20 s) and returns the last result.
fn meas(ui: &egui::Ui, cache: &Arc<preview::Cache>) -> Option<Meas> {
    let m = MEAS.lock().unwrap().clone();
    let stale = m.as_ref().is_none_or(|m| m.1.elapsed() > Duration::from_secs(20));
    if stale && !MEASURING.swap(true, Ordering::SeqCst) {
        let c = cache.clone();
        std::thread::spawn(move || {
            library::platform::background_priority();
            let r = measure(&c);
            *MEAS.lock().unwrap() = Some((r, Instant::now()));
            MEASURING.store(false, Ordering::SeqCst);
        });
        ui.ctx().request_repaint_after(Duration::from_millis(400));
    }
    m.map(|m| m.0)
}

/// Trim as the limit says; `manual`: after the confirmation (goes down to 90 % even when the limit is not exceeded
/// yet, does not wait for an idle Mac). Does nothing without a limit.
fn run(cache: Arc<preview::Cache>, manual: bool) {
    if RUNNING.lock().unwrap().is_some() {
        return;
    }
    let Some(l) = limit::limit_bytes() else { return };
    *RUNNING.lock().unwrap() = Some((manual, Instant::now()));
    FREED.store(0, Ordering::Relaxed);
    PLANNED.store(0, Ordering::Relaxed);
    std::thread::spawn(move || {
        library::platform::background_priority();
        let m = measure_fresh(&cache);
        let plan = limit::plan(candidates(&cache), m.total(), l, if manual { limit::TARGET_PERCENT } else { 100 });
        PLANNED.store(plan.bytes, Ordering::Relaxed);
        let roots = roots(&cache);
        let remove = |it: &limit::Item| match it.kind {
            limit::Kind::Base => std::fs::remove_file(&it.path).is_ok(),
            limit::Kind::Preview { key, level } => {
                cache.remove(key, level);
                true
            }
        };
        let idle = || manual || crate::jobs::idle();
        let o = limit::execute(&roots, &plan, &remove, &idle, &|b| FREED.store(b, Ordering::Relaxed));
        crate::log(&format!("Preview cache cleaned ({}): {} files, {} bytes, {} refused{}", if manual { "by hand" } else { "automatic" }, o.removed, o.bytes, o.refused, if o.stopped { ", interrupted" } else { "" }));
        if manual {
            *LAST.lock().unwrap() = crate::trf!("Zuletzt entfernt: {a} ({n} Dateien)", a = gb(o.bytes), n = thousands(o.removed as i64));
        }
        *MEAS.lock().unwrap() = None;
        *RUNNING.lock().unwrap() = None;
    });
}

fn measure_fresh(cache: &preview::Cache) -> Meas {
    // the sizes without the plan (the plan is made by the caller with the same data)
    let mut m = measure(cache);
    (m.plan_auto, m.plan_manual) = (0, 0);
    m
}

/// Called once by the library view: the automatic trimming (first check 90 s after the start, then every 10 min).
/// Test runs: `GRAINEO_CACHE_LIMIT_SECS=<s>` sets the first delay and the interval.
pub(super) fn start(cache: Arc<preview::Cache>) {
    STARTED.call_once(|| {
        let secs = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_CACHE_LIMIT_SECS").ok()).and_then(|s| s.parse::<u64>().ok());
        std::thread::spawn(move || {
            library::platform::background_priority();
            // once per start: states of merged photos (HDR) that no recipe needs any more
            std::thread::sleep(Duration::from_secs(secs.unwrap_or(90).min(30)));
            let freed = rawdev::merged::sweep();
            if freed > 0 {
                crate::log(&format!("merge cache: {freed} bytes of unused states removed"));
                *MEAS.lock().unwrap() = None;
            }
            std::thread::sleep(Duration::from_secs(secs.unwrap_or(90).saturating_sub(30)));
            loop {
                if limit::limit_bytes().is_some() && crate::jobs::idle() {
                    run(cache.clone(), false);
                    while RUNNING.lock().unwrap().is_some() {
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
                std::thread::sleep(Duration::from_secs(secs.unwrap_or(600)));
            }
        });
    });
}

/// Entry for the activity display in the title bar while trimming runs.
pub(super) fn task() -> Option<crate::activity::Task> {
    use crate::activity::{Rank, Task};
    let (manual, _) = (*RUNNING.lock().unwrap())?;
    let (done, plan) = (FREED.load(Ordering::Relaxed), PLANNED.load(Ordering::Relaxed));
    let text = if plan > 0 { crate::trf!("Vorschau-Cache bereinigen: {a} von {b}", a = gb(done), b = gb(plan)) } else { crate::tr("Vorschau-Cache bereinigen …").to_string() };
    Some(Task::new(if manual { Rank::Started } else { Rank::Settings }, text))
}

/// The group in the settings (below "Previews").
pub(super) fn group_ui(ui: &mut egui::Ui) {
    let Some(cache) = PREVIEW_CACHE.lock().unwrap().clone() else { return };
    let chosen = limit::limit_bytes().map_or(0, |b| b / preview::GB);
    let m = meas(ui, &cache);
    let running = *RUNNING.lock().unwrap();
    group(ui, crate::tr("Obergrenze des Caches"), |ui| {
        row(ui, crate::tr("Obergrenze"), None, |ui| {
            crate::help::info(ui, "prefs-cachegrenze", "Obergrenze des Caches", &crate::help::CACHE_LIMIT);
            let mut g = chosen as u32;
            let opts: Vec<(u32, &str)> = vec![(0, crate::tr("Unbegrenzt")), (10, "10 GB"), (20, "20 GB"), (50, "50 GB"), (100, "100 GB")];
            if lr::segmented_pad(ui, &mut g, &opts, 6.0) {
                limit::set_limit_gb(g as u64);
                CONFIRM.store(false, Ordering::SeqCst);
                *MEAS.lock().unwrap() = None;
            }
        });
        separator(ui);
        row(ui, crate::tr("Belegt"), None, |ui| match &m {
            Some(m) => {
                let over = limit::limit_bytes().is_some_and(|l| m.total() > l);
                value(ui, &gb(m.total()), if over { Signal::Yellow } else { Signal::Green });
            }
            None => determining(ui),
        });
        if let Some(m) = &m {
            for (name, bytes, trimmed) in [
                (crate::tr("Vorschauen von Fotos auf diesem Mac"), m.previews, true),
                (crate::tr("Foveon-Zwischenspeicher"), m.bases, true),
                (crate::tr("HDR-Zwischenspeicher"), m.merged, true),
                (crate::tr("Offline-Kopien, andere Laufwerke"), m.offline, false),
                (crate::tr("Entwickelte Vorschauen"), m.developed, false),
                (crate::tr("Übriges (Fingerabdrücke, Gesichter)"), m.other, false),
            ] {
                separator(ui);
                let merged = name == crate::tr("HDR-Zwischenspeicher");
                row(ui, name, None, |ui| {
                    // the computed merges can be emptied by hand: they are made again from the sources when a photo opens
                    if merged {
                        let r = ui.add_enabled(bytes > 0 && !rawdev::merged::other_library(), egui::Button::new(crate::tr("Leeren")));
                        if r.clicked() {
                            std::thread::spawn(|| {
                                let freed = rawdev::merged::clear_cache();
                                crate::log(&format!("merge cache emptied by hand: {freed} bytes"));
                                *MEAS.lock().unwrap() = None;
                            });
                        }
                        lr::hover_tip(ui, &r, crate::tr("HDR-Fotos bleiben; ihr Bild wird beim Öffnen aus den Quellen neu berechnet"));
                    }
                    value(ui, &gb(bytes), if trimmed { Signal::Gray } else { Signal::Green })
                });
            }
        }
        separator(ui);
        let have_limit = chosen > 0;
        if CONFIRM.load(Ordering::SeqCst) && have_limit {
            let plan = m.as_ref().map_or(0, |m| m.plan_manual);
            row(ui, crate::tr("Vorschau-Cache bereinigen"), None, |ui| {
                if ui.add_enabled(plan > 0 && running.is_none(), egui::Button::new(crate::tr("Bereinigen"))).clicked() {
                    CONFIRM.store(false, Ordering::SeqCst);
                    run(cache.clone(), true);
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    CONFIRM.store(false, Ordering::SeqCst);
                }
            });
            let text = if plan == 0 {
                crate::tr("Es gibt nichts zu entfernen.").to_string()
            } else {
                crate::trf!("Es werden etwa {a} entfernt: die ältesten Foveon- und HDR-Zwischenspeicher und die am längsten nicht angesehenen Vorschauen von Fotos auf diesem Mac. Sie werden bei Bedarf neu erzeugt; das kostet beim Blättern Rechenzeit, besonders bei Sigma Quattro. Originale, Bearbeitungen, Offline-Kopien und entwickelte Vorschauen bleiben.", a = gb(plan))
            };
            dialog_hint_small(ui, &text);
            if plan > 0 && m.as_ref().is_some_and(|m| !m.reaches) {
                hint_yellow(ui, &crate::tr("Die Obergrenze wird dadurch nicht erreicht: der Rest ist geschützt oder erst kürzlich benutzt."));
            }
        } else {
            row(ui, crate::tr("Vorschau-Cache bereinigen"), None, |ui| {
                let can = have_limit && running.is_none() && m.as_ref().is_some_and(|m| m.plan_manual > 0);
                let r = ui.add_enabled(can, egui::Button::new(crate::tr("Jetzt bereinigen …")));
                if r.clicked() {
                    CONFIRM.store(true, Ordering::SeqCst);
                }
                if !have_limit {
                    lr::hover_tip(ui, &r, crate::tr("Zuerst eine Obergrenze wählen"));
                }
                if let Some((_, since)) = running {
                    ui.ctx().request_repaint_after(Duration::from_millis(500));
                    let _ = since;
                    value(ui, &crate::trf!("{a} von {b}", a = gb(FREED.load(Ordering::Relaxed)), b = gb(PLANNED.load(Ordering::Relaxed))), Signal::Yellow);
                }
            });
            let last = LAST.lock().unwrap().clone();
            if running.is_none() && !last.is_empty() {
                dialog_hint_small(ui, &last);
            }
            if have_limit && running.is_none() {
                if let Some(m) = m.as_ref().filter(|m| m.plan_auto > 0) {
                    dialog_hint_small(ui, &crate::trf!("Über der Obergrenze: etwa {a} werden im Hintergrund entfernt, sobald der Mac nichts anderes tut.", a = gb(m.plan_auto)));
                    if !m.reaches {
                        hint_yellow(ui, &crate::tr("Die Obergrenze wird dadurch nicht erreicht: der Rest ist geschützt oder erst kürzlich benutzt."));
                    }
                }
            }
        }
    });
}
