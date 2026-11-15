//! `graineo-dev qa <output_folder> <files|folders …>` – automatic quality check.
//!
//! Per file: header/model/format, decode time, development with default settings via the
//! SPP path (`Session::render_full` with the x3fuse-core base, GPU if available) and our own
//! pipeline (`render_full_own`), metrics (PSNR own ↔ SPP overall and per brightness zone,
//! comparison with the embedded camera JPEG: exposure in stops, color cast in
//! Lab a*/b*, clipping), traffic light, contact image (SPP | own | camera) and as a report
//! `outcome.json` + `report.html`.

use std::fmt::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use develop::{Rgb16, Session, Settings};
use serde_json::{json, Value};

/// Long edge of the downscaled images (contact image fields and metrics against the camera JPEG).
const EDGE: usize = 1200;

// Traffic light thresholds (green, yellow).
/// PSNR own ↔ SPP overall in dB (higher = better).
const PSNR_TOTAL: (f64, f64) = (30.0, 25.0);
/// PSNR own ↔ SPP in the worst brightness zone (≥ 2 % of pixels).
const PSNR_ZONE: (f64, f64) = (27.0, 22.0);
/// |exposure deviation| against the camera JPEG in stops.
const DEV: (f64, f64) = (0.3, 0.7);
/// Mean color cast against the camera JPEG (magnitude of Δa*/Δb* averaged over the midtones).
const DAB: (f64, f64) = (3.0, 6.0);
/// Additional clipping compared to the camera JPEG in percentage points.
const CLIP: (f64, f64) = (0.5, 2.0);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Verdict {
    Green,
    Yellow,
    Red,
    Gray,
}

impl Verdict {
    fn key(self) -> &'static str {
        match self {
            Verdict::Green => "gruen",
            Verdict::Yellow => "gelb",
            Verdict::Red => "rot",
            Verdict::Gray => "grau",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Verdict::Green => "green",
            Verdict::Yellow => "yellow",
            Verdict::Red => "red",
            Verdict::Gray => "not supported",
        }
    }
}

/// Lower is better.
fn grade_low(v: f64, (g, y): (f64, f64)) -> Verdict {
    if v < g {
        Verdict::Green
    } else if v < y {
        Verdict::Yellow
    } else {
        Verdict::Red
    }
}

/// Higher is better.
fn grade_high(v: f64, (g, y): (f64, f64)) -> Verdict {
    if v >= g {
        Verdict::Green
    } else if v >= y {
        Verdict::Yellow
    } else {
        Verdict::Red
    }
}

// ---------------------------------------------------------------- image helpers

/// Downscaled image, linear, sRGB primaries (D65).
#[derive(Clone)]
struct Small {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

fn lut16() -> &'static [f32] {
    static L: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    L.get_or_init(|| (0..65536).map(|i| srgb_decode(i as f32 / 65535.0)).collect())
}

/// Area average (in linear light) to `dw`×`dh`.
fn resize<T: Copy + Into<usize>>(src: &[T], sw: usize, sh: usize, lut: &[f32], dw: usize, dh: usize) -> Small {
    let mut data = vec![0f32; dw * dh * 3];
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let mut s = [0f32; 3];
            for y in y0..y1 {
                let row = &src[(y * sw + x0) * 3..(y * sw + x1) * 3];
                for p in row.chunks_exact(3) {
                    s[0] += lut[p[0].into()];
                    s[1] += lut[p[1].into()];
                    s[2] += lut[p[2].into()];
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            data[(dy * dw + dx) * 3..(dy * dw + dx) * 3 + 3].copy_from_slice(&[s[0] / n, s[1] / n, s[2] / n]);
        }
    }
    Small { w: dw, h: dh, data }
}

/// Target size with long edge [`EDGE`].
fn target(w: usize, h: usize) -> (usize, usize) {
    if w >= h {
        (EDGE.min(w), (EDGE.min(w) * h + w / 2) / w)
    } else {
        ((EDGE.min(h) * w + h / 2) / h, EDGE.min(h))
    }
}

/// `q` quarter turns clockwise.
fn rotate(img: &Small, q: u8) -> Small {
    let mut cur = img.clone();
    for _ in 0..q % 4 {
        let (w, h) = (cur.w, cur.h);
        let mut data = vec![0f32; w * h * 3];
        // new: width h, height w; new(x, y) = old(y, h−1−x)
        for y in 0..w {
            for x in 0..h {
                let s = ((h - 1 - x) * w + y) * 3;
                let d = (y * h + x) * 3;
                data[d..d + 3].copy_from_slice(&cur.data[s..s + 3]);
            }
        }
        cur = Small { w: h, h: w, data };
    }
    cur
}

/// Fraction of pixels where at least one channel is ≥ `lim`.
fn clip_fraction<T: Copy + Into<usize>>(data: &[T], lim: usize) -> f64 {
    let n = data.len() / 3;
    let c = data.chunks_exact(3).filter(|p| p.iter().any(|&v| v.into() >= lim)).count();
    c as f64 / n.max(1) as f64
}

const SRGB_TO_XYZ: [f32; 9] = [0.4124564, 0.3575761, 0.1804375, 0.2126729, 0.7151522, 0.0721750, 0.0193339, 0.1191920, 0.9503041];
const ADOBE_TO_XYZ: [f32; 9] = [0.5767309, 0.1855540, 0.1881852, 0.2973769, 0.6273491, 0.0752741, 0.0270343, 0.0706872, 0.9911085];
const XYZ_TO_SRGB: [f32; 9] = [3.2404542, -1.5371385, -0.4985314, -0.9692660, 1.8760108, 0.0415560, 0.0556434, -0.2040259, 1.0572252];

fn mul(m: &[f32; 9], v: [f32; 3]) -> [f32; 3] {
    [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
}

fn lab(rgb: [f32; 3]) -> [f32; 3] {
    let xyz = mul(&SRGB_TO_XYZ, rgb);
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(xyz[0] / 0.95047), f(xyz[1]), f(xyz[2] / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn luminance(p: &[f32]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

fn median(v: &mut [f32]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    Some(*m as f64)
}

/// Color space of the camera JPEG from EXIF `ColorSpace` (0xA001): 1 = sRGB, otherwise (0xFFFF) Adobe RGB.
fn jpeg_is_adobe(jpg: &[u8]) -> Option<bool> {
    let head = &jpg[..jpg.len().min(65536)];
    for (pat, be) in [(&[0x01u8, 0xA0, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00][..], false), (&[0xA0u8, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01][..], true)] {
        if let Some(i) = head.windows(8).position(|w| w == pat) {
            let b = head.get(i + 8..i + 10)?;
            let v = if be { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) };
            return Some(v != 1);
        }
    }
    None
}

/// Camera model from the JPEG's EXIF block (IFD0, tag 0x0110 "Model").
fn exif_model(jpg: &[u8]) -> Option<String> {
    let head = &jpg[..jpg.len().min(65536)];
    let t = head.windows(6).position(|w| w == b"Exif\0\0")? + 6;
    let tiff = &head[t..];
    let be = tiff.get(0..2)? == b"MM";
    let u16_at = |o: usize| tiff.get(o..o + 2).map(|b| if be { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) });
    let u32_at = |o: usize| tiff.get(o..o + 4).map(|b| {
        let a = [b[0], b[1], b[2], b[3]];
        if be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
    });
    let ifd = u32_at(4)? as usize;
    for i in 0..u16_at(ifd)? as usize {
        let e = ifd + 2 + i * 12;
        if u16_at(e)? == 0x0110 {
            let n = u32_at(e + 4)? as usize;
            let off = if n <= 4 { e + 8 } else { u32_at(e + 8)? as usize };
            let raw = tiff.get(off..off + n)?;
            let s = String::from_utf8_lossy(raw).trim_matches(char::from(0)).trim().to_string();
            return (!s.is_empty()).then_some(s);
        }
    }
    None
}

// ---------------------------------------------------------------- metrics

#[derive(Default, Clone)]
struct Zone {
    name: &'static str,
    share: f64,
    psnr: f64,
    bias: [f64; 3],
}

#[derive(Default, Clone)]
struct Psnr {
    total: f64,
    bias: [f64; 3],
    zones: Vec<Zone>,
}

/// PSNR (8-bit reference) own ↔ SPP, overall and by brightness of the SPP output.
fn psnr(ours: &Rgb16, reference: &Rgb16) -> Psnr {
    const NAMES: [&str; 3] = ["shadows", "midtones", "highlights"];
    let mut se = 0f64;
    let mut bias = [0f64; 3];
    let mut zse = [0f64; 3];
    let mut zn = [0f64; 3];
    let mut zb = [[0f64; 3]; 3];
    for (a, b) in ours.data.chunks_exact(3).zip(reference.data.chunks_exact(3)) {
        let lum = (b[0] as f64 * 0.2126 + b[1] as f64 * 0.7152 + b[2] as f64 * 0.0722) / 65535.0;
        let z = if lum < 0.25 { 0 } else if lum < 0.75 { 1 } else { 2 };
        for c in 0..3 {
            let d = (a[c] as f64 - b[c] as f64) / 257.0;
            se += d * d;
            zse[z] += d * d;
            bias[c] += d;
            zb[z][c] += d;
        }
        zn[z] += 1.0;
    }
    let n = (ours.data.len() / 3) as f64;
    let p = |se: f64, n: f64| if se <= 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / (se / (n * 3.0))).log10() };
    Psnr {
        total: p(se, n),
        bias: bias.map(|b| b / n),
        zones: (0..3)
            .filter(|&z| zn[z] > 0.0)
            .map(|z| Zone { name: NAMES[z], share: zn[z] / n, psnr: p(zse[z], zn[z]), bias: zb[z].map(|b| b / zn[z]) })
            .collect(),
    }
}

#[derive(Default, Clone)]
struct JpegCmp {
    /// median log2(Y_own / Y_jpeg) over valid pixels
    d_ev: f64,
    /// the same per zone (by L* of the camera JPEG: < 35, 35–70, > 70)
    d_ev_zones: [Option<f64>; 3],
    d_a: f64,
    d_b: f64,
    d_ab: f64,
    /// mean C* own / mean C* JPEG (saturation)
    chroma_ratio: f64,
    de_median: f64,
    /// cast on (according to the camera JPEG) neutral areas: Δa*, Δb*, fraction of pixels
    gray: Option<(f64, f64, f64)>,
    clip: f64,
    clip_jpeg: f64,
}

fn compare_jpeg(ours: &Small, jpg: &Small, clip: f64, clip_jpeg: f64) -> JpegCmp {
    let mut ev = Vec::new();
    let mut evz: [Vec<f32>; 3] = Default::default();
    let mut de = Vec::new();
    let (mut sa, mut sb, mut n) = (0f64, 0f64, 0f64);
    let (mut co, mut cj) = (0f64, 0f64);
    let (mut ga, mut gb, mut gn) = (0f64, 0f64, 0f64);
    for (o, j) in ours.data.chunks_exact(3).zip(jpg.data.chunks_exact(3)) {
        let (yo, yj) = (luminance(o), luminance(j));
        let clipped = o.iter().chain(j.iter()).any(|&v| v >= 0.95);
        if clipped || yj < 0.003 || yo < 1e-4 {
            continue;
        }
        let d = (yo / yj).log2();
        ev.push(d);
        let (lo, lj) = (lab([o[0], o[1], o[2]]), lab([j[0], j[1], j[2]]));
        let z = if lj[0] < 35.0 { 0 } else if lj[0] <= 70.0 { 1 } else { 2 };
        evz[z].push(d);
        de.push(((lo[0] - lj[0]).powi(2) + (lo[1] - lj[1]).powi(2) + (lo[2] - lj[2]).powi(2)).sqrt());
        if (20.0..=90.0).contains(&lj[0]) {
            sa += (lo[1] - lj[1]) as f64;
            sb += (lo[2] - lj[2]) as f64;
            co += lo[1].hypot(lo[2]) as f64;
            cj += lj[1].hypot(lj[2]) as f64;
            n += 1.0;
            if lj[1].hypot(lj[2]) < 6.0 && (30.0..=85.0).contains(&lj[0]) {
                ga += (lo[1] - lj[1]) as f64;
                gb += (lo[2] - lj[2]) as f64;
                gn += 1.0;
            }
        }
    }
    let total = (ours.w * ours.h) as f64;
    let n1 = n.max(1.0);
    let (d_a, d_b) = (sa / n1, sb / n1);
    JpegCmp {
        d_ev: median(&mut ev).unwrap_or(f64::NAN),
        d_ev_zones: [0, 1, 2].map(|z| if evz[z].len() as f64 > total * 0.02 { median(&mut evz[z]) } else { None }),
        d_a,
        d_b,
        d_ab: d_a.hypot(d_b),
        chroma_ratio: if cj > 0.0 { co / cj } else { f64::NAN },
        de_median: median(&mut de).unwrap_or(f64::NAN),
        gray: (gn > total * 0.01).then(|| (ga / gn, gb / gn, gn / total)),
        clip,
        clip_jpeg,
    }
}

// ---------------------------------------------------------------- result per file

#[derive(Default)]
struct PathRes {
    ok: bool,
    seconds: f64,
    note: Option<String>,
    gpu: Option<bool>,
    jpeg: Option<JpegCmp>,
}

#[derive(Default)]
struct FileRes {
    path: String,
    name: String,
    group: String,
    model: String,
    version: String,
    format: String,
    size: String,
    iso: String,
    mode: String,
    wb: String,
    adobe: Option<bool>,
    orientation: u8,
    t_decode: Option<f64>,
    t_open: Option<f64>,
    supported: bool,
    error: Option<String>,
    spp: PathRes,
    own: PathRes,
    psnr: Option<Psnr>,
    verdict: Option<Verdict>,
    reasons: Vec<(Verdict, String)>,
    sheet: Option<String>,
    seconds: f64,
}

fn fmt_ev(v: f64) -> String {
    format!("{v:+.2}")
}

impl FileRes {
    /// Traffic light from the individual metrics (the worst wins) with reasons.
    fn grade(&mut self) {
        if !self.supported {
            self.verdict = Some(Verdict::Gray);
            return;
        }
        if let Some(e) = &self.error {
            self.reasons.push((Verdict::Red, format!("error: {e}")));
            self.verdict = Some(Verdict::Red);
            return;
        }
        let mut r = Vec::new();
        if let Some(p) = &self.psnr {
            r.push((grade_high(p.total, PSNR_TOTAL), format!("PSNR own↔SPP {:.1} dB", p.total)));
            if let Some(z) = p.zones.iter().filter(|z| z.share >= 0.02).min_by(|a, b| a.psnr.total_cmp(&b.psnr)) {
                r.push((grade_high(z.psnr, PSNR_ZONE), format!("worst zone {} {:.1} dB", z.name, z.psnr)));
            }
        }
        if !self.spp.ok {
            r.push((Verdict::Yellow, format!("SPP path not available: {}", self.spp.note.clone().unwrap_or_default())));
        }
        if !self.own.ok {
            r.push((Verdict::Red, format!("own pipeline: {}", self.own.note.clone().unwrap_or_default())));
        }
        // camera JPEG comparison for the main path (SPP, otherwise own)
        let (label, main) = if self.spp.ok { ("SPP", &self.spp) } else { ("own", &self.own) };
        if let Some(j) = &main.jpeg {
            r.push((grade_low(j.d_ev.abs(), DEV), format!("{label}↔camera ΔEV {}", fmt_ev(j.d_ev))));
            r.push((grade_low(j.d_ab, DAB), format!("{label}↔camera color cast Δa* {:+.1} Δb* {:+.1}", j.d_a, j.d_b)));
            let extra = (j.clip - j.clip_jpeg) * 100.0;
            r.push((grade_low(extra, CLIP), format!("{label} clipping {:.2} % (camera {:.2} %)", j.clip * 100.0, j.clip_jpeg * 100.0)));
        } else if self.spp.ok || self.own.ok {
            r.push((Verdict::Yellow, "no camera JPEG for comparison".into()));
        }
        self.verdict = Some(r.iter().map(|x| x.0).max().unwrap_or(Verdict::Yellow));
        self.reasons = r;
    }

    fn to_json(&self) -> Value {
        let jc = |j: &Option<JpegCmp>| {
            j.as_ref().map(|j| {
                json!({
                    "delta_ev": j.d_ev, "delta_ev_shadows": j.d_ev_zones[0], "delta_ev_center": j.d_ev_zones[1],
                    "delta_ev_highlights": j.d_ev_zones[2], "delta_a": j.d_a, "delta_b": j.d_b, "delta_ab": j.d_ab,
                    "chroma_ratio": j.chroma_ratio, "delta_e_median": j.de_median,
                    "gray_cast": j.gray.map(|(a, b, s)| json!({"delta_a": a, "delta_b": b, "fraction": s})),
                    "clipping": j.clip, "clipping_camera": j.clip_jpeg,
                })
            })
        };
        let pr = |p: &PathRes| json!({"ok": p.ok, "seconds": p.seconds, "hint": p.note, "gpu": p.gpu, "camera_jpeg": jc(&p.jpeg)});
        json!({
            "file": self.path, "name": self.name, "group": self.group, "model": self.model,
            "x3f_version": self.version, "format": self.format, "size": self.size, "iso": self.iso,
            "color_mode": self.mode, "white_balance": self.wb, "camera_adobe_rgb": self.adobe,
            "position_quarter": self.orientation, "supported": self.supported, "error": self.error,
            "decode_s": self.t_decode, "open_s": self.t_open, "total_s": self.seconds,
            "spp": pr(&self.spp), "own": pr(&self.own),
            "psnr_own_spp": self.psnr.as_ref().map(|p| json!({
                "total": p.total, "deviation_rgb": p.bias,
                "zones": p.zones.iter().map(|z| json!({"zone": z.name, "fraction": z.share, "psnr": z.psnr, "deviation_rgb": z.bias})).collect::<Vec<_>>(),
            })),
            "traffic_light": self.verdict.map(|a| a.key()),
            "reasons": self.reasons.iter().map(|(a, t)| json!({"traffic_light": a.key(), "text": t})).collect::<Vec<_>>(),
            "contact_image": self.sheet,
        })
    }
}

fn is_unsupported(e: &x3f::Error) -> bool {
    matches!(e, x3f::Error::Unsupported(_))
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into())
}

/// Checks one file; errors end up in `r.error`, panics are caught.
fn check_file(path: &Path, idx: usize, out: &Path, r: &mut FileRes) {
    let t_all = Instant::now();
    r.path = path.display().to_string();
    r.name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    r.supported = true;
    let res = std::panic::catch_unwind(AssertUnwindSafe(|| check_inner(path, idx, out, r)));
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            if r.error.is_none() {
                r.error = Some(format!("{e:#}"));
            }
        }
        Err(p) => r.error = Some(format!("panic: {}", panic_text(p))),
    }
    r.seconds = t_all.elapsed().as_secs_f64();
    r.grade();
}

fn check_inner(path: &Path, idx: usize, out: &Path, r: &mut FileRes) -> Result<()> {
    // 1. header, model, format, decode time
    let (jpeg_bytes, props_rotation) = {
        let file = x3f::X3fFile::open(path).context("open")?;
        r.version = format!("{}.{}", file.version >> 16, file.version & 0xffff);
        if let Ok(p) = file.properties() {
            r.model = p.get("CAMMODEL").unwrap_or("").to_string();
            r.iso = p.get("ISO").unwrap_or("").to_string();
        }
        let red = file
            .properties()
            .ok()
            .and_then(|p| p.get("ROTATION").and_then(|s| s.trim().parse::<i32>().ok()))
            .or_else(|| file.header_rotation().map(|d| d as i32))
            .unwrap_or(0);
        if let Ok((_, h)) = file.raw_entry() {
            let kind = if h.is_merrill_raw() {
                "Merrill-RAW"
            } else if h.is_classic_raw() {
                "Classic-RAW"
            } else {
                "RAW (not Merrill)"
            };
            r.format = format!("{kind}, type {} / format {}", h.image_type, h.format);
            r.size = format!("{}×{}", h.columns, h.rows);
        }
        // SD9/SD10: no JPEG, then the uncompressed preview (encoded as JPEG)
        let jpg = file.preview_jpeg().map(|b| b.to_vec()).or_else(|| {
            let (w, h, rgb) = x3f::read_embedded_plain(path, true)?;
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95).encode(&rgb, w, h, image::ExtendedColorType::Rgb8).ok()?;
            Some(out)
        });
        if r.model.is_empty() {
            // Quattro: no PROP section – model from the EXIF of the camera JPEG
            r.model = jpg.as_deref().and_then(exif_model).unwrap_or_default();
        }
        let t = Instant::now();
        // Quattro/Classic: the session decodes via x3fuse-core (our Merrill decoder declines)
        let quattro = file.raw_entry().is_ok_and(|(_, h)| !h.is_merrill_raw()) && develop::quattro::is_prepared(path);
        match if quattro { Err(x3f::Error::Unsupported(String::new())) } else { file.decode_raw() } {
            Err(_) if quattro => {}
            Ok(raw) => {
                r.t_decode = Some(t.elapsed().as_secs_f64());
                drop(raw);
            }
            Err(e) if is_unsupported(&e) => {
                r.supported = false;
                r.error = Some(e.to_string());
            }
            Err(e) => return Err(e).context("decoding RAW"),
        }
        (jpg, develop::geometry::quarters_from_degrees(red))
    };
    r.orientation = props_rotation;

    // camera JPEG (sensor orientation, EXIF orientation is deliberately not applied)
    let camera = match &jpeg_bytes {
        Some(b) => match image::load_from_memory_with_format(b, image::ImageFormat::Jpeg) {
            Ok(img) => {
                let rgb = img.to_rgb8();
                r.adobe = jpeg_is_adobe(b);
                Some((rgb.width() as usize, rgb.height() as usize, rgb.into_raw()))
            }
            Err(e) => {
                eprintln!("  camera JPEG not readable: {e}");
                None
            }
        },
        None => None,
    };
    let jpeg_small = |dw: usize, dh: usize, adobe: bool| -> Option<(Small, f64)> {
        let (w, h, data) = camera.as_ref()?;
        let clip = clip_fraction(data, 253);
        let mut s = if adobe {
            let lut: Vec<f32> = (0..256).map(|i| (i as f32 / 255.0).powf(563.0 / 256.0)).collect();
            resize(data, *w, *h, &lut, dw, dh)
        } else {
            let lut: Vec<f32> = (0..256).map(|i| srgb_decode(i as f32 / 255.0)).collect();
            resize(data, *w, *h, &lut, dw, dh)
        };
        if adobe {
            for p in s.data.chunks_exact_mut(3) {
                let v = mul(&XYZ_TO_SRGB, mul(&ADOBE_TO_XYZ, [p[0], p[1], p[2]]));
                p.copy_from_slice(&v);
            }
        }
        Some((s, clip))
    };

    if !r.supported {
        // show only the camera JPEG
        if let Some((w, h, _)) = &camera {
            let (dw, dh) = target(*w, *h);
            let cam = jpeg_small(dw, dh, r.adobe.unwrap_or(false)).map(|x| x.0);
            r.sheet = write_sheet(out, idx, &r.name, [None, None, cam.as_ref()], r.orientation)?;
        }
        return Ok(());
    }

    // 2. develop
    let t = Instant::now();
    let mut session = Session::open(path).map_err(|e| {
        if is_unsupported(&e) {
            r.supported = false;
        }
        anyhow::anyhow!("opening session: {e}")
    })?;
    r.t_open = Some(t.elapsed().as_secs_f64());
    let cal = session.cal.clone();
    r.model = if r.model.is_empty() { cal.camera.clone() } else { r.model.clone() };
    r.mode = cal.camera_mode.clone();
    r.wb = cal.wb_name.clone();
    r.orientation = cal.orientation;
    if r.iso.is_empty() {
        r.iso = format!("{:.0}", cal.iso);
    }
    let cam_adobe = cal.output_to_srgb.is_some();
    if let Some(a) = r.adobe {
        if a != cam_adobe {
            eprintln!("  note: EXIF color space (Adobe={a}) ≠ calibration (Adobe={cam_adobe})");
        }
    }
    let adobe = r.adobe.unwrap_or(cam_adobe);
    let [w, h] = session.size();
    let (dw, dh) = target(w, h);
    let s = Settings::default();

    // (a) SPP path
    let t = Instant::now();
    let spp_img = match std::panic::catch_unwind(AssertUnwindSafe(|| session.ensure_spp(&s).map(|_| ()))) {
        Ok(Ok(())) => {
            r.spp.gpu = Some(develop::spp::last_used_gpu());
            let img = session.render_full(&s);
            r.spp.ok = true;
            Some(img)
        }
        Ok(Err(e)) => {
            r.spp.note = Some(e);
            None
        }
        Err(p) => {
            r.spp.note = Some(format!("panic: {}", panic_text(p)));
            None
        }
    };
    r.spp.seconds = t.elapsed().as_secs_f64();

    // (b) own pipeline
    let t = Instant::now();
    let own_img = match std::panic::catch_unwind(AssertUnwindSafe(|| session.render_full_own(&s))) {
        Ok(img) => {
            r.own.ok = true;
            Some(img)
        }
        Err(p) => {
            r.own.note = Some(format!("panic: {}", panic_text(p)));
            None
        }
    };
    r.own.seconds = t.elapsed().as_secs_f64();
    session.release_heavy();
    drop(session);

    // 3. metrics
    if let (Some(a), Some(b)) = (&own_img, &spp_img) {
        if (a.width, a.height) == (b.width, b.height) {
            r.psnr = Some(psnr(a, b));
        } else {
            r.own.note = Some(format!("size {}×{} ≠ SPP {}×{}", a.width, a.height, b.width, b.height));
        }
    }
    let small = |img: &Option<Rgb16>| img.as_ref().map(|i| (resize(&i.data, i.width, i.height, lut16(), dw, dh), clip_fraction(&i.data, 65000)));
    let spp_small = small(&spp_img);
    let own_small = small(&own_img);
    drop(spp_img);
    drop(own_img);
    let cam = jpeg_small(dw, dh, adobe);
    if let Some((cs, cclip)) = &cam {
        if let Some((ss, sclip)) = &spp_small {
            r.spp.jpeg = Some(compare_jpeg(ss, cs, *sclip, *cclip));
        }
        if let Some((os, oclip)) = &own_small {
            r.own.jpeg = Some(compare_jpeg(os, cs, *oclip, *cclip));
        }
    }

    // 4. contact image
    r.sheet = write_sheet(
        out,
        idx,
        &r.name,
        [spp_small.as_ref().map(|x| &x.0), own_small.as_ref().map(|x| &x.0), cam.as_ref().map(|x| &x.0)],
        r.orientation,
    )?;
    Ok(())
}

/// Contact image: SPP | own | camera side by side (upright); missing fields gray.
fn write_sheet(out: &Path, idx: usize, name: &str, panels: [Option<&Small>; 3], q: u8) -> Result<Option<String>> {
    let Some(first) = panels.iter().flatten().next() else { return Ok(None) };
    let red: Vec<Option<Small>> = panels.iter().map(|p| p.map(|s| rotate(s, q))).collect();
    let (pw, ph) = if q % 2 == 1 { (first.h, first.w) } else { (first.w, first.h) };
    const GAP: usize = 8;
    let (sw, sh) = (pw * 3 + GAP * 2, ph);
    let mut data = vec![10000u16; sw * sh * 3];
    for (i, p) in red.iter().enumerate() {
        let x0 = i * (pw + GAP);
        for y in 0..ph {
            for x in 0..pw {
                let d = (y * sw + x0 + x) * 3;
                match p {
                    Some(s) if s.w == pw && s.h == ph => {
                        for c in 0..3 {
                            data[d + c] = (srgb_encode(s.data[(y * pw + x) * 3 + c]) * 65535.0 + 0.5) as u16;
                        }
                    }
                    // not available: mid gray with diagonal stripes
                    _ => data[d..d + 3].fill(if (x + y) / 24 % 2 == 0 { 22000 } else { 26000 }),
                }
            }
        }
    }
    let rel = format!("bilder/{idx:03}_{}.jpg", Path::new(name).file_stem().unwrap_or_default().to_string_lossy());
    let img = Rgb16 { width: sw, height: sh, data };
    develop::export::write_jpeg(&img, &out.join(&rel), 85).with_context(|| format!("writing {rel}"))?;
    Ok(Some(rel))
}

// ---------------------------------------------------------------- run

fn collect(inputs: &[String]) -> Vec<PathBuf> {
    fn walk(p: &Path, v: &mut Vec<PathBuf>) {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(p).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            entries.sort();
            for e in entries {
                walk(&e, v);
            }
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f")) {
            v.push(p.to_path_buf());
        }
    }
    let mut v = Vec::new();
    for i in inputs {
        let p = Path::new(i);
        if p.is_file() {
            v.push(p.to_path_buf());
        } else {
            walk(p, &mut v);
        }
    }
    v
}

pub fn run(out_dir: &str, inputs: &[String]) -> Result<()> {
    let out = Path::new(out_dir);
    std::fs::create_dir_all(out.join("images"))?;
    let files = collect(inputs);
    anyhow::ensure!(!files.is_empty(), "no X3F files found");
    match develop::spp::gpu_adapter() {
        Ok(name) => eprintln!("GPU: {name}"),
        Err(e) => eprintln!("no GPU: {e}"),
    }
    let started = Instant::now();
    let mut results: Vec<FileRes> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        eprintln!("[{}/{}] {}", i + 1, files.len(), f.display());
        let mut r = FileRes::default();
        check_file(f, i, out, &mut r);
        r.group = if r.model.is_empty() {
            f.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            // group = model without maker and firmware info
            let m = r.model.trim_start_matches("SIGMA ");
            m.split(" ver").next().unwrap_or(m).trim().to_string()
        };
        eprintln!(
            "  → {} ({:.1} s){}",
            r.verdict.map_or("?", |a| a.label()),
            r.seconds,
            r.error.as_ref().map(|e| format!(" – {e}")).unwrap_or_default()
        );
        results.push(r);
        // write after every file so that an abort loses nothing
        write_reports(out, &results, started.elapsed().as_secs_f64())?;
    }
    develop::spp::gpu_release();
    eprintln!("report: {}", out.join("report.html").display());
    Ok(())
}

fn write_reports(out: &Path, results: &[FileRes], secs: f64) -> Result<()> {
    let json = json!({
        "created_s": secs,
        "thresholds": {
            "psnr_total_db": [PSNR_TOTAL.0, PSNR_TOTAL.1], "psnr_zone_db": [PSNR_ZONE.0, PSNR_ZONE.1],
            "delta_ev": [DEV.0, DEV.1], "color_cast_delta_ab": [DAB.0, DAB.1], "clipping_more_pp": [CLIP.0, CLIP.1],
        },
        "files": results.iter().map(FileRes::to_json).collect::<Vec<_>>(),
    });
    std::fs::write(out.join("result.json"), serde_json::to_string_pretty(&json)?)?;
    std::fs::write(out.join("report.html"), html(results, secs))?;
    Ok(())
}

// ---------------------------------------------------------------- HTML

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn dot(a: Verdict) -> String {
    format!("<span class=\"dot {}\" title=\"{}\"></span>", a.key(), a.label())
}

fn opt(v: Option<f64>, f: impl Fn(f64) -> String) -> String {
    v.filter(|x| x.is_finite()).map(f).unwrap_or_else(|| "–".into())
}

fn html(results: &[FileRes], secs: f64) -> String {
    let mut groups: Vec<(String, Vec<&FileRes>)> = Vec::new();
    for r in results {
        match groups.iter_mut().find(|g| g.0 == r.group) {
            Some(g) => g.1.push(r),
            None => groups.push((r.group.clone(), vec![r])),
        }
    }
    let count = |v: &[&FileRes], a: Verdict| v.iter().filter(|r| r.verdict == Some(a)).count();
    let all: Vec<&FileRes> = results.iter().collect();
    let mut h = String::new();
    let _ = write!(
        h,
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>X3F quality check</title><style>
:root{{--bg:#111316;--panel:#1a1d22;--line:#2a2f37;--fg:#e4e6ea;--mute:#8d95a1;--gruen:#3fb950;--gelb:#d29922;--rot:#f85149;--grau:#6e7681}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}}
main{{max-width:1500px;margin:0 auto;padding:24px 16px 64px}}h1{{font-size:22px;margin:0 0 4px}}h2{{font-size:18px;margin:36px 0 10px;padding-top:12px;border-top:1px solid var(--line)}}
.mute{{color:var(--mute)}}.dot{{display:inline-block;width:11px;height:11px;border-radius:50%;vertical-align:-1px}}
.dot.gruen{{background:var(--gruen)}}.dot.gelb{{background:var(--gelb)}}.dot.rot{{background:var(--rot)}}.dot.grau{{background:var(--grau)}}
.chips{{display:flex;gap:14px;flex-wrap:wrap;margin:8px 0}}.chip{{background:var(--panel);border:1px solid var(--line);border-radius:6px;padding:4px 10px}}
.wrap{{overflow-x:auto}}table{{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}}th,td{{padding:5px 8px;border-bottom:1px solid var(--line);text-align:left;vertical-align:top;white-space:nowrap}}
th{{color:var(--mute);font-weight:500;font-size:12px}}td.num{{text-align:right}}td.why{{white-space:normal;min-width:280px;font-size:12px}}
.why div{{margin:1px 0}}.err{{color:var(--rot)}}figure{{margin:14px 0;background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:10px}}
figure img{{width:100%;height:auto;display:block;border-radius:4px}}figcaption{{margin-bottom:8px}}.legend{{display:grid;grid-template-columns:repeat(3,1fr);text-align:center;color:var(--mute);font-size:12px;margin-top:4px}}
a{{color:#79c0ff}}details{{margin:12px 0}}summary{{cursor:pointer;color:var(--mute)}}
</style></head><body><main>
<h1>X3F quality check</h1><div class="mute">{} files · run time {:.0} s · default settings · reference: embedded camera JPEG</div>
<div class="chips"><span class="chip">{} green {}</span><span class="chip">{} yellow {}</span><span class="chip">{} red {}</span><span class="chip">{} not supported {}</span></div>
<details><summary>Thresholds</summary><table><tr><th>Metric</th><th>green</th><th>yellow</th><th>otherwise</th></tr>
<tr><td>PSNR own↔SPP overall</td><td>≥ {} dB</td><td>≥ {} dB</td><td>red</td></tr>
<tr><td>PSNR worst brightness zone (≥ 2 % of pixels)</td><td>≥ {} dB</td><td>≥ {} dB</td><td>red</td></tr>
<tr><td>|ΔEV| against camera JPEG (median log2 Y ratio)</td><td>&lt; {}</td><td>&lt; {}</td><td>red</td></tr>
<tr><td>Color cast |Δa*,Δb*| (midtones L* 20–90)</td><td>&lt; {}</td><td>&lt; {}</td><td>red</td></tr>
<tr><td>Extra clipping compared to camera (percentage points)</td><td>&lt; {}</td><td>&lt; {}</td><td>red</td></tr>
<tr><td>SPP path not available / no camera JPEG</td><td></td><td>yellow</td><td></td></tr>
</table><p class="mute">Overall light = worst individual light. Camera JPEG metrics apply to the main path (SPP, otherwise own pipeline).</p></details>
"#,
        results.len(),
        secs,
        dot(Verdict::Green), count(&all, Verdict::Green), dot(Verdict::Yellow), count(&all, Verdict::Yellow),
        dot(Verdict::Red), count(&all, Verdict::Red), dot(Verdict::Gray), count(&all, Verdict::Gray),
        PSNR_TOTAL.0, PSNR_TOTAL.1, PSNR_ZONE.0, PSNR_ZONE.1, DEV.0, DEV.1, DAB.0, DAB.1, CLIP.0, CLIP.1,
    );
    for (name, rs) in &groups {
        let _ = write!(
            h,
            "<h2>{}</h2><div class=\"chips\"><span class=\"chip\">{} {}</span><span class=\"chip\">{} {}</span><span class=\"chip\">{} {}</span><span class=\"chip\">{} {}</span></div>",
            esc(name),
            dot(Verdict::Green), count(rs, Verdict::Green), dot(Verdict::Yellow), count(rs, Verdict::Yellow),
            dot(Verdict::Red), count(rs, Verdict::Red), dot(Verdict::Gray), count(rs, Verdict::Gray)
        );
        h.push_str(
            "<div class=\"wrap\"><table><tr><th></th><th>File</th><th>X3F / format</th><th>ISO · mode · WB</th>\
             <th>Dec.</th><th>SPP</th><th>own</th><th>PSNR o↔S</th><th>shadows/midtones/highlights</th>\
             <th>ΔEV SPP / own</th><th>ΔEV S/M/H (main)</th><th>Δa* Δb*</th><th>Chroma</th><th>ΔE median</th><th>Clip % / camera</th><th>Reason</th></tr>",
        );
        for r in rs {
            let a = r.verdict.unwrap_or(Verdict::Red);
            let main = if r.spp.ok { &r.spp } else { &r.own };
            let j = main.jpeg.as_ref();
            let t = |p: &PathRes| {
                if p.ok {
                    format!("{:.1} s{}", p.seconds, if p.gpu == Some(true) { " GPU" } else { "" })
                } else {
                    "–".into()
                }
            };
            let zones = r.psnr.as_ref().map_or("–".into(), |p| {
                p.zones.iter().map(|z| format!("{:.1}", z.psnr)).collect::<Vec<_>>().join(" / ")
            });
            let why: String = if !r.supported {
                format!("<div class=\"mute\">{}</div>", esc(r.error.as_deref().unwrap_or("not supported")))
            } else {
                r.reasons
                    .iter()
                    .map(|(a, t)| format!("<div>{} {}</div>", dot(*a), if *a == Verdict::Red && t.starts_with("error") { format!("<span class=\"err\">{}</span>", esc(t)) } else { esc(t) }))
                    .collect()
            };
            let _ = write!(
                h,
                "<tr><td>{}</td><td><a href=\"#f{}\">{}</a><div class=\"mute\">{}</div></td><td>{}<div class=\"mute\">{} {}</div></td><td>{}<div class=\"mute\">{} · {}</div></td>\
                 <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>\
                 <td class=\"num\">{} / {}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"why\">{}</td></tr>",
                dot(a),
                esc(&r.name), esc(&r.name),
                esc(r.path.rsplit('/').nth(1).unwrap_or("")),
                esc(&r.version), esc(&r.format), esc(&r.size),
                esc(&r.iso), esc(&r.mode), esc(&r.wb),
                opt(r.t_decode, |v| format!("{v:.2} s")),
                t(&r.spp), t(&r.own),
                opt(r.psnr.as_ref().map(|p| p.total), |v| format!("{v:.1} dB")),
                zones,
                opt(r.spp.jpeg.as_ref().map(|j| j.d_ev), fmt_ev), opt(r.own.jpeg.as_ref().map(|j| j.d_ev), fmt_ev),
                j.map_or("–".into(), |j| j.d_ev_zones.iter().map(|z| opt(*z, fmt_ev)).collect::<Vec<_>>().join(" / ")),
                j.map_or("–".into(), |j| format!("{:+.1} {:+.1}", j.d_a, j.d_b)),
                opt(j.map(|j| j.chroma_ratio), |v| format!("{v:.2}")),
                opt(j.map(|j| j.de_median), |v| format!("{v:.1}")),
                j.map_or("–".into(), |j| format!("{:.2} / {:.2}", j.clip * 100.0, j.clip_jpeg * 100.0)),
                why
            );
        }
        h.push_str("</table></div>");
        for r in rs {
            if let Some(s) = &r.sheet {
                let _ = write!(
                    h,
                    "<figure id=\"f{}\"><figcaption>{} <b>{}</b> <span class=\"mute\">{}</span></figcaption><a href=\"{}\"><img loading=\"lazy\" src=\"{}\" alt=\"{}\"></a>\
                     <div class=\"legend\"><span>SPP path{}</span><span>own pipeline</span><span>camera JPEG</span></div></figure>",
                    esc(&r.name),
                    dot(r.verdict.unwrap_or(Verdict::Red)),
                    esc(&r.name),
                    esc(&r.path),
                    esc(s), esc(s), esc(&r.name),
                    if r.spp.ok { "" } else { " (not available)" }
                );
            }
        }
    }
    h.push_str("</main></body></html>\n");
    h
}
