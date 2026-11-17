//! Settings (⌘,) as a window of its own in Mac style (modelled on the macOS System Settings):
//! toolbar with short names, per page groups with a capitals heading in a box;
//! per row name on the left (fixed width), control on the right, at most one short grey line
//! below. On/off = toggle, choice = segment, paths = field + "Choose …", actions on the right at the
//! end of the group, states as grey values with a dot (green/yellow/red).
//!
//! Everything expensive (counting the database, folder sizes, module state, searching locally installed lens profiles) is computed by a
//! background thread on open ([`State`]); until then "determining …" is shown. Writing to
//! the database also runs in the background. Values live in `settings.json` or the small
//! files in the data folder. Fixed size, no jumps; help only via "?" cards.

use crate::menu::i18n::DeComma as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[path = "prefs_i18n.rs"]
mod localized;
#[path = "prefs_cache.rs"]
mod cache_limit_ui;
use crate::menu::i18n::{self, Text};

use eframe::egui;
use egui::Color32;
use serde_json::json;

use crate::lr;

static OPEN: AtomicBool = AtomicBool::new(false);
static SECTION: AtomicUsize = AtomicUsize::new(0);
/// Last measured usage (SPP cache: bytes, images, paused); `None` = measure again
static STATS: Mutex<Option<(u64, usize, bool)>> = Mutex::new(None);
/// States determined in the background (None = being determined)
static STATE: Mutex<Option<State>> = Mutex::new(None);
static STATE_RUNNING: AtomicBool = AtomicBool::new(false);
/// Trash on network drives: per root folder (path, files, bytes); None = being determined
static NET_BIN: Mutex<Option<Vec<(String, usize, u64)>>> = Mutex::new(None);
static NET_BIN_RUNNING: AtomicBool = AtomicBool::new(false);
/// "Empty" is waiting for confirmation
static NET_BIN_QUESTION: AtomicBool = AtomicBool::new(false);
static NET_BIN_MESSAGE: Mutex<String> = Mutex::new(String::new());

/// Read the trash folders of the reachable network drives in the background (a network drive can be slow).
fn net_bin_reload() {
    if NET_BIN_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let mut v = Vec::new();
        if let Ok(db) = library::db::Db::standard() {
            for st in db.roots().unwrap_or_default() {
                let p = std::path::Path::new(&st.path);
                if p.is_dir() && library::network_trash::is_network(p) {
                    let (n, b) = library::network_trash::content(p);
                    if n > 0 {
                        v.push((st.path.clone(), n, b));
                    }
                }
            }
        }
        *NET_BIN.lock().unwrap() = Some(v);
        NET_BIN_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Action on all trash folders in the background (put back or empty), then determine again.
fn net_bin_action(empty: bool) {
    let roots: Vec<String> = NET_BIN.lock().unwrap().clone().unwrap_or_default().into_iter().map(|(p, _, _)| p).collect();
    *NET_BIN.lock().unwrap() = None;
    std::thread::spawn(move || {
        let (mut n, mut messages) = (0usize, Vec::<String>::new());
        for st in &roots {
            let p = std::path::Path::new(st);
            if empty {
                match library::network_trash::clear(p) {
                    Ok(k) => n += k,
                    Err(e) => messages.push(e),
                }
            } else {
                let (k, m) = library::network_trash::restore_all(p);
                n += k;
                messages.extend(m);
            }
        }
        let mut t = if empty { crate::trf!("{a} Dateien endgültig gelöscht.", a = n) } else { crate::trf!("{a} Dateien zurückgelegt; sie erscheinen beim nächsten Abgleich der Netzlaufwerke (spätestens nach 5 Minuten) wieder in der Bibliothek.", a = n) };
        if !messages.is_empty() {
            t += &format!(" {}", messages.iter().take(3).cloned().collect::<Vec<_>>().join(" · "));
        }
        *NET_BIN_MESSAGE.lock().unwrap() = t;
        NET_BIN_RUNNING.store(false, Ordering::SeqCst);
    });
    NET_BIN_RUNNING.store(true, Ordering::SeqCst);
}

/// What the pages show and is expensive to determine.
#[derive(Clone, Default)]
struct State {
    lr_catalog: String,
    backup_target: String,
    /// second target (e.g. the NAS), empty: none
    backup_target2: String,
    backups: usize,
    /// daily backups of both targets (newest first) with their size, for "Wiederherstellen"
    backup_list: Vec<(std::path::PathBuf, u64)>,
    /// backup restored at the next start, if one was chosen
    restore_pending: Option<std::path::PathBuf>,
    ai_count: i64,
    vectors: i64,
    text_vectors: i64,
    module: Vec<(&'static str, library::module::Status, u64)>,
    adobe: Option<String>,
    own_profiles: usize,
    /// User lens profiles: file and display name
    own_list: Vec<(std::path::PathBuf, String)>,
    sizes: Vec<(&'static str, u64)>,
    free: u64,
    spp: (u64, usize, bool),
    duration_ms: u128,
}

/// Determine states again in the background.
fn state_new() {
    if STATE_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let t = std::time::Instant::now();
        let mut z = State::default();
        if let Ok(db) = library::db::Db::standard() {
            z.lr_catalog = library::lightroom_catalog(&db).map(|p| p.display().to_string()).unwrap_or_else(|| crate::tr("nicht gewählt").to_string());
            let target = library::backup::target(&db);
            z.backups = library::backup::list(&target).len();
            z.backup_target = target.display().to_string();
            z.backup_target2 = library::backup::second_target(&db).map(|p| p.display().to_string()).unwrap_or_default();
            let with_size = |v: Vec<std::path::PathBuf>| v.into_iter().map(|p| { let n = std::fs::metadata(&p).map_or(0, |m| m.len()); (p, n) }).collect::<Vec<_>>();
            z.backup_list = with_size(library::backup::list(&target));
            if let Some(t2) = library::backup::second_target(&db).filter(|t| t.is_dir()) {
                z.backup_list.extend(with_size(library::backup::list(&t2)));
            }
            z.restore_pending = library::backup::pending_restore(&db.path);
            let number = |sql: &str| db.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
            z.ai_count = number("SELECT count(*) FROM ai");
            z.vectors = number("SELECT count(*) FROM image_vector");
            z.text_vectors = number("SELECT count(*) FROM text_vector");
        }
        z.module = library::module::ALL.iter().map(|d| {
            let (st, g) = library::module::status(d);
            (d.id, st, g)
        }).collect();
        z.adobe = library::module::adobe_lcp().map(|p| p.display().to_string());
        z.own_profiles = std::fs::read_dir(library::data_dir().join("lens-profiles/own")).map(|r| r.count()).unwrap_or(0);
        z.own_list = std::fs::read_dir(library::data_dir().join("lens-profiles/own"))
            .map(|r| {
                let mut v: Vec<(std::path::PathBuf, String)> = r
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                    .map(|p| {
                        let name = std::fs::read_to_string(&p)
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                            .and_then(|mut j| {
                                legacy_names::settings_keys::upgrade_lens_profile(&mut j);
                                Some(format!("{} · {}", j.get("model")?.as_str()?, j.get("lens")?.as_str()?))
                            })
                            .unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
                        (p, name)
                    })
                    .collect();
                v.sort();
                v
            })
            .unwrap_or_default();
        let d = library::data_dir();
        z.sizes = vec![
            ("Vorschauen", folder_size(&library::cache_folder())),
            ("Bibliothek", std::fs::metadata(d.join("library.sqlite")).map_or(0, |m| m.len())),
            ("Sicherungen", folder_size(&d.join("backups"))),
            ("Module", folder_size(&d.join("modules"))),
            ("Journal", folder_size(&d.join("journal"))),
        ];
        z.free = library::free(&d).unwrap_or(0);
        let (b, n) = develop::diskcache::stats();
        z.spp = (b, n, develop::diskcache::paused());
        z.duration_ms = t.elapsed().as_millis();
        if crate::env::var_os("GRAINEO_LOG").is_some() {
            eprintln!("settings: state determined in {} ms", z.duration_ms);
        }
        *STATE.lock().unwrap() = Some(z);
        STATE_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Determined state or None (then show "determining …" and redraw).
fn state(ui: &egui::Ui) -> Option<State> {
    let z = STATE.lock().unwrap().clone();
    if z.is_none() {
        state_new();
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
    }
    z
}

/// Write a database setting in the background (the UI never waits for the DB).
fn set_db(k: &'static str, v: String) {
    std::thread::spawn(move || {
        // never silently lost: the error shows under the backup section (the view re-reads the stored value)
        if let Err(e) = library::db::Db::standard().and_then(|db| db.set_info(k, &v)) {
            crate::log(&format!("setting {k} not saved: {e}"));
            *RESTORE_MESSAGE.lock().unwrap_or_else(|x| x.into_inner()) = crate::trf!("Einstellung nicht gespeichert: {e}", e = e);
        }
        *STATE.lock().unwrap_or_else(|x| x.into_inner()) = None;
    });
}
/// Running background work (installing a module, counting/writing sidecars, backup), one entry per kind of work
/// (`what`). Modules have their own entries (`what` = module ID), so one model can download while another is removed;
/// all other work of the settings still runs one at a time (see [`running`]).
static WORK: Mutex<Vec<Work>> = Mutex::new(Vec::new());
/// Number of the next work (a finished thread only reports to its own entry).
static WORK_NR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone)]
struct Work {
    nr: u64,
    what: String,
    /// ended with an error (`text` says which)
    failed: bool,
    progress: f32,
    text: String,
    done: bool,
    cancel: Arc<AtomicBool>,
    /// States determined again after the end
    state_new: bool,
    /// start (for "running for …" where other buttons wait for it)
    started: std::time::Instant,
}

const SECTIONS: [&str; 11] = ["Allgemein", "Oberfläche", "Bibliothek", "Entwickeln", "Export", "Objektive", "Suche", "Module", "Speicher", "Looks", "Browser"];
/// Icon per section (number in [`symbol`])
const SYMBOL_NR: [usize; 11] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 2, 9];

/// UI sizes (scales everything evenly: font, spacing, icons).
const SCALES: [(f32, &str); 3] = [(0.9, "Klein"), (1.0, "Normal"), (1.15, "Groß")];

fn scale_file() -> Option<std::path::PathBuf> {
    crate::app_file("interface.txt")
}

/// Apply the saved UI size (at startup).
pub fn apply_scale(ctx: &egui::Context) {
    // ⌘+/⌘− from egui off: the size is only chosen in the settings
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let f = scale_file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.trim().parse::<f32>().ok()).unwrap_or(1.0);
    if SCALES.iter().any(|(v, _)| (*v - f).abs() < 1e-3) {
        ctx.set_zoom_factor(f);
    }
}

/// Are the settings open? They are an ordinary egui window; the native image layer of the wide display would otherwise lie above them.
pub fn opened() -> bool {
    OPEN.load(Ordering::Relaxed)
}

pub fn open() {
    *STATS.lock().unwrap() = None;
    state_new();
    OPEN.store(true, Ordering::Relaxed);
}

/// Test hook: preselect a section (`GRAINEO_TAB=prefs:Module`).
pub fn open_section(name: &str) {
    if let Some(i) = SECTIONS.iter().position(|b| b.eq_ignore_ascii_case(name) || b.starts_with(name)) {
        SECTION.store(i, Ordering::Relaxed);
    }
    open();
}

fn gb(bytes: u64) -> String {
    let v = bytes as f64 / 1e9;
    let s = if v >= 10.0 { format!("{v:.0}") } else if v >= 0.1 { format!("{v:.1}") } else { format!("{:.0} MB", bytes as f64 / 1e6) };
    if v < 0.1 {
        return s;
    }
    format!("{} GB", s.de_comma())
}

fn folder_size(p: &std::path::Path) -> u64 {
    let mut n = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => n += e.metadata().map_or(0, |m| m.len()),
                _ => {}
            }
        }
    }
    n
}

/// Icon per section (own vector graphics, no third-party assets).
fn symbol(p: &egui::Painter, r: egui::Rect, i: usize, c: Color32) {
    let s = egui::Stroke::new(1.4, c);
    let m = r.center();
    let w = r.width() * 0.36;
    match i {
        0 => {
            // Gear
            p.circle_stroke(m, w * 0.55, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let d = egui::vec2(a.cos(), a.sin());
                p.line_segment([m + d * w * 0.65, m + d * w * 0.95], s);
            }
        }
        1 => {
            // Window
            let q = egui::Rect::from_center_size(m, egui::vec2(w * 2.0, w * 1.5));
            p.rect_stroke(q, 0.0, s, egui::StrokeKind::Middle);
            p.line_segment([egui::pos2(q.left(), q.top() + w * 0.4), egui::pos2(q.right(), q.top() + w * 0.4)], s);
        }
        2 => {
            // Image stack
            for k in 0..3 {
                let q = egui::Rect::from_center_size(m + egui::vec2(k as f32 * 3.0 - 3.0, -(k as f32) * 3.0 + 3.0), egui::vec2(w * 1.5, w * 1.1));
                p.rect_stroke(q, 0.0, s, egui::StrokeKind::Middle);
            }
        }
        3 => {
            // Sliders
            for k in 0..3 {
                let y = m.y - w * 0.7 + k as f32 * w * 0.7;
                p.line_segment([egui::pos2(m.x - w, y), egui::pos2(m.x + w, y)], s);
                p.circle_filled(egui::pos2(m.x - w * 0.5 + k as f32 * w * 0.6, y), 2.6, c);
            }
        }
        4 => {
            // Box with arrow up
            let q = egui::Rect::from_center_size(m + egui::vec2(0.0, w * 0.35), egui::vec2(w * 1.8, w * 1.0));
            p.rect_stroke(q, 0.0, s, egui::StrokeKind::Middle);
            p.line_segment([m + egui::vec2(0.0, w * 0.5), m - egui::vec2(0.0, w)], s);
            p.line_segment([m - egui::vec2(0.0, w), m - egui::vec2(w * 0.4, w * 0.6)], s);
            p.line_segment([m - egui::vec2(0.0, w), m + egui::vec2(w * 0.4, -w * 0.6)], s);
        }
        5 => {
            // Lens
            p.circle_stroke(m, w, s);
            p.circle_stroke(m, w * 0.55, s);
            p.circle_filled(m, w * 0.18, c);
        }
        6 => {
            // Magnifier
            p.circle_stroke(m - egui::vec2(w * 0.25, w * 0.25), w * 0.6, s);
            p.line_segment([m + egui::vec2(w * 0.2, w * 0.2), m + egui::vec2(w * 0.9, w * 0.9)], egui::Stroke::new(2.0, c));
        }
        7 => {
            // four blocks
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                p.rect_stroke(egui::Rect::from_center_size(m + egui::vec2(dx * w * 0.5, dy * w * 0.5), egui::vec2(w * 0.8, w * 0.8)), 0.0, s, egui::StrokeKind::Middle);
            }
        }
        9 => {
            // Tablet
            let q = egui::Rect::from_center_size(m, egui::vec2(w * 1.5, w * 2.0));
            p.rect_stroke(q, 3.0, s, egui::StrokeKind::Middle);
            p.circle_filled(egui::pos2(m.x, q.bottom() - 4.0), 1.4, c);
        }
        _ => {
            // Disk
            let q = egui::Rect::from_center_size(m, egui::vec2(w * 1.6, w * 1.8));
            p.rect_stroke(q, 0.0, s, egui::StrokeKind::Middle);
            p.line_segment([egui::pos2(q.left(), q.center().y), egui::pos2(q.right(), q.center().y)], s);
            p.circle_filled(egui::pos2(q.right() - 5.0, q.bottom() - 5.0), 1.6, c);
        }
    }
}

/// Call every frame from `App::ui`: shortcut ⌘, and the window itself.
pub fn ui(ctx: &egui::Context) {
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
        open();
    }
    let mut open = OPEN.load(Ordering::Relaxed);
    if !open {
        return;
    }
    let mut area=SECTION.load(Ordering::Relaxed);
    show_window(ctx,&mut open,&mut area);
    SECTION.store(area,Ordering::Relaxed);
    if running() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    // determine states again after a job ends
    let ended = WORK.lock().unwrap().iter_mut().filter(|a| a.done && !a.state_new).fold(false, |_, a| {
        a.state_new = true;
        true
    });
    if ended {
        *STATE.lock().unwrap() = None;
    }
    OPEN.store(open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)), Ordering::Relaxed);
}


/// Same production window, with local open/page state for deterministic geometry tests.
fn show_window(ctx:&egui::Context,open:&mut bool,area:&mut usize) {
    // Own modal window: the rest of the app lies dimmed below and takes no clicks until the settings are closed
    egui::Area::new(egui::Id::new("app-preferences-dim")).order(egui::Order::Middle).fixed_pos(ctx.content_rect().min).show(ctx, |ui| {
        let r = ctx.content_rect();
        ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(150));
        let _ = ui.allocate_rect(r, egui::Sense::click_and_drag());
    });
    egui::Window::new(egui::RichText::new(i18n::text(Text::PrefsTitle)).font(lr::semibold(15.0)))
        .id(egui::Id::unique("app-preferences"))
        .order(egui::Order::Foreground)
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(780.0)
        .constrain_to(ctx.content_rect())
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(14, 10)).corner_radius(0.0))
        .show(ctx, |ui| {
            ui.set_width(780.0);
            // Toolbar
            let mut chosen = *area;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for i in 0..SECTIONS.len() {
                    let name=localized::area_label(i);
                    let (r, resp) = ui.allocate_exact_size(egui::vec2((780.0 - 20.0) / SECTIONS.len() as f32, 76.0), egui::Sense::click());
                    let an = i == chosen;
                    if an {
                        ui.painter().rect_filled(r, 0.0, Color32::from_gray(0x3a));
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 0.0, Color32::from_gray(0x2a));
                    }
                    let c = if an { Color32::WHITE } else { Color32::from_gray(0xb0) };
                    symbol(ui.painter(), egui::Rect::from_center_size(r.center_top() + egui::vec2(0.0, 19.0), egui::vec2(28.0, 28.0)), SYMBOL_NR[i], c);
                    let mut job=egui::text::LayoutJob::simple(name.to_owned(),egui::FontId::proportional(10.0),c,r.width()-4.0);
                    job.wrap.break_anywhere=true;
                    let text=ui.painter().layout_job(job);
                    ui.painter().galley(egui::pos2(r.center().x-text.size().x/2.0,r.bottom()-4.0-text.size().y),text,c);
                    if resp.clicked() {
                        chosen = i;
                    }
                }
            });
            *area=chosen;
            ui.separator();
            // fixed content area
            let (rect, _) = ui.allocate_exact_size(egui::vec2(780.0, (ctx.content_rect().height()-150.0).clamp(160.0,470.0)), egui::Sense::hover());
            let mut kind = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
            let mut area = egui::ScrollArea::vertical().id_salt(("prefs", chosen)).auto_shrink([false; 2]);
            // test aid for screenshots: GRAINEO_PREFS_SCROLL=<points> scrolls the page (only in test runs)
            if crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
                if let Some(y) = crate::env::var("GRAINEO_PREFS_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
                    area = area.vertical_scroll_offset(y);
                }
            }
            area.show(&mut kind, |ui| crate::sticky::scope(ui, |ui| {
                ui.set_width(760.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                // A long value (path, model name, translation) never widens the page
                lr::bounded(ui, |ui| match chosen {
                    0 => localized::general(ui),
                    1 => interface_section(ui, ctx),
                    2 => library(ui),
                    3 => develop_section(ui),
                    4 => export(ui),
                    5 => lenses_section(ui),
                    6 => ai(ui, ctx),
                    7 => module(ui, ctx),
                    9 => crate::film_profiles_ui::settings(ui),
                    10 => crate::remote_access::settings_page(ui),
                    _ => storage_section(ui),
                })
            }));
        });
}

// ─────────────────────────────────────────────────────────── Building blocks

const NAME_WIDTH: f32 = 290.0;

/// Group: capitals heading and box.
pub(crate) fn group(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    let g = lr::caps(ui.painter(), title, lr::semibold(10.5), lr::TEXT_DIM);
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), g.size().y + 4.0), egui::Sense::hover());
    ui.painter().galley(r.min + egui::vec2(4.0, 0.0), g, lr::TEXT_DIM);
    // the heading sticks to the top of a long settings page while the group scrolls
    let owned = title.to_string();
    crate::sticky::register(crate::sticky::RANK_CATEGORY, ui.id().with(("gruppe", title)), r.expand2(egui::vec2(0.0, 3.0)), move |p, bar, _| {
        let g = lr::caps(p, &owned, lr::semibold(10.5), lr::TEXT_DIM);
        p.galley(egui::pos2(bar.left() + 4.0, bar.center().y - g.size().y / 2.0), g, lr::TEXT_DIM);
    });
    egui::Frame::new().fill(Color32::from_gray(0x1d)).stroke(egui::Stroke::new(1.0, Color32::from_gray(0x2c))).corner_radius(0.0).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        content(ui);
    });
}

/// Row: name on the left (fixed width), control on the right, optionally a grey line below.
pub(crate) fn row(ui: &mut egui::Ui, name: &str, description: Option<&str>, right: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(NAME_WIDTH, 24.0), egui::Sense::hover());
        ui.painter().with_clip_rect(r).text(r.left_center(), egui::Align2::LEFT_CENTER, crate::tr(name), egui::FontId::proportional(lr::UI_SIZE), lr::TEXT);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
    if let Some(b) = description {
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),30.0),egui::Layout::top_down(egui::Align::Min),|ui| {
            ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(b)).small().color(lr::TEXT_DIM)).wrap());
        });
        ui.add_space(2.0);
    }
}

/// Row whose name is longer than the name column: name on the left, controls on the right.
fn row_wide(ui: &mut egui::Ui, name: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(name).color(lr::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

/// Separator between rows of a group.
pub(crate) fn separator(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 5.0), egui::Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, Color32::from_gray(0x2a)));
}

/// Toggle (on/off) in Mac style. Returns: toggled.
pub(crate) fn toggle(ui: &mut egui::Ui, an: &mut bool) -> bool {
    crate::lr::switch(ui, an)
}

/// Traffic-light colors for states.
#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Green,
    Yellow,
    Red,
    Gray,
}

/// State value: dot + grey text (right-aligned).
pub(crate) fn value(ui: &mut egui::Ui, text: &str, a: Signal) {
    ui.label(egui::RichText::new(crate::menu::i18n::tr_msg(text)).color(lr::TEXT_DIM));
    let c = match a {
        Signal::Green => Color32::from_rgb(0x4c, 0xc2, 0x6a),
        Signal::Yellow => Color32::from_rgb(0xe0, 0xb0, 0x40),
        Signal::Red => Color32::from_rgb(0xe0, 0x5c, 0x50),
        Signal::Gray => Color32::from_gray(0x94),
    };
    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 4.0, c);
}

fn determining(ui: &mut egui::Ui) {
    crate::dialog::pending(ui, crate::tr("wird ermittelt …"));
}

/// Path field (shortened, read only) and "Choose …".
fn path(ui: &mut egui::Ui, p: &str) -> bool {
    let w = ui.button(crate::tr("Wählen …")).clicked();
    path_text(ui, p, 320.0);
    w
}

/// Read-only path, shortened in the middle ("/Users/…/graineo") so it fits `width` and the space left in
/// the row; the full path is reached through "Show in Finder".
pub(super) fn path_text(ui: &mut egui::Ui, p: &str, width: f32) {
    let width = width.min(ui.available_width()).max(24.0);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let painter = ui.painter().clone();
    let fits = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), lr::TEXT_DIM).size().x <= width - 2.0;
    let shown = if fits(p) {
        p.to_string()
    } else {
        let chars: Vec<char> = p.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(&middle_ellipsis(&chars, mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        middle_ellipsis(&chars, lo)
    };
    ui.add_sized([width, 20.0], egui::Label::new(egui::RichText::new(shown).monospace().small().color(lr::TEXT_DIM)).truncate());
}

/// `keep` characters of `chars` around "…": a little more of the end (the folder name) than of the start.
fn middle_ellipsis(chars: &[char], keep: usize) -> String {
    let head = keep * 2 / 5;
    let tail = keep - head;
    let mut s: String = chars[..head].iter().collect();
    s.push('…');
    s.extend(&chars[chars.len() - tail..]);
    s
}

/// Actions right-aligned at the end of the group.
fn actions(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(2.0);
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::right_to_left(egui::Align::Center), content);
}

/// Question before switching sidecars on: number of sidecars written once (`None`: no question open).
static SIDECAR_QUESTION: Mutex<Option<usize>> = Mutex::new(None);

fn sidecar_question(ctx: &egui::Context) {
    let Some(n) = *SIDECAR_QUESTION.lock().unwrap() else { return };
    let answer = crate::dialog::show(
        ctx,
        "begleit-einschalten",
        crate::tr("Begleitdateien schreiben?"),
        crate::dialog::WIDTH,
        |ui| crate::dialog::chip(ui, &crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos"))),
        crate::dialog::Buttons::confirm(crate::tr("Schreiben"), true),
    );
    match answer {
        crate::dialog::Answer::Open => {}
        crate::dialog::Answer::Cancel => *SIDECAR_QUESTION.lock().unwrap() = None,
        crate::dialog::Answer::Primary => {
            *SIDECAR_QUESTION.lock().unwrap() = None;
            std::thread::spawn(|| {
                let r = library::store::set_write_sidecars(true).and_then(|_| library::db::Db::standard().map_err(|e| e.to_string()).and_then(|db| {
                    let n = library::store::queue_all(&db)?;
                    // smart collections as a visible copy in the root folder (written only while the option is on)
                    if let Err(e) = library::collection_file::write_if_enabled(&db) {
                        crate::log(&format!("collections.json: {e}"));
                    }
                    Ok(n)
                }));
                if let Err(e) = r {
                    crate::log(&format!("writeSidecars: {e}"));
                }
            });
        }
    }
}

/// Question before deleting every develop history, and its outcome.
static HISTORY_QUESTION: Mutex<bool> = Mutex::new(false);
static HISTORY_MESSAGE: Mutex<String> = Mutex::new(String::new());

fn history_question(ctx: &egui::Context) {
    if !*HISTORY_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) {
        return;
    }
    let answer = crate::dialog::show(
        ctx,
        "verlauf-loeschen",
        crate::tr("Alle Verläufe löschen?"),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::text(ui, crate::tr("Die Schritte aller Fotos werden aus der Bibliothek gelöscht. Die Bearbeitungen selbst bleiben, wie sie jetzt sind."));
            crate::dialog::hint(ui, crate::tr("Ältere Sicherungen der Bibliothek enthalten die Verläufe weiterhin."));
        },
        crate::dialog::Buttons::destructive(crate::tr("Alle löschen"), true),
    );
    match answer {
        crate::dialog::Answer::Open => {}
        crate::dialog::Answer::Cancel => *HISTORY_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = false,
        crate::dialog::Answer::Primary => {
            *HISTORY_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = false;
            crate::history::delete_all(|r| {
                *HISTORY_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = match r {
                    Ok(n) => crate::trf!("{n} Schritte gelöscht", n = n),
                    Err(e) => crate::trf!("Fehler: {e}", e = e),
                };
            });
        }
    }
}

/// Backup chosen in "Wiederherstellen" (index into `State::backup_list`) and the open question before restoring.
static RESTORE_CHOICE: Mutex<usize> = Mutex::new(0);
static RESTORE_QUESTION: Mutex<Option<(std::path::PathBuf, u64)>> = Mutex::new(None);
static RESTORE_MESSAGE: Mutex<String> = Mutex::new(String::new());
/// Backup unpacked for "Sicherung ansehen …": graineo starts again into its view (next frame).
static VIEW_READY: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Name of a backup for the list: its date, the folder (which target) and, if known, the size (packed backups are
/// about a third of the unpacked ones from before 0.107.0).
fn backup_name(p: &std::path::Path, size: Option<u64>) -> String {
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let n = name.trim_start_matches("library-").trim_end_matches(".zst").trim_end_matches(".sqlite");
    let dir = p.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    match size {
        Some(b) if b < 1_000_000 => format!("{n} · {dir} · {} KB", b.div_ceil(1000)),
        Some(b) => format!("{n} · {dir} · {:.0} MB", b as f64 / 1e6),
        None => format!("{n} · {dir}"),
    }
}

fn restore_question(ctx: &egui::Context) {
    let Some((p, size)) = RESTORE_QUESTION.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let answer = crate::dialog::show(
        ctx,
        "sicherung-wiederherstellen",
        crate::tr("Bibliothek wiederherstellen?"),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::chip(ui, &backup_name(&p, Some(size)));
            crate::dialog::text(ui, crate::tr("Beim nächsten Start ersetzt graineo die Bibliothek durch diese Sicherung. Änderungen seit der Sicherung (Bewertungen, Stichwörter, Entwicklungen) fehlen danach."));
            crate::dialog::hint(ui, crate::tr("Die jetzige Bibliothek bleibt daneben erhalten (library-before-restore-….sqlite)."));
        },
        crate::dialog::Buttons::confirm(crate::tr("Beim nächsten Start wiederherstellen"), true),
    );
    match answer {
        crate::dialog::Answer::Open => {}
        crate::dialog::Answer::Cancel => *RESTORE_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = None,
        crate::dialog::Answer::Primary => {
            *RESTORE_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
            std::thread::spawn(move || {
                let m = match library::backup::schedule_restore(&library::real_db_path(), &p) {
                    Ok(i) => crate::trf!("Wird beim nächsten Start wiederhergestellt ({n} Fotos) – graineo jetzt neu starten", n = i.images),
                    Err(e) => crate::trf!("Sicherung nicht verwendbar: {e}", e = e),
                };
                *RESTORE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = m;
                *STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            });
        }
    }
}

/// Setting "Grafikkarte verwenden" (`useGpu`, default on).
pub(crate) fn gpu_setting() -> bool {
    setting_bool("useGpu", true)
}

/// At start: hand the setting "Grafikkarte verwenden" to the computing crates (`GRAINEO_GPU=0` still wins).
pub fn apply_gpu_setting() {
    pools::set_gpu_allowed(gpu_setting());
}

fn setting_bool(k: &str, standard: bool) -> bool {
    library::settings::value(k).as_bool().unwrap_or(standard)
}

/// Show the layer mixer in color mode (setting `layerMixer`, default off); kept in
/// memory because the Develop panel asks for it every frame.
static LAYER_MIXER: std::sync::OnceLock<AtomicBool> = std::sync::OnceLock::new();

pub fn layer_mixer_on() -> bool {
    LAYER_MIXER.get_or_init(|| AtomicBool::new(setting_bool("layerMixer", false))).load(std::sync::atomic::Ordering::Relaxed)
}

fn set_layer_mixer(an: bool) {
    LAYER_MIXER.get_or_init(|| AtomicBool::new(an)).store(an, std::sync::atomic::Ordering::Relaxed);
    library::settings::set("layerMixer", json!(an));
}

fn start_work(what: &str, f: impl FnOnce(Arc<AtomicBool>, &dyn Fn(f32, &str)) -> Result<String, String> + Send + 'static) {
    let cancel = Arc::new(AtomicBool::new(false));
    let nr = WORK_NR.fetch_add(1, Ordering::Relaxed);
    {
        let mut w = WORK.lock().unwrap();
        w.retain(|a| a.what != what && !(a.done && a.state_new && a.started.elapsed().as_secs() > 600));
        w.push(Work { nr, what: what.into(), failed: false, progress: 0.0, text: "beginne …".into(), done: false, cancel: cancel.clone(), state_new: false, started: std::time::Instant::now() });
    }
    std::thread::spawn(move || {
        let report_to = |p: f32, t: &str| {
            if let Some(a) = WORK.lock().unwrap().iter_mut().find(|a| a.nr == nr) {
                a.progress = p;
                a.text = t.to_string();
            }
        };
        let r = f(cancel, &report_to);
        if let Some(a) = WORK.lock().unwrap().iter_mut().find(|a| a.nr == nr) {
            a.done = true;
            a.progress = 1.0;
            a.failed = r.is_err();
            a.text = match r {
                Ok(t) => t,
                Err(e) => crate::trf!("Fehler: {e}", e = e),
            };
        }
    });
}

/// Work of one kind (`what`), running or finished.
fn work_of(what: &str) -> Option<Work> {
    WORK.lock().unwrap().iter().find(|a| a.what == what).cloned()
}

fn show_work(ui: &mut egui::Ui, what: &str) {
    if let Some(a) = work_of(what) {
        ui.horizontal(|ui| {
            ui.add(egui::ProgressBar::new(a.progress).desired_width(240.0).show_percentage());
            ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(&a.text)).small().color(lr::TEXT_DIM)).truncate());
            if !a.done && ui.small_button(crate::tr("Abbrechen")).clicked() {
                a.cancel.store(true, Ordering::Relaxed);
            }
        });
    }
}

fn running() -> bool {
    WORK.lock().unwrap().iter().any(|a| !a.done)
}

/// First running work that is not one of `own`.
fn running_other(own: &[&str]) -> Option<Work> {
    WORK.lock().unwrap().iter().find(|a| !a.done && !own.contains(&a.what.as_str())).cloned()
}

/// Name of a work of the settings window for the explanation of locked buttons.
fn work_name(what: &str) -> String {
    match what {
        "sicherung" => crate::tr("Sicherung der Bibliothek").to_string(),
        "ansehen" => crate::tr("Sicherung zum Ansehen entpacken").to_string(),
        "einlesen" => crate::tr("Begleitdateien einlesen").to_string(),
        "begleit" | "begleit-schreiben" => crate::tr("Begleitdateien").to_string(),
        id => match library::module::ALL.iter().find(|d| d.id == id) {
            Some(d) => crate::trf!("Modell „{name}“", name = short_model_name(d)),
            None => crate::tr("Modul installieren oder entfernen").to_string(),
        },
    }
}

/// Buttons of a section wait for work started elsewhere (another section of the settings, or a job of the
/// library for `library = true`): say what runs, why, since when and how much longer. Nothing while the section's
/// own work runs (`own`, its progress shows below the button).
fn show_blocked(ui: &mut egui::Ui, own: &[&str], library: bool) {
    if let Some(a) = running_other(own) {
        let r = crate::dialog::Running { what: work_name(&a.what), since: a.started.elapsed(), fraction: (a.progress > 0.0).then_some(a.progress), last: None };
        crate::dialog::blocked(ui, crate::tr("Bis dahin sind diese Knöpfe gesperrt; die Einstellungen führen immer nur eine Arbeit zur Zeit aus."), &r);
    } else if let Some(r) = crate::library_ui::running_job().filter(|_| library) {
        crate::dialog::blocked(ui, crate::tr("Prüfen und Neu aufbauen gehen erst danach, damit nicht zwei Aufträge gleichzeitig in die Bibliothek schreiben."), &r);
    }
}

/// Running work of the settings window (backup, sidecars, modules) for the status in the title bar.
pub fn work_status() -> Option<String> {
    let a = running_other(&[])?;
    let what = match a.what.as_str() {
        "sicherung" | "ansehen" => crate::tr("Sicherung"),
        "begleit" | "begleit-schreiben" => crate::tr("Begleitdateien"),
        _ => crate::tr("Modul"),
    };
    Some(format!("{what} · {}", crate::menu::i18n::tr_msg(&a.text)))
}

// ─────────────────────────────────────────────────────────── Pages

fn interface_section(ui: &mut egui::Ui, ctx: &egui::Context) {
    group(ui, i18n::text(Text::Appearance), |ui| {
        row(ui, i18n::text(Text::InterfaceScale), None, |ui| {
            let mut z = SCALES.iter().map(|(v, _)| *v).min_by(|a, b| (a - ctx.zoom_factor()).abs().total_cmp(&(b - ctx.zoom_factor()).abs())).unwrap_or(1.0);
            let opts = [(0.9,i18n::text(Text::Small)),(1.0,i18n::text(Text::Normal)),(1.15,i18n::text(Text::Large))];
            if lr::segmented(ui, &mut z, &opts) {
                ctx.set_zoom_factor(z);
                if let Some(p) = scale_file() {
                    let _ = std::fs::write(p, format!("{z}"));
                }
            }
        });
    });
}

fn library(ui: &mut egui::Ui) {
    let z = state(ui);
    // Lightroom: catalog, import and handover of the edits all happen in the dialog "Import from Lightroom"
    group(ui, crate::tr("Lightroom"), |ui| {
        row(ui, crate::tr("Katalog"), None, |ui| {
            if ui.button(crate::tr("Aus Lightroom importieren …")).clicked() {
                OPEN.store(false, Ordering::Relaxed);
                crate::library_ui::request_lightroom();
            }
            match &z {
                Some(z) => {
                    let name = std::path::Path::new(&z.lr_catalog).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| crate::tr("Kein Katalog gewählt").to_string());
                    ui.add_sized([300.0, 20.0], egui::Label::new(egui::RichText::new(name).monospace().small().color(lr::TEXT_DIM)).truncate());
                }
                None => determining(ui),
            }
        });
    });
    // Apple Photos exists only on the Mac (access is asked for only when the switch is turned on)
    #[cfg(target_os = "macos")]
    group(ui, crate::tr("Apple Fotos"), |ui| {
        let m = crate::apple_photos_ui::media_library();
        let hint = match &m {
            Some(p) => crate::trf!("{p} · graineo liest deine Bibliothek nur. In Apple Fotos ändert sich nichts.", p = p.display()),
            None => crate::tr("Keine Bibliothek von Apple Fotos gefunden").to_string(),
        };
        row(ui, crate::tr("Bibliothek von Apple Fotos verbinden"), Some(&hint), |ui| {
            let mut an = crate::apple_photos_ui::an();
            ui.add_enabled_ui(m.is_some(), |ui| {
                if toggle(ui, &mut an) {
                    crate::apple_photos_ui::set(an);
                }
            });
        });
        if crate::apple_photos_ui::an() && m.is_some() {
            separator(ui);
            row(ui, crate::tr("Abgleich"), Some(&crate::apple_photos_ui::message()), |ui| {
                if crate::apple_photos_ui::running() {
                    ui.spinner();
                } else if ui.button(crate::tr("Jetzt abgleichen")).clicked() {
                    crate::apple_photos_ui::kick(ui.ctx());
                }
            });
        }
    });
    // Edits live in the library; sidecar files next to the photos are an option (default off)
    group(ui, crate::tr("Bearbeitungen"), |ui| {
        row(ui, crate::tr("Gespeichert in"), None, |ui| {
            ui.label(egui::RichText::new(crate::tr("Bibliothek")).color(lr::TEXT_DIM));
        });
        separator(ui);
        row_wide(ui, crate::tr("Begleitdateien (XMP) neben die Fotos schreiben"), |ui| {
            crate::help::info(ui, "prefs-begleit", crate::tr("Begleitdateien"), &crate::help::SIDECARS);
            let mut an = library::store::write_sidecars();
            if toggle(ui, &mut an) {
                if an {
                    // switching on: ask first, with the number of sidecars written once now
                    std::thread::spawn(|| {
                        let n = library::db::Db::standard().map(|db| library::store::count_all(&db.conn)).unwrap_or(0);
                        *SIDECAR_QUESTION.lock().unwrap() = Some(n);
                    });
                } else if let Err(e) = library::store::set_write_sidecars(false) {
                    crate::log(&format!("writeSidecars: {e}"));
                }
            }
        });
        sidecar_question(ui.ctx());
        restore_question(ui.ctx());
        separator(ui);
        // develop history (steps of every photo): kept in the library, may be switched off and deleted
        row(ui, crate::tr("Verlauf speichern"), Some(crate::tr("Schritte jeder Bearbeitung, nur in der Bibliothek auf diesem Rechner. Aus: nur bis zum Beenden.")), |ui| {
            let mut an = library::store::save_history();
            if toggle(ui, &mut an) {
                if let Err(e) = library::store::set_save_history(an) {
                    crate::log(&format!("saveHistory: {e}"));
                }
            }
        });
        row(ui, crate::tr("Alle Verläufe löschen"), None, |ui| {
            if ui.add_enabled(!running(), egui::Button::new(crate::tr("Löschen …"))).clicked() {
                *HISTORY_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = true;
            }
            let m = HISTORY_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if !m.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(m).small().color(lr::TEXT_DIM)).truncate());
            }
        });
        history_question(ui.ctx());
        separator(ui);
        row(ui, crate::tr("Begleitdateien einlesen"), None, |ui| {
            if ui.add_enabled(!running(), egui::Button::new(crate::tr("Einlesen …"))).clicked() {
                start_work("einlesen", |cancel, report| {
                    let db = library::db::Db::standard().map_err(|e| e.to_string())?;
                    let r = library::sidecar_import::run(&db, &|d, t| report(if t > 0 { d as f32 / t as f32 } else { 1.0 }, &crate::trf!("{d} von {t}", d = d, t = t)), &cancel)?;
                    library::sidecar_import::save_protocol(&r);
                    Ok(crate::trf!("{a} übernommen · {b} gelesen", a = r.taken, b = r.sidecars))
                });
            }
        });
        show_work(ui, "einlesen");
        show_blocked(ui, &["einlesen"], false);
    });
    // series and stacks (library_series.rs, series_job.rs): detection, collapsing, what ⇧X does in the review
    group(ui, crate::tr("Serien und Stapel"), |ui| {
        row(ui, crate::tr("Serien automatisch erkennen"), Some(crate::tr("Fotos derselben Kamera, kurz nacheinander und mit gleichem Motiv, leise im Hintergrund.")), |ui| {
            let mut on = crate::series_job::enabled();
            if toggle(ui, &mut on) {
                library::settings::set("seriesDetect", serde_json::json!(on));
                if on {
                    crate::series_job::poke();
                } else {
                    crate::series_job::cancel();
                }
            }
        });
        separator(ui);
        row(ui, crate::tr("Serien zuklappen"), None, |ui| {
            let now = library::settings::value("stackCollapse").as_str().unwrap_or("reviewed").to_string();
            let name = |k: &str| match k {
                "always" => crate::tr("immer"),
                "never" => crate::tr("nie"),
                _ => crate::tr("nach der Sichtung"),
            };
            egui::ComboBox::from_id_salt("prefs-serien-zuklappen").selected_text(name(&now)).width(170.0).show_ui(ui, |ui| {
                for k in ["reviewed", "always", "never"] {
                    if ui.selectable_label(now == k, name(k)).clicked() && now != k {
                        library::settings::set("stackCollapse", serde_json::json!(k));
                        let mode = k.to_string();
                        std::thread::spawn(move || {
                            if let Ok(db) = library::db::Db::standard() {
                                if let Err(e) = library::series::apply_collapse_setting(&db, &mode) {
                                    crate::log(&format!("stackCollapse: {e}"));
                                }
                            }
                            crate::series_job::changed();
                        });
                    }
                }
            });
        });
        separator(ui);
        row(ui, crate::tr("⇧X beim Sichten"), Some(crate::tr("Was mit den übrigen Fotos einer Serie geschieht, wenn das beste gewählt ist. Gelöscht wird nie.")), |ui| {
            let now = library::settings::value("stackRest").as_str().unwrap_or("reject").to_string();
            let name = |k: &str| match k {
                "lower" => crate::tr("auf 0 Sterne (außer 4–5)"),
                "none" => crate::tr("nichts"),
                _ => crate::tr("ablehnen"),
            };
            egui::ComboBox::from_id_salt("prefs-serien-rest").selected_text(name(&now)).width(170.0).show_ui(ui, |ui| {
                for k in ["reject", "lower", "none"] {
                    if ui.selectable_label(now == k, name(k)).clicked() {
                        library::settings::set("stackRest", serde_json::json!(k));
                    }
                }
            });
        });
        if !cfg!(target_os = "macos") {
            ui.label(egui::RichText::new(crate::tr("Hinweise zu Gesichtern und Augen gibt es nur unter macOS.")).small().color(lr::TEXT_DIM));
        }
    });
    group(ui, crate::tr("Sicherung"), |ui| {
        row(ui, crate::tr("Ziel"), None, |ui| match &z {
            Some(z) => {
                if path(ui, &z.backup_target) {
                    crate::file_dialog::target_folder(ui.ctx(), "prefs-sicherung", |d| d);
                }
                if let Some(p) = crate::file_dialog::fetch_one("prefs-sicherung") {
                    set_db("backup_dir", p.to_string_lossy().into_owned());
                }
            }
            None => determining(ui),
        });
        separator(ui);
        // the library holds all edits: a second copy elsewhere (e.g. the NAS) protects them
        row(ui, crate::tr("Zweites Ziel"), None, |ui| match &z {
            Some(z) => {
                if path(ui, if z.backup_target2.is_empty() { "–" } else { &z.backup_target2 }) {
                    crate::file_dialog::target_folder(ui.ctx(), "prefs-sicherung2", |d| d);
                }
                if let Some(p) = crate::file_dialog::fetch_one("prefs-sicherung2") {
                    set_db("backup_dir2", p.to_string_lossy().into_owned());
                }
            }
            None => determining(ui),
        });
        separator(ui);
        row(ui, crate::tr("Vorhanden"), Some(crate::tr("Täglich automatisch, gepackt. Bleiben: die letzten 3 Tage und eine Monatssicherung.")), |ui| match &z {
            Some(z) => value(ui, &crate::trf!("{a} von {b}", a = z.backups, b = library::backup::KEEP + library::backup::MONTHLY), if z.backups > 0 { Signal::Green } else { Signal::Yellow }),
            None => determining(ui),
        });
        actions(ui, |ui| {
            if ui.add_enabled(!running(), egui::Button::new(crate::tr("Jetzt sichern"))).clicked() {
                start_work("sicherung", |_, _| {
                    let db = library::db::Db::standard().map_err(|e| e.to_string())?;
                    library::backup::create_backup(&db).map(|(p, n)| format!("{} ({:.0} MB)", p.display(), n as f64 / 1e6))
                });
            }
        });
        show_work(ui, "sicherung");
        show_blocked(ui, &["sicherung"], false);
        separator(ui);
        // restore: carried out at the next start, before anything opens the library
        row(ui, crate::tr("Wiederherstellen"), Some(crate::tr("Beim nächsten Start")), |ui| match &z {
            Some(z) => match &z.restore_pending {
                Some(p) => {
                    if ui.button(crate::tr("Nicht wiederherstellen")).clicked() {
                        library::backup::cancel_restore(&library::db_path());
                        RESTORE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clear();
                        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                    value(ui, &backup_name(p, None), Signal::Yellow);
                }
                None if z.backup_list.is_empty() => value(ui, crate::tr("keine Sicherung"), Signal::Yellow),
                None => {
                    let mut choice = *RESTORE_CHOICE.lock().unwrap_or_else(|e| e.into_inner());
                    choice = choice.min(z.backup_list.len() - 1);
                    // not while a backup is viewed: first back to the library
                    let free = !running() && library::backup_view::active().is_none();
                    if ui.add_enabled(free, egui::Button::new(crate::tr("Wiederherstellen …"))).clicked() {
                        *RESTORE_QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = Some(z.backup_list[choice].clone());
                    }
                    let r = ui.add_enabled(free, egui::Button::new(crate::tr("Ansehen …")));
                    lr::hover_tip(ui, &r, "Sicherung nur zum Ansehen öffnen – graineo startet neu mit ihrem Stand, die Bibliothek bleibt unberührt");
                    if r.clicked() {
                        let p = z.backup_list[choice].0.clone();
                        start_work("ansehen", move |_, report| {
                            report(0.3, "Sicherung wird entpackt …");
                            library::backup_view::prepare(&p)?;
                            *VIEW_READY.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
                            Ok(crate::tr("graineo startet neu …").to_string())
                        });
                    }
                    egui::ComboBox::from_id_salt("prefs-wiederherstellen").selected_text(backup_name(&z.backup_list[choice].0, Some(z.backup_list[choice].1))).width(260.0).show_ui(ui, |ui| {
                        for (i, (p, n)) in z.backup_list.iter().enumerate() {
                            ui.selectable_value(&mut choice, i, backup_name(p, Some(*n)));
                        }
                    });
                    *RESTORE_CHOICE.lock().unwrap_or_else(|e| e.into_inner()) = choice;
                }
            },
            None => determining(ui),
        });
        show_work(ui, "ansehen");
        if let Some(p) = VIEW_READY.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if let Err(e) = crate::backup_view_ui::restart(ui.ctx(), Some(&p)) {
                *RESTORE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = e;
            }
        }
        let m = RESTORE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !m.is_empty() {
            crate::dialog::hint(ui, &m);
        }
    });
    group(ui, crate::tr("Wartung"), |ui| {
        row(ui, crate::tr("Bibliothek prüfen"), None, |ui| {
            crate::help::info(ui, "prefs-pruefen", crate::tr("Bibliothek prüfen"), &crate::help::LIBRARY_CHECK);
            if ui.add_enabled(!running() && crate::library_ui::running_job().is_none(), egui::Button::new(crate::tr("Prüfen"))).clicked() {
                OPEN.store(false, Ordering::Relaxed);
                crate::library_ui::request_maintenance("check");
            }
        });
        separator(ui);
        row(ui, crate::tr("Neu aufbauen"), None, |ui| {
            if ui.add_enabled(!running() && crate::library_ui::running_job().is_none(), egui::Button::new(crate::tr("Neu aufbauen …"))).clicked() {
                OPEN.store(false, Ordering::Relaxed);
                crate::library_ui::request_maintenance("rebuild");
            }
        });
        show_blocked(ui, &[], true);
    });
    previews_group(ui, &z);
    cache_limit_ui::group_ui(ui);
}

/// Preview store of the library (registered by the library view; for usage, free space and unreachable photos).
static PREVIEW_CACHE: Mutex<Option<Arc<library::preview::Cache>>> = Mutex::new(None);

/// Confirmation of "Regenerate previews" open
static REGENERATE_QUESTION: AtomicBool = AtomicBool::new(false);

pub fn register_previews(cache: Arc<library::preview::Cache>) {
    *PREVIEW_CACHE.lock().unwrap() = Some(cache.clone());
    cache_limit_ui::start(cache);
}

/// Activity entry while the preview cache is being trimmed (title bar).
pub fn cache_task() -> Option<crate::activity::Task> {
    cache_limit_ui::task()
}

/// Number of photos on other drives (for the projection of the offline copies; set by the library view).
static OFFLINE_PHOTO_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_offline_photo_count(n: usize) {
    OFFLINE_PHOTO_COUNT.store(n, Ordering::Relaxed);
}

/// Offline copies that show the development: keys of all photos concerned (set by the library view), counted in the
/// background at most every 10 s while the settings show them ([`developed_counter`]).
static DEVELOP_KEYS: Mutex<Option<Arc<Vec<u64>>>> = Mutex::new(None);
static DEVELOPED: Mutex<Option<(usize, usize, std::time::Instant)>> = Mutex::new(None);
static COUNTING: AtomicBool = AtomicBool::new(false);

pub fn set_offline_develop_keys(keys: Vec<u64>) {
    *DEVELOP_KEYS.lock().unwrap() = Some(Arc::new(keys));
    // count anew on the next look
    if let Some(d) = DEVELOPED.lock().unwrap().as_mut() {
        d.2 = std::time::Instant::now() - std::time::Duration::from_secs(60);
    }
}

/// (developed, all) of the offline copies of RAW/X3F photos on other drives in the current level; `None`: not counted yet.
fn developed_counter(cache: &Arc<library::preview::Cache>) -> Option<(usize, usize)> {
    let due = DEVELOPED.lock().unwrap().as_ref().is_none_or(|d| d.2.elapsed().as_secs() >= 10);
    if due && !COUNTING.swap(true, Ordering::SeqCst) {
        let keys = DEVELOP_KEYS.lock().unwrap().clone();
        let cache = cache.clone();
        std::thread::spawn(move || {
            library::platform::background_priority();
            let level = library::preview::large();
            let keys = keys.unwrap_or_default();
            let n = keys.iter().filter(|k| cache.is_developed(**k, level)).count();
            *DEVELOPED.lock().unwrap() = Some((n, keys.len(), std::time::Instant::now()));
            COUNTING.store(false, Ordering::SeqCst);
        });
    }
    DEVELOPED.lock().unwrap().map(|d| (d.0, d.1))
}

/// Hint in yellow (space warnings in the settings).
fn hint_yellow(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(Color32::from_rgb(0xe0, 0xb0, 0x40))).wrap());
}

/// Settings → Library → Previews: both levels, usage, regenerate.
fn previews_group(ui: &mut egui::Ui, z: &Option<State>) {
    use library::preview;
    let cache = PREVIEW_CACHE.lock().unwrap().clone();
    group(ui, crate::tr("Vorschauen"), |ui| {
        row(ui, crate::tr("Kleine Vorschau"), None, |ui| {
            crate::help::info(ui, "prefs-vorschauen", "Vorschauen", &crate::help::PREVIEWS);
            let mut s = preview::small();
            let opts: Vec<(u32, &str)> = vec![(512, "512 px"), (1024, "1024 px")];
            if lr::segmented(ui, &mut s, &opts) {
                preview::set_levels(s, preview::large());
            }
        });
        separator(ui);
        row(ui, crate::tr("Offline-Kopie, andere Laufwerke"), None, |ui| {
            let mut l = preview::large();
            let opts: Vec<(u32, &str)> = vec![(2560, "2560 px"), (4000, "4000 px"), (preview::FULL, crate::tr("Volle Auflösung"))];
            if lr::segmented(ui, &mut l, &opts) {
                preview::set_levels(preview::small(), l);
            }
        });
        separator(ui);
        row(ui, crate::tr("Im Hintergrund vorbereiten"), None, |ui| {
            let mut an = setting_bool("previewsBackground", true);
            if toggle(ui, &mut an) {
                library::settings::set("previewsBackground", json!(an));
            }
        });
        separator(ui);
        let usage = cache.as_ref().map(|c| (c.usage(), c.paused()));
        row(ui, crate::tr("Belegt"), None, |ui| {
            if ui.button(crate::tr("Im Finder zeigen")).clicked() {
                crate::system::show(library::cache_folder());
            }
            match (usage, z) {
                (Some((u, paused)), _) => {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
                    value(ui, &gb(u.small + u.large + u.large_other), if paused { Signal::Yellow } else { Signal::Green });
                }
                (None, Some(z)) => value(ui, &gb(z.sizes[0].1), Signal::Green),
                (None, None) => determining(ui),
            }
        });
        if let (Some((u, paused)), Some(c)) = (usage, &cache) {
            separator(ui);
            row(ui, crate::tr("Kleine Vorschauen"), None, |ui| value(ui, &gb(u.small), Signal::Gray));
            separator(ui);
            // (large previews of photos on this Mac are left-overs of earlier versions and are being tidied away)
            let offline = u.large + u.large_other;
            row(ui, crate::tr("Offline-Kopien"), None, |ui| value(ui, &gb(offline), Signal::Gray));
            let photos = OFFLINE_PHOTO_COUNT.load(Ordering::Relaxed);
            let projected = preview::projected_offline(&u, photos, preview::large());
            if photos > 0 {
                dialog_hint_small(ui, &crate::trf!("Alle Fotos auf anderen Laufwerken in dieser Größe: etwa {gb}", gb = gb(projected)));
            }
            if let Some((done, all)) = developed_counter(c).filter(|d| d.1 > 0) {
                separator(ui);
                row(ui, crate::tr("Offline-Kopien entwickelt"), None, |ui| {
                    value(ui, &crate::trf!("{a} von {b}", a = thousands(done as i64), b = thousands(all as i64)), if done < all { Signal::Yellow } else { Signal::Green })
                });
            }
            let free = c.free_space().unwrap_or(0);
            if paused || preview::offline_shortfall(projected, u.large_other, free, c.min_free).is_some() {
                hint_yellow(ui, &crate::trf!("Auf diesem Mac sind nur noch {free} frei. Offline-Kopien entstehen nur, solange mindestens {min} frei bleiben; es wird nichts gelöscht.", free = gb(free), min = gb(c.min_free)));
            }
        }
        separator(ui);
        let unreachable = cache.as_ref().map_or(0, |c| c.unreachable_count());
        if REGENERATE_QUESTION.load(Ordering::SeqCst) {
            row(ui, crate::tr("Vorschauen neu erzeugen"), None, |ui| {
                if ui.button(crate::tr("Neu erzeugen")).clicked() {
                    REGENERATE_QUESTION.store(false, Ordering::SeqCst);
                    preview::request_regenerate();
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    REGENERATE_QUESTION.store(false, Ordering::SeqCst);
                }
            });
            let text = if unreachable > 0 {
                crate::trf!("Alle Vorschauen werden gelöscht und im Hintergrund neu erzeugt, zuerst die sichtbaren. {a} Fotos sind gerade nicht erreichbar: ihre Vorschauen bleiben und werden erst neu erzeugt, wenn der Speicherort wieder verbunden ist.", a = thousands(unreachable as i64))
            } else {
                crate::tr("Alle Vorschauen werden gelöscht und im Hintergrund neu erzeugt, zuerst die sichtbaren.").to_string()
            };
            dialog_hint_small(ui, &text);
        } else {
            row(ui, crate::tr("Vorschauen neu erzeugen"), None, |ui| {
                if ui.add_enabled(cache.is_some(), egui::Button::new(crate::tr("Neu erzeugen …"))).clicked() {
                    REGENERATE_QUESTION.store(true, Ordering::SeqCst);
                }
            });
        }
    });
}

fn develop_section(ui: &mut egui::Ui) {
    let z = state(ui);
    group(ui, crate::tr("Anzeige"), |ui| {
        row(ui, crate::tr("Breiter Farbraum in der Anzeige"), Some(crate::tr("Entwickeln zeigt die Farben ohne Beschneiden auf sRGB, wenn der Bildschirm mehr kann (MacBook Pro, EIZO): eingepasst, vergrößert, Lupe und Vergleich. Gilt für Foveon und alle anderen Formate, nicht für Film, Schwarzweiß und ältere Foveon-Kameras (DP1, DP2, SD15 …)")), |ui| {
            let mut an = crate::photo_display::wide_on();
            if toggle(ui, &mut an) {
                crate::photo_display::set_wide(an);
            }
        });
        separator(ui);
        row_wide(ui, crate::tr("Farbproof beim Start"), |ui| {
            crate::help::info(ui, "prefs-farbproof", crate::tr("Farbproof"), &crate::help::PROOF);
            let mut start = crate::proof::start();
            egui::ComboBox::from_id_salt("prefs-proof-start").selected_text(start.name()).width(150.0).show_ui(ui, |ui| {
                for p in crate::proof::Proof::ALL {
                    if ui.selectable_value(&mut start, p, p.name()).changed() {
                        crate::proof::set_start(p);
                    }
                }
            });
        });
    });
    // Operating mode for slower computers (`perf_mode`), everything on the processor, timing log and report
    group(ui, crate::tr("Leistung"), |ui| {
        row(ui, crate::tr("Betriebsart"), Some(crate::tr("Sparsam wartet auf einem Foto etwas länger, bevor die genaue Ansicht gerechnet wird, rechnet nichts im Voraus und hält weniger Fotos im Speicher. Das Ergebnis bleibt gleich: Zoom, Rauschregler und Export rechnen sofort genau. Automatisch wählt nach den gemessenen Rechenzeiten dieses Rechners und rechnet bei Akku, Stromsparmodus, Wärme oder knappem Speicher vorübergehend sparsam")), |ui| {
            let mut c = crate::perf_mode::choice();
            egui::ComboBox::from_id_salt("prefs-perf-mode").selected_text(c.name()).width(150.0).show_ui(ui, |ui| {
                for k in crate::perf_mode::Choice::ALL {
                    if ui.selectable_value(&mut c, k, k.name()).changed() {
                        crate::perf_mode::set_choice(k);
                    }
                }
            });
        });
        let st = crate::perf_mode::status();
        let why = st.cause.text();
        row(ui, crate::tr("Rechnet jetzt"), Some(why.as_str()), |ui| {
            if !st.factors.is_empty() && ui.button(crate::tr("Neu messen")).clicked() {
                crate::perf_mode::reset_measurements();
            }
            value(ui, st.tier.name(), if st.tier == crate::perf_mode::Tier::Exact { Signal::Green } else { Signal::Yellow });
        });
        separator(ui);
        row(ui, crate::tr("Grafikkarte verwenden"), Some(crate::tr("Aus: alles rechnet auf dem Prozessor (Rauschminderung, Film, Masken, andere RAW-Formate). Langsamer, der Lüfter läuft eher an; wirkt ab dem nächsten Rechnen")), |ui| {
            let mut an = gpu_setting();
            if toggle(ui, &mut an) {
                library::settings::set("useGpu", json!(an));
                pools::set_gpu_allowed(an);
            }
        });
        separator(ui);
        row(ui, crate::tr("Zeitprotokoll"), Some(crate::tr("Schreibt je geöffnetem Foto eine Zeile mit Zeiten (ohne Datei- und Ordnernamen) in timings.log im Protokollordner. Bleibt auf diesem Rechner; der Leistungsbericht hängt die letzten 100 Zeilen an")), |ui| {
            let mut an = crate::timing_log::enabled();
            if toggle(ui, &mut an) {
                crate::timing_log::set_enabled(an);
            }
        });
        separator(ui);
        row(ui, crate::tr("Leistungsbericht"), Some(crate::tr("Misst, wie schnell dieser Rechner Fotos öffnet und entwickelt, und schreibt eine Textdatei zum Einsenden")), |ui| {
            if ui.button(crate::tr("Erstellen …")).clicked() {
                crate::perf_report::open();
            }
        });
    });
    group(ui, crate::tr("Foveon: Zwischenspeicher der Foveon-Basis"), |ui| {
        row(ui, crate::tr("Belegt"), Some(crate::tr("Höchstens 20 GB; pausiert unter 15 GB freiem Platz")), |ui| match &z {
            Some(z) => {
                let (b, n, paused) = z.spp;
                if ui.add_enabled(b > 0, egui::Button::new(crate::tr("Leeren"))).clicked() {
                    develop::diskcache::clear();
                    *STATE.lock().unwrap() = None;
                }
                value(ui, &crate::trf!("{a} · {n} Bilder", a = gb(b), n = n), if paused { Signal::Yellow } else { Signal::Green });
            }
            None => determining(ui),
        });
        separator(ui);
        row(ui, crate::tr("Schichtmischer anzeigen"), Some(crate::tr("Verteilt die drei Foveon-Schichten frei auf Rot, Grün und Blau; nur sinnvoll mit einer zu Infrarot umgebauten Kamera")), |ui| {
            let mut an = layer_mixer_on();
            if toggle(ui, &mut an) {
                set_layer_mixer(an);
            }
        });
    });
    group(ui, crate::tr("Foveon: Quattro"), |ui| {
        row(ui, crate::tr("Binning bei hoher ISO"), Some(crate::tr("Für neue Fotos von sd Quattro und sd Quattro H; Auto mittelt nach Sigmas Stufen ab ISO 800 benachbarte Pixel (kleineres Bild, weniger Rauschen). Je Foto änderbar unter „Details“")), |ui| {
            crate::binning_ui::prefs_switch(ui);
        });
    });
    group(ui, crate::tr("Andere Formate"), |ui| {
        row(ui, crate::tr("Startwerte aus Lightroom"), Some(crate::tr("Für Bilder ohne eigene Einstellungen; aus den Begleitdateien, der Katalog wird nicht gebraucht")), |ui| {
            let mut an = setting_bool("lightroomStartValues", true);
            if toggle(ui, &mut an) {
                library::settings::set("lightroomStartValues", json!(an));
            }
        });
        separator(ui);
        row(ui, crate::tr("Lightroom-Bearbeitung bei JPEG übernehmen"), Some(crate::tr("JPEG-, HEIC- und TIFF-Originale mit Bearbeitung in Lightroom zeigen diese im Raster und beim Öffnen; Lightroom-Exporte selbst nie")), |ui| {
            let mut an = setting_bool("lightroomJpegEdits", true);
            if toggle(ui, &mut an) {
                library::settings::set("lightroomJpegEdits", json!(an));
            }
        });
        if cfg!(target_os = "macos") {
            separator(ui);
            // the decoder version of the automatic fallback (Nikon High Efficiency): see rawdev::apple
            row(ui, crate::tr("Apple-Version für Nikon HE"), Some(crate::tr("Nikon „Hohe Effizienz“ kann graineo noch nicht dekodieren; dann dekodiert macOS. „Bis RAW 8“ ist schneller und entrauscht nicht zusätzlich, „Neueste“ nimmt RAW 9, wo macOS es anbietet (3- bis 7-mal langsamer). Gilt für Fotos ohne festgehaltene Version")), |ui| {
                let key = rawdev::apple::FALLBACK_SETTING;
                let mut newest = library::settings::value(key).as_str() == Some("newest");
                if lr::segmented(ui, &mut newest, &[(false, "Bis RAW 8"), (true, "Neueste")]) {
                    library::settings::set(key, json!(if newest { "newest" } else { "8" }));
                }
            });
        }
    });
    group(ui, crate::tr("Personen"), |ui| {
        // where graineo computes the detection itself (no Apple Vision), say so honestly
        let description = if library::people::detection_computed_itself() {
            crate::tr("Läuft im Hintergrund mit niedriger Priorität; vor dem einmaligen Laden des Gesichtsmodells (39 MB) fragt graineo nach. Auf diesem System rechnet graineo die Gesichtserkennung selbst; das kann länger dauern. Wirkt ab dem nächsten Start")
        } else {
            crate::tr("Läuft im Hintergrund mit niedriger Priorität; vor dem einmaligen Laden des Gesichtsmodells (39 MB) fragt graineo nach. Wirkt ab dem nächsten Start")
        };
        row(ui, crate::tr("Gesichter automatisch erkennen"), Some(description), |ui| {
            let mut an = setting_bool("facesAutomatic", library::people::automatic_default());
            if toggle(ui, &mut an) {
                library::settings::set("facesAutomatic", json!(an));
            }
        });
    });
}

fn export(ui: &mut egui::Ui) {
    let mut e = library::settings::value("export");
    if e.is_null() {
        e = json!({"format": "jpeg", "quality": 92, "engine": "spp", "reveal": true});
    }
    let before = e.clone();
    group(ui, crate::tr("Vorgaben für den Export-Dialog"), |ui| {
        row(ui, crate::tr("Format"), None, |ui| {
            let mut f = e["format"].as_str().unwrap_or("jpeg").to_string();
            if lr::segmented(ui, &mut f, &[("jpeg".to_string(), "JPEG"), ("jxl".to_string(), "JPEG XL"), ("tiff".to_string(), "TIFF 16 Bit"), ("dng".to_string(), "DNG (X3F)")]) {
                e["format"] = json!(f);
            }
        });
        separator(ui);
        row(ui, crate::tr("JPEG-Qualität"), None, |ui| {
            let mut q = e["quality"].as_u64().unwrap_or(92) as u8;
            if ui.add(egui::Slider::new(&mut q, 60..=100)).changed() {
                e["quality"] = json!(q);
            }
        });
        separator(ui);
        row(ui, crate::tr("Foveon-Berechnung"), None, |ui| {
            let mut g = e["engine"].as_str().unwrap_or("spp").to_string();
            if lr::segmented(ui, &mut g, &[("spp".to_string(), "Foveon-Verarbeitung"), ("own".to_string(), "Schneller")]) {
                e["engine"] = json!(g);
            }
        });
        separator(ui);
        row(ui, crate::tr("Danach im Finder zeigen"), None, |ui| {
            let mut r = e["reveal"].as_bool().unwrap_or(true);
            if toggle(ui, &mut r) {
                e["reveal"] = json!(r);
            }
        });
        separator(ui);
        crate::keyword_export::settings_rows(ui);
    });
    if e != before {
        library::settings::set("export", e);
    }
}

fn lenses_section(ui: &mut egui::Ui) {
    let z = state(ui);
    group(ui, crate::tr("Quellen (in dieser Reihenfolge)"), |ui| {
        row(ui, crate::tr("Eigene Profile"), None, |ui| match &z {
            Some(z) => {
                if ui.button(crate::tr("Im Finder zeigen")).clicked() {
                    let p = library::data_dir().join("lens-profiles/own");
                    let _ = std::fs::create_dir_all(&p);
                    crate::system::open(&p);
                }
                value(ui, &format!("{}", z.own_profiles), if z.own_profiles > 0 { Signal::Green } else { Signal::Gray });
            }
            None => determining(ui),
        });
        if let Some(z) = &z {
            for (path, name) in &z.own_list {
                separator(ui);
                row(ui, name, Some(crate::tr("Wirkt vor allen anderen Profilen; im Papierkorb wiederherstellbar")), |ui| {
                    if ui.button(crate::tr("In den Papierkorb")).clicked() {
                        let _ = trash::delete(path);
                        state_new();
                    }
                });
            }
        }
        separator(ui);
        row(ui, crate::tr("Objektiv vermessen"), None, |ui| {
            if ui.button(crate::tr("Assistent …")).clicked() {
                OPEN.store(false, Ordering::Relaxed);
                crate::lens_wizard::open();
            }
        });
        separator(ui);
        row(ui, crate::tr("Adobe (LCP)"), Some(crate::tr("Aus einem selbst installierten Adobe DNG Converter, nur gelesen")), |ui| match &z {
            Some(z) => {
                if ui.button(crate::tr("DNG Converter …")).clicked() {
                    crate::system::open("https://helpx.adobe.com/camera-raw/using/adobe-dng-converter.html");
                }
                value(ui, if z.adobe.is_some() { "gefunden" } else { crate::tr("nicht gefunden") }, if z.adobe.is_some() { Signal::Green } else { Signal::Yellow });
            }
            None => determining(ui),
        });
        separator(ui);
        row(ui, crate::tr("Zusätzlicher Ordner"), None, |ui| {
            let p = library::settings::value("lcpFolder").as_str().unwrap_or("–").to_string();
            if path(ui, &p) {
                crate::file_dialog::folder(ui.ctx(), "prefs-lcp", |d| d);
            }
            if let Some(p) = crate::file_dialog::fetch_one("prefs-lcp") {
                library::settings::set("lcpFolder", json!(p));
            }
        });
        separator(ui);
        row(ui, crate::tr("Kamera"), None, |ui| value(ui, crate::tr("aus der RAW-Datei"), Signal::Green));
        separator(ui);
        row(ui, "lensfun", None, |ui| value(ui, crate::tr("Auszug enthalten (CC BY-SA)"), Signal::Green));
    });
}

fn ai(ui: &mut egui::Ui, ctx: &egui::Context) {
    let z = state(ui);
    // choice of the search model in stages (prefs_search_model.rs), first: the most important decision of the page
    group(ui, crate::tr("Suchmodell"), |ui| crate::prefs_search_model::cards(ui, ctx));
    group(ui, crate::tr("KI-Beschreibungen"), |ui| {
        row(ui, crate::tr("Beschreibungen"), None, |ui| match &z {
            Some(z) if z.ai_count > 0 => value(ui, &crate::trf!("{a} Fotos beschrieben", a = thousands(z.ai_count)), Signal::Green),
            Some(_) => value(ui, crate::tr("noch keine"), Signal::Yellow),
            None => determining(ui),
        });
    });
    group(ui, crate::tr("Bedeutungssuche"), |ui| {
        row(ui, crate::tr("Nach Bedeutung ergänzen"), Some(crate::tr("„Ähnlich in der Bedeutung“ unter den Worttreffern")), |ui| {
            let mut an = setting_bool("semanticSearch", true);
            if toggle(ui, &mut an) {
                library::settings::set("semanticSearch", json!(an));
            }
        });

        // the text model is chosen with the stage cards above (prefs_search_model.rs)
        separator(ui);
        row(ui, crate::tr("Bildvektoren"), None, |ui| match &z {
            Some(z) => value(ui, &crate::trf!("{a} Fotos · {b} Beschreibungen", a = thousands(z.vectors), b = thousands(z.text_vectors)), if z.vectors > 0 { Signal::Green } else { Signal::Gray }),
            None => determining(ui),
        });
        separator(ui);
        row(ui, crate::tr("Neue Fotos im Hintergrund"), None, |ui| {
            let mut an = setting_bool("imageEncoderBackground", true);
            if toggle(ui, &mut an) {
                library::settings::set("imageEncoderBackground", json!(an));
            }
        });
    });
    group(ui, crate::tr("Sprach-KI"), |ui| {
        row(ui, crate::tr("Anbieter"), None, |ui| value(ui, crate::tr("noch ohne Funktion"), Signal::Gray));
    });
}

/// Module whose removal is to be confirmed (ID, bytes).
static REMOVE: Mutex<Option<(&'static str, u64)>> = Mutex::new(None);
/// Non-commercial module whose download waits for the confirmation (ID, box ticked).
static CONFIRM_NC: Mutex<Option<(&'static str, bool)>> = Mutex::new(None);

/// Confirmation before loading a module that may only be used non-commercially: licence, its core in one sentence,
/// link, and a box the user must tick. The confirmation is stored with date, model, revision and graineo version.
fn non_commercial_dialog(ctx: &egui::Context) {
    let Some((id, mut ticked)) = *CONFIRM_NC.lock().unwrap() else { return };
    let Some(d) = library::module::ALL.iter().copied().find(|x| x.id == id) else { return };
    let Some(t) = library::module::non_commercial(d) else { return };
    // the button follows the box of the frame before (the box is drawn inside the dialog)
    let was = ticked;
    let a = crate::dialog::show(
        ctx,
        "modul-nicht-kommerziell",
        &crate::trf!("„{name}“ laden?", name = short_model_name(d)),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::warning(ui, crate::tr("Nur für nicht-kommerzielle Nutzung"));
            crate::dialog::text(ui, &crate::trf!("Lizenz: {l}. {s}", l = t.name, s = crate::tr(t.summary)));
            ui.hyperlink_to(crate::tr("Lizenztext lesen"), t.url);
            crate::dialog::hint(ui, &crate::trf!("Das Modell wird nicht von graineo bereitgestellt, sondern auf deinen Wunsch von der Originalquelle geladen ({s}). Alle Funktionen gibt es auch mit dem freien Modell.", s = format!("huggingface.co/{}", d.repo)));
            ui.add_space(4.0);
            ui.checkbox(&mut ticked, crate::tr("Ich nutze graineo mit diesem Modell ausschließlich nicht-kommerziell"));
        },
        crate::dialog::Buttons::confirm(crate::tr("Laden"), was),
    );
    match a {
        crate::dialog::Answer::Primary if was => {
            *CONFIRM_NC.lock().unwrap() = None;
            match library::module::confirm_non_commercial(d, env!("CARGO_PKG_VERSION")) {
                Ok(()) => install_module(ctx, id),
                Err(e) => crate::log(&format!("non-commercial confirmation not stored: {e}")),
            }
        }
        crate::dialog::Answer::Cancel => *CONFIRM_NC.lock().unwrap() = None,
        _ => {
            if let Some(c) = CONFIRM_NC.lock().unwrap().as_mut() {
                c.1 = ticked;
            }
        }
    }
}

/// Download and install a module in the background (progress in its row).
fn install_module(ctx: &egui::Context, id: &'static str) {
    let c = ctx.clone();
    start_work(id, move |cancel, report_to| {
        let d = library::module::ALL.iter().find(|x| x.id == id).ok_or("unbekanntes Modell")?;
        library::module::install(d, &|a, b, f| {
            let t = crate::trf!("{a} von {b}", a = gb(a), b = gb(b));
            let t = if f.starts_with("prüfe") { crate::trf!("{t} · prüft", t = t) } else { t };
            report_to(a as f32 / b.max(1) as f32, &t);
            c.request_repaint();
        }, &cancel)?;
        Ok("installiert".into())
    });
}

fn module(ui: &mut egui::Ui, ctx: &egui::Context) {
    let z = state(ui);
    // test hook (screenshots, own data folder only): GRAINEO_MODULE_CONFIRM=<module id> opens the confirmation once
    static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ASKED.swap(true, Ordering::Relaxed) {
        if let Ok(id) = crate::env::var("GRAINEO_MODULE_CONFIRM") {
            if let Some(d) = library::module::ALL.iter().find(|d| d.id == id) {
                *CONFIRM_NC.lock().unwrap() = Some((d.id, false));
            }
        }
    }
    non_commercial_dialog(ctx);
    // "Remove model?" (destructive)
    let question = *REMOVE.lock().unwrap();
    if let Some((id, bytes)) = question {
        let name = library::module::ALL.iter().find(|x| x.id == id).map_or(id, |d| d.name);
        let a = crate::dialog::show(
            ctx,
            "modul-entfernen",
            &crate::trf!("„{name}“ entfernen?", name = name),
            crate::dialog::WIDTH,
            |ui| {
                crate::dialog::text(ui, crate::tr("Die Modelldateien werden gelöscht. Funktionen, die das Modell brauchen, sind danach aus; neu installieren geht jederzeit."));
                crate::dialog::chip(ui, &gb(bytes));
            },
            crate::dialog::Buttons::destructive(crate::tr("Entfernen"), true),
        );
        match a {
            crate::dialog::Answer::Primary => {
                *REMOVE.lock().unwrap() = None;
                start_work(id, move |_, report_to| {
                    report_to(0.5, "entfernt …");
                    let d = library::module::ALL.iter().find(|x| x.id == id).ok_or("unbekanntes Modell")?;
                    library::module::remove(d).map_err(|e| e.to_string())?;
                    library::semantic::text_model_changed();
                    *STATE.lock().unwrap() = None;
                    Ok("entfernt".into())
                });
                // the row tells removing from loading by this text from the first frame on
                if let Some(a) = WORK.lock().unwrap().iter_mut().find(|a| a.what == id && !a.done) {
                    a.text = "entfernt …".into();
                }
            }
            crate::dialog::Answer::Cancel => *REMOVE.lock().unwrap() = None,
            crate::dialog::Answer::Open => {}
        }
    }
    group(ui, crate::tr("Modelle"), |ui| {
        // search models (standard text model, M-CLIP, SigLIP 2) are loaded with the stage cards under "Suche"
        // (prefs_search_model.rs), not twice here – the row "Suchmodelle → unter Suche" below points there; every other
        // module (image encoder for "Ähnliche Fotos" …) gets the same quiet row, new ones without special case
        let search_ids = [library::module::TEXT.id, library::module::MCLIP.id];
        let listed: Vec<&library::module::Definition> = library::module::ALL
            .iter()
            .copied()
            .filter(|d| !search_ids.contains(&d.id) && !library::module::SEARCH.iter().any(|x| x.id == d.id))
            // withheld modules (no download offered) only appear once installed, to be removed
            .filter(|d| {
                library::module::offered(d)
                    || z.as_ref().is_some_and(|z| z.module.iter().any(|m| m.0 == d.id && m.1 == library::module::Status::Installed))
            })
            .collect();
        let ids: Vec<&str> = listed.iter().map(|d| d.id).collect();
        show_blocked(ui, &ids, false);
        for (k, d) in listed.iter().enumerate() {
            if k > 0 {
                separator(ui);
            }
            let st = z.as_ref().and_then(|z| z.module.iter().find(|m| m.0 == d.id).cloned()).map(|m| (m.1, m.2));
            model_row(ui, ctx, d, st);
        }
    });
    group(ui, crate::tr("Weitere"), |ui| {
        row(ui, crate::tr("lensfun-Objektivprofile"), None, |ui| value(ui, "enthalten", Signal::Green));
        separator(ui);
        row(ui, crate::tr("Adobe-Objektivprofile"), None, |ui| match &z {
            Some(z) => value(ui, if z.adobe.is_some() { "gefunden" } else { crate::tr("nicht gefunden") }, if z.adobe.is_some() { Signal::Green } else { Signal::Yellow }),
            None => determining(ui),
        });
        separator(ui);
        row(ui, crate::tr("Filmsimulation (spektrafilm)"), None, |ui| value(ui, "enthalten", Signal::Green));
        separator(ui);
        // the search models (text models, SigLIP 2) are loaded and removed on the search page (not offered twice)
        row(ui, crate::tr("Suchmodelle"), None, |ui| value(ui, crate::tr("unter „Suche“"), Signal::Gray));
        separator(ui);
        row(ui, crate::tr("Sprach-KI"), None, |ui| value(ui, crate::tr("später"), Signal::Gray));
        separator(ui);
        row(ui, crate::tr("Ordner"), None, |ui| {
            path_text(ui, &library::semantic::modules_folder().display().to_string(), 400.0);
        });
    });
}

/// Model name without the bracket ("Text-Encoder (semantische Suche)" → "Text-Encoder"), translated.
fn short_model_name(d: &library::module::Definition) -> String {
    let n = crate::tr(d.name);
    n.split(" (").next().unwrap_or(n).trim().to_string()
}

/// Short license ("MIT (OpenAI CLIP), model card …" → "MIT").
fn short_license(l: &str) -> &str {
    l.split([' ', ',']).next().unwrap_or(l)
}

const MODEL_BUTTON_W: f32 = 112.0;
const MODEL_STATUS_W: f32 = 190.0;

/// One model (Settings → Modules): name and status with ONE button per state (Installieren / Fortsetzen /
/// Abbrechen / Entfernen …); below one dimmed line: purpose and license, while working the progress with percent
/// and size, an error or why the button waits. Fixed heights, so nothing jumps when the state changes.
fn model_row(ui: &mut egui::Ui, ctx: &egui::Context, d: &'static library::module::Definition, st: Option<(library::module::Status, u64)>) {
    use library::module::Status;
    let total: u64 = d.files.iter().map(|f| f.size).sum();
    let own = work_of(d.id);
    let busy = own.as_ref().is_some_and(|a| !a.done);
    let removing = busy && own.as_ref().is_some_and(|a| a.text.starts_with("entfern"));
    // other work of the settings (backup, sidecars) still runs alone; other models do not lock this one
    let ids: Vec<&str> = library::module::ALL.iter().map(|d| d.id).collect();
    let other = running_other(&ids);
    let name = short_model_name(d);
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let Some((st, size)) = st else {
            determining(ui);
            return;
        };
        let (label, enabled) = if busy && !removing {
            (crate::tr("Abbrechen"), true)
        } else {
            match st {
                Status::Installed => (crate::tr("Entfernen …"), !busy && other.is_none()),
                Status::Partial => (crate::tr("Fortsetzen"), !busy && other.is_none()),
                Status::Missing => (crate::tr("Installieren"), !busy && other.is_none()),
            }
        };
        let b = ui.add_enabled(enabled, egui::Button::new(label).min_size(egui::vec2(MODEL_BUTTON_W, 0.0)));
        if b.clicked() {
            if busy {
                if let Some(a) = &own {
                    a.cancel.store(true, Ordering::Relaxed);
                }
            } else if st == Status::Installed {
                *REMOVE.lock().unwrap() = Some((d.id, size));
            } else if library::module::non_commercial(d).is_some() && library::module::confirmation(d).is_none() {
                // never without the user's confirmation of non-commercial use
                *CONFIRM_NC.lock().unwrap() = Some((d.id, false));
            } else {
                install_module(ctx, d.id);
            }
        }
        let status = if removing {
            crate::tr("wird entfernt …").to_string()
        } else if let Some(a) = own.as_ref().filter(|_| busy) {
            crate::trf!("lädt {p} %", p = (a.progress * 100.0).floor() as u32)
        } else {
            match st {
                Status::Installed => crate::trf!("installiert · {a}", a = gb(size)),
                Status::Partial => crate::trf!("unvollständig · {a} von {b}", a = gb(size), b = gb(total)),
                Status::Missing => crate::trf!("nicht installiert · {a}", a = gb(total)),
            }
        };
        let color = if st == Status::Installed && !busy { lr::TEXT } else { lr::TEXT_DIM };
        ui.allocate_ui_with_layout(egui::vec2(MODEL_STATUS_W, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(status).color(color)).truncate());
        });
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let r = ui.add(egui::Label::new(egui::RichText::new(&name).font(lr::semibold(lr::UI_SIZE)).color(lr::TEXT)).truncate().sense(egui::Sense::hover()));
            lr::hover_tip(ui, &r, crate::tr(d.name));
        });
    });
    // second line, always the same height
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let dim = |t: String, c: Color32| egui::Label::new(egui::RichText::new(t).small().color(c)).truncate().sense(egui::Sense::hover());
        if let Some(a) = own.as_ref().filter(|_| busy && !removing) {
            ui.add(egui::ProgressBar::new(a.progress).desired_width(240.0).desired_height(6.0));
            ui.add(dim(crate::menu::i18n::tr_msg(&a.text).to_string(), lr::TEXT_DIM));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        } else if let Some(a) = own.as_ref().filter(|a| a.done && a.failed) {
            ui.add(dim(crate::menu::i18n::tr_msg(&a.text).to_string(), Color32::from_rgb(0xe0, 0x8c, 0x70)));
        } else if let Some(o) = other.as_ref().filter(|_| !busy) {
            let t = match (o.progress > 0.0).then_some(o.progress) {
                Some(p) => crate::trf!("Gesperrt, solange {w} läuft ({p} %)", w = work_name(&o.what), p = (p * 100.0).floor() as u32),
                None => crate::trf!("Gesperrt, solange {w} läuft", w = work_name(&o.what)),
            };
            ui.add(dim(t, Color32::from_rgb(0xe0, 0xb0, 0x40)));
        } else if let Some(t) = library::module::non_commercial(d) {
            // visibly marked: only for non-commercial use, licence and link
            let note = crate::trf!("Nur für nicht-kommerzielle Nutzung · {l}", l = t.name);
            let r = ui.add(dim(note, Color32::from_rgb(0xe0, 0xb0, 0x40)));
            let confirmed = library::module::confirmation(d).is_some();
            lr::hover_tip(ui, &r, &crate::trf!("{p}. Lizenz: {l} · Quelle: huggingface.co/{r} · nicht von graineo bereitgestellt{c}", p = crate::tr(d.purpose), l = d.license, r = d.repo, c = if confirmed { crate::tr(" · nicht-kommerzielle Nutzung bestätigt").to_string() } else { String::new() }));
            ui.hyperlink_to(egui::RichText::new(crate::tr("Lizenz")).small(), t.url);
        } else {
            let t = crate::trf!("{a} · Lizenz {b}", a = crate::tr(d.purpose), b = short_license(d.license));
            let r = ui.add(dim(t, lr::TEXT_DIM));
            lr::hover_tip(ui, &r, &crate::trf!("Lizenz: {l} · Quelle: huggingface.co/{r}", l = d.license, r = d.repo));
        }
    });
}

fn storage_section(ui: &mut egui::Ui) {
    let z = state(ui);
    if crate::apple_photos_ui::an() {
        group(ui, crate::tr("Apple Fotos (Zwischenspeicher)"), |ui| {
            row(ui, crate::tr("Belegt"), Some(crate::tr("Originale aus iCloud zum Bearbeiten, höchstens 2 GB; ältere fallen von selbst heraus. In Apple Fotos ändert „Leeren“ nichts.")), |ui| {
                let (b, n) = crate::apple_photos_ui::cache_status();
                if ui.add_enabled(n > 0, egui::Button::new(crate::tr("Leeren"))).clicked() {
                    crate::apple_photos_ui::clear_cache();
                }
                value(ui, &crate::trf!("{a} · {b} Originale", a = gb(b), b = n), Signal::Gray);
            });
        });
    }
    group(ui, crate::tr("Entwickelte Vorschauen"), |ui| {
        let st = crate::stack::snapshot();
        row(ui, crate::tr("Alle Vorschauen neu rechnen"), Some(crate::tr("Rechnet die Vorschauen von JPEG-, HEIC-, TIFF- und PNG-Originalen, die in Lightroom bearbeitet wurden, mit graineos eigener Entwicklung statt der unbearbeiteten Datei, damit beim Bearbeiten kein Sprung entsteht. Läuft im Hintergrund und setzt nach einem Anhalten fort. Selbst bearbeitete Fotos bleiben unberührt.")), |ui| {
            if st.running {
                if ui.button(crate::tr("Anhalten")).clicked() {
                    crate::stack::cancel();
                }
            } else {
                if ui.button(crate::tr("Von vorn")).clicked() {
                    crate::stack::start(true);
                }
                let resume = st.before_done > 0 || st.done > 0;
                if ui.button(if resume { crate::tr("Fortsetzen") } else { crate::tr("Starten") }).clicked() {
                    crate::stack::start(false);
                }
            }
        });
        if st.running || st.total > 0 || !st.message.is_empty() {
            separator(ui);
            row(ui, crate::tr("Stand"), None, |ui| {
                if st.running {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                    let done = st.done.saturating_sub(st.before_done) + st.skipped + st.error;
                    let rest = st.total.saturating_sub(st.before_done + done);
                    let secs = st.start.map_or(0.0, |t| t.elapsed().as_secs_f64());
                    let eta = if done >= 10 && secs > 5.0 { crate::trf!(" · noch etwa {a} Std.", a = format!("{:.1}", rest as f64 * secs / done as f64 / 3600.0).de_comma()) } else { String::new() };
                    if st.total == 0 {
                        determining(ui);
                    } else {
                        value(ui, &crate::trf!("{a} von {b}{c}", a = st.done, b = st.total, c = eta), Signal::Yellow);
                    }
                } else if !st.message.is_empty() {
                    value(ui, &st.message, Signal::Gray);
                }
            });
        }
    });
    {
        // Trash on network drives
        let mut bin = NET_BIN.lock().unwrap().clone();
        if bin.is_none() && !NET_BIN_RUNNING.load(Ordering::SeqCst) {
            net_bin_reload();
        }
        if NET_BIN_RUNNING.load(Ordering::SeqCst) {
            bin = None;
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
        }
        group(ui, crate::tr("Papierkorb auf Netzlaufwerken"), |ui| {
            row(ui, crate::tr("Ordner"), Some(crate::tr("Dorthin wandert, was du auf einem Netzlaufwerk löschst (dort gibt es keinen Papierkorb). Relativ zum Stammordner des Laufwerks oder ein Ordner auf demselben Laufwerk.")), |ui| {
                if ui.small_button(crate::tr("Standard")).clicked() {
                    library::network_trash::set(library::network_trash::STANDARD);
                    *NET_BIN.lock().unwrap() = None;
                }
                let mut t = library::network_trash::setting();
                if crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut t).desired_width(260.0)).changed() {
                    library::network_trash::set(&t);
                    *NET_BIN.lock().unwrap() = None;
                }
            });
            separator(ui);
            let (n, bytes) = bin.as_ref().map_or((0, 0), |v| v.iter().fold((0, 0), |a, (_, n, b)| (a.0 + n, a.1 + b)));
            row(ui, crate::tr("Inhalt"), None, |ui| match &bin {
                None => determining(ui),
                Some(_) if NET_BIN_QUESTION.load(Ordering::SeqCst) => {
                    if ui.button(crate::tr("Abbrechen")).clicked() {
                        NET_BIN_QUESTION.store(false, Ordering::SeqCst);
                    }
                    if ui.add(egui::Button::new(egui::RichText::new(crate::tr("Endgültig leeren")).color(Color32::from_rgb(0xe0, 0x6c, 0x5a)))).clicked() {
                        NET_BIN_QUESTION.store(false, Ordering::SeqCst);
                        net_bin_action(true);
                    }
                    value(ui, &crate::trf!("{a} Dateien · {b} – wirklich endgültig löschen?", a = n, b = gb(bytes)), Signal::Red);
                }
                Some(_) => {
                    if ui.add_enabled(n > 0, egui::Button::new(crate::tr("Leeren …"))).clicked() {
                        NET_BIN_QUESTION.store(true, Ordering::SeqCst);
                    }
                    if ui.add_enabled(n > 0, egui::Button::new(crate::tr("Alle zurücklegen"))).clicked() {
                        net_bin_action(false);
                    }
                    value(ui, &crate::trf!("{a} Dateien · {b}", a = n, b = gb(bytes)), Signal::Gray);
                }
            });
            let m = NET_BIN_MESSAGE.lock().unwrap().clone();
            if !m.is_empty() && bin.is_some() {
                dialog_hint_small(ui, &m);
            }
        });
    }
    group(ui, crate::tr("Datenordner"), |ui| {
        let Some(z) = &z else {
            determining(ui);
            return;
        };
        for (i, (k, n)) in z.sizes.iter().enumerate().skip(1) {
            if i > 1 {
                separator(ui);
            }
            row(ui, k, None, |ui| value(ui, &gb(*n), Signal::Gray));
        }
        separator(ui);
        row(ui, crate::tr("Foveon-Basis (Merrill/Quattro)"), None, |ui| value(ui, &gb(z.spp.0), Signal::Gray));
        separator(ui);
        let a = if z.free < 10_000_000_000 { Signal::Red } else if z.free < 20_000_000_000 { Signal::Yellow } else { Signal::Green };
        row(ui, crate::tr("Frei auf dem Datenträger"), None, |ui| value(ui, &gb(z.free), a));
    });
}

/// Small grey hint text below a row.
fn dialog_hint_small(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(text)).small().color(lr::TEXT_DIM)).wrap());
}

pub(crate) fn thousands(n: i64) -> String {
    let s = n.to_string();
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            o.push('\u{202f}');
        }
        o.push(c);
    }
    o
}
