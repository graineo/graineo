//! Learned camera looks in the look panel (`rawdev::learn`, `raw::learn_look`): the section "Gelernter Look" of a RAW
//! (switch, strength, reset in two steps) and the button that opens the window "Kamera-Looks"
//! (`camera_looks_ui.rs`), plus the display names of style keys and learned looks.

use eframe::egui;
use rawdev::learn;

use crate::lr;

/// Display name of a style key: the style's name, fine settings in brackets (`provia_h+1_s+1` → "Provia (H +1, S +1)").
pub fn style_label(style: &str) -> String {
    let mut parts = style.split('_');
    let base = crate::raw_app::look_name(parts.next().unwrap_or(style));
    let fine: Vec<String> = parts
        .filter_map(|p| {
            let (k, v) = p.split_at(1);
            let name = match k {
                "c" => crate::tr("Farbe"),
                "h" => crate::tr("Lichter"),
                "s" => crate::tr("Schatten"),
                _ => return None,
            };
            Some(format!("{name} {v}"))
        })
        .collect();
    if fine.is_empty() {
        base
    } else {
        format!("{base} ({})", fine.join(", "))
    }
}

/// "GR III Standard (gelernt)" – or "(Startwert von …)" for a look taken over from a related camera, "(geladen)" for
/// a shared look loaded from a file.
pub fn learned_label(key: &str, camera_name: &str, meta: Option<&serde_json::Value>) -> String {
    let style = key.split_once('/').map_or(key, |(_, s)| s);
    let name = format!("{camera_name} {}", style_label(style));
    match meta.and_then(|m| m["derived_from"].as_str()) {
        Some(from) => crate::trf!("{a} (Startwert von {b})", a = name, b = from.split('/').next().unwrap_or(from).to_uppercase()),
        None if meta.and_then(|m| m["imported"].as_bool()) == Some(true) => crate::trf!("{a} (geladen)", a = name),
        None => crate::trf!("{a} (gelernt)", a = name),
    }
}

/// Did the learned looks change since the last frame? (Then the open photo and the look thumbnails are rendered again.)
pub fn generation_changed(ctx: &egui::Context) -> bool {
    let g = learn::generation();
    ctx.data_mut(|d| {
        let id = egui::Id::unique("learned-look-generation");
        let old = d.get_temp::<u64>(id);
        d.insert_temp(id, g);
        old.is_some_and(|o| o != g)
    })
}

/// Section in the look panel for a RAW: the learned look of its camera and style (switch, strength, reset) and the
/// button that opens the window. Returns true when the window should open.
pub fn panel_section(ui: &mut egui::Ui, doc: Option<&rawdev::RawDoc>) -> bool {
    let Some(doc) = doc.filter(|d| !d.finished) else { return false };
    let camera_name = doc.picture_style.as_ref().map(|b| b.model.trim().to_string()).unwrap_or_default();
    if let Some(key) = &doc.learn_key {
        if let Some((e, meta)) = learn::entries().iter().find(|(e, _)| &e.key == key).cloned() {
            lr::category(ui, crate::tr("Gelernter Look"));
            let label = learned_label(key, &camera_name, meta.as_ref());
            let mut on = e.enabled;
            let r = ui.checkbox(&mut on, label);
            lr::hover_tip(ui, &r, crate::tr("Als Standard dieser Kamera und dieses Bildstils verwenden (aus: eingebaute Entwicklung)"));
            let mut v = (e.amount * 100.0).round() as f64;
            let moved = ui.add_enabled_ui(on, |ui| lr::slider_hint(ui, crate::tr("Stärke"), &mut v, 0.0..=100.0, 100.0, 1.0, crate::tr("Stärke des gelernten Looks"))).inner;
            if r.changed() || moved {
                if let Err(err) = learn::set(key, on, (v / 100.0) as f32) {
                    crate::log(&format!("learned look {key}: {err}"));
                }
            }
            // reset in two steps (the file is kept in `replaced/`, the built-in development takes over)
            let id = egui::Id::unique(("learned-reset", key.as_str()));
            let armed = ui.data(|d| d.get_temp::<f64>(id)).is_some_and(|t| ui.input(|i| i.time) - t < 4.0);
            let b = ui.button(if armed { crate::tr("Wirklich zurücksetzen?") } else { crate::tr("Zurücksetzen") });
            lr::hover_tip(ui, &b, crate::tr("Gelernten Look entfernen: wieder die eingebaute Entwicklung dieser Kamera"));
            if b.clicked() {
                if armed {
                    ui.data_mut(|d| d.remove::<f64>(id));
                    if let Err(err) = learn::reset(key) {
                        crate::log(&format!("learned look {key}: {err}"));
                    }
                } else {
                    let t = ui.input(|i| i.time);
                    ui.data_mut(|d| d.insert_temp(id, t));
                }
            }
            if armed {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
    }
    ui.add_space(6.0);
    let b = ui.button(crate::tr("Kamera-Looks …"));
    lr::hover_tip(ui, &b, crate::tr("Looks der Kameras der Bibliothek: Übersicht, Vergleich mit dem Kamera-JPEG, Lernen aus RAW und Kamera-JPEG der eigenen Fotos, Fassungen und Teilen"));
    b.clicked()
}
