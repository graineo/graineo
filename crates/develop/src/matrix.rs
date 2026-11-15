//! Small 3×3 matrix helper (row-major).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat3(pub [f64; 9]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([1., 0., 0., 0., 1., 0., 0., 0., 1.]);

    /// Linear sRGB (D65) → CIE XYZ (D65).
    pub const SRGB_TO_XYZ: Mat3 = Mat3([
        0.4124564, 0.3575761, 0.1804375, //
        0.2126729, 0.7151522, 0.0721750, //
        0.0193339, 0.1191920, 0.9503041,
    ]);

    pub fn diag(d: [f64; 3]) -> Mat3 {
        Mat3([d[0], 0., 0., 0., d[1], 0., 0., 0., d[2]])
    }

    pub fn mul(&self, o: &Mat3) -> Mat3 {
        let (a, b) = (&self.0, &o.0);
        Mat3(std::array::from_fn(|i| {
            let (r, c) = (i / 3, i % 3);
            a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c]
        }))
    }

    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let m = &self.0;
        std::array::from_fn(|r| m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2])
    }

    pub fn scale(&self, k: f64) -> Mat3 {
        Mat3(self.0.map(|x| x * k))
    }

    pub fn inverse(&self) -> Option<Mat3> {
        let m = &self.0;
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        Some(Mat3(inv.map(|x| x / det)))
    }

    pub fn to_f32(self) -> [f32; 9] {
        self.0.map(|x| x as f32)
    }
}
