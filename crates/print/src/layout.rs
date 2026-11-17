//! Page layout in millimetres: margins, grid cells, photos fitted or filled into their cells, page breaks,
//! captions, page numbers and crop marks. The result ([`Layout`]) is a plain value from which the preview, the
//! page raster and the print files are made; the same input always gives the same layout.

use serde::Serialize;

use crate::template::{Background, InfoMode, Ink, Margins, Template};

/// Millimetres per point.
pub const MM_PER_PT: f64 = 25.4 / 72.0;
/// Space between photo and caption.
pub const CAPTION_GAP_MM: f64 = 1.0;
/// Crop marks: length and distance from the photo edge.
pub const CROP_MARK_LEN_MM: f64 = 5.0;
pub const CROP_MARK_GAP_MM: f64 = 2.0;
/// Crop mark line width (0.25 pt).
pub const CROP_MARK_WIDTH_MM: f64 = 0.25 * MM_PER_PT;
/// Text size of page numbers and page information.
pub const PAGE_TEXT_PT: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Default)]
pub struct RectMm {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectMm {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// One photo as the layout needs it: size of the developed image (after crop and rotation) and the caption.
#[derive(Clone, Debug, Default)]
pub struct PhotoSpec {
    pub width: u32,
    pub height: u32,
    /// caption under the photo (already resolved, see [`caption`])
    pub caption: String,
    /// shift of the visible part with "fill", −1 … 1 per axis (0 = centred)
    pub offset: [f64; 2],
}

/// Fields for the caption placeholders.
#[derive(Clone, Debug, Default)]
pub struct PhotoFields {
    pub filename: String,
    pub title: String,
    pub date: String,
    pub camera: String,
    pub exposure: String,
}

/// Caption of a photo for the template's mode.
pub fn caption(mode: InfoMode, pattern: &str, f: &PhotoFields) -> String {
    let s = match mode {
        InfoMode::Off => String::new(),
        InfoMode::Filename => f.filename.clone(),
        InfoMode::Title => if f.title.is_empty() { f.filename.clone() } else { f.title.clone() },
        InfoMode::Date => f.date.clone(),
        InfoMode::Exposure => [f.camera.as_str(), f.exposure.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · "),
        InfoMode::Custom => pattern
            .replace("{filename}", &f.filename)
            .replace("{title}", &f.title)
            .replace("{date}", &f.date)
            .replace("{camera}", &f.camera)
            .replace("{exposure}", &f.exposure),
    };
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextItem {
    pub text: String,
    /// anchor: x according to `align`, y = top of the line
    pub x: f64,
    pub y: f64,
    pub size_pt: f64,
    pub align: Align,
    /// longer lines are shortened with "…"
    pub max_w: f64,
    pub ink: Ink,
}

/// A filled rectangle (stroke, crop mark).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Fill {
    pub rect: RectMm,
    pub ink: Ink,
}

/// A photo on a page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Placed {
    /// index into the photo list
    pub photo: usize,
    /// the cell (guides, hit test)
    pub cell: RectMm,
    /// where the photo's pixels go
    pub image: RectMm,
    /// visible part of the (turned) photo, normalised: [x0, y0, x1, y1]
    pub crop: [f64; 4],
    /// turned by 90° clockwise
    pub turned: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Page {
    pub photos: Vec<Placed>,
    /// cells of this page without a photo (last page; guides only)
    pub empty_cells: Vec<RectMm>,
    pub fills: Vec<Fill>,
    pub texts: Vec<TextItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Layout {
    pub width_mm: f64,
    pub height_mm: f64,
    /// margins in effect (set margin or unprintable edge, whichever is larger)
    pub margins: Margins,
    /// area inside the margins
    pub content: RectMm,
    pub cell_w: f64,
    pub cell_h: f64,
    pub background: Background,
    pub pages: Vec<Page>,
    /// a template value had to give way (e.g. cell larger than the page allows)
    pub notes: Vec<Note>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Note {
    /// the set margins were raised to the printer's unprintable edge
    MarginRaised,
    /// the set cell size did not fit and was reduced
    CellReduced,
    /// the margins and gaps leave no room for a cell
    NoRoom,
    /// the stroke is wider than the gap between cells
    StrokeIntoNeighbour,
}

/// Largest cell for the grid in `content`: (width, height).
pub fn max_cell(content_w: f64, content_h: f64, columns: u32, rows: u32, gap: [f64; 2]) -> (f64, f64) {
    let c = columns.max(1) as f64;
    let r = rows.max(1) as f64;
    (((content_w - (c - 1.0) * gap[0]) / c).max(0.0), ((content_h - (r - 1.0) * gap[1]) / r).max(0.0))
}

/// Gap that makes `cell` fill the content (cell size set by the user); never below 0.
pub fn gap_for_cell(content: f64, n: u32, cell: f64) -> f64 {
    if n <= 1 {
        return 0.0;
    }
    ((content - n as f64 * cell) / (n as f64 - 1.0)).max(0.0)
}

/// Content area of a template on its paper with the printer's unprintable edges.
pub fn content_area(t: &Template, unprintable: Option<Margins>) -> (RectMm, Margins) {
    let (pw, ph) = t.paper.size_mm();
    let m = match unprintable {
        Some(u) => t.margins_mm.max(u),
        None => t.margins_mm,
    };
    (RectMm { x: m.left, y: m.top, w: (pw - m.left - m.right).max(0.0), h: (ph - m.top - m.bottom).max(0.0) }, m)
}

/// Height a caption takes under the photo.
pub fn caption_height(size_pt: f64) -> f64 {
    CAPTION_GAP_MM + size_pt * MM_PER_PT * 1.2
}

/// Lay out `photos` on pages according to `t`. `unprintable`: edges the printer cannot print (from the page
/// setup; `None` for file output). `page_info`: text of the page information line (made by the caller).
pub fn layout(t: &Template, photos: &[PhotoSpec], unprintable: Option<Margins>, page_info: &str) -> Layout {
    let (pw, ph) = t.paper.size_mm();
    let (content, margins) = content_area(t, unprintable);
    let mut notes = Vec::new();
    if margins != t.margins_mm {
        notes.push(Note::MarginRaised);
    }
    let g = &t.grid;
    let (cols, rows) = (g.columns.max(1), g.rows.max(1));
    let (max_w, max_h) = max_cell(content.w, content.h, cols, rows, g.gap_mm);
    let pick = |want: f64, max: f64| if want > 0.0 && want <= max + 1e-9 { want } else { max };
    let (cw, ch) = (pick(g.cell_mm[0], max_w), pick(g.cell_mm[1], max_h));
    if (g.cell_mm[0] > max_w + 1e-9) || (g.cell_mm[1] > max_h + 1e-9) {
        notes.push(Note::CellReduced);
    }
    // with a user cell size the rest is spread as gap (6.2), otherwise the set gap; grid centred in the content
    let gx = if g.cell_mm[0] > 0.0 && g.cell_mm[0] <= max_w + 1e-9 && cols > 1 { gap_for_cell(content.w, cols, cw) } else { g.gap_mm[0] };
    let gy = if g.cell_mm[1] > 0.0 && g.cell_mm[1] <= max_h + 1e-9 && rows > 1 { gap_for_cell(content.h, rows, ch) } else { g.gap_mm[1] };
    let grid_w = cols as f64 * cw + (cols - 1) as f64 * gx;
    let grid_h = rows as f64 * ch + (rows - 1) as f64 * gy;
    let x0 = content.x + (content.w - grid_w).max(0.0) / 2.0;
    let y0 = content.y + (content.h - grid_h).max(0.0) / 2.0;
    let cells: Vec<RectMm> = (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .map(|(r, c)| RectMm { x: x0 + c as f64 * (cw + gx), y: y0 + r as f64 * (ch + gy), w: cw, h: ch })
        .collect();
    let room = cw > 0.5 && ch > 0.5;
    if !room {
        notes.push(Note::NoRoom);
    }
    let stroke_mm = if t.image.stroke.on { t.image.stroke.width_pt * MM_PER_PT } else { 0.0 };
    if stroke_mm > 0.0 && (cols > 1 && 2.0 * stroke_mm > gx || rows > 1 && 2.0 * stroke_mm > gy) {
        notes.push(Note::StrokeIntoNeighbour);
    }
    let info = &t.page.photo_info;
    let cap_h = if info.mode == InfoMode::Off { 0.0 } else { caption_height(info.size_pt) };
    let text_ink = match t.page.background {
        Background::White => Ink::Black,
        Background::Black => Ink::White,
    };

    // which photo goes into which cell of which page
    let per_page = cells.len();
    let mut assignment: Vec<Vec<Option<usize>>> = Vec::new();
    if room && !photos.is_empty() {
        if t.image.repeat_one_per_page {
            for i in 0..photos.len() {
                assignment.push(vec![Some(i); per_page]);
            }
        } else {
            for chunk in (0..photos.len()).collect::<Vec<_>>().chunks(per_page) {
                let mut v: Vec<Option<usize>> = chunk.iter().map(|&i| Some(i)).collect();
                v.resize(per_page, None);
                assignment.push(v);
            }
        }
    }
    let n_pages = assignment.len();
    let mut pages = Vec::with_capacity(n_pages);
    for (pi, slots) in assignment.into_iter().enumerate() {
        let mut page = Page::default();
        for (cell, slot) in cells.iter().zip(slots) {
            let Some(i) = slot else {
                page.empty_cells.push(*cell);
                continue;
            };
            let p = &photos[i];
            let area = RectMm { h: (cell.h - cap_h).max(0.0), ..*cell };
            let placed = place(i, *cell, area, p, t.image.fill, t.image.rotate_to_fit);
            if stroke_mm > 0.0 {
                let r = placed.image;
                let ink = t.image.stroke.color;
                let s = stroke_mm;
                for rect in [
                    RectMm { x: r.x - s, y: r.y - s, w: r.w + 2.0 * s, h: s },
                    RectMm { x: r.x - s, y: r.bottom(), w: r.w + 2.0 * s, h: s },
                    RectMm { x: r.x - s, y: r.y, w: s, h: r.h },
                    RectMm { x: r.right(), y: r.y, w: s, h: r.h },
                ] {
                    page.fills.push(Fill { rect, ink });
                }
            }
            if cap_h > 0.0 && !p.caption.is_empty() {
                let r = placed.image;
                page.texts.push(TextItem {
                    text: p.caption.clone(),
                    x: r.x + r.w / 2.0,
                    y: r.bottom() + CAPTION_GAP_MM + stroke_mm,
                    size_pt: info.size_pt,
                    align: Align::Center,
                    max_w: cell.w,
                    ink: text_ink,
                });
            }
            page.photos.push(placed);
        }
        if t.page.crop_marks {
            crop_marks(&mut page, &content, pw, ph);
        }
        let text_h = PAGE_TEXT_PT * MM_PER_PT;
        // page texts in the bottom margin, vertically centred in it (never over the photos)
        let y = (ph - margins.bottom / 2.0 - text_h / 2.0).max(content.bottom()).min(ph - text_h);
        if t.page.page_numbers {
            page.texts.push(TextItem { text: format!("{} / {}", pi + 1, n_pages), x: pw / 2.0, y, size_pt: PAGE_TEXT_PT, align: Align::Center, max_w: pw / 4.0, ink: text_ink });
        }
        if t.page.page_info && !page_info.is_empty() {
            let x = margins.left.max(3.0);
            page.texts.push(TextItem { text: page_info.to_string(), x, y, size_pt: PAGE_TEXT_PT, align: Align::Left, max_w: (pw / 2.0 - x - 12.0).max(10.0), ink: text_ink });
        }
        pages.push(page);
    }
    Layout { width_mm: pw, height_mm: ph, margins, content, cell_w: cw, cell_h: ch, background: t.page.background, pages, notes }
}

/// Photo `p` in `area` (the cell without caption line): fitted or filled, turned if that makes it larger.
fn place(i: usize, cell: RectMm, area: RectMm, p: &PhotoSpec, fill: bool, rotate_to_fit: bool) -> Placed {
    let (iw, ih) = (p.width.max(1) as f64, p.height.max(1) as f64);
    let (aw, ah) = (area.w.max(1e-6), area.h.max(1e-6));
    let fit_k = |w: f64, h: f64| (aw / w).min(ah / h);
    let fill_k = |w: f64, h: f64| (aw / w).max(ah / h);
    let turned = rotate_to_fit
        && ((iw >= ih) != (aw >= ah))
        && (iw - ih).abs() > 1e-9
        && if fill {
            // less cut away when turned
            let shown = |w: f64, h: f64| { let k = fill_k(w, h); (aw * ah) / (k * k * w * h) };
            shown(ih, iw) > shown(iw, ih) + 1e-12
        } else {
            fit_k(ih, iw) > fit_k(iw, ih) + 1e-12
        };
    let (w, h) = if turned { (ih, iw) } else { (iw, ih) };
    if fill {
        let k = fill_k(w, h);
        // visible fraction of the photo per axis, shifted by the offset within the overhang
        let fx = (aw / (k * w)).min(1.0);
        let fy = (ah / (k * h)).min(1.0);
        let ox = p.offset[0].clamp(-1.0, 1.0);
        let oy = p.offset[1].clamp(-1.0, 1.0);
        let cx = 0.5 + ox * (1.0 - fx) / 2.0;
        let cy = 0.5 + oy * (1.0 - fy) / 2.0;
        Placed { photo: i, cell, image: area, crop: [cx - fx / 2.0, cy - fy / 2.0, cx + fx / 2.0, cy + fy / 2.0], turned }
    } else {
        let k = fit_k(w, h);
        let (dw, dh) = (w * k, h * k);
        // centred horizontally; vertically centred in the area, the caption follows the photo
        let image = RectMm { x: area.x + (aw - dw) / 2.0, y: area.y + (ah - dh) / 2.0, w: dw, h: dh };
        Placed { photo: i, cell, image, crop: [0.0, 0.0, 1.0, 1.0], turned }
    }
}

/// Crop marks in the margin: at each photo edge a short line outside the content area.
fn crop_marks(page: &mut Page, content: &RectMm, pw: f64, ph: f64) {
    let lw = CROP_MARK_WIDTH_MM;
    let mut xs: Vec<f64> = page.photos.iter().flat_map(|p| [p.image.x, p.image.right()]).collect();
    let mut ys: Vec<f64> = page.photos.iter().flat_map(|p| [p.image.y, p.image.bottom()]).collect();
    let dedup = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    };
    dedup(&mut xs);
    dedup(&mut ys);
    let push = |page: &mut Page, r: RectMm| {
        // only on the paper, never over the content
        let x0 = r.x.max(0.0);
        let y0 = r.y.max(0.0);
        let x1 = r.right().min(pw);
        let y1 = r.bottom().min(ph);
        if x1 - x0 > 0.01 && y1 - y0 > 0.01 {
            page.fills.push(Fill { rect: RectMm { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, ink: Ink::Black });
        }
    };
    let top_end = content.y - CROP_MARK_GAP_MM;
    let bottom_start = content.bottom() + CROP_MARK_GAP_MM;
    let left_end = content.x - CROP_MARK_GAP_MM;
    let right_start = content.right() + CROP_MARK_GAP_MM;
    for &x in &xs {
        if top_end > 0.0 {
            push(page, RectMm { x: x - lw / 2.0, y: (top_end - CROP_MARK_LEN_MM).max(0.0), w: lw, h: top_end.min(CROP_MARK_LEN_MM) });
        }
        if bottom_start < ph {
            push(page, RectMm { x: x - lw / 2.0, y: bottom_start, w: lw, h: CROP_MARK_LEN_MM });
        }
    }
    for &y in &ys {
        if left_end > 0.0 {
            push(page, RectMm { x: (left_end - CROP_MARK_LEN_MM).max(0.0), y: y - lw / 2.0, w: left_end.min(CROP_MARK_LEN_MM), h: lw });
        }
        if right_start < pw {
            push(page, RectMm { x: right_start, y: y - lw / 2.0, w: CROP_MARK_LEN_MM, h: lw });
        }
    }
}

/// Pixels of a length in mm at `ppi` (one rounding rule for all edges, so neighbouring cells never overlap or gap).
pub fn px(mm: f64, ppi: f64) -> i64 {
    (mm / 25.4 * ppi).round() as i64
}

/// A rectangle in whole pixels: [x0, y0, x1, y1) (both edges rounded with [`px`]).
pub fn rect_px(r: &RectMm, ppi: f64) -> [i64; 4] {
    [px(r.x, ppi), px(r.y, ppi), px(r.right(), ppi), px(r.bottom(), ppi)]
}

impl Layout {
    /// Page size in pixels at `ppi`.
    pub fn size_px(&self, ppi: f64) -> (usize, usize) {
        (px(self.width_mm, ppi).max(1) as usize, px(self.height_mm, ppi).max(1) as usize)
    }

    /// Stable text of the layout (rounded to 1 µm) – basis of the layout checksums in the tests.
    pub fn fingerprint(&self) -> String {
        let v = serde_json::to_value(self).expect("layout serialises");
        round_json(&v)
    }
}

fn round_json(v: &serde_json::Value) -> String {
    use serde_json::Value::*;
    match v {
        Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => format!("{:.3}", (f * 1000.0).round() / 1000.0 + 0.0),
            _ => n.to_string(),
        },
        Array(a) => format!("[{}]", a.iter().map(round_json).collect::<Vec<_>>().join(",")),
        Object(o) => format!("{{{}}}", o.iter().map(|(k, v)| format!("{k}:{}", round_json(v))).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{builtin, Orientation, Paper};

    fn photo(w: u32, h: u32) -> PhotoSpec {
        PhotoSpec { width: w, height: h, caption: format!("IMG_{w}x{h}.CR3"), offset: [0.0, 0.0] }
    }

    fn named(name: &str) -> Template {
        builtin().into_iter().find(|t| t.name == name).unwrap()
    }

    #[test]
    fn single_photo_fits_inside_the_margins_centred() {
        let t = named("1 Foto, A4, 10 mm Rand");
        let l = layout(&t, &[photo(6000, 4000)], None, "");
        assert_eq!(l.pages.len(), 1);
        let p = &l.pages[0].photos[0];
        // landscape photo in a portrait cell: turned, because it gets larger
        assert!(p.turned);
        // cell 190 × 277; turned photo 4000 × 6000 → k = min(190/4000, 277/6000) = 0.0461…: 184.67 × 277
        assert!((p.image.h - 277.0).abs() < 1e-9);
        assert!((p.image.w - 4000.0 * 277.0 / 6000.0).abs() < 1e-9);
        assert!((p.image.x + p.image.w / 2.0 - 105.0).abs() < 1e-9);
        let mut t2 = t.clone();
        t2.image.rotate_to_fit = false;
        let p = &layout(&t2, &[photo(6000, 4000)], None, "").pages[0].photos[0];
        assert!(!p.turned);
        assert!((p.image.w - 190.0).abs() < 1e-9 && (p.image.h - 190.0 * 4000.0 / 6000.0).abs() < 1e-9);
    }

    #[test]
    fn fill_crops_around_the_centre_and_follows_the_offset() {
        let mut t = named("Randlos 10 × 15");
        t.image.rotate_to_fit = false;
        // 3:2 landscape on 152.4 × 101.6 (3:2): nothing cut away
        let p = &layout(&t, &[photo(6000, 4000)], None, "").pages[0].photos[0];
        assert!(p.crop.iter().zip([0.0, 0.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-9), "{:?}", p.crop);
        // square photo: top and bottom cut, 2/3 of the height visible
        let p = &layout(&t, &[photo(4000, 4000)], None, "").pages[0].photos[0];
        assert!((p.crop[3] - p.crop[1] - 101.6 / 152.4).abs() < 1e-9);
        assert!((p.crop[1] + p.crop[3] - 1.0).abs() < 1e-9);
        let mut s = photo(4000, 4000);
        s.offset = [0.0, 1.0];
        let p = &layout(&t, &[s], None, "").pages[0].photos[0];
        assert!((p.crop[3] - 1.0).abs() < 1e-9, "offset 1 shows the bottom edge: {:?}", p.crop);
        assert_eq!(p.image, p.cell);
    }

    #[test]
    fn grid_cells_fill_the_content_and_pages_break() {
        let t = named("Kontaktabzug A4 5 × 6");
        let photos: Vec<_> = (0..67).map(|i| if i % 3 == 0 { photo(4000, 6000) } else { photo(6000, 4000) }).collect();
        let l = layout(&t, &photos, None, "info");
        assert_eq!(l.pages.len(), 3);
        assert_eq!(l.pages[2].photos.len(), 7);
        assert_eq!(l.pages[2].empty_cells.len(), 23);
        // 5 columns, gap 3: (190 − 12) / 5 = 35.6; 6 rows in 297 − 10 − 15 = 272: (272 − 15) / 6
        assert!((l.cell_w - 35.6).abs() < 1e-9);
        assert!((l.cell_h - 257.0 / 6.0).abs() < 1e-9);
        let first = &l.pages[0].photos[0].cell;
        let last = &l.pages[0].photos[29].cell;
        assert!((first.x - 10.0).abs() < 1e-9 && (last.right() - 200.0).abs() < 1e-9);
        assert!((last.bottom() - 282.0).abs() < 1e-9);
        // captions and page number
        assert_eq!(l.pages[0].texts.iter().filter(|t| t.align == Align::Center && t.text.starts_with("IMG_")).count(), 30);
        assert!(l.pages[1].texts.iter().any(|t| t.text == "2 / 3"));
        assert!(l.pages[0].texts.iter().any(|t| t.text == "info" && t.align == Align::Left));
        // captions never leave the cell, photos never cover the caption line
        for p in &l.pages[0].photos {
            assert!(p.image.bottom() + caption_height(6.0) <= p.cell.bottom() + 1e-9);
        }
    }

    #[test]
    fn repeat_one_photo_per_page() {
        let mut t = named("4 Fotos, A4");
        t.image.repeat_one_per_page = true;
        let l = layout(&t, &[photo(6000, 4000), photo(4000, 6000)], None, "");
        assert_eq!(l.pages.len(), 2);
        assert!(l.pages[1].photos.iter().all(|p| p.photo == 1) && l.pages[1].photos.len() == 4);
    }

    #[test]
    fn unprintable_edges_raise_the_margins() {
        let mut t = named("Randlos 10 × 15");
        t.margins_mm = Margins::all(0.0);
        let l = layout(&t, &[photo(6000, 4000)], Some(Margins { top: 3.0, bottom: 5.0, left: 3.0, right: 3.0 }), "");
        assert_eq!(l.notes, vec![Note::MarginRaised]);
        assert_eq!(l.content, RectMm { x: 3.0, y: 3.0, w: 152.4 - 6.0, h: 101.6 - 8.0 });
    }

    #[test]
    fn user_cell_size_spreads_the_rest_as_gap() {
        let mut t = named("4 Fotos, A4");
        t.grid.cell_mm = [90.0, 130.0];
        let l = layout(&t, &[photo(6000, 4000)], None, "");
        assert!((l.cell_w - 90.0).abs() < 1e-9);
        let a = &l.pages[0].photos[0].cell;
        let cells: Vec<_> = l.pages[0].empty_cells.clone();
        // 190 − 2·90 = 10 mm gap between the columns; content edges stay
        assert!((cells[0].x - a.right() - 10.0).abs() < 1e-9);
        assert!((a.x - 10.0).abs() < 1e-9);
        // too large: reduced to the maximum and noted
        t.grid.cell_mm = [150.0, 150.0];
        let l = layout(&t, &[photo(6000, 4000)], None, "");
        assert!(l.notes.contains(&Note::CellReduced));
        assert!((l.cell_w - 92.0).abs() < 1e-9);
    }

    #[test]
    fn crop_marks_stay_in_the_margin() {
        let mut t = named("4 Fotos, A4");
        t.page.crop_marks = true;
        let l = layout(&t, &vec![photo(6000, 4000); 4], None, "");
        let marks = &l.pages[0].fills;
        assert!(!marks.is_empty());
        for m in marks {
            let r = m.rect;
            let inside = r.x < l.content.right() && r.right() > l.content.x && r.y < l.content.bottom() && r.bottom() > l.content.y;
            assert!(!inside, "mark {r:?} lies over the content");
        }
    }

    #[test]
    fn captions_and_placeholders() {
        let f = PhotoFields { filename: "DP2M5418.X3F".into(), title: String::new(), date: "14.11.2026".into(), camera: "SIGMA DP2 Merrill".into(), exposure: "1/250 s · f/5,6 · ISO 100".into() };
        assert_eq!(caption(InfoMode::Title, "", &f), "DP2M5418.X3F");
        assert_eq!(caption(InfoMode::Custom, "{filename}  {date}", &f), "DP2M5418.X3F 14.11.2026");
        assert_eq!(caption(InfoMode::Exposure, "", &f), "SIGMA DP2 Merrill · 1/250 s · f/5,6 · ISO 100");
        assert_eq!(caption(InfoMode::Off, "", &f), "");
    }

    #[test]
    fn edges_round_alike_so_cells_never_overlap() {
        let t = named("Kontaktabzug A3+ 6 × 8");
        let l = layout(&t, &vec![photo(6000, 4000); 48], None, "");
        for ppi in [150.0, 300.0, 360.0, 720.0] {
            let (w, h) = l.size_px(ppi);
            assert_eq!((w, h), (px(329.0, ppi) as usize, px(483.0, ppi) as usize));
            let rects: Vec<_> = l.pages[0].photos.iter().map(|p| rect_px(&p.cell, ppi)).collect();
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    let overlap = a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
                    assert!(!overlap);
                }
            }
        }
        assert_eq!(px(483.0, 360.0), 6846);
        assert_eq!(px(329.0, 360.0), 4663);
    }

    /// Layout checksums: twelve fixed inputs (paper × grid × photo sizes) → hash of the layout. A deliberate change
    /// rewrites `tests/layout-reference.txt` (with `GRAINEO_PRINT_WRITE_REFERENCE=1`) and says why in the commit.
    #[test]
    fn layout_checksums() {
        use sha2::{Digest, Sha256};
        let landscape = photo(6000, 4000);
        let portrait = photo(4000, 6000);
        let square = photo(3000, 3000);
        let pano = photo(9000, 3000);
        let mixed: Vec<PhotoSpec> = (0..60).map(|i| [&landscape, &portrait, &square, &pano][i % 4].clone()).collect();
        let mut cases: Vec<(String, Template, Vec<PhotoSpec>, Option<Margins>)> = Vec::new();
        for t in builtin() {
            cases.push((t.name.clone(), t.clone(), mixed[..t.grid.rows as usize * t.grid.columns as usize + 1].to_vec(), None));
        }
        let mut t = named("4 Fotos, A4");
        t.paper = Paper::known("super-a3", Orientation::Landscape).unwrap();
        t.image.fill = true;
        t.image.stroke.on = true;
        t.page.crop_marks = true;
        t.page.photo_info.mode = InfoMode::Filename;
        cases.push(("A3+ quer, ausfüllen, Kontur, Schnittmarken".into(), t, mixed[..9].to_vec(), Some(Margins { top: 3.0, bottom: 14.0, left: 3.0, right: 3.0 })));
        let mut t = named("1 Foto, A4, 10 mm Rand");
        t.paper = Paper { id: "custom".into(), width_mm: 200.0, height_mm: 250.0, orientation: Orientation::Portrait };
        t.grid = crate::template::Grid { rows: 3, columns: 2, gap_mm: [4.0, 7.0], cell_mm: [80.0, 60.0] };
        t.image.repeat_one_per_page = true;
        cases.push(("frei 200 × 250, Zellmaß, wiederholen".into(), t, mixed[..3].to_vec(), None));
        assert_eq!(cases.len(), 12);
        let mut lines = Vec::new();
        for (name, t, photos, unprintable) in &cases {
            let l = layout(t, photos, *unprintable, "2026-11-17 · test");
            let hash = Sha256::digest(l.fingerprint().as_bytes());
            let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
            lines.push(format!("{hex}  {name}"));
        }
        let text = lines.join("\n") + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/layout-reference.txt");
        if std::env::var_os("GRAINEO_PRINT_WRITE_REFERENCE").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let reference = std::fs::read_to_string(&path).expect("tests/layout-reference.txt");
        assert_eq!(text, reference, "layout changed; if intended, rewrite with GRAINEO_PRINT_WRITE_REFERENCE=1");
    }
}
