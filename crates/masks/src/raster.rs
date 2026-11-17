//! Raster (bitmap) masks: an 8-bit weight map 0..1 of fixed resolution, immutable and named by its content hash
//! (`sha256:<hex>`). The mask tree only holds the name and the rule that made the raster ([`RasterSource`]); the data
//! live elsewhere (the library) and come in through a resolver set once by the program ([`set_provider`]). This crate
//! still has no file access: without a resolver, or when a raster is missing, the leaf weighs 0, never 1.
//!
//! The raster lies in the mask space like the vector masks (sensor orientation, after lens correction, before crop,
//! rotation, perspective and camera rotation), normalised to 0..1 per axis, so crop and rotation need no extra step.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

/// Which instances (foreground objects or people) of the analysis a raster combines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instances {
    All,
    /// indices in the order of the analysis at creation time (1-based, at most 64)
    Some(Vec<u8>),
}

/// How a raster was made and can be made again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RasterSource {
    /// foreground objects found by the system's image analysis (macOS Vision)
    Subject { instances: Instances },
    /// people found by the system's image analysis (macOS Vision)
    Person { instances: Instances },
    /// click selection with a segmentation model: points x, y (0..1 of the mask space) and +1 (include) / −1 (exclude),
    /// optionally a box `[x0, y0, x1, y1]` around the object (needs document version 4)
    Click {
        points: Vec<[f32; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rect: Option<[f32; 4]>,
    },
    /// a part of the people found by the system's image analysis (face landmarks and person segmentation, macOS);
    /// needs document version 4
    PersonPart { part: PersonPart },
    /// raster without a rule (cannot be computed again)
    Painted,
    /// whole segments of the photo chosen by their depth: the raster is a 16-bit segment map ([`DepthData`], see
    /// [`segment_label`]: depth class and number of each segment); a segment belongs to the mask when its class is in
    /// `classes`, the segments in `toggled` are flipped (clicked in or out). Never a range of depths with a gradual
    /// transition: a segment is in or out, the soft edge is spatial only. `model`: where the depth came from
    /// ("embedded" = depth map stored in the photo, otherwise the id of the depth module). Needs document version 4.
    Depth {
        model: String,
        #[serde(default)]
        classes: Vec<DepthClass>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        toggled: Vec<u16>,
    },
    /// a part of the scene found automatically ("sky"); needs document version 4
    Scene { part: String },
}

/// Depth class of a segment: the median of its nearness ranks in the nearest, middle or farthest third of the photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepthClass {
    Near,
    Middle,
    Far,
}

impl DepthClass {
    pub const ALL: [DepthClass; 3] = [DepthClass::Near, DepthClass::Middle, DepthClass::Far];
    /// The class of a median nearness rank (0 = nearest … 65535 = farthest; ranks are spread evenly over the pixels,
    /// so the thirds are fixed quantiles of the photo).
    pub fn of_rank(rank: u16) -> Self {
        match rank {
            0..=21844 => Self::Near,
            21845..=43689 => Self::Middle,
            _ => Self::Far,
        }
    }
    fn index(self) -> u16 {
        self as u16
    }
}

/// Bits of a segment label that number the segment (the two bits above hold the depth class).
pub const SEGMENT_BITS: u32 = 14;
/// Most segments of a segment map.
pub const SEGMENTS_MAX: usize = (1 << SEGMENT_BITS) - 1;
/// Most toggled segments of a depth leaf.
pub const TOGGLED_MAX: usize = 4096;

/// Label of segment number `id` (< [`SEGMENTS_MAX`]) of depth class `class` in a segment map.
pub fn segment_label(class: DepthClass, id: u16) -> u16 {
    (class.index() << SEGMENT_BITS) | (id & SEGMENTS_MAX as u16)
}

/// Depth class and number of a segment label (`None`: not a label of a segment map).
pub fn split_label(label: u16) -> Option<(DepthClass, u16)> {
    let class = match label >> SEGMENT_BITS {
        0 => DepthClass::Near,
        1 => DepthClass::Middle,
        2 => DepthClass::Far,
        _ => return None,
    };
    Some((class, label & SEGMENTS_MAX as u16))
}

/// Does a segment with `label` belong to the selection (`classes`, flipped by `toggled`)?
pub fn segment_selected(label: u16, classes: &[DepthClass], toggled: &[u16]) -> bool {
    split_label(label).is_some_and(|(c, id)| classes.contains(&c) != toggled.contains(&id))
}

/// Parts of a scene that [`RasterSource::Scene`] knows.
pub const SCENE_PARTS: &[&str] = &["sky"];

/// Parts of people that the system's face landmarks give (no hair, body skin or clothing: those need a face-parsing
/// model).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonPart {
    /// the face from the chin to the forehead (landmark hull, cut to the person)
    Face,
    /// the skin of the face: the face without eyes, eyebrows and mouth, by the colour of the face
    Skin,
    Eyes,
    Brows,
    Lips,
    /// the opening of the mouth (teeth)
    Mouth,
}

impl PersonPart {
    pub const ALL: [PersonPart; 6] = [PersonPart::Face, PersonPart::Skin, PersonPart::Eyes, PersonPart::Brows, PersonPart::Lips, PersonPart::Mouth];
}

impl RasterSource {
    /// Can the raster be computed again from the rule on another photo (paste, sync)? Click points belong to the
    /// subject of one photo, a painted raster has no rule.
    pub fn transferable(&self) -> bool {
        matches!(self, Self::Subject { .. } | Self::Person { .. } | Self::PersonPart { .. } | Self::Depth { .. } | Self::Scene { .. })
    }
    /// Does the rule need document version 4 (sources added with it)?
    pub fn needs_version_4(&self) -> bool {
        matches!(self, Self::Depth { .. } | Self::Scene { .. } | Self::PersonPart { .. })
    }
    /// Is the raster 16 bit ([`DepthData`]: a segment map)?
    pub fn wide(&self) -> bool {
        matches!(self, Self::Depth { .. })
    }
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Subject { instances } | Self::Person { instances } => match instances {
                Instances::All => true,
                Instances::Some(v) => !v.is_empty() && v.len() <= 64 && v.iter().all(|&i| (1..=64).contains(&i)),
            },
            Self::Click { points, rect } => {
                (!points.is_empty() || rect.is_some())
                    && rect.is_none_or(|r| r.iter().all(|v| (0.0..=1.0).contains(v)) && (r[2] - r[0]).abs() > 1e-4 && (r[3] - r[1]).abs() > 1e-4)
                    && points.len() <= 64
                    && points.iter().all(|p| (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]) && (p[2] == 1.0 || p[2] == -1.0))
            }
            Self::PersonPart { .. } | Self::Painted => true,
            Self::Depth { model, classes, toggled } => {
                !model.is_empty()
                    && model.len() <= 32
                    && model.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && classes.len() <= DepthClass::ALL.len()
                    && classes.iter().enumerate().all(|(i, c)| !classes[..i].contains(c))
                    && toggled.len() <= TOGGLED_MAX
                    && toggled.iter().all(|&t| (t as usize) < SEGMENTS_MAX)
            }
            Self::Scene { part } => SCENE_PARTS.contains(&part.as_str()),
        }
    }
}

/// A 16-bit raster (a segment map of a depth mask, see [`segment_label`]), row-major, `width × height`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepthData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u16>,
}

impl DepthData {
    /// Content name: SHA-256 over "u16", width, height (little endian u32) and the values (little endian u16); never
    /// equal to the name of an 8-bit raster.
    pub fn id(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"u16");
        h.update(self.width.to_le_bytes());
        h.update(self.height.to_le_bytes());
        // the values as little-endian bytes, in blocks (one update per value was most of the time)
        let mut block = Vec::with_capacity(64 * 1024);
        for c in self.data.chunks(32 * 1024) {
            block.clear();
            block.extend(c.iter().flat_map(|v| v.to_le_bytes()));
            h.update(&block);
        }
        let mut s = String::with_capacity(71);
        s.push_str("sha256:");
        for b in h.finalize().iter() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
    pub fn valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.width <= 16384 && self.height <= 16384 && self.data.len() == self.width as usize * self.height as usize
    }
}

type DepthProvider = Box<dyn Fn(&str) -> Option<Arc<DepthData>> + Send + Sync>;
static DEPTH_PROVIDER: RwLock<Option<DepthProvider>> = RwLock::new(None);
/// 16-bit rasters held in memory with their name (few and recent, oldest first: a depth map of 2048 px is 5–6 MB).
/// A lookup compares names only; the name is computed once, when a map comes in.
static DEPTH_MEMORY: Mutex<Vec<(String, Arc<DepthData>)>> = Mutex::new(Vec::new());
const DEPTH_MEMORY_MAPS: usize = 6;

fn keep_depth(id: String, data: Arc<DepthData>) {
    let mut g = DEPTH_MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|(i, _)| *i != id);
    g.push((id, data));
    if g.len() > DEPTH_MEMORY_MAPS {
        g.remove(0);
    }
}

/// Set the resolver of 16-bit rasters (the program: from the library).
pub fn set_depth_provider(f: impl Fn(&str) -> Option<Arc<DepthData>> + Send + Sync + 'static) {
    *DEPTH_PROVIDER.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

/// Keep a 16-bit raster in memory; returns its name.
pub fn remember_depth(data: Arc<DepthData>) -> String {
    let id = data.id();
    keep_depth(id.clone(), data);
    id
}

/// The 16-bit raster named `id` (memory first, then the resolver).
pub fn resolve_depth(id: &str) -> Option<Arc<DepthData>> {
    if id.is_empty() {
        return None;
    }
    if let Some((_, d)) = DEPTH_MEMORY.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(i, _)| i == id) {
        return Some(d.clone());
    }
    let found = DEPTH_PROVIDER.read().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|p| p(id))?;
    if !found.valid() || found.id() != id {
        return None;
    }
    keep_depth(id.to_string(), found.clone());
    Some(found)
}

/// Is the raster of a leaf with this rule there (8 or 16 bit)?
pub fn present(id: &str, source: &RasterSource) -> bool {
    if source.wide() { resolve_depth(id).is_some() } else { resolve(id).is_some() }
}

/// The data of one raster: 8-bit weights, row-major, `width × height`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RasterData {
    /// Content name of the raster: SHA-256 over width, height (little endian u32) and the weights.
    pub fn id(&self) -> String {
        {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.width.to_le_bytes());
        h.update(self.height.to_le_bytes());
        h.update(&self.data);
        let d = h.finalize();
        let mut s = String::with_capacity(71);
        s.push_str("sha256:");
        for b in d.iter() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
    }
    pub fn valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.width <= 16384 && self.height <= 16384 && self.data.len() == self.width as usize * self.height as usize
    }
    /// Weights from values 0..1 (clamped).
    pub fn from_weights(width: u32, height: u32, w: &[f32]) -> Self {
        Self { width, height, data: w.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect() }
    }
}

/// Is `id` the name of a raster (`sha256:` + 64 lower-case hex digits) or the empty name of a pending leaf?
pub fn valid_id(id: &str) -> bool {
    id.is_empty() || id.strip_prefix("sha256:").is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

type Provider = Box<dyn Fn(&str) -> Option<Arc<RasterData>> + Send + Sync>;
static PROVIDER: RwLock<Option<Provider>> = RwLock::new(None);

/// Rasters held in memory (just computed, or recently resolved), at most [`MEMORY_BYTES`].
struct Memory {
    map: HashMap<String, (Arc<RasterData>, u64)>,
    tick: u64,
    bytes: usize,
}
static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);
const MEMORY_BYTES: usize = 64 << 20;

/// Set the resolver that loads a raster by its name (the program: from the library). Replaces an earlier one.
pub fn set_provider(f: impl Fn(&str) -> Option<Arc<RasterData>> + Send + Sync + 'static) {
    *PROVIDER.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

/// Keep a raster in memory (e.g. right after computing it, before it reached the library).
pub fn remember(data: Arc<RasterData>) -> String {
    let id = data.id();
    let mut g = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(|| Memory { map: HashMap::new(), tick: 0, bytes: 0 });
    m.tick += 1;
    let tick = m.tick;
    if let Some(e) = m.map.get_mut(&id) {
        e.1 = tick;
        return id;
    }
    m.bytes += data.data.len();
    m.map.insert(id.clone(), (data, tick));
    while m.bytes > MEMORY_BYTES && m.map.len() > 1 {
        let Some(old) = m.map.iter().min_by_key(|e| e.1 .1).map(|e| e.0.clone()) else { break };
        if let Some((d, _)) = m.map.remove(&old) {
            m.bytes -= d.data.len();
        }
    }
    id
}

/// The raster named `id` (memory first, then the resolver); `None`: missing or pending.
pub fn resolve(id: &str) -> Option<Arc<RasterData>> {
    if id.is_empty() {
        return None;
    }
    {
        let mut g = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_mut() {
            m.tick += 1;
            let tick = m.tick;
            if let Some(e) = m.map.get_mut(id) {
                e.1 = tick;
                return Some(e.0.clone());
            }
        }
    }
    let found = PROVIDER.read().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|p| p(id))?;
    if !found.valid() || found.id() != id {
        return None;
    }
    remember(found.clone());
    Some(found)
}

/// Prepared raster for sampling: weights 0..1 as f32, softened by `feather` (share of the short side).
pub(crate) struct Sampler {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

/// Softened rasters already prepared (name, feather and edge in 1/10000 of the short side, fingerprint of a segment
/// selection) → weights; small, recent only.
static SOFTENED: Mutex<Vec<((String, u32, i32, u64), Arc<Sampler>)>> = Mutex::new(Vec::new());

impl Sampler {
    /// `full`: pixels of the image the mask is rendered for; a raster whose aspect ratio deviates by more than 2 %
    /// belongs to another image and is not used (weight 0). `edge` (share of the short side) grows (> 0) or shrinks
    /// (< 0) the weights before `feather` softens them.
    pub(crate) fn new(id: &str, feather: f32, edge: f32, full: [usize; 2]) -> Option<Arc<Self>> {
        Self::make(id, feather, edge, full, None)
    }

    /// A segment map ([`DepthData`]): weight 1 for the selected segments, 0 for the others (then edge and feather as
    /// for any raster: the soft edge is spatial only).
    pub(crate) fn new_segments(id: &str, classes: &[DepthClass], toggled: &[u16], feather: f32, edge: f32, full: [usize; 2]) -> Option<Arc<Self>> {
        Self::make(id, feather, edge, full, Some((classes, toggled)))
    }

    fn make(id: &str, feather: f32, edge: f32, full: [usize; 2], pick: Option<(&[DepthClass], &[u16])>) -> Option<Arc<Self>> {
        let (w, h, values): (usize, usize, Box<dyn Fn() -> Vec<f32>>) = match pick {
            Some((classes, toggled)) => {
                let r = resolve_depth(id)?;
                let (classes, toggled) = (classes.to_vec(), toggled.to_vec());
                // one look-up per label instead of a search per pixel
                let mut table = vec![0.0f32; 1 << 16];
                for (label, v) in table.iter_mut().enumerate() {
                    *v = if segment_selected(label as u16, &classes, &toggled) { 1.0 } else { 0.0 };
                }
                (r.width as usize, r.height as usize, Box::new(move || r.data.iter().map(|&v| table[v as usize]).collect()))
            }
            None => {
                let r = resolve(id)?;
                (r.width as usize, r.height as usize, Box::new(move || r.data.iter().map(|&v| v as f32 / 255.0).collect()))
            }
        };
        let a = w as f32 / h as f32;
        let b = full[0] as f32 / full[1].max(1) as f32;
        if ((a - b) / b).abs() > 0.02 {
            return None;
        }
        let fingerprint = pick.map_or(0, |(c, t)| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (c, t).hash(&mut h);
            h.finish() | 1
        });
        let key = (id.to_string(), (feather * 10000.0).round() as u32, (edge * 10000.0).round() as i32, fingerprint);
        if let Some(s) = SOFTENED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.0 == key) {
            return Some(s.1.clone());
        }
        let mut data: Vec<f32> = values();
        let shift = (edge.abs() * w.min(h) as f32).round() as usize;
        if shift > 0 {
            morph(&mut data, w, h, shift, edge > 0.0);
        }
        let radius = (feather * w.min(h) as f32 / 2.0).round() as usize;
        if radius > 0 {
            // three box passes ≈ Gaussian with σ ≈ radius
            for _ in 0..3 {
                box_rows(&mut data, w, h, radius);
                box_cols(&mut data, w, h, radius);
            }
        }
        let s = Arc::new(Self { width: w, height: h, data });
        let mut g = SOFTENED.lock().unwrap_or_else(|e| e.into_inner());
        g.push((key, s.clone()));
        if g.len() > 4 {
            g.remove(0);
        }
        Some(s)
    }

    /// Bilinear weight at the normalised point `p` (pixel centres at (i + 0.5) / size, clamped at the edges).
    pub(crate) fn sample(&self, p: [f32; 2]) -> f32 {
        let x = (p[0] * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (p[1] * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| self.data[y * self.width + x];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
        top + (bottom - top) * ty
    }
}

/// Grey-value dilation (`grow`) or erosion of `d` (`w × h`) with an octagon of radius `r` pixels, close to a disc: a
/// square of radius ⌊0.414·r⌉ (running maximum or minimum along rows and columns, cost independent of `r`) followed by
/// ⌊0.586·r⌉ steps of the 3×3 cross. Pixels outside the raster do not count (the edge of the image neither grows nor
/// eats into the mask).
pub(crate) fn morph(d: &mut [f32], w: usize, h: usize, r: usize, grow: bool) {
    use rayon::prelude::*;
    let pick = move |a: f32, b: f32| if grow { a.max(b) } else { a.min(b) };
    let square = (r as f32 * 0.414).round() as usize;
    let cross = r - square.min(r);
    if square > 0 {
        d.par_chunks_mut(w).for_each_init(Vec::new, |tmp, row| running(row, tmp, square, grow));
        let mut t = transpose(d, w, h);
        t.par_chunks_mut(h).for_each_init(Vec::new, |tmp, col| running(col, tmp, square, grow));
        d.copy_from_slice(&transpose(&t, h, w));
    }
    let mut src = d.to_vec();
    for _ in 0..cross {
        d.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let line = &src[y * w..(y + 1) * w];
            let up = (y > 0).then(|| &src[(y - 1) * w..y * w]);
            let down = (y + 1 < h).then(|| &src[(y + 1) * w..(y + 2) * w]);
            for x in 0..w {
                let mut v = line[x];
                if x > 0 {
                    v = pick(v, line[x - 1]);
                }
                if x + 1 < w {
                    v = pick(v, line[x + 1]);
                }
                if let Some(u) = up {
                    v = pick(v, u[x]);
                }
                if let Some(u) = down {
                    v = pick(v, u[x]);
                }
                row[x] = v;
            }
        });
        src.copy_from_slice(d);
    }
}

/// Running maximum (`grow`) or minimum over the window `i − r … i + r` (clipped at the ends), van Herk / Gil-Werman:
/// prefix and suffix extrema per block of `2r + 1`.
fn running(line: &mut [f32], tmp: &mut Vec<f32>, r: usize, grow: bool) {
    let n = line.len();
    let k = 2 * r + 1;
    let pick = |a: f32, b: f32| if grow { a.max(b) } else { a.min(b) };
    let none = if grow { f32::NEG_INFINITY } else { f32::INFINITY };
    // padded by r on both sides with the neutral element, so the window never needs clipping
    let m = n + 2 * r;
    let at = |i: usize| if i < r || i >= r + n { none } else { line[i - r] };
    tmp.clear();
    tmp.resize(2 * m, none);
    let (pre, suf) = tmp.split_at_mut(m);
    for i in 0..m {
        pre[i] = if i % k == 0 { at(i) } else { pick(pre[i - 1], at(i)) };
    }
    for i in (0..m).rev() {
        suf[i] = if i % k == k - 1 || i == m - 1 { at(i) } else { pick(suf[i + 1], at(i)) };
    }
    for (i, out) in line.iter_mut().enumerate() {
        // window in padded coordinates: i … i + 2r
        *out = pick(suf[i], pre[i + 2 * r]);
    }
}

fn transpose(d: &[f32], w: usize, h: usize) -> Vec<f32> {
    use rayon::prelude::*;
    let mut t = vec![0.0; d.len()];
    t.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = d[y * w + x];
        }
    });
    t
}

fn box_line(line: &mut [f32], tmp: &mut Vec<f32>, r: usize) {
    let n = line.len();
    tmp.clear();
    tmp.extend_from_slice(line);
    let at = |i: isize| tmp[i.clamp(0, n as isize - 1) as usize];
    let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
    let k = (2 * r + 1) as f32;
    for i in 0..n {
        line[i] = sum / k;
        sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
    }
}

pub(crate) fn box_rows(d: &mut [f32], w: usize, _h: usize, r: usize) {
    use rayon::prelude::*;
    d.par_chunks_mut(w).for_each_init(Vec::new, |tmp, row| box_line(row, tmp, r));
}

pub(crate) fn box_cols(d: &mut [f32], w: usize, h: usize, r: usize) {
    let mut col = vec![0.0; h];
    let mut tmp = Vec::new();
    for x in 0..w {
        for y in 0..h {
            col[y] = d[y * w + x];
        }
        box_line(&mut col, &mut tmp, r);
        for y in 0..h {
            d[y * w + x] = col[y];
        }
    }
}

/// Weight of a value with an optional window `[low, high, feather]` (0 outside, 1 inside, smooth edges).
pub(crate) fn window(v: f32, range: &Option<[f32; 3]>) -> f32 {
    match range {
        None => v,
        Some([low, high, feather]) => {
            if v < *low {
                crate::falloff(low - v, 0.0, *feather)
            } else if v > *high {
                crate::falloff(v - high, 0.0, *feather)
            } else {
                1.0
            }
        }
    }
}

/// Names of all raster leaves of a document (pending leaves give "").
pub fn ids(doc: &crate::Document) -> Vec<String> {
    let mut out = Vec::new();
    for l in &doc.layers {
        l.mask.visit(&mut |m| {
            if let crate::Mask::Raster { raster, .. } = m {
                out.push(raster.clone());
            }
        });
    }
    out
}

/// Raster leaves of a document that are pending or whose raster cannot be found (for the status card).
pub fn missing(doc: &crate::Document) -> usize {
    let mut n = 0;
    for l in &doc.layers {
        l.mask.visit(&mut |m| {
            if let crate::Mask::Raster { raster, source, .. } = m {
                n += usize::from(!present(raster, source));
            }
        });
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_content_hash() {
        let a = RasterData { width: 2, height: 1, data: vec![0, 255] };
        let b = RasterData { width: 1, height: 2, data: vec![0, 255] };
        assert!(valid_id(&a.id()));
        assert_ne!(a.id(), b.id(), "same bytes, other shape → other name");
        assert!(valid_id(""));
        assert!(!valid_id("sha256:XYZ"));
        assert!(!valid_id("/etc/passwd"));
    }

    fn depth(model: &str, classes: &[DepthClass], toggled: &[u16]) -> RasterSource {
        RasterSource::Depth { model: model.into(), classes: classes.to_vec(), toggled: toggled.to_vec() }
    }

    #[test]
    fn segment_maps_are_16_bit_with_their_own_names() {
        // two segments: number 0 near (left), number 1 far (right)
        let (near, far) = (segment_label(DepthClass::Near, 0), segment_label(DepthClass::Far, 1));
        let d = Arc::new(DepthData { width: 2, height: 1, data: vec![near, far] });
        let r = RasterData { width: 2, height: 1, data: vec![0, 255] };
        assert_ne!(d.id(), r.id());
        assert!(valid_id(&d.id()));
        let id = remember_depth(d);
        let s = Sampler::new_segments(&id, &[DepthClass::Near], &[], 0.0, 0.0, [200, 100]).unwrap();
        assert_eq!((s.sample([0.0, 0.5]), s.sample([1.0, 0.5])), (1.0, 0.0));
        // the far segment clicked in, the near one clicked out
        let s = Sampler::new_segments(&id, &[DepthClass::Near], &[0, 1], 0.0, 0.0, [200, 100]).unwrap();
        assert_eq!((s.sample([0.0, 0.5]), s.sample([1.0, 0.5])), (0.0, 1.0));
        // between the two segments a spatial transition, never a depth-dependent one
        assert!((s.sample([0.5, 0.5]) - 0.5).abs() < 1e-6);
        assert!(present(&id, &depth("depth", &[], &[])));
        assert!(!present(&id, &RasterSource::Painted));
    }

    #[test]
    fn labels_carry_class_and_number() {
        for c in DepthClass::ALL {
            for id in [0u16, 1, 77, SEGMENTS_MAX as u16 - 1] {
                assert_eq!(split_label(segment_label(c, id)), Some((c, id)));
            }
        }
        assert_eq!(split_label(0xC000), None);
        assert_eq!((DepthClass::of_rank(0), DepthClass::of_rank(21844), DepthClass::of_rank(21845)), (DepthClass::Near, DepthClass::Near, DepthClass::Middle));
        assert_eq!((DepthClass::of_rank(43689), DepthClass::of_rank(43690), DepthClass::of_rank(65535)), (DepthClass::Middle, DepthClass::Far, DepthClass::Far));
        let l = segment_label(DepthClass::Middle, 5);
        assert!(segment_selected(l, &[DepthClass::Middle], &[]));
        assert!(!segment_selected(l, &[DepthClass::Middle], &[5]));
        assert!(segment_selected(l, &[], &[5]));
        assert!(!segment_selected(0xFFFF, &DepthClass::ALL, &[]));
    }

    #[test]
    fn new_sources_validate() {
        assert!(depth("depth", &[DepthClass::Near], &[]).valid());
        assert!(depth("embedded", &DepthClass::ALL, &[3, 9]).valid());
        assert!(!depth("", &[], &[]).valid());
        assert!(!depth("../x", &[], &[]).valid());
        assert!(!depth("depth", &[DepthClass::Near, DepthClass::Near], &[]).valid(), "a class twice");
        assert!(!depth("depth", &[], &[SEGMENTS_MAX as u16]).valid());
        assert!(RasterSource::Scene { part: "sky".into() }.valid());
        assert!(!RasterSource::Scene { part: "moon".into() }.valid());
        assert!(depth("depth", &[], &[]).transferable() && RasterSource::Scene { part: "sky".into() }.transferable());
    }

    #[test]
    fn sampling_is_bilinear_and_feather_softens() {
        let r = Arc::new(RasterData { width: 4, height: 4, data: (0..16).map(|i| if i % 4 >= 2 { 255 } else { 0 }).collect() });
        let id = remember(r);
        let s = Sampler::new(&id, 0.0, 0.0, [400, 400]).unwrap();
        assert_eq!(s.sample([0.1, 0.5]), 0.0);
        assert_eq!(s.sample([0.9, 0.5]), 1.0);
        assert!((s.sample([0.5, 0.5]) - 0.5).abs() < 1e-6);
        // other aspect ratio: not this image's raster
        assert!(Sampler::new(&id, 0.0, 0.0, [600, 400]).is_none());
        // an 8-bit name is no 16-bit raster
        assert!(Sampler::new_segments(&id, &DepthClass::ALL, &[], 0.0, 0.0, [400, 400]).is_none());
        let soft = Sampler::new(&id, 0.5, 0.0, [400, 400]).unwrap();
        let v = soft.sample([0.9, 0.5]);
        assert!(v < 1.0 && v > 0.5, "{v}");
    }

    /// Brute-force reference: maximum / minimum over the octagon the decomposition builds (square of radius a, then b
    /// steps of the cross = all offsets with |dx| ≤ a + i, |dy| ≤ a + j, i + j ≤ b).
    fn reference(d: &[f32], w: usize, h: usize, r: usize, grow: bool) -> Vec<f32> {
        let a = (r as f32 * 0.414).round() as i64;
        let b = r as i64 - a;
        let mut out = vec![0.0; d.len()];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let mut v = d[(y * w as i64 + x) as usize];
                for dy in -(a + b)..=(a + b) {
                    for dx in -(a + b)..=(a + b) {
                        let ex = (dx.abs() - a).max(0);
                        let ey = (dy.abs() - a).max(0);
                        let (xx, yy) = (x + dx, y + dy);
                        if ex + ey > b || xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                            continue;
                        }
                        let q = d[(yy * w as i64 + xx) as usize];
                        v = if grow { v.max(q) } else { v.min(q) };
                    }
                }
                out[(y * w as i64 + x) as usize] = v;
            }
        }
        out
    }

    #[test]
    fn morphology_matches_the_octagon_and_is_close_to_a_disc() {
        let (w, h) = (37, 29);
        let d: Vec<f32> = (0..w * h).map(|i| ((i * 7919) % 101) as f32 / 100.0).collect();
        for r in [1, 2, 3, 5, 8] {
            for grow in [true, false] {
                let mut m = d.clone();
                morph(&mut m, w, h, r, grow);
                assert_eq!(m, reference(&d, w, h, r, grow), "r {r} grow {grow}");
            }
        }
        // one lit pixel grows into an octagon close to a disc of radius 10: on the axes and the diagonal at about
        // the radius, never in the corner of the square
        let (w, h) = (41, 41);
        let mut dot = vec![0.0; w * h];
        dot[20 * w + 20] = 1.0;
        morph(&mut dot, w, h, 10, true);
        let at = |x: usize, y: usize| dot[y * w + x];
        assert_eq!((at(30, 20), at(31, 20), at(20, 10), at(20, 9)), (1.0, 0.0, 1.0, 0.0));
        assert_eq!(at(27, 27), 1.0, "diagonal at 9.9 px");
        assert_eq!(at(29, 29), 0.0, "corner of the square at 12.7 px");
        // erosion undoes the dilation of a large shape away from its corners, and shrinks it by r
        let mut sq = vec![0.0; w * h];
        for y in 10..31 {
            for x in 10..31 {
                sq[y * w + x] = 1.0;
            }
        }
        let mut e = sq.clone();
        morph(&mut e, w, h, 3, false);
        assert_eq!((at_(&e, w, 13, 20), at_(&e, w, 12, 20)), (1.0, 0.0));
        let mut g = sq.clone();
        morph(&mut g, w, h, 3, true);
        assert_eq!((at_(&g, w, 7, 20), at_(&g, w, 6, 20)), (1.0, 0.0));
        // the image border does not eat into a mask that touches it
        let mut full = vec![1.0; w * h];
        morph(&mut full, w, h, 4, false);
        assert!(full.iter().all(|v| *v == 1.0));
    }

    fn at_(d: &[f32], w: usize, x: usize, y: usize) -> f32 {
        d[y * w + x]
    }

    #[test]
    fn edge_grows_and_shrinks_the_sampled_raster() {
        // a disc of radius 100 px in a 400 × 400 raster
        let data = (0..400 * 400).map(|i| if (((i % 400) as f32 - 199.5).powi(2) + ((i / 400) as f32 - 199.5).powi(2)).sqrt() < 100.0 { 255 } else { 0 }).collect();
        let id = remember(Arc::new(RasterData { width: 400, height: 400, data }));
        let at = |s: &Sampler, x: f32| s.sample([(x + 0.5) / 400.0, 0.5 - 0.5 / 400.0 + 0.5 / 400.0]);
        let plain = Sampler::new(&id, 0.0, 0.0, [800, 800]).unwrap();
        let grown = Sampler::new(&id, 0.0, 0.025, [800, 800]).unwrap();
        let shrunk = Sampler::new(&id, 0.0, -0.025, [800, 800]).unwrap();
        // 2.5 % of the short side of the raster = 10 px
        assert_eq!((at(&plain, 302.0), at(&grown, 302.0), at(&grown, 310.0)), (0.0, 1.0, 0.0));
        assert_eq!((at(&plain, 295.0), at(&shrunk, 295.0), at(&shrunk, 285.0)), (1.0, 0.0, 1.0));
    }
}
