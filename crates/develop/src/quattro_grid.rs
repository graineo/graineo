//! Quattro: colour grid directly from the stored sensor values, for the first noise reduction stage from capture
//! ISO 400 on.
//!
//! The grid for the first noise reduction stage is formed from the unprocessed sensor values: the bottom and middle
//! layers at their own resolution, the top layer as the mean of its 2 × 2 block, all in units of
//! (value − `RawOffset`) / (SensorISO/100); this matches Sigma's output. The grid recovered from x3fuse-core's
//! prepared layers differs from that at high ISO (its luminance noise reduction lowers dark values, its top layer sits slightly higher in the darks), which
//! shifts the colour of dark regions once the stage smooths them. This module reads the grid from the stored values
//! instead.
//!
//! Defective sensor pixels are not in the stored values' repair: a grid value that lies far outside the expected noise
//! of the defect-repaired prepared grid takes the prepared value.

use rayon::prelude::*;

use crate::quattro_nr::Px;

/// Stored sensor values of a Quattro file in the units of Sigma's first stage.
pub struct RawGrid {
    /// grid (bottom, middle, mean of the 2 × 2 top block), `gw × gh`
    pub grid: Vec<Px>,
    pub gw: usize,
    pub gh: usize,
    /// top layer at full resolution, `tw × th`
    pub top: Vec<f32>,
    pub tw: usize,
    pub th: usize,
}

/// Reads the stored values of `path` (active image area) and converts them: (value − `offset`) / (SensorISO/100)
/// · `digital[c]` (`unit_scale`).
pub fn read(path: &std::path::Path, offset: f32, unit_scale: [f32; 3]) -> Result<RawGrid, String> {
    let err = |e: x3f_core::Error| e.to_string();
    let mut r = x3f_core::Reader::open(path).map_err(err)?;
    r.load_camf().map_err(err)?;
    r.load_property_list().map_err(err)?;
    r.load_raw().map_err(err)?;
    let opts = |enc| x3f_core::ProcessOptions { color_encoding: enc, crop: true, fix_bad: false, apply_sgain: Some(false), denoise_intensity: 0, ..Default::default() };
    let low = r.get_image(&opts(x3f_core::ColorEncoding::Unprocessed)).map_err(err)?;
    let top = r.get_image(&opts(x3f_core::ColorEncoding::Qtop)).map_err(err)?;
    let (gw, gh) = (low.columns as usize, low.rows as usize);
    let (tw, th) = (top.columns as usize, top.rows as usize);
    if tw < 2 * gw || th < 2 * gh || low.channels < 2 {
        return Err(format!("Quattro-Raster: unerwartete Größen {gw}×{gh} / {tw}×{th}"));
    }
    let lc = low.channels as usize;
    let tc = top.channels as usize;
    let top_units: Vec<f32> = (0..th)
        .into_par_iter()
        .flat_map_iter(|y| {
            let row = top.row(y as u32);
            (0..tw).map(move |x| (row[x * tc] as f32 - offset) * unit_scale[2])
        })
        .collect();
    let grid: Vec<Px> = (0..gh)
        .into_par_iter()
        .flat_map_iter(|y| {
            let row = low.row(y as u32);
            let top_units = &top_units;
            (0..gw).map(move |x| {
                let t = 2 * y * tw + 2 * x;
                let mean = 0.25 * (top_units[t] + top_units[t + 1] + top_units[t + tw] + top_units[t + tw + 1]);
                [(row[x * lc] as f32 - offset) * unit_scale[0], (row[x * lc + 1] as f32 - offset) * unit_scale[1], mean]
            })
        })
        .collect();
    Ok(RawGrid { grid, gw, gh, top: top_units, tw, th })
}

/// Defect repair: a value farther than [`DEFECT_SIGMA`] noise deviations (plus [`DEFECT_SHARE`] of the value) from
/// the defect-repaired prepared value takes the prepared value (`grid`: prepared grid, `top`: prepared top layer at
/// full resolution, same units). The grid's top channel is formed again from the repaired top layer.
pub fn repair(r: &mut RawGrid, grid: &[Px], top: &[f32]) {
    let (gw, tw) = (r.gw, r.tw);
    let far = |v: f32, reference: f32, var: f64| (v - reference).abs() > DEFECT_SIGMA * var.sqrt() as f32 + DEFECT_SHARE * reference.abs();
    // top layer: one full-resolution value has four times the variance of the 2 × 2 mean the model describes
    r.top.par_chunks_mut(tw).zip(top.par_chunks(tw)).for_each(|(row, reference)| {
        for (v, &q) in row.iter_mut().zip(reference) {
            if far(*v, q, 4.0 * crate::quattro_nr::grid_variance([0.0, 0.0, q])[2]) {
                *v = q;
            }
        }
    });
    let t = &r.top;
    r.grid.par_chunks_mut(gw).zip(grid.par_chunks(gw)).enumerate().for_each(|(y, (row, reference))| {
        for (x, (p, q)) in row.iter_mut().zip(reference).enumerate() {
            let var = crate::quattro_nr::grid_variance(*q);
            for c in 0..2 {
                if far(p[c], q[c], var[c]) {
                    p[c] = q[c];
                }
            }
            let i = 2 * y * tw + 2 * x;
            p[2] = 0.25 * (t[i] + t[i + 1] + t[i + tw] + t[i + tw + 1]);
        }
    });
}

/// Defect threshold in noise deviations of the stored value.
const DEFECT_SIGMA: f32 = 8.0;
/// Defect threshold as a share of the value (bright regions, where the model's deviation is small against shading).
const DEFECT_SHARE: f32 = 0.25;


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_replaces_defects_only() {
        let (gw, gh) = (4, 3);
        let (tw, th) = (8, 6);
        let reference: Vec<Px> = vec![[100.0, 80.0, 60.0]; gw * gh];
        let top_ref = vec![60.0f32; tw * th];
        let mut top = vec![61.0f32; tw * th];
        // hot top pixel (e.g. a highlight pixel of the dp Quattro) and a hot bottom-layer value
        top[2 * tw + 3] = 900.0;
        let mut grid: Vec<Px> = vec![[103.0, 78.0, 61.0]; gw * gh];
        grid[5][0] = 2000.0;
        let mut r = RawGrid { grid, gw, gh, top, tw, th };
        repair(&mut r, &reference, &top_ref);
        assert_eq!(r.top[2 * tw + 3], 60.0);
        assert_eq!(r.top[0], 61.0);
        assert_eq!(r.grid[5][0], 100.0);
        assert_eq!(r.grid[0][0], 103.0);
        assert_eq!(r.grid[0][1], 78.0);
        // the grid's top channel is the 2 × 2 mean of the repaired top layer
        assert!((r.grid[5][2] - 0.25 * (61.0 * 3.0 + 60.0)).abs() < 1e-4);
        assert!((r.grid[0][2] - 61.0).abs() < 1e-4);
    }
}
