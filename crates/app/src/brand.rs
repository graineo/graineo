//! Branding: app logo (top left), splash window at program start and the same
//! window as "About graineo" (stays until you click or press Esc).
//! Graphics in `crates/app/assets/` (made by `scripts/make-brand.py` from the vector sources in
//! `assets/brand/`); the name is set in Syne Medium (SIL OFL, `assets/fonts/`).

use eframe::egui;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};

use crate::lr;

const LOGO_PNG: &[u8] = include_bytes!("../assets/logo-64.png");
/// Word mark "graineo" (Syne Medium, aperture in the last "o"), light on transparent, 960 px wide.
const WORDMARK_PNG: &[u8] = include_bytes!("../assets/wordmark.png");
/// Position of the baseline in the word mark (share of its height, see scripts/make-brand.py).
const WORDMARK_BASELINE: f32 = 0.7656;
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-1024.png");

/// Minimum and maximum duration of the splash window (seconds) and fade-out time.
const SPLASH_MIN: f64 = 2.0;
const SPLASH_MAX: f64 = 3.5;
/// Fade-out of the start screen (window and dimming together), seconds; ease-out: quick start, soft end.
const SPLASH_FADE: f64 = 0.42;
/// Fade-out when the About window is closed, seconds.
const ABOUT_FADE: f64 = 0.25;
/// Dimming of the rest of the app behind the window (0..255, 140 = about 55 %).
const DIM_ALPHA: u8 = 140;
/// Soft dark veil behind word mark and subtitle: strength (0..255, 80 = about 31 %) and ellipse radii
/// as a fraction of the window width / height.
const VEIL_ALPHA: u8 = 80;
const VEIL_RX: f32 = 0.46;
const VEIL_RY: f32 = 0.34;
/// Grain size factor: template pixels -> physical pixels = pixels_per_point * this (0.375 = a bit more than a
/// third of a point per template pixel, finer than the template's canvas of about 1.5 pixels per point).
const GRAIN_SCALE: f32 = 0.375;
/// Window size in points (layout is designed for this width).
const WIN_W: f32 = 656.0;
const WIN_H: f32 = 438.0;

pub struct Brand {
    logo: Option<TextureHandle>,
    wordmark: Option<TextureHandle>,
    /// Film grain of the splash card (generated once per card size in pixels)
    grain: Option<(TextureHandle, [usize; 2])>,
    /// Time from which it fades out (None = still visible)
    fade_from: Option<f64>,
    done: bool,
    /// Opened as "About" window: does not fade out by itself
    about: bool,
    /// "Licences and trademarks" card is open (only from the About window)
    licenses: bool,
    /// Text block under the sub title that keeps the card open (start screen while the library is converted)
    hold: Option<Hold>,
}

/// Text block of the start card: title, current step, progress bar (0 … 1) and a note.
pub struct Hold {
    pub title: String,
    pub step: String,
    pub fraction: f32,
    pub note: String,
}

/// The word mark (Syne Medium, coloured aperture in the "o") as a texture, loaded once per context: wherever the
/// name stands as a title (empty library), not set in the interface font.
pub fn wordmark(ctx: &egui::Context) -> Option<TextureHandle> {
    let id = egui::Id::unique("brand-wordmark-shared");
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return Some(t);
    }
    let t = decode(WORDMARK_PNG).map(|i| ctx.load_texture("brand-wordmark-shared", i, TextureOptions::LINEAR))?;
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// Black-and-white film grain for the splash and About card: coarse in the dark areas, finer in the
/// bright ones, brightest at the lower right corner. Fixed seed, computed once per card size (CPU),
/// opaque inside the rounded corners (`corner_px`), transparent outside. `k` scales the grain sizes
/// from "template pixels" to physical pixels (pixels per point), so it looks equally coarse on Retina.
fn grain_image(w: usize, h: usize, corner_px: f32, k: f32) -> ColorImage {
    // chosen values: shape = corner (lower right), reach 0.76, bright 0.185, base black 0.002, softness 2.40,
    // grain size dark 2.9 / bright 0.7, strength 0.160, follows brightness 0.28, clumping 0.15, still
    const REACH: f32 = 0.76;
    const HI: f32 = 0.185;
    const LO: f32 = 0.002;
    const SOFT: f32 = 2.40;
    const SMAX: f32 = 2.9;
    const SMIN: f32 = 0.7;
    const AMP: f32 = 0.136;
    const FOLLOW: f32 = 0.28;
    const CLUMP: f32 = 0.15;
    const SEED: i32 = 1;
    let mulberry = |seed: i32| {
        let mut a = seed;
        move || -> f32 {
            a = a.wrapping_add(0x6D2B79F5u32 as i32);
            let mut t = (a ^ (((a as u32) >> 15) as i32)).wrapping_mul(1 | a);
            t = t.wrapping_add((t ^ (((t as u32) >> 7) as i32)).wrapping_mul(61 | t)) ^ t;
            (((t ^ (((t as u32) >> 14) as i32)) as u32) as f64 / 4294967296.0) as f32
        }
    };
    let n = w * h;
    // one layer: coarse grid of Gaussian values, smoothly interpolated, normalised to standard deviation 1
    let make_layer = |s: f32, seed: i32| -> Vec<f32> {
        let mut r = mulberry(seed);
        let (gw, gh) = ((w as f32 / s).ceil() as usize + 3, (h as f32 / s).ceil() as usize + 3);
        let g: Vec<f32> = (0..gw * gh)
            .map(|_| {
                let mut u = 0.0;
                while u == 0.0 {
                    u = r();
                }
                let v = r();
                (-2.0 * (u as f64).ln()).sqrt() as f32 * (2.0 * std::f64::consts::PI * v as f64).cos() as f32
            })
            .collect();
        let mut out = vec![0.0_f32; n];
        let (mut sum, mut sq) = (0.0_f64, 0.0_f64);
        for y in 0..h {
            let fy = y as f32 / s;
            let iy = fy.floor();
            let ty = fy - iy;
            let ty = ty * ty * (3.0 - 2.0 * ty);
            let iy = iy as usize;
            for x in 0..w {
                let fx = x as f32 / s;
                let ix = fx.floor();
                let tx = fx - ix;
                let tx = tx * tx * (3.0 - 2.0 * tx);
                let ix = ix as usize;
                let (a, b, c, d) = (g[iy * gw + ix], g[iy * gw + ix + 1], g[(iy + 1) * gw + ix], g[(iy + 1) * gw + ix + 1]);
                let v = (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty;
                out[y * w + x] = v;
                sum += v as f64;
                sq += (v * v) as f64;
            }
        }
        let m = sum / n as f64;
        let sd = (sq / n as f64 - m * m).max(1e-9).sqrt();
        for v in out.iter_mut() {
            *v = ((*v as f64 - m) / sd) as f32;
        }
        out
    };
    let sizes: Vec<f32> = [0.6_f32, 0.85, 1.2, 1.7, 2.4, 3.4, 4.8, 6.8, 9.0].iter().map(|s| s * k).collect();
    let layers: Vec<Vec<f32>> = sizes.iter().enumerate().map(|(i, &s)| make_layer(s, SEED * 131 + i as i32 * 977 + 1)).collect();
    let fine = make_layer(0.5 * k, SEED * 17 + 3);
    let asp = w as f32 / h as f32;
    let mut px = vec![0_u8; n * 4];
    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let i = y * w + x;
            // brightness field: corner at the lower right
            let d = (((u - 1.0) * asp).powi(2) + (v - 1.0).powi(2)).sqrt() / (REACH * asp);
            let t = (1.0 - d.min(1.0)).max(0.0);
            let l = LO + (HI - LO) * t.powf(SOFT);
            // grain size: coarse in the dark, fine in the bright
            let tb = ((l - LO) / (HI - LO).max(1e-4)).clamp(0.0, 1.0);
            let s = (SMAX * (1.0 - tb) + SMIN * tb) * k;
            let mut j = 0;
            while j < sizes.len() - 2 && sizes[j + 1] < s {
                j += 1;
            }
            let wgt = ((s - sizes[j]) / (sizes[j + 1] - sizes[j])).clamp(0.0, 1.0);
            let mut nz = layers[j][i] * (1.0 - wgt) + layers[j + 1][i] * wgt;
            if CLUMP > 0.0 {
                nz = nz * (1.0 - CLUMP) + (nz * 2.2).tanh() * 1.35 * CLUMP;
            }
            let f = 1.0 - FOLLOW + FOLLOW * (0.35 + 1.3 * (std::f32::consts::PI * (tb * 0.9 + 0.08).min(1.0)).sin());
            let g = nz * AMP * f;
            let val = (l + g * (l * 0.9 + 0.03).max(0.02) + fine[i] * 0.004).clamp(0.0, 1.0);
            let c = (255.0 * val.powf(1.0 / 2.2)).round();
            // rounded corners with a one pixel soft edge
            let (px_, py_) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (corner_px - px_).max(px_ - (w as f32 - corner_px)).max(0.0);
            let dy = (corner_px - py_).max(py_ - (h as f32 - corner_px)).max(0.0);
            let a = if dx > 0.0 && dy > 0.0 { (0.5 - ((dx * dx + dy * dy).sqrt() - corner_px)).clamp(0.0, 1.0) } else { 1.0 };
            let o = i * 4;
            let cv = (c * a).round() as u8;
            px[o] = cv;
            px[o + 1] = cv;
            px[o + 2] = cv;
            px[o + 3] = (a * 255.0).round() as u8;
        }
    }
    ColorImage::from_rgba_premultiplied([w, h], &px)
}

impl Brand {
    pub fn new() -> Self {
        Self { logo: None, wordmark: None, grain: None, fade_from: None, done: crate::env::var_os("GRAINEO_NO_SPLASH").is_some(), about: false, licenses: false, hold: None }
    }

    /// The start card with a progress block, shown as long as it is called every frame (no fading, a click does not
    /// close it): the library is converted while the window stays usable.
    pub fn show_hold(&mut self, ctx: &egui::Context, hold: Hold) {
        self.done = false;
        self.hold = Some(hold);
        self.splash(ctx, false, "");
    }

    /// "About graineo": shows the splash window again.
    pub fn show_about(&mut self) {
        self.done = false;
        self.fade_from = None;
        self.about = true;
        // test aid for screenshots: open the licence card right away
        self.licenses = crate::env::var_os("GRAINEO_LICENSES").is_some();
    }

    fn logo(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        if self.logo.is_none() {
            self.logo = decode(LOGO_PNG).map(|i| ctx.load_texture("brand-logo", i, TextureOptions::LINEAR));
        }
        self.logo.clone()
    }

    /// Logo and name at the top left of the bar.
    pub fn header(&mut self, ui: &mut egui::Ui) {
        if let Some(t) = self.logo(ui.ctx()) {
            ui.add(egui::Image::new((t.id(), egui::vec2(18.0, 18.0))));
        }
        ui.label(egui::RichText::new("graineo").font(lr::brand(14.5)).color(Color32::from_rgb(0xf2, 0xef, 0xe6)));
        ui.add_space(6.0);
    }

    /// Splash window: centred, borderless, fades out once the app is ready (`ready`) and
    /// the minimum duration has passed; a click closes it immediately.
    pub fn splash(&mut self, ctx: &egui::Context, ready: bool, status: &str) {
        if self.done {
            return;
        }
        let now = ctx.input(|i| i.time);
        if self.about && ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.fade_from.is_none() {
            if self.licenses {
                self.licenses = false;
            } else {
                self.fade_from = Some(now);
            }
        }
        if !self.about && self.hold.is_none() && self.fade_from.is_none() && ((ready && now >= SPLASH_MIN) || now >= SPLASH_MAX) {
            self.fade_from = Some(now);
        }
        let alpha = match self.fade_from {
            Some(t) => {
                let dur = if self.about { ABOUT_FADE } else { SPLASH_FADE };
                let p = ((now - t) / dur).clamp(0.0, 1.0) as f32;
                (1.0 - p) * (1.0 - p) // ease-out on the way down
            }
            None => 1.0,
        };
        if alpha <= 0.0 {
            self.done = true;
            self.about = false;
            self.wordmark = None;
            self.grain = None;
            return;
        }
        if self.wordmark.is_none() {
            self.wordmark = decode(WORDMARK_PNG).map(|i| ctx.load_texture("brand-wordmark", i, TextureOptions::LINEAR));
        }
        let screen = ctx.content_rect();
        let size = egui::vec2(WIN_W, WIN_H).min(screen.size() - egui::vec2(32.0, 32.0));
        let rect = egui::Rect::from_center_size(screen.center(), size);
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("brand-splash"));
        let p = ctx.layer_painter(layer);
        let a = |c: Color32| c.gamma_multiply(alpha);
        // slightly dim the background, window with shadow
        p.rect_filled(screen, 0.0, a(Color32::from_black_alpha(DIM_ALPHA)));
        // soft shadow (a hard, offset box looked like a duplicate)
        let shadow = egui::epaint::Shadow { offset: [0, 10], blur: 48, spread: 0, color: a(Color32::from_black_alpha(140)) };
        p.add(shadow.as_shape(rect, 14));
        // dark ground of the app symbol (#111214)
        p.rect_filled(rect, 14.0, a(Color32::from_rgb(0x11, 0x12, 0x14)));
        p.rect_stroke(rect, 14.0, egui::Stroke::new(1.0, a(Color32::from_white_alpha(28))), egui::StrokeKind::Inside);
        // black-and-white film grain on the card, below word mark and text (texture once per card size)
        let ppp = ctx.pixels_per_point();
        let px = [(size.x * ppp).round().max(1.0) as usize, (size.y * ppp).round().max(1.0) as usize];
        if self.grain.as_ref().map(|g| g.1) != Some(px) {
            self.grain = Some((ctx.load_texture("brand-grain", grain_image(px[0], px[1], 14.0 * ppp, (ppp * GRAIN_SCALE).max(0.75)), TextureOptions::LINEAR), px));
        }
        if let Some((t, _)) = &self.grain {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            p.image(t.id(), rect, uv, a(Color32::WHITE));
        }
        let k = size.x / WIN_W;
        let cx = rect.center().x;
        // very soft dark veil behind word mark and subtitle (radial, no visible edge): calms the grain there
        {
            let (c, rx, ry) = (egui::pos2(cx, rect.top() + size.y * 0.47 + 6.0 * k), size.x * VEIL_RX, size.y * VEIL_RY);
            let mut mesh = egui::epaint::Mesh::default();
            let rings = 8usize;
            let segs = 64usize;
            mesh.colored_vertex(c, a(Color32::from_black_alpha(VEIL_ALPHA)));
            for r in 1..=rings {
                let f = r as f32 / rings as f32;
                let case = 1.0 - f;
                let al = (VEIL_ALPHA as f32 * case * case * (3.0 - 2.0 * case)).round() as u8; // smoothstep falloff
                for sgm in 0..segs {
                    let ang = sgm as f32 / segs as f32 * std::f32::consts::TAU;
                    mesh.colored_vertex(c + egui::vec2(ang.cos() * rx * f, ang.sin() * ry * f), a(Color32::from_black_alpha(al)));
                }
            }
            let idx = |r: usize, sgm: usize| -> u32 { (1 + (r - 1) * segs + sgm % segs) as u32 };
            for sgm in 0..segs {
                mesh.add_triangle(0, idx(1, sgm), idx(1, sgm + 1));
            }
            for r in 1..rings {
                for sgm in 0..segs {
                    mesh.add_triangle(idx(r, sgm), idx(r + 1, sgm), idx(r + 1, sgm + 1));
                    mesh.add_triangle(idx(r, sgm), idx(r + 1, sgm + 1), idx(r, sgm + 1));
                }
            }
            p.add(egui::Shape::mesh(mesh));
        }
        // word mark centred, subtitle in letter-spaced capitals below (like the section headers)
        let mut baseline = rect.top() + size.y * 0.47;
        if let Some(t) = &self.wordmark {
            let w = 380.0 * k;
            let h = w * t.size()[1] as f32 / t.size()[0] as f32;
            let r = egui::Rect::from_min_size(egui::pos2(cx - w / 2.0, baseline - h * WORDMARK_BASELINE), egui::vec2(w, h));
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            p.image(t.id(), r, uv, a(Color32::WHITE));
            baseline = r.bottom();
        }
        let line1 = lr::caps_spaced(&p, crate::tr("RAW & Film"), egui::FontId::proportional(12.5 * k), a(Color32::from_gray(0xb4)), 2.6 * k);
        let line2 = lr::caps_spaced(&p, "with Foveon support", egui::FontId::proportional(10.5 * k), a(Color32::from_gray(0xc4)), 1.8 * k);
        let y1 = baseline + 14.0 * k;
        let y2 = y1 + line1.size().y + 5.0 * k;
        p.galley(egui::pos2(cx - line1.size().x / 2.0, y1), line1, a(Color32::from_gray(0xb4)));
        p.galley(egui::pos2(cx - line2.size().x / 2.0, y2), line2, a(Color32::from_gray(0xc4)));
        // splash and About window: only the version; About adds a quiet text button to the licence card
        let _ = status;
        let version = crate::trf!("Version {a}", a = env!("CARGO_PKG_VERSION"));
        let mut job = egui::text::LayoutJob::simple(version, egui::FontId::proportional(12.0 * k), a(Color32::from_gray(0x78)), (size.x - 96.0 * k).max(1.0));
        job.halign = egui::Align::Center;
        let body = p.layout_job(job);
        let version_top = rect.bottom() - 30.0 * k - body.size().y - if self.about { 30.0 * k } else { 0.0 };
        p.galley(egui::pos2(cx, version_top), body.clone(), a(Color32::from_gray(0x78)));
        let mut over_button = false;
        if let Some(h) = &self.hold {
            let w = (size.x - 150.0 * k).max(1.0);
            let mut y = rect.top() + size.y * 0.672;
            let line = |text: &str, px: f32, gray: u8, y: &mut f32| {
                let mut job = egui::text::LayoutJob::simple(text.to_string(), egui::FontId::proportional(px * k), a(Color32::from_gray(gray)), w);
                job.halign = egui::Align::Center;
                let g = p.layout_job(job);
                let h = g.size().y;
                p.galley(egui::pos2(cx, *y), g, a(Color32::from_gray(gray)));
                *y += h;
            };
            line(&h.title, 13.0, 0xd0, &mut y);
            y += 5.0 * k;
            line(&h.step, 11.5, 0xa0, &mut y);
            y += 8.0 * k;
            let bar = egui::Rect::from_min_size(egui::pos2(cx - 150.0 * k, y), egui::vec2(300.0 * k, 3.0 * k));
            p.rect_filled(bar, 1.5 * k, a(Color32::from_white_alpha(28)));
            let fill = egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * h.fraction.clamp(0.0, 1.0), bar.height()));
            p.rect_filled(fill, 1.5 * k, a(Color32::from_gray(0xc0)));
            y += 12.0 * k;
            line(&h.note, 10.5, 0x88, &mut y);
        }
        if self.about && self.fade_from.is_none() {
            let label = crate::tr("Lizenzen und Marken …").to_string();
            let top = version_top + body.size().y + 10.0 * k;
            egui::Area::new(egui::Id::new("brand-licenses-button"))
                .order(egui::Order::Tooltip)
                .fixed_pos(egui::pos2(cx, top))
                .pivot(egui::Align2::CENTER_TOP)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let r = lr::link_text(ui, egui::RichText::new(label).size(11.0), Color32::from_gray(0xa0));
                        over_button = r.hovered() || r.contains_pointer();
                        if r.clicked() {
                            self.licenses = true;
                        }
                        // quiet support link next to it, only once an address is set (support.rs); opens the browser
                        if crate::support::enabled() {
                            ui.add_space(14.0);
                            let r = lr::link_text(ui, egui::RichText::new(crate::tr("graineo unterstützen …")).size(11.0), Color32::from_gray(0xa0));
                            over_button |= r.hovered() || r.contains_pointer();
                            if r.clicked() {
                                crate::support::open();
                            }
                        }
                    });
                });
        }
        if self.licenses {
            self.licenses_card(ctx, rect);
            over_button = true;
        }
        if !over_button && self.hold.is_none() && ctx.input(|i| i.pointer.any_click()) && self.fade_from.is_none() {
            self.fade_from = Some(now);
        }
        // repaint only as long as something moves: fade, or the start screen waiting for the app
        if self.fade_from.is_some() {
            ctx.request_repaint();
        } else if !self.about {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    /// Quiet card "Licences and trademarks" over the About window: everything the licences require, grouped.
    fn licenses_card(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let head = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(text.to_uppercase()).size(10.0).color(Color32::from_gray(0x90)));
            ui.add_space(2.0);
        };
        let line = |ui: &mut egui::Ui, text: &str| {
            ui.label(egui::RichText::new(text).size(10.5).color(Color32::from_gray(0xb0)));
        };
        egui::Area::new(egui::Id::new("brand-licenses-card"))
            .order(egui::Order::Tooltip)
            .fixed_pos(rect.center())
            .pivot(egui::Align2::CENTER_CENTER)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x15, 0x16, 0x19))
                    .stroke(egui::Stroke::new(1.0, Color32::from_white_alpha(28)))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_width((rect.width() - 120.0).clamp(260.0, 620.0));
                        ui.label(egui::RichText::new(crate::tr("Lizenzen und Marken")).size(13.0).color(Color32::from_rgb(0xf2, 0xef, 0xe6)));
                        egui::ScrollArea::vertical().max_height((rect.height() - 96.0).max(120.0)).show(ui, |ui| {
                            head(ui, &crate::tr("Lizenz"));
                            line(ui, &crate::tr("graineo steht unter der GPL-3.0."));
                            head(ui, &crate::tr("Bausteine"));
                            line(ui, "x3fuse-core (Apache 2.0)");
                            line(ui, &crate::tr("Film: spektrafilm, A. Volpato (GPL-3.0)"));
                            line(ui, &crate::tr("Filmdaten: CC BY-SA 4.0"));
                            line(ui, &crate::tr("RAW-Dekodierung: rawler (LGPL-2.1)"));
                            line(ui, &crate::tr("Objektivdaten: lensfun (CC BY-SA 3.0)"));
                            line(ui, &crate::tr("Ortsnamen: GeoNames (geonames.org), CC BY 4.0 (creativecommons.org/licenses/by/4.0)"));
                            line(ui, &crate::tr("Gesichter finden: YuNet, Shiqi Yu (MIT)"));
                            line(ui, &crate::tr("Tiefe (Modul): Depth Anything V2 Small, L. Yang et al. (Apache-2.0)"));
                            // optional non-commercial modules: never shipped, attribution as their licence asks
                            for d in library::module::ALL.iter().filter(|d| library::module::non_commercial(d).is_some()) {
                                if let Some(t) = library::module::non_commercial(d) {
                                    line(ui, &crate::trf!("Optional, nicht mitgeliefert, nur nicht-kommerziell: {a}", a = t.attribution));
                                }
                            }
                            line(ui, &crate::tr("Werkzeugideen: Körnungs-Lupe, Vorbelichtung, Weichzeichner und Lichtpunkte nach spektrafilm"));
                            head(ui, &crate::tr("Schriften und Symbole"));
                            line(ui, "IBM Plex Mono, Syne (OFL)");
                            line(ui, "Phosphor (MIT)");
                            head(ui, &crate::tr("Marken"));
                            line(ui, &crate::tr("Foveon und Sigma sind Marken der Sigma Corporation; graineo ist nicht mit Sigma verbunden."));
                            ui.add_space(4.0);
                            line(ui, &crate::tr("Filmnamen wie Portra, Ektar, Tri-X, Velvia, Provia oder Acros sind Marken ihrer jeweiligen Inhaber. graineo bildet diese Filme anhand veröffentlichter Daten nach und steht in keiner Verbindung zu Kodak, Fujifilm, Ilford, Agfa oder anderen Herstellern."));
                        });
                        ui.add_space(10.0);
                        if lr::link_text(ui, egui::RichText::new(crate::tr("Schließen")).size(11.0), Color32::from_gray(0xa0)).clicked() {
                            self.licenses = false;
                        }
                    });
            });
    }
}
