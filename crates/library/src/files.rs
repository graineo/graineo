//! Safe file writing: never into an existing path (not via a link either),
//! but into a newly created, unpredictable side file that then replaces the target name.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `content` atomically to `target`: create the side file exclusively (`create_new`, follows no link),
/// write to disk, then rename over `target`. On errors `target` stays unchanged
/// and the side file is removed.
pub fn write_atomic(target: &Path, content: &[u8]) -> std::io::Result<()> {
    write_atomic_with(target, content, true)
}

/// Like [`write_atomic`] without forcing the content to disk first (`fsync`) – for background copies such as the
/// sidecars next to the photos (the library holds the truth; a lost write is repeated).
pub fn write_atomic_no_sync(target: &Path, content: &[u8]) -> std::io::Result<()> {
    write_atomic_with(target, content, false)
}

fn write_atomic_with(target: &Path, content: &[u8], sync: bool) -> std::io::Result<()> {
    // protection: never write into an Apple Photos library (sidecars are redirected by `xmp::path`)
    if crate::apple_photos::in_photos_library(target) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nie in die Mediathek von Apple Fotos schreiben"));
    }
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if target.starts_with(crate::apple_photos::sidecar_folder()) {
        std::fs::create_dir_all(dir)?;
    }
    let name = target.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "kein Dateiname"))?.to_string_lossy().into_owned();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for _ in 0..8 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{name}.{}-{n}-{:x}.tmp", std::process::id(), nanos));
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let r = f.write_all(content).and_then(|_| if sync { f.sync_all() } else { Ok(()) }).and_then(|_| {
            drop(f);
            std::fs::rename(&tmp, target)
        });
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        return r;
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "keine freie Nebendatei"))
}

/// Name of a new, unpredictable side file next to `target` (`.NAME.<process>-<counter>-<time>.<ext>`).
/// Extension of the side file of a copy in progress (`.NAME.<process>-<counter>-<time>.copying`).
pub const PARTIAL_COPY: &str = "copying";

/// Name of a side file of an interrupted copy without the leading dot and the extension (`NAME.<marker>`); also the
/// names graineo 1.0 used ([`legacy_names::files::PARTIAL_COPY`]).
pub fn partial_copy_stem(name: &str) -> Option<&str> {
    let rest = name.strip_prefix('.')?;
    std::iter::once(PARTIAL_COPY).chain(std::iter::once(legacy_names::files::PARTIAL_COPY)).find_map(|ext| rest.strip_suffix(ext)?.strip_suffix('.'))
}

fn side_name(target: &Path, extension_of: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "kein Dateiname"))?.to_string_lossy().into_owned();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!(".{name}.{}-{n}-{:x}.{extension_of}", std::process::id(), nanos)))
}

/// Path for a side file that another writer creates itself (e.g. an image export): next to `target`,
/// with an unpredictable prefix (`.<process>-<counter>-<time>-NAME.EXT`) and the same extension as `target`.
/// The file is not created; but the name is free and practically impossible to guess in advance.
pub fn side_path(target: &Path) -> std::io::Result<std::path::PathBuf> {
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "kein Dateiname"))?.to_string_lossy().into_owned();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!(".{}-{n}-{:x}-{name}", std::process::id(), nanos)))
}

/// Creates a new side file next to `target` (`create_new`: follows no existing link, overwrites nothing)
/// and copies the content of `source` into it, flushed to disk. Returns: path of the side file;
/// on errors it is removed.
pub fn side_copy(source: &Path, target: &Path) -> std::io::Result<std::path::PathBuf> {
    for _ in 0..8 {
        let tmp = side_name(target, PARTIAL_COPY)?;
        let mut out = match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let r = std::fs::File::open(source).and_then(|mut entry| std::io::copy(&mut entry, &mut out)).and_then(|_| out.sync_all());
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        return Ok(tmp);
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "keine freie Nebendatei"))
}

/// Like [`side_copy`], but reads the source only once and computes its SHA-256 (hex) along the way. `bytes` receives
/// the amount copied so far, continuously.
pub fn side_copy_with_sum(source: &Path, target: &Path, bytes: &mut dyn FnMut(u64)) -> std::io::Result<(std::path::PathBuf, String)> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    for _ in 0..8 {
        let tmp = side_name(target, PARTIAL_COPY)?;
        let mut out = match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let mut h = Sha256::new();
        let r = (|| {
            let mut entry = std::fs::File::open(source)?;
            let mut buf = vec![0u8; 1 << 20];
            let mut n = 0u64;
            loop {
                let k = entry.read(&mut buf)?;
                if k == 0 {
                    break;
                }
                h.update(&buf[..k]);
                out.write_all(&buf[..k])?;
                n += k as u64;
                bytes(n);
            }
            out.sync_all()
        })();
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        return Ok((tmp, h.finalize().iter().map(|b| format!("{b:02x}")).collect()));
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "keine freie Nebendatei"))
}

/// Renames `from` to `by_key` without ever replacing anything existing (no link, no empty folder either).
/// If `by_key` already exists – or appears at the same moment – `AlreadyExists` is returned and nothing changes.
/// macOS: `renamex_np` with `RENAME_EXCL` (atomic). File systems without this support (e.g. some network drives):
/// files via a hard link, else check and rename.
pub fn rename_no_replace(from: &Path, by_key: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let a = std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Pfad mit NUL"))?;
        let b = std::ffi::CString::new(by_key.as_os_str().as_bytes()).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Pfad mit NUL"))?;
        // SAFETY: both pointers point to valid NUL-terminated strings that live until the call returns.
        let r = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_EXCL) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::EINVAL) => {} // the file system cannot do it: fallback below
            _ => return Err(e),
        }
    }
    if std::fs::symlink_metadata(from).is_ok_and(|m| m.is_file()) {
        match std::fs::hard_link(from, by_key) {
            Ok(()) => return std::fs::remove_file(from),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
    }
    if std::fs::symlink_metadata(by_key).is_ok() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
    }
    std::fs::rename(from, by_key)
}

/// Installs a finished side file `tmp` under the name `target`: without overwriting; on errors `tmp` is removed.
pub fn place_sibling_file(tmp: &Path, target: &Path) -> std::io::Result<()> {
    let r = rename_no_replace(tmp, target);
    if r.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-atomar-{}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replaces_atomically_and_leaves_nothing() {
        let d = tmp();
        let z = d.join("a.json");
        write_atomic(&z, b"eins").unwrap();
        write_atomic(&z, b"zwei").unwrap();
        assert_eq!(std::fs::read(&z).unwrap(), b"zwei");
        let rest: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(rest, ["a.json"], "no side files left");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn does_not_write_through_a_planted_link() {
        let d = tmp();
        let foreign = d.join("fremd.txt");
        std::fs::write(&foreign, b"fremd").unwrap();
        let z = d.join("a.json");
        // the former, predictable side file name points to a foreign file
        std::os::unix::fs::symlink(&foreign, d.join(".a.json.tmp")).unwrap();
        write_atomic(&z, b"neu").unwrap();
        assert_eq!(std::fs::read(&foreign).unwrap(), b"fremd");
        assert_eq!(std::fs::read(&z).unwrap(), b"neu");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn missing_folder_is_an_error_without_trace() {
        let d = tmp();
        assert!(write_atomic(&d.join("nicht/da/a.json"), b"x").is_err());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn rename_never_replaces_anything() {
        let d = tmp();
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let e = rename_no_replace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!((std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()), (b"a".to_vec(), b"b".to_vec()));
        // a broken link at the target also counts as taken
        let l = d.join("l.txt");
        std::os::unix::fs::symlink(d.join("gibt-es-nicht"), &l).unwrap();
        assert_eq!(rename_no_replace(&a, &l).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        // free target: the file moves
        let c = d.join("c.txt");
        rename_no_replace(&a, &c).unwrap();
        assert!(!a.exists() && c.exists());
        // folders: not onto an existing (even empty) folder
        let (o1, o2) = (d.join("o1"), d.join("o2"));
        std::fs::create_dir(&o1).unwrap();
        std::fs::create_dir(&o2).unwrap();
        assert_eq!(rename_no_replace(&o1, &o2).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn side_copy_follows_no_planted_link() {
        let d = tmp();
        let q = d.join("q.bin");
        std::fs::write(&q, b"inhalt").unwrap();
        let foreign = d.join("fremd.txt");
        std::fs::write(&foreign, b"fremd").unwrap();
        let target = d.join("z.bin");
        std::os::unix::fs::symlink(&foreign, d.join(".z.bin.teil")).unwrap();
        std::os::unix::fs::symlink(&foreign, d.join(".z.bin.copying")).unwrap();
        let t = side_copy(&q, &target).unwrap();
        assert_eq!(std::fs::read(&t).unwrap(), b"inhalt");
        place_sibling_file(&t, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"inhalt");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"fremd");
        // second install fails and cleans up the side file
        let t2 = side_copy(&q, &target).unwrap();
        assert!(place_sibling_file(&t2, &target).is_err());
        assert!(!t2.exists());
        std::fs::remove_dir_all(d).unwrap();
    }
}
