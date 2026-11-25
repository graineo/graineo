//! Darkroom, phase 3: border and frame of the print and the "On paper" viewing mode.
//!
//! - **Border and frame** (`film.print_border`, `film.print_border_bottom`, `film.film_border`): white
//!   paper border and optionally a black film border with rounded corners. The darkroom draws
//!   it around the photo; it only goes into the file when exporting "as print" ([`develop::film::apply_border`]).
//! - **On paper**: shows the photo with paper white (slightly tinted, never pure white, warm or cool
//!   with paper tone) in darker surroundings. Display only: settings and export stay untouched.

use develop::film::{self, FilmSettings, BorderFractions};
use eframe::egui;
use egui::{Color32, Rect, Vec2};

use crate::{lr, App};

/// Extra margin around the sheet in "On paper" mode (fraction) so the surroundings stay visible.
const SURROUND: f32 = 1.07;

/// The sheet around the photo: border, paper color and display mode.
pub struct Sheet {
    pub weights: Option<BorderFractions>,
    /// Paper white (sRGB-encoded, 0…1)
    pub paper: [f32; 3],
    pub on_paper: bool,
    /// Paper texture (kind, strength), see `film::paper_texture`
    pub structure: (u8, f32),
    /// Edge print of the film strip (empty without strip)
    pub strip_text: String,
    /// Place of the image info box on screen: perforation, edge print and frame numbers leave it free, so that the
    /// capture data never lie over holes or print (display only)
    pub free_zone: Option<Rect>,
}

fn color(c: [f32; 3]) -> Color32 {
    Color32::from_rgb((c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8)
}

impl Sheet {
    /// Fractions of the long image edge: (all-round film border + paper border top/left/right, bottom).
    fn borders(&self) -> (f32, f32) {
        match self.weights {
            Some(a) => (a.film + a.paper, a.film + a.bottom),
            None => (0.0, 0.0),
        }
    }

    /// Size of the whole sheet in image pixels (photo plus borders, with surroundings in "On paper" mode).
    pub fn total(&self, image: Vec2) -> Vec2 {
        let long_side = image.x.max(image.y);
        let (side, bottom) = self.borders();
        let (bmx, bmy) = self.weights.map_or((0.0, 0.0), |a| film::band_extent(&a, image.x, image.y));
        let v = Vec2::new(image.x + 2.0 * (side * long_side + bmx), image.y + (side + bottom) * long_side + 2.0 * bmy);
        if self.on_paper { v * SURROUND } else { v }
    }

    /// Offset of the image centre against the centre of the sheet (wider bottom → photo slightly higher), in screen points.
    pub fn offset(&self, image: Vec2, scale: f32) -> Vec2 {
        let long_side = image.x.max(image.y);
        let (side, bottom) = self.borders();
        Vec2::new(0.0, (side - bottom) * long_side * 0.5 * scale)
    }

    /// Tint the photo is drawn with: in "On paper" mode white becomes paper white.
    pub fn tint(&self) -> Color32 {
        if self.on_paper { color(self.paper) } else { Color32::WHITE }
    }

    /// Surroundings, shadow, paper and film border behind the photo (`rect` = photo area on screen).
    pub fn background(&self, painter: &egui::Painter, area: Rect, rect: Rect) {
        if self.on_paper {
            painter.rect_filled(area, 0.0, crate::lr::gray(0x04));
        }
        let Some(a) = self.weights else {
            if self.on_paper {
                shadow(painter, rect);
            }
            return;
        };
        let long_side = rect.width().max(rect.height());
        let (fb, pb, ub) = (a.film * long_side, a.paper * long_side, a.bottom * long_side);
        let (bmx, bmy) = film::band_extent(&a, rect.width(), rect.height());
        let sheet = Rect::from_min_max(rect.min - Vec2::new(fb + pb + bmx, fb + pb + bmy), rect.max + Vec2::new(fb + pb + bmx, fb + ub + bmy));
        if self.on_paper {
            shadow(painter, sheet);
        }
        painter.rect_filled(sheet, 0.0, color(self.paper));
        let (kind, amp) = self.structure;
        if kind > 0 && amp > 0.0 {
            // Texture as a small, tiling darkening texture over the paper (cached per kind and strength)
            let tex = structure_texture(painter.ctx(), kind, amp);
            let tile = 256.0;
            let uv = Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(sheet.width() / tile, sheet.height() / tile));
            painter.image(tex.id(), sheet, uv, Color32::WHITE);
        }
        if fb > 0.0 {
            let film = Rect::from_min_max(rect.min - Vec2::new(fb + bmx, fb + bmy), rect.max + Vec2::new(fb + bmx, fb + bmy));
            let corner = if a.paper > 0.0 { a.corner() * long_side } else { 0.0 };
            painter.rect_filled(film, corner, Color32::BLACK);
            if a.band > 0.0 {
                self.draw_strip(painter, film, rect, long_side);
            }
        }
    }
}

/// Tile (256 × 256) of the paper texture as semi-transparent black; remembered per kind and strength.
fn structure_texture(ctx: &egui::Context, kind: u8, amp: f32) -> egui::TextureHandle {
    let id = egui::Id::new(("dk-papier-struktur", kind, (amp * 1000.0) as u32));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t;
    }
    let n = 256usize;
    let mut px = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            // Edge values mirrored so that the tiles join seamlessly
            let (fx, fy) = (if x < n / 2 { x } else { n - 1 - x } as f32, if y < n / 2 { y } else { n - 1 - y } as f32);
            let v = develop::film::texture_value(kind, fx, fy);
            let dark = (amp * 255.0 * 2.2 * (0.5 - 0.5 * v)).clamp(0.0, 255.0) as u8;
            px.push(Color32::from_black_alpha(dark));
        }
    }
    let t = ctx.load_texture("dk-papier-struktur", egui::ColorImage { size: [n, n], source_size: egui::vec2(n as f32, n as f32), pixels: px }, egui::TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..egui::TextureOptions::LINEAR });
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

impl Sheet {
    /// Perforation, edge print and frame numbers on the film strip (display; the export draws the same in `apply_border`).
    fn draw_strip(&self, painter: &egui::Painter, film: Rect, image: Rect, long_side: f32) {
        // nothing of the strip under the image info box (a little air around it)
        let zone = self.free_zone.map(|z| z.expand(3.0));
        let free = |r: Rect| zone.is_none_or(|z| !z.intersects(r));
        let paper = color(self.paper);
        let amber = Color32::from_rgb(0xf2, 0xd0, 0x7a);
        let (pitch, lb, lh, hole_mid) = (film::HOLE_PITCH * long_side, film::HOLE_W * long_side, film::HOLE_H * long_side, film::HOLE_CENTER * long_side);
        let th = 0.0105 * long_side;
        let portrait = image.height() > image.width();
        let font = egui::FontId::monospace((th * 1.15).max(5.0));
        let label = format!("{}  >", self.strip_text);
        let r = 0.004 * long_side;
        if !portrait {
            let n = (film.width() / pitch).floor() as i32;
            let start = film.left() + (film.width() - n as f32 * pitch) / 2.0 + (pitch - lb) / 2.0;
            for i in 0..n {
                let x = start + i as f32 * pitch;
                for hole in [Rect::from_min_size(egui::pos2(x, film.top() + hole_mid - lh / 2.0), Vec2::new(lb, lh)), Rect::from_min_size(egui::pos2(x, film.bottom() - hole_mid - lh / 2.0), Vec2::new(lb, lh))] {
                    if free(hole) {
                        painter.rect_filled(hole, r, paper);
                    }
                }
            }
            let top = (film.top() + hole_mid + lh / 2.0 + image.top()) / 2.0;
            let bottom = (image.bottom() + film.bottom() - hole_mid - lh / 2.0) / 2.0;
            if !self.strip_text.is_empty() {
                let g = painter.layout_no_wrap(label.clone(), font.clone(), amber);
                let advance = g.size().x + 0.035 * long_side;
                let mut x = image.left() + 0.004 * long_side;
                while x + g.size().x < image.right() {
                    let at = egui::pos2(x, top - g.size().y / 2.0);
                    if free(Rect::from_min_size(at, g.size())) {
                        painter.galley(at, painter.layout_no_wrap(label.clone(), font.clone(), amber), amber);
                    }
                    x += advance;
                }
            }
            for (number, x) in [("> 12", image.left() + 0.01 * long_side), ("12A", image.center().x)] {
                let g = painter.layout_no_wrap(number.to_string(), font.clone(), amber);
                let at = egui::pos2(x, bottom - g.size().y / 2.0);
                if free(Rect::from_min_size(at, g.size())) {
                    painter.galley(at, g, amber);
                }
            }
        } else {
            let n = (film.height() / pitch).floor() as i32;
            let start = film.top() + (film.height() - n as f32 * pitch) / 2.0 + (pitch - lb) / 2.0;
            for i in 0..n {
                let y = start + i as f32 * pitch;
                for hole in [Rect::from_min_size(egui::pos2(film.left() + hole_mid - lh / 2.0, y), Vec2::new(lh, lb)), Rect::from_min_size(egui::pos2(film.right() - hole_mid - lh / 2.0, y), Vec2::new(lh, lb))] {
                    if free(hole) {
                        painter.rect_filled(hole, r, paper);
                    }
                }
            }
            let left = (film.left() + hole_mid + lh / 2.0 + image.left()) / 2.0;
            if !self.strip_text.is_empty() {
                let advance = painter.layout_no_wrap(label.clone(), font.clone(), amber).size().x + 0.035 * long_side;
                let mut y = image.top() + 0.004 * long_side;
                loop {
                    let g = painter.layout_no_wrap(label.clone(), font.clone(), amber);
                    if y + g.size().x >= image.bottom() {
                        break;
                    }
                    // rotated by 90°: text runs from top to bottom
                    let (h, len) = (g.size().y, g.size().x);
                    if free(Rect::from_min_size(egui::pos2(left - h / 2.0, y), Vec2::new(h, len))) {
                        painter.add(egui::epaint::TextShape::new(egui::pos2(left + h / 2.0, y), g, amber).with_angle(std::f32::consts::FRAC_PI_2));
                    }
                    y += advance;
                }
            }
            // frame numbers on the right side, also from top to bottom
            let right = (image.right() + film.right() - hole_mid - lh / 2.0) / 2.0;
            for (number, y) in [("> 12", image.top() + 0.01 * long_side), ("12A", image.center().y)] {
                let g = painter.layout_no_wrap(number.to_string(), font.clone(), amber);
                let (h, len) = (g.size().y, g.size().x);
                if free(Rect::from_min_size(egui::pos2(right - h / 2.0, y), Vec2::new(h, len))) {
                    painter.add(egui::epaint::TextShape::new(egui::pos2(right + h / 2.0, y), g, amber).with_angle(std::f32::consts::FRAC_PI_2));
                }
            }
        }
    }
}

/// Soft shadow around a sheet (several translucent rectangles).
fn shadow(painter: &egui::Painter, r: Rect) {
    for i in 1..=6 {
        let d = i as f32 * 3.0;
        painter.rect_filled(r.expand(d).translate(Vec2::new(0.0, d * 0.5)), 4.0 + d, Color32::from_black_alpha(18));
    }
}

/// Settings without border: a normal export (not "as print") does not change the image.
pub fn without_border(mut s: develop::Settings) -> develop::Settings {
    s.film.print_border = 0.0;
    s.film.print_border_bottom = 0.0;
    s.film.film_border = false;
    s.film.film_strip = false;
    s
}

impl App {
    /// The sheet of the open photo, if a border is set or "On paper" is on.
    pub(crate) fn darkroom_sheet(&self, index: usize) -> Option<Sheet> {
        if !self.darkroom_active() {
            return None;
        }
        let f = &self.entries.get(index)?.settings.film;
        if !f.is_active() {
            return None; // without film there is no paper
        }
        let weights = film::border_fractions(f);
        if weights.is_none() && !self.darkroom.on_paper {
            return None;
        }
        Some(Sheet { weights, paper: film::paper_white(f), on_paper: self.darkroom.on_paper, structure: film::paper_texture(f), strip_text: if f.film_strip { film::strip_text(f) } else { String::new() }, free_zone: None })
    }

    /// "Border & frame" sliders on the right side.
    pub(crate) fn darkroom_border_sliders(&mut self, ui: &mut egui::Ui, index: usize) {
        let d = FilmSettings::default();
        let f = &mut self.entries[index].settings.film;
        if !f.is_active() {
            return;
        }
        let changed = f.print_border != d.print_border || f.print_border_bottom != d.print_border_bottom || f.film_border != d.film_border || f.film_strip != d.film_strip || f.paper_white != d.paper_white;
        if lr::section_reset(ui, "Rand & Rahmen", true, changed, |ui| {
            let mut t = f.print_border as f64;
            if lr::slider(ui, "Papierrand", &mut t, 0.0..=15.0, 0.0, 0.1) {
                f.print_border = t as f32;
            }
            let mut u = f.print_border_bottom as f64;
            if lr::slider_hint(ui, "Unten zusätzlich", &mut u, 0.0..=10.0, 0.0, 0.1, "Wird zum Papierrand addiert: unten etwas breiter wirkt ausgewogener (klassisch 10–25 % mehr).") {
                f.print_border_bottom = u as f32;
            }
            ui.vertical(|ui| {
                lr::field_label(ui, crate::tr("Filmrand"));
                let mut style = if f.film_strip { 2u8 } else if f.film_border { 1 } else { 0 };
                if lr::segmented(ui, &mut style, &[(0, "Aus"), (1, "Rand"), (2, "Streifen")]) {
                    f.film_border = style == 1;
                    f.film_strip = style == 2;
                }
            });
            ui.vertical(|ui| {
                lr::field_label(ui, crate::tr("Papierweiß"));
                lr::segmented(ui, &mut f.paper_white, &[(0, "Warm"), (1, "Neutral"), (2, "Creme")]);
            });
        }) {
            f.print_border = d.print_border;
            f.print_border_bottom = d.print_border_bottom;
            f.film_border = d.film_border;
            f.film_strip = d.film_strip;
            f.paper_white = d.paper_white;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;
    use egui::Pos2;

    /// Whole window with a sample image; provides context and app.
    fn app_with_image(name: &str) -> (egui::Context, App) {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-dk-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(1800, 1200, image::Rgb([110, 80, 60])).save(folder.join("sample.png")).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(folder);
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (ctx, a)
    }

    fn image(ctx: &egui::Context, a: &mut App, time: &mut f64) {
        *time += 0.5;
        let mut input = egui::RawInput { time: Some(*time), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
    }

    /// Border and "On paper" in the whole window: the photo shrinks for the border, never a border without film.
    #[test]
    fn border_and_on_paper_in_full_window() {
        if film::films().is_empty() {
            return;
        }
        let (ctx, mut a) = app_with_image("rand");
        let mut time = 10.0;
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        image(&ctx, &mut a, &mut time);
        image(&ctx, &mut a, &mut time);
        let i = a.current.as_ref().unwrap().index;
        let without = a.image_rect.expect("image").width();
        assert!(a.darkroom_sheet(i).is_none(), "no sheet without border and without On paper");

        // Border: the photo gets smaller so the sheet fits the area
        {
            let f = &mut a.entries[i].settings.film;
            f.print_border = 10.0;
            f.print_border_bottom = 5.0;
            f.film_border = true;
        }
        image(&ctx, &mut a, &mut time);
        image(&ctx, &mut a, &mut time);
        let with_border = a.image_rect.expect("image").width();
        assert!(with_border < without * 0.9, "smaller with border: {with_border} vs {without}");
        assert!(a.darkroom_sheet(i).is_some());

        // On paper: display only, settings stay
        let before = a.entries[i].settings.film.clone();
        a.darkroom.on_paper = true;
        image(&ctx, &mut a, &mut time);
        assert!(a.darkroom_sheet(i).is_some_and(|b| b.on_paper));
        assert_eq!(a.entries[i].settings.film, before);
        let smaller = a.image_rect.expect("image").width();
        assert!(smaller < with_border, "surroundings make the photo a bit smaller still");

        // without film: no sheet
        a.entries[i].settings.film.active = false;
        assert!(a.darkroom_sheet(i).is_none());
        // leave the darkroom: normal view without sheet
        a.entries[i].settings.film.active = true;
        a.leave_darkroom();
        assert!(a.darkroom_sheet(i).is_none());
    }

    /// Export: with border only "as print"; otherwise the image stays unchanged.
    #[test]
    fn export_with_and_without_border() {
        if film::films().is_empty() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("graineo-dk-rand-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("quelle.png");
        image::RgbImage::from_pixel(400, 300, image::Rgb([120, 100, 80])).save(&source).unwrap();
        let doc = rawdev::RawDoc::open(&source, &|| true).unwrap().unwrap();
        let mut s = develop::Settings::default();
        s.film = FilmSettings { active: true, stock: "kodak_trix_400".into(), print_border: 10.0, print_border_bottom: 5.0, ..Default::default() };
        let format = develop::export::Format::Jpeg { quality: 90 };
        // normal export: border off → original size
        let normal = dir.join("normal.jpg");
        rawdev::export::export(&doc, &without_border(s.clone()), format, &normal).unwrap();
        let (w, h) = image::image_dimensions(&normal).unwrap();
        assert_eq!((w, h), (400, 300));
        // as print: border included (long edge 400: 40 px at the sides, 40 at the top, 60 at the bottom)
        let print = dir.join("abzug.jpg");
        rawdev::export::export(&doc, &s, format, &print).unwrap();
        let (w, h) = image::image_dimensions(&print).unwrap();
        assert_eq!((w, h), (400 + 80, 300 + 40 + 60));
        // the corner is paper white (not pure white)
        let rgb = image::open(&print).unwrap().to_rgb8();
        let p = rgb.get_pixel(2, 2).0;
        assert!(p[0] > 220 && p[0] < 255, "paper white {p:?}");
    }

    fn sheet(border: f32, bottom: f32, film: bool, on_paper: bool) -> Sheet {
        let fs = FilmSettings { active: true, stock: "kodak_trix_400".into(), print_border: border, print_border_bottom: bottom, film_border: film, ..Default::default() };
        Sheet { weights: film::border_fractions(&fs), paper: film::paper_white(&fs), on_paper, structure: film::paper_texture(&fs), strip_text: String::new(), free_zone: None }
    }

    #[test]
    fn total_size_and_offset() {
        let b = sheet(10.0, 5.0, false, false);
        let g = b.total(Vec2::new(200.0, 100.0));
        assert!((g.x - 240.0).abs() < 1e-3 && (g.y - 150.0).abs() < 1e-3, "{g:?}");
        // wider bottom: the photo sits higher than the sheet centre
        assert!(b.offset(Vec2::new(200.0, 100.0), 1.0).y < 0.0);
        // even border: no offset
        assert_eq!(sheet(10.0, 0.0, false, false).offset(Vec2::new(200.0, 100.0), 1.0), Vec2::ZERO);
        // On paper leaves room for the surroundings
        let ap = sheet(10.0, 0.0, false, true).total(Vec2::new(200.0, 100.0));
        assert!(ap.x > 240.0 * 1.05);
    }

    #[test]
    fn on_paper_tint_is_paper_white_never_pure_white() {
        let t = sheet(0.0, 0.0, false, true).tint();
        assert!(t.r() < 255 && t.g() < 255 && t.b() < 255 && t.r() > 220);
        assert_eq!(sheet(0.0, 0.0, false, false).tint(), Color32::WHITE);
        // with warm paper tone: more red than blue
        let fs = FilmSettings { active: true, stock: "kodak_trix_400".into(), paper_tone: 1.0, ..Default::default() };
        let b = Sheet { weights: None, paper: film::paper_white(&fs), on_paper: true, structure: (0, 0.0), strip_text: String::new(), free_zone: None };
        assert!(b.tint().r() > b.tint().b());
    }

    /// Where the image info box lies (top right), the film strip draws no perforation hole, edge print
    /// or frame number – the capture data never lie over them. Outside the box the strip stays complete.
    #[test]
    fn film_strip_leaves_the_info_box_free() {
        for (w, h) in [(600.0, 400.0), (400.0, 600.0)] {
            let fs = FilmSettings { active: true, stock: "kodak_trix_400".into(), print_border: 4.0, film_strip: true, ..Default::default() };
            let mut b = Sheet { weights: film::border_fractions(&fs), paper: film::paper_white(&fs), on_paper: false, structure: (0, 0.0), strip_text: "ILFORD HP5 PLUS".into(), free_zone: None };
            let area = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(w + 200.0, h + 200.0));
            // box at the top right of the area, over the strip
            let zone = Rect::from_min_max(egui::pos2(area.right() - 260.0, 80.0), egui::pos2(area.right() - 6.0, 150.0));
            let count = |b: &Sheet| {
                let ctx = egui::Context::default();
                let out = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1200.0, 1200.0))), ..Default::default() }, |ui| {
                    b.background(ui.painter(), area, Rect::from_center_size(area.center(), Vec2::new(w, h)));
                });
                let (mut inside, mut all) = (0, 0);
                for s in out.shapes {
                    let r = match &s.shape {
                        egui::Shape::Rect(r) if r.rect.width() < 40.0 && r.rect.height() < 40.0 => r.rect,
                        egui::Shape::Text(t) => {
                            let g = t.galley.size();
                            if t.angle != 0.0 { Rect::from_min_size(t.pos - Vec2::new(g.y, 0.0), Vec2::new(g.y, g.x)) } else { Rect::from_min_size(t.pos, g) }
                        }
                        _ => continue,
                    };
                    all += 1;
                    inside += usize::from(r.intersects(zone.expand(3.0)));
                }
                (inside, all)
            };
            let (inside_before, all_before) = count(&b);
            assert!(inside_before > 0, "{w}×{h}: the test box must lie over the strip");
            b.free_zone = Some(zone);
            let (inside, all) = count(&b);
            assert_eq!(inside, 0, "{w}×{h}: nothing of the strip under the info box");
            assert_eq!(all, all_before - inside_before, "{w}×{h}: elsewhere the strip stays complete");
        }
    }

    #[test]
    fn background_draws_without_crash() {
        let ctx = egui::Context::default();
        let _ = crate::ui_test::run(&ctx, egui::RawInput::default(), |ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(300.0, 200.0), egui::Sense::hover());
            let rect = r.shrink(40.0);
            for b in [sheet(8.0, 4.0, true, true), sheet(0.0, 0.0, false, true), sheet(5.0, 0.0, false, false)] {
                b.background(ui.painter(), r, rect);
            }
        });
    }
}
