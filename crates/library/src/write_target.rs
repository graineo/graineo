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
        Some(pkg) => Err(format!("Ziel liegt in „{}“ – in die Bibliothek oder das Paket eines anderen Programms schreibt graineo nicht", pkg.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
        None => Ok(()),
    }
}

/// Is the target folder writable? Writes and removes a small scratch file in the folder (or, if it does not exist
/// yet, in the nearest existing folder above it) – so the user gets a clear message before an export, import or move
/// starts instead of an error in the middle of the run (read-only drive, Windows "controlled folder access",
/// missing rights on a NAS). The error text is German and carries the system error (`… (os error N)`), which
/// the app turns into a plain phrase on display.
pub fn probe(target: &Path) -> Result<(), String> {
    use std::io::Write;
    let Some(base) = target.ancestors().find(|a| a.is_dir()) else {
        return Err(format!("Zielordner nicht gefunden: {}", sysdirs::display_path(target)));
    };
    // an unmounted drive leaves only "/Volumes" (or "/") as the existing part: not the user's fault, the
    // later steps report "not reachable"
    if base != target && matches!(base.to_str(), Some("/Volumes" | "/" | "/mnt" | "/media")) {
        return Ok(());
    }
    // unique name (process id, counter, clock); `create_new` never touches an existing file, and we only remove
    // the file if we created it ourselves
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = format!(
        ".graineo-write-test-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0)
    );
    let file = base.join(name);
    let result = match std::fs::OpenOptions::new().write(true).create_new(true).open(&file) {
        Ok(mut f) => {
            let r = f.write_all(b"x");
            drop(f);
            let _ = std::fs::remove_file(&file);
            r
        }
        Err(e) => Err(e),
    };
    result.map_err(|e| format!("Zielordner nicht beschreibbar: {}: {e}", sysdirs::display_path(base)))
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

    #[test]
    fn probe_accepts_writable_and_not_yet_existing_folders() {
        let d = std::env::temp_dir().join(format!("graineo-probe-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(probe(&d).is_ok());
        assert!(probe(&d.join("new").join("deeper")).is_ok());
        // no scratch file stays behind
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        // a file is not a folder: the folder above is probed, not the file
        let f = d.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(probe(&f).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn probe_reports_read_only_folder() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("graineo-probe-ro-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = probe(&d.join("sub"));
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        // root ignores the mode bits
        if let Err(e) = r {
            assert!(e.starts_with("Zielordner nicht beschreibbar: "), "{e}");
            assert!(e.contains("os error 13"), "{e}");
        }
    }
}
