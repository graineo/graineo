//! "Sensor layers (unprocessed)" export: the three Foveon layers exactly as stored in the X3F
//! (no black subtraction, no gain, no noise reduction), as a 16-bit TIFF with three channels and a
//! description of the data (black level, saturation, image depth, active area, colour matrix) in
//! TIFF tags and XMP. Quattro: the X3F stores the top layer at four times the resolution; it is
//! written as a second file `…_top.tif` at full resolution next to the three-layer file.

use std::path::{Path, PathBuf};

use crate::calib::Calibration;
use crate::tiffw::{self, Compression, Ifd, Value::*};

/// Description of a layer image for tags and XMP.
struct Describe<'a> {
    cal: &'a Calibration,
    /// image depth in bits (12 Merrill, 14 Quattro/Classic)
    depth: u32,
    black: Option<[f64; 3]>,
    channels: usize,
    note: &'a str,
}

fn xmp(d: &Describe) -> String {
    let m = crate::camera_to_xyz(d.cal, None);
    let list = |v: &[f64]| v.iter().map(|x| format!("{x:.6}")).collect::<Vec<_>>().join(" ");
    let black = d.black.map(|b| list(&b)).unwrap_or_default();
    let [x0, y0, x1, y1] = d.cal.active;
    format!(
        concat!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
            "<rdf:Description rdf:about=\"\" xmlns:graineo=\"https://graineo.github.io/ns/1.0/\"\n",
            " graineo:RawLayers=\"{note}\"\n",
            " graineo:RawChannels=\"{ch}\"\n",
            " graineo:ImageDepth=\"{depth}\"\n",
            " graineo:RawSaturation=\"{sat}\"\n",
            " graineo:RawBlack=\"{black}\"\n",
            " graineo:ActiveArea=\"{x0} {y0} {x1} {y1}\"\n",
            " graineo:WhiteBalance=\"{wb}\"\n",
            " graineo:WhiteBalanceGains=\"{gains}\"\n",
            " graineo:CameraToXYZ=\"{m}\"/>\n",
            "</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>"
        ),
        note = d.note,
        ch = d.channels,
        depth = d.depth,
        sat = (1u32 << d.depth) - 1,
        black = black,
        wb = d.cal.wb_name,
        gains = list(&d.cal.wb_gain),
        m = list(&m.0),
        x0 = x0,
        y0 = y0,
        x1 = x1,
        y1 = y1,
    )
}

fn write_tiff(path: &Path, width: usize, height: usize, channels: usize, data: &[u16], d: &Describe, c: Compression) -> std::io::Result<()> {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (strips, mut tags) = tiffw::strips(&bytes, width, channels, tiffw::Samples::U16, c);
    tags.extend([
        (256, Long(vec![width as u32])),
        (257, Long(vec![height as u32])),
        (258, Short(vec![16; channels])),
        (262, Short(vec![if channels == 3 { 2 } else { 1 }])),
        (270, Ascii(format!("{} – {}", d.cal.camera, d.note))),
        (271, Ascii("SIGMA".into())),
        (272, Ascii(d.cal.camera.clone())),
        (274, Short(vec![1])),
        (277, Short(vec![channels as u16])),
        (284, Short(vec![1])),
        (305, Ascii(crate::export::SOFTWARE.into())),
        (339, Short(vec![1; channels])),
        (700, Byte(xmp(d).into_bytes())),
    ]);
    // the full sensor range with the black-level reference as the maximum value range
    tags.push((281, Short(vec![((1u32 << d.depth) - 1).min(65535) as u16; channels]))); // MaxSampleValue
    let ifd = Ifd { tags, strips, ..Default::default() };
    tiffw::write_ifd(std::io::BufWriter::new(std::fs::File::create(path)?), ifd)
}

/// Mean of the dark shield (black level reference) per layer, if the CAMF describes it.
fn dark_mean(file: &x3f::X3fFile, planes: &[Vec<u16>; 3], columns: usize) -> Option<[f64; 3]> {
    let r = file.camf().ok()?.matrix("DarkShieldTop").ok()?.as_f64();
    let [x0, y0, x1, y1] = [r.first()?, r.get(1)?, r.get(2)?, r.get(3)?].map(|v| *v as usize);
    let rows = planes[0].len() / columns.max(1);
    if x1 >= columns || y1 >= rows || x0 > x1 || y0 > y1 {
        return None;
    }
    Some(std::array::from_fn(|c| {
        let (mut sum, mut n) = (0f64, 0f64);
        for y in y0..=y1 {
            for x in x0..=x1 {
                sum += planes[c][y * columns + x] as f64;
                n += 1.0;
            }
        }
        sum / n.max(1.0)
    }))
}

/// Writes the sensor layers of `x3f` to `path` (and for Quattro the top layer to `…_top.tif`).
/// Returns all written files.
pub fn write(x3f: &Path, cal: &Calibration, path: &Path, c: Compression) -> std::io::Result<Vec<PathBuf>> {
    let io = |e: x3f::Error| std::io::Error::other(e.to_string());
    let file = x3f::X3fFile::open(x3f).map_err(io)?;
    if !file.is_quattro() {
        let raw = file.decode_raw().map_err(io)?;
        let black = dark_mean(&file, &raw.planes, raw.columns);
        let depth = if cal.is_classic() { 14 } else { (cal.pixel_saturation + 1.0).log2().round().clamp(8.0, 16.0) as u32 };
        let d = Describe { cal, depth, black, channels: 3, note: "Foveon sensor layers, unprocessed (channels: R = bottom, G = middle, B = top layer)" };
        write_tiff(path, raw.columns, raw.rows, 3, &raw.interleaved(), &d, c)?;
        return Ok(vec![path.to_path_buf()]);
    }
    // Quattro: decoding via x3fuse-core (unprocessed, no crop, no repair, no gain, no noise reduction)
    let err = |e: x3f_core::Error| std::io::Error::other(e.to_string());
    let mut r = x3f_core::Reader::open(x3f).map_err(err)?;
    r.load_camf().map_err(err)?;
    r.load_property_list().map_err(err)?;
    r.load_raw().map_err(err)?;
    let opts = |enc| x3f_core::ProcessOptions { color_encoding: enc, crop: false, fix_bad: false, apply_sgain: Some(false), denoise_intensity: 0, ..Default::default() };
    let pack = |img: &x3f_core::Image, ch: usize| -> Vec<u16> {
        let mut v = Vec::with_capacity(img.rows as usize * img.columns as usize * ch);
        for y in 0..img.rows {
            let row = img.row(y);
            for x in 0..img.columns as usize {
                for k in 0..ch {
                    v.push(row[x * img.channels as usize + k.min(img.channels as usize - 1)]);
                }
            }
        }
        v
    };
    let low = r.get_image(&opts(x3f_core::ColorEncoding::Unprocessed)).map_err(err)?;
    let top = r.get_image(&opts(x3f_core::ColorEncoding::Qtop)).map_err(err)?;
    let (w, h) = (low.columns as usize, low.rows as usize);
    let top_px = pack(&top, 1);
    let (tw, th) = (top.columns as usize, top.rows as usize);
    // three layers at the resolution of the lower layers: the top layer as the mean of its 2×2 block
    let mut all = pack(&low, 3);
    for y in 0..h {
        for x in 0..w {
            let (mut sum, mut n) = (0u32, 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (tx, ty) = (2 * x + dx, 2 * y + dy);
                if tx < tw && ty < th {
                    sum += top_px[ty * tw + tx] as u32;
                    n += 1;
                }
            }
            all[(y * w + x) * 3 + 2] = if n > 0 { ((sum + n / 2) / n) as u16 } else { 0 };
        }
    }
    let d = Describe { cal, depth: 14, black: Some(cal.black), channels: 3, note: "Foveon Quattro sensor layers, unprocessed, at the resolution of the lower layers (channels: R = bottom, G = middle, B = top layer as mean of 2x2); unchanged full-resolution top layer in the _top file" };
    write_tiff(path, w, h, 3, &all, &d, c)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let top_path = path.with_file_name(format!("{stem}_top.tif"));
    let d = Describe { cal, depth: 14, black: None, channels: 1, note: "Foveon Quattro top layer, unprocessed, full resolution" };
    write_tiff(&top_path, tw, th, 1, &top_px, &d, c)?;
    Ok(vec![path.to_path_buf(), top_path])
}
