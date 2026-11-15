//! Lens profiles from an extract of the **lensfun** database (`crates/raw/data/lensfun-excerpt.xml`,
//! data CC BY-SA 3.0, © lensfun contributors; separate data file, see the header of the XML file).
//!
//! Own reader (no lensfun code). Models and coordinates as in lensfun
//! (`libs/lensfun/modifier.cpp`, comment "About coordinate systems"):
//! - Distortion `ptlens` (r_q = r·(a r³ + b r² + c r + 1 − a − b − c)), `poly3` (r_q = r·(1 − k1 + k1 r²)),
//!   `poly5` (r_q = r·(1 + k1 r² + k2 r⁴)) and lateral CA `poly3` (r_q = r·(b r² + c r + v)) or `linear`
//!   (r_q = k·r): radius in Hugin units, 1 = half the *short* side of the calibration sensor.
//!   r is the corrected (target) radius, r_q the radius in the uncorrected image.
//! - Vignetting `pa`: value_q = value·(1 + k1 r² + k2 r⁴ + k3 r⁶), 1 = half the *diagonal* of the
//!   calibration sensor; corrected by dividing.
//! - Sensor size via the crop factor (`cropfactor`) and the aspect ratio of the calibration.
//! - Interpolation between focal lengths is linear, for vignetting additionally linear over
//!   log₂(aperture); distance: the largest available in each case.

use std::sync::OnceLock;

const DATA: &str = include_str!("../data/lensfun-excerpt.xml");

#[derive(Debug, Clone)]
pub struct Camera {
    pub maker: String,
    pub models: Vec<String>,
    pub mount: String,
    pub crop: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum Distortion {
    PtLens(f32, f32, f32),
    Poly3(f32),
    Poly5(f32, f32),
}

#[derive(Debug, Clone, Copy)]
pub enum LateralCa {
    Linear { kr: f32, kb: f32 },
    /// vr, vb, cr, cb, br, bb
    Poly3([f32; 6]),
}

#[derive(Debug, Clone)]
pub struct Lens {
    pub models: Vec<String>,
    pub mounts: Vec<String>,
    pub crop: f32,
    pub aspect_ratio: f32,
    pub distortion: Vec<(f32, Distortion)>,
    pub lateral_ca: Vec<(f32, LateralCa)>,
    /// (focal length, aperture, distance, [k1, k2, k3])
    pub vignette: Vec<(f32, f32, f32, [f32; 3])>,
}

pub struct Database {
    pub cameras: Vec<Camera>,
    pub lenses: Vec<Lens>,
}

fn number(s: Option<&str>) -> Option<f32> {
    s.and_then(|v| v.trim().parse().ok())
}

fn ratio_val(s: &str) -> Option<f32> {
    if let Some((a, b)) = s.split_once(':') {
        let (a, b): (f32, f32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        return Some(a.max(b) / a.min(b));
    }
    s.trim().parse().ok()
}

impl Database {
    pub fn load(xml: &str) -> Result<Self, String> {
        let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
        let root = doc.root_element();
        let text = |n: roxmltree::Node, name: &str| -> Option<String> {
            n.children().find(|c| c.has_tag_name(name) && c.attribute("lang").is_none()).and_then(|c| c.text()).map(|s| s.trim().to_string())
        };
        let all = |n: roxmltree::Node, name: &str| -> Vec<String> {
            n.children().filter(|c| c.has_tag_name(name)).filter_map(|c| c.text()).map(|s| s.trim().to_string()).collect()
        };
        let mut cameras = Vec::new();
        let mut lenses = Vec::new();
        for n in root.children().filter(|c| c.is_element()) {
            if n.has_tag_name("camera") {
                cameras.push(Camera {
                    maker: text(n, "maker").unwrap_or_default(),
                    models: all(n, "model"),
                    mount: text(n, "mount").unwrap_or_default(),
                    crop: number(text(n, "cropfactor").as_deref()).unwrap_or(1.0),
                });
            } else if n.has_tag_name("lens") {
                let mut o = Lens {
                    models: all(n, "model"),
                    mounts: all(n, "mount"),
                    crop: number(text(n, "cropfactor").as_deref()).unwrap_or(1.0),
                    aspect_ratio: text(n, "aspect-ratio").as_deref().and_then(ratio_val).unwrap_or(1.5),
                    distortion: Vec::new(),
                    lateral_ca: Vec::new(),
                    vignette: Vec::new(),
                };
                for c in n.children().filter(|c| c.has_tag_name("calibration")) {
                    for e in c.children().filter(|e| e.is_element()) {
                        let a = |k: &str| number(e.attribute(k));
                        let f = a("focal").unwrap_or(0.0);
                        match (e.tag_name().name(), e.attribute("model")) {
                            ("distortion", Some("ptlens")) => {
                                o.distortion.push((f, Distortion::PtLens(a("a").unwrap_or(0.0), a("b").unwrap_or(0.0), a("c").unwrap_or(0.0))))
                            }
                            ("distortion", Some("poly3")) => o.distortion.push((f, Distortion::Poly3(a("k1").unwrap_or(0.0)))),
                            ("distortion", Some("poly5")) => o.distortion.push((f, Distortion::Poly5(a("k1").unwrap_or(0.0), a("k2").unwrap_or(0.0)))),
                            ("tca", Some("linear")) => o.lateral_ca.push((f, LateralCa::Linear { kr: a("kr").unwrap_or(1.0), kb: a("kb").unwrap_or(1.0) })),
                            ("tca", Some("poly3")) => o.lateral_ca.push((
                                f,
                                LateralCa::Poly3([
                                    a("vr").unwrap_or(1.0),
                                    a("vb").unwrap_or(1.0),
                                    a("cr").unwrap_or(0.0),
                                    a("cb").unwrap_or(0.0),
                                    a("br").unwrap_or(0.0),
                                    a("bb").unwrap_or(0.0),
                                ]),
                            )),
                            ("vignetting", Some("pa")) => o.vignette.push((
                                f,
                                a("aperture").unwrap_or(8.0),
                                a("distance").unwrap_or(1000.0),
                                [a("k1").unwrap_or(0.0), a("k2").unwrap_or(0.0), a("k3").unwrap_or(0.0)],
                            )),
                            _ => {}
                        }
                    }
                }
                o.distortion.sort_by(|a, b| a.0.total_cmp(&b.0));
                o.lateral_ca.sort_by(|a, b| a.0.total_cmp(&b.0));
                lenses.push(o);
            }
        }
        Ok(Self { cameras, lenses })
    }

    /// Built-in extract (loaded once).
    pub fn built_in() -> &'static Database {
        static DB: OnceLock<Database> = OnceLock::new();
        DB.get_or_init(|| Database::load(DATA).unwrap_or(Database { cameras: Vec::new(), lenses: Vec::new() }))
    }

    /// Camera by EXIF make/model.
    pub fn camera(&self, maker: &str, model: &str) -> Option<&Camera> {
        let m = norm(model, maker);
        self.cameras.iter().find(|k| k.models.iter().any(|km| norm(km, &k.maker) == m || norm(km, maker) == m))
    }

    /// Matching lens entries: for compact cameras via the mount (without converter lenses),
    /// otherwise via the EXIF lens name. Only calibrations with crop factor ≤ that of the camera
    /// (otherwise the data does not cover the image corners); sorted by crop factor descending.
    pub fn lenses(&self, camera: &Camera, lens_exif: Option<&str>) -> Vec<&Lens> {
        let is_fixed = !camera.mount.to_ascii_lowercase().starts_with("canon ef") && !camera.mount.to_ascii_lowercase().starts_with("canon rf");
        let mut v: Vec<&Lens> = if is_fixed {
            self.lenses
                .iter()
                .filter(|o| o.mounts.iter().any(|m| *m == camera.mount))
                .filter(|o| o.models.iter().all(|m| {
                    let l = m.to_ascii_lowercase();
                    !l.contains(" with ") && !l.contains("tcl") && !l.contains("wcl")
                }))
                .collect()
        } else {
            let Some(name) = lens_exif else { return Vec::new() };
            let n = norm_lens(name);
            if n.is_empty() {
                return Vec::new();
            }
            self.lenses.iter().filter(|o| o.models.iter().any(|m| norm_lens(m) == n)).collect()
        };
        v.retain(|o| o.crop <= camera.crop * 1.01);
        v.sort_by(|a, b| b.crop.total_cmp(&a.crop));
        v
    }
}

/// Make a model name comparable: lowercase, without maker names, without spaces.
fn norm(s: &str, maker: &str) -> String {
    let mut l = s.to_ascii_lowercase();
    for h in [maker.to_ascii_lowercase().as_str(), "canon", "ricoh", "fujifilm", "sony", "leica", "imaging company, ltd."] {
        if !h.is_empty() {
            l = l.replace(h, "");
        }
    }
    l.chars().filter(|c| !c.is_whitespace()).collect()
}

fn norm_lens(s: &str) -> String {
    // drop suffixes in square brackets (rawler database, e.g. "[II]")
    let l = match s.find('[') {
        Some(i) => &s[..i],
        None => s,
    };
    let l = l.to_ascii_lowercase().replace("canon", "").replace("mkii", "ii").replace("mark ii", "ii");
    l.chars().filter(|c| !c.is_whitespace()).collect()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Interpolate a value linearly over a sorted list of nodes (key, value),
/// clamped at the ends outside. `mix` blends two values.
fn interpolate<T: Copy>(list: &[(f32, T)], x: f32, mix: impl Fn(T, T, f32) -> Option<T>) -> Option<T> {
    let first = list.first()?;
    if list.len() == 1 || x <= first.0 {
        return Some(first.1);
    }
    let last = list.last()?;
    if x >= last.0 {
        return Some(last.1);
    }
    let i = list.iter().position(|p| p.0 >= x)?;
    let (a, b) = (list[i - 1], list[i]);
    let t = if b.0 > a.0 { (x - a.0) / (b.0 - a.0) } else { 0.0 };
    mix(a.1, b.1, t).or(Some(if t < 0.5 { a.1 } else { b.1 }))
}

/// Everything that follows from the profile for one image (functions in the radius "half image diagonal = 1").
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    /// Image half-diagonal in Hugin units (distortion/lateral CA)
    pub hugin: f32,
    /// Image half-diagonal in vignetting units
    pub vig_unit: f32,
    pub distortion: Option<Distortion>,
    pub lateral_ca: Option<LateralCa>,
    pub vignette: Option<[f32; 3]>,
}

impl Profile {
    /// For the focal length/aperture of the image. `lenses` from [`Database::lenses`].
    pub fn determine(camera: &Camera, lenses: &[&Lens], focal_length: Option<f32>, aperture: Option<f32>) -> Option<Self> {
        // distortion/lateral CA and vignetting can come from different entries
        let geo = lenses.iter().find(|o| !o.distortion.is_empty())?;
        let f = focal_length.unwrap_or_else(|| geo.distortion[0].0);
        let distortion = interpolate(&geo.distortion, f, |a, b, t| match (a, b) {
            (Distortion::PtLens(a0, b0, c0), Distortion::PtLens(a1, b1, c1)) => Some(Distortion::PtLens(lerp(a0, a1, t), lerp(b0, b1, t), lerp(c0, c1, t))),
            (Distortion::Poly3(a0), Distortion::Poly3(a1)) => Some(Distortion::Poly3(lerp(a0, a1, t))),
            (Distortion::Poly5(a0, b0), Distortion::Poly5(a1, b1)) => Some(Distortion::Poly5(lerp(a0, a1, t), lerp(b0, b1, t))),
            _ => None,
        });
        let lateral_ca = interpolate(&geo.lateral_ca, f, |a, b, t| match (a, b) {
            (LateralCa::Poly3(x), LateralCa::Poly3(y)) => Some(LateralCa::Poly3(std::array::from_fn(|i| lerp(x[i], y[i], t)))),
            (LateralCa::Linear { kr: r0, kb: b0 }, LateralCa::Linear { kr: r1, kb: b1 }) => Some(LateralCa::Linear { kr: lerp(r0, r1, t), kb: lerp(b0, b1, t) }),
            _ => None,
        });
        // vignetting: entry with data, interpolate over log₂(aperture) per focal length
        let vig_obj = lenses.iter().find(|o| !o.vignette.is_empty());
        let vignette = vig_obj.and_then(|o| {
            let n = aperture.unwrap_or(8.0).max(0.5).log2();
            let mut focal_lengths: Vec<f32> = o.vignette.iter().map(|v| v.0).collect();
            focal_lengths.sort_by(f32::total_cmp);
            focal_lengths.dedup();
            let per_focal_length: Vec<(f32, [f32; 3])> = focal_lengths
                .iter()
                .filter_map(|&bf| {
                    // largest distance per aperture
                    let mut list: Vec<(f32, [f32; 3])> = Vec::new();
                    let mut entries: Vec<&(f32, f32, f32, [f32; 3])> = o.vignette.iter().filter(|v| v.0 == bf).collect();
                    entries.sort_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)));
                    for e in entries {
                        let k = e.1.max(0.5).log2();
                        if list.last().map(|l| l.0) != Some(k) {
                            list.push((k, e.3));
                        }
                    }
                    interpolate(&list, n, |a, b, t| Some(std::array::from_fn(|i| lerp(a[i], b[i], t)))).map(|k| (bf, k))
                })
                .collect();
            interpolate(&per_focal_length, f, |a, b, t| Some(std::array::from_fn(|i| lerp(a[i], b[i], t))))
        });
        let diag_img = (36f32.hypot(24.0)) / camera.crop / 2.0;
        let hugin_mm = 36f32.hypot(24.0) / geo.crop / geo.aspect_ratio.hypot(1.0) / 2.0;
        let vig_mm = vig_obj.map(|o| 36f32.hypot(24.0) / o.crop / 2.0).unwrap_or(diag_img);
        Some(Self {
            name: geo.models.first().cloned().unwrap_or_default(),
            hugin: diag_img / hugin_mm,
            vig_unit: diag_img / vig_mm,
            distortion,
            lateral_ca,
            vignette,
        })
    }

    /// Ratio source radius/target radius per channel at target radius `r` (1 = half image diagonal).
    pub fn geometry(&self, r: f32) -> [f32; 3] {
        let rh = r * self.hugin;
        let d = match self.distortion {
            Some(Distortion::PtLens(a, b, c)) => a * rh * rh * rh + b * rh * rh + c * rh + 1.0 - a - b - c,
            Some(Distortion::Poly3(k1)) => 1.0 - k1 + k1 * rh * rh,
            Some(Distortion::Poly5(k1, k2)) => 1.0 + k1 * rh * rh + k2 * rh.powi(4),
            None => 1.0,
        };
        // lateral CA acts on the already distorted coordinates (order as in lensfun)
        let rq = rh * d;
        let (fr, fb) = match self.lateral_ca {
            Some(LateralCa::Poly3([vr, vb, cr, cb, br, bb])) => (br * rq * rq + cr * rq + vr, bb * rq * rq + cb * rq + vb),
            Some(LateralCa::Linear { kr, kb }) => (kr, kb),
            None => (1.0, 1.0),
        };
        [d * fr, d, d * fb]
    }

    /// Light falloff (≤ 1) at source radius `r` (1 = half image diagonal).
    pub fn falloff(&self, r: f32) -> f32 {
        match self.vignette {
            Some([k1, k2, k3]) => {
                let r2 = (r * self.vig_unit).powi(2);
                1.0 + r2 * (k1 + r2 * (k2 + r2 * k3))
            }
            None => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_loads_and_finds() {
        let db = Database::built_in();
        assert!(db.cameras.len() > 10, "{} cameras", db.cameras.len());
        let k = db.camera("Ricoh", "GR III").expect("GR III");
        assert_eq!(k.mount, "ricohGRIII");
        let o = db.lenses(k, None);
        assert!(!o.is_empty());
        let p = Profile::determine(k, &o, Some(18.3), Some(2.8)).expect("profile");
        assert!(p.vignette.is_some() && p.distortion.is_some());
        // EOS RP with EF 40 mm
        let k = db.camera("Canon", "EOS RP").expect("EOS RP");
        let o = db.lenses(k, Some("EF40mm f/2.8 STM"));
        assert!(!o.is_empty(), "EF40");
        let o = db.lenses(k, Some("RF35mm F1.8 MACRO IS STM"));
        assert!(!o.is_empty(), "RF35");
        assert!(db.camera("Ricoh", "GR II").is_some());
        assert!(db.camera("Sony", "DSC-RX100M3").is_some());
        assert!(db.camera("Leica", "LEICA Q (Typ 116)").is_some());
        assert!(db.camera("Canon", "PowerShot S95").is_some());
    }
}
