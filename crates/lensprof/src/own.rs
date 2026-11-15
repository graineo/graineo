//! Own lens profiles, file format 2 (`lens-profiles/own/<id>.json`).
//!
//! Format 1 (up to 0.94) held only make, model, lens, origin and the profile (with other key names, translated on
//! loading by `legacy_names::settings_keys`). Format 2 keeps these keys and adds
//! English fields: id, display name, version, sensor kind, colour-cast maps per channel/layer,
//! revision history and quality. Format-1 files load unchanged (missing fields get defaults); older app
//! versions read format 2 because unknown fields are skipped.

use serde::{Deserialize, Serialize};

pub use crate::cast::{CastEntry, CastGrid, CastQuality};

/// Format written by this version.
pub const FORMAT: u32 = 2;

/// Kind of sensor the profile was measured on (a flat field depends on the sensor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorKind {
    Bayer,
    Xtrans,
    Foveon,
    Linear,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionAction {
    /// Format-1 file taken over as version 1
    Migrated,
    Measured,
    Extended,
    Restored { from: u32 },
    Imported,
    Duplicated { from: String },
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotRole {
    Grid,
    FlatField,
}

/// A shot that went into a measurement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotRef {
    pub name: String,
    #[serde(default)]
    pub sha256: String,
    pub role: ShotRole,
    #[serde(default)]
    pub focal_length: Option<f32>,
    #[serde(default)]
    pub aperture: Option<f32>,
    /// second shot of a flat-field pair, camera turned by 180°
    #[serde(default)]
    pub rotated: bool,
}

/// One entry of the revision history. A revision with `version == 0` handed to
/// [`crate::store::save_new`] / [`crate::store::save_version`] is a draft: its shots and note are used
/// for the new version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub version: u32,
    /// seconds since 1970
    pub time: i64,
    pub action: RevisionAction,
    #[serde(default)]
    pub shots: Vec<ShotRef>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPart {
    Distortion,
    LateralCa,
    Vignette,
    Cast,
}

/// Quality 0–100; the score of the profile is the weakest part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quality {
    pub score: u8,
    pub parts: Vec<(QualityPart, u8)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnProfile {
    /// 2 when written by this version
    pub format: u32,
    pub maker: String,
    pub model: String,
    /// lens name from EXIF or "" (manual lens)
    pub lens: String,
    pub origin: String,
    pub profile: crate::model::Profile,
    /// format 1: the file stem
    #[serde(default)]
    pub id: String,
    /// display name; format 1: `profil.name`
    #[serde(default)]
    pub name: String,
    /// 0 in format 1 → 1
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub sensor: SensorKind,
    #[serde(default)]
    pub cast: Vec<CastEntry>,
    #[serde(default)]
    pub history: Vec<Revision>,
    #[serde(default)]
    pub quality: Option<Quality>,
}

impl OwnProfile {
    /// New profile (version 0 until it is saved).
    pub fn new(maker: &str, model: &str, lens: &str, origin: &str, profile: crate::model::Profile) -> Self {
        OwnProfile {
            format: FORMAT,
            maker: maker.to_string(),
            model: model.to_string(),
            lens: lens.to_string(),
            origin: origin.to_string(),
            name: profile.name.clone(),
            profile,
            id: String::new(),
            version: 0,
            sensor: SensorKind::Unknown,
            cast: Vec::new(),
            history: Vec::new(),
            quality: None,
        }
    }

    /// Read a file of format 1 or 2; `stem` is the file stem (it is the id: a copied file gets its own id).
    pub fn from_json(text: &str, stem: &str) -> Result<Self, String> {
        // files of graineo 1.0 have old keys: translated here, written with the current ones
        let mut v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        legacy_names::settings_keys::upgrade_lens_profile(&mut v);
        let mut p: OwnProfile = serde_json::from_value(v).map_err(|e| e.to_string())?;
        if p.format > FORMAT {
            return Err(format!("Profilformat {} ist neuer als diese Fassung ({FORMAT})", p.format));
        }
        p.normalize(stem);
        Ok(p)
    }

    /// Fill the fields that format 1 lacks.
    pub fn normalize(&mut self, stem: &str) {
        if !stem.is_empty() {
            self.id = stem.to_string();
        }
        if self.name.trim().is_empty() {
            self.name = self.profile.name.clone();
        }
        if self.version == 0 {
            self.version = 1;
        }
    }

    pub fn reference(&self) -> ProfileRef {
        ProfileRef { id: self.id.clone(), version: self.version }
    }
}

/// Reference to one version of an own profile, written as `id@version` (e.g. in `Settings.lens_profile`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileRef {
    pub id: String,
    pub version: u32,
}

impl ProfileRef {
    /// `"3fa9c0d1e2b4@2"` → id and version; `None` for anything else (also for `""`).
    pub fn parse(s: &str) -> Option<Self> {
        let (id, v) = s.trim().rsplit_once('@')?;
        let version: u32 = v.parse().ok()?;
        (crate::store::valid_id(id) && version > 0).then(|| ProfileRef { id: id.to_string(), version })
    }
}

impl std::fmt::Display for ProfileRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.id, self.version)
    }
}

impl std::str::FromStr for ProfileRef {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Self::parse(s).ok_or_else(|| format!("keine Profilangabe: {s}"))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Format 1 as written by `raw::measure_run::save` up to 0.94 (shortened to one entry): the current document
    /// turned back into the old keys by the legacy table.
    pub(crate) fn v1() -> String {
        let mut v: serde_json::Value = serde_json::from_str(CURRENT_V1).unwrap();
        legacy_names::settings_keys::to_old_for_tests(&mut v, legacy_names::settings_keys::LENS_PROFILE);
        serde_json::to_string_pretty(&v).unwrap()
    }

    const CURRENT_V1: &str = r#"{
  "format": 1,
  "maker": "Canon",
  "model": "Canon EOS 400D DIGITAL",
  "lens": "EF-S10-22mm f/3.5-4.5 USM",
  "origin": "graineo-Messung",
  "profile": {
    "name": "Canon Canon EOS 400D DIGITAL EF-S10-22mm f/3.5-4.5 USM (graineo-Messung)",
    "crop": 1.6,
    "entries": [
      {
        "focal_length": 10.0,
        "distance": 1000.0,
        "av": 6.0,
        "geo": { "fx": 0.444, "fy": 0.444, "cx": 0.5, "cy": 0.5, "k": [-0.09, 0.02, 0.0], "p": [0.0, 0.0], "scale": 1.0 },
        "ca": null,
        "vig": { "fx": 0.444, "fy": 0.444, "cx": 0.5, "cy": 0.5, "a": [-0.6, 0.2, 0.0] }
      }
    ]
  }
}"#;

    #[test]
    fn v1_file_loads_with_derived_id_name_and_version_1() {
        let p = OwnProfile::from_json(&v1(), "Canon_Canon_EOS_400D_DIGITAL_EF-S10-22mm_f_3.5-4.5_USM").unwrap();
        assert_eq!(p.id, "Canon_Canon_EOS_400D_DIGITAL_EF-S10-22mm_f_3.5-4.5_USM");
        assert_eq!(p.name, p.profile.name);
        assert_eq!(p.version, 1);
        assert!(p.cast.is_empty() && p.history.is_empty() && p.quality.is_none());
        assert_eq!(p.sensor, SensorKind::Unknown);
        assert_eq!(p.profile.entries.len(), 1);
    }

    #[test]
    fn v2_roundtrip_writes_current_keys_for_a_v1_file() {
        let mut p = OwnProfile::from_json(&v1(), "abc").unwrap();
        p.format = FORMAT;
        p.cast.push(CastEntry { focal_length: 10.0, av: 4.0, grid: CastGrid::neutral(3, 2), quality: CastQuality::default() });
        p.history.push(Revision { version: 1, time: 1, action: RevisionAction::Restored { from: 3 }, shots: vec![], note: String::new() });
        p.quality = Some(Quality { score: 80, parts: vec![(QualityPart::Cast, 80)] });
        let j = serde_json::to_string(&p).unwrap();
        for k in ["\"maker\"", "\"model\"", "\"lens\"", "\"origin\"", "\"profile\"", "\"entries\"", "\"focal_length\"", "\"cast\"", "\"history\"", "\"restored\""] {
            assert!(j.contains(k), "{k} fehlt in {j}");
        }
        let q = OwnProfile::from_json(&j, "abc").unwrap();
        assert_eq!(serde_json::to_string(&q).unwrap(), j);
    }

    #[test]
    fn newer_format_is_refused() {
        let j = v1().replacen("\"format\": 1", "\"format\": 3", 1);
        assert!(OwnProfile::from_json(&j, "x").is_err());
    }

    #[test]
    fn profile_ref_parse_and_display() {
        let r = ProfileRef::parse("3fa9c0d1e2b4@2").unwrap();
        assert_eq!((r.id.as_str(), r.version), ("3fa9c0d1e2b4", 2));
        assert_eq!(r.to_string(), "3fa9c0d1e2b4@2");
        for bad in ["", "abc", "abc@", "abc@0", "@1", "../x@1", "a/b@1", ".hidden@1", "abc@x"] {
            assert!(ProfileRef::parse(bad).is_none(), "{bad}");
        }
        // file stems of format 1 contain only letters, digits, '.', '-' and '_'
        assert!(ProfileRef::parse("Canon_EOS_400D_EF-S10-22mm_f_3.5-4.5@1").is_some());
    }
}
