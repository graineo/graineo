//! Large preview level with the development: after an edit, the photo is developed once more in the
//! background at full resolution and stored as a developed preview of the large level
//! ([`library::preview::large`], e.g. 2560 px). The interactive render is smaller than that (≈ 2.8 MP),
//! so it cannot serve as the large level itself.
//!
//! - Latest wins: a new edit of the same photo replaces the waiting job; a render that has been overtaken
//!   in the meantime is discarded.
//! - Debounced ([`DELAY`]): while sliders are still being moved, nothing is computed.
//! - One photo at a time, on a low-priority thread; the UI never waits for it.
//! - Finished photos are collected for the library ([`take_finished`]), which then replaces the
//!   textures and removes outdated entries of the preview store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use develop::{Settings, View};

/// Wait this long after the last change before developing.
const DELAY: Duration = Duration::from_millis(1500);

struct Pending {
    path: PathBuf,
    settings: Settings,
    fingerprint: u64,
    due: Instant,
    generation: u64,
}

#[derive(Default)]
struct Queue {
    pending: HashMap<i64, Pending>,
    /// newest generation per file (a finished render with an older one is discarded)
    newest: HashMap<i64, u64>,
    counter: u64,
    finished: Vec<i64>,
    started: bool,
}

fn queue() -> &'static (Mutex<Queue>, Condvar) {
    static Q: OnceLock<(Mutex<Queue>, Condvar)> = OnceLock::new();
    Q.get_or_init(|| (Mutex::new(Queue::default()), Condvar::new()))
}

/// Develop the offline copy (large preview) of `file` – only for photos on other drives – (library file ID) with these settings, once the photo has been left alone for a moment.
/// `fingerprint`: of the settings ([`crate::browse::expected`]); if the stored offline copy already shows them, nothing is done.
/// `offline_developed`: does the offline copy in the preview store (made in the background) show the development? It
/// counts like a developed large preview (asked only when the fingerprint matches).
pub fn schedule(file: i64, path: &Path, settings: &Settings, fingerprint: u64, offline_developed: impl FnOnce() -> bool) {
    // only photos on other drives have an offline copy (large level); on this Mac the original is loaded
    if file <= 0 || library::platform::on_startup_volume(path) {
        return;
    }
    // already developed with exactly these settings: no second development at full resolution (each visit of a photo
    // otherwise cost a full render in the background while paging on)
    let level = library::preview::large();
    let (developed, stored_fp) = (library::preview::has_developed(file, level), crate::browse::large(path, level));
    // the offline copy made in the background has the same development (its fingerprint says so): developing it again
    // at full resolution cost 3–10 s of processor at every first look at a photo
    if stored_fp == Some(fingerprint) && (developed || offline_developed()) {
        return;
    }
    // why it is developed again (measurements: the same photo should not be developed twice with the same settings)
    if crate::env::var_os("GRAINEO_LOG").is_some() {
        crate::log(&format!("large preview scheduled {}: developed {developed}, fingerprint {} → {fingerprint:016x}", path.display(), stored_fp.map_or("none".into(), |v| format!("{v:016x}"))));
    }
    let (m, cv) = queue();
    let mut q = m.lock().unwrap();
    q.counter += 1;
    let generation = q.counter;
    q.newest.insert(file, generation);
    q.pending.insert(file, Pending { path: path.to_path_buf(), settings: settings.clone(), fingerprint, due: Instant::now() + DELAY, generation });
    if !q.started {
        q.started = true;
        std::thread::Builder::new().name("large-preview".into()).spawn(worker).ok();
    }
    cv.notify_all();
}

/// Files whose large preview was just written (for the library: refresh textures).
pub fn take_finished() -> Vec<i64> {
    std::mem::take(&mut queue().0.lock().unwrap().finished)
}


fn worker() {
    lower_priority();
    let (m, cv) = queue();
    loop {
        let job = {
            let mut q = m.lock().unwrap();
            loop {
                let now = Instant::now();
                let next = q.pending.iter().min_by_key(|(_, p)| p.due).map(|(f, p)| (*f, p.due));
                match next {
                    Some((file, due)) if due <= now => {
                        let p = q.pending.remove(&file).unwrap();
                        break (file, p);
                    }
                    Some((_, due)) => {
                        q = cv.wait_timeout(q, due - now).unwrap().0;
                    }
                    None => {
                        q = cv.wait(q).unwrap();
                    }
                }
            }
        };
        let (file, p) = job;
        // while the user is paging through images, opening them has priority (processor and NAS)
        crate::jobs::wait_for_foreground(Duration::from_secs(10));
        let level = library::preview::large();
        let t = Instant::now();
        // on the background threads (their priority follows the user's activity, see `offline_priority`)
        offline_priority();
        let result = library::platform::background_run(|| render(&p.path, &p.settings, level));
        let mut q = m.lock().unwrap();
        // overtaken by a newer edit: discard (the newer job is already waiting)
        if q.newest.get(&file) != Some(&p.generation) {
            continue;
        }
        q.newest.remove(&file);
        match result {
            Ok(img) => {
                drop(q);
                // the level may have changed during the render
                let level_now = library::preview::large();
                let img = library::preview::downscale(img, level_now);
                if library::preview::write_developed(file, level_now, &img) {
                    crate::browse::set_large(&p.path, level_now, p.fingerprint);
                    crate::log(&format!("large preview {}: {}×{} in {:.1} s", p.path.display(), img.width(), img.height(), t.elapsed().as_secs_f32()));
                    m.lock().unwrap().finished.push(file);
                }
            }
            Err(e) => {
                drop(q);
                crate::log(&format!("large preview {}: {e}", p.path.display()));
            }
        }
    }
}

/// Develop at full resolution (with crop and rotation, without darkroom border) and reduce to `level`.
pub fn render(path: &Path, settings: &Settings, level: u32) -> Result<image::RgbImage, String> {
    use crate::image_kind::ImageKind;
    // Apple Photos, cropped or rotated there: develop Apple's current version (as shown everywhere)
    let current = library::apple_photos::current_version(path);
    let path = current.as_deref().unwrap_or(path);
    if library::apple_photos::in_photos_library(path) && !path.is_file() {
        return Err(crate::tr("Original nur in iCloud").into());
    }
    match crate::image_kind::of(path) {
        ImageKind::Other => Err(crate::tr("lässt sich nicht entwickeln").into()),
        ImageKind::Foveon => render_foveon(path, settings, level),
        ImageKind::Raw | ImageKind::Done => {
            let mut doc = rawdev::RawDoc::open_for(path, settings, &|| true)?.ok_or("abgebrochen")?;
            // oriented as on opening (also rotated by hand in Lightroom): the settings (crop, rotation) refer to it;
            // without it a photo rotated in Lightroom got a large preview lying on its side
            rawdev::lightroom::apply_orientation(&mut doc);
            render_doc(&doc, settings, level)
        }
    }
}

/// Develop at full resolution in the mask space (sensor orientation, after lens correction, before crop, rotation and
/// perspective; for bitmap masks), with the photo's camera orientation in quarter turns.
pub fn render_mask_space(path: &Path, settings: &Settings) -> Result<(develop::Rgb16, u8), String> {
    use crate::image_kind::ImageKind;
    let current = library::apple_photos::current_version(path);
    let path = current.as_deref().unwrap_or(path);
    if library::apple_photos::in_photos_library(path) && !path.is_file() {
        return Err(crate::tr("Original nur in iCloud").into());
    }
    let s = for_render(settings)?;
    match crate::image_kind::of(path) {
        ImageKind::Other => Err(crate::tr("lässt sich nicht entwickeln").into()),
        ImageKind::Foveon => {
            let mut session = develop::Session::open_for(path, settings).map_err(|e| e.to_string())?;
            let img = session.render_full(&s);
            Ok((img, session.cal.orientation))
        }
        ImageKind::Raw | ImageKind::Done => {
            let mut doc = rawdev::RawDoc::open_for(path, &s, &|| true)?.ok_or("abgebrochen")?;
            rawdev::lightroom::apply_orientation(&mut doc);
            Ok((doc.render_full(&s)?, doc.orientation))
        }
    }
}

fn for_render(settings: &Settings) -> Result<Settings, String> {
    let s = crate::darkroom_border::without_border(Settings { view: View::Developed, ..settings.clone() });
    rawdev::film_profiles::for_export(&s, true)
}

fn to_rgb(img: develop::Rgb16, level: u32) -> Result<image::RgbImage, String> {
    let rgb = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).ok_or("Bildgröße passt nicht")?;
    Ok(library::preview::downscale(rgb, level))
}

fn render_foveon(path: &Path, settings: &Settings, level: u32) -> Result<image::RgbImage, String> {
    let s = for_render(settings)?;
    let mut session = develop::Session::open_for(path, settings).map_err(|e| e.to_string())?;
    let img = session.render_full(&s);
    to_rgb(develop::geometry::apply(img, &s, session.cal.orientation), level)
}

pub fn render_doc(doc: &rawdev::RawDoc, settings: &Settings, level: u32) -> Result<image::RgbImage, String> {
    let s = for_render(settings)?;
    let img = doc.render_full(&s)?;
    to_rgb(develop::geometry::apply(img, &s, doc.orientation), level)
}

/// Offline copy of a RAW or X3F in `level` ([`library::preview::set_developer`]): the development with the photo's own
/// settings; an unedited photo gets the standard development as on opening (Lightroom initial values, otherwise the
/// defaults), not the camera JPEG. Runs in a pool of low-priority threads, so the UI and the visible tiles stay fast.
pub fn render_offline(path: &Path, level: u32) -> Result<image::RgbImage, String> {
    render_offline_timed(path, level).0
}

/// Where the time of an offline copy went (s): waiting for the foreground (paging, opening), reading and
/// decoding the RAW, developing.
#[derive(Clone, Copy, Debug, Default)]
pub struct OfflineTimes {
    pub waited: f32,
    pub open: f32,
    pub develop: f32,
}

/// [`render_offline`] with the times of its steps. Waits before it starts and between the steps while the user is
/// paging or an image is being opened (an offline copy otherwise ran on during paging and took processor and NAS).
pub fn render_offline_timed(path: &Path, level: u32) -> (Result<image::RgbImage, String>, OfflineTimes) {
    use crate::image_kind::ImageKind;
    let times = Mutex::new(OfflineTimes::default());
    let wait = || {
        let w = crate::jobs::wait_for_foreground(Duration::from_secs(10)).as_secs_f32();
        times.lock().unwrap().waited += w;
        // the user may have started or stopped working meanwhile
        offline_priority();
    };
    if !path.is_file() {
        return (Err("Original nicht erreichbar".into()), OfflineTimes::default());
    }
    // opening images in the Develop module has priority (processor and NAS); at the gates of the development an
    // offline copy is background work (idle)
    pools::set_intent(pools::Intent::Idle);
    wait();
    offline_priority();
    let result = match crate::image_kind::of(path) {
        ImageKind::Foveon => library::platform::background_run(|| {
            let own = crate::sidecar::own_settings(path);
            render_foveon(path, &own.unwrap_or_default(), level)
        }),
        ImageKind::Raw => (|| {
            let t = Instant::now();
            // pauses between reading and decoding while the user pages (the time waited is counted separately)
            let keep_going = || {
                wait();
                true
            };
            // own settings first: they name the engine of the raw stage (graineo, Apple)
            let (own, _, stored) = crate::raw_app::load_settings(path, None);
            let doc = library::platform::background_run(|| rawdev::RawDoc::open_for(path, &own, &keep_going));
            let mut doc = doc?.ok_or("abgebrochen")?;
            // orientation as on opening (also rotated by hand in Lightroom), before initial values and crop
            rawdev::lightroom::apply_orientation(&mut doc);
            let s = if stored { own } else { rawdev::lightroom::initial_settings(&doc).unwrap_or_default() };
            {
                let mut tm = times.lock().unwrap();
                tm.open = t.elapsed().as_secs_f32() - tm.waited;
            }
            wait();
            let t = Instant::now();
            let img = library::platform::background_run(|| render_doc(&doc, &s, level))?;
            times.lock().unwrap().develop = t.elapsed().as_secs_f32();
            // fingerprint of what it shows; not with Upright from Lightroom (the app straightens with its own
            // detection on opening, the copy here does not – they would differ)
            if stored || s.raw.upright_from_lr.is_none() {
                crate::browse::set_large(path, level, if stored { crate::browse::settings_fp(&s) } else { crate::browse::standard() });
            }
            Ok(img)
        })(),
        ImageKind::Done | ImageKind::Other => Err(crate::tr("kein RAW").into()),
    };
    let t = *times.lock().unwrap();
    (result, t)
}

/// Priority of the calling thread (reading and decoding before the parallel part, one thread): while the user works
/// the lowest one; when nothing happens ([`crate::jobs::idle`]) "utility" – macOS throttles the disk and network
/// access of background threads so much that reading one RAW took minutes. That is at most one core outside the
/// efficiency cores; the parallel development stays in the background pool.
fn offline_priority() {
    use library::platform::{set_priority, Priority};
    set_priority(if crate::jobs::idle() { Priority::Utility } else { Priority::Background });
}

fn lower_priority() {
    library::platform::background_priority();
    pools::set_intent(pools::Intent::Idle);
}

#[cfg(test)]
mod tests {
    /// Develops a free test RAW (testbilder/signature-edits) at full resolution and reduces it to the large level (skipped without the file).
    #[test]
    fn renders_the_large_level_from_the_raw() {
        // GRAINEO_TEST_RAW points to a RAW elsewhere (testbilder/ is not part of the repository)
        let p = crate::env::var_os("GRAINEO_TEST_RAW").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/signature-edits/01-P1060804.dng"));
        if !p.exists() {
            eprintln!("skipped: {} missing", p.display());
            return;
        }
        let t = std::time::Instant::now();
        let img = super::render(&p, &develop::Settings::default(), 2560).expect("render");
        eprintln!("large preview {}×{} in {:.2} s", img.width(), img.height(), t.elapsed().as_secs_f32());
        assert_eq!(img.width().max(img.height()), 2560);
        // the image is not black
        let mean = img.pixels().map(|p| p.0[1] as u64).sum::<u64>() / img.pixels().len() as u64;
        assert!(mean > 10, "{mean}");
    }

    /// Time per offline copy (2560 px, or `GRAINEO_TEST_OFFLINE_LEVEL`) for a Bayer RAW, an X3F and a JPEG; paths from
    /// `GRAINEO_TEST_OFFLINE` (separated by `;`, only read). Skipped without it. Prints the size of the camera JPEG.
    #[test]
    fn offline_copy_times() {
        let Ok(list) = crate::env::var("GRAINEO_TEST_OFFLINE") else {
            eprintln!("skipped: GRAINEO_TEST_OFFLINE not set");
            return;
        };
        let level: u32 = crate::env::var("GRAINEO_TEST_OFFLINE_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(2560);
        for p in list.split(';').map(std::path::PathBuf::from) {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let format = library::format_from_extension(&name).unwrap_or("").to_string();
            let camera = library::preview::load(&p, &format, level).map(|(i, _)| i.width().max(i.height())).unwrap_or(0);
            for round in 0..2 {
                let t = std::time::Instant::now();
                let img = if library::preview::develops(&format) || library::is_raw(&format) {
                    super::render_offline(&p, level).expect("render")
                } else {
                    library::preview::load(&p, &format, level).expect("load").0
                };
                eprintln!("offline copy {name} (round {round}): {}×{} in {:.2} s (embedded/original at {level}: {camera} px)", img.width(), img.height(), t.elapsed().as_secs_f32());
                assert!(img.width().max(img.height()) <= level);
            }
        }
    }
}
