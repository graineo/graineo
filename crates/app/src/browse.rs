//! Paging through images in the Develop module: what is shown right away and when the RAW is read.
//!
//! - The best stored preview appears at once (large preview/offline copy before the last developed view); the RAW
//!   is opened only after a short dwell ([`OPEN_DWELL`]) – paging on cancels it. A Sigma X3F waits for the dwell only
//!   while paging ([`Browse::paging`]); chosen on its own it opens at once. While paging fast ([`Browse::fast`]) no
//!   exact Sigma base starts.
//! - Neighbours are preloaded only after a dwell and never while the selected image is being opened; fast paging
//!   ([`fast`]) pauses it.
//! - Fingerprints: a stored preview remembers which settings (and which revision of the development) it shows. If it
//!   matches the current settings, it stays on screen until a slider is moved or the view is zoomed, so nothing jumps
//!   when the interactive image arrives (that one is still prepared in the background).
//! - After a longer dwell ([`FULL_DWELL`]) the full resolution is prepared in the background, so zooming is sharp at once.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use develop::{Settings, View};
use eframe::egui;

/// Dwell before the RAW of the selected image is opened (s).
/// (0 in tests: their clock stands still while they wait for the image to open)
pub const OPEN_DWELL: f64 = if cfg!(test) { 0.0 } else { 0.25 };
/// Dwell after the image is shown before neighbours are preloaded (s).
pub const PREFETCH_DWELL: f64 = 0.35;
/// Dwell after the image is shown before the full resolution is prepared (s), in the operating mode "Genau" (the
/// mode's plan decides, see `perf_mode::Plan::full_dwell`).
pub const FULL_DWELL: f64 = 1.5;
/// Large previews decoded ahead in the direction of travel.
pub const LARGE_AHEAD: isize = 3;

/// Revision of the development: raise it when the rendering changes visibly, so that stored previews no longer
/// count as current (their fingerprints stop matching).
const REVISION: u64 = 1;

/// State of paging (part of the app).
#[derive(Default)]
pub struct Browse {
    /// image whose RAW is opened once the dwell is over (index, due time)
    pub open_due: Option<(usize, f64)>,
    /// times of the last image changes
    switches: VecDeque<f64>,
    /// full resolution is prepared for this image at this time
    pub full_due: Option<(usize, f64)>,
    /// a stored preview with matching fingerprint stays on screen: (index, texture, fingerprint)
    pub hold: Option<(usize, egui::TextureHandle, u64)>,
    /// image whose large previews ahead were last requested
    pub ahead_for: Option<(usize, isize)>,
    /// last logged view state (index, kind, texture) – for the measurements
    shown: Option<(usize, &'static str, egui::TextureId)>,
    /// image whose fingerprints were last logged (measurements)
    pub expected_logged: Option<usize>,
    /// fingerprint of the entry's settings, cached (index, settings, fingerprint)
    expected: Option<(usize, Settings, bool, u64)>,
}

impl Browse {
    /// Remember an image change.
    pub fn switched(&mut self, now: f64) {
        self.switches.push_back(now);
        while self.switches.len() > 8 {
            self.switches.pop_front();
        }
    }

    /// The image list was rebuilt: indices follow their photos (`map`: old → new index); what lost its photo is dropped.
    pub fn remap(&mut self, map: &dyn Fn(usize) -> Option<usize>) {
        self.open_due = self.open_due.and_then(|(i, t)| map(i).map(|n| (n, t)));
        self.full_due = self.full_due.and_then(|(i, t)| map(i).map(|n| (n, t)));
        self.hold = self.hold.take().and_then(|(i, t, fp)| map(i).map(|n| (n, t, fp)));
        self.ahead_for = None;
        self.shown = None;
        self.expected_logged = None;
        // the cached fingerprint was computed with the path of the photo at the old index
        self.expected = None;
    }

    /// Paging right now: the change before the last one was less than a second ago. A photo chosen on its own (first
    /// click, first key press after looking for a while) is not paging.
    pub fn paging(&self, now: f64) -> bool {
        self.switches.iter().rev().nth(1).is_some_and(|&t| now - t < 1.0)
    }

    /// Fast paging right now: more than two changes within the last second.
    pub fn fast(&self, now: f64) -> bool {
        self.switches.iter().filter(|&&t| now - t < 1.0).count() > 2
    }

    /// Fingerprint the entry's current settings should have (cached while they do not change).
    pub fn expected(&mut self, index: usize, settings: &Settings, stored: bool, path: &Path, copy: Option<&str>) -> u64 {
        if let Some((i, s, st, fp)) = &self.expected {
            if *i == index && *st == stored && s == settings {
                return *fp;
            }
        }
        let fp = expected(settings, stored, path, copy);
        self.expected = Some((index, settings.clone(), stored, fp));
        fp
    }

    /// Log the view state when it changes (with `GRAINEO_LOG`: one line per visible change of the image).
    pub fn log_view(&mut self, index: usize, kind: &'static str, tex: egui::TextureId, since_select: Option<f64>) {
        let now = Some((index, kind, tex));
        if self.shown != now && crate::env::var_os("GRAINEO_LOG").is_some() {
            self.shown = now;
            crate::log(&format!("view {index}: {kind}{}", since_select.map(|t| format!(" after {:.0} ms", t * 1000.0)).unwrap_or_default()));
        }
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn normalized(s: &Settings) -> Settings {
    Settings { view: View::Developed, ..s.clone() }
}

/// Fingerprint of the standard development of an unedited photo (Lightroom initial values or the defaults).
fn start_fp() -> u64 {
    fnv(b"start") ^ REVISION.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Fingerprint of settings (the defaults count as the standard development).
pub fn settings_fp(s: &Settings) -> u64 {
    let n = normalized(s);
    if n == normalized(&Settings::default()) {
        return start_fp();
    }
    let json = serde_json::to_string(&n).unwrap_or_default();
    fnv(json.as_bytes()) ^ REVISION.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Fingerprint the entry shows with these settings: an unedited photo (nothing stored) with the defaults or exactly the
/// initial values applied on opening gets the standard development – the same as its offline copy.
pub fn expected(settings: &Settings, stored: bool, path: &Path, copy: Option<&str>) -> u64 {
    if !stored && (normalized(settings) == normalized(&Settings::default()) || crate::raw_app::start_value(path, copy).is_some_and(|st| normalized(&st) == normalized(settings))) {
        return start_fp();
    }
    settings_fp(settings)
}

/// Fingerprint of the standard development (offline copy of an unedited photo).
pub fn standard() -> u64 {
    start_fp()
}

/// Where the fingerprint of a stored preview lives: the last developed view next to its JPEG
/// ([`crate::developed_cache`]), the large previews in the preview store (both are tidied together with what they belong to).
fn folder(name: &str) -> PathBuf {
    if name.starts_with("developed-") {
        library::data_dir().join("previews").join("developed")
    } else {
        library::cache_folder().join("fingerprints")
    }
}

/// Fingerprints read so far (file name → value); writing updates the entry.
static CACHE: Mutex<Option<HashMap<String, Option<u64>>>> = Mutex::new(None);

fn read(name: String) -> Option<u64> {
    let mut g = CACHE.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(v) = m.get(&name) {
        return *v;
    }
    let v = std::fs::read_to_string(folder(&name).join(&name)).ok().and_then(|t| u64::from_str_radix(t.trim(), 16).ok());
    m.insert(name, v);
    v
}

fn write(name: String, fp: u64) {
    let dir = folder(&name);
    let _ = std::fs::create_dir_all(&dir);
    let target = dir.join(&name);
    let part = target.with_extension("part");
    if std::fs::write(&part, format!("{fp:016x}")).is_ok() && std::fs::rename(&part, &target).is_ok() {
        CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(name, Some(fp));
    }
}

fn developed_name(image: i64) -> String {
    format!("developed-{image}.fp")
}

fn large_name(path: &Path, level: u32) -> String {
    format!("large-{:016x}-{level}.fp", fnv(path.to_string_lossy().as_bytes()))
}

/// Fingerprint of the last developed view of an image ([`crate::developed_cache`]).
pub fn developed(image: i64) -> Option<u64> {
    read(developed_name(image))
}

pub fn set_developed(image: i64, fp: u64) {
    write(developed_name(image), fp);
}

fn forget(name: String) {
    let _ = std::fs::remove_file(folder(&name).join(&name));
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(name, None);
}

/// The last developed view of an image was discarded: its fingerprint goes too.
pub fn forget_developed(image: i64) {
    forget(developed_name(image));
}

/// The large preview of a photo in this level was discarded: its fingerprint goes too.
pub fn forget_large(path: &Path, level: u32) {
    forget(large_name(path, level));
}

/// Fingerprint of the large preview (offline copy) of a photo in this level.
pub fn large(path: &Path, level: u32) -> Option<u64> {
    read(large_name(path, level))
}

pub fn set_large(path: &Path, level: u32, fp: u64) {
    write(large_name(path, level), fp);
}
