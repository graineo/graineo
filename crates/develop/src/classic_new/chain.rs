//! Classic chain: the whole run from the raw values to the linear working codes, in this order
//! (sensor correction, defective pixels,
//! highlight neutralisation, speckle removal; then either the range-based reconstruction from ISO
//! 400 on, or chroma smoothing, colour, log-ratio reconstruction and the large recursive chroma
//! smoothing below; finally the active area and the detail signal).
//!
//! Output stage (tone curve, colour adjustment, sharpening, output matrix): not part of this run,
//! because it depends on the user's settings and is applied at every rendering. The linear codes of
//! [`run`] go to `quattro::classic_chain`; the output stage ([`super::output`]) is built there per
//! colour mode and applied in the rendering (`lib.rs`), with [`super::conversion`] between the
//! pipeline's camera values and the codes.
//!
//! The sensor correction, defective pixels, first-layer sharpening, both chroma smoothings, the
//! colour coring and the large recursive chroma smoothing follow dcraw ([`super::dcraw`]); the
//! highlight step is an own form.

use std::path::Path;

use x3f::{Camf, PropList, X3fFile};

use super::calibration::{self, ChromaKind, Data, Input, Calibration};
use super::{color, dcraw, detail, edge, region};
use super::meta::{Codes, Meta};

/// Writes an intermediate result (`GRAINEO_CLASSIC_DUMP`, little-endian `f32`, one file per step;
/// the names are kept stable for comparisons between versions).
fn dump(dir: Option<&Path>, name: &str, data: &[i32]) {
    let Some(dir) = dir else { return };
    let _ = std::fs::create_dir_all(dir);
    let mut b = Vec::with_capacity(data.len() * 4);
    for v in data {
        b.extend_from_slice(&(*v as f32).to_le_bytes());
    }
    let _ = std::fs::write(dir.join(format!("{name}.bin")), b);
}

/// Time of each step on stderr (`GRAINEO_CLASSIC_TIMING`, for measurements).
struct Clock(Option<std::time::Instant>);
impl Clock {
    fn new() -> Self {
        Clock(std::env::var_os("GRAINEO_CLASSIC_TIMING").map(|_| std::time::Instant::now()))
    }
    fn step(&mut self, name: &str) {
        if let Some(t) = self.0.as_mut() {
            eprintln!("classic {name}: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
            *t = std::time::Instant::now();
        }
    }
}

/// Raw values as interleaved codes (`w × h × 3`): TRUE coding (SD14 and newer) or the older Huffman
/// coding with difference table (SD9/SD10, signed 16-bit sums).
pub(crate) fn raw_values(file: &X3fFile) -> Result<(Vec<i32>, usize, usize), String> {
    let (r, sign) = match file.decode_raw() {
        Ok(r) => (r, false),
        Err(e) => (file.decode_raw_format6().map_err(|e6| format!("Rohdaten nicht lesbar ({e}; {e6})"))?, true),
    };
    let n = r.columns * r.rows;
    let mut v = vec![0i32; n * 3];
    for i in 0..n {
        for c in 0..3 {
            let x = r.planes[c][i];
            v[i * 3 + c] = if sign { i32::from(x as i16) } else { i32::from(x) };
        }
    }
    Ok((v, r.columns, r.rows))
}

/// Runs the chain on the raw values of `file` for white balance `wb` and colour mode `mode`;
/// `rotation` is the image's rotation in degrees (only the detail filter's axis order depends on it).
pub fn run(file: &X3fFile, camf: &Camf, props: &PropList, wb: &str, mode: &str, rotation: u32) -> Result<Codes, String> {
    let m = Meta { camf, props, adobe: file.exif().and_then(|e| e.color_space) == Some(0xFFFF) };
    flow(file, &m, wb, mode, rotation)
}

/// The chain on one file.
fn flow(file: &X3fFile, m: &Meta, wb: &str, mode: &str, rotation: u32) -> Result<Codes, String> {
    let dumpdir = std::env::var_os("GRAINEO_CLASSIC_DUMP").map(std::path::PathBuf::from);
    let dir = dumpdir.as_deref();
    let d = Data { camf: m.camf };
    let mut clock = Clock::new();
    let (mut img, w, h) = raw_values(file)?;
    clock.step("raw values");
    dump(dir, "sensor.layers", &img);
    let e = |gray| Input { camf: m.camf, props: m.props, wb, adobe: m.adobe, auto_neutral: None, gray_mean: gray };
    let k0 = calibration::calc(&e(None))?;
    // sensor correction (also keeps the grey mean for incandescent light at high ISO)
    super::correction_new(&mut img, w, h, m, wb)?;
    clock.step("sensor correction");
    dump(dir, "calibration.layers", &img);
    super::repair(&mut img, w, h, m);
    clock.step("defects");
    dump(dir, "repair.layers", &img);
    super::highlights_new(&mut img, w, h, m, wb)?;
    clock.step("highlights");
    dump(dir, "highlight.layers", &img);
    // (the step decides the way itself: long exposure or exposure over the camera's limit)
    super::despeckle(&mut img, w, h, m, k0.iso.bulb);
    clock.step("despeckle");
    dump(dir, "despeckle.layers", &img);
    // calibration of the later steps (with the image's automatic white balance, if any)
    let k1 = super::cal(m, wb)?;
    let codes = if k1.iso.high {
        let k = match super::gray_mean(&k1)? {
            Some(g) => calibration::calc(&Input { gray_mean: Some(g), auto_neutral: super::AUTO_NEUTRAL.get(), ..e(None) })?,
            None => k1,
        };
        let c = if k.skip & 0x800000 != 0 {
            // variant without noise reduction: black level and matrix only
            region::without_noise_reduction(&img, &d, &k, mode, w, h)?
        } else {
            let p = region::parameter(&d, &k, mode)?;
            region::run(&img, w, h, &p)
        };
        dump(dir, "reconstruction.codes", &c);
        c
    } else {
        low(img, w, h, &d, &k1, mode, dir)?
    };
    clock.step("reconstruction");
    let r = end(codes, w, h, m, &d, rotation, dir);
    clock.step("detail");
    r
}

/// Below ISO 400: chroma smoothing, colour, log-ratio reconstruction, large recursive chroma
/// smoothing.
fn low(mut img: Vec<i32>, w: usize, h: usize, d: &Data, k: &Calibration, mode: &str, dir: Option<&Path>) -> Result<Vec<i32>, String> {
    for (file_kind, name) in [(ChromaKind::Small, "chroma.small-layers"), (ChromaKind::Large, "chroma.layers")] {
        if let Some(p) = k.chroma(d.camf, file_kind) {
            // the curve and the two smoothings after dcraw
            let curve = dcraw::Curve::new(p.amp[0], p.amp[0]);
            match file_kind {
                ChromaKind::Small => dcraw::small(&mut img, w, h, &curve, p.shift),
                ChromaKind::Large => dcraw::large(&mut img, w, h, &curve, p.spatial, p.shift),
            }
            dump(dir, name, &img);
        }
    }
    let mut p = color::parameter(d, k, mode)?;
    if !d.present("ColorDQCamRGB") && d.present("ColorDQ") && k.skip & 0x20000 == 0 {
        // like dcraw, the camera's `ColorDQ` when it lists no `ColorDQCamRGB`, at half strength (own
        // measurement on the SD9 reference export, mean ΔE00: × 1 → 1.53, × 0.75 → 1.40, × 0.5 →
        // 1.15, × 0.33 → 1.18; standard amplitude 1.18)
        let dq = d.triple("ColorDQ", [0.0; 3]);
        let g = k.amplification.gain;
        p.amp = std::array::from_fn(|c| 0.5 * dq[c] * k.iso.bulb_factor * ((g[c] * f64::from(k.iso.factor1)) * k.statistics));
    }
    let f = dcraw::color(&img, w, h, &p);
    drop(img);
    dump(dir, "color.codes", &f.codes);
    dump(dir, "color.corrected-layers", &f.layers);
    let mut codes = f.codes;
    if p.edge {
        let kp = edge::parameter(d, k, f.green_sum);
        edge::run(&mut codes, &f.layers, w, h, &kp);
        dump(dir, "reconstruction.codes", &codes);
    }
    drop(f.layers);
    // large recursive chroma smoothing (very high ChromaBlur, SD9/SD10)
    if (d.number("ChromaBlur") as i32) as u32 > 100 && !k.color.degenerate && k.skip & 0x200000 == 0 {
        let shift = if d.present("ChromaSumShift") { d.integer("ChromaSumShift") & 0xffff } else { 3 };
        let bulb = k.iso.bulb_factor;
        let g = k.amplification.gain;
        let dq = d.triple("ChromaDQ", [0.0; 3]);
        let scale = f64::from(k.iso.factor + k.iso.factor) / 3.0;
        let amp: [f64; 3] = std::array::from_fn(|c| ((dq[c] * bulb) * g[c] * scale) / 2.0);
        let spatial = (f64::from(d.number("SpatialDQ") as i32) * bulb) as i32;
        let reg = ((spatial.wrapping_mul((k.iso.factor * 10.0) as i32)) as u32 >> 1) as i32;
        if amp.iter().all(|a| *a > 0.0) {
            dcraw::recursive(&mut codes, w, h, &dcraw::curves(amp), reg, shift);
            dump(dir, "chroma.rgb-codes", &codes);
        }
    }
    Ok(codes)
}

/// Active area (keep area of the stored image minus the active area's corner) and the detail
/// signal to the linear codes.
fn end(codes: Vec<i32>, w: usize, h: usize, m: &Meta, d: &Data, rotation: u32, dir: Option<&Path>) -> Result<Codes, String> {
    let keep = d.values("KeepImageArea").filter(|v| v.len() >= 4).map_or([0.0; 4], |v| [v[0], v[1], v[2], v[3]]);
    let act = d.values("ActiveImageArea").filter(|v| v.len() >= 4).ok_or("ActiveImageArea fehlt")?;
    let crop = [(act[0] - keep[0]) as usize, (act[1] - keep[1]) as usize, (act[2] - keep[0] + 1.0) as usize, (act[3] - keep[1] + 1.0) as usize];
    if crop[0] >= crop[2] || crop[1] >= crop[3] || crop[2] > w || crop[3] > h {
        return Err("Bildbereich ungültig".into());
    }
    let cross = matches!(rotation % 360, 90 | 270);
    let skip = d.integer("SkipStepFlags");
    let lin = detail::run(&codes, w, h, crop, detail::green_parameter(d, m.capture_iso(), skip), cross);
    dump(dir, "linear.codes", &lin);
    Ok(Codes { width: w, height: h, crop, data: lin })
}
