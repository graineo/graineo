//! Apple Photos in the library: setting (`applePhotos`), sync in the background at startup and
//! whenever the library database changes (iCloud brings changes from the iPhone there), and the
//! original names for display. The Photos library is only read (`library::apple_photos`).

use crate::menu::i18n::DeComma as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

/// Is the Photos library included?
pub fn an() -> bool {
    library::settings::value("applePhotos").as_bool().unwrap_or(false)
}

pub fn set(an: bool) {
    library::settings::set("applePhotos", serde_json::json!(an));
    if an {
        CHECK.lock().unwrap().state_key = None; // sync right away
    }
}

/// The Photos library (default location).
pub fn media_library() -> Option<PathBuf> {
    library::apple_photos::standard()
}

static RUNNING: AtomicBool = AtomicBool::new(false);
/// A sync has finished: re-read the library.
static RELOAD: AtomicBool = AtomicBool::new(false);

struct Check {
    last: Option<Instant>,
    /// Modification time of the database at the last sync
    state_key: Option<SystemTime>,
    message: String,
}

static CHECK: Mutex<Check> = Mutex::new(Check { last: None, state_key: None, message: String::new() });

/// Last message (for the settings).
pub fn message() -> String {
    CHECK.lock().unwrap().message.clone()
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// `true` once after a sync (then re-read the library).
pub fn reload_needed() -> bool {
    RELOAD.swap(false, Ordering::Relaxed)
}

/// Preview keys of photos cropped or rotated in Apple Photos whose previews may still show the original (once after
/// the first sync with this version): the library removes them from its cache, they are made again when shown.
static OUTDATED_PREVIEWS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

pub fn take_outdated_previews() -> Vec<u64> {
    std::mem::take(&mut *OUTDATED_PREVIEWS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Most recent change of the Photos library database (with WAL).
fn db_version(m: &std::path::Path) -> Option<SystemTime> {
    let d = m.join("database");
    ["Photos.sqlite", "Photos.sqlite-wal"].iter().filter_map(|n| std::fs::metadata(d.join(n)).ok()?.modified().ok()).max()
}

/// Changes in the Photos library were noticed, but the sync has not started yet (the database was changed a moment ago:
/// Apple Photos is probably still writing, so the sync waits until it has been quiet for a few seconds).
static PENDING: AtomicBool = AtomicBool::new(false);

/// Quiet time of the Photos database before a sync reads it (seconds).
const QUIET_SECS: u64 = 8;

/// Text for the status line in the title bar: a sync is running, or changes were noticed and it follows. Nothing otherwise.
pub fn status() -> Option<String> {
    if RUNNING.load(Ordering::Relaxed) {
        Some(crate::tr("Apple Fotos wird abgeglichen …").to_string())
    } else if PENDING.load(Ordering::Relaxed) {
        Some(crate::tr("Apple Fotos: Änderungen erkannt – Abgleich folgt").to_string())
    } else {
        None
    }
}

/// How long ago the Photos database was changed (the newest of database and WAL).
fn quiet_for(state_key: Option<SystemTime>) -> u64 {
    state_key.and_then(|t| t.elapsed().ok()).map_or(u64::MAX, |d| d.as_secs())
}

/// Call every frame: check at most every 20 s (every 4 s while a change is waiting) whether the Photos library has
/// changed, and then sync.
pub fn update(ctx: &eframe::egui::Context) {
    // Test hook: GRAINEO_APPLE_SYNC=1 syncs even with folder sync disabled
    let forced = crate::env::var_os("GRAINEO_APPLE_SYNC").is_some();
    if (crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_some() && !forced) || RUNNING.load(Ordering::Relaxed) {
        return;
    }
    {
        let mut p = CHECK.lock().unwrap();
        let interval = if PENDING.load(Ordering::Relaxed) { 4 } else { 20 };
        if p.last.is_some_and(|t| t.elapsed().as_secs() < interval) {
            return;
        }
        p.last = Some(Instant::now());
    }
    if !an() && !forced {
        return;
    }
    let Some(m) = media_library() else { return };
    let state_key = db_version(&m);
    if state_key.is_some() && CHECK.lock().unwrap().state_key == state_key {
        PENDING.store(false, Ordering::Relaxed);
        return;
    }
    // changed a moment ago: Apple Photos is still writing (iCloud brings changes in bursts) – wait until it is quiet
    if quiet_for(state_key) < QUIET_SECS {
        if !PENDING.swap(true, Ordering::Relaxed) {
            ctx.request_repaint();
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(4));
        return;
    }
    sync_now(ctx, m, state_key);
}

/// Sync immediately (button in the settings).
pub fn kick(ctx: &eframe::egui::Context) {
    if let Some(m) = media_library() {
        let state_key = db_version(&m);
        sync_now(ctx, m, state_key);
    }
}

/// A sync held back because it would remove many photos at once: (would be removed, known, Photos library).
static HELD: Mutex<Option<(usize, usize, PathBuf)>> = Mutex::new(None);

/// Every frame (library view): a held-back sync asks the user whether to carry it out.
pub fn held_dialog(ctx: &eframe::egui::Context) {
    let Some((gone, known, m)) = HELD.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let answer = crate::dialog::show(
        ctx,
        "apple-fotos-zurueckgehalten",
        crate::tr("Apple Fotos: viele Fotos verschwunden"),
        420.0,
        |ui| {
            crate::dialog::text(ui, &crate::trf!("{n} von {k} Fotos sind auf einmal aus Apple Fotos verschwunden (auch nicht unter „Zuletzt gelöscht“). Das kann eine Mediathek sein, die Apple Fotos gerade repariert oder aus iCloud neu aufbaut.", n = gone, k = known));
            crate::dialog::hint(ui, crate::tr("Der Abgleich wurde angehalten, in graineo ist nichts entfernt. Beim Abgleichen werden diese Fotos samt Sternen, Stichwörtern und Bearbeitung aus der Bibliothek entfernt."));
        },
        crate::dialog::Buttons::destructive(crate::tr("Trotzdem abgleichen"), true),
    );
    match answer {
        crate::dialog::Answer::Primary => {
            *HELD.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let state_key = db_version(&m);
            sync_with(ctx, m, state_key, true);
        }
        crate::dialog::Answer::Cancel => *HELD.lock().unwrap_or_else(|e| e.into_inner()) = None,
        crate::dialog::Answer::Open => {}
    }
}

fn sync_now(ctx: &eframe::egui::Context, m: PathBuf, state_key: Option<SystemTime>) {
    sync_with(ctx, m, state_key, false)
}

fn sync_with(ctx: &eframe::egui::Context, m: PathBuf, state_key: Option<SystemTime>, force: bool) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    PENDING.store(false, Ordering::Relaxed);
    CHECK.lock().unwrap().message = crate::tr("Apple Fotos wird abgeglichen …").to_string();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // the marker goes back also if the sync panics (otherwise no further sync until the app restarts)
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                RUNNING.store(false, Ordering::SeqCst);
            }
        }
        let _done = Done;
        let t = Instant::now();
        let db = library::db::Db::standard().map_err(|e| e.to_string());
        let r = db.as_ref().map_err(|e| e.clone()).and_then(|db| library::apple_photos::reconcile_with(db, &m, force));
        if let (Ok(db), Ok(b)) = (&db, &r) {
            if b.held_back.is_none() {
                let keys = library::apple_photos::outdated_preview_keys(db);
                if !keys.is_empty() {
                    crate::log(&format!("apple-photos: {} previews of photos cropped in Apple Photos renewed", keys.len()));
                    OUTDATED_PREVIEWS.lock().unwrap_or_else(|e| e.into_inner()).extend(keys);
                    RELOAD.store(true, Ordering::Relaxed);
                }
            }
        }
        // pull in named faces (people) from Apple Photos
        let g = match (&db, &r) {
            (Ok(db), Ok(b)) if b.held_back.is_none() => Some(library::apple_photos::match_faces(db, &m)),
            _ => None,
        };
        let mut text = match &r {
            Ok(b) if b.held_back.is_some() => {
                let (gone, known) = b.held_back.unwrap_or_default();
                *HELD.lock().unwrap_or_else(|e| e.into_inner()) = Some((gone, known, m.clone()));
                crate::trf!("Apple Fotos: Abgleich angehalten – {n} von {k} Fotos wären entfernt worden", n = gone, k = known)
            }
            Ok(b) => crate::trf!("{a} Fotos · {n} neu · {e} entfernt · {p} gelöscht oder ausgeblendet · {l} Sammlungen ({s} s)", a = b.total, n = b.new, e = b.removed, p = b.parked, l = b.albums, s = format!("{:.1}", t.elapsed().as_secs_f32()).de_comma()),
            Err(e) => crate::trf!("Apple Fotos: {e}", e = e),
        };
        match &g {
            Some(Ok(g)) => {
                crate::log(&format!("apple-photos: faces {g:?}"));
                text.push_str(&crate::trf!(" · {a} Gesichter von {p} Personen", a = g.total, p = g.people));
            }
            Some(Err(e)) => crate::log(&format!("apple-photos: faces: {e}")),
            None => {}
        }
        // Apple's unnamed groups: clear ones are assigned to the matching person, unclear ones become suggestions
        // (only graineo's library changes; Apple Photos is only read)
        let groups = match (&db, &g) {
            (Ok(db), Some(Ok(_))) => {
                // nobody waits for it: the rest of this thread runs on the efficiency cores
                library::platform::background_priority();
                let r = library::apple_groups::read_groups(&m).and_then(|groups| library::apple_groups::run(db, &groups, &library::apple_groups::Rules::default()));
                match &r {
                    Ok(r) => crate::log(&format!("apple-photos: groups {r:?}")),
                    Err(e) => crate::log(&format!("apple-photos: groups: {e}")),
                }
                r.ok()
            }
            _ => None,
        };
        if let Some(r) = groups.as_ref().filter(|r| r.assigned_faces > 0) {
            text.push_str(&crate::trf!(" · {a} Gesichter aus Apple-Gruppen zugeordnet", a = r.assigned_faces));
        }
        let faces_changed = matches!(&g, Some(Ok(g)) if g.new + g.connected + g.removed > 0) || groups.is_some_and(|r| r.assigned_faces + r.suggested_faces > 0);
        crate::log(&format!("apple-photos: {text}"));
        {
            let mut p = CHECK.lock().unwrap();
            p.message = text;
            if r.is_ok() {
                p.state_key = state_key;
            }
        }
        if r.as_ref().is_ok_and(|b| b.new > 0 || b.removed > 0 || b.state_changed > 0) || faces_changed || NAMES.lock().unwrap().is_none() {
            *NAMES.lock().unwrap() = None;
            RELOAD.store(true, Ordering::Relaxed);
        }
        RUNNING.store(false, Ordering::SeqCst);
        ctx.request_repaint();
    });
}

static NAMES: Mutex<Option<std::collections::HashMap<i64, String>>> = Mutex::new(None);

/// Original name of a photo from Apple Photos (e.g. `IMG_1234.HEIC`) instead of the identifier.
pub fn original_name(file: i64) -> Option<String> {
    let mut g = NAMES.lock().unwrap();
    g.get_or_insert_with(library::apple_photos::read_names).get(&file).cloned()
}

/// Maximum size of the cache for originals from Apple Photos (2 GB).
const CACHE_MAX: u64 = 2 * 1024 * 1024 * 1024;

/// Folder of the working copies (originals from Apple Photos for editing); can be emptied at any time.
pub fn cache() -> PathBuf {
    library::cache_folder().parent().map(|p| p.join("apple-photos-originals")).unwrap_or_else(|| PathBuf::from("/tmp/graineo-apple-photos-originals"))
}

/// Used space and number of working copies.
pub fn cache_status() -> (u64, usize) {
    std::fs::read_dir(cache()).map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).fold((0, 0), |(b, n), m| (b + m.len(), n + 1))).unwrap_or((0, 0))
}

/// Remove all working copies (own temporary files; the original stays in Apple Photos).
pub fn clear_cache() {
    let _ = std::fs::remove_dir_all(cache());
}

/// Remove the oldest working copies until the cache is below the maximum size.
fn prune() {
    let Ok(rd) = std::fs::read_dir(cache()) else { return };
    let mut v: Vec<(SystemTime, u64, PathBuf)> = rd.flatten().filter_map(|e| { let m = e.metadata().ok()?; m.is_file().then(|| (m.accessed().or_else(|_| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH), m.len(), e.path())) }).collect();
    let mut total: u64 = v.iter().map(|x| x.1).sum();
    v.sort_by_key(|x| x.0);
    for (_, n, p) in v {
        if total <= CACHE_MAX {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= n;
        }
    }
}

/// Readable file for a photo from Apple Photos: the original in the Photos library if it is stored locally there,
/// otherwise a working copy in the cache (via PhotoKit, downloaded from iCloud if necessary).
pub fn original(path: &std::path::Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    let uuid = library::apple_photos::uuid_from_path(path).ok_or("keine Kennung")?;
    let name = path.file_name().ok_or("kein Dateiname")?;
    let target = cache().join(name);
    if target.is_file() {
        return Ok(target);
    }
    std::fs::create_dir_all(cache()).map_err(|e| e.to_string())?;
    // Test hook: GRAINEO_NO_PHOTOKIT=1 – test runs never query Apple Photos (no dialog, no iCloud download)
    if crate::env::var_os("GRAINEO_NO_PHOTOKIT").is_some() {
        return Err("PhotoKit in Testläufen abgeschaltet".into());
    }
    #[cfg(target_os = "macos")]
    {
        crate::log(&format!("apple-photos: fetching original {uuid}"));
        let part = cache().join(format!(".{uuid}.laden"));
        let _ = std::fs::remove_file(&part);
        crate::photokit::write_original(&uuid, &part)?;
        std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
        prune();
        Ok(target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = uuid;
        Err(crate::tr("Apple Fotos gibt es nur auf dem Mac").into())
    }
}
