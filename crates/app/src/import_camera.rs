//! Source "Kamera": cameras connected by USB that do not appear as a drive (PTP/MTP), in the left column of the import
//! window below the cards. The import logic is the same as for a card (`library::import_source`); this module finds the
//! cameras and serves their files.
//!
//! - **macOS**: ImageCaptureCore. The device browser runs only while the import window exists. A camera is opened only
//!   when it is chosen (or when it is the only source): session, catalog of its files (read step by step by the
//!   camera, the column shows how far), thumbnails of the visible cells, partial checksums, downloads. Cameras that
//!   mount as a drive are left out (they appear as a card).
//! - A camera that goes away (cable, switched off, asleep) keeps its row ("getrennt"); a running import waits for it and
//!   continues when it is back. A locked phone or a camera that asks for permission shows what to do.
//! - **Other systems**: no cameras (later perhaps Windows Portable Devices).
//! - **Not switched on yet**: cameras are only looked for with
//!   `GRAINEO_CAMERA=1`, also in the installed app; without it the window shows no camera and no camera texts.
//! - Tests and screenshots (only with `GRAINEO_DATA_DIR` and `GRAINEO_CAMERA=1`): `GRAINEO_FAKE_CAMERA=<folder>` shows a folder as camera
//!   (`GRAINEO_FAKE_CAMERA_STATE=reading|restricted|disconnected` for the states). Real cameras are looked for in test runs only
//!   with `GRAINEO_CAMERA=1`; `GRAINEO_NO_CAMERA=1` switches them off everywhere; `GRAINEO_CAMERA_LOG=1` logs every
//!   request to stderr (`library::camera_mac`).

use super::*;
use library::import_source::{self as isrc, CamState, Row, Source};

/// The camera import is switched on (`GRAINEO_CAMERA=1`, not `GRAINEO_NO_CAMERA`). Prepared, not released yet.
pub(super) fn enabled() -> bool {
    crate::env::var_os("GRAINEO_CAMERA").is_some() && crate::env::var_os("GRAINEO_NO_CAMERA").is_none()
}

/// The cameras of the open import window.
pub(super) struct Cameras {
    fake: Option<(Arc<isrc::FolderSource>, CamState)>,
    #[cfg(target_os = "macos")]
    mac: Option<library::camera_mac::Browser>,
}

impl Cameras {
    pub fn new(ctx: &egui::Context) -> Cameras {
        let test = crate::env::var_os("GRAINEO_DATA_DIR").is_some();
        let fake = crate::env::var_os("GRAINEO_FAKE_CAMERA").filter(|_| test && enabled()).map(|p| {
            let src = Arc::new(isrc::FolderSource::new(Path::new(&p), "ersatz-kamera", "Ersatz-Kamera"));
            isrc::register(src.clone());
            let n = src.list().map(|l| l.len()).unwrap_or(0);
            let state = match crate::env::var("GRAINEO_FAKE_CAMERA_STATE").as_deref() {
                Ok("reading") => CamState::Catalog(40),
                Ok("restricted") => CamState::Restricted,
                Ok("disconnected") => CamState::Gone,
                _ => CamState::Ready(n),
            };
            if state != CamState::Ready(n) {
                src.plug(false);
            }
            (src, state)
        });
        let real = enabled();
        let _ = (ctx, real);
        Cameras {
            fake,
            #[cfg(target_os = "macos")]
            mac: if real {
                let ctx = ctx.clone();
                library::camera_mac::Browser::start(Box::new(move || ctx.request_repaint()))
            } else {
                None
            },
        }
    }

    /// Cameras to show (in the order they were found).
    pub fn rows(&self) -> Vec<Row> {
        #[allow(unused_mut)]
        let mut v: Vec<Row> = self.fake.iter().map(|(s, st)| Row { id: s.id(), name: s.name(), state: st.clone() }).collect();
        #[cfg(target_os = "macos")]
        if let Some(m) = &self.mac {
            v.extend(m.rows());
        }
        v
    }

    /// Open the camera `id` (also one not connected yet: it is opened as soon as it appears) and return it as source.
    pub fn open(&self, id: &str) -> Option<Arc<dyn Source>> {
        if let Some((s, _)) = self.fake.as_ref().filter(|(s, _)| isrc::device_key(&s.id()) == isrc::device_key(id)) {
            return Some(s.clone());
        }
        #[cfg(target_os = "macos")]
        if let Some(m) = &self.mac {
            return Some(m.open(id));
        }
        None
    }

    /// Read the camera `id` again: close and open its session (its list is read anew).
    pub fn reload(&self, id: &str) {
        let _ = id;
        #[cfg(target_os = "macos")]
        if let Some(m) = &self.mac {
            m.reload(id);
        }
    }

    /// Something is under way (opening, catalog): draw again soon.
    pub fn busy(&self) -> bool {
        self.rows().iter().any(|r| matches!(r.state, CamState::Opening | CamState::Catalog(_)))
    }
}

impl Drop for Cameras {
    fn drop(&mut self) {
        if let Some((s, _)) = &self.fake {
            isrc::unregister(&s.id());
        }
    }
}

/// Is this source a camera?
pub(super) fn is_camera(source: Option<&Path>) -> bool {
    source.is_some_and(isrc::is_camera)
}

/// Short state line below a camera.
fn state_text(s: &CamState) -> (String, Color32) {
    let warn = Color32::from_rgb(0xe0, 0xb0, 0x40);
    match s {
        CamState::Found => (crate::tr("angeschlossen – zum Lesen anklicken").to_string(), lr::TEXT_DIM),
        CamState::Opening => (crate::tr("verbindet …").to_string(), lr::TEXT_DIM),
        CamState::Catalog(p) => (crate::trf!("liest die Dateiliste … {p} %", p = p), lr::TEXT_DIM),
        CamState::Ready(0) => (crate::tr("keine Dateien – Kamera einschalten?").to_string(), warn),
        CamState::Ready(n) => (crate::trf!("bereit · {n}", n = dialog::count(*n, crate::tr("Datei"), crate::tr("Dateien"))), lr::TEXT_DIM),
        CamState::Restricted => (crate::tr("gesperrt – entsperren und Zugriff erlauben").to_string(), warn),
        CamState::Gone => (crate::tr("getrennt – anschließen und einschalten").to_string(), warn),
        CamState::InfoOnly(_) => (crate::tr("nur Info-Ablage – Auswahl an der Kamera prüfen").to_string(), warn),
        CamState::Failed(e) => (e.clone(), Color32::from_rgb(0xe0, 0x5c, 0x50)),
    }
}

/// Longer explanation of a state (tooltip of the state line, the empty grid).
pub(super) fn state_hint(s: &CamState) -> String {
    let t = match s {
        CamState::Found => crate::tr("Die Kamera ist per USB verbunden. Ein Klick liest ihre Fotos; auf der Kamera wird nichts verändert."),
        CamState::Opening => crate::tr("graineo öffnet die Verbindung zur Kamera."),
        CamState::Catalog(_) => crate::tr("Die Kamera stellt die Liste ihrer Dateien zusammen. Bei vielen Fotos dauert das einen Moment."),
        CamState::Ready(0) => crate::tr("Die Kamera meldet keine Speicherkarte und keine Dateien. Meist ist sie ausgeschaltet oder im Ruhezustand und hängt nur noch zum Laden am USB, oder sie zeigt beim Anschließen eine Auswahl am Display. Kamera einschalten, Auswahl am Kamera-Display beim Anschließen prüfen (Datenübertragung wählen), Karte prüfen, dann „Neu einlesen“."),
        CamState::InfoOnly(what) => {
            return crate::trf!("Die Kamera gibt nur eine Info-Ablage frei („{a}“), nicht die Speicherkarte. Auswahl am Kamera-Display beim Anschließen prüfen (Datenübertragung wählen), prüfen, ob die Karte steckt, dann „Neu einlesen“.", a = what);
        }
        CamState::Ready(_) => crate::tr("Die Kamera ist bereit. Kopiert wird geprüft; auf der Kamera bleibt alles unverändert."),
        CamState::Restricted => crate::tr("Die Kamera oder das Telefon ist gesperrt oder fragt, ob dieser Mac zugreifen darf: entsperren und auf dem Gerät „Erlauben“ bzw. „Vertrauen“ wählen."),
        CamState::Gone => crate::tr("Die Kamera ist nicht mehr verbunden (Kabel, ausgeschaltet oder im Ruhezustand). Sobald sie wieder da ist, geht es weiter."),
        CamState::Failed(_) => crate::tr("Die Kamera hat die Verbindung abgelehnt. Kamera aus- und einschalten, Auswahl am Kamera-Display beim Anschließen prüfen bzw. im USB-Menü der Kamera „PTP“ oder „PC-Verbindung“ wählen und erneut versuchen."),
    };
    t.to_string()
}

/// The state of the camera chosen as source (`None`: no camera chosen or not known).
pub(super) fn chosen_state(imp: &Import) -> Option<CamState> {
    let (id, _) = isrc::parts(imp.source.as_deref()?)?;
    imp.cameras.rows().into_iter().find(|r| isrc::device_key(&r.id) == id).map(|r| r.state)
}

/// Entries for connected cameras in the source column (below the cards): name and a state line.
pub(super) fn sources(ui: &mut egui::Ui, imp: &mut Import, pick: &mut side::Picks) {
    let rows = imp.cameras.rows();
    for r in rows {
        let path = isrc::camera_path(&r.id, "");
        let chosen = imp.source.as_ref() == Some(&path);
        let resp = lr::list_item(ui, &format!("◉ {}", r.name), chosen);
        lr::hover_tip(ui, &resp, "Kamera per USB: Fotos werden geprüft kopiert, auf der Kamera bleibt alles");
        if resp.clicked() && !chosen {
            pick.camera = Some(r.id.clone());
        }
        let (t, c) = state_text(&r.state);
        let resp = ui.add(egui::Label::new(egui::RichText::new(format!("   {t}")).small().color(c)).truncate().sense(egui::Sense::hover()));
        lr::hover_tip(ui, &resp, &state_hint(&r.state));
        if chosen {
            let busy = matches!(r.state, CamState::Opening | CamState::Catalog(_)) || imp.scanning;
            let resp = ui.add_enabled(!busy, egui::Button::new(crate::tr("Neu einlesen")));
            lr::hover_tip(ui, &resp, "Verbindung zur Kamera neu öffnen und ihre Dateiliste noch einmal lesen");
            if resp.clicked() {
                pick.camera_reload = Some(r.id.clone());
            }
        }
    }
    if imp.cameras.busy() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
    }
}

/// A camera that appeared while no source is chosen and no card is inserted becomes the source by itself.
pub(super) fn only_camera(imp: &Import) -> Option<String> {
    if imp.source.is_some() || !imp.cards.is_empty() || imp.run.is_some() {
        return None;
    }
    let rows = imp.cameras.rows();
    match rows.as_slice() {
        [r] if r.state == CamState::Found || matches!(r.state, CamState::Ready(_)) => Some(r.id.clone()),
        _ => None,
    }
}
