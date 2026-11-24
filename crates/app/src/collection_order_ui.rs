//! Own order of the collections in the left panel, without the mouse: the submenu "Ordnen" (up, down, to the start, to
//! the end), "Nach Namen ordnen" per level (context menu of the header SAMMLUNGEN and of a set), the shortcuts ⌥⌘↑/⌥⌘↓
//! for the collection the grid shows, the remembered open sets and the set choice of the dialogs.
//!
//! The panel shows a new order at once (`library::collection_order::place_in`); the library writes it in the background
//! (`library::collection_order::place`) and the list is loaded again afterwards; a failed write puts the old order back
//! and says why. Every step is one ⌘Z step of the management stack (`Undo::Order`). Ordering never changes which photos
//! a collection holds. Dragging is in `collection_drag.rs`.

use super::*;
use library::collection_order as order;
use library_manage::Undo;

/// Settings key of the sets open in the panel (ids), kept over a restart.
const OPEN_KEY: &str = "libraryOpenCollections";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Up,
    Down,
    Start,
    End,
}

impl LibraryView {
    /// New list of collections (loaded or changed): the panel's order per level is built once here, not while drawing.
    pub(super) fn set_collections(&mut self, list: Vec<Collection>) {
        self.collections = list;
        self.collection_tree = order::Tree::build(&self.collections);
    }

    /// Members of a level as the panel shows them (ids; the quick collection taken over from Lightroom is not listed).
    pub(super) fn level_ids(&self, parent: Option<i64>) -> Vec<i64> {
        self.collection_tree.children(parent).iter().filter_map(|&i| self.collections.get(i)).filter(|c| Some(c.id) != self.quick_id).map(|c| c.id).collect()
    }

    /// Ordering is refused while a backup is viewed (it runs on a copy that takes no changes).
    /// Why ordering and moving are locked (the tooltip of the disabled entries).
    pub(super) fn locked_why() -> &'static str {
        crate::tr("Gesperrt, solange du eine Sicherung ansiehst – erst zurück zur Bibliothek")
    }

    pub(super) fn ordering_locked() -> bool {
        library::backup_view::active().is_some()
    }

    /// Put collection `id` into level `parent` before `before` (`None`: at the end): shown at once, written in the
    /// background, one ⌘Z step. Blocked targets only say why.
    pub(super) fn place_collection(&mut self, id: i64, parent: Option<i64>, before: Option<i64>) {
        if Self::ordering_locked() {
            return;
        }
        if let Err(b) = order::check(&self.collections, id, parent) {
            self.manage_message = Some(crate::tr(b.message()).to_string());
            return;
        }
        let Some(c) = self.collections.iter().find(|c| c.id == id).cloned() else { return };
        let moved = c.parent != parent;
        // the same name of the same kind in the target set is allowed (as in different sets today), with a hint
        let same_name = moved && self.collections.iter().any(|o| o.id != id && o.parent == parent && o.kind == c.kind && o.name == c.name);
        let mut list = std::mem::take(&mut self.collections);
        let changed = order::place_in(&mut list, id, parent, before);
        self.set_collections(list);
        if !changed {
            return;
        }
        if let Some(p) = parent {
            self.open_collections.insert(p);
        }
        let label = match parent {
            _ if !moved => crate::trf!("„{a}“ verschoben", a = c.name),
            Some(p) => crate::trf!("„{a}“ in „{b}“ gelegt", a = c.name, b = self.collections.iter().find(|s| s.id == p).map(|s| s.name.clone()).unwrap_or_default()),
            None => crate::trf!("„{a}“ auf die oberste Ebene gelegt", a = c.name),
        };
        let status = if same_name { crate::trf!("{a} – dort gibt es schon eine mit diesem Namen", a = label) } else { label.clone() };
        self.collection_drag.flash(id);
        self.order_job(label, status, move |db| order::place(db, id, parent, before));
    }

    /// Write an order change in the background; afterwards the list is loaded again (also after an error: the old
    /// order comes back, the dialog says why).
    fn order_job(&mut self, label: String, status: String, work: impl FnOnce(&Db) -> Result<library::manage::Snapshot, String> + Send + 'static) {
        self.in_background(work, move |b, actions, r| {
            b.after_collections_changed();
            match r {
                Ok(snap) if snap.is_empty() => {}
                Ok(snap) => {
                    actions.push(Action::Status(status));
                    b.manage.push(Undo::Order { label, snap });
                    b.ops += 1;
                }
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            }
        });
    }

    /// Where a step puts the collection: `Some(before)` (`before` `None` = at the end), `None` if it is already there.
    pub(super) fn step_target(&self, id: i64, step: Step) -> Option<Option<i64>> {
        let c = self.collections.iter().find(|c| c.id == id)?;
        let ids = self.level_ids(c.parent);
        let i = ids.iter().position(|&x| x == id)?;
        let last = i + 1 == ids.len();
        match step {
            Step::Up if i > 0 => Some(Some(ids[i - 1])),
            Step::Start if i > 0 => Some(Some(ids[0])),
            Step::Down if !last => Some(ids.get(i + 2).copied()),
            Step::End if !last => Some(None),
            _ => None,
        }
    }

    pub(super) fn step_collection(&mut self, id: i64, step: Step) {
        let Some(before) = self.step_target(id, step) else { return };
        let parent = self.collections.iter().find(|c| c.id == id).and_then(|c| c.parent);
        self.place_collection(id, parent, before);
    }

    /// "Nach Namen ordnen" for a level (`None`: top level): back to sets first, then by name; ⌘Z brings the own
    /// order back.
    pub(super) fn sort_level_by_name(&mut self, parent: Option<i64>) {
        if Self::ordering_locked() {
            return;
        }
        let mut list = std::mem::take(&mut self.collections);
        let changed = order::sort_by_name_in(&mut list, parent);
        self.set_collections(list);
        if !changed {
            return;
        }
        let label = match parent.and_then(|p| self.collections.iter().find(|c| c.id == p)) {
            Some(s) => crate::trf!("„{a}“ nach Namen geordnet", a = s.name),
            None => crate::tr("Sammlungen nach Namen geordnet").to_string(),
        };
        self.order_job(label.clone(), label, move |db| order::sort_by_name(db, parent));
    }

    /// The submenu "Ordnen" of a collection's context menu (entries disabled where the collection already is).
    pub(super) fn order_menu(&mut self, ui: &mut egui::Ui, c: &Collection) {
        let free = !Self::ordering_locked();
        let locked_why = Self::locked_why();
        ui.menu_button(crate::tr("Ordnen"), |ui| {
            for (step, text, key) in [
                (Step::Up, crate::tr("Nach oben"), "move_collection_up"),
                (Step::Down, crate::tr("Nach unten"), "move_collection_down"),
                (Step::Start, crate::tr("An den Anfang"), ""),
                (Step::End, crate::tr("Ans Ende"), ""),
            ] {
                let shortcut = if key.is_empty() { String::new() } else { crate::shortcuts::display(key) };
                let why = if !free {
                    locked_why
                } else {
                    match step {
                        Step::Up | Step::Start => crate::tr("Steht schon ganz oben"),
                        Step::Down | Step::End => crate::tr("Steht schon ganz unten"),
                    }
                };
                if lr::menu_item_why(ui, text, &shortcut, free && self.step_target(c.id, step).is_some(), why) {
                    self.step_collection(c.id, step);
                }
            }
        });
    }

    /// "Nach Namen ordnen" as a menu entry for a level (only active when the level has an own order).
    pub(super) fn sort_by_name_item(&mut self, ui: &mut egui::Ui, parent: Option<i64>) {
        let can = !Self::ordering_locked() && order::is_ordered(&self.collections, parent);
        let why = if Self::ordering_locked() { Self::locked_why() } else { crate::tr("Schon nach Namen geordnet") };
        if lr::menu_item_why(ui, crate::tr("Nach Namen ordnen"), "", can, why) {
            self.sort_level_by_name(parent);
        }
    }

    /// Context menu of the header SAMMLUNGEN (right click anywhere on it): order of the top level. The header itself
    /// keeps its click (open/close); this only listens for the right click.
    pub(super) fn collections_header_menu(&mut self, ui: &mut egui::Ui, header: egui::Rect) {
        let resp = ui.interact(header, egui::Id::unique("library-collections-header-menu"), egui::Sense::hover());
        let right = ui.input(|i| i.pointer.secondary_clicked()) && resp.contains_pointer();
        egui::Popup::menu(&resp).open_memory(right.then_some(egui::SetOpenCommand::Bool(true))).at_pointer_fixed().show(|ui| {
            self.sort_by_name_item(ui, None);
        });
    }

    /// ⌥⌘↑ / ⌥⌘↓ in the grid: the collection shown moves up/down within its level.
    pub(super) fn collection_order_keys(&mut self, ctx: &egui::Context) {
        let step = if crate::shortcuts::pressed(ctx, "move_collection_up") {
            Step::Up
        } else if crate::shortcuts::pressed(ctx, "move_collection_down") {
            Step::Down
        } else {
            return;
        };
        if let Source::Collection(id) | Source::Smart(id) = self.source {
            self.step_collection(id, step);
        }
    }

    /// Sets for the choice "Im Sammlungssatz" (new collection, save as smart collection): top level first, then
    /// every set by its path; never the set "Apple Fotos" (it only mirrors Apple Photos).
    pub(super) fn set_choices(&self) -> Vec<(Option<i64>, String)> {
        let mut sets: Vec<(Option<i64>, String)> = vec![(None, crate::tr("Keiner (oberste Ebene)").to_string())];
        let mut more: Vec<(Option<i64>, String)> = self.collections.iter().filter(|s| s.kind == 1 && !s.apple).map(|s| (Some(s.id), self.collection_path(s.id))).collect();
        more.sort_by_key(|(_, p)| p.to_lowercase());
        sets.extend(more);
        sets
    }

    /// The choice "Im Sammlungssatz" of a dialog (`sets` from [`Self::set_choices`]).
    pub(super) fn set_choice(ui: &mut egui::Ui, salt: &str, sets: &[(Option<i64>, String)], parent: &mut Option<i64>) {
        ui.label(egui::RichText::new(crate::tr("Im Sammlungssatz")).small().color(lr::TEXT_DIM()));
        let shown = sets.iter().find(|(id, _)| id == parent).map(|(_, n)| n.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt(salt).selected_text(shown).width(ui.available_width()).show_ui(ui, |ui| {
            for (id, n) in sets {
                ui.selectable_value(parent, *id, n);
            }
        });
    }

    /// After the first load: open the sets that were open at the end of the last session (and the sets above them).
    pub(super) fn restore_open_collections(&mut self) {
        let saved: Vec<i64> = library::settings::value(OPEN_KEY).as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
        for id in saved {
            if self.collections.iter().any(|c| c.id == id && c.kind == 1) {
                self.open_collections.insert(id);
            }
        }
        self.open_collections_saved = Some(self.open_collections.clone());
    }

    /// Open the sets above a collection (it becomes visible in the panel).
    pub(super) fn open_parents_of(&mut self, id: i64) {
        let mut cur = self.collections.iter().find(|c| c.id == id).and_then(|c| c.parent);
        for _ in 0..32 {
            let Some(p) = cur else { break };
            self.open_collections.insert(p);
            cur = self.collections.iter().find(|c| c.id == p).and_then(|c| c.parent);
        }
    }

    /// Write the open sets when they changed (only after they were restored; a small settings file, rarely).
    pub(super) fn remember_open_collections(&mut self) {
        let Some(saved) = &self.open_collections_saved else { return };
        if *saved == self.open_collections {
            return;
        }
        let mut ids: Vec<i64> = self.open_collections.iter().copied().filter(|id| self.collections.iter().any(|c| c.id == *id)).collect();
        ids.sort_unstable();
        library::settings::set(OPEN_KEY, serde_json::json!(ids));
        self.open_collections_saved = Some(self.open_collections.clone());
    }
}
