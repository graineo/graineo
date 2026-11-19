//! Photos of a folder only being viewed ("Ordner ansehen", `Source::Browse`) have no library entry. Functions that
//! need one (collections, keywords, stacks, virtual copies, capture time, similar photos, rename and move with their
//! journal, "show in library folder", removing from the library) refuse them the same way: the entry is greyed with
//! the reason as a tooltip, a key or a drop puts the reason in the status line; nothing is written, there is no undo
//! step and no error dialog. Exporting, printing, developing (sidecar next to the photo), "Show in Finder" and moving
//! to the Trash keep working.
//!
//! New functions that act on library photos: check [`needs_library`] (or [`Menu::needs_library`] for a context menu
//! entry) before writing and show [`NEEDS_LIBRARY`].

use eframe::egui;

use super::{LibraryView, Menu, QUICK_REFUSED};

/// The reason, the same everywhere (tooltip, status line; the quick collection uses it too).
pub const NEEDS_LIBRARY: &str = QUICK_REFUSED;

/// Is this the ID of a photo of a viewed folder? Only `view_id` hands out negative IDs; library IDs are row IDs
/// (> 0).
pub fn viewed(image: i64) -> bool {
    image < 0
}

/// Does any of these photos lack the library entry the function needs?
pub fn needs_library(images: &[i64]) -> bool {
    images.iter().any(|&b| viewed(b))
}

/// The reason as a tooltip over a widget greyed because it needs the library (a disabled widget reports no hover
/// itself).
pub fn why_tip(ui: &egui::Ui, resp: &egui::Response) {
    let probe = ui.interact(resp.rect, resp.id.with("needs-library"), egui::Sense::hover());
    crate::lr::hover_tip(ui, &probe, crate::tr(NEEDS_LIBRARY));
}

impl Menu {
    /// Does this context menu command need a library entry for its photos?
    pub fn needs_library(self) -> bool {
        matches!(
            self,
            Menu::Quick | Menu::VirtualCopy | Menu::Similar | Menu::Time | Menu::Rename | Menu::Move | Menu::ShowFolder | Menu::RemoveFromLibrary | Menu::RemoveFromCollection
        )
    }
}

impl LibraryView {
    /// The reason if a command needing the library is refused for these photos (`None`: it may go ahead).
    pub(super) fn refuse_viewed(&self, images: &[i64]) -> Option<String> {
        needs_library(images).then(|| crate::tr(NEEDS_LIBRARY).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Action, Dialog, Source};
    use super::*;
    use std::path::Path;

    fn png(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(p).unwrap();
    }

    #[test]
    fn which_commands_need_the_library() {
        let need = [Menu::Quick, Menu::VirtualCopy, Menu::Similar, Menu::Time, Menu::Rename, Menu::Move, Menu::ShowFolder, Menu::RemoveFromLibrary, Menu::RemoveFromCollection];
        let free = [Menu::Export, Menu::Print, Menu::Finder, Menu::Delete, Menu::CopySettings, Menu::PasteSettings];
        assert!(need.iter().all(|m| m.needs_library()));
        assert!(free.iter().all(|m| !m.needs_library()));
        assert!(needs_library(&[3, -2]) && !needs_library(&[3, 4]) && !needs_library(&[]));
    }

    /// A viewed folder: every library-only command refuses with the reason in the status line; no dialog, no
    /// question, no background job, no undo step, the source stays the viewed folder. Before: rename closed
    /// silently, "show in library folder" did nothing, capture time and "remove from library" ran against the
    /// database without a matching entry, keywords and collections failed with a database error.
    #[test]
    fn library_commands_refused_while_viewing() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let root = std::env::temp_dir().join(format!("graineo-view-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        png(&root.join("a.png"));
        png(&root.join("b.png"));
        let ctx = egui::Context::default();
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&root, None);
        let ids: Vec<i64> = b.rows.iter().map(|z| z.image).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|&i| viewed(i)), "viewed photos carry negative IDs: {ids:?}");
        assert!(b.needs_library(&ids));
        b.active = ids.first().copied();
        let undo_before = b.manage.undo.len();
        let ops_before = b.ops;
        let reason = crate::tr(NEEDS_LIBRARY).to_string();
        for m in [Menu::Quick, Menu::VirtualCopy, Menu::Similar, Menu::Time, Menu::Rename, Menu::Move, Menu::ShowFolder, Menu::RemoveFromLibrary] {
            let mut actions = Vec::new();
            b.menu(m, &ctx, &mut actions);
            assert!(matches!(actions.as_slice(), [Action::Status(t)] if *t == reason), "{m:?}: {:?}", actions.len());
            assert!(b.dialog.is_none() && b.manage.ask.is_none() && b.similar_state.is_none(), "{m:?} opened something");
            assert!(matches!(b.source, Source::Browse(_)), "{m:?} left the viewed folder");
        }
        // drop onto a collection, a new collection with the selection, bundling: refused before any job
        b.drop_on_collection(1, ids.clone());
        assert_eq!(b.manage_message.take().as_deref(), Some(reason.as_str()));
        b.selection = ids.iter().copied().collect();
        b.nothing_selected = false;
        b.ask_new_collection(library::manage::COLLECTION, None);
        assert!(matches!(&b.manage.ask, Some(super::super::library_manage::Ask::NewCollection { with_selection: false, selected, .. }) if selected.is_empty()), "a new collection never takes viewed photos");
        b.manage.ask = None;
        let mut actions = Vec::new();
        b.series_command(super::super::library_series::StackCmd::Group, ids[0], &mut actions);
        assert!(matches!(actions.as_slice(), [Action::Status(t)] if *t == reason));
        assert_eq!((b.manage.undo.len(), b.ops), (undo_before, ops_before), "no undo step, no operation");
        assert!(!b.db_writer.busy(), "no background job started");
        assert!(!matches!(b.dialog, Some(Dialog::Error(_))));
        let _ = std::fs::remove_dir_all(&root);
    }
}
