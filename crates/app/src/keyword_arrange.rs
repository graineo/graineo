//! Dialog "Stichwörter ordnen …" (⇧⌘K, context menu of the keyword column): the whole keyword tree with search,
//! multi-selection, drag and drop (onto a row: below it; with ⌥: merge into it; onto "Oberste Ebene": to the top),
//! a properties column with the actions for the selection, and one main button. Every change runs on the library's
//! writer thread and is one step of the management's ⌘Z stack; questions (merge, delete) come as their own dialogs
//! above this one. Nothing blocks while drawing.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::*;
use library::keyword_arrange as arrange;

/// Fixed size of the dialog content.
const WIDTH: f32 = 760.0;
const LIST_H: f32 = 430.0;
const ROW_H: f32 = 18.0;
const INDENT: f32 = 12.0;

/// Typed input of the properties column.
#[derive(Clone, Debug, PartialEq)]
enum Mode {
    None,
    Rename(String),
    NewChild(String),
    NewParent(String),
    /// choose a target in a filtered list: move below it, or merge into it
    Pick { merge: bool, filter: String },
}

/// State of the open dialog.
pub(crate) struct Arrange {
    search: String,
    people: bool,
    by_count: bool,
    open: HashSet<i64>,
    selected: BTreeSet<i64>,
    anchor: Option<i64>,
    mode: Mode,
    /// photos with the keyword or one below it (background query)
    counts: Option<HashMap<i64, usize>>,
    /// keywords marked "never export" themselves (loaded with the counts)
    marks: HashSet<i64>,
    /// state of the keyword list the counts belong to
    counts_for: u64,
    counts_pending: bool,
    /// ⌘Z stack length when opened: the footer counts the changes made here
    undo_start: usize,
    message: Option<String>,
    scroll_to: Option<i64>,
    /// the list of suggestions for ordering (own window above the dialog)
    suggest: Option<Suggest>,
}

/// Dragged keywords (egui drag and drop payload).
#[derive(Clone, Debug)]
struct KeywordDrag(Vec<i64>);

/// A visible row of the tree.
struct Row {
    id: i64,
    depth: usize,
    has_children: bool,
    open: bool,
    hit: bool,
}

/// What a drop does.
#[derive(Clone, Copy, PartialEq)]
enum Drop {
    Below(Option<i64>),
    Merge(i64),
}

impl Arrange {
    fn new(focus: Option<i64>, keywords: &[Keyword], undo_start: usize) -> Self {
        let mut open = HashSet::new();
        let mut selected = BTreeSet::new();
        if let Some(f) = focus {
            selected.insert(f);
            let mut cur = keywords.iter().find(|k| k.id == f).and_then(|k| k.parent);
            while let Some(p) = cur {
                if !open.insert(p) {
                    break;
                }
                cur = keywords.iter().find(|k| k.id == p).and_then(|k| k.parent);
            }
        }
        Arrange { search: String::new(), people: false, by_count: false, open, selected, anchor: focus, mode: Mode::None, counts: None, marks: HashSet::new(), counts_for: 0, counts_pending: false, undo_start, message: None, scroll_to: focus, suggest: None }
    }
}

/// A cheap fingerprint of the keyword list (changes after every change of the tree or the assignments).
fn fingerprint(keywords: &[Keyword]) -> u64 {
    let mut h: u64 = keywords.len() as u64;
    for k in keywords {
        h = h.wrapping_mul(1_000_003).wrapping_add(k.id as u64 ^ (k.parent.unwrap_or(0) as u64) << 20 ^ (k.count as u64) << 40);
        h = h.wrapping_add(k.name.len() as u64);
    }
    h
}

/// Tree in memory for one frame.
struct Tree<'a> {
    by_id: HashMap<i64, &'a Keyword>,
    children: HashMap<Option<i64>, Vec<i64>>,
}

impl<'a> Tree<'a> {
    fn new(keywords: &'a [Keyword], people: bool, by_count: bool, counts: Option<&HashMap<i64, usize>>) -> Self {
        let by_id: HashMap<i64, &Keyword> = keywords.iter().map(|k| (k.id, k)).collect();
        let mut children: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
        for k in keywords {
            if k.person && !people {
                continue;
            }
            // a parent that does not exist (damaged data) counts as the top level
            let p = k.parent.filter(|p| by_id.contains_key(p));
            children.entry(p).or_default().push(k.id);
        }
        let count = |id: i64| counts.and_then(|c| c.get(&id).copied()).unwrap_or_else(|| by_id.get(&id).map_or(0, |k| k.count));
        for v in children.values_mut() {
            if by_count {
                v.sort_by(|a, b| count(*b).cmp(&count(*a)).then_with(|| by_id[a].name.to_lowercase().cmp(&by_id[b].name.to_lowercase())));
            } else {
                v.sort_by_key(|id| (by_id[id].name.to_lowercase(), *id));
            }
        }
        Tree { by_id, children }
    }

    fn chain(&self, id: i64) -> Vec<i64> {
        let mut v = vec![id];
        let mut cur = self.by_id.get(&id).and_then(|k| k.parent);
        while let Some(p) = cur {
            if v.len() >= library::keywords::MAX_DEPTH || v.contains(&p) || !self.by_id.contains_key(&p) {
                break;
            }
            v.push(p);
            cur = self.by_id.get(&p).and_then(|k| k.parent);
        }
        v.reverse();
        v
    }

    fn path(&self, id: i64) -> String {
        self.chain(id).iter().filter_map(|i| self.by_id.get(i).map(|k| k.name.clone())).collect::<Vec<_>>().join(library::keywords::SHOWN_SEPARATOR)
    }

    fn below(&self, roots: &[i64]) -> HashSet<i64> {
        let mut out: HashSet<i64> = HashSet::new();
        let mut stack: Vec<i64> = roots.to_vec();
        while let Some(k) = stack.pop() {
            if out.insert(k) {
                stack.extend(self.children.get(&Some(k)).into_iter().flatten().copied());
            }
        }
        out
    }

    /// Visible rows: depth-first, children of open terms; with a search the hits and their parents (open).
    fn rows(&self, open: &HashSet<i64>, search: &str) -> Vec<Row> {
        let pattern = library::search::fold_text(search.trim());
        let shown: Option<(HashSet<i64>, HashSet<i64>)> = (!pattern.is_empty()).then(|| {
            let hits: HashSet<i64> = self.children.values().flatten().copied().filter(|id| library::search::fold_text(&self.by_id[id].name).contains(&pattern)).collect();
            let mut with_parents = HashSet::new();
            for h in &hits {
                with_parents.extend(self.chain(*h));
            }
            (hits, with_parents)
        });
        let mut out = Vec::new();
        let mut stack: Vec<(i64, usize)> = self.children.get(&None).into_iter().flatten().rev().map(|id| (*id, 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            if let Some((_, visible)) = &shown {
                if !visible.contains(&id) {
                    continue;
                }
            }
            let kids = self.children.get(&Some(id));
            let has_children = kids.is_some_and(|k| !k.is_empty());
            let is_open = has_children && match &shown {
                Some((hits, visible)) => kids.into_iter().flatten().any(|c| visible.contains(c)) && (!hits.contains(&id) || open.contains(&id) || kids.into_iter().flatten().any(|c| visible.contains(c))),
                None => open.contains(&id),
            };
            out.push(Row { id, depth, has_children, open: is_open, hit: shown.as_ref().map_or(true, |(h, _)| h.contains(&id)) });
            if is_open && depth < library::keywords::MAX_DEPTH {
                for c in kids.into_iter().flatten().rev() {
                    stack.push((*c, depth + 1));
                }
            }
        }
        out
    }
}

impl LibraryView {
    /// Open "Stichwörter ordnen …", optionally with this keyword chosen (its parents open).
    pub(crate) fn open_arrange(&mut self, focus: Option<i64>) {
        self.manage.arrange = Some(Arrange::new(focus, &self.keywords, self.manage.undo.len()));
    }

    /// Open the dialog on this keyword with the input for a new term below it.
    pub(crate) fn open_arrange_with_child(&mut self, id: i64) {
        self.open_arrange(Some(id));
        if let Some(a) = &mut self.manage.arrange {
            a.mode = Mode::NewChild(String::new());
        }
    }

    /// ⇧⌘K and the dialog itself (called every frame from the management's dialogs).
    pub(super) fn arrange_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // test aid (screenshots): GRAINEO_KEYWORD_ARRANGE=<keyword id or 0> opens the dialog once, after the keywords
        // loaded; GRAINEO_KEYWORD_ARRANGE_SELECT=<ids> selects more
        static TEST_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !self.keywords.is_empty() && !TEST_OPEN.swap(true, std::sync::atomic::Ordering::Relaxed) {
            if let Some(id) = std::env::var("GRAINEO_KEYWORD_ARRANGE").ok().and_then(|v| v.parse::<i64>().ok()) {
                self.open_arrange((id > 0).then_some(id));
                if let (Some(a), Ok(s)) = (&mut self.manage.arrange, std::env::var("GRAINEO_KEYWORD_ARRANGE_SELECT")) {
                    a.selected.extend(s.split(',').filter_map(|x| x.trim().parse::<i64>().ok()));
                }
                // GRAINEO_KEYWORD_ARRANGE_ASK=delete (question for the focused keyword) | parent=<name> (input filled)
                match std::env::var("GRAINEO_KEYWORD_ARRANGE_ASK").as_deref() {
                    Ok("delete") if id > 0 => self.ask_delete_keyword(id),
                    Ok("suggest") => {
                        if let Some(a) = &mut self.manage.arrange {
                            a.suggest = Some(Suggest::default());
                        }
                        self.load_suggestions();
                    }
                    Ok(p) if p.starts_with("parent=") => {
                        if let Some(a) = &mut self.manage.arrange {
                            a.mode = Mode::NewParent(p[7..].to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        if self.manage.arrange.is_none() {
            if self.manage.ask.is_none() && crate::shortcuts::pressed(ctx, "arrange_keywords") {
                self.open_arrange(None);
            } else {
                return;
            }
        }
        let Some(mut a) = self.manage.arrange.take() else { return };
        // counts with everything below: in the background, again after every change
        // also after a step that changes no keyword row (export mark): the ⌘Z stack length is part of it
        let fp = fingerprint(&self.keywords) ^ (self.manage.undo.len() as u64).wrapping_mul(0x9e37_79b9);
        if a.counts_for != fp && !a.counts_pending {
            a.counts_pending = true;
            let started = self.in_background(
                move |db| Ok((library::keyword_arrange::subtree_counts(&db.conn)?, library::keyword_arrange::export_marks(&db.conn)?)),
                move |b, _, r| {
                    if let Some(a) = &mut b.manage.arrange {
                        a.counts_pending = false;
                        a.counts_for = fp;
                        if let Ok((c, m)) = r {
                            a.counts = Some(c);
                            a.marks = m;
                        }
                    }
                },
            );
            if !started {
                a.counts_pending = false;
            }
        }
        let keep = self.arrange_window(ctx, &mut a, actions);
        // a question (merge, delete) may have been opened; the dialog stays below it
        if keep && self.manage.arrange.is_none() {
            self.manage.arrange = Some(a);
        }
    }

    fn arrange_window(&mut self, ctx: &egui::Context, a: &mut Arrange, actions: &mut Vec<Action>) -> bool {
        let keywords = std::mem::take(&mut self.keywords);
        // the counts are lent to this frame (the state itself is changed while drawing)
        let counts = a.counts.take();
        let tree = Tree::new(&keywords, a.people, a.by_count, counts.as_ref());
        a.selected.retain(|id| tree.by_id.contains_key(id));
        let rows = tree.rows(&a.open, &a.search);
        let count = |id: i64| -> usize { counts.as_ref().and_then(|c| c.get(&id).copied()).unwrap_or_else(|| tree.by_id.get(&id).map_or(0, |k| k.count)) };
        let mut request: Option<Request> = None;
        let mut done = false;
        let lr_catalog = self.db.as_ref().and_then(library::lightroom_catalog).is_some_and(|p| p.exists());
        let frame = egui::Frame::new().fill(Color32::from_gray(0x1e)).stroke(egui::Stroke::new(1.0, Color32::from_gray(0x3a))).corner_radius(0.0).inner_margin(egui::Margin::same(14));
        let modal = egui::Modal::new(egui::Id::unique("keyword-arrange")).frame(frame).backdrop_color(Color32::from_black_alpha(150)).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add(egui::Label::new(egui::RichText::new(crate::tr("Stichwörter ordnen")).font(lr::semibold(15.0)).color(Color32::from_gray(0xee))));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                lr::edit_field(ui, egui::TextEdit::singleline(&mut a.search).hint_text(crate::tr("Suchen")).desired_width(300.0));
                ui.add_space(8.0);
                let sort = if a.by_count { crate::tr("Häufigkeit") } else { crate::tr("Name") };
                lr::field_label(ui, crate::tr("Sortieren"));
                egui::ComboBox::from_id_salt("kw-arrange-sort").selected_text(sort).width(110.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut a.by_count, false, crate::tr("Name"));
                    ui.selectable_value(&mut a.by_count, true, crate::tr("Häufigkeit"));
                });
                ui.add_space(8.0);
                let r = ui.checkbox(&mut a.people, crate::tr("Personen zeigen"));
                lr::hover_tip(ui, &r, crate::tr("Personen sind Stichwörter mit Gesichtern; sie bleiben sonst ausgeblendet, damit sie nicht aus Versehen umgehängt werden."));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // "never export" marks of Lightroom for keywords imported before (only on request)
                    if lr_catalog {
                        let b = ui.button(crate::tr("Marken aus Lightroom …"));
                        lr::hover_tip(ui, &b, crate::tr("Stichwörter, die Lightroom nicht exportiert, hier als „nicht exportieren“ markieren (der Katalog wird nur gelesen)"));
                        if b.clicked() {
                            request = Some(Request::LightroomMarks);
                        }
                    }
                    let b = ui.button(crate::tr("Vorschläge …"));
                    lr::hover_tip(ui, &b, crate::tr("Vorschläge zum Ordnen: Schreibweisen, Einzahl/Mehrzahl, Mehrwort-Namen, fast immer gemeinsam vergebene Stichwörter"));
                    if b.clicked() {
                        request = Some(Request::Suggest);
                    }
                });
            });
            // fixed body: the list on the left, the properties on the right (nothing grows or jumps)
            let list_w = 440.0;
            let body_h = LIST_H + ROW_H + 8.0;
            let (body, _) = ui.allocate_exact_size(egui::vec2(WIDTH, body_h), egui::Sense::hover());
            let left = egui::Rect::from_min_size(body.min, egui::vec2(list_w, body_h));
            let right = egui::Rect::from_min_max(egui::pos2(body.min.x + list_w + 16.0, body.min.y), body.max);
            ui.painter().vline(body.min.x + list_w + 8.0, body.y_range(), egui::Stroke::new(1.0, Color32::from_gray(0x3a)));
            let mut lui = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::top_down(egui::Align::Min)));
            lui.set_clip_rect(left);
            self.arrange_list(&mut lui, a, &tree, &rows, &count, &mut request);
            let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::top_down(egui::Align::Min)));
            rui.set_clip_rect(right);
            arrange_properties(&mut rui, a, &tree, &count, &mut request);
            ui.separator();
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if lr::main_button_min(ui, crate::tr("Fertig"), true, 110.0).clicked() {
                    done = true;
                }
                ui.label(egui::RichText::new(crate::tr("Esc")).small().color(lr::TEXT_DIM));
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let n = self.manage.undo.len().saturating_sub(a.undo_start);
                    let status = match (drag_status(ui, &tree), &a.message) {
                        (Some(s), _) => s,
                        (None, Some(m)) => m.clone(),
                        (None, None) if n > 0 => crate::trf!("{n} Änderungen · ⌘Z nimmt sie einzeln zurück", n = n),
                        (None, None) => String::new(),
                    };
                    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(&status)).small().color(lr::TEXT_DIM)).truncate());
                });
            });
        });
        // the suggestions lie above the dialog
        if a.suggest.is_some() {
            self.suggest_window(ctx, a);
        }
        // keys of the list (not while typing, not below a question)
        if modal.is_top_modal && !ctx.memory(|m| m.focused().is_some()) {
            arrange_keys(ctx, a, &tree, &rows, &mut request);
        }
        if modal.is_top_modal && a.mode == Mode::None && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            done = true;
        } else if modal.is_top_modal && a.mode != Mode::None && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            a.mode = Mode::None;
        }
        drop(tree);
        self.keywords = keywords;
        if a.counts.is_none() {
            a.counts = counts;
        }
        if let Some(r) = request {
            a.message = None;
            done |= self.arrange_request(a, r, actions);
        }
        !done
    }

    /// The tree list with the drop zone "Oberste Ebene" above it.
    fn arrange_list(&self, ui: &mut egui::Ui, a: &mut Arrange, tree: &Tree, rows: &[Row], count: &dyn Fn(i64) -> usize, request: &mut Option<Request>) {
        // rows without gaps: the list scrolls by whole rows (show_rows)
        ui.spacing_mut().item_spacing.y = 0.0;
        // drop zone: to the top level
        let (top, top_resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::hover());
        let dragging = egui::DragAndDrop::payload::<KeywordDrag>(ui.ctx());
        let top_ok = dragging.as_ref().is_some_and(|d| d.0.iter().any(|k| tree.by_id.get(k).is_some_and(|k| k.parent.is_some())));
        ui.painter().text(top.left_center() + egui::vec2(3.0, 0.0), egui::Align2::LEFT_CENTER, crate::tr("Oberste Ebene"), egui::FontId::proportional(10.5), if top_ok { lr::TEXT } else { lr::TEXT_DIM });
        if top_resp.dnd_hover_payload::<KeywordDrag>().is_some() && top_ok {
            ui.painter().rect_stroke(top, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0xe0)), egui::StrokeKind::Inside);
            set_drag_status(ui, crate::trf!("{a} auf die oberste Ebene legen", a = dragged_names(tree, dragging.as_deref().map(|d| d.0.as_slice()).unwrap_or(&[]))));
        }
        if top_ok {
            if let Some(d) = top_resp.dnd_release_payload::<KeywordDrag>() {
                *request = Some(Request::Drop(d.0.clone(), Drop::Below(None)));
            }
        }
        let alt = ui.input(|i| i.modifiers.alt);
        let mut scroll = egui::ScrollArea::vertical().id_salt("kw-arrange-list").auto_shrink([false; 2]).max_height(LIST_H);
        if let Some(target) = a.scroll_to.take() {
            if let Some(i) = rows.iter().position(|r| r.id == target) {
                scroll = scroll.vertical_scroll_offset((i as f32 * ROW_H - LIST_H / 2.0).max(0.0));
            }
        }
        scroll.show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for i in range {
                let row = &rows[i];
                let Some(k) = tree.by_id.get(&row.id) else { continue };
                let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click_and_drag());
                let arrow = egui::Rect::from_min_size(egui::pos2(r.left() + 2.0 + row.depth as f32 * INDENT, r.top()), egui::vec2(12.0, ROW_H));
                let over_arrow = row.has_children && resp.hover_pos().is_some_and(|p| arrow.contains(p));
                let on = a.selected.contains(&row.id);
                let p = ui.painter();
                if on {
                    p.rect_filled(r, 0.0, Color32::from_gray(0x4a));
                } else if resp.hovered() && !over_arrow {
                    p.rect_filled(r, 0.0, Color32::from_gray(0x2a));
                }
                if row.has_children {
                    let m = arrow.center();
                    let pts = if row.open { vec![m + egui::vec2(-3.5, -2.0), m + egui::vec2(3.5, -2.0), m + egui::vec2(0.0, 2.5)] } else { vec![m + egui::vec2(-2.0, -3.5), m + egui::vec2(2.5, 0.0), m + egui::vec2(-2.0, 3.5)] };
                    p.add(egui::Shape::convex_polygon(pts, if over_arrow { Color32::WHITE } else { Color32::from_gray(0x8a) }, egui::Stroke::NONE));
                }
                let n = count(row.id);
                let dim = !row.hit || n == 0;
                let color = if on || resp.hovered() { Color32::WHITE } else if dim { Color32::from_gray(0x7a) } else { lr::TEXT };
                let font = egui::FontId::proportional(10.5);
                let count_g = p.layout_no_wrap(crate::library_ui::thousands(n), font.clone(), lr::TEXT_DIM);
                let space = egui::Rect::from_min_max(egui::pos2(arrow.right() + 2.0, r.top()), egui::pos2(r.right() - count_g.size().x - 24.0, r.bottom()));
                let label = if k.person { format!("{} ·", k.name) } else { k.name.clone() };
                let g = p.layout_no_wrap(label, font, color);
                p.with_clip_rect(space).galley(egui::pos2(space.left(), r.center().y - g.size().y / 2.0), g, color);
                let count_x = r.right() - 4.0 - count_g.size().x;
                p.galley(egui::pos2(count_x, r.center().y - count_g.size().y / 2.0), count_g, lr::TEXT_DIM);
                // marked "never export" (itself; the branch below inherits it)
                if a.marks.contains(&row.id) {
                    p.text(egui::pos2(count_x - 8.0, r.center().y), egui::Align2::RIGHT_CENTER, "⊘", egui::FontId::proportional(10.5), lr::TEXT_DIM);
                }
                // drag: the selection if the row belongs to it, else this row
                if resp.drag_started() {
                    let ids: Vec<i64> = if on { a.selected.iter().copied().collect() } else { vec![row.id] };
                    resp.dnd_set_drag_payload(KeywordDrag(ids));
                }
                if let Some(d) = resp.dnd_hover_payload::<KeywordDrag>() {
                    let target = if alt { Drop::Merge(row.id) } else { Drop::Below(Some(row.id)) };
                    match drop_problem(tree, &d.0, target) {
                        None => {
                            let c = if alt { Color32::from_rgb(0x6a, 0xa8, 0xe8) } else { Color32::from_gray(0xe0) };
                            ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, c), egui::StrokeKind::Inside);
                            let names = dragged_names(tree, &d.0);
                            set_drag_status(ui, if alt { crate::trf!("{a} mit „{b}“ zusammenführen", a = names, b = k.name) } else { crate::trf!("{a} unter „{b}“ legen", a = names, b = k.name) });
                        }
                        Some(why) => {
                            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(110));
                            ui.ctx().set_cursor_icon(egui::CursorIcon::NoDrop);
                            set_drag_status(ui, crate::tr(why).to_string());
                        }
                    }
                }
                if let Some(d) = resp.dnd_release_payload::<KeywordDrag>() {
                    let target = if alt { Drop::Merge(row.id) } else { Drop::Below(Some(row.id)) };
                    if drop_problem(tree, &d.0, target).is_none() {
                        *request = Some(Request::Drop(d.0.clone(), target));
                    }
                }
                if over_arrow {
                    lr::hover_tip(ui, &resp, crate::tr(if row.open { "Zuklappen (⌥-Klick: mit allen Unterbegriffen)" } else { "Aufklappen (⌥-Klick: mit allen Unterbegriffen)" }));
                }
                resp.context_menu(|ui| {
                    if !on {
                        a.selected = BTreeSet::from([row.id]);
                        a.anchor = Some(row.id);
                    }
                    arrange_menu(ui, a, request);
                });
                if resp.clicked() {
                    let mods = ui.input(|i| i.modifiers);
                    if row.has_children && resp.interact_pointer_pos().is_some_and(|p| arrow.contains(p)) {
                        let ids: Vec<i64> = if mods.alt { tree.below(&[row.id]).into_iter().collect() } else { vec![row.id] };
                        let open = !row.open;
                        for id in ids {
                            if open {
                                a.open.insert(id);
                            } else {
                                a.open.remove(&id);
                            }
                        }
                    } else {
                        click(a, rows, row.id, mods);
                        a.mode = Mode::None;
                    }
                }
                if resp.double_clicked() && !over_arrow {
                    a.mode = Mode::Rename(k.name.clone());
                }
            }
        });
    }

    /// Carry out what the dialog asked for: questions open above it, changes run as management jobs (⌘Z).
    /// Returns: the dialog closes ("Fotos zeigen").
    fn arrange_request(&mut self, a: &mut Arrange, r: Request, _actions: &mut Vec<Action>) -> bool {
        let sel: Vec<i64> = a.selected.iter().copied().collect();
        let name = |s: &Self, id: i64| s.keywords.iter().find(|k| k.id == id).map(|k| k.name.clone()).unwrap_or_default();
        match r {
            Request::Mode(m) => a.mode = m,
            Request::Drop(ids, Drop::Below(parent)) => self.arrange_move(a, ids, parent),
            Request::Drop(ids, Drop::Merge(into)) => self.ask_merge(ids, into),
            Request::Submit => match std::mem::replace(&mut a.mode, Mode::None) {
                Mode::Rename(new) if sel.len() == 1 => {
                    let id = sel[0];
                    let old = name(self, id);
                    let new = new.trim().to_string();
                    if new.is_empty() || new == old {
                        return false;
                    }
                    let parent = self.keywords.iter().find(|k| k.id == id).and_then(|k| k.parent);
                    match self.keywords.iter().find(|k| k.id != id && k.parent == parent && k.name.trim().to_lowercase() == new.to_lowercase()) {
                        Some(o) => self.manage.ask = Some(Ask::MergeKeyword { from: id, into: o.id }),
                        None => self.keyword_job(crate::trf!("„{a}“ umbenannt in „{b}“", a = old, b = new), move |db| arrange::rename(db, id, &new)),
                    }
                }
                Mode::NewChild(text) => {
                    let parent = if sel.len() == 1 { Some(sel[0]) } else { None };
                    if let Some(p) = parent {
                        a.open.insert(p);
                    }
                    let label = crate::trf!("„{a}“ angelegt", a = text.trim());
                    self.keyword_job(label, move |db| arrange::new_child(db, parent, &text).map(|x| x.1));
                }
                Mode::NewParent(text) if !sel.is_empty() => {
                    let label = crate::trf!("Neuer Oberbegriff „{a}“", a = text.trim());
                    self.keyword_job(label, move |db| arrange::new_parent(db, &sel, &text).map(|x| x.1));
                }
                _ => {}
            },
            Request::Pick(target) => {
                let merge = matches!(a.mode, Mode::Pick { merge: true, .. });
                a.mode = Mode::None;
                if merge {
                    if let Some(t) = target {
                        self.ask_merge(sel, t);
                    }
                } else {
                    self.arrange_move(a, sel, target);
                }
            }
            Request::ToTop => self.arrange_move(a, sel, None),
            Request::Delete => {
                if let [id] = sel[..] {
                    self.ask_delete_keyword(id);
                }
            }
            Request::Show => {
                if let [id] = sel[..] {
                    let n = name(self, id);
                    if !n.contains('"') {
                        self.filter.text = format!("keyword:=\"{n}\"");
                        self.refilter = true;
                        return true;
                    }
                }
            }
            Request::LightroomMarks => {
                let Some(cat) = self.db.as_ref().and_then(library::lightroom_catalog) else { return false };
                self.manage.ask = Some(Ask::LightroomMarks { ids: None });
                self.in_background(
                    move |db| library::keyword_arrange::lightroom_export_marks(db, &cat),
                    |b, _, r| {
                        if let Some(Ask::LightroomMarks { ids }) = &mut b.manage.ask {
                            *ids = Some(r);
                        }
                    },
                );
            }
            Request::Suggest => {
                a.suggest = Some(Suggest::default());
                self.load_suggestions();
            }
            Request::Export(export) => {
                let label = if export { crate::trf!("{n} Stichwörter werden wieder exportiert", n = sel.len()) } else { crate::trf!("{n} Stichwörter werden nicht exportiert", n = sel.len()) };
                self.keyword_job(label, move |db| arrange::set_export(db, &sel, export));
            }
            Request::Open(id, open) => {
                if open {
                    a.open.insert(id);
                } else {
                    a.open.remove(&id);
                }
            }
        }
        false
    }

    /// Move keywords below `parent` (`None`: top level); a single one onto a name that exists there becomes the
    /// question to merge, several with such a name are refused with the reason.
    fn arrange_move(&mut self, a: &mut Arrange, ids: Vec<i64>, parent: Option<i64>) {
        let ids: Vec<i64> = ids.into_iter().filter(|id| self.keywords.iter().any(|k| k.id == *id && k.parent != parent)).collect();
        if ids.is_empty() {
            return;
        }
        if let [id] = ids[..] {
            let n = self.keywords.iter().find(|k| k.id == id).map(|k| k.name.trim().to_lowercase()).unwrap_or_default();
            if let Some(o) = self.keywords.iter().find(|k| k.id != id && k.parent == parent && k.name.trim().to_lowercase() == n) {
                self.manage.ask = Some(Ask::MergeKeyword { from: id, into: o.id });
                return;
            }
        }
        if let Some(p) = parent {
            a.open.insert(p);
        }
        let target = parent.and_then(|p| self.keywords.iter().find(|k| k.id == p)).map(|k| k.name.clone());
        let label = match (&target, ids.len()) {
            (Some(t), 1) => crate::trf!("„{a}“ unter „{b}“ gelegt", a = self.keywords.iter().find(|k| k.id == ids[0]).map(|k| k.name.clone()).unwrap_or_default(), b = t),
            (Some(t), n) => crate::trf!("{n} Stichwörter unter „{b}“ gelegt", n = n, b = t),
            (None, n) => crate::trf!("{n} Stichwörter auf die oberste Ebene gelegt", n = n),
        };
        self.keyword_job(label, move |db| arrange::move_keywords(db, &ids, parent));
    }

    /// Question before merging (one or several into `into`).
    fn ask_merge(&mut self, from: Vec<i64>, into: i64) {
        let from: Vec<i64> = from.into_iter().filter(|f| *f != into).collect();
        match from[..] {
            [] => {}
            [one] => self.manage.ask = Some(Ask::MergeKeyword { from: one, into }),
            _ => self.manage.ask = Some(Ask::MergeKeywords { from, into }),
        }
    }
}

/// What the dialog asks the library view to do (collected while drawing, carried out afterwards).
enum Request {
    Mode(Mode),
    Submit,
    Drop(Vec<i64>, Drop),
    Pick(Option<i64>),
    ToTop,
    Delete,
    Show,
    Open(i64, bool),
    /// mark the selection "never export" (false) or take the mark away (true)
    Export(bool),
    /// open the suggestions for ordering
    Suggest,
    /// take the "never export" marks over from the Lightroom catalog (question first)
    LightroomMarks,
}

fn click(a: &mut Arrange, rows: &[Row], id: i64, mods: egui::Modifiers) {
    if mods.shift {
        if let Some(anchor) = a.anchor {
            let (i, j) = (rows.iter().position(|r| r.id == anchor), rows.iter().position(|r| r.id == id));
            if let (Some(i), Some(j)) = (i, j) {
                let (lo, hi) = (i.min(j), i.max(j));
                if !mods.command {
                    a.selected.clear();
                }
                a.selected.extend(rows[lo..=hi].iter().map(|r| r.id));
                return;
            }
        }
    }
    if mods.command {
        if !a.selected.remove(&id) {
            a.selected.insert(id);
        }
    } else {
        a.selected = BTreeSet::from([id]);
    }
    a.anchor = Some(id);
}

/// Why a drop is not possible (`None`: it is).
fn drop_problem(tree: &Tree, ids: &[i64], target: Drop) -> Option<&'static str> {
    match target {
        Drop::Below(None) => None,
        Drop::Below(Some(t)) => {
            if tree.below(ids).contains(&t) {
                Some("Ein Stichwort kann nicht unter sich selbst liegen.")
            } else if ids.iter().all(|i| tree.by_id.get(i).and_then(|k| k.parent) == Some(t)) {
                Some("Liegt schon dort.")
            } else {
                None
            }
        }
        Drop::Merge(t) => {
            let person = tree.by_id.get(&t).is_some_and(|k| k.person);
            if ids.iter().all(|i| *i == t) {
                Some("Ein Stichwort lässt sich nicht mit sich selbst zusammenführen.")
            } else if tree.below(&ids.iter().copied().filter(|i| *i != t).collect::<Vec<_>>()).contains(&t) {
                Some("Das Ziel liegt unter dem Stichwort, das zusammengeführt wird.")
            } else if ids.iter().any(|i| tree.by_id.get(i).is_some_and(|k| k.person != person)) {
                Some("Ein Stichwort und eine Person lassen sich nicht zusammenführen.")
            } else {
                None
            }
        }
    }
}

fn dragged_names(tree: &Tree, ids: &[i64]) -> String {
    match ids {
        [one] => format!("„{}“", tree.by_id.get(one).map(|k| k.name.as_str()).unwrap_or("")),
        _ => crate::trf!("{n} Stichwörter", n = ids.len()),
    }
}

fn set_drag_status(ui: &egui::Ui, text: String) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("kw-arrange-drag-status"), (ui.ctx().cumulative_frame_nr(), text)));
}

/// The drop description of this frame, while keywords are dragged.
fn drag_status(ui: &egui::Ui, _tree: &Tree) -> Option<String> {
    egui::DragAndDrop::payload::<KeywordDrag>(ui.ctx())?;
    let (frame, text) = ui.ctx().data(|d| d.get_temp::<(u64, String)>(egui::Id::unique("kw-arrange-drag-status")))?;
    (frame + 1 >= ui.ctx().cumulative_frame_nr()).then_some(text)
}

/// Context menu of a row (the same actions as the properties column).
fn arrange_menu(ui: &mut egui::Ui, a: &Arrange, request: &mut Option<Request>) {
    let one = a.selected.len() == 1;
    let name = String::new();
    for (text, key, on, r) in [
        ("Umbenennen …", "↩", one, Request::Mode(Mode::Rename(name.clone()))),
        ("Neuer Unterbegriff …", "", one, Request::Mode(Mode::NewChild(String::new()))),
        ("Neuer Oberbegriff …", "", !a.selected.is_empty(), Request::Mode(Mode::NewParent(String::new()))),
        ("Verschieben nach …", "", !a.selected.is_empty(), Request::Mode(Mode::Pick { merge: false, filter: String::new() })),
        ("Zusammenführen mit …", "", !a.selected.is_empty(), Request::Mode(Mode::Pick { merge: true, filter: String::new() })),
        ("Auf oberste Ebene", "", !a.selected.is_empty(), Request::ToTop),
    ] {
        if lr::menu_item(ui, crate::tr(text), key, on) {
            *request = Some(r);
            ui.close();
        }
    }
    ui.separator();
    if lr::menu_item(ui, crate::tr("Stichwort löschen …"), "⌫", one) {
        *request = Some(Request::Delete);
        ui.close();
    }
}

/// Keys of the list: ↑/↓ choose, ←/→ close/open, ↩ rename, ⌫ delete, ⌘A all visible.
fn arrange_keys(ctx: &egui::Context, a: &mut Arrange, tree: &Tree, rows: &[Row], request: &mut Option<Request>) {
    let (up, down, left, right, enter, del, all) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            i.key_pressed(egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
        )
    });
    let cur = a.anchor.and_then(|id| rows.iter().position(|r| r.id == id));
    if up || down {
        let i = match (cur, up) {
            (Some(i), true) => i.saturating_sub(1),
            (Some(i), false) => (i + 1).min(rows.len().saturating_sub(1)),
            (None, _) => 0,
        };
        if let Some(r) = rows.get(i) {
            a.selected = BTreeSet::from([r.id]);
            a.anchor = Some(r.id);
            a.scroll_to = Some(r.id);
        }
    }
    if let Some(r) = cur.and_then(|i| rows.get(i)) {
        if left && r.open {
            *request = Some(Request::Open(r.id, false));
        } else if left {
            // to the parent
            if let Some(p) = tree.by_id.get(&r.id).and_then(|k| k.parent) {
                a.selected = BTreeSet::from([p]);
                a.anchor = Some(p);
            }
        }
        if right && r.has_children && !r.open {
            *request = Some(Request::Open(r.id, true));
        }
    }
    if enter && a.selected.len() == 1 {
        let id = *a.selected.iter().next().unwrap();
        *request = Some(Request::Mode(Mode::Rename(tree.by_id.get(&id).map(|k| k.name.clone()).unwrap_or_default())));
    }
    if del && a.selected.len() == 1 {
        *request = Some(Request::Delete);
    }
    if all {
        a.selected = rows.iter().map(|r| r.id).collect();
    }
}

/// The properties column: what is chosen, and the actions (text buttons).
fn arrange_properties(ui: &mut egui::Ui, a: &mut Arrange, tree: &Tree, count: &dyn Fn(i64) -> usize, request: &mut Option<Request>) {
    let sel: Vec<i64> = a.selected.iter().copied().collect();
    let heading = |ui: &mut egui::Ui, t: &str| {
        ui.add(egui::Label::new(egui::RichText::new(t).font(lr::semibold(13.0)).color(Color32::from_gray(0xee))).truncate());
    };
    match &sel[..] {
        [] => {
            heading(ui, crate::tr("Kein Stichwort gewählt"));
            ui.label(egui::RichText::new(crate::trf!("{n} Stichwörter", n = crate::library_ui::thousands(tree.by_id.values().filter(|k| a.people || !k.person).count()))).small().color(lr::TEXT_DIM));
        }
        [id] => {
            let Some(k) = tree.by_id.get(id) else { return };
            heading(ui, &k.name);
            if k.parent.is_some() {
                ui.add(egui::Label::new(egui::RichText::new(tree.path(*id)).small().color(lr::TEXT_DIM)).wrap());
            }
            let kids = tree.children.get(&Some(*id)).map_or(0, |v| v.len());
            ui.label(egui::RichText::new(crate::trf!("{a} Fotos ({b} direkt)", a = crate::library_ui::thousands(count(*id)), b = crate::library_ui::thousands(k.count))).small().color(lr::TEXT));
            if kids > 0 {
                ui.label(egui::RichText::new(crate::trf!("{n} Unterbegriffe", n = kids)).small().color(lr::TEXT));
            }
            if k.person {
                ui.label(egui::RichText::new(crate::tr("Person")).small().color(lr::TEXT_DIM));
            }
        }
        many => {
            heading(ui, &crate::trf!("{n} Stichwörter gewählt", n = many.len()));
        }
    }
    // the export mark of the selection (holds for everything below; Lightroom "include on export")
    if !sel.is_empty() && sel.iter().all(|id| tree.by_id.get(id).is_some_and(|k| !k.person)) {
        let marked = sel.iter().all(|id| a.marks.contains(id));
        let inherited = !marked && sel.iter().any(|id| tree.chain(*id).iter().any(|p| *p != *id && a.marks.contains(p)));
        let mut on = marked;
        let r = ui.add_enabled(!inherited, egui::Checkbox::new(&mut on, crate::tr("Nicht exportieren")));
        lr::hover_tip(ui, &r, crate::tr(if inherited { "Ein Oberbegriff ist schon als „nicht exportieren“ markiert" } else { "Gilt mit allem darunter: diese Stichwörter kommen nicht in exportierte Dateien" }));
        if r.changed() {
            *request = Some(Request::Export(!on));
        }
    }
    ui.add_space(8.0);
    // typed input (rename, new term) or the target list
    let mut submit = false;
    let kind = a.mode_kind();
    match &mut a.mode {
        Mode::Rename(t) | Mode::NewChild(t) | Mode::NewParent(t) => {
            let label = match kind {
                1 => crate::tr("Neuer Name"),
                2 => crate::tr("Neuer Unterbegriff (auch „A > B“)"),
                _ => crate::tr("Name des Oberbegriffs"),
            };
            ui.label(egui::RichText::new(label).small().color(lr::TEXT_DIM));
            let id = egui::Id::unique("kw-arrange-input");
            let r = lr::edit_field(ui, egui::TextEdit::singleline(t).id(id).desired_width(ui.available_width() - 4.0));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            ui.horizontal(|ui| {
                if ui.button(crate::tr("Übernehmen")).clicked() {
                    submit = true;
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    *request = Some(Request::Mode(Mode::None));
                }
            });
        }
        Mode::Pick { merge, filter } => {
            ui.label(egui::RichText::new(if *merge { crate::tr("Zusammenführen mit (filtern)") } else { crate::tr("Verschieben unter (filtern)") }).small().color(lr::TEXT_DIM));
            let r = lr::edit_field(ui, egui::TextEdit::singleline(filter).desired_width(ui.available_width() - 4.0));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            let f = library::search::fold_text(filter.trim());
            let below = tree.below(&sel);
            let mut v: Vec<(i64, String)> = tree
                .children
                .values()
                .flatten()
                .copied()
                .filter(|id| !below.contains(id) && (f.is_empty() || library::search::fold_text(&tree.by_id[id].name).contains(&f)))
                .map(|id| (id, tree.path(id)))
                .collect();
            v.sort_by_key(|(_, p)| p.to_lowercase());
            v.truncate(200);
            egui::ScrollArea::vertical().id_salt("kw-arrange-pick").max_height(220.0).show(ui, |ui| {
                if !*merge && lr::list_item(ui, crate::tr("Oberste Ebene"), false).clicked() {
                    *request = Some(Request::Pick(None));
                }
                for (id, p) in &v {
                    if lr::list_item(ui, p, false).clicked() {
                        *request = Some(Request::Pick(Some(*id)));
                    }
                }
            });
            if ui.button(crate::tr("Abbrechen")).clicked() {
                *request = Some(Request::Mode(Mode::None));
            }
        }
        Mode::None => {
            let one = sel.len() == 1;
            let any = !sel.is_empty();
            let name = one.then(|| tree.by_id.get(&sel[0]).map(|k| k.name.clone()).unwrap_or_default()).unwrap_or_default();
            let mut button = |ui: &mut egui::Ui, text: &str, on: bool, tip: &str, r: Request| {
                let b = ui.add_enabled(on, egui::Button::new(crate::tr(text)).min_size(egui::vec2(200.0, 22.0)));
                if !tip.is_empty() {
                    lr::hover_tip(ui, &b, crate::tr(tip));
                }
                if b.clicked() {
                    *request = Some(r);
                }
            };
            button(ui, "Umbenennen …", one, "", Request::Mode(Mode::Rename(name)));
            button(ui, if any { "Neuer Unterbegriff …" } else { "Neues Stichwort …" }, one || !any, "", Request::Mode(Mode::NewChild(String::new())));
            button(ui, "Neuer Oberbegriff …", any, "Legt einen Begriff an und hängt die Auswahl darunter", Request::Mode(Mode::NewParent(String::new())));
            button(ui, "Verschieben nach …", any, "Auch per Ziehen auf eine Zeile", Request::Mode(Mode::Pick { merge: false, filter: String::new() }));
            button(ui, "Zusammenführen mit …", any, "Auch per Ziehen mit ⌥", Request::Mode(Mode::Pick { merge: true, filter: String::new() }));
            let up = sel.iter().any(|id| tree.by_id.get(id).is_some_and(|k| k.parent.is_some()));
            button(ui, "Auf oberste Ebene", up, "", Request::ToTop);
            ui.add_space(6.0);
            button(ui, "Stichwort löschen …", one, if any && !one { "Nur einzeln" } else { "" }, Request::Delete);
            ui.add_space(6.0);
            button(ui, "Fotos zeigen", one, "Schließt den Dialog und zeigt die Fotos mit diesem Stichwort und allen darunter", Request::Show);
        }
    }
    if submit {
        *request = Some(Request::Submit);
    }
}

impl Arrange {
    /// After applying: the suggestions are closed (they would be outdated).
    pub(crate) fn close_suggestions(&mut self) {
        self.suggest = None;
    }

    fn mode_kind(&self) -> u8 {
        match self.mode {
            Mode::Rename(_) => 1,
            Mode::NewChild(_) => 2,
            _ => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(id: i64, name: &str, parent: Option<i64>, person: bool) -> Keyword {
        Keyword { id, name: name.into(), parent, count: id as usize, person }
    }

    #[test]
    fn tree_rows_fold_search_and_hide_people() {
        let k = vec![kw(1, "Tiere", None, false), kw(2, "Hunde", Some(1), false), kw(3, "Labrador", Some(2), false), kw(4, "Orte", None, false), kw(5, "Paris", Some(4), false), kw(6, "Personen", None, false), kw(7, "Paris", Some(6), true)];
        let t = Tree::new(&k, false, false, None);
        let ids = |rows: Vec<Row>| rows.into_iter().map(|r| (r.id, r.depth)).collect::<Vec<_>>();
        assert_eq!(ids(t.rows(&HashSet::new(), "")), vec![(4, 0), (6, 0), (1, 0)], "by name, all closed");
        assert_eq!(ids(t.rows(&HashSet::from([1]), "")), vec![(4, 0), (6, 0), (1, 0), (2, 1)]);
        // search: hits with their parents, opened; the person Paris hidden
        assert_eq!(ids(t.rows(&HashSet::new(), "lab")), vec![(1, 0), (2, 1), (3, 2)]);
        assert_eq!(ids(t.rows(&HashSet::new(), "paris")), vec![(4, 0), (5, 1)]);
        let t = Tree::new(&k, true, false, None);
        assert_eq!(ids(t.rows(&HashSet::new(), "paris")), vec![(4, 0), (5, 1), (6, 0), (7, 1)]);
        assert_eq!(t.path(7), "Personen › Paris");
    }

    #[test]
    fn drops_are_checked() {
        let k = vec![kw(1, "Tiere", None, false), kw(2, "Hunde", Some(1), false), kw(3, "Anna", None, true)];
        let t = Tree::new(&k, true, false, None);
        assert!(drop_problem(&t, &[1], Drop::Below(Some(2))).is_some(), "never below its own child");
        assert!(drop_problem(&t, &[2], Drop::Below(Some(1))).is_some(), "already there");
        assert!(drop_problem(&t, &[2], Drop::Below(None)).is_none());
        assert!(drop_problem(&t, &[3], Drop::Merge(1)).is_some(), "person and keyword");
        assert!(drop_problem(&t, &[1], Drop::Merge(2)).is_some(), "target below the merged one");
        assert!(drop_problem(&t, &[2], Drop::Merge(1)).is_none());
    }

    #[test]
    fn click_selects_like_the_finder() {
        let k = vec![kw(1, "A", None, false), kw(2, "B", None, false), kw(3, "C", None, false)];
        let t = Tree::new(&k, false, false, None);
        let rows = t.rows(&HashSet::new(), "");
        let mut a = Arrange::new(None, &k, 0);
        click(&mut a, &rows, 1, egui::Modifiers::NONE);
        click(&mut a, &rows, 3, egui::Modifiers::SHIFT);
        assert_eq!(a.selected, BTreeSet::from([1, 2, 3]));
        click(&mut a, &rows, 2, egui::Modifiers::COMMAND);
        assert_eq!(a.selected, BTreeSet::from([1, 3]));
    }
}

// ───────────────────────────────────────────── Suggestions for ordering

/// The list of suggestions (nothing checked at the start; nothing is applied without the question).
#[derive(Default)]
pub(crate) struct Suggest {
    list: Option<Result<Vec<library::keyword_suggest::Suggestion>, String>>,
    checked: BTreeSet<usize>,
    /// kinds shown (spelling, plural, ending, together)
    hidden: HashSet<library::keyword_suggest::Kind>,
}

fn kind_name(k: library::keyword_suggest::Kind) -> &'static str {
    use library::keyword_suggest::Kind as K;
    match k {
        K::Spelling => "Schreibweise",
        K::Plural => "Einzahl/Mehrzahl",
        K::Ending => "Mehrwort-Name",
        K::Together => "Fast immer zusammen",
    }
}

impl LibraryView {
    /// Compute the suggestions in the background (read only).
    fn load_suggestions(&mut self) {
        self.in_background(
            |db| library::keyword_suggest::suggest(&db.conn),
            |b, _, r| {
                if let Some(Arrange { suggest: Some(s), .. }) = &mut b.manage.arrange {
                    s.list = Some(r);
                }
            },
        );
    }

    fn suggest_window(&mut self, ctx: &egui::Context, a: &mut Arrange) {
        use library::keyword_suggest::{Change, Kind};
        let Some(s) = &mut a.suggest else { return };
        let mut close = false;
        let mut apply: Option<Vec<(Change, String)>> = None;
        let frame = egui::Frame::new().fill(Color32::from_gray(0x1e)).stroke(egui::Stroke::new(1.0, Color32::from_gray(0x3a))).corner_radius(0.0).inner_margin(egui::Margin::same(14));
        let modal = egui::Modal::new(egui::Id::unique("keyword-suggest")).frame(frame).backdrop_color(Color32::from_black_alpha(120)).show(ctx, |ui| {
            ui.set_width(720.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add(egui::Label::new(egui::RichText::new(crate::tr("Vorschläge zum Ordnen")).font(lr::semibold(15.0)).color(Color32::from_gray(0xee))));
            match &s.list {
                None => {
                    ui.allocate_ui(egui::vec2(720.0, 420.0), |ui| crate::dialog::pending(ui, crate::tr("Vorschläge werden berechnet …")));
                }
                Some(Err(e)) => {
                    ui.allocate_ui(egui::vec2(720.0, 420.0), |ui| crate::dialog::warning(ui, e));
                }
                Some(Ok(list)) => {
                    ui.horizontal(|ui| {
                        for k in [Kind::Spelling, Kind::Plural, Kind::Ending, Kind::Together] {
                            let n = list.iter().filter(|x| x.kind == k).count();
                            let mut on = !s.hidden.contains(&k);
                            if ui.checkbox(&mut on, crate::trf!("{a} ({n})", a = crate::tr(kind_name(k)), n = n)).changed() {
                                if on {
                                    s.hidden.remove(&k);
                                } else {
                                    s.hidden.insert(k);
                                }
                            }
                        }
                    });
                    let shown: Vec<usize> = (0..list.len()).filter(|i| !s.hidden.contains(&list[*i].kind)).collect();
                    egui::ScrollArea::vertical().id_salt("kw-suggest-list").auto_shrink([false; 2]).max_height(380.0).show_rows(ui, 22.0, shown.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for i in range {
                            let idx = shown[i];
                            let x = &list[idx];
                            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                let mut on = s.checked.contains(&idx);
                                let text = match x.change {
                                    Change::Merge { .. } => crate::trf!("„{a}“ mit „{b}“ zusammenführen", a = x.from, b = x.to),
                                    Change::Below { .. } => crate::trf!("„{a}“ unter „{b}“ legen", a = x.from, b = x.to),
                                };
                                if ui.checkbox(&mut on, "").changed() {
                                    if on {
                                        s.checked.insert(idx);
                                    } else {
                                        s.checked.remove(&idx);
                                    }
                                }
                                ui.add_sized(egui::vec2(120.0, 20.0), egui::Label::new(egui::RichText::new(crate::tr(kind_name(x.kind))).small().color(lr::TEXT_DIM)).truncate());
                                ui.add_sized(egui::vec2(400.0, 20.0), egui::Label::new(egui::RichText::new(text).color(lr::TEXT)).truncate());
                                let counts = match x.share {
                                    Some(sh) => crate::trf!("{a} / {b} Fotos · {p} %", a = x.photos_from, b = x.photos_to, p = (sh * 100.0).round()),
                                    None => crate::trf!("{a} / {b} Fotos", a = x.photos_from, b = x.photos_to),
                                };
                                ui.label(egui::RichText::new(counts).small().color(lr::TEXT_DIM));
                            });
                        }
                    });
                    if list.is_empty() {
                        crate::dialog::hint(ui, crate::tr("Keine Vorschläge."));
                    }
                }
            }
            ui.separator();
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = s.checked.len();
                if lr::main_button_min(ui, crate::tr("Gewählte anwenden …"), n > 0, 160.0).clicked() {
                    if let Some(Ok(list)) = &s.list {
                        apply = Some(s.checked.iter().filter_map(|i| list.get(*i)).map(|x| (x.change, match x.change {
                            Change::Merge { .. } => crate::trf!("„{a}“ → „{b}“", a = x.from, b = x.to),
                            Change::Below { .. } => crate::trf!("„{a}“ unter „{b}“", a = x.from, b = x.to),
                        })).collect());
                    }
                }
                if ui.button(crate::tr("Schließen")).clicked() {
                    close = true;
                }
                ui.label(egui::RichText::new(crate::trf!("{n} gewählt", n = n)).small().color(lr::TEXT_DIM));
                if n > 0 && ui.button(crate::tr("Keine")).clicked() {
                    s.checked.clear();
                }
            });
        });
        if modal.is_top_modal && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if close {
            a.suggest = None;
        }
        if let Some(v) = apply {
            let changes: Vec<Change> = v.iter().map(|x| x.0).collect();
            self.manage.ask = Some(Ask::ApplySuggestions { items: v, impact: None });
            self.in_background(
                move |db| library::keyword_suggest::impact(db, &changes),
                |b, _, r| {
                    if let Some(Ask::ApplySuggestions { impact, .. }) = &mut b.manage.ask {
                        *impact = Some(r);
                    }
                },
            );
        }
    }
}
