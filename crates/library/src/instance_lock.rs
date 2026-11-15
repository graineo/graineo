//! One running graineo per library: an exclusive OS file lock on `instance.lock` next to `library.sqlite`.
//! The operating system releases the lock when the process ends, also after a crash or a forced quit, so a
//! lock file left behind never blocks the next start (no stale locks).

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The held lock: kept open for the lifetime of the process.
static HELD: OnceLock<File> = OnceLock::new();

/// The lock file of a library database.
pub fn lock_path(db: &Path) -> PathBuf {
    db.parent().map(Path::to_path_buf).unwrap_or_default().join("instance.lock")
}

/// Result of [`acquire`].
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// This process now holds the lock (or already did).
    Acquired,
    /// Another running process holds it; its process id if it wrote one.
    Busy(Option<u32>),
}

/// Take the lock for the library `db`, waiting up to `patience` for a previous process that is still ending
/// (a restart into or out of a backup view, an update). Errors (folder not writable, file system without locks)
/// let the start go on: the lock only protects against a second instance, it must never prevent the first.
pub fn acquire(db: &Path, patience: Duration) -> Result<Outcome, String> {
    if HELD.get().is_some() {
        return Ok(Outcome::Acquired);
    }
    let path = lock_path(db);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let start = Instant::now();
    loop {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {
                // the process id only helps a person or the message of a second start; the lock itself is what counts
                let _ = file.set_len(0);
                let _ = write!(file, "{}", std::process::id());
                let _ = file.flush();
                let _ = HELD.set(file);
                return Ok(Outcome::Acquired);
            }
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() >= patience {
                    let pid = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok());
                    return Ok(Outcome::Busy(pid));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(TryLockError::Error(e)) => return Err(format!("{}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_handle_sees_lock_and_release_frees_it() {
        let dir = std::env::temp_dir().join(format!("graineo-instance-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.sqlite");
        let path = lock_path(&db);
        // a first holder (stands in for another process: OS locks on separate open files exclude each other)
        let first = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).unwrap();
        first.try_lock().unwrap();
        let other = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        // ending the holder (like a crash) frees the lock although the file stays
        drop(first);
        assert!(path.exists());
        assert!(other.try_lock().is_ok());
        drop(other);
        assert_eq!(acquire(&db, Duration::ZERO).unwrap(), Outcome::Acquired);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
