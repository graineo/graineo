//! Resolution-independent local adjustments; CPU reference, no file access.
//! Linear RGB with sRGB/Rec.709 primaries.
mod correction;
pub mod curve;
mod filters;
pub mod gpu;
pub mod raster;
pub mod refine;
pub use raster::{DepthClass, DepthData, Instances, PersonPart, RasterData, RasterSource};
pub use correction::{apply, apply_view, margin, Adjustments, CURVE_POINTS_MAX};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, from = "StoredDocument")]
pub struct Document {
    pub version: u32,
    pub layers: Vec<Layer>,
}

/// A document as stored, repaired on reading (see [`Document::repair`]).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDocument {
    version: u32,
    layers: Vec<Layer>,
}

impl From<StoredDocument> for Document {
    fn from(s: StoredDocument) -> Self {
        let mut d = Document { version: s.version, layers: s.layers };
        d.repair();
        d
    }
}

impl Document {
    /// Repairs what a short-lived development state wrote and the checks now refuse, so that one leaf never blocks
    /// the whole document (rendering and every save): a depth leaf with a value range (instead of depth classes)
    /// loses the range and its segment map, which is then computed again.
    pub fn repair(&mut self) {
        for l in &mut self.layers {
            l.mask.visit_mut(&mut |m| {
                if let Mask::Raster { raster, source, range, .. } = m {
                    if source.wide() && range.is_some() {
                        *range = None;
                        raster.clear();
                    }
                }
            });
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub mask: Mask,
    pub adjustments: Adjustments,
    /// own name of the mask (`None`: named after its kind); needs document version 4
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// switched off: kept, but not applied; needs document version 4 when `false`
    #[serde(default = "on", skip_serializing_if = "is_on")]
    pub enabled: bool,
    /// opacity of the whole mask, 0…1 (weight × amount); needs document version 4 when below 1
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub amount: f32,
    /// "Kante verfeinern", 0…1: the edge of the mask follows the edges of the picture in a narrow band (see
    /// [`refine`]); needs document version 4 when above 0
    #[serde(default, skip_serializing_if = "is_zero")]
    pub refine: f32,
}
fn on() -> bool {
    true
}
fn is_on(v: &bool) -> bool {
    *v
}
fn full() -> f32 {
    1.0
}
fn is_full(v: &f32) -> bool {
    *v == 1.0
}
/// Longest own name of a mask (characters).
pub const NAME_MAX: usize = 64;
impl Layer {
    /// A switched-on layer at full strength without an own name (the only kind before document version 4).
    pub fn new(mask: Mask, adjustments: Adjustments) -> Self {
        Self { mask, adjustments, name: None, enabled: true, amount: 1.0, refine: 0.0 }
    }
    /// Does the layer use a field of document version 4 (own name, switched off, strength below 100 %, refined edge)?
    pub fn uses_layer_fields(&self) -> bool {
        self.name.is_some() || !self.enabled || self.amount != 1.0 || self.refine != 0.0
    }
    /// Does the refined edge act? Only for masks without a raster of an image analysis: the guided filter is not used
    /// behind such masks (patent review), and the own edge snap measured no gain (see [`refine`]).
    pub fn refines(&self) -> bool {
        self.refine > 0.0 && !self.mask.has_raster()
    }
    /// The weights of the layer's mask for the picture `image` it sees (a crop `view` of the whole), with the refined
    /// edge when it acts, without the strength.
    pub fn weights(&self, image: Image<'_>, view: View) -> Result<Vec<f32>> {
        let mut w = rasterize_view(image, &self.mask, view)?;
        if self.refines() {
            refine::refine(&mut w, image.data, image.width, image.height, view.short(), self.refine, false);
        }
        Ok(w)
    }
    /// Does the layer change the image (switched on, strength above 0, a slider away from neutral)?
    pub fn active(&self) -> bool {
        self.enabled && self.amount > 0.0 && self.adjustments != Adjustments::default()
    }
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
    /// Colour range by perceived colour (needs document version 4): Oklab distance of a pixel to the nearest of the
    /// `samples` (linear RGB, at most [`SAMPLES_MAX`]; none = weight 0, waiting for the first sample), lightness
    /// counted half; full weight up to `radius`, then the soft edge `feather` (Oklab units).
    ColorRange {
        samples: Vec<[f32; 3]>,
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
    /// Weight map stored elsewhere (see [`raster`]); needs document version 3.
    Raster {
        /// content name of the raster (`sha256:<hex>`), empty while pending (computed again from `source`)
        raster: String,
        /// how the raster was made and can be made again
        source: RasterSource,
        /// soft edge in units of the short image side (0 = as stored), applied when rendering
        feather: f32,
        /// optional window over the raster value `[low, high, feather]`; `None` = the value itself
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<[f32; 3]>,
        /// edge shift in units of the short image side (−[`EDGE_MAX`] … +[`EDGE_MAX`]): above 0 the mask grows
        /// (grey-value dilation), below 0 it shrinks (erosion), applied when rendering before the soft edge;
        /// needs document version 4 when not 0
        #[serde(default, skip_serializing_if = "is_zero")]
        edge: f32,
    },
}
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}
/// Most colour samples of a colour range.
pub const SAMPLES_MAX: usize = 8;
/// Linear sRGB/Rec.709 to Oklab (Ottosson 2020, Oklab, public domain); cube root keeps the sign (negative and HDR
/// values stay finite).
pub fn oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.4122214708 * c[0] + 0.5363325363 * c[1] + 0.0514459929 * c[2];
    let m = 0.2119034982 * c[0] + 0.6806995451 * c[1] + 0.1073969566 * c[2];
    let s = 0.0883024619 * c[0] + 0.2817188376 * c[1] + 0.6299787005 * c[2];
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}
/// Distance of two Oklab colours for colour ranges: lightness counted half (a colour in shade stays the colour).
pub fn color_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0.5 * (a[0] - b[0])).hypot(a[1] - b[1]).hypot(a[2] - b[2])
}
/// Largest edge shift of a raster leaf (share of the short image side).
pub const EDGE_MAX: f32 = 0.05;
/// Highest document version this program reads. 3: raster leaves (same filters as 2). 4: name, on/off and strength
/// per layer (includes 3).
pub const VERSION_MAX: u32 = 4;
impl Document {
    /// Does any layer contain a raster leaf?
    pub fn has_raster(&self) -> bool {
        self.layers.iter().any(|l| l.mask.has_raster())
    }
    /// Does any layer use a field of version 4 (name, off, strength)?
    pub fn uses_layer_fields(&self) -> bool {
        self.layers.iter().any(Layer::uses_layer_fields)
    }
    /// Does any raster leaf use a rule added with version 4 (depth, scene part)?
    pub fn uses_new_sources(&self) -> bool {
        let mut found = false;
        for l in &self.layers {
            l.mask.visit(&mut |m| found |= matches!(m, Mask::Raster { source, .. } if source.needs_version_4()));
        }
        found
    }
    /// Can the layers get a name, be switched off or weakened? Not in version 1: version 4 would change its filters
    /// (the user switches it to version 2 first).
    pub fn allows_layer_fields(&self) -> bool {
        self.version >= 2
    }
    /// Does any layer use something of version 4: a layer field (name, off, strength), a slider of version 4 or mask
    /// content of version 4 (see [`Mask::uses_version_4`]; also the raster rules of version 4: depth, scene part,
    /// person part)?
    pub fn uses_version_4(&self) -> bool {
        self.layers.iter().any(|l| l.uses_layer_fields() || l.adjustments.uses_version_4() || l.mask.uses_version_4())
    }
    /// Version after an edit: 4 exactly when something of version 4 is used, otherwise 3 exactly when a raster leaf is
    /// present, otherwise 2 (version 1 stays 1 without rasters, so that older documents keep their filters; like a
    /// raster leaf, mask content of version 4 (also depth and scene parts) switches it, layer fields and sliders of
    /// version 4 stay locked there). Older programs refuse versions they do not know instead of misreading them, so
    /// the lowest sufficient one is written.
    pub fn settle_version(&mut self) {
        let content = self.uses_new_sources() || self.layers.iter().any(|l| l.mask.uses_version_4());
        let fields = self.layers.iter().any(|l| l.uses_layer_fields() || l.adjustments.uses_version_4());
        if content || (self.version >= 2 && fields) {
            self.version = 4;
        } else if self.has_raster() {
            self.version = 3;
        } else if self.version >= 3 {
            self.version = 2;
        }
    }
    /// The document for another photo (paste, sync, several photos at once): a raster belongs to one image, so
    /// leaves whose rule works on any photo (subject, people) become pending and are computed again there; leaves
    /// without such a rule (click points, painted) are left out. Returns the document (`None`: nothing left) and the
    /// number of leaves left out.
    pub fn for_other_photo(&self) -> (Option<Self>, usize) {
        fn prune(m: &Mask, dropped: &mut usize) -> Option<Mask> {
            match m {
                Mask::Raster { source, .. } if !source.transferable() => {
                    *dropped += 1;
                    None
                }
                Mask::Raster { source, feather, range, edge, .. } => {
                    // the classes of a depth mask work on any photo, the clicked segments belong to this one
                    let source = match source {
                        RasterSource::Depth { model, classes, .. } => RasterSource::Depth { model: model.clone(), classes: classes.clone(), toggled: Vec::new() },
                        s => s.clone(),
                    };
                    Some(Mask::Raster { raster: String::new(), source, feather: *feather, range: *range, edge: *edge })
                }
                Mask::Combine { operation, left, right } => match (prune(left, dropped), prune(right, dropped)) {
                    (Some(l), Some(r)) => Some(Mask::Combine { operation: *operation, left: Box::new(l), right: Box::new(r) }),
                    // without its base the rest of a subtraction or intersection would act on the whole photo
                    (None, _) if *operation != Operation::Add => None,
                    (Some(l), None) => Some(l),
                    (None, r) => r,
                },
                Mask::Invert { mask } => prune(mask, dropped).map(|m| Mask::Invert { mask: Box::new(m) }),
                m => Some(m.clone()),
            }
        }
        let mut dropped = 0;
        let layers: Vec<Layer> = self.layers.iter().filter_map(|l| prune(&l.mask, &mut dropped).map(|mask| Layer { mask, ..l.clone() })).collect();
        if layers.is_empty() {
            return (None, dropped);
        }
        let mut d = Self { version: self.version, layers };
        d.settle_version();
        (Some(d), dropped)
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=VERSION_MAX).contains(&self.version) {
            return Err("Unbekannte Maskenversion".into());
        }
        if self.version < 3 && self.has_raster() {
            return Err("Bitmap-Masken brauchen Maskenversion 3".into());
        }
        if self.version < 4 && self.uses_version_4() {
            return Err("Name, Aus, Stärke, Kante, Farbproben und neue Regler einer Maske brauchen Maskenversion 4".into());
        }
        if self.version < 4 && self.uses_new_sources() {
            return Err("Tiefen- und Himmelsmasken brauchen Maskenversion 4".into());
        }
        if self.layers.len() > 64 {
            return Err("Mehr als 64 Ebenen".into());
        }
        for layer in &self.layers {
            layer.mask.validate()?;
            layer.adjustments.validate()?;
            if !unit(layer.amount) {
                return Err("Stärke einer Maske außerhalb 0…1".into());
            }
            if !unit(layer.refine) {
                return Err("„Kante verfeinern“ außerhalb 0…1".into());
            }
            if layer.name.as_ref().is_some_and(|n| n.chars().count() > NAME_MAX || n.chars().any(char::is_control)) {
                return Err("Ungültiger Maskenname".into());
            }
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
    /// Visit this node and all nodes below it.
    pub fn visit(&self, f: &mut impl FnMut(&Mask)) {
        f(self);
        match self {
            Self::Combine { left, right, .. } => {
                left.visit(f);
                right.visit(f);
            }
            Self::Invert { mask } => mask.visit(f),
            _ => (),
        }
    }
    /// Visit this node and all nodes below it, mutably.
    pub fn visit_mut(&mut self, f: &mut impl FnMut(&mut Mask)) {
        f(self);
        match self {
            Self::Combine { left, right, .. } => {
                left.visit_mut(f);
                right.visit_mut(f);
            }
            Self::Invert { mask } => mask.visit_mut(f),
            _ => (),
        }
    }
    /// Does the mask use content of document version 4 (an edge shift of a raster leaf …)?
    pub fn uses_version_4(&self) -> bool {
        let mut found = false;
        self.visit(&mut |m| {
            found |= match m {
                Self::Raster { edge, source, .. } => *edge != 0.0 || matches!(source, RasterSource::Click { rect: Some(_), .. }) || source.needs_version_4(),
                Self::ColorRange { .. } => true,
                _ => false,
            }
        });
        found
    }
    pub fn has_raster(&self) -> bool {
        let mut found = false;
        self.visit(&mut |m| found |= matches!(m, Self::Raster { .. }));
        found
    }
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
            Self::ColorRange { samples, radius, feather } => {
                samples.len() <= SAMPLES_MAX
                    && samples.iter().flatten().all(|v| v.is_finite() && v.abs() <= 1e6)
                    && radius.is_finite()
                    && (0.0..=2.0).contains(radius)
                    && feather.is_finite()
                    && (0.0..=2.0).contains(feather)
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
            Self::Raster { raster: id, source, feather, range, edge } => {
                raster::valid_id(id)
                    && source.valid()
                    // a depth mask selects whole segments, never a window over the depth
                    && !(source.wide() && range.is_some())
                    && feather.is_finite()
                    && (0.0..=0.25).contains(feather)
                    && range.is_none_or(|r| r.iter().all(|v| unit(*v)) && r[0] <= r[1])
                    && edge.is_finite()
                    && edge.abs() <= EDGE_MAX
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
pub(crate) fn falloff(d: f32, r: f32, feather: f32) -> f32 {
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
    /// raster leaf: prepared weights (`None`: missing or pending → weight 0)
    raster: Option<std::sync::Arc<raster::Sampler>>,
    /// colour range: the samples in Oklab
    labs: Vec<[f32; 3]>,
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
            raster: None,
            labs: Vec::new(),
        };
        match mask {
            Mask::ColorRange { samples, .. } => result.labs = samples.iter().map(|c| oklab(*c)).collect(),
            Mask::Raster { raster: id, feather, edge, source, .. } => {
                result.raster = match source {
                    RasterSource::Depth { classes, toggled, .. } => raster::Sampler::new_segments(id, classes, toggled, *feather, *edge, [w, h]),
                    _ => raster::Sampler::new(id, *feather, *edge, [w, h]),
                };
            }
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
            Mask::ColorRange { radius, feather, .. } => {
                if self.labs.is_empty() {
                    return 0.0;
                }
                let q = oklab([rgb[0], rgb[1], rgb[2]]);
                let d = self.labs.iter().map(|s| color_distance(q, *s)).fold(f32::INFINITY, f32::min);
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
            Mask::Raster { range, .. } => self.raster.as_ref().map_or(0.0, |r| raster::window(r.sample(p), range)),
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
