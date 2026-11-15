//! Upper limit of the app cache (`~/Library/Caches/graineo`): which files may be trimmed, in which order, and the
//! hard guard that keeps trimming inside the cache folder.
//!
//! What counts and what is touched (only with a limit chosen, setting `previewCacheLimitGB`; default: unlimited):
//! - **Trimmed** (can be made again from the originals at any time, nothing else is lost): the stored bases of the
//!   Sigma development (`spp`, tier 0, oldest first), then the small previews and left-over large previews of photos
//!   on this Mac (tier 1, least recently used first; the most recently used ones stay).
//! - **Never trimmed:** offline copies and every preview of photos on other drives or not reachable (without
//!   them the photo would have no view once the drive is gone), developed previews (they show the development;
//!   without them the grid would show the camera preview until the photo is developed again), fingerprints,
//!   faces, film looks, everything else. These still count towards the total.
//! - Nothing outside the cache folder: every file is checked against the canonical cache root before it is
//!   removed ([`Roots::allows`]); links are never followed and never removed.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Setting key (GB, `0` or absent = unlimited).
pub const SETTING: &str = "previewCacheLimitGB";
/// Choices in the settings (`0` = unlimited).
pub const CHOICES_GB: [u64; 5] = [0, 10, 20, 50, 100];
/// Trimming goes down to this share of the limit.
pub const TARGET_PERCENT: u64 = 90;
/// The most recently used entries of the preview store are never trimmed.
pub const KEEP_RECENT: usize = 2000;
/// Stored bases younger than this are never trimmed (the photo may be open).
pub const KEEP_YOUNG_SECS: u64 = 2 * 3600;

/// Chosen limit in bytes (decimal GB, as the Finder counts); `None` = unlimited or an unknown value.
pub fn limit_bytes() -> Option<u64> {
    let gb = crate::settings::value(SETTING).as_u64()?;
    CHOICES_GB.contains(&gb).then_some(gb).filter(|g| *g > 0).map(|g| g * crate::preview::GB)
}

/// Choose the limit in GB (`0` = unlimited).
pub fn set_limit_gb(gb: u64) {
    if CHOICES_GB.contains(&gb) {
        crate::settings::set(SETTING, serde_json::json!(gb));
    }
}

/// Kind of a candidate (also the order: lower tier first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// stored base of the Sigma development
    Base,
    /// preview of the store (key, level)
    Preview { key: u64, level: u32 },
}

impl Kind {
    fn tier(self) -> u8 {
        match self {
            Kind::Base => 0,
            Kind::Preview { .. } => 1,
        }
    }
}

/// A file that may be trimmed. `age`: smaller = older (time in seconds or an LRU counter, within one tier).
#[derive(Clone, Debug)]
pub struct Item {
    pub path: PathBuf,
    pub bytes: u64,
    pub age: u64,
    pub kind: Kind,
}

/// Canonical cache roots: the only places where something may be removed.
#[derive(Clone, Debug)]
pub struct Roots(Vec<PathBuf>);

impl Roots {
    /// Takes the folders that exist; refuses (skips) anything that is not a deep folder (`/`, the home folder, a
    /// volume root …), so a wrong setting can never point trimming at something large.
    pub fn new(folders: &[&Path]) -> Roots {
        let home = sysdirs::home().and_then(|h| std::fs::canonicalize(h).ok());
        let v = folders
            .iter()
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .filter(|c| c.is_dir() && c.components().count() >= 4 && Some(c) != home.as_ref())
            .collect();
        Roots(v)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// May this file be removed? Absolute, no `..`, a regular file (not a link), and its canonical path (links in
    /// the folders above resolved) lies strictly inside one of the roots.
    pub fn allows(&self, path: &Path) -> bool {
        use std::path::Component;
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
            return false;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
        if !meta.file_type().is_file() {
            return false;
        }
        let Ok(canon) = std::fs::canonicalize(path) else { return false };
        self.0.iter().any(|r| canon.starts_with(r) && canon != *r)
    }
}

/// Files of a flat folder with this extension, as bases (age = modification time in seconds); files younger than
/// [`KEEP_YOUNG_SECS`] are left out. Links are skipped.
pub fn scan_bases(dir: &Path, extension: &str, now: SystemTime) -> Vec<Item> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v = Vec::new();
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if !ft.is_file() || p.extension().and_then(|x| x.to_str()) != Some(extension) {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        let t = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(t).map_or(true, |d| d.as_secs() < KEEP_YOUNG_SECS) {
            continue;
        }
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        v.push(Item { path: p, bytes: m.len(), age: secs, kind: Kind::Base });
    }
    v
}

/// Candidates of the preview store from its inventory (`entries`: key, level, bytes, LRU counter; larger = more
/// recent): the [`KEEP_RECENT`] most recent ones and everything protected (`protected(key)`) are left out. `path`
/// gives the file of an entry. Without known protection nothing is a candidate.
pub fn preview_items(entries: &[(u64, u32, u64, u64)], protection_known: bool, protected: &dyn Fn(u64) -> bool, path: &dyn Fn(u64, u32) -> PathBuf) -> Vec<Item> {
    if !protection_known {
        return Vec::new();
    }
    let mut sorted: Vec<&(u64, u32, u64, u64)> = entries.iter().collect();
    sorted.sort_by_key(|e| e.3);
    let older = sorted.len().saturating_sub(KEEP_RECENT);
    sorted
        .into_iter()
        .take(older)
        .filter(|e| !protected(e.0))
        .map(|e| Item { path: path(e.0, e.1), bytes: e.2, age: e.3, kind: Kind::Preview { key: e.0, level: e.1 } })
        .collect()
}

/// What to remove.
#[derive(Debug, Default)]
pub struct Plan {
    pub items: Vec<Item>,
    pub bytes: u64,
    /// the target is reached with these (otherwise the rest is protected or recent)
    pub reaches_target: bool,
}

/// Plan: with `total` bytes in the cache and a `limit`, remove candidates (bases first, then previews, the oldest
/// first) until `total` is at [`TARGET_PERCENT`] of the limit. Nothing if `total` is not above `trigger_percent`
/// of the limit (automatic: 100, by hand: [`TARGET_PERCENT`]).
pub fn plan(mut candidates: Vec<Item>, total: u64, limit: u64, trigger_percent: u64) -> Plan {
    if limit == 0 || total <= limit / 100 * trigger_percent {
        return Plan { reaches_target: true, ..Plan::default() };
    }
    let target = limit / 100 * TARGET_PERCENT;
    let need = total.saturating_sub(target);
    candidates.sort_by_key(|c| (c.kind.tier(), c.age));
    let mut p = Plan::default();
    for c in candidates {
        if p.bytes >= need {
            break;
        }
        p.bytes += c.bytes;
        p.items.push(c);
    }
    p.reaches_target = p.bytes >= need;
    p
}

/// Outcome of [`execute`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub removed: usize,
    pub bytes: u64,
    pub refused: usize,
    pub stopped: bool,
}

/// Remove the planned files. Each one is checked by [`Roots::allows`]; `remove` does the work (default
/// `std::fs::remove_file`; the preview store passes a function that also updates its inventory). `go_on` is asked
/// before every file (`false` stops, e.g. when the Mac is busy again); `progress` gets the bytes freed so far.
pub fn execute(roots: &Roots, plan: &Plan, remove: &dyn Fn(&Item) -> bool, go_on: &dyn Fn() -> bool, progress: &dyn Fn(u64)) -> Outcome {
    let mut o = Outcome::default();
    for it in &plan.items {
        if !go_on() {
            o.stopped = true;
            break;
        }
        if !roots.allows(&it.path) {
            o.refused += 1;
            continue;
        }
        if remove(it) {
            o.removed += 1;
            o.bytes += it.bytes;
            progress(o.bytes);
        }
    }
    o
}

/// Size of a folder (files only, links not followed; 0 if missing).
pub fn folder_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                total += e.metadata().map_or(0, |m| m.len());
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(name: &str) -> PathBuf {
        let d = PathBuf::from("/private/tmp").join(format!("graineo-cachelimit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    fn file(p: &Path, bytes: usize, age_secs: u64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; bytes]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    fn item(p: &Path, bytes: u64, age: u64, kind: Kind) -> Item {
        Item { path: p.to_path_buf(), bytes, age, kind }
    }

    #[test]
    fn oldest_first_down_to_ninety_percent_and_young_ones_stay() {
        let root = tmp("alt");
        let spp = root.join("spp");
        file(&spp.join("a.sppc"), 100, 10 * 86400);
        file(&spp.join("b.sppc"), 100, 5 * 86400);
        file(&spp.join("c.sppc"), 100, 3 * 86400);
        file(&spp.join("d.sppc"), 100, 60); // young: the photo may be open
        file(&spp.join("e.tmp"), 100, 10 * 86400); // not a base
        let items = scan_bases(&spp, "sppc", SystemTime::now());
        assert_eq!(items.len(), 3);
        // limit 300, total 400: target 270 -> need 130 -> two files (the two oldest)
        let p = plan(items, 400, 300, 100);
        assert_eq!(p.items.iter().map(|i| i.path.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), ["a.sppc", "b.sppc"]);
        assert!(p.reaches_target);
        let roots = Roots::new(&[&root]);
        let o = execute(&roots, &p, &|i| std::fs::remove_file(&i.path).is_ok(), &|| true, &|_| {});
        assert_eq!((o.removed, o.bytes, o.refused), (2, 200, 0));
        assert!(!spp.join("a.sppc").exists() && !spp.join("b.sppc").exists());
        assert!(spp.join("c.sppc").exists() && spp.join("d.sppc").exists() && spp.join("e.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bases_go_before_previews_and_unlimited_or_below_limit_removes_nothing() {
        let c = vec![
            item(Path::new("/x/p1"), 50, 1, Kind::Preview { key: 1, level: 512 }),
            item(Path::new("/x/b1"), 50, 999, Kind::Base),
        ];
        let p = plan(c.clone(), 1000, 800, 100);
        assert_eq!(p.items[0].kind, Kind::Base);
        // unlimited (0) never removes
        assert!(plan(c.clone(), u64::MAX / 2, 0, 100).items.is_empty());
        // below the limit: nothing; between 90 % and 100 %: only by hand
        assert!(plan(c.clone(), 700, 800, 100).items.is_empty());
        assert!(plan(c.clone(), 750, 800, 100).items.is_empty());
        assert!(!plan(c.clone(), 790, 800, TARGET_PERCENT).items.is_empty());
        // target not reachable: everything offered goes, and it says so
        let p = plan(c, 10_000, 800, 100);
        assert_eq!(p.items.len(), 2);
        assert!(!p.reaches_target);
    }

    #[test]
    fn previews_protected_and_recent_stay() {
        // 2005 entries: keys 0..2005, LRU counter = key; 5 old ones, of which key 2 is protected
        let entries: Vec<(u64, u32, u64, u64)> = (0..(KEEP_RECENT as u64 + 5)).map(|k| (k, 512, 10, k)).collect();
        let items = preview_items(&entries, true, &|k| k == 2, &|k, l| PathBuf::from(format!("/x/{k}-{l}")));
        assert_eq!(items.iter().map(|i| i.age).collect::<Vec<_>>().len(), 4);
        assert!(items.iter().all(|i| i.age != 2 && i.age < 5));
        // protection unknown: nothing
        assert!(preview_items(&entries, false, &|_| false, &|k, l| PathBuf::from(format!("/x/{k}-{l}"))).is_empty());
    }

    #[test]
    fn files_outside_the_roots_and_links_are_refused() {
        let root = tmp("wurzel");
        let outside = tmp("draussen");
        let cache = root.join("cache");
        file(&cache.join("in.sppc"), 10, 86400 * 9);
        file(&outside.join("secret.jpg"), 10, 86400 * 9);
        // symlink to a file outside, symlink to a folder outside, relative path with ..
        std::os::unix::fs::symlink(outside.join("secret.jpg"), cache.join("link.sppc")).unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("ordner")).unwrap();
        let roots = Roots::new(&[&cache]);
        assert!(roots.allows(&cache.join("in.sppc")));
        assert!(!roots.allows(&outside.join("secret.jpg")));
        assert!(!roots.allows(&cache.join("link.sppc")));
        assert!(!roots.allows(&cache.join("ordner/secret.jpg")));
        assert!(!roots.allows(&cache.join("../draussen/secret.jpg")));
        assert!(!roots.allows(&cache));
        assert!(!roots.allows(Path::new("relativ/pfad.jpg")));
        // a plan that contains such paths removes nothing of them
        let p = Plan {
            items: vec![
                item(&outside.join("secret.jpg"), 10, 1, Kind::Base),
                item(&cache.join("link.sppc"), 10, 2, Kind::Base),
                item(&cache.join("ordner/secret.jpg"), 10, 3, Kind::Base),
                item(&cache.join("in.sppc"), 10, 4, Kind::Base),
            ],
            bytes: 40,
            reaches_target: true,
        };
        let o = execute(&roots, &p, &|i| std::fs::remove_file(&i.path).is_ok(), &|| true, &|_| {});
        assert_eq!((o.removed, o.refused), (1, 3));
        assert!(outside.join("secret.jpg").exists());
        assert!(std::fs::symlink_metadata(cache.join("link.sppc")).is_ok());
        assert!(!cache.join("in.sppc").exists());
        let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&outside));
    }

    #[test]
    fn shallow_or_missing_roots_are_not_accepted() {
        assert!(Roots::new(&[Path::new("/")]).is_empty());
        assert!(Roots::new(&[Path::new("/private")]).is_empty());
        assert!(Roots::new(&[Path::new("/private/tmp/gibt-es-nicht-graineo")]).is_empty());
        if let Some(h) = sysdirs::home() {
            assert!(Roots::new(&[&h]).is_empty());
        }
    }

    #[test]
    fn stopping_and_progress() {
        let root = tmp("stop");
        let f: Vec<Item> = (0..4).map(|i| {
            let p = root.join(format!("cache/{i}.sppc"));
            file(&p, 10, 86400 * 9);
            item(&p, 10, i, Kind::Base)
        }).collect();
        let roots = Roots::new(&[&root.join("cache")]);
        let n = std::cell::Cell::new(0);
        let last = std::cell::Cell::new(0);
        let o = execute(&roots, &Plan { items: f, bytes: 40, reaches_target: true }, &|i| std::fs::remove_file(&i.path).is_ok(), &|| { n.set(n.get() + 1); n.get() <= 2 }, &|b| last.set(b));
        assert_eq!((o.removed, o.stopped, last.get()), (2, true, 20));
        assert_eq!(folder_bytes(&root.join("cache")), 20);
        let _ = std::fs::remove_dir_all(&root);
    }
}
