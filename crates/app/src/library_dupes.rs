//! "Duplikate" – UI: cleaning up duplicates group by group.
//!
//! - Centre (source "Duplikate"): one block per group, the photos side by side with file name, folder, format,
//!   resolution, size, date, rating and own data. The suggested photo to keep comes first, with the reason
//!   ("bleibt – RAW"); in confirmed identical groups the others are marked for removal. "Similar" groups are never
//!   marked by themselves (series!): a click marks or unmarks a photo, a double click keeps only this one.
//! - Filters: kind (identical/similar), place (Lightroom, own folders, network drive), year, "only with
//!   consequences"; sorting by space freed, date or folder. "Als Raster" shows the groups in the normal grid.
//! - Removing: confirmation with summary, ratings/keywords … go to the staying photo first, the files go into the
//!   Trash collected in blocks (`library::trash_batch`), progress with remaining time and cancel, result; ⌘Z brings the
//!   library entries back (the files come back from the Trash with "Put Back").
//!
//! Decision and checks: `library::duplicate_cleanup`. Submodule of `library_ui` (accesses its fields).

use super::*;
use library::duplicate_cleanup::{self as dc, Criterion, Member, Outcome, Phase, Plan, Removal};
use library::duplicates::Kind;

#[derive(Default)]
struct Load {
    running: bool,
    plans: Option<Arc<Vec<Plan>>>,
    left_out: usize,
    error: Option<String>,
    generation: u64,
    /// (groups, photos) of the duplicate search the plans were made for
    made_for: Option<(usize, usize)>,
}

#[derive(Default)]
struct RunState {
    running: bool,
    phase: Option<Phase>,
    done: usize,
    total: usize,
    phase_start: Option<std::time::Instant>,
    outcome: Option<Outcome>,
}

/// Result as shown (the snapshot went to the ⌘Z list).
struct Shown {
    removed: usize,
    bytes: u64,
    transferred: usize,
    skipped: Vec<(String, String)>,
    errors: Vec<String>,
    cancelled: bool,
}

enum Ask {
    Confirm,
    Running,
    Done(Shown),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum KindFilter {
    #[default]
    All,
    Identical,
    Similar,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Place {
    #[default]
    All,
    Lightroom,
    Own,
    Network,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    #[default]
    Space,
    Date,
    Folder,
}

pub(super) struct DupesState {
    /// show the groups in the normal grid instead of side by side
    pub grid: bool,
    load: Arc<Mutex<Load>>,
    generation_seen: u64,
    /// marked for removal (image IDs)
    marked: HashSet<i64>,
    kind: KindFilter,
    place: Place,
    year: Option<String>,
    consequences: bool,
    sort: Sort,
    ask: Option<Ask>,
    run: Arc<Mutex<RunState>>,
    cancel: Arc<AtomicBool>,
    /// carry ratings, keywords … over to the staying photo (default on)
    transfer: bool,
    test_done: bool,
}

impl Default for DupesState {
    fn default() -> Self {
        DupesState {
            grid: false,
            load: Default::default(),
            generation_seen: 0,
            marked: HashSet::new(),
            kind: KindFilter::All,
            place: Place::All,
            year: None,
            consequences: false,
            sort: Sort::Space,
            ask: None,
            run: Default::default(),
            cancel: Default::default(),
            transfer: true,
            test_done: false,
        }
    }
}

const CARD_W: f32 = 212.0;
const THUMB_H: f32 = 112.0;
const CARD_H: f32 = THUMB_H + 7.0 * 15.0 + 12.0;
const GAP: f32 = 8.0;
const HEAD_H: f32 = 24.0;

/// The advantage of the staying photo, as a short phrase.
fn advantage(c: Option<Criterion>) -> &'static str {
    match c {
        Some(Criterion::Reachable) => crate::tr("erreichbar"),
        Some(Criterion::Developed) => crate::tr("Entwicklung"),
        Some(Criterion::Data) => crate::tr("Bewertung/Stichwörter"),
        Some(Criterion::Raw) => crate::tr("RAW"),
        Some(Criterion::Resolution) => crate::tr("höhere Auflösung"),
        Some(Criterion::FileSize) => crate::tr("größere Datei"),
        Some(Criterion::MainFolder) => crate::tr("Hauptordner"),
        Some(Criterion::Older) => crate::tr("älter"),
        None => crate::tr("kürzerer Pfad"),
    }
}

fn folder_text(p: &Path) -> String {
    let parts: Vec<String> = p.parent().into_iter().flat_map(|d| d.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>()).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn year_of(p: &Plan) -> String {
    p.members.iter().find_map(|m| m.capture.as_deref().and_then(|c| c.get(0..4)).map(str::to_string)).unwrap_or_default()
}

fn place_of(m: &Member) -> Place {
    if m.network {
        Place::Network
    } else if m.origin == "lightroom" {
        Place::Lightroom
    } else {
        Place::Own
    }
}

/// Own data of a photo as a short line.
fn data_line(m: &Member) -> String {
    let mut v: Vec<String> = Vec::new();
    if m.developed {
        v.push(crate::tr("entwickelt").into());
    }
    if m.copies > 0 {
        v.push(crate::trf!("{a} virt. Kopien", a = m.copies));
    }
    if m.keywords > 0 {
        v.push(crate::trf!("{a} Stichw.", a = m.keywords));
    }
    if m.collections > 0 {
        v.push(crate::trf!("{a} Samml.", a = m.collections));
    }
    if m.persons > 0 {
        v.push(crate::trf!("{a} Pers.", a = m.persons));
    }
    if v.is_empty() {
        crate::tr("keine eigenen Daten").into()
    } else {
        v.join(" · ")
    }
}

fn rating_line(m: &Member) -> String {
    let mut s = m.capture.as_deref().map(|c| c.replace('T', " ")).unwrap_or_else(|| crate::tr("ohne Datum").into());
    if m.stars > 0 {
        s.push_str("  ");
        s.push_str(&"★".repeat(m.stars as usize));
    }
    if m.flag == 1 {
        s.push_str("  ✓");
    } else if m.flag == -1 {
        s.push_str("  ✕");
    }
    s
}

impl LibraryView {
    /// Make the plans for the current duplicate search (in the background), once per search.
    fn dupes_keep_fresh(&mut self, ctx: &egui::Context) {
        let groups = match self.dup.lock().unwrap().as_ref() {
            Some(g) => g.clone(),
            None => return,
        };
        let key = (groups.len(), groups.iter().map(|g| g.files.len()).sum::<usize>());
        {
            let mut l = self.dupes.load.lock().unwrap();
            if l.running || l.made_for == Some(key) {
                return;
            }
            l.running = true;
            l.made_for = Some(key);
        }
        let (load, ctx) = (self.dupes.load.clone(), ctx.clone());
        crate::threads::spawn(crate::threads::Lane::Back, move || {
            let r = Db::read_only(&library::db_path()).map_err(|e| e.to_string()).and_then(|db| dc::plans(&db, &groups));
            let mut l = load.lock().unwrap();
            match r {
                Ok((p, left)) => {
                    l.plans = Some(Arc::new(p));
                    l.left_out = left;
                    l.error = None;
                }
                Err(e) => l.error = Some(e),
            }
            l.running = false;
            l.generation += 1;
            ctx.request_repaint();
        });
    }

    /// New plans: marks from the suggestion (once per search).
    fn dupes_apply(&mut self) -> Option<Arc<Vec<Plan>>> {
        let l = self.dupes.load.lock().unwrap();
        let v = l.plans.clone();
        if l.generation != self.dupes.generation_seen {
            self.dupes.generation_seen = l.generation;
            self.dupes.marked = v.iter().flat_map(|v| v.iter()).flat_map(|p| p.members.iter().zip(&p.suggested).filter(|(_, s)| **s).map(|(m, _)| m.image)).collect();
        }
        v
    }

    /// Search again (after removing or on request).
    fn dupes_research(&mut self) {
        *self.dup.lock().unwrap() = None;
        self.dup_running = false;
        self.dupes.load.lock().unwrap().made_for = None;
        self.show_duplicates();
    }

    fn dupes_visible(&self, plans: &[Plan]) -> Vec<usize> {
        let mut v: Vec<usize> = (0..plans.len())
            .filter(|&i| {
                let p = &plans[i];
                let kind_ok = match self.dupes.kind {
                    KindFilter::All => true,
                    KindFilter::Identical => p.kind == Kind::Identical,
                    KindFilter::Similar => p.kind == Kind::Similar,
                };
                let place_ok = self.dupes.place == Place::All || p.members.iter().any(|m| place_of(m) == self.dupes.place);
                let year_ok = self.dupes.year.as_ref().is_none_or(|y| year_of(p) == *y);
                kind_ok && place_ok && year_ok && (!self.dupes.consequences || p.with_consequences())
            })
            .collect();
        // space: what the marks free, then what the group could free at most
        let freed = |p: &Plan| -> (u64, u64) {
            let marked = p.members.iter().filter(|m| self.dupes.marked.contains(&m.image)).map(|m| m.size.max(0) as u64).sum();
            (marked, p.members.iter().skip(1).map(|m| m.size.max(0) as u64).sum())
        };
        match self.dupes.sort {
            Sort::Space => v.sort_by_key(|&i| std::cmp::Reverse(freed(&plans[i]))),
            Sort::Date => v.sort_by(|&a, &b| plans[b].members[0].capture.cmp(&plans[a].members[0].capture)),
            Sort::Folder => v.sort_by(|&a, &b| plans[a].members[0].path.parent().cmp(&plans[b].members[0].path.parent())),
        }
        v
    }

    /// Removals of the marked photos of the given plans: each against the best staying photo of its group.
    fn dupes_removals(&self, plans: &[Plan], visible: &[usize]) -> Vec<Removal> {
        let mut out = Vec::new();
        for &i in visible {
            let p = &plans[i];
            let Some(keep) = p.members.iter().find(|m| !self.dupes.marked.contains(&m.image) && m.status == 0) else { continue };
            for m in &p.members {
                if self.dupes.marked.contains(&m.image) && m.not_removable().is_none() {
                    out.push(Removal { remove: m.clone(), keep: keep.clone(), identical: p.kind == Kind::Identical });
                }
            }
        }
        out
    }

    /// Bar above the grid while the duplicates are shown as a grid.
    pub(super) fn dupes_banner(&mut self, ui: &mut egui::Ui) {
        if self.source != Source::Duplicates || !self.dupes.grid {
            return;
        }
        egui::Frame::new().fill(crate::lr::gray(0x24)).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::tr("Duplikate als Raster: die Fotos einer Gruppe stehen nebeneinander.")).color(lr::TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tr("Gruppen zeigen")).clicked() {
                        self.dupes.grid = false;
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    /// The group view in the centre of the library.
    pub(super) fn dupes_center(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.dupes_keep_fresh(&ctx);
        let list = self.dupes_apply();
        let (loading, error, left_out) = {
            let l = self.dupes.load.lock().unwrap();
            (l.running, l.error.clone(), l.left_out)
        };
        let searching = self.dup_running || self.dup.lock().unwrap().is_none();
        let busy = self.dupes.ask.is_some();
        // test aid: GRAINEO_DUPES=confirm opens the question once the groups are there
        if !self.dupes.test_done && list.is_some() {
            if let Ok(v) = crate::env::var("GRAINEO_DUPES") {
                self.dupes.test_done = true;
                if v == "confirm" {
                    self.dupes.ask = Some(Ask::Confirm);
                }
            }
        }
        ui.add_space(4.0);
        // the same chip as in the filter row of the grid ("Ähnlich zu: …"): × (or Esc) goes back to the source before
        let mut leave = false;
        ui.horizontal(|ui| {
            let text = self.source_chip().unwrap_or_default();
            let w = ui.painter().layout_no_wrap(text, egui::FontId::proportional(11.0), crate::lr::TEXT_STRONG()).size().x + 30.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
            if let Some((_, true)) = self.paint_source_chip(ui, rect, rect.left(), ui.scope_id().with("duplikate-chip")) {
                leave = true;
            }
            crate::help::info(ui, "duplikate", "Duplikate", &crate::help::DUPLICATES);
        });
        if leave {
            self.leave_special_source();
            return;
        }
        let plans: Arc<Vec<Plan>> = list.clone().unwrap_or_default();
        let n_identical = plans.iter().filter(|p| p.kind == Kind::Identical).count();
        let n_similar = plans.len() - n_identical;
        let head = if let Some(e) = &error {
            crate::trf!("Suche nicht möglich: {f}", f = e)
        } else if searching {
            crate::tr("Duplikate werden gesucht … (Größe, Aufnahmezeit und Prüfsumme, ähnliche über die Bildmerkmale)").to_string()
        } else if list.is_none() || loading {
            crate::tr("Gruppen werden zusammengestellt …").to_string()
        } else if plans.is_empty() {
            crate::tr("Keine Duplikate gefunden.").to_string()
        } else {
            crate::trf!(
                "{a} gleiche und {b} ähnliche Gruppen · {c}",
                a = thousands(n_identical),
                b = thousands(n_similar),
                c = crate::dialog::count(plans.iter().map(|p| p.members.len()).sum(), crate::tr("Foto"), crate::tr("Fotos"))
            )
        };
        ui.horizontal(|ui| {
            if searching || loading {
                ui.add(egui::Spinner::new().size(12.0));
            }
            ui.label(egui::RichText::new(head).color(lr::TEXT()));
        });
        if left_out > 0 {
            ui.label(egui::RichText::new(crate::trf!("{a} nur mit Fotos aus Apple Fotos ausgeblendet (Apple Fotos wird nie verändert)", a = crate::dialog::count(left_out, crate::tr("Gruppe"), crate::tr("Gruppen")))).small().color(lr::TEXT_DIM()));
        }
        ui.add_space(6.0);
        // filters
        ui.horizontal_wrapped(|ui| {
            let all = crate::trf!("Alle {a}", a = thousands(plans.len()));
            let ident = crate::trf!("Gleich {a}", a = thousands(n_identical));
            let sim = crate::trf!("Ähnlich {a}", a = thousands(n_similar));
            lr::segmented(ui, &mut self.dupes.kind, &[(KindFilter::All, all.as_str()), (KindFilter::Identical, ident.as_str()), (KindFilter::Similar, sim.as_str())]);
            ui.add_space(8.0);
            lr::segmented(ui, &mut self.dupes.place, &[(Place::All, "Alle Orte"), (Place::Lightroom, "Lightroom-Ordner"), (Place::Own, "Eigene Ordner"), (Place::Network, "Netzlaufwerk")]);
            ui.add_space(8.0);
            let mut years: Vec<String> = plans.iter().map(year_of).filter(|y| !y.is_empty()).collect();
            years.sort_unstable_by(|a, b| b.cmp(a));
            years.dedup();
            let label = self.dupes.year.clone().unwrap_or_else(|| crate::tr("Alle Jahre").into());
            egui::ComboBox::from_id_salt("dupes-jahr").selected_text(label).width(96.0).show_ui(ui, |ui| {
                if ui.selectable_label(self.dupes.year.is_none(), crate::tr("Alle Jahre")).clicked() {
                    self.dupes.year = None;
                }
                for y in &years {
                    if ui.selectable_label(self.dupes.year.as_ref() == Some(y), y.as_str()).clicked() {
                        self.dupes.year = Some(y.clone());
                    }
                }
            });
            ui.add_space(8.0);
            let r = lr::toggle(ui, &mut self.dupes.consequences, crate::tr("Nur mit Folgen"));
            lr::hover_tip(ui, &r, crate::tr("Nur Gruppen, in denen mehr als ein Foto Bewertung, Stichwörter oder eine Entwicklung hat"));
            ui.add_space(8.0);
            lr::segmented(ui, &mut self.dupes.sort, &[(Sort::Space, "Nach Speicher"), (Sort::Date, "Nach Datum"), (Sort::Folder, "Nach Ordner")]);
        });
        ui.add_space(6.0);
        let visible = self.dupes_visible(&plans);
        let removals = self.dupes_removals(&plans, &visible);
        let bytes: u64 = removals.iter().map(|r| r.remove.size.max(0) as u64).sum();
        ui.horizontal(|ui| {
            let label = if removals.is_empty() { crate::tr("In den Papierkorb …").to_string() } else { crate::trf!("{a} in den Papierkorb …", a = crate::dialog::count(removals.len(), crate::tr("Foto"), crate::tr("Fotos"))) };
            let r = lr::main_button(ui, &label, !removals.is_empty() && !busy && !self.pairs_running());
            lr::hover_tip(ui, &r, crate::tr("Die markierten Fotos der sichtbaren Gruppen in den Papierkorb legen; je Gruppe bleibt mindestens ein Foto"));
            if r.clicked() {
                self.dupes.ask = Some(Ask::Confirm);
            }
            ui.add_space(8.0);
            let r = ui.add_enabled(!visible.is_empty(), egui::Button::new(crate::tr("Vorschlag")));
            lr::hover_tip(ui, &r, crate::tr("Markierungen der sichtbaren Gruppen auf den Vorschlag zurücksetzen"));
            if r.clicked() {
                for &i in &visible {
                    for (m, s) in plans[i].members.iter().zip(&plans[i].suggested) {
                        if *s {
                            self.dupes.marked.insert(m.image);
                        } else {
                            self.dupes.marked.remove(&m.image);
                        }
                    }
                }
            }
            let r = ui.add_enabled(!removals.is_empty(), egui::Button::new(crate::tr("Keine")));
            lr::hover_tip(ui, &r, crate::tr("In den sichtbaren Gruppen nichts zum Entfernen markieren"));
            if r.clicked() {
                for &i in &visible {
                    for m in &plans[i].members {
                        self.dupes.marked.remove(&m.image);
                    }
                }
            }
            ui.add_space(8.0);
            if ui.button(crate::tr("Als Raster")).clicked() {
                self.dupes.grid = true;
                self.refilter = true;
            }
            if ui.add_enabled(!searching && !loading && !busy, egui::Button::new(crate::tr("Neu suchen"))).clicked() {
                self.dupes_research();
            }
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(crate::trf!(
                "{a} sichtbar · {b} zum Entfernen markiert · {c} würden frei",
                a = crate::dialog::count(visible.len(), crate::tr("Gruppe"), crate::tr("Gruppen")),
                b = crate::dialog::count(removals.len(), crate::tr("Foto"), crate::tr("Fotos")),
                c = if bytes == 0 { "0 MB".to_string() } else { crate::dialog::size(bytes) }
            ))
            .small()
            .color(lr::TEXT_DIM()),
        );
        ui.add_space(4.0);
        ui.separator();
        self.dupes_groups(ui, &plans, &visible);
    }

    /// The groups, drawn only where they are visible (variable heights: several lines of photos per group).
    fn dupes_groups(&mut self, ui: &mut egui::Ui, plans: &Arc<Vec<Plan>>, visible: &[usize]) {
        let width = ui.available_width();
        let per_line = (((width - 8.0) / (CARD_W + GAP)).floor() as usize).max(1);
        let mut tops = Vec::with_capacity(visible.len() + 1);
        let mut y = 0.0f32;
        for &i in visible {
            tops.push(y);
            let lines = plans[i].members.len().div_ceil(per_line);
            y += HEAD_H + lines as f32 * (CARD_H + GAP) + 10.0;
        }
        tops.push(y);
        let total = y;
        let mut toggles: Vec<(usize, i64, bool)> = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show_viewport(ui, |ui, viewport| {
            ui.set_height(total.max(1.0));
            let origin = ui.max_rect().min;
            let first = tops.partition_point(|&t| t < viewport.min.y).saturating_sub(1);
            let mut wanted: Vec<i64> = Vec::new();
            for k in first..visible.len() {
                if tops[k] > viewport.max.y {
                    break;
                }
                let p = &plans[visible[k]];
                wanted.extend(p.members.iter().map(|m| m.image));
                let rect = egui::Rect::from_min_max(origin + egui::vec2(0.0, tops[k]), origin + egui::vec2(width, tops[k + 1]));
                self.dupes_group(ui, rect, visible[k], p, per_line, &mut toggles);
            }
            self.request_thumbnails(&wanted);
        });
        for (gi, image, only) in toggles {
            let p = &plans[gi];
            if only {
                // keep only this one: all others that may go are marked
                for m in &p.members {
                    if m.image == image || m.not_removable().is_some() {
                        self.dupes.marked.remove(&m.image);
                    } else {
                        self.dupes.marked.insert(m.image);
                    }
                }
            } else if !self.dupes.marked.remove(&image) {
                // at least one reachable photo of the group stays
                let stays = p.members.iter().filter(|m| m.image != image && m.status == 0 && !self.dupes.marked.contains(&m.image)).count();
                if stays > 0 {
                    self.dupes.marked.insert(image);
                }
            }
        }
    }

    fn dupes_group(&mut self, ui: &mut egui::Ui, rect: egui::Rect, gi: usize, p: &Plan, per_line: usize, toggles: &mut Vec<(usize, i64, bool)>) {
        let painter = ui.painter_at(rect);
        let small = egui::TextStyle::Small.resolve(ui.style());
        let body = egui::TextStyle::Body.resolve(ui.style());
        let marked_bytes: u64 = p.members.iter().filter(|m| self.dupes.marked.contains(&m.image)).map(|m| m.size.max(0) as u64).sum();
        let (kind, why) = match p.kind {
            Kind::Identical if p.verified => (crate::tr("GLEICH"), crate::tr("Größe, Aufnahmezeit, Prüfsumme").to_string()),
            Kind::Identical => (crate::tr("GLEICH?"), crate::tr("nicht erreichbar, nichts wird entfernt").to_string()),
            Kind::Similar => (crate::tr("ÄHNLICH"), crate::trf!("Bild {a} % gleich, gleicher Tag, nur von Hand", a = (p.score * 100.0).floor() as i32)),
        };
        let kind_color = if p.kind == Kind::Identical { Color32::from_rgb(0x4c, 0xc2, 0x6a) } else { Color32::from_rgb(0xe0, 0xb0, 0x40) };
        let g = painter.layout_no_wrap(kind.to_string(), small.clone(), kind_color);
        let kw = g.size().x;
        painter.galley(egui::pos2(rect.left() + 4.0, rect.top() + 6.0), g, kind_color);
        let right = if marked_bytes > 0 { crate::trf!("{a} würden frei", a = crate::dialog::size(marked_bytes)) } else { String::new() };
        painter.text(egui::pos2(rect.right() - 8.0, rect.top() + 6.0), egui::Align2::RIGHT_TOP, &right, small.clone(), lr::TEXT());
        let clip = ui.painter_at(egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 140.0, rect.top() + HEAD_H)));
        clip.text(egui::pos2(rect.left() + 12.0 + kw, rect.top() + 6.0), egui::Align2::LEFT_TOP, format!("· {why}"), small.clone(), lr::TEXT_DIM());
        for (j, m) in p.members.iter().enumerate() {
            let (line, col) = (j / per_line, j % per_line);
            let card = egui::Rect::from_min_size(egui::pos2(rect.left() + 4.0 + col as f32 * (CARD_W + GAP), rect.top() + HEAD_H + line as f32 * (CARD_H + GAP)), egui::vec2(CARD_W, CARD_H));
            let resp = ui.interact(card, ui.scope_id().with(("dupe", gi, m.image)), egui::Sense::click());
            let marked = self.dupes.marked.contains(&m.image);
            let fill = if resp.hovered() { crate::lr::gray(0x2a) } else { crate::lr::gray(0x20) };
            painter.rect_filled(card, 0.0, fill);
            // state (coloured) and its reason (dimmed, own line: the card is narrow)
            let (state, reason, state_color) = if marked {
                // the staying photo of the group and what it has ahead of this one
                let keep = p.members.iter().find(|k| !self.dupes.marked.contains(&k.image) && k.status == 0);
                let adv = keep.map(|k| dc::decisive(k, m)).unwrap_or(None);
                (crate::tr("ENTFERNEN").to_string(), crate::trf!("bleibendes: {a}", a = advantage(adv)), Color32::from_rgb(0xe0, 0x6c, 0x5c))
            } else if let Some(why) = m.not_removable().filter(|_| j > 0) {
                (crate::tr("bleibt").to_string(), crate::menu::i18n::tr_msg(why).into_owned(), lr::TEXT())
            } else if j == 0 {
                let adv = p.members.get(1).map(|o| dc::decisive(m, o)).unwrap_or(None);
                (crate::tr("BLEIBT").to_string(), crate::trf!("Vorteil: {a}", a = advantage(adv)), Color32::from_rgb(0x4c, 0xc2, 0x6a))
            } else {
                (crate::tr("bleibt").to_string(), String::new(), lr::TEXT())
            };
            if marked {
                painter.rect_stroke(card, 0.0, egui::Stroke::new(1.5, Color32::from_rgb(0xb0, 0x50, 0x44)), egui::StrokeKind::Inside);
            }
            let thumb = egui::Rect::from_min_size(card.min + egui::vec2(6.0, 6.0), egui::vec2(CARD_W - 12.0, THUMB_H - 6.0));
            painter.rect_filled(thumb, 0.0, crate::lr::gray(0x16));
            if let Some(t) = self.thumbnail(m.file) {
                let s = t.size_vec2();
                if s.x > 0.0 && s.y > 0.0 {
                    let k = (thumb.width() / s.x).min(thumb.height() / s.y);
                    let r = egui::Rect::from_center_size(thumb.center(), s * k);
                    let tint = if marked { Color32::from_gray(0x80) } else { Color32::WHITE };
                    painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
                }
            }
            let text = ui.painter_at(card.shrink(6.0));
            let x = card.left() + 6.0;
            let mut ty = card.top() + THUMB_H + 4.0;
            let mut line_at = |t: String, font: &egui::FontId, color: Color32| {
                text.text(egui::pos2(x, ty), egui::Align2::LEFT_TOP, t, font.clone(), color);
                ty += 15.0;
            };
            line_at(m.name.clone(), &body, lr::TEXT());
            line_at(folder_text(&m.path), &small, lr::TEXT_DIM());
            let res = match (m.width, m.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => format!("{w}×{h}"),
                _ => "–".into(),
            };
            line_at(format!("{} · {res} · {}", m.format, crate::dialog::size(m.size.max(0) as u64)), &small, lr::TEXT());
            line_at(rating_line(m), &small, lr::TEXT_DIM());
            line_at(data_line(m), &small, lr::TEXT_DIM());
            line_at(state, &small, state_color);
            line_at(reason, &small, lr::TEXT_DIM());
            let tip = crate::trf!(
                "{a}\n\nKlick: zum Entfernen markieren oder wieder behalten · Doppelklick: nur dieses behalten",
                a = m.path.display()
            );
            lr::hover_tip(ui, &resp, &tip);
            if resp.double_clicked() {
                toggles.push((gi, m.image, true));
            } else if resp.clicked() && (marked || m.not_removable().is_none()) {
                toggles.push((gi, m.image, false));
            }
            resp.context_menu(|ui| {
                if lr::menu_item(ui, crate::tr("Nur dieses behalten"), "", true) {
                    toggles.push((gi, m.image, true));
                }
                if lr::menu_item(ui, crate::tr("Im Finder zeigen"), "", m.status == 0) {
                    crate::system::show(&m.path);
                }
            });
        }
    }

    /// Question, progress and result (every frame from `after`).
    pub(super) fn dupes_dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::dialog::{self, Answer, Buttons};
        let Some(ask) = self.dupes.ask.take() else { return };
        match ask {
            Ask::Confirm => {
                let plans = self.dupes.load.lock().unwrap().plans.clone().unwrap_or_default();
                let visible = self.dupes_visible(&plans);
                let chosen = self.dupes_removals(&plans, &visible);
                let n = chosen.len();
                let bytes: u64 = chosen.iter().map(|r| r.remove.size.max(0) as u64).sum();
                let with_data = chosen.iter().filter(|r| r.remove.has_data()).count();
                let similar = chosen.iter().filter(|r| !r.identical).count();
                let groups: HashSet<i64> = chosen.iter().map(|r| r.keep.image).collect();
                let mut transfer = self.dupes.transfer;
                let title = crate::trf!("{a} in den Papierkorb legen?", a = dialog::count(n, crate::tr("Duplikat"), crate::tr("Duplikate")));
                let a = dialog::show(
                    ctx,
                    "duplikate-entfernen",
                    &title,
                    dialog::WIDTH,
                    |ui| {
                        dialog::text(ui, &crate::trf!("Aus {a} bleibt jeweils das Foto, das nicht markiert ist. Die markierten verschwinden aus der Bibliothek und kommen in den Papierkorb.", a = dialog::count(groups.len(), crate::tr("Gruppe"), crate::tr("Gruppen"))));
                        dialog::chip(ui, &crate::trf!("{a} · {b}", a = dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")), b = dialog::size(bytes)));
                        if similar > 0 {
                            dialog::warning(ui, &crate::trf!("{a} davon aus „ähnlich“-Gruppen: nicht inhaltsgleich (z. B. Serienbild oder Export). Bitte sicher sein.", a = thousands(similar)));
                        }
                        dialog::options(ui, |ui| {
                            dialog::option(
                                ui,
                                &mut transfer,
                                crate::tr("Daten aufs bleibende Foto übertragen"),
                                &crate::trf!("Bewertung, Stichwörter, Sammlungen, Personen: {a} (nur ergänzt)", a = dialog::count(with_data, crate::tr("Foto"), crate::tr("Fotos"))),
                                None,
                            );
                        });
                        if !transfer && with_data > 0 {
                            dialog::hint(ui, crate::tr("Ohne Übertragen bleiben Fotos mit Daten, die dem bleibenden fehlen, stehen."));
                        }
                        dialog::hint(ui, crate::tr("Gleiche Fotos werden vorher vollständig verglichen (SHA-256); weicht der Inhalt ab, bleibt das Foto. Fotos mit eigener Entwicklung, aus Apple Fotos oder nicht erreichbare bleiben immer."));
                        dialog::hint(ui, crate::tr("Wiederherstellen: ⌘Z holt die Bibliothekseinträge zurück, die Dateien im Papierkorb mit „Zurücklegen“ (Netzlaufwerk: Papierkorb-Ordner des Laufwerks)."));
                    },
                    Buttons::destructive(crate::tr("In den Papierkorb"), n > 0),
                );
                self.dupes.transfer = transfer;
                match a {
                    Answer::Primary if n > 0 => {
                        self.dupes_start_run(ctx, chosen);
                        self.dupes.ask = Some(Ask::Running);
                    }
                    Answer::Cancel => {}
                    _ => self.dupes.ask = Some(Ask::Confirm),
                }
            }
            Ask::Running => {
                let (phase, done, total, started, outcome) = {
                    let mut r = self.dupes.run.lock().unwrap();
                    (r.phase, r.done, r.total, r.phase_start, r.outcome.take())
                };
                if let Some(mut o) = outcome {
                    let snap = std::mem::take(&mut o.snapshot);
                    if o.removed > 0 {
                        let label = crate::trf!("{a} entfernt", a = crate::dialog::count(o.removed, crate::tr("Duplikat"), crate::tr("Duplikate")));
                        self.manage.push(library_manage::Undo::Snap { label: label.clone(), snap, keywords: false, smart: false });
                        actions.push(Action::Status(label));
                    }
                    self.reload();
                    self.dupes_research();
                    self.dupes.ask = Some(Ask::Done(Shown { removed: o.removed, bytes: o.bytes, transferred: o.transferred, skipped: o.skipped, errors: o.errors, cancelled: o.cancelled }));
                    ctx.request_repaint();
                    return;
                }
                let cancelling = self.dupes.cancel.load(Ordering::Relaxed);
                let a = dialog::show(
                    ctx,
                    "duplikate-laeuft",
                    crate::tr("Duplikate werden entfernt …"),
                    dialog::WIDTH,
                    |ui| {
                        let f = if total > 0 { Some(done as f32 / total as f32) } else { None };
                        let what = match phase {
                            Some(Phase::Trash) => crate::tr("in den Papierkorb"),
                            _ => crate::tr("wird geprüft"),
                        };
                        let mut t = if cancelling { crate::tr("wird abgebrochen …").to_string() } else { crate::trf!("{w}: {a} von {b}", w = what, a = thousands(done), b = thousands(total)) };
                        if let Some(s) = started.filter(|_| done >= 3 && done < total && !cancelling) {
                            let rest = s.elapsed().mul_f64((total - done) as f64 / done as f64);
                            t = crate::trf!("{t} · noch etwa {d}", t = t, d = dialog::duration(rest));
                        }
                        dialog::progress(ui, f, &t);
                    },
                    Buttons { cancel: crate::tr("Abbrechen"), primary: None },
                );
                if a == Answer::Cancel {
                    self.dupes.cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                self.dupes.ask = Some(Ask::Running);
            }
            Ask::Done(o) => {
                let verdict = if !o.errors.is_empty() { dialog::Verdict::Red } else if !o.skipped.is_empty() || o.cancelled { dialog::Verdict::Yellow } else { dialog::Verdict::Green };
                let mut parts = vec![crate::trf!("{a} in den Papierkorb gelegt ({b})", a = dialog::count(o.removed, crate::tr("Foto"), crate::tr("Fotos")), b = dialog::size(o.bytes))];
                if o.transferred > 0 {
                    parts.push(crate::trf!("{a}: Bewertungen/Stichwörter aufs bleibende Foto übertragen", a = thousands(o.transferred)));
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
                if o.removed > 0 {
                    parts.push(crate::tr("⌘Z holt die Bibliothekseinträge zurück.").to_string());
                }
                let details: Vec<String> = o.errors.iter().map(|e| crate::trf!("Fehler: {f}", f = e)).chain(o.skipped.iter().map(|(n, r)| format!("{n}: {}", crate::menu::i18n::tr_msg(r)))).collect();
                let a = dialog::show(ctx, "duplikate-ergebnis", crate::tr("Duplikate entfernt"), dialog::WIDTH, |ui| dialog::outcome(ui, verdict, &parts.join("\n"), &details), Buttons::close());
                if a == Answer::Open {
                    self.dupes.ask = Some(Ask::Done(o));
                }
            }
        }
    }

    fn dupes_start_run(&mut self, ctx: &egui::Context, chosen: Vec<Removal>) {
        {
            let mut r = self.dupes.run.lock().unwrap();
            *r = RunState { running: true, phase: Some(Phase::Check), done: 0, total: chosen.len(), phase_start: Some(std::time::Instant::now()), outcome: None };
        }
        self.dupes.cancel.store(false, Ordering::Relaxed);
        let (run, cancel, ctx) = (self.dupes.run.clone(), self.dupes.cancel.clone(), ctx.clone());
        let opt = dc::Options { transfer: self.dupes.transfer };
        // the user waits for it: not the lowest priority
        crate::threads::spawn(crate::threads::Lane::Ahead, move || {
            let t0 = std::time::Instant::now();
            let outcome = match Db::standard() {
                Ok(db) => {
                    let local = |files: &[PathBuf]| trash::delete_all(files).map_err(|e| e.to_string());
                    let root_of = |p: &Path| library::pairs::root_of(&db, p);
                    let targets = library::trash_batch::Targets { local: &local, is_network: &library::network_trash::is_network, root_of: &root_of, day: library::card_import::today() };
                    let progress = |phase: Phase, done: usize, total: usize| {
                        let mut r = run.lock().unwrap();
                        if r.phase != Some(phase) {
                            r.phase = Some(phase);
                            r.phase_start = Some(std::time::Instant::now());
                        }
                        r.done = done;
                        r.total = total;
                        drop(r);
                        ctx.request_repaint();
                    };
                    dc::remove(&db, &chosen, opt, &targets, &progress, &|| cancel.load(Ordering::Relaxed))
                }
                Err(e) => Outcome { errors: vec![e.to_string()], ..Default::default() },
            };
            crate::log(&format!(
                "duplicates: removed {}, transferred {}, skipped {}, errors {}, {:.1} s",
                outcome.removed,
                outcome.transferred,
                outcome.skipped.len(),
                outcome.errors.len(),
                t0.elapsed().as_secs_f64()
            ));
            let mut r = run.lock().unwrap();
            r.running = false;
            r.outcome = Some(outcome);
            ctx.request_repaint();
        });
    }

}
