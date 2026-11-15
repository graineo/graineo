//! 33³ sRGB LUT of the point-wise film/paper response, without spatial effects.
use super::*;
pub fn cube_lut(fs: &FilmSettings, name: &str) -> Result<String, String> {
    let n = 33usize;
    let values = color_response(fs, &graineo_color::cube::grid(n))?;
    graineo_color::cube::write(
        name,
        &[
            "sRGB encoded input and output",
            "Spatial effects (grain, halation, diffusion and local masks) excluded",
            "Film model: spektrafilm, Andrea Volpato (GPL-3.0); profile data CC BY-SA 4.0",
        ],
        n,
        &values,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cube_has_correct_size_order_and_finite_samples() {
        let f = FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            ..Default::default()
        };
        let text = cube_lut(&f, "test\n\"name").unwrap();
        assert!(text.contains("LUT_3D_SIZE 33"));
        let rows: Vec<Vec<f32>> = text
            .lines()
            .filter_map(|l| {
                l.split_whitespace()
                    .map(str::parse)
                    .collect::<Result<Vec<f32>, _>>()
                    .ok()
            })
            .filter(|p| p.len() == 3)
            .collect();
        assert_eq!(rows.len(), 33usize.pow(3));
        assert!(rows
            .iter()
            .flatten()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        let center = 16 * 33 * 33 + 16 * 33 + 16;
        let direct = color_response(&f, &[[0.5; 3]]).unwrap();
        for c in 0..3 {
            assert!((rows[center][c] - direct[0][c]).abs() < 1e-6);
        }
        assert!(rows[0][0] < rows.last().unwrap()[0]);
    }
}
