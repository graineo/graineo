//! Resolution-independent local adjustments; CPU reference, no file access.
//! Linear RGB with sRGB/Rec.709 primaries.
mod correction;
mod filters;
pub mod gpu;
pub use correction::{apply, apply_view, margin, Adjustments};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    pub layers: Vec<Layer>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub mask: Mask,
    pub adjustments: Adjustments,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Add,
    Subtract,
    Intersect,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mask {
    Brush {
        points: Vec<[f32; 2]>,
        radius: f32,
        hardness: f32,
        flow: f32,
    },
    Linear {
        start: [f32; 2],
        end: [f32; 2],
    },
    Radial {
        center: [f32; 2],
        radii: [f32; 2],
        angle: f32,
        feather: f32,
    },
    Luminance {
        low: f32,
        high: f32,
        feather: f32,
    },
    Color {
        reference: [f32; 3],
        radius: f32,
        feather: f32,
    },
    Combine {
        operation: Operation,
        left: Box<Mask>,
        right: Box<Mask>,
    },
    Invert {
        mask: Box<Mask>,
    },
}
impl Document {
    pub fn validate(&self) -> Result<()> {
        if ![1, 2].contains(&self.version) {
            return Err("Unbekannte Maskenversion".into());
        }
        if self.layers.len() > 64 {
            return Err("Mehr als 64 Ebenen".into());
        }
        for layer in &self.layers {
            layer.mask.validate()?;
            layer.adjustments.validate()?;
        }
        Ok(())
    }
    pub fn from_json(json: &str) -> Result<Self> {
        if json.len() > 16 * 1024 * 1024 {
            return Err("Masken-JSON größer als 16 MiB".into());
        }
        let doc: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        doc.validate()?;
        Ok(doc)
    }
    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        if json.len() > 16 * 1024 * 1024 {
            return Err("Masken-JSON größer als 16 MiB".into());
        }
        Ok(json)
    }
}
fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn point(p: &[f32; 2]) -> bool {
    p.iter().all(|&v| v.is_finite() && v.abs() <= 16.0)
}
impl Mask {
    pub fn validate(&self) -> Result<()> {
        let mut nodes = 0;
        self.check(0, &mut nodes)
    }
    fn check(&self, depth: usize, nodes: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > 32 || *nodes > 4096 {
            return Err("Maskenbaum zu groß oder zu tief".into());
        }
        let valid = match self {
            Self::Brush {
                points,
                radius,
                hardness,
                flow,
            } => {
                !points.is_empty()
                    && points.len() <= 100_000
                    && points.iter().all(point)
                    && radius.is_finite()
                    && *radius >= 1e-6
                    && *radius <= 16.0
                    && unit(*hardness)
                    && unit(*flow)
            }
            Self::Linear { start, end } => {
                point(start) && point(end) && (start[0] - end[0]).hypot(start[1] - end[1]) > 1e-6
            }
            Self::Radial {
                center,
                radii,
                angle,
                feather,
            } => {
                point(center)
                    && radii
                        .iter()
                        .all(|r| r.is_finite() && *r >= 1e-6 && *r <= 16.0)
                    && angle.is_finite()
                    && angle.abs() <= 1000.0
                    && unit(*feather)
            }
            Self::Luminance { low, high, feather } => {
                low.is_finite()
                    && high.is_finite()
                    && low <= high
                    && low.abs() <= 1e6
                    && high.abs() <= 1e6
                    && feather.is_finite()
                    && *feather >= 0.0
                    && *feather <= 1e6
            }
            Self::Color {
                reference,
                radius,
                feather,
            } => {
                reference.iter().all(|v| v.is_finite() && v.abs() <= 1e6)
                    && radius.is_finite()
                    && *radius >= 0.0
                    && *radius <= 1e6
                    && feather.is_finite()
                    && *feather >= 0.0
                    && *feather <= 1e6
            }
            Self::Combine { left, right, .. } => {
                left.check(depth + 1, nodes)?;
                right.check(depth + 1, nodes)?;
                true
            }
            Self::Invert { mask } => {
                mask.check(depth + 1, nodes)?;
                true
            }
        };
        if valid {
            Ok(())
        } else {
            Err("Ungültige Maskenparameter".into())
        }
    }
}
#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) data: &'a [f32],
}
impl<'a> Image<'a> {
    pub fn new(width: usize, height: usize, data: &'a [f32]) -> Result<Self> {
        if width == 0
            || height == 0
            || width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(data.len())
        {
            return Err("Ungültiger RGB-Puffer".into());
        }
        if !data.par_iter().all(|v| v.is_finite()) {
            return Err("Nichtendlicher Bildwert".into());
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
}
/// Part of a larger image: the buffer handed to the functions is the crop at `offset` (pixels) of an
/// image of `full` pixels. Masks, radii and filter sizes then follow the whole image, so that a crop
/// with enough margin (see [`margin`]) comes out like the same part of the whole image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub full: [usize; 2],
    pub offset: [usize; 2],
}
impl View {
    /// The whole image itself.
    pub fn whole(image: &Image<'_>) -> Self {
        Self { full: [image.width, image.height], offset: [0, 0] }
    }
    /// Mask coordinate of the centre of pixel (x, y) of the crop.
    pub(crate) fn point(&self, x: usize, y: usize) -> [f32; 2] {
        [
            ((x + self.offset[0]) as f32 + 0.5) / self.full[0] as f32,
            ((y + self.offset[1]) as f32 + 0.5) / self.full[1] as f32,
        ]
    }
    pub(crate) fn short(&self) -> usize {
        self.full[0].min(self.full[1])
    }
}
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn falloff(d: f32, r: f32, feather: f32) -> f32 {
    if d <= r {
        1.0
    } else if feather == 0.0 {
        0.0
    } else {
        1.0 - smooth((d - r) / feather)
    }
}
pub(crate) fn luma(p: &[f32]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}
// Brush dabs are prepared once, never sampled from mouse events or output pixel spacing.
pub(crate) struct Prepared<'a> {
    mask: &'a Mask,
    dabs: Vec<[f32; 2]>,
    children: Vec<Prepared<'a>>,
    scale: [f32; 2],
    tiles: Vec<Vec<usize>>,
    grid: usize,
}
impl<'a> Prepared<'a> {
    pub(crate) fn new(mask: &'a Mask, w: usize, h: usize) -> Result<Self> {
        let short = w.min(h) as f32;
        let scale = [w as f32 / short, h as f32 / short];
        let mut result = Self {
            mask,
            dabs: Vec::new(),
            children: Vec::new(),
            scale,
            tiles: Vec::new(),
            grid: 1,
        };
        match mask {
            Mask::Brush { points, radius, .. } => {
                let pts: Vec<_> = points
                    .iter()
                    .map(|p| [p[0] * scale[0], p[1] * scale[1]])
                    .collect();
                result.dabs.push(pts[0]);
                let spacing = radius * 0.25;
                let mut until = spacing;
                for segment in pts.windows(2) {
                    let dx = segment[1][0] - segment[0][0];
                    let dy = segment[1][1] - segment[0][1];
                    let length = dx.hypot(dy);
                    let mut distance = until;
                    if (length / spacing) > 100_000.0 {
                        return Err("Pinselzug erzeugt zu viele Tupfer".into());
                    }
                    while distance <= length {
                        if result.dabs.len() >= 100_000 {
                            return Err("Mehr als 100000 Pinseltupfer".into());
                        }
                        let t = distance / length;
                        result
                            .dabs
                            .push([segment[0][0] + t * dx, segment[0][1] + t * dy]);
                        distance += spacing;
                    }
                    until = distance - length;
                }
            }
            Mask::Combine { left, right, .. } => {
                result.children.push(Self::new(left, w, h)?);
                result.children.push(Self::new(right, w, h)?);
            }
            Mask::Invert { mask } => result.children.push(Self::new(mask, w, h)?),
            _ => (),
        }
        if let Mask::Brush { radius, .. } = mask {
            if result.dabs.len() > 16 {
                result.grid =
                    ((1_000_000.0 / result.dabs.len() as f32).sqrt() as usize).clamp(1, 32);
                result.tiles = vec![Vec::new(); result.grid * result.grid];
                let g = result.grid as f32;
                for (i, q) in result.dabs.iter().enumerate() {
                    let lo = [
                        ((q[0] - radius) / scale[0] * g - 1e-5).floor(),
                        ((q[1] - radius) / scale[1] * g - 1e-5).floor(),
                    ];
                    let hi = [
                        ((q[0] + radius) / scale[0] * g + 1e-5).floor(),
                        ((q[1] + radius) / scale[1] * g + 1e-5).floor(),
                    ];
                    if hi[0] < 0.0 || hi[1] < 0.0 || lo[0] >= g || lo[1] >= g {
                        continue;
                    }
                    for y in (lo[1].max(0.0) as usize)..=(hi[1].min(g - 1.0) as usize) {
                        for x in (lo[0].max(0.0) as usize)..=(hi[0].min(g - 1.0) as usize) {
                            result.tiles[y * result.grid + x].push(i);
                        }
                    }
                }
            }
        }
        Ok(result)
    }
    pub(crate) fn sample(&self, p: [f32; 2], rgb: &[f32]) -> f32 {
        match self.mask {
            Mask::Brush {
                radius,
                hardness,
                flow,
                ..
            } => {
                let mut remaining = 1.0;
                let ids = if self.tiles.is_empty() {
                    None
                } else {
                    Some(
                        &self.tiles[((p[1] * self.grid as f32) as usize).min(self.grid - 1)
                            * self.grid
                            + ((p[0] * self.grid as f32) as usize).min(self.grid - 1)],
                    )
                };
                let p = [p[0] * self.scale[0], p[1] * self.scale[1]];
                let all: &[usize] = &[];
                let indices = ids.map_or(all, |v| v.as_slice());
                for n in indices
                    .iter()
                    .copied()
                    .chain(0..if ids.is_none() { self.dabs.len() } else { 0 })
                {
                    let q = &self.dabs[n];
                    let distance = (p[0] - q[0]).hypot(p[1] - q[1]);
                    let a = falloff(distance, radius * hardness, radius * (1.0 - hardness));
                    remaining *= 1.0 - flow * a;
                }
                1.0 - remaining
            }
            Mask::Linear { start, end } => {
                let d = [
                    (end[0] - start[0]) * self.scale[0],
                    (end[1] - start[1]) * self.scale[1],
                ];
                smooth(
                    ((p[0] - start[0]) * self.scale[0] * d[0]
                        + (p[1] - start[1]) * self.scale[1] * d[1])
                        / (d[0] * d[0] + d[1] * d[1]),
                )
            }
            Mask::Radial {
                center,
                radii,
                angle,
                feather,
            } => {
                let x = (p[0] - center[0]) * self.scale[0];
                let y = (p[1] - center[1]) * self.scale[1];
                let (s, c) = angle.sin_cos();
                let d = ((c * x + s * y) / radii[0]).hypot((-s * x + c * y) / radii[1]);
                falloff(d, 1.0 - feather, *feather)
            }
            Mask::Luminance { low, high, feather } => {
                let y = luma(rgb);
                if y < *low {
                    falloff(low - y, 0.0, *feather)
                } else if y > *high {
                    falloff(y - high, 0.0, *feather)
                } else {
                    1.0
                }
            }
            Mask::Color {
                reference,
                radius,
                feather,
            } => {
                let d = (rgb[0] - reference[0])
                    .hypot(rgb[1] - reference[1])
                    .hypot(rgb[2] - reference[2]);
                falloff(d, *radius, *feather)
            }
            Mask::Combine { operation, .. } => {
                let a = self.children[0].sample(p, rgb);
                let b = self.children[1].sample(p, rgb);
                match operation {
                    Operation::Add => (a + b).min(1.0),
                    Operation::Subtract => (a - b).max(0.0),
                    Operation::Intersect => a.min(b),
                }
            }
            Mask::Invert { .. } => 1.0 - self.children[0].sample(p, rgb),
        }
    }
}
/// Pixel centers (x+0.5)/width; color-dependent masks read exactly this input image.
pub fn rasterize(image: Image<'_>, mask: &Mask) -> Result<Vec<f32>> {
    rasterize_view(image, mask, View::whole(&image))
}
/// Like [`rasterize`] for a crop of a larger image (see [`View`]).
pub fn rasterize_view(image: Image<'_>, mask: &Mask, view: View) -> Result<Vec<f32>> {
    mask.validate()?;
    let prepared = Prepared::new(mask, view.full[0], view.full[1])?;
    Ok((0..image.width * image.height)
        .into_par_iter()
        .map(|i| prepared.sample(view.point(i % image.width, i / image.width), &image.data[i * 3..i * 3 + 3]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn soft_boundary_is_continuous_and_bounded() {
        let eps = 1e-5;
        assert!((falloff(1.0 - eps, 1.0, 0.5) - falloff(1.0 + eps, 1.0, 0.5)).abs() < 1e-6);
        let values: Vec<_> = (0..200)
            .map(|i| falloff(i as f32 / 100., 1.0, 0.5))
            .collect();
        assert!(values.iter().all(|&v| unit(v)));
        assert!(values.windows(2).all(|p| p[0] >= p[1]));
    }
}

#[cfg(test)]
mod tile_tests {
    use super::*;
    #[test]
    fn brush_tiles_match_unindexed_reference() {
        let points = (0..300)
            .map(|i| {
                [
                    0.1 + 0.8 * i as f32 / 300.0,
                    0.5 + 0.3 * (i as f32 * 0.05).sin(),
                ]
            })
            .collect();
        let mask = Mask::Brush {
            points,
            radius: 0.03,
            hardness: 0.6,
            flow: 0.05,
        };
        let fast = Prepared::new(&mask, 600, 400).unwrap();
        let mut reference = Prepared::new(&mask, 600, 400).unwrap();
        reference.tiles.clear();
        assert!(!fast.tiles.is_empty());
        for y in 0..40 {
            for x in 0..60 {
                let p = [(x as f32 + 0.5) / 60.0, (y as f32 + 0.5) / 40.0];
                assert_eq!(fast.sample(p, &[0.5; 3]), reference.sample(p, &[0.5; 3]));
            }
        }
    }
}

#[cfg(test)]
mod long_brush_tests {
    use super::*;
    #[test]
    #[ignore = "24 MP brush timing"]
    fn long_brush_24mp() {
        let mask = Mask::Brush {
            points: (0..10000)
                .map(|i| {
                    [
                        0.05 + 0.9 * i as f32 / 10000.0,
                        0.5 + 0.3 * (i as f32 * 0.001).sin(),
                    ]
                })
                .collect(),
            radius: 0.01,
            hardness: 0.5,
            flow: 0.1,
        };
        let mut ref_mask = Prepared::new(&mask, 6000, 4000).unwrap();
        ref_mask.tiles.clear();
        let fast = Prepared::new(&mask, 6000, 4000).unwrap();
        let t = std::time::Instant::now();
        let expected: Vec<_> = (0..4000)
            .map(|i| {
                ref_mask.sample(
                    [(i % 100) as f32 / 100.0, (i / 100) as f32 / 40.0],
                    &[0.5; 3],
                )
            })
            .collect();
        let slow_time = t.elapsed();
        let t = std::time::Instant::now();
        let actual: Vec<_> = (0..4000)
            .map(|i| {
                fast.sample(
                    [(i % 100) as f32 / 100.0, (i / 100) as f32 / 40.0],
                    &[0.5; 3],
                )
            })
            .collect();
        let fast_time = t.elapsed();
        assert_eq!(expected, actual);
        println!(
            "10000 Pfadpunkte, {} Tupfer: Referenz 4000 Proben {:.2?}, Index {:.2?}",
            fast.dabs.len(),
            slow_time,
            fast_time
        );
        let data = vec![0.5; 6000 * 4000 * 3];
        let image = Image::new(6000, 4000, &data).unwrap();
        let t = std::time::Instant::now();
        let weights = rasterize(image, &mask).unwrap();
        println!(
            "Long brush, 24 MP: {:.2?}, {} weights",
            t.elapsed(),
            weights.len()
        );
    }
}
