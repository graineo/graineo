//! Develop history per photo and virtual copy (tables `develop_step`, `develop_cursor`).
//!
//! - The current state in `develop_state.settings` stays the only truth for display, export and previews. The history
//!   is a companion list: if it is lost or incomplete, nothing changes on the photo.
//! - One row per step. Step 0 (`start`) and compaction points (`compacted`) hold the **whole** state (the serde JSON of
//!   the develop settings); every other step holds a **patch** from the state of the step before.
//! - The cursor (the step whose state applies) is stored only when it is not on the newest step.
//!
//! ## Patch format
//!
//! A JSON merge patch (RFC 7386) on objects, with three extensions (keys starting with `$` never occur in the settings):
//!
//! - `{"$v": X}`: the value `X` literally (for `null` and for objects that must not be merged),
//! - `{"$a": {"n": len, "c": {"i": P, …}, "add": [P, …]}}`: element-wise change of a list – element `i` patched with
//!   `P`, new elements appended, the length cut to `n`. A new brush stroke or retouch spot costs its own size, not the
//!   whole list's,
//! - `{"$n": k}`: the `k`-th list or object of the **previous** state (counted from 0 in pre-order, object members in
//!   key order), unchanged. Moved subtrees: a mask's strokes are a balanced tree that is rebuilt with every stroke,
//!   its unchanged strokes are referenced, not copied.
//!
//! Everything else is plain RFC 7386: a member `null` removes it, an object is merged, any other value replaces.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};

/// Steps kept per photo or copy; older ones are compacted into one step with the whole state (the start stays).
pub const MAX_STEPS: usize = 100;
/// Upper limit of the whole table (bytes of patches and labels); above it a quiet background run compacts the photos
/// touched longest ago down to start state + current state.
pub const MAX_TOTAL_BYTES: i64 = 100 * 1000 * 1000;
/// Steps older than this (seconds) are included when a photo's history is compacted.
pub const MAX_AGE: i64 = 365 * 24 * 3600;

/// Step kinds (column `kind`).
pub mod kind {
    pub const START: &str = "start";
    pub const SLIDER: &str = "slider";
    pub const PRESET: &str = "preset";
    pub const FILM: &str = "film";
    pub const PASTE: &str = "paste";
    pub const SYNC: &str = "sync";
    pub const AUTO: &str = "auto";
    pub const RESET: &str = "reset";
    pub const CROP: &str = "crop";
    pub const MASK: &str = "mask";
    pub const RETOUCH: &str = "retouch";
    pub const LIGHTROOM: &str = "lightroom";
    pub const EXTERNAL: &str = "external";
    pub const VERSION: &str = "version";
    pub const COMPACTED: &str = "compacted";
}

/// One step of the history.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// 0 = start state, then ascending (gaps possible after compaction)
    pub seq: i64,
    /// Unix seconds (last change of a merged step)
    pub time: i64,
    pub kind: String,
    /// key and values of the displayed name (JSON text, translated when shown)
    pub label: Option<String>,
    /// `start`/`compacted`: the whole state; else the patch from the state of the step before
    pub patch: Value,
    /// merged steps
    pub count: i64,
    /// command for several photos the step belongs to
    pub command: Option<i64>,
}

impl Step {
    /// Does the step hold the whole state (start or compaction point)?
    pub fn is_full(&self) -> bool {
        is_full_kind(&self.kind)
    }

    /// Bytes the step takes in the table (patch and label text).
    pub fn bytes(&self) -> usize {
        self.patch.to_string().len() + self.label.as_ref().map_or(0, String::len)
    }
}

pub fn is_full_kind(kind: &str) -> bool {
    kind == kind::START || kind == kind::COMPACTED
}

// ─────────────────────────────────────────────── patches

/// Subtrees smaller than this (serialized bytes) are never referenced (the reference would not be shorter).
const MIN_REF: usize = 48;

/// Lists and objects of the previous state in pre-order (object members in key order), and the larger ones by
/// structure hash.
struct Index<'a> {
    nodes: Vec<&'a Value>,
    map: HashMap<u64, Vec<usize>>,
}

/// Lists and objects of `root` in pre-order, object members in key order (the numbering of `$n`).
fn preorder<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Array(a) => {
            out.push(v);
            for x in a {
                preorder(x, out);
            }
        }
        Value::Object(o) => {
            out.push(v);
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            for k in keys {
                preorder(&o[k], out);
            }
        }
        _ => {}
    }
}

fn hash_value(v: &Value, h: &mut std::collections::hash_map::DefaultHasher) {
    match v {
        Value::Null => 0u8.hash(h),
        Value::Bool(b) => (1u8, *b).hash(h),
        Value::Number(n) => (2u8, n.to_string()).hash(h),
        Value::String(s) => (3u8, s).hash(h),
        Value::Array(a) => {
            (4u8, a.len()).hash(h);
            for x in a {
                hash_value(x, h);
            }
        }
        Value::Object(o) => {
            (5u8, o.len()).hash(h);
            for (k, x) in o {
                k.hash(h);
                hash_value(x, h);
            }
        }
    }
}

fn hash_of(v: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_value(v, &mut h);
    h.finish()
}

/// Rough serialized size (exact enough to choose between encodings; no allocation).
fn size_of(v: &Value) -> usize {
    match v {
        Value::Null => 4,
        Value::Bool(_) => 5,
        Value::Number(_) => 8,
        Value::String(s) => s.len() + 2,
        Value::Array(a) => 2 + a.iter().map(|x| size_of(x) + 1).sum::<usize>(),
        Value::Object(o) => 2 + o.iter().map(|(k, x)| k.len() + 4 + size_of(x)).sum::<usize>(),
    }
}

impl<'a> Index<'a> {
    fn new(root: &'a Value) -> Self {
        let mut nodes = Vec::new();
        preorder(root, &mut nodes);
        let mut map: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, v) in nodes.iter().enumerate().skip(1) {
            if size_of(v) >= MIN_REF {
                map.entry(hash_of(v)).or_default().push(i);
            }
        }
        Index { nodes, map }
    }

    /// Number of a subtree equal to `v` in the previous state.
    fn find(&self, v: &Value) -> Option<usize> {
        if !matches!(v, Value::Array(_) | Value::Object(_)) || size_of(v) < MIN_REF {
            return None;
        }
        self.map.get(&hash_of(v))?.iter().copied().find(|&i| self.nodes[i] == v)
    }
}

fn literal(v: &Value) -> Value {
    match v {
        Value::Null | Value::Object(_) => Value::Object(Map::from_iter([("$v".to_string(), v.clone())])),
        other => other.clone(),
    }
}

fn serialized_len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(usize::MAX, |s| s.len())
}

/// Patch for the place whose previous value is `old` (`None`: not there) and whose new value is `new` (`old != new`).
fn diff_at(ix: &Index, old: Option<&Value>, new: &Value) -> Value {
    if let Some(k) = ix.find(new) {
        return Value::Object(Map::from_iter([("$n".to_string(), Value::from(k))]));
    }
    match (old, new) {
        (Some(Value::Object(a)), Value::Object(b)) => {
            let mut out = Map::new();
            for (k, nv) in b {
                if a.get(k) != Some(nv) {
                    out.insert(k.clone(), diff_at(ix, a.get(k), nv));
                }
            }
            for k in a.keys().filter(|k| !b.contains_key(*k)) {
                out.insert(k.clone(), Value::Null);
            }
            Value::Object(out)
        }
        (_, Value::Object(b)) => {
            // a fresh object: merged onto nothing (members that are `null` must stay: literal)
            let mut out = Map::new();
            for (k, nv) in b {
                out.insert(k.clone(), diff_at(ix, None, nv));
            }
            Value::Object(out)
        }
        (Some(Value::Array(a)), Value::Array(b)) => {
            let mut changed = Map::new();
            for i in 0..a.len().min(b.len()) {
                if a[i] != b[i] {
                    changed.insert(i.to_string(), diff_at(ix, Some(&a[i]), &b[i]));
                }
            }
            let add: Vec<Value> = b.iter().skip(a.len()).map(|x| diff_at(ix, None, x)).collect();
            let mut spec = Map::new();
            spec.insert("n".into(), Value::from(b.len()));
            if !changed.is_empty() {
                spec.insert("c".into(), Value::Object(changed));
            }
            if !add.is_empty() {
                spec.insert("add".into(), Value::Array(add));
            }
            let elementwise = Value::Object(Map::from_iter([("$a".to_string(), Value::Object(spec))]));
            if serialized_len(&elementwise) < serialized_len(new) {
                elementwise
            } else {
                new.clone()
            }
        }
        (_, Value::Array(b)) => {
            // a fresh list: literally, or built from references if that is shorter
            let add: Vec<Value> = b.iter().map(|x| diff_at(ix, None, x)).collect();
            let built = Value::Object(Map::from_iter([(
                "$a".to_string(),
                Value::Object(Map::from_iter([("n".to_string(), Value::from(b.len())), ("add".to_string(), Value::Array(add))])),
            )]));
            if serialized_len(&built) < serialized_len(new) {
                built
            } else {
                new.clone()
            }
        }
        (_, v) => literal(v),
    }
}

/// Patch from the state `old` to the state `new` (both the serde JSON of the settings). Equal states: `{}`.
pub fn diff(old: &Value, new: &Value) -> Value {
    if old == new {
        return Value::Object(Map::new());
    }
    let ix = Index::new(old);
    match (old, new) {
        (Value::Object(_), Value::Object(_)) => diff_at(&ix, Some(old), new),
        _ => literal(new),
    }
}

fn special<'a>(p: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    (p.len() == 1).then(|| p.get(key)).flatten()
}

/// Least upper limit of a state's size (serialized bytes, as [`size_of`] counts) while patches are applied. A history
/// whose replay grows beyond the limit counts as damaged (a manipulated or broken table must not exhaust memory).
pub const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;
/// Deepest nesting of a state or patch (the settings are far shallower; serde reads at most 128 levels).
pub const MAX_STATE_DEPTH: usize = 64;

/// Size limit for states replayed from the whole state `base`: [`MAX_STATE_BYTES`], or four times the base if larger.
fn limit_for(base: &Value) -> usize {
    MAX_STATE_BYTES.max(size_of(base).saturating_mul(4))
}

/// Is `v` nested at most `max` levels deep (a scalar is level 0)? Stops descending at the limit.
fn depth_within(v: &Value, max: usize) -> bool {
    match v {
        Value::Array(a) => max > 0 && a.iter().all(|x| depth_within(x, max - 1)),
        Value::Object(o) => max > 0 && o.values().all(|x| depth_within(x, max - 1)),
        _ => true,
    }
}

/// The whole previous state of a patch, numbered on first use of `$n`, and what the patch may still copy from it.
struct Root<'a> {
    root: &'a Value,
    nodes: Option<Vec<&'a Value>>,
    /// bytes `$n` may still copy (all references of one patch together)
    budget: usize,
}

/// Apply the patch `p` to the place whose previous value is `old`; `root` is the whole previous state (for `$n`),
/// `depth` the nesting of the place.
fn apply_at<'a>(root: &mut Root<'a>, old: Option<&Value>, p: &Value, depth: usize) -> Result<Value, String> {
    if depth > MAX_STATE_DEPTH {
        return Err(format!("patch deeper than {MAX_STATE_DEPTH} levels"));
    }
    let Value::Object(pm) = p else { return Ok(p.clone()) };
    if let Some(v) = special(pm, "$v") {
        return Ok(v.clone());
    }
    if let Some(k) = special(pm, "$n") {
        let k = k.as_u64().ok_or("$n without a number")? as usize;
        // 0 would be the whole previous state: never written (`Index` skips it), and a way to double a state per step
        if k == 0 {
            return Err("$n 0: the whole previous state cannot be referenced".into());
        }
        let r = root.root;
        let nodes = root.nodes.get_or_insert_with(|| {
            let mut v = Vec::new();
            preorder(r, &mut v);
            v
        });
        let node = *nodes.get(k).ok_or_else(|| format!("$n {k}: not in the previous state"))?;
        let size = size_of(node);
        root.budget = root.budget.checked_sub(size).ok_or_else(|| format!("$n {k}: patch copies more than the size limit"))?;
        return Ok(node.clone());
    }
    if let Some(Value::Object(spec)) = special(pm, "$a") {
        let base: &[Value] = match old {
            Some(Value::Array(a)) => a,
            _ => &[],
        };
        let n = spec.get("n").and_then(Value::as_u64).ok_or("$a without a length")? as usize;
        let mut out: Vec<Value> = base.iter().take(n).cloned().collect();
        if let Some(Value::Object(c)) = spec.get("c") {
            for (i, ep) in c {
                let i: usize = i.parse().map_err(|_| format!("$a: index {i}"))?;
                let slot = out.get_mut(i).ok_or_else(|| format!("$a: index {i} out of range"))?;
                *slot = apply_at(root, base.get(i), ep, depth + 1)?;
            }
        }
        if let Some(Value::Array(add)) = spec.get("add") {
            for ep in add {
                out.push(apply_at(root, None, ep, depth + 1)?);
            }
        }
        if out.len() != n {
            return Err(format!("$a: length {} instead of {n}", out.len()));
        }
        return Ok(Value::Array(out));
    }
    let mut out = match old {
        Some(Value::Object(o)) => o.clone(),
        _ => Map::new(),
    };
    for (k, mp) in pm {
        if mp.is_null() {
            out.remove(k);
        } else {
            let prev = match old {
                Some(Value::Object(o)) => o.get(k),
                _ => None,
            };
            let v = apply_at(root, prev, mp, depth + 1)?;
            out.insert(k.clone(), v);
        }
    }
    Ok(Value::Object(out))
}

/// State after applying `patch` to the state `old` (size limit from `old`, see [`MAX_STATE_BYTES`]).
pub fn apply(old: &Value, patch: &Value) -> Result<Value, String> {
    apply_limited(old, patch, limit_for(old).saturating_add(size_of(patch)))
}

/// [`apply`] with an explicit size limit of the result: a patch that would copy more, or a result larger than `limit`
/// or deeper than [`MAX_STATE_DEPTH`], is an error (never a huge allocation).
fn apply_limited(old: &Value, patch: &Value, limit: usize) -> Result<Value, String> {
    let out = apply_at(&mut Root { root: old, nodes: None, budget: limit }, Some(old), patch, 0)?;
    if !depth_within(&out, MAX_STATE_DEPTH) {
        return Err(format!("state deeper than {MAX_STATE_DEPTH} levels"));
    }
    if size_of(&out) > limit {
        return Err("state larger than the size limit".into());
    }
    Ok(out)
}

/// State at step `index`: from the nearest whole state at or before it, forward through the patches. `None`: no whole
/// state before it or a patch that does not fit or grows the state beyond the limits (damaged history; the stored
/// settings stay the truth).
pub fn state_at(steps: &[Step], index: usize) -> Option<Value> {
    let index = index.min(steps.len().checked_sub(1)?);
    let from = steps[..=index].iter().rposition(Step::is_full)?;
    let mut s = steps[from].patch.clone();
    // one limit for the whole replay: a step cannot raise it for the next one
    let limit = limit_for(&s);
    for st in &steps[from + 1..=index] {
        s = apply_limited(&s, &st.patch, limit).ok()?;
    }
    Some(s)
}

/// The JSON a step's patch is stored as.
pub fn patch_text(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "{}".into())
}

// ─────────────────────────────────────────────── compaction and check

/// Fold the steps 1 ..= `k` into one `compacted` step holding their resulting state ("Verlauf bis hier
/// zusammenfassen"); the start and later steps stay. `k` must not lie after the cursor. Returns the new list and
/// cursor, `None` if nothing is to be done (`k < 2`).
pub fn compact_to(steps: &[Step], cursor: usize, k: usize) -> Option<(Vec<Step>, usize)> {
    let len = steps.len();
    let cursor = cursor.min(len.checked_sub(1)?);
    if k < 2 || k > cursor {
        return None;
    }
    let state = state_at(steps, k)?;
    let folded: i64 = steps[1..=k].iter().map(|s| if s.kind == kind::COMPACTED { label_number(s, "n").unwrap_or(1) } else { s.count.max(1) }).sum();
    let c = Step {
        seq: steps[k].seq,
        time: steps[k].time,
        kind: kind::COMPACTED.into(),
        label: Some(serde_json::json!({ "k": kind::COMPACTED, "n": folded }).to_string()),
        patch: state,
        count: 1,
        command: None,
    };
    let mut out = Vec::with_capacity(len - k + 1);
    out.push(steps[0].clone());
    out.push(c);
    out.extend(steps[k + 1..].iter().cloned());
    Some((out, cursor - k + 1))
}

/// Compact a history longer than `max`: the start stays, the oldest steps (and every step older than [`MAX_AGE`]) up to
/// at most the cursor become one `compacted` step holding their resulting state. Later patches stay valid (they
/// start from that state). Returns the new list and cursor, `None` if nothing is to be done.
pub fn compact(steps: &[Step], cursor: usize, max: usize, now: i64) -> Option<(Vec<Step>, usize)> {
    let len = steps.len();
    let max = max.max(3);
    let cursor = cursor.min(len.checked_sub(1)?);
    let mut k = if len > max { len - max + 1 } else { 0 };
    if let Some(old) = steps.iter().rposition(|s| s.time < now - MAX_AGE && s.seq != steps[0].seq) {
        k = k.max(old);
    }
    let k = k.min(cursor);
    if k < 2 {
        return None;
    }
    let state = state_at(steps, k)?;
    let folded: i64 = steps[1..=k]
        .iter()
        .map(|s| if s.kind == kind::COMPACTED { label_number(s, "n").unwrap_or(1) } else { s.count.max(1) })
        .sum();
    let c = Step {
        seq: steps[k].seq,
        time: steps[k].time,
        kind: kind::COMPACTED.into(),
        label: Some(serde_json::json!({ "k": kind::COMPACTED, "n": folded }).to_string()),
        patch: state,
        count: 1,
        command: None,
    };
    let mut out = Vec::with_capacity(len - k + 1);
    out.push(steps[0].clone());
    out.push(c);
    out.extend(steps[k + 1..].iter().cloned());
    let cursor = cursor - k + 1;
    Some((out, cursor))
}

fn label_number(s: &Step, key: &str) -> Option<i64> {
    serde_json::from_str::<Value>(s.label.as_deref()?).ok()?.get(key)?.as_i64()
}

/// Check the history against the current state: if replaying up to the cursor does not give `current` (another
/// version of the app, sidecars read in, a Lightroom import, an error), a step `external` with the difference is
/// appended (steps after the cursor fall away first). The state is never changed. Returns the operations that bring the
/// stored history in line (empty: it fits).
pub fn reconcile(steps: &mut Vec<Step>, cursor: &mut usize, current: &Value, now: i64) -> Vec<Op> {
    if steps.is_empty() {
        return Vec::new();
    }
    *cursor = (*cursor).min(steps.len() - 1);
    match state_at(steps, *cursor) {
        Some(s) if s == *current => Vec::new(),
        Some(s) => {
            let mut ops = Vec::new();
            if *cursor + 1 < steps.len() {
                steps.truncate(*cursor + 1);
                ops.push(Op::TruncateAfter(steps[*cursor].seq));
            }
            let step = Step {
                seq: steps[*cursor].seq + 1,
                time: now,
                kind: kind::EXTERNAL.into(),
                label: Some(serde_json::json!({ "k": kind::EXTERNAL }).to_string()),
                patch: diff(&s, current),
                count: 1,
                command: None,
            };
            steps.push(step.clone());
            *cursor = steps.len() - 1;
            ops.push(Op::Put(step));
            ops.push(Op::Cursor(None));
            ops
        }
        None => {
            // damaged: start anew from the current state (nothing else can be trusted)
            let step = Step {
                seq: steps.last().map_or(0, |s| s.seq) + 1,
                time: now,
                kind: kind::COMPACTED.into(),
                label: Some(serde_json::json!({ "k": kind::COMPACTED, "n": 0 }).to_string()),
                patch: current.clone(),
                count: 1,
                command: None,
            };
            *steps = vec![step];
            *cursor = 0;
            vec![Op::ReplaceAll(steps.clone()), Op::Cursor(None)]
        }
    }
}

/// A command for several photos changed one photo from `before` to `after` (paste, transfer, reset …, or the undo /
/// redo of such a command, `undo`): the operations for its history (steps in order, cursor `seq`).
/// - no history yet: the start state (`before`) first,
/// - a history that does not fit `before`: an `external` step first (as [`reconcile`]),
/// - the command's own step right before the cursor and `after` is the state before it (its undo): the cursor goes
///   back; the command's step right after the cursor and `after` is its state (its redo): the cursor goes forward,
/// - otherwise a new step (steps after the cursor fall away); an undo that cannot move the cursor (the photo was
///   edited since) is named "Rückgängig: …" (`{"k": "undo", "of": label}`).
#[allow(clippy::too_many_arguments)]
pub fn plan_command(steps: &[Step], cursor: Option<i64>, before: &Value, after: &Value, kind_: &str, label: &Value, command: i64, undo: bool, now: i64) -> Vec<Op> {
    let mut steps = steps.to_vec();
    let mut cur = cursor;
    let mut ops = Vec::new();
    if before == after {
        return ops;
    }
    if steps.is_empty() {
        let start = Step { seq: 0, time: now, kind: kind::START.into(), label: Some(serde_json::json!({ "k": "start" }).to_string()), patch: before.clone(), count: 1, command: None };
        ops.push(Op::Put(start.clone()));
        steps.push(start);
        cur = None;
    } else {
        let mut ci = cursor_index(&steps, cur);
        let fix = reconcile(&mut steps, &mut ci, before, now);
        if !fix.is_empty() {
            ops.extend(fix);
            cur = None;
        }
    }
    let ci = cursor_index(&steps, cur);
    let top = steps.len() - 1;
    if ci > 0 && steps[ci].command == Some(command) && state_at(&steps, ci - 1).as_ref() == Some(after) {
        ops.push(Op::Cursor(Some(steps[ci - 1].seq)));
        return ops;
    }
    if ci < top && steps[ci + 1].command == Some(command) && state_at(&steps, ci + 1).as_ref() == Some(after) {
        ops.push(Op::Cursor((ci + 1 < top).then(|| steps[ci + 1].seq)));
        return ops;
    }
    if ci < top {
        ops.push(Op::TruncateAfter(steps[ci].seq));
        steps.truncate(ci + 1);
    }
    let label = if undo { serde_json::json!({ "k": "undo", "of": label }) } else { label.clone() };
    let step = Step { seq: steps[ci].seq + 1, time: now, kind: kind_.into(), label: Some(label.to_string()), patch: diff(before, after), count: 1, command: Some(command) };
    ops.push(Op::Put(step.clone()));
    ops.push(Op::Cursor(None));
    steps.push(step);
    if let Some((c, _)) = compact(&steps, steps.len() - 1, MAX_STEPS, now) {
        ops.push(Op::ReplaceAll(c));
    }
    ops
}

// ─────────────────────────────────────────────── storage

/// A change of the stored history of one photo/copy (applied in order, in the same transaction as the state).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// insert or replace the step with this `seq` (appending, or the newest step after merging)
    Put(Step),
    /// remove the steps after this `seq`
    TruncateAfter(i64),
    /// cursor on this `seq`; `None`: on the newest step
    Cursor(Option<i64>),
    /// replace the whole history (compaction)
    ReplaceAll(Vec<Step>),
    /// remove the whole history
    Clear,
}

fn copy_key(copy: Option<&str>) -> &str {
    copy.unwrap_or("")
}

fn step_row(r: &rusqlite::Row) -> rusqlite::Result<Step> {
    let patch: String = r.get(4)?;
    Ok(Step {
        seq: r.get(0)?,
        time: r.get(1)?,
        kind: r.get(2)?,
        label: r.get(3)?,
        patch: serde_json::from_str(&patch).unwrap_or(Value::Null),
        count: r.get(5)?,
        command: r.get(6)?,
    })
}

/// The stored history of `file`/`copy`: steps in order and the cursor (`seq`, `None`: on the newest step).
pub fn load(conn: &Connection, file: i64, copy: Option<&str>) -> rusqlite::Result<(Vec<Step>, Option<i64>)> {
    let steps = conn
        .prepare_cached("SELECT seq, time, kind, label, patch, count, command FROM develop_step WHERE file = ?1 AND copy = ?2 ORDER BY seq")?
        .query_map(params![file, copy_key(copy)], step_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let cursor = conn
        .prepare_cached("SELECT seq FROM develop_cursor WHERE file = ?1 AND copy = ?2")?
        .query_row(params![file, copy_key(copy)], |r| r.get::<_, i64>(0))
        .optional()?;
    Ok((steps, cursor))
}

/// Index of the cursor `seq` in `steps` (`None` or not found: the newest step).
pub fn cursor_index(steps: &[Step], cursor: Option<i64>) -> usize {
    let top = steps.len().saturating_sub(1);
    cursor.and_then(|c| steps.iter().position(|s| s.seq == c)).unwrap_or(top)
}

fn put(conn: &Connection, file: i64, copy: &str, s: &Step) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO develop_step(file, copy, seq, time, kind, label, patch, count, command) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(file, copy, seq) DO UPDATE SET time = excluded.time, kind = excluded.kind, label = excluded.label,
            patch = excluded.patch, count = excluded.count, command = excluded.command",
    )?
    .execute(params![file, copy, s.seq, s.time, s.kind, s.label, patch_text(&s.patch), s.count, s.command])?;
    Ok(())
}

/// Write operations into the library (inside the caller's transaction).
pub fn apply_ops(conn: &Connection, file: i64, copy: Option<&str>, ops: &[Op]) -> rusqlite::Result<()> {
    let copy = copy_key(copy);
    for op in ops {
        match op {
            Op::Put(s) => put(conn, file, copy, s)?,
            Op::TruncateAfter(seq) => {
                conn.prepare_cached("DELETE FROM develop_step WHERE file = ?1 AND copy = ?2 AND seq > ?3")?.execute(params![file, copy, seq])?;
            }
            Op::Cursor(None) => {
                conn.prepare_cached("DELETE FROM develop_cursor WHERE file = ?1 AND copy = ?2")?.execute(params![file, copy])?;
            }
            Op::Cursor(Some(seq)) => {
                conn.prepare_cached("INSERT INTO develop_cursor(file, copy, seq) VALUES(?1, ?2, ?3) ON CONFLICT(file, copy) DO UPDATE SET seq = excluded.seq")?
                    .execute(params![file, copy, seq])?;
            }
            Op::ReplaceAll(steps) => {
                conn.prepare_cached("DELETE FROM develop_step WHERE file = ?1 AND copy = ?2")?.execute(params![file, copy])?;
                for s in steps {
                    put(conn, file, copy, s)?;
                }
            }
            Op::Clear => forget(conn, file, Some(copy))?,
        }
    }
    Ok(())
}

/// Apply operations to a history in memory (steps in order, cursor `seq`) – the same result as [`apply_ops`] on the
/// table: reads see changes still on their way to the library.
pub fn apply_ops_mem(steps: &mut Vec<Step>, cursor: &mut Option<i64>, ops: &[Op]) {
    for op in ops {
        match op {
            Op::Put(s) => match steps.binary_search_by_key(&s.seq, |x| x.seq) {
                Ok(i) => steps[i] = s.clone(),
                Err(i) => steps.insert(i, s.clone()),
            },
            Op::TruncateAfter(seq) => steps.retain(|x| x.seq <= *seq),
            Op::Cursor(c) => *cursor = *c,
            Op::ReplaceAll(v) => {
                *steps = v.clone();
                steps.sort_by_key(|x| x.seq);
            }
            Op::Clear => {
                steps.clear();
                *cursor = None;
            }
        }
    }
}

/// Remove the history of one photo/copy (`copy = None`: the original's only).
pub fn forget(conn: &Connection, file: i64, copy: Option<&str>) -> rusqlite::Result<()> {
    let copy = copy_key(copy);
    conn.prepare_cached("DELETE FROM develop_step WHERE file = ?1 AND copy = ?2")?.execute(params![file, copy])?;
    conn.prepare_cached("DELETE FROM develop_cursor WHERE file = ?1 AND copy = ?2")?.execute(params![file, copy])?;
    Ok(())
}

/// Remove every history of the library (the user's "Delete all histories …"). Returns the steps removed.
pub fn forget_all(conn: &Connection) -> rusqlite::Result<usize> {
    let n = conn.execute("DELETE FROM develop_step", [])?;
    conn.execute("DELETE FROM develop_cursor", [])?;
    Ok(n)
}

/// A file row `s` is merged into `t` (moved photo already at the target): the histories go with the develop states.
/// Copies of `s` take theirs along unless `t` has a history for a copy of the same name; the original's moves when
/// `original` (the develop state of `s` won) – replacing that of `t` – and is dropped otherwise.
pub fn merge_file(conn: &Connection, s: i64, t: i64, original: bool) -> rusqlite::Result<()> {
    // the copies whose steps move (the target has no steps for them): their cursor moves with them, never alone (a
    // cursor of `s` over the steps of `t` would point into another history)
    let moved: Vec<String> = conn
        .prepare("SELECT DISTINCT copy FROM develop_step WHERE file = ?1 AND copy <> '' AND copy NOT IN (SELECT copy FROM develop_step WHERE file = ?2)")?
        .query_map(params![s, t], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for copy in &moved {
        conn.execute("UPDATE develop_step SET file = ?2 WHERE file = ?1 AND copy = ?3", params![s, t, copy])?;
        // a cursor of the target without steps is left over: the moved one replaces it
        conn.execute("DELETE FROM develop_cursor WHERE file = ?2 AND copy = ?3 AND EXISTS (SELECT 1 FROM develop_cursor WHERE file = ?1 AND copy = ?3)", params![s, t, copy])?;
        conn.execute("UPDATE develop_cursor SET file = ?2 WHERE file = ?1 AND copy = ?3", params![s, t, copy])?;
    }
    if original {
        forget(conn, t, None)?;
        conn.execute("UPDATE develop_step SET file = ?2 WHERE file = ?1 AND copy = ''", params![s, t])?;
        conn.execute("UPDATE develop_cursor SET file = ?2 WHERE file = ?1 AND copy = ''", params![s, t])?;
    }
    // what is left of `s` goes with its row (ON DELETE CASCADE)
    Ok(())
}

/// Bytes of the whole history table (patches and labels).
pub fn total_bytes(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT coalesce(sum(length(patch) + coalesce(length(label), 0)), 0) FROM develop_step", [], |r| r.get(0))
}

/// Number of steps and photos/copies with a history.
pub fn counts(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row("SELECT count(*), (SELECT count(*) FROM (SELECT 1 FROM develop_step GROUP BY file, copy)) FROM develop_step", [], |r| Ok((r.get(0)?, r.get(1)?)))
}

/// Shortest form of a history: start state and the state at the cursor (later steps fall away).
pub fn shortest(steps: &[Step], cursor: usize) -> Option<Vec<Step>> {
    let first = steps.first()?;
    let cursor = cursor.min(steps.len() - 1);
    if cursor == 0 {
        return Some(vec![first.clone()]);
    }
    let state = state_at(steps, cursor)?;
    let folded: i64 = steps[1..=cursor].iter().map(|s| if s.kind == kind::COMPACTED { label_number(s, "n").unwrap_or(1) } else { s.count.max(1) }).sum();
    Some(vec![
        first.clone(),
        Step {
            seq: steps[cursor].seq,
            time: steps[cursor].time,
            kind: kind::COMPACTED.into(),
            label: Some(serde_json::json!({ "k": kind::COMPACTED, "n": folded }).to_string()),
            patch: state,
            count: 1,
            command: None,
        },
    ])
}

/// Histories the app holds in memory ((file, copy) → number of holders) and the one being compacted right now.
struct InUse {
    held: std::collections::BTreeMap<(i64, String), usize>,
    compacting: Option<(i64, String)>,
}

static IN_USE: std::sync::Mutex<InUse> = std::sync::Mutex::new(InUse { held: std::collections::BTreeMap::new(), compacting: None });
static COMPACTED: std::sync::Condvar = std::sync::Condvar::new();

fn in_use() -> std::sync::MutexGuard<'static, InUse> {
    IN_USE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A history the app holds in memory: [`enforce_total`] never compacts it while the hold lives (the app would go on
/// with its long list and write steps that no longer fit the table). Taken before the history is read; dropping it
/// never waits.
#[derive(Debug)]
pub struct Hold(i64, String);

/// Hold the history of `file`/`copy` (see [`Hold`]). Waits only while exactly this history is being compacted (one
/// short transaction), so what is read afterwards is the compacted one.
pub fn hold(file: i64, copy: Option<&str>) -> Hold {
    let key = (file, copy.unwrap_or("").to_string());
    let mut g = in_use();
    while g.compacting.as_ref() == Some(&key) {
        g = COMPACTED.wait(g).unwrap_or_else(|e| e.into_inner());
    }
    *g.held.entry(key.clone()).or_default() += 1;
    Hold(key.0, key.1)
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut g = in_use();
        let key = (self.0, std::mem::take(&mut self.1));
        if let Some(n) = g.held.get_mut(&key) {
            *n -= 1;
            if *n == 0 {
                g.held.remove(&key);
            }
        }
    }
}

/// Keep the whole table under `max` bytes: compact the histories touched longest ago (not since `protect_since`, and
/// none the app holds in memory, [`hold`]) to start + current state, one per transaction, until it is below 90 % of
/// `max`. Returns the histories compacted.
pub fn enforce_total(conn: &Connection, max: i64, protect_since: i64) -> rusqlite::Result<usize> {
    let mut total = total_bytes(conn)?;
    if total <= max {
        return Ok(0);
    }
    let goal = max / 10 * 9;
    let keys: Vec<(i64, String)> = conn
        .prepare("SELECT file, copy FROM develop_step GROUP BY file, copy HAVING count(*) > 2 AND max(time) < ?1 ORDER BY max(time)")?
        .query_map([protect_since], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut done = 0;
    for key in keys {
        // a history the app holds is skipped; the others are marked while compacting (a hold waits for it)
        {
            let mut g = in_use();
            if g.held.contains_key(&key) {
                continue;
            }
            g.compacting = Some(key.clone());
        }
        let r = (|| -> rusqlite::Result<i64> {
            let (file, copy) = (key.0, (!key.1.is_empty()).then_some(key.1.as_str()));
            let tx = conn.unchecked_transaction()?;
            let (steps, cursor) = load(&tx, file, copy)?;
            let before: i64 = steps.iter().map(|s| s.bytes() as i64).sum();
            let ci = cursor_index(&steps, cursor);
            let Some(short) = shortest(&steps, ci) else { return Ok(-1) };
            let after: i64 = short.iter().map(|s| s.bytes() as i64).sum();
            apply_ops(&tx, file, copy, &[Op::ReplaceAll(short), Op::Cursor(None)])?;
            tx.commit()?;
            Ok(before - after)
        })();
        in_use().compacting = None;
        COMPACTED.notify_all();
        let saved = r?;
        if saved >= 0 {
            total -= saved;
            done += 1;
        }
        if total <= goal {
            break;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Small deterministic random numbers (xorshift) for the round trips.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
        fn f(&mut self) -> f64 {
            (self.below(2001) as f64 - 1000.0) / 1000.0
        }
    }

    fn sample() -> Value {
        json!({
            "exposure": 0.0, "contrast": 0.0, "color_mode": null, "mix": [0.33, 0.33, 0.34],
            "hsl": [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
            "film": { "active": false, "stock": "", "paper": null, "grain": 0.0 },
            "raw": { "look": "", "whites": 0.0 },
            "view": "Developed"
        })
    }

    /// One random change of a settings-like JSON (numbers, nulls, strings, members added and removed, lists grown,
    /// shrunk and changed, nested objects).
    fn mutate(r: &mut Rng, v: &mut Value, depth: u32) {
        match v {
            Value::Object(o) => {
                let keys: Vec<String> = o.keys().cloned().collect();
                match r.below(10) {
                    0 if !keys.is_empty() => {
                        let k = &keys[r.below(keys.len() as u64) as usize];
                        o.remove(k);
                    }
                    1 => {
                        let k = format!("k{}", r.below(6));
                        let x = match r.below(5) {
                            0 => Value::Null,
                            1 => json!(r.f()),
                            2 => json!({ "a": r.f(), "b": null }),
                            3 => json!([r.f(), r.f()]),
                            _ => json!(format!("s{}", r.below(9))),
                        };
                        o.insert(k, x);
                    }
                    _ if !keys.is_empty() => {
                        let k = &keys[r.below(keys.len() as u64) as usize];
                        if depth < 4 {
                            mutate(r, o.get_mut(k).unwrap(), depth + 1);
                        }
                    }
                    _ => {}
                }
            }
            Value::Array(a) => match r.below(5) {
                0 => a.push(if r.below(2) == 0 { json!(r.f()) } else { json!({ "x": r.f(), "pts": [[r.f(), r.f()]] }) }),
                1 if !a.is_empty() => {
                    a.pop();
                }
                _ if !a.is_empty() => {
                    let i = r.below(a.len() as u64) as usize;
                    if depth < 4 {
                        mutate(r, &mut a[i], depth + 1);
                    }
                }
                _ => {}
            },
            Value::Number(_) => *v = json!(r.f()),
            Value::Null => *v = json!(r.f()),
            Value::Bool(b) => *b = !*b,
            Value::String(_) => *v = if r.below(4) == 0 { Value::Null } else { json!(format!("t{}", r.below(99))) },
        }
    }

    fn step(seq: i64, kind: &str, patch: Value) -> Step {
        Step { seq, time: 1000 + seq, kind: kind.into(), label: None, patch, count: 1, command: None }
    }

    /// Patches rebuild every state of long random sequences exactly, also after a trip through the stored text.
    #[test]
    fn round_trip_random_sequences() {
        for seed in 1..40u64 {
            let mut r = Rng(seed * 0x9E37_79B9_7F4A_7C15);
            let mut state = sample();
            let mut steps = vec![step(0, kind::START, state.clone())];
            let mut states = vec![state.clone()];
            for i in 1..150 {
                let mut next = state.clone();
                for _ in 0..1 + r.below(3) {
                    mutate(&mut r, &mut next, 0);
                }
                let p = diff(&state, &next);
                let stored: Value = serde_json::from_str(&patch_text(&p)).unwrap();
                assert_eq!(apply(&state, &stored).unwrap(), next, "seed {seed}, step {i}: {p}");
                steps.push(step(i, kind::SLIDER, stored));
                states.push(next.clone());
                state = next;
            }
            for (i, s) in states.iter().enumerate() {
                assert_eq!(state_at(&steps, i).as_ref(), Some(s), "seed {seed}, state {i}");
            }
        }
    }

    /// Malicious patches (a manipulated table or backup): no reference to the whole state, no growth beyond the size
    /// and depth limits; replaying such a history counts as damaged and `reconcile` starts anew from the current state.
    #[test]
    fn malicious_patches_are_refused() {
        let big: Vec<Value> = (0..2000).map(|i| json!({ "x": i, "pad": "0123456789abcdef0123456789abcdef" })).collect();
        let s0 = json!({ "a": big, "b": 1 });
        // `$n 0` (the whole previous state) is never accepted
        assert!(apply(&s0, &json!({ "c": { "$n": 0 } })).is_err());
        assert!(apply(&s0, &json!({ "c": { "$a": { "n": 1, "add": [{ "$n": 0 }] } } })).is_err());
        // many references to a large node: refused once the copies exceed the limit
        let many: Vec<Value> = (0..10_000).map(|_| json!({ "$n": 1 })).collect();
        let p = json!({ "c": { "$a": { "n": 10_000, "add": many } } });
        assert!(apply(&s0, &p).is_err());
        // doubling per step through a history: stops at the limit, the replay is damaged (`None`), not huge
        let mut steps = vec![step(0, kind::START, s0.clone())];
        for i in 1..60 {
            // `a` becomes [a, a]: the old list twice
            steps.push(step(i, kind::SLIDER, json!({ "a": { "$a": { "n": 2, "c": { "0": { "$n": 1 }, "1": { "$n": 1 } } } } })));
        }
        assert!(state_at(&steps, 2).is_some());
        let t = std::time::Instant::now();
        assert_eq!(state_at(&steps, steps.len() - 1), None);
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
        // deeper than the limit: refused
        let mut deep = json!(1);
        for _ in 0..MAX_STATE_DEPTH + 2 {
            deep = json!({ "x": deep });
        }
        assert!(apply(&json!({}), &json!({ "d": { "$v": deep } })).is_err());
        let mut chain = json!(1);
        for _ in 0..MAX_STATE_DEPTH + 2 {
            chain = json!({ "y": chain });
        }
        assert!(apply(&json!({}), &chain).is_err());
        // reconcile: the stored state stays the truth, the history starts anew
        let current = json!({ "b": 2 });
        let mut bad = vec![step(0, kind::START, s0.clone()), step(1, kind::SLIDER, json!({ "c": { "$n": 0 } }))];
        let mut cursor = 1;
        let ops = reconcile(&mut bad, &mut cursor, &current, 0);
        assert!(matches!(ops.first(), Some(Op::ReplaceAll(_))), "{ops:?}");
        assert_eq!(state_at(&bad, cursor), Some(current));
        // normal references still work
        let s1 = json!({ "a": [{ "big": "0123456789abcdef0123456789abcdef0123456789abcdef" }], "b": [] });
        let s2 = json!({ "a": [], "b": [{ "big": "0123456789abcdef0123456789abcdef0123456789abcdef" }] });
        let p = diff(&s1, &s2);
        assert!(p.to_string().contains("$n"), "{p}");
        assert_eq!(apply(&s1, &p).unwrap(), s2);
    }

    /// The special forms: null as a value, null inside a fresh object, lists element-wise.
    #[test]
    fn special_values() {
        let a = json!({ "x": 1.0, "o": { "p": 1 }, "l": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] });
        let b = json!({ "x": null, "o": { "p": null, "q": [1] }, "l": [1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12], "n": { "m": null } });
        let p = diff(&a, &b);
        assert_eq!(apply(&a, &p).unwrap(), b, "{p}");
        let c = json!({ "x": 1.0 });
        assert_eq!(apply(&b, &diff(&b, &c)).unwrap(), c);
        assert_eq!(diff(&a, &a), json!({}));
    }

    /// A brush mask rebuilt as a balanced tree with every stroke: the patch of one more stroke stays small because
    /// the unchanged strokes are referenced.
    #[test]
    fn mask_with_200_strokes_patch_is_small() {
        fn stroke(i: usize) -> Value {
            let pts: Vec<Value> = (0..60).map(|j| json!([0.1 + i as f64 * 0.001 + j as f64 * 0.0007, 0.2 + j as f64 * 0.0011])).collect();
            json!({ "type": "brush", "points": pts, "radius": 0.05, "hardness": 0.5, "flow": 1.0 })
        }
        fn balance(v: &[Value]) -> Value {
            if v.len() == 1 {
                return v[0].clone();
            }
            let n = v.len() / 2;
            json!({ "type": "combine", "operation": "add", "left": balance(&v[..n]), "right": balance(&v[n..]) })
        }
        let settings = |n: usize| {
            let strokes: Vec<Value> = (0..n).map(stroke).collect();
            let mut s = sample();
            s["local"] = json!({ "version": 2, "layers": [ { "mask": balance(&strokes), "adjustments": { "exposure": 0.3 } } ] });
            s
        };
        let mut worst = 0usize;
        let mut sum = 0usize;
        let mut prev = settings(1);
        for n in 2..=200 {
            let next = settings(n);
            let p = diff(&prev, &next);
            assert_eq!(apply(&prev, &p).unwrap(), next);
            let len = patch_text(&p).len();
            worst = worst.max(len);
            sum += len;
            prev = next;
        }
        let whole = patch_text(&prev).len();
        let one_stroke = patch_text(&stroke(0)).len();
        eprintln!("mask 200 strokes: whole state {whole} B, one stroke {one_stroke} B, patch per stroke mean {} B, worst {worst} B", sum / 199);
        // measured: mean ≈ 2 strokes, worst ≈ 2 % of the whole state (the tree is restructured around the new stroke)
        assert!(sum / 199 < one_stroke * 3, "mean patch {} B (one stroke {one_stroke} B)", sum / 199);
        assert!(worst < whole / 20, "worst patch {worst} B (whole {whole} B)");
    }

    #[test]
    fn compaction_keeps_start_and_states() {
        let mut state = sample();
        let mut steps = vec![step(0, kind::START, state.clone())];
        let mut r = Rng(77);
        for i in 1..=130 {
            let mut next = state.clone();
            mutate(&mut r, &mut next, 0);
            steps.push(step(i, kind::SLIDER, diff(&state, &next)));
            state = next;
        }
        let top = steps.len() - 1;
        let (c, ci) = compact(&steps, top, MAX_STEPS, 2000).unwrap();
        assert_eq!(c.len(), MAX_STEPS);
        assert_eq!(c[0], steps[0], "start stays");
        assert_eq!(c[1].kind, kind::COMPACTED);
        assert_eq!(ci, c.len() - 1);
        assert_eq!(state_at(&c, ci), Some(state.clone()));
        assert_eq!(label_number(&c[1], "n"), Some(32));
        // the cursor further back limits what is folded: its state stays reachable
        let back = 20;
        let (c2, ci2) = compact(&steps, back, MAX_STEPS, 2000).unwrap();
        assert_eq!(state_at(&c2, ci2), state_at(&steps, back));
        assert_eq!(c2.last(), steps.last());
        // short enough: nothing to do; old steps (one year) are folded too
        assert!(compact(&steps[..50], 49, MAX_STEPS, 2000).is_none());
        let (c3, _) = compact(&steps[..50], 49, MAX_STEPS, 1040 + MAX_AGE).unwrap();
        assert_eq!(c3.len(), 50 - 39 + 1);
        assert_eq!(state_at(&c3, c3.len() - 1), state_at(&steps, 49));
    }

    #[test]
    fn reconcile_appends_external_step() {
        let a = sample();
        let mut b = a.clone();
        b["exposure"] = json!(0.5);
        let mut c = b.clone();
        c["contrast"] = json!(0.25);
        let mut steps = vec![step(0, kind::START, a.clone()), step(1, kind::SLIDER, diff(&a, &b)), step(2, kind::SLIDER, diff(&b, &c))];
        // fits: nothing
        let mut cursor = 2;
        assert!(reconcile(&mut steps, &mut cursor, &c, 5000).is_empty());
        // cursor on step 1, the state differs: steps after it go, `external` comes
        let mut d = b.clone();
        d["film"]["active"] = json!(true);
        let mut cursor = 1;
        let ops = reconcile(&mut steps, &mut cursor, &d, 5000);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[2].kind, kind::EXTERNAL);
        assert_eq!(cursor, 2);
        assert_eq!(state_at(&steps, 2), Some(d.clone()));
        assert!(matches!(ops[0], Op::TruncateAfter(1)));
        // damaged history: starts anew from the current state
        let mut bad = vec![step(5, kind::SLIDER, json!({ "x": 1 }))];
        let mut cursor = 0;
        reconcile(&mut bad, &mut cursor, &d, 5000);
        assert_eq!(state_at(&bad, 0), Some(d));
    }

    /// A command step per photo; its undo moves the cursor back, its redo forward; an undo after further edits is a
    /// step "undo" of its own.
    #[test]
    fn command_steps_and_their_undo() {
        let a = sample();
        let mut b = a.clone();
        b["exposure"] = json!(0.7);
        let label = json!({ "k": "paste", "from": "X.jpg" });
        let run = |steps: &mut Vec<Step>, cur: &mut Option<i64>, from: &Value, to: &Value, undo: bool| {
            let ops = plan_command(steps, *cur, from, to, kind::PASTE, &label, 42, undo, 3000);
            apply_ops_mem(steps, cur, &ops);
        };
        let (mut steps, mut cur) = (Vec::new(), None);
        run(&mut steps, &mut cur, &a, &b, false);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].command, Some(42));
        assert_eq!(state_at(&steps, cursor_index(&steps, cur)), Some(b.clone()));
        // undo: cursor back, the step stays
        run(&mut steps, &mut cur, &b, &a, true);
        assert_eq!((steps.len(), cur), (2, Some(0)));
        // redo: forward again
        run(&mut steps, &mut cur, &a, &b, false);
        assert_eq!((steps.len(), cur), (2, None));
        // edited since: the undo becomes a step of its own
        let mut c = b.clone();
        c["contrast"] = json!(0.3);
        steps.push(step(2, kind::SLIDER, diff(&b, &c)));
        let mut back = c.clone();
        back["exposure"] = json!(0.0);
        run(&mut steps, &mut cur, &c, &back, true);
        assert_eq!(steps.len(), 4);
        assert!(steps[3].label.as_deref().unwrap().contains("\"undo\""));
        assert_eq!(state_at(&steps, 3), Some(back));
    }

    #[test]
    fn compact_to_folds_up_to_a_step() {
        let a = sample();
        let mut steps = vec![step(0, kind::START, a.clone())];
        let mut state = a.clone();
        for i in 1..=6 {
            let mut n = state.clone();
            n["exposure"] = json!(i as f64 / 10.0);
            steps.push(step(i, kind::SLIDER, diff(&state, &n)));
            state = n;
        }
        let (c, ci) = compact_to(&steps, 6, 4).unwrap();
        assert_eq!(c.len(), 4);
        assert_eq!(ci, 3);
        assert_eq!(state_at(&c, 1), state_at(&steps, 4));
        assert_eq!(state_at(&c, 3), Some(state));
        assert!(compact_to(&steps, 3, 4).is_none(), "not past the cursor");
    }

    fn test_db(name: &str) -> (crate::db::Db, std::path::PathBuf, i64) {
        let p = std::env::temp_dir().join(format!("graineo-history-{name}-{}-{}.sqlite", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let db = crate::db::Db::open(&p).unwrap();
        let root = db.create_root("/tmp/graineo-history/", "h", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'x.jpg', 'JPG')", [folder]).unwrap();
        let file = db.conn.last_insert_rowid();
        db.conn.execute("INSERT INTO image(file) VALUES(?1)", [file]).unwrap();
        (db, p, file)
    }

    fn drop_db(db: crate::db::Db, p: std::path::PathBuf) {
        drop(db);
        for x in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{x}", p.display()));
        }
    }

    /// Operations give the same history in the table and in memory.
    #[test]
    fn ops_in_table_and_memory_agree() {
        let (db, p, file) = test_db("ops");
        let a = sample();
        let mut b = a.clone();
        b["exposure"] = json!(1.0);
        let ops = vec![
            Op::Put(step(0, kind::START, a.clone())),
            Op::Put(step(1, kind::SLIDER, diff(&a, &b))),
            Op::Put(step(2, kind::SLIDER, json!({ "contrast": 0.5 }))),
            Op::Cursor(Some(1)),
            Op::TruncateAfter(1),
            Op::Put(step(2, kind::PRESET, json!({ "contrast": 0.7 }))),
            Op::Cursor(None),
        ];
        apply_ops(&db.conn, file, Some("Kopie 1"), &ops).unwrap();
        let (mut mem, mut cur) = (Vec::new(), None);
        apply_ops_mem(&mut mem, &mut cur, &ops);
        let (stored, scur) = load(&db.conn, file, Some("Kopie 1")).unwrap();
        assert_eq!(stored, mem);
        assert_eq!(scur, cur);
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[2].kind, kind::PRESET);
        assert!(load(&db.conn, file, None).unwrap().0.is_empty(), "the original has its own history");
        drop_db(db, p);
    }

    /// Removing a virtual copy removes its history, the original's stays; removing the file removes all.
    #[test]
    fn removing_a_copy_removes_its_history() {
        let (db, p, file) = test_db("copy");
        db.conn.execute("INSERT INTO image(file, copy) VALUES(?1, 'Kopie 1')", [file]).unwrap();
        let copy_image = db.conn.last_insert_rowid();
        let a = sample();
        for c in [None, Some("Kopie 1")] {
            apply_ops(&db.conn, file, c, &[Op::Put(step(0, kind::START, a.clone())), Op::Put(step(1, kind::SLIDER, json!({ "exposure": 1 }))), Op::Cursor(Some(0))]).unwrap();
        }
        db.remove_images(&[copy_image], &[]).unwrap();
        assert!(load(&db.conn, file, Some("Kopie 1")).unwrap().0.is_empty());
        let n: i64 = db.conn.query_row("SELECT count(*) FROM develop_cursor WHERE copy = 'Kopie 1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert_eq!(load(&db.conn, file, None).unwrap().0.len(), 2);
        // a new copy of the same name starts without a history
        let original: i64 = db.conn.query_row("SELECT id FROM image WHERE file = ?1 AND copy IS NULL", [file], |r| r.get(0)).unwrap();
        db.remove_images(&[original], &[]).unwrap();
        assert_eq!(counts(&db.conn).unwrap(), (0, 0));
        drop_db(db, p);
    }

    /// Merging files: a copy's cursor goes only with its steps; a target that keeps its own steps keeps its own cursor
    /// (or none), a stray cursor of the target without steps gives way.
    #[test]
    fn merge_moves_cursor_only_with_the_steps() {
        let (db, p, s) = test_db("merge");
        let folder: i64 = db.conn.query_row("SELECT folder FROM file WHERE id = ?1", [s], |r| r.get(0)).unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'y.jpg', 'JPG')", [folder]).unwrap();
        let t = db.conn.last_insert_rowid();
        let a = sample();
        let two = |c: &str| vec![Op::Put(step(0, kind::START, a.clone())), Op::Put(step(1, kind::SLIDER, json!({ "exposure": 1 }))), Op::Put(step(2, kind::SLIDER, json!({ "exposure": 2 }))), Op::Cursor(Some(if c == "K1" { 0 } else { 1 }))];
        // K1: both have steps, only the source a cursor; K2: only the source has steps, the target a stray cursor
        apply_ops(&db.conn, s, Some("K1"), &two("K1")).unwrap();
        apply_ops(&db.conn, t, Some("K1"), &[Op::Put(step(0, kind::START, a.clone()))]).unwrap();
        apply_ops(&db.conn, s, Some("K2"), &two("K2")).unwrap();
        db.conn.execute("INSERT INTO develop_cursor(file, copy, seq) VALUES(?1, 'K2', 7)", [t]).unwrap();
        merge_file(&db.conn, s, t, false).unwrap();
        let (k1, c1) = load(&db.conn, t, Some("K1")).unwrap();
        assert_eq!((k1.len(), c1), (1, None), "the target's own history, no foreign cursor");
        let (k2, c2) = load(&db.conn, t, Some("K2")).unwrap();
        assert_eq!((k2.len(), c2), (3, Some(1)), "steps and their cursor together");
        drop_db(db, p);
    }

    /// The total limit compacts the histories touched longest ago to start + current state; recent ones stay.
    #[test]
    fn total_limit_compacts_oldest() {
        let (db, p, file) = test_db("total");
        let a = sample();
        for (n, c) in ["A", "B", "C"].iter().enumerate() {
            let mut state = a.clone();
            let mut ops = vec![Op::Put(Step { time: 100 * n as i64, ..step(0, kind::START, a.clone()) })];
            for i in 1..=40 {
                let mut next = state.clone();
                next["exposure"] = json!(i as f64 / 10.0);
                next["film"]["stock"] = json!(format!("stock-{c}-{i}-{}", "x".repeat(40)));
                ops.push(Op::Put(Step { time: 100 * n as i64 + i, ..step(i, kind::SLIDER, diff(&state, &next)) }));
                state = next;
            }
            apply_ops(&db.conn, file, Some(c), &ops).unwrap();
        }
        let before = total_bytes(&db.conn).unwrap();
        let expected_c = state_at(&load(&db.conn, file, Some("C")).unwrap().0, 40).unwrap();
        let a_top = state_at(&load(&db.conn, file, Some("A")).unwrap().0, 40).unwrap();
        // limit: about two thirds; "C" is protected (touched recently)
        let done = enforce_total(&db.conn, before * 2 / 3, 200).unwrap();
        assert!(done >= 1);
        let (sa, _) = load(&db.conn, file, Some("A")).unwrap();
        assert_eq!(sa.len(), 2);
        assert_eq!(state_at(&sa, 1), Some(a_top));
        assert_eq!(load(&db.conn, file, Some("C")).unwrap().0.len(), 41);
        assert_eq!(state_at(&load(&db.conn, file, Some("C")).unwrap().0, 40), Some(expected_c));
        assert!(total_bytes(&db.conn).unwrap() < before);
        drop_db(db, p);
    }

    /// Review 1.2.0, N2: merging file rows moves a copy's cursor only together with its steps.
    #[test]
    fn merge_file_moves_cursor_only_with_steps() {
        let (db, p, s_file) = test_db("merge-n2");
        let folder: i64 = db.conn.query_row("SELECT folder FROM file WHERE id = ?1", [s_file], |r| r.get(0)).unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'y.jpg', 'JPG')", [folder]).unwrap();
        let t_file = db.conn.last_insert_rowid();
        let a = sample();
        let three = |c: &str, f: i64| apply_ops(&db.conn, f, Some(c), &[Op::Put(step(0, kind::START, a.clone())), Op::Put(step(1, kind::SLIDER, json!({ "exposure": 1 }))), Op::Put(step(2, kind::SLIDER, json!({ "exposure": 2 })))]).unwrap();
        // "K1": both have steps, only `s` has a cursor; "K2": only `s` has a history
        three("K1", s_file);
        three("K1", t_file);
        apply_ops(&db.conn, s_file, Some("K1"), &[Op::Cursor(Some(0))]).unwrap();
        three("K2", s_file);
        apply_ops(&db.conn, s_file, Some("K2"), &[Op::Cursor(Some(1))]).unwrap();
        merge_file(&db.conn, s_file, t_file, false).unwrap();
        assert_eq!(load(&db.conn, t_file, Some("K1")).unwrap().1, None, "the cursor of s stays away from the steps of t");
        assert_eq!(load(&db.conn, t_file, Some("K2")).unwrap(), (load(&db.conn, t_file, Some("K2")).unwrap().0, Some(1)));
        assert_eq!(load(&db.conn, t_file, Some("K2")).unwrap().0.len(), 3);
        drop_db(db, p);
    }

    /// Regression (review 1.2.0, N1): a history the app holds in memory is never compacted by the total limit (the app
    /// would go on with its long list); after the hold is dropped it is.
    #[test]
    fn total_limit_skips_held_histories() {
        let (db, p, file) = test_db("held");
        let a = sample();
        for (n, c) in ["held-A", "held-B"].iter().enumerate() {
            let mut state = a.clone();
            let mut ops = vec![Op::Put(Step { time: 100 * n as i64, ..step(0, kind::START, a.clone()) })];
            for i in 1..=40 {
                let mut next = state.clone();
                next["film"]["stock"] = json!(format!("stock-{c}-{i}-{}", "x".repeat(40)));
                ops.push(Op::Put(Step { time: 100 * n as i64 + i, ..step(i, kind::SLIDER, diff(&state, &next)) }));
                state = next;
            }
            apply_ops(&db.conn, file, Some(c), &ops).unwrap();
        }
        let before = total_bytes(&db.conn).unwrap();
        let h = hold(file, Some("held-A"));
        // a tiny limit: everything not held would go
        enforce_total(&db.conn, before / 10, 1000).unwrap();
        assert_eq!(load(&db.conn, file, Some("held-A")).unwrap().0.len(), 41, "held: untouched");
        assert_eq!(load(&db.conn, file, Some("held-B")).unwrap().0.len(), 2, "not held: compacted");
        drop(h);
        enforce_total(&db.conn, before / 10, 1000).unwrap();
        assert_eq!(load(&db.conn, file, Some("held-A")).unwrap().0.len(), 2, "after the hold: compacted");
        drop_db(db, p);
    }
}
