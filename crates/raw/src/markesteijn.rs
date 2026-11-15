//! Markesteijn demosaicing (1- and 3-pass) for Fujifilm X-Trans sensors.
//!
//! Origin: Rust port of `xtrans_markesteijn_interpolate()` from **darktable**
//! `src/iop/demosaicing/xtrans.c` (master, retrieved 2026-10-27), license **GPL-3.0-or-later**,
//! Copyright (C) 2010-2026 darktable developers. darktable took the method by Frank Markesteijn
//! from dcraw 9.20 and extended it with mirrored borders and YPbPr instead of CIELab.
//!
//! Changes from the original: Rust instead of C, tile rows via rayon instead of OpenMP,
//! tile size fixed at 122 (as in darktable), negative remainders via `rem_euclid` (darktable uses
//! C `%`, which yields negative remainders in the mirrored border region – only affects pixels outside the
//! image). The final `_vng_lininterpolate` border handling is dropped because the mirrored
//! tiles cover the whole image.

use rayon::prelude::*;

const TS: usize = 122;
const TSI: isize = TS as isize;
const PAD_G1_G3: isize = 3;
const PAD_G_INTERP: isize = 3;
const PAD_G_RECALC: isize = 6;
const HOMOGENITY_EPS: f32 = 1e-10;
const SOLITARY_EPS: f32 = 1e-12;

#[inline(always)]
fn sqrf(x: f32) -> f32 {
    x * x
}

/// Hexagon tables of the pattern (8 offsets per 3×3 phase, in tile indices `h + v·TS`), as well as the
/// position of the isolated green pixels (`sgrow`, `sgcol`). Shared by CPU and GPU ([`gpu_table`]).
fn tables(xt: [[u8; 6]; 6], passes: usize) -> ([[[isize; 8]; 3]; 3], isize, isize) {
    let fc = |row: isize, col: isize| -> usize { xt[(row + 600).rem_euclid(6) as usize][(col + 600).rem_euclid(6) as usize] as usize };
    const ORTH: [i16; 12] = [1, 0, 0, 1, -1, 0, 0, -1, 1, 0, 0, 1];
    const PATT: [[i16; 16]; 2] = [[0, 1, 0, -1, 2, 0, -1, 0, 1, 1, 1, -1, 0, 0, 0, 0], [0, 1, 0, -2, 1, 0, -2, 0, 1, 1, -2, -2, 1, -1, -1, 1]];
    let _ = passes;
    let mut allhex = [[[0isize; 8]; 3]; 3];
    let (mut sgrow, mut sgcol) = (0isize, 0isize);
    for row in 0..3isize {
        for col in 0..3isize {
            let mut ng = 0;
            let mut d = 0usize;
            while d < 10 {
                let g = (fc(row, col) == 1) as usize;
                if fc(row + ORTH[d] as isize, col + ORTH[d + 2] as isize) == 1 {
                    ng = 0;
                } else {
                    ng += 1;
                }
                if ng == 4 {
                    sgrow = row;
                    sgcol = col;
                }
                if ng == g + 1 {
                    for c in 0..8usize {
                        let v = ORTH[d] as isize * PATT[g][c * 2] as isize + ORTH[d + 1] as isize * PATT[g][c * 2 + 1] as isize;
                        let h = ORTH[d + 2] as isize * PATT[g][c * 2] as isize + ORTH[d + 3] as isize * PATT[g][c * 2 + 1] as isize;
                        allhex[row as usize][col as usize][c ^ (g * 2 & d)] = h + v * TSI;
                    }
                }
                d += 2;
            }
        }
    }
    (allhex, sgrow, sgcol)
}

/// Tables for the GPU path: for each 3×3 phase (row mod 3, column mod 3) 8 offsets (dx, dy) in
/// image pixels, plus `sgrow` and `sgcol`.
pub fn gpu_table(xt: [[u8; 6]; 6]) -> (Vec<[i32; 2]>, i32, i32) {
    let (hex, sgrow, sgcol) = tables(xt, 3);
    let mut v = Vec::with_capacity(72);
    for r in 0..3 {
        for c in 0..3 {
            for k in 0..8 {
                let o = hex[r][c][k];
                // o = h + v·TS with |h| < TS/2
                let dy = (o as f32 / TS as f32).round() as isize;
                let dx = o - dy * TSI;
                v.push([dx as i32, dy as i32]);
            }
        }
    }
    (v, sgrow as i32, sgcol as i32)
}

/// `xt[y][x]` is the color (0/1/2) of the 6×6 pattern relative to the image origin.
/// `in_` normalized mosaic values (w×h). Result RGB per pixel.
pub(crate) fn markesteijn(in_: &[f32], width: usize, height: usize, xt: [[u8; 6]; 6], passes: usize) -> Vec<[f32; 3]> {
    let fc = |row: isize, col: isize| -> usize { xt[(row + 600).rem_euclid(6) as usize][(col + 600).rem_euclid(6) as usize] as usize };
    let dir: [isize; 4] = [1, TSI, TSI + 1, TSI - 1];
    let ndir: usize = 4 << (passes > 1) as usize;
    let (allhex, sgrow, sgcol) = tables(xt, passes);
    let hexmap = |row: isize, col: isize| -> &[isize; 8] { &allhex[((row + 600) % 3) as usize][((col + 600) % 3) as usize] };

    let pad_tile: isize = if passes == 1 { 12 } else { 17 };
    let step = TSI - pad_tile * 2;
    let (w, h) = (width as isize, height as isize);
    let tops: Vec<isize> = {
        let mut v = Vec::new();
        let mut t = -pad_tile;
        while t < h - pad_tile {
            v.push(t);
            t += step;
        }
        v
    };
    let mut out = vec![[0f32; 3]; width * height];
    // each tile row writes the image rows [top+pad, min(top+TS, h+pad)-pad) → disjoint
    let mut slices: Vec<(isize, &mut [[f32; 3]])> = Vec::new();
    {
        let mut rest: &mut [[f32; 3]] = &mut out[..];
        let mut pos = 0isize;
        for &top in &tops {
            let a = (top + pad_tile).max(0);
            let b = ((top + TSI).min(h + pad_tile) - pad_tile).min(h);
            if a >= b {
                continue;
            }
            let (_, r) = rest.split_at_mut(((a - pos) * w) as usize);
            let (band, r2) = r.split_at_mut(((b - a) * w) as usize);
            slices.push((top, band));
            rest = r2;
            pos = b;
        }
    }

    slices.into_par_iter().for_each(|(top, band)| {
        let tt = TS * TS;
        let mut rgb_all = vec![[0f32; 3]; tt * ndir];
        let mut yuv = vec![0f32; tt * 3];
        let mut drv = vec![0f32; tt * ndir];
        let mut gmin = vec![0f32; tt];
        let mut gmax = vec![0f32; tt];
        let mut homo = vec![0u8; tt * ndir];
        let mut homosum = vec![0u8; tt * ndir];
        let band_a = (top + pad_tile).max(0);

        let mut left = -pad_tile;
        while left < w - pad_tile {
            let mrow = (top + TSI).min(h + pad_tile);
            let mcol = (left + TSI).min(w + pad_tile);
            let ti = |row: isize, col: isize| -> usize { ((row - top) * TSI + (col - left)) as usize };

            // fill the tile (mirrored at the image edge)
            let translate = |n: isize, size: isize| if n >= size { 2 * size - n - 2 } else { n.abs() };
            for row in top..mrow {
                for col in left..mcol {
                    let f = fc(row, col);
                    let mut pix = [0f32; 3];
                    if col >= 0 && row >= 0 && col < w && row < h {
                        pix[f] = in_[(row * w + col) as usize].max(0.0);
                    } else {
                        let cy = translate(row, h);
                        let cx = translate(col, w);
                        if f == fc(cy, cx) {
                            pix[f] = in_[(cy * w + cx) as usize].max(0.0);
                        } else {
                            let (mut sum, mut count) = (0f32, 0u32);
                            for y in row - 1..=row + 1 {
                                for x in col - 1..=col + 1 {
                                    let yy = translate(y, h);
                                    let xx = translate(x, w);
                                    if fc(yy, xx) == f {
                                        sum += in_[(yy * w + xx) as usize].max(0.0);
                                        count += 1;
                                    }
                                }
                            }
                            pix[f] = if count > 0 { sum / count as f32 } else { 0.0 };
                        }
                    }
                    rgb_all[ti(row, col)] = pix;
                }
            }
            // copy rgb[0] to rgb[1..3]
            let (first, restb) = rgb_all.split_at_mut(tt);
            for c in 0..3 {
                restb[c * tt..(c + 1) * tt].copy_from_slice(first);
            }

            // green minimum/maximum per R/B pair
            let mut row = top + PAD_G1_G3;
            while row < mrow - PAD_G1_G3 {
                let (mut min, mut max) = (f32::MAX, 0f32);
                let mut col = left + PAD_G1_G3;
                while col < mcol - PAD_G1_G3 {
                    if fc(row, col) == 1 {
                        min = f32::MAX;
                        max = 0.0;
                        col += 1;
                        continue;
                    }
                    if max == 0.0 {
                        let base = ti(row, col) as isize;
                        let hex = hexmap(row, col);
                        for &hx in hex.iter().take(6) {
                            let val = rgb_all[(base + hx) as usize][1];
                            if min > val {
                                min = val;
                            }
                            if max < val {
                                max = val;
                            }
                        }
                    }
                    gmin[ti(row, col)] = min;
                    gmax[ti(row, col)] = max;
                    match (row - sgrow).rem_euclid(3) {
                        1 => {
                            if row < mrow - 4 {
                                row += 1;
                                col -= 1;
                            }
                        }
                        2 => {
                            min = f32::MAX;
                            max = 0.0;
                            col += 2;
                            if col < mcol - 4 && row > top + 3 {
                                row -= 1;
                            }
                        }
                        _ => {}
                    }
                    col += 1;
                }
                row += 1;
            }

            // interpolate green horizontally, vertically and diagonally
            for row in top + PAD_G_INTERP..mrow - PAD_G_INTERP {
                for col in left + PAD_G_INTERP..mcol - PAD_G_INTERP {
                    let f = fc(row, col);
                    if f == 1 {
                        continue;
                    }
                    let base = ti(row, col) as isize;
                    let p = |o: isize, c: usize| rgb_all[(base + o) as usize][c];
                    let hex = hexmap(row, col);
                    let mut color = [0f32; 4];
                    color[0] = 0.6796875 * (p(hex[1], 1) + p(hex[0], 1)) - 0.1796875 * (p(2 * hex[1], 1) + p(2 * hex[0], 1));
                    color[1] = 0.87109375 * p(hex[3], 1) + p(hex[2], 1) * 0.13 + 0.359375 * (p(0, f) - p(-hex[2], f));
                    for c in 0..2 {
                        color[2 + c] = 0.640625 * p(hex[4 + c], 1) + 0.359375 * p(-2 * hex[4 + c], 1) + 0.12890625 * (2.0 * p(0, f) - p(3 * hex[4 + c], f) - p(-3 * hex[4 + c], f));
                    }
                    let sw = ((row - sgrow).rem_euclid(3) == 0) as usize;
                    let i = ti(row, col);
                    let (lo, hi) = (gmin[i], gmax[i]);
                    for (c, &v) in color.iter().enumerate() {
                        rgb_all[(c ^ sw) * tt + i][1] = v.max(lo).min(hi);
                    }
                }
            }

            let mut roff = 0usize; // offset of the current rgb group (0 or 4 tiles)
            for pass in 0..passes {
                if pass == 1 {
                    let (a, b) = rgb_all.split_at_mut(4 * tt);
                    b[..4 * tt].copy_from_slice(a);
                    roff = 4 * tt;
                }
                if pass > 0 {
                    for row in top + PAD_G_RECALC..mrow - PAD_G_RECALC {
                        for col in left + PAD_G_RECALC..mcol - PAD_G_RECALC {
                            let f = fc(row, col);
                            if f == 1 {
                                continue;
                            }
                            let hex = hexmap(row, col);
                            let sw = ((row - sgrow).rem_euclid(3) == 0) as usize;
                            let i = ti(row, col);
                            for d in 3..6usize {
                                let base = (roff + ((d - 2) ^ sw) * tt + i) as isize;
                                let q = |o: isize, c: usize| rgb_all[(base + o) as usize][c];
                                let val = q(-2 * hex[d], 1) + 2.0 * q(hex[d], 1) - q(-2 * hex[d], f) - 2.0 * q(hex[d], f) + 3.0 * q(0, f);
                                rgb_all[base as usize][1] = (val / 3.0).max(gmin[i]).min(gmax[i]);
                            }
                        }
                    }
                }

                // R and B for isolated green pixels
                let pad_rb_g: isize = if passes == 1 { 6 } else { 5 };
                let mut row = (top - sgrow + pad_rb_g + 2) / 3 * 3 + sgrow;
                while row < mrow - pad_rb_g {
                    let mut col = (left - sgcol + pad_rb_g + 2) / 3 * 3 + sgcol;
                    while col < mcol - pad_rb_g {
                        let mut rbase = (roff + ti(row, col)) as isize;
                        let mut hh = fc(row, col + 1);
                        let mut diff = [0f32; 6];
                        let mut color = [[0f32; 6]; 2];
                        let mut i: isize = 1;
                        for d in 0..6usize {
                            for c in 0..2u32 {
                                let r = |o: isize, ch: usize| rgb_all[(rbase + o) as usize][ch];
                                let ic = i << c;
                                let g = 2.0 * r(0, 1) - r(ic, 1) - r(-ic, 1);
                                color[(hh != 0) as usize][d] = g + r(ic, hh) + r(-ic, hh);
                                if d > 1 {
                                    diff[d] += sqrf(r(ic, 1) - r(-ic, 1) - r(ic, hh) + r(-ic, hh)) + sqrf(g);
                                }
                                hh ^= 2;
                            }
                            if d < 2 || (d & 1) == 1 {
                                let d_out = d - ((d > 1) && (diff[d - 1] + SOLITARY_EPS < diff[d])) as usize;
                                let px = &mut rgb_all[rbase as usize];
                                px[0] = color[0][d_out] / 2.0;
                                px[2] = color[1][d_out] / 2.0;
                                rbase += tt as isize;
                            }
                            i ^= TSI ^ 1;
                            hh ^= 2;
                        }
                        col += 3;
                    }
                    row += 3;
                }

                // R at B and vice versa
                let pad_rb_br: isize = if passes == 1 { 6 } else { 5 };
                for row in top + pad_rb_br..mrow - pad_rb_br {
                    for col in left + pad_rb_br..mcol - pad_rb_br {
                        let f = 2 - fc(row, col) as isize;
                        if f == 1 {
                            continue;
                        }
                        let f = f as usize;
                        let mut rbase = (roff + ti(row, col)) as isize;
                        let c: isize = if (row - sgrow).rem_euclid(3) != 0 { TSI } else { 1 };
                        let hh: isize = 3 * (c ^ TSI ^ 1);
                        for d in 0..4isize {
                            let r = |o: isize, ch: usize| rgb_all[(rbase + o) as usize][ch];
                            let use_c = d > 1
                                || ((d ^ c) & 1) != 0
                                || ((r(0, 1) - r(c, 1)).abs() + (r(0, 1) - r(-c, 1)).abs()) < 2.0 * ((r(0, 1) - r(hh, 1)).abs() + (r(0, 1) - r(-hh, 1)).abs());
                            let i = if use_c { c } else { hh };
                            let v = (r(i, f) + r(-i, f) + 2.0 * r(0, 1) - r(i, 1) - r(-i, 1)) / 2.0;
                            rgb_all[rbase as usize][f] = v;
                            rbase += tt as isize;
                        }
                    }
                }

                // R and B in 2×2 green blocks
                let pad_g22: isize = if passes == 1 { 8 } else { 4 };
                for row in top + pad_g22..mrow - pad_g22 {
                    if (row - sgrow).rem_euclid(3) == 0 {
                        continue;
                    }
                    for col in left + pad_g22..mcol - pad_g22 {
                        if (col - sgcol).rem_euclid(3) == 0 {
                            continue;
                        }
                        let hex = hexmap(row, col);
                        let mut rbase = (roff + ti(row, col)) as isize;
                        let mut d = 0usize;
                        while d < ndir {
                            let r = |o: isize, ch: usize| rgb_all[(rbase + o) as usize][ch];
                            if hex[d] + hex[d + 1] != 0 {
                                let g = 3.0 * r(0, 1) - 2.0 * r(hex[d], 1) - r(hex[d + 1], 1);
                                let v0 = (g + 2.0 * r(hex[d], 0) + r(hex[d + 1], 0)) / 3.0;
                                let v2 = (g + 2.0 * r(hex[d], 2) + r(hex[d + 1], 2)) / 3.0;
                                let px = &mut rgb_all[rbase as usize];
                                px[0] = v0;
                                px[2] = v2;
                            } else {
                                let g = 2.0 * r(0, 1) - r(hex[d], 1) - r(hex[d + 1], 1);
                                let v0 = (g + r(hex[d], 0) + r(hex[d + 1], 0)) / 2.0;
                                let v2 = (g + r(hex[d], 2) + r(hex[d + 1], 2)) / 2.0;
                                let px = &mut rgb_all[rbase as usize];
                                px[0] = v0;
                                px[2] = v2;
                            }
                            d += 2;
                            rbase += tt as isize;
                        }
                    }
                }
            }

            // tile coordinates from here on
            let mrow_t = mrow - top;
            let mcol_t = mcol - left;

            // YPbPr and derivatives per direction
            for d in 0..ndir {
                let pad_yuv: isize = if passes == 1 { 8 } else { 13 };
                for row in pad_yuv..mrow_t - pad_yuv {
                    for col in pad_yuv..mcol_t - pad_yuv {
                        let i = (row * TSI + col) as usize;
                        let rx = rgb_all[d * tt + i];
                        let y = 0.2627 * rx[0] + 0.6780 * rx[1] + 0.0593 * rx[2];
                        yuv[i] = y;
                        yuv[tt + i] = (rx[2] - y) * 0.56433;
                        yuv[2 * tt + i] = (rx[0] - y) * 0.67815;
                    }
                }
                let f = dir[d & 3];
                let pad_drv: isize = if passes == 1 { 9 } else { 14 };
                for row in pad_drv..mrow_t - pad_drv {
                    for col in pad_drv..mcol_t - pad_drv {
                        let i = row * TSI + col;
                        let y = |k: usize, o: isize| yuv[k * tt + (i + o) as usize];
                        drv[d * tt + i as usize] = sqrf(2.0 * y(0, 0) - y(0, f) - y(0, -f)) + sqrf(2.0 * y(1, 0) - y(1, f) - y(1, -f)) + sqrf(2.0 * y(2, 0) - y(2, f) - y(2, -f));
                    }
                }
            }

            // homogeneity maps
            homo.iter_mut().for_each(|v| *v = 0);
            let pad_homo: isize = if passes == 1 { 10 } else { 15 };
            for row in pad_homo..mrow_t - pad_homo {
                for col in pad_homo..mcol_t - pad_homo {
                    let i = (row * TSI + col) as usize;
                    let mut tr = f32::MAX;
                    for d in 0..ndir {
                        if tr > drv[d * tt + i] {
                            tr = drv[d * tt + i];
                        }
                    }
                    tr = (8.0 * tr).max(HOMOGENITY_EPS);
                    for d in 0..ndir {
                        let mut s = 0u8;
                        for v in -1..=1isize {
                            for hh in -1..=1isize {
                                if drv[d * tt + (i as isize + v * TSI + hh) as usize] <= tr {
                                    s += 1;
                                }
                            }
                        }
                        homo[d * tt + i] = s;
                    }
                }
            }

            // 5×5 sums
            for d in 0..ndir {
                for row in pad_tile..mrow_t - pad_tile {
                    let mut col = pad_tile - 5;
                    let mut v5sum = [0u8; 5];
                    homosum[d * tt + (row * TSI + col) as usize] = 0;
                    col += 1;
                    while col < mcol_t - pad_tile {
                        let mut colsum = 0u8;
                        for v in -2..=2isize {
                            colsum = colsum.wrapping_add(homo[d * tt + ((row + v) * TSI + col + 2) as usize]);
                        }
                        let i = (row * TSI + col) as usize;
                        homosum[d * tt + i] = homosum[d * tt + i - 1].wrapping_sub(v5sum[(col % 5) as usize]).wrapping_add(colsum);
                        v5sum[(col % 5) as usize] = colsum;
                        col += 1;
                    }
                }
            }

            // average the most homogeneous directions → output
            for row in pad_tile..mrow_t - pad_tile {
                let irow = row + top;
                if irow < band_a || irow >= h {
                    continue;
                }
                let orow = &mut band[((irow - band_a) * w) as usize..((irow - band_a + 1) * w) as usize];
                for col in pad_tile..mcol_t - pad_tile {
                    let icol = col + left;
                    if icol < 0 || icol >= w {
                        continue;
                    }
                    let i = (row * TSI + col) as usize;
                    let mut hm = [0u8; 8];
                    let mut maxval = 0u8;
                    for d in 0..ndir {
                        hm[d] = homosum[d * tt + i];
                        maxval = maxval.max(hm[d]);
                    }
                    maxval -= maxval >> 3;
                    for d in 0..ndir - 4 {
                        if hm[d] < hm[d + 4] {
                            hm[d] = 0;
                        } else if hm[d] > hm[d + 4] {
                            hm[d + 4] = 0;
                        }
                    }
                    let mut avg = [0f32; 4];
                    for d in 0..ndir {
                        if hm[d] >= maxval {
                            let p = rgb_all[d * tt + i];
                            avg[0] += p[0];
                            avg[1] += p[1];
                            avg[2] += p[2];
                            avg[3] += 1.0;
                        }
                    }
                    orow[icol as usize] = [(avg[0] / avg[3]).max(0.0), (avg[1] / avg[3]).max(0.0), (avg[2] / avg[3]).max(0.0)];
                }
            }
            left += step;
        }
    });
    out
}

/// Anchors of the green minimum/maximum loop for the GPU path: the CPU shares values of neighboring R/B pixels
/// (only the first pixel of a group computes). For each pattern phase (row mod 6, column mod 6) this holds the
/// offset (row, column) of the pixel whose hexagon applies. The loop is replayed with anchor points
/// (for several tile phases); `None` if the anchors depend on the tile phase or pixels remain unvisited.
pub fn gpu_anchor(xt: [[u8; 6]; 6]) -> Option<[[[i32; 2]; 6]; 6]> {
    let fc = |row: isize, col: isize| -> usize { xt[(row + 600).rem_euclid(6) as usize][(col + 600).rem_euclid(6) as usize] as usize };
    let (_, sgrow, _) = tables(xt, 3);
    let mut per_phase: [[Option<(isize, isize)>; 6]; 6] = [[None; 6]; 6];
    for (top, left) in [(-17isize, -17isize), (71, -17), (-17, 71), (71, 71), (159, 159), (-17, 159), (5, 4), (3, 2)] {
        let mrow = top + TSI;
        let mcol = left + TSI;
        let mut anchor: std::collections::HashMap<(isize, isize), (isize, isize)> = Default::default();
        let mut row = top + PAD_G1_G3;
        while row < mrow - PAD_G1_G3 {
            let mut set_count = false;
            let mut cur = (0isize, 0isize);
            let mut col = left + PAD_G1_G3;
            while col < mcol - PAD_G1_G3 {
                if fc(row, col) == 1 {
                    set_count = false;
                    col += 1;
                    continue;
                }
                if !set_count {
                    cur = (row, col);
                    set_count = true;
                }
                anchor.insert((row, col), cur);
                match (row - sgrow).rem_euclid(3) {
                    1 => {
                        if row < mrow - 4 {
                            row += 1;
                            col -= 1;
                        }
                    }
                    2 => {
                        set_count = false;
                        col += 2;
                        if col < mcol - 4 && row > top + 3 {
                            row -= 1;
                        }
                    }
                    _ => {}
                }
                col += 1;
            }
            row += 1;
        }
        for row in top + 17..mrow - 17 {
            for col in left + 17..mcol - 17 {
                if fc(row, col) == 1 {
                    continue;
                }
                let a = anchor.get(&(row, col))?;
                let d = (a.0 - row, a.1 - col);
                let slot = &mut per_phase[row.rem_euclid(6) as usize][col.rem_euclid(6) as usize];
                match slot {
                    None => *slot = Some(d),
                    Some(old) if *old != d => return None,
                    _ => {}
                }
            }
        }
    }
    let mut out = [[[0i32; 2]; 6]; 6];
    for r in 0..6 {
        for c in 0..6 {
            if let Some((dr, dc)) = per_phase[r][c] {
                out[r][c] = [dr as i32, dc as i32];
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The green minimum/maximum loop shares values of neighboring R/B pixels (only the first pixel of a
    /// group computes). For the GPU path we need, per R/B pixel, WHOSE hexagon applies: here the
    /// loop is replayed with anchor points – for several tile phases – and the anchors per pattern phase
    /// (row mod 6, column mod 6) are printed and checked for uniqueness.
    #[test]
    fn anchors_of_green_bounds() {
        let xt: [[u8; 6]; 6] = [[1, 2, 1, 1, 0, 1], [0, 1, 0, 2, 1, 2], [1, 2, 1, 1, 0, 1], [1, 0, 1, 1, 2, 1], [2, 1, 2, 0, 1, 0], [1, 0, 1, 1, 2, 1]];
        let a = gpu_anchor(xt).expect("anchors unique, all R/B pixels visited");
        // anchors are at most one pixel away
        assert!(a.iter().flatten().all(|d| d[0].abs() <= 1 && d[1].abs() <= 1));
        assert_eq!(a[0][1], [-1, 0]);
        assert_eq!(a[1][0], [0, -1]);
    }
}
