//! Photo comparison: a view of the centre in the library and in Develop (instead of the grid or the single image).
//! The photos come from the selection in the normal filmstrip; ⌘-click there puts a photo in or takes it out. Up to
//! four fields side by side or 2 × 2, or up to 8 small fields with the arrangement "Alle" (it replaced the former
//! light table), coupled zoom and pan, paging through more photos than fields, and a
//! tournament (knock-out with a short runoff for places 2 and 3).
//!
//! This module holds the state and the pure rules (which photos get a field, where a photo lies in its field, the
//! tournament); `photo_compare_ui.rs` draws it and `library_ui/library_compare.rs` measures the hints (sharpness,
//! eyes, motion, clipping) and carries out decisions on series.
//!
//! The comparisons of variants of one photo (Film & Look, "Default development | Current", darkroom A | B, the
//! Apple RAW comparison) are separate and stay as they are.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use eframe::egui::{self, Rect, Vec2};
use library::series_quality::{Face, Quality, Suggestion};

/// At most this many fields in the arrangements with large fields.
pub const MAX_FIELDS: usize = 4;
/// At most this many fields per page with the arrangement "Alle" (fewer when they would get smaller than
/// [`MIN_ALL_FIELD`]); more photos are paged.
pub const MAX_ALL: usize = 8;
/// Smallest field of "Alle" in points (photo plus foot).
pub const MIN_ALL_FIELD: f32 = 160.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 2–4 photos large, coupled zoom
    Compare,
    /// two photos at a time, knock-out to places 1–3
    Tournament,
}

/// Arrangement of the fields (remembered for the session; zooming never changes it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    /// equal fields in the grid that shows the photos largest (a short last row centred)
    Auto,
    Row,
    Column,
    Square,
    /// the focused photo large, the others small beside or below it
    FocusLarge,
    /// up to [`MAX_ALL`] small fields (an overview of the selection; zoom and the inspection aids act as always)
    All,
}

impl Arrange {
    pub fn key(self) -> &'static str {
        match self {
            Arrange::Auto => "auto",
            Arrange::Row => "row",
            Arrange::Column => "column",
            Arrange::Square => "square",
            Arrange::FocusLarge => "focus",
            Arrange::All => "all",
        }
    }
    /// The arrangement chosen last (setting "compareArrange").
    pub fn remembered() -> Arrange {
        let v = library::settings::value("compareArrange");
        [Arrange::Row, Arrange::Column, Arrange::Square, Arrange::FocusLarge, Arrange::All].into_iter().find(|a| v.as_str() == Some(a.key())).unwrap_or(Arrange::Auto)
    }
    pub fn next(self) -> Arrange {
        match self {
            Arrange::Auto => Arrange::Row,
            Arrange::Row => Arrange::Column,
            Arrange::Column => Arrange::Square,
            Arrange::Square => Arrange::FocusLarge,
            Arrange::FocusLarge => Arrange::All,
            Arrange::All => Arrange::Auto,
        }
    }
}

/// How a photo sits in its field at the zoom "fit" (remembered, setting "compareFit").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// the photo fills its field (cropped to the field's shape): no empty space between the fields
    Fill,
    /// the whole photo in its field, with borders
    Whole,
}

impl Fit {
    pub fn key(self) -> &'static str {
        match self {
            Fit::Fill => "fill",
            Fit::Whole => "whole",
        }
    }
    /// The way chosen last (setting "compareFit"; default: fill).
    pub fn remembered() -> Fit {
        if library::settings::value("compareFit").as_str() == Some(Fit::Whole.key()) { Fit::Whole } else { Fit::Fill }
    }
    pub fn toggled(self) -> Fit {
        match self {
            Fit::Fill => Fit::Whole,
            Fit::Whole => Fit::Fill,
        }
    }
    pub fn fill(self) -> bool {
        self == Fit::Fill
    }
}

/// How the candidate is shown against the reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefView {
    SideBySide,
    Blink,
    Wipe,
    Difference,
}

/// Inspection aids; each acts in all fields at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Checks {
    pub focus_point: bool,
    pub peaking: bool,
    pub clipping: bool,
    pub exif: bool,
    pub measure: bool,
}

/// Measured hints of one photo.
#[derive(Clone, Default)]
pub struct Hint {
    pub quality: Option<Quality>,
    pub faces: Option<Vec<Face>>,
    pub signature: Option<Vec<u8>>,
    /// focus point of the camera (centre of the main area, 0..1 of the upright image)
    pub focus: Option<[f32; 2]>,
    /// focus area of the camera (x, y, w, h, 0..1 of the upright image)
    pub focus_area: Option<[f32; 4]>,
}

impl Hint {
    /// The camera's focus point lies beside every face found (only a hint, no rating).
    pub fn focus_beside_faces(&self) -> bool {
        let (Some([x, y]), Some(faces)) = (self.focus, &self.faces) else { return false };
        !faces.is_empty() && !faces.iter().any(|f| x >= f.x - f.w * 0.25 && x <= f.x + f.w * 1.25 && y >= f.y - f.h * 0.25 && y <= f.y + f.h * 1.25)
    }

    /// Where "100 %" goes: the camera's focus point, else the sharpest (largest, best) face, else the sharpest region.
    pub fn zoom_target(&self) -> Option<egui::Pos2> {
        if let Some([x, y]) = self.focus {
            return Some(egui::pos2(x, y));
        }
        if let Some(f) = self.faces.as_ref().and_then(|f| f.iter().max_by(|a, b| (a.w * a.quality.unwrap_or(0.5)).total_cmp(&(b.w * b.quality.unwrap_or(0.5))))) {
            return Some(egui::pos2(f.x + f.w / 2.0, f.y + f.h * 0.4));
        }
        self.quality.map(|q| egui::pos2(q.sharp_x, q.sharp_y))
    }
}

/// A command from a button, carried out after drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// pick flag and cover of the series for this photo
    Best(i64),
    /// ⇧X: the rest of the series rejected (or lowered, setting "stackRest")
    Rest,
    /// ⌘↩: series reviewed, on to the next one
    Finish,
    /// back to the grid or the single image
    Close,
    /// focus this photo (Develop: open it)
    Focus(i64),
    /// open this photo in Develop
    Develop(i64),
    /// take the photo out of the comparison (and out of the selection)
    Remove(i64),
    /// show the photo in the grid
    ShowInGrid(i64),
    /// stars of this photo (0 clears)
    Stars(i64, u8),
    /// flag of this photo (1 pick, −1 reject, 0 none)
    Flag(i64, i8),
    /// tournament: take over the result (one ⌘Z)
    Adopt,
}

/// The comparison.
pub struct Compare {
    /// the series being reviewed (`None`: a free selection)
    pub stack: Option<i64>,
    /// the selected photos in capture order (refreshed every frame from the filmstrip selection)
    pub images: Vec<i64>,
    /// photos with a field, in the order of `images` (the reference first)
    pub fields: Vec<i64>,
    /// the fields were set by hand (a photo swapped into a field): they stay; otherwise they are a window of
    /// `images` that follows its order (paging, a photo taken in)
    pub swapped: bool,
    /// the focused photo
    pub focus: Option<i64>,
    pub reference: Option<i64>,
    pub mode: Mode,
    pub arrange: Arrange,
    /// the photo fills its field or is shown whole
    pub fit: Fit,
    /// zoom and pan act on all fields
    pub coupled: bool,
    /// individual shift of a field (⇧-drag, or every drag when not coupled), in 0..1 of its photo
    pub offsets: HashMap<i64, Vec2>,
    pub hints: HashMap<i64, Hint>,
    pub hints_rx: Option<Receiver<(i64, Hint)>>,
    /// files measured or being measured
    pub measuring: Vec<i64>,
    pub suggestion: Suggestion,
    /// the suggestion's index refers to these images
    pub suggestion_for: Vec<i64>,
    pub faces_bar: bool,
    pub checks: Checks,
    pub ref_view: RefView,
    /// position of the wipe divider (0..1 of the field width)
    pub wipe: f32,
    /// blink interval in seconds
    pub blink: f32,
    /// measuring rectangle in 0..1 of the photos (follows the coupling)
    pub measure_rect: Option<Rect>,
    pub tournament: Option<Tournament>,
    pub pending: Vec<Cmd>,
    /// fields of "Alle" that fit the area (set by the view every frame, at most [`MAX_ALL`])
    pub all_max: usize,
    /// the tournament's ranking is shown: the fields in "Alle" in the order of the
    /// places with a place mark; the arrangement before it comes back afterwards
    pub ranking_before: Option<Arrange>,
}

impl Compare {
    pub fn new(stack: Option<i64>, reference: Option<i64>) -> Compare {
        Compare {
            stack,
            images: Vec::new(),
            fields: Vec::new(),
            swapped: false,
            focus: reference,
            reference,
            mode: Mode::Compare,
            arrange: Arrange::remembered(),
            fit: Fit::remembered(),
            coupled: true,
            offsets: HashMap::new(),
            hints: HashMap::new(),
            hints_rx: None,
            measuring: Vec::new(),
            suggestion: Suggestion::default(),
            suggestion_for: Vec::new(),
            faces_bar: true,
            checks: Checks::default(),
            ref_view: RefView::SideBySide,
            wipe: 0.5,
            blink: 0.6,
            measure_rect: None,
            tournament: None,
            pending: Vec::new(),
            all_max: MAX_ALL,
            ranking_before: None,
        }
    }

    /// The tournament's ranking is shown (its last duel decided, nothing set yet).
    pub fn ranking_shown(&self) -> bool {
        self.ranking_before.is_some() && self.tournament.as_ref().is_some_and(|t| t.done)
    }

    /// The last duel was decided: the ranking appears as fields of "Alle" in the order of the places, place 1 focused.
    /// Called every frame before [`Self::follow`]; `true` in the frame the ranking appears.
    pub fn show_ranking_if_done(&mut self) -> bool {
        // the tournament went away by another way (cancelled, a new selection): the arrangement of before comes back
        if self.ranking_before.is_some() && !self.tournament.as_ref().is_some_and(|t| t.done) && self.mode == Mode::Compare {
            self.leave_ranking();
        }
        if self.mode != Mode::Tournament || !self.tournament.as_ref().is_some_and(|t| t.done) {
            return false;
        }
        self.ranking_before = Some(self.arrange);
        self.arrange = Arrange::All;
        self.mode = Mode::Compare;
        self.reference = None;
        self.fields.clear();
        self.swapped = false;
        self.focus = self.tournament.as_ref().and_then(|t| t.places()[0]);
        true
    }

    /// The ranking ends ("Übernehmen", "Verwerfen", another tournament): the arrangement of before comes back.
    pub fn leave_ranking(&mut self) {
        if let Some(a) = self.ranking_before.take() {
            self.arrange = a;
            self.fields.clear();
            self.swapped = false;
        }
    }

    /// ⌘Z / "Letztes Duell" in the tournament or on its ranking: the last duel back (the ranking gives way to it).
    pub fn undo_duel(&mut self) -> bool {
        let ranking = self.ranking_shown();
        if !self.tournament.as_mut().is_some_and(|t| t.undo()) {
            return false;
        }
        if ranking {
            self.leave_ranking();
            self.mode = Mode::Tournament;
        }
        true
    }

    /// Place (1–3) of a photo in the ranking shown.
    pub fn rank_of(&self, image: i64) -> Option<usize> {
        if !self.ranking_shown() {
            return None;
        }
        self.tournament.as_ref()?.places().iter().position(|p| *p == Some(image)).map(|k| k + 1)
    }

    /// How many fields a page has: up to [`MAX_ALL`] with "Alle" (as many as fit), otherwise [`MAX_FIELDS`].
    pub fn capacity(&self) -> usize {
        if self.arrange == Arrange::All && self.mode == Mode::Compare { self.all_max.clamp(1, MAX_ALL) } else { MAX_FIELDS }
    }

    /// Follow the selection: `selection` in capture order, `active` the photo active in the filmstrip.
    pub fn follow(&mut self, selection: Vec<i64>, active: Option<i64>) {
        // the reference stays even when the filmstrip shows another folder
        let mut images = selection;
        // the ranking: places first, then the others in their order (the fields and paging follow it)
        if self.ranking_shown() {
            if let Some(order) = self.tournament.as_ref().map(|t| t.ranking()) {
                images.sort_by_key(|x| order.iter().position(|y| y == x).unwrap_or(usize::MAX));
            }
        }
        if let Some(r) = self.reference {
            if !images.contains(&r) {
                images.insert(0, r);
            }
        }
        let before = std::mem::replace(&mut self.images, images);
        // a photo taken in (⌘-click in the filmstrip): it gets the focus and the fields show the page holding it
        let added = (!before.is_empty()).then(|| self.images.iter().copied().filter(|x| !before.contains(x)).last()).flatten();
        // the same photos in another order (the strip opened a series, so members moved): the window follows the
        // order of the strip, unless fields were set by hand (QA 1.5.0-test.5: "Felder 1,4,5,6 von 6")
        let reordered = added.is_none() && before.len() == self.images.len() && before != self.images && before.iter().all(|x| self.images.contains(x));
        if let Some(a) = added.filter(|a| !self.fields.contains(a)) {
            self.focus = Some(a);
            self.swapped = false;
        }
        if self.focus.is_none_or(|f| !self.images.contains(&f)) {
            self.focus = active.filter(|a| self.images.contains(a)).or_else(|| self.images.first().copied());
        }
        let fresh = (added.is_some_and(|a| !self.fields.contains(&a))) || (reordered && !self.swapped);
        let current: &[i64] = if fresh { &[] } else { &self.fields };
        let wanted = match &self.tournament {
            Some(t) if self.mode == Mode::Tournament => t.pair().map(|(a, b)| vec![a, b]).unwrap_or_default(),
            _ => choose_fields(&self.images, current, self.focus, self.reference, self.capacity()),
        };
        self.fields = wanted;
        self.offsets.retain(|k, _| self.images.contains(k));
        if self.mode == Mode::Tournament && self.fields.is_empty() {
            // between the duels (result shown) the focus keeps its photo
        } else if self.focus.is_none_or(|f| !self.fields.contains(&f)) {
            self.focus = self.fields.first().copied();
        }
    }

    /// Put `image` into the focused field (it stays selected; the photo it replaces keeps its selection).
    pub fn swap_into_focus(&mut self, image: i64) {
        if self.fields.contains(&image) {
            self.focus = Some(image);
            return;
        }
        self.swapped = true;
        let at = self.focus.and_then(|f| self.fields.iter().position(|&x| x == f)).filter(|&k| Some(self.fields[k]) != self.reference);
        match at {
            Some(k) => self.fields[k] = image,
            None if self.fields.len() < self.capacity() => self.fields.push(image),
            None => {
                let k = self.fields.iter().rposition(|&x| Some(x) != self.reference).unwrap_or(0);
                self.fields[k] = image;
            }
        }
        // the fields keep the strip's order in this very frame (Develop draws right after the swap; the next
        // `follow` would sort them and the photos would jump a frame later – QA matrix 20.11.2026)
        let reference = self.reference;
        let images = &self.images;
        self.fields.sort_by_key(|x| if Some(*x) == reference { -1 } else { images.iter().position(|y| y == x).map_or(i64::MAX, |p| p as i64) });
        self.focus = Some(image);
    }

    /// More photos than fields: which places (1-based, in selection order) the fields show, e.g. "1–4"; `None` when
    /// every photo has a field.
    pub fn page_text(&self) -> Option<String> {
        if self.mode != Mode::Compare || self.fields.len() >= self.images.len() || self.fields.is_empty() {
            return None;
        }
        let mut at: Vec<usize> = self.fields.iter().filter_map(|f| self.images.iter().position(|x| x == f)).map(|p| p + 1).collect();
        at.sort_unstable();
        let contiguous = at.windows(2).all(|w| w[1] == w[0] + 1);
        Some(if contiguous { format!("{}–{}", at[0], at[at.len() - 1]) } else { at.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",") })
    }

    /// Is there a page in that direction (`false`: the arrow is greyed)?
    pub fn can_page(&self, step: isize) -> bool {
        self.paged(step).is_some()
    }

    /// The fields one page on (`None`: there is none in that direction).
    fn paged(&self, step: isize) -> Option<Vec<i64>> {
        if self.mode != Mode::Compare {
            return None;
        }
        let rest: Vec<i64> = self.images.iter().copied().filter(|&x| Some(x) != self.reference).collect();
        let k = self.fields.iter().filter(|&&x| Some(x) != self.reference).count().max(1);
        if rest.len() <= k {
            return None;
        }
        let first = self.fields.iter().filter(|&&x| Some(x) != self.reference).filter_map(|f| rest.iter().position(|x| x == f)).min().unwrap_or(0);
        let start = (first as isize + step * k as isize).clamp(0, (rest.len() - k) as isize) as usize;
        let mut fields: Vec<i64> = self.reference.filter(|r| self.images.contains(r)).into_iter().collect();
        fields.extend(rest[start..start + k].iter().copied());
        (fields != self.fields).then_some(fields)
    }

    /// The next (`step` 1) or previous (−1) photos of the selection into the fields, as a window in selection order
    /// (the reference keeps its field). Returns whether the fields changed.
    pub fn page(&mut self, step: isize) -> bool {
        let Some(fields) = self.paged(step) else { return false };
        self.fields = fields;
        self.swapped = false;
        self.focus = self.fields.iter().copied().find(|&x| Some(x) != self.reference).or(self.focus);
        true
    }

    /// Move the focus by `step` fields (← →).
    pub fn step_focus(&mut self, step: isize) {
        let list = &self.fields;
        if list.is_empty() {
            return;
        }
        let at = self.focus.and_then(|f| list.iter().position(|&x| x == f)).unwrap_or(0) as isize;
        let k = (at + step).clamp(0, list.len() as isize - 1) as usize;
        self.focus = Some(list[k]);
    }

    pub fn field_index(&self, image: i64) -> Option<usize> {
        self.fields.iter().position(|&x| x == image)
    }

    /// Place of a photo in the selection ("3/8").
    pub fn place(&self, image: i64) -> String {
        match self.images.iter().position(|&x| x == image) {
            Some(k) => format!("{}/{}", k + 1, self.images.len()),
            None => String::new(),
        }
    }

    pub fn toggle_reference(&mut self, image: i64) {
        if self.reference == Some(image) {
            self.reference = None;
            self.ref_view = RefView::SideBySide;
        } else {
            self.reference = Some(image);
        }
    }

    /// The candidate shown against the reference: the focused photo, else the first field that is not the reference.
    pub fn candidate(&self) -> Option<i64> {
        let r = self.reference?;
        self.focus.filter(|&f| f != r && self.images.contains(&f)).or_else(|| self.fields.iter().copied().find(|&x| x != r))
    }

    /// The suggestion as a photo (its index refers to the images it was computed for).
    pub fn suggested(&self) -> Option<i64> {
        self.suggestion.best.and_then(|k| self.suggestion_for.get(k)).copied()
    }

    pub fn sharp_of(&self, image: i64) -> Option<f32> {
        let k = self.suggestion_for.iter().position(|&x| x == image)?;
        self.suggestion.sharp.get(k).copied()
    }
}

/// Which photos get a field: those already shown and still selected keep theirs, the reference always has one
/// (first), the rest is filled from the focused photo on in selection order.
pub fn choose_fields(images: &[i64], current: &[i64], focus: Option<i64>, reference: Option<i64>, max: usize) -> Vec<i64> {
    let mut v: Vec<i64> = current.iter().copied().filter(|x| images.contains(x)).collect();
    if let Some(r) = reference.filter(|r| images.contains(r)) {
        v.retain(|&x| x != r);
        v.insert(0, r);
    }
    if let Some(f) = focus.filter(|f| images.contains(f) && !v.contains(f)) {
        if v.len() >= max {
            v.pop();
        }
        v.push(f);
    }
    let n = images.len();
    // fill as one window in selection order that holds the focus: 1–4 of 5 with the cover (3) focused, not 3, 4, 5, 1
    // (QA 1.5.0-test.5: the fields of the same series changed from run to run)
    let at = focus.and_then(|f| images.iter().position(|&x| x == f)).unwrap_or(0);
    let start = if at < max { 0 } else { at + 1 - max };
    for k in 0..n {
        if v.len() >= max.min(n) {
            break;
        }
        let img = images[(start + k) % n];
        if !v.contains(&img) {
            v.push(img);
        }
    }
    v.truncate(max);
    // order: reference first, then in selection order
    let pos = |x: &i64| if Some(*x) == reference { -1 } else { images.iter().position(|y| y == x).map_or(i64::MAX, |p| p as i64) };
    v.sort_by_key(pos);
    v
}

/// Cells of `n` fields in `cols` × `rows`, row by row. A short last row is centred with equal cells, or (`stretch`)
/// its cells share the full width, so the cells cover the area without empty space.
fn grid_cells(n: usize, area: Rect, cols: usize, rows: usize, gap: f32, stretch: bool) -> Vec<Rect> {
    let cell = Vec2::new((area.width() - gap * (cols as f32 - 1.0)) / cols as f32, (area.height() - gap * (rows as f32 - 1.0)) / rows as f32);
    (0..n)
        .map(|k| {
            let (row, col) = (k / cols, k % cols);
            let in_row = if row + 1 == rows { n - row * cols } else { cols };
            let y = row as f32 * (cell.y + gap);
            if stretch && in_row < cols {
                let w = (area.width() - gap * (in_row as f32 - 1.0)) / in_row as f32;
                return Rect::from_min_size(area.min + Vec2::new(col as f32 * (w + gap), y), Vec2::new(w, cell.y));
            }
            let shift = (cols - in_row) as f32 * (cell.x + gap) / 2.0;
            Rect::from_min_size(area.min + Vec2::new(shift + col as f32 * (cell.x + gap), y), cell)
        })
        .collect()
}

/// Area of a photo of `aspect` in a cell of `size` with a foot of `foot` below it.
fn photo_area(size: Vec2, aspect: f32, foot: f32) -> f32 {
    let f = fit_size(Vec2::new(size.x, (size.y - foot).max(1.0)), aspect);
    f.x * f.y
}

/// Columns and rows of the equal grid that shows `n` photos of `aspect` largest in `area` (each cell with a foot of
/// `foot`); fewer rows win a tie.
pub fn best_grid(n: usize, area: Vec2, aspect: f32, foot: f32, gap: f32) -> (usize, usize) {
    let n = n.max(1);
    let mut best = (n, 1, 0.0f32);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        // no empty rows (e.g. 3 photos in 2 × 3)
        if rows > 1 && (rows - 1) * cols >= n {
            continue;
        }
        let cell = Vec2::new((area.x - gap * (cols as f32 - 1.0)) / cols as f32, (area.y - gap * (rows as f32 - 1.0)) / rows as f32);
        let a = photo_area(cell, aspect, foot);
        if a > best.2 * 1.01 || (a >= best.2 * 0.99 && rows < best.1 && a > 0.0) {
            best = (cols, rows, a);
        }
    }
    (best.0, best.1)
}

/// Share of a photo of `aspect` (w / h) that a field of `size` cuts away when the photo covers it ("fill"):
/// 0 = nothing cut, 0.75 = only a quarter of the photo visible.
pub fn crop_loss(size: Vec2, aspect: f32) -> f32 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return 1.0;
    }
    let field = size.x / size.y;
    let a = aspect.max(0.01);
    1.0 - (field / a).min(a / field)
}

/// Columns and rows of the grid (short last row stretched) that crops the photos of `aspects` (field order) least
/// when they cover their fields: the smallest largest loss of a single photo wins,
/// then the smallest mean loss, then the larger fields, then fewer rows. Each field loses `foot` below the photo.
pub fn best_fill_grid(area: Rect, aspects: &[f32], foot: f32, gap: f32) -> (usize, usize) {
    const TOL: f32 = 0.02;
    let n = aspects.len().max(1);
    let mut best: Option<(usize, usize, f32, f32, f32)> = None;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        if rows > 1 && (rows - 1) * cols >= n {
            continue;
        }
        let cells = grid_cells(n, area, cols, rows, gap, true);
        let losses: Vec<f32> = cells
            .iter()
            .enumerate()
            .map(|(k, c)| crop_loss(Vec2::new(c.width(), (c.height() - foot).max(1.0)), aspects.get(k).copied().unwrap_or(1.5)))
            .collect();
        let max = losses.iter().copied().fold(0.0f32, f32::max);
        let mean = losses.iter().sum::<f32>() / n as f32;
        let size = cells.iter().map(|c| c.width() * (c.height() - foot).max(1.0)).fold(f32::MAX, f32::min);
        let better = match best {
            None => true,
            Some((_, brows, bmax, bmean, bsize)) => {
                if (max - bmax).abs() > TOL {
                    max < bmax
                } else if (mean - bmean).abs() > TOL {
                    mean < bmean
                } else if (size - bsize).abs() > 0.01 * bsize {
                    size > bsize
                } else {
                    rows < brows
                }
            }
        };
        if better {
            best = Some((cols, rows, max, mean, size));
        }
    }
    best.map(|b| (b.0, b.1)).unwrap_or((n, 1))
}

/// Cells of the fields (in field order) for an arrangement. `aspects`: the photos' shapes in field order (the
/// median decides an equal grid; "fill" weighs every photo's crop); `focus`: the field shown large by
/// [`Arrange::FocusLarge`]; `fill`: the photos cover their fields and a short last row stretches over the full
/// width. Depends on the area and the photos' shapes only, never on the zoom.
pub fn layout(n: usize, arrange: Arrange, area: Rect, aspects: &[f32], foot: f32, gap: f32, focus: usize, fill: bool) -> Vec<Rect> {
    let n = n.max(1);
    let mut sorted: Vec<f32> = aspects.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let aspect = sorted.get(sorted.len() / 2).copied().unwrap_or(1.5);
    match arrange {
        Arrange::Row => grid_cells(n, area, n, 1, gap, fill),
        Arrange::Column => grid_cells(n, area, 1, n, gap, fill),
        Arrange::Square => {
            let cols = (n as f32).sqrt().ceil() as usize;
            grid_cells(n, area, cols, n.div_ceil(cols), gap, fill)
        }
        Arrange::Auto | Arrange::All if fill => {
            let mut all: Vec<f32> = aspects.iter().copied().take(n).collect();
            all.resize(n, aspect);
            let (c, r) = best_fill_grid(area, &all, foot, gap);
            grid_cells(n, area, c, r, gap, true)
        }
        Arrange::Auto | Arrange::All => {
            let (c, r) = best_grid(n, area.size(), aspect, foot, gap);
            grid_cells(n, area, c, r, gap, fill)
        }
        Arrange::FocusLarge if n == 1 => vec![area],
        Arrange::FocusLarge => {
            let others = n - 1;
            // the others in a strip beside (a quarter of the width) or below (a third of the height): whichever
            // shows the large photo larger
            let side_w = (area.width() * 0.25).max(80.0);
            let big_side = Rect::from_min_max(area.min, egui::pos2(area.max.x - side_w - gap, area.max.y));
            let strip_side = Rect::from_min_max(egui::pos2(big_side.max.x + gap, area.min.y), area.max);
            let low_h = (area.height() / 3.0).max(80.0);
            let big_low = Rect::from_min_max(area.min, egui::pos2(area.max.x, area.max.y - low_h - gap));
            let strip_low = Rect::from_min_max(egui::pos2(area.min.x, big_low.max.y + gap), area.max);
            let (big, small) = if photo_area(big_side.size(), aspect, foot) >= photo_area(big_low.size(), aspect, foot) {
                (big_side, grid_cells(others, strip_side, 1, others, gap, fill))
            } else {
                (big_low, grid_cells(others, strip_low, others, 1, gap, fill))
            };
            let mut small = small.into_iter();
            (0..n).map(|k| if k == focus.min(n - 1) { big } else { small.next().unwrap_or(big) }).collect()
        }
    }
}

/// How many fields of "Alle" fit `area` with each at least [`MIN_ALL_FIELD`] (at most [`MAX_ALL`], at least 1).
pub fn all_capacity(area: Vec2) -> usize {
    let cols = (area.x / MIN_ALL_FIELD).floor().max(1.0) as usize;
    let rows = (area.y / MIN_ALL_FIELD).floor().max(1.0) as usize;
    (cols * rows).clamp(1, MAX_ALL)
}

/// The arrangements other than `arrange` whose fields are exactly the ones of `arrange` (e.g. "2 × 2" and "Eine
/// Reihe" with two photos): choosing one would change nothing visible.
#[allow(clippy::too_many_arguments)]
pub fn same_layouts(n: usize, arrange: Arrange, area: Rect, aspects: &[f32], foot: f32, gap: f32, focus: usize, fill: bool) -> Vec<Arrange> {
    let now = layout(n, arrange, area, aspects, foot, gap, focus, fill);
    let close = |a: &[Rect], b: &[Rect]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x.min - y.min).length() < 1.0 && (x.max - y.max).length() < 1.0);
    [Arrange::Auto, Arrange::Row, Arrange::Column, Arrange::Square, Arrange::FocusLarge, Arrange::All]
        .into_iter()
        .filter(|&a| a != arrange && close(&layout(n, a, area, aspects, foot, gap, focus, fill), &now))
        .collect()
}

/// Size of a photo of aspect `aspect` (w / h) fitted into `field`.
pub fn fit_size(field: Vec2, aspect: f32) -> Vec2 {
    let a = aspect.max(0.01);
    let k = (field.x / a).min(field.y);
    Vec2::new(k * a, k)
}

/// Size of a photo of aspect `aspect` at the zoom "fit": whole in `field`, or (`fill`) covering it.
pub fn base_size(field: Vec2, aspect: f32, fill: bool) -> Vec2 {
    if !fill {
        return fit_size(field, aspect);
    }
    let a = aspect.max(0.01);
    let k = (field.x / a).max(field.y);
    Vec2::new(k * a, k)
}

/// Centre (0..1 of the photo) kept so that a photo larger than its field never shows an empty border; a smaller one
/// stays in the middle.
pub fn clamp_center(center: Vec2, size: Vec2, field: Vec2) -> Vec2 {
    let mut c = center;
    for k in 0..2 {
        c[k] = if size[k] <= field[k] + 0.5 { 0.5 } else { c[k].clamp(0.5 * field[k] / size[k], 1.0 - 0.5 * field[k] / size[k]) };
    }
    c
}

/// Rectangle of the photo in `field`: fitted (or filling the field, `fill`), magnified by `zoom` (1 = fitted), with
/// `center` (0..1) in the middle.
pub fn image_rect(field: Rect, aspect: f32, zoom: f32, center: Vec2, fill: bool) -> Rect {
    let size = base_size(field.size(), aspect, fill) * zoom.max(1.0);
    let c = clamp_center(center, size, field.size());
    Rect::from_center_size(field.center() + (Vec2::new(0.5, 0.5) - c) * size, size)
}

/// One coupled field for [`clamp_shared_center`]: its photo's size on screen, the field's size and the offset of
/// its centre against the shared one (alignment shift plus a field moved on its own).
#[derive(Clone, Copy, Debug)]
pub struct Coupled {
    pub size: Vec2,
    pub field: Vec2,
    pub offset: Vec2,
}

/// The shared centre of coupled fields, kept where at least one field can still move: within the union of the
/// fields' own ranges (each in shared coordinates). Every field then stops at its own edge ([`image_rect`] clamps
/// it) while the others go on; no field stops the others. An axis on which no photo is larger than its field stays
/// in the middle.
pub fn clamp_shared_center(center: Vec2, fields: &[Coupled]) -> Vec2 {
    let mut c = center;
    for k in 0..2 {
        let mut range: Option<(f32, f32)> = None;
        for f in fields.iter().filter(|f| f.size[k] > f.field[k] + 0.5) {
            let half = 0.5 * f.field[k] / f.size[k];
            let (lo, hi) = (half - f.offset[k], 1.0 - half - f.offset[k]);
            range = Some(range.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        }
        c[k] = match range {
            Some((lo, hi)) => c[k].clamp(lo, hi),
            None => 0.5,
        };
    }
    c
}

/// Zoom about a point: the photo point under `at` stays under it. Returns the new centre.
pub fn zoom_about(field: Rect, aspect: f32, zoom: f32, center: Vec2, new_zoom: f32, at: egui::Pos2, fill: bool) -> Vec2 {
    let r = image_rect(field, aspect, zoom, center, fill);
    let uv = (at - r.min) / r.size();
    let size = base_size(field.size(), aspect, fill) * new_zoom.max(1.0);
    // the point uv lies at `at`: centre = uv − (at − field centre) / size
    uv - (at - field.center()) / size
}

// ───────────────────────────────────────────── Tournament

/// One state of the tournament (kept for "Letztes Duell").
#[derive(Clone, Debug, PartialEq)]
struct Round {
    /// photos not yet in a duel of this phase
    queue: Vec<i64>,
    /// left (holder) and right (challenger)
    pair: Option<(i64, i64)>,
    /// loser → the photo it lost to (main phase and runoffs)
    lost_to: Vec<(i64, i64)>,
    /// out without a winner ("Beide raus")
    out: Vec<i64>,
    /// photos already sent back once after "Gleich gut"
    requeued: Vec<i64>,
    /// 0 main knock-out, 1 runoff for place 2, 2 runoff for place 3, 3 done
    phase: u8,
    places: [Option<i64>; 3],
    duels: usize,
}

/// Knock-out as the duel of before (the winner stays, the next one challenges it), then short runoffs for places 2
/// and 3 among the photos that lost directly to the ones above. Nothing is set on the photos until "Übernehmen".
#[derive(Clone, Debug)]
pub struct Tournament {
    pub entrants: Vec<i64>,
    now: Round,
    history: Vec<Round>,
    /// the result is shown (the tournament itself stays until "Übernehmen" or "Abbrechen")
    pub done: bool,
}

/// What a decision in the duel was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Left,
    Right,
    Tie,
    BothOut,
}

impl Tournament {
    pub fn new(entrants: Vec<i64>) -> Tournament {
        let mut t = Tournament { now: Round { queue: entrants.clone(), pair: None, lost_to: Vec::new(), out: Vec::new(), requeued: Vec::new(), phase: 0, places: [None; 3], duels: 0 }, entrants, history: Vec::new(), done: false };
        t.fill();
        t
    }

    pub fn pair(&self) -> Option<(i64, i64)> {
        self.now.pair
    }

    pub fn places(&self) -> [Option<i64>; 3] {
        self.now.places
    }

    pub fn duels(&self) -> usize {
        self.now.duels
    }

    /// All entrants in the order of the result: places 1–3, then the others still in the running at the end, then
    /// those put out (each group in entrant order).
    pub fn ranking(&self) -> Vec<i64> {
        let placed: Vec<i64> = self.now.places.iter().flatten().copied().collect();
        let mut v = placed.clone();
        let rest = self.entrants.iter().copied().filter(|x| !placed.contains(x));
        let (out, kept): (Vec<i64>, Vec<i64>) = rest.partition(|x| self.now.out.contains(x));
        v.extend(kept);
        v.extend(out);
        v
    }

    /// Rough number of duels in all (main knock-out plus the runoffs), for the progress bar.
    pub fn estimate(&self) -> usize {
        let n = self.entrants.len();
        if n < 2 {
            return 0;
        }
        let runoff = ((n as f32).log2().ceil() as usize).saturating_sub(1);
        (n - 1) + runoff + runoff.saturating_sub(1).max(1)
    }

    /// Out of the running (dimmed in the filmstrip): lost and not a candidate of the phase running, or "Beide raus".
    pub fn is_out(&self, image: i64) -> bool {
        if self.now.out.contains(&image) {
            return true;
        }
        if self.now.places.contains(&Some(image)) {
            return false;
        }
        let in_play = self.now.queue.contains(&image) || self.now.pair.is_some_and(|(a, b)| a == image || b == image);
        !in_play && self.entrants.contains(&image) && (self.now.lost_to.iter().any(|(l, _)| *l == image) || self.now.phase >= 3)
    }

    /// Decide the duel shown.
    pub fn decide(&mut self, d: Decision) {
        let Some((left, right)) = self.now.pair else { return };
        self.history.push(self.now.clone());
        self.now.duels += 1;
        self.now.pair = None;
        match d {
            Decision::Left | Decision::Right => {
                let (win, lose) = if d == Decision::Left { (left, right) } else { (right, left) };
                self.now.lost_to.push((lose, win));
                self.now.queue.insert(0, win);
            }
            Decision::Tie => {
                // both stay: the challenger goes back to the end once; a second tie counts as a loss to the holder
                if self.now.requeued.contains(&right) || self.now.queue.is_empty() {
                    self.now.lost_to.push((right, left));
                } else {
                    self.now.requeued.push(right);
                    self.now.queue.push(right);
                }
                self.now.queue.insert(0, left);
            }
            Decision::BothOut => {
                self.now.out.push(left);
                self.now.out.push(right);
            }
        }
        self.fill();
    }

    /// "Letztes Duell": back to the state before the last decision.
    pub fn undo(&mut self) -> bool {
        match self.history.pop() {
            Some(r) => {
                self.now = r;
                self.done = false;
                true
            }
            None => false,
        }
    }

    /// Next pair, or the end of a phase.
    fn fill(&mut self) {
        loop {
            if self.now.phase >= 3 {
                self.now.queue.clear();
                self.done = true;
                return;
            }
            if self.now.queue.len() >= 2 {
                let a = self.now.queue.remove(0);
                let b = self.now.queue.remove(0);
                self.now.pair = Some((a, b));
                return;
            }
            // one left: the winner of this phase
            let winner = self.now.queue.pop();
            let phase = self.now.phase as usize;
            self.now.places[phase] = winner;
            self.now.phase += 1;
            self.now.requeued.clear();
            // candidates of the next runoff: lost directly to one of the places so far – or to a photo put out with
            // "Beide raus", which leaves its beaten ones without a place otherwise (QA matrix 20.11.2026) – not
            // placed, not out
            let placed: Vec<i64> = self.now.places.iter().flatten().copied().collect();
            let mut next: Vec<i64> = Vec::new();
            for (l, w) in &self.now.lost_to {
                if (placed.contains(w) || self.now.out.contains(w)) && !placed.contains(l) && !self.now.out.contains(l) && !next.contains(l) {
                    next.push(*l);
                }
            }
            // in entrant order
            next.sort_by_key(|x| self.entrants.iter().position(|e| e == x));
            if next.is_empty() || winner.is_none() {
                self.now.phase = 3;
                continue;
            }
            self.now.queue = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_keep_their_photos_and_fill_from_the_focus() {
        let imgs = [1, 2, 3, 4, 5, 6];
        // a window in selection order holding the focus: 1–4 with 3 focused, 3–6 with 6 focused
        assert_eq!(choose_fields(&imgs, &[], Some(3), None, 4), vec![1, 2, 3, 4]);
        assert_eq!(choose_fields(&imgs, &[], Some(6), None, 4), vec![3, 4, 5, 6]);
        // a field keeps its photo while it stays selected; the reference is always first
        assert_eq!(choose_fields(&imgs, &[2, 5], Some(5), Some(6), 4), vec![6, 2, 3, 5]);
        // deselected photos leave their field
        assert_eq!(choose_fields(&[1, 3], &[1, 2, 3], Some(1), None, 4), vec![1, 3]);
    }

    /// QA test.5 repeat: the fields follow the order of the strip when it changes (a series opened in the strip moved
    /// its members), and a photo taken in shows on the page that holds it.
    #[test]
    fn fields_follow_the_strip_order_and_show_a_photo_taken_in() {
        let mut c = Compare::new(None, None);
        // first frame: hidden members at the end
        c.follow(vec![1, 2, 30, 40, 50, 60], Some(1));
        assert_eq!(c.fields, vec![1, 2, 30, 40]);
        // the strip opened the series: the same photos, other order → a window again, no gaps
        c.follow(vec![1, 30, 40, 50, 60, 2], Some(1));
        assert_eq!(c.fields, vec![1, 30, 40, 50]);
        assert_eq!(c.page_text().as_deref(), Some("1–4"));
        // ⌘-click takes 70 in: focused, on the page that holds it
        c.follow(vec![1, 30, 40, 50, 60, 2, 70], None);
        assert!(c.fields.contains(&70));
        assert_eq!(c.focus, Some(70));
        // a photo swapped in by hand keeps its field when the order changes
        c.swap_into_focus(1);
        let kept = c.fields.clone();
        c.follow(vec![70, 1, 30, 40, 50, 60, 2], None);
        assert_eq!(c.fields.iter().collect::<std::collections::HashSet<_>>(), kept.iter().collect());
    }

    /// QA 1.5.0-test.5: five photos, four fields – which are shown is said ("1–4") and the others are reached by
    /// paging, also for a selection that is no series.
    #[test]
    fn more_photos_than_fields_page_through() {
        let mut c = Compare::new(None, None);
        c.follow(vec![1, 2, 3, 4, 5], Some(3));
        assert_eq!(c.fields, vec![1, 2, 3, 4]);
        assert_eq!(c.page_text().as_deref(), Some("1–4"));
        assert!(c.page(1));
        assert_eq!(c.fields, vec![2, 3, 4, 5]);
        // the next follow keeps the page
        c.follow(vec![1, 2, 3, 4, 5], None);
        assert_eq!(c.fields, vec![2, 3, 4, 5]);
        assert_eq!(c.page_text().as_deref(), Some("2–5"));
        assert!(!c.can_page(1) && c.can_page(-1), "at the end: only back");
        assert!(!c.page(1), "at the end");
        assert!(c.page(-1));
        assert_eq!(c.fields, vec![1, 2, 3, 4]);
        // everything shown: no pages
        c.follow(vec![1, 2], None);
        assert_eq!(c.page_text(), None);
    }

    #[test]
    fn swapping_replaces_the_focused_field_never_the_reference() {
        let mut c = Compare::new(None, Some(1));
        c.follow(vec![1, 2, 3, 4, 5], Some(2));
        assert_eq!(c.fields, vec![1, 2, 3, 4]);
        c.focus = Some(3);
        c.swap_into_focus(5);
        // 5 takes the place of 3 and stands where the strip has it
        assert_eq!(c.fields, vec![1, 2, 4, 5]);
        c.focus = Some(1);
        c.swap_into_focus(3);
        assert!(c.fields.contains(&1), "the reference keeps its field: {:?}", c.fields);
    }

    #[test]
    fn auto_shows_equal_fields_largest_and_centres_a_short_row() {
        // three landscape photos on a wide area: one row; on a square-ish area: 2 + 1 with the third centred
        assert_eq!(best_grid(3, Vec2::new(1600.0, 500.0), 1.5, 26.0, 18.0), (3, 1));
        assert_eq!(best_grid(3, Vec2::new(900.0, 800.0), 1.5, 26.0, 18.0), (2, 2));
        let cells = layout(3, Arrange::Auto, Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(900.0, 800.0)), &[1.5], 26.0, 18.0, 0, false);
        let mid = cells[2].center().x;
        assert!((mid - 450.0).abs() < 1.0, "third field centred: {mid}");
        // portrait photos in a row, four landscape ones 2 × 2
        assert_eq!(best_grid(3, Vec2::new(1600.0, 900.0), 0.66, 26.0, 18.0), (3, 1));
        assert_eq!(best_grid(4, Vec2::new(1600.0, 900.0), 1.5, 26.0, 18.0), (2, 2));
        // all fields of an arrangement have the same size
        for c in &cells {
            assert!((c.width() - cells[0].width()).abs() < 0.01 && (c.height() - cells[0].height()).abs() < 0.01);
        }
    }

    #[test]
    fn focus_large_gives_the_focused_photo_the_big_cell() {
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1200.0, 800.0));
        let cells = layout(3, Arrange::FocusLarge, area, &[1.5], 26.0, 18.0, 1, false);
        assert!(cells[1].area() > cells[0].area() * 3.0 && (cells[0].area() - cells[2].area()).abs() < 1.0);
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                assert!(!a.shrink(0.5).intersects(b.shrink(0.5)), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let field = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(400.0, 300.0));
        let at = egui::pos2(300.0, 100.0);
        for fill in [false, true] {
        let c = zoom_about(field, 1.5, 1.0, Vec2::splat(0.5), 4.0, at, fill);
        let before = image_rect(field, 1.5, 1.0, Vec2::splat(0.5), fill);
        let after = image_rect(field, 1.5, 4.0, c, fill);
        let uv_before = (at - before.min) / before.size();
        let uv_after = (at - after.min) / after.size();
        assert!((uv_before - uv_after).length() < 1e-3, "{uv_before:?} {uv_after:?}");
    }
    }

    #[test]
    fn a_zoomed_photo_never_shows_an_empty_border() {
        let field = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(400.0, 300.0));
        let r = image_rect(field, 1.5, 3.0, Vec2::new(0.0, 1.0), false);
        assert!(r.left() <= field.left() + 0.01 && r.bottom() >= field.bottom() - 0.01, "{r:?}");
    }

    /// Two coupled fields of different shapes and sizes, the second one shifted against the first: dragged far to
    /// the right, each reaches its own right edge; the one at its edge first stays there while the other goes on.
    #[test]
    fn coupled_fields_stop_each_at_their_own_edge() {
        let fields = [
            // a landscape photo, zoomed 3×, in a wide field
            (Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(600.0, 400.0)), 1.5, Vec2::ZERO),
            // a slightly narrower (cropped) photo in a smaller field, aligned 0.1 to the left
            (Rect::from_min_size(egui::pos2(620.0, 0.0), Vec2::new(500.0, 400.0)), 1.25, Vec2::new(-0.1, 0.0)),
        ];
        let zoom = 3.0;
        let coupled: Vec<Coupled> = fields.iter().map(|(r, a, o)| Coupled { size: base_size(r.size(), *a, false) * zoom, field: r.size(), offset: *o }).collect();
        let shown = |center: Vec2| fields.map(|(r, a, o)| image_rect(r, a, zoom, center + o, false));
        // far beyond every edge: both photos end exactly at their field's right edge (nothing missing, no border)
        let c = clamp_shared_center(Vec2::new(5.0, 0.5), &coupled);
        for ((field, ..), r) in fields.iter().zip(shown(c)) {
            assert!((r.right() - field.right()).abs() < 0.01, "{r:?} in {field:?}");
        }
        // the first field reaches its edge at a smaller shared centre; the second still moves beyond it
        let first_edge = 1.0 - 0.5 / zoom;
        let a = shown(Vec2::new(first_edge, 0.5));
        let b = shown(Vec2::new(first_edge + 0.05, 0.5));
        assert!((a[0].right() - fields[0].0.right()).abs() < 0.01 && a[0] == b[0], "the first stays at its edge");
        assert!(b[1].left() < a[1].left() - 1.0, "the second goes on: {:?} → {:?}", a[1], b[1]);
        // the other way round the same, and the shared centre never leaves the union of the ranges
        let c = clamp_shared_center(Vec2::new(-5.0, 0.5), &coupled);
        assert!((c.x - (0.5 / zoom - 0.0)).abs() < 1e-4, "{c:?}");
        for ((field, ..), r) in fields.iter().zip(shown(c)) {
            assert!(r.left() <= field.left() + 0.01, "{r:?} in {field:?}");
        }
    }

    /// Coupled over the photo point: the same normalized point lies in the middle of both fields, whatever their
    /// pixel sizes, as long as neither is at its edge; an axis without room stays centred.
    #[test]
    fn coupling_keeps_the_same_photo_point_in_fields_of_different_sizes() {
        let fa = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(600.0, 400.0));
        let fb = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(300.0, 300.0));
        let zoom = 4.0;
        let coupled = [fa, fb].map(|f| Coupled { size: base_size(f.size(), 1.5, false) * zoom, field: f.size(), offset: Vec2::ZERO });
        let c = clamp_shared_center(Vec2::new(0.6, 0.45), &coupled);
        assert_eq!(c, Vec2::new(0.6, 0.45));
        for f in [fa, fb] {
            let r = image_rect(f, 1.5, zoom, c, false);
            let uv = (f.center() - r.min) / r.size();
            assert!((uv - c).length() < 1e-4, "{uv:?}");
        }
        // at "fit" nothing moves
        let fitted = [fa, fb].map(|f| Coupled { size: base_size(f.size(), 1.5, false), field: f.size(), offset: Vec2::ZERO });
        assert_eq!(clamp_shared_center(Vec2::new(0.9, 0.1), &fitted), Vec2::splat(0.5));
    }

    fn run(t: &mut Tournament, prefer: &[i64]) {
        // the photo earlier in `prefer` wins
        while let Some((a, b)) = t.pair() {
            let rank = |x: i64| prefer.iter().position(|&p| p == x).unwrap_or(usize::MAX);
            t.decide(if rank(a) <= rank(b) { Decision::Left } else { Decision::Right });
        }
    }

    #[test]
    fn tournament_finds_the_best_three() {
        let mut t = Tournament::new((1..=8).collect());
        run(&mut t, &[6, 3, 1, 8, 2, 4, 5, 7]);
        assert!(t.done);
        assert_eq!(t.places(), [Some(6), Some(3), Some(1)]);
        assert!(t.duels() <= 13, "{} duels", t.duels());
        assert!(t.is_out(7) && !t.is_out(6));
    }

    /// The finished tournament shows its ranking in "Alle": fields in the order of the places with place marks, the
    /// arrangement of before comes back afterwards; ⌘Z takes the last duel back into the tournament.
    #[test]
    fn ranking_shows_the_places_as_fields_of_all() {
        let mut c = Compare::new(None, None);
        c.arrange = Arrange::Square;
        let imgs: Vec<i64> = (1..=6).collect();
        c.follow(imgs.clone(), Some(1));
        c.tournament = Some(Tournament::new(imgs.clone()));
        c.mode = Mode::Tournament;
        run(c.tournament.as_mut().unwrap(), &[5, 2, 6, 1, 3, 4]);
        let places = c.tournament.as_ref().unwrap().places();
        assert_eq!(places[0], Some(5));
        c.show_ranking_if_done();
        c.follow(imgs.clone(), Some(1));
        assert!(c.ranking_shown());
        assert_eq!((c.mode, c.arrange), (Mode::Compare, Arrange::All));
        let placed: Vec<i64> = places.iter().flatten().copied().collect();
        assert_eq!(&c.fields[..placed.len()], &placed[..], "places first: {:?}", c.fields);
        assert_eq!(c.fields.len(), 6);
        assert_eq!(c.focus, Some(5));
        assert_eq!(c.rank_of(5), Some(1));
        assert_eq!(c.rank_of(placed[2]), Some(3));
        assert!(c.rank_of(*c.fields.last().unwrap()).is_none());
        // ⌘Z: the last duel again, the arrangement of before
        assert!(c.undo_duel());
        assert_eq!((c.mode, c.arrange), (Mode::Tournament, Arrange::Square));
        assert!(!c.ranking_shown());
        // decided again, then taken over: the ranking ends, the arrangement of before
        run(c.tournament.as_mut().unwrap(), &[5, 2, 6, 1, 3, 4]);
        c.show_ranking_if_done();
        assert!(c.ranking_shown());
        c.tournament = None;
        c.leave_ranking();
        c.follow(imgs, Some(1));
        assert_eq!(c.arrange, Arrange::Square);
        assert!(c.rank_of(5).is_none());
    }

    #[test]
    fn tournament_undo_and_both_out() {
        let mut t = Tournament::new(vec![1, 2, 3]);
        assert_eq!(t.pair(), Some((1, 2)));
        t.decide(Decision::BothOut);
        assert_eq!(t.pair(), None);
        assert!(t.done);
        assert_eq!(t.places()[0], Some(3));
        assert!(t.undo());
        assert_eq!(t.pair(), Some((1, 2)));
        // a tie keeps both; a second tie with the same challenger counts as a loss
        t.decide(Decision::Tie);
        assert_eq!(t.pair(), Some((1, 3)));
        t.decide(Decision::Left);
        assert_eq!(t.pair(), Some((1, 2)));
        t.decide(Decision::Tie);
        assert!(t.places()[0] == Some(1));
    }

    /// QA matrix: ← (1 beats 2), = (1 and 3 tie), → (4 beats 1), "Beide raus" (4 and 5): 3 won, and 1 and 2 – beaten
    /// by a photo that is out now – got no place at all (rejected by "Übernehmen" without ever meeting 3).
    #[test]
    fn both_out_keeps_the_beaten_in_the_running() {
        let mut t = Tournament::new(vec![1, 2, 3, 4, 5]);
        for d in [Decision::Left, Decision::Tie, Decision::Right, Decision::BothOut] {
            t.decide(d);
        }
        while let Some(_) = t.pair() {
            t.decide(Decision::Left);
        }
        assert_eq!(t.places(), [Some(3), Some(1), Some(2)]);
        assert!(t.is_out(4) && t.is_out(5));
    }

    // ───────────────────────────── permutation tests (QA matrix of the comparison)

    /// Small deterministic random source (xorshift), so a failing seed can be run again.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// What the view promises after every frame: fields are selected photos without doubles, as many as fit,
    /// the reference first and the rest in the order of the filmstrip, the focus on a field.
    fn check_fields(c: &Compare, what: &str) {
        if c.mode == Mode::Tournament {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for f in &c.fields {
            assert!(c.images.contains(f), "{what}: field {f} not selected: {:?} / {:?}", c.fields, c.images);
            assert!(seen.insert(*f), "{what}: field {f} twice: {:?}", c.fields);
        }
        assert_eq!(c.fields.len(), c.images.len().min(c.capacity()), "{what}: {:?} / {:?}", c.fields, c.images);
        if let Some(r) = c.reference.filter(|r| c.images.contains(r)) {
            assert_eq!(c.fields.first(), Some(&r), "{what}: reference not first: {:?}", c.fields);
        }
        let rest: Vec<usize> = c.fields.iter().filter(|&&f| Some(f) != c.reference).map(|f| c.images.iter().position(|x| x == f).unwrap()).collect();
        assert!(rest.windows(2).all(|w| w[0] < w[1]), "{what}: fields not in strip order: {:?} / {:?}", c.fields, c.images);
        match c.focus {
            Some(f) => assert!(c.fields.contains(&f), "{what}: focus {f} without a field: {:?}", c.fields),
            None => assert!(c.images.is_empty(), "{what}: no focus with {:?}", c.images),
        }
    }

    /// Random sequences of what a user does while comparing (⌘-click in and out, a click on an unselected photo of
    /// the strip, ← →, reference, light table, field keys, a new selection of 30+ photos), on selections of 1–40
    /// photos: after every step the next frame (`follow`) keeps the promises of [`check_fields`].
    #[test]
    fn permuted_changes_keep_fields_consistent() {
        let all: Vec<i64> = (1..=40).collect();
        for seed in 1..=400u64 {
            let mut rng = Rng(seed * 0x9E37_79B9_7F4A_7C15);
            let mut sel: Vec<i64> = Vec::new();
            let n0 = 1 + rng.below(8);
            while sel.len() < n0 {
                let x = all[rng.below(all.len())];
                if !sel.contains(&x) {
                    sel.push(x);
                }
            }
            sel.sort();
            let mut active = sel.first().copied();
            let mut c = Compare::new(None, None);
            c.follow(sel.clone(), active);
            check_fields(&c, &format!("seed {seed} start"));
            for step in 0..40 {
                let what = rng.below(10);
                match what {
                    // ⌘-click: a photo into the comparison
                    0 => {
                        let x = all[rng.below(all.len())];
                        if !sel.contains(&x) {
                            sel.push(x);
                            sel.sort();
                        }
                    }
                    // ⌘-click: a photo out (never the last one)
                    1 if sel.len() > 1 => {
                        let x = sel.remove(rng.below(sel.len()));
                        if c.reference == Some(x) {
                            c.reference = None;
                        }
                        if active == Some(x) {
                            active = sel.first().copied();
                        }
                    }
                    // a click on a photo of the strip: a selected one goes into the focused field, an unselected one
                    // takes the place of the focused photo in the selection (as `compare_strip_click`)
                    2 => {
                        let x = all[rng.below(all.len())];
                        if !sel.contains(&x) {
                            if let Some(f) = c.focus.filter(|f| Some(*f) != c.reference && sel.len() > 1) {
                                sel.retain(|&y| y != f);
                            }
                            sel.push(x);
                            sel.sort();
                        }
                        c.swap_into_focus(x);
                        active = Some(x);
                    }
                    // Develop: the photo opened on screen gets the focused field in the frame it is drawn
                    // (`compare_center`: follow, then swap) – no other follow before drawing
                    8 => {
                        let x = sel[rng.below(sel.len())];
                        active = Some(x);
                        c.follow(sel.clone(), active);
                        if !c.fields.contains(&x) {
                            c.swap_into_focus(x);
                        }
                        c.focus = Some(x);
                        check_fields(&c, &format!("seed {seed} step {step}: Develop opens {x} (same frame)"));
                    }
                    3 => c.step_focus(if rng.below(2) == 0 { -1 } else { 1 }),
                    4 => {
                        if let Some(f) = c.focus {
                            c.toggle_reference(f);
                        }
                    }
                    // "Alle" on and off (up to 8 fields)
                    5 => c.arrange = if c.arrange == Arrange::All { Arrange::Auto } else { Arrange::All },
                    // ⌥1–4: focus a field
                    6 => {
                        if !c.fields.is_empty() {
                            c.focus = Some(c.fields[rng.below(c.fields.len())]);
                        }
                    }
                    // a new selection of 30+ photos (⌘A in another folder)
                    7 => {
                        sel = all[..30 + rng.below(11)].to_vec();
                        active = Some(sel[rng.below(sel.len())]);
                    }
                    _ => {}
                }
                c.follow(sel.clone(), active);
                check_fields(&c, &format!("seed {seed} step {step} (action {what})"));
            }
        }
    }

    /// Random duels with ties, "both out" and "last duel" on 2–12 photos: the tournament always ends, never shows a
    /// photo twice or one that is out, places are distinct entrants, and undoing everything gives the first duel back.
    #[test]
    fn permuted_tournaments_end_and_undo_fully() {
        for seed in 1..=600u64 {
            let mut rng = Rng(seed * 0x2545_F491_4F6C_DD1D);
            let n = 2 + rng.below(11);
            let entrants: Vec<i64> = (1..=n as i64).map(|x| x * 10).collect();
            let mut t = Tournament::new(entrants.clone());
            let first = t.pair();
            let mut steps = 0;
            while t.pair().is_some() {
                steps += 1;
                assert!(steps < 20 * n * n, "seed {seed}: no end after {steps} steps");
                let (a, b) = t.pair().unwrap();
                assert!(a != b && entrants.contains(&a) && entrants.contains(&b), "seed {seed}: pair {a} {b}");
                assert!(!t.is_out(a) && !t.is_out(b), "seed {seed}: a photo out in the duel {a} {b}");
                if rng.below(7) == 0 {
                    t.undo();
                    continue;
                }
                t.decide(match rng.below(10) {
                    0..=3 => Decision::Left,
                    4..=7 => Decision::Right,
                    8 => Decision::Tie,
                    _ => Decision::BothOut,
                });
            }
            assert!(t.done, "seed {seed}: no pair but not done");
            let places: Vec<i64> = t.places().iter().flatten().copied().collect();
            for p in &places {
                assert!(entrants.contains(p) && !t.is_out(*p), "seed {seed}: place {p}");
            }
            let mut d = places.clone();
            d.dedup();
            assert_eq!(d.len(), places.len(), "seed {seed}: places {places:?}");
            // places fill from the top: no place 2 without place 1
            let p = t.places();
            assert!(!(p[0].is_none() && (p[1].is_some() || p[2].is_some())) && !(p[1].is_none() && p[2].is_some()), "seed {seed}: {p:?}");
            while t.undo() {}
            assert_eq!((t.pair(), t.duels(), t.done), (first, 0, false), "seed {seed}");
        }
    }

    /// Every arrangement for 1–4 fields on wide, narrow and small areas and portrait or landscape photos: as many
    /// cells as fields, each inside the area, none overlapping, none empty.
    #[test]
    fn layouts_fit_the_area_in_every_permutation() {
        let areas = [Vec2::new(2400.0, 700.0), Vec2::new(1200.0, 800.0), Vec2::new(420.0, 900.0), Vec2::new(320.0, 240.0)];
        for &size in &areas {
            let area = Rect::from_min_size(egui::pos2(10.0, 20.0), size);
            for n in 1..=MAX_FIELDS {
                for arrange in [Arrange::Auto, Arrange::Row, Arrange::Column, Arrange::Square, Arrange::FocusLarge] {
                    for (aspect, fill) in [0.66, 1.0, 1.5, 2.4].into_iter().flat_map(|a| [(a, false), (a, true)]) {
                        for focus in 0..n {
                            let cells = layout(n, arrange, area, &[aspect], 26.0, 18.0, focus, fill);
                            let what = format!("{n} {arrange:?} {size:?} {aspect} fill {fill} focus {focus}: {cells:?}");
                            assert_eq!(cells.len(), n, "{what}");
                            for (i, a) in cells.iter().enumerate() {
                                assert!(a.width() > 1.0 && a.height() > 1.0, "{what}");
                                assert!(area.expand(0.5).contains_rect(*a), "{what}");
                                for b in &cells[i + 1..] {
                                    assert!(!a.shrink(0.5).intersects(b.shrink(0.5)), "{what}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// "Füllen" – the cells cover the whole area (only the joints between them), also a
    /// short last row; and every photo covers its field at the zoom "fit", landscape and portrait alike.
    #[test]
    fn fill_leaves_no_empty_space() {
        for &size in &[Vec2::new(2400.0, 700.0), Vec2::new(1200.0, 800.0), Vec2::new(500.0, 900.0)] {
            let area = Rect::from_min_size(egui::pos2(0.0, 0.0), size);
            for n in 1..=8 {
                for arrange in [Arrange::Auto, Arrange::Row, Arrange::Column, Arrange::Square, Arrange::FocusLarge] {
                    let gap = 6.0;
                    let cells = layout(n, arrange, area, &[1.5], 26.0, gap, 0, true);
                    // covered area = whole area minus the joints: every row reaches both edges
                    let covered: f32 = cells.iter().map(|r| r.area()).sum();
                    let rows: std::collections::BTreeSet<i64> = cells.iter().map(|r| r.top().round() as i64).collect();
                    let cols_max = cells.iter().filter(|r| r.top().round() as i64 == *rows.iter().next().unwrap()).count();
                    let joints = gap * (size.y * (cols_max as f32 + 1.0) + size.x * rows.len() as f32);
                    assert!(covered >= area.area() - joints - 1.0, "{n} {arrange:?} {size:?}: {covered} of {}", area.area());
                    for r in &cells {
                        let at_left = cells.iter().any(|x| (x.top() - r.top()).abs() < 0.5 && (x.left() - area.left()).abs() < 0.5);
                        let at_right = cells.iter().any(|x| (x.top() - r.top()).abs() < 0.5 && (x.right() - area.right()).abs() < 0.5);
                        assert!(at_left && at_right || arrange == Arrange::FocusLarge, "{n} {arrange:?} {size:?}: row of {r:?} has empty space");
                    }
                }
            }
        }
        let field = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(400.0, 300.0));
        for aspect in [0.66, 1.0, 1.5, 2.4] {
            let r = image_rect(field, aspect, 1.0, Vec2::splat(0.5), true);
            assert!(r.contains_rect(field), "{aspect}: {r:?}");
            assert!((r.width() - field.width()).abs() < 0.5 || (r.height() - field.height()).abs() < 0.5, "{aspect}: cropped on one side only {r:?}");
            // moved to a corner, still no border
            let r = image_rect(field, aspect, 1.0, Vec2::new(0.0, 1.0), true);
            assert!(r.expand(0.01).contains_rect(field), "{aspect}: {r:?}");
            let w = image_rect(field, aspect, 1.0, Vec2::splat(0.5), false);
            assert!(field.expand(0.01).contains_rect(w), "{aspect}: whole {w:?}");
        }
    }

    /// In "Füllen" Auto picks the arrangement that crops least (no photo loses three
    /// quarters in a narrow strip); "Ganz zeigen" keeps the largest equal fields.
    #[test]
    fn fill_auto_crops_least() {
        const Q: f32 = 1.5;
        const H: f32 = 2.0 / 3.0;
        let wide = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1600.0, 900.0));
        let narrow = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(600.0, 900.0));
        let shape = |cells: &[Rect]| {
            let rows: std::collections::BTreeSet<i64> = cells.iter().map(|r| r.top().round() as i64).collect();
            (cells.iter().filter(|r| r.top().round() as i64 == *rows.iter().next().unwrap()).count(), rows.len())
        };
        let max_loss = |cells: &[Rect], a: &[f32]| cells.iter().zip(a).map(|(c, &a)| crop_loss(Vec2::new(c.width(), c.height() - 26.0), a)).fold(0.0f32, f32::max);
        // (area, photos, expected columns × rows)
        let cases: &[(Rect, &[f32], (usize, usize))] = &[
            (wide, &[Q, H, H, H], (2, 2)),
            (wide, &[H, Q, H, H], (2, 2)),
            (wide, &[Q, Q, Q], (2, 2)),
            (wide, &[H, H, H, H], (4, 1)),
            (wide, &[Q, H], (2, 1)),
            (wide, &[Q, Q], (2, 1)),
            (wide, &[Q, Q, Q, Q, Q, Q], (3, 2)),
            (wide, &[H, H, H, H, H], (5, 1)),
            (narrow, &[Q, Q], (1, 2)),
            (narrow, &[Q, Q, Q], (1, 3)),
            (narrow, &[H, H, Q, Q], (2, 2)),
        ];
        for &(area, aspects, expect) in cases {
            let cells = layout(aspects.len(), Arrange::Auto, area, aspects, 26.0, 4.0, 0, true);
            let got = shape(&cells);
            assert_eq!(got, expect, "{aspects:?} in {:?}", area.size());
            // never worse than a plain row or column for the most cropped photo
            for other in [Arrange::Row, Arrange::Column, Arrange::Square] {
                let o = layout(aspects.len(), other, area, aspects, 26.0, 4.0, 0, true);
                assert!(max_loss(&cells, aspects) <= max_loss(&o, aspects) + 0.021, "{aspects:?} {other:?}");
            }
        }
        // the reported case: one landscape among three portraits keeps far more than a quarter
        let cells = layout(4, Arrange::Auto, wide, &[Q, H, H, H], 26.0, 4.0, 0, true);
        assert!(crop_loss(Vec2::new(cells[0].width(), cells[0].height() - 26.0), Q) < 0.4, "{cells:?}");
        // "whole" unchanged: equal grid by the median shape
        let whole = layout(4, Arrange::Auto, wide, &[Q, H, H, H], 26.0, 18.0, 0, false);
        let (c, r) = best_grid(4, wide.size(), H, 26.0, 18.0);
        assert_eq!(shape(&whole), (c, r));
    }
}

#[cfg(test)]
mod all_tests {
    use super::*;

    /// "Alle": up to 8 fields per page, fewer when a field would get smaller than
    /// 160 pt; more photos are paged "1–8 von N"; the other arrangements keep 4.
    #[test]
    fn all_shows_up_to_eight_and_pages_the_rest() {
        assert_eq!(all_capacity(Vec2::new(1600.0, 900.0)), MAX_ALL);
        assert_eq!(all_capacity(Vec2::new(500.0, 350.0)), 6);
        assert_eq!(all_capacity(Vec2::new(100.0, 100.0)), 1);
        let images: Vec<i64> = (1..=20).collect();
        let mut c = Compare::new(None, None);
        c.arrange = Arrange::All;
        c.follow(images.clone(), Some(1));
        assert_eq!(c.fields, (1..=8).collect::<Vec<i64>>());
        assert_eq!(c.page_text().as_deref(), Some("1–8"));
        assert!(c.page(1));
        assert_eq!(c.fields, (9..=16).collect::<Vec<i64>>());
        // back to a large arrangement: four fields around the focus
        c.arrange = Arrange::Auto;
        c.follow(images.clone(), Some(9));
        assert_eq!(c.fields.len(), MAX_FIELDS);
        // the tournament keeps its pair
        c.arrange = Arrange::All;
        c.mode = Mode::Tournament;
        assert_eq!(c.capacity(), MAX_FIELDS);
    }

    /// Arrangements that give the same fields are recognised (they would change nothing on screen): with two photos
    /// "2 × 2" is one row; "Alle" equals "Auto" with up to four photos.
    #[test]
    fn same_layouts_are_found() {
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1200.0, 700.0));
        let same = same_layouts(2, Arrange::Row, area, &[1.5, 1.5], 26.0, 18.0, 0, false);
        assert!(same.contains(&Arrange::Square), "{same:?}");
        assert!(!same.contains(&Arrange::Column), "{same:?}");
        let same = same_layouts(3, Arrange::Auto, area, &[1.5; 3], 26.0, 18.0, 0, false);
        assert!(same.contains(&Arrange::All), "{same:?}");
    }
}
