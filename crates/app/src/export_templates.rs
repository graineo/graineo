//! Export templates: naming rules (pattern, number, subfolder, existing files) plus format, colour space and size of
//! the export dialog. One JSON file per template in `<data_dir>/export-templates/<slug>.json` (English keys, format id
//! and version), like the print templates. A broken file is skipped, never fatal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::export_names::Naming;

/// Format id of a template file.
pub const FORMAT: &str = "graineo-export-template";
pub const VERSION: u32 = 1;
/// Folder of the templates inside the data folder.
pub const FOLDER: &str = "export-templates";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub format: String,
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub naming: Naming,
    /// output format; `None`: the template leaves it as it is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_format: Option<develop::export::Format>,
    /// export the originals unchanged
    #[serde(default)]
    pub original: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<graineo_color::Space>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_size: Option<bool>,
    /// `develop::export::OutputSize` as stored in the settings (read with `OutputSize::from_json`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<serde_json::Value>,
}

impl Template {
    pub fn new(name: &str, naming: Naming) -> Template {
        Template { format: FORMAT.into(), version: VERSION, name: name.trim().to_string(), naming, file_format: None, original: false, color_space: None, limit_size: None, size: None }
    }

    pub fn from_json(text: &str) -> Result<Template, String> {
        let t: Template = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if t.format != FORMAT {
            return Err(format!("not an export template: {}", t.format));
        }
        if t.version > VERSION {
            return Err(format!("template version {} is newer than this program", t.version));
        }
        if t.name.trim().is_empty() {
            return Err("template without a name".into());
        }
        Ok(t)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("template serialises")
    }

    /// The output size, if the template has one.
    pub fn output_size(&self) -> Option<develop::export::OutputSize> {
        self.size.clone().and_then(|v| develop::export::OutputSize::from_json(v).ok())
    }
}

pub fn folder(data_dir: &Path) -> PathBuf {
    data_dir.join(FOLDER)
}

fn path_of(data_dir: &Path, name: &str) -> PathBuf {
    folder(data_dir).join(format!("{}.json", print::template::slug(name)))
}

/// The templates (sorted by name); unreadable or broken files are skipped.
pub fn load_all(data_dir: &Path) -> Vec<(PathBuf, Template)> {
    let mut ok = Vec::new();
    let Ok(rd) = std::fs::read_dir(folder(data_dir)) else { return ok };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            if let Ok(t) = std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| Template::from_json(&t)) {
                ok.push((p, t));
            }
        }
    }
    ok.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()));
    ok
}

/// The file a template named `name` would replace, if there is one ([`save`] replaces it, so the caller asks first).
pub fn existing(data_dir: &Path, name: &str) -> Option<PathBuf> {
    let p = path_of(data_dir, name);
    std::fs::symlink_metadata(&p).is_ok().then_some(p)
}

/// Save a template under its name (side file, then rename: a crash never leaves half a template).
pub fn save(data_dir: &Path, t: &Template) -> std::io::Result<PathBuf> {
    let dir = folder(data_dir);
    std::fs::create_dir_all(&dir)?;
    let path = path_of(data_dir, &t.name);
    let side = dir.join(format!(".{}.json.part", print::template::slug(&t.name)));
    std::fs::write(&side, t.to_json())?;
    std::fs::rename(&side, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_names::{Collision, Subfolder};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-export-templates-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_load_replace_and_broken_files() {
        let d = tmp();
        assert!(load_all(&d).is_empty());
        let mut t = Template::new("Web klein", Naming { pattern: "{date}_{n:3}".into(), start: 1, subfolder: Subfolder::Date, collision: Collision::Skip });
        t.file_format = Some(develop::export::Format::Jpeg { quality: 80 });
        t.limit_size = Some(true);
        t.size = serde_json::to_value(develop::export::OutputSize::LongEdge(2048)).ok();
        let p = save(&d, &t).unwrap();
        assert_eq!(p, d.join("export-templates/web-klein.json"));
        let text = std::fs::read_to_string(&p).unwrap();
        for key in ["\"naming\"", "\"pattern\"", "\"fileFormat\"", "\"limitSize\"", "\"collision\": \"skip\""] {
            assert!(text.contains(key), "{key} in {text}");
        }
        assert_eq!(load_all(&d), vec![(p.clone(), t.clone())]);
        assert_eq!(load_all(&d)[0].1.output_size(), Some(develop::export::OutputSize::LongEdge(2048)));
        assert_eq!(existing(&d, "web KLEIN"), Some(p.clone()));
        // broken files, foreign formats and newer versions are skipped
        std::fs::write(d.join("export-templates/kaputt.json"), "{ nicht json").unwrap();
        std::fs::write(d.join("export-templates/fremd.json"), r#"{"format":"x","version":1,"name":"x"}"#).unwrap();
        std::fs::write(d.join("export-templates/neu.json"), format!(r#"{{"format":"{FORMAT}","version":99,"name":"neu"}}"#)).unwrap();
        assert_eq!(load_all(&d).len(), 1);
        // a minimal file: everything else default
        std::fs::write(d.join("export-templates/min.json"), format!(r#"{{"format":"{FORMAT}","version":1,"name":"Min"}}"#)).unwrap();
        let all = load_all(&d);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].1.naming, Naming::default());
        std::fs::remove_dir_all(d).unwrap();
    }
}
