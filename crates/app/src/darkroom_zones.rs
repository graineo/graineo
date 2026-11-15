//! Darkroom, phase 2: zone system display and metering point. At the bottom edge of the image a bar with
//! zones 0 to X (equal brightness steps, zone V = middle grey) and above it how many pixels of the
//! finished image lie in each zone. The pointer in the image is the metering point: zone and brightness of the
//! pixel below appear as a label and in the bar.
//!
//! The values come from the rendered preview image (after film, paper and all sliders).

use develop::geometry::Frame;
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::lr;

/// Roman numerals of zones 0 to X.
const ROMAN: [&str; 11] = ["0", "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X"];

#[derive(Default)]
pub struct Zones {
    /// Display on?
    pub an: bool,
    /// last rendered image (width, height, RGB) and zone distribution
    image: Option<(usize, usize, Vec<u8>)>,
    histo: [u32; 11],
}

/// sRGB-encoded (0 … 1) → linear.
fn linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Lightness L* (0 … 100) of an sRGB pixel.
pub fn lstar(p: [u8; 3]) -> f32 {
    let y = 0.2126 * linear(p[0] as f32 / 255.0) + 0.7152 * linear(p[1] as f32 / 255.0) + 0.0722 * linear(p[2] as f32 / 255.0);
    if y > 0.008856 { 116.0 * y.cbrt() - 16.0 } else { 903.3 * y }
}

/// Zone (0 … 10, fractional) for a lightness L*: zone V corresponds to middle grey (18 %, L* ≈ 50).
pub fn zone_of_lstar(l: f32) -> f32 {
    (l / 10.0).clamp(0.0, 10.0)
}

/// Grey value (sRGB, 0 … 255) of the centre of a zone.
pub fn zones_gray(z: usize) -> u8 {
    let l = z as f32 * 10.0;
    let y = if l > 8.0 { ((l + 16.0) / 116.0).powi(3) } else { l / 903.3 };
    let s = if y <= 0.0031308 { y * 12.92 } else { 1.055 * y.powf(1.0 / 2.4) - 0.055 };
    (s.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl Zones {
    /// Remember the rendered image and compute the zone distribution (every third pixel suffices).
    pub fn remember(&mut self, w: usize, h: usize, rgb: &[u8]) {
        let mut histo = [0u32; 11];
        for p in rgb.chunks_exact(3).step_by(3) {
            let z = zone_of_lstar(lstar([p[0], p[1], p[2]])).round() as usize;
            histo[z.min(10)] += 1;
        }
        self.histo = histo;
        self.image = Some((w, h, rgb.to_vec()));
    }

    /// Distribution over the zones in percent (sum 100), empty without image.
    pub fn distribution(&self) -> [f32; 11] {
        let n: u32 = self.histo.iter().sum();
        let mut v = [0.0; 11];
        if n > 0 {
            for (a, b) in v.iter_mut().zip(&self.histo) {
                *a = *b as f32 * 100.0 / n as f32;
            }
        }
        v
    }

    /// Pixel under a screen point: (zone, brightness in percent of the sRGB encoding).
    pub fn measure(&self, p: Pos2, rect: Rect, frame: &Frame, full: [usize; 2]) -> Option<(f32, f32)> {
        let (w, h, rgb) = self.image.as_ref()?;
        if !rect.contains(p) || rect.width() <= 0.0 {
            return None;
        }
        let u = [(p.x - rect.left()) / rect.width(), (p.y - rect.top()) / rect.height()];
        let s = frame.to_src(u);
        let x = ((s[0] / full[0].max(1) as f32) * *w as f32).clamp(0.0, *w as f32 - 1.0) as usize;
        let y = ((s[1] / full[1].max(1) as f32) * *h as f32).clamp(0.0, *h as f32 - 1.0) as usize;
        let i = (y * w + x) * 3;
        let px = [*rgb.get(i)?, *rgb.get(i + 1)?, *rgb.get(i + 2)?];
        let gray = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 2.55;
        Some((zone_of_lstar(lstar(px)), gray))
    }

    /// Draw the bar and metering point over the image. `area`: visible image area, `image`: rectangle
    /// of the whole image, `pointer`: mouse position.
    pub fn draw(&self, painter: &egui::Painter, area: Rect, image: Rect, frame: &Frame, full: [usize; 2], pointer: Option<Pos2>) {
        if !self.an {
            return;
        }
        let reading = pointer.filter(|p| area.contains(*p)).and_then(|p| self.measure(p, image.intersect(area), frame, full).map(|m| (p, m)));
        // Bar at the bottom edge
        let height = 44.0;
        let width = 11.0 * 44.0;
        let strip = Rect::from_min_size(Pos2::new(area.center().x - width / 2.0, area.bottom() - height - 10.0), Vec2::new(width, height));
        painter.rect_filled(strip.expand(6.0), 0.0, Color32::from_black_alpha(190));
        let v = self.distribution();
        let max = v.iter().cloned().fold(0.0f32, f32::max).max(1.0);
        for z in 0..11 {
            let cell = Rect::from_min_size(Pos2::new(strip.left() + z as f32 * 44.0, strip.top()), Vec2::new(44.0, height));
            let bar = (v[z] / max * 22.0).max(if v[z] > 0.0 { 1.0 } else { 0.0 });
            let g = zones_gray(z);
            let patch = Rect::from_min_max(Pos2::new(cell.left() + 1.0, cell.top() + 24.0), Pos2::new(cell.right() - 1.0, cell.bottom()));
            painter.rect_filled(patch, 0.0, Color32::from_gray(g));
            painter.rect_filled(Rect::from_min_max(Pos2::new(cell.left() + 6.0, cell.top() + 22.0 - bar), Pos2::new(cell.right() - 6.0, cell.top() + 22.0)), 0.0, Color32::from_gray(0xaa));
            let text_color = if g > 120 { Color32::BLACK } else { Color32::WHITE };
            painter.text(patch.center(), egui::Align2::CENTER_CENTER, ROMAN[z], egui::FontId::monospace(10.5), text_color);
        }
        if let Some((p, (zone, gray))) = reading {
            // Mark in the bar and label at the pointer
            let x = strip.left() + (zone + 0.5) * 44.0;
            let x = x.clamp(strip.left(), strip.right());
            painter.line_segment([Pos2::new(x, strip.top() + 22.0), Pos2::new(x, strip.bottom())], Stroke::new(2.0, lr::ACCENT));
            painter.circle_stroke(p, 6.0, Stroke::new(1.5, lr::ACCENT));
            let text = crate::trf!("ZONE {a} · {b} %", a = ROMAN[zone.round().clamp(0.0, 10.0) as usize], b = format!("{:.0}", gray));
            let g = painter.layout_no_wrap(text, egui::FontId::monospace(11.0), Color32::WHITE);
            let o = p + Vec2::new(12.0, -22.0);
            let o = Pos2::new(o.x.min(area.right() - g.size().x - 14.0).max(area.left() + 4.0), o.y.max(area.top() + 4.0));
            painter.rect_filled(Rect::from_min_size(o - Vec2::new(5.0, 3.0), g.size() + Vec2::new(10.0, 6.0)), 0.0, Color32::from_black_alpha(190));
            painter.galley(o, g, Color32::WHITE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_gray_is_zone_five() {
        // 18 % linear corresponds to sRGB ≈ 118
        let z = zone_of_lstar(lstar([118, 118, 118]));
        assert!((z - 5.0).abs() < 0.15, "{z}");
        assert!(zone_of_lstar(lstar([0, 0, 0])) < 0.05);
        assert!((zone_of_lstar(lstar([255, 255, 255])) - 10.0).abs() < 0.05);
    }

    #[test]
    fn zone_gray_is_monotonic() {
        let g: Vec<u8> = (0..11).map(zones_gray).collect();
        assert!(g.windows(2).all(|w| w[0] < w[1]), "{g:?}");
        assert_eq!(g[0], 0);
        assert_eq!(g[10], 255);
    }

    #[test]
    fn distribution_sums_to_100() {
        let mut z = Zones::default();
        let mut rgb = Vec::new();
        for i in 0..3000 {
            let v = (i % 256) as u8;
            rgb.extend_from_slice(&[v, v, v]);
        }
        z.remember(100, 30, &rgb);
        let s: f32 = z.distribution().iter().sum();
        assert!((s - 100.0).abs() < 0.01, "{s}");
    }

    #[test]
    fn measure_reads_the_pixel_under_the_pointer() {
        let mut z = Zones::default();
        // left half black, right half white
        let mut rgb = vec![0u8; 20 * 10 * 3];
        for y in 0..10 {
            for x in 10..20 {
                let i = (y * 20 + x) * 3;
                rgb[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        z.remember(20, 10, &rgb);
        let frame = Frame::full(20, 10, 0);
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(200.0, 100.0));
        let left = z.measure(Pos2::new(150.0, 100.0), rect, &frame, [20, 10]).unwrap();
        let right = z.measure(Pos2::new(250.0, 100.0), rect, &frame, [20, 10]).unwrap();
        assert!(left.0 < 0.1 && right.0 > 9.9, "{left:?} {right:?}");
        assert!(z.measure(Pos2::new(10.0, 10.0), rect, &frame, [20, 10]).is_none());
    }
}
