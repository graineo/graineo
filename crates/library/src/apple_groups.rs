//! Apple Photos knows people in groups it has not been given a name for. graineo reads these groups (read-only, like
//! everything from Apple Photos) and compares them with its own people.
//!
//! - **Clear groups** are assigned automatically during the Apple sync ([`run`]): the faces get the person with
//!   `lokal = 2` ("from an Apple group"). They count as confirmed, but the user's own decisions (`lokal = 1`, rejected
//!   names) always win and are never overwritten; a name given later in Apple Photos replaces them. They can be
//!   released again per person or group ([`release`]).
//! - **Unclear groups** become ordinary suggestions of the person for their faces without a name (confirm one by one
//!   or several at once).
//! - The manual offer ([`hints`], [`accept`]: "assign all") stays for groups that are left.
//!
//! Only graineo's library is changed; nothing is written into Apple Photos.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection};

/// One face of an Apple group; box normalized to the upright image, origin top left.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupFace {
    pub id: String,
    pub photo: String,
    pub rect: (f32, f32, f32, f32),
}

/// A group of faces without a name in Apple Photos.
#[derive(Clone, Debug, Default)]
pub struct AppleGroup {
    pub id: i64,
    pub faces: Vec<GroupFace>,
    /// names of people Apple itself proposes to merge this group with (merge candidates)
    pub candidates: Vec<String>,
    /// names the user told Apple this group is not
    pub not_candidates: Vec<String>,
}

/// Groups with at least this many faces are looked at (a pair of faces says nothing).
pub const MIN_FACES: usize = 3;

/// `local` value of faces assigned from an Apple group (1 = decided by the user, 0 = automatic/imported).
pub const LOCAL_APPLE_GROUP: i64 = 2;

/// Read the unnamed groups (read-only). Merged groups (their faces belong to the merge target) are left out.
pub fn read_groups(photos_library: &Path) -> Result<Vec<AppleGroup>, String> {
    let c = crate::apple_photos::open(photos_library)?;
    let f = |e: rusqlite::Error| e.to_string();
    let columns: Vec<String> = c.prepare("PRAGMA table_info(ZPERSON)").map_err(f)?.query_map([], |r| r.get::<_, String>(1)).map_err(f)?.flatten().collect();
    let merged = if columns.iter().any(|n| n == "ZMERGETARGETPERSON") { "AND p.ZMERGETARGETPERSON IS NULL" } else { "" };
    let full = if columns.iter().any(|n| n == "ZFULLNAME") { "trim(coalesce(p.ZFULLNAME,''))" } else { "''" };
    let display = if columns.iter().any(|n| n == "ZDISPLAYNAME") { "trim(coalesce(p.ZDISPLAYNAME,''))" } else { "''" };
    let sql = format!(
        "SELECT p.Z_PK, f.ZUUID, a.ZUUID, f.ZCENTERX, f.ZCENTERY, f.ZSIZE, a.ZWIDTH, a.ZHEIGHT
         FROM ZDETECTEDFACE f JOIN ZASSET a ON a.Z_PK = f.ZASSETFORFACE JOIN ZPERSON p ON p.Z_PK = f.ZPERSONFORFACE
         WHERE a.ZTRASHEDSTATE = 0 AND coalesce(f.ZHIDDEN, 0) = 0 AND f.ZUUID IS NOT NULL AND a.ZUUID IS NOT NULL
           AND {full} = '' AND {display} = '' {merged}"
    );
    let mut st = c.prepare(&sql).map_err(f)?;
    let rows = st
        .query_map([], |r| {
            let n = |i: usize| -> rusqlite::Result<f64> { Ok(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0)) };
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, n(3)?, n(4)?, n(5)?, n(6)?, n(7)?))
        })
        .map_err(f)?;
    let mut groups: std::collections::BTreeMap<i64, Vec<GroupFace>> = Default::default();
    for (p, id, photo, cx, cy, size, w, h) in rows.flatten() {
        if let Some(rect) = crate::apple_photos::rect_list(cx, cy, size, w, h) {
            groups.entry(p).or_default().push(GroupFace { id, photo, rect });
        }
    }
    let candidates = merge_pairs(&c, &columns, false);
    let not_candidates = merge_pairs(&c, &columns, true);
    Ok(groups
        .into_iter()
        .filter(|(_, v)| v.len() >= MIN_FACES)
        .map(|(id, faces)| AppleGroup { id, faces, candidates: candidates.get(&id).cloned().unwrap_or_default(), not_candidates: not_candidates.get(&id).cloned().unwrap_or_default() })
        .collect())
}

/// Apple's merge candidates between an unnamed and a named person (`invalid`: pairs the user rejected in Apple Photos):
/// unnamed person → names. The join table carries the entity number in its name (`Z_<n>MERGECANDIDATES`); missing
/// tables (older or newer libraries) simply give nothing.
fn merge_pairs(c: &Connection, columns: &[String], invalid: bool) -> HashMap<i64, Vec<String>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    if !columns.iter().any(|n| n == "ZFULLNAME") {
        return out;
    }
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'Z\\_%MERGECANDIDATES' ESCAPE '\\'")
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    let Some(table) = tables.into_iter().find(|n| n.contains("INVALID") == invalid && !n.contains("WITHCONFIDENCE")) else { return out };
    let cols: Vec<String> = c.prepare(&format!("PRAGMA table_info(\"{table}\")")).and_then(|mut s| s.query_map([], |r| r.get::<_, String>(1)).map(|r| r.flatten().collect())).unwrap_or_default();
    let (Some(a), Some(b)) = (cols.first(), cols.get(1)) else { return out };
    let name = "coalesce(nullif(trim(n.ZFULLNAME),''), trim(coalesce(n.ZDISPLAYNAME,'')))";
    let unnamed = "trim(coalesce(u.ZFULLNAME,'')) = '' AND trim(coalesce(u.ZDISPLAYNAME,'')) = ''";
    // both directions: the pair is stored once or twice depending on who proposed it
    let sql = format!(
        "SELECT u.Z_PK, {name} FROM \"{table}\" m JOIN ZPERSON u ON u.Z_PK = m.\"{a}\" JOIN ZPERSON n ON n.Z_PK = m.\"{b}\" WHERE {unnamed} AND {name} <> ''
         UNION SELECT u.Z_PK, {name} FROM \"{table}\" m JOIN ZPERSON u ON u.Z_PK = m.\"{b}\" JOIN ZPERSON n ON n.Z_PK = m.\"{a}\" WHERE {unnamed} AND {name} <> ''"
    );
    if let Ok(mut st) = c.prepare(&sql) {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for (u, n) in rows.flatten() {
                let v = out.entry(u).or_default();
                if !v.contains(&n) {
                    v.push(n);
                }
            }
        }
    }
    out
}

/// An graineo face that sits at the position of a group face.
#[derive(Clone, Debug)]
struct Own {
    id: i64,
    image: i64,
    person: Option<i64>,
    state: String,
    apple_id: Option<String>,
    local: i64,
    value: f32,
    second: f32,
}

/// Photo identifier → image (Apple photos in the library: the file name without extension is the identifier).
fn images(c: &Connection) -> Result<HashMap<String, i64>, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut st = c
        .prepare("SELECT d.name, b.id FROM image b JOIN file d ON d.id = b.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE s.origin = ?1 AND b.copy IS NULL")
        .map_err(f)?;
    let r = st
        .query_map([crate::apple_photos::ORIGIN], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(f)?
        .flatten()
        .map(|(n, b)| (n.split('.').next().unwrap_or_default().to_string(), b))
        .collect();
    Ok(r)
}

/// The own face at the place of a group face (the best overlap above 0.45), if there is one.
fn own_face(c: &Connection, image: i64, face: &GroupFace) -> Option<Own> {
    let mut st = c.prepare_cached("SELECT id, x, y, w, h, person, status, apple_id, local, coalesce(confidence, 0), coalesce(score_own, 0) FROM face WHERE image = ?1").ok()?;
    let rows: Vec<(f32, Own)> = st
        .query_map([image], |r| {
            let rect = (r.get::<_, f64>(1)? as f32, r.get::<_, f64>(2)? as f32, r.get::<_, f64>(3)? as f32, r.get::<_, f64>(4)? as f32);
            Ok((
                crate::people::overlap(face.rect, rect),
                Own { id: r.get(0)?, image, person: r.get(5)?, state: r.get(6)?, apple_id: r.get(7)?, local: r.get(8)?, value: r.get::<_, f64>(9)? as f32, second: r.get::<_, f64>(10)? as f32 },
            ))
        })
        .ok()?
        .flatten()
        .collect();
    // a face already joined to this Apple face counts first
    if let Some((_, o)) = rows.iter().find(|(_, o)| o.apple_id.as_deref() == Some(face.id.as_str())) {
        return Some(o.clone());
    }
    rows.into_iter().filter(|(o, _)| *o > 0.45).max_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, o)| o)
}

fn hidden_people(c: &Connection) -> HashSet<i64> {
    c.prepare("SELECT keyword FROM person_info WHERE hidden=1").and_then(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect())).unwrap_or_default()
}

/// Offer: this Apple group seems to be this person.
#[derive(Clone, Debug)]
pub struct Hint {
    pub group: i64,
    pub person: i64,
    pub name: String,
    /// faces (photos) of the group
    pub faces: Vec<GroupFace>,
    /// of them with a face of its own at the place
    pub matched: usize,
    /// of those: confirmed as this person / suggested as this person
    pub confirmed: usize,
    pub suggested: usize,
    /// would be assigned newly by [`accept`] (including faces without a box of their own)
    pub open: usize,
}

/// Compare the groups with the people of the library. A hint appears when at least [`MIN_FACES`] matching faces
/// and at least half of the matching faces belong (confirmed or as suggestion) to one person, and something is left to assign.
pub fn hints(c: &Connection, groups: &[AppleGroup]) -> Result<Vec<Hint>, String> {
    let images = images(c)?;
    let hidden = hidden_people(c);
    let mut out = Vec::new();
    for g in groups {
        let mut matched = 0usize;
        let mut per_person: HashMap<i64, (usize, usize)> = HashMap::new();
        for f in &g.faces {
            let Some(&image) = images.get(&f.photo) else { continue };
            let Some(o) = own_face(c, image, f) else { continue };
            matched += 1;
            if let Some(p) = o.person.filter(|p| !hidden.contains(p)) {
                let e = per_person.entry(p).or_default();
                match o.state.as_str() {
                    "confirmed" => e.0 += 1,
                    "suggested" => e.1 += 1,
                    _ => {}
                }
            }
        }
        let Some((&person, &(confirmed, suggested))) = per_person.iter().max_by_key(|(p, (a, b))| (a + b, std::cmp::Reverse(**p))) else { continue };
        if confirmed + suggested < MIN_FACES || (confirmed + suggested) * 2 < matched {
            continue;
        }
        let open = plan(c, &images, g, person, false)?.len();
        if open == 0 {
            continue;
        }
        let name: String = c.query_row("SELECT name FROM keyword WHERE id=?1", [person], |r| r.get(0)).map_err(|e| e.to_string())?;
        out.push(Hint { group: g.id, person, name, faces: g.faces.clone(), matched, confirmed, suggested, open });
    }
    out.sort_by(|a, b| b.open.cmp(&a.open).then(a.group.cmp(&b.group)));
    Ok(out)
}

/// What assigning would do with one face of the group.
enum Step {
    /// an own face: set it to the person
    Face(Own),
    /// no box of its own: add one from Apple's
    New(i64, GroupFace),
}

/// Is the person already named on another face of this image (confirmed)? Then this face is someone else.
fn person_in_image(c: &Connection, image: i64, except: Option<i64>, person: i64) -> Result<bool, String> {
    c.query_row("SELECT count(*) FROM face WHERE image=?1 AND person=?2 AND status='confirmed' AND id IS NOT ?3", params![image, person, except], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
        .map_err(|e| e.to_string())
}

/// The steps for assigning group `g` to `person`. `automatic`: stricter, for [`run`] – faces decided by the user
/// (`lokal = 1`) and faces suggested as someone else stay as they are, and a person is never set twice in one photo.
fn plan(c: &Connection, images: &HashMap<String, i64>, g: &AppleGroup, person: i64, automatic: bool) -> Result<Vec<Step>, String> {
    let mut out = Vec::new();
    for f in &g.faces {
        let Some(&image) = images.get(&f.photo) else { continue };
        match own_face(c, image, f) {
            Some(o) => {
                let rejected: bool = c.query_row("SELECT count(*) FROM face_never WHERE face=?1 AND person=?2", params![o.id, person], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())? > 0;
                let done = o.state == "confirmed" && o.person == Some(person);
                let other = o.state == "confirmed" && o.person.is_some_and(|p| p != person);
                if done || other || rejected || o.state == "rejected" {
                    continue;
                }
                if automatic && (o.local == 1 || (o.state == "suggested" && o.person.is_some_and(|p| p != person)) || person_in_image(c, image, Some(o.id), person)?) {
                    continue;
                }
                out.push(Step::Face(o));
            }
            None => {
                // a face Apple knows that graineo's own search did not find (small, in profile …); the Apple face id is unique
                let known: i64 = c.query_row("SELECT count(*) FROM face WHERE apple_id=?1", [&f.id], |r| r.get(0)).map_err(|e| e.to_string())?;
                if known == 0 && !(automatic && person_in_image(c, image, None, person)?) {
                    out.push(Step::New(image, f.clone()));
                }
            }
        }
    }
    Ok(out)
}

/// Carry out the steps: `local` = 1 (by the user) or [`LOCAL_APPLE_GROUP`]. Returns the ids of the faces.
fn carry_out(db: &crate::db::Db, steps: &[Step], person: i64, local: i64) -> Result<Vec<i64>, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let now = crate::now();
    let mut ids = Vec::new();
    // in blocks: the library is never occupied for long
    for block in steps.chunks(100) {
        let tx = db.conn.unchecked_transaction().map_err(fe)?;
        for s in block {
            match s {
                Step::Face(o) => {
                    tx.execute(
                        "UPDATE face SET person=?2, status='confirmed', local=?3, suspect=0, guessed=NULL, cluster=NULL, confidence=NULL, score_own=NULL WHERE id=?1",
                        params![o.id, person, local],
                    )
                    .map_err(fe)?;
                    let old = o.person.filter(|p| *p != person);
                    crate::people::change_image_person_without_file(db, o.image, old, Some(person))?;
                    ids.push(o.id);
                }
                Step::New(image, f) => {
                    let (x, y, b, h) = f.rect;
                    tx.execute(
                        "INSERT INTO face(image, x, y, w, h, source, apple_id, person, status, local, detected) VALUES(?1,?2,?3,?4,?5,'apple',?6,?7,'confirmed',?8,?9)",
                        params![image, x, y, b, h, f.id, person, local, now],
                    )
                    .map_err(fe)?;
                    ids.push(tx.last_insert_rowid());
                    crate::people::change_image_person_without_file(db, *image, None, Some(person))?;
                }
            }
        }
        tx.commit().map_err(fe)?;
    }
    Ok(ids)
}

/// Assign the group to the person (after the user's confirmation). Returns the number of faces newly assigned.
/// Faces already decided otherwise (another person confirmed, rejected, "no face") stay as they are.
pub fn accept(db: &crate::db::Db, hint: &Hint) -> Result<usize, String> {
    let images = images(&db.conn)?;
    let group = AppleGroup { id: hint.group, faces: hint.faces.clone(), ..Default::default() };
    let steps = plan(&db.conn, &images, &group, hint.person, false)?;
    Ok(carry_out(db, &steps, hint.person, 1)?.len())
}

// ───────────────────────────────────────────── Automatic assignment

/// Keys in the table `info` (no schema change needed): assigned groups, released groups, open group suggestions.
const KEY_ASSIGNED: &str = "apple_groups_assigned";
const KEY_RELEASED: &str = "apple_gruppen_geloest";
const KEY_SUGGESTED: &str = "apple_groups_suggestions";

/// An automatically assigned group: person and the faces set by it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assigned {
    pub person: i64,
    pub faces: Vec<i64>,
}

/// A JSON array stored under `key` (empty if there is none).
fn read_info(c: &Connection, key: &str) -> Vec<serde_json::Value> {
    c.query_row("SELECT value FROM info WHERE name=?1", [key], |r| r.get::<_, String>(0)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Pairs of numbers `[[a, b], …]`.
fn read_pairs(c: &Connection, key: &str) -> Vec<(i64, i64)> {
    read_info(c, key).iter().filter_map(|v| Some((v.get(0)?.as_i64()?, v.get(1)?.as_i64()?))).collect()
}

fn write_info(c: &Connection, key: &str, value: &serde_json::Value) -> Result<(), String> {
    let text = value.to_string();
    let old: Option<String> = c.query_row("SELECT value FROM info WHERE name=?1", [key], |r| r.get(0)).ok();
    if old.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    c.execute("INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value", params![key, text]).map(|_| ()).map_err(|e| e.to_string())
}

/// Stored as `[[group, person, [faces …]], …]`.
fn assigned_map(c: &Connection) -> HashMap<i64, Assigned> {
    read_info(c, KEY_ASSIGNED)
        .iter()
        .filter_map(|v| {
            let faces = v.get(2)?.as_array()?.iter().filter_map(|f| f.as_i64()).collect();
            Some((v.get(0)?.as_i64()?, Assigned { person: v.get(1)?.as_i64()?, faces }))
        })
        .collect()
}

fn store_assigned(c: &Connection, m: &HashMap<i64, Assigned>) -> Result<(), String> {
    let mut v: Vec<(&i64, &Assigned)> = m.iter().collect();
    v.sort_by_key(|x| *x.0);
    write_info(c, KEY_ASSIGNED, &serde_json::json!(v.into_iter().map(|(g, a)| serde_json::json!([g, a.person, a.faces])).collect::<Vec<_>>()))
}

fn released_set(c: &Connection) -> HashSet<(i64, i64)> {
    read_pairs(c, KEY_RELEASED).into_iter().collect()
}

fn pairs_json(v: &[(i64, i64)]) -> serde_json::Value {
    serde_json::json!(v.iter().map(|(a, b)| serde_json::json!([a, b])).collect::<Vec<_>>())
}

/// The thresholds of the automatic assignment (see [`decide`]). Measured on the user's library (13.11.2026): in all
/// 63 Apple groups where graineo already had a confirmed name or a suggestion, not a single face pointed to another
/// person (6 confirmed and 263 "very likely" suggestions, each checked against a decision made without it).
#[derive(Clone, Debug)]
pub struct Rules {
    /// votes (confirmed + suggestions) for the person
    pub min_votes: usize,
    /// of them confirmed or "very likely"
    pub min_strong: usize,
    /// share of the person among all votes in the group
    pub min_share: f32,
    /// share of the person among the faces of the group that graineo knows (own box at the place)
    pub min_known_share: f32,
    /// for suggestions only (unclear groups)
    pub suggest_min_votes: usize,
    pub suggest_min_share: f32,
}

impl Default for Rules {
    fn default() -> Self {
        Rules { min_votes: 3, min_strong: 2, min_share: 0.9, min_known_share: 0.3, suggest_min_votes: 2, suggest_min_share: 0.67 }
    }
}

/// Votes of graineo's own faces in a group.
#[derive(Debug, Default)]
struct Tally {
    /// faces of the group with an own box at the place
    known: usize,
    /// person → [confirmed (user, Lightroom, Apple), very likely suggestion, other suggestion, from an Apple group]
    votes: HashMap<i64, [usize; 4]>,
    /// person → faces of the group on which the user rejected this name
    rejected: HashMap<i64, usize>,
}

fn tally(c: &Connection, images: &HashMap<String, i64>, g: &AppleGroup, hidden: &HashSet<i64>) -> Result<Tally, String> {
    let mut t = Tally::default();
    let mut never = c.prepare_cached("SELECT person FROM face_never WHERE face=?1").map_err(|e| e.to_string())?;
    for f in &g.faces {
        let Some(&image) = images.get(&f.photo) else { continue };
        let Some(o) = own_face(c, image, f) else { continue };
        t.known += 1;
        for p in never.query_map([o.id], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?.flatten() {
            *t.rejected.entry(p).or_default() += 1;
        }
        let Some(p) = o.person.filter(|p| !hidden.contains(p)) else { continue };
        let slot = match (o.state.as_str(), o.local) {
            ("confirmed", LOCAL_APPLE_GROUP) => 3,
            ("confirmed", _) => 0,
            ("suggested", _) if crate::face_index::sure(o.value, o.second) => 1,
            ("suggested", _) => 2,
            _ => continue,
        };
        t.votes.entry(p).or_default()[slot] += 1;
    }
    Ok(t)
}

/// What to do with a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// assign automatically
    Assign(i64),
    /// suggest the person for the faces without a name
    Suggest(i64),
    Nothing,
}

/// The rule (own definition):
/// - The person with the most votes of graineo's own faces in the group (confirmed and suggested; with no votes at
///   all the one person Apple proposes as merge candidate).
/// - **Assign** when nothing speaks against it – no face of the group confirmed or "very likely" as someone else, no
///   rejected name for the person in the group, Apple not told "not this person", not released by the user – and the
///   evidence is clear: ≥ 3 votes, of them ≥ 2 confirmed or "very likely" (with Apple's own merge proposal: ≥ 2 votes,
///   ≥ 1 strong), ≥ 90 % of all votes and ≥ 30 % of the faces graineo knows in the group. A group assigned before
///   stays assigned (new faces join) as long as nothing speaks against it.
/// - **Suggest** when the person has ≥ 2 votes and ≥ 2/3 of them (or is Apple's merge proposal) and strong
///   contradiction is rare.
fn decide(t: &Tally, g: &AppleGroup, names: &HashMap<String, i64>, before: Option<i64>, released: &HashSet<(i64, i64)>, hidden: &HashSet<i64>, r: &Rules) -> Decision {
    let by_name = |v: &[String]| -> HashSet<i64> { v.iter().filter_map(|n| names.get(&crate::people::person_brief(n)).copied()).collect() };
    let candidates = by_name(&g.candidates);
    let not = by_name(&g.not_candidates);
    let total = |v: &[usize; 4]| v.iter().sum::<usize>();
    let top = t
        .votes
        .iter()
        .max_by_key(|(p, v)| (total(v), v[0] + v[1], candidates.contains(p), std::cmp::Reverse(**p)))
        .map(|(p, _)| *p)
        .or_else(|| if candidates.len() == 1 { candidates.iter().next().copied() } else { None });
    let Some(p) = top else { return Decision::Nothing };
    if hidden.contains(&p) || not.contains(&p) {
        return Decision::Nothing;
    }
    let mine = t.votes.get(&p).copied().unwrap_or_default();
    let (votes, strong) = (total(&mine), mine[0] + mine[1]);
    let all: usize = t.votes.values().map(total).sum();
    let against_strong: usize = t.votes.iter().filter(|(q, _)| **q != p).map(|(_, v)| v[0] + v[1] + v[3]).sum();
    let rejected = t.rejected.get(&p).copied().unwrap_or(0);
    let candidate = candidates.contains(&p);
    let clean = against_strong == 0 && rejected == 0 && !released.contains(&(g.id, p));
    if clean && before == Some(p) {
        return Decision::Assign(p);
    }
    let evidence = (votes >= r.min_votes && strong >= r.min_strong) || (candidate && votes >= 2 && strong >= 1);
    if clean && before.is_none() && evidence && votes as f32 >= r.min_share * all as f32 && votes as f32 >= r.min_known_share * t.known as f32 {
        return Decision::Assign(p);
    }
    if against_strong <= strong / 4 && ((votes >= r.suggest_min_votes && votes as f32 >= r.suggest_min_share * all as f32) || (candidate && against_strong == 0)) {
        return Decision::Suggest(p);
    }
    Decision::Nothing
}

/// Result of [`run`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RunReport {
    pub groups: usize,
    pub assigned_groups: usize,
    /// faces newly assigned in this run
    pub assigned_faces: usize,
    pub suggested_groups: usize,
    /// faces that got a suggestion from a group in this run
    pub suggested_faces: usize,
}

/// The automatic pass after the Apple sync: assign clear groups, turn unclear ones into suggestions. Short and
/// quiet: reads graineo's faces at the places of the group faces and writes only what changes.
pub fn run(db: &crate::db::Db, groups: &[AppleGroup], r: &Rules) -> Result<RunReport, String> {
    let c = &db.conn;
    let images = images(c)?;
    let hidden = hidden_people(c);
    let names: HashMap<String, i64> = crate::people::person_names(c).into_iter().map(|(id, n)| (crate::people::person_brief(&n), id)).collect();
    let released = released_set(c);
    let mut assigned = assigned_map(c);
    let mut suggestions: Vec<(i64, i64)> = Vec::new();
    let mut report = RunReport { groups: groups.len(), ..Default::default() };
    for g in groups {
        let t = tally(c, &images, g, &hidden)?;
        let before = assigned.get(&g.id).map(|a| a.person);
        match decide(&t, g, &names, before, &released, &hidden, r) {
            Decision::Assign(p) => {
                report.assigned_groups += 1;
                let steps = plan(c, &images, g, p, true)?;
                let ids = carry_out(db, &steps, p, LOCAL_APPLE_GROUP)?;
                report.assigned_faces += ids.len();
                let e = assigned.entry(g.id).or_insert_with(|| Assigned { person: p, faces: Vec::new() });
                e.faces.extend(ids);
            }
            Decision::Suggest(p) => {
                report.suggested_groups += 1;
                for f in &g.faces {
                    let Some(&image) = images.get(&f.photo) else { continue };
                    if let Some(o) = own_face(c, image, f).filter(|o| o.person.is_none() && o.state == "unknown" && o.local == 0) {
                        suggestions.push((o.id, p));
                    }
                }
            }
            Decision::Nothing => {}
        }
    }
    // faces of assigned groups that no longer carry the assignment (answered by the user, deleted) are left out
    for a in assigned.values_mut() {
        a.faces.retain(|id| c.query_row("SELECT local=?2 AND person=?3 FROM face WHERE id=?1", params![id, LOCAL_APPLE_GROUP, a.person], |r| r.get::<_, bool>(0)).unwrap_or(false));
    }
    assigned.retain(|_, a| !a.faces.is_empty());
    store_assigned(c, &assigned)?;
    suggestions.sort_unstable();
    suggestions.dedup_by_key(|s| s.0);
    write_info(c, KEY_SUGGESTED, &pairs_json(&suggestions))?;
    report.suggested_faces = apply_suggestions(c)?;
    Ok(report)
}

/// Set the suggestions from unclear groups again (the full computation of suggestions resets them): only faces still
/// without a name and not decided by the user, never a rejected name, never a person already confirmed in the photo.
/// Faces with a suggestion of graineo's own comparison keep it. Returns the faces written.
pub fn apply_suggestions(c: &Connection) -> Result<usize, String> {
    let list = read_pairs(c, KEY_SUGGESTED);
    if list.is_empty() {
        return Ok(0);
    }
    let hidden = hidden_people(c);
    let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut n = 0;
    for (face, person) in list.into_iter().filter(|(_, p)| !hidden.contains(p)) {
        n += tx
            .execute(
                "UPDATE face SET person=?2, status='suggested', confidence=NULL, score_own=NULL WHERE id=?1 AND person IS NULL AND status='unknown' AND local=0
                   AND NOT EXISTS (SELECT 1 FROM face_never WHERE face=?1 AND person=?2)
                   AND NOT EXISTS (SELECT 1 FROM face x WHERE x.image = face.image AND x.person=?2 AND x.status='confirmed')",
                params![face, person],
            )
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n)
}

/// Release faces assigned from Apple groups: the groups containing one of `faces`, or (with no faces) all groups of
/// `person`. The faces carry no name afterwards (Apple's own boxes that graineo had added are removed again) and get
/// graineo's own suggestions; the groups are not assigned automatically again (their faces become suggestions).
/// Faces the user has answered meanwhile stay. Returns the number of faces released.
pub fn release(db: &crate::db::Db, person: Option<i64>, faces: &[i64]) -> Result<usize, String> {
    let c = &db.conn;
    let fe = |e: rusqlite::Error| e.to_string();
    let mut assigned = assigned_map(c);
    let chosen: Vec<i64> = assigned
        .iter()
        .filter(|(_, a)| if faces.is_empty() { Some(a.person) == person } else { a.faces.iter().any(|f| faces.contains(f)) })
        .map(|(g, _)| *g)
        .collect();
    let mut ids: Vec<(i64, i64)> = Vec::new();
    for g in &chosen {
        let a = &assigned[g];
        ids.extend(a.faces.iter().map(|f| (*f, a.person)));
    }
    // with a person: also faces of hers outside the list (to be safe)
    if let (true, Some(p)) = (faces.is_empty(), person) {
        let mut st = c.prepare("SELECT id FROM face WHERE person=?1 AND local=?2").map_err(fe)?;
        ids.extend(st.query_map(params![p, LOCAL_APPLE_GROUP], |r| r.get::<_, i64>(0)).map_err(fe)?.flatten().map(|f| (f, p)));
    }
    ids.sort_unstable();
    ids.dedup();
    let mut released = released_set(c);
    for g in &chosen {
        if let Some(a) = assigned.remove(g) {
            released.insert((*g, a.person));
        }
    }
    let mut images = Vec::new();
    let mut n = 0;
    for block in ids.chunks(200) {
        let tx = c.unchecked_transaction().map_err(fe)?;
        for (id, p) in block {
            let row: Option<(i64, String)> = tx.query_row("SELECT image, source FROM face WHERE id=?1 AND local=?2 AND person=?3", params![id, LOCAL_APPLE_GROUP, p], |r| Ok((r.get(0)?, r.get(1)?))).ok();
            let Some((image, source)) = row else { continue };
            if source == "apple" {
                tx.execute("DELETE FROM face WHERE id=?1", [id]).map_err(fe)?;
            } else {
                tx.execute("UPDATE face SET person=NULL, status='unknown', local=0, suspect=0, guessed=NULL WHERE id=?1", [id]).map_err(fe)?;
            }
            crate::people::change_image_person_without_file(db, image, Some(*p), None)?;
            images.push(image);
            n += 1;
        }
        tx.commit().map_err(fe)?;
    }
    store_assigned(c, &assigned)?;
    let mut list: Vec<(i64, i64)> = released.into_iter().collect();
    list.sort_unstable();
    write_info(c, KEY_RELEASED, &pairs_json(&list))?;
    // graineo's own suggestions for the faces again
    images.sort_unstable();
    images.dedup();
    let _ = crate::people::suggestions_for_images(c, &images, &crate::people::Thresholds::default());
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use rusqlite::Connection;

    fn photos_library(name: &str) -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("graineo-apple-gruppen-{}-{}.photoslibrary", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("database")).unwrap();
        let c = Connection::open(dir.join("database").join("Photos.sqlite")).unwrap();
        c.execute_batch(
            "CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
             INSERT INTO Z_PRIMARYKEY VALUES (3,'Asset'),(32,'GenericAlbum'),(33,'Album');
             CREATE TABLE ZASSET (Z_PK INTEGER PRIMARY KEY, ZUUID TEXT, ZDIRECTORY TEXT, ZFILENAME TEXT, ZDATECREATED REAL, ZWIDTH INTEGER, ZHEIGHT INTEGER, ZKIND INTEGER, ZFAVORITE INTEGER, ZHIDDEN INTEGER, ZTRASHEDSTATE INTEGER, ZADJUSTMENTTIMESTAMP REAL, ZDURATION REAL);
             CREATE TABLE ZADDITIONALASSETATTRIBUTES (ZASSET INTEGER, ZORIGINALFILENAME TEXT, ZTIMEZONEOFFSET INTEGER, ZORIGINALFILESIZE INTEGER);
             CREATE TABLE ZEXTENDEDATTRIBUTES (ZASSET INTEGER, ZCAMERAMAKE TEXT, ZCAMERAMODEL TEXT, ZLENSMODEL TEXT);
             CREATE TABLE ZGENERICALBUM (Z_PK INTEGER PRIMARY KEY, ZKIND INTEGER, ZTITLE TEXT, ZTRASHEDSTATE INTEGER);
             CREATE TABLE Z_33ASSETS (Z_33ALBUMS INTEGER, Z_3ASSETS INTEGER, Z_FOK_3ASSETS INTEGER);
             CREATE TABLE ZPERSON (Z_PK INTEGER PRIMARY KEY, ZFULLNAME TEXT, ZDISPLAYNAME TEXT, ZMERGETARGETPERSON INTEGER);
             CREATE TABLE ZDETECTEDFACE (Z_PK INTEGER PRIMARY KEY, ZUUID TEXT, ZCENTERX REAL, ZCENTERY REAL, ZSIZE REAL, ZASSETFORFACE INTEGER, ZPERSONFORFACE INTEGER, ZHIDDEN INTEGER);",
        )
        .unwrap();
        (dir, c)
    }

    /// Group 10 (unnamed, 6 faces on 6 photos), group 11 (unnamed, 2 faces: too small), group 12 (named: not read).
    /// In the library: 4 faces of group 10 are Anna (3 confirmed, 1 suggestion), 1 has no person, 1 has no box at all.
    #[test]
    fn group_hint_and_assignment() {
        let (dir, c) = photos_library("hint");
        for i in 1..=8 {
            c.execute("INSERT INTO ZASSET VALUES (?1, ?2, 'A', ?3, 0, 1000, 1000, 0, 0, 0, 0, NULL, NULL)", params![i, format!("U{i}"), format!("U{i}.heic")]).unwrap();
        }
        c.execute_batch("INSERT INTO ZPERSON VALUES (10, NULL, '', NULL), (11, '', NULL, NULL), (12, 'Ben', 'Ben', NULL), (13, NULL, NULL, 10);").unwrap();
        // centre (0.5, 0.5 from the bottom), size 0.2
        for (i, person) in [(1, 10), (2, 10), (3, 10), (4, 10), (5, 10), (6, 10), (7, 11), (8, 11)] {
            c.execute("INSERT INTO ZDETECTEDFACE VALUES (?1, ?2, 0.5, 0.5, 0.2, ?1, ?3, 0)", params![i, format!("F{i}"), person]).unwrap();
        }
        c.execute("INSERT INTO ZDETECTEDFACE VALUES (20, 'FB', 0.5, 0.5, 0.2, 1, 12, 0), (21, 'FM', 0.5, 0.5, 0.2, 2, 13, 0)", []).unwrap();
        drop(c);
        let groups = read_groups(&dir).unwrap();
        assert_eq!(groups.len(), 1, "named, merged and tiny groups are left out");
        assert_eq!((groups[0].id, groups[0].faces.len()), (10, 6));

        let db = Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        crate::apple_photos::reconcile(&db, &dir).unwrap();
        let image = |n: i32| -> i64 { db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id=b.file WHERE d.name=?1", [format!("U{n}.heic")], |r| r.get(0)).unwrap() };
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(900,'Anna',1)", []).unwrap();
        // own boxes at the same place (x = 0.4, y = 0.4, 0.2 × 0.2)
        let own = |n: i32, person: Option<i64>, state: &str| {
            db.conn.execute("INSERT INTO face(image, x, y, w, h, source, person, status) VALUES(?1,0.4,0.4,0.2,0.2,'graineo',?2,?3)", params![image(n), person, state]).unwrap();
        };
        own(1, Some(900), "confirmed");
        own(2, Some(900), "confirmed");
        own(3, Some(900), "confirmed");
        own(4, Some(900), "suggested");
        own(5, None, "unknown");
        // photo 6: no own box
        let hints = hints(&db.conn, &groups).unwrap();
        assert_eq!(hints.len(), 1);
        let h = &hints[0];
        assert_eq!((h.person, h.name.as_str(), h.faces.len(), h.matched, h.confirmed, h.suggested), (900, "Anna", 6, 5, 3, 1));
        assert_eq!(h.open, 3, "suggestion, unknown face and the face without a box; the 3 confirmed need nothing");

        // nothing happened until now (no automatic assignment)
        let confirmed = |db: &Db| -> i64 { db.conn.query_row("SELECT count(*) FROM face WHERE person=900 AND status='confirmed'", [], |r| r.get(0)).unwrap() };
        assert_eq!(confirmed(&db), 3);
        // one face was rejected for Anna by the user: stays out
        db.conn.execute("INSERT INTO face_never(face, person) SELECT id, 900 FROM face WHERE image=?1", [image(5)]).unwrap();
        assert_eq!(hints_open(&db, &groups), 2);
        db.conn.execute("DELETE FROM face_never", []).unwrap();
        let n = accept(&db, h).unwrap();
        assert_eq!(n, 3);
        assert_eq!(confirmed(&db), 6);
        let local: i64 = db.conn.query_row("SELECT count(*) FROM face WHERE person=900 AND local=1", [], |r| r.get(0)).unwrap();
        assert_eq!(local, 3, "the new ones count as decided by the user");
        assert!(crate::sync::keyword_paths(&db, image(6)).unwrap().iter().any(|p| p.ends_with("Anna")), "photo keyword set also where the box was added");
        // a second time: nothing left
        assert!(super::hints(&db.conn, &groups).unwrap().is_empty());
        assert_eq!(accept(&db, h).unwrap(), 0);
        // the Photos library was only read
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hints_open(db: &Db, groups: &[AppleGroup]) -> usize {
        super::hints(&db.conn, groups).unwrap().first().map_or(0, |h| h.open)
    }

    /// Faces mostly of another person, or too few: no hint.
    #[test]
    fn no_hint_when_unclear() {
        let (dir, c) = photos_library("unklar");
        for i in 1..=6 {
            c.execute("INSERT INTO ZASSET VALUES (?1, ?2, 'A', ?3, 0, 1000, 1000, 0, 0, 0, 0, NULL, NULL)", params![i, format!("V{i}"), format!("V{i}.heic")]).unwrap();
            c.execute("INSERT INTO ZDETECTEDFACE VALUES (?1, ?2, 0.5, 0.5, 0.2, ?1, 10, 0)", params![i, format!("G{i}")]).unwrap();
        }
        c.execute("INSERT INTO ZPERSON VALUES (10, NULL, NULL, NULL)", []).unwrap();
        drop(c);
        let groups = read_groups(&dir).unwrap();
        let db = Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        crate::apple_photos::reconcile(&db, &dir).unwrap();
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(900,'Anna',1),(901,'Ben',1)", []).unwrap();
        for (n, person) in [(1, 900), (2, 900), (3, 901), (4, 901), (5, 901), (6, 901)] {
            let image: i64 = db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id=b.file WHERE d.name=?1", [format!("V{n}.heic")], |r| r.get(0)).unwrap();
            db.conn.execute("INSERT INTO face(image, x, y, w, h, source, person, status) VALUES(?1,0.4,0.4,0.2,0.2,'graineo',?2,'confirmed')", params![image, person]).unwrap();
        }
        // Ben 4 of 6 faces: hint for Ben only if the rest is open – here the other two are confirmed Anna: nothing to assign
        let h = hints(&db.conn, &groups).unwrap();
        assert!(h.is_empty(), "{h:?}");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Photos library with one unnamed group (id 10) of `n` faces on photos `W1`…, optionally Apple's merge
    /// proposal "Anna" for it; the library with the photos and the people Anna (900) and Ben (901).
    fn setup(name: &str, n: i32, merge: Option<&str>) -> (std::path::PathBuf, Db, Vec<i64>) {
        let (dir, c) = photos_library(name);
        c.execute_batch("CREATE TABLE Z_59MERGECANDIDATES (Z_59MERGECANDIDATES INTEGER, REFLEXIVE INTEGER); CREATE TABLE Z_59INVALIDMERGECANDIDATES (Z_59INVALIDMERGECANDIDATES INTEGER, REFLEXIVE INTEGER);").unwrap();
        for i in 1..=n {
            c.execute("INSERT INTO ZASSET VALUES (?1, ?2, 'A', ?3, 0, 1000, 1000, 0, 0, 0, 0, NULL, NULL)", params![i, format!("W{i}"), format!("W{i}.heic")]).unwrap();
            c.execute("INSERT INTO ZDETECTEDFACE VALUES (?1, ?2, 0.5, 0.5, 0.2, ?1, 10, 0)", params![i, format!("H{i}")]).unwrap();
        }
        c.execute_batch("INSERT INTO ZPERSON VALUES (10, NULL, NULL, NULL), (20, 'Anna', 'Anna', NULL);").unwrap();
        match merge {
            Some("valid") => c.execute("INSERT INTO Z_59MERGECANDIDATES VALUES (20, 10)", []).unwrap(),
            Some(_) => c.execute("INSERT INTO Z_59INVALIDMERGECANDIDATES VALUES (10, 20)", []).unwrap(),
            None => 0,
        };
        drop(c);
        let db = Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        crate::apple_photos::reconcile(&db, &dir).unwrap();
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(900,'Anna',1),(901,'Ben',1)", []).unwrap();
        let images = (1..=n).map(|i| db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id=b.file WHERE d.name=?1", [format!("W{i}.heic")], |r| r.get(0)).unwrap()).collect();
        (dir, db, images)
    }

    /// An own box at the place of the Apple face: person, state, suggestion values, `local`.
    fn own_box(db: &Db, image: i64, person: Option<i64>, state: &str, value: f64, local: i64) -> i64 {
        db.conn
            .execute("INSERT INTO face(image, x, y, w, h, source, person, status, confidence, score_own, local) VALUES(?1,0.4,0.4,0.2,0.2,'graineo',?2,?3,?4,0.3,?5)", params![image, person, state, value, local])
            .unwrap();
        db.conn.last_insert_rowid()
    }

    fn row(db: &Db, id: i64) -> (Option<i64>, String, i64) {
        db.conn.query_row("SELECT person, status, local FROM face WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
    }

    /// A clear group is assigned automatically (`lokal = 2`, source "Apple group"), only once, and can be released.
    #[test]
    fn clear_group_assigned_and_released() {
        let (dir, db, im) = setup("auto", 7, None);
        let confirmed = own_box(&db, im[0], Some(900), "confirmed", 0.0, 0); // e.g. from Lightroom
        let sure1 = own_box(&db, im[1], Some(900), "suggested", 0.82, 0);
        let sure2 = own_box(&db, im[2], Some(900), "suggested", 0.78, 0);
        let maybe = own_box(&db, im[3], Some(900), "suggested", 0.63, 0);
        let unknown = own_box(&db, im[4], None, "unknown", 0.0, 0);
        let user_no_name = own_box(&db, im[5], None, "unknown", 0.0, 1); // the user removed the name: stays
        // photo 7: no own box (Apple's box is added)
        let groups = read_groups(&dir).unwrap();
        let r = run(&db, &groups, &Rules::default()).unwrap();
        assert_eq!((r.assigned_groups, r.assigned_faces), (1, 5), "{r:?}");
        for id in [sure1, sure2, maybe, unknown] {
            assert_eq!(row(&db, id), (Some(900), "confirmed".into(), LOCAL_APPLE_GROUP));
        }
        assert_eq!(row(&db, confirmed), (Some(900), "confirmed".into(), 0), "confirmed before: unchanged");
        assert_eq!(row(&db, user_no_name), (None, "unknown".into(), 1), "the user's decision has priority");
        let added: i64 = db.conn.query_row("SELECT count(*) FROM face WHERE image=?1 AND source='apple' AND local=2 AND person=900", [im[6]], |r| r.get(0)).unwrap();
        assert_eq!(added, 1);
        assert!(crate::sync::keyword_paths(&db, im[4]).unwrap().iter().any(|p| p.ends_with("Anna")));
        // in the people view: source "Apple group", status confirmed
        let ix = crate::face_index::load(&db.conn).unwrap();
        let f = ix.face(unknown).unwrap();
        assert_eq!((f.source, f.status), (crate::face_index::Source::AppleGroup, crate::face_index::Status::Confirmed));
        // a second run changes nothing
        assert_eq!(run(&db, &groups, &Rules::default()).unwrap().assigned_faces, 0);
        // a full computation of the suggestions leaves them alone
        crate::people::assign(&db.conn, &crate::people::Thresholds::default()).unwrap();
        assert_eq!(row(&db, unknown).2, LOCAL_APPLE_GROUP);
        // the user answers one face "no": it stays decided, the rest stays assigned
        crate::people::apply_answer(&db, sure1, crate::people::Answer::RemoveName).unwrap();
        run(&db, &groups, &Rules::default()).unwrap();
        assert_eq!(row(&db, sure1), (None, "unknown".into(), 1));
        assert_eq!(row(&db, sure2).2, LOCAL_APPLE_GROUP);
        // release for Anna: names gone, Apple's added box removed, keyword gone, not assigned again
        let n = release(&db, Some(900), &[]).unwrap();
        assert_eq!(n, 4);
        assert_eq!(row(&db, unknown).0, None);
        assert_eq!(row(&db, unknown).2, 0);
        let added: i64 = db.conn.query_row("SELECT count(*) FROM face WHERE image=?1", [im[6]], |r| r.get(0)).unwrap();
        assert_eq!(added, 0);
        assert!(!crate::sync::keyword_paths(&db, im[4]).unwrap().iter().any(|p| p.ends_with("Anna")));
        let again = run(&db, &groups, &Rules::default()).unwrap();
        assert_eq!(again.assigned_faces, 0, "released groups are not assigned again");
        assert_eq!(row(&db, confirmed), (Some(900), "confirmed".into(), 0), "confirmed faces are not released");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unclear groups (no strong votes) become suggestions; they survive a full computation of the suggestions; a
    /// face confirmed as someone else by the user stops the automatic assignment.
    #[test]
    fn unclear_group_suggests_and_user_wins() {
        let (dir, db, im) = setup("unklar2", 6, None);
        own_box(&db, im[0], Some(900), "suggested", 0.64, 0);
        own_box(&db, im[1], Some(900), "suggested", 0.62, 0);
        let open1 = own_box(&db, im[2], None, "unknown", 0.0, 0);
        let open2 = own_box(&db, im[3], None, "unknown", 0.0, 0);
        let groups = read_groups(&dir).unwrap();
        let r = run(&db, &groups, &Rules::default()).unwrap();
        assert_eq!((r.assigned_faces, r.suggested_groups, r.suggested_faces), (0, 1, 2), "{r:?}");
        assert_eq!(row(&db, open1), (Some(900), "suggested".into(), 0));
        // the full computation resets suggestions; the group's come back
        crate::people::assign(&db.conn, &crate::people::Thresholds::default()).unwrap();
        assert_eq!(row(&db, open2), (Some(900), "suggested".into(), 0));
        // "no" to a suggestion: never again for this face
        crate::people::answer_suggestion(&db, open2, false).unwrap();
        crate::people::assign(&db.conn, &crate::people::Thresholds::default()).unwrap();
        assert_eq!(row(&db, open2).0, None);

        // now strong votes for Anna, but the user confirmed one face as Ben: no automatic assignment
        let (dir2, db2, im2) = setup("nutzer", 6, None);
        own_box(&db2, im2[0], Some(900), "confirmed", 0.0, 0);
        own_box(&db2, im2[1], Some(900), "suggested", 0.8, 0);
        own_box(&db2, im2[2], Some(900), "suggested", 0.8, 0);
        let ben = own_box(&db2, im2[3], Some(901), "confirmed", 0.0, 1);
        let open = own_box(&db2, im2[4], None, "unknown", 0.0, 0);
        let r = run(&db2, &read_groups(&dir2).unwrap(), &Rules::default()).unwrap();
        assert_eq!(r.assigned_faces, 0, "{r:?}");
        assert_eq!(row(&db2, ben), (Some(901), "confirmed".into(), 1));
        assert_eq!(row(&db2, open).2, 0);
        drop((db, db2));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// Apple's merge proposal lowers the evidence needed; a rejected proposal ("not Anna") blocks.
    #[test]
    fn merge_candidates() {
        let (dir, db, im) = setup("merge", 4, Some("valid"));
        own_box(&db, im[0], Some(900), "suggested", 0.8, 0);
        own_box(&db, im[1], Some(900), "suggested", 0.62, 0);
        own_box(&db, im[2], None, "unknown", 0.0, 0);
        let groups = read_groups(&dir).unwrap();
        assert_eq!(groups[0].candidates, vec!["Anna".to_string()]);
        let r = run(&db, &groups, &Rules::default()).unwrap();
        assert_eq!(r.assigned_groups, 1, "{r:?}");

        let (dir2, db2, im2) = setup("merge-nein", 4, Some("invalid"));
        own_box(&db2, im2[0], Some(900), "suggested", 0.8, 0);
        own_box(&db2, im2[1], Some(900), "suggested", 0.8, 0);
        own_box(&db2, im2[2], Some(900), "suggested", 0.8, 0);
        let groups = read_groups(&dir2).unwrap();
        assert_eq!(groups[0].not_candidates, vec!["Anna".to_string()]);
        let r = run(&db2, &groups, &Rules::default()).unwrap();
        assert_eq!((r.assigned_groups, r.suggested_groups), (0, 0), "{r:?}");
        drop((db, db2));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// A name given in Apple Photos later replaces the assignment from the group.
    #[test]
    fn apple_name_replaces_group_assignment() {
        let (dir, db, im) = setup("benannt", 5, None);
        own_box(&db, im[0], Some(900), "confirmed", 0.0, 0);
        own_box(&db, im[1], Some(900), "suggested", 0.8, 0);
        let f = own_box(&db, im[2], Some(900), "suggested", 0.8, 0);
        run(&db, &read_groups(&dir).unwrap(), &Rules::default()).unwrap();
        assert_eq!(row(&db, f).2, LOCAL_APPLE_GROUP);
        // the user names the group "Ben" in Apple Photos
        let list = vec![crate::apple_photos::Face { id: "H3".into(), photo: "W3".into(), rect_list: (0.4, 0.4, 0.2, 0.2), name: "Ben".into() }];
        crate::apple_photos::import_faces(&db, &list).unwrap();
        assert_eq!(row(&db, f), (Some(901), "confirmed".into(), 0));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
