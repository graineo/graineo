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
    /// resampling filter key (`develop::resize::ResizeFilter::key`); `None`: the template leaves the choice as it is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<String>,
    /// export folder; `None` (also every template of earlier versions): the template keeps the current folder
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

impl Template {
    pub fn new(name: &str, naming: Naming) -> Template {
        Template { format: FORMAT.into(), version: VERSION, name: name.trim().to_string(), naming, file_format: None, original: false, color_space: None, limit_size: None, size: None, resize: None, dir: None }
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

    /// The resampling filter, if the template has one.
    pub fn resize_filter(&self) -> Option<develop::resize::ResizeFilter> {
        self.resize.as_deref().and_then(develop::resize::ResizeFilter::from_key)
    }

    /// The output size, if the template has one.
    pub fn output_size(&self) -> Option<develop::export::OutputSize> {
        self.size.clone().and_then(|v| develop::export::OutputSize::from_json(v).ok())
    }
}

/// Is the stored folder of a template there? Checked on a helper thread with a short wait, so a sleeping network
/// drive cannot freeze the window. The error is a German phrase for the message ("nicht erreichbar", …).
pub fn folder_reachable(dir: &Path) -> Result<(), &'static str> {
    folder_reachable_within(dir, std::time::Duration::from_millis(400))
}

fn folder_reachable_within(dir: &Path, wait: std::time::Duration) -> Result<(), &'static str> {
    // a relative folder (hand-edited file) would be taken relative to wherever the app was started
    if !dir.is_absolute() {
        return Err("ist kein erlaubtes Ziel");
    }
    library::write_target::check(dir).map_err(|_| "ist kein erlaubtes Ziel")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let d = dir.to_path_buf();
    std::thread::Builder::new().name("export-template-folder".into()).spawn(move || {
        let _ = tx.send(d.is_dir());
    }).map_err(|_| "nicht erreichbar")?;
    match rx.recv_timeout(wait) {
        Ok(true) => Ok(()),
        Ok(false) => Err("nicht erreichbar"),
        Err(_) => Err("antwortet nicht"),
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
        let mut t = Template::new("Web klein", Naming { pattern: "{date}_{n:3}".into(), start: 1, subfolder: Subfolder::Date, collision: Collision::Skip, ..Default::default() });
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
        assert_eq!(all[0].1.dir, None, "a template of earlier versions keeps the current folder");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn folder_target_and_custom_subfolder_round_trip() {
        let d = tmp();
        let naming = Naming { subfolder: Subfolder::Custom, subfolder_text: "Export/{date:YYYY}".into(), beside: true, ..Default::default() };
        let mut t = Template::new("Neben Original", naming);
        t.dir = Some("/Volumes/NAS/Export".into());
        let p = save(&d, &t).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        for key in ["\"dir\": \"/Volumes/NAS/Export\"", "\"beside\": true", "\"subfolderText\": \"Export/{date:YYYY}\"", "\"subfolder\": \"custom\""] {
            assert!(text.contains(key), "{key} in {text}");
        }
        assert_eq!(load_all(&d), vec![(p, t)]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn template_folder_check() {
        let d = tmp();
        assert_eq!(folder_reachable(&d), Ok(()));
        assert_eq!(folder_reachable(&d.join("fehlt")), Err("nicht erreichbar"));
        // hand-edited templates: relative folders and folders inside another program's library are refused
        assert_eq!(folder_reachable(Path::new(".")), Err("ist kein erlaubtes Ziel"));
        assert_eq!(folder_reachable(Path::new("../Export")), Err("ist kein erlaubtes Ziel"));
        assert_eq!(folder_reachable(&d.join("Photos Library.photoslibrary/originals")), Err("ist kein erlaubtes Ziel"));
        std::fs::remove_dir_all(d).unwrap();
    }
}
