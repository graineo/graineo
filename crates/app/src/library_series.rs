//! Series and stacks in the grid (and the filmstrip, which shows the same list).
//!
//! - A collapsed stack shows one tile: the cover (or, under a filter, the matching member with the best rating) with
//!   two thin edges behind the photo and a number at its bottom right ("7", filtered "3 von 7"); a small symbol says the
//!   kind (series, exposure bracket, own stack), a dot "not reviewed yet". Same tile size as every photo.
//! - An expanded stack: its members in file order (merged results first), whatever the grid's sort direction,
//!   numbered 1/7 … 7/7 in that order, on a slightly lighter ground with a thin bracket line below, across line breaks.
//! - Only one photo of a stack shown (filter, source): it stands as a photo of its own, without marks.
//! - While comparing, every stack of a compared photo stands open with all its members (`SeriesView::held`).
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
    /// expanded: every second group in the grid (the two tones of the group ground alternate)
    pub alt: bool,
    /// held open by the comparison (photos of it are compared): expanded whatever its own state
    pub held: bool,
}

/// Ground tones of expanded groups (bluish dark; neighbouring groups alternate).
const GROUP_TONES: [Color32; 2] = [Color32::from_rgb(0x1c, 0x2a, 0x3c), Color32::from_rgb(0x2a, 0x2a, 0x40)];
/// The same tone, brighter by `k` (0 = unchanged, 1 = twice as bright): an active or selected tile keeps the colour
/// of its group.
pub(super) fn lighter(c: Color32, k: f32) -> Color32 {
    let f = |v: u8| (v as f32 * (1.0 + k)).round().min(255.0) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Gap left at the ends of a group (between neighbouring groups).
const GROUP_GAP: f32 = 4.0;

/// A member file of a stack as the grid orders it.
#[derive(Clone, Copy, Debug)]
struct Member {
    file: i64,
    /// index into `SeriesView::stacks`
    stack: usize,
    /// place in the file order of the stack ([`file_order`])
    order: usize,
}

/// Order of the members of a stack: merged results (HDR) first, then the files by name – the order of the camera,
/// whatever the sort direction of the grid (QA 1.5.0-test.5: "01, 03, 02" with the cover first and the rest turned
/// round in a grid sorted newest first). `(merged, name, file)` per member; returns the files in order.
fn file_order(mut v: Vec<(bool, String, i64)>) -> Vec<i64> {
    v.sort_by(|a, b| (!a.0, a.1.to_lowercase(), a.2).cmp(&(!b.0, b.1.to_lowercase(), b.2)));
    v.into_iter().map(|x| x.2).collect()
}

/// Order of the shown pictures of one stack (files, each once), in the file order of the stack. The tile numbers
/// are the places in this list, so they have no gaps when members are hidden.
fn shown_order(mut shown: Vec<Member>) -> Vec<i64> {
    shown.sort_by_key(|m| (m.order, m.file));
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
    /// a load of the stacks on its way: its number ([`Self::loads`]) and the channel
    loading: Option<(u64, std::sync::mpsc::Receiver<Vec<Stack>>)>,
    seen: Option<u64>,
    /// loads started so far (each load gets the next number)
    loads: u64,
    /// open / closed states written here and not yet seen in a load started after the write finished: a load that
    /// read the database before (a race with the background writer) must not turn them back
    pending: HashMap<i64, Pending>,
    /// writes of open / closed states started so far (the token of each)
    writes: u64,
    pub(super) ask: Option<ScopeAsk>,
    /// the question was answered: the next export/delete goes through without asking
    answered: bool,
    /// stacks (ids) held open while the comparison is shown: every stack a compared photo belongs to stands expanded
    /// in the grid and the filmstrip with all its members, so that others of it can be taken in (⌘-click). Only in
    /// memory: closing the comparison brings back each stack's own state.
    pub(super) held: HashSet<i64>,
    /// photos compared (also outside any stack): they stand in the filmstrip while comparing, also when the source
    /// or a filter would hide them (a selection made in another folder, QA 1.5.0-test.5)
    pub(super) held_photos: HashSet<i64>,
    /// members of the active photo's stack at the last filtering: where the active photo goes when it drops out
    pub(super) active_peers: Vec<i64>,
    /// [`crate::series_job::rows_generation`] the rows were last read for
    rows_seen: u64,
    /// test aid `GRAINEO_SERIES_TEST="compare=…"`: photos to compare and since when it waits
    test_compare: Option<(std::time::Instant, Vec<i64>)>,
    /// stack (id) opened by ⌥←/→ only in memory: it closes again as soon as the active photo leaves it
    pub(super) peek: Option<i64>,
    /// ⌥←/→ went into this stack (id): after the next filtering its first (forward) or last member becomes active;
    /// `from`: the step started on this tile (the cover) – then its neighbour in file order becomes active
    pub(super) enter: Option<Enter>,
    /// a stack was opened or closed: the filmstrip keeps the stack where it stands (taken by the app)
    pub(super) strip_keep: Option<std::time::Instant>,
}

/// An open / closed state written in the background ([`SeriesView::pending`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    collapsed: bool,
    /// the write that stores it
    token: u64,
    /// the write finished: loads with this number or higher read it from the database
    from_load: Option<u64>,
}

/// Pending step into a stack ([`SeriesView::enter`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Enter {
    pub(super) stack: i64,
    pub(super) forward: bool,
    pub(super) from: Option<i64>,
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

    /// Stacks read again from the database (load number `load`): states only in memory stay – a stack opened by ⌥
    /// ([`Self::peek`]) and open / closed states whose write this load may not have seen yet.
    fn adopt(&mut self, mut stacks: Vec<Stack>, load: u64) {
        self.pending.retain(|_, p| p.from_load.is_none_or(|f| load < f));
        for s in &mut stacks {
            if let Some(p) = self.pending.get(&s.id) {
                s.collapsed = p.collapsed;
            }
            if self.peek == Some(s.id) {
                s.collapsed = false;
            }
        }
        self.set(stacks);
    }

    /// A write of open / closed states starts (stacks `ids`): its token for [`Self::write_done`].
    fn write_started(&mut self, ids: &[i64], collapsed: bool) -> u64 {
        self.writes += 1;
        let token = self.writes;
        for &id in ids {
            self.pending.insert(id, Pending { collapsed, token, from_load: None });
        }
        token
    }

    /// The write `token` finished: the next load reads its states (failed: the database's state counts again).
    fn write_done(&mut self, token: u64, ok: bool) {
        let next = self.loads + 1;
        self.pending.retain(|_, p| {
            if p.token != token {
                return true;
            }
            p.from_load = Some(next);
            ok
        });
    }

    /// The stored state of stack `k` (a stack opened by ⌥ only in memory is stored collapsed).
    pub(super) fn stored_collapsed(&self, k: usize) -> bool {
        let s = &self.stacks[k];
        s.collapsed || self.peek == Some(s.id)
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
            ev = crate::menu::i18n::format::decimal(crate::menu::i18n::language(), r.ev_range.unwrap_or(0.0) as f64, 1)
        ),
        StackKind::Series => {
            let span = span_text(r.span_s);
            match r.sim_min {
                Some(sim) if r.model != "time" => crate::trf!("Serie: {n} Fotos in {t}, gleiches Motiv (Ähnlichkeit {s})", n = n, t = span, s = crate::menu::i18n::format::decimal(crate::menu::i18n::language(), sim as f64, 2)),
                _ => crate::trf!("Serie: {n} Fotos in {t} (nur nach Zeit, Bildinhalt noch unbekannt)", n = n, t = span),
            }
        }
        StackKind::Manual if s.origin == "lightroom" => crate::trf!("Stapel aus Lightroom: {n} Fotos", n = n),
        StackKind::Manual => crate::trf!("Stapel: {n} Fotos", n = n),
    }
}

/// The reason line of a stack number's tooltip: only what the button (kind symbol, number) does not say already –
/// the exposure range of a bracket, the span and likeness of a series, the origin of a Lightroom stack; `None` for an
/// own stack (its symbol and number say it all).
fn reason_short(s: &Stack) -> Option<String> {
    let r = &s.reason;
    match s.kind {
        StackKind::Bracket => r.ev_range.map(|ev| crate::trf!("Belichtungsreihe über {ev} EV", ev = crate::menu::i18n::format::decimal(crate::menu::i18n::language(), ev as f64, 1))),
        StackKind::Series => {
            let span = span_text(r.span_s);
            Some(match r.sim_min {
                Some(_) if r.model != "time" => crate::trf!("Serie in {t}, gleiches Motiv", t = span),
                _ => crate::trf!("Serie in {t}, nur nach Zeit", t = span),
            })
        }
        StackKind::Manual if s.origin == "lightroom" => Some(crate::tr("Stapel aus Lightroom").to_string()),
        StackKind::Manual => None,
    }
}

/// Menu and tooltip texts named after the kind of a stack: an exposure bracket is a
/// "Reihe", an automatic series a "Serie", an own (or Lightroom) stack a "Stapel".
pub(super) struct KindTexts {
    pub(super) open: &'static str,
    pub(super) close: &'static str,
    pub(super) review: &'static str,
    pub(super) take_out: &'static str,
    pub(super) split: &'static str,
    pub(super) dissolve: &'static str,
}

pub(super) fn kind_texts(kind: StackKind) -> KindTexts {
    match kind {
        StackKind::Bracket => KindTexts {
            open: crate::tr("Reihe aufklappen"),
            close: crate::tr("Reihe zuklappen"),
            review: crate::tr("Reihe auswählen und vergleichen"),
            take_out: crate::tr("Aus Reihe nehmen"),
            split: crate::tr("Reihe teilen ab hier"),
            dissolve: crate::tr("Reihe auflösen"),
        },
        StackKind::Series => KindTexts {
            open: crate::tr("Serie aufklappen"),
            close: crate::tr("Serie zuklappen"),
            review: crate::tr("Serie auswählen und vergleichen"),
            take_out: crate::tr("Aus Serie nehmen"),
            split: crate::tr("Serie teilen ab hier"),
            dissolve: crate::tr("Serie auflösen"),
        },
        StackKind::Manual => KindTexts {
            open: crate::tr("Stapel aufklappen"),
            close: crate::tr("Stapel zuklappen"),
            review: crate::tr("Stapel auswählen und vergleichen"),
            take_out: crate::tr("Aus Stapel nehmen"),
            split: crate::tr("Stapel teilen ab hier"),
            dissolve: crate::tr("Stapel auflösen"),
        },
    }
}

/// Text of the open / close command of a stack.
pub(super) fn toggle_text(kind: StackKind, collapsed: bool) -> &'static str {
    let t = kind_texts(kind);
    if collapsed { t.open } else { t.close }
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
        // a stack opened by ⌥←/→ closes again once the active photo left it (also by a click or a plain arrow)
        self.series_peek_check();
        // the job read shooting data (right after an import, without a sync): the rows follow in the background, the
        // active photo and the scroll position stay (`adopt_load`)
        let rows = crate::series_job::rows_generation();
        if rows != self.series.rows_seen {
            self.series.rows_seen = rows;
            if self.db.is_some() {
                crate::log("library: reload after the series job read shooting data");
                self.reload();
            }
        }
        if self.series.test_compare.as_ref().is_some_and(|(t, _)| t.elapsed() > std::time::Duration::from_secs(3)) {
            if let Some((_, ids)) = self.series.test_compare.take() {
                if let Some(&first) = ids.first() {
                    self.active = Some(first);
                    self.selection = ids.iter().copied().collect();
                    self.nothing_selected = false;
                    self.request_compare(None, None);
                }
            }
        }
        if let Some((load, rx)) = &self.series.loading {
            let load = *load;
            match rx.try_recv() {
                Ok(v) => {
                    self.series.loading = None;
                    let first = self.series.stacks.is_empty() && !v.is_empty();
                    self.series.adopt(v, load);
                    self.refilter = true;
                    // the grid changes now (members together, collapsed stacks shorter): one more frame at once, so
                    // the scroll area settles before the next click (otherwise it clamped its offset only in the frame
                    // of the next click and the grid jumped under the pointer: a wrong photo was chosen)
                    if let Some(c) = &self.quick_ctx {
                        c.request_repaint();
                    }
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
            self.series.loads += 1;
            self.series.loading = Some((self.series.loads, rx));
        }
    }

    /// The filtered list with stacks applied: a collapsed stack keeps one tile, an expanded one shows its members
    /// together (cover first, then capture order in the grid's direction: newest first in a grid sorted newest first). Fills the marks for drawing. "Ähnliche Fotos" and "Duplikate"
    /// show every photo on its own.
    pub(super) fn series_collapse(&mut self, v: Vec<usize>) -> Vec<usize> {
        self.series.marks.clear();
        let forced = self.source == Source::Series;
        let held = |s: &Stack| self.compare.is_some() && self.series.held.contains(&s.id);
        if self.series.stacks.is_empty() || matches!(self.source, Source::Similar(_) | Source::Duplicates) {
            return if forced { Vec::new() } else { v };
        }
        let rows = &self.rows;
        let st = &self.series;
        // each member's file: its stack and its place in the file order (virtual copies follow their original
        // through the file)
        let mut member: HashMap<i64, Member> = HashMap::new();
        for (k, s) in st.stacks.iter().enumerate() {
            let files: Vec<(bool, String, i64)> = s
                .images
                .iter()
                .filter_map(|img| self.index.get(img).map(|&ri| &rows[ri]))
                .map(|z| (z.status as i64 == library::merge::STATUS_VIRTUAL, z.name.clone(), z.file))
                .collect();
            for (j, file) in file_order(files).into_iter().enumerate() {
                member.insert(file, Member { file, stack: k, order: j });
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
        // a stack of which only one picture is shown here is no stack in this view: the photo stands as a photo of its
        // own – no number, no ground, nothing to expand (the others are hidden by a filter or the source, or are no
        // longer in the library). Before, such a tile said "1/1" and a click "expanded" it to the same photo. The view
        // "Serien" keeps every stack with at least two pictures in the library (its click compares all of them).
        members.retain(|&k, vis| {
            let s = &st.stacks[k];
            if forced && !held(s) {
                return self.stack_pictures(s) >= 2;
            }
            vis.iter().map(|&r| rows[r].file).collect::<HashSet<_>>().len() >= 2
        });
        // place of each shown picture (file → 0-based), counted over the shown pictures only: the numbers on the tiles
        // run 1, 2, 3 … without gaps, also when members are hidden (filter, source, a camera JPEG folded into its RAW)
        let mut rank: HashMap<i64, usize> = HashMap::new();
        for vis in members.values() {
            let shown: Vec<Member> = vis.iter().filter_map(|&r| stack_of(&rows[r])).collect();
            for (p, file) in shown_order(shown).into_iter().enumerate() {
                rank.insert(file, p);
            }
        }
        let place = |z: &Row| rank.get(&z.file).copied().unwrap_or(usize::MAX);
        let mut out = Vec::with_capacity(v.len());
        let mut marks = HashMap::new();
        let mut done: HashSet<usize> = HashSet::new();
        let mut expanded_groups = 0usize;
        for &i in &v {
            let Some(k) = stack_of(&rows[i]).map(|m| m.stack).filter(|k| members.contains_key(k)) else {
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
            let hold = held(s);
            if collapsed(s, forced) && !hold {
                let top = s.top();
                // the cover if it matches the filter, else the matching original rated best
                let rep = vis
                    .iter()
                    .copied()
                    .find(|&r| rows[r].image == top)
                    .or_else(|| vis.iter().copied().filter(|&r| rows[r].copy.is_none()).max_by_key(|&r| ((rows[r].flag == 1) as u8, rows[r].stars, std::cmp::Reverse(place(&rows[r])))))
                    .unwrap_or(vis[0]);
                out.push(rep);
                marks.insert(rows[rep].image, Mark { stack: k, pos: place(&rows[rep]).saturating_add(1), total, shown, collapsed: true, first: true, last: true, alt: false, held: false });
            } else {
                let mut ordered = vis.clone();
                ordered.sort_by_key(|&r| (place(&rows[r]), rows[r].copy.is_some(), rows[r].image));
                let n = ordered.len();
                for (j, &r) in ordered.iter().enumerate() {
                    out.push(r);
                    marks.insert(rows[r].image, Mark { stack: k, pos: place(&rows[r]).saturating_add(1), total, shown, collapsed: false, first: j == 0, last: j + 1 == n, alt: expanded_groups % 2 == 1, held: hold });
                }
                expanded_groups += 1;
            }
        }
        if forced {
            // "Serien": not reviewed first, newest first
            // (a series held open by the comparison keeps its members together in their order)
            out.sort_by_key(|&r| {
                let m = marks.get(&rows[r].image);
                let s = m.map(|m| &st.stacks[m.stack]);
                (s.is_some_and(|s| s.reviewed.is_some()), std::cmp::Reverse(s.and_then(|s| s.day.clone())), s.map(|s| s.top()), m.map(|m| m.pos), rows[r].image)
            });
        }
        self.series.marks = marks;
        self.series_enter_resolve(&out);
        out
    }

    /// Ground behind the tiles of an expanded stack (before the cell is drawn): one tinted surface
    /// over the whole group (the tile itself takes the same tone, see [`Self::group_tone`]), rounded and with a gap at
    /// the group's ends, open at a line break.
    pub(super) fn paint_series_ground(&self, p: &egui::Painter, rect: egui::Rect, image: i64) {
        let Some(m) = self.series.marks.get(&image).filter(|m| !m.collapsed) else { return };
        let r = egui::Rect::from_min_max(egui::pos2(rect.left() + if m.first { GROUP_GAP } else { 0.0 }, rect.top()), egui::pos2(rect.right() - if m.last { GROUP_GAP } else { 0.0 }, rect.bottom()));
        let round = |end: bool| if end { 6 } else { 0 };
        let corners = egui::CornerRadius { nw: round(m.first), sw: round(m.first), ne: round(m.last), se: round(m.last) };
        p.rect_filled(r, corners, GROUP_TONES[m.alt as usize]);
    }

    /// The tone a tile of an expanded group takes, so that the group reads as one surface.
    pub(super) fn group_tone(&self, image: i64) -> Option<Color32> {
        let m = self.series.marks.get(&image).filter(|m| !m.collapsed)?;
        Some(GROUP_TONES[m.alt as usize])
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
        }
        // expanded: the group's ground shows that the tiles belong together (paint_series_ground)
        // number at the bottom right of the photo: "7" / "3 von 7" (shown of all) collapsed, "2/7" (place among the
        // shown) expanded. While the pointer is on the tile it becomes a button with its action ("8 ▸", "◂";
        // in the view "Serien", which shows one tile per series, "8 vergleichen"), anchored at the same corner.
        let series_view = self.source == Source::Series;
        let hot = ui.rect_contains_pointer(rect);
        let text = match (hot, series_view, m.collapsed) {
            (true, true, _) => crate::trf!("{n} vergleichen", n = m.total),
            (true, false, true) => format!("{} ▸", m.total),
            (true, false, false) => format!("{} ◂", tile_number(&m)),
            _ => tile_number(&m),
        };
        let g = p.layout_no_wrap(text, egui::FontId::proportional(10.5), if hot { Color32::WHITE } else { look.text });
        let symbol_w = if m.collapsed || m.first { 12.0 } else { 0.0 };
        let dot_w = if s.is_series() && s.reviewed.is_none() && (m.collapsed || m.first) { 8.0 } else { 0.0 };
        let size = egui::vec2(g.size().x + 10.0 + symbol_w + dot_w, if hot { 18.0 } else { 16.0 });
        let field = egui::Rect::from_min_size(r.right_bottom() - size - egui::vec2(3.0, 3.0), size);
        let resp = ui.interact(field, egui::Id::unique(("stapel-zahl", z.image)), egui::Sense::click());
        let bg = if resp.hovered() { Color32::from_gray(0x50) } else if hot { Color32::from_black_alpha(230) } else { look.ground };
        p.rect_filled(field, 0.0, bg);
        if hot {
            p.rect_stroke(field, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x9a)), egui::StrokeKind::Inside);
        }
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
        let tip = self.mark_tip(&m, series_view, false);
        lr::hover_tip(ui, &resp, &tip);
        if resp.clicked() {
            return Some(number_click(series_view));
        }
        None
    }

    /// Do the stack keys (S, N, ⇧⌘G) act on stack `k`? They take the series of the active photo, so menus and tooltips
    /// name them only there.
    fn keys_reach(&self, k: usize) -> bool {
        self.active.and_then(|a| self.series.of(a)) == Some(k)
    }

    /// Tooltip of a stack number (grid tile and filmstrip cell), at most two lines ("zu viel
    /// Geschwurbel"): what is new beyond the button – why the photos belong together ([`reason_short`]; nothing when
    /// the kind symbol and the number say it all) – then the action with its key. `strip`: without a reason line the
    /// double click of the filmstrip takes the second line.
    fn mark_tip(&self, m: &Mark, series_view: bool, strip: bool) -> String {
        let s = &self.series.stacks[m.stack];
        // the keys act on the series of the active photo: named only on that one (QA 5)
        let keys = self.keys_reach(m.stack);
        let action = if series_view {
            if keys { lr::with_shortcut("Im Vergleich öffnen", "stack_review") } else { crate::tr("Im Vergleich öffnen").to_string() }
        } else if m.held {
            crate::tr("Die verglichenen Serien bleiben aufgeklappt, solange der Vergleich offen ist").to_string()
        } else if keys {
            lr::with_shortcut(toggle_text(s.kind, m.collapsed), "stack_toggle")
        } else {
            toggle_text(s.kind, m.collapsed).to_string()
        };
        match reason_short(s) {
            Some(r) => format!("{r}\n{action}"),
            None if strip && !series_view && !m.held => {
                let double = if m.collapsed { crate::tr("Doppelklick auf die Kachel: aufklappen") } else { crate::tr("Doppelklick auf das aktive Foto: zuklappen") };
                format!("{action}\n{double}")
            }
            None => action,
        }
    }

    /// The stack number of a filmstrip cell as a button: text, tooltip (as on the grid tile, plus the double click)
    /// and whether a click acts (`false`: greyed – held open by the comparison, or nothing to fold in this view).
    pub fn strip_mark(&self, image: i64) -> Option<StripMark> {
        let m = self.series.marks.get(&image)?;
        let series_view = self.source == Source::Series;
        let usable = series_view || (!m.held && self.toggle_refused(image).is_none());
        Some(StripMark { text: tile_number(m), tip: self.mark_tip(m, series_view, true), usable })
    }

    /// Click on the stack number of a filmstrip cell: as on the grid tile (open / close the stack; in the view
    /// "Serien" compare it). The photo selected stays.
    pub fn strip_mark_click(&mut self, image: i64) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.strip_mark(image).is_some_and(|m| m.usable) {
            let cmd = number_click(self.source == Source::Series);
            self.series_command(cmd, image, &mut actions);
        }
        actions
    }

    /// Double click on a filmstrip cell: a collapsed stack opens; on the photo that was
    /// already active before the double click, its open stack closes (only this one). Returns the active photo when
    /// something changed.
    pub fn strip_double_click(&mut self, image: i64, was_active: bool) -> Option<i64> {
        let m = *self.series.marks.get(&image)?;
        if m.held || self.source == Source::Series || self.toggle_refused(image).is_some() || (!m.collapsed && !was_active) {
            return None;
        }
        self.series_command(StackCmd::Toggle, image, &mut Vec::new());
        if m.collapsed {
            // opened: the photo double-clicked (the cover) stays active
            self.active = Some(image);
        }
        self.active
    }

    /// ⌥←/→: one step through the photos as if every stack were open. On a collapsed
    /// stack – the active tile, or the tile the step reaches – the stack opens and its first (→) or last (←) member
    /// becomes active after the next filtering ([`SeriesView::enter`]); a stack opened like this closes again once
    /// the active photo leaves it ([`Self::series_peek_check`]), its stored state stays collapsed. `false`: nothing to
    /// do here (view "Serien", comparison, no active photo, end of the list) – the caller steps as before.
    pub fn series_step(&mut self, forward: bool) -> bool {
        let Some(a) = self.active else { return false };
        if self.source == Source::Series || self.compare.is_some() || self.series.enter.is_some() {
            return self.series.enter.is_some();
        }
        let Some(p) = self.position_of(a) else { return false };
        if self.peek_into(a, forward, true) {
            return true;
        }
        let q = p as isize + if forward { 1 } else { -1 };
        let Some(&r) = (q >= 0).then(|| self.visible.get(q as usize)).flatten() else { return false };
        let b = self.rows[r].image;
        self.sel_ext = None;
        self.active = Some(b);
        self.nothing_selected = false;
        self.selection.clear();
        self.to_active = true;
        self.zoom100 = false;
        self.pan = egui::Vec2::ZERO;
        self.series_peek_check();
        self.peek_into(b, forward, false);
        true
    }

    /// Open the collapsed stack whose tile is `image` for a step into it (in memory only). `own`: the step starts on
    /// that tile (then it goes on from the tile's own place among the members).
    fn peek_into(&mut self, image: i64, forward: bool, own: bool) -> bool {
        let Some(m) = self.series.marks.get(&image).copied().filter(|m| m.collapsed && !m.held) else { return false };
        if self.toggle_refused(image).is_some() {
            return false;
        }
        let s = &mut self.series.stacks[m.stack];
        s.collapsed = false;
        let id = s.id;
        // an earlier peek of another stack is closed by `series_peek_check` (the active photo left it)
        self.series.peek = Some(id);
        self.series.enter = Some(Enter { stack: id, forward, from: own.then_some(image) });
        self.series.strip_keep = Some(std::time::Instant::now());
        self.refilter = true;
        true
    }

    /// Every frame and after a ⌥ step: a stack opened by ⌥←/→ closes again when the active photo is no longer in it.
    pub(super) fn series_peek_check(&mut self) {
        let Some(id) = self.series.peek else { return };
        if self.series.enter.is_some() {
            return;
        }
        let here = self.active.and_then(|a| self.series.of(a)).map(|k| self.series.stacks[k].id);
        if here == Some(id) {
            return;
        }
        self.series.peek = None;
        if let Some(s) = self.series.stacks.iter_mut().find(|s| s.id == id) {
            s.collapsed = true;
            self.series.strip_keep = Some(std::time::Instant::now());
            self.refilter = true;
        }
    }

    /// After filtering: the member a ⌥ step went to (see [`Self::series_step`]). `out`: the filtered list.
    fn series_enter_resolve(&mut self, out: &[usize]) {
        let Some(e) = self.series.enter else { return };
        let Some(k) = self.series.stacks.iter().position(|s| s.id == e.stack) else {
            self.series.enter = None;
            return;
        };
        let members: Vec<i64> = out.iter().map(|&r| self.rows[r].image).filter(|b| self.series.marks.get(b).is_some_and(|m| m.stack == k && !m.collapsed)).collect();
        self.series.enter = None;
        // started on the tile itself: one step from its place in file order (QA 5: from the cover _02 ⌥→ went to _01);
        // past the stack's end the step leaves it (the next tile), at the end of the list it stays
        let from = e.from.and_then(|f| out.iter().position(|&r| self.rows[r].image == f));
        let pick = match from {
            Some(p) => {
                let q = p as isize + if e.forward { 1 } else { -1 };
                (q >= 0).then(|| out.get(q as usize)).flatten().map(|&r| self.rows[r].image).or(e.from)
            }
            None if e.forward => members.first().copied(),
            None => members.last().copied(),
        };
        if let Some(b) = pick {
            self.active = Some(b);
            self.selection.clear();
            self.nothing_selected = false;
            self.to_active = true;
        }
    }

    /// ↩ on a collapsed stack (the keyboard's double click of the filmstrip): it opens, the cover stays active.
    /// `false`: the active photo is no collapsed stack here – ↩ keeps its own meaning (open the single photo).
    pub fn enter_expand(&mut self) -> bool {
        let Some(a) = self.active else { return false };
        let collapsed = self.series.marks.get(&a).is_some_and(|m| m.collapsed && !m.held);
        if !collapsed || self.source == Source::Series || self.toggle_refused(a).is_some() {
            return false;
        }
        self.series_command(StackCmd::Toggle, a, &mut Vec::new());
        self.active = Some(a);
        true
    }

    /// S in Develop: open / close the stack of the active photo (as S in the grid).
    pub fn toggle_active_stack(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(a) = self.active {
            match self.toggle_refused(a) {
                Some(why) if self.series.of(a).is_some() => actions.push(Action::Status(why.to_string())),
                _ => self.series_command(StackCmd::Toggle, a, &mut actions),
            }
        }
        actions
    }

    /// A stack was opened or closed in the last moments: the filmstrip keeps it where it stands (taken once).
    pub fn take_strip_keep(&mut self) -> bool {
        self.series.strip_keep.take().is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2))
    }

    /// The stack (id) a photo belongs to.
    pub fn stack_key(&self, image: i64) -> Option<i64> {
        self.series.of(image).map(|k| self.series.stacks[k].id)
    }

    /// The list is about to change (a filtering is pending).
    pub fn list_pending(&self) -> bool {
        self.refilter || self.series.enter.is_some()
    }

    /// Top entry of a filmstrip cell's context menu when the cell is too small for the number button: open / close
    /// its stack (named after the kind, with the key; greyed with the reason). Returns `true` when chosen.
    pub fn strip_toggle_menu(&self, ui: &mut egui::Ui, image: i64) -> bool {
        if self.source == Source::Series {
            return false;
        }
        let Some(m) = self.series.marks.get(&image) else { return false };
        let s = &self.series.stacks[m.stack];
        let text = toggle_text(s.kind, m.collapsed);
        let why = if m.held { Some(crate::tr("Die verglichenen Serien bleiben aufgeklappt, solange der Vergleich offen ist")) } else { self.toggle_refused(image) };
        let key = if self.keys_reach(m.stack) { crate::shortcuts::display("stack_toggle") } else { String::new() };
        let chosen = lr::menu_item_why(ui, text, &key, why.is_none(), why.unwrap_or(""));
        ui.separator();
        chosen && why.is_none()
    }

    /// The tile that stands for `image`: itself, or the tile of its collapsed stack when it is hidden behind it
    /// (the active photo inside a collapsed stack lights up that tile, QA 1.5.0-test.5).
    pub fn tile_for(&self, image: i64) -> i64 {
        if self.series.marks.contains_key(&image) || self.index.get(&image).is_none() {
            return image;
        }
        let Some(k) = self.series.of(image) else { return image };
        self.series.marks.iter().find(|(_, m)| m.stack == k && m.collapsed).map_or(image, |(&b, _)| b)
    }


    /// Context menu entries for a photo in a stack (and "bundle" for a selection). Returns the chosen command.
    pub(super) fn series_menu(&self, ui: &mut egui::Ui, image: i64) -> Option<StackCmd> {
        use crate::shortcuts::display;
        let mut chosen = None;
        let stack = self.series.of(image).map(|k| &self.series.stacks[k]);
        let several = self.selection.len() > 1 && self.selection.contains(&image);
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
            let t = kind_texts(s.kind);
            let toggle = toggle_text(s.kind, self.series.marks.get(&image).is_some_and(|m| m.collapsed));
            // the keys act on the series of the active photo: named only when it is this one (QA 5)
            let reach = self.series.of(image).is_some_and(|k| self.keys_reach(k));
            let display = |id: &str| if reach { display(id) } else { String::new() };
            // nothing to fold where the view shows the photo alone (only it of the stack shown, or "Ähnliche Fotos" /
            // "Duplikate", which show every photo on its own): greyed with the reason
            match self.toggle_refused(image) {
                Some(why) => {
                    lr::menu_item_why(ui, toggle, &display("stack_toggle"), false, why);
                }
                None => item(ui, StackCmd::Toggle, toggle, display("stack_toggle"), true),
            }
            item(ui, StackCmd::Review, t.review, display("stack_review"), true);
            item(ui, StackCmd::Cover, crate::tr("Als Titelfoto"), String::new(), s.top() != image);
            item(ui, StackCmd::TakeOut, t.take_out, String::new(), true);
            item(ui, StackCmd::Split, t.split, String::new(), s.images.first() != Some(&image));
            item(ui, StackCmd::Dissolve, t.dissolve, display("stack_dissolve"), true);
            // "HDR zusammenfügen …" is the entry of the photo menu above (it takes the whole stack as sources)
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
        // ⇧S comes as a menu command (menu.rs), everywhere: `stack_menu_command(StackMenu::ToggleAll)`
        if crate::shortcuts::pressed(ctx, "stack_group") {
            self.series_command(StackCmd::Group, active, actions);
        }
        if crate::shortcuts::pressed(ctx, "stack_dissolve") {
            self.series_command(StackCmd::Dissolve, active, actions);
        }
        if crate::shortcuts::pressed(ctx, "stack_review") {
            match review_scope(&self.target(), |b| self.series.of(b), |k| self.series.stacks[k].images.iter().filter(|b| !self.folded_jpeg(**b)).count()) {
                // several photos selected: N compares them, as the button "Vergleichen" does (its tooltip names N)
                ReviewScope::Selection => self.request_compare(None, None),
                ReviewScope::Series => self.series_command(StackCmd::Review, active, actions),
            }
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

    /// Why expanding or collapsing the stack of `image` does nothing in this view (`None`: it does something).
    fn toggle_refused(&self, image: i64) -> Option<&'static str> {
        let k = self.series.of(image)?;
        if self.series.marks.values().any(|m| m.stack == k) {
            return None;
        }
        Some(if matches!(self.source, Source::Similar(_) | Source::Duplicates) {
            crate::tr("Diese Ansicht zeigt jedes Foto einzeln")
        } else {
            crate::tr("Nur dieses Foto davon ist in der Ansicht sichtbar")
        })
    }

    /// ⇧S and the button "all series": collapse all stacks when every shown one is open, else open all (mixed or all
    /// collapsed: a click opens them all). The button shows the same state ([`Self::series_all`]).
    pub(super) fn series_toggle_all(&mut self) {
        let collapse = self.series_all() == SeriesAll::Collapse;
        self.series.peek = None;
        self.series.strip_keep = Some(std::time::Instant::now());
        for s in self.series.stacks.iter_mut() {
            s.collapsed = collapse;
        }
        // collapsing: the active photo's stack keeps one tile, its cover becomes the active photo (as S does)
        if collapse && self.compare.is_none() {
            if let Some(k) = self.active.and_then(|a| self.series.of(a)) {
                self.active = Some(self.series.stacks[k].top());
            }
        }
        self.refilter = true;
        // the active photo stays in view (the grid grows or shrinks above it)
        self.to_active = true;
        self.store_all_collapsed(collapse);
    }

    /// What the button "all series" offers in the current view (grid row and filmstrip header).
    pub fn series_all(&self) -> SeriesAll {
        if self.source == Source::Series {
            return SeriesAll::SeriesView;
        }
        all_state(self.series.marks.values())
    }

    /// The button "all series" was clicked (grid row or filmstrip header): as ⇧S; nothing without stacks in the view.
    pub fn series_all_click(&mut self) {
        if matches!(self.series_all(), SeriesAll::Expand | SeriesAll::Collapse) {
            self.series_toggle_all();
        }
    }

    /// Run a stack command for a photo (the selection for "bundle").
    pub(super) fn series_command(&mut self, cmd: StackCmd, image: i64, actions: &mut Vec<Action>) {
        let k = self.series.of(image);
        match cmd {
            StackCmd::Toggle => {
                let Some(k) = k else { return };
                // the stack has no tile in this view (only this photo of it shown): nothing to fold, its state stays
                if !self.series.marks.values().any(|m| m.stack == k) {
                    return;
                }
                let now_collapsed = self.series.marks.values().any(|m| m.stack == k && m.collapsed);
                // a stack opened by ⌥←/→ becomes an ordinary one (its state is written now)
                self.series.peek = None;
                self.series.strip_keep = Some(std::time::Instant::now());
                let s = &mut self.series.stacks[k];
                s.collapsed = !now_collapsed;
                let (id, c, top) = (s.id, s.collapsed, s.top());
                // collapsing: the tile left is the cover – it becomes the active photo, but only when the active photo
                // was in this stack (QA 5: closing another series by its number made its cover active)
                if c && self.active.is_some_and(|a| self.series.of(a) == Some(k)) {
                    self.active = Some(top);
                }
                self.refilter = true;
                self.store_collapsed(vec![id], c);
            }
            // the series as a whole into the comparison (a selection without a series: the selection)
            StackCmd::Review => self.request_compare(k.map(|k| self.series.stacks[k].id), None),
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
                let label = kind_texts(self.series.stacks[k].kind).dissolve.to_string();
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
                self.series_change(move |db| ser::take_out(db, image)?.ok_or_else(|| crate::tr("Das Foto liegt in keinem Stapel").to_string()), k.map_or(crate::tr("Aus Serie nehmen"), |k| kind_texts(self.series.stacks[k].kind).take_out).to_string());
            }
            StackCmd::Split => {
                self.series_change(move |db| ser::split_at(db, image)?.ok_or_else(|| crate::tr("Hier lässt sich nicht teilen").to_string()), crate::tr("Serie teilen").to_string());
            }
        }
    }

    /// Store the open / closed state of stacks `ids` in the background (the state in memory is set already). A load of
    /// the stacks that read the database before the write cannot turn it back ([`SeriesView::adopt`]).
    pub(super) fn store_collapsed(&mut self, ids: Vec<i64>, collapsed: bool) {
        let token = self.series.write_started(&ids, collapsed);
        let started = self.in_background(move |db| ids.iter().try_for_each(|&id| ser::set_collapsed(db, id, collapsed)), move |b, _, r| b.series.write_done(token, r.is_ok()));
        if !started {
            self.series.write_done(token, true);
        }
    }

    /// Store one open / closed state for every stack (⇧S, test aid), as [`Self::store_collapsed`].
    fn store_all_collapsed(&mut self, collapsed: bool) {
        let ids: Vec<i64> = self.series.stacks.iter().map(|s| s.id).collect();
        let token = self.series.write_started(&ids, collapsed);
        let started = self.in_background(move |db| ser::set_all_collapsed(db, collapsed), move |b, _, r| b.series.write_done(token, r.is_ok()));
        if !started {
            self.series.write_done(token, true);
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
            self.store_collapsed(ask.stacks.clone(), false);
            self.series.answered = true;
            action(actions, self);
        } else if a == crate::dialog::Answer::Cancel {
            self.series.ask = None;
        }
    }

    /// Left panel: row "Serien", as high as every row of the catalog. Its number column says what is there or going
    /// on: "3 offen" (series not reviewed yet), while the detection runs or before it found any "sucht 19/27",
    /// "wartet", "keine", "aus"; the details in the tooltip. No line of its own below it (found in 1.5.0-test.6:
    /// the empty line looked like a gap).
    pub(super) fn series_row(&mut self, ui: &mut egui::Ui) {
        let open = self.series.open_count();
        let text = crate::tr("Serien");
        let empty = self.series.stacks.is_empty();
        let now = crate::series_job::row_state();
        let (phase, done, total) = (now.phase, now.done, now.total);
        let st = crate::series_job::empty_state(now.enabled, phase, done, total, now.passed, now.low_power);
        let searching = phase != crate::series_job::Phase::Idle;
        let right = series_row_count(empty, searching, open, &st.count);
        let (click, _, resp) = Self::list_row_icon(ui, Some(lr::Icon::CopySimple), text, right, self.source_marked(&Source::Series), 0, None, empty);
        if empty || searching {
            let mut tip = format!("{}\n{}", st.title, st.tip);
            if !empty {
                tip = format!("{}\n{tip}", crate::tr("Alle Serien und Stapel, je eine Kachel; ungesichtete zuerst"));
            }
            lr::hover_tip(ui, &resp, &tip);
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
            // as the button: what it does, or why it does nothing here (the reason of the greyed button)
            match self.series_all() {
                _ if self.series.stacks.is_empty() => actions.push(Action::Status(crate::tr("Noch keine Serien oder Stapel").into())),
                SeriesAll::Expand | SeriesAll::Collapse => self.series_toggle_all(),
                other => actions.push(Action::Status(other.look().1)),
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
    /// `collapse` / `expand` (all stacks collapsed / expanded), `source` (source "Serien"), `review[:light|:duel]` (review of the first series).
    fn series_test(&mut self) {
        if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let Ok(spec) = crate::env::var("GRAINEO_SERIES_TEST") else { return };
        for part in spec.split(';') {
            match part.trim() {
                // stored like a click (a reload keeps it)
                "collapse" | "expand" => {
                    let collapsed = part.trim() == "collapse";
                    self.series.peek = None;
                    for s in self.series.stacks.iter_mut() {
                        s.collapsed = collapsed;
                    }
                    self.store_all_collapsed(collapsed);
                }
                "source" => {
                    self.enter_special_source();
                    self.source = Source::Series;
                }
                p if p.starts_with("review") => {
                    self.compare_test_open(p.strip_prefix("review:").unwrap_or(""));
                }
                // "compare=11+15+12": these photos (image ids) selected and compared, 3 s after the stacks are there
                // (the start restores the last active photo meanwhile)
                p if p.starts_with("compare=") => {
                    let ids: Vec<i64> = p["compare=".len()..].split('+').filter_map(|t| t.trim().parse().ok()).collect();
                    self.series.test_compare = Some((std::time::Instant::now(), ids));
                }
                _ => {}
            }
        }
        self.refilter = true;
    }
}

/// State of the button "all series and stacks expand / collapse".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeriesAll {
    /// no series or stack among the shown photos: greyed with the reason
    None,
    /// the view "Serien" shows one tile per series on purpose: greyed with the reason
    SeriesView,
    /// at least one shown stack is collapsed: a click expands all
    Expand,
    /// every shown stack is expanded: a click collapses all
    Collapse,
    /// the only stacks shown are held open by the comparison (their photos are compared): greyed with the reason
    Held,
}

/// Number column of the row "Serien": the state of the detection while it searches or nothing was found yet,
/// otherwise the series not reviewed yet ("3 offen"), else nothing.
fn series_row_count(empty: bool, searching: bool, open: usize, state: &str) -> Option<String> {
    if empty || searching {
        Some(state.to_string())
    } else {
        (open > 0).then(|| crate::trf!("{n} offen", n = thousands(open)))
    }
}

/// The stack number of a filmstrip cell ([`LibraryView::strip_mark`]).
pub struct StripMark {
    pub text: String,
    pub tip: String,
    /// a click acts (otherwise greyed, the tooltip says why)
    pub usable: bool,
}

/// Rectangle of the stack number on a filmstrip cell, its bottom right corner at `corner`; `button`: at least 18 pt
/// high (a click area).
pub fn strip_number_rect(p: &egui::Painter, corner: egui::Pos2, text: &str, button: bool) -> egui::Rect {
    let g = p.layout_no_wrap(text.to_string(), egui::FontId::proportional(9.5), Color32::from_gray(0xe8));
    let h = if button { 18.0 } else { g.size().y + 2.0 };
    egui::Rect::from_min_max(corner - egui::vec2(g.size().x + 8.0, h), corner)
}

/// Stack number on a filmstrip cell in `r` (small, on a dark ground); `hot`: under the pointer (a button), `dim`: greyed.
pub fn paint_strip_number(p: &egui::Painter, r: egui::Rect, text: &str, hot: bool, dim: bool) {
    let col = if dim { Color32::from_gray(0x80) } else { Color32::from_gray(0xe8) };
    let g = p.layout_no_wrap(text.to_string(), egui::FontId::proportional(9.5), col);
    p.rect_filled(r, 0.0, if hot { Color32::from_gray(0x50) } else { Color32::from_black_alpha(215) });
    if hot {
        p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x9a)), egui::StrokeKind::Inside);
    }
    p.galley(egui::pos2(r.left() + 4.0, r.center().y - g.size().y / 2.0), g, col);
}

/// State from the marks of the shown photos.
/// Stacks held open by the comparison do not count: the button cannot fold them (a click that changed nothing on
/// screen would be a silent button).
fn all_state<'a>(marks: impl IntoIterator<Item = &'a Mark>) -> SeriesAll {
    let (mut any, mut held) = (false, false);
    for m in marks {
        if m.held {
            held = true;
            continue;
        }
        if m.collapsed {
            return SeriesAll::Expand;
        }
        any = true;
    }
    match (any, held) {
        (true, _) => SeriesAll::Collapse,
        (false, true) => SeriesAll::Held,
        (false, false) => SeriesAll::None,
    }
}

impl SeriesAll {
    /// Symbol (what a click does: spread out / draw together) and tooltip; `false`: greyed.
    fn look(self) -> (lr::Icon, String, bool) {
        match self {
            SeriesAll::None => (lr::Icon::ArrowsOutLineH, crate::tr("Keine Serien oder Stapel in dieser Ansicht").to_string(), false),
            SeriesAll::SeriesView => (lr::Icon::ArrowsOutLineH, crate::tr("Die Ansicht „Serien“ zeigt je Serie eine Kachel").to_string(), false),
            SeriesAll::Expand => (lr::Icon::ArrowsOutLineH, lr::with_shortcut("Alle Serien aufklappen", "stack_toggle_all"), true),
            SeriesAll::Collapse => (lr::Icon::ArrowsInLineH, lr::with_shortcut("Alle Serien zuklappen", "stack_toggle_all"), true),
            SeriesAll::Held => (lr::Icon::ArrowsInLineH, crate::tr("Die verglichenen Serien bleiben aufgeklappt, solange der Vergleich offen ist").to_string(), false),
        }
    }
}

/// The button "all series" (one symbol cell of `w` × `h` on the ground of the button groups, always in its place).
/// Greyed, it says why in the tooltip and ignores clicks. Returns: clicked while usable.
pub fn series_all_button(ui: &mut egui::Ui, state: SeriesAll, w: f32, h: f32) -> bool {
    let (icon, tip, usable) = state.look();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let hot = usable && resp.hovered();
    let p = ui.painter();
    p.rect_filled(rect, 0.0, if hot { Color32::from_gray(0x2e) } else { lr::GROUP_FILL });
    let color = if !usable { Color32::from_gray(0x5a) } else if hot { Color32::WHITE } else { lr::TEXT };
    lr::paint_icon(p, egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), icon, color);
    lr::hover_tip(ui, &resp, &tip);
    usable && resp.clicked()
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
        // dark enough to read on a nearly white photo (QA run 3: "6/6" on an overexposed frame)
        ground: Color32::from_black_alpha(215),
    };
    match open {
        Some(k) if k == m.stack => MarkLook { bracket: Color32::from_gray(0xd0), width: 1.5, tick: 7.0, text: Color32::from_gray(0xf8), ground: Color32::from_black_alpha(235), ..normal },
        // other stacks while one is open: dimmed text, the ground stays dark (readable on bright photos)
        Some(_) => MarkLook { text: Color32::from_gray(0xa0), ground: Color32::from_black_alpha(200), ..normal },
        None => normal,
    }
}

/// What a click on the number field of a tile does. The view "Serien" shows every series as one tile on purpose
/// (its tiles stay collapsed whatever the series' own state, see `collapsed`): expanding would change nothing there,
/// so the click opens the series in the comparison; elsewhere it expands or collapses the series in place.
fn number_click(series_view: bool) -> StackCmd {
    if series_view { StackCmd::Review } else { StackCmd::Toggle }
}

/// What N compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewScope {
    /// the series of the active photo as a whole (or the selection when the photo is in none)
    Series,
    /// the selected photos
    Selection,
}

/// N: one photo opens its series as a whole (as "Serie auswählen und vergleichen"), and so does a selection that is
/// exactly one whole series; any other selection of several photos is compared as it is – the series of the active
/// photo must not replace it (the button "Vergleichen" names N in its tooltip).
fn review_scope(target: &[i64], series_of: impl Fn(i64) -> Option<usize>, series_len: impl Fn(usize) -> usize) -> ReviewScope {
    match target.split_first() {
        Some((&first, rest)) if !rest.is_empty() => match series_of(first) {
            Some(k) if rest.iter().all(|&b| series_of(b) == Some(k)) && target.len() >= series_len(k) => ReviewScope::Series,
            _ => ReviewScope::Selection,
        },
        _ => ReviewScope::Series,
    }
}

/// Text of the stack number on a tile: collapsed "7" or "3 von 7" (shown of all pictures), expanded "2/3" (place among
/// the shown pictures of the stack, so the numbers run without gaps).
fn tile_number(m: &Mark) -> String {
    if m.collapsed {
        // how many: "7", filtered "3 von 7" (not "3/7", which reads as a place like the numbers of an open stack)
        if m.shown < m.total { crate::trf!("{a} von {b}", a = m.shown, b = m.total) } else { m.total.to_string() }
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
    #[test]
    fn a_click_on_the_number_in_the_series_view_opens_the_comparison() {
        // the view "Serien" keeps every tile collapsed: toggling would show nothing new there
        let st = Stack { id: 1, kind: StackKind::Series, origin: "auto".into(), cover: None, cover_by_user: false, collapsed: false, reviewed: None, reason: Default::default(), day: None, images: vec![1, 2] };
        assert!(super::collapsed(&st, true), "forced collapsed in the view \"Serien\"");
        assert!(super::number_click(true) == super::StackCmd::Review);
        assert!(super::number_click(false) == super::StackCmd::Toggle);
    }

    use super::*;
    use crate::series_job::{empty_state, Phase};

    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output.shapes.iter().filter_map(|s| if let egui::epaint::Shape::Text(t) = &s.shape { Some(t.galley.text().to_string()) } else { None }).collect()
    }

    #[test]
    fn series_row_is_there_without_series_and_says_what_happens() {
        // the detection's state is process-wide (other tests run a pass or switch it): fixed for this test
        use crate::series_job::{Phase, RowState};
        crate::series_job::fix_row_state(Some(RowState { enabled: true, phase: Phase::Idle, done: 0, total: 0, passed: false, low_power: false }));
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        assert!(library.series.stacks.is_empty());
        let out = crate::ui_test::run(&ctx, egui::RawInput::default(), |ui| library.series_row(ui));
        crate::series_job::fix_row_state(None);
        let t = texts(&out);
        assert!(t.iter().any(|s| s == "Serien"), "{t:?}");
        // the state of the detection in the row's number column (not started in this test: about to begin)
        assert!(t.iter().any(|s| s == "sucht …"), "{t:?}");
        // symbol, name and number column – no line of its own below the row
        assert_eq!(t.len(), 3, "{t:?}");
    }

    /// The number column: the detection's state while it searches or nothing is there, else "n offen".
    #[test]
    fn series_row_count_says_state_or_open() {
        assert_eq!(super::series_row_count(true, false, 0, "keine").as_deref(), Some("keine"));
        assert_eq!(super::series_row_count(false, true, 3, "sucht 19/27").as_deref(), Some("sucht 19/27"));
        assert_eq!(super::series_row_count(false, false, 3, "x").as_deref(), Some("3 offen"));
        assert_eq!(super::series_row_count(false, false, 0, "x"), None);
    }

    /// The row keeps its height when the first series is found (review 1.3.0, logic N5: everything below moved up).
    #[test]
    fn series_row_keeps_its_height() {
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        let height = |library: &mut LibraryView| {
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
        // as high as one row of the catalog (20 pt plus the spacing), no extra line
        assert!(without <= 20.0 + egui::Style::default().spacing.item_spacing.y + 0.5, "{without}");
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
        // no library behind the rows: the series of the test's shared library (other tests write theirs at the same
        // time) would replace the given stacks at the next poll
        library.db = None;
        library.stack = true;
        library.index = rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        library.rows = Arc::new(rows);
        library.series.set(stacks);
        library
    }

    /// Found in 1.3.2: the grouping puts RAW and camera JPEG of a shot into the series; the JPEG folded into its
    /// RAW took a number of its own, so the grid showed "1/4, 2/4, 4/4" (and "1/3, 3/3"). The numbers now count the
    /// shown pictures and run without gaps.
    #[test]
    fn numbers_skip_no_place_for_a_folded_camera_jpeg() {
        // series A: 1 (+JPEG), 2; series B: 3, 4 (+JPEG), 5 – in capture order, as the grouping writes them
        let rows = rows_with_pairs(&[(1, "S5399", true), (2, "S5400", false), (3, "S5396", false), (4, "S5397", true), (5, "S5398", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 101, 2], false), stack(2, vec![3, 4, 104, 5], false)]);
        // grid newest first (default): the members still stand in file order (QA 1.5.0-test.5)
        let n = numbers(&mut library, |_| true);
        let text = |b: i64| n.iter().find(|x| x.0 == b).map(|x| x.1.clone()).unwrap();
        assert_eq!((text(1).as_str(), text(2).as_str()), ("1/2", "2/2"), "{n:?}");
        assert_eq!((text(3).as_str(), text(4).as_str(), text(5).as_str()), ("1/3", "2/3", "3/3"), "{n:?}");
        // the members stand together in the order of their numbers
        let b: Vec<&str> = n.iter().filter(|x| [3, 4, 5].contains(&x.0)).map(|x| x.1.as_str()).collect();
        assert_eq!(b, ["1/3", "2/3", "3/3"]);
        // sorted by name: the same order
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

    /// Filmstrip: a double click on a collapsed series opens it; a double click on the
    /// photo that was already active closes its open series (only this one); on a photo not active before nothing.
    /// The number button acts like the grid's (open / close), and the comparison holding a series greys it.
    #[test]
    fn filmstrip_double_click_opens_and_closes_one_series() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], false)]);
        library.sort_order = SortOrder::Name;
        let all: Vec<usize> = (0..library.rows.len()).collect();
        let marks = |l: &mut LibraryView| {
            l.series_collapse(all.clone());
        };
        marks(&mut library);
        // collapsed series 1: a double click opens it, the photo stays active
        assert!(library.series.marks[&1].collapsed);
        assert_eq!(library.strip_double_click(1, false), Some(1));
        assert!(!library.series.stacks[0].collapsed);
        assert!(library.series.stacks[1].collapsed == false, "the other series stays as it was");
        marks(&mut library);
        // open series 2, photo 4 not active before the double click: nothing
        assert_eq!(library.strip_double_click(4, false), None);
        assert!(!library.series.stacks[1].collapsed);
        // photo 4 active before: its series closes, its cover becomes active; series 1 stays open
        library.active = Some(4);
        assert_eq!(library.strip_double_click(4, true), Some(3));
        assert!(library.series.stacks[1].collapsed);
        assert!(!library.series.stacks[0].collapsed);
        marks(&mut library);
        // the number button: open / close as on the grid tile, its tooltip names the double click
        let m = library.strip_mark(3).expect("number on the cover");
        assert!(m.usable && m.text == "2", "{}", m.text);
        // at most two lines: the reason of the series, then the action
        assert!(m.tip.lines().count() <= 2 && m.tip.contains("Serie aufklappen"), "{}", m.tip);
        assert!(!m.tip.contains(" Fotos"), "the number is on the button already: {}", m.tip);
        library.strip_mark_click(3);
        assert!(!library.series.stacks[1].collapsed);
        // held open by the comparison: greyed, nothing happens
        marks(&mut library);
        library.series.marks.get_mut(&3).unwrap().held = true;
        let m = library.strip_mark(3).unwrap();
        assert!(!m.usable && m.tip.contains("bleiben aufgeklappt"), "{}", m.tip);
        assert_eq!(library.strip_double_click(3, true), None);
        assert!(!library.series.stacks[1].collapsed);
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
        assert_eq!(numbers(&mut library, |z| z.image != 2), [(1, "3 von 4".to_string())]);
    }

    /// Found in 1.4.2: tiles said "1/1" and a click "expanded" the series to the same photo. A stack of which only
    /// one picture is shown is no stack in that view: no number, no ground, nothing to expand – expanded or collapsed,
    /// hidden by a filter (e.g. the rest rejected and hidden), by a virtual copy beside its original, or because the
    /// other members are not in the library any more. Two or more shown keep their marks ("2/4").
    #[test]
    fn a_stack_with_one_shown_photo_has_no_marks() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 2, 3, 4], false)]);
        // expanded, the rest hidden: before "1/1"
        let v: Vec<usize> = (0..library.rows.len()).filter(|&i| [1, 5].contains(&library.rows[i].image)).collect();
        let out = library.series_collapse(v);
        assert_eq!(out.len(), 2, "the photo stays in the grid");
        assert!(library.series.marks.is_empty(), "{:?}", library.series.marks);
        assert_eq!(library.series_all(), SeriesAll::None, "nothing to fold in this view");
        assert!(library.toggle_refused(1).is_some(), "the menu entry is greyed with the reason");
        // S on it does nothing (its state stays, no hidden change)
        library.series_command(StackCmd::Toggle, 1, &mut Vec::new());
        assert!(!library.series.stacks[0].collapsed);
        // collapsed, one shown: no "1/4" either
        library.series.stacks[0].collapsed = true;
        assert!(numbers(&mut library, |z| z.image == 2).is_empty());
        // two shown: the marks stay
        assert_eq!(numbers(&mut library, |z| z.image == 2 || z.image == 3), [(2, "2 von 4".to_string())]);
        library.series.stacks[0].collapsed = false;
        assert_eq!(numbers(&mut library, |z| z.image == 2 || z.image == 3).len(), 2);
        assert!(library.toggle_refused(2).is_none());
    }

    /// An original with its virtual copy is one picture: no stack marks when the rest is hidden.
    #[test]
    fn original_and_virtual_copy_alone_have_no_marks() {
        let mut rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false)]);
        rows.push(Row { image: 50, file: 1, folder: 1, name: "A.DNG".into(), format: "DNG".into(), copy: Some("Kopie 1".into()), capture: rows[0].capture.clone(), ..Row::default() });
        let mut library = library_with(rows, vec![stack(1, vec![1, 2, 3], false)]);
        assert!(numbers(&mut library, |z| z.file == 1).is_empty());
        // with a second picture both tiles of the original are numbered
        assert_eq!(numbers(&mut library, |z| z.file == 1 || z.file == 2).len(), 3);
    }

    /// A stack whose other members are not in the library (hidden for good, not loaded) shows no marks, not even in
    /// the view "Serien", which keeps a stack of two or more pictures even when a filter shows only one ("1/4").
    #[test]
    fn a_stack_of_one_picture_is_none_also_in_the_series_view() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        // stack 2: 5 and an image the library does not have
        let mut library = library_with(rows, vec![stack(1, vec![1, 2, 3, 4], false), stack(2, vec![5, 99], false)]);
        assert_eq!(library.stack_pictures(&library.series.stacks[1]), 1);
        assert_eq!(numbers(&mut library, |_| true).iter().filter(|x| x.0 == 5).count(), 0);
        library.source = Source::Series;
        let n = numbers(&mut library, |z| z.image == 3 || z.image == 5);
        assert_eq!(n, [(3, "1 von 4".to_string())], "the series view: one tile per series, its click compares all four");
    }

    /// Found in 1.5.0-test.5: comparing a collapsed bracket of 5 showed one photo in the filmstrip, the others
    /// could not be reached. While comparing, every stack of a compared photo stands open with all its members (also
    /// in the view "Serien" and with a filter that hides members); closing brings back its own state. The button
    /// "all series" is never silent: with only held stacks shown it is greyed with the reason.
    #[test]
    fn compared_series_stand_open_in_the_filmstrip() {
        let mut rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false), (6, "F", false), (7, "G", false)]);
        for z in rows.iter_mut() {
            z.flag = if z.image <= 2 { 1 } else { -1 };
        }
        let mut library = library_with(rows, vec![stack(1, vec![1, 2, 3, 4, 5], true), stack(2, vec![6, 7], true)]);
        library.filter.flags = FLAG_PICKED;
        library.source = Source::All;
        let visible = |library: &mut LibraryView| {
            library.refilter = true;
            library.apply_filter();
            library.visible.iter().map(|&i| library.rows[i].image).collect::<Vec<_>>()
        };
        // before: the collapsed series is one tile (the rest is rejected and hidden as well)
        assert_eq!(visible(&mut library), [1]);
        // compare 1 and 3 of it: all five stand open, numbered 1/5 … 5/5
        library.selection = [1, 3].into_iter().collect();
        library.active = Some(1);
        library.nothing_selected = false;
        library.compare_start(None, None);
        let mut v = visible(&mut library);
        v.sort();
        assert_eq!(v, [1, 2, 3, 4, 5]);
        assert!(library.series.marks.values().all(|m| !m.collapsed && m.held && m.shown == 5));
        assert!(library.series.stacks[0].collapsed, "held open in memory only: the stack's own state stays");
        assert_eq!(library.series_all(), SeriesAll::Held);
        let (_, tip, usable) = SeriesAll::Held.look();
        assert!(!usable && !tip.is_empty());
        // a photo of the other series taken in: that one opens too (its members are rejected, still shown)
        library.compare_hold(&[6]);
        assert_eq!(visible(&mut library).len(), 7);
        // the view "Serien" keeps the compared series open as well
        library.source = Source::Series;
        assert_eq!(visible(&mut library).len(), 7);
        // a source holding only part of the series (a series across two folders, the filmstrip showed
        // one of eight compared photos): the members from outside stand beside it, not counted as photos of the source
        library.source = Source::Recent;
        library.last_run = Some(1);
        let mut rows = (*library.rows).clone();
        for z in rows.iter_mut() {
            z.run = (z.image == 1).then_some(1);
        }
        library.rows = Arc::new(rows);
        assert_eq!(visible(&mut library).len(), 7);
        assert_eq!(library.source_total, 1);
        library.source = Source::All;
        // closed: back to one tile
        library.compare_close();
        assert_eq!(visible(&mut library), [1]);
        assert!(library.series.held.is_empty());
    }

    /// The button "all series": its state follows the shown stacks (mixed: expand), a click toggles all, the view
    /// "Serien" and a view without stacks grey it out.
    #[test]
    fn all_series_button_state_and_toggle() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 2], false), stack(2, vec![3, 4], true)]);
        let shown = |library: &mut LibraryView| {
            numbers(library, |_| true);
            library.series_all()
        };
        // mixed: one open, one collapsed → a click expands all
        assert_eq!(shown(&mut library), SeriesAll::Expand);
        library.series_all_click();
        assert!(library.series.stacks.iter().all(|s| !s.collapsed));
        assert_eq!(shown(&mut library), SeriesAll::Collapse);
        // all open → a click collapses all; ⇧S does the same
        library.series_all_click();
        assert!(library.series.stacks.iter().all(|s| s.collapsed));
        assert_eq!(shown(&mut library), SeriesAll::Expand);
        library.series_toggle_all();
        assert!(library.series.stacks.iter().all(|s| !s.collapsed));
        // only photos outside any stack shown: greyed, a click changes nothing
        numbers(&mut library, |z| z.image == 5);
        assert_eq!(library.series_all(), SeriesAll::None);
        library.series_all_click();
        assert!(library.series.stacks.iter().all(|s| !s.collapsed));
        // the view "Serien": greyed with its own reason
        library.source = Source::Series;
        assert_eq!(library.series_all(), SeriesAll::SeriesView);
        library.series_all_click();
        assert!(library.series.stacks.iter().all(|s| !s.collapsed));
        // greyed states say why, usable ones carry the shortcut
        for (st, usable) in [(SeriesAll::None, false), (SeriesAll::SeriesView, false), (SeriesAll::Expand, true), (SeriesAll::Collapse, true)] {
            let (_, tip, u) = st.look();
            assert_eq!(u, usable);
            assert!(!tip.is_empty());
            assert_eq!(tip.contains(&crate::shortcuts::display("stack_toggle_all")), usable, "{tip}");
        }
        assert_ne!(SeriesAll::Expand.look().0, SeriesAll::Collapse.look().0);
    }

    /// QA test.5 repeat: ⇧S works wherever the list is shown (it comes from main.rs, not from drawing the grid) and
    /// never silently: where it does nothing the status line says why; "Vergleichen" with one photo of a series
    /// active compares the series (as N).
    #[test]
    fn shift_s_says_why_and_the_compare_button_takes_the_series() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false)]);
        let mut library = library_with(rows, vec![stack(7, vec![1, 2], true)]);
        numbers(&mut library, |_| true);
        assert!(library.stack_menu_command(StackMenu::ToggleAll).is_empty());
        assert!(!library.series.stacks[0].collapsed, "opened");
        library.source = Source::Series;
        let a = library.stack_menu_command(StackMenu::ToggleAll);
        assert!(matches!(a.as_slice(), [Action::Status(t)] if !t.is_empty()), "a reason, not silence");
        library.source = Source::All;
        library.active = Some(1);
        library.nothing_selected = false;
        assert_eq!(library.button_series(), Some(7));
        library.active = Some(3);
        assert_eq!(library.button_series(), None, "a photo outside any series");
    }

    /// QA run 3: when a collapsed series comes to cover the active photo (regrouping, the view "Serien") or the active
    /// photo leaves the library (⌘Z of an HDR), the active one becomes the tile of its own series or one of the photos
    /// it was stacked with – never the photo at its old place, which may belong to another series.
    #[test]
    fn the_active_photo_stays_in_its_own_series() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false), (6, "F", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 2, 3], false), stack(2, vec![4, 5, 6], false)]);
        library.source = Source::All;
        library.sort_order = SortOrder::Name;
        let filter = |library: &mut LibraryView| {
            library.refilter = true;
            library.apply_filter();
        };
        library.active = Some(6);
        library.nothing_selected = false;
        filter(&mut library);
        // the series of 6 collapses (a regrouping, or another session): its tile, not the photo now at its place
        library.series.stacks[1].collapsed = true;
        filter(&mut library);
        assert_eq!(library.active, Some(4), "the cover of its own series");
        // the active photo leaves the library (⌘Z of a merged photo): one of the photos it was stacked with
        library.series.stacks[1].collapsed = false;
        library.active = Some(5);
        filter(&mut library);
        let rows: Vec<Row> = library.rows.iter().filter(|z| z.image != 5).cloned().collect();
        library.index = rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        library.rows = Arc::new(rows);
        library.series.set(vec![stack(1, vec![1, 2, 3], false), stack(2, vec![4, 6], false)]);
        filter(&mut library);
        assert!(matches!(library.active, Some(4 | 6)), "{:?}", library.active);
    }

    /// ←/→ pass a collapsed series as one tile; ⌥→ goes into it (first member, then through the members), ⌥ out of it
    /// closes it again (in memory only); ⌥← enters at the last member.
    #[test]
    fn alt_arrows_go_into_a_collapsed_series_and_close_it_behind() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![2, 3, 4], true)]);
        library.source = Source::All;
        library.sort_order = SortOrder::Name;
        let filter = |l: &mut LibraryView| {
            l.refilter = true;
            l.apply_filter();
        };
        let shown = |l: &LibraryView| l.visible.iter().map(|&i| l.rows[i].image).collect::<Vec<_>>();
        library.active = Some(1);
        library.nothing_selected = false;
        filter(&mut library);
        assert_eq!(shown(&library), [1, 2, 5], "collapsed: one tile");
        // plain →: over the series as one tile
        library.step_external(1);
        assert_eq!(library.active, Some(2));
        library.step_external(1);
        assert_eq!(library.active, Some(5));
        // ⌥← from 5: into the series at its last member
        assert!(library.series_step(false));
        assert!(library.list_pending());
        filter(&mut library);
        assert_eq!(shown(&library), [1, 2, 3, 4, 5]);
        assert_eq!(library.active, Some(4));
        assert!(!library.series.stacks[0].collapsed);
        assert!(library.take_strip_keep(), "the strip keeps the series in place");
        // ⌥← through the members, then out: the series closes again
        assert!(library.series_step(false));
        assert_eq!(library.active, Some(3));
        library.series_step(false);
        library.series_step(false);
        assert_eq!(library.active, Some(1));
        assert!(library.series.stacks[0].collapsed, "left: closed again");
        filter(&mut library);
        assert_eq!(shown(&library), [1, 2, 5]);
        // ⌥→ on the collapsed tile itself: the cover (first member) is skipped, the second one becomes active
        library.step_external(1);
        assert_eq!(library.active, Some(2));
        library.series_step(true);
        filter(&mut library);
        assert_eq!(library.active, Some(3));
        // a plain arrow out of it closes it as well
        library.step_external(1);
        library.step_external(1);
        assert_eq!(library.active, Some(5));
        library.series_poll();
        assert!(library.series.stacks[0].collapsed);
        // an ordinary S on a series opened by ⌥ makes it an ordinary stack (no closing behind)
        filter(&mut library);
        library.active = Some(1);
        library.series_step(true);
        filter(&mut library);
        assert_eq!(library.active, Some(2));
        library.series_command(StackCmd::Toggle, 2, &mut Vec::new());
        assert!(library.series.peek.is_none());
    }

    /// ↩ on a collapsed series opens it (the cover stays active); elsewhere ↩ keeps its meaning (`false`).
    #[test]
    fn return_opens_a_collapsed_series_only() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![2, 3], true)]);
        library.source = Source::All;
        library.sort_order = SortOrder::Name;
        library.active = Some(1);
        library.nothing_selected = false;
        library.refilter = true;
        library.apply_filter();
        assert!(!library.enter_expand(), "no series: ↩ opens the photo");
        library.active = Some(2);
        assert!(library.enter_expand());
        assert!(!library.series.stacks[0].collapsed);
        assert_eq!(library.active, Some(2));
        library.refilter = true;
        library.apply_filter();
        assert!(!library.enter_expand(), "open series: ↩ opens the photo");
    }

    /// QA 5: ⌥→/← on the collapsed tile whose cover stands in the middle of the series (cover _02) go on from the
    /// cover's own place in file order: → to _03, ← to _01.
    #[test]
    fn alt_arrows_from_a_cover_in_the_middle_go_from_its_place() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut s = stack(1, vec![2, 3, 4], true);
        s.cover = Some(3);
        let mut library = library_with(rows, vec![s]);
        library.source = Source::All;
        library.sort_order = SortOrder::Name;
        let filter = |l: &mut LibraryView| {
            l.refilter = true;
            l.apply_filter();
        };
        library.active = Some(3);
        library.nothing_selected = false;
        filter(&mut library);
        assert!(library.series_step(true));
        filter(&mut library);
        assert_eq!(library.active, Some(4));
        // back to the cover's tile, then ⌥←
        library.active = Some(5);
        library.series_poll();
        filter(&mut library);
        assert!(library.series.stacks[0].collapsed);
        library.active = Some(3);
        assert!(library.series_step(false));
        filter(&mut library);
        assert_eq!(library.active, Some(2));
        // the cover as the last member: ⌥→ leaves the series to the next tile
        library.active = Some(1);
        library.series_poll();
        filter(&mut library);
        library.series.stacks[0].cover = Some(4);
        filter(&mut library);
        library.active = Some(4);
        assert!(library.series_step(true));
        filter(&mut library);
        assert_eq!(library.active, Some(5));
    }

    /// QA 5: reading the stacks again keeps what is only in memory – a series opened by ⌥ – and an open / closed state
    /// written in the background that the load may have read before (race with the writer); a load started after the
    /// write finished counts as it is.
    #[test]
    fn a_reload_keeps_states_not_yet_stored() {
        let mut v = SeriesView::default();
        v.set(vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], true)]);
        // opened by a click: written in the background
        v.stacks[0].collapsed = false;
        let token = v.write_started(&[1], false);
        // a load started before the write finished still says "collapsed"
        v.loads = 1;
        v.adopt(vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], true)], 1);
        assert!(!v.stacks[0].collapsed, "the click stays");
        v.write_done(token, true);
        // the same old load arriving late: still kept
        v.adopt(vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], true)], 1);
        assert!(!v.stacks[0].collapsed);
        // a load started after the write: the database counts (here someone closed it again meanwhile)
        v.adopt(vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], true)], 2);
        assert!(v.stacks[0].collapsed);
        assert!(v.pending.is_empty());
        // a second write replaces the first: the first one finishing does not release it
        let a = v.write_started(&[2], false);
        let b = v.write_started(&[2], true);
        v.write_done(a, true);
        v.adopt(vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], false)], 5);
        assert!(v.stacks[1].collapsed, "the newer write stays");
        v.write_done(b, true);
        // a series opened by ⌥ only in memory stays open, its stored state is collapsed
        v.peek = Some(2);
        v.adopt(vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], true)], 9);
        assert!(!v.stacks[1].collapsed);
        assert!(v.stored_collapsed(1), "stored: collapsed (an HDR made from it must not store it open)");
    }

    /// QA 5: "Vergleichen" in the menu of a collapsed series compares the series (as N), and the keys S / N are named
    /// only on the series of the active photo (they act on that one).
    #[test]
    fn menu_compare_takes_the_series_and_keys_only_on_the_active_one() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![1, 2], true), stack(2, vec![3, 4], true)]);
        library.source = Source::All;
        library.sort_order = SortOrder::Name;
        library.active = Some(1);
        library.nothing_selected = false;
        library.refilter = true;
        library.apply_filter();
        assert_eq!(library.compare_count(3), 2, "the collapsed series counts as a whole");
        assert_eq!(library.compare_count(5), 1);
        let own = library.strip_mark(1).unwrap();
        let other = library.strip_mark(3).unwrap();
        let s = crate::shortcuts::display("stack_toggle");
        assert!(own.tip.contains(&format!("({s})")), "{}", own.tip);
        assert!(!other.tip.contains(&format!("({s})")), "{}", other.tip);
        // the menu compares the series of the photo it was opened on
        library.active = Some(3);
        assert_eq!(library.button_series(), Some(2));
    }

    /// The number's tooltip says only what the button does not – at most two lines, never
    /// the number again; an own stack has no reason line (the filmstrip names the double click instead).
    #[test]
    fn number_tooltips_are_short() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false)]);
        let mut bracket = stack(1, vec![1, 2], true);
        bracket.kind = StackKind::Bracket;
        bracket.reason.ev_range = Some(2.0);
        let mut own = stack(2, vec![3, 4], true);
        own.kind = StackKind::Manual;
        own.origin = "user".into();
        let mut library = library_with(rows, vec![bracket, own]);
        library.sort_order = SortOrder::Name;
        library.active = Some(1);
        numbers(&mut library, |_| true);
        let b = library.strip_mark(1).unwrap().tip;
        assert_eq!(b.lines().count(), 2, "{b}");
        assert!(b.starts_with("Belichtungsreihe über 2") && b.contains("Reihe aufklappen"), "{b}");
        let o = library.strip_mark(3).unwrap().tip;
        assert_eq!(o.lines().next(), Some("Stapel aufklappen"), "{o}");
        assert!(o.contains("Doppelklick") && o.lines().count() == 2, "{o}");
        assert!(!b.contains("Fotos") && !o.contains("Fotos"), "{b} / {o}");
    }

    /// QA 5: the number of another series opens and closes it without touching the active photo; closing makes the
    /// cover active only when the active photo was in that series.
    #[test]
    fn the_number_of_another_series_leaves_the_active_photo() {
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false), (4, "D", false), (5, "E", false)]);
        let mut library = library_with(rows, vec![stack(1, vec![2, 3, 4], true)]);
        library.source = Source::All;
        library.sort_order = SortOrder::Name;
        let filter = |l: &mut LibraryView| {
            l.refilter = true;
            l.apply_filter();
        };
        library.active = Some(5);
        library.nothing_selected = false;
        filter(&mut library);
        library.strip_mark_click(2);
        assert!(!library.series.stacks[0].collapsed, "opened");
        assert_eq!(library.active, Some(5));
        filter(&mut library);
        library.strip_mark_click(3);
        assert!(library.series.stacks[0].collapsed, "closed");
        assert_eq!(library.active, Some(5), "not the cover of the other series");
        // the active photo in the series: closing makes its cover active
        filter(&mut library);
        library.strip_mark_click(2);
        filter(&mut library);
        library.active = Some(4);
        library.strip_mark_click(4);
        assert!(library.series.stacks[0].collapsed);
        assert_eq!(library.active, Some(2));
    }

    /// Texts named after the kind: bracket "Reihe", series "Serie", own stack "Stapel" – tooltip and menu.
    #[test]
    fn texts_name_the_kind() {
        assert_eq!(toggle_text(StackKind::Bracket, true), "Reihe aufklappen");
        assert_eq!(toggle_text(StackKind::Bracket, false), "Reihe zuklappen");
        assert_eq!(toggle_text(StackKind::Series, true), "Serie aufklappen");
        assert_eq!(toggle_text(StackKind::Manual, false), "Stapel zuklappen");
        assert_eq!(kind_texts(StackKind::Bracket).dissolve, "Reihe auflösen");
        let rows = rows_with_pairs(&[(1, "A", false), (2, "B", false), (3, "C", false)]);
        let mut bracket = stack(1, vec![1, 2, 3], true);
        bracket.kind = StackKind::Bracket;
        bracket.reason.ev_range = Some(4.0);
        let mut library = library_with(rows, vec![bracket]);
        library.series_collapse((0..3).collect());
        let m = library.strip_mark(1).unwrap();
        assert!(m.tip.contains("Reihe aufklappen"), "{}", m.tip);
        // the number stands on the button only, the EV with the language's decimal sign
        assert_eq!(m.tip.matches('3').count(), 0, "{}", m.tip);
        assert!(m.tip.contains("4,0 EV") || m.tip.contains("4.0 EV"), "{}", m.tip);
    }

    /// The button keeps its size in every state (no layout jump in the grid row or the filmstrip header).
    #[test]
    fn all_series_button_keeps_its_place() {
        let ctx = egui::Context::default();
        for st in [SeriesAll::None, SeriesAll::SeriesView, SeriesAll::Expand, SeriesAll::Collapse, SeriesAll::Held] {
            let mut size = egui::Vec2::ZERO;
            crate::ui_test::run(&ctx, egui::RawInput::default(), |ui| {
                let top = ui.cursor().min;
                series_all_button(ui, st, 26.0, 20.0);
                size = ui.min_rect().max - top;
            });
            assert_eq!(size, egui::vec2(26.0, 20.0), "{st:?}");
        }
    }

    /// QA matrix: N with a selection across series, part of a series or with photos outside any series compared the
    /// whole series of the active photo and dropped the selection, unlike the button "Vergleichen" whose tooltip
    /// names N.
    #[test]
    fn n_compares_a_selection_across_series_as_it_is() {
        let of = |b: i64| match b {
            1..=3 => Some(0),
            4..=6 => Some(1),
            _ => None,
        };
        let len = |_| 3;
        assert_eq!(review_scope(&[2], of, len), ReviewScope::Series);
        assert_eq!(review_scope(&[9], of, len), ReviewScope::Series);
        assert_eq!(review_scope(&[], of, len), ReviewScope::Series);
        // the whole series selected: still the series (with "Weiter")
        assert_eq!(review_scope(&[1, 2, 3], of, len), ReviewScope::Series);
        // two photos of a series of three: just these two
        assert_eq!(review_scope(&[1, 2], of, len), ReviewScope::Selection);
        assert_eq!(review_scope(&[1, 4], of, len), ReviewScope::Selection);
        assert_eq!(review_scope(&[1, 9], of, len), ReviewScope::Selection);
        assert_eq!(review_scope(&[8, 9], of, len), ReviewScope::Selection);
    }

    #[test]
    fn members_stand_in_file_order_merged_first() {
        let m = |file, order| Member { file, stack: 0, order };
        assert_eq!(shown_order(vec![m(30, 2), m(10, 0), m(20, 1), m(30, 2)]), [10, 20, 30]);
        let v = vec![(false, "B_03.CR2".to_string(), 3), (false, "b_01.CR2".to_string(), 1), (true, "B_02-HDR.dng".to_string(), 9), (false, "B_02.CR2".to_string(), 2)];
        assert_eq!(file_order(v), [9, 1, 2, 3]);
    }

    /// Members of the open stack of the active photo are marked (bright bracket, taller end marks, bright number),
    /// the numbers of other stacks are dimmed; without an open stack of the active photo everything stays as before.
    #[test]
    fn open_stack_of_the_active_photo_is_marked() {
        let mark = |stack, collapsed| Mark { stack, pos: 1, total: 3, shown: 3, collapsed, first: true, last: true, alt: false, held: false };
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
        // a selection of three photos asks the app for the comparison (photo_compare_ui.rs)
        library.selection = ids.iter().copied().collect();
        let a = library.stack_menu_command(StackMenu::Review);
        assert!(status(&a).is_none(), "{:?}", status(&a));
        assert_eq!(library.take_compare_request(), Some(super::CompareRequest::default()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
