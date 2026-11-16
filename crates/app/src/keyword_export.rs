//! How keywords go into exported files: the parent terms of every keyword in `dc:subject` (default on, as other
//! programs without hierarchy then still find "Tiere" for a photo with "Labrador") and the full paths in
//! `lr:hierarchicalSubject` (default off). Two settings, shown in the export dialog and in Settings → Export; a
//! branch marked "never export" (dialog "Stichwörter ordnen …") is always left out.

use eframe::egui;

use library::keywords::{export_choices, EXPORT_HIERARCHY_KEY, EXPORT_PARENTS_KEY};

/// The two switches side by side (export dialog). `personal`: keywords are exported at all (metadata "Alles").
pub fn controls(ui: &mut egui::Ui, personal: bool) {
    let (mut parents, mut hierarchy) = export_choices();
    ui.add_enabled_ui(personal, |ui| {
        let r = ui.checkbox(&mut parents, crate::tr("Oberbegriffe mitschreiben"));
        crate::lr::hover_tip(ui, &r, crate::tr("Zu „Labrador“ auch „Hunde“ und „Tiere“ in die Stichwörter der Datei"));
        if r.changed() {
            library::settings::set(EXPORT_PARENTS_KEY, serde_json::json!(parents));
        }
        let r = ui.checkbox(&mut hierarchy, crate::tr("Hierarchie mitschreiben"));
        crate::lr::hover_tip(ui, &r, crate::tr("Zusätzlich die vollen Pfade („Tiere|Hunde|Labrador“, lr:hierarchicalSubject) für Programme, die Stichwort-Hierarchien lesen"));
        if r.changed() {
            library::settings::set(EXPORT_HIERARCHY_KEY, serde_json::json!(hierarchy));
        }
    });
}

/// The same as rows of Settings → Export.
pub fn settings_rows(ui: &mut egui::Ui) {
    let (mut parents, mut hierarchy) = export_choices();
    crate::prefs::row(ui, crate::tr("Oberbegriffe der Stichwörter mitschreiben"), Some(crate::tr("Zu „Labrador“ auch „Hunde“ und „Tiere“; gilt beim Export mit Metadaten „Alles“")), |ui| {
        if crate::lr::switch(ui, &mut parents) {
            library::settings::set(EXPORT_PARENTS_KEY, serde_json::json!(parents));
        }
    });
    crate::prefs::row(ui, crate::tr("Stichwort-Hierarchie mitschreiben"), Some(crate::tr("Volle Pfade in lr:hierarchicalSubject, für Programme, die Hierarchien lesen")), |ui| {
        if crate::lr::switch(ui, &mut hierarchy) {
            library::settings::set(EXPORT_HIERARCHY_KEY, serde_json::json!(hierarchy));
        }
    });
}
