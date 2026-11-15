//! Test hook `GRAINEO_MATRIX=<folder for screenshots>`: checks for every image of the list
//! (start the app with a folder) that Edit shows the path of its image kind
//! ([`crate::image_kind`]): X3F → Sigma path (group "Noise & Sigma detail", profile "Sigma …"),
//! everything else → general path (group "Noise reduction", no Sigma group).
//!
//! Sequence: 1. slowly every image (→), 2. quickly back (← every 50 ms), 3. grid → two images
//! forward → single image. After every stop it waits until the active image is loaded and checks
//! the sections drawn in the last frame. Result: log lines "matrix: …".

use std::sync::Mutex;

use eframe::egui;

use crate::image_kind::{self, ImageKind};
use crate::{lr, App};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Start,
    Slow,
    Quick,
    Raster,
    Done,
}

struct Snapshot {
    phase: Phase,
    /// Target index being waited for
    target: usize,
    /// keys still to be injected (key, time)
    shortcuts: Vec<(egui::Key, f64)>,
    since: f64,
    checks: usize,
    error: Vec<String>,
    folder: String,
    photos: usize,
}

static STATE: Mutex<Option<Snapshot>> = Mutex::new(None);

fn key_id(ctx: &egui::Context, key: egui::Key) {
    ctx.input_mut(|i| i.events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }));
}

impl App {
    pub(crate) fn test_matrix(&mut self, ctx: &egui::Context) {
        let mut g = STATE.lock().unwrap();
        if g.is_none() {
            let Ok(folder) = crate::env::var("GRAINEO_MATRIX") else { return };
            lr::remember_sections_on();
            *g = Some(Snapshot { phase: Phase::Start, target: 0, shortcuts: Vec::new(), since: 0.0, checks: 0, error: Vec::new(), folder, photos: 0 });
        }
        let Some(st) = g.as_mut() else { return };
        // sections of the previous frame (this one redraws right away)
        let sections = lr::take_sections();
        if st.phase == Phase::Done {
            return;
        }
        ctx.request_repaint();
        let now = ctx.input(|i| i.time);
        // due keys
        while let Some(&(k, t)) = st.shortcuts.first() {
            if t > now {
                break;
            }
            key_id(ctx, k);
            st.shortcuts.remove(0);
        }
        if !st.shortcuts.is_empty() {
            st.since = now;
            return;
        }
        let n = self.entries.len();
        if n == 0 || now < 3.0 {
            return;
        }
        if st.phase == Phase::Start {
            self.lib.set_active(self.entries[0].image);
            self.to_single_image();
            self.tool_tab = crate::workspace::ToolTab::Edit;
            self.show_right = true;
            st.phase = Phase::Slow;
            st.target = 0;
            st.since = now;
            crate::log(&format!("matrix: {n} images"));
            return;
        }
        // settled? active image = target, loaded (or not editable), some quiet
        let active = self.active_entry();
        let loaded = self.current.as_ref().map(|c| c.index).or(self.not_developable);
        if active != Some(st.target) || loaded != Some(st.target) || self.loading.is_some() || now - st.since < 1.5 {
            if active != Some(st.target) || loaded != Some(st.target) || self.loading.is_some() {
                st.since = now;
            }
            if now - st.since > 60.0 {
                st.error.push(format!("{}: does not load", self.entries[st.target].name));
                st.phase = Phase::Done;
            }
            return;
        }
        // check
        let e = &self.entries[st.target];
        let kind = image_kind::of(&e.path);
        let route = self.current.as_ref().filter(|c| c.index == st.target).map(|c| if c.loaded.previews.foreign.is_some() { "allgemein" } else { "Sigma" });
        let sigma_group = sections.iter().any(|a| a == "Rauschen & Foveon-Detail");
        let raw_group = sections.iter().any(|a| a == "Rauschminderung");
        let profile = sections.iter().find(|a| a.starts_with("Profil: ")).cloned().unwrap_or_default();
        let sigma_profile = profile.contains("Sigma");
        let ok = match kind {
            ImageKind::Foveon => route == Some("Sigma") && sigma_group && !raw_group && sigma_profile,
            ImageKind::Raw | ImageKind::Done => route == Some("allgemein") && !sigma_group && raw_group && !sigma_profile,
            ImageKind::Other => route.is_none(),
        };
        let groups: Vec<&str> = sections.iter().filter(|a| ["Rauschen & Foveon-Detail", "Rauschminderung", "Schärfen", "Objektivkorrekturen", "Profilkorrektur", "S/W-Mischung (Sensorschichten)"].contains(&a.as_str())).map(String::as_str).collect();
        st.checks += 1;
        crate::log(&format!(
            "matrix [{:?}] {}: file {} → sliders {} · {} · groups {:?} – {}",
            st.phase,
            e.name,
            kind.name(),
            route.unwrap_or("none"),
            profile,
            groups,
            if ok { "OK" } else { "ERROR" }
        ));
        if !ok {
            st.error.push(format!("{} ({:?})", e.name, st.phase));
        }
        // Screenshots: first X3F and first non-X3F (slow pass)
        if st.phase == Phase::Slow && st.photos < 2 {
            let first_of = |a: ImageKind| self.entries.iter().position(|e| image_kind::of(&e.path) == a);
            if Some(st.target) == first_of(ImageKind::Foveon) || Some(st.target) == first_of(ImageKind::Raw) {
                crate::test_image::add(ctx, format!("{}/matrix_{}.png", st.folder, e.name));
                st.photos += 1;
            }
        }
        // next step
        match st.phase {
            Phase::Slow if st.target + 1 < n => {
                st.target += 1;
                st.shortcuts.push((egui::Key::ArrowRight, now + 0.05));
            }
            Phase::Slow => {
                // quickly back to the first image
                st.phase = Phase::Quick;
                for k in 0..st.target {
                    st.shortcuts.push((egui::Key::ArrowLeft, now + 0.05 * (k + 1) as f64));
                }
                st.target = 0;
            }
            Phase::Quick => {
                // grid, two forward, back to single image
                st.phase = Phase::Raster;
                self.to_grid();
                st.shortcuts.push((egui::Key::ArrowRight, now + 0.3));
                st.shortcuts.push((egui::Key::ArrowRight, now + 0.35));
                st.shortcuts.push((egui::Key::Enter, now + 0.5));
                st.target = 2.min(n - 1);
            }
            Phase::Raster | Phase::Start | Phase::Done => {
                st.phase = Phase::Done;
                crate::log(&format!("matrix: {} checks, {} errors {:?}", st.checks, st.error.len(), st.error));
            }
        }
        st.since = now;
    }
}
