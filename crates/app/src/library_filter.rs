//! Column filters of the library (facets): camera, lens, year, keyword, AI quality,
//! format, label, stars with multi-selection and hit counts that restrict each
//! other; number lists for aperture, ISO (one row per value), focal length and shutter speed (groups), each
//! row with its count and a small bar.
//!
//! Counting: for every row a bit field of the columns whose selection it does not
//! satisfy is built. A row counts in column c if it satisfies all *other* columns – so
//! each column shows what a further selection there would yield.
//! Own implementation.

use crate::menu::i18n::DeComma as _;
use std::collections::{BTreeSet, HashMap, HashSet};

use library::db::{Keyword, Row};
use eframe::egui;
use egui::Color32;

use crate::lr;

#[path = "keyword_tree.rs"]
mod keyword_tree;

#[path = "library_facets_build.rs"]
mod facets_build;
pub use facets_build::inputs_fingerprint;

#[cfg(test)]
#[path = "library_facets_bench.rs"]
mod facets_bench;

pub const CATEGORIES: [&str; N] = ["Kamera", "Objektiv", "Jahr", "Stichwort", "KI-Qualität", "Format", "Etikett", "Sterne", "Person", "Monat", "Tag im Monat", "Wochentag", "Tageszeit", "Ort"];
/// Number of columns
const N: usize = 14;
const WEEKDAY_COL: usize = 11;
const TIME_COL: usize = 12;
const KEYWORD_COL: usize = 3;
const PERSON_COL: usize = 8;
const PLACE_COL: usize = 13;
pub const RANGES: [&str; 4] = ["Blende", "Brennweite", "ISO", "Zeit"];
const NO_VALUE: u32 = u32::MAX;
/// Height of the columns; below that a row for filters on attributes not currently in any column
pub const HEIGHT: f32 = 150.0;
/// Minimum width of a column: it determines how many (3 to 6) fit side by side
const MIN_COLUMN_W: f32 = 170.0;

/// What a column shows: an attribute with a value list or a range slider (aperture, focal length, ISO, shutter speed).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Cat(usize),
    Range(usize),
}

/// All selectable contents in the order of the selection menu
const ALL_SLOTS: [Slot; N + 4] = [
    Slot::Cat(0),
    Slot::Cat(1),
    Slot::Cat(2),
    Slot::Cat(3),
    Slot::Cat(8),
    Slot::Cat(4),
    Slot::Cat(5),
    Slot::Cat(6),
    Slot::Cat(7),
    Slot::Range(0),
    Slot::Range(1),
    Slot::Range(2),
    Slot::Range(3),
    Slot::Cat(9),
    Slot::Cat(10),
    Slot::Cat(11),
    Slot::Cat(12),
    Slot::Cat(PLACE_COL),
];
/// IDs for `settings.json` (`metadata_columns`), same order as [`ALL_SLOTS`]
const SLOT_IDS: [&str; N + 4] = ["camera", "lens", "year", "keyword", "person", "quality", "format", "label", "stars", "aperture", "focal_length", "iso", "shutter", "month", "day", "weekday", "time", "place"];
const DEFAULT_SLOTS: [Slot; 4] = [Slot::Cat(0), Slot::Cat(1), Slot::Cat(2), Slot::Cat(3)];

impl Slot {
    fn title(self) -> &'static str {
        match self {
            Slot::Cat(k) => CATEGORIES[k],
            Slot::Range(r) => RANGES[r],
        }
    }
    fn slot_id(self) -> &'static str {
        ALL_SLOTS.iter().position(|s| *s == self).map_or("", |i| SLOT_IDS[i])
    }
    fn from_slot_id(k: &str) -> Option<Slot> {
        SLOT_IDS.iter().position(|x| *x == k).map(|i| ALL_SLOTS[i])
    }
}

/// The menu follows the currently displayed language; persisted IDs keep their stable order.
fn sorted_slots() -> Vec<Slot> {
    let mut slots = ALL_SLOTS.to_vec();
    slots.sort_by_cached_key(|s| library::search::fold_text(crate::tr(s.title())));
    slots
}

/// Read the remembered column choice (without duplicates; empty = default).
fn load_slots() -> Vec<Slot> {
    let mut v: Vec<Slot> = Vec::new();
    if let Some(a) = library::settings::value("metadata_columns").as_array() {
        for x in a {
            if let Some(sl) = x.as_str().and_then(Slot::from_slot_id) {
                if !v.contains(&sl) {
                    v.push(sl);
                }
            }
        }
    }
    if v.is_empty() {
        v = DEFAULT_SLOTS.to_vec();
    }
    v
}

/// One row of a number list (aperture, focal length, ISO, shutter speed): a single value.
/// `search`: the same selection in the app's search syntax (smart collections), exact for this value.
#[derive(Clone, Debug, PartialEq)]
struct Band {
    text: String,
    search: String,
}

impl Band {
    /// Language-independent name (for remembering the selection across a rebuild).
    fn key(&self) -> &str {
        &self.search
    }
}

const FIELDS: [&str; 4] = ["aperture", "focal", "iso", "shutter"];

fn range_value(z: &Row, k: usize) -> Option<f64> {
    match k {
        0 => z.aperture,
        1 => z.focal,
        2 => z.iso.map(|v| v as f64),
        _ => z.time,
    }
}

/// Key of the row a value belongs to in number list `k` (the same quantisation as the exact search).
fn band_key(k: usize, v: f64) -> i64 {
    library::search::value_key(FIELDS[k], v)
}

fn band_of(k: usize, key: i64) -> Band {
    let field = FIELDS[k];
    let (text, value) = match k {
        0 => {
            let t = trim(key as f64 / 10.0);
            (format!("f/{t}"), t)
        }
        1 => {
            let t = trim(key as f64 / 10.0);
            (format!("{t} mm"), t)
        }
        2 => (key.to_string(), key.to_string()),
        _ => {
            if key > 0 {
                (format!("1/{key}"), format!("1/{key}"))
            } else {
                let t = trim(-key as f64 / 10.0);
                (format!("{t} s"), t)
            }
        }
    };
    Band { text, search: format!("{field}:{}", value.replace(',', ".")) }
}

pub fn value_text(k: usize, v: f64) -> String {
    let t = band_of(k, band_key(k, v)).text;
    if k == 2 { format!("ISO {t}") } else { t }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.trim_end_matches('0').trim_end_matches('.').de_comma()
}

/// Click on a row of a list (all columns alike): a plain click selects only this value (again on the only
/// selected one: clears), ⌘-click adds or removes it, ⇧-click selects the range from the last clicked row.
pub(crate) fn list_click(selection: &mut BTreeSet<u32>, order: &[u32], anchor: &mut Option<u32>, value: u32, mods: egui::Modifiers) {
    if mods.shift {
        let from = anchor.and_then(|a| order.iter().position(|x| *x == a));
        if let (Some(a), Some(b)) = (from, order.iter().position(|x| *x == value)) {
            if !mods.command {
                selection.clear();
            }
            selection.extend(order[a.min(b)..=a.max(b)].iter().copied());
            return;
        }
    }
    if mods.command {
        if !selection.remove(&value) {
            selection.insert(value);
        }
    } else if selection.len() == 1 && selection.contains(&value) {
        selection.clear();
    } else {
        selection.clear();
        selection.insert(value);
    }
    *anchor = Some(value);
}

/// Row height of the value lists.
pub(crate) const ROW_H: f32 = 17.0;

/// Selection captured by name ([`Facets::remember`]): columns, then number lists.
pub type Remembered = (Vec<(usize, String)>, Vec<(usize, String)>);

pub struct Facets {
    /// Value names per column (keyword: names of the keywords)
    names: [Vec<String>; N],
    /// per row the value index per column (keyword unused)
    values: Vec<[u32; N]>,
    /// per row the keywords (index into `names[3]`)
    kw: Vec<Vec<u32>>,
    /// per row the people (index into `names[8]`)
    persons: Vec<Vec<u32>>,
    pub selection: [BTreeSet<u32>; N],
    /// Hit counts per column (value, count), sorted
    pub numbers: [Vec<(u32, usize)>; N],
    /// Rows of the number lists (aperture, focal length, ISO, shutter speed), ascending
    bands: [Vec<Band>; 4],
    /// per photo the row in each number list (`NO_VALUE`: no value)
    band_rows: Vec<[u32; 4]>,
    /// selected rows per number list
    band_selection: [BTreeSet<u32>; 4],
    /// hit counts per number list (row, count) in list order, all rows (also those with 0)
    band_numbers: [Vec<(u32, usize)>; 4],
    /// last clicked row per list (for ⇧-click): columns first, then the number lists
    anchor: [Option<u32>; N + 4],
    /// Search text per column: hides values that do not contain it (selected ones stay visible)
    search: [String; N],
    /// Column contents from left to right (more than shown stays remembered)
    slots: Vec<Slot>,
    /// keyword column: keyword ID and "has a parent" per value (the keywords are managed here)
    kw_ids: Vec<(i64, bool)>,
    /// keyword column: the parent term per value (index into `names[3]`), for the hierarchy (Orte › Italien › Rom)
    kw_parent: Vec<Option<u32>>,
    /// keyword column: photos carrying the term itself (whole library, without the filter)
    kw_direct: Vec<usize>,
    /// keyword column: values whose name occurs more than once (ignoring case): shown with their path
    kw_ambiguous: HashSet<u32>,
    /// chosen in the context menu of a keyword row, taken by the library ([`Facets::take_keyword_request`])
    keyword_request: Option<(i64, KeywordAction)>,
    /// [`inputs_fingerprint`] of what these filters were built from (0: not known)
    pub fingerprint: u64,
}

/// Management of a keyword from the context menu of the keyword column (formerly in the left panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeywordAction {
    Rename,
    Move,
    ToTop,
    Delete,
    /// a new term below this one (dialog "Stichwörter ordnen …")
    NewChild,
    /// open "Stichwörter ordnen …" on this keyword
    Arrange,
}

impl Facets {
    /// Build from all rows and the keyword assignments (image → keyword IDs); see `library_facets_build.rs`.
    pub fn build(rows: &[Row], keywords: &[Keyword], keywords_per_image: &HashMap<i64, Vec<i64>>) -> Self {
        Self::build_timed(rows, keywords, keywords_per_image).0
    }

    /// Number of rows these filters were built for (they only fit a list of exactly this length).
    pub fn rows_len(&self) -> usize {
        self.values.len()
    }

    /// A keyword action chosen in the keyword column since the last call.
    pub fn take_keyword_request(&mut self) -> Option<(i64, KeywordAction)> {
        self.keyword_request.take()
    }

    /// Select a column value by its name (test helper).
    pub fn choose_name(&mut self, k: usize, name: &str) {
        if let Some(i) = self.names[k].iter().position(|n| n == name) {
            self.selection[k].insert(i as u32);
        }
    }

    /// Select the rows of number list `k` (0 aperture, 1 focal length, 2 ISO, 3 shutter) whose values lie in `a`…`e`
    /// (test helper `GRAINEO_LIBRARY_TEST`, formerly a range on the slider).
    pub fn choose_range(&mut self, rows: &[Row], k: usize, a: f64, e: f64) {
        for (z, b) in rows.iter().zip(&self.band_rows) {
            if b[k] != NO_VALUE && range_value(z, k).is_some_and(|v| v >= a * 0.999 && v <= e * 1.001) {
                self.band_selection[k].insert(b[k]);
            }
        }
    }

    /// Capture selection and ranges by name (value IDs change on rebuild).
    pub fn remember(&self) -> Remembered {
        let mut v = Vec::new();
        for (k, a) in self.selection.iter().enumerate() {
            for &i in a {
                if k == KEYWORD_COL && (i as usize) < self.names[k].len() {
                    // by path: a name in two branches ("Orte › Paris", "Personen › Paris") stays the one chosen
                    v.push((k, self.kw_path(i)));
                } else if let Some(n) = self.names[k].get(i as usize) {
                    v.push((k, n.clone()));
                } else if i == NO_VALUE {
                    v.push((k, String::from("\u{0}ohne")));
                }
            }
        }
        let mut b = Vec::new();
        for (k, a) in self.band_selection.iter().enumerate() {
            for &i in a {
                b.push((k, if i == NO_VALUE { String::from("\u{0}ohne") } else { self.bands[k][i as usize].key().to_string() }));
            }
        }
        (v, b)
    }

    /// Restore the selection captured with [`Self::remember`] (names that no longer exist are dropped).
    pub fn restore(&mut self, g: &Remembered) {
        for (k, n) in &g.0 {
            if n == "\u{0}ohne" {
                self.selection[*k].insert(NO_VALUE);
            } else if *k == KEYWORD_COL {
                match (0..self.names[*k].len() as u32).find(|v| self.kw_path(*v) == *n) {
                    Some(v) => {
                        self.selection[*k].insert(v);
                    }
                    None => self.choose_name(*k, n),
                }
            } else {
                self.choose_name(*k, n);
            }
        }
        for (k, n) in &g.1 {
            if n == "\u{0}ohne" {
                self.band_selection[*k].insert(NO_VALUE);
            } else if let Some(i) = self.bands[*k].iter().position(|b| b.key() == n) {
                self.band_selection[*k].insert(i as u32);
            }
        }
    }

    pub fn active(&self) -> bool {
        self.selection.iter().any(|a| !a.is_empty()) || self.band_selection.iter().any(|b| !b.is_empty())
    }

    pub fn reset(&mut self) {
        self.selection = Default::default();
        self.band_selection = Default::default();
    }

    /// The filtering columns for the filter button: (title as translation key, chosen values in display form).
    /// Order: the columns shown in the panel from left to right, then the others in menu order.
    pub fn active_columns(&self) -> Vec<(&'static str, String)> {
        let mut order: Vec<Slot> = self.slots.clone();
        order.extend(ALL_SLOTS.iter().copied().filter(|s| !self.slots.contains(s)));
        order
            .into_iter()
            .filter(|s| self.slot_active(*s))
            .map(|s| {
                let values: Vec<String> = match s {
                    Slot::Cat(k) => self.selection[k].iter().map(|v| if k == KEYWORD_COL && self.kw_ambiguous.contains(v) { self.kw_path(*v) } else { self.display_name(k, *v) }).collect(),
                    Slot::Range(r) => self.band_selection[r].iter().map(|v| self.band_name(r, *v)).collect(),
                };
                (s.title(), values_text(values))
            })
            .collect()
    }
}

/// Values of a column for a tooltip: at most eight, then "…".
fn values_text(values: Vec<String>) -> String {
    let more = values.len() > 8;
    let mut t = values.into_iter().take(8).collect::<Vec<_>>().join(", ");
    if more {
        t.push_str(", …");
    }
    t
}

/// Like [`Facets::active_columns`] for a selection waiting for its rebuilt filters (the button keeps showing it).
pub fn remembered_columns(r: &Remembered) -> Vec<(&'static str, String)> {
    // number lists remember their search form (`iso:200`): the value after the field
    let shown = |n: &str, band: bool| if n == "\u{0}ohne" { crate::tr("(ohne)").to_string() } else if band { n.split_once(':').map_or(n, |(_, v)| v).to_string() } else { n.to_string() };
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut add = |title: &'static str, value: String| match out.iter_mut().find(|(t, _)| *t == title) {
        Some((_, v)) => {
            v.push_str(", ");
            v.push_str(&value);
        }
        None => out.push((title, value)),
    };
    for (k, n) in &r.0 {
        add(CATEGORIES[*k], shown(n, false));
    }
    for (k, n) in &r.1 {
        add(RANGES[*k], shown(n, true));
    }
    out
}

impl Facets {

    /// Rows (indices) from `basis` that satisfy all columns; updates the counts of all lists.
    pub fn apply(&mut self, _rows: &[Row], basis: &[usize]) -> Vec<usize> {
        let mut counts: [HashMap<u32, usize>; N] = Default::default();
        let mut band_counts: [HashMap<u32, usize>; 4] = Default::default();
        let mut band_seen: [bool; 4] = [false; 4];
        let mut out = Vec::with_capacity(basis.len());
        for &i in basis {
            let w = &self.values[i];
            let mut misses: u32 = 0;
            for k in 0..N {
                if self.selection[k].is_empty() {
                    continue;
                }
                let ok = match self.row_list(k, i) {
                    Some(l) => l.iter().any(|s| self.selection[k].contains(s)),
                    None => self.selection[k].contains(&w[k]),
                };
                if !ok {
                    misses |= 1 << k;
                }
            }
            let b = &self.band_rows[i];
            for r in 0..4 {
                if !self.band_selection[r].is_empty() && !self.band_selection[r].contains(&b[r]) {
                    misses |= 1 << (N + r);
                }
            }
            for k in 0..N {
                if misses & !(1 << k) == 0 {
                    if let Some(l) = self.row_list(k, i) {
                        for s in l {
                            *counts[k].entry(*s).or_default() += 1;
                        }
                    } else {
                        *counts[k].entry(w[k]).or_default() += 1;
                    }
                }
            }
            for r in 0..4 {
                if b[r] == NO_VALUE {
                    band_seen[r] = true;
                }
                if misses & !(1 << (N + r)) == 0 {
                    *band_counts[r].entry(b[r]).or_default() += 1;
                }
            }
            if misses == 0 {
                out.push(i);
            }
        }
        for k in 0..N {
            let mut v: Vec<(u32, usize)> = counts[k].drain().collect();
            // always show selected values (even with 0 hits) so they can be deselected
            for &a in &self.selection[k] {
                if !v.iter().any(|(x, _)| *x == a) {
                    v.push((a, 0));
                }
            }
            self.sort_values(k, &mut v);
            // Keywords and people can be very many; the column search also finds rare ones
            v.truncate(5000);
            self.numbers[k] = v;
        }
        // number lists: always all rows in ascending order (a row with 0 hits stays dimmed in its place, nothing jumps)
        for r in 0..4 {
            let mut v: Vec<(u32, usize)> = (0..self.bands[r].len() as u32).map(|i| (i, band_counts[r].get(&i).copied().unwrap_or(0))).collect();
            if band_seen[r] || self.band_selection[r].contains(&NO_VALUE) {
                v.push((NO_VALUE, band_counts[r].get(&NO_VALUE).copied().unwrap_or(0)));
            }
            self.band_numbers[r] = v;
        }
        out
    }

    /// Order of the values in a column: camera, lens, format and person by name, year newest first, label, stars and
    /// AI quality in fixed order, person by name, keyword by frequency. "(none)" is always last.
    fn sort_values(&self, k: usize, v: &mut [(u32, usize)]) {
        if k == KEYWORD_COL {
            self.sort_tree(v);
            return;
        }
        let rank = |w: u32| -> usize {
            let n = self.name(k, w).to_lowercase();
            match k {
                4 => ["gut", "mittel", "ausschuss"].iter().position(|x| *x == n).unwrap_or(3),
                6 => [["rot", "red"], ["gelb", "yellow"], ["grün", "green"], ["blau", "blue"], ["lila", "purple"], ["keine", "none"]].iter().position(|x| x.contains(&n.as_str())).unwrap_or(6),
                7 => 5 - n.chars().filter(|c| *c == '★').count().min(5),
                // morning, day, evening, night
                TIME_COL => ["1", "2", "3", "0"].iter().position(|x| *x == n).unwrap_or(4),
                _ => 0,
            }
        };
        v.sort_by(|a, b| {
            let (ao, bo) = (a.0 == NO_VALUE, b.0 == NO_VALUE);
            ao.cmp(&bo).then_with(|| match k {
                0 | 1 | 5 | PERSON_COL => self.name(k, a.0).to_lowercase().cmp(&self.name(k, b.0).to_lowercase()),
                2 => self.name(k, b.0).cmp(&self.name(k, a.0)),
                9 | 10 | WEEKDAY_COL => self.name(k, a.0).cmp(&self.name(k, b.0)),
                4 | 6 | 7 | TIME_COL => rank(a.0).cmp(&rank(b.0)),
                _ => b.1.cmp(&a.1).then_with(|| self.name(k, a.0).to_lowercase().cmp(&self.name(k, b.0).to_lowercase())),
            })
        });
    }

    /// Keyword column as a tree: every term directly below its parent, siblings by frequency (then by name).
    /// "(none)" last.
    fn sort_tree(&self, v: &mut [(u32, usize)]) {
        let count: HashMap<u32, usize> = v.iter().copied().collect();
        let key = |w: u32| -> Vec<(std::cmp::Reverse<usize>, String, u32)> {
            self.kw_chain(w).into_iter().map(|x| (std::cmp::Reverse(count.get(&x).copied().unwrap_or(0)), self.name(KEYWORD_COL, x).to_lowercase(), x)).collect()
        };
        v.sort_by_cached_key(|(w, _)| (*w == NO_VALUE, if *w == NO_VALUE { Vec::new() } else { key(*w) }));
    }

    /// A keyword with its parents, from the top down (`[Orte, Italien, Rom]`).
    fn kw_chain(&self, w: u32) -> Vec<u32> {
        let mut chain = vec![w];
        let mut cur = self.kw_parent.get(w as usize).copied().flatten();
        while let Some(p) = cur {
            if chain.contains(&p) || chain.len() > 64 {
                break;
            }
            chain.push(p);
            cur = self.kw_parent.get(p as usize).copied().flatten();
        }
        chain.reverse();
        chain
    }

    /// Shown path of a keyword value ("Orte › Italien › Rom").
    fn kw_path(&self, w: u32) -> String {
        self.kw_chain(w).into_iter().map(|x| self.name(KEYWORD_COL, x)).collect::<Vec<_>>().join(library::keywords::SHOWN_SEPARATOR)
    }

    /// The list of values of a row for columns with several values per image (keyword, person).
    fn row_list(&self, k: usize, i: usize) -> Option<&[u32]> {
        match k {
            KEYWORD_COL => Some(&self.kw[i]),
            PERSON_COL => Some(&self.persons[i]),
            _ => None,
        }
    }

    fn name(&self, k: usize, w: u32) -> String {
        if w == NO_VALUE {
            crate::tr("(ohne)").into()
        } else {
            self.names[k].get(w as usize).cloned().unwrap_or_default()
        }
    }

    fn display_name(&self, k: usize, value: u32) -> String {
        let name = self.name(k, value);
        if value == NO_VALUE {
            return name;
        }
        match k {
            9 => crate::library_ui::month_name(&format!("0000-{name}")),
            WEEKDAY_COL => weekday_name(name.parse().unwrap_or(0)).to_string(),
            TIME_COL => time_of_day_name(name.parse().unwrap_or(9)).to_string(),
            1 => crate::cell_info::lens_label(&name),
            _ => name,
        }
    }

    /// Selection as search text in the app's own syntax (for smart collections). Text columns (camera, lens, keyword,
    /// format, person) are written as the exact comparison `field:="value"`, so the rule finds the same photos as the
    /// filter (no partial matches: "Canon EOS R" is not "Canon EOS R5"). `Err`: the selection cannot be written as a
    /// rule (the choice "(ohne)" has no counterpart in the search syntax; a name with a quotation mark cannot be quoted).
    pub fn as_search(&self) -> Result<String, String> {
        let mut parts = Vec::new();
        // English base forms of the search fields
        let keys = ["camera", "lens", "year", "keyword", "ai", "format", "label", "rating", "person", "month", "day", "weekday", "time", "place"];
        const EXACT_COLS: [usize; 6] = [0, 1, KEYWORD_COL, 5, PERSON_COL, PLACE_COL];
        for k in 0..N {
            if self.selection[k].contains(&NO_VALUE) {
                return Err(crate::tr("„(ohne)“ lässt sich nicht als Regel sichern").to_string());
            }
            let mut values: Vec<String> = Vec::new();
            for w in &self.selection[k] {
                let n = self.name(k, *w);
                let n = match k {
                    // a keyword name in several branches: the path names exactly the one chosen
                    KEYWORD_COL if self.kw_ambiguous.contains(w) => self.kw_chain(*w).into_iter().map(|x| self.name(KEYWORD_COL, x)).collect::<Vec<_>>().join(" > "),
                    4 | 6 => n.to_lowercase(),
                    7 if n == "ohne" => "0".into(),
                    7 => n.chars().count().to_string(),
                    9 | 10 => n.trim_start_matches('0').to_string(),
                    TIME_COL => library::search_fields::TIME_OF_DAY.get(n.parse::<usize>().unwrap_or(9)).copied().unwrap_or("day").to_string(),
                    _ => n,
                };
                if EXACT_COLS.contains(&k) {
                    if n.contains('"') {
                        return Err(crate::trf!("„{a}“ enthält ein Anführungszeichen und lässt sich nicht als Regel sichern", a = n));
                    }
                    values.push(format!("{}:=\"{n}\"", keys[k]));
                } else {
                    let n = if n.chars().any(|c| c.is_whitespace() || c == '|' || c == '"') { format!("\"{}\"", n.replace('"', "")) } else { n };
                    values.push(format!("{}:{n}", keys[k]));
                }
            }
            if !values.is_empty() {
                parts.push(values.join(" | "));
            }
        }
        for (r, a) in self.band_selection.iter().enumerate() {
            if a.contains(&NO_VALUE) {
                return Err(crate::tr("„(ohne)“ lässt sich nicht als Regel sichern").to_string());
            }
            let values: Vec<&str> = a.iter().map(|i| self.bands[r][*i as usize].search.as_str()).collect();
            if !values.is_empty() {
                parts.push(values.join(" | "));
            }
        }
        Ok(parts.join(" "))
    }

    fn slot_active(&self, slot: Slot) -> bool {
        match slot {
            Slot::Cat(k) => !self.selection[k].is_empty(),
            Slot::Range(r) => !self.band_selection[r].is_empty(),
        }
    }

    fn clear_slot(&mut self, slot: Slot) {
        match slot {
            Slot::Cat(k) => self.selection[k].clear(),
            Slot::Range(r) => self.band_selection[r].clear(),
        }
    }

    /// Fill at least `n` columns: fill missing ones with the first content not yet shown.
    fn fill_slots(&mut self, n: usize) {
        while self.slots.len() < n {
            let Some(&free_slot) = ALL_SLOTS.iter().find(|s| !self.slots.contains(s)) else { break };
            self.slots.push(free_slot);
        }
    }

    /// Column `i` now shows `new_val`; if it was already shown elsewhere, the two swap places.
    fn set_slot(&mut self, i: usize, new_val: Slot) {
        match self.slots.iter().position(|s| *s == new_val) {
            Some(j) => self.slots.swap(i, j),
            None => self.slots[i] = new_val,
        }
        let ids: Vec<&str> = self.slots.iter().map(|s| s.slot_id()).collect();
        library::settings::set("metadata_columns", serde_json::json!(ids));
    }

    /// Draw columns: 3 to 6 equally wide columns across the full width (no horizontal scrolling), at the top
    /// of each column you choose what it shows. Returns: selection changed.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let gap = 6.0;
        let total = ui.available_width();
        let n = (((total + gap) / (MIN_COLUMN_W + gap)).floor() as usize).clamp(3, 6);
        self.fill_slots(n);
        let w = ((total - gap * (n as f32 - 1.0)) / n as f32).floor();
        let mut swap: Option<(usize, Slot)> = None;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for i in 0..n {
                changed |= self.column(ui, i, w, &mut swap);
            }
        });
        if let Some((i, new_val)) = swap {
            self.set_slot(i, new_val);
        }
        // Filters on attributes that are currently in no column stay visible and can be cleared
        let hidden: Vec<Slot> = ALL_SLOTS.iter().copied().filter(|s| !self.slots[..n].contains(s) && self.slot_active(*s)).collect();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.set_height(16.0);
            if !hidden.is_empty() {
                ui.label(egui::RichText::new(crate::tr("Weitere Filter:")).size(10.5).color(lr::TEXT_DIM()));
                for sl in hidden {
                    if ui.add(egui::Button::new(format!("{} ×", crate::tr(sl.title()))).small()).clicked() {
                        self.clear_slot(sl);
                        changed = true;
                    }
                }
            }
        });
        changed
    }

    fn column(&mut self, ui: &mut egui::Ui, i: usize, w: f32, swap: &mut Option<(usize, Slot)>) -> bool {
        let slot = self.slots[i];
        let mut changed = false;
        let (frame, _) = ui.allocate_exact_size(egui::vec2(w, HEIGHT), egui::Sense::hover());
        ui.painter().rect_filled(frame, 0.0, Color32::from_gray(0x16));
        let mut kind = ui.new_child(egui::UiBuilder::new().max_rect(frame.shrink2(egui::vec2(4.0, 3.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut kind;
        let active = self.slot_active(slot);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("bib-facette-wahl", i))
                .selected_text(crate::tr(slot.title()))
                .width(w - if active { 40.0 } else { 14.0 })
                .show_ui(ui, |ui| {
                    for sl in sorted_slots() {
                        if ui.selectable_label(sl == slot, crate::tr(sl.title())).clicked() {
                            *swap = Some((i, sl));
                        }
                    }
                });
            if active {
                let (r, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(12.0), if resp.hovered() { Color32::WHITE } else { lr::TEXT_DIM() });
                if resp.clicked() {
                    self.clear_slot(slot);
                    changed = true;
                }
            }
        });
        match slot {
            Slot::Cat(k) => changed |= self.value_list(ui, k, w),
            Slot::Range(r) => changed |= self.number_list(ui, r, w),
        }
        changed
    }

    /// Search field and value list of an attribute (only this list scrolls, and only up and down).
    fn value_list(&mut self, ui: &mut egui::Ui, k: usize, w: f32) -> bool {
        if k == KEYWORD_COL {
            return self.keyword_list(ui, w);
        }
        crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut self.search[k]).hint_text(crate::tr("Suchen")).font(egui::FontId::proportional(10.5)).desired_width(w - 8.0).margin(egui::vec2(4.0, 1.0)));
        let pattern = self.search[k].trim().to_lowercase();
        let numbers: Vec<(u32, usize)> = if pattern.is_empty() {
            self.numbers[k].clone()
        } else {
            // selected values stay visible so they can be deselected
            let shown: HashSet<u32> = self.numbers[k].iter().map(|x| x.0).filter(|w| self.selection[k].contains(w) || self.display_name(k, *w).to_lowercase().contains(&pattern)).collect();
            self.numbers[k].iter().copied().filter(|(w, _)| shown.contains(w)).collect()
        };
        // (only the rows in view are named: up to 5000 values per frame took milliseconds)
        let name_of = |i: usize| -> String { self.display_name(k, numbers[i].0) };
        let clicked = list_rows(ui, ("bib-facette", k), &numbers, &name_of, &self.selection[k], None);
        let Some((value, mods)) = clicked else { return false };
        let order: Vec<u32> = numbers.iter().map(|x| x.0).collect();
        list_click(&mut self.selection[k], &order, &mut self.anchor[k], value, mods);
        true
    }

    /// Number list (aperture, focal length, ISO, shutter speed): a header with "all" or the selection, then one row
    /// per value, ascending, with count.
    fn number_list(&mut self, ui: &mut egui::Ui, r: usize, w: f32) -> bool {
        let sel = &self.band_selection[r];
        let text = match sel.len() {
            0 => crate::tr("alle").to_string(),
            1 => self.band_name(r, *sel.iter().next().unwrap()),
            n => crate::trf!("{n} gewählt", n = n),
        };
        // same height as the search field of the other columns: nothing jumps when switching the content
        let (header, _) = ui.allocate_exact_size(egui::vec2(w - 8.0, 18.0), egui::Sense::hover());
        let font = egui::FontId::proportional(10.5);
        let color = if sel.is_empty() { lr::TEXT_DIM() } else { Color32::WHITE };
        let g = ui.painter().layout_no_wrap(text, font, color);
        ui.painter().with_clip_rect(header).galley(egui::pos2(header.left() + 4.0, header.center().y - g.size().y / 2.0), g, color);
        let numbers = self.band_numbers[r].clone();
        let name_of = |i: usize| self.band_name(r, numbers[i].0);
        let clicked = list_rows(ui, ("bib-zahlen", r), &numbers, &name_of, &self.band_selection[r], None);
        let Some((value, mods)) = clicked else { return false };
        let order: Vec<u32> = numbers.iter().map(|x| x.0).collect();
        list_click(&mut self.band_selection[r], &order, &mut self.anchor[N + r], value, mods);
        true
    }

    fn band_name(&self, r: usize, i: u32) -> String {
        if i == NO_VALUE {
            crate::tr("(ohne)").into()
        } else {
            self.bands[r].get(i as usize).map(|b| b.text.clone()).unwrap_or_default()
        }
    }
}

/// Name of a weekday (1 Monday … 7 Sunday) in the interface language.
pub fn weekday_name(d: u8) -> &'static str {
    match d {
        1 => crate::tr("Montag"),
        2 => crate::tr("Dienstag"),
        3 => crate::tr("Mittwoch"),
        4 => crate::tr("Donnerstag"),
        5 => crate::tr("Freitag"),
        6 => crate::tr("Samstag"),
        7 => crate::tr("Sonntag"),
        _ => "?",
    }
}

/// Name of a time of day (0 night, 1 morning, 2 day, 3 evening) with its hours.
pub fn time_of_day_name(t: u8) -> &'static str {
    match t {
        0 => crate::tr("Nacht (22–5 Uhr)"),
        1 => crate::tr("Morgen (5–11 Uhr)"),
        2 => crate::tr("Tagsüber (11–17 Uhr)"),
        3 => crate::tr("Abend (17–22 Uhr)"),
        _ => "?",
    }
}

/// Rows of a list column (shared by all columns): name on the left, count on the right. Scrolls up and down only, fixed row height. Returns: clicked value and the modifiers held.
fn list_rows(ui: &mut egui::Ui, salt: (&str, usize), numbers: &[(u32, usize)], name_of: &dyn Fn(usize) -> String, selection: &BTreeSet<u32>, menu: Option<&mut dyn FnMut(&egui::Response, u32)>) -> Option<(u32, egui::Modifiers)> {
    let row_at = |i: usize| {
        let (value, n) = numbers[i];
        ListRow { value, name: name_of(i), count: crate::library_ui::thousands(n), dim: n == 0 }
    };
    list_rows_with(ui, salt, numbers.len(), &row_at, selection, menu)
}

/// A row of a list column with a ready count text (e.g. "1 234 · 312").
pub(crate) struct ListRow {
    pub value: u32,
    pub name: String,
    pub count: String,
    /// shown dimmed (no hits under the other filters)
    pub dim: bool,
}

/// Like [`list_rows`] with ready count texts (the people view uses it for its columns).
pub(crate) fn list_rows_labeled(ui: &mut egui::Ui, salt: (&str, usize), rows: &[ListRow], selection: &BTreeSet<u32>, menu: Option<&mut dyn FnMut(&egui::Response, u32)>) -> Option<(u32, egui::Modifiers)> {
    let row_at = |i: usize| ListRow { value: rows[i].value, name: rows[i].name.clone(), count: rows[i].count.clone(), dim: rows[i].dim };
    list_rows_with(ui, salt, rows.len(), &row_at, selection, menu)
}

/// The list itself: a row is only made (`row_at`) when it is in view.
fn list_rows_with(ui: &mut egui::Ui, salt: (&str, usize), len: usize, row_at: &dyn Fn(usize) -> ListRow, selection: &BTreeSet<u32>, mut menu: Option<&mut dyn FnMut(&egui::Response, u32)>) -> Option<(u32, egui::Modifiers)> {
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false; 2]).show_rows(ui, ROW_H, len, |ui, range| {
        for i in range {
            let row = &row_at(i);
            let value = row.value;
            let is_on = selection.contains(&value);
            let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
            let p = ui.painter();
            if is_on {
                p.rect_filled(r, 0.0, Color32::from_gray(0x4a));
            } else if resp.hovered() {
                p.rect_filled(r, 0.0, Color32::from_gray(0x2a));
            }
            let color = if row.dim && !is_on { Color32::from_gray(0x7a) } else if is_on || resp.hovered() { Color32::WHITE } else { lr::TEXT() };
            let font = egui::FontId::proportional(10.5);
            let right = r.right() - 2.0;
            let count_galley = p.layout_no_wrap(row.count.clone(), font.clone(), lr::TEXT_DIM());
            let space = egui::Rect::from_min_max(r.min + egui::vec2(3.0, 0.0), egui::pos2(right - count_galley.size().x - 4.0, r.bottom()));
            let name_galley = p.layout_no_wrap(row.name.clone(), font, color);
            p.with_clip_rect(space).galley(egui::pos2(space.left(), r.center().y - name_galley.size().y / 2.0), name_galley, color);
            p.galley(egui::pos2(right - count_galley.size().x, r.center().y - count_galley.size().y / 2.0), count_galley, lr::TEXT_DIM());
            if let Some(m) = menu.as_mut() {
                m(&resp, value);
            }
            if resp.clicked() {
                clicked = Some((value, ui.input(|i| i.modifiers)));
            }
        }
    });
    clicked
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Click alone selects only this row (again: clears), ⌘ adds/removes, ⇧ selects the range from the last click.
    #[test]
    fn list_click_like_finder() {
        let order = [5u32, 3, 9, 1];
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        let none = egui::Modifiers::NONE;
        list_click(&mut sel, &order, &mut anchor, 3, none);
        assert_eq!(sel, BTreeSet::from([3]));
        list_click(&mut sel, &order, &mut anchor, 9, none);
        assert_eq!(sel, BTreeSet::from([9]));
        list_click(&mut sel, &order, &mut anchor, 9, none);
        assert!(sel.is_empty());
        list_click(&mut sel, &order, &mut anchor, 5, none);
        list_click(&mut sel, &order, &mut anchor, 1, egui::Modifiers::COMMAND);
        assert_eq!(sel, BTreeSet::from([5, 1]));
        list_click(&mut sel, &order, &mut anchor, 1, egui::Modifiers::COMMAND);
        assert_eq!(sel, BTreeSet::from([5]));
        // range from the last clicked row (1) to 3 in list order: 3, 9, 1
        list_click(&mut sel, &order, &mut anchor, 3, egui::Modifiers::SHIFT);
        assert_eq!(sel, BTreeSet::from([3, 9, 1]));
    }

    /// Every value is its own row with an exact search text; the search finds what the list shows.
    #[test]
    fn values_and_search() {
        assert_eq!(band_of(2, band_key(2, 200.0)).search, "iso:200");
        assert_eq!(band_of(0, band_key(0, 2.83)).search, "aperture:2.8");
        assert_eq!(band_of(0, band_key(0, 2.0)).text, "f/2");
        assert_eq!(band_of(1, band_key(1, 35.0)).search, "focal:35");
        assert_eq!(band_of(1, band_key(1, 35.5)).search, "focal:35.5");
        assert_ne!(band_key(1, 35.0), band_key(1, 35.5));
        assert_eq!(band_of(3, band_key(3, 1.0 / 900.0)).text, "1/900");
        assert_eq!(band_of(3, band_key(3, 1.0 / 900.0)).search, "shutter:1/900");
        assert_eq!(band_of(3, band_key(3, 1.0 / 3.0)).text, "1/3");
        assert_eq!(band_of(3, band_key(3, 0.8)).text, "0,8 s");
        assert_eq!(band_of(3, band_key(3, 0.8)).search, "shutter:0.8");
        assert_eq!(band_of(3, band_key(3, 4.0)).text, "4 s");
        assert_ne!(band_key(3, 1.0 / 125.0), band_key(3, 1.0 / 100.0));
        assert_eq!(value_text(3, 1.0 / 250.0), "1/250");
        assert_eq!(value_text(0, 5.6), "f/5,6");
    }

    /// Rows with a few cameras (every third "Canon"), for tests of the background build.
    pub(crate) fn test_rows(n: usize) -> Vec<Row> {
        (0..n)
            .map(|i| {
                let mut r = row(35.0 + (i % 7) as f64, 0.01, 100 + (i % 5) as u32 * 100, 2.0 + (i % 4) as f64);
                r.camera = Some(if i % 3 == 0 { "Canon" } else { "Sony" }.to_string());
                r.image = i as i64;
                r
            })
            .collect()
    }

    /// The number lists hold each value once, ascending (sorted by value, not by key text), and every row points to its own.
    #[test]
    fn number_lists_are_ascending_and_distinct() {
        let rows = test_rows(500);
        let f = Facets::build(&rows, &[], &HashMap::new());
        let texts: Vec<&str> = f.bands[0].iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["f/2", "f/3", "f/4", "f/5"]);
        let iso: Vec<&str> = f.bands[2].iter().map(|b| b.text.as_str()).collect();
        assert_eq!(iso, ["100", "200", "300", "400", "500"]);
        for (i, z) in rows.iter().enumerate() {
            assert_eq!(f.bands[0][f.band_rows[i][0] as usize].text, format!("f/{}", z.aperture.unwrap()));
        }
    }

    fn row(focal: f64, time: f64, iso: u32, aperture: f64) -> Row {
        Row {
            image: 1, file: 1, folder: 1, name: "a.jpg".into(), format: "JPG".into(), copy: None, stars: 0, flag: 0, label: 0, status: 0, camera: None, capture: None,
            run: None, size: None, mtime: None, width: None, height: None, orientation: None, lens: None, aperture: Some(aperture), focal: Some(focal),
            iso: Some(iso as i64), time: Some(time), ai_quality: None, changed: None, imported: None, from_lightroom: false, sidecar_jpeg: None, jpeg_for: None, place: None,
        }
    }

    /// A parent term includes its children: its count is the photos with it or a term below it (each photo once), the
    /// column shows the tree (parent first, children indented below), choosing the parent filters its children too.
    #[test]
    fn keyword_hierarchy_includes_children() {
        let kw = |id: i64, name: &str, parent: Option<i64>, person: bool| Keyword { id, name: name.into(), parent, count: 0, person };
        let keywords = vec![kw(1, "Orte", None, false), kw(2, "Italien", Some(1), false), kw(3, "Rom", Some(2), false), kw(4, "Venedig", Some(2), false), kw(5, "Hund", None, false), kw(6, "Personen", None, false), kw(7, "Christa", Some(6), true)];
        let mut rows = vec![row(35.0, 0.01, 100, 2.0); 5];
        for (i, z) in rows.iter_mut().enumerate() {
            z.image = i as i64 + 1;
        }
        // photo 1: Rom and Italien (no double count), 2: Venedig, 3: Hund, 4: Christa (a person), 5: nothing
        let per_image: HashMap<i64, Vec<i64>> = HashMap::from([(1, vec![3, 2]), (2, vec![4]), (3, vec![5]), (4, vec![7])]);
        let mut f = Facets::build(&rows, &keywords, &per_image);
        let all: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(f.apply(&rows, &all).len(), 5);
        let count = |f: &Facets, name: &str| f.numbers[KEYWORD_COL].iter().find(|(w, _)| f.name(KEYWORD_COL, *w) == name).map(|x| x.1);
        assert_eq!(count(&f, "Orte"), Some(2));
        assert_eq!(count(&f, "Italien"), Some(2), "photo 1 has Rom and Italien: counted once");
        assert_eq!(count(&f, "Rom"), Some(1));
        assert_eq!(count(&f, "Personen"), Some(1), "the parent of a person counts its photos");
        assert_eq!(count(&f, "Christa"), None, "people stay in the person column");
        // tree order: Orte (2) before Hund (1) and Personen (1), Italien directly below Orte, then Rom and Venedig
        let order: Vec<String> = f.numbers[KEYWORD_COL].iter().map(|(w, _)| f.name(KEYWORD_COL, *w)).collect();
        assert_eq!(order, vec!["Orte", "Italien", "Rom", "Venedig", "Hund", "Personen"]);
        assert_eq!(f.kw_chain(f.names[KEYWORD_COL].iter().position(|n| n == "Rom").unwrap() as u32).len(), 3);
        // the parent filters its children; ⌘-click adds a second term
        f.choose_name(KEYWORD_COL, "Italien");
        assert_eq!(f.apply(&rows, &all), vec![0, 1]);
        f.choose_name(KEYWORD_COL, "Hund");
        assert_eq!(f.apply(&rows, &all), vec![0, 1, 2]);
        f.reset();
        f.choose_name(KEYWORD_COL, "Rom");
        assert_eq!(f.apply(&rows, &all), vec![0]);
        assert_eq!(f.as_search().unwrap(), "keyword:=\"Rom\"");
    }

    /// "Paris" below places and below people: the filter button shows the path, remembering and restoring the
    /// selection keeps the one chosen (not the first of that name).
    #[test]
    fn ambiguous_keyword_names_keep_their_path() {
        let kw = |id: i64, name: &str, parent: Option<i64>| Keyword { id, name: name.into(), parent, count: 1, person: false };
        let keywords = vec![kw(1, "Orte", None), kw(2, "Paris", Some(1)), kw(3, "Personen", None), kw(4, "Paris", Some(3))];
        let mut rows = vec![row(35.0, 0.01, 100, 2.0); 2];
        rows[0].image = 1;
        rows[1].image = 2;
        let per_image: HashMap<i64, Vec<i64>> = HashMap::from([(1, vec![2]), (2, vec![4])]);
        let mut f = Facets::build(&rows, &keywords, &per_image);
        let all: Vec<usize> = (0..rows.len()).collect();
        f.apply(&rows, &all);
        let second = f.names[KEYWORD_COL].iter().rposition(|n| n == "Paris").unwrap() as u32;
        f.selection[KEYWORD_COL].insert(second);
        assert_eq!(f.apply(&rows, &all), vec![1]);
        assert_eq!(f.active_columns(), vec![("Stichwort", "Personen › Paris".to_string())]);
        assert_eq!(f.as_search().unwrap(), "keyword:=\"Personen > Paris\"", "the rule names the path");
        let remembered = f.remember();
        let mut g = Facets::build(&rows, &keywords, &per_image);
        g.restore(&remembered);
        assert_eq!(g.apply(&rows, &all), vec![1], "the person Paris, not the place");
    }

    #[test]
    fn month_and_day_filters_round_trip_to_search() {
        let mut rows = vec![row(35.0, 0.01, 100, 2.0); 4];
        rows[0].capture = Some("2020-07-15T10:30:00".into());
        rows[1].capture = Some("2021-07-08T10:30:00".into());
        rows[2].capture = Some("2021-01-15T10:30:00".into());
        rows[3].capture = None;
        let mut facets = Facets::build(&rows, &[], &HashMap::new());
        facets.choose_name(9, "07");
        facets.choose_name(10, "15");
        assert_eq!(facets.apply(&rows, &[0, 1, 2, 3]), vec![0]);
        let query = facets.as_search().unwrap();
        assert_eq!(query, "month:7 day:15");
        let found: Vec<_> = rows.iter().enumerate().filter(|(_, r)| matches_search(&query, r)).map(|(i, _)| i).collect();
        assert_eq!(found, vec![0]);
        let db = library::db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let folders = HashMap::new();
        let context = library::search::Context { db: &db, rows: &rows, folder_paths: &folders };
        assert_eq!(library::search::evaluate(&context, &library::search::parse(&query)), vec![true, false, false, false]);
        assert_eq!(library::search::evaluate(&context, &library::search::parse("jahr:2021 monat:07")), vec![false, true, false, false]);
        let saved = facets.remember();
        let mut restored = Facets::build(&rows, &[], &HashMap::new());
        restored.restore(&saved);
        assert_eq!(restored.apply(&rows, &[0, 1, 2, 3]), vec![0]);
        assert_eq!(Slot::from_slot_id("month"), Some(Slot::Cat(9)));
        assert_eq!(Slot::from_slot_id("day"), Some(Slot::Cat(10)));
    }

    /// Weekday and time of day as columns, same photos as the search.
    #[test]
    fn weekday_and_time_columns_round_trip() {
        let mut rows = vec![row(35.0, 0.01, 100, 2.0); 3];
        rows[0].capture = Some("2024-05-04T19:30:00".into()); // Saturday evening
        rows[1].capture = Some("2024-05-06T08:00:00".into()); // Monday morning
        rows[2].capture = Some("2024-05-05T20:00:00".into()); // Sunday evening
        let mut facets = Facets::build(&rows, &[], &HashMap::new());
        facets.choose_name(WEEKDAY_COL, "6");
        facets.choose_name(TIME_COL, "3");
        assert_eq!(facets.apply(&rows, &[0, 1, 2]), vec![0]);
        let query = facets.as_search().unwrap();
        assert_eq!(query, "weekday:6 time:evening");
        let db = library::db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let folders = HashMap::new();
        let context = library::search::Context { db: &db, rows: &rows, folder_paths: &folders };
        assert_eq!(library::search::evaluate(&context, &library::search::parse(&query)), vec![true, false, false]);
        assert_eq!(Slot::from_slot_id("weekday"), Some(Slot::Cat(WEEKDAY_COL)));
        assert_eq!(Slot::from_slot_id("time"), Some(Slot::Cat(TIME_COL)));
    }

    #[test]
    fn category_menu_follows_display_language_alphabetically() {
        for language in crate::menu::i18n::Language::ALL {
            let _guard = crate::menu::i18n::locale::TestLanguage::set(language);
            let slots = sorted_slots();
            let keys: Vec<_> = slots.iter().map(|s| library::search::fold_text(crate::tr(s.title()))).collect();
            assert_eq!(slots.len(), N + 4);
            assert!(keys.windows(2).all(|v| v[0] <= v[1]), "not sorted in {language:?}: {keys:?}");
        }
    }

    /// Lists: one row per occurring value, ascending; multiple and range selection give exact searches that
    /// the search finds again (smart collection round trip).
    #[test]
    fn lists_selection_and_round_trip() {
        let rows = vec![
            row(35.0, 1.0 / 900.0, 200, 2.0),
            row(35.5, 1.0 / 3.0, 400, 2.2),
            row(50.0, 0.8, 200, 2.0),
            row(28.0, 1.0 / 125.0, 800, 5.6),
            row(28.0, 1.0 / 100.0, 100, 5.6),
        ];
        let mut f = Facets::build(&rows, &[], &HashMap::new());
        let texts = |f: &Facets, k: usize| f.bands[k].iter().map(|b| b.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&f, 1), ["28 mm", "35 mm", "35,5 mm", "50 mm"]);
        assert_eq!(texts(&f, 3), ["1/900", "1/125", "1/100", "1/3", "0,8 s"]);
        assert_eq!(texts(&f, 0), ["f/2", "f/2,2", "f/5,6"]);
        assert_eq!(texts(&f, 2), ["100", "200", "400", "800"]);
        // single value
        f.band_selection[1].insert(1);
        let basis: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(f.apply(&rows, &basis), vec![0]);
        // range 35 … 50 (three rows)
        let order: Vec<u32> = (0..4).collect();
        let mut anchor = None;
        f.band_selection[1].clear();
        list_click(&mut f.band_selection[1], &order, &mut anchor, 1, egui::Modifiers::NONE);
        list_click(&mut f.band_selection[1], &order, &mut anchor, 3, egui::Modifiers::SHIFT);
        assert_eq!(f.apply(&rows, &basis), vec![0, 1, 2]);
        // search text finds the same photos
        f.reset();
        f.band_selection[3].extend([0, 3]);
        let q = f.as_search().unwrap();
        assert_eq!(q, "shutter:1/900 | shutter:1/3");
        let found: Vec<usize> = (0..rows.len()).filter(|i| matches_search(&q, &rows[*i])).collect();
        assert_eq!(found, vec![0, 1]);
        let mut g = Facets::build(&rows, &[], &HashMap::new());
        g.band_selection[1].extend([1, 2]);
        g.band_selection[3].insert(4);
        let q = g.as_search().unwrap();
        assert_eq!(q, "focal:35 | focal:35.5 shutter:0.8");
        assert!((0..rows.len()).all(|i| !matches_search(&q, &rows[i])));
        g.band_selection[3].clear();
        let q = g.as_search().unwrap();
        let found: Vec<usize> = (0..rows.len()).filter(|i| matches_search(&q, &rows[*i])).collect();
        assert_eq!(found, vec![0, 1]);
        // remember/restore after a rebuild
        let r = f.remember();
        let mut h = Facets::build(&rows, &[], &HashMap::new());
        h.restore(&r);
        assert_eq!(h.band_selection[3], BTreeSet::from([0, 3]));
    }

    /// Regression (review M2): the rule saved from the filter finds exactly the photos of the filter, not those whose
    /// camera/lens/keyword only contains the name ("Canon EOS R" and "Canon EOS R5").
    #[test]
    fn saved_rule_finds_exactly_the_filter_photos() {
        let mut rows = vec![row(35.0, 0.01, 100, 2.0); 4];
        for (z, c) in rows.iter_mut().zip(["Canon EOS R", "Canon EOS R5", "Canon EOS RP", "Canon EOS R"]) {
            z.camera = Some(c.into());
        }
        rows[1].image = 2;
        rows[2].image = 3;
        rows[3].image = 4;
        let mut f = Facets::build(&rows, &[], &HashMap::new());
        f.choose_name(0, "Canon EOS R");
        let basis: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(f.apply(&rows, &basis), vec![0, 3]);
        let q = f.as_search().unwrap();
        assert_eq!(q, "camera:=\"Canon EOS R\"");
        let db = library::db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let folders = HashMap::new();
        let context = library::search::Context { db: &db, rows: &rows, folder_paths: &folders };
        assert_eq!(library::search::evaluate(&context, &library::search::parse(&q)), vec![true, false, false, true]);
        // the old "contains" writing keeps working
        assert_eq!(library::search::evaluate(&context, &library::search::parse("camera:\"canon eos r\"")), vec![true, true, true, true]);
    }

    /// Review M3/N3: "(ohne)" and names with a quotation mark are no rule (never silently left out); a name with
    /// `|` or spaces stays one value.
    #[test]
    fn unsaveable_selection_is_an_error_and_special_names_stay_whole() {
        let mut rows = vec![row(35.0, 0.01, 100, 2.0); 2];
        rows[0].camera = Some("A|B x".into());
        rows[1].image = 2;
        let mut f = Facets::build(&rows, &[], &HashMap::new());
        f.selection[1].insert(NO_VALUE);
        assert!(f.as_search().is_err(), "no-value choice of a column");
        f.reset();
        f.band_selection[0].insert(NO_VALUE);
        assert!(f.as_search().is_err(), "no-value choice of a list");
        f.reset();
        f.choose_name(0, "A|B x");
        let q = f.as_search().unwrap();
        assert_eq!(q, "camera:=\"A|B x\"");
        let db = library::db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let folders = HashMap::new();
        let context = library::search::Context { db: &db, rows: &rows, folder_paths: &folders };
        let parsed = library::search::parse(&format!("{q} year:2020"));
        assert_eq!(parsed.clauses.len(), 2, "no accidental or: {parsed:?}");
        assert_eq!(library::search::evaluate(&context, &library::search::parse(&q)), vec![true, false]);
        let mut g = Facets::build(&[{ let mut z = row(35.0, 0.01, 100, 2.0); z.camera = Some("Say \"x\"".into()); z }], &[], &HashMap::new());
        g.choose_name(0, "Say \"x\"");
        assert!(g.as_search().is_err(), "quotation mark");
    }

    /// Evaluate a search text against the number fields of a row (only the clauses of number fields).
    fn matches_search(q: &str, z: &Row) -> bool {
        library::search::parse(q).clauses.iter().all(|c| {
            c.iter().any(|t| match &t.kind {
                library::search::Kind::Field(f, cmp) => library::search::number_matches(f, cmp, z),
                _ => false,
            })
        })
    }
}
