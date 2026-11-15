//! Film shelf of the darkroom: a 100 % crop of the current photo per film row, a sign for the kind of film
//! (cartridge, roll film, sheet film, cine reel, photo paper; colour = genre, no maker colours or logos) and a short,
//! instructive characterisation per film and paper.
//!
//! The crops show one telling place of the photo, chosen once per photo and crop ([`best_spot`] or the largest known
//! face); they do not follow zooming or panning. The crops are computed in the background ([`crate::jobs::Job::FilmCrops`], lane "back", one job at a time) only
//! for visible rows, once per settings state and crop position; until new ones arrive the old ones stay shown, and a
//! row without any crop yet shows a grey placeholder with a small computing sign ([`paint_waiting`]).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use develop::film::{Category, StockInfo};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Vec2};

use crate::App;

/// Size of the crop field in a shelf row (points); at 100 % one image pixel per physical pixel.
pub const FIELD: Vec2 = Vec2::new(64.0, 40.0);
/// Height of a film row.
pub const ROW_H: f32 = 48.0;
/// Room for the sign on the right of a row.
pub const SIGN_W: f32 = 34.0;
/// Wait after the last change of settings or crop position before computing (no work while sliding or panning).
const REST: f64 = 0.35;
/// Retry when the full resolution was not ready yet.
const RETRY: f64 = 1.5;

/// Kind of film (shape of the sign).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 35 mm cartridge
    Small,
    /// 120 roll film on a spool
    Roll,
    /// sheet film with notch code
    Sheet,
    /// cine reel (motion picture negative, reversal and print film)
    Cine,
    /// sheet of photo paper (print)
    Paper,
}

/// Usual format of a film from the profile data: motion picture stocks (Vision3, Verita, Double-X, Tri-X 7266, print
/// films) are cine; otherwise the format the film is best known in; unknown 35 mm.
pub fn format_of(s: &StockInfo) -> Format {
    match s.category {
        Category::Cine | Category::PrintFilm => return Format::Cine,
        Category::Paper => return Format::Paper,
        _ => {}
    }
    match s.id.as_str() {
        "kodak_doublex" | "kodak_trix" => Format::Cine,
        "kodak_portra_400" | "kodak_trix_400" | "fujifilm_pro_400h" | "fujifilm_velvia_100" => Format::Roll,
        "ilford_delta_100" | "ilford_ortho_plus" | "kodak_portra_160" => Format::Sheet,
        _ => Format::Small,
    }
}

/// Colour of the sign by genre: black & white grey, colour negative terracotta, slide petrol, cine violet; papers
/// neutral grey.
pub fn color_of(s: &StockInfo) -> Color32 {
    let grey = Color32::from_rgb(0xb9, 0xb6, 0xae);
    match s.category {
        Category::Paper => Color32::from_gray(0xa4),
        Category::PrintFilm if s.monochrome => grey,
        Category::PrintFilm | Category::Cine => Color32::from_rgb(0xa2, 0x7f, 0xd1),
        Category::BlackWhite => grey,
        Category::Slide => Color32::from_rgb(0x5f, 0xb3, 0xb3),
        Category::ColorNegative => Color32::from_rgb(0xd9, 0x8a, 0x5b),
    }
}

/// Tooltip of the sign (opens the info card).
pub fn tip(f: Format) -> &'static str {
    match f {
        Format::Small => "Filminfo (Kleinbild 35 mm)",
        Format::Roll => "Filminfo (Rollfilm 120)",
        Format::Sheet => "Filminfo (Planfilm)",
        Format::Cine => "Filminfo (Kinofilm)",
        Format::Paper => "Papierinfo (Fotopapier)",
    }
}

/// Short, instructive characterisation (German key, translated at the display site). Sources: the info cards of the
/// profiles (data sheets).
pub fn character(id: &str) -> Option<&'static str> {
    Some(match id {
        // black & white
        "agfa_apx_100" => "klassisches Korn, fein und weich",
        "agfa_apx_400" => "klassisches Korn, mittel und weich",
        "agfa_scala_200x" => "Schwarzweiß-Dia, dicht und hart",
        "fujifilm_acros_100_ii" => "extrem fein, kräftiges Schwarz",
        "ilford_delta_100" => "Tafelkristall, sehr fein",
        "ilford_delta_3200" => "Tafelkristall, weich, für die Nacht",
        "ilford_delta_400" => "Tafelkristall, fein für einen 400er",
        "ilford_fp4_plus" => "klassisches Korn, fein und glatt",
        "ilford_hp5_plus" => "klassisches Korn, großer Spielraum",
        "ilford_ortho_plus" => "orthochromatisch · Rot wird dunkel",
        "kodak_doublex" => "Kinonegativ, grafisch, sichtbares Korn",
        "kodak_tmax_100" => "Tafelkristall, sehr fein und scharf",
        "kodak_tmax_400" => "Tafelkristall, fein für einen 400er",
        "kodak_tmax_p3200" => "Tafelkristall, deutliches Korn",
        "kodak_trix" => "Kino-Umkehrfilm, direkt positiv",
        "kodak_trix_400" => "kräftig, deutliches Korn",
        // colour negative
        "fujifilm_c200" => "ruhig, neutral bis kühl",
        "fujifilm_pro_400h" => "vierte Farbschicht, kühl, pastellig",
        "fujifilm_superia_200" => "satt, leicht grünlich",
        "fujifilm_xtra_400" => "kräftig, kühl-grünlich",
        "kodak_ektar_100" => "sehr fein, satt und klar",
        "kodak_gold_200" => "warm, goldener Grundton",
        "kodak_gold_800" => "warm, sichtbares Korn",
        "kodak_portra_160" => "sehr fein, ruhige Farben",
        "kodak_portra_400" => "weiche Haut, viel Spielraum",
        "kodak_portra_800" => "natürliche Haut auch bei Mischlicht",
        "kodak_portra_800_push1" => "1 Blende gepusht · dichter, kräftiger",
        "kodak_portra_800_push2" => "2 Blenden gepusht · hart, grobes Korn",
        "kodak_ultramax_400" => "warm, satt, lebhaft",
        // slide
        "fujifilm_provia_100f" => "neutral, sehr fein",
        "fujifilm_velvia_100" => "sehr satt, tiefe Schatten",
        "kodak_ektachrome_100" => "neutral, sauber, extrem fein",
        "kodak_ektachrome_e200" => "mäßiger Kontrast, natürliche Haut",
        "kodak_kodachrome_25" => "feinster Kodachrome, wenig Spielraum",
        "kodak_kodachrome_64" => "kräftiges Rot, dichte Schatten",
        // cine
        "kodak_verita_200d" => "Tageslicht · klassisch filmisch",
        "kodak_vision3_200t" => "Kunstlicht · fein wie ein 100er",
        "kodak_vision3_250d" => "Tageslicht · weich, viel Reserve",
        "kodak_vision3_500t" => "Kunstlicht · bei Tag bläulich",
        "kodak_vision3_50d" => "Tageslicht · sehr fein",
        // papers and print films
        "fujifilm_crystal_archive_typeii" => "Farbpapier · lebendig, reines Weiß",
        "ilford_multigrade_fb_classic" => "Baryt · tiefes Schwarz, Grad 00–5",
        "ilford_multigrade_iv_rc" => "PE-Papier · neutral, Grad 00–5",
        "kodak_2302" => "S/W-Kopierfilm · steil, sehr fein",
        "kodak_2383" => "Kinokopie · tiefes Schwarz",
        "kodak_2393" => "Kinokopie · dichter und satter",
        "kodak_ektacolor_edge" => "Farbpapier · satt, Grün und Blau",
        "kodak_endura_premier" => "Farbpapier · großer Farbraum",
        "kodak_portra_endura" => "Farbpapier · weich, sanfte Haut",
        "kodak_supra_endura" => "Farbpapier · kräftiger Mittelweg",
        "kodak_ultra_endura" => "Farbpapier · sehr satt, hart",
        _ => return None,
    })
}

/// Draw the sign of a format into `r` (line drawing on a 40 × 42 grid, scaled to the height of `r`).
pub fn paint_sign(p: &egui::Painter, r: Rect, f: Format, c: Color32) {
    let s = r.height() / 42.0;
    let o = Pos2::new(r.center().x - 20.0 * s, r.top());
    let at = |x: f32, y: f32| o + Vec2::new(x * s, y * s);
    let w = (1.6 * s).max(1.1);
    let st = Stroke::new(w, c);
    let faint = |a: f32| Stroke::new(w, c.gamma_multiply(a));
    let rect = |x: f32, y: f32, rw: f32, rh: f32| Rect::from_min_max(at(x, y), at(x + rw, y + rh));
    let line = |pts: &[(f32, f32)], st: Stroke| {
        p.add(egui::Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), st));
    };
    let closed = |pts: &[(f32, f32)], st: Stroke| {
        p.add(egui::Shape::closed_line(pts.iter().map(|&(x, y)| at(x, y)).collect(), st));
    };
    // quadratic Bézier from a over control b to c, as a polyline
    let quad = |a: (f32, f32), b: (f32, f32), e: (f32, f32)| -> Vec<(f32, f32)> {
        (0..=8)
            .map(|i| {
                let t = i as f32 / 8.0;
                let u = 1.0 - t;
                (u * u * a.0 + 2.0 * u * t * b.0 + t * t * e.0, u * u * a.1 + 2.0 * u * t * b.1 + t * t * e.1)
            })
            .collect()
    };
    match f {
        Format::Small => {
            // cartridge with spool knob, felt lips and film tongue with perforation
            p.rect_stroke(rect(8.0, 9.0, 17.0, 24.0), 2.5 * s, st, egui::StrokeKind::Middle);
            p.rect_stroke(rect(11.0, 5.0, 11.0, 4.0), 1.0 * s, st, egui::StrokeKind::Middle);
            line(&[(8.0, 13.0), (25.0, 13.0)], faint(0.6));
            line(&[(8.0, 29.0), (25.0, 29.0)], faint(0.6));
            line(&[(25.0, 14.0), (34.0, 14.0), (35.4, 14.6), (36.0, 16.0), (36.0, 24.0), (33.0, 27.0), (25.0, 27.0)], st);
            p.rect_filled(rect(27.5, 16.5, 2.0, 2.0), 0.0, c);
            p.rect_filled(rect(31.0, 16.5, 2.0, 2.0), 0.0, c);
        }
        Format::Roll => {
            // spool with flanges and backing paper
            p.rect_stroke(rect(10.0, 10.0, 20.0, 22.0), 2.0 * s, st, egui::StrokeKind::Middle);
            line(&[(7.0, 8.0), (33.0, 8.0)], st);
            line(&[(7.0, 34.0), (33.0, 34.0)], st);
            line(&[(14.0, 10.0), (14.0, 32.0)], faint(0.45));
            line(&[(26.0, 10.0), (26.0, 32.0)], faint(0.45));
            let mut pts = quad((30.0, 14.0), (36.0, 15.0), (36.0, 21.0));
            pts.extend(quad((36.0, 21.0), (36.0, 27.0), (30.0, 28.0)).into_iter().skip(1));
            line(&pts, faint(0.7));
        }
        Format::Sheet => {
            // sheet with cut corner and notch code
            closed(&[(9.0, 6.0), (29.0, 6.0), (32.0, 9.0), (32.0, 35.0), (9.0, 35.0)], st);
            line(&[(29.0, 6.0), (29.0, 9.0), (32.0, 9.0)], faint(0.6));
            line(&[(32.0, 12.0), (30.0, 13.5), (32.0, 15.0)], st);
            line(&[(32.0, 17.0), (29.5, 17.0), (29.5, 19.0), (32.0, 19.0)], st);
            line(&[(13.0, 28.0), (24.0, 28.0)], faint(0.5));
        }
        Format::Cine => {
            // reel with five windows
            p.circle_stroke(at(20.0, 21.0), 14.0 * s, st);
            p.circle_stroke(at(20.0, 21.0), 3.0 * s, st);
            for k in 0..5 {
                let a = (k as f32 * 72.0).to_radians();
                p.circle_stroke(at(20.0 + 8.0 * a.cos(), 21.0 + 8.0 * a.sin()), 2.6 * s, st);
            }
        }
        Format::Paper => {
            // print with white border and a small landscape
            p.rect_stroke(rect(6.0, 8.0, 28.0, 26.0), 1.0 * s, st, egui::StrokeKind::Middle);
            p.rect_stroke(rect(10.0, 12.0, 20.0, 15.0), 0.0, faint(0.7), egui::StrokeKind::Middle);
            line(&[(10.0, 24.0), (15.5, 18.5), (19.0, 22.0), (23.5, 17.0), (30.0, 23.5)], faint(0.7));
            line(&[(13.0, 30.5), (20.0, 30.5)], faint(0.45));
        }
    }
}

/// Sign as a button at the right of a row: hover lighter, tooltip, click opens the info card. Returns: clicked?
pub fn sign_button(ui: &mut egui::Ui, row: Rect, id: egui::Id, f: Format, c: Color32) -> bool {
    let r = Rect::from_center_size(Pos2::new(row.right() - SIGN_W / 2.0 - 2.0, row.center().y), Vec2::new(SIGN_W - 6.0, (row.height() - 12.0).min(30.0)));
    let resp = ui.interact(r, id, egui::Sense::click());
    let color = if resp.hovered() { lighter(c) } else { c };
    let h = (r.height() - 4.0).min(26.0);
    paint_sign(ui.painter(), Rect::from_center_size(r.center(), Vec2::new(h * 40.0 / 42.0, h)), f, color);
    crate::lr::hover_tip(ui, &resp, tip(f));
    resp.clicked()
}

fn lighter(c: Color32) -> Color32 {
    let m = |v: u8| v + ((255 - v) as f32 * 0.45) as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}

/// Paint a crop texture 1:1 (one texel per physical pixel) centred in the field, aligned to the pixel grid.
pub fn paint_crop(p: &egui::Painter, field: Rect, t: &TextureHandle, ppp: f32) {
    let size = t.size_vec2() / ppp;
    let snap = |v: f32| (v * ppp).round() / ppp;
    let min = Pos2::new(snap(field.center().x - size.x / 2.0), snap(field.center().y - size.y / 2.0));
    p.with_clip_rect(field.intersect(p.clip_rect())).image(t.id(), Rect::from_min_size(min, size), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
}

/// Placeholder while a crop is being computed: a neutral mid grey (clearly not a dark crop) with three small dots
/// lighting up one after the other at the bottom right.
pub fn paint_waiting(p: &egui::Painter, field: Rect, time: f64) {
    p.rect_filled(field, 0.0, Color32::from_gray(0x2e));
    let step = ((time * 3.0) as usize) % 3;
    for k in 0..3 {
        let c = Pos2::new(field.right() - 16.0 + 5.0 * k as f32, field.bottom() - 6.0);
        p.circle_filled(c, 1.3, Color32::from_gray(if k == step { 0x9a } else { 0x55 }));
    }
}

/// Test hook (`GRAINEO_DARKROOM_SHELF_SCROLL=<points>`, only with `GRAINEO_DATA_DIR`): scroll the film shelf once on
/// its first draw (screenshots of the colour section).
pub fn test_scroll() -> Option<f32> {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    crate::env::var_os("GRAINEO_DATA_DIR")?;
    let y = crate::env::var("GRAINEO_DARKROOM_SHELF_SCROLL").ok()?.parse::<f32>().ok()?;
    (!DONE.swap(true, Ordering::SeqCst)).then_some(y)
}

/// Crops of the films for the current photo, settings state and position.
#[derive(Default)]
pub struct Crops {
    key: u64,
    index: usize,
    fresh: HashMap<String, TextureHandle>,
    /// crops of the previous state of the same photo: shown until the new ones are there
    stale: HashMap<String, TextureHandle>,
    /// failed for this state: not requested again
    failed: HashSet<String>,
    /// key of the job on its way and when it was sent
    pending: Option<u64>,
    sent_at: f64,
    retry_at: f64,
    /// candidate key and since when it is unchanged
    since: (u64, f64),
    valid: Arc<AtomicU64>,
    /// Lightness grid of the open photo (from its first developed view), for choosing the spot
    grid: Option<(std::path::PathBuf, Arc<LumaGrid>)>,
    /// Known faces of the photo (library image ID), read once in the background
    faces: Option<(i64, Arc<std::sync::Mutex<Option<Vec<library::people::FaceInfo>>>>)>,
    /// Chosen spot in sensor pixels for (photo, crop) and since when it is awaited
    spot: Option<(u64, [f32; 2])>,
    spot_wait: (u64, f64),
}

/// Lightness L* (0 … 100) of the developed photo on a coarse grid (sensor orientation, long side [`GRID`] cells).
pub struct LumaGrid {
    pub w: usize,
    pub h: usize,
    pub l: Vec<f32>,
}

/// Long side of the lightness grid.
const GRID: usize = 192;

/// Lightness grid from the developed view (sRGB, `w` × `h`): mean of a sparse sample of each cell, then L*. A few ms
/// once per photo.
pub fn luma_grid(w: usize, h: usize, rgb: &[u8]) -> LumaGrid {
    let k = (w.max(h) as f32 / GRID as f32).max(1.0);
    let (gw, gh) = (((w as f32 / k).round() as usize).max(1), ((h as f32 / k).round() as usize).max(1));
    let mut l = vec![0.0; gw * gh];
    for gy in 0..gh {
        let (y0, y1) = (gy * h / gh, ((gy + 1) * h / gh).max(gy * h / gh + 1).min(h));
        for gx in 0..gw {
            let (x0, x1) = (gx * w / gw, ((gx + 1) * w / gw).max(gx * w / gw + 1).min(w));
            let (mut sum, mut n) = ([0u32; 3], 0u32);
            for y in (y0..y1).step_by(2) {
                for x in (x0..x1).step_by(2) {
                    let i = (y * w + x) * 3;
                    if let Some(p) = rgb.get(i..i + 3) {
                        sum[0] += p[0] as u32;
                        sum[1] += p[1] as u32;
                        sum[2] += p[2] as u32;
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            l[gy * gw + gx] = crate::darkroom_zones::lstar([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]);
        }
    }
    LumaGrid { w: gw, h: gh, l }
}

/// Weight of a lightness for the spot: 1 for the middle tones (L* 25 … 75), falling to 0 at 10 and 90.
fn mid_weight(l: f32) -> f32 {
    if l < 25.0 {
        ((l - 10.0) / 15.0).clamp(0.0, 1.0)
    } else if l > 75.0 {
        ((90.0 - l) / 15.0).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// A telling place for the 100 % crops: the window (`win`: size relative to the photo) inside `bounds` (x0, y0, x1,
/// y1 relative; the kept crop) with the best mix of middle tones and structure (lightness gradients), away from the
/// edges and slightly preferring the centre. Deterministic. Returns the centre relative to the photo.
pub fn best_spot(g: &LumaGrid, bounds: [f32; 4], win: [f32; 2]) -> [f32; 2] {
    let (w, h) = (g.w, g.h);
    let centre = [(bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0];
    if w < 3 || h < 3 {
        return centre;
    }
    // per cell: middle-tone weight and gradient energy; summed tables for window means
    let mut mid = vec![0f64; (w + 1) * (h + 1)];
    let mut grad = vec![0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            let v = g.l[y * w + x];
            let gx = g.l[y * w + (x + 1).min(w - 1)] - g.l[y * w + x.saturating_sub(1)];
            let gy = g.l[(y + 1).min(h - 1) * w + x] - g.l[y.saturating_sub(1) * w + x];
            let i = (y + 1) * (w + 1) + x + 1;
            mid[i] = mid_weight(v) as f64 + mid[i - 1] + mid[i - w - 1] - mid[i - w - 2];
            // capped, so that a single hard edge (black against white) does not win
            let e = (gx.abs() + gy.abs()).min(30.0) as f64;
            grad[i] = e + grad[i - 1] + grad[i - w - 1] - grad[i - w - 2];
        }
    }
    let sum = |t: &[f64], x0: usize, y0: usize, x1: usize, y1: usize| t[y1 * (w + 1) + x1] - t[y0 * (w + 1) + x1] - t[y1 * (w + 1) + x0] + t[y0 * (w + 1) + x0];
    let ww = ((win[0] * w as f32).ceil() as usize).clamp(3, w);
    let wh = ((win[1] * h as f32).ceil() as usize).clamp(3, h);
    // centres only in the inner part of the kept crop (10 % margin on each side)
    let (bx0, bx1) = (bounds[0] + 0.1 * (bounds[2] - bounds[0]), bounds[2] - 0.1 * (bounds[2] - bounds[0]));
    let (by0, by1) = (bounds[1] + 0.1 * (bounds[3] - bounds[1]), bounds[3] - 0.1 * (bounds[3] - bounds[1]));
    let half = [(bounds[2] - bounds[0]).max(1e-3) / 2.0, (bounds[3] - bounds[1]).max(1e-3) / 2.0];
    let n = (ww * wh) as f64;
    let mut best: Option<(f64, [f32; 2])> = None;
    for y0 in 0..=(h - wh) {
        for x0 in 0..=(w - ww) {
            let c = [(x0 as f32 + ww as f32 / 2.0) / w as f32, (y0 as f32 + wh as f32 / 2.0) / h as f32];
            if c[0] < bx0 || c[0] > bx1 || c[1] < by0 || c[1] > by1 {
                continue;
            }
            let m = sum(&mid, x0, y0, x0 + ww, y0 + wh) / n;
            let e = sum(&grad, x0, y0, x0 + ww, y0 + wh) / n;
            let d = (((c[0] - centre[0]) / half[0]).powi(2) + ((c[1] - centre[1]) / half[1]).powi(2)).sqrt().min(1.0) as f64;
            let score = m * m * e.sqrt() * (1.0 - 0.35 * d);
            if best.is_none_or(|(b, _)| score > b) {
                best = Some((score, c));
            }
        }
    }
    match best {
        Some((score, c)) if score > 0.0 => c,
        _ => centre,
    }
}

/// Centre of a face for the crop: middle across, eye height (a good third from the top), in the upright photo
/// relative 0 … 1 before the user's quarter turns.
fn face_point(g: &library::people::FaceInfo) -> [f32; 2] {
    [g.x + g.b / 2.0, g.y + 0.4 * g.h]
}

impl Crops {
    /// Remember the lightness grid of the open photo from its developed view (once per photo; called with every
    /// result of the preview development).
    pub fn note_view(&mut self, path: &std::path::Path, w: usize, h: usize, rgb: &[u8]) {
        if self.grid.as_ref().is_some_and(|(p, _)| p == path) {
            return;
        }
        self.grid = Some((path.to_path_buf(), Arc::new(luma_grid(w, h, rgb))));
    }

    /// Films whose crop failed for the current state (no placeholder for them).
    pub fn failed_ids(&self) -> HashSet<String> {
        self.failed.clone()
    }

    /// Result of [`crate::jobs::Job::FilmCrops`].
    pub fn receive(&mut self, ctx: &egui::Context, index: usize, key: u64, done: Vec<(String, Option<(usize, usize, Vec<u8>)>)>, not_ready: bool, end: bool) {
        if end && self.pending == Some(key) {
            self.pending = None;
            if not_ready {
                self.retry_at = ctx.input(|i| i.time) + RETRY;
            }
        }
        if key != self.key || index != self.index {
            return;
        }
        for (id, img) in done {
            match img {
                Some((w, h, rgb)) => {
                    let img = egui::ColorImage::from_rgb([w, h], &rgb);
                    match self.fresh.get_mut(&id) {
                        Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                        None => {
                            let t = ctx.load_texture(format!("film-crop-{id}"), img, egui::TextureOptions::NEAREST);
                            self.fresh.insert(id, t);
                        }
                    }
                }
                None => {
                    self.failed.insert(id);
                }
            }
        }
    }
}

/// Wait at most this long for the faces or the lightness grid before falling back to the centre.
const SPOT_WAIT: f64 = 4.0;

impl Crops {
    /// The textures shown now (new ones over old ones), without requesting anything.
    fn shown(&self) -> HashMap<String, TextureHandle> {
        let mut out = self.stale.clone();
        out.extend(self.fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
        out
    }

    /// Spot of the crops (sensor pixels) for the fitted photo, once per photo and crop: the largest known face lying
    /// in the crop, else [`best_spot`] on the lightness grid. `None` while the faces (read once in the background)
    /// or the grid (from the first developed view) are still on their way.
    #[allow(clippy::too_many_arguments)]
    fn spot_for(&mut self, ctx: &egui::Context, index: usize, image: i64, path: &std::path::Path, s: &develop::Settings, [fw, fh]: [usize; 2], cam: u8, ppp: f32, now: f64) -> Option<[f32; 2]> {
        let frame = develop::geometry::Frame::from_settings(s, fw, fh, cam);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (index, path, image, s.rotate, fw, fh, cam).hash(&mut h);
            for v in s.crop.iter().chain([&s.crop_angle]).chain(&[frame.pv, frame.ph]) {
                v.to_bits().hash(&mut h);
            }
            h.finish().max(1)
        };
        if let Some((k, p)) = self.spot {
            if k == key {
                return Some(p);
            }
        }
        if self.spot_wait.0 != key {
            self.spot_wait = (key, now);
        }
        let late = now - self.spot_wait.1 > SPOT_WAIT;
        let to_src = |p: [f32; 2]| frame.to_src(p);
        // 1. faces from the library (only photos of the library; read once in the background, never while drawing)
        let mut faces_known = image <= 0;
        if image > 0 {
            if self.faces.as_ref().is_none_or(|(i, _)| *i != image) {
                let slot = Arc::new(std::sync::Mutex::new(None));
                self.faces = Some((image, slot.clone()));
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let faces = library::db::Db::standard().map(|db| library::people::faces_to_image(&db.conn, image)).unwrap_or_default();
                    *slot.lock().unwrap() = Some(faces);
                    ctx.request_repaint();
                });
            }
            if let Some((_, slot)) = &self.faces {
                if let Some(faces) = slot.lock().unwrap().as_ref() {
                    faces_known = true;
                    let (ow, oh) = frame.oriented();
                    let r = s.rotate % 4;
                    let face = faces
                        .iter()
                        .map(|g| {
                            let d = crate::face_frames::rotate_quarters(face_point(g), r);
                            (g.b * g.h, frame.ori_to_persp([d[0] * ow, d[1] * oh]))
                        })
                        .filter(|(_, p)| {
                            let u = frame.from_ori(*p);
                            (0.0..=1.0).contains(&u[0]) && (0.0..=1.0).contains(&u[1])
                        })
                        .max_by(|a, b| a.0.total_cmp(&b.0));
                    if let Some((_, p)) = face {
                        let src = to_src(frame.from_ori(p));
                        self.spot = Some((key, src));
                        return Some(src);
                    }
                }
            }
        }
        // 2. middle tones with structure on the grid of the developed view
        let grid = self.grid.as_ref().filter(|(p, _)| p == path).map(|(_, g)| g.clone());
        let p = match grid {
            Some(g) if faces_known || late => {
                let b = frame.bounds();
                let bounds = [b[0] as f32 / fw as f32, b[1] as f32 / fh as f32, (b[0] + b[2]) as f32 / fw as f32, (b[1] + b[3]) as f32 / fh as f32];
                let (pw, ph) = (FIELD.x * ppp, FIELD.y * ppp);
                let (rw, rh) = if frame.q % 2 == 1 { (ph, pw) } else { (pw, ph) };
                let c = best_spot(&g, bounds, [rw / fw as f32, rh / fh as f32]);
                [c[0] * fw as f32, c[1] * fh as f32]
            }
            _ if late => to_src([0.5, 0.5]),
            _ => return None,
        };
        if crate::env::var_os("GRAINEO_LOG").is_some() {
            eprintln!("film shelf: spot {:.0},{:.0} of {fw}×{fh}", p[0], p[1]);
        }
        self.spot = Some((key, p));
        Some(p)
    }
}

impl App {
    /// 100 % crops of the films for the shelf: returns the existing textures (new ones over old ones) and requests the
    /// `requested` ones (the visible rows) that are missing, once the settings and the position have rested.
    pub(crate) fn darkroom_shelf_crops(&mut self, ui: &egui::Ui, requested: &[String]) -> HashMap<String, TextureHandle> {
        let Some(c) = &self.current else { return HashMap::new() };
        let index = c.index;
        let ppp = ui.ctx().pixels_per_point();
        let [fw, fh] = c.loaded.full_size;
        let now = ui.input(|i| i.time);
        // always the telling place of the photo (face, else middle tones with structure), fixed per photo and crop:
        // zooming and panning neither change the crops nor compute anything; not yet known: wait
        let image = self.entries[index].image;
        let path = self.entries[index].path.clone();
        let settings = self.entries[index].settings.clone();
        let cam = c.loaded.cal.orientation;
        let q = develop::geometry::quarters(&settings, cam);
        let Some(center) = self.darkroom.shelf.spot_for(ui.ctx(), index, image, &path, &settings, [fw, fh], cam, ppp, now) else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
            return self.darkroom.shelf.shown();
        };
        let Some(c) = &self.current else { return HashMap::new() };
        // region in the sensor image: the field in pixels, turned with the image
        let (pw, ph) = ((FIELD.x * ppp).round() as usize, (FIELD.y * ppp).round() as usize);
        let (rw, rh) = if q % 2 == 1 { (ph, pw) } else { (pw, ph) };
        let (rw, rh) = (rw.min(fw).max(1), rh.min(fh).max(1));
        let x = (center[0] - rw as f32 / 2.0).round().clamp(0.0, (fw - rw) as f32) as usize;
        let y = (center[1] - rh as f32 / 2.0).round().clamp(0.0, (fh - rh) as f32) as usize;
        let region = [x, y, rw, rh];
        let base = self.entries[index].settings.clone();
        let key = {
            use std::hash::{Hash, Hasher};
            let mut b = base.clone();
            b.film.stock.clear();
            b.film.active = false;
            b.film.paper = None;
            b.film.film_dev = 0.0;
            b.film.paper_grade = -1.0;
            b.film.push = 0.0;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(&b).unwrap_or_default().hash(&mut h);
            (index, region, q, raw::film_profile::generation()).hash(&mut h);
            h.finish().max(1)
        };
        let st = &mut self.darkroom.shelf;
        if st.since.0 != key {
            st.since = (key, now);
        }
        if st.key != key && (now - st.since.1 >= REST || st.key == 0 || st.index != index) {
            if st.index == index {
                let fresh = std::mem::take(&mut st.fresh);
                st.stale.extend(fresh);
            } else {
                st.stale.clear();
                st.fresh.clear();
            }
            st.key = key;
            st.index = index;
            st.failed.clear();
            st.retry_at = 0.0;
            st.valid.store(key, Ordering::SeqCst);
        }
        let mut out = st.stale.clone();
        out.extend(st.fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mut missing: Vec<String> = requested.iter().filter(|id| !st.fresh.contains_key(*id) && !st.failed.contains(*id)).cloned().collect();
        missing.dedup();
        // safety: a job that never answered does not block the shelf for ever
        if st.pending.is_some() && now - st.sent_at > 30.0 {
            st.pending = None;
        }
        let resting = st.key == key && now - st.since.1 >= REST;
        if !missing.is_empty() && st.pending.is_none() && resting && now >= st.retry_at {
            let list: Vec<(String, develop::Settings)> = missing
                .into_iter()
                .map(|id| {
                    let mut s = base.clone();
                    crate::darkroom::set_film(&mut s, Some(id.clone()));
                    (id, s)
                })
                .collect();
            st.pending = Some(key);
            st.sent_at = now;
            self.worker.send(crate::jobs::Job::FilmCrops { index, session: c.loaded.session.clone(), foreign: c.loaded.previews.foreign.clone(), list, region, q, key, valid: st.valid.clone() });
        }
        let st = &self.darkroom.shelf;
        if st.pending.is_some() || (!requested.is_empty() && requested.iter().any(|id| !st.fresh.contains_key(id) && !st.failed.contains(id))) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every film and paper on the shelf has its characterisation, short enough for two lines (papers one line).
    #[test]
    fn every_stock_has_a_short_characterisation() {
        for s in develop::film::films().iter().chain(develop::film::papers()) {
            let t = character(&s.id).unwrap_or_else(|| panic!("no characterisation for {}", s.id));
            assert!(t.chars().count() <= 40, "{}: {t:?} too long", s.id);
            for (lang, tsv) in [("en", include_str!("../locales/en-ui.tsv")), ("es", include_str!("../locales/es-ui.tsv")), ("fr", include_str!("../locales/fr-ui.tsv")), ("ja", include_str!("../locales/ja-ui.tsv"))] {
                assert!(tsv.lines().any(|l| l.split('\t').next() == Some(t)), "{t:?} missing in {lang}");
            }
        }
    }

    /// Synthetic photo: a large dark centre (as the black trousers in DP2M5418), flat middle grey around it and a
    /// structured middle-tone area at the top left: the spot lands in the structured area, not in the centre.
    #[test]
    fn spot_prefers_structured_middle_tones_over_a_dark_centre() {
        let (w, h) = (1200usize, 800usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v: u8 = if (x as i32 - 600).abs() < 250 && (y as i32 - 400).abs() < 180 {
                    12
                } else if (200..420).contains(&x) && (120..300).contains(&y) {
                    if (x / 12 + y / 12) % 2 == 0 { 90 } else { 160 }
                } else {
                    120
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(v);
            }
        }
        let g = luma_grid(w, h, &rgb);
        assert_eq!(g.w.max(g.h), GRID);
        let c = best_spot(&g, [0.0, 0.0, 1.0, 1.0], [128.0 / w as f32, 80.0 / h as f32]);
        let (x, y) = (c[0] * w as f32, c[1] * h as f32);
        assert!((200.0..420.0).contains(&x) && (120.0..300.0).contains(&y), "spot {x},{y}");
        // deterministic
        assert_eq!(c, best_spot(&g, [0.0, 0.0, 1.0, 1.0], [128.0 / w as f32, 80.0 / h as f32]));
        // within a crop that leaves out the structured area, the spot stays inside the crop
        let c = best_spot(&g, [0.5, 0.5, 1.0, 1.0], [0.05, 0.05]);
        assert!(c[0] >= 0.5 && c[1] >= 0.5, "{c:?}");
    }

    /// A photo without any structure (flat) keeps the centre of the crop.
    #[test]
    fn spot_of_a_flat_photo_is_the_centre() {
        let g = LumaGrid { w: 30, h: 20, l: vec![50.0; 600] };
        assert_eq!(best_spot(&g, [0.0, 0.0, 1.0, 1.0], [0.05, 0.05]), [0.5, 0.5]);
    }

    /// Cine stocks get the reel, papers the sheet of paper, the rest a still format.
    #[test]
    fn formats_follow_the_profile_data() {
        for s in develop::film::films().iter().chain(develop::film::papers()) {
            let f = format_of(s);
            match s.id.as_str() {
                id if id.starts_with("kodak_vision3") || id == "kodak_doublex" || id == "kodak_2302" || id == "kodak_2383" => assert_eq!(f, Format::Cine, "{id}"),
                id if id.contains("endura") || id.contains("multigrade") => assert_eq!(f, Format::Paper, "{id}"),
                "kodak_portra_400" => assert_eq!(f, Format::Roll),
                "agfa_apx_100" | "kodak_ektar_100" => assert_eq!(f, Format::Small),
                _ => {}
            }
        }
    }
}
