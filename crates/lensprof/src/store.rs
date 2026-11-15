//! Storage of the own profiles: `<root>/own/<id>.json` (format 1 or 2), snapshots in
//! `<root>/own/history/<id>/<version>.json`.
//!
//! The root follows the test isolation of the rest of the app: `GRAINEO_LENS_BACKUP` (exact folder,
//! empty = off) → `GRAINEO_DATA_DIR/lens-profiles` → in a cargo test binary its own temp folder
//! (like `library::test_binary_dir`) → `<app data>/graineo/lens-profiles`. Test runs therefore never write
//! into the real data folder.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::names::{first_word, model_matches, norm};
use crate::own::{OwnProfile, ProfileRef, Revision, RevisionAction, ShotRef, FORMAT};

static GENERATION: AtomicU64 = AtomicU64::new(0);

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Safety net as in `library::test_binary_dir`: a cargo test program (`…/deps/…`) gets its own folder.
fn test_binary_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let s = exe.to_string_lossy();
    (s.contains("/deps/") || s.contains("\\deps\\")).then(|| std::env::temp_dir().join(format!("graineo-test-{}", std::process::id())))
}

/// Folder `lens-profiles` (own profiles, private LCP backups, assignment rules).
pub fn root() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("GRAINEO_LENS_BACKUP") {
        return (!v.is_empty()).then(|| PathBuf::from(v));
    }
    if let Some(d) = env_path("GRAINEO_DATA_DIR") {
        return Some(d.join("lens-profiles"));
    }
    if let Some(d) = test_binary_dir() {
        return Some(d.join("lens-profiles"));
    }
    sysdirs::app_data_dir().map(|d| d.join(sysdirs::APP).join("lens-profiles"))
}

/// Folder of the own profiles (`<root>/own`).
pub fn own_dir() -> Option<PathBuf> {
    root().map(|d| d.join("own"))
}

/// Usable as file stem: letters, digits, `.`, `-`, `_`, space; not empty, no leading dot, not `history`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id != "history"
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
}

pub fn path_of(id: &str) -> Option<PathBuf> {
    valid_id(id).then_some(())?;
    Some(own_dir()?.join(format!("{id}.json")))
}

/// Rises with every write of this process (caches of callers compare it).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

fn bump() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Tell the caches that something changed (other files of the folder, e.g. rules).
pub fn bump_generation() {
    bump();
}

struct ListCache {
    dir: PathBuf,
    mtime: Option<SystemTime>,
    generation: u64,
    list: Vec<Arc<OwnProfile>>,
}

static LIST: Mutex<Option<ListCache>> = Mutex::new(None);

fn read_all(dir: &Path) -> Vec<Arc<OwnProfile>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("json")).collect())
        .unwrap_or_default();
    files.sort();
    files
        .iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?;
            if !valid_id(stem) {
                return None;
            }
            let t = std::fs::read_to_string(p).ok()?;
            OwnProfile::from_json(&t, stem).ok().map(Arc::new)
        })
        .collect()
}

/// All own profiles (current versions), sorted by file name. Cached; read again when the folder changes
/// (modification time) or after a write of this process.
pub fn list() -> Vec<Arc<OwnProfile>> {
    let Some(dir) = own_dir() else { return Vec::new() };
    let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
    let generation = generation();
    if let Ok(c) = LIST.lock() {
        if let Some(c) = c.as_ref().filter(|c| c.dir == dir && c.mtime == mtime && c.generation == generation) {
            return c.list.clone();
        }
    }
    // read without holding the lock
    let list = read_all(&dir);
    if let Ok(mut c) = LIST.lock() {
        *c = Some(ListCache { dir, mtime, generation, list: list.clone() });
    }
    list
}

type HistKey = (PathBuf, String, u32);
static HISTORY: Mutex<Option<std::collections::HashMap<HistKey, Arc<OwnProfile>>>> = Mutex::new(None);

/// One version of a profile: the current file if its version matches, otherwise the snapshot from the history.
/// `None`: profile or version not there (deleted, other machine).
pub fn load(r: &ProfileRef) -> Option<Arc<OwnProfile>> {
    if !valid_id(&r.id) {
        return None;
    }
    if let Some(p) = list().into_iter().find(|p| p.id == r.id && p.version == r.version) {
        return Some(p);
    }
    // snapshots never change: cache them
    let key = (own_dir()?, r.id.clone(), r.version);
    if let Some(p) = HISTORY.lock().ok().and_then(|h| h.as_ref().and_then(|m| m.get(&key).cloned())) {
        return Some(p);
    }
    let p = Arc::new(crate::history::read(&r.id, r.version)?);
    if let Ok(mut h) = HISTORY.lock() {
        h.get_or_insert_with(Default::default).insert(key, p.clone());
    }
    Some(p)
}

/// Automatic choice as up to 0.94 (`lcp::load_custom`): same maker (first word), same camera model and the
/// same lens name (`None`/"" only matches profiles without a lens name); profiles without entries do not count.
pub fn find_auto(maker: &str, model: &str, lens: Option<&str>) -> Option<Arc<OwnProfile>> {
    let mk = first_word(maker);
    let obj = lens.map(norm).unwrap_or_default();
    list().into_iter().find(|s| first_word(&s.maker) == mk && model_matches(&s.model, model) && norm(&s.lens) == obj && !s.profile.entries.is_empty())
}

pub(crate) fn write_atomic<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Remove draft revisions (`version == 0`) and return shots and note of the last one.
fn take_draft(history: &mut Vec<Revision>) -> (Vec<ShotRef>, String) {
    let draft = history.iter().rev().find(|r| r.version == 0).map(|r| (r.shots.clone(), r.note.clone()));
    history.retain(|r| r.version != 0);
    draft.unwrap_or_default()
}

/// Save as a new profile: new id (12 hex digits), version 1, revision "measured".
pub fn save_new(p: OwnProfile) -> std::io::Result<ProfileRef> {
    save_new_as(p, RevisionAction::Measured)
}

/// Like [`save_new`] with another first revision (import, duplicate).
pub fn save_new_as(mut p: OwnProfile, action: RevisionAction) -> std::io::Result<ProfileRef> {
    let dir = own_dir().ok_or_else(|| std::io::Error::other("kein Benutzerordner"))?;
    std::fs::create_dir_all(&dir)?;
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let id = (0u128..)
        .map(|i| format!("{:012x}", fnv(&format!("{}|{}|{}|{}", p.maker, p.model, p.lens, t + i)) & 0xffff_ffff_ffff))
        .find(|id| !dir.join(format!("{id}.json")).exists() && !dir.join("history").join(id).exists())
        .expect("free id");
    let (shots, note) = take_draft(&mut p.history);
    p.format = FORMAT;
    p.id = id;
    p.version = 1;
    if p.name.trim().is_empty() {
        p.name = p.profile.name.clone();
    }
    p.history = vec![Revision { version: 1, time: now_secs(), action, shots, note }];
    write_atomic(&dir.join(format!("{}.json", p.id)), &p)?;
    bump();
    Ok(p.reference())
}

/// Save `p` as the next version of the existing profile `p.id`: the current file is kept as snapshot
/// (`history/<id>/<version>.json`), the new file gets version + 1 and the history of the current file plus
/// one revision (a format-1 file first gets the revision "migrated" for its version 1).
pub fn save_version(mut p: OwnProfile, action: RevisionAction) -> std::io::Result<ProfileRef> {
    let path = path_of(&p.id).ok_or_else(|| std::io::Error::other("kein gültiger Profilname"))?;
    let text = std::fs::read_to_string(&path)?;
    let current = OwnProfile::from_json(&text, &p.id).map_err(std::io::Error::other)?;
    crate::history::write(&current)?;
    let (shots, note) = take_draft(&mut p.history);
    let mut history = current.history.clone();
    if history.is_empty() {
        let time = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
        history.push(Revision { version: current.version, time, action: RevisionAction::Migrated, shots: Vec::new(), note: String::new() });
    }
    p.format = FORMAT;
    p.id = current.id.clone();
    p.version = current.version + 1;
    if p.name.trim().is_empty() {
        p.name = p.profile.name.clone();
    }
    history.push(Revision { version: p.version, time: now_secs(), action, shots, note });
    p.history = history;
    write_atomic(&path, &p)?;
    bump();
    Ok(p.reference())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::own::tests::v1;

    /// Tests change environment variables: one at a time.
    pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
        static L: Mutex<()> = Mutex::new(());
        L.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Fresh folder as `GRAINEO_LENS_BACKUP`.
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lensprof-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("GRAINEO_LENS_BACKUP", &d);
        d
    }

    fn v1_file(root: &Path, stem: &str) {
        std::fs::create_dir_all(root.join("own")).unwrap();
        std::fs::write(root.join("own").join(format!("{stem}.json")), v1()).unwrap();
    }

    #[test]
    fn save_version_snapshots_previous() {
        let _l = lock();
        let root = scratch("version");
        let p = OwnProfile::from_json(&v1(), "").unwrap();
        let r1 = save_new(p).unwrap();
        assert_eq!(r1.version, 1);
        assert_eq!(r1.id.len(), 12);
        let mut p2 = (*load(&r1).unwrap()).clone();
        p2.name = "geändert".into();
        let r2 = save_version(p2, RevisionAction::Extended).unwrap();
        assert_eq!(r2, ProfileRef { id: r1.id.clone(), version: 2 });
        assert!(root.join("own/history").join(&r1.id).join("1.json").exists());
        let cur = load(&r2).unwrap();
        assert_eq!(cur.version, 2);
        assert_eq!(cur.history.len(), 2);
        assert_eq!(cur.history[0].action, RevisionAction::Measured);
        assert_eq!(cur.history[1].action, RevisionAction::Extended);
        assert_eq!(crate::history::versions(&r1.id), vec![1]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pinned_old_version_loads_from_history() {
        let _l = lock();
        let root = scratch("pinned");
        let r1 = save_new(OwnProfile::from_json(&v1(), "").unwrap()).unwrap();
        let mut p2 = (*load(&r1).unwrap()).clone();
        p2.name = "neu".into();
        p2.profile.entries.clear();
        save_version(p2, RevisionAction::Extended).unwrap();
        let old = load(&ProfileRef::parse(&format!("{}@1", r1.id)).unwrap()).expect("v1 from the history");
        assert_eq!(old.version, 1);
        assert_ne!(old.name, "neu");
        assert_eq!(old.profile.entries.len(), 1);
        assert!(load(&ProfileRef { id: r1.id.clone(), version: 9 }).is_none());
        assert!(load(&ProfileRef { id: "dead".into(), version: 1 }).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn migrated_v1_gets_history_on_first_version() {
        let _l = lock();
        let root = scratch("migrate");
        v1_file(&root, "Canon_400D_EF-S10-22");
        let cur = list().into_iter().next().unwrap();
        assert_eq!(cur.reference(), ProfileRef { id: "Canon_400D_EF-S10-22".into(), version: 1 });
        let r = save_version((*cur).clone(), RevisionAction::Measured).unwrap();
        assert_eq!(r.version, 2);
        let p = load(&r).unwrap();
        assert_eq!(p.history.iter().map(|h| (h.version, h.action.clone())).collect::<Vec<_>>(), vec![(1, RevisionAction::Migrated), (2, RevisionAction::Measured)]);
        // the old file stays readable as version 1, the snapshot holds it
        assert_eq!(load(&ProfileRef { id: r.id.clone(), version: 1 }).unwrap().format, 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn find_auto_like_load_custom_and_list_skips_history() {
        let _l = lock();
        let root = scratch("auto");
        v1_file(&root, "a");
        assert!(find_auto("Canon", "EOS 400D DIGITAL", Some("EF-S10-22mm f/3.5-4.5 USM")).is_some());
        assert!(find_auto("CANON INC.", "Canon EOS 400D DIGITAL", Some("EF-S 10-22mm f/3.5-4.5 USM")).is_some());
        assert!(find_auto("Canon", "EOS 400D DIGITAL", None).is_none());
        assert!(find_auto("Nikon", "EOS 400D DIGITAL", Some("EF-S10-22mm f/3.5-4.5 USM")).is_none());
        // a draft revision gives its shots to the saved version
        let mut p = OwnProfile::from_json(&v1(), "").unwrap();
        p.lens = String::new();
        p.history.push(Revision { version: 0, time: 0, action: RevisionAction::Measured, shots: vec![ShotRef { name: "A.X3F".into(), sha256: String::new(), role: crate::own::ShotRole::FlatField, focal_length: None, aperture: Some(4.0), rotated: false }], note: "Tageslicht".into() });
        let r = save_new(p).unwrap();
        let s = load(&r).unwrap();
        assert_eq!((s.history.len(), s.history[0].shots.len(), s.history[0].note.as_str()), (1, 1, "Tageslicht"));
        save_version((*s).clone(), RevisionAction::Extended).unwrap();
        // manual lens: matches photos without lens name
        assert_eq!(find_auto("Canon", "EOS 400D DIGITAL", None).unwrap().id, r.id);
        assert_eq!(list().len(), 2, "snapshots do not count as a profile");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn root_follows_data_dir() {
        let _l = lock();
        let before = std::env::var_os("GRAINEO_LENS_BACKUP");
        std::env::remove_var("GRAINEO_LENS_BACKUP");
        std::env::set_var("GRAINEO_DATA_DIR", "/x");
        assert_eq!(root(), Some(PathBuf::from("/x/lens-profiles")));
        assert_eq!(own_dir(), Some(PathBuf::from("/x/lens-profiles/own")));
        std::env::remove_var("GRAINEO_DATA_DIR");
        // test program without data folder: its own temp folder, never the real one
        let r = root().unwrap();
        assert!(r.starts_with(std::env::temp_dir()), "{r:?}");
        if let Some(real) = sysdirs::app_data_dir() {
            assert!(!r.starts_with(real.join(sysdirs::APP)), "{r:?}");
        }
        std::env::set_var("GRAINEO_LENS_BACKUP", "");
        assert_eq!(root(), None);
        match before {
            Some(v) => std::env::set_var("GRAINEO_LENS_BACKUP", v),
            None => std::env::remove_var("GRAINEO_LENS_BACKUP"),
        }
    }
}
