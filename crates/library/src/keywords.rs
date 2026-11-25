//! Keywords by identity: a keyword is its row id, never its name or path text (see the hierarchy concept). Path
//! texts only exist at the edges: typed input ("Orte > Italien > Rom"), sidecars and Lightroom ("Orte|Italien|Rom"),
//! and the interface ("Orte › Italien › Rom"). Everything that only moves assignments around (people, faces,
//! virtual copies, the keyword field of the info panel) works with ids, so a name containing a separator ("AC/DC",
//! "A > B") is never split into a chain.
//!
//! Typed text is resolved by [`resolve`]: an existing keyword is found by its full path, by the end of its path or by
//! a name that occurs exactly once in the tree; a name in several branches ("Paris" below places and below people) is
//! never silently duplicated – the caller gets the candidates and asks.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Db;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Longest parent chain followed or created (protects against cycles in damaged data).
pub const MAX_DEPTH: usize = 64;

/// From this many affected photos a merge (or another mass change of keywords) first backs up the library.
pub const BACKUP_FROM: usize = 1000;

/// Separators understood in typed paths.
pub const TYPED_SEPARATORS: [char; 3] = ['>', '›', '|'];

/// Separator between path segments in the interface.
pub const SHOWN_SEPARATOR: &str = " › ";

/// Segments of a typed path ("Orte > Italien > Rom"), trimmed, empty ones dropped.
pub fn split_typed(text: &str) -> Vec<String> {
    text.split(TYPED_SEPARATORS).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Path segments of a keyword, root first. Ends after [`MAX_DEPTH`] steps.
pub fn segments(conn: &Connection, id: i64) -> R<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = Some(id);
    let mut st = conn.prepare_cached("SELECT name, parent FROM keyword WHERE id = ?1").map_err(f)?;
    while let Some(k) = cur {
        if out.len() >= MAX_DEPTH {
            break;
        }
        let (name, parent): (String, Option<i64>) = st.query_row([k], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?;
        out.push(name);
        cur = parent;
    }
    out.reverse();
    Ok(out)
}

/// [`segments`] for an exported file: `None` if the keyword or any keyword above it is a person (marked as person or
/// naming a face) – a person's name must not leave through a parent term.
fn segments_without_people(conn: &Connection, id: i64) -> R<Option<Vec<String>>> {
    let mut out = Vec::new();
    let mut cur = Some(id);
    let mut st = conn
        .prepare_cached("SELECT name, parent, person <> 0 OR EXISTS (SELECT 1 FROM face f WHERE f.person = keyword.id) FROM keyword WHERE id = ?1")
        .map_err(f)?;
    while let Some(k) = cur {
        if out.len() >= MAX_DEPTH {
            break;
        }
        let (name, parent, person): (String, Option<i64>, bool) = st.query_row([k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?;
        if person {
            return Ok(None);
        }
        out.push(name);
        cur = parent;
    }
    out.reverse();
    Ok(Some(out))
}

/// Shown path of a keyword ("Orte › Italien › Rom").
pub fn shown_path(conn: &Connection, id: i64) -> R<String> {
    Ok(segments(conn, id)?.join(SHOWN_SEPARATOR))
}

/// Ids of the keywords an image carries itself (not the inherited parent terms).
pub fn ids_of(conn: &Connection, image: i64) -> R<Vec<i64>> {
    let mut st = conn.prepare_cached("SELECT keyword FROM image_keyword WHERE image = ?1 ORDER BY keyword").map_err(f)?;
    let v = st.query_map([image], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<i64>>>().map_err(f)?;
    Ok(v)
}

/// Keywords of an image with their shown path, sorted by path (case-insensitive).
pub fn shown_of(conn: &Connection, image: i64) -> R<Vec<(i64, String)>> {
    let mut v: Vec<(i64, String)> = ids_of(conn, image)?.into_iter().map(|id| shown_path(conn, id).map(|p| (id, p))).collect::<R<_>>()?;
    v.sort_by_key(|(_, p)| p.to_lowercase());
    Ok(v)
}

/// Replace an image's keywords by these ids (all or nothing); `own`: remember it as a change made in graineo.
pub fn set_ids(db: &Db, image: i64, ids: &[i64], time: i64, own: bool) -> R<()> {
    db.conn.execute_batch("SAVEPOINT set_keyword_ids").map_err(f)?;
    let r = (|| -> R<()> {
        db.conn.execute("DELETE FROM image_keyword WHERE image = ?1", [image]).map_err(f)?;
        let mut ins = db.conn.prepare_cached("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1, ?2)").map_err(f)?;
        for id in ids {
            ins.execute(params![image, id]).map_err(f)?;
        }
        db.conn
            .execute("UPDATE image SET keywords_time = ?2, keywords_changed = CASE WHEN ?3 THEN ?2 ELSE keywords_changed END WHERE id = ?1", params![image, time, own])
            .map_err(f)?;
        Ok(())
    })();
    match r {
        Ok(()) => db.conn.execute_batch("RELEASE set_keyword_ids").map_err(f)?,
        Err(e) => {
            let _ = db.conn.execute_batch("ROLLBACK TO set_keyword_ids; RELEASE set_keyword_ids");
            return Err(e);
        }
    }
    if own {
        let _ = crate::search::update_image(db, image);
    }
    Ok(())
}

/// The keyword `name` directly below `parent` (`None`: top level), created if missing. The name is taken literally
/// (no splitting). An exact match wins; otherwise a single match ignoring case.
pub fn child(conn: &Connection, parent: Option<i64>, name: &str) -> R<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Der Name darf nicht leer sein.".into());
    }
    if let Some(id) = conn.query_row("SELECT id FROM keyword WHERE name = ?1 AND parent IS ?2 ORDER BY id LIMIT 1", params![name, parent], |r| r.get(0)).optional().map_err(f)? {
        return Ok(id);
    }
    if let Some(id) = sibling_named(conn, parent, name, None)? {
        return Ok(id);
    }
    conn.execute("INSERT INTO keyword(name, parent) VALUES(?1, ?2)", params![name, parent]).map_err(f)?;
    Ok(conn.last_insert_rowid())
}

/// Another keyword below `parent` whose name equals `name` ignoring case (the first one; `except`: not this one).
/// Names at one place are unique in this sense: moving or renaming onto such a name means merging.
pub fn sibling_named(conn: &Connection, parent: Option<i64>, name: &str, except: Option<i64>) -> R<Option<i64>> {
    let want = name.trim().to_lowercase();
    let mut st = conn.prepare_cached("SELECT id, name FROM keyword WHERE parent IS ?1 ORDER BY id").map_err(f)?;
    let rows = st.query_map(params![parent], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(f)?;
    for row in rows {
        let (id, n) = row.map_err(f)?;
        if Some(id) != except && n.trim().to_lowercase() == want {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Result of resolving typed text.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    /// exactly this keyword exists
    Existing(i64),
    /// newly created (with any missing parents)
    Created(i64),
    /// nothing fits (only with `create = false`)
    Missing,
    /// several keywords fit: their shown paths (the caller asks which one is meant)
    Ambiguous(Vec<String>),
}

/// The keyword tree in memory (for resolving; 11 000 keywords load in a few milliseconds).
struct Tree {
    node: HashMap<i64, (String, Option<i64>)>,
}

impl Tree {
    fn load(conn: &Connection) -> R<Self> {
        let mut st = conn.prepare("SELECT id, name, parent FROM keyword").map_err(f)?;
        let node = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        Ok(Tree { node })
    }

    fn chain(&self, id: i64) -> Vec<i64> {
        let mut v = vec![id];
        let mut cur = self.node.get(&id).and_then(|n| n.1);
        while let Some(p) = cur {
            if v.len() >= MAX_DEPTH || v.contains(&p) {
                break;
            }
            v.push(p);
            cur = self.node.get(&p).and_then(|n| n.1);
        }
        v.reverse();
        v
    }

    fn shown(&self, id: i64) -> String {
        self.chain(id).iter().filter_map(|i| self.node.get(i).map(|n| n.0.clone())).collect::<Vec<_>>().join(SHOWN_SEPARATOR)
    }

    /// Keywords whose path ends with `segs` (ignoring case); `exact`: also with the same case in every segment.
    fn ending_with(&self, segs: &[String]) -> Vec<(i64, bool, bool)> {
        let lower: Vec<String> = segs.iter().map(|s| s.to_lowercase()).collect();
        let mut out = Vec::new();
        for (&id, (name, _)) in &self.node {
            if name.trim().to_lowercase() != *lower.last().unwrap_or(&String::new()) {
                continue;
            }
            let chain = self.chain(id);
            if chain.len() < segs.len() {
                continue;
            }
            let tail = &chain[chain.len() - segs.len()..];
            let names: Vec<&str> = tail.iter().map(|i| self.node[i].0.trim()).collect();
            if names.iter().zip(&lower).all(|(n, l)| n.to_lowercase() == *l) {
                let exact = names.iter().zip(segs).all(|(n, s)| *n == s.as_str());
                out.push((id, exact, chain.len() == segs.len()));
            }
        }
        out.sort_unstable();
        out
    }

    /// The one keyword meant by `segs`, or the candidates. A full path from the top wins over an ending; among
    /// several, the one with the same case wins.
    fn find(&self, segs: &[String]) -> Result<Option<i64>, Vec<i64>> {
        let all = self.ending_with(segs);
        let pick = |v: Vec<(i64, bool, bool)>| -> Result<Option<i64>, Vec<i64>> {
            match v.len() {
                0 => Ok(None),
                1 => Ok(Some(v[0].0)),
                _ => {
                    let exact: Vec<i64> = v.iter().filter(|c| c.1).map(|c| c.0).collect();
                    if exact.len() == 1 { Ok(Some(exact[0])) } else { Err(v.iter().map(|c| c.0).collect()) }
                }
            }
        };
        // a typed path with parents ("Orte > Paris") means the one from the top if there is one
        if segs.len() > 1 {
            let rooted: Vec<(i64, bool, bool)> = all.iter().copied().filter(|c| c.2).collect();
            if !rooted.is_empty() {
                return pick(rooted);
            }
        }
        pick(all)
    }
}

/// Find the keyword meant by typed text ("Labrador", "Hunde > Labrador", "Tiere › Hunde › Labrador"):
/// 1. a keyword whose path is the text or ends with it (a name that occurs exactly once anywhere in the tree);
/// 2. several fit: [`Resolved::Ambiguous`] with their paths – never a silent duplicate;
/// 3. none fits: with `create`, the missing part is created below the longest existing prefix (or at the top).
pub fn resolve(db: &Db, text: &str, create: bool) -> R<Resolved> {
    let segs = split_typed(text);
    if segs.is_empty() {
        return Err("Kein Stichwort angegeben.".into());
    }
    if segs.len() > MAX_DEPTH {
        return Err("Der Pfad ist zu tief.".into());
    }
    let tree = Tree::load(&db.conn)?;
    match tree.find(&segs) {
        Ok(Some(id)) => return Ok(Resolved::Existing(id)),
        Err(ids) => return Ok(Resolved::Ambiguous(ids.into_iter().map(|i| tree.shown(i)).collect())),
        Ok(None) if !create => return Ok(Resolved::Missing),
        Ok(None) => {}
    }
    // the longest prefix that exists exactly once carries the rest
    let mut parent: Option<i64> = None;
    let mut start = 0;
    for k in (1..segs.len()).rev() {
        match tree.find(&segs[..k]) {
            Ok(Some(id)) => {
                parent = Some(id);
                start = k;
                break;
            }
            Ok(None) => {}
            Err(ids) => {
                let rest = segs[k..].join(SHOWN_SEPARATOR);
                return Ok(Resolved::Ambiguous(ids.into_iter().map(|i| format!("{}{SHOWN_SEPARATOR}{rest}", tree.shown(i))).collect()));
            }
        }
    }
    if parent.map_or(0, |p| tree.chain(p).len()) + segs.len() - start > MAX_DEPTH {
        return Err("Der Pfad ist zu tief.".into());
    }
    for s in &segs[start..] {
        parent = Some(child(&db.conn, parent, s)?);
    }
    Ok(Resolved::Created(parent.expect("at least one segment")))
}

/// Text for an ambiguous input (shown in the interface and returned by the text paths).
pub fn ambiguous_message(text: &str, paths: &[String]) -> String {
    format!("„{}“ gibt es mehrfach: {}. Bitte mit Pfad angeben (z. B. „{}“).", text.trim(), paths.join("; "), paths.first().map(|p| p.replace(SHOWN_SEPARATOR, " > ")).unwrap_or_default())
}

/// Add and remove keywords (by id) on several images; only images whose list really changes are written (their
/// keywords count as changed in graineo, their sidecars are queued, their search entries renewed). Returns the
/// number of files touched and the snapshot for ⌘Z ([`crate::manage::undo`] + [`crate::manage::after_keyword_undo`]).
pub fn change(db: &Db, images: &[i64], add: &[i64], remove: &[i64]) -> R<(usize, crate::manage::Snapshot)> {
    let t = crate::now();
    let file_of = |b: i64| -> R<Option<i64>> { db.conn.query_row("SELECT file FROM image WHERE id = ?1", [b], |r| r.get(0)).optional().map_err(f) };
    // remember the sidecar state of files on unreachable locations before anything changes (for merging later)
    let all_files: Vec<i64> = images.iter().filter_map(|&b| file_of(b).ok().flatten()).collect();
    crate::location::note_before_change(db, &all_files);
    let remove: HashSet<i64> = remove.iter().copied().collect();
    let mut added: Vec<(i64, i64)> = Vec::new();
    let mut removed: Vec<(i64, i64)> = Vec::new();
    let mut flags: Vec<(i64, Value)> = Vec::new();
    let mut changed_images: Vec<i64> = Vec::new();
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    for &b in images {
        let have: HashSet<i64> = ids_of(&tx, b)?.into_iter().collect();
        let plus: Vec<i64> = add.iter().copied().filter(|k| !have.contains(k) && !remove.contains(k)).collect();
        let minus: Vec<i64> = have.iter().copied().filter(|k| remove.contains(k)).collect();
        if plus.is_empty() && minus.is_empty() {
            continue;
        }
        let old: Value = tx.query_row("SELECT keywords_changed FROM image WHERE id = ?1", [b], |r| r.get(0)).map_err(f)?;
        flags.push((b, old));
        for k in plus {
            tx.execute("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1, ?2)", params![b, k]).map_err(f)?;
            added.push((b, k));
        }
        for k in minus {
            tx.execute("DELETE FROM image_keyword WHERE image = ?1 AND keyword = ?2", params![b, k]).map_err(f)?;
            removed.push((b, k));
        }
        tx.execute("UPDATE image SET keywords_time = ?2, keywords_changed = ?2 WHERE id = ?1", params![b, t]).map_err(f)?;
        changed_images.push(b);
    }
    tx.commit().map_err(f)?;
    // committed: search entries and sidecars follow; a failure there is a warning on the snapshot, never an error
    // (that would lose the ⌘Z step although the keywords changed)
    let mut snap = crate::manage::assignment_snapshot(added, removed, flags);
    let mut files: Vec<i64> = Vec::new();
    for &b in &changed_images {
        snap.warn("search index", crate::search::update_image(db, b));
        if let Some(Some(d)) = snap.warn("sidecars", file_of(b)) {
            files.push(d);
        }
    }
    files.sort_unstable();
    files.dedup();
    let mut written = 0;
    for d in &files {
        if snap.warn("sidecars", crate::sync::write_sidecar(db, *d)).is_some() {
            written += 1;
        }
    }
    Ok((written, snap))
}

/// Like [`change`] with typed texts (import dialog, command line): additions are resolved and created if missing,
/// removals only resolved. An ambiguous text is an error naming the candidates (nothing is changed then).
pub fn change_text(db: &Db, images: &[i64], add: &[String], remove: &[String]) -> R<usize> {
    let mut plus = Vec::new();
    let mut minus = Vec::new();
    for (list, out, create) in [(add, &mut plus, true), (remove, &mut minus, false)] {
        for text in list {
            if split_typed(text).is_empty() {
                continue;
            }
            match resolve(db, text, create)? {
                Resolved::Existing(id) | Resolved::Created(id) => out.push(id),
                Resolved::Missing => {}
                Resolved::Ambiguous(paths) => return Err(ambiguous_message(text, &paths)),
            }
        }
    }
    change(db, images, &plus, &minus).map(|(n, _)| n)
}

// ───────────────────────────────────────────── Export

/// Settings: write the parent terms of every keyword into exported files too (default on, as Lightroom).
pub const EXPORT_PARENTS_KEY: &str = "exportKeywordParents";
/// Settings: write `lr:hierarchicalSubject` (full paths) into exported files (default off).
pub const EXPORT_HIERARCHY_KEY: &str = "exportKeywordHierarchy";

/// The export choices as set (export dialog, settings).
pub fn export_choices() -> (bool, bool) {
    let parents = crate::settings::value(EXPORT_PARENTS_KEY).as_bool().unwrap_or(true);
    let hierarchy = crate::settings::value(EXPORT_HIERARCHY_KEY).as_bool().unwrap_or(false);
    (parents, hierarchy)
}

/// Keywords marked "never export" with everything below them (the mark holds for the whole branch).
pub fn not_exported(conn: &Connection) -> R<HashSet<i64>> {
    let mut st = conn
        .prepare_cached(
            "WITH RECURSIVE t(id, d) AS (SELECT keyword, 0 FROM keyword_option WHERE export = 0
                UNION SELECT k.id, t.d + 1 FROM keyword k JOIN t ON k.parent = t.id WHERE t.d < 64)
             SELECT DISTINCT id FROM t",
        )
        .map_err(f)?;
    let v = st.query_map([], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<HashSet<i64>>>().map_err(f)?;
    Ok(v)
}

/// Is this keyword itself marked "never export"?
pub fn export_marked(conn: &Connection, id: i64) -> bool {
    conn.query_row("SELECT export = 0 FROM keyword_option WHERE keyword = ?1", [id], |r| r.get::<_, bool>(0)).optional().ok().flatten().unwrap_or(false)
}

/// What an exported file carries of an image's keywords: `dc:subject` (the leaves first, then – with `parents` –
/// every parent term, each once ignoring case) and the full paths for `lr:hierarchicalSubject` ("Orte|Italien|Rom").
/// Never names of people (keywords marked as person or naming a face), also not as a parent term: a keyword below a
/// person ("Christa › Geburtstag", or "Paris › Louvre" once a face was named "Paris") is left out with its whole path.
/// Nothing of a branch marked "never export".
pub fn export_lists(conn: &Connection, image: i64, parents: bool) -> R<(Vec<String>, Vec<String>)> {
    let hidden = not_exported(conn)?;
    let mut st = conn
        .prepare_cached(
            "SELECT k.id FROM image_keyword ik JOIN keyword k ON k.id = ik.keyword
             WHERE ik.image = ?1 AND k.person = 0 AND NOT EXISTS (SELECT 1 FROM face f WHERE f.person = k.id)",
        )
        .map_err(f)?;
    let ids: Vec<i64> = st.query_map([image], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
    let mut paths: Vec<Vec<String>> = Vec::new();
    for id in ids.into_iter().filter(|i| !hidden.contains(i)) {
        if let Some(p) = segments_without_people(conn, id)? {
            paths.push(p);
        }
    }
    paths.sort_by_key(|p| p.join("|").to_lowercase());
    let mut seen: HashSet<String> = HashSet::new();
    let mut subject: Vec<String> = Vec::new();
    let leaves = paths.iter().filter_map(|p| p.last());
    let above: Box<dyn Iterator<Item = &String>> = if parents { Box::new(paths.iter().flat_map(|p| p[..p.len().saturating_sub(1)].iter())) } else { Box::new(std::iter::empty()) };
    for k in leaves.chain(above) {
        if seen.insert(k.trim().to_lowercase()) {
            subject.push(k.clone());
        }
    }
    let hierarchy = paths.iter().map(|p| p.join("|")).collect();
    Ok((subject, hierarchy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (std::path::PathBuf, Db) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("graineo-keywords-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, '');
                 INSERT INTO file(id, folder, name, format) VALUES(10, 1, 'a.jpg', 'JPG'), (11, 1, 'b.jpg', 'JPG');
                 INSERT INTO image(id, file) VALUES(20, 10), (21, 11);
                 INSERT INTO keyword(id, name) VALUES(1, 'Tiere'), (4, 'Orte'), (6, 'Personen'), (8, 'AC/DC'), (9, 'A > B');
                 INSERT INTO keyword(id, name, parent) VALUES(2, 'Hunde', 1), (3, 'Labrador', 2), (5, 'Paris', 4), (7, 'Paris', 6);
                 INSERT INTO image_keyword(image, keyword) VALUES(20, 3), (20, 5), (20, 7), (20, 8), (20, 9);",
            )
            .unwrap();
        (d, db)
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// (b) A name that is in the tree once is that keyword, also when typed without a suggestion; a name in two
    /// branches asks; a path picks one; only what exists nowhere is created.
    #[test]
    fn typed_text_finds_the_keyword_in_the_tree() {
        let (d, db) = library();
        assert_eq!(resolve(&db, "Labrador", true).unwrap(), Resolved::Existing(3));
        assert_eq!(resolve(&db, "labrador", true).unwrap(), Resolved::Existing(3), "case does not matter");
        assert_eq!(resolve(&db, "Hunde > Labrador", true).unwrap(), Resolved::Existing(3), "end of a path");
        assert_eq!(resolve(&db, "Tiere › Hunde › Labrador", true).unwrap(), Resolved::Existing(3));
        match resolve(&db, "Paris", true).unwrap() {
            Resolved::Ambiguous(p) => assert_eq!(p, vec!["Orte › Paris".to_string(), "Personen › Paris".to_string()]),
            other => panic!("{other:?}"),
        }
        assert_eq!(resolve(&db, "Orte > Paris", true).unwrap(), Resolved::Existing(5));
        let n = count(&db, "SELECT count(*) FROM keyword");
        assert_eq!(count(&db, "SELECT count(*) FROM keyword"), n, "nothing created while resolving");
        // new below an existing parent found by its unique name
        let Resolved::Created(id) = resolve(&db, "Hunde > Pudel", true).unwrap() else { panic!() };
        assert_eq!(segments(&db.conn, id).unwrap(), vec!["Tiere", "Hunde", "Pudel"]);
        // new below an ambiguous parent: ask
        assert!(matches!(resolve(&db, "Paris > Eiffelturm", true).unwrap(), Resolved::Ambiguous(_)));
        // really new: at the top
        let Resolved::Created(id) = resolve(&db, "Sommer", true).unwrap() else { panic!() };
        assert_eq!(segments(&db.conn, id).unwrap(), vec!["Sommer"]);
        assert_eq!(resolve(&db, "Winter", false).unwrap(), Resolved::Missing);
        // the text path refuses an ambiguous name instead of creating a duplicate
        assert!(change_text(&db, &[21], &["Paris".into()], &[]).unwrap_err().contains("Orte › Paris"));
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name = 'Paris'"), 2);
        change_text(&db, &[21], &["Labrador".into()], &[]).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name = 'Labrador'"), 1, "no duplicate at the top");
        assert_eq!(ids_of(&db.conn, 21).unwrap(), vec![3]);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// (c) Removing takes exactly the keyword meant, not every keyword with the same name; ⌘Z puts it back and
    /// restores the "changed in graineo" mark.
    #[test]
    fn removing_by_id_takes_only_that_keyword_and_undo() {
        let (d, db) = library();
        let (n, snap) = change(&db, &[20], &[], &[5]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(ids_of(&db.conn, 20).unwrap(), vec![3, 7, 8, 9], "Personen › Paris stays");
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE id = 20 AND keywords_changed IS NOT NULL"), 1);
        crate::manage::undo(&db, &snap).unwrap();
        crate::manage::after_keyword_undo(&db, &snap).unwrap();
        assert_eq!(ids_of(&db.conn, 20).unwrap(), vec![3, 5, 7, 8, 9]);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE id = 20 AND keywords_changed IS NOT NULL"), 0, "mark undone too");
        // adding: only images whose list changes count
        let (n, snap) = change(&db, &[20, 21], &[3], &[]).unwrap();
        assert_eq!(n, 1, "image 20 already had it");
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 1);
        crate::manage::undo(&db, &snap).unwrap();
        assert!(ids_of(&db.conn, 21).unwrap().is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Export: leaves, then parents; nothing of a branch marked "never export", no people.
    #[test]
    fn export_lists_with_parents_and_marks() {
        let (d, db) = library();
        db.conn.execute_batch("INSERT INTO keyword(id, name, parent) VALUES(12, 'KI', NULL), (13, 'Strand', 12); INSERT INTO image_keyword(image, keyword) VALUES(20, 13); UPDATE keyword SET person = 1 WHERE id = 7;").unwrap();
        let (subject, paths) = export_lists(&db.conn, 20, true).unwrap();
        assert_eq!(subject, vec!["A > B", "AC/DC", "Strand", "Paris", "Labrador", "KI", "Orte", "Tiere", "Hunde"]);
        assert!(paths.contains(&"Tiere|Hunde|Labrador".to_string()) && !paths.iter().any(|p| p.contains("Personen")));
        db.conn.execute("INSERT INTO keyword_option(keyword, export) VALUES(12, 0)", []).unwrap();
        let (subject, _) = export_lists(&db.conn, 20, false).unwrap();
        assert_eq!(subject, vec!["A > B", "AC/DC", "Paris", "Labrador"], "the marked branch and the parents are left out");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Export: a keyword below a person never carries the person's name out, neither as a parent term nor in a path
    /// (marked person, or a face named with the keyword).
    #[test]
    fn export_lists_never_name_people_as_parents() {
        let (d, db) = library();
        db.conn
            .execute_batch(
                "INSERT INTO keyword(id, name, parent, person) VALUES(30, 'Christa', 6, 1);
                 INSERT INTO keyword(id, name, parent) VALUES(31, 'Geburtstag', 30), (32, 'Louvre', 5), (33, 'Torte', 31);
                 INSERT INTO image_keyword(image, keyword) VALUES(21, 31), (21, 32), (21, 33), (21, 3);",
            )
            .unwrap();
        let (subject, paths) = export_lists(&db.conn, 21, true).unwrap();
        assert!(!subject.iter().any(|s| s == "Christa") && !paths.iter().any(|p| p.contains("Christa")), "{subject:?} {paths:?}");
        assert!(!subject.iter().any(|s| s == "Geburtstag" || s == "Torte"), "below a person: left out, {subject:?}");
        assert!(subject.contains(&"Louvre".to_string()) && subject.contains(&"Paris".to_string()) && subject.contains(&"Labrador".to_string()), "{subject:?}");
        // "Orte › Paris" becomes a person by a named face: "Louvre" no longer carries "Paris" out
        db.conn.execute("INSERT INTO face(id, image, x, y, w, h, source, person) VALUES(60, 20, 0.1, 0.1, 0.2, 0.2, 'manual', 5)", []).unwrap();
        let (subject, paths) = export_lists(&db.conn, 21, true).unwrap();
        assert!(!subject.iter().any(|s| s == "Paris" || s == "Louvre"), "{subject:?}");
        assert!(!paths.iter().any(|p| p.contains("Paris")), "{paths:?}");
        assert!(subject.contains(&"Labrador".to_string()) && subject.contains(&"Tiere".to_string()), "{subject:?}");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// (a) Names with separators survive everything that moves assignments around: people, virtual copies,
    /// the face undo.
    #[test]
    fn names_with_separators_are_never_split() {
        let (d, db) = library();
        let before = ids_of(&db.conn, 20).unwrap();
        // a person named on a face of the image (rewrites the image's keywords)
        let anna = crate::people::find_or_create_person(&db, "Anna | Berlin").unwrap();
        assert_eq!(segments(&db.conn, anna).unwrap(), vec!["Anna | Berlin"], "a person's name is literal");
        db.conn.execute("INSERT INTO face(id, image, x, y, w, h, source) VALUES(50, 20, 0.1, 0.1, 0.2, 0.2, 'manual')", []).unwrap();
        crate::people::apply_answer_with_name(&db, 50, "Anna | Berlin").unwrap();
        let mut want = before.clone();
        want.push(anna);
        want.sort_unstable();
        assert_eq!(ids_of(&db.conn, 20).unwrap(), want);
        // a virtual copy takes the same ids
        let copy = crate::sync::create_copy(&db, 20, None).unwrap();
        assert_eq!(ids_of(&db.conn, copy).unwrap(), want);
        let names: Vec<String> = shown_of(&db.conn, copy).unwrap().into_iter().map(|x| x.1).collect();
        assert!(names.contains(&"AC/DC".to_string()) && names.contains(&"A > B".to_string()), "{names:?}");
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name IN ('A', 'B', 'AC', 'DC', 'Anna', 'Berlin')"), 0, "nothing split");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A virtual copy belongs to the import and the collections of its original (forum report 1.5.5: a copy made in
    /// a collection or in "Zuletzt importiert" was not in that view, Develop opened the next real photo instead).
    #[test]
    fn a_virtual_copy_joins_the_import_and_collections_of_its_original() {
        let (d, db) = library();
        let run = db.new_run("import", "").unwrap();
        db.conn.execute("UPDATE image SET run = ?1", [run]).unwrap();
        let c = db.create_collection("Auswahl").unwrap();
        db.add_to_collection(c, &[20, 21]).unwrap();
        let copy = crate::sync::create_copy(&db, 20, None).unwrap();
        assert_eq!(count(&db, &format!("SELECT run FROM image WHERE id = {copy}")), run);
        assert_eq!(db.images_of_collection(c).unwrap(), vec![20, copy, 21], "beside its original");
        std::fs::remove_dir_all(d).unwrap();
    }
}
