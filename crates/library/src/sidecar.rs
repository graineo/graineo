//! Sidecar of an image (optional since 08.11.2026 – the library is the main store, [`crate::store`]) with stars,
//! flag, label, keywords, virtual copies and develop settings:
//! - All formats: XMP next to the image ([`crate::xmp`]); for X3F additionally with the develop settings
//!   (`graineo:DevelopSettings`), as for any other RAW. Without them, developing an X3F uses the starting values of
//!   SIGMA Photo Pro. (The JSON file of the versions up to 0.38 is no longer read; the user's files were taken over.)
//!
//! Virtual copies are stored in the original's sidecar (several copies in one file).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::xmp::{self, ReadResult, Values};

/// A virtual copy (in the original's sidecar).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VirtualCopy {
    pub name: String,
    pub stars: u8,
    pub flag: i8,
    pub label: u8,
    pub keywords: Vec<String>,
    /// own develop settings (X3F: as in the sidecar), else empty
    pub settings: Option<Value>,
    /// the copy's develop settings and orientation from Lightroom (taken over once, see [`crate::xmp::Lightroom`])
    pub lightroom: Option<String>,
    pub lightroom_orientation: Option<String>,
}

impl VirtualCopy {
    pub fn json(&self) -> Value {
        let mut v = json!({"name": self.name, "rating": self.stars, "flag": self.flag, "label": self.label, "keywords": self.keywords});
        if let Some(e) = &self.settings {
            v["settings"] = e.clone();
        }
        if let Some(t) = &self.lightroom {
            v["lightroomDevelop"] = Value::String(t.clone());
        }
        if let Some(o) = &self.lightroom_orientation {
            v["lightroomOrientation"] = Value::String(o.clone());
        }
        v
    }
    pub fn from_json(v: &Value) -> Option<VirtualCopy> {
        Some(VirtualCopy {
            name: v["name"].as_str()?.to_string(),
            stars: v["rating"].as_u64().unwrap_or(0).min(5) as u8,
            flag: v["flag"].as_i64().unwrap_or(0).clamp(-1, 1) as i8,
            label: v["label"].as_u64().unwrap_or(0).min(5) as u8,
            keywords: v["keywords"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            settings: v.get("settings").cloned(),
            lightroom: v["lightroomDevelop"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string),
            lightroom_orientation: v["lightroomOrientation"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string),
        })
    }
}

pub fn read_copies(json: Option<&str>) -> Vec<VirtualCopy> {
    json.and_then(|j| serde_json::from_str::<Value>(j).ok()).and_then(|v| v.as_array().map(|a| a.iter().filter_map(VirtualCopy::from_json).collect())).unwrap_or_default()
}

pub fn copies_text(k: &[VirtualCopy]) -> Option<String> {
    (!k.is_empty()).then(|| Value::Array(k.iter().map(VirtualCopy::json).collect()).to_string())
}

/// Location of the sidecar. `raw_beside`: RAW with the same name in the folder (see [`xmp::path`]).
pub fn path(image: &Path, format: &str, raw_beside: bool) -> PathBuf {
    xmp::path(image, format, raw_beside)
}

/// Read the sidecar (XMP) of an image, with the file's modification time.
pub fn read(image: &Path, format: &str, raw_beside: bool) -> (ReadResult, Option<i64>) {
    let mtime = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    let p = path(image, format, raw_beside);
    (xmp::read(&p), mtime(&p))
}

/// Write (only our fields; the rest stays).
pub fn write(image: &Path, format: &str, raw_beside: bool, w: &Values) -> Result<PathBuf, String> {
    let p = path(image, format, raw_beside);
    xmp::write(&p, w)?;
    Ok(p)
}

/// Values are empty (nothing a sidecar would need)?
pub fn empty(w: &Values) -> bool {
    w.stars == 0 && w.flag == 0 && w.label == 0 && w.keywords.is_empty() && w.copies_json.is_none() && w.time_correction.is_none()
}
