//! Lens profiles from the **lensfun** database (`crates/raw/data/lensfun-db.xml.gz`, all of `data/db`
//! merged by `scripts/lensfun-update.py`; data CC BY-SA 3.0, © lensfun contributors; separate data
//! file, see the header of the XML file). Unpacked and parsed once on first use.
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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

const DATA_GZ: &[u8] = include_bytes!("../data/lensfun-db.xml.gz");

/// Built-in database, unpacked and parsed once.
static BUILT_IN: OnceLock<Arc<Database>> = OnceLock::new();

/// Database in use ([`Database::active`]); `None` = not chosen yet. A replaced one is freed once the last user
/// drops its `Arc`.
static ACTIVE: std::sync::RwLock<Option<Arc<Database>>> = std::sync::RwLock::new(None);
/// Counts [`reload`]s, so a load that started before one does not install outdated data.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Why the downloaded database could not be used (set once per failed load, taken by the app for one message).
static LOAD_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The error of the last failed load of the downloaded database, once.
pub fn take_load_error() -> Option<String> {
    LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Choose the database in use again (after an update or a revert); loads it again in the background.
pub fn reload() {
    {
        let mut g = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
        GENERATION.fetch_add(1, Ordering::SeqCst);
        *g = None;
    }
    start_loading();
}

/// A background load is running.
static LOADING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Run at the start of the loader thread (the app sets its thread priority here).
static LOADER_SETUP: OnceLock<fn()> = OnceLock::new();

/// Set what the loader thread runs first (e.g. lowering its priority). Only the first call counts.
pub fn set_loader_setup(f: fn()) {
    let _ = LOADER_SETUP.set(f);
}

/// Unpack and parse the built-in and the database in use on a background thread, unless they are loaded or a
/// load runs already. For the UI thread, which must never parse (≈ 35 ms per database); see
/// [`Database::try_active`].
pub fn start_loading() {
    if Database::loaded().is_some() && BUILT_IN.get().is_some() {
        return;
    }
    if LOADING.swap(true, Ordering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new().name("lensfun-load".into()).spawn(|| {
        if let Some(f) = LOADER_SETUP.get() {
            f();
        }
        let _ = Database::built_in();
        let _ = Database::active();
        LOADING.store(false, Ordering::SeqCst);
        // a reload during the load cleared the result: load once more
        if Database::loaded().is_none() {
            start_loading();
        }
    });
    if started.is_err() {
        LOADING.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub struct Camera {
    pub maker: String,
    /// all maker spellings (incl. the English one)
    pub makers: Vec<String>,
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
    /// declared interchangeable-lens mounts with their compatible mounts; a camera whose mount is
    /// not declared has a fixed lens (compact camera)
    pub mounts: Vec<(String, Vec<String>)>,
    /// state of the data from the file header: lensfun commit and its date (empty if the header names none)
    pub commit: String,
    pub date: String,
}

/// Commit and date from the header comment that `scripts/lensfun-update.py` (and the update in the app) writes:
/// "… data/db, commit <hash>, <YYYY-MM-DD>)".
pub fn header_version(xml: &str) -> (String, String) {
    // byte 2000 may fall inside a multi-byte character: cut at the character boundary before it
    let mut end = xml.len().min(2000);
    while !xml.is_char_boundary(end) {
        end -= 1;
    }
    let head = &xml[..end];
    let Some(i) = head.find("commit ") else { return (String::new(), String::new()) };
    let rest = &head[i + 7..];
    let commit: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    let date: String = rest[commit.len()..].trim_start_matches([',', ' ']).chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    (commit, date)
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
        let mut mounts = Vec::new();
        for n in root.children().filter(|c| c.is_element()) {
            if n.has_tag_name("mount") {
                if let Some(name) = text(n, "name") {
                    mounts.push((name, all(n, "compat")));
                }
            } else if n.has_tag_name("camera") {
                cameras.push(Camera {
                    maker: text(n, "maker").unwrap_or_default(),
                    makers: all(n, "maker"),
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
        let (commit, date) = header_version(xml);
        Ok(Self { cameras, lenses, mounts, commit, date })
    }

    /// Unpack the compressed built-in database.
    pub fn built_in_xml() -> Option<String> {
        use std::io::Read;
        let mut s = String::with_capacity(5 << 20);
        flate2::read::GzDecoder::new(DATA_GZ).read_to_string(&mut s).ok()?;
        Some(s)
    }

    /// Built-in database (unpacked and parsed once, on first use).
    pub fn built_in() -> &'static Database {
        Self::built_in_arc()
    }

    fn built_in_arc() -> &'static Arc<Database> {
        BUILT_IN.get_or_init(|| {
            let empty = || Database { cameras: Vec::new(), lenses: Vec::new(), mounts: Vec::new(), commit: String::new(), date: String::new() };
            Arc::new(Self::built_in_xml().and_then(|x| Database::load(&x).ok()).unwrap_or_else(empty))
        })
    }

    /// The database in use: the one downloaded with "Objektivprofile aktualisieren" ([`crate::lensfun_update`]) if
    /// present and readable, otherwise the built-in one. Read once; [`reload`] after an update or a revert.
    ///
    /// Loading happens outside the lock, so readers are never held up by it; a load overtaken by a [`reload`]
    /// is used by its caller but not kept.
    pub fn active() -> Arc<Database> {
        if let Some(d) = Self::loaded() {
            return d;
        }
        let generation = GENERATION.load(Ordering::SeqCst);
        let d = Self::downloaded().map(Arc::new).unwrap_or_else(|| Self::built_in_arc().clone());
        let mut g = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some(x) => x.clone(),
            None => {
                if GENERATION.load(Ordering::SeqCst) == generation {
                    *g = Some(d.clone());
                }
                d
            }
        }
    }

    /// The database in use if it is loaded; otherwise starts loading it in the background ([`start_loading`]) and
    /// returns `None`. For the UI thread.
    pub fn try_active() -> Option<Arc<Database>> {
        let d = Self::loaded();
        if d.is_none() {
            start_loading();
        }
        d
    }

    /// The built-in database if it is parsed already; otherwise starts loading and returns `None`.
    pub fn try_built_in() -> Option<&'static Database> {
        let d = BUILT_IN.get().map(|d| &**d);
        if d.is_none() {
            start_loading();
        }
        d
    }

    /// The database in use if it is loaded already (never loads).
    pub fn loaded() -> Option<Arc<Database>> {
        ACTIVE.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Is this the built-in database?
    pub fn is_built_in(&self) -> bool {
        BUILT_IN.get().is_some_and(|b| std::ptr::eq(&**b, self))
    }

    /// The downloaded database, if present and usable. A present but unusable file is reported once
    /// ([`take_load_error`]) and the built-in database is used; reading it never panics the app.
    pub fn downloaded() -> Option<Database> {
        let f = crate::lensfun_update::file().filter(|f| f.exists())?;
        let r = std::panic::catch_unwind(|| Self::read_downloaded(&f)).unwrap_or_else(|_| Err("reader panicked".into()));
        match r {
            Ok(d) => Some(d),
            Err(e) => {
                *LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{}: {e}", f.display()));
                None
            }
        }
    }

    fn read_downloaded(f: &std::path::Path) -> Result<Database, String> {
        use std::io::Read;
        let gz = std::fs::read(f).map_err(|e| e.to_string())?;
        let mut s = String::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut s).map_err(|e| e.to_string())?;
        crate::lensfun_update::check(&s)
    }

    /// Camera by EXIF make/model; among several entries with the same model name the one whose
    /// maker matches wins.
    pub fn camera(&self, maker: &str, model: &str) -> Option<&Camera> {
        let m = norm(model, maker);
        let mut hits = self.cameras.iter().filter(|k| k.models.iter().any(|km| norm(km, &k.maker) == m || norm(km, maker) == m));
        let first = hits.next()?;
        let want = first_word(maker);
        if want.is_empty() || first.makers.iter().any(|x| first_word(x) == want) {
            return Some(first);
        }
        Some(hits.find(|k| k.makers.iter().any(|x| first_word(x) == want)).unwrap_or(first))
    }

    /// Mounts whose lenses fit a camera with mount `m` (the mount itself and its compatible ones).
    fn compatible<'a>(&'a self, m: &'a str) -> Vec<&'a str> {
        let mut v = vec![m];
        if let Some((_, c)) = self.mounts.iter().find(|(n, _)| n == m) {
            v.extend(c.iter().map(String::as_str));
        }
        v
    }

    /// Matching lens entries: for compact cameras via the mount (without converter lenses),
    /// otherwise via the EXIF lens name(s) (`lens_exif`, alternatives in order). Only calibrations with
    /// crop factor ≤ that of the camera (otherwise the data does not cover the image corners); sorted
    /// by crop factor descending.
    ///
    /// Interchangeable lenses (every mount lensfun declares with `<mount>`, plus its `<compat>`
    /// mounts): first an exact match of the normalized name on a compatible mount (then on any
    /// mount, e.g. adapted lenses), then a fuzzy match in the spirit of lensfun's word comparison:
    /// same focal range and maximum aperture, best word overlap (brand aliases such as
    /// Zuiko = Olympus), fewest extra words, e.g. EXIF "OLYMPUS M.12-40mm F2.8" →
    /// "Olympus M.Zuiko Digital ED 12-40mm f/2.8 Pro".
    pub fn lenses(&self, camera: &Camera, lens_exif: &[&str]) -> Vec<&Lens> {
        let is_fixed = !self.mounts.iter().any(|(n, _)| *n == camera.mount);
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
            let compatible = self.compatible(&camera.mount);
            let fits_crop = |o: &&Lens| o.crop <= camera.crop * 1.01;
            let fits = |o: &&Lens| fits_crop(o) && o.mounts.iter().any(|m| compatible.contains(&m.as_str()));
            let mut found = Vec::new();
            for name in lens_exif {
                let n = norm_lens(name);
                if n.is_empty() || teleconverter(name).is_some() {
                    continue;
                }
                let exact = |o: &&Lens| o.models.iter().any(|m| norm_lens(m) == n);
                found = self.lenses.iter().filter(fits).filter(exact).collect();
                if found.is_empty() {
                    found = self.lenses.iter().filter(fits_crop).filter(exact).collect();
                }
                if found.is_empty() {
                    found = fuzzy(name, self.lenses.iter().filter(fits));
                }
                if !found.is_empty() {
                    break;
                }
            }
            found
        };
        v.retain(|o| o.crop <= camera.crop * 1.01);
        v.sort_by(|a, b| b.crop.total_cmp(&a.crop));
        v
    }
}

impl Database {
    /// Entries for a manual choice: every lens name (first model name) once, with its mounts, sorted by name.
    pub fn lens_names(&self) -> Vec<(String, Vec<String>)> {
        let mut v: Vec<(String, Vec<String>)> = Vec::new();
        for o in &self.lenses {
            let Some(n) = o.models.first() else { continue };
            if o.distortion.is_empty() && o.vignette.is_empty() && o.lateral_ca.is_empty() {
                continue;
            }
            match v.iter_mut().find(|(m, _)| m == n) {
                Some((_, mounts)) => {
                    for m in &o.mounts {
                        if !mounts.contains(m) {
                            mounts.push(m.clone());
                        }
                    }
                }
                None => v.push((n.clone(), o.mounts.clone())),
            }
        }
        v.sort_by_key(|(n, _)| n.to_lowercase());
        v
    }

    /// All entries of a lens chosen by name (first model name; several calibrations with different crop factors),
    /// sorted by crop factor descending.
    pub fn lens_by_name(&self, name: &str) -> Vec<&Lens> {
        let mut v: Vec<&Lens> = self.lenses.iter().filter(|o| o.models.first().is_some_and(|m| m == name)).collect();
        v.sort_by(|a, b| b.crop.total_cmp(&a.crop));
        v
    }

    /// Entries matching an EXIF lens name on any mount (exact, otherwise fuzzy), e.g. to tell "other mount" apart.
    pub fn lenses_any_mount(&self, name: &str) -> Vec<&Lens> {
        let n = norm_lens(name);
        if n.is_empty() {
            return Vec::new();
        }
        let exact: Vec<&Lens> = self.lenses.iter().filter(|o| o.models.iter().any(|m| norm_lens(m) == n)).collect();
        if !exact.is_empty() {
            return exact;
        }
        fuzzy(name, self.lenses.iter())
    }

    /// Does a lens with these mounts fit the camera without an adapter (same or compatible mount)? A camera unknown
    /// to lensfun counts as fitting (nothing to warn about).
    pub fn mount_fits(&self, camera: Option<&Camera>, mounts: &[String]) -> bool {
        let Some(c) = camera else { return true };
        let ok = self.compatible(&c.mount);
        mounts.iter().any(|m| ok.contains(&m.as_str()))
    }
}

/// Teleconverter in an EXIF lens name ("… + 1.4x", "TC-14", "MC-20", "EF 1.4x III", "Extender", "with 2x"): its
/// factor (0.0 if the name says converter without a factor). lensfun has no calibrations of lens + converter, and
/// the lens alone does not describe the combination, so such names get no automatic profile.
pub fn teleconverter(name: &str) -> Option<f32> {
    let l = name.to_ascii_lowercase();
    let factor_after = |i: usize| -> Option<f32> {
        let rest = l.get(i..)?.trim_start().trim_start_matches('-');
        let t: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let f: f32 = t.parse().ok()?;
        (rest[t.len()..].starts_with('x') && (1.2..=3.0).contains(&f)).then_some(f)
    };
    // "+ 1.4x", "with 2x"
    for key in ["+", " with "] {
        if let Some(i) = l.find(key) {
            if let Some(f) = factor_after(i + key.len()) {
                return Some(f);
            }
        }
    }
    // Nikon TC-14/17/20, OM System MC-14/20, "TC-1.4x" (not a built-in "TC1.25x": the lens name never changes)
    let b = l.as_bytes();
    for (i, _) in l.match_indices("tc").chain(l.match_indices("mc")) {
        if i > 0 && b[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let j = i + 2 + usize::from(b.get(i + 2) == Some(&b'-'));
        let digits: String = l[j..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let f = match digits.as_str() {
            "14" | "1.4" => 1.4,
            "17" | "1.7" => 1.7,
            "20" | "2.0" | "2" => 2.0,
            _ => continue,
        };
        return Some(f);
    }
    // Canon "Extender EF 1.4x III", "RF 2x"
    for key in ["extender ef", "extender rf", "ef ", "rf "] {
        if let Some(i) = l.find(key) {
            if let Some(f) = factor_after(i + key.len()) {
                return Some(f);
            }
        }
    }
    (l.contains("extender") || l.contains("teleconverter") || l.contains("tele converter")).then_some(0.0)
}

/// Lens name split into focal range, maximum aperture and words.
#[derive(Debug, PartialEq)]
struct LensKey {
    focal: (f32, f32),
    aperture: Option<f32>,
    words: Vec<String>,
}

/// Brand aliases: words that name the same maker/series.
fn alias(w: &str) -> &str {
    match w {
        "zuiko" | "om" => "olympus",
        "lumix" => "panasonic",
        "nikkor" => "nikon",
        _ => w,
    }
}

fn lens_key(name: &str) -> Option<LensKey> {
    let name = match name.find('[') {
        Some(i) => &name[..i],
        None => name,
    };
    let l = name.to_ascii_lowercase();
    // focal length: the number directly before "mm" ("12-40mm", "M.12-40mm", "XF23mm"); maker codes
    // without "mm" (Sigma "Art 019", Tamron "A065") are never taken as focal length
    let mm = l.find("mm")?;
    // (after a non-ASCII character such as "–" the next character starts more than one byte later)
    let start = l[..mm].rfind(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).map_or(0, |i| i + l[i..].chars().next().map_or(1, char::len_utf8));
    let fr = l[start..mm].trim_matches(|c| c == '.' || c == '-');
    let mut it = fr.split('-').filter_map(|x| x.parse::<f32>().ok());
    let f0 = it.next()?;
    let focal = (f0, it.next().unwrap_or(f0));
    // maximum aperture: "f/2.8", "F2.8", "f/4.0-5.6", "mmF2", "1:2.8"
    let b = l.as_bytes();
    let mut aperture = None;
    for i in 0..b.len() {
        let boundary = i == 0 || !b[i - 1].is_ascii_alphanumeric() || (i >= 2 && &b[i - 2..i] == b"mm");
        let after = if b[i] == b'f' && boundary {
            Some(if b.get(i + 1) == Some(&b'/') { i + 2 } else { i + 1 })
        } else if b[i] == b':' && i > 0 && b[i - 1] == b'1' && (i < 2 || !b[i - 2].is_ascii_alphanumeric()) {
            Some(i + 1)
        } else {
            None
        };
        if let Some(j) = after {
            let num: String = l[j.min(l.len())..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if let Ok(a) = num.trim_end_matches('.').parse::<f32>() {
                aperture = Some(a);
                break;
            }
        }
    }
    // words: letter runs only (digits split them: "xf23mmf2" → "xf"), without "mm"/"f"
    let mut words: Vec<String> = Vec::new();
    for w in l.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| !w.is_empty() && *w != "mm" && *w != "f") {
        let w = alias(w).to_string();
        if !words.contains(&w) {
            words.push(w);
        }
    }
    Some(LensKey { focal, aperture, words })
}

/// Fuzzy lens search: entries with the same focal range (and maximum aperture, if known) and at least
/// one common word; best = most common words, then fewest extra words; ties are all returned
/// (database order).
fn fuzzy<'a>(name: &str, lenses: impl Iterator<Item = &'a Lens>) -> Vec<&'a Lens> {
    let Some(k) = lens_key(name) else { return Vec::new() };
    let same = |a: f32, b: f32| (a - b).abs() < 0.15;
    let mut best: Option<(i32, Vec<&Lens>)> = None;
    for o in lenses {
        let score = o
            .models
            .iter()
            .filter_map(|m| lens_key(m))
            .filter(|c| same(c.focal.0, k.focal.0) && same(c.focal.1, k.focal.1))
            .filter(|c| match (c.aperture, k.aperture) {
                (Some(a), Some(b)) => (a - b).abs() < 0.06,
                _ => true,
            })
            .filter_map(|c| {
                let common = c.words.iter().filter(|w| k.words.contains(w)).count() as i32;
                let extra = c.words.len() as i32 - common;
                (common > 0).then_some(common * 100 - extra)
            })
            .max();
        if let Some(s) = score {
            match &mut best {
                Some((b, v)) if *b == s => v.push(o),
                Some((b, _)) if *b > s => {}
                _ => best = Some((s, vec![o])),
            }
        }
    }
    best.map(|(_, v)| v).unwrap_or_default()
}

fn first_word(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| !w.is_empty()).unwrap_or("").to_ascii_lowercase()
}

/// Make a model name comparable: lowercase, without maker names, without spaces.
fn norm(s: &str, maker: &str) -> String {
    let mut l = s.to_ascii_lowercase();
    for h in [maker.to_ascii_lowercase().as_str(), "canon", "ricoh", "fujifilm", "sony", "leica", "nikon", "olympus", "om digital solutions", "om system", "panasonic", "pentax", "sigma", "hasselblad", "imaging company, ltd."] {
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
    let l = l.to_ascii_lowercase().replace("canon", "").replace("mkii", "ii").replace("mark ii", "ii").replace("f/", "f");
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
    fn header_version_cuts_at_a_char_boundary() {
        // "ä" (2 bytes) and "€" (3 bytes) straddling byte 2000 must not panic
        for pad in 1995..2001 {
            let x = format!("<!-- commit abc1, 2026-01-02) -->{}ä€ä tail", "x".repeat(pad - 34));
            assert_eq!(header_version(&x), ("abc1".into(), "2026-01-02".into()));
        }
        let x = "€".repeat(1000);
        assert_eq!(header_version(&x), (String::new(), String::new()));
    }

    #[test]
    fn database_loads_and_finds() {
        let db = Database::built_in();
        assert!(db.cameras.len() > 10, "{} cameras", db.cameras.len());
        let k = db.camera("Ricoh", "GR III").expect("GR III");
        assert_eq!(k.mount, "ricohGRIII");
        let o = db.lenses(k, &[]);
        assert!(!o.is_empty());
        let p = Profile::determine(k, &o, Some(18.3), Some(2.8)).expect("profile");
        assert!(p.vignette.is_some() && p.distortion.is_some());
        // EOS RP with EF 40 mm
        let k = db.camera("Canon", "EOS RP").expect("EOS RP");
        let o = db.lenses(k, &["EF40mm f/2.8 STM"]);
        assert!(!o.is_empty(), "EF40");
        let o = db.lenses(k, &["RF35mm F1.8 MACRO IS STM"]);
        assert!(!o.is_empty(), "RF35");
        assert!(db.camera("Ricoh", "GR II").is_some());
        assert!(db.camera("Sony", "DSC-RX100M3").is_some());
        assert!(db.camera("Leica", "LEICA Q (Typ 116)").is_some());
        assert!(db.camera("Canon", "PowerShot S95").is_some());
    }

    /// Name of the first matching entry for camera `maker`/`model` and EXIF lens names `names`.
    fn first(maker: &str, model: &str, names: &[&str]) -> Option<String> {
        let db = Database::built_in();
        let k = db.camera(maker, model)?;
        db.lenses(k, names).first().map(|o| o.models[0].clone())
    }

    #[test]
    fn real_exif_lens_names() {
        let cases = [
            ("OLYMPUS CORPORATION", "E-M1MarkII", "OLYMPUS M.12-40mm F2.8", "Olympus M.Zuiko Digital ED 12-40mm f/2.8 Pro"),
            ("OLYMPUS CORPORATION", "E-M1MarkIII", "OLYMPUS M.17mm F1.2", "Olympus M.Zuiko Digital ED 17mm f/1.2 Pro"),
            ("OLYMPUS CORPORATION", "E-M1MarkII", "M.Zuiko Digital ED 12-40mm F2.8 Pro", "Olympus M.Zuiko Digital ED 12-40mm f/2.8 Pro"),
            ("OM Digital Solutions", "OM-1", "OLYMPUS M.12-40mm F2.8", "Olympus M.Zuiko Digital ED 12-40mm f/2.8 Pro"),
            ("SONY", "ILCE-7M4", "FE 24-70mm F2.8 GM II", "FE 24-70mm f/2.8 GM II"),
            ("NIKON CORPORATION", "NIKON Z 6_2", "NIKKOR Z 24-70mm f/4 S", "Nikkor Z 24-70mm f/4 S"),
            ("FUJIFILM", "X-T3", "XF23mmF2 R WR", "XF23mmF2 R WR"),
            ("FUJIFILM", "X-T3", "Fujinon XF 23mm F2 R WR", "XF23mmF2 R WR"),
        ];
        for (maker, model, exif, want) in cases {
            assert!(Database::built_in().camera(maker, model).is_some(), "camera {model}");
            assert_eq!(first(maker, model, &[exif]).as_deref(), Some(want), "{model} / {exif}");
        }
        // maker codes are no focal length; a 17 mm f/1.8 does not match the f/1.2
        assert_eq!(lens_key("Sigma 35mm F1.4 DG HSM | Art 012").map(|k| k.focal), Some((35.0, 35.0)));
        // a non-ASCII character right before the focal length must not panic
        assert_eq!(lens_key("Foo –13mm F9").map(|k| k.focal), Some((13.0, 13.0)));
        assert_eq!(lens_key("Tamron 28-75mm F/2.8 Di III RXD (A036)").map(|k| (k.focal, k.aperture)), Some(((28.0, 75.0), Some(2.8))));
        assert_ne!(first("OLYMPUS CORPORATION", "E-M1MarkII", &["OLYMPUS M.17mm F1.8"]).as_deref(), Some("Olympus M.Zuiko Digital ED 17mm f/1.2 Pro"));
        // compact cameras still go via their mount
        assert!(first("Ricoh", "GR II", &[]).is_some());
    }

    #[test]
    fn teleconverters_are_recognized() {
        for (n, f) in [
            ("EF70-200mm f/2.8L IS III USM +1.4x III", 1.4),
            ("NIKKOR Z 100-400mm f/4.5-5.6 VR S + TC-1.4x", 1.4),
            ("AF-S Nikkor 300mm f/4E PF ED VR + TC-14E III", 1.4),
            ("OLYMPUS M.300mm F4.0 + MC-14", 1.4),
            ("Canon Extender EF 2x III", 2.0),
            ("XF100-400mmF4.5-5.6 R LM OIS WR + 1.4x", 1.4),
            ("Sony FE 200-600mm with 2x Teleconverter", 2.0),
        ] {
            assert_eq!(teleconverter(n), Some(f), "{n}");
        }
        for n in ["Laowa 100mm f/2.8 2X Ultra Macro APO", "EF70-200mm f/2.8L IS III USM", "XF16-80mmF4 R OIS WR", "FE 24-70mm F2.8 GM II", "Sigma 150-600mm F5-6.3 DG OS HSM | C", "MC-11", "M.Zuiko Digital ED 150-400mm F4.5 TC1.25x IS PRO"] {
            assert_eq!(teleconverter(n), None, "{n}");
        }
        // a name with converter gets no lensfun entry
        let db = Database::built_in();
        let k = db.camera("Canon", "EOS R5").expect("R5");
        assert!(db.lenses(k, &["EF 70-200mm f/2.8L IS III USM + 1.4x"]).is_empty());
    }

    #[test]
    fn header_names_commit_and_date() {
        let db = Database::built_in();
        assert_eq!(db.commit.len(), 40, "{}", db.commit);
        assert_eq!(db.date.len(), 10, "{}", db.date);
        assert_eq!(header_version("<!-- x (https://github.com/lensfun/lensfun, data/db, commit abc123, 2026-11-17). -->"), ("abc123".into(), "2026-11-17".into()));
    }

    #[test]
    fn try_active_loads_in_the_background() {
        // never blocks; after the background load the same data as the blocking path
        let t = std::time::Instant::now();
        let first = Database::try_active();
        let _ = Database::try_built_in();
        eprintln!("try_active: {:?} (loaded: {})", t.elapsed(), first.is_some());
        assert!(t.elapsed() < std::time::Duration::from_millis(5));
        let mut d = None;
        for _ in 0..500 {
            d = Database::try_active();
            if d.is_some() && Database::try_built_in().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let d = d.expect("loaded in the background");
        assert_eq!(d.commit, Database::active().commit);
        assert!(!d.lenses.is_empty());
    }

    #[test]
    fn parse_time() {
        let t = std::time::Instant::now();
        let x = Database::built_in_xml().expect("unpack");
        let t1 = t.elapsed();
        let db = Database::load(&x).expect("parse");
        eprintln!("lensfun: {} bytes, unpack {:?}, parse {:?}, {} cameras, {} lenses, {} mounts", x.len(), t1, t.elapsed() - t1, db.cameras.len(), db.lenses.len(), db.mounts.len());
        assert!(db.lenses.len() > 1500 && db.cameras.len() > 1000);
    }
}
