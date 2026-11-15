//! **Sensor view**: analyses directly on the color filter mosaic of Bayer and X-Trans RAWs
//! (compute core of the "Sensor" view, without UI).
//!
//! Entry point: [`RawImage::open`] decodes the file (via `crates/raw`/rawler) and holds the
//! normalized mosaic; [`analyze`] returns an [`Outcome`] per [`Kind`] with image layers (RGBA) and
//! metrics as JSON.
//!
//! ```no_run
//! use sensor_view::{analyze, Kind, Options, RawImage};
//! let rb = RawImage::open("image.RAF").unwrap();
//! let e = analyze(&rb, Kind::Histogram, &Options::default()).unwrap();
//! println!("{}", e.metrics["headroom_ev"]);
//! for layer in &e.layers { layer.canvas.write_png(format!("{}.png", layer.name)).unwrap(); }
//! ```
//!
//! ## Coordinates
//! - **Sensor coordinates**: the full raw image from rawler (including masked borders), origin top left,
//!   sensor orientation (EXIF rotation *not* applied). The color pattern is relative to this
//!   origin (`pattern[y % 6][x % 6]`).
//! - **Image coordinates**: the crop (`RawImage::crop`, DefaultCrop or active area),
//!   i.e. exactly the image the development (`raw::RawFile::develop`) returns – also without
//!   EXIF rotation. [`Options::detail`] and all positions in the metrics (`x`, `y`) are
//!   image coordinates; sensor coordinates are given additionally as `sx`, `sy`.
//! - **Maps** (overlays over the whole image) cover the crop and are downscaled to the long
//!   edge [`Options::size`]; [`Layer::reference`] names the covered area.
//!
//! ## Values
//! "Normalized" means: (raw value − black per pattern position) / (white − black), without white balance.
//! EV values refer to the measured saturation of the respective channel.
//!
//! ## Cost
//! Fast (< 150 ms on 24 MP after decoding): [`Kind::Histogram`], [`Kind::Mosaic`],
//! [`Kind::Sharpness`], [`Kind::Internals`]. Expensive (cached per options):
//! [`Kind::DemosaicComparison`] (whole image), [`Kind::Noise`], [`Kind::Health`], [`Kind::Lens`].
//!
//! Origin of the methods: noise model from `raw::noise` (ideas from darktable "denoise profiled",
//! GPL-3); Tenengrad (Krotkov 1987) and Laplacian for sharpness; false color via
//! high-frequency chroma energy (idea of the "false color suppression" masks in RawTherapee/darktable,
//! own formula); all our own implementation.

pub mod canvas;
pub mod sheet;
mod demosaic;
mod health;
mod histogram;
mod internals;
mod mosaic;
mod lens;
mod noise;
mod sharpness;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde_json::{json, Value};

pub use canvas::Canvas;
pub use raw::{Region, Sensor};

/// Error with plain-text message.
#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<raw::Error> for Error {
    fn from(e: raw::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Kind of analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// RAW histogram per filter class (linear and EV), headroom, clipping mask,
    /// comparison with the rendered image (camera JPEG)
    Histogram,
    /// Raw values in filter color, nearest neighbor, for a detail area (400–1600 %)
    Mosaic,
    /// the same detail area with RCD/Markesteijn/bilinear, false color/moiré warning map
    DemosaicComparison,
    /// Noise map per channel, usable dynamic range, SNR curve
    Noise,
    /// Hot/dead/stuck pixels, black level, optical black border, banding
    Health,
    /// Vignetting (profile and estimate from the image), distortion as a grid
    Lens,
    /// Focus/sharpness map from the green pixels, focus peaking mask
    Sharpness,
    /// Camera internals as data (WB, black/white, ISO, Fuji DR, picture style, pattern, bit depth)
    Internals,
}

impl Kind {
    pub const ALL: [Kind; 8] = [Kind::Internals, Kind::Histogram, Kind::Mosaic, Kind::DemosaicComparison, Kind::Noise, Kind::Health, Kind::Lens, Kind::Sharpness];

    /// Short name (for file names and the command line).
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Histogram => "histogram",
            Kind::Mosaic => "mosaic",
            Kind::DemosaicComparison => "demosaic",
            Kind::Noise => "noise",
            Kind::Health => "health",
            Kind::Lens => "lens",
            Kind::Sharpness => "sharpness",
            Kind::Internals => "internals",
        }
    }

    pub fn from_name(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|a| a.name() == s.to_ascii_lowercase())
    }

    /// Expensive (gets cached)?
    pub fn expensive(&self) -> bool {
        matches!(self, Kind::DemosaicComparison | Kind::Noise | Kind::Health | Kind::Lens)
    }
}

/// Analysis settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// long edge of the maps (overlays over the whole image) in pixels
    pub size: usize,
    /// Detail area in image coordinates for mosaic and demosaic comparison; None = image center
    pub detail: Option<Region>,
    /// Magnification for the mosaic/demosaic detail (4 = 400 % … 16 = 1600 %)
    pub zoom: usize,
    /// Draw pixel grid and pattern tiles in the mosaic
    pub grid: bool,
    /// Histogram: compare with the embedded camera preview (decodes the JPEG, ~50–150 ms)
    pub render_comparison: bool,
    /// Demosaic comparison: additionally the warning map over the whole image (expensive)
    pub full_image: bool,
    /// Focus peaking: threshold relative to the 99.9 % value of the edge energy (0…1)
    pub peaking_threshold: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self { size: 1024, detail: None, zoom: 8, grid: true, render_comparison: true, full_image: true, peaking_threshold: 0.35 }
    }
}

/// Role of an image layer for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// opaque image (e.g. mosaic, demosaic detail, preview)
    View,
    /// partly transparent layer over the image or detail area (`reference`)
    Overlay,
    /// chart (histogram, SNR curve) – standalone, not over the image
    Chart,
}

/// One image layer of a result.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Short name, e.g. `clipping`, `mosaic`, `rcd`, `falschfarben`
    pub name: String,
    pub role: Role,
    /// covered area in image coordinates (None for charts)
    pub reference: Option<Region>,
    pub canvas: Canvas,
}

/// Result of an analysis.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub kind: Kind,
    pub layers: Vec<Layer>,
    pub metrics: Value,
    /// Compute time (for cache hits, that of the first computation)
    pub duration: Duration,
    /// from the cache
    pub cached: bool,
}

impl Outcome {
    /// First layer (main image) – convenience for simple callers.
    pub fn canvas(&self) -> Option<&Canvas> {
        self.layers.first().map(|e| &e.canvas)
    }

    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|e| e.name == name)
    }
}

/// Filter classes for statistics: Bayer R, G1 (Gr, green in the red row), G2 (Gb), B;
/// X-Trans R, G, B.
#[derive(Debug, Clone)]
pub struct FilterClasses {
    pub names: Vec<&'static str>,
    /// Color (0/1/2) per class
    pub color: Vec<usize>,
    /// Class per pattern position
    pub class_at: [[u8; 6]; 6],
}

/// Opened RAW file with normalized mosaic (see module header for coordinates).
pub struct RawImage {
    pub file: Option<PathBuf>,
    /// Metadata (None for synthetic test images)
    pub info: Option<raw::Info>,
    raw: Option<Mutex<raw::RawFile>>,
    /// full sensor image
    pub width: usize,
    pub height: usize,
    /// normalized values (without white balance), `width`×`height`
    pub norm: Vec<f32>,
    /// 6×6 color pattern relative to the sensor origin (Bayer repeats within it)
    pub pattern: [[u8; 6]; 6],
    /// 2 (Bayer) or 6 (X-Trans)
    pub period: usize,
    pub sensor: Sensor,
    /// Image area (crop) on the sensor
    pub crop: Region,
    /// Active area on the sensor
    pub active: Region,
    /// White balance as shot (G = 1)
    pub wb: [f32; 3],
    /// White point in raw values (for normalization)
    pub white_dn: f32,
    /// Black level per color (average over the pattern) in raw values
    pub black_dn: [f32; 3],
    /// Optical black borders according to rawler (sensor coordinates)
    pub black_margins: Vec<Region>,
    pub classes: FilterClasses,
    /// Duration of decoding + normalizing
    pub decode_time: Duration,
    cache: Mutex<HashMap<String, Outcome>>,
    histo: OnceLock<histogram::Histo16>,
    saturation: OnceLock<Saturation>,
    noise_model: OnceLock<Option<raw::noise::NoiseModel>>,
    preview: Mutex<HashMap<usize, Canvas>>,
    /// Camera JPEG "blown out" per map size (decoding costs 50–200 ms)
    pub(crate) jpeg_masks: Mutex<HashMap<(usize, usize), JpegMask>>,
}

/// Blown-out mask of the camera JPEG at map size: Ok((mask, JPEG size)) or error text.
pub(crate) type JpegMask = std::result::Result<(Vec<bool>, (usize, usize)), String>;

/// Measured saturation per class (normalized).
#[derive(Debug, Clone)]
pub struct Saturation {
    /// Saturation level (maximum if values pile up there, otherwise nominally 1)
    pub level: Vec<f32>,
    /// from here on a value counts as clipped
    pub threshold: Vec<f32>,
    /// Pile-up at the maximum found (real clipping)
    pub measured: Vec<bool>,
}

impl RawImage {
    /// Open the file, decode it and normalize the mosaic. Bayer and X-Trans only.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let t = Instant::now();
        let rf = raw::RawFile::open(path)?;
        let info = rf.info.clone();
        if !matches!(info.sensor, Sensor::Bayer | Sensor::XTrans) {
            return Err(Error(format!("Sensor view needs a Bayer or X-Trans mosaic (file: {:?})", info.sensor)));
        }
        if info.fuji_rotated {
            return Err(Error("Fuji SuperCCD (gedrehter Sensor) wird nicht unterstützt".into()));
        }
        let (norm, pattern) = rf.mosaic_normalized()?;
        let (w, h) = (info.width, info.height);
        let active = crop_to(info.active.unwrap_or(Region { x: 0, y: 0, w, h }), w, h);
        let crop = crop_to(info.crop.or(info.active).unwrap_or(Region { x: 0, y: 0, w, h }), w, h);
        let mut black = [0f32; 3];
        let mut n = [0f32; 3];
        let (bw, bh, bc) = info.black_pattern;
        let (bw, bh, bc) = (bw.max(1), bh.max(1), bc.max(1));
        for y in 0..6 {
            for x in 0..6 {
                let c = pattern[y][x] as usize;
                let b = if info.black.is_empty() { 0.0 } else { info.black.get(((y % bh) * bw + x % bw) * bc).copied().unwrap_or(info.black[0]) };
                black[c] += b;
                n[c] += 1.0;
            }
        }
        let black_dn = std::array::from_fn(|c| black[c] / n[c].max(1.0));
        let black_margins = rf.black_margins();
        let mut rb = Self::assemble(norm, w, h, pattern, info.sensor, crop, active, info.wb.unwrap_or([1.0; 3]));
        rb.white_dn = info.white.first().copied().unwrap_or(65535.0);
        rb.black_dn = black_dn;
        rb.black_margins = black_margins;
        rb.file = Some(path.to_path_buf());
        rb.info = Some(info);
        rb.raw = Some(Mutex::new(rf));
        rb.decode_time = t.elapsed();
        Ok(rb)
    }

    /// Synthetic mosaic (tests, simulation): `norm` normalized, `pattern` relative to the origin.
    pub fn from_mosaic(norm: Vec<f32>, width: usize, height: usize, pattern: [[u8; 6]; 6], sensor: Sensor, wb: [f32; 3]) -> Self {
        let b = Region { x: 0, y: 0, w: width, h: height };
        Self::assemble(norm, width, height, pattern, sensor, b, b, wb)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(norm: Vec<f32>, width: usize, height: usize, pattern: [[u8; 6]; 6], sensor: Sensor, crop: Region, active: Region, wb: [f32; 3]) -> Self {
        let period = if sensor == Sensor::Bayer { 2 } else { 6 };
        let classes = determine_classes(&pattern, sensor);
        Self {
            file: None,
            info: None,
            raw: None,
            width,
            height,
            norm,
            pattern,
            period,
            sensor,
            crop,
            active,
            wb,
            white_dn: 16383.0,
            black_dn: [0.0; 3],
            black_margins: Vec::new(),
            classes,
            decode_time: Duration::ZERO,
            cache: Mutex::new(HashMap::new()),
            histo: OnceLock::new(),
            saturation: OnceLock::new(),
            noise_model: OnceLock::new(),
            preview: Mutex::new(HashMap::new()),
            jpeg_masks: Mutex::new(HashMap::new()),
        }
    }

    /// Filter color (0 = R, 1 = G, 2 = B) at sensor position (x, y).
    #[inline]
    pub fn color(&self, x: usize, y: usize) -> usize {
        self.pattern[y % 6][x % 6] as usize
    }

    /// Filter class at sensor position (see [`FilterClasses`]).
    #[inline]
    pub fn class(&self, x: usize, y: usize) -> usize {
        self.classes.class_at[y % 6][x % 6] as usize
    }

    /// Precompute shared foundations (16-bit histogram, saturation, noise model) – e.g.
    /// in a background thread right after opening, so that the interactive analyses
    /// only compute their own part afterwards.
    pub fn prepare(&self) {
        let _ = self.saturation();
        let _ = self.noise_model();
    }

    /// Access to the rawler file (None for synthetic images).
    pub fn with_raw<T>(&self, f: impl FnOnce(&raw::RawFile) -> T) -> Option<T> {
        self.raw.as_ref().map(|m| f(&m.lock().unwrap_or_else(|e| e.into_inner())))
    }

    /// Normalized value at sensor position.
    #[inline]
    pub fn value(&self, x: usize, y: usize) -> f32 {
        self.norm[y * self.width + x]
    }

    /// Undo normalization using the actual black-level pattern at this sensor position.
    pub fn value_dn(&self, x: usize, y: usize) -> f32 {
        let black = self.info.as_ref().map(|i| {
            let (bw,bh,bc) = i.black_pattern;
            let (bw,bh,bc) = (bw.max(1),bh.max(1),bc.max(1));
            i.black.get(((y % bh) * bw + x % bw) * bc).copied().unwrap_or_else(|| i.black.first().copied().unwrap_or(0.0))
        }).unwrap_or(self.black_dn[self.color(x,y)]);
        self.value(x,y) * (self.white_dn-black) + black
    }

    /// 16-bit histogram per class over the crop (computed once).
    pub(crate) fn histo(&self) -> &histogram::Histo16 {
        self.histo.get_or_init(|| histogram::histo16(self))
    }

    /// Saturation per class (from the histogram: pile-up at the maximum = real clipping).
    pub fn saturation(&self) -> &Saturation {
        self.saturation.get_or_init(|| histogram::determine_saturation(self, self.histo()))
    }

    /// Saturation per color (minimum over the classes of that color).
    pub fn saturation_color(&self) -> [f32; 3] {
        let s = self.saturation();
        let mut o = [f32::MAX; 3];
        for (k, &c) in self.classes.color.iter().enumerate() {
            o[c] = o[c].min(s.threshold[k]);
        }
        o.map(|v| if v == f32::MAX { 1.0 } else { v })
    }

    /// Poisson-Gaussian noise model (variance = a·v + b, normalized values without white balance; the same for all
    /// colors) from `raw::noise::estimate`, computed once.
    pub fn noise_model(&self) -> Option<raw::noise::NoiseModel> {
        *self.noise_model.get_or_init(|| {
            let s = self.saturation_color();
            let clip = s.map(|v| 0.98 * v);
            let m = self.pattern;
            let ca = move |x: usize, y: usize| m[y % 6][x % 6] as usize;
            raw::noise::estimate(&self.norm, self.width, self.height, &ca, clip, [1.0; 3])
        })
    }

    /// Exposure of the default development in EV (base + camera + BaselineExposure + Fuji DR).
    pub fn standard_ev(&self) -> f32 {
        match &self.info {
            Some(i) => raw::BASE_EV + raw::development::camera_ev(&i.maker, &i.model) + i.baseline_exposure + raw::development::fuji_compensation(&i.maker, &i.model, i.exif.iso, i.fuji_dr),
            None => raw::BASE_EV,
        }
    }

    /// Matrix camera → linear sRGB (identity without a camera matrix).
    pub fn camera_srgb(&self) -> [[f32; 3]; 3] {
        self.with_raw(|r| r.camera_to_srgb().ok()).flatten().unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
    }

    /// Fast preview of the crop (averaged per color in blocks, white balance, camera matrix,
    /// simple tone curve) with long edge `size` – basis for viewing the overlays.
    pub fn preview(&self, size: usize) -> Canvas {
        if let Some(b) = self.preview.lock().unwrap().get(&size) {
            return b.clone();
        }
        let k = GridMap::new_at(self.crop, size);
        let mean = self.block_mean(&k);
        let px: Vec<[f32; 3]> = mean.iter().map(|m| [m[0] * self.wb[0], m[1] * self.wb[1], m[2] * self.wb[2]]).collect();
        let lin = raw::Linear { width: k.mean, height: k.mh, px };
        let srgb = raw::camera_to_srgb(&lin, [1.0; 3], &self.camera_srgb(), true);
        let img = raw::tone_curve_srgb8(&srgb, self.standard_ev());
        let b = Canvas::from_rgb(k.mean, k.mh, img.as_raw());
        self.preview.lock().unwrap().insert(size, b.clone());
        b
    }

    /// Mean per color in the blocks of a map (normalized, without WB).
    pub(crate) fn block_mean(&self, k: &GridMap) -> Vec<[f32; 3]> {
        let mut out = vec![[0f32; 3]; k.mean * k.mh];
        out.par_chunks_mut(k.mean).enumerate().for_each(|(my, row)| {
            let (y0, y1) = k.rows(my);
            let mut s = vec![[0f32; 3]; k.mean];
            let mut n = vec![[0u32; 3]; k.mean];
            for y in y0..y1 {
                let zr = &self.norm[y * self.width..(y + 1) * self.width];
                let pr = &self.pattern[y % 6];
                for (i, x) in (k.x0..k.x0 + k.w).enumerate() {
                    let mx = k.xmap[i] as usize;
                    let c = pr[x % 6] as usize;
                    s[mx][c] += zr[x];
                    n[mx][c] += 1;
                }
            }
            for (mx, o) in row.iter_mut().enumerate() {
                *o = std::array::from_fn(|c| s[mx][c] / n[mx][c].max(1) as f32);
            }
        });
        out
    }

    /// Validate a detail area (image coordinates) or determine the default (image center, `w`×`h`).
    pub(crate) fn detail(&self, o: &Options, w: usize, h: usize) -> Region {
        let z = self.crop;
        let a = o.detail.unwrap_or(Region { x: z.w.saturating_sub(w) / 2, y: z.h.saturating_sub(h) / 2, w, h });
        let w = a.w.clamp(8, z.w);
        let h = a.h.clamp(8, z.h);
        Region { x: a.x.min(z.w - w), y: a.y.min(z.h - h), w, h }
    }
}

fn crop_to(r: Region, w: usize, h: usize) -> Region {
    let x = r.x.min(w);
    let y = r.y.min(h);
    Region { x, y, w: r.w.min(w - x), h: r.h.min(h - y) }
}

fn determine_classes(m: &[[u8; 6]; 6], sensor: Sensor) -> FilterClasses {
    let mut class_at = [[0u8; 6]; 6];
    if sensor == Sensor::Bayer {
        for y in 0..6 {
            // Red in this row? → green there is G1 (Gr)
            let red_row = (0..6).any(|x| m[y][x] == 0);
            for x in 0..6 {
                class_at[y][x] = match m[y][x] {
                    0 => 0,
                    1 if red_row => 1,
                    1 => 2,
                    _ => 3,
                };
            }
        }
        FilterClasses { names: vec!["R", "G1", "G2", "B"], color: vec![0, 1, 1, 2], class_at }
    } else {
        for y in 0..6 {
            for x in 0..6 {
                class_at[y][x] = m[y][x];
            }
        }
        FilterClasses { names: vec!["R", "G", "B"], color: vec![0, 1, 2], class_at }
    }
}

/// Mapping image (crop) → downscaled map.
#[derive(Debug, Clone)]
pub(crate) struct GridMap {
    pub x0: usize,
    pub y0: usize,
    pub w: usize,
    pub h: usize,
    pub mean: usize,
    pub mh: usize,
    /// Map column per image column
    pub xmap: Vec<u32>,
}

impl GridMap {
    pub fn new_at(r: Region, size: usize) -> Self {
        let f = (size.max(16) as f64 / r.w.max(r.h) as f64).min(1.0);
        let mean = ((r.w as f64 * f).round() as usize).max(1);
        let mh = ((r.h as f64 * f).round() as usize).max(1);
        let xmap = (0..r.w).map(|x| (x * mean / r.w) as u32).collect();
        Self { x0: r.x, y0: r.y, w: r.w, h: r.h, mean, mh, xmap }
    }

    /// Sensor rows [a, b) of map row `my`.
    pub fn rows(&self, my: usize) -> (usize, usize) {
        (self.y0 + (my * self.h).div_ceil(self.mh), self.y0 + ((my + 1) * self.h).div_ceil(self.mh))
    }

    /// Image area (reference) of the map in image coordinates.
    pub fn reference(&self, z: Region) -> Region {
        Region { x: self.x0 - z.x, y: self.y0 - z.y, w: self.w, h: self.h }
    }
}

/// Run an analysis. Whole-image analyses are cached; transient demosaic crops are not.
pub fn analyze(rb: &RawImage, kind: Kind, o: &Options) -> Result<Outcome> {
    let cache_key = format!("{kind:?}|{o:?}");
    let cacheable = kind.expensive() && !(kind == Kind::DemosaicComparison && !o.full_image);
    if cacheable {
        if let Some(e) = rb.cache.lock().unwrap().get(&cache_key) {
            let mut e = e.clone();
            e.cached = true;
            return Ok(e);
        }
    }
    let t = Instant::now();
    let (layers, mut metrics) = match kind {
        Kind::Histogram => histogram::analyze(rb, o)?,
        Kind::Mosaic => mosaic::analyze(rb, o)?,
        Kind::DemosaicComparison => demosaic::analyze(rb, o)?,
        Kind::Noise => noise::analyze(rb, o)?,
        Kind::Health => health::analyze(rb, o)?,
        Kind::Lens => lens::analyze(rb, o)?,
        Kind::Sharpness => sharpness::analyze(rb, o)?,
        Kind::Internals => internals::analyze(rb, o)?,
    };
    let duration = t.elapsed();
    if let Value::Object(m) = &mut metrics {
        m.insert("kind".into(), json!(kind.name()));
        m.insert("duration_ms".into(), json!(round_to(duration.as_secs_f64() * 1000.0, 1)));
    }
    let e = Outcome { kind, layers, metrics, duration, cached: false };
    if cacheable {
        rb.cache.lock().unwrap().insert(cache_key, e.clone());
    }
    Ok(e)
}

/// All analyses in sequence (for test output).
pub fn all(rb: &RawImage, o: &Options) -> Vec<(Kind, Result<Outcome>)> {
    Kind::ALL.into_iter().map(|a| (a, analyze(rb, a, o))).collect()
}

/// Round a number for JSON.
pub(crate) fn round_to(v: f64, digits: i32) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

pub(crate) fn region_json(b: Region) -> Value {
    json!({"x": b.x, "y": b.y, "w": b.w, "h": b.h})
}

/// Pattern as text (rows), starting at sensor position (x0, y0), `n`×`n`.
pub(crate) fn pattern_text(m: &[[u8; 6]; 6], x0: usize, y0: usize, n: usize) -> Vec<String> {
    (0..n).map(|y| (0..n).map(|x| ['R', 'G', 'B'][m[(y0 + y) % 6][(x0 + x) % 6] as usize]).collect()).collect()
}

/// Offsets of same-color neighbors (excluding the center) per pattern position within the window ±`r`.
pub(crate) fn same_color_neighbors(m: &[[u8; 6]; 6], r: isize) -> Vec<Vec<(isize, isize)>> {
    let mut out = Vec::with_capacity(36);
    for py in 0..6isize {
        for px in 0..6isize {
            let c = m[py as usize][px as usize];
            let mut v = Vec::new();
            for dy in -r..=r {
                for dx in -r..=r {
                    if (dx, dy) != (0, 0) && m[((py + dy + 12) % 6) as usize][((px + dx + 12) % 6) as usize] == c {
                        v.push((dx, dy));
                    }
                }
            }
            out.push(v);
        }
    }
    out
}

/// Percentile from an (unsorted) list.
pub(crate) fn percentile(v: &mut [f32], p: f32) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    let k = ((v.len() - 1) as f32 * p).round() as usize;
    let (_, x, _) = v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    *x
}
