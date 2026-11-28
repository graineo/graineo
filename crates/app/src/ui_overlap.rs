//! Test hook `GRAINEO_UI_OVERLAP=1`: finds clickable widgets whose hit areas partly overlap (one neither contains
//! the other) in the same layer of the last frame, logs each pair once (stderr, prefix `ui-overlap:`) and outlines
//! both in red on top of the window, so screenshots (`GRAINEO_SCREENSHOT`) show where. A row that contains a cell
//! (section header with its reset icon) is fine; two neighbours sharing a strip (switch and reset icon, switch over
//! a value field) are what this catches. Strips up to 8 pt thin (panel edges, resize handles, scroll bars) are skipped.
//!
//! Painted areas count as well: a widget whose painted rect is larger than its hit area is checked with the painted
//! rect, and fills painted by hand (hover fill of a switch cell, the field behind a slider value, see [`fill`]) are
//! checked against every other clickable widget of their layer. A fill may lie inside a larger clickable row (row
//! highlight), but must not touch a foreign hit area otherwise, even when it covers it completely (a value field's
//! hover fill over a switch).

use std::collections::HashMap;
use std::sync::Mutex;

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::env::var("GRAINEO_UI_OVERLAP").is_ok_and(|v| v == "1"))
}

/// Overlap of `a` and `b` that counts: both ways partial and larger than a hairline.
fn partial(a: egui::Rect, b: egui::Rect) -> Option<egui::Rect> {
    let i = a.intersect(b);
    if !i.is_positive() || i.width() < 1.5 || i.height() < 1.5 {
        return None;
    }
    let contains = |o: egui::Rect, n: egui::Rect| o.expand(0.5).contains_rect(n);
    if contains(a, b) || contains(b, a) {
        return None;
    }
    Some(i)
}

/// Fills painted by hand in this frame: (layer, painted rect, id of the widget that paints it).
static FILLS: Mutex<Vec<(egui::LayerId, egui::Rect, egui::Id)>> = Mutex::new(Vec::new());

/// Register a fill (hover or field background) that `owner` paints at `rect` in `layer` (clipped to what is visible);
/// no-op unless the hook is on.
pub fn fill(layer: egui::LayerId, rect: egui::Rect, owner: egui::Id) {
    if enabled() {
        FILLS.lock().unwrap().push((layer, rect, owner));
    }
}

/// Does the fill `f` collide with the foreign hit area `b`? Inside a larger row is fine, any other touch is not.
fn fill_hits(f: egui::Rect, b: egui::Rect) -> bool {
    let i = f.intersect(b);
    if !i.is_positive() || i.width() < 1.5 || i.height() < 1.5 {
        return false;
    }
    !b.expand(0.5).contains_rect(f)
}

/// Painted area of a widget beyond its hit area: only a uniform growth (egui's hover expansion) counts; a hit area
/// cut on some sides is clipped by a scroll area or a sticky header, where the painted rect is hidden anyway.
fn painted(rect: egui::Rect, hit: egui::Rect) -> egui::Rect {
    let d = [hit.min.x - rect.min.x, hit.min.y - rect.min.y, rect.max.x - hit.max.x, rect.max.y - hit.max.y];
    let (lo, hi) = d.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if lo > 0.0 && hi - lo < 0.5 {
        rect
    } else {
        hit
    }
}

/// Call once per frame (after the UI is built): checks the widgets of the previous pass.
pub fn step(ctx: &egui::Context) {
    if !enabled() {
        return;
    }
    static SEEN: Mutex<Option<HashMap<(i32, i32, i32, i32, i32, i32, i32, i32), u32>>> = Mutex::new(None);
    let fills = std::mem::take(&mut *FILLS.lock().unwrap());
    let found: Vec<(egui::LayerId, egui::Rect, egui::Rect)> = ctx.viewport(|v| {
        let w = &v.prev_pass.widgets;
        let mut out = Vec::new();
        for layer in w.layer_ids() {
            // (id, hit area, painted area)
            let items: Vec<_> = w
                .get_layer(layer)
                .filter(|r| r.visible && r.enabled && (r.sense.senses_click() || r.sense.senses_drag()))
                .map(|r| (r.id, r.interact_rect, painted(r.rect, r.interact_rect)))
                .filter(|(_, r, _)| r.is_positive())
                // thin strips are panel edges, resize handles and scroll bars: they overlay rows on purpose
                .filter(|(_, r, _)| r.width().min(r.height()) > 8.0)
                .collect();
            for (i, (_, a, pa)) in items.iter().enumerate() {
                for (_, b, pb) in &items[i + 1..] {
                    // hit areas, and a painted area that reaches beyond the widget's hit area
                    if partial(*a, *b).is_some() || partial(a.union(*pa), *b).is_some() || partial(*a, b.union(*pb)).is_some() {
                        out.push((layer, *a, *b));
                    }
                }
            }
            for (fl, f, owner) in &fills {
                if *fl != layer {
                    continue;
                }
                for (id, b, _) in &items {
                    if id != owner && fill_hits(*f, *b) {
                        out.push((layer, *f, *b));
                    }
                }
            }
        }
        out
    });
    if found.is_empty() {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::new("ui-overlap")));
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(HashMap::new);
    for (layer, a, b) in found {
        let key = |r: egui::Rect| (r.min.x.round() as i32, r.min.y.round() as i32, r.max.x.round() as i32, r.max.y.round() as i32);
        let (ka, kb) = (key(a), key(b));
        // only pairs that stay for a while: the first frames of a view lay out with estimated widths
        let n = seen.entry((ka.0, ka.1, ka.2, ka.3, kb.0, kb.1, kb.2, kb.3)).or_insert(0);
        *n += 1;
        if *n == 10 {
            eprintln!("ui-overlap: {:?} {:?} x {:?}", layer.order, a, b);
        }
        for r in [a, b] {
            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::RED), egui::StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x0: f32, y0: f32, x1: f32, y1: f32) -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
    }

    #[test]
    fn fill_over_switch_is_found() {
        // 1.5.6: value field 60 × 17 at the right end, switch hit area inside it
        let field = r(200.0, 10.0, 260.0, 27.0).expand(1.0);
        assert!(fill_hits(field, r(206.0, 9.0, 238.0, 29.0)));
        // switch completely covered by the fill
        assert!(fill_hits(field, r(210.0, 12.0, 230.0, 25.0)));
        // fill inside a row highlight is fine, neighbours that only share an edge too
        assert!(!fill_hits(field, r(0.0, 0.0, 300.0, 40.0)));
        assert!(!fill_hits(r(150.0, 10.0, 182.0, 27.0), r(182.0, 10.0, 260.0, 27.0)));
    }
}
