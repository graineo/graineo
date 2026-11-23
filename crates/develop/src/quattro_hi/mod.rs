//! Quattro from capture ISO 400 on: the high-ISO chain on the stored sensor values.
//!
//! The chain follows the behaviour of the camera maker's raw converter, stage by stage, so that noise, colour and
//! detail of the developed picture match its output:
//!
//! 1. [`lattice`] and [`outliers`]: values the sensor cannot be trusted with (the highlight-pixel lattice of the
//!    dp Quattro, single outliers of the top layer and of the colour grid) are replaced from their neighbours.
//! 2. [`grid_denoise`]: colour noise reduction of the colour grid (bottom and middle layer at their resolution,
//!    the top layer averaged 2 × 2) on two scales.
//! 3. [`tint`]: the colour stage on the grid (highlights towards neutral, matrix colour blended with the camera
//!    colour by brightness, hue and saturation, chroma outliers clamped).
//! 4. [`upscale`]: back to the full resolution of the top layer, with its brightness noise smoothed.
//! 5. [`polish`]: local contrast and sharpening.
//!
//! [`pool`] forms the block means of the binned output, [`gpu`] runs the large range filter of step 2 on the
//! graphics card with the same arithmetic.
//!
//! Units throughout: sensor values above black divided by SensorISO/100, before the white balance, unless a step
//! says otherwise.

pub mod gpu;
pub mod grid_denoise;
pub mod lattice;
pub mod outliers;
pub mod polish;
pub mod pool;
pub mod tint;
pub mod upscale;

mod curve;

pub use crate::quattro_nr::Px;

/// Noise variance of a full-resolution top-layer value `v`: `[0] + [1]·|v| + [2]·v²`.
pub const TOP_MODEL: [f64; 3] = [20.843, 0.4342, 8e-7];

/// Noise variance of the colour grid for the outlier steps, per layer (bottom, middle, top mean) as [`TOP_MODEL`].
pub const GRID_MODEL: [[f32; 3]; 3] = [[24.362, 0.04835, -1.25e-7], [15.16425, 0.0275, -5e-8], [1.3026875, 0.0271375, 5e-8]];

/// White of the top layer for the brightness-dependent shares: compact cameras (dp Quattro) and system cameras
/// (sd Quattro, sd Quattro H).
pub const WHITE_COMPACT: f32 = 13000.0;
pub const WHITE_SYSTEM: f32 = 9000.0;

/// Print the time of every step (`GRAINEO_QUATTRO_TIMING=1`)?
pub fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("GRAINEO_QUATTRO_TIMING").is_ok_and(|v| v == "1"))
}

/// Stopwatch for [`timing`]: every [`Clock::lap`] prints the time since the previous one.
pub struct Clock(std::time::Instant);

impl Clock {
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }

    pub fn lap(&mut self, step: &str) {
        if timing() {
            eprintln!("quattro timing: {step:<40} {:>8.1} ms   (largest memory so far {:.2} GB)", self.0.elapsed().as_secs_f64() * 1e3, peak_memory() as f64 / 1e9);
        }
        self.0 = std::time::Instant::now();
    }
}

/// Largest resident memory of the process so far in bytes (0 where the system does not tell).
fn peak_memory() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: getrusage fills the zeroed structure it is given
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
            // bytes on macOS, kilobytes elsewhere
            return usage.ru_maxrss as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 };
        }
    }
    0
}

/// Value of a developer switch of the Quattro development (measurements, comparisons). Such switches change
/// results without being part of the disk cache key, so they only act in a test environment with its own data
/// directory (`GRAINEO_DATA_DIR`); elsewhere they read as not set.
pub fn developer_switch(name: &str) -> Result<String, std::env::VarError> {
    if std::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return Err(std::env::VarError::NotPresent);
    }
    std::env::var(name)
}

/// Replacement of the highlight-pixel lattice (`GRAINEO_QUATTRO_FULL_HP=0`: off, `cells`: calibrated own values;
/// part of the disk cache key, see [`cache_tag`]).
pub fn lattice_replacement() -> lattice::Replacement {
    match std::env::var("GRAINEO_QUATTRO_FULL_HP").as_deref() {
        Ok("0") => lattice::Replacement::Off,
        Ok("cells") => lattice::Replacement::Calibrated,
        _ => lattice::Replacement::Neighbours,
    }
}

/// Mark of the chain's switches for the disk cache key: empty with the defaults.
pub fn cache_tag() -> String {
    match lattice_replacement() {
        lattice::Replacement::Neighbours => String::new(),
        other => format!("quattro_lattice={other:?}\n"),
    }
}

/// Is the capture a long exposure for the outlier steps? Shutter time in ms (at least 200) times the doubling of
/// the dark current per 8 °C sensor temperature above 64 000: single bright points may be stars then.
pub fn long_exposure(shutter_us: f64, temperature: f64) -> bool {
    (shutter_us / 1000.0).max(200.0) * (std::f64::consts::LN_2 / 8.0 * temperature).exp() > 64_000.0
}

/// Outlier repair of the stored values (step 1): the colour grid, then dark pairs and single outliers of the top
/// layer. `view`: colour of a grid cell for the grid step (skipped without). Returns the numbers of replaced grid
/// cells, pairs and single top-layer pixels.
pub fn repair_outliers(grid: &mut [Px], top: &mut [f32], gw: usize, gh: usize, view: Option<&outliers::ColourView>) -> (usize, usize, usize) {
    let mut clock = Clock::start();
    let cells = view.map_or(0, |v| outliers::grid(grid, gw, gh, v));
    clock.lap("outliers: grid");
    let pairs = outliers::top_pairs(top, 2 * gw, 2 * gh);
    clock.lap("outliers: top-layer pairs");
    let singles = outliers::top_singles(top, 2 * gw, 2 * gh);
    clock.lap("outliers: top-layer singles");
    (cells, pairs, singles)
}

/// Inputs of the chain for one photo, kept with the prepared file: everything up to the first stage at the camera's
/// colour step. The slider steps and the colour mode act in [`Inputs::develop`].
pub struct Inputs {
    /// colour grid before the first stage (outliers repaired), `gw × gh`
    pub plain: Vec<Px>,
    /// the grid after the first stage at the camera's colour step
    pub denoised: Vec<Px>,
    pub gw: usize,
    pub gh: usize,
    /// top layer at full resolution (outliers repaired), `2gw × 2gh`
    pub top: Vec<f32>,
    /// settings of the first stage (the colour slider changes the step)
    pub stage: grid_denoise::GridDenoise,
    /// normalised layer value → units of the chain
    pub unit: [f32; 3],
    pub capture_iso: f64,
}

/// Colour step of the camera's setting (slider middle).
pub const CAMERA_STEP: usize = 2;
/// Detail step of the camera's setting (the middle of seven steps, 0 = softest … 6 = strongest).
const CAMERA_DETAIL: usize = 3;

/// Detail step 0 … 6 of the chain for the slider position `detail` (−3 … 3 around the camera's setting).
fn detail_step(detail: i8) -> usize {
    (CAMERA_DETAIL as i8 + detail.clamp(-3, 3)) as usize
}

impl Inputs {
    /// From the repaired stored values: `grid` (`gw × gh`) and `top` (`2gw × 2gh`). The highlight-pixel lattice is
    /// replaced here (`lattice`: its positions, `None` for cameras without). `gains`: daylight white balance on the
    /// grid values, `score`: dark-current score of the exposure ([`grid_denoise::night`]).
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(grid: &[Px], top: &[f32], gw: usize, gh: usize, lattice: Option<&lattice::Lattice>, gains: [f64; 3], capture_iso: f64, score: f32, unit: [f32; 3]) -> Self {
        let mut clock = Clock::start();
        let (mut plain, mut top) = (grid.to_vec(), top.to_vec());
        if let Some(l) = lattice {
            let fill = lattice_replacement() == lattice::Replacement::Neighbours;
            if fill {
                lattice::fill_top(&mut top, gw, gh, l);
            }
            // the grid's top value of a lattice cell follows its four top-layer pixels
            let tw = 2 * gw;
            for (i, p) in plain.iter_mut().enumerate() {
                let (x, y) = (i % gw, i / gw);
                if l.contains(x, y) {
                    let t = 2 * y * tw + 2 * x;
                    p[2] = 0.25 * (top[t] + top[t + 1] + top[t + tw] + top[t + tw + 1]);
                }
            }
            if fill {
                lattice::fill_grid(&mut plain, gw, gh, l);
            }
        }
        let stage = grid_denoise::GridDenoise { gains, step: CAMERA_STEP, variance: 1.0, night: grid_denoise::night(capture_iso, score) };
        clock.lap("inputs: lattice");
        let denoised = grid_denoise::denoise(&plain, gw, gh, &stage);
        Self { plain, denoised, gw, gh, top, stage, unit, capture_iso }
    }

    /// The developed picture for the look `look` and the slider steps `luma` and `chroma` (−1 = off, 2 = camera
    /// setting, up to 4) and the detail step `detail` (−3 = soft … 3 = strong, 0 = camera setting), as means over
    /// `block × block` pixels when `block > 1`.
    pub fn develop(&self, look: &Look, luma: i8, chroma: i8, detail: i8, block: usize) -> Developed {
        let detail = detail_step(detail);
        let mut clock = Clock::start();
        let pooled = (block > 1).then(|| pool::pool(&self.plain, &self.top, self.gw, self.gh, block));
        let (gw, gh, plain, top): (usize, usize, &[Px], &[f32]) = match &pooled {
            Some(p) => (p.gw, p.gh, &p.grid, &p.top),
            None => (self.gw, self.gh, &self.plain, &self.top),
        };
        clock.lap("develop: block means");
        let again;
        let denoised: &[Px] = if chroma < 0 {
            plain
        } else if chroma as usize == self.stage.step && pooled.is_none() {
            &self.denoised
        } else {
            let stage = grid_denoise::GridDenoise { step: chroma as usize, variance: if pooled.is_some() { 0.25 } else { 1.0 }, ..self.stage };
            again = grid_denoise::denoise(plain, gw, gh, &stage);
            &again
        };
        let iso_factor = (self.capture_iso / 100.0) as f32;
        let setup = tint::Tint { matrix: look.matrix, gains: look.gains, sensor: look.sensor, iso_factor, highlights: 0.0, compact: look.compact, pooled: pooled.is_some() };
        let mut colour = tint::tint(denoised, gw, gh, &setup);
        if let Some(fine) = look.fine {
            use rayon::prelude::*;
            colour.par_iter_mut().for_each(|p| fine.apply(p));
        }
        clock.lap("develop: colour stage");
        if let Ok(dir) = developer_switch("GRAINEO_QUATTRO_HI_DUMP") {
            dump(&format!("{dir}/stage1.bin"), gw, gh, plain.iter().chain(denoised).flatten().chain(top));
            dump(&format!("{dir}/raw-color.bin"), gw, gh, colour.iter().flatten());
        }
        let settings = upscale::Upscale { iso_factor, luma_step: luma.max(0) as usize, detail_step: detail, white: if look.compact { WHITE_COMPACT } else { WHITE_SYSTEM }, pooled: pooled.is_some() };
        let box_contrast = iso_factor >= 8.0;
        let up = upscale::upscale(denoised, plain, &colour, gw, gh, top, &settings);
        clock.lap("develop: upscale");
        let (width, height) = (2 * gw, 2 * gh);
        let mut rgb = up.rgb;
        if box_contrast {
            polish::local_contrast(&mut rgb, width, height, detail, iso_factor, pooled.is_some());
        } else {
            // below ISO factor 8 the local contrast works on L* over several scales
            crate::quattro_lce::apply_step(&mut rgb, width, height, look.portrait, 1.0, detail);
        }
        clock.lap("develop: local contrast");
        polish::sharpen(&mut rgb, &up.detail, detail, iso_factor, 0.0, pooled.is_some());
        clock.lap("develop: sharpening");
        if let Ok(dir) = developer_switch("GRAINEO_QUATTRO_HI_DUMP") {
            dump(&format!("{dir}/detail.bin"), width, height, up.detail.iter());
            dump(&format!("{dir}/sharpen.bin"), width, height, rgb.iter());
        }
        // binned: only the part with image content (the pooled grid may carry repeated columns and rows)
        if let Some((kw, kh)) = pooled.as_ref().map(|p| p.keep).filter(|k| *k != (width, height)) {
            let mut cut = Vec::with_capacity(kw * kh * 3);
            for row in rgb.chunks_exact(width * 3).take(kh) {
                cut.extend_from_slice(&row[..kw * 3]);
            }
            return Developed { rgb: cut, width: kw, height: kh };
        }
        Developed { rgb, width, height }
    }
}

/// The colour rendering of [`Inputs::develop`].
pub struct Look<'a> {
    /// white-balanced camera values → output colour (white balance and colour mode)
    pub matrix: [f64; 9],
    /// white balance on the grid values, middle layer = 1
    pub gains: [f32; 3],
    /// SensorISO / 100
    pub sensor: f32,
    /// compact camera (dp Quattro) rather than a system camera
    pub compact: bool,
    /// fine correction of the colour mode by hue (system cameras)
    pub fine: Option<&'a crate::quattro_color::Axes>,
    /// the colour mode is the portrait mode
    pub portrait: bool,
}

/// Result of [`Inputs::develop`]: interleaved RGB in the 16-bit value range of the colour stage.
pub struct Developed {
    pub rgb: Vec<f32>,
    pub width: usize,
    pub height: usize,
}

/// Development aid ([`developer_switch`] `GRAINEO_QUATTRO_HI_DUMP=<directory>`): intermediate images as width,
/// height (u32) and f32 values.
fn dump<'a>(path: &str, w: usize, h: usize, values: impl Iterator<Item = &'a f32>) {
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&(w as u32).to_le_bytes());
    bytes.extend_from_slice(&(h as u32).to_le_bytes());
    for v in values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Err(e) = std::fs::write(path, bytes) {
        eprintln!("Quattro high-ISO chain: {path}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproducible noise in −1 … 1.
    pub(super) fn noise(i: usize) -> f32 {
        let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
        s ^= s >> 29;
        s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        s ^= s >> 32;
        (s & 0xFFFF) as f32 / 32768.0 - 1.0
    }

    /// A small scene: a brightness ramp with a coloured square, an edge and noise on all layers.
    fn scene(gw: usize, gh: usize) -> (Vec<Px>, Vec<f32>) {
        let level = |x: usize, y: usize| 150.0 + 6.0 * x as f32 + if y > gh / 2 { 900.0 } else { 0.0 };
        let top: Vec<f32> = (0..4 * gw * gh).map(|i| level(i % (2 * gw) / 2, i / (2 * gw) / 2) + 25.0 * noise(7 * i + 1)).collect();
        let grid = (0..gw * gh)
            .map(|i| {
                let (x, y) = (i % gw, i / gw);
                let tint = if (20..40).contains(&x) && (10..30).contains(&y) { 1.4 } else { 1.0 };
                let t = 2 * y * 2 * gw + 2 * x;
                [level(x, y) * 1.2 * tint + 14.0 * noise(3 * i), level(x, y) * 1.1 + 11.0 * noise(3 * i + 1), 0.25 * (top[t] + top[t + 1] + top[t + 2 * gw] + top[t + 2 * gw + 1])]
            })
            .collect();
        (grid, top)
    }

    fn look() -> Look<'static> {
        Look { matrix: [1.9, -0.6, -0.3, -0.4, 1.7, -0.3, -0.1, -0.5, 1.6], gains: [1.1, 1.0, 1.3], sensor: 1.0, compact: true, fine: None, portrait: false }
    }

    #[test]
    fn chain_develops_every_step_and_block_size() {
        let (gw, gh) = (64, 48);
        let (grid, top) = scene(gw, gh);
        let inputs = Inputs::assemble(&grid, &top, gw, gh, None, [1.2, 1.0, 1.1], 1600.0, 0.0, [1.0; 3]);
        let reference = inputs.develop(&look(), 2, 2, 0, 1);
        assert_eq!((reference.width, reference.height, reference.rgb.len()), (128, 96, 128 * 96 * 3));
        assert!(reference.rgb.iter().all(|v| v.is_finite() && (0.0..=65535.0).contains(v)));
        // the mean brightness stays with the steps, the fine structure changes
        let mean = |d: &Developed| d.rgb.iter().sum::<f32>() / d.rgb.len() as f32;
        for (luma, chroma) in [(-1, -1), (0, 2), (4, 2), (2, 0), (2, 4)] {
            let d = inputs.develop(&look(), luma, chroma, 0, 1);
            assert!((mean(&d) / mean(&reference) - 1.0).abs() < 0.03, "steps {luma}/{chroma}: {} against {}", mean(&d), mean(&reference));
            assert!(d.rgb != reference.rgb, "steps {luma}/{chroma} change nothing");
        }
        let pooled = inputs.develop(&look(), 2, 2, 0, 2);
        assert_eq!((pooled.width, pooled.height), (64, 48));
        assert!((mean(&pooled) / mean(&reference) - 1.0).abs() < 0.05);
    }

    #[test]
    fn more_luminance_smoothing_leaves_less_fine_noise() {
        let (gw, gh) = (64, 48);
        let (grid, top) = scene(gw, gh);
        let inputs = Inputs::assemble(&grid, &top, gw, gh, None, [1.2, 1.0, 1.1], 1600.0, 0.0, [1.0; 3]);
        let roughness = |d: &Developed| {
            let g = |x: usize, y: usize| d.rgb[(y * d.width + x) * 3 + 1];
            (10..38usize).flat_map(|y| (10..118usize).map(move |x| (x, y))).map(|(x, y)| (g(x, y) - 0.25 * (g(x - 1, y) + g(x + 1, y) + g(x, y - 1) + g(x, y + 1))).abs()).sum::<f32>()
        };
        let (off, most) = (inputs.develop(&look(), -1, 2, 0, 1), inputs.develop(&look(), 4, 2, 0, 1));
        assert!(roughness(&most) < 0.8 * roughness(&off), "{} against {}", roughness(&most), roughness(&off));
    }

    /// Graphics card against processor on the range filter of the first stage: equal up to rounding. Skipped without a card.
    #[test]
    fn graphics_card_matches_processor() {
        if !gpu::available() {
            eprintln!("no graphics card: comparison skipped");
            return;
        }
        let (w, h) = (150, 97);
        let (grid, _) = scene(w, h);
        let close = |a: &[Px], b: &[Px], tolerance: f32, what: &str| {
            let worst = a.iter().zip(b).flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs())).fold(0f32, f32::max);
            assert!(worst <= tolerance, "{what}: largest difference {worst}");
        };
        // range filter with factors of a plausible size, both window sizes
        let factors: Vec<Px> = grid.iter().map(|p| [25.0 / (100.0 + 0.2 * p[0]), 25.0 / (60.0 + 0.1 * p[1]), 25.0 / (5.0 + 0.1 * p[2])]).collect();
        for (radius, floor) in [(5, 0.3), (15, 0.75)] {
            let curve = grid_denoise::weight_curve(floor);
            let on_card = gpu::range_filter(&grid, &factors, w, h, radius, &curve).expect("range filter on the card");
            close(&on_card, &grid_denoise::range_filter(&grid, &factors, w, h, radius, &curve), 0.02, "range filter");
        }
    }
}
