//! DNG OpcodeList2 value maps: MapTable (opcode 6) and MapPolynomial (opcode 8).
//!
//! The decoder delivers the stored values but does not run these opcodes. Lossy DNGs (Adobe DNG Converter,
//! "lossy compression") store 8-bit values and put the curve back to linear as a cubic MapPolynomial per plane
//! (0.0625·x + 0.9375·x³); without it the image came out 1.5–2 EV too bright with a color cast. Per the DNG
//! specification OpcodeList2 works on values normalized to black…white (0…1), after linearization and before
//! demosaicing. The pixel data is replaced by `f32` values on the original raw scale, so every later step reads it as
//! before and nothing is quantized to the 8-bit grid.

/// One map of the list (area, plane range, pitch) and its function.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueMap {
    pub top: usize,
    pub left: usize,
    pub bottom: usize,
    pub right: usize,
    pub plane: usize,
    pub planes: usize,
    pub row_pitch: usize,
    pub col_pitch: usize,
    pub kind: MapKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MapKind {
    /// coefficients c0 + c1·x + c2·x² …
    Polynomial(Vec<f64>),
    /// table over 0…1 (entries spread evenly; the last entry holds for values beyond)
    Table(Vec<f32>),
}

impl MapKind {
    fn apply(&self, x: f32) -> f32 {
        match self {
            MapKind::Polynomial(c) => {
                let x = x as f64;
                c.iter().rev().fold(0.0, |acc, &k| acc * x + k) as f32
            }
            MapKind::Table(t) => {
                // DNG: the table index is the 16-bit value (0…65535 over 0…1); entries past the end repeat the last
                let i = (x.clamp(0.0, 1.0) * 65535.0).round() as usize;
                t.get(i).or(t.last()).copied().unwrap_or(0.0) / 65535.0
            }
        }
    }
}

/// Parse an opcode list (big-endian, as stored in the file); only the value maps, other opcodes are skipped.
pub fn parse(b: &[u8]) -> Vec<ValueMap> {
    let be32 = |o: usize| -> Option<u32> { Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
    let be16 = |o: usize| -> Option<u16> { Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?)) };
    let f64_ = |o: usize| -> Option<f64> { Some(f64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?)) };
    let mut out = Vec::new();
    let Some(n) = be32(0) else { return out };
    let mut p = 4;
    for _ in 0..n.min(256) {
        let (Some(id), Some(size)) = (be32(p), be32(p + 12)) else { break };
        let d = p + 16;
        if matches!(id, 6 | 8) {
            let u = |k: usize| be32(d + 4 * k).map(|v| v as usize);
            if let (Some(top), Some(left), Some(bottom), Some(right), Some(plane), Some(planes), Some(row_pitch), Some(col_pitch), Some(count)) =
                (u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7), u(8))
            {
                let kind = if id == 8 {
                    (count <= 8).then(|| (0..=count).map(|k| f64_(d + 36 + 8 * k)).collect::<Option<Vec<f64>>>()).flatten().map(MapKind::Polynomial)
                } else {
                    (count > 0 && count <= 65536).then(|| (0..count).map(|k| be16(d + 36 + 2 * k).map(f32::from)).collect::<Option<Vec<f32>>>()).flatten().map(MapKind::Table)
                };
                if let Some(kind) = kind {
                    out.push(ValueMap { top, left, bottom, right, plane, planes: planes.max(1), row_pitch: row_pitch.max(1), col_pitch: col_pitch.max(1), kind });
                }
            }
        }
        p = d + size as usize;
    }
    out
}

/// Apply the maps to raw values `data` (width × height × `cpp` interleaved); `black`/`white` per position and plane
/// as the caller's normalization uses them.
pub fn apply(maps: &[ValueMap], data: &mut [f32], width: usize, height: usize, cpp: usize, black: &(dyn Fn(usize, usize, usize) -> f32 + Sync), white: &(dyn Fn(usize) -> f32 + Sync)) {
    use rayon::prelude::*;
    for m in maps {
        let (y0, y1) = (m.top.min(height), m.bottom.min(height));
        let (x0, x1) = (m.left.min(width), m.right.min(width));
        let planes = m.plane..(m.plane + m.planes).min(cpp);
        data.par_chunks_mut(width * cpp).enumerate().filter(|(y, _)| *y >= y0 && *y < y1 && (*y - m.top) % m.row_pitch == 0).for_each(|(y, row)| {
            for x in (x0..x1).step_by(m.col_pitch) {
                for c in planes.clone() {
                    let (b, w) = (black(x, y, c), white(c));
                    let v = &mut row[x * cpp + c];
                    *v = b + (w - b) * m.kind.apply((*v - b) / (w - b));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polynomial_list(planes: &[(u32, [f64; 4])], w: u32, h: u32) -> Vec<u8> {
        let mut b = (planes.len() as u32).to_be_bytes().to_vec();
        for (plane, c) in planes {
            b.extend(8u32.to_be_bytes());
            b.extend(0x0103_0000u32.to_be_bytes());
            b.extend(0u32.to_be_bytes());
            b.extend(68u32.to_be_bytes());
            for v in [0, 0, h, w, *plane, 1, 1, 1, 3] {
                b.extend(v.to_be_bytes());
            }
            for v in c {
                b.extend(v.to_be_bytes());
            }
        }
        b
    }

    /// The curve of a lossy DNG (as written by the Adobe DNG Converter) maps the stored 8-bit value back to linear:
    /// mid gray of the file (0.34) becomes ≈ 0.058, white stays white, black stays black.
    #[test]
    fn lossy_dng_polynomial() {
        let list = polynomial_list(&[(0, [0.0, 0.0625, 0.0, 0.9375]), (1, [0.0, 0.0625, 0.0, 0.9375]), (2, [0.0, 0.0625, 0.0, 0.9375])], 2, 1);
        let maps = parse(&list);
        assert_eq!(maps.len(), 3);
        let mut px = vec![0.0, 255.0, 0.34 * 255.0, 255.0, 0.0, 0.34 * 255.0];
        apply(&maps, &mut px, 2, 1, 3, &|_, _, _| 0.0, &|_| 255.0);
        assert_eq!(px[0], 0.0);
        assert!((px[1] - 255.0).abs() < 1e-3);
        assert!((px[2] / 255.0 - 0.0581).abs() < 1e-3, "{}", px[2] / 255.0);
    }

    /// Other opcodes in the list are skipped, a damaged list gives no maps.
    #[test]
    fn other_opcodes_and_damage() {
        let mut list = 2u32.to_be_bytes().to_vec();
        // opcode 9 (GainMap) with 8 bytes of parameters
        list.extend(9u32.to_be_bytes());
        list.extend(0u32.to_be_bytes());
        list.extend(0u32.to_be_bytes());
        list.extend(8u32.to_be_bytes());
        list.extend([0u8; 8]);
        let poly = polynomial_list(&[(0, [0.0, 1.0, 0.0, 0.0])], 4, 4);
        list.extend(&poly[4..]);
        assert_eq!(parse(&list).len(), 1);
        assert!(parse(&list[..30]).is_empty());
        assert!(parse(&[]).is_empty());
    }
}
