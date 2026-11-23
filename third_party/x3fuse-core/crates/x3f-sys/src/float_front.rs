//! Native float-image ApplyGains kernel for the ordered Merrill float
//! frontend. Verified bit-for-bit against the supplied Merrill library
//! through an archived differential oracle.
//!
//! The operation is
//! `out = f32((f64(sample) - black) * (gain * spatial_value))`
//! with no display clamp and no gamma. It works on sensor-count float
//! samples before any normalization. The ordered frontend follow-up
//! wires this kernel into the conversion path; production still uses
//! the normalized uint16 frontend.

use crate::control::{Error, Result};
use crate::Control;

pub mod repair;
/// graineo addition: highlight reconstruction of our own ([`HighlightRestoration::restore_own`]).
mod propagate;
/// graineo addition: own outlier detection (`GRAINEO_MERRILL_NEW=outliers`).
mod speckle_own;
/// graineo addition: second own highlight reconstruction (`GRAINEO_MERRILL_NEW=highlights`).
mod opposed;

/// One channel of the spatial gain table. `values` is row-major with
/// `rows` rows and `columns` knots per row. The CAMF builders in
/// `spatial_gain.rs` produce this layout.
///
/// A hand-built table with public fields bypasses the `new`
/// shape checks; `apply_gains` indexes `values` unchecked, and
/// panics on a shape mismatch.
#[derive(Clone, Debug)]
pub struct GainTable {
    pub values: Vec<f32>,
    pub columns: usize,
    pub rows: usize,
}

impl GainTable {
    pub fn new(values: Vec<f32>, columns: usize, rows: usize) -> Self {
        assert_eq!(values.len(), columns * rows, "gain table shape mismatch");
        assert!(
            columns >= 2 && rows >= 2,
            "gain table needs two knots per axis"
        );
        Self {
            values,
            columns,
            rows,
        }
    }
}

/// FNV-1a over the little-endian f32 bits of the raster (the
/// recorded hashes use `to_le_bytes` order; this platform is
/// little-endian). Matches the archived native-oracle hashes.
pub(crate) fn fnv1a_f32(values: &[f32]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for value in values {
        for byte in value.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// Native WB radial shading for the ordered Merrill float frontend,
/// stage 2. Reconstructed from the native preprocess step (geometry
/// first, then the shading arithmetic). The pass
/// runs over the gained float raster after ApplyGains, in
/// `activeMinusKeep` coordinates. C1 stays unchanged.
///
/// Geometry:
/// - `activeMinusKeep = ActiveImageArea - KeepImageArea` component-wise
///   (top and bottom both subtract `keep.top`), a half-open rect.
/// - `margin = 32` when the resolution string contains "HI", else `16`
///   (native computes `scale * +-32` with scale 1.0 or 0.5, then
///   truncates toward zero).
/// - `valid.left = DarkShieldColRange[1] + 6`,
///   `valid.right = DarkShieldColRange[2] - 8` (matrix elements 1 and
///   2 of the 2x2 entry), `valid.top/bottom = activeMinusKeep.top/bottom
///   -/+ margin`.
/// - Half-extents truncate toward zero: `(right - left)` with a `+1`
///   correction when negative, then arithmetic shift by one.
/// - Center `= (valid.left + halfX, valid.top + halfY)`.
///
/// Arithmetic, exact native order:
/// - Denominator `sqrt(contains_MED ? fma(2*halfY, 2*halfY, halfX*halfX)
///   : fma(halfY, halfY, halfX*halfX))`: the stretch literal is "MED",
///   selected by a substring test on the resolution string.
/// - Per pixel `dx = x - centerX`, `dy = y - centerY`,
///   `r2 = sqrt(stretch ? fma(2dy, 2dy, dx*dx) : fma(dy, dy, dx*dx)) /
///   denom`, `t = r2 * r2`, `t2 = t * t`.
/// - `factorC0 = f32(fma(a, t2, t*b) + 1.0)`, `factorC2 = f32(fma(c,
///   t2, t*d) + 1.0)`; each multiplies its f32 channel in place.
#[derive(Clone, Debug)]
pub struct RadialShading {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    center_x: f64,
    center_y: f64,
    denom: f64,
    stretch_y: bool,
}

impl RadialShading {
    /// The inclusive ValidPixRect, shared by the
    /// radial and highlight stages: translated activeMinusKeep bounds,
    /// HI 32 / non-HI 16 margin, DarkShieldColRange[1]+6 and [2]-8
    /// horizontal bounds. The rect must carry a two-pixel halo.
    pub fn valid_pix_rect(
        active: [i32; 4],
        keep: [i32; 4],
        col_range: [i32; 4],
        resolution: &str,
    ) -> [i32; 4] {
        // 4-wide SIMD subtract in the native code, activeMinusKeep = active - keep.
        let minus_keep = [
            active[0] - keep[0],
            active[1] - keep[1],
            active[2] - keep[0],
            active[3] - keep[1],
        ];
        let hi = resolution.contains("HI");
        // Scale 1.0 for HI else 0.5; valid.top/bottom =
        // fcvtzs(i32(activeMinusKeep.top/bottom) fmadd scale * -/+32).
        // The subtraction result is i32; scvtf converts it to f32, the
        // fmadd adds scale*(+-32) in one fused op, fcvtzs truncates.
        let scale = if hi { 1.0_f32 } else { 0.5_f32 };
        let valid_top = scale.mul_add(-32.0, minus_keep[1] as f32) as i32;
        let valid_bottom = scale.mul_add(32.0, minus_keep[3] as f32) as i32;
        let valid_left = col_range[1] + 6;
        let valid_right = col_range[2] - 8;
        [valid_left, valid_top, valid_right, valid_bottom]
    }

    /// Build the stage from CAMF rectangles and coefficients. `active`
    /// and `keep` are half-open camera-file rects; `col_range`
    /// holds the four elements of the DarkShieldColRange 2x2 matrix.
    pub fn new(
        active: [i32; 4],
        keep: [i32; 4],
        col_range: [i32; 4],
        coefficients: [f64; 4],
        resolution: &str,
    ) -> Result<Self> {
        let [valid_left, valid_top, valid_right, valid_bottom] =
            Self::valid_pix_rect(active, keep, col_range, resolution);
        // Half-extent: (right - left) with +1 when negative (truncate toward
        // zero), then arithmetic shift by one.
        let truncate_half = |range: i32| {
            let delta = if range < 0 { range + 1 } else { range };
            delta >> 1
        };
        let half_x = truncate_half(valid_right - valid_left);
        let half_y = truncate_half(valid_bottom - valid_top);
        let [a, b, c, d] = coefficients;
        let stretch_y = resolution.contains("MED");
        let half_x_f = f64::from(half_x);
        let half_y_f = f64::from(half_y);
        let denom = if stretch_y {
            (2.0 * half_y_f)
                .mul_add(2.0 * half_y_f, half_x_f * half_x_f)
                .sqrt()
        } else {
            half_y_f.mul_add(half_y_f, half_x_f * half_x_f).sqrt()
        };
        if [a, b, c, d, denom].iter().any(|v| !v.is_finite()) || denom <= 0.0 {
            return Err(Error::InvalidData("invalid Merrill radial calibration"));
        }
        Ok(Self {
            a,
            b,
            c,
            d,
            center_x: f64::from(valid_left) + f64::from(half_x),
            center_y: f64::from(valid_top) + f64::from(half_y),
            denom,
            stretch_y,
        })
    }

    /// One pixel's factors, native order. Exposed for tests.
    fn factors(&self, x: i32, y: i32) -> (f32, f32) {
        let dx = f64::from(x) - self.center_x;
        let dy = f64::from(y) - self.center_y;
        let squared = dx * dx;
        let unstretched = dy.mul_add(dy, squared);
        let stretched = (dy + dy).mul_add(dy + dy, squared);
        let selected = if self.stretch_y {
            stretched
        } else {
            unstretched
        };
        let radius = selected.sqrt() / self.denom;
        let t = radius * radius;
        let t2 = t * t;
        let bottom = self.a.mul_add(t2, t * self.b) + 1.0;
        let top = self.c.mul_add(t2, t * self.d) + 1.0;
        (bottom as f32, top as f32)
    }

    /// Apply the pass in place over the gained float raster. `data`
    /// holds interleaved RGB f32 samples; `row_stride` may exceed
    /// `width * 3`.
    pub fn apply(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        row_stride: usize,
        control: Control<'_>,
    ) -> Result<()> {
        control.check()?;
        if width == 0 || height == 0 {
            return Err(Error::InvalidData("empty Merrill radial image"));
        }
        if row_stride < width * 3 || data.len() < row_stride * (height - 1) + width * 3 {
            return Err(Error::InvalidData("invalid Merrill radial image layout"));
        }
        for y in 0..height {
            let row = &mut data[y * row_stride..y * row_stride + width * 3];
            for x in 0..width {
                let (bottom, top) = self.factors(x as i32, y as i32);
                let pixel = &mut row[3 * x..3 * x + 3];
                pixel[0] *= bottom;
                pixel[2] *= top;
            }
        }
        Ok(())
    }
}

/// One camera-listed bad-pixel record. Layout matches the native
/// bad-pixel record: row, column, then a packed u16 flags word
/// (bits 0..7 neighbor mask, bits 8..11 badness, bits 12..14 channel
/// mask C0=4/C1=2/C2=1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct BadPixelRecord {
    pub row: u16,
    pub column: u16,
    pub flags: u16,
}

/// Offsets `(dx, dy)` of the eight neighbours in the bit order of the neighbour masks
/// (row by row from the top left, the centre skipped).
const NEIGHBOR_OFFSETS: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

/// graineo: relative deviation of the intact channels from their neighbour median above which a listed
/// pixel with plausible marked channels keeps its values (see [`ListedRepair::replace_one`]).
const LISTED_OWN_STRUCTURE: f32 = 0.15;

/// Median of a short list (sorted in place; mean of the two middle values for an even count).
fn median(values: &mut [f32]) -> f32 {
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n == 0 {
        f32::NAN
    } else if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

/// Stage 3 of the ordered float frontend: listed bad-pixel repair,
/// the scalar reconstruction of the native bad-pixel replacement step.
/// Verified bit-for-bit against the native export over all
/// 35,840 standalone cases (256 neighbor masks x 7 channel masks,
/// badness cutoffs, override bounds, zero/negative finite neighborhoods
/// and geometric skips) plus 8 sequential lists / 1,044 records.
#[derive(Clone, Debug)]
pub struct ListedRepair {
    minimum_badness: u32,
}

impl ListedRepair {
    /// `badness` is the selected badness label; `offset` is the
    /// metadata BPBadnessOffset. The native minimum is unsigned
    /// subtraction `badness == 0 ? 0 : badness - offset`, not
    /// saturating (tested offsets above badness wrap).
    pub fn new(badness: u32, offset: u32) -> Self {
        Self {
            minimum_badness: if badness == 0 {
                0
            } else {
                badness.wrapping_sub(offset)
            },
        }
    }

    pub fn minimum_badness(&self) -> u32 {
        self.minimum_badness
    }

    /// Repair one record in place. Mirrors the native skip conditions:
    /// out-of-halo coordinates (x or y below 2 or at the outermost
    /// border), badness below the request minimum, or an empty
    /// neighbor mask.
    #[allow(clippy::too_many_lines)]
    fn replace_one(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        row_stride: usize,
        record: BadPixelRecord,
        override_mask: Option<u32>,
    ) {
        let x = record.column as usize;
        let y = record.row as usize;
        let channel_mask = (record.flags >> 12) as u32;
        let mut neighbors = u32::from(record.flags & 0xff);
        // The per-record override applies only within the caller's
        // supplied count, when 1 <= override <= 254 and the override is
        // numerically below the original mask.
        if let Some(value) = override_mask {
            if (1..=254).contains(&value) && value < neighbors {
                neighbors = value;
            }
        }
        let badness = u32::from((record.flags >> 8) & 0xf);
        if x < 2
            || y < 2
            || x >= width.saturating_sub(1)
            || y >= height.saturating_sub(1)
            || badness < self.minimum_badness
            || neighbors == 0
        {
            return;
        }
        // graineo: only the channels marked as defective are replaced, each from the same
        // channel of the permitted neighbours (median); the other channels of the pixel stay as they
        // are. No channel is estimated from another one. (Until 0.102 the whole pixel was replaced,
        // which in fine structure also overwrote the intact channels of thousands of listed pixels.)
        if channel_mask & 7 == 0 {
            return;
        }
        let index = y * row_stride + x * 3;
        let mut values = [0.0_f32; 8];
        let gather = |data: &[f32], values: &mut [f32; 8], c: usize| -> usize {
            let mut count = 0;
            for (bit, (dx, dy)) in NEIGHBOR_OFFSETS.iter().enumerate() {
                if neighbors & (1 << bit) != 0 {
                    values[count] = data[(y as i32 + dy) as usize * row_stride + (x as i32 + dx) as usize * 3 + c];
                    count += 1;
                }
            }
            count
        };
        // The pixel carries its own structure in the intact channels (they differ clearly from their
        // neighbour median) and the marked channels still lie within the range of their neighbours:
        // the neighbour median would not fit this pixel and would shift its colour, so it stays as it is.
        if channel_mask & 7 != 7 {
            let mut structure = 0.0_f32;
            let mut plausible = true;
            for c in 0..3 {
                let count = gather(data, &mut values, c);
                let v = &mut values[..count];
                let own = data[index + c];
                if channel_mask & (4 >> c) == 0 {
                    let mid = median(v);
                    structure = structure.max((own - mid).abs() / (mid.abs().max(own.abs()) + 1.0));
                } else {
                    let (lo, hi) = v.iter().fold((f32::MAX, f32::MIN), |(a, b), &q| (a.min(q), b.max(q)));
                    plausible &= (lo..=hi).contains(&own);
                }
            }
            if plausible && structure > LISTED_OWN_STRUCTURE {
                return;
            }
        }
        for c in 0..3 {
            if channel_mask & (4 >> c) == 0 {
                continue;
            }
            let count = gather(data, &mut values, c);
            let value = median(&mut values[..count]);
            if value.is_finite() {
                data[index + c] = value;
            }
        }
    }

    /// Apply a sequential list in place. The native changes the image
    /// between records, so later records observe earlier repairs.
    pub fn apply(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        row_stride: usize,
        records: &[BadPixelRecord],
        overrides: &[u32],
        control: Control<'_>,
    ) -> Result<()> {
        control.check()?;
        if width < 4 || height < 4 {
            // The halo requirement (>=2 and < dim-1) needs at least 4.
            return Err(Error::InvalidData("invalid Merrill repair image size"));
        }
        if row_stride < width * 3 || data.len() < row_stride * (height - 1) + width * 3 {
            return Err(Error::InvalidData("invalid Merrill repair image layout"));
        }
        for (index, record) in records.iter().enumerate() {
            control.check()?;
            let override_mask = overrides.get(index).copied();
            self.replace_one(data, width, height, row_stride, *record, override_mask);
        }
        Ok(())
    }
}

/// Stage 4 of the ordered float frontend: highlight restoration (graineo: the own reconstruction
/// [`Self::restore_own`]).
///
/// The saturation map is packed RGB bytes, `sat[3*(y*width+x)+channel]`,
/// decoded from the camera RLE or Full entries. `dense` is one byte per
/// pixel. `valid` is the inclusive ValidPixRect (the same rect the radial
/// stage uses) and must carry a two-pixel lower/right halo.
/// graineo addition: input of a [`HighlightHook`] (the frontend's buffer after the clip map was cleaned).
pub struct HighlightInput<'a> {
    /// Interleaved layers (bottom, middle, top) per pixel, rows `row_stride` floats apart.
    pub data: &'a mut [f32],
    pub width: usize,
    pub height: usize,
    pub row_stride: usize,
    /// Clipped layers per pixel (bit c = layer c), after removing isolated marks.
    pub clipped: &'a [u8],
    /// Inclusive valid rectangle: left, top, right, bottom.
    pub valid: [usize; 4],
    /// Brightness threshold of the restoration (counts).
    pub threshold: f64,
}

/// graineo addition: replacement of the own highlight reconstruction registered by the application; returns `true`
/// when it handled the image (the own steps are then skipped).
pub type HighlightHook = fn(&mut HighlightInput<'_>) -> bool;

static HIGHLIGHT_HOOK: std::sync::RwLock<Option<HighlightHook>> = std::sync::RwLock::new(None);

/// graineo addition: registers (or removes) the [`HighlightHook`].
pub fn set_highlight_hook(hook: Option<HighlightHook>) {
    if let Ok(mut h) = HIGHLIGHT_HOOK.write() {
        *h = hook;
    }
}

pub(crate) fn highlight_hook() -> Option<HighlightHook> {
    HIGHLIGHT_HOOK.read().ok().and_then(|h| *h)
}

#[derive(Clone, Copy, Debug)]
pub struct HighlightRestoration {
    threshold: f64,
}

/// Statistics of one restoration pass, native counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighlightStats {
    pub donors: u32,
    pub occupied: u32,
    pub removed: u32,
    pub dense_pixels: u32,
    pub have_lut: bool,
}

/// One RLE saturation-map entry: alternating gap and run lengths. The
/// trailing gap word is not consumed. The native decoder takes a
/// second size argument that it ignores; this port drops it.
pub fn decode_sat_map_rle(words: &[u32], sat: &mut [u8], channel: usize) {
    // Native adds u32 positions without bounds; the caller validates the
    // runs against the map size before this port decodes them.
    let mut position = 0_u64;
    let mut index = 0;
    while index + 1 < words.len() {
        let start = position + u64::from(words[index]);
        let end = start + u64::from(words[index + 1]);
        for pixel in start..end {
            let pixel = pixel as usize;
            if pixel < sat.len() / 3 {
                sat[3 * pixel + channel] = 1;
            }
        }
        position = end;
        index += 2;
    }
}

/// One Full saturation-map entry: 32 bits per word, most significant
/// first, writing both zeros and ones. The final word is ignored.
pub fn decode_sat_map_full(words: &[u32], sat: &mut [u8], channel: usize) {
    for (index, &word) in words.iter().enumerate().take(words.len().saturating_sub(1)) {
        for bit in 0..32 {
            let pixel = 32 * index + bit;
            if pixel < sat.len() / 3 {
                sat[3 * pixel + channel] = ((word >> (31 - bit)) & 1) as u8;
            }
        }
    }
}

impl HighlightRestoration {
    /// Native threshold:
    /// `e = max(ExpEstimate + ExpNet - 1.5, 0)`,
    /// `mix = e > 1 ? 0 : 1 - e`, `threshold = fma(mix, 3000, 4000)`.
    pub fn new(exp_estimate: f32, exp_net: f32) -> Self {
        let e = (f64::from(exp_estimate) + f64::from(exp_net) - 1.5).max(0.0);
        let mix = if e > 1.0 { 0.0 } else { 1.0 - e };
        Self {
            threshold: mix.mul_add(3000.0, 4000.0),
        }
    }

    pub fn threshold(&self) -> f64 {
        self.threshold
    }
}

/// Stage 5 of the ordered float frontend: raster despeckle, the scalar
/// reconstruction of the native despeckle wrapper with its speckle
/// detector, the eight-way row split of the worker pool and the mode-3
/// bad-pixel correction (native minimum-badness argument 3). Verified
/// against the archived native oracle: 7 detector cases / 66,876
/// records exactly equal in count, order and flags, every LUT index
/// 0..4095 in all three channels straddled by nextafter probes on both
/// threshold boundaries, 2,597 mode-3 correction cases and 4 full
/// wrapper buffers byte-identical including row padding.
///
/// `gain` is the ApplyGains request gain triple (spatial factors
/// already changed the pixels), `adjust` is the typed CAMF `DespAdjust`
/// vector (default 15,15,5) scaled by the request strength, and `rect`
/// is ActiveImageArea translated by KeepImageArea's left/top, not the
/// ValidPixRect. The detector reads only the rect's left/top words.
#[derive(Clone, Debug)]
pub struct Despeckle {
    lut: [[f32; 4096]; 3],
    rect: [u32; 4],
    /// graineo addition: strength-scaled `DespAdjust` (for the own detection)
    adjust: [f64; 3],
    /// graineo addition: own detection, set by [`Self::apply`] when switched on
    own: Option<speckle_own::OwnSpeckle>,
}

impl Despeckle {
    /// Native LUT construction: 4096 float thresholds per
    /// channel with explicit double fma and the exact native double
    /// constants (channel-B terms are exact doubles of rounded floats).
    /// The thresholds are float32 sample-count differences.
    fn make_lut(gain: [f64; 3], adjust: [f64; 3]) -> Result<[[f32; 4096]; 3]> {
        const K: [u64; 3] = [
            0x3d08_c511_7a7e_165e,
            0x3d00_e374_a4f8_e0b4,
            0x3cec_d2b2_97d8_89bc,
        ];
        const B: [u64; 3] = [
            0x3ea5_798e_e000_0000,
            0x3eab_2dd8_e000_0000,
            0x3e65_798e_e000_0000,
        ];
        const C1: u64 = 0x3f36_6666_6666_6666;
        const C2: u64 = 0x3c04_8ad3_caef_e280;
        const C3: u64 = 0x3c07_9ca1_0c92_4223;
        const C4: u64 = 0x3fe8_3886_594a_f4f1;
        let mut lut = [[0.0_f32; 4096]; 3];
        for channel in 0..3 {
            for index in 0..4096 {
                let a = f64::from((f64::from_bits(K[channel]) / gain[channel]) as f32);
                let mut t = ((f64::from(index as f32) * f64::from_bits(C1)) * a
                    / f64::from_bits(C2)) as f32;
                t = t.sqrt();
                t = ((f64::from(t) * f64::from_bits(C3)) / a) as f32;
                let inner = f64::from(t).mul_add(
                    f64::from(t) * f64::from_bits(C4),
                    f64::from_bits(B[channel]),
                );
                let mut u = inner.sqrt() as f32;
                u *= adjust[channel] as f32;
                u *= 4096.0_f32;
                lut[channel][index] = (f64::from(u) / 1.4) as f32;
            }
        }
        if lut
            .iter()
            .any(|channel| channel.iter().any(|v| !v.is_finite()))
        {
            return Err(Error::InvalidData("invalid Merrill despeckle thresholds"));
        }
        Ok(lut)
    }

    /// Build the stage from the request gains, the scaled adjust vector
    /// and the translated active rect `[left, top, right, bottom]`.
    pub fn new(gain: [f64; 3], adjust: [f64; 3], rect: [u32; 4]) -> Result<Self> {
        // The verified domain requires positive finite gains and
        // non-negative finite adjust values; other inputs reach native
        // non-finite thresholds.
        if gain.iter().any(|v| !v.is_finite() || *v <= 0.0)
            || adjust.iter().any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err(Error::InvalidData("invalid Merrill despeckle calibration"));
        }
        // The column scan bounds need left >= 5 so the one-pixel
        // predicate halo stays inside the image; the row bounds depend
        // on the worker split and are validated per scan against the
        // effective range.
        if rect[0] < 5 {
            return Err(Error::InvalidData("invalid Merrill despeckle rectangle"));
        }
        let lut = Self::make_lut(gain, adjust)?;
        Ok(Self { lut, rect, adjust, own: None })
    }

    /// One LUT threshold, for tests and traces.
    pub fn lut(&self, channel: usize, index: usize) -> f32 {
        self.lut[channel][index]
    }

    /// The translated active rect `[left, top, right, bottom]`.
    pub fn rect(&self) -> [u32; 4] {
        self.rect
    }

    /// The per-pixel channel mask (C0 bit 4, C1 bit 2, C2 bit 1) of
    /// the native predicate: a strict horizontal maximum or minimum
    /// that clears the cross and diagonal neighbor thresholds. The
    /// maximum branch clamps the compared sample and the LUT index to
    /// 4095; the minimum branch compares the original center with an
    /// index truncated from the center clamped to 0..4095. Equality
    /// at a threshold fails the strict comparison.
    fn predicate(&self, data: &[f32], row_stride: usize, x: usize, y: usize) -> u32 {
        if let Some(own) = &self.own {
            return own.predicate(data, row_stride, x, y);
        }
        let at = |dx: i32, dy: i32, channel: usize| -> f32 {
            data[(y as i32 + dy) as usize * row_stride + (x as i32 + dx) as usize * 3 + channel]
        };
        let mut mask = 0_u32;
        for channel in 0..3 {
            let center = at(0, 0, channel);
            let left = at(-1, 0, channel);
            let right = at(1, 0, channel);
            if center > left && center > right {
                // Native clamps the compared sample as well as the LUT
                // index.
                let center = center.min(4095.0);
                let threshold = self.lut[channel][(center.max(0.0)) as usize];
                let cross = left
                    .max(right)
                    .max(at(0, -1, channel))
                    .max(at(0, 1, channel));
                let diagonal = at(-1, -1, channel)
                    .max(at(1, -1, channel))
                    .max(at(-1, 1, channel))
                    .max(at(1, 1, channel));
                if center > cross + threshold && center > diagonal + threshold {
                    mask |= 4 >> channel;
                }
            } else if center < left && center < right {
                let threshold = self.lut[channel][(center.clamp(0.0, 4095.0)) as usize];
                let cross = left
                    .min(right)
                    .min(at(0, -1, channel))
                    .min(at(0, 1, channel));
                let diagonal = at(-1, -1, channel)
                    .min(at(1, -1, channel))
                    .min(at(-1, 1, channel))
                    .min(at(1, 1, channel));
                if center < cross - threshold && center < diagonal - threshold {
                    mask |= 4 >> channel;
                }
            }
        }
        mask
    }

    /// One worker's detection scan over its inclusive row range. The
    /// native bounds read only the rect's first two words:
    /// rows intersect the worker range through
    /// `max(top - 4, start) ..= min(height - (top - 4), end)` and
    /// columns run `left - 4 ..= width - left + 2`; the detector
    /// stops at 624,999 marks (its 625,000 six-byte segment slots
    /// minus the four-byte count).
    fn detect_worker(
        &self,
        data: &[f32],
        width: usize,
        height: usize,
        row_stride: usize,
        start: usize,
        end: usize,
        out: &mut Vec<BadPixelRecord>,
        control: Control<'_>,
    ) -> Result<()> {
        control.check()?;
        let [left, top, _, _] = self.rect;
        let col_begin = i64::from(left) - 4;
        let col_end = width as i64 - i64::from(left) + 2;
        let row_begin = (i64::from(top) - 4).max(start as i64);
        let row_end = (height as i64 - (i64::from(top) - 4)).min(end as i64);
        // The column invariant (`left >= 5`) is owned by `new`. The
        // row range depends on the worker split, so it is validated
        // here: the predicate reads a one-pixel halo, and scans that
        // would leave the image are outside the verified contract (the
        // native reads without a clamp).
        if row_begin > row_end {
            // A worker outside the scan range detects nothing.
            return Ok(());
        }
        if row_begin < 1 || row_end > height as i64 - 2 {
            return Err(Error::InvalidData("invalid Merrill despeckle geometry"));
        }
        // graineo addition: search rows in parallel (read only), then merge in
        // raster order and truncate at 624 999 marks like the original:
        // bit-identical to the serial version.
        use rayon::prelude::*;
        control.check()?;
        let rows: Vec<Vec<BadPixelRecord>> = (row_begin..=row_end)
            .into_par_iter()
            .map(|y| {
                let y = y as usize;
                let mut row = Vec::new();
                for x in col_begin..=col_end {
                    let x = x as usize;
                    let mask = self.predicate(data, row_stride, x, y);
                    if mask != 0 {
                        row.push(BadPixelRecord { row: y as u16, column: x as u16, flags: (0x3ff | (mask << 12)) as u16 });
                    }
                }
                row
            })
            .collect();
        control.check()?;
        let mut marks = 0_usize;
        for row in rows {
            for record in row {
                out.push(record);
                marks += 1;
                if marks >= 624_999 {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Detect over the native worker-pool partition: eight
    /// nonoverlapping row ranges of `floor(height / 8)`, the remainder
    /// in the final worker, joined in worker order (raster order).
    pub fn detect(
        &self,
        data: &[f32],
        width: usize,
        height: usize,
        row_stride: usize,
        control: Control<'_>,
    ) -> Result<Vec<BadPixelRecord>> {
        control.check()?;
        if height < 8 {
            // floor(height / 8) cannot split fewer than eight rows.
            return Err(Error::InvalidData("invalid Merrill despeckle size"));
        }
        if width > u16::MAX as usize || height > u16::MAX as usize {
            return Err(Error::InvalidData("invalid Merrill despeckle size"));
        }
        if row_stride < width * 3 || data.len() < row_stride * (height - 1) + width * 3 {
            return Err(Error::InvalidData("invalid Merrill despeckle layout"));
        }
        let step = height / 8;
        let mut records = Vec::new();
        let mut start = 0;
        for worker in 0..8 {
            let end = if worker == 7 {
                height - 1
            } else {
                start + step - 1
            };
            self.detect_worker(
                data,
                width,
                height,
                row_stride,
                start,
                end,
                &mut records,
                control,
            )?;
            start += step;
        }
        Ok(records)
    }

    /// Run the full stage in place: detect all eight worker lists, join
    /// them in worker order, then correct sequentially with the mode-3
    /// bad-pixel replacement (minimum badness 3, all eight neighbors
    /// enabled by the 0x3ff record flags, no overrides). Detection
    /// finishes before correction starts; correction never re-detects.
    /// Returns the joined record count.
    pub fn apply(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        row_stride: usize,
        control: Control<'_>,
    ) -> Result<usize> {
        if self.own.is_none() && crate::merrill_new::active("outliers") {
            let mut own = self.clone();
            own.own = Some(speckle_own::OwnSpeckle::estimate(data, width, height, row_stride, self.adjust));
            return own.apply(data, width, height, row_stride, control);
        }
        let records = self.detect(data, width, height, row_stride, control)?;
        control.check()?;
        if std::env::var_os("X3F_SPECKLE_DEBUG").is_some() {
            let mut per = [0usize; 3];
            for r in &records {
                for (c, n) in per.iter_mut().enumerate() {
                    *n += usize::from((r.flags >> 12) & (4 >> c) != 0);
                }
            }
            eprintln!("speckle {}: {} records, per layer {per:?}", if self.own.is_some() { "own" } else { "today" }, records.len());
        }
        for record in &records {
            self.replace_own(data, width, height, row_stride, *record);
        }
        Ok(records.len())
    }

    /// graineo: own replacement of one detected speckle. Only the marked channels change, each from
    /// its own channel of the eight neighbours:
    /// - target between the neighbour median (isolated outlier) and the neighbour extreme on the side
    ///   of the speckle, plus [`SPECKLE_KEEP`] of the excess beyond that extreme (fine detail);
    /// - the weight follows whether the unmarked channels of the same pixel show a peak of the same
    ///   sign against their own neighbour median, measured in units of the detection threshold of
    ///   that channel ([`SPECKLE_LAYER_EVIDENCE`]). Foveon layers see the same structure, while noise
    ///   is independent per layer: a peak in one layer only is replaced, a peak in all layers (a fibre,
    ///   a thread, a light point) is only limited. The other channels only steer the weight; the new
    ///   value never comes from another channel.
    fn replace_own(&self, data: &mut [f32], width: usize, height: usize, row_stride: usize, record: BadPixelRecord) {
        let (x, y) = (record.column as usize, record.row as usize);
        if x < 2 || y < 2 || x >= width.saturating_sub(1) || y >= height.saturating_sub(1) {
            return;
        }
        let marked = u32::from(record.flags >> 12) & 7;
        let index = y * row_stride + x * 3;
        let around = |data: &[f32], c: usize| -> [f32; 8] {
            let mut v = NEIGHBOR_OFFSETS.map(|(dx, dy)| data[(y as i32 + dy) as usize * row_stride + (x as i32 + dx) as usize * 3 + c]);
            v.sort_by(f32::total_cmp);
            v
        };
        let threshold = |c: usize, v: f32| match &self.own {
            Some(own) => own.threshold(c, v).max(1e-3),
            None => self.lut[c][v.clamp(0.0, 4095.0) as usize].max(1e-3),
        };
        for c in 0..3 {
            if marked & (4 >> c) == 0 {
                continue;
            }
            let value = data[index + c];
            let n = around(data, c);
            let up = value > n[7];
            if !up && value >= n[0] {
                continue;
            }
            let extreme = if up { n[7] } else { n[0] };
            let median = 0.5 * (n[3] + n[4]);
            // evidence of the same structure in the unmarked layers
            let mut evidence = 0.0_f32;
            for k in 0..3 {
                if marked & (4 >> k) != 0 {
                    continue;
                }
                let own = data[index + k];
                let m = around(data, k);
                let mid = 0.5 * (m[3] + m[4]);
                let peak = if up { own - mid } else { mid - own };
                evidence = evidence.max(peak / threshold(k, own));
            }
            let [from, to] = SPECKLE_LAYER_EVIDENCE;
            let weight = ((evidence - from) / (to - from)).clamp(0.0, 1.0);
            let new = median + weight * (extreme - median) + weight * SPECKLE_KEEP * (value - extreme);
            if new.is_finite() {
                data[index + c] = new;
            }
        }
    }
}

/// graineo: evidence range (in detection thresholds of the unmarked layers) over which a speckle
/// goes from "replace by the neighbour median" to "limit to the neighbour extreme".
const SPECKLE_LAYER_EVIDENCE: [f32; 2] = [0.15, 1.0];
/// graineo: share of the excess beyond the neighbour extreme kept for a speckle that all layers show.
const SPECKLE_KEEP: f32 = 0.25;

/// The ordered Merrill float frontend, stage 1: ApplyGains.
///
/// `black` is the per-channel DarkShieldTop trimmed mean, `gain` is the
/// request gain from the parameter setup (native order: `((baseGain * FNumberGainFact)
/// * TempGainFact) * SensorAdjustmentGainFact`, where `baseGain` is
/// `wb + wb` or `wb / 0.55` by process version), and `spatial` holds the
/// three native spatial gain tables or `None` to select the global gain
/// alone (the native all-three gate).
#[derive(Clone, Debug)]
pub struct FloatFront {
    black: [f64; 3],
    gain: [f64; 3],
    spatial: Option<[GainTable; 3]>,
}

impl FloatFront {
    pub fn new(black: [f64; 3], gain: [f64; 3], spatial: Option<[GainTable; 3]>) -> Result<Self> {
        let valid = |v: [f64; 3]| v.iter().all(|v| v.is_finite());
        if let Some(tables) = spatial.as_ref() {
            if tables.iter().any(|t| t.columns < 2 || t.rows < 2) {
                return Err(Error::InvalidData("invalid Merrill float spatial tables"));
            }
        }
        if !valid(black) || !valid(gain) {
            return Err(Error::InvalidData(
                "invalid Merrill float front calibration",
            ));
        }
        Ok(Self {
            black,
            gain,
            spatial,
        })
    }

    pub fn black(&self) -> [f64; 3] {
        self.black
    }

    pub fn gain(&self) -> [f64; 3] {
        self.gain
    }

    pub fn spatial(&self) -> Option<&[GainTable; 3]> {
        self.spatial.as_ref()
    }

    /// Apply stage 1 over decoded sensor counts. `decoded` holds
    /// interleaved RGB u16 samples with `row_stride` u16 elements per row;
    /// the u16 to f32 conversion is exact. The result is an owned f32
    /// raster with the same layout, gained but not normalized.
    ///
    /// Cancellation is checked once before the single native pass; the
    /// native ApplyGains has no intra-stage cancellation point.
    pub fn apply(
        &self,
        decoded: &[u16],
        width: usize,
        height: usize,
        row_stride: usize,
        control: Control<'_>,
    ) -> Result<Vec<f32>> {
        control.check()?;
        if height == 0 || width == 0 {
            return Err(Error::InvalidData("empty Merrill float image"));
        }
        if self.spatial.is_some() && height < 2 {
            return Err(Error::InvalidData("invalid Merrill float image size"));
        }
        if row_stride < width * 3 || decoded.len() < row_stride * (height - 1) + width * 3 {
            return Err(Error::InvalidData("invalid Merrill float image layout"));
        }
        let mut data = raw_float(decoded)?;
        self.apply_in_place(&mut data, width, height, row_stride, control)?;
        Ok(data)
    }

    /// Apply stage 1 to a raw float raster from `raw_float`. The detector
    /// stage reads this raster before the gains.
    pub fn apply_in_place(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        row_stride: usize,
        control: Control<'_>,
    ) -> Result<()> {
        control.check()?;
        if height == 0 || width == 0 {
            return Err(Error::InvalidData("empty Merrill float image"));
        }
        if self.spatial.is_some() && height < 2 {
            return Err(Error::InvalidData("invalid Merrill float image size"));
        }
        if row_stride < width * 3 || data.len() < row_stride * (height - 1) + width * 3 {
            return Err(Error::InvalidData("invalid Merrill float image layout"));
        }
        let refs: [Option<&GainTable>; 3] = match &self.spatial {
            None => [None, None, None],
            Some(t) => [Some(&t[0]), Some(&t[1]), Some(&t[2])],
        };
        apply_gains(data, width, height, row_stride, self.black, self.gain, refs);
        Ok(())
    }
}

/// Convert decoded sensor counts to the owned native float raster.
/// u16 to f32 is exact: every u16 value is representable in f32. Native
/// sign-extends the 16-bit sample before the float conversion; the result
/// is identical for valid 0..4095 sensor data (recorded in the stage 1 notes).
pub fn raw_float(decoded: &[u16]) -> Result<Vec<f32>> {
    let mut data = Vec::new();
    data.try_reserve_exact(decoded.len())
        .map_err(|_| Error::Allocation)?;
    data.extend(decoded.iter().map(|&v| f32::from(v)));
    Ok(data)
}

/// Apply black offsets, channel gains and spatial tables in one pass.
///
/// `data` holds interleaved RGB float samples with `row_stride` floats
/// per row. `row_stride` may exceed `width * 3`; the padding floats are
/// never read or written. When any entry of `tables` is `None`, the
/// native all-three gate applies and every channel uses `gain` alone.
///
/// The spatial path requires `height >= 2`. The native operation divides
/// by `height - 1` and produces NaN for a single row.
pub fn apply_gains(
    data: &mut [f32],
    width: usize,
    height: usize,
    row_stride: usize,
    black: [f64; 3],
    gain: [f64; 3],
    tables: [Option<&GainTable>; 3],
) {
    assert!(
        row_stride >= width * 3,
        "row stride smaller than the pixel row"
    );
    assert!(
        data.len() >= row_stride * (height - 1) + width * 3,
        "buffer smaller than the image"
    );
    let spatial = tables.iter().all(Option::is_some);
    for y in 0..height {
        // Per-channel interpolated gain row. The vertical coordinate is
        // float32, and each knot is blended with one float32 fma.
        let mut row = [Vec::new(), Vec::new(), Vec::new()];
        let mut edge = [0.0_f64; 3];
        let mut span = [0.0_f64; 3];
        let mut value = [0.0_f64; 3];
        let mut slope = [0.0_f64; 3];
        let mut knot = [0_usize; 3];
        if spatial {
            for c in 0..3 {
                let table = tables[c].expect("spatial path requires all tables");
                // Native: (f32(y) / f32(height-1)) * f32(rows-1).
                let position = (y as f32 / (height - 1) as f32) * (table.rows - 1) as f32;
                let lo = position.floor() as usize;
                let hi = position.ceil() as usize;
                let fraction = position - lo as f32;
                row[c].resize(table.columns, 0.0);
                for x in 0..table.columns {
                    let a = table.values[lo * table.columns + x];
                    let b = table.values[hi * table.columns + x];
                    row[c][x] = fraction.mul_add(b - a, a);
                }
                // Native horizontal span: width / (columns - 1), not
                // width - 1. The slope numerator is a rounded float32
                // difference before the double conversion.
                span[c] = width as f64 / (table.columns - 1) as f64;
                edge[c] = span[c];
                value[c] = row[c][0] as f64;
                slope[c] = (row[c][1] - row[c][0]) as f64 / span[c];
            }
        }
        let pixels = &mut data[y * row_stride..y * row_stride + width * 3];
        for x in 0..width {
            for c in 0..3 {
                if spatial && x as f64 >= edge[c] {
                    knot[c] += 1;
                    value[c] = row[c][knot[c]] as f64;
                    slope[c] = (row[c][knot[c] + 1] - row[c][knot[c]]) as f64 / span[c];
                    edge[c] += span[c];
                }
                let adjusted = if spatial { gain[c] * value[c] } else { gain[c] };
                let index = 3 * x + c;
                pixels[index] = ((pixels[index] as f64 - black[c]) * adjusted) as f32;
                if spatial {
                    value[c] += slope[c];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Archived native results: (width, height, spatial, fnv1a hash).
    /// Source: the archived native run; an independent repeat confirmed
    /// every hash.
    const NATIVE_CASES: &[(usize, usize, bool, u64)] = &[
        (1, 5, false, 0x138328c54eef5e5f),
        (2, 5, false, 0xe74038c3d75ccf3e),
        (3, 5, false, 0xcce4f8b3ddcaf15b),
        (4, 5, false, 0xaf74bebb544d557c),
        (5, 5, false, 0x5f4802a8b6ca169d),
        (9, 5, false, 0xe98c4c467bd4bc02),
        (9, 13, true, 0x975b9335a4245ddf),
        (37, 5, false, 0x3ea06b0be3260cbb),
        (37, 13, true, 0x7951b2f5927245f1),
        (97, 5, false, 0x595afa5a2e4e4c46),
        (97, 13, true, 0xb63b750844e59ce2),
    ];

    fn fnv1a(values: &[f32]) -> u64 {
        super::fnv1a_f32(values)
    }

    fn synthetic_tables() -> [GainTable; 3] {
        std::array::from_fn(|c| {
            let columns = 3 + 2 * c;
            let rows = 3 + c;
            let values = (0..columns * rows)
                .map(|i| 0.8_f32 + (((i * 7 + c * 3) % 17) as f32) * 0.071_f32)
                .collect();
            GainTable::new(values, columns, rows)
        })
    }

    /// Port of the archived differential oracle. The padding floats
    /// (-12345.25) must stay untouched, and the kernel must reproduce
    /// every archived native hash bit-for-bit.
    #[test]
    fn apply_gains_matches_native_oracle() {
        let black = [128.125_f64, 255.3, -64.75];
        let gain = [2.21234567_f64, 0.98765432, 4.4433322];
        let tables = synthetic_tables();
        let mut cases = 0;
        let mut samples = 0;
        for &(width, height, spatial, expected_hash) in NATIVE_CASES {
            let stride = width * 3 + 7;
            let mut data = vec![-12345.25_f32; stride * height];
            for y in 0..height {
                for x in 0..width {
                    for c in 0..3 {
                        let raw = ((x * 191 + y * 131 + c * 977) % 8192) as i32 - 128;
                        data[y * stride + 3 * x + c] = (raw as f32) * 0.75_f32;
                    }
                }
            }
            // The archived no-spatial cases pass table 0 and table 2
            // while table 1 is absent: the all-three gate must select
            // the global gain alone.
            let refs: [Option<&GainTable>; 3] = [
                Some(&tables[0]),
                if spatial { Some(&tables[1]) } else { None },
                Some(&tables[2]),
            ];
            apply_gains(&mut data, width, height, stride, black, gain, refs);
            for y in 0..height {
                for pad in width * 3..stride {
                    assert_eq!(data[y * stride + pad], -12345.25_f32, "padding changed");
                }
            }
            assert_eq!(
                fnv1a(&data),
                expected_hash,
                "case {width}x{height} spatial={spatial}"
            );
            cases += 1;
            samples += width * height * 3;
        }
        assert_eq!(cases, 11);
        assert_eq!(samples, 7947);
    }

    #[test]
    fn missing_any_table_selects_global_gain_only() {
        let tables = synthetic_tables();
        let mut with_all = vec![100.5_f32; 3 * 2 * 2];
        let mut without_middle = with_all.clone();
        let black = [1.0_f64; 3];
        let gain = [2.0_f64; 3];
        apply_gains(
            &mut with_all,
            2,
            2,
            6,
            black,
            gain,
            [Some(&tables[0]), Some(&tables[1]), Some(&tables[2])],
        );
        apply_gains(
            &mut without_middle,
            2,
            2,
            6,
            black,
            gain,
            [Some(&tables[0]), None, Some(&tables[2])],
        );
        // The all-three gate makes the missing-middle case equal to the
        // pure global gain, different from the spatial result.
        let pure = vec![((100.5_f64 - 1.0) * 2.0) as f32; 12];
        assert_eq!(without_middle, pure);
        assert_ne!(with_all, pure);
    }

    /// Port of the instruction-derived radial reference
    /// (archived reference script). The geometry
    /// uses the documented DP2 example: native HI center (2416, 1604)
    /// and half-extents (2400, 1600) with the shown rect inputs.
    #[test]
    fn radial_factors_match_instruction_reference() {
        // The docs' DP2 HI example: native center (2416, 1604) and
        // half-extents (2400, 1600). valid.top = 4, valid.bottom = 3204
        // (activeMinusKeep.top 36 - 32 margin; 3172 + 32 - keep-relative
        // already applied), valid.left = 16, valid.right = 4816.
        // minus_keep[1] = active.top - keep.top = 36, minus_keep[3] =
        // active.bottom - keep.top = 3172.
        let active = [0, 68, 0, 3204];
        let keep = [0, 32, 0, 32];
        let col_range = [0, 10, 4824, 0];
        let coefficients = [1e-4, -2e-4, 2e-4, -1e-4];
        let radial = RadialShading::new(active, keep, col_range, coefficients, "HI").unwrap();
        assert_eq!(radial.center_x as i32, 2416);
        assert_eq!(radial.center_y as i32, 1604);

        // Instruction-order reference (radial_reference.py): dy squared
        // first through fma, radius sqrt/divide, square, fma factors.
        let reference = |x: i32, y: i32, stretch: f64| {
            let dx = x as f64 - 2416.0;
            let dy = y as f64 - 1604.0;
            let dy_s = stretch * dy;
            let radius = dy_s.mul_add(dy_s, dx * dx).sqrt()
                / (stretch * 1600.0)
                    .mul_add(stretch * 1600.0, 2400.0 * 2400.0)
                    .sqrt();
            let t = radius * radius;
            let t2 = t * t;
            let [a, b, c, d] = coefficients;
            let bottom = a.mul_add(t2, t * b) + 1.0;
            let top = c.mul_add(t2, t * d) + 1.0;
            (bottom as f32, top as f32)
        };
        for (x, y) in [(2416, 1604), (0, 0), (4815, 3203), (100, 200), (2416, 0)] {
            let (bottom, top) = radial.factors(x, y);
            let (r_bottom, r_top) = reference(x, y, 1.0);
            assert_eq!(bottom, r_bottom, "C0 factor at ({x},{y})");
            assert_eq!(top, r_top, "C2 factor at ({x},{y})");
        }
    }

    #[test]
    fn radial_med_stretches_the_vertical_axis() {
        let active = [0, 68, 0, 3204];
        let keep = [0, 32, 0, 32];
        let col_range = [0, 10, 4824, 0];
        let coefficients = [1e-4, -2e-4, 2e-4, -1e-4];
        let hi = RadialShading::new(active, keep, col_range, coefficients, "HI").unwrap();
        let med = RadialShading::new(active, keep, col_range, coefficients, "MED").unwrap();
        // A pixel directly above the center: dx = 0, so the radius is
        // pure dy. MED doubles dy in the numerator AND the denominator
        // half-extent, but dy (1604) exceeds halfY (1600), so the MED
        // radius is larger and its C0 factor differs from HI.
        let (hi_bottom, _) = hi.factors(2416, 0);
        let (med_bottom, _) = med.factors(2416, 0);
        assert_ne!(med_bottom, hi_bottom, "MED must change vertical geometry");
        // MED uses the 16 margin (scale 0.5): valid.top = 36-16 = 20,
        // valid.bottom = 3172+16 = 3188, halfY = (3188-20)/2 = 1584,
        // center_y = 20+1584 = 1604 (same symmetric center, different
        // half-extent: the denominator stretches by 2*1584).
        assert_eq!(med.center_y as i32, 1604);
    }

    #[test]
    fn radial_applies_factors_in_place_and_leaves_c1() {
        let active = [0, 0, 100, 100];
        let keep = [0, 0, 0, 0];
        let col_range = [0, 0, 92, 0];
        let radial =
            RadialShading::new(active, keep, col_range, [1e-3, -1e-3, 2e-3, -2e-3], "HI").unwrap();
        let stride = 10 * 3 + 3;
        let mut data = vec![10.0_f32; stride * 10];
        radial
            .apply(&mut data, 10, 10, stride, Control::none())
            .unwrap();
        for y in 0..10 {
            for x in 0..10 {
                let (bottom, top) = radial.factors(x as i32, y as i32);
                let pixel = &data[y * stride + 3 * x..y * stride + 3 * x + 3];
                assert_eq!(pixel[0], 10.0_f32 * bottom, "C0 at ({x},{y})");
                assert_eq!(pixel[1], 10.0_f32, "C1 unchanged at ({x},{y})");
                assert_eq!(pixel[2], 10.0_f32 * top, "C2 at ({x},{y})");
            }
            for pad in 10 * 3..stride {
                assert_eq!(data[y * stride + pad], 10.0_f32, "padding changed");
            }
        }
    }

    #[test]
    fn radial_rejects_degenerate_geometry() {
        // A collapsed valid rect (zero half-extents on both axes) gives
        // a zero denominator. valid.top == valid.bottom needs
        // active.bottom - active.top == -64 for the HI margin.
        let zero = RadialShading::new([0, 100, 0, 36], [0; 4], [0, 0, 14, 0], [1.0; 4], "HI");
        assert!(zero.is_err());
        // Non-finite coefficients reject.
        let bad = [f64::NAN, 0.0, 0.0, 0.0];
        let result = RadialShading::new([0, 0, 100, 100], [0; 4], [0, 0, 92, 0], bad, "HI");
        assert!(result.is_err());
    }

    #[test]
    fn listed_repair_minimum_uses_unsigned_subtraction() {
        // Native wraps instead of saturating: badness 1 with offset 3
        // wraps to u32::MAX - 1 (0xFFFFFFFE).
        let repair = ListedRepair::new(1, 3);
        assert_eq!(repair.minimum_badness(), 0xFFFF_FFFE);
        // Badness 0 selects minimum 0 regardless of the offset.
        assert_eq!(ListedRepair::new(0, 3).minimum_badness(), 0);
        assert_eq!(ListedRepair::new(4, 1).minimum_badness(), 3);
    }

    #[test]
    fn listed_repair_rejects_invalid_layout() {
        let repair = ListedRepair::new(0, 1);
        let mut data = vec![0.0_f32; 3 * 3 * 3];
        // Width below 4 cannot host a halo record.
        assert!(repair
            .apply(
                &mut data,
                3,
                3,
                9,
                &[BadPixelRecord {
                    row: 1,
                    column: 1,
                    flags: 0xff
                }],
                &[],
                Control::none()
            )
            .is_err());
    }

    /// 7×7 test image: every pixel `base`, slightly varied
    fn flat(base: [f32; 3]) -> Vec<f32> {
        (0..49)
            .flat_map(|i| {
                let d = (i % 5) as f32;
                [base[0] + d, base[1] + d, base[2] + d]
            })
            .collect()
    }

    #[test]
    fn own_listed_repair_keeps_unmarked_channels() {
        let mut data = flat([100.0, 200.0, 300.0]);
        let i = (3 * 7 + 3) * 3;
        data[i..i + 3].copy_from_slice(&[999.0, 555.0, 777.0]);
        // channel 1 marked (bit 2 of the channel mask), all neighbours good, badness 15
        let rec = BadPixelRecord { row: 3, column: 3, flags: (2 << 12) | (15 << 8) | 0xff };
        ListedRepair::new(0, 0).apply(&mut data, 7, 7, 21, &[rec], &[], Control::none()).unwrap();
        assert_eq!(data[i], 999.0);
        assert_eq!(data[i + 2], 777.0);
        assert!((200.0..=204.0).contains(&data[i + 1]), "{}", data[i + 1]);
        // a pixel with its own structure in the intact channels and a plausible marked value stays
        let mut data = flat([100.0, 200.0, 300.0]);
        data[i..i + 3].copy_from_slice(&[180.0, 203.0, 500.0]);
        ListedRepair::new(0, 0).apply(&mut data, 7, 7, 21, &[rec], &[], Control::none()).unwrap();
        assert_eq!(data[i..i + 3], [180.0, 203.0, 500.0]);
    }

    #[test]
    fn own_speckle_replaces_single_layer_peaks_and_limits_peaks_of_all_layers() {
        let despeckle = Despeckle::new([1.0; 3], [1.0; 3], [5, 5, 0, 0]).unwrap();
        let i = (3 * 7 + 3) * 3;
        // channel 2 marked
        let rec = BadPixelRecord { row: 3, column: 3, flags: (1 << 12) | 0x3ff };
        // peak in the marked channel only: noise, goes to the neighbour median
        let mut data = vec![1000.0_f32; 49 * 3];
        data[i + 2] = 3000.0;
        despeckle.replace_own(&mut data, 7, 7, 21, rec);
        assert_eq!(data[i..i + 3], [1000.0; 3]);
        // the same peak in all layers (a fibre): only limited, the unmarked layers stay
        let mut data = flat([1000.0, 1000.0, 1000.0]);
        data[i..i + 3].copy_from_slice(&[3000.0; 3]);
        despeckle.replace_own(&mut data, 7, 7, 21, rec);
        assert!(data[i + 2] > 1400.0 && data[i + 2] < 3000.0, "{}", data[i + 2]);
        assert_eq!((data[i], data[i + 1]), (3000.0, 3000.0));
    }

    #[test]
    fn sat_map_rle_decodes_runs_and_ignores_the_trailing_gap() {
        let mut sat = vec![0_u8; 8 * 2 * 3];
        // Alternating gap/run words; the trailing gap word is not consumed.
        super::decode_sat_map_rle(&[2, 3, 1, 2, 5], &mut sat, 1);
        let mut marked = vec![false; 16];
        for pixel in 0..16 {
            marked[pixel] = sat[3 * pixel + 1] != 0;
        }
        for pixel in [0_usize, 1, 5] {
            assert!(!marked[pixel], "pixel {pixel} must stay clear");
        }
        for pixel in [2_usize, 3, 4, 6, 7] {
            assert!(marked[pixel], "pixel {pixel} must be marked");
        }
        // Only the selected channel is written.
        assert!(sat
            .iter()
            .enumerate()
            .all(|(i, &v)| { v == 0 || i % 3 == 1 }));
    }

    #[test]
    fn sat_map_full_decodes_words_most_significant_first() {
        let mut sat = vec![0_u8; 64 * 3];
        // 0xF0 repeats the pattern 11110000; the final word is ignored.
        super::decode_sat_map_full(&[0xf0f0_f0f0, u32::MAX], &mut sat, 0);
        for (pixel, expected) in [(0, 1), (4, 0), (7, 0), (8, 1), (31, 0), (32, 0), (63, 0)] {
            assert_eq!(u32::from(sat[3 * pixel]), expected, "pixel {pixel}");
        }
    }

    /// Minimal MT19937 (std::mt19937) so the port reproduces the
    /// archived native-oracle mode matrix inputs exactly.
    struct Mt19937 {
        state: [u32; 624],
        index: usize,
    }

    impl Mt19937 {
        fn new(seed: u32) -> Self {
            let mut state = [0_u32; 624];
            state[0] = seed;
            for i in 1..624 {
                let previous = state[i - 1];
                state[i] = 1812433253_u32
                    .wrapping_mul(previous ^ (previous >> 30))
                    .wrapping_add(i as u32);
            }
            Self { state, index: 624 }
        }

        fn next(&mut self) -> u32 {
            if self.index == 624 {
                for i in 0..624 {
                    let y =
                        (self.state[i] & 0x8000_0000) | (self.state[(i + 1) % 624] & 0x7fff_ffff);
                    self.state[i] = self.state[(i + 397) % 624]
                        ^ (y >> 1)
                        ^ if y & 1 != 0 { 0x9908_b0df } else { 0 };
                }
                self.index = 0;
            }
            let mut y = self.state[self.index];
            self.index += 1;
            y ^= y >> 11;
            y ^= (y << 7) & 0x9d2c_5680;
            y ^= (y << 15) & 0xefc6_0000;
            y ^ (y >> 18)
        }
    }

}
