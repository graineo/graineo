//! Labels of a grid cell (Bibliothek) in fixed zones, never overlapping.
//!
//! - Header: number on the left, quick collection on the right with a fixed width, in between the file name (its
//!   extension stays visible: "IMG_85…34.JPG") followed by small marks: a virtual copy as a stack with its number,
//!   a RAW+JPEG pair as "+JPG". The name gets only what is left; below its minimum it is left out (the marks stay
//!   as long as they fit).
//! - Bottom: stars on the left, flag on the right in a fixed zone. Narrow: "★5" instead of five stars, without stars
//!   then nothing.
//!
//! All thresholds come from the measured text widths (`measure`), no fixed sums. The layout functions are pure (tests
//! below); the painting helpers draw what they decided.

use eframe::egui::{self, Color32};

use crate::lr;

/// Width of the quick-collection zone at the right of the header.
pub const QUICK_W: f32 = 20.0;
/// Width of the flag zone at the right of the bottom row.
pub const FLAG_W: f32 = 22.0;
/// Space between zones.
pub const GAP: f32 = 4.0;
/// Font size of the header and the stars.
pub const SIZE: f32 = 10.5;

/// What the header shows (decided by [`header`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub number: bool,
    /// the name as shown (shortened in the middle), `None` = left out
    pub name: Option<String>,
    /// the name was shortened (full name in the tooltip)
    pub shortened: bool,
    pub copy: bool,
    pub pair: bool,
}

/// Header layout for a zone of `width` (without the quick-collection zone). `left`: width of the number (and what
/// stands next to it: warning sign, search score), `copy_w`/`pair_w`: width of the marks (0 = none). Dropped in this
/// order when space runs short: name, pair, number, copy.
pub fn header(width: f32, left: f32, name: &str, copy_w: f32, pair_w: f32, measure: &dyn Fn(&str) -> f32) -> Header {
    let mut h = Header { number: true, name: None, shortened: false, copy: copy_w > 0.0, pair: pair_w > 0.0 };
    // widths of the parts shown, with a gap between neighbours
    let used = |h: &Header| -> f32 {
        let parts = [(h.number, left), (h.copy, copy_w), (h.pair, pair_w)];
        let shown: Vec<f32> = parts.iter().filter(|p| p.0).map(|p| p.1).collect();
        shown.iter().sum::<f32>() + GAP * shown.len().saturating_sub(1) as f32
    };
    let before = used(&h);
    let room = width - before - if before > 0.0 { GAP } else { 0.0 };
    // the name needs at least a few characters and its extension ("I…G.JPG")
    let min_name = measure(&shortest(name));
    if room >= min_name {
        let shown = middle_with_extension(name, room, measure);
        h.shortened = shown != name;
        h.name = Some(shown);
        return h;
    }
    for drop in [2, 0, 1] {
        if used(&h) <= width {
            break;
        }
        match drop {
            2 => h.pair = false,
            0 => h.number = false,
            _ => h.copy = false,
        }
    }
    h
}

/// Name and extension: ("IMG_8534", ".JPG"); without a dot the extension is empty.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 6 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The shortest form of a name that is still shown: first and last character of the stem, the extension.
fn shortest(name: &str) -> String {
    let (stem, ext) = split_extension(name);
    let c: Vec<char> = stem.chars().collect();
    if c.len() <= 2 {
        return name.to_string();
    }
    format!("{}…{}{ext}", c[0], c[c.len() - 1])
}

/// `name` shortened in the middle of its stem so that it fits into `width`; the extension always stays visible
/// ("IMG_85…34.JPG").
pub fn middle_with_extension(name: &str, width: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if measure(name) <= width {
        return name.to_string();
    }
    let (stem, ext) = split_extension(name);
    let chars: Vec<char> = stem.chars().collect();
    let build = |n: usize| {
        let left = n.div_ceil(2);
        let right = n / 2;
        format!("{}…{}{ext}", chars[..left].iter().collect::<String>(), chars[chars.len() - right..].iter().collect::<String>())
    };
    let (mut lo, mut hi) = (2.min(chars.len()), chars.len().saturating_sub(1));
    while lo < hi {
        let n = (lo + hi).div_ceil(2);
        if measure(&build(n)) <= width {
            lo = n;
        } else {
            hi = n - 1;
        }
    }
    build(lo)
}

/// Stars of the bottom row for a zone of `width`: five stars, narrow "★5"; without stars the five dots, narrow
/// nothing. `None`: nothing fits.
pub fn stars(stars: u8, width: f32, measure: &dyn Fn(&str) -> f32) -> Option<String> {
    let full = if stars > 0 { "★".repeat(stars as usize) } else { "·····".to_string() };
    if measure(&full) <= width {
        return Some(full);
    }
    let short = format!("★{stars}");
    (stars > 0 && measure(&short) <= width).then_some(short)
}

/// Number of a virtual copy from its name ("Kopie 3" → 3).
pub fn copy_number(name: &str) -> Option<u32> {
    let digits: String = name.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok()
}

/// Width of the copy mark (stack plus number).
pub fn copy_width(number: Option<u32>, measure: &dyn Fn(&str) -> f32) -> f32 {
    11.0 + number.map_or(0.0, |n| 2.0 + measure(&n.to_string()))
}

/// Draws the copy mark (two offset sheets, like a stack) with its number, left-centred at `at`; returns its rect.
pub fn paint_copy(p: &egui::Painter, at: egui::Pos2, number: Option<u32>, color: Color32) -> egui::Rect {
    let back = egui::Rect::from_min_size(at + egui::vec2(3.0, -6.0), egui::vec2(7.0, 8.0));
    let front = egui::Rect::from_min_size(at + egui::vec2(0.0, -3.0), egui::vec2(7.0, 8.0));
    p.rect_stroke(back, 0.0, egui::Stroke::new(1.0, color.gamma_multiply(0.7)), egui::StrokeKind::Inside);
    p.rect_filled(front, 0.0, Color32::from_gray(0x1a));
    p.rect_stroke(front, 0.0, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);
    let mut r = back.union(front);
    if let Some(n) = number {
        let g = p.text(egui::pos2(r.right() + 2.0, at.y), egui::Align2::LEFT_CENTER, n.to_string(), egui::FontId::proportional(SIZE - 1.0), color);
        r = r.union(g);
    }
    r
}

/// Tooltip text of the copy mark.
pub fn copy_tip(copy: &str) -> String {
    match copy_number(copy) {
        Some(n) => crate::trf!("Virtuelle Kopie {n}", n = n),
        None => crate::trf!("Virtuelle Kopie „{k}“", k = copy),
    }
}

/// Measuring function for the painter in the given font size.
pub fn measurer(p: &egui::Painter, size: f32) -> impl Fn(&str) -> f32 + '_ {
    move |s: &str| p.layout_no_wrap(s.to_owned(), egui::FontId::proportional(size), lr::TEXT).size().x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monospaced measuring (6 points per character), like the app's font.
    fn m(s: &str) -> f32 {
        s.chars().count() as f32 * 6.0
    }

    #[test]
    fn long_names_are_shortened_in_the_middle_with_the_extension_kept() {
        let s = middle_with_extension("IMG_8534_verlaengert.JPG", 13.0 * 6.0, &m);
        assert!(s.ends_with(".JPG") && s.contains('…') && m(&s) <= 13.0 * 6.0, "{s}");
        assert!(s.starts_with("IMG_"));
        assert_eq!(middle_with_extension("IMG_8534.JPG", 200.0, &m), "IMG_8534.JPG");
        assert_eq!(split_extension("README"), ("README", ""));
    }

    #[test]
    fn header_at_several_widths() {
        let name = "IMG_8534.JPG";
        let copy = copy_width(Some(1), &m);
        // wide: everything, nothing shortened
        let h = header(200.0, 6.0, name, copy, 0.0, &m);
        assert_eq!((h.number, h.name.as_deref(), h.shortened, h.copy), (true, Some(name), false, true));
        // middle: name shortened, extension visible, never wider than its room
        let h = header(80.0, 6.0, name, copy, 0.0, &m);
        let shown = h.name.clone().unwrap();
        assert!(h.shortened && shown.ends_with(".JPG"), "{shown}");
        assert!(6.0 + GAP + copy + GAP + m(&shown) <= 80.0, "never wider than the zone");
        // narrow: name left out, copy mark stays
        let h = header(40.0, 6.0, name, copy, 0.0, &m);
        assert_eq!((h.name, h.copy, h.number), (None, true, true));
        // very narrow: then the number goes, the copy mark last
        let h = header(20.0, 6.0, name, copy, 0.0, &m);
        assert_eq!((h.number, h.copy), (false, true));
        let h = header(5.0, 6.0, name, copy, 0.0, &m);
        assert!(!h.number && !h.copy && h.name.is_none());
    }

    #[test]
    fn pair_mark_goes_before_the_number() {
        let pair = m("+JPG");
        let h = header(120.0, 6.0, "DSCF0001.RAF", 0.0, pair, &m);
        assert!(h.pair && h.name.is_some());
        let h = header(36.0, 6.0, "DSCF0001.RAF", 0.0, pair, &m);
        assert!(h.name.is_none() && h.number);
        assert_eq!(h.pair, 6.0 + GAP + pair <= 36.0);
    }

    #[test]
    fn stars_reduce_with_the_width() {
        assert_eq!(stars(5, 60.0, &m).as_deref(), Some("★★★★★"));
        assert_eq!(stars(5, 20.0, &m).as_deref(), Some("★5"));
        assert_eq!(stars(0, 60.0, &m).as_deref(), Some("·····"));
        assert_eq!(stars(0, 20.0, &m), None);
        assert_eq!(stars(3, 5.0, &m), None);
    }

    #[test]
    fn copy_numbers() {
        assert_eq!(copy_number("Kopie 1"), Some(1));
        assert_eq!(copy_number("Copy 12"), Some(12));
        assert_eq!(copy_number("Schwarzweiß"), None);
    }
}
