//! First start after an update: files that earlier versions wrote under German names get their current names
//! (table: `legacy_names::app::FILES`). Only renames, nothing is ever deleted or overwritten; runs again without
//! effect once everything is renamed (also in test runs with `GRAINEO_DATA_DIR`, whose folder is the app's own).

use legacy_names::app::{Place, FILES};
use std::path::Path;

/// Rename the files of the table that still have their old name. Call first thing in `main` (before the log is opened).
pub fn run() {
    let data = library::data_dir();
    let logs = crate::crash_log::folder().map(|(dir, _)| dir);
    for (old, new, place) in FILES {
        let dir = match place {
            Place::Data => Some(data.clone()),
            Place::Logs => logs.clone(),
        };
        if let Some(dir) = dir {
            rename_if_free(&dir, old, new);
        }
    }
}

/// `dir/old` → `dir/new`, only if `old` exists and `new` does not.
fn rename_if_free(dir: &Path, old: &str, new: &str) -> bool {
    let (from, to) = (dir.join(old), dir.join(new));
    if from.exists() && !to.exists() {
        return std::fs::rename(from, to).is_ok();
    }
    false
}

/// The module the app starts in (`module.txt`: `library` or `develop`; the old German values are translated).
pub fn start_module() -> Option<String> {
    let text = std::fs::read_to_string(crate::app_file("module.txt")?).ok()?;
    Some(legacy_names::translate(legacy_names::app::START_MODULE, text.trim()).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_once_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("graineo-file-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "old").unwrap();
        assert!(rename_if_free(&dir, "a.txt", "b.txt"));
        assert!(!dir.join("a.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "old");
        // second run: nothing to do
        assert!(!rename_if_free(&dir, "a.txt", "b.txt"));
        // the new name exists: the old file stays untouched
        std::fs::write(dir.join("a.txt"), "again").unwrap();
        assert!(!rename_if_free(&dir, "a.txt", "b.txt"));
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn table_has_no_clashes() {
        for (old, new, _) in FILES {
            assert_ne!(old, new);
            assert!(!FILES.iter().any(|(o, _, _)| o == new), "{new} is also an old name");
        }
    }
}
