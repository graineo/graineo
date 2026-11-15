//! Lens measurement, workflow for a folder of test shots: read files, classify by content
//! (grid/straight lines or uniform surface), group by focal length or aperture, measure
//! ([`crate::measure`]) and build a profile from it that is saved as an own profile.
//!
//! All shots must come from the same camera and the same lens (RAW, not cropped, without
//! the camera's lens correction). Per focal length all grid shots count together (more lines, more accurate);
//! vignetting is determined per focal length and aperture from flat-field shots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use lensprof::flatfield::{self, FlatBlocks, FlatResult, FlatShot};
use lensprof::own::{QualityPart, SensorKind};

use crate::development::{Demosaic, Size};
use crate::lcp::Profile;
use crate::lensfun::Database;
use crate::measure::{self, GeoMeasurement, MeasureImage, LateralCa, Distortion, VigMeasurement, VignetteMeasurement};
use crate::RawFile;

/// What a shot is good for in the measurement.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    Grid,
    FlatField,
    Unusable(String),
}

#[derive(Debug, Clone)]
pub struct Shot {
    pub path: PathBuf,
    pub name: String,
    pub focal_length: Option<f32>,
    pub aperture: Option<f32>,
    pub role: Role,
    /// second shot of a flat-field pair (camera turned by 180°, from [`MeasureHints::pairs`])
    pub rotated: bool,
}

/// Measurement of the distortion (and lateral chromatic aberration) at one focal length.
#[derive(Debug, Clone)]
pub struct GeoGroup {
    pub focal_length: Option<f64>,
    pub images: Vec<String>,
    /// Shots that did not match the rest (name, reason) and therefore do not count
    pub rejected: Vec<(String, String)>,
    pub distortion: Result<Distortion, String>,
    pub lateral_ca: Option<LateralCa>,
    /// Before/after preview (width, height, sRGB8 before, sRGB8 after) of the first shot
    pub preview: Option<(usize, usize, Vec<u8>, Vec<u8>)>,
}

#[derive(Debug, Clone)]
pub struct VigGroup {
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub images: Vec<String>,
    pub vignette: Result<VignetteMeasurement, String>,
}

/// Colour cast per channel/layer from flat fields at one focal length and aperture.
#[derive(Debug, Clone)]
pub struct CastGroup {
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub images: Vec<String>,
    pub cast: Result<FlatResult, String>,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub maker: String,
    pub model: String,
    pub lens: Option<String>,
    pub crop: f64,
    /// false: camera not in the database – sensor size unknown (crop factor 1 assumed)
    pub crop_known: bool,
    pub shots: Vec<Shot>,
    pub geo: Vec<GeoGroup>,
    pub vig: Vec<VigGroup>,
    pub notes: Vec<String>,
    pub profile: Option<Profile>,
    /// colour cast per focal length and aperture (flat fields)
    pub cast: Vec<CastGroup>,
    /// quality per part and in total (weakest part)
    pub quality: Option<lensprof::own::Quality>,
    pub sensor: SensorKind,
    /// display name given by the user ([`MeasureHints::lens_name`])
    pub name: Option<String>,
}

/// A shot opened for the measurement: linear camera channels (Foveon: top/middle/bottom layer) in sensor position.
#[derive(Debug, Clone)]
pub struct ShotImage {
    pub maker: String,
    pub model: String,
    pub lens: Option<String>,
    pub focal_length: Option<f32>,
    pub aperture: Option<f32>,
    pub width: usize,
    pub height: usize,
    pub px: Vec<[f32; 3]>,
    /// clipping level in the unit of `px`
    pub saturation: f32,
    pub sensor: SensorKind,
}

/// Where the shots come from (rawler here, X3F in `rawdev::lens_shots`).
pub trait ShotSource: Sync {
    /// Error: reason for the user, starting with "nicht lesbar" or "nicht entwickelbar".
    fn open(&self, path: &Path) -> Result<ShotImage, String>;
}

/// Non-Sigma RAWs through rawler (as up to 0.94).
pub struct RawSource;

impl ShotSource for RawSource {
    fn open(&self, path: &Path) -> Result<ShotImage, String> {
        let rf = RawFile::open(path).map_err(|e| format!("nicht lesbar: {e}"))?;
        let basis = rf.basis(Demosaic::Auto, false, false, Size::Full).map_err(|e| format!("nicht entwickelbar: {e}"))?;
        let i = &rf.info;
        let sensor = match i.sensor {
            crate::Sensor::Bayer => SensorKind::Bayer,
            crate::Sensor::XTrans => SensorKind::Xtrans,
            crate::Sensor::Linear => SensorKind::Linear,
            _ => SensorKind::Unknown,
        };
        Ok(ShotImage {
            maker: i.maker.clone(),
            model: i.model.clone(),
            lens: i.exif.lens.clone(),
            focal_length: i.exif.focal_length,
            aperture: i.exif.aperture,
            width: basis.width,
            height: basis.height,
            px: basis.px,
            saturation: 1.0,
            sensor,
        })
    }
}

/// What the user told the assistant (all optional).
#[derive(Debug, Clone, Default)]
pub struct MeasureHints {
    /// display name of the lens (manual lenses have none in the file)
    pub lens_name: Option<String>,
    /// focal length when the files carry none
    pub focal_length: Option<f32>,
    /// aperture per file when the file carries none
    pub apertures: HashMap<PathBuf, f32>,
    /// flat-field pairs (A, B with the camera turned by 180°)
    pub pairs: Vec<(PathBuf, PathBuf)>,
    /// role per file instead of the automatic classification
    pub roles: HashMap<PathBuf, Role>,
}

fn is_raw(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("cr2" | "cr3" | "crw" | "dng" | "raf" | "arw" | "nef" | "nrw" | "orf" | "rw2" | "pef" | "srw" | "x3f" | "iiq" | "3fr" | "erf" | "mef" | "mrw" | "raw")
    )
}

/// RAW files of a folder (not recursive), sorted.
pub fn files_in_folder(folders: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(folders).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_raw(p)).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Area-preserving downscale by an integer factor (box filter): longest edge at most `target`.
fn downscaled(px: &[[f32; 3]], w: usize, h: usize, target: usize) -> (Vec<[f32; 3]>, usize, usize) {
    let n = w.max(h).div_ceil(target).max(1);
    let (sw, sh) = (w / n, h / n);
    let mut small = vec![[0f32; 3]; sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            let mut s = [0f32; 3];
            for dy in 0..n {
                for dx in 0..n {
                    let p = px[(y * n + dy) * w + x * n + dx];
                    for c in 0..3 {
                        s[c] += p[c];
                    }
                }
            }
            small[y * sw + x] = [s[0] / (n * n) as f32, s[1] / (n * n) as f32, s[2] / (n * n) as f32];
        }
    }
    (small, sw, sh)
}

/// Downscaled (box filter) and as sRGB8 for the preview.
fn preview_img(px: &[[f32; 3]], w: usize, h: usize, k: &[f64; 3], target: usize) -> (usize, usize, Vec<u8>, Vec<u8>) {
    let n = w.max(h).div_ceil(target).max(1);
    let (sw, sh) = (w / n, h / n);
    let mut small = vec![[0f32; 3]; sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            let mut s = [0f32; 3];
            for dy in 0..n {
                for dx in 0..n {
                    let p = px[(y * n + dy) * w + x * n + dx];
                    for c in 0..3 {
                        s[c] += p[c];
                    }
                }
            }
            small[y * sw + x] = [s[0] / (n * n) as f32, s[1] / (n * n) as f32, s[2] / (n * n) as f32];
        }
    }
    // brightness: 99th percentile of the green channel to 1
    let mut gr: Vec<f32> = small.iter().step_by(5).map(|p| p[1]).collect();
    gr.sort_by(f32::total_cmp);
    let high = gr.get(gr.len() * 99 / 100).copied().unwrap_or(1.0).max(1e-4);
    let encode = |v: &[[f32; 3]]| -> Vec<u8> {
        v.iter()
            .flat_map(|p| {
                [0, 1, 2].map(|c| {
                    let l = (p[c] / high).clamp(0.0, 1.0);
                    (crate::srgb_oetf(l) * 255.0 + 0.5) as u8
                })
            })
            .collect()
    };
    let before = encode(&small);
    let b = MeasureImage { width: sw, height: sh, px: &small };
    let after_px = measure::undistort_img(&b, k);
    let after = encode(&after_px);
    (sw, sh, before, after)
}

/// Rough check for a uniform surface: hardly any edges, brightness in a usable range, little spread
/// over the angle (a gradient across the surface is rejected).
fn is_flat_field(b: &MeasureImage, chain_count: usize) -> Result<VignetteMeasurement, String> {
    if chain_count > 4 {
        return Err("zu viele Kanten für eine gleichmäßige Fläche".into());
    }
    let mean: f64 = b.px.iter().step_by(97).map(|p| p[1] as f64).sum::<f64>() / b.px.iter().step_by(97).count().max(1) as f64;
    if mean < 0.08 {
        return Err("Fläche zu dunkel (mindestens etwa ein Drittel der Helligkeit nötig)".into());
    }
    let blown = b.px.iter().step_by(97).filter(|p| p[1] > 0.985).count() as f64 / b.px.iter().step_by(97).count().max(1) as f64;
    if blown > 0.02 {
        return Err("Fläche überbelichtet (Lichter ausgebrannt)".into());
    }
    let v = measure::estimate_vignette(b).ok_or("Vignettierung nicht ableitbar")?;
    if v.angle_spread > 0.07 {
        return Err(format!("Fläche nicht gleichmäßig genug (Streuung {:.0} % entlang der Ringe)", v.angle_spread * 100.0));
    }
    Ok(v)
}

fn round1(v: f32) -> i64 {
    (v * 10.0).round() as i64
}

/// Measure a folder of test shots (RAW through rawler). `progress(done, total, text)` reports each step;
/// `aborted` aborts. Error: nothing usable found.
pub fn measure_img(files_found: &[PathBuf], progress: &dyn Fn(usize, usize, &str), aborted: &AtomicBool) -> Result<Outcome, String> {
    measure_with(files_found, &RawSource, &MeasureHints::default(), progress, aborted)
}

/// One flat-field shot collected for the evaluation.
struct FlatRec {
    path: PathBuf,
    name: String,
    f: Option<f32>,
    n: Option<f32>,
    vignette: Result<VignetteMeasurement, String>,
    blocks: FlatBlocks,
}

/// Like [`measure_img`] with any source of shots and the user's hints.
pub fn measure_with(files_found: &[PathBuf], src: &dyn ShotSource, hints: &MeasureHints, progress: &dyn Fn(usize, usize, &str), aborted: &AtomicBool) -> Result<Outcome, String> {
    if files_found.is_empty() {
        return Err("keine RAW-Dateien im gewählten Ordner".into());
    }
    struct GridShot {
        name: String,
        f: Option<f32>,
        chains: Vec<Vec<[f32; 2]>>,
        width: usize,
        height: usize,
        lateral_ca: Option<LateralCa>,
        preview_source: Option<(Vec<[f32; 3]>, usize, usize)>,
    }
    let mut maker = String::new();
    let mut model = String::new();
    let mut lens: Option<String> = None;
    let mut shots: Vec<Shot> = Vec::new();
    let mut grid: Vec<GridShot> = Vec::new();
    let mut flat_fields: Vec<FlatRec> = Vec::new();
    let mut sensor = SensorKind::Unknown;
    let rotated = |p: &PathBuf| hints.pairs.iter().any(|(_, b)| b == p);
    let mut notes: Vec<String> = Vec::new();
    let total = files_found.len();
    for (i, path) in files_found.iter().enumerate() {
        if aborted.load(Ordering::Relaxed) {
            return Err("abgebrochen".into());
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        progress(i, total, &name);
        let basis = match src.open(path) {
            Ok(r) => r,
            Err(e) => {
                shots.push(Shot { path: path.clone(), name, focal_length: None, aperture: None, role: Role::Unusable(e), rotated: rotated(path) });
                continue;
            }
        };
        let (h, m, o) = (basis.maker.clone(), basis.model.clone(), basis.lens.clone());
        if maker.is_empty() {
            (maker, model, lens) = (h.clone(), m.clone(), o.clone());
            sensor = basis.sensor;
        }
        let (f, n) = (basis.focal_length.or(hints.focal_length), basis.aperture.or(hints.apertures.get(path).copied()));
        if (h.as_str(), m.as_str(), &o) != (maker.as_str(), model.as_str(), &lens) {
            shots.push(Shot {
                path: path.clone(),
                name,
                focal_length: f,
                aperture: n,
                role: Role::Unusable(format!("andere Kamera oder anderes Objektiv ({} {} · {})", h, m, o.as_deref().unwrap_or("?"))),
                rotated: rotated(path),
            });
            continue;
        }
        let b = MeasureImage { width: basis.width, height: basis.height, px: &basis.px };
        let forced = hints.roles.get(path);
        let chains = if forced == Some(&Role::FlatField) { Vec::new() } else { measure::find_chains(&b, 3200) };
        let points: usize = chains.iter().map(|k| k.len()).sum();
        let role = if forced == Some(&Role::Grid) || (forced.is_none() && chains.len() >= 12 && points >= 2500) {
            let q = measure::estimate_lateral_ca(&b, &chains);
            let preview_source = if grid.iter().all(|g| g.f != f) { Some(downscaled(&basis.px, basis.width, basis.height, 1800)) } else { None };
            grid.push(GridShot { name: name.clone(), f, chains, width: basis.width, height: basis.height, lateral_ca: q, preview_source });
            Role::Grid
        } else {
            match is_flat_field(&b, chains.len()) {
                Ok(v) => {
                    let blocks = flatfield::blocks(&FlatShot { width: basis.width, height: basis.height, px: &basis.px, saturation: basis.saturation });
                    flat_fields.push(FlatRec { path: path.clone(), name: name.clone(), f, n, vignette: Ok(v), blocks });
                    Role::FlatField
                }
                Err(reason) => {
                    let g = if chains.len() > 4 { format!("zu wenige gerade Linien ({} Linien; für ein Gitter sind mindestens 12 nötig, sonst: {reason})", chains.len()) } else { reason };
                    Role::Unusable(g)
                }
            }
        };
        shots.push(Shot { path: path.clone(), name, focal_length: f, aperture: n, role, rotated: rotated(path) });
    }
    progress(total, total, "");
    if maker.is_empty() {
        return Err("keine der Dateien ließ sich lesen".into());
    }
    let (crop, crop_known) = match Database::built_in().camera(&maker, &model) {
        Some(k) => (k.crop as f64, true),
        None => {
            notes.push(format!("{maker} {model}: Sensorgröße nicht in der Datenbank – Formatfaktor 1 angenommen"));
            (1.0, false)
        }
    };
    // geometry per focal length
    let mut geo: Vec<GeoGroup> = Vec::new();
    let mut cache_key: Vec<Option<i64>> = grid.iter().map(|g| g.f.map(round1)).collect();
    cache_key.sort();
    cache_key.dedup();
    for s in cache_key {
        let members: Vec<&GridShot> = grid.iter().filter(|g| g.f.map(round1) == s).collect();
        let (w, h) = (members[0].width, members[0].height);
        let members: Vec<&GridShot> = members.into_iter().filter(|g| (g.width, g.height) == (w, h)).collect();
        // With several shots at the same focal length: fit each one individually and only count those whose curve matches the
        // common (median) curve – subjects with crooked "lines" drop out this way.
        let (members, rejected) = consensus(members, w, h, |g| &g.chains, |g| &g.name);
        let all: Vec<Vec<[f32; 2]>> = members.iter().flat_map(|g| g.chains.iter().cloned()).collect();
        let dist_fit = measure::estimate_distortion(&all, w, h);
        // lateral CA: average the coefficients of the shots weighted by point count
        let mut qs = LateralCa { red: [0.0; 3], blue: [0.0; 3], red_edge: 0.0, blue_edge: 0.0, points: 0 };
        let mut weight_sum = 0.0;
        for q in members.iter().filter_map(|g| g.lateral_ca.as_ref()) {
            let p = q.points as f64;
            for j in 0..3 {
                qs.red[j] += q.red[j] * p;
                qs.blue[j] += q.blue[j] * p;
            }
            qs.red_edge += q.red_edge * p;
            qs.blue_edge += q.blue_edge * p;
            qs.points += q.points;
            weight_sum += p;
        }
        let lateral_ca = (weight_sum > 0.0).then(|| {
            for j in 0..3 {
                qs.red[j] /= weight_sum;
                qs.blue[j] /= weight_sum;
            }
            qs.red_edge /= weight_sum;
            qs.blue_edge /= weight_sum;
            qs
        });
        let preview = match (&dist_fit, members.iter().find_map(|g| g.preview_source.as_ref())) {
            (Ok(v), Some((px, bw, bh))) => Some(preview_img(px, *bw, *bh, &v.k, 900)),
            _ => None,
        };
        geo.push(GeoGroup { focal_length: s.map(|v| v as f64 / 10.0), images: members.iter().map(|g| g.name.clone()).collect(), rejected, distortion: dist_fit, lateral_ca, preview });
    }
    // vignetting per focal length and aperture (third stops)
    let mut vig: Vec<VigGroup> = Vec::new();
    let third = |n: Option<f32>| n.map(|n| (2.0 * (n.max(0.5) as f64).log2() * 3.0).round() as i64);
    let mut cast: Vec<CastGroup> = Vec::new();
    let mut vs: Vec<(Option<i64>, Option<i64>)> = flat_fields.iter().map(|r| (r.f.map(round1), third(r.n))).collect();
    vs.sort();
    vs.dedup();
    for (sf, sn) in vs {
        let members: Vec<&FlatRec> = flat_fields.iter().filter(|r| r.f.map(round1) == sf && third(r.n) == sn).collect();
        let is_valid: Vec<&VignetteMeasurement> = members.iter().filter_map(|m| m.vignette.as_ref().ok()).collect();
        let mut a = [0.0; 3];
        for g in &is_valid {
            for j in 0..3 {
                a[j] += g.a[j] / is_valid.len() as f64;
            }
        }
        let v = if is_valid.is_empty() {
            Err("keine brauchbare Fläche".to_string())
        } else {
            let r = is_valid.iter().map(|g| g.rest).sum::<f64>() / is_valid.len() as f64;
            let ws = is_valid.iter().map(|g| g.angle_spread).sum::<f64>() / is_valid.len() as f64;
            Ok(VignetteMeasurement { a, rest: r, angle_spread: ws, corner: is_valid.iter().map(|g| g.corner).sum::<f64>() / is_valid.len() as f64 })
        };
        let aperture = members.iter().find_map(|m| m.n).map(|n| n as f64);
        vig.push(VigGroup { focal_length: sf.map(|v| v as f64 / 10.0), aperture, images: members.iter().map(|m| m.name.clone()).collect(), vignette: v });
        // colour cast: a pair given by the user, otherwise the first shot alone
        let pair = hints.pairs.iter().find_map(|(a, b)| Some((members.iter().find(|m| &m.path == a)?, members.iter().find(|m| &m.path == b)?)));
        let (first, second) = match pair {
            Some((a, b)) => (*a, Some(*b)),
            None => (members[0], None),
        };
        let result = flatfield::check_blocks(&first.blocks)
            .map_err(|e| format!("{}: {e}", first.name))
            .and_then(|_| match second {
                Some(b) => flatfield::check_blocks(&b.blocks).map_err(|e| format!("{}: {e}", b.name)),
                None => Ok(()),
            })
            .and_then(|_| flatfield::measure_blocks(&first.blocks, second.map(|b| &b.blocks)));
        let images = std::iter::once(first.name.clone()).chain(second.map(|b| b.name.clone())).collect();
        cast.push(CastGroup { focal_length: sf.map(|v| v as f64 / 10.0), aperture, images, cast: result });
    }
    // notes
    if geo.is_empty() && vig.is_empty() {
        return Err(format!(
            "keine brauchbare Aufnahme: {}",
            shots.iter().filter_map(|a| if let Role::Unusable(g) = &a.role { Some(format!("{}: {g}", a.name)) } else { None }).take(3).collect::<Vec<_>>().join("; ")
        ));
    }
    if geo.iter().filter(|g| g.distortion.is_ok()).count() == 0 {
        notes.push("Verzeichnung konnte nicht gemessen werden – Gitter oder gerade Linien über das ganze Bild fotografieren.".into());
    }
    for g in &geo {
        if let Ok(v) = &g.distortion {
            if v.spread < 0.7 {
                notes.push(format!("{}: Linien nur in {:.0} % des Bildes – Ecken und Ränder fehlen", focal_length_text(g.focal_length), v.spread * 100.0));
            }
            if v.coverage < 0.8 {
                notes.push(format!("{}: Linien reichen nur bis {:.0} % des Bildradius – das Profil ist außerhalb unsicher", focal_length_text(g.focal_length), v.coverage * 100.0));
            }
        }
    }
    // build the profile
    let focal_ok = |f: Option<f64>| f.unwrap_or(0.0);
    let geo_m: Vec<GeoMeasurement> = geo
        .iter()
        .filter_map(|g| match &g.distortion {
            Ok(v) => Some(GeoMeasurement { focal_length: focal_ok(g.focal_length), aperture: 8.0, distortion: v.clone(), lateral_ca: g.lateral_ca.clone() }),
            Err(_) => None,
        })
        .collect();
    let vig_m: Vec<VigMeasurement> = vig
        .iter()
        .filter_map(|g| match &g.vignette {
            Ok(v) => Some(VigMeasurement { focal_length: focal_ok(g.focal_length), aperture: g.aperture.unwrap_or(8.0), vignette: v.clone() }),
            Err(_) => None,
        })
        .collect();
    let profile = (!geo_m.is_empty() || !vig_m.is_empty()).then(|| {
        let name = match hints.lens_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => format!("{} {} {}", maker, model, lens.as_deref().unwrap_or("")).trim().to_string(),
        };
        measure::build_profile(&format!("{name} (graineo-Messung)"), crop, |f| if f > 0.0 { f * crop / 36.0 } else { 1.0 }, &geo_m, &vig_m)
    });
    let quality = quality_of(&geo, &vig, &cast);
    let name = hints.lens_name.clone().filter(|n| !n.trim().is_empty());
    Ok(Outcome { maker, model, lens, crop, crop_known, shots, geo, vig, notes, profile, cast, quality, sensor, name })
}

/// Quality per part (the weakest group counts) and in total.
fn quality_of(geo: &[GeoGroup], vig: &[VigGroup], cast: &[CastGroup]) -> Option<lensprof::own::Quality> {
    use lensprof::quality as q;
    let mut parts: Vec<(QualityPart, u8)> = Vec::new();
    let mut add = |part: QualityPart, scores: Vec<u8>| {
        if let Some(m) = scores.into_iter().min() {
            parts.push((part, m));
        }
    };
    add(QualityPart::Distortion, geo.iter().filter_map(|g| g.distortion.as_ref().ok()).map(|d| q::distortion(d.rms_after, d.spread)).collect());
    add(QualityPart::LateralCa, geo.iter().filter_map(|g| g.lateral_ca.as_ref()).map(|c| q::lateral_ca(c.points)).collect());
    add(QualityPart::Vignette, vig.iter().filter_map(|g| g.vignette.as_ref().ok()).map(|v| q::vignette(v.rest, v.angle_spread)).collect());
    add(QualityPart::Cast, cast.iter().filter_map(|g| g.cast.as_ref().ok()).map(|c| c.quality.score).collect());
    q::overall(&parts)
}

/// Check shots at the same focal length for agreement. Returns: the matching shots and the rejected ones
/// (name, reason). Fewer than four individually evaluable shots: all are kept.
fn consensus<'a, T>(members: Vec<&'a T>, w: usize, h: usize, chains: impl Fn(&T) -> &Vec<Vec<[f32; 2]>>, name: impl Fn(&T) -> &String) -> (Vec<&'a T>, Vec<(String, String)>) {
    let rmax = (w as f64).hypot(h as f64) / 2.0 / w.max(h) as f64;
    let curve = |k: &[f64; 3]| -> Vec<f64> { (1..=10).map(|j| measure::ratio_val(k, j as f64 / 10.0 * rmax) - 1.0).collect() };
    let single: Vec<Option<Vec<f64>>> = members.iter().map(|g| measure::estimate_distortion(chains(g), w, h).ok().map(|v| curve(&v.k))).collect();
    if single.iter().filter(|e| e.is_some()).count() < 4 {
        return (members, Vec::new());
    }
    let mut kept: Vec<bool> = single.iter().map(|e| e.is_some()).collect();
    for _ in 0..3 {
        let med: Vec<f64> = (0..10)
            .map(|j| {
                let mut v: Vec<f64> = single.iter().zip(&kept).filter(|(_, b)| **b).filter_map(|(e, _)| e.as_ref().map(|c| c[j])).collect();
                v.sort_by(f64::total_cmp);
                v[v.len() / 2]
            })
            .collect();
        let spacing: Vec<Option<f64>> = single.iter().map(|e| e.as_ref().map(|c| c.iter().zip(&med).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max))).collect();
        let mut ab: Vec<f64> = spacing.iter().flatten().copied().collect();
        ab.sort_by(f64::total_cmp);
        let cutoff = (2.5 * ab[ab.len() / 2]).max(0.004);
        let new_at: Vec<bool> = spacing.iter().map(|a| a.is_some_and(|a| a <= cutoff)).collect();
        if new_at.iter().filter(|b| **b).count() < 3 || new_at == kept {
            break;
        }
        kept = new_at;
    }
    let mut ok = Vec::new();
    let mut rejected = Vec::new();
    for (g, b) in members.into_iter().zip(kept) {
        if b {
            ok.push(g);
        } else {
            rejected.push((name(g).clone(), "weicht von den übrigen Aufnahmen dieser Brennweite ab (Motiv ohne durchgehend gerade Linien?)".to_string()));
        }
    }
    (ok, rejected)
}

/// Evaluate chains of several shots at the same focal length and image size together (with agreement check).
/// Returns: result, used names, rejected (name, reason).
pub fn estimate_jointly(images: &[(String, Vec<Vec<[f32; 2]>>)], w: usize, h: usize) -> (Result<Distortion, String>, Vec<String>, Vec<(String, String)>) {
    let refs: Vec<&(String, Vec<Vec<[f32; 2]>>)> = images.iter().collect();
    let (ok, rejected) = consensus(refs, w, h, |g| &g.1, |g| &g.0);
    let all: Vec<Vec<[f32; 2]>> = ok.iter().flat_map(|g| g.1.iter().cloned()).collect();
    (measure::estimate_distortion(&all, w, h), ok.iter().map(|g| g.0.clone()).collect(), rejected)
}

pub fn focal_length_text(f: Option<f64>) -> String {
    f.map_or("Brennweite unbekannt".to_string(), |f| format!("{f:.0} mm"))
}

/// Save the profile as an own profile (`lens-profiles/own/<id>.json`, format 2, highest priority in the correction).
/// A lens with a name that already has an own profile gets a new version of it (the previous one stays in the
/// history, the automatic choice by lens name keeps finding exactly one profile); otherwise – also for every manual
/// lens without a name – a new profile is created. Returns: path of the file.
pub fn save(e: &Outcome) -> std::io::Result<PathBuf> {
    let profile = e.profile.clone().ok_or_else(|| std::io::Error::other("kein Profil gemessen"))?;
    let lens = e.lens.clone().unwrap_or_default();
    let mut p = lensprof::OwnProfile::new(&e.maker, &e.model, &lens, "graineo-Messung", profile);
    p.sensor = e.sensor;
    p.quality = e.quality.clone();
    if let Some(n) = &e.name {
        p.name = n.clone();
    }
    p.cast = e
        .cast
        .iter()
        .filter_map(|g| {
            let c = g.cast.as_ref().ok()?;
            Some(lensprof::own::CastEntry { focal_length: g.focal_length.unwrap_or(0.0), av: 2.0 * g.aperture.unwrap_or(8.0).max(0.5).log2(), grid: c.cast.clone(), quality: c.quality.clone() })
        })
        .collect();
    let shots = e
        .shots
        .iter()
        .filter_map(|s| {
            let role = match s.role {
                Role::Grid => lensprof::own::ShotRole::Grid,
                Role::FlatField => lensprof::own::ShotRole::FlatField,
                Role::Unusable(_) => return None,
            };
            Some(lensprof::own::ShotRef { name: s.name.clone(), sha256: String::new(), role, focal_length: s.focal_length, aperture: s.aperture, rotated: s.rotated })
        })
        .collect();
    // draft revision: its shots go into the saved version
    p.history.push(lensprof::own::Revision { version: 0, time: 0, action: lensprof::own::RevisionAction::Measured, shots, note: String::new() });
    let existing = (!lens.trim().is_empty()).then(|| lensprof::store::find_auto(&e.maker, &e.model, Some(&lens))).flatten();
    let r = match existing {
        Some(old) => lensprof::store::save_version(lensprof::OwnProfile { id: old.id.clone(), ..p }, lensprof::own::RevisionAction::Measured)?,
        None => lensprof::store::save_new(p)?,
    };
    lensprof::store::path_of(&r.id).ok_or_else(|| std::io::Error::other("kein Benutzerordner"))
}
