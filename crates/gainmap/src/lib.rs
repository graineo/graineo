//! HDR gain map JPEG: an ordinary (SDR) JPEG with a second, small JPEG appended that says per pixel how much brighter
//! the image may get on an HDR display.
//!
//! Layout written here (the "Ultra HDR 1.1" arrangement):
//! - primary image: SDR base, unchanged entropy-coded data; added are an XMP packet with `hdrgm:Version` and the
//!   `Container:Directory` (Adobe gain map / Android), an APP2 segment with the ISO 21496-1 version and an APP2 MPF
//!   index (CIPA DC-007) pointing at the second image;
//! - gain map image: 8-bit grayscale JPEG at a quarter of the area with the full metadata twice: Adobe `hdrgm` XMP and
//!   the binary ISO 21496-1 block.
//!
//! Map semantics (both specifications): `G = min + (max − min) · v^(1/γ)` in stops, HDR = (SDR + o_sdr) · 2^(G·w) − o_hdr
//! with `w` the weight of the display (0 on SDR, 1 at the alternate headroom). The base is always the SDR image, so
//! readers that do not know gain maps show the normal picture.
//!
//! Platform independent; the reader also serves to read gain maps where the system cannot (Windows, Linux).
//! This product includes Gain Map technology under license by Adobe.

use std::ops::Range;

/// Namespace of the ISO 21496-1 APP2 segment (with the terminating zero).
pub const ISO_NS: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";
/// Namespace of a standard XMP APP1 segment.
pub const XMP_NS: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const MPF_NS: &[u8] = b"MPF\0";
const HDRGM: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
/// Common denominator of the rational values in the ISO block.
const DENOM: u32 = 1_000_000;
/// Offsets added to SDR and HDR before the ratio (avoids infinite gains in black), as the reference encoder uses.
pub const OFFSET: f32 = 1.0 / 64.0;

/// Gain map metadata (ISO 21496-1 terms; log2 values in stops).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metadata {
    /// 1 (luminance) or 3 (RGB) channels in the map
    pub channels: u8,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub gamma: [f32; 3],
    pub offset_sdr: [f32; 3],
    pub offset_hdr: [f32; 3],
    /// log2 headroom of the base (0 = SDR) and of the alternate (HDR) rendition
    pub base_headroom: f32,
    pub alternate_headroom: f32,
    /// base image is the HDR rendition (we never write that, readers must handle it)
    pub base_is_hdr: bool,
}

/// Largest magnitude in stops accepted for gains and headrooms read from a file (real files use a few stops; larger
/// values only overflow `exp2` to infinity downstream).
pub const MAX_STOPS: f32 = 32.0;
/// Largest gain map (pixels) read from a file: the map has at most the size of the image; this covers 200 MP.
pub const MAX_MAP_PIXELS: usize = 200_000_000;

impl Metadata {
    /// Metadata read from a file, checked: every value finite, gamma positive, gains and headrooms within
    /// ±[`MAX_STOPS`], offsets within 0 … 1 in magnitude (XMP accepts "NaN" and "inf" as numbers).
    fn sane(self) -> Option<Self> {
        let all = [self.min, self.max, self.gamma, self.offset_sdr, self.offset_hdr];
        let finite = all.iter().flatten().chain([&self.base_headroom, &self.alternate_headroom]).all(|v| v.is_finite());
        let stops = self.min.iter().chain(&self.max).chain([&self.base_headroom, &self.alternate_headroom]).all(|v| v.abs() <= MAX_STOPS);
        let gamma = self.gamma.iter().all(|g| *g > 1e-3 && *g <= 1e3);
        let offsets = self.offset_sdr.iter().chain(&self.offset_hdr).all(|v| v.abs() <= 1.0);
        (finite && stops && gamma && offsets).then_some(self)
    }

    /// One-channel map with gains from `min` to `max` stops, SDR base, HDR rendition at `max` stops headroom.
    pub fn luminance(min: f32, max: f32) -> Self {
        Metadata { channels: 1, min: [min; 3], max: [max; 3], gamma: [1.0; 3], offset_sdr: [OFFSET; 3], offset_hdr: [OFFSET; 3], base_headroom: 0.0, alternate_headroom: max.max(0.0), base_is_hdr: false }
    }

    /// Linear factor of map value `v` (0 … 1) in channel `c` at full weight.
    pub fn log2_gain(&self, c: usize, v: f32) -> f32 {
        let g = if self.gamma[c] == 1.0 { v } else { v.max(0.0).powf(1.0 / self.gamma[c]) };
        self.min[c] + (self.max[c] - self.min[c]) * g
    }

    /// Weight of the map for a display with `display_headroom` stops (ISO 21496-1: linear in log2 between base and
    /// alternate headroom).
    pub fn weight(&self, display_headroom: f32) -> f32 {
        let d = self.alternate_headroom - self.base_headroom;
        if d.abs() < 1e-6 {
            return if display_headroom >= self.alternate_headroom { 1.0 } else { 0.0 };
        }
        let w = ((display_headroom - self.base_headroom) / d).clamp(0.0, 1.0);
        if self.base_is_hdr {
            1.0 - w
        } else {
            w
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// ISO 21496-1 binary block

fn put_u16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn rational(v: f32) -> u32 {
    ((v as f64) * DENOM as f64).round() as i64 as i32 as u32
}

/// The ISO 21496-1 metadata block (without namespace): version, flags, common denominator, headrooms, per channel
/// min/max/gamma/offsets.
pub fn iso_encode(m: &Metadata) -> Vec<u8> {
    let mut o = Vec::with_capacity(96);
    put_u16(&mut o, 0); // minimum version
    put_u16(&mut o, 0); // writer version
    // version 0 defines only "multi-channel" and "use base colour space"; the other bits are reserved (0), every
    // value is its own fraction
    let mut flags = 0x40u8;
    if m.channels == 3 {
        flags |= 0x80;
    }
    o.push(flags);
    let frac = |o: &mut Vec<u8>, v: f32| {
        put_u32(o, rational(v));
        put_u32(o, DENOM);
    };
    frac(&mut o, m.base_headroom);
    frac(&mut o, m.alternate_headroom);
    for c in 0..m.channels.clamp(1, 3) as usize {
        frac(&mut o, m.min[c]);
        frac(&mut o, m.max[c]);
        frac(&mut o, m.gamma[c]);
        frac(&mut o, m.offset_sdr[c]);
        frac(&mut o, m.offset_hdr[c]);
    }
    o
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}
impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.d.get(self.p..self.p + 2)?;
        self.p += 2;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.d.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Read an ISO 21496-1 block (without namespace). None: only the version (primary image) or malformed.
pub fn iso_decode(d: &[u8]) -> Option<Metadata> {
    let mut r = Reader { d, p: 0 };
    let min_version = r.u16()?;
    let _writer = r.u16()?;
    if min_version != 0 {
        return None;
    }
    let flags = r.u8()?;
    let channels = if flags & 0x80 != 0 { 3 } else { 1 };
    let signed = |v: u32| v as i32 as f64;
    let mut m = Metadata::luminance(0.0, 0.0);
    m.channels = channels as u8;
    m.base_is_hdr = flags & 0x04 != 0;
    if flags & 0x08 != 0 {
        let den = r.u32()?.max(1) as f64;
        m.base_headroom = (r.u32()? as f64 / den) as f32;
        m.alternate_headroom = (r.u32()? as f64 / den) as f32;
        for c in 0..channels {
            m.min[c] = (signed(r.u32()?) / den) as f32;
            m.max[c] = (signed(r.u32()?) / den) as f32;
            m.gamma[c] = (r.u32()? as f64 / den) as f32;
            m.offset_sdr[c] = (signed(r.u32()?) / den) as f32;
            m.offset_hdr[c] = (signed(r.u32()?) / den) as f32;
        }
    } else {
        let mut frac = |s: bool| -> Option<f32> {
            let n = r.u32()?;
            let d = r.u32()?.max(1) as f64;
            Some((if s { signed(n) } else { n as f64 } / d) as f32)
        };
        m.base_headroom = frac(false)?;
        m.alternate_headroom = frac(false)?;
        for c in 0..channels {
            m.min[c] = frac(true)?;
            m.max[c] = frac(true)?;
            m.gamma[c] = frac(false)?;
            m.offset_sdr[c] = frac(true)?;
            m.offset_hdr[c] = frac(true)?;
        }
    }
    if channels == 1 {
        for f in [&mut m.min, &mut m.max, &mut m.gamma, &mut m.offset_sdr, &mut m.offset_hdr] {
            *f = [f[0]; 3];
        }
    }
    m.sane()
}

// ---------------------------------------------------------------------------------------------------------------------
// XMP

fn num(v: f32) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

/// hdrgm attribute value: one number, or an `rdf:Seq` element for three different channels.
fn hdrgm_values(m: &Metadata) -> (String, String) {
    let fields: [(&str, &[f32; 3]); 5] = [("GainMapMin", &m.min), ("GainMapMax", &m.max), ("Gamma", &m.gamma), ("OffsetSDR", &m.offset_sdr), ("OffsetHDR", &m.offset_hdr)];
    let mut attrs = String::new();
    let mut elems = String::new();
    for (name, v) in fields {
        if m.channels == 3 && (v[0] != v[1] || v[1] != v[2]) {
            elems.push_str(&format!("<hdrgm:{name}><rdf:Seq><rdf:li>{}</rdf:li><rdf:li>{}</rdf:li><rdf:li>{}</rdf:li></rdf:Seq></hdrgm:{name}>", num(v[0]), num(v[1]), num(v[2])));
        } else {
            attrs.push_str(&format!(" hdrgm:{name}=\"{}\"", num(v[0])));
        }
    }
    (attrs, elems)
}

/// XMP packet of the gain map image (Adobe gain map 1.0).
pub fn xmp_gain_map(m: &Metadata) -> String {
    let (attrs, elems) = hdrgm_values(m);
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"graineo\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:hdrgm=\"{HDRGM}\" hdrgm:Version=\"1.0\"{attrs} hdrgm:HDRCapacityMin=\"{}\" hdrgm:HDRCapacityMax=\"{}\" hdrgm:BaseRenditionIsHDR=\"{}\">{elems}</rdf:Description></rdf:RDF></x:xmpmeta>",
        num(m.base_headroom),
        num(m.alternate_headroom),
        if m.base_is_hdr { "True" } else { "False" }
    )
}

/// The `rdf:Description` of the primary image: gain map version and container directory (map length `map_len`).
fn primary_description(map_len: usize) -> String {
    format!(
        "<rdf:Description rdf:about=\"\" xmlns:Container=\"http://ns.google.com/photos/1.0/container/\" xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\" xmlns:hdrgm=\"{HDRGM}\" hdrgm:Version=\"1.0\"><Container:Directory><rdf:Seq><rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"Primary\" Item:Mime=\"image/jpeg\"/></rdf:li><rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"GainMap\" Item:Mime=\"image/jpeg\" Item:Length=\"{map_len}\"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description>"
    )
}

/// Primary XMP packet: the existing one (`old`, e.g. title and keywords of the export) with the gain map description
/// added before `</rdf:RDF>`, or a new packet.
fn primary_xmp(old: Option<&str>, map_len: usize) -> String {
    let desc = primary_description(map_len);
    // an earlier gain map description (rewrite) is replaced
    let stripped;
    let old = match old {
        Some(o) if o.contains("<Container:Directory>") => {
            let d = o.find("<Container:Directory>").unwrap_or(0);
            match (o[..d].rfind("<rdf:Description"), o[d..].find("</rdf:Description>")) {
                (Some(a), Some(b)) => {
                    stripped = format!("{}{}", &o[..a], &o[d + b + "</rdf:Description>".len()..]);
                    Some(stripped.as_str())
                }
                _ => Some(o),
            }
        }
        o => o,
    };
    if let Some(old) = old {
        if let Some(i) = old.rfind("</rdf:RDF>") {
            return format!("{}{desc}{}", &old[..i], &old[i..]);
        }
    }
    format!("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"graineo\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">{desc}</rdf:RDF></x:xmpmeta>")
}

/// Value of attribute `hdrgm:<name>` (attribute form or first `rdf:li` of the element form).
fn xmp_value(x: &str, name: &str, c: usize) -> Option<f32> {
    let key = format!("hdrgm:{name}");
    let mut from = 0;
    while let Some(i) = x[from..].find(&key) {
        let at = from + i + key.len();
        let rest = &x[at..];
        if let Some(r) = rest.strip_prefix("=\"").or_else(|| rest.strip_prefix("='")) {
            let end = r.find(['"', '\''])?;
            return r[..end].trim().parse().ok();
        }
        if rest.starts_with('>') {
            let mut r = rest;
            let mut v = None;
            for _ in 0..=c {
                let s = r.find("<rdf:li>")? + 8;
                let e = r[s..].find("</rdf:li>")? + s;
                v = r[s..e].trim().parse().ok();
                r = &r[e..];
            }
            if v.is_some() {
                return v;
            }
            return rest[1..].split('<').next()?.trim().parse().ok();
        }
        from = at;
    }
    None
}

/// Gain map metadata from an `hdrgm` XMP packet.
pub fn xmp_decode(x: &str) -> Option<Metadata> {
    if !x.contains(HDRGM) {
        return None;
    }
    let max0 = xmp_value(x, "GainMapMax", 0)?;
    let mut m = Metadata::luminance(0.0, max0);
    let three = x.contains("<hdrgm:GainMapMax>") || x.contains("<hdrgm:GainMapMin>") || x.contains("<hdrgm:Gamma>");
    m.channels = if three { 3 } else { 1 };
    for c in 0..3 {
        m.min[c] = xmp_value(x, "GainMapMin", c).unwrap_or(0.0);
        m.max[c] = xmp_value(x, "GainMapMax", c).unwrap_or(max0);
        m.gamma[c] = xmp_value(x, "Gamma", c).unwrap_or(1.0);
        m.offset_sdr[c] = xmp_value(x, "OffsetSDR", c).unwrap_or(OFFSET);
        m.offset_hdr[c] = xmp_value(x, "OffsetHDR", c).unwrap_or(OFFSET);
    }
    m.base_headroom = xmp_value(x, "HDRCapacityMin", 0).unwrap_or(0.0);
    m.alternate_headroom = xmp_value(x, "HDRCapacityMax", 0).unwrap_or(max0);
    m.base_is_hdr = x.contains("hdrgm:BaseRenditionIsHDR=\"True\"");
    m.sane()
}

// ---------------------------------------------------------------------------------------------------------------------
// JPEG segments

/// A marker segment of a JPEG header.
struct Segment<'a> {
    marker: u8,
    /// payload without marker and length
    data: &'a [u8],
    /// whole segment incl. marker
    raw: &'a [u8],
}

/// Header segments up to (not including) the first segment that is not APPn/COM; returns them and the offset where the
/// rest of the image starts.
fn header(jpeg: &[u8]) -> Option<(Vec<Segment<'_>>, usize)> {
    if jpeg.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut p = 2;
    let mut out = Vec::new();
    loop {
        if *jpeg.get(p)? != 0xFF {
            return None;
        }
        let marker = *jpeg.get(p + 1)?;
        if marker == 0xFF {
            p += 1;
            continue;
        }
        if !(0xE0..=0xEF).contains(&marker) && marker != 0xFE {
            return Some((out, p));
        }
        let len = u16::from_be_bytes([*jpeg.get(p + 2)?, *jpeg.get(p + 3)?]) as usize;
        if len < 2 {
            return None;
        }
        let end = p + 2 + len;
        out.push(Segment { marker, data: jpeg.get(p + 4..end)?, raw: jpeg.get(p..end)? });
        p = end;
    }
}

/// End of the first image (offset after its EOI): walks the marker segments and the entropy-coded data.
pub fn image_end(jpeg: &[u8], start: usize) -> Option<usize> {
    let mut p = start + 2;
    loop {
        if *jpeg.get(p)? != 0xFF {
            return None;
        }
        let marker = *jpeg.get(p + 1)?;
        match marker {
            0xFF => p += 1,
            0xD9 => return Some(p + 2),
            0xD0..=0xD7 | 0x01 => p += 2,
            _ => {
                let len = u16::from_be_bytes([*jpeg.get(p + 2)?, *jpeg.get(p + 3)?]) as usize;
                p += 2 + len;
                if marker == 0xDA {
                    // entropy-coded data: up to the next marker that is not a stuffed zero or a restart marker
                    loop {
                        let b = *jpeg.get(p)?;
                        if b == 0xFF {
                            let n = *jpeg.get(p + 1)?;
                            if n == 0x00 || (0xD0..=0xD7).contains(&n) || n == 0xFF {
                                p += if n == 0xFF { 1 } else { 2 };
                                continue;
                            }
                            break;
                        }
                        p += 1;
                    }
                }
            }
        }
    }
}

fn segment(marker: u8, ns: &[u8], body: &[u8]) -> Result<Vec<u8>, String> {
    let len = 2 + ns.len() + body.len();
    if len > 0xFFFF {
        return Err("JPEG segment too large".into());
    }
    let mut o = Vec::with_capacity(len + 2);
    o.extend_from_slice(&[0xFF, marker]);
    o.extend_from_slice(&(len as u16).to_be_bytes());
    o.extend_from_slice(ns);
    o.extend_from_slice(body);
    Ok(o)
}

/// Is this segment one of ours (gain map XMP version, ISO, MPF) that a rewrite replaces?
fn is_replaced(s: &Segment) -> bool {
    (s.marker == 0xE1 && s.data.starts_with(XMP_NS)) || (s.marker == 0xE2 && (s.data.starts_with(ISO_NS) || s.data.starts_with(MPF_NS)))
}

/// MPF APP2 segment for two images: primary (`primary_len` bytes at 0) and gain map (`map_len` bytes at `map_offset`
/// from the start of the MPF TIFF header).
fn mpf_segment(primary_len: u32, map_len: u32, map_offset: u32) -> Vec<u8> {
    let mut t = Vec::with_capacity(82);
    t.extend_from_slice(b"MM\0*");
    put_u32(&mut t, 8);
    put_u16(&mut t, 3);
    // MPFVersion, UNDEFINED ×4
    put_u16(&mut t, 0xB000);
    put_u16(&mut t, 7);
    put_u32(&mut t, 4);
    t.extend_from_slice(b"0100");
    // NumberOfImages, LONG
    put_u16(&mut t, 0xB001);
    put_u16(&mut t, 4);
    put_u32(&mut t, 1);
    put_u32(&mut t, 2);
    // MPEntry, UNDEFINED ×32 at offset 50
    put_u16(&mut t, 0xB002);
    put_u16(&mut t, 7);
    put_u32(&mut t, 32);
    put_u32(&mut t, 50);
    put_u32(&mut t, 0); // no next IFD
    // entry 1: baseline primary image
    put_u32(&mut t, 0x0003_0000);
    put_u32(&mut t, primary_len);
    put_u32(&mut t, 0);
    put_u16(&mut t, 0);
    put_u16(&mut t, 0);
    // entry 2: gain map
    put_u32(&mut t, 0);
    put_u32(&mut t, map_len);
    put_u32(&mut t, map_offset);
    put_u16(&mut t, 0);
    put_u16(&mut t, 0);
    segment(0xE2, MPF_NS, &t).expect("small")
}

/// Encode a one-channel map (`log2` gains in stops, `w × h`) as a grayscale JPEG with metadata `m` (XMP + ISO).
pub fn encode_map(log2: &[f32], w: usize, h: usize, m: &Metadata, quality: u8) -> Result<Vec<u8>, String> {
    if log2.len() != w * h || w == 0 || h == 0 || w > 65535 || h > 65535 {
        return Err("gain map: wrong size".into());
    }
    let span = (m.max[0] - m.min[0]).max(1e-6);
    let px: Vec<u8> = log2
        .iter()
        .map(|g| {
            let v = ((g - m.min[0]) / span).clamp(0.0, 1.0);
            let v = if m.gamma[0] == 1.0 { v } else { v.powf(m.gamma[0]) };
            (v * 255.0 + 0.5) as u8
        })
        .collect();
    let mut plain = Vec::new();
    jpeg_encoder::Encoder::new(&mut plain, quality).encode(&px, w as u16, h as u16, jpeg_encoder::ColorType::Luma).map_err(|e| e.to_string())?;
    // insert XMP and ISO metadata after SOI and a JFIF APP0 (which must come first)
    let at = if plain.get(2..4) == Some(&[0xFF, 0xE0]) { 4 + u16::from_be_bytes([plain[4], plain[5]]) as usize } else { 2 };
    let mut o = Vec::with_capacity(plain.len() + 1024);
    o.extend_from_slice(&plain[..at]);
    o.extend(segment(0xE1, XMP_NS, xmp_gain_map(m).as_bytes())?);
    o.extend(segment(0xE2, ISO_NS, &iso_encode(m))?);
    o.extend_from_slice(&plain[at..]);
    Ok(o)
}

/// Gain map JPEG `primary` + `map` (from [`encode_map`]) to one file. The entropy-coded data and all other segments of
/// `primary` stay byte for byte; an earlier gain map (trailing image, our segments) is replaced.
pub fn assemble(primary: &[u8], map: &[u8]) -> Result<Vec<u8>, String> {
    let (segs, rest) = header(primary).ok_or("not a JPEG")?;
    let end = image_end(primary, 0).ok_or("JPEG without end")?;
    let old_xmp = segs.iter().find(|s| s.marker == 0xE1 && s.data.starts_with(XMP_NS)).and_then(|s| std::str::from_utf8(&s.data[XMP_NS.len()..]).ok());
    let xmp = segment(0xE1, XMP_NS, primary_xmp(old_xmp, map.len()).as_bytes())?;
    let iso = segment(0xE2, ISO_NS, &[0, 0, 0, 0])?;
    let kept: Vec<&[u8]> = segs.iter().filter(|s| !is_replaced(s)).map(|s| s.raw).collect();
    // primary = SOI + kept + xmp + iso + mpf + rest
    let mpf_len = mpf_segment(0, 0, 0).len();
    let before_mpf = 2 + kept.iter().map(|s| s.len()).sum::<usize>() + xmp.len() + iso.len();
    let primary_len = before_mpf + mpf_len + (end - rest);
    // the MPF offsets count from its TIFF header: marker (2) + length (2) + "MPF\0" (4)
    let tiff_start = before_mpf + 8;
    let map_offset = primary_len - tiff_start;
    let mpf = mpf_segment(primary_len as u32, map.len() as u32, map_offset as u32);
    let mut o = Vec::with_capacity(primary_len + map.len());
    o.extend_from_slice(&[0xFF, 0xD8]);
    for s in kept {
        o.extend_from_slice(s);
    }
    o.extend(xmp);
    o.extend(iso);
    o.extend(mpf);
    o.extend_from_slice(&primary[rest..end]);
    debug_assert_eq!(o.len(), primary_len);
    o.extend_from_slice(map);
    Ok(o)
}

// ---------------------------------------------------------------------------------------------------------------------
// Reading

/// Where the gain map sits in a file and what it means.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub meta: Metadata,
    /// byte range of the gain map JPEG in the file
    pub map: Range<usize>,
    /// the metadata came from the ISO block (else from XMP)
    pub iso: bool,
}

/// MPF entries of the primary image: (offset in file, size) of every image after the first.
fn mpf_images(file: &[u8], segs: &[Segment]) -> Vec<(usize, usize)> {
    let Some(s) = segs.iter().find(|s| s.marker == 0xE2 && s.data.starts_with(MPF_NS)) else { return Vec::new() };
    let tiff = &s.data[4..];
    let tiff_start = s.data.as_ptr() as usize - file.as_ptr() as usize + 4;
    let be = match tiff.get(0..2) {
        Some(b"MM") => true,
        Some(b"II") => false,
        _ => return Vec::new(),
    };
    let u16_at = |p: usize| tiff.get(p..p + 2).map(|b| if be { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) });
    let u32_at = |p: usize| tiff.get(p..p + 4).map(|b| if be { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) });
    let Some(ifd) = u32_at(4) else { return Vec::new() };
    let ifd = ifd as usize;
    let n = u16_at(ifd).unwrap_or(0) as usize;
    let mut entries = None;
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if u16_at(e) == Some(0xB002) {
            let count = u32_at(e + 4).unwrap_or(0) as usize;
            let off = u32_at(e + 8).unwrap_or(0) as usize;
            entries = Some((off, count / 16));
        }
    }
    let Some((off, count)) = entries else { return Vec::new() };
    // every entry lies inside the segment: bounds the loop for a forged count
    let count = count.min(tiff.len() / 16);
    (1..count)
        .filter_map(|k| {
            let e = off + k * 16;
            let size = u32_at(e + 4)? as usize;
            let o = u32_at(e + 8)? as usize;
            (o > 0).then_some((tiff_start + o, size))
        })
        .collect()
}

/// Metadata of a gain map image (ISO block preferred, else XMP).
fn map_metadata(img: &[u8]) -> Option<(Metadata, bool)> {
    let (segs, _) = header(img)?;
    let iso = segs.iter().filter(|s| s.marker == 0xE2 && s.data.starts_with(ISO_NS)).find_map(|s| iso_decode(&s.data[ISO_NS.len()..]));
    if let Some(m) = iso {
        return Some((m, true));
    }
    segs.iter().filter(|s| s.marker == 0xE1 && s.data.starts_with(XMP_NS)).find_map(|s| std::str::from_utf8(&s.data[XMP_NS.len()..]).ok().and_then(xmp_decode)).map(|m| (m, false))
}

/// Find the gain map of a JPEG: MPF index (any image with gain map metadata), else the image after the primary EOI.
pub fn parse(file: &[u8]) -> Option<Parsed> {
    let (segs, _) = header(file)?;
    let mut candidates = mpf_images(file, &segs);
    if let Some(end) = image_end(file, 0) {
        if file.get(end..end + 2) == Some(&[0xFF, 0xD8]) {
            candidates.push((end, file.len() - end));
        }
    }
    candidates.into_iter().find_map(|(o, size)| {
        let img = file.get(o..o.checked_add(size)?.min(file.len()))?;
        let (meta, iso) = map_metadata(img)?;
        Some(Parsed { meta, map: o..o + img.len(), iso })
    })
}

/// Decoded gain map: log2 gains at full weight, `channels` values per pixel.
#[derive(Clone, Debug)]
pub struct Map {
    pub w: usize,
    pub h: usize,
    pub channels: usize,
    pub log2: Vec<f32>,
    pub meta: Metadata,
    pub iso: bool,
}

/// Read the gain map of a JPEG file's bytes.
pub fn read(file: &[u8]) -> Option<Map> {
    let p = parse(file)?;
    let mut dec = jpeg_decoder::Decoder::new(&file[p.map.clone()]);
    // the map's size comes from the file: refuse absurd sizes before the decoder allocates
    dec.read_info().ok()?;
    let i = dec.info()?;
    if (i.width as usize) * (i.height as usize) > MAX_MAP_PIXELS {
        return None;
    }
    dec.set_max_decoding_buffer_size(MAX_MAP_PIXELS * 3);
    let px = dec.decode().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let src_ch = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => 1,
        jpeg_decoder::PixelFormat::RGB24 => 3,
        _ => return None,
    };
    let channels = if p.meta.channels == 3 && src_ch == 3 { 3 } else { 1 };
    if w == 0 || h == 0 || px.len() < w * h * src_ch {
        return None;
    }
    let mut log2 = Vec::with_capacity(w * h * channels);
    for i in 0..w * h {
        for c in 0..channels {
            let v = px[i * src_ch + c] as f32 / 255.0;
            log2.push(p.meta.log2_gain(c, v));
        }
    }
    Some(Map { w, h, channels, log2, meta: p.meta, iso: p.iso })
}

// ---------------------------------------------------------------------------------------------------------------------
// Computing a map

/// sRGB-encoded 8-bit value → linear.
fn srgb_lut() -> &'static [f32; 256] {
    static L: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        std::array::from_fn(|i| {
            let v = i as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

/// Linear luminance of 8-bit sRGB pixels (`rgb`, `w × h`) averaged over 2×2 blocks: the SDR base at map resolution.
pub fn sdr_luminance_half(rgb: &[u8], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let l = srgb_lut();
    let (mw, mh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0f32; mw * mh];
    for my in 0..mh {
        for mx in 0..mw {
            let (mut s, mut n) = (0f32, 0f32);
            for y in my * 2..(my * 2 + 2).min(h) {
                for x in mx * 2..(mx * 2 + 2).min(w) {
                    let p = &rgb[(y * w + x) * 3..];
                    s += 0.2126 * l[p[0] as usize] + 0.7152 * l[p[1] as usize] + 0.0722 * l[p[2] as usize];
                    n += 1.0;
                }
            }
            out[my * mw + mx] = s / n;
        }
    }
    (out, mw, mh)
}

/// Box-resample a one-channel image to `tw × th` (area average; also enlarges by nearest neighbour).
pub fn resample(src: &[f32], sw: usize, sh: usize, tw: usize, th: usize) -> Vec<f32> {
    let mut out = vec![0f32; tw * th];
    let fx = sw as f32 / tw as f32;
    let fy = sh as f32 / th as f32;
    for ty in 0..th {
        let y0 = (ty as f32 * fy) as usize;
        let y1 = (((ty + 1) as f32 * fy).ceil() as usize).clamp(y0 + 1, sh);
        for tx in 0..tw {
            let x0 = (tx as f32 * fx) as usize;
            let x1 = (((tx + 1) as f32 * fx).ceil() as usize).clamp(x0 + 1, sw);
            let (mut s, mut n) = (0f32, 0f32);
            for y in y0.min(sh - 1)..y1 {
                for x in x0.min(sw - 1)..x1 {
                    s += src[y * sw + x];
                    n += 1.0;
                }
            }
            out[ty * tw + tx] = if n > 0.0 { s / n } else { 0.0 };
        }
    }
    out
}

/// Factor that brings scene luminance `scene` to the scale of the SDR luminance `sdr` in the midtones (median of
/// sdr/scene where the SDR lies between 5 and 35 % linear). 1 without enough midtones.
pub fn midtone_scale(sdr: &[f32], scene: &[f32]) -> f32 {
    let mut r: Vec<f32> = sdr.iter().zip(scene).filter(|(y, s)| (0.05..0.35).contains(*y) && **s > 1e-6).map(|(y, s)| y / s).collect();
    if r.len() < 64 {
        return 1.0;
    }
    let mid = r.len() / 2;
    r.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    r[mid].clamp(1.0 / 64.0, 64.0)
}

/// Target gain in stops of one pixel: SDR luminance `y` (linear, 0 … 1) and scene luminance `s` on the same scale.
///
/// The HDR rendition follows the output curve of the SDR image and, where that curve rolls the highlights off towards
/// SDR white, continues with the scene instead – up to `headroom` stops above SDR white, approached smoothly. Below
/// the highlights (SDR under 30 % linear) nothing changes, so the HDR rendition is the SDR image with brighter lights.
#[inline]
pub fn target_gain(y: f32, s: f32, headroom: f32) -> f32 {
    if headroom <= 0.0 {
        return 0.0;
    }
    let t = ((y - 0.30) / (0.75 - 0.30)).clamp(0.0, 1.0);
    let w = t * t * (3.0 - 2.0 * t);
    if w == 0.0 {
        return 0.0;
    }
    let raw = (s / y.max(1e-4)).log2().max(0.0) * w;
    headroom * (raw / headroom).tanh()
}

/// Gain map (log2, map resolution) for an SDR base with map-resolution luminance `sdr` and scene luminance `scene`
/// (already at map resolution and scale): gain incl. the offsets so that a reader's (SDR + o) · 2^G − o hits the target.
pub fn compute(sdr: &[f32], scene: &[f32], headroom: f32) -> Vec<f32> {
    sdr.iter()
        .zip(scene)
        .map(|(&y, &s)| {
            let g = target_gain(y, s, headroom);
            if g <= 0.0 {
                0.0
            } else {
                ((y * g.exp2() + OFFSET) / (y + OFFSET)).log2()
            }
        })
        .collect()
}

/// Result of [`add_to_jpeg`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Written {
    /// map size
    pub w: usize,
    pub h: usize,
    /// largest gain in stops (= HDR capacity written)
    pub max: f32,
    /// bytes of the map JPEG
    pub bytes: usize,
}

/// Add a gain map to the finished SDR JPEG bytes `jpeg`. `scene(w, h)` returns the scene luminance of the exported
/// image at the requested map size (same framing as the JPEG). `headroom` in stops. None: nothing above SDR white
/// (no map written; the JPEG stays as it is).
pub fn add_to_jpeg(jpeg: &[u8], scene: &dyn Fn(usize, usize) -> Option<Vec<f32>>, headroom: f32, quality: u8) -> Result<Option<(Vec<u8>, Written)>, String> {
    let mut dec = jpeg_decoder::Decoder::new(jpeg);
    let px = dec.decode().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or("JPEG without info")?;
    if info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
        return Ok(None);
    }
    let (w, h) = (info.width as usize, info.height as usize);
    let (sdr, mw, mh) = sdr_luminance_half(&px, w, h);
    let Some(mut sc) = scene(mw, mh) else { return Ok(None) };
    if sc.len() != mw * mh {
        return Err("gain map: scene size".into());
    }
    let k = midtone_scale(&sdr, &sc);
    sc.iter_mut().for_each(|v| *v *= k);
    let g = compute(&sdr, &sc, headroom);
    let max = g.iter().copied().fold(0.0f32, f32::max);
    if max < 0.05 {
        return Ok(None);
    }
    let meta = Metadata::luminance(0.0, max);
    let map = encode_map(&g, mw, mh, &meta, quality)?;
    let bytes = map.len();
    Ok(Some((assemble(jpeg, &map)?, Written { w: mw, h: mh, max, bytes })))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod robustness_tests;
