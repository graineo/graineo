//! Managing the library in the left panel: remove folders and photos from the library, the list of imports,
//! create, rename, move and delete collections, sets, smart collections, keywords and people, and dragging
//! photos onto a collection.
//!
//! Rules for everything that removes entries: the files stay untouched; a dialog names what is lost; the
//! writing runs in the background (`library_writer`), the UI never waits; ⌘Z puts it back
//! (`library::manage::undo`).

use super::*;
use library::manage::{self, Loss, Snapshot};

#[path = "keyword_arrange.rs"]
mod keyword_arrange;

/// Limits of the ⌘Z stack of the management: at most this many steps and rows (older steps drop out).
const UNDO_STEPS: usize = 50;
const UNDO_ROWS: usize = 200_000;

/// Photos dragged from the grid or the filmstrip (egui drag and drop payload).
#[derive(Clone, Debug)]
pub struct PhotoDrag {
    pub images: Vec<i64>,
    /// file of the photo grabbed (thumbnail of the drag image)
    pub file: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Target {
    Collection(i64),
    Keyword(i64),
}

/// Open question of the management (one at a time).
pub(super) enum Ask {
    RemoveFolder { root: i64, rel: String, name: String, loss: Option<Result<Loss, String>> },
    RemovePhotos { images: Vec<i64>, loss: Option<Result<Loss, String>> },
    NewCollection { kind: i64, name: String, parent: Option<i64>, with_selection: bool, selected: Vec<i64>, rule: String },
    Rename { target: Target, name: String },
    /// renaming a keyword onto a name that exists at the same place: merge `from` into `into`
    MergeKeyword { from: i64, into: i64 },
    /// merging several keywords into one (dialog "Stichwörter ordnen …", ⌥-drag)
    MergeKeywords { from: Vec<i64>, into: i64 },
    /// "never export" marks from the Lightroom catalog: the keywords that would be marked (background)
    LightroomMarks { ids: Option<Result<Vec<i64>, String>> },
    /// applying suggestions for ordering: (change, shown text), photos touched (background)
    ApplySuggestions { items: Vec<(library::keyword_suggest::Change, String)>, impact: Option<Result<usize, String>> },
    /// deleting a keyword with terms below it: everything, or only it (its children move up one level)
    DeleteKeyword { id: i64, name: String, keep_children: bool, loss: Option<Result<(usize, usize, usize), String>> },
    Delete { target: Target, name: String, detail: Option<Result<String, String>> },
    MoveKeyword { id: i64, filter: String },
    Imports { list: Option<Result<Vec<manage::Import>, String>> },
    RemoveImport { run: i64, label: String, entry_only: bool, loss: Option<Result<Loss, String>> },
}

pub(super) enum Undo {
    Snap { label: String, snap: Snapshot, keywords: bool, smart: bool },
    /// own order of collections (collection_order_ui.rs): only the list is loaded again afterwards
    Order { label: String, snap: Snapshot },
    Added { label: String, snap: Snapshot },
    Quick { label: String, images: Vec<i64> },
    /// photos taken out of the quick collection: ⌘Z puts them back in
    QuickRemoved { label: String, images: Vec<i64> },
}

#[derive(Default)]
pub(super) struct Manage {
    pub ask: Option<Ask>,
    pub undo: Vec<Undo>,
    /// when each step was made (parallel to `undo`, [`crate::undo_order::tick`])
    pub undo_at: Vec<u64>,
    /// the open dialog "Stichwörter ordnen …"
    pub(super) arrange: Option<keyword_arrange::Arrange>,
}

impl Manage {
    /// A new undoable step of the management.
    pub(super) fn push(&mut self, u: Undo) {
        self.undo.push(u);
        self.undo_at.push(crate::undo_order::tick());
    }
}

impl Undo {
    /// Rows the step holds (limit of the stack).
    fn size(&self) -> usize {
        match self {
            Undo::Snap { snap, .. } | Undo::Order { snap, .. } | Undo::Added { snap, .. } => snap.size(),
            Undo::Quick { images, .. } | Undo::QuickRemoved { images, .. } => images.len(),
        }
    }
}

/// Loss as dialog lines ("12 Fotos", "3 bearbeitete Fotos" …), only what is there.
pub(super) fn loss_lines(l: &Loss) -> Vec<String> {
    use crate::dialog::count;
    let mut v = vec![count(l.photos, crate::tr("Foto"), crate::tr("Fotos"))];
    for (n, one, many) in [
        (l.copies, "virtuelle Kopie", "virtuelle Kopien"),
        (l.edited, "bearbeitetes Foto (Entwicklung)", "bearbeitete Fotos (Entwicklung)"),
        (l.rated, "bewertetes Foto (Sterne, Markierung, Etikett)", "bewertete Fotos (Sterne, Markierung, Etikett)"),
        (l.keywords, "Stichwort-Zuordnung", "Stichwort-Zuordnungen"),
        (l.collections, "Zuordnung zu einer Sammlung", "Zuordnungen zu Sammlungen"),
        (l.faces, "benanntes Gesicht", "benannte Gesichter"),
    ] {
        if n > 0 {
            v.push(count(n, crate::tr(one), crate::tr(many)));
        }
    }
    v
}

/// Date of an import (local, short).
fn import_date(t: i64) -> String {
    use crate::menu::i18n;
    let l = library::platform::local_time(t);
    let date = i18n::format::date(i18n::language(), l.year, l.month as u8, l.tag as u8).unwrap_or_default();
    let time = i18n::format::time(l.hour as u8, l.minute as u8, None).unwrap_or_default();
    format!("{date} {time}")
}

fn import_kind(kind: &str) -> String {
    match kind {
        "lightroom" => crate::tr("Lightroom").into(),
        "folder" | "root_folder" => crate::tr("Ordner hinzugefügt").into(),
        "import" => crate::tr("Import").into(),
        "sync" => crate::tr("Abgleich").into(),
        other => other.to_string(),
    }
}

impl LibraryView {
    // ───────────────────────────────────────── Requests (from context menus)

    pub(super) fn ask_remove_folder(&mut self, root: i64, rel: String, name: String) {
        self.manage.ask = Some(Ask::RemoveFolder { root, rel: rel.clone(), name, loss: None });
        self.in_background(
            move |db| manage::folder_loss(db, root, &rel).map(|l| (rel, l)),
            move |b, _, r| {
                if let Some(Ask::RemoveFolder { root: r0, rel: rel0, loss, .. }) = &mut b.manage.ask {
                    match r {
                        Ok((rel, l)) if *r0 == root && *rel0 == rel => *loss = Some(Ok(l)),
                        Err(e) => *loss = Some(Err(e)),
                        _ => {}
                    }
                }
            },
        );
    }

    pub(super) fn ask_remove_photos(&mut self, images: Vec<i64>) {
        if images.is_empty() {
            return;
        }
        let v = images.clone();
        self.manage.ask = Some(Ask::RemovePhotos { images, loss: None });
        self.in_background(
            move |db| manage::photos_loss(db, &v),
            |b, _, r| {
                if let Some(Ask::RemovePhotos { loss, .. }) = &mut b.manage.ask {
                    *loss = Some(r);
                }
            },
        );
    }

    /// "New collection …" (⌘N, "+" at SAMMLUNGEN, menu): kind 0 collection, 1 set, 2 smart collection; `parent`: in this set.
    pub fn ask_new_collection(&mut self, kind: i64, parent: Option<i64>) {
        let selected = if self.nothing_selected { Vec::new() } else { self.target() };
        let with_selection = kind == manage::COLLECTION && !selected.is_empty();
        let name = match kind {
            manage::SET => crate::tr("Neuer Sammlungssatz"),
            manage::SMART => crate::tr("Neue Smart-Sammlung"),
            _ => crate::tr("Neue Sammlung"),
        }
        .to_string();
        self.manage.ask = Some(Ask::NewCollection { kind, name, parent, with_selection, selected, rule: self.filter.text.trim().to_string() });
    }

    pub fn ask_imports_external(&mut self) {
        self.ask_imports();
    }

    pub(super) fn ask_imports(&mut self) {
        self.manage.ask = Some(Ask::Imports { list: None });
        self.in_background(
            manage::imports,
            |b, _, r| {
                if let Some(Ask::Imports { list }) = &mut b.manage.ask {
                    *list = Some(r);
                }
            },
        );
    }

    fn ask_remove_import(&mut self, run: i64, label: String, entry_only: bool) {
        self.manage.ask = Some(Ask::RemoveImport { run, label, entry_only, loss: None });
        if !entry_only {
            self.in_background(
                move |db| manage::import_loss(db, run),
                |b, _, r| {
                    if let Some(Ask::RemoveImport { loss, .. }) = &mut b.manage.ask {
                        *loss = Some(r);
                    }
                },
            );
        }
    }

    pub(super) fn ask_delete(&mut self, target: Target, name: String) {
        self.manage.ask = Some(Ask::Delete { target, name, detail: None });
        self.in_background(
            move |db| match target {
                Target::Collection(id) => {
                    let (n, photos) = manage::collection_loss(db, id)?;
                    let mut t = crate::trf!("Fotos in ihr: {a}", a = photos);
                    if n > 1 {
                        t = crate::trf!("{a} Sammlungen und Sätze darin, Fotos in ihnen: {b}", a = n - 1, b = photos);
                    }
                    Ok(t)
                }
                Target::Keyword(id) => manage::keyword_loss(db, id).map(|n| crate::trf!("Fotos mit diesem Stichwort (oder einem darunter): {a}", a = n)),
            },
            |b, _, r| {
                if let Some(Ask::Delete { detail, .. }) = &mut b.manage.ask {
                    *detail = Some(r);
                }
            },
        );
    }

    // ───────────────────────────────────────── Context menus

    /// Context menu of a collection, set or smart collection.
    pub(super) fn collection_menu(&mut self, ui: &mut egui::Ui, c: &library::db::Collection) {
        let id = c.id;
        let lr_set = |ui: &mut egui::Ui| ui.separator();
        // the set "Apple Fotos" mirrors Apple Photos: nothing is created in it
        if c.kind == 1 && !c.apple {
            if lr::menu_item(ui, crate::tr("Sammlung erstellen …"), "", true) {
                self.ask_new_collection(manage::COLLECTION, Some(id));
            }
            if lr::menu_item(ui, crate::tr("Smart-Sammlung erstellen …"), "", true) {
                self.ask_new_collection(manage::SMART, Some(id));
            }
            if lr::menu_item(ui, crate::tr("Sammlungssatz erstellen …"), "", true) {
                self.ask_new_collection(manage::SET, Some(id));
            }
            lr_set(ui);
        }
        if lr::menu_item(ui, crate::tr("Umbenennen …"), "", true) {
            self.manage.ask = Some(Ask::Rename { target: Target::Collection(id), name: c.name.clone() });
        }
        // own order within the level (collection_order_ui.rs); a set: its content by name again
        self.order_menu(ui, c);
        if c.kind == 1 {
            self.sort_by_name_item(ui, Some(id));
        }
        // move: top level or into another set (never into itself or below, Apple Photos only within its set)
        let free = !Self::ordering_locked();
        let allowed = |list: &[library::db::Collection], to: Option<i64>| library::collection_order::check(list, id, to).is_ok();
        let sets: Vec<(i64, String)> = {
            let mut v: Vec<(i64, String)> = self.collections.iter().filter(|s| s.kind == 1 && allowed(&self.collections, Some(s.id))).map(|s| (s.id, self.collection_path(s.id))).collect();
            v.sort_by_key(|(_, p)| p.to_lowercase());
            v
        };
        let top = allowed(&self.collections, None);
        let moved = ui.add_enabled_ui(free && (top || !sets.is_empty()), |ui| ui.menu_button(crate::tr("Verschieben nach"), |ui| {
            let why = if c.parent.is_none() { crate::tr("Steht schon auf der obersten Ebene") } else { crate::tr("Alben aus Apple Fotos bleiben in ihrem Satz") };
            if lr::menu_item_why(ui, crate::tr("Oberste Ebene"), "", top && c.parent.is_some(), why) {
                self.move_collection(id, None);
            }
            if !sets.is_empty() {
                ui.separator();
            }
            for (s, path) in &sets {
                if lr::menu_item(ui, path, "", c.parent != Some(*s)) {
                    self.move_collection(id, Some(*s));
                }
            }
        }));
        if !free {
            // a disabled widget reports no hover: the reason on a hover area over the same place
            let probe = ui.interact(moved.response.rect, moved.response.id.with("warum"), egui::Sense::hover());
            lr::hover_tip(ui, &probe, Self::locked_why());
        }
        lr_set(ui);
        let text = match c.kind {
            1 => crate::tr("Sammlungssatz löschen …"),
            2 => crate::tr("Smart-Sammlung löschen …"),
            _ => crate::tr("Sammlung löschen …"),
        };
        if lr::menu_item(ui, text, "", true) {
            self.ask_delete(Target::Collection(id), c.name.clone());
        }
    }

    /// Context menu of a keyword (also a person).
    /// A keyword action from the context menu of the keyword column of the metadata filter.
    pub(super) fn keyword_action(&mut self, id: i64, action: crate::library_filter::KeywordAction) {
        use crate::library_filter::KeywordAction as A;
        let Some(k) = self.keywords.iter().find(|k| k.id == id).cloned() else { return };
        match action {
            A::Rename => self.manage.ask = Some(Ask::Rename { target: Target::Keyword(id), name: k.name.clone() }),
            A::Move => self.manage.ask = Some(Ask::MoveKeyword { id, filter: String::new() }),
            A::ToTop => self.move_keyword(id, None),
            A::Delete => self.ask_delete_keyword(id),
            A::NewChild => self.open_arrange_with_child(id),
            A::Arrange => self.open_arrange(Some(id)),
        }
    }

    /// Delete a keyword: with terms below it the question offers both ways (everything / only it), else the plain
    /// question.
    pub(super) fn ask_delete_keyword(&mut self, id: i64) {
        let Some(k) = self.keywords.iter().find(|k| k.id == id).cloned() else { return };
        if !self.keywords.iter().any(|c| c.parent == Some(id)) {
            self.ask_delete(Target::Keyword(id), k.name.clone());
            return;
        }
        self.manage.ask = Some(Ask::DeleteKeyword { id, name: k.name.clone(), keep_children: false, loss: None });
        self.in_background(
            move |db| library::keyword_arrange::delete_loss(db, id),
            move |b, _, r| {
                if let Some(Ask::DeleteKeyword { id: i, loss, .. }) = &mut b.manage.ask {
                    if *i == id {
                        *loss = Some(r);
                    }
                }
            },
        );
    }

    /// Move a keyword below `parent` (`None`: top level). A keyword of the same name already there (ignoring case)
    /// is never duplicated: the question to merge into it comes instead.
    fn move_keyword(&mut self, id: i64, parent: Option<i64>) {
        let Some(k) = self.keywords.iter().find(|k| k.id == id).cloned() else { return };
        let name = k.name.trim().to_lowercase();
        if let Some(into) = self.keywords.iter().find(|x| x.id != id && x.parent == parent && x.name.trim().to_lowercase() == name).map(|x| x.id) {
            self.manage.ask = Some(Ask::MergeKeyword { from: id, into });
            return;
        }
        self.keyword_job(crate::trf!("„{a}“ verschoben", a = k.name), move |db| manage::move_keyword(db, id, parent));
    }

    /// "Set › Subset" for menus.
    pub(super) fn collection_path(&self, id: i64) -> String {
        let mut v = Vec::new();
        let mut cur = Some(id);
        for _ in 0..20 {
            let Some(c) = cur.and_then(|c| self.collections.iter().find(|s| s.id == c)) else { break };
            v.push(c.name.clone());
            cur = c.parent;
        }
        v.reverse();
        v.join(" › ")
    }

    // ───────────────────────────────────────── Jobs in the background

    fn collection_job(&mut self, label: String, smart: bool, work: impl FnOnce(&Db) -> Result<Snapshot, String> + Send + 'static) {
        self.in_background(
            move |db| {
                let snap = work(db)?;
                if smart {
                    // optional copy of the smart collections in the root folder (only with "write sidecars")
                    let _ = library::collection_file::write_if_enabled(db);
                }
                Ok(snap)
            },
            move |b, actions, r| match r {
                Ok(snap) => {
                    b.after_collections_changed();
                    actions.push(Action::Status(label.clone()));
                    b.manage.push(Undo::Snap { label, snap, keywords: false, smart });
                    b.ops += 1;
                }
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            },
        );
    }

    fn keyword_job(&mut self, label: String, work: impl FnOnce(&Db) -> Result<Snapshot, String> + Send + 'static) {
        let ticket = self.load_ticket();
        self.in_background(
            move |db| work(db).map(|s| (s, ticket.load(db).ok())),
            move |b, actions, r| match r {
                Ok((snap, loaded)) => {
                    b.reload_keywords();
                    b.adopt_or_reload(loaded);
                    // rules of smart collections may have followed a rename or a merge
                    b.after_collections_changed();
                    actions.push(Action::Status(keyword_status(&label, &snap)));
                    b.manage.push(Undo::Snap { label, snap, keywords: true, smart: true });
                    b.ops += 1;
                }
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            },
        );
    }

    /// Removal of photos (folder, import, selection): re-read the list afterwards.
    fn photos_job(&mut self, label: String, work: impl FnOnce(&Db) -> Result<Snapshot, String> + Send + 'static) {
        let ticket = self.load_ticket();
        self.in_background(
            move |db| work(db).map(|s| (s, ticket.load(db).ok())),
            move |b, actions, r| match r {
                Ok((snap, loaded)) => {
                    b.adopt_or_reload(loaded);
                    b.build_tree();
                    if matches!(b.source, Source::Folder(..)) && !b.folder_source_exists() {
                        b.set_source(Source::All);
                    }
                    actions.push(Action::Status(label.clone()));
                    b.manage.push(Undo::Snap { label, snap, keywords: false, smart: false });
                    b.ops += 1;
                }
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            },
        );
    }

    fn folder_source_exists(&self) -> bool {
        match &self.source {
            Source::Folder(root, path) => self.folder.iter().any(|o| o.root == *root && o.path.starts_with(path.as_str())) || self.roots.iter().any(|r| r.id == *root) && path.is_empty(),
            _ => true,
        }
    }

    pub(super) fn after_collections_changed(&mut self) {
        if let Some(db) = &self.db {
            let list = db.collections().unwrap_or_default();
            self.set_collections(list);
        }
        self.smart_counts.clear();
        let gone = match self.source {
            Source::Collection(id) | Source::Smart(id) => !self.collections.iter().any(|c| c.id == id),
            _ => false,
        };
        if gone {
            self.set_source(Source::All);
        }
    }

    /// "Verschieben nach": to the end of the target level (shown at once, `collection_order_ui.rs`).
    fn move_collection(&mut self, id: i64, parent: Option<i64>) {
        self.place_collection(id, parent, None);
    }

    /// ⌘Z of the management: the newest step. Returns: something was undone.
    pub(super) fn manage_undo(&mut self) -> bool {
        let Some(u) = self.manage.undo.pop() else { return false };
        self.manage.undo_at.pop();
        match u {
            Undo::Quick { label, images } => {
                for b in &images {
                    self.quick.remove(b);
                }
                self.quick_send(QuickJob::Set(images, false));
                self.manage_message = Some(crate::trf!("Rückgängig: {a}", a = label));
            }
            Undo::QuickRemoved { label, images } => {
                self.quick.extend(images.iter().copied());
                if self.source == Source::Quick {
                    self.source_changed();
                }
                self.quick_send(QuickJob::Set(images, true));
                self.manage_message = Some(crate::trf!("Rückgängig: {a}", a = label));
            }
            Undo::Added { label, snap } => {
                self.in_background(
                    move |db| manage::undo_add(db, &snap),
                    move |b, actions, r| match r {
                        Ok(()) => {
                            b.after_collections_changed();
                            b.reload();
                            actions.push(Action::Status(crate::trf!("Rückgängig: {a}", a = label)));
                        }
                        Err(e) => b.dialog = Some(Dialog::Error(e)),
                    },
                );
            }
            Undo::Order { label, snap } => {
                self.in_background(
                    move |db| manage::undo(db, &snap),
                    move |b, actions, r| {
                        b.after_collections_changed();
                        match r {
                            Ok(()) => actions.push(Action::Status(crate::trf!("Rückgängig: {a}", a = label))),
                            Err(e) => b.dialog = Some(Dialog::Error(e)),
                        }
                    },
                );
            }
            Undo::Snap { label, snap, keywords, smart } => {
                let ticket = self.load_ticket();
                self.in_background(
                    move |db| {
                        manage::undo(db, &snap)?;
                        // the undo is committed: sidecars and search entries following it only warn
                        let warning = if keywords { manage::after_keyword_undo(db, &snap).err() } else { None };
                        if let Some(e) = &warning {
                            eprintln!("undo of a keyword change: follow-up failed: {e}");
                        }
                        if smart {
                            let _ = library::collection_file::write_if_enabled(db);
                        }
                        Ok((ticket.load(db).ok(), warning))
                    },
                    move |b, actions, r| match r {
                        Ok((loaded, warning)) => {
                            b.after_collections_changed();
                            b.reload_keywords();
                            b.adopt_or_reload(loaded);
                            b.build_tree();
                            let mut text = crate::trf!("Rückgängig: {a}", a = label);
                            if let Some(e) = warning {
                                text = format!("{text} · {}", crate::trf!("Gespeichert, aber nicht alles nachgezogen: {e}", e = e));
                            }
                            actions.push(Action::Status(text));
                        }
                        Err(e) => b.dialog = Some(Dialog::Error(e)),
                    },
                );
            }
        }
        true
    }

    // ───────────────────────────────────────── Taking photos out of a collection

    /// The collection the grid shows, if photos can be taken out of it (a plain collection or the quick
    /// collection): its name. `None` everywhere else (folders, smart collections, search …).
    pub fn removable_collection(&self) -> Option<String> {
        match self.source {
            Source::Quick => Some(crate::tr("Schnellsammlung").to_string()),
            Source::Collection(id) => {
                self.collections.iter().find(|c| c.id == id && c.kind as i64 == manage::COLLECTION).map(|c| c.name.clone())
            }
            _ => None,
        }
    }

    /// Take `images` out of the collection shown (⌫ in a collection, context menu): only the assignment, the
    /// photos and their files stay; ⌘Z puts them back. Nothing happens outside a collection.
    pub fn remove_from_collection(&mut self, images: Vec<i64>, actions: &mut Vec<Action>) {
        let Some(name) = self.removable_collection() else { return };
        if images.is_empty() {
            return;
        }
        let n = images.len();
        let label = crate::trf!("{a} aus „{b}“ entfernt", a = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")), b = name);
        match self.source {
            Source::Quick => {
                let inside: Vec<i64> = images.into_iter().filter(|b| self.quick.contains(b)).collect();
                if inside.is_empty() {
                    return;
                }
                for b in &inside {
                    self.quick.remove(b);
                }
                self.source_changed();
                self.quick_send(QuickJob::Set(inside.clone(), false));
                actions.push(Action::Status(label.clone()));
                self.manage.push(Undo::QuickRemoved { label, images: inside });
            }
            Source::Collection(id) => {
                let ticket = self.load_ticket();
                self.in_background(
                    move |db| manage::remove_from_collection(db, id, &images).map(|(n, snap)| (n, snap, ticket.load(db).ok())),
                    move |b, actions, r| match r {
                        Ok((0, _, _)) => {}
                        Ok((_, snap, loaded)) => {
                            b.after_collections_changed();
                            b.adopt_or_reload(loaded);
                            actions.push(Action::Status(label.clone()));
                            b.manage.push(Undo::Snap { label, snap, keywords: false, smart: false });
                        }
                        Err(e) => b.dialog = Some(Dialog::Error(e)),
                    },
                );
            }
            _ => {}
        }
    }

    /// Like [`Self::remove_from_collection`] for a call from outside the grid (the ⌫ key in every view).
    pub fn remove_from_collection_external(&mut self, images: Vec<i64>) -> Vec<Action> {
        let mut a = Vec::new();
        self.remove_from_collection(images, &mut a);
        a
    }

    // ───────────────────────────────────────── Drag and drop onto collections

    /// Add dragged photos to a collection (or the quick collection).
    pub(super) fn drop_on_collection(&mut self, id: i64, images: Vec<i64>) {
        let name = self.collections.iter().find(|c| c.id == id).map(|c| c.name.clone()).unwrap_or_default();
        self.in_background(
            move |db| manage::add_to_collection(db, id, &images),
            move |b, actions, r| match r {
                Ok((n, snap)) => {
                    b.after_collections_changed();
                    let text = if n == 0 {
                        crate::trf!("Schon in „{a}“", a = name)
                    } else {
                        crate::trf!("{a} zu „{b}“ hinzugefügt", a = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")), b = name)
                    };
                    actions.push(Action::Status(text.clone()));
                    if n > 0 {
                        b.manage.push(Undo::Added { label: text, snap });
                    }
                }
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            },
        );
    }

    pub(super) fn drop_on_quick(&mut self, images: Vec<i64>) -> String {
        let new: Vec<i64> = images.into_iter().filter(|b| !self.quick.contains(b)).collect();
        if new.is_empty() {
            return crate::tr("Schon in der Schnellsammlung").into();
        }
        self.quick.extend(new.iter().copied());
        self.quick_send(QuickJob::Set(new.clone(), true));
        let text = crate::trf!("{a} zur Schnellsammlung hinzugefügt", a = crate::dialog::count(new.len(), crate::tr("Foto"), crate::tr("Fotos")));
        self.manage.push(Undo::Quick { label: text.clone(), images: new });
        text
    }

    /// The photos a drag takes: the whole selection if the grabbed photo belongs to it, otherwise only that one.
    pub fn drag_payload(&self, image: i64, file: i64) -> PhotoDrag {
        let sel = if self.nothing_selected { Vec::new() } else { self.target() };
        let images = if sel.len() > 1 && sel.contains(&image) { sel } else { vec![image] };
        PhotoDrag { images, file }
    }

    /// Drag image at the pointer: small thumbnail with the number of photos.
    pub fn paint_drag(&mut self, ctx: &egui::Context) {
        let Some(p) = egui::DragAndDrop::payload::<PhotoDrag>(ctx) else { return };
        let Some(pos) = ctx.pointer_latest_pos() else { return };
        let level = preview::small();
        let tex = self.textures.get(&(p.file, level)).map(|(t, _)| t.clone());
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("photo-drag")));
        let at = pos + egui::vec2(14.0, 10.0);
        let size = match &tex {
            Some(t) => {
                let s = t.size_vec2();
                s * (64.0 / s.x.max(s.y))
            }
            None => egui::vec2(64.0, 44.0),
        };
        let r = egui::Rect::from_min_size(at, size);
        if p.images.len() > 1 {
            painter.rect_filled(r.translate(egui::vec2(4.0, 4.0)), 0.0, Color32::from_gray(0x50));
        }
        match &tex {
            Some(t) => {
                painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::from_white_alpha(230));
            }
            None => {
                painter.rect_filled(r, 0.0, Color32::from_gray(0x30));
            }
        }
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0xc0)), egui::StrokeKind::Outside);
        let n = p.images.len().to_string();
        let g = painter.layout_no_wrap(n, egui::FontId::proportional(10.5), Color32::WHITE);
        let badge = egui::Rect::from_min_size(r.right_top() + egui::vec2(-g.size().x - 6.0, -8.0), g.size() + egui::vec2(8.0, 4.0));
        painter.rect_filled(badge, 0.0, Color32::from_black_alpha(200));
        painter.galley(badge.min + egui::vec2(4.0, 2.0), g, Color32::WHITE);
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    /// Drop target row of the left panel: highlight while photos hover over it (blocked targets visibly dimmed),
    /// returns the photos dropped onto an allowed target.
    pub(super) fn drop_target(ui: &egui::Ui, resp: &egui::Response, allowed: bool) -> Option<Vec<i64>> {
        let hovering = resp.dnd_hover_payload::<PhotoDrag>().is_some();
        if hovering {
            if allowed {
                ui.painter().rect_stroke(resp.rect, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0xe0)), egui::StrokeKind::Inside);
            } else {
                ui.painter().rect_filled(resp.rect, 0.0, Color32::from_black_alpha(110));
                ui.ctx().set_cursor_icon(egui::CursorIcon::NoDrop);
            }
        }
        if !allowed || !hovering {
            return None;
        }
        // only with photos: egui's release takes any payload before checking its type (a dragged collection would be lost)
        resp.dnd_release_payload::<PhotoDrag>().map(|p| p.images.clone())
    }

    // ───────────────────────────────────────── Dialogs

    pub(super) fn manage_dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::dialog::{self, Answer, Buttons};
        if let Some(m) = self.manage_message.take() {
            actions.push(Action::Status(m));
        }
        // the ⌘Z stack keeps the newest steps (large merges hold many rows)
        let mut dropped = 0;
        while self.manage.undo.len() > UNDO_STEPS || self.manage.undo.len() > 1 && self.manage.undo.iter().map(Undo::size).sum::<usize>() > UNDO_ROWS {
            self.manage.undo.remove(0);
            self.manage.undo_at.remove(0);
            dropped += 1;
        }
        if dropped > 0 {
            actions.push(Action::Status(crate::trf!("Ältere Schritte lassen sich nicht mehr rückgängig machen ({n})", n = dropped)));
        }
        // the dialog "Stichwörter ordnen …" lies below the questions it opens
        self.arrange_dialog(ctx, actions);
        let Some(mut ask) = self.manage.ask.take() else { return };
        let mut keep = true;
        match &mut ask {
            Ask::RemoveFolder { root, rel, name, loss } => {
                let ready = matches!(loss, Some(Ok(_)));
                let a = dialog::show(
                    ctx,
                    "bib-ordner-entfernen",
                    &crate::trf!("„{a}“ aus der Bibliothek entfernen?", a = name),
                    dialog::WIDTH,
                    |ui| {
                        dialog::text(ui, crate::tr("Die Dateien auf dem Datenträger bleiben unverändert. Aus der Bibliothek verschwinden, mit allen Unterordnern:"));
                        match loss {
                            None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                            Some(Ok(l)) => loss_lines(l).iter().for_each(|t| dialog::chip(ui, t)),
                            Some(Err(e)) => dialog::warning(ui, e),
                        }
                        let hint = if rel.is_empty() {
                            crate::tr("⌘Z holt alles zurück. Später wieder aufnehmen: Importieren … → An Ort und Stelle.")
                        } else {
                            crate::tr("Der Abgleich nimmt den Ordner nicht wieder auf. ⌘Z holt alles zurück; Importieren … → An Ort und Stelle nimmt ihn später wieder auf.")
                        };
                        dialog::hint(ui, hint);
                    },
                    Buttons::destructive(crate::tr("Entfernen"), ready),
                );
                match a {
                    Answer::Primary => {
                        let (root, rel) = (*root, rel.clone());
                        self.photos_job(crate::trf!("„{a}“ aus der Bibliothek entfernt", a = name), move |db| manage::remove_folder(db, root, &rel));
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::RemovePhotos { images, loss } => {
                let ready = matches!(loss, Some(Ok(_)));
                let title = crate::trf!("{a} aus der Bibliothek entfernen?", a = dialog::count(images.len(), crate::tr("Foto"), crate::tr("Fotos")));
                let a = dialog::show(
                    ctx,
                    "bib-fotos-entfernen",
                    &title,
                    dialog::WIDTH,
                    |ui| {
                        dialog::text(ui, crate::tr("Die Dateien bleiben unverändert und kommen nicht in den Papierkorb. Aus der Bibliothek verschwinden:"));
                        match loss {
                            None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                            Some(Ok(l)) => loss_lines(l).iter().for_each(|t| dialog::chip(ui, t)),
                            Some(Err(e)) => dialog::warning(ui, e),
                        }
                        dialog::hint(ui, crate::tr("Der Abgleich nimmt die Fotos nicht wieder auf. ⌘Z holt alles zurück."));
                    },
                    Buttons::destructive(crate::tr("Entfernen"), ready),
                );
                match a {
                    Answer::Primary => {
                        let v = images.clone();
                        let label = crate::trf!("{a} aus der Bibliothek entfernt", a = dialog::count(v.len(), crate::tr("Foto"), crate::tr("Fotos")));
                        self.photos_job(label, move |db| manage::remove_photos(db, &v));
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::NewCollection { kind, name, parent, with_selection, selected, rule } => {
                let title = match *kind {
                    manage::SET => crate::tr("Sammlungssatz erstellen"),
                    manage::SMART => crate::tr("Smart-Sammlung erstellen"),
                    _ => crate::tr("Sammlung erstellen"),
                };
                let sets = self.set_choices();
                let can = !name.trim().is_empty() && (*kind != manage::SMART || !rule.trim().is_empty());
                let a = dialog::show(
                    ctx,
                    "bib-neue-sammlung",
                    title,
                    dialog::WIDTH,
                    |ui| {
                        dialog::input(ui, crate::tr("Name"), name);
                        ui.add_space(4.0);
                        Self::set_choice(ui, "bib-neue-sammlung-satz", &sets, parent);
                        if *kind == manage::SMART {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(crate::tr("Regel (Suche, z. B. kamera:dp2 sterne>=3)")).small().color(lr::TEXT_DIM));
                            lr::edit_field(ui, egui::TextEdit::singleline(rule).desired_width(ui.available_width()));
                        }
                        if *kind == manage::COLLECTION {
                            ui.add_space(4.0);
                            let label = crate::trf!("Ausgewählte Fotos aufnehmen ({a})", a = selected.len());
                            ui.add_enabled_ui(!selected.is_empty(), |ui| {
                                dialog::option(ui, with_selection, &label, "", None);
                            });
                        }
                    },
                    Buttons::confirm(crate::tr("Erstellen"), can),
                );
                match a {
                    Answer::Primary => {
                        let (kind, name, parent) = (*kind, name.trim().to_string(), *parent);
                        let images = if *with_selection { selected.clone() } else { Vec::new() };
                        let rule = (kind == manage::SMART).then(|| rule.trim().to_string());
                        if let Some(p) = parent {
                            self.open_collections.insert(p);
                        }
                        self.in_background(
                            move |db| {
                                let id = manage::create_collection(db, kind, &name, parent, rule.as_deref())?;
                                if !images.is_empty() {
                                    manage::add_to_collection(db, id, &images)?;
                                }
                                if kind == manage::SMART {
                                    let _ = library::collection_file::write_if_enabled(db);
                                }
                                Ok((id, name))
                            },
                            move |b, actions, r| match r {
                                Ok((id, name)) => {
                                    b.after_collections_changed();
                                    // in an ordered level it comes last: show it there
                                    b.open_parents_of(id);
                                    b.collection_drag.scroll_to = Some(id);
                                    b.collection_drag.flash(id);
                                    // the new collection is chosen (a set is opened)
                                    match kind {
                                        manage::SET => {
                                            b.open_collections.insert(id);
                                        }
                                        manage::SMART => b.set_source(Source::Smart(id)),
                                        _ => b.set_source(Source::Collection(id)),
                                    }
                                    let snap_label = crate::trf!("„{a}“ erstellt", a = name);
                                    actions.push(Action::Status(snap_label));
                                }
                                Err(e) => b.dialog = Some(Dialog::Error(e)),
                            },
                        );
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::Rename { target, name } => {
                let old = match target {
                    Target::Collection(id) => self.collections.iter().find(|c| c.id == *id).map(|c| c.name.clone()),
                    Target::Keyword(id) => self.keywords.iter().find(|k| k.id == *id).map(|k| k.name.clone()),
                }
                .unwrap_or_default();
                let can = !name.trim().is_empty() && name.trim() != old;
                // a keyword of this name at the same place: renaming becomes merging (with its own question)
                let clash = match *target {
                    Target::Keyword(id) => {
                        let parent = self.keywords.iter().find(|k| k.id == id).and_then(|k| k.parent);
                        self.keywords.iter().find(|k| k.id != id && k.parent == parent && k.name.trim().to_lowercase() == name.trim().to_lowercase()).map(|k| k.id)
                    }
                    Target::Collection(_) => None,
                };
                let button = if clash.is_some() { crate::tr("Zusammenführen …") } else { crate::tr("Umbenennen") };
                let a = dialog::show(ctx, "bib-umbenennen", &crate::trf!("„{a}“ umbenennen", a = old), dialog::WIDTH, |ui| {
                    dialog::input(ui, crate::tr("Neuer Name"), name);
                    if clash.is_some() {
                        dialog::warning(ui, &crate::trf!("„{a}“ gibt es hier schon.", a = name.trim()));
                    }
                }, Buttons::confirm(button, can));
                match a {
                    Answer::Primary if clash.is_some() => {
                        if let (Target::Keyword(from), Some(into)) = (*target, clash) {
                            self.manage.ask = Some(Ask::MergeKeyword { from, into });
                        }
                        keep = false;
                    }
                    Answer::Primary => {
                        let new = name.trim().to_string();
                        let label = crate::trf!("„{a}“ umbenannt in „{b}“", a = old, b = new);
                        match *target {
                            Target::Collection(id) => {
                                let smart = self.collections.iter().any(|c| c.id == id && c.kind == 2);
                                self.collection_job(label, smart, move |db| manage::rename_collection(db, id, &new));
                            }
                            Target::Keyword(id) => self.keyword_job(label, move |db| library::keyword_arrange::rename(db, id, &new)),
                        }
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::MergeKeyword { from, into } => {
                let (from, into) = (*from, *into);
                let kw = |id: i64| self.keywords.iter().find(|k| k.id == id).cloned();
                let (Some(a_kw), Some(b_kw)) = (kw(from), kw(into)) else {
                    self.manage.ask = None;
                    return;
                };
                // the paths: after a move the two may lie in different branches
                let (a_path, b_path) = (self.kw_path(from).replace('|', " › "), self.kw_path(into).replace('|', " › "));
                let large = a_kw.count >= library::keywords::BACKUP_FROM;
                let a = dialog::show(ctx, "bib-stichwort-zusammenfuehren", &crate::trf!("„{a}“ mit „{b}“ zusammenführen?", a = a_kw.name, b = b_kw.name), dialog::WIDTH, |ui| {
                    dialog::text(ui, &crate::trf!("Alle Fotos mit „{a}“ bekommen „{b}“. „{a}“ verschwindet; Stichwörter darunter liegen danach unter „{b}“.", a = a_kw.name, b = b_kw.name));
                    dialog::chip(ui, &crate::trf!("„{a}“: {n}", a = a_path, n = crate::dialog::count(a_kw.count, crate::tr("Foto"), crate::tr("Fotos"))));
                    dialog::chip(ui, &crate::trf!("„{a}“: {n}", a = b_path, n = crate::dialog::count(b_kw.count, crate::tr("Foto"), crate::tr("Fotos"))));
                    if large {
                        dialog::hint(ui, crate::tr("Vorher wird die Bibliothek gesichert."));
                    }
                    dialog::hint(ui, crate::tr("⌘Z holt alles zurück."));
                }, Buttons::confirm(crate::tr("Zusammenführen"), true));
                match a {
                    Answer::Primary => {
                        let label = crate::trf!("„{a}“ mit „{b}“ zusammengeführt", a = a_kw.name, b = b_kw.name);
                        self.keyword_job(label, move |db| {
                            // a merge touching many photos: a backup of the library first (besides ⌘Z)
                            if manage::keyword_loss(db, from)? >= library::keywords::BACKUP_FROM {
                                library::backup::create_action_backup(db, "merge")?;
                            }
                            library::keyword_arrange::merge_keywords(db, &[from], into)
                        });
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::MergeKeywords { from, into } => {
                let into = *into;
                let kw = |id: i64| self.keywords.iter().find(|k| k.id == id).cloned();
                let Some(target) = kw(into) else {
                    self.manage.ask = None;
                    return;
                };
                let froms: Vec<Keyword> = from.iter().filter_map(|f| kw(*f)).collect();
                let total: usize = froms.iter().map(|k| k.count).sum();
                let a = dialog::show(ctx, "bib-stichwoerter-zusammenfuehren", &crate::trf!("{n} Stichwörter mit „{b}“ zusammenführen?", n = froms.len(), b = target.name), dialog::WIDTH, |ui| {
                    dialog::text(ui, &crate::trf!("Alle Fotos mit diesen Stichwörtern bekommen „{b}“. Die Stichwörter verschwinden; Begriffe darunter liegen danach unter „{b}“.", b = target.name));
                    for k in froms.iter().take(8) {
                        dialog::chip(ui, &crate::trf!("„{a}“: {n}", a = self.kw_path(k.id).replace('|', " › "), n = crate::dialog::count(k.count, crate::tr("Foto"), crate::tr("Fotos"))));
                    }
                    if froms.len() > 8 {
                        dialog::hint(ui, &crate::trf!("und {n} weitere", n = froms.len() - 8));
                    }
                    dialog::chip(ui, &crate::trf!("Ziel „{a}“: {n}", a = self.kw_path(into).replace('|', " › "), n = crate::dialog::count(target.count, crate::tr("Foto"), crate::tr("Fotos"))));
                    if total >= library::keywords::BACKUP_FROM {
                        dialog::hint(ui, crate::tr("Vorher wird die Bibliothek gesichert."));
                    }
                    dialog::hint(ui, crate::tr("⌘Z holt alles zurück."));
                }, Buttons::confirm(crate::tr("Zusammenführen"), !froms.is_empty()));
                match a {
                    Answer::Primary => {
                        let ids: Vec<i64> = froms.iter().map(|k| k.id).collect();
                        let label = crate::trf!("{n} Stichwörter mit „{b}“ zusammengeführt", n = ids.len(), b = target.name);
                        self.keyword_job(label, move |db| {
                            let mut n = 0;
                            for k in &ids {
                                n += manage::keyword_loss(db, *k)?;
                            }
                            if n >= library::keywords::BACKUP_FROM {
                                library::backup::create_action_backup(db, "merge")?;
                            }
                            library::keyword_arrange::merge_keywords(db, &ids, into)
                        });
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::LightroomMarks { ids } => {
                let n = matches!(ids, Some(Ok(v)) if !v.is_empty());
                let a = dialog::show(ctx, "bib-lr-marken", crate::tr("Marken aus Lightroom übernehmen?"), dialog::WIDTH, |ui| {
                    match ids {
                        None => dialog::pending(ui, crate::tr("Lightroom-Katalog wird gelesen …")),
                        Some(Err(e)) => dialog::warning(ui, e),
                        Some(Ok(v)) if v.is_empty() => dialog::text(ui, crate::tr("Alle Stichwörter, die Lightroom nicht exportiert, sind hier schon markiert.")),
                        Some(Ok(v)) => {
                            dialog::text(ui, &crate::trf!("{n} Stichwörter würden als „nicht exportieren“ markiert (mit allem darunter).", n = v.len()));
                            for id in v.iter().take(8) {
                                dialog::chip(ui, &self.kw_path(*id).replace('|', " › "));
                            }
                            if v.len() > 8 {
                                dialog::hint(ui, &crate::trf!("und {n} weitere", n = v.len() - 8));
                            }
                        }
                    }
                    dialog::hint(ui, crate::tr("Der Katalog wird nur gelesen. ⌘Z holt alles zurück."));
                }, Buttons::confirm(crate::tr("Markieren"), n));
                match a {
                    Answer::Primary => {
                        if let Some(Ok(v)) = ids.take() {
                            let label = crate::trf!("{n} Stichwörter werden nicht exportiert", n = v.len());
                            self.keyword_job(label, move |db| library::keyword_arrange::set_export(db, &v, false));
                        }
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::ApplySuggestions { items, impact } => {
                use library::keyword_suggest::Change;
                let below = items.iter().filter(|x| matches!(x.0, Change::Below { .. })).count();
                let merged = items.len() - below;
                let ready = matches!(impact, Some(Ok(_)));
                let a = dialog::show(ctx, "bib-vorschlaege-anwenden", &crate::trf!("{n} Vorschläge anwenden?", n = items.len()), dialog::WIDTH, |ui| {
                    match impact {
                        None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                        Some(Err(e)) => dialog::warning(ui, e),
                        Some(Ok(p)) => dialog::text(ui, &crate::trf!("{a} Stichwörter werden untergeordnet, {b} zusammengeführt; {c} betroffen.", a = below, b = merged, c = crate::dialog::count(*p, crate::tr("Foto"), crate::tr("Fotos")))),
                    }
                    for (_, t) in items.iter().take(6) {
                        dialog::chip(ui, t);
                    }
                    if items.len() > 6 {
                        dialog::hint(ui, &crate::trf!("und {n} weitere", n = items.len() - 6));
                    }
                    dialog::hint(ui, crate::tr("Vorher wird die Bibliothek gesichert."));
                    dialog::hint(ui, crate::tr("⌘Z holt alles zurück."));
                }, Buttons::confirm(crate::tr("Anwenden"), ready));
                match a {
                    Answer::Primary => {
                        let changes: Vec<Change> = items.iter().map(|x| x.0).collect();
                        let ticket = self.load_ticket();
                        self.in_background(
                            move |db| {
                                // always a backup first: suggestions change many keywords at once
                                library::backup::create_action_backup(db, "suggestions")?;
                                let r = library::keyword_suggest::apply(db, &changes)?;
                                Ok((r, ticket.load(db).ok()))
                            },
                            move |b, actions, r| match r {
                                Ok(((snap, done, skipped), loaded)) => {
                                    b.reload_keywords();
                                    b.adopt_or_reload(loaded);
                                    b.after_collections_changed();
                                    let label = if skipped.is_empty() { crate::trf!("{n} Vorschläge angewendet", n = done) } else { crate::trf!("{n} Vorschläge angewendet, {m} übersprungen", n = done, m = skipped.len()) };
                                    actions.push(Action::Status(keyword_status(&label, &snap)));
                                    if !snap.is_empty() {
                                        b.manage.push(Undo::Snap { label, snap, keywords: true, smart: true });
                                    }
                                    if let Some(a) = &mut b.manage.arrange {
                                        a.close_suggestions();
                                    }
                                }
                                Err(e) => b.dialog = Some(Dialog::Error(e)),
                            },
                        );
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::DeleteKeyword { id, name, keep_children, loss } => {
                let id = *id;
                let ready = matches!(loss, Some(Ok(_)));
                let a = dialog::show(ctx, "bib-oberbegriff-loeschen", &crate::trf!("„{a}“ löschen?", a = name), dialog::WIDTH, |ui| {
                    match loss {
                        None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                        Some(Err(e)) => dialog::warning(ui, e),
                        Some(Ok((all, own, children))) => {
                            let all_text = crate::trf!("Mit allen Unterbegriffen löschen ({n} Unterbegriffe; {p} verlieren Stichwörter)", n = children, p = crate::dialog::count(*all, crate::tr("Foto"), crate::tr("Fotos")));
                            let own_text = crate::trf!("Nur „{a}“ löschen; Unterbegriffe rücken eine Ebene hoch ({p} verlieren „{a}“)", a = name, p = crate::dialog::count(*own, crate::tr("Foto"), crate::tr("Fotos")));
                            ui.radio_value(keep_children, false, all_text);
                            ui.radio_value(keep_children, true, own_text);
                            let n = if *keep_children { *own } else { *all };
                            if n >= library::keywords::BACKUP_FROM {
                                dialog::hint(ui, crate::tr("Vorher wird die Bibliothek gesichert."));
                            }
                        }
                    }
                    dialog::hint(ui, crate::tr("Die Fotos bleiben in der Bibliothek. ⌘Z holt alles zurück."));
                }, Buttons::destructive(crate::tr("Löschen"), ready));
                match a {
                    Answer::Primary => {
                        let keep_kids = *keep_children;
                        let label = crate::trf!("„{a}“ gelöscht", a = name);
                        self.keyword_job(label, move |db| {
                            let (all, own, _) = library::keyword_arrange::delete_loss(db, id)?;
                            if (if keep_kids { own } else { all }) >= library::keywords::BACKUP_FROM {
                                library::backup::create_action_backup(db, "delete-keywords")?;
                            }
                            if keep_kids { library::keyword_arrange::delete_keep_children(db, id) } else { library::keyword_arrange::delete_with_children(db, id) }
                        });
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::Delete { target, name, detail } => {
                let (title, text) = match target {
                    Target::Collection(id) => {
                        let kind = self.collections.iter().find(|c| c.id == *id).map_or(0, |c| c.kind);
                        let what = match kind {
                            1 => crate::tr("Der Satz und alles darin werden gelöscht. Die Fotos bleiben in der Bibliothek, ihre Dateien unverändert."),
                            2 => crate::tr("Die Smart-Sammlung (ihre Regel) wird gelöscht. Die Fotos bleiben in der Bibliothek."),
                            _ => crate::tr("Die Sammlung wird gelöscht. Die Fotos bleiben in der Bibliothek, ihre Dateien unverändert."),
                        };
                        (crate::trf!("„{a}“ löschen?", a = name), what)
                    }
                    Target::Keyword(id) => {
                        let person = self.keywords.iter().any(|k| k.id == *id && k.person);
                        let what = if person {
                            crate::tr("Die Person wird gelöscht; ihre Gesichter bleiben, aber ohne Namen. Die Fotos bleiben in der Bibliothek.")
                        } else {
                            crate::tr("Das Stichwort und alle darunter werden gelöscht und von den Fotos genommen. Die Fotos bleiben in der Bibliothek.")
                        };
                        (crate::trf!("„{a}“ löschen?", a = name), what)
                    }
                };
                let ready = matches!(detail, Some(Ok(_)));
                let a = dialog::show(ctx, "bib-loeschen-eintrag", &title, dialog::WIDTH, |ui| {
                    dialog::text(ui, text);
                    match detail {
                        None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                        Some(Ok(t)) => dialog::chip(ui, t),
                        Some(Err(e)) => dialog::warning(ui, e),
                    }
                    dialog::hint(ui, crate::tr("⌘Z holt alles zurück."));
                }, Buttons::destructive(crate::tr("Löschen"), ready));
                match a {
                    Answer::Primary => {
                        let label = crate::trf!("„{a}“ gelöscht", a = name);
                        match *target {
                            Target::Collection(id) => {
                                let smart = self.collections.iter().any(|c| c.id == id && c.kind == 2);
                                self.collection_job(label, smart, move |db| manage::delete_collection(db, id));
                            }
                            Target::Keyword(id) => self.keyword_job(label, move |db| {
                                if manage::keyword_loss(db, id)? >= library::keywords::BACKUP_FROM {
                                    library::backup::create_action_backup(db, "delete-keywords")?;
                                }
                                library::keyword_arrange::delete_with_children(db, id)
                            }),
                        }
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
            Ask::MoveKeyword { id, filter } => {
                let me = *id;
                let name = self.keywords.iter().find(|k| k.id == me).map(|k| k.name.clone()).unwrap_or_default();
                // never below itself
                let mut below = HashSet::from([me]);
                let mut grew = true;
                while grew {
                    grew = false;
                    for k in &self.keywords {
                        if k.parent.is_some_and(|p| below.contains(&p)) && below.insert(k.id) {
                            grew = true;
                        }
                    }
                }
                let f = filter.trim().to_lowercase();
                let mut choices: Vec<(i64, String)> = self
                    .keywords
                    .iter()
                    .filter(|k| !below.contains(&k.id) && (f.is_empty() || k.name.to_lowercase().contains(&f)))
                    .map(|k| (k.id, self.kw_path(k.id).replace('|', " › ")))
                    .collect();
                choices.sort_by_key(|(_, p)| p.to_lowercase());
                choices.truncate(200);
                let mut chosen = None;
                let a = dialog::show(ctx, "bib-stichwort-verschieben", &crate::trf!("„{a}“ verschieben", a = name), dialog::WIDTH, |ui| {
                    dialog::input(ui, crate::tr("Unter das Stichwort (filtern)"), filter);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for (k, p) in &choices {
                            if lr::list_item(ui, p, false).clicked() {
                                chosen = Some(*k);
                            }
                        }
                    });
                }, Buttons::close());
                if let Some(k) = chosen {
                    self.manage.ask = None;
                    self.move_keyword(me, Some(k));
                    keep = false;
                } else if a != Answer::Open {
                    keep = false;
                }
            }
            Ask::Imports { list } => {
                let mut request: Option<(i64, String, bool)> = None;
                let a = dialog::show(ctx, "bib-importe", crate::tr("Importe"), 620.0, |ui| {
                    match list {
                        None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                        Some(Err(e)) => dialog::warning(ui, e),
                        Some(Ok(v)) if v.is_empty() => dialog::text(ui, crate::tr("Noch keine Importe.")),
                        Some(Ok(v)) => {
                            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                egui::Grid::new("bib-importe-liste").num_columns(4).spacing(egui::vec2(14.0, 6.0)).striped(true).show(ui, |ui| {
                                    for h in ["Datum", "Quelle", "Fotos", ""] {
                                        ui.label(egui::RichText::new(crate::tr(h)).small().color(lr::TEXT_DIM));
                                    }
                                    ui.end_row();
                                    for i in v.iter() {
                                        let when = import_date(i.begin);
                                        let source = if i.source.is_empty() { import_kind(&i.kind) } else { format!("{} · {}", import_kind(&i.kind), i.source) };
                                        ui.label(&when);
                                        ui.allocate_ui_with_layout(egui::vec2(330.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(egui::Label::new(&source).truncate()));
                                        ui.label(crate::library_ui::thousands(i.photos));
                                        let label = format!("{when} · {source}");
                                        ui.menu_button("…", |ui| {
                                            if lr::menu_item(ui, crate::tr("Fotos dieses Imports aus der Bibliothek entfernen …"), "", i.photos > 0) {
                                                request = Some((i.id, label.clone(), false));
                                            }
                                            ui.separator();
                                            if lr::menu_item(ui, crate::tr("Eintrag löschen …"), "", true) {
                                                request = Some((i.id, label.clone(), true));
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                            });
                        }
                    }
                }, Buttons::close());
                if let Some((run, label, entry_only)) = request {
                    self.ask_remove_import(run, label, entry_only);
                    return;
                }
                if a != Answer::Open {
                    keep = false;
                }
            }
            Ask::RemoveImport { run, label, entry_only, loss } => {
                let ready = *entry_only || matches!(loss, Some(Ok(_)));
                let title = if *entry_only { crate::tr("Eintrag des Imports löschen?") } else { crate::tr("Fotos des Imports aus der Bibliothek entfernen?") };
                let a = dialog::show(ctx, "bib-import-entfernen", title, dialog::WIDTH, |ui| {
                    dialog::chip(ui, label);
                    if *entry_only {
                        dialog::text(ui, crate::tr("Nur der Eintrag in der Liste geht. Die Fotos bleiben in der Bibliothek; sie zählen dann nicht mehr als „Zuletzt importiert“."));
                    } else {
                        dialog::text(ui, crate::tr("Die Dateien bleiben unverändert. Aus der Bibliothek verschwinden:"));
                        match loss {
                            None => dialog::pending(ui, crate::tr("wird ermittelt …")),
                            Some(Ok(l)) => loss_lines(l).iter().for_each(|t| dialog::chip(ui, t)),
                            Some(Err(e)) => dialog::warning(ui, e),
                        }
                        dialog::hint(ui, crate::tr("Der Abgleich nimmt die Fotos nicht wieder auf."));
                    }
                    dialog::hint(ui, crate::tr("⌘Z holt alles zurück."));
                }, Buttons::destructive(if *entry_only { crate::tr("Eintrag löschen") } else { crate::tr("Entfernen") }, ready));
                match a {
                    Answer::Primary => {
                        let run = *run;
                        if *entry_only {
                            self.photos_job(crate::tr("Import-Eintrag gelöscht").into(), move |db| manage::delete_import_entry(db, run));
                        } else {
                            self.photos_job(crate::tr("Fotos des Imports aus der Bibliothek entfernt").into(), move |db| manage::remove_import_photos(db, run));
                        }
                        keep = false;
                    }
                    Answer::Cancel => keep = false,
                    Answer::Open => {}
                }
            }
        }
        if keep && self.manage.ask.is_none() {
            self.manage.ask = Some(ask);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_lines_name_only_what_is_lost() {
        let l = Loss { photos: 3, copies: 0, edited: 2, rated: 0, keywords: 0, collections: 1, faces: 0 };
        let v = loss_lines(&l);
        assert_eq!(v.len(), 3, "{v:?}");
        assert!(v[0].contains('3'));
    }
}

/// Status text after a keyword change: the label, the smart collections that followed or were kept unchanged
/// (ambiguous terms), and a follow-up step that failed after the change was saved (the change stands, ⌘Z works).
pub(crate) fn keyword_status(label: &str, snap: &Snapshot) -> String {
    let mut parts = vec![label.to_string()];
    match snap.smart_changed() {
        0 => {}
        1 => parts.push(crate::tr("1 Smart-Sammlung angepasst").to_string()),
        n => parts.push(crate::trf!("{n} Smart-Sammlungen angepasst", n = n)),
    }
    if !snap.smart_kept().is_empty() {
        parts.push(crate::trf!("Smart-Sammlungen unverändert (Begriff mehrdeutig): {a}", a = snap.smart_kept().join(", ")));
    }
    if let Some(w) = snap.warnings().first() {
        parts.push(crate::trf!("Gespeichert, aber nicht alles nachgezogen: {e}", e = w));
    }
    parts.join(" · ")
}
