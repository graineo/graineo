//! "Ordner ansehen" (⌘O): the section "ANGESEHEN" at the top of the left panel. The root is the chosen folder,
//! below it a folder tree that reads one level when a folder is opened (no depth limit; hidden folders and packages
//! are left out, see `view_folder::children`). A click on a folder shows its photos; the switch "Unterordner
//! einbeziehen" (default on) decides whether the photos of its subfolders are shown too. The section stays while the
//! folder is viewed, also when another source of the library is chosen; it goes with "Schließen".
//!
//! Refreshing (button, ⌥⌘R, and quietly when the window gets the focus back, at most every few seconds) reads the
//! shown folder and the open tree nodes again in a background thread; there is no file system watching and no
//! polling. New photos appear, removed ones go, selection and active photo stay where possible.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32};
use library::db::Row;

use super::{view_id, Action, Filter, LibraryView, Source, VIEW_FOLDER};
use crate::lr;
use crate::view_folder::{self, Child, Listing};

/// Setting: include the photos of subfolders (default on: the root shows everything, as before).
pub const SUBFOLDERS_KEY: &str = "viewFolderSubfolders";

/// Quiet look at the folder when the window gets the focus back: not more often than this.
pub const FOCUS_CHECK_EVERY: Duration = Duration::from_secs(5);

pub fn include_subfolders() -> bool {
    library::settings::value(SUBFOLDERS_KEY).as_bool().unwrap_or(true)
}

/// `a` + `/` + `b` for relative tree paths ("" is the root).
pub fn rel_join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}

/// Folder of a relative tree path (component by component, so Windows paths get their own separators).
pub fn dir_of(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|t| !t.is_empty()).fold(root.to_path_buf(), |p, t| p.join(t))
}

/// What a background refresh found.
pub(super) struct Refreshed {
    root: PathBuf,
    sel: String,
    recursive: bool,
    manual: bool,
    listing: Listing,
    /// rows for the photos not shown before
    new_rows: Vec<Row>,
    /// subfolders of the open nodes, read again
    children: Vec<(String, Option<Vec<Child>>)>,
}

/// State of the viewed folder's tree.
pub(super) struct ViewTree {
    pub root: PathBuf,
    /// shown folder, relative to the root ("" = the root)
    pub sel: String,
    /// open nodes (the root is open from the start)
    pub open: HashSet<String>,
    /// subfolders of the nodes read so far (`None`: not readable)
    pub children: HashMap<String, Option<Vec<Child>>>,
    loading: HashSet<String>,
    tx: Sender<(String, Option<Vec<Child>>)>,
    rx: Receiver<(String, Option<Vec<Child>>)>,
    /// photos of the root (last time it was listed) and whether that hit the limit
    pub root_count: (usize, bool),
    refresh: Option<Receiver<Refreshed>>,
    /// manual refresh asked while one runs: say the result
    refresh_manual: bool,
    pub last_check: Instant,
    focused: bool,
    /// IDs of the photos seen in this folder (path → file ID): a photo keeps its ID when another folder of the tree
    /// is chosen and back (active photo, previews, loaded settings stay)
    ids: HashMap<PathBuf, i64>,
}

impl ViewTree {
    pub fn new(root: PathBuf) -> Self {
        let (tx, rx) = channel();
        ViewTree {
            root,
            sel: String::new(),
            open: HashSet::from([String::new()]),
            children: HashMap::new(),
            loading: HashSet::new(),
            tx,
            rx,
            root_count: (0, false),
            refresh: None,
            refresh_manual: false,
            last_check: Instant::now(),
            focused: true,
            ids: HashMap::new(),
        }
    }

    /// The folder shown now.
    pub fn shown_dir(&self) -> PathBuf {
        dir_of(&self.root, &self.sel)
    }

    /// Read the subfolders of `rel` (once; in the background unless `now`).
    fn want_children(&mut self, rel: &str, now: bool, ctx: Option<&egui::Context>) {
        if self.children.contains_key(rel) || self.loading.contains(rel) {
            return;
        }
        let dir = dir_of(&self.root, rel);
        if now {
            self.children.insert(rel.to_string(), view_folder::children(&dir));
            return;
        }
        self.loading.insert(rel.to_string());
        let (tx, rel, ctx) = (self.tx.clone(), rel.to_string(), ctx.cloned());
        let _ = std::thread::Builder::new().name("ansehen-baum".into()).spawn(move || {
            let _ = tx.send((rel, view_folder::children(&dir)));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    fn take_children(&mut self) {
        while let Ok((rel, c)) = self.rx.try_recv() {
            self.loading.remove(&rel);
            self.children.insert(rel, c);
        }
    }
}

/// Rows for photos of a viewed folder (rating and virtual copies from the sidecar, read only). `ids`: known file IDs
/// by path (a new ID otherwise).
pub(super) fn viewed_rows(dir: &Path, names: &[String], ids: &HashMap<PathBuf, i64>) -> Vec<Row> {
    let raw_stems: HashSet<String> = names.iter().filter(|n| library::format_from_extension(n).is_some_and(library::is_raw)).filter_map(|n| n.rsplit_once('.').map(|(s, _)| s.to_lowercase())).collect();
    let mut rows: Vec<Row> = Vec::new();
    for n in names {
        let Some(f) = library::format_from_extension(n) else { continue };
        let path = dir.join(n);
        let raw_beside = !library::is_raw(f) && n.rsplit_once('.').is_some_and(|(s, _)| raw_stems.contains(&s.to_lowercase()));
        let values = match library::sidecar::read(&path, f, raw_beside).0 {
            library::xmp::ReadResult::Ok(w) => w,
            _ => library::xmp::Values::default(),
        };
        let m = std::fs::metadata(&path).ok();
        let basis = Row {
            image: 0,
            file: 0,
            folder: VIEW_FOLDER,
            name: n.clone(),
            format: f.to_string(),
            copy: None,
            stars: values.stars,
            flag: values.flag,
            label: values.label,
            status: 0,
            camera: None,
            capture: None,
            run: None,
            size: m.as_ref().map(|m| m.len() as i64),
            mtime: m.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64),
            width: None,
            height: None,
            orientation: None,
            lens: None,
            aperture: None,
            focal: None,
            iso: None,
            time: None,
            ai_quality: None,
            changed: None,
            imported: None,
            from_lightroom: false,
            sidecar_jpeg: None,
            jpeg_for: None,
            place: None,
        };
        let file = ids.get(&path).copied().unwrap_or_else(view_id);
        rows.push(Row { image: file, file, ..basis.clone() });
        for k in library::sidecar::read_copies(values.copies_json.as_deref()) {
            let id = view_id();
            rows.push(Row { image: id, file, copy: Some(k.name), stars: k.stars, flag: k.flag, label: k.label, ..basis.clone() });
        }
    }
    rows
}

/// Status text after a refresh (`None`: nothing to say).
pub fn refresh_text(new: usize, gone: usize, manual: bool) -> Option<String> {
    match (new, gone) {
        (0, 0) if manual => Some(crate::tr("Ordner aktualisiert – keine Änderung").to_string()),
        (0, 0) => None,
        (n, 0) => Some(crate::trf!("{a} neu", a = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")))),
        (0, g) => Some(crate::trf!("{a} entfernt", a = crate::dialog::count(g, crate::tr("Foto"), crate::tr("Fotos")))),
        (n, g) => Some(crate::trf!("{a} neu, {b} entfernt", a = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")), b = g)),
    }
}

impl LibraryView {
    /// Start viewing `dir` (⌘O, command line, "graineo <file>"): new tree, the root shown.
    pub(super) fn view_open(&mut self, dir: &Path) {
        // a newly opened folder starts with the default order again: no reading of all headers for it
        self.view_sort_chosen = false;
        let mut tree = ViewTree::new(dir.to_path_buf());
        tree.want_children("", self.loader.foreground, self.quick_ctx.as_ref());
        self.view_tree = Some(tree);
        self.view_list();
        self.source = Source::All; // forces the switch below
        self.set_source(Source::Browse(dir.to_path_buf()));
        // search and column filters apply to the library: in a foreign folder they would
        // silently hide everything (no keywords, no capture dates in the database)
        if self.filter != Filter::default() || self.facets.as_ref().is_some_and(|f| f.active()) {
            crate::log("view folder: filter reset");
            self.filter = Filter::default();
            if let Some(f) = &mut self.facets {
                f.reset();
            }
        }
        self.refilter = true;
        let pending = TEST_SELECT.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(rel) = pending {
            self.view_open_down(&rel);
        }
    }

    /// List the shown folder of the tree (in the foreground, like ⌘O always did) and take its rows.
    pub(super) fn view_list(&mut self) {
        let Some(tree) = &self.view_tree else { return };
        let dir = tree.shown_dir();
        let listing = view_folder::list(&dir, include_subfolders());
        let mut rows = viewed_rows(&dir, &listing.names, &tree.ids);
        library::db::mark_pairs(&mut rows);
        if let Some(tree) = &mut self.view_tree {
            for z in rows.iter().filter(|z| z.copy.is_none()) {
                tree.ids.insert(dir.join(&z.name), z.file);
            }
            if tree.sel.is_empty() {
                tree.root_count = (listing.names.len(), listing.truncated);
            }
            tree.last_check = Instant::now();
            tree.refresh = None;
        }
        self.viewed_listing = Some(listing);
        self.insert_viewed(&dir, rows);
    }

    /// A folder of the tree clicked: show its photos (the library source becomes the viewed folder again).
    pub(super) fn view_select(&mut self, rel: &str) {
        let Some(tree) = &mut self.view_tree else { return };
        tree.sel = rel.to_string();
        let root = tree.root.clone();
        self.view_list();
        // to the top of the new list, like any other source
        self.source = Source::All;
        self.set_source(Source::Browse(root));
        self.refilter = true;
        self.apply_filter();
        self.view_activate_first();
    }

    /// Test aid (`GRAINEO_LIBRARY_TEST="view=A/B"`): open the tree down to `rel` (read in the foreground) and show it.
    pub(super) fn view_test_select(&mut self, rel: &str) {
        // the hook can run before the folder from the command line is opened (again): then also right after that
        *TEST_SELECT.lock().unwrap_or_else(|e| e.into_inner()) = Some(rel.to_string());
        self.view_open_down(rel);
    }

    fn view_open_down(&mut self, rel: &str) {
        let Some(tree) = &mut self.view_tree else { return };
        let mut part = String::new();
        for t in std::iter::once("").chain(rel.split('/').filter(|t| !t.is_empty())) {
            part = rel_join(&part, t);
            tree.want_children(&part, true, None);
            tree.open.insert(part.clone());
        }
        self.view_select(rel);
    }

    /// "Unterordner einbeziehen" switched: list the shown folder again.
    pub(super) fn view_set_subfolders(&mut self, on: bool) {
        library::settings::set(SUBFOLDERS_KEY, serde_json::json!(on));
        if self.view_tree.is_some() {
            self.view_list();
            self.refilter = true;
            self.apply_filter();
            self.view_activate_first();
        }
    }

    /// The active photo is not in the list (any more): the first one becomes active.
    fn view_activate_first(&mut self) {
        if !self.active.is_some_and(|a| self.visible.iter().any(|&i| self.rows[i].image == a)) {
            if let Some(&i) = self.visible.first() {
                let b = self.rows[i].image;
                self.set_active(b);
            }
        }
    }

    /// Read the shown folder and the open tree nodes again (button, ⌥⌘R, focus back). In a background thread; the
    /// result is taken in [`Self::view_poll`]. `manual`: say the result also when nothing changed.
    pub fn refresh_viewed(&mut self, manual: bool) {
        let foreground = self.loader.foreground;
        let ctx = self.quick_ctx.clone();
        let Some(tree) = &mut self.view_tree else { return };
        tree.last_check = Instant::now();
        tree.refresh_manual |= manual;
        if tree.refresh.is_some() {
            return;
        }
        let (root, sel, recursive) = (tree.root.clone(), tree.sel.clone(), include_subfolders());
        let known: HashSet<String> = self.viewed.as_ref().map(|(_, z)| z.iter().filter(|z| z.copy.is_none()).map(|z| z.name.clone()).collect()).unwrap_or_default();
        let open: Vec<String> = tree.open.iter().filter(|r| tree.children.contains_key(*r)).cloned().collect();
        let job = move || {
            let dir = dir_of(&root, &sel);
            let listing = view_folder::list(&dir, recursive);
            let fresh: Vec<String> = listing.names.iter().filter(|n| !known.contains(*n)).cloned().collect();
            let new_rows = viewed_rows(&dir, &fresh, &HashMap::new());
            let children = open.into_iter().map(|r| {
                let c = view_folder::children(&dir_of(&root, &r));
                (r, c)
            }).collect();
            Refreshed { root, sel, recursive, manual, listing, new_rows, children }
        };
        let (tx, rx) = channel();
        if foreground {
            let _ = tx.send(job());
        } else {
            let _ = std::thread::Builder::new().name("ansehen-aktualisieren".into()).spawn(move || {
                let _ = tx.send(job());
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            });
        }
        tree.refresh = Some(rx);
    }

    /// Every frame while a folder is viewed: take loaded tree levels and a finished refresh; look again quietly when
    /// the window gets the focus back.
    pub(super) fn view_poll(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(tree) = &mut self.view_tree else { return };
        tree.take_children();
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let back = focused && !tree.focused;
        tree.focused = focused;
        if back && tree.refresh.is_none() && tree.last_check.elapsed() >= FOCUS_CHECK_EVERY {
            self.refresh_viewed(false);
        } else if test_refresh_due() {
            self.refresh_viewed(true);
        }
        let Some(tree) = &mut self.view_tree else { return };
        let Some(rx) = &tree.refresh else { return };
        let Ok(r) = rx.try_recv() else { return };
        tree.refresh = None;
        let manual = std::mem::take(&mut tree.refresh_manual) || r.manual;
        if let Some(t) = self.view_take_refresh(r, manual) {
            actions.push(Action::Status(t));
        }
    }

    /// Take a finished refresh: new photos in, removed ones out, the rest (IDs, ratings shown) unchanged. Returns the
    /// status text.
    pub(super) fn view_take_refresh(&mut self, r: Refreshed, manual: bool) -> Option<String> {
        let tree = self.view_tree.as_mut()?;
        if r.root != tree.root {
            return None;
        }
        for (rel, c) in r.children {
            tree.children.insert(rel, c);
        }
        // another folder or the switch changed meanwhile: that listing is newer
        if r.sel != tree.sel || r.recursive != include_subfolders() {
            return None;
        }
        let dir = tree.shown_dir();
        if tree.sel.is_empty() {
            tree.root_count = (r.listing.names.len(), r.listing.truncated);
        }
        let keep: HashSet<&str> = r.listing.names.iter().map(String::as_str).collect();
        let mut rows: Vec<Row> = self.viewed.as_ref().map(|(_, z)| z.clone()).unwrap_or_default();
        let before: HashSet<String> = rows.iter().filter(|z| z.copy.is_none()).map(|z| z.name.clone()).collect();
        let gone = before.iter().filter(|n| !keep.contains(n.as_str())).count();
        let new = r.new_rows.iter().filter(|z| z.copy.is_none() && !before.contains(&z.name)).count();
        if gone == 0 && new == 0 {
            self.viewed_listing = Some(r.listing);
            return refresh_text(0, 0, manual);
        }
        rows.retain(|z| keep.contains(z.name.as_str()));
        rows.extend(r.new_rows.into_iter().filter(|z| !before.contains(&z.name)));
        for z in rows.iter_mut() {
            (z.sidecar_jpeg, z.jpeg_for) = (None, None);
        }
        library::db::mark_pairs(&mut rows);
        for z in rows.iter().filter(|z| z.copy.is_none()) {
            tree.ids.insert(dir.join(&z.name), z.file);
        }
        self.viewed_listing = Some(r.listing);
        self.insert_viewed(&dir, rows);
        let ids: HashSet<i64> = self.index.keys().copied().collect();
        self.selection.retain(|b| ids.contains(b));
        self.apply_filter();
        if matches!(self.source, Source::Browse(_)) {
            self.view_activate_first();
        }
        refresh_text(new, gone, manual)
    }

    /// Leave the viewed folder entirely (band "Schließen", menu of the root).
    pub(super) fn view_close(&mut self) {
        self.view_tree = None;
    }

    /// The section "ANGESEHEN" at the top of the left panel (nothing while no folder is viewed).
    pub(super) fn view_section(&mut self, ui: &mut egui::Ui) {
        if self.view_tree.is_none() {
            return;
        }
        let browsing = matches!(self.source, Source::Browse(_)) && !self.grid_replaced();
        let foreground = self.loader.foreground;
        let ctx = ui.ctx().clone();
        let mut select: Option<String> = None;
        let mut subfolders: Option<bool> = None;
        let mut close = false;
        lr::section(ui, "Angesehen", true, |ui| {
            // the option first, at a fixed place under the header
            let mut on = include_subfolders();
            if option_row(ui, &mut on, crate::tr("Unterordner einbeziehen"), crate::tr("Aus: nur die Fotos direkt im gewählten Ordner")) {
                subfolders = Some(on);
            }
            ui.add_space(2.0);
            let note = self.viewed_listing.as_ref().and_then(Listing::limit_note);
            let Some(tree) = &mut self.view_tree else { return };
            tree.take_children();
            let name = super::view_folder_band::folder_name(&tree.root);
            let (n, cut) = tree.root_count;
            let count = if cut { format!("{}+", super::thousands(n)) } else { super::thousands(n) };
            let has = tree.children.get("").map(|c| c.as_ref().is_some_and(|c| !c.is_empty()));
            let open = tree.open.contains("");
            let (click, triangle, resp) = Self::list_row_text(ui, &name, Some(count), browsing && tree.sel.is_empty(), 0, Some(has.unwrap_or(true)).filter(|h| *h).map(|_| open), false);
            lr::hover_tip(ui, &resp, &tree.root.display().to_string());
            resp.context_menu(|ui| {
                if lr::menu_item(ui, crate::tr("Schließen"), "", true) {
                    close = true;
                }
            });
            if triangle {
                if open {
                    tree.open.remove("");
                } else {
                    tree.open.insert(String::new());
                }
            }
            if click {
                select = Some(String::new());
            }
            if tree.open.contains("") {
                Self::view_nodes(ui, tree, "", 1, browsing, foreground, &ctx, &mut select);
            }
            if let Some(note) = note {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    lr::paint_icon(ui.painter(), egui::Rect::from_min_size(ui.cursor().min + egui::vec2(0.0, 2.0), egui::vec2(12.0, 12.0)), lr::Icon::Warning, LIMIT);
                    ui.add_space(16.0);
                    ui.add(egui::Label::new(egui::RichText::new(note).small().color(LIMIT)).wrap());
                });
            }
            // what works here, in one line, and the way out right where the folder is
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.add(egui::Label::new(egui::RichText::new(crate::tr("Hier gehen Ansehen, Entwickeln, Exportieren und Drucken. Alles andere erst in der Bibliothek.")).small().color(Color32::from_gray(0x8c))).wrap());
            });
            if option_link(ui, crate::tr("Ordner schließen")) {
                close = true;
            }
        });
        if close {
            self.close_view_folder();
        } else if let Some(on) = subfolders {
            self.view_set_subfolders(on);
        } else if let Some(rel) = select {
            self.view_select(&rel);
        }
    }

    /// The subfolders of `rel` (loaded on demand), recursively for the open ones.
    #[allow(clippy::too_many_arguments)]
    fn view_nodes(ui: &mut egui::Ui, tree: &mut ViewTree, rel: &str, depth: usize, browsing: bool, foreground: bool, ctx: &egui::Context, select: &mut Option<String>) {
        tree.want_children(rel, foreground, Some(ctx));
        let Some(kids) = tree.children.get(rel).cloned() else {
            // reading (a network drive can take a moment): a quiet placeholder of one row
            Self::list_row_text(ui, "…", None, false, depth, None, true);
            return;
        };
        let Some(kids) = kids else {
            Self::list_row_text(ui, crate::tr("nicht lesbar"), None, false, depth, None, true);
            return;
        };
        for c in kids {
            let r = rel_join(rel, &c.name);
            let open = tree.open.contains(&r);
            let count = (c.photos > 0).then(|| super::thousands(c.photos));
            let (click, triangle, _) = Self::list_row_text(ui, &c.name, count, browsing && tree.sel == r, depth, c.has_dirs.then_some(open), false);
            if triangle {
                if open {
                    tree.open.remove(&r);
                } else {
                    tree.open.insert(r.clone());
                }
            }
            if click {
                *select = Some(r.clone());
                if c.has_dirs {
                    tree.open.insert(r.clone());
                }
            }
            if tree.open.contains(&r) {
                Self::view_nodes(ui, tree, &r, depth + 1, browsing, foreground, ctx, select);
            }
        }
    }
}

/// A quiet link row for the left panel (symbol, text; the whole row is the click target).
fn option_link(ui: &mut egui::Ui, label: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::click());
    let hot = resp.hovered();
    if hot {
        ui.painter().rect_filled(rect, 0.0, Color32::from_gray(0x2c));
    }
    let color = if hot { lr::TEXT } else { Color32::from_gray(0xa4) };
    lr::paint_icon(ui.painter(), egui::Rect::from_center_size(egui::pos2(rect.left() + 9.0, rect.center().y), egui::vec2(10.0, 10.0)), lr::Icon::X, color);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
    ui.painter().galley(egui::pos2(rect.left() + 18.0, rect.center().y - galley.size().y / 2.0), galley, color);
    resp.clicked()
}

/// A quiet option row for the left panel: a small box in the column of the tree triangles, the label beside it; the
/// whole row is the click target. Returns: toggled.
fn option_row(ui: &mut egui::Ui, on: &mut bool, label: &str, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::click());
    let hot = resp.hovered();
    let painter = ui.painter();
    if hot {
        painter.rect_filled(rect, 0.0, Color32::from_gray(0x2c));
    }
    let x0 = rect.left() + 4.0;
    let square = egui::Rect::from_center_size(egui::pos2(x0 + 5.0, rect.center().y), egui::vec2(10.0, 10.0));
    painter.rect_stroke(square, 1.0, egui::Stroke::new(1.0, Color32::from_gray(if hot { 0xc0 } else { 0x80 })), egui::StrokeKind::Inside);
    if *on {
        painter.rect_filled(square.shrink(2.5), 0.0, Color32::from_gray(0xe0));
    }
    let color = if hot { lr::TEXT } else { Color32::from_gray(0xa4) };
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = painter.layout_no_wrap(label.to_string(), font, color);
    painter.galley(egui::pos2(x0 + 14.0, rect.center().y - galley.size().y / 2.0), galley, color);
    lr::hover_tip(ui, &resp, tip);
    let clicked = resp.clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// Test aid `GRAINEO_VIEW_REFRESH_TEST=<s>` (own data folder only): once, that many seconds after the start, like the
/// button "Aktualisieren" (screenshots of a refresh in the background).
fn test_refresh_due() -> bool {
    static START: std::sync::OnceLock<Option<(Instant, f64)>> = std::sync::OnceLock::new();
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some((t0, s)) = *START.get_or_init(|| {
        let s: f64 = crate::env::var("GRAINEO_VIEW_REFRESH_TEST").ok()?.parse().ok()?;
        crate::env::var_os("GRAINEO_DATA_DIR").map(|_| (Instant::now(), s))
    }) else {
        return false;
    };
    t0.elapsed().as_secs_f64() >= s && !DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// Test aid: folder of the tree to show once the viewed folder is open (`GRAINEO_LIBRARY_TEST="view=…"`).
static TEST_SELECT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Colour of the limit note (amber, like other warnings of the left panel).
const LIMIT: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x60);

impl LibraryView {
    /// Test aid: the last look at the viewed folder is long ago (the next focus return looks again).
    #[cfg(test)]
    pub fn view_test_age(&mut self) {
        if let Some(t) = &mut self.view_tree {
            t.last_check = Instant::now() - FOCUS_CHECK_EVERY * 2;
        }
    }
}

/// Rows of the viewed folder now (tests).
#[cfg(test)]
pub(super) fn viewed_names(b: &LibraryView) -> Vec<String> {
    let mut v: Vec<String> = b.visible.iter().map(|&i| b.rows[i].name.clone()).collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(rel_join("", "A"), "A");
        assert_eq!(rel_join("A", "B"), "A/B");
        assert_eq!(dir_of(Path::new("/x"), ""), PathBuf::from("/x"));
        assert_eq!(dir_of(Path::new("/x"), "A/B"), PathBuf::from("/x").join("A").join("B"));
    }

    /// The two UI tests share the setting "Unterordner einbeziehen": one after the other.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn png(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(p).unwrap();
    }

    fn texts(out: &egui::FullOutput) -> Vec<String> {
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect()
    }

    /// The tree: the root shows everything (subfolders included, any depth), a folder clicked shows its photos, the
    /// switch limits them to the folder itself; refreshing brings new photos and drops removed ones, the active photo
    /// stays; "Schließen" removes the section.
    #[test]
    fn tree_select_switch_refresh_close() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("graineo-view-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in ["top.png", "A/a1.png", "A/deep/er/d.png", "B/b.png"] {
            png(&root.join(f));
        }
        library::settings::set(SUBFOLDERS_KEY, serde_json::json!(true));
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&root, None);
        assert_eq!(viewed_names(&b), vec!["A/a1.png", "A/deep/er/d.png", "B/b.png", "top.png"]);
        let tree = b.view_tree.as_ref().unwrap();
        assert_eq!(tree.root_count, (4, false));
        let kids = tree.children.get("").cloned().flatten().unwrap();
        assert_eq!(kids.iter().map(|c| (c.name.as_str(), c.photos, c.has_dirs)).collect::<Vec<_>>(), vec![("A", 1, true), ("B", 1, false)]);
        // the section: header, switch, root with its count, the first level
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(260.0, 600.0));
        let frame = |b: &mut LibraryView, t: f64| {
            crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), time: Some(t), ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| b.view_section(ui));
            })
        };
        let all = texts(&frame(&mut b, 0.0)).join(" | ");
        for t in [crate::tr("Angesehen").to_uppercase().as_str(), crate::tr("Unterordner einbeziehen"), &crate::library_ui::view_folder_band::folder_name(&root), "A", "B"] {
            assert!(all.contains(t), "{t}: {all}");
        }
        // a folder clicked: its photos with its subfolders, relative to it; the same photos keep their IDs
        let d_id = b.rows.iter().find(|z| z.name == "A/deep/er/d.png").unwrap().image;
        b.view_select("A");
        assert_eq!(viewed_names(&b), vec!["a1.png", "deep/er/d.png"]);
        assert_eq!(b.rows.iter().find(|z| z.name == "deep/er/d.png").unwrap().image, d_id, "same photo, same ID");
        assert!(matches!(b.source, Source::Browse(ref p) if p == &root), "the source stays the viewed root");
        assert_eq!(b.view_tree.as_ref().unwrap().root_count, (4, false), "the root keeps its count");
        // without subfolders: only the photos directly in it
        b.view_set_subfolders(false);
        assert_eq!(viewed_names(&b), vec!["a1.png"]);
        b.view_set_subfolders(true);
        b.set_active(d_id);
        // refresh: one new, one removed, the active photo stays
        png(&root.join("A/neu.png"));
        std::fs::remove_file(root.join("A/a1.png")).unwrap();
        b.refresh_viewed(true);
        let mut actions = Vec::new();
        b.view_poll(&ctx, &mut actions);
        assert_eq!(viewed_names(&b), vec!["deep/er/d.png", "neu.png"]);
        assert_eq!(b.active, Some(d_id));
        let said: Vec<String> = actions.iter().filter_map(|a| match a { Action::Status(t) => Some(t.clone()), _ => None }).collect();
        assert_eq!(said, vec![refresh_text(1, 1, true).unwrap()]);
        // nothing changed: a manual refresh says so, a quiet one says nothing
        b.refresh_viewed(false);
        let mut actions = Vec::new();
        b.view_poll(&ctx, &mut actions);
        assert!(actions.is_empty());
        // the root again: everything
        b.view_select("");
        assert_eq!(viewed_names(&b), vec!["A/deep/er/d.png", "A/neu.png", "B/b.png", "top.png"]);
        // another source of the library keeps the section; closing removes it and the rows
        b.set_source(Source::All);
        assert!(b.view_tree.is_some());
        b.close_view_folder();
        assert!(b.view_tree.is_none());
        assert!(b.rows.iter().all(|z| z.folder != VIEW_FOLDER));
        assert!(texts(&frame(&mut b, 1.0)).is_empty(), "no section without a viewed folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// More than 5000 photos: the first 5000 are shown and the band and the section say so; a subfolder that is
    /// small shows everything and no note.
    #[test]
    fn limit_shown_in_band_and_section() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("graineo-view-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let seed = root.join("klein/seed.png");
        png(&seed);
        std::fs::create_dir_all(root.join("viele")).unwrap();
        for i in 0..view_folder::MAX_FILES + 2 {
            let p = root.join("viele").join(format!("f{i:05}.png"));
            if std::fs::hard_link(&seed, &p).is_err() {
                std::fs::copy(&seed, &p).unwrap();
            }
        }
        library::settings::set(SUBFOLDERS_KEY, serde_json::json!(true));
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&root, None);
        assert_eq!(b.visible.len(), view_folder::MAX_FILES);
        let note = b.viewed_listing.as_ref().and_then(Listing::limit_note).expect("limit note");
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 700.0));
        let frame = |b: &mut LibraryView, t: f64| {
            crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), time: Some(t), ..Default::default() }, |root| {
                b.view_folder_band(root, false, false);
                egui::Panel::left("l").exact_size(260.0).show(root, |ui| b.view_section(ui));
                egui::CentralPanel::default().show(root, |_| {});
            })
        };
        let all = texts(&frame(&mut b, 0.0));
        assert_eq!(all.iter().filter(|t| **t == note).count(), 2, "band and section: {all:?}");
        assert!(all.iter().any(|t| t.ends_with('+')), "the root count says there are more: {all:?}");
        b.view_select("klein");
        assert_eq!(b.visible.len(), 1);
        let all = texts(&frame(&mut b, 1.0));
        assert!(!all.iter().any(|t| *t == note), "{all:?}");
        b.view_select("viele");
        let all = texts(&frame(&mut b, 2.0));
        assert_eq!(all.iter().filter(|t| **t == note).count(), 2, "per folder clicked: {all:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The quick collection cannot hold photos of a viewed folder (no library entry): B, the badge and a drop say
    /// why and change nothing; no undo step, nothing written. Before, B marked the photo, the write failed
    /// ("FOREIGN KEY constraint failed") and the mark vanished again.
    #[test]
    fn quick_collection_refused_while_viewing() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("graineo-view-quick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        png(&root.join("a.png"));
        let ctx = egui::Context::default();
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&root, None);
        let id = b.active.unwrap();
        assert!(!b.quick_possible(id));
        let undo_before = b.manage.undo.len();
        let sent_before = b.loader.quick_sent;
        assert_eq!(b.quick_toggle(&[id]).as_deref(), Some(crate::tr(super::super::QUICK_REFUSED)));
        assert_eq!(b.drop_on_quick(vec![id]), crate::tr(super::super::QUICK_REFUSED));
        assert!(!b.in_quick(id));
        assert_eq!((b.manage.undo.len(), b.loader.quick_sent), (undo_before, sent_before), "no undo step, nothing sent to the writer");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding for the report (not changed here): collections and keywords refuse photos of a viewed folder only
    /// in the database (foreign key), the app shows that error text.
    #[test]
    fn library_only_functions_fail_in_the_database_for_viewed_photos() {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        let c = library::manage::create_collection(&db, library::manage::COLLECTION, &format!("probe-{}", std::process::id()), None, None).unwrap();
        let viewed = view_id();
        assert!(library::manage::add_to_collection(&db, c, &[viewed]).is_err());
        let kw = match library::keywords::resolve(&db, &format!("probe{}", std::process::id()), true).unwrap() {
            library::keywords::Resolved::Existing(k) | library::keywords::Resolved::Created(k) => k,
            _ => unreachable!(),
        };
        assert!(library::keywords::change(&db, &[viewed], &[kw], &[]).is_err());
        // the other library-only commands, as they ran before the guard (`view_folder_guard`): nothing to act on
        // (capture time, remove from library: no change, yet reported as done), or a database error (virtual copy,
        // stack, move/rename)
        let other = view_id();
        let time = library::time_correction::relocate(&db, &[viewed], 3600, false).map(|r| r.changed);
        let removed = library::manage::remove_photos(&db, &[viewed]).map(|s| s.is_empty());
        let copy = library::sync::create_copy(&db, viewed, None);
        let stack = library::series::create(&db, &[viewed, other], viewed).map(|_| ());
        let moved = library::relocate::plan_files(&db, &[viewed], &std::env::temp_dir(), Some("x")).map(|o| o.steps.len());
        eprintln!("viewed photo against the library: time {time:?}, remove {removed:?}, copy {copy:?}, stack {stack:?}, move {moved:?}");
        assert_eq!(time, Ok(0));
        assert_eq!(removed, Ok(true));
        assert!(copy.is_err());
    }

    #[test]
    fn refresh_texts() {
        assert_eq!(refresh_text(0, 0, false), None);
        assert!(refresh_text(0, 0, true).is_some());
        assert!(refresh_text(2, 0, false).unwrap().contains("2"));
        assert!(refresh_text(0, 1, false).unwrap().contains("entfernt"));
    }
}
