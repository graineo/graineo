//! One-off after the update to 1.5.0: stored placeholders of Merrill photos at high ISO that were made from the rough
//! fast preview (see `preview_hold.rs`).
//!
//! Grid tiles are never developed in the background: a photo that was never developed shows the camera's embedded
//! picture, a developed one gets its tile from the finished picture in Develop (`App::thumb_from_render`).

impl crate::App {
    /// One-off after the update (marker `roughViewRevision` in the settings): the "last developed view" of a photo
    /// (the placeholder when it opens, `developed_cache.rs`) is stored from its first render, and for Merrill photos
    /// at high ISO that was the rough fast preview until 1.5.0 – a coloured noise picture that would flash up once
    /// more at the next opening. Only these placeholders are dropped (a pure cache, stored again by the next render);
    /// the developed grid previews and offline copies stay as they are: they follow the last render of a photo, which
    /// was the exact one. Nothing is dropped for a photo whose original is not reachable right now (it could not be
    /// developed again).
    pub(crate) fn drop_rough_views_once(&mut self) {
        const MARKER: &str = "roughViewRevision";
        const REVISION: u64 = 1;
        if self.rough_views_checked {
            return;
        }
        if library::settings::value(MARKER).as_u64().unwrap_or(0) >= REVISION {
            self.rough_views_checked = true;
            return;
        }
        // (not before the library list is there)
        let Some(photos) = self.lib.reachable_sigma_images_where(|camera, iso| !camera.to_lowercase().contains("quattro") && crate::preview_hold::rough_by_name(camera, iso)) else { return };
        self.rough_views_checked = true;
        let mut dropped = 0;
        for image in photos {
            if crate::browse::developed(image).is_some() {
                crate::browse::forget_developed(image);
                crate::developed_cache::discard(image);
                // also the copy in memory (the photo may be the one opening right now)
                if self.developed_tex.as_ref().is_some_and(|(b, ..)| *b == image) {
                    self.developed_tex = None;
                }
                dropped += 1;
            }
        }
        crate::log(&format!("placeholders of {dropped} Merrill photos at high ISO dropped (stored from the rough fast preview)"));
        library::settings::set(MARKER, serde_json::json!(REVISION));
    }
}
