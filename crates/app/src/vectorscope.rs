//! Vectorscope of the display at the top of the Edit and Masks trays: computation of the scope image (on a worker,
//! see [`crate::histogram_pin::request`]), drawing with saturation rings, crosshair, target marks, skin tone line and
//! the readout under the pointer, and its two options (zoom on the saturation, skin tone line), kept in
//! `settings.json`.
//!
//! Geometry: Cb/Cr per Rec. 709, Cb to the right, Cr upwards; a chroma of 0.5 reaches the rim at zoom ×1 (= 100 %
//! saturation in the readout). Red lies at the top, blue on the right, cyan at the bottom, yellow on the left.

use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{self, Color32, Stroke};

use crate::lr::{self, PANEL_DARK, TEXT, TEXT_DIM};

/// Key in `settings.json`: zoom on the saturation (1, 2 or 4).
pub const ZOOM_KEY: &str = "vectorscopeZoom";
/// Key in `settings.json`: skin tone line shown (default on).
pub const SKIN_KEY: &str = "vectorscopeSkinLine";
/// The zoom steps.
pub const ZOOMS: [u8; 3] = [1, 2, 4];
/// Side of the scope image in texels.
pub const N: usize = 256;
/// Direction of the skin tone line, degrees from the Cb axis (counter-clockwise): skin of every type lies close to it.
pub const SKIN_ANGLE: f32 = 123.0;

/// 0 = not read yet, otherwise value + 1.
static ZOOM: AtomicU8 = AtomicU8::new(0);
static SKIN: AtomicU8 = AtomicU8::new(0);

/// Zoom from a value of `settings.json` (missing or unknown: ×1).
pub fn zoom_from_value(v: &serde_json::Value) -> u8 {
    v.as_u64().and_then(|z| ZOOMS.iter().copied().find(|&s| s as u64 == z)).unwrap_or(1)
}

/// Skin tone line from a value of `settings.json` (missing: on).
pub fn skin_from_value(v: &serde_json::Value) -> bool {
    v.as_bool().unwrap_or(true)
}

/// The zoom step after `z` (×1 → ×2 → ×4 → ×1).
pub fn next_zoom(z: u8) -> u8 {
    let i = ZOOMS.iter().position(|&s| s == z).unwrap_or(0);
    ZOOMS[(i + 1) % ZOOMS.len()]
}

pub fn zoom() -> u8 {
    match ZOOM.load(Ordering::Relaxed) {
        0 => {
            let z = zoom_from_value(&library::settings::value(ZOOM_KEY));
            ZOOM.store(z + 1, Ordering::Relaxed);
            z
        }
        v => v - 1,
    }
}

pub fn set_zoom(z: u8) {
    ZOOM.store(z + 1, Ordering::Relaxed);
    library::settings::set(ZOOM_KEY, serde_json::json!(z));
}

pub fn skin_line() -> bool {
    match SKIN.load(Ordering::Relaxed) {
        0 => {
            let on = skin_from_value(&library::settings::value(SKIN_KEY));
            SKIN.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        v => v == 2,
    }
}

pub fn set_skin_line(on: bool) {
    SKIN.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    library::settings::set(SKIN_KEY, serde_json::json!(on));
}

/// Cb/Cr (Rec. 709) of a linear-free RGB triple in 0…1.
pub fn cb_cr(rgb: [f32; 3]) -> (f32, f32) {
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    ((rgb[2] - y) / 1.8556, (rgb[0] - y) / 1.5748)
}

/// Hue in degrees (0 red, 60 yellow, 120 green, 180 cyan, 240 blue, 300 magenta) of a point of the scope. Adding the
/// same amount to R, G and B changes neither hue nor Cb/Cr, so the hue follows from Cb/Cr alone.
pub fn hue_deg(cb: f32, cr: f32) -> f32 {
    let r = cr * 1.5748;
    let b = cb * 1.8556;
    let g = -(0.2126 * r + 0.0722 * b) / 0.7152;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d <= 1e-6 {
        return 0.0;
    }
    let h = if max == r {
        60.0 * ((g - b) / d)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    h.rem_euclid(360.0)
}

/// Saturation in percent of a point of the scope: 100 % = the rim at zoom ×1.
pub fn saturation_pct(cb: f32, cr: f32) -> f32 {
    cb.hypot(cr) / 0.5 * 100.0
}

/// Names of the hues every 30° (German = translation keys).
const HUES: [&str; 12] = ["Rot", "Orange", "Gelb", "Gelbgrün", "Grün", "Türkisgrün", "Cyan", "Azurblau", "Blau", "Violett", "Magenta", "Rosa"];

/// Hue name: the name of the nearest 30° step within ±7.5°, otherwise both neighbours ("Rot-Orange"). Untranslated
/// keys (index of one or two names).
pub fn hue_name_keys(deg: f32) -> (&'static str, Option<&'static str>) {
    let d = deg.rem_euclid(360.0);
    let step = (d / 30.0).round() as usize % 12;
    let off = d - (d / 30.0).round() * 30.0;
    if off.abs() <= 7.5 {
        return (HUES[step], None);
    }
    let lower = (d / 30.0).floor() as usize % 12;
    (HUES[lower], Some(HUES[(lower + 1) % 12]))
}

/// A colour name in the language of the program (literal keys, so the key list finds them).
fn color_name(key: &str) -> &'static str {
    match key {
        "Rot" => crate::tr("Rot"),
        "Orange" => crate::tr("Orange"),
        "Gelb" => crate::tr("Gelb"),
        "Gelbgrün" => crate::tr("Gelbgrün"),
        "Grün" => crate::tr("Grün"),
        "Türkisgrün" => crate::tr("Türkisgrün"),
        "Cyan" => crate::tr("Cyan"),
        "Azurblau" => crate::tr("Azurblau"),
        "Blau" => crate::tr("Blau"),
        "Violett" => crate::tr("Violett"),
        "Magenta" => crate::tr("Magenta"),
        _ => crate::tr("Rosa"),
    }
}

/// Hue name in the language of the program, e.g. "Rot-Orange".
pub fn hue_name(deg: f32) -> String {
    match hue_name_keys(deg) {
        (a, None) => color_name(a).to_string(),
        (a, Some(b)) => format!("{}-{}", color_name(a), color_name(b)),
    }
}

/// Saturation rings (percent) for a zoom: round values, all inside the rim.
pub fn ring_levels(zoom: u8) -> &'static [f32] {
    match zoom {
        4 => &[5.0, 10.0, 15.0, 20.0],
        2 => &[10.0, 20.0, 30.0, 40.0],
        _ => &[25.0, 50.0, 75.0],
    }
}

/// Radius of a saturation ring as a fraction of the circle's radius.
pub fn ring_radius(pct: f32, zoom: u8) -> f32 {
    pct / 100.0 * zoom as f32
}

/// The six target marks: German long name (translation key), German short name, RGB, colour of the label dot.
pub const TARGETS: [(&str, &str, [f32; 3], Color32); 6] = [
    ("Rot", "R", [1.0, 0.0, 0.0], Color32::from_rgb(0xe8, 0x50, 0x48)),
    ("Magenta", "Mg", [1.0, 0.0, 1.0], Color32::from_rgb(0xd8, 0x5c, 0xd8)),
    ("Blau", "B", [0.0, 0.0, 1.0], Color32::from_rgb(0x5c, 0x7c, 0xf0)),
    ("Cyan", "Cy", [0.0, 1.0, 1.0], Color32::from_rgb(0x48, 0xcc, 0xd8)),
    ("Grün", "G", [0.0, 1.0, 0.0], Color32::from_rgb(0x58, 0xc8, 0x60)),
    ("Gelb", "Gb", [1.0, 1.0, 0.0], Color32::from_rgb(0xe0, 0xc8, 0x40)),
];

/// Label of target `i` in the language of the program: the short form for the small display (R, G, B stay the
/// same in every language), otherwise the colour name.
pub fn target_label(i: usize, short: bool) -> String {
    let (long, abbr, ..) = TARGETS[i];
    if !short {
        return color_name(long).to_string();
    }
    match abbr {
        "Mg" => crate::tr("Mg").to_string(),
        "Cy" => crate::tr("Cy").to_string(),
        "Gb" => crate::tr("Gb").to_string(),
        s => s.to_string(),
    }
}

/// Position of a 75 % target in the scope (Cb, Cr).
pub fn target_point(i: usize) -> (f32, f32) {
    let (cb, cr) = cb_cr(TARGETS[i].2);
    (cb * 0.75, cr * 0.75)
}

/// The scope image of an 8-bit RGB picture: hue as angle, saturation as distance from the centre, brightness =
/// number of pixels (logarithmic, so that a few pixels stay visible), tinted with their own hue. `zoom` enlarges the
/// centre (saturation × zoom); points beyond the square are left out.
pub fn image(rgb: &[u8], zoom: u8) -> egui::ColorImage {
    let z = zoom.max(1) as f32;
    let mut counts = vec![0u32; N * N];
    let mut col = vec![[0f32; 3]; N * N];
    for p in rgb.chunks_exact(3) {
        let c = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
        let (cb, cr) = cb_cr(c);
        let x = ((cb * z + 0.5) * N as f32).floor() as isize;
        let y = ((0.5 - cr * z) * N as f32).floor() as isize;
        if (0..N as isize).contains(&x) && (0..N as isize).contains(&y) {
            let i = y as usize * N + x as usize;
            counts[i] += 1;
            col[i][0] += c[0];
            col[i][1] += c[1];
            col[i][2] += c[2];
        }
    }
    let top = density_reference(&counts);
    let pixels = counts
        .iter()
        .zip(&col)
        .map(|(&n, c)| {
            if n == 0 {
                return Color32::TRANSPARENT;
            }
            let v = density(n, top);
            // colour of the pixels, brought to full brightness so the hue is recognisable
            let m = (c[0].max(c[1]).max(c[2]) / n as f32).max(1e-3);
            let k = |x: f32| ((x / n as f32 / m) * 0.7 + 0.3).min(1.0) * v * 255.0;
            Color32::from_rgb(k(c[0]) as u8, k(c[1]) as u8, k(c[2]) as u8)
        })
        .collect();
    egui::ColorImage { size: [N, N], source_size: egui::vec2(N as f32, N as f32), pixels }
}

/// The count that is shown at full brightness: the 99th percentile of the occupied cells (a few very full cells,
/// e.g. a grey sky, do not darken the rest).
pub fn density_reference(counts: &[u32]) -> u32 {
    let mut used: Vec<u32> = counts.iter().copied().filter(|&n| n > 0).collect();
    if used.is_empty() {
        return 1;
    }
    let k = ((used.len() - 1) as f32 * 0.99).round() as usize;
    *used.select_nth_unstable(k).1
}

/// Brightness 0…1 of a cell with `n` pixels: logarithmic, at least 0.3 for a single pixel.
pub fn density(n: u32, top: u32) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let v = (1.0 + n as f32).ln() / (1.0 + top.max(1) as f32).ln();
    (0.3 + 0.7 * v).min(1.0)
}

/// Result of [`view`].
#[derive(Default)]
pub struct ViewResponse {
    /// Double click on the display (not on one of its buttons): next size
    pub double_clicked: bool,
    /// Zoom changed (the image must be computed again)
    pub zoom_changed: bool,
}

/// Small text button laid over the display; returns: clicked.
fn overlay_button(ui: &egui::Ui, rect: egui::Rect, id: egui::Id, label: &str, on: bool, tip: &str) -> bool {
    let r = ui.interact(rect, id, egui::Sense::click());
    let p = ui.painter();
    let fill = if r.hovered() { Color32::from_gray(0x2c) } else { Color32::from_black_alpha(150) };
    p.rect_filled(rect, 0.0, fill);
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(if on { 0x6a } else { 0x38 })), egui::StrokeKind::Inside);
    let color = if on || r.hovered() { TEXT() } else { Color32::from_gray(0x80) };
    p.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.0), color);
    lr::hover_tip(ui, &r, tip);
    r.clicked()
}

/// Draws the vectorscope in a field `height` points high and as wide as the tray. `tex` is the image of [`image`]
/// for the current zoom. `small`: the smallest size step (short labels, no ring labels).
pub fn view(ui: &mut egui::Ui, tex: Option<&egui::TextureHandle>, height: f32, small: bool) -> ViewResponse {
    let mut out = ViewResponse::default();
    let (outer, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    let z = zoom();
    let p = ui.painter_at(outer);
    p.rect_filled(outer, 0.0, PANEL_DARK);
    let d = (height.min(outer.width()) - 8.0).max(20.0);
    let sq = egui::Rect::from_center_size(outer.center(), egui::vec2(d, d));
    let c = sq.center();
    let r = d / 2.0;
    let at = |cb: f32, cr: f32| egui::pos2(sq.left() + (cb * z as f32 + 0.5) * d, sq.top() + (0.5 - cr * z as f32) * d);
    // rim, saturation rings and crosshair
    p.circle_stroke(c, r, Stroke::new(1.0, Color32::from_gray(0x44)));
    let label_font = egui::FontId::proportional(if small { 9.0 } else { 9.5 });
    for &pct in ring_levels(z) {
        let rr = r * ring_radius(pct, z);
        p.circle_stroke(c, rr, Stroke::new(1.0, Color32::from_gray(0x24)));
        if !small {
            // label in the lower left between yellow and green: photos rarely have colours there (sky and skin lie
            // on the other diagonal)
            let a = (-152f32).to_radians();
            p.text(c + egui::vec2(a.cos(), -a.sin()) * rr + egui::vec2(-2.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{pct:.0}"), label_font.clone(), Color32::from_gray(0x58));
        }
    }
    p.line_segment([c - egui::vec2(r, 0.0), c + egui::vec2(r, 0.0)], Stroke::new(1.0, Color32::from_gray(0x1e)));
    p.line_segment([c - egui::vec2(0.0, r), c + egui::vec2(0.0, r)], Stroke::new(1.0, Color32::from_gray(0x1e)));
    let skin = skin_line();
    let skin_color = Color32::from_rgb(0xe0, 0xa0, 0x80);
    if skin {
        let a = SKIN_ANGLE.to_radians();
        let dir = egui::vec2(a.cos(), -a.sin());
        p.line_segment([c, c + dir * r], Stroke::new(1.2, skin_color.gamma_multiply(0.55)));
    }
    if let Some(t) = tex {
        p.image(t.id(), sq, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    // centre mark on top of the cloud: neutral grey lies here
    let cm = Color32::from_gray(0x70);
    p.line_segment([c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)], Stroke::new(1.0, cm));
    p.line_segment([c - egui::vec2(0.0, 4.0), c + egui::vec2(0.0, 4.0)], Stroke::new(1.0, cm));
    if skin && !small {
        let a = SKIN_ANGLE.to_radians();
        let dir = egui::vec2(a.cos(), -a.sin());
        // to the right of the line, inside, clear of the red label
        p.text(c + dir * (r * 0.55) + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, crate::tr("Haut"), label_font.clone(), skin_color);
    }
    // target marks (75 %) with labels; with zoom they lie outside the circle: then a tick on the rim shows the direction
    let font = egui::FontId::proportional(if small { 10.0 } else { 11.0 });
    for (i, &(.., dot)) in TARGETS.iter().enumerate() {
        let (cb, cr) = target_point(i);
        let pos = at(cb, cr);
        let dir = (pos - c).normalized();
        let label = target_label(i, small);
        let label_at = if z == 1 {
            p.rect_stroke(egui::Rect::from_center_size(pos, egui::vec2(7.0, 7.0)), 0.0, Stroke::new(1.0, Color32::from_gray(0x70)), egui::StrokeKind::Middle);
            pos + egui::vec2(if dir.x < 0.0 { -7.0 } else { 7.0 }, 0.0)
        } else {
            p.line_segment([c + dir * (r - 5.0), c + dir * (r + 2.0)], Stroke::new(2.0, dot));
            c + dir * (r - 14.0)
        };
        // ×1: the label outward of the box; with zoom inward of the rim tick
        let align = match (z == 1, dir.x < 0.0) {
            (true, true) | (false, false) => egui::Align2::RIGHT_CENTER,
            _ => egui::Align2::LEFT_CENTER,
        };
        let g = p.layout_no_wrap(label, font.clone(), Color32::from_gray(0xc4));
        let text_rect = align.anchor_size(label_at, g.size() + egui::vec2(8.0, 0.0));
        // colour dot in front of the text, the text after it
        let dot_at = egui::pos2(text_rect.left() + 3.0, text_rect.center().y);
        p.circle_filled(dot_at, 2.6, dot);
        p.galley(egui::pos2(text_rect.left() + 8.0, text_rect.top()), g, Color32::from_gray(0xc4));
    }
    // options in the lower corners
    let bh = 16.0;
    let zoom_rect = egui::Rect::from_min_size(outer.left_bottom() + egui::vec2(4.0, -bh - 4.0), egui::vec2(26.0, bh));
    let zoom_tip = crate::tr("Zoom auf die Sättigung (×1, ×2, ×4): Fotos liegen meist nahe der Mitte");
    if overlay_button(ui, zoom_rect, resp.id.with("zoom"), &format!("×{z}"), z > 1, zoom_tip) {
        set_zoom(next_zoom(z));
        out.zoom_changed = true;
    }
    let skin_label = crate::tr("Haut");
    let skin_w = ui.painter().layout_no_wrap(skin_label.to_string(), egui::FontId::proportional(10.0), TEXT()).size().x + 10.0;
    let skin_rect = egui::Rect::from_min_size(outer.right_bottom() + egui::vec2(-skin_w - 4.0, -bh - 4.0), egui::vec2(skin_w, bh));
    if overlay_button(ui, skin_rect, resp.id.with("haut"), skin_label, skin, crate::tr("Hauttonlinie zeigen: Hauttöne aller Menschen liegen nahe daran")) {
        set_skin_line(!skin);
    }
    // readout under the pointer (or the test position of GRAINEO_VECTORSCOPE_POINTER="cb,cr")
    let probe = resp.hover_pos().filter(|q| sq.contains(*q)).map(|q| (((q.x - sq.left()) / d - 0.5) / z as f32, (0.5 - (q.y - sq.top()) / d) / z as f32)).or_else(test_pointer);
    if let Some((cb, cr)) = probe {
        if resp.hover_pos().is_none() {
            // test position: show where it is
            p.circle_stroke(at(cb, cr), 4.0, Stroke::new(1.0, Color32::WHITE));
        }
        let sat = saturation_pct(cb, cr);
        let line1 = if sat < 3.0 { crate::tr("Neutral (Grau)").to_string() } else { format!("{} · {:.0}°", hue_name(hue_deg(cb, cr)), hue_deg(cb, cr)) };
        let line2 = crate::trf!("Sättigung {s} %", s = format!("{sat:.0}"));
        let f = egui::FontId::proportional(10.5);
        let g1 = p.layout_no_wrap(line1, f.clone(), TEXT());
        let g2 = p.layout_no_wrap(line2, f, TEXT_DIM());
        let w = g1.size().x.max(g2.size().x) + 10.0;
        let box_rect = egui::Rect::from_min_size(outer.left_top() + egui::vec2(4.0, 4.0), egui::vec2(w, g1.size().y + g2.size().y + 6.0));
        p.rect_filled(box_rect, 0.0, Color32::from_black_alpha(190));
        let h1 = g1.size().y;
        p.galley(box_rect.left_top() + egui::vec2(5.0, 3.0), g1, TEXT());
        p.galley(box_rect.left_top() + egui::vec2(5.0, 3.0 + h1), g2, TEXT_DIM());
    }
    // the "?" card stays visible: the display needs an explanation for whoever sees it for the first time
    crate::help::question_mark(ui, outer.right_top() + egui::vec2(-10.0, 10.0), resp.id.with("hilfe"), true, "Vektorskop", "", &crate::help::VECTORSCOPE, "");
    lr::hover_tip(ui, &resp, crate::tr("Vektorskop: Farbton = Winkel, Sättigung = Abstand von der Mitte (Doppelklick: Größe)"));
    out.double_clicked = resp.double_clicked();
    out
}

/// GRAINEO_VECTORSCOPE_POINTER="cb,cr": readout at this point without a pointer (screenshots).
fn test_pointer() -> Option<(f32, f32)> {
    let s = crate::env::var("GRAINEO_VECTORSCOPE_POINTER").ok()?;
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primaries_have_their_hue() {
        for (i, want) in [0.0, 300.0, 240.0, 180.0, 120.0, 60.0].into_iter().enumerate() {
            let (cb, cr) = target_point(i);
            let h = hue_deg(cb, cr);
            let diff = (h - want + 180.0).rem_euclid(360.0) - 180.0;
            assert!(diff.abs() < 0.5, "{} → {h}", TARGETS[i].0);
        }
        // grey has no saturation, brightness does not change the point
        assert_eq!(cb_cr([0.5, 0.5, 0.5]), (0.0, 0.0));
        let (a, b) = cb_cr([0.8, 0.4, 0.2]);
        let (c, d) = cb_cr([0.7, 0.3, 0.1]);
        assert!((a - c).abs() < 1e-6 && (b - d).abs() < 1e-6);
    }

    #[test]
    fn the_layout_matches_the_usual_scope() {
        // red at the top, blue on the right, cyan at the bottom, yellow on the left
        let (rb, rr) = target_point(0);
        assert!(rr > 0.3 && rb.abs() < rr);
        let (bb, br) = target_point(2);
        assert!(bb > 0.3 && br.abs() < bb);
        let (cb, cr) = target_point(3);
        assert!(cr < -0.3 && cb.abs() < -cr);
        let (yb, yr) = target_point(5);
        assert!(yb < -0.3 && yr.abs() < -yb);
        // full yellow and blue reach the rim at ×1
        let (b, r) = cb_cr([0.0, 0.0, 1.0]);
        assert!((saturation_pct(b, r) - 100.0).abs() < 1.0);
    }

    #[test]
    fn skin_line_points_to_orange_red() {
        let a = SKIN_ANGLE.to_radians();
        let h = hue_deg(a.cos() * 0.2, a.sin() * 0.2);
        assert!((12.0..30.0).contains(&h), "{h}");
        assert_eq!(hue_name_keys(h), ("Rot", Some("Orange")));
    }

    #[test]
    fn hue_names_step_every_fifteen_degrees() {
        assert_eq!(hue_name_keys(0.0), ("Rot", None));
        assert_eq!(hue_name_keys(359.0), ("Rot", None));
        assert_eq!(hue_name_keys(12.0), ("Rot", Some("Orange")));
        assert_eq!(hue_name_keys(30.0), ("Orange", None));
        assert_eq!(hue_name_keys(60.0), ("Gelb", None));
        assert_eq!(hue_name_keys(345.0), ("Rosa", Some("Rot")));
        assert_eq!(hue_name_keys(240.0), ("Blau", None));
    }

    #[test]
    fn rings_stay_inside_and_are_round_values() {
        for z in ZOOMS {
            let levels = ring_levels(z);
            assert!(levels.len() >= 3);
            for (k, &pct) in levels.iter().enumerate() {
                let rr = ring_radius(pct, z);
                assert!(rr > 0.0 && rr < 1.0, "zoom {z}: {pct} → {rr}");
                assert_eq!(pct.fract(), 0.0);
                if k > 0 {
                    assert!(rr > ring_radius(levels[k - 1], z));
                }
            }
        }
        assert_eq!(ring_levels(1), &[25.0, 50.0, 75.0]);
    }

    #[test]
    fn zoom_and_skin_settings_round_trip() {
        assert_eq!(zoom_from_value(&serde_json::Value::Null), 1);
        assert_eq!(zoom_from_value(&serde_json::json!(3)), 1);
        assert_eq!(zoom_from_value(&serde_json::json!("2")), 1);
        for z in ZOOMS {
            assert_eq!(zoom_from_value(&serde_json::json!(z)), z);
        }
        assert_eq!(next_zoom(1), 2);
        assert_eq!(next_zoom(2), 4);
        assert_eq!(next_zoom(4), 1);
        assert!(skin_from_value(&serde_json::Value::Null));
        assert!(!skin_from_value(&serde_json::json!(false)));
    }

    #[test]
    fn few_pixels_stay_visible() {
        assert_eq!(density(0, 100), 0.0);
        assert!(density(1, 100_000) >= 0.3);
        assert!((density(100, 100) - 1.0).abs() < 1e-6);
        assert!(density(10, 100) > density(2, 100));
        assert_eq!(density_reference(&[0, 0, 0]), 1);
        let mut v = vec![1u32; 99];
        v.push(1_000_000);
        assert_eq!(density_reference(&v), 1);
    }

    #[test]
    fn zoom_moves_a_point_outwards() {
        // a pale orange: at ×1 near the centre, at ×4 four times further out
        let px = [200u8, 170, 150];
        let find = |img: &egui::ColorImage| {
            let i = img.pixels.iter().position(|c| c.a() > 0).unwrap();
            let (x, y) = ((i % N) as f32 + 0.5, (i / N) as f32 + 0.5);
            ((x - N as f32 / 2.0).hypot(y - N as f32 / 2.0)) / (N as f32 / 2.0)
        };
        let d1 = find(&image(&px, 1));
        let d4 = find(&image(&px, 4));
        assert!(d1 > 0.0 && d1 < 0.25, "{d1}");
        assert!((d4 / d1 - 4.0).abs() < 0.6, "{d1} {d4}");
    }

    #[test]
    fn labels_follow_the_language() {
        use crate::menu::i18n::{locale::TestLanguage, Language};
        {
            let _l = TestLanguage::set(Language::De);
            assert_eq!(target_label(5, true), "Gb");
            assert_eq!(target_label(5, false), "Gelb");
            assert_eq!(target_label(0, true), "R");
            assert_eq!(hue_name(12.0), "Rot-Orange");
        }
        {
            let _l = TestLanguage::set(Language::En);
            assert_eq!(target_label(5, true), "Yl");
            assert_eq!(target_label(5, false), "Yellow");
            assert_eq!(target_label(1, true), "Mg");
            assert_eq!(target_label(4, true), "G");
            assert_eq!(hue_name(12.0), "Red-Orange");
        }
    }
}
