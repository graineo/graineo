//! Camera looks by name (`camera/style`), wherever they come from: the user's own looks in the data folder first,
//! then the bundled ones ([`raw::looks::Look::built_in`]).
//!
//! An own look is a file `<data folder>/looks/<camera>/<style>.x3look` (the folder of the learned looks, see
//! [`crate::learn::dir`]) that is **not** a learned look (not listed in `looks/learned.json`). It counts like a
//! bundled look everywhere: it is offered for photos of its camera, chosen automatically as "as shot" for its picture
//! style, applied by name. With the same name as a bundled look, the own look wins (whoever has their own look gets
//! it). Learned looks stay what they are (the standard development of their camera and style, [`crate::learn`]);
//! this module never writes anything.
//!
//! The folder is read once per state of the learned looks ([`crate::learn::generation`], raised by every change made
//! in the app and by [`crate::learn::set_dir`]); a file placed there by hand is seen after the next start. A file
//! that is not a valid look (cut, damaged, too large) is skipped and affects nothing else; an unreadable folder
//! means no own looks.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use raw::looks::Look;

use crate::learn;

/// Largest look file read from the data folder (a 65³ table has about 1.7 MB).
const MAX_FILE: u64 = 4 << 20;

/// Where a look of a given name comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// a file in the data folder (not learned)
    Own,
    /// bundled with the app
    Bundled,
}

type OwnLooks = Arc<Vec<(String, Arc<Look>)>>;
static CACHE: Mutex<Option<(u64, PathBuf, OwnLooks)>> = Mutex::new(None);

/// The own looks in the data folder (sorted by name), read again only after a change of the learned looks.
pub fn own() -> OwnLooks {
    let generation = learn::generation();
    let dir = learn::dir();
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((g, d, l)) = c.as_ref() {
        if *g == generation && *d == dir {
            return l.clone();
        }
    }
    let learned: HashSet<String> = learn::read_state().looks.into_iter().map(|e| e.key).collect();
    let list: OwnLooks = Arc::new(scan(&dir, &learned));
    *c = Some((generation, dir, list.clone()));
    list
}

/// The look files `<dir>/<camera>/<style>.x3look` whose key is valid, not in `learned` and whose file is a valid look.
fn scan(dir: &Path, learned: &HashSet<String>) -> Vec<(String, Arc<Look>)> {
    let mut out = Vec::new();
    for cam in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(camera) = cam.file_name().to_str().map(str::to_string) else { continue };
        if !cam.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(cam.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("x3look") {
                continue;
            }
            let Some(style) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let key = format!("{camera}/{style}");
            if !learn::valid_key(&key) || learned.contains(&key) {
                continue;
            }
            if f.metadata().map_or(true, |m| !m.is_file() || m.len() > MAX_FILE) {
                eprintln!("own look {}: not a file or too large, skipped", p.display());
                continue;
            }
            match Look::file(&p) {
                Some(mut l) => {
                    // the place names the look (as for learned looks), whatever name the file carries
                    l.name = key.clone();
                    out.push((key, Arc::new(l)));
                }
                None => eprintln!("own look {}: not a valid look file (X3LOOK1), skipped", p.display()),
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The look of `name` (`camera/style`): the own look if there is one, otherwise the bundled one.
pub fn find(name: &str) -> Option<Arc<Look>> {
    if let Some((_, l)) = own().iter().find(|(n, _)| n == name) {
        return Some(l.clone());
    }
    Look::built_in(name).map(Arc::new)
}

/// Where the look `name` comes from (`None`: no such own or bundled look).
pub fn source(name: &str) -> Option<Source> {
    if own().iter().any(|(n, _)| n == name) {
        Some(Source::Own)
    } else if Look::names().contains(&name) {
        Some(Source::Bundled)
    } else {
        None
    }
}

/// Names of all own and bundled looks, sorted, each once.
pub fn names() -> Vec<String> {
    let mut v: Vec<String> = own().iter().map(|(n, _)| n.clone()).collect();
    v.extend(Look::names().into_iter().map(str::to_string));
    v.sort();
    v.dedup();
    v
}

/// Names of the own and bundled looks of one camera (short name, e.g. `eos-rp`).
pub fn names_for_camera(short: &str) -> Vec<String> {
    names().into_iter().filter(|n| n.split('/').next() == Some(short)).collect()
}

/// The look for the picture style stored in the file ("as shot").
pub fn for_picture_style(b: &raw::picture_style::PictureStyle) -> Option<Arc<Look>> {
    find(&format!("{}/{}", b.camera_short(), b.style))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(name: &str, v: f32) -> Look {
        let n = 3;
        Look::from_table(name, "{}", n, vec![[v, 0.0, -v]; n * n * n]).unwrap()
    }

    fn write(dir: &Path, key: &str, l: &Look) -> PathBuf {
        let p = learn::look_path(key);
        assert!(p.starts_with(dir));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, l.to_bytes().unwrap()).unwrap();
        p
    }

    /// Own looks are found by name, win over bundled ones of the same name, are offered with the camera's looks, and
    /// a broken file next to them changes nothing; learned looks stay learned looks.
    #[test]
    fn own_looks_are_found_first_and_broken_files_change_nothing() {
        let _s = learn::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("graineo-own-looks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        learn::set_dir(Some(d.clone()));
        let bundled = Look::names()[0];
        let bundled_look = Look::built_in(bundled).unwrap();
        assert_eq!(source(bundled), Some(Source::Bundled));
        assert!(source("eos-rp/user1-pc").is_none(), "personal styles are no longer bundled");

        // an own look with a name of its own, and one with a bundled name (wins); the file names a different look
        write(&d, "eos-rp/user1-pc", &look("something/else", 0.02));
        write(&d, bundled, &look(bundled, 0.03));
        // broken files: cut, wrong magic, empty; a file in `replaced/`; a file with an invalid name
        let cut = write(&d, "eos-rp/user3-pc", &look("eos-rp/user3-pc", 0.01));
        let bytes = std::fs::read(&cut).unwrap();
        std::fs::write(&cut, &bytes[..bytes.len() - 7]).unwrap();
        std::fs::write(d.join("eos-rp").join("junk.x3look"), b"not a look").unwrap();
        std::fs::write(d.join("eos-rp").join("empty.x3look"), b"").unwrap();
        std::fs::create_dir_all(d.join("eos-rp").join("replaced")).unwrap();
        std::fs::write(d.join("eos-rp").join("replaced").join("user9-1.x3look"), look("eos-rp/user9", 0.0).to_bytes().unwrap()).unwrap();
        std::fs::write(d.join("eos-rp").join(".hidden.x3look"), look("eos-rp/x", 0.0).to_bytes().unwrap()).unwrap();
        learn::set_dir(Some(d.clone()));

        let found = own();
        assert_eq!(found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), {
            let mut v = vec![bundled, "eos-rp/user1-pc"];
            v.sort();
            v
        });
        let u = find("eos-rp/user1-pc").expect("own look found");
        assert_eq!(u.name, "eos-rp/user1-pc");
        assert_eq!(u.table().1[0], [0.02, 0.0, -0.02].map(|x: f32| (x * 8192.0).round() / 8192.0));
        assert_eq!(source("eos-rp/user1-pc"), Some(Source::Own));
        assert!(find("eos-rp/user3-pc").is_none() && find("eos-rp/junk").is_none() && find("eos-rp/user9").is_none());
        // the own look wins over the bundled one of the same name
        assert_eq!(source(bundled), Some(Source::Own));
        assert_ne!(find(bundled).unwrap().table().1, bundled_look.table().1);
        assert!(names().iter().filter(|n| *n == bundled).count() == 1);
        assert_eq!(names_for_camera("eos-rp"), ["eos-rp/user1-pc"]);
        let ps = raw::picture_style::PictureStyle { maker: "Canon".into(), model: "Canon EOS RP".into(), style: "user1-pc".into(), ..Default::default() };
        assert_eq!(ps.camera_short(), "eos-rp");
        assert_eq!(for_picture_style(&ps).unwrap().name, "eos-rp/user1-pc");

        // a learned look of the same name is no own look (it stays the learned standard), nothing is overwritten:
        // installing moves the own file to `replaced/`
        learn::install("eos-rp/user1-pc", &look("eos-rp/user1-pc", 0.05)).unwrap();
        assert!(source("eos-rp/user1-pc").is_none());
        assert_eq!(std::fs::read_dir(d.join("eos-rp").join("replaced")).unwrap().count(), 2);
        assert!(learn::active("eos-rp/user1-pc").is_some());

        // unreadable / missing folder: no own looks, bundled ones as before
        learn::set_dir(Some(d.join("missing")));
        assert!(own().is_empty());
        assert_eq!(source(bundled), Some(Source::Bundled));
        learn::set_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A damaged `learned.json` does not turn own looks into learned ones (the recovered state takes only files that
    /// carry the marks of a learned, derived or imported look).
    #[test]
    fn damaged_state_keeps_own_looks_own() {
        let _s = learn::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("graineo-own-looks-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        learn::set_dir(Some(d.clone()));
        write(&d, "cam/user1", &look("cam/user1", 0.01));
        let n = 3;
        let learned = Look::from_table("cam/std", r#"{"learned":true}"#, n, vec![[0.01; 3]; n * n * n]).unwrap();
        write(&d, "cam/std", &learned);
        std::fs::write(d.join("learned.json"), b"{broken").unwrap();
        learn::set_dir(Some(d.clone()));
        assert_eq!(learn::read_state().looks.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["cam/std"]);
        assert!(learn::active("cam/std").is_some() && learn::active("cam/user1").is_none());
        assert_eq!(own().iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["cam/user1"]);
        learn::set_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
