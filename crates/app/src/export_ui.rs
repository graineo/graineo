//! Layout of the export dialog (and of the single-column export panel).
//!
//! Wide dialog (content ≥ [`TWO_COLUMNS_MIN`]): two equally wide columns without headings of their own, on the left
//! the sections "Bilder", "Format", "Größe und Metadaten", on the right "Ziel" and "Dateiname"; the template applies to
//! everything and sits in the dialog's title row. Below them a collapsed "Advanced" part (remembered) with the rarely used
//! options, again in two columns. Narrow: the same parts one below the other. Rows that do not apply
//! are greyed out; only the format detail rows below "Format" depend on the format and have no empty rows.
//! The name preview, conflicts, progress and the buttons sit in the dialog's fixed footer ([`crate::dialog::show_scrolling`]).

use eframe::egui;

use crate::lr;
use crate::{file_dialog, image_kind, jobs, App};
use develop::export::{Engine, Format, OutputSize};
use graineo_color::Space;

/// Width of the row labels.
pub(crate) const LABEL_W: f32 = 108.0;
/// Height of one row.
pub(crate) const ROW_H: f32 = 24.0;
/// Content width from which the dialog shows two columns.
pub(crate) const TWO_COLUMNS_MIN: f32 = 760.0;
/// Space between the two columns.
const COLUMN_GAP: f32 = 28.0;
/// Height of the status lines (name preview + conflicts, or progress).
pub(crate) const STATUS_H: f32 = 36.0;
/// Settings key: "Advanced" opened.
const ADVANCED_KEY: &str = "exportAdvanced";

/// What the left column found out about the photos (for the advanced part and the footer).
#[derive(Default)]
pub(crate) struct Scope {
    /// the photos "Export" takes
    pub items: Vec<usize>,
    /// left out: original not reachable
    pub unreachable: usize,
    has_x3f: bool,
    /// a finished image is written (JPEG, JPEG XL, TIFF 16, PNG; not the originals)
    finished: bool,
}

/// Section heading ("BILDER", "FORMAT", "ZIEL" …): capitals with a thin line to the right edge. The dialog has only
/// this one level (no column headings), "Advanced" looks the same.
pub(crate) fn section(ui: &mut egui::Ui, title: &str) {
    band(ui, title, 11.5, crate::lr::gray(0xbc), crate::lr::gray(0x36), 8.0);
}

fn band(ui: &mut egui::Ui, title: &str, size: f32, color: egui::Color32, line: egui::Color32, before: f32) -> egui::Response {
    ui.add_space(before);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), size + 8.0), egui::Sense::hover());
    paint_band(ui.painter(), rect, title, size, color, line);
    ui.add_space(2.0);
    r
}

fn paint_band(painter: &egui::Painter, rect: egui::Rect, title: &str, size: f32, color: egui::Color32, line: egui::Color32) {
    let g = lr::caps_spaced(painter, &crate::tr(title).to_uppercase(), lr::semibold(size), color, 0.6);
    painter.galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0), g.clone(), color);
    let x = rect.left() + g.size().x + 10.0;
    if x < rect.right() {
        painter.line_segment([egui::pos2(x, rect.center().y), egui::pos2(rect.right(), rect.center().y)], egui::Stroke::new(1.0, line));
    }
}

/// Row: label on the left (fixed width), controls on the right, fixed height; clipped to the column.
pub(crate) fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    row_if(ui, label, true, add);
}

/// [`row`], greyed out when `enabled` is false (the row keeps its place).
pub(crate) fn row_if(ui: &mut egui::Ui, label: &str, enabled: bool, add: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(w, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(egui::vec2(w, ROW_H));
        let clip = ui.clip_rect();
        ui.set_clip_rect(clip.intersect(egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), clip.y_range())));
        ui.add_enabled_ui(enabled, |ui| {
            ui.allocate_ui_with_layout(egui::vec2(LABEL_W, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_width(LABEL_W);
                if !label.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(crate::tr(label)).color(lr::TEXT_DIM())).truncate());
                }
            });
            add(ui);
        });
    });
}

/// Segments when they fit into the row, otherwise a drop-down list (same choice, same tooltips).
pub(crate) fn choice<T: PartialEq + Clone>(ui: &mut egui::Ui, id: &str, value: &mut T, options: &[(T, &str)], tips: &[&str]) -> bool {
    const PAD: f32 = 8.0;
    let font = egui::TextStyle::Button.resolve(ui.style());
    let need: f32 = options.iter().map(|(_, l)| ui.painter().layout_no_wrap(crate::tr(l).to_string(), font.clone(), lr::TEXT()).size().x + 2.0 * PAD).sum();
    if need <= ui.available_width() - 2.0 {
        return lr::segmented_tips(ui, value, options, tips, PAD);
    }
    let shown = options.iter().find(|(v, _)| v == value).map(|(_, l)| crate::tr(l).to_string()).unwrap_or_default();
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(shown).width(ui.available_width().clamp(80.0, 240.0)).show_ui(ui, |ui| {
        for (i, (v, l)) in options.iter().enumerate() {
            let r = ui.selectable_label(value == v, crate::tr(l));
            if let Some(t) = tips.get(i).filter(|t| !t.is_empty()) {
                lr::hover_tip(ui, &r, t);
            }
            if r.clicked() && value != v {
                *value = v.clone();
                changed = true;
            }
        }
    });
    changed
}

/// A fixed-width column (top down): its rows never get more room than `width`, whatever a row inside asks for.
fn column<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, 100_000.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let r = add(&mut child);
    let h = child.min_rect().height();
    ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::hover());
    r
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Jpeg,
    Jxl,
    Tiff,
    Png,
    Dng,
    Linear,
    Exr,
    Original,
}

impl App {
    /// Single-column export panel with its own main button below; also the content check of the tests.
    /// `in_dialog`: without its own start button. Returns: the images that "Export" would take.
    pub(crate) fn export_controls(&mut self, ui: &mut egui::Ui, in_dialog: bool) -> Vec<usize> {
        if !in_dialog {
            row(ui, crate::tr("Vorlage"), |ui| self.export_template_controls(ui));
        }
        let scope = self.export_body(ui, false);
        ui.add_space(10.0);
        self.export_status_lines(ui, &scope);
        if !in_dialog {
            ui.add_space(6.0);
            // without a folder the button asks for one first (never a button that does nothing)
            let can = !self.exporting && !scope.items.is_empty();
            let label = self.export_button_label(scope.items.len());
            let go = lr::main_button_min(ui, &label, can, ui.available_width());
            if go.clicked() && self.export_requested(ui.ctx(), scope.items.clone()) {
                self.show_export = false;
            }
        }
        scope.items
    }

    /// Everything above the footer: the two parts side by side (`two_columns`) or one below the other, and "Advanced".
    pub(crate) fn export_body(&mut self, ui: &mut egui::Ui, two_columns: bool) -> Scope {
        ui.spacing_mut().item_spacing.y = 3.0;
        let width = ui.available_width();
        let advanced = *self.export_names_ui.advanced.get_or_insert_with(|| library::settings::value(ADVANCED_KEY).as_bool().unwrap_or(false));
        let scope;
        if two_columns {
            let col = ((width - COLUMN_GAP) / 2.0).floor();
            let mut s = Scope::default();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                column(ui, col, |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    s = self.export_what(ui);
                });
                ui.add_space(COLUMN_GAP);
                column(ui, col, |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    self.export_where(ui, &s.items);
                });
            });
            scope = s;
            ui.add_space(4.0);
            if self.export_advanced_header(ui, &scope, advanced) && advanced {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    column(ui, col, |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        self.export_what_advanced(ui, &scope);
                    });
                    ui.add_space(COLUMN_GAP);
                    column(ui, col, |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        self.export_where_advanced(ui);
                    });
                });
            }
        } else {
            scope = self.export_what(ui);
            self.export_where(ui, &scope.items);
            ui.add_space(4.0);
            if self.export_advanced_header(ui, &scope, advanced) && advanced {
                self.export_what_advanced(ui, &scope);
                self.export_where_advanced(ui);
            }
        }
        scope
    }

    /// Header "▸ Advanced" (click toggles, remembered); collapsed it names what is set there and differs from the
    /// default. Returns true (always drawn).
    fn export_advanced_header(&mut self, ui: &mut egui::Ui, scope: &Scope, open: bool) -> bool {
        let mut set = Vec::new();
        if self.export_as_print && scope.finished {
            set.push(crate::tr("Abzug").to_string());
        }
        if scope.has_x3f && scope.finished && self.export_engine == Engine::Own {
            set.push(crate::tr("Entwurf (schnell)").to_string());
        }
        if self.export_options.gps && scope.finished && self.export_options.metadata != develop::export::Metadata::None {
            set.push(crate::tr("GPS").to_string());
        }
        match self.export_naming.subfolder {
            crate::export_names::Subfolder::Date => set.push(crate::tr("Unterordner nach Datum").to_string()),
            crate::export_names::Subfolder::Folder => set.push(crate::tr("Unterordner nach Ordner").to_string()),
            crate::export_names::Subfolder::Custom => set.push(crate::trf!("Unterordner „{a}“", a = self.export_naming.subfolder_text.trim())),
            crate::export_names::Subfolder::None => {}
        }
        // a sub-heading with a disclosure triangle; collapsed, the line carries what is set there
        ui.add_space(8.0);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 19.5), egui::Sense::click());
        let color = if r.hovered() { crate::lr::gray(0xe2) } else { crate::lr::gray(0xbc) };
        let title = format!("{} {}", if open { "▾" } else { "▸" }, crate::tr("Erweitert"));
        let mut line = rect;
        if !open && !set.is_empty() {
            // right end of the line, shortened when the dialog is narrow
            let area = egui::Rect::from_min_max(egui::pos2(rect.center().x, rect.top()), rect.max);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::right_to_left(egui::Align::Center)));
            let summary = child.add(egui::Label::new(egui::RichText::new(set.join(" · ")).small().color(lr::TEXT_DIM())).truncate().selectable(false));
            line = rect.with_max_x(summary.rect.left() - 10.0);
        }
        paint_band(ui.painter(), line, &title, 11.5, color, crate::lr::gray(0x36));
        ui.add_space(2.0);
        lr::hover_tip(ui, &r, crate::tr("Abzug, Foveon, GPS, Stichwörter, Unterordner, Nummer"));
        if r.clicked() {
            self.export_names_ui.advanced = Some(!open);
            library::settings::set(ADVANCED_KEY, serde_json::Value::Bool(!open));
        }
        true
    }

    /// Left column: photos, format with its detail rows, colour space, size, metadata.
    fn export_what(&mut self, ui: &mut egui::Ui) -> Scope {
        let mono = self.settings().is_some_and(|s| s.monochrome);

        // ── Photos
        let sel = self.selection();
        let visible = self.visible();
        let all = self.entries.len();
        let mut options: Vec<(usize, String)> = vec![(0, if sel.len() > 1 { crate::trf!("Auswahl ({a})", a = sel.len()) } else { crate::tr("Dieses Bild").into() })];
        if visible.len() != all {
            options.push((1, crate::trf!("Gefilterte ({a})", a = visible.len())));
        }
        options.push((2, crate::trf!("Alle ({all})", all = all)));
        let labels: Vec<(usize, &str)> = options.iter().map(|(i, l)| (*i, l.as_str())).collect();
        let mut scope = self.export_scope.min(2);
        if scope == 1 && visible.len() == all {
            scope = 0;
        }
        section(ui, crate::tr("Bilder"));
        row(ui, crate::tr("Umfang"), |ui| {
            choice(ui, "export-scope", &mut scope, &labels, &[]);
        });
        self.export_scope = scope;
        let items: Vec<usize> = match scope {
            1 => visible,
            2 => (0..all).collect(),
            _ => sel,
        };
        // originals on an unreachable location cannot be exported (the previews are only for viewing)
        let unreachable = items.iter().filter(|&&i| self.entries[i].state == 2).count();
        let items: Vec<usize> = items.into_iter().filter(|&i| self.entries[i].state != 2).collect();
        let x3f_only = !items.is_empty() && items.iter().all(|&i| image_kind::of(&self.entries[i].path) == image_kind::ImageKind::Foveon);
        let has_x3f = items.iter().any(|&i| image_kind::of(&self.entries[i].path) == image_kind::ImageKind::Foveon);
        // HDR on for one of the photos (automatic: merged HDR photos and finished images that may bring a gain map)
        let has_hdr = items.iter().any(|&i| {
            let e = &self.entries[i];
            let auto = library::merge::is_virtual_path(&e.path) || image_kind::of(&e.path) == image_kind::ImageKind::Done;
            e.settings.hdr.is_on(auto, e.settings.film.is_active())
        });
        // the HDR option starts as the photos are: on when one has HDR on (again for every other selection)
        {
            let key = egui::Id::unique("export-hdr-preset");
            let sig = items.iter().fold(items.len() as u64, |h, &i| h.wrapping_mul(31).wrapping_add(i as u64));
            if ui.data(|d| d.get_temp::<u64>(key)) != Some(sig) {
                ui.data_mut(|d| d.insert_temp(key, sig));
                self.export_gain_map = has_hdr;
            }
        }
        // scene-linear formats (TIFF 32 bit, OpenEXR, linear DNG) for every developable RAW: Sigma X3F and the
        // other cameras; the camera-profile DNG and the sensor layers stay Sigma only
        let raw_ok = !items.is_empty() && items.iter().all(|&i| matches!(image_kind::of(&self.entries[i].path), image_kind::ImageKind::Foveon | image_kind::ImageKind::Raw));
        let allowed = self.export_format.is_finished_image() || (raw_ok && (self.export_format.is_scene_linear() || self.export_format == Format::Dng)) || x3f_only;
        if !allowed {
            self.export_format = Format::Jpeg { quality: 92 };
        }

        // ── Format
        section(ui, crate::tr("Format"));
        let mut kind = match self.export_format {
            _ if self.export_original => Kind::Original,
            Format::Jpeg { .. } => Kind::Jpeg,
            Format::Jxl { .. } => Kind::Jxl,
            Format::Tiff16 | Format::TiffFloat | Format::RawLayers => Kind::Tiff,
            Format::Png16 => Kind::Png,
            Format::DngProfiles => Kind::Dng,
            Format::Dng => Kind::Linear,
            Format::Exr { .. } => Kind::Exr,
        };
        let mut kinds = vec![(Kind::Jpeg, "JPEG"), (Kind::Jxl, "JPEG XL"), (Kind::Tiff, "TIFF"), (Kind::Png, "PNG")];
        if x3f_only {
            kinds.push((Kind::Dng, "DNG (Raw)"));
        }
        if raw_ok {
            kinds.push((Kind::Linear, crate::tr("Lineares DNG")));
            kinds.push((Kind::Exr, "OpenEXR"));
        }
        kinds.push((Kind::Original, crate::tr("Original")));
        // what each format is: in the tooltip
        let tips: Vec<&str> = kinds
            .iter()
            .map(|(k, _)| match k {
                Kind::Jpeg => crate::tr("Fertiges Bild zum Teilen, für Web und Handy."),
                Kind::Jxl => crate::tr("JPEG XL (.jxl): kleiner als JPEG bei gleicher Qualität, auch verlustfrei und mit 16 Bit"),
                Kind::Tiff | Kind::Png => crate::tr("Fertiges Bild ohne Verlust, für Druck und Bildbearbeitung."),
                Kind::Dng if mono => crate::tr("Foveon-Schwarzweiß als Raw; deine Regler kommen als XMP mit."),
                Kind::Dng => crate::tr("Raw mit dem Farbmodus der Kamera als Kameraprofil; deine Regler kommen als XMP mit."),
                Kind::Linear if !x3f_only => crate::tr("Lineares DNG aus graineos Entwicklung: Kamerawerte ohne Tonkurve, nichts beschnitten, auch Lichter über Weiß bleiben erhalten."),
                Kind::Linear => crate::tr("Raw aus graineos eigener Entwicklung, ohne Sigma-Profile."),
                Kind::Exr => crate::tr("Szenenlinear ohne Tonkurve, nichts beschnitten."),
                Kind::Original => crate::tr("Die Datei unverändert, geprüft kopiert (SHA-256); nichts wird überschrieben."),
            })
            .collect();
        row(ui, crate::tr("Format"), |ui| {
            // room for the "?" card on the right
            ui.allocate_ui_with_layout(egui::vec2((ui.available_width() - 24.0).max(60.0), ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                choice(ui, "export-format", &mut kind, &kinds, &tips);
            });
            crate::help::info(ui, "export-verlustfrei", crate::tr("Verlustfrei exportieren"), &crate::help::EXPORT_LOSSLESS);
        });
        self.export_original = kind == Kind::Original;
        let before_options = self.export_options.clone();
        self.export_format = match kind {
            Kind::Jpeg => Format::Jpeg {
                quality: match self.export_format {
                    Format::Jpeg { quality } => quality,
                    _ => 92,
                },
            },
            Kind::Jxl => match self.export_format {
                f @ Format::Jxl { .. } => f,
                _ => Format::JXL_DEFAULT,
            },
            Kind::Tiff => match self.export_format {
                Format::TiffFloat if raw_ok => self.export_format,
                Format::RawLayers if x3f_only => self.export_format,
                _ => Format::Tiff16,
            },
            Kind::Png => Format::Png16,
            Kind::Dng => Format::DngProfiles,
            Kind::Linear => Format::Dng,
            Kind::Exr => match self.export_format {
                Format::Exr { half } => Format::Exr { half },
                _ => Format::Exr { half: true },
            },
            // the format stays for the next time; it does not apply to the originals
            Kind::Original => self.export_format,
        };
        self.export_format_details(ui, kind, x3f_only, raw_ok, has_hdr, &items);
        if self.export_options != before_options {
            if let Ok(v) = serde_json::to_value(&self.export_options) {
                library::settings::set("exportOptions", v);
            }
        }

        // ── Colour space: of the finished image, of the scene-linear formats, otherwise greyed out
        let finished = self.export_format.is_finished_image() && !self.export_original;
        if self.export_format.is_scene_linear() && !self.export_original {
            let before = self.export_options.linear_space;
            row(ui, crate::tr("Farbraum"), |ui| {
                use graineo_color::LinearSpace as L;
                choice(ui, "export-linear-space", &mut self.export_options.linear_space, &[(L::Rec2020, "Rec. 2020 linear"), (L::AcesCg, "ACEScg"), (L::ProPhoto, "ProPhoto linear")], &[]);
            });
            if self.export_options.linear_space != before {
                if let Ok(v) = serde_json::to_value(&self.export_options) {
                    library::settings::set("exportOptions", v);
                }
            }
        } else {
            let before = self.export_color_space;
            row_if(ui, crate::tr("Farbraum"), finished, |ui| {
                choice(
                    ui,
                    "export-color-space",
                    &mut self.export_color_space,
                    &[(Space::Srgb, "sRGB"), (Space::DisplayP3, "Display P3"), (Space::AdobeRgb, "Adobe RGB"), (Space::ProPhotoRgb, "ProPhoto")],
                    &[
                        crate::tr("Für Web, E-Mail, Handy und fast alle Programme."),
                        crate::tr("Kräftigere Farben auf Apple-Bildschirmen; ohne Farbmanagement etwas flau."),
                        crate::tr("Für Druckereien und Labore, die Adobe RGB verlangen."),
                        crate::tr("Größter Farbraum zum Weiterbearbeiten, am besten als TIFF 16 Bit."),
                    ],
                );
            });
            if self.export_color_space != before {
                if let Ok(v) = serde_json::to_value(self.export_color_space) {
                    library::settings::set("exportColorSpace", v);
                }
            }
        }

        // ── Colour of the scene-linear formats of Sigma photos: with the colour mode or camera matrix only
        {
            let before = self.export_options.camera_matrix_only;
            let on = self.export_format.is_scene_linear() && !self.export_original && has_x3f;
            let row_top = ui.cursor().top();
            row_if(ui, crate::tr("Farbe"), on, |ui| {
                choice(
                    ui,
                    "export-camera-matrix",
                    &mut self.export_options.camera_matrix_only,
                    &[(false, crate::tr("Mit Farbmodus")), (true, crate::tr("Nur Kameramatrix"))],
                    &[crate::tr("Farbe wie im gewählten Farbmodus der Entwicklung"), crate::tr("Kalibrierte Kamerafarbe ohne den Look des Farbmodus")],
                );
            });
            if !on {
                // greyed out: say why (a disabled widget reports no hover, so a hover area over the row)
                let row = egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), row_top), egui::pos2(ui.max_rect().right(), ui.cursor().top()));
                let r = ui.interact(row, ui.id().with("export-camera-matrix-locked"), egui::Sense::hover());
                let why = if !has_x3f {
                    crate::tr("Nur für Sigma-Fotos (X3F) in den Formaten TIFF 32 Bit und OpenEXR")
                } else {
                    crate::tr("Nur für die Formate TIFF 32 Bit und OpenEXR")
                };
                lr::hover_tip(ui, &r, why);
            }
            if self.export_options.camera_matrix_only != before {
                if let Ok(v) = serde_json::to_value(&self.export_options) {
                    library::settings::set("exportOptions", v);
                }
            }
        }

        // ── Size
        section(ui, crate::tr("Größe und Metadaten"));
        self.export_size_row(ui, finished);
        self.export_resize_row(ui, finished);

        // ── Metadata (never serial numbers or names of people)
        let before = self.export_options.metadata;
        row_if(ui, crate::tr("Metadaten"), finished, |ui| {
            use develop::export::Metadata as M;
            choice(
                ui,
                "export-metadata",
                &mut self.export_options.metadata,
                &[(M::None, crate::tr("Keine")), (M::Shooting, crate::tr("Aufnahmedaten")), (M::All, crate::tr("Alles"))],
                &[
                    crate::tr("Keine Metadaten: nur Bild und Farbprofil"),
                    crate::tr("Aufnahmedaten: Kamera, Objektiv, Blende, Zeit, ISO, Datum, Urheber"),
                    crate::tr("Alles: zusätzlich Stichwörter, Beschreibung, Sterne, Etikett"),
                ],
            );
        });
        if self.export_options.metadata != before {
            if let Ok(v) = serde_json::to_value(&self.export_options) {
                library::settings::set("exportOptions", v);
            }
        }
        self.export_sharpen_rows(ui, finished);
        Scope { items, unreachable, has_x3f, finished }
    }

    /// The rows below "Format": quality, bit depth, compression, … of the chosen format; only the rows the format has,
    /// without empty rows (the HDR row exists only for formats that can carry a gain map, i.e. JPEG).
    fn export_format_details(&mut self, ui: &mut egui::Ui, kind: Kind, x3f_only: bool, raw_ok: bool, has_hdr: bool, items: &[usize]) {
        match kind {
            Kind::Jpeg => {
                if let Format::Jpeg { quality } = self.export_format {
                    let mut v = quality;
                    row(ui, crate::tr("Qualität"), |ui| quality_slider(ui, &mut v));
                    self.export_format = Format::Jpeg { quality: v };
                }
                self.export_hdr_row(ui, has_hdr);
            }
            Kind::Jxl => {
                if let Format::Jxl { quality, mut lossless, mut bits16 } = self.export_format {
                    let mut q = quality;
                    row(ui, crate::tr("Kompression"), |ui| {
                        choice(ui, "export-jxl-lossless", &mut lossless, &[(false, crate::tr("verlustbehaftet")), (true, crate::tr("verlustfrei"))], &[]);
                    });
                    if lossless {
                        row(ui, crate::tr("Bit-Tiefe"), |ui| {
                            choice(ui, "export-jxl-bits", &mut bits16, &[(false, crate::tr("8 Bit")), (true, crate::tr("16 Bit"))], &[]);
                        });
                    } else {
                        row(ui, crate::tr("Qualität"), |ui| quality_slider(ui, &mut q));
                    }
                    self.export_format = Format::Jxl { quality: q, lossless, bits16 };
                }
            }
            Kind::Tiff => {
                #[derive(Clone, Copy, PartialEq)]
                enum Depth {
                    B16,
                    Float,
                    Layers,
                }
                let mut d = match self.export_format {
                    Format::TiffFloat => Depth::Float,
                    Format::RawLayers => Depth::Layers,
                    _ => Depth::B16,
                };
                let mut depths = vec![(Depth::B16, crate::tr("16 Bit"))];
                if raw_ok {
                    depths.push((Depth::Float, crate::tr("32 Bit linear")));
                }
                if x3f_only {
                    depths.push((Depth::Layers, crate::tr("Sensorschichten (unbearbeitet)")));
                }
                row(ui, crate::tr("Bit-Tiefe"), |ui| {
                    choice(ui, "export-tiff-depth", &mut d, &depths, &[]);
                });
                self.export_format = match d {
                    Depth::Float => Format::TiffFloat,
                    Depth::Layers => Format::RawLayers,
                    Depth::B16 => Format::Tiff16,
                };
                row(ui, crate::tr("Kompression"), |ui| {
                    use develop::tiffw::Compression as C;
                    choice(ui, "export-tiff-compression", &mut self.export_options.tiff_compression, &[(C::None, crate::tr("keine")), (C::Deflate, "ZIP"), (C::Lzw, "LZW")], &[]);
                });
            }
            Kind::Exr => {
                let mut half = matches!(self.export_format, Format::Exr { half: true });
                row(ui, crate::tr("Bit-Tiefe"), |ui| {
                    choice(ui, "export-exr-depth", &mut half, &[(true, crate::tr("16 Bit (half)")), (false, crate::tr("32 Bit"))], &[]);
                });
                self.export_format = Format::Exr { half };
            }
            Kind::Linear => {
                row(ui, crate::tr("Bit-Tiefe"), |ui| {
                    choice(ui, "export-dng-depth", &mut self.export_options.dng_float, &[(false, crate::tr("16 Bit")), (true, crate::tr("32 Bit Gleitkomma"))], &[]);
                });
                if x3f_only {
                    row(ui, "", |ui| {
                        ui.checkbox(&mut self.export_options.dng_embed_original, crate::tr("Original-X3F einbetten"));
                    });
                }
            }
            Kind::Original => {
                // the one hint that explains a decision: the originals are copied as they are
                row(ui, "", |ui| {
                    let r = ui.add(egui::Label::new(egui::RichText::new(crate::tr("Unverändert, geprüft kopiert (SHA-256)")).small().color(lr::TEXT_DIM())).truncate());
                    lr::hover_tip(ui, &r, crate::tr("Die Datei unverändert, geprüft kopiert (SHA-256); nichts wird überschrieben."));
                });
                let pairs = items.iter().filter(|&&i| self.lib.sidecar_jpeg_of(self.entries[i].image).is_some()).count();
                row(ui, "", |ui| {
                    let r = ui.checkbox(&mut self.export_original_sidecars, crate::tr("XMP-Begleitdatei"));
                    lr::hover_tip(ui, &r, crate::tr("Begleitdatei (XMP) mit Bewertung und Entwicklung"));
                    if pairs > 0 {
                        let r = ui.checkbox(&mut self.export_original_pair, crate::tr("Kamera-JPEG"));
                        lr::hover_tip(ui, &r, crate::tr("Kamera-JPEG der RAW+JPEG-Paare"));
                    }
                });
            }
            Kind::Png | Kind::Dng => {}
        }
    }

    /// Row "HDR": Aus | Gain-Map, shown only for JPEG; greyed unless sRGB/Display P3 and a photo has HDR on.
    fn export_hdr_row(&mut self, ui: &mut egui::Ui, has_hdr: bool) {
        let on = has_hdr && matches!(self.export_color_space, Space::Srgb | Space::DisplayP3);
        row_if(ui, crate::tr("HDR"), on, |ui| {
            choice(ui, "export-hdr", &mut self.export_gain_map, &[(false, "Aus"), (true, "Gain-Map")], &["", "Nur JPEG, sRGB oder Display P3"]);
        });
    }

    /// Row "Größe": full resolution or a limit (kind, value, a few common values when there is room).
    fn export_size_row(&mut self, ui: &mut egui::Ui, finished: bool) {
        #[derive(Clone, Copy, PartialEq)]
        enum G {
            Full,
            Long,
            Short,
            Frame,
            Mp,
        }
        let before = (self.export_limit_size, self.export_size);
        row_if(ui, crate::tr("Größe"), finished, |ui| {
            let mut g = match (self.export_limit_size, self.export_size) {
                (false, _) => G::Full,
                (_, OutputSize::LongEdge(_)) => G::Long,
                (_, OutputSize::ShortEdge(_)) => G::Short,
                (_, OutputSize::BoundingBox { .. }) => G::Frame,
                (_, OutputSize::Megapixel(_)) => G::Mp,
            };
            let old = g;
            let options = [(G::Full, crate::tr("Volle Auflösung")), (G::Long, crate::tr("Lange Kante")), (G::Short, crate::tr("Kurze Kante")), (G::Frame, crate::tr("Breite × Höhe")), (G::Mp, "Megapixel")];
            let shown = options.iter().find(|(v, _)| *v == g).map(|(_, l)| *l).unwrap_or_default();
            let r = egui::ComboBox::from_id_salt("export-size-kind").selected_text(shown).width(128.0).show_ui(ui, |ui| {
                for (v, l) in &options {
                    ui.selectable_value(&mut g, *v, *l);
                }
            });
            lr::hover_tip(ui, &r.response, crate::tr("Hoch- wie Querformat; kleinere Bilder werden nicht vergrößert."));
            if g != old {
                self.export_limit_size = g != G::Full;
                match g {
                    G::Long if !matches!(self.export_size, OutputSize::LongEdge(_)) => self.export_size = OutputSize::LongEdge(2048),
                    G::Short if !matches!(self.export_size, OutputSize::ShortEdge(_)) => self.export_size = OutputSize::ShortEdge(1080),
                    G::Frame if !matches!(self.export_size, OutputSize::BoundingBox { .. }) => self.export_size = OutputSize::BoundingBox { width: 1920, height: 1080 },
                    G::Mp if !matches!(self.export_size, OutputSize::Megapixel(_)) => self.export_size = OutputSize::Megapixel(12.0),
                    _ => {}
                }
            }
            if !self.export_limit_size {
                return;
            }
            let px = |ui: &mut egui::Ui, n: &mut u32| {
                lr::text_field(ui, egui::DragValue::new(n).range(64..=30000).clamp_existing_to_range(false).speed(10.0));
            };
            match &mut self.export_size {
                OutputSize::LongEdge(n) | OutputSize::ShortEdge(n) => {
                    px(ui, n);
                    ui.label("px");
                    // common values only when they fit (never a second line)
                    if ui.available_width() >= 120.0 {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for v in [1080u32, 2048, 3840] {
                            if ui.add(egui::Button::new(egui::RichText::new(v.to_string()).small()).selected(*n == v)).clicked() {
                                *n = v;
                            }
                        }
                    }
                }
                OutputSize::BoundingBox { width, height } => {
                    px(ui, width);
                    ui.label("×");
                    px(ui, height);
                    ui.label("px");
                }
                OutputSize::Megapixel(mp) => {
                    lr::text_field(ui, egui::DragValue::new(mp).range(0.5..=100.0).clamp_existing_to_range(false).speed(0.1).max_decimals(1));
                    ui.label("MP");
                }
            }
        });
        if (self.export_limit_size, self.export_size) != before {
            library::settings::set("exportLimitSize", serde_json::Value::Bool(self.export_limit_size));
            if let Ok(v) = serde_json::to_value(self.export_size) {
                library::settings::set("exportSize", v);
            }
        }
    }

    /// Row "Verkleinern": resampling filter (only acts when a size limit is set; otherwise greyed out, the row stays).
    fn export_resize_row(&mut self, ui: &mut egui::Ui, finished: bool) {
        use develop::resize::ResizeFilter as F;
        let before = self.export_resize;
        row_if(ui, crate::tr("Verkleinerung"), finished && self.export_limit_size, |ui| {
            let name = |f: F| match f {
                F::Sharp => crate::tr("Scharf"),
                F::Balanced => crate::tr("Ausgewogen"),
                F::Soft => crate::tr("Weich"),
                F::Smooth => crate::tr("Glatt"),
            };
            let brief = |f: F| match f {
                F::Sharp => crate::tr("sehr scharf, kann an harten Kanten leicht saumen"),
                F::Balanced => crate::tr("fast so scharf, weniger Säume"),
                F::Soft => crate::tr("ruhiger, kaum Säume, mindert Moiré"),
                F::Smooth => crate::tr("sehr glatt, keine Säume, dämpft Moiré und Rauschen"),
            };
            let detail = |f: F| match f {
                F::Sharp => crate::tr("Lanczos, 3 Lappen (Fensterbreite 3 Pixel je Seite). Der bisherige Standard."),
                F::Balanced => crate::tr("Catmull-Rom, kubisch (B 0, C 0,5; Fensterbreite 2). Etwas weniger Überschwingen als Lanczos."),
                F::Soft => crate::tr("Hann-gefenstertes Sinc (Fensterbreite 3), in XnView und ImageMagick „Hanning“."),
                F::Smooth => crate::tr("Mitchell-Netravali, kubisch (B = C = 1/3; Fensterbreite 2). Kaum Überschwingen."),
            };
            let r = egui::ComboBox::from_id_salt("export-resize").selected_text(name(self.export_resize)).width(128.0).show_ui(ui, |ui| {
                for f in F::ALL {
                    let r = ui.selectable_value(&mut self.export_resize, f, format!("{} – {}", name(f), brief(f)));
                    lr::hover_tip(ui, &r, detail(f));
                }
            });
            lr::hover_tip(ui, &r.response, detail(self.export_resize));
        });
        if self.export_resize != before {
            library::settings::set("exportResize", serde_json::Value::String(self.export_resize.key().into()));
        }
    }

    /// Section "Ausgabeschärfung": target (off/screen/matte/glossy print) and strength; only for finished images.
    fn export_sharpen_rows(&mut self, ui: &mut egui::Ui, finished: bool) {
        use develop::output_sharpen::{Amount as A, Target as T};
        let before = self.export_sharpen;
        section(ui, crate::tr("Ausgabeschärfung"));
        row_if(ui, crate::tr("Schärfen für"), finished, |ui| {
            choice(
                ui,
                "export-sharpen-target",
                &mut self.export_sharpen.target,
                &[(T::Off, crate::tr("Aus")), (T::Screen, crate::tr("Bildschirm")), (T::PrintMatte, crate::tr("Druck matt")), (T::PrintGlossy, crate::tr("Druck glänzend"))],
                &[
                    "",
                    crate::tr("Nach dem Verkleinern auf die Ausgabegröße geschärft, Radius in Ausgabepixeln"),
                    crate::tr("Für Drucke auf mattem Papier: etwas größerer Radius, kräftiger"),
                    crate::tr("Für Drucke auf glänzendem Papier: feiner Radius"),
                ],
            );
        });
        row_if(ui, crate::tr("Stärke"), finished && !self.export_sharpen.is_off(), |ui| {
            // "Hoch" alone is the portrait orientation in the catalogue: own key, German text unchanged
            let high = crate::menu::i18n::german_or("Hoch", crate::tr("Hoch (Schärfe)"));
            choice(ui, "export-sharpen-amount", &mut self.export_sharpen.amount, &[(A::Low, crate::tr("Niedrig")), (A::Standard, crate::tr("Normal")), (A::High, high)], &[]);
        });
        if self.export_sharpen != before {
            if let Ok(v) = serde_json::to_value(self.export_sharpen) {
                library::settings::set("exportSharpen", v);
            }
        }
    }

    /// Left part of "Advanced": print border, Foveon quality, GPS, keywords (greyed out where they do not apply).
    fn export_what_advanced(&mut self, ui: &mut egui::Ui, scope: &Scope) {
        row_if(ui, crate::tr("Abzug"), scope.finished, |ui| {
            let r = ui.checkbox(&mut self.export_as_print, crate::tr("Papier- und Filmrand mitnehmen"));
            lr::hover_tip(ui, &r, crate::tr("Papier- und Filmrand aus der Dunkelkammer mitnehmen"));
        });
        row_if(ui, crate::tr("Foveon"), scope.has_x3f && scope.finished, |ui| {
            choice(
                ui,
                "export-engine",
                &mut self.export_engine,
                &[(Engine::Spp, crate::tr("Beste Qualität")), (Engine::Own, crate::tr("Entwurf (schnell)"))],
                &[
                    crate::tr("Volle Foveon-Verarbeitung wie in der Vorschau, etwa 1–3 s je Bild."),
                    crate::tr("Entwurf: ohne Foveon-Schärfung und ohne erweiterte Rauschminderung, etwas weicher, etwa 1 s je Bild."),
                ],
            );
        });
        let metadata = scope.finished && self.export_options.metadata != develop::export::Metadata::None;
        let before = self.export_options.gps;
        row_if(ui, crate::tr("GPS"), metadata, |ui| {
            let r = ui.checkbox(&mut self.export_options.gps, crate::tr("Standort (GPS) mitschreiben"));
            lr::hover_tip(ui, &r, crate::tr("Nur wenn das Original einen Standort hat"));
        });
        if self.export_options.gps != before {
            if let Ok(v) = serde_json::to_value(&self.export_options) {
                library::settings::set("exportOptions", v);
            }
        }
        // keywords: parent terms in dc:subject (default on), the hierarchy (lr:hierarchicalSubject) optional
        let personal = scope.finished && self.export_options.metadata == develop::export::Metadata::All;
        row_if(ui, crate::tr("Stichwörter"), personal, |ui| crate::keyword_export::controls(ui, personal));
    }

    /// Right column: template, folder, name with placeholder chips, existing files, reveal afterwards.
    fn export_where(&mut self, ui: &mut egui::Ui, items: &[usize]) {
        section(ui, crate::tr("Ziel"));
        let before = self.export_naming.clone();
        row(ui, crate::tr("Speichern in"), |ui| {
            choice(
                ui,
                "export-target",
                &mut self.export_naming.beside,
                &[(false, crate::tr("Ordner")), (true, crate::tr("Neben dem Original"))],
                &[crate::tr("Alle Bilder in einen gewählten Ordner"), crate::tr("Jedes Bild in den Ordner seines Originals (mit Unterordner aus „Erweitert“)")],
            );
        });
        self.export_naming_commit(&before);
        let beside = self.export_naming.beside;
        row_if(ui, crate::tr("Ordner"), !beside, |ui| {
            let none = self.export_dir.is_none();
            let dir = self.export_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| crate::tr("– noch kein Ordner –").into());
            let button_w = ui.painter().layout_no_wrap(crate::tr("Ändern …").to_string(), egui::TextStyle::Button.resolve(ui.style()), lr::TEXT()).size().x + 2.0 * ui.spacing().button_padding.x;
            let w = (ui.available_width() - button_w - ui.spacing().item_spacing.x).max(40.0);
            let r = ui.add_sized([w, 18.0], egui::Label::new(egui::RichText::new(&dir).color(if none { lr::TEXT_DIM() } else { lr::TEXT() })).truncate());
            if !none {
                lr::hover_tip(ui, &r, &dir);
            }
            if ui.button(crate::tr("Ändern …")).clicked() {
                file_dialog::target_folder(ui.ctx(), "export-ordner", |d| d);
            }
        });
        if let Some(d) = file_dialog::fetch_one("export-ordner") {
            self.set_export_dir(d);
        }
        row(ui, "", |ui| {
            if ui.checkbox(&mut self.reveal_after_export, crate::tr("Danach im Finder zeigen")).changed() {
                // remembered like the default in the preferences (the same value, "export" → "reveal")
                let mut e = library::settings::value("export");
                if !e.is_object() {
                    e = serde_json::json!({"format": "jpeg", "quality": 92, "engine": "spp"});
                }
                e["reveal"] = serde_json::json!(self.reveal_after_export);
                library::settings::set("export", e);
            }
        });
        section(ui, crate::tr("Dateiname"));
        self.export_name_rows(ui, items);
    }

    /// Right part of "Advanced": subfolders, number start and digits.
    fn export_where_advanced(&mut self, ui: &mut egui::Ui) {
        self.export_numbering_rows(ui);
    }

    /// The fixed status lines: progress while exporting, otherwise the name preview and the conflicts
    /// (or the last export's message). Always [`STATUS_H`] high.
    pub(crate) fn export_status_lines(&mut self, ui: &mut egui::Ui, scope: &Scope) {
        let w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(w, STATUS_H), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(egui::vec2(w, STATUS_H));
            ui.spacing_mut().item_spacing.y = 2.0;
            if self.exporting {
                let (d, t) = self.export_progress;
                ui.horizontal(|ui| {
                    let cancelling = jobs::EXPORT_CANCEL.load(std::sync::atomic::Ordering::SeqCst);
                    let bar = (ui.available_width() - 130.0).max(60.0);
                    ui.allocate_ui(egui::vec2(bar, STATUS_H), |ui| crate::dialog::progress(ui, (t > 0).then(|| d as f32 / t as f32), &self.export_status));
                    if ui.add_enabled(!cancelling, egui::Button::new(if cancelling { crate::tr("Wird angehalten …") } else { crate::tr("Export anhalten") })).clicked() {
                        jobs::EXPORT_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                });
                return;
            }
            let (preview, mut notes, mut warn) = self.export_preview(ui.ctx(), &scope.items);
            if scope.unreachable > 0 {
                notes.insert(0, crate::trf!("{a} übersprungen: Original nicht erreichbar", a = scope.unreachable));
                warn = true;
            }
            ui.add(egui::Label::new(egui::RichText::new(preview).color(lr::TEXT())).truncate());
            let message = &self.export_names_ui.message;
            let (line, color) = if !notes.is_empty() {
                // yellow only where files are replaced or left out; "get a number" is plain information
                (notes.join(" · "), if warn { egui::Color32::from_rgb(0xe0, 0xb0, 0x40) } else { lr::TEXT_DIM() })
            } else if !message.is_empty() {
                (message.clone(), egui::Color32::from_rgb(0xe0, 0xb0, 0x40))
            } else {
                (crate::menu::i18n::tr_msg(&self.export_status).to_string(), lr::TEXT_DIM())
            };
            ui.add(egui::Label::new(egui::RichText::new(if line.is_empty() { " ".to_string() } else { line }).small().color(color)).truncate());
        });
    }
}

/// Quality slider 60–100 for JPEG and JPEG XL (lossy).
fn quality_slider(ui: &mut egui::Ui, v: &mut u8) {
    ui.spacing_mut().slider_width = (ui.available_width() - 60.0).clamp(80.0, 220.0);
    let r = crate::lr::slider_add(ui, egui::Slider::new(v, 60..=100).clamping(egui::SliderClamping::Edits));
    lr::hover_tip(ui, &r, crate::tr("Verlustbehaftet; 92 ist kaum vom Original zu unterscheiden"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Context, Pos2};

    /// Two columns and one column offer the same photos; switching the format changes no height (no jumping), and
    /// "Advanced" adds its rows only when opened.
    #[test]
    fn format_switch_keeps_the_layout_and_advanced_folds() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-export-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            image::RgbImage::from_pixel(64, 48, image::Rgb([90, 110, 120])).save(dir.join(format!("photo-{i}.png"))).unwrap();
        }
        let ctx = Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(dir.clone());
        assert_eq!(a.entries.len(), 3);
        a.export_scope = 2;
        let screen = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 900.0));
        let measure = |a: &mut App, two: bool| {
            let mut h = 0.0;
            let mut items = Vec::new();
            for _ in 0..2 {
                let _ = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                    let top = ui.cursor().top();
                    items = a.export_body(ui, two).items;
                    h = ui.min_rect().bottom() - top;
                });
            }
            (items, h)
        };
        a.export_names_ui.advanced = Some(false);
        let mut heights = Vec::new();
        for (format, original) in [(Format::Jpeg { quality: 92 }, false), (Format::Png16, false), (Format::Tiff16, false), (Format::JXL_DEFAULT, false), (Format::Jpeg { quality: 92 }, true)] {
            a.export_format = format;
            a.export_original = original;
            let (items, h) = measure(&mut a, true);
            assert_eq!(items, vec![0, 1, 2]);
            heights.push(h);
        }
        // format-dependent rows (e.g. HDR only for JPEG) are shown only where they apply, so heights may differ
        a.export_original = false;
        let (items, one) = measure(&mut a, false);
        assert_eq!(items, vec![0, 1, 2]);
        assert!(one > heights[0], "one column {one} vs two {}", heights[0]);
        a.export_names_ui.advanced = Some(true);
        let (_, open) = measure(&mut a, true);
        assert!(open > heights[0] + 3.0 * ROW_H, "{open} vs {}", heights[0]);
        drop(a);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
