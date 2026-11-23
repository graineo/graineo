//! Which dropped files and folders hold looks the look list can take. A `.cube` must be a complete, valid LUT
//! (header, size and every table row); an `.xmp` must be a Camera Raw look profile with a readable table (the same
//! check the look list uses). Sidecars of photos (an `.xmp` next to a photo of the same name, ratings and develop
//! settings only) are never looks; a folder counts only when it holds a look. Nothing is written or copied here.

use super::{cube, manager, read_profile, FilmProfile};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Kind of a recognised look file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookKind {
    /// a 3D/1D lookup table (`.cube`)
    Cube,
    /// a Camera Raw look profile (`.xmp` with `crs:PresetType="Look"` and an RGB table)
    Profile,
}

/// What one file is, as far as the look list is concerned.
#[derive(Clone, Debug)]
pub enum FileKind {
    /// a look; `own`: written by graineo (an exported `.cube`), no third-party licence applies
    Look { profile: FilmProfile, kind: LookKind, own: bool },
    /// the metadata sidecar of a photo (or another XMP without a look): silently left alone
    Sidecar,
    /// looks like a profile but cannot be used, with the reason
    Rejected(String),
    /// any other file
    Other,
}

/// A recognised look and what would be referenced for it (the dropped folder or the dropped file itself).
#[derive(Clone, Debug)]
pub struct FoundLook {
    pub profile: FilmProfile,
    pub kind: LookKind,
    pub own: bool,
    pub reference: PathBuf,
}

/// Result of looking at a drop.
#[derive(Clone, Debug, Default)]
pub struct Found {
    /// recognised looks (one per identifier, in the order found)
    pub looks: Vec<FoundLook>,
    /// what becomes a look source: dropped folders that hold a look, dropped look files
    pub references: Vec<PathBuf>,
    /// sidecars of photos and other XMPs without a look
    pub sidecars: usize,
    /// profile-like files that cannot be used, with the reason
    pub rejected: Vec<(PathBuf, String)>,
}

/// Folder levels and directory entries looked at per dropped folder (a drop of a huge tree stays quick).
const MAX_DEPTH: u32 = 6;
const MAX_ENTRIES: usize = 20_000;

fn extension(p: &Path) -> Option<String> {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Start of a file as text (at most `n` bytes).
fn head(p: &Path, n: u64) -> Option<String> {
    let mut s = Vec::new();
    fs::File::open(p).ok()?.take(n).read_to_end(&mut s).ok()?;
    Some(String::from_utf8_lossy(&s).into_owned())
}

/// Is there a photo (any other file) of the same name next to this XMP: `IMG_1.xmp` beside `IMG_1.CR3`, or
/// `IMG_1.CR3.xmp` beside `IMG_1.CR3`?
fn has_photo_beside(xmp: &Path) -> bool {
    let (Some(dir), Some(stem)) = (xmp.parent(), xmp.file_stem()) else { return false };
    if dir.join(stem).is_file() {
        return true;
    }
    let Ok(rd) = fs::read_dir(dir) else { return false };
    rd.flatten().take(10_000).any(|e| {
        let p = e.path();
        p.file_stem() == Some(stem) && !matches!(extension(&p).as_deref(), Some("xmp" | "cube" | "json")) && p.is_file()
    })
}

/// What `path` is for the look list (see [`FileKind`]).
pub fn classify(path: &Path) -> FileKind {
    match extension(path).as_deref() {
        Some("cube") => {
            let Some(profile) = read_profile(path) else {
                return FileKind::Rejected("Keine gültige LUT (Kopf oder Größe)".into());
            };
            let text = match manager::read_limited(path, manager::MAX_FILE) {
                Ok(b) => match String::from_utf8(b) {
                    Ok(t) => t,
                    Err(_) => return FileKind::Rejected("LUT ist keine Textdatei".into()),
                },
                Err(e) => return FileKind::Rejected(e),
            };
            if let Err(e) = cube::check(&text) {
                return FileKind::Rejected(format!("LUT fehlerhaft: {e}"));
            }
            let own = text.lines().take(4).any(|l| l.trim_start().starts_with('#') && l.contains("Created by graineo"));
            FileKind::Look { profile, kind: LookKind::Cube, own }
        }
        Some("xmp") => {
            if let Some(profile) = read_profile(path) {
                // the whole file and its table, as when the look is used
                let checked = manager::read_limited(path, manager::MAX_FILE).and_then(|b| String::from_utf8(b).map_err(|e| e.to_string())).and_then(|t| super::look_from_text(&t, &profile).map(|_| ()));
                return match checked {
                    Ok(()) => FileKind::Look { profile, kind: LookKind::Profile, own: false },
                    Err(_) => FileKind::Rejected("Look ohne lesbare Farbtabelle".into()),
                };
            }
            let h = head(path, 6000).unwrap_or_default();
            match h.find("crs:PresetType=\"") {
                // a Camera Raw preset or profile: say why it is not taken
                Some(i) => {
                    let rest = &h[i + 16..];
                    let kind = &rest[..rest.find('"').unwrap_or(0)];
                    let reason = if super::adobe_origin(path) || h.to_ascii_lowercase().contains("crs:copyright=\"© adobe") || h.contains("crs:Copyright=\"Adobe") {
                        "Profil von Adobe (nicht unterstützt)"
                    } else if kind != "Look" {
                        "Vorgabe ohne Look-Tabelle (nicht unterstützt)"
                    } else {
                        "Look ohne lesbare Farbtabelle"
                    };
                    FileKind::Rejected(reason.into())
                }
                // ratings, keywords, develop settings of a photo: a sidecar, never a look
                None if has_photo_beside(path) || h.contains("x:xmpmeta") || h.contains("x:xapmeta") => FileKind::Sidecar,
                None => FileKind::Rejected("Keine XMP-Datei mit Look".into()),
            }
        }
        _ => FileKind::Other,
    }
}

/// Look at dropped files and folders (folders down to a few levels): which looks they hold and what would be
/// referenced. Hidden files and symbolic links are skipped.
pub fn detect(paths: &[PathBuf]) -> Found {
    let mut found = Found::default();
    let mut ids: HashSet<String> = HashSet::new();
    let mut take = |found: &mut Found, p: &Path, reference: &Path| -> bool {
        match classify(p) {
            FileKind::Look { profile, kind, own } => {
                if ids.insert(profile.uuid.clone()) {
                    found.looks.push(FoundLook { profile, kind, own, reference: reference.to_path_buf() });
                }
                true
            }
            FileKind::Sidecar => {
                found.sidecars += 1;
                false
            }
            FileKind::Rejected(r) => {
                found.rejected.push((p.to_path_buf(), r));
                false
            }
            FileKind::Other => false,
        }
    };
    for p in paths {
        let Ok(meta) = fs::symlink_metadata(p) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_file() {
            if take(&mut found, p, p) && !found.references.contains(p) {
                found.references.push(p.clone());
            }
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        let mut any = false;
        let mut budget = MAX_ENTRIES;
        let mut stack = vec![(p.clone(), 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            let mut subs = Vec::new();
            for e in entries {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                if e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                    continue;
                }
                let Ok(m) = fs::symlink_metadata(&e) else { continue };
                if m.file_type().is_symlink() {
                    continue;
                }
                if m.is_dir() {
                    if depth < MAX_DEPTH {
                        subs.push(e);
                    }
                } else if m.is_file() && matches!(extension(&e).as_deref(), Some("xmp" | "cube")) {
                    any |= take(&mut found, &e, p);
                }
            }
            stack.extend(subs.into_iter().rev().map(|d| (d, depth + 1)));
        }
        if any && !found.references.contains(p) {
            found.references.push(p.clone());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-look-detect-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn cube_text(n: usize, rows: usize) -> String {
        let mut t = format!("# Created by graineo\nTITLE \"t\"\nLUT_3D_SIZE {n}\n");
        for i in 0..rows {
            let v = (i % n) as f32 / (n - 1) as f32;
            t += &format!("{v} {v} {v}\n");
        }
        t
    }

    /// A Camera Raw look profile with a small valid table (same shape as the manager tests use).
    fn look_xmp(id: &str) -> String {
        use std::io::Write;
        let mut raw = Vec::new();
        for w in [1u32, 1, 3, 2] {
            raw.extend(w.to_le_bytes());
        }
        raw.extend(std::iter::repeat_n(0u8, 8 * 3 * 2));
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&raw).unwrap();
        let mut b = (raw.len() as u32).to_le_bytes().to_vec();
        b.extend(z.finish().unwrap());
        while b.len() % 4 != 0 {
            b.push(0);
        }
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><rdf:RDF><rdf:Description crs:PresetType="Look" crs:UUID="{id}" crs:SupportsAmount="True" crs:RGBTable="Synthetic" crs:Table_Synthetic="{}"><crs:Name><rdf:Alt><rdf:li>Test Look</rdf:li></rdf:Alt></crs:Name><crs:Group><rdf:Alt><rdf:li>Test Pack</rdf:li></rdf:Alt></crs:Group></rdf:Description></rdf:RDF></x:xmpmeta>"#,
            super::super::base85_encode(&b)
        )
    }

    const SIDECAR: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>";

    #[test]
    fn valid_cube_is_a_look_and_own_export_is_marked() {
        let d = scratch("cube");
        let p = d.join("Warm.cube");
        fs::write(&p, cube_text(17, 17 * 17 * 17)).unwrap();
        match classify(&p) {
            FileKind::Look { kind, own, profile } => {
                assert_eq!(kind, LookKind::Cube);
                assert!(own);
                assert_eq!(profile.name, "Warm");
            }
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn broken_cubes_are_rejected() {
        let d = scratch("broken");
        // header fine, rows missing
        let short = d.join("short.cube");
        fs::write(&short, cube_text(17, 100)).unwrap();
        assert!(matches!(classify(&short), FileKind::Rejected(r) if r.starts_with("LUT fehlerhaft")));
        // no size in the header
        let none = d.join("none.cube");
        fs::write(&none, "TITLE \"x\"\n0 0 0\n").unwrap();
        assert!(matches!(classify(&none), FileKind::Rejected(_)));
        // a size out of range
        let big = d.join("big.cube");
        fs::write(&big, "LUT_3D_SIZE 300\n").unwrap();
        assert!(matches!(classify(&big), FileKind::Rejected(_)));
        // a number that is no number
        let bad = d.join("bad.cube");
        fs::write(&bad, cube_text(17, 17 * 17 * 17).replacen("0 0 0", "0 x 0", 1)).unwrap();
        assert!(matches!(classify(&bad), FileKind::Rejected(_)));
    }

    #[test]
    fn photo_sidecar_is_never_a_look() {
        let d = scratch("sidecar");
        fs::write(d.join("IMG_1.CR3"), b"raw").unwrap();
        let x = d.join("IMG_1.xmp");
        fs::write(&x, SIDECAR).unwrap();
        assert!(matches!(classify(&x), FileKind::Sidecar));
        // darktable style: IMG_2.CR3.xmp
        fs::write(d.join("IMG_2.CR3"), b"raw").unwrap();
        let y = d.join("IMG_2.CR3.xmp");
        fs::write(&y, SIDECAR).unwrap();
        assert!(matches!(classify(&y), FileKind::Sidecar));
        // even without the photo (moved away) metadata is not a look
        let z = d.join("lonely.xmp");
        fs::write(&z, SIDECAR).unwrap();
        assert!(matches!(classify(&z), FileKind::Sidecar));
        let f = detect(&[d.clone()]);
        assert!(f.looks.is_empty() && f.references.is_empty());
        assert_eq!(f.sidecars, 3);
    }

    #[test]
    fn profile_xmp_is_a_look_and_preset_without_table_is_not() {
        let d = scratch("profile");
        let p = d.join("Look A.xmp");
        fs::write(&p, look_xmp("11111111-2222-3333-4444-555555555555")).unwrap();
        match classify(&p) {
            FileKind::Look { kind, own, profile } => {
                assert_eq!(kind, LookKind::Profile);
                assert!(!own);
                assert_eq!(profile.name, "Test Look");
                assert_eq!(profile.group, "Test Pack");
            }
            k => panic!("{k:?}"),
        }
        let broken = d.join("Broken.xmp");
        fs::write(&broken, look_xmp("22222222-2222-3333-4444-555555555555").replace("crs:Table_Synthetic=\"", "crs:Table_Synthetic=\"!!")).unwrap();
        assert!(matches!(classify(&broken), FileKind::Rejected(r) if r.contains("Farbtabelle")));
        let preset = d.join("Preset.xmp");
        fs::write(&preset, "<x:xmpmeta><rdf:Description crs:PresetType=\"Normal\" crs:UUID=\"AB\" crs:Exposure2012=\"+0.5\"/></x:xmpmeta>").unwrap();
        assert!(matches!(classify(&preset), FileKind::Rejected(r) if r.contains("Vorgabe")));
    }

    #[test]
    fn mixed_folders_count_only_with_looks() {
        let root = scratch("mixed");
        // photos with sidecars only: not a look folder
        let photos = root.join("photos");
        fs::create_dir_all(&photos).unwrap();
        fs::write(photos.join("A.RAF"), b"raw").unwrap();
        fs::write(photos.join("A.xmp"), SIDECAR).unwrap();
        // a look pack in a subfolder, next to a broken LUT and a sidecar
        let looks = root.join("looks");
        let sub = looks.join("pack");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("One.cube"), cube_text(17, 17 * 17 * 17)).unwrap();
        fs::write(sub.join("Two.xmp"), look_xmp("AAAAAAAA-2222-3333-4444-555555555555")).unwrap();
        fs::write(sub.join("Broken.cube"), cube_text(17, 3)).unwrap();
        fs::write(sub.join("B.JPG"), b"jpg").unwrap();
        fs::write(sub.join("B.xmp"), SIDECAR).unwrap();
        let f = detect(&[photos.clone(), looks.clone()]);
        assert_eq!(f.references, vec![looks.clone()]);
        assert_eq!(f.looks.len(), 2);
        assert!(f.looks.iter().all(|l| l.reference == looks));
        assert_eq!(f.sidecars, 2);
        assert_eq!(f.rejected.len(), 1);
        // a dropped look file is referenced by itself; the same look twice counts once
        let one = sub.join("One.cube");
        let f = detect(&[one.clone(), one.clone()]);
        assert_eq!(f.references, vec![one]);
        assert_eq!(f.looks.len(), 1);
        // nothing at all
        assert!(detect(&[photos]).looks.is_empty());
    }
}
