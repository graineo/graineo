//! The image the masks compare colours and brightness against, for the photo shown: the development before the first
//! mask (after retouching), small (long side [`SIDE`]), linear RGB. The colour pipette of a colour range takes its
//! samples from it, and the overlay of colour and brightness ranges is computed on it (instead of neutral grey). It is
//! developed once in the background ([`ensure`]), when a colour or brightness range is selected, and again only when
//! the settings outside the masks change.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use develop::local::Captured;
use develop::Settings;

use crate::analysis_progress::{self as progress, Tool};

/// Long side of the kept image (pixels).
pub const SIDE: usize = 1024;

struct Entry {
    /// filmstrip index and path of the photo
    index: usize,
    path: PathBuf,
    /// fingerprint of the settings without the masks
    key: u64,
    image: Option<Arc<Captured>>,
    /// a development for `key` is running
    busy: bool,
}

static CURRENT: Mutex<Option<Entry>> = Mutex::new(None);

/// Fingerprint of the settings that change what the masks see (all but the masks themselves).
pub fn key(s: &Settings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&Settings { local: None, ..s.clone() }).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Make sure the image of photo `index` at `path` with settings `s` is there or being developed (background, one job
/// at a time; a stale image stays usable until the new one is there).
pub fn ensure(ctx: &egui::Context, index: usize, path: &Path, s: &Settings) {
    let k = key(s);
    {
        let mut g = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.as_mut().filter(|e| e.path == path && e.index == index) {
            if e.key == k || e.busy {
                return;
            }
            e.key = k;
            e.busy = true;
        } else {
            if g.as_ref().is_some_and(|e| e.busy) {
                return;
            }
            *g = Some(Entry { index, path: path.to_path_buf(), key: k, image: None, busy: true });
        }
    }
    let ticket = progress::begin(Tool::Sample, path, progress::DEVELOP);
    let (path, settings, ctx) = (path.to_path_buf(), Settings { local: None, ..s.clone() }, ctx.clone());
    let failed = path.clone();
    let work = move || {
        library::platform::set_priority(library::platform::Priority::UserInitiated);
        let t = std::time::Instant::now();
        let r = crate::subject_ui::guarded("mask sample", || {
            let (img, captured) = develop::local::capture(SIDE, || crate::large_preview::render_mask_space(&path, &settings));
            img?;
            captured.ok_or_else(|| "no capture".to_string())
        });
        let mut g = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.as_mut().filter(|e| e.path == path) {
            e.busy = false;
            match r {
                Ok(c) => {
                    crate::log(&format!("mask sample {}: {}×{} in {:.2} s", path.display(), c.width, c.height, t.elapsed().as_secs_f32()));
                    e.image = Some(Arc::new(c));
                }
                Err(err) => crate::log(&format!("mask sample {}: {err}", path.display())),
            }
        }
        drop(g);
        drop(ticket);
        ctx.request_repaint();
    };
    crate::subject_ui::spawn_job("mask-sample", work, |_| {
        if let Some(e) = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|e| e.path == failed) {
            e.busy = false;
        }
    });
}

/// The image of photo `index`, if developed.
pub fn image(index: usize) -> Option<Arc<Captured>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|e| e.index == index).and_then(|e| e.image.clone())
}

/// Colour at `p` (0..1 of the mask space) of photo `index`: mean of 3 × 3 pixels of the small image (≈ 0.3 % of the
/// long side); `None` while it is not developed.
pub fn pick(index: usize, p: [f32; 2]) -> Option<[f32; 3]> {
    image(index).map(|c| sample(&c, p))
}

/// Mean colour of 3 × 3 pixels around `p`.
pub fn sample(c: &Captured, p: [f32; 2]) -> [f32; 3] {
    let x = ((p[0] * c.width as f32) as usize).min(c.width - 1);
    let y = ((p[1] * c.height as f32) as usize).min(c.height - 1);
    let mut acc = [0.0f32; 3];
    let mut n = 0.0;
    for yy in y.saturating_sub(1)..(y + 2).min(c.height) {
        for xx in x.saturating_sub(1)..(x + 2).min(c.width) {
            for k in 0..3 {
                acc[k] += c.data[(yy * c.width + xx) * 3 + k];
            }
            n += 1.0;
        }
    }
    acc.map(|a| a / n)
}

/// The image resampled to `w × h` (nearest pixel), for the overlay.
pub fn resized(c: &Captured, w: usize, h: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let sy = ((y as f32 + 0.5) / h as f32 * c.height as f32) as usize;
        for x in 0..w {
            let sx = ((x as f32 + 0.5) / w as f32 * c.width as f32) as usize;
            let i = (sy.min(c.height - 1) * c.width + sx.min(c.width - 1)) * 3;
            out.extend_from_slice(&c.data[i..i + 3]);
        }
    }
    out
}

/// Does the mask compare against image content (colour or brightness range)?
pub fn needs_image(m: &masks::Mask) -> bool {
    let mut found = false;
    m.visit(&mut |n| found |= matches!(n, masks::Mask::Color { .. } | masks::Mask::ColorRange { .. } | masks::Mask::Luminance { .. }));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_average_and_stay_inside() {
        let c = Captured { width: 4, height: 2, data: (0..24).map(|i| i as f32).collect() };
        // top left corner: pixels (0,0) (1,0) (0,1) (1,1)
        assert_eq!(sample(&c, [0.0, 0.0]), [7.5, 8.5, 9.5]);
        assert_eq!(sample(&c, [1.0, 1.0]), sample(&c, [0.99, 0.99]));
        let r = resized(&c, 2, 1);
        assert_eq!(r.len(), 6);
    }
}
