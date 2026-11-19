//! Series and stacks in the grid (and the filmstrip, which shows the same list).
//!
//! - A collapsed stack shows one tile: the cover (or, under a filter, the matching member with the best rating) with
//!   two thin edges behind the photo and a number at its bottom right ("7", filtered "3/7"); a small symbol says the
//!   kind (series, exposure bracket, own stack), a dot "not reviewed yet". Same tile size as every photo.
//! - An expanded stack: the cover first, then the other members in capture order (in the grid's direction: newest
//!   first when the grid shows the newest first), numbered 1/7 … 7/7 in that order, on a slightly lighter ground with a
//!   thin bracket line below, across line breaks.
//! - Commands: S toggles the stack of the active photo, ⇧S all stacks, ⌘G bundles the selection, ⇧⌘G dissolves,
//!   ⌥ + rating key rates the whole series; context menu "Als Titelfoto", "Aus Serie nehmen", "Serie teilen ab hier",
//!   "Serie sichten …". Changes go through `library::series` in the background and are undone with ⌘Z.
//! - Exporting, deleting or moving a collapsed stack asks: only the top photo, or the whole series?
//!
//! Submodule of `library_ui` (accesses its fields).

use super::*;
use library::series::{self as ser, Kind as StackKind, Stack};

/// Place of a shown photo in its stack.
#[derive(Clone, Copy, Debug)]
pub(super) struct Mark {
    /// index into `SeriesView::stacks`
    pub stack: usize,
    /// 1-based place among the shown pictures of the stack (cover = 1), see [`shown_order`]
    pub pos: usize,
    /// pictures of the stack ([`LibraryView::stack_pictures`])
    pub total: usize,
    /// pictures of the stack that are shown (filter, source); a virtual copy shares the place of its original
    pub shown: usize,
    pub collapsed: bool,
    /// expanded: first / last tile of the stack in the grid (ends of the bracket line)
    pub first: bool,
    pub last: bool,
}

/// A member file of a stack as the grid orders it.
#[derive(Clone, Copy, Debug)]
struct Member {
    file: i64,
    /// index into `SeriesView::stacks`
    stack: usize,
    /// place in the stack order (`stack_image.position`)
    order: usize,
    cover: bool,
}

/// Order of the shown pictures of one stack (files, each once): the cover first, then the stack order, turned round
/// when the grid shows the newest first. The tile numbers are the places in this list, so they have no gaps
/// when members are hidden.
fn shown_order(mut shown: Vec<Member>, backwards: bool) -> Vec<i64> {
    shown.sort_by_key(|m| (!m.cover, if backwards { usize::MAX - m.order } else { m.order }, m.file));
    shown.dedup_by_key(|m| m.file);
    shown.into_iter().map(|m| m.file).collect()
}

/// What an export, delete or move should do with a collapsed stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Export,
    Delete,
    Move,
}

pub(super) struct ScopeAsk {
    kind: ScopeKind,
    /// members hidden behind the collapsed tiles of the target
    hidden: Vec<i64>,
    /// stacks to open when the whole series is meant
    stacks: Vec<i64>,
}

/// Commands on stacks (keys, context menu, number on the tile).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StackCmd {
    Toggle,
    Cover,
    TakeOut,
    Split,
    Review,
    Group,
    Dissolve,
}

#[derive(Default)]
pub(super) struct SeriesView {
    pub(super) stacks: Vec<Stack>,
    /// image → index into `stacks`
    pub(super) by_image: HashMap<i64, usize>,
    /// shown image → its place (filled by the filter)
    pub(super) marks: HashMap<i64, Mark>,
    loading: Option<std::sync::mpsc::Receiver<Vec<Stack>>>,
    seen: Option<u64>,
    pub(super) ask: Option<ScopeAsk>,
    /// the question was answered: the next export/delete goes through without asking
    answered: bool,
    /// series review (centre view)
    pub(super) review: Option<series_review::Review>,
}

impl SeriesView {
    fn set(&mut self, stacks: Vec<Stack>) {
        self.by_image.clear();
        for (k, s) in stacks.iter().enumerate() {
            for &i in &s.images {
                self.by_image.insert(i, k);
            }
        }
        self.stacks = stacks;
    }

    /// Stack (index) of an image.
    pub(super) fn of(&self, image: i64) -> Option<usize> {
        self.by_image.get(&image).copied()
    }

    /// Number of series not reviewed yet.
    pub(super) fn open_count(&self) -> usize {
        self.stacks.iter().filter(|s| s.is_series() && s.reviewed.is_none()).count()
    }
}

/// Text for the length of a series ("12 s", "2 min 5 s").
fn span_text(s: i64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s % 60 == 0 {
        format!("{} min", s / 60)
    } else {
        format!("{} min {} s", s / 60, s % 60)
    }
}

/// Explanation of a stack: why the photos belong together.
/// `n`: pictures of the stack ([`LibraryView::stack_pictures`]).
pub(super) fn reason_text(s: &Stack, n: usize) -> String {
    let r = &s.reason;
    match s.kind {
        StackKind::Bracket => crate::trf!(
            "Belichtungsreihe: {n} Fotos, {k} Stufen über {ev} EV",
            n = n,
            k = r.ev_steps.unwrap_or(0),
            ev = format!("{:.1}", r.ev_range.unwrap_or(0.0))
        ),
        StackKind::Series => {
            let span = span_text(r.span_s);
            match r.sim_min {
                Some(sim) if r.model != "time" => crate::trf!("Serie: {n} Fotos in {t}, gleiches Motiv (Ähnlichkeit {s})", n = n, t = span, s = format!("{sim:.2}")),
                _ => crate::trf!("Serie: {n} Fotos in {t} (nur nach Zeit, Bildinhalt noch unbekannt)", n = n, t = span),
            }
        }
        StackKind::Manual if s.origin == "lightroom" => crate::trf!("Stapel aus Lightroom: {n} Fotos", n = n),
        StackKind::Manual => crate::trf!("Stapel: {n} Fotos", n = n),
    }
}

/// Is the stack collapsed in the grid? The source "Serien" shows every stack as one tile.
fn collapsed(s: &Stack, forced: bool) -> bool {
    forced || s.collapsed
}

impl LibraryView {
    /// A camera JPEG folded into its RAW (RAW+JPEG stacks on): no picture of its own, it follows the RAW.
    pub(super) fn folded_jpeg(&self, image: i64) -> bool {
        self.stack && self.index.get(&image).is_some_and(|&i| self.rows[i].jpeg_for.is_some())
    }

    /// Pictures of a stack as the grid counts them: members in the library; a camera JPEG folded into its RAW is
    /// part of that picture (the grouping puts both files of a RAW+JPEG shot into the stack).
    pub(super) fn stack_pictures(&self, s: &Stack) -> usize {
        s.images.iter().filter(|&&i| self.index.contains_key(&i) && !self.folded_jpeg(i)).count()
    }

    /// Every frame: read the stacks again when the series job or a command changed them (background, read-only).
    pub(super) fn series_poll(&mut self) {
        if let Some(rx) = &self.series.loading {
            match rx.try_recv() {
                Ok(v) => {
                    self.series.loading = None;
                    let first = self.series.stacks.is_empty() && !v.is_empty();
                    self.series.set(v);
                    self.series_review_refresh();
                    self.refilter = true;
                    if first {
                        self.series_test();
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.series.loading = None,
            }
        }
        let g = crate::series_job::generation();
        if self.series.loading.is_some() || self.series.seen == Some(g) {
            return;
        }
        let Some(path) = self.db.as_ref().map(|d| d.path.clone()) else { return };
        self.series.seen = Some(g);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.quick_ctx.clone();
        let started = std::thread::Builder::new()
            .name("serien-laden".into())
            .spawn(move || {
                let v = Db::read_only(&path).map_err(|e| e.to_string()).and_then(|db| ser::load(&db.conn));
                match v {
                    Ok(v) => {
                        let _ = tx.send(v);
                    }
                    Err(e) => crate::log(&format!("series: loading failed: {e}")),
                }
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            })
            .is_ok();
        if started {
            self.series.loading = Some(rx);
        }
    }

    /// The filtered list with stacks applied: a collapsed stack keeps one tile, an expanded one shows its members
    /// together (cover first, then capture order in the grid's direction: newest first in a grid sorted newest first). Fills the marks for drawing. "Ähnliche Fotos" and "Duplikate"
    /// show every photo on its own.
    pub(super) fn series_collapse(&mut self, v: Vec<usize>) -> Vec<usize> {
        self.series.marks.clear();
        let forced = self.source == Source::Series;
        if self.series.stacks.is_empty() || matches!(self.source, Source::Similar(_) | Source::Duplicates) {
            return if forced { Vec::new() } else { v };
        }
        let rows = &self.rows;
        let st = &self.series;
        // the members follow the cover in the direction of the grid: newest first when the grid shows the newest first
        let backwards = matches!(self.sort_order, SortOrder::CaptureTime) && !matches!(self.source, Source::Collection(_));
        let backwards = backwards != self.sort_reversed;
        // each member's file: its stack, its place in the stack order and whether it is the cover (virtual copies
        // follow their original through the file)
        let mut member: HashMap<i64, Member> = HashMap::new();
        for (k, s) in st.stacks.iter().enumerate() {
            let top = s.top();
            for (j, img) in s.images.iter().enumerate() {
                if let Some(&ri) = self.index.get(img) {
                    member.insert(rows[ri].file, Member { file: rows[ri].file, stack: k, order: j, cover: *img == top });
                }
            }
        }
        let stack_of = |z: &Row| -> Option<Member> {
            if let Some(&k) = st.by_image.get(&z.image) {
                return member.get(&z.file).copied().filter(|m| m.stack == k);
            }
            z.copy.as_ref().and_then(|_| member.get(&z.file).copied())
        };
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for &i in &v {
            if let Some(m) = stack_of(&rows[i]) {
                members.entry(m.stack).or_default().push(i);
            }
        }
        // place of each shown picture (file → 0-based), counted over the shown pictures only: the numbers on the tiles
        // run 1, 2, 3 … without gaps, also when members are hidden (filter, source, a camera JPEG folded into its RAW)
        let mut rank: HashMap<i64, usize> = HashMap::new();
        for vis in members.values() {
            let shown: Vec<Member> = vis.iter().filter_map(|&r| stack_of(&rows[r])).collect();
            for (p, file) in shown_order(shown, backwards).into_iter().enumerate() {
                rank.insert(file, p);
            }
        }
        let place = |z: &Row| rank.get(&z.file).copied().unwrap_or(usize::MAX);
        let mut out = Vec::with_capacity(v.len());
        let mut marks = HashMap::new();
        let mut done: HashSet<usize> = HashSet::new();
        for &i in &v {
            let Some(k) = stack_of(&rows[i]).map(|m| m.stack) else {
                if !forced {
                    out.push(i);
                }
                continue;
            };
            if !done.insert(k) {
                continue;
            }
            let s = &st.stacks[k];
            let vis = &members[&k];
            let total = self.stack_pictures(s);
            // pictures shown (a virtual copy shares the place of its original)
            let shown = vis.iter().map(|&r| rows[r].file).collect::<HashSet<_>>().len();
            if collapsed(s, forced) {
                let top = s.top();
                // the cover if it matches the filter, else the matching original rated best
                let rep = vis
                    .iter()
                    .copied()
                    .find(|&r| rows[r].image == top)
                    .or_else(|| vis.iter().copied().filter(|&r| rows[r].copy.is_none()).max_by_key(|&r| ((rows[r].flag == 1) as u8, rows[r].stars, std::cmp::Reverse(place(&rows[r])))))
                    .unwrap_or(vis[0]);
                out.push(rep);
                marks.insert(rows[rep].image, Mark { stack: k, pos: place(&rows[rep]).saturating_add(1), total, shown, collapsed: true, first: true, last: true });
            } else {
                let mut ordered = vis.clone();
                ordered.sort_by_key(|&r| (place(&rows[r]), rows[r].copy.is_some(), rows[r].image));
                let n = ordered.len();
                for (j, &r) in ordered.iter().enumerate() {
                    out.push(r);
                    marks.insert(rows[r].image, Mark { stack: k, pos: place(&rows[r]).saturating_add(1), total, shown, collapsed: false, first: j == 0, last: j + 1 == n });
                }
            }
        }
        if forced {
            // "Serien": not reviewed first, newest first
            out.sort_by_key(|&r| {
                let s = marks.get(&rows[r].image).map(|m| &st.stacks[m.stack]);
                (s.is_some_and(|s| s.reviewed.is_some()), std::cmp::Reverse(s.and_then(|s| s.day.clone())), rows[r].image)
            });
        }
        self.series.marks = marks;
        out
    }

    /// Lighter ground behind the tiles of an expanded stack (before the cell is drawn).
    pub(super) fn paint_series_ground(&self, p: &egui::Painter, rect: egui::Rect, image: i64) {
        if self.series.marks.get(&image).is_some_and(|m| !m.collapsed) {
            p.rect_filled(rect, 0.0, Color32::from_gray(0x26));
        }
    }

    /// Stack marks on a drawn cell: edges and number (collapsed), place and bracket line (expanded). Returns a
    /// command when the number was clicked.
    pub(super) fn paint_series_mark(&self, ui: &egui::Ui, rect: egui::Rect, z: &Row, tex: Option<&TextureHandle>) -> Option<StackCmd> {
        let m = *self.series.marks.get(&z.image)?;
        let s = &self.series.stacks[m.stack];
        let look = mark_look(&m, open_stack(&self.series.marks, self.active));
        let p = ui.painter();
        let inner = rect.shrink(3.0);
        let area = egui::Rect::from_min_max(inner.min + egui::vec2(8.0, 25.0), inner.max - egui::vec2(8.0, 22.0));
        let r = match tex {
            Some(t) => {
                let sz = crate::library_turn::turned_size(t.size_vec2(), self.turned.quarters(z.image));
                let k = (area.width() / sz.x).min(area.height() / sz.y);
                egui::Rect::from_center_size(area.center(), sz * k)
            }
            None => egui::Rect::from_center_size(area.center(), egui::vec2(area.width() * 0.8, area.width() * 0.55).min(area.size())),
        };
        if m.collapsed {
            // two thin edges behind the photo (a pile of prints)
            for (d, g) in [(3.0, 0x70), (6.0, 0x4c)] {
                let c = Color32::from_gray(g);
                let pts = vec![egui::pos2(r.left() + d, r.top() - d), egui::pos2(r.right() + d, r.top() - d), egui::pos2(r.right() + d, r.bottom() - d)];
                p.add(egui::Shape::line(pts, egui::Stroke::new(1.0, c)));
            }
        } else {
            // bracket line below the tiles of the stack (brighter, with taller end marks for the open stack of the
            // active photo)
            let y = inner.bottom() + 1.5;
            let (x0, x1) = (if m.first { inner.left() + 4.0 } else { rect.left() }, if m.last { inner.right() - 4.0 } else { rect.right() });
            let w = egui::Stroke::new(look.width, look.bracket);
            p.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], w);
            if m.first {
                p.line_segment([egui::pos2(x0, y), egui::pos2(x0, y - look.tick)], w);
            }
            if m.last {
                p.line_segment([egui::pos2(x1, y), egui::pos2(x1, y - look.tick)], w);
            }
        }
        // number at the bottom right of the photo: "7" / "3/7" (shown of all) collapsed, "2/7" (place among the
        // shown) expanded
        let text = tile_number(&m);
        let g = p.layout_no_wrap(text, egui::FontId::proportional(10.5), look.text);
        let symbol_w = if m.collapsed || m.first { 12.0 } else { 0.0 };
        let dot_w = if s.is_series() && s.reviewed.is_none() && (m.collapsed || m.first) { 8.0 } else { 0.0 };
        let size = egui::vec2(g.size().x + 10.0 + symbol_w + dot_w, 16.0);
        let field = egui::Rect::from_min_size(r.right_bottom() - size - egui::vec2(3.0, 3.0), size);
        let resp = ui.interact(field, egui::Id::unique(("stapel-zahl", z.image)), egui::Sense::click());
        let bg = if resp.hovered() { Color32::from_black_alpha(230) } else { look.ground };
        p.rect_filled(field, 0.0, bg);
        let mut x = field.left() + 5.0;
        let cy = field.center().y;
        if dot_w > 0.0 {
            p.circle_filled(egui::pos2(x + 2.0, cy), 2.0, Color32::from_gray(0xd8));
            x += dot_w;
        }
        if symbol_w > 0.0 {
            paint_kind(p, egui::pos2(x + 4.5, cy), s);
            x += symbol_w;
        }
        p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, Color32::WHITE);
        let toggle = match (s.is_series(), m.collapsed) {
            (true, true) => "Serie aufklappen",
            (true, false) => "Serie zuklappen",
            (false, true) => "Stapel aufklappen",
            (false, false) => "Stapel zuklappen",
        };
        let mut tip = lr::with_shortcut(toggle, "stack_toggle");
        tip.push('\n');
        tip.push_str(&reason_text(s, m.total));
        if !m.collapsed && m.shown < m.total {
            tip.push('\n');
            tip.push_str(&crate::trf!("{a} von {b} Fotos sichtbar, die übrigen blendet die Auswahl links oder ein Filter aus", a = m.shown, b = m.total));
        }
        if s.is_series() && s.reviewed.is_none() {
            tip.push('\n');
            tip.push_str(crate::tr("Noch nicht gesichtet"));
        }
        lr::hover_tip(ui, &resp, &tip);
        resp.clicked().then_some(StackCmd::Toggle)
    }

    /// Context menu entries for a photo in a stack (and "bundle" for a selection). Returns the chosen command.
    pub(super) fn series_menu(&self, ui: &mut egui::Ui, image: i64) -> Option<StackCmd> {
        use crate::shortcuts::display;
        let mut chosen = None;
        let stack = self.series.of(image).map(|k| &self.series.stacks[k]);
        let several = self.selection.len() > 1 && self.selection.contains(&image);
        // a selection of 2–60 photos can be reviewed like a series
        let reviewable = several && stack.is_none() && (2..=60).contains(&self.selection.len());
        if stack.is_none() && !several {
            return None;
        }
        ui.separator();
        let mut item = |ui: &mut egui::Ui, c: StackCmd, text: &str, key: String, enabled: bool| {
            if lr::menu_item(ui, text, &key, enabled) {
                chosen = Some(c);
            }
        };
        if let Some(s) = stack {
            let series = s.is_series();
            let toggle = if self.series.marks.get(&image).is_some_and(|m| m.collapsed) {
                if series { crate::tr("Serie aufklappen") } else { crate::tr("Stapel aufklappen") }
            } else if series {
                crate::tr("Serie zuklappen")
            } else {
                crate::tr("Stapel zuklappen")
            };
            item(ui, StackCmd::Toggle, toggle, display("stack_toggle"), true);
            item(ui, StackCmd::Review, if series { crate::tr("Serie sichten …") } else { crate::tr("Stapel sichten …") }, display("stack_review"), true);
            item(ui, StackCmd::Cover, crate::tr("Als Titelfoto"), String::new(), s.top() != image);
            item(ui, StackCmd::TakeOut, if series { crate::tr("Aus Serie nehmen") } else { crate::tr("Aus Stapel nehmen") }, String::new(), true);
            item(ui, StackCmd::Split, if series { crate::tr("Serie teilen ab hier") } else { crate::tr("Stapel teilen ab hier") }, String::new(), s.images.first() != Some(&image));
            item(ui, StackCmd::Dissolve, if series { crate::tr("Serie auflösen") } else { crate::tr("Stapel auflösen") }, display("stack_dissolve"), true);
        }
        if reviewable {
            item(ui, StackCmd::Review, crate::tr("Serie sichten …"), display("stack_review"), true);
        }
        if several {
            // a viewed folder: greyed with the reason (stacks live in the library)
            let refused = self.needs_library(&self.target());
            let why = if refused { crate::tr(super::view_folder_guard::NEEDS_LIBRARY) } else { "" };
            if lr::menu_item_why(ui, crate::tr("Als Stapel bündeln"), &display("stack_group"), !refused, why) {
                chosen = Some(StackCmd::Group);
            }
        }
        chosen
    }

    /// Keys of the grid for stacks (S, ⇧S, ⌘G, ⇧⌘G, N, ⌥ + rating key).
    pub(super) fn series_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(active) = self.active else { return };
        if crate::shortcuts::pressed(ctx, "stack_toggle") {
            self.series_command(StackCmd::Toggle, active, actions);
        }
        if crate::shortcuts::pressed(ctx, "stack_toggle_all") {
            self.series_toggle_all();
        }
        if crate::shortcuts::pressed(ctx, "stack_group") {
            self.series_command(StackCmd::Group, active, actions);
        }
        if crate::shortcuts::pressed(ctx, "stack_dissolve") {
            self.series_command(StackCmd::Dissolve, active, actions);
        }
        if crate::shortcuts::pressed(ctx, "stack_review") {
            self.series_command(StackCmd::Review, active, actions);
        }
        // ⌥ + P/X/U/0–9: the whole series of the target photos
        let change = if crate::shortcuts::pressed(ctx, "stack_pick") {
            Some((None, Some(1), None))
        } else if crate::shortcuts::pressed(ctx, "stack_reject") {
            Some((None, Some(-1), None))
        } else if crate::shortcuts::pressed(ctx, "stack_unflag") {
            Some((None, Some(0), None))
        } else if let Some(n) = crate::shortcuts::which(ctx, "stack_stars") {
            Some((Some(n as u8), None, None))
        } else {
            crate::shortcuts::which(ctx, "stack_label").map(|n| (None, None, Some(n as u8 + 1)))
        };
        if let Some((s, f, l)) = change {
            let mut target: Vec<i64> = Vec::new();
            for b in self.target() {
                match self.series.of(b) {
                    Some(k) => target.extend(self.series.stacks[k].images.iter().copied()),
                    None => target.push(b),
                }
            }
            target.sort_unstable();
            target.dedup();
            let new = self.rate_images(&target, s, f, l);
            if !new.is_empty() {
                actions.push(Action::Status(crate::trf!("Ganze Serie bewertet ({n} Fotos)", n = new.len())));
                actions.push(Action::Rated(new));
            }
        }
    }

    /// ⇧S: collapse all stacks when any shown one is open, else open all.
    pub(super) fn series_toggle_all(&mut self) {
        let any_open = self.series.marks.values().any(|m| !m.collapsed);
        for s in self.series.stacks.iter_mut() {
            s.collapsed = any_open;
        }
        self.refilter = true;
        self.in_background(move |db| ser::set_all_collapsed(db, any_open), |_, _, _| {});
    }

    /// Run a stack command for a photo (the selection for "bundle").
    pub(super) fn series_command(&mut self, cmd: StackCmd, image: i64, actions: &mut Vec<Action>) {
        let k = self.series.of(image);
        match cmd {
            StackCmd::Toggle => {
                let Some(k) = k else { return };
                let now_collapsed = self.series.marks.values().any(|m| m.stack == k && m.collapsed) || !self.series.marks.values().any(|m| m.stack == k);
                let s = &mut self.series.stacks[k];
                s.collapsed = !now_collapsed;
                let (id, c, top) = (s.id, s.collapsed, s.top());
                // collapsing: the tile left is the cover – it becomes the active photo
                if c {
                    self.active = Some(top);
                }
                self.refilter = true;
                self.in_background(move |db| ser::set_collapsed(db, id, c), |_, _, _| {});
            }
            StackCmd::Review => self.series_review_open(k.map(|k| self.series.stacks[k].id), actions),
            StackCmd::Group => {
                let mut sel: Vec<i64> = self.target();
                // stacks live in the library: photos of a viewed folder are refused with the reason
                if let Some(t) = self.refuse_viewed(&sel) {
                    actions.push(Action::Status(t));
                    return;
                }
                // the photos of collapsed stacks in the selection come along
                let extra: Vec<i64> = sel.iter().filter(|b| self.series.marks.get(b).is_some_and(|m| m.collapsed)).filter_map(|b| self.series.of(*b)).flat_map(|k| self.series.stacks[k].images.clone()).collect();
                sel.extend(extra);
                // originals only (virtual copies follow their original)
                sel.retain(|b| self.index.get(b).is_some_and(|&i| self.rows[i].copy.is_none()));
                sel.sort_unstable();
                sel.dedup();
                if sel.len() < 2 {
                    actions.push(Action::Status(crate::tr("Zum Bündeln mindestens zwei Fotos auswählen").into()));
                    return;
                }
                let cover = if sel.contains(&image) { image } else { sel[0] };
                // capture order behind the cover
                let time = |b: &i64| self.index.get(b).and_then(|&i| self.rows[i].capture.clone()).unwrap_or_default();
                sel.sort_by_key(time);
                let n = sel.len();
                self.series_change(move |db| ser::create(db, &sel, cover).map(|(c, _)| c), crate::trf!("Stapel aus {n} Fotos", n = n));
            }
            StackCmd::Dissolve => {
                let Some(k) = k else { return };
                let id = self.series.stacks[k].id;
                let label = if self.series.stacks[k].is_series() { crate::tr("Serie auflösen") } else { crate::tr("Stapel auflösen") }.to_string();
                self.series_change(move |db| ser::dissolve(db, id), label);
            }
            StackCmd::Cover => {
                let Some(k) = k else { return };
                let id = self.series.stacks[k].id;
                self.series.stacks[k].cover = Some(image);
                self.refilter = true;
                self.series_change(move |db| ser::set_cover(db, id, image), crate::tr("Als Titelfoto").to_string());
            }
            StackCmd::TakeOut => {
                self.series_change(move |db| ser::take_out(db, image)?.ok_or_else(|| crate::tr("Das Foto liegt in keinem Stapel").to_string()), crate::tr("Aus Serie nehmen").to_string());
            }
            StackCmd::Split => {
                self.series_change(move |db| ser::split_at(db, image)?.ok_or_else(|| crate::tr("Hier lässt sich nicht teilen").to_string()), crate::tr("Serie teilen").to_string());
            }
        }
    }

    /// A change of stacks in the background: ⌘Z takes it back, the grid reads the stacks again.
    pub(super) fn series_change(&mut self, work: impl FnOnce(&Db) -> Result<ser::Change, String> + Send + 'static, label: String) {
        self.in_background(work, move |b, actions, r| match r {
            Ok(change) => {
                b.manage.push(library_manage::Undo::Stack { label: label.clone(), change });
                crate::series_job::changed();
                actions.push(Action::Status(crate::trf!("{a} – ⌘Z macht es rückgängig", a = label)));
            }
            Err(e) => actions.push(Action::Status(crate::trf!("Stapel nicht geändert: {e}", e = e))),
        });
    }

    /// Before an export, delete or move: does the target hold collapsed stacks with hidden photos? Then ask once
    /// (`true`: the question is open, the caller stops; the answer repeats the action).
    pub fn series_ask_scope(&mut self, kind: ScopeKind) -> bool {
        if std::mem::take(&mut self.series.answered) {
            return false;
        }
        let mut hidden = Vec::new();
        let mut stacks = Vec::new();
        for b in self.target() {
            let Some(m) = self.series.marks.get(&b).filter(|m| m.collapsed) else { continue };
            let s = &self.series.stacks[m.stack];
            stacks.push(s.id);
            // a folded camera JPEG comes along with its RAW (as everywhere with RAW+JPEG stacks)
            hidden.extend(s.images.iter().copied().filter(|&i| i != b && !self.folded_jpeg(i)));
        }
        if hidden.is_empty() {
            return false;
        }
        self.series.ask = Some(ScopeAsk { kind, hidden, stacks });
        true
    }

    /// The question "only the top photo / the whole series".
    pub(super) fn series_dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(ask) = &self.series.ask else { return };
        let n_top = self.target().len();
        let n_all = n_top + ask.hidden.len();
        let what = match ask.kind {
            ScopeKind::Export => crate::tr("Exportieren"),
            ScopeKind::Delete => crate::tr("In den Papierkorb"),
            ScopeKind::Move => crate::tr("Verschieben"),
        };
        let all = crate::trf!("Ganze Serie ({n} Fotos)", n = n_all);
        let mut only_top = false;
        let a = crate::dialog::show(
            ctx,
            "stapel-umfang",
            &crate::trf!("{a}: nur oberstes Foto oder ganze Serie?", a = what),
            440.0,
            |ui| {
                crate::dialog::text(ui, &crate::trf!("Die Auswahl enthält zugeklappte Serien oder Stapel mit {n} weiteren Fotos.", n = ask.hidden.len()));
                ui.add_space(4.0);
                let label = if n_top == 1 { crate::tr("Nur oberstes Foto").to_string() } else { crate::trf!("Nur oberste Fotos ({n})", n = n_top) };
                if ui.add(egui::Button::new(label).corner_radius(0.0)).clicked() {
                    only_top = true;
                }
            },
            crate::dialog::Buttons::confirm(&all, true),
        );
        let kind = ask.kind;
        let action = |actions: &mut Vec<Action>, b: &mut LibraryView| match kind {
            ScopeKind::Export => actions.push(Action::Export),
            ScopeKind::Delete => actions.push(Action::Delete),
            ScopeKind::Move => b.menu_move(ctx),
        };
        if only_top {
            self.series.ask = None;
            self.series.answered = true;
            action(actions, self);
        } else if a == crate::dialog::Answer::Primary {
            let ask = self.series.ask.take().unwrap();
            // open the stacks and select all their photos: the action then sees every one of them
            for s in self.series.stacks.iter_mut().filter(|s| ask.stacks.contains(&s.id)) {
                s.collapsed = false;
            }
            for b in self.target() {
                self.selection.insert(b);
            }
            self.selection.extend(ask.hidden.iter().copied());
            self.nothing_selected = false;
            self.refilter = true;
            self.apply_filter();
            let ids = ask.stacks.clone();
            self.in_background(move |db| ids.iter().try_for_each(|&id| ser::set_collapsed(db, id, false)), |_, _, _| {});
            self.series.answered = true;
            action(actions, self);
        } else if a == crate::dialog::Answer::Cancel {
            self.series.ask = None;
        }
    }

    /// Left panel: row "Serien" with the number of series not reviewed yet. Always there: without series yet a small
    /// line below says what the detection is doing (searching, n of m, waiting for the search index, none found).
    pub(super) fn series_row(&mut self, ui: &mut egui::Ui) {
        let open = self.series.open_count();
        let text = crate::tr("Serien");
        let empty = self.series.stacks.is_empty();
        let (click, _, resp) = Self::list_row_icon(ui, Some(lr::Icon::CopySimple), text, (open > 0).then(|| crate::trf!("{n} offen", n = thousands(open))), self.source_marked(&Source::Series), 0, None, empty);
        let (phase, done, total) = crate::series_job::progress();
        let st = crate::series_job::empty_state(crate::series_job::enabled(), phase, done, total, crate::series_job::passed(), crate::perf_mode::power() == crate::perf_mode::Power::LowPower);
        let searching = phase != crate::series_job::Phase::Idle;
        // below the row, in the indent of the names; always there (also empty), so nothing below moves when the
        // first series is found
        let line = if empty || searching { st.short.clone() } else { String::new() };
        let r = ui
            .horizontal(|ui| {
                ui.add_space(24.0);
                ui.add(egui::Label::new(egui::RichText::new(if line.is_empty() { " " } else { line.as_str() }).small().color(lr::TEXT_DIM)).truncate().sense(egui::Sense::hover()))
            })
            .inner;
        if empty {
            let tip = format!("{}\n{}", st.title, st.tip);
            lr::hover_tip(ui, &resp, &tip);
            lr::hover_tip(ui, &r, &tip);
        } else {
            lr::hover_tip(ui, &resp, crate::tr("Alle Serien und Stapel, je eine Kachel; ungesichtete zuerst"));
        }
        // the search runs: the line follows it
        if searching {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        if click {
            self.enter_special_source();
            self.source = Source::Series;
            self.source_ids = None;
            self.loupe = false;
            self.refilter = true;
        }
    }

    /// Once ever (first start with the series detection): when it begins searching and there are no series yet, a
    /// quiet line in the status bar says where they will appear. No dialog.
    pub(super) fn series_first_hint(&mut self, actions: &mut Vec<Action>) {
        const KEY: &str = "seriesSearchHintShown";
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if DONE.load(std::sync::atomic::Ordering::Relaxed) || !self.series.stacks.is_empty() || crate::series_job::progress().0 == crate::series_job::Phase::Idle {
            return;
        }
        DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        if library::settings::value(KEY).as_bool().unwrap_or(false) {
            return;
        }
        library::settings::set(KEY, serde_json::json!(true));
        actions.push(Action::Status(crate::tr("Serien werden gesucht … sie erscheinen links unter „Serien“").into()));
    }

    /// Middle of the grid for the source "Serien" while there are none: the state of the detection and the ways in.
    pub(super) fn series_empty(&mut self, ui: &mut egui::Ui) {
        let (phase, done, total) = crate::series_job::progress();
        let st = crate::series_job::empty_state(crate::series_job::enabled(), phase, done, total, crate::series_job::passed(), crate::perf_mode::power() == crate::perf_mode::Power::LowPower);
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(&st.title).size(16.0).color(lr::TEXT));
            ui.add_space(6.0);
            for t in st.tip.lines() {
                ui.label(egui::RichText::new(t).color(lr::TEXT_DIM));
            }
        });
        if phase != crate::series_job::Phase::Idle {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    /// Commands from the menu bar (Foto → Serie sichten, Als Stapel bündeln, Stapel auflösen, Serien auf-/zuklappen)
    /// for the active photo or the selection.
    pub fn stack_menu_command(&mut self, cmd: StackMenu) -> Vec<Action> {
        let mut actions = Vec::new();
        if cmd == StackMenu::ToggleAll {
            if self.series.stacks.is_empty() {
                actions.push(Action::Status(crate::tr("Noch keine Serien oder Stapel").into()));
            } else {
                self.series_toggle_all();
            }
            return actions;
        }
        let Some(active) = self.active.or_else(|| self.target().first().copied()) else {
            actions.push(Action::Status(crate::tr("Erst ein Foto auswählen").into()));
            return actions;
        };
        match cmd {
            StackMenu::Review => self.series_command(StackCmd::Review, active, &mut actions),
            StackMenu::Group => self.series_command(StackCmd::Group, active, &mut actions),
            StackMenu::Dissolve if self.series.of(active).is_none() => actions.push(Action::Status(crate::tr("Das Foto liegt in keiner Serie und keinem Stapel").into())),
            StackMenu::Dissolve => self.series_command(StackCmd::Dissolve, active, &mut actions),
            StackMenu::ToggleAll => {}
        }
        actions
    }
}

/// Stack commands of the menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackMenu {
    Review,
    Group,
    Dissolve,
    ToggleAll,
}

impl LibraryView {
    /// Test aid `GRAINEO_SERIES_TEST` (only with `GRAINEO_DATA_DIR`), once the stacks are there:
    /// `collapse` (all stacks collapsed), `source` (source "Serien"), `review[:light|:duel]` (review of the first series).
    fn series_test(&mut self) {
        if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let Ok(spec) = crate::env::var("GRAINEO_SERIES_TEST") else { return };
        let mut actions = Vec::new();
        for part in spec.split(';') {
            match part.trim() {
                "collapse" => {
                    for s in self.series.stacks.iter_mut() {
                        s.collapsed = true;
                    }
                }
                "source" => {
                    self.enter_special_source();
                    self.source = Source::Series;
                }
                p if p.starts_with("review") => {
                    let id = self.series.stacks.iter().filter(|s| s.kind == StackKind::Series).max_by_key(|s| s.images.len()).map(|s| s.id);
                    self.series_review_open(id, &mut actions);
                    self.series_review_test_mode(p.strip_prefix("review:").unwrap_or(""));
                }
                _ => {}
            }
        }
        self.refilter = true;
    }
}

/// The expanded stack the active photo belongs to (index into `SeriesView::stacks`): its members are marked in the
/// grid (brighter bracket line and number), the numbers of the other stacks are dimmed.
fn open_stack(marks: &HashMap<i64, Mark>, active: Option<i64>) -> Option<usize> {
    active.and_then(|a| marks.get(&a)).filter(|m| !m.collapsed).map(|m| m.stack)
}

/// How the stack marks of a tile are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MarkLook {
    /// bracket line below expanded stacks: colour, width, height of the end marks
    bracket: Color32,
    width: f32,
    tick: f32,
    /// number field: text and ground
    text: Color32,
    ground: Color32,
}

/// Look of a tile's stack marks: members of the open stack of the active photo bright (bracket and number, no gold,
/// no ground behind the tile), the numbers of other stacks dimmed while such a stack is open; else as before.
fn mark_look(m: &Mark, open: Option<usize>) -> MarkLook {
    let normal = MarkLook {
        bracket: Color32::from_gray(0x6a),
        width: 1.0,
        tick: 4.0,
        text: Color32::from_gray(if m.collapsed { 0xf0 } else { 0xc0 }),
        ground: Color32::from_black_alpha(170),
    };
    match open {
        Some(k) if k == m.stack => MarkLook { bracket: Color32::from_gray(0xd0), width: 1.5, tick: 7.0, text: Color32::from_gray(0xf8), ground: Color32::from_black_alpha(215), ..normal },
        Some(_) => MarkLook { text: Color32::from_gray(0x86), ground: Color32::from_black_alpha(110), ..normal },
        None => normal,
    }
}

/// Text of the stack number on a tile: collapsed "7" or "3/7" (shown of all pictures), expanded "2/3" (place among
/// the shown pictures of the stack, so the numbers run without gaps).
fn tile_number(m: &Mark) -> String {
    if m.collapsed {
        if m.shown < m.total { format!("{}/{}", m.shown, m.total) } else { m.total.to_string() }
    } else {
        format!("{}/{}", m.pos, m.shown)
    }
}

/// Small symbol of the kind: two rectangles (series), a step (exposure bracket), three sheets (own stack).
fn paint_kind(p: &egui::Painter, c: egui::Pos2, s: &Stack) {
    let w = egui::Stroke::new(1.0, Color32::from_gray(0xd0));
    match s.kind {
        StackKind::Series => {
            p.rect_stroke(egui::Rect::from_center_size(c + egui::vec2(-1.0, 1.0), egui::vec2(6.0, 5.0)), 0.0, w, egui::StrokeKind::Inside);
            p.rect_stroke(egui::Rect::from_center_size(c + egui::vec2(1.5, -1.5), egui::vec2(6.0, 5.0)), 0.0, w, egui::StrokeKind::Inside);
        }
        StackKind::Bracket => {
            let pts = vec![c + egui::vec2(-4.0, 3.0), c + egui::vec2(-1.5, 3.0), c + egui::vec2(-1.5, 0.0), c + egui::vec2(1.5, 0.0), c + egui::vec2(1.5, -3.0), c + egui::vec2(4.0, -3.0)];
            p.add(egui::Shape::line(pts, w));
        }
        StackKind::Manual => {
            for dy in [-3.0, 0.0, 3.0] {
                p.line_segment([c + egui::vec2(-4.0, dy), c + egui::vec2(4.0, dy)], w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series_job::{empty_state, Phase};

    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output.shapes.iter().filter_map(|s| if let egui::epaint::Shape::Text(t) = &s.shape { Some(t.galley.text().to_string()) } else { None }).collect()
    }

    #[test]
    fn series_row_is_there_without_series_and_says_what_happens() {
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        assert!(library.series.stacks.is_empty());
        let out = crate::ui_test::run(&ctx, egui::RawInput::default(), |ui| library.series_row(ui));
        let t = texts(&out);
        assert!(t.iter().any(|s| s == "Serien"), "{t:?}");
        // a second line with the state of the detection (not started in this test: about to begin)
        assert!(t.len() >= 2, "{t:?}");
    }

    /// The row keeps its height when the first series is found (review 1.3.0, logic N5: everything below moved up).
    #[test]
    fn series_row_keeps_its_height() {
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        let mut height = |library: &mut LibraryView| {
            let mut hgt = 0.0;
            crate::ui_test::run(&ctx, egui::RawInput::default(), |ui| {
                let top = ui.cursor().top();
                library.series_row(ui);
                hgt = ui.cursor().top() - top;
            });
            hgt
        };
        let without = height(&mut library);
        library.series.set(vec![Stack {
            id: 1,
            kind: StackKind::Series,
            origin: "auto".into(),
            cover: None,
            cover_by_user: false,
            collapsed: true,
            reviewed: None,
            reason: Default::default(),
            day: None,
            images: vec![1, 2],
        }]);
        let with = height(&mut library);
        assert!(without > 0.0);
        assert_eq!(with, without);
    }

    /// Library rows for the stack tests: RAW `image` with an optional camera JPEG `image + 100` (same name).
    fn rows_with_pairs(raws: &[(i64, &str, bool)]) -> Vec<Row> {
        let mut v = Vec::new();
        for &(image, name, jpeg) in raws {
            let mut raw = Row { image, file: image, folder: 1, name: format!("{name}.DNG"), format: "DNG".into(), capture: Some(format!("2026-11-01T10:00:{:02}", image)), ..Row::default() };
            if jpeg {
                raw.sidecar_jpeg = Some(image + 100);
                v.push(Row { image: image + 100, file: image + 100, folder: 1, name: format!("{name}.JPG"), format: "JPG".into(), capture: raw.capture.clone(), jpeg_for: Some(image), ..Row::default() });
            }
            v.push(raw);
        }
        v
    }

    fn stack(id: i64, images: Vec<i64>, collapsed: bool) -> Stack {
        Stack { id, kind: StackKind::Series, origin: "auto".into(), cover: Some(images[0]), cover_by_user: false, collapsed, reviewed: None, reason: Default::default(), day: None, images }
    }

    /// The grid's numbers after filtering: (image, tile text) in grid order.
    fn numbers(library: &mut LibraryView, shown: impl Fn(&Row) -> bool) -> Vec<(i64, String)> {
        let v: Vec<usize> = (0..library.rows.len()).filter(|&i| shown(&library.rows[i]) && !(library.stack && library.rows[i].jpeg_for.is_some())).collect();
        let out = library.series_collapse(v);
        out.iter().map(|&i| library.rows[i].image).filter_map(|b| library.series.marks.get(&b).map(|m| (b, tile_number(m)))).collect()
    }

    fn library_with(rows: Vec<Row>, stacks: Vec<Stack>) -> LibraryView {
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        library.stack = true;
        library.index = rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        library.rows = Arc::new(rows);
        library.series.set(stacks);
        library
    }

    /// Nutzerbefund 1.3.2: the grouping puts RAW and camera JPEG of a shot into the series; the JPEG folded into its
    /// RAW took a number of its own, so the grid showed "1/4, 2/4, 4/4" (and "1/3, 3/3"). The numbers now count the
    /// shown pictures and run without gaps.
    #[test]
    fn numbers_skip_no_place_for_a_folded_camera_jpeg() {
        // series A: 1 (+JPEG), 2; series B: 3, 4 (+JPEG), 5 – in capture order, as the grouping writes them
        let rows = rows_with_pairs(&[(1, "S5399", true), (2, "S5400", false), (3, "S5396", false), (4, "S5397", true), (5, "S5398", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 101, 2], false), stack(2, vec![3, 4, 104, 5], false)]);
        // grid newest first (default): cover first, then the newest
        let n = numbers(&mut library, |_| true);
        let text = |b: i64| n.iter().find(|x| x.0 == b).map(|x| x.1.clone()).unwrap();
        assert_eq!((text(1).as_str(), text(2).as_str()), ("1/2", "2/2"), "{n:?}");
        assert_eq!((text(3).as_str(), text(5).as_str(), text(4).as_str()), ("1/3", "2/3", "3/3"), "{n:?}");
        // the members stand together in the order of their numbers
        let b: Vec<&str> = n.iter().filter(|x| [3, 4, 5].contains(&x.0)).map(|x| x.1.as_str()).collect();
        assert_eq!(b, ["1/3", "2/3", "3/3"]);
        // sorted by name: oldest first after the cover
        library.sort_order = SortOrder::Name;
        let n = numbers(&mut library, |_| true);
        let b: Vec<(i64, &str)> = n.iter().filter(|x| [3, 4, 5].contains(&x.0)).map(|x| (x.0, x.1.as_str())).collect();
        assert_eq!(b, [(3, "1/3"), (4, "2/3"), (5, "3/3")]);
        // the explanation counts pictures, not files
        assert_eq!(library.stack_pictures(&library.series.stacks[1]), 3);
        // without RAW+JPEG stacks the camera JPEG is a tile of its own and gets its own number
        library.stack = false;
        let n = numbers(&mut library, |_| true);
        let b: Vec<&str> = n.iter().filter(|x| [3, 4, 104, 5].contains(&x.0)).map(|x| x.1.as_str()).collect();
        assert_eq!(b, ["1/4", "2/4", "3/4", "4/4"]);
    }

    /// A member hidden by a filter: the shown ones are numbered 1…n of the shown; collapsed: "shown/all".
    #[test]
    fn numbers_of_a_filtered_stack_have_no_gap() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 2, 3, 4], false)]);
        library.sort_order = SortOrder::Name;
        let n = numbers(&mut library, |z| z.image != 2);
        assert_eq!(n, [(1, "1/3".to_string()), (3, "2/3".to_string()), (4, "3/3".to_string())]);
        library.series.stacks[0].collapsed = true;
        assert_eq!(numbers(&mut library, |z| z.image != 2), [(1, "3/4".to_string())]);
    }

    #[test]
    fn shown_order_cover_first_then_stack_order() {
        let m = |file, order, cover| Member { file, stack: 0, order, cover };
        let shown = vec![m(30, 3, false), m(10, 1, false), m(20, 2, true), m(30, 3, false)];
        assert_eq!(shown_order(shown.clone(), false), [20, 10, 30]);
        assert_eq!(shown_order(shown, true), [20, 30, 10]);
    }

    /// Members of the open stack of the active photo are marked (bright bracket, taller end marks, bright number),
    /// the numbers of other stacks are dimmed; without an open stack of the active photo everything stays as before.
    #[test]
    fn open_stack_of_the_active_photo_is_marked() {
        let mark = |stack, collapsed| Mark { stack, pos: 1, total: 3, shown: 3, collapsed, first: true, last: true };
        let marks: HashMap<i64, Mark> = HashMap::from([(1, mark(0, false)), (2, mark(0, false)), (3, mark(1, false)), (4, mark(2, true))]);
        assert_eq!(open_stack(&marks, Some(2)), Some(0));
        assert_eq!(open_stack(&marks, Some(3)), Some(1));
        // a collapsed stack or a photo outside any stack marks nothing
        assert_eq!(open_stack(&marks, Some(4)), None);
        assert_eq!(open_stack(&marks, Some(99)), None);
        assert_eq!(open_stack(&marks, None), None);
        let normal = mark_look(&marks[&1], None);
        let member = mark_look(&marks[&1], Some(0));
        let other = mark_look(&marks[&3], Some(0));
        let other_collapsed = mark_look(&marks[&4], Some(0));
        assert!(member.bracket.r() > normal.bracket.r() && member.width > normal.width && member.tick > normal.tick);
        assert!(member.text.r() >= normal.text.r() && member.ground.a() > normal.ground.a());
        assert!(other.text.r() < normal.text.r() && other_collapsed.text.r() < mark_look(&marks[&4], None).text.r());
        // other stacks keep their bracket; no colour (gold) anywhere
        assert_eq!(other.bracket, normal.bracket);
        for l in [normal, member, other, other_collapsed] {
            assert!(l.bracket.r() == l.bracket.g() && l.bracket.g() == l.bracket.b());
            assert!(l.text.r() == l.text.g() && l.text.g() == l.text.b());
        }
    }

    #[test]
    fn empty_state_lines() {
        let s = empty_state(true, Phase::Grouping, 3, 120, false, false);
        assert_eq!(s.short, "Suche … 3 von 120 Tagen");
        assert_eq!(s.title, "Serien werden gesucht … 3 von 120 Tagen");
        let s = empty_state(true, Phase::Signatures, 40, 2321, false, true);
        assert_eq!(s.short, "Suche … 40 von 2321 Fotos · gedrosselt");
        assert!(s.title.ends_with("gedrosselt: Stromsparmodus"));
        let s = empty_state(true, Phase::WaitIndex, 0, 0, false, false);
        assert_eq!(s.short, "wartet auf den Suchindex");
        assert!(s.tip.contains("5 Minuten"));
        assert_eq!(empty_state(true, Phase::Idle, 0, 0, true, false).title, "Noch keine Serien gefunden");
        assert_eq!(empty_state(true, Phase::Idle, 0, 0, false, false).title, "Serien werden gesucht …");
        assert_eq!(empty_state(false, Phase::Idle, 0, 0, true, false).short, "Erkennung aus");
        // the ways in are always named
        assert!(empty_state(true, Phase::Idle, 0, 0, true, false).tip.contains("⌘G"));
    }

    #[test]
    fn menu_commands_without_series() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-series-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(dir.join(format!("image-{i}.png"))).unwrap();
        }
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        library.browse_folder(&dir, None);
        let ids: Vec<i64> = library.visible.iter().map(|&i| library.rows[i].image).collect();
        assert_eq!(ids.len(), 3);
        let status = |a: &[Action]| a.iter().find_map(|a| if let Action::Status(s) = a { Some(s.clone()) } else { None });
        // nothing to collapse, nothing to dissolve: said in the status bar
        assert_eq!(status(&library.stack_menu_command(StackMenu::ToggleAll)).as_deref(), Some("Noch keine Serien oder Stapel"));
        library.active = Some(ids[0]);
        assert_eq!(status(&library.stack_menu_command(StackMenu::Dissolve)).as_deref(), Some("Das Foto liegt in keiner Serie und keinem Stapel"));
        // photos of a viewed folder are never bundled (stacks live in the library): the reason, nothing else
        library.selection = HashSet::from([ids[0]]);
        library.nothing_selected = false;
        assert_eq!(status(&library.stack_menu_command(StackMenu::Group)).as_deref(), Some(crate::tr(super::view_folder_guard::NEEDS_LIBRARY)));
        // a selection of three photos opens the review like a series
        library.selection = ids.iter().copied().collect();
        let a = library.stack_menu_command(StackMenu::Review);
        assert!(status(&a).is_none(), "{:?}", status(&a));
        assert!(library.series.review.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
