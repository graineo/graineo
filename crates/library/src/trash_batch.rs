//! Putting many files into the Trash at once ("collected", not one call per file).
//!
//! On the Mac every single call into the system Trash costs about a quarter to one second (the Finder moves the
//! file so that "Put Back" works). For hundreds of photos that adds up to many minutes. Here the files are handed
//! over in blocks: all local files of a block in ONE call, files on a network drive per root folder in one move
//! into the drive's own trash folder ([`crate::network_trash`]).
//!
//! The caller passes the actual Trash call for local files (`local`), so this module needs no platform code and
//! can be tested with a stand-in. If a whole block fails, each entry is tried on its own, so that one bad file
//! does not keep the others back; an entry whose files are already gone after the failed block counts as done.
//!
//! Entries are units that go together (a photo and its sidecars): either all their files go or the entry
//! reports an error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files per call. Large enough to save the per-call cost (measured for "RAW+JPEG doppelt": 2 052 JPEGs in 54 s
/// instead of one Trash request per file), small enough for a quick cancel and a short command line.
pub const BLOCK: usize = 200;

/// One unit to throw away: a key of the caller (e.g. image ID) and its files.
#[derive(Clone, Debug)]
pub struct Entry<K> {
    pub key: K,
    pub files: Vec<PathBuf>,
}

/// Where the files go.
pub struct Targets<'a> {
    /// local files (system Trash), all of a block at once
    pub local: &'a dyn Fn(&[PathBuf]) -> Result<(), String>,
    /// is the file on a network drive (own trash folder)?
    pub is_network: &'a dyn Fn(&Path) -> bool,
    /// root folder of a file on a network drive (the trash folder lies in it)
    pub root_of: &'a dyn Fn(&Path) -> Option<PathBuf>,
    /// day folder in the network trash (`YYYY-MM-DD`)
    pub day: String,
}

impl Targets<'_> {
    fn network(&self, root: &Path, files: &[PathBuf]) -> Result<(), String> {
        crate::network_trash::discard(root, files, &self.day).map(|_| ())
    }
}

/// Throw the entries away in blocks of `block`. `progress(done, total)` (entries) and `cancel` are checked between
/// blocks; a cancel leaves the rest untouched (not in the result). Returns per handled entry: done or the error.
pub fn dispose<K: Clone>(entries: &[Entry<K>], block: usize, t: &Targets, progress: &dyn Fn(usize, usize), cancel: &dyn Fn() -> bool) -> Vec<(K, Result<(), String>)> {
    if let Err(e) = crate::backup_view::refuse() {
        return entries.iter().map(|x| (x.key.clone(), Err(e.clone()))).collect();
    }
    let mut out = Vec::with_capacity(entries.len());
    let total = entries.len();
    for (i, chunk) in entries.chunks(block.max(1)).enumerate() {
        if cancel() {
            break;
        }
        progress(i * block.max(1), total);
        // local vs. network (per root folder)
        let mut local: Vec<&Entry<K>> = Vec::new();
        let mut network: BTreeMap<PathBuf, Vec<&Entry<K>>> = BTreeMap::new();
        for e in chunk {
            let Some(first) = e.files.first() else {
                out.push((e.key.clone(), Ok(())));
                continue;
            };
            if first.parent().is_some_and(|p| (t.is_network)(p)) {
                match (t.root_of)(first) {
                    Some(root) => network.entry(root).or_default().push(e),
                    None => out.push((e.key.clone(), Err(format!("{}: Stammordner unbekannt", first.display())))),
                }
            } else {
                local.push(e);
            }
        }
        if !local.is_empty() {
            let all: Vec<PathBuf> = local.iter().flat_map(|e| e.files.iter().cloned()).collect();
            let r = (t.local)(&all);
            settle(&local, r, &|files| (t.local)(files), &mut out);
        }
        for (root, list) in network {
            let all: Vec<PathBuf> = list.iter().flat_map(|e| e.files.iter().cloned()).collect();
            let r = t.network(&root, &all);
            settle(&list, r, &|files| t.network(&root, files), &mut out);
        }
    }
    progress(out.len(), total);
    out
}

/// All `files` in one go (each file its own entry, one call for the local ones): the number taken, or the first
/// error if any file stayed (the others are gone all the same).
pub fn dispose_all(files: &[PathBuf], t: &Targets) -> Result<usize, String> {
    let entries: Vec<Entry<usize>> = files.iter().enumerate().map(|(i, f)| Entry { key: i, files: vec![f.clone()] }).collect();
    let r = dispose(&entries, entries.len().max(1), t, &|_, _| {}, &|| false);
    let n = r.iter().filter(|(_, x)| x.is_ok()).count();
    match r.into_iter().find_map(|(_, x)| x.err()) {
        Some(e) => Err(e),
        None => Ok(n),
    }
}

/// Result of one block: all fine, or each entry on its own (entries already gone count as done).
fn settle<K: Clone>(list: &[&Entry<K>], r: Result<(), String>, one: &dyn Fn(&[PathBuf]) -> Result<(), String>, out: &mut Vec<(K, Result<(), String>)>) {
    if r.is_ok() {
        out.extend(list.iter().map(|e| (e.key.clone(), Ok(()))));
        return;
    }
    for e in list {
        let left: Vec<PathBuf> = e.files.iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).cloned().collect();
        if left.is_empty() {
            out.push((e.key.clone(), Ok(())));
            continue;
        }
        out.push((e.key.clone(), one(&left)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn dir(name: &str) -> PathBuf {
        let d = PathBuf::from(format!("/private/tmp/graineo-trash-batch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// stand-in for the Trash: moves the files into `_weg`, fails on names containing "kaputt"
    fn fake(calls: &Cell<usize>) -> impl Fn(&[PathBuf]) -> Result<(), String> + '_ {
        move |files: &[PathBuf]| {
            calls.set(calls.get() + 1);
            if files.iter().any(|f| f.to_string_lossy().contains("kaputt")) {
                // a real batch call moves nothing when it fails
                return Err("geht nicht".into());
            }
            for f in files {
                let to = f.parent().unwrap().join("_weg").join(f.file_name().unwrap());
                std::fs::create_dir_all(to.parent().unwrap()).unwrap();
                std::fs::rename(f, to).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }

    #[test]
    fn blocks_need_one_call_each() {
        let d = dir("bloecke");
        let entries: Vec<Entry<usize>> = (0..250)
            .map(|i| {
                let p = d.join(format!("F{i}.JPG"));
                std::fs::write(&p, b"x").unwrap();
                Entry { key: i, files: vec![p] }
            })
            .collect();
        let calls = Cell::new(0);
        let local = fake(&calls);
        let t = Targets { local: &local, is_network: &|_| false, root_of: &|_| None, day: "2026-11-11".into() };
        let r = dispose(&entries, BLOCK, &t, &|_, _| {}, &|| false);
        assert_eq!(r.len(), 250);
        assert!(r.iter().all(|(_, x)| x.is_ok()));
        assert_eq!(calls.get(), 2, "250 files in blocks of 200 = 2 calls");
        assert!(!d.join("F0.JPG").exists() && d.join("_weg/F0.JPG").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_file_does_not_hold_back_the_others() {
        let d = dir("einzeln");
        let mut entries = Vec::new();
        for (i, n) in ["A.JPG", "kaputt.JPG", "C.JPG"].iter().enumerate() {
            let p = d.join(n);
            std::fs::write(&p, b"x").unwrap();
            let xmp = d.join(format!("{n}.xmp"));
            std::fs::write(&xmp, b"x").unwrap();
            entries.push(Entry { key: i, files: vec![p, xmp] });
        }
        let calls = Cell::new(0);
        let local = fake(&calls);
        let t = Targets { local: &local, is_network: &|_| false, root_of: &|_| None, day: "2026-11-11".into() };
        let r = dispose(&entries, BLOCK, &t, &|_, _| {}, &|| false);
        assert_eq!(r.iter().filter(|(_, x)| x.is_ok()).count(), 2);
        assert!(r.iter().find(|(k, _)| *k == 1).unwrap().1.is_err());
        assert!(d.join("kaputt.JPG").exists(), "the failing photo stays");
        assert!(d.join("_weg/A.JPG.xmp").exists() && d.join("_weg/C.JPG").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_stops_between_blocks() {
        let d = dir("abbruch");
        let entries: Vec<Entry<usize>> = (0..30)
            .map(|i| {
                let p = d.join(format!("F{i}.JPG"));
                std::fs::write(&p, b"x").unwrap();
                Entry { key: i, files: vec![p] }
            })
            .collect();
        let calls = Cell::new(0);
        let local = fake(&calls);
        let t = Targets { local: &local, is_network: &|_| false, root_of: &|_| None, day: "2026-11-11".into() };
        let r = dispose(&entries, 10, &t, &|_, _| {}, &|| calls.get() >= 2);
        assert_eq!(r.len(), 20);
        assert!(d.join("F29.JPG").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn network_files_go_into_the_drive_trash_folder() {
        let d = dir("netz");
        let root = d.join("nas");
        std::fs::create_dir_all(root.join("2024")).unwrap();
        let p = root.join("2024/IMG.JPG");
        std::fs::write(&p, b"x").unwrap();
        let calls = Cell::new(0);
        let local = fake(&calls);
        let r2 = root.clone();
        let root_of = move |_: &Path| Some(r2.clone());
        let t = Targets { local: &local, is_network: &|_| true, root_of: &root_of, day: "2026-11-11".into() };
        let r = dispose(&[Entry { key: 1, files: vec![p.clone()] }], BLOCK, &t, &|_, _| {}, &|| false);
        assert!(r[0].1.is_ok(), "{:?}", r[0].1);
        assert_eq!(calls.get(), 0);
        assert!(!p.exists());
        assert!(crate::network_trash::base(&root).join("2026-11-11/2024/IMG.JPG").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
