//! Face alignment: from five facial landmarks (eyes, nose, mouth corners) compute a similarity transform
//! (rotation, scale, translation) onto the standard 112×112 template and crop the image
//! accordingly.

use image::RgbImage;

/// Standard template of the face models (112×112): left eye, right eye, nose, left, right mouth corner
/// – "left" as seen in the image.
pub const TEMPLATE: [(f32, f32); 5] = [(38.2946, 51.6963), (73.5318, 51.5014), (56.0252, 71.7366), (41.5493, 92.3655), (70.7299, 92.2041)];
pub const SIDE: usize = 112;

/// Similarity transform target = s·R·source + t as [a, b, tx, ty] with x' = a·x − b·y + tx, y' = b·x + a·y + ty
/// (least squares, Umeyama).
pub fn similarity_transform(src: &[(f32, f32); 5], dst: &[(f32, f32); 5]) -> [f32; 4] {
    let n = 5.0f64;
    let mean = |p: &[(f32, f32); 5]| (p.iter().map(|q| q.0 as f64).sum::<f64>() / n, p.iter().map(|q| q.1 as f64).sum::<f64>() / n);
    let (mq, mz) = (mean(src), mean(dst));
    let (mut sxx, mut sxy, mut var) = (0f64, 0f64, 0f64);
    for i in 0..5 {
        let (qx, qy) = (src[i].0 as f64 - mq.0, src[i].1 as f64 - mq.1);
        let (zx, zy) = (dst[i].0 as f64 - mz.0, dst[i].1 as f64 - mz.1);
        sxx += qx * zx + qy * zy;
        sxy += qx * zy - qy * zx;
        var += qx * qx + qy * qy;
    }
    let var = var.max(1e-9);
    let (a, b) = (sxx / var, sxy / var);
    let tx = mz.0 - (a * mq.0 - b * mq.1);
    let ty = mz.1 - (b * mq.0 + a * mq.1);
    [a as f32, b as f32, tx as f32, ty as f32]
}

/// Aligned 112×112 face (RGB, row by row), bilinear from the source image.
pub fn crop_aligned(img: &RgbImage, points: &[(f32, f32); 5]) -> Vec<u8> {
    let [a, b, tx, ty] = similarity_transform(points, &TEMPLATE);
    // Backward mapping (target → source): inverse of the similarity
    let d = a * a + b * b;
    let (ia, ib) = (a / d, -b / d);
    let mut out = vec![0u8; SIDE * SIDE * 3];
    let (w, h) = (img.width() as i64, img.height() as i64);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let (dx, dy) = (x as f32 - tx, y as f32 - ty);
            let sx = ia * dx - ib * dy;
            let sy = ib * dx + ia * dy;
            let (x0, y0) = (sx.floor() as i64, sy.floor() as i64);
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let sample = |xx: i64, yy: i64, c: usize| -> f32 {
                let (xx, yy) = (xx.clamp(0, w - 1), yy.clamp(0, h - 1));
                img.as_raw()[((yy * w + xx) * 3) as usize + c] as f32
            };
            for c in 0..3 {
                let v = sample(x0, y0, c) * (1.0 - fx) * (1.0 - fy) + sample(x0 + 1, y0, c) * fx * (1.0 - fy) + sample(x0, y0 + 1, c) * (1.0 - fx) * fy + sample(x0 + 1, y0 + 1, c) * fx * fy;
                out[(y * SIDE + x) * 3 + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Estimate five points from a face box (when no points are available, e.g. from an imported
/// box): fixed proportions within the box. Less accurate than real points.
pub fn points_from_box(x: f32, y: f32, b: f32, h: f32) -> [(f32, f32); 5] {
    [(x + 0.32 * b, y + 0.40 * h), (x + 0.68 * b, y + 0.40 * h), (x + 0.50 * b, y + 0.60 * h), (x + 0.36 * b, y + 0.78 * h), (x + 0.64 * b, y + 0.78 * h)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_finds_rotation_and_scale() {
        // points = template, rotated by 20°, twice as large, shifted
        let (c, s) = (20f32.to_radians().cos() * 2.0, 20f32.to_radians().sin() * 2.0);
        let mut q = [(0f32, 0f32); 5];
        for (i, p) in TEMPLATE.iter().enumerate() {
            q[i] = (c * p.0 - s * p.1 + 30.0, s * p.0 + c * p.1 - 10.0);
        }
        let [a, b, tx, ty] = similarity_transform(&q, &TEMPLATE);
        for (i, p) in q.iter().enumerate() {
            let (x, y) = (a * p.0 - b * p.1 + tx, b * p.0 + a * p.1 + ty);
            assert!((x - TEMPLATE[i].0).abs() < 0.01 && (y - TEMPLATE[i].1).abs() < 0.01, "point {i}: {x} {y}");
        }
    }
}
