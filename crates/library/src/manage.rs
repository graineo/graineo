//! Managing the library: remove folders, imports and photos from the library (the files stay untouched),
//! rename, delete and move collections, collection sets, smart collections, keywords and people.
//!
//! Every removal first takes a [`Snapshot`] of all rows it takes along (it follows the foreign keys of the
//! database, so new tables are covered without changes here). ⌘Z puts exactly these rows back ([`undo`]).
//! Folders and photos removed from the library are remembered in `excluded_path`, so the automatic sync does
//! not take them in again; importing or adding them explicitly lifts that ([`lift_exclusions`]).

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::db::Db;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}


// ───────────────────────────────────────────── Snapshot and undo

#[derive(Clone, Debug)]
pub(crate) struct TableRows {
    pub(crate) table: String,
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Value>>,
}

/// A reference that is not deleted with its parent but set to NULL (or would block the delete): its old value.
#[derive(Clone, Debug)]
struct Nulled {
    table: String,
    key_column: String,
    key: Value,
    column: String,
    old: Value,
}

/// All rows a removal takes along, in the order they have to be put back (parents first).
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub(crate) tables: Vec<TableRows>,
    nulled: Vec<Nulled>,
    /// exclusions this removal added (⌘Z lifts them again)
    excluded: Vec<(i64, String)>,
    /// Lightroom files remembered as removed (⌘Z forgets them again)
    lr_removed: Vec<i64>,
    /// renames/moves: (table, id, column, old value)
    pub(crate) changed: Vec<(String, i64, String, Value)>,
    /// rows the change added (merging keywords, own order of collections): ⌘Z deletes them again
    pub(crate) added: Vec<TableRows>,
    /// follow-up steps that failed after the change was committed (keyword times, sidecars, search index, smart
    /// rules): the change itself stands and ⌘Z works, the interface shows them as a warning
    pub(crate) warnings: Vec<String>,
    /// smart collections whose rule followed the change (renaming, merging keywords)
    pub(crate) smart_changed: usize,
    /// smart collections left unchanged because a term naming the keyword also finds other keywords
    pub(crate) smart_kept: Vec<String>,
}

impl Snapshot {
    /// Number of rows of a table in the snapshot (tests, messages).
    pub fn rows_of(&self, table: &str) -> usize {
        self.tables.iter().filter(|t| t.table == table).map(|t| t.rows.len()).sum()
    }
    /// Rows the snapshot holds (for the limit of the ⌘Z stack).
    pub fn size(&self) -> usize {
        self.tables.iter().chain(self.added.iter()).map(|t| t.rows.len()).sum::<usize>() + self.nulled.len() + self.changed.len() + self.excluded.len()
    }
    /// Keywords the change created (⌘Z deletes them again).
    pub(crate) fn add_created_keywords(&mut self, ids: &[i64]) {
        if !ids.is_empty() {
            self.added.push(TableRows { table: "keyword".into(), columns: vec!["id".into()], rows: ids.iter().map(|i| vec![Value::Integer(*i)]).collect() });
        }
    }
    /// Rows the change added (⌘Z deletes them) and rows it removed (⌘Z puts them back), any table.
    pub(crate) fn add_rows(&mut self, table: &str, columns: &[&str], added: Vec<Vec<Value>>, removed: Vec<Vec<Value>>) {
        let columns: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
        if !added.is_empty() {
            self.added.push(TableRows { table: table.into(), columns: columns.clone(), rows: added });
        }
        if !removed.is_empty() {
            self.tables.push(TableRows { table: table.into(), columns, rows: removed });
        }
    }
    /// Old values of changed rows (renames, moves).
    pub(crate) fn add_changed(&mut self, table: &str, id: i64, column: &str, old: Value) {
        self.changed.push((table.to_string(), id, column.to_string(), old));
    }
    /// Follow-up steps that failed after the change was committed (the change stands; shown as a warning).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    /// Smart collections whose rule followed the change.
    pub fn smart_changed(&self) -> usize {
        self.smart_changed
    }
    /// Names of smart collections left unchanged (a term naming the keyword was ambiguous).
    pub fn smart_kept(&self) -> &[String] {
        &self.smart_kept
    }
    /// Note a failed follow-up step (after the commit): kept on the snapshot and logged, never an error.
    pub(crate) fn warn<T>(&mut self, step: &str, r: R<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                // once per step (a failing search index would otherwise give one line per photo)
                let head = format!("{step}: ");
                if !self.warnings.iter().any(|w| w.starts_with(&head)) {
                    eprintln!("keyword change: {step} failed after commit: {e}");
                    self.warnings.push(format!("{head}{e}"));
                }
                None
            }
        }
    }
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.nulled.is_empty() && self.excluded.is_empty() && self.changed.is_empty() && self.added.is_empty()
    }
    /// Take over the rows of a later removal (several blocks of one run as one ⌘Z step).
    pub fn merge(&mut self, o: Snapshot) {
        self.tables.extend(o.tables);
        self.nulled.extend(o.nulled);
        self.excluded.extend(o.excluded);
        self.lr_removed.extend(o.lr_removed);
        self.changed.extend(o.changed);
        self.added.extend(o.added);
        self.warnings.extend(o.warnings);
        self.smart_changed += o.smart_changed;
        self.smart_kept.extend(o.smart_kept);
    }
}

struct Fk {
    child: String,
    from: String,
    to: Option<String>,
    on_delete: String,
}

fn foreign_keys(conn: &Connection) -> R<Vec<(String, Fk)>> {
    let mut st = conn
        .prepare(
            "SELECT p.\"table\", m.name, p.\"from\", p.\"to\", p.on_delete FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) p
             WHERE m.type = 'table' AND m.sql NOT LIKE 'CREATE VIRTUAL%'",
        )
        .map_err(f)?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, Fk { child: r.get(1)?, from: r.get(2)?, to: r.get(3)?, on_delete: r.get(4)? })))
        .map_err(f)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(f)
}

fn columns(conn: &Connection, table: &str) -> R<(Vec<String>, Option<String>)> {
    let mut st = conn.prepare("SELECT name, pk FROM pragma_table_info(?1) ORDER BY cid").map_err(f)?;
    let v: Vec<(String, i64)> = st.query_map([table], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
    let pk = {
        let keys: Vec<&(String, i64)> = v.iter().filter(|(_, k)| *k > 0).collect();
        if keys.len() == 1 { Some(keys[0].0.clone()) } else { None }
    };
    Ok((v.into_iter().map(|(n, _)| n).collect(), pk))
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

struct Collector<'a> {
    conn: &'a Connection,
    fks: Vec<(String, Fk)>,
    seen: HashMap<String, HashSet<String>>,
    snap: Snapshot,
}

impl Collector<'_> {
    /// Rows of `table` whose `column` is one of `values`, then everything that hangs on them.
    fn collect(&mut self, table: &str, column: &str, values: &[Value]) -> R<()> {
        if values.is_empty() {
            return Ok(());
        }
        let (cols, pk) = columns(self.conn, table)?;
        let list: Vec<String> = cols.iter().map(|c| quote(c)).collect();
        let mut new_rows: Vec<Vec<Value>> = Vec::new();
        for chunk in values.chunks(400) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!("SELECT {} FROM {} WHERE {} IN ({marks})", list.join(","), quote(table), quote(column));
            let mut st = self.conn.prepare(&sql).map_err(f)?;
            let mut rows = st.query(params_from_iter(chunk.iter())).map_err(f)?;
            while let Some(r) = rows.next().map_err(f)? {
                let row: Vec<Value> = (0..cols.len()).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<_>>().map_err(f)?;
                let key = match pk.as_ref().and_then(|p| cols.iter().position(|c| c == p)) {
                    Some(i) => format!("{:?}", row[i]),
                    None => format!("{row:?}"),
                };
                if self.seen.entry(table.to_string()).or_default().insert(key) {
                    new_rows.push(row);
                }
            }
        }
        if new_rows.is_empty() {
            return Ok(());
        }
        self.snap.tables.push(TableRows { table: table.to_string(), columns: cols.clone(), rows: new_rows.clone() });
        let children: Vec<(String, String, Option<String>, String)> =
            self.fks.iter().filter(|(p, _)| p == table).map(|(_, k)| (k.child.clone(), k.from.clone(), k.to.clone(), k.on_delete.clone())).collect();
        for (child, from, to, on_delete) in children {
            let to = match to.or_else(|| pk.clone()) {
                Some(t) => t,
                None => continue,
            };
            let Some(i) = cols.iter().position(|c| *c == to) else { continue };
            let parent_values: Vec<Value> = new_rows.iter().map(|r| r[i].clone()).filter(|v| *v != Value::Null).collect();
            if on_delete.eq_ignore_ascii_case("CASCADE") {
                self.collect(&child, &from, &parent_values)?;
            } else {
                // SET NULL, NO ACTION, RESTRICT: the row stays, only the reference goes (and comes back with ⌘Z)
                let (_, child_pk) = columns(self.conn, &child)?;
                let Some(child_pk) = child_pk else { continue };
                for chunk in parent_values.chunks(400) {
                    let marks = vec!["?"; chunk.len()].join(",");
                    let sql = format!("SELECT {}, {} FROM {} WHERE {} IN ({marks})", quote(&child_pk), quote(&from), quote(&child), quote(&from));
                    let mut st = self.conn.prepare(&sql).map_err(f)?;
                    let mut rows = st.query(params_from_iter(chunk.iter())).map_err(f)?;
                    while let Some(r) = rows.next().map_err(f)? {
                        self.snap.nulled.push(Nulled { table: child.clone(), key_column: child_pk.clone(), key: r.get(0).map_err(f)?, column: from.clone(), old: r.get(1).map_err(f)? });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Take the snapshot of `table` rows with `column` in `values` and delete them (in one transaction).
pub(crate) fn remove_rows(conn: &Connection, table: &str, column: &str, values: &[Value], snap: Snapshot) -> R<Snapshot> {
    let tx = conn.unchecked_transaction().map_err(f)?;
    let snap = remove_rows_in(&tx, table, column, values, snap)?;
    tx.commit().map_err(f)?;
    Ok(snap)
}

/// [`remove_rows`] inside a transaction the caller holds (and commits).
pub(crate) fn remove_rows_in(tx: &Connection, table: &str, column: &str, values: &[Value], mut snap: Snapshot) -> R<Snapshot> {
    let mut c = Collector { conn: tx, fks: foreign_keys(tx)?, seen: HashMap::new(), snap: Snapshot::default() };
    c.collect(table, column, values)?;
    let taken = c.snap;
    for n in &taken.nulled {
        tx.execute(&format!("UPDATE {} SET {} = NULL WHERE {} = ?1", quote(&n.table), quote(&n.column), quote(&n.key_column)), [&n.key]).map_err(f)?;
    }
    for chunk in values.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        tx.execute(&format!("DELETE FROM {} WHERE {} IN ({marks})", quote(table), quote(column)), params_from_iter(chunk.iter())).map_err(f)?;
    }
    for (root, rel) in &snap.excluded {
        tx.execute("INSERT OR REPLACE INTO excluded_path(root, rel, time) VALUES(?1, ?2, ?3)", params![root, rel, crate::now()]).map_err(f)?;
    }
    for id in &snap.lr_removed {
        tx.execute("INSERT OR IGNORE INTO removed_lr_file(lr_id, path, time) VALUES(?1, NULL, ?2)", params![id, crate::now()]).map_err(f)?;
    }
    snap.tables = taken.tables;
    snap.nulled = taken.nulled;
    Ok(snap)
}

/// Tables whose rows a change replaces under the same primary key (see [`undo`]).
const REPLACED_BY_KEY: [&str; 2] = ["collection_order", "keyword_option"];

/// Delete exactly the rows a change added to a table.
fn delete_added(tx: &rusqlite::Transaction, t: &TableRows) -> R<()> {
    let cond: Vec<String> = t.columns.iter().enumerate().map(|(i, c)| format!("{} = ?{}", quote(c), i + 1)).collect();
    let sql = format!("DELETE FROM {} WHERE {}", quote(&t.table), cond.join(" AND "));
    let mut st = tx.prepare(&sql).map_err(f)?;
    for r in t.rows.iter().rev() {
        st.execute(params_from_iter(r.iter())).map_err(f)?;
    }
    Ok(())
}

/// Put back what a removal took (⌘Z); renames and moves get their old values. Rows that exist again in the
/// meantime stay as they are. Order: removed rows back, references and old values back, then the rows the change
/// added go (a new parent term is deleted only after its children hang below their old parents again).
pub fn undo(db: &Db, snap: &Snapshot) -> R<()> {
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    // Tables whose rows a change replaces under the same key (the order of collections): the new row has to go
    // before the old one is put back, otherwise "INSERT OR IGNORE" keeps the new row and the old one is lost.
    for t in snap.added.iter().rev().filter(|t| REPLACED_BY_KEY.contains(&t.table.as_str())) {
        delete_added(&tx, t)?;
    }
    for t in &snap.tables {
        let cols: Vec<String> = t.columns.iter().map(|c| quote(c)).collect();
        let marks = vec!["?"; cols.len()].join(",");
        let sql = format!("INSERT OR IGNORE INTO {}({}) VALUES({marks})", quote(&t.table), cols.join(","));
        let mut st = tx.prepare(&sql).map_err(f)?;
        for r in &t.rows {
            st.execute(params_from_iter(r.iter())).map_err(f)?;
        }
    }
    for n in &snap.nulled {
        tx.execute(&format!("UPDATE {} SET {} = ?1 WHERE {} = ?2", quote(&n.table), quote(&n.column), quote(&n.key_column)), params![n.old, n.key]).map_err(f)?;
    }
    for (table, id, column, old) in snap.changed.iter().rev() {
        tx.execute(&format!("UPDATE {} SET {} = ?1 WHERE id = ?2", quote(table), quote(column)), params![old, id]).map_err(f)?;
    }
    for t in snap.added.iter().rev().filter(|t| !REPLACED_BY_KEY.contains(&t.table.as_str())) {
        delete_added(&tx, t)?;
    }
    for (root, rel) in &snap.excluded {
        tx.execute("DELETE FROM excluded_path WHERE root = ?1 AND rel = ?2", params![root, rel]).map_err(f)?;
    }
    for id in &snap.lr_removed {
        tx.execute("DELETE FROM removed_lr_file WHERE lr_id = ?1", [id]).map_err(f)?;
    }
    tx.commit().map_err(f)
}

// ───────────────────────────────────────────── Exclusions (sync)

/// Excluded paths of a root (relative, folders with '/' at the end).
pub fn exclusions(conn: &Connection, root: i64) -> HashSet<String> {
    let Ok(mut st) = conn.prepare("SELECT rel FROM excluded_path WHERE root = ?1") else { return HashSet::new() };
    st.query_map([root], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// Is `rel` (folder 'a/b/' or file 'a/b/x.jpg') excluded or inside an excluded folder?
pub fn is_excluded(excluded: &HashSet<String>, rel: &str) -> bool {
    if excluded.is_empty() {
        return false;
    }
    if excluded.contains(rel) {
        return true;
    }
    let mut end = 0;
    while let Some(i) = rel[end..].find('/') {
        end += i + 1;
        if end < rel.len() && excluded.contains(&rel[..end]) {
            return true;
        }
    }
    false
}

/// Explicit import or adding of a folder: lift exclusions of it, below it and above it (the user wants it back).
pub fn lift_exclusions(conn: &Connection, root: i64, rel: &str) -> R<usize> {
    let mut n = conn.execute("DELETE FROM excluded_path WHERE root = ?1 AND (substr(rel, 1, length(?2)) = ?2)", params![root, rel]).map_err(f)?;
    // a parent folder excluded: lift it too, otherwise the sync would not look inside
    let mut end = 0;
    while let Some(i) = rel[end..].find('/') {
        end += i + 1;
        n += conn.execute("DELETE FROM excluded_path WHERE root = ?1 AND rel = ?2", params![root, &rel[..end]]).map_err(f)?;
    }
    Ok(n)
}

// ───────────────────────────────────────────── What a removal costs

/// What disappears with a removal from the library (the files stay).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loss {
    pub photos: usize,
    pub copies: usize,
    pub edited: usize,
    pub rated: usize,
    pub keywords: usize,
    pub collections: usize,
    pub faces: usize,
}

fn loss_for(conn: &Connection, files_sql: &str, p: &[&dyn rusqlite::ToSql]) -> R<Loss> {
    let q = |sql: String| -> R<usize> { conn.query_row(&sql, p, |r| r.get::<_, i64>(0)).map(|n| n as usize).map_err(f) };
    Ok(Loss {
        photos: q(format!("SELECT count(*) FROM ({files_sql})"))?,
        copies: q(format!("SELECT count(*) FROM image WHERE copy IS NOT NULL AND file IN ({files_sql})"))?,
        edited: q(format!("SELECT count(DISTINCT file) FROM develop_state WHERE settings IS NOT NULL AND file IN ({files_sql})"))?,
        rated: q(format!("SELECT count(*) FROM image WHERE (stars > 0 OR flag <> 0 OR label > 0) AND file IN ({files_sql})"))?,
        keywords: q(format!("SELECT count(*) FROM image_keyword WHERE image IN (SELECT id FROM image WHERE file IN ({files_sql}))"))?,
        collections: q(format!(
            "SELECT count(*) FROM collection_image sb JOIN collection s ON s.id = sb.collection WHERE s.kind <> 3 AND sb.image IN (SELECT id FROM image WHERE file IN ({files_sql}))"
        ))?,
        faces: q(format!("SELECT count(*) FROM face WHERE person IS NOT NULL AND image IN (SELECT id FROM image WHERE file IN ({files_sql}))"))?,
    })
}

const FOLDER_FILES: &str = "SELECT d.id FROM file d JOIN folder o ON o.id = d.folder WHERE o.root = ?1 AND substr(o.path, 1, length(?2)) = ?2";

/// Loss when removing a folder (`rel` '' = the whole root folder) with its subfolders.
pub fn folder_loss(db: &Db, root: i64, rel: &str) -> R<Loss> {
    loss_for(&db.conn, FOLDER_FILES, &[&root, &rel])
}

/// Remove a folder with its subfolders from the library (database only, never files). A subfolder is
/// remembered as excluded so the sync does not take it in again; a root folder simply goes (the sync only
/// looks at the root folders of the library).
pub fn remove_folder(db: &Db, root: i64, rel: &str) -> R<Snapshot> {
    let mut snap = Snapshot::default();
    lr_removed_of(&db.conn, &format!("SELECT d.lr_id FROM file d WHERE d.lr_id IS NOT NULL AND d.id IN ({FOLDER_FILES})"), &[&root, &rel], &mut snap)?;
    if rel.is_empty() {
        return remove_rows(&db.conn, "root_folder", "id", &[Value::Integer(root)], snap);
    }
    snap.excluded.push((root, rel.to_string()));
    let ids: Vec<Value> = {
        let mut st = db.conn.prepare("SELECT id FROM folder WHERE root = ?1 AND substr(path, 1, length(?2)) = ?2").map_err(f)?;
        let v = st.query_map(params![root, rel], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
        v.into_iter().map(Value::Integer).collect()
    };
    remove_rows(&db.conn, "folder", "id", &ids, snap)
}

/// Lightroom files among a removal: the Lightroom import must not bring them back.
fn lr_removed_of(conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql], snap: &mut Snapshot) -> R<()> {
    let mut st = conn.prepare(sql).map_err(f)?;
    let v = st.query_map(p, |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
    snap.lr_removed.extend(v);
    Ok(())
}

// ───────────────────────────────────────────── Photos

/// Remove photos from the library (files untouched). Originals go with their file (including virtual copies),
/// a virtual copy alone only as itself. The files are remembered as excluded (sync).
pub fn remove_photos(db: &Db, images: &[i64]) -> R<Snapshot> {
    let mut files: Vec<Value> = Vec::new();
    let mut copies: Vec<Value> = Vec::new();
    let mut snap = Snapshot::default();
    for &b in images {
        let row: Option<(i64, Option<String>)> = db.conn.query_row("SELECT file, copy FROM image WHERE id = ?1", [b], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(f)?;
        match row {
            Some((d, None)) => {
                if !files.contains(&Value::Integer(d)) {
                    files.push(Value::Integer(d));
                    let at: Option<(i64, String, Option<i64>)> = db
                        .conn
                        .query_row("SELECT o.root, o.path || d.name, d.lr_id FROM file d JOIN folder o ON o.id = d.folder WHERE d.id = ?1", [d], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .optional()
                        .map_err(f)?;
                    if let Some((root, rel, lr)) = at {
                        snap.excluded.push((root, rel));
                        snap.lr_removed.extend(lr);
                    }
                }
            }
            Some((_, Some(_))) => copies.push(Value::Integer(b)),
            None => {}
        }
    }
    let mut snap = remove_rows(&db.conn, "file", "id", &files, snap)?;
    if !copies.is_empty() {
        let more = remove_rows(&db.conn, "image", "id", &copies, Snapshot::default())?;
        snap.tables.extend(more.tables);
        snap.nulled.extend(more.nulled);
    }
    Ok(snap)
}

/// Photos whose files went into the Trash (cleaning up duplicates): like [`remove_photos`], but the files are not
/// remembered as excluded – put back from the Trash, the sync takes them in again. ⌘Z ([`undo`]) brings the entries
/// back with all their data (until the file is back, the photo shows as missing).
pub fn remove_trashed_photos(db: &Db, images: &[i64]) -> R<Snapshot> {
    let mut snap = remove_photos(db, images)?;
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    for (root, rel) in snap.excluded.drain(..) {
        tx.execute("DELETE FROM excluded_path WHERE root = ?1 AND rel = ?2", params![root, rel]).map_err(f)?;
    }
    tx.commit().map_err(f)?;
    Ok(snap)
}

/// Loss when removing these photos.
pub fn photos_loss(db: &Db, images: &[i64]) -> R<Loss> {
    let list = images.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(",");
    let files = format!("SELECT DISTINCT file FROM image WHERE copy IS NULL AND id IN ({list})");
    loss_for(&db.conn, &files, &[])
}

// ───────────────────────────────────────────── Imports

/// One import (run that brought photos): date, kind, source, photos still from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub id: i64,
    pub kind: String,
    pub source: String,
    pub begin: i64,
    pub photos: usize,
}

/// Imports, newest first: every run except the silent sync, plus syncs that brought photos still in the library.
pub fn imports(db: &Db) -> R<Vec<Import>> {
    let mut st = db
        .conn
        .prepare(
            "SELECT l.id, l.kind, coalesce(l.source, ''), l.started, (SELECT count(*) FROM image b WHERE b.run = l.id AND b.copy IS NULL) AS n
             FROM run l WHERE l.kind <> 'sync' OR n > 0 ORDER BY l.started DESC, l.id DESC",
        )
        .map_err(f)?;
    let rows = st
        .query_map([], |r| Ok(Import { id: r.get(0)?, kind: r.get(1)?, source: r.get(2)?, begin: r.get(3)?, photos: r.get::<_, i64>(4)? as usize }))
        .map_err(f)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(f)
}

const IMPORT_FILES: &str = "SELECT DISTINCT file FROM image WHERE run = ?1 AND copy IS NULL";

pub fn import_loss(db: &Db, run: i64) -> R<Loss> {
    loss_for(&db.conn, IMPORT_FILES, &[&run])
}

/// Remove the photos of an import from the library (files untouched, excluded for the sync).
pub fn remove_import_photos(db: &Db, run: i64) -> R<Snapshot> {
    let images: Vec<i64> = {
        let mut st = db.conn.prepare("SELECT id FROM image WHERE run = ?1 AND copy IS NULL").map_err(f)?;
        let v = st.query_map([run], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
        v
    };
    remove_photos(db, &images)
}

/// Delete only the entry of an import from the list (photos stay; they no longer count as "recently imported").
pub fn delete_import_entry(db: &Db, run: i64) -> R<Snapshot> {
    remove_rows(&db.conn, "run", "id", &[Value::Integer(run)], Snapshot::default())
}

// ───────────────────────────────────────────── Collections

/// Kinds in the table `collection`.
pub const COLLECTION: i64 = 0;
pub const SET: i64 = 1;
pub const SMART: i64 = 2;

pub(crate) fn changed(db: &Db, table: &str, id: i64, column: &str, new: Value) -> R<Snapshot> {
    let old: Value = db.conn.query_row(&format!("SELECT {} FROM {} WHERE id = ?1", quote(column), quote(table)), [id], |r| r.get(0)).map_err(f)?;
    db.conn.execute(&format!("UPDATE {} SET {} = ?1 WHERE id = ?2", quote(table), quote(column)), params![new, id]).map_err(f)?;
    Ok(Snapshot { changed: vec![(table.to_string(), id, column.to_string(), old)], ..Snapshot::default() })
}

pub(crate) fn clean_name(name: &str) -> R<String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("Der Name darf nicht leer sein.".into());
    }
    Ok(n.to_string())
}

/// Rename a collection, set or smart collection.
pub fn rename_collection(db: &Db, id: i64, name: &str) -> R<Snapshot> {
    changed(db, "collection", id, "name", Value::Text(clean_name(name)?))
}

/// Ids of a collection and everything below it.
fn collection_tree(conn: &Connection, id: i64) -> R<Vec<i64>> {
    let mut st = conn
        .prepare("WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM collection s JOIN t ON s.parent = t.id) SELECT id FROM t")
        .map_err(f)?;
    let v = st.query_map([id], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
    Ok(v)
}

/// What deleting a collection (with everything inside a set) takes: (collections, photo assignments).
pub fn collection_loss(db: &Db, id: i64) -> R<(usize, usize)> {
    let tree = collection_tree(&db.conn, id)?;
    let list = tree.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let n: i64 = db.conn.query_row(&format!("SELECT count(*) FROM collection_image WHERE collection IN ({list})"), [], |r| r.get(0)).map_err(f)?;
    Ok((tree.len(), n as usize))
}

/// Delete a collection (a set with everything inside). Photos stay in the library.
pub fn delete_collection(db: &Db, id: i64) -> R<Snapshot> {
    remove_rows(&db.conn, "collection", "id", &[Value::Integer(id)], Snapshot::default())
}

/// Move a collection into a set (`None`: top level), at the end of its new level. Never into itself or below itself,
/// only into sets, albums of Apple Photos never out of their set (`crate::collection_order::place`).
pub fn move_collection(db: &Db, id: i64, parent: Option<i64>) -> R<Snapshot> {
    crate::collection_order::place(db, id, parent, None)
}

/// New collection, set or smart collection (`search` for smart collections), optionally in a set; the name is
/// made unique within its set. Returns the new id.
pub fn create_collection(db: &Db, kind: i64, name: &str, parent: Option<i64>, search: Option<&str>) -> R<i64> {
    let base = clean_name(name)?;
    if let Some(p) = parent {
        // the set "Apple Fotos" mirrors Apple Photos: nothing else goes in
        let apple: bool = db.conn.query_row("SELECT coalesce(rules, '') = ?2 FROM collection WHERE id = ?1", params![p, crate::apple_photos::COLLECTION_MARKER], |r| r.get(0)).optional().map_err(f)?.unwrap_or(false);
        if apple {
            return Err(crate::collection_order::Blocked::AppleSet.message().into());
        }
    }
    let mut new = base.clone();
    let mut n = 2;
    while db.conn.query_row("SELECT count(*) FROM collection WHERE name = ?1 AND parent IS ?2 AND kind = ?3", params![&new, parent, kind], |r| r.get::<_, i64>(0)).map_err(f)? > 0 {
        new = format!("{base} {n}");
        n += 1;
    }
    db.conn
        .execute("INSERT INTO collection(name, kind, parent, search, translatable) VALUES(?1, ?2, ?3, ?4, 1)", params![&new, kind, parent, search])
        .map_err(f)?;
    Ok(db.conn.last_insert_rowid())
}

/// Add photos to a plain collection (drag and drop, new collection with the selection). Returns: newly added and
/// the snapshot for ⌘Z (only the new assignments).
/// Why photos cannot be put into an album of Apple Photos (shown as it is; translated in the app).
pub const APPLE_ALBUM: &str = "Alben aus Apple Fotos lassen sich hier nicht füllen – der nächste Abgleich würde die Zuordnung wieder überschreiben.";

pub fn add_to_collection(db: &Db, collection: i64, images: &[i64]) -> R<(usize, Snapshot)> {
    let row: Option<(i64, Option<String>)> = db.conn.query_row("SELECT kind, rules FROM collection WHERE id = ?1", [collection], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(f)?;
    if row.as_ref().is_some_and(|(_, rules)| rules.as_deref() == Some(crate::apple_photos::COLLECTION_MARKER)) {
        return Err(APPLE_ALBUM.into());
    }
    if !matches!(row.map(|r| r.0), Some(COLLECTION) | Some(3)) {
        return Err("Fotos lassen sich nur in eine einfache Sammlung legen.".into());
    }
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let mut pos: f64 = tx.query_row("SELECT coalesce(max(position), 0) FROM collection_image WHERE collection = ?1", [collection], |r| r.get(0)).map_err(f)?;
    let mut added: Vec<Vec<Value>> = Vec::new();
    for &b in images {
        pos += 1.0;
        if tx.execute("INSERT OR IGNORE INTO collection_image(collection, image, position) VALUES(?1, ?2, ?3)", params![collection, b, pos]).map_err(f)? > 0 {
            added.push(vec![Value::Integer(collection), Value::Integer(b)]);
        }
    }
    tx.commit().map_err(f)?;
    let n = added.len();
    Ok((n, Snapshot { tables: vec![TableRows { table: "collection_image".into(), columns: vec!["collection".into(), "image".into()], rows: added }], ..Snapshot::default() }))
}

/// Take photos out of a plain collection (the photos and their files stay in the library). Returns: number removed
/// and the snapshot for ⌘Z ([`undo`] puts the assignments back with their positions). In a collection from
/// Lightroom the removal also survives a new import (an assignment graineo no longer holds stays out).
pub fn remove_from_collection(db: &Db, collection: i64, images: &[i64]) -> R<(usize, Snapshot)> {
    let kind: Option<i64> = db.conn.query_row("SELECT kind FROM collection WHERE id = ?1", [collection], |r| r.get(0)).optional().map_err(f)?;
    if !matches!(kind, Some(COLLECTION) | Some(3)) {
        return Err("Fotos lassen sich nur aus einer einfachen Sammlung entfernen.".into());
    }
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let mut removed: Vec<Vec<Value>> = Vec::new();
    {
        let mut get = tx.prepare_cached("SELECT position FROM collection_image WHERE collection = ?1 AND image = ?2").map_err(f)?;
        let mut del = tx.prepare_cached("DELETE FROM collection_image WHERE collection = ?1 AND image = ?2").map_err(f)?;
        for &b in images {
            let Some(pos) = get.query_row(params![collection, b], |r| r.get::<_, Value>(0)).optional().map_err(f)? else { continue };
            del.execute(params![collection, b]).map_err(f)?;
            removed.push(vec![Value::Integer(collection), Value::Integer(b), pos]);
        }
    }
    tx.commit().map_err(f)?;
    let n = removed.len();
    let columns = vec!["collection".into(), "image".into(), "position".into()];
    Ok((n, Snapshot { tables: vec![TableRows { table: "collection_image".into(), columns, rows: removed }], ..Snapshot::default() }))
}

/// Undo [`add_to_collection`]: take exactly the assignments it added out again.
pub fn undo_add(db: &Db, snap: &Snapshot) -> R<()> {
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    for t in snap.tables.iter().filter(|t| t.table == "collection_image") {
        for r in &t.rows {
            tx.execute("DELETE FROM collection_image WHERE collection = ?1 AND image = ?2", params![r[0], r[1]]).map_err(f)?;
        }
    }
    tx.commit().map_err(f)
}

// ───────────────────────────────────────────── Keywords and people

/// Photos whose keywords change with this keyword (it and everything below): their sidecars are rewritten.
pub(crate) fn keyword_images(conn: &Connection, id: i64) -> R<Vec<i64>> {
    let mut st = conn
        .prepare(
            "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM keyword s JOIN t ON s.parent = t.id)
             SELECT DISTINCT b.file FROM image_keyword bs JOIN image b ON b.id = bs.image WHERE bs.keyword IN (SELECT id FROM t)",
        )
        .map_err(f)?;
    let v = st.query_map([id], |r| r.get(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
    Ok(v)
}

/// The keyword paths of these photos changed (a keyword was moved, renamed, merged or deleted): note the time
/// (the newer state wins when sidecars are read in) and queue their sidecars. Not marked as an own change of the
/// assignments: the Lightroom import keeps moved and renamed keywords by itself (three-way merge of name and
/// parent) and never brings back deleted or merged ones, so marking every photo of a large branch would only stop
/// later Lightroom updates of all their other keywords.
pub(crate) fn touch_keywords(conn: &Connection, files: &[i64]) -> R<()> {
    let t = crate::now();
    // every photo is tried (one failure does not leave the rest behind); the first error and the count come back
    let mut first: Option<String> = None;
    let mut failed = 0usize;
    for d in files {
        let r = conn.execute("UPDATE image SET keywords_time = ?2 WHERE file = ?1", params![d, t]).map_err(f).and_then(|_| crate::store::queue_sidecar(conn, *d));
        if let Err(e) = r {
            failed += 1;
            first.get_or_insert(e);
        }
    }
    match first {
        None => Ok(()),
        Some(e) => Err(format!("{failed} of {} photos: {e}", files.len())),
    }
}

/// After a committed keyword change: keyword times and sidecars of `files`, search entries of `images`. A failure
/// here only goes onto the snapshot as a warning: the change stands, and an error would lose its ⌘Z step.
pub(crate) fn follow_up(db: &Db, snap: &mut Snapshot, files: &[i64], images: &[i64]) {
    snap.warn("keyword times and sidecars", touch_keywords(&db.conn, files));
    snap.warn("search index", crate::keyword_arrange::renew_search(db, images));
}

/// Photo files and images below keyword `id` (for [`follow_up`]); a failed lookup is a warning on `snap`.
pub(crate) fn below_for_follow_up(db: &Db, snap: &mut Snapshot, ids: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let mut files = Vec::new();
    for &id in ids {
        files.extend(snap.warn("keyword times and sidecars", keyword_images(&db.conn, id)).unwrap_or_default());
    }
    files.sort_unstable();
    files.dedup();
    let images = snap.warn("search index", crate::keyword_arrange::images_below(&db.conn, ids)).unwrap_or_default();
    (files, images)
}

/// Mark the assignments of these photos as changed in graineo (a Lightroom import keeps them) and return the old
/// marks for ⌘Z.
fn mark_assignments_changed(conn: &Connection, images: &[i64]) -> R<Vec<(String, i64, String, Value)>> {
    let t = crate::now();
    let mut old = Vec::with_capacity(images.len());
    let mut get = conn.prepare_cached("SELECT keywords_changed FROM image WHERE id = ?1").map_err(f)?;
    let mut set = conn.prepare_cached("UPDATE image SET keywords_changed = ?2 WHERE id = ?1").map_err(f)?;
    for &b in images {
        let Some(v) = get.query_row([b], |r| r.get::<_, Value>(0)).optional().map_err(f)? else { continue };
        set.execute(params![b, t]).map_err(f)?;
        old.push(("image".to_string(), b, "keywords_changed".to_string(), v));
    }
    Ok(old)
}

/// Snapshot of assignments changed by hand ([`crate::keywords::change`]): `added` and `removed` (image, keyword),
/// the old "changed in graineo" marks per image. ⌘Z: [`undo`] + [`after_keyword_undo`].
pub(crate) fn assignment_snapshot(added: Vec<(i64, i64)>, removed: Vec<(i64, i64)>, marks: Vec<(i64, Value)>) -> Snapshot {
    let rows = |v: Vec<(i64, i64)>| v.into_iter().map(|(b, k)| vec![Value::Integer(b), Value::Integer(k)]).collect::<Vec<_>>();
    let columns = vec!["image".to_string(), "keyword".to_string()];
    let mut snap = Snapshot::default();
    if !added.is_empty() {
        snap.added.push(TableRows { table: "image_keyword".into(), columns: columns.clone(), rows: rows(added) });
    }
    if !removed.is_empty() {
        snap.tables.push(TableRows { table: "image_keyword".into(), columns, rows: rows(removed) });
    }
    snap.changed = marks.into_iter().map(|(b, v)| ("image".to_string(), b, "keywords_changed".to_string(), v)).collect();
    snap
}

/// Number of photos with this keyword (or one below it).
pub fn keyword_loss(db: &Db, id: i64) -> R<usize> {
    let n: i64 = db
        .conn
        .query_row(
            "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM keyword s JOIN t ON s.parent = t.id)
             SELECT count(DISTINCT image) FROM image_keyword WHERE keyword IN (SELECT id FROM t)",
            [id],
            |r| r.get(0),
        )
        .map_err(f)?;
    Ok(n as usize)
}

/// Keyword of the same name at the same place as `id` (another one), if any: renaming to `name` would have to
/// merge into it ([`merge_keyword`]).
pub fn keyword_clash(db: &Db, id: i64, name: &str) -> R<Option<i64>> {
    let name = clean_name(name)?;
    let parent: Option<i64> = db.conn.query_row("SELECT parent FROM keyword WHERE id = ?1", [id], |r| r.get(0)).map_err(f)?;
    crate::keywords::sibling_named(&db.conn, parent, &name, Some(id))
}

/// Keyword of the same name (ignoring case) already below `parent` (`None`: top level): moving `id` there would
/// make two of them at one place – the interface offers merging into it instead ([`merge_keyword`]).
pub fn move_clash(db: &Db, id: i64, parent: Option<i64>) -> R<Option<i64>> {
    let name: String = db.conn.query_row("SELECT name FROM keyword WHERE id = ?1", [id], |r| r.get(0)).map_err(f)?;
    crate::keywords::sibling_named(&db.conn, parent, &name, Some(id))
}

/// Rename a keyword (also a person). A keyword of the same name at the same place is not merged: error
/// (the interface offers [`merge_keyword`] instead).
pub fn rename_keyword(db: &Db, id: i64, name: &str) -> R<Snapshot> {
    let name = clean_name(name)?;
    if keyword_clash(db, id, &name)?.is_some() {
        return Err("Ein Stichwort mit diesem Namen gibt es dort schon.".into());
    }
    let mut snap = changed(db, "keyword", id, "name", Value::Text(name))?;
    // committed: what follows only warns
    let (files, images) = below_for_follow_up(db, &mut snap, &[id]);
    follow_up(db, &mut snap, &files, &images);
    Ok(snap)
}

/// Merge keyword `from` into `into` (renaming onto an existing name): every photo with `from` gets `into`,
/// keywords below `from` move below `into`, faces named with `from` (person) get `into`, then `from` is
/// deleted. Both must be keywords or both people; a keyword below `from` with the name of one below `into`
/// is refused (it would have to be merged too). ⌘Z ([`undo`] + [`after_keyword_undo`]) restores everything.
pub fn merge_keyword(db: &Db, from: i64, into: i64) -> R<Snapshot> {
    if from == into {
        return Err("Ein Stichwort lässt sich nicht mit sich selbst zusammenführen.".into());
    }
    let person = |id: i64| -> R<i64> { db.conn.query_row("SELECT person FROM keyword WHERE id = ?1", [id], |r| r.get(0)).map_err(f) };
    if person(from)? != person(into)? {
        return Err("Ein Stichwort und eine Person lassen sich nicht zusammenführen.".into());
    }
    let below = |id: i64| -> R<Vec<(i64, String)>> {
        let mut st = db.conn.prepare("SELECT id, name FROM keyword WHERE parent = ?1").map_err(f)?;
        let v = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
        Ok(v)
    };
    let (children, target_children) = (below(from)?, below(into)?);
    if let Some((_, n)) = children.iter().find(|(_, n)| target_children.iter().any(|(_, t)| t.to_lowercase() == n.to_lowercase())) {
        return Err(format!("Unter beiden Stichwörtern gibt es „{n}“. Bitte zuerst eines davon umbenennen oder zusammenführen."));
    }
    // `into` must not lie below `from` (it would be deleted with it)
    let inside: i64 = db
        .conn
        .query_row(
            "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM keyword s JOIN t ON s.parent = t.id) SELECT count(*) FROM t WHERE id = ?2",
            params![from, into],
            |r| r.get(0),
        )
        .map_err(f)?;
    if inside > 0 {
        return Err("Das Ziel liegt unter dem Stichwort, das zusammengeführt wird.".into());
    }
    let files = keyword_images(&db.conn, from)?;
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    // 1. photos with `from` get `into` (remember only the new assignments)
    let mut added: Vec<Vec<Value>> = Vec::new();
    {
        let mut st = tx.prepare("SELECT image FROM image_keyword WHERE keyword = ?1 AND image NOT IN (SELECT image FROM image_keyword WHERE keyword = ?2)").map_err(f)?;
        let images = st.query_map(params![from, into], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
        let mut ins = tx.prepare("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1, ?2)").map_err(f)?;
        for b in images {
            if ins.execute(params![b, into]).map_err(f)? > 0 {
                added.push(vec![Value::Integer(b), Value::Integer(into)]);
            }
        }
    }
    // 2. keywords below `from` and faces named with it move to `into`
    let mut changes: Vec<(String, i64, String, Value)> = Vec::new();
    for (c, _) in &children {
        tx.execute("UPDATE keyword SET parent = ?1 WHERE id = ?2", params![into, c]).map_err(f)?;
        changes.push(("keyword".into(), *c, "parent".into(), Value::Integer(from)));
    }
    {
        let mut st = tx.prepare("SELECT id FROM face WHERE person = ?1").map_err(f)?;
        let faces = st.query_map([from], |r| r.get::<_, i64>(0)).map_err(f)?.collect::<rusqlite::Result<Vec<_>>>().map_err(f)?;
        for g in faces {
            tx.execute("UPDATE face SET person = ?1 WHERE id = ?2", params![into, g]).map_err(f)?;
            changes.push(("face".into(), g, "person".into(), Value::Integer(from)));
        }
    }
    // 3. only the photos that newly got `into` have a different list of assignments: marked as changed in graineo
    //    (otherwise a Lightroom import would take `into` away again, `from` no longer existing there)
    let newly: Vec<i64> = added.iter().filter_map(|r| if let Value::Integer(b) = r[0] { Some(b) } else { None }).collect();
    changes.extend(mark_assignments_changed(&tx, &newly)?);
    // 4. delete `from` (snapshot of its row and its remaining assignments), in the same transaction: either the
    //    whole merge or nothing
    let mut snap = remove_rows_in(&tx, "keyword", "id", &[Value::Integer(from)], Snapshot::default())?;
    tx.commit().map_err(f)?;
    snap.changed = changes;
    snap.added = vec![TableRows { table: "image_keyword".into(), columns: vec!["image".into(), "keyword".into()], rows: added }];
    // committed: what follows only warns (the search entries are renewed by the caller)
    snap.warn("keyword times and sidecars", touch_keywords(&db.conn, &files));
    Ok(snap)
}

/// Delete a keyword with everything below it; the photos lose it (they stay in the library). Faces named with a
/// person become unnamed.
pub fn delete_keyword(db: &Db, id: i64) -> R<Snapshot> {
    let files = keyword_images(&db.conn, id)?;
    let mut snap = remove_rows(&db.conn, "keyword", "id", &[Value::Integer(id)], Snapshot::default())?;
    // committed: what follows only warns (the search entries are renewed by the caller)
    snap.warn("keyword times and sidecars", touch_keywords(&db.conn, &files));
    Ok(snap)
}

/// Move a keyword below another (`None`: top level); never below itself, never beside a keyword of the same name
/// (error; [`move_clash`] names it, the interface asks to merge).
pub fn move_keyword(db: &Db, id: i64, parent: Option<i64>) -> R<Snapshot> {
    if let Some(p) = parent {
        let below: i64 = db
            .conn
            .query_row(
                "WITH RECURSIVE t(id) AS (SELECT ?1 UNION SELECT s.id FROM keyword s JOIN t ON s.parent = t.id) SELECT count(*) FROM t WHERE id = ?2",
                params![id, p],
                |r| r.get(0),
            )
            .map_err(f)?;
        if below > 0 {
            return Err("Ein Stichwort kann nicht unter sich selbst liegen.".into());
        }
    }
    if move_clash(db, id, parent)?.is_some() {
        return Err("Ein Stichwort mit diesem Namen gibt es dort schon.".into());
    }
    let mut snap = changed(db, "keyword", id, "parent", parent.map_or(Value::Null, Value::Integer))?;
    // committed: what follows only warns
    let (files, images) = below_for_follow_up(db, &mut snap, &[id]);
    follow_up(db, &mut snap, &files, &images);
    Ok(snap)
}

/// Re-queue sidecars after ⌘Z of a keyword change (the keywords of these photos changed back).
pub fn after_keyword_undo(db: &Db, snap: &Snapshot) -> R<()> {
    let mut ids: Vec<i64> = snap.changed.iter().filter(|c| c.0 == "keyword").map(|c| c.1).collect();
    for t in snap.tables.iter().filter(|t| t.table == "keyword") {
        ids.extend(t.rows.iter().filter_map(|r| if let Value::Integer(i) = r[0] { Some(i) } else { None }));
    }
    let mut files = Vec::new();
    let mut tree_images: Vec<i64> = Vec::new();
    for id in ids {
        files.extend(keyword_images(&db.conn, id)?);
        tree_images.extend(crate::keyword_arrange::images_below(&db.conn, &[id])?);
    }
    // photos that lost an assignment added by the change (merge, adding by hand) or got one back (removing by hand)
    let mut images: Vec<i64> = Vec::new();
    for t in snap.added.iter().chain(snap.tables.iter()).filter(|t| t.table == "image_keyword") {
        for r in &t.rows {
            if let Value::Integer(b) = r[0] {
                images.push(b);
            }
        }
    }
    images.sort_unstable();
    images.dedup();
    for b in &images {
        if let Some(d) = db.conn.query_row("SELECT file FROM image WHERE id = ?1", [b], |r| r.get::<_, i64>(0)).optional().map_err(f)? {
            files.push(d);
        }
    }
    files.sort_unstable();
    files.dedup();
    touch_keywords(&db.conn, &files)?;
    // their search entries follow (the index holds the whole chain of every keyword)
    images.extend(tree_images);
    crate::keyword_arrange::renew_search(db, &images)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (tempdir::Dir, Db) {
        let d = tempdir::Dir::new();
        let db = Db::open(&d.0.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, ''), (2, 1, 'reise/'), (3, 1, 'reise/tag2/');
                 INSERT INTO run(id, kind, source, started) VALUES(7, 'import', 'Karte', 100);
                 INSERT INTO file(id, folder, name, format) VALUES(10, 1, 'a.jpg', 'JPG'), (11, 2, 'b.jpg', 'JPG'), (12, 3, 'c.jpg', 'JPG');
                 INSERT INTO image(id, file, stars, run) VALUES(20, 10, 0, NULL), (21, 11, 3, 7), (22, 12, 0, 7);
                 INSERT INTO image(id, file, copy) VALUES(23, 11, 'Kopie 1');
                 INSERT INTO keyword(id, name) VALUES(30, 'Urlaub'), (32, 'Anna');
                 INSERT INTO keyword(id, name, parent) VALUES(31, 'Meer', 30);
                 UPDATE keyword SET person = 1 WHERE id = 32;
                 INSERT INTO image_keyword(image, keyword) VALUES(21, 31), (22, 30), (20, 32);
                 INSERT INTO collection(id, name, kind) VALUES(40, 'Satz', 1);
                 INSERT INTO collection(id, name, kind, parent) VALUES(41, 'Best of', 0, 40);
                 INSERT INTO collection_image(collection, image, position) VALUES(41, 21, 1), (41, 20, 2);
                 INSERT INTO develop_state(file, copy, settings) VALUES(11, '', '{}');
                 INSERT INTO face(id, image, x, y, w, h, source, person) VALUES(50, 20, 0.1, 0.1, 0.2, 0.2, 'manual', 32);",
            )
            .unwrap();
        (d, db)
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let p = std::env::temp_dir().join(format!("graineo-manage-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    /// Albums mirrored from Apple Photos take no photos by hand (the next sync would overwrite them); plain
    /// collections still do.
    #[test]
    fn apple_albums_take_no_photos() {
        let (_d, db) = library();
        db.conn.execute_batch("INSERT INTO collection(id, name, kind, rules) VALUES(60, 'Apple Fotos', 1, 'apple-photos'); INSERT INTO collection(id, name, kind, parent, rules) VALUES(61, 'Urlaub', 0, 60, 'apple-photos');").unwrap();
        assert_eq!(add_to_collection(&db, 61, &[20, 22]).err().as_deref(), Some(APPLE_ALBUM));
        assert_eq!(count(&db, "SELECT count(*) FROM collection_image WHERE collection = 61"), 0);
        assert!(add_to_collection(&db, 60, &[20]).is_err());
        assert_eq!(add_to_collection(&db, 41, &[22]).unwrap().0, 1);
    }

    #[test]
    fn removing_a_subfolder_takes_everything_below_and_undo_puts_it_back() {
        let (_d, db) = library();
        let loss = folder_loss(&db, 1, "reise/").unwrap();
        assert_eq!(loss, Loss { photos: 2, copies: 1, edited: 1, rated: 1, keywords: 2, collections: 1, faces: 0 });
        let snap = remove_folder(&db, 1, "reise/").unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM folder"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM develop_state"), 0);
        assert!(is_excluded(&exclusions(&db.conn, 1), "reise/"));
        assert!(is_excluded(&exclusions(&db.conn, 1), "reise/tag2/"));
        assert!(is_excluded(&exclusions(&db.conn, 1), "reise/tag2/c.jpg"));
        assert!(!is_excluded(&exclusions(&db.conn, 1), "a.jpg"));
        assert_eq!(snap.rows_of("file"), 2);
        undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 3);
        assert_eq!(count(&db, "SELECT count(*) FROM image"), 4);
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword"), 3);
        assert_eq!(count(&db, "SELECT count(*) FROM collection_image"), 2);
        assert_eq!(count(&db, "SELECT count(*) FROM develop_state"), 1);
        assert_eq!(count(&db, "SELECT stars FROM image WHERE id = 21"), 3);
        assert!(exclusions(&db.conn, 1).is_empty());
    }

    #[test]
    fn removing_from_a_collection_keeps_the_photo_and_undo_restores_the_position() {
        let (_d, db) = library();
        let (n, snap) = remove_from_collection(&db, 41, &[20, 22]).unwrap();
        // only 20 was in the collection; the photo itself stays
        assert_eq!(n, 1);
        assert_eq!(count(&db, "SELECT count(*) FROM collection_image WHERE collection = 41"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE id = 20"), 1);
        undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM collection_image WHERE collection = 41"), 2);
        assert_eq!(count(&db, "SELECT CAST(position AS INTEGER) FROM collection_image WHERE collection = 41 AND image = 20"), 2);
        // not from a collection set
        assert!(remove_from_collection(&db, 40, &[21]).is_err());
    }

    #[test]
    fn removing_a_root_folder_and_undo() {
        let (_d, db) = library();
        let snap = remove_folder(&db, 1, "").unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM root_folder"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM image"), 0);
        // the face's person (keyword) stays, the face goes with the photo
        assert_eq!(count(&db, "SELECT count(*) FROM keyword"), 3);
        undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM face WHERE person = 32"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM image"), 4);
    }

    #[test]
    fn the_sync_does_not_take_a_removed_folder_in_again() {
        let d = tempdir::Dir::new();
        let photos = d.0.join("fotos");
        std::fs::create_dir_all(photos.join("reise")).unwrap();
        std::fs::write(photos.join("reise/b.jpg"), b"x").unwrap();
        std::fs::write(photos.join("a.jpg"), b"x").unwrap();
        let db = Db::open(&d.0.join("library.sqlite")).unwrap();
        let root = db.create_root(&format!("{}/", photos.display()), "fotos", "own", None).unwrap();
        db.create_folder(root, "", None).unwrap();
        crate::reconcile::reconcile(&db, None, &|_| {}).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 2);
        remove_folder(&db, root, "reise/").unwrap();
        // a single photo too
        let a: i64 = db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id = b.file WHERE d.name = 'a.jpg'", [], |r| r.get(0)).unwrap();
        remove_photos(&db, &[a]).unwrap();
        crate::reconcile::reconcile(&db, None, &|_| {}).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 0, "files stay out after the sync");
        assert!(photos.join("reise/b.jpg").exists() && photos.join("a.jpg").exists(), "files untouched");
        // adding the folder explicitly takes it back
        db.create_folder(root, "reise/", None).unwrap();
        crate::reconcile::reconcile(&db, None, &|_| {}).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 1);
    }

    #[test]
    fn lifting_exclusions_on_explicit_import() {
        let (_d, db) = library();
        remove_folder(&db, 1, "reise/").unwrap();
        assert_eq!(lift_exclusions(&db.conn, 1, "reise/tag2/").unwrap(), 1);
        assert!(exclusions(&db.conn, 1).is_empty());
    }

    #[test]
    fn imports_list_remove_photos_and_entry() {
        let (_d, db) = library();
        let list = imports(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].id, list[0].photos), (7, 2));
        let snap = remove_import_photos(&db, 7).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 1);
        assert!(is_excluded(&exclusions(&db.conn, 1), "reise/b.jpg"));
        undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE run = 7"), 2);
        // only the entry: photos stay, their reference to the run goes and comes back
        let snap = delete_import_entry(&db, 7).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM run"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE run IS NULL"), 4);
        undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE run = 7"), 2);
    }

    #[test]
    fn removing_a_virtual_copy_only_takes_the_copy() {
        let (_d, db) = library();
        let snap = remove_photos(&db, &[23]).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image"), 3);
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 3);
        assert!(exclusions(&db.conn, 1).is_empty());
        undo(&db, &snap).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image"), 4);
    }

    #[test]
    fn collections_rename_move_delete_and_undo() {
        let (_d, db) = library();
        let s = rename_collection(&db, 41, "  Beste  ").unwrap();
        assert_eq!(db.conn.query_row("SELECT name FROM collection WHERE id = 41", [], |r| r.get::<_, String>(0)).unwrap(), "Beste");
        undo(&db, &s).unwrap();
        assert_eq!(db.conn.query_row("SELECT name FROM collection WHERE id = 41", [], |r| r.get::<_, String>(0)).unwrap(), "Best of");
        assert!(rename_collection(&db, 41, " ").is_err());
        // move out of the set, and back; never into a plain collection or itself
        let s = move_collection(&db, 41, None).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM collection WHERE parent IS NULL"), 2);
        undo(&db, &s).unwrap();
        assert!(move_collection(&db, 40, Some(41)).is_err());
        assert!(move_collection(&db, 40, Some(40)).is_err());
        assert_eq!(collection_loss(&db, 40).unwrap(), (2, 2));
        let s = delete_collection(&db, 40).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM collection"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM image"), 4, "photos stay");
        undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM collection_image"), 2);
        assert_eq!(count(&db, "SELECT parent FROM collection WHERE id = 41"), 40);
    }

    #[test]
    fn create_and_add_to_collection_with_undo() {
        let (_d, db) = library();
        let id = create_collection(&db, COLLECTION, "Best of", Some(40), None).unwrap();
        assert_eq!(db.conn.query_row("SELECT name FROM collection WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap(), "Best of 2");
        let (n, s) = add_to_collection(&db, 41, &[20, 21, 22]).unwrap();
        assert_eq!(n, 1, "20 and 21 are already in it");
        undo_add(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM collection_image WHERE collection = 41"), 2);
        assert!(add_to_collection(&db, 40, &[20]).is_err(), "sets take no photos");
    }

    #[test]
    fn keywords_rename_move_delete_and_undo() {
        let (_d, db) = library();
        let s = rename_keyword(&db, 31, "Urlaub").unwrap();
        // same name at another place is fine – but moving it next to the other "Urlaub" is not (merging instead)
        assert!(move_keyword(&db, 31, None).is_err());
        undo(&db, &s).unwrap();
        let s = rename_keyword(&db, 31, "See").unwrap();
        // (e) renaming and moving change no assignment: the photos are not marked as changed in graineo (that
        // would stop later Lightroom updates of all their keywords), only their time and sidecars follow
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE id = 21 AND keywords_time IS NOT NULL"), 1);
        undo(&db, &s).unwrap();
        let s = move_keyword(&db, 31, None).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE parent IS NULL"), 3);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 0);
        undo(&db, &s).unwrap();
        assert!(move_keyword(&db, 30, Some(31)).is_err());
        assert_eq!(keyword_loss(&db, 30).unwrap(), 2);
        let s = delete_keyword(&db, 30).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 0, "a deleted keyword does not come back from Lightroom anyway");
        undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword"), 3);
        // deleting a person: the face stays, unnamed; ⌘Z names it again
        let s = delete_keyword(&db, 32).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM face WHERE person IS NULL"), 1);
        undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM face WHERE person = 32"), 1);
    }

    /// (d) Moving next to a keyword of the same name (ignoring case) is refused and names the one to merge into;
    /// renaming checks the same way.
    #[test]
    fn moving_onto_a_sibling_of_the_same_name_is_refused() {
        let (_d, db) = library();
        db.conn.execute_batch("INSERT INTO keyword(id, name) VALUES(36, 'meer');").unwrap();
        assert_eq!(move_clash(&db, 31, None).unwrap(), Some(36));
        assert!(move_keyword(&db, 31, None).is_err());
        assert_eq!(count(&db, "SELECT parent FROM keyword WHERE id = 31"), 30, "nothing moved");
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE lower(name) = 'meer' AND parent IS NULL"), 1, "no second one at the top");
        // the interface merges instead; ⌘Z puts everything back
        let s = merge_keyword(&db, 31, 36).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 21 AND keyword = 36"), 1);
        undo(&db, &s).unwrap();
        after_keyword_undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE image = 21 AND keyword = 31"), 1);
        assert_eq!(keyword_clash(&db, 36, "URLAUB").unwrap(), Some(30), "renaming ignores case too");
        assert_eq!(move_clash(&db, 31, Some(30)).unwrap(), None, "its own place is fine");
    }

    #[test]
    fn merging_keywords_and_undo() {
        let (_d, db) = library();
        // second top-level keyword "Ferien" with photo 20 and 22 (22 also has "Urlaub"), a child "Berge"
        db.conn
            .execute_batch(
                "INSERT INTO keyword(id, name) VALUES(33, 'Ferien');
                 INSERT INTO keyword(id, name, parent) VALUES(34, 'Berge', 33);
                 INSERT INTO image_keyword(image, keyword) VALUES(20, 33), (22, 33);",
            )
            .unwrap();
        assert_eq!(keyword_clash(&db, 33, " Urlaub ").unwrap(), Some(30));
        assert_eq!(keyword_clash(&db, 33, "Neu").unwrap(), None);
        assert!(rename_keyword(&db, 33, "Urlaub").is_err(), "renaming onto an existing name stays refused");
        assert!(merge_keyword(&db, 33, 32).is_err(), "keyword and person are not merged");
        assert!(merge_keyword(&db, 30, 31).is_err(), "never into a keyword below itself");
        let pairs = |db: &Db| -> Vec<(i64, i64)> {
            let mut st = db.conn.prepare("SELECT image, keyword FROM image_keyword ORDER BY 1, 2").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect()
        };
        let before = pairs(&db);
        let s = merge_keyword(&db, 33, 30).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE id = 33"), 0, "the old keyword is gone");
        assert_eq!(count(&db, "SELECT count(*) FROM image_keyword WHERE keyword = 30 AND image IN (20, 22)"), 2, "both photos have the target");
        assert_eq!(count(&db, "SELECT parent FROM keyword WHERE id = 34"), 30, "the child moved below the target");
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE id = 20 AND keywords_changed IS NOT NULL"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 1, "only photo 20 got a new assignment (22 had both)");
        undo(&db, &s).unwrap();
        after_keyword_undo(&db, &s).unwrap();
        assert_eq!(before, pairs(&db), "⌘Z restores every assignment");
        assert_eq!(count(&db, "SELECT count(*) FROM image WHERE keywords_changed IS NOT NULL"), 0, "⌘Z takes the mark back too");
        assert_eq!(count(&db, "SELECT parent FROM keyword WHERE id = 34"), 33);
        // people: faces follow the merge
        db.conn.execute_batch("INSERT INTO keyword(id, name, person) VALUES(35, 'Anna B.', 1);").unwrap();
        let s = merge_keyword(&db, 32, 35).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM face WHERE person = 35"), 1);
        undo(&db, &s).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM face WHERE person = 32"), 1);
    }
}
