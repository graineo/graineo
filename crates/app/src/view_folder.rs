//! "View folder" (⌘O): which files of a folder outside the library are shown. Subfolders are included (the photos of
//! a card or an export often lie one or two levels down); the grid shows the relative path as name. If nothing is
//! shown, [`Listing::empty_reason`] says why instead of an empty grid.

use std::path::Path;

/// Depth of subfolders looked into (the folder itself is 0).
pub const MAX_DEPTH: usize = 4;
/// At most this many photos (a whole disk chosen by mistake must not block the window).
pub const MAX_FILES: usize = 5000;

/// Files of a viewed folder.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Listing {
    /// photos and videos graineo can show, relative to the folder (`a.jpg`, `Tag 1/b.raf`), sorted
    pub names: Vec<String>,
    /// other files (documents, sidecars …)
    pub others: usize,
    /// subfolders looked into
    pub folders: usize,
    /// stopped at [`MAX_FILES`] or [`MAX_DEPTH`]
    pub truncated: bool,
    /// the folder itself could not be read (no permission, drive gone)
    pub unreadable: bool,
}

/// Folders never looked into: hidden ones, packages of other programs (Apple Photos library, Lightroom previews,
/// apps) and graineo's own folder.
fn skipped(name: &str) -> bool {
    name.starts_with('.') || [".photoslibrary", ".lrdata", ".app", ".graineopreviews", ".bundle"].iter().any(|e| name.ends_with(e))
}

/// List `dir` with its subfolders (breadth first: the files of the folder itself come first when the limit is hit).
pub fn list(dir: &Path) -> Listing {
    let mut out = Listing::default();
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, String, usize)> = std::collections::VecDeque::from([(dir.to_path_buf(), String::new(), 0)]);
    while let Some((folder, prefix, depth)) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&folder) else {
            if depth == 0 {
                out.unreadable = true;
            }
            continue;
        };
        let mut entries: Vec<(String, bool)> = rd.flatten().filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.file_type().ok()?.is_dir()))).collect();
        entries.sort();
        for (name, is_dir) in entries {
            if is_dir {
                if skipped(&name) {
                    continue;
                }
                if depth + 1 > MAX_DEPTH {
                    out.truncated = true;
                    continue;
                }
                out.folders += 1;
                queue.push_back((folder.join(&name), format!("{prefix}{name}/"), depth + 1));
            } else if name.starts_with('.') {
                continue;
            } else if library::format_from_extension(&name).is_some() {
                if out.names.len() >= MAX_FILES {
                    out.truncated = true;
                    return sorted(out);
                }
                out.names.push(format!("{prefix}{name}"));
            } else {
                out.others += 1;
            }
        }
    }
    sorted(out)
}

fn sorted(mut l: Listing) -> Listing {
    l.names.sort();
    l
}

impl Listing {
    /// Why the grid stays empty (`None`: there are photos).
    pub fn empty_reason(&self) -> Option<String> {
        if !self.names.is_empty() {
            return None;
        }
        Some(if self.unreadable {
            crate::tr("Der Ordner ist nicht lesbar (keine Berechtigung oder Laufwerk nicht verbunden).").to_string()
        } else if self.others > 0 {
            crate::trf!("Keine Fotos oder Videos. Gefunden: {a} andere Dateien in diesem Ordner und {b} Unterordnern, keine in einem Format, das graineo zeigt.", a = self.others, b = self.folders)
        } else if self.folders > 0 {
            crate::trf!("Keine Fotos oder Videos – auch nicht in den {a} Unterordnern (bis {b} Ebenen tief).", a = self.folders, b = MAX_DEPTH)
        } else {
            crate::tr("Der Ordner ist leer.").to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-view-folder-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Photos in subfolders are shown (relative path as name); hidden folders and packages are not
    /// looked into.
    #[test]
    fn photos_in_subfolders_are_listed() {
        let d = folder("sub");
        for f in ["Tag 1/a.jpg", "Tag 1/b.RAF", "Tag 2/tief/c.png", "notiz.txt", ".versteckt/x.jpg", "Fotos.photoslibrary/originals/y.jpg", "top.heic"] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let l = list(&d);
        assert_eq!(l.names, vec!["Tag 1/a.jpg", "Tag 1/b.RAF", "Tag 2/tief/c.png", "top.heic"]);
        assert_eq!((l.others, l.folders, l.truncated, l.unreadable), (1, 3, false, false));
        assert_eq!(l.empty_reason(), None);
        let _ = std::fs::remove_dir_all(d);
    }

    /// An empty result says why.
    #[test]
    fn empty_result_names_the_reason() {
        let d = folder("leer");
        assert_eq!(list(&d).empty_reason().as_deref(), Some("Der Ordner ist leer."));
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/text.pdf"), b"x").unwrap();
        let l = list(&d);
        assert_eq!((l.others, l.folders), (1, 2));
        assert!(l.empty_reason().unwrap().contains("1 andere Dateien in diesem Ordner und 2 Unterordnern"));
        std::fs::remove_file(d.join("a/b/text.pdf")).unwrap();
        assert!(list(&d).empty_reason().unwrap().contains("auch nicht in den 2 Unterordnern"));
        assert!(list(&d.join("gibt-es-nicht")).unreadable);
        let _ = std::fs::remove_dir_all(d);
    }

    /// Deeper than [`MAX_DEPTH`] is not looked into, and it is noted.
    #[test]
    fn depth_is_limited() {
        let d = folder("tief");
        let deep = d.join("1/2/3/4/5");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("x.jpg"), b"x").unwrap();
        std::fs::write(d.join("1/2/3/4/y.jpg"), b"x").unwrap();
        let l = list(&d);
        assert_eq!(l.names, vec!["1/2/3/4/y.jpg"]);
        assert!(l.truncated);
        let _ = std::fs::remove_dir_all(d);
    }
}
