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


/// Colour and geometry properties of a HEIF file's `meta` box (ICC/nclx for colour management, irot/imir).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeifColour {
    /// embedded ICC profile (`colr` of type `prof`)
    pub icc: Option<Vec<u8>>,
    /// colour primaries of a `colr` of type `nclx` (ITU-T H.273: 1 = BT.709/sRGB, 12 = Display P3)
    pub nclx_primaries: Option<u16>,
    /// the file has an `irot` or `imir` property: the HEIF decoder applies them and returns upright pixels
    pub transformed: bool,
    /// bits per channel of the first `pixi` property (8, 10 …)
    pub bit_depth: Option<u8>,
}

/// Read [`HeifColour`] from the `meta` box at the start of a HEIF file (the first `colr` counts: it belongs to the
/// primary image in files from phones and cameras).
pub fn heif_colour(data: &[u8]) -> HeifColour {
    let mut out = HeifColour::default();
    let be32 = |o: usize| data.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    // top-level boxes up to `meta`
    let mut o = 0usize;
    let meta = loop {
        let (Some(size), Some(kind)) = (be32(o), data.get(o + 4..o + 8)) else { return out };
        let (head, size) = match size {
            1 => (16, data.get(o + 8..o + 16).map(|b| u64::from_be_bytes(b.try_into().unwrap()) as usize).unwrap_or(0)),
            0 => (8, data.len() - o),
            s => (8, s),
        };
        if size < head {
            return out;
        }
        // sizes come from the file: a forged 64-bit size must not wrap the offset back (endless loop)
        let Some(end) = o.checked_add(size) else { return out };
        if kind == b"meta" {
            break (o + head + 4, end.min(data.len()));
        }
        o = end;
    };
    // walk the box tree inside meta; only container boxes on the way to ipco are entered
    fn walk(data: &[u8], start: usize, end: usize, out: &mut HeifColour, depth: u8) {
        let mut o = start;
        while o + 8 <= end && depth < 6 {
            let size = u32::from_be_bytes(data[o..o + 4].try_into().unwrap()) as usize;
            if size < 8 || o + size > end {
                return;
            }
            let kind = &data[o + 4..o + 8];
            match kind {
                b"iprp" | b"ipco" => walk(data, o + 8, o + size, out, depth + 1),
                b"irot" | b"imir" => out.transformed = true,
                b"pixi" if size >= 14 && out.bit_depth.is_none() => out.bit_depth = Some(data[o + 13]),
                b"colr" if size >= 12 && out.icc.is_none() && out.nclx_primaries.is_none() => match &data[o + 8..o + 12] {
                    b"prof" | b"rICC" => out.icc = Some(data[o + 12..o + size].to_vec()),
                    b"nclx" if size >= 14 => out.nclx_primaries = Some(u16::from_be_bytes([data[o + 12], data[o + 13]])),
                    _ => {}
                },
                _ => {}
            }
            o += size;
        }
    }
    walk(data, meta.0, meta.1, &mut out, 0);
    out
}

#[cfg(test)]
mod heif_colour_tests {
    fn bx(kind: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn heif_colour_reads_profile_nclx_and_rotation() {
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        let icc = b"ICCDATA".to_vec();
        let colr = bx(b"colr", &[&b"prof"[..], &icc].concat());
        let ipco = bx(b"ipco", &[bx(b"ispe", &[0u8; 12]), colr, bx(b"irot", &[1]), bx(b"pixi", &[0, 0, 0, 0, 3, 10, 10, 10])].concat());
        let meta = bx(b"meta", &[&[0u8; 4][..], &bx(b"hdlr", &[0u8; 24]), &bx(b"iprp", &ipco)].concat());
        let file = [ftyp.clone(), meta, bx(b"mdat", b"irotcolrprof")].concat();
        let c = super::heif_colour(&file);
        assert_eq!(c.icc, Some(icc));
        assert!(c.transformed);
        assert_eq!(c.bit_depth, Some(10));
        let nclx = bx(b"colr", &[&b"nclx"[..], &12u16.to_be_bytes(), &13u16.to_be_bytes(), &[0, 0, 0x80]].concat());
        let meta = bx(b"meta", &[&[0u8; 4][..], &bx(b"iprp", &bx(b"ipco", &nclx))].concat());
        let c = super::heif_colour(&[ftyp, meta].concat());
        assert_eq!((c.icc, c.nclx_primaries, c.transformed), (None, Some(12), false));
        assert_eq!(super::heif_colour(b"garbage"), super::HeifColour::default());
    }

    /// A forged 64-bit box size that wraps the offset back to 0 ended in an endless loop (1.5.6).
    #[test]
    fn heif_colour_forged_largesize_terminates() {
        // box at 0: size 16; box at 16: largesize 2^64 − 16 (o + size wraps to 0 without the check)
        let mut f = bx(b"ftyp", b"heicmif1");
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(b"free");
        f.extend_from_slice(&(u64::MAX - 15).to_be_bytes());
        f.extend_from_slice(&[0; 16]);
        assert_eq!(super::heif_colour(&f), super::HeifColour::default());
    }
}
