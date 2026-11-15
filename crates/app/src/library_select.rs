//! Selection in the grid and the filmstrip: ⇧+arrows extend from an anchor, a rubber band on free space selects
//! the tiles it touches. The pure parts (positions, rectangles) live here and are tested; `library_ui.rs` and
//! `main.rs` only call them.

use eframe::egui::{self, Pos2, Rect};
use std::collections::HashSet;

use crate::App;

/// Growing selection with ⇧+arrow: anchor and moving end, both as positions in the visible list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ext {
    pub anchor: i64,
    pub head: i64,
    /// the selection this extension produced (changed from outside → start again at the active photo)
    pub made: HashSet<i64>,
}

impl App {
    /// ⇧+←/→ in the filmstrip of the single view: extend the selection from the open photo (anchor) one photo at a
    /// time; the opposite arrow shrinks it back. The open photo stays open, the strip scrolls after the moving end.
    pub(crate) fn extend_strip(&mut self, forward: bool) {
        let vis = self.visible();
        let Some(cur) = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable) else { return };
        let Some(cur_pos) = vis.iter().position(|&v| v == cur) else { return };
        let selected: Vec<usize> = vis.iter().enumerate().filter(|(_, &v)| self.entries[v].selected && v != cur).map(|(p, _)| p).collect();
        let now: HashSet<usize> = vis.iter().copied().filter(|&v| self.entries[v].selected || v == cur).collect();
        let kept = self.strip_ext.filter(|&(a, _)| a == cur).and_then(|(a, h)| {
            let (pa, ph) = (vis.iter().position(|&v| v == a)?, vis.iter().position(|&v| v == h)?);
            let expected: HashSet<usize> = span(pa, ph).map(|p| vis[p]).collect();
            (expected == now).then_some((pa, ph))
        });
        let (anchor, head) = kept.unwrap_or_else(|| start_state(cur_pos, &selected));
        let head = next_head(head, if forward { 1 } else { -1 }, vis.len());
        let range = span(anchor, head);
        for (p, &v) in vis.iter().enumerate() {
            self.entries[v].selected = range.contains(&p);
        }
        self.strip_ext = Some((vis[anchor], vis[head]));
        self.strip_follow = Some(vis[head]);
    }
}

/// New moving end after an arrow key: `d` = ±1 (←/→) or ±columns (↑/↓), clamped to the list.
pub fn next_head(head: usize, d: isize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    (head as isize + d).clamp(0, n as isize - 1) as usize
}

/// Positions between anchor and head (both included), in either direction.
pub fn span(anchor: usize, head: usize) -> std::ops::RangeInclusive<usize> {
    anchor.min(head)..=anchor.max(head)
}

/// Start of an extension without usable state: the anchor is the active photo, the moving end the selected photo
/// farthest from it (after a ⇧-click the range ends there), otherwise the anchor itself.
pub fn start_state(anchor: usize, selected: &[usize]) -> (usize, usize) {
    let head = selected.iter().copied().max_by_key(|&p| p.abs_diff(anchor)).unwrap_or(anchor);
    (anchor, head)
}

/// Layout of the grid in content coordinates (x from the left edge of the grid, y from the top of the content).
#[derive(Clone, Copy, Debug)]
pub struct GridGeometry {
    pub columns: usize,
    /// number of photos in the first section (all when there are no "similar" hits behind a separator row)
    pub first: usize,
    pub total: usize,
    /// separator row between full-text hits and similar photos
    pub separated: bool,
    pub cell: f32,
    /// row pitch (cell height plus the gap between rows)
    pub pitch: f32,
    /// pitch of the (low) separator row
    pub sep_pitch: f32,
}

impl GridGeometry {
    fn rows_first(&self) -> usize {
        self.first.div_ceil(self.columns.max(1))
    }

    pub fn rows(&self) -> usize {
        let c = self.columns.max(1);
        self.rows_first() + if self.separated { 1 + (self.total - self.first).div_ceil(c) } else { 0 }
    }

    /// First list position of a grid row (`None`: separator row).
    pub fn start_of(&self, row: usize) -> Option<usize> {
        let c = self.columns.max(1);
        let r1 = self.rows_first();
        if row < r1 {
            Some(row * c)
        } else if row == r1 && self.separated {
            None
        } else {
            Some(self.first + (row - r1 - 1) * c)
        }
    }

    fn layout(&self) -> crate::grid_jump::Rows {
        crate::grid_jump::Rows { first: self.rows_first(), separated: self.separated, pitch: self.pitch, sep_pitch: self.sep_pitch }
    }

    fn tile(&self, row: usize, col: usize) -> Rect {
        Rect::from_min_size(Pos2::new(col as f32 * self.cell, self.layout().top(row)), egui::vec2(self.cell, self.cell))
    }

    /// Position of the photo at a point (`None`: free space, gap between rows, separator row, behind the last photo).
    pub fn position_at(&self, p: Pos2) -> Option<usize> {
        if p.x < 0.0 || p.y < 0.0 || self.cell <= 0.0 || self.pitch <= 0.0 {
            return None;
        }
        let col = (p.x / self.cell).floor() as usize;
        let row = self.layout().row_at(p.y);
        if col >= self.columns || row >= self.rows() || !self.tile(row, col).contains(p) {
            return None;
        }
        let pos = self.start_of(row)? + col;
        let end = if row < self.rows_first() { self.first } else { self.total };
        (pos < end).then_some(pos)
    }

    /// Positions of all photos whose tile (without its 3 pt margin) the rectangle touches.
    pub fn touched(&self, band: Rect) -> Vec<usize> {
        let mut out = Vec::new();
        if self.columns == 0 || self.cell <= 0.0 || self.pitch <= 0.0 || self.total == 0 {
            return out;
        }
        let rows = self.rows();
        let r0 = self.layout().row_at(band.min.y);
        let r1 = self.layout().row_at(band.max.y).min(rows.saturating_sub(1));
        let c0 = (band.min.x / self.cell).floor().max(0.0) as usize;
        let c1 = ((band.max.x / self.cell).floor().max(0.0) as usize).min(self.columns - 1);
        for row in r0..=r1 {
            let Some(start) = self.start_of(row) else { continue };
            let end = if start < self.first { self.first } else { self.total };
            for col in c0..=c1 {
                let pos = start + col;
                if pos < end && self.tile(row, col).shrink(3.0).intersects(band) {
                    out.push(pos);
                }
            }
        }
        out
    }
}

/// How a rubber band combines with the selection that existed before it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandMode {
    Replace,
    /// ⇧: add
    Add,
    /// ⌘: toggle (touched photos that were selected leave, the others join)
    Toggle,
}

pub fn band_mode(shift: bool, cmd: bool) -> BandMode {
    if cmd {
        BandMode::Toggle
    } else if shift {
        BandMode::Add
    } else {
        BandMode::Replace
    }
}

/// Selection resulting from the band: `before` (as at the start of the band) combined with the touched photos.
pub fn band_result(before: &HashSet<i64>, touched: &[i64], mode: BandMode) -> HashSet<i64> {
    match mode {
        BandMode::Replace => touched.iter().copied().collect(),
        BandMode::Add => before.iter().chain(touched.iter()).copied().collect(),
        BandMode::Toggle => {
            let hit: HashSet<i64> = touched.iter().copied().collect();
            before.symmetric_difference(&hit).copied().collect()
        }
    }
}

/// Rubber band while the mouse button is held: start in content coordinates (scrolls with the content), mode and the
/// selection at the start.
#[derive(Clone, Debug)]
pub struct Band {
    pub origin: Pos2,
    /// current pointer position, screen coordinates
    pub pointer: Pos2,
    pub mode: BandMode,
    pub before: HashSet<i64>,
    /// moved far enough to count as a drag (otherwise it is a click on free space)
    pub moved: bool,
}

/// Distance the pointer must travel before a press on free space becomes a rubber band.
pub const BAND_THRESHOLD: f32 = 4.0;

/// Scroll speed (points per second) of the autoscroll when the pointer is outside of / near the edge of the view:
/// grows with the distance, capped so that it stays calm.
pub fn autoscroll_speed(pointer_y: f32, top: f32, bottom: f32) -> f32 {
    const ZONE: f32 = 28.0;
    const MAX: f32 = 700.0;
    if pointer_y < top + ZONE {
        -((top + ZONE - pointer_y) / ZONE).min(3.0) * MAX / 3.0
    } else if pointer_y > bottom - ZONE {
        ((pointer_y - (bottom - ZONE)) / ZONE).min(3.0) * MAX / 3.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(total: usize) -> GridGeometry {
        GridGeometry { columns: 4, first: total, total, separated: false, cell: 100.0, pitch: 108.0, sep_pitch: 108.0 }
    }

    #[test]
    fn head_stays_inside_the_list() {
        assert_eq!(next_head(0, -1, 10), 0);
        assert_eq!(next_head(9, 1, 10), 9);
        assert_eq!(next_head(2, 4, 10), 6);
        assert_eq!(next_head(2, -4, 10), 0);
        assert_eq!(next_head(8, 4, 10), 9);
        assert_eq!(next_head(0, 1, 0), 0);
    }

    #[test]
    fn extending_and_reversing_shrinks_back_to_the_anchor() {
        let anchor = 5;
        let mut head = anchor;
        head = next_head(head, 1, 20);
        assert_eq!(span(anchor, head), 5..=6);
        head = next_head(head, 1, 20);
        assert_eq!(span(anchor, head), 5..=7);
        head = next_head(head, -1, 20);
        head = next_head(head, -1, 20);
        assert_eq!(span(anchor, head), 5..=5);
        head = next_head(head, -1, 20);
        assert_eq!(span(anchor, head), 4..=5);
    }

    #[test]
    fn row_jump_adds_a_whole_row() {
        let (anchor, mut head) = (5, 5);
        head = next_head(head, 4, 30);
        assert_eq!(span(anchor, head).count(), 5);
        head = next_head(head, -4, 30);
        head = next_head(head, -4, 30);
        assert_eq!(span(anchor, head), 1..=5);
        // at the top edge the selection reaches the start
        head = next_head(head, -4, 30);
        assert_eq!(head, 0);
    }

    #[test]
    fn start_without_state_takes_the_far_end() {
        assert_eq!(start_state(3, &[]), (3, 3));
        assert_eq!(start_state(3, &[3, 4, 5, 6]), (3, 6));
        assert_eq!(start_state(6, &[3, 4, 5, 6]), (6, 3));
    }

    #[test]
    fn tile_lookup_needs_a_tile() {
        let g = geo(10);
        assert_eq!(g.position_at(Pos2::new(10.0, 10.0)), Some(0));
        assert_eq!(g.position_at(Pos2::new(350.0, 10.0)), Some(3));
        assert_eq!(g.position_at(Pos2::new(10.0, 120.0)), Some(4));
        // gap between the rows, right of the grid, behind the last photo, below the content
        assert_eq!(g.position_at(Pos2::new(10.0, 104.0)), None);
        assert_eq!(g.position_at(Pos2::new(410.0, 10.0)), None);
        assert_eq!(g.position_at(Pos2::new(250.0, 230.0)), None);
        assert_eq!(g.position_at(Pos2::new(10.0, 900.0)), None);
        assert_eq!(g.position_at(Pos2::new(-5.0, 10.0)), None);
    }

    #[test]
    fn band_touches_tiles_in_rows_and_columns() {
        let g = geo(10);
        // inside tile 0 only
        assert_eq!(g.touched(Rect::from_min_max(Pos2::new(20.0, 20.0), Pos2::new(60.0, 60.0))), vec![0]);
        // across the first two columns of the first two rows
        let t = g.touched(Rect::from_min_max(Pos2::new(50.0, 50.0), Pos2::new(150.0, 150.0)));
        assert_eq!(t, vec![0, 1, 4, 5]);
        // only the margin of the tile (3 pt) does not count
        assert!(g.touched(Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(2.0, 2.0))).is_empty());
        // beyond the last photo nothing is touched
        assert_eq!(g.touched(Rect::from_min_max(Pos2::new(0.0, 230.0), Pos2::new(400.0, 400.0))), vec![8, 9]);
        // reversed drag direction gives the same rectangle after normalising
        let r = Rect::from_two_pos(Pos2::new(150.0, 150.0), Pos2::new(50.0, 50.0));
        assert_eq!(g.touched(r), vec![0, 1, 4, 5]);
    }

    #[test]
    fn band_with_separator_row() {
        // 6 hits (rows 0 and 1), separator (row 2), then 5 similar ones from row 3
        let g = GridGeometry { columns: 4, first: 6, total: 11, separated: true, cell: 100.0, pitch: 108.0, sep_pitch: 108.0 };
        assert_eq!(g.rows(), 2 + 1 + 2);
        assert_eq!(g.start_of(1), Some(4));
        assert_eq!(g.start_of(2), None);
        assert_eq!(g.start_of(3), Some(6));
        let t = g.touched(Rect::from_min_max(Pos2::new(0.0, 100.0), Pos2::new(400.0, 500.0)));
        assert_eq!(t, vec![4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(g.position_at(Pos2::new(10.0, 2.0 * 108.0 + 10.0)), None);
        assert_eq!(g.position_at(Pos2::new(10.0, 3.0 * 108.0 + 10.0)), Some(6));
        // the empty cells behind the last hit of the first section
        assert_eq!(g.position_at(Pos2::new(250.0, 108.0 + 10.0)), None);
    }

    #[test]
    fn band_modes_combine_with_the_old_selection() {
        let before: HashSet<i64> = [1, 2, 3].into_iter().collect();
        let hit = [3, 4];
        assert_eq!(band_result(&before, &hit, BandMode::Replace), [3, 4].into_iter().collect());
        assert_eq!(band_result(&before, &hit, BandMode::Add), [1, 2, 3, 4].into_iter().collect());
        assert_eq!(band_result(&before, &hit, BandMode::Toggle), [1, 2, 4].into_iter().collect());
        assert_eq!(band_mode(false, false), BandMode::Replace);
        assert_eq!(band_mode(true, false), BandMode::Add);
        assert_eq!(band_mode(true, true), BandMode::Toggle);
    }

    #[test]
    fn autoscroll_only_at_the_edges() {
        assert_eq!(autoscroll_speed(300.0, 0.0, 600.0), 0.0);
        assert!(autoscroll_speed(5.0, 0.0, 600.0) < 0.0);
        assert!(autoscroll_speed(700.0, 0.0, 600.0) > 0.0);
        assert!(autoscroll_speed(-500.0, 0.0, 600.0).abs() <= 700.0);
    }
}
