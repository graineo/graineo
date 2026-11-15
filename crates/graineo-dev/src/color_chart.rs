//! `graineo-dev color-chart A B [options]`: yardstick for color film profiles.
//!
//! Creates a test chart with 71 self-generated patches (24 ColorChecker-like patches with
//! known sRGB values, 21-step gray wedge, 18 saturation patches = 6 hues × 3 levels, 8 skin tones),
//! runs it with two film profiles through the same pipeline (grain and halation off, same
//! paper, otherwise default settings) and compares the results in CIE L*a*b*: ΔE00
//! (mean/median/max, per group), chroma and contrast ratio, gray cast, brightness.
//!
//! ```text
//! graineo-dev color-chart A B [--paper ID] [--data FOLDER] [--negative] [--image PHOTO] [--out PREFIX] [--json FILE]
//! ```
//! * `A`, `B`: profile ID in the data folder (`GRAINEO_FILM_DATA`, otherwise the usual location) or path to a
//!   profile file `*.json` (placed in a temporary folder for this run).
//!   `B` = `offiziell`: the profile with the ID of `A` without the suffix `_eigen`/`_vorhersage`/`_pred…`
//!   from the data folder `--data` (default: the usual data folder in the source tree).
//! * `A` is the candidate, `B` the yardstick; ratios are A to B.
//! * `--paper ID`: paper for both (default: target paper of B, otherwise of A). Slide film is scanned directly.
//! * `--negative`: scan the negative directly instead of printing on paper.
//! * `--image PHOTO`: additionally run a photo (JPEG/PNG, downscaled to ≤ 1000 px) with both profiles and report ΔE00 over all
//!   pixels (mean, 95th percentile).
//! * `--off PREFIX`: writes `PREFIX_tafel.png` (input), `PREFIX_a.png`, `PREFIX_b.png` (with `--image` also `PREFIX_bild[_a|_b].png`).
//! * `--json FILE`: measurements as JSON.
//!
//! Threshold for "good": mean ΔE00 < 1.5 and skin tones < 1.5.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use develop::film::{self, FilmSettings};
use develop::look::Geo;
use develop::Linear;

/// Edge length of a patch in pixels.
const P: usize = 96;
const COLUMNS: usize = 12;
const ROWS: usize = 6;

/// Group of a test patch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatchGroup {
    ColorChecker,
    Gray,
    Saturation,
    Skin,
}

impl PatchGroup {
    pub const ALL_GROUPS: [PatchGroup; 4] = [PatchGroup::Gray, PatchGroup::ColorChecker, PatchGroup::Saturation, PatchGroup::Skin];
    pub fn name(self) -> &'static str {
        match self {
            PatchGroup::ColorChecker => "colorchecker",
            PatchGroup::Gray => "grau",
            PatchGroup::Saturation => "saturation",
            PatchGroup::Skin => "skin",
        }
    }
}

/// A test patch: group, name, sRGB target value (8 bit).
pub struct Patch {
    pub group: PatchGroup,
    pub name: String,
    pub rgb: [u8; 3],
}

const CC: [([u8; 3], &str); 24] = [
    ([115, 82, 68], "dark skin"),
    ([194, 150, 130], "light skin"),
    ([98, 122, 157], "blue sky"),
    ([87, 108, 67], "foliage"),
    ([133, 128, 177], "blue flower"),
    ([103, 189, 170], "bluish green"),
    ([214, 126, 44], "orange"),
    ([80, 91, 166], "purplish blue"),
    ([193, 90, 99], "moderate red"),
    ([94, 60, 108], "purple"),
    ([157, 188, 64], "yellow green"),
    ([224, 163, 46], "orange yellow"),
    ([56, 61, 150], "blue"),
    ([70, 148, 73], "green"),
    ([175, 54, 60], "red"),
    ([231, 199, 31], "yellow"),
    ([187, 86, 149], "magenta"),
    ([8, 133, 161], "cyan"),
    ([243, 243, 242], "white"),
    ([200, 200, 200], "neutral 8"),
    ([160, 160, 160], "neutral 6.5"),
    ([122, 122, 121], "neutral 5"),
    ([85, 85, 85], "neutral 3.5"),
    ([52, 52, 52], "black"),
];

const SKIN: [[u8; 3]; 8] =
    [[255, 224, 189], [241, 194, 125], [224, 172, 105], [198, 134, 66], [141, 85, 36], [92, 56, 38], [238, 200, 175], [210, 160, 130]];

fn srgb_enc(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.0031308 { 12.92 * l } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_dec(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn hsv(h: f64, s: f64, v: f64) -> [u8; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|x| (255.0 * x).round() as u8)
}

/// All test patches in chart order: 24 ColorChecker, 21 gray wedge, 18 saturation, 8 skin.
pub fn patches() -> Vec<Patch> {
    let mut v = Vec::new();
    for (rgb, n) in CC {
        v.push(Patch { group: PatchGroup::ColorChecker, name: n.into(), rgb });
    }
    // gray wedge: 21 steps, linear 0.004 … 0.95, evenly spaced logarithmically
    for i in 0..21 {
        let lin = 0.004f64 * (0.95f64 / 0.004).powf(i as f64 / 20.0);
        let c = (255.0 * srgb_enc(lin)).round() as u8;
        v.push(Patch { group: PatchGroup::Gray, name: format!("gray{}", i + 1), rgb: [c; 3] });
    }
    // saturation rows: 6 hues (R, Y, G, C, B, M) at saturation 0.35 / 0.7 / 1.0
    for (si, s) in [0.35, 0.7, 1.0].into_iter().enumerate() {
        for hi in 0..6 {
            v.push(Patch { group: PatchGroup::Saturation, name: format!("Sat{}", si * 6 + hi), rgb: hsv(hi as f64 / 6.0, s, 0.75) });
        }
    }
    for (i, rgb) in SKIN.into_iter().enumerate() {
        v.push(Patch { group: PatchGroup::Skin, name: format!("skin{}", i + 1), rgb });
    }
    v
}

/// Chart image (sRGB 8 bit, width, height): patches row by row in 12 columns on mid gray.
pub fn chart(patches: &[Patch]) -> (Vec<u8>, usize, usize) {
    let (w, h) = (COLUMNS * P, ROWS * P);
    let mut px = vec![118u8; w * h * 3];
    for (i, f) in patches.iter().enumerate() {
        let (r, c) = (i / COLUMNS, i % COLUMNS);
        for y in r * P..(r + 1) * P {
            for x in c * P..(c + 1) * P {
                px[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&f.rgb);
            }
        }
    }
    (px, w, h)
}

/// Median values of the patch centers (border P/4 left out) from an image made by [`chart`].
pub fn patch_medians(px: &[u8], w: usize, n: usize) -> Vec<[f64; 3]> {
    (0..n)
        .map(|i| {
            let (r, c) = (i / COLUMNS, i % COLUMNS);
            let mut ch: [Vec<u8>; 3] = Default::default();
            for y in r * P + P / 4..(r + 1) * P - P / 4 {
                for x in c * P + P / 4..(c + 1) * P - P / 4 {
                    for k in 0..3 {
                        ch[k].push(px[(y * w + x) * 3 + k]);
                    }
                }
            }
            let mut m = [0.0; 3];
            for k in 0..3 {
                ch[k].sort_unstable();
                let l = ch[k].len();
                m[k] = if l % 2 == 1 { ch[k][l / 2] as f64 } else { (ch[k][l / 2 - 1] as f64 + ch[k][l / 2] as f64) / 2.0 };
            }
            m
        })
        .collect()
}

const M_SRGB: [[f64; 3]; 3] = [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]];

/// sRGB (8 bit, as decimal values 0 … 255) → CIE L*a*b* (white point = sRGB white, D65).
pub fn lab(rgb: [f64; 3]) -> [f64; 3] {
    let lin = rgb.map(|v| srgb_dec(v / 255.0));
    let wp: Vec<f64> = M_SRGB.iter().map(|r| r.iter().sum()).collect();
    let xyz: Vec<f64> = (0..3).map(|i| (M_SRGB[i][0] * lin[0] + M_SRGB[i][1] * lin[1] + M_SRGB[i][2] * lin[2]) / wp[i]).collect();
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(xyz[0]), f(xyz[1]), f(xyz[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Color difference CIEDE2000 (Sharma et al. 2005, kL = kC = kH = 1).
pub fn de00(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (l1, a1, b1) = (a[0], a[1], a[2]);
    let (l2, a2, b2) = (b[0], b[1], b[2]);
    let (c1, c2) = (a1.hypot(b1), a2.hypot(b2));
    let cb = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cb.powi(7) / (cb.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let hue = |y: f64, x: f64| if x == 0.0 && y == 0.0 { 0.0 } else { y.atan2(x).to_degrees().rem_euclid(360.0) };
    let (h1, h2) = (hue(b1, a1p), hue(b2, a2p));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let mut dh = h2 - h1;
    if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }
    if c1p * c2p == 0.0 {
        dh = 0.0;
    }
    let dh_big = 2.0 * (c1p * c2p).sqrt() * (dh.to_radians() / 2.0).sin();
    let lb = (l1 + l2) / 2.0;
    let cbp = (c1p + c2p) / 2.0;
    let hs = h1 + h2;
    let hb = if c1p * c2p == 0.0 {
        hs
    } else if (h1 - h2).abs() > 180.0 {
        (hs + 360.0) / 2.0
    } else {
        hs / 2.0
    };
    let t = 1.0 - 0.17 * (hb - 30.0).to_radians().cos() + 0.24 * (2.0 * hb).to_radians().cos() + 0.32 * (3.0 * hb + 6.0).to_radians().cos()
        - 0.2 * (4.0 * hb - 63.0).to_radians().cos();
    let dth = 30.0 * (-((hb - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cbp.powi(7) / (cbp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lb - 50.0).powi(2) / (20.0 + (lb - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbp;
    let sh = 1.0 + 0.015 * cbp * t;
    let rt = -(2.0 * dth).to_radians().sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dh_big / sh).powi(2) + rt * (dc / sc) * (dh_big / sh)).sqrt()
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let l = s.len();
    if l % 2 == 1 { s[l / 2] } else { (s[l / 2 - 1] + s[l / 2]) / 2.0 }
}

/// Slope of the regression line y(x).
fn slope(x: &[f64], y: &[f64]) -> f64 {
    let (mx, my) = (mean(x), mean(y));
    let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let sxx: f64 = x.iter().map(|a| (a - mx).powi(2)).sum();
    if sxx == 0.0 { 0.0 } else { sxy / sxx }
}

/// Group values.
pub struct Stat {
    pub n: usize,
    pub mean: f64,
    pub median: f64,
    pub max: f64,
}

/// Measurement result of comparing A (candidate) against B (yardstick).
pub struct Comparison {
    pub all: Stat,
    pub groups: Vec<(PatchGroup, Stat)>,
    pub per_patch: Vec<(String, PatchGroup, f64)>,
    /// chroma A/B, median over all color patches (ColorChecker, saturation, skin)
    pub chroma_ratio: f64,
    /// L* slope of the gray wedge (steps 7 … 16) A/B
    pub contrast_ratio: f64,
    /// mean brightness difference L*(A) − L*(B), gray wedge
    pub gray_dl: f64,
    /// mean color cast a*, b* (A − B) on the gray wedge
    pub gray_cast: [f64; 2],
    /// mean hue shift A − B in degrees (patches with chroma > 10), signed
    pub mean_hue_shift: f64,
}

impl Comparison {
    /// Threshold "good": mean ΔE00 < 1.5 and skin tones < 1.5.
    pub fn is_good(&self) -> bool {
        self.all.mean < 1.5 && self.groups.iter().find(|(g, _)| *g == PatchGroup::Skin).is_some_and(|(_, s)| s.mean < 1.5)
    }
}

fn stat(v: &[f64]) -> Stat {
    Stat { n: v.len(), mean: mean(v), median: median(v), max: v.iter().cloned().fold(0.0, f64::max) }
}

/// Compares two series of measurements (patch medians in sRGB 8 bit) for the patches `f`.
pub fn compare_patches(f: &[Patch], a: &[[f64; 3]], b: &[[f64; 3]]) -> Comparison {
    let la: Vec<[f64; 3]> = a.iter().map(|&c| lab(c)).collect();
    let lb: Vec<[f64; 3]> = b.iter().map(|&c| lab(c)).collect();
    let de: Vec<f64> = la.iter().zip(&lb).map(|(&x, &y)| de00(x, y)).collect();
    let groups = PatchGroup::ALL_GROUPS
        .iter()
        .map(|&g| {
            let v: Vec<f64> = f.iter().zip(&de).filter(|(x, _)| x.group == g).map(|(_, d)| *d).collect();
            (g, stat(&v))
        })
        .collect();
    let idx = |g: PatchGroup| -> Vec<usize> { (0..f.len()).filter(|&i| f[i].group == g).collect() };
    let color_name: Vec<usize> = (0..f.len()).filter(|&i| f[i].group != PatchGroup::Gray).collect();
    let chroma: Vec<f64> = color_name.iter().map(|&i| la[i][1].hypot(la[i][2]) / lb[i][1].hypot(lb[i][2]).max(1e-3)).collect();
    let gray = idx(PatchGroup::Gray);
    let x: Vec<f64> = (6..16).map(|i| i as f64).collect();
    let ya: Vec<f64> = (6..16).map(|i| la[gray[i]][0]).collect();
    let yb: Vec<f64> = (6..16).map(|i| lb[gray[i]][0]).collect();
    let contrast = if gray.len() >= 16 { slope(&x, &ya) / slope(&x, &yb).max(1e-9) } else { 1.0 };
    let dl: Vec<f64> = gray.iter().map(|&i| la[i][0] - lb[i][0]).collect();
    let da: Vec<f64> = gray.iter().map(|&i| la[i][1] - lb[i][1]).collect();
    let db: Vec<f64> = gray.iter().map(|&i| la[i][2] - lb[i][2]).collect();
    let dh: Vec<f64> = color_name
        .iter()
        .filter(|&&i| lb[i][1].hypot(lb[i][2]) > 10.0 && la[i][1].hypot(la[i][2]) > 10.0)
        .map(|&i| ((la[i][2].atan2(la[i][1]) - lb[i][2].atan2(lb[i][1])).to_degrees() + 540.0).rem_euclid(360.0) - 180.0)
        .collect();
    Comparison {
        all: stat(&de),
        groups,
        per_patch: f.iter().zip(&de).map(|(x, &d)| (x.name.clone(), x.group, d)).collect(),
        chroma_ratio: median(&chroma),
        contrast_ratio: contrast,
        gray_dl: mean(&dl),
        gray_cast: [mean(&da), mean(&db)],
        mean_hue_shift: mean(&dh),
    }
}

impl Comparison {
    pub fn json(&self) -> serde_json::Value {
        let st = |s: &Stat| serde_json::json!({"n": s.n, "mean": s.mean, "median": s.median, "max": s.max});
        let mut g = serde_json::Map::new();
        for (k, s) in &self.groups {
            g.insert(k.name().into(), st(s));
        }
        serde_json::json!({
            "all": st(&self.all), "groups": g, "good": self.is_good(),
            "chroma_ratio": self.chroma_ratio, "contrast_ratio": self.contrast_ratio,
            "gray_dL": self.gray_dl, "gray_cast_ab": self.gray_cast, "hue_mean_deg": self.mean_hue_shift,
            "per_patch": self.per_patch.iter().map(|(n, g, d)| serde_json::json!({"name": n, "group": g.name(), "dE00": d})).collect::<Vec<_>>(),
        })
    }
}

/// Load a photo (JPEG/PNG), long edge at most 1000 pixels.
fn load_photo(path: &str) -> Result<(usize, usize, Vec<u8>)> {
    let mut img = image::open(path).with_context(|| format!("opening {path}"))?.to_rgb8();
    let l = img.width().max(img.height());
    if l > 1000 {
        let f = 1000.0 / l as f32;
        img = image::imageops::resize(&img, (img.width() as f32 * f).round() as u32, (img.height() as f32 * f).round() as u32, image::imageops::FilterType::Triangle);
    }
    Ok((img.width() as usize, img.height() as usize, img.into_raw()))
}

/// ΔE00 of two equally sized sRGB 8-bit images: mean, 95th percentile, median of the chroma ratio A/B (points with C* > 5).
pub fn image_distance(a: &[u8], b: &[u8]) -> (f64, f64, f64) {
    let n = a.len() / 3;
    let mut de = Vec::with_capacity(n);
    let mut ch = Vec::new();
    for i in 0..n {
        let la = lab([a[3 * i] as f64, a[3 * i + 1] as f64, a[3 * i + 2] as f64]);
        let lb = lab([b[3 * i] as f64, b[3 * i + 1] as f64, b[3 * i + 2] as f64]);
        de.push(de00(la, lb));
        let (ca, cb) = (la[1].hypot(la[2]), lb[1].hypot(lb[2]));
        if cb > 5.0 {
            ch.push(ca / cb);
        }
    }
    let m = mean(&de);
    de.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let p95 = de[((de.len() as f64) * 0.95) as usize % de.len().max(1)];
    (m, p95, median(&ch))
}

/// Base data folder like [`film::data_dir`], but without filling the cache.
fn base_data(data: Option<&str>) -> Result<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    if let Some(d) = data {
        c.push(PathBuf::from(d));
    }
    if let Some(d) = std::env::var_os("GRAINEO_FILM_DATA") {
        c.push(PathBuf::from(d));
    }
    if let Ok(d) = std::env::current_dir() {
        c.extend(d.ancestors().take(8).map(|a| a.join("third_party/spektrafilm-rs/data")));
    }
    c.into_iter().find(|p| p.join("profiles").is_dir() && p.join("luts").is_dir()).context("film data (spektrafilm) not found")
}

/// Temporary data folder: luts/filters linked, profiles = all profiles of the base + the files from `extra`.
fn work_dir(base: &Path, extra: &[PathBuf]) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("color-chart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // this run's own temporary folder
    std::fs::create_dir_all(dir.join("profiles"))?;
    for n in ["luts", "filters"] {
        if base.join(n).exists() {
            std::os::unix::fs::symlink(base.join(n).canonicalize()?, dir.join(n))?;
        }
    }
    for e in std::fs::read_dir(base.join("profiles"))?.flatten() {
        let _ = std::os::unix::fs::symlink(e.path().canonicalize()?, dir.join("profiles").join(e.file_name()));
    }
    for f in extra {
        let target = dir.join("profiles").join(f.file_name().context("file name")?);
        let _ = std::fs::remove_file(&target);
        std::fs::copy(f, &target).with_context(|| format!("copying {}", f.display()))?;
    }
    Ok(dir)
}

/// ID without the suffix `_eigen`, `_vorhersage`, `_pred…`, `_neu`.
fn official_id(id: &str) -> String {
    let mut s = id.to_string();
    for z in ["_eigen", "_vorhersage", "_pred", "_regel", "_opt"] {
        if let Some(p) = s.find(z) {
            s.truncate(p);
        }
    }
    s
}

pub fn run(args: &[String]) -> Result<()> {
    let pos: Vec<&String> = {
        let mut v = Vec::new();
        let mut i = 0;
        while i < args.len() {
            if args[i].starts_with("--") {
                i += if ["--negative"].contains(&args[i].as_str()) { 1 } else { 2 };
            } else {
                v.push(&args[i]);
                i += 1;
            }
        }
        v
    };
    if pos.len() < 2 {
        bail!("usage: graineo-dev color-chart A B [--paper ID] [--data FOLDER] [--negative] [--image PHOTO] [--out PREFIX] [--json FILE]");
    }
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let (a_arg, b_arg) = (pos[0].as_str(), pos[1].as_str());
    let base = base_data(opt("--data").as_deref())?;

    // resolve profiles: file or ID
    let mut extra: Vec<PathBuf> = Vec::new();
    let mut id_of = |arg: &str| -> Result<String> {
        if arg.ends_with(".json") {
            let p = PathBuf::from(arg);
            extra.push(p.clone());
            Ok(p.file_stem().context("file name")?.to_string_lossy().into_owned())
        } else {
            Ok(arg.to_string())
        }
    };
    let id_a = id_of(a_arg)?;
    let id_b = if b_arg == "official" { official_id(&id_a) } else { id_of(b_arg)? };
    let dir = work_dir(&base, &extra)?;
    std::env::set_var("GRAINEO_FILM_DATA", &dir);
    let outcome = (|| -> Result<Comparison> {
        let st_a = film::stock(&id_a).with_context(|| format!("unknown profile {id_a}"))?;
        let st_b = film::stock(&id_b).with_context(|| format!("unknown profile {id_b}"))?;
        let paper = opt("--paper").or_else(|| st_b.default_paper.clone()).or_else(|| st_a.default_paper.clone());
        let negative = args.iter().any(|a| a == "--negative");
        let fl = patches();
        let (px, w, h) = chart(&fl);
        let dec = |v: u8| srgb_dec(v as f64 / 255.0) as f32;
        let scene = Linear { mono: false, haze: None, toneq: None, width: w, height: h, data: px.iter().map(|&v| dec(v)).collect(), detail: None };
        let compute_scene = |id: &str, scene: &Linear, w: usize, h: usize| -> Result<Vec<u8>> {
            let fs = FilmSettings {
                active: true,
                stock: id.to_string(),
                paper: paper.clone(),
                negative: negative,
                grain: 0.0,
                halation: 0.0,
                format_mm: 35.0,
                ..FilmSettings::default()
            };
            Ok(film::develop(scene, &fs, Geo::full(w, h)).map_err(anyhow::Error::msg)?.to_rgb8())
        };
        let compute = |id: &str| compute_scene(id, &scene, w, h);
        let (out_a, out_b) = (compute(&id_a)?, compute(&id_b)?);
        if let Some(pre) = opt("--out") {
            let save = |n: &str, d: &[u8]| -> Result<()> {
                image::RgbImage::from_raw(w as u32, h as u32, d.to_vec()).context("image size")?.save(format!("{pre}_{n}.png"))?;
                Ok(())
            };
            save("chart", &px)?;
            save("a", &out_a)?;
            save("b", &out_b)?;
        }
        // photo (optional): mean and 95th percentile of ΔE00 over all pixels
        if let Some(photo_path) = opt("--image") {
            let (bw, bh, bpx) = load_photo(&photo_path)?;
            let dec = |v: u8| srgb_dec(v as f64 / 255.0) as f32;
            let photo_scene = Linear { mono: false, haze: None, toneq: None, width: bw, height: bh, data: bpx.iter().map(|&v| dec(v)).collect(), detail: None };
            let (fa, fb) = (compute_scene(&id_a, &photo_scene, bw, bh)?, compute_scene(&id_b, &photo_scene, bw, bh)?);
            let (m, p95, chroma) = image_distance(&fa, &fb);
            println!("PHOTO {photo_path}: ΔE00 mean {m:.2}, 95th percentile {p95:.2}, chroma A/B {chroma:.3}");
            if let Some(pre) = opt("--out") {
                for (n, d) in [("image", &bpx), ("image_a", &fa), ("image_b", &fb)] {
                    image::RgbImage::from_raw(bw as u32, bh as u32, d.to_vec()).context("image size")?.save(format!("{pre}_{n}.png"))?;
                }
            }
        }
        let e = compare_patches(&fl, &patch_medians(&out_a, w, fl.len()), &patch_medians(&out_b, w, fl.len()));
        println!("COLOR_CHART A = {id_a}   B = {id_b}   paper = {}{}", paper.as_deref().unwrap_or("-"), if negative { "   (negative scan)" } else { "" });
        println!("{:<14}{:>4}{:>9}{:>9}{:>9}", "group", "n", "mean", "median", "max");
        let row = |n: &str, s: &Stat| println!("{n:<14}{:>4}{:>9.2}{:>9.2}{:>9.2}", s.n, s.mean, s.median, s.max);
        row("all", &e.all);
        for (g, s) in &e.groups {
            row(g.name(), s);
        }
        println!(
            "chroma A/B (median) {:.3}   contrast A/B (gray wedge) {:.3}   gray dL* {:+.2}   gray cast a*/b* {:+.2}/{:+.2}   hue {:+.1} degrees",
            e.chroma_ratio, e.contrast_ratio, e.gray_dl, e.gray_cast[0], e.gray_cast[1], e.mean_hue_shift
        );
        let mut bad = e.per_patch.clone();
        bad.sort_by(|x, y| y.2.partial_cmp(&x.2).unwrap());
        println!("worst patches: {}", bad.iter().take(5).map(|(n, _, d)| format!("{n} {d:.1}")).collect::<Vec<_>>().join(", "));
        println!("threshold (mean < 1.5 and skin < 1.5): {}", if e.is_good() { "reached" } else { "not reached" });
        if let Some(j) = opt("--json") {
            std::fs::write(&j, serde_json::to_string_pretty(&e.json())?)?;
        }
        Ok(e)
    })();
    let _ = std::fs::remove_dir_all(&dir); // own temporary folder
    outcome.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_has_71_patches_in_four_groups() {
        let f = patches();
        assert_eq!(f.len(), 71);
        let n = |g: PatchGroup| f.iter().filter(|x| x.group == g).count();
        assert_eq!((n(PatchGroup::ColorChecker), n(PatchGroup::Gray), n(PatchGroup::Saturation), n(PatchGroup::Skin)), (24, 21, 18, 8));
        // gray wedge rises monotonically, ends at 0.4 % and 95 % linear
        let g: Vec<u8> = f.iter().filter(|x| x.group == PatchGroup::Gray).map(|x| x.rgb[0]).collect();
        assert!(g.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(g[0], (255.0 * srgb_enc(0.004)).round() as u8);
        assert_eq!(g[20], (255.0 * srgb_enc(0.95)).round() as u8);
    }

    #[test]
    fn patch_median_hits_the_patches() {
        let f = patches();
        let (px, w, _) = chart(&f);
        let m = patch_medians(&px, w, f.len());
        for (x, y) in f.iter().zip(&m) {
            assert_eq!(x.rgb.map(|v| v as f64), *y);
        }
    }

    #[test]
    fn lab_white_and_black() {
        let w = lab([255.0; 3]);
        assert!((w[0] - 100.0).abs() < 1e-6 && w[1].abs() < 1e-6 && w[2].abs() < 1e-6);
        let s = lab([0.0; 3]);
        assert!(s[0].abs() < 1e-9);
    }

    #[test]
    fn de00_against_sharma_test_data() {
        // example pairs from Sharma/Wu/Dalal 2005 (table 1), target values to 4 decimal places
        let pairs: [([f64; 3], [f64; 3], f64); 6] = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
            ([50.0, -1.3802, -84.2814], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, 2.5, 0.0], [50.0, 0.0, -2.5], 4.3065),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
        ];
        for (a, b, expected) in pairs {
            let d = de00(a, b);
            assert!((d - expected).abs() < 1e-3, "{a:?} {b:?}: {d} instead of {expected}");
            assert!((de00(b, a) - d).abs() < 1e-9, "symmetric");
        }
        assert_eq!(de00([50.0, 10.0, 10.0], [50.0, 10.0, 10.0]), 0.0);
    }

    #[test]
    fn compare_identical_is_zero_and_good() {
        let f = patches();
        let (px, w, _) = chart(&f);
        let m = patch_medians(&px, w, f.len());
        let e = compare_patches(&f, &m, &m);
        assert_eq!(e.all.max, 0.0);
        assert!((e.chroma_ratio - 1.0).abs() < 1e-9 && (e.contrast_ratio - 1.0).abs() < 1e-9);
        assert!(e.is_good());
    }

    #[test]
    fn compare_detects_more_saturation_and_contrast() {
        let f = patches();
        let (px, w, _) = chart(&f);
        let b = patch_medians(&px, w, f.len());
        // A: colors 20 % further from gray (in sRGB), gray wedge slightly steeper around the middle
        let a: Vec<[f64; 3]> = b
            .iter()
            .map(|c| {
                let m = (c[0] + c[1] + c[2]) / 3.0;
                c.map(|v| (m + (v - m) * 1.2).clamp(0.0, 255.0))
            })
            .collect();
        let e = compare_patches(&f, &a, &b);
        assert!(e.chroma_ratio > 1.1, "{}", e.chroma_ratio);
        assert!(e.all.mean > 1.0 && !e.is_good());
        assert!((e.contrast_ratio - 1.0).abs() < 1e-6, "gray stays the same");
    }

    #[test]
    fn image_distance_is_zero_for_identical_and_detects_more_chroma() {
        let a: Vec<u8> = (0..300).map(|i| (i * 7 % 256) as u8).collect();
        let (m0, p0, _) = image_distance(&a, &a);
        assert_eq!((m0, p0), (0.0, 0.0));
        // pixel (150, 100, 50) -> stronger color (200, 100, 0): chroma rises, ΔE00 > 0
        let x = [150u8, 100, 50].repeat(100);
        let y = [200u8, 100, 0].repeat(100);
        let (m, p95, c) = image_distance(&y, &x);
        assert!(m > 3.0 && p95 >= m - 1e-9 && c > 1.2, "{m} {p95} {c}");
    }

    #[test]
    fn official_id_strips_suffixes() {
        assert_eq!(official_id("kodak_portra_800_eigen"), "kodak_portra_800");
        assert_eq!(official_id("kodak_ektar_100_pred_regel"), "kodak_ektar_100");
        assert_eq!(official_id("kodak_gold_200"), "kodak_gold_200");
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(hsv(2.0 / 3.0, 1.0, 1.0), [0, 0, 255]);
        assert_eq!(hsv(0.5, 0.0, 0.5), [128, 128, 128]);
    }
}
