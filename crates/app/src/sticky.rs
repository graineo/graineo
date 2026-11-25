//! Sticky headers: in long, scrollable lists with section or category headers, the header that the
//! visible area belongs to stays at the top until the next header approaches from below and pushes it up
//! out of view (as in lists on the iPhone).
//!
//! Usage: wrap the content of the ScrollArea in [`scope`]. Headers register themselves while drawing with
//! [`register`] (`lr::section*`, `lr::category` and the package header of the Film & Look list do this on their own);
//! outside a `scope` this costs nothing. At the end of the area `scope` draws the sticky header as an
//! overlay into the visible area. The positions come from the same frame, so there is no
//! delay, and the list itself does not change (no layout jumps).
//!
//! The pure selection computation ([`pick`]) only knows the top edge and height of the headers and is tested.

use eframe::egui;
use egui::{Color32, Id, Pos2, Rect};
use std::cell::RefCell;

/// Header of a group in the content of the list: top edge and height in the same coordinate axis as the view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Head {
    pub top: f32,
    pub height: f32,
}

/// Result of the selection: which header sticks and how far the next one pushes it up (≤ 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stuck {
    pub index: usize,
    pub shift: f32,
}

/// Which header sticks at the top edge `view_top` of the view? `heads` in drawing order (top to bottom).
/// No header sticks while the first one is still fully visible (no duplicate); otherwise the last header above the
/// view. If the next header moves up into its area, the sticky one is shifted up by the overlap.
pub fn pick(heads: &[Head], view_top: f32) -> Option<Stuck> {
    // half a pixel of tolerance: a header exactly at the edge counts as visible
    let index = heads.iter().rposition(|h| h.top < view_top - 0.5)?;
    let h = heads[index].height.max(0.0);
    let shift = match heads.get(index + 1) {
        Some(next) => (next.top - (view_top + h)).clamp(-h, 0.0),
        None => 0.0,
    };
    Some(Stuck { index, shift })
}

/// Ranks: where sections exist, these stick (the category there is only a thin line above);
/// if there are only categories (Film & Look), the categories stick.
pub const RANK_CATEGORY: u8 = 0;
pub const RANK_SECTION: u8 = 1;

type Paint = Box<dyn FnOnce(&egui::Painter, Rect, bool)>;

struct Entry {
    rank: u8,
    id: Id,
    left: f32,
    right: f32,
    head: Head,
    paint: Paint,
}

struct Collect {
    entries: Vec<Entry>,
    /// > 0: we are currently drawing the content of a section (nested headers do not stick)
    depth: usize,
}

thread_local!(static STACK: RefCell<Vec<Collect>> = const { RefCell::new(Vec::new()) });

/// A header registers. `bar` is the band of the header (full width), `id` identifies it for clicks.
/// `paint(painter, band, hover)` draws the header into the given band (without background).
pub fn register(rank: u8, id: Id, bar: Rect, paint: impl FnOnce(&egui::Painter, Rect, bool) + 'static) {
    STACK.with(|s| {
        if let Some(c) = s.borrow_mut().last_mut() {
            if c.depth == 0 {
                c.entries.push(Entry { rank, id, left: bar.left(), right: bar.right(), head: Head { top: bar.top(), height: bar.height() }, paint: Box::new(paint) });
            }
        }
    });
}

/// The content of a section begins/ends (headers registered there do not stick).
pub fn enter() {
    STACK.with(|s| {
        if let Some(c) = s.borrow_mut().last_mut() {
            c.depth += 1;
        }
    });
}

pub fn leave() {
    STACK.with(|s| {
        if let Some(c) = s.borrow_mut().last_mut() {
            c.depth = c.depth.saturating_sub(1);
        }
    });
}

fn click_slot() -> Id {
    Id::new("sticky-head-click")
}

/// Did a click on the sticky header `id` trigger the real header? The real header treats it like
/// a click of its own (once, it is picked up in the process).
pub fn take_click(ui: &egui::Ui, id: Id) -> bool {
    ui.ctx().data_mut(|d| {
        if d.get_temp::<Id>(click_slot()) == Some(id) {
            d.remove::<Id>(click_slot());
            true
        } else {
            false
        }
    })
}

/// Wrap the content of a ScrollArea: collects the headers and draws the sticky one at the end.
pub fn scope<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    STACK.with(|s| s.borrow_mut().push(Collect { entries: Vec::new(), depth: 0 }));
    let result = add(ui);
    let collect = STACK.with(|s| s.borrow_mut().pop());
    if let Some(c) = collect {
        paint_stuck(ui, c.entries);
    }
    result
}

fn paint_stuck(ui: &mut egui::Ui, mut entries: Vec<Entry>) {
    let Some(rank) = entries.iter().map(|e| e.rank).max() else { return };
    entries.retain(|e| e.rank == rank);
    let view = ui.clip_rect();
    let heads: Vec<Head> = entries.iter().map(|e| e.head).collect();
    let Some(stuck) = pick(&heads, view.top()) else { return };
    if stuck.shift <= -entries[stuck.index].head.height {
        return;
    }
    let Entry { id, left, right, head, paint, .. } = entries.swap_remove(stuck.index);
    let bar = Rect::from_min_max(Pos2::new(left, view.top() + stuck.shift), Pos2::new(right, view.top() + stuck.shift + head.height));
    let clip = bar.intersect(view);
    if !clip.is_positive() {
        return;
    }
    // Clicks are caught by the overlay (nothing underneath reacts); the real header carries them out
    let resp = ui.interact(clip, id.with("sticky"), egui::Sense::click());
    let painter = ui.painter().with_clip_rect(clip);
    painter.rect_filled(bar, 0.0, ui.visuals().panel_fill);
    paint(&painter, bar, resp.hovered());
    // thin divider line and small shadow: content passes underneath
    let shadow = ui.painter().with_clip_rect(Rect::from_min_max(clip.left_bottom(), Pos2::new(clip.right(), clip.bottom() + 5.0)).intersect(view));
    painter.line_segment([Pos2::new(bar.left(), bar.bottom() - 0.5), Pos2::new(bar.right(), bar.bottom() - 0.5)], egui::Stroke::new(1.0, crate::lr::gray(0x2c)));
    for (i, alpha) in [46u8, 26, 12, 4].into_iter().enumerate() {
        let y = bar.bottom() + i as f32;
        shadow.line_segment([Pos2::new(bar.left(), y + 0.5), Pos2::new(bar.right(), y + 0.5)], egui::Stroke::new(1.0, Color32::from_black_alpha(alpha)));
    }
    if resp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(click_slot(), id));
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(tops: &[f32]) -> Vec<Head> {
        tops.iter().map(|&top| Head { top, height: 28.0 }).collect()
    }

    #[test]
    fn nothing_sticks_while_the_first_head_is_fully_visible() {
        let h = heads(&[0.0, 300.0, 600.0]);
        assert_eq!(pick(&h, 0.0), None);
        // exactly at the edge still counts as visible
        assert_eq!(pick(&h, 0.3), None);
    }

    #[test]
    fn the_head_above_the_view_sticks() {
        let h = heads(&[0.0, 300.0, 600.0]);
        assert_eq!(pick(&h, 10.0), Some(Stuck { index: 0, shift: 0.0 }));
        assert_eq!(pick(&h, 299.0 - 28.0 - 1.0), Some(Stuck { index: 0, shift: 0.0 }));
        assert_eq!(pick(&h, 350.0), Some(Stuck { index: 1, shift: 0.0 }));
        assert_eq!(pick(&h, 5000.0), Some(Stuck { index: 2, shift: 0.0 }));
    }

    #[test]
    fn the_next_head_pushes_the_stuck_one_out() {
        let h = heads(&[0.0, 300.0]);
        // at top edge 282 the sticky one reaches to 310, the next starts at 300: 10 points of overlap
        assert_eq!(pick(&h, 282.0), Some(Stuck { index: 0, shift: -10.0 }));
        // the next header is exactly at the edge of the sticky one: no shift yet
        assert_eq!(pick(&h, 272.0), Some(Stuck { index: 0, shift: 0.0 }));
        // pushed out completely, never further than the height
        assert_eq!(pick(&h, 299.0), Some(Stuck { index: 0, shift: -27.0 }));
        // as soon as the next one reaches the top edge, it takes over
        assert_eq!(pick(&h, 301.0), Some(Stuck { index: 1, shift: 0.0 }));
    }

    #[test]
    fn push_is_continuous_at_the_hand_over() {
        // shortly before the handover the old header is almost completely out, after that the new one is at the top
        let h = heads(&[0.0, 300.0]);
        let before = pick(&h, 299.4).unwrap();
        assert_eq!(before.index, 0);
        assert!(before.shift > -28.0 && before.shift < -26.0);
        assert_eq!(pick(&h, 300.6).unwrap(), Stuck { index: 1, shift: 0.0 });
    }

    #[test]
    fn edge_cases() {
        // no headers
        assert_eq!(pick(&[], 100.0), None);
        // a single section
        let one = heads(&[0.0]);
        assert_eq!(pick(&one, 0.0), None);
        assert_eq!(pick(&one, 1000.0), Some(Stuck { index: 0, shift: 0.0 }));
        // empty sections: headers directly one behind the other
        let empty = heads(&[0.0, 28.0, 56.0, 84.0]);
        assert_eq!(pick(&empty, 30.0), Some(Stuck { index: 1, shift: -2.0 }));
        assert_eq!(pick(&empty, 40.0), Some(Stuck { index: 1, shift: -12.0 }));
        // collapsed sections (headers only, 28 high) and an open one in between
        let mixed = heads(&[0.0, 28.0, 56.0, 700.0]);
        assert_eq!(pick(&mixed, 400.0), Some(Stuck { index: 2, shift: 0.0 }));
        // section taller than the view: keeps sticking until the next header comes
        assert_eq!(pick(&mixed, 690.0), Some(Stuck { index: 2, shift: -18.0 }));
        // headers of different heights
        let uneven = [Head { top: 0.0, height: 18.0 }, Head { top: 100.0, height: 40.0 }];
        assert_eq!(pick(&uneven, 90.0), Some(Stuck { index: 0, shift: -8.0 }));
        assert_eq!(pick(&uneven, 150.0), Some(Stuck { index: 1, shift: 0.0 }));
    }
}
