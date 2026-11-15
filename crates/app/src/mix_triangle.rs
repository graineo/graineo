//! Mixing triangle for the B&W mix of the Foveon layers: a point in the triangle sets all three shares (sum always 1).
//! Corners = one layer only, side midpoints = two layers at half each, centre = all three equal. The point snaps to these spots and to
//! the sides; with ⌥ held it moves freely. Double click sets the centre.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke};

use crate::lr;

/// Snap radius around notable points and at the sides (screen points).
const SNAP_POINT: f32 = 11.0;
const SNAP_SIDE: f32 = 6.0;

/// Notable spots (shares bottom, middle, top): corners, side midpoints, centre.
pub const MARKS: [[f32; 3]; 7] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.5, 0.5, 0.0],
    [0.0, 0.5, 0.5],
    [0.5, 0.0, 0.5],
    [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0],
];

/// Corners on screen: bottom (red) lower left, middle (green) lower right, top (blue) at the top.
fn corners(r: Rect) -> [Pos2; 3] {
    [r.left_bottom(), r.right_bottom(), Pos2::new(r.center().x, r.top())]
}

fn point(e: &[Pos2; 3], m: [f32; 3]) -> Pos2 {
    Pos2::new(m[0] * e[0].x + m[1] * e[1].x + m[2] * e[2].x, m[0] * e[0].y + m[1] * e[1].y + m[2] * e[2].y)
}

/// Screen point → shares, clamped to the triangle.
fn weights(e: &[Pos2; 3], p: Pos2) -> [f32; 3] {
    let c = ((e[0].y - p.y) / (e[0].y - e[2].y)).clamp(0.0, 1.0);
    let b = (p.x - e[0].x - c * (e[2].x - e[0].x)) / (e[1].x - e[0].x);
    let b = b.clamp(0.0, 1.0 - c);
    [1.0 - b - c, b, c]
}

/// Snapping: first to notable points, then to the sides (one layer = 0).
fn snap(e: &[Pos2; 3], m: [f32; 3]) -> ([f32; 3], Option<usize>) {
    let p = point(e, m);
    if let Some((i, _)) = MARKS.iter().enumerate().map(|(i, k)| (i, point(e, *k).distance(p))).filter(|(_, d)| *d < SNAP_POINT).min_by(|a, b| a.1.total_cmp(&b.1)) {
        return (MARKS[i], Some(i));
    }
    // Distance to the side opposite corner k: share k × height of the triangle
    let height = e[0].y - e[2].y;
    let mut m = m;
    if let Some(k) = (0..3).filter(|&k| m[k] * height < SNAP_SIDE).min_by(|&a, &b| m[a].total_cmp(&m[b])) {
        let rest = 1.0 - m[k];
        m[k] = 0.0;
        if rest > 1e-6 {
            for (j, v) in m.iter_mut().enumerate() {
                if j != k {
                    *v /= rest;
                }
            }
        }
    }
    (m, None)
}

/// Draws the triangle and handles input. Returns: changed.
pub fn show(ui: &mut egui::Ui, mix: &mut [f32; 3]) -> bool {
    let border = egui::vec2(30.0, 20.0);
    let side = (ui.available_width().min(250.0) - 2.0 * border.x).max(80.0);
    let size = egui::vec2(side, side * 0.866);
    let (outer, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), size.y + 2.0 * border.y), egui::Sense::click_and_drag());
    let inner = Rect::from_center_size(outer.center(), size);
    let e = corners(inner);
    let painter = ui.painter_at(outer);

    // Area with the colors of the layers in the corners
    let colors = [Color32::from_rgb(0x6e, 0x2c, 0x28), Color32::from_rgb(0x2c, 0x5e, 0x32), Color32::from_rgb(0x2c, 0x40, 0x78)];
    let mut mesh = egui::Mesh::default();
    for (p, f) in e.iter().zip(colors) {
        mesh.colored_vertex(*p, f);
    }
    mesh.add_triangle(0, 1, 2);
    painter.add(egui::Shape::mesh(mesh));
    // Guide lines at 25/50/75 % per layer
    let line = Stroke::new(1.0, Color32::from_white_alpha(28));
    for k in 0..3 {
        for t in [0.25f32, 0.5, 0.75] {
            let (j, l) = ((k + 1) % 3, (k + 2) % 3);
            let mut a = [0.0; 3];
            let mut b = [0.0; 3];
            a[k] = t;
            a[j] = 1.0 - t;
            b[k] = t;
            b[l] = 1.0 - t;
            painter.line_segment([point(&e, a), point(&e, b)], line);
        }
    }
    painter.add(egui::Shape::closed_line(e.to_vec(), Stroke::new(1.2, Color32::from_gray(0x8a))));

    let mut changed = false;
    let free = ui.input(|i| i.modifiers.alt);
    if resp.double_clicked() {
        *mix = MARKS[6];
        changed = true;
    } else if resp.is_pointer_button_down_on() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let m = weights(&e, p);
            let m = if free { m } else { snap(&e, m).0 };
            let m = m.map(|v| (v * 1000.0).round() / 1000.0);
            let m = { let s: f32 = m.iter().sum(); m.map(|v| v / s) };
            if m != *mix {
                *mix = m;
                changed = true;
            }
        }
    }

    // Marks: active (point lies on it) bright, under the pointer slightly larger
    let pointer = resp.hover_pos();
    let current = point(&e, *mix);
    for k in MARKS {
        let q = point(&e, k);
        let on_target = q.distance(current) < 1.0;
        let near = pointer.is_some_and(|z| z.distance(q) < SNAP_POINT);
        let r = if near { 4.5 } else { 3.0 };
        painter.circle(q, r, if on_target { Color32::from_gray(0xe8) } else { Color32::from_black_alpha(120) }, Stroke::new(1.0, Color32::from_gray(if near { 0xe8 } else { 0x9a })));
    }
    // current point
    painter.circle_stroke(current, 8.0, Stroke::new(3.0, Color32::from_black_alpha(160)));
    painter.circle_stroke(current, 8.0, Stroke::new(1.6, Color32::WHITE));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }

    // Corner labels with the shares in percent
    let font = egui::TextStyle::Small.resolve(ui.style());
    let pct = |v: f32| format!("{:.0} %", v * 100.0);
    let names = [crate::tr("Unten"), crate::tr("Mitte"), crate::tr("Oben")];
    let targets = [
        (e[0] + egui::vec2(-4.0, 6.0), egui::Align2::LEFT_TOP),
        (e[1] + egui::vec2(4.0, 6.0), egui::Align2::RIGHT_TOP),
        (e[2] + egui::vec2(10.0, -2.0), egui::Align2::LEFT_TOP),
    ];
    for (i, (p, a)) in targets.iter().enumerate() {
        painter.text(*p, *a, format!("{} {}", names[i].to_uppercase(), pct(mix[i])), font.clone(), [lr::GRADIENT_RED, lr::GRADIENT_GREEN, lr::GRADIENT_BLUE][i][1]);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Rect {
        Rect::from_min_size(Pos2::new(10.0, 10.0), egui::vec2(200.0, 173.2))
    }

    #[test]
    fn point_and_weights_match() {
        let e = corners(r());
        for m in [[0.2, 0.3, 0.5], [1.0, 0.0, 0.0], [0.1, 0.8, 0.1], [1.0 / 3.0; 3]] {
            let a = weights(&e, point(&e, m));
            for k in 0..3 {
                assert!((a[k] - m[k]).abs() < 1e-3, "{m:?} → {a:?}");
            }
        }
        // outside: clamped to the triangle, sum 1, nothing negative
        let a = weights(&e, Pos2::new(-100.0, 400.0));
        assert!(a.iter().all(|v| *v >= 0.0) && (a.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn snaps_to_center_and_sides() {
        let e = corners(r());
        let (m, i) = snap(&e, [0.34, 0.33, 0.33]);
        assert_eq!((m, i), (MARKS[6], Some(6)));
        let (m, i) = snap(&e, [0.6, 0.38, 0.02]);
        assert!(i.is_none() && m[2] == 0.0 && (m[0] + m[1] - 1.0).abs() < 1e-5, "{m:?}");
        let (m, _) = snap(&e, [0.4, 0.3, 0.3]);
        assert_eq!(m, [0.4, 0.3, 0.3]);
    }
}
