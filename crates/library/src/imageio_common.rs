//! Parts of image I/O that do not depend on macOS (types, EXIF date, gain map conversion).

/// Key data from the image properties.
#[derive(Clone, Debug, Default)]
pub struct Properties {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<u32>,
    /// 'YYYY-MM-DDThh:mm:ss'
    pub capture: Option<String>,
    pub camera: Option<String>,
    /// manufacturer (TIFF Make)
    pub maker: Option<String>,
    pub lens: Option<String>,
    pub aperture: Option<f64>,
    pub focal: Option<f64>,
    pub iso: Option<i64>,
    pub time: Option<f64>,
}

/// EXIF date "2024:05:11 14:03:22" → "2024-05-11T14:03:22".
pub fn exif_date(s: &str) -> Option<String> {
    let s = s.trim();
    let prefix = s.as_bytes().get(..19)?;
    for (i, &b) in prefix.iter().enumerate() {
        let valid = match i {
            4 | 7 | 13 | 16 => b == b':',
            10 => b == b' ',
            _ => b.is_ascii_digit(),
        };
        if !valid { return None; }
    }
    Some(format!("{}-{}-{}T{}", &s[0..4], &s[5..7], &s[8..10], &s[11..19]))
}

/// What goes into the file on export (see [`crate::export_meta`]). Shooting data and copyright are always
/// written; the personal fields only with `personal`, the location only with `gps`.
#[derive(Debug, Clone, Default)]
pub struct ExportFields {
    /// write stars, label, keywords, title and description (XMP)
    pub personal: bool,
    pub stars: u8,
    /// label name as in common photo apps ("Rot" …), empty = none
    pub label: String,
    /// keywords without names of people (`dc:subject`: leaves, with the parent terms if chosen)
    pub keywords: Vec<String>,
    /// full keyword paths ("Orte|Italien|Rom") for `lr:hierarchicalSubject`; empty = not written
    pub hierarchy: Vec<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// copyright (given at import): EXIF Copyright and XMP dc:rights
    pub copyright: Option<String>,
    /// take over the location (GPS) of the original, if it has one
    pub gps: bool,
    /// shooting data the app knows itself (X3F properties, RAW decoder); fills what the original's
    /// metadata (ImageIO) does not contain
    pub shot: Option<crate::export_meta::Shot>,
}

/// HDR data of an image with a gain map (Apple or ISO 21496-1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrInfo {
    /// largest brightening relative to the SDR image (linear factor, e.g. 3.8)
    pub headroom: f32,
    /// ISO gain map (else Apple only)
    pub iso: bool,
}

/// Gain map in its own resolution, already converted to log2 brightening (full effect at
/// `alternate_headroom`): ISO 21496-1 `G = min + (max − min) · g^(1/γ)`; Apple metadata only:
/// `log2(1 + (headroom − 1) · g)`. Without EXIF rotation (like the image).
#[derive(Clone, Debug)]
pub struct GainMap {
    pub w: usize,
    pub h: usize,
    pub log2: Vec<f32>,
    /// log2 of the headroom values of base (SDR, usually 0) and alternate (HDR)
    pub base_headroom: f32,
    pub alternate_headroom: f32,
    pub base_offset: f32,
    pub alternate_offset: f32,
}

/// How the export image was derived from the original (rotation, crop, fine angle,
/// perspective): this gives the gain map the same geometry as the image.
pub struct Mapping<'a> {
    /// size of the original in pixels (width, height)
    pub source: (usize, usize),
    /// size of the export image in pixels
    pub output: (usize, usize),
    /// point in the export image (0 … 1 per axis) → point in the original (continuous in pixels, pixel i
    /// covers i … i+1)
    pub to_source: &'a dyn Fn([f32; 2]) -> [f32; 2],
}

/// Convert an 8-bit gain map (`L008`) to the geometry of the export image (bilinear). The map
/// keeps its resolution relative to the image (map pixels per image pixel as in the original).
/// Returns: pixels, width, height.
pub fn map_gain_map(data: &[u8], bpr: usize, w: usize, h: usize, map_px: &Mapping) -> (Vec<u8>, usize, usize) {
    let (sw, sh) = (map_px.source.0.max(1) as f32, map_px.source.1.max(1) as f32);
    let scale_factor = w as f32 / sw;
    let nw = ((map_px.output.0 as f32 * scale_factor).round() as usize).max(1);
    let nh = ((map_px.output.1 as f32 * scale_factor).round() as usize).max(1);
    let mut out = vec![0u8; nw * nh];
    let (kx, ky) = (w as f32 / sw, h as f32 / sh);
    for y in 0..nh {
        for x in 0..nw {
            let p = (map_px.to_source)([(x as f32 + 0.5) / nw as f32, (y as f32 + 0.5) / nh as f32]);
            let mx = (p[0] * kx - 0.5).clamp(0.0, (w - 1) as f32);
            let my = (p[1] * ky - 0.5).clamp(0.0, (h - 1) as f32);
            let (x0, y0) = (mx.floor() as usize, my.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (mx - x0 as f32, my - y0 as f32);
            let g = |xx: usize, yy: usize| data[yy * bpr + xx] as f32;
            let o = (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy;
            out[y * nw + x] = (o + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
    (out, nw, nh)
}
