//! Search in the library: full-text index (SQLite FTS5), query language, evaluation,
//! highlighting and translation of Lightroom smart collections.
//!
//! **Index** (one document per image, rowid = image ID): file name, folder, camera, lens,
//! caption, name of the virtual copy, keywords including parent terms, AI keywords,
//! AI scene and reasoning, collection names. Two tables:
//! - `search_word`: word stems (own simple German stems, umlauts folded, ß → ss),
//!   queried as prefix – "Wände" → `wand*` finds Wand, Wände, Wandfarbe.
//! - `search_trigram`: trigrams over the folded text – finds word parts in compounds
//!   ("schimmel" in "Wandschimmel"), only for search words of 5 characters or more (shorter ones would be too imprecise).
//! - Typos: if there is no hit, stems from the vocabulary with edit distance 1
//!   (from 8 characters 2) are used.
//!
//! **Syntax** (one field): words = AND; `a | b` or `a ODER b`; `-word` = NOT; `"Phrase"`;
//! `wand*` = prefix; fields `lens:` `camera:` `aperture:<=2.8` `focal:20-35` `iso:>=1600`
//! `shutter:<=1/60` `year:2021` `date:>2009-07-24` `keyword:` `folder:` `rating:>=3` `format:`
//! `label:rot` `flag:markiert` `ai:gut` `name:`, `person:` `place:` (keywords below "Orte"/"Places") `album:`
//! `month:7` `day:15` `weekday:sa` `time:abend`, plus `no:keyword` / `has:keyword` and `no:person` / `has:person`.
//! Every field also has a short form and second names (former German names, other languages), see
//! [`crate::search_fields`]; inside, only the English base form is used.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rusqlite::params;

use crate::db::{Db, Row};

// ───────────────────────────────────────────── Normalizing

/// Lower case, umlauts folded (ä → a), ß → ss, other accents removed.
pub fn fold_text(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::with_capacity(s.len());
    for c in s.nfd() {
        match c {
            'ß' => out.push_str("ss"),
            '\u{300}'..='\u{36f}' => {}
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Very simple German stem (on the folded word): strip common endings, stem ≥ 4 characters.
pub fn root(w: &str) -> String {
    for suffix in ["ungen", "heiten", "keiten", "ung", "heit", "keit", "ern", "en", "er", "es", "em", "e", "n", "s"] {
        if let Some(st) = w.strip_suffix(suffix) {
            if st.chars().count() >= 4 {
                return st.to_string();
            }
        }
    }
    w.to_string()
}

/// Words (letters/digits) of a text, folded.
pub fn words(s: &str) -> Vec<String> {
    fold_text(s).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn fts_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

// ───────────────────────────────────────────── Index

/// Texts of the index: image → all words (file name, folder, camera, …, keywords with parent terms, collections).
/// `only`: only this image (for the small update after a change), otherwise all.
fn index_texts(c: &rusqlite::Connection, only: Option<i64>) -> Result<HashMap<i64, String>, String> {
    let f = |e: rusqlite::Error| e.to_string();
    // `only` is a number, not text: safe to put into the statement (and the primary key is used)
    let (w_image, w_bs, w_sb) = match only {
        Some(i) => (format!(" WHERE b.id = {i}"), format!(" WHERE image = {i}"), format!(" WHERE sb.image = {i}")),
        None => (String::new(), String::new(), String::new()),
    };
    // keywords with all parent terms
    let sw: HashMap<i64, (String, Option<i64>)> = {
        let mut st = c.prepare("SELECT id, name, parent FROM keyword").map_err(f)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?)))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        r
    };
    let chain = |mut id: i64| -> String {
        let mut v = Vec::new();
        for _ in 0..20 {
            let Some((n, e)) = sw.get(&id) else { break };
            v.push(n.as_str());
            match e {
                Some(p) => id = *p,
                None => break,
            }
        }
        v.join(" ")
    };
    let mut texts: HashMap<i64, String> = HashMap::new();
    {
        let mut st = c
            .prepare(&format!(
                "SELECT b.id, d.name, st.name || ' ' || o.path, coalesce(d.camera,''), coalesce(d.lens,''), coalesce(b.caption,''), coalesce(b.copy,''), d.format,
                        coalesce(b.lr_text,'') || ' ' || coalesce(d.textmeta,''),
                        coalesce((SELECT group_concat(coalesce(k.keywords,'') || ' ' || coalesce(k.scene,'') || ' ' || coalesce(k.reason,''), ' ') FROM ai k WHERE k.file = d.id), '')
                 FROM image b JOIN file d ON d.id=b.file JOIN folder o ON o.id=d.folder JOIN root_folder st ON st.id=o.root{w_image}"
            ))
            .map_err(f)?;
        let mut rows = st.query([]).map_err(f)?;
        while let Some(r) = rows.next().map_err(f)? {
            let mut t = String::new();
            for i in 1..10 {
                let v: String = r.get(i).map_err(f)?;
                t.push_str(&v);
                t.push(' ');
            }
            texts.insert(r.get(0).map_err(f)?, t);
        }
    }
    {
        let mut st = c.prepare(&format!("SELECT image, keyword FROM image_keyword{w_bs}")).map_err(f)?;
        let mut rows = st.query([]).map_err(f)?;
        let mut cache: HashMap<i64, String> = HashMap::new();
        while let Some(r) = rows.next().map_err(f)? {
            let (b, s): (i64, i64) = (r.get(0).map_err(f)?, r.get(1).map_err(f)?);
            let k = cache.entry(s).or_insert_with(|| chain(s)).clone();
            if let Some(t) = texts.get_mut(&b) {
                t.push_str(&k);
                t.push(' ');
            }
        }
    }
    {
        let mut st = c.prepare(&format!("SELECT sb.image, s.name FROM collection_image sb JOIN collection s ON s.id=sb.collection{w_sb}")).map_err(f)?;
        let mut rows = st.query([]).map_err(f)?;
        while let Some(r) = rows.next().map_err(f)? {
            let (b, n): (i64, String) = (r.get(0).map_err(f)?, r.get(1).map_err(f)?);
            if let Some(t) = texts.get_mut(&b) {
                t.push_str(&n);
                t.push(' ');
            }
        }
    }
    Ok(texts)
}

/// The two index columns of a text: stems (`search_word`) and folded words (`search_trigram`). Every word stays in its
/// place: phrases ("Christa M") are searched as words in sequence, so words must not be dropped when they occur
/// earlier in the text (formerly a single "t" vanished this way and the phrase found nothing).
fn index_columns(t: &str) -> (String, String) {
    let ws = words(t);
    (ws.iter().map(|x| root(x)).collect::<Vec<_>>().join(" "), ws.join(" "))
}

/// Rebuild the full-text index completely. Returns: (documents, milliseconds).
pub fn build_index(db: &Db) -> Result<(usize, u128), String> {
    let f = |e: rusqlite::Error| e.to_string();
    let t = Instant::now();
    let c = &db.conn;
    let texts = index_texts(c, None)?;
    let tx = c.unchecked_transaction().map_err(f)?;
    tx.execute("DELETE FROM search_word", []).map_err(f)?;
    tx.execute("DELETE FROM search_trigram", []).map_err(f)?;
    {
        let mut w = tx.prepare("INSERT INTO search_word(rowid, text) VALUES(?1, ?2)").map_err(f)?;
        let mut tr = tx.prepare("INSERT INTO search_trigram(rowid, text) VALUES(?1, ?2)").map_err(f)?;
        for (b, t) in &texts {
            let (stems, raw) = index_columns(t);
            w.execute(params![b, stems]).map_err(f)?;
            tr.execute(params![b, raw]).map_err(f)?;
        }
    }
    tx.execute("INSERT INTO search_word(search_word) VALUES('optimize')", []).map_err(f)?;
    tx.execute("INSERT INTO search_trigram(search_trigram) VALUES('optimize')", []).map_err(f)?;
    tx.commit().map_err(f)?;
    let _ = db.set_info("search_index", &crate::now().to_string());
    Ok((texts.len(), t.elapsed().as_millis()))
}

/// Bring the index entry of one image up to date (after a change of its keywords or people) instead of rebuilding
/// everything. Does nothing as long as there is no index (it is built in full the first time). Returns whether it wrote.
pub fn update_image(db: &Db, image: i64) -> Result<bool, String> {
    let f = |e: rusqlite::Error| e.to_string();
    if db.info("search_index").is_none() {
        return Ok(false);
    }
    let texts = index_texts(&db.conn, Some(image))?;
    // a savepoint also works inside the transaction of the caller (blocks of the Apple Photos import)
    let c = &db.conn;
    c.execute_batch("SAVEPOINT suche_bild").map_err(f)?;
    let r: Result<(), String> = (|| {
        c.execute("DELETE FROM search_word WHERE rowid = ?1", [image]).map_err(f)?;
        c.execute("DELETE FROM search_trigram WHERE rowid = ?1", [image]).map_err(f)?;
        if let Some(t) = texts.get(&image) {
            let (stems, raw) = index_columns(t);
            c.execute("INSERT INTO search_word(rowid, text) VALUES(?1, ?2)", params![image, stems]).map_err(f)?;
            c.execute("INSERT INTO search_trigram(rowid, text) VALUES(?1, ?2)", params![image, raw]).map_err(f)?;
        }
        Ok(())
    })();
    match r {
        Ok(()) => c.execute_batch("RELEASE suche_bild").map_err(f)?,
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK TO suche_bild; RELEASE suche_bild");
            return Err(e);
        }
    }
    Ok(true)
}

// ───────────────────────────────────────────── Query language

#[derive(Clone, Debug, PartialEq)]
pub enum Cmp {
    /// contains (folded)
    Text(String),
    /// exactly this value (folded), written `field:="value"`; only for the text fields of [`EXACT_FIELDS`]
    Exact(String),
    Number(&'static str, f64),
    Between(f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// free word; `phrase`: in quotes; `prefix`: with *
    Word { text: String, phrase: bool, prefix: bool },
    Field(String, Cmp),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub negated: bool,
    pub kind: Kind,
}

/// Query: AND over clauses, each clause OR over terms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub clauses: Vec<Vec<Term>>,
}

/// Base form of a field name (any name of the field); unknown names stay as they are.
fn canon(field: &str) -> &str {
    crate::search_fields::field_id(field).unwrap_or(field)
}

/// ISO weekday (1 Monday … 7 Sunday) of the capture date.
pub fn weekday_of(z: &Row) -> Option<u8> {
    let a = z.capture.as_deref()?;
    let (y, m, d): (i32, i32, i32) = (a.get(0..4)?.parse().ok()?, a.get(5..7)?.parse().ok()?, a.get(8..10)?.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Sakamoto: 0 = Sunday
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let w = (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d).rem_euclid(7);
    Some(if w == 0 { 7 } else { w as u8 })
}

/// Hour of the capture time (0–23).
pub fn hour_of(z: &Row) -> Option<u8> {
    z.capture.as_deref().and_then(|a| a.get(11..13)).and_then(|h| h.parse().ok()).filter(|h| *h < 24)
}

/// Names of keyword roots that hold places ("Orte > Italien > Rom"): their descendants count as places (`ort:`).
pub fn is_place_root(name: &str) -> bool {
    matches!(fold_text(name.trim()).as_str(), "orte" | "ort" | "places" | "place" | "locations" | "location" | "lieux" | "lugares" | "luoghi" | "場所")
}

/// IDs of all keywords below a place root (not the roots themselves).
pub fn place_keywords(db: &Db) -> HashSet<i64> {
    let mut parent: HashMap<i64, (Option<i64>, String)> = HashMap::new();
    if let Ok(mut st) = db.conn.prepare_cached("SELECT id, parent, name FROM keyword") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?))) {
            for (id, p, n) in rows.flatten() {
                parent.insert(id, (p, n));
            }
        }
    }
    places_in(&parent)
}

/// Keywords (id → (parent, name)) below a place root.
pub fn places_in(parent: &HashMap<i64, (Option<i64>, String)>) -> HashSet<i64> {
    let mut out = HashSet::new();
    for &id in parent.keys() {
        let mut cur = parent.get(&id).and_then(|x| x.0);
        let mut steps = 0;
        while let Some(p) = cur {
            if parent.get(&p).is_some_and(|x| is_place_root(&x.1)) {
                out.insert(id);
                break;
            }
            cur = parent.get(&p).and_then(|x| x.0);
            steps += 1;
            if steps > 64 {
                break;
            }
        }
    }
    out
}

/// Age "<2d", "<10w", ">3m", "<12h" → (younger than?, seconds).
fn old_state(t: &str) -> Option<(bool, i64)> {
    let (newer, r) = if let Some(r) = t.strip_prefix('<') { (true, r) } else if let Some(r) = t.strip_prefix('>') { (false, r) } else { (true, t) };
    let r = r.trim_start_matches('=');
    let (number, unit) = r.split_at(r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len()));
    let n: i64 = number.parse().ok()?;
    let s = match unit {
        "h" => 3600,
        "d" | "" => 86400,
        "w" => 7 * 86400,
        "m" => 30 * 86400,
        "y" | "j" => 365 * 86400,
        _ => return None,
    };
    Some((newer, n * s))
}

fn read_number(s: &str, field: &str) -> Option<f64> {
    if field == "weekday" {
        return crate::search_fields::weekday_number(s).map(f64::from);
    }
    let s = s.trim().trim_start_matches("f/").trim_end_matches("mm").trim_end_matches('s').replace(',', ".");
    if field == "shutter" {
        if let Some((a, b)) = s.split_once('/') {
            return Some(a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?);
        }
    }
    s.parse().ok()
}

fn compare(field: &str, value: &str) -> Cmp {
    if field == "weekday" && crate::search_fields::is_weekend(value) {
        return Cmp::Number(">=", 6.0);
    }
    // time of day: a name ("abend") or hours ("6-9", ">=20")
    let numeric = matches!(field, "aperture" | "focal" | "iso" | "shutter" | "year" | "month" | "day" | "rating" | "weekday")
        || (field == "time" && crate::search_fields::time_of_day_named(value).is_none());
    if numeric {
        for op in ["<=", ">=", "<", ">", "="] {
            if let Some(r) = value.strip_prefix(op) {
                if let Some(z) = read_number(r, field) {
                    return Cmp::Number(op, z);
                }
            }
        }
        // range a-b (for shutter speed "1/250-1/60")
        if let Some((a, b)) = value.split_once('-').filter(|(a, _)| !a.is_empty()) {
            if let (Some(a), Some(b)) = (read_number(a, field), read_number(b, field)) {
                return Cmp::Between(a.min(b), a.max(b));
            }
        }
        if let Some(z) = read_number(value, field) {
            return Cmp::Number("=", z);
        }
    }
    if field == "date" {
        for op in ["<=", ">=", "<", ">"] {
            if let Some(r) = value.strip_prefix(op) {
                return Cmp::Text(format!("{op}{r}"));
            }
        }
    }
    // exact comparison of a text field: `camera:="Canon EOS R"` (the quotes only keep the spaces together)
    if EXACT_FIELDS.contains(&field) {
        if let Some(r) = value.strip_prefix('=') {
            return Cmp::Exact(fold_text(r.trim()));
        }
    }
    Cmp::Text(fold_text(value))
}

/// Parse text into a query.
pub fn parse(input: &str) -> Query {
    // raw tokens: keep quoted parts together
    // (text, was quoted, quoted from the start: a literal like "-x" or "kamera:x", never a field or a NOT)
    let mut toks: Vec<(String, bool, bool)> = Vec::new();
    let mut cur = String::new();
    let mut quote = false;
    let mut had_quote = false;
    let mut literal = false;
    let mut started = false;
    for ch in input.chars() {
        match ch {
            '"' => {
                if !started {
                    literal = true;
                }
                started = true;
                quote = !quote;
                had_quote = true;
            }
            c if c.is_whitespace() && !quote => {
                if !cur.is_empty() || had_quote {
                    toks.push((std::mem::take(&mut cur), had_quote, literal));
                }
                had_quote = false;
                literal = false;
                started = false;
            }
            // full-width colon (Japanese input) counts as a colon
            '：' => {
                started = true;
                cur.push(':');
            }
            c => {
                started = true;
                cur.push(c);
            }
        }
    }
    if !cur.is_empty() || had_quote {
        toks.push((cur, had_quote, literal));
    }
    let mut a = Query::default();
    let mut or_follows = false;
    for (t, q, literal) in toks {
        if !q && (t == "|" || t == "ODER" || t == "OR") {
            or_follows = true;
            continue;
        }
        let (negated, rest) = match t.strip_prefix('-') {
            Some(r) if !r.is_empty() && !literal => (true, r.to_string()),
            _ => (false, t.clone()),
        };
        // a|b without spaces
        let parts: Vec<String> = if q { vec![rest] } else { rest.split('|').filter(|x| !x.is_empty()).map(str::to_string).collect() };
        let mut terms = Vec::new();
        for part in parts {
            let field = if literal { None } else { part.split_once(':').and_then(|(k, v)| Some((crate::search_fields::field_id(k)?, v))) };
            let kind = match field {
                // a field without value (`camera:` or `camera:""` while typing) is no condition yet
                Some((_, v)) if v.trim().is_empty() => continue,
                Some((k, v)) => Kind::Field(k.to_string(), compare(k, v.trim())),
                None if part.is_empty() => continue,
                None => {
                    let prefix = part.ends_with('*');
                    Kind::Word { text: part.trim_end_matches('*').to_string(), phrase: q && part.contains(' '), prefix }
                }
            };
            terms.push(Term { negated, kind });
        }
        if terms.is_empty() {
            continue;
        }
        if or_follows && !a.clauses.is_empty() {
            a.clauses.last_mut().unwrap().extend(terms);
        } else {
            a.clauses.push(terms);
        }
        or_follows = false;
    }
    a
}

// ───────────────────────────────────────────── Evaluation

/// Image IDs for a free search word (full text).
pub fn word_matches(db: &Db, text: &str, phrase: bool, prefix: bool) -> HashSet<i64> {
    let ws = words(text);
    let mut out = HashSet::new();
    if ws.is_empty() {
        return out;
    }
    let query = |tab: &str, m: &str, out: &mut HashSet<i64>| {
        if let Ok(mut st) = db.conn.prepare_cached(&format!("SELECT rowid FROM {tab} WHERE {tab} MATCH ?1")) {
            if let Ok(rows) = st.query_map([m], |r| r.get::<_, i64>(0)) {
                out.extend(rows.flatten());
            }
        }
    };
    if phrase || ws.len() > 1 {
        // phrase or compound word ("EF20-35mm"): stems in sequence
        let p = ws.iter().map(|w| root(w)).collect::<Vec<_>>().join(" ");
        query("search_word", &fts_quote(&p), &mut out);
        if !phrase {
            let raw_refs = ws.join(" ");
            if raw_refs.chars().count() >= 3 {
                query("search_trigram", &fts_quote(&raw_refs), &mut out);
            }
        }
        return out;
    }
    let w = &ws[0];
    let s = if prefix { w.clone() } else { root(w) };
    query("search_word", &format!("{}*", fts_quote(&s)), &mut out);
    if w.chars().count() >= 5 {
        query("search_trigram", &fts_quote(w), &mut out);
    }
    if out.is_empty() && w.chars().count() >= 5 {
        // typos: similar stems from the vocabulary
        // only real typos: one character off, two for very long words ("geschlecht" is not a typo of "Geschichte")
        let max = if w.chars().count() >= 12 { 2 } else { 1 };
        let near: Vec<String> = vocab(db).into_iter().filter(|v| v.len().abs_diff(s.len()) <= max && distance(v, &s) <= max).collect();
        for v in near.iter().take(20) {
            query("search_word", &fts_quote(v), &mut out);
        }
    }
    out
}

fn vocab(db: &Db) -> Vec<String> {
    let Ok(mut st) = db.conn.prepare_cached("SELECT term FROM search_vocab") else { return Vec::new() };
    st.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Edit distance (Levenshtein).
pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// What the evaluation needs per row (prepared by the caller).
pub struct Context<'a> {
    pub db: &'a Db,
    pub rows: &'a [Row],
    /// folder path per folder ID (absolute)
    pub folder_paths: &'a HashMap<i64, String>,
}

/// Exact value of a number field as the value lists show it: aperture and focal length to 0,1, ISO whole,
/// shutter speed as 1/n (from 1/3 s on) or in tenths of a second (negative key). Photos with the same key
/// are the same list row; the search `field:value` (single value) compares exactly this key.
pub fn value_key(field: &str, v: f64) -> i64 {
    match canon(field) {
        "aperture" | "focal" => (v * 10.0).round() as i64,
        "shutter" => {
            if v > 0.0 && 1.0 / v >= 2.75 {
                (1.0 / v).round() as i64
            } else {
                -((v * 10.0).round() as i64)
            }
        }
        _ => v.round() as i64,
    }
}

/// Seconds (for shutter speed), mm or f-number or ISO of a key of [`value_key`], for sorting and display.
pub fn key_value(field: &str, key: i64) -> f64 {
    match canon(field) {
        "aperture" | "focal" => key as f64 / 10.0,
        "shutter" => {
            if key > 0 {
                1.0 / key as f64
            } else {
                -key as f64 / 10.0
            }
        }
        _ => key as f64,
    }
}

/// Does a row satisfy a number field (`aperture`, `focal`, `iso`, `shutter`, `year`, `month`, `day`, `weekday`, `time`)
/// comparison? (for tests and callers outside; any name of the field)
pub fn number_matches(field: &str, cmp: &Cmp, z: &crate::db::Row) -> bool {
    let field = canon(field);
    match field {
        "aperture" => number_ok(z.aperture, cmp, field),
        "focal" => number_ok(z.focal, cmp, field),
        "iso" => number_ok(z.iso.map(|v| v as f64), cmp, field),
        "shutter" => number_ok(z.time, cmp, field),
        "year" => number_ok(z.year().map(|v| v as f64), cmp, field),
        "month" => number_ok(z.month().filter(|v| (1..=12).contains(v)).map(|v| v as f64), cmp, field),
        "day" => number_ok(z.day().map(|v| v as f64), cmp, field),
        "weekday" => number_ok(weekday_of(z).map(f64::from), cmp, field),
        "time" => match cmp {
            Cmp::Text(t) => match (crate::search_fields::time_of_day_named(t), hour_of(z)) {
                (Some(want), Some(h)) => crate::search_fields::time_of_day(h) == want,
                _ => false,
            },
            _ => number_ok(hour_of(z).map(f64::from), cmp, field),
        },
        _ => false,
    }
}

fn number_ok(v: Option<f64>, cmp: &Cmp, field: &str) -> bool {
    let Some(v) = v else { return false };
    // single value: exactly the list row (no tolerance that could mix up neighbouring values)
    if let Cmp::Number("=", z) = cmp {
        if matches!(field, "aperture" | "focal" | "iso" | "shutter") {
            return value_key(field, v) == value_key(field, *z);
        }
    }
    let eps = match field {
        "aperture" => 0.05,
        "shutter" => v * 0.02,
        _ => 0.5,
    };
    match cmp {
        Cmp::Number("<=", z) => v <= z + eps,
        Cmp::Number(">=", z) => v >= z - eps,
        Cmp::Number("<", z) => v < *z - eps,
        Cmp::Number(">", z) => v > *z + eps,
        Cmp::Number(_, z) => (v - z).abs() <= eps,
        Cmp::Between(a, b) => v >= a - eps && v <= b + eps,
        Cmp::Text(_) | Cmp::Exact(_) => false,
    }
}

/// Folded, letters and digits only ("DP2M_5436.X3F" → "dp2m5436x3f").
fn only_alnum(s: &str) -> String {
    fold_text(s).chars().filter(|c| c.is_alphanumeric()).collect()
}

fn text_ok(v: Option<&str>, cmp: &Cmp) -> bool {
    match (v, cmp) {
        (Some(v), Cmp::Text(t)) => fold_text(v).contains(t.as_str()),
        (Some(v), Cmp::Exact(t)) => fold_text(v) == *t,
        _ => false,
    }
}

/// Name test of the keyword, person, place and album fields: contains (`exact` false) or equals (folded).
fn name_ok(name: &str, t: &str, exact: bool) -> bool {
    let n = fold_text(name);
    if exact { n == t } else { n.contains(t) }
}

/// Text fields that know the exact comparison `field:="value"` (the others only know "contains").
pub const EXACT_FIELDS: [&str; 9] = ["camera", "lens", "format", "keyword", "person", "place", "album", "folder", "name"];

/// The value of a text comparison and whether it has to match exactly.
fn text_of(cmp: &Cmp) -> Option<(&str, bool)> {
    match cmp {
        Cmp::Text(t) => Some((t, false)),
        Cmp::Exact(t) => Some((t, true)),
        _ => None,
    }
}

/// Images with a confirmed face of a person whose name contains `t` (folded), or with that person as keyword
/// (people from Lightroom are keywords marked as person).
fn person_images(db: &Db, t: &str, exact: bool) -> HashSet<i64> {
    // the few people first (names folded once), then only their photos (formerly every assignment of
    // every person was read and folded, for each search word)
    let mut ids: Vec<i64> = Vec::new();
    if let Ok(mut st) = db.conn.prepare_cached("SELECT id, name FROM keyword WHERE person = 1") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            ids = rows.flatten().filter(|(_, n)| name_ok(n, t, exact)).map(|(i, _)| i).collect();
        }
    }
    let mut out = HashSet::new();
    for id in ids {
        for sql in ["SELECT image FROM image_keyword WHERE keyword = ?1", "SELECT image FROM face WHERE person = ?1 AND status = 'confirmed'"] {
            if let Ok(mut st) = db.conn.prepare_cached(sql) {
                if let Ok(rows) = st.query_map([id], |r| r.get::<_, i64>(0)) {
                    out.extend(rows.flatten());
                }
            }
        }
    }
    out
}

/// Images with a keyword (name contains `t`, folded) or one below it.
fn keyword_images(db: &Db, t: &str, exact: bool) -> HashSet<i64> {
    let mut ids: Vec<i64> = Vec::new();
    if let Ok(mut st) = db.conn.prepare_cached("SELECT id, name FROM keyword") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            ids = rows.flatten().filter(|(_, n)| name_ok(n, t, exact)).map(|(i, _)| i).collect();
        }
    }
    let mut out = HashSet::new();
    for id in ids {
        if let Ok(v) = db.images_with_keyword(id) {
            out.extend(v);
        }
    }
    out
}

/// Images with a place keyword (name contains `t`, folded) or one below it.
fn place_images(db: &Db, t: &str, exact: bool) -> HashSet<i64> {
    let places = place_keywords(db);
    let mut out = HashSet::new();
    if let Ok(mut st) = db.conn.prepare_cached("SELECT id, name FROM keyword") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for (id, _) in rows.flatten().filter(|(id, n)| places.contains(id) && name_ok(n, t, exact)) {
                if let Ok(v) = db.images_with_keyword(id) {
                    out.extend(v);
                }
            }
        }
    }
    out
}

/// Images in a collection whose name contains `t` (folded).
fn collection_images(db: &Db, t: &str, exact: bool) -> HashSet<i64> {
    let mut out = HashSet::new();
    if let Ok(mut st) = db.conn.prepare_cached("SELECT sb.image, s.name FROM collection_image sb JOIN collection s ON s.id = sb.collection") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for (image, name) in rows.flatten() {
                if name_ok(&name, t, exact) {
                    out.insert(image);
                }
            }
        }
    }
    out
}

fn images_with_people(db: &Db) -> HashSet<i64> {
    let Ok(mut st) = db.conn.prepare_cached("SELECT DISTINCT image FROM face WHERE status='confirmed' AND person IS NOT NULL") else { return HashSet::new() };
    st.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()).unwrap_or_default()
}

fn images_with_keywords(db: &Db) -> HashSet<i64> {
    let Ok(mut st) = db.conn.prepare_cached("SELECT DISTINCT image FROM image_keyword") else { return HashSet::new() };
    st.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Hits of a term (without NOT) as a bit field per row.
fn eval_term(k: &Context, kind: &Kind, index: &HashMap<i64, usize>) -> Vec<bool> {
    let n = k.rows.len();
    let from_set = |m: HashSet<i64>| {
        let mut v = vec![false; n];
        for b in m {
            if let Some(&i) = index.get(&b) {
                v[i] = true;
            }
        }
        v
    };
    match kind {
        Kind::Word { text, phrase, prefix } => {
            let mut v = from_set(word_matches(k.db, text, *phrase, *prefix));
            // plus the photos with a confirmed face of a person with this name: live, the full-text index may not know
            // a person confirmed a moment ago ("Christa", "Christa M.")
            let folded = fold_text(text);
            let folded = folded.trim();
            if folded.chars().count() >= 3 {
                for b in person_images(k.db, folded, false) {
                    if let Some(&i) = index.get(&b) {
                        v[i] = true;
                    }
                }
            }
            // file name: substring regardless of _ - . ("5436" and "DP2M_5436" find DP2M5436.X3F)
            let q = only_alnum(text);
            if q.chars().count() >= 3 {
                for (i, z) in k.rows.iter().enumerate() {
                    if !v[i] && (only_alnum(&z.name).contains(&q) || z.copy.as_deref().is_some_and(|c| only_alnum(c).contains(&q))) {
                        v[i] = true;
                    }
                }
            }
            v
        }
        Kind::Field(f, cmp) => match f.as_str() {
            "keyword" => match text_of(cmp) {
                Some((t, exact)) => from_set(keyword_images(k.db, t, exact)),
                None => vec![false; n],
            },
            "person" => match text_of(cmp) {
                // photos with a confirmed face of this person (name contains … / equals … with `person:="…"`)
                Some((t, exact)) => from_set(person_images(k.db, t, exact)),
                None => vec![false; n],
            },
            "place" => match text_of(cmp) {
                Some((t, exact)) => from_set(place_images(k.db, t, exact)),
                None => vec![false; n],
            },
            "album" => match text_of(cmp) {
                Some((t, exact)) => from_set(collection_images(k.db, t, exact)),
                None => vec![false; n],
            },
            "no" | "has" => {
                // "has:person" / "no:person": photos with/without a named (confirmed) face; any name of the person
                // field counts ("personne", "人物" …)
                let person = matches!(cmp, Cmp::Text(t) if crate::search::fold_text(t).starts_with("person") || crate::search_fields::field_id(t) == Some("person"));
                let m = if person { images_with_people(k.db) } else { images_with_keywords(k.db) };
                let v = from_set(m);
                if f == "no" {
                    v.into_iter().map(|x| !x).collect()
                } else {
                    v
                }
            }
            _ => k
                .rows
                .iter()
                .map(|z| match f.as_str() {
                    "lens" => text_ok(z.lens.as_deref(), cmp),
                    "camera" => text_ok(z.camera.as_deref(), cmp),
                    "format" => text_ok(Some(z.format.as_str()), cmp),
                    "name" => text_ok(Some(&z.name), cmp) || text_ok(z.copy.as_deref(), cmp),
                    "folder" => text_ok(k.folder_paths.get(&z.folder).map(String::as_str), cmp),
                    "aperture" | "focal" | "iso" | "shutter" | "year" | "month" | "day" | "weekday" | "time" => number_matches(f, cmp, z),
                    "rating" => number_ok(Some(z.stars as f64), cmp, "rating"),
                    "date" => match (z.capture.as_deref(), cmp) {
                        (Some(a), Cmp::Text(t)) => {
                            for op in ["<=", ">=", "<", ">"] {
                                if let Some(d) = t.strip_prefix(op) {
                                    let a = &a[..d.len().min(a.len())];
                                    return match op {
                                        "<=" => a <= d,
                                        ">=" => a >= d,
                                        "<" => a < d,
                                        _ => a > d,
                                    };
                                }
                            }
                            a.starts_with(t.as_str())
                        }
                        _ => false,
                    },
                    "label" => match cmp {
                        Cmp::Text(t) => z.label == label_value(t).unwrap_or(0),
                        _ => false,
                    },
                    "edited" | "imported" => match cmp {
                        Cmp::Text(t) => {
                            let value = if f == "edited" { z.changed } else { z.imported };
                            match (old_state(t), value) {
                                (Some((newer, s)), Some(w)) => {
                                    let limit = crate::now() - s;
                                    if newer {
                                        w >= limit
                                    } else {
                                        w < limit
                                    }
                                }
                                _ => false,
                            }
                        }
                        _ => false,
                    },
                    "flag" => match cmp {
                        // an unknown word counts as "unflagged" (as before)
                        Cmp::Text(t) => z.flag == flag_value(t).unwrap_or(0),
                        _ => false,
                    },
                    "ai" => match cmp {
                        Cmp::Text(t) => z.ai_quality.is_some_and(|q| q as i64 == crate::ai::quality(t).unwrap_or(-1)),
                        _ => false,
                    },
                    _ => false,
                })
                .collect(),
        },
    }
}

/// Colour label code (1 red … 5 purple, 0 none) of a folded value in any interface language.
pub fn label_value(t: &str) -> Option<u8> {
    Some(match fold_text(t.trim()).as_str() {
        "rot" | "red" | "rouge" | "rojo" | "roja" | "赤" => 1,
        "gelb" | "yellow" | "jaune" | "amarillo" | "amarilla" | "黄" => 2,
        "grun" | "gruen" | "green" | "vert" | "verte" | "verde" | "緑" => 3,
        "blau" | "blue" | "bleu" | "bleue" | "azul" | "青" => 4,
        "lila" | "violett" | "purple" | "violet" | "violette" | "morado" | "morada" | "紫" => 5,
        "ohne" | "none" | "keine" | "aucune" | "aucun" | "ninguna" | "ninguno" | "なし" => 0,
        _ => return None,
    })
}

/// Flag (1 picked, −1 rejected, 0 none) of a value in any interface language.
pub fn flag_value(t: &str) -> Option<i8> {
    const NAMES: [(i8, &[&str]); 3] = [
        (1, &["markiert", "ja", "picked", "flagged", "yes", "marque", "marquee", "retenu", "retenue", "oui", "marcado", "marcada", "si", "フラグ付き", "フラグあり"]),
        (-1, &["abgelehnt", "rejected", "rejete", "rejetee", "rechazado", "rechazada", "除外"]),
        (0, &["ohne", "nein", "none", "unflagged", "no", "non", "aucun", "aucune", "sans", "ninguna", "ninguno", "sin", "なし"]),
    ];
    // folded on both sides: the folding splits Japanese voiced marks (グ → ク + ゙)
    let w = fold_text(t.trim());
    NAMES.iter().find(|(_, names)| names.iter().any(|n| fold_text(n) == w)).map(|(v, _)| *v)
}

/// Does the parser understand the value of a field term (a number where a number is expected, a known word for
/// label, flag, AI quality, time of day …)? For the help texts and their test: every example must be understood.
pub fn value_understood(field: &str, cmp: &Cmp) -> bool {
    use crate::search_fields::{field_def, FieldKind};
    let Some(def) = field_def(field) else { return false };
    match (def.id, cmp) {
        (_, Cmp::Number(..) | Cmp::Between(..)) => matches!(def.kind, FieldKind::Number) || def.id == "time",
        ("time", Cmp::Text(t)) => crate::search_fields::time_of_day_named(t).is_some(),
        ("label", Cmp::Text(t)) => label_value(t).is_some(),
        ("flag", Cmp::Text(t)) => flag_value(t).is_some(),
        ("ai", Cmp::Text(t)) => crate::ai::quality(t).is_some(),
        ("has" | "no", Cmp::Text(t)) => !t.is_empty(),
        ("edited" | "imported", Cmp::Text(t)) => old_state(t).is_some(),
        ("date", Cmp::Text(t)) => t.trim_start_matches(['<', '>', '=']).starts_with(|c: char| c.is_ascii_digit()),
        (_, Cmp::Text(t) | Cmp::Exact(t)) => matches!(def.kind, FieldKind::Text) && !t.is_empty(),
    }
}

/// Evaluate a query: bit field per row (order as in `k.rows`). Empty query = everything.
pub fn evaluate(k: &Context, a: &Query) -> Vec<bool> {
    evaluate_with(k, a, &|| false).unwrap_or_default()
}

/// Like [`evaluate`], but stops between the terms as soon as `cancelled` says so (background search:
/// an outdated query is not computed to the end). `None`: cancelled.
pub fn evaluate_with(k: &Context, a: &Query, cancelled: &dyn Fn() -> bool) -> Option<Vec<bool>> {
    let n = k.rows.len();
    let mut result = vec![true; n];
    // filler words ("an", "der", "mit" …) do not count as a condition
    let clauses: Vec<&Vec<Term>> = a.clauses.iter().filter(|clause| !(clause.len() == 1 && !clause[0].negated && matches!(&clause[0].kind, Kind::Word { text, phrase: false, .. } if STOP_WORDS.contains(&fold_text(text).as_str())))).collect();
    if clauses.is_empty() {
        return Some(if a.clauses.is_empty() { result } else { vec![false; n] });
    }
    let index: HashMap<i64, usize> = k.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
    let mut per_clause: Vec<(bool, Vec<bool>)> = Vec::new();
    for clause in &clauses {
        let mut or_clause = vec![false; n];
        for t in clause.iter() {
            if cancelled() {
                return None;
            }
            let v = eval_term(k, &t.kind, &index);
            for i in 0..n {
                or_clause[i] |= v[i] != t.negated;
            }
        }
        let word = clause.len() == 1 && !clause[0].negated && matches!(clause[0].kind, Kind::Word { phrase: false, .. });
        for i in 0..n {
            result[i] &= or_clause[i];
        }
        per_clause.push((word, or_clause));
    }
    // no hit with ≥ 3 free words: one may be missing (like "at least n−1" in search engines)
    let words = per_clause.iter().filter(|(w, _)| *w).count();
    if words >= 3 && !result.iter().any(|x| *x) {
        for i in 0..n {
            let missing_word = per_clause.iter().filter(|(w, v)| *w && !v[i]).count();
            let other_ok = per_clause.iter().filter(|(w, _)| !*w).all(|(_, v)| v[i]);
            result[i] = other_ok && missing_word <= 1;
        }
    }
    // typed name in several words ("Christa M."): the words are searched one by one above; additionally the photos
    // of a person whose name contains the whole input (live from the confirmed faces)
    let plain: Vec<&str> = clauses
        .iter()
        .filter_map(|c| match c.as_slice() {
            [Term { negated: false, kind: Kind::Word { text, phrase: false, prefix: false } }] => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if plain.len() == clauses.len() && plain.len() >= 2 {
        let whole = fold_text(&plain.join(" "));
        if whole.chars().count() >= 3 {
            for b in person_images(k.db, whole.trim(), false) {
                if let Some(&i) = index.get(&b) {
                    result[i] = true;
                }
            }
        }
    }
    Some(result)
}

/// Filler words that are ignored as free search words (folded).
const STOP_WORDS: [&str; 34] = [
    "an", "am", "auf", "aus", "bei", "das", "dem", "den", "der", "des", "die", "ein", "eine", "einem", "einen", "einer", "fur", "im", "in", "ist", "mit", "nach", "oder",
    "und", "uber", "unter", "vom", "von", "vor", "zu", "zum", "zur", "the", "of",
];

/// Free search words (for highlighting).
pub fn free_words(a: &Query) -> Vec<String> {
    a.clauses
        .iter()
        .flatten()
        .filter(|t| !t.negated)
        .filter_map(|t| match &t.kind {
            Kind::Word { text, .. } => Some(text.clone()),
            _ => None,
        })
        .flat_map(|t| words(&t))
        .collect()
}

/// Positions (byte ranges) in `text` that match the free search words.
pub fn highlight(text: &str, search_words: &[String]) -> Vec<(usize, usize)> {
    if search_words.is_empty() {
        return Vec::new();
    }
    let search_words: Vec<String> = search_words.iter().filter(|w| !STOP_WORDS.contains(&w.as_str())).cloned().collect();
    let stems: Vec<String> = search_words.iter().map(|w| root(w)).collect();
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let check = |a: usize, e: usize, out: &mut Vec<(usize, usize)>| {
        let w = fold_text(&text[a..e]);
        let st = root(&w);
        let ok = stems.iter().any(|s| st.starts_with(s.as_str()) || w.starts_with(s.as_str())) || search_words.iter().any(|s| s.chars().count() >= 5 && w.contains(s.as_str()));
        if ok {
            out.push((a, e));
        }
    };
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(a) = start.take() {
            check(a, i, &mut out);
        }
    }
    if let Some(a) = start {
        check(a, text.len(), &mut out);
    }
    out
}

// ───────────────────────────────────────────── Lightroom smart collections

/// Translate the rules of a Lightroom smart collection (Lua text) into the own search syntax.
/// `None` if a rule cannot be translated (e.g. edit time, plug-in fields).
pub fn lightroom_rules(lua: &str) -> Option<String> {
    let field = |block: &str, name: &str| -> Option<String> {
        let i = block.find(&format!("{name} = "))?;
        let r = &block[i + name.len() + 3..];
        let r = r.trim_start();
        if let Some(q) = r.strip_prefix('"') {
            Some(q[..q.find('"')?].to_string())
        } else {
            Some(r[..r.find([',', '\n', '}'])?].trim().to_string())
        }
    };
    let mut parts = Vec::new();
    let mut rest = lua;
    while let Some(i) = rest.find("criteria = ") {
        let block_end = rest[i..].find('}').map_or(rest.len(), |e| i + e);
        let block = &rest[i..block_end];
        rest = &rest[block_end..];
        let criteria = field(block, "criteria")?;
        let op = field(block, "operation").unwrap_or_default();
        let value = field(block, "value").unwrap_or_default();
        let value2 = field(block, "value2").unwrap_or_default();
        let q = |v: &str| if v.contains(' ') { format!("\"{v}\"") } else { v.to_string() };
        let cmp_op = |op: &str| match op {
            "==" | "=" => Some(""),
            ">=" => Some(">="),
            "<=" => Some("<="),
            ">" => Some(">"),
            "<" => Some("<"),
            _ => None,
        };
        let t = match criteria.as_str() {
            "rating" => format!("rating:{}{}", cmp_op(&op)?, value),
            "pick" => format!("flag:{}", if value.starts_with('1') { "markiert" } else if value.starts_with('-') { "abgelehnt" } else { "ohne" }),
            "keywords" => match op.as_str() {
                "empty" => "no:keyword".into(),
                "notEmpty" => "has:keyword".into(),
                "any" | "all" | "words" | "==" => value.split_whitespace().map(|w| format!("keyword:{}", q(w))).collect::<Vec<_>>().join(" "),
                "noneOf" => value.split_whitespace().map(|w| format!("-keyword:{}", q(w))).collect::<Vec<_>>().join(" "),
                _ => return None,
            },
            "fileFormat" => format!("format:{value}"),
            "camera" if matches!(op.as_str(), "==" | "any" | "all" | "words") => format!("camera:{}", q(&value)),
            "lens" if matches!(op.as_str(), "==" | "any" | "all" | "words") => format!("lens:{}", q(&value)),
            "filename" if matches!(op.as_str(), "any" | "all" | "words" | "==") => format!("name:{}", q(&value)),
            "captureTime" => match op.as_str() {
                "in" if !value2.is_empty() => format!("date:>={value} date:<={value2}"),
                o => format!("date:{}{value}", cmp_op(o)?),
            },
            "labelColor" | "colorLabel" => format!("label:{value}"),
            "aperture" => format!("aperture:{}{}", cmp_op(&op)?, value),
            // modification time in Lightroom (touchTime) or afterwards our own modification time
            "touchTime" if op == "inLast" => {
                let unit = match field(block, "value_units").as_deref() {
                    Some("hours") => "h",
                    Some("days") | None => "d",
                    Some("weeks") => "w",
                    Some("months") => "m",
                    Some("years") => "y",
                    _ => return None,
                };
                format!("edited:<{value}{unit}")
            }
            "isoSpeedRating" => format!("iso:{}{}", cmp_op(&op)?, value),
            "focalLength" => format!("focal:{}{}", cmp_op(&op)?, value),
            _ => return None,
        };
        parts.push(t);
    }
    if parts.is_empty() {
        return None;
    }
    let sep = if lua.contains("combine = \"union\"") { " | " } else { " " };
    Some(parts.join(sep))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems() {
        assert_eq!(root(&fold_text("Wände")), "wand");
        assert_eq!(root(&fold_text("Wänden")), "wand");
        assert_eq!(root(&fold_text("Wand")), "wand");
        assert_eq!(root(&fold_text("Schimmel")), "schimmel");
        assert_eq!(fold_text("Straße"), "strasse");
        assert_eq!(distance("schimel", "schimmel"), 1);
    }

    #[test]
    fn syntax() {
        let a = parse("objektiv:20-35 blende:<=2.8 schimmel -auto \"alte wand\" a | b brennweite:20-35 zeit:<=1/60");
        assert_eq!(a.clauses.len(), 8);
        assert_eq!(a.clauses[0][0].kind, Kind::Field("lens".into(), Cmp::Text("20-35".into())));
        assert_eq!(a.clauses[1][0].kind, Kind::Field("aperture".into(), Cmp::Number("<=", 2.8)));
        assert!(a.clauses[3][0].negated);
        assert_eq!(a.clauses[4][0].kind, Kind::Word { text: "alte wand".into(), phrase: true, prefix: false });
        assert_eq!(a.clauses[5].len(), 2);
        assert_eq!(a.clauses[6][0].kind, Kind::Field("focal".into(), Cmp::Between(20.0, 35.0)));
        let z = parse("zeit:<=1/60");
        assert_eq!(z.clauses[0][0].kind, Kind::Field("shutter".into(), Cmp::Number("<=", 1.0 / 60.0)));
        assert_eq!(parse("wand ODER mauer").clauses.len(), 1);
    }

    /// A single value finds exactly its list row: neighbours are never mixed up, nothing is missed.
    #[test]
    fn single_value_exact() {
        let hit = |field: &str, q: &str, v: f64| -> bool {
            match &parse(q).clauses[0][0].kind {
                Kind::Field(f, c) => {
                    assert_eq!(f, canon(field));
                    number_ok(Some(v), c, canon(field))
                }
                _ => panic!("{q}"),
            }
        };
        // shutter speed: stored values are slightly off
        assert!(hit("zeit", "zeit:1/900", 1.0 / 900.0));
        assert!(hit("zeit", "zeit:1/900", 0.0011105));
        assert!(!hit("zeit", "zeit:1/900", 0.0011));
        assert!(!hit("zeit", "zeit:1/900", 1.0 / 800.0));
        assert!(!hit("zeit", "zeit:1/125", 1.0 / 100.0));
        assert!(hit("zeit", "zeit:1/125", 0.008));
        assert!(hit("zeit", "zeit:1/3", 0.3333));
        assert!(!hit("zeit", "zeit:1/3", 0.5));
        assert!(hit("zeit", "zeit:0,8", 0.8));
        assert!(!hit("zeit", "zeit:0,8", 1.0));
        assert!(hit("zeit", "zeit:1", 1.0));
        // focal length and aperture
        assert!(hit("brennweite", "brennweite:35", 35.0));
        assert!(!hit("brennweite", "brennweite:35", 35.5));
        assert!(hit("brennweite", "brennweite:35.5", 35.5));
        assert!(hit("blende", "blende:2", 2.0));
        assert!(!hit("blende", "blende:2", 2.2));
        assert!(hit("blende", "blende:2.8", 2.83));
        assert!(hit("iso", "iso:200", 200.0));
        assert!(!hit("iso", "iso:200", 250.0));
        // keys and back
        for (f, v) in [("zeit", 1.0 / 900.0), ("zeit", 0.8), ("brennweite", 35.5), ("blende", 2.8), ("iso", 3200.0)] {
            let k = value_key(f, v);
            assert_eq!(value_key(f, key_value(f, k)), k, "{f} {v}");
        }
        // old syntax (groups, several values) still parses and works
        assert_eq!(parse("iso:200 | iso:400").clauses[0].len(), 2);
        match &parse("brennweite:29-35").clauses[0][0].kind {
            Kind::Field(_, c) => assert!(number_ok(Some(30.0), c, "focal") && !number_ok(Some(50.0), c, "focal")),
            _ => panic!(),
        }
    }

    /// `person:` (face or person keyword), `ort:` (below "Orte"), `sammlung:`.
    #[test]
    fn person_place_collection_fields() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/x/', 'x', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, '');
                 INSERT INTO file(id, folder, name, format) VALUES(1, 1, 'a.jpg', 'JPG'), (2, 1, 'b.jpg', 'JPG'), (3, 1, 'c.jpg', 'JPG');
                 INSERT INTO image(id, file) VALUES(1, 1), (2, 2), (3, 3);
                 INSERT INTO keyword(id, name, parent, person) VALUES(1, 'Orte', NULL, 0), (2, 'Italien', 1, 0), (3, 'Rom', 2, 0), (4, 'Christa M.', NULL, 1), (5, 'Rom-Reise', NULL, 0);
                 INSERT INTO image_keyword(image, keyword) VALUES(1, 3), (2, 4), (3, 5);
                 INSERT INTO collection(id, name, kind) VALUES(1, 'Urlaub 2020', 0);
                 INSERT INTO collection_image(collection, image) VALUES(1, 3);",
            )
            .unwrap();
        let rows: Vec<Row> = (1..=3).map(|i| Row { image: i, file: i, folder: 1, name: format!("{i}.jpg"), format: "JPG".into(), ..Default::default() }).collect();
        let folders = HashMap::new();
        let k = Context { db: &db, rows: &rows, folder_paths: &folders };
        assert_eq!(evaluate(&k, &parse("ort:rom")), vec![true, false, false], "only the place, not the keyword Rom-Reise");
        assert_eq!(evaluate(&k, &parse("ort:italien")), vec![true, false, false], "the parent place finds its children");
        assert_eq!(evaluate(&k, &parse("person:christa")), vec![false, true, false]);
        assert_eq!(evaluate(&k, &parse("sammlung:urlaub")), vec![false, false, true]);
        // keyword: a parent term finds the photos of its children (also in smart collections, which are searches)
        assert_eq!(evaluate(&k, &parse("keyword:orte")), vec![true, false, false], "Orte › Italien › Rom");
        assert_eq!(evaluate(&k, &parse("kw:italien")), vec![true, false, false]);
        assert_eq!(evaluate(&k, &parse("keyword:rom")), vec![true, false, true], "Rom and Rom-Reise");
        assert_eq!(place_keywords(&db), HashSet::from([2, 3]));
    }

    /// Every name of a field gives the same query (base form inside), quoted values can be negated,
    /// a field without value is no condition, weekday and time of day.
    #[test]
    fn all_field_names() {
        for d in &crate::search_fields::FIELD_DEFS {
            for n in crate::search_fields::names(d) {
                let q = parse(&format!("{n}:5"));
                match &q.clauses[0][0].kind {
                    Kind::Field(f, _) => assert_eq!(f, d.id, "{n}"),
                    k => panic!("{n}: {k:?}"),
                }
            }
        }
        assert_eq!(parse("kamera:X100V"), parse("camera:X100V"));
        assert_eq!(parse("cam:X100V"), parse("camera:X100V"));
        assert_eq!(parse("カメラ：X100V"), parse("camera:X100V"));
        assert_eq!(parse("Stichwort:Baum"), parse("kw:Baum"));
        let q = parse("-camera:\"Canon EOS\"");
        assert!(q.clauses[0][0].negated);
        assert_eq!(q.clauses[0][0].kind, Kind::Field("camera".into(), Cmp::Text("canon eos".into())));
        // a quoted literal stays a word
        assert!(matches!(parse("\"-x\"").clauses[0][0].kind, Kind::Word { .. }));
        assert!(matches!(parse("\"kamera:x\"").clauses[0][0].kind, Kind::Word { .. }));
        // while typing: no condition, no full-text word "camera"
        assert!(parse("camera:").clauses.is_empty());
        assert!(parse("camera:\"\"").clauses.is_empty());
        assert_eq!(parse("rom camera:\"\"").clauses.len(), 1);
        // weekday and time of day
        let mut z = Row { image: 1, file: 1, folder: 1, name: "a".into(), format: "JPG".into(), ..Default::default() };
        z.capture = Some("2024-05-04T19:30:00".into()); // Saturday evening
        let hit = |q: &str, z: &Row| match &parse(q).clauses[0][0].kind {
            Kind::Field(f, c) => number_matches(f, c, z),
            _ => false,
        };
        assert_eq!(weekday_of(&z), Some(6));
        assert!(hit("weekday:sa", &z) && hit("wochentag:samedi", &z) && hit("wd:6", &z) && hit("weekday:wochenende", &z) && hit("weekday:fr-so", &z));
        assert!(!hit("weekday:mo", &z));
        assert!(hit("time:abend", &z) && hit("tageszeit:evening", &z) && hit("time:>=19", &z) && hit("time:18-20", &z));
        assert!(!hit("time:morgen", &z) && !hit("time:7", &z));
        // the shutter speed keeps its former German name "zeit"
        assert_eq!(parse("zeit:1/60"), parse("shutter:1/60"));
        z.capture = Some("2000-01-01T03:00:00".into());
        assert_eq!(weekday_of(&z), Some(6));
        assert!(hit("time:nacht", &z));
    }

    #[test]
    fn file_name_fragment() {
        assert!(only_alnum("DP2M5436.X3F").contains(&only_alnum("5436")));
        assert!(only_alnum("DP2M5436.X3F").contains(&only_alnum("DP2M_5436")));
        assert!(!only_alnum("DP2M5436.X3F").contains(&only_alnum("5437")));
    }

    #[test]
    fn flag() {
        let t = "Schimmel an der Wand, feuchte Wände im Bad.";
        let h = highlight(t, &["wande".into(), "schimmel".into()]);
        let w: Vec<&str> = h.iter().map(|&(a, e)| &t[a..e]).collect();
        assert_eq!(w, vec!["Schimmel", "Wand", "Wände"]);
    }

    /// Field terms in every interface language: names and values the interface shows are understood.
    #[test]
    fn field_terms_in_all_languages() {
        let understood = |q: &str| {
            let a = parse(q);
            match a.clauses.as_slice() {
                [c] => match c.as_slice() {
                    [t] => match &t.kind {
                        Kind::Field(f, cmp) => value_understood(f, cmp),
                        _ => false,
                    },
                    _ => false,
                },
                _ => false,
            }
        };
        for q in [
            "lens:24-70", "objektiv:24-70", "objectif:35", "objetivo:35", "レンズ:35", "camera:X100V", "kamera:X100V",
            "aperture:<=2.8", "blende:<=2.8", "focal:20-35", "iso:>=1600", "shutter:<=1/60", "zeit:<=1/60",
            "date:>2020-05", "year:2020", "month:7", "day:15", "rating:>=3", "sterne:>=3",
            "weekday:sa", "weekday:sat", "weekday:sam", "weekday:sab", "weekday:土", "wochentag:sa", "weekday:6",
            "weekday:weekend", "weekday:week-end", "weekday:週末", "weekday:sábado", "weekday:土曜日",
            "time:abend", "time:evening", "time:soir", "time:noche", "time:tarde", "time:夜", "time:tagsüber",
            "time:daytime", "time:journée", "time:día", "time:日中", "time:6-9", "time:>=20",
            "label:red", "label:rot", "label:verte", "label:morada", "label:赤", "etikett:rot",
            "flag:picked", "flag:markiert", "flag:flagged", "flag:marqué", "flag:retenue", "flag:marcada", "flag:フラグ付き",
            "flag:rejected", "flag:rejetée", "flag:rechazada", "flag:除外", "flag:none", "flag:aucun", "flag:なし",
            "ai:good", "ai:gut", "ai:fair", "ai:bonne", "ai:buena", "ai:良", "ai:中", "ai:除外", "ai:rejected",
            "has:person", "no:keyword", "hat:person", "ohne:stichwort", "edited:<2d", "imported:<1w", "format:X3F",
        ] {
            assert!(understood(q), "{q}");
        }
        for q in ["weekday:xyz", "label:pink", "flag:vielleicht", "ai:super", "time:<=1/60", "aperture:gross", "edited:bald"] {
            assert!(!understood(q), "{q}");
        }
        // Spanish "tarde" is the evening, as the interface shows it
        assert_eq!(crate::search_fields::time_of_day_named("tarde"), Some(3));
    }

    #[test]
    fn smart_rules() {
        assert_eq!(lightroom_rules("s = { { criteria = \"rating\", operation = \">=\", value = 5, }, combine = \"intersect\", }").as_deref(), Some("rating:>=5"));
        assert_eq!(lightroom_rules("s = { { criteria = \"keywords\", operation = \"empty\", }, combine = \"intersect\", }").as_deref(), Some("no:keyword"));
        assert_eq!(lightroom_rules("s = { { criteria = \"captureTime\", operation = \">\", value = \"2009-07-24\", value2 = \"2009-10-24\", }, combine = \"intersect\", }").as_deref(), Some("date:>2009-07-24"));
        assert_eq!(lightroom_rules("s = { { criteria = \"camera\", operation = \"==\", value = \"iPhone\", value2 = \"\", }, combine = \"intersect\", }").as_deref(), Some("camera:iPhone"));
        assert_eq!(lightroom_rules("s = { { criteria = \"touchTime\", operation = \"inLast\", value = 2, value_units = \"days\", }, combine = \"intersect\", }").as_deref(), Some("edited:<2d"));
        assert_eq!(lightroom_rules("s = { { criteria = \"touchTime\", operation = \"inLast\", value = 10, value2 = 1, value_units = \"weeks\", }, combine = \"intersect\", }").as_deref(), Some("edited:<10w"));
        assert_eq!(old_state("<10w"), Some((true, 70 * 86400)));
        assert!(lightroom_rules("s = { { criteria = \"sdktext:com.jkeir.lightroom.finddupes2.DupesId\", operation = \"notEmpty\", }, combine = \"intersect\", }").is_none());
    }
}
