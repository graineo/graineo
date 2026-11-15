//! False color suppression (X-Trans) on the GPU: the same 5×5 color median as [`crate::false_color`], which stays
//! the reference and the fallback (no GPU, setting "Grafikkarte verwenden" off, image too small or too large).

use super::*;

/// Suppress false colors in the finished 16-bit image `data` (w·h·3 values) in place. `None`: not computed (no GPU,
/// buffers do not fit, the GPU did not deliver) – the caller computes on the CPU; `data` is then unchanged.
pub fn suppress(data: &mut [u16], w: usize, h: usize, strength: f32) -> Option<()> {
    let g = gpu()?;
    let n = w * h;
    if data.len() != n * 3 || w < 16 || h < 16 || (n * 3 * 2) as u64 > g.max_buffer || (n * 4) as u64 > g.max_buffer || !pools::gpu_fits_memory(n) {
        return None;
    }
    let t0 = std::time::Instant::now();
    let packed = g.packed_u16_buffer(data);
    let (y, u, v) = (g.buffer(n), g.buffer(n), g.buffer(n));
    let mut l = Run::new(g, w, h);
    let e = l.empty.clone();
    crate::timings::lap("GPU Hochladen (Falschfarben)", t0.elapsed());
    l.started = std::time::Instant::now();
    l.dispatch("aus_fc_yuv", &[], &[&y, &u, &v, &e, &e, &packed]);
    l.dispatch("aus_fc_median", &[(2, strength.clamp(0.0, 1.0))], &[&y, &u, &v, &e, &e, &packed]);
    let out = l.finish_u16(&packed, n)?;
    data.copy_from_slice(&out);
    crate::timings::lap("GPU Falschfarben gesamt", t0.elapsed());
    Some(())
}
