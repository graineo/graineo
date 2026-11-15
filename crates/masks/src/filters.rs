use rayon::prelude::*;
/// Dense separable box filters; f64 running sums avoid cancellation on long HDR scanlines.
/// `radii`: see `correction::radii` (from the short edge of the whole image).
pub(crate) fn planes(data: &[f32], w: usize, h: usize, needed: [bool; 3], radii: [usize; 3]) -> [Vec<f32>; 3] {
    radii
    .into_iter()
    .enumerate()
    .map(|(i, r)| {
        if needed[i] {
            box_filter(data, w, h, r)
        } else {
            Vec::new()
        }
    })
    .collect::<Vec<_>>()
    .try_into()
    .unwrap()
}
pub(crate) fn box_filter(data: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut horizontal = vec![0.0; data.len()];
    horizontal
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for c in 0..3 {
                let src = &data[y * w * 3..(y + 1) * w * 3];
                let mut sum = 0.0f64;
                for dx in -(r as isize)..=r as isize {
                    sum += src[(dx.clamp(0, w as isize - 1) as usize) * 3 + c] as f64;
                }
                for x in 0..w {
                    row[x * 3 + c] = (sum / (2 * r + 1) as f64) as f32;
                    sum += src[((x + r + 1).min(w - 1)) * 3 + c] as f64
                        - src[(x.saturating_sub(r)) * 3 + c] as f64;
                }
            }
        });
    // Columns are independent; collect to avoid unsafe interleaved writes.
    let columns: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let mut col = vec![0.0; h * 3];
            for c in 0..3 {
                let mut sum = 0.0f64;
                for dy in -(r as isize)..=r as isize {
                    sum +=
                        horizontal[(dy.clamp(0, h as isize - 1) as usize * w + x) * 3 + c] as f64;
                }
                for y in 0..h {
                    col[y * 3 + c] = (sum / (2 * r + 1) as f64) as f32;
                    sum += horizontal[((y + r + 1).min(h - 1) * w + x) * 3 + c] as f64
                        - horizontal[(y.saturating_sub(r) * w + x) * 3 + c] as f64;
                }
            }
            col
        })
        .collect();
    let mut result = vec![0.0; data.len()];
    result
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                row[x * 3..x * 3 + 3].copy_from_slice(&columns[x][y * 3..y * 3 + 3]);
            }
        });
    result
}
