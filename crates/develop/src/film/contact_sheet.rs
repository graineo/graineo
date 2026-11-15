//! Contact sheet (darkroom): a sheet of paper with film strips of six frames each, like a real
//! contact sheet. Each row is a black strip with perforation, edge print ("KODAK TRI-X 400 >")
//! and frame numbers; the images are in 35 mm format 3:2, portrait images stand upright with black sides
//! in the frame. The images are already developed (film and paper as set); this only lays them out.

use super::border::{draw_hole, paper_white, draw_text, HOLE_PITCH, HOLE_W, HOLE_H, HOLE_CENTER};
use super::{strip_text, FilmSettings};
use crate::Rgb16;

/// Frames per strip.
pub const CONTACT_SHEET_COLUMNS: usize = 6;

/// Width of the strip border above and below the frame (share of the frame width).
const BAND: f32 = 0.066;
/// Gap between two frames and border left/right (share of the frame width).
const GAP: f32 = 0.012;
const MARGIN_LR: f32 = 0.030;
/// Paper border around the block of strips and spacing of the strips (share of the frame width).
const PAPER: f32 = 0.060;
const ROW_SPACING: f32 = 0.050;

/// Size of the sheet in pixels for `n` images, `columns` per strip and frame width `zb`.
pub fn contact_sheet_size(n: usize, columns: usize, zb: usize) -> (usize, usize) {
    let (rows, zbf) = (n.div_ceil(columns.max(1)).max(1), zb as f32);
    let zh = zbf * 2.0 / 3.0;
    let strip_w = columns as f32 * zbf + (columns as f32 - 1.0) * GAP * zbf + 2.0 * MARGIN_LR * zbf;
    let strip_h = zh + 2.0 * BAND * zbf;
    let b = strip_w + 2.0 * PAPER * zbf;
    let h = rows as f32 * strip_h + (rows as f32 - 1.0) * ROW_SPACING * zbf + 2.0 * PAPER * zbf;
    (b.round() as usize, h.round() as usize)
}

/// Box filter: downscale or upscale (bilinear) `src` to `w`×`h`; the background stays untouched.
fn scaled(src: &Rgb16, w: usize, h: usize) -> Rgb16 {
    let (w, h) = (w.max(1), h.max(1));
    let mut out = Rgb16 { width: w, height: h, data: vec![0; w * h * 3] };
    let (sx, sy) = (src.width as f32 / w as f32, src.height as f32 / h as f32);
    for y in 0..h {
        let (y0, y1) = (y as f32 * sy, (y as f32 + 1.0) * sy);
        for x in 0..w {
            let (x0, x1) = (x as f32 * sx, (x as f32 + 1.0) * sx);
            let (xa, xe) = (x0.floor() as usize, (x1.ceil() as usize).min(src.width).max(x0.floor() as usize + 1));
            let (ya, ye) = (y0.floor() as usize, (y1.ceil() as usize).min(src.height).max(y0.floor() as usize + 1));
            let mut acc = [0f32; 3];
            let mut n = 0f32;
            for yy in ya..ye.min(src.height) {
                let wy = (yy as f32 + 1.0).min(y1) - (yy as f32).max(y0);
                for xx in xa..xe.min(src.width) {
                    let wx = (xx as f32 + 1.0).min(x1) - (xx as f32).max(x0);
                    let wgt = (wx * wy).max(1e-4);
                    let i = (yy * src.width + xx) * 3;
                    for c in 0..3 {
                        acc[c] += src.data[i + c] as f32 * wgt;
                    }
                    n += wgt;
                }
            }
            let o = (y * w + x) * 3;
            for c in 0..3 {
                out.data[o + c] = (acc[c] / n.max(1e-4)).round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
    out
}

/// Downscale a photo so that its longest edge has at most `edge` pixels (a smaller one is never upscaled).
pub fn downscaled(src: &Rgb16, edge: usize) -> Rgb16 {
    let long_side = src.width.max(src.height);
    if long_side <= edge || edge == 0 {
        return Rgb16 { width: src.width, height: src.height, data: src.data.clone() };
    }
    let k = edge as f32 / long_side as f32;
    scaled(src, ((src.width as f32 * k).round() as usize).max(1), ((src.height as f32 * k).round() as usize).max(1))
}

/// Lay out the contact sheet. `cells`: the developed photo per image (`None` = not ready yet, stays black),
/// `fs`: film (for the edge print) and paper white, `zb`: width of a frame in pixels.
pub fn contact_sheet(cells: &[Option<Rgb16>], fs: &FilmSettings, columns: usize, zb: usize) -> Rgb16 {
    let columns = columns.max(1);
    let (w, h) = contact_sheet_size(cells.len(), columns, zb);
    let white = paper_white(fs).map(|v| (v * 65535.0).round() as u16);
    let mut out = Rgb16 { width: w, height: h, data: white.iter().copied().cycle().take(w * h * 3).collect() };
    let zbf = zb as f32;
    let (zh, band, gap, margin_lr, paper) = ((zbf * 2.0 / 3.0).round() as usize, (BAND * zbf).round() as usize, (GAP * zbf).round() as usize, (MARGIN_LR * zbf).round() as usize, (PAPER * zbf).round() as usize);
    let strip_w = columns * zb + (columns - 1) * gap + 2 * margin_lr;
    let strip_h = zh + 2 * band;
    let row_gap = (ROW_SPACING * zbf).round() as usize;
    let text = strip_text(fs);
    let amber = [62000u16, 52500, 31000];
    let (hole_pitch, hole_w, hole_h, hole_mid) = (HOLE_PITCH * zbf, HOLE_W * zbf, HOLE_H * zbf, HOLE_CENTER * zbf);
    let th = 0.0105 * zbf;
    let text_len = |t: &str| ((t.chars().count() * 6).saturating_sub(1)) as f32 / 7.0 * th;
    for (z, row) in cells.chunks(columns).enumerate() {
        let (x0, y0) = (paper, paper + z * (strip_h + row_gap));
        // black strip
        for y in y0..(y0 + strip_h).min(h) {
            let a = (y * w + x0) * 3;
            out.data[a..a + strip_w * 3].fill(0);
        }
        // Perforation at top and bottom
        let n = (strip_w as f32 / hole_pitch).floor() as i32;
        let start = x0 as f32 + (strip_w as f32 - n as f32 * hole_pitch) / 2.0 + (hole_pitch - hole_w) / 2.0;
        let r = 0.004 * zbf;
        for i in 0..n {
            let x = start + i as f32 * hole_pitch;
            draw_hole(&mut out, x, y0 as f32 + hole_mid - hole_h / 2.0, hole_w, hole_h, r, white);
            draw_hole(&mut out, x, (y0 + strip_h) as f32 - hole_mid - hole_h / 2.0, hole_w, hole_h, r, white);
        }
        // Edge print between hole row and frames (continuous across the whole strip)
        let ty = ((y0 as f32 + hole_mid + hole_h / 2.0) + (y0 + band) as f32) / 2.0 - th / 2.0;
        let edge_mark = format!("{text}  >");
        let advance = text_len(&edge_mark) + 0.035 * zbf;
        let mut x = (x0 + margin_lr) as f32 + 0.004 * zbf;
        while x + text_len(&edge_mark) < (x0 + strip_w - margin_lr) as f32 {
            draw_text(&mut out, &edge_mark, x, ty, th, amber, false);
            x += advance;
        }
        // Frames with numbers
        let uy = (((y0 + band + zh) as f32) + ((y0 + strip_h) as f32 - hole_mid - hole_h / 2.0)) / 2.0 - th / 2.0;
        for (s, photo) in row.iter().enumerate() {
            let (fx, fy) = (x0 + margin_lr + s * (zb + gap), y0 + band);
            if let Some(img) = photo {
                let (iw, ih) = (img.width as f32, img.height as f32);
                let k = (zb as f32 / iw).min(zh as f32 / ih);
                let (dw, dh) = (((iw * k).round() as usize).clamp(1, zb), ((ih * k).round() as usize).clamp(1, zh));
                let kl = if dw == img.width && dh == img.height { None } else { Some(scaled(img, dw, dh)) };
                let small = kl.as_ref().unwrap_or(img);
                let (ox, oy) = (fx + (zb - dw) / 2, fy + (zh - dh) / 2);
                for yy in 0..dh {
                    let src = &small.data[yy * dw * 3..(yy + 1) * dw * 3];
                    let dst = ((oy + yy) * w + ox) * 3;
                    out.data[dst..dst + dw * 3].copy_from_slice(src);
                }
            }
            let nr = z * columns + s + 1;
            draw_text(&mut out, &format!("> {nr}"), fx as f32 + 0.01 * zbf, uy, th, amber, false);
            draw_text(&mut out, &format!("{nr}A"), fx as f32 + zbf * 0.5, uy, th, amber, false);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn film() -> FilmSettings {
        FilmSettings { active: true, stock: "kodak_trix_400".into(), ..Default::default() }
    }

    fn solid(w: usize, h: usize, v: u16) -> Rgb16 {
        Rgb16 { width: w, height: h, data: vec![v; w * h * 3] }
    }

    fn px(img: &Rgb16, x: usize, y: usize) -> [u16; 3] {
        let i = (y * img.width + x) * 3;
        [img.data[i], img.data[i + 1], img.data[i + 2]]
    }

    #[test]
    fn size_grows_with_rows() {
        let (b1, h1) = contact_sheet_size(6, 6, 300);
        let (b2, h2) = contact_sheet_size(7, 6, 300);
        assert_eq!(b1, b2);
        assert!(h2 > h1 + 200, "second row needs space: {h1} → {h2}");
        let (b0, h0) = contact_sheet_size(0, 6, 300);
        assert_eq!((b0, h0), (b1, h1), "empty = one row");
    }

    #[test]
    fn strip_is_black_with_perforation_and_frames() {
        let zb = 300;
        let cells: Vec<Option<Rgb16>> = (0..8).map(|i| if i == 3 { None } else { Some(solid(600, 400, 40000)) }).collect();
        let out = contact_sheet(&cells, &film(), 6, zb);
        assert_eq!((out.width, out.height), contact_sheet_size(8, 6, zb));
        let white = paper_white(&film()).map(|v| (v * 65535.0).round() as u16);
        assert_eq!(px(&out, 2, 2), white, "Papierrand");
        // Centre of the first frame: image; strip border next to it: black
        let paper = (PAPER * zb as f32).round() as usize;
        let band = (BAND * zb as f32).round() as usize;
        let margin_lr = (MARGIN_LR * zb as f32).round() as usize;
        let (fx, fy) = (paper + margin_lr, paper + band);
        assert_eq!(px(&out, fx + zb / 2, fy + 100), [40000; 3], "image lies in the frame");
        assert_eq!(px(&out, paper + 2, paper + band + 100), [0; 3], "Streifen schwarz");
        // the missing image (no. 4) stays black
        let f4 = fx + 3 * (zb + (GAP * zb as f32).round() as usize);
        assert_eq!(px(&out, f4 + zb / 2, fy + 100), [0; 3]);
        // Perforation: paper white occurs somewhere in the hole row
        let hole_row = paper + (HOLE_CENTER * zb as f32).round() as usize;
        assert!((paper..out.width - paper).any(|x| px(&out, x, hole_row) == white), "holes");
    }

    #[test]
    fn portrait_stays_upright_with_black_sides() {
        let zb = 300;
        let out = contact_sheet(&[Some(solid(400, 600, 50000))], &film(), 6, zb);
        let paper = (PAPER * zb as f32).round() as usize;
        let band = (BAND * zb as f32).round() as usize;
        let margin_lr = (MARGIN_LR * zb as f32).round() as usize;
        let (fx, fy) = (paper + margin_lr, paper + band);
        let zh = zb * 2 / 3;
        // Image 200 high × 133 wide, centred: centre bright, left frame edge black
        assert_eq!(px(&out, fx + zb / 2, fy + zh / 2), [50000; 3]);
        assert_eq!(px(&out, fx + 5, fy + zh / 2), [0; 3]);
    }
}
