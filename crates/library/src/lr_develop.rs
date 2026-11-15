//! Read an image's Lightroom develop settings from the catalog (read-only,
//! see [`crate::lightroom::open_catalog`]): text from
//! `Adobe_imageDevelopSettings.text` (Lua table, interpreted in `rawdev::lightroom`).
//!
//! Matched via the path in the catalog (root folder + folder + name). If the image is now
//! somewhere else (e.g. moved to the NAS), via the name and the last folder names.

use std::path::Path;

use rusqlite::params;

/// Settings of an image or a virtual copy (`copy` = name of the copy).
#[derive(Clone, Debug)]
pub struct Entry {
    pub copy: Option<String>,
    pub text: String,
    /// path in the catalog
    pub catalog_path: String,
    /// orientation of the image in Lightroom (`AB`, `BC`, `CD`, `DA` …, including manual rotation)
    pub orientation: Option<String>,
}

fn parts(p: &str) -> Vec<String> {
    p.split('/').filter(|t| !t.is_empty()).map(|t| crate::nfc(t).to_lowercase()).collect()
}

/// State of the catalog files (size and modification time of the catalog and its WAL): a cached
/// answer is only valid as long as this stays the same.
fn catalog_state(catalog: &Path) -> Option<[(u64, std::time::SystemTime); 2]> {
    let stamp = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
    let mut wal = catalog.as_os_str().to_owned();
    wal.push("-wal");
    Some([stamp(catalog)?, stamp(Path::new(&wal)).unwrap_or((0, std::time::UNIX_EPOCH))])
}

type CacheEntry = (std::path::PathBuf, std::path::PathBuf, [(u64, std::time::SystemTime); 2], Vec<Entry>);

/// Recent answers: opening an image asks twice (orientation, initial values), preloading and the
/// offline copy ask again – each query scans the file table of the catalog (≈ 30–50 ms).
static CACHE: std::sync::Mutex<Vec<CacheEntry>> = std::sync::Mutex::new(Vec::new());

/// All settings (original and virtual copies) for the file `image`.
pub fn read(catalog: &Path, image: &Path) -> Result<Vec<Entry>, String> {
    let state = catalog_state(catalog);
    if let Some(state) = state {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, _, e)) = cache.iter().find(|(c, b, s, _)| c == catalog && b == image && *s == state) {
            return Ok(e.clone());
        }
    }
    let entries = read_uncached(catalog, image)?;
    if let Some(state) = state {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(c, b, s, _)| !(c == catalog && b == image) && *s == state);
        cache.push((catalog.to_path_buf(), image.to_path_buf(), state, entries.clone()));
        if cache.len() > 64 {
            cache.remove(0);
        }
    }
    Ok(entries)
}

fn read_uncached(catalog: &Path, image: &Path) -> Result<Vec<Entry>, String> {
    let conn = crate::lightroom::open_catalog(catalog).map_err(|e| e.to_string())?;
    let name = image.file_name().and_then(|n| n.to_str()).ok_or("kein Dateiname")?;
    let (root, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let mut st = conn
        .prepare(
            "SELECT r.absolutePath || fo.pathFromRoot || f.baseName || '.' || f.extension, i.copyName, d.text, i.orientation
             FROM AgLibraryFile f JOIN AgLibraryFolder fo ON fo.id_local = f.folder JOIN AgLibraryRootFolder r ON r.id_local = fo.rootFolder
             JOIN Adobe_images i ON i.rootFile = f.id_local LEFT JOIN Adobe_imageDevelopSettings d ON d.image = i.id_local
             WHERE f.baseName = ?1 COLLATE NOCASE AND f.extension = ?2 COLLATE NOCASE",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, Option<String>, Option<String>, Option<String>)> = st
        .query_map(params![root, ext], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    let target = parts(&image.to_string_lossy());
    // best path: exactly equal, else the longest common suffix (at least folder + name)
    let fits = |p: &str| -> usize {
        let t = parts(p);
        t.iter().rev().zip(target.iter().rev()).take_while(|(a, b)| a == b).count()
    };
    let best_match = rows.iter().map(|z| fits(&z.0)).max().unwrap_or(0);
    if best_match < 2 {
        return Ok(Vec::new());
    }
    Ok(rows
        .into_iter()
        .filter(|z| fits(&z.0) == best_match)
        // Lightroom sometimes keeps a second, empty settings row next to the real one (118 photos in the user's catalog):
        // skip it, otherwise the photo opens without its edits.
        .filter_map(|(p, copy, text, orientation)| Some(Entry { copy, text: text.filter(|t| !t.trim().is_empty())?, catalog_path: p, orientation }))
        .collect())
}
