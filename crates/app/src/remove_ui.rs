//! Removing objects ("Objekte entfernen" in the "Retouch" tool): click on a spot, an inpainting model (LaMa, downloadable module)
//! invents a fill from the surroundings. The fill is stored in the library as a patch and blended in with Poisson
//! healing when rendering (`develop::spots::Removal`). Clearly an invention, not a photograph: the tooltip and the
//! help card say so. One computation at a time, by click, at user-initiated priority.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use develop::spots::Removal;
use develop::Settings;
use egui::{Color32, Context, Ui};

use crate::analysis_progress::{self as progress, Tool};
use crate::lr;

#[derive(Clone, Copy, Default)]
struct State {
    on: bool,
    /// radius as a fraction of the image width
    radius: f32,
}

fn key() -> egui::Id {
    egui::Id::unique("remove_tool")
}

fn state(ctx: &Context) -> State {
    let s = ctx.data_mut(|d| d.get_temp::<State>(key())).unwrap_or_default();
    State { radius: if s.radius > 0.0 { s.radius } else { 0.02 }, ..s }
}

/// Is the removal tool on (clicks in the image belong to it)?
pub fn active(ctx: &Context) -> bool {
    state(ctx).on
}

/// Switch the tool off (Esc).
pub fn finish(ctx: &Context) -> bool {
    let mut s = state(ctx);
    let was = s.on;
    s.on = false;
    ctx.data_mut(|d| d.insert_temp(key(), s));
    was
}

/// Path, centre of the removal, result (patch name, region, seconds).
static JOB: Mutex<Option<(PathBuf, [f32; 2], Option<Result<(String, [f32; 4], f32), String>>)>> = Mutex::new(None);
static MESSAGE: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

pub fn available() -> bool {
    library::module::status(&library::module::LAMA).0 == library::module::Status::Installed
}

fn busy() -> bool {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.2.is_none())
}

/// Section in the retouch tray. Returns the index of a removal to compute (pending, nothing running).
pub fn panel(ui: &mut Ui, removals: &mut Vec<Removal>, path: &Path) -> Option<usize> {
    let mut s = state(ui.ctx());
    // test hook GRAINEO_REMOVE_TEST=x,y,radius – one removal (screenshots, own data folder only)
    if let Some(v) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_REMOVE_TEST").ok()) {
        let v: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        let key = egui::Id::unique("remove_test_done");
        if v.len() == 3 && removals.is_empty() && available() && !ui.ctx().data_mut(|d| d.get_temp::<bool>(key).unwrap_or(false)) {
            ui.ctx().data_mut(|d| d.insert_temp(key, true));
            removals.push(Removal { center: [v[0], v[1]], radius: v[2], region: [0.0; 4], patch: String::new() });
            s.on = true;
        }
    }
    // the tool appears only with the model present (see library::module::WITHHELD for withheld modules);
    // photos that already carry removals keep the section, so they can be cleared
    if !available() && removals.is_empty() {
        return None;
    }
    // a finished computation of this photo
    let done = {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.0 == path && j.2.is_some()) { g.take() } else { None }
    };
    if let Some((_, center, Some(r))) = done {
        match r {
            Ok((id, region, seconds)) => {
                if let Some(x) = removals.iter_mut().find(|x| x.center == center && x.patch.is_empty()) {
                    x.patch = id;
                    x.region = region;
                }
                *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), crate::trf!("Stelle entfernt · {t} s", t = format!("{seconds:.1}"))));
            }
            Err(e) => {
                removals.retain(|x| !(x.center == center && x.patch.is_empty()));
                let text = if progress::is_cancel(&e) { crate::tr(progress::CANCELLED).to_string() } else { crate::trf!("Entfernen fehlgeschlagen: {e}", e = e) };
                *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), text));
            }
        }
    }
    {
        lr::category_with(ui, "Objekte entfernen", 20.0, |ui| crate::help::info(ui, "remove_objects", "Objekte entfernen", &crate::help::REMOVE_OBJECTS));
        ui.horizontal(|ui| {
            let ok = available();
            // while a spot is filled: spinner and "Ermittle …" in the same place (the tool can still be switched off)
            let running = progress::running(path, Tool::Remove);
            let r = progress::button(ui, "Stelle entfernen", running, ok, s.on);
            lr::hover_tip(ui, &r, if !ok { "Braucht das Modul „Objekte entfernen“ (Einstellungen → Module)" } else if running { "Wird gerade ermittelt – Fortschritt und Abbrechen in der Zeile darunter; weitere Stellen erst danach" } else { "Stelle anklicken: der Bereich wird mit erfundenem Inhalt aus der Umgebung gefüllt (rechnet auf dem Rechner)" });
            if r.clicked() {
                s.on = !s.on;
            }
            let r = ui.add_enabled(!removals.is_empty() && !busy(), egui::Button::new(crate::tr("Alle wiederherstellen")));
            lr::hover_tip(ui, &r, if busy() { "Erst möglich, wenn die laufende Berechnung fertig ist" } else { "Alle entfernten Stellen wiederherstellen" });
            if r.clicked() {
                removals.clear();
            }
        });
        let mut pct = s.radius * 100.0;
        if lr::slider_plain_f32(ui, "Größe (%)", &mut pct, 0.3..=10.0, 2.0, 0.1) {
            s.radius = (pct / 100.0).clamp(0.003, 0.1);
        }
        let line = if let Some((_, m)) = MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|m| m.0 == path) {
            m.clone()
        } else {
            crate::trf!("{n} entfernte Stellen", n = removals.len())
        };
        // while computing: step, seconds and "Abbrechen" in the same line
        progress::status_line(ui, path, &[Tool::Remove], Some(&line));
    }
    ui.ctx().data_mut(|d| d.insert_temp(key(), s));
    if busy() {
        return None;
    }
    removals.iter().position(|x| x.patch.is_empty())
}

/// Clicks in the image (tool on) and the circles of the removed spots.
pub fn overlay(ui: &Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, frame: &crate::Frame, full: [f32; 2], removals: &mut Vec<Removal>) {
    let s = state(ui.ctx());
    let to_screen = |p: [f32; 2]| {
        let u = frame.from_src([p[0] * full[0], p[1] * full[1]]);
        rect.min + egui::vec2(u[0], u[1]) * rect.size()
    };
    if s.on && !crate::retouch_ui::active(ui.ctx()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        if resp.clicked() && !busy() && removals.len() < 64 {
            if let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p)) {
                let u = (pos - rect.min) / rect.size();
                let q = frame.to_src([u.x, u.y]);
                let c = [(q[0] / full[0]).clamp(0.0, 1.0), (q[1] / full[1]).clamp(0.0, 1.0)];
                removals.push(Removal { center: c, radius: s.radius, region: [0.0; 4], patch: String::new() });
                *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    }
    if s.on || crate::retouch_ui::overlays_visible(ui.ctx()) {
        // points per unit of image width on screen
        let ppw = (to_screen([1.0, 0.5]) - to_screen([0.0, 0.5])).length();
        for x in removals.iter() {
            let color = if x.patch.is_empty() { Color32::from_rgb(230, 180, 70) } else { Color32::from_rgb(120, 200, 230) };
            painter.circle_stroke(to_screen(x.center), x.radius * ppw, egui::Stroke::new(1.0, color));
        }
    }
}

/// Compute the fill of removal `index` in the background.
pub fn start(ctx: &Context, path: &Path, settings: &Settings, index: usize) {
    let Some(r) = settings.removals.get(index).cloned() else { return };
    {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.2.is_none()) {
            return;
        }
        *g = Some((path.to_path_buf(), r.center, None));
    }
    let ticket = progress::begin(Tool::Remove, path, progress::DEVELOP);
    let (path, ctx) = (path.to_path_buf(), ctx.clone());
    let mut s = Settings { local: None, ..settings.clone() };
    s.removals.retain(|x| !x.patch.is_empty() && x.center != r.center);
    let (failed_path, center) = (path.clone(), r.center);
    let work = move || {
        library::platform::set_priority(library::platform::Priority::UserInitiated);
        let res = crate::subject_ui::guarded("remove object", || compute(&path, &s, &r, &ticket));
        if let Err(e) = &res {
            crate::log(&format!("remove object {}: {e}", path.display()));
        }
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.0 == path && j.1 == r.center) {
            j.2 = Some(res);
        }
        drop(ticket);
        ctx.request_repaint();
    };
    crate::subject_ui::spawn_job("remove-object", work, |e| {
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.0 == failed_path && j.1 == center) {
            j.2 = Some(Err(e));
        }
    });
}

fn compute(path: &Path, s: &Settings, r: &Removal, ticket: &progress::Ticket) -> Result<(String, [f32; 4], f32), String> {
    let started = std::time::Instant::now();
    let t = std::time::Instant::now();
    let (img, _) = crate::large_preview::render_mask_space(path, s)?;
    let (w, h) = (img.width as f32, img.height as f32);
    if img.width == 0 || img.height == 0 {
        return Err("image size".into());
    }
    let rgb = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).ok_or("image size")?;
    let (cx, cy, rad) = (r.center[0] * w, r.center[1] * h, (r.radius * w).max(2.0));
    // region: four radii around the centre (context for the model), inside the image
    let side = (rad * 4.0).max(96.0).min(w.min(h));
    // whole pixels, the square always inside the image (rounding at x.5 once gave one pixel too many)
    let s_px = (side.round() as u32).clamp(1, img.width.min(img.height) as u32);
    let x0 = ((cx - side / 2.0).round().max(0.0) as u32).min(img.width as u32 - s_px);
    let y0 = ((cy - side / 2.0).round().max(0.0) as u32).min(img.height as u32 - s_px);
    let region = image::imageops::crop_imm(&rgb, x0, y0, s_px, s_px).to_image();
    let mask: Vec<f32> = (0..s_px * s_px)
        .map(|i| {
            let (x, y) = ((i % s_px) as f32 + 0.5 + x0 as f32, (i / s_px) as f32 + 0.5 + y0 as f32);
            if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= rad * 1.15 { 1.0 } else { 0.0 }
        })
        .collect();
    let developed = t.elapsed().as_secs_f32();
    let t = std::time::Instant::now();
    let out = segment::inpaint::fill_steps(&library::module::folder(&library::module::LAMA), &region, &mask, &mut ticket.model_step(progress::FILL))?;
    crate::log(&format!("remove object {}: develop {developed:.2} s, fill {:.2} s", path.display(), t.elapsed().as_secs_f32()));
    let id = library::mask_raster::put_rgb(out.width(), out.height(), out.as_raw(), &serde_json::json!({"rule": "remove", "model": "lama", "file": path.file_name().map(|n| n.to_string_lossy().to_string()), "app": env!("CARGO_PKG_VERSION")}), path)?;
    Ok((id, [x0 as f32 / w, y0 as f32 / h, s_px as f32 / w, s_px as f32 / h], started.elapsed().as_secs_f32()))
}
