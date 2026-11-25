//! "RAW+JPEG doppelt" – UI: the same shot is in the library twice (RAW and its camera JPEG as two photos).
//!
//! - Left panel: entry with the number of sure pairs (only if there are pairs).
//! - Centre: one row per pair (both photos side by side, level of certainty), sure pairs ticked, a main
//!   button "JPEGs entfernen …" for the ticked ones.
//! - Dialog: what happens (Trash by default, deleting for good only if switched on), carrying ratings and keywords
//!   over to the RAW, progress that can be cancelled, result.
//!
//! Detection and removal: `library::pairs` (the RAW is never touched, offline copies are not read at all).
//! The search runs in the background (lowest priority), the removal at normal priority while the dialog waits (in
//! blocks: one Trash request and one transaction per block); the UI never waits for the database.
//! Submodule of `library_ui` (accesses its fields).

use super::*;
use library::pairs::{self, Disposal, Level, Outcome, Pair, Summary};

/// Result of the background search.
#[derive(Default)]
struct Scan {
    running: bool,
    pairs: Option<Arc<Vec<Pair>>>,
    summary: Summary,
    error: Option<String>,
    /// number of the finished search (the UI applies the default ticks once per search)
    generation: u64,
    /// number of photos in the list when the search started
    rows: usize,
    at: Option<std::time::Instant>,
}

#[derive(Default)]
struct RunState {
    running: bool,
    done: usize,
    total: usize,
    outcome: Option<Outcome>,
    /// start of the run (for "running for … · about … left")
    started: Option<std::time::Instant>,
}

enum Ask {
    Confirm,
    Running,
    Done(Outcome),
}

pub(super) struct PairsState {
    pub open: bool,
    scan: Arc<Mutex<Scan>>,
    generation_seen: u64,
    /// ticked JPEGs (image IDs)
    checked: HashSet<i64>,
    ask: Option<Ask>,
    run: Arc<Mutex<RunState>>,
    cancel: Arc<AtomicBool>,
    /// carry ratings, keywords … over to the RAW (default on)
    transfer: bool,
    /// delete for good instead of the Trash (default off)
    permanent: bool,
    /// re-read the library and search again after the run
    after_run: bool,
    test_done: bool,
    /// Filter and order of the list
    view: View,
    /// visible pairs (indices into the list) for (search number, view)
    shown: Option<(u64, View, Arc<Vec<usize>>)>,
    /// "Paare prüfen": position in the visible list shown large
    review: Option<usize>,
}

/// Which pairs the list shows and in which order.
#[derive(Clone, Debug, Default, PartialEq)]
struct View {
    /// None: all levels
    level: Option<Level>,
    why: Why,
    /// RAW format (CR2, X3F …); None: all
    format: Option<String>,
    place: Place,
    sort: SortBy,
    /// year of the capture; None: all
    year: Option<i32>,
    /// origin of the JPEG's root folder ('lightroom', 'own', 'apple-photos'); None: all
    origin: Option<String>,
}

/// Reason filter: why a JPEG has data of its own, or what would go over to the RAW.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Why {
    #[default]
    Any,
    Develop,
    Lightroom,
    Copies,
    /// rating, label, caption, keywords, people or collections only at the JPEG
    Transfer,
}

/// Where the JPEG lies relative to its RAW.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Place {
    #[default]
    Any,
    Same,
    /// JPEG in a subfolder of the RAW's folder (camera folders like "jpg/")
    Sub,
    /// elsewhere (neither the same folder nor a subfolder of it)
    Other,
}

/// JPEG in a subfolder of the RAW's folder (e.g. "IMG_1.CR2" and "jpg/IMG_1.JPG")?
fn in_subfolder(p: &Pair) -> bool {
    let (Some(raw_dir), Some(jpeg_dir)) = (p.raw.path.parent(), p.jpeg.path.parent()) else { return false };
    jpeg_dir != raw_dir && jpeg_dir.starts_with(raw_dir)
}

fn year_of(p: &Pair) -> Option<i32> {
    p.jpeg.capture.get(..4).and_then(|y| y.parse().ok())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SortBy {
    #[default]
    Level,
    /// largest JPEG first (space freed)
    Size,
    /// newest first
    Date,
    Folder,
    Name,
    /// largest departure first (other name, time, aspect ratio, smaller JPEG: `pairs::deviation`)
    Deviation,
}

fn why_fits(why: Why, p: &Pair) -> bool {
    use library::pairs::reason;
    match why {
        Why::Any => true,
        Why::Develop => p.reasons & reason::DEVELOP != 0,
        Why::Lightroom => p.reasons & reason::LIGHTROOM != 0,
        Why::Copies => p.reasons & reason::COPIES != 0,
        Why::Transfer => !p.transfer.is_empty(),
    }
}

/// The pairs the view shows, in its order (indices into `pairs`).
fn visible(view: &View, pairs: &[Pair]) -> Vec<usize> {
    let mut v: Vec<usize> = (0..pairs.len())
        .filter(|&i| {
            let p = &pairs[i];
            view.level.is_none_or(|l| p.level == l)
                && why_fits(view.why, p)
                && view.format.as_ref().is_none_or(|f| &p.raw.format == f)
                && match view.place {
                    Place::Any => true,
                    Place::Same => p.raw.folder == p.jpeg.folder,
                    Place::Sub => in_subfolder(p),
                    Place::Other => p.raw.folder != p.jpeg.folder && !in_subfolder(p),
                }
                && view.year.is_none_or(|y| year_of(p) == Some(y))
                && view.origin.as_ref().is_none_or(|o| &p.jpeg.origin == o)
        })
        .collect();
    let key = |p: &Pair| (p.level, p.jpeg.path.clone());
    match view.sort {
        // the list comes sorted by level and path already
        SortBy::Level => v.sort_by(|&a, &b| key(&pairs[a]).cmp(&key(&pairs[b]))),
        SortBy::Size => v.sort_by(|&a, &b| pairs[b].jpeg.size.cmp(&pairs[a].jpeg.size).then(a.cmp(&b))),
        SortBy::Date => v.sort_by(|&a, &b| pairs[b].jpeg.time.cmp(&pairs[a].jpeg.time).then(a.cmp(&b))),
        SortBy::Folder => v.sort_by(|&a, &b| pairs[a].jpeg.path.parent().cmp(&pairs[b].jpeg.path.parent()).then_with(|| pairs[a].jpeg.name.cmp(&pairs[b].jpeg.name))),
        SortBy::Name => v.sort_by(|&a, &b| pairs[a].jpeg.name.to_lowercase().cmp(&pairs[b].jpeg.name.to_lowercase()).then(a.cmp(&b))),
        SortBy::Deviation => {
            let d: std::collections::HashMap<usize, f64> = v.iter().map(|&i| (i, library::pairs::deviation(&pairs[i]))).collect();
            v.sort_by(|a, b| d[b].total_cmp(&d[a]).then(a.cmp(b)));
        }
    }
    v
}

/// Short description of the level with its reason (row of the list).
fn level_detail(p: &Pair) -> String {
    use library::pairs::reason;
    match p.level {
        Level::Safe if !p.transfer.is_empty() => crate::trf!("{a} · Bewertung/Stichwörter gehen aufs RAW", a = level_name(p.level)),
        Level::Probable => {
            let why = if !p.same_name { crate::tr("anderer Name") } else if !p.shape_ok { crate::tr("Größe oder Seitenverhältnis weicht ab") } else { crate::tr("nicht eindeutig") };
            format!("{} · {why}", level_name(p.level))
        }
        Level::OwnData => {
            let mut parts: Vec<&str> = Vec::new();
            if p.reasons & reason::DEVELOP != 0 {
                parts.push(crate::tr("in graineo entwickelt"));
            }
            if p.reasons & reason::LIGHTROOM != 0 {
                parts.push(crate::tr("in Lightroom bearbeitet"));
            }
            if p.reasons & reason::COPIES != 0 {
                parts.push(crate::tr("virtuelle Kopien"));
            }
            if parts.is_empty() { level_name(p.level).to_string() } else { format!("{}: {}", level_name(p.level), parts.join(", ")) }
        }
        _ => level_name(p.level).to_string(),
    }
}

impl Default for PairsState {
    fn default() -> Self {
        PairsState {
            open: false,
            scan: Default::default(),
            generation_seen: 0,
            checked: HashSet::new(),
            ask: None,
            run: Default::default(),
            cancel: Default::default(),
            transfer: true,
            permanent: false,
            after_run: false,
            test_done: false,
            view: View::default(),
            shown: None,
            review: None,
        }
    }
}

/// Searching again earliest after this time when the list of photos changed.
const RESCAN_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Safe => crate::tr("sicher"),
        Level::Probable => crate::tr("wahrscheinlich"),
        Level::RawUnreachable => crate::tr("RAW nicht erreichbar"),
        Level::OwnData => crate::tr("hat eigene Daten"),
    }
}

fn level_reason(p: &Pair) -> String {
    match p.level {
        Level::Safe => crate::tr("Gleicher Name, gleiche Aufnahmezeit und Kamera: das RAW liegt auch in der Bibliothek.").into(),
        Level::Probable => if p.same_name {
            crate::tr("Name, Zeit und Kamera passen, aber Seitenverhältnis oder Größe weichen ab (vielleicht ein verkleinerter Export). Nur auf Wunsch entfernen.").into()
        } else {
            crate::tr("Gleiche Aufnahmezeit, Kamera und Ordner, aber ein anderer Name. Nur auf Wunsch entfernen.").into()
        },
        Level::RawUnreachable => crate::tr("Das RAW ist gerade nicht erreichbar (Laufwerk nicht verbunden oder Datei fehlt). Das JPEG bleibt vorerst.").into(),
        Level::OwnData => crate::tr("Das JPEG ist bearbeitet oder hat virtuelle Kopien. Es bleibt.").into(),
    }
}

fn level_color(l: Level) -> Color32 {
    match l {
        Level::Safe => Color32::from_rgb(0x4c, 0xc2, 0x6a),
        Level::Probable => Color32::from_rgb(0xe0, 0xb0, 0x40),
        _ => lr::TEXT_DIM(),
    }
}

/// May this pair be ticked (and removed)?
fn removable(p: &Pair) -> bool {
    matches!(p.level, Level::Safe | Level::Probable)
}

/// Last two parts of the folder, for the display.
fn folder_text(p: &Path) -> String {
    let parts: Vec<String> = p.parent().into_iter().flat_map(|d| d.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>()).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn size_text(bytes: i64) -> String {
    crate::dialog::size(bytes.max(0) as u64)
}

impl LibraryView {
    /// Start the search in the background (if none is running).
    fn pairs_scan(&mut self, ctx: &egui::Context) {
        {
            let mut s = self.pairs.scan.lock().unwrap();
            if s.running {
                return;
            }
            s.running = true;
            s.rows = self.rows.len();
        }
        let scan = self.pairs.scan.clone();
        let ctx = ctx.clone();
        crate::threads::spawn(crate::threads::Lane::Back, move || {
            let r = Db::read_only(&library::db_path()).map_err(|e| e.to_string()).and_then(|db| pairs::find(&db));
            let mut s = scan.lock().unwrap();
            match r {
                Ok(v) => {
                    s.summary = pairs::summarize(&v);
                    s.pairs = Some(Arc::new(v));
                    s.error = None;
                }
                Err(e) => s.error = Some(e),
            }
            s.running = false;
            s.generation += 1;
            s.at = Some(std::time::Instant::now());
            ctx.request_repaint();
        });
    }

    /// Search once the library is loaded and again (rarely) when the list of photos changed.
    fn pairs_keep_fresh(&mut self, ctx: &egui::Context) {
        if self.db.is_none() || self.first_loading() {
            return;
        }
        let (running, rows, at) = {
            let s = self.pairs.scan.lock().unwrap();
            (s.running, s.rows, s.at)
        };
        if running || self.pairs.ask.is_some() {
            return;
        }
        let stale = at.is_none() || (rows != self.rows.len() && at.is_some_and(|t| t.elapsed() > RESCAN_AFTER));
        if stale {
            self.pairs_scan(ctx);
        }
        // test aid: GRAINEO_PAIRS=open|confirm|review opens the view (and the question) after the first search
        if !self.pairs.test_done && at.is_some() {
            if let Ok(v) = crate::env::var("GRAINEO_PAIRS") {
                self.pairs.test_done = true;
                crate::people_ui::close();
                self.pairs.open = true;
                if v == "confirm" {
                    self.pairs.ask = Some(Ask::Confirm);
                }
                // "Paare prüfen" (screenshots)
                if v == "review" {
                    self.pairs.review = Some(0);
                }
            }
        }
    }

    /// Entry in the left panel (under "Duplikate"); only if there are pairs.
    pub(super) fn pairs_list_row(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.pairs_keep_fresh(&ctx);
        let (safe, all) = {
            let s = self.pairs.scan.lock().unwrap();
            (s.summary.safe, s.summary.safe + s.summary.probable + s.summary.unreachable + s.summary.own_data)
        };
        if all == 0 && !self.pairs.open {
            return;
        }
        let (click, _, resp) = Self::special_row(ui, lr::Icon::Stack, crate::tr("RAW+JPEG doppelt"), Some(safe), self.pairs.open, safe == 0);
        lr::hover_tip(ui, &resp, crate::tr("RAW und JPEG desselben Fotos liegen beide in der Bibliothek"));
        if click {
            crate::people_ui::close();
            self.pairs.open = true;
            self.loupe = false;
            // look again when opening (new imports)
            if self.pairs.scan.lock().unwrap().at.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(5)) {
                self.pairs_scan(&ctx);
            }
        }
    }

    /// New search finished: sure pairs are ticked by default.
    fn pairs_apply_scan(&mut self) -> Option<Arc<Vec<Pair>>> {
        let s = self.pairs.scan.lock().unwrap();
        let v = s.pairs.clone();
        if s.generation != self.pairs.generation_seen {
            self.pairs.generation_seen = s.generation;
            self.pairs.checked = v.iter().flat_map(|v| v.iter()).filter(|p| p.level == Level::Safe).map(|p| p.jpeg.image).collect();
        }
        v
    }

    fn pairs_thumb(&mut self, ui: &mut egui::Ui, item: &pairs::Item) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(66.0, 46.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, crate::lr::gray(0x1c));
        if let Some(t) = self.thumbnail(item.file) {
            let s = t.size_vec2();
            if s.x > 0.0 && s.y > 0.0 {
                let k = (rect.width() / s.x).min(rect.height() / s.y);
                let r = egui::Rect::from_center_size(rect.center(), s * k);
                ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
    }

    /// The view in the centre of the library.
    pub(super) fn pairs_center(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.pairs_keep_fresh(&ctx);
        let list = self.pairs_apply_scan();
        let (running, error, summary) = {
            let s = self.pairs.scan.lock().unwrap();
            (s.running, s.error.clone(), s.summary.clone())
        };
        let busy = self.pairs.ask.is_some();
        ui.add_space(4.0);
        ui.label(egui::RichText::new(crate::tr("RAW+JPEG DOPPELT")).color(lr::TEXT_DIM()).size(11.0));
        let total = summary.safe + summary.probable + summary.unreachable + summary.own_data;
        let head = if let Some(e) = &error {
            crate::trf!("Suche nicht möglich: {f}", f = e)
        } else if list.is_none() {
            crate::tr("Wird gesucht …").to_string()
        } else if total == 0 {
            crate::tr("Keine doppelten Fotos aus RAW und JPEG gefunden.").to_string()
        } else {
            crate::trf!(
                "{a} Paare gefunden: {b} sicher, {c} wahrscheinlich, {d} mit eigenen Daten, {e} mit nicht erreichbarem RAW",
                a = thousands(total),
                b = thousands(summary.safe),
                c = thousands(summary.probable),
                d = thousands(summary.own_data),
                e = thousands(summary.unreachable)
            )
        };
        ui.label(egui::RichText::new(head).color(lr::TEXT()));
        ui.add_space(6.0);
        let pairs_all: Arc<Vec<Pair>> = list.unwrap_or_default();
        // Filter, order; ticking, sums and removing only on what is visible
        if total > 0 {
            self.pairs_filter_bar(ui, &pairs_all, &summary);
            ui.add_space(4.0);
        }
        let shown = self.pairs_visible(&pairs_all);
        let chosen = |p: &Pair| removable(p) && self.pairs.checked.contains(&p.jpeg.image);
        let n_checked = shown.iter().filter(|&&i| chosen(&pairs_all[i])).count();
        let bytes_checked: i64 = shown.iter().filter(|&&i| chosen(&pairs_all[i])).map(|&i| pairs_all[i].jpeg.size.max(0)).sum();
        let safe_shown = shown.iter().filter(|&&i| pairs_all[i].level == Level::Safe).count();
        ui.horizontal_wrapped(|ui| {
            let label = if n_checked == 0 { crate::tr("JPEGs entfernen …").to_string() } else { crate::trf!("{a} JPEGs entfernen …", a = thousands(n_checked)) };
            let r = lr::main_button(ui, &label, n_checked > 0 && !busy);
            lr::hover_tip(ui, &r, crate::tr("Die angehakten JPEGs entfernen; die RAWs bleiben"));
            if r.clicked() {
                self.pairs.ask = Some(Ask::Confirm);
            }
            ui.add_space(8.0);
            let r = ui.add_enabled(safe_shown > 0, egui::Button::new(crate::tr("Alle sicheren")));
            lr::hover_tip(ui, &r, crate::tr("Alle sichtbaren sicheren Paare anhaken"));
            if r.clicked() {
                self.pairs.checked.extend(shown.iter().map(|&i| &pairs_all[i]).filter(|p| p.level == Level::Safe).map(|p| p.jpeg.image));
            }
            let r = ui.add_enabled(n_checked > 0, egui::Button::new(crate::tr("Keine")));
            lr::hover_tip(ui, &r, crate::tr("Alle sichtbaren Paare abhaken"));
            if r.clicked() {
                for &i in shown.iter() {
                    self.pairs.checked.remove(&pairs_all[i].jpeg.image);
                }
            }
            let r = ui.add_enabled(!shown.is_empty() && !busy, egui::Button::new(crate::tr("Paare prüfen …")));
            lr::hover_tip(ui, &r, crate::tr("Paare einzeln groß ansehen und anhaken"));
            if r.clicked() {
                self.pairs.review = Some(0);
            }
            ui.add_space(8.0);
            if ui.add_enabled(!running && !busy, egui::Button::new(crate::tr("Neu suchen"))).clicked() {
                self.pairs_scan(&ctx);
            }
            if running {
                ui.add(egui::Spinner::new().size(12.0));
            }
            crate::help::info(ui, "raw-jpeg-doppelt", "RAW+JPEG doppelt", &crate::help::RAW_JPEG_PAIRS);
        });
        if total > 0 {
            let sum = crate::trf!(
                "{a} angehakt von {b} sichtbar · {c} würden frei",
                a = thousands(n_checked),
                b = thousands(shown.len()),
                c = size_text(bytes_checked)
            );
            ui.add(egui::Label::new(egui::RichText::new(sum).small().color(lr::TEXT_DIM())).truncate());
        }
        ui.add_space(6.0);
        ui.separator();
        if let Some(pos) = self.pairs.review {
            if busy || shown.is_empty() {
                self.pairs.review = None;
            } else {
                self.pairs_review(ui, &pairs_all, &shown, pos.min(shown.len() - 1));
                return;
            }
        }
        const ROW: f32 = 64.0;
        let mut toggled: Vec<(i64, bool)> = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, ROW, shown.len(), |ui, range| {
            let images: Vec<i64> = shown[range.clone()].iter().flat_map(|&i| [pairs_all[i].raw.image, pairs_all[i].jpeg.image]).collect();
            self.request_thumbnails(&images);
            for p in shown[range].iter().map(|&i| &pairs_all[i]) {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), egui::Sense::hover());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 0.0, crate::lr::gray(0x22));
                }
                ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, crate::lr::gray(0x2a)));
                let mut on = self.pairs.checked.contains(&p.jpeg.image);
                let can = removable(p);
                let box_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 16.0, rect.center().y), egui::vec2(20.0, 20.0));
                let cb = ui.put(box_rect, |ui: &mut egui::Ui| ui.add_enabled(can, egui::Checkbox::without_text(&mut on)));
                lr::hover_tip(ui, &cb, crate::tr("JPEG zum Entfernen anhaken"));
                if cb.changed() {
                    toggled.push((p.jpeg.image, on));
                }
                let thumb_raw = egui::Rect::from_min_size(egui::pos2(rect.left() + 34.0, rect.top() + 9.0), egui::vec2(66.0, 46.0));
                let thumb_jpeg = thumb_raw.translate(egui::vec2(72.0, 0.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(thumb_raw), |ui| self.pairs_thumb(ui, &p.raw));
                ui.scope_builder(egui::UiBuilder::new().max_rect(thumb_jpeg), |ui| self.pairs_thumb(ui, &p.jpeg));
                let x = rect.left() + 34.0 + 144.0;
                let w = (rect.right() - x - 8.0).max(40.0);
                let clip = ui.painter_at(egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, ROW)));
                let font = egui::TextStyle::Body.resolve(ui.style());
                let small = egui::TextStyle::Small.resolve(ui.style());
                let line = |y: f32, text: String, color: Color32, font: &egui::FontId| {
                    clip.text(egui::pos2(x, rect.top() + y), egui::Align2::LEFT_CENTER, text, font.clone(), color);
                };
                line(12.0, crate::trf!("RAW  {a} · {b} · {c}", a = p.raw.name, b = p.raw.format, c = size_text(p.raw.size)), lr::TEXT(), &font);
                line(28.0, crate::trf!("JPEG {a} · {b}", a = p.jpeg.name, b = size_text(p.jpeg.size)), lr::TEXT(), &font);
                let mut third = format!("{} · {}", p.jpeg.capture.replace('T', " "), folder_text(&p.jpeg.path));
                if p.raw.folder != p.jpeg.folder {
                    third = format!("{third} ← {}", folder_text(&p.raw.path));
                }
                line(43.0, third, lr::TEXT_DIM(), &small);
                line(55.0, level_detail(p), level_color(p.level), &small);
                let tip = ui.interact(rect, ui.scope_id().with(("paar", p.jpeg.image)), egui::Sense::hover());
                lr::hover_tip(ui, &tip, &level_reason(p));
            }
        });
        for (b, on) in toggled {
            if on {
                self.pairs.checked.insert(b);
            } else {
                self.pairs.checked.remove(&b);
            }
        }
    }

    /// "Paare prüfen": one pair large (RAW left, JPEG right), ←/→ to the neighbour, space ticks,
    /// Esc back to the list.
    fn pairs_review(&mut self, ui: &mut egui::Ui, pairs_all: &[Pair], shown: &[usize], pos: usize) {
        let n = shown.len();
        let p = &pairs_all[shown[pos]];
        let (mut prev, mut next, mut toggle, mut close) = (false, false, false, false);
        if !ui.ctx().egui_wants_keyboard_input() {
            ui.input_mut(|i| {
                prev = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft);
                next = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight);
                toggle = i.consume_key(egui::Modifiers::NONE, egui::Key::Space);
                close = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            });
        }
        let can = removable(p);
        let mut on = self.pairs.checked.contains(&p.jpeg.image);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::trf!("Paar {a} von {b}", a = thousands(pos + 1), b = thousands(n))).color(lr::TEXT()));
            ui.add_space(8.0);
            let r = ui.add_enabled(pos > 0, egui::Button::new(crate::tr("◀ Vorheriges")));
            lr::tip_key(ui, &r, "Vorheriges Paar", "pairs_switch");
            prev |= r.clicked();
            let r = ui.add_enabled(pos + 1 < n, egui::Button::new(crate::tr("Nächstes ▶")));
            lr::tip_key(ui, &r, "Nächstes Paar", "pairs_switch");
            next |= r.clicked();
            ui.add_space(8.0);
            let r = ui.add_enabled(can, egui::Checkbox::new(&mut on, crate::tr("JPEG entfernen")));
            lr::tip_key(ui, &r, "JPEG zum Entfernen anhaken", "pairs_check");
            if r.changed() {
                toggle = !toggle;
            }
            ui.add_space(8.0);
            let r = ui.button(crate::tr("Zur Liste"));
            lr::tip_key(ui, &r, "Zurück zur Liste", "pairs_list");
            close |= r.clicked();
        });
        if toggle && can {
            if self.pairs.checked.contains(&p.jpeg.image) {
                self.pairs.checked.remove(&p.jpeg.image);
            } else {
                self.pairs.checked.insert(p.jpeg.image);
            }
        }
        ui.add(egui::Label::new(egui::RichText::new(level_detail(p)).color(level_color(p.level))).truncate());
        ui.add_space(4.0);
        // both photos as large as the space allows, side by side
        self.request_thumbnails(&[p.raw.image, p.jpeg.image]);
        let area = ui.available_rect_before_wrap();
        let caption_h = 40.0;
        let gap = 12.0;
        let w = ((area.width() - gap) / 2.0).max(0.0);
        let h = (area.height() - caption_h).max(0.0);
        for (k, item) in [&p.raw, &p.jpeg].into_iter().enumerate() {
            let frame = egui::Rect::from_min_size(egui::pos2(area.left() + k as f32 * (w + gap), area.top()), egui::vec2(w, h));
            ui.painter().rect_filled(frame, 0.0, crate::lr::gray(0x1c));
            if let Some(t) = self.thumbnail(item.file) {
                let s = t.size_vec2();
                if s.x > 0.0 && s.y > 0.0 {
                    let f = (frame.width() / s.x).min(frame.height() / s.y);
                    let r = egui::Rect::from_center_size(frame.center(), s * f);
                    ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
            let size = match (item.width, item.height) {
                (Some(a), Some(b)) => format!(" · {a} × {b}"),
                _ => String::new(),
            };
            let line1 = format!("{} {} · {}{size}", if k == 0 { "RAW " } else { "JPEG" }, item.name, size_text(item.size));
            let line2 = format!("{} · {}", item.capture.replace('T', " "), folder_text(&item.path));
            let clip = ui.painter_at(egui::Rect::from_min_size(egui::pos2(frame.left(), frame.bottom()), egui::vec2(w, caption_h)));
            let font = egui::TextStyle::Body.resolve(ui.style());
            let small = egui::TextStyle::Small.resolve(ui.style());
            clip.text(egui::pos2(frame.left(), frame.bottom() + 12.0), egui::Align2::LEFT_CENTER, line1, font, lr::TEXT());
            clip.text(egui::pos2(frame.left(), frame.bottom() + 30.0), egui::Align2::LEFT_CENTER, line2, small, lr::TEXT_DIM());
        }
        ui.allocate_rect(area, egui::Sense::hover());
        if close {
            self.pairs.review = None;
        } else if prev && pos > 0 {
            self.pairs.review = Some(pos - 1);
        } else if next && pos + 1 < n {
            self.pairs.review = Some(pos + 1);
        } else {
            self.pairs.review = Some(pos);
        }
    }

    /// The pairs view (list or review) takes the keys of the library while it is open.
    pub fn pairs_view_open(&self) -> bool {
        self.pairs.open
    }

    /// The visible pairs for the current view (cached per search and view).
    fn pairs_visible(&mut self, pairs_all: &[Pair]) -> Arc<Vec<usize>> {
        let generation = self.pairs.generation_seen;
        if let Some((g, v, list)) = &self.pairs.shown {
            if *g == generation && *v == self.pairs.view && list.iter().all(|&i| i < pairs_all.len()) {
                return list.clone();
            }
        }
        let list = Arc::new(visible(&self.pairs.view, pairs_all));
        self.pairs.shown = Some((generation, self.pairs.view.clone(), list.clone()));
        list
    }

    /// Level buttons with counts, then reason, RAW format, folder and order.
    fn pairs_filter_bar(&mut self, ui: &mut egui::Ui, pairs_all: &[Pair], summary: &Summary) {
        let all = summary.safe + summary.probable + summary.unreachable + summary.own_data;
        let count = |text: &str, n: usize| format!("{text} {}", thousands(n));
        let labels = [
            count(crate::tr("Alle"), all),
            count(crate::tr("sicher"), summary.safe),
            count(crate::tr("wahrscheinlich"), summary.probable),
            count(crate::tr("eigene Daten"), summary.own_data),
            count(crate::tr("RAW nicht erreichbar"), summary.unreachable),
        ];
        let options: [(Option<Level>, &str); 5] = [
            (None, &labels[0]),
            (Some(Level::Safe), &labels[1]),
            (Some(Level::Probable), &labels[2]),
            (Some(Level::OwnData), &labels[3]),
            (Some(Level::RawUnreachable), &labels[4]),
        ];
        let view = &mut self.pairs.view;
        ui.horizontal_wrapped(|ui| {
            lr::segmented_pad(ui, &mut view.level, &options, 8.0);
        });
        ui.add_space(4.0);
        let mut formats: Vec<&str> = pairs_all.iter().map(|p| p.raw.format.as_str()).collect();
        formats.sort_unstable();
        formats.dedup();
        let mut years: Vec<i32> = pairs_all.iter().filter_map(year_of).collect();
        years.sort_unstable_by(|a, b| b.cmp(a));
        years.dedup();
        let mut origins: Vec<&str> = pairs_all.iter().map(|p| p.jpeg.origin.as_str()).collect();
        origins.sort_unstable();
        origins.dedup();
        let origin_name = |o: &str| -> String {
            match o {
                "lightroom" => crate::tr("Aus Lightroom").to_string(),
                "own" => crate::tr("Eigene Ordner").to_string(),
                library::apple_photos::ORIGIN => crate::tr("Apple Fotos").to_string(),
                other => other.to_string(),
            }
        };
        ui.horizontal_wrapped(|ui| {
            let current = view.year.map_or_else(|| crate::tr("Alle Jahre").to_string(), |y| y.to_string());
            egui::ComboBox::from_id_salt("paare-jahr").selected_text(current).width(100.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut view.year, None, crate::tr("Alle Jahre"));
                for y in &years {
                    ui.selectable_value(&mut view.year, Some(*y), y.to_string());
                }
            });
            let current = view.origin.as_deref().map_or_else(|| crate::tr("Alle Herkünfte").to_string(), origin_name);
            let r = egui::ComboBox::from_id_salt("paare-herkunft").selected_text(current).width(140.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut view.origin, None, crate::tr("Alle Herkünfte"));
                for o in &origins {
                    ui.selectable_value(&mut view.origin, Some(o.to_string()), origin_name(o));
                }
            });
            lr::hover_tip(ui, &r.response, crate::tr("Woher das JPEG in die Bibliothek kam"));
            let whys = [
                (Why::Any, crate::tr("Alle Gründe")),
                (Why::Develop, crate::tr("In graineo entwickelt")),
                (Why::Lightroom, crate::tr("In Lightroom bearbeitet")),
                (Why::Copies, crate::tr("Virtuelle Kopien")),
                (Why::Transfer, crate::tr("Bewertung/Stichwörter nur am JPEG")),
            ];
            let current = whys.iter().find(|w| w.0 == view.why).map_or("", |w| w.1);
            let r = egui::ComboBox::from_id_salt("paare-grund").selected_text(current).width(190.0).show_ui(ui, |ui| {
                for (w, text) in whys {
                    ui.selectable_value(&mut view.why, w, text);
                }
            });
            lr::hover_tip(ui, &r.response, crate::tr("Nach Grund filtern: warum ein JPEG eigene Daten hat oder was aufs RAW übertragen würde"));
            let current = view.format.clone().unwrap_or_else(|| crate::tr("Alle RAW-Formate").to_string());
            egui::ComboBox::from_id_salt("paare-format").selected_text(current).width(130.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut view.format, None, crate::tr("Alle RAW-Formate"));
                for f in &formats {
                    ui.selectable_value(&mut view.format, Some(f.to_string()), *f);
                }
            });
            let places = [
                (Place::Any, crate::tr("Alle Ordner")),
                (Place::Same, crate::tr("JPEG im Ordner des RAWs")),
                (Place::Sub, crate::tr("JPEG in Unterordner des RAWs (z. B. „jpg“)")),
                (Place::Other, crate::tr("JPEG in anderem Ordner")),
            ];
            let current = places.iter().find(|w| w.0 == view.place).map_or("", |w| w.1);
            egui::ComboBox::from_id_salt("paare-ort").selected_text(current).width(170.0).show_ui(ui, |ui| {
                for (w, text) in places {
                    ui.selectable_value(&mut view.place, w, text);
                }
            });
            let sorts = [
                (SortBy::Level, crate::tr("Nach Stufe")),
                (SortBy::Size, crate::tr("Größte JPEGs zuerst")),
                (SortBy::Date, crate::tr("Neueste zuerst")),
                (SortBy::Folder, crate::tr("Nach Ordner")),
                (SortBy::Name, crate::tr("Nach Dateiname")),
                (SortBy::Deviation, crate::tr("Größte Abweichung zuerst")),
            ];
            let current = sorts.iter().find(|w| w.0 == view.sort).map_or("", |w| w.1);
            let r = egui::ComboBox::from_id_salt("paare-sortierung").selected_text(current).width(150.0).show_ui(ui, |ui| {
                for (w, text) in sorts {
                    ui.selectable_value(&mut view.sort, w, text);
                }
            });
            lr::hover_tip(ui, &r.response, crate::tr("Reihenfolge der Liste; „Abweichung“: anderer Name, Zeitunterschied, Seitenverhältnis, kleineres JPEG"));
            if *view != View::default() && ui.button(crate::tr("Zurücksetzen")).clicked() {
                *view = View::default();
            }
        });
    }

    /// Question, progress and result (called every frame from `after`).
    pub(super) fn pairs_dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::dialog::{self, Answer, Buttons};
        let Some(ask) = self.pairs.ask.take() else { return };
        let list = self.pairs.scan.lock().unwrap().pairs.clone().unwrap_or_default();
        match ask {
            Ask::Confirm => {
                // only what the list shows (a filter hides ticked pairs: they stay)
                let shown = self.pairs_visible(&list);
                let chosen: Vec<Pair> = shown.iter().map(|&i| &list[i]).filter(|p| removable(p) && self.pairs.checked.contains(&p.jpeg.image)).cloned().collect();
                let n = chosen.len();
                let bytes: i64 = chosen.iter().map(|p| p.jpeg.size.max(0)).sum();
                let with_transfer = chosen.iter().filter(|p| !p.transfer.is_empty()).count();
                let probable = chosen.iter().filter(|p| p.level == Level::Probable).count();
                let (mut transfer, mut permanent) = (self.pairs.transfer, self.pairs.permanent);
                let title = crate::trf!("{a} entfernen?", a = dialog::count(n, crate::tr("JPEG"), crate::tr("JPEGs")));
                let permanent_label = self.pairs.permanent;
                let a = dialog::show(
                    ctx,
                    "raw-jpeg-entfernen",
                    &title,
                    dialog::WIDTH,
                    |ui| {
                        dialog::text(ui, crate::tr("Die JPEGs verschwinden aus der Bibliothek und von der Platte. Die RAWs bleiben unberührt, Offline-Kopien auch."));
                        dialog::chip(ui, &crate::trf!("{a} · {b}", a = dialog::count(n, crate::tr("JPEG"), crate::tr("JPEGs")), b = dialog::size(bytes.max(0) as u64)));
                        if probable > 0 {
                            dialog::warning(ui, &crate::trf!("{a} davon nur „wahrscheinlich“: der Name oder die Größe passt nicht ganz.", a = thousands(probable)));
                        }
                        dialog::options(ui, |ui| {
                            dialog::option(
                                ui,
                                &mut transfer,
                                crate::tr("Bewertungen und Stichwörter aufs RAW übertragen"),
                                &crate::trf!("{a} mit Daten, die dem RAW fehlen (nur ergänzt)", a = dialog::count(with_transfer, crate::tr("JPEG"), crate::tr("JPEGs"))),
                                None,
                            );
                            ui.add_space(2.0);
                            dialog::option(ui, &mut permanent, crate::tr("Stattdessen endgültig löschen"), crate::tr("Nicht wiederherstellbar – ohne Papierkorb"), None);
                        });
                        if permanent {
                            dialog::warning(ui, crate::tr("Die JPEGs werden endgültig gelöscht und lassen sich nicht zurückholen."));
                        } else {
                            dialog::hint(ui, crate::tr("Die JPEGs werden in den Papierkorb gelegt (wiederherstellbar). Auf Netzlaufwerken kommen sie in den Papierkorb-Ordner des Laufwerks."));
                        }
                        if !transfer && with_transfer > 0 {
                            dialog::hint(ui, &crate::trf!("Ohne Übertragen bleiben {a} stehen.", a = dialog::count(with_transfer, crate::tr("JPEG"), crate::tr("JPEGs"))));
                        }
                    },
                    if permanent_label { Buttons::destructive(crate::tr("Endgültig löschen"), n > 0) } else { Buttons::destructive(crate::tr("In den Papierkorb"), n > 0) },
                );
                self.pairs.transfer = transfer;
                self.pairs.permanent = permanent;
                match a {
                    Answer::Primary if n > 0 => {
                        self.pairs_start_run(ctx, chosen);
                        self.pairs.ask = Some(Ask::Running);
                    }
                    Answer::Cancel => {}
                    _ => self.pairs.ask = Some(Ask::Confirm),
                }
            }
            Ask::Running => {
                let (done, total, outcome, started) = {
                    let r = self.pairs.run.lock().unwrap();
                    (r.done, r.total, r.outcome.clone(), r.started)
                };
                if let Some(o) = outcome {
                    self.pairs.run.lock().unwrap().outcome = None;
                    self.pairs.after_run = false;
                    self.reload();
                    self.pairs.scan.lock().unwrap().at = None;
                    self.pairs_scan(ctx);
                    let msg = crate::trf!("{a} entfernt", a = dialog::count(o.removed, crate::tr("JPEG"), crate::tr("JPEGs")));
                    actions.push(Action::Status(msg));
                    self.pairs.ask = Some(Ask::Done(o));
                    ctx.request_repaint();
                    return;
                }
                let cancelling = self.pairs.cancel.load(Ordering::Relaxed);
                let a = dialog::show(
                    ctx,
                    "raw-jpeg-laeuft",
                    crate::tr("JPEGs werden entfernt …"),
                    dialog::WIDTH,
                    |ui| {
                        let f = if total > 0 { Some(done as f32 / total as f32) } else { None };
                        let t = if cancelling { crate::tr("wird abgebrochen …").to_string() } else { crate::trf!("{a} von {b}", a = thousands(done), b = thousands(total)) };
                        dialog::progress(ui, f, &t);
                        // How long it has been running and about how much longer
                        let running = dialog::Running { since: started.map(|t| t.elapsed()).unwrap_or_default(), fraction: f, ..Default::default() };
                        ui.label(egui::RichText::new(running.timing()).small().color(lr::TEXT_DIM()));
                    },
                    Buttons { cancel: crate::tr("Abbrechen"), primary: None },
                );
                if a == Answer::Cancel {
                    self.pairs.cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                self.pairs.ask = Some(Ask::Running);
            }
            Ask::Done(o) => {
                let verdict = if !o.errors.is_empty() { dialog::Verdict::Red } else if !o.skipped.is_empty() || o.cancelled { dialog::Verdict::Yellow } else { dialog::Verdict::Green };
                let mut parts = vec![crate::trf!("{a} entfernt ({b})", a = dialog::count(o.removed, crate::tr("JPEG"), crate::tr("JPEGs")), b = dialog::size(o.bytes))];
                if o.transferred > 0 {
                    parts.push(crate::trf!("{a}: Bewertungen/Stichwörter aufs RAW übertragen", a = thousands(o.transferred)));
                }
                if !o.skipped.is_empty() {
                    parts.push(crate::trf!("{a} blieben stehen", a = thousands(o.skipped.len())));
                }
                if !o.errors.is_empty() {
                    parts.push(crate::trf!("{a} mit Fehler", a = dialog::count(o.errors.len(), crate::tr("Fehler"), crate::tr("Fehler"))));
                }
                if o.cancelled {
                    parts.push(crate::tr("abgebrochen").to_string());
                }
                let details: Vec<String> = o.errors.iter().map(|e| crate::trf!("Fehler: {f}", f = e)).chain(o.skipped.iter().map(|(n, r)| format!("{n}: {}", crate::menu::i18n::tr_msg(r)))).collect();
                let a = dialog::show(ctx, "raw-jpeg-ergebnis", crate::tr("JPEGs entfernt"), dialog::WIDTH, |ui| dialog::outcome(ui, verdict, &parts.join("\n"), &details), Buttons::close());
                if a == Answer::Open {
                    self.pairs.ask = Some(Ask::Done(o));
                }
            }
        }
    }

    fn pairs_start_run(&mut self, ctx: &egui::Context, chosen: Vec<Pair>) {
        {
            let mut r = self.pairs.run.lock().unwrap();
            *r = RunState { running: true, done: 0, total: chosen.len(), outcome: None, started: Some(std::time::Instant::now()) };
        }
        self.pairs.cancel.store(false, Ordering::Relaxed);
        let (run, cancel, ctx) = (self.pairs.run.clone(), self.pairs.cancel.clone(), ctx.clone());
        let opt = pairs::Options { disposal: if self.pairs.permanent { Disposal::Permanent } else { Disposal::Trash }, transfer: self.pairs.transfer };
        // the user waits in the dialog: normal priority (mostly waiting for the disk and the Trash, little computing)
        crate::threads::spawn(crate::threads::Lane::Front, move || {
            let outcome = match Db::standard() {
                Ok(db) => {
                    // the system Trash for local files; blocks and network drives: library::trash_batch (shared with Duplikate)
                    let local = |files: &[PathBuf]| trash::delete_all(files).map_err(|e| e.to_string());
                    let dispose = |db: &Db, files: &[PathBuf]| -> Result<usize, String> { pairs::dispose(db, files, opt.disposal, &local) };
                    let progress = |done: usize, total: usize| {
                        let mut r = run.lock().unwrap();
                        let changed = r.done != done;
                        r.done = done;
                        r.total = total;
                        drop(r);
                        if changed {
                            ctx.request_repaint();
                        }
                    };
                    pairs::remove(&db, &chosen, opt, &dispose, &progress, &|| cancel.load(Ordering::Relaxed))
                }
                Err(e) => Outcome { errors: vec![e.to_string()], ..Default::default() },
            };
            crate::log(&format!("raw-jpeg: removed {}, transferred {}, skipped {}, errors {}", outcome.removed, outcome.transferred, outcome.skipped.len(), outcome.errors.len()));
            let mut r = run.lock().unwrap();
            r.running = false;
            r.outcome = Some(outcome);
            ctx.request_repaint();
        });
    }

    /// Removing is running (status in the title bar).
    pub fn pairs_running(&self) -> bool {
        self.pairs.run.lock().map(|r| r.running).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, level: Level, format: &str, same_folder: bool, size: i64, time: i64) -> Pair {
        let raw = pairs::Item { image: 1, file: 1, folder: 1, path: PathBuf::from(format!("/f/{name}.{format}")), name: format!("{name}.{format}"), format: format.into(), ..Default::default() };
        let folder = if same_folder { 1 } else { 2 };
        let dir = if same_folder { "/f" } else { "/g" };
        let jpeg = pairs::Item { image: 2, file: 2, folder, path: PathBuf::from(format!("{dir}/{name}.JPG")), name: format!("{name}.JPG"), format: "JPG".into(), size, time, ..Default::default() };
        Pair { raw, jpeg, level, same_name: true, transfer: Default::default(), own_data: level == Level::OwnData, reasons: 0, shape_ok: true }
    }

    #[test]
    fn view_filters_and_sorts() {
        use library::pairs::reason;
        let mut list = vec![
            pair("a", Level::Safe, "CR2", true, 300, 10),
            pair("b", Level::Safe, "X3F", false, 900, 30),
            pair("c", Level::Probable, "CR2", true, 100, 20),
            pair("d", Level::OwnData, "CR2", false, 500, 5),
        ];
        list[3].reasons = reason::LIGHTROOM;
        list[0].transfer.stars = Some(3);
        let mut v = View::default();
        assert_eq!(visible(&v, &list), vec![0, 1, 2, 3]);
        v.sort = SortBy::Size;
        assert_eq!(visible(&v, &list), vec![1, 3, 0, 2]);
        v.sort = SortBy::Date;
        assert_eq!(visible(&v, &list), vec![1, 2, 0, 3]);
        // departure: the renamed pair first, then by time difference; equal ones in list order
        let mut dev = list.clone();
        dev.iter_mut().for_each(|p| p.raw.time = p.jpeg.time);
        dev[2].same_name = false;
        dev[3].raw.time += 1;
        v.sort = SortBy::Deviation;
        assert_eq!(visible(&v, &dev), vec![2, 3, 0, 1]);
        v.level = Some(Level::Safe);
        assert_eq!(visible(&v, &list), vec![1, 0]);
        v.format = Some("CR2".into());
        assert_eq!(visible(&v, &list), vec![0]);
        let v = View { why: Why::Lightroom, ..Default::default() };
        assert_eq!(visible(&v, &list), vec![3]);
        let v = View { why: Why::Transfer, ..Default::default() };
        assert_eq!(visible(&v, &list), vec![0]);
        let v = View { place: Place::Other, ..Default::default() };
        assert_eq!(visible(&v, &list), vec![1, 3]);
        assert!(level_detail(&list[3]).contains(crate::tr("in Lightroom bearbeitet")));
        // subfolder "jpg" of the RAW's folder, year, origin
        list[2].jpeg.path = PathBuf::from("/f/jpg/c.JPG");
        list[2].jpeg.folder = 3;
        list[2].jpeg.capture = "2019-03-01T12:00:00".into();
        list[1].jpeg.origin = "lightroom".into();
        assert_eq!(visible(&View { place: Place::Sub, ..Default::default() }, &list), vec![2]);
        assert_eq!(visible(&View { place: Place::Other, ..Default::default() }, &list), vec![1, 3]);
        assert_eq!(visible(&View { year: Some(2019), ..Default::default() }, &list), vec![2]);
        assert_eq!(visible(&View { origin: Some("lightroom".into()), ..Default::default() }, &list), vec![1]);
    }
}
