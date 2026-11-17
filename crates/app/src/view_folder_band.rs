//! "Ordner ansehen" (⌘O): the band that says so in every view. While a folder outside the library is only being viewed
//! (`Source::Browse`), a fixed band below the navigation names the folder, says that it is not in the library, offers
//! the way in ("Zur Bibliothek hinzufügen …", the gold main button of the window then) and out ("Schließen"), holds the
//! switch for ratings as sidecars and a "?" card that explains what is saved where. The band has the same height and
//! place in the library grid, Develop and the darkroom, so switching views moves nothing. The left panel shows a quiet
//! line of the same height above the library sources, so they no longer look like part of the viewed folder.
//!
//! Only presentation: what is stored (develop edits always as a sidecar next to the photo, ratings only with the
//! switch) is decided elsewhere (`raw_app::save_settings`, `LibraryView::rate_images`).

use std::path::Path;

use eframe::egui::{self, Color32};

use super::{view_folder_sidecars, LibraryView, Source, VIEW_SIDECARS_KEY};
use crate::lr;

/// Height of the band (and of the matching line in the left panel).
pub const BAND_H: f32 = 32.0;
/// Background: a cool dark blue-grey, clearly not the library (gold stays for the main button, amber for backups).
const BAND: Color32 = Color32::from_rgb(0x17, 0x22, 0x2c);
const BAND_TEXT: Color32 = Color32::from_rgb(0xd6, 0xe2, 0xec);
const BAND_DIM: Color32 = Color32::from_rgb(0x9a, 0xab, 0xba);

/// Name of the viewed folder as the band shows it (last path component; the whole path for a root like "/").
pub fn folder_name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| dir.display().to_string())
}

/// Main sentence of the band (German key, translated).
pub fn band_text(dir: &Path) -> String {
    crate::trf!("Du siehst den Ordner „{a}“ an – er ist nicht in der Bibliothek", a = folder_name(dir))
}

/// Is the band shown? Only while a folder is viewed, and not in the fullscreen preview (F) or the darkroom's
/// "image only" mode (nothing but the photo there).
pub fn band_shown(browsing: bool, fullscreen: bool, image_only: bool) -> bool {
    browsing && !fullscreen && !image_only
}

impl LibraryView {
    /// The folder only being viewed (⌘O), if any.
    pub fn viewed_folder(&self) -> Option<&Path> {
        match &self.source {
            Source::Browse(p) => Some(p.as_path()),
            _ => None,
        }
    }

    /// The band below the navigation while a folder is viewed (nothing otherwise). `ui` is the window's root: the band
    /// is a top panel over the whole width, drawn right after the navigation.
    pub fn view_folder_band(&mut self, ui: &mut egui::Ui, fullscreen: bool, image_only: bool) {
        let Some(dir) = self.viewed_folder().map(Path::to_path_buf) else { return };
        if !band_shown(true, fullscreen, image_only) {
            return;
        }
        let ctx = ui.ctx().clone();
        let (mut add, mut close) = (false, false);
        let frame = egui::Frame::new().fill(BAND).inner_margin(egui::Margin::symmetric(12, 0));
        egui::Panel::top("ansehen-band").show_separator_line(false).resizable(false).exact_size(BAND_H).frame(frame).show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            // right first (it takes precedence): close, add (gold), the ratings switch; the sentence gets what is left
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(egui::Layout::right_to_left(egui::Align::Center)));
            {
                let ui = &mut right;
                ui.spacing_mut().item_spacing.x = 8.0;
                let r = ui.add(egui::Button::new(egui::RichText::new(crate::tr("Schließen")).color(BAND_TEXT)).frame(false));
                lr::hover_tip(ui, &r, crate::tr("Ordner nicht mehr ansehen – zurück zu „Alle Fotos“"));
                close = r.clicked();
                let r = lr::main_button(ui, crate::tr("Zur Bibliothek hinzufügen …"), true);
                lr::hover_tip(ui, &r, crate::tr("Öffnet den Import mit diesem Ordner"));
                add = r.clicked();
                ui.add_space(14.0);
                let sw = ui.label(egui::RichText::new(crate::tr("Bewertungen als XMP")).color(BAND_DIM));
                lr::hover_tip(ui, &sw, crate::tr("Sterne, Markierungen und Etiketten als Begleitdatei (XMP) neben die Fotos schreiben. Aus: Bewerten ist hier gesperrt."));
                let mut on = view_folder_sidecars();
                if crate::dialog::toggle(ui, &mut on) {
                    library::settings::set(VIEW_SIDECARS_KEY, serde_json::json!(on));
                }
            }
            let used = right.min_rect();
            let left_rect = egui::Rect::from_min_max(full.min, egui::pos2((used.left() - 16.0).max(full.left()), full.bottom()));
            let mut left = ui.new_child(egui::UiBuilder::new().max_rect(left_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
            {
                let ui = &mut left;
                ui.spacing_mut().item_spacing.x = 6.0;
                let icon = egui::Rect::from_center_size(egui::pos2(ui.cursor().min.x + 7.0, left_rect.center().y), egui::vec2(13.0, 13.0));
                lr::paint_icon(ui.painter(), icon, lr::Icon::Info, BAND_TEXT);
                ui.add_space(20.0);
                let text = band_text(&dir);
                let r = ui.add(egui::Label::new(egui::RichText::new(&text).color(BAND_TEXT)).truncate());
                // shortened in a narrow window: the whole sentence and the path on hover
                lr::hover_tip(ui, &r, &format!("{text}\n{}", dir.display()));
                crate::help::info(ui, "ordner-ansehen", "Ordner ansehen", &crate::help::VIEW_FOLDER);
            }
        });
        if add {
            self.import_folder(&ctx, dir);
        } else if close {
            self.close_view_folder();
        }
    }

    /// Top of the left panel while a folder is viewed: a quiet line of the band's height saying that the sources
    /// below are the library, without the viewed folder. Nothing clickable.
    pub(super) fn view_folder_sources_note(&self, ui: &mut egui::Ui) {
        if self.viewed_folder().is_none() {
            return;
        }
        egui::Panel::top("bib-links-ansehen").show_separator_line(true).resizable(false).exact_size(BAND_H).frame(egui::Frame::new()).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(crate::tr("Deine Bibliothek – der angesehene Ordner gehört nicht dazu")).small().color(lr::TEXT_DIM)).wrap());
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn band_names_the_folder() {
        assert_eq!(folder_name(&PathBuf::from("/Volumes/Karte/DCIM/100CANON")), "100CANON");
        assert_eq!(folder_name(&PathBuf::from("/Users/x/Bilder/")), "Bilder");
        assert_eq!(folder_name(&PathBuf::from("/")), "/");
        assert!(band_text(&PathBuf::from("/a/Urlaub 2024")).contains("Urlaub 2024"));
    }

    #[test]
    fn band_only_while_viewing_and_never_over_the_bare_photo() {
        assert!(band_shown(true, false, false));
        assert!(!band_shown(false, false, false), "library: no band");
        assert!(!band_shown(true, true, false), "fullscreen preview (F): only the photo");
        assert!(!band_shown(true, false, true), "darkroom, image only: only the photo");
    }

    /// The band draws the sentence, both buttons and the "?" in a frame; "Schließen" leaves the folder; nothing
    /// is drawn while the library is shown.
    #[test]
    fn band_in_a_frame_and_close_leaves_the_folder() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let dir = std::env::temp_dir().join(format!("graineo-band-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..2 {
            image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(dir.join(format!("image-{i}.png"))).unwrap();
        }
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut b = LibraryView::new(&ctx);
        let library_before = b.library_photo_count();
        b.browse_folder(&dir, None);
        assert_eq!(b.viewed_folder(), Some(dir.as_path()));
        // review 5.3: the viewed photos never count as library photos ("Alle Fotos"); other tests may share the data
        // folder's library, so compare with the count before viewing
        assert_eq!(b.visible.len(), 2);
        assert_eq!(b.library_photo_count(), library_before);
        assert_eq!(b.library_photo_count() + 2, b.rows.len());
        assert_eq!(b.library_is_empty(), library_before == 0);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 600.0));
        let texts = |out: &egui::FullOutput| -> Vec<String> {
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect()
        };
        let mut frame = |b: &mut LibraryView, t: f64, events: Vec<egui::Event>| {
            crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() }, |root| {
                b.view_folder_band(root, false, false);
                egui::CentralPanel::default().show(root, |_| {});
            })
        };
        let out = frame(&mut b, 0.0, vec![]);
        let all = texts(&out).join(" | ");
        let name = folder_name(&dir);
        assert!(all.contains(&name), "{all}");
        assert!(all.contains(crate::tr("Zur Bibliothek hinzufügen …")), "{all}");
        assert!(all.contains(crate::tr("Schließen")), "{all}");
        assert!(all.contains("?"), "{all}");
        // the band is exactly BAND_H high and at the top: the central panel starts below it
        let close_pos = out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == crate::tr("Schließen") => Some(t.pos + t.galley.rect.center().to_vec2()),
            _ => None,
        }).expect("close button");
        assert!(close_pos.y < BAND_H, "{close_pos:?}");
        let press = |pressed| egui::Event::PointerButton { pos: close_pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&mut b, 1.0, vec![egui::Event::PointerMoved(close_pos), press(true)]);
        frame(&mut b, 1.05, vec![press(false)]);
        assert_eq!(b.viewed_folder(), None, "Schließen leaves the viewed folder");
        assert!(b.source == Source::All);
        let out = frame(&mut b, 2.0, vec![]);
        assert!(!texts(&out).iter().any(|t| t.contains(&name)), "no band in the library");
    }
}
