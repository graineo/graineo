//! Adobe lens profiles in the open **LCP** format (XMP/RDF, "Adobe Camera Model"), as
//! shipped with Lightroom/Camera Raw. The profiles are read **only at runtime** from the local
//! Adobe folders (read-only) and never copied or shipped; if they are missing,
//! embedded data or lensfun take over.
//!
//! Own reader following the public description of the model (Adobe Lens Profile Creator,
//! "Adobe Camera Model") and, as a cross-check, the formulas of RawTherapee `rtengine/lcp.cc` (GPL-3):
//! - normalized coordinates `x = (X − cx·W) / (fx·D)`, `D = max(W, H)` of the whole image,
//! - distortion (ideal → captured): `x_q = x·(1 + k1 r² + k2 r⁴ + k3 r⁶) + 2·(p1 y + p2 x)·x + p2·r²`
//!   (y correspondingly with p1), applied as a backward mapping target → source,
//! - lateral CA: the models "ChromaticRedGreen"/"ChromaticBlueGreen" distort the green coordinates
//!   once more (same polynomial) and scale with `ScaleFactor`,
//! - vignetting: falloff `1 + a1 r² + a2 r⁴ + a3 r⁶` (source coordinates), corrected by dividing,
//! - aperture as APEX value (`ApertureValue = 2·log₂ N`), distance in meters.
//! Selection: make, lens name from EXIF (for compact cameras the camera model),
//! RAW profile before JPEG profile, same camera model preferred, matching sensor size.
//! Interpolation: focal length linear, distance linear in 1/d, aperture linear in APEX.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use rayon::prelude::*;

/// Header data of a profile (for selection, without reading the entries).
#[derive(Debug, Clone)]
pub struct Header {
    pub path: PathBuf,
    pub make: String,
    pub model: String,
    pub lens: String,
    pub lens_pretty: String,
    pub raw_bytes: bool,
    pub crop: f32,
}

/// Folders with Adobe lens profiles: system-wide (also filled by the free Adobe DNG Converter),
/// Lightroom package, user, plus a freely selectable extra folder (settings).
/// `GRAINEO_LCP_DIR` replaces the default list (tests; empty = disable LCP).
pub fn folders(additional: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = if let Ok(e) = std::env::var("GRAINEO_LCP_DIR") {
        e.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
    } else {
        let mut v: Vec<PathBuf> = sysdirs::system_data_dir().map(|s| s.join("Adobe/CameraRaw/LensProfiles/1.0")).into_iter().collect();
        v.push(PathBuf::from("/Applications/Adobe Lightroom Classic/Adobe Lightroom Classic.app/Contents/Resources/LensProfiles"));
        if let Some(d) = sysdirs::app_data_dir() {
            v.push(d.join("Adobe/CameraRaw/LensProfiles"));
        }
        v
    };
    if let Some(z) = additional {
        v.push(z.to_path_buf());
    }
    v
}

fn files_found(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files_found(&p, out);
        } else if p.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("lcp")) {
            out.push(p);
        }
    }
}

/// Value of a field from the text (attribute or element form).
fn field(t: &str, name: &str) -> Option<String> {
    let a = format!("stCamera:{name}=\"");
    if let Some(i) = t.find(&a) {
        let r = &t[i + a.len()..];
        return r.find('"').map(|j| xml_text(&r[..j]));
    }
    let e = format!("<stCamera:{name}>");
    let i = t.find(&e)?;
    let r = &t[i + e.len()..];
    r.find('<').map(|j| xml_text(r[..j].trim()))
}

fn xml_text(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&apos;", "'")
}

/// Briefly read all profiles once (the first entry is enough for the header data); once per folder list
/// per program run.
pub fn index(additional: Option<&Path>) -> Arc<Vec<Header>> {
    static IDX: OnceLock<Mutex<HashMap<Option<PathBuf>, Arc<Vec<Header>>>>> = OnceLock::new();
    let cache = IDX.get_or_init(|| Mutex::new(HashMap::new()));
    let key = additional.map(Path::to_path_buf);
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return v;
    }
    let v = Arc::new(read_index(additional));
    if let Ok(mut c) = cache.lock() {
        c.insert(key, v.clone());
    }
    v
}

fn read_index(additional: Option<&Path>) -> Vec<Header> {
    {
        let mut all = Vec::new();
        for d in folders(additional) {
            files_found(&d, &mut all);
        }
        all.par_iter()
            .filter_map(|p| {
                use std::io::Read;
                let mut f = std::fs::File::open(p).ok()?;
                let mut buf = vec![0u8; 8192];
                let n = f.read(&mut buf).ok()?;
                let t = String::from_utf8_lossy(&buf[..n]);
                Some(Header {
                    path: p.clone(),
                    make: field(&t, "Make")?,
                    model: field(&t, "Model").unwrap_or_default(),
                    lens: field(&t, "Lens").unwrap_or_default(),
                    lens_pretty: field(&t, "LensPrettyName").unwrap_or_default(),
                    raw_bytes: field(&t, "CameraRawProfile").is_some_and(|v| v.eq_ignore_ascii_case("true")),
                    crop: field(&t, "SensorFormatFactor").and_then(|v| v.parse().ok()).unwrap_or(1.0),
                })
            })
            .collect()
    }
}

use lensprof::names::{first_word, model_matches, norm as n};

fn lens_core(s: &str, maker: &str) -> String {
    let without_brackets: String = {
        let mut t = String::new();
        let mut depth_level = 0;
        for c in s.chars() {
            match c {
                '[' | '(' => depth_level += 1,
                ']' | ')' => depth_level -= 1,
                _ if depth_level == 0 => t.push(c),
                _ => {}
            }
        }
        t
    };
    let v = n(&without_brackets);
    let h = first_word(maker);
    v.strip_prefix(&h).map(str::to_string).unwrap_or(v)
}

/// Choose a profile: first via the EXIF lens name, otherwise (compact camera) via the
/// camera model; RAW profile and same camera model preferred.
pub fn search(maker: &str, model: &str, lens: Option<&str>, additional: Option<&Path>) -> Option<Header> {
    let idx = index(additional);
    let mk = first_word(maker);
    let candidates = idx.iter().filter(|k| first_word(&k.make) == mk || (mk == "ricoh" && first_word(&k.make) == "ricoh"));
    let score = |k: &Header| -> i32 { (k.raw_bytes as i32) * 4 + model_matches(&k.model, model) as i32 * 2 };
    let obj = lens.map(n).filter(|o| o.len() >= 3);
    if let Some(o) = &obj {
        if let Some(k) = candidates.clone().filter(|k| n(&k.lens) == *o).max_by_key(|k| score(k)) {
            return Some(k.clone());
        }
        // second attempt: without maker names and suffixes in square brackets, also against the
        // spelled-out lens name (e.g. "Canon EF-S 18-55mm f/3.5-5.6 [II]" ↔ "EF-S18-55mm f/3.5-5.6")
        let o2 = lens_core(lens.unwrap_or(""), maker);
        if o2.len() >= 3 {
            if let Some(k) = candidates.clone().filter(|k| lens_core(&k.lens, &k.make) == o2 || lens_core(&k.lens_pretty, &k.make) == o2).max_by_key(|k| score(k)) {
                return Some(k.clone());
            }
        }
    }
    // compact camera: profile without its own lens name (or with the same model) for this camera model
    // only profiles of built-in lenses (lens name contains the camera model or is missing),
    // otherwise an interchangeable-lens camera without lens info would get a foreign profile (e.g. Laowa)
    let is_fixed = |k: &Header| {
        let lp = n(&k.lens_pretty);
        let m = n(model);
        let m_short = m.trim_start_matches(&first_word(maker)).to_string();
        lp.is_empty() || lp.contains(&m) || (!m_short.is_empty() && lp.contains(&m_short)) || n(&k.lens_pretty) == n(&k.model)
    };
    candidates.filter(|k| model_matches(&k.model, model) && is_fixed(k)).max_by_key(|k| score(k) * 2 + (k.lens.is_empty() || obj.as_deref() == Some(&n(&k.lens))) as i32).cloned()
}

/// Profile by its file name (manual choice; the name is stored, so a profile found in another folder still matches).
pub fn by_file(name: &str, additional: Option<&Path>) -> Option<Header> {
    index(additional).iter().find(|h| h.path.file_name().and_then(|f| f.to_str()) == Some(name)).cloned()
}

// ---------------------------------------------------------------------------------------------
// Read a profile

// Lens model, entries, selection and interpolation: `lensprof::model` (moved 10.11.2026)
pub use lensprof::model::*;

const NS: &str = "http://ns.adobe.com/photoshop/1.0/camera-profile";

/// Properties of a node: attributes in the stCamera namespace and simple child elements.
fn properties(node: roxmltree::Node) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for a in node.attributes() {
        if a.namespace() == Some(NS) {
            m.insert(a.name().to_string(), a.value().to_string());
        }
    }
    for c in node.children().filter(|c| c.is_element() && c.tag_name().namespace() == Some(NS)) {
        if !c.children().any(|cc| cc.is_element()) {
            if let Some(t) = c.text() {
                m.insert(c.tag_name().name().to_string(), t.trim().to_string());
            }
        }
    }
    m
}

/// Child node `name` (attribute form or with an embedded rdf:Description).
fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    let c = node.children().find(|c| c.is_element() && c.tag_name().namespace() == Some(NS) && c.tag_name().name() == name)?;
    Some(c.children().find(|d| d.is_element() && d.tag_name().name() == "Description").unwrap_or(c))
}

fn number(m: &HashMap<String, String>, k: &str) -> Option<f64> {
    m.get(k).and_then(|v| v.trim().parse().ok())
}

fn model(m: &HashMap<String, String>, basis: Option<&Model>) -> Option<Model> {
    let fx = number(m, "FocalLengthX").or(basis.map(|b| b.fx))?;
    Some(Model {
        fx,
        fy: number(m, "FocalLengthY").unwrap_or(fx),
        cx: number(m, "ImageXCenter").or(basis.map(|b| b.cx)).unwrap_or(0.5),
        cy: number(m, "ImageYCenter").or(basis.map(|b| b.cy)).unwrap_or(0.5),
        k: [number(m, "RadialDistortParam1").unwrap_or(0.0), number(m, "RadialDistortParam2").unwrap_or(0.0), number(m, "RadialDistortParam3").unwrap_or(0.0)],
        p: [number(m, "TangentialDistortParam1").unwrap_or(0.0), number(m, "TangentialDistortParam2").unwrap_or(0.0)],
        scale: number(m, "ScaleFactor").unwrap_or(1.0),
    })
}

pub fn load(path: &Path) -> Option<Profile> {
    let txt = std::fs::read_to_string(path).ok()?;
    let doc = roxmltree::Document::parse(&txt).ok()?;
    let mut entries = Vec::new();
    let mut name = String::new();
    let mut crop = 1.0;
    let seq = doc.descendants().find(|d| d.tag_name().name() == "CameraProfiles" && d.tag_name().namespace() == Some("http://ns.adobe.com/photoshop/1.0/"))?;
    for li in seq.descendants().filter(|d| d.tag_name().name() == "li" && d.parent().is_some_and(|p| p.tag_name().name() == "Seq" && p.parent() == Some(seq))) {
        let desc = li.children().find(|d| d.is_element() && d.tag_name().name() == "Description").unwrap_or(li);
        let header = properties(desc);
        if name.is_empty() {
            name = header.get("ProfileName").or(header.get("LensPrettyName")).cloned().unwrap_or_default();
            crop = number(&header, "SensorFormatFactor").unwrap_or(1.0);
        }
        let Some(pm) = child(desc, "PerspectiveModel") else { continue };
        let pe = properties(pm);
        let geo = model(&pe, None);
        let ca = match (child(pm, "ChromaticRedGreenModel"), child(pm, "ChromaticBlueGreenModel")) {
            (Some(r), Some(b)) => model(&properties(r), geo.as_ref()).zip(model(&properties(b), geo.as_ref())),
            _ => None,
        };
        let vig = child(pm, "VignetteModel").and_then(|v| {
            let m = properties(v);
            let fx = number(&m, "FocalLengthX").or(geo.map(|g| g.fx))?;
            Some(Vignette {
                fx,
                fy: number(&m, "FocalLengthY").unwrap_or(fx),
                cx: number(&m, "ImageXCenter").or(geo.map(|g| g.cx)).unwrap_or(0.5),
                cy: number(&m, "ImageYCenter").or(geo.map(|g| g.cy)).unwrap_or(0.5),
                a: [number(&m, "VignetteModelParam1").unwrap_or(0.0), number(&m, "VignetteModelParam2").unwrap_or(0.0), number(&m, "VignetteModelParam3").unwrap_or(0.0)],
            })
        });
        entries.push(Entry {
            focal_length: number(&header, "FocalLength").unwrap_or(0.0),
            distance: number(&header, "FocusDistance").unwrap_or(1000.0).max(0.01),
            av: number(&header, "ApertureValue").unwrap_or(6.0),
            geo,
            ca,
            vig,
        });
    }
    (!entries.is_empty()).then_some(Profile { name, crop, entries })
}

// ---------------------------------------------------------------------------------------------
// Private backup (local only, never in the repository or the app bundle)

/// `~/Library/Application Support/graineo/lens-profiles/`; test runs: `GRAINEO_LENS_BACKUP` (exact folder) or
/// `GRAINEO_DATA_DIR/lens-profiles`, in a cargo test program its own temp folder ([`lensprof::store::root`]).
pub fn backup_dir() -> Option<PathBuf> {
    lensprof::store::root()
}

/// Backed-up coefficients of a camera/lens combination (own JSON format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub format: u32,
    pub maker: String,
    pub model: String,
    pub lens: String,
    /// File name of the LCP profile the backup was made from
    pub origin: String,
    pub profile: Profile,
}

impl Backup {
    /// Read a backup file; files of graineo 1.0 (old keys) are translated on reading.
    pub fn from_json(text: &str) -> Option<Backup> {
        let mut v: serde_json::Value = serde_json::from_str(text).ok()?;
        legacy_names::settings_keys::upgrade_lens_profile(&mut v);
        serde_json::from_value(v).ok()
    }
}

fn backup_file(maker: &str, model: &str, lens: Option<&str>) -> Option<PathBuf> {
    let name: String = format!("{} {} {}", first_word(maker), model.trim(), lens.unwrap_or("").trim())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    Some(backup_dir()?.join(format!("{}.json", name.trim_matches('_'))))
}

pub fn load_backup(maker: &str, model: &str, lens: Option<&str>) -> Option<Backup> {
    let t = std::fs::read_to_string(backup_file(maker, model, lens)?).ok()?;
    // a backup without entries would be selected (`covers` says yes without a focal length) and then indexed
    Backup::from_json(&t).filter(|b| !b.profile.entries.is_empty())
}

/// Back up the profile for this combination (overwrites nothing that is already there, except with `new_at`).
pub fn write_backup(maker: &str, model: &str, lens: Option<&str>, header: &Header, profile: &Profile, new_at: bool) -> std::io::Result<PathBuf> {
    let path = backup_file(maker, model, lens).ok_or_else(|| std::io::Error::other("kein Benutzerordner"))?;
    if path.exists() && !new_at {
        return Ok(path);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let s = Backup {
        format: 1,
        maker: maker.to_string(),
        model: model.to_string(),
        lens: lens.unwrap_or("").to_string(),
        origin: header.path.file_name().and_then(|f| f.to_str()).unwrap_or("").to_string(),
        profile: profile.clone(),
    };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&s).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Own (self-measured) profiles: `lens-profiles/own/*.json` (format 1 or 2, [`lensprof::own::OwnProfile`]);
/// selection, versions and history in [`lensprof::store`]. Highest priority.
pub fn custom_folders() -> Option<PathBuf> {
    lensprof::store::own_dir()
}

/// "Back up lens profiles": for the given combinations (make, model, lens)
/// read and back up the locally available LCP profile. Result per combination: path or reason.
pub fn backup_enabled(combinations: &[(String, String, Option<String>)], additional: Option<&Path>) -> Vec<Result<PathBuf, String>> {
    combinations
        .iter()
        .map(|(h, m, o)| {
            let k = search(h, m, o.as_deref(), additional).ok_or("kein Adobe-Profil gefunden")?;
            let p = load(&k.path).ok_or("Profil nicht lesbar")?;
            write_backup(h, m, o.as_deref(), &k, &p, true).map_err(|e| e.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Format 2 (own profiles from 10.11.2026) is still read by the format-1 reader of older versions.
    #[test]
    fn old_reader_accepts_v2() {
        let p = Profile { name: "T".into(), crop: 1.0, entries: vec![Entry { focal_length: 50.0, distance: 1000.0, av: 4.0, geo: None, ca: None, vig: None }] };
        let mut o = lensprof::OwnProfile::new("SIGMA", "sd Quattro", "", "graineo-Messung", p);
        o.cast.push(lensprof::own::CastEntry { focal_length: 50.0, av: 4.0, grid: lensprof::cast::CastGrid::neutral(3, 3), quality: Default::default() });
        let j = serde_json::to_string(&o).unwrap();
        let b: Backup = serde_json::from_str(&j).expect("format 1 reader");
        assert_eq!((b.format, b.maker.as_str(), b.profile.entries.len()), (2, "SIGMA", 1));
    }

    #[test]
    fn backup_in_the_old_format_loads() {
        let p = Profile { name: "T".into(), crop: 1.0, entries: vec![Entry { focal_length: 50.0, distance: 1000.0, av: 4.0, geo: None, ca: None, vig: None }] };
        let o = lensprof::OwnProfile::new("SIGMA", "sd Quattro", "", "graineo-Messung", p);
        let mut v = serde_json::to_value(&o).unwrap();
        assert_eq!(v["maker"], "SIGMA");
        legacy_names::settings_keys::to_old_for_tests(&mut v, legacy_names::settings_keys::LENS_PROFILE);
        assert!(v.get("maker").is_none());
        let b = Backup::from_json(&v.to_string()).expect("old file loads");
        assert_eq!((b.maker.as_str(), b.model.as_str(), b.profile.entries.len()), ("SIGMA", "sd Quattro", 1));
    }

    /// Only if Adobe profiles are present on the machine: GR III and EF 40 mm are found.
    #[test]
    fn local_profiles() {
        if index(None).is_empty() {
            return;
        }
        let k = search("RICOH IMAGING COMPANY, LTD.", "GR III", None, None).expect("GR III");
        assert!(k.model.contains("GR III") && k.raw_bytes, "{k:?}");
        let p = load(&k.path).expect("read");
        let a = p.select(Some(18.3), Some(2.8), None);
        assert!(a.geo.is_some() && a.vig.is_some());
        let k = search("Canon", "EOS RP", Some("EF40mm f/2.8 STM"), None).expect("EF 40");
        assert!(k.lens.contains("EF40"), "{k:?}");
    }
}
