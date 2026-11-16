//! Smart collections follow renamed and merged keywords: their rules name keywords by text (`keyword:="Rom"`,
//! `keyword:Rom`, `keyword:="Orte > Rom"`), so a rename or a merge would leave them finding nothing. The terms naming
//! the old keyword exactly (its name or its path, ignoring case) are rewritten to the new one – by name, or by path
//! where the name exists more than once. Everything else in a rule (other fields, words, spacing) stays as it was.
//! The old rules are part of the change's ⌘Z step.
//!
//! A term is only rewritten when, before the change, it found nothing but the changed keyword (for a merge: or its
//! target). A bare name that exists twice ("Orte › Rom", "Personen › Rom") or a partial term that also finds other
//! keywords (`keyword:rom` finds "Romantik") would change what the collection shows: such a rule stays as it is
//! and the collection is named in the result ([`Snapshot::smart_kept`]).

use std::collections::HashMap;

use rusqlite::types::Value;

use crate::db::Db;
use crate::keywords::{self, SHOWN_SEPARATOR};
use crate::manage::Snapshot;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Folded comparison form of a keyword name or path ("Orte › Paris", "orte>paris" → "orte>paris").
fn key(text: &str) -> String {
    keywords::split_typed(text).iter().map(|s| crate::search::fold_text(s)).collect::<Vec<_>>().join(">")
}

/// How a rule names the keyword `id` best: its name, or its path ("Orte > Paris") when the name is not unique.
pub fn rule_text(db: &Db, id: i64) -> R<String> {
    let segs = keywords::segments(&db.conn, id)?;
    let name = segs.last().cloned().unwrap_or_default();
    let same: i64 = db.conn.query_row("SELECT count(*) FROM keyword WHERE lower(name) = lower(?1)", [&name], |r| r.get(0)).map_err(f)?;
    let folded = crate::search::fold_text(&name);
    // lower() only folds ASCII: count again with the app's folding when it might differ
    let same = if same > 1 || !name.is_ascii() {
        let mut st = db.conn.prepare_cached("SELECT name FROM keyword").map_err(f)?;
        let names = st.query_map([], |r| r.get::<_, String>(0)).map_err(f)?.flatten().filter(|n| crate::search::fold_text(n) == folded).count();
        names as i64
    } else {
        same
    };
    Ok(if same > 1 { segs.join(" > ") } else { name })
}

/// A value as it goes into a rule (quoted when needed).
fn quoted(v: &str) -> String {
    if v.chars().any(|c| c.is_whitespace() || c == '|' || c == '>' || c == '›') { format!("\"{}\"", v.replace('"', "")) } else { v.to_string() }
}

/// A keyword term split up: (`-` or "", field as written, `=` or "", alternatives without quotes).
fn keyword_term(term: &str) -> Option<(&str, &str, &str, Vec<String>)> {
    let (not, rest) = match term.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", term),
    };
    let (field, value) = rest.split_once([':', '：'])?;
    if crate::search_fields::field_id(field) != Some("keyword") {
        return None;
    }
    let (exact, value) = match value.strip_prefix('=') {
        Some(v) => ("=", v),
        None => ("", value),
    };
    let alternatives: Vec<String> = if value.contains('"') { vec![value.trim_matches('"').to_string()] } else { value.split('|').map(str::to_string).collect() };
    Some((not, field, exact, alternatives))
}

/// Rewrite one term (`keyword:…`, `-kw:="…"`, `keyword:a|b`) if it names `old` (folded name or path keys).
fn rewrite_term(term: &str, old: &[String], new: &str) -> Option<String> {
    let (not, field, exact, alternatives) = keyword_term(term)?;
    let mut changed = false;
    let rewritten: Vec<String> = alternatives
        .iter()
        .map(|a| {
            if old.contains(&key(a)) {
                changed = true;
                quoted(new)
            } else {
                quoted(a)
            }
        })
        .collect();
    if !changed {
        return None;
    }
    // quoted alternatives cannot stand in `a|b` without spaces: then as separate terms joined by " | "
    if rewritten.len() > 1 && rewritten.iter().any(|v| v.starts_with('"')) {
        return Some(rewritten.iter().map(|v| format!("{not}{field}:{exact}{v}")).collect::<Vec<_>>().join(" | "));
    }
    Some(format!("{not}{field}:{exact}{}", rewritten.join("|")))
}

/// Rewrite a whole rule; `None` if nothing in it names the old keyword.
pub fn rewrite_rule(rule: &str, old: &[String], new: &str) -> Option<String> {
    let mut out = String::with_capacity(rule.len());
    let mut changed = false;
    for (part, is_token) in crate::search_fields::tokens_with_gaps(rule) {
        match is_token.then(|| rewrite_term(part, old, new)).flatten() {
            Some(t) => {
                out.push_str(&t);
                changed = true;
            }
            None => out.push_str(part),
        }
    }
    changed.then_some(out)
}

/// The keys a rule may use for keyword `id` as it is called now: its name and its path.
pub fn old_keys(db: &Db, id: i64) -> R<Vec<String>> {
    let segs = keywords::segments(&db.conn, id)?;
    let mut v = vec![key(segs.last().map(String::as_str).unwrap_or("")), key(&segs.join(SHOWN_SEPARATOR))];
    v.dedup();
    Ok(v)
}

/// What the smart rules may follow, decided before a rename or merge of one keyword (afterwards the old names are
/// gone and a term's matches cannot be judged any more).
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// keys of the keyword as it is called before the change ([`old_keys`])
    keys: Vec<String>,
    /// per smart collection naming the keyword: (id, name, rule); `None` as rule if one of its terms is ambiguous
    rules: Vec<(i64, String, Option<String>)>,
}

/// Keyword ids a term value finds (as the search does: folded, by name or by path, exact or contained).
fn matches(db: &Db, value: &str, exact: bool) -> Vec<i64> {
    // folded as the query parser folds a value
    crate::search::keyword_ids(db, &crate::search::fold_text(value.trim()), exact)
}

/// Decide for every smart collection naming keyword `id` whether its rule can follow: every alternative that names the
/// keyword must find nothing but `id` (or `also`, the target of a merge); else the collection is kept as it is.
pub fn plan(db: &Db, id: i64, also: Option<i64>) -> R<Plan> {
    let keys = old_keys(db, id)?;
    let all: Vec<(i64, String, String)> = {
        let mut st = db.conn.prepare("SELECT id, name, search FROM collection WHERE kind = 2 AND search IS NOT NULL").map_err(f)?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        v
    };
    let mut cache: HashMap<(String, bool), bool> = HashMap::new();
    let mut rules = Vec::new();
    for (cid, name, rule) in all {
        let mut names_it = false;
        let mut unique = true;
        for (part, is_token) in crate::search_fields::tokens_with_gaps(&rule) {
            let Some((_, _, exact, alternatives)) = is_token.then(|| keyword_term(part)).flatten() else { continue };
            for a in alternatives.iter().filter(|a| keys.contains(&key(a))) {
                names_it = true;
                let ok = *cache.entry((a.clone(), exact == "=")).or_insert_with(|| matches(db, a, exact == "=").iter().all(|k| *k == id || Some(*k) == also));
                unique &= ok;
            }
        }
        if names_it {
            rules.push((cid, name, unique.then_some(rule)));
        }
    }
    Ok(Plan { keys, rules })
}

/// Rewrite the rules of the smart collections in `plan` to `new` (rule text, [`rule_text`] after the change). The
/// snapshot holds the old rules (⌘Z, same step as the change), how many changed ([`Snapshot::smart_changed`]) and
/// the collections kept because a term was ambiguous ([`Snapshot::smart_kept`]).
pub fn follow(db: &Db, plan: &Plan, new: &str) -> R<Snapshot> {
    let mut snap = Snapshot::default();
    let mut n = 0;
    for (id, name, rule) in &plan.rules {
        let Some(rule) = rule else {
            snap.smart_kept.push(name.clone());
            continue;
        };
        if let Some(new_rule) = rewrite_rule(rule, &plan.keys, new) {
            db.conn.execute("UPDATE collection SET search = ?1 WHERE id = ?2", rusqlite::params![new_rule, id]).map_err(f)?;
            snap.add_changed("collection", *id, "search", Value::Text(rule.clone()));
            n += 1;
        }
    }
    snap.smart_changed = n;
    if n > 0 {
        // the optional copy of the smart collections next to the photos
        let _ = crate::collection_file::write_if_enabled(db);
    }
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_naming_the_keyword_are_rewritten() {
        let old = vec![key("Rom"), key("Orte > Italien > Rom")];
        assert_eq!(rewrite_rule("keyword:=\"Rom\" rating>=3", &old, "Roma").as_deref(), Some("keyword:=Roma rating>=3"));
        assert_eq!(rewrite_rule("kw:rom|Paris  -keyword:=\"Orte › Italien › Rom\"", &old, "Orte > Roma").as_deref(), Some("kw:\"Orte > Roma\" | kw:Paris  -keyword:=\"Orte > Roma\""));
        assert_eq!(rewrite_rule("keyword:rom|Paris", &old, "Roma").as_deref(), Some("keyword:Roma|Paris"));
        assert_eq!(rewrite_rule("keyword:Romantik camera:rom", &old, "Roma"), None, "other names and fields stay");
    }
}
