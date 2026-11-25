//! Update notice: a small card at the bottom right (same place and style as the messages), drawn
//! as an overlay so nothing in the layout moves. "graineo {v} is available" with Install (main
//! button), Later, Skip this version and collapsible release notes; while installing the same card
//! shows the progress. Logic and verification live in `library::update`.
//!
//! After an installation the new version confirms its start after a few seconds (only then the previous
//! app goes to the trash); a version that did not start was put back by the relaunch helper and is
//! reported here once.
//!
//! Test hooks: `GRAINEO_UPDATE_URL` (local test server), `GRAINEO_UPDATE_AUTOINSTALL=<seconds>`
//! (only together with `GRAINEO_UPDATE_URL`: install that many seconds after the card appeared,
//! without a click). With `GRAINEO_DATA_DIR`, replaced apps go to `<data>/updates/trash` instead of
//! the system trash.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use eframe::egui;
use egui::Color32;
use library::update::{self, Check, InstallError, Manifest};

use crate::lr;

#[derive(Clone)]
enum Phase {
    Idle,
    Checking,
    Available { m: Manifest, since: f64 },
    Downloading { m: Manifest, done: u64, total: u64 },
    Installing { m: Manifest },
    /// Installed: quit, the new version starts by itself
    Restarting,
    Failed { text: String, page: bool },
}

struct State {
    phase: Phase,
    /// "Later": hidden for this session
    hidden: bool,
    /// Result of the last check for the settings page (already translated)
    line: String,
    /// One-off message for the toast
    toast: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State { phase: Phase::Idle, hidden: false, line: String::new(), toast: None });
static STARTED: AtomicBool = AtomicBool::new(false);
/// The start of this version after an installation is still to be confirmed
static CONFIRM: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Height of the card in the last frame (f32 bits), so the toast can sit above it
static HEIGHT: AtomicU32 = AtomicU32::new(0);
const WIDTH: f32 = 380.0;

fn set(phase: Phase) {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
}

/// Once at startup: tidy up after an update (Windows) and, if due, check in the background.
fn start(ctx: &egui::Context) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    for p in update::leftovers() {
        match trash::delete(&p) {
            Ok(()) => crate::log(&format!("update: removed {}", p.display())),
            Err(e) => crate::log(&format!("update: {} not removed: {e}", p.display())),
        }
    }
    match update::outcome(&update::current_version(), &dispose) {
        update::Outcome::Started(v) => {
            crate::log(&format!("update: {v} started, confirming in {} s", update::CONFIRM_AFTER));
            CONFIRM.store(true, Ordering::SeqCst);
        }
        update::Outcome::RolledBack(v) => {
            crate::log(&format!("update: {v} did not start, previous version restored"));
            set(Phase::Failed { text: crate::trf!("graineo {v} ließ sich nicht starten. Die bisherige Version wurde wiederhergestellt; {v} wird nicht mehr automatisch angeboten.", v = v), page: true });
            return;
        }
        update::Outcome::Nothing => {}
    }
    if update::auto_check_due() {
        spawn_check(ctx, false);
    }
}

/// Where a replaced app goes: the trash (never deleted for good); test runs keep it in their data folder.
fn dispose(p: &std::path::Path) -> Result<(), String> {
    if let Some(d) = crate::env::var_os("GRAINEO_DATA_DIR") {
        let dir = std::path::PathBuf::from(d).join("updates/trash");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        return std::fs::rename(p, dir.join(format!("{n}-{}", p.file_name().unwrap_or_default().to_string_lossy()))).map_err(|e| e.to_string());
    }
    trash::delete(p).map_err(|e| e.to_string())
}

/// After an installation: once this version has run for a moment, confirm its start (the relaunch
/// helper stops watching, the previous app goes to the trash).
fn confirm(ctx: &egui::Context) {
    if !CONFIRM.load(Ordering::Relaxed) {
        return;
    }
    if ctx.input(|i| i.time) < update::CONFIRM_AFTER {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    CONFIRM.store(false, Ordering::SeqCst);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if let Some(v) = update::confirm_start(&update::current_version(), &dispose) {
            crate::log(&format!("update: {v} confirmed, previous app to the trash"));
            STATE.lock().unwrap_or_else(|e| e.into_inner()).toast = Some(crate::trf!("graineo {v} ist installiert.", v = v));
            ctx.request_repaint();
        }
    });
}

/// "Check for updates now" (settings, menu).
pub fn check_now(ctx: &egui::Context) {
    STARTED.store(true, Ordering::SeqCst);
    let busy = matches!(STATE.lock().unwrap_or_else(|e| e.into_inner()).phase, Phase::Checking | Phase::Downloading { .. } | Phase::Installing { .. } | Phase::Restarting);
    if !busy {
        spawn_check(ctx, true);
    }
}

pub fn checking() -> bool {
    matches!(STATE.lock().unwrap_or_else(|e| e.into_inner()).phase, Phase::Checking)
}

/// State line for the settings.
pub fn line() -> String {
    let st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match &st.phase {
        Phase::Checking => crate::tr("Suche nach Updates …").to_string(),
        Phase::Available { m, .. } | Phase::Downloading { m, .. } | Phase::Installing { m } => crate::trf!("graineo {v} ist verfügbar", v = m.version),
        _ if !st.line.is_empty() => st.line.clone(),
        _ => match update::last_check() {
            Some(t) => crate::trf!("Installiert: {a} · zuletzt geprüft {b}", a = update::current_version(), b = when(t)),
            None => crate::trf!("Installiert: {a}", a = update::current_version()),
        },
    }
}

fn when(t: i64) -> String {
    let ago = (library::now() - t).max(0);
    if ago < 3600 {
        crate::tr("vor weniger als einer Stunde").to_string()
    } else if ago < 86_400 {
        crate::trf!("vor {n} Std.", n = ago / 3600)
    } else {
        crate::trf!("vor {n} Tagen", n = ago / 86_400)
    }
}

fn spawn_check(ctx: &egui::Context, manual: bool) {
    set(Phase::Checking);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = update::check();
        let now = ctx.input(|i| i.time);
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        match r {
            Ok(Check::Available(m)) => {
                crate::log(&format!("update: {} available", m.version));
                let skipped = update::skipped_version().as_deref() == Some(m.version.as_str());
                if skipped && !manual {
                    st.phase = Phase::Idle;
                } else {
                    st.hidden = false;
                    st.line.clear();
                    st.phase = Phase::Available { m, since: now };
                }
            }
            Ok(Check::UpToDate { current, latest }) => {
                crate::log(&format!("update: up to date ({current}, latest {latest})"));
                st.line = crate::trf!("graineo {v} ist aktuell.", v = current);
                if manual {
                    st.toast = Some(st.line.clone());
                }
                st.phase = Phase::Idle;
            }
            Err(e) => {
                // automatic checks fail silently (log only)
                crate::log(&format!("update check failed: {e}"));
                st.line = crate::trf!("Update-Prüfung fehlgeschlagen: {e}", e = e);
                if manual {
                    st.toast = Some(st.line.clone());
                }
                st.phase = Phase::Idle;
            }
        }
        drop(st);
        ctx.request_repaint();
    });
}

fn install(ctx: &egui::Context, m: Manifest) {
    CANCEL.store(false, Ordering::SeqCst);
    set(Phase::Downloading { m: m.clone(), done: 0, total: 0 });
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = run_install(&ctx, &m);
        match result {
            Ok(done) => {
                crate::log(&format!("update: installed {}, restarting {}; previous kept at {:?}; warnings {:?}", m.version, done.relaunch.display(), done.previous, done.warnings));
                match update::relaunch(&done) {
                    Ok(()) => set(Phase::Restarting),
                    Err(e) => set(Phase::Failed { text: crate::trf!("Installiert; bitte graineo neu starten ({e}).", e = e), page: false }),
                }
            }
            Err(InstallError::NotWritable(p)) => {
                crate::log(&format!("update: not writable {p}"));
                set(Phase::Failed { text: crate::trf!("Der Ordner von graineo ist nicht beschreibbar ({p}). Lade die neue Version von der Release-Seite und ersetze die App von Hand.", p = p), page: true });
            }
            Err(InstallError::NotInstalled) => {
                set(Phase::Failed { text: crate::tr("Diese graineo-Fassung ist nicht als App installiert. Lade die neue Version von der Release-Seite.").to_string(), page: true });
            }
            Err(InstallError::NotMoved) => {
                crate::log("update: app runs from a disk image or a translocated copy");
                set(Phase::Failed { text: crate::tr("graineo läuft noch aus dem Disk-Image oder aus dem Download-Ordner. Beende graineo, zieh die App in den Ordner „Programme“ und starte sie von dort; dann klappt das Update.").to_string(), page: false });
            }
            Err(InstallError::NoSpace { need, free }) => {
                crate::log(&format!("update: not enough space ({need} needed, {free} free)"));
                let text = if need > 0 {
                    crate::trf!("Nicht genug freier Speicher: nötig sind {a}, frei sind {b}. Die bisherige Version bleibt.", a = crate::dialog::size(need), b = crate::dialog::size(free))
                } else {
                    crate::tr("Nicht genug freier Speicher. Die bisherige Version bleibt.").to_string()
                };
                set(Phase::Failed { text, page: false });
            }
            Err(InstallError::Blocked) => {
                crate::log("update: macOS refused to change the app (App Management)");
                set(Phase::Failed { text: crate::tr("macOS hat das Ersetzen der App verhindert. Erlaube graineo unter Systemeinstellungen → Datenschutz & Sicherheit → App-Verwaltung und versuche es erneut, oder ersetze die App von Hand.").to_string(), page: true });
            }
            Err(InstallError::Busy) => {
                set(Phase::Failed { text: crate::tr("Ein anderes graineo-Fenster installiert gerade ein Update.").to_string(), page: false });
            }
            Err(InstallError::Failed(e)) => {
                crate::log(&format!("update: failed: {e}"));
                let text = if e == "cancelled" { crate::tr("Update abgebrochen.").to_string() } else { crate::trf!("Update fehlgeschlagen: {e}. Die bisherige Version bleibt.", e = friendly(&e)) };
                set(Phase::Failed { text, page: e != "cancelled" });
            }
        }
        ctx.request_repaint();
    });
}

/// Download, verify, replace. Returns what to start after quitting.
fn run_install(ctx: &egui::Context, m: &Manifest) -> Result<update::Installed, InstallError> {
    let key = update::asset_key().ok_or(InstallError::NotInstalled)?;
    // one installation at a time, also across several running graineo processes
    let _lock = update::lock()?;
    let size = m.asset(key).map_or(0, |a| a.size);
    // where the new version goes, before anything is downloaded
    #[cfg(target_os = "macos")]
    let bundle = update::running_bundle().ok_or(InstallError::NotInstalled)?;
    #[cfg(target_os = "macos")]
    {
        if let Some(e) = update::location_problem(&bundle) {
            return Err(e);
        }
        let parent = bundle.parent().ok_or(InstallError::NotInstalled)?;
        update::writable(parent)?;
        update::check_space(parent, size.saturating_mul(update::UNPACKED_FACTOR))?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(std::path::Path::to_path_buf)).ok_or(InstallError::NotInstalled)?;
        update::writable(&dir)?;
        update::check_space(&dir, size.saturating_mul(update::UNPACKED_FACTOR))?;
    }
    let downloads = update::downloads_dir();
    let _ = std::fs::create_dir_all(&downloads);
    update::check_space(&downloads, size)?;
    let progress = |done: u64, total: u64| {
        set(Phase::Downloading { m: m.clone(), done, total });
        ctx.request_repaint();
    };
    let zip = update::download(m, key, &progress, &CANCEL).map_err(InstallError::Failed)?;
    set(Phase::Installing { m: m.clone() });
    ctx.request_repaint();
    #[cfg(target_os = "macos")]
    {
        let team = update::required_team(&bundle);
        crate::log(&format!("update: required signing team: {}", team.as_deref().unwrap_or("none (ad hoc)")));
        update::install_macos(&zip, &bundle, &m.version, &update::current_version(), team.as_deref()).inspect_err(|_| {
            let _ = std::fs::remove_file(&zip); // own download
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let exe = std::env::current_exe().map_err(|_| InstallError::NotInstalled)?;
        update::install_windows(&zip, &exe).inspect_err(|_| {
            let _ = std::fs::remove_file(&zip);
        })
    }
}

/// The usual reasons in words (the technical text stays in the log).
fn friendly(e: &str) -> String {
    if e.starts_with("download failed") {
        crate::tr("Download abgebrochen oder unvollständig – prüfe die Internetverbindung und versuche es erneut").to_string()
    } else if e.contains("checksum") || e.starts_with("size ") || e.contains("larger than announced") {
        crate::tr("das geladene Paket ist beschädigt oder verändert und wurde verworfen").to_string()
    } else if e.starts_with("code signature") {
        crate::tr("die Signatur des Pakets passt nicht, es wurde verworfen").to_string()
    } else {
        e.to_string()
    }
}

/// Height of the visible card (0 = none), for placing the toast above it.
pub fn card_height() -> f32 {
    f32::from_bits(HEIGHT.load(Ordering::Relaxed))
}

/// Every frame: start the check once, draw the card. Returns a message for the toast.
pub fn frame(ctx: &egui::Context, bottom: f32) -> Option<String> {
    start(ctx);
    confirm(ctx);
    let (phase, hidden, toast) = {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        (st.phase.clone(), st.hidden, st.toast.take())
    };
    let visible = !hidden && !matches!(phase, Phase::Idle | Phase::Checking) && !crate::prefs::opened() && ctx.memory(|m| m.top_modal_layer()).is_none();
    if matches!(phase, Phase::Restarting) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if !visible {
        HEIGHT.store(0, Ordering::Relaxed);
        return toast;
    }
    // Test hook: install without a click
    if let Phase::Available { m, since } = &phase {
        if let (Ok(v), true) = (crate::env::var("GRAINEO_UPDATE_AUTOINSTALL"), update::source_url().1) {
            let delay = v.trim().parse::<f64>().unwrap_or(0.0);
            if ctx.input(|i| i.time) - since >= delay {
                crate::log("update: test hook GRAINEO_UPDATE_AUTOINSTALL");
                install(ctx, m.clone());
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
    }
    let german = crate::menu::i18n::language() == crate::menu::i18n::Language::De;
    let r = egui::Area::new(egui::Id::unique("update-card"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -bottom))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(crate::lr::ui_rgb(0x26, 0x26, 0x26))
                .stroke(egui::Stroke::new(1.0, crate::lr::gray(0x44)))
                .corner_radius(0.0)
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(WIDTH);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    card(ui, &phase, german);
                })
        })
        .response;
    HEIGHT.store((r.rect.height() + 8.0).to_bits(), Ordering::Relaxed);
    if matches!(phase, Phase::Downloading { .. } | Phase::Installing { .. }) {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
    toast
}

fn title(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(t).font(lr::semibold(13.0)).color(crate::lr::gray(0xee))).wrap());
}

fn small(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(t).small().color(lr::TEXT_DIM())).wrap());
}

fn card(ui: &mut egui::Ui, phase: &Phase, german: bool) {
    match phase {
        Phase::Available { m, .. } => {
            title(ui, &crate::trf!("graineo {v} ist verfügbar", v = m.version));
            // a system without an update package (Linux): only a pointer to the release page
            let manual = update::asset_key().is_none();
            if manual {
                small(ui, &format!("{} · {}", crate::trf!("Installiert: {a}", a = update::current_version()), crate::tr("Keine automatische Installation auf diesem System")));
            } else {
                let size = update::asset_key().and_then(|k| m.asset(k)).map(|a| a.size).unwrap_or(0);
                small(ui, &format!("{} · {}", crate::trf!("Installiert: {a}", a = update::current_version()), crate::dialog::size(size)));
            }
            let notes = m.notes_for(german).trim();
            if !notes.is_empty() {
                egui::CollapsingHeader::new(egui::RichText::new(crate::tr("Neuigkeiten")).small().color(lr::TEXT_DIM())).id_salt("update-notes").default_open(false).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| notes_view(ui, notes));
                });
            }
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if manual {
                    if lr::main_button_min(ui, crate::tr("Release-Seite öffnen"), true, 100.0).clicked() {
                        crate::system::open(update::RELEASES_PAGE);
                        STATE.lock().unwrap_or_else(|e| e.into_inner()).hidden = true;
                    }
                } else if lr::main_button_min(ui, crate::tr("Installieren"), true, 100.0).clicked() {
                    install(ui.ctx(), m.clone());
                }
                if ui.add(egui::Button::new(crate::tr("Später")).min_size(egui::vec2(70.0, 24.0)).corner_radius(0.0)).clicked() {
                    STATE.lock().unwrap_or_else(|e| e.into_inner()).hidden = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(egui::RichText::new(crate::tr("Diese Version überspringen")).small().color(lr::TEXT_DIM())).frame(false)).clicked() {
                        update::skip_version(&m.version);
                        set(Phase::Idle);
                    }
                });
            });
        }
        Phase::Downloading { m, done, total } => {
            title(ui, &crate::trf!("graineo {v} wird geladen …", v = m.version));
            let fraction = (*total > 0).then(|| *done as f32 / *total as f32);
            crate::dialog::progress(ui, fraction, &format!("{} / {}", crate::dialog::size(*done), crate::dialog::size(*total)));
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(crate::tr("Abbrechen")).min_size(egui::vec2(90.0, 24.0)).corner_radius(0.0)).clicked() {
                    CANCEL.store(true, Ordering::SeqCst);
                }
            });
        }
        Phase::Installing { m } => {
            title(ui, &crate::trf!("graineo {v} wird installiert …", v = m.version));
            crate::dialog::progress(ui, None, crate::tr("Paket prüfen und App ersetzen"));
        }
        Phase::Restarting => {
            title(ui, crate::tr("graineo startet neu …"));
        }
        Phase::Failed { text, page } => {
            title(ui, crate::tr("Update nicht installiert"));
            ui.add(egui::Label::new(egui::RichText::new(text).color(lr::TEXT())).wrap());
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if *page && lr::main_button_min(ui, crate::tr("Release-Seite öffnen"), true, 100.0).clicked() {
                    crate::system::open(update::RELEASES_PAGE);
                    set(Phase::Idle);
                }
                if ui.add(egui::Button::new(crate::tr("Schließen")).min_size(egui::vec2(70.0, 24.0)).corner_radius(0.0)).clicked() {
                    set(Phase::Idle);
                }
            });
        }
        Phase::Idle | Phase::Checking => {}
    }
}

/// Release notes (Markdown) as plain text: headings bold, list items with a bullet.
fn notes_view(ui: &mut egui::Ui, notes: &str) {
    for line in notes.lines() {
        let l = line.trim_end().replace("**", "").replace('`', "");
        let t = l.trim_start();
        if t.is_empty() {
            ui.add_space(2.0);
        } else if let Some(h) = t.strip_prefix('#') {
            ui.label(egui::RichText::new(h.trim_start_matches('#').trim()).font(lr::semibold(lr::UI_SIZE)).color(lr::TEXT()));
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            ui.add(egui::Label::new(egui::RichText::new(format!("•  {item}")).small().color(lr::TEXT())).wrap());
        } else {
            ui.add(egui::Label::new(egui::RichText::new(t).small().color(lr::TEXT())).wrap());
        }
    }
}
