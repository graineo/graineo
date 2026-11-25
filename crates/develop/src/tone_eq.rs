//! Tonwert-Equalizer (tone equalizer): exposure per brightness zone.
//!
//! Nine zones from −8 EV to 0 EV (0 EV = white of the scene-linear image after exposure). Each zone gets an exposure
//! correction of −2 … +2 EV; between the zones the correction is interpolated smoothly. The correction is a factor
//! on all three channels in the linear stage (like light added to or taken from the scene), so colours keep their
//! ratios.
//!
//! Which zone a pixel belongs to is not decided by the pixel alone but by an edge-preserving brightness mask: a
//! guided filter (He, Sun & Tang, "Guided Image Filtering", ECCV 2010; fast variant on a reduced grid: He & Sun,
//! "Fast Guided Filter", 2015) smooths the brightness inside surfaces and keeps it at edges. A whole surface
//! (a face in backlight, the sky) therefore moves as one, local contrast inside it is kept, and there are no halos at
//! the edges.
//!
//! Method after darktable's tone equalizer (Aurélien Pierre, `src/iop/toneequal.c` and
//! `src/common/fast_guided_filter.h`, GPL-3.0): zones at −8 … 0 EV, radial-basis interpolation with eight Gaussians
//! (σ = √2 EV) fitted by least squares to the nine slider values, correction clamped to ±2 EV, mask diameter in
//! percent of the long image edge, guided-filter coefficients on a downscaled grid, upsampled bilinearly and applied
//! to the full-resolution brightness.
//! Own deviations: the Gaussians are fitted to the slider values in EV (darktable fits the linear factors), so all
//! sliders at 0 give exactly no change and the curve is linear in the sliders; and the guided filter runs on log2
//! brightness (EV) instead of linear brightness. The mask is then
//! independent of the exposure (it only shifts), so it is computed once per image and mask setting, and edges in the
//! shadows are kept as well as edges in the highlights.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Number of zones (sliders).
pub const ZONES: usize = 9;
/// Centre of each zone in EV (0 = white).
pub const ZONE_EV: [f32; ZONES] = [-8.0, -7.0, -6.0, -5.0, -4.0, -3.0, -2.0, -1.0, 0.0];
/// Slider range in EV.
pub const MAX_EV: f32 = 2.0;
/// Lower end of the zone range (the interpolation is only valid in −8 … 0 EV; brightness outside is clamped).
const MIN_EV: f32 = -8.0;
/// Gaussians of the interpolation: eight centres spread evenly over −8 … 0 EV (darktable).
const CENTERS: usize = 8;
/// Width of the Gaussians in EV (darktable's default `smoothing` = √2).
const SIGMA: f64 = std::f64::consts::SQRT_2;
/// Table resolution: entries per EV.
const LUT_PER_EV: usize = 512;
/// Lower bound of the brightness relative to white (−14 EV): noise around black does not decide the mask.
const FLOOR_EV: f32 = -14.0;

/// Settings of the tone equalizer (stored in the development settings as `toneEqualizer`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneEq {
    /// Main slider "Ausgleich" in EV (−2 … +2): + brightens shadows and midtones, the highlights stay exactly as they
    /// are; − darkens highlights and midtones, the deep shadows stay exactly as they are (see [`balance_ev`])
    pub amount: f32,
    /// Fine adjustment: exposure correction per zone in EV (−2 … +2), from the darkest (−8 EV) to white (0 EV); adds to
    /// the main slider
    pub zones: [f32; ZONES],
    /// Mask smoothing: diameter of the guided filter in percent of the long image edge (1 … 25, default 5)
    pub smoothing: f32,
    /// Edge sharpness of the mask 0 … 1 (default 0.5): higher keeps the mask closer to edges
    pub edges: f32,
}

impl Default for ToneEq {
    fn default() -> Self {
        Self { amount: 0.0, zones: [0.0; ZONES], smoothing: 5.0, edges: 0.5 }
    }
}

impl ToneEq {
    /// Does the equalizer change anything? (Main slider and all zones at 0: off, the image stays bit-identical.)
    pub fn active(&self) -> bool {
        self.amount != 0.0 || self.zones_active()
    }

    /// Is any fine-adjustment zone set?
    pub fn zones_active(&self) -> bool {
        self.zones.iter().any(|z| *z != 0.0)
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Key of the mask: it only depends on the image, smoothing and edge sharpness.
    pub fn mask_key(&self) -> (u32, u32) {
        ((self.smoothing.clamp(0.5, 50.0) * 100.0).round() as u32, (self.edges.clamp(0.0, 1.0) * 1000.0).round() as u32)
    }

    /// Regularisation ε of the guided filter in EV²: from 4 (very soft, edge sharpness 0) to 1/64 (edge sharpness 1).
    fn eps(&self) -> f32 {
        (2.0 - 8.0 * self.edges.clamp(0.0, 1.0)).exp2()
    }
}

/// Brightness estimate of a pixel: Euclidean norm of the three channels, scaled so that grey keeps its value (darktable's
/// default estimator "RGB euclidean norm"; saturated colours count a little brighter than their luminance, which keeps
/// strongly coloured highlights from being pushed).
#[inline]
pub fn brightness(p: [f32; 3]) -> f32 {
    ((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]) * (1.0 / 3.0)).sqrt()
}

/// Guided-filter coefficients of a whole image on a reduced grid: mask(EV) = a · log2(brightness) + b.
/// Depends only on the image and the mask settings, never on the zone sliders or the exposure.
#[derive(Debug)]
pub struct Grid {
    /// Size of the analysed image
    pub width: usize,
    pub height: usize,
    /// Grid size and cell size (analysed pixels per grid cell)
    gw: usize,
    gh: usize,
    cell: usize,
    a: Vec<f32>,
    b: Vec<f32>,
    /// Data value of white (0 EV before exposure) in the units of the analysed data
    white: f32,
    /// [`ToneEq::mask_key`] the grid was computed for
    pub key: (u32, u32),
}

/// Box mean with radius `r` (clamped window at the borders), rows and columns in parallel.
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        let mut cs = vec![0f64; w + 1];
        for x in 0..w {
            cs[x + 1] = cs[x] + s[x] as f64;
        }
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(w));
            row[x] = ((cs[b] - cs[a]) / (b - a) as f64) as f32;
        }
    });
    // columns: blocks of columns in parallel, each with its own running sums
    const BLOCK: usize = 32;
    let cols: Vec<(usize, Vec<f32>)> = (0..w.div_ceil(BLOCK))
        .into_par_iter()
        .map(|blk| {
            let (x0, x1) = (blk * BLOCK, ((blk + 1) * BLOCK).min(w));
            let bw = x1 - x0;
            let mut cs = vec![0f64; (h + 1) * bw];
            for y in 0..h {
                for i in 0..bw {
                    cs[(y + 1) * bw + i] = cs[y * bw + i] + tmp[y * w + x0 + i] as f64;
                }
            }
            let mut out = vec![0f32; h * bw];
            for y in 0..h {
                let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
                let n = (b - a) as f64;
                for i in 0..bw {
                    out[y * bw + i] = ((cs[b * bw + i] - cs[a * bw + i]) / n) as f32;
                }
            }
            (x0, out)
        })
        .collect();
    let mut out = vec![0f32; w * h];
    for (x0, block) in cols {
        let bw = block.len() / h.max(1);
        for y in 0..h {
            out[y * w + x0..y * w + x0 + bw].copy_from_slice(&block[y * bw..(y + 1) * bw]);
        }
    }
    out
}

impl Grid {
    /// Analyse interleaved RGB `data` (`w × h`, linear, any scale): `white` is the data value of white before
    /// exposure (e.g. 1 for values normalised to sensor saturation).
    pub fn analyze(data: &[f32], w: usize, h: usize, white: f32, t: &ToneEq) -> Self {
        let long = w.max(h).max(1);
        // grid of at most ≈ 768 cells on the long edge (at least 2 × 2 pixels per cell)
        let cell = long.div_ceil(768).max(2);
        let (gw, gh) = (w.div_ceil(cell).max(1), h.div_ceil(cell).max(1));
        let floor = white.max(1e-12) * FLOOR_EV.exp2();
        // mean log2 brightness per cell
        let g: Vec<f32> = (0..gw * gh)
            .into_par_iter()
            .map(|k| {
                let (cx, cy) = (k % gw, k / gw);
                let (mut s, mut n) = (0f32, 0f32);
                for y in cy * cell..((cy + 1) * cell).min(h) {
                    for x in cx * cell..((cx + 1) * cell).min(w) {
                        let i = (y * w + x) * 3;
                        s += brightness([data[i], data[i + 1], data[i + 2]]).max(floor).log2();
                        n += 1.0;
                    }
                }
                if n > 0.0 { s / n } else { floor.log2() }
            })
            .collect();
        // radius of the guided filter in grid cells (diameter in percent of the long edge)
        let diameter = t.smoothing.clamp(0.5, 50.0) / 100.0 * long as f32;
        let r = ((diameter / 2.0) / cell as f32).round().max(1.0) as usize;
        let eps = t.eps();
        // self-guided filter (guide = input): a = var / (var + ε), b = mean · (1 − a); then the window mean of a, b
        let mean = box_mean(&g, gw, gh, r);
        let mean2 = box_mean(&g.iter().map(|v| v * v).collect::<Vec<_>>(), gw, gh, r);
        let (a, b): (Vec<f32>, Vec<f32>) = mean
            .par_iter()
            .zip(mean2.par_iter())
            .map(|(&m, &m2)| {
                let var = (m2 - m * m).max(0.0);
                let a = var / (var + eps);
                (a, m - a * m)
            })
            .unzip();
        let a = box_mean(&a, gw, gh, r);
        let b = box_mean(&b, gw, gh, r);
        Self { width: w, height: h, gw, gh, cell, a, b, white: white.max(1e-12), key: t.mask_key() }
    }

    /// Does this grid fit the mask settings?
    pub fn fits(&self, t: &ToneEq) -> bool {
        self.key == t.mask_key()
    }

    /// Coefficients (a, b) at a position in analysed pixels (pixel centres at +0.5), bilinear between the cell centres.
    #[inline]
    fn ab(&self, x: f32, y: f32) -> (f32, f32) {
        let c = self.cell as f32;
        let fx = (x / c - 0.5).clamp(0.0, (self.gw - 1) as f32);
        let fy = (y / c - 0.5).clamp(0.0, (self.gh - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(self.gw - 1), (y0 + 1).min(self.gh - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let lerp = |v: &[f32]| {
            let top = v[y0 * self.gw + x0] * (1.0 - tx) + v[y0 * self.gw + x1] * tx;
            let bottom = v[y1 * self.gw + x0] * (1.0 - tx) + v[y1 * self.gw + x1] * tx;
            top * (1.0 - ty) + bottom * ty
        };
        (lerp(&self.a), lerp(&self.b))
    }
}

/// The mask of an image or of a part of it: a view into the [`Grid`] of the whole image. Crops and downscaled copies
/// take their part of the same grid, so preview, 100 % view and export see the same mask.
#[derive(Clone, Debug)]
pub struct Mask {
    grid: Arc<Grid>,
    /// Position of pixel (i, j) of this image in the analysed image: (x0 + (i + 0.5) · step, y0 + (j + 0.5) · step)
    x0: f32,
    y0: f32,
    step: f32,
    pub width: usize,
    pub height: usize,
}

impl Mask {
    /// The mask of the analysed image itself.
    pub fn full(grid: Arc<Grid>) -> Self {
        let (width, height) = (grid.width, grid.height);
        Self { grid, x0: 0.0, y0: 0.0, step: 1.0, width, height }
    }

    /// The grid this mask comes from.
    pub fn grid(&self) -> &Arc<Grid> {
        &self.grid
    }

    /// Crop `[x, y, w, h]` (same rectangle as the image crop).
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { grid: self.grid.clone(), x0: self.x0 + x as f32 * self.step, y0: self.y0 + y as f32 * self.step, step: self.step, width: w, height: h }
    }

    /// Downscaled by an integer factor (block means; the new pixel covers `factor` old ones).
    pub fn downsample(&self, factor: usize, w: usize, h: usize) -> Self {
        Self { grid: self.grid.clone(), x0: self.x0, y0: self.y0, step: self.step * factor as f32, width: w, height: h }
    }

    /// The part `region = [x, y, w, h]` of an image of size `full_w × full_h` that shows the same scene as the
    /// analysed image (e.g. full resolution, analysed at preview size).
    pub fn resample(grid: Arc<Grid>, full_w: usize, full_h: usize, region: [usize; 4]) -> Self {
        let step = (grid.width as f32 / full_w.max(1) as f32 + grid.height as f32 / full_h.max(1) as f32) / 2.0;
        Self { x0: region[0] as f32 * step, y0: region[1] as f32 * step, step, width: region[2], height: region[3], grid }
    }

    /// The pixel's own brightness in EV relative to white (before exposure), same scale as [`Self::ev`].
    #[inline]
    pub fn pixel_ev(&self, p: [f32; 3]) -> f32 {
        let g = &*self.grid;
        (brightness(p).max(g.white * FLOOR_EV.exp2()) / g.white).log2()
    }

    /// Mask brightness in EV relative to white (before exposure) of pixel (x, y) with the colour `p` (same units as
    /// the analysed data).
    #[inline]
    pub fn ev(&self, x: usize, y: usize, p: [f32; 3]) -> f32 {
        let g = &*self.grid;
        let l = brightness(p).max(g.white * FLOOR_EV.exp2()).log2();
        let (a, b) = g.ab(self.x0 + (x as f32 + 0.5) * self.step, self.y0 + (y as f32 + 0.5) * self.step);
        a * l + b - g.white.log2()
    }
}

/// Correction curve of the fine adjustment: correction in EV over the zone brightness, as a table over −8 … 0 EV.
#[derive(Clone, Debug)]
pub struct Curve {
    lut: Vec<f32>,
}

/// Centres of the eight Gaussians in EV.
fn centers() -> [f64; CENTERS] {
    std::array::from_fn(|j| MIN_EV as f64 + j as f64 * (-MIN_EV as f64) / (CENTERS - 1) as f64)
}

#[inline]
fn gauss(d: f64) -> f64 {
    (-d * d / (2.0 * SIGMA * SIGMA)).exp()
}

/// Least-squares weights of the Gaussians for the nine slider values (in EV): A·w ≈ zones (in EV), with
/// A[i][j] = gauss(zone_i − centre_j), solved over the normal equations (8 × 8, Gaussian elimination).
fn weights(zones: &[f32; ZONES]) -> [f64; CENTERS] {
    let c = centers();
    let a: Vec<[f64; CENTERS]> = ZONE_EV.iter().map(|&z| std::array::from_fn(|j| gauss(z as f64 - c[j]))).collect();
    let y: Vec<f64> = zones.iter().map(|&v| v.clamp(-MAX_EV, MAX_EV) as f64).collect();
    let mut m = [[0f64; CENTERS + 1]; CENTERS];
    for r in 0..CENTERS {
        for k in 0..CENTERS {
            m[r][k] = (0..ZONES).map(|i| a[i][r] * a[i][k]).sum();
        }
        m[r][CENTERS] = (0..ZONES).map(|i| a[i][r] * y[i]).sum();
    }
    for col in 0..CENTERS {
        let piv = (col..CENTERS).max_by(|&p, &q| m[p][col].abs().total_cmp(&m[q][col].abs())).unwrap_or(col);
        m.swap(col, piv);
        let d = m[col][col];
        if d.abs() < 1e-300 {
            continue;
        }
        for r in 0..CENTERS {
            if r != col {
                let f = m[r][col] / d;
                for k in col..=CENTERS {
                    m[r][k] -= f * m[col][k];
                }
            }
        }
    }
    std::array::from_fn(|j| if m[j][j].abs() < 1e-300 { 0.0 } else { m[j][CENTERS] / m[j][j] })
}

impl Curve {
    pub fn new(zones: &[f32; ZONES]) -> Self {
        let w = weights(zones);
        let c = centers();
        let n = (-MIN_EV) as usize * LUT_PER_EV;
        let lut = (0..=n)
            .map(|k| {
                let ev = MIN_EV as f64 + k as f64 / LUT_PER_EV as f64;
                let g: f64 = (0..CENTERS).map(|j| w[j] * gauss(ev - c[j])).sum();
                g.clamp(-MAX_EV as f64, MAX_EV as f64) as f32
            })
            .collect();
        Self { lut }
    }

    /// Correction in EV at zone brightness `ev` (clamped to −8 … 0 EV), interpolated in the table.
    #[inline]
    pub fn ev(&self, ev: f32) -> f32 {
        let t = (ev.clamp(MIN_EV, 0.0) - MIN_EV) * LUT_PER_EV as f32;
        let i = (t as usize).min(self.lut.len() - 2);
        let f = t - i as f32;
        self.lut[i] + f * (self.lut[i + 1] - self.lut[i])
    }

    /// Linear gain at zone brightness `ev`.
    #[inline]
    pub fn gain(&self, ev: f32) -> f32 {
        self.ev(ev).exp2()
    }
}

#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Brightening: the highlights from here up (EV, pixel and surroundings) stay exactly unchanged.
pub const PROTECT_HIGHLIGHTS_EV: f32 = -1.0;
/// Darkening: the deep shadows from here down (EV, pixel and surroundings) stay exactly unchanged.
pub const PROTECT_SHADOWS_EV: f32 = -7.0;

/// Correction in EV of the main slider "Ausgleich" for a pixel with its own brightness `pixel_ev` and the brightness
/// of its surroundings `zone_ev` (mask), both relative to white after exposure.
///
/// - `amount > 0` (brighten): full effect where the surroundings are darker than −4.5 EV, fading out to exactly 0 at
///   −1 EV; additionally the pixel's own brightness fades the effect out from −4 EV to exactly 0 at −1 EV. Pixels at
///   or above −1 EV (and pixels in surroundings at or above −1 EV) are never touched; the fade over 3 EV keeps the
///   result monotone up to +2 EV, so nothing below −1 EV is lifted past it or into clipping.
/// - `amount < 0` (darken): mirror image for the shadows: full effect from −3 EV up, nothing at or below −6.5 EV
///   surroundings or −7 EV pixel brightness.
#[inline]
pub fn balance_ev(amount: f32, pixel_ev: f32, zone_ev: f32) -> f32 {
    let a = amount.clamp(-MAX_EV, MAX_EV);
    if a > 0.0 {
        if pixel_ev >= PROTECT_HIGHLIGHTS_EV || zone_ev >= PROTECT_HIGHLIGHTS_EV {
            return 0.0;
        }
        a * (1.0 - smoothstep(-4.5, PROTECT_HIGHLIGHTS_EV, zone_ev)) * (1.0 - smoothstep(-4.0, PROTECT_HIGHLIGHTS_EV, pixel_ev))
    } else if a < 0.0 {
        if pixel_ev <= PROTECT_SHADOWS_EV || zone_ev <= -6.5 {
            return 0.0;
        }
        a * smoothstep(-6.5, -3.0, zone_ev) * smoothstep(PROTECT_SHADOWS_EV, -4.0, pixel_ev)
    } else {
        0.0
    }
}

/// Everything one render needs: mask, curve and the exposure offset (zone EV = mask EV + exposure).
pub struct Apply<'a> {
    mask: &'a Mask,
    /// Fine adjustment (only when a zone is set)
    curve: Option<Curve>,
    amount: f32,
    offset: f32,
    /// Show the mask instead of the image ("Maske zeigen")
    pub show: bool,
}

impl<'a> Apply<'a> {
    /// `exposure`: exposure in EV acting after this stage (slider plus base exposure of the pipeline).
    pub fn new(t: &ToneEq, mask: &'a Mask, exposure: f32, show: bool) -> Self {
        Self { mask, curve: t.zones_active().then(|| Curve::new(&t.zones)), amount: t.amount, offset: exposure, show }
    }

    /// Zone brightness of pixel (x, y) in EV (0 = white after exposure).
    #[inline]
    pub fn zone_ev(&self, x: usize, y: usize, p: [f32; 3]) -> f32 {
        self.mask.ev(x, y, p) + self.offset
    }

    /// Linear gain of pixel (x, y) with colour `p`.
    #[inline]
    pub fn gain(&self, x: usize, y: usize, p: [f32; 3]) -> f32 {
        let zone = self.zone_ev(x, y, p);
        let mut ev = self.curve.as_ref().map_or(0.0, |c| c.ev(zone));
        if self.amount != 0.0 {
            ev += balance_ev(self.amount, self.mask.pixel_ev(p) + self.offset, zone);
        }
        if ev == 0.0 { 1.0 } else { ev.exp2() }
    }

    /// Grey value 0 … 1 (display-encoded) for "show mask": the nine zones as even steps from black (−8 EV and
    /// below) to white (0 EV and above).
    #[inline]
    pub fn mask_grey(&self, x: usize, y: usize, p: [f32; 3]) -> f32 {
        ((self.zone_ev(x, y, p) - MIN_EV) / -MIN_EV).clamp(0.0, 1.0)
    }
}

/// Zone histogram: lower end in EV (one zone below the darkest slider, darker pixels count here).
pub const HIST_MIN_EV: f32 = -9.0;
/// Zone histogram: upper end in EV (one zone above white, brighter pixels count here).
pub const HIST_MAX_EV: f32 = 1.0;
/// Zone histogram: bins per EV.
pub const HIST_PER_EV: usize = 16;
/// Number of bins of the zone histogram.
pub const HIST_BINS: usize = (HIST_MAX_EV - HIST_MIN_EV) as usize * HIST_PER_EV;

/// Bin of a zone brightness in EV (outside the range: the first or last bin).
#[inline]
pub fn hist_bin(zone_ev: f32) -> usize {
    (((zone_ev - HIST_MIN_EV) * HIST_PER_EV as f32).max(0.0) as usize).min(HIST_BINS - 1)
}

/// Collects the histogram of the zone brightness (the quantity the curve is evaluated at, see [`Apply::zone_ev`])
/// while an image is rendered. Rows add their counts at once.
#[derive(Debug)]
pub struct HistSink {
    bins: Vec<std::sync::atomic::AtomicU32>,
    used: std::sync::atomic::AtomicBool,
}

impl Default for HistSink {
    fn default() -> Self {
        Self::new()
    }
}

impl HistSink {
    pub fn new() -> Self {
        Self { bins: (0..HIST_BINS).map(|_| std::sync::atomic::AtomicU32::new(0)).collect(), used: std::sync::atomic::AtomicBool::new(false) }
    }

    /// Add the counts of one row (or any part of the image).
    pub fn add(&self, local: &[u32; HIST_BINS]) {
        use std::sync::atomic::Ordering::Relaxed;
        for (b, &n) in self.bins.iter().zip(local.iter()) {
            if n > 0 {
                b.fetch_add(n, Relaxed);
            }
        }
        self.used.store(true, Relaxed);
    }

    /// Add the zone brightness of every pixel of a `w × h` image (`p(x, y)` = the pixel as the tone equalizer sees it).
    pub fn add_image(&self, t: &Apply, w: usize, h: usize, p: impl Fn(usize, usize) -> [f32; 3] + Sync) {
        (0..h).into_par_iter().for_each(|y| {
            let mut local = [0u32; HIST_BINS];
            for x in 0..w {
                local[hist_bin(t.zone_ev(x, y, p(x, y)))] += 1;
            }
            self.add(&local);
        });
    }

    /// The counts, `None` when nothing was added.
    pub fn take(&self) -> Option<Vec<u32>> {
        use std::sync::atomic::Ordering::Relaxed;
        self.used.load(Relaxed).then(|| self.bins.iter().map(|b| b.load(Relaxed)).collect())
    }
}

thread_local! {
    static COLLECT: std::cell::RefCell<Option<Arc<HistSink>>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` (a render on this thread) and collect the zone histogram of what it renders. `None` when nothing was
/// collected (e.g. another view than the developed image). Rendering itself is unchanged: with the equalizer off the
/// mask is computed only for the histogram and the gain stays exactly 1.
pub fn collect<R>(f: impl FnOnce() -> R) -> (R, Option<Vec<u32>>) {
    let sink = Arc::new(HistSink::new());
    let prev = COLLECT.with(|c| c.replace(Some(sink.clone())));
    let r = f();
    COLLECT.with(|c| *c.borrow_mut() = prev);
    (r, sink.take())
}

/// The histogram collector of this thread (set by [`collect`]).
pub fn collector() -> Option<Arc<HistSink>> {
    COLLECT.with(|c| c.borrow().clone())
}

/// Is the mask needed: for the equalizer itself ([`needed`]) or for a zone histogram collected on this thread?
pub fn wanted(s: &crate::Settings) -> bool {
    needed(s) || (s.view == crate::View::Developed && collector().is_some())
}

/// Is the tone equalizer needed for these settings (zones set or the mask shown)?
pub fn needed(s: &crate::Settings) -> bool {
    s.view == crate::View::Developed && (s.tone_eq.active() || s.tone_eq_mask)
}

/// Mask for an image: the one handed in (of the whole image, cropped) if it fits the settings, else computed for
/// this image as a whole.
pub fn mask_for(given: Option<&Mask>, data: &[f32], w: usize, h: usize, white: f32, t: &ToneEq) -> Mask {
    match given.filter(|m| m.grid.fits(t) && (m.width, m.height) == (w, h)) {
        Some(m) => m.clone(),
        None => Mask::full(Arc::new(Grid::analyze(data, w, h, white, t))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hist_bins_cover_range() {
        assert_eq!(hist_bin(-100.0), 0);
        assert_eq!(hist_bin(HIST_MIN_EV), 0);
        assert_eq!(hist_bin(-8.0), HIST_PER_EV);
        assert_eq!(hist_bin(0.0), (0.0 - HIST_MIN_EV) as usize * HIST_PER_EV);
        assert_eq!(hist_bin(50.0), HIST_BINS - 1);
        let sink = HistSink::new();
        assert!(sink.take().is_none());
        let mut row = [0u32; HIST_BINS];
        row[3] = 2;
        sink.add(&row);
        sink.add(&row);
        assert_eq!(sink.take().unwrap()[3], 4);
        let ((), h) = collect(|| assert!(collector().is_some()));
        assert!(h.is_none());
        assert!(collector().is_none());
    }

    /// Neutral: all zones at 0 → not active (the image stays bit-identical), and the curve itself is exactly 1.
    #[test]
    fn neutral_is_off() {
        let t = ToneEq::default();
        assert!(!t.active());
        let c = Curve::new(&t.zones);
        for k in 0..=80 {
            let ev = -8.0 + k as f32 * 0.1;
            assert!((c.gain(ev) - 1.0).abs() < 1e-6, "gain at {ev}: {}", c.gain(ev));
        }
    }

    /// The fitted curve meets the slider values at the zone centres (within the approximation of nine points by eight
    /// Gaussians) for a smooth setting.
    #[test]
    fn curve_follows_sliders() {
        let z = [1.0, 1.0, 0.8, 0.6, 0.4, 0.2, 0.0, -0.3, -0.5];
        let c = Curve::new(&z);
        for (i, &ev) in ZONE_EV.iter().enumerate() {
            let got = c.gain(ev).log2();
            assert!((got - z[i]).abs() < 0.12, "zone {ev}: {got} vs {}", z[i]);
        }
    }

    /// Brightening the shadows keeps the output ordered (no tone inversion) and leaves white below clipping.
    #[test]
    fn shadows_up_monotone_and_highlights_kept() {
        let z = [1.5, 1.5, 1.4, 1.2, 0.8, 0.4, 0.1, 0.0, 0.0];
        let c = Curve::new(&z);
        let mut last = 0.0f32;
        for k in 0..=1600 {
            let ev = -12.0 + k as f32 * 0.0075;
            let x = ev.exp2();
            let y = x * c.gain(ev);
            assert!(y >= last, "not monotone at {ev}: {y} < {last}");
            last = y;
        }
        // highlights: 0 EV and −0.5 EV stay within 3 % of their value, white does not clip
        for ev in [-0.5f32, -0.25, 0.0] {
            let g = c.gain(ev);
            assert!((g - 1.0).abs() < 0.03, "highlight gain at {ev}: {g}");
        }
        // a pixel just below white stays below clipping
        let x = 0.97f32;
        assert!(x * c.gain(x.log2()) < 1.0, "0.97 → {}", x * c.gain(x.log2()));
    }

    /// Darkening the highlights does not press the shadows.
    #[test]
    fn highlights_down_keeps_shadows() {
        let z = [0.0, 0.0, 0.0, 0.0, 0.0, -0.2, -0.6, -1.0, -1.2];
        let c = Curve::new(&z);
        for ev in [-8.0f32, -7.0, -6.0, -5.0] {
            assert!((c.gain(ev).log2()).abs() < 0.05, "shadow gain at {ev}: {}", c.gain(ev).log2());
        }
        assert!(c.gain(0.0).log2() < -1.0);
    }

    /// Main slider +2: everything at or above −1 EV (pixel or surroundings) keeps exactly its value; below, the result
    /// stays ordered and below −1 EV + rounding (no new clipping); −2 mirrors this for the deep shadows.
    #[test]
    fn balance_protects() {
        for &zone in &[-9.0f32, -6.0, -4.5, -3.0, -2.0, -1.2, -1.0, 0.0, 1.0] {
            let mut last = f32::MIN;
            for k in 0..=1200 {
                let x = -10.0 + k as f32 * 0.01;
                let e = balance_ev(2.0, x, zone);
                if x >= PROTECT_HIGHLIGHTS_EV || zone >= PROTECT_HIGHLIGHTS_EV {
                    assert_eq!(e, 0.0, "pixel {x} zone {zone}");
                } else {
                    assert!(x + e <= PROTECT_HIGHLIGHTS_EV + 1e-4, "lifted past −1 EV: {x} → {}", x + e);
                }
                assert!(x + e >= last - 1e-4, "not monotone at {x} (zone {zone})");
                last = x + e;
                let d = balance_ev(-2.0, x, zone);
                if x <= PROTECT_SHADOWS_EV || zone <= -6.5 {
                    assert_eq!(d, 0.0);
                } else {
                    assert!(x + d >= PROTECT_SHADOWS_EV - 1e-4, "pressed below −7 EV: {x} → {}", x + d);
                }
            }
        }
        // a whole image: +2 EV leaves every pixel at or above −1 EV bit-identical
        let (w, h) = (200usize, 100usize);
        let data: Vec<f32> = (0..w * h).flat_map(|k| {
            let v = (-9.0 + 9.5 * ((k % w) as f32 / w as f32) + 0.3 * ((k / w) as f32 / 7.0).sin()).exp2();
            [v, v * 0.8, v * 1.1]
        }).collect();
        let t = ToneEq { amount: 2.0, ..ToneEq::default() };
        let m = Mask::full(Arc::new(Grid::analyze(&data, w, h, 1.0, &t)));
        let a = Apply::new(&t, &m, 0.0, false);
        let mut changed = 0;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                let p = [data[i], data[i + 1], data[i + 2]];
                let g = a.gain(x, y, p);
                if m.pixel_ev(p) >= PROTECT_HIGHLIGHTS_EV {
                    assert_eq!(g, 1.0, "highlight pixel changed at {x},{y}");
                } else if g > 1.0 {
                    changed += 1;
                    assert!(brightness(p.map(|c| c * g)) <= 0.5f32.powf(1.0) + 1e-4);
                }
            }
        }
        assert!(changed > w * h / 3, "shadows lifted: {changed}");
    }

    /// Image with a dark half and a bright half: the mask is flat inside each half (smoothed texture) and keeps the
    /// edge between them (no halo: the dark side next to the edge stays dark in the mask).
    #[test]
    fn mask_keeps_edges() {
        let (w, h) = (400usize, 200usize);
        let mut data = vec![0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                // dark side −6 EV, bright side −1 EV, with ±0.3 EV texture
                let base: f32 = if x < w / 2 { -6.0 } else { -1.0 };
                let tex = if (x / 3 + y / 3) % 2 == 0 { 0.3 } else { -0.3 };
                let v = (base + tex).exp2();
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v, v, v]);
            }
        }
        let t = ToneEq { edges: 0.7, ..ToneEq::default() };
        let m = Mask::full(Arc::new(Grid::analyze(&data, w, h, 1.0, &t)));
        let at = |x: usize| {
            let i = (100 * w + x) * 3;
            m.ev(x, 100, [data[i], data[i + 1], data[i + 2]])
        };
        // inside the surfaces: texture smoothed (±0.3 EV → much less), level kept
        let dark: Vec<f32> = (40..60).map(at).collect();
        let bright: Vec<f32> = (340..360).map(at).collect();
        let spread = |v: &[f32]| v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread(&dark) < 0.4 && spread(&bright) < 0.4, "texture {} {}", spread(&dark), spread(&bright));
        assert!((dark.iter().sum::<f32>() / 20.0 + 6.0).abs() < 0.3);
        assert!((bright.iter().sum::<f32>() / 20.0 + 1.0).abs() < 0.3);
        // a few pixels from the edge each side keeps its own level (within 1 EV)
        assert!(at(w / 2 - 4) < -5.0, "dark next to the edge: {}", at(w / 2 - 4));
        assert!(at(w / 2 + 4) > -2.0, "bright next to the edge: {}", at(w / 2 + 4));
    }

    /// Crops of the mask see the same values as the whole image, and a mask analysed at a quarter of the size gives
    /// nearly the same values at full size.
    #[test]
    fn crops_and_scales_agree() {
        let (w, h) = (320usize, 240usize);
        let data: Vec<f32> = (0..w * h).flat_map(|k| {
            let (x, y) = ((k % w) as f32, (k / w) as f32);
            let v = (-7.0 + 6.0 * (x / w as f32) + 0.5 * (y / 20.0).sin()).exp2();
            [v, v * 0.9, v * 1.1]
        }).collect();
        let t = ToneEq::default();
        let full = Mask::full(Arc::new(Grid::analyze(&data, w, h, 1.0, &t)));
        let crop = full.crop(100, 50, 80, 60);
        let px = |x: usize, y: usize| { let i = (y * w + x) * 3; [data[i], data[i + 1], data[i + 2]] };
        for (x, y) in [(0, 0), (10, 20), (79, 59)] {
            assert_eq!(crop.ev(x, y, px(100 + x, 50 + y)), full.ev(100 + x, 50 + y, px(100 + x, 50 + y)));
        }
        // quarter-size analysis (block means), applied at full size
        let (sw, sh) = (w / 4, h / 4);
        let small: Vec<f32> = (0..sw * sh).flat_map(|k| {
            let (x, y) = (k % sw, k / sw);
            let mut s = [0f32; 3];
            for yy in y * 4..y * 4 + 4 { for xx in x * 4..x * 4 + 4 { let p = px(xx, yy); for c in 0..3 { s[c] += p[c] / 16.0; } } }
            s
        }).collect();
        let grid = Arc::new(Grid::analyze(&small, sw, sh, 1.0, &t));
        let up = Mask::resample(grid, w, h, [0, 0, w, h]);
        let mut worst = 0f32;
        for y in (10..h - 10).step_by(17) {
            for x in (10..w - 10).step_by(13) {
                worst = worst.max((up.ev(x, y, px(x, y)) - full.ev(x, y, px(x, y))).abs());
            }
        }
        assert!(worst < 0.35, "scale difference {worst} EV");
    }
}
