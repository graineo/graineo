//! Merrill: the own highlight rebuild ([`crate::highlight_rebuild`]) in the float frontend of the exact pipeline.
//!
//! The frontend hands over its buffer after cleaning the camera's clip map ([`x3f_core::HighlightInput`]). When the
//! rebuild is on, it replaces the frontend's own filling and desaturation:
//! 1. The clip level of each layer is read from the clip map (low percentile of the marked values).
//! 2. The top layer (clips first) is rebuilt from the bottom and middle layer, then the middle layer from the bottom
//!    and the rebuilt top layer (gradient domain, global fit, see [`crate::highlight_rebuild`]).
//!    The rebuilt values stay above the clip level (the frontend's later stages take them). The image-wide fit of
//!    the top layer from the lower ones is a floor of the rebuilt top layer (see the comment there).
//! 3. Neutral where the scene is bright in every layer: weight = smoothstep of the smallest (white-balanced) layer
//!    over [`NEUTRAL_WIDTH`] counts above the frontend's threshold, so a white surface turns white while a bright
//!    coloured one (a red petal: its smallest layer stays low) keeps its colour; and where the bottom layer is
//!    clipped as well (nothing measurable), by the share of such pixels in a 9 × 9 window. The pixel glides towards
//!    its bottom layer (neutral at its level).
//!
//! Without the rebuild the frontend's own steps run unchanged (the hook returns `false`).

use rayon::prelude::*;

/// Registers the hook in the frontend (once; the hook itself checks whether the rebuild is on).
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| x3f_core::set_highlight_hook(Some(hook)));
}

/// Percentile of the marked values taken as the clip level of a layer.
const LEVEL_PERCENTILE: f64 = 0.02;
/// Width of the neutral ramp of step 3 (counts above the threshold).
const NEUTRAL_WIDTH: f64 = 1000.0;
/// Radius of the window of step 3 (pixels).
const NEUTRAL_RADIUS: usize = 4;

fn hook(input: &mut x3f_core::HighlightInput<'_>) -> bool {
    if !crate::highlight_rebuild::enabled_merrill() {
        return false;
    }
    let t0 = std::time::Instant::now();
    let [left, top, right, bottom] = input.valid;
    let (w, h) = (right + 1 - left, bottom + 1 - top);
    if w < 4 || h < 4 {
        return false;
    }
    let (stride, width) = (input.row_stride, input.width);
    let at = |x: usize, y: usize| (y + top) * stride + 3 * (x + left);
    let bits = |x: usize, y: usize| input.clipped[(y + top) * width + x + left];
    // 1. planar layers of the valid rectangle and the clip level per layer
    let mut layers: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; w * h]);
    for y in 0..h {
        for x in 0..w {
            let p = at(x, y);
            for c in 0..3 {
                layers[c][y * w + x] = input.data[p + c];
            }
        }
    }
    let mut level = [0f32; 3];
    for c in 0..3 {
        let mut v: Vec<f32> = (0..w * h).filter(|&i| bits(i % w, i / w) & (1 << c) != 0).map(|i| layers[c][i]).collect();
        level[c] = if v.len() < 16 {
            // never clipped: above every value
            layers[c].par_iter().copied().reduce(|| 0.0, f32::max) * 2.0 + 1.0
        } else {
            let k = ((v.len() as f64 * LEVEL_PERCENTILE) as usize).min(v.len() - 1);
            v.select_nth_unstable_by(k, f32::total_cmp);
            v[k]
        };
    }
    let sat = level.map(|l| l / crate::highlight_rebuild::CLIP_SHARE);
    // 2. top, then middle layer
    let variant = crate::highlight_rebuild::variant();
    let top_out = crate::highlight_rebuild::rebuild_layer_with(&mut layers, w, h, sat, 2, variant, false);
    // the global fit as a floor of the rebuilt top layer: the gradient-domain fill takes its level from the border of
    // a blown area, which is often another surface (a white bird against a darker fence, a blown sky above a wall);
    // its level stays near the clip there and the area turned yellow. The fit of the whole image never lies below.
    if let Some(o) = top_out.as_ref() {
        let k = o.stats.weights;
        for &(i, wt) in &o.weight {
            let i = i as usize;
            let g = k[0] * layers[0][i] + k[1] * layers[1][i];
            layers[2][i] = layers[2][i].max(layers[2][i] + wt * (g - layers[2][i]));
        }
    }
    let mid_out = crate::highlight_rebuild::rebuild_layer_with(&mut layers, w, h, sat, 1, variant, false);
    // 3. neutral where the bottom layer is clipped
    let bottom_clipped: Vec<f32> = (0..w * h).map(|i| f32::from(u8::from(bits(i % w, i / w) & 1 != 0))).collect();
    let share = box_share(&bottom_clipped, w, h, NEUTRAL_RADIUS);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let p = at(x, y);
            let lo = layers[0][i].min(layers[1][i]).min(layers[2][i]) as f64;
            let r = ((lo - input.threshold) / NEUTRAL_WIDTH).clamp(0.0, 1.0);
            let t = share[i].max((r * r * (3.0 - 2.0 * r)) as f32);
            for c in 0..3 {
                let v = layers[c][i];
                input.data[p + c] = v + t * (layers[0][i] - v);
            }
        }
    }
    if std::env::var_os("X3F_HL_DEBUG").is_some() {
        eprintln!(
            "Merrill highlight rebuild: levels {level:?}, top {:?}, middle {:?}, {:.2?}",
            top_out.as_ref().map(|o| &o.stats),
            mid_out.as_ref().map(|o| &o.stats),
            t0.elapsed()
        );
    }
    true
}

/// Mean of `v` over a `(2r + 1)²` window (edges clamped), by summed-area table.
fn box_share(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut sum = vec![0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += v[y * w + x] as f64;
            sum[(y + 1) * (w + 1) + x + 1] = sum[y * (w + 1) + x + 1] + row;
        }
    }
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (x0, y0, x1, y1) = (x.saturating_sub(r), y.saturating_sub(r), (x + r + 1).min(w), (y + r + 1).min(h));
            let s = sum[y1 * (w + 1) + x1] - sum[y0 * (w + 1) + x1] - sum[y1 * (w + 1) + x0] + sum[y0 * (w + 1) + x0];
            (s / ((x1 - x0) * (y1 - y0)) as f64) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn box_share_of_constant_is_constant() {
        let v = vec![1.0f32; 30];
        assert!(super::box_share(&v, 6, 5, 2).iter().all(|s| (s - 1.0).abs() < 1e-6));
        let mut v = vec![0.0f32; 25];
        v[12] = 1.0;
        let s = super::box_share(&v, 5, 5, 1);
        assert!((s[12] - 1.0 / 9.0).abs() < 1e-6 && s[0] == 0.0);
    }
}
