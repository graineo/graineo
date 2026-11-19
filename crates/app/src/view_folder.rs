//! "View folder" (⌘O): which files of a folder outside the library are shown. By default subfolders are included at
//! any depth (the photos of a card or an export often lie a few levels down); the grid then shows the relative path
//! as name. Without subfolders only the photos directly in the folder are listed. The tree on the left reads one
//! level at a time ([`children`]). If nothing is shown, [`Listing::empty_reason`] says why instead of an empty grid.

use std::path::Path;

/// At most this many photos (a whole disk chosen by mistake must not block the window).
pub const MAX_FILES: usize = 5000;
/// At most this many subfolders are looked into when subfolders are included (a disk with few photos and very many
/// folders must not block the window either).
pub const MAX_FOLDERS: usize = 10_000;

/// Files of a viewed folder.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Listing {
    /// photos and videos graineo can show, relative to the folder (`a.jpg`, `Tag 1/b.raf`), sorted
    pub names: Vec<String>,
    /// other files (documents, sidecars …)
    pub others: usize,
    /// subfolders looked into (without subfolders: the direct subfolders, not looked into)
    pub folders: usize,
    /// stopped at [`MAX_FILES`]: more photos than shown
    pub truncated: bool,
    /// stopped at [`MAX_FOLDERS`]: not every subfolder was looked into
    pub folders_capped: bool,
    /// the folder itself could not be read (no permission, drive gone)
    pub unreadable: bool,
    /// subfolders were included
    pub recursive: bool,
}

/// A subfolder in the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    pub name: String,
    /// photos directly in it
    pub photos: usize,
    /// it has subfolders itself (the tree shows a triangle)
    pub has_dirs: bool,
}

/// Folders never looked into: hidden ones, packages of other programs (Apple Photos library, Lightroom previews,
/// apps) and graineo's own folder.
fn skipped(name: &str) -> bool {
    name.starts_with('.') || [".photoslibrary", ".lrdata", ".app", ".graineopreviews", ".bundle"].iter().any(|e| name.ends_with(e))
}

/// Entries of a folder: (name, is a folder), sorted; `None`: not readable.
fn entries(dir: &Path) -> Option<Vec<(String, bool)>> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut v: Vec<(String, bool)> = rd.flatten().filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.file_type().ok()?.is_dir()))).collect();
    v.sort();
    Some(v)
}

/// List `dir`; with `recursive` its subfolders at any depth (breadth first: the files of the folder itself come
/// first when the limit is hit).
pub fn list(dir: &Path, recursive: bool) -> Listing {
    let mut out = Listing { recursive, ..Default::default() };
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, String)> = std::collections::VecDeque::from([(dir.to_path_buf(), String::new())]);
    let mut first = true;
    while let Some((folder, prefix)) = queue.pop_front() {
        let Some(entries) = entries(&folder) else {
            if first {
                out.unreadable = true;
            }
            first = false;
            continue;
        };
        first = false;
        for (name, is_dir) in entries {
            if is_dir {
                if skipped(&name) {
                    continue;
                }
                if recursive && out.folders >= MAX_FOLDERS {
                    out.folders_capped = true;
                    continue;
                }
                out.folders += 1;
                if recursive {
                    queue.push_back((folder.join(&name), format!("{prefix}{name}/")));
                }
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

/// The subfolders of `dir` for the tree (one level; each one is read once to count its photos and to know whether
/// it has subfolders). `None`: not readable.
pub fn children(dir: &Path) -> Option<Vec<Child>> {
    let v = entries(dir)?;
    Some(
        v.into_iter()
            .filter(|(n, d)| *d && !skipped(n))
            .map(|(name, _)| {
                let inner = entries(&dir.join(&name)).unwrap_or_default();
                let photos = inner.iter().filter(|(n, d)| !d && !n.starts_with('.') && library::format_from_extension(n).is_some()).count();
                let has_dirs = inner.iter().any(|(n, d)| *d && !skipped(n));
                Child { name, photos, has_dirs }
            })
            .collect(),
    )
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
        } else if !self.recursive && self.folders > 0 {
            crate::trf!("Keine Fotos direkt in diesem Ordner. „Unterordner einbeziehen“ zeigt auch die {a} Unterordner.", a = self.folders)
        } else if self.others > 0 {
            crate::trf!("Keine Fotos oder Videos. Gefunden: {a} andere Dateien in diesem Ordner und {b} Unterordnern, keine in einem Format, das graineo zeigt.", a = self.others, b = self.folders)
        } else if self.folders > 0 {
            crate::trf!("Keine Fotos oder Videos – auch nicht in den {a} Unterordnern.", a = self.folders)
        } else {
            crate::tr("Der Ordner ist leer.").to_string()
        })
    }

    /// Short note when not everything is shown (`None`: everything is).
    pub fn limit_note(&self) -> Option<String> {
        if self.truncated {
            Some(crate::trf!("Nur die ersten {a} Fotos gezeigt", a = crate::dialog::number(MAX_FILES)))
        } else if self.folders_capped {
            Some(crate::trf!("Nur die ersten {a} Unterordner durchsucht", a = crate::dialog::number(MAX_FOLDERS)))
        } else {
            None
        }
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

    fn files(d: &Path, names: &[&str]) {
        for f in names {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
    }

    /// Photos in subfolders are shown (relative path as name); hidden folders and packages are not
    /// looked into.
    #[test]
    fn photos_in_subfolders_are_listed() {
        let d = folder("sub");
        files(&d, &["Tag 1/a.jpg", "Tag 1/b.RAF", "Tag 2/tief/c.png", "notiz.txt", ".versteckt/x.jpg", "Fotos.photoslibrary/originals/y.jpg", "top.heic"]);
        let l = list(&d, true);
        assert_eq!(l.names, vec!["Tag 1/a.jpg", "Tag 1/b.RAF", "Tag 2/tief/c.png", "top.heic"]);
        assert_eq!((l.others, l.folders, l.truncated, l.unreadable), (1, 3, false, false));
        assert_eq!(l.empty_reason(), None);
        assert_eq!(l.limit_note(), None);
        // only this folder: the photos directly in it, the subfolders are counted, not looked into
        let l = list(&d, false);
        assert_eq!(l.names, vec!["top.heic"]);
        assert_eq!((l.others, l.folders), (1, 2));
        let _ = std::fs::remove_dir_all(d);
    }

    /// An empty result says why.
    #[test]
    fn empty_result_names_the_reason() {
        let d = folder("leer");
        assert_eq!(list(&d, true).empty_reason().as_deref(), Some("Der Ordner ist leer."));
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/text.pdf"), b"x").unwrap();
        let l = list(&d, true);
        assert_eq!((l.others, l.folders), (1, 2));
        assert!(l.empty_reason().unwrap().contains("1 andere Dateien in diesem Ordner und 2 Unterordnern"));
        std::fs::remove_file(d.join("a/b/text.pdf")).unwrap();
        assert!(list(&d, true).empty_reason().unwrap().contains("auch nicht in den 2 Unterordnern"));
        std::fs::write(d.join("a/b/x.jpg"), b"x").unwrap();
        assert!(list(&d, false).empty_reason().unwrap().contains("Unterordner einbeziehen"), "only this folder: says where the photos are");
        assert!(list(&d.join("gibt-es-nicht"), true).unreadable);
        let _ = std::fs::remove_dir_all(d);
    }

    /// No depth limit any more: photos ten levels down are found.
    #[test]
    fn any_depth() {
        let d = folder("tief");
        let deep = d.join("1/2/3/4/5/6/7/8/9/10");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("x.jpg"), b"x").unwrap();
        let l = list(&d, true);
        assert_eq!(l.names, vec!["1/2/3/4/5/6/7/8/9/10/x.jpg"]);
        assert!(!l.truncated);
        let _ = std::fs::remove_dir_all(d);
    }

    /// More than [`MAX_FILES`] photos: the first ones are shown and it is said.
    #[test]
    fn limit_is_reported() {
        let d = folder("limit");
        let src = d.join("seed.jpg");
        std::fs::write(&src, b"x").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for i in 0..MAX_FILES + 3 {
            let p = d.join("sub").join(format!("f{i:05}.jpg"));
            if std::fs::hard_link(&src, &p).is_err() {
                std::fs::write(&p, b"x").unwrap();
            }
        }
        let l = list(&d, true);
        assert_eq!(l.names.len(), MAX_FILES);
        assert!(l.truncated);
        assert!(l.limit_note().unwrap().contains("5"), "{:?}", l.limit_note());
        // the folder itself has one photo: without subfolders nothing is cut
        let l = list(&d, false);
        assert_eq!((l.names.len(), l.truncated), (1, false));
        // a subfolder clicked in the tree is limited in the same way
        let l = list(&d.join("sub"), false);
        assert!(l.truncated && l.limit_note().is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    /// The tree reads one level: subfolders with their photo count and whether they have subfolders.
    #[test]
    fn children_one_level() {
        let d = folder("kinder");
        files(&d, &["A/a.jpg", "A/b.jpg", "A/inner/c.jpg", "B/notes.txt", ".hidden/x.jpg", "Lib.photoslibrary/y.jpg", "top.jpg"]);
        let c = children(&d).unwrap();
        assert_eq!(c, vec![Child { name: "A".into(), photos: 2, has_dirs: true }, Child { name: "B".into(), photos: 0, has_dirs: false }]);
        assert_eq!(children(&d.join("A")).unwrap(), vec![Child { name: "inner".into(), photos: 1, has_dirs: false }]);
        assert!(children(&d.join("missing")).is_none());
        let _ = std::fs::remove_dir_all(d);
    }
}
