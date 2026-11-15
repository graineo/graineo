//! RGBA image for the analyses, drawing helpers (lines, circles, text), color scales and PNG output.
//!
//! Deliberately kept small: the UI receives finished RGBA buffers (not premultiplied,
//! straight alpha) and lays them over the image as a layer.

use std::path::Path;
use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

/// RGBA8 image (rows top to bottom, alpha not premultiplied).
#[derive(Clone, PartialEq)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Canvas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bild({}×{})", self.width, self.height)
    }
}

pub type Color = [u8; 4];

pub const TRANSPARENT: Color = [0, 0, 0, 0];
pub const BLACK: Color = [0, 0, 0, 255];
pub const WHITE: Color = [255, 255, 255, 255];
pub const BACKGROUND: Color = [24, 24, 26, 255];
pub const RASTER: Color = [70, 70, 76, 255];
pub const TEXT: Color = [220, 220, 224, 255];

/// Display colors of the filter classes (R, G1, G2, B or R, G, B).
pub const RED: Color = [235, 70, 60, 255];
pub const GREEN: Color = [70, 205, 80, 255];
pub const GREEN2: Color = [150, 230, 90, 255];
pub const BLUE: Color = [80, 130, 255, 255];

impl Canvas {
    pub fn new_at(width: usize, height: usize, f: Color) -> Self {
        let mut rgba = Vec::with_capacity(width * height * 4);
        for _ in 0..width * height {
            rgba.extend_from_slice(&f);
        }
        Self { width, height, rgba }
    }

    pub fn from_rgb(width: usize, height: usize, rgb: &[u8]) -> Self {
        let mut rgba = Vec::with_capacity(width * height * 4);
        for p in rgb.chunks_exact(3) {
            rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        Self { width, height, rgba }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> Color {
        let i = (y * self.width + x) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, f: Color) {
        if x < self.width && y < self.height {
            let i = (y * self.width + x) * 4;
            self.rgba[i..i + 4].copy_from_slice(&f);
        }
    }

    /// Blend a color with its alpha over the pixel (resulting alpha = max).
    #[inline]
    pub fn blend(&mut self, x: isize, y: isize, f: Color) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let i = (y as usize * self.width + x as usize) * 4;
        let a = f[3] as u32;
        for c in 0..3 {
            self.rgba[i + c] = ((f[c] as u32 * a + self.rgba[i + c] as u32 * (255 - a) + 127) / 255) as u8;
        }
        self.rgba[i + 3] = self.rgba[i + 3].max(f[3]);
    }

    pub fn rect(&mut self, x: isize, y: isize, w: usize, h: usize, f: Color) {
        for yy in y..y + h as isize {
            for xx in x..x + w as isize {
                self.blend(xx, yy, f);
            }
        }
    }

    pub fn frame(&mut self, x: isize, y: isize, w: usize, h: usize, f: Color) {
        let (w, h) = (w as isize, h as isize);
        self.line(x as f32, y as f32, (x + w - 1) as f32, y as f32, f);
        self.line(x as f32, (y + h - 1) as f32, (x + w - 1) as f32, (y + h - 1) as f32, f);
        self.line(x as f32, y as f32, x as f32, (y + h - 1) as f32, f);
        self.line((x + w - 1) as f32, y as f32, (x + w - 1) as f32, (y + h - 1) as f32, f);
    }

    /// Line (simple, no anti-aliasing).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, f: Color) {
        let n = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
        for k in 0..=n {
            let t = k as f32 / n as f32;
            self.blend((x0 + t * (x1 - x0)).round() as isize, (y0 + t * (y1 - y0)).round() as isize, f);
        }
    }

    pub fn polyline(&mut self, p: &[[f32; 2]], f: Color) {
        for s in p.windows(2) {
            self.line(s[0][0], s[0][1], s[1][0], s[1][1], f);
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, f: Color) {
        let n = ((r * 7.0) as usize).max(12);
        let mut old = None;
        for k in 0..=n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let p = [cx + r * a.cos(), cy + r * a.sin()];
            if let Some(q) = old {
                let q: [f32; 2] = q;
                self.line(q[0], q[1], p[0], p[1], f);
            }
            old = Some(p);
        }
    }

    /// Insert `source` at (x, y) (alpha blended).
    pub fn insert_at(&mut self, source: &Canvas, x: isize, y: isize) {
        for yy in 0..source.height {
            for xx in 0..source.width {
                self.blend(x + xx as isize, y + yy as isize, source.get(xx, yy));
            }
        }
    }

    /// Blend a layer of the same size on top with opacity `opacity` (0…1).
    pub fn overlay(&mut self, layer: &Canvas, opacity: f32) {
        let k = layer.width.min(self.width);
        for y in 0..layer.height.min(self.height) {
            for x in 0..k {
                let mut f = layer.get(x, y);
                f[3] = (f[3] as f32 * opacity).round() as u8;
                self.blend(x as isize, y as isize, f);
            }
        }
    }

    /// Nearest-neighbor scaling to `w`×`h`.
    pub fn scaled(&self, w: usize, h: usize) -> Canvas {
        let mut o = Canvas::new_at(w, h, TRANSPARENT);
        for y in 0..h {
            let sy = (y * self.height / h.max(1)).min(self.height - 1);
            for x in 0..w {
                let sx = (x * self.width / w.max(1)).min(self.width - 1);
                o.set(x, y, self.get(sx, sy));
            }
        }
        o
    }

    /// Area-average downscaling (for images, not for masks).
    pub fn downscaled(&self, w: usize, h: usize) -> Canvas {
        if w >= self.width && h >= self.height {
            return self.scaled(w, h);
        }
        let mut o = Canvas::new_at(w, h, TRANSPARENT);
        for y in 0..h {
            let (y0, y1) = (y * self.height / h, ((y + 1) * self.height / h).max(y * self.height / h + 1));
            for x in 0..w {
                let (x0, x1) = (x * self.width / w, ((x + 1) * self.width / w).max(x * self.width / w + 1));
                let mut s = [0u32; 4];
                for yy in y0..y1.min(self.height) {
                    for xx in x0..x1.min(self.width) {
                        let p = self.get(xx, yy);
                        for c in 0..4 {
                            s[c] += p[c] as u32;
                        }
                    }
                }
                let n = ((y1.min(self.height) - y0) * (x1.min(self.width) - x0)).max(1) as u32;
                o.set(x, y, [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, (s[3] / n) as u8]);
            }
        }
        o
    }

    /// Draw text (IBM Plex Mono), `(x, y)` = top-left corner of the line. Returns the width.
    pub fn text(&mut self, x: f32, y: f32, size: f32, f: Color, s: &str) -> f32 {
        let font = plex_font();
        let sk = font.as_scaled(PxScale::from(size));
        let mut cx = x;
        let basis = y + sk.ascent();
        for ch in s.chars() {
            let id = font.glyph_id(ch);
            let g = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(cx, basis));
            cx += sk.h_advance(id);
            if let Some(o) = font.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    let a = (cov.clamp(0.0, 1.0) * f[3] as f32) as u8;
                    if a > 0 {
                        self.blend(b.min.x as isize + gx as isize, b.min.y as isize + gy as isize, [f[0], f[1], f[2], a]);
                    }
                });
            }
        }
        cx - x
    }

    /// Width of a text in pixels.
    pub fn text_width(size: f32, s: &str) -> f32 {
        let font = plex_font();
        let sk = font.as_scaled(PxScale::from(size));
        s.chars().map(|c| sk.h_advance(font.glyph_id(c))).sum()
    }

    /// Write as PNG (RGBA).
    pub fn write_png(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        image::save_buffer(path.as_ref(), &self.rgba, self.width as u32, self.height as u32, image::ExtendedColorType::Rgba8).map_err(std::io::Error::other)
    }
}

fn plex_font() -> &'static FontRef<'static> {
    static F: OnceLock<FontRef<'static>> = OnceLock::new();
    F.get_or_init(|| FontRef::try_from_slice(include_bytes!("../assets/IBMPlexMono-Regular.ttf")).expect("font"))
}

/// Heat scale (modeled on "inferno", own control points), `t` in 0…1.
pub fn heat(t: f32) -> [u8; 3] {
    const S: [[f32; 3]; 9] = [
        [0.0, 0.0, 4.0],
        [31.0, 12.0, 72.0],
        [85.0, 15.0, 109.0],
        [136.0, 34.0, 106.0],
        [186.0, 54.0, 85.0],
        [227.0, 89.0, 51.0],
        [249.0, 140.0, 10.0],
        [249.0, 201.0, 50.0],
        [252.0, 255.0, 164.0],
    ];
    scale(&S, t)
}

/// Cool-warm scale (blue → gray → red), `t` in 0…1, middle 0.5 neutral.
pub fn two_sided(t: f32) -> [u8; 3] {
    const S: [[f32; 3]; 5] = [[40.0, 80.0, 200.0], [110.0, 160.0, 230.0], [225.0, 225.0, 225.0], [240.0, 150.0, 90.0], [200.0, 40.0, 40.0]];
    scale(&S, t)
}

fn scale(s: &[[f32; 3]], t: f32) -> [u8; 3] {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let p = t * (s.len() - 1) as f32;
    let i = (p as usize).min(s.len() - 2);
    let f = p - i as f32;
    std::array::from_fn(|c| (s[i][c] + f * (s[i + 1][c] - s[i][c])).round() as u8)
}

/// Heat map from values (0…1, NaN = transparent) with fixed opacity.
pub fn heat_map(w: usize, h: usize, values: &[f32], alpha: u8) -> Canvas {
    let mut b = Canvas::new_at(w, h, TRANSPARENT);
    for (i, &v) in values.iter().enumerate().take(w * h) {
        if v.is_finite() {
            let c = heat(v);
            b.rgba[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], alpha]);
        }
    }
    b
}

/// Small color bar with label (for charts and test sheets).
pub fn legend(width: usize, title: &str, left: &str, right: &str, f: fn(f32) -> [u8; 3]) -> Canvas {
    let mut b = Canvas::new_at(width, 40, BACKGROUND);
    b.text(4.0, 1.0, 12.0, TEXT, title);
    for x in 0..width.saturating_sub(8) {
        let c = f(x as f32 / (width - 9).max(1) as f32);
        for y in 18..28 {
            b.set(x + 4, y, [c[0], c[1], c[2], 255]);
        }
    }
    b.text(4.0, 27.0, 11.0, TEXT, left);
    let rb = Canvas::text_width(11.0, right);
    b.text(width as f32 - 4.0 - rb, 27.0, 11.0, TEXT, right);
    b
}
