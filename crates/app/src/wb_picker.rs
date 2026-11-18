//! White balance picker (W): a click on a neutral spot sets the white balance.
//!
//! - **Sample**: a square of the sensor preview (linear, before the colour matrix: camera RGB for RAWs, linear sRGB for
//!   finished images, linear Rec. 2020 on the Apple path) around the pointer, [`SIDES`] preview pixels wide (− / +).
//!   Pixels with a blown channel and very dark pixels are left out; of the rest, outliers in colour (hot pixels, edges,
//!   noise) are trimmed around the median before the linear mean is taken ([`sample`]).
//! - **Loupe** while hovering ([`paint_loupe`]): the preview pixels around the pointer as sharp blocks with the sampled square as a frame, and
//!   what a click would set (Kelvin/tint, relative values for finished images, channel gains for Sigma), or why the
//!   spot is not usable (blown out, too dark) or doubtful (an unusual light: probably not neutral; outside the Kelvin
//!   scale).
//! - **Click** sets the white balance and ends the picker; **⇧-click** adds the spot to the samples taken so far
//!   (mean of all) and stays on. Esc or W ends it. Every click is one step of the develop history (⌘Z).
//!
//! The rendering itself is never recomputed while hovering: the loupe shows the current image, the numbers say what
//! the click will set.

use eframe::egui::{self, Color32};

use crate::jobs::Loaded;
use crate::lr;
use crate::menu::i18n::DeComma as _;
use develop::Settings;

/// Side lengths of the sampled square in preview pixels (the preview has ≈ 2048 px on the long side; one preview pixel
/// is ≈ 2–4 pixels of the photo).
pub const SIDES: [usize; 5] = [3, 5, 9, 17, 33];
/// Index of the standard size in [`SIDES`] (9 × 9 ≈ 25 × 25 pixels of a 24 MP photo): measured on ISO 100…6400
/// photos, the result scatters about half as much as with 5 × 5 when the click lands a few pixels off, and the square
/// is still small enough for a grey card at "fit".
pub const STANDARD: usize = 2;
/// Settings key of the chosen size (index into [`SIDES`]).
const SIZE_SETTING: &str = "wbPickerSize";

/// Share of the white level at and above which a channel counts as blown.
pub const CLIP: f32 = 0.95;
/// Share of the white level below which a pixel counts as too dark (≈ −8 EV: mostly noise).
pub const DARK: f32 = 0.004;
/// A light further than this from the daylight/tungsten curve (tint units, daylight ≈ +10) or bluer than
/// [`DOUBT_KELVIN`] is unusual: the spot is probably not neutral (foliage, sky, coloured paint). A warm or cool surface
/// (brick, wood) cannot be told from warm or cool light by one sample; no warning there.
pub const DOUBT_TINT: f32 = 30.0;
pub const DOUBT_KELVIN: f32 = 20000.0;

/// How usable a sample is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Good,
    /// more than half of the square has a blown channel: rejected
    Clipped,
    /// more than half of the square is too dark: rejected
    Dark,
}

impl Verdict {
    pub fn usable(self) -> bool {
        self == Verdict::Good
    }
}

/// Result of [`sample`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// linear mean of the pixels kept
    pub mean: [f64; 3],
    pub kept: usize,
    pub total: usize,
    pub verdict: Verdict,
}

/// Levels of the sample image: `white` per channel (a blown channel reaches [`CLIP`] of it).
#[derive(Clone, Copy, Debug)]
pub struct Levels {
    pub white: [f32; 3],
}

/// Robust mean of the `side` × `side` square around (`cx`, `cy`) of an image `w` × `h` (`px` reads a pixel).
pub fn sample(px: &dyn Fn(usize, usize) -> [f32; 3], w: usize, h: usize, cx: usize, cy: usize, side: usize, lv: &Levels) -> Sample {
    let r = side / 2;
    let (x0, x1) = (cx.saturating_sub(r), (cx + r + 1).min(w));
    let (y0, y1) = (cy.saturating_sub(r), (cy + r + 1).min(h));
    let (mut total, mut clipped, mut dark) = (0usize, 0usize, 0usize);
    let mut good: Vec<([f32; 3], f32, f32)> = Vec::with_capacity(side * side);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = px(x, y);
            total += 1;
            let rel: [f32; 3] = std::array::from_fn(|c| p[c] / lv.white[c].max(1e-9));
            if rel.iter().any(|v| !v.is_finite() || *v >= CLIP) {
                clipped += 1;
            } else if rel.iter().copied().fold(0.0, f32::max) < DARK || p.iter().any(|v| *v <= 0.0) {
                dark += 1;
            } else {
                good.push((p, (p[0] / p[1]).ln(), (p[2] / p[1]).ln()));
            }
        }
    }
    let reject = good.len() * 2 < total || good.is_empty();
    let (mean, trimmed) = trimmed_mean(&good);
    let verdict = match (reject, clipped >= dark) {
        (false, _) => Verdict::Good,
        (true, true) => Verdict::Clipped,
        (true, false) => Verdict::Dark,
    };
    let kept = if reject { 0 } else { trimmed };
    Sample { mean, kept, total, verdict }
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Linear mean of the pixels whose colour (log red/green, log blue/green) lies near the median colour: within three
/// robust standard deviations (1.4826 · MAD), at least ±0.03 (3 %).
fn trimmed_mean(good: &[([f32; 3], f32, f32)]) -> ([f64; 3], usize) {
    if good.is_empty() {
        return ([0.0; 3], 0);
    }
    let ma = median(&mut good.iter().map(|g| g.1).collect::<Vec<_>>());
    let mb = median(&mut good.iter().map(|g| g.2).collect::<Vec<_>>());
    let dev = |g: &([f32; 3], f32, f32)| (g.1 - ma).abs().max((g.2 - mb).abs());
    let mad = median(&mut good.iter().map(dev).collect::<Vec<_>>());
    let limit = (3.0 * 1.4826 * mad).max(0.03);
    let mut s = [0.0f64; 3];
    let mut n = 0.0f64;
    for g in good.iter().filter(|g| dev(g) <= limit) {
        for c in 0..3 {
            s[c] += g.0[c] as f64;
        }
        n += 1.0;
    }
    (s.map(|v| v / n.max(1.0)), n as usize)
}

/// Mean colour of several samples (⇧-click): geometric mean of red/green and blue/green, at the green level of the last.
pub fn combine(samples: &[[f64; 3]]) -> Option<[f64; 3]> {
    let last = samples.last()?;
    let n = samples.len() as f64;
    let (mut a, mut b) = (0.0, 0.0);
    for s in samples {
        a += (s[0] / s[1]).ln();
        b += (s[2] / s[1]).ln();
    }
    let g = last[1];
    Some([g * (a / n).exp(), g, g * (b / n).exp()])
}

/// What a click would set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// RAW: absolute Kelvin and tint; `exact` false: outside the scale (the spot keeps a cast)
    Kelvin { kelvin: f32, tint: f32, exact: bool },
    /// finished image: the relative sliders (−100 … +100) and the absolute values behind them
    Relative { temp: f32, tint: f32, kelvin: f32, abs_tint: f32, exact: bool },
    /// Sigma: channel gains (green = 1), relative to the chosen preset
    Gains([f32; 3]),
    /// no white balance makes this spot neutral
    None,
}

impl Outcome {
    /// Outside the scale: usable, but the spot will not come out fully neutral.
    pub fn out_of_range(&self) -> bool {
        matches!(self, Outcome::Kelvin { exact: false, .. } | Outcome::Relative { exact: false, .. })
    }

    /// The light it implies is unusual ([`DOUBT_TINT`], [`DOUBT_KELVIN`]): the spot is probably not neutral.
    pub fn doubtful(&self) -> bool {
        match *self {
            Outcome::Kelvin { kelvin, tint, .. } | Outcome::Relative { kelvin, abs_tint: tint, .. } => (tint - 10.0).abs() > DOUBT_TINT || kelvin > DOUBT_KELVIN,
            _ => false,
        }
    }
}

/// Levels of the sample image of `loaded` (see [`Levels`]).
pub fn levels(loaded: &Loaded) -> Levels {
    match &loaded.previews.foreign {
        // graineo RAW: camera RGB times the as-shot gains (a blown channel reaches its gain)
        Some(doc) if !doc.finished && doc.apple.is_none() => Levels { white: doc.info.wb.unwrap_or([1.0; 3]) },
        // finished image, Apple path, Sigma: 1 = white
        _ => Levels { white: [1.0; 3] },
    }
}

/// What a click with the (combined) sample `mean` would set.
pub fn outcome(loaded: &Loaded, s: &Settings, mean: [f64; 3]) -> Outcome {
    match &loaded.previews.foreign {
        Some(doc) => match rawdev::wb_pick::light_of(doc, mean) {
            Some(l) if doc.finished => {
                let (temp, tint) = rawdev::wb::relative_from(l.kelvin, l.tint);
                Outcome::Relative { temp, tint, kelvin: l.kelvin, abs_tint: l.tint, exact: l.exact }
            }
            Some(l) => Outcome::Kelvin { kelvin: l.kelvin, tint: l.tint, exact: l.exact },
            None => Outcome::None,
        },
        None => develop::pick_white_balance(&loaded.cal, s, mean).map_or(Outcome::None, Outcome::Gains),
    }
}

/// Apply an outcome to the settings. Returns whether something was set.
pub fn apply(o: &Outcome, s: &mut Settings) -> bool {
    match *o {
        Outcome::Kelvin { kelvin, tint, .. } | Outcome::Relative { kelvin, abs_tint: tint, .. } => {
            s.raw.wb_mode = "custom".into();
            // the sliders' resolution: 1 K, 0.1 tint (so a later slider move does not jump)
            s.raw.temperature = kelvin.round();
            s.raw.tint = (tint * 10.0).round() / 10.0;
            true
        }
        Outcome::Gains(m) => {
            s.wb_mult = m;
            s.wb_temp = 0.0;
            s.wb_tint = 0.0;
            true
        }
        Outcome::None => false,
    }
}

/// State of the picker besides on/off (`App::wb_picker`): chosen size and the samples of ⇧-clicks.
#[derive(Debug)]
pub struct State {
    /// index into [`SIDES`]
    pub size: usize,
    /// samples taken with ⇧-click since the picker was switched on (colour only, see [`combine`])
    pub taken: Vec<[f64; 3]>,
    /// the photo the samples belong to
    pub image: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        let size = library::settings::value(SIZE_SETTING).as_u64().map_or(STANDARD, |v| (v as usize).min(SIDES.len() - 1));
        State { size, taken: Vec::new(), image: None }
    }
}

impl State {
    /// The picker works on photo `index`: samples taken on another photo are forgotten (other colour space, other light).
    pub fn follow(&mut self, index: usize) {
        if self.image != Some(index) {
            self.taken.clear();
            self.image = Some(index);
        }
    }

    pub fn side(&self) -> usize {
        SIDES[self.size.min(SIDES.len() - 1)]
    }

    /// − / + : a smaller or larger square (remembered in the settings).
    pub fn resize(&mut self, larger: bool) {
        let new = if larger { (self.size + 1).min(SIDES.len() - 1) } else { self.size.saturating_sub(1) };
        if new != self.size {
            self.size = new;
            std::thread::spawn(move || library::settings::set(SIZE_SETTING, serde_json::json!(new)));
        }
    }
}

/// What the loupe shows for the spot under the pointer.
pub struct Probe {
    pub sample: Sample,
    pub outcome: Outcome,
    /// number of samples the outcome combines (1 = only this spot)
    pub samples: usize,
}

const WARN: Color32 = Color32::from_rgb(0xe0, 0xa4, 0x58);
const BAD: Color32 = Color32::from_rgb(0xe0, 0x6a, 0x5c);
/// Size of the loupe (points): magnified image, then three lines of text.
const LENS: egui::Vec2 = egui::vec2(150.0, 96.0);
const LINE: f32 = 15.0;
const PAD: f32 = 6.0;

/// Text lines of the loupe: the two values a click sets and the state of the sample (with its colour).
pub fn lines(p: &Probe, side: usize) -> ([String; 2], (String, Color32)) {
    let usable = p.sample.verdict.usable();
    let values = match p.outcome {
        _ if !usable => ["–".to_string(), String::new()],
        Outcome::Kelvin { kelvin, tint, .. } => [format!("{kelvin:.0} K"), crate::trf!("Tönung {t}", t = format!("{tint:+.0}"))],
        Outcome::Relative { temp, tint, .. } => [crate::trf!("Temperatur {t}", t = format!("{temp:+.0}")), crate::trf!("Tönung {t}", t = format!("{tint:+.0}"))],
        Outcome::Gains(m) => [crate::trf!("Rot ×{v}", v = format!("{:.2}", m[0]).de_comma()), crate::trf!("Blau ×{v}", v = format!("{:.2}", m[2]).de_comma())],
        Outcome::None => ["–".to_string(), String::new()],
    };
    let state = match p.sample.verdict {
        Verdict::Clipped => (crate::tr("Ausgefressen").to_string(), BAD),
        Verdict::Dark => (crate::tr("Zu dunkel").to_string(), BAD),
        _ if p.outcome == Outcome::None => (crate::tr("Nicht neutral messbar").to_string(), BAD),
        _ if p.outcome.out_of_range() => (crate::tr("Außerhalb der Skala").to_string(), WARN),
        _ if p.outcome.doubtful() => (crate::tr("Wohl nicht neutral").to_string(), WARN),
        _ if p.samples > 1 => (crate::trf!("{s}×{s} · {n} Proben", s = side, n = p.samples), lr::TEXT_DIM),
        _ => (format!("{side}×{side}"), lr::TEXT_DIM),
    };
    (values, state)
}

/// Preview pixels of margin around the measured square that stay visible in the loupe (vertically).
const LENS_MARGIN: usize = 2;

/// Layout of the magnified view: whole device pixels per preview pixel, so that every preview pixel is a sharp block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoupeGrid {
    /// size of one preview pixel in points (a whole number of device pixels)
    pub cell: f32,
    /// measured preview pixels `x0..x1` × `y0..y1` (the square, clipped at the image border)
    pub measured: [usize; 4],
}

impl LoupeGrid {
    /// The largest whole-device-pixel magnification that shows the `side` × `side` square plus `LENS_MARGIN` pixels all
    /// around inside the lens (height decides for the big squares; the width then shows more context).
    pub fn new(lens: egui::Vec2, ppp: f32, side: usize, cx: usize, cy: usize, w: usize, h: usize) -> Self {
        let r = side / 2;
        let measured = [cx.saturating_sub(r), cy.saturating_sub(r), (cx + r + 1).min(w), (cy + r + 1).min(h)];
        let span = (2 * r + 1 + 2 * LENS_MARGIN) as f32;
        let dev = ((lens.x * ppp / span).min(lens.y * ppp / span)).floor().max(2.0);
        LoupeGrid { cell: dev / ppp, measured }
    }

    /// Top-left corner (points, on a device pixel) of the preview pixel `(x, y)` when pixel `(cx, cy)` sits in the middle of `lens`.
    pub fn corner(&self, lens: egui::Rect, ppp: f32, cx: usize, cy: usize, x: i64, y: i64) -> egui::Pos2 {
        let snap = |v: f32| (v * ppp).round() / ppp;
        let o = egui::pos2(snap(lens.center().x - self.cell / 2.0), snap(lens.center().y - self.cell / 2.0));
        o + egui::vec2((x - cx as i64) as f32, (y - cy as i64) as f32) * self.cell
    }

    /// The measured square on screen: exactly the outer edges of its pixels.
    pub fn frame(&self, lens: egui::Rect, ppp: f32, cx: usize, cy: usize) -> egui::Rect {
        let [x0, y0, x1, y1] = self.measured;
        egui::Rect::from_min_max(self.corner(lens, ppp, cx, cy, x0 as i64, y0 as i64), self.corner(lens, ppp, cx, cy, x1 as i64, y1 as i64))
    }

    /// Range of preview pixels (relative to the centre pixel) that touch the lens: `(dx, dy)` from `-n` to `n`.
    pub fn reach(&self, lens: egui::Vec2) -> (i64, i64) {
        ((lens.x / 2.0 / self.cell).ceil() as i64 + 1, (lens.y / 2.0 / self.cell).ceil() as i64 + 1)
    }
}

/// Paint the loupe next to the pointer `m` inside `area`: the preview pixels around the sampled pixel (`px`, `py` of
/// the `w` × `h` preview) as sharp blocks (no smoothing), the measured square as a thin frame on the pixel edges and
/// the clicked pixel marked in its middle. `draw_cell(painter, rect, x, y)` paints preview pixel `(x, y)` flat into
/// `rect`. `side`: edge of the measured square in preview pixels.
#[allow(clippy::too_many_arguments)]
pub fn paint_loupe(painter: &egui::Painter, area: egui::Rect, m: egui::Pos2, (px, py): (usize, usize), (w, h): (usize, usize), side: usize, probe: &Probe, mut draw_cell: impl FnMut(&egui::Painter, egui::Rect, usize, usize)) -> egui::Rect {
    let size = egui::vec2(LENS.x, LENS.y + 3.0 * LINE + PAD);
    // below right of the pointer, flipped at the edges of the image area (never under the pointer)
    let gap = 22.0;
    let x = if m.x + gap + size.x <= area.right() - 4.0 { m.x + gap } else { m.x - gap - size.x };
    let y = if m.y + gap + size.y <= area.bottom() - 4.0 { m.y + gap } else { m.y - gap - size.y };
    let outer = egui::Rect::from_min_size(egui::pos2(x.max(area.left() + 4.0), y.max(area.top() + 4.0)), size);
    let lens = egui::Rect::from_min_size(outer.min, LENS);
    painter.rect_filled(outer, 2.0, lr::PANEL_DARK);
    let ppp = painter.ctx().pixels_per_point();
    let grid = LoupeGrid::new(LENS, ppp, side, px, py, w, h);
    let lp = painter.with_clip_rect(lens);
    lp.rect_filled(lens, 0.0, Color32::BLACK);
    let (nx, ny) = grid.reach(LENS);
    for dy in -ny..=ny {
        for dx in -nx..=nx {
            let (x, y) = (px as i64 + dx, py as i64 + dy);
            if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                continue;
            }
            let c = grid.corner(lens, ppp, px, py, x, y);
            draw_cell(&lp, egui::Rect::from_min_size(c, egui::vec2(grid.cell, grid.cell)), x as usize, y as usize);
        }
    }
    // the measured square: thin light line with a dark rim, outside of the measured pixels (visible on any colour)
    let sq = grid.frame(lens, ppp, px, py);
    let hair = 1.0 / ppp;
    lp.rect_stroke(sq, 0.0, egui::Stroke::new(hair, Color32::from_white_alpha(235)), egui::StrokeKind::Outside);
    lp.rect_stroke(sq.expand(hair), 0.0, egui::Stroke::new(hair, Color32::from_black_alpha(200)), egui::StrokeKind::Outside);
    // the clicked pixel: a small dot in its middle
    let dot = egui::Rect::from_center_size(grid.corner(lens, ppp, px, py, px as i64, py as i64) + egui::vec2(grid.cell, grid.cell) / 2.0, egui::vec2(grid.cell * 0.34, grid.cell * 0.34).max(egui::vec2(2.0, 2.0)));
    lp.rect_filled(dot, 0.0, Color32::from_white_alpha(235));
    lp.rect_stroke(dot, 0.0, egui::Stroke::new(hair, Color32::from_black_alpha(220)), egui::StrokeKind::Outside);
    let (values, (state, color)) = lines(probe, side);
    let tp = painter.with_clip_rect(outer.shrink(2.0));
    let font = egui::FontId::proportional(11.5);
    let mut y = lens.bottom() + PAD * 0.5;
    for v in values.iter() {
        tp.text(egui::pos2(outer.left() + PAD, y), egui::Align2::LEFT_TOP, v, font.clone(), lr::TEXT);
        y += LINE;
    }
    tp.text(egui::pos2(outer.left() + PAD, y), egui::Align2::LEFT_TOP, state, font, color);
    painter.rect_stroke(outer, 2.0, egui::Stroke::new(1.0, lr::FIELD_STROKE), egui::StrokeKind::Inside);
    outer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_follow_their_photo() {
        let mut st = State::default();
        st.follow(3);
        st.taken.push([1.0, 1.0, 1.0]);
        st.follow(3);
        assert_eq!(st.taken.len(), 1);
        st.follow(4);
        assert!(st.taken.is_empty());
    }

    /// Loupe: whole device pixels per preview pixel, the frame sits exactly on the pixel edges, also at the image border.
    #[test]
    fn loupe_grid() {
        let lens = LENS;
        for ppp in [1.0f32, 2.0] {
            let cell = |side| LoupeGrid::new(lens, ppp, side, 100, 100, 400, 300).cell;
            // small squares are magnified much more than big ones, never below 1 point
            assert!(cell(5) > cell(9) && cell(9) > cell(33) && cell(33) >= 1.0 / ppp);
            for side in [1usize, 3, 5, 9, 17, 33] {
                let g = LoupeGrid::new(lens, ppp, side, 100, 100, 400, 300);
                assert!((g.cell * ppp).fract() == 0.0, "whole device pixels: {g:?}");
                // the square plus the margin fits the lens height
                assert!(g.cell * (side + 2 * LENS_MARGIN) as f32 <= lens.y + 1e-3 || g.cell * ppp <= 2.0);
                let l = egui::Rect::from_min_size(egui::pos2(10.3, 20.7), lens);
                let f = g.frame(l, ppp, 100, 100);
                assert!((f.width() - side as f32 * g.cell).abs() < 1e-3 && (f.height() - side as f32 * g.cell).abs() < 1e-3);
                for v in [f.min.x, f.min.y, f.max.x, f.max.y] {
                    assert!(((v * ppp) - (v * ppp).round()).abs() < 1e-3, "frame edge on a device pixel: {v}");
                }
                // the clicked pixel is centred in the lens
                let c = g.corner(l, ppp, 100, 100, 100, 100) + egui::vec2(g.cell, g.cell) / 2.0;
                assert!((c.x - l.center().x).abs() <= 0.5 / ppp + 1e-3 && (c.y - l.center().y).abs() <= 0.5 / ppp + 1e-3);
            }
        }
        // at the border the frame covers only the measured pixels: 9 × 9 at (1, 0) → x 0..6, y 0..5
        let g = LoupeGrid::new(lens, 1.0, 9, 1, 0, 400, 300);
        assert_eq!(g.measured, [0, 0, 6, 5]);
        let l = egui::Rect::from_min_size(egui::Pos2::ZERO, lens);
        let f = g.frame(l, 1.0, 1, 0);
        assert!((f.width() - 6.0 * g.cell).abs() < 1e-3 && (f.height() - 5.0 * g.cell).abs() < 1e-3);
        let g = LoupeGrid::new(lens, 1.0, 9, 399, 299, 400, 300);
        assert_eq!(g.measured, [395, 295, 400, 300]);
    }

    const LV: Levels = Levels { white: [1.0; 3] };

    fn flat(v: [f32; 3]) -> impl Fn(usize, usize) -> [f32; 3] {
        move |_, _| v
    }

    /// A grey patch: the mean is the patch, every pixel kept.
    #[test]
    fn grey_patch() {
        let s = sample(&flat([0.2, 0.25, 0.3]), 100, 100, 50, 50, 5, &LV);
        assert_eq!(s.verdict, Verdict::Good);
        assert_eq!((s.kept, s.total), (25, 25));
        assert!((s.mean[0] - 0.2).abs() < 1e-6 && (s.mean[2] - 0.3).abs() < 1e-6);
    }

    /// Noise (high ISO, σ = 20 % per channel): the 17 × 17 sample scatters about a tenth as much as a single pixel and
    /// stays within 1.5 % of the true colour on average.
    #[test]
    fn robust_against_noise() {
        // deterministic pseudo-random noise
        let noisy = |x: usize, y: usize| {
            let n = |k: u32| {
                let mut h = (x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663) ^ k.wrapping_mul(83_492_791);
                h ^= h >> 13;
                h = h.wrapping_mul(0x5bd1_e995);
                h ^= h >> 15;
                // sum of two uniforms: triangular, σ ≈ 0.41 · 0.49 ≈ 0.2
                ((h & 0xffff) as f32 / 65535.0 + ((h >> 16) & 0xffff) as f32 / 65535.0 - 1.0) * 0.49
            };
            [0.2 * (1.0 + n(1)), 0.2 * (1.0 + n(2)), 0.2 * (1.0 + n(3))]
        };
        let scatter = |side: usize| {
            let v: Vec<f64> = (0..40).map(|i| {
                let s = sample(&noisy, 2000, 2000, 50 + i * 40, 60 + i * 37, side, &LV);
                assert_eq!(s.verdict, Verdict::Good);
                s.mean[0] / s.mean[1]
            }).collect();
            let m = v.iter().sum::<f64>() / v.len() as f64;
            (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt())
        };
        let (m17, s17) = scatter(17);
        let (_, s1) = scatter(1);
        assert!((m17 - 1.0).abs() < 0.015, "mean {m17}");
        assert!(s17 < s1 / 8.0, "17×17 {s17} vs 1×1 {s1}");
    }

    /// A hot pixel and an edge of another colour inside the square do not pull the mean away.
    #[test]
    fn outliers_are_trimmed() {
        let img = |x: usize, y: usize| {
            if (x, y) == (50, 50) {
                [0.9, 0.05, 0.05]
            } else if x == 52 {
                [0.3, 0.1, 0.05]
            } else {
                [0.2, 0.2, 0.2]
            }
        };
        let s = sample(&img, 100, 100, 50, 50, 5, &LV);
        assert_eq!(s.verdict, Verdict::Good);
        assert!((s.mean[0] / s.mean[1] - 1.0).abs() < 1e-6 && (s.mean[2] / s.mean[1] - 1.0).abs() < 1e-6, "{:?}", s.mean);
        assert_eq!(s.kept, 19);
    }

    /// Blown out and too dark spots are rejected; a partly blown square keeps its good pixels.
    #[test]
    fn clipped_and_dark_are_rejected() {
        assert_eq!(sample(&flat([0.97, 0.99, 0.9]), 50, 50, 25, 25, 5, &LV).verdict, Verdict::Clipped);
        assert_eq!(sample(&flat([0.001, 0.002, 0.001]), 50, 50, 25, 25, 5, &LV).verdict, Verdict::Dark);
        // per-channel white level: red reaches its as-shot gain 2.0 before it is blown
        let raw = Levels { white: [2.0, 1.0, 1.5] };
        assert_eq!(sample(&flat([1.5, 0.6, 0.8]), 50, 50, 25, 25, 5, &raw).verdict, Verdict::Good);
        assert_eq!(sample(&flat([1.95, 0.6, 0.8]), 50, 50, 25, 25, 5, &raw).verdict, Verdict::Clipped);
        let part = |x: usize, _y: usize| if x < 25 { [1.0, 1.0, 1.0] } else { [0.5, 0.5, 0.5] };
        let s = sample(&part, 50, 50, 25, 25, 5, &LV);
        assert_eq!((s.verdict, s.kept), (Verdict::Good, 15));
    }

    /// Foliage or sky imply a light far from the daylight/tungsten curve: flagged; daylight, tungsten, shade are not.
    #[test]
    fn unusual_light_is_doubtful() {
        let k = |kelvin: f32, tint: f32| Outcome::Kelvin { kelvin, tint, exact: true };
        assert!(!k(5500.0, 10.0).doubtful() && !k(2850.0, 0.0).doubtful() && !k(7500.0, 10.0).doubtful() && !k(3800.0, 21.0).doubtful());
        assert!(k(5000.0, 60.0).doubtful() && k(5000.0, -35.0).doubtful() && k(30000.0, 10.0).doubtful());
    }

    #[test]
    fn samples_combine_by_colour() {
        let c = combine(&[[0.2, 0.1, 0.05], [0.05, 0.1, 0.2]]).unwrap();
        assert!((c[0] / c[1] - 1.0).abs() < 1e-9 && (c[2] / c[1] - 1.0).abs() < 1e-9, "{c:?}");
        assert_eq!(combine(&[]), None);
    }

    /// The square is cut at the image border.
    #[test]
    fn border() {
        let s = sample(&flat([0.2, 0.2, 0.2]), 10, 10, 0, 0, 5, &LV);
        assert_eq!(s.total, 9);
    }
}

/// The picker in the whole app (needs `GRAINEO_DATA_DIR`): a grey card under 3200 K light in a finished image, picked
/// with a click, gives 3200 K / tint 0 (the relative sliders −…); ⌘Z takes the white balance back, ⇧⌘Z sets it again;
/// a ⇧-click keeps the picker on; a blown-out spot changes nothing.
#[cfg(test)]
mod app_tests {
    use crate::App;
    use eframe::App as _;
    use eframe::egui;

    /// sRGB-encoded 16-bit value of a neutral surface (luminance `y`) under the light (Kelvin, tint), as a camera
    /// balanced for D65 records it (finished image without adaptation).
    fn under_light(kelvin: f32, tint: f32, y: f32) -> [u16; 3] {
        let [x, yy] = rawdev::wb::temp_to_xy(kelvin, tint);
        let xyz = [x / yy * y, y, (1.0 - x - yy) / yy * y];
        let m = raw::color::inv(&raw::color::srgb_to_xyz_d65()).unwrap();
        let lin = raw::color::mulv(&m, xyz);
        lin.map(|v| {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (e * 65535.0).round() as u16
        })
    }

    #[test]
    fn pick_undo_redo_on_a_grey_card() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-wb-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        // left: grey card under 3200 K; right quarter: blown out
        let card = under_light(3200.0, 0.0, 0.2);
        let img = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_fn(600, 400, |x, _| if x < 450 { image::Rgb(card) } else { image::Rgb([65535; 3]) });
        img.save(folder.join("card.png")).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(folder.clone());
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline, "photo does not open");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut time = 10.0;
        let mut frame = |a: &mut App, events: Vec<egui::Event>| {
            time += 0.05;
            // held modifier keys: those of the events (egui reads them from the input, not from the events)
            let modifiers = events.iter().find_map(|e| match e {
                egui::Event::PointerButton { modifiers, .. } | egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            }).unwrap_or_default();
            let mut events = events;
            events.insert(0, egui::Event::ModifiersChanged(modifiers));
            let mut input = egui::RawInput { time: Some(time), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1000.0))), events, ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        for _ in 0..4 {
            frame(&mut a, vec![]);
        }
        let r = a.image_rect.expect("image shown");
        let on_card = r.left_center() + egui::vec2(r.width() * 0.3, 0.0);
        let blown = r.right_center() - egui::vec2(r.width() * 0.1, 0.0);
        let mods = |command: bool, shift: bool| egui::Modifiers { command, mac_cmd: command, shift, ..Default::default() };
        let click = |p: egui::Pos2, m: egui::Modifiers| vec![vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: m }], vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: m }], vec![]];
        let key = |k: egui::Key, m: egui::Modifiers| vec![egui::Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers: m }, egui::Event::Key { key: k, physical_key: Some(k), pressed: false, repeat: false, modifiers: m }];
        let cur = a.current.as_ref().unwrap().index;
        let wb = |a: &App| {
            let r = &a.entries[cur].settings.raw;
            (r.wb_mode.clone(), r.temperature, r.tint)
        };
        let start = wb(&a);
        assert_eq!(start.0, "as_shot");

        // a blown-out spot: nothing changes, the picker stays on
        a.wb_picker = true;
        for e in click(blown, mods(false, false)) {
            frame(&mut a, e);
        }
        assert_eq!(wb(&a), start, "a blown-out spot must not change the white balance");
        assert!(a.wb_picker, "the picker stays on after a rejected spot");

        // the grey card: 3200 K / 0
        for e in click(on_card, mods(false, false)) {
            frame(&mut a, e);
        }
        let picked = wb(&a);
        assert_eq!(picked.0, "custom");
        assert!((picked.1 - 3200.0).abs() < 25.0 && picked.2.abs() < 1.0, "grey card under 3200 K: {picked:?}");
        assert!(!a.wb_picker, "a plain click ends the picker");

        // ⌘Z: back to as shot; ⇧⌘Z: the picked white balance again
        frame(&mut a, key(egui::Key::Z, mods(true, false)));
        frame(&mut a, vec![]);
        assert_eq!(wb(&a), start, "⌘Z must take the picked white balance back");
        frame(&mut a, key(egui::Key::Z, mods(true, true)));
        frame(&mut a, vec![]);
        assert_eq!(wb(&a), picked, "⇧⌘Z must set it again");

        // ⇧-click: the picker stays on, each click is a step
        a.wb_picker = true;
        for e in click(on_card + egui::vec2(10.0, 10.0), mods(false, true)) {
            frame(&mut a, e);
        }
        assert!(a.wb_picker, "⇧-click keeps the picker on");
        assert_eq!(a.wb_pick.taken.len(), 1);
        // the samples belong to their photo: after a switch to another photo the next ⇧-click starts afresh
        assert_eq!(a.wb_pick.image, Some(cur));
        a.wb_pick.image = Some(usize::MAX);
        for e in click(on_card + egui::vec2(12.0, 12.0), mods(false, true)) {
            frame(&mut a, e);
        }
        assert_eq!(a.wb_pick.taken.len(), 1, "samples of another photo are forgotten");
        assert_eq!(a.wb_pick.image, Some(cur));
        frame(&mut a, key(egui::Key::Escape, mods(false, false)));
        frame(&mut a, vec![]);
        assert!(!a.wb_picker && a.wb_pick.taken.is_empty(), "Esc ends the picker and forgets the samples");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

/// Measurement (not a check): `GRAINEO_WB_PICK_SPOTS="file@fx,fy;…"` (fractions of the oriented preview) prints, per
/// spot and sample size, what a click sets and how much the result scatters when the click lands a few preview pixels
/// off (7 × 7 positions, ±6 px), next to the method before 18.11.2026 (plain 5 × 5 mean, any value of the as-shot
/// balanced preview outside 0.2 … 95 % rejects the spot).
#[cfg(test)]
mod measure {
    use super::*;

    fn stats(v: &[f64]) -> (f64, f64) {
        let m = v.iter().sum::<f64>() / v.len().max(1) as f64;
        (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt())
    }

    #[test]
    fn spots() {
        let Ok(spec) = std::env::var("GRAINEO_WB_PICK_SPOTS") else { return };
        for item in spec.split(';').filter(|s| !s.trim().is_empty()) {
            let (file, at) = item.rsplit_once('@').expect("file@fx,fy");
            let (fx, fy) = at.split_once(',').map(|(a, b)| (a.parse::<f32>().unwrap(), b.parse::<f32>().unwrap())).unwrap();
            let mut doc = rawdev::RawDoc::open(std::path::Path::new(file), &|| true).unwrap().unwrap();
            rawdev::lightroom::apply_orientation(&mut doc);
            let st = doc.preview.clone();
            let (w, h) = (st.w, st.h);
            let px = |x: usize, y: usize| st.px[y * w + x];
            let lv = Levels { white: if doc.finished || doc.apple.is_some() { [1.0; 3] } else { doc.info.wb.unwrap_or([1.0; 3]) } };
            let (cx, cy) = ((fx * w as f32) as usize, (fy * h as f32) as usize);
            println!("{file} @ {cx},{cy} of {w}×{h} (ISO {:?}), as shot {:.0} K / {:+.1}", doc.info.exif.iso, doc.as_shot.0, doc.as_shot.1);
            let light = |mean: [f64; 3]| rawdev::wb_pick::light_of(&doc, mean);
            for side in [1usize, 3, 5, 9, 17, 33] {
                let s = sample(&px, w, h, cx, cy, side, &lv);
                let l = light(s.mean);
                let mut mired = Vec::new();
                let mut tint = Vec::new();
                for dy in (-6i32..=6).step_by(2) {
                    for dx in (-6i32..=6).step_by(2) {
                        let (x, y) = ((cx as i32 + dx).clamp(0, w as i32 - 1) as usize, (cy as i32 + dy).clamp(0, h as i32 - 1) as usize);
                        let t = sample(&px, w, h, x, y, side, &lv);
                        if let (true, Some(l)) = (t.verdict.usable(), light(t.mean)) {
                            mired.push(1e6 / l.kelvin as f64);
                            tint.push(l.tint as f64);
                        }
                    }
                }
                let (mm, ms) = stats(&mired);
                let (_, ts) = stats(&tint);
                match l {
                    Some(l) => println!("  {side:>2}×{side:<2} {:?} kept {:>4}/{:<4} → {:>6.0} K {:+6.1} exact {} | scatter ±{:.1} mired (≈ ±{:.0} K) ±{:.1} tint over {} usable of 49", s.verdict, s.kept, s.total, l.kelvin, l.tint, l.exact, ms, (1e6 / (mm - ms) - 1e6 / (mm + ms)).abs() / 2.0, ts, mired.len()),
                    None => println!("  {side:>2}×{side:<2} {:?} → none", s.verdict),
                }
            }
            // before: plain 5 × 5 mean, range check on the balanced values
            let mut acc = [0.0f64; 3];
            let mut n = 0.0;
            for y in cy.saturating_sub(2)..(cy + 3).min(h) {
                for x in cx.saturating_sub(2)..(cx + 3).min(w) {
                    let q = px(x, y);
                    for k in 0..3 {
                        acc[k] += q[k] as f64;
                    }
                    n += 1.0;
                }
            }
            let m = acc.map(|a| a / n);
            let rejected = m.iter().any(|&v| !(0.002..0.95).contains(&v));
            println!("  before: 5×5 plain mean {m:.4?} → {}", if rejected { "rejected".to_string() } else { light(m).map_or("none".into(), |l| format!("{:.0} K {:+.1}", l.kelvin, l.tint)) });
        }
    }
}
