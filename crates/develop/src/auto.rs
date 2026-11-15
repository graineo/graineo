//! Automatic tone ("Auto"): estimates exposure, contrast,
//! highlights, shadows and, under strong backlight, Fill Light from the preview.
//!
//! Measured not against a model but against the actual result: the preview is downscaled to
//! ≈ 320 px and developed with candidate values through the same output stage (Sigma tone curve,
//! gamma); brightness distribution and clipping of the result drive the search.
//! The goal is a conservatively "correct" exposure: mean brightness close to a
//! middle grey (the original mood of the image is partly preserved), no newly
//! blown highlights, shadows lifted only as far as the noise of the ISO setting
//! allows (Foveon shadows get noisy quickly).

use crate::session::Preview;
use crate::{Calibration, Linear, Settings};

/// Result of the analysis: values for the tone sliders.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoTone {
    pub exposure: f64,
    pub contrast: f64,
    pub highlight: f64,
    pub shadow: f64,
    pub fill_light: f32,
}

impl AutoTone {
    /// Rounded to the slider step (0.05).
    pub fn snapped(&self) -> Self {
        Self {
            exposure: snap(self.exposure).clamp(-3.0, 3.0),
            contrast: snap(self.contrast).clamp(-2.0, 2.0),
            highlight: snap(self.highlight).clamp(-2.0, 2.0),
            shadow: snap(self.shadow).clamp(-2.0, 2.0),
            fill_light: snap(self.fill_light as f64).clamp(-2.0, 2.0) as f32,
        }
    }

    /// Apply the values to the settings.
    pub fn apply_to(&self, s: &mut Settings) {
        s.exposure = self.exposure;
        s.contrast = self.contrast;
        s.highlight = self.highlight;
        s.shadow = self.shadow;
        s.fill_light = self.fill_light;
    }
}

/// Longer edge of the analysis image in pixels.
const EDGE: usize = 320;
/// Target mean brightness (sRGB luma 0 … 1, ≈ 117/255).
const TARGET: f64 = 0.46;
/// Minimum share of the original brightness that is preserved (mood: night stays
/// darker, bright scenes stay bright).
const KEEP: f64 = 0.25;

/// Metrics of a developed analysis image (sRGB luma, relative to the tone curve's white).
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Average of median and mean
    pub key: f64,
    pub p01: f64,
    pub p05: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub p998: f64,
    /// Share of pixels with at least one channel at white
    pub clip: f64,
    /// Share of very dark pixels (luma < 0.04)
    pub dark: f64,
}

/// Downscaled preview plus Fill Light map, used to develop candidates.
struct Probe<'a> {
    lin: Linear,
    map: Vec<f32>,
    cal: &'a Calibration,
    base: Settings,
    spp: Option<crate::matrix::Mat3>,
    /// Weight per pixel (image centre slightly stronger)
    weight: Vec<f32>,
}

impl Probe<'_> {
    fn eval(&self, t: &AutoTone) -> Stats {
        // Rounded to the slider step: repeated candidates and the app's later render
        // hit the tone curve cache
        let mut s = self.base.clone();
        t.snapped().apply_to(&mut s);
        let map = (t.fill_light != 0.0).then_some(self.map.as_slice());
        let img = match &self.spp {
            Some(m) => crate::render_counts(&self.lin, self.cal, &s, m, map),
            None => crate::render(&self.lin, self.cal, &s, map),
        };
        // White of the tone curve (largest count value); metrics are relative to it
        let mode = self.cal.mode(s.color_mode.as_deref());
        let tone = crate::cached_tone_lut(&mode.tone, &s, if s.monochrome { 0.0 } else { mode.contrast }, self.cal.pixel_saturation, self.cal.is_prepared());
        let white = (crate::gamma_lut()[tone[crate::sigma_out::tone_index(self.cal.max_count())] as usize] as f64).max(1.0);
        const BINS: usize = 1024;
        let mut hist = vec![0f64; BINS];
        let (mut total, mut sum, mut clip, mut dark) = (0f64, 0f64, 0f64, 0f64);
        for (i, p) in img.data.chunks_exact(3).enumerate() {
            let wt = self.weight[i] as f64;
            let v = [p[0] as f64, p[1] as f64, p[2] as f64];
            let l = (luma(v) / white).clamp(0.0, 1.0);
            hist[((l * (BINS - 1) as f64) + 0.5) as usize] += wt;
            total += wt;
            sum += wt * l;
            if v.iter().any(|&c| c >= 0.985 * white) {
                clip += wt;
            }
            if l < 0.04 {
                dark += wt;
            }
        }
        let pct = |q: f64| -> f64 {
            let limit = q * total;
            let mut acc = 0.0;
            for (i, &v) in hist.iter().enumerate() {
                acc += v;
                if acc >= limit {
                    return i as f64 / (BINS - 1) as f64;
                }
            }
            1.0
        };
        let p50 = pct(0.5);
        Stats {
            key: 0.5 * (p50 + sum / total),
            p01: pct(0.01),
            p05: pct(0.05),
            p50,
            p95: pct(0.95),
            p99: pct(0.99),
            p998: pct(0.998),
            clip: clip / total,
            dark: dark / total,
        }
    }

    /// Exposure at which the mean brightness reaches `want` (secant method on the
    /// logarithm of brightness; the other sliders from `t`).
    fn solve_exposure(&self, t: &AutoTone, want: f64, start: (f64, Stats)) -> (f64, Stats) {
        let f = |st: &Stats| st.key.max(1e-3).ln() - want.ln();
        let (mut e0, mut s0) = start;
        // First step: luma grows roughly by 0.4 · ln 2 per stop
        let mut e1 = snap((e0 - f(&s0) / (0.4 * std::f64::consts::LN_2)).clamp(-3.0, 3.0));
        let mut s1 = self.eval(&AutoTone { exposure: e1, ..*t });
        for _ in 0..3 {
            if f(&s1).abs() < 0.02 {
                break;
            }
            let slope = (f(&s1) - f(&s0)) / (e1 - e0);
            let next = if slope.is_finite() && slope > 0.05 { e1 - f(&s1) / slope } else { e1 - f(&s1) / (0.4 * std::f64::consts::LN_2) };
            let next = snap(next.clamp(-3.0, 3.0));
            if (next - e1).abs() < 0.025 {
                break;
            }
            (e0, s0) = (e1, s1);
            e1 = next;
            s1 = self.eval(&AutoTone { exposure: e1, ..*t });
        }
        (e1, s1)
    }
}

#[inline]
fn luma(v: [f64; 3]) -> f64 {
    0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]
}

/// Round to the slider step (0.05).
fn snap(v: f64) -> f64 {
    (v * 20.0).round() / 20.0
}

/// Analysis image from the preview: crop region, downscale.
fn probe<'a>(preview: &Preview, cal: &'a Calibration, s: &Settings) -> Option<Probe<'a>> {
    let src = &preview.lin;
    if src.width < 8 || src.height < 8 {
        return None;
    }
    // Crop frame (axis-aligned bounding box) in preview pixels
    let f = preview.factor as f32;
    let (fw, fh) = (src.width as f32 * f, src.height as f32 * f);
    let frame = crate::geometry::Frame::from_settings(s, fw as usize, fh as usize, cal.orientation);
    let [bx, by, bw, bh] = frame.bounds();
    let to_p = |v: usize| (v as f32 / f) as usize;
    let (x0, y0) = (to_p(bx).min(src.width - 2), to_p(by).min(src.height - 2));
    let (cw, ch) = (to_p(bw).clamp(2, src.width - x0), to_p(bh).clamp(2, src.height - y0));
    let factor = cw.max(ch).div_ceil(EDGE).max(1);
    let lin = src.crop(x0, y0, cw, ch).downsample(factor);
    let (w, h) = (lin.width, lin.height);
    if w < 2 || h < 2 {
        return None;
    }
    // Average the Fill Light map (logarithm) block by block
    let mut map = vec![0f32; w * h];
    if preview.fill_map.len() == src.width * src.height {
        let n = (factor * factor) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0f32;
                for yy in 0..factor {
                    let row = (y0 + y * factor + yy) * src.width + x0 + x * factor;
                    acc += preview.fill_map[row..row + factor].iter().sum::<f32>();
                }
                map[y * w + x] = acc / n;
            }
        }
    }
    let spp = preview.spp.as_ref().map(|(_, m)| *m);
    // Centre weighting: 1 in the centre, 0.5 in the corners
    let weight = (0..h)
        .flat_map(|y| {
            (0..w).map(move |x| {
                let dx = (x as f32 + 0.5) / w as f32 - 0.5;
                let dy = (y as f32 + 0.5) / h as f32 - 0.5;
                1.0 - (dx * dx + dy * dy)
            })
        })
        .collect();
    let mut base = s.effective().into_owned();
    base.view = crate::View::Developed;
    base.sigma_sharpness = 0.0;
    Some(Probe { lin, map, cal, base, spp, weight })
}

/// Automatic tone for the preview image `preview` with the remaining settings `s`
/// (white balance, colour mode, presence stay as they are).
pub fn auto_tone(preview: &Preview, cal: &Calibration, s: &Settings) -> Option<AutoTone> {
    auto_tone_traced(preview, cal, s).map(|(t, _)| t)
}

/// Like [`auto_tone`], additionally with before/after metrics (for the check tool).
pub fn auto_tone_traced(preview: &Preview, cal: &Calibration, s: &Settings) -> Option<(AutoTone, [Stats; 2])> {
    let probe = probe(preview, cal, s)?;
    // Noise: less headroom for lifting per stop above ISO 100
    let iso_stops = (cal.iso.max(100.0) / 100.0).log2();
    let max_push = (2.0 - 0.4 * iso_stops).clamp(0.5, 2.0);
    let max_lift = (0.8 - 0.25 * iso_stops).clamp(0.0, 0.8);

    let mut t = AutoTone::default();
    let s0 = probe.eval(&t);
    // Target: middle grey, but part of the original mood is preserved
    // (very dark scenes, night or low key, keep more of it; otherwise only noise gets brighter)
    let keep = KEEP + 0.35 * ((0.25 - s0.key) / 0.2).clamp(0.0, 1.0);
    let want = (TARGET.powf(1.0 - keep) * s0.key.max(0.02).powf(keep)).clamp(0.10, 0.58);
    let (mut ev, mut st) = probe.solve_exposure(&t, want, (0.0, s0));

    // Bright, low-contrast scenes (snow, fog, beach): white should be white
    if st.p998 < 0.90 && st.key < 0.6 {
        let lift = ((0.94 / st.p998.max(0.05)).ln() / (0.4 * std::f64::consts::LN_2)).min(0.7);
        if lift > 0.05 {
            let cand = probe.eval(&AutoTone { exposure: (ev + lift).min(3.0), ..t });
            if cand.key < 0.66 {
                (ev, st) = ((ev + lift).min(3.0), cand);
            }
        }
    }

    // No newly blown highlights: pull exposure back until clipping is barely above
    // that of the starting point (areas already blown there are lost anyway)
    let allowed = s0.clip * 1.08 + 0.002;
    if st.clip > allowed && ev > 0.0 {
        let (mut lo, mut hi) = (0.0f64.min(ev), ev);
        for _ in 0..4 {
            let mid = 0.5 * (lo + hi);
            if probe.eval(&AutoTone { exposure: mid, ..t }).clip > allowed {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        ev = lo;
        st = probe.eval(&AutoTone { exposure: ev, ..t });
    }
    ev = ev.min(max_push);
    t.exposure = ev;

    // Sigma's highlight/shadow sliders mainly act on the brightness of the upper or
    // lower tonal range (hardly any recovery); the effective tool for a large dynamic range
    // is Fill Light (lifts shadows locally and pulls highlights back).
    // Highlights: pull back wide bright areas just below white a little
    if st.p95 > 0.88 {
        t.highlight = -((st.p95 - 0.88) * 8.0).clamp(0.0, 0.5);
    }
    // Large dynamic range (deep shadows with real area, plus bright highlights): Fill Light
    let backlit = st.dark > 0.10 && (st.p99 > 0.85 || st.clip > 0.005);
    if backlit {
        t.fill_light = ((st.dark - 0.06) * 2.5).clamp(0.0, max_lift.min(0.5)) as f32;
    } else if st.dark > 0.03 {
        // Shadows: open dark image regions of noticeable area carefully
        t.shadow = (st.dark * 5.0).min(max_lift);
    }
    // Contrast: stretch flat images (fog, backlight haze) a little, not for night
    // and low key, where it would mostly make the noise stronger
    let range = st.p99 - st.p01;
    if range < 0.75 && !backlit && s0.key > 0.15 {
        t.contrast = ((0.75 - range) * 1.5).min(0.5);
    }

    // Restore the mean brightness with the new sliders
    if t != (AutoTone { exposure: ev, ..AutoTone::default() }) {
        let cur = probe.eval(&t);
        let (e, _) = probe.solve_exposure(&t, st.key, (t.exposure, cur));
        t.exposure = e.min(max_push);
    }
    let t = t.snapped();
    Some((t, [s0, probe.eval(&t)]))
}
