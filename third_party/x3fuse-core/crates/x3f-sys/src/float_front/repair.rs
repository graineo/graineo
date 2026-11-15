//! Ordered Merrill float repair stages that run around listed repair:
//! bad-red detection on the raw float raster before ApplyGains, and
//! bad-pixel cluster repair after the three listed repairs.

use super::BadPixelRecord;
use crate::control::{Error, Result};
use crate::Control;

/// Native output allocation for detected records (6,000 bytes). The
/// detector keeps one slot free, so at most 999 records are produced.
pub const DETECTED_CAPACITY: usize = 1000;
/// Native primary override allocation (capacity * 1000 entries).
pub const OVERRIDE_CAPACITY: usize = DETECTED_CAPACITY * 1000;

const NEIGHBOR_BITS: [u32; 9] = [1, 2, 4, 8, 0, 16, 32, 64, 128];

fn check_layout(len: usize, width: usize, height: usize, row_stride: usize) -> Result<()> {
    let row = width.checked_mul(3);
    if width == 0
        || height == 0
        || !row.is_some_and(|row| row <= row_stride)
        || !(height - 1)
            .checked_mul(row_stride)
            .and_then(|n| n.checked_add(width * 3))
            .is_some_and(|n| n <= len)
    {
        return Err(Error::InvalidData("invalid Merrill repair image layout"));
    }
    Ok(())
}

// The native float multiplications round separately. Additions and the
// comparison are in f64. There is no FMA.
fn rejects(r: f32, b: f32, noise: &[f64; 9]) -> bool {
    let nr = (noise[0] + f64::from(r * noise[3] as f32)) + f64::from((r * r) * noise[6] as f32);
    let nb = (noise[2] + f64::from(b * noise[5] as f32)) + f64::from((b * b) * noise[8] as f32);
    f64::from(r.max(0.0)) - nr > nb + f64::from(b.max(0.0))
}

// Donor mask for a candidate at `position` in the listed 3x3 neighborhood.
// It excludes the listed center and the original disabled neighbors.
fn replacement_mask(position: usize, original: u32) -> u32 {
    let (px, py) = (position as i32 % 3 - 1, position as i32 / 3 - 1);
    let mut mask = 0;
    for j in (0..9).filter(|&j| j != 4) {
        let (x, y) = (px + j % 3 - 1, py + j / 3 - 1);
        if x == 0 && y == 0 {
            continue;
        }
        let inside = (-1..=1).contains(&x) && (-1..=1).contains(&y);
        if inside && original & NEIGHBOR_BITS[((y + 1) * 3 + x + 1) as usize] == 0 {
            continue;
        }
        mask |= NEIGHBOR_BITS[j as usize];
    }
    mask
}

/// Run the native bad-red detection over one listed set and append to `output`.
///
/// `data` is the decoded raw float raster before black subtraction and gains.
/// Only enabled neighbors of in-bounds listed records are tested. Pixels do
/// not change. A rejected neighbor becomes a new record with badness 3,
/// channel C0 and the donor mask from its position. Appends stop at
/// `DETECTED_CAPACITY - 1` records in `output`.
///
/// `noise` is ColorNoiseModel: constant, linear and quadratic rows of
/// C0, C1, C2. C1 terms are unused. With `overrides`, entry `i` (below
/// `OVERRIDE_CAPACITY`) starts as the record mask and becomes the mask of
/// the neighbors that stayed, if any stayed.
#[allow(clippy::too_many_arguments)]
pub fn detect_bad_reds(
    data: &[f32],
    width: usize,
    height: usize,
    row_stride: usize,
    listed: &[BadPixelRecord],
    noise: &[f64; 9],
    output: &mut Vec<BadPixelRecord>,
    mut overrides: Option<&mut Vec<u32>>,
    control: Control<'_>,
) -> Result<()> {
    control.check()?;
    check_layout(data.len(), width, height, row_stride)?;
    if output.len() >= DETECTED_CAPACITY || !noise.iter().all(|v| v.is_finite()) {
        return Err(Error::InvalidData("invalid Merrill bad red request"));
    }
    if let Some(overrides) = overrides.as_mut() {
        overrides.clear();
        overrides.reserve(listed.len().min(OVERRIDE_CAPACITY));
    }
    for (i, record) in listed.iter().enumerate() {
        if i % 4096 == 0 {
            control.check()?;
        }
        let (x, y) = (usize::from(record.column), usize::from(record.row));
        let mask = u32::from(record.flags & 0xff);
        let mut remaining = mask;
        if let Some(overrides) = overrides.as_mut().filter(|_| i < OVERRIDE_CAPACITY) {
            overrides.push(mask);
        }
        if y < 2 || y + 1 >= height || x < 2 || x + 1 >= width || mask == 0 {
            continue;
        }
        for j in (0..9).filter(|&j| mask & NEIGHBOR_BITS[j] != 0) {
            let (xx, yy) = (x + j % 3 - 1, y + j / 3 - 1);
            let p = yy * row_stride + 3 * xx;
            if rejects(data[p], data[p + 2], noise) && output.len() < DETECTED_CAPACITY - 1 {
                output.push(BadPixelRecord {
                    row: yy as u16,
                    column: xx as u16,
                    flags: (0x4300 | replacement_mask(j, mask)) as u16,
                });
                remaining -= NEIGHBOR_BITS[j];
            }
        }
        if let Some(overrides) = overrides.as_mut().filter(|_| i < OVERRIDE_CAPACITY) {
            if remaining != 0 {
                overrides[i] = remaining;
            }
        }
    }
    Ok(())
}

/// Counts from one packed-cluster pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClusterStats {
    pub records: usize,
    pub points: usize,
    pub zero_fallback: usize,
}

struct Cluster<'a> {
    x: usize,
    y: usize,
    rows: usize,
    columns: usize,
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
    payload: &'a [u16],
}

// Select records and validate them before any write. The native returns
// from the whole call on the first selected record with invalid geometry.
fn plan_clusters<'a>(
    words: &'a [u16],
    width: usize,
    height: usize,
    badness: u32,
    request_margin: u32,
) -> Result<Vec<Cluster<'a>>> {
    let invalid = || Error::InvalidData("invalid Merrill packed cluster list");
    let mut clusters = Vec::new();
    let mut offset = 0;
    // The native counts a record at every start before the last word.
    while offset + 1 < words.len() {
        let header = words.get(offset..offset + 4).ok_or_else(invalid)?;
        let count = usize::from(header[3] & 0xff);
        let payload = words
            .get(offset + 4..offset + 4 + count)
            .ok_or_else(invalid)?;
        offset += 4 + count;
        if u32::from(header[0] >> 14) != badness {
            continue;
        }
        let (y, x) = (usize::from(header[1]), usize::from(header[2]));
        let (rows, columns) = (
            usize::from((header[0] >> 7) & 127),
            usize::from(header[0] & 127),
        );
        if y >= height || x >= width || rows > 25 || columns > 25 {
            break;
        }
        let margin = 5.max(u32::from(header[3] >> 8).min(request_margin)) as usize;
        let (left, top) = (x.saturating_sub(margin), y.saturating_sub(margin));
        let right = (width - 1).min(x + columns + margin);
        let bottom = (height - 1).min(y + rows + margin);
        let (w, h) = (right - left + 1, bottom - top + 1);
        if w * h > 10_000 {
            break;
        }
        // The scan rectangle and each 5x5 priority window must lie inside
        // the expanded bounds. Sites must be unique, in the scan rectangle,
        // and have a channel mask 1..7.
        if x - left < 1 || y - top < 1 || x - left + columns + 2 > w || y - top + rows + 2 > h {
            return Err(invalid());
        }
        let mut seen = std::collections::HashSet::new();
        for &word in payload {
            let (px, py) = (
                x - left + usize::from((word >> 4) & 63),
                y - top + usize::from(word >> 10),
            );
            let mask = word & 15;
            if !(1..=7).contains(&mask)
                || px > x - left + columns
                || py > y - top + rows
                || (px >= 2 && py >= 2 && (px + 2 >= w || py + 2 >= h))
                || !seen.insert((px, py))
            {
                return Err(invalid());
            }
        }
        clusters.push(Cluster {
            x,
            y,
            rows,
            columns,
            left,
            top,
            right,
            bottom,
            payload,
        });
    }
    Ok(clusters)
}

/// graineo addition: fill the marked pixels of the packed `BadClustersF20` records (same selection and
/// validation as the native cluster repair) from the outside in, every channel only from the same channel.
/// A marked pixel is replaced as a whole (like the per-channel listed repair), so its channels stay
/// consistent. Each pass looks at the four opposite pairs of direct neighbours (left/right, above/below and
/// the two diagonals) that are not (or no longer) marked and takes the per-channel mean of the pair with
/// the smallest difference (sum over the channels); only when a pass fills nothing, the per-channel mean of
/// any usable neighbours is accepted. All values of a pass are written together, so the order inside a
/// pass does not matter. Nothing is copied from elsewhere in the image. `zero_fallback` counts marked
/// pixels without any usable neighbour (left unchanged).
#[allow(clippy::too_many_arguments)]
pub fn fill_bad_pixel_clusters(
    data: &mut [f32],
    width: usize,
    height: usize,
    row_stride: usize,
    words: &[u16],
    badness: u32,
    request_margin: u32,
    control: Control<'_>,
) -> Result<ClusterStats> {
    control.check()?;
    check_layout(data.len(), width, height, row_stride)?;
    let clusters = plan_clusters(words, width, height, badness, request_margin)?;
    let mut stats = ClusterStats::default();
    // opposite pairs: (dx, dy) and its mirror
    const PAIRS: [(i32, i32); 4] = [(-1, 0), (0, -1), (-1, -1), (1, -1)];
    for cluster in &clusters {
        control.check()?;
        let mut open: Vec<(usize, usize)> = cluster
            .payload
            .iter()
            .map(|&word| (cluster.x + usize::from((word >> 4) & 63), cluster.y + usize::from(word >> 10)))
            .filter(|&(x, y)| x < width && y < height)
            .collect();
        let points = open.len();
        let mut relaxed = false;
        while !open.is_empty() {
            control.check()?;
            let mut filled: Vec<(usize, usize, [f32; 3])> = Vec::new();
            for &(x, y) in &open {
                let good = |dx: i32, dy: i32| -> Option<[f32; 3]> {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx as usize >= width || ny as usize >= height {
                        return None;
                    }
                    let (nx, ny) = (nx as usize, ny as usize);
                    if open.contains(&(nx, ny)) {
                        return None;
                    }
                    let i = ny * row_stride + 3 * nx;
                    Some([data[i], data[i + 1], data[i + 2]])
                };
                let mut best: Option<(f32, [f32; 3])> = None;
                for (dx, dy) in PAIRS {
                    if let (Some(p), Some(q)) = (good(dx, dy), good(-dx, -dy)) {
                        let difference = (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>();
                        if best.is_none_or(|(d, _)| difference < d) {
                            best = Some((difference, std::array::from_fn(|c| 0.5 * (p[c] + q[c]))));
                        }
                    }
                }
                let value = match best {
                    Some((_, v)) => Some(v),
                    None if relaxed => {
                        let around: Vec<[f32; 3]> = (-1..=1)
                            .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
                            .filter(|&(dx, dy)| (dx, dy) != (0, 0))
                            .filter_map(|(dx, dy)| good(dx, dy))
                            .collect();
                        (!around.is_empty()).then(|| {
                            std::array::from_fn(|c| around.iter().map(|v| v[c]).sum::<f32>() / around.len() as f32)
                        })
                    }
                    None => None,
                };
                if let Some(v) = value.filter(|v| v.iter().all(|x| x.is_finite())) {
                    filled.push((x, y, v));
                }
            }
            if filled.is_empty() {
                if relaxed {
                    stats.zero_fallback += open.len();
                    break;
                }
                relaxed = true;
                continue;
            }
            relaxed = false;
            for &(x, y, v) in &filled {
                let i = y * row_stride + 3 * x;
                data[i..i + 3].copy_from_slice(&v);
            }
            open.retain(|&(x, y)| !filled.iter().any(|&(fx, fy, _)| (fx, fy) == (x, y)));
        }
        stats.points += points;
        stats.records += 1;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::float_front::fnv1a_f32;

    fn open_corpus(name: &str) -> Option<(*mut crate::x3f_t, *mut crate::sysabi::FILE)> {
        use crate::{x3f_get_camf, x3f_get_raw, x3f_load_data, x3f_new_from_file};
        let root = std::env::var_os("X3F_TEST_FILES")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../x3f_test_files")
            });
        let path = root.join(name);
        if !path.exists() {
            return None;
        }
        let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        unsafe {
            let file = crate::sysabi::fopen(path.as_ptr(), c"rb".as_ptr());
            assert!(!file.is_null());
            let x3f = x3f_new_from_file(file.cast());
            assert!(!x3f.is_null());
            assert_eq!(
                x3f_load_data(x3f, x3f_get_camf(x3f)),
                crate::x3f_return_e_X3F_OK
            );
            let prop = crate::x3f_get_prop(x3f);
            if !prop.is_null() {
                assert_eq!(x3f_load_data(x3f, prop), crate::x3f_return_e_X3F_OK);
            }
            assert_eq!(
                x3f_load_data(x3f, x3f_get_raw(x3f)),
                crate::x3f_return_e_X3F_OK
            );
            Some((x3f, file))
        }
    }

    fn write<T: Copy>(dir: &std::path::Path, name: &str, values: &[T]) {
        let bytes = unsafe {
            std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
        };
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn record_words(records: &[BadPixelRecord]) -> Vec<u16> {
        records
            .iter()
            .flat_map(|r| [r.row, r.column, r.flags])
            .collect()
    }

    fn fnv_bytes<T: Copy>(values: &[T]) -> u64 {
        let bytes = unsafe {
            std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
        };
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0 >> 8
        }
    }

    /// Fixed-bit native witnesses (archived arithmetic witnesses).
    /// Each rejects one wrong formula: reassociated f64 additions, r * (r * n6),
    /// or comparison in f32. Noise holds indices 0, 2, 3, 5, 6, 8.
    #[test]
    fn detector_predicate_matches_native_witnesses() {
        let witnesses: [(u32, u32, [u64; 6], bool); 6] = [
            (
                0x45707bec,
                0x44e7ee1e,
                [
                    0x4075de9dc12d8cd5,
                    0x4007fc7f6939956b,
                    0x3fac15bcca651b70,
                    0x3fc4e9da31cae970,
                    0x3f0d058306f37d48,
                    0x3f17462010908eca,
                ],
                false,
            ),
            (
                0x4522287a,
                0x440681a7,
                [
                    0x4099f147fbc49dcb,
                    0x3ff5f6f4ed88d2d6,
                    0x3fba29ff11a1647f,
                    0x3fbca85b7dc3141d,
                    0x3edad6708387f4ae,
                    0x3f17da6fff78fe29,
                ],
                false,
            ),
            (
                0x454b77dd,
                0x44a5b522,
                [
                    0x408642a69c000000,
                    0x0,
                    0x3fc55d53673fdf74,
                    0x3fbbc00bfb3d2a7f,
                    0x3f05d0518fc1ca3a,
                    0x3f0ab11c7a199d33,
                ],
                false,
            ),
            (
                0x45652718,
                0x44a2181a,
                [
                    0x4096ce6ccd800000,
                    0x0,
                    0x3fc1c3c5e2dac030,
                    0x3fbed084216cd9c2,
                    0x3ef26427d2515f85,
                    0x3ed75ad35dee9e8c,
                ],
                false,
            ),
            (
                0x4443b2f1,
                0x4300847a,
                [
                    0x408181ebfa3b3df5,
                    0x3fdd8e0ba61055ca,
                    0x3fa6d47dbbe4112a,
                    0x3fc54b0039f7168c,
                    0x3f0eef0265bb673a,
                    0x3f121190ab1d3a80,
                ],
                true,
            ),
            (
                0x45293839,
                0x43cdbbd8,
                [
                    0x40983d67b1ef7c00,
                    0x3fa3843841ffdfb8,
                    0x3f9c8058915bfea4,
                    0x3fc66e34ffbfe608,
                    0x3f151b21c5bdb8d9,
                    0x3f059f3af3c46777,
                ],
                true,
            ),
        ];
        for (r, b, n, native) in witnesses {
            let mut noise = [0.0; 9];
            for (slot, bits) in [0, 2, 3, 5, 6, 8].into_iter().zip(n) {
                noise[slot] = f64::from_bits(bits);
            }
            assert_eq!(
                rejects(f32::from_bits(r), f32::from_bits(b), &noise),
                native
            );
        }
    }

    #[test]
    fn invalid_repair_calls_leave_buffers_unchanged() {
        let none = Control::none();
        let noise = [0.0; 9];
        let data = vec![1.5_f32; 3 * 8 * 8];
        let record = BadPixelRecord {
            row: 3,
            column: 3,
            flags: 0x43ff,
        };
        for (len, width, height, stride) in [
            (191, 8, 8, 24),
            (192, 8, 8, 23),
            (192, 0, 8, 24),
            (192, 8, 0, 24),
        ] {
            let mut output = vec![record];
            let mut overrides = vec![7];
            assert!(detect_bad_reds(
                &data[..len],
                width,
                height,
                stride,
                &[record],
                &noise,
                &mut output,
                Some(&mut overrides),
                none
            )
            .is_err());
            assert_eq!((output.len(), overrides), (1, vec![7]));
        }
        let mut full = vec![record; DETECTED_CAPACITY];
        assert!(
            detect_bad_reds(&data, 8, 8, 24, &[record], &noise, &mut full, None, none).is_err()
        );
        assert!(detect_bad_reds(
            &data,
            8,
            8,
            24,
            &[record],
            &[f64::NAN; 9],
            &mut Vec::new(),
            None,
            none
        )
        .is_err());
        // Malformed packed lists: truncated header and payload, a zero
        // channel mask, a duplicate site, a site outside the extents, and a
        // window without the two-pixel halo.
        let site = |dy: u16, dx: u16, mask: u16| (dy << 10) | (dx << 4) | mask;
        for words in [
            vec![0, 3, 3],
            vec![0, 3, 3, 2, site(0, 0, 4)],
            vec![0, 3, 3, 1, site(0, 0, 0)],
            vec![1, 3, 3, 2, site(0, 0, 4), site(0, 0, 2)],
            vec![0, 3, 3, 1, site(0, 1, 4)],
            vec![0, 6, 3, 1, site(0, 0, 4)],
        ] {
            for repair in cluster_repairs() {
                let mut image = data.clone();
                assert!(
                    repair(&mut image, 8, 8, 24, &words, 0, 25, none).is_err(),
                    "{words:?}"
                );
                assert_eq!(image, data);
            }
        }
        // A non-selected malformed record is skipped like the native.
        for repair in cluster_repairs() {
            let mut image = data.clone();
            assert!(repair(&mut image, 8, 8, 24, &[0, 3, 3, 1, site(0, 0, 0)], 1, 25, none).is_ok());
            assert_eq!(image, data);
        }
    }

    type ClusterRepair = for<'a> fn(
        &mut [f32],
        usize,
        usize,
        usize,
        &[u16],
        u32,
        u32,
        Control<'a>,
    ) -> Result<ClusterStats>;

    /// The cluster repairs of this crate.
    fn cluster_repairs() -> Vec<ClusterRepair> {
        vec![fill_bad_pixel_clusters]
    }

    /// graineo addition: the fill restores a linear ramp exactly (every marked pixel finds a clean opposite
    /// pair from the outside in) and leaves all other pixels untouched.
    #[test]
    fn fill_bad_pixel_clusters_per_channel_from_outside() {
        const W: usize = 16;
        let ramp = |x: usize, y: usize, c: usize| (10 * x + 3 * y + 100 * c) as f32;
        let mut data = vec![0.0_f32; W * W * 3];
        for y in 0..W {
            for x in 0..W {
                for c in 0..3 {
                    data[(y * W + x) * 3 + c] = ramp(x, y, c);
                }
            }
        }
        let clean = data.clone();
        let site = |dy: u16, dx: u16, mask: u16| (dy << 10) | (dx << 4) | mask;
        // record at (7, 7), 2×2 sites, badness 0, stored margin 5
        let words = [
            (1 << 7) | 1,
            7,
            7,
            (5 << 8) | 4,
            site(0, 0, 4),
            site(0, 1, 4),
            site(1, 0, 4),
            site(1, 1, 7),
        ];
        let marked = [(7, 7), (8, 7), (7, 8), (8, 8)];
        for &(x, y) in &marked {
            data[(y * W + x) * 3] = 9999.0;
        }
        data[(8 * W + 8) * 3 + 1] = -5.0;
        data[(8 * W + 8) * 3 + 2] = 1e6;
        let stats =
            fill_bad_pixel_clusters(&mut data, W, W, W * 3, &words, 0, 25, Control::none())
                .unwrap();
        assert_eq!((stats.records, stats.points, stats.zero_fallback), (1, 4, 0));
        for y in 0..W {
            for x in 0..W {
                for c in 0..3 {
                    let (a, b) = (data[(y * W + x) * 3 + c], clean[(y * W + x) * 3 + c]);
                    if marked.contains(&(x, y)) {
                        assert!((a - b).abs() < 1e-3, "({x}, {y}, {c}): {a} vs {b}");
                    } else {
                        assert_eq!(a.to_bits(), b.to_bits(), "unmarked ({x}, {y}, {c}) changed");
                    }
                }
            }
        }
        // records of another badness stay untouched
        let mut other = clean.clone();
        other[(7 * W + 7) * 3] = 9999.0;
        let before = other.clone();
        fill_bad_pixel_clusters(&mut other, W, W, W * 3, &words, 1, 25, Control::none()).unwrap();
        assert_eq!(other, before);
    }


    /// The ordered Merrill float stages on the two retained reference files.
    /// The fixture pins each stage hash and detail. The native corpus harness
    /// (archived native harness)
    /// runs the exported native kernels from the dumped stage inputs and
    /// reproduces every pinned hash except radial and offset, which have no
    /// direct native export. The prepared row also pins the as-shot burst
    /// gate, raw saturation-map counts, and offset ISO. Set X3F_REPAIR_REGEN
    /// to print rows. Set X3F_REPAIR_DUMP to a directory to write the harness
    /// inputs.
    #[test]
    fn prepared_float_matches_corpus_pins() {
        if crate::fixtures::missing(&["spp-repair-corpus.tsv"]) {
            return;
        }
        let regen = std::env::var_os("X3F_REPAIR_REGEN").is_some();
        let dump = std::env::var_os("X3F_REPAIR_DUMP").map(std::path::PathBuf::from);
        let fixture = crate::fixtures::get("spp-repair-corpus.tsv");
        let mut checked = 0;
        for name in ["sigma_sd1_merrill_15.x3f", "_P2M0358.X3F"] {
            let Some((x3f, file)) = open_corpus(name) else {
                continue;
            };
            let wb = unsafe { crate::x3f_get_wb(x3f) };
            let mut rows = Vec::new();
            let mut raw = Vec::new();
            let mut radial = Vec::new();
            let mut observe = |stage: &'static str, data: &[f32], detail: String| {
                if stage == "raw" {
                    raw = data.to_vec();
                }
                if stage == "radial" {
                    radial = data.to_vec();
                }
                rows.push(
                    format!("{name}\t{stage}\t{:016x}\t{detail}", fnv1a_f32(data))
                        .trim_end()
                        .to_owned(),
                );
            };
            let prepared = unsafe {
                crate::process::merrill_prepare_float(
                    x3f,
                    wb,
                    true,
                    true,
                    None,
                    Control::none(),
                    &mut observe,
                )
            }
            .unwrap();
            let (width, height, stride) = (prepared.width, prepared.height, prepared.row_stride);
            let burst = unsafe { crate::process::merrill_burst_exposure_adjust_included(x3f) };
            assert!(!burst, "{name} as-shot burst gate");
            let (active, keep, col_range) = unsafe { crate::process::merrill_rects(x3f) }.unwrap();
            let highlight = unsafe {
                crate::process::merrill_highlight_restoration(
                    x3f,
                    active,
                    keep,
                    col_range,
                    Control::none(),
                )
            }
            .unwrap()
            .expect("corpus saturation maps");
            let saturation =
                highlight
                    .sat
                    .chunks_exact(3)
                    .fold([0_usize; 3], |mut counts, channels| {
                        for channel in 0..3 {
                            counts[channel] += usize::from(channels[channel] != 0);
                        }
                        counts
                    });
            let offset = unsafe { crate::process::merrill_cr_offset(x3f, wb, Control::none()) }
                .unwrap()
                .expect("corpus offset calibration");
            rows.push(format!(
                "{name}\tprepared\t{:016x}\t{}x{} stride={} burst={burst} sat={saturation:?} offset_iso={:08x}",
                fnv1a_f32(&prepared.data),
                width,
                height,
                stride,
                offset.iso.to_bits()
            ));
            let primary = unsafe {
                crate::process::merrill_bad_pixel_list(x3f, c"BadPixelsF20", Control::none())
            }
            .unwrap()
            .unwrap();
            let jpeg = unsafe {
                crate::process::merrill_bad_pixel_list(x3f, c"Jpeg_BadClusters", Control::none())
            }
            .unwrap()
            .unwrap_or_default();
            let noise = unsafe { crate::process::merrill_color_noise_model(x3f) }.unwrap();
            let mut detected = Vec::new();
            let mut overrides = Vec::new();
            detect_bad_reds(
                &raw,
                width,
                height,
                stride,
                &primary,
                &noise,
                &mut detected,
                Some(&mut overrides),
                Control::none(),
            )
            .unwrap();
            detect_bad_reds(
                &raw,
                width,
                height,
                stride,
                &jpeg,
                &noise,
                &mut detected,
                None,
                Control::none(),
            )
            .unwrap();
            if name.starts_with("sigma_sd1") {
                // The archived native run on the pinned SD1 raw float copy
                // (archived findings).
                let records: Vec<_> = detected
                    .iter()
                    .map(|r| (r.row, r.column, r.flags))
                    .collect();
                assert_eq!(
                    records,
                    [
                        (766, 4859, 17375),
                        (996, 4737, 17403),
                        (2004, 279, 17399),
                        (3041, 4832, 17403)
                    ]
                );
            }
            if let Some(dir) = &dump {
                let dir = dir.join(name);
                std::fs::create_dir_all(&dir).unwrap();
                write(&dir, "raw.f32", &raw);
                write(&dir, "radial.f32", &radial);
                write(&dir, "primary.u16", &record_words(&primary));
                write(&dir, "jpeg.u16", &record_words(&jpeg));
                write(&dir, "detected.u16", &record_words(&detected));
                write(&dir, "overrides.u32", &overrides);
                write(&dir, "noise.f64", &noise);
                let front = unsafe {
                    crate::process::merrill_float_gain_front(x3f, wb, true, Control::none())
                }
                .unwrap();
                let tables = front.spatial().unwrap();
                for (c, table) in tables.iter().enumerate() {
                    write(&dir, &format!("table{c}.f32"), &table.values);
                }
                let burst = unsafe { crate::process::merrill_burst_exposure_adjust_included(x3f) };
                let (label, offset) = unsafe {
                    crate::process::merrill_badness_selection(x3f, burst, Control::none())
                }
                .unwrap()
                .unwrap();
                let words = unsafe { crate::process::merrill_packed_cluster_words(x3f) }
                    .unwrap()
                    .unwrap_or_default();
                write(&dir, "clusters.u16", &words);
                let mut cluster_offset = 1_u32;
                let mut margin = 25_u32;
                let mut skip = 0_u32;
                unsafe {
                    crate::x3f_get_camf_unsigned(
                        x3f,
                        c"BCBadnessOffset".as_ptr().cast_mut(),
                        &mut cluster_offset,
                    );
                    crate::x3f_get_camf_unsigned(
                        x3f,
                        c"BadClusterMargin".as_ptr().cast_mut(),
                        &mut margin,
                    );
                    crate::x3f_get_camf_unsigned(
                        x3f,
                        c"SkipStepFlags".as_ptr().cast_mut(),
                        &mut skip,
                    );
                }
                let (active, keep, col_range) =
                    unsafe { crate::process::merrill_rects(x3f) }.unwrap();
                let highlight = unsafe {
                    crate::process::merrill_highlight_restoration(
                        x3f,
                        active,
                        keep,
                        col_range,
                        Control::none(),
                    )
                }
                .unwrap()
                .unwrap();
                write(&dir, "sat.u8", &highlight.sat);
                let (dgain, dadjust, drect) = unsafe {
                    crate::process::merrill_despeckle_inputs(x3f, wb, None, Control::none())
                }
                .unwrap()
                .unwrap();
                let bits = |v: &[f64]| {
                    v.iter()
                        .map(|v| format!("{:016x}", v.to_bits()))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                let meta = [
                    format!("size {width} {height} {stride}"),
                    format!("lists {} {} {}", primary.len(), jpeg.len(), detected.len()),
                    format!("badness {label} {offset} {cluster_offset} {margin} {skip}"),
                    format!("black {}", bits(&front.black())),
                    format!("gain {}", bits(&front.gain())),
                    format!(
                        "tables {}",
                        tables
                            .iter()
                            .map(|t| format!("{} {}", t.columns, t.rows))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                    format!(
                        "highlight {:016x} {:?}",
                        highlight.restore.threshold().to_bits(),
                        highlight.valid
                    ),
                    format!("despeckle {} {} {:?}", bits(&dgain), bits(&dadjust), drect),
                    format!("words {}", words.len()),
                ];
                std::fs::write(dir.join("meta.txt"), meta.join("\n") + "\n").unwrap();
                std::fs::write(dir.join("rust.tsv"), rows.join("\n") + "\n").unwrap();
            }
            unsafe {
                crate::x3f_delete(x3f);
                crate::sysabi::fclose(file);
            }
            if regen {
                println!("{}", rows.join("\n"));
            } else {
                let expected: Vec<_> = fixture
                    .lines()
                    .filter(|l| l.starts_with(&format!("{name}\t")))
                    .collect();
                assert_eq!(rows, expected, "{name}");
            }
            checked += 1;
        }
        eprintln!("Merrill prepared float: {checked}/2 corpus files present");
    }
}
