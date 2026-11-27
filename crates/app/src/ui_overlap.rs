//! Test hook `GRAINEO_UI_OVERLAP=1`: finds clickable widgets whose hit areas partly overlap (one neither contains
//! the other) in the same layer of the last frame, logs each pair once (stderr, prefix `ui-overlap:`) and outlines
//! both in red on top of the window, so screenshots (`GRAINEO_SCREENSHOT`) show where. A row that contains a cell
//! (section header with its reset icon) is fine; two neighbours sharing a strip (switch and reset icon, switch over
//! a value field) are what this catches. Strips up to 8 pt thin (panel edges, resize handles, scroll bars) are skipped.

use std::collections::HashMap;
use std::sync::Mutex;

fn enabled() -> bool {
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

/// Call once per frame (after the UI is built): checks the widgets of the previous pass.
pub fn step(ctx: &egui::Context) {
    if !enabled() {
        return;
    }
    static SEEN: Mutex<Option<HashMap<(i32, i32, i32, i32, i32, i32, i32, i32), u32>>> = Mutex::new(None);
    let found: Vec<(egui::LayerId, egui::Rect, egui::Rect)> = ctx.viewport(|v| {
        let w = &v.prev_pass.widgets;
        let mut out = Vec::new();
        for layer in w.layer_ids() {
            let items: Vec<_> = w
                .get_layer(layer)
                .filter(|r| r.visible && r.enabled && (r.sense.senses_click() || r.sense.senses_drag()))
                .map(|r| r.interact_rect)
                .filter(|r| r.is_positive())
                // thin strips are panel edges, resize handles and scroll bars: they overlay rows on purpose
                .filter(|r| r.width().min(r.height()) > 8.0)
                .collect();
            for (i, a) in items.iter().enumerate() {
                for b in &items[i + 1..] {
                    if partial(*a, *b).is_some() {
                        out.push((layer, *a, *b));
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
