//! Trash for files on network drives (SMB, NFS …). There is no macOS Trash there: deleting would either
//! not work at all or be permanent. Instead the file is moved into a folder on the same volume (instantly, without
//! copying) and can be put back from there.
//!
//! Location: `<root folder>/.graineo/Papierkorb/YYYY-MM-DD/<path relative to the root folder>`. In the settings
//! (`networkTrashFolder`) a different folder can be chosen: relative to the root folder or an absolute path on the same
//! volume. Because the path in the trash contains the original location, the folder name suffices for putting back; the sync
//! skips folders starting with a dot, so files in the trash do not show up in the library.

use std::path::{Path, PathBuf};

pub const STANDARD: &str = ".graineo/Papierkorb";
const SETTINGS_KEY: &str = "networkTrashFolder";

/// Configured folder (relative to the root folder or absolute); empty or missing = default.
pub fn setting() -> String {
    crate::settings::value(SETTINGS_KEY).as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(STANDARD).to_string()
}

pub fn set(folder: &str) {
    crate::settings::set(SETTINGS_KEY, serde_json::json!(folder.trim()));
}

/// Trash folder for a root folder. A setting that would make the root folder itself (or a folder above it, or one
/// outside it via `..`) the trash falls back to the default: emptying the trash removes its day folders for good, and
/// photographers name photo folders `YYYY-MM-DD`.
pub fn base(root: &Path) -> PathBuf {
    base_for(root, &setting())
}

fn base_for(root: &Path, e: &str) -> PathBuf {
    let p = Path::new(e);
    let unsafe_rel = p.components().any(|c| matches!(c, std::path::Component::ParentDir)) || p.components().all(|c| matches!(c, std::path::Component::CurDir));
    let b = if p.is_absolute() { p.to_path_buf() } else if unsafe_rel { root.join(STANDARD) } else { root.join(p) };
    if root.starts_with(&b) {
        return root.join(STANDARD);
    }
    b
}

/// Marker file in a trash folder graineo made: only there are day folders taken as trash content (an own setting
/// could point at an ordinary folder whose `YYYY-MM-DD` subfolders are photo folders).
const MARKER: &str = ".graineo-trash";

fn is_trash(root: &Path, base: &Path) -> bool {
    base == root.join(STANDARD) || base.join(MARKER).is_file()
}

/// Is the file or folder on a network drive (not on a local volume)?
pub fn is_network(path: &Path) -> bool {
    !crate::watch::local(path)
}

fn device(p: &Path) -> Option<u64> {
    let mut q = Some(p);
    while let Some(x) = q {
        if let Some(g) = crate::platform::device(x) {
            return Some(g);
        }
        q = x.parent();
    }
    None
}

fn free_name(target: PathBuf) -> PathBuf {
    if !target.exists() {
        return target;
    }
    let root = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let extension_of = target.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| target.with_file_name(format!("{root} ({n}){extension_of}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Move files (a photo including its sidecars) into the network drive's trash. `tag`: `YYYY-MM-DD`.
/// Returns: number of moved files. On an error the rest stays where it was.
pub fn discard(root: &Path, files: &[PathBuf], tag: &str) -> Result<usize, String> {
    crate::backup_view::refuse()?;
    let base = base(root);
    let mut n = 0;
    for d in files {
        let rel = d.strip_prefix(root).map_err(|_| format!("{} liegt nicht im Stammordner {}", d.display(), root.display()))?;
        let target = base.join(tag).join(rel);
        let folder = target.parent().ok_or("kein Zielordner")?;
        std::fs::create_dir_all(folder).map_err(|e| format!("Papierkorb-Ordner {}: {e}", folder.display()))?;
        if !base.join(MARKER).exists() {
            let _ = std::fs::write(base.join(MARKER), b"graineo trash folder (network drives)\n");
        }
        if device(d) != device(folder) {
            return Err(format!("Der Papierkorb-Ordner {} liegt nicht auf demselben Volume wie {}", base.display(), d.display()));
        }
        // never replace a file that appeared in the trash folder meanwhile
        crate::files::rename_no_replace(d, &free_name(target)).map_err(|e| format!("{}: {e}", d.display()))?;
        n += 1;
    }
    Ok(n)
}

/// The day folders (`YYYY-MM-DD`) in the trash of `root` (none if the folder is not a trash folder of graineo).
fn day_folder(root: &Path) -> Vec<PathBuf> {
    let base = base(root);
    if !is_trash(root, &base) {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(&base) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name().map(|n| n.to_string_lossy().into_owned()).is_some_and(|n| {
                    let b = n.as_bytes();
                    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
                })
        })
        .collect();
    v.sort();
    v
}

fn files_in(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(folder) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files_in(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Number and size of the files in a root folder's trash.
pub fn content(root: &Path) -> (usize, u64) {
    let mut all = Vec::new();
    for t in day_folder(root) {
        files_in(&t, &mut all);
    }
    let bytes = all.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    (all.len(), bytes)
}

fn remove_empty_folders(folder: &Path) {
    let Ok(rd) = std::fs::read_dir(folder) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            remove_empty_folders(&p);
        }
    }
    let _ = std::fs::remove_dir(folder); // only empty folders
}

/// Put everything from the trash back to its original location. Whatever already exists there again stays in the trash.
/// Returns: (put back, messages).
pub fn restore_all(root: &Path) -> (usize, Vec<String>) {
    if let Err(e) = crate::backup_view::refuse() {
        return (0, vec![e]);
    }
    let (mut n, mut messages) = (0, Vec::new());
    for t in day_folder(root) {
        let mut files = Vec::new();
        files_in(&t, &mut files);
        for d in files {
            let Ok(rel) = d.strip_prefix(&t) else { continue };
            let target = root.join(rel);
            if target.exists() {
                messages.push(format!("{} existiert schon, bleibt im Papierkorb", target.display()));
                continue;
            }
            if let Some(o) = target.parent() {
                if let Err(e) = std::fs::create_dir_all(o) {
                    messages.push(format!("{}: {e}", o.display()));
                    continue;
                }
            }
            match crate::files::rename_no_replace(&d, &target) {
                Ok(()) => n += 1,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => messages.push(format!("{} existiert schon, bleibt im Papierkorb", target.display())),
                Err(e) => messages.push(format!("{}: {e}", target.display())),
            }
        }
        remove_empty_folders(&t);
    }
    (n, messages)
}

/// Empty the trash permanently (only the day folders, never the folder itself or anything else in it). Returns: deleted files.
/// Call only after explicit confirmation by the user.
pub fn clear(root: &Path) -> Result<usize, String> {
    crate::backup_view::refuse()?;
    let mut n = 0;
    for t in day_folder(root) {
        let mut d = Vec::new();
        files_in(&t, &mut d);
        std::fs::remove_dir_all(&t).map_err(|e| format!("{}: {e}", t.display()))?;
        n += d.len();
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("graineo-nk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn discard_restore_and_empty() {
        let root = tmp("a");
        std::fs::create_dir_all(root.join("Sigma/DP2M")).unwrap();
        let (x3f, xmp) = (root.join("Sigma/DP2M/A.X3F"), root.join("Sigma/DP2M/A.xmp"));
        std::fs::write(&x3f, b"raw").unwrap();
        std::fs::write(&xmp, b"<x/>").unwrap();
        assert_eq!(discard(&root, &[x3f.clone(), xmp.clone()], "2026-10-31").unwrap(), 2);
        assert!(!x3f.exists() && !xmp.exists());
        assert!(root.join(STANDARD).join("2026-10-31/Sigma/DP2M/A.X3F").exists());
        assert_eq!(content(&root).0, 2);
        // same name again: is not overwritten
        std::fs::write(&x3f, b"zwei").unwrap();
        discard(&root, &[x3f.clone()], "2026-10-31").unwrap();
        assert!(root.join(STANDARD).join("2026-10-31/Sigma/DP2M/A (2).X3F").exists());
        // put back: the first version goes to the location, the second ("A (2)") is there as well
        let (n, msgs) = restore_all(&root);
        assert_eq!(n, 3, "{msgs:?}");
        assert!(x3f.exists() && xmp.exists());
        assert_eq!(content(&root).0, 0);
        // emptying deletes only day folders
        discard(&root, &[xmp.clone()], "2026-10-30").unwrap();
        std::fs::write(root.join(STANDARD).join("fremd.txt"), b"x").unwrap();
        assert_eq!(clear(&root).unwrap(), 1);
        assert!(root.join(STANDARD).join("fremd.txt").exists(), "foreign files in the folder stay");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A setting pointing at the root folder, above it or out of it via `..` falls back to the default; a folder
    /// without the marker is no trash (its `YYYY-MM-DD` folders are not counted or emptied).
    #[test]
    fn unsafe_trash_settings_fall_back() {
        let root = PathBuf::from("/Volumes/nas/Fotos");
        let std = root.join(STANDARD);
        for e in [".", "./", "..", "../x", "a/../../b", "/Volumes/nas", "/Volumes/nas/Fotos", "/"] {
            assert_eq!(base_for(&root, e), std, "{e}");
        }
        assert_eq!(base_for(&root, "Papierkorb"), root.join("Papierkorb"));
        assert_eq!(base_for(&root, "/Volumes/nas/.trash"), PathBuf::from("/Volumes/nas/.trash"));
        let r = tmp("d");
        std::fs::create_dir_all(r.join("fotos/2024-05-12")).unwrap();
        assert!(!is_trash(&r, &r.join("fotos")));
        assert!(is_trash(&r, &r.join(STANDARD)));
        std::fs::write(r.join("fotos").join(MARKER), b"").unwrap();
        assert!(is_trash(&r, &r.join("fotos")));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn file_outside_root_is_rejected() {
        let (root, other) = (tmp("b"), tmp("c"));
        let f = other.join("x.X3F");
        std::fs::write(&f, b"x").unwrap();
        assert!(discard(&root, &[f.clone()], "2026-10-31").is_err());
        assert!(f.exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }
}
