//! "Ordner ansehen" (⌘O): the band that says so in every view. While a folder outside the library is only being viewed
//! (`Source::Browse`), a fixed band below the navigation names the folder, says that it is not in the library, offers
//! the way in ("Zur Bibliothek hinzufügen …", the gold main button of the window then) and out ("Schließen"), holds the
//! switch for ratings as sidecars, "Aktualisieren" and a "?" card that explains what is saved where. The band has the
//! same height and place in the library grid, Develop and the darkroom, so switching views moves nothing. The folder
//! tree is the section "ANGESEHEN" of the left panel (view_folder_tree.rs); the band keeps a short sentence and, when
//! the listing hit its limit, says so.
//!
//! Only presentation: what is stored (develop edits always as a sidecar next to the photo, ratings only with the
//! switch) is decided elsewhere (`raw_app::save_settings`, `LibraryView::rate_images`).

use std::path::Path;

use eframe::egui::{self, Color32};

use super::{view_folder_sidecars, LibraryView, Source, VIEW_SIDECARS_KEY};
use crate::lr;

/// Height of the band (and of the matching line in the left panel).
pub const BAND_H: f32 = 28.0;
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
    crate::trf!("„{a}“ · nur angesehen, nicht in der Bibliothek", a = folder_name(dir))
}

/// Limit note of the band (amber).
const LIMIT: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x60);

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
        let Some(root) = self.viewed_folder().map(Path::to_path_buf) else { return };
        if !band_shown(true, fullscreen, image_only) {
            return;
        }
        // the folder shown (a subfolder chosen in the tree) and whether its listing hit the limit
        let dir = self.view_tree.as_ref().map(|t| t.shown_dir()).unwrap_or_else(|| root.clone());
        let note = self.viewed_listing.as_ref().and_then(crate::view_folder::Listing::limit_note);
        let ctx = ui.ctx().clone();
        let (mut add, mut close, mut refresh) = (false, false, false);
        let frame = egui::Frame::new().fill(BAND).inner_margin(egui::Margin::symmetric(12, 0));
        egui::Panel::top("ansehen-band").show_separator_line(false).resizable(false).exact_size(BAND_H).frame(frame).show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            // right first (it takes precedence), from the edge inwards: close and refresh as small icons, the way in
            // as an outlined button, the ratings option; the sentence gets what is left
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(egui::Layout::right_to_left(egui::Align::Center)));
            {
                let ui = &mut right;
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = band_button(ui, crate::tr("Schließen"));
                lr::hover_tip(ui, &r, crate::tr("Ordner nicht mehr ansehen – zurück zu „Alle Fotos“"));
                close = r.clicked();
                let r = band_icon(ui, lr::Icon::RotCw);
                lr::tip_key(ui, &r, "Ordner neu einlesen", "refresh_folder");
                refresh = r.clicked();
                ui.add_space(10.0);
                let r = band_button(ui, crate::tr("Zur Bibliothek hinzufügen …"));
                lr::hover_tip(ui, &r, crate::tr("Öffnet den Import mit diesem Ordner"));
                add = r.clicked();
                ui.add_space(18.0);
                let mut on = view_folder_sidecars();
                let r = band_check(ui, &mut on, crate::tr("Bewertungen als XMP"));
                lr::hover_tip(ui, &r, crate::tr("Sterne, Markierungen und Etiketten als Begleitdatei (XMP) neben die Fotos schreiben. Aus: Bewerten ist hier gesperrt."));
                if r.clicked() {
                    library::settings::set(VIEW_SIDECARS_KEY, serde_json::json!(on));
                }
            }
            let used = right.min_rect();
            let left_rect = egui::Rect::from_min_max(full.min, egui::pos2((used.left() - 24.0).max(full.left()), full.bottom()));
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
                if let Some(note) = &note {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(egui::RichText::new(note).color(LIMIT)).truncate());
                }
            }
        });
        if add {
            self.import_folder(&ctx, root);
        } else if close {
            self.close_view_folder();
        } else if refresh {
            self.refresh_viewed(true);
        }
    }
}

/// Small square icon button of the band (no frame; a quiet fill on hover).
fn band_icon(ui: &mut egui::Ui, icon: lr::Icon) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 20.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 1.0, Color32::from_rgb(0x22, 0x31, 0x3d));
    }
    let color = if resp.hovered() { BAND_TEXT } else { BAND_DIM };
    lr::paint_icon(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(12.0, 12.0)), icon, color);
    resp
}

/// The way in: a small outlined button (the band's one call to action; gold stays for the real main buttons).
fn band_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, BAND_TEXT);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + 22.0, 20.0), egui::Sense::click());
    let hot = resp.hovered();
    if hot {
        ui.painter().rect_filled(rect, 1.0, Color32::from_rgb(0x22, 0x31, 0x3d));
    }
    ui.painter().rect_stroke(rect, 1.0, egui::Stroke::new(1.0, if hot { Color32::from_rgb(0x8f, 0xa4, 0xb4) } else { Color32::from_rgb(0x4c, 0x62, 0x73) }), egui::StrokeKind::Inside);
    ui.painter().galley(egui::pos2(rect.left() + 11.0, rect.center().y - galley.size().y / 2.0), galley, BAND_TEXT);
    resp
}

/// Option with a small box and a label (the same box as the options of the left panel).
fn band_check(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, BAND_DIM);
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(16.0 + galley.size().x, 20.0), egui::Sense::click());
    let hot = resp.hovered();
    let square = egui::Rect::from_center_size(egui::pos2(rect.left() + 5.0, rect.center().y), egui::vec2(10.0, 10.0));
    ui.painter().rect_stroke(square, 1.0, egui::Stroke::new(1.0, if hot { BAND_TEXT } else { BAND_DIM }), egui::StrokeKind::Inside);
    if *on {
        ui.painter().rect_filled(square.shrink(2.5), 0.0, BAND_TEXT);
    }
    let color = if hot { BAND_TEXT } else { BAND_DIM };
    ui.painter().galley(egui::pos2(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0), galley, color);
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
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
