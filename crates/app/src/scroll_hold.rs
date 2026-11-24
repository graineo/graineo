//! Keeps the scroll position of a side panel while its content is briefly replaced by a placeholder.
//!
//! When the photo on screen is opened again (binning switch, reload) or another photo loads, the Edit panel draws
//! only a short "loading" line for a few frames. egui clamps the scroll offset to the content height, so the whole
//! panel jumped back to the top and stayed there once the controls returned. While the content is transient, the
//! panel is padded to the height of the last real content; the offset survives and the controls come back where
//! they were. Real changes of the content (collapsing a section, another tool) are not padded.

use eframe::egui;

/// Pure rule: height of the padding to add and the remembered height for the next frame.
/// `remembered`: content height of the last real frame; `now`: height drawn this frame.
pub fn pad(remembered: Option<f32>, now: f32, transient: bool) -> (f32, Option<f32>) {
    if transient {
        let target = remembered.unwrap_or(now);
        ((target - now).max(0.0), remembered)
    } else {
        (0.0, Some(now))
    }
}

/// Runs `add_contents` inside a scroll area and pads the content as described above. `transient` is asked after
/// drawing (the content decides it, e.g. by drawing a placeholder).
pub fn scope<R>(ui: &mut egui::Ui, key: &str, add_contents: impl FnOnce(&mut egui::Ui) -> (R, bool)) -> R {
    let top = ui.min_rect().top();
    let (r, transient) = add_contents(ui);
    let id = egui::Id::new(("scroll-hold", key));
    let remembered = ui.ctx().data(|d| d.get_temp::<f32>(id));
    let now = ui.min_rect().bottom() - top;
    let (extra, keep) = pad(remembered, now, transient);
    if extra > 0.0 {
        ui.allocate_space(egui::vec2(0.0, extra));
    }
    if let Some(h) = keep {
        ui.ctx().data_mut(|d| d.insert_temp(id, h));
    }
    r
}

/// Test hook `GRAINEO_SIDEBAR_SCROLL=<points>`: scroll offset for the first draw of a panel with real content only
/// (`ready`); afterwards the panel scrolls normally, so a test can show that the position survives later frames.
pub fn first_offset(ui: &egui::Ui, key: &str, ready: bool) -> Option<f32> {
    if !ready {
        return None;
    }
    let px = crate::env::var("GRAINEO_SIDEBAR_SCROLL").ok()?.parse::<f32>().ok()?;
    let id = egui::Id::new(("scroll-hold-test", key));
    if ui.ctx().data(|d| d.get_temp::<bool>(id)).is_some() {
        return None;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, true));
    Some(px)
}

#[cfg(test)]
mod tests {
    use super::pad;

    #[test]
    fn placeholder_keeps_height() {
        // real content 1800, then a loading line of 20: padded to 1800, remembered height unchanged
        let (_, m) = pad(None, 1800.0, false);
        assert_eq!(m, Some(1800.0));
        let (extra, m) = pad(m, 20.0, true);
        assert_eq!(extra, 1780.0);
        assert_eq!(m, Some(1800.0));
        // controls back: no padding, new height remembered
        assert_eq!(pad(m, 1750.0, false), (0.0, Some(1750.0)));
    }

    #[test]
    fn real_shrink_is_not_padded() {
        // collapsing a section is a real change: no padding
        assert_eq!(pad(Some(1800.0), 600.0, false), (0.0, Some(600.0)));
        // nothing remembered yet: nothing to pad
        assert_eq!(pad(None, 20.0, true), (0.0, None));
    }
}
