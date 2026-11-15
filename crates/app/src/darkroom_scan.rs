//! Scan automation from the whole preview image, never separately from zoomed crops.
use crate::{jobs::Job, App};
use std::hash::{Hash, Hasher};
#[derive(Default)]
pub struct Scan {
    pub desired: Option<u64>,
    pub busy: Option<u64>,
    pub ready: Option<u64>,
}
impl App {
    pub(crate) fn darkroom_scan_key(&self) -> Option<u64> {
        let c = self.current.as_ref()?;
        let e = self.entries.get(c.index)?;
        if !e.settings.film.scan_auto {
            return None;
        }
        let mut settings = e.settings.clone();
        settings.film.scan_black = 0.;
        settings.film.scan_white = 1.;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        e.image.hash(&mut h);
        e.path.hash(&mut h);
        format!(
            "{:?}",
            c.loaded
                .previews
                .spp
                .as_ref()
                .and_then(|p| p.spp.as_ref().map(|v| &v.0))
        )
        .hash(&mut h);
        serde_json::to_string(&settings)
            .unwrap_or_default()
            .hash(&mut h);
        Some(h.finish())
    }
    pub(crate) fn darkroom_scan_update(&mut self) {
        if !self.darkroom_active() {
            return;
        }
        let Some(c) = self.current.as_ref() else {
            return;
        };
        let s = &self.entries[c.index].settings;
        if !s.film.scan_auto
            || !s.film.is_active()
            || (!s.film.negative
                && !develop::film::stock(&s.film.stock).is_some_and(|f| f.positive))
        {
            return;
        }
        let mut settings = s.clone();
        settings.film.scan_black = 0.;
        settings.film.scan_white = 1.;
        let Some(key) = self.darkroom_scan_key() else {
            return;
        };
        let scan = &mut self.darkroom.scan;
        scan.desired = Some(key);
        if scan.busy.is_none() && scan.ready != Some(key) {
            scan.busy = Some(key);
            self.worker.send(Job::ScanBounds {
                key,
                image: self.entries[c.index].image,
                path: self.entries[c.index].path.clone(),
                previews: c.loaded.previews.clone(),
                cal: c.loaded.cal.clone(),
                settings,
            });
        }
    }
}
