//! Tools for exploring the sensor: measuring spot statistics per layer, export of the
//! raw layers as linear 16-bit TIFFs, exposure headroom (ETTR).

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use x3f::RawImage;

use crate::{tiffw, Calibration};

/// Statistics of one layer in a measuring spot (sensor units after black subtraction).
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerStats {
    pub mean: f64,
    /// Standard deviation of all values (includes image structure)
    pub std: f64,
    /// Noise by the difference method: std. dev. of the difference of horizontal neighbours / √2;
    /// insensitive to brightness gradients within the spot
    pub noise: f64,
    pub min: f64,
    pub max: f64,
    /// Share of pixels near saturation
    pub clipped: f64,
}

impl LayerStats {
    /// Signal-to-noise ratio in dB (difference method)
    pub fn snr_db(&self) -> f64 {
        if self.noise > 0.0 && self.mean > 0.0 {
            20.0 * (self.mean / self.noise).log10()
        } else {
            f64::NAN
        }
    }
}

/// Statistics in the rectangle `[x, y, w, h]` (coordinates in the active image area) directly on the
/// raw data, without spatial gain, noise reduction or scaling.
pub fn region_stats(raw: &RawImage, cal: &Calibration, rect: [usize; 4]) -> [LayerStats; 3] {
    let [x0, y0, ..] = cal.active;
    let [x, y, w, h] = rect;
    let (cols, rows) = (raw.columns, raw.rows);
    let sat_raw = cal.pixel_saturation * 0.995;
    std::array::from_fn(|c| {
        let plane = &raw.planes[c];
        let (mut n, mut sum, mut sum2, mut min, mut max, mut clip) = (0.0f64, 0.0, 0.0, f64::MAX, f64::MIN, 0.0);
        let (mut nd, mut dsum, mut dsum2) = (0.0f64, 0.0, 0.0);
        for yy in y0 + y..(y0 + y + h).min(rows) {
            let mut prev: Option<f64> = None;
            for xx in x0 + x..(x0 + x + w).min(cols) {
                let r = plane[yy * cols + xx] as f64;
                let v = r - cal.black[c];
                n += 1.0;
                sum += v;
                sum2 += v * v;
                min = min.min(v);
                max = max.max(v);
                if r >= sat_raw {
                    clip += 1.0;
                }
                if let Some(p) = prev {
                    let d = v - p;
                    nd += 1.0;
                    dsum += d;
                    dsum2 += d * d;
                }
                prev = Some(v);
            }
        }
        if n == 0.0 {
            return LayerStats::default();
        }
        let mean = sum / n;
        let std = (sum2 / n - mean * mean).max(0.0).sqrt();
        let noise = if nd > 1.0 { ((dsum2 / nd - (dsum / nd).powi(2)).max(0.0) / 2.0).sqrt() } else { 0.0 };
        LayerStats { mean, std, noise, min, max, clipped: clip / n }
    })
}

/// Exposure headroom per layer in stops: distance of the 99.9 % percentile to
/// saturation (from a linear histogram over 0 … saturation). Negative = blown out.
pub fn ettr_headroom(hist: &[Vec<u32>; 3]) -> [f64; 3] {
    std::array::from_fn(|c| {
        let h = &hist[c];
        let total: u64 = h.iter().map(|&v| v as u64).sum();
        if total == 0 {
            return f64::NAN;
        }
        let bins = h.len();
        if h[bins - 1] as f64 / total as f64 > 0.001 {
            return 0.0;
        }
        let mut acc = 0u64;
        let limit = (total as f64 * 0.999) as u64;
        for (i, &v) in h.iter().enumerate() {
            acc += v as u64;
            if acc >= limit {
                let level = (i as f64 + 0.5) / (bins - 1) as f64;
                return -level.max(1e-4).log2();
            }
        }
        0.0
    })
}

/// Writes the three layers as linear 16-bit greyscale TIFFs (active area,
/// black level subtracted, stretched ×16 to 16 bits; optionally with spatial gain/colour shading).
/// Returns: files written (bottom, middle, top).
pub fn export_layers(raw: &RawImage, cal: &Calibration, dir: &Path, stem: &str, flat_field: bool) -> std::io::Result<Vec<PathBuf>> {
    let lin = flat_field.then(|| crate::linearize(raw, cal));
    let [x0, y0, x1, y1] = cal.active;
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let names = ["1-unten", "2-mitte", "3-oben"];
    let mut out = Vec::new();
    for c in 0..3 {
        let mut bytes = vec![0u8; w * h * 2];
        bytes.par_chunks_mut(w * 2).enumerate().for_each(|(y, line)| {
            for x in 0..w {
                let v = match &lin {
                    Some(l) => l.data[(y * w + x) * 3 + c] as f64 * cal.saturation,
                    None => raw.planes[c][(y0 + y) * raw.columns + x0 + x] as f64 - cal.black[c],
                };
                let q = (v * 16.0).round().clamp(0.0, 65535.0) as u16;
                line[x * 2..x * 2 + 2].copy_from_slice(&q.to_le_bytes());
            }
        });
        use tiffw::Value::*;
        let tags = vec![
            (256, Long(vec![w as u32])),
            (257, Long(vec![h as u32])),
            (258, Short(vec![16])),
            (259, Short(vec![1])),
            (262, Short(vec![1])),
            (274, Short(vec![1])),
            (277, Short(vec![1])),
            (278, Long(vec![h as u32])),
            (284, Short(vec![1])),
            (305, Ascii(format!("graineo – Foveon-Schicht {} (linear{})", names[c], if flat_field { ", flat-field" } else { ", roh" }))),
        ];
        let path = dir.join(format!("{stem}_{}.tif", names[c]));
        tiffw::write(std::io::BufWriter::new(std::fs::File::create(&path)?), tags, &bytes)?;
        out.push(path);
    }
    Ok(out)
}
