//! Tool rail at the right edge: a narrow icon bar selects what the right panel shows.
//!
//! Groups (from the top): Edit, Film & Look · Crop, Masks, Retouch · Presets; Info at the top, Sensor at the bottom.
//! Tools that can be opened and left again (crop) open their panel on the right and the
//! mode in the image; a click on another tool applies and leaves them.

use eframe::egui;
use egui::Color32;

use crate::workspace::ToolTab;
use crate::{lr, App, Tab};

/// Width of the rail.
pub const WIDTH: f32 = 42.0;

impl App {
    /// Active tool: the open crop mode and the sensor view take precedence.
    pub(crate) fn tool_rail_active(&self) -> ToolTab {
        if self.crop_mode.is_some() {
            ToolTab::Crop
        } else if self.tab == Tab::Sensor && self.tool_tab != ToolTab::Info {
            ToolTab::Sensor
        } else if self.in_people() {
            // people view: Info (the editing tools open the chosen face's photo in Develop)
            ToolTab::Info
        } else if self.tool_tab == ToolTab::Merge && self.merge_info().is_none() {
            // the photo is not a merged one (any more): the tool has nothing to show
            ToolTab::Edit
        } else {
            self.tool_tab
        }
    }

    /// Select a tool (click in the rail).
    pub(crate) fn choose_tool(&mut self, w: ToolTab) {
        self.show_right = true;
        match w {
            ToolTab::Crop => {
                if self.in_grid() {
                    self.to_single_image();
                    self.tool_open = Some(ToolTab::Crop);
                } else {
                    self.open_geo_tool(crate::align::GeoTool::Crop);
                }
            }
            // Masks and retouching are drawn into the image: in the grid the active photo opens in single image view
            ToolTab::Masks | ToolTab::Retouch if self.in_grid() => {
                self.to_single_image();
                self.tool_tab = w;
            }
            ToolTab::Sensor => {
                if self.crop_mode.is_some() {
                    self.toggle_crop(true);
                }
                if self.in_grid() {
                    self.to_single_image();
                }
                if self.tab == Tab::Sensor && self.tool_tab == ToolTab::Info {
                    // from Info back to the sensor view (do not switch it off)
                } else {
                    self.tab = if self.tab == Tab::Sensor { Tab::Develop } else { Tab::Sensor };
                }
                self.tool_tab = ToolTab::Edit;
            }
            _ => {
                if self.crop_mode.is_some() {
                    self.toggle_crop(true);
                }
                if w == ToolTab::Info {
                    self.navigation.finish();
                } else {
                    self.wb_picker = false;
                    if self.tab == Tab::Sensor {
                        self.tab = Tab::Develop;
                    }
                    if w != ToolTab::FilmLook {
                        self.profile_browser = false;
                    }
                }
                self.tool_tab = w;
                // people view: an editing tool works on the chosen face's photo, in Develop
                if w != ToolTab::Info && self.in_people() {
                    self.to_single_image();
                    self.tool_tab = w;
                }
                if matches!(w, ToolTab::Masks | ToolTab::Retouch) && self.in_grid() {
                    self.to_single_image();
                    self.tool_tab = w;
                }
            }
        }
    }

    /// Open a pending tool as soon as the image is loaded (click from the grid).
    pub(crate) fn tool_rail_follow(&mut self) {
        if let Some(w) = self.tool_open {
            if self.current.is_some() && self.loading.is_none() {
                self.tool_open = None;
                self.choose_tool(w);
            }
        }
    }

    /// The icon bar itself.
    pub(crate) fn tool_rail(&mut self, ui: &mut egui::Ui) {
        let active = self.tool_rail_active();
        let has_image = if self.in_people() { self.has_photo() } else { self.current.is_some() || self.active_entry().is_some() };
        let mut chosen: Option<ToolTab> = None;
        let compare_blocks = self.compare_in_develop();
        let blocked_tip = lr::with_shortcut("Erst den Vergleich beenden", "compare_open");
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let (t_info, t_edit, t_crop) = (lr::with_shortcut("Info: Metadaten, Stichwörter, Bewertung", "tab_info"), lr::with_shortcut("Bearbeiten", "tab_edit"), lr::with_shortcut("Freistellen & Ausrichten", "crop"));
            let t_retouch = lr::with_shortcut("Retusche: Heilen, Klonen, rote Augen, Objekte entfernen", crate::shortcuts::RETOUCH);
            // the merge tool exists for merged photos only
            let mut tools: Vec<(ToolTab, lr::Icon, &str)> = vec![(ToolTab::Crop, lr::Icon::Crop, t_crop.as_str()), (ToolTab::Masks, lr::Icon::Mask, crate::tr("Masken: lokale Korrekturen")), (ToolTab::Retouch, lr::Icon::Bandaids, t_retouch.as_str())];
            if self.merge_info().is_some() {
                tools.push((ToolTab::Merge, lr::Icon::Images, crate::tr("Zusammenfügen: Quelle malen, Bewegung")));
            }
            let groups: [&[(ToolTab, lr::Icon, &str)]; 4] = [
                &[(ToolTab::Info, lr::Icon::Info, t_info.as_str())],
                &[(ToolTab::Edit, lr::Icon::Sliders, t_edit.as_str()), (ToolTab::FilmLook, lr::Icon::Film, crate::tr("Film & Look"))],
                tools.as_slice(),
                &[(ToolTab::Presets, lr::Icon::Sparkle, crate::tr("Vorgaben & Verlauf")), (ToolTab::Versions, lr::Icon::Stack, crate::tr("Versionen: virtuelle Kopien"))],
            ];
            for (n, g) in groups.iter().enumerate() {
                if n > 0 {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(24.0, 1.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 0.0, crate::lr::gray(0x2c));
                    ui.add_space(4.0);
                }
                for (w, icon, tip) in g.iter() {
                    // the comparison draws in the centre: tools that draw into the image wait until it ends
                    let blocked = compare_blocks && matches!(w, ToolTab::Crop | ToolTab::Masks | ToolTab::Retouch | ToolTab::Merge);
                    let tip = if blocked { blocked_tip.as_str() } else { *tip };
                    if lr::tool_rail_button(ui, *icon, active == *w, has_image && !blocked, tip) && !blocked {
                        chosen = Some(*w);
                    }
                }
            }
        });
        // bottom: Sensor
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add_space(6.0);
            let t_sensor = lr::with_shortcut("Sensor-Ansicht", "sensor_field");
            let tip = if compare_blocks { blocked_tip.as_str() } else { t_sensor.as_str() };
            for (w, icon, tip, enabled) in [(ToolTab::Sensor, lr::Icon::Aperture, tip, has_image && !compare_blocks)] {
                if lr::tool_rail_button(ui, icon, active == w, enabled, tip) && enabled {
                    chosen = Some(w);
                }
            }
        });
        if let Some(w) = chosen {
            self.choose_tool(w);
        }
    }

    /// Header of the right panel: tool name.
    pub(crate) fn tool_header(&mut self, ui: &mut egui::Ui, w: ToolTab) {
        let title = match w {
            ToolTab::Info => crate::tr("INFO"),
            ToolTab::Edit => crate::tr("BEARBEITEN"),
            ToolTab::FilmLook => crate::tr("FILM & LOOK"),
            ToolTab::Crop => crate::tr("FREISTELLEN & AUSRICHTEN"),
            ToolTab::Masks => crate::tr("MASKEN"),
            ToolTab::Retouch => crate::tr("RETUSCHE"),
            ToolTab::Presets => crate::tr("VORGABEN & VERLAUF"),
            ToolTab::Sensor => crate::tr("SENSOR"),
            ToolTab::Versions => crate::tr("VERSIONEN"),
            ToolTab::Merge => crate::tr("ZUSAMMENFÜGEN"),
        };
        // The former row "Auto · B&W · Reset" was removed: everything appeared twice –
        // "Auto" under Tone, color/black & white under Treatment, Reset at the bottom (⌥⇧⌘R).
        ui.horizontal(|ui| {
            ui.set_height(24.0);
            let g = lr::caps(ui.painter(), title, lr::semibold(11.5), crate::lr::gray(0xd8));
            let (r, _) = ui.allocate_exact_size(egui::vec2(g.size().x, 24.0), egui::Sense::hover());
            ui.painter().galley(egui::pos2(r.left(), r.center().y - g.size().y / 2.0), g, crate::lr::TEXT_STRONG());
            if w == ToolTab::Crop {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| crate::help::info(ui, "freistellen-hilfe", "Freistellen & Ausrichten", &crate::help::CROP_ALIGN));
            }
            // comparing in Develop: the panel edits the focused field – its file and place, so no other photo is changed
            if w != ToolTab::Info {
                if let Some(t) = self.compare_panel_head() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(t).small().color(lr::TEXT_DIM())).truncate());
                    });
                }
            }
            // Bayer/X-Trans: one card for the whole tray (the X3F tray has its "?" in "View")
            if w == ToolTab::Sensor && self.sensor_raw_active() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| crate::help::info(ui, "sensor-ansicht", "Sensor-Ansicht", &crate::help::SENSOR_BAR));
            }
        });
    }

    /// "Versions" tool: the original and its virtual copies (own settings, same file).
    pub(crate) fn versions_panel(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &self.current else { return };
        let (file, active) = (self.entries[c.index].file, c.index);
        let versions: Vec<(usize, String, bool)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.file == file)
            .map(|(i, e)| (i, e.copy.clone().unwrap_or_else(|| "Original".to_string()), e.loaded && e.settings != develop::Settings::default()))
            .collect();
        let mut pick: Option<usize> = None;
        let mut new_copy = false;
        lr::panel(ui, |ui| {
            ui.add_space(4.0);
            for (i, name, edited) in &versions {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
                let p = ui.painter();
                if *i == active {
                    p.rect_filled(rect, 0.0, crate::lr::gray(0x3a));
                } else if resp.hovered() {
                    p.rect_filled(rect, 0.0, crate::lr::gray(0x26));
                }
                let color = if *i == active { crate::lr::TEXT_STRONG() } else { lr::TEXT() };
                p.text(rect.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, crate::tr(name), egui::TextStyle::Body.resolve(ui.style()), color);
                if *edited {
                    lr::paint_icon(p, egui::Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), egui::vec2(13.0, 13.0)), lr::Icon::Pencil, lr::TEXT_DIM());
                }
                if resp.clicked() {
                    pick = Some(*i);
                }
            }
            ui.add_space(8.0);
            // not gold: the one main button of the window is "Export"
            ui.horizontal(|ui| {
                let r = ui.button(crate::tr("Neue virtuelle Kopie"));
                crate::lr::tip_key(ui, &r, "Eigene Einstellungen, dieselbe Datei", "virtual_copy");
                if r.clicked() {
                    new_copy = true;
                }
            });
        });
        if let Some(i) = pick {
            self.select(i, false);
        }
        if new_copy {
            self.create_virtual_copy(None);
        }
    }
}
