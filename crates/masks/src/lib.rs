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
        /// highest weight of the stroke (0…1, missing = 1): the flow builds up to it, never beyond; needs document
        /// version 4 when below 1
        #[serde(default = "full", skip_serializing_if = "is_full")]
        density: f32,
        /// edge-aware stroke: acts only on pixels whose colour is close to `auto.reference`; needs document version 4
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto: Option<AutoMask>,
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
/// "Auto-Maske" of a brush stroke: the stroke acts only where the picture the layer sees resembles the colour under
/// the start of the stroke. Weight factor: full up to half the `tolerance` (Oklab distance, lightness counted half as
/// in colour ranges, [`color_distance`]), then a soft edge to 0 at `tolerance`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoMask {
    /// colour under the start of the stroke, linear RGB
    pub reference: [f32; 3],
    /// Oklab distance where the stroke stops acting (0 < t ≤ 1)
    pub tolerance: f32,
}
impl AutoMask {
    fn valid(&self) -> bool {
        self.reference.iter().all(|v| v.is_finite() && v.abs() <= 1e6) && self.tolerance.is_finite() && self.tolerance > 0.0 && self.tolerance <= 1.0
    }
    /// Weight factor of a pixel of colour `rgb` (linear RGB) for the reference in Oklab `lab`.
    fn factor(&self, lab: [f32; 3], rgb: &[f32]) -> f32 {
        let d = color_distance(oklab([rgb[0], rgb[1], rgb[2]]), lab);
        falloff(d, 0.5 * self.tolerance, 0.5 * self.tolerance)
    }
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
                Self::Brush { density, auto, .. } => *density != 1.0 || auto.is_some(),
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
                density,
                auto,
            } => {
                !points.is_empty()
                    && points.len() <= 100_000
                    && points.iter().all(point)
                    && radius.is_finite()
                    && *radius >= 1e-6
                    && *radius <= 16.0
                    && unit(*hardness)
                    && unit(*flow)
                    && unit(*density)
                    && *density > 0.0
                    && auto.is_none_or(|a| a.valid())
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
/// Most dabs of one stroke when a crop is rendered ([`Prepared::for_view`]): a sanity limit far above what painting
/// produces (a stroke of 100 000 points with the smallest brush stays below it).
pub const DABS_MAX: usize = 16_000_000;

/// The dabs of a stroke (short-side units): the first point, then every `radius / 4` along the path. Never sampled
/// from mouse events or output pixel spacing, so the weights do not depend on the resolution.
fn brush_dabs(points: &[[f32; 2]], radius: f32, scale: [f32; 2], max: usize) -> Result<Vec<[f32; 2]>> {
    let pts: Vec<_> = points.iter().map(|p| [p[0] * scale[0], p[1] * scale[1]]).collect();
    let mut dabs = vec![pts[0]];
    let spacing = radius * 0.25;
    let mut until = spacing;
    for segment in pts.windows(2) {
        let dx = segment[1][0] - segment[0][0];
        let dy = segment[1][1] - segment[0][1];
        let length = dx.hypot(dy);
        let mut distance = until;
        if (length / spacing) > max as f32 {
            return Err("Pinselzug erzeugt zu viele Tupfer".into());
        }
        while distance <= length {
            if dabs.len() >= max {
                return Err(if max == 100_000 { "Mehr als 100000 Pinseltupfer" } else { "Pinselzug erzeugt zu viele Tupfer" }.into());
            }
            let t = distance / length;
            dabs.push([segment[0][0] + t * dx, segment[0][1] + t * dy]);
            distance += spacing;
        }
        until = distance - length;
    }
    Ok(dabs)
}

/// The strokes of a brush part: one stroke or an add-tree of strokes (`false`: something else in the tree).
fn strokes<'m>(m: &'m Mask, out: &mut Vec<&'m Mask>) -> bool {
    match m {
        Mask::Brush { .. } => {
            out.push(m);
            true
        }
        Mask::Combine { operation: Operation::Add, left, right } => strokes(left, out) && strokes(right, out),
        _ => false,
    }
}

/// The weights of a brush part painted once into a raster over the crop (instead of summing all dabs for every
/// pixel): pixels `x0 .. x0 + w`, `y0 .. y0 + h` of the whole image, 0 outside.
struct Painted {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    full: [usize; 2],
    data: Vec<f32>,
}

impl Painted {
    fn get(&self, p: [f32; 2]) -> f32 {
        // p is the centre of a pixel ((x + 0.5) / width): its index again
        let x = (p[0] * self.full[0] as f32).floor();
        let y = (p[1] * self.full[1] as f32).floor();
        if x < self.x0 as f32 || y < self.y0 as f32 {
            return 0.0;
        }
        let (x, y) = (x as usize - self.x0, y as usize - self.y0);
        if x >= self.w || y >= self.h {
            return 0.0;
        }
        self.data[y * self.w + x]
    }
}

/// Rows per band when the dabs of a stroke are sorted by rows.
const BAND: usize = 16;

/// Unclamped sum of the weights of some strokes over pixels `x0 .. x0 + w`, `y0 .. y0 + h` of the whole image.
struct Sum {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    data: Vec<f32>,
}

/// Sums of all strokes of a brush part but the last, for the crops computed last: while painting only the last stroke
/// grows, so the others are summed once and not again for every new point (overlay and preview each frame).
static PREFIX: std::sync::Mutex<Vec<(u64, std::sync::Arc<Sum>)>> = std::sync::Mutex::new(Vec::new());
/// Kept sums: at most this many, and only up to [`PREFIX_PIXELS`] pixels each.
const PREFIX_KEPT: usize = 6;
const PREFIX_PIXELS: usize = 4_000_000;

/// Fingerprint of strokes painted into the crop `size` at `view` of the picture `rgb` (read only when a stroke has an
/// auto mask, whose weights depend on it).
fn strokes_key(group: &[&Mask], view: View, size: [usize; 2], rgb: &[f32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (view.full, view.offset, size, group.len()).hash(&mut h);
    if group.iter().any(|m| matches!(m, Mask::Brush { auto: Some(_), .. })) {
        // a quick fingerprint of the whole picture (in parallel; every value counts)
        let parts: Vec<u64> = rgb
            .par_chunks(1 << 16)
            .map(|c| c.iter().fold(0xcbf29ce484222325u64, |a, v| (a ^ v.to_bits() as u64).wrapping_mul(0x100000001b3)))
            .collect();
        parts.hash(&mut h);
    }
    for m in group {
        if let Mask::Brush { points, radius, hardness, flow, density, auto } = m {
            points.len().hash(&mut h);
            for p in points {
                (p[0].to_bits(), p[1].to_bits()).hash(&mut h);
            }
            (radius.to_bits(), hardness.to_bits(), flow.to_bits(), density.to_bits()).hash(&mut h);
            auto.map(|a| (a.reference.map(f32::to_bits), a.tolerance.to_bits())).hash(&mut h);
        }
    }
    h.finish()
}

/// Paint the strokes of a brush part into a raster over the crop `size` at `view`. Within a stroke the factors
/// `1 − flow · a` of all dabs that reach a pixel are multiplied in the same order as [`Prepared::sample`] does (same
/// numbers); the strokes of an add-tree are summed and clamped to 1 at the end (the tree clamps after each sum, which
/// is the same for weights ≥ 0 up to rounding). The sum of all strokes but the last is kept for the next call
/// ([`PREFIX`]).
fn paint(group: &[&Mask], view: View, size: [usize; 2], scale: [f32; 2], rgb: &[f32]) -> Result<Painted> {
    let full = view.full;
    let (rest, last) = group.split_at(group.len().saturating_sub(1));
    let base = if rest.is_empty() || size[0] * size[1] > PREFIX_PIXELS {
        None
    } else {
        let key = strokes_key(rest, view, size, rgb);
        let kept = PREFIX.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key).map(|(_, s)| s.clone());
        match kept {
            Some(s) => Some(s),
            None => {
                let s = std::sync::Arc::new(sum(rest, view, size, scale, rgb, None)?);
                let mut g = PREFIX.lock().unwrap_or_else(|e| e.into_inner());
                g.retain(|(k, _)| *k != key);
                g.push((key, s.clone()));
                if g.len() > PREFIX_KEPT {
                    g.remove(0);
                }
                Some(s)
            }
        }
    };
    let strokes = if base.is_some() { last } else { group };
    let mut s = sum(strokes, view, size, scale, rgb, base.as_deref())?;
    s.data.par_iter_mut().for_each(|v| *v = v.min(1.0));
    Ok(Painted { x0: s.x0, y0: s.y0, w: s.w, h: s.h, full, data: s.data })
}

/// The unclamped sum of `base` (if any) and the weights of `group` over the crop `size` at `view`; `rgb` is the crop
/// of the picture the layer sees (auto masks).
fn sum(group: &[&Mask], view: View, size: [usize; 2], scale: [f32; 2], rgb: &[f32], base: Option<&Sum>) -> Result<Sum> {
    let full = view.full;
    let short = full[0].min(full[1]) as f32;
    let crop = [view.offset[0], view.offset[1], view.offset[0] + size[0], view.offset[1] + size[1]];
    struct Stroke {
        dabs: Vec<[f32; 2]>,
        radius: f32,
        hardness: f32,
        flow: f32,
        density: f32,
        /// auto mask and its reference in Oklab
        auto: Option<(AutoMask, [f32; 3])>,
        /// pixels x0 y0 x1 y1 (exclusive) it can reach, within the crop
        rect: [usize; 4],
    }
    // pixel range a dab at short-side coordinate `c` with radius `r` reaches (with one pixel to spare)
    let span = |c: f32, r: f32, lo: usize, hi: usize| -> Option<(usize, usize)> {
        let a = (c * short - 0.5 - r * short - 1.0).floor();
        let b = (c * short - 0.5 + r * short + 1.0).ceil();
        let a = a.max(lo as f32);
        let b = b.min(hi as f32 - 1.0);
        (a <= b).then_some((a as usize, b as usize + 1))
    };
    let mut list = Vec::with_capacity(group.len());
    for m in group {
        let Mask::Brush { points, radius, hardness, flow, density, auto } = m else { continue };
        let dabs = brush_dabs(points, *radius, scale, DABS_MAX)?;
        let mut rect = [usize::MAX, usize::MAX, 0, 0];
        for q in &dabs {
            if let (Some(x), Some(y)) = (span(q[0], *radius, crop[0], crop[2]), span(q[1], *radius, crop[1], crop[3])) {
                rect = [rect[0].min(x.0), rect[1].min(y.0), rect[2].max(x.1), rect[3].max(y.1)];
            }
        }
        if rect[0] < rect[2] && rect[1] < rect[3] {
            let auto = auto.map(|a| (a, oklab(a.reference)));
            list.push(Stroke { dabs, radius: *radius, hardness: *hardness, flow: *flow, density: *density, auto, rect });
        }
    }
    let start = base.filter(|b| b.w > 0 && b.h > 0).map_or([usize::MAX, usize::MAX, 0, 0], |b| [b.x0, b.y0, b.x0 + b.w, b.y0 + b.h]);
    let area = list.iter().fold(start, |a, s| [a[0].min(s.rect[0]), a[1].min(s.rect[1]), a[2].max(s.rect[2]), a[3].max(s.rect[3])]);
    if area[0] >= area[2] || area[1] >= area[3] {
        return Ok(Sum { x0: 0, y0: 0, w: 0, h: 0, data: Vec::new() });
    }
    let (aw, ah) = (area[2] - area[0], area[3] - area[1]);
    let mut sum = vec![0.0f32; aw * ah];
    if let Some(b) = base.filter(|b| b.w > 0 && b.h > 0) {
        for y in 0..b.h {
            let at = (y + b.y0 - area[1]) * aw + b.x0 - area[0];
            sum[at..at + b.w].copy_from_slice(&b.data[y * b.w..(y + 1) * b.w]);
        }
    }
    // pixel centres in short-side units (as `View::point` and `sample` compute them)
    let xs: Vec<f32> = (area[0]..area[2]).map(|x| ((x as f32 + 0.5) / full[0] as f32) * scale[0]).collect();
    let ys: Vec<f32> = (area[1]..area[3]).map(|y| ((y as f32 + 0.5) / full[1] as f32) * scale[1]).collect();
    let bands = ah.div_ceil(BAND);
    for s in &list {
        // the dabs of each band of rows, in their order
        let mut by_band: Vec<Vec<u32>> = vec![Vec::new(); bands];
        for (i, q) in s.dabs.iter().enumerate() {
            if let Some((y0, y1)) = span(q[1], s.radius, s.rect[1], s.rect[3]) {
                for b in (y0 - area[1]) / BAND..=(y1 - 1 - area[1]) / BAND {
                    by_band[b].push(i as u32);
                }
            }
        }
        let (r, rh, f) = (s.radius, s.radius * s.hardness, s.radius * (1.0 - s.hardness));
        // beyond the radius a dab gives exactly 0 (its factor is exactly 1): skipped, with a margin for rounding
        let reach = (r * 1.0001) * (r * 1.0001);
        sum.par_chunks_mut(aw * BAND).zip(by_band.par_iter()).enumerate().for_each(|(b, (rows, ids))| {
            if ids.is_empty() {
                return;
            }
            let (sx0, sx1) = (s.rect[0] - area[0], s.rect[2] - area[0]);
            let mut remaining = vec![1.0f32; sx1 - sx0];
            for (k, row) in rows.chunks_mut(aw).enumerate() {
                let y = b * BAND + k;
                let ay = y + area[1];
                if ay < s.rect[1] || ay >= s.rect[3] {
                    continue;
                }
                let py = ys[y];
                remaining.fill(1.0);
                let mut touched = false;
                for &i in ids {
                    let q = s.dabs[i as usize];
                    let dy = py - q[1];
                    if dy * dy > reach {
                        continue;
                    }
                    let half = (reach - dy * dy).max(0.0).sqrt();
                    let Some((x0, x1)) = span(q[0], half, s.rect[0], s.rect[2]) else { continue };
                    for x in x0..x1 {
                        let px = xs[x - area[0]];
                        let dx = px - q[0];
                        if dx * dx + dy * dy > reach {
                            continue;
                        }
                        let a = falloff(dx.hypot(dy), rh, f);
                        remaining[x - area[0] - sx0] *= 1.0 - s.flow * a;
                        touched = true;
                    }
                }
                if touched {
                    match s.auto {
                        None => {
                            for (o, rem) in row[sx0..sx1].iter_mut().zip(&remaining) {
                                *o += (1.0 - rem).min(s.density);
                            }
                        }
                        Some((a, lab)) => {
                            // the pixel of the crop under this one
                            let at = |x: usize| ((ay - crop[1]) * size[0] + x + area[0] - crop[0]) * 3;
                            for (k, (o, rem)) in row[sx0..sx1].iter_mut().zip(&remaining).enumerate() {
                                let w = (1.0 - rem).min(s.density);
                                if w > 0.0 {
                                    let i = at(sx0 + k);
                                    *o += w * a.factor(lab, &rgb[i..i + 3]);
                                }
                            }
                        }
                    }
                }
            }
        });
    }
    Ok(Sum { x0: area[0], y0: area[1], w: aw, h: ah, data: sum })
}

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
    /// brush part rendered for a crop: its weights painted once ([`Prepared::for_view`])
    painted: Option<Painted>,
}
impl<'a> Prepared<'a> {
    /// Prepared for sampling at any point: brush strokes sum their dabs for every point (the reference of the tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(mask: &'a Mask, w: usize, h: usize) -> Result<Self> {
        Self::make(mask, w, h, None)
    }
    /// Prepared for sampling the pixel centres of the crop `size` at `view` only: brush parts are painted once into a
    /// raster over the crop (cost independent of the number of pixels each dab misses).
    pub(crate) fn for_view(mask: &'a Mask, view: View, image: Image<'_>) -> Result<Self> {
        Self::make(mask, view.full[0], view.full[1], Some((view, image)))
    }
    fn make(mask: &'a Mask, w: usize, h: usize, crop: Option<(View, Image<'_>)>) -> Result<Self> {
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
            painted: None,
        };
        if let Some((view, image)) = crop {
            let mut group = Vec::new();
            if strokes(mask, &mut group) {
                result.painted = Some(paint(&group, view, [image.width, image.height], scale, image.data)?);
                return Ok(result);
            }
        }
        match mask {
            Mask::ColorRange { samples, .. } => result.labs = samples.iter().map(|c| oklab(*c)).collect(),
            Mask::Raster { raster: id, feather, edge, source, .. } => {
                result.raster = match source {
                    RasterSource::Depth { classes, toggled, .. } => raster::Sampler::new_segments(id, classes, toggled, *feather, *edge, [w, h]),
                    _ => raster::Sampler::new(id, *feather, *edge, [w, h]),
                };
            }
            Mask::Brush { points, radius, .. } => {
                // the reference sums every dab per point: kept to the former budget
                result.dabs = brush_dabs(points, *radius, scale, 100_000)?;
            }
            Mask::Combine { left, right, .. } => {
                result.children.push(Self::make(left, w, h, crop)?);
                result.children.push(Self::make(right, w, h, crop)?);
            }
            Mask::Invert { mask } => result.children.push(Self::make(mask, w, h, crop)?),
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
        if let Some(painted) = &self.painted {
            return painted.get(p);
        }
        match self.mask {
            Mask::Brush {
                radius,
                hardness,
                flow,
                density,
                auto,
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
                let w = (1.0 - remaining).min(*density);
                match auto {
                    Some(a) if w > 0.0 => w * a.factor(oklab(a.reference), rgb),
                    _ => w,
                }
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
    let prepared = Prepared::for_view(mask, view, image)?;
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
            density: 1.0,
            auto: None,
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
mod painted_tests {
    use super::*;

    fn walk(n: usize, seed: u64, step: f32) -> Vec<[f32; 2]> {
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f32) / (1u64 << 31) as f32
        };
        let (mut p, mut angle) = ([0.5f32, 0.5], 0.0f32);
        (0..n)
            .map(|_| {
                angle += (next() - 0.5) * 2.5;
                p[0] = (p[0] + angle.cos() * step * (0.2 + next())).clamp(0.05, 0.95);
                p[1] = (p[1] + angle.sin() * step * (0.2 + next())).clamp(0.05, 0.95);
                p
            })
            .collect()
    }
    fn stroke(n: usize, seed: u64, radius: f32, hardness: f32, flow: f32) -> Mask {
        Mask::Brush { points: walk(n, seed, 0.01), radius, hardness, flow, density: 1.0, auto: None }
    }
    fn with_density(mut m: Mask, d: f32) -> Mask {
        if let Mask::Brush { density, .. } = &mut m {
            *density = d;
        }
        m
    }
    fn add(a: Mask, b: Mask) -> Mask {
        Mask::Combine { operation: Operation::Add, left: Box::new(a), right: Box::new(b) }
    }
    /// The reference: every pixel sums all dabs ([`Prepared::new`]), as before the painted raster.
    fn reference(mask: &Mask, full: [usize; 2], view: View, size: [usize; 2]) -> Vec<f32> {
        let p = Prepared::new(mask, full[0], full[1]).unwrap();
        (0..size[0] * size[1]).map(|i| p.sample(view.point(i % size[0], i / size[0]), &[0.5; 3])).collect()
    }
    fn painted(mask: &Mask, view: View, size: [usize; 2]) -> Vec<f32> {
        let data = vec![0.5f32; size[0] * size[1] * 3];
        rasterize_view(Image::new(size[0], size[1], &data).unwrap(), mask, view).unwrap()
    }

    /// The painted raster gives the weights of the reference: the same numbers within a stroke, sums of strokes up to
    /// float rounding; also for crops, inverted parts and parts combined with other shapes.
    #[test]
    fn painted_strokes_match_the_reference() {
        let ellipse = Mask::Radial { center: [0.4, 0.5], radii: [0.3, 0.2], angle: 0.2, feather: 0.4 };
        let cases = vec![
            stroke(400, 1, 0.04, 0.6, 0.2),
            stroke(300, 2, 0.01, 1.0, 1.0),
            stroke(300, 3, 0.02, 0.0, 0.5),
            stroke(1, 4, 0.05, 0.5, 0.3),
            add(add(stroke(200, 5, 0.03, 0.6, 0.2), stroke(200, 6, 0.05, 0.3, 0.4)), stroke(150, 7, 0.02, 0.9, 0.8)),
            Mask::Invert { mask: Box::new(add(stroke(200, 8, 0.03, 0.6, 0.2), stroke(200, 9, 0.03, 0.6, 0.2))) },
            Mask::Combine { operation: Operation::Subtract, left: Box::new(ellipse.clone()), right: Box::new(add(stroke(200, 10, 0.04, 0.5, 0.6), stroke(100, 11, 0.02, 0.5, 0.6))) },
            Mask::Combine { operation: Operation::Intersect, left: Box::new(stroke(300, 12, 0.06, 0.4, 0.3)), right: Box::new(ellipse) },
            with_density(stroke(400, 13, 0.04, 0.6, 0.5), 0.4),
            add(with_density(stroke(200, 14, 0.03, 0.6, 0.6), 0.3), with_density(stroke(200, 15, 0.05, 0.2, 0.8), 0.7)),
        ];
        let full = [300, 200];
        let whole = View { full, offset: [0, 0] };
        let crop = View { full, offset: [70, 40] };
        let mut worst = 0.0f32;
        for (k, m) in cases.iter().enumerate() {
            for (view, size) in [(whole, full), (crop, [120, 90])] {
                let a = painted(m, view, size);
                let b = reference(m, full, view, size);
                let d = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
                worst = worst.max(d);
                assert!(d <= 2e-6, "case {k}, offset {:?}: largest difference {d}", view.offset);
                if k < 4 || k == 8 {
                    assert_eq!(a, b, "case {k}: one stroke gives the same numbers");
                }
            }
        }
        println!("painted vs reference: largest difference {worst:e}");
    }

    /// While the last stroke grows, the kept sum of the other strokes gives the same weights as computing all again.
    #[test]
    fn growing_last_stroke_uses_the_kept_sum() {
        let full = [320, 240];
        let view = View { full, offset: [0, 0] };
        let mut strokes: Vec<Mask> = (0..5).map(|i| stroke(200, 50 + i, 0.03, 0.6, 0.3)).collect();
        for n in [3usize, 10, 40] {
            if let Mask::Brush { points, .. } = &mut strokes[4] {
                points.extend(walk(n, 60 + n as u64, 0.01));
            }
            let tree = strokes[1..].iter().cloned().fold(strokes[0].clone(), add);
            let a = painted(&tree, view, full);
            let b = reference(&tree, full, view, full);
            let d = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
            assert!(d <= 2e-6, "{n} more points: largest difference {d}");
        }
    }

    /// Density: the weight of a stroke stops at it however often it is painted over; documents without the field read
    /// as before (density 1, not written back), with it they need version 4.
    #[test]
    fn density_caps_the_stroke_and_old_documents_read_unchanged() {
        let m = with_density(stroke(2000, 70, 0.05, 0.6, 0.5), 0.35);
        let w = painted(&m, View { full: [200, 200], offset: [0, 0] }, [200, 200]);
        let top = w.iter().cloned().fold(0.0f32, f32::max);
        assert!((top - 0.35).abs() < 1e-6, "highest weight {top}");
        let old = r#"{"version":2,"layers":[{"mask":{"type":"brush","points":[[0.2,0.3],[0.4,0.5]],"radius":0.05,"hardness":0.5,"flow":0.8},"adjustments":{"exposure":0.5}}]}"#;
        let d = Document::from_json(old).unwrap();
        assert!(matches!(d.layers[0].mask, Mask::Brush { density, .. } if density == 1.0));
        assert!(!d.to_json().unwrap().contains("density"), "density 1 is not written");
        let mut d = d;
        if let Mask::Brush { density, .. } = &mut d.layers[0].mask {
            *density = 0.5;
        }
        assert!(d.validate().is_err(), "version 2 cannot hold a density");
        d.settle_version();
        assert_eq!(d.version, 4);
        let back = Document::from_json(&d.to_json().unwrap()).unwrap();
        assert_eq!(back, d);
        for bad in [0.0f32, -0.1, 1.5, f32::NAN] {
            assert!(with_density(stroke(3, 1, 0.05, 0.5, 0.5), bad).validate().is_err());
        }
    }

    /// A picture with a hard edge: sky (light blue) above a dark green horizon with a soft gradient in the sky.
    fn horizon(w: usize, h: usize) -> Vec<f32> {
        (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let edge = h as f32 * (0.55 + 0.1 * (x as f32 / w as f32 * 6.0).sin());
                if (y as f32) < edge {
                    let t = y as f32 / h as f32;
                    [0.35 + 0.1 * t, 0.5 + 0.08 * t, 0.8]
                } else {
                    [0.03, 0.06, 0.02]
                }
            })
            .collect()
    }
    fn auto(m: Mask, reference: [f32; 3], tolerance: f32) -> Mask {
        match m {
            Mask::Brush { points, radius, hardness, flow, density, .. } => Mask::Brush { points, radius, hardness, flow, density, auto: Some(AutoMask { reference, tolerance }) },
            m => m,
        }
    }

    /// Auto mask: the painted raster gives the weights of the reference path (per pixel, with its colour); the stroke
    /// acts on the sky it started on and not across the edge; the kept sum follows a changed picture.
    #[test]
    fn auto_mask_stops_at_the_edge() {
        let (w, h) = (300usize, 200usize);
        let rgb = horizon(w, h);
        let image = Image::new(w, h, &rgb).unwrap();
        let view = View { full: [w, h], offset: [0, 0] };
        let sky = [0.37, 0.51, 0.8];
        // a stroke along the horizon, half over the dark ground
        let line: Vec<[f32; 2]> = (0..60).map(|i| [0.1 + 0.8 * i as f32 / 59.0, 0.55 + 0.1 * ((0.1 + 0.8 * i as f32 / 59.0) * 6.0).sin()]).collect();
        let plain = Mask::Brush { points: line, radius: 0.08, hardness: 0.5, flow: 0.6, density: 1.0, auto: None };
        let edge_aware = auto(plain.clone(), sky, 0.15);
        let a = rasterize_view(image, &edge_aware, view).unwrap();
        let p = Prepared::new(&edge_aware, w, h).unwrap();
        let r: Vec<f32> = (0..w * h).map(|i| p.sample(view.point(i % w, i / w), &rgb[i * 3..i * 3 + 3])).collect();
        let d = a.iter().zip(&r).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(d <= 1e-6, "painted vs reference: {d}");
        let b = rasterize_view(image, &plain, view).unwrap();
        let (mut sky_on, mut ground_on, mut ground_plain) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..w * h {
            let ground = rgb[i * 3 + 2] < 0.1;
            if ground {
                ground_on = ground_on.max(a[i]);
                ground_plain = ground_plain.max(b[i]);
            } else {
                sky_on = sky_on.max(a[i]);
            }
        }
        println!("auto mask: sky up to {sky_on:.3}, ground up to {ground_on:.4} (without: {ground_plain:.3})");
        assert!(sky_on > 0.9 && ground_plain > 0.9 && ground_on == 0.0);
        // two strokes (the first is kept as sum): a changed picture gives new weights
        let two = Mask::Combine { operation: Operation::Add, left: Box::new(edge_aware.clone()), right: Box::new(auto(stroke(50, 3, 0.03, 0.5, 0.5), sky, 0.15)) };
        let first = rasterize_view(image, &two, view).unwrap();
        let dark = vec![0.03f32; w * h * 3];
        let second = rasterize_view(Image::new(w, h, &dark).unwrap(), &two, view).unwrap();
        assert!(first.iter().any(|v| *v > 0.5) && second.iter().all(|v| *v == 0.0));
        // stored only when used; version 4; bad tolerances refused
        let mut d = Document { version: 2, layers: vec![Layer::new(edge_aware.clone(), Adjustments::default())] };
        assert!(d.validate().is_err());
        d.settle_version();
        let json = d.to_json().unwrap();
        assert!(json.contains("\"auto\"") && json.contains("tolerance"));
        assert_eq!(Document::from_json(&json).unwrap(), d);
        for t in [0.0f32, 1.5, f32::NAN] {
            assert!(auto(plain.clone(), sky, t).validate().is_err());
        }
    }

    /// A crop gives exactly the pixels of the whole picture (preview, detail and export agree).
    #[test]
    fn crops_match_the_whole() {
        let m = add(stroke(500, 21, 0.03, 0.6, 0.2), stroke(500, 22, 0.05, 0.2, 0.5));
        let full = [400, 300];
        let all = painted(&m, View { full, offset: [0, 0] }, full);
        let (off, size) = ([123, 77], [150, 101]);
        let part = painted(&m, View { full, offset: off }, size);
        for y in 0..size[1] {
            for x in 0..size[0] {
                assert_eq!(part[y * size[0] + x], all[(y + off[1]) * full[0] + x + off[0]]);
            }
        }
    }

    /// A long stroke with the smallest brush (far more dabs than the former limit of 100 000) is drawn.
    #[test]
    fn long_strokes_with_a_small_brush_are_drawn() {
        let m = Mask::Brush { points: walk(20_000, 31, 0.008), radius: 0.001, hardness: 0.6, flow: 0.2, density: 1.0, auto: None };
        assert!(Prepared::new(&m, 600, 400).is_err(), "the reference path keeps its limit");
        let w = painted(&m, View { full: [600, 400], offset: [0, 0] }, [600, 400]);
        assert!(w.iter().any(|v| *v > 0.1));
    }
}

#[cfg(test)]
mod old_vs_new_timing {
    use super::*;
    /// The former way (every pixel sums the dabs of its tile) against the painted raster, same stroke, same run.
    #[test]
    #[ignore = "24 MP brush timing, former way"]
    fn wild_stroke_old_and_new() {
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32) / (1u64 << 31) as f32
        };
        let (mut p, mut angle) = ([0.5f32, 0.5], 0.0f32);
        let points: Vec<[f32; 2]> = (0..20_000)
            .map(|_| {
                angle += (next() - 0.5) * 2.5;
                let step = 0.001 + 0.008 * next();
                p[0] += angle.cos() * step;
                p[1] += angle.sin() * step;
                for v in &mut p {
                    if !(0.1..=0.9).contains(v) {
                        *v = v.clamp(0.1, 0.9);
                        angle += std::f32::consts::PI;
                    }
                }
                p
            })
            .collect();
        let (w, h) = (6000usize, 4000usize);
        let data = vec![0.18f32; w * h * 3];
        let image = Image::new(w, h, &data).unwrap();
        let view = View::whole(&image);
        for radius in [0.04f32, 0.01] {
            let m = Mask::Brush { points: points.clone(), radius, hardness: 0.6, flow: 0.2, density: 1.0, auto: None };
            let t = std::time::Instant::now();
            let old = Prepared::new(&m, w, h).unwrap();
            let a: Vec<f32> = (0..w * h).into_par_iter().map(|i| old.sample(view.point(i % w, i / w), &[0.18; 3])).collect();
            let t_old = t.elapsed();
            let t = std::time::Instant::now();
            let b = rasterize(image, &m).unwrap();
            let t_new = t.elapsed();
            let d = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
            println!("radius {radius}: former way {t_old:.2?}, painted raster {t_new:.2?}, largest difference {d:e}");
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
            density: 1.0,
            auto: None,
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
