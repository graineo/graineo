//! Develop preset on import: only looks (presets of the scope "Look") and film recipes, one choice, "Keine" by default,
//! remembered. It applies to RAWs (X3F and other RAWs); to JPEG/HEIC only when switched on. Applied after the photos are
//! in the library, exactly like pasting settings: the photo's current settings (its starting values) with the preset on
//! top, saved into the library.

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::image_kind::ImageKind;
use crate::presets::{self, Preset, Scope};

/// The presets offered: (group, preset) – the looks of the library, own looks, the film recipes.
pub(super) fn choices() -> Vec<(String, Preset)> {
    let mut v: Vec<(String, Preset)> = Vec::new();
    for c in presets::library() {
        for p in c.presets {
            if matches!(p.scope, Scope::Look | Scope::Film) {
                v.push((c.name.to_string(), p));
            }
        }
    }
    for p in presets::load() {
        if matches!(p.scope, Scope::Look | Scope::Film) {
            v.push(("Eigene".to_string(), p));
        }
    }
    v
}

/// The preset with this name (`""` = none).
pub(super) fn chosen(name: &str) -> Option<Preset> {
    if name.is_empty() {
        return None;
    }
    choices().into_iter().map(|(_, p)| p).find(|p| p.name == name)
}

/// Selection box (grouped by category).
pub(super) fn combo(ui: &mut egui::Ui, name: &mut String, width: f32) -> egui::Response {
    let shown = if name.is_empty() { crate::tr("Keine").to_string() } else { crate::tr(name).to_string() };
    egui::ComboBox::from_id_salt("imp-vorgabe")
        .selected_text(shown)
        .width(width)
        .height(360.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(name, String::new(), crate::tr("Keine"));
            let mut group = String::new();
            for (g, p) in choices() {
                if g != group {
                    ui.separator();
                    ui.label(egui::RichText::new(crate::tr(&g).to_uppercase()).small().color(crate::lr::TEXT_DIM));
                    group = g;
                }
                ui.selectable_value(name, p.name.clone(), crate::tr(&p.name));
            }
        })
        .response
}

/// Does the preset apply to this photo?
fn applies(path: &Path, jpeg: bool) -> bool {
    match crate::image_kind::of(path) {
        ImageKind::Foveon | ImageKind::Raw => true,
        ImageKind::Done => jpeg,
        ImageKind::Other => false,
    }
}

/// Apply the preset to the photos just imported (path, file id, image id). Returns the photos changed with their new
/// settings (image, file, path, settings) – their grid previews are rendered again in the background.
/// Runs in the import thread (reads and writes through the library's store, never the UI).
pub(super) fn apply(p: &Preset, jpeg: bool, files: &[(PathBuf, i64)], images: &[i64]) -> Vec<(i64, i64, PathBuf, develop::Settings)> {
    let mut out = Vec::new();
    for ((path, file), image) in files.iter().zip(images) {
        if !applies(path, jpeg) {
            continue;
        }
        let (cur, meta, _) = crate::sidecar::load_entry(path, None);
        let new = p.apply(&cur);
        if new == cur {
            continue;
        }
        match crate::sidecar::save_entry(path, None, &new, &meta) {
            Ok(()) => out.push((*image, *file, path.clone(), develop::Settings { view: develop::View::Developed, ..new })),
            Err(e) => crate::log(&format!("import: preset \"{}\" not applied to {}: {e}", p.name, path.display())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_looks_and_film_recipes_are_offered() {
        let v = choices();
        assert!(!v.is_empty());
        assert!(v.iter().all(|(_, p)| matches!(p.scope, Scope::Look | Scope::Film)));
        assert!(v.iter().any(|(g, _)| g == "Film-Rezepte"), "film recipes are offered");
        assert!(chosen("").is_none());
        let first = &v[0].1.name;
        assert_eq!(chosen(first).map(|p| p.name), Some(first.clone()));
    }

    #[test]
    fn preset_applies_to_raw_and_jpeg_only_on_request() {
        assert!(applies(Path::new("/x/a.CR3"), false));
        assert!(!applies(Path::new("/x/a.JPG"), false));
        assert!(applies(Path::new("/x/a.JPG"), true));
        assert!(!applies(Path::new("/x/a.MOV"), true));
    }
}
