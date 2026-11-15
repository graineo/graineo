//! Replace defective pixels.
//!
//! Each list entry names the defective channels (mode) and which of the eight
//! neighbours are usable as a source.
//! The pixel is replaced from its permitted neighbours, every channel only from the same channel ([`repair_own`]).

use x3f::RawImage;

use crate::calib::{BadPixel, Calibration};

/// Neighbour offsets in the bit order of the mask: NW, N, NE, W, E, SW, S, SE.
const NEIGHBORS: [(i32, i32); 8] = [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)];

/// Trimmed mean per channel of the permitted neighbours (minimum and maximum dropped when there are more than two).
fn trimmed_mean(neighbors: &[[f32; 3]]) -> Option<[f32; 3]> {
    let mut n = neighbors.len();
    if n == 0 {
        return None;
    }
    let mut sum = [0f32; 3];
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for p in neighbors {
        for c in 0..3 {
            sum[c] += p[c];
            mn[c] = mn[c].min(p[c]);
            mx[c] = mx[c].max(p[c]);
        }
    }
    // Discard outliers (trimmed mean)
    if n > 2 {
        for c in 0..3 {
            sum[c] -= mn[c] + mx[c];
        }
        n -= 2;
    }
    Some(sum.map(|s| s / n as f32))
}

/// Repair of one pixel: it is replaced as a whole from its own permitted
/// neighbours, every channel only from the same channel: the opposite pair with the smallest
/// difference (sum over the channels) is averaged, otherwise the trimmed mean. No channel is
/// estimated from another one. `around[bit]` follows the bit order of the mask.
fn repair_own(around: &[Option<[f32; 3]>; 8], neighbors: &[[f32; 3]]) -> Option<[f32; 3]> {
    let mut best: Option<(f32, [f32; 3])> = None;
    for (a, b) in [(1, 6), (3, 4), (0, 7), (2, 5)] {
        if let (Some(p), Some(q)) = (around[a], around[b]) {
            let g: f32 = (0..3).map(|c| (p[c] - q[c]).abs()).sum();
            if best.is_none_or(|(h, _)| g < h) {
                best = Some((g, std::array::from_fn(|c| 0.5 * (p[c] + q[c]))));
            }
        }
    }
    match best {
        Some((_, n)) => n.iter().all(|x| x.is_finite()).then_some(n),
        None => trimmed_mean(neighbors).filter(|n| n.iter().all(|x| x.is_finite())),
    }
}

/// Replaces the defective pixels directly in the raw data (sensor coordinates).
/// The formulas mix channels, so they work after black subtraction and white balance (as in SPP);
/// the values are converted for that and the result is written back as raw values.
/// Returns the number of replaced pixels.
pub fn replace_bad_pixels_raw(raw: &mut RawImage, cal: &Calibration) -> usize {
    let g = cal.wb_gain.map(|v| v as f32);
    let black = cal.black.map(|v| v as f32);
    let (w, h) = (raw.columns as i32, raw.rows as i32);
    let value = |raw: &RawImage, y: i32, x: i32| -> [f32; 3] {
        let p = raw.pixel(y as usize, x as usize);
        std::array::from_fn(|c| (p[c] as f32 - black[c]) * g[c])
    };
    let mut replaced = 0;
    let mut neighbors = Vec::with_capacity(8);
    for &BadPixel { row, col, flags } in &cal.bad_pixels {
        let badness = (flags >> 8) & 0xf;
        let mask = flags & 0xff;
        if mask == 0 || badness < cal.badness_threshold {
            continue;
        }
        let (y, x) = (row as i32, col as i32);
        if y < 1 || x < 1 || y >= h - 1 || x >= w - 1 {
            continue;
        }
        neighbors.clear();
        let mut around = [None; 8];
        for (bit, (dy, dx)) in NEIGHBORS.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                let p = value(raw, y + dy, x + dx);
                neighbors.push(p);
                around[bit] = Some(p);
            }
        }
        let new = repair_own(&around, &neighbors);
        if let Some(new) = new {
            let i = y as usize * raw.columns + x as usize;
            for c in 0..3 {
                raw.planes[c][i] = (new[c] / g[c] + black[c]).round().clamp(0.0, 65535.0) as u16;
            }
            replaced += 1;
        }
    }
    replaced
}
