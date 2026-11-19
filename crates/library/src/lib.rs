//! graineo's library.
//!
//! - [`db`]: own SQLite database (WAL) under `~/Library/Application Support/graineo/`,
//!   never on the NAS.
//! - [`lightroom`]: import from a Lightroom Classic catalog – the catalog is opened
//!   strictly read-only (`mode=ro`, `query_only`; `immutable=1` only with an empty WAL, see `lightroom.rs`).
//! - [`reconcile`]: database ↔ disk (missing files, unmounted volumes, new
//!   files, graineo sidecars).
//! - [`preview`]: thumbnail cache (JPEG, LRU, size limit, pauses when space is low).
//!
//! The photos themselves are never modified: reading uses `stat`, `read_dir` and read access;
//! writing only goes to the own database and the preview cache.

pub mod reconcile;
pub mod pairs;
pub mod apple_photos;
pub mod sidecar;
pub mod platform;
pub mod watch;
pub mod similar;
pub mod files;
pub mod db;
pub mod manage;
pub mod duplicates;
pub mod series;
pub mod series_quality;
pub mod duplicate_cleanup;
pub mod trash_batch;
pub mod settings;
pub mod label;
pub mod lens_use;
pub mod exif;
pub mod exif_header;
pub mod textmeta;
pub mod people;
pub mod face_index;
pub mod face_frame;
pub mod people_state;
pub mod apple_groups;
#[cfg_attr(not(target_os = "macos"), path = "imageio_fallback.rs")]
pub mod imageio;
pub mod imageio_common;
pub mod export_meta;
pub mod card_import;
pub mod card_watch;
pub mod import_select;
pub mod import_source;
#[cfg(target_os = "macos")]
pub mod camera_mac;
pub mod import_journal;
pub mod ai;
pub mod lightroom;
pub mod lr_develop;
pub mod lr_handover;
pub mod module;
pub mod network_trash;
pub mod not_in_catalog;
pub mod collection_file;
pub mod collection_order;
pub mod semantic;
// SigLIP 2 for the semantic search (stages "Ausgewogen" and "Beste Treffer")
pub mod sentencepiece;
pub mod siglip;
pub mod sky_check;
pub mod search_model;
pub mod xlmr;
pub mod semantic_backfill;
pub mod backup;
pub mod backup_view;
pub mod search;
pub mod search_fields;
pub mod sync;
pub mod keywords;
pub mod keyword_arrange;
pub mod keyword_rules;
pub mod keyword_suggest;
pub mod content_keywords;
pub mod store;
pub mod develop_history;
pub mod mask_raster;
pub mod sidecar_import;
pub mod location;
pub mod places;
pub mod relocate;
pub mod move_folder;
pub mod move_job;
pub mod relink;
pub mod xmp;
pub mod time_correction;
pub mod preview;
pub mod cache_limit;
pub mod video;
pub mod write_target;
pub mod original_export;
pub mod update;
pub mod instance_lock;

use std::path::{Path, PathBuf};

/// `~/Library/Application Support/<APP>` (like the other app files).
/// Name of the app (for folders under ~/Library), see [`sysdirs::APP`].
pub const APP: &str = sysdirs::APP;
/// Subfolder of the app next to the photos (`collections.json`, trash folder on network drives).
pub const SIDECAR_DIR: &str = ".graineo";
/// Own XMP namespace (English field names) and its prefix (since 0.103; the URI only names the namespace).
pub const XMP_NS: &str = "https://graineo.github.io/ns/1.0/";
pub const XMP_PREFIX: &str = "graineo";

/// Program name in XMP (`xmp:CreatorTool`): XMP with it counts as our own.
pub const XMP_TOOL: &str = "graineo";

/// Was this XMP text written by this program?
pub fn is_own_xmp(text: &str) -> bool {
    text.contains(&format!("xmp:CreatorTool=\"{XMP_TOOL}\""))
}

pub fn data_dir() -> PathBuf {
    // test runs: own data folder (never open the real library with a development build)
    if let Some(p) = std::env::var_os("GRAINEO_DATA_DIR") {
        return PathBuf::from(p);
    }
    if let Some(p) = test_binary_dir() {
        return p;
    }
    sysdirs::app_data_dir().unwrap_or_else(std::env::temp_dir).join(APP)
}

/// Safety net for test runs without `GRAINEO_DATA_DIR`: a cargo test binary (`…/deps/…`) gets
/// its own empty folder in the temp directory and never opens the real library, previews or settings.
pub fn test_binary_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let s = exe.to_string_lossy();
    // cargo puts test programs into `<target dir>/<profile>/deps/` (the target dir may be named anything)
    let is_test = s.contains("/deps/") || s.contains("\\deps\\");
    is_test.then(|| std::env::temp_dir().join(format!("graineo-test-{}", std::process::id())))
}

/// One-time switch to English names of files and folders the app created under German names (graineo 1.0 and
/// older): old location → new location, only if the new one is missing; nothing is deleted. The names are listed in
/// [`legacy_names::files`]. Also translates the run reports in `reports/` (file names and keys). Repeatable: a second
/// call finds nothing to do. Returns: what was renamed (one line per entry, reports summed up).
pub fn migrate() -> Vec<String> {
    if std::env::var_os("GRAINEO_LIBRARY_DB").is_some() {
        return Vec::new(); // test run with its own library file: the data folder may be the real one
    }
    migrate_folders(&data_dir(), &cache_base())
}

/// [`migrate`] for the given data and cache folders.
pub fn migrate_folders(data: &Path, cache: &Path) -> Vec<String> {
    let mut v = Vec::new();
    let pairs = legacy_names::files::DATA.iter().map(|(o, n)| (data.join(o), data.join(n))).chain(legacy_names::files::CACHE.iter().map(|(o, n)| (cache.join(o), cache.join(n))));
    for (old, new) in pairs {
        if old.exists() && !new.exists() && std::fs::rename(&old, &new).is_ok() {
            v.push(format!("{} → {}", old.display(), new.display()));
        }
    }
    if settings::migrate_file(&data.join("settings.json")) {
        v.push(format!("{}: keys translated (old file kept as settings.json.before-1.0.1)", data.join("settings.json").display()));
    }
    let n = migrate_reports(&data.join("reports"));
    if n > 0 {
        v.push(format!("{n} reports in {} translated", data.join("reports").display()));
    }
    v
}

/// Run reports written under German names (`<kind>-<time>.json`, `lr-handover-<time>.txt` with JSON at the top):
/// keys and kinds translated, files renamed. Unreadable files are left alone.
fn migrate_reports(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // first JSON value of the file and what follows it (the handover log appends text lines)
        let mut stream = serde_json::Deserializer::from_str(&text).into_iter::<serde_json::Value>();
        let Some(Ok(mut value)) = stream.next() else { continue };
        let rest = &text[stream.byte_offset()..];
        let mut changed = legacy_names::rename_keys(&mut value, legacy_names::sql::REPORT_KEYS);
        if let Some(k) = value.get("kind").and_then(|k| k.as_str()) {
            let new = legacy_names::translate(legacy_names::files::REPORT_KINDS, k);
            if new != k {
                value["kind"] = new.into();
                changed = true;
            }
        }
        let new_name = legacy_names::files::REPORT_KINDS
            .iter()
            .find_map(|(old, new)| name.strip_prefix(&format!("{old}-")).map(|t| format!("{new}-{t}")))
            .unwrap_or_else(|| name.clone());
        if !changed && new_name == name {
            continue;
        }
        let target = dir.join(&new_name);
        if new_name != name && target.exists() {
            continue;
        }
        let body = if name.ends_with(".json") { serde_json::to_string_pretty(&value).unwrap_or_default() } else { format!("{}{rest}", serde_json::to_string_pretty(&value).unwrap_or_default()) };
        let tmp = dir.join(format!(".{new_name}.tmp"));
        if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &target).is_ok() {
            if new_name != name {
                let _ = std::fs::remove_file(&path);
            }
            n += 1;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    n
}

/// Default location of the database (can be overridden with `GRAINEO_LIBRARY_DB`, e.g. for tests).
pub fn db_path() -> PathBuf {
    // viewing a backup: everything works on its copy, never on the real library
    if let Some(v) = backup_view::active() {
        return v.copy.clone();
    }
    real_db_path()
}

/// The real library, also while a backup is viewed (only for restoring and taking over an edit).
pub fn real_db_path() -> PathBuf {
    std::env::var_os("GRAINEO_LIBRARY_DB").map(PathBuf::from).unwrap_or_else(|| data_dir().join("library.sqlite"))
}

/// Name of the preview folder: on macOS a package (the Finder shows it as a single file, the app declares the
/// type `graineopreviews` in its Info.plist), elsewhere a normal folder.
pub const PREVIEW_FOLDER: &str = if cfg!(target_os = "macos") { "graineo Previews.graineopreviews" } else { "previews" };

/// Default location of the preview cache (can be overridden with `GRAINEO_PREVIEW_DIR`).
pub fn cache_folder() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAINEO_PREVIEW_DIR") {
        return PathBuf::from(p);
    }
    cache_base().join(PREVIEW_FOLDER)
}

fn cache_base() -> PathBuf {
    match std::env::var_os("GRAINEO_DATA_DIR").map(PathBuf::from).or_else(test_binary_dir) {
        Some(p) => p,
        None => sysdirs::app_cache_dir(APP).unwrap_or_else(|| std::env::temp_dir().join(APP)),
    }
}

/// Previous location of the preview cache (`previews` next to the package), if it differs from the current one:
/// it is moved into the package once ([`preview::move_into`]).
pub fn legacy_cache_folder() -> Option<PathBuf> {
    if std::env::var_os("GRAINEO_PREVIEW_DIR").is_some() {
        return None;
    }
    let old = cache_base().join("previews");
    (old != cache_folder()).then_some(old)
}

/// The Lightroom catalog chosen for the import (remembered in the library, `None` if none was chosen yet).
/// Only the import and the one-time handover ([`lr_handover`]) read it; nothing else depends on it.
pub fn lightroom_catalog(db: &db::Db) -> Option<PathBuf> {
    db.info("lightroom_catalog").filter(|s| !s.trim().is_empty()).map(PathBuf::from)
}

/// Unicode NFC: Lightroom stores paths composed; depending on the source, macOS returns
/// decomposed umlauts (NFD). Always compare in this form.
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    if s.is_ascii() {
        return s.to_string();
    }
    s.nfc().collect()
}

/// Format identifier from the file extension (upper case, normalized). `None` = not an image/video.
pub fn format_from_extension(name: &str) -> Option<&'static str> {
    let ext = Path::new(name).extension()?.to_str()?;
    extension_format(ext)
}

/// Format identifier of a bare extension ("orf", "ORI", "jpeg" …, any case). `None` = not an image/video.
pub fn extension_format(ext: &str) -> Option<&'static str> {
    EXTENSIONS.iter().find(|(e, _)| e.eq_ignore_ascii_case(ext)).map(|(_, f)| *f)
}

/// Every file extension graineo reads (lower case) with its format: the one list behind [`format_from_extension`]
/// and everything derived from it (import, folder view, sync, editability in the app, the Explorer menu on Windows,
/// the document types of the Mac bundle). RAW formats are those of [`is_raw`]; each of them is read by the RAW
/// decoder (preview, metadata, development). Check a new entry with `scripts/format-check.sh` (docs: formats).
pub const EXTENSIONS: &[(&str, &str)] = &[
    ("x3f", "X3F"),
    ("cr3", "CR3"),
    ("cr2", "CR2"),
    ("crw", "CRW"),
    ("raf", "RAF"),
    ("dng", "DNG"),
    ("arw", "ARW"),
    ("srf", "ARW"),
    ("sr2", "ARW"),
    ("nef", "NEF"),
    ("nrw", "NEF"),
    ("orf", "ORF"),
    // OM System / Olympus high-resolution shot: the single frame next to the composite, an ORF by content
    ("ori", "ORF"),
    ("rw2", "RW2"),
    // Leica compacts built by Panasonic: RW2 by content
    ("rwl", "RW2"),
    // older Panasonic and Leica cameras (FZ50, LX1, Digilux 3 …): RW2 by content
    ("raw", "RW2"),
    ("pef", "PEF"),
    ("srw", "SRW"),
    ("mrw", "MRW"),
    ("erf", "ERF"),
    ("3fr", "3FR"),
    ("fff", "3FR"),
    ("iiq", "IIQ"),
    ("mos", "MOS"),
    ("dcr", "DCR"),
    ("kdc", "KDC"),
    ("mef", "MEF"),
    ("jpg", "JPG"),
    ("jpeg", "JPG"),
    ("jpe", "JPG"),
    ("heic", "HEIC"),
    ("heif", "HEIC"),
    ("hif", "HEIC"),
    ("tif", "TIFF"),
    ("tiff", "TIFF"),
    ("png", "PNG"),
    ("psd", "PSD"),
    ("mov", "VIDEO"),
    ("mp4", "VIDEO"),
    ("m4v", "VIDEO"),
    ("avi", "VIDEO"),
    ("mts", "VIDEO"),
];

/// RAW formats (embedded preview via rawler or x3f).
pub fn is_raw(format: &str) -> bool {
    matches!(format, "X3F" | "CR3" | "CR2" | "CRW" | "RAF" | "DNG" | "ARW" | "NEF" | "ORF" | "RW2" | "PEF" | "SRW" | "MRW" | "ERF" | "3FR" | "IIQ" | "MOS" | "DCR" | "KDC" | "MEF")
}

/// Finished image formats the general path edits (JPEG, HEIC, TIFF, PNG).
pub fn is_finished_image(format: &str) -> bool {
    matches!(format, "JPG" | "HEIC" | "TIFF" | "PNG")
}

/// RAW of the general RAW path (every RAW except Sigma X3F), by extension (any case).
pub fn is_raw_extension(ext: &str) -> bool {
    extension_format(ext).is_some_and(|f| f != "X3F" && is_raw(f))
}

/// Finished image the general path edits, by extension (any case).
pub fn is_finished_image_extension(ext: &str) -> bool {
    extension_format(ext).is_some_and(is_finished_image)
}

/// Unix seconds now.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Free space on the volume of `p` (bytes).
pub fn free(p: &Path) -> Option<u64> {
    let mut q = p.to_path_buf();
    while !q.exists() {
        q = q.parent()?.to_path_buf();
    }
    platform::free_bytes(&q)
}

#[cfg(test)]
mod test_safety {
    /// A cargo test binary is recognised, so `data_dir()` never points at the real library during tests.
    #[test]
    fn test_binary_gets_own_folder() {
        let dir = super::test_binary_dir().expect("test binary not recognised");
        assert!(dir.starts_with(std::env::temp_dir()));
        if std::env::var_os("GRAINEO_DATA_DIR").is_none() {
            assert_eq!(super::data_dir(), dir);
        }
    }
}

#[cfg(test)]
mod format_tests {
    use super::{format_from_extension, is_raw};

    /// Nikon RAWs: NEF and NRW (Coolpix) in any case are one format, a RAW.
    #[test]
    fn nikon_extensions_are_raw() {
        for name in ["DSC_0001.NEF", "dsc_0001.nef", "_DSC0070.Nef", "DSCN2039.NRW", "dscn2039.nrw"] {
            let f = format_from_extension(name);
            assert_eq!(f, Some("NEF"), "{name}");
            assert!(is_raw(f.unwrap()), "{name}");
        }
        assert_eq!(format_from_extension("DSC_0001.NEF.xmp"), None);
    }

    /// Every RAW extension the decoder reads is in the list and counts as RAW of the general path (X3F excepted),
    /// the finished formats are not RAW, and the list has no duplicates.
    #[test]
    fn one_list_for_all_formats() {
        use super::{extension_format, is_finished_image_extension, is_raw_extension, EXTENSIONS};
        for e in ["CR2", "cr3", "CRW", "raf", "DNG", "arw", "SRF", "sr2", "NEF", "nrw", "ORF", "ori", "RW2", "rwl", "raw", "PEF", "srw", "MRW", "erf", "3FR", "fff", "IIQ", "mos", "DCR", "kdc", "MEF"] {
            assert!(is_raw_extension(e), "{e}");
            assert!(!is_finished_image_extension(e), "{e}");
        }
        assert!(!is_raw_extension("x3f") && is_raw(extension_format("X3F").unwrap()));
        for e in ["jpg", "JPEG", "jpe", "heic", "HIF", "tif", "TIFF", "png"] {
            assert!(is_finished_image_extension(e) && !is_raw_extension(e), "{e}");
        }
        assert!(!is_raw_extension("psd") && !is_finished_image_extension("mov") && extension_format("xmp").is_none());
        let mut seen = std::collections::HashSet::new();
        assert!(EXTENSIONS.iter().all(|(e, _)| e.chars().all(|c| !c.is_ascii_uppercase()) && seen.insert(*e)));
    }
}
