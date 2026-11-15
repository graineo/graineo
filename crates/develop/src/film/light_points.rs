//! Own test chart, inspired by spektrafilm's light-point display.
//! Nine light points and a stepped line, −2 to +6 EV relative to middle grey.
use crate::Linear;
pub fn light_points(width: usize, height: usize) -> Linear {
    let w = width.max(180);
    let h = height.max(100);
    let mut data = vec![0.0001; w * h * 3];
    let radius = (w as f32 / 320.).max(2.);
    let line_y = (h as f32 * 0.75) as usize;
    for i in 0..9 {
        let cx = (i + 1) * w / 10;
        let cy = h / 3;
        let level = 0.184 * 2f32.powi(i as i32 - 2);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - cx as f32).powi(2) + (y as f32 - cy as f32).powi(2)).sqrt();
                if d <= radius {
                    data[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(level);
                }
            }
        }
        let start = (i * w / 10 + w / 20).min(w);
        let end = ((i + 1) * w / 10 + w / 20).min(w);
        for y in line_y..(line_y + radius.ceil() as usize).min(h) {
            for x in start..end {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(level);
            }
        }
    }
    Linear { mono: false, haze: None, toneq: None,
        width: w,
        height: h,
        data,
        detail: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn points_have_exact_exposures() {
        let im = light_points(1000, 500);
        for i in 0..9 {
            let actual = im.data[((500 / 3) * 1000 + (i + 1) * 100) * 3];
            assert_eq!(actual, 0.184 * 2f32.powi(i as i32 - 2));
        }
        assert!(im.data.iter().all(|v| v.is_finite()));
    }
}
