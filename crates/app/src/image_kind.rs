//! **One** place decides which development an image gets – from the file itself
//! (extension, for X3F additionally the "FOVb" signature at the start of the file), never from the camera maker,
//! library entry or cached state.
//!
//! - [`ImageKind::Foveon`]: X3F only (Merrill, Quattro incl. sd Quattro H, Classic SD9–SD15/DP1–DP3)
//!   → Sigma path (SPP-matching base, Sigma detail, Fill Light, levels, layer mixer, camera JPEG, sensor).
//! - [`ImageKind::Raw`]: Bayer/X-Trans RAWs, **including Sigma fp/fp L/BF (DNG)** → general RAW path.
//! - [`ImageKind::Done`]: JPEG/HEIC/TIFF/PNG → general path with basic sliders.
//! - [`ImageKind::Other`]: video, PSD … → not editable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKind {
    Foveon,
    Raw,
    Done,
    Other,
}

impl ImageKind {
    pub fn name(self) -> &'static str {
        match self {
            ImageKind::Foveon => crate::tr("Foveon"),
            ImageKind::Raw => crate::tr("RAW (allgemeiner Weg)"),
            ImageKind::Done => crate::tr("fertiges Bild (allgemeiner Weg)"),
            ImageKind::Other => crate::tr("nicht bearbeitbar"),
        }
    }

    /// Can it be edited (one of the two developments)?
    pub fn editable(self) -> bool {
        self != ImageKind::Other
    }
}

/// Only from the extension or the library's format name (X3F, RAF, JPG …), without file access.
pub fn from_extension(extension: &str) -> ImageKind {
    match extension.to_ascii_lowercase().as_str() {
        "x3f" => ImageKind::Foveon,
        "cr2" | "cr3" | "raf" | "dng" | "arw" | "nef" | "nrw" | "orf" | "rw2" | "pef" | "srw" => ImageKind::Raw,
        "jpg" | "jpeg" | "heic" | "heif" | "hif" | "tif" | "tiff" | "png" => ImageKind::Done,
        _ => ImageKind::Other,
    }
}

static CHECKED: Mutex<Option<HashMap<PathBuf, ImageKind>>> = Mutex::new(None);

/// Image kind of a file. X3F only counts with the "FOVb" signature (4 bytes, read once per path);
/// an unreadable X3F (e.g. volume missing) counts as Foveon by its extension.
pub fn of(path: &Path) -> ImageKind {
    let kind = from_extension(path.extension().and_then(|e| e.to_str()).unwrap_or(""));
    if kind != ImageKind::Foveon {
        return kind;
    }
    if let Some(a) = CHECKED.lock().unwrap().as_ref().and_then(|m| m.get(path).copied()) {
        return a;
    }
    use std::io::Read;
    let mut header = [0u8; 4];
    let a = match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) if &header == b"FOVb" => ImageKind::Foveon,
        Ok(()) => ImageKind::Other,
        Err(_) => return ImageKind::Foveon,
    };
    CHECKED.lock().unwrap().get_or_insert_with(HashMap::new).insert(path.to_path_buf(), a);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(from_extension("X3F"), ImageKind::Foveon);
        // Sigma fp/fp L/BF write Bayer DNGs → general path
        assert_eq!(from_extension("DNG"), ImageKind::Raw);
        for e in ["RAF", "cr3", "CR2", "ARW", "NEF", "nef", "NRW", "nrw"] {
            assert_eq!(from_extension(e), ImageKind::Raw, "{e}");
        }
        for e in ["JPG", "jpeg", "HEIC", "TIFF", "png"] {
            assert_eq!(from_extension(e), ImageKind::Done, "{e}");
        }
        assert_eq!(from_extension("MOV"), ImageKind::Other);
        assert_eq!(from_extension("VIDEO"), ImageKind::Other);
    }

    #[test]
    fn x3f_signature() {
        let d = std::env::temp_dir().join(format!("bildart-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let real = d.join("a.X3F");
        std::fs::write(&real, b"FOVb\x00\x00").unwrap();
        let wrong = d.join("b.x3f");
        std::fs::write(&wrong, b"\xFF\xD8\xFF\xE0\x00").unwrap();
        assert_eq!(of(&real), ImageKind::Foveon);
        assert_eq!(of(&wrong), ImageKind::Other);
        assert_eq!(of(&d.join("fehlt.X3F")), ImageKind::Foveon);
        assert_eq!(of(Path::new("/x/fp.DNG")), ImageKind::Raw);
    }
}
