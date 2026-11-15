//! Markesteijn demosaicing (3 passes, X-Trans) on the GPU – the strip-wise host code for `gpu_xtrans.wgsl`.
//! The CPU version (`raw::markesteijn`) remains the reference and fallback.

use super::*;

/// Margin around each strip (≥ 17, the total inset of the CPU version with 3 passes)
const PAD: usize = 24;

/// Queue the kernels of the Markesteijn demosaic into job `l`. `mos` is the mosaic (w·h values on the GPU).
/// Returns: buffer with RGB per pixel (w·h·3). `None`: pattern/buffers do not fit.
pub(super) fn kernels(g: &Gpu, l: &mut Run, mos: &wgpu::Buffer, w: usize, h: usize, xt: [[u8; 6]; 6]) -> Option<wgpu::Buffer> {
    if w < 64 || h < 64 || (w * h * 3 * 4) as u64 > g.max_buffer || !pools::gpu_fits_memory(w * h) {
        return None;
    }
    let anchor = raw::markesteijn::gpu_anchor(xt)?;
    let (hex, sgrow, sgcol) = raw::markesteijn::gpu_table(xt);
    let t0 = std::time::Instant::now();
    let wb = w + 2 * PAD;
    // Strip height: planes (8 × 3 floats per pixel) at most ≈ 256 MB (saves memory; more strips only cost
    // the margin of 2·PAD rows per strip)
    let max_bytes = g.max_buffer.min(256_000_000) as usize;
    let rows = (max_bytes / (96 * wb)).saturating_sub(2 * PAD);
    if rows < 48 {
        return None;
    }
    let band = rows.min(h).min(720);
    let hb_max = band + 2 * PAD;
    let n_max = wb * hb_max;
    let planes = g.buffer(8 * 3 * n_max);
    let bounds = g.buffer(2 * n_max);
    let derivative = g.buffer(8 * n_max);
    let homo = g.buffer(8 * n_max);
    let out_buf = g.image_buffer(None, w * h * 3);
    let log = std::env::var_os("GRAINEO_LOG").is_some();
    if log {
        eprintln!("gpu xtrans: prepared {:.0?}, strip {band} rows", t0.elapsed());
    }
    let mut r0 = 0;
    while r0 < h {
        let r1 = (r0 + band).min(h);
        let hb = (r1 - r0) + 2 * PAD;
        let n = wb * hb;
        // base parameters of the strip
        let mut p = vec![0f32; 288];
        p[0] = wb as f32;
        p[1] = hb as f32;
        p[2] = w as f32;
        p[3] = h as f32;
        p[4] = r0 as f32 - PAD as f32;
        p[5] = PAD as f32;
        p[6] = sgrow as f32;
        p[7] = sgcol as f32;
        p[8] = n as f32;
        p[10] = r0 as f32;
        p[11] = r1 as f32;
        for r in 0..6 {
            for c in 0..6 {
                p[12 + r * 6 + c] = xt[r][c] as f32;
                p[192 + 2 * (r * 6 + c)] = anchor[r][c][0] as f32;
                p[193 + 2 * (r * 6 + c)] = anchor[r][c][1] as f32;
            }
        }
        for (k, v) in hex.iter().enumerate() {
            p[48 + 2 * k] = v[0] as f32;
            p[49 + 2 * k] = v[1] as f32;
        }
        // grid of the isolated green pixels (every 3 pixels in both directions) for the R/B kernel at the green pixel
        let phase = |n: usize, start: i64, want: i32| -> usize { (5..8usize).find(|&v| (start + v as i64 - want as i64).rem_euclid(3) == 0).unwrap_or(5).max(5) + 0 * n };
        let x0 = phase(0, -(PAD as i64), sgcol);
        let y0 = phase(0, r0 as i64 - PAD as i64, sgrow);
        p[264] = x0 as f32;
        p[265] = y0 as f32;
        let dim_rb1 = ((wb.saturating_sub(10)) / 3 + 1, (hb.saturating_sub(10)) / 3 + 1, 1);
        let dim = (wb, hb, 1);
        let dim8 = (wb, hb, 8);
        let with_offset = |roff: usize| -> Vec<f32> {
            let mut q = p.clone();
            q[9] = roff as f32;
            q
        };
        // fill planes 0–3 with 0, then the kernel only writes the mosaic value at its own color
        l.clear(&planes, (12 * n * 4) as u64);
        l.dispatch_p("xt_fuellen", &p, dim, &[&planes, mos]);
        l.dispatch_p("xt_gminmax", &p, dim, &[&planes, mos, &bounds]);
        l.dispatch_p("xt_gruen", &p, dim, &[&planes, mos, &bounds]);
        for cpass in 0..3 {
            let roff = if cpass == 0 { 0 } else { 4 };
            let q = with_offset(roff);
            if cpass == 1 {
                // second set = copy of the first (planes 0–3 → 4–7, 12·N values each)
                l.dispatch_p("xt_kopie", &q, dim, &[&planes]);
            }
            if cpass > 0 {
                l.dispatch_p("xt_gruen2_a", &q, dim, &[&planes, mos, &bounds, &derivative]);
                l.dispatch_p("xt_gruen2_b", &q, dim, &[&planes, mos, &bounds, &derivative]);
            }
            l.dispatch_p("xt_rb1", &q, dim_rb1, &[&planes]);
            l.dispatch_p("xt_rb2", &q, dim, &[&planes]);
            l.dispatch_p("xt_g22", &q, dim, &[&planes]);
        }
        l.dispatch_p("xt_drv", &p, dim8, &[&planes, mos, &bounds, &derivative]);
        l.dispatch_p("xt_homo", &p, dim, &[&planes, mos, &bounds, &derivative, &homo]);
        l.dispatch_p("xt_h5", &p, dim8, &[&planes, mos, &bounds, &derivative, &homo]);
        l.dispatch_p("xt_v5", &p, dim8, &[&planes, mos, &bounds, &derivative, &homo]);
        l.dispatch_p("xt_fertig", &p, dim, &[&planes, mos, &bounds, &derivative, &homo, &out_buf]);
        r0 = r1;
    }
    if log {
        eprintln!("gpu xtrans: built {:.0?} ({} kernels)", t0.elapsed(), l.params.len());
    }
    Some(out_buf)
}

/// Mosaic (normalized, white balanced, highlights reconstructed) → RGB per pixel. `None`: no GPU, buffer too
/// small or pattern not matching – then the CPU computes it.
pub fn markesteijn(mosaic: &[f32], w: usize, h: usize, xt: [[u8; 6]; 6]) -> Option<Vec<[f32; 3]>> {
    let g = gpu()?;
    if mosaic.len() != w * h {
        return None;
    }
    let t0 = std::time::Instant::now();
    let mos = g.image_buffer(Some(mosaic), w * h);
    let mut l = Run::new(g, w, h);
    let out_buf = kernels(g, &mut l, &mos, w, h, xt)?;
    let px = l.finish_rgb(&out_buf)?;
    crate::timings::lap("GPU Demosaik", t0.elapsed());
    Some(px)
}
