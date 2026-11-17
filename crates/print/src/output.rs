//! Print files: PDF (one image per page, exact page size, embedded ICC profile, 8 or 16 bit, Flate), TIFF
//! (8 or 16 bit, ICC, resolution) and JPEG (ICC, resolution). All three are written band by band from a
//! [`PageRaster`]; the page is never held in memory as a whole (JPEG excepted: the encoder needs the 8-bit page).
//!
//! The PDF writer is graineo's own, written from the PDF 1.7 specification (ISO 32000-1): a page is a single image
//! XObject drawn over the whole MediaBox, so the stream can go straight to the file and only its length is
//! written afterwards as an indirect object.

use std::io::{self, Seek, Write};
use std::path::Path;

use crate::raster::{PageRaster, BAND_ROWS};

/// Points per millimetre.
pub const PT_PER_MM: f64 = 72.0 / 25.4;

/// Something that delivers a page in bands.
pub trait Bands {
    fn size(&self) -> (usize, usize);
    fn band(&self, y0: usize, rows: usize, out: &mut Vec<u16>);
}

impl Bands for PageRaster {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn band(&self, y0: usize, rows: usize, out: &mut Vec<u16>) {
        PageRaster::band(self, y0, rows, out)
    }
}

/// Counts the bytes written (PDF offsets).
struct Counting<W: Write> {
    inner: W,
    pos: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A PDF with one full-page image per page.
pub struct PdfWriter<W: Write> {
    out: Counting<W>,
    /// byte offset of each object (index = object number − 1)
    offsets: Vec<u64>,
    icc_obj: usize,
    pages: Vec<usize>,
    pages_obj: usize,
}

fn num(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

impl<W: Write> PdfWriter<W> {
    /// Start the file: header and the ICC profile (`icc`, an RGB profile) shared by all pages.
    pub fn new(inner: W, icc: &[u8]) -> io::Result<Self> {
        let mut w = PdfWriter { out: Counting { inner, pos: 0 }, offsets: Vec::new(), icc_obj: 0, pages: Vec::new(), pages_obj: 0 };
        // 1.5: 16-bit images
        w.out.write_all(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")?;
        w.pages_obj = w.reserve();
        let icc_obj = w.reserve();
        let data = deflate(icc)?;
        w.begin(icc_obj)?;
        write!(w.out, "<< /N 3 /Alternate /DeviceRGB /Filter /FlateDecode /Length {} >>\nstream\n", data.len())?;
        w.out.write_all(&data)?;
        w.out.write_all(b"\nendstream\nendobj\n")?;
        w.icc_obj = icc_obj;
        Ok(w)
    }

    fn reserve(&mut self) -> usize {
        self.offsets.push(0);
        self.offsets.len()
    }

    fn begin(&mut self, obj: usize) -> io::Result<()> {
        self.offsets[obj - 1] = self.out.pos;
        write!(self.out, "{obj} 0 obj\n")
    }

    /// One page of `width_mm` × `height_mm` holding the raster of `page` (pixels from its bands).
    pub fn page(&mut self, page: &dyn Bands, width_mm: f64, height_mm: f64, bits16: bool) -> io::Result<()> {
        let (w, h) = page.size();
        let image = self.reserve();
        let length = self.reserve();
        let contents = self.reserve();
        let page_obj = self.reserve();
        self.begin(image)?;
        write!(
            self.out,
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace [/ICCBased {} 0 R] /BitsPerComponent {} /Interpolate false /Filter /FlateDecode /Length {length} 0 R >>\nstream\n",
            self.icc_obj,
            if bits16 { 16 } else { 8 }
        )?;
        let start = self.out.pos;
        {
            let mut z = flate2::write::ZlibEncoder::new(&mut self.out, flate2::Compression::new(6));
            let mut band = Vec::new();
            let mut bytes = Vec::new();
            let mut y = 0;
            while y < h {
                let rows = BAND_ROWS.min(h - y);
                page.band(y, rows, &mut band);
                bytes.clear();
                if bits16 {
                    bytes.extend(band.iter().flat_map(|v| v.to_be_bytes()));
                } else {
                    bytes.extend(band.iter().map(|&v| to8(v)));
                }
                z.write_all(&bytes)?;
                y += rows;
            }
            z.finish()?;
        }
        let len = self.out.pos - start;
        self.out.write_all(b"\nendstream\nendobj\n")?;
        self.begin(length)?;
        write!(self.out, "{len}\nendobj\n")?;
        let (wp, hp) = (num(width_mm * PT_PER_MM), num(height_mm * PT_PER_MM));
        let content = format!("q {wp} 0 0 {hp} 0 0 cm /Im0 Do Q\n");
        self.begin(contents)?;
        write!(self.out, "<< /Length {} >>\nstream\n{content}endstream\nendobj\n", content.len())?;
        self.begin(page_obj)?;
        write!(
            self.out,
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {wp} {hp}] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {contents} 0 R >>\nendobj\n",
            self.pages_obj
        )?;
        self.pages.push(page_obj);
        Ok(())
    }

    /// Page tree, catalogue, cross-reference table; returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        let pages = self.pages_obj;
        self.begin(pages)?;
        let kids: Vec<String> = self.pages.iter().map(|p| format!("{p} 0 R")).collect();
        write!(self.out, "<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n", kids.join(" "), self.pages.len())?;
        let info = self.reserve();
        self.begin(info)?;
        write!(self.out, "<< /Producer (graineo) /Creator (graineo) >>\nendobj\n")?;
        let catalog = self.reserve();
        self.begin(catalog)?;
        write!(self.out, "<< /Type /Catalog /Pages {pages} 0 R >>\nendobj\n")?;
        let xref = self.out.pos;
        write!(self.out, "xref\n0 {}\n0000000000 65535 f \n", self.offsets.len() + 1)?;
        for o in &self.offsets {
            write!(self.out, "{o:010} 00000 n \n")?;
        }
        write!(self.out, "trailer\n<< /Size {} /Root {catalog} 0 R /Info {info} 0 R >>\nstartxref\n{xref}\n%%EOF\n", self.offsets.len() + 1)?;
        self.out.flush()?;
        Ok(self.out.inner)
    }
}

fn to8(v: u16) -> u8 {
    ((v as u32 + 128) / 257) as u8
}

fn deflate(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(data)?;
    z.finish()
}

/// Write a file through a side file (`.part`) and rename it at the end: a cancelled or failed job never leaves half a file.
pub fn atomic_file(path: &Path, write: impl FnOnce(&mut io::BufWriter<std::fs::File>) -> io::Result<()>) -> io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let side = path.with_file_name(format!(".{name}.part"));
    let r = (|| {
        let mut f = io::BufWriter::new(std::fs::File::create(&side)?);
        write(&mut f)?;
        f.flush()?;
        f.into_inner().map_err(|e| e.into_error())?.sync_all()
    })();
    match r {
        Ok(()) => std::fs::rename(&side, path),
        Err(e) => {
            let _ = std::fs::remove_file(&side);
            Err(e)
        }
    }
}

/// TIFF (uncompressed strips, RGB, 8 or 16 bit, ICC profile, resolution in ppi).
pub fn write_tiff<W: Write + Seek>(out: &mut W, page: &dyn Bands, icc: &[u8], ppi: u32, bits16: bool) -> io::Result<()> {
    let (w, h) = page.size();
    let bps: u32 = if bits16 { 16 } else { 8 };
    let row_bytes = w as u64 * 3 * bps as u64 / 8;
    let strips = h.div_ceil(BAND_ROWS);
    let image_bytes = row_bytes * h as u64;
    if image_bytes > u32::MAX as u64 - 1_000_000 {
        return Err(io::Error::other("TIFF larger than 4 GB: choose a lower resolution or PDF"));
    }
    // layout: header, IFD, values that do not fit into the IFD, then the strips
    let n_tags = 15u64;
    let ifd = 8u64;
    let after_ifd = ifd + 2 + n_tags * 12 + 4;
    let bps_at = after_ifd;
    let xres_at = bps_at + 6;
    let yres_at = xres_at + 8;
    let soft = b"graineo\0";
    let soft_at = yres_at + 8;
    let offsets_at = soft_at + soft.len() as u64;
    let counts_at = offsets_at + 4 * strips as u64;
    let icc_at = counts_at + 4 * strips as u64;
    let data_at = (icc_at + icc.len() as u64 + 1) & !1;
    let mut head: Vec<u8> = Vec::new();
    head.extend_from_slice(b"II*\0");
    head.extend_from_slice(&(ifd as u32).to_le_bytes());
    head.extend_from_slice(&(n_tags as u16).to_le_bytes());
    let mut tag = |id: u16, typ: u16, count: u32, value: u32| {
        head.extend_from_slice(&id.to_le_bytes());
        head.extend_from_slice(&typ.to_le_bytes());
        head.extend_from_slice(&count.to_le_bytes());
        head.extend_from_slice(&value.to_le_bytes());
    };
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;
    const ASCII: u16 = 2;
    const UNDEFINED: u16 = 7;
    tag(256, LONG, 1, w as u32);
    tag(257, LONG, 1, h as u32);
    tag(258, SHORT, 3, bps_at as u32);
    tag(259, SHORT, 1, 1);
    tag(262, SHORT, 1, 2);
    tag(273, LONG, strips as u32, if strips == 1 { data_at as u32 } else { offsets_at as u32 });
    tag(274, SHORT, 1, 1);
    tag(277, SHORT, 1, 3);
    tag(278, LONG, 1, BAND_ROWS as u32);
    let last_rows = h - (strips - 1) * BAND_ROWS;
    let count = |i: usize| if i + 1 == strips { row_bytes * last_rows as u64 } else { row_bytes * BAND_ROWS as u64 };
    tag(279, LONG, strips as u32, if strips == 1 { count(0) as u32 } else { counts_at as u32 });
    tag(282, RATIONAL, 1, xres_at as u32);
    tag(283, RATIONAL, 1, yres_at as u32);
    tag(296, SHORT, 1, 2);
    tag(305, ASCII, soft.len() as u32, soft_at as u32);
    tag(34675, UNDEFINED, icc.len() as u32, icc_at as u32);
    head.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(head.len() as u64, after_ifd);
    for _ in 0..3 {
        head.extend_from_slice(&(bps as u16).to_le_bytes());
    }
    for _ in 0..2 {
        head.extend_from_slice(&ppi.to_le_bytes());
        head.extend_from_slice(&1u32.to_le_bytes());
    }
    head.extend_from_slice(soft);
    for i in 0..strips {
        head.extend_from_slice(&((data_at + row_bytes * (i * BAND_ROWS) as u64) as u32).to_le_bytes());
    }
    for i in 0..strips {
        head.extend_from_slice(&(count(i) as u32).to_le_bytes());
    }
    head.extend_from_slice(icc);
    head.resize(data_at as usize, 0);
    out.write_all(&head)?;
    let mut band = Vec::new();
    let mut bytes = Vec::new();
    let mut y = 0;
    while y < h {
        let rows = BAND_ROWS.min(h - y);
        page.band(y, rows, &mut band);
        bytes.clear();
        if bits16 {
            bytes.extend(band.iter().flat_map(|v| v.to_le_bytes()));
        } else {
            bytes.extend(band.iter().map(|&v| to8(v)));
        }
        out.write_all(&bytes)?;
        y += rows;
    }
    Ok(())
}

/// JPEG (8 bit, ICC profile, resolution in ppi).
pub fn write_jpeg<W: Write>(out: &mut W, page: &dyn Bands, icc: &[u8], ppi: u32, quality: u8) -> io::Result<()> {
    let (w, h) = page.size();
    let (Ok(w16), Ok(h16)) = (u16::try_from(w), u16::try_from(h)) else {
        return Err(io::Error::other("JPEG pages are limited to 65,535 pixels per side: choose TIFF or PDF"));
    };
    // the whole page in 8 bit: asked for, so a page too large for the memory is an error and not the end of the program
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(w * h * 3).map_err(|_| io::Error::other("not enough memory for this JPEG page: choose TIFF, PDF or a lower resolution"))?;
    let mut band = Vec::new();
    let mut y = 0;
    while y < h {
        let rows = BAND_ROWS.min(h - y);
        page.band(y, rows, &mut band);
        rgb.extend(band.iter().map(|&v| to8(v)));
        y += rows;
    }
    let to_io = |e: jpeg_encoder::EncodingError| io::Error::other(e.to_string());
    let mut enc = jpeg_encoder::Encoder::new(out, quality);
    let d = ppi.min(u16::MAX as u32) as u16;
    enc.set_density(jpeg_encoder::Density::Inch { x: d, y: d });
    enc.add_icc_profile(icc).map_err(to_io)?;
    enc.encode(&rgb, w16, h16, jpeg_encoder::ColorType::Rgb).map_err(to_io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{layout, PhotoSpec};
    use crate::raster::{prepare_cell, targets, CellImage};
    use crate::template::builtin;
    use crate::Image16;
    use std::io::Read;

    fn icc() -> Vec<u8> {
        // stand-in bytes (the app passes the real profile); the writer copies them unchanged
        (0..600u32).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn page(ppi: f64) -> (crate::Layout, PageRaster) {
        let t = builtin().into_iter().find(|t| t.name == "2 Fotos, A4").unwrap();
        let specs = vec![PhotoSpec { width: 300, height: 200, caption: String::new(), offset: [0.0; 2] }; 3];
        let l = layout(&t, &specs, None, "");
        let src = Image16 { width: 300, height: 200, data: (0..300 * 200 * 3).map(|i| (i * 13 % 65536) as u16).collect() };
        let nearest = |s: &Image16, w: usize, h: usize| {
            let mut o = Image16::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * s.height / h) * s.width + x * s.width / w) * 3;
                    o.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&s.data[i..i + 3]);
                }
            }
            o
        };
        let cells = targets(&l, 0, ppi).into_iter().map(|(k, rect)| CellImage { rect, img: prepare_cell(&src, &l.pages[0].photos[k], (rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize, &nearest) }).collect();
        let r = PageRaster::new(&l, 0, ppi, cells);
        (l, r)
    }

    /// Minimal PDF reader for the tests: objects by the cross-reference table.
    fn objects(pdf: &[u8]) -> Vec<(usize, Vec<u8>)> {
        let sx = pdf.windows(10).rposition(|w| w == b"startxref\n").unwrap();
        let tail = std::str::from_utf8(&pdf[sx + 10..]).unwrap();
        let xref: usize = tail.lines().next().unwrap().trim().parse().unwrap();
        assert!(pdf[xref..].starts_with(b"xref\n"), "startxref points to the table");
        let table = std::str::from_utf8(&pdf[xref..sx]).unwrap();
        let mut lines = table.lines().skip(1);
        let head: Vec<usize> = lines.next().unwrap().split(' ').map(|v| v.parse().unwrap()).collect();
        let mut out = Vec::new();
        for (n, line) in lines.take(head[1]).enumerate() {
            if line.ends_with("f ") || line.ends_with('f') {
                continue;
            }
            let off: usize = line[..10].parse().unwrap();
            let start = format!("{n} 0 obj\n");
            assert!(pdf[off..].starts_with(start.as_bytes()), "object {n} at {off}");
            let end = pdf[off..].windows(7).position(|w| w == b"endobj\n").unwrap();
            out.push((n, pdf[off..off + end].to_vec()));
        }
        out
    }

    fn stream_of(obj: &[u8]) -> Vec<u8> {
        let s = obj.windows(7).position(|w| w == b"stream\n").unwrap() + 7;
        let e = obj.windows(10).rposition(|w| w == b"\nendstream").unwrap();
        let mut z = flate2::read::ZlibDecoder::new(&obj[s..e]);
        let mut v = Vec::new();
        z.read_to_end(&mut v).unwrap();
        v
    }

    #[test]
    fn pdf_has_exact_page_size_pixels_profile_and_valid_xref() {
        for (bits16, ppi) in [(false, 150.0), (true, 100.0)] {
            let (l, r) = page(ppi);
            let mut w = PdfWriter::new(Vec::new(), &icc()).unwrap();
            w.page(&r, l.width_mm, l.height_mm, bits16).unwrap();
            w.page(&r, l.width_mm, l.height_mm, bits16).unwrap();
            let pdf = w.finish().unwrap();
            let objs = objects(&pdf);
            let text: Vec<String> = objs.iter().map(|(_, o)| String::from_utf8_lossy(&o[..o.len().min(400)]).into_owned()).collect();
            // A4 = 595.2756 × 841.8898 pt
            assert_eq!(text.iter().filter(|t| t.contains("/MediaBox [0 0 595.2756 841.8898]")).count(), 2);
            let img = objs.iter().find(|(_, o)| String::from_utf8_lossy(&o[..o.len().min(200)]).contains("/Subtype /Image")).unwrap();
            let head = String::from_utf8_lossy(&img.1[..300]).into_owned();
            let (wpx, hpx) = l.size_px(ppi);
            assert!(head.contains(&format!("/Width {wpx} /Height {hpx}")), "{head}");
            assert!(head.contains(&format!("/BitsPerComponent {}", if bits16 { 16 } else { 8 })));
            assert!(head.contains("/ColorSpace [/ICCBased 2 0 R]"));
            // the image stream is exactly the raster
            let raster = r.full();
            let expect: Vec<u8> = if bits16 { raster.data.iter().flat_map(|v| v.to_be_bytes()).collect() } else { raster.data.iter().map(|&v| to8(v)).collect() };
            assert_eq!(stream_of(&img.1), expect);
            // the profile goes in unchanged
            let profile = objs.iter().find(|(_, o)| String::from_utf8_lossy(&o[..o.len().min(100)]).contains("/N 3")).unwrap();
            assert_eq!(stream_of(&profile.1), icc());
            assert!(text.iter().any(|t| t.contains("/Type /Pages") && t.contains("/Count 2")));
        }
    }

    #[test]
    fn tiff_header_and_pixels() {
        let (_, r) = page(120.0);
        let mut c = io::Cursor::new(Vec::new());
        write_tiff(&mut c, &r, &icc(), 120, true).unwrap();
        let tif = c.into_inner();
        let (w, h) = r.size();
        let data_len = w * h * 6;
        assert_eq!(&tif[..4], b"II*\0");
        let raster = r.full();
        let tail: Vec<u8> = raster.data.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(&tif[tif.len() - data_len..], &tail[..]);
        // the profile is in the file unchanged
        assert!(tif.windows(icc().len()).any(|win| win == icc().as_slice()));
    }

    #[test]
    fn jpeg_is_written() {
        let (_, r) = page(100.0);
        let mut v = Vec::new();
        write_jpeg(&mut v, &r, &icc(), 100, 90).unwrap();
        assert_eq!(&v[..2], &[0xFF, 0xD8]);
        // JFIF density 100 ppi
        let jfif = v.windows(5).position(|w| w == b"JFIF\0").unwrap();
        assert_eq!(v[jfif + 7], 1, "unit inch");
        assert_eq!(u16::from_be_bytes([v[jfif + 8], v[jfif + 9]]), 100);
    }

    #[test]
    fn numbers_in_points() {
        assert_eq!(num(210.0 * PT_PER_MM), "595.2756");
        assert_eq!(num(72.0), "72");
        assert_eq!(num(483.0 * PT_PER_MM), "1369.1339");
    }
}
