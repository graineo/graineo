//! Layout of the filmstrip: cells as wide as the photo (aspect ratio from the library's dimensions, not from
//! loaded textures, so nothing jumps), tight gaps, positions per index as prefix sums. Only the visible range is
//! drawn; finding it is a binary search, so 70 000 photos cost nothing per frame.

/// Frame around the thumbnail (shows active / selected as a light background).
pub const PAD: f32 = 2.0;
/// Gap between two cells.
pub const GAP: f32 = 3.0;
/// Height of the row with stars and flag below the thumbnail.
pub const META: f32 = 15.0;
/// Thumbnail height from which the stars row appears (below it the strip shows only the photos).
pub const META_FROM: f32 = 92.0;
/// Narrowest and widest cell (relative to the height): panoramas and odd crops stay usable.
const ASPECT_RANGE: (f32, f32) = (0.4, 4.0);
/// Height of the header row (source, count, selection, filter).
pub const HEADER_H: f32 = 22.0;
/// Band below the cells for the (thin, floating) scroll bar, so it never covers the photos.
pub const SCROLLBAR: f32 = 5.0;
/// Smallest and largest height of the filmstrip panel (thumbnails about 48 to 300 pt).
pub const PANEL_MIN: f32 = HEADER_H + 2.0 * PAD + 48.0 + SCROLLBAR + 8.0;
pub const PANEL_MAX: f32 = 380.0;
/// Height when nothing is remembered yet.
pub const PANEL_DEFAULT: f32 = 120.0;

/// Thumbnail height and height of the stars row for the height available to the cells. Continuous: while the stars
/// row grows in, the thumbnails keep their height, so dragging the edge never makes them jump.
pub fn metrics(h: f32) -> (f32, f32) {
    let t = (h - 2.0 * PAD).max(16.0);
    if t <= META_FROM {
        (t, 0.0)
    } else {
        let m = (t - META_FROM).min(META);
        (t - m, m)
    }
}

/// Aspect ratio (width / height) of the upright photo from the library's dimensions, if known.
pub fn aspect(width: Option<i64>, height: Option<i64>, orientation: Option<i64>) -> Option<f32> {
    let (w, h) = (width.filter(|w| *w > 0)?, height.filter(|h| *h > 0)?);
    let (w, h) = if matches!(orientation, Some(5..=8)) { (h, w) } else { (w, h) };
    Some(w as f32 / h as f32)
}

/// Cell width for photos whose dimensions are not known yet (3:2).
pub const ASPECT_UNKNOWN: f32 = 1.5;

pub struct Layout {
    pub thumb_h: f32,
    pub meta_h: f32,
    /// start of each cell, plus the end of the strip (n + 1 values)
    xs: Vec<f32>,
}

impl Layout {
    pub fn new(aspects: impl Iterator<Item = f32>, h: f32) -> Self {
        let (thumb_h, meta_h) = metrics(h);
        let mut xs = Vec::with_capacity(aspects.size_hint().0 + 1);
        let mut x = 0.0;
        xs.push(x);
        for a in aspects {
            x += thumb_h * a.clamp(ASPECT_RANGE.0, ASPECT_RANGE.1) + 2.0 * PAD + GAP;
            xs.push(x);
        }
        Layout { thumb_h, meta_h, xs }
    }

    pub fn len(&self) -> usize {
        self.xs.len() - 1
    }

    /// Left edge of cell `i`.
    pub fn x(&self, i: usize) -> f32 {
        self.xs[i.min(self.len())]
    }

    /// Width of cell `i` (without the gap).
    pub fn width(&self, i: usize) -> f32 {
        if i >= self.len() {
            return 0.0;
        }
        self.xs[i + 1] - self.xs[i] - GAP
    }

    /// Width of the whole strip.
    pub fn total(&self) -> f32 {
        (self.xs[self.len()] - GAP).max(0.0)
    }

    /// Cells overlapping the horizontal range `a..b`.
    pub fn visible(&self, a: f32, b: f32) -> std::ops::Range<usize> {
        let n = self.len();
        // first cell whose end lies right of a
        let first = self.xs[1..].partition_point(|&end| end <= a).min(n);
        let last = self.xs.partition_point(|&start| start < b).min(n);
        first..last.max(first)
    }

    /// Scroll position that shows cell `i` (centred if it was outside the view), never past either end: an offset
    /// beyond the end is clamped by the scroll area a frame later, which made the strip jerk at the last photos.
    pub fn bring_into_view(&self, i: usize, offset: f32, view_w: f32) -> f32 {
        let (x, w) = (self.x(i), self.width(i));
        let max = (self.total() - view_w).max(0.0);
        if x < offset || x + w > offset + view_w {
            (x - (view_w - w) / 2.0).clamp(0.0, max)
        } else {
            offset.clamp(0.0, max)
        }
    }
}

/// Offset that puts cell `i` with its left edge at `x` in the visible strip (a stack opened or closed stays where it
/// stood), clamped to the strip; a cell that would not be visible there is brought into view instead.
pub fn keep_at(layout: &Layout, i: usize, x: f32, view_w: f32) -> f32 {
    let max = (layout.total() - view_w).max(0.0);
    let offset = (layout.x(i) - x).clamp(0.0, max);
    layout.bring_into_view(i, offset, view_w)
}

/// Salt of the filmstrip's scroll area.
pub const AREA: &str = "filmstrip";

/// Remember the id of the filmstrip's scroll area (from its output) for [`forget_scroll_motion`].
pub fn remember_area(ctx: &eframe::egui::Context, id: eframe::egui::Id) {
    ctx.data_mut(|d| d.insert_temp(eframe::egui::Id::unique("filmstrip-area-id"), id));
}

/// Did the view (library / single photo) or the list change since the last frame? Then the active photo is brought into
/// view again.
pub fn view_or_list_changed(ctx: &eframe::egui::Context, library: bool, list: u64) -> bool {
    let key = eframe::egui::Id::unique("filmstrip-view-list");
    let now = (library, list);
    let before = ctx.data(|d| d.get_temp::<(bool, u64)>(key));
    ctx.data_mut(|d| d.insert_temp(key, now));
    before.is_some_and(|b| b != now)
}

/// Before a jump: drop coasting speed, a running scroll animation and the scroll range of the last frame, so that the
/// offset set afterwards is taken as it is (see `App::filmstrip`).
pub fn forget_scroll_motion(ctx: &eframe::egui::Context) {
    if let Some(id) = ctx.data(|d| d.get_temp::<eframe::egui::Id>(eframe::egui::Id::unique("filmstrip-area-id"))) {
        eframe::egui::scroll_area::State::default().store(ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_continuous_and_compact() {
        // small: only photos, no stars row
        let (t, m) = metrics(52.0);
        assert_eq!((t, m), (48.0, 0.0));
        // no jump anywhere while dragging
        let mut prev = metrics(30.0).0;
        let mut h = 30.0;
        while h < 400.0 {
            h += 0.25;
            let (t, m) = metrics(h);
            assert!((t - prev).abs() <= 0.25 + 1e-3, "jump at {h}");
            assert!((t + m + 2.0 * PAD - h).abs() < 1e-3 || t == 16.0);
            prev = t;
        }
        assert_eq!(metrics(300.0).1, META);
    }

    #[test]
    fn aspect_from_dimensions() {
        assert_eq!(aspect(Some(6000), Some(4000), Some(1)), Some(1.5));
        assert!((aspect(Some(6000), Some(4000), Some(6)).unwrap() - 4000.0 / 6000.0).abs() < 1e-6);
        assert_eq!(aspect(None, Some(4000), None), None);
        assert_eq!(aspect(Some(0), Some(4000), None), None);
    }

    #[test]
    fn positions_and_visible_range() {
        let a = [1.5, 0.75, 1.0, 1.5];
        let l = Layout::new(a.iter().copied(), 50.0 + 2.0 * PAD);
        assert_eq!(l.len(), 4);
        assert_eq!(l.width(0), 75.0 + 2.0 * PAD);
        assert_eq!(l.width(1), 37.5 + 2.0 * PAD);
        assert_eq!(l.x(1), l.width(0) + GAP);
        assert_eq!(l.total(), l.x(3) + l.width(3));
        assert_eq!(l.visible(0.0, 1.0), 0..1);
        assert_eq!(l.visible(0.0, l.total()), 0..4);
        assert_eq!(l.visible(l.x(1) + 1.0, l.x(2) + 1.0), 1..3);
        assert_eq!(l.visible(l.total() + 10.0, l.total() + 20.0), 4..4);
        let empty = Layout::new(std::iter::empty(), 60.0);
        assert_eq!(empty.visible(0.0, 100.0), 0..0);
        assert_eq!(empty.total(), 0.0);
    }

    #[test]
    fn bring_into_view_clamps_at_both_ends() {
        let l = Layout::new(std::iter::repeat(1.5).take(1000), 60.0);
        let view = 800.0;
        let max = l.total() - view;
        assert_eq!(l.bring_into_view(0, 500.0, view), 0.0);
        assert_eq!(l.bring_into_view(999, 0.0, view), max);
        // already visible: stays
        assert_eq!(l.bring_into_view(5, 0.0, view), 0.0);
        // in the middle: centred
        let o = l.bring_into_view(500, 0.0, view);
        assert!((o + view / 2.0 - (l.x(500) + l.width(500) / 2.0)).abs() < 1e-2);
        // stepping one by one to the end never moves past max and never goes back
        let mut off = 0.0;
        for i in 0..1000 {
            let n = l.bring_into_view(i, off, view);
            assert!(n <= max + 1e-3 && n + 1e-3 >= off.min(max));
            off = n;
        }
    }

    /// A stack closed or opened: its first cell keeps its place in the visible strip; at the ends the strip clamps.
    #[test]
    fn keep_at_holds_the_stack_in_place() {
        let l = Layout::new(std::iter::repeat(1.5).take(200), 60.0);
        let view = 800.0;
        let o = keep_at(&l, 100, 240.0, view);
        assert!((l.x(100) - o - 240.0).abs() < 1e-3);
        // near the start: clamped at 0, the cell stays visible
        assert_eq!(keep_at(&l, 1, 600.0, view), 0.0);
        // near the end: clamped at the end
        assert!((keep_at(&l, 199, 0.0, view) - (l.total() - view)).abs() < 1e-3);
    }

    #[test]
    fn large_list_is_cheap() {
        let t = std::time::Instant::now();
        let l = Layout::new((0..70_000).map(|i| if i % 3 == 0 { 0.667 } else { 1.5 }), 60.0);
        let r = l.visible(1_000_000.0, 1_002_000.0);
        assert!(!r.is_empty() && r.len() < 100);
        assert!(t.elapsed().as_millis() < 200);
    }
}
