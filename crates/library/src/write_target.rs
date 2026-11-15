//! Where the app may write new files (export, moving, import, contact sheets, saved looks): never inside a package of
//! another program – an Apple Photos library (`….photoslibrary/originals/…`), a Lightroom preview folder, an app
//! bundle and the like. Writing there damages the other program's library; Apple Photos does not even notice the
//! files. Checked when a target is chosen and again right before writing.

use std::path::{Path, PathBuf};

/// Folder extensions of packages and libraries of other programs (macOS shows them as one file).
pub const PACKAGES: [&str; 22] = [
    "photoslibrary", "photolibrary", "migratedphotolibrary", "aplibrary", "lrdata", "lrlibrary", "app", "graineopreviews", "fcpbundle", "imovielibrary", "tvlibrary", "musiclibrary", "cocatalog", "cosessiondb",
    "bundle", "framework", "plugin", "pkg", "logicx", "band", "sparsebundle", "rtfd",
];

/// Is this folder name (last path component) a package of another program?
pub fn is_package_name(name: &str) -> bool {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    PACKAGES.contains(&ext.as_str())
}

/// The package that contains `p` (or is `p`), if any – then nothing may be written to `p`.
pub fn protected_by(p: &Path) -> Option<PathBuf> {
    let mut so_far = PathBuf::new();
    for c in p.components() {
        so_far.push(c);
        if is_package_name(&c.as_os_str().to_string_lossy()) {
            return Some(so_far);
        }
    }
    None
}

/// Error text when `target` lies in a package of another program (German; the app translates it on display).
pub fn check(target: &Path) -> Result<(), String> {
    match protected_by(target) {
        Some(pkg) => Err(format!("Ziel liegt in „{}“ – in die Mediathek oder das Paket eines anderen Programms schreibt graineo nicht", pkg.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_of_other_programs_are_refused() {
        let photos = Path::new("/Users/a/Pictures/Photos Library.photoslibrary/originals/A/export");
        assert_eq!(protected_by(photos), Some(PathBuf::from("/Users/a/Pictures/Photos Library.photoslibrary")));
        assert!(check(photos).is_err());
        assert!(check(Path::new("/Users/a/Pictures/Lightroom/Catalog Previews.lrdata/x")).is_err());
        assert!(check(Path::new("/Applications/graineo.app/Contents")).is_err());
        assert!(check(Path::new("/Users/a/Pictures/Fotos.PhotosLibrary")).is_err());
        // ordinary folders, also with dots in their names
        assert!(check(Path::new("/Users/a/Pictures/export")).is_ok());
        assert!(check(Path::new("/Volumes/media/2024.05 Urlaub/export")).is_ok());
        assert!(check(Path::new("/Users/a/Pictures/Lightroom")).is_ok());
    }
}
