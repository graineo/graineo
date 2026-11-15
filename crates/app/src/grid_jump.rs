//! Geometry of the library grid: rows of equal height for the full-text hits, then (when there are any) a low
//! separator row "Similar in meaning" and the similar photos. Where the active photo lies, whether it is in view, the
//! scroll position that puts it in the middle, and a variant of `ScrollArea::show_rows` for the low separator row.

use eframe::egui;

/// Height of the separator row "Similar in meaning" (heading, line, count and strictness slider):
/// formerly as high as a photo row, which left a large empty area below the slider.
pub const SEP_H: f32 = 70.0;

/// Vertical layout of the grid rows (content coordinates).
#[derive(Clone, Copy, Debug)]
pub struct Rows {
    /// number of rows of the full-text hits (the separator row follows when `separated`)
    pub first: usize,
    pub separated: bool,
    /// row pitch: cell height plus the gap between rows
    pub pitch: f32,
    /// pitch of the separator row ([`SEP_H`] plus the gap)
    pub sep_pitch: f32,
}

impl Rows {
    /// Layout for `first` full-text hits in `columns` columns.
    pub fn new(first: usize, columns: usize, separated: bool, pitch: f32, gap: f32) -> Rows {
        Rows { first: first.div_ceil(columns.max(1)), separated, pitch, sep_pitch: SEP_H + gap }
    }

    /// Without separator: every row has the same pitch.
    pub fn uniform(pitch: f32) -> Rows {
        Rows { first: usize::MAX, separated: false, pitch, sep_pitch: pitch }
    }

    /// Top of a row.
    pub fn top(&self, row: usize) -> f32 {
        if self.separated && row > self.first {
            self.first as f32 * self.pitch + self.sep_pitch + (row - self.first - 1) as f32 * self.pitch
        } else {
            row as f32 * self.pitch
        }
    }

    /// Pitch of a row (the separator row is low).
    pub fn pitch_of(&self, row: usize) -> f32 {
        if self.separated && row == self.first {
            self.sep_pitch
        } else {
            self.pitch
        }
    }

    /// Row whose band (row plus the gap below it) contains `y`.
    pub fn row_at(&self, y: f32) -> usize {
        if self.pitch <= 0.0 || y <= 0.0 {
            return 0;
        }
        let sep_top = self.first as f32 * self.pitch;
        if !self.separated || y < sep_top {
            (y / self.pitch).floor() as usize
        } else if y < sep_top + self.sep_pitch {
            self.first
        } else {
            self.first + 1 + ((y - sep_top - self.sep_pitch) / self.pitch).floor() as usize
        }
    }

    /// Height of the content with `rows` rows (without the gap after the last).
    pub fn height(&self, rows: usize, gap: f32) -> f32 {
        if rows == 0 {
            return 0.0;
        }
        (self.top(rows - 1) + self.pitch_of(rows - 1) - gap).max(0.0)
    }
}

/// Row of the grid for a position in the list. `first`: number of full-text hits (the separator row follows when
/// `separated`), all further positions come after it.
pub fn row_of(pos: usize, columns: usize, first: usize, separated: bool) -> usize {
    let columns = columns.max(1);
    if !separated || pos < first {
        pos / columns
    } else {
        first.div_ceil(columns) + 1 + (pos - first) / columns
    }
}

/// Is the whole cell (top `top`) inside the view (scroll position `y`, view height `view_h`)?
pub fn in_view(top: f32, cell: f32, y: f32, view_h: f32) -> bool {
    top >= y - 0.5 && top + cell <= y + view_h + 0.5
}

/// Scroll position that puts the cell (top `top`) in the middle of the view (never negative).
pub fn center_offset(top: f32, cell: f32, view_h: f32) -> f32 {
    (top - view_h / 2.0 + cell / 2.0).max(0.0)
}

/// Position of the first photo whose whole cell is in view (`n` photos, layout as in [`row_of`]); `None` when nothing is.
pub fn first_in_view(n: usize, columns: usize, first: usize, rows: &Rows, cell: f32, y: f32, view_h: f32) -> Option<usize> {
    if n == 0 || rows.pitch <= 0.0 {
        return None;
    }
    let columns = columns.max(1);
    // start a little above the first row that can be in view and walk down row by row
    let row0 = rows.row_at(y).saturating_sub(1);
    let rows_first = first.min(n).div_ceil(columns);
    let pos_of_row = |row: usize| -> Option<usize> {
        if !rows.separated || row < rows_first {
            Some(row * columns)
        } else if row == rows_first {
            None // separator row
        } else {
            Some(first + (row - rows_first - 1) * columns)
        }
    };
    for row in row0..row0 + 4 + (view_h / rows.pitch).ceil() as usize {
        let Some(pos) = pos_of_row(row) else { continue };
        if pos >= n {
            return None;
        }
        if in_view(rows.top(row), cell, y, view_h) {
            return Some(pos);
        }
    }
    None
}

/// Like `ScrollArea::show_rows`, but the separator row has its own (lower) height. The rows are added top-down by
/// `add_contents` with the usual item spacing; each must take exactly its height (photo rows `pitch − gap`, the
/// separator row [`SEP_H`]).
pub fn show_rows<R>(area: egui::ScrollArea, ui: &mut egui::Ui, rows: Rows, total: usize, add_contents: impl FnOnce(&mut egui::Ui, std::ops::Range<usize>) -> R) -> egui::scroll_area::ScrollAreaOutput<R> {
    let gap = ui.spacing().item_spacing.y;
    area.show_viewport(ui, |ui, viewport| {
        ui.set_height(rows.height(total, gap));
        let min_row = rows.row_at(viewport.min.y).min(total);
        let max_row = (rows.row_at(viewport.max.y) + 2).min(total);
        let y_min = ui.max_rect().top() + rows.top(min_row);
        let y_max = ui.max_rect().top() + if max_row > 0 { rows.top(max_row - 1) + rows.pitch_of(max_row - 1) } else { 0.0 };
        let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), y_min..=y_max.max(y_min));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |viewport_ui| {
            viewport_ui.skip_ahead_auto_ids(min_row);
            add_contents(viewport_ui, min_row..max_row)
        })
        .inner
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_photo_in_view() {
        let (pitch, cell, view_h) = (100.0, 98.0, 500.0);
        let flat = Rows::uniform(pitch);
        assert_eq!(first_in_view(100, 4, 100, &flat, cell, 0.0, view_h), Some(0));
        // scrolled to 250: row 2 is cut off at the top, row 3 is the first whole one
        assert_eq!(first_in_view(100, 4, 100, &flat, cell, 250.0, view_h), Some(12));
        assert_eq!(first_in_view(0, 4, 0, &flat, cell, 0.0, view_h), None);
        // with the separator: 10 hits (rows 0..2), separator row 3 (low), similar ones from row 4 (position 10)
        let sep = Rows::new(10, 4, true, pitch, 2.0);
        assert_eq!(first_in_view(30, 4, 10, &sep, cell, sep.top(4) - 1.0, view_h), Some(10));
        // scrolled beyond the end
        assert_eq!(first_in_view(8, 4, 8, &flat, cell, 5000.0, view_h), None);
    }

    #[test]
    fn rows_with_and_without_separator() {
        // 4 columns, 10 hits: rows 0..2 (3 rows), separator row 3, then the similar ones from row 4
        assert_eq!(row_of(0, 4, 10, true), 0);
        assert_eq!(row_of(9, 4, 10, true), 2);
        assert_eq!(row_of(10, 4, 10, true), 4);
        assert_eq!(row_of(14, 4, 10, true), 5);
        // without separator everything is one block
        assert_eq!(row_of(10, 4, 10, false), 2);
        assert_eq!(row_of(14, 4, 14, false), 3);
        assert_eq!(row_of(5, 0, 10, false), 5, "no division by zero");
    }

    /// The separator row is low, the similar photos follow right below it.
    #[test]
    fn low_separator_row() {
        let r = Rows::new(10, 4, true, 300.0, 2.0);
        assert_eq!(r.top(3), 900.0);
        assert_eq!(r.top(4), 900.0 + SEP_H + 2.0, "the first similar row starts right after the separator");
        assert_eq!(r.top(5), 900.0 + SEP_H + 2.0 + 300.0);
        for row in 0..8 {
            assert_eq!(r.row_at(r.top(row) + 0.5), row);
            assert_eq!(r.row_at(r.top(row) + r.pitch_of(row) - 0.5), row);
        }
        assert_eq!(r.height(6, 2.0), r.top(5) + 298.0);
        let flat = Rows::uniform(100.0);
        assert_eq!(flat.top(7), 700.0);
        assert_eq!(flat.row_at(750.0), 7);
    }

    #[test]
    fn visibility_and_target() {
        let (pitch, cell, view_h) = (100.0, 98.0, 500.0);
        let flat = Rows::uniform(pitch);
        assert!(in_view(flat.top(0), cell, 0.0, view_h));
        assert!(in_view(flat.top(4), cell, 0.0, view_h));
        assert!(!in_view(flat.top(5), cell, 0.0, view_h), "cut off at the bottom");
        assert!(!in_view(flat.top(1), cell, 150.0, view_h), "cut off at the top");
        // far below: centred; near the top: not below zero
        let y = center_offset(flat.top(40), cell, view_h);
        assert!(in_view(flat.top(40), cell, y, view_h));
        assert!((y + view_h / 2.0 - (40.0 * pitch + cell / 2.0)).abs() < 0.01);
        assert_eq!(center_offset(flat.top(1), cell, view_h), 0.0);
        // with the separator row between hit and "similar": the target is the row after it
        let sep = Rows::new(10, 4, true, pitch, 2.0);
        let row = row_of(12, 4, 10, true);
        assert_eq!(row, 4);
        assert!(in_view(sep.top(row), cell, center_offset(sep.top(row), cell, view_h), view_h));
    }
}
