//! Export "Original": the file unchanged, copied and verified (SHA-256, never overwriting anything), optionally with
//! its XMP sidecar (ratings, keywords and edits from the library, as the optional sidecars next to the photos) and
//! the camera JPEG of a RAW+JPEG pair.

use std::path::{Path, PathBuf};

use crate::db::Db;

/// One file to copy: where it is read from, the name it gets, and its file id in the library (for the sidecar).
#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub name: String,
    pub file: Option<i64>,
}

impl Source {
    pub fn of(path: &Path, file: Option<i64>) -> Source {
        Source { path: path.to_path_buf(), name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), file }
    }
}

/// Copy `photo` (and with `pair` its camera JPEG, named like the copied photo) into `dir`. `sidecars`: an XMP next to
/// each copy – from the library if the file is in it, otherwise the existing sidecar of the original, if any.
/// Returns the files written (photo first).
pub fn export(db: Option<&Db>, photo: &Source, pair: Option<&Source>, dir: &Path, sidecars: bool) -> Result<Vec<PathBuf>, String> {
    crate::write_target::check(dir)?;
    let mut written = Vec::new();
    let copy = crate::card_import::copy_verified_as(&photo.path, dir, &photo.name, &mut |_| {})?;
    written.push(copy.clone());
    let pair_copy = match pair {
        Some(j) => {
            // same name as the copied photo (it may have become "NAME-1"), with the JPEG's extension
            let stem = copy.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = Path::new(&j.name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "JPG".into());
            let c = crate::card_import::copy_verified_as(&j.path, dir, &format!("{stem}.{ext}"), &mut |_| {})?;
            written.push(c.clone());
            Some(c)
        }
        None => None,
    };
    if sidecars {
        if let Some(x) = write_sidecar(db, photo, &copy, false)? {
            written.push(x);
        }
        if let (Some(j), Some(c)) = (pair, &pair_copy) {
            if let Some(x) = write_sidecar(db, j, c, true)? {
                written.push(x);
            }
        }
    }
    Ok(written)
}

/// XMP next to `copy` (never replacing a file there). `raw_beside`: the copy is the camera JPEG of a RAW
/// (`NAME.JPG.xmp`, as next to the photos).
fn write_sidecar(db: Option<&Db>, src: &Source, copy: &Path, raw_beside: bool) -> Result<Option<PathBuf>, String> {
    let name = copy.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let format = crate::format_from_extension(&name).unwrap_or("DNG");
    let target = crate::xmp::path(copy, format, raw_beside);
    // the existing sidecar of the original: its foreign fields stay
    let existing = crate::xmp::path_to(&src.path);
    let old = std::fs::read_to_string(&existing).ok();
    let text = match (db, src.file) {
        (Some(db), Some(file)) => crate::store::sidecar_text(db, file, old.as_deref())?.or(old),
        _ => old,
    };
    let Some(text) = text else { return Ok(None) };
    let taken = || format!("{}: gibt es schon – nichts wird überschrieben", target.display());
    if target.exists() {
        return Err(taken());
    }
    // side file, then an atomic rename that never replaces (a file appearing meanwhile stays)
    let tmp = crate::files::side_path(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Err(e) = crate::files::place_sibling_file(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(if e.kind() == std::io::ErrorKind::AlreadyExists { taken() } else { format!("{}: {e}", target.display()) });
    }
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-original-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The copy is byte-identical, a pair JPEG and the sidecars follow the name of the copy, nothing existing is
    /// overwritten, and a package of another program is refused.
    #[test]
    fn copies_unchanged_with_pair_and_sidecars() {
        let root = tmp("export");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("A.CR3"), b"raw-bytes").unwrap();
        std::fs::write(src.join("A.JPG"), b"jpeg-bytes").unwrap();
        std::fs::write(src.join("A.xmp"), "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>").unwrap();
        let out = root.join("out");
        let photo = Source::of(&src.join("A.CR3"), None);
        let pair = Source::of(&src.join("A.JPG"), None);
        let w = export(None, &photo, Some(&pair), &out, true).unwrap();
        assert_eq!(w, vec![out.join("A.CR3"), out.join("A.JPG"), out.join("A.xmp")]);
        assert_eq!(std::fs::read(out.join("A.CR3")).unwrap(), b"raw-bytes");
        assert_eq!(std::fs::read(out.join("A.JPG")).unwrap(), b"jpeg-bytes");
        // a second export of a different file with the same name: "A-1", its pair and sidecar follow
        std::fs::write(src.join("A.CR3"), b"other-raw").unwrap();
        let w = export(None, &photo, Some(&pair), &out, true).unwrap();
        assert_eq!(w, vec![out.join("A-1.CR3"), out.join("A-1.JPG"), out.join("A-1.xmp")]);
        assert_eq!(std::fs::read(out.join("A.CR3")).unwrap(), b"raw-bytes");
        // without sidecars and pair: only the photo
        let w = export(None, &Source::of(&src.join("A.JPG"), None), None, &root.join("out2"), false).unwrap();
        assert_eq!(w, vec![root.join("out2/A.JPG")]);
        // never into an Apple Photos library
        let lib = root.join("Photos Library.photoslibrary/originals/0");
        assert!(export(None, &photo, None, &lib, false).is_err());
        assert!(!lib.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
