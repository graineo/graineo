//! Filling the parts of a damaged Sigma file that could not be rescued (see `x3f::rescue`).
//!
//! - A pixel with at least one intact plane: the missing plane is estimated from the intact ones,
//!   learnt locally (column tiles, rows next to the gap) where all planes are intact. With two
//!   intact planes as a share of their sum looked up by their ratio (≈ material), with one by a
//!   proportional model. Structure stays, the colour is an approximation (≈ 3–4 % mean deviation
//!   on artificially damaged copies, `graineo-dev fillcheck`).
//! - A pixel without any intact plane: neutral grey (clearly recognisable as missing).
//!
//! For measurements: `GRAINEO_FILL=learn rows,tile,intercept 0|1` and `GRAINEO_FILL_MODE=linear|material`.

use x3f::{RawImage, Rescue};

use crate::calib::Calibration;

/// How much of the image is real, estimated or grey (shares of the rows, 0 … 1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Damage {
    /// rows with all three planes intact
    pub intact: f64,
    /// rows with one or two planes estimated from the others
    pub estimated: f64,
    /// rows without any plane (grey)
    pub missing: f64,
    /// per plane (0 bottom, 1 middle, 2 top): share of the rows really read from the file
    pub read: [f64; 3],
    /// rows filled with neutral grey (half-open ranges, ascending); all other lost rows are estimated
    pub grey: Vec<(usize, usize)>,
}

/// State of a plane in a row: really read, estimated from the other planes, or filled with neutral grey.
pub const READ: u8 = 0;
pub const ESTIMATED: u8 = 1;
pub const GREY: u8 = 2;

impl Damage {
    /// Per row of a sensor view downscaled by `factor` whose first row is sensor row `y0` (`height` rows): the
    /// state of each plane ([`READ`], [`ESTIMATED`], [`GREY`]); a view row covering several sensor rows takes the worst.
    pub fn row_states(&self, rescue: &Rescue, y0: usize, factor: usize, height: usize) -> Vec<[u8; 3]> {
        let factor = factor.max(1);
        (0..height)
            .map(|y| {
                let a = y0 + y * factor;
                let mut st = [READ; 3];
                for r in a..(a + factor).min(rescue.rows) {
                    if self.grey.iter().any(|&(g0, g1)| r >= g0 && r < g1) {
                        return [GREY; 3];
                    }
                    for (p, v) in st.iter_mut().enumerate() {
                        if rescue.is_lost(p, r) {
                            *v = ESTIMATED;
                        }
                    }
                }
                st
            })
            .collect()
    }
}

/// Fills the lost rows of `raw` and returns the shares.
pub fn fill(raw: &mut RawImage, rescue: &Rescue, cal: &Calibration) -> Damage {
    let (rows, cols) = (raw.rows, raw.columns);
    let lost: Vec<[bool; 3]> = (0..rows).map(|r| std::array::from_fn(|p| rescue.is_lost(p, r))).collect();
    let intact = (0..rows).filter(|&r| !lost[r].iter().any(|&l| l)).count();

    // neutral grey: equal after white balance, at 18 % of the saturation of the weakest channel
    let g_min = cal.wb_gain.iter().cloned().fold(f64::MAX, f64::min).max(1e-6);
    let grey: [u16; 3] = std::array::from_fn(|c| (cal.black[c] + 0.18 * cal.saturation * g_min / cal.wb_gain[c].max(1e-6)).round().clamp(0.0, 65535.0) as u16);

    let (mut estimated, mut missing) = (0usize, 0usize);
    let mut grey_rows: Vec<(usize, usize)> = Vec::new();
    // bands of consecutive rows with the same lost planes
    let mut r = 0;
    while r < rows {
        let l = lost[r];
        let end = (r..rows).find(|&q| lost[q] != l).unwrap_or(rows);
        if l.iter().any(|&x| x) {
            let have: Vec<usize> = (0..3).filter(|&p| !l[p]).collect();
            let done = !have.is_empty() && (0..3).filter(|&m| l[m]).all(|m| estimate_band(raw, &lost, r, end, m, &have, cal.black));
            if done {
                estimated += end - r;
            } else {
                missing += end - r;
                grey_rows.push((r, end));
                for q in r..end {
                    for c in 0..cols {
                        for p in 0..3 {
                            raw.planes[p][q * cols + c] = grey[p];
                        }
                    }
                }
            }
        }
        r = end;
    }
    let n = rows.max(1) as f64;
    let read = std::array::from_fn(|p| (0..rows).filter(|&q| !lost[q][p]).count() as f64 / n);
    Damage { intact: intact as f64 / n, estimated: estimated as f64 / n, missing: missing as f64 / n, read, grey: grey_rows }
}

/// How the missing planes are learnt: rows above and below a band, width of the column tiles with their
/// own model (colour relations change across the image), with or without offset term.
#[derive(Clone, Copy)]
struct Params {
    learn_rows: usize,
    tile: usize,
    intercept: bool,
}

// measured on artificially damaged copies (graineo-dev fillcheck): smallest error of the variants tried
const PARAMS: Params = Params { learn_rows: 24, tile: 64, intercept: false };
/// Two intact planes: estimate by material ([`estimate_by_material`]) instead of the linear model.
const MATERIAL: bool = true;
/// Estimates never exceed the 12-bit range of the sensor data.
const MAX_VALUE: f64 = 4095.0;

/// For measurements (graineo-dev): `GRAINEO_FILL=learn,tile,intercept`.
fn params() -> Params {
    std::env::var("GRAINEO_FILL")
        .ok()
        .and_then(|v| {
            let f: Vec<usize> = v.split(',').filter_map(|x| x.parse().ok()).collect();
            (f.len() == 3).then(|| Params { learn_rows: f[0], tile: f[1], intercept: f[2] == 1 })
        })
        .unwrap_or(PARAMS)
}

/// Estimates plane `m` in rows `a..b` from the planes `have`: per column tile a linear model learnt from
/// the nearest rows above and below the band in which all these planes are intact; the models are
/// blended across tiles and from top to bottom so that no seams appear. `false` if there is nothing to learn from.
fn estimate_band(raw: &mut RawImage, lost: &[[bool; 3]], a: usize, b: usize, m: usize, have: &[usize], black: [f64; 3]) -> bool {
    let (rows, cols) = (raw.rows, raw.columns);
    let Params { learn_rows, tile, intercept } = params();
    let tile = tile.clamp(16, cols);
    let usable = |q: usize| !lost[q][m] && have.iter().all(|&p| !lost[q][p]);
    // many learning rows: every k-th (at most 96 rows per side)
    let thin = |v: Vec<usize>| -> Vec<usize> {
        let k = v.len().div_ceil(96).max(1);
        v.into_iter().step_by(k).collect()
    };
    let above: Vec<usize> = thin((0..a).rev().filter(|&q| usable(q)).take(learn_rows).collect());
    let below: Vec<usize> = thin((b..rows).filter(|&q| usable(q)).take(learn_rows).collect());
    if above.len() < 4 && below.len() < 4 {
        return false;
    }
    if have.len() == 2 && std::env::var("GRAINEO_FILL_MODE").map_or(MATERIAL, |v| v == "material") {
        let learn: Vec<usize> = above.iter().chain(&below).copied().collect();
        estimate_by_material(raw, &learn, a, b, m, have, black, tile);
        return true;
    }
    let tiles = cols.div_ceil(tile);
    let models = |learn: &[usize]| -> Option<Vec<Vec<f64>>> {
        if learn.len() < 4 {
            return None;
        }
        Some(
            (0..tiles)
                .map(|t| {
                    let (c0, c1) = (t * tile, ((t + 1) * tile).min(cols));
                    let sample: Vec<usize> = learn.iter().flat_map(|&q| (c0..c1).step_by(2).map(move |c| q * cols + c)).collect();
                    fit(raw, &sample, m, have, black, intercept)
                })
                .collect(),
        )
    };
    let (top, bottom) = (models(&above), models(&below));
    let (top, bottom) = match (top, bottom) {
        (Some(t), Some(b)) => (t, b),
        (Some(t), None) => (t.clone(), t),
        (None, Some(b)) => (b.clone(), b),
        (None, None) => return false,
    };
    let n = have.len() + intercept as usize;
    for q in a..b {
        let wy = if b - a > 1 { (q - a) as f64 / (b - a - 1) as f64 } else { 0.5 };
        for c in 0..cols {
            // position between the tile centres
            let x = (c as f64 + 0.5) / tile as f64 - 0.5;
            let t0 = (x.floor().max(0.0) as usize).min(tiles - 1);
            let t1 = (t0 + 1).min(tiles - 1);
            let wx = (x - t0 as f64).clamp(0.0, 1.0);
            let i = q * cols + c;
            let mut v = black[m];
            for k in 0..n {
                let up = top[t0][k] * (1.0 - wx) + top[t1][k] * wx;
                let down = bottom[t0][k] * (1.0 - wx) + bottom[t1][k] * wx;
                let coef = up * (1.0 - wy) + down * wy;
                v += match (intercept, k) {
                    (true, 0) => coef,
                    (true, k) => coef * (raw.planes[have[k - 1]][i] as f64 - black[have[k - 1]]),
                    (false, k) => coef * (raw.planes[have[k]][i] as f64 - black[have[k]]),
                };
            }
            raw.planes[m][i] = v.round().clamp(0.0, MAX_VALUE) as u16;
        }
    }
    true
}

/// Number of classes of the layer ratio (see [`estimate_by_material`]).
const MATERIAL_BINS: usize = 48;

/// Two intact planes: the missing one as a share of their sum, looked up by their ratio. Surfaces of
/// different material (sky, rock, foliage) differ in that ratio, so each gets its own share, learnt
/// per column tile from the learning rows (classes with few samples from all tiles together).
#[allow(clippy::too_many_arguments)]
fn estimate_by_material(raw: &mut RawImage, learn: &[usize], a: usize, b: usize, m: usize, have: &[usize], black: [f64; 3], tile: usize) {
    let cols = raw.columns;
    let tiles = cols.div_ceil(tile);
    let key = |x0: f64, x1: f64| -> (usize, f64) {
        let (x0, x1) = (x0.max(1.0), x1.max(1.0));
        let c = x0 / (x0 + x1);
        (((c * MATERIAL_BINS as f64) as usize).min(MATERIAL_BINS - 1), x0 + x1)
    };
    // sums of the share and counts per (tile, class); index tiles = all tiles together
    let mut sum = vec![[0f64; MATERIAL_BINS]; tiles + 1];
    let mut cnt = vec![[0u32; MATERIAL_BINS]; tiles + 1];
    for &q in learn {
        for c in 0..cols {
            let i = q * cols + c;
            let (x0, x1) = (raw.planes[have[0]][i] as f64 - black[have[0]], raw.planes[have[1]][i] as f64 - black[have[1]]);
            let (k, total) = key(x0, x1);
            if total < 16.0 {
                continue;
            }
            let share = (raw.planes[m][i] as f64 - black[m]) / total;
            for t in [c / tile, tiles] {
                sum[t][k] += share;
                cnt[t][k] += 1;
            }
        }
    }
    let overall = {
        let (s, n) = (sum[tiles].iter().sum::<f64>(), cnt[tiles].iter().sum::<u32>());
        if n > 0 { s / n as f64 } else { 0.0 }
    };
    let share = |t: usize, k: usize| -> f64 {
        if cnt[t][k] >= 24 {
            sum[t][k] / cnt[t][k] as f64
        } else if cnt[tiles][k] >= 8 {
            sum[tiles][k] / cnt[tiles][k] as f64
        } else {
            overall
        }
    };
    let table: Vec<Vec<f64>> = (0..tiles).map(|t| (0..MATERIAL_BINS).map(|k| share(t, k)).collect()).collect();
    for q in a..b {
        for c in 0..cols {
            let i = q * cols + c;
            let (x0, x1) = (raw.planes[have[0]][i] as f64 - black[have[0]], raw.planes[have[1]][i] as f64 - black[have[1]]);
            let (k, _) = key(x0, x1);
            // blend between neighbouring tiles against seams
            let x = (c as f64 + 0.5) / tile as f64 - 0.5;
            let t0 = (x.floor().max(0.0) as usize).min(tiles - 1);
            let t1 = (t0 + 1).min(tiles - 1);
            let wx = (x - t0 as f64).clamp(0.0, 1.0);
            let r = table[t0][k] * (1.0 - wx) + table[t1][k] * wx;
            raw.planes[m][i] = (black[m] + r * (x0 + x1).max(0.0)).round().clamp(0.0, MAX_VALUE) as u16;
        }
    }
}

/// Least squares `planes[m] − black ≈ Σ k_j · (planes[have[j]] − black)` over the sample positions
/// (proportional, without offset: the planes are linear in light, so the relation holds for dark and
/// bright parts alike and does not bend at edges between materials).
fn fit(raw: &RawImage, sample: &[usize], m: usize, have: &[usize], black: [f64; 3], intercept: bool) -> Vec<f64> {
    let n = have.len() + intercept as usize;
    let mut ata = vec![vec![0f64; n]; n];
    let mut atb = vec![0f64; n];
    for &i in sample {
        let mut x: Vec<f64> = if intercept { vec![1.0] } else { Vec::new() };
        x.extend(have.iter().map(|&p| raw.planes[p][i] as f64 - black[p]));
        let y = raw.planes[m][i] as f64 - black[m];
        for a in 0..n {
            atb[a] += x[a] * y;
            for b in 0..n {
                ata[a][b] += x[a] * x[b];
            }
        }
    }
    solve(ata, atb).unwrap_or_else(|| {
        // degenerate (e.g. all dark): ratio of the means to the first intact plane
        let mean = |p: usize| sample.iter().map(|&i| raw.planes[p][i] as f64 - black[p]).sum::<f64>() / sample.len().max(1) as f64;
        let mut k = vec![0.0; n];
        if intercept {
            k[0] = mean(m);
        } else {
            k[0] = mean(m) / mean(have[0]).max(1.0);
        }
        k
    })
}

/// Gaussian elimination with pivoting for the small normal equations.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[piv][col].abs() < 1e-9 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            for k in col..n {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0f64; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `GRAINEO_TEST_DAMAGED_X3F_DIR` with copies of `_SDI2984.X3F`/`_SDI3314.X3F`/`_SDI3742.X3F` (see
    /// `crates/x3f/tests/damaged.rs`); skipped without.
    #[test]
    fn damaged_files_open_with_rescue() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_DAMAGED_X3F_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_DAMAGED_X3F_DIR not set, skipped");
            return;
        };
        for (name, min_intact) in [("_SDI2984.X3F", 0.85), ("_SDI3314.X3F", 0.1)] {
            let p = dir.join(name);
            if !p.is_file() {
                continue;
            }
            let s = crate::Session::open(&p).unwrap();
            let (_, d) = s.damage.clone().expect("damage reported");
            assert!(d.intact >= min_intact, "{name}: {d:?}");
            assert!((d.intact + d.estimated + d.missing - 1.0).abs() < 1e-9, "{name}: {d:?}");
            // the developed raw data stay in the 12-bit range
            let raw = s.raw();
            for (c, p) in raw.planes.iter().enumerate() {
                // (defect-pixel repair may go slightly above 4095; wild values would show undetected garbage)
                if let Some(i) = p.iter().position(|&v| v > 6000) {
                    panic!("{name}: plane {c} row {} col {} value {} (lost {})", i / raw.columns, i % raw.columns, p[i], s.damage.as_ref().unwrap().0.is_lost(c, i / raw.columns));
                }
            }
        }
        let low = dir.join("_SDI3742.X3F");
        if low.is_file() {
            let s = crate::Session::open(&low).unwrap();
            assert!(s.damage.is_none());
            let [w, h] = s.size();
            assert!(w > 2300 && w < 2400 && h > 1500 && h < 1600, "{w}×{h}");
        }
    }

    #[test]
    fn solves_small_system() {
        let x = solve(vec![vec![2.0, 1.0], vec![1.0, 3.0]], vec![3.0, 5.0]).unwrap();
        assert!((x[0] - 0.8).abs() < 1e-9 && (x[1] - 1.4).abs() < 1e-9);
    }
}

/// Marks the rescued areas in a sensor view (`img` at the size of the preview whose rows `rows` describes): rows
/// whose shown plane was estimated get a yellow tint, rows filled with grey a grey hatching; read rows stay as they
/// are. A single layer view looks at that plane, the other views at the worst of the three.
pub fn mark_rows(img: &mut crate::Rgb16, rows: &[[u8; 3]], view: crate::View) {
    let w = img.width;
    for (y, line) in img.data.chunks_exact_mut(w * 3).enumerate() {
        let Some(st) = rows.get(y) else { break };
        let state = match view {
            crate::View::Layer(c) => st[c.min(2)],
            _ => st.iter().copied().max().unwrap_or(READ),
        };
        match state {
            ESTIMATED => {
                for px in line.chunks_exact_mut(3) {
                    let v = px.iter().map(|&x| x as u32).sum::<u32>() / 3;
                    px[0] = (v * 7 / 10 + 65535 * 3 / 10) as u16;
                    px[1] = (v * 7 / 10 + 50000 * 3 / 10) as u16;
                    px[2] = (v * 55 / 100) as u16;
                }
            }
            GREY => {
                for (x, px) in line.chunks_exact_mut(3).enumerate() {
                    let g = if (x + y) / 5 % 2 == 0 { 0x9a00 } else { 0x5a00 };
                    px.fill(g);
                }
            }
            _ => {}
        }
    }
}
