//! Film shelf of the darkroom: a small preview of the whole photo per film row, a sign for the kind of film
//! (cartridge, roll film, sheet film, cine reel, photo paper; colour = genre, no maker colours or logos) and a short,
//! instructive characterisation per film and paper.
//!
//! The previews are the same tiles as in "Film & Look" ([`crate::film_previews`]): one small whole image per photo,
//! the film computed on it per row, visible rows first, the others quietly afterwards. Until a row's tile arrives it
//! shows a grey placeholder with a small computing sign ([`paint_waiting`]); after a change of the settings the old
//! tiles stay until the new ones are there.

use std::sync::atomic::Ordering;

use develop::film::{Category, StockInfo};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Vec2};


/// Size of the preview field in a shelf row (points); the whole photo is fitted into it.
pub const FIELD: Vec2 = Vec2::new(64.0, 40.0);
/// Height of a film row.
pub const ROW_H: f32 = 48.0;
/// Room for the sign on the right of a row.
pub const SIGN_W: f32 = 34.0;
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

/// Paint a tile: the whole photo fitted into the field (aspect kept, centred).
pub fn paint_tile(p: &egui::Painter, field: Rect, t: &TextureHandle, ppp: f32) {
    let r = crate::film_previews::fit(field, t.size_vec2(), ppp);
    p.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
}

/// Placeholder while a tile is being computed: a neutral mid grey (clearly not a dark photo) with three small dots
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
