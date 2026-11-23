//! Browser access in the app (server in `crates/remote`): switched on and off by the setting, the app's own develop
//! and save paths for the browser, changes from the browser into the open views, and the settings page.
//!
//! Off by default. The server runs only while graineo runs on this computer; it reads the library and the previews
//! and saves ratings and develop settings through `library::store`, exactly like the app.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use develop::Settings;
use egui::Color32;
use remote::{Change, Role};

use crate::lr;

const KEY_ON: &str = "browserAccess";
const KEY_OUTSIDE: &str = "browserAccessOutside";
const KEY_DAYS: &str = "browserAccessDays";
const KEY_AUTO_OFF: &str = "browserAccessAutoOff";
const KEY_WRITE: &str = "browserAccessWrite";
const KEY_PORT: &str = "browserAccessPort";

struct State {
    server: Option<Arc<remote::Running>>,
    error: Option<String>,
    started: Option<Instant>,
    /// the setting was read (once per start; afterwards only the settings page changes it)
    known: bool,
    want: bool,
}

static STATE: Mutex<State> = Mutex::new(State { server: None, error: None, started: None, known: false, want: false });
static CHANGES: Mutex<Vec<Change>> = Mutex::new(Vec::new());
/// Photo open for editing in the app (file, copy): the browser must not change its development meanwhile.
static OPEN: Mutex<Option<(i64, Option<String>)>> = Mutex::new(None);
static CTX: OnceLock<egui::Context> = OnceLock::new();
static CHECKED_ON_START: AtomicBool = AtomicBool::new(false);

fn setting_bool(key: &str, default: bool) -> bool {
    library::settings::value(key).as_bool().unwrap_or(default)
}

fn setting_i64(key: &str, default: i64) -> i64 {
    library::settings::value(key).as_i64().unwrap_or(default)
}

fn port() -> u16 {
    // test runs: GRAINEO_SERVE=127.0.0.1:0 (own data folder only) – any free port, only this computer
    setting_i64(KEY_PORT, remote::DEFAULT_PORT as i64).clamp(1024, 65535) as u16
}

fn options() -> remote::Options {
    let mut o = remote::Options::standard(port());
    if crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
        if let Some(a) = crate::env::var("GRAINEO_SERVE").ok().and_then(|v| v.parse().ok()) {
            o.bind = a;
        }
    }
    o.allow_outside = setting_bool(KEY_OUTSIDE, false);
    o.device_days = setting_i64(KEY_DAYS, 30).clamp(0, 3650);
    o
}

fn start_now(st: &mut State) {
    if st.server.is_some() {
        return;
    }
    match remote::start(options(), Arc::new(AppHooks::default())) {
        Ok(r) => {
            crate::log(&format!("browser access on, port {}", r.port));
            st.server = Some(Arc::new(r));
            st.error = None;
            st.started = Some(Instant::now());
        }
        Err(e) => {
            crate::log(&format!("browser access could not start: {e}"));
            st.error = Some(e);
        }
    }
}

fn stop_now(st: &mut State) {
    if let Some(s) = st.server.take() {
        s.stop();
        crate::log("browser access off");
    }
    st.started = None;
}

fn set_on(on: bool) {
    library::settings::set(KEY_ON, serde_json::json!(on));
    let mut st = STATE.lock().unwrap();
    st.want = on;
    st.known = true;
    if on {
        start_now(&mut st);
    } else {
        stop_now(&mut st);
        st.error = None;
    }
}

fn server() -> Option<Arc<remote::Running>> {
    STATE.lock().unwrap().server.clone()
}

/// Is the browser access on and running?
pub fn running() -> bool {
    STATE.lock().unwrap().server.is_some()
}

/// For the activity display: what the browser access does right now.
pub fn activity() -> Option<(bool, String)> {
    let s = server()?;
    let busy = s.busy();
    Some(if busy > 0 { (true, crate::tr("Browser-Zugriff: rechnet für ein Gerät").to_string()) } else { (false, crate::tr("Browser-Zugriff an").to_string()) })
}

/// Every frame from `App::ui`: start/stop by setting (once), automatic switch-off, the photo open for editing, and
/// the changes made in the browser.
pub(crate) fn frame(app: &mut crate::App, ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
    if !CHECKED_ON_START.swap(true, Ordering::Relaxed) {
        let mut st = STATE.lock().unwrap();
        if !st.known {
            st.known = true;
            st.want = setting_bool(KEY_ON, false);
            if st.want {
                start_now(&mut st);
                // test aid (own data folder only): GRAINEO_REMOTE_PAIR=1 shows a pairing code at once and writes it to
                // `remote/test-pair-code.txt` of the test folder (screenshots, HTTP tests against the running app)
                if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var_os("GRAINEO_REMOTE_PAIR").is_some() {
                    if let Some(s) = st.server.as_ref() {
                        let (code, _) = s.start_pairing(Role::Write);
                        let _ = std::fs::write(s.dir.join("test-pair-code.txt"), &code);
                    }
                }
            }
        }
    }
    // automatic switch-off
    let auto_off = {
        let st = STATE.lock().unwrap();
        st.started.map(|t| (t, setting_i64(KEY_AUTO_OFF, 0)))
    };
    if let Some((t, hours)) = auto_off {
        if hours > 0 {
            let limit = Duration::from_secs(hours as u64 * 3600);
            if t.elapsed() >= limit {
                set_on(false);
            } else {
                ctx.request_repaint_after(limit - t.elapsed() + Duration::from_millis(200));
            }
        }
    }
    if !running() {
        return;
    }
    let open = app.current.as_ref().and_then(|c| app.entries.get(c.index)).filter(|e| e.file > 0).map(|e| (e.file, e.copy.clone()));
    {
        let mut o = OPEN.lock().unwrap();
        if *o != open {
            *o = open;
        }
    }
    let changes: Vec<Change> = std::mem::take(&mut *CHANGES.lock().unwrap());
    for c in changes {
        match c {
            Change::Rating { image, stars, flag, label } => {
                app.lib.show_rating(image, stars, flag, label);
                for e in app.entries.iter_mut().filter(|e| e.image == image) {
                    e.meta.rating = stars;
                    e.meta.flag = crate::sidecar::Flag::from_i8(flag);
                    e.meta.label = label;
                }
                if let Some((_, m)) = app.saved.get_mut(&image) {
                    m.rating = stars;
                    m.flag = crate::sidecar::Flag::from_i8(flag);
                    m.label = label;
                }
            }
            Change::Develop { image, file, copy, preview } => {
                let current = app.current.as_ref().map(|c| c.index);
                for (i, e) in app.entries.iter_mut().enumerate() {
                    // read again when shown next (the browser never changes the photo open for editing)
                    if e.image == image && Some(i) != current {
                        e.loaded = false;
                    }
                }
                if copy.is_none() {
                    if let Some(img) = preview {
                        app.lib.store_developed(ctx, file, (*img).clone(), true, false);
                    }
                }
                crate::browse::forget_developed(image);
            }
            Change::Devices => {}
        }
    }
    if server().is_some_and(|s| s.busy() > 0) {
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

/// The app's develop and save paths for the browser.
#[derive(Default)]
struct AppHooks {
    /// the last opened RAW (sliders of one photo follow each other quickly)
    doc: Mutex<Option<(PathBuf, Arc<rawdev::RawDoc>)>>,
}

impl AppHooks {
    /// The open document of `path` for the settings `s`: opened again when their engine of the raw stage (graineo,
    /// Apple and its version, `RawAdjust::engine`) is not the one it was opened for, as in the app.
    fn doc(&self, path: &Path, s: &Settings) -> Result<Arc<rawdev::RawDoc>, String> {
        if let Some((p, d)) = self.doc.lock().unwrap().as_ref() {
            if reuse_doc(p, d, path, s) {
                return Ok(d.clone());
            }
        }
        // (a merged photo whose result is not in the cache is merged first: the browser never gets its reference frame
        // as the picture of the merged photo)
        let mut doc = rawdev::RawDoc::open_for_output(path, s, &|| true)?.ok_or("abgebrochen")?;
        // oriented as on opening (the settings refer to it)
        rawdev::lightroom::apply_orientation(&mut doc);
        let d = Arc::new(doc);
        *self.doc.lock().unwrap() = Some((path.to_path_buf(), d.clone()));
        Ok(d)
    }
}

/// Can the open document `d` of `p` serve `path` with the settings `s`? Same file and the engine of the raw stage
/// the settings call for (otherwise a photo set to Apple developed with graineo in the browser, or the other way).
fn reuse_doc(p: &Path, d: &rawdev::RawDoc, path: &Path, s: &Settings) -> bool {
    p == path && d.engine_fits(s)
}

impl remote::Hooks for AppHooks {
    fn load_settings(&self, path: &Path, copy: Option<&str>) -> Result<Settings, String> {
        use crate::image_kind::ImageKind;
        let (s, _, stored) = crate::sidecar::load_entry(path, copy);
        match crate::image_kind::of(path) {
            ImageKind::Other => Err(crate::tr("lässt sich nicht entwickeln").into()),
            // unedited RAW: the start values as on opening (Lightroom handover, otherwise the defaults)
            ImageKind::Raw | ImageKind::Done if !stored => Ok(rawdev::lightroom::initial_settings(&*self.doc(path, &s)?).unwrap_or_default()),
            _ => Ok(s),
        }
    }

    fn white_balance(&self, path: &Path, s: &Settings) -> Option<(f32, f32)> {
        use crate::image_kind::ImageKind;
        match crate::image_kind::of(path) {
            ImageKind::Raw | ImageKind::Done => self.doc(path, s).ok().map(|d| d.temp_tint(s)),
            _ => None,
        }
    }

    fn render(&self, path: &Path, s: &Settings, level: u32) -> Result<image::RgbImage, String> {
        use crate::image_kind::ImageKind;
        match crate::image_kind::of(path) {
            ImageKind::Raw | ImageKind::Done => crate::large_preview::render_doc(&*self.doc(path, s)?, s, level),
            _ => crate::large_preview::render(path, s, level),
        }
    }

    fn save_settings(&self, path: &Path, file: i64, copy: Option<&str>, before: &Settings, after: &Settings) -> Result<(), String> {
        let m = library::store::meta(file, copy).unwrap_or_default();
        let meta = crate::sidecar::Meta { rating: m.0, flag: crate::sidecar::Flag::from_i8(m.1), label: m.2, ..Default::default() };
        // the history step first: it is written with the settings in the same transaction
        let fields = remote::develop_edit::changed_fields(before, after);
        let hint = crate::history::Hint { kind: library::develop_history::kind::SLIDER, label: serde_json::json!({ "k": "remote", "f": fields }) };
        let cmd = crate::history::Command::new(hint);
        library::store::command_step(file, copy, &crate::history::value(before), &crate::history::value(after), cmd.hint.kind, &cmd.hint.label, cmd.id, false);
        let result = if crate::image_kind::of(path) == crate::image_kind::ImageKind::Foveon && copy.is_none() {
            crate::sidecar::save(path, after, &meta)
        } else {
            crate::raw_app::save_settings(path, copy, after, &meta)
        };
        // a development equal to the start values is not stored: the history step goes alone
        library::store::send_history_of(file, copy);
        result
    }

    fn is_open(&self, file: i64, copy: Option<&str>) -> bool {
        OPEN.lock().unwrap().as_ref().is_some_and(|(f, c)| *f == file && c.as_deref() == copy)
    }

    fn changed(&self, change: Change) {
        CHANGES.lock().unwrap().push(change);
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }
}

// ───────────────────────────────────────────────────────── settings page

struct Page {
    /// pairing link as QR modules (for the code shown)
    qr: Option<(String, Vec<Vec<bool>>)>,
    log: Option<(Instant, Vec<String>)>,
}

static PAGE: Mutex<Page> = Mutex::new(Page { qr: None, log: None });

fn when(unix: i64) -> String {
    if unix <= 0 {
        return "–".into();
    }
    let days = (unix - library::now()) as f64 / 86_400.0;
    if days.abs() < 1.0 {
        let h = ((unix - library::now()) as f64 / 3600.0).round() as i64;
        if h >= 0 {
            crate::trf!("in {n} h", n = h)
        } else {
            crate::trf!("vor {n} h", n = -h)
        }
    } else if days >= 0.0 {
        crate::trf!("in {n} Tagen", n = days.round() as i64)
    } else {
        crate::trf!("vor {n} Tagen", n = (-days).round() as i64)
    }
}

fn draw_qr(ui: &mut egui::Ui, modules: &[Vec<bool>], size: f32) {
    let n = modules.len() as f32 + 8.0; // quiet zone of 4 modules
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, Color32::WHITE);
    let m = size / n;
    for (y, row) in modules.iter().enumerate() {
        for (x, &dark) in row.iter().enumerate() {
            if dark {
                let min = rect.min + egui::vec2((x as f32 + 4.0) * m, (y as f32 + 4.0) * m);
                p.rect_filled(egui::Rect::from_min_size(min, egui::vec2(m + 0.3, m + 0.3)), 0.0, Color32::BLACK);
            }
        }
    }
}

/// Settings → Browser.
pub fn settings_page(ui: &mut egui::Ui) {
    use crate::prefs::{group, row, separator, toggle};
    let (on, error) = {
        let st = STATE.lock().unwrap();
        (st.server.is_some() || (st.known && st.want) || (!st.known && setting_bool(KEY_ON, false)), st.error.clone())
    };
    group(ui, crate::tr("Browser-Zugriff"), |ui| {
        row(
            ui,
            "Browser-Zugriff erlauben",
            Some("Diese Bibliothek im Browser eines Tablets oder anderen Rechners in deinem Heimnetz ansehen, bewerten und einfach entwickeln, solange graineo hier läuft. Zugreifen kann nur ein Gerät, das du hier mit einem Code koppelst; die Verbindung ist verschlüsselt (HTTPS). Nichts geht an fremde Server."),
            |ui| {
                let mut v = on;
                if toggle(ui, &mut v) {
                    set_on(v);
                }
            },
        );
        if let Some(e) = &error {
            ui.label(egui::RichText::new(crate::trf!("Konnte nicht starten: {e}", e = e)).small().color(Color32::from_rgb(0xe0, 0x78, 0x6f)));
        }
        if let Some(s) = server() {
            separator(ui);
            row(ui, "Adresse im Browser", Some("Auf dem Tablet eine dieser Adressen öffnen (gleiches WLAN). Beim ersten Mal warnt der Browser vor dem Zertifikat: Fingerabdruck vergleichen oder das Zertifikat installieren (siehe unten)."), |ui| {
                ui.vertical(|ui| {
                    for u in s.urls.iter().take(3) {
                        ui.add(egui::Label::new(egui::RichText::new(u).color(lr::TEXT)).selectable(true));
                    }
                });
            });
            separator(ui);
            row(ui, "Fingerabdruck des Zertifikats", None, |_| {});
            ui.add(egui::Label::new(egui::RichText::new(format!("SHA-256 {}", s.fingerprint)).small().monospace().color(lr::TEXT_DIM)).wrap().selectable(true));
        }
    });

    let Some(s) = server() else { return };
    group(ui, crate::tr("Geräte"), |ui| {
        let mut write = setting_bool(KEY_WRITE, true);
        row(ui, "Neue Geräte dürfen ändern", Some("Bewerten und die einfachen Regler. Aus: nur ansehen."), |ui| {
            if toggle(ui, &mut write) {
                library::settings::set(KEY_WRITE, serde_json::json!(write));
            }
        });
        separator(ui);
        match s.pairing() {
            Some((code, expires)) => {
                let link = format!("{}/#pair={code}", s.urls.first().cloned().unwrap_or_default());
                let mut page = PAGE.lock().unwrap();
                if page.qr.as_ref().is_none_or(|(l, _)| *l != link) {
                    page.qr = remote::qr::modules(&link).map(|m| (link.clone(), m));
                }
                ui.horizontal(|ui| {
                    if let Some((_, m)) = &page.qr {
                        draw_qr(ui, m, 156.0);
                    }
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(crate::tr("Kopplungscode")).small().color(lr::TEXT_DIM));
                        ui.add(egui::Label::new(egui::RichText::new(&code).font(lr::semibold(26.0)).color(lr::TEXT)).selectable(true));
                        let left = (expires - library::now()).max(0);
                        ui.label(egui::RichText::new(crate::trf!("gültig noch {m}:{s}", m = left / 60, s = format!("{:02}", left % 60))).small().color(lr::TEXT_DIM));
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(egui::RichText::new(crate::tr("QR-Code mit der Kamera des Tablets scannen oder die Adresse öffnen und den Code eingeben. Der Code gilt einmal und fünf Minuten.")).small().color(lr::TEXT_DIM)).wrap());
                        if ui.button(crate::tr("Abbrechen")).clicked() {
                            s.cancel_pairing();
                        }
                    });
                });
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
            None => {
                row(ui, "Neues Gerät", None, |ui| {
                    if ui.button(crate::tr("Gerät koppeln …")).clicked() {
                        s.start_pairing(if write { Role::Write } else { Role::Read });
                    }
                });
            }
        }
        let devices = s.devices();
        if !devices.is_empty() {
            separator(ui);
        }
        for d in &devices {
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(&d.name).color(lr::TEXT));
                let role = if d.role == Role::Write { crate::tr("darf ändern") } else { crate::tr("nur ansehen") };
                ui.label(egui::RichText::new(format!("· {role} · {} {} · {} {}", crate::tr("zuletzt"), when(d.last_seen), crate::tr("gültig bis"), if d.expires == 0 { crate::tr("Widerruf").to_string() } else { when(d.expires) })).small().color(lr::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tr("Entfernen")).clicked() {
                        s.revoke(&d.id);
                    }
                });
            });
        }
        if devices.len() > 1 {
            row(ui, "Alle Geräte", None, |ui| {
                if ui.button(crate::tr("Alle entfernen")).clicked() {
                    s.revoke_all();
                }
            });
        }
    });

    group(ui, crate::tr("Sicherheit"), |ui| {
        let mut days = setting_i64(KEY_DAYS, 30);
        row(ui, "Gekoppelte Geräte gelten", Some("Danach ist eine neue Kopplung nötig. Gilt für neu gekoppelte Geräte."), |ui| {
            if lr::segmented(ui, &mut days, &[(1, "1 Tag"), (7, "7 Tage"), (30, "30 Tage"), (90, "90 Tage")]) {
                library::settings::set(KEY_DAYS, serde_json::json!(days));
                restart();
            }
        });
        separator(ui);
        let mut hours = setting_i64(KEY_AUTO_OFF, 0);
        row(ui, "Automatisch ausschalten", Some("Der Browser-Zugriff schaltet sich nach dieser Zeit selbst ab."), |ui| {
            if lr::segmented(ui, &mut hours, &[(0, "Nie"), (1, "1 h"), (4, "4 h"), (12, "12 h")]) {
                library::settings::set(KEY_AUTO_OFF, serde_json::json!(hours));
                if let Some(st) = STATE.lock().unwrap().started.as_mut() {
                    *st = Instant::now();
                }
            }
        });
        separator(ui);
        let mut outside = setting_bool(KEY_OUTSIDE, false);
        row(ui, "Auch von außerhalb des Heimnetzes", Some("Nur nötig für ein VPN mit öffentlichen Adressen. Unterwegs immer über ein VPN ins Heimnetz (z. B. das VPN des Routers oder NAS), nie über eine Portfreigabe im Router."), |ui| {
            if toggle(ui, &mut outside) {
                library::settings::set(KEY_OUTSIDE, serde_json::json!(outside));
                s.set_allow_outside(outside);
            }
        });
        separator(ui);
        row(ui, "Zertifikat", Some("Neu erzeugen, wenn du ihm nicht mehr traust. Geräte müssen danach dem neuen Zertifikat vertrauen."), |ui| {
            if ui.button(crate::tr("Neu erzeugen")).clicked() {
                let dir = s.dir.clone();
                let mut st = STATE.lock().unwrap();
                stop_now(&mut st);
                remote::tls::reset(&dir);
                start_now(&mut st);
            }
        });
        separator(ui);
        row(ui, "Zugriffsprotokoll", Some("Anmeldungen, Kopplungen, Änderungen und abgewiesene Anfragen mit Zeit, Adresse und Gerät, ohne Inhalte."), |ui| {
            if let Some(p) = s.log_path() {
                if ui.button(crate::tr("Im Finder zeigen")).clicked() {
                    crate::system::show(&p);
                }
            }
        });
        let lines = {
            let mut page = PAGE.lock().unwrap();
            if page.log.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(2)) {
                page.log = Some((Instant::now(), s.log_tail(5)));
            }
            page.log.as_ref().map(|(_, l)| l.clone()).unwrap_or_default()
        };
        for l in lines {
            ui.add(egui::Label::new(egui::RichText::new(l).small().monospace().color(lr::TEXT_DIM)).truncate());
        }
    });

    group(ui, crate::tr("Hinweise"), |ui| {
        for t in [
            "Zertifikat auf dem iPad: die Adresse öffnen, …/ca.crt laden, unter Einstellungen → Allgemein → VPN und Geräteverwaltung das Profil installieren und unter Info → Zertifikatsvertrauenseinstellungen einschalten. Fingerabdruck dabei vergleichen.",
            "Fotos auf dem NAS: Das NAS muss auf diesem Rechner eingehängt sein, damit Originale entwickelt werden können. Zum Ansehen genügen die Vorschauen und Offline-Kopien, die hier liegen.",
            "Ruhezustand: Schläft der Rechner oder ist graineo beendet, ist der Browser-Zugriff nicht erreichbar.",
        ] {
            ui.add(egui::Label::new(egui::RichText::new(crate::tr(t)).small().color(lr::TEXT_DIM)).wrap());
            ui.add_space(4.0);
        }
    });
}

/// Restart with new options (lifetime of new devices).
fn restart() {
    let mut st = STATE.lock().unwrap();
    if st.server.is_some() {
        stop_now(&mut st);
        start_now(&mut st);
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;

    /// The browser keeps the last document only while it has the engine of the photo's settings (`RawAdjust::engine`).
    #[test]
    fn the_browser_reopens_a_photo_whose_engine_changed() {
        let dir = std::env::temp_dir().join(format!("graineo-remote-engine-{}-{}", std::process::id(), library::now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.jpg");
        image::RgbImage::from_pixel(32, 24, image::Rgb([90, 90, 90])).save(&path).unwrap();
        let mut doc = rawdev::RawDoc::open(&path, &|| true).unwrap().unwrap();
        // as a RAW opened with the automatic engine (graineo)
        doc.finished = false;
        let auto = Settings::default();
        let mut apple = Settings::default();
        apple.raw.engine = Some("apple:8".into());
        assert!(reuse_doc(&path, &doc, &path, &auto));
        assert!(!reuse_doc(&path, &doc, &path, &apple), "Apple chosen: opened again");
        assert!(!reuse_doc(&path, &doc, &dir.join("b.jpg"), &auto), "another file");
        doc.engine_request = Some("apple:8".into());
        assert!(reuse_doc(&path, &doc, &path, &apple));
        assert!(!reuse_doc(&path, &doc, &path, &auto), "back to automatic: opened again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
