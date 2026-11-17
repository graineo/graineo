//! One-off check of the developed previews after the mix-up of 0.73–0.80.
//!
//! From 0.73 on, results of background jobs could land on another photo when the image list changed (another folder)
//! while they ran, and photos of two viewed folders shared their IDs. The developed view of the wrong photo was then
//! shown and stored as the preview of the right one (grid, filmstrip, placeholder when opening). Separately, the large
//! developed preview of a photo rotated by hand in Lightroom was developed without that rotation (lying on its side).
//!
//! Once per installation (marker `developedPreviewCheck` in the settings) and in the background at low priority:
//! - the last developed views (`previews/developed`, pure placeholders) stored since the first installation of 0.73
//!   are discarded with their fingerprints;
//! - every developed grid preview stored since then is compared with a fresh development of its own photo (same
//!   settings as the offline copy: own settings, otherwise the Lightroom initial values). If it does not show that
//!   photo (other orientation or other picture), it is replaced by the fresh one, and the copies made from it in the
//!   preview store are removed (they are made again);
//! - large developed previews of any age are checked for their orientation the same way.
//!
//! Originals and sidecars are only read. Photos whose original is not reachable right now are checked at a later
//! start (progress in `developed-grid/check-1.txt`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use library::preview;

/// Settings key of the marker; raise the value for a new check.
const MARKER: &str = "developedPreviewCheck";
const VERSION: u64 = 1;
/// Start of 7 Nov 2026 (local time), before the first installation of 0.73.0 (00:43): everything stored since then is
/// checked (generously earlier, as agreed).
pub const SINCE: u64 = 1_794_006_000;

static STARTED: AtomicBool = AtomicBool::new(false);

fn done_file() -> PathBuf {
    preview::developed_folder().join(format!("check-{VERSION}.txt"))
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// Developed grid previews: (file ID, level, modified).
fn developed_grid() -> Vec<(i64, u32, u64)> {
    let Ok(rd) = std::fs::read_dir(preview::developed_folder()) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (f, l) = name.strip_suffix(".jpg")?.split_once('-')?;
            Some((f.parse().ok()?, l.parse().ok()?, mtime(&e.path())))
        })
        .collect()
}

/// Files whose developed previews are checked: a level stored since [`SINCE`], or a large level (orientation).
pub fn candidates(grid: &[(i64, u32, u64)], since: u64) -> HashSet<i64> {
    grid.iter().filter(|(_, l, t)| *t >= since || preview::is_heavy(*l)).map(|(f, _, _)| *f).collect()
}

/// Every frame from the app: starts the check once, as soon as the preview store is open.
pub fn maybe_start(lib: &crate::library_ui::LibraryView) {
    if STARTED.load(Ordering::Relaxed) {
        return;
    }
    // tests start it themselves
    if cfg!(test) || library::settings::value(MARKER).as_u64().unwrap_or(0) >= VERSION {
        STARTED.store(true, Ordering::Relaxed);
        return;
    }
    let grid = developed_grid();
    let done: HashSet<i64> = std::fs::read_to_string(done_file()).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect();
    let files: HashSet<i64> = candidates(&grid, SINCE).into_iter().filter(|f| !done.contains(f)).collect();
    let Some((cache, jobs)) = lib.preview_check_jobs(&files) else { return };
    STARTED.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("preview-check".into())
        .spawn(move || {
            // nobody waits for it
            library::platform::background_priority();
            let r = run(&cache, &jobs, &grid, SINCE, true);
            crate::log(&format!("preview check: {r:?}"));
            if r.unreachable == 0 && r.failed == 0 {
                library::settings::set(MARKER, serde_json::json!(VERSION));
            }
        })
        .ok();
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// last developed views (placeholders) discarded
    pub views_removed: usize,
    pub checked: usize,
    /// small developed preview showed another photo or orientation: replaced
    pub wrong_small: usize,
    /// large developed preview with another orientation: removed
    pub wrong_large: usize,
    pub unreachable: usize,
    pub failed: usize,
    /// file IDs found wrong
    pub wrong_files: Vec<i64>,
}

/// The check itself. `write`: repair (otherwise only count – for measurements).
pub fn run(cache: &preview::Cache, jobs: &[preview::Job], grid: &[(i64, u32, u64)], since: u64, write: bool) -> Report {
    let mut r = Report::default();
    if write {
        r.views_removed = remove_views(since);
    }
    let (small, large) = (preview::small(), preview::large());
    for job in jobs {
        crate::jobs::wait_for_foreground(Duration::from_secs(10));
        let levels: Vec<(u32, u64)> = grid.iter().filter(|(f, _, _)| *f == job.file).map(|(_, l, t)| (*l, *t)).collect();
        if !job.path.is_file() {
            r.unreachable += 1;
            continue;
        }
        let fresh = match standard(&job.path, small) {
            Ok(img) => img,
            Err(e) => {
                crate::log(&format!("preview check {}: {e}", job.path.display()));
                r.failed += 1;
                continue;
            }
        };
        r.checked += 1;
        let mut wrong = false;
        // small level (and the others stored since then): the same photo?
        for &(level, _) in levels.iter().filter(|(l, t)| *t >= since && !preview::is_heavy(*l)) {
            let Some(stored) = preview::read_developed(job.file, level) else { continue };
            if !same_photo(&stored, &fresh) {
                wrong = true;
                r.wrong_small += 1;
                if write {
                    preview::write_developed(job.file, level, &preview::downscale(fresh.clone(), level));
                }
            }
        }
        // large level: the same orientation?
        for &(level, _) in levels.iter().filter(|(l, _)| preview::is_heavy(*l)) {
            let Ok((w, h)) = image::image_dimensions(preview::developed_folder().join(format!("{}-{level}.jpg", job.file))) else { continue };
            if shape(w, h).is_some_and(|s| Some(s) != shape(fresh.width(), fresh.height())) {
                r.wrong_large += 1;
                wrong = true;
                if write {
                    preview::remove_developed(job.file, &[level]);
                    crate::browse::forget_large(&job.path, level);
                    cache.remove(job.key, level);
                }
            }
        }
        if wrong {
            r.wrong_files.push(job.file);
            if write {
                // copies of the wrong preview in the preview store (the grid takes them before the developed one)
                cache.remove(job.key, small);
                if !preview::has_developed(job.file, large) {
                    cache.remove(job.key, large);
                }
            }
        }
        if write {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(done_file()) {
                let _ = writeln!(f, "{}", job.file);
            }
        }
    }
    r
}

/// Discard the last developed views stored since `since` (with their fingerprints). Returns: number removed.
fn remove_views(since: u64) -> usize {
    let dir = library::data_dir().join("previews").join("developed");
    let Ok(rd) = std::fs::read_dir(&dir) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        let Some(image) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".jpg")).and_then(|n| n.parse::<i64>().ok()) else { continue };
        if mtime(&p) >= since && std::fs::remove_file(&p).is_ok() {
            crate::browse::forget_developed(image);
            n += 1;
        }
    }
    n
}

/// Standard development of a photo as its offline copy has it (own settings, otherwise the Lightroom initial values;
/// oriented as on opening), reduced to `level`.
pub fn standard(path: &Path, level: u32) -> Result<image::RgbImage, String> {
    use crate::image_kind::ImageKind;
    match crate::image_kind::of(path) {
        ImageKind::Foveon => {
            let own = crate::sidecar::own_settings(path);
            crate::large_preview::render(path, &own.unwrap_or_default(), level)
        }
        ImageKind::Raw | ImageKind::Done => {
            let (own, _, stored) = crate::raw_app::load_settings(path, None);
            let mut doc = rawdev::RawDoc::open_for(path, &own, &|| true)?.ok_or("abgebrochen")?;
            rawdev::lightroom::apply_orientation(&mut doc);
            let s = if stored { own } else { rawdev::lightroom::initial_settings(&doc).unwrap_or_default() };
            crate::large_preview::render_doc(&doc, &s, level)
        }
        ImageKind::Other => Err(crate::tr("lässt sich nicht entwickeln").into()),
    }
}

/// Landscape (`true`) or portrait (`false`); `None` for nearly square images.
fn shape(w: u32, h: u32) -> Option<bool> {
    let r = w as f32 / h.max(1) as f32;
    if r > 1.08 {
        Some(true)
    } else if r < 1.0 / 1.08 {
        Some(false)
    } else {
        None
    }
}

/// Brightness pattern 24 × 24 (normalised).
fn pattern(img: &image::RgbImage) -> Vec<f32> {
    let g = image::imageops::grayscale(img);
    let s = image::imageops::resize(&g, 24, 24, image::imageops::FilterType::Triangle);
    let v: Vec<f32> = s.pixels().map(|p| p.0[0] as f32).collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    let d = (v.iter().map(|x| (x - m).powi(2)).sum::<f32>()).sqrt().max(1e-3);
    v.iter().map(|x| (x - m) / d).collect()
}

/// Does the stored preview show this photo (same orientation, similar picture)? Colour and tone may differ
/// (a slider moved since), the picture itself not.
pub fn same_photo(stored: &image::RgbImage, fresh: &image::RgbImage) -> bool {
    let (a, b) = (shape(stored.width(), stored.height()), shape(fresh.width(), fresh.height()));
    if a.is_some() && b.is_some() && a != b {
        return false;
    }
    similarity(stored, fresh) >= 0.5
}

/// Correlation of the brightness patterns (1 = same picture, around 0 = unrelated).
pub fn similarity(a: &image::RgbImage, b: &image::RgbImage) -> f32 {
    pattern(a).iter().zip(pattern(b)).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(seed: u32, w: u32, h: u32) -> image::RgbImage {
        image::RgbImage::from_fn(w, h, |x, y| {
            let v = ((x * (seed + 3) / 7 + y * (seed * 5 + 1) / 11 + (x * y * seed) / 97) % 256) as u8;
            image::Rgb([v, v / 2 + 40, 255 - v])
        })
    }

    #[test]
    fn same_photo_tolerates_tone_but_not_another_picture() {
        let a = photo(1, 600, 400);
        // darker, black and white: still the same photo
        let darker = image::RgbImage::from_fn(600, 400, |x, y| {
            let p = a.get_pixel(x, y).0;
            let g = ((p[0] as u32 + p[1] as u32 + p[2] as u32) / 6) as u8;
            image::Rgb([g, g, g])
        });
        assert!(same_photo(&darker, &a), "{}", similarity(&darker, &a));
        // another picture
        let other = image::RgbImage::from_fn(600, 400, |x, y| if (x / 50 + y / 40) % 2 == 0 { image::Rgb([250, 250, 250]) } else { image::Rgb([10, 10, 10]) });
        assert!(!same_photo(&other, &a), "{}", similarity(&other, &a));
        // lying on its side
        let side = image::imageops::rotate90(&a);
        assert!(!same_photo(&side, &a));
    }

    /// Measurement only (nothing is written): how many developed previews of a copy of the preview folder are wrong.
    /// `GRAINEO_PREVIEW_CHECK_JOBS`: file with `file ID|path|format` per line (from the library, read only).
    #[test]
    fn count_wrong_previews_of_a_copy() {
        let Some(list) = crate::env::var_os("GRAINEO_PREVIEW_CHECK_JOBS") else {
            eprintln!("skipped: GRAINEO_PREVIEW_CHECK_JOBS not set");
            return;
        };
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let grid = developed_grid();
        let files = candidates(&grid, SINCE);
        let jobs: Vec<preview::Job> = std::fs::read_to_string(list)
            .unwrap()
            .lines()
            .filter_map(|l| {
                let mut p = l.split('|');
                let file: i64 = p.next()?.parse().ok()?;
                let path = PathBuf::from(p.next()?);
                let format = p.next()?.to_string();
                files.contains(&file).then(|| preview::Job { file, path, format, key: 0, level: preview::small(), orientation: None })
            })
            .collect();
        let cache = preview::Cache::open(&std::env::temp_dir().join("graineo-check-cache-unused"));
        let r = run(&cache, &jobs, &grid, SINCE, false);
        eprintln!("preview check (count only): {} candidates, {r:?}", files.len());
    }

    /// Repair on a copy: a developed grid preview showing another photo is replaced by the photo's own development, a
    /// correct one stays untouched, a large one lying on its side is removed, old placeholders stay.
    #[test]
    fn repair_replaces_only_previews_of_another_photo() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let src = crate::env::var_os("GRAINEO_TEST_RAW_DIR").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/signature-edits"));
        let (a, b) = (src.join("05-DSC05627.ARW"), src.join("07-DSC03768.ARW"));
        if !a.is_file() || !b.is_file() {
            eprintln!("skipped: no test RAWs");
            return;
        }
        let small = preview::small();
        let (fa, fb) = (900_001_i64, 900_002_i64);
        let (img_a, img_b) = (standard(&a, small).unwrap(), standard(&b, small).unwrap());
        // the correct one, the wrong one (photo b stored for a would be file a showing b)
        assert!(same_photo(&img_a, &img_a) && !same_photo(&img_b, &img_a), "the two test photos must differ: {}", similarity(&img_b, &img_a));
        preview::write_developed(fa, small, &img_b);
        preview::write_developed(fb, small, &img_b);
        // large level of b lying on its side
        let large = preview::large();
        preview::write_developed(fb, large, &image::imageops::rotate90(&img_b));
        let cache = preview::Cache::open(&std::env::temp_dir().join(format!("graineo-repair-cache-{}", std::process::id())));
        let job = |file: i64, path: &Path| preview::Job { file, path: path.to_path_buf(), format: "ARW".into(), key: file as u64, level: small, orientation: None };
        let jobs = vec![job(fa, &a), job(fb, &b)];
        let grid: Vec<_> = developed_grid().into_iter().filter(|(f, _, _)| *f == fa || *f == fb).collect();
        let since = SINCE.min(super::mtime(&preview::developed_folder()).saturating_sub(60));
        // counting only changes nothing
        let r = run(&cache, &jobs, &grid, since, false);
        assert_eq!((r.checked, r.wrong_small, r.wrong_large, r.wrong_files.clone()), (2, 1, 1, vec![fa, fb]), "{r:?}");
        assert!(preview::has_developed(fb, large));
        let r = run(&cache, &jobs, &grid, since, true);
        assert_eq!((r.wrong_small, r.wrong_large), (1, 1), "{r:?}");
        assert!(same_photo(&preview::read_developed(fa, small).unwrap(), &img_a), "file a shows its own photo again");
        assert!(same_photo(&preview::read_developed(fb, small).unwrap(), &img_b), "file b unchanged");
        assert!(!preview::has_developed(fb, large), "large preview lying on its side removed");
        for f in [fa, fb] {
            preview::remove_developed(f, &[small, large]);
        }
    }
}
