//! Own order of the collections, sets and smart collections in the left panel (table `collection_order`).
//!
//! Every level (all collections with the same parent) has its own order. A stored position only counts while its
//! `parent` equals the collection's parent: whoever changes the parent without knowing the table (an older version,
//! a Lightroom import, the Apple Photos sync) leaves the collection "not placed", and it shows behind the placed ones
//! of its new level. Not placed collections keep the old order among themselves (sets first, then by name), so a level
//! nobody ever ordered looks exactly as before.
//!
//! The first step in a level writes the order shown at that moment for all its members; later newcomers are not placed
//! and therefore come last. Ordering never touches `collection_image` (which photos a collection holds), never deletes
//! anything and returns a [`Snapshot`] for ⌘Z ([`crate::manage::undo`]).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::db::{Collection, Db};
use crate::manage::{Snapshot, TableRows};

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Kind of a collection set (column `collection.kind`).
const SET: u8 = 1;
/// Kind of the quick collection: never part of the list.
const QUICK: u8 = 3;

/// Sort key of the display within a level: placed ones by position, then the others as before (sets first, then by
/// name, case-insensitive); the id keeps equal names stable.
pub fn display_key(position: Option<i64>, kind: u8, name: &str, id: i64) -> (bool, i64, bool, String, i64) {
    (position.is_none(), position.unwrap_or(0), kind != SET, name.to_lowercase(), id)
}

pub fn compare(a: &Collection, b: &Collection) -> Ordering {
    display_key(a.position, a.kind, &a.name, a.id).cmp(&display_key(b.position, b.kind, &b.name, b.id))
}

/// Children of every level in display order (indexes into the list it was built from). Built once after loading, so
/// drawing the panel never filters or sorts.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    children: HashMap<Option<i64>, Vec<usize>>,
}

impl Tree {
    pub fn build(list: &[Collection]) -> Tree {
        let mut children: HashMap<Option<i64>, Vec<usize>> = HashMap::new();
        for (i, c) in list.iter().enumerate() {
            if c.kind != QUICK {
                children.entry(c.parent).or_default().push(i);
            }
        }
        for v in children.values_mut() {
            v.sort_by(|&a, &b| compare(&list[a], &list[b]));
        }
        Tree { children }
    }

    /// Members of a level in display order (`None`: top level).
    pub fn children(&self, parent: Option<i64>) -> &[usize] {
        self.children.get(&parent).map_or(&[], |v| v.as_slice())
    }
}

/// Does the level have an own order (at least one valid position)? Only then "Sort by name" changes something.
pub fn is_ordered(list: &[Collection], parent: Option<i64>) -> bool {
    list.iter().any(|c| c.parent == parent && c.kind != QUICK && c.position.is_some())
}

/// Why a collection cannot go to a level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// the collection or the target is gone
    Missing,
    /// only sets take collections
    NotASet,
    /// a set never goes into itself or below itself
    IntoItself,
    /// albums of Apple Photos stay in the set "Apple Fotos" (the sync would create them there again)
    AppleAlbum,
    /// the set "Apple Fotos" mirrors Apple Photos: nothing else goes in
    AppleSet,
}

impl Blocked {
    /// Message for the user (German key, translated where it is shown).
    pub fn message(self) -> &'static str {
        match self {
            Blocked::Missing => "Die Sammlung gibt es nicht mehr.",
            Blocked::NotASet => "Ziel ist kein Sammlungssatz.",
            Blocked::IntoItself => "Ein Satz kann nicht in sich selbst liegen.",
            Blocked::AppleAlbum => "Alben von Apple Fotos bleiben im Satz „Apple Fotos“.",
            Blocked::AppleSet => "In den Satz „Apple Fotos“ kommen nur Alben von Apple Fotos.",
        }
    }
}

/// Ids of a collection and everything below it (on the loaded list).
pub fn descendants(list: &[Collection], id: i64) -> HashSet<i64> {
    let mut out = HashSet::from([id]);
    let mut grew = true;
    while grew {
        grew = false;
        for c in list {
            if c.parent.is_some_and(|p| out.contains(&p)) && out.insert(c.id) {
                grew = true;
            }
        }
    }
    out
}

/// Can `id` go to the level `parent` (`None`: top level)? Checked on the loaded list for the interface (blocked drop
/// targets, menu entries); [`place`] checks the same again in the library.
pub fn check(list: &[Collection], id: i64, parent: Option<i64>) -> Result<(), Blocked> {
    let Some(c) = list.iter().find(|c| c.id == id) else { return Err(Blocked::Missing) };
    if c.parent == parent {
        return Ok(());
    }
    if c.apple && c.kind != SET {
        return Err(Blocked::AppleAlbum);
    }
    if let Some(p) = parent {
        let Some(t) = list.iter().find(|c| c.id == p) else { return Err(Blocked::Missing) };
        if t.kind != SET {
            return Err(Blocked::NotASet);
        }
        if t.apple {
            return Err(Blocked::AppleSet);
        }
        if descendants(list, id).contains(&p) {
            return Err(Blocked::IntoItself);
        }
    }
    Ok(())
}

/// The same check in the library (the second line behind the interface).
fn check_db(conn: &Connection, id: i64, parent: Option<i64>) -> R<()> {
    let marker = crate::apple_photos::COLLECTION_MARKER;
    let row = |id: i64| -> R<Option<(Option<i64>, u8, bool)>> {
        conn.query_row("SELECT parent, kind, coalesce(rules, '') = ?2 FROM collection WHERE id = ?1", params![id, marker], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u8, r.get(2)?)))
            .optional()
            .map_err(f)
    };
    let Some((old, kind, apple)) = row(id)? else { return Err(Blocked::Missing.message().into()) };
    if old == parent {
        return Ok(());
    }
    if apple && kind != SET {
        return Err(Blocked::AppleAlbum.message().into());
    }
    if let Some(p) = parent {
        let Some((_, kind, apple)) = row(p)? else { return Err(Blocked::Missing.message().into()) };
        if kind != SET {
            return Err(Blocked::NotASet.message().into());
        }
        if apple {
            return Err(Blocked::AppleSet.message().into());
        }
        let below: i64 = conn
            .query_row(
                "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM collection s JOIN t ON s.parent = t.id) SELECT count(*) FROM t WHERE id = ?2",
                params![id, p],
                |r| r.get(0),
            )
            .map_err(f)?;
        if below > 0 {
            return Err(Blocked::IntoItself.message().into());
        }
    }
    Ok(())
}

struct Entry {
    id: i64,
    position: Option<i64>,
}

/// Members of a level in display order (without the quick collection).
fn level(conn: &Connection, parent: Option<i64>) -> R<Vec<Entry>> {
    let mut st = conn
        .prepare(
            "SELECT s.id, s.kind, s.name, o.position FROM collection s
             LEFT JOIN collection_order o ON o.collection = s.id AND o.parent IS s.parent
             WHERE s.parent IS ?1 AND s.kind <> 3",
        )
        .map_err(f)?;
    let mut v: Vec<(Entry, (bool, i64, bool, String, i64))> = st
        .query_map([parent], |r| {
            let (id, kind, name, position): (i64, i64, String, Option<i64>) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            Ok((Entry { id, position }, display_key(position, kind as u8, &name, id)))
        })
        .map_err(f)?
        .collect::<rusqlite::Result<_>>()
        .map_err(f)?;
    v.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(v.into_iter().map(|(e, _)| e).collect())
}

const COLUMNS: [&str; 3] = ["collection", "parent", "position"];

/// Rows of `collection_order` of these collections (as they are, valid or not).
fn rows_of(conn: &Connection, ids: &[i64]) -> R<Vec<Vec<Value>>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut st = conn.prepare(&format!("SELECT collection, parent, position FROM collection_order WHERE collection IN ({marks})")).map_err(f)?;
        let mut rows = st.query(params_from_iter(chunk.iter())).map_err(f)?;
        while let Some(r) = rows.next().map_err(f)? {
            out.push((0..3).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>().map_err(f)?);
        }
    }
    Ok(out)
}

fn delete_rows(conn: &Connection, ids: &[i64]) -> R<()> {
    for chunk in ids.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        conn.execute(&format!("DELETE FROM collection_order WHERE collection IN ({marks})"), params_from_iter(chunk.iter())).map_err(f)?;
    }
    Ok(())
}

/// Snapshot of an order change: the old rows come back with ⌘Z, the new ones (by collection) go.
fn snapshot(old: Vec<Vec<Value>>, new: &[i64], parent_change: Option<(i64, Option<i64>)>) -> Snapshot {
    let mut s = Snapshot::default();
    s.tables.push(TableRows { table: "collection_order".into(), columns: COLUMNS.iter().map(|c| c.to_string()).collect(), rows: old });
    s.added.push(TableRows { table: "collection_order".into(), columns: vec!["collection".into()], rows: new.iter().map(|&c| vec![Value::Integer(c)]).collect() });
    if let Some((id, old_parent)) = parent_change {
        s.changed.push(("collection".into(), id, "parent".into(), old_parent.map_or(Value::Null, Value::Integer)));
    }
    s
}

/// Put collection `id` into the level `parent` (`None`: top level) before the member `before` (`None`: at the end).
/// The level gets its order written as shown (all members numbered 1 … n); a level the collection leaves is renumbered
/// if it had an order. Returns an empty snapshot if nothing changes (no ⌘Z step then). Sets only, never into itself,
/// albums of Apple Photos never out of their set and nothing else into it.
pub fn place(db: &Db, id: i64, parent: Option<i64>, before: Option<i64>) -> R<Snapshot> {
    if before == Some(id) {
        return Ok(Snapshot::default());
    }
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let old_parent: Option<i64> = tx.query_row("SELECT parent FROM collection WHERE id = ?1", [id], |r| r.get(0)).optional().map_err(f)?.ok_or_else(|| Blocked::Missing.message().to_string())?;
    check_db(&tx, id, parent)?;
    let target = level(&tx, parent)?;
    let shown: Vec<i64> = target.iter().map(|e| e.id).collect();
    let mut order: Vec<i64> = shown.iter().copied().filter(|&x| x != id).collect();
    let at = before.and_then(|b| order.iter().position(|&x| x == b)).unwrap_or(order.len());
    order.insert(at, id);
    let moved = old_parent != parent;
    if !moved && order == shown {
        return Ok(Snapshot::default());
    }
    // the level left behind: renumbered only if it had an order (closes the gap; an unordered one stays unordered)
    let left: Vec<i64> = if moved {
        let l = level(&tx, old_parent)?;
        if l.iter().any(|e| e.position.is_some()) { l.into_iter().map(|e| e.id).filter(|&x| x != id).collect() } else { Vec::new() }
    } else {
        Vec::new()
    };
    let touched: Vec<i64> = order.iter().chain(left.iter()).copied().collect();
    let old = rows_of(&tx, &touched)?;
    delete_rows(&tx, &touched)?;
    if moved {
        tx.execute("UPDATE collection SET parent = ?1 WHERE id = ?2", params![parent, id]).map_err(f)?;
    }
    {
        let mut ins = tx.prepare("INSERT INTO collection_order(collection, parent, position) VALUES(?1, ?2, ?3)").map_err(f)?;
        for (i, c) in order.iter().enumerate() {
            ins.execute(params![c, parent, i as i64 + 1]).map_err(f)?;
        }
        for (i, c) in left.iter().enumerate() {
            ins.execute(params![c, old_parent, i as i64 + 1]).map_err(f)?;
        }
    }
    tx.commit().map_err(f)?;
    Ok(snapshot(old, &touched, moved.then_some((id, old_parent))))
}

/// [`place`] on the loaded list (the interface shows the new order at once, before the library has written it; the
/// list is loaded again afterwards). Same rules for numbering; the checks are the caller's ([`check`]). Returns whether
/// anything changes.
pub fn place_in(list: &mut [Collection], id: i64, parent: Option<i64>, before: Option<i64>) -> bool {
    if before == Some(id) {
        return false;
    }
    let Some(ix) = list.iter().position(|c| c.id == id) else { return false };
    let old_parent = list[ix].parent;
    let tree = Tree::build(list);
    let shown: Vec<i64> = tree.children(parent).iter().map(|&i| list[i].id).collect();
    let mut order: Vec<i64> = shown.iter().copied().filter(|&x| x != id).collect();
    let at = before.and_then(|b| order.iter().position(|&x| x == b)).unwrap_or(order.len());
    order.insert(at, id);
    let moved = old_parent != parent;
    if !moved && order == shown {
        return false;
    }
    let left: Vec<i64> = if moved && is_ordered(list, old_parent) {
        tree.children(old_parent).iter().map(|&i| list[i].id).filter(|&x| x != id).collect()
    } else {
        Vec::new()
    };
    list[ix].parent = parent;
    for (n, c) in order.iter().chain(left.iter()).enumerate() {
        let n = if n < order.len() { n } else { n - order.len() };
        if let Some(e) = list.iter_mut().find(|e| e.id == *c) {
            e.position = Some(n as i64 + 1);
        }
    }
    true
}

/// [`sort_by_name`] on the loaded list. Returns whether anything changes.
pub fn sort_by_name_in(list: &mut [Collection], parent: Option<i64>) -> bool {
    let mut any = false;
    for c in list.iter_mut().filter(|c| c.parent == parent && c.kind != QUICK) {
        any |= c.position.take().is_some();
    }
    any
}

/// "Sort by name" for a level: its positions go, the level shows the old order again (sets first, then by name).
/// ⌘Z brings the own order back. Empty snapshot if the level had none.
pub fn sort_by_name(db: &Db, parent: Option<i64>) -> R<Snapshot> {
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let members: Vec<i64> = level(&tx, parent)?.into_iter().map(|e| e.id).collect();
    let old = rows_of(&tx, &members)?;
    if old.is_empty() {
        return Ok(Snapshot::default());
    }
    delete_rows(&tx, &members)?;
    tx.commit().map_err(f)?;
    Ok(snapshot(old, &[], None))
}

/// The collection set an import puts its new collections into (import dialog "Sammlungssatz", Lightroom import
/// "into a set"): the set remembered for this name (`info` key `collection_set:<name>`) wherever it lies now and
/// however it is called now, otherwise a set of this name (top level first), otherwise a new one at the top level.
/// So moving or renaming the set never makes the next import create a second one.
pub fn find_or_create_set(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    let name = name.trim();
    let key = format!("collection_set:{}", name.to_lowercase());
    let remembered: Option<i64> = conn.query_row("SELECT value FROM info WHERE name = ?1", [&key], |r| r.get::<_, Option<String>>(0)).optional()?.flatten().and_then(|v| v.parse().ok());
    if let Some(id) = remembered {
        if conn.query_row("SELECT 1 FROM collection WHERE id = ?1 AND kind = 1", [id], |_| Ok(())).optional()?.is_some() {
            return Ok(id);
        }
    }
    let found: Option<i64> = conn
        .query_row("SELECT id FROM collection WHERE kind = 1 AND lower(name) = lower(?1) ORDER BY parent IS NOT NULL, id LIMIT 1", [name], |r| r.get(0))
        .optional()?;
    let id = match found {
        Some(id) => id,
        None => {
            conn.execute("INSERT INTO collection(name, kind) VALUES(?1, 1)", [name])?;
            conn.last_insert_rowid()
        }
    };
    conn.execute("INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value", params![key, id.to_string()])?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manage::undo;

    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Top level: set "Reisen" (10) with "Rom" (11) and "Paris" (12), set "Apple Fotos" (20) with albums "Urlaub" (21)
    /// and "Favoriten" (22), collections "Zebra" (30), "alpha" (31), smart "Mitte" (32), quick collection (40).
    fn library() -> (Dir, Db) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("graineo-order-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, '');
                 INSERT INTO file(id, folder, name, format) VALUES(1, 1, 'a.jpg', 'JPG'), (2, 1, 'b.jpg', 'JPG');
                 INSERT INTO image(id, file, stars) VALUES(1, 1, 0), (2, 2, 0);
                 INSERT INTO collection(id, name, kind) VALUES(10, 'Reisen', 1), (30, 'Zebra', 0), (31, 'alpha', 0), (32, 'Mitte', 2), (40, 'Schnellsammlung', 3);
                 INSERT INTO collection(id, name, kind, parent) VALUES(11, 'Rom', 0, 10), (12, 'Paris', 0, 10);
                 INSERT INTO collection(id, name, kind, rules) VALUES(20, 'Apple Fotos', 1, 'apple-photos');
                 INSERT INTO collection(id, name, kind, parent, rules) VALUES(21, 'Urlaub', 0, 20, 'apple-photos'), (22, 'Favoriten', 0, 20, 'apple-photos');
                 INSERT INTO collection_image(collection, image, position) VALUES(30, 1, 1), (30, 2, 2), (11, 2, 1), (21, 1, 1), (40, 2, 1);",
            )
            .unwrap();
        (Dir(d), db)
    }

    fn names(db: &Db, parent: Option<i64>) -> Vec<String> {
        let list = db.collections().unwrap();
        let tree = Tree::build(&list);
        tree.children(parent).iter().map(|&i| list[i].name.clone()).collect()
    }

    fn table(db: &Db, sql: &str) -> Vec<Vec<String>> {
        let mut st = db.conn.prepare(sql).unwrap();
        let n = st.column_count();
        let rows = st.query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i).map(|v| format!("{v:?}"))).collect::<rusqlite::Result<Vec<_>>>()).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn content(db: &Db) -> Vec<Vec<String>> {
        table(db, "SELECT collection, image, position FROM collection_image ORDER BY 1, 2")
    }

    fn state(db: &Db) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
        (table(db, "SELECT collection, parent, position FROM collection_order ORDER BY 1"), table(db, "SELECT id, parent FROM collection ORDER BY 1"))
    }

    #[test]
    fn a_level_never_ordered_looks_as_before() {
        let (_d, db) = library();
        // sets first, then by name (case-insensitive); the quick collection is never part of the list
        assert_eq!(names(&db, None), ["Apple Fotos", "Reisen", "alpha", "Mitte", "Zebra"]);
        assert_eq!(names(&db, Some(10)), ["Paris", "Rom"]);
        assert!(!is_ordered(&db.collections().unwrap(), None));
    }

    #[test]
    fn placing_within_a_level_writes_the_shown_order_and_never_the_content() {
        let (_d, db) = library();
        let before = content(&db);
        let start = state(&db);
        // "Zebra" to the start: the whole level is numbered as shown, Zebra first
        let s = place(&db, 30, None, Some(20)).unwrap();
        assert_eq!(names(&db, None), ["Zebra", "Apple Fotos", "Reisen", "alpha", "Mitte"]);
        assert_eq!(table(&db, "SELECT count(*) FROM collection_order WHERE parent IS NULL")[0][0], "Integer(5)");
        // middle and end
        let s2 = place(&db, 20, None, Some(32)).unwrap();
        assert_eq!(names(&db, None), ["Zebra", "Reisen", "alpha", "Apple Fotos", "Mitte"]);
        let s3 = place(&db, 30, None, None).unwrap();
        assert_eq!(names(&db, None), ["Reisen", "alpha", "Apple Fotos", "Mitte", "Zebra"]);
        assert!(is_ordered(&db.collections().unwrap(), None));
        assert_eq!(content(&db), before, "ordering never changes which photos a collection holds");
        // ⌘Z step by step back to the start
        undo(&db, &s3).unwrap();
        assert_eq!(names(&db, None), ["Zebra", "Reisen", "alpha", "Apple Fotos", "Mitte"]);
        undo(&db, &s2).unwrap();
        undo(&db, &s).unwrap();
        assert_eq!(state(&db), start, "⌘Z restores the table exactly");
        assert_eq!(names(&db, None), ["Apple Fotos", "Reisen", "alpha", "Mitte", "Zebra"]);
        assert_eq!(content(&db), before);
    }

    #[test]
    fn nothing_changes_nothing_is_written() {
        let (_d, db) = library();
        assert!(place(&db, 20, None, Some(10)).unwrap().is_empty(), "already before Reisen");
        assert!(place(&db, 30, None, Some(30)).unwrap().is_empty(), "before itself");
        assert!(place(&db, 30, None, None).unwrap().is_empty(), "already last");
        assert!(sort_by_name(&db, None).unwrap().is_empty(), "no order to drop");
        assert_eq!(table(&db, "SELECT count(*) FROM collection_order")[0][0], "Integer(0)");
    }

    #[test]
    fn into_a_set_and_out_again_with_undo() {
        let (_d, db) = library();
        let before = content(&db);
        // the top level is ordered, the set is not
        place(&db, 31, None, Some(20)).unwrap();
        let ordered = state(&db);
        // "Zebra" into "Reisen" before "Rom": the set gets its order, the top level closes the gap
        let s = place(&db, 30, Some(10), Some(11)).unwrap();
        assert_eq!(names(&db, Some(10)), ["Paris", "Zebra", "Rom"]);
        assert_eq!(names(&db, None), ["alpha", "Apple Fotos", "Reisen", "Mitte"]);
        assert_eq!(table(&db, "SELECT position FROM collection_order WHERE collection = 32")[0][0], "Integer(4)", "gap closed");
        assert_eq!(content(&db), before);
        undo(&db, &s).unwrap();
        assert_eq!(state(&db), ordered, "⌘Z: positions and parent exactly as before");
        // out of an unordered set: the set stays unordered, the newcomer goes to the end of the top level
        let s = place(&db, 12, None, None).unwrap();
        assert_eq!(names(&db, None), ["alpha", "Apple Fotos", "Reisen", "Mitte", "Zebra", "Paris"]);
        assert_eq!(table(&db, "SELECT count(*) FROM collection_order WHERE parent = 10")[0][0], "Integer(0)");
        undo(&db, &s).unwrap();
        assert_eq!(state(&db), ordered);
    }

    #[test]
    fn the_order_shown_at_once_equals_the_order_written() {
        let (_d, db) = library();
        let mut list = db.collections().unwrap();
        let steps = [(30, None, Some(20)), (11, None, Some(31)), (31, Some(10), None), (12, Some(10), Some(31)), (10, None, None), (32, Some(10), Some(11)), (30, None, Some(30)), (22, Some(20), Some(21))];
        for (id, parent, before) in steps {
            let local = place_in(&mut list, id, parent, before);
            let snap = place(&db, id, parent, before).unwrap();
            assert_eq!(local, !snap.is_empty(), "step {id}");
            let loaded = db.collections().unwrap();
            for p in [None, Some(10), Some(20)] {
                let shown: Vec<i64> = Tree::build(&list).children(p).iter().map(|&i| list[i].id).collect();
                let written: Vec<i64> = Tree::build(&loaded).children(p).iter().map(|&i| loaded[i].id).collect();
                assert_eq!(shown, written, "step {id}, level {p:?}");
            }
        }
        assert!(sort_by_name_in(&mut list, Some(10)));
        sort_by_name(&db, Some(10)).unwrap();
        let loaded = db.collections().unwrap();
        let ids = |l: &[Collection]| -> Vec<i64> { Tree::build(l).children(Some(10)).iter().map(|&i| l[i].id).collect() };
        assert_eq!(ids(&list), ids(&loaded));
    }

    #[test]
    fn blocked_targets() {
        let (_d, db) = library();
        let list = db.collections().unwrap();
        // a set never into itself or below itself, only sets take collections
        db.conn.execute_batch("INSERT INTO collection(id, name, kind, parent) VALUES(13, 'Italien', 1, 10);").unwrap();
        let list2 = db.collections().unwrap();
        assert_eq!(check(&list2, 10, Some(13)), Err(Blocked::IntoItself));
        assert_eq!(check(&list2, 10, Some(10)), Err(Blocked::IntoItself));
        assert!(place(&db, 10, Some(13), None).is_err());
        assert_eq!(check(&list, 31, Some(30)), Err(Blocked::NotASet));
        assert!(place(&db, 31, Some(30), None).is_err());
        // Apple Photos: albums stay in their set, nothing else goes in; inside it ordering is free
        assert_eq!(check(&list, 21, None), Err(Blocked::AppleAlbum));
        assert!(place(&db, 21, None, None).is_err());
        assert!(place(&db, 21, Some(10), None).is_err());
        assert_eq!(check(&list, 31, Some(20)), Err(Blocked::AppleSet));
        assert!(place(&db, 31, Some(20), None).is_err());
        assert_eq!(check(&list, 21, Some(20)), Ok(()));
        place(&db, 22, Some(20), None).unwrap();
        assert_eq!(names(&db, Some(20)), ["Urlaub", "Favoriten"]);
        // the set itself may move
        assert_eq!(check(&list, 20, Some(10)), Ok(()));
        // a refusal changes nothing
        assert_eq!(table(&db, "SELECT parent FROM collection WHERE id = 21")[0][0], "Integer(20)");
    }

    #[test]
    fn sort_by_name_and_undo() {
        let (_d, db) = library();
        place(&db, 30, None, Some(20)).unwrap();
        let ordered = state(&db);
        let s = sort_by_name(&db, None).unwrap();
        assert_eq!(names(&db, None), ["Apple Fotos", "Reisen", "alpha", "Mitte", "Zebra"]);
        assert!(!is_ordered(&db.collections().unwrap(), None));
        undo(&db, &s).unwrap();
        assert_eq!(state(&db), ordered);
        assert_eq!(names(&db, None)[0], "Zebra");
    }

    #[test]
    fn deleting_a_set_takes_its_order_and_undo_brings_it_back() {
        let (_d, db) = library();
        place(&db, 11, Some(10), Some(12)).unwrap();
        place(&db, 10, None, None).unwrap();
        let ordered = state(&db);
        let snap = crate::manage::delete_collection(&db, 10).unwrap();
        assert_eq!(table(&db, "SELECT count(*) FROM collection_order WHERE collection IN (10, 11, 12)")[0][0], "Integer(0)");
        undo(&db, &snap).unwrap();
        assert_eq!(state(&db), ordered);
        assert_eq!(names(&db, Some(10)), ["Rom", "Paris"]);
    }

    #[test]
    fn a_parent_changed_elsewhere_makes_the_position_invalid() {
        let (_d, db) = library();
        place(&db, 11, Some(10), None).unwrap();
        place(&db, 31, None, Some(20)).unwrap();
        // an older version moves "Paris" to the top level without knowing the table: its old position (set 10) no longer
        // counts, it is not placed and shows last
        db.conn.execute("UPDATE collection SET parent = NULL WHERE id = 12", []).unwrap();
        assert_eq!(names(&db, None).last().map(String::as_str), Some("Paris"));
        // a new collection in an ordered level comes last too
        db.create_collection_in("Aaa neu", None).unwrap();
        let n = names(&db, None);
        assert_eq!(&n[n.len() - 2..], ["Aaa neu", "Paris"], "not placed: behind the placed ones, among themselves as before");
        // the next step in that level places everyone as shown
        place(&db, 30, None, None).unwrap();
        assert_eq!(names(&db, None).last().map(String::as_str), Some("Zebra"));
        assert_eq!(table(&db, "SELECT position FROM collection_order WHERE collection = 12")[0][0], "Integer(6)");
    }

    #[test]
    fn the_import_set_is_found_after_moving_or_renaming() {
        let (_d, db) = library();
        let a = find_or_create_set(&db.conn, "Importe").unwrap();
        assert_eq!(find_or_create_set(&db.conn, " importe ").unwrap(), a);
        // moved into another set and renamed: still the same set, no second "Importe"
        place(&db, a, Some(10), None).unwrap();
        crate::manage::rename_collection(&db, a, "Alte Importe").unwrap();
        assert_eq!(find_or_create_set(&db.conn, "Importe").unwrap(), a);
        assert_eq!(table(&db, "SELECT count(*) FROM collection WHERE kind = 1 AND name LIKE '%Importe%'")[0][0], "Integer(1)");
        // deleted: a new one at the top level
        crate::manage::delete_collection(&db, a).unwrap();
        let b = find_or_create_set(&db.conn, "Importe").unwrap();
        assert_eq!(table(&db, &format!("SELECT name FROM collection WHERE id = {b}"))[0][0], "Text(\"Importe\")");
        assert_eq!(table(&db, &format!("SELECT parent FROM collection WHERE id = {b}"))[0][0], "Null");
        // a library from before (no remembered id) finds an existing set by name, also inside another set
        db.conn.execute("DELETE FROM info WHERE name LIKE 'collection_set:%'", []).unwrap();
        db.conn.execute("UPDATE collection SET parent = 10 WHERE id = ?1", [b]).unwrap();
        assert_eq!(find_or_create_set(&db.conn, "Importe").unwrap(), b);
    }

    #[test]
    fn opening_a_library_without_the_table_adds_it_without_a_version_step() {
        let (d, db) = library();
        db.conn.execute_batch("DROP TABLE collection_order;").unwrap();
        drop(db);
        let db = Db::open(&d.0.join("library.sqlite")).unwrap();
        let v: i32 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, crate::db::VERSION);
        assert_eq!(table(&db, "SELECT count(*) FROM sqlite_master WHERE name = 'collection_order'")[0][0], "Integer(1)");
        assert_eq!(names(&db, None)[0], "Apple Fotos");
    }
}
