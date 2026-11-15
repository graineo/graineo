//! Offline copies (large level, photos on other drives) show the development: a camera JPEG stored as a stand-in is
//! replaced by the development in the chosen level (first the new one, then the old one goes), a failed development
//! leaves the stand-in alone, JPEG originals are scaled from the original (never larger), the minimum free space holds,
//! and the jobs "to do soon" (after an import) run even with the background switched off. The developer stands in for
//! the app's development (it only returns a plain image). Everything in own temporary folders.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::RgbImage;
use library::preview::{self, Cache, Generator, Job};

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn image(w: u32, h: u32, shade: u8) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| image::Rgb([shade, (x % 256) as u8, (y % 256) as u8]))
}

fn job(file: i64, path: &Path, format: &str, key: u64, level: u32) -> Job {
    Job { file, path: path.to_path_buf(), format: format.into(), key, level, orientation: None }
}

/// Upper bound for waiting on the workers. They run at background priority (efficiency cores, throttled) and a debug
/// build decodes and scales slowly; on a machine busy with builds the first jobs took more than 30 s (in release 16 s
/// under load). The bound only catches a hang; it does not measure speed.
const PATIENCE: Duration = Duration::from_secs(240);

fn wait_for(what: &str, ok: &dyn Fn() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(t.elapsed() < PATIENCE, "timed out after {} s: {what}", PATIENCE.as_secs());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The "development" the developer returns, made once. Release (the usual test run of this crate): 3000 px, so the
/// worker has to scale it to the 2560 px level. Debug: exactly 2560 px – scaling 3000 → 2560 px takes the unoptimized
/// image crate minutes on a background-priority worker; the check "development in the chosen level, not the 1616 px
/// camera JPEG" stays the same.
fn developed_image() -> RgbImage {
    static IMG: std::sync::OnceLock<RgbImage> = std::sync::OnceLock::new();
    let (w, h) = if cfg!(debug_assertions) { (2560, 1707) } else { (3000, 2000) };
    IMG.get_or_init(|| image(w, h, 120)).clone()
}

fn long_edge(c: &Cache, key: u64, level: u32) -> u32 {
    let img = c.read(key, level).expect("preview present");
    img.width().max(img.height())
}

#[test]
fn offline_copies_show_the_development() {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir: PathBuf = std::env::temp_dir().join(format!("graineo-offline-{}-{n}", std::process::id()));
    let photos = dir.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    std::env::set_var("GRAINEO_DATA_DIR", dir.join("data"));
    std::env::set_var("GRAINEO_PREVIEW_MIN_FREE_GB", "0");
    let (small, large) = (preview::small(), preview::large());
    assert_eq!((small, large), (512, 2560), "default levels");

    preview::set_developer(Box::new(|p: &Path, _level: u32| {
        CALLS.fetch_add(1, Ordering::SeqCst);
        if p.to_string_lossy().contains("fail") {
            return Err("cannot develop".into());
        }
        if !p.is_file() {
            return Err("not reachable".into());
        }
        Ok(developed_image())
    }));
    let _ = developed_image();
    assert!(preview::develops("DNG") && preview::develops("X3F") && !preview::develops("JPG") && !preview::develops("HEIC"));

    let raw = photos.join("A.DNG");
    let fail = photos.join("fail.DNG");
    let fresh = photos.join("E.DNG");
    for p in [&raw, &fail, &fresh] {
        std::fs::write(p, b"raw bytes").unwrap();
    }
    let jpg = photos.join("C.jpg");
    image(1000, 667, 60).save_with_format(&jpg, image::ImageFormat::Jpeg).unwrap();
    let (a, b, c, e) = (1u64 << 56, 2u64 << 56, 3u64 << 56, 5u64 << 56);

    let mut cache = Cache::open(&dir.join("data/previews"));
    cache.min_free = 0;
    // stand-ins (camera JPEG, 1616 px) of a and b, as stored before or by the single image
    assert!(cache.write(a, large, &image(1616, 1080, 30)));
    assert!(cache.write(b, large, &image(1616, 1080, 30)));
    assert!(!cache.is_developed(a, large));
    let stand_in_b = std::fs::read(cache.path(b, large)).unwrap();
    cache.set_protection([a, b, c, e].into_iter().collect(), Default::default());
    let cache = Arc::new(cache);
    let g = Generator::new(cache.clone(), 2, Box::new(|_| {}));

    // jobs to do soon (after an import) run with the background switched off
    g.soon(vec![job(5, &fresh, "DNG", e, small), job(5, &fresh, "DNG", e, large)]);
    wait_for("soon", &|| cache.is_developed(e, large) && cache.has(e, small));
    assert_eq!(long_edge(&cache, e, large), 2560);

    // background: a is replaced by the development, b keeps its stand-in, c (JPEG) comes from the original
    let back = vec![job(1, &raw, "DNG", a, large), job(2, &fail, "DNG", b, large), job(3, &jpg, "JPG", c, large)];
    let calls_before = CALLS.load(Ordering::SeqCst);
    g.background(back.clone());
    g.background_on(true);
    wait_for("background", &|| cache.is_developed(a, large) && cache.has(c, large) && g.open_set() == 0 && CALLS.load(Ordering::SeqCst) >= calls_before + 2);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(long_edge(&cache, a, large), 2560, "development in the chosen level, not the 1616 px camera JPEG");
    assert_eq!(std::fs::read(cache.path(b, large)).unwrap(), stand_in_b, "failed development: the stand-in stays untouched");
    assert!(!cache.is_developed(b, large));
    assert_eq!(long_edge(&cache, c, large), 1000, "JPEG: from the original, never larger than it");
    assert!(!cache.is_developed(c, large));
    assert_eq!(CALLS.load(Ordering::SeqCst), calls_before + 2, "JPEG is not developed");

    // the same list again: nothing is developed twice, the failed one is not retried in this run
    g.background(back);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(CALLS.load(Ordering::SeqCst), calls_before + 2);

    // the mark survives renaming the key (move, relink) and deriving another level when tidying
    assert_eq!(cache.rekey(a, 9u64 << 56), 2);
    assert!(cache.is_developed(9u64 << 56, large));
    assert!(cache.write_marked(7u64 << 56, 4000, &image(4000, 2667, 90)));
    cache.set_protection([7u64 << 56].into_iter().collect(), Default::default());
    cache.tidy_levels(small, large, &|| true);
    assert_eq!(cache.levels_of(7u64 << 56), vec![512, 2560]);
    assert!(cache.is_developed(7u64 << 56, large));

    // below the minimum free space nothing is developed (and nothing written)
    let mut full = Cache::open(&dir.join("data/previews-full"));
    full.min_free = u64::MAX;
    full.set_protection([a].into_iter().collect(), Default::default());
    let full = Arc::new(full);
    let g2 = Generator::new(full.clone(), 1, Box::new(|_| {}));
    let calls_before = CALLS.load(Ordering::SeqCst);
    g2.soon(vec![job(1, &raw, "DNG", a, large)]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(CALLS.load(Ordering::SeqCst), calls_before, "no development without space");
    assert!(!full.has(a, large) && full.paused());

    let _ = std::fs::remove_dir_all(&dir);
}
