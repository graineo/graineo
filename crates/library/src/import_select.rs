//! Selection rules of the import window, kept apart from the UI so that they can be tested.
//!
//! One rule for all numbers:
//! - The unit is the **photo** (one shot): RAW+JPEG of the same shot (same folder, same name without extension, case
//!   ignored) is one photo with two files. Every count of photos counts these; file counts are shown separately per type.
//! - The left column describes the **source** as it is: photos found, split into new / already there / probably there /
//!   identical (the four add up to the photos found), and the files per type. These numbers do not change with filters.
//! - Filters decide **what is imported**: a file type switched off and "RAW+JPEG: RAW only" remove files from the
//!   import; a file that is filtered out is neither shown, nor counted as chosen, nor imported. A photo none of whose
//!   files is left is hidden. The footer and the button count exactly what will be imported.
//! - **Already there**: every RAW of the photo is in the library (a photo without a RAW: every file). The JPEG half of a
//!   shot whose RAW was imported earlier (as "RAW only") does not make the photo new. Photos already there are not
//!   checked; a missing JPEG half can still be checked by hand (with "Schon importierte zeigen").

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crate::card_import::{pair_key, Candidate};

/// A JPEG-like half (JPEG, HEIC) that can stand next to a RAW of the same shot.
pub fn is_jpeg_like(format: &str) -> bool {
    matches!(format, "JPG" | "JPEG" | "HEIC" | "HEIF")
}

/// Group the files into photos (RAW+JPEG of one shot = one photo), in the order of the list; within a photo the RAW
/// comes first.
pub fn group(cand: &[Candidate]) -> Vec<Vec<usize>> {
    let mut index: HashMap<(PathBuf, String), usize> = HashMap::new();
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (i, k) in cand.iter().enumerate() {
        let key = pair_key(&k.path);
        match index.get(&key) {
            Some(&c) => out[c].push(i),
            None => {
                index.insert(key, out.len());
                out.push(vec![i]);
            }
        }
    }
    for m in &mut out {
        m.sort_by_key(|&i| (!crate::is_raw(&cand[i].format), cand[i].path.clone()));
    }
    out
}

/// What is left out of the import.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// file types switched off
    pub types_off: HashSet<String>,
    /// RAW+JPEG of one shot: import only the RAW (a JPEG without a RAW stays)
    pub raw_only: bool,
}

impl Filter {
    /// Is file `i` of the photo `members` part of the import (filters only; "already there" is not looked at)?
    pub fn includes(&self, cand: &[Candidate], members: &[usize], i: usize) -> bool {
        let f = cand[i].format.as_str();
        if self.types_off.contains(f) {
            return false;
        }
        !(self.raw_only && is_jpeg_like(f) && members.iter().any(|&m| crate::is_raw(&cand[m].format)))
    }

    /// Files of the photo that would be imported: passed by the filters and not already in the library.
    pub fn files(&self, cand: &[Candidate], members: &[usize]) -> Vec<usize> {
        members.iter().copied().filter(|&i| self.includes(cand, members, i) && !cand[i].duplicate).collect()
    }

    /// Is the photo shown at all (at least one of its files passes the filters)?
    pub fn shows(&self, cand: &[Candidate], members: &[usize]) -> bool {
        members.iter().any(|&i| self.includes(cand, members, i))
    }
}

/// Where a photo of the source stands with respect to the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    New,
    /// in the library (by content, whatever its name or folder)
    There,
    /// probably in the library: same size and name or capture time as a library file that could not be compared
    Probable,
    /// byte-identical copy of another photo of this source
    Identical,
}

/// Status of a photo (see the module rule: the RAW decides for RAW+JPEG).
pub fn status(cand: &[Candidate], members: &[usize]) -> Status {
    let raws: Vec<usize> = members.iter().copied().filter(|&i| crate::is_raw(&cand[i].format)).collect();
    let decisive: &[usize] = if raws.is_empty() { members } else { &raws };
    if !decisive.is_empty() && decisive.iter().all(|&i| cand[i].duplicate) {
        Status::There
    } else if members.iter().all(|&i| cand[i].identical || cand[i].duplicate) {
        Status::Identical
    } else if members.iter().any(|&i| cand[i].probable) {
        Status::Probable
    } else {
        Status::New
    }
}

/// Checked when the source has been read: only new photos (already there, probably there, identical: not).
pub fn preselected(s: Status) -> bool {
    s == Status::New
}

/// Photos and files of a set of photos.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub photos: usize,
    /// files per type, RAW types first, then by name
    pub files: Vec<(String, usize)>,
    pub bytes: u64,
}

impl Tally {
    pub fn file_count(&self) -> usize {
        self.files.iter().map(|(_, n)| n).sum()
    }
}

fn ordered(m: BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = m.into_iter().collect();
    v.sort_by_key(|(f, _)| (!crate::is_raw(f), f.clone()));
    v
}

/// What the import takes from these photos: photos with at least one file left, their files per type and bytes.
pub fn tally<'a>(cand: &[Candidate], photos: impl IntoIterator<Item = &'a [usize]>, filter: &Filter) -> Tally {
    let mut t = Tally::default();
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    for m in photos {
        let files = filter.files(cand, m);
        if files.is_empty() {
            continue;
        }
        t.photos += 1;
        for i in files {
            *types.entry(cand[i].format.clone()).or_default() += 1;
            t.bytes += cand[i].size;
        }
    }
    t.files = ordered(types);
    t
}

/// The source as it is (independent of filters): photos by status, files per type, size of what is new.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub photos: usize,
    pub new: usize,
    pub there: usize,
    pub probable: usize,
    pub identical: usize,
    /// bytes of the files not yet in the library of the new and probable photos
    pub new_bytes: u64,
    /// all files of the source per type
    pub files: Vec<(String, usize)>,
    /// photos with a RAW and a JPEG half
    pub pairs: usize,
}

pub fn overview(cand: &[Candidate], photos: &[Vec<usize>]) -> Overview {
    let mut o = Overview { photos: photos.len(), ..Default::default() };
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    for k in cand {
        *types.entry(k.format.clone()).or_default() += 1;
    }
    o.files = ordered(types);
    for m in photos {
        match status(cand, m) {
            Status::There => o.there += 1,
            Status::Identical => o.identical += 1,
            s => {
                if s == Status::Probable {
                    o.probable += 1;
                } else {
                    o.new += 1;
                }
                o.new_bytes += m.iter().filter(|&&i| !cand[i].duplicate).map(|&i| cand[i].size).sum::<u64>();
            }
        }
        if is_pair(cand, m) {
            o.pairs += 1;
        }
    }
    o
}

/// RAW and JPEG of the same shot.
pub fn is_pair(cand: &[Candidate], members: &[usize]) -> bool {
    members.iter().any(|&i| crate::is_raw(&cand[i].format)) && members.iter().any(|&i| is_jpeg_like(&cand[i].format))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_import::{complete, list};
    use crate::db::Db;

    fn k(path: &str, format: &str, size: u64) -> Candidate {
        Candidate { path: PathBuf::from(path), size, format: format.into(), ..Default::default() }
    }

    /// The card of the bug report in small: 3 shots as X3F+JPG (2 of the X3F already imported as RAW only), 1 JPEG
    /// alone, 1 X3F alone.
    fn card() -> Vec<Candidate> {
        let mut v = vec![
            k("/K/DCIM/102SIGMA/DP2M5482.X3F", "X3F", 48_000_000),
            k("/K/DCIM/102SIGMA/DP2M5482.JPG", "JPG", 6_000_000),
            k("/K/DCIM/102SIGMA/DP2M5483.X3F", "X3F", 47_000_000),
            k("/K/DCIM/102SIGMA/dp2m5483.jpg", "JPG", 6_100_000),
            k("/K/DCIM/102SIGMA/DP2M5501.X3F", "X3F", 35_000_000),
            k("/K/DCIM/102SIGMA/DP2M5501.JPG", "JPG", 300_000),
            k("/K/DCIM/102SIGMA/DP2M5600.JPG", "JPG", 5_000_000),
            k("/K/DCIM/102SIGMA/DP2M5601.X3F", "X3F", 44_000_000),
        ];
        v[0].duplicate = true;
        v[2].duplicate = true;
        v
    }

    #[test]
    fn pairs_ignore_case_and_put_the_raw_first() {
        let c = card();
        let g = group(&c);
        assert_eq!(g.len(), 5);
        assert_eq!(g[1], vec![2, 3], "DP2M5483.X3F + dp2m5483.jpg is one photo");
        let mut c2 = c.clone();
        c2.swap(0, 1);
        assert_eq!(group(&c2)[0], vec![1, 0], "the RAW comes first");
        // same name in another folder: another photo
        let other = vec![k("/K/DCIM/100/A.X3F", "X3F", 1), k("/K/DCIM/101/A.JPG", "JPG", 1)];
        assert_eq!(group(&other).len(), 2);
    }

    /// The error of the report: the X3F of a pair was in the library, the JPEG never was – the photo counted as new,
    /// "0 schon da", and 62 of 62 stayed checked.
    #[test]
    fn raw_in_library_makes_the_pair_already_there() {
        let c = card();
        let g = group(&c);
        let st: Vec<Status> = g.iter().map(|m| status(&c, m)).collect();
        assert_eq!(st, vec![Status::There, Status::There, Status::New, Status::New, Status::New]);
        let o = overview(&c, &g);
        assert_eq!((o.photos, o.new, o.there, o.probable, o.identical, o.pairs), (5, 3, 2, 0, 0, 3));
        assert_eq!(o.files, vec![("X3F".to_string(), 4), ("JPG".to_string(), 4)]);
        assert_eq!(o.new_bytes, 35_000_000 + 300_000 + 5_000_000 + 44_000_000);
        // a JPEG in the library does not make a pair with a new RAW "already there"
        let mut c2 = c.clone();
        c2[5].duplicate = true;
        assert_eq!(status(&c2, &g[2]), Status::New);
    }

    /// Filtered files are not shown, not chosen and not imported; all numbers count the same thing.
    #[test]
    fn filters_decide_what_is_imported() {
        let c = card();
        let g = group(&c);
        let new: Vec<&[usize]> = g.iter().filter(|m| preselected(status(&c, m))).map(|m| m.as_slice()).collect();
        let all = Filter::default();
        let t = tally(&c, new.iter().copied(), &all);
        assert_eq!(t.photos, 3);
        assert_eq!(t.files, vec![("X3F".to_string(), 2), ("JPG".to_string(), 2)]);
        // RAW only: the JPEG of a pair goes, the JPEG alone stays
        let raw_only = Filter { raw_only: true, ..Default::default() };
        let t = tally(&c, new.iter().copied(), &raw_only);
        assert_eq!((t.photos, t.files.clone()), (3, vec![("X3F".to_string(), 2), ("JPG".to_string(), 1)]));
        assert_eq!(t.bytes, 35_000_000 + 44_000_000 + 5_000_000);
        // JPG switched off: the photo that is only a JPEG disappears, nothing of type JPG is imported
        let no_jpg = Filter { types_off: ["JPG".to_string()].into(), ..Default::default() };
        assert!(!no_jpg.shows(&c, &g[3]));
        let t = tally(&c, new.iter().copied(), &no_jpg);
        assert_eq!((t.photos, t.files.clone()), (2, vec![("X3F".to_string(), 2)]));
        // a photo already there has nothing left to import with RAW only
        assert!(raw_only.files(&c, &g[0]).is_empty());
        // without RAW only its missing JPEG half can still be chosen by hand
        assert_eq!(all.files(&c, &g[0]), vec![1]);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-import-select-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn content(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    /// End to end with real files: the library holds the RAWs in date folders under other paths (as after an import),
    /// the card holds RAW+JPEG. Also the error cases: a truncated copy, the same name with another size, a library file
    /// without a stored partial checksum (read once, then stored), and the name in other case.
    #[test]
    fn card_against_library_by_content() {
        let dir = tmp("karte");
        let lib = dir.join("Bilder");
        std::fs::create_dir_all(lib.join("2013/2013-03-03")).unwrap();
        std::fs::create_dir_all(lib.join("2012/2012-01-01")).unwrap();
        std::fs::write(lib.join("2013/2013-03-03/DP2M5482.X3F"), content(1, 400_000)).unwrap();
        std::fs::write(lib.join("2013/2013-03-03/DP2M5483.X3F"), content(2, 410_000)).unwrap();
        std::fs::write(lib.join("2012/2012-01-01/dp2m5503.x3f"), content(3, 420_000)).unwrap();
        std::fs::write(lib.join("2012/2012-01-01/DP2M5504.X3F"), content(4, 430_000)).unwrap();
        std::fs::write(lib.join("2012/2012-01-01/DP2M5505.X3F"), content(5, 440_000)).unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        crate::reconcile::add_root(&db, &lib, &|_| {}).unwrap();
        // as in the real library: some partial checksums were never computed
        db.conn.execute("UPDATE file SET partial_hash = NULL WHERE name = 'DP2M5483.X3F'", []).unwrap();
        let card = dir.join("Karte/DCIM/102SIGMA");
        std::fs::create_dir_all(&card).unwrap();
        for (n, seed, len) in [(5482, 1, 400_000), (5483, 2, 410_000), (5503, 3, 420_000)] {
            std::fs::write(card.join(format!("DP2M{n}.X3F")), content(seed, len)).unwrap();
            std::fs::write(card.join(format!("DP2M{n}.JPG")), content(seed + 100, 50_000)).unwrap();
        }
        // truncated: same name, shorter → new
        std::fs::write(card.join("DP2M5504.X3F"), &content(4, 430_000)[..300_000]).unwrap();
        // same size, same name, other content → new (not "probably": the library file was compared)
        std::fs::write(card.join("DP2M5505.X3F"), content(6, 440_000)).unwrap();
        let mut c = list(&dir.join("Karte"));
        complete(&db, &mut c, &|_, _| {});
        let g = group(&c);
        let name = |m: &Vec<usize>| c[m[0]].path.file_stem().unwrap().to_string_lossy().into_owned();
        let st: HashMap<String, Status> = g.iter().map(|m| (name(m), status(&c, m))).collect();
        assert_eq!(st["DP2M5482"], Status::There);
        assert_eq!(st["DP2M5483"], Status::There, "missing partial checksum: read from the library file");
        assert_eq!(st["DP2M5503"], Status::There, "name in other case and other folder");
        assert_eq!(st["DP2M5504"], Status::New, "truncated copy");
        assert_eq!(st["DP2M5505"], Status::New, "same name and size, other content");
        let stored: Option<String> = db.conn.query_row("SELECT partial_hash FROM file WHERE name = 'DP2M5483.X3F'", [], |r| r.get(0)).unwrap();
        assert!(stored.is_some(), "the partial checksum read is remembered");
        let o = overview(&c, &g);
        assert_eq!((o.photos, o.there, o.new), (5, 3, 2));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
