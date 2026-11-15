//! Border and frame of the print (darkroom): white paper border (even or wider at the
//! bottom) and optionally a black film border with rounded corners. The border is only written
//! to the file on "export as print" and is drawn in the darkroom view; the developed
//! image itself stays unchanged.

use super::{stock, FilmSettings};
use crate::Rgb16;

/// Width of the borders as a share of the long image edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderFractions {
    /// black film border all around (0 = off)
    pub film: f32,
    /// paper border left, top and right
    pub paper: f32,
    /// paper border at the bottom
    pub bottom: f32,
    /// Film strip: band with perforation on both long sides (0 = off), share of the long image edge
    pub band: f32,
}

impl BorderFractions {
    /// Radius of the rounded corners of the film border (share of the long edge).
    pub fn corner(&self) -> f32 {
        0.012
    }
}

/// Borders for these settings; `None` if no border is set.
pub fn border_fractions(fs: &FilmSettings) -> Option<BorderFractions> {
    if !fs.is_active() {
        return None;
    }
    let paper = fs.print_border.clamp(0.0, 15.0) / 100.0;
    let bottom = paper + fs.print_border_bottom.clamp(0.0, 10.0) / 100.0;
    let strip = fs.film_strip;
    let film = if strip { 0.004 } else if fs.film_border { 0.025 } else { 0.0 };
    let band = if strip { BAND } else { 0.0 };
    if paper <= 0.0 && bottom <= paper && film <= 0.0 {
        return None;
    }
    Some(BorderFractions { film, paper, bottom, band })
}

/// Width of the perforation band: drawn finer than on real film (5.5 mm of 36 mm there).
const BAND: f32 = 0.062;
/// Perforation pitch, hole width, hole height (shares of the long image edge).
pub const HOLE_PITCH: f32 = 0.050;
pub const HOLE_W: f32 = 0.026;
pub const HOLE_H: f32 = 0.017;
/// Position of the hole centre from the outer edge of the band (share of the long edge).
pub const HOLE_CENTER: f32 = 0.025;

/// Additional band width (left/right, top/bottom) in image units: the band lies on the two long sides.
pub fn band_extent(a: &BorderFractions, w: f32, h: f32) -> (f32, f32) {
    if a.band <= 0.0 {
        return (0.0, 0.0);
    }
    let long_edge = w.max(h);
    if w >= h { (0.0, a.band * long_edge) } else { (a.band * long_edge, 0.0) }
}

/// Edge print of the strip: maker and film name in capitals ("KODAK TRI-X 400").
pub fn strip_text(fs: &FilmSettings) -> String {
    let name = stock(&fs.stock).map(|s| s.name.clone()).unwrap_or_default();
    let base_name = name.split([',', '(']).next().unwrap_or("").trim().to_string();
    const DROP_WORDS: [&str; 11] = ["Negative", "Film", "Professional", "Black", "and", "White", "Reversal", "Pro", "Eastman", "ILFOTEC", "Print"];
    let words: Vec<String> = base_name.split_whitespace().filter(|t| !DROP_WORDS.iter().any(|w| w.eq_ignore_ascii_case(t))).map(|t| t.to_uppercase()).collect();
    words.join(" ")
}

/// 5×7 dot font (rows top to bottom, 5 bits per row) for edge prints.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        '>' => [0b10000, 0b01000, 0b00100, 0b00010, 0b00100, 0b01000, 0b10000],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        _ => [0; 7],
    }
}

/// Dot font as a bit mask (width, height, pixels), character spacing 1 dot.
fn text_mask(text: &str) -> (usize, usize, Vec<bool>) {
    let n = text.chars().count();
    let w = (n * 6).saturating_sub(1).max(1);
    let mut m = vec![false; w * 7];
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c);
        for (y, row) in g.iter().enumerate() {
            for x in 0..5 {
                if row >> (4 - x) & 1 == 1 {
                    m[y * w + i * 6 + x] = true;
                }
            }
        }
    }
    (w, 7, m)
}

/// Place text on the canvas: top left corner (`x0`, `y0`), letter height `height` in pixels;
/// `rotated` rotates the text by 90° (reading top to bottom).
pub(super) fn draw_text(out: &mut Rgb16, text: &str, x0: f32, y0: f32, height: f32, color: [u16; 3], rotated: bool) {
    let (mean, mh, m) = text_mask(text);
    let sk = (height / mh as f32).max(0.5);
    let (bw, bh) = if rotated { ((mh as f32 * sk), (mean as f32 * sk)) } else { ((mean as f32 * sk), (mh as f32 * sk)) };
    let (xa, ya) = (x0.max(0.0) as usize, y0.max(0.0) as usize);
    let (xe, ye) = (((x0 + bw).ceil() as usize).min(out.width), ((y0 + bh).ceil() as usize).min(out.height));
    for y in ya..ye {
        for x in xa..xe {
            let (u, v) = (x as f32 + 0.5 - x0, y as f32 + 0.5 - y0);
            let (mx, my) = if rotated { (v / sk, (bw - u) / sk) } else { (u / sk, v / sk) };
            if mx < 0.0 || my < 0.0 {
                continue;
            }
            let (ix, iy) = (mx as usize, my as usize);
            if ix < mean && iy < mh && m[iy * mean + ix] {
                let i = (y * out.width + x) * 3;
                out.data[i..i + 3].copy_from_slice(&color);
            }
        }
    }
}

/// Fill a rounded rectangle with one colour (perforation hole).
pub(super) fn draw_hole(out: &mut Rgb16, x0: f32, y0: f32, b: f32, h: f32, r: f32, color: [u16; 3]) {
    let (xa, ya) = (x0.floor().max(0.0) as usize, y0.floor().max(0.0) as usize);
    let (xe, ye) = (((x0 + b).ceil() as usize).min(out.width), ((y0 + h).ceil() as usize).min(out.height));
    for y in ya..ye {
        for x in xa..xe {
            let (px, py) = (x as f32 + 0.5 - x0, y as f32 + 0.5 - y0);
            let cx = if px < r { r - px } else if px > b - r { px - (b - r) } else { 0.0 };
            let cy = if py < r { r - py } else if py > h - r { py - (h - r) } else { 0.0 };
            if (cx * cx + cy * cy).sqrt() <= r {
                let i = (y * out.width + x) * 3;
                out.data[i..i + 3].copy_from_slice(&color);
            }
        }
    }
}

/// Place perforation, edge print and frame numbers on the black strip. `strip_area` = (x, y, width, height)
/// of the strip, `image_rect` = (x, y, width, height) of the photo inside it.
fn draw_strip(out: &mut Rgb16, fs: &FilmSettings, strip_area: (f32, f32, f32, f32), image_rect: (f32, f32, f32, f32), long_edge: f32) {
    let paper = paper_white(fs).map(|v| (v * 65535.0).round() as u16);
    let amber = [62000u16, 52500, 31000];
    let (fx, fy, fw, fh) = strip_area;
    let (bx, by, bw, bh) = image_rect;
    let rotated = bh > bw;
    let text = strip_text(fs);
    let (pitch, lb, lh, hole_mid) = (HOLE_PITCH * long_edge, HOLE_W * long_edge, HOLE_H * long_edge, HOLE_CENTER * long_edge);
    let th = 0.0105 * long_edge;
    let r = 0.004 * long_edge;
    // Length of a text in pixels at letter height `th` (5×7 dots, spacing 1)
    let text_len = |t: &str| ((t.chars().count() * 6).saturating_sub(1)) as f32 / 7.0 * th;
    if !rotated {
        // Holes at the top and bottom, running across
        let n = (fw / pitch).floor() as i32;
        let start = fx + (fw - n as f32 * pitch) / 2.0 + (pitch - lb) / 2.0;
        for i in 0..n {
            let x = start + i as f32 * pitch;
            draw_hole(out, x, fy + hole_mid - lh / 2.0, lb, lh, r, paper);
            draw_hole(out, x, fy + fh - hole_mid - lh / 2.0, lb, lh, r, paper);
        }
        // free strip between hole row and image
        let free_top = (fy + hole_mid + lh / 2.0, by);
        let ty = (free_top.0 + free_top.1) / 2.0 - th / 2.0;
        let edge_mark = format!("{text}  >");
        let advance = text_len(&edge_mark) + 0.035 * long_edge;
        let mut x = bx + 0.004 * long_edge;
        while x + text_len(&edge_mark) < bx + bw {
            draw_text(out, &edge_mark, x, ty, th, amber, false);
            x += advance;
        }
        // bottom: frame numbers
        let free_bottom = (by + bh, fy + fh - hole_mid - lh / 2.0);
        let uy = (free_bottom.0 + free_bottom.1) / 2.0 - th / 2.0;
        draw_text(out, "> 12", bx + 0.01 * long_edge, uy, th, amber, false);
        draw_text(out, "12A", bx + bw * 0.5, uy, th, amber, false);
    } else {
        let n = (fh / pitch).floor() as i32;
        let start = fy + (fh - n as f32 * pitch) / 2.0 + (pitch - lb) / 2.0;
        for i in 0..n {
            let y = start + i as f32 * pitch;
            draw_hole(out, fx + hole_mid - lh / 2.0, y, lh, lb, r, paper);
            draw_hole(out, fx + fw - hole_mid - lh / 2.0, y, lh, lb, r, paper);
        }
        let free_left = (fx + hole_mid + lh / 2.0, bx);
        let tx = (free_left.0 + free_left.1) / 2.0 - th / 2.0;
        let edge_mark = format!("{text}  >");
        let advance = text_len(&edge_mark) + 0.035 * long_edge;
        let mut y = by + 0.004 * long_edge;
        while y + text_len(&edge_mark) < by + bh {
            draw_text(out, &edge_mark, tx, y, th, amber, true);
            y += advance;
        }
        let free_right = (bx + bw, fx + fw - hole_mid - lh / 2.0);
        let ux = (free_right.0 + free_right.1) / 2.0 - th / 2.0;
        draw_text(out, "> 12", ux, by + 0.01 * long_edge, th, amber, true);
        draw_text(out, "12A", ux, by + bh * 0.5, th, amber, true);
    }
}

/// Paper white (sRGB-encoded, 0…1): never pure white, warm by default (`paper_white`); for black-and-white paper
/// additionally warmer (brownish) or cooler (bluish) with the paper tone.
pub fn paper_white(fs: &FilmSettings) -> [f32; 3] {
    let neutral = match fs.paper_white {
        1 => [0.955, 0.952, 0.940],
        2 => [0.958, 0.930, 0.850],
        _ => [0.962, 0.944, 0.900],
    };
    let mono = stock(&fs.stock).is_some_and(|s| s.monochrome && !s.positive);
    if !mono {
        return neutral;
    }
    let t = fs.paper_tone.clamp(-1.0, 1.0);
    let target_size = if t >= 0.0 { [0.968, 0.935, 0.870] } else { [0.925, 0.945, 0.972] };
    let k = t.abs();
    [0, 1, 2].map(|i| neutral[i] + (target_size[i] - neutral[i]) * k)
}


/// Paper texture (type, strength): baryta with fine fibres, colour paper with a fine lustre grain pattern,
/// paper with high matteness finely grained, smooth resin-coated paper and print film without texture. The patterns are
/// computed (no scans) and subtle; they act on the paper border and "on paper".
pub fn paper_texture(fs: &FilmSettings) -> (u8, f32) {
    let id = fs.paper.clone().or_else(|| stock(&fs.stock).and_then(|s| s.default_paper.clone())).unwrap_or_default();
    let (kind, amp) = if id.contains("multigrade_fb") {
        (1, 0.032)
    } else if id.starts_with("ilford_multigrade") || id.starts_with("kodak_2") {
        (0, 0.0)
    } else if id.contains("endura") || id.contains("ektacolor") || id.contains("crystal_archive") {
        (2, 0.026)
    } else {
        (0, 0.0)
    };
    let matte = fs.paper_matte.clamp(0.0, 6.0);
    match (kind, matte > 0.4) {
        (0, true) => (3, 0.006 * matte),
        _ => (kind, amp + 0.004 * matte),
    }
}

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263) ^ seed.wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535.0
}

fn vnoise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (xi, yi) = (xi as i32, yi as i32);
    let a = hash(xi, yi, seed) * (1.0 - sx) + hash(xi + 1, yi, seed) * sx;
    let b = hash(xi, yi + 1, seed) * (1.0 - sx) + hash(xi + 1, yi + 1, seed) * sx;
    a * (1.0 - sy) + b * sy
}

/// Texture value −1…1 at position (x, y) (pixels of an image with a 3000 px long edge).
pub fn texture_value(kind: u8, x: f32, y: f32) -> f32 {
    let n = match kind {
        1 => 0.35 * vnoise(x * 0.12, y * 0.9, 11) + 0.35 * vnoise(x * 0.9, y * 0.12, 23) + 0.3 * vnoise(x * 1.3, y * 1.3, 31),
        2 => 0.6 * vnoise(x * 0.7, y * 0.7, 5) + 0.4 * vnoise(x * 0.25, y * 0.25, 9),
        3 => vnoise(x * 1.1, y * 1.1, 3),
        _ => 0.5,
    };
    (n - 0.5) * 2.0
}

/// Add a border to the image; returned unchanged if no border is set.
pub fn apply_border(img: Rgb16, fs: &FilmSettings) -> Rgb16 {
    let Some(r) = border_fractions(fs) else { return img };
    let long_edge = img.width.max(img.height) as f32;
    let px = |a: f32| (a * long_edge).round() as usize;
    let (fb, side, bottom) = (px(r.film), px(r.paper), px(r.bottom));
    // Film strip: additional band with perforation on both long sides
    let (bmx, bmy) = band_extent(&r, img.width as f32, img.height as f32);
    let (bx, by) = (bmx.round() as usize, bmy.round() as usize);
    let (w, h) = (img.width + 2 * fb + 2 * bx + 2 * side, img.height + 2 * fb + 2 * by + side + bottom);
    let white = paper_white(fs).map(|v| (v * 65535.0).round() as u16);
    let mut out = Rgb16 { width: w, height: h, data: white.iter().copied().cycle().take(w * h * 3).collect() };
    let (kind, amp) = paper_texture(fs);
    if kind > 0 && amp > 0.0 {
        let sk = (long_edge / 3000.0).max(0.3);
        for y in 0..h {
            for x in 0..w {
                let f = 1.0 + amp * texture_value(kind, x as f32 / sk, y as f32 / sk);
                let i = (y * w + x) * 3;
                for c in 0..3 {
                    out.data[i + c] = (out.data[i + c] as f32 * f).clamp(0.0, 65535.0) as u16;
                }
            }
        }
    }
    // black film border with rounded corners (coverage at the edge soft over one pixel width)
    if fb > 0 {
        let (x0, y0, fw, fh) = (side as f32, side as f32, (img.width + 2 * fb + 2 * bx) as f32, (img.height + 2 * fb + 2 * by) as f32);
        // without a paper border the black reaches the sheet edge: then no rounded corners (paper would show through)
        let rad = if r.paper > 0.0 { (r.corner() * long_edge).max(1.0) } else { 0.5 };
        for y in side..side + img.height + 2 * fb + 2 * by {
            for x in side..side + img.width + 2 * fb + 2 * bx {
                let (px_, py_) = (x as f32 + 0.5 - x0, y as f32 + 0.5 - y0);
                // Distance to the corner: rounded only in the four corner squares
                let cx = if px_ < rad { rad - px_ } else if px_ > fw - rad { px_ - (fw - rad) } else { 0.0 };
                let cy = if py_ < rad { rad - py_ } else if py_ > fh - rad { py_ - (fh - rad) } else { 0.0 };
                let d = (cx * cx + cy * cy).sqrt();
                let a = (rad - d + 0.5).clamp(0.0, 1.0); // 1 = inside
                if a <= 0.0 {
                    continue;
                }
                let i = (y * w + x) * 3;
                for c in 0..3 {
                    let alt = out.data[i + c] as f32;
                    out.data[i + c] = (alt * (1.0 - a)).round() as u16;
                }
            }
        }
    }
    // Insert the image
    let (ox, oy) = (side + fb + bx, side + fb + by);
    for y in 0..img.height {
        let src = &img.data[y * img.width * 3..(y + 1) * img.width * 3];
        let dst = ((oy + y) * w + ox) * 3;
        out.data[dst..dst + img.width * 3].copy_from_slice(src);
    }
    // The paper surface also shows in the image, especially in the bright parts
    if kind > 0 && amp > 0.0 {
        let sk = (long_edge / 3000.0).max(0.3);
        for y in 0..img.height {
            for x in 0..img.width {
                let i = ((oy + y) * w + ox + x) * 3;
                let lum = (out.data[i] as f32 + out.data[i + 1] as f32 + out.data[i + 2] as f32) / (3.0 * 65535.0);
                let f = 1.0 + amp * 0.5 * lum * lum * texture_value(kind, (ox + x) as f32 / sk, (oy + y) as f32 / sk);
                for c in 0..3 {
                    out.data[i + c] = (out.data[i + c] as f32 * f).clamp(0.0, 65535.0) as u16;
                }
            }
        }
    }
    if r.band > 0.0 {
        let strip_area = (side as f32, side as f32, (img.width + 2 * fb + 2 * bx) as f32, (img.height + 2 * fb + 2 * by) as f32);
        let image_rect = (ox as f32, oy as f32, img.width as f32, img.height as f32);
        draw_strip(&mut out, fs, strip_area, image_rect, long_edge);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_image(w: usize, h: usize, v: u16) -> Rgb16 {
        Rgb16 { width: w, height: h, data: vec![v; w * h * 3] }
    }

    fn film() -> FilmSettings {
        FilmSettings { active: true, stock: "kodak_trix_400".into(), ..Default::default() }
    }

    #[test]
    fn no_border_keeps_image() {
        let fs = film();
        assert!(border_fractions(&fs).is_none());
        let bordered = apply_border(gray_image(40, 30, 1234), &fs);
        assert_eq!((bordered.width, bordered.height), (40, 30));
        assert!(bordered.data.iter().all(|&v| v == 1234));
        // never a border without film
        let without = FilmSettings { active: false, print_border: 5.0, ..film() };
        assert!(border_fractions(&without).is_none());
    }

    #[test]
    fn paper_border_even_and_wider_at_bottom() {
        let fs = FilmSettings { print_border: 10.0, print_border_bottom: 5.0, ..film() };
        let a = border_fractions(&fs).unwrap();
        assert!((a.paper - 0.10).abs() < 1e-6 && (a.bottom - 0.15).abs() < 1e-6 && a.film == 0.0);
        let out = apply_border(gray_image(200, 100, 100), &fs);
        // long edge 200: sides 20, bottom 30
        assert_eq!((out.width, out.height), (200 + 40, 100 + 20 + 30));
        let white = paper_white(&fs).map(|v| (v * 65535.0).round() as u16);
        let px = |x: usize, y: usize| [out.data[(y * out.width + x) * 3], out.data[(y * out.width + x) * 3 + 1], out.data[(y * out.width + x) * 3 + 2]];
        assert_eq!(px(0, 0), white);
        assert_eq!(px(out.width - 1, out.height - 1), white);
        assert_eq!(px(20, 20), [100, 100, 100], "image starts behind the border");
        assert_eq!(px(20 + 199, 20 + 99), [100, 100, 100]);
        assert_eq!(px(20 + 100, out.height - 5), white, "wider at the bottom");
        assert!(white.iter().all(|&v| v < 65535), "never pure white");
    }

    #[test]
    fn film_border_black_with_round_corners() {
        let fs = FilmSettings { print_border: 4.0, film_border: true, ..film() };
        let out = apply_border(gray_image(400, 300, 30000), &fs);
        let a = border_fractions(&fs).unwrap();
        let (fb, side) = ((a.film * 400.0).round() as usize, (a.paper * 400.0).round() as usize);
        assert_eq!(out.width, 400 + 2 * fb + 2 * side);
        let px = |x: usize, y: usize| out.data[(y * out.width + x) * 3];
        let white = (paper_white(&fs)[0] * 65535.0).round() as u16;
        assert_eq!(px(0, 0), white);
        // Middle of the top film border edge is black, the outermost corner is rounded (paper white)
        assert_eq!(px(side + fb + 200, side + 1), 0);
        assert!(px(side, side) > 40000, "Ecke abgerundet");
        // the image inside
        assert_eq!(px(side + fb + 10, side + fb + 10), 30000);
    }

    #[test]
    fn paper_tone_tints_white_only_for_bw() {
        let neutral = paper_white(&film());
        let warm = paper_white(&FilmSettings { paper_tone: 1.0, ..film() });
        let cool = paper_white(&FilmSettings { paper_tone: -1.0, ..film() });
        assert!(warm[0] > warm[2] && cool[2] > cool[0]);
        // Default is a warm white (slightly creamy), never pure white
        assert!(neutral[0] > neutral[2] && (neutral[0] - neutral[2]).abs() < 0.08 && neutral.iter().all(|&v| v < 0.98));
        let color = paper_white(&FilmSettings { stock: "kodak_portra_400".into(), paper_tone: 1.0, ..film() });
        assert_eq!(color, paper_white(&FilmSettings { stock: "kodak_portra_400".into(), ..film() }), "no paper tone for colour film");
    }
}

#[cfg(test)]
mod texture_tests {
    use super::*;
    use super::super::FilmSettings;
    use crate::Rgb16;

    /// Paper texture only acts on papers with texture and changes the white only subtly.
    #[test]
    fn texture_per_paper() {
        let mut fs = FilmSettings { active: true, stock: "kodak_trix_400".into(), print_border: 4.0, ..Default::default() };
        fs.paper = Some("ilford_multigrade_iv_rc".into());
        assert_eq!(paper_texture(&fs).0, 0);
        fs.paper = Some("ilford_multigrade_fb_classic".into());
        assert_eq!(paper_texture(&fs).0, 1);
        fs.paper = Some("kodak_portra_endura".into());
        assert_eq!(paper_texture(&fs).0, 2);
        let img = Rgb16 { width: 300, height: 200, data: vec![30000; 300 * 200 * 3] };
        let bordered = apply_border(img, &fs);
        let corner = &bordered.data[0..3];
        let white = paper_white(&fs);
        for c in 0..3 {
            let expected = white[c] * 65535.0;
            assert!((corner[c] as f32 - expected).abs() < expected * 0.05, "corner {corner:?} expected {expected}");
        }
    }

    /// Visual check: `GRAINEO_BORDER_IMAGE=<path.ppm>` writes one sheet per paper side by side,
    /// `GRAINEO_BORDER_PHOTO=<file.ppm>` (P6) uses a real photo instead of the grey area.
    #[test]
    #[ignore]
    fn write_texture_image() {
        let Some(out_path) = std::env::var_os("GRAINEO_BORDER_IMAGE") else { return };
        let photo = std::env::var_os("GRAINEO_BORDER_PHOTO").and_then(|p| read_ppm(std::path::Path::new(&p)));
        let mut words = Vec::new();
        for (paper, white, strip) in [("ilford_multigrade_iv_rc", 0u8, false), ("ilford_multigrade_fb_classic", 0, true), ("kodak_portra_endura", 2, false)] {
            let fs = FilmSettings { active: true, stock: "kodak_trix_400".into(), print_border: 6.0, film_border: true, film_strip: strip, paper: Some(paper.into()), paper_white: white, ..Default::default() };
            let img = match &photo { Some(f) => Rgb16 { width: f.width, height: f.height, data: f.data.clone() }, None => Rgb16 { width: 600, height: 400, data: vec![26000; 600 * 400 * 3] } };
            words.push(apply_border(img, &fs));
        }
        let (w, h) = (words.iter().map(|t| t.width).sum::<usize>(), words.iter().map(|t| t.height).max().unwrap());
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for y in 0..h {
            for t in &words {
                for x in 0..t.width {
                    for c in 0..3 {
                        out.push(if y < t.height { (t.data[(y * t.width + x) * 3 + c] >> 8) as u8 } else { 0 });
                    }
                }
            }
        }
        std::fs::write(out_path, out).unwrap();
    }

    fn read_ppm(p: &std::path::Path) -> Option<Rgb16> {
        let d = std::fs::read(p).ok()?;
        let mut words = Vec::new();
        let mut i = 0;
        while words.len() < 4 && i < d.len() {
            while i < d.len() && d[i].is_ascii_whitespace() { i += 1; }
            let a = i;
            while i < d.len() && !d[i].is_ascii_whitespace() { i += 1; }
            words.push(String::from_utf8_lossy(&d[a..i]).to_string());
        }
        i += 1;
        let (w, h): (usize, usize) = (words[1].parse().ok()?, words[2].parse().ok()?);
        Some(Rgb16 { width: w, height: h, data: d[i..i + w * h * 3].iter().map(|&b| (b as u16) << 8 | b as u16).collect() })
    }
}
