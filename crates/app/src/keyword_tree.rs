//! The keyword column of the library filters as a foldable tree: ▸/▾ in front of terms with children, the fold
//! state kept across rebuilds and restarts (setting `keywordTreeOpen`, list of keyword ids), a search that opens
//! the parents of its hits and marks the hits. The counts are those of the filter (photos with the term or one
//! below it, [`super::Facets::apply`]); the tooltip of a parent adds how many photos carry the term itself.
//!
//! Rows keep the fixed row height of the other columns; folding only changes which rows the fixed-height list
//! shows (nothing around it moves).

use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;

use eframe::egui;
use egui::Color32;

use crate::lr;

/// Settings key of the open terms.
const OPEN_KEY: &str = "keywordTreeOpen";

/// Open terms (keyword ids), loaded once from the settings.
static OPEN: Mutex<Option<HashSet<i64>>> = Mutex::new(None);

fn with_open<T>(f: impl FnOnce(&mut HashSet<i64>) -> T) -> T {
    let mut g = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.get_or_insert_with(|| {
        library::settings::value(OPEN_KEY).as_array().map(|v| v.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default()
    });
    f(set)
}

/// Is the term with this keyword id open?
pub fn is_open(id: i64) -> bool {
    with_open(|s| s.contains(&id))
}

/// Open or close terms and remember it (settings, survives a restart).
pub fn set_open(ids: &[i64], open: bool) {
    let list = with_open(|s| {
        for id in ids {
            if open {
                s.insert(*id);
            } else {
                s.remove(id);
            }
        }
        let mut v: Vec<i64> = s.iter().copied().collect();
        v.sort_unstable();
        v
    });
    library::settings::set(OPEN_KEY, serde_json::json!(list));
}

/// A row of the tree as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub value: u32,
    pub count: usize,
    /// 0 = top level
    pub depth: usize,
    pub has_children: bool,
    pub open: bool,
    /// matches the column search (with a search: rows shown only as parents of a hit are not)
    pub hit: bool,
}

/// The visible rows of the column. `numbers`: (value, count) in tree order (parents before their children);
/// `chain(v)`: v with its parents from the top; `open(v)`: the term is open. With a search (`hits`), exactly the hits
/// and their parents are shown, the parents open whatever their stored state.
pub fn visible_rows(numbers: &[(u32, usize)], chain: &dyn Fn(u32) -> Vec<u32>, open: &dyn Fn(u32) -> bool, hits: Option<&HashSet<u32>>, no_value: u32) -> Vec<TreeRow> {
    let present: HashSet<u32> = numbers.iter().map(|x| x.0).collect();
    // terms with a child in the list
    let mut parents: HashSet<u32> = HashSet::new();
    let chains: Vec<Vec<u32>> = numbers.iter().map(|(v, _)| if *v == no_value { vec![*v] } else { chain(*v) }).collect();
    for c in &chains {
        if c.len() > 1 {
            if let Some(p) = c.get(c.len() - 2) {
                if present.contains(p) {
                    parents.insert(*p);
                }
            }
        }
    }
    let shown: Option<HashSet<u32>> = hits.map(|h| {
        let mut s: HashSet<u32> = HashSet::new();
        for (c, (v, _)) in chains.iter().zip(numbers) {
            if h.contains(v) {
                s.extend(c.iter().copied());
            }
        }
        s
    });
    let mut out = Vec::new();
    for (c, &(v, count)) in chains.iter().zip(numbers) {
        let depth = c.len().saturating_sub(1);
        let visible = match &shown {
            Some(s) => s.contains(&v),
            None => c[..depth].iter().all(|p| open(*p)),
        };
        if !visible {
            continue;
        }
        let has_children = parents.contains(&v);
        let is_open = match &shown {
            // searching: a parent of a hit is open (its hit children show)
            Some(s) => has_children && numbers.iter().zip(&chains).any(|((w, _), wc)| *w != v && wc.contains(&v) && s.contains(w)),
            None => has_children && open(v),
        };
        out.push(TreeRow { value: v, count, depth, has_children, open: is_open, hit: hits.map_or(true, |h| h.contains(&v)) });
    }
    out
}

/// What happened in the tree.
pub enum Event {
    /// the name was clicked (select as in the other columns)
    Select(u32, egui::Modifiers),
    /// the triangle was clicked; `branch`: with ⌥, the whole branch below too
    Toggle(u32, bool),
}

/// Indentation per level.
const INDENT: f32 = 10.0;
/// Width of the triangle.
const ARROW_W: f32 = 10.0;

/// Draw the rows (only those in view are made). `name_of(row)` the shown name, `tip_of(row)` the tooltip of the
/// name (empty: none), `menu` the context menu of a row.
pub fn show(
    ui: &mut egui::Ui,
    salt: (&str, usize),
    rows: &[TreeRow],
    name_of: &dyn Fn(&TreeRow) -> String,
    tip_of: &dyn Fn(&TreeRow) -> String,
    selection: &BTreeSet<u32>,
    mut menu: Option<&mut dyn FnMut(&egui::Response, u32)>,
) -> Option<Event> {
    let mut event = None;
    let row_h = super::ROW_H;
    egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false; 2]).show_rows(ui, row_h, rows.len(), |ui, range| {
        for i in range {
            let row = &rows[i];
            let is_on = selection.contains(&row.value);
            let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
            let arrow = egui::Rect::from_min_size(egui::pos2(r.left() + 2.0 + row.depth as f32 * INDENT, r.top()), egui::vec2(ARROW_W, row_h));
            let over_arrow = row.has_children && resp.hover_pos().is_some_and(|p| arrow.contains(p));
            let p = ui.painter();
            if is_on {
                p.rect_filled(r, 0.0, Color32::from_gray(0x4a));
            } else if resp.hovered() && !over_arrow {
                p.rect_filled(r, 0.0, Color32::from_gray(0x2a));
            }
            let dim = row.count == 0 || !row.hit;
            let color = if dim && !is_on { Color32::from_gray(0x7a) } else if is_on || resp.hovered() { Color32::WHITE } else { lr::TEXT() };
            let font = egui::FontId::proportional(10.5);
            if row.has_children {
                let c = if over_arrow { Color32::WHITE } else { Color32::from_gray(0x8a) };
                // the same triangle as the collection tree of the left panel
                let m = arrow.center();
                let pts = if row.open { vec![m + egui::vec2(-3.5, -2.0), m + egui::vec2(3.5, -2.0), m + egui::vec2(0.0, 2.5)] } else { vec![m + egui::vec2(-2.0, -3.5), m + egui::vec2(2.5, 0.0), m + egui::vec2(-2.0, 3.5)] };
                p.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
            }
            let right = r.right() - 2.0;
            let count_galley = p.layout_no_wrap(crate::library_ui::thousands(row.count), font.clone(), lr::TEXT_DIM());
            let space = egui::Rect::from_min_max(egui::pos2(arrow.right() + 2.0, r.top()), egui::pos2(right - count_galley.size().x - 4.0, r.bottom()));
            let name_galley = p.layout_no_wrap(name_of(row), font, color);
            let name_pos = egui::pos2(space.left(), r.center().y - name_galley.size().y / 2.0);
            let name_w = name_galley.size().x;
            p.with_clip_rect(space).galley(name_pos, name_galley, color);
            // a hit of the column search is underlined (its parents shown only for context are dimmed)
            if row.hit && !dim && ui.data(|d| d.get_temp::<bool>(egui::Id::unique((salt.0, salt.1, "searching")))).unwrap_or(false) {
                let y = r.bottom() - 2.5;
                p.with_clip_rect(space).line_segment([egui::pos2(name_pos.x, y), egui::pos2(name_pos.x + name_w, y)], egui::Stroke::new(1.0, Color32::from_gray(0xb0)));
            }
            p.galley(egui::pos2(right - count_galley.size().x, r.center().y - count_galley.size().y / 2.0), count_galley, lr::TEXT_DIM());
            if over_arrow {
                let tip = if row.open { "Zuklappen (⌥-Klick: mit allen Unterbegriffen)" } else { "Aufklappen (⌥-Klick: mit allen Unterbegriffen)" };
                lr::hover_tip(ui, &resp, crate::tr(tip));
            } else {
                lr::hover_tip(ui, &resp, &tip_of(row));
            }
            if let Some(m) = menu.as_mut() {
                m(&resp, row.value);
            }
            if resp.clicked() {
                let mods = ui.input(|i| i.modifiers);
                let on_arrow = row.has_children && resp.interact_pointer_pos().is_some_and(|p| arrow.contains(p));
                event = Some(if on_arrow { Event::Toggle(row.value, mods.alt) } else { Event::Select(row.value, mods) });
            }
        }
    });
    event
}

/// Mark the list as searching (hits are underlined); called before [`show`].
pub fn set_searching(ui: &egui::Ui, salt: (&str, usize), on: bool) {
    ui.data_mut(|d| d.insert_temp(egui::Id::unique((salt.0, salt.1, "searching")), on));
}

impl super::Facets {
    /// The keyword column: search field and the foldable tree. Returns: selection changed.
    pub(super) fn keyword_list(&mut self, ui: &mut egui::Ui, w: f32) -> bool {
        let k = super::KEYWORD_COL;
        let salt = ("bib-facette", k);
        // test aid (screenshots): GRAINEO_KEYWORD_SEARCH fills the column search once
        static TEST_SEARCH: std::sync::Once = std::sync::Once::new();
        TEST_SEARCH.call_once(|| {
            if let Ok(t) = std::env::var("GRAINEO_KEYWORD_SEARCH") {
                self.search[k] = t;
            }
        });
        crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut self.search[k]).hint_text(crate::tr("Suchen")).font(egui::FontId::proportional(10.5)).desired_width(w - 8.0).margin(egui::vec2(4.0, 1.0)));
        let pattern = self.search[k].trim().to_lowercase();
        // selected values stay visible so they can be deselected
        let hits: Option<HashSet<u32>> =
            (!pattern.is_empty()).then(|| self.numbers[k].iter().map(|x| x.0).filter(|v| self.selection[k].contains(v) || self.display_name(k, *v).to_lowercase().contains(&pattern)).collect());
        let id_of = |v: u32| self.kw_ids.get(v as usize).map(|x| x.0);
        let rows = visible_rows(&self.numbers[k], &|v| self.kw_chain(v), &|v| id_of(v).is_some_and(is_open), hits.as_ref(), super::NO_VALUE);
        set_searching(ui, salt, hits.is_some());
        let name_of = |r: &TreeRow| self.display_name(k, r.value);
        let tip_of = |r: &TreeRow| -> String {
            if r.value == super::NO_VALUE {
                return String::new();
            }
            let mut parts: Vec<String> = Vec::new();
            // a name in several branches: which one this is
            if self.kw_ambiguous.contains(&r.value) && r.depth > 0 {
                parts.push(self.kw_path(r.value));
            }
            if r.has_children {
                let direct = self.kw_direct.get(r.value as usize).copied().unwrap_or(0);
                parts.push(crate::trf!("{a} mit Unterbegriffen · {b} direkt vergeben", a = crate::library_ui::thousands(r.count), b = crate::library_ui::thousands(direct)));
            }
            parts.join("\n")
        };
        // rename, move, delete in the context menu of a row
        let kw_ids = self.kw_ids.clone();
        let mut request = None;
        let mut menu = |resp: &egui::Response, value: u32| {
            let Some(&(id, has_parent)) = kw_ids.get(value as usize) else { return };
            resp.context_menu(|ui| {
                let arrange_key = crate::shortcuts::display("arrange_keywords");
                for (action, text, key, on) in [
                    (super::KeywordAction::Rename, "Umbenennen …", String::new(), true),
                    (super::KeywordAction::NewChild, "Neuer Unterbegriff …", String::new(), true),
                    (super::KeywordAction::Move, "Verschieben nach …", String::new(), true),
                    (super::KeywordAction::ToTop, "Auf oberste Ebene", String::new(), has_parent),
                    (super::KeywordAction::Arrange, "Stichwörter ordnen …", arrange_key, true),
                    (super::KeywordAction::Delete, "Stichwort löschen …", String::new(), true),
                ] {
                    if matches!(action, super::KeywordAction::Delete | super::KeywordAction::Arrange) {
                        ui.separator();
                    }
                    if lr::menu_item(ui, crate::tr(text), &key, on) {
                        request = Some((id, action));
                    }
                }
            });
        };
        let event = show(ui, salt, &rows, &name_of, &tip_of, &self.selection[k], Some(&mut menu));
        if request.is_some() {
            self.keyword_request = request;
        }
        match event {
            None => false,
            Some(Event::Toggle(v, branch)) => {
                let Some(id) = id_of(v) else { return false };
                let open = !is_open(id);
                let mut ids = vec![id];
                if branch {
                    // ⌥: the whole branch below opens or closes with it
                    ids.extend((0..self.kw_parent.len() as u32).filter(|x| *x != v && self.kw_chain(*x).contains(&v)).filter_map(id_of));
                }
                set_open(&ids, open);
                false
            }
            Some(Event::Select(value, mods)) => {
                let order: Vec<u32> = rows.iter().map(|r| r.value).collect();
                super::list_click(&mut self.selection[k], &order, &mut self.anchor[k], value, mods);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tree Orte › Italien › Rom, Orte › Paris, Personen › Paris, Hund (values 0…5).
    fn tree() -> (Vec<(u32, usize)>, impl Fn(u32) -> Vec<u32>) {
        let parent = |v: u32| -> Option<u32> {
            match v {
                1 => Some(0),
                2 => Some(1),
                3 => Some(0),
                5 => Some(4),
                _ => None,
            }
        };
        let chain = move |v: u32| {
            let mut c = vec![v];
            let mut cur = parent(v);
            while let Some(p) = cur {
                c.push(p);
                cur = parent(p);
            }
            c.reverse();
            c
        };
        // tree order: Orte, Italien, Rom, Paris (Orte), Personen, Paris (Personen), Hund
        (vec![(0, 9), (1, 4), (2, 3), (3, 5), (4, 2), (5, 2), (6, 7)], chain)
    }

    #[test]
    fn closed_terms_hide_their_children_and_open_ones_show_them() {
        let (numbers, chain) = tree();
        let closed = visible_rows(&numbers, &chain, &|_| false, None, u32::MAX);
        assert_eq!(closed.iter().map(|r| r.value).collect::<Vec<_>>(), vec![0, 4, 6]);
        assert!(closed[0].has_children && !closed[0].open && !closed[2].has_children);
        let open = visible_rows(&numbers, &chain, &|v| v == 0, None, u32::MAX);
        assert_eq!(open.iter().map(|r| (r.value, r.depth)).collect::<Vec<_>>(), vec![(0, 0), (1, 1), (3, 1), (4, 0), (6, 0)]);
        assert!(open[1].has_children && !open[1].open, "Italien has Rom below, still closed");
    }

    #[test]
    fn search_opens_the_parents_of_its_hits() {
        let (numbers, chain) = tree();
        // "Paris": both hits with their parents, everything else hidden, parents open but not hits
        let hits = HashSet::from([3, 5]);
        let rows = visible_rows(&numbers, &chain, &|_| false, Some(&hits), u32::MAX);
        assert_eq!(rows.iter().map(|r| (r.value, r.hit, r.open)).collect::<Vec<_>>(), vec![(0, false, true), (3, true, false), (4, false, true), (5, true, false)]);
        // "Rom": two levels opened, Paris below Orte hidden
        let hits = HashSet::from([2]);
        let rows = visible_rows(&numbers, &chain, &|_| false, Some(&hits), u32::MAX);
        assert_eq!(rows.iter().map(|r| r.value).collect::<Vec<_>>(), vec![0, 1, 2]);
    }
}
