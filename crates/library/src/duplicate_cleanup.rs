//! Cleaning up duplicates: which photo of a group stays, why, and removing the others safely.
//!
//! The groups come from [`crate::duplicates`] ("identical": same size, capture time and partial checksum;
//! "similar": image vectors). Here each group gets its members with everything the decision needs and a
//! suggestion which member stays, decided by fixed, visible criteria ([`Criterion`], in this order):
//!
//! 1. reachable (file present) before missing or offline
//! 2. own development or virtual copies
//! 3. rating, flag, label, keywords, collections, persons
//! 4. original RAW before JPEG and other formats
//! 5. higher resolution
//! 6. larger file
//! 7. a main folder before a folder that looks like a backup, copy or export
//! 8. older (imported earlier, else older file)
//!
//! Safety rules:
//! - Only "identical" groups whose content was confirmed get a removal suggestion; "similar" ones (series!) never –
//!   there the user marks by hand.
//! - Before removing an "identical" photo the WHOLE content of both files is compared (SHA-256).
//! - Photos from Apple Photos are never removed (that library is read-only for graineo); they are left out of the
//!   groups. Photos with an own development or virtual copies, missing or offline files are never removed either.
//! - Ratings, flag, label, caption, keywords, persons and collections of a removed photo the staying one lacks are
//!   carried over first (only added, never overwritten; the same as for "RAW+JPEG twice").
//! - Files only go into the Trash (on a network drive into its trash folder), collected in blocks
//!   ([`crate::trash_batch`]); the library entries go with a [`crate::manage::Snapshot`], so ⌘Z brings them back.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::OptionalExtension;

use crate::db::Db;
use crate::duplicates::{Group, Kind};
use crate::manage::Snapshot;
use crate::pairs;
use crate::trash_batch::{self, Entry, Targets};

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// One photo of a group with what the decision needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Member {
    pub image: i64,
    pub file: i64,
    pub path: PathBuf,
    pub name: String,
    pub format: String,
    pub size: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub capture: Option<String>,
    /// 0 present, 1 missing, 2 drive not connected
    pub status: u8,
    pub stars: u8,
    pub flag: i8,
    pub label: u8,
    pub keywords: usize,
    pub collections: usize,
    pub persons: usize,
    /// own development (graineo) or marked as edited by Lightroom
    pub developed: bool,
    pub copies: usize,
    /// origin of the root folder ('lightroom', 'own', …)
    pub origin: String,
    pub network: bool,
    /// start of the import run (Unix seconds)
    pub imported: Option<i64>,
    /// modification time of the file (Unix seconds)
    pub mtime: Option<i64>,
}

impl Member {
    pub fn pixels(&self) -> i64 {
        self.width.unwrap_or(0).max(0) * self.height.unwrap_or(0).max(0)
    }
    /// Rating, flag, label, keywords, collections or persons.
    pub fn has_data(&self) -> bool {
        self.stars > 0 || self.flag != 0 || self.label > 0 || self.keywords > 0 || self.collections > 0 || self.persons > 0
    }
    pub fn own_development(&self) -> bool {
        self.developed || self.copies > 0
    }
    pub fn is_raw(&self) -> bool {
        crate::is_raw(&self.format)
    }
    /// Does the folder look like a backup, copy, export or trash folder?
    pub fn secondary_folder(&self) -> bool {
        // only the folders next to the photo (system folders such as /tmp further up say nothing)
        const WORDS: [&str; 10] = ["backup", "sicherung", "kopie", "copy", "copies", "export", "duplikat", "duplicate", "trash", "papierkorb"];
        self.path.parent().is_some_and(|p| {
            p.components().rev().take(2).any(|c| {
                let s = c.as_os_str().to_string_lossy().to_lowercase();
                WORDS.iter().any(|w| s.contains(w))
            })
        })
    }
    fn age(&self) -> i64 {
        self.imported.or(self.mtime).unwrap_or(i64::MAX)
    }
    /// Why this photo can never be removed here (`None`: it may).
    pub fn not_removable(&self) -> Option<&'static str> {
        if self.status != 0 {
            Some("Datei fehlt oder Laufwerk nicht verbunden")
        } else if self.own_development() {
            Some("hat eine eigene Entwicklung oder virtuelle Kopien")
        } else {
            None
        }
    }
}

/// The criteria for "which one stays", in their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Criterion {
    Reachable,
    Developed,
    Data,
    Raw,
    Resolution,
    FileSize,
    MainFolder,
    Older,
}

pub const ORDER: [Criterion; 8] = [Criterion::Reachable, Criterion::Developed, Criterion::Data, Criterion::Raw, Criterion::Resolution, Criterion::FileSize, Criterion::MainFolder, Criterion::Older];

/// Less = `a` is the better one to keep.
fn compare(a: &Member, b: &Member, c: Criterion) -> Ordering {
    match c {
        Criterion::Reachable => (a.status != 0).cmp(&(b.status != 0)),
        Criterion::Developed => b.own_development().cmp(&a.own_development()),
        Criterion::Data => b.has_data().cmp(&a.has_data()),
        Criterion::Raw => b.is_raw().cmp(&a.is_raw()),
        Criterion::Resolution => b.pixels().cmp(&a.pixels()),
        Criterion::FileSize => b.size.cmp(&a.size),
        Criterion::MainFolder => a.secondary_folder().cmp(&b.secondary_folder()),
        Criterion::Older => a.age().cmp(&b.age()),
    }
}

/// Order of the members, best to keep first (ties: shorter path, then the older entry).
pub fn rank(members: &[Member]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..members.len()).collect();
    idx.sort_by(|&x, &y| {
        let (a, b) = (&members[x], &members[y]);
        ORDER.iter().map(|&c| compare(a, b, c)).find(|o| o.is_ne()).unwrap_or(Ordering::Equal).then(a.path.as_os_str().len().cmp(&b.path.as_os_str().len())).then(a.image.cmp(&b.image))
    });
    idx
}

/// The first criterion where `keep` beats `other` (`None`: equal in all, decided by the path or the entry).
pub fn decisive(keep: &Member, other: &Member) -> Option<Criterion> {
    ORDER.iter().copied().find(|&c| compare(keep, other, c) == Ordering::Less)
}

/// A group with its members and the suggestion.
#[derive(Clone, Debug)]
pub struct Plan {
    pub kind: Kind,
    /// identical: content confirmed by the partial checksum
    pub verified: bool,
    /// similar: lowest similarity of two members
    pub score: f32,
    /// members, the suggested one to keep first
    pub members: Vec<Member>,
    /// suggested for removal (same order as `members`); only for confirmed identical groups
    pub suggested: Vec<bool>,
    /// members from Apple Photos that were left out
    pub hidden: usize,
}

impl Plan {
    /// Bytes that the suggested removals free.
    pub fn suggested_bytes(&self) -> u64 {
        self.members.iter().zip(&self.suggested).filter(|(_, s)| **s).map(|(m, _)| m.size.max(0) as u64).sum()
    }
    /// More than one member has own data (rating, development …): the decision has consequences.
    pub fn with_consequences(&self) -> bool {
        self.members.iter().filter(|m| m.has_data() || m.own_development()).count() > 1
    }
}

fn members(db: &Db, files: &[i64]) -> R<Vec<Member>> {
    let mut out = Vec::new();
    for &file in files {
        let row = db
            .conn
            .query_row(
                "SELECT b.id, s.path || o.path || d.name, d.name, d.format, coalesce(d.size, 0), d.width, d.height, d.capture_time, d.status,
                        b.stars, b.flag, b.label, s.origin, d.mtime, (SELECT started FROM run WHERE id = b.run)
                 FROM file d JOIN image b ON b.file = d.id AND b.copy IS NULL JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
                 WHERE d.id = ?1",
                [file],
                |r| {
                    Ok(Member {
                        image: r.get(0)?,
                        file,
                        path: PathBuf::from(r.get::<_, String>(1)?),
                        name: r.get(2)?,
                        format: r.get(3)?,
                        size: r.get(4)?,
                        width: r.get(5)?,
                        height: r.get(6)?,
                        capture: r.get(7)?,
                        status: r.get::<_, i64>(8)?.clamp(0, 255) as u8,
                        stars: r.get::<_, i64>(9)?.clamp(0, 5) as u8,
                        flag: r.get::<_, i64>(10)?.clamp(-1, 1) as i8,
                        label: r.get::<_, i64>(11)?.clamp(0, 5) as u8,
                        origin: r.get(12)?,
                        mtime: r.get(13)?,
                        imported: r.get(14)?,
                        ..Default::default()
                    })
                },
            )
            .optional()
            .map_err(f)?;
        let Some(mut m) = row else { continue };
        let count = |sql: &str, id: i64| -> R<usize> { db.conn.query_row(sql, [id], |r| r.get::<_, i64>(0)).map(|n| n as usize).map_err(f) };
        m.keywords = count("SELECT count(*) FROM image_keyword bs JOIN keyword s ON s.id = bs.keyword WHERE bs.image = ?1 AND s.person = 0", m.image)?;
        m.persons = count("SELECT count(DISTINCT person) FROM face WHERE image = ?1 AND person IS NOT NULL AND status = 'confirmed'", m.image)?;
        m.collections = count("SELECT count(*) FROM collection_image sb JOIN collection s ON s.id = sb.collection WHERE sb.image = ?1 AND s.kind <> 3", m.image)?;
        m.copies = count("SELECT count(*) FROM image WHERE file = ?1 AND copy IS NOT NULL", file)?;
        m.developed = count("SELECT count(*) FROM develop_state WHERE file = ?1 AND (settings IS NOT NULL OR lr_edited <> 0)", file)? > 0;
        m.network = m.status == 0 && m.path.parent().is_some_and(crate::network_trash::is_network);
        out.push(m);
    }
    Ok(out)
}

fn is_apple(m: &Member) -> bool {
    m.origin == crate::apple_photos::ORIGIN || crate::apple_photos::in_photos_library(&m.path)
}

/// Members, order and suggestion of one group (`None`: fewer than two photos left without Apple Photos).
pub fn plan(db: &Db, g: &Group) -> R<Option<Plan>> {
    let all = members(db, &g.files)?;
    let n_all = all.len();
    let list: Vec<Member> = all.into_iter().filter(|m| !is_apple(m)).collect();
    let hidden = n_all - list.len();
    if list.len() < 2 {
        return Ok(None);
    }
    let members: Vec<Member> = rank(&list).into_iter().map(|i| list[i].clone()).collect();
    let auto = g.kind == Kind::Identical && g.verified && members[0].status == 0;
    let suggested = members.iter().enumerate().map(|(i, m)| auto && i > 0 && m.not_removable().is_none()).collect();
    Ok(Some(Plan { kind: g.kind.clone(), verified: g.verified, score: g.score, members, suggested, hidden }))
}

/// Plans for all groups (reads only). Returns the plans and the number of groups left out because only photos from
/// Apple Photos (or a single other one) were in them.
pub fn plans(db: &Db, groups: &[Group]) -> R<(Vec<Plan>, usize)> {
    let mut out = Vec::with_capacity(groups.len());
    let mut left_out = 0;
    for g in groups {
        match plan(db, g)? {
            Some(p) => out.push(p),
            None => left_out += 1,
        }
    }
    Ok((out, left_out))
}

// ───────────────────────────────────────────── Removing

/// Remove `remove`, keep `keep` (both of the same group).
#[derive(Clone, Debug)]
pub struct Removal {
    pub remove: Member,
    pub keep: Member,
    /// from an "identical" group: the whole content must match
    pub identical: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// carry ratings, keywords … over to the staying photo (off: photos with such data stay)
    pub transfer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Check,
    Trash,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub removed: usize,
    pub bytes: u64,
    pub transferred: usize,
    /// (name, reason): stayed
    pub skipped: Vec<(String, String)>,
    pub errors: Vec<String>,
    pub cancelled: bool,
    /// library entries taken out (⌘Z puts them back)
    pub snapshot: Snapshot,
}

/// Current path, format and state of an image, fresh from the library.
fn fresh(db: &Db, image: i64) -> R<(PathBuf, String, u8, PathBuf, i64, i64)> {
    db.conn
        .query_row(
            "SELECT s.path || o.path || d.name, d.format, d.status, s.path, d.id, d.folder FROM image b JOIN file d ON d.id = b.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE b.id = ?1 AND b.copy IS NULL",
            [image],
            |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get(1)?, r.get::<_, i64>(2)?.clamp(0, 255) as u8, PathBuf::from(r.get::<_, String>(3)?), r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(f)?
        .ok_or_else(|| "nicht mehr in der Bibliothek".to_string())
}

/// The files that may go for this removal, checked fresh against the library and the disk – or the reason why not.
/// `hashes`: SHA-256 already computed (the staying photo is compared with several).
pub fn verify(db: &Db, r: &Removal, hashes: &mut HashMap<PathBuf, String>) -> R<Vec<PathBuf>> {
    let (path, _, status, root, file, folder) = fresh(db, r.remove.image)?;
    let (kpath, _, kstatus, _, kfile, _) = fresh(db, r.keep.image).map_err(|_| "das bleibende Foto ist nicht mehr in der Bibliothek".to_string())?;
    if file == kfile {
        return Err("gleiche Datei".into());
    }
    if status != 0 || kstatus != 0 {
        return Err("Datei fehlt oder Laufwerk nicht verbunden".into());
    }
    if crate::apple_photos::in_photos_library(&path) {
        return Err("Foto aus Apple Fotos".into());
    }
    let own: i64 = db
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM image WHERE file = ?1 AND copy IS NOT NULL) + (SELECT count(*) FROM develop_state WHERE file = ?1 AND (settings IS NOT NULL OR lr_edited <> 0))",
            [file],
            |x| x.get(0),
        )
        .map_err(f)?;
    if own > 0 {
        return Err("hat eine eigene Entwicklung oder virtuelle Kopien".into());
    }
    // the staying photo: a real file with content
    let km = std::fs::symlink_metadata(&kpath).map_err(|_| "das bleibende Foto ist nicht erreichbar".to_string())?;
    if !km.is_file() || km.len() == 0 {
        return Err("das bleibende Foto ist nicht lesbar".into());
    }
    // the one to remove: a real file (no link), inside its root folder, not the same file as the staying one
    let m = std::fs::symlink_metadata(&path).map_err(|_| "Datei nicht erreichbar".to_string())?;
    if !m.is_file() {
        return Err("keine normale Datei".into());
    }
    let (c, cr) = (path.canonicalize().map_err(|e| e.to_string())?, root.canonicalize().map_err(|e| e.to_string())?);
    if !c.starts_with(&cr) {
        return Err("liegt außerhalb des Stammordners".into());
    }
    if kpath.canonicalize().ok().as_deref() == Some(c.as_path()) {
        return Err("beide Einträge zeigen auf dieselbe Datei".into());
    }
    if r.identical {
        if m.len() != km.len() {
            return Err("Inhalt nicht gleich (Größe weicht ab)".into());
        }
        let a = crate::relocate::sha256(&path)?;
        let b = match hashes.get(&kpath) {
            Some(h) => h.clone(),
            None => {
                let h = crate::relocate::sha256(&kpath)?;
                hashes.insert(kpath.clone(), h.clone());
                h
            }
        };
        if a != b {
            return Err("Inhalt nicht gleich (Prüfsumme weicht ab)".into());
        }
    }
    // sidecars: "NAME.EXT.xmp" always; "NAME.xmp" only if no other photo of the folder has the same name stem
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut files = vec![path.clone()];
    let long = path.with_file_name(format!("{name}.xmp"));
    if std::fs::symlink_metadata(&long).is_ok_and(|m| m.is_file()) {
        files.push(long);
    }
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s).to_string();
    let short = path.with_file_name(format!("{stem}.xmp"));
    if std::fs::symlink_metadata(&short).is_ok_and(|m| m.is_file()) {
        let others: i64 = db
            .conn
            .query_row("SELECT count(*) FROM file WHERE folder = ?1 AND id <> ?2 AND lower(name) LIKE lower(?3) || '.%'", rusqlite::params![folder, file, stem], |x| x.get(0))
            .map_err(f)?;
        if others == 0 {
            files.push(short);
        }
    }
    Ok(files)
}

/// Remove these photos (into the Trash, collected in blocks). The caller picks them; [`verify`] decides per photo
/// whether it really goes. `progress(phase, done, total)` and `cancel` are polled between photos and blocks.
pub fn remove(db: &Db, list: &[Removal], opt: Options, targets: &Targets, progress: &dyn Fn(Phase, usize, usize), cancel: &dyn Fn() -> bool) -> Outcome {
    let mut out = Outcome::default();
    let mut hashes: HashMap<PathBuf, String> = HashMap::new();
    // 1. check everything first (reads only)
    let mut ready: Vec<(Removal, Vec<PathBuf>, pairs::Transfer)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, r) in list.iter().enumerate() {
        progress(Phase::Check, i, list.len());
        if cancel() {
            out.cancelled = true;
            return out;
        }
        if !seen.insert(r.remove.image) {
            continue;
        }
        let name = r.remove.name.clone();
        let files = match verify(db, r, &mut hashes) {
            Ok(v) => v,
            Err(e) => {
                out.skipped.push((name, e));
                continue;
            }
        };
        let t = match pairs::transfer_of(db, r.remove.image, r.keep.image) {
            Ok(t) => t,
            Err(e) => {
                out.errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        if !t.is_empty() && !opt.transfer {
            out.skipped.push((name, "hat Bewertung/Stichwörter, die das bleibende Foto nicht hat".into()));
            continue;
        }
        ready.push((r.clone(), files, t));
    }
    // 2. in blocks: carry data over, into the Trash (one call per block), take the entries out of the library
    let total = ready.len();
    for (bi, block) in ready.chunks(trash_batch::BLOCK).enumerate() {
        progress(Phase::Trash, bi * trash_batch::BLOCK, total);
        if cancel() {
            out.cancelled = true;
            break;
        }
        let mut entries: Vec<Entry<usize>> = Vec::new();
        for (k, (r, files, t)) in block.iter().enumerate() {
            if !t.is_empty() {
                if let Err(e) = pairs::apply_transfer(db, r.keep.image, r.keep.file, t) {
                    out.errors.push(format!("{}: {e}", r.remove.name));
                    continue;
                }
                out.transferred += 1;
            }
            entries.push(Entry { key: k, files: files.clone() });
        }
        let result = trash_batch::dispose(&entries, trash_batch::BLOCK, targets, &|_, _| {}, &|| false);
        let mut gone: Vec<i64> = Vec::new();
        for (k, res) in result {
            let r = &block[k].0;
            match res {
                Ok(()) => {
                    gone.push(r.remove.image);
                    out.removed += 1;
                    out.bytes += r.remove.size.max(0) as u64;
                }
                Err(e) => out.errors.push(format!("{}: {e}", r.remove.name)),
            }
        }
        if !gone.is_empty() {
            match crate::manage::remove_trashed_photos(db, &gone) {
                Ok(s) => out.snapshot.merge(s),
                Err(e) => out.errors.push(format!("Bibliothekseinträge: {e}")),
            }
        }
    }
    progress(Phase::Trash, total, total);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(image: i64, name: &str, format: &str) -> Member {
        Member { image, file: image, path: PathBuf::from(format!("/fotos/{name}")), name: name.into(), format: format.into(), size: 1000, width: Some(6000), height: Some(4000), ..Default::default() }
    }

    #[test]
    fn criteria_in_order() {
        let mut a = m(1, "A.JPG", "JPG");
        let mut b = m(2, "B.CR3", "CR3");
        // RAW before JPEG
        assert_eq!(rank(&[a.clone(), b.clone()]), vec![1, 0]);
        assert_eq!(decisive(&b, &a), Some(Criterion::Raw));
        // data beats RAW
        a.stars = 3;
        assert_eq!(rank(&[a.clone(), b.clone()]), vec![0, 1]);
        assert_eq!(decisive(&a, &b), Some(Criterion::Data));
        // development beats data
        b.developed = true;
        assert_eq!(rank(&[a.clone(), b.clone()]), vec![1, 0]);
        // reachable beats everything
        b.status = 2;
        assert_eq!(rank(&[a.clone(), b.clone()]), vec![0, 1]);
        assert_eq!(decisive(&a, &b), Some(Criterion::Reachable));
    }

    #[test]
    fn resolution_size_folder_age() {
        let a = m(1, "A.JPG", "JPG");
        let mut b = m(2, "B.JPG", "JPG");
        b.width = Some(3000);
        assert_eq!(rank(&[b.clone(), a.clone()]), vec![1, 0]);
        let mut c = m(3, "C.JPG", "JPG");
        c.size = 2000;
        assert_eq!(decisive(&c, &a), Some(Criterion::FileSize));
        let mut d = m(4, "D.JPG", "JPG");
        d.path = PathBuf::from("/fotos/Backup 2019/D.JPG");
        assert_eq!(decisive(&a, &d), Some(Criterion::MainFolder));
        let (mut e, mut g) = (m(5, "E.JPG", "JPG"), m(6, "G.JPG", "JPG"));
        e.imported = Some(100);
        g.imported = Some(50);
        assert_eq!(decisive(&g, &e), Some(Criterion::Older));
        assert_eq!(decisive(&e, &e.clone()), None);
    }
}
