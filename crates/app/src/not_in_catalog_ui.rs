//! "Not in catalog" – UI: folders on disk whose photos the catalog does not know.
//!
//! - In the folder tree such folders appear dimmed at their place with "+count". A click opens them
//!   in browse mode (read only); a bar at the top offers "Add" and "Ignore".
//! - Under "Catalog" there is the "Not in catalog" overview with multi-selection.
//! - The search runs in the background (read only), every 60 seconds and after every add.
//!
//! Submodule of `library_ui` (accesses its fields).

use super::*;
use library::not_in_catalog::{self as not_in_catalog, Entry};

/// Result of the background search.
#[derive(Default)]
pub(super) struct Snapshot {
    entries: Vec<Entry>,
    searched: bool,
    error: Option<String>,
    new: bool,
    /// hidden with "Nicht mehr anzeigen" / removed from the library earlier (not offered again)
    ignored: usize,
    removed: usize,
    /// what the running search is reading (root folder · folders read)
    progress: String,
}

#[derive(Default)]
pub(super) struct NotInCatalogState {
    snapshot: Arc<Mutex<Snapshot>>,
    running: Arc<AtomicBool>,
    /// Start of the first observation and last search
    start: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
    /// State for display (copy of the search result)
    pub(super) copy: Vec<Entry>,
    searched: bool,
    error: Option<String>,
    /// checked rows of the overview (key = absolute path)
    selection: HashSet<String>,
    capture: Arc<Mutex<Option<Result<usize, String>>>>,
    capture_running: bool,
    /// search again right after adding
    rescan: bool,
    /// foreign folder last expanded in the tree (key)
    shown: Option<String>,
    ignored: usize,
    removed: usize,
}

fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| library::nfc(p.to_string_lossy().trim_end_matches('/'));
    n(a) == n(b)
}

impl LibraryView {
    /// Once per frame: trigger the search and take over the result.
    pub(super) fn update_not_in_catalog(&mut self, ctx: &egui::Context) {
        if self.db.is_none() || (crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_some() && crate::env::var_os("GRAINEO_NOT_IN_CATALOG").is_none()) {
            return;
        }
        let now = std::time::Instant::now();
        let start = *self.not_in_catalog.start.get_or_insert(now);
        // take over the search result
        let take_over = {
            let mut s = self.not_in_catalog.snapshot.lock().unwrap();
            let new = s.new;
            s.new = false;
            new.then(|| (s.entries.clone(), s.searched, s.error.clone(), s.ignored, s.removed))
        };
        if let Some((e, g, f, ignored, removed)) = take_over {
            let changed = e != self.not_in_catalog.copy;
            self.not_in_catalog.copy = e;
            self.not_in_catalog.searched = g;
            self.not_in_catalog.error = f;
            self.not_in_catalog.ignored = ignored;
            self.not_in_catalog.removed = removed;
            let visible: HashSet<String> = self.not_in_catalog.copy.iter().map(|x| x.cache_key()).collect();
            self.not_in_catalog.selection.retain(|k| visible.contains(k));
            if changed {
                self.build_tree();
            }
        }
        // every minute only while the view is open; otherwise now and then (each run reads every folder of every root
        // folder – it ran almost all the time); other drives (NAS) at the first run, on request and with the view open
        let view_open = self.source == Source::NotInCatalog;
        let due = match self.not_in_catalog.last {
            None => now.duration_since(start).as_secs_f64() > 3.0,
            Some(t) => now.duration_since(t).as_secs() >= if view_open { 60 } else { 30 * 60 },
        };
        // with the view open the user waits: do not wait for a running library job (the search only reads folder
        // lists); otherwise it waits so the disk is not read twice in the background
        if (due || self.not_in_catalog.rescan) && !self.not_in_catalog.running.load(std::sync::atomic::Ordering::Relaxed) && (view_open || !self.work.lock().unwrap().running) {
            let other_drives = view_open || self.not_in_catalog.rescan || self.not_in_catalog.last.is_none();
            self.not_in_catalog.rescan = false;
            self.search_not_in_catalog_in(ctx, other_drives);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
    }

    /// Start the search in the background; `other_drives`: also root folders on other drives (otherwise their earlier
    /// result stays).
    pub(super) fn search_not_in_catalog_in(&mut self, ctx: &egui::Context, other_drives: bool) {
        if self.not_in_catalog.running.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.not_in_catalog.last = Some(std::time::Instant::now());
        let (snapshot, running, ctx) = (self.not_in_catalog.snapshot.clone(), self.not_in_catalog.running.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("nicht-im-katalog".into())
            .spawn(move || {
                // lowest priority: efficiency cores, throttled disk and network access
                library::preview::background_priority_thread();
                let progress = |t: &str| {
                    snapshot.lock().unwrap().progress = t.to_string();
                    ctx.request_repaint();
                };
                let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| not_in_catalog::search_report(&db, &progress, other_drives));
                {
                    let mut s = snapshot.lock().unwrap();
                    s.progress.clear();
                    match r {
                        Ok(report) => {
                            let (mut e, skipped) = (report.entries, report.skipped);
                            s.ignored = report.ignored;
                            s.removed = report.removed;
                            // root folders left out this time: their earlier result stays
                            e.extend(s.entries.iter().filter(|x| skipped.contains(&x.root)).cloned());
                            e.sort_by(|a, b| a.cache_key().to_lowercase().cmp(&b.cache_key().to_lowercase()));
                            s.entries = e;
                            s.error = None;
                        }
                        Err(e) => s.error = Some(e),
                    }
                    s.searched = true;
                    s.new = true;
                }
                running.store(false, std::sync::atomic::Ordering::SeqCst);
                ctx.request_repaint();
            })
            .ok();
    }

    /// Add folders (in the background). The photos themselves stay untouched.
    pub(super) fn add_not_in_catalog(&mut self, ctx: &egui::Context, entries: Vec<Entry>) {
        if entries.is_empty() || self.not_in_catalog.capture_running {
            return;
        }
        self.not_in_catalog.capture_running = true;
        let (target, ctx) = (self.not_in_catalog.capture.clone(), ctx.clone());
        let selection: Vec<(i64, String)> = entries.iter().map(|e| (e.root, e.rel.clone())).collect();
        std::thread::Builder::new()
            .name("nik-aufnehmen".into())
            .spawn(move || {
                let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| not_in_catalog::add(&db, &selection, &|_| {}));
                *target.lock().unwrap() = Some(r);
                ctx.request_repaint();
            })
            .ok();
    }

    /// Remember folders that should no longer appear as "not in catalog".
    fn ignore_not_in_catalog(&mut self, ctx: &egui::Context, entries: &[Entry]) {
        let key: Vec<String> = entries.iter().map(|e| e.cache_key()).collect();
        if let Some(db) = &self.db {
            if let Err(e) = not_in_catalog::ignore(db, &key) {
                self.dialog = Some(Dialog::Error(crate::trf!("Ordner konnten nicht gemerkt werden: {e}", e = e)));
                return;
            }
        }
        self.not_in_catalog.copy.retain(|e| !key.contains(&e.cache_key()));
        self.not_in_catalog.selection.clear();
        self.build_tree();
        self.not_in_catalog.rescan = true;
        ctx.request_repaint();
    }

    /// After adding: message, re-read the catalog, search again.
    pub(super) fn not_in_catalog_done(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let r = self.not_in_catalog.capture.lock().unwrap().take();
        let Some(r) = r else { return };
        self.not_in_catalog.capture_running = false;
        match r {
            Ok(n) => {
                actions.push(Action::Status(crate::trf!("In die Bibliothek aufgenommen: {a}", a = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")))));
                self.not_in_catalog.selection.clear();
                self.not_in_catalog.rescan = true;
                // if the added folder was just in browse mode, now show it as a catalog folder (once it is read)
                self.reload_then(|b, _| {
                    if let Source::Browse(p) = b.source.clone() {
                        b.source = Source::All;
                        b.browse_folder(&p, None);
                    }
                });
            }
            Err(e) => {
                self.dialog = Some(Dialog::Error(crate::trf!("Aufnehmen hat nicht geklappt:\n{e}\n\nDie Fotos auf der Platte sind unverändert. Du kannst es später noch einmal versuchen.", e = e)));
            }
        }
        ctx.request_repaint();
    }

    /// Number of folders and photos not in the catalog (for the side panel).
    pub(super) fn not_in_catalog_count(&self) -> Option<(usize, usize)> {
        self.not_in_catalog.searched.then(|| (self.not_in_catalog.copy.len(), self.not_in_catalog.copy.iter().map(|e| e.count).sum()))
    }

    /// Insert foreign folders into the folder tree (dimmed, with "+count").
    pub(super) fn not_in_catalog_in_tree(&mut self) {
        if self.not_in_catalog.copy.is_empty() {
            return;
        }
        let entries = self.not_in_catalog.copy.clone();
        let mut pos: HashMap<(i64, String), usize> = self.nodes.iter().enumerate().map(|(i, k)| ((k.root, k.path.clone()), i)).collect();
        for e in &entries {
            let Some(&root) = pos.get(&(e.root, String::new())) else { continue };
            self.nodes[root].foreign += e.count;
            let mut parent = root;
            let mut path = String::new();
            for part in e.rel.split('/').filter(|t| !t.is_empty()) {
                path.push_str(part);
                path.push('/');
                let key = (e.root, path.clone());
                let i = match pos.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = self.nodes.len();
                        self.nodes.push(Node { root: e.root, path: path.clone(), name: part.to_string(), disk_only: true, ..Default::default() });
                        self.nodes[parent].children.push(i);
                        pos.insert(key, i);
                        i
                    }
                };
                self.nodes[i].foreign += e.count;
                parent = i;
            }
            self.nodes[parent].disk = Some(e.count);
        }
        let names: Vec<String> = self.nodes.iter().map(|k| k.name.to_lowercase()).collect();
        for k in &mut self.nodes {
            if k.foreign > 0 {
                k.children.sort_by(|a, b| names[*a].cmp(&names[*b]));
            }
        }
    }

    /// Entry for a folder (root, path) in the tree.
    fn not_in_catalog_entry(&self, root: i64, rel: &str) -> Option<Entry> {
        self.not_in_catalog.copy.iter().find(|e| e.root == root && e.rel == rel).cloned()
    }

    /// Click on a foreign folder in the tree: browse only.
    pub(super) fn view_not_in_catalog(&mut self, root: i64, rel: &str) {
        if let Some(e) = self.not_in_catalog_entry(root, rel) {
            self.browse_folder(&e.path(), None);
        }
    }

    /// Right-click menu of a foreign folder in the tree.
    pub(super) fn not_in_catalog_menu(&mut self, ui: &mut egui::Ui, root: i64, rel: &str) {
        let Some(e) = self.not_in_catalog_entry(root, rel) else { return };
        // frequent first, then show, hiding last
        if crate::lr::menu_item(ui, crate::tr("In die Bibliothek aufnehmen"), "", true) {
            self.add_not_in_catalog(&ui.ctx().clone(), vec![e.clone()]);
        }
        ui.separator();
        if crate::lr::menu_item(ui, crate::tr("Im Finder zeigen"), "", true) {
            crate::system::show(e.path());
        }
        ui.separator();
        if crate::lr::menu_item(ui, crate::tr("Nicht mehr anzeigen"), "", true) {
            self.ignore_not_in_catalog(&ui.ctx().clone(), std::slice::from_ref(&e));
        }
    }

    /// Bar above the grid when a folder that is not in the catalog is being browsed.
    pub(super) fn not_in_catalog_banner(&mut self, ui: &mut egui::Ui) {
        let Source::Browse(p) = &self.source else { return };
        let Some(e) = self.not_in_catalog.copy.iter().find(|e| same_path(&e.path(), p)).cloned() else { return };
        let ctx = ui.ctx().clone();
        // show the folder in the tree: expand the branch and scroll there (once per folder)
        if self.not_in_catalog.shown.as_deref() != Some(e.cache_key().as_str()) {
            self.not_in_catalog.shown = Some(e.cache_key());
            let mut path = String::new();
            for part in e.rel.split('/').filter(|t| !t.is_empty()) {
                self.open_folders.insert((e.root, path.clone()));
                path.push_str(part);
                path.push('/');
            }
            self.scroll_to_folder = Some((e.root, e.rel.clone()));
        }
        egui::Frame::new().fill(Color32::from_gray(0x24)).corner_radius(0.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::trf!("Dieser Ordner ist noch nicht in der Bibliothek · {a}", a = crate::dialog::count(e.count, crate::tr("Foto"), crate::tr("Fotos")))).color(lr::TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // not gold: the one main button of the library is "Import"
                    if ui.add_enabled(!self.not_in_catalog.capture_running, egui::Button::new(crate::tr("Aufnehmen"))).clicked() {
                        self.add_not_in_catalog(&ctx, vec![e.clone()]);
                    }
                    if ui.add_enabled(!self.not_in_catalog.capture_running, egui::Button::new(crate::tr("Nicht mehr anzeigen"))).clicked() {
                        self.ignore_not_in_catalog(&ctx, std::slice::from_ref(&e));
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    /// "Not in catalog" overview (replaces the grid).
    ///
    /// Layout: title, one calm summary line, the action bar, then the list grouped by reason (the reason is the group
    /// header, not repeated per row). Every row has a fixed height: checkbox | folder name over its dimmed parent path
    /// (shortened in the middle, full path as tooltip) | photos and size in right-aligned columns | "Import …".
    pub(super) fn not_in_catalog_center(&mut self, ui: &mut egui::Ui) {
        use not_in_catalog::Reason;
        let ctx = ui.ctx().clone();
        let entries = self.not_in_catalog.copy.clone();
        let photos: usize = entries.iter().map(|e| e.count).sum();
        let bytes: u64 = entries.iter().map(|e| e.bytes).sum();
        let running = self.not_in_catalog.running.load(std::sync::atomic::Ordering::Relaxed);
        // Never "0 photos" without a word – searching, waiting for a sync, or the result
        let waiting = !running && !self.not_in_catalog.searched;
        let progress = self.not_in_catalog.snapshot.lock().unwrap().progress.clone();
        let small = egui::TextStyle::Small.resolve(ui.style());
        // the visible width at the start: wide content must never widen the view (it ran under the right panel)
        let full_w = ui.available_width();
        ui.set_max_width(full_w);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::tr("NICHT IN DER BIBLIOTHEK")).color(lr::TEXT_DIM()).size(11.0));
            crate::help::info(ui, "nicht-in-bibliothek", "Nicht in der Bibliothek", &crate::help::NOT_IN_LIBRARY);
        });
        ui.add_space(6.0);
        // summary: one calm line ("13 Ordner · 41 Fotos · 1,2 GB"), the explanation dimmed below it
        let (header, detail) = if let Some(f) = &self.not_in_catalog.error {
            (crate::trf!("Suche nicht möglich: {f}", f = f), String::new())
        } else if !self.not_in_catalog.searched {
            let t = if progress.is_empty() { crate::tr("Stammordner werden durchsucht …").to_string() } else { crate::trf!("Stammordner werden durchsucht … {a}", a = progress_text(&progress)) };
            (t, String::new())
        } else if entries.is_empty() {
            (crate::tr("Alles, was in deinen Stammordnern liegt, ist in der Bibliothek.").to_string(), String::new())
        } else {
            (
                format!("{} · {} · {}", folder_count(entries.len()), crate::dialog::count(photos, crate::tr("Foto"), crate::tr("Fotos")), crate::dialog::size(bytes)),
                crate::tr("liegen in deinen Stammordnern, sind aber nicht in der Bibliothek.").to_string(),
            )
        };
        ui.horizontal(|ui| {
            if running || waiting {
                ui.add(egui::Spinner::new().size(12.0));
            }
            ui.add(egui::Label::new(egui::RichText::new(header).font(lr::semibold(lr::UI_SIZE)).color(lr::TEXT())).truncate());
        });
        // second line: explanation, refresh state, hidden folders – always one line (no jump when a search starts)
        let mut info = Vec::new();
        if !detail.is_empty() {
            info.push(detail);
        }
        if self.not_in_catalog.searched && running {
            info.push(if progress.is_empty() { crate::tr("wird aktualisiert …").to_string() } else { crate::trf!("wird aktualisiert … {a}", a = progress_text(&progress)) });
        }
        let (ignored, removed) = (self.not_in_catalog.ignored, self.not_in_catalog.removed);
        if ignored > 0 {
            info.push(crate::trf!("{a} ausgeblendet („Nicht mehr anzeigen“)", a = folder_count(ignored)));
        }
        if removed > 0 {
            info.push(crate::trf!("{a} früher aus der Bibliothek entfernt (wird nicht wieder angeboten)", a = folder_count(removed)));
        }
        let info = info.join(" · ");
        let r = ui.add(egui::Label::new(egui::RichText::new(if info.is_empty() { " ".to_string() } else { info.clone() }).small().color(lr::TEXT_DIM())).truncate());
        if !info.is_empty() {
            lr::hover_tip(ui, &r, &info);
        }
        ui.add_space(10.0);
        // action bar: select, add, hide – then (after a gap) search again and show hidden ones
        let selected: Vec<Entry> = entries.iter().filter(|e| self.not_in_catalog.selection.contains(&e.cache_key())).cloned().collect();
        let busy = self.not_in_catalog.capture_running;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            // fixed width for both labels, so the bar does not jump when the selection changes
            let all = !entries.is_empty() && selected.len() == entries.len();
            let (t_all, t_none) = (crate::tr("Alle auswählen"), crate::tr("Auswahl aufheben"));
            let pad = ui.spacing().button_padding.x * 2.0;
            let w = [t_all, t_none].iter().map(|t| ui.painter().layout_no_wrap(t.to_string(), egui::TextStyle::Button.resolve(ui.style()), lr::TEXT()).size().x).fold(0.0, f32::max) + pad;
            if ui.add_enabled(!entries.is_empty(), egui::Button::new(if all { t_none } else { t_all }).min_size(egui::vec2(w, 0.0))).clicked() {
                if all {
                    self.not_in_catalog.selection.clear();
                } else {
                    self.not_in_catalog.selection = entries.iter().map(|e| e.cache_key()).collect();
                }
            }
            // not gold: the one main button of the library is "Import" on the left
            let r = ui.add_enabled(!selected.is_empty() && !busy, egui::Button::new(crate::tr("Zur Bibliothek hinzufügen")));
            lr::hover_tip(ui, &r, crate::tr("Ausgewählte Ordner in die Bibliothek aufnehmen; die Fotos bleiben, wo sie sind"));
            if r.clicked() {
                self.add_not_in_catalog(&ctx, selected.clone());
            }
            let r = ui.add_enabled(!selected.is_empty() && !busy, egui::Button::new(crate::tr("Nicht mehr anzeigen")));
            lr::hover_tip(ui, &r, crate::tr("Ausgewählte Ordner aus dieser Liste ausblenden"));
            if r.clicked() {
                self.ignore_not_in_catalog(&ctx, &selected);
            }
            ui.add_space(14.0);
            if ui.add_enabled(!running, egui::Button::new(crate::tr("Neu suchen"))).clicked() {
                self.not_in_catalog.rescan = true;
            }
            let r = ui.add_enabled(ignored > 0, egui::Button::new(crate::tr("Ausgeblendete wieder zeigen")));
            lr::hover_tip(ui, &r, crate::tr("Mit „Nicht mehr anzeigen“ ausgeblendete Ordner wieder in der Liste zeigen"));
            if r.clicked() {
                if let Some(db) = &self.db {
                    let _ = not_in_catalog::clear_ignored(db);
                }
                self.not_in_catalog.rescan = true;
            }
        });
        // selection and adding state on a line of its own (always there: no jump)
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.set_min_height(14.0);
            if busy {
                ui.add(egui::Spinner::new().size(11.0));
                ui.label(egui::RichText::new(crate::tr("wird aufgenommen …")).small().color(lr::TEXT_DIM()));
            } else if !selected.is_empty() {
                let n: usize = selected.iter().map(|e| e.count).sum();
                ui.label(egui::RichText::new(crate::trf!("{a} ausgewählt · {b}", a = folder_count(selected.len()), b = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")))).small().color(lr::TEXT_DIM()));
            }
        });

        // ---- list: group header per reason, then the rows ----
        enum Item<'a> {
            /// reason title; `bool`: a group above it (more air on top)
            Group(String, bool),
            Row(&'a Entry),
        }
        let mut items: Vec<Item> = Vec::new();
        for (reason, title) in [
            (Reason::LightroomRoot, crate::tr("Lightroom kannte diese Ordner nicht")),
            (Reason::NewInOwnRoot, crate::tr("Neu in eigenen Stammordnern")),
        ] {
            let group: Vec<&Entry> = entries.iter().filter(|e| e.reason == reason).collect();
            if group.is_empty() {
                continue;
            }
            items.push(Item::Group(title.to_string(), !items.is_empty()));
            items.extend(group.into_iter().map(Item::Row));
        }
        const ROW: f32 = 50.0;
        const GROUP: f32 = 26.0;
        // "Import …" button as wide as its label (English and German differ a lot)
        let import_label = crate::tr("Importieren …");
        let button_w = (ui.painter().layout_no_wrap(import_label.to_string(), egui::TextStyle::Button.resolve(ui.style()), lr::TEXT()).size().x + ui.spacing().button_padding.x * 2.0 + 8.0).ceil().max(80.0);
        let height = |i: &Item| match i {
            Item::Group(_, later) => GROUP + if *later { 18.0 } else { 0.0 },
            Item::Row(_) => ROW,
        };
        let total: f32 = items.iter().map(height).sum();
        // column widths from the widest value, so the numbers line up
        let photos_text = |e: &Entry| crate::dialog::count(e.count, crate::tr("Foto"), crate::tr("Fotos"));
        let measure = |t: String| ui.painter().layout_no_wrap(t, small.clone(), lr::TEXT()).size().x;
        let col_photos = entries.iter().map(|e| measure(photos_text(e))).fold(measure(crate::tr("Fotos").to_uppercase()) + 6.0, f32::max).ceil();
        let col_size = entries.iter().map(|e| measure(crate::dialog::size(e.bytes))).fold(measure(crate::tr("Größe").to_uppercase()) + 6.0, f32::max).ceil();
        let body = egui::TextStyle::Body.resolve(ui.style());
        let name_font = lr::semibold(lr::UI_SIZE);
        let mut clicked_entry: Option<Entry> = None;
        let mut import_entry: Option<Entry> = None;
        let mut add_entry: Option<Entry> = None;
        let mut hide_entry: Option<Entry> = None;
        let mut toggled: Option<String> = None;
        egui::ScrollArea::vertical().auto_shrink([false; 2]).max_height((ui.available_height() - 24.0).max(60.0)).show_viewport(ui, |ui, viewport| {
            let width = ui.available_width().min(full_w - 12.0);
            let (all_rect, _) = ui.allocate_exact_size(egui::vec2(width, total), egui::Sense::hover());
            // columns from the right: [photos] [size] [Import …]; narrow windows drop the size column
            let pad_x = 10.0;
            let button_left = all_rect.right() - pad_x - button_w;
            // the size column only when the name keeps enough room
            let wide = button_left - all_rect.left() - col_photos - col_size - 36.0 - 18.0 - 36.0 >= 240.0;
            let size_right = button_left - 18.0;
            let photos_right = if wide { size_right - col_size - 18.0 } else { size_right };
            let name_right = photos_right - col_photos - 18.0;
            let name_left = all_rect.left() + pad_x + 14.0 + 12.0;
            let mut y = all_rect.top();
            for item in &items {
                let h = height(item);
                let rect = egui::Rect::from_min_size(egui::pos2(all_rect.left(), y), egui::vec2(width, h));
                y += h;
                if rect.bottom() < all_rect.top() + viewport.top() - 1.0 || rect.top() > all_rect.top() + viewport.bottom() + 1.0 {
                    continue;
                }
                let p = ui.painter().clone();
                match item {
                    Item::Group(title, _) => {
                        // reason as group header, with the column labels on the right
                        let base = rect.bottom() - 7.0;
                        // shortened with "…" at the end when the column is narrow (never cut off)
                        let room = name_right - rect.left() - pad_x - 8.0;
                        let mut t: Vec<char> = title.chars().collect();
                        let mut g = lr::caps_spaced(&p, title, small.clone(), lr::TEXT_DIM(), 0.6);
                        while g.size().x > room && t.len() > 1 {
                            t.pop();
                            let s = format!("{}…", t.iter().collect::<String>().trim_end());
                            g = lr::caps_spaced(&p, &s, small.clone(), lr::TEXT_DIM(), 0.6);
                        }
                        p.galley(egui::pos2(rect.left() + pad_x, base - g.size().y), g, lr::TEXT_DIM());
                        let label = |text: &str, right: f32| {
                            let g = lr::caps_spaced(&p, text, small.clone(), lr::TEXT_DIM(), 0.6);
                            p.galley(egui::pos2(right - g.size().x, base - g.size().y), g, lr::TEXT_DIM());
                        };
                        label("Fotos", photos_right);
                        if wide {
                            label("Größe", size_right);
                        }
                        p.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, Color32::from_gray(0x33)));
                    }
                    Item::Row(e) => {
                        let e: &Entry = e;
                        let key = e.cache_key();
                        let checked = self.not_in_catalog.selection.contains(&key);
                        let resp = ui.interact(rect, ui.scope_id().with(("nik-row", &key)), egui::Sense::click());
                        if checked {
                            p.rect_filled(rect, 0.0, Color32::from_gray(0x22));
                        } else if resp.hovered() {
                            p.rect_filled(rect, 0.0, Color32::from_gray(0x1c));
                        }
                        p.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, Color32::from_gray(0x26)));
                        let mid = rect.center().y;
                        let (line1, line2) = (mid - 8.0, mid + 9.0);
                        // checkbox, vertically centred
                        let check_box = egui::Rect::from_center_size(egui::pos2(rect.left() + pad_x + 7.0, mid), egui::vec2(14.0, 14.0));
                        p.rect_stroke(check_box, 2.0, egui::Stroke::new(1.0, Color32::from_gray(if checked { 0xc8 } else { 0x7a })), egui::StrokeKind::Inside);
                        if checked {
                            p.rect_filled(check_box.shrink(3.0), 1.0, Color32::from_gray(0xe0));
                        }
                        // folder name (bright) over the parent path (dimmed), both shortened in the middle
                        let parts: Vec<&str> = e.rel.trim_end_matches('/').split('/').filter(|t| !t.is_empty()).collect();
                        let name = parts.last().copied().unwrap_or("");
                        let root = e.root_path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                        let parent = std::iter::once(root).chain(parts[..parts.len().saturating_sub(1)].iter().copied()).collect::<Vec<_>>().join(" › ");
                        let name_w = (name_right - name_left).max(40.0);
                        let name = lr::middle_ellipsis_font(&p, name, &name_font, name_w);
                        p.text(egui::pos2(name_left, line1), egui::Align2::LEFT_CENTER, name, name_font.clone(), lr::TEXT());
                        // second line: parent path, then the hint from the name and probable duplicates
                        // the hints stay whole as long as possible; the path gives way first
                        let extra = row_hints(e);
                        let second = if extra.is_empty() {
                            lr::middle_ellipsis_font(&p, &parent, &small, name_w)
                        } else {
                            let extra = format!(" · {extra}");
                            let width = |t: &str| p.layout_no_wrap(t.to_string(), small.clone(), lr::TEXT()).size().x;
                            // the path keeps at least half the room, the hints get the rest
                            let path = lr::middle_ellipsis_font(&p, &parent, &small, (name_w - width(&extra)).max(name_w * 0.5));
                            let extra = lr::middle_ellipsis_font(&p, &extra, &small, name_w - width(&path));
                            format!("{path}{extra}")
                        };
                        p.text(egui::pos2(name_left, line2), egui::Align2::LEFT_CENTER, second, small.clone(), lr::TEXT_DIM());
                        // numbers: photos (bright) with the formats below, size
                        p.text(egui::pos2(photos_right, line1), egui::Align2::RIGHT_CENTER, photos_text(e), body.clone(), lr::TEXT());
                        if wide {
                            p.text(egui::pos2(size_right, line1), egui::Align2::RIGHT_CENTER, crate::dialog::size(e.bytes), body.clone(), lr::TEXT());
                        }
                        let formats = e.formats.iter().take(2).map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(" · ");
                        let right_cluster = if wide { size_right } else { photos_right };
                        let fclip = p.with_clip_rect(egui::Rect::from_min_max(egui::pos2(name_right + 6.0, rect.top()), egui::pos2(right_cluster, rect.bottom())));
                        fclip.text(egui::pos2(right_cluster, line2), egui::Align2::RIGHT_CENTER, formats, small.clone(), lr::TEXT_DIM());
                        // full path on hover (the text may be shortened)
                        lr::hover_tip(ui, &resp, &e.path().display().to_string());
                        // not gold: the one main button of the library is "Import …" on the left
                        let button = egui::Rect::from_min_size(egui::pos2(button_left, mid - 11.0), egui::vec2(button_w, 22.0));
                        let b = ui.put(button, egui::Button::new(import_label));
                        lr::hover_tip(ui, &b, &crate::trf!("Import-Fenster mit diesem Ordner als Quelle öffnen ({a})", a = e.path().display().to_string()));
                        if b.clicked() {
                            import_entry = Some(e.clone());
                        }
                        if resp.clicked() {
                            // checkbox or row: toggle; double click: browse the folder
                            toggled = Some(key.clone());
                        }
                        if resp.double_clicked() {
                            clicked_entry = Some(e.clone());
                        }
                        resp.context_menu(|ui| {
                            if crate::lr::menu_item(ui, crate::tr("Ordner ansehen"), "", true) {
                                clicked_entry = Some(e.clone());
                            }
                            if crate::lr::menu_item(ui, crate::tr("In die Bibliothek aufnehmen"), "", true) {
                                add_entry = Some(e.clone());
                            }
                            ui.separator();
                            if crate::lr::menu_item(ui, crate::tr("Im Finder zeigen"), "", true) {
                                crate::system::show(e.path());
                            }
                            ui.separator();
                            if crate::lr::menu_item(ui, crate::tr("Nicht mehr anzeigen"), "", true) {
                                hide_entry = Some(e.clone());
                            }
                        });
                    }
                }
            }
        });
        if let Some(k) = toggled {
            if !self.not_in_catalog.selection.remove(&k) {
                self.not_in_catalog.selection.insert(k);
            }
        }
        if let Some(e) = add_entry {
            self.add_not_in_catalog(&ctx, vec![e]);
        }
        if let Some(e) = hide_entry {
            self.ignore_not_in_catalog(&ctx, std::slice::from_ref(&e));
        }
        if let Some(e) = clicked_entry {
            self.browse_folder(&e.path(), None);
        }
        if let Some(e) = import_entry {
            self.import_folder(&ctx, e.path());
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(crate::tr("Doppelklick auf eine Zeile zeigt den Ordner.")).small().color(lr::TEXT_DIM()));
    }

    /// Text for the filmstrip header while the overview is shown (instead of "0 Bilder").
    pub fn not_in_catalog_strip(&self) -> Option<String> {
        if self.source != Source::NotInCatalog {
            return None;
        }
        Some(match self.not_in_catalog_count() {
            Some((folders, photos)) => crate::trf!(
                "Nicht in der Bibliothek · {a} mit {b}",
                a = folder_count(folders),
                b = crate::dialog::count(photos, crate::tr("Foto"), crate::tr("Fotos"))
            ),
            None => crate::tr("Nicht in der Bibliothek · wird gesucht …").to_string(),
        })
    }
}

/// "1 folder" / "26 folders" (the word "Ordner" alone is the same in German singular and plural).
fn folder_count(n: usize) -> String {
    if n == 1 { crate::tr("1 Ordner").to_string() } else { crate::trf!("{n} Ordner", n = crate::dialog::number(n)) }
}

/// The search's current step for display: "root · folders read · photos found" (see
/// `not_in_catalog::search_report`); anything else is shown as it is.
fn progress_text(p: &str) -> String {
    let mut it = p.rsplitn(3, " · ");
    let (Some(found), Some(read), Some(root)) = (it.next(), it.next(), it.next()) else { return p.to_string() };
    let (Ok(found), Ok(read)) = (found.parse::<usize>(), read.parse::<usize>()) else { return p.to_string() };
    crate::trf!("{root}: {a} Ordner geprüft, {b} Fotos gefunden", root = root, a = crate::dialog::number(read), b = crate::dialog::number(found))
}

/// Extra notes of a row after the parent path: hint from the name, probable duplicates (the reason itself is the
/// group header).
fn row_hints(e: &Entry) -> String {
    use not_in_catalog::Hint;
    let mut v: Vec<String> = Vec::new();
    if let Some(h) = e.hint {
        v.push(
            match h {
                Hint::Trash => crate::tr("Name klingt nach Papierkorb"),
                Hint::Export => crate::tr("Name klingt nach Export"),
                Hint::Backup => crate::tr("Name klingt nach Sicherung oder Kopie"),
                Hint::PhotoBooth => crate::tr("Photo-Booth-Bibliothek"),
                Hint::Picasa => crate::tr("Picasa-Ordner"),
            }
            .to_string(),
        );
    }
    if e.known > 0 {
        v.push(crate::trf!("{a} vermutlich schon in der Bibliothek (gleicher Name und Größe)", a = thousands(e.known)));
    }
    v.join(" · ")
}

impl LibraryView {
    /// Is the centre currently showing "Not in catalog" (there are no photos there that Del could delete)?
    pub fn not_in_catalog_view(&self) -> bool {
        self.source == Source::NotInCatalog
    }
}

impl NotInCatalogState {
    /// Search fresh right away when the overview opens.
    pub(super) fn trigger_research(&mut self) {
        self.rescan = true;
    }
}
