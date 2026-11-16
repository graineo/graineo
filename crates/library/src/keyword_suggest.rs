//! Suggestions for ordering a flat keyword collection (decision 4 B): never applied by themselves. The dialog
//! "Stichwörter ordnen …" lists them unchecked; only what is checked is applied, after a report and a backup of the
//! library, as one ⌘Z step.
//!
//! Kinds, in the order shown (the most reliable first):
//! - spelling: same name at the same place apart from case and accents ("architektur" / "Architektur") → merge
//! - plural: singular and plural at the same place ("Kind" / "Kinder"; endings n, e, en, er, s) → merge
//! - ending: adjective + noun ending in another keyword ("rote Wand" → below "Wand")
//! - together: almost always together (≥ 90 % of the photos of A also carry B, B at least twice as frequent) → A
//!   below B. Often only a context ("Strandkorb → Sommer"), so a hint, shown last.
//!
//! People are never touched, nor the branches below a top-level keyword starting with "KI-" (the AI branches are
//! decided separately).

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::db::Db;
use crate::manage::{self, Snapshot};

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Smallest number of photos of a keyword for the kind "together".
const TOGETHER_MIN: usize = 10;
/// Share of the photos of A that also carry B.
const TOGETHER_SHARE: f64 = 0.95;
/// A target carried by more than this share of all photos with keywords describes a whole situation ("Familie",
/// "Zuhause"), not a parent term: no "together" suggestion below it. Measured on the user's library (16.11.2026):
/// without it, "Mutter → Familie", "Spielmatte → Zuhause".
const TOGETHER_MAX_TARGET: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Spelling,
    Plural,
    Ending,
    Together,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    /// merge `from` into `into`
    Merge { from: i64, into: i64 },
    /// put `child` below `parent`
    Below { child: i64, parent: i64 },
}

/// One suggestion with what the dialog shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub kind: Kind,
    pub change: Change,
    /// shown names (paths where they are not at the top)
    pub from: String,
    pub to: String,
    /// photos with the first keyword (merge: they get the second), with the second
    pub photos_from: usize,
    pub photos_to: usize,
    /// "together": share of the photos of the first that carry the second
    pub share: Option<f64>,
}

struct Node {
    name: String,
    parent: Option<i64>,
    person: bool,
}

fn fold(s: &str) -> String {
    crate::search::fold_text(s.trim())
}

/// Same spelling apart from case and ß/ss ("architektur"/"Architektur", "Spass"/"Spaß"). Umlauts and accents stay
/// apart: "Burger"/"Bürger", "Plane"/"Pläne" are different words.
fn spelling_key(s: &str) -> String {
    s.trim().to_lowercase().replace('ß', "ss")
}

/// Compute the suggestions (read only; on 11 000 keywords and 340 000 assignments about a second).
pub fn suggest(conn: &Connection) -> R<Vec<Suggestion>> {
    suggest_timed(conn).map(|x| x.0)
}

/// Like [`suggest`], with the time of each stage (reading, the four kinds) for measurements.
pub fn suggest_timed(conn: &Connection) -> R<(Vec<Suggestion>, Vec<(&'static str, std::time::Duration)>)> {
    let mut times: Vec<(&'static str, std::time::Duration)> = Vec::new();
    let mut t = std::time::Instant::now();
    let mut lap = |name: &'static str, times: &mut Vec<(&'static str, std::time::Duration)>| {
        times.push((name, t.elapsed()));
        t = std::time::Instant::now();
    };
    let nodes: HashMap<i64, Node> = {
        let mut st = conn.prepare("SELECT id, name, parent, person FROM keyword").map_err(f)?;
        let v = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, Node { name: r.get(1)?, parent: r.get(2)?, person: r.get::<_, i64>(3)? != 0 }))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        v
    };
    let root_of = |mut id: i64| -> i64 {
        for _ in 0..crate::keywords::MAX_DEPTH {
            match nodes.get(&id).and_then(|n| n.parent) {
                Some(p) if nodes.contains_key(&p) => id = p,
                _ => break,
            }
        }
        id
    };
    // keywords that take part: no people, nothing in an AI branch (and not the branch roots themselves)
    let usable: HashSet<i64> = nodes.iter().filter(|(id, n)| !n.person && !nodes.get(&root_of(**id)).is_some_and(|r| r.name.trim().starts_with("KI-"))).map(|(id, _)| *id).collect();
    let path = |id: i64| crate::keywords::shown_path(conn, id).unwrap_or_default();
    // photos per keyword and keywords per photo
    let mut photos: HashMap<i64, usize> = HashMap::new();
    let mut per_image: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut st = conn.prepare("SELECT ik.image, ik.keyword FROM image_keyword ik JOIN image b ON b.id = ik.image JOIN file d ON d.id = b.file WHERE d.status <> 4").map_err(f)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(f)?;
        for row in rows {
            let (b, k) = row.map_err(f)?;
            *photos.entry(k).or_default() += 1;
            if usable.contains(&k) {
                per_image.entry(b).or_default().push(k);
            }
        }
    }
    let count = |k: i64| photos.get(&k).copied().unwrap_or(0);
    let mut out: Vec<Suggestion> = Vec::new();
    let mut taken: HashSet<i64> = HashSet::new();
    let add = |out: &mut Vec<Suggestion>, taken: &mut HashSet<i64>, kind: Kind, change: Change, share: Option<f64>| {
        let (a, b) = match change {
            Change::Merge { from, into } => (from, into),
            Change::Below { child, parent } => (child, parent),
        };
        // one suggestion per keyword (the first, most reliable kind wins)
        if !taken.insert(a) {
            return;
        }
        out.push(Suggestion { kind, change, from: path(a), to: path(b), photos_from: count(a), photos_to: count(b), share });
    };
    // siblings by place
    let mut by_place: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
    for id in &usable {
        by_place.entry(nodes[id].parent).or_default().push(*id);
    }
    for v in by_place.values_mut() {
        v.sort_unstable();
    }
    lap("read", &mut times);
    // 1. spelling: same folded name at one place → into the most frequent
    for ids in by_place.values() {
        let mut groups: HashMap<String, Vec<i64>> = HashMap::new();
        for id in ids {
            groups.entry(spelling_key(&nodes[id].name)).or_default().push(*id);
        }
        for g in groups.values().filter(|g| g.len() > 1) {
            let into = *g.iter().max_by_key(|k| (count(**k), std::cmp::Reverse(**k))).unwrap();
            for from in g.iter().filter(|k| **k != into) {
                add(&mut out, &mut taken, Kind::Spelling, Change::Merge { from: *from, into }, None);
            }
        }
    }
    lap("spelling", &mut times);
    // 2. singular/plural at one place → into the more frequent; single words only (names of several words are
    //    often proper names: "Südwest Press" / "Südwest Presse")
    for ids in by_place.values() {
        let names: HashMap<String, i64> = ids.iter().filter(|id| !nodes[id].name.trim().contains(' ')).map(|id| (fold(&nodes[id].name), *id)).collect();
        for (n, id) in &names {
            for end in ["n", "e", "en", "er", "s"] {
                let Some(other) = names.get(&format!("{n}{end}")) else { continue };
                let (a, b) = (*id, *other);
                let (from, into) = if count(a) > count(b) { (b, a) } else { (a, b) };
                if !taken.contains(&into) {
                    add(&mut out, &mut taken, Kind::Plural, Change::Merge { from, into }, None);
                }
            }
        }
    }
    lap("plural", &mut times);
    // 3. adjective + noun ending in an existing keyword ("blaue Wand" → below "Wand"): exactly two words, the first
    //    in lower case, the second the keyword in its own spelling (top level, a name that exists once). Longer names
    //    ("Vater und Sohn") and verbs ("Steine sammeln") are no parent relation.
    let mut by_name: HashMap<String, Vec<i64>> = HashMap::new();
    for id in &usable {
        by_name.entry(fold(&nodes[id].name)).or_default().push(*id);
    }
    let two_words = |n: &str| -> Option<(String, String)> {
        let w: Vec<&str> = n.split_whitespace().collect();
        (w.len() == 2 && w[0].chars().next().is_some_and(char::is_lowercase) && w[1].chars().next().is_some_and(char::is_uppercase)).then(|| (w[0].to_string(), w[1].to_string()))
    };
    let mut multi: Vec<i64> = usable.iter().copied().filter(|id| nodes[id].parent.is_none() && two_words(&nodes[id].name).is_some()).collect();
    multi.sort_unstable();
    for id in multi {
        let Some((_, noun)) = two_words(&nodes[&id].name) else { continue };
        if let Some([parent]) = by_name.get(&fold(&noun)).map(Vec::as_slice) {
            if *parent != id && nodes[parent].name.trim() == noun && !taken.contains(parent) {
                add(&mut out, &mut taken, Kind::Ending, Change::Below { child: id, parent: *parent }, None);
            }
        }
    }
    lap("ending", &mut times);
    // 4. almost always together: A (top level, ≥ 10 photos) below B (not one of the very frequent situation words)
    let with_keywords = per_image.len().max(1) as f64;
    let mut pairs: HashMap<(i64, i64), u32> = HashMap::new();
    for ks in per_image.values() {
        for &a in ks {
            if count(a) < TOGETHER_MIN || nodes[&a].parent.is_some() {
                continue;
            }
            for &b in ks {
                // (the frequency limit only means something in a library of some size)
                let situation = with_keywords >= 1000.0 && count(b) as f64 > TOGETHER_MAX_TARGET * with_keywords;
                if a != b && count(b) >= 2 * count(a) && !situation {
                    *pairs.entry((a, b)).or_default() += 1;
                }
            }
        }
    }
    let mut best: HashMap<i64, (i64, u32)> = HashMap::new();
    for ((a, b), n) in pairs {
        if f64::from(n) >= TOGETHER_SHARE * count(a) as f64 {
            let e = best.entry(a).or_insert((b, n));
            if n > e.1 || n == e.1 && count(b) > count(e.0) {
                *e = (b, n);
            }
        }
    }
    let mut together: Vec<(i64, i64, u32)> = best.into_iter().map(|(a, (b, n))| (a, b, n)).collect();
    together.sort_by_key(|(a, _, _)| (std::cmp::Reverse(count(*a)), *a));
    for (a, b, n) in together {
        // never below one of its own children
        let mut cur = Some(b);
        let mut below_itself = false;
        while let Some(c) = cur {
            if c == a {
                below_itself = true;
                break;
            }
            cur = nodes.get(&c).and_then(|x| x.parent);
        }
        if !below_itself {
            add(&mut out, &mut taken, Kind::Together, Change::Below { child: a, parent: b }, Some(f64::from(n) / count(a) as f64));
        }
    }
    lap("together", &mut times);
    out.sort_by(|x, y| x.kind.cmp(&y.kind).then(y.photos_from.cmp(&x.photos_from)).then(x.from.to_lowercase().cmp(&y.from.to_lowercase())));
    Ok((out, times))
}

/// Photos the chosen changes touch (each once): their keywords or keyword paths change.
pub fn impact(db: &Db, changes: &[Change]) -> R<usize> {
    let roots: Vec<i64> = changes.iter().map(|c| match c {
        Change::Merge { from, .. } => *from,
        Change::Below { child, .. } => *child,
    }).collect();
    Ok(crate::keyword_arrange::images_below(&db.conn, &roots)?.len())
}

/// Apply the chosen changes in one step (one ⌘Z). A change that no longer fits (the keyword is gone, a name clash,
/// a person) is skipped and named in the result. The search entries are renewed once at the end.
pub fn apply(db: &Db, changes: &[Change]) -> R<(Snapshot, usize, Vec<String>)> {
    let mut snap = Snapshot::default();
    let mut done = 0;
    let mut skipped = Vec::new();
    let mut touched: Vec<i64> = Vec::new();
    for c in changes {
        let exists = |id: i64| crate::keyword_arrange::exists(&db.conn, id);
        let r = match *c {
            Change::Merge { from, into } if exists(from) && exists(into) => {
                touched.push(into);
                manage::merge_keyword(db, from, into)
            }
            Change::Below { child, parent } if exists(child) && exists(parent) => {
                touched.push(child);
                crate::keyword_arrange::move_keywords_quiet(db, &[child], Some(parent))
            }
            _ => Err("Stichwort gibt es nicht mehr".into()),
        };
        match r {
            Ok(s) => {
                snap.merge(s);
                done += 1;
            }
            Err(e) => skipped.push(e),
        }
    }
    // the changes are committed: renewing the search entries only warns (an error would lose the ⌘Z step)
    let images = snap.warn("search index", crate::keyword_arrange::images_below(&db.conn, &touched)).unwrap_or_default();
    snap.warn("search index", crate::keyword_arrange::renew_search(db, &images));
    Ok((snap, done, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measurement on a copy of a real library (never the library itself): GRAINEO_SUGGEST_DB=<copy>.
    /// `cargo test -p library --release keyword_suggest::tests::measure -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measure() {
        let Ok(path) = std::env::var("GRAINEO_SUGGEST_DB") else { return };
        let c = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let all = std::time::Instant::now();
        let (s, times) = suggest_timed(&c).unwrap();
        println!("total {:?}", all.elapsed());
        for (n, d) in &times {
            println!("time {n}: {d:?}");
        }
        for k in [Kind::Spelling, Kind::Plural, Kind::Ending, Kind::Together] {
            let v: Vec<&Suggestion> = s.iter().filter(|x| x.kind == k).collect();
            println!("== {k:?}: {}", v.len());
            // ten examples spread over the list
            let step = (v.len() / 10).max(1);
            for x in v.iter().step_by(step).take(10) {
                println!("  {} -> {} ({}/{}{})", x.from, x.to, x.photos_from, x.photos_to, x.share.map(|p| format!(", {:.0} %", p * 100.0)).unwrap_or_default());
            }
        }
    }

    fn library() -> (std::path::PathBuf, Db) {
        let d = std::env::temp_dir().join(format!("graineo-suggest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        db.conn.execute_batch("INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/f/', 'f', 'own'); INSERT INTO folder(id, root, path) VALUES(1, 1, '');").unwrap();
        // 20 photos
        for i in 0..20 {
            db.conn.execute("INSERT INTO file(id, folder, name, format) VALUES(?1, 1, ?2, 'JPG')", rusqlite::params![100 + i, format!("{i}.jpg")]).unwrap();
            db.conn.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [100 + i]).unwrap();
        }
        db.conn
            .execute_batch(
                "INSERT INTO keyword(id, name) VALUES(1, 'Architektur'), (2, 'architektur'), (3, 'Kind'), (4, 'Kinder'), (5, 'Wand'), (6, 'rote Wand'),
                    (7, 'Hochzeit'), (8, 'Brautpaar'), (9, 'Anna'), (10, 'KI-Analyse');
                 INSERT INTO keyword(id, name, parent) VALUES(11, 'Kinder', 10), (12, 'Kind', 10);
                 UPDATE keyword SET person = 1 WHERE id = 9;",
            )
            .unwrap();
        let assign = |k: i64, from: i64, to: i64| {
            for b in from..to {
                db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, ?2)", [100 + b, k]).unwrap();
            }
        };
        assign(1, 0, 4);
        assign(2, 4, 5);
        assign(3, 0, 2);
        assign(4, 2, 8);
        assign(5, 0, 3);
        assign(6, 3, 4);
        assign(7, 0, 20);
        assign(8, 0, 10);
        assign(9, 0, 6);
        assign(11, 0, 3);
        (d, db)
    }

    #[test]
    fn spelling_and_ending_rules() {
        assert_eq!(spelling_key("Spaß"), spelling_key("spass"));
        assert_ne!(spelling_key("Bürger"), spelling_key("Burger"));
    }

    #[test]
    fn suggestions_by_kind_never_people_or_ai_branches() {
        let (d, db) = library();
        let s = suggest(&db.conn).unwrap();
        let changes: Vec<(Kind, Change)> = s.iter().map(|x| (x.kind, x.change)).collect();
        assert!(changes.contains(&(Kind::Spelling, Change::Merge { from: 2, into: 1 })), "{changes:?}");
        assert!(changes.contains(&(Kind::Plural, Change::Merge { from: 3, into: 4 })));
        assert!(changes.contains(&(Kind::Ending, Change::Below { child: 6, parent: 5 })));
        assert!(changes.contains(&(Kind::Together, Change::Below { child: 8, parent: 7 })), "Brautpaar always with Hochzeit");
        assert!(!changes.iter().any(|(_, c)| matches!(c, Change::Merge { from: 9, .. } | Change::Below { child: 9, .. } | Change::Merge { from: 11 | 12, .. })), "no people, no AI branch");
        assert_eq!(s.iter().map(|x| x.kind).collect::<Vec<_>>().windows(2).all(|w| w[0] <= w[1]), true, "reliable kinds first");
        // applying two of them: one ⌘Z step
        let pick = [Change::Merge { from: 3, into: 4 }, Change::Below { child: 8, parent: 7 }];
        assert_eq!(impact(&db, &pick).unwrap(), 10, "photos 0–9 (Kind: 0–1, Brautpaar: 0–9)");
        let (snap, n, skipped) = apply(&db, &pick).unwrap();
        assert_eq!((n, skipped.len()), (2, 0));
        assert_eq!(db.conn.query_row("SELECT parent FROM keyword WHERE id = 8", [], |r| r.get::<_, Option<i64>>(0)).unwrap(), Some(7));
        manage::undo(&db, &snap).unwrap();
        manage::after_keyword_undo(&db, &snap).unwrap();
        assert_eq!(db.conn.query_row("SELECT count(*) FROM keyword WHERE id = 3", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.conn.query_row("SELECT parent FROM keyword WHERE id = 8", [], |r| r.get::<_, Option<i64>>(0)).unwrap(), None);
        std::fs::remove_dir_all(d).unwrap();
    }
}
