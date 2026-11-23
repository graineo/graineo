//! Binned output of the high-ISO chain (sd Quattro, sd Quattro H): colour grid and top layer as means over
//! `n × n` blocks, the chain then runs on the smaller images.

use rayon::prelude::*;

use super::Px;

/// Block means of the inputs of the chain.
pub struct Pooled {
    pub gw: usize,
    pub gh: usize,
    /// `gw × gh`
    pub grid: Vec<Px>,
    /// `2gw × 2gh`
    pub top: Vec<f32>,
    /// width and height of the top layer that hold image content (the rest is repeated from the edge)
    pub keep: (usize, usize),
}

/// Means over `n × n` blocks of the grid (`gw × gh`) and of the top layer (`2gw × 2gh`). The pooled grid keeps
/// whole blocks only. The chain needs a width and height divisible by 4 (the first stage halves the grid once more):
/// where the number of whole blocks is not, the last column or row of blocks is repeated up to the next multiple
/// ([`Pooled::keep`] names the part with content). Cutting down instead made the binned picture smaller than every
/// other binned size of the photo (image size / n): 4 pixels narrower at factor 4 on both sensor sizes, one row
/// lower at factor 3 on the sd Quattro.
pub fn pool(grid: &[Px], top: &[f32], gw: usize, gh: usize, n: usize) -> Pooled {
    let n = n.max(1);
    // whole blocks of the top layer (an odd factor leaves one more top block than twice the whole grid blocks)
    let keep = (2 * gw / n, 2 * gh / n);
    let cells = (gw / n, gh / n);
    let (pw, ph) = ((keep.0.div_ceil(2) + 3) & !3, (keep.1.div_ceil(2) + 3) & !3);
    if cells.0 == 0 || cells.1 == 0 {
        return Pooled { gw: 0, gh: 0, grid: Vec::new(), top: Vec::new(), keep: (0, 0) };
    }
    let area = (n * n) as f32;
    let means = (0..pw * ph)
        .into_par_iter()
        .map(|i| {
            let (x0, y0) = ((i % pw).min(cells.0 - 1) * n, (i / pw).min(cells.1 - 1) * n);
            let mut sum = [0f32; 3];
            for row in grid[y0 * gw..].chunks(gw).take(n) {
                for p in &row[x0..x0 + n] {
                    sum = [sum[0] + p[0], sum[1] + p[1], sum[2] + p[2]];
                }
            }
            sum.map(|s| s / area)
        })
        .collect();
    let (tw, fw) = (2 * pw, 2 * gw);
    let layer = (0..tw * 2 * ph)
        .into_par_iter()
        .map(|i| {
            let (x0, y0) = ((i % tw).min(keep.0 - 1) * n, (i / tw).min(keep.1 - 1) * n);
            top[y0 * fw..].chunks(fw).take(n).map(|row| row[x0..x0 + n].iter().sum::<f32>()).sum::<f32>() / area
        })
        .collect();
    Pooled { gw: pw, gh: ph, grid: means, top: layer, keep }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_factor_gives_the_binned_size_of_both_sensors() {
        // grid sizes of the sd Quattro (2712 × 1808) and the sd Quattro H (3096 × 2064), binning 2, 3, 4: the part
        // with content is the image size divided by the factor, the pooled grid divisible by 4
        for (gw, gh) in [(2712usize, 1808usize), (3096, 2064)] {
            let grid: Vec<Px> = (0..gw * gh).map(|i| [(i % gw) as f32, 1.0, 2.0]).collect();
            let top = vec![3.0f32; 4 * gw * gh];
            for n in [2usize, 3, 4] {
                let p = pool(&grid, &top, gw, gh, n);
                assert_eq!(p.keep, (2 * gw / n, 2 * gh / n), "{gw} × {gh} / {n}");
                assert!(p.gw % 4 == 0 && p.gh % 4 == 0 && 2 * p.gw >= p.keep.0 && 2 * p.gw < p.keep.0 + 8 && 2 * p.gh >= p.keep.1);
                assert_eq!((p.grid.len(), p.top.len()), (p.gw * p.gh, 4 * p.gw * p.gh));
                // repeated columns equal the last column with content
                assert_eq!(p.grid[p.gw - 1], p.grid[gw / n - 1]);
            }
        }
    }

    #[test]
    fn means_of_whole_blocks() {
        let (gw, gh, n) = (26, 14, 3);
        let grid: Vec<Px> = (0..gw * gh).map(|i| [(i % gw) as f32, (i / gw) as f32, 7.0]).collect();
        let top: Vec<f32> = (0..4 * gw * gh).map(|i| (i % (2 * gw)) as f32).collect();
        let p = pool(&grid, &top, gw, gh, n);
        // 8 × 4 whole grid blocks, 17 × 9 whole top blocks; the pooled grid is filled up to multiples of 4
        assert_eq!((p.gw, p.gh), (12, 8));
        assert_eq!(p.keep, (17, 9));
        assert_eq!(p.grid.len(), 96);
        assert_eq!(p.top.len(), 4 * 96);
        // block (1, 1) covers the columns 3 … 5 and the rows 3 … 5
        assert_eq!(p.grid[p.gw + 1], [4.0, 4.0, 7.0]);
        assert_eq!(p.top[1], 4.0);
    }
}
