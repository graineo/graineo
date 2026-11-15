//! Rescue of damaged TRUE RAW data (Merrill, Classic format 30).
//!
//! Damaged memory cards leave X3F files in which whole blocks of the file were overwritten with
//! zeros or with foreign data, while header, previews and the other planes stay intact. The TRUE
//! code has no restart markers, but it can be picked up again behind a damaged block:
//!
//! 1. Each plane is decoded row by row until the first bit pattern that is no code of the
//!    table (or a value outside 12 bits) – everything before is intact.
//! 2. Behind it, the stream is decoded without row structure. Damaged blocks produce codes
//!    outside the table (zeros as well as foreign data); intact data never does. So the damaged
//!    bit ranges can be told apart from the intact ones in between.
//! 3. The Huffman code resynchronises by itself a few codes after a damaged block. What is
//!    unknown is *where in the image* the following codes belong (row and column) and the start
//!    values of the prediction:
//!    - behind the last damaged block exactly from the number of codes up to the end of the plane,
//!    - otherwise row and column at once by comparing the codes (horizontal prediction
//!      differences) with another plane that is known there, near the position estimated from the
//!      number of bits; without a clear match the part counts as lost (never misplaced),
//!    - start values: the prediction makes the error a constant per row/column parity; it is
//!      taken from the dark shield columns (black level known from the intact rows).
//! 4. Rows that could not be placed are reported as lost ([`Rescue::lost`]); the caller fills
//!    them (e.g. neutral grey) and tells the user how much was rescued.

use rayon::prelude::*;

use crate::bits::BitReader;
use crate::container::ImageHeader;
use crate::error::Result;
use crate::huffman::TrueHuffman;
use crate::raw::{RawImage, TrueLayout};

/// Which rows of which plane could not be recovered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rescue {
    pub rows: usize,
    /// per plane: lost rows as half-open ranges, ascending
    pub lost: [Vec<(usize, usize)>; 3],
}

impl Rescue {
    /// Is row `row` of plane `plane` lost?
    pub fn is_lost(&self, plane: usize, row: usize) -> bool {
        self.lost[plane].iter().any(|&(a, b)| row >= a && row < b)
    }

    /// Is any plane lost in row `row`?
    pub fn row_lost(&self, row: usize) -> bool {
        (0..3).any(|p| self.is_lost(p, row))
    }

    /// Share of rows (all three planes intact) that were rescued, 0 … 1.
    pub fn rescued_fraction(&self) -> f64 {
        if self.rows == 0 {
            return 0.0;
        }
        let ok = (0..self.rows).filter(|&r| !self.row_lost(r)).count();
        ok as f64 / self.rows as f64
    }

    /// Anything lost at all?
    pub fn is_damaged(&self) -> bool {
        self.lost.iter().any(|l| !l.is_empty())
    }
}

/// Dark columns (light shield) of the sensor, inclusive; for the start values behind a damage.
pub type DarkColumns = Option<(usize, usize)>;

/// Decodes a TRUE RAW section (without the 28-byte header) and rescues as much as possible of
/// damaged planes. Lost rows hold 0 in the returned image.
pub fn decode_rescue(body: &[u8], h: ImageHeader, dark: DarkColumns) -> Result<(RawImage, Rescue)> {
    let t = TrueLayout::parse(body)?;
    let huff = TrueHuffman::new(&t.pairs)?;
    let (rows, cols) = (h.rows as usize, h.columns as usize);
    let streams: [&[u8]; 3] = std::array::from_fn(|c| &t.data[t.starts[c]..t.starts[c] + t.plane_size[c] as usize]);

    // 1. + 2.: intact beginning and damaged bit ranges per plane
    let scans: Vec<PlaneScan> = (0..3).into_par_iter().map(|c| scan_plane(&huff, streams[c], t.seeds[c] as i32, rows, cols)).collect();
    let mut planes: Vec<PlaneState> = scans.into_iter().map(|s| PlaneState::new(s, rows, cols)).collect();

    // 3.: place the segments between damaged ranges; the last one (exact) first, then the others
    // with the other planes as reference where possible
    for pass in 0..2 {
        for c in 0..3 {
            let pending: Vec<usize> = (0..planes[c].scan.segments.len()).filter(|&i| !planes[c].placed[i]).collect();
            for i in pending {
                let last = i + 1 == planes[c].scan.segments.len();
                if pass == 0 && !last {
                    continue;
                }
                let refs: Vec<(usize, &PlaneState)> = (0..3).filter(|&o| o != c).map(|o| (o, &planes[o])).collect();
                let placement = place_segment(&huff, streams[c], &planes[c], i, last, &refs, rows, cols);
                planes[c].apply(i, placement, dark);
            }
        }
    }

    let mut rescue = Rescue { rows, ..Default::default() };
    let mut out: [Vec<u16>; 3] = Default::default();
    for (c, p) in planes.into_iter().enumerate() {
        rescue.lost[c] = ranges(&p.row_ok, false);
        out[c] = p.values;
    }
    Ok((RawImage { columns: cols, rows, planes: out }, rescue))
}

/// Half-open ranges of rows where `flags[r] == want`.
fn ranges(flags: &[bool], want: bool) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut start = None;
    for (r, &f) in flags.iter().enumerate() {
        match (f == want, start) {
            (true, None) => start = Some(r),
            (false, Some(s)) => {
                v.push((s, r));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        v.push((s, flags.len()));
    }
    v
}

/// Bits that must separate two damaged ranges to count as separate (8 KiB).
const MERGE_GAP: u64 = 65_536;
/// Distance behind a damaged range before decoding resumes (the code resynchronises within a few codes).
const RESYNC_MARGIN: u64 = 4_096;

/// A run of intact codes between two damaged ranges (bit positions in the plane).
#[derive(Debug, Clone)]
struct Segment {
    start_bit: u64,
    end_bit: u64,
}

struct PlaneScan {
    /// intact rows from the start (values filled)
    good_rows: usize,
    values: Vec<u16>,
    /// start bit of each intact row and of the row after them
    row_bits: Vec<u64>,
    segments: Vec<Segment>,
    total_bits: u64,
}

/// Decodes the intact beginning of a plane and finds the damaged bit ranges behind it.
fn scan_plane(huff: &TrueHuffman, data: &[u8], seed: i32, rows: usize, cols: usize) -> PlaneScan {
    let total_bits = data.len() as u64 * 8;
    let mut values = vec![0u16; rows * cols];
    let mut row_bits = Vec::with_capacity(rows + 1);
    let mut br = BitReader::new(data);
    let mut row_start = [[seed; 2]; 2];
    let mut first_bad = None;
    'rows: for row in 0..rows {
        row_bits.push(br.bit_position());
        let mut acc = [0i32; 2];
        let mut invalid = 0usize;
        let mut bad = false;
        for col in 0..cols {
            let d = huff.decode_diff_counting(&mut br, &mut invalid);
            let prev = if col < 2 { row_start[row & 1][col & 1] } else { acc[col & 1] };
            let v = prev + d;
            acc[col & 1] = v;
            if col < 2 {
                row_start[row & 1][col & 1] = v;
            }
            bad |= !(0..=4095).contains(&v);
            values[row * cols + col] = v.clamp(0, u16::MAX as i32) as u16;
        }
        if invalid > 0 || bad || br.bit_position() > total_bits {
            first_bad = Some(row);
            break 'rows;
        }
    }
    let Some(first_bad) = first_bad else {
        row_bits.push(br.bit_position());
        return PlaneScan { good_rows: rows, values, row_bits, segments: Vec::new(), total_bits };
    };
    // the row before the first visible error may already contain foreign data
    let good_rows = first_bad.saturating_sub(1);
    row_bits.truncate(good_rows + 1);
    for v in &mut values[good_rows * cols..] {
        *v = 0;
    }

    // damaged ranges: positions of codes outside the table, behind the intact beginning
    let from = row_bits[good_rows];
    let mut br = BitReader::new(data);
    skip_bits(&mut br, from);
    let mut damaged: Vec<(u64, u64)> = Vec::new();
    loop {
        let pos = br.bit_position();
        if pos >= total_bits {
            break;
        }
        let mut invalid = 0usize;
        huff.decode_diff_counting(&mut br, &mut invalid);
        if invalid > 0 {
            match damaged.last_mut() {
                Some(last) if pos < last.1 + MERGE_GAP => last.1 = pos + 8,
                _ => damaged.push((pos, pos + 8)),
            }
        }
    }
    // the damage starts where the intact beginning ends (foreign data does not always produce
    // invalid codes right away)
    match damaged.first_mut() {
        Some(first) if first.0 < from + MERGE_GAP => first.0 = from,
        _ => damaged.insert(0, (from, from)),
    }
    let mut segments = Vec::new();
    for (i, d) in damaged.iter().enumerate() {
        let start_bit = d.1 + RESYNC_MARGIN;
        let end_bit = damaged.get(i + 1).map_or(total_bits, |n| n.0);
        if end_bit > start_bit + 4 * 8 * cols as u64 {
            segments.push(Segment { start_bit, end_bit });
        }
    }
    PlaneScan { good_rows, values, row_bits, segments, total_bits }
}

fn skip_bits(br: &mut BitReader, mut n: u64) {
    while n > 0 {
        let k = n.min(24) as u32;
        br.read(k);
        n -= k as u64;
    }
}

/// Codes of a segment, decoded without row structure: the differences and the bit position
/// before each code; the first codes (resynchronisation) are skipped.
fn segment_codes(huff: &TrueHuffman, data: &[u8], seg: &Segment, total_bits: u64) -> (Vec<i32>, u64, u64) {
    let mut br = BitReader::new(data);
    skip_bits(&mut br, seg.start_bit);
    // resynchronisation: 64 codes
    for _ in 0..64 {
        huff.decode_diff(&mut br);
    }
    let first_bit = br.bit_position();
    let mut d = Vec::new();
    let mut last_bit = first_bit;
    loop {
        let pos = br.bit_position();
        if pos >= seg.end_bit || pos >= total_bits {
            break;
        }
        let mut invalid = 0usize;
        let v = huff.decode_diff_counting(&mut br, &mut invalid);
        if invalid > 0 || br.bit_position() > total_bits {
            // end of the plane (padding) or the next damage
            break;
        }
        d.push(v);
        last_bit = br.bit_position();
    }
    (d, first_bit, last_bit)
}

/// Where the codes of a segment belong.
struct Placement {
    codes: Vec<i32>,
    /// bit position of the first code and behind the last code
    first_bit: u64,
    last_bit: u64,
    /// absolute code index (row · columns + column) of `codes[0]`
    start: Option<usize>,
}

struct PlaneState {
    scan: PlaneScan,
    values: Vec<u16>,
    row_ok: Vec<bool>,
    placed: Vec<bool>,
    /// (absolute code index, bit position) behind the intact beginning and behind each placed
    /// segment: anchors for estimates
    prefix_anchor: (usize, u64),
    /// per placed segment: (code index, bit position) behind it and its bits per code
    anchors: Vec<Option<(usize, u64, f64)>>,
    cols: usize,
}

impl PlaneState {
    fn new(scan: PlaneScan, rows: usize, cols: usize) -> Self {
        let mut row_ok = vec![false; rows];
        row_ok[..scan.good_rows].iter_mut().for_each(|r| *r = true);
        let values = scan.values.clone();
        let placed = vec![false; scan.segments.len()];
        let prefix_anchor = (scan.good_rows * cols, scan.row_bits[scan.good_rows]);
        let anchors = vec![None; scan.segments.len()];
        Self { scan, values, row_ok, placed, prefix_anchor, anchors, cols }
    }

    /// Writes the codes of a placed segment as values; start values from the dark columns.
    fn apply(&mut self, i: usize, p: Placement, dark: DarkColumns) {
        self.placed[i] = true;
        let Some(start) = p.start else { return };
        if !p.codes.is_empty() {
            self.anchors[i] = Some((start + p.codes.len(), p.last_bit, (p.last_bit - p.first_bit) as f64 / p.codes.len() as f64));
        }
        let cols = self.cols;
        let rows = self.row_ok.len();
        // first and last complete row of the segment; one row less at the end (damage may start before
        // its first invalid code)
        let first_row = start.div_ceil(cols);
        let end = start + p.codes.len();
        let last_row = if end == rows * cols { rows } else { (end / cols).saturating_sub(1).min(rows) };
        if last_row <= first_row + 2 {
            return;
        }
        let mut rel = vec![0i32; (last_row - first_row) * cols];
        let mut row_start = [[0i32; 2]; 2];
        for row in first_row..last_row {
            let mut acc = [0i32; 2];
            for col in 0..cols {
                let d = p.codes[row * cols + col - start];
                let prev = if col < 2 { row_start[row & 1][col & 1] } else { acc[col & 1] };
                let v = prev + d;
                acc[col & 1] = v;
                if col < 2 {
                    row_start[row & 1][col & 1] = v;
                }
                rel[(row - first_row) * cols + col] = v;
            }
        }
        let (offsets, mut consistent) = self.offsets(&rel, first_row, last_row, dark);
        // foreign data can fit the dark level by chance for a row or two: keep only runs of at
        // least 8 consistent rows (real data forms long runs)
        let mut r = 0;
        while r < consistent.len() {
            let run = consistent[r..].iter().take_while(|&&c| c == consistent[r]).count();
            if consistent[r] && run < 8 {
                consistent[r..r + run].iter_mut().for_each(|c| *c = false);
            }
            r += run;
        }
        for row in first_row..last_row {
            if self.row_ok[row] {
                // already known (intact beginning or another segment): never overwrite
                continue;
            }
            let mut bad = 0usize;
            let line = &rel[(row - first_row) * cols..(row - first_row + 1) * cols];
            for (col, &v) in line.iter().enumerate() {
                let v = v + offsets[row & 1][col & 1];
                bad += !(-64..=4095 + 64).contains(&v) as usize;
                self.values[row * cols + col] = v.clamp(0, 4095) as u16;
            }
            // a row that leaves the value range or whose dark level does not fit was not placed correctly
            let ok = bad <= cols / 100 && consistent[row - first_row];
            self.row_ok[row] = ok;
            if !ok {
                self.values[row * cols..(row + 1) * cols].iter_mut().for_each(|v| *v = 0);
            }
        }
    }

    /// Start values of the prediction (constant per row/column parity) and which segment rows are
    /// consistent with them. With dark columns: every intact row has its own dark level equal to the
    /// black level of the known rows; the common offset is the median over the rows, refined with the
    /// rows nearest to the known part (the black level drifts slowly). Rows whose dark level does not
    /// fit (foreign data that happens to decode without invalid codes) are reported as inconsistent.
    fn offsets(&self, rel: &[i32], first_row: usize, last_row: usize, dark: DarkColumns) -> ([[i32; 2]; 2], Vec<bool>) {
        let cols = self.cols;
        let n = last_row - first_row;
        let mut out = [[0i32; 2]; 2];
        let mut consistent = vec![true; n];
        let reference_rows: Vec<usize> = (0..self.row_ok.len()).filter(|&r| self.row_ok[r] && (r < first_row || r >= last_row)).collect();
        const NEAR: usize = 200;
        let distance = |r: usize| if r < first_row { first_row - r } else { r + 1 - last_row };
        let mut near = reference_rows.clone();
        near.sort_by_key(|&r| distance(r));
        near.truncate(NEAR);
        let above = near.first().is_some_and(|&r| r < first_row);
        let columns = |cp: usize| -> Vec<usize> {
            match dark {
                Some((a, b)) if b < cols && a <= b => (a..=b).filter(|c| c & 1 == cp).collect(),
                _ => Vec::new(),
            }
        };
        if !near.is_empty() && !columns(0).is_empty() && !columns(1).is_empty() {
            for rp in 0..2 {
                for cp in 0..2 {
                    let cs = columns(cp);
                    let mut known: Vec<i32> = Vec::new();
                    for &row in near.iter().filter(|r| *r & 1 == rp) {
                        known.extend(cs.iter().map(|&c| self.values[row * cols + c] as i32));
                    }
                    let black = mean(&known);
                    // offset each segment row would need
                    let per_row: Vec<(usize, f64)> = (first_row..last_row)
                        .filter(|r| r & 1 == rp)
                        .map(|r| {
                            let v: Vec<i32> = cs.iter().map(|&c| rel[(r - first_row) * cols + c]).collect();
                            (r, black - mean(&v))
                        })
                        .collect();
                    if per_row.is_empty() {
                        continue;
                    }
                    let mut rounded: Vec<i32> = per_row.iter().map(|x| x.1.round() as i32).collect();
                    let rough = median(&mut rounded);
                    let fits: Vec<&(usize, f64)> = per_row.iter().filter(|x| (x.1 - rough as f64).abs() <= 8.0).collect();
                    for x in &per_row {
                        if (x.1 - rough as f64).abs() > 8.0 {
                            consistent[x.0 - first_row] = false;
                        }
                    }
                    // precise: mean over the fitting rows nearest to the known part (integer offset → round)
                    let mut nearest: Vec<&&(usize, f64)> = fits.iter().collect();
                    nearest.sort_by_key(|x| if above { x.0 } else { usize::MAX - x.0 });
                    nearest.truncate(NEAR / 2);
                    if !nearest.is_empty() {
                        out[rp][cp] = (nearest.iter().map(|x| x.1).sum::<f64>() / nearest.len() as f64).round() as i32;
                    } else {
                        out[rp][cp] = rough;
                    }
                }
            }
            return (out, consistent);
        }
        // without dark columns: match the nearest intact row of the same parity above
        for rp in 0..2 {
            for cp in 0..2 {
                if let Some(&above) = reference_rows.iter().filter(|r| **r < first_row && *r & 1 == rp).last() {
                    let row = if first_row & 1 == rp { first_row } else { first_row + 1 };
                    if row >= last_row {
                        continue;
                    }
                    let mut d: Vec<i32> = (cp..cols)
                        .step_by(2)
                        .map(|c| self.values[above * cols + c] as i32 - rel[(row - first_row) * cols + c])
                        .collect();
                    out[rp][cp] = median(&mut d);
                }
            }
        }
        (out, consistent)
    }

    /// Is row `r` known (intact or rescued)?
    fn known(&self, r: usize) -> bool {
        self.row_ok.get(r).copied().unwrap_or(false)
    }
}

fn mean(v: &[i32]) -> f64 {
    v.iter().map(|&x| x as f64).sum::<f64>() / v.len().max(1) as f64
}

fn median(v: &mut [i32]) -> i32 {
    let mid = v.len() / 2;
    *v.select_nth_unstable(mid).1
}

/// Finds the absolute code index of the first code of segment `i` of plane `p`.
fn place_segment(
    huff: &TrueHuffman,
    data: &[u8],
    p: &PlaneState,
    i: usize,
    last: bool,
    refs: &[(usize, &PlaneState)],
    rows: usize,
    cols: usize,
) -> Placement {
    let seg = &p.scan.segments[i];
    let (codes, first_bit, last_bit) = segment_codes(huff, data, seg, p.scan.total_bits);
    let total = rows * cols;
    if codes.len() < 8 * cols {
        return Placement { codes, first_bit, last_bit, start: None };
    }

    if std::env::var_os("X3F_RESCUE_LOG").is_some() {
        eprintln!("segment {i} (last {last}): bits {}..{} first code at bit {first_bit}, codes {}", seg.start_bit, seg.end_bit, codes.len());
    }

    // estimate of the position from the bits: behind the last securely placed part, with the bit rate
    // of that part (image detail changes slowly) or of the segment itself
    let (anchor_code, anchor_bit, rate) = p.anchors[..i].iter().rev().find_map(|a| *a).unwrap_or_else(|| {
        let (c, b) = p.prefix_anchor;
        let rate = if p.scan.good_rows >= 8 { b as f64 / c.max(1) as f64 } else { (seg.end_bit - first_bit) as f64 / codes.len() as f64 };
        (c, b, rate)
    });
    let estimate = anchor_code as f64 + (first_bit.saturating_sub(anchor_bit)) as f64 / rate.max(0.5);
    // uncertainty grows with the distance to the anchor
    let window_rows = (20.0 + 0.15 * (estimate - anchor_code as f64) / cols as f64).min(120.0) as usize;

    if last {
        // exact: the codes run up to the end of the plane (the padding bits are zeros = no code)
        let end_start = total - codes.len();
        let start = cross_search(&codes, refs, end_start, 0, 3, cols, total).map_or(end_start, |(s, _)| s);
        return Placement { codes, first_bit, last_bit, start: Some(start) };
    }

    // position by comparison with another plane that is known there (row and column at once)
    if let Some((start, _)) = cross_search(&codes, refs, estimate.max(0.0) as usize, window_rows * cols, window_rows * cols, cols, total) {
        return Placement { codes, first_bit, last_bit, start: Some(start) };
    }
    // no unambiguous position (e.g. sky without structure in the reference): rather lost than misplaced
    Placement { start: None, codes, first_bit, last_bit }
}

/// Searches the start index of `codes` in `center − before … center + after` by correlating the codes
/// (horizontal prediction differences) with the same differences of a plane that is known there.
/// Returns the start and the score if there is a clear maximum.
fn cross_search(codes: &[i32], refs: &[(usize, &PlaneState)], center: usize, before: usize, after: usize, cols: usize, total: usize) -> Option<(usize, f64)> {
    let span = codes.len().min(12 * cols);
    let lo = center.saturating_sub(before);
    let hi = (center + after).min(total.saturating_sub(codes.len()));
    if hi < lo {
        return None;
    }
    // coarse with few samples over all candidates, then fine for the best ones
    let coarse_sample: Vec<usize> = (0..span).step_by((span / 500).max(1)).collect();
    let coarse = score_candidates(codes, refs, (lo..=hi).collect(), &coarse_sample, span, cols);
    let mut sorted = coarse.clone();
    sorted.sort_by(|x, y| y.0.total_cmp(&x.0));
    // the 64 best, plus the 32 best more than 16 columns away from the coarse maximum (so that the
    // competitors are measured finely as well, not only the neighbours of the maximum)
    let lead = sorted.first().map_or(0, |t| t.1 % cols);
    let away = |s: usize| {
        let d = (s % cols).abs_diff(lead);
        d.min(cols - d) > 16
    };
    let mut top: Vec<usize> = sorted.iter().take(64).map(|t| t.1).collect();
    top.extend(sorted.iter().filter(|t| away(t.1)).take(32).map(|t| t.1));
    top.sort_unstable();
    top.dedup();
    let fine_sample: Vec<usize> = (0..span).step_by(3).collect();
    let scores = score_candidates(codes, refs, top, &fine_sample, span, cols);
    let best = scores.iter().copied().max_by(|x, y| x.0.total_cmp(&y.0))?;
    // neighbouring rows and columns look alike (the image is smooth): the maximum has to stand out
    // against the candidates more than 16 columns away; row and nearby columns follow the maximum
    let far = |s: usize| {
        let d = (s % cols).abs_diff(best.1 % cols);
        d.min(cols - d) > 16
    };
    let fine_second = scores.iter().filter(|(_, s)| far(*s)).map(|x| x.0).fold(0.0f64, f64::max);
    let coarse_best = coarse.iter().filter(|(_, s)| *s == best.1).map(|x| x.0).next().unwrap_or(best.0);
    let coarse_second = coarse.iter().filter(|(_, s)| far(*s)).map(|x| x.0).fold(0.0f64, f64::max);
    // (the coarse scores only preselect: with few samples their noise is too high for a decision)
    let clear = best.0 > 0.15 && best.0 > 1.5 * fine_second;
    if std::env::var_os("X3F_RESCUE_LOG").is_some() {
        eprintln!(
            "cross search {lo}..{hi}: {} candidates, best {:.3} at {} (row {} col {}), second fine {fine_second:.3} coarse {coarse_best:.3}/{coarse_second:.3} → {clear}",
            coarse.len(), best.0, best.1, best.1 / cols, best.1 % cols
        );
    }
    if !clear {
        return None;
    }
    // the column is certain now; neighbouring rows of the same column are almost as similar, so the row
    // is decided with all codes of up to 64 rows
    let long = codes.len().min(64 * cols);
    let every: Vec<usize> = (0..long).collect();
    let rows_near: Vec<usize> = (-3i64..=3)
        .map(|k| best.1 as i64 + k * cols as i64)
        .filter(|&s| s >= 0 && s as usize + long <= total)
        .map(|s| s as usize)
        .collect();
    let refined = score_candidates(codes, refs, rows_near, &every, long, cols);
    let pick = refined.into_iter().max_by(|x, y| x.0.total_cmp(&y.0)).map_or(best, |r| r);
    if std::env::var_os("X3F_RESCUE_LOG").is_some() && pick.1 != best.1 {
        eprintln!("  row refined: {} → {}", best.1 / cols, pick.1 / cols);
    }
    Some((pick.1, pick.0))
}

/// Correlation of the codes with the horizontal differences of a known reference plane, per start candidate.
fn score_candidates(codes: &[i32], refs: &[(usize, &PlaneState)], candidates: Vec<usize>, sample: &[usize], span: usize, cols: usize) -> Vec<(f64, usize)> {
    let clip = |v: f64| v.clamp(-64.0, 64.0);
    candidates
        .into_par_iter()
        .filter_map(|s| {
            let rows = (s / cols, (s + span) / cols + 1);
            let (_, rp) = refs.iter().find(|(_, rp)| (rows.0..rows.1).all(|q| rp.known(q)))?;
            let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
            for &k in sample {
                let i = s + k;
                if i % cols < 2 {
                    continue;
                }
                let a = clip(codes[k] as f64);
                let b = clip(rp.values[i] as f64 - rp.values[i - 2] as f64);
                sab += a * b;
                saa += a * a;
                sbb += b * b;
            }
            (saa > 0.0 && sbb > 0.0).then(|| (sab / (saa * sbb).sqrt(), s))
        })
        .collect()
}
