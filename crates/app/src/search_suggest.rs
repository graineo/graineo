//! Completion in the search field.
//!
//! - Typed names with their kind ("PERSON Christa", "ORT Rom", "ORDNER 2012-05-30" …). Taking one puts the matching
//!   field term into the field (`person:Christa`), so the search looks in that field instead of the free text.
//! - Field names like in an IDE: ":" or the start of a field name, short form or second name ("ca", "kam", ":c") lists
//!   the fields ("camera:" · Kamera · Kamera-Modell). Tab or ↩ takes it: the field then reads `camera:""` with the
//!   cursor between the quotes, and the list shows the values of the field; a value fills the quotes.
//!   Inserted is always the English base form (`library::search_fields`).
//!
//! The names and counts come from an index built in the background (read-only database connection) when the
//! search field gets the focus and the library changed (names without photos are left out); the UI thread only
//! ranks the prepared, folded names. Submodule of `library_ui` (accesses its fields).

use super::*;
use library::search_fields::{self, FieldDef};

/// Test aid (`GRAINEO_LIBRARY_TEST="search=chri;focus=1"`): put the focus into the search field once.
pub(super) static TEST_FOCUS: AtomicBool = AtomicBool::new(false);

/// Kind of a suggestion, in the order of the display when the match is equally good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Kind {
    Person,
    Place,
    Keyword,
    Collection,
    Folder,
    Camera,
    Lens,
}

impl Kind {
    /// Field of the search language (base form).
    pub(super) fn field(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Place => "place",
            Kind::Keyword => "keyword",
            Kind::Collection => "album",
            Kind::Folder => "folder",
            Kind::Camera => "camera",
            Kind::Lens => "lens",
        }
    }
    /// Shown in capitals before the name.
    pub(super) fn label(self) -> &'static str {
        match self {
            Kind::Person => crate::tr("Person"),
            Kind::Place => crate::tr("Ort"),
            Kind::Keyword => crate::tr("Stichwort"),
            Kind::Collection => crate::tr("Sammlung"),
            Kind::Folder => crate::tr("Ordner"),
            Kind::Camera => crate::tr("Kamera"),
            Kind::Lens => crate::tr("Objektiv"),
        }
    }
    fn of_field(id: &str) -> Option<Kind> {
        Some(match id {
            "person" => Kind::Person,
            "place" => Kind::Place,
            "keyword" => Kind::Keyword,
            "album" => Kind::Collection,
            "folder" => Kind::Folder,
            "camera" => Kind::Camera,
            "lens" => Kind::Lens,
            _ => return None,
        })
    }
}

/// Name in the interface language and short explanation of a field (base form `id`).
pub fn field_text(id: &str) -> (&'static str, &'static str) {
    match id {
        "camera" => (crate::tr("Kamera"), crate::tr("Kamera-Modell")),
        "lens" => (crate::tr("Objektiv"), crate::tr("Objektiv-Modell")),
        "aperture" => (crate::tr("Blende"), crate::tr("Blendenzahl, z. B. <=2.8")),
        "focal" => (crate::tr("Brennweite"), crate::tr("Brennweite in mm, z. B. 20-35")),
        "iso" => ("ISO", crate::tr("Empfindlichkeit, z. B. >=1600")),
        "shutter" => (crate::tr("Belichtungszeit"), crate::tr("Belichtungszeit, z. B. <=1/60")),
        "date" => (crate::tr("Datum"), crate::tr("Aufnahmedatum, z. B. >2020-05")),
        "year" => (crate::tr("Jahr"), crate::tr("Aufnahmejahr")),
        "month" => (crate::tr("Monat"), crate::tr("Monat über alle Jahre, 1–12")),
        "day" => (crate::tr("Tag im Monat"), crate::tr("Tag im Monat, 1–31")),
        "weekday" => (crate::tr("Wochentag"), crate::tr("Wochentag, z. B. sa oder 6")),
        "time" => (crate::tr("Tageszeit"), crate::tr("Morgen, tagsüber, Abend, Nacht oder Stunden")),
        "keyword" => (crate::tr("Stichwort"), crate::tr("Stichwort und alle darunter")),
        "person" => (crate::tr("Person"), crate::tr("Erkannte Person oder Personen-Stichwort")),
        "place" => (crate::tr("Ort"), crate::tr("Stichwort unter „Orte“")),
        "album" => (crate::tr("Sammlung"), crate::tr("Name einer Sammlung")),
        "folder" => (crate::tr("Ordner"), crate::tr("Teil des Ordnerpfads")),
        "rating" => (crate::tr("Sterne"), crate::tr("Bewertung 0–5")),
        "label" => (crate::tr("Etikett"), crate::tr("Farbetikett")),
        "flag" => (crate::tr("Markierung"), crate::tr("markiert, abgelehnt oder ohne")),
        "format" => (crate::tr("Format"), crate::tr("Dateiformat, z. B. X3F")),
        "name" => (crate::tr("Dateiname"), crate::tr("Teil des Dateinamens")),
        "ai" => (crate::tr("KI-Qualität"), crate::tr("gut, mittel oder Ausschuss")),
        "has" => (crate::tr("Hat"), crate::tr("hat Stichwörter oder Personen")),
        "no" => (crate::tr("Ohne"), crate::tr("ohne Stichwörter oder Personen")),
        "edited" => (crate::tr("Geändert"), crate::tr("zuletzt geändert, z. B. <2d")),
        "imported" => (crate::tr("Importiert"), crate::tr("aufgenommen in die Bibliothek, z. B. <1w")),
        _ => ("", ""),
    }
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub kind: Kind,
    pub name: String,
    folded: String,
    pub count: usize,
}

impl Entry {
    pub(super) fn new(kind: Kind, name: String, count: usize) -> Entry {
        let folded = search::fold_text(&name);
        Entry { kind, name, folded, count }
    }
    /// 0: name starts with it, 1: a word of the name starts with it, 2: contained; None: no match.
    fn fit(&self, f: &str) -> Option<u8> {
        if self.folded.starts_with(f) {
            Some(0)
        } else if self.folded.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(f)) {
            Some(1)
        } else if self.folded.contains(f) {
            Some(2)
        } else {
            None
        }
    }
}

/// All names that can be suggested.
#[derive(Default)]
pub(super) struct Index {
    pub entries: Vec<Entry>,
}

/// Index plus its state: built for which library load, running?
#[derive(Default)]
pub(super) struct Shared {
    pub index: Option<Arc<Index>>,
    pub built_for: Option<usize>,
    pub running: bool,
    /// last list and what it was made for (ranked once per change, not in every frame)
    cache: Option<(String, Vec<Suggestion>)>,
}

/// One row of the suggestion list.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Suggestion {
    /// replaces the byte range `range` of the search text (the token under the cursor)
    pub insert: String,
    pub range: (usize, usize),
    /// cursor this many characters before the end of the insert (1: between the quotes of `camera:""`)
    pub back: usize,
    /// a space follows (a finished term)
    pub space: bool,
    pub kind: Option<&'static str>,
    pub text: String,
    pub count: Option<usize>,
    /// field name row: base form on the left, explanation dimmed on the right
    pub detail: Option<&'static str>,
}

impl Suggestion {
    pub(super) fn plain(insert: String, range: (usize, usize), text: String) -> Suggestion {
        Suggestion { insert, range, back: 0, space: true, kind: None, text, count: None, detail: None }
    }
}

/// Most rows of the list (the popup grows with the hits up to this).
pub(super) const ROWS: usize = 9;
const ROW_H: f32 = 22.0;

/// The best matches for a typed word: start of the name before start of a word before inside, then kind, then count.
pub(super) fn rank<'a>(index: &'a Index, word: &str, max: usize) -> Vec<&'a Entry> {
    let f = search::fold_text(word.trim_matches('"'));
    if f.chars().count() < 2 {
        return Vec::new();
    }
    let mut hits: Vec<(u8, &Entry)> = index.entries.iter().filter_map(|e| e.fit(&f).map(|k| (k, e))).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.kind.cmp(&b.1.kind)).then(b.1.count.cmp(&a.1.count)).then_with(|| a.1.folded.cmp(&b.1.folded)));
    hits.into_iter().take(max).map(|(_, e)| e).collect()
}

/// Names of one kind that contain the typed part (for `person:…`, `place:…`, `album:…`), most frequent first, with count.
pub(super) fn values(index: &Index, kind: Kind, part: &str) -> Vec<(String, usize)> {
    let f = search::fold_text(part);
    let mut v: Vec<&Entry> = index.entries.iter().filter(|e| e.kind == kind && (f.is_empty() || e.fit(&f).is_some())).collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.folded.cmp(&b.folded)));
    v.into_iter().take(8).map(|e| (e.name.clone(), e.count)).collect()
}

/// Build the index (background thread; reads only).
fn build(db: &Db) -> Index {
    let mut entries = Vec::new();
    let pairs = |sql: &str, kind: Kind, entries: &mut Vec<Entry>| {
        if let Ok(mut st) = db.conn.prepare(sql) {
            if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
                entries.extend(rows.flatten().filter(|(n, _)| !n.trim().is_empty()).map(|(n, c)| Entry::new(kind, n, c.max(0) as usize)));
            }
        }
    };
    // people: as keyword (Lightroom) or as confirmed face
    pairs(
        "SELECT s.name, (SELECT count(*) FROM (SELECT image FROM image_keyword WHERE keyword = s.id UNION SELECT image FROM face WHERE person = s.id AND status = 'confirmed')) FROM keyword s WHERE s.person = 1",
        Kind::Person,
        &mut entries,
    );
    pairs("SELECT s.name, count(sb.image) FROM collection s LEFT JOIN collection_image sb ON sb.collection = s.id WHERE s.kind IN (0, 2, 3) GROUP BY s.id", Kind::Collection, &mut entries);
    pairs("SELECT camera, count(*) FROM file WHERE camera IS NOT NULL AND status <> 4 GROUP BY camera", Kind::Camera, &mut entries);
    pairs("SELECT lens, count(*) FROM file WHERE lens IS NOT NULL AND status <> 4 GROUP BY lens", Kind::Lens, &mut entries);
    // folders: the last part of the path (the root folder by its name), photos directly in it
    if let Ok(mut st) = db.conn.prepare("SELECT o.path, st.name, count(d.id) FROM folder o JOIN root_folder st ON st.id = o.root JOIN file d ON d.folder = o.id AND d.status <> 4 GROUP BY o.id") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))) {
            for (path, root, n) in rows.flatten() {
                let name = path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or(root);
                if !name.trim().is_empty() {
                    entries.push(Entry::new(Kind::Folder, name, n.max(0) as usize));
                }
            }
        }
    }
    // keywords (not people): places are those below a root "Orte"/"Places"; the roots themselves are no suggestion
    let mut tree: HashMap<i64, (Option<i64>, String)> = HashMap::new();
    let mut count: HashMap<i64, usize> = HashMap::new();
    if let Ok(mut st) = db.conn.prepare("SELECT id, parent, name FROM keyword WHERE person = 0") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?))) {
            for (id, p, n) in rows.flatten() {
                tree.insert(id, (p, n));
            }
        }
    }
    if let Ok(mut st) = db.conn.prepare("SELECT keyword, count(*) FROM image_keyword GROUP BY keyword") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
            for (id, c) in rows.flatten() {
                count.insert(id, c.max(0) as usize);
            }
        }
    }
    let places = search::places_in(&tree);
    for (id, (_, name)) in &tree {
        if name.trim().is_empty() || search::is_place_root(name) {
            continue;
        }
        let kind = if places.contains(id) { Kind::Place } else { Kind::Keyword };
        entries.push(Entry::new(kind, name.clone(), count.get(id).copied().unwrap_or(0)));
    }
    // names without photos lead nowhere
    entries.retain(|e| e.count > 0);
    // the same name twice (keyword in two branches, folders of the same name): once, with the larger count
    entries.sort_by(|a, b| (a.kind, &a.folded).cmp(&(b.kind, &b.folded)).then(b.count.cmp(&a.count)));
    entries.dedup_by(|b, a| a.kind == b.kind && a.folded == b.folded);
    Index { entries }
}

/// Quote a value when it needs it (spaces) or when the user is typing in quotes.
fn quoted(v: &str, force: bool) -> String {
    if force || v.contains(char::is_whitespace) {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

/// Put a suggestion into the text; returns the new cursor position (character index).
pub(super) fn apply(text: &mut String, s: &Suggestion) -> usize {
    let (a, b) = (s.range.0.min(text.len()), s.range.1.min(text.len()));
    let mut insert = s.insert.clone();
    // a finished term gets a space, unless one follows already
    if s.space && !text[b..].starts_with(' ') {
        insert.push(' ');
    }
    text.replace_range(a..b, &insert);
    let end = a + insert.len();
    let end = if s.space && !insert.ends_with(' ') { end + 1 } else { end };
    text[..end.min(text.len())].chars().count() - s.back
}

/// Cursor of the search field (byte index into `text`; end of the text when unknown).
pub(super) fn cursor_byte(ctx: &egui::Context, id: egui::Id, text: &str) -> usize {
    let chars = egui::TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index);
    match chars {
        Some(n) => text.char_indices().nth(n.0).map_or(text.len(), |(i, _)| i),
        None => text.len(),
    }
}

/// Put the cursor of the search field at a character index.
pub(super) fn set_cursor(ctx: &egui::Context, id: egui::Id, chars: usize) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(chars))));
    state.store(ctx, id);
}

/// Fixed values of a field: (insert, shown text).
fn fixed_values(id: &str) -> Vec<(String, String)> {
    let same = |v: &[&str]| v.iter().map(|s| (s.to_string(), s.to_string())).collect::<Vec<_>>();
    match id {
        "month" => (1..=12).map(|m| (m.to_string(), crate::library_ui::month_name(&format!("0000-{m:02}")))).collect(),
        "day" => (1..=31).map(|d| (d.to_string(), d.to_string())).collect(),
        "weekday" => (1..=7u8).map(|d| (d.to_string(), crate::library_filter::weekday_name(d).to_string())).chain(std::iter::once(("weekend".to_string(), crate::tr("Wochenende").to_string()))).collect(),
        "time" => [1u8, 2, 3, 0].iter().map(|&t| (search_fields::TIME_OF_DAY[t as usize].to_string(), crate::library_filter::time_of_day_name(t).to_string())).collect(),
        "label" => [("red", "Etikett Rot"), ("yellow", "Etikett Gelb"), ("green", "Etikett Grün"), ("blue", "Etikett Blau"), ("purple", "Etikett Lila")].iter().map(|(v, t)| (v.to_string(), crate::tr(t).to_string())).collect(),
        "ai" => [("good", "gut"), ("medium", "mittel"), ("reject", "Ausschuss")].iter().map(|(v, t)| (v.to_string(), crate::tr(t).to_string())).collect(),
        "flag" => [("picked", "markiert"), ("rejected", "abgelehnt"), ("none", "ohne")].iter().map(|(v, t)| (v.to_string(), crate::tr(t).to_string())).collect(),
        "has" | "no" => [("keyword", "Stichwort"), ("person", "Person")].iter().map(|(v, t)| (v.to_string(), crate::tr(t).to_string())).collect(),
        "rating" => same(&[">=1", ">=2", ">=3", ">=4", "5", "0"]),
        "aperture" => same(&["<=1.8", "<=2.8", "<=4", "1.4-2.8", ">=8"]),
        "focal" => same(&["<=24", "20-35", "35-85", ">=100"]),
        "iso" => same(&["<=200", ">=800", ">=3200"]),
        "shutter" => same(&["<=1/500", ">=1/30", ">=1"]),
        "edited" | "imported" => same(&["<1d", "<1w", "<1m", "<1y"]),
        _ => Vec::new(),
    }
}

impl LibraryView {
    /// Make sure the index fits the loaded library (starts the background build; never waits).
    pub(super) fn suggest_index(&mut self, ctx: &egui::Context) -> Option<Arc<Index>> {
        let stamp = self.rows.len() ^ (self.keywords.len() << 20) ^ (self.collections.len() << 40);
        let mut s = self.suggest.lock().unwrap();
        if s.built_for != Some(stamp) && !s.running && self.db.is_some() {
            s.running = true;
            s.built_for = Some(stamp);
            let shared = self.suggest.clone();
            let ctx = ctx.clone();
            crate::threads::spawn(crate::threads::Lane::Ahead, move || {
                let index = Db::read_only(&library::db_path()).ok().map(|db| Arc::new(build(&db)));
                let mut s = shared.lock().unwrap();
                if index.is_some() {
                    s.index = index;
                }
                s.running = false;
                ctx.request_repaint();
            });
        }
        s.index.clone()
    }

    /// Suggestions for the token under the cursor (`cursor`: byte index): field names, values of a field, names.
    pub(super) fn suggestions(&mut self, ctx: &egui::Context, cursor: usize) -> Vec<Suggestion> {
        // names come from the index built in the background (never a query in the UI thread)
        let index = self.suggest_index(ctx);
        let key = format!(
            "{}\u{0}{cursor}\u{0}{:?}\u{0}{}{}{}{}{}",
            self.filter.text,
            index.as_ref().map(Arc::as_ptr),
            self.search_in_source,
            self.filter.attributes_active(),
            self.facets.as_ref().is_some_and(|f| f.active()),
            self.source == Source::All,
            self.formats.len() + self.years.len(),
        );
        if let Some((k, list)) = &self.suggest.lock().unwrap().cache {
            if *k == key {
                return list.clone();
            }
        }
        let list = self.suggestions_for(index, cursor);
        self.suggest.lock().unwrap().cache = Some((key, list.clone()));
        list
    }

    fn suggestions_for(&self, index: Option<Arc<Index>>, cursor: usize) -> Vec<Suggestion> {
        let text = self.filter.text.clone();
        let Some(range) = search_fields::token_at(&text, cursor.min(text.len())) else { return Vec::new() };
        let word = &text[range.0..range.1];
        let (not, word) = match word.strip_prefix('-') {
            Some(w) => ("-", w),
            None => ("", word),
        };
        let mut out: Vec<Suggestion> = Vec::new();
        let field_row = |d: &FieldDef| {
            let (name, detail) = field_text(d.id);
            Suggestion { insert: format!("{not}{}:\"\"", d.id), range, back: 1, space: false, kind: None, text: name.to_string(), count: None, detail: Some(detail) }
        };
        // ":" or ":ca": all fields that start with it
        if let Some(prefix) = word.strip_prefix([':', '：']) {
            if !prefix.contains([':', '：']) {
                out.extend(search_fields::fields_starting(prefix).into_iter().take(ROWS).map(|(d, _)| field_row(d)));
            }
            return out;
        }
        let colon = word.find([':', '：']);
        if let Some(i) = colon {
            let (k, rest) = word.split_at(i);
            let Some(def) = search_fields::field_def(k) else { return out };
            let rest = &rest[rest.chars().next().map_or(0, char::len_utf8)..];
            let in_quotes = rest.starts_with('"');
            let part = search::fold_text(rest.trim_matches('"'));
            let label = field_text(def.id).0;
            let values: Vec<(String, String, Option<usize>)> = match (Kind::of_field(def.id), def.id) {
                (_, "format") => self.formats.iter().map(|(n, c)| (n.clone(), n.clone(), Some(*c))).collect(),
                (_, "year") => self.years.keys().rev().map(|j| (j.to_string(), j.to_string(), None)).collect(),
                (Some(kind), _) => index.as_deref().map(|i| values(i, kind, &part)).unwrap_or_default().into_iter().map(|(n, c)| (n.clone(), n, Some(c))).collect(),
                _ => fixed_values(def.id).into_iter().map(|(v, t)| (v, t, None)).collect(),
            };
            let shown = values.into_iter().filter(|(v, t, _)| part.is_empty() || search::fold_text(v).contains(&part) || search::fold_text(t).contains(&part));
            for (v, t, count) in shown.take(8) {
                out.push(Suggestion { insert: format!("{not}{}:{}", def.id, quoted(&v, in_quotes)), range, back: 0, space: true, kind: Some(label), text: t, count, detail: None });
            }
            return out;
        }
        let f = search::fold_text(word);
        if f.chars().count() >= 2 {
            // the start of a field name: at most two field rows, the names below keep their room
            out.extend(search_fields::fields_starting(&f).into_iter().take(2).map(|(d, _)| field_row(d)));
            // count only if it really matches the result: whole library, no further
            // words, no other filters – otherwise the number would promise photos that don't appear
            let count_valid = range.0 == 0
                && range.1 == text.len()
                && not.is_empty()
                && !self.search_in_source
                && !self.filter.attributes_active()
                && !self.facets.as_ref().is_some_and(|f| f.active())
                && self.source == Source::All;
            // typed names: the chosen one goes in as field term ("person:Christa")
            if let Some(index) = &index {
                for e in rank(index, word, ROWS - 1 - out.len()) {
                    out.push(Suggestion {
                        insert: format!("{not}{}:{}", e.kind.field(), quoted(&e.name, false)),
                        range,
                        back: 0,
                        space: true,
                        kind: Some(e.kind.label()),
                        text: e.name.clone(),
                        count: count_valid.then_some(e.count),
                        detail: None,
                    });
                }
            }
            // the last row keeps the free text (full text and meaning search)
            if !out.is_empty() {
                out.push(Suggestion::plain(format!("{not}{}", quoted(word, false)), range, crate::trf!("Frei suchen: {a}", a = word)));
            }
        }
        out.truncate(ROWS);
        out
    }
}

/// One row of the popup: kind in capitals (dimmed, fixed column) or the field's base form, name, count or explanation
/// on the right. Fixed height, so the list never jumps.
pub(super) fn row(ui: &mut egui::Ui, s: &Suggestion, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
    let painter = ui.painter().clone();
    if active {
        painter.rect_filled(rect, 0.0, crate::lr::gray(0x4a));
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, crate::lr::gray(0x38));
    }
    let color = if active || resp.hovered() { crate::lr::TEXT_STRONG() } else { lr::TEXT() };
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut left = rect.left() + 8.0;
    if s.detail.is_some() {
        // field name: base form as it is inserted
        let base = s.insert.trim_start_matches('-').trim_end_matches('"').trim_end_matches('"');
        let g = painter.layout_no_wrap(base.to_string(), font.clone(), Color32::from_rgb(0x8c, 0xb4, 0xe6));
        let r = egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(left + 84.0, rect.bottom()));
        painter.with_clip_rect(r).galley(egui::pos2(left, rect.center().y - g.size().y / 2.0), g, lr::TEXT());
        left += 90.0;
    } else if let Some(kind) = s.kind {
        let g = lr::caps(&painter, kind, lr::semibold(9.5), lr::TEXT_DIM());
        let kind_rect = egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(left + 84.0, rect.bottom()));
        painter.with_clip_rect(kind_rect).galley(egui::pos2(left, rect.center().y - g.size().y / 2.0), g, lr::TEXT_DIM());
        left += 90.0;
    }
    let mut right = rect.right() - 8.0;
    if let Some(n) = s.count {
        let g = painter.layout_no_wrap(thousands(n), font.clone(), lr::TEXT_DIM());
        right -= g.size().x;
        painter.galley(egui::pos2(right, rect.center().y - g.size().y / 2.0), g, lr::TEXT_DIM());
        right -= 10.0;
    }
    let text_rect = egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(right.max(left), rect.bottom()));
    let g = painter.layout_no_wrap(s.text.clone(), font.clone(), color);
    let mut cut = g.size().x > text_rect.width();
    let name_w = g.size().x;
    painter.with_clip_rect(text_rect).galley(egui::pos2(left, rect.center().y - g.size().y / 2.0), g, color);
    if let Some(detail) = s.detail {
        // explanation after the name, dimmed, cut at the right edge
        let x = left + name_w + 12.0;
        let g = painter.layout_no_wrap(detail.to_string(), egui::FontId::proportional(10.5), lr::TEXT_DIM());
        cut |= x + g.size().x > text_rect.right();
        painter.with_clip_rect(text_rect).galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, lr::TEXT_DIM());
    }
    if cut {
        let tip = match s.detail {
            Some(d) => format!("{} · {d}", s.text),
            None => s.text.clone(),
        };
        lr::hover_tip(ui, &resp, &tip);
    }
    resp
}

/// Table of all fields for the "?" card of the search field: base form, short form, second names, explanation.
pub fn field_table(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(crate::tr("Felder")).font(lr::semibold(11.0)).color(crate::lr::gray(0xe0)));
    ui.label(egui::RichText::new(crate::tr("Grundform (Kurzform) · Zweitnamen – Erklärung. Alle Namen werden verstanden; eingefügt und gespeichert wird die Grundform.")).size(10.5).color(lr::TEXT_DIM()));
    ui.add_space(2.0);
    egui::ScrollArea::vertical().id_salt("suche-felder").max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
        for d in &search_fields::FIELD_DEFS {
            let (name, detail) = field_text(d.id);
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = ui.available_width();
            let mono = egui::TextFormat { font_id: egui::FontId::proportional(11.0), color: Color32::from_rgb(0x8c, 0xb4, 0xe6), ..Default::default() };
            let dim = egui::TextFormat { font_id: egui::FontId::proportional(10.5), color: lr::TEXT_DIM(), ..Default::default() };
            let normal = egui::TextFormat { font_id: egui::FontId::proportional(10.5), color: lr::TEXT(), ..Default::default() };
            job.append(&format!("{}:", d.id), 0.0, mono.clone());
            if !d.short.is_empty() {
                job.append(&format!(" ({}:)", d.short), 0.0, mono);
            }
            job.append(&format!("  {name}"), 0.0, normal.clone());
            job.append(&format!(" – {detail}"), 0.0, normal);
            job.append(&format!("\n{}", d.aliases.join(", ")), 0.0, dim);
            ui.label(job);
            ui.add_space(2.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every search example the help shows – syntax lines of the search card, examples in the field explanations,
    /// value lists and the values the suggestions show – is understood by the parser, in every interface language.
    #[test]
    fn help_examples_work_in_every_language() {
        use crate::menu::i18n::{locale::TestLanguage, Language};
        use library::search::{parse, value_understood, Kind};
        let understood = |q: &str| match parse(q).clauses.as_slice() {
            [c] => matches!(c.as_slice(), [t] if matches!(&t.kind, Kind::Field(f, cmp) if value_understood(f, cmp))),
            _ => false,
        };
        let seps = [", ", "、", "・", " oder ", " or ", " ou ", " o ", " u ", " または", "または"];
        let split = |text: &str| -> Vec<String> {
            let mut parts = vec![text.to_string()];
            for s in seps {
                parts = parts.iter().flat_map(|p| p.split(s).map(str::to_string).collect::<Vec<_>>()).collect();
            }
            parts.into_iter().map(|p| p.trim().trim_matches(['（', '）', '(', ')', '.']).to_string()).filter(|p| !p.is_empty()).collect()
        };
        for lang in Language::ALL {
            let _guard = TestLanguage::set(lang);
            let mut bad = Vec::new();
            // syntax lines of the search card: every token with a colon
            let card = crate::tr(crate::help::search_card_text());
            let syntax = card.split("### ").nth(2).expect("syntax section");
            for tok in syntax.split_whitespace().filter(|t| t.contains([':', '：']) && !t.starts_with('"')) {
                if !understood(tok) {
                    bad.push(tok.to_string());
                }
            }
            for d in &library::search_fields::FIELD_DEFS {
                let explanation = field_text(d.id).1;
                // "z. B. <=2.8", "e.g. sat or 6", "例: 土 または 6"
                for marker in ["z. B. ", "e.g. ", "p. ex. ", "p. ej. ", "例: ", "例："] {
                    if let Some(rest) = explanation.split(marker).nth(1) {
                        let example = rest.split_whitespace().next().unwrap_or("").trim_end_matches(['）', ')']);
                        let q = format!("{}:{example}", d.id);
                        if !understood(&q) {
                            bad.push(q);
                        }
                    }
                }
                // value lists ("picked, rejected or unflagged"); for the time of day the last item says "or hours"
                if matches!(d.id, "flag" | "ai" | "time") {
                    let mut items = split(&explanation);
                    if d.id == "time" {
                        items.pop();
                    }
                    for v in items {
                        let q = format!("{}:{}", d.id, v.to_lowercase());
                        if !understood(&q) {
                            bad.push(q);
                        }
                    }
                }
            }
            // values the suggestions show (the inserted base form always works; the shown name should as well)
            let shown: Vec<(&str, String)> = ["markiert", "abgelehnt", "ohne"]
                .iter()
                .map(|v| ("flag", crate::tr(v).to_string()))
                .chain(["gut", "mittel", "Ausschuss"].iter().map(|v| ("ai", crate::tr(v).to_string())))
                .chain(["Nacht (22–5 Uhr)", "Morgen (5–11 Uhr)", "Tagsüber (11–17 Uhr)", "Abend (17–22 Uhr)"].iter().map(|v| ("time", crate::tr(v).to_string().split(['(', '（']).next().unwrap_or("").trim().to_string())))
                .chain(["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag", "Wochenende"].iter().map(|v| ("weekday", crate::tr(v).to_string())))
                .chain(["Etikett Rot", "Etikett Gelb", "Etikett Grün", "Etikett Blau", "Etikett Lila"].iter().map(|v| ("label", crate::tr(v).to_string().split_whitespace().last().unwrap_or("").to_string())))
                .collect();
            for (field, v) in shown {
                let q = format!("{field}:{}", v.replace(' ', "-"));
                let q = if v.contains(' ') { format!("{field}:\"{v}\"") } else { q };
                if !understood(&q) {
                    bad.push(q);
                }
            }
            assert!(bad.is_empty(), "{}: not understood: {bad:?}", lang.code());
        }
    }

    #[test]
    fn ranks_start_before_word_before_inside_and_kind_then_count() {
        let index = Index {
            entries: vec![
                Entry::new(Kind::Keyword, "Christbaum".into(), 40),
                Entry::new(Kind::Person, "Christa M.".into(), 120),
                Entry::new(Kind::Place, "Sankt Christophorus".into(), 5),
                Entry::new(Kind::Keyword, "Weihnachtschrist".into(), 900),
                Entry::new(Kind::Lens, "XF 23mm".into(), 10),
            ],
        };
        let names: Vec<&str> = rank(&index, "chri", 8).iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Christa M.", "Christbaum", "Sankt Christophorus", "Weihnachtschrist"]);
        // umlauts and case do not matter, one letter is not enough
        let index = Index { entries: vec![Entry::new(Kind::Place, "Görlitz".into(), 1)] };
        assert_eq!(rank(&index, "GOR", 8).len(), 1);
        assert!(rank(&index, "g", 8).is_empty());
        assert_eq!(values(&index, Kind::Place, ""), vec![("Görlitz".to_string(), 1)]);
        assert!(values(&index, Kind::Person, "").is_empty());
    }

    fn library_with(entries: Vec<Entry>) -> (egui::Context, LibraryView) {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let ctx = egui::Context::default();
        let library = LibraryView::new(&ctx);
        {
            let mut s = library.suggest.lock().unwrap();
            s.index = Some(Arc::new(Index { entries }));
            s.running = true; // no background build in the test
        }
        (ctx, library)
    }

    #[test]
    fn typed_suggestions_insert_field_terms_and_keep_free_text() {
        let (ctx, mut library) = library_with(vec![Entry::new(Kind::Person, "Christa M.".into(), 3), Entry::new(Kind::Place, "Christchurch".into(), 1)]);
        library.filter.text = "rom chri".into();
        let list = library.suggestions(&ctx, 8);
        let inserts: Vec<&str> = list.iter().map(|s| s.insert.as_str()).collect();
        assert_eq!(inserts, vec!["person:\"Christa M.\"", "place:Christchurch", "chri"]);
        assert_eq!(list[0].kind, Some(Kind::Person.label()));
        assert!(list.len() <= ROWS);
        let mut t = library.filter.text.clone();
        assert_eq!(apply(&mut t, &list[1]), "rom place:Christchurch ".chars().count());
        assert_eq!(t, "rom place:Christchurch ");
        library.filter.text = "person:chr".into();
        let list = library.suggestions(&ctx, 10);
        assert_eq!(list.first().map(|s| s.insert.as_str()), Some("person:\"Christa M.\""));
        // the old German field name still gives values, inserted in the base form
        library.filter.text = "ort:chr".into();
        assert_eq!(library.suggestions(&ctx, 7).first().map(|s| s.insert.as_str()), Some("place:Christchurch"));
    }

    /// Field names like in an IDE: ":" lists the fields, "kam" finds camera by its German name; taking one gives
    /// `camera:""` with the cursor between the quotes, then the values fill the quotes.
    #[test]
    fn field_completion_with_cursor() {
        let (ctx, mut library) = library_with(vec![Entry::new(Kind::Camera, "X100V".into(), 9), Entry::new(Kind::Camera, "Canon EOS R5".into(), 4)]);
        library.filter.text = "rom :".into();
        let list = library.suggestions(&ctx, 5);
        assert_eq!(list.len(), ROWS);
        assert_eq!(list[0].insert, "camera:\"\"");
        library.filter.text = "rom kam".into();
        let list = library.suggestions(&ctx, 7);
        assert_eq!(list[0].insert, "camera:\"\"");
        assert_eq!(list[0].detail, Some(field_text("camera").1));
        let mut t = library.filter.text.clone();
        let cursor = apply(&mut t, &list[0]);
        assert_eq!(t, "rom camera:\"\"");
        assert_eq!(cursor, "rom camera:\"".chars().count());
        // right after: the values of the field, the cursor between the quotes
        library.filter.text = t.clone();
        let list = library.suggestions(&ctx, "rom camera:\"".len());
        let inserts: Vec<&str> = list.iter().map(|s| s.insert.as_str()).collect();
        assert_eq!(inserts, vec!["camera:\"X100V\"", "camera:\"Canon EOS R5\""]);
        let cursor = apply(&mut t, &list[1]);
        assert_eq!(t, "rom camera:\"Canon EOS R5\" ");
        assert_eq!(cursor, t.chars().count());
        // in the middle of the text: only the token under the cursor is replaced, the rest stays
        let mut t = String::from("kam rom");
        library.filter.text = t.clone();
        let list = library.suggestions(&ctx, 3);
        let cursor = apply(&mut t, &list[0]);
        assert_eq!(t, "camera:\"\" rom");
        assert_eq!(cursor, 8);
        // ":c" and the short form; a negated field keeps its minus
        library.filter.text = ":c".into();
        assert_eq!(library.suggestions(&ctx, 2)[0].insert, "camera:\"\"");
        library.filter.text = "-kw".into();
        assert_eq!(library.suggestions(&ctx, 3)[0].insert, "-keyword:\"\"");
        // weekday values with names, base form inserted
        library.filter.text = "weekday:sam".into();
        let list = library.suggestions(&ctx, 11);
        assert_eq!(list.first().map(|s| s.insert.as_str()), Some("weekday:6"));
        // cursor in white space: nothing
        library.filter.text = "a  b".into();
        assert!(library.suggestions(&ctx, 2).is_empty());
    }

    #[test]
    fn every_field_has_a_text() {
        for d in &search_fields::FIELD_DEFS {
            let (name, detail) = field_text(d.id);
            assert!(!name.is_empty() && !detail.is_empty(), "{}", d.id);
        }
    }

    #[test]
    fn index_from_a_library() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/x/', 'x', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, ''), (2, 1, '2012/2012-05-30/');
                 INSERT INTO file(id, folder, name, format, camera, lens) VALUES(1, 1, 'a.jpg', 'JPG', 'X100V', 'Fixed'), (2, 2, 'b.jpg', 'JPG', 'X100V', NULL);
                 INSERT INTO image(id, file) VALUES(1, 1), (2, 2);
                 INSERT INTO keyword(id, name, parent, person) VALUES(1, 'Orte', NULL, 0), (2, 'Rom', 1, 0), (3, 'Christa', NULL, 1), (4, 'Baum', NULL, 0), (5, 'Leer', NULL, 0), (6, 'Niemand', NULL, 1);
                 INSERT INTO image_keyword(image, keyword) VALUES(1, 2), (2, 2), (1, 3), (2, 4);
                 INSERT INTO collection(id, name, kind) VALUES(1, 'Urlaub', 0);
                 INSERT INTO collection_image(collection, image) VALUES(1, 1);",
            )
            .unwrap();
        let index = build(&db);
        let find = |kind: Kind, name: &str| index.entries.iter().find(|e| e.kind == kind && e.name == name).map(|e| e.count);
        assert_eq!(find(Kind::Place, "Rom"), Some(2));
        assert_eq!(find(Kind::Person, "Christa"), Some(1));
        assert_eq!(find(Kind::Keyword, "Baum"), Some(1));
        assert_eq!(find(Kind::Collection, "Urlaub"), Some(1));
        assert_eq!(find(Kind::Camera, "X100V"), Some(2));
        assert_eq!(find(Kind::Lens, "Fixed"), Some(1));
        assert_eq!(find(Kind::Folder, "2012-05-30"), Some(1));
        assert_eq!(find(Kind::Folder, "x"), Some(1), "the root folder by its name");
        assert!(index.entries.iter().all(|e| e.name != "Orte"), "the root of the places is no suggestion");
        assert!(index.entries.iter().all(|e| e.name != "Leer" && e.name != "Niemand"), "names without photos are left out");
    }
}
