//! What the zoomed view of the shown photo asks for at full resolution: nothing, a crop around the visible part, the
//! whole image, or waiting for Sigma's exact base. A pure function of the facts of one frame (view and photo), without
//! egui and without the jobs, so that its rules are tested directly – above all the zoom guard: **a photo is computed
//! once per settings state as a whole image at full resolution; afterwards every zoom in or out and every pan draws
//! only from that image and computes nothing, until a slider moves or the photo changes.** The first crop with a
//! margin before the whole image stays (fast sharp view of the visible part).
//!
//! `App::viewer` collects the facts, calls [`decide`] and carries out the [`Request`] (sending the job, setting
//! `detail_pending` / `full_job`, the repaint).

use develop::Settings;

use crate::{contains, region_with_margin as with_margin};

/// Without zoom or pan for this long the view counts as resting (no crop during a gesture).
pub const SETTLED: f64 = 0.15;
/// A hovered preset gets its crop only after the view rested this long.
pub const HOVER_SETTLED: f64 = 0.25;
/// The whole image in the background (visible part already sharp) only once the settings rested this long.
pub const SETTINGS_REST: f64 = 0.5;
/// Margin around the visible part for the first crop (share of its size on each side).
pub const MARGIN: f32 = 0.25;
/// A crop with margin covering at least this share of the image: the whole image right away.
pub const NEARLY_ALL: f64 = 0.6;
/// Texture budget of the whole image plus its half level in 8 bit (bytes per pixel × pixels ≤ 256 MB, ≈ 51 MP);
/// larger images stay with crops. With preserved gamut (8 bytes per pixel) up to 45 MP (`WIDE_FULL_MAX_PIXELS` in
/// `main.rs`), above that the 8-bit whole image (`detail_job`).
pub const BUDGET_BYTES: usize = 256_000_000;

/// An image at full resolution the view holds: its source rectangle and the settings it was computed with.
#[derive(Clone, Copy)]
pub struct Have<'a> {
    pub region: [usize; 4],
    pub settings: &'a Settings,
}

/// The facts of one frame of the zoomed view.
pub struct Facts<'a> {
    /// visible part of the image in source pixels
    pub region: [usize; 4],
    /// size of the image at full resolution
    pub full: [usize; 2],
    /// magnification of the film grain loupe (1: plain view)
    pub factor: usize,
    /// seconds since the last zoom or pan
    pub since_view_change: f64,
    /// seconds the current settings have been resting
    pub settings_rest: f64,
    /// HDR gain map: its EDR layer works on crops, never the whole image
    pub has_gain_map: bool,
    /// Whether a full-resolution whole-image texture is allowed in this view.
    pub allow_full: bool,
    /// current settings of the photo
    pub settings: &'a Settings,
    /// hovered preset or film (only when it differs from the settings) and whether it is still the hovered one
    pub hover: Option<&'a Settings>,
    pub hover_is_preset: bool,
    /// the detail image for `factor` (grain loupe: its own slot), the whole-image slot, the hover crop
    pub detail: Option<Have<'a>>,
    pub whole: Option<Have<'a>>,
    pub detail_hover: Option<Have<'a>>,
    /// a crop job is on its way; a failed one pauses further attempts (`blocked`)
    pub detail_pending: bool,
    pub blocked: bool,
    /// settings of the whole image being computed, a finished one waiting for the view to rest, settings for which
    /// the whole image failed
    pub full_job: Option<&'a Settings>,
    pub full_waiting: bool,
    pub full_failed: Option<&'a Settings>,
    /// Sigma's exact base is there for these settings (or not needed); asked only where it decides something
    pub ready: &'a dyn Fn(&Settings) -> bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    Nothing,
    /// the base for these settings is missing: wait (the view keeps the zoomed preview), look again in 100 ms
    WaitForSpp,
    /// crop at full resolution (`hover`: with the hovered settings, without margin)
    Region { hover: bool, region: [usize; 4] },
    /// whole image (`nearly_all`: the visible part was nearly all of it; else in the background, sharp already)
    Full { nearly_all: bool },
    /// nothing yet, look again after this many seconds (the settings have not rested long enough)
    Later(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    /// the visible part is not sharp yet for the current settings (loading spinner, log "sharp after")
    pub stale: bool,
    pub request: Request,
}

fn covers(have: Option<Have>, settings: &Settings, region: [usize; 4]) -> bool {
    have.is_some_and(|h| h.settings == settings && (h.region == region || contains(h.region, region)))
}

/// What the zoomed view asks for in this frame.
pub fn decide(f: &Facts) -> Decision {
    let settled = f.since_view_change > SETTLED;
    // hovered preset: its own crop (never the whole image)
    if let Some(hs) = f.hover {
        let hover_stale = !covers(f.detail_hover, hs, f.region);
        let request = if hover_stale && f.hover_is_preset && !f.detail_pending && !f.blocked && f.since_view_change > HOVER_SETTLED && (f.ready)(hs) {
            Request::Region { hover: true, region: f.region }
        } else {
            Request::Nothing
        };
        return Decision { stale: false, request };
    }
    let s = f.settings;
    let stale = !covers(f.detail, s, f.region);
    let [fw, fh] = f.full;
    let full_have = f.whole.is_some_and(|h| h.settings == s && contains(h.region, [0, 0, fw, fh]));
    let full_running = f.full_job == Some(s);
    let within_budget = fw * fh * 5 <= BUDGET_BYTES;
    let full_free = f.allow_full && f.factor == 1 && !f.has_gain_map && within_budget && !full_have && f.full_job.is_none() && !f.full_waiting && f.full_failed != Some(s) && !f.blocked;
    // first a crop around the visible part (fast) with a generous margin, so that small pans stay sharp; if that would
    // be nearly the whole image anyway, the whole image right away
    let request = with_margin(f.region, MARGIN, f.full);
    let nearly_all = f.factor == 1 && (request[2] * request[3]) as f64 >= NEARLY_ALL * (fw * fh) as f64;
    let resting = f.settings_rest >= SETTINGS_REST;
    let ready = || (f.ready)(s);
    let request = if stale && !f.detail_pending && !f.blocked && settled && !ready() {
        Request::WaitForSpp
    } else if stale && full_free && nearly_all && settled && ready() {
        Request::Full { nearly_all: true }
    } else if stale && !(nearly_all && full_running) && !f.detail_pending && !f.blocked && settled {
        Request::Region { hover: false, region: request }
    } else if !stale && full_free && resting && !f.detail_pending && ready() {
        Request::Full { nearly_all: false }
    } else if !stale && full_free && !resting {
        Request::Later(0.15)
    } else {
        Request::Nothing
    };
    Decision { stale, request }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: [usize; 2] = [4704, 3136];

    fn facts<'a>(s: &'a Settings) -> Facts<'a> {
        Facts {
            region: [1000, 800, 1200, 800],
            full: FULL,
            factor: 1,
            since_view_change: 1.0,
            settings_rest: 1.0,
            has_gain_map: false,
            allow_full: true,
            settings: s,
            hover: None,
            hover_is_preset: false,
            detail: None,
            whole: None,
            detail_hover: None,
            detail_pending: false,
            blocked: false,
            full_job: None,
            full_waiting: false,
            full_failed: None,
            ready: &|_| true,
        }
    }

    /// small pseudo-random numbers without a dependency
    fn rng(seed: &mut u64) -> usize {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as usize
    }

    /// The zoom guard as a unit test: the whole image of the current settings is there – 200 random zoom and pan
    /// positions (visible parts of every size, also during and right after a gesture) ask for nothing.
    #[test]
    fn zoom_pan_after_full_requests_nothing() {
        let s = Settings::default();
        let whole = Have { region: [0, 0, FULL[0], FULL[1]], settings: &s };
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..200 {
            let w = 50 + rng(&mut seed) % (FULL[0] - 50);
            let h = 50 + rng(&mut seed) % (FULL[1] - 50);
            let x = rng(&mut seed) % (FULL[0] - w + 1);
            let y = rng(&mut seed) % (FULL[1] - h + 1);
            let mut f = facts(&s);
            f.region = [x, y, w, h];
            f.detail = Some(whole);
            f.whole = Some(whole);
            f.since_view_change = (rng(&mut seed) % 2000) as f64 / 1000.0;
            f.settings_rest = (rng(&mut seed) % 2000) as f64 / 1000.0;
            let d = decide(&f);
            assert_eq!(d, Decision { stale: false, request: Request::Nothing }, "region {:?}", f.region);
        }
    }

    /// Zoomed into a small part: first a crop with margin around it (not the whole image), but only once the view rests.
    #[test]
    fn first_crop_with_margin_before_whole() {
        let s = Settings::default();
        let mut f = facts(&s);
        f.since_view_change = 0.05;
        assert_eq!(decide(&f), Decision { stale: true, request: Request::Nothing }, "nothing during the gesture");
        f.since_view_change = 0.3;
        let d = decide(&f);
        assert!(d.stale);
        assert_eq!(d.request, Request::Region { hover: false, region: with_margin(f.region, MARGIN, FULL) });
        // once the crop is there and the settings rest: the whole image in the background
        let crop = Have { region: with_margin(f.region, MARGIN, FULL), settings: &s };
        f.detail = Some(crop);
        f.whole = Some(crop);
        assert_eq!(decide(&f), Decision { stale: false, request: Request::Full { nearly_all: false } });
        // settings still moving: look again later, nothing requested
        f.settings_rest = 0.2;
        assert_eq!(decide(&f).request, Request::Later(0.15));
    }

    /// The visible part is nearly the whole image: the whole image at once, no crop first.
    #[test]
    fn nearly_all_requests_full() {
        let s = Settings::default();
        let mut f = facts(&s);
        f.region = [200, 200, 4000, 2600];
        assert_eq!(decide(&f), Decision { stale: true, request: Request::Full { nearly_all: true } });
        // the whole image is already on its way: no crop besides it
        f.full_job = Some(&s);
        assert_eq!(decide(&f).request, Request::Nothing);
    }

    /// A hovered preset gets a crop of its own (without margin), never the whole image.
    #[test]
    fn hover_never_requests_full() {
        let s = Settings::default();
        let h = Settings { exposure: 0.5, ..Settings::default() };
        let mut f = facts(&s);
        f.region = [0, 0, FULL[0], FULL[1]];
        f.hover = Some(&h);
        f.hover_is_preset = true;
        assert_eq!(decide(&f), Decision { stale: false, request: Request::Region { hover: true, region: f.region } });
        f.detail_hover = Some(Have { region: f.region, settings: &h });
        assert_eq!(decide(&f).request, Request::Nothing);
        // the pointer has moved on to another preset (the old one is no longer the hovered one)
        f.detail_hover = None;
        f.hover_is_preset = false;
        assert_eq!(decide(&f).request, Request::Nothing);
    }

    #[test]
    fn sensor_view_requests_visible_crops_without_full_image_prefetch() {
        let s = Settings::default();
        let mut f = facts(&s);
        f.allow_full = false;
        let region = match decide(&f).request {
            Request::Region { hover: false, region } => region,
            other => panic!("expected a visible crop, got {other:?}"),
        };
        f.detail = Some(Have { region, settings: &s });
        assert_eq!(decide(&f).request, Request::Nothing);
        f.detail = None;
        f.region = [0, 0, FULL[0], FULL[1]];
        assert!(matches!(decide(&f).request, Request::Region { hover: false, .. }));
    }

    /// Grain loupe (factor > 1): enlarged crops only, never the whole image.
    #[test]
    fn grain_loupe_factor_gt_1_never_full() {
        let s = Settings::default();
        let mut f = facts(&s);
        f.factor = 2;
        f.region = [0, 0, FULL[0], FULL[1]];
        assert!(matches!(decide(&f).request, Request::Region { hover: false, .. }));
        let crop = Have { region: f.region, settings: &s };
        f.detail = Some(crop);
        assert_eq!(decide(&f).request, Request::Nothing);
    }

    /// While a crop is on its way or after a failure (pause), nothing new is requested; without the base: wait.
    #[test]
    fn no_request_while_pending_or_blocked() {
        let s = Settings::default();
        let mut f = facts(&s);
        f.detail_pending = true;
        assert_eq!(decide(&f).request, Request::Nothing);
        f.detail_pending = false;
        f.blocked = true;
        assert_eq!(decide(&f).request, Request::Nothing);
        f.blocked = false;
        f.ready = &|_| false;
        assert_eq!(decide(&f).request, Request::WaitForSpp);
    }

    /// Settings changed: the whole image of the old settings no longer counts, a crop of the new ones is asked for;
    /// the old whole image still on its way blocks no crop and no second whole image is requested beside it.
    #[test]
    fn settings_change_makes_the_old_whole_image_stale() {
        let old = Settings::default();
        let new = Settings { exposure: 0.3, ..Settings::default() };
        let whole = Have { region: [0, 0, FULL[0], FULL[1]], settings: &old };
        let mut f = facts(&new);
        f.detail = Some(whole);
        f.whole = Some(whole);
        let d = decide(&f);
        assert!(d.stale);
        assert!(matches!(d.request, Request::Region { hover: false, .. }));
        // a whole image of the new settings is running: no second one
        f.full_job = Some(&new);
        f.detail = Some(Have { region: with_margin(f.region, MARGIN, FULL), settings: &new });
        assert_eq!(decide(&f).request, Request::Nothing);
    }

    /// The whole image failed for these settings, or a gain-map HDR or an oversized image: crops only.
    #[test]
    fn no_whole_image_when_failed_hdr_or_over_budget() {
        let s = Settings::default();
        for case in 0..3 {
            let mut f = facts(&s);
            match case {
                0 => f.full_failed = Some(&s),
                1 => f.has_gain_map = true,
                _ => f.full = [12000, 8000],
            }
            let crop = Have { region: with_margin(f.region, MARGIN, f.full), settings: &s };
            f.detail = Some(crop);
            f.whole = Some(crop);
            assert_eq!(decide(&f).request, Request::Nothing, "case {case}");
        }
    }
}
