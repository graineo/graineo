//! Arranging the keyword tree ("Stichwörter ordnen …"): moving several keywords at once, a new parent term for a
//! selection, a new child term, deleting a term while its children move up, merging several terms into one, the
//! counts with everything below, and keeping the search index in step (it holds the whole chain of every keyword).
//!
//! Every change returns a [`Snapshot`] for ⌘Z ([`crate::manage::undo`] + [`crate::manage::after_keyword_undo`]).
//! Only `keyword` rows change when moving (one row per keyword, nothing per photo); the photos below only get their
//! keyword time, their sidecars (if switched on) and their search entries renewed.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Db;
use crate::keywords::{self, MAX_DEPTH};
use crate::manage::{self, Snapshot};

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Photos per short transaction when renewing search entries (the write lock is released in between).
const RENEW_BLOCK: usize = 200;

/// Ids of the keywords and everything below them.
pub fn subtree(conn: &Connection, roots: &[i64]) -> R<Vec<i64>> {
    let mut out: Vec<i64> = Vec::new();
    let mut st = conn.prepare_cached("WITH RECURSIVE t(id, d) AS (SELECT ?1, 0 UNION ALL SELECT s.id, t.d + 1 FROM keyword s JOIN t ON s.parent = t.id WHERE t.d < 64) SELECT DISTINCT id FROM t").map_err(f)?;
    for r in roots {
        out.extend(st.query_map([r], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Photos (image ids) with one of these keywords or one below them.
pub fn images_below(conn: &Connection, roots: &[i64]) -> R<Vec<i64>> {
    let ids = subtree(conn, roots)?;
    let mut out: Vec<i64> = Vec::new();
    let mut st = conn.prepare_cached("SELECT image FROM image_keyword WHERE keyword = ?1").map_err(f)?;
    for k in ids {
        out.extend(st.query_map([k], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Bring the search entries of these photos up to date (their keyword chains changed), in blocks of short
/// transactions: a large branch (11 000 photos) never holds the write lock for long (a rebuild of the whole index
/// held it over 10 s).
pub fn renew_search(db: &Db, images: &[i64]) -> R<()> {
    let mut v = images.to_vec();
    v.sort_unstable();
    v.dedup();
    if v.is_empty() || db.info("search_index").is_none() {
        return Ok(());
    }
    for chunk in v.chunks(RENEW_BLOCK) {
        let tx = db.conn.unchecked_transaction().map_err(f)?;
        for b in chunk {
            crate::search::update_image(db, *b)?;
        }
        tx.commit().map_err(f)?;
    }
    Ok(())
}

fn parent_of(conn: &Connection, id: i64) -> R<Option<i64>> {
    conn.query_row("SELECT parent FROM keyword WHERE id = ?1", [id], |r| r.get(0)).map_err(f)
}

fn name_of(conn: &Connection, id: i64) -> R<String> {
    conn.query_row("SELECT name FROM keyword WHERE id = ?1", [id], |r| r.get(0)).map_err(f)
}

fn depth_of(conn: &Connection, id: i64) -> R<usize> {
    Ok(keywords::segments(conn, id)?.len())
}

/// Height of the subtree below `id` (0: no children).
fn height_of(conn: &Connection, id: i64) -> R<usize> {
    conn.query_row(
        "WITH RECURSIVE t(id, d) AS (SELECT ?1, 0 UNION ALL SELECT s.id, t.d + 1 FROM keyword s JOIN t ON s.parent = t.id WHERE t.d < 64) SELECT max(d) FROM t",
        [id],
        |r| r.get::<_, i64>(0),
    )
    .map(|d| d as usize)
    .map_err(f)
}

/// Why `ids` cannot go below `parent` (`None`: it works): below itself or one of its children, a keyword of the same
/// name already there (or twice among the moved ones), too deep.
pub fn move_problem(db: &Db, ids: &[i64], parent: Option<i64>) -> R<Option<String>> {
    if let Some(p) = parent {
        if subtree(&db.conn, ids)?.contains(&p) {
            return Ok(Some("Ein Stichwort kann nicht unter sich selbst liegen.".into()));
        }
    }
    let moved: HashSet<i64> = ids.iter().copied().collect();
    let mut names: HashSet<String> = HashSet::new();
    let base = parent.map_or(Ok(0), |p| depth_of(&db.conn, p))?;
    for &id in ids {
        if parent_of(&db.conn, id)? == parent {
            continue;
        }
        let name = name_of(&db.conn, id)?;
        if !names.insert(name.trim().to_lowercase()) {
            return Ok(Some(format!("„{name}“ ist zweimal ausgewählt; am selben Ort darf es den Namen nur einmal geben.")));
        }
        if let Some(other) = keywords::sibling_named(&db.conn, parent, &name, Some(id))? {
            if !moved.contains(&other) {
                return Ok(Some(format!("„{name}“ gibt es dort schon.")));
            }
        }
        if base + 1 + height_of(&db.conn, id)? > MAX_DEPTH {
            return Ok(Some("Der Pfad wäre zu tief.".into()));
        }
    }
    Ok(None)
}

/// Move several keywords below `parent` (`None`: top level) in one step (one ⌘Z). Once the move is committed, a
/// failure of the follow-up steps (keyword times, sidecars, search index) is a warning on the snapshot, never an error.
pub fn move_keywords(db: &Db, ids: &[i64], parent: Option<i64>) -> R<Snapshot> {
    let mut snap = move_keywords_quiet(db, ids, parent)?;
    renew_below(db, &mut snap, ids);
    Ok(snap)
}

/// Like [`move_keywords`] without renewing the search entries (several moves in a row renew them once).
pub fn move_keywords_quiet(db: &Db, ids: &[i64], parent: Option<i64>) -> R<Snapshot> {
    if let Some(why) = move_problem(db, ids, parent)? {
        return Err(why);
    }
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let mut snap = move_in(&tx, ids, parent)?;
    tx.commit().map_err(f)?;
    touch_below(db, &mut snap, ids);
    Ok(snap)
}

/// The moves of [`move_keywords`] inside a transaction the caller holds: the snapshot holds the old parents.
fn move_in(tx: &Connection, ids: &[i64], parent: Option<i64>) -> R<Snapshot> {
    let mut snap = Snapshot::default();
    for &id in ids {
        let old = parent_of(tx, id)?;
        if old == parent {
            continue;
        }
        tx.execute("UPDATE keyword SET parent = ?1 WHERE id = ?2", params![parent, id]).map_err(f)?;
        snap.add_changed("keyword", id, "parent", old.map_or(Value::Null, Value::Integer));
    }
    Ok(snap)
}

/// After a committed change: keyword times and sidecars of the photos with these keywords (or below them).
fn touch_below(db: &Db, snap: &mut Snapshot, ids: &[i64]) {
    let mut files: Vec<i64> = Vec::new();
    for r in ids {
        files.extend(snap.warn("keyword times and sidecars", manage::keyword_images(&db.conn, *r)).unwrap_or_default());
    }
    files.sort_unstable();
    files.dedup();
    snap.warn("keyword times and sidecars", manage::touch_keywords(&db.conn, &files));
}

/// After a committed change: the search entries of the photos with these keywords (or below them).
fn renew_below(db: &Db, snap: &mut Snapshot, ids: &[i64]) {
    let images = snap.warn("search index", images_below(&db.conn, ids)).unwrap_or_default();
    snap.warn("search index", renew_search(db, &images));
}

/// Rename a keyword ([`manage::rename_keyword`]); smart collections naming it follow (same ⌘Z step), unless a term
/// naming it also finds other keywords (then the rule stays and the collection is named in the snapshot).
pub fn rename(db: &Db, id: i64, name: &str) -> R<Snapshot> {
    let plan = crate::keyword_rules::plan(db, id, None)?;
    let mut snap = manage::rename_keyword(db, id, name)?;
    // committed: the rules follow, a failure only warns
    let rules = crate::keyword_rules::rule_text(db, id).and_then(|t| crate::keyword_rules::follow(db, &plan, &t));
    if let Some(r) = snap.warn("smart collections", rules) {
        snap.merge(r);
    }
    Ok(snap)
}

/// A new parent term `name` for the selection: created where the selected keywords lie (all at one place) or at the
/// top level, and the selection moved below it. An existing keyword of that name there is used instead of a second
/// one. Returns its id.
pub fn new_parent(db: &Db, ids: &[i64], name: &str) -> R<(i64, Snapshot)> {
    let name = manage::clean_name(name)?;
    if ids.is_empty() {
        return Err("Keine Stichwörter ausgewählt.".into());
    }
    let parents: HashSet<Option<i64>> = ids.iter().map(|&i| parent_of(&db.conn, i)).collect::<R<_>>()?;
    let place = if parents.len() == 1 { parents.into_iter().next().flatten() } else { None };
    let existing = keywords::sibling_named(&db.conn, place, &name, None)?;
    if existing.is_some_and(|e| ids.contains(&e)) {
        return Err(format!("„{name}“ ist selbst ausgewählt."));
    }
    // creating the term and moving the selection: one transaction (all or nothing). A refusal or an error rolls it
    // back – nothing is ever deleted afterwards (a delete would cascade to the keywords already moved below it).
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let (id, created) = match existing {
        Some(e) => (e, false),
        None => {
            tx.execute("INSERT INTO keyword(name, parent) VALUES(?1, ?2)", params![&name, place]).map_err(f)?;
            (tx.last_insert_rowid(), true)
        }
    };
    // (same connection: the checks see the new term)
    if let Some(why) = move_problem(db, ids, Some(id))? {
        return Err(why);
    }
    let mut snap = move_in(&tx, ids, Some(id))?;
    tx.commit().map_err(f)?;
    if created {
        snap.add_created_keywords(&[id]);
    }
    // committed: keyword times, sidecars and search entries follow, a failure only warns
    touch_below(db, &mut snap, ids);
    renew_below(db, &mut snap, ids);
    Ok((id, snap))
}

/// A new keyword below `parent` (`None`: top level); typed paths ("Hunde > Labrador") create every missing level.
/// Refused if it exists already.
pub fn new_child(db: &Db, parent: Option<i64>, text: &str) -> R<(i64, Snapshot)> {
    let segs = keywords::split_typed(text);
    if segs.is_empty() {
        return Err("Der Name darf nicht leer sein.".into());
    }
    if parent.map_or(Ok(0), |p| depth_of(&db.conn, p))? + segs.len() > MAX_DEPTH {
        return Err("Der Pfad wäre zu tief.".into());
    }
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let mut cur = parent;
    let mut created = Vec::new();
    for s in &segs {
        cur = Some(match keywords::sibling_named(&tx, cur, s, None)? {
            Some(e) => e,
            None => {
                tx.execute("INSERT INTO keyword(name, parent) VALUES(?1, ?2)", params![s, cur]).map_err(f)?;
                let id = tx.last_insert_rowid();
                created.push(id);
                id
            }
        });
    }
    if created.is_empty() {
        return Err(format!("„{}“ gibt es dort schon.", segs.join(keywords::SHOWN_SEPARATOR)));
    }
    tx.commit().map_err(f)?;
    let mut snap = Snapshot::default();
    snap.add_created_keywords(&created);
    Ok((cur.expect("one segment at least"), snap))
}

/// What deleting a keyword costs: (photos with it or one below it, photos with exactly it, children).
pub fn delete_loss(db: &Db, id: i64) -> R<(usize, usize, usize)> {
    let all = manage::keyword_loss(db, id)?;
    let own: i64 = db.conn.query_row("SELECT count(DISTINCT image) FROM image_keyword WHERE keyword = ?1", [id], |r| r.get(0)).map_err(f)?;
    let children: i64 = db.conn.query_row("SELECT count(*) FROM keyword WHERE parent = ?1", [id], |r| r.get(0)).map_err(f)?;
    Ok((all, own as usize, children as usize))
}

/// Delete only this keyword: its children move up one level (to its parent), its own assignments go. A child whose
/// name exists at the new place already is refused (merge it first).
pub fn delete_keep_children(db: &Db, id: i64) -> R<Snapshot> {
    let parent = parent_of(&db.conn, id)?;
    let children: Vec<i64> = {
        let mut st = db.conn.prepare("SELECT id FROM keyword WHERE parent = ?1").map_err(f)?;
        let v = st.query_map([id], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<i64>>>().map_err(f)?;
        v
    };
    for &c in &children {
        let name = name_of(&db.conn, c)?;
        if keywords::sibling_named(&db.conn, parent, &name, Some(id))?.is_some_and(|o| o != c) {
            return Err(format!("„{name}“ gibt es eine Ebene höher schon. Bitte zuerst zusammenführen oder umbenennen."));
        }
    }
    let files = manage::keyword_images(&db.conn, id)?;
    let images = images_below(&db.conn, &[id])?;
    let mut changes = Snapshot::default();
    // children up and the keyword away: one transaction
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    for &c in &children {
        tx.execute("UPDATE keyword SET parent = ?1 WHERE id = ?2", params![parent, c]).map_err(f)?;
        changes.add_changed("keyword", c, "parent", Value::Integer(id));
    }
    let mut snap = manage::remove_rows_in(&tx, "keyword", "id", &[Value::Integer(id)], Snapshot::default())?;
    tx.commit().map_err(f)?;
    // put back: the keyword first (tables), then the children below it again (changed)
    snap.merge(changes);
    // committed: what follows only warns
    manage::follow_up(db, &mut snap, &files, &images);
    Ok(snap)
}

/// Delete a keyword with everything below it ([`manage::delete_keyword`]) and renew the search entries (after the
/// delete is committed a failure there only warns).
pub fn delete_with_children(db: &Db, id: i64) -> R<Snapshot> {
    let images = images_below(&db.conn, &[id])?;
    let mut snap = manage::delete_keyword(db, id)?;
    snap.warn("search index", renew_search(db, &images));
    Ok(snap)
}

/// Merge several keywords into `into` (one ⌘Z). Each is checked as by [`manage::merge_keyword`]; if one is refused,
/// the ones already merged are put back and the reason comes as the error. Once merged, failures of the follow-up
/// steps (smart rules, keyword times, sidecars, search index) are warnings on the snapshot, never an error.
pub fn merge_keywords(db: &Db, from: &[i64], into: i64) -> R<Snapshot> {
    let mut snap = Snapshot::default();
    for &k in from {
        // smart collections naming the merged keyword name the target afterwards (decided before the merge)
        let plan = if snap.is_empty() { crate::keyword_rules::plan(db, k, Some(into))? } else { snap.warn("smart collections", crate::keyword_rules::plan(db, k, Some(into))).unwrap_or_default() };
        match manage::merge_keyword(db, k, into) {
            Ok(s) => {
                snap.merge(s);
                let rules = crate::keyword_rules::rule_text(db, into).and_then(|t| crate::keyword_rules::follow(db, &plan, &t));
                if let Some(r) = snap.warn("smart collections", rules) {
                    snap.merge(r);
                }
            }
            Err(e) => {
                if snap.is_empty() {
                    return Err(e);
                }
                match manage::undo(db, &snap) {
                    Ok(()) => {
                        if let Err(x) = manage::after_keyword_undo(db, &snap) {
                            eprintln!("merge refused, earlier merges put back; follow-up failed: {x}");
                        }
                        return Err(e);
                    }
                    // the earlier merges stand: keep their ⌘Z step and name the refusal as a warning
                    Err(x) => {
                        snap.warnings.push(format!("{e} (put back failed: {x})"));
                        break;
                    }
                }
            }
        }
    }
    renew_below(db, &mut snap, &[into]);
    Ok(snap)
}

/// Mark keywords "never export" (`export = false`; the mark holds for everything below them) or take the mark
/// away. One ⌘Z step; the sidecars are not touched (the mark only concerns exported files).
pub fn set_export(db: &Db, ids: &[i64], export: bool) -> R<Snapshot> {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    for &id in ids {
        let row: Option<(i64, Option<String>)> = tx.query_row("SELECT export, synonyms FROM keyword_option WHERE keyword = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(f)?;
        // the mark taken away is kept as `export = 1` (not deleted): a later Lightroom import only adds marks that are
        // missing ("INSERT OR IGNORE") and must not bring it back
        let want = i64::from(export);
        match row {
            None if !export => {
                tx.execute("INSERT INTO keyword_option(keyword, export) VALUES(?1, 0)", [id]).map_err(f)?;
                added.push(vec![Value::Integer(id), Value::Integer(0)]);
            }
            Some((old, syn)) if (old != 0) != export => {
                tx.execute("UPDATE keyword_option SET export = ?2 WHERE keyword = ?1", params![id, want]).map_err(f)?;
                // ⌘Z: the changed row goes, the old one comes back (keyword_option is replaced by key, see manage::undo)
                added.push(vec![Value::Integer(id), Value::Integer(want)]);
                removed.push(vec![Value::Integer(id), Value::Integer(old), syn.map_or(Value::Null, Value::Text)]);
            }
            _ => {}
        }
    }
    tx.commit().map_err(f)?;
    let mut snap = Snapshot::default();
    snap.add_rows("keyword_option", &["keyword", "export"], added, Vec::new());
    snap.add_rows("keyword_option", &["keyword", "export", "synonyms"], Vec::new(), removed);
    Ok(snap)
}

/// Keywords that Lightroom does not include on export (`includeOnExport = 0`) and that are not marked here yet: for
/// the action "Marken aus Lightroom übernehmen" (asked first, never by itself). The catalog is only read
/// ([`crate::lightroom::open_catalog`]: `mode=ro`, `query_only`). Returns graineo keyword ids.
pub fn lightroom_export_marks(db: &Db, catalog: &std::path::Path) -> R<Vec<i64>> {
    let c = crate::lightroom::open_catalog(catalog).map_err(|e| format!("Lightroom-Katalog: {e}"))?;
    // the ids of another catalog collide with the ones of the library: refused as by the import
    let schema = crate::lightroom::CatalogSchema::read(&c).map_err(f)?;
    crate::lightroom::check_catalog(&db.conn, &c, &schema)?;
    let has = c.prepare("SELECT includeOnExport FROM AgLibraryKeyword LIMIT 0").is_ok();
    if !has {
        return Ok(Vec::new());
    }
    let mut st = c.prepare("SELECT id_local FROM AgLibraryKeyword WHERE includeOnExport = 0").map_err(f)?;
    let lr: Vec<i64> = st.query_map([], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
    let marked = export_marks(&db.conn)?;
    let mut find = db.conn.prepare_cached("SELECT id FROM keyword WHERE lr_id = ?1").map_err(f)?;
    let mut out = Vec::new();
    for l in lr {
        if let Some(id) = find.query_row([l], |r| r.get::<_, i64>(0)).optional().map_err(f)? {
            if !marked.contains(&id) {
                out.push(id);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Keywords marked "never export" themselves (not those below them).
pub fn export_marks(conn: &Connection) -> R<HashSet<i64>> {
    let mut st = conn.prepare("SELECT keyword FROM keyword_option WHERE export = 0").map_err(f)?;
    let v = st.query_map([], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<HashSet<i64>>>().map_err(f)?;
    Ok(v)
}

/// Photos with each keyword or one below it (distinct photos, without those marked as gone), for all keywords
/// in one query (measured 0.1–0.5 s on 11 000 keywords).
pub fn subtree_counts(conn: &Connection) -> R<HashMap<i64, usize>> {
    let mut st = conn
        .prepare(
            "WITH RECURSIVE anc(k, a, d) AS (SELECT id, id, 0 FROM keyword
                UNION ALL SELECT anc.k, s.parent, anc.d + 1 FROM anc JOIN keyword s ON s.id = anc.a WHERE s.parent IS NOT NULL AND anc.d < 64)
             SELECT anc.a, count(DISTINCT ik.image) FROM anc JOIN image_keyword ik ON ik.keyword = anc.k
                JOIN image b ON b.id = ik.image JOIN file d ON d.id = b.file WHERE d.status <> 4 GROUP BY anc.a",
        )
        .map_err(f)?;
    let v = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize))).map_err(f)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(f)?;
    Ok(v)
}

/// The keyword `id` exists (it may have been removed by ⌘Z or another view meanwhile).
pub fn exists(conn: &Connection, id: i64) -> bool {
    conn.query_row("SELECT 1 FROM keyword WHERE id = ?1", [id], |_| Ok(())).optional().ok().flatten().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (std::path::PathBuf, Db) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("graineo-arrange-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, '');
                 INSERT INTO file(id, folder, name, format) VALUES(10, 1, 'a.jpg', 'JPG'), (11, 1, 'b.jpg', 'JPG'), (12, 1, 'c.jpg', 'JPG');
                 INSERT INTO image(id, file) VALUES(20, 10), (21, 11), (22, 12);
                 INSERT INTO keyword(id, name) VALUES(1, 'Hund'), (2, 'Katze'), (3, 'Pferd'), (4, 'Orte'), (7, 'Paris');
                 INSERT INTO keyword(id, name, parent) VALUES(5, 'Paris', 4), (6, 'Louvre', 5);
                 INSERT INTO image_keyword(image, keyword) VALUES(20, 1), (21, 2), (21, 1), (22, 6), (22, 4);",
            )
            .unwrap();
        (d, db)
    }

    fn parent(db: &Db, id: i64) -> Option<i64> {
        parent_of(&db.conn, id).unwrap()
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// A follow-up step after the commit fails: setting the keyword time of the photos (disk full, lock, …).
    fn break_keyword_times(db: &Db) {
        db.conn.execute_batch("CREATE TRIGGER fail_times BEFORE UPDATE OF keywords_time ON image BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
    }

    /// A follow-up step after the commit fails: the search index is broken.
    fn break_search_index(db: &Db) {
        db.conn.execute("INSERT OR REPLACE INTO info(name, value) VALUES('search_index', '1')", []).unwrap();
        db.conn.execute_batch("DROP TABLE IF EXISTS search_word; CREATE TABLE search_word(x);").unwrap();
    }

    /// Regression (review 1.2.0, H1): a failure after the move never deletes the new parent term (that cascaded to the
    /// selected keywords and their assignments). The change stands, ⌘Z works, the failure is a warning.
    #[test]
    fn new_parent_failure_after_move_loses_nothing() {
        let (d, db) = library();
        break_keyword_times(&db);
        let before = count(&db, "SELECT count(*) FROM image_keyword");
        let (tiere, snap) = new_parent(&db, &[1, 2], "Tiere").expect("committed: no error");
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (1, 2)"), 2, "Hund and Katze are still there");
        assert_eq!((parent(&db, 1), parent(&db, 2)), (Some(tiere), Some(tiere)));
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword"), before, "no assignment lost");
        assert!(!snap.warnings().is_empty(), "the failed follow-up step is reported");
        manage::undo(&db, &snap).unwrap();
        assert_eq!((parent(&db, 1), parent(&db, 2)), (None, None));
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name = 'Tiere'"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword"), before);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A refused new parent term (the move is not possible) leaves nothing behind: one transaction.
    #[test]
    fn new_parent_refused_leaves_nothing() {
        let (d, db) = library();
        // "Paris" twice in the selection: refused after the term was created inside the transaction
        assert!(new_parent(&db, &[5, 7], "Städte").is_err());
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name = 'Städte'"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM keyword"), 7);
        assert_eq!(parent(&db, 5), Some(4));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Regression (review 1.2.0, M1): deleting with everything below commits; a broken search index afterwards keeps
    /// the ⌘Z step (warning instead of an error).
    #[test]
    fn delete_failure_after_commit_keeps_undo() {
        let (d, db) = library();
        break_search_index(&db);
        let snap = delete_with_children(&db, 4).expect("committed: no error");
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (4, 5, 6)"), 0);
        assert!(!snap.warnings().is_empty());
        manage::undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (4, 5, 6)"), 3);
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 22"), 2);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// M1 for every other keyword path: after the commit, a failed follow-up step (keyword times or search index) is a
    /// warning on the snapshot; the result is `Ok` and ⌘Z puts the change back.
    #[test]
    fn follow_up_failures_keep_undo_on_every_path() {
        for broken in [break_keyword_times as fn(&Db), break_search_index] {
            // delete, children move up
            let (d, db) = library();
            db.conn.execute("UPDATE keyword SET name = 'Paris (Stadt)' WHERE id = 7", []).unwrap();
            broken(&db);
            let s = delete_keep_children(&db, 4).expect("delete_keep_children");
            assert!(!s.warnings().is_empty());
            manage::undo(&db, &s).unwrap();
            assert_eq!((parent(&db, 5), count(&db, "SELECT count(*) FROM image_keyword WHERE image = 22")), (Some(4), 2));
            std::fs::remove_dir_all(d).unwrap();
            // move several
            let (d, db) = library();
            broken(&db);
            let s = move_keywords(&db, &[1, 2], Some(4)).expect("move_keywords");
            assert!(!s.warnings().is_empty());
            manage::undo(&db, &s).unwrap();
            assert_eq!((parent(&db, 1), parent(&db, 2)), (None, None));
            std::fs::remove_dir_all(d).unwrap();
            // merge several
            let (d, db) = library();
            broken(&db);
            let s = merge_keywords(&db, &[2, 3], 1).expect("merge_keywords");
            assert!(!s.warnings().is_empty());
            assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (2, 3)"), 0);
            manage::undo(&db, &s).unwrap();
            assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 21 AND keyword = 2"), 1);
            std::fs::remove_dir_all(d).unwrap();
            // rename (with smart rules) and move one
            let (d, db) = library();
            broken(&db);
            let s = rename(&db, 1, "Hunde").expect("rename");
            assert!(!s.warnings().is_empty());
            manage::undo(&db, &s).unwrap();
            assert_eq!(name_of(&db.conn, 1).unwrap(), "Hund");
            let s = manage::move_keyword(&db, 1, Some(4)).expect("move_keyword");
            assert!(!s.warnings().is_empty());
            manage::undo(&db, &s).unwrap();
            assert_eq!(parent(&db, 1), None);
            std::fs::remove_dir_all(d).unwrap();
            // suggestions
            let (d, db) = library();
            broken(&db);
            use crate::keyword_suggest::Change;
            let (s, done, skipped) = crate::keyword_suggest::apply(&db, &[Change::Merge { from: 2, into: 1 }, Change::Below { child: 3, parent: 4 }]).expect("apply");
            assert_eq!((done, skipped.len()), (2, 0));
            assert!(!s.warnings().is_empty());
            manage::undo(&db, &s).unwrap();
            assert_eq!((count(&db, "SELECT count(*) FROM keyword WHERE id = 2"), parent(&db, 3)), (1, None));
            std::fs::remove_dir_all(d).unwrap();
        }
        // keywords set by hand: the keyword time is part of the change itself (all or nothing), the search index follows
        let (d, db) = library();
        break_search_index(&db);
        let (_, s) = keywords::change(&db, &[20, 22], &[3], &[1]).expect("change");
        assert!(!s.warnings().is_empty());
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE keyword = 3"), 2);
        manage::undo(&db, &s).unwrap();
        assert_eq!((count(&db, "SELECT count(*) FROM image_keyword WHERE keyword = 3"), count(&db, "SELECT count(*) FROM image_keyword WHERE image = 20 AND keyword = 1")), (0, 1));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Review 1.2.0, M3: a bare name that exists twice, or a partial term that also finds other keywords, is not
    /// rewritten (the collection would show other photos); the collection is named. Unique terms follow and are
    /// counted; ⌘Z takes them back in the same step.
    #[test]
    fn smart_rules_keep_ambiguous_terms() {
        let (d, db) = library();
        db.conn
            .execute_batch(
                "INSERT INTO keyword(id, name) VALUES(8, 'Personen'), (11, 'Romantik');
                 INSERT INTO keyword(id, name, parent) VALUES(9, 'Rom', 4), (10, 'Rom', 8);
                 INSERT INTO collection(id, name, kind, search) VALUES
                    (40, 'Beide Rom', 2, 'keyword:=Rom'), (41, 'Teilwort', 2, 'keyword:rom'), (42, 'Pfad', 2, 'keyword:=\"Orte > Rom\" rating>=1');",
            )
            .unwrap();
        let rule = |id: i64| db.conn.query_row("SELECT search FROM collection WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
        let s = rename(&db, 9, "Roma").unwrap();
        assert_eq!(rule(40), "keyword:=Rom", "two keywords named Rom: unchanged");
        assert_eq!(rule(41), "keyword:rom", "also finds Romantik: unchanged");
        assert_eq!(rule(42), "keyword:=Roma rating>=1", "the path was unique: follows (Roma is unique by name)");
        assert_eq!(s.smart_changed(), 1);
        let mut kept = s.smart_kept().to_vec();
        kept.sort();
        assert_eq!(kept, vec!["Beide Rom".to_string(), "Teilwort".to_string()]);
        manage::undo(&db, &s).unwrap();
        assert_eq!(rule(42), "keyword:=\"Orte > Rom\" rating>=1");
        assert_eq!(name_of(&db.conn, 9).unwrap(), "Rom");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn new_parent_term_for_a_selection_and_undo() {
        let (d, db) = library();
        let (tiere, snap) = new_parent(&db, &[1, 2, 3], "Tiere").unwrap();
        assert_eq!((parent(&db, 1), parent(&db, 2), parent(&db, 3)), (Some(tiere), Some(tiere), Some(tiere)));
        assert_eq!(subtree_counts(&db.conn).unwrap()[&tiere], 2, "photos 20 and 21, 21 once");
        manage::undo(&db, &snap).unwrap();
        manage::after_keyword_undo(&db, &snap).unwrap();
        assert_eq!((parent(&db, 1), parent(&db, 2)), (None, None));
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name = 'Tiere'"), 0, "the new term is gone, its children are not");
        assert_eq!(count(&db, "SELECT count(*) FROM keyword"), 7);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn moving_several_checks_names_and_cycles() {
        let (d, db) = library();
        // "Paris" (top) below "Orte", where "Paris" exists: refused, nothing moved
        assert!(move_keywords(&db, &[1, 7], Some(4)).unwrap_err().contains("Paris"));
        assert_eq!(parent(&db, 1), None);
        assert!(move_keywords(&db, &[4], Some(6)).is_err(), "never below its own child");
        let s = move_keywords(&db, &[1, 2], Some(4)).unwrap();
        assert_eq!((parent(&db, 1), parent(&db, 2)), (Some(4), Some(4)));
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 0);
        manage::undo(&db, &s).unwrap();
        assert_eq!((parent(&db, 1), parent(&db, 2)), (None, None));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn deleting_a_parent_term_two_ways_and_undo() {
        let (d, db) = library();
        assert_eq!(delete_loss(&db, 4).unwrap(), (1, 1, 1));
        // only "Orte": "Paris" moves up – but a "Paris" is at the top already: refused
        assert!(delete_keep_children(&db, 4).unwrap_err().contains("Paris"));
        db.conn.execute("UPDATE keyword SET name = 'Paris (Stadt)' WHERE id = 7", []).unwrap();
        let s = delete_keep_children(&db, 4).unwrap();
        assert_eq!(parent(&db, 5), None, "children one level up");
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 22"), 1, "only the own assignment of Orte went");
        manage::undo(&db, &s).unwrap();
        manage::after_keyword_undo(&db, &s).unwrap();
        assert_eq!(parent(&db, 5), Some(4));
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 22"), 2);
        // with everything below
        let s = delete_with_children(&db, 4).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (4, 5, 6)"), 0);
        manage::undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (4, 5, 6)"), 3);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn new_child_and_merging_several() {
        let (d, db) = library();
        let (id, s) = new_child(&db, Some(4), "Frankreich > Lyon").unwrap();
        assert_eq!(keywords::segments(&db.conn, id).unwrap(), vec!["Orte", "Frankreich", "Lyon"]);
        assert!(new_child(&db, Some(4), "frankreich > lyon").is_err(), "exists already");
        manage::undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE name IN ('Frankreich', 'Lyon')"), 0);
        let s = merge_keywords(&db, &[2, 3], 1).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id IN (2, 3)"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE keyword = 1"), 2);
        manage::undo(&db, &s).unwrap();
        manage::after_keyword_undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 21 AND keyword = 2"), 1);
        // a refused merge in the middle puts the earlier ones back
        db.conn.execute("UPDATE keyword SET person = 1 WHERE id = 3", []).unwrap();
        assert!(merge_keywords(&db, &[2, 3], 1).is_err());
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id = 2"), 1);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn export_mark_and_undo() {
        let (d, db) = library();
        let s = set_export(&db, &[4], false).unwrap();
        assert!(keywords::not_exported(&db.conn).unwrap().contains(&6), "the mark holds for the branch");
        manage::undo(&db, &s).unwrap();
        assert!(export_marks(&db.conn).unwrap().is_empty());
        set_export(&db, &[4], false).unwrap();
        let s = set_export(&db, &[4], true).unwrap();
        assert!(export_marks(&db.conn).unwrap().is_empty());
        manage::undo(&db, &s).unwrap();
        assert_eq!(export_marks(&db.conn).unwrap(), HashSet::from([4]));
        // review 1.2.0, N7: a mark taken away stays away after a Lightroom import ("INSERT OR IGNORE … export 0")
        let s = set_export(&db, &[4], true).unwrap();
        db.conn.execute("INSERT OR IGNORE INTO keyword_option(keyword, export) VALUES(4, 0)", []).unwrap();
        assert!(export_marks(&db.conn).unwrap().is_empty());
        manage::undo(&db, &s).unwrap();
        assert_eq!(export_marks(&db.conn).unwrap(), HashSet::from([4]));
        let s = set_export(&db, &[4], true).unwrap();
        let s2 = set_export(&db, &[4], false).unwrap();
        assert_eq!(export_marks(&db.conn).unwrap(), HashSet::from([4]), "marked again over a kept row");
        manage::undo(&db, &s2).unwrap();
        manage::undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword_option WHERE keyword = 4 AND export = 0"), 1);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Smart collections follow a rename and a merge (decision 7 B); ⌘Z takes the old rule back.
    #[test]
    fn smart_rules_follow_rename_and_merge() {
        let (d, db) = library();
        db.conn.execute("INSERT INTO collection(id, name, kind, search) VALUES(40, 'Hunde', 2, 'keyword:=\"Hund\" rating>=2'), (41, 'Orte', 2, 'keyword:=\"Orte > Paris\"')", []).unwrap();
        let rule = |id: i64| db.conn.query_row("SELECT search FROM collection WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
        let s = rename(&db, 1, "Hunde").unwrap();
        assert_eq!(rule(40), "keyword:=Hunde rating>=2");
        manage::undo(&db, &s).unwrap();
        assert_eq!(rule(40), "keyword:=\"Hund\" rating>=2");
        // "Paris" exists twice: the rule names the path of the target
        let s = merge_keywords(&db, &[5], 7).unwrap();
        assert_eq!(rule(41), "keyword:=Paris", "the remaining Paris is unique now");
        manage::undo(&db, &s).unwrap();
        assert_eq!(rule(41), "keyword:=\"Orte > Paris\"");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// "Marken aus Lightroom übernehmen": only keywords from the catalog with includeOnExport = 0 not yet marked;
    /// the catalog stays untouched.
    #[test]
    fn export_marks_from_a_lightroom_catalog() {
        let (d, db) = library();
        let cat = d.join("mini.lrcat");
        {
            let c = Connection::open(&cat).unwrap();
            c.execute_batch(
                "CREATE TABLE AgLibraryKeyword(id_local INTEGER PRIMARY KEY, name TEXT, includeOnExport INTEGER NOT NULL DEFAULT 1);
                 INSERT INTO AgLibraryKeyword(id_local, name, includeOnExport) VALUES(501, 'Hund', 0), (502, 'Katze', 1), (503, 'Pferd', 0), (504, 'Weg', 0);",
            )
            .unwrap();
        }
        db.conn.execute_batch("UPDATE keyword SET lr_id = 501 WHERE id = 1; UPDATE keyword SET lr_id = 502 WHERE id = 2; UPDATE keyword SET lr_id = 503 WHERE id = 3; INSERT INTO keyword_option(keyword, export) VALUES(3, 0);").unwrap();
        let before = std::fs::read(&cat).unwrap();
        assert_eq!(lightroom_export_marks(&db, &cat).unwrap(), vec![1], "Pferd is marked already, 504 is not in the library");
        assert_eq!(std::fs::read(&cat).unwrap(), before, "catalog only read");
        let s = set_export(&db, &[1], false).unwrap();
        assert!(lightroom_export_marks(&db, &cat).unwrap().is_empty());
        manage::undo(&db, &s).unwrap();
        assert_eq!(lightroom_export_marks(&db, &cat).unwrap(), vec![1]);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Review 1.2.0, N8: "Marken aus Lightroom übernehmen" refuses another catalog than the one the library came from
    /// (its ids collide), as the import does.
    #[test]
    fn export_marks_refuse_another_catalog() {
        let (d, db) = library();
        let cat = d.join("other.lrcat");
        {
            let c = Connection::open(&cat).unwrap();
            c.execute_batch(
                "CREATE TABLE AgLibraryKeyword(id_local INTEGER PRIMARY KEY, name TEXT, includeOnExport INTEGER NOT NULL DEFAULT 1);
                 INSERT INTO AgLibraryKeyword(id_local, name, includeOnExport) VALUES(501, 'Strand', 0);
                 CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, baseName TEXT, extension TEXT);
                 INSERT INTO AgLibraryFile(id_local, baseName, extension) VALUES(900, 'IMG_0001', 'CR3'), (901, 'IMG_0002', 'CR3'), (902, 'IMG_0003', 'CR3');",
            )
            .unwrap();
        }
        db.conn.execute_batch("UPDATE keyword SET lr_id = 501 WHERE id = 1; UPDATE file SET lr_id = 900 WHERE id = 10; UPDATE file SET lr_id = 901 WHERE id = 11; UPDATE file SET lr_id = 902 WHERE id = 12;").unwrap();
        let e = lightroom_export_marks(&db, &cat).unwrap_err();
        assert!(e.starts_with(crate::lightroom::OTHER_CATALOG), "{e}");
        // the same catalog (names fit): accepted
        Connection::open(&cat).unwrap().execute_batch("UPDATE AgLibraryFile SET baseName = 'a', extension = 'jpg' WHERE id_local = 900; UPDATE AgLibraryFile SET baseName = 'b', extension = 'jpg' WHERE id_local = 901; UPDATE AgLibraryFile SET baseName = 'c', extension = 'jpg' WHERE id_local = 902;").unwrap();
        assert_eq!(lightroom_export_marks(&db, &cat).unwrap(), vec![1]);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The search index holds the chain of every keyword: after moving, a photo is found by its new parent term.
    #[test]
    fn search_follows_a_move() {
        let (d, db) = library();
        crate::search::build_index(&db).unwrap();
        let (_, _) = new_parent(&db, &[1], "Haustiere").unwrap();
        assert!(crate::search::word_matches(&db, "haustiere", false, false).contains(&20));
        std::fs::remove_dir_all(d).unwrap();
    }
}
