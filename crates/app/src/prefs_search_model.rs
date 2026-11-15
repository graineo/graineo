//! Settings → Search: choice of the search model in three stages ("Schnell – klein", "Ausgewogen",
//! "Beste Treffer – groß"), one calm card per stage with purpose, strengths and weaknesses in everyday
//! words, languages, download size, memory, speed, quality, maker and licence. One button per state
//! (install / activate); the gold main button only on the card the user is most likely to want next.
//! Downloading and reading the photos in run in the background; their progress shows here and in the
//! activity display. The facts come from `library::search_model` (measured values).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32};
use library::module::Status;
use library::search_model::{self as sm, Stage, STAGES};

use crate::lr;

/// Facts read in the background (never in the UI thread): module states, vector progress per stage,
/// number of photos.
#[derive(Clone, Default)]
struct Facts {
    installed: Vec<(Stage, Status)>,
    vectors: Vec<(Stage, i64, i64)>,
    read: Option<Instant>,
}

static FACTS: Mutex<Option<Facts>> = Mutex::new(None);
static FACTS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Running download: stage, done bytes, total, cancel, error of the last attempt.
struct Download {
    stage: Stage,
    done: u64,
    total: u64,
    cancel: Arc<AtomicBool>,
    error: Option<String>,
    finished: bool,
}

static DOWNLOAD: Mutex<Option<Download>> = Mutex::new(None);

/// Stage whose removal is to be confirmed.
static REMOVE: Mutex<Option<Stage>> = Mutex::new(None);

fn refresh(force: bool) {
    let stale = FACTS.lock().unwrap().as_ref().is_none_or(|f| f.read.is_none_or(|t| t.elapsed() > Duration::from_secs(if sm::running() { 2 } else { 10 })));
    if !(force || stale) || FACTS_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let mut f = Facts { read: Some(Instant::now()), ..Default::default() };
        for s in STAGES {
            let st = s.modules().iter().map(|d| library::module::status(d).0).fold(Status::Installed, |a, b| match (a, b) {
                (Status::Installed, x) => x,
                (Status::Missing, Status::Missing) => Status::Missing,
                _ => Status::Partial,
            });
            // "Schnell" also works with a text encoder found in a test cache
            // the CLIP stages also work with a text model found in a test cache
            let st = if s.text_model().is_some() && s.folder().is_some() && st != Status::Installed { Status::Installed } else { st };
            f.installed.push((s, st));
        }
        if let Ok(db) = library::db::Db::standard() {
            for s in STAGES {
                if let Some((done, total)) = sm::progress_of(&db, s) {
                    f.vectors.push((s, done, total));
                }
            }
        }
        *FACTS.lock().unwrap() = Some(f);
        FACTS_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// "etwa 2 Std. 10 Min." / "etwa 25 Min." / "unter 1 Min."
pub fn about(d: Duration) -> String {
    let m = (d.as_secs() + 30) / 60;
    if m < 1 {
        crate::tr("unter 1 Min.").to_string()
    } else if m < 60 {
        crate::trf!("etwa {m} Min.", m = m)
    } else {
        crate::trf!("etwa {h} Std. {m} Min.", h = m / 60, m = m % 60)
    }
}

fn mb(n: u32) -> String {
    crate::dialog::size(u64::from(n) * 1_000_000)
}

/// Activity display: reading the photos in for a new search model (or downloading it).
pub fn task() -> Option<crate::activity::Task> {
    use crate::activity::{Rank, Task};
    if let Some(p) = sm::progress() {
        let what = crate::tr(p.stage.info().title);
        let mut t = if p.part == "Fotos" {
            crate::trf!("Suchmodell „{m}“: {a} von {b} Fotos eingelesen", m = what, a = crate::dialog::number(p.done as usize), b = crate::dialog::number(p.total as usize))
        } else {
            crate::trf!("Suchmodell „{m}“: Beschreibungen {a} von {b}", m = what, a = crate::dialog::number(p.done as usize), b = crate::dialog::number(p.total as usize))
        };
        if let Some(r) = p.remaining() {
            t.push_str(&format!(" · {}", crate::trf!("noch {t}", t = about(r))));
        }
        return Some(Task::new(Rank::Settings, t));
    }
    let d = DOWNLOAD.lock().unwrap();
    let d = d.as_ref().filter(|d| !d.finished)?;
    Some(Task::new(Rank::Settings, crate::trf!("Suchmodell „{m}“ wird installiert · {p} %", m = crate::tr(d.stage.info().title), p = d.done * 100 / d.total.max(1))))
}

fn start_download(s: Stage, ctx: &egui::Context) {
    let cancel = Arc::new(AtomicBool::new(false));
    let total = s.download_bytes();
    *DOWNLOAD.lock().unwrap() = Some(Download { stage: s, done: 0, total, cancel: cancel.clone(), error: None, finished: false });
    let c = ctx.clone();
    std::thread::spawn(move || {
        library::platform::background_priority();
        let mut before = 0u64;
        let mut r = Ok(());
        for d in s.modules() {
            r = library::module::install(
                d,
                &|a, _, _| {
                    if let Some(x) = DOWNLOAD.lock().unwrap().as_mut() {
                        x.done = before + a;
                    }
                    c.request_repaint_after(Duration::from_millis(250));
                },
                &cancel,
            );
            if r.is_err() {
                break;
            }
            before += d.files.iter().map(|f| f.size).sum::<u64>();
        }
        if let Some(x) = DOWNLOAD.lock().unwrap().as_mut() {
            x.finished = true;
            x.error = r.err();
        }
        sm::modules_changed();
        // a newly installed text model may become the chosen one (library::semantic)
        library::semantic::text_model_changed();
        refresh(true);
        c.request_repaint();
    });
}

/// Modules several stages use (CLIP image encoder, standard text model): never removed from a card.
fn shared(d: &library::module::Definition) -> bool {
    d.id == library::module::IMAGE.id || d.id == library::module::TEXT.id
}

fn busy() -> bool {
    DOWNLOAD.lock().unwrap().as_ref().is_some_and(|d| !d.finished)
}

/// Stage choice: one card per stage (inside the group "Suchmodell" of the search page).
pub fn cards(ui: &mut egui::Ui, ctx: &egui::Context) {
    refresh(false);
    remove_question(ctx);
    let facts = FACTS.lock().unwrap().clone();
    let active = sm::active();
    let target = sm::target();
    if sm::running() || busy() {
        ctx.request_repaint_after(Duration::from_millis(500));
    }
    // the gold button: the recommended stage while it is not active (one per window)
    let gold = (active != sm::RECOMMENDED && target != Some(sm::RECOMMENDED)).then_some(sm::RECOMMENDED);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::trf!("Für die meisten: {s}", s = crate::tr(sm::RECOMMENDED.info().title))).color(lr::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::help::info(ui, "suchmodell", "Suchmodell", &crate::help::SEARCH_MODEL);
        });
    });
    for s in STAGES {
        ui.add_space(6.0);
        card(ui, ctx, s, facts.as_ref(), active, target, gold == Some(s));
    }
}

fn fact(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(150.0, 16.0), egui::Sense::hover());
        ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, crate::tr(name), egui::FontId::proportional(lr::UI_SIZE - 1.0), lr::TEXT_DIM);
        ui.add(egui::Label::new(egui::RichText::new(value).size(lr::UI_SIZE - 1.0).color(lr::TEXT)).wrap());
    });
}

/// Five small squares, `n` filled.
fn scale(ui: &mut egui::Ui, n: u8) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(5.0 * 10.0, 12.0), egui::Sense::hover());
    for i in 0..5u8 {
        let b = egui::Rect::from_min_size(egui::pos2(r.left() + f32::from(i) * 10.0, r.center().y - 3.5), egui::vec2(7.0, 7.0));
        if i < n {
            ui.painter().rect_filled(b, 0.0, Color32::from_gray(0xc8));
        } else {
            ui.painter().rect_stroke(b, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x5a)), egui::StrokeKind::Inside);
        }
    }
    resp
}

fn card(ui: &mut egui::Ui, ctx: &egui::Context, s: Stage, facts: Option<&Facts>, active: Stage, target: Option<Stage>, gold: bool) {
    let i = s.info();
    let installed = facts.and_then(|f| f.installed.iter().find(|x| x.0 == s).map(|x| x.1.clone()));
    let vectors = facts.and_then(|f| f.vectors.iter().find(|x| x.0 == s).map(|x| (x.1, x.2)));
    let is_active = s == active;
    let frame_color = if is_active { Color32::from_gray(0x55) } else { Color32::from_gray(0x2e) };
    egui::Frame::new().fill(Color32::from_gray(0x19)).stroke(egui::Stroke::new(1.0, frame_color)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 1.0;
        // heading: name, recommendation, quality scale
        ui.horizontal(|ui| {
            let g = lr::caps(ui.painter(), crate::tr(i.title), lr::semibold(11.0), lr::TEXT);
            let (r, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
            ui.painter().galley(r.min, g, lr::TEXT);
            if s == sm::RECOMMENDED {
                ui.add_space(6.0);
                crate::dialog::chip(ui, crate::tr("Für die meisten"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let resp = scale(ui, i.quality);
                lr::hover_tip(ui, &resp, &crate::trf!("Qualität {n} von 5", n = i.quality));
                ui.label(egui::RichText::new(crate::tr("Qualität")).small().color(lr::TEXT_DIM));
            });
        });
        ui.add(egui::Label::new(egui::RichText::new(crate::tr(i.purpose)).color(lr::TEXT)).wrap());
        ui.add_space(2.0);
        fact(ui, "Für wen", crate::tr(i.for_whom));
        fact(ui, "Stärken", crate::tr(i.strengths));
        fact(ui, "Schwächen", crate::tr(i.weaknesses));
        fact(ui, "Sprachen", crate::tr(i.languages));
        fact(ui, "Größe", &crate::trf!("Download {d} · Arbeitsspeicher {a} beim Suchen, {b} beim Einlesen", d = crate::dialog::size(s.download_bytes()), a = mb(i.memory_search_mb), b = mb(i.memory_read_mb)));
        let all = vectors.map_or(0, |v| v.1);
        let read_time = about(Duration::from_millis(u64::from(i.ms_photo) * 2 * all.max(0) as u64));
        if s.needs_reindex() {
            fact(ui, "Tempo", &crate::trf!("Anfrage {q} ms · alle {n} Fotos einlesen {t}", q = i.ms_query, n = crate::dialog::number(all as usize), t = read_time));
        } else {
            fact(ui, "Tempo", &crate::trf!("Anfrage {q} ms · kein Neu-Einlesen", q = i.ms_query));
        }
        fact(ui, "Treffer auf Platz 1", &crate::trf!("{de} % deutsch · {en} % englisch", de = format!("{:.0}", i.r1_de), en = format!("{:.0}", i.r1_en)));
        fact(ui, "Herkunft", &crate::trf!("{m} · Lizenz {l}", m = crate::tr(i.maker), l = i.license));
        let complete = vectors.is_some_and(|(d, t)| d >= t);
        let reread = if !s.needs_reindex() || complete { crate::tr("nein") } else { crate::tr("ja, einmal alle Fotos") };
        fact(ui, "Neu-Einlesen nötig", reread);
        ui.add_space(4.0);
        state_row(ui, ctx, s, installed, vectors, is_active, target, gold);
    });
}

#[allow(clippy::too_many_arguments)]
fn state_row(ui: &mut egui::Ui, ctx: &egui::Context, s: Stage, installed: Option<Status>, vectors: Option<(i64, i64)>, is_active: bool, target: Option<Stage>, gold: bool) {
    let download = {
        let d = DOWNLOAD.lock().unwrap();
        d.as_ref().filter(|d| d.stage == s).map(|d| (d.done, d.total, d.finished, d.error.clone(), d.cancel.clone()))
    };
    // downloading
    if let Some((done, total, false, _, cancel)) = &download {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
                ui.vertical(|ui| crate::dialog::progress(ui, Some(*done as f32 / (*total).max(1) as f32), &crate::trf!("{a} von {b}", a = crate::dialog::size(*done), b = crate::dialog::size(*total))));
            });
        });
        return;
    }
    // reading the photos in for this stage
    if target == Some(s) {
        let p = sm::progress().filter(|p| p.stage == s);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    sm::cancel_target();
                    refresh(true);
                }
                ui.vertical(|ui| match &p {
                    Some(p) => {
                        let what = if p.part == "Fotos" {
                            crate::trf!("{a} von {b} Fotos eingelesen", a = crate::dialog::number(p.done as usize), b = crate::dialog::number(p.total as usize))
                        } else {
                            crate::trf!("Beschreibungen {a} von {b}", a = crate::dialog::number(p.done as usize), b = crate::dialog::number(p.total as usize))
                        };
                        let rest = p.remaining().map(|r| format!(" · {}", crate::trf!("noch {t}", t = about(r)))).unwrap_or_default();
                        crate::dialog::progress(ui, Some(p.done as f32 / p.total.max(1) as f32), &format!("{what}{rest}"));
                    }
                    None => crate::dialog::progress(ui, None, crate::tr("Einlesen beginnt …")),
                });
            });
        });
        ui.label(egui::RichText::new(crate::tr("Die Suche nutzt bis dahin das bisherige Modell.")).small().color(lr::TEXT_DIM));
        return;
    }
    ui.horizontal(|ui| {
        match (&installed, is_active) {
            (_, true) => {
                ui.label(egui::RichText::new("●").color(Color32::from_rgb(0x4c, 0xc2, 0x6a)));
                let more = vectors.filter(|(d, t)| d < t).map(|(d, t)| crate::trf!(" · {n} neue Fotos folgen", n = crate::dialog::number((t - d) as usize))).unwrap_or_default();
                ui.label(egui::RichText::new(format!("{}{more}", crate::tr("Aktiv"))).color(lr::TEXT));
            }
            (Some(Status::Installed), false) => {
                let have = vectors.map_or(0, |v| v.0);
                let t = if have > 0 && s.needs_reindex() { crate::trf!("installiert · {n} Fotos schon eingelesen", n = crate::dialog::number(have as usize)) } else { crate::tr("installiert").to_string() };
                ui.label(egui::RichText::new(t).color(lr::TEXT_DIM));
            }
            (Some(Status::Partial), _) => {
                ui.label(egui::RichText::new(crate::trf!("unvollständig · {a}", a = crate::dialog::size(s.download_bytes()))).color(lr::TEXT_DIM));
            }
            (Some(Status::Missing), _) => {
                ui.label(egui::RichText::new(crate::trf!("nicht installiert · {a}", a = crate::dialog::size(s.download_bytes()))).color(lr::TEXT_DIM));
            }
            (None, _) => crate::dialog::pending(ui, crate::tr("wird ermittelt …")),
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let free = !busy() && target.is_none();
            let label: Option<String> = match installed {
                Some(Status::Installed) if !is_active => Some(crate::tr("Aktivieren").to_string()),
                Some(Status::Missing) => Some(crate::tr("Installieren").to_string()),
                Some(Status::Partial) => Some(crate::tr("Fortsetzen").to_string()),
                _ => None,
            };
            if let Some(label) = label {
                let resp = if gold { lr::main_button(ui, &label, free) } else { ui.add_enabled(free, egui::Button::new(label.as_str())) };
                if !free {
                    // locked: say why
                    let why = if busy() { crate::tr("Ein anderes Suchmodell wird gerade installiert") } else { crate::tr("Ein anderes Suchmodell wird gerade eingelesen") };
                    lr::hover_tip(ui, &resp, why);
                }
                if resp.clicked() {
                    if installed == Some(Status::Installed) {
                        activate(s, ctx);
                    } else {
                        start_download(s, ctx);
                    }
                }
            }
            if s != Stage::Fast && installed.as_ref().is_some_and(|x| *x != Status::Missing) && !is_active && ui.add_enabled(!busy() && target.is_none(), egui::Button::new(crate::tr("Entfernen …"))).clicked() {
                *REMOVE.lock().unwrap() = Some(s);
            }
        });
    });
    if let Some((_, _, true, Some(e), _)) = download {
        ui.label(egui::RichText::new(crate::trf!("Installieren fehlgeschlagen: {e}", e = crate::menu::i18n::tr_msg(&e))).small().color(Color32::from_rgb(0xe0, 0x5c, 0x50)));
    }
}

fn activate(s: Stage, ctx: &egui::Context) {
    let c = ctx.clone();
    std::thread::spawn(move || {
        if let Ok(db) = library::db::Db::standard() {
            let now = sm::choose(&db, s);
            if !now {
                sm::start(|| refresh(true));
            }
        }
        refresh(true);
        c.request_repaint();
    });
}

fn remove_question(ctx: &egui::Context) {
    let Some(s) = *REMOVE.lock().unwrap() else { return };
    let a = crate::dialog::show(
        ctx,
        "suchmodell-entfernen",
        &crate::trf!("„{name}“ entfernen?", name = crate::tr(s.info().title)),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::text(ui, crate::tr("Die Modelldateien werden gelöscht. Die schon eingelesenen Fotos bleiben in der Bibliothek; nach erneutem Installieren ist das Modell ohne Neu-Einlesen wieder bereit."));
            crate::dialog::chip(ui, &crate::dialog::size(s.download_bytes()));
        },
        crate::dialog::Buttons::destructive(crate::tr("Entfernen"), true),
    );
    match a {
        crate::dialog::Answer::Primary => {
            *REMOVE.lock().unwrap() = None;
            std::thread::spawn(move || {
                // only the stage's own model; the shared CLIP image and standard text modules stay
                for d in s.modules().iter().filter(|d| !shared(d)) {
                    let _ = library::module::remove(d);
                }
                sm::modules_changed();
                library::semantic::text_model_changed();
                refresh(true);
            });
        }
        crate::dialog::Answer::Cancel => *REMOVE.lock().unwrap() = None,
        crate::dialog::Answer::Open => {}
    }
}
