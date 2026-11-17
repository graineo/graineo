//! Conditions of a single photo (damaged and partly rescued, Sigma's exact processing unavailable, original not
//! reachable …). They belong to the photo, not to an action of the user: they are kept per file and shown only
//! while that photo is shown – as a muted yellow line in the image info, a yellow mark in the toolbar (a click
//! opens a card with the details) and in the "State" row of the info panel. Never as a message.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eframe::egui::Color32;

/// Muted yellow of the conditions (line in the image info, mark in the toolbar).
pub const YELLOW: Color32 = Color32::from_rgb(0xd2, 0xae, 0x52);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// damaged file: partly rescued or only the camera's preview
    Damaged,
    /// could not be opened (other reason)
    OpenFailed,
    /// Sigma's exact processing is not available for this photo
    SigmaExact,
    /// original missing / location not connected
    Original,
    /// film look of the settings is not installed
    Look,
    /// face detection failed
    Faces,
    /// developed by the system RAW engine (graineo cannot decode the file)
    Engine,
    /// the engine chosen for the photo is not available (pinned Apple version missing, other system)
    EngineMissing,
    /// the stored development comes from a newer version and is not overwritten (`develop_guard.rs`)
    NewerDevelop,
}

/// What a stored condition was made from. The texts are created anew from it whenever it is read (`get`), so that a
/// switch of the display language reaches conditions remembered earlier (the strings in `Notice` are only a snapshot).
#[derive(Clone, Debug, PartialEq)]
enum Spec {
    None,
    Damaged(develop::damaged::Damage),
    Unreadable(String),
    OpenFailed(String),
    SigmaExact(String),
    Faces(String),
    NewerDevelop,
}

impl Spec {
    fn build(&self) -> Option<Notice> {
        Some(match self {
            Spec::None => return None,
            Spec::Damaged(d) => damaged(d),
            Spec::Unreadable(t) => unreadable(t),
            Spec::OpenFailed(t) => open_failed(t),
            Spec::SigmaExact(t) => sigma_exact(t),
            Spec::Faces(t) => faces(t),
            Spec::NewerDevelop => newer_develop(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    spec: Spec,
    pub kind: Kind,
    /// short line in the image info, e.g. "Beschädigt · 22 % gerettet"
    pub short: String,
    /// value of the "State" row in the info panel, e.g. "beschädigt – 22 % gerettet"
    pub state: String,
    /// title and explanation in the card
    pub title: String,
    pub detail: String,
    /// damaged Sigma file: share of the rows really read per plane (bottom, middle, top), shown as bars in the card
    pub layers: Option<[f64; 3]>,
}

impl Notice {
    pub fn new(kind: Kind, short: impl Into<String>, state: impl Into<String>, title: impl Into<String>, detail: impl Into<String>) -> Notice {
        Notice { spec: Spec::None, kind, short: short.into(), state: state.into(), title: title.into(), detail: detail.into(), layers: None }
    }
}

static STORE: Mutex<Option<HashMap<PathBuf, Vec<Notice>>>> = Mutex::new(None);

/// Remember a condition of the photo at `path` (replaces one of the same kind).
pub fn set(path: &Path, n: Notice) {
    let mut g = STORE.lock().unwrap();
    let v = g.get_or_insert_with(HashMap::new).entry(path.to_path_buf()).or_default();
    v.retain(|x| x.kind != n.kind);
    v.push(n);
}

/// The condition `kind` no longer applies to the photo.
pub fn clear(path: &Path, kind: Kind) {
    let mut g = STORE.lock().unwrap();
    if let Some(m) = g.as_mut() {
        if let Some(v) = m.get_mut(path) {
            v.retain(|x| x.kind != kind);
            if v.is_empty() {
                m.remove(path);
            }
        }
    }
}

/// Remembered conditions of the photo (without derived ones such as a missing look, see `App::photo_notices`).
pub fn get(path: &Path) -> Vec<Notice> {
    let mut v: Vec<Notice> = STORE.lock().unwrap().as_ref().and_then(|m| m.get(path).cloned()).unwrap_or_default();
    // texts in the language shown now
    for n in &mut v {
        if let Some(fresh) = n.spec.build() {
            *n = fresh;
        }
    }
    // a damaged file never gets Sigma's exact processing: one line is enough
    if v.iter().any(|n| n.kind == Kind::Damaged) {
        v.retain(|n| n.kind != Kind::SigmaExact);
    }
    v
}

/// Damaged Sigma file, partly rescued (see `develop::damaged`).
pub fn damaged(d: &develop::damaged::Damage) -> Notice {
    let pct = ((d.intact * 100.0).round() as i64).to_string();
    Notice {
        layers: Some(d.read),
        spec: Spec::Damaged(d.clone()),
        ..Notice::new(
            Kind::Damaged,
            crate::trf!("Beschädigt · {a} % gerettet", a = pct),
            crate::trf!("beschädigt – {a} % gerettet", a = pct),
            crate::tr("Datei beschädigt"),
            crate::jobs::damage_note(d),
        )
    }
}

/// Damaged Sigma file whose RAW data cannot be read: the camera's preview is shown.
pub fn unreadable(detail: &str) -> Notice {
    Notice { spec: Spec::Unreadable(detail.to_string()), ..Notice::new(Kind::Damaged, crate::tr("Beschädigt · Vorschau aus der Kamera"), crate::tr("beschädigt – nur Vorschau der Kamera"), crate::tr("Datei beschädigt"), crate::menu::i18n::tr_msg(detail)) }
}

pub fn open_failed(error: &str) -> Notice {
    if error.contains(raw::NIKON_HE) {
        // a known gap of the decoder, not a broken file: say what it is and what still works
        return Notice {
            spec: Spec::OpenFailed(error.to_string()),
            ..Notice::new(
                Kind::OpenFailed,
                crate::tr("NEF-Kompression nicht unterstützt"),
                crate::tr("NEF-Kompression nicht unterstützt"),
                crate::tr("NEF-Kompression nicht unterstützt"),
                crate::tr("Nikon „Hohe Effizienz“ (HE und HE★) kann graineo noch nicht entwickeln. Die Vorschau aus der Kamera bleibt sichtbar. Fotos mit „Verlustfrei komprimiert“, „Komprimiert“ oder „Unkomprimiert“ lassen sich entwickeln."),
            )
        };
    }
    Notice { spec: Spec::OpenFailed(error.to_string()), ..Notice::new(Kind::OpenFailed, crate::tr("Lässt sich nicht öffnen"), crate::tr("lässt sich nicht öffnen"), crate::tr("Foto lässt sich nicht öffnen"), crate::menu::i18n::tr_msg(error)) }
}

/// Short reason instead of "<format> – lässt sich hier nicht entwickeln" when the photo uses a known unsupported
/// variant of its format (Nikon High Efficiency NEF); `None` otherwise.
pub fn unsupported_text(path: &Path) -> Option<String> {
    let g = STORE.lock().unwrap();
    let v = g.as_ref()?.get(path)?;
    v.iter().any(|n| matches!(&n.spec, Spec::OpenFailed(t) if t.contains(raw::NIKON_HE))).then(|| crate::tr("NEF-Kompression „Hohe Effizienz“ – noch nicht unterstützt").to_string())
}

/// The stored development comes from a newer version of graineo: this version cannot read it, shows the initial values
/// and never overwrites it (`develop_guard.rs`).
pub fn newer_develop() -> Notice {
    Notice {
        spec: Spec::NewerDevelop,
        ..Notice::new(
            Kind::NewerDevelop,
            crate::tr("Bearbeitung aus neuerer Version"),
            crate::tr("Bearbeitung aus neuerer Version – geschützt"),
            crate::tr("Bearbeitung aus einer neueren Version"),
            crate::tr("Diese Bearbeitung stammt aus einer neueren Version von graineo und wird nicht überschrieben. Diese Version kann sie nicht lesen und zeigt die Startwerte; Änderungen an diesem Foto werden nicht gespeichert, Sterne und Markierungen schon. Mit der neueren Version lässt sich die Bearbeitung wieder öffnen."),
        )
    }
}

/// Developed by the system RAW engine of macOS because graineo cannot decode the file (Nikon High Efficiency).
pub fn engine_fallback(version: &str) -> Notice {
    Notice::new(
        Kind::Engine,
        crate::trf!("Apple RAW {a} · HE", a = version),
        crate::trf!("entwickelt mit Apple RAW {a} (graineo kann HE noch nicht)", a = version),
        crate::trf!("Entwickelt mit Apple RAW {a}", a = version),
        crate::trf!("graineo kann Nikon „Hohe Effizienz“ (HE und HE★) noch nicht dekodieren. macOS dekodiert dieses Foto (Apple RAW {a}); Farbe, Ton, Looks, Film und Masken rechnet graineo. Farbe und Schärfe können von graineos eigener Entwicklung abweichen. Die Version ist für dieses Foto festgehalten; wechseln im Sensor-Fach unter „Pipeline“.", a = version),
    )
}

/// The engine chosen for the photo is not available (title and explanation from the section "Pipeline").
pub fn engine_missing(title: &str, text: &str) -> Notice {
    Notice::new(Kind::EngineMissing, title.to_string(), crate::tr("andere Pipeline als festgehalten"), title.to_string(), text.to_string())
}

/// Sigma's exact processing is not available; `detail` says why.
pub fn sigma_exact(detail: &str) -> Notice {
    Notice { spec: Spec::SigmaExact(detail.to_string()), ..Notice::new(Kind::SigmaExact, crate::tr("Foveon-Verarbeitung nicht verfügbar"), crate::tr("Foveon-Verarbeitung nicht verfügbar"), crate::tr("Volle Foveon-Verarbeitung nicht verfügbar"), crate::menu::i18n::tr_msg(detail)) }
}

pub fn faces(error: &str) -> Notice {
    Notice { spec: Spec::Faces(error.to_string()), ..Notice::new(Kind::Faces, crate::tr("Gesichter nicht erkannt"), crate::tr("Gesichtererkennung fehlgeschlagen"), crate::tr("Gesichtererkennung fehlgeschlagen"), crate::menu::i18n::tr_msg(error)) }
}

/// Original missing or its location not connected (from the library state of the file).
pub fn original(state: u8) -> Notice {
    let short = crate::state_message(state);
    let detail = match state {
        1 => crate::tr("Die Datei ist nicht mehr an ihrem Ort – gelöscht oder außerhalb von graineo verschoben. Die Vorschau bleibt sichtbar."),
        _ => crate::tr("Der Speicherort ist gerade nicht verbunden. Sobald er wieder da ist, lässt sich das Foto bearbeiten; die Vorschau bleibt sichtbar."),
    };
    Notice::new(Kind::Original, short.clone(), short.to_lowercase(), short, detail)
}

/// The film look of the settings is not installed.
pub fn look(what: &str) -> Notice {
    Notice::new(Kind::Look, crate::tr("Look fehlt"), crate::tr("Look fehlt"), crate::tr("Look fehlt"), what)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_photo_and_kind() {
        let (a, b) = (Path::new("/x/a.X3F"), Path::new("/x/b.X3F"));
        set(a, sigma_exact("weil"));
        set(a, sigma_exact("anders"));
        assert_eq!(get(a).len(), 1);
        assert_eq!(get(a)[0].detail, "anders");
        assert!(get(b).is_empty());
        set(a, open_failed("kaputt"));
        clear(a, Kind::SigmaExact);
        assert_eq!(get(a).iter().map(|n| n.kind).collect::<Vec<_>>(), vec![Kind::OpenFailed]);
        clear(a, Kind::OpenFailed);
        assert!(get(a).is_empty());
    }

    /// Conditions are remembered per photo for a long time: their texts must follow a language switch
    /// (they are created from data when read), also the ones whose detail is a German message of the loader.
    #[test]
    fn stored_conditions_follow_the_language() {
        use crate::menu::i18n::{locale::TestLanguage, Language};
        let (p, q) = (Path::new("/x/sprache.X3F"), Path::new("/x/sprache2.X3F"));
        let d = develop::damaged::Damage { intact: 0.9, estimated: 0.1, missing: 0.0, read: [0.9, 1.0, 1.0], grey: Vec::new() };
        {
            let _de = TestLanguage::set(Language::De);
            set(p, damaged(&d));
            set(q, sigma_exact(crate::menu::i18n::de("Die Berechnung hat zu lange gedauert. graineo zeigt die eigene Entwicklung; beim nächsten Öffnen wird es erneut versucht.")));
            let n = get(p);
            assert_eq!(n[0].short, "Beschädigt · 90 % gerettet");
            assert_eq!(n[0].title, "Datei beschädigt");
            assert!(n[0].detail.starts_with("Datei beschädigt – 90 % gerettet, 10 % aus den übrigen Schichten geschätzt."), "{}", n[0].detail);
        }
        let _en = TestLanguage::set(Language::En);
        let n = get(p);
        assert_eq!(n[0].short, "Damaged · 90 % recovered");
        assert_eq!(n[0].state, "damaged – 90 % recovered");
        assert_eq!(n[0].title, "File damaged");
        assert!(n[0].detail.starts_with("File damaged – 90 % recovered, 10 % estimated from the other layers."), "{}", n[0].detail);
        assert!(!n[0].detail.contains("beschädigt") && !n[0].detail.contains("Teile der Datei"), "{}", n[0].detail);
        let m = get(q);
        assert!(!m[0].detail.contains("Berechnung"), "{}", m[0].detail);
        clear(p, Kind::Damaged);
        clear(q, Kind::SigmaExact);
    }

    #[test]
    fn damaged_short_line() {
        let n = damaged(&develop::damaged::Damage { intact: 0.22, estimated: 0.78, missing: 0.0, read: [0.22, 1.0, 1.0], grey: Vec::new() });
        assert!(n.short.contains("22 %"), "{}", n.short);
        assert!(n.detail.contains("78 %"), "{}", n.detail);
    }
}
