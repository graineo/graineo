//! The page as a raster, built in horizontal bands: background, photos (already scaled to their pixels),
//! strokes, crop marks and text (IBM Plex Mono). The same pixels go to the printer, the PDF and the image files.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

use crate::layout::{px, rect_px, Align, Layout, Placed};
use crate::template::{Background, Ink};
use crate::Image16;

/// Rows per band (the page is never held as a whole while writing).
pub const BAND_ROWS: usize = 512;

static FONT: &[u8] = include_bytes!("../../app/assets/fonts/IBMPlexMono-Regular.ttf");

/// Encoded value of an ink (white = paper white of the space, black = its black; neutral in every RGB space).
pub fn ink(i: Ink) -> [u16; 3] {
    match i {
        Ink::Black => [0; 3],
        Ink::White => [65535; 3],
        Ink::Gray => [32768; 3],
    }
}

/// A photo scaled to its place: `rect` in page pixels [x0, y0, x1, y1), `img` exactly that size.
#[derive(Clone, Debug)]
pub struct CellImage {
    pub rect: [i64; 4],
    pub img: Image16,
}

/// Most pixels of one page, and of all photo cells of one page together. The cells are held as 16-bit RGB (6 bytes
/// per pixel) until the page is written, sharpening needs 12 more bytes per pixel of one cell: about 4.5 GB at the
/// limit. A0 at 360 ppi (≈ 200 Mpx) fits; 2 × 2 m at 300 ppi (≈ 560 Mpx) does not.
pub const MAX_PAGE_PIXELS: u64 = 250_000_000;
/// Longest page edge in pixels (a band of [`BAND_ROWS`] rows stays at about 200 MB; also the JPEG limit).
pub const MAX_EDGE: u64 = 65_535;

/// A page too large to print at the chosen resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TooLarge {
    pub page: usize,
    pub width: u64,
    pub height: u64,
}

/// Pixels of a length in mm at `ppi`, without the integer conversion of [`px`] (no overflow for absurd sizes).
fn px_f(mm: f64, ppi: f64) -> f64 {
    let v = (mm / 25.4 * ppi).round();
    if v.is_finite() {
        v.max(0.0)
    } else {
        f64::MAX
    }
}

/// Checks before anything is allocated that every page of `layout` at `ppi` stays within [`MAX_PAGE_PIXELS`] and
/// [`MAX_EDGE`] (page and photo cells). A larger page would make the allocation fail and end the whole program.
pub fn check_size(layout: &Layout, ppi: f64) -> Result<(), TooLarge> {
    let (w, h) = (px_f(layout.width_mm, ppi), px_f(layout.height_mm, ppi));
    let too_large = |page| TooLarge { page, width: w.min(u64::MAX as f64) as u64, height: h.min(u64::MAX as f64) as u64 };
    if !ppi.is_finite() || ppi <= 0.0 || w > MAX_EDGE as f64 || h > MAX_EDGE as f64 || w * h > MAX_PAGE_PIXELS as f64 {
        return Err(too_large(0));
    }
    for (i, p) in layout.pages.iter().enumerate() {
        let cells: f64 = p.photos.iter().map(|pl| px_f(pl.image.w, ppi) * px_f(pl.image.h, ppi)).sum();
        if cells > MAX_PAGE_PIXELS as f64 {
            return Err(too_large(i));
        }
    }
    Ok(())
}

/// Pixel target of each photo on page `page`: (index in the page's photo list, rectangle in pixels).
pub fn targets(layout: &Layout, page: usize, ppi: f64) -> Vec<(usize, [i64; 4])> {
    layout.pages.get(page).map(|p| p.photos.iter().enumerate().map(|(i, pl)| (i, rect_px(&pl.image, ppi))).collect()).unwrap_or_default()
}

/// Cut the visible part of `src` for `placed` (turned by 90° clockwise if needed) and scale it to `w` × `h`
/// with `resize` (the app uses Lanczos3, also upwards).
pub fn prepare_cell(src: &Image16, placed: &Placed, w: usize, h: usize, resize: &dyn Fn(&Image16, usize, usize) -> Image16) -> Image16 {
    let [u0, v0, u1, v1] = placed.crop;
    // crop of the turned photo → crop in the source: turning clockwise maps source (x, y) to (1 − y, x)
    let (x0, y0, x1, y1) = if placed.turned { (v0, 1.0 - u1, v1, 1.0 - u0) } else { (u0, v0, u1, v1) };
    let (sw, sh) = (src.width as f64, src.height as f64);
    let ix0 = ((x0 * sw).round().max(0.0) as usize).min(src.width.saturating_sub(1));
    let iy0 = ((y0 * sh).round().max(0.0) as usize).min(src.height.saturating_sub(1));
    let ix1 = ((x1 * sw).round() as usize).clamp(ix0 + 1, src.width);
    let iy1 = ((y1 * sh).round() as usize).clamp(iy0 + 1, src.height);
    let mut part = Image16::new(ix1 - ix0, iy1 - iy0);
    for y in iy0..iy1 {
        let s = &src.data[(y * src.width + ix0) * 3..(y * src.width + ix1) * 3];
        let o = (y - iy0) * part.width * 3;
        part.data[o..o + s.len()].copy_from_slice(s);
    }
    if placed.turned {
        part = turn_clockwise(&part);
    }
    if (part.width, part.height) == (w, h) {
        part
    } else {
        resize(&part, w.max(1), h.max(1))
    }
}

/// Turn by 90° clockwise.
pub fn turn_clockwise(img: &Image16) -> Image16 {
    let (w, h) = (img.width, img.height);
    let mut out = Image16::new(h, w);
    for y in 0..h {
        for x in 0..w {
            // source (x, y) → target (h − 1 − y, x)
            let s = (y * w + x) * 3;
            let t = (x * h + (h - 1 - y)) * 3;
            out.data[t..t + 3].copy_from_slice(&img.data[s..s + 3]);
        }
    }
    out
}

struct TextRaster {
    x: i64,
    y: i64,
    w: usize,
    h: usize,
    alpha: Vec<u8>,
    color: [u16; 3],
}

/// One page, ready to be written band by band.
pub struct PageRaster {
    pub width: usize,
    pub height: usize,
    bg: [u16; 3],
    cells: Vec<CellImage>,
    fills: Vec<([i64; 4], [u16; 3])>,
    texts: Vec<TextRaster>,
}

impl PageRaster {
    /// Page `page` of `layout` at `ppi` with the photos `cells` (from [`targets`] and [`prepare_cell`]).
    pub fn new(layout: &Layout, page: usize, ppi: f64, cells: Vec<CellImage>) -> PageRaster {
        let (width, height) = layout.size_px(ppi);
        let bg = match layout.background {
            Background::White => [65535; 3],
            Background::Black => [0; 3],
        };
        let mut fills = Vec::new();
        let mut texts = Vec::new();
        if let Some(p) = layout.pages.get(page) {
            for f in &p.fills {
                let mut r = rect_px(&f.rect, ppi);
                // hairlines never vanish: at least one pixel
                if r[2] <= r[0] {
                    r[2] = r[0] + 1;
                }
                if r[3] <= r[1] {
                    r[3] = r[1] + 1;
                }
                fills.push((r, ink(f.ink)));
            }
            let font = FontRef::try_from_slice(FONT).expect("embedded font");
            for t in &p.texts {
                if let Some(r) = rasterize(&font, &t.text, t.size_pt / 72.0 * ppi, px(t.max_w, ppi).max(1) as f32) {
                    let x = match t.align {
                        Align::Left => px(t.x, ppi),
                        Align::Center => px(t.x, ppi) - r.1 as i64 / 2,
                        Align::Right => px(t.x, ppi) - r.1 as i64,
                    };
                    texts.push(TextRaster { x, y: px(t.y, ppi), w: r.1, h: r.2, alpha: r.0, color: ink(t.ink) });
                }
            }
        }
        PageRaster { width, height, bg, cells, fills, texts }
    }

    /// Rows `y0 .. y0 + rows` of the page into `out` (RGB, 16 bit).
    pub fn band(&self, y0: usize, rows: usize, out: &mut Vec<u16>) {
        let w = self.width;
        let rows = rows.min(self.height.saturating_sub(y0));
        out.clear();
        out.reserve(rows * w * 3);
        for _ in 0..rows * w {
            out.extend_from_slice(&self.bg);
        }
        let (by0, by1) = (y0 as i64, (y0 + rows) as i64);
        for c in &self.cells {
            let [x0, cy0, x1, cy1] = c.rect;
            let (ys, ye) = (cy0.max(by0), cy1.min(by1));
            if ys >= ye || c.img.width == 0 {
                continue;
            }
            let (xs, xe) = (x0.max(0), x1.min(w as i64));
            if xs >= xe {
                continue;
            }
            for y in ys..ye {
                let sy = (y - cy0) as usize;
                if sy >= c.img.height {
                    continue;
                }
                let sx0 = (xs - x0) as usize;
                let n = ((xe - xs) as usize).min(c.img.width.saturating_sub(sx0));
                let src = &c.img.data[(sy * c.img.width + sx0) * 3..(sy * c.img.width + sx0 + n) * 3];
                let o = ((y - by0) as usize * w + xs as usize) * 3;
                out[o..o + n * 3].copy_from_slice(src);
            }
        }
        for (r, col) in &self.fills {
            let (ys, ye) = (r[1].max(by0), r[3].min(by1));
            let (xs, xe) = (r[0].max(0), r[2].min(w as i64));
            for y in ys..ye {
                for x in xs..xe {
                    let o = ((y - by0) as usize * w + x as usize) * 3;
                    out[o..o + 3].copy_from_slice(col);
                }
            }
        }
        for t in &self.texts {
            let (ys, ye) = (t.y.max(by0), (t.y + t.h as i64).min(by1));
            for y in ys..ye {
                for tx in 0..t.w {
                    let x = t.x + tx as i64;
                    if x < 0 || x >= w as i64 {
                        continue;
                    }
                    let a = t.alpha[(y - t.y) as usize * t.w + tx] as u32;
                    if a == 0 {
                        continue;
                    }
                    let o = ((y - by0) as usize * w + x as usize) * 3;
                    for c in 0..3 {
                        let v = out[o + c] as u32;
                        out[o + c] = ((v * (255 - a) + t.color[c] as u32 * a + 127) / 255) as u16;
                    }
                }
            }
        }
    }

    /// The whole page (tests, small pages).
    pub fn full(&self) -> Image16 {
        let mut data = Vec::new();
        self.band(0, self.height, &mut data);
        Image16 { width: self.width, height: self.height, data }
    }
}

/// Text as coverage mask (alpha, width, height); shortened with "…" when wider than `max_w` pixels.
fn rasterize(font: &FontRef, text: &str, size_px: f64, max_w: f32) -> Option<(Vec<u8>, usize, usize)> {
    if text.is_empty() || size_px < 2.0 {
        return None;
    }
    let scale = PxScale::from(size_px as f32);
    let sf = font.as_scaled(scale);
    let width = |s: &str| s.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum::<f32>();
    let mut line = text.to_string();
    if width(&line) > max_w {
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() && width(&(chars.iter().collect::<String>() + "…")) > max_w {
            chars.pop();
        }
        if chars.is_empty() {
            return None;
        }
        line = chars.iter().collect::<String>() + "…";
    }
    let w = width(&line).ceil() as usize + 2;
    let h = (sf.ascent() - sf.descent()).ceil() as usize + 2;
    let mut alpha = vec![0u8; w * h];
    let mut x = 1.0f32;
    for c in line.chars() {
        let id = font.glyph_id(c);
        let g = id.with_scale_and_position(scale, ab_glyph::point(x, 1.0 + sf.ascent()));
        x += sf.h_advance(id);
        if let Some(o) = font.outline_glyph(g) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let px = b.min.x as i64 + gx as i64;
                let py = b.min.y as i64 + gy as i64;
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let i = py as usize * w + px as usize;
                    alpha[i] = alpha[i].max((cov.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            });
        }
    }
    Some((alpha, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{layout, PhotoSpec};
    use crate::template::builtin;

    fn gradient(w: usize, h: usize, seed: u16) -> Image16 {
        let mut img = Image16::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                img.data[i] = (x * 65535 / w.max(1)) as u16;
                img.data[i + 1] = (y * 65535 / h.max(1)) as u16;
                img.data[i + 2] = seed;
            }
        }
        img
    }

    /// nearest neighbour (tests only; the app scales with Lanczos3)
    fn nearest(src: &Image16, w: usize, h: usize) -> Image16 {
        let mut out = Image16::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let sx = x * src.width / w;
                let sy = y * src.height / h;
                let s = (sy * src.width + sx) * 3;
                out.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&src.data[s..s + 3]);
            }
        }
        out
    }

    fn page(name: &str, photos: &[(usize, usize)], ppi: f64) -> (Layout, PageRaster) {
        let t = builtin().into_iter().find(|t| t.name == name).unwrap();
        let specs: Vec<PhotoSpec> = photos.iter().enumerate().map(|(i, &(w, h))| PhotoSpec { width: w as u32, height: h as u32, caption: format!("photo-{i}.jpg"), offset: [0.0; 2] }).collect();
        let l = layout(&t, &specs, None, "2026-11-17 · test");
        let sources: Vec<Image16> = photos.iter().enumerate().map(|(i, &(w, h))| gradient(w, h, 1000 * i as u16)).collect();
        let cells = targets(&l, 0, ppi)
            .into_iter()
            .map(|(k, rect)| {
                let pl = &l.pages[0].photos[k];
                let img = prepare_cell(&sources[pl.photo], pl, (rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize, &nearest);
                CellImage { rect, img }
            })
            .collect();
        let r = PageRaster::new(&l, 0, ppi, cells);
        (l, r)
    }

    #[test]
    fn cell_edges_land_on_the_exact_pixels() {
        let ppi = 300.0;
        let (l, r) = page("1 Foto, A4, 10 mm Rand", &[(600, 400)], ppi);
        let img = r.full();
        assert_eq!((img.width, img.height), (2480, 3508));
        let pl = &l.pages[0].photos[0];
        let [x0, y0, x1, y1] = rect_px(&pl.image, ppi);
        assert_eq!(x0, px(pl.image.x, ppi));
        let at = |x: i64, y: i64| { let i = ((y as usize) * img.width + x as usize) * 3; [img.data[i], img.data[i + 1], img.data[i + 2]] };
        // paper outside, photo inside – on both sides of each edge
        assert_eq!(at(x0 - 1, (y0 + y1) / 2), [65535; 3]);
        assert_ne!(at(x0, (y0 + y1) / 2), [65535; 3]);
        assert_ne!(at(x1 - 1, (y0 + y1) / 2), [65535; 3]);
        assert_eq!(at(x1, (y0 + y1) / 2), [65535; 3]);
        assert_eq!(at((x0 + x1) / 2, y0 - 1), [65535; 3]);
        assert_ne!(at((x0 + x1) / 2, y1 - 1), [65535; 3]);
    }

    #[test]
    fn bands_put_together_give_the_whole_page() {
        let (_, r) = page("Kontaktabzug A4 5 × 6", &[(300, 200), (200, 300), (250, 250)], 150.0);
        let whole = r.full();
        let mut joined = Vec::new();
        let mut band = Vec::new();
        let mut y = 0;
        while y < r.height {
            r.band(y, 97, &mut band);
            joined.extend_from_slice(&band);
            y += 97;
        }
        assert_eq!(joined, whole.data);
    }

    #[test]
    fn turned_photo_is_turned_clockwise() {
        let mut img = Image16::new(3, 2);
        // top-left red marker
        img.data[0] = 65535;
        let t = turn_clockwise(&img);
        assert_eq!((t.width, t.height), (2, 3));
        // top-left goes to top-right
        assert_eq!(t.data[3], 65535);
    }

    #[test]
    fn text_and_strokes_are_drawn_crop_marks_in_the_margin() {
        let mut t = builtin().into_iter().find(|t| t.name == "4 Fotos, A4").unwrap();
        t.page.crop_marks = true;
        t.image.stroke.on = true;
        t.page.photo_info.mode = crate::template::InfoMode::Filename;
        t.page.page_numbers = true;
        let specs: Vec<PhotoSpec> = (0..4).map(|i| PhotoSpec { width: 300, height: 200, caption: format!("IMG_000{i}.CR3"), offset: [0.0; 2] }).collect();
        let l = layout(&t, &specs, None, "");
        let ppi = 150.0;
        let r = PageRaster::new(&l, 0, ppi, Vec::new());
        assert_eq!(r.texts.len(), 5);
        assert!(r.texts.iter().all(|t| t.alpha.iter().any(|&a| a > 200)), "glyphs have ink");
        let img = r.full();
        let dark = img.data.chunks_exact(3).filter(|p| p[0] < 20000).count();
        assert!(dark > 500, "strokes, marks and text: {dark} dark pixels");
        // the top margin holds the crop marks (5 mm long, 2 mm off the content)
        let y = px(10.0 - 2.0 - 2.5, ppi) as usize;
        let row = &img.data[y * img.width * 3..(y + 1) * img.width * 3];
        assert!(row.chunks_exact(3).any(|p| p[0] == 0));
    }

    #[test]
    fn long_captions_are_shortened() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        let (_, w, _) = rasterize(&font, "a very long file name that never fits.jpg", 30.0, 200.0).unwrap();
        assert!(w <= 203);
    }

    /// Raster checksum of a fixed page (synthetic photos, nearest scaling): guards the compositor.
    #[test]
    fn raster_checksum() {
        use sha2::{Digest, Sha256};
        let (_, r) = page("Kontaktabzug A4 5 × 6", &[(300, 200), (200, 300), (250, 250), (600, 200)], 100.0);
        let img = r.full();
        let bytes: Vec<u8> = img.data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let hex: String = Sha256::digest(&bytes).iter().take(8).map(|b| format!("{b:02x}")).collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/raster-reference.txt");
        if std::env::var_os("GRAINEO_PRINT_WRITE_REFERENCE").is_some() {
            std::fs::write(&path, format!("{hex}\n")).unwrap();
        }
        assert_eq!(format!("{hex}\n"), std::fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn page_size_is_checked_before_anything_is_allocated() {
        let photos = vec![PhotoSpec { width: 6000, height: 4000, caption: String::new(), offset: [0.0; 2] }];
        let mut t = builtin().into_iter().find(|t| t.name == "4 Fotos, A4").unwrap();
        let l = layout(&t, &photos, None, "");
        assert_eq!(check_size(&l, 300.0), Ok(()));
        assert_eq!(check_size(&l, 1440.0), Ok(()), "A4 at the highest template resolution");
        // free paper of 2 × 2 m at 300 ppi (≈ 560 Mpx) and at 720 ppi (≈ 3.2 Gpx): refused, nothing allocated
        t.paper.id = crate::template::CUSTOM_PAPER.into();
        t.paper.width_mm = 2000.0;
        t.paper.height_mm = 2000.0;
        let l = layout(&t, &photos, None, "");
        for ppi in [300.0, 720.0, 1440.0] {
            let e = check_size(&l, ppi).unwrap_err();
            assert!(e.width > 20_000 && e.height > 20_000, "{e:?}");
        }
        assert!(check_size(&l, 72.0).is_ok());
        // a long thin roll: few pixels but an edge longer than a band allows
        t.paper.width_mm = 100.0;
        t.paper.height_mm = 2000.0;
        let l = layout(&t, &photos, None, "");
        assert!(check_size(&l, 1440.0).is_err());
        assert!(check_size(&l, f64::NAN).is_err() && check_size(&l, 0.0).is_err());
    }
}
