//! Renew the stored developed previews of Sigma photos whose development changed (version 0.95.0; revision 2: one
//! own processing path for every camera, Quattro colour and noise reduction rebuilt).
//!
//! Only previews that show graineo's own development are stale: the developed grid previews (`developed-grid`), the
//! offline copies in the preview store (marked as developed) and the last developed views. Camera JPEGs in the grid are
//! not affected. Which photos are affected is decided by camera and ISO (and, for Merrill, by the header):
//! - all Quattro (sd Quattro, sd Quattro H, dp Quattro): white balance "Auto (natural)", noise reduction, binning;
//! - Merrill above ISO 1600: sharpening image; Merrill saved by SIGMA Photo Pro with fringe or CA correction.
//!
//! Other formats and cameras are never touched (the global revision of [`crate::browse`] stays as it is).
//!
//! Second, separate renewal with the same machinery ([`LOOK`], marker `cameraLookPreviewRevision`): since the camera
//! look (picture style from the makernote, e.g. Fuji "Monochrom + Ye") is no longer applied automatically (an empty
//! look means neutral), non-Sigma RAWs that have a stored developed preview, an empty look and a picture style with a
//! bundled look still show the old automatic look. Only those are developed again. It runs after the Sigma renewal.
//!
//! Third renewal ([`COLOR`], marker `cameraColorPreviewRevision`): the standard development of some cameras got an even
//! saturation correction after the tone curve ([`raw::development::camera_saturation`]: Ricoh GR, GR II, GR III, GR IIIx,
//! GR IV, Fujifilm X100). Only RAWs of these cameras with a stored developed preview are developed again. Revision 2
//! (1.3.0): new hue table of the standard development for FinePix X100, X100V, Ricoh GR and GR III; after revision 1
//! only these four.
//!
//! Classic stage ([`CLASSIC`], marker `classicPreviewRevision`): in 0.107.0 the processing of all older Foveon
//! cameras (SD9, SD10, SD14, SD15, DP1, DP1s, DP1x, DP2, DP2s, DP2x) follows dcraw in most steps (sensor correction,
//! defects, chroma smoothing, colour coring) with an own highlight step. Photos of these cameras with a stored developed
//! preview are developed again. It runs right after the Sigma renewal.
//!
//! Fourth stage ([`FILM`], marker `filmGrainPreviewRevision`): in 0.107.0 every photo with an active film looks
//! different: the grain model (colour films: sub-layers or measured grain; B&W: measured grain size) and the corrections
//! towards the spektrafilm original (B&W spectral sensitivity, halation per film, light sources "T"/"K75P", GPU paths).
//! Photos with a stored developed preview and an active film are developed again.
//! It runs last, so a photo of a corrected camera with film is developed with both changes.
//!
//! Quiet: one photo at a time, only while the user leaves the app alone, lowest priority, idle pools. Resumable: the
//! photos done are listed in a file (`developed-grid/sigma-refresh-<revision>.txt`); the marker in the settings is
//! set when nothing is left. Raise [`REVISION`] to renew them again after a later change. Originals are only read.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use library::preview;

/// Raise it when the development of these cameras changes visibly again.
pub const REVISION: u64 = 2;
const MARKER: &str = "sigmaPreviewRevision";

/// One renewal run: which photos (by group), the marker in the settings, the list of photos done.
pub struct Stage {
    pub name: &'static str,
    pub marker: &'static str,
    pub revision: u64,
    done_name: &'static str,
    pub selects: fn(Group) -> bool,
}

/// The Sigma renewal of 0.95.0.
pub const SIGMA: Stage = Stage { name: "sigma", marker: MARKER, revision: REVISION, done_name: "sigma-refresh", selects: |g| !matches!(g, Group::Classic | Group::CameraLook | Group::FilmGrain) };
/// The older Foveon cameras after the processing change of 0.107.0 (one way for all of them). Raise the revision when
/// their development changes visibly again.
pub const CLASSIC: Stage = Stage { name: "classic", marker: "classicPreviewRevision", revision: 1, done_name: "classic-refresh", selects: |g| g == Group::Classic };
/// Non-Sigma RAWs with an empty look whose old previews show the former automatic camera look. Raise the revision to
/// repeat it.
pub const LOOK: Stage = Stage { name: "camera look", marker: "cameraLookPreviewRevision", revision: 1, done_name: "look-refresh", selects: |g| g == Group::CameraLook };
/// Non-Sigma RAWs of the cameras whose standard development changed (see [`color_changed`]).
/// Raise the revision when these factors or tables change visibly, and name the cameras in [`color_changed`].
pub const COLOR: Stage = Stage { name: "camera color", marker: "cameraColorPreviewRevision", revision: 2, done_name: "color-refresh", selects: |g| g == Group::CameraLook };
/// Photos with an active film after the grain model and the spektrafilm corrections of 0.107.0.
pub const FILM: Stage = Stage { name: "film grain", marker: "filmGrainPreviewRevision", revision: 1, done_name: "film-grain-refresh", selects: |g| g == Group::FilmGrain };

static STARTED: AtomicBool = AtomicBool::new(false);
/// the startup renewals are running (the learned-look renewal waits for them, they share the status counters)
static MAIN_RUNNING: AtomicBool = AtomicBool::new(false);
static LEARNED_RUNNING: AtomicBool = AtomicBool::new(false);
/// revision of the learned looks handled last (see [`maybe_start_learned`])
static LEARNED_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static LOOK_STAGE: AtomicBool = AtomicBool::new(false);
static COLOR_STAGE: AtomicBool = AtomicBool::new(false);
static FILM_STAGE: AtomicBool = AtomicBool::new(false);
static FINISHED: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// Which development changed for a camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Quattro,
    /// SD9 to DP2x (see [`CLASSIC`])
    Classic,
    /// Merrill above ISO 1600
    MerrillHighIso,
    /// Merrill up to ISO 1600: only if SIGMA Photo Pro saved fringe or CA correction in the file
    MerrillMaybeSpp,
    /// Non-Sigma RAW (any camera): affected if the look is empty and the picture style has a bundled look
    CameraLook,
    /// Any photo: affected if it has an active film (see [`FILM`])
    FilmGrain,
}

/// Model names of the older Foveon cameras (library camera name without "SIGMA", lower case).
const CLASSIC_MODELS: [&str; 10] = ["sd9", "sd10", "sd14", "sd15", "dp1", "dp1s", "dp1x", "dp2", "dp2s", "dp2x"];

/// Group of a photo by camera name and ISO (from the library); `None`: not affected.
pub fn group_of(camera: &str, iso: Option<i64>) -> Option<Group> {
    let c = camera.trim().to_ascii_lowercase();
    if c.contains("quattro") {
        Some(Group::Quattro)
    } else if CLASSIC_MODELS.contains(&c.strip_prefix("sigma ").unwrap_or(&c).trim()) {
        Some(Group::Classic)
    } else if c.contains("merrill") || c == "sigma sd1" {
        Some(if iso.is_some_and(|i| i > 1600) { Group::MerrillHighIso } else { Group::MerrillMaybeSpp })
    } else {
        None
    }
}

/// A photo of the library that may need a new preview.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub image: i64,
    pub file: i64,
    pub path: PathBuf,
    pub key: u64,
    pub group: Group,
    /// camera name as the library stores it (model, e.g. "GR II")
    pub camera: String,
}

/// Did the standard development of this camera (library camera name) change since revision `done` of [`COLOR`]?
/// Revision 1: saturation correction ([`raw::development::model_saturation`]); revision 2 (1.3.0): new hue table of
/// the standard development for FinePix X100, X100V, Ricoh GR and GR III ([`raw::development::camera_look`]).
pub fn color_changed(camera: &str, done: u64) -> bool {
    let m = camera.trim().to_ascii_uppercase();
    (done < 1 && raw::development::model_saturation(camera).is_some()) || (done < 2 && matches!(m.as_str(), "FINEPIX X100" | "X100V" | "GR" | "RICOH GR" | "GR III" | "RICOH GR III"))
}

/// Does the development of this photo differ from before? (Merrill up to ISO 1600: reads a few KB of the header.)
pub fn affected(c: &Candidate) -> bool {
    match c.group {
        Group::Quattro | Group::Classic | Group::MerrillHighIso => true,
        Group::CameraLook => look_stale(&c.path),
        // 0.107.0: not only the grain changed (see [`develop::film::grain_changed_since_models`]) but the film
        // simulation itself (B&W spectral sensitivity, halation, light sources): every active film is affected
        Group::FilmGrain => crate::sidecar::load(&c.path).0.film.is_active(),
        Group::MerrillMaybeSpp => develop::sppa::load(&c.path).is_some_and(|s| s.spp_defringe != Default::default() || s.ca_mode != develop::lens::CaMode::Off),
    }
}

/// Does the old automatic camera look still show in the stored previews of this photo? Yes if the stored look is empty
/// (neutral now) and the makernote's picture style has a bundled look (reads the start of the file only).
pub fn look_stale(path: &std::path::Path) -> bool {
    let (settings, _) = crate::sidecar::load(path);
    settings.raw.look.is_empty() && raw::picture_style::read(path).is_some_and(|b| raw::looks::Look::for_picture_style(&b).is_some())
}

fn done_file(stage: &Stage) -> PathBuf {
    preview::developed_folder().join(format!("{}-{}.txt", stage.done_name, stage.revision))
}

/// Remaining work for the status in the title bar: (done, total) while it runs.
pub fn progress() -> Option<(usize, usize)> {
    let total = TOTAL.load(Ordering::Relaxed);
    let done = DONE.load(Ordering::Relaxed);
    (total > 0 && done < total).then_some((done, total))
}

/// Does the running renewal belong to the camera look (for the wording of the status)?
pub fn look_stage_running() -> bool {
    LOOK_STAGE.load(Ordering::Relaxed)
}

/// Does the running renewal belong to the camera color correction (for the wording of the status)?
pub fn color_stage_running() -> bool {
    COLOR_STAGE.load(Ordering::Relaxed)
}

/// Does the running renewal belong to the film grain (for the wording of the status)?
pub fn film_stage_running() -> bool {
    FILM_STAGE.load(Ordering::Relaxed)
}

/// Photos whose previews were just renewed (the library drops their textures).
pub fn take_finished() -> Vec<i64> {
    std::mem::take(&mut *FINISHED.lock().unwrap())
}

/// Every frame from the app: starts the renewals once, as soon as the library is read. The stages run one after the
/// other in one quiet thread.
pub fn maybe_start(lib: &crate::library_ui::LibraryView) {
    if STARTED.load(Ordering::Relaxed) {
        return;
    }
    let stages: Vec<&'static Stage> = [&SIGMA, &CLASSIC, &LOOK, &COLOR, &FILM].into_iter().filter(|s| library::settings::value(s.marker).as_u64().unwrap_or(0) < s.revision).collect();
    if cfg!(test) || stages.is_empty() {
        STARTED.store(true, Ordering::Relaxed);
        return;
    }
    let Some((cache, cands)) = lib.sigma_refresh_candidates() else { return };
    STARTED.store(true, Ordering::Relaxed);
    MAIN_RUNNING.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("sigma-refresh".into())
        .spawn(move || {
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    MAIN_RUNNING.store(false, Ordering::Relaxed);
                }
            }
            let _done = Done;
            library::platform::background_priority();
            pools::set_intent(pools::Intent::Idle);
            for stage in stages {
                let done: HashSet<i64> = std::fs::read_to_string(done_file(stage)).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect();
                let mut list: Vec<Candidate> = cands.iter().filter(|c| (stage.selects)(c.group) && !done.contains(&c.file)).cloned().collect();
                if stage.name == LOOK.name || stage.name == FILM.name {
                    // almost every RAW of the library is a candidate: only the photos with a stored developed preview count
                    list.retain(|c| has_stored(&cache, c));
                }
                if stage.name == COLOR.name {
                    let done_revision = library::settings::value(COLOR.marker).as_u64().unwrap_or(0);
                    list.retain(|c| color_changed(&c.camera, done_revision) && has_stored(&cache, c));
                }
                let every: &dyn Fn(&Candidate) -> bool = &|_| true;
                let r = run_stage(stage, &cache, &list, if stage.name == COLOR.name { every } else { &affected }, true);
                crate::log(&format!("{} previews: {r:?}", stage.name));
                if r.unreachable == 0 {
                    library::settings::set(stage.marker, serde_json::json!(stage.revision));
                }
            }
        })
        .ok();
}

/// Every frame from the app: renew the stored developed previews of the camera and style whose learned look was
/// switched, changed or reset ([`rawdev::learn::stale`]). Only unedited photos (empty look, no film) of that key show
/// it; the picture style is read from the start of the file. Quiet like the other renewals, after them; started again
/// whenever the learned looks change.
pub fn maybe_start_learned(lib: &crate::library_ui::LibraryView) {
    if cfg!(test) || MAIN_RUNNING.load(Ordering::Relaxed) || LEARNED_RUNNING.load(Ordering::Relaxed) || !STARTED.load(Ordering::Relaxed) {
        return;
    }
    let generation = rawdev::learn::generation();
    if LEARNED_SEEN.load(Ordering::Relaxed) == generation {
        return;
    }
    // read once per generation, not in every frame while the library is still loading (the list is a file)
    static STALE: Mutex<Option<(u64, Vec<String>, u64)>> = Mutex::new(None);
    let (keys, revision) = {
        let mut g = STALE.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref().filter(|s| s.0 == generation) {
            Some(s) => (s.1.clone(), s.2),
            None => {
                let (k, r) = rawdev::learn::stale();
                *g = Some((generation, k.clone(), r));
                (k, r)
            }
        }
    };
    if keys.is_empty() {
        LEARNED_SEEN.store(generation, Ordering::Relaxed);
        return;
    }
    let Some((cache, cands)) = lib.sigma_refresh_candidates() else { return };
    LEARNED_SEEN.store(generation, Ordering::Relaxed);
    LEARNED_RUNNING.store(true, Ordering::Relaxed);
    let cameras: HashSet<String> = keys.iter().filter_map(|k| k.split('/').next().map(str::to_string)).collect();
    let started = std::thread::Builder::new()
        .name("learned-look-refresh".into())
        .spawn(move || {
            // not running any more also after a panic (else the renewal of learned looks stopped until the next start)
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    LEARNED_RUNNING.store(false, Ordering::Relaxed);
                }
            }
            let _done = Done;
            library::platform::background_priority();
            pools::set_intent(pools::Intent::Idle);
            let list: Vec<Candidate> = cands.into_iter().filter(|c| c.group == Group::CameraLook && maybe_camera(&c.camera, &cameras) && has_stored(&cache, &c)).collect();
            let stage = Stage { name: LOOK.name, marker: "", revision, done_name: "learned-refresh", selects: |g| g == Group::CameraLook };
            let keys2 = keys.clone();
            let affected = move |c: &Candidate| {
                let (settings, _) = crate::sidecar::load(&c.path);
                settings.raw.look.is_empty()
                    && !settings.film.is_active()
                    && raw::picture_style::read(&c.path).filter(raw::learn_look::learnable_style).is_some_and(|b| keys2.contains(&format!("{}/{}", b.camera_short(), raw::learn_look::style_key(&b))))
            };
            let r = run_stage(&stage, &cache, &list, &affected, true);
            crate::log(&format!("learned look previews: {r:?}"));
            if r.unreachable == 0 {
                rawdev::learn::mark_fresh(&keys, revision);
            }
            let _ = std::fs::remove_file(done_file(&stage));
        });
    if started.is_err() {
        LEARNED_RUNNING.store(false, Ordering::Relaxed);
    }
}

/// Could the library camera name (model, e.g. "RICOH GR III", "FinePix X100") belong to one of these short camera
/// names (`gr3`, `x100`)? The library does not store the maker, so the usual makers are tried.
fn maybe_camera(model: &str, cameras: &HashSet<String>) -> bool {
    ["", "RICOH", "PENTAX", "FUJIFILM", "Canon", "SONY", "NIKON", "LEICA", "OLYMPUS", "Panasonic"].iter().any(|m| cameras.contains(&raw::picture_style::camera_short(m, model)))
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// photos looked at
    pub looked: usize,
    /// previews renewed (photos)
    pub renewed: usize,
    /// affected, but without a developed preview: nothing to renew
    pub without_preview: usize,
    /// not affected (e.g. Merrill without SPP changes)
    pub unaffected: usize,
    /// original not reachable now: tried again at the next start
    pub unreachable: usize,
    pub failed: usize,
}

const DEVELOPED_LEVELS: [u32; 6] = [256, 512, 1024, 2560, 4000, preview::FULL];

/// Developed levels of a photo: (grid previews, offline copies in the store).
fn stored_levels(cache: &preview::Cache, c: &Candidate) -> (Vec<u32>, Vec<u32>) {
    let grid = DEVELOPED_LEVELS.into_iter().filter(|&l| preview::has_developed(c.file, l)).collect();
    let store = cache.levels_of(c.key).into_iter().filter(|&l| cache.is_developed(c.key, l)).collect();
    (grid, store)
}

/// Does the photo have a stored developed preview (grid, offline copy, last developed view)?
fn has_stored(cache: &preview::Cache, c: &Candidate) -> bool {
    let (grid, store) = stored_levels(cache, c);
    !grid.is_empty() || !store.is_empty() || view_file(c.image).is_file()
}

/// The renewal. `write`: store the result (otherwise only count). `is_affected`: see [`affected`].
pub fn run(cache: &preview::Cache, cands: &[Candidate], is_affected: &dyn Fn(&Candidate) -> bool, write: bool) -> Report {
    run_stage(&SIGMA, cache, cands, is_affected, write)
}

/// [`run`] for one stage (it names the file of the photos done).
pub fn run_stage(stage: &Stage, cache: &preview::Cache, cands: &[Candidate], is_affected: &dyn Fn(&Candidate) -> bool, write: bool) -> Report {
    let mut r = Report::default();
    LOOK_STAGE.store(stage.name == LOOK.name, Ordering::Relaxed);
    COLOR_STAGE.store(stage.name == COLOR.name, Ordering::Relaxed);
    FILM_STAGE.store(stage.name == FILM.name, Ordering::Relaxed);
    // all photos of the list count for the status; the work stays in order
    TOTAL.store(cands.len(), Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);
    let mut seen: HashSet<i64> = HashSet::new();
    for c in cands {
        r.looked += 1;
        let finish = |remember: bool| {
            DONE.fetch_add(1, Ordering::Relaxed);
            if write && remember {
                use std::io::Write;
                // the folder may not exist yet (fresh data folder): without it the progress was lost silently and every
                // start checked all candidates again
                let path = done_file(stage);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "{}", c.file);
                }
            }
        };
        let (grid, store) = stored_levels(cache, c);
        // the last developed view (placeholder when opening) of this image
        let view = view_file(c.image);
        let has_view = view.is_file();
        if grid.is_empty() && store.is_empty() && !has_view {
            r.without_preview += 1;
            finish(true);
            continue;
        }
        if !is_affected(c) {
            r.unaffected += 1;
            finish(true);
            continue;
        }
        if !c.path.is_file() {
            r.unreachable += 1;
            finish(false);
            continue;
        }
        // another virtual-copy-free photo with the same file: renew once
        if !seen.insert(c.file) {
            finish(true);
            continue;
        }
        // the user comes first: wait until they leave the app alone
        wait_until_quiet();
        let Some(top) = grid.iter().chain(store.iter()).copied().max() else {
            // only the placeholder: it is discarded, the view is developed again when the photo is opened
            if write {
                let _ = std::fs::remove_file(&view);
                crate::browse::forget_developed(c.image);
            }
            r.renewed += 1;
            finish(true);
            continue;
        };
        let (settings, _) = crate::sidecar::load(&c.path);
        let img = match library::platform::background_run(|| crate::large_preview::render(&c.path, &settings, top)) {
            Ok(i) => i,
            Err(e) => {
                crate::log(&format!("{} previews {}: {e}", stage.name, c.path.display()));
                r.failed += 1;
                finish(true);
                continue;
            }
        };
        if write {
            for &l in &grid {
                preview::write_developed(c.file, l, &preview::downscale(img.clone(), l));
            }
            for &l in &store {
                cache.write_marked(c.key, l, &preview::downscale(img.clone(), l));
            }
            // the placeholder is made again from the development when the photo is opened
            if has_view {
                let _ = std::fs::remove_file(&view);
                crate::browse::forget_developed(c.image);
            }
            FINISHED.lock().unwrap().push(c.file);
        }
        r.renewed += 1;
        finish(true);
    }
    r
}

fn view_file(image: i64) -> PathBuf {
    library::data_dir().join("previews").join("developed").join(format!("{image}.jpg"))
}

/// Waits until the app has been left alone for a while (at most one photo at a time is developed then).
fn wait_until_quiet() {
    while !cfg!(test) && !crate::jobs::idle() {
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_camera_and_iso() {
        assert_eq!(group_of("SIGMA sd Quattro H", Some(100)), Some(Group::Quattro));
        assert_eq!(group_of("SIGMA dp3 Quattro", None), Some(Group::Quattro));
        assert_eq!(group_of("SIGMA SD10", Some(100)), Some(Group::Classic));
        assert_eq!(group_of("SIGMA SD9", None), Some(Group::Classic));
        for cam in ["SIGMA SD14", "SIGMA SD15", "SIGMA DP1", "SIGMA DP1S", "SIGMA DP1X", "SIGMA DP2", "SIGMA DP2s", "SIGMA DP2X", "SD10"] {
            assert_eq!(group_of(cam, Some(6400)), Some(Group::Classic), "{cam}");
        }
        assert!((CLASSIC.selects)(Group::Classic) && !(SIGMA.selects)(Group::Classic) && !(CLASSIC.selects)(Group::Quattro) && !(CLASSIC.selects)(Group::FilmGrain));
        assert_eq!(group_of("SIGMA DP2 Merrill", Some(3200)), Some(Group::MerrillHighIso));
        assert_eq!(group_of("SIGMA DP2 Merrill", Some(1600)), Some(Group::MerrillMaybeSpp));
        assert_eq!(group_of("SIGMA SD1 Merrill", Some(100)), Some(Group::MerrillMaybeSpp));
        // other cameras and formats stay untouched
        for cam in ["Sigma fp", "SIGMA fp L", "Canon EOS R5", "FUJIFILM X-T5"] {
            assert_eq!(group_of(cam, Some(6400)), None, "{cam}");
        }
    }

    /// Stale developed previews (grid, offline copy, placeholder) of an affected photo are renewed, those of a photo
    /// that is not affected and files without previews stay. Needs `GRAINEO_DATA_DIR`; X3F from
    /// `GRAINEO_TEST_X3F` (default: `DP2M5418.X3F` in `GRAINEO_TEST_MERRILL_DIR`, only read).
    #[test]
    fn renews_only_affected_photos() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        // The renewal runs in the background pool. Its workers (lowest priority, efficiency cores) get next to no
        // processor time while the computer is busy (measured during parallel builds, load 45–60: both workers
        // runnable, 0.4–2 s of processor time per minute, 202 s instead of 48 s; nothing was waiting). The test checks
        // which previews are renewed, not priorities, so the pool runs as "utility" meanwhile (as in the lock-up test).
        library::platform::background_as_utility(true);
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                library::platform::background_as_utility(false);
            }
        }
        let _restore = Restore;
        let x3f = crate::env::var_os("GRAINEO_TEST_X3F").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("GRAINEO_TEST_MERRILL_DIR").unwrap_or_default()).join("DP2M5418.X3F"));
        if !x3f.is_file() {
            eprintln!("skipped: {} missing", x3f.display());
            return;
        }
        let (a, b) = (910_001_i64, 910_002_i64);
        let stale = image::RgbImage::from_pixel(300, 200, image::Rgb([7, 7, 7]));
        let large = preview::large();
        for f in [a, b] {
            preview::write_developed(f, 512, &stale);
        }
        preview::write_developed(a, large, &stale);
        let cache = preview::Cache::open(&std::env::temp_dir().join(format!("graineo-refresh-{}", std::process::id())));
        assert!(cache.write_marked(a as u64, 1024, &stale));
        let view = view_file(a);
        std::fs::create_dir_all(view.parent().unwrap()).unwrap();
        stale.save(&view).unwrap();
        let cand = |image: i64, file: i64, group| Candidate { image, file, path: x3f.clone(), key: file as u64, group, camera: String::new() };
        let cands = vec![cand(a, a, Group::Quattro), cand(b, b, Group::MerrillMaybeSpp), cand(920_000, 920_000, Group::Quattro)];
        let mean = |i: &image::RgbImage| i.pixels().map(|p| p.0[1] as u64).sum::<u64>() / i.pixels().len() as u64;
        // counting only changes nothing
        let r = run(&cache, &cands, &|c| c.group == Group::Quattro, false);
        assert_eq!((r.renewed, r.unaffected, r.without_preview), (1, 1, 1), "{r:?}");
        assert_eq!(mean(&preview::read_developed(a, 512).unwrap()), 7);
        let r = run(&cache, &cands, &|c| c.group == Group::Quattro, true);
        assert_eq!((r.renewed, r.unaffected, r.without_preview, r.failed), (1, 1, 1, 0), "{r:?}");
        let fresh = preview::read_developed(a, 512).unwrap();
        assert!(mean(&fresh) > 20 && fresh.width().max(fresh.height()) == 512, "{}", mean(&fresh));
        assert!(mean(&preview::read_developed(a, large).unwrap()) > 20);
        assert!(cache.is_developed(a as u64, 1024) && mean(&cache.read(a as u64, 1024).unwrap()) > 20);
        assert!(!view.is_file(), "placeholder removed");
        // not affected: untouched
        assert_eq!(mean(&preview::read_developed(b, 512).unwrap()), 7);
        assert_eq!(take_finished(), vec![a]);
        preview::remove_developed(a, &[512, large]);
        preview::remove_developed(b, &[512]);
    }

    /// The color renewal picks only the cameras whose standard development changed (library camera names): from
    /// scratch the saturation cameras and the new hue tables, after revision 1 only X100, X100V, GR and GR III.
    #[test]
    fn color_renewal_selects_by_camera() {
        for cam in ["GR", "GR II", "RICOH GR III", "RICOH GR IIIx", "RICOH GR IV", "RICOH GR IV                     ", "FinePix X100", "X100V"] {
            assert!(color_changed(cam, 0), "{cam}");
        }
        for cam in ["GR DIGITAL 4", "X100F", "X-T30", "DSC-RX100M3", "PENTAX Optio 550", "", "SIGMA DP2 Merrill"] {
            assert!(!color_changed(cam, 0), "{cam}");
        }
        for cam in ["FinePix X100", "X100V", "GR", "RICOH GR III"] {
            assert!(color_changed(cam, 1) && !color_changed(cam, 2), "{cam}");
        }
        for cam in ["GR II", "RICOH GR IIIx", "RICOH GR IV", "X100F", "X-T30"] {
            assert!(!color_changed(cam, 1), "{cam}");
        }
        assert!((COLOR.selects)(Group::CameraLook) && !(COLOR.selects)(Group::Quattro));
    }

    /// Minimal TIFF-like file (little endian) of a Fujifilm X-T30 with the makernote "Acros + Ye" (Saturation 0x502).
    fn fuji_file(dir: &std::path::Path, name: &str, model: &str, saturation: u16) -> PathBuf {
        let mut b: Vec<u8> = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        let make = b"FUJIFILM\0";
        let model = [model.as_bytes(), b"\0"].concat();
        let strings = 8 + 2 + 3 * 12 + 4;
        let exif = (strings + make.len() + model.len()) as u32;
        b.extend(3u16.to_le_bytes());
        for (tag, ty, n, v) in [(0x010fu16, 2u16, make.len() as u32, strings as u32), (0x0110, 2, model.len() as u32, (strings + make.len()) as u32), (0x8769, 4, 1, exif)] {
            b.extend(tag.to_le_bytes());
            b.extend(ty.to_le_bytes());
            b.extend(n.to_le_bytes());
            b.extend(v.to_le_bytes());
        }
        b.extend(0u32.to_le_bytes());
        b.extend(make);
        b.extend(&model);
        // Exif IFD with the makernote (12-byte header "FUJIFILM" + IFD offset, then one entry)
        let mn_at = b.len() + 2 + 12 + 4;
        b.extend(1u16.to_le_bytes());
        b.extend(0x927cu16.to_le_bytes());
        b.extend(7u16.to_le_bytes());
        b.extend(30u32.to_le_bytes());
        b.extend((mn_at as u32).to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(b"FUJIFILM");
        b.extend(12u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(0x1003u16.to_le_bytes());
        b.extend(3u16.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend((saturation as u32).to_le_bytes());
        b.extend(0u32.to_le_bytes());
        let p = dir.join(name);
        std::fs::write(&p, b).unwrap();
        p
    }

    /// The camera-look renewal picks only photos with an empty look and a picture style that has a bundled look, and
    /// only those with a stored developed preview are counted (nothing is written here).
    #[test]
    fn look_renewal_selects_by_picture_style() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-lookrefresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let acros = fuji_file(&dir, "a.raf", "X-T30", 0x502);
        let plain = fuji_file(&dir, "b.raf", "X-T30", 0x999);
        assert!(!look_stale(&plain), "picture style without bundled look");
        let other_cam = fuji_file(&dir, "c.raf", "X-Z99", 0x502);
        assert!(look_stale(&acros), "empty look + picture style with a look");
        assert!(!look_stale(&other_cam), "camera without bundled look");
        // stored previews decide what is counted
        let (a, b) = (930_001_i64, 930_002_i64);
        preview::write_developed(a, 512, &image::RgbImage::from_pixel(64, 40, image::Rgb([7, 7, 7])));
        let cache = preview::Cache::open(&dir.join("cache"));
        let cand = |image: i64, path: &PathBuf| Candidate { image, file: image, path: path.clone(), key: image as u64, group: Group::CameraLook, camera: "X-T30".into() };
        let cands = vec![cand(a, &acros), cand(b, &acros), cand(940_000, &other_cam)];
        assert!(has_stored(&cache, &cands[0]) && !has_stored(&cache, &cands[1]));
        assert!((LOOK.selects)(Group::CameraLook) && !(LOOK.selects)(Group::Quattro) && (SIGMA.selects)(Group::Quattro) && !(SIGMA.selects)(Group::CameraLook));
        assert!((FILM.selects)(Group::FilmGrain) && !(SIGMA.selects)(Group::FilmGrain) && !(LOOK.selects)(Group::FilmGrain) && !(COLOR.selects)(Group::FilmGrain));
        assert!(!(FILM.selects)(Group::CameraLook) && !(FILM.selects)(Group::Quattro));
        let r = run_stage(&LOOK, &cache, &cands, &affected, false);
        // the one with a stored preview is selected and developed (this synthetic file has no image: counted as failed)
        assert_eq!((r.renewed + r.failed, r.without_preview, r.unaffected), (1, 2, 0), "{r:?}");
        preview::remove_developed(a, &[512]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
