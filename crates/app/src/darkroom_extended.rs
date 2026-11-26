//! New fine sliders stay grouped in the section that is closed by default.
use crate::{help, lr, App};
use eframe::egui;
fn slider(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
) {
    let mut x = *v as f64;
    if lr::slider(ui, label, &mut x, range, default, 0.01) {
        *v = x as f32;
    }
}
impl App {
    pub(crate) fn darkroom_extended_panel(&mut self, ui: &mut egui::Ui, index: usize) {
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some()
            && crate::env::var("GRAINEO_DARKROOM_TEST_ADVANCED").as_deref() == Ok("1")
        {
            let rect =
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 400.));
            ui.scroll_to_rect(rect, Some(egui::Align::Min));
        }
        let initial = *self.darkroom.advanced_open.get_or_insert_with(|| {
            library::settings::value("darkroomAdvanced")
                .as_bool()
                .unwrap_or(false)
        });
        let section_id = ui.make_persistent_id(("lr-section", "Erweitert"));
        lr::section(ui, "Erweitert", initial, |ui| {
            let f = &mut self.entries[index].settings.film;
            subgroup(ui, "Objektiv", |ui| help::info(ui, "optics", crate::tr("Schärfe-Kette"), &help::FILM_OPTICS));
            egui::ComboBox::from_id_salt("film-optics").width(ui.available_width() - 8.0)
                .selected_text(match f.optics.as_str() {
                    "sharp" => crate::tr("Scharf"),
                    "soft" => crate::tr("Weich"),
                    "vintage" => crate::tr("Alt"),
                    _ => "Standard",
                })
                .show_ui(ui, |ui| {
                    for (id, name) in [
                        ("", "Standard"),
                        ("sharp", crate::tr("Scharf")),
                        ("soft", crate::tr("Weich")),
                        ("vintage", crate::tr("Alt")),
                    ] {
                        ui.selectable_value(&mut f.optics, id.into(), name);
                    }
                });
            let mut individual =
                f.camera_blur_um.is_some() || f.enlarger_blur.is_some() || f.scanner_blur.is_some();
            if lr::switch_label(ui, &mut individual, "Einzelwerte").clicked() {
                f.camera_blur_um = individual.then_some(0.);
                f.enlarger_blur = individual.then_some(0.);
                f.scanner_blur = individual.then_some(0.);
            }
            if individual {
                slider(
                    ui,
                    "Kamera · µm",
                    f.camera_blur_um.get_or_insert(0.),
                    0.0..=50.0,
                    0.0,
                );
                slider(
                    ui,
                    "Vergrößerer",
                    f.enlarger_blur.get_or_insert(0.),
                    0.0..=3.0,
                    0.0,
                );
                slider(
                    ui,
                    "Scanner",
                    f.scanner_blur.get_or_insert(0.),
                    0.0..=3.0,
                    0.0,
                );
            }
            subgroup(ui, "Hof-Typ", |ui| help::info(ui, "halo-type", "Hof-Typ", &help::FILM_HALO));
            egui::ComboBox::from_id_salt("film-halo-type").width(ui.available_width() - 8.0)
                .selected_text(match f.halation_type.as_str() {
                    "none" => crate::tr("Kein"),
                    "subtle" => crate::tr("Dezent"),
                    "strong" => crate::tr("Stark"),
                    "red" => crate::tr("Rote Glut"),
                    _ => crate::tr("Film"),
                })
                .show_ui(ui, |ui| {
                    for (id, name) in [
                        ("none", crate::tr("Kein")),
                        ("subtle", crate::tr("Dezent")),
                        ("", crate::tr("Film")),
                        ("strong", crate::tr("Stark")),
                        ("red", crate::tr("Rote Glut")),
                    ] {
                        ui.selectable_value(&mut f.halation_type, id.into(), name);
                    }
                });
            if ui.button(crate::tr("LICHTPUNKTE")).clicked() {
                self.darkroom.probe_light.an = true;
            }
            subgroup(ui, "Weichzeichner", |ui| help::info(ui, "diffusion", "Weichzeichner", &help::FILM_DIFFUSION));
            egui::ComboBox::from_id_salt("film-diffusion").width(ui.available_width() - 8.0)
                .selected_text(match f.diffusion.as_str() {
                    "soft" => crate::tr("Weich"),
                    "mist" => crate::tr("Nebel"),
                    "bloom" => crate::tr("Leuchten"),
                    _ => crate::tr("Aus"),
                })
                .show_ui(ui, |ui| {
                    for (id, name) in [
                        ("", crate::tr("Aus")),
                        ("soft", crate::tr("Weich")),
                        ("mist", crate::tr("Nebel")),
                        ("bloom", crate::tr("Leuchten")),
                    ] {
                        ui.selectable_value(&mut f.diffusion, id.into(), name);
                    }
                });
            // directly below the menu: where the diffusion sits; the strength only while it is on (no grey rows)
            ui.add_space(4.0);
            ui.add_enabled_ui(!f.diffusion.is_empty(), |ui| {
                lr::segmented(
                    ui,
                    &mut f.diffusion_enlarger,
                    &[(false, "Objektiv"), (true, crate::tr("Vergrößerer"))],
                );
            });
            if f.diffusion.is_empty() {
                hint(ui, crate::tr("Stärke erst mit einem Weichzeichner."));
            } else {
                slider(ui, "Stärke", &mut f.diffusion_amount, 0.0..=1.0, 0.5);
            }
            let paper = !f.negative && !develop::film::stock(&f.stock).is_some_and(|s| s.positive);
            subgroup(ui, "Vorbelichtung", |ui| help::info(ui, "preflash", "Vorbelichtung", &help::FILM_PREFLASH));
            if !paper {
                hint(ui, crate::tr("Nur beim Abzug auf Papier."));
            }
            ui.add_enabled_ui(paper, |ui| {
                slider(ui, "Stärke", &mut f.preflash, 0.0..=1.0, 0.0);
                if !develop::film::stock(&f.stock).is_some_and(|s| s.monochrome) {
                    slider(ui, "Wärme", &mut f.preflash_warmth, -1.0..=1.0, 0.0);
                    slider(ui, "Tönung", &mut f.preflash_tint, -1.0..=1.0, 0.0);
                }
            });
            subgroup(ui, "Scan-Ausgabe", |ui| help::info(ui, "scan-levels", "Scan-Ausgabe", &help::FILM_SCAN));
            if paper {
                hint(ui, crate::tr("Nur ohne Papier (Diafilm oder Negativansicht)."));
            } else {
                if lr::switch_label(ui, &mut f.scan_auto, "Automatisch").clicked() && f.scan_auto {
                    self.darkroom.scan.ready = None;
                }
                if f.scan_auto {
                    hint(ui, crate::tr("Schwarz- und Weißpunkt kommen automatisch aus dem Bild."));
                } else {
                    slider(ui, "Schwarzpunkt", &mut f.scan_black, 0.0..=0.95, 0.0);
                    let min = f.scan_black as f64 + 0.01;
                    slider(ui, "Weißpunkt", &mut f.scan_white, min..=1.0, 1.0);
                }
            }
            ui.add_space(14.0);
            self.darkroom_curve_panel(ui, index);
            if ui.button(crate::tr("LOOK ALS LUT SPEICHERN …")).clicked() {
                self.darkroom_lut_open();
            }
        });
        if let Some(state) = egui::collapsing_header::CollapsingState::load(ui.ctx(), section_id) {
            let open = state.is_open();
            if open != initial {
                self.darkroom.advanced_open = Some(open);
                library::settings::set("darkroomAdvanced", serde_json::json!(open));
            }
        }
    }
}

/// Subgroup in "Advanced": the same subheading as everywhere in the sidebars ([`lr::subheading_with`]), `extra` (the
/// "?" of the group) at its right end; the subheading brings its own gap above.
pub(crate) fn subgroup(ui: &mut egui::Ui, title: &str, extra: impl FnOnce(&mut egui::Ui)) {
    lr::subheading_with(ui, title, 24.0, extra);
}

/// Short line why controls are not there.
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(crate::lr::TEXT_DIM())).wrap());
}
