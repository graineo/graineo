//! Point curve of a mask (as the global tone curve of the development, on sRGB-encoded values): monotone cubic Hermite
//! spline (Fritsch-Carlson), horizontal before the first and after the last point, as a table of
//! [`crate::correction::LUT`] samples shared by the processor and the GPU.

/// Table of the curve over 0…1; empty for no curve or the diagonal.
pub(crate) fn table(points: &[[f32; 2]]) -> Vec<f32> {
    let n_lut = crate::correction::LUT;
    if is_identity(points) {
        return Vec::new();
    }
    let mut p: Vec<[f32; 2]> = points.iter().map(|q| [q[0].clamp(0.0, 1.0), q[1].clamp(0.0, 1.0)]).collect();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let x: Vec<f32> = p.iter().map(|q| q[0]).collect();
    let y: Vec<f32> = p.iter().map(|q| q[1]).collect();
    let n = x.len();
    let mut m = vec![0f32; n];
    if n >= 2 {
        let d: Vec<f32> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / (x[k + 1] - x[k]).max(1e-6)).collect();
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { 0.5 * (d[k - 1] + d[k]) };
        }
        for k in 0..n - 1 {
            if d[k] == 0.0 {
                m[k] = 0.0;
                m[k + 1] = 0.0;
                continue;
            }
            let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[k] = t * a * d[k];
                m[k + 1] = t * b * d[k];
            }
        }
    }
    let eval = |v: f32| -> f32 {
        if v <= x[0] || n == 1 {
            return y[0];
        }
        if v >= x[n - 1] {
            return y[n - 1];
        }
        let k = x.partition_point(|&xi| xi <= v).clamp(1, n - 1) - 1;
        let h = (x[k + 1] - x[k]).max(1e-6);
        let t = (v - x[k]) / h;
        let (t2, t3) = (t * t, t * t * t);
        ((2.0 * t3 - 3.0 * t2 + 1.0) * y[k] + (t3 - 2.0 * t2 + t) * h * m[k] + (-2.0 * t3 + 3.0 * t2) * y[k + 1] + (t3 - t2) * h * m[k + 1]).clamp(0.0, 1.0)
    };
    (0..n_lut).map(|i| eval(i as f32 / (n_lut - 1) as f32)).collect()
}

/// No points, or all on the diagonal from 0 to 1.
pub fn is_identity(p: &[[f32; 2]]) -> bool {
    p.is_empty() || (p.iter().all(|q| (q[0] - q[1]).abs() < 1e-5) && p[0][0] <= 1e-5 && p[p.len() - 1][0] >= 1.0 - 1e-5)
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_is_monotone_and_hits_the_points() {
        let t = super::table(&[[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]);
        assert_eq!(t.len(), crate::correction::LUT);
        assert!(t.windows(2).all(|w| w[1] >= w[0]));
        let at = |v: f32| t[(v * (t.len() - 1) as f32).round() as usize];
        assert!((at(0.25) - 0.15).abs() < 2e-3 && (at(0.75) - 0.85).abs() < 2e-3);
        assert!(super::table(&[[0.0, 0.0], [1.0, 1.0]]).is_empty());
    }
}
