//! EXIF of the embedded JPEG.
//!
//! Quattro files (X3F 4.x) have no PROP section; camera, exposure, orientation and the
//! colour mode selected in the camera are only in the EXIF of the preview JPEG (the colour mode
//! in Sigma's MakerNote, tag 0x003D "PictureMode").

/// The EXIF fields relevant to us.
#[derive(Debug, Clone, Default)]
pub struct JpegExif {
    pub make: Option<String>,
    pub model: Option<String>,
    /// EXIF orientation 1 … 8
    pub orientation: Option<u16>,
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    pub exposure_bias: Option<f64>,
    pub focal_length: Option<f64>,
    pub focal_length_35mm: Option<u32>,
    /// 1 = sRGB, 0xFFFF = uncalibrated (Adobe RGB)
    pub color_space: Option<u16>,
    pub date_time: Option<String>,
    pub serial: Option<String>,
    /// EXIF LensModel (sd Quattro: name of the mounted lens)
    pub lens_model: Option<String>,
    /// Sigma MakerNote 0x003D, e.g. "Standard", "Vivid", "FCBlue", "Monochrome"
    pub picture_mode: Option<String>,
    /// Sigma MakerNote 0x0007, white balance as set in the camera, e.g. "Auto", "Auto(Natural)"
    pub white_balance: Option<String>,
}

#[derive(Clone, Copy)]
struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, o: usize) -> Option<u16> {
        let b = self.d.get(o..o + 2)?;
        Some(if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    }

    fn u32(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o + 4)?;
        Some(if self.le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) })
    }

    /// Entries of an IFD at `ifd` (relative to `self.d`): (tag, type, count, value field offset).
    fn entries(&self, ifd: usize) -> Vec<(u16, u16, u32, usize)> {
        let Some(n) = self.u16(ifd) else { return Vec::new() };
        (0..n as usize)
            .filter_map(|k| {
                let e = ifd + 2 + k * 12;
                Some((self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)?, e + 8))
            })
            .collect()
    }

    fn type_size(ty: u16) -> usize {
        match ty {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => 1,
        }
    }

    /// Data of an entry (inline or via offset; offsets relative to `base`).
    fn data(&self, ty: u16, count: u32, field: usize, base: usize) -> Option<&'a [u8]> {
        let len = Self::type_size(ty).checked_mul(count as usize)?;
        if len <= 4 {
            self.d.get(field..field + len)
        } else {
            let off = (self.u32(field)? as usize).checked_sub(base)?;
            self.d.get(off..off + len)
        }
    }

    fn ascii(&self, ty: u16, count: u32, field: usize, base: usize) -> Option<String> {
        if ty != 2 && ty != 7 {
            return None;
        }
        let b = self.data(ty, count, field, base)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        let s = String::from_utf8_lossy(&b[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn short(&self, ty: u16, field: usize) -> Option<u32> {
        match ty {
            3 => self.u16(field).map(u32::from),
            4 => self.u32(field),
            _ => None,
        }
    }

    fn rational(&self, ty: u16, count: u32, field: usize) -> Option<f64> {
        let b = self.data(ty, count.min(1), field, 0)?;
        let t = Tiff { d: b, le: self.le };
        let (n, d) = (t.u32(0)?, t.u32(4)?);
        if d == 0 {
            return None;
        }
        Some(if ty == 10 { n as i32 as f64 / d as i32 as f64 } else { n as f64 / d as f64 })
    }
}

/// Read EXIF from a JPEG (first APP1 "Exif" segment).
pub fn parse_jpeg_exif(jpg: &[u8]) -> Option<JpegExif> {
    let mut i = 2;
    while i + 4 <= jpg.len() && jpg[i] == 0xFF {
        let marker = jpg[i + 1];
        let len = u16::from_be_bytes([jpg[i + 2], jpg[i + 3]]) as usize;
        let seg = jpg.get(i + 4..i + 2 + len)?;
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            return parse_tiff(&seg[6..]);
        }
        if marker == 0xDA {
            break;
        }
        i += 2 + len;
    }
    None
}

fn parse_tiff(d: &[u8]) -> Option<JpegExif> {
    let le = match d.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff { d, le };
    let mut out = JpegExif::default();
    let ifd0 = t.u32(4)? as usize;
    let mut exif_ifd = None;
    for (tag, ty, count, field) in t.entries(ifd0) {
        match tag {
            0x010F => out.make = t.ascii(ty, count, field, 0),
            0x0110 => out.model = t.ascii(ty, count, field, 0),
            0x0112 => out.orientation = t.short(ty, field).map(|v| v as u16),
            0x0132 => out.date_time = t.ascii(ty, count, field, 0),
            0x8769 => exif_ifd = t.u32(field).map(|v| v as usize),
            _ => {}
        }
    }
    if let Some(ifd) = exif_ifd {
        for (tag, ty, count, field) in t.entries(ifd) {
            match tag {
                0x829A => out.exposure_time = t.rational(ty, count, field),
                0x829D => out.f_number = t.rational(ty, count, field),
                0x8827 => out.iso = t.short(ty, field),
                0x9204 => out.exposure_bias = t.rational(ty, count, field),
                0x920A => out.focal_length = t.rational(ty, count, field),
                0xA405 => out.focal_length_35mm = t.short(ty, field),
                0xA001 => out.color_space = t.short(ty, field).map(|v| v as u16),
                0x9003 => out.date_time = t.ascii(ty, count, field, 0).or(out.date_time.take()),
                0xA431 => out.serial = t.ascii(ty, count, field, 0),
                0xA434 => out.lens_model = t.ascii(ty, count, field, 0),
                0x927C => {
                    out.picture_mode = sigma_maker_text(&t, ty, count, field, 0x003D);
                    out.white_balance = sigma_maker_text(&t, ty, count, field, 0x0007);
                }
                _ => {}
            }
        }
    }
    Some(out)
}

/// Text entry `want` of the Sigma MakerNote: "SIGMA\0\0\0", 2 bytes, then an IFD; offsets refer to the
/// TIFF start (not to the MakerNote).
fn sigma_maker_text(t: &Tiff, ty: u16, count: u32, field: usize, want: u16) -> Option<String> {
    if ty != 7 {
        return None;
    }
    let start = t.u32(field)? as usize;
    let maker = t.d.get(start..start.checked_add(count as usize)?)?;
    if maker.get(..8)? != b"SIGMA\0\0\0" {
        return None;
    }
    for (tag, ty, count, field) in t.entries(start + 10) {
        if tag == want {
            return t.ascii(ty, count, field, 0);
        }
    }
    None
}
