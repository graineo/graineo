//! RCD demosaicing ("Ratio Corrected Demosaicing") for Bayer sensors.
//!
//! Origin: Rust port of `rtengine/rcd_demosaic.cc` from **RawTherapee**
//! (dev branch, retrieved 2026-10-27), license **GPL-3.0-or-later**.
//! Copyright (c) 2017-2020 Luis Sanz Rodriguez and Ingo Weyrich; tiled version by Ingo Weyrich,
//! coordinated with Hanno Schwalm. Original algorithm: <https://github.com/LuisSR/RCD-Demosaicing>
//! (release 2.3, GPL-3).
//!
//! Changes from the original: Rust instead of C++, tiles via rayon instead of OpenMP (one band per tile
//! row, bands write disjoint rows), input is already normalized to 0…1 (instead of /65536),
//! the border (9 px) is filled by the caller with the simple averaging method instead of `border_interpolate`.
//! Computation steps 1–4.3 are taken over unchanged.

use rayon::prelude::*;

const TILE_BORDER: usize = 9;
pub(crate) const RCD_BORDER: usize = 9;
const TS: usize = 194;
const TSN: usize = TS - 2 * TILE_BORDER;
const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;

#[inline(always)]
fn sqr(x: f32) -> f32 {
    x * x
}

#[inline(always)]
fn intp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}

/// RCD over the whole image. `cfa` are normalized mosaic values (w×h), `pat[y&1][x&1]` the color
/// (0 = R, 1 = G, 2 = B) relative to the image origin. Result: RGB per pixel; a border of
/// [`RCD_BORDER`] pixels stays 0 and must be filled by the caller.
pub(crate) fn rcd(cfa_in: &[f32], w: usize, h: usize, pat: [[usize; 2]; 2]) -> Vec<[f32; 3]> {
    let mut out = vec![[0f32; 3]; w * h];
    if w < 2 * TS.min(32) || h < 32 {
        return out;
    }
    let num_th = h.div_ceil(TSN);
    let num_tw = w.div_ceil(TSN);
    // bands: per tile row the rows [rowStart+9, rowEnd-9)
    let mut bands: Vec<(usize, usize, usize)> = Vec::new(); // (tr, a, b)
    for tr in 0..num_th {
        let row_start = tr * TSN;
        let row_end = (row_start + TS).min(h);
        let a = row_start + TILE_BORDER;
        let b = row_end.saturating_sub(TILE_BORDER);
        if a < b {
            bands.push((tr, a, b));
        }
    }
    // split the output into disjoint band slices
    let mut slices: Vec<(usize, usize, &mut [[f32; 3]])> = Vec::with_capacity(bands.len());
    {
        let mut rest: &mut [[f32; 3]] = &mut out[..];
        let mut pos = 0usize; // first row of `rest`
        for &(tr, a, b) in &bands {
            let (_, r) = rest.split_at_mut((a - pos) * w);
            let (band, r2) = r.split_at_mut((b - a) * w);
            slices.push((tr, a, band));
            rest = r2;
            pos = b;
        }
    }
    slices.into_par_iter().for_each(|(tr, a, band)| {
        let mut t = Tile::new();
        for tc in 0..num_tw {
            t.run(cfa_in, w, h, pat, tr, tc, a, band);
        }
    });
    out
}

struct Tile {
    cfa: Vec<f32>,
    rgb: [Vec<f32>; 3],
    vh_dir: Vec<f32>,
    pq_dir: Vec<f32>,
    lpf: Vec<f32>,
    p_hpf: Vec<f32>,
    q_hpf: Vec<f32>,
}

impl Tile {
    fn new() -> Self {
        let n = TS * TS;
        Self {
            cfa: vec![0.0; n],
            rgb: [vec![0.0; n], vec![0.0; n], vec![0.0; n]],
            vh_dir: vec![0.0; n],
            pq_dir: vec![0.0; n / 2],
            lpf: vec![0.0; n / 2],
            p_hpf: vec![0.0; n / 2],
            q_hpf: vec![0.0; n / 2],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run(&mut self, raw: &[f32], w: usize, h: usize, pat: [[usize; 2]; 2], tr: usize, tc: usize, band_a: usize, band: &mut [[f32; 3]]) {
        let fc = |r: usize, c: usize| pat[r & 1][c & 1];
        let row_start = tr * TSN;
        let row_end = (row_start + TS).min(h);
        let col_start = tc * TSN;
        let col_end = (col_start + TS).min(w);
        if row_start + TILE_BORDER >= row_end.saturating_sub(TILE_BORDER) || col_start + TILE_BORDER >= col_end.saturating_sub(TILE_BORDER) {
            return;
        }
        let tile_rows = row_end - row_start;
        let tile_cols = col_end - col_start;
        let (w1, w2, w3, w4) = (TS as isize, 2 * TS as isize, 3 * TS as isize, 4 * TS as isize);
        let cfa = &mut self.cfa;
        for row in row_start..row_end {
            let c0 = fc(row, col_start);
            let c1 = fc(row, col_start + 1);
            let mut indx = (row - row_start) * TS;
            for col in col_start..col_end {
                let v = raw[row * w + col].clamp(0.0, 1.0);
                cfa[indx] = v;
                self.rgb[c0][indx] = v;
                self.rgb[c1][indx] = v;
                indx += 1;
            }
        }
        let cfa = &self.cfa;
        let at = |i: usize, d: isize| -> f32 { cfa[(i as isize + d) as usize] };

        // Step 1: directions (vertical/horizontal)
        let mut bv = [vec![0f32; TS], vec![0f32; TS], vec![0f32; TS]];
        let vterm = |indx: usize| -> f32 {
            sqr((at(indx, -w3) - at(indx, -w1) - at(indx, w1) + at(indx, w3)) - 3.0 * (at(indx, -w2) + at(indx, w2)) + 6.0 * cfa[indx])
        };
        for row in 3..(tile_rows - 3).min(5) {
            for col in 4..tile_cols - 4 {
                bv[row - 3][col - 4] = vterm(row * TS + col);
            }
        }
        let mut bh = vec![0f32; TS];
        let (mut i0, mut i1, mut i2) = (0usize, 1usize, 2usize);
        for row in 4..tile_rows - 4 {
            for col in 3..tile_cols - 3 {
                let indx = row * TS + col;
                bh[col - 3] = sqr((at(indx, -3) - at(indx, -1) - at(indx, 1) + at(indx, 3)) - 3.0 * (at(indx, -2) + at(indx, 2)) + 6.0 * cfa[indx]);
            }
            for col in 4..tile_cols - 4 {
                bv[i2][col - 4] = vterm((row + 1) * TS + col);
            }
            for col in 4..tile_cols - 4 {
                let indx = row * TS + col;
                let v_stat = EPSSQ.max(bv[i0][col - 4] + bv[i1][col - 4] + bv[i2][col - 4]);
                let h_stat = EPSSQ.max(bh[col - 4] + bh[col - 3] + bh[col - 2]);
                self.vh_dir[indx] = v_stat / (v_stat + h_stat);
            }
            // V0,V1,V2 → V1,V2,V0
            let t = i0;
            i0 = i1;
            i1 = i2;
            i2 = t;
        }

        // Step 2: low pass at R/B positions
        for row in 2..tile_rows - 2 {
            let mut col = 2 + (fc(row, 0) & 1);
            while col < tile_cols - 2 {
                let indx = row * TS + col;
                self.lpf[indx / 2] = cfa[indx]
                    + 0.5 * (at(indx, -w1) + at(indx, w1) + at(indx, -1) + at(indx, 1))
                    + 0.25 * (at(indx, -w1 - 1) + at(indx, -w1 + 1) + at(indx, w1 - 1) + at(indx, w1 + 1));
                col += 2;
            }
        }

        // Step 3: green at R/B positions
        let lpf = &self.lpf;
        let vh = &self.vh_dir;
        for row in 4..tile_rows - 4 {
            let mut col = 4 + (fc(row, 0) & 1);
            while col < tile_cols - 4 {
                let indx = row * TS + col;
                let lp = indx / 2;
                let cfai = cfa[indx];
                let n_deg = EPS + ((at(indx, -w1) - at(indx, w1)).abs() + (cfai - at(indx, -w2)).abs()) + ((at(indx, -w1) - at(indx, -w3)).abs() + (at(indx, -w2) - at(indx, -w4)).abs());
                let s_deg = EPS + ((at(indx, -w1) - at(indx, w1)).abs() + (cfai - at(indx, w2)).abs()) + ((at(indx, w1) - at(indx, w3)).abs() + (at(indx, w2) - at(indx, w4)).abs());
                let w_deg = EPS + ((at(indx, -1) - at(indx, 1)).abs() + (cfai - at(indx, -2)).abs()) + ((at(indx, -1) - at(indx, -3)).abs() + (at(indx, -2) - at(indx, -4)).abs());
                let e_deg = EPS + ((at(indx, -1) - at(indx, 1)).abs() + (cfai - at(indx, 2)).abs()) + ((at(indx, 1) - at(indx, 3)).abs() + (at(indx, 2) - at(indx, 4)).abs());
                let lpfi = lpf[lp];
                let n_est = at(indx, -w1) * (lpfi + lpfi) / (EPS + lpfi + lpf[lp - TS]);
                let s_est = at(indx, w1) * (lpfi + lpfi) / (EPS + lpfi + lpf[lp + TS]);
                let w_est = at(indx, -1) * (lpfi + lpfi) / (EPS + lpfi + lpf[lp - 1]);
                let e_est = at(indx, 1) * (lpfi + lpfi) / (EPS + lpfi + lpf[lp + 1]);
                let v_est = (s_deg * n_est + n_deg * s_est) / (n_deg + s_deg);
                let h_est = (w_deg * e_est + e_deg * w_est) / (e_deg + w_deg);
                let c = vh[indx];
                let nb = 0.25 * ((vh[indx - TS - 1] + vh[indx - TS + 1]) + (vh[indx + TS - 1] + vh[indx + TS + 1]));
                let disc = if (0.5 - c).abs() < (0.5 - nb).abs() { nb } else { c };
                self.rgb[1][indx] = intp(disc, h_est, v_est);
                col += 2;
            }
        }

        // Step 4.0: diagonal high pass
        for row in 3..tile_rows - 3 {
            let mut col = 3;
            while col < tile_cols - 3 {
                let indx = row * TS + col;
                let i2 = indx / 2;
                self.p_hpf[i2] = sqr((at(indx, -w3 - 3) - at(indx, -w1 - 1) - at(indx, w1 + 1) + at(indx, w3 + 3)) - 3.0 * (at(indx, -w2 - 2) + at(indx, w2 + 2)) + 6.0 * cfa[indx]);
                self.q_hpf[i2] = sqr((at(indx, -w3 + 3) - at(indx, -w1 + 1) - at(indx, w1 - 1) + at(indx, w3 - 3)) - 3.0 * (at(indx, -w2 + 2) + at(indx, w2 - 2)) + 6.0 * cfa[indx]);
                col += 2;
            }
        }
        // Step 4.1: diagonal directions
        for row in 4..tile_rows - 4 {
            let mut col = 4 + (fc(row, 0) & 1);
            while col < tile_cols - 4 {
                let indx = row * TS + col;
                let i2 = indx / 2;
                let i3 = (indx - TS - 1) / 2;
                let i4 = (indx + TS - 1) / 2;
                let p = EPSSQ.max(self.p_hpf[i3] + self.p_hpf[i2] + self.p_hpf[i4 + 1]);
                let q = EPSSQ.max(self.q_hpf[i3 + 1] + self.q_hpf[i2] + self.q_hpf[i4]);
                self.pq_dir[i2] = p / (p + q);
                col += 2;
            }
        }
        // Step 4.2: R at B and B at R
        for row in 4..tile_rows - 4 {
            let mut col = 4 + (fc(row, 0) & 1);
            while col < tile_cols - 4 {
                let indx = row * TS + col;
                let c = 2 - fc(row, col);
                let pq = indx / 2;
                let pq2 = (indx - TS - 1) / 2;
                let pq3 = (indx + TS - 1) / 2;
                let pqd = &self.pq_dir;
                let cv = pqd[pq];
                let nb = 0.25 * (pqd[pq2] + pqd[pq2 + 1] + pqd[pq3] + pqd[pq3 + 1]);
                let disc = if (0.5 - cv).abs() < (0.5 - nb).abs() { nb } else { cv };
                let rc = &self.rgb[c];
                let g = &self.rgb[1];
                let o = |d: isize| (indx as isize + d) as usize;
                let nw_deg = EPS + (rc[o(-w1 - 1)] - rc[o(w1 + 1)]).abs() + (rc[o(-w1 - 1)] - rc[o(-w3 - 3)]).abs() + (g[indx] - g[o(-w2 - 2)]).abs();
                let ne_deg = EPS + (rc[o(-w1 + 1)] - rc[o(w1 - 1)]).abs() + (rc[o(-w1 + 1)] - rc[o(-w3 + 3)]).abs() + (g[indx] - g[o(-w2 + 2)]).abs();
                let sw_deg = EPS + (rc[o(-w1 + 1)] - rc[o(w1 - 1)]).abs() + (rc[o(w1 - 1)] - rc[o(w3 - 3)]).abs() + (g[indx] - g[o(w2 - 2)]).abs();
                let se_deg = EPS + (rc[o(-w1 - 1)] - rc[o(w1 + 1)]).abs() + (rc[o(w1 + 1)] - rc[o(w3 + 3)]).abs() + (g[indx] - g[o(w2 + 2)]).abs();
                let nw_est = rc[o(-w1 - 1)] - g[o(-w1 - 1)];
                let ne_est = rc[o(-w1 + 1)] - g[o(-w1 + 1)];
                let sw_est = rc[o(w1 - 1)] - g[o(w1 - 1)];
                let se_est = rc[o(w1 + 1)] - g[o(w1 + 1)];
                let p_est = (nw_deg * se_est + se_deg * nw_est) / (nw_deg + se_deg);
                let q_est = (ne_deg * sw_est + sw_deg * ne_est) / (ne_deg + sw_deg);
                let v = g[indx] + intp(disc, q_est, p_est);
                self.rgb[c][indx] = v;
                col += 2;
            }
        }
        // Step 4.3: R and B at G positions
        for row in 4..tile_rows - 4 {
            let mut col = 4 + (fc(row, 1) & 1);
            while col < tile_cols - 4 {
                let indx = row * TS + col;
                let o = |d: isize| (indx as isize + d) as usize;
                let cv = vh[indx];
                let nb = 0.25 * ((vh[indx - TS - 1] + vh[indx - TS + 1]) + (vh[indx + TS - 1] + vh[indx + TS + 1]));
                let disc = if (0.5 - cv).abs() < (0.5 - nb).abs() { nb } else { cv };
                let g = &self.rgb[1];
                let rgb1 = g[indx];
                let n1 = EPS + (rgb1 - g[o(-w2)]).abs();
                let s1 = EPS + (rgb1 - g[o(w2)]).abs();
                let w1_ = EPS + (rgb1 - g[o(-2)]).abs();
                let e1 = EPS + (rgb1 - g[o(2)]).abs();
                let (gmw1, gpw1, gm1, gp1) = (g[o(-w1)], g[o(w1)], g[o(-1)], g[o(1)]);
                let mut res = [0f32; 2];
                for (k, c) in [0usize, 2].into_iter().enumerate() {
                    let rc = &self.rgb[c];
                    let sn = (rc[o(-w1)] - rc[o(w1)]).abs();
                    let ew = (rc[o(-1)] - rc[o(1)]).abs();
                    let n_deg = n1 + sn + (rc[o(-w1)] - rc[o(-w3)]).abs();
                    let s_deg = s1 + sn + (rc[o(w1)] - rc[o(w3)]).abs();
                    let w_deg = w1_ + ew + (rc[o(-1)] - rc[o(-3)]).abs();
                    let e_deg = e1 + ew + (rc[o(1)] - rc[o(3)]).abs();
                    let n_est = rc[o(-w1)] - gmw1;
                    let s_est = rc[o(w1)] - gpw1;
                    let w_est = rc[o(-1)] - gm1;
                    let e_est = rc[o(1)] - gp1;
                    let v_est = (n_deg * s_est + s_deg * n_est) / (n_deg + s_deg);
                    let h_est = (e_deg * w_est + w_deg * e_est) / (e_deg + w_deg);
                    res[k] = rgb1 + intp(disc, h_est, v_est);
                }
                self.rgb[0][indx] = res[0];
                self.rgb[2][indx] = res[1];
                col += 2;
            }
        }

        // write the result into the band (omit the 9 px tile border)
        let first_v = row_start + TILE_BORDER;
        let last_v = row_end - TILE_BORDER;
        let first_h = col_start + TILE_BORDER;
        let last_h = col_end - TILE_BORDER;
        for row in first_v..last_v {
            let orow = &mut band[(row - band_a) * w..(row - band_a + 1) * w];
            for col in first_h..last_h {
                let idx = (row - row_start) * TS + col - col_start;
                orow[col] = [self.rgb[0][idx].max(0.0), self.rgb[1][idx].max(0.0), self.rgb[2][idx].max(0.0)];
            }
        }
    }
}
