//! Fast output stage for non-Sigma RAWs: the linear, white-balanced camera RGB
//! (after noise reduction and lens correction, [`crate::Stage`]) becomes, with the sliders, an
//! sRGB image (16 bit) – followed by the same look stage as for X3F ([`develop::look::apply`]).
//!
//! Chain per pixel (all linear up to the tone curve, working space Rec. 2020 as in [`raw::development`]):
//! 1. White balance: factor target/as-shot per channel, color matrix according to the DNG specification for the
//!    target white point (temperature/tint or preset/auto).
//! 2. Exposure: base exposure of the standard development (camera, BaselineExposure, Fuji DR) + slider.
//! 3. Shadows/highlights (locally via a soft luminance map, [`Stage::mask`]) and whites/blacks
//!    (per pixel) as exposure in EV depending on luminance (tone equalizer).
//! 4. Sigmoid tone curve with hue preservation ([`raw::tone`], darktable), contrast via the slope.
//! 5. Presence (vibrance, saturation) in linear sRGB, black & white if selected.
//! 6. sRGB encoding, camera look ([`raw::looks`], intensity), 16 bit.
//!
//! Film (spektrafilm) replaces 4.–6.: the scene-linear sRGB after 3. goes into [`develop::film`].

use std::sync::Arc;

use develop::look::Geo;
use develop::{Linear, Rgb16, Settings};
use raw::color::{self, M3};
use raw::tone::{Sigmoid, ToneCurve};
use rayon::prelude::*;

use crate::RawDoc;

/// Everything that is computed once per slider state.
pub struct Params {
    /// camera (as-shot WB) → working space, incl. WB change and exposure
    m: M3,
    /// luminance weights of the working space
    lum: [f32; 3],
    /// working space → linear sRGB
    ws_srgb: M3,
    curve: ToneCurve,
    /// finished image: instead of the sigmoid curve only contrast around middle gray (default = unchanged)
    finished: Option<f32>,
    shadows: f32,
    highlights: f32,
    whites: f32,
    blacks: f32,
    /// Highlight roll of a lowered Whites slider: reach in stops (0 = off) and the scene white of the curve
    /// ([`raw::tone::whites_roll`])
    roll_reach: f32,
    roll_white: f32,
    /// camera profile (DCP): HueSatMap and LookTable in the working space (CPU only)
    dcp: Option<raw::dcp::Stage>,
    /// total exposure in EV (for the luminance map)
    ev: f32,
    look: Option<Arc<raw::looks::Look>>,
    look_amount: f32,
    std_look: Option<Arc<raw::development::LookLut>>,
    saturation: f32,
    vibrance: f32,
    mono: bool,
    /// black & white mixer (only with `mono`)
    gray_mix: [f32; 8],
    /// calibration: shadow tint in EV (green, + = magenta)
    shadow_tint: f32,
    /// Noise brake active: exponents of luminance/colour detail and the exposure slider (CPU: [`prepare`]; GPU: in the
    /// output stage, base layer computed there)
    brake: Option<crate::gpu::GpuBrake>,
    /// Tone equalizer set or its mask shown: only the CPU stage has it ([`prepare`])
    tone_eq: bool,
}

/// Effect of the black & white mixer: at +100 the color gets lighter by this fraction (darker at −100). On 93 reference exports (mostly
/// the reference's automatic mix, small values) 0.4 was narrowly best (ΔE 2.54 vs. 2.56 without mixer, worse with larger factors)
const GRAY_MIXER_FACTOR: f32 = 0.4;

pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Encoding linear → sRGB (0 … 1) as a table over 0 … 1 linear (65 536 steps).
fn encode_lut() -> &'static [f32] {
    static LUT: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    LUT.get_or_init(|| (0..=65536).map(|i| raw::srgb_oetf(i as f32 / 65536.0)).collect())
}

#[inline]
fn encode(v: f32) -> f32 {
    let t = v.clamp(0.0, 1.0) * 65536.0;
    let i = (t as usize).min(65535);
    let f = t - i as f32;
    let l = encode_lut();
    l[i] + f * (l[i + 1] - l[i])
}

impl Params {
    pub fn new(doc: &RawDoc, s: &Settings) -> Self {
        let r = &s.raw;
        let wb_shot = doc.info.wb.unwrap_or([1.0; 3]);
        let n_shot = [1.0 / wb_shot[0], 1.0, 1.0 / wb_shot[2]].map(|v| v * wb_shot[1]);
        let n_target = doc.target_neutral(s).unwrap_or(n_shot);
        let scale_wb = [n_shot[0] / n_target[0], n_shot[1] / n_target[1], n_shot[2] / n_target[2]];
        let ab = doc.info.analog_balance.map(color::diag).unwrap_or(color::diag([1.0; 3]));
        let ws_xyz = color::rec2020_to_xyz_d65();
        // a camera profile (DCP) brings its own matrices and tables; without it (or unreadable) the file's matrices
        // linear scene rendering: no tone curve (contrast as for finished images), no camera profile tables, look, hue
        // table or saturation correction
        let linear = doc.is_linear(s);
        let profile = if doc.finished || doc.apple.is_some() || linear { None } else { r.camera_profile.as_deref().and_then(camera_profile) };
        let cal = profile.as_ref().map_or(&doc.info.calibration, |p| &p.calibration);
        let det = color::determine(n_target, cal, &ab);
        let cam_xyz = det.map(|f| f.camera_to_xyz_d50).unwrap_or(doc.cam_xyz_shot);
        let dcp = profile.as_ref().and_then(|p| raw::dcp::Stage::new(p, &ws_xyz, det.map_or(0.5, |d| d.weight), s.exposure as f32, std::env::var_os("GRAINEO_DCP_NO_LOOK").is_none()));
        let cam_ws = color::mul(&color::xyz_d50_to(&ws_xyz), &cam_xyz);
        let ws_srgb = color::mul(&color::inv(&color::srgb_to_xyz_d65()).expect("sRGB"), &ws_xyz);
        let ev = doc.base_ev_for(s) + s.exposure as f32;
        let gain = ev.exp2();
        let m = color::mul(&cam_ws, &color::diag(scale_wb)).map(|row| row.map(|v| v * gain));
        // calibration (primaries) acts like a modified profile: in the working space after the camera matrix
        let m = crate::calibration::matrix(&r.calibration).map_or(m, |k| color::mul(&k, &m));
        let c = s.contrast.clamp(-1.0, 1.0) as f32;
        let mut look = if s.film.is_active() { None } else { doc.look_for(s) };
        // A bundled camera look (picture style) is a table fitted to the camera JPEG on the plain standard development:
        // without the per-camera saturation correction and without the camera hue table (`std_look`). It already
        // contains the camera's saturation, so on top of the correction it desaturated twice (Ricoh GR IIIx
        // "Positivfilm" ΔC* -4 against the camera JPEG). With such a look both are left out, as in the fit.
        // Film profiles are not fitted on graineo's development: unchanged.
        // a camera profile is the colour of the development: no per-camera saturation or hue table on top
        let mut camera_look = profile.is_some() || (look.is_some() && doc.look_name(s).is_some_and(|n| !n.starts_with("film:")));
        let mut look_amount = s.profile_amount.clamp(0.0, 1.0);
        // A learned look (`crate::learn`) is the standard of its camera and style: it stands in for the hue table and
        // the saturation correction (it was fitted on the development without them), so they never act twice; its
        // strength is its own. Only for a neutral photo without film; a chosen look or film wins.
        if look.is_none() && !s.film.is_active() && !linear {
            if let Some((l, a)) = doc.learned_look() {
                look = Some(l);
                look_amount = a.clamp(0.0, 1.0);
                camera_look = true;
            }
        }
        let plain = doc.plain.load(std::sync::atomic::Ordering::Relaxed) || linear;
        // per-camera saturation of the standard development (only for RAWs; finished images skip the curve)
        let saturation = if doc.finished || doc.apple.is_some() || camera_look || plain { 1.0 } else { raw::development::camera_saturation(&doc.info.maker, &doc.info.model) };
        let tone = Sigmoid { contrast: 1.8 * (0.55 * c).exp2(), saturation, ..raw::development::default_tone() };
        let std_look = if camera_look || plain { None } else { doc.std_look.clone() };
        let curve = ToneCurve::new(tone);
        // finished images have no sigmoid (no white to roll towards)
        let roll_reach = if doc.finished { 0.0 } else { raw::tone::whites_roll_reach(r.whites.clamp(-1.0, 1.0)) };
        let roll_white = if roll_reach > 0.0 && !linear { curve.scene_white(raw::tone::ROLL_WHITE_LEVEL) } else { 1.0 };
        Params {
            m,
            lum: ws_xyz[1],
            ws_srgb,
            curve,
            finished: (doc.finished || linear).then_some(c),
            shadows: s.shadow.clamp(-1.0, 1.0) as f32,
            highlights: s.highlight.clamp(-1.0, 1.0) as f32,
            whites: r.whites.clamp(-1.0, 1.0),
            blacks: r.blacks.clamp(-1.0, 1.0),
            roll_reach,
            roll_white,
            dcp,
            ev,
            look,
            look_amount,
            std_look,
            saturation: s.saturation.clamp(-1.0, 1.0),
            vibrance: s.vibrance.clamp(-1.0, 1.0),
            mono: s.monochrome,
            gray_mix: s.gray_mix,
            shadow_tint: r.calibration[6].clamp(-1.0, 1.0) * crate::calibration::shadow_ev(),
            brake: s.noise_brake.exponents().filter(|_| brake_active(s)).map(|(le, ce)| crate::gpu::GpuBrake { exposure: s.exposure as f32, luma_exp: le, chroma_exp: ce }),
            tone_eq: develop::tone_eq::needed(s),
        }
    }

    /// Everything for the GPU output stage ([`crate::gpu::full`]); `camera_lum` = `RawDoc::lum()` (luminance map).
    /// None: this slider state needs features only the CPU has (finished image without sigmoid).
    pub fn gpu_data(&self, camera_lum: [f32; 3]) -> Option<crate::gpu::OutputData> {
        // the black & white mixer only exists in the CPU stage
        if self.finished.is_some() || self.dcp.is_some() || self.shadow_tint != 0.0 || self.tone_eq || (self.mono && self.gray_mix.iter().any(|v| *v != 0.0)) {
            return None;
        }
        let flatten_table = |t: &[[f32; 3]]| -> Vec<f32> { t.iter().flatten().copied().collect() };
        let tone = self.curve.parameter();
        Some(crate::gpu::OutputData {
            m: self.m,
            lum: self.lum,
            ws_srgb: self.ws_srgb,
            camera_lum,
            tone: self.curve.table().to_vec(),
            hue: tone.hue,
            curve_saturation: tone.saturation,
            ev: self.ev,
            shadows: self.shadows,
            highlights: self.highlights,
            whites: self.whites,
            blacks: self.blacks,
            roll_reach: self.roll_reach,
            roll_white: self.roll_white,
            saturation: self.saturation,
            vibrance: self.vibrance,
            mono: self.mono,
            std_look: self.std_look.as_ref().map(|l| flatten_table(l.table().1)),
            look: self.look.as_ref().map(|l| {
                let (n, d) = l.table();
                (n, flatten_table(d))
            }),
            look_amount: self.look_amount,
            look_tetrahedral: self.look.as_ref().is_some_and(|l| l.tetrahedral()),
            haze: None,
            brake: self.brake,
            scene: false,
        })
    }

    /// Exposure correction (EV) from shadows/highlights (map `m`: log2 luminance of the surroundings before
    /// exposure) and whites/blacks (`x`: log2 luminance of the pixel relative to middle gray).
    #[inline]
    fn equalizer(&self, x: f32, m: f32) -> f32 {
        let mut ev = 0.0;
        if self.shadows != 0.0 || self.highlights != 0.0 {
            // surroundings relative to middle gray (exposure included)
            let u = m + self.ev - (raw::tone::MID_GRAY).log2();
            let ws = smoothstep(-9.0, -5.0, u) * (1.0 - smoothstep(-2.5, 0.5, u));
            let wh = smoothstep(-0.8, 1.6, u);
            ev += 1.6 * self.shadows * ws + 1.5 * self.highlights * wh;
        }
        if self.whites != 0.0 {
            ev += 1.1 * self.whites * smoothstep(0.3, 2.8, x);
        }
        if self.blacks != 0.0 {
            ev += blacks_ev(self.blacks, x);
        }
        ev
    }

    /// Brightening of pixel `p` with surroundings `m` in EV that the noise brake acts on: positive exposure plus the
    /// positive part of the shadows/whites/blacks equalizer.
    fn brightening_ev(&self, p: [f32; 3], m: f32, exposure: f32) -> f32 {
        let v = color::mulv(&self.m, p);
        let y = (self.lum[0] * v[0] + self.lum[1] * v[1] + self.lum[2] * v[2]).max(1e-9);
        let x = (y / raw::tone::MID_GRAY).log2();
        exposure.max(0.0) + self.equalizer(x, m).max(0.0)
    }

    /// Scene-linear pixel in the working space (after exposure and equalizer).
    #[inline]
    fn scene(&self, p: [f32; 3], m: f32) -> [f32; 3] {
        let mut v = color::mulv(&self.m, p);
        if let Some(d) = &self.dcp {
            v = d.apply(v);
        }
        let y = (self.lum[0] * v[0] + self.lum[1] * v[1] + self.lum[2] * v[2]).max(1e-9);
        let x = (y / raw::tone::MID_GRAY).log2();
        let k = self.equalizer(x, m);
        let mut v = if k == 0.0 {
            v
        } else {
            let g = k.exp2();
            [v[0] * g, v[1] * g, v[2] * g]
        };
        // Whites down: also bring lights over white back under it (one gain, hue kept)
        if self.roll_reach > 0.0 {
            v = raw::tone::whites_roll(v, self.roll_reach, self.roll_white);
        }
        if self.shadow_tint != 0.0 {
            // shadow tint: lower (+) or raise (−) green in the shadows (below about 2 EV under middle gray)
            v[1] *= (-self.shadow_tint * (1.0 - smoothstep(-4.0, 0.0, x))).exp2();
        }
        v
    }

    /// Gray value with the black & white mixer: luminance is raised or lowered depending on the pixel's hue (weights of the eight color ranges),
    /// no effect on gray tones. `y` = luminance, `s` = linear RGB before the conversion.
    #[inline]
    fn gray(&self, s: [f32; 3], y: f32) -> f32 {
        if self.gray_mix.iter().all(|v| *v == 0.0) {
            return y;
        }
        let e = [s[0].max(0.0), s[1].max(0.0), s[2].max(0.0)];
        let (h, sat, _) = develop::look::rgb_to_hsv(e);
        if sat < 0.01 {
            return y;
        }
        let w = develop::look::hsl_weights(h);
        let dl: f32 = (0..8).map(|i| w[i] * self.gray_mix[i]).sum();
        y * (1.0 + dl * GRAY_MIXER_FACTOR * (sat * 2.5).min(1.0))
    }

    pub(crate) fn pixel_extended(&self, p: [f32; 3], m: f32) -> [f32; 3] {
        // A camera or film look is a table on the sRGB cube (its result lies in 0 … 1): bring the colors
        // into sRGB exactly as the export does (desaturate towards the luminance, then clip) and only then
        // apply it. Otherwise the display saw extended values the table does not cover – a black & white
        // look kept saturated stage light visibly colored – and display and export differed.
        if self.look.is_some() {
            return self.pixel(p, m);
        }
        let v = self.scene(p, m);
        let t = match self.finished {
            None => self.curve.pixel(v, self.lum),
            Some(c) => contrast_finished(v, c, self.lum),
        };
        let mut s = color::mulv(&self.ws_srgb, t);
        if let Some(l) = &self.std_look {
            s = l.apply(s);
        }
        if self.saturation != 0.0 || self.vibrance != 0.0 {
            s = presence(s, self.saturation, self.vibrance);
        }
        let y=0.2126*s[0]+0.7152*s[1]+0.0722*s[2];
        if self.mono{s=[self.gray(s,y);3];}
        let mut e=s.map(graineo_color::srgb_encode);
        if let Some(l)=&self.look{let o=l.pixel(e);let a=self.look_amount;e=std::array::from_fn(|c|e[c]+a*(o[c]-e[c]));}
        e
    }

    fn pixel(&self, p: [f32; 3], m: f32) -> [f32; 3] {
        let v = self.scene(p, m);
        let t = match self.finished {
            None => self.curve.pixel(v, self.lum),
            Some(c) => contrast_finished(v, c, self.lum),
        };
        let mut s = color::mulv(&self.ws_srgb, t);
        if let Some(l) = &self.std_look {
            s = l.apply(s);
        }
        if self.saturation != 0.0 || self.vibrance != 0.0 {
            s = presence(s, self.saturation, self.vibrance);
        }
        // gamut: desaturate negative components towards the luminance
        let y = 0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2];
        let mn = s[0].min(s[1]).min(s[2]);
        if mn < 0.0 && y > 0.0 {
            let f = y / (y - mn);
            s = [y + f * (s[0] - y), y + f * (s[1] - y), y + f * (s[2] - y)];
        }
        if self.mono {
            s = [self.gray(s, y).max(0.0); 3];
        }
        let mut e = [encode(s[0]), encode(s[1]), encode(s[2])];
        if let Some(l) = &self.look {
            let o = l.pixel(e);
            let a = self.look_amount;
            e = [e[0] + a * (o[0] - e[0]), e[1] + a * (o[1] - e[1]), e[2] + a * (o[2] - e[2])];
        }
        e
    }
}

/// Camera profile (DCP) at `path`, read once per process and kept (profiles are small; the file is only read).
pub fn camera_profile(path: &str) -> Option<Arc<raw::dcp::Profile>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Option<Arc<raw::dcp::Profile>>>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(p) = cache.lock().ok()?.get(path) {
        return p.clone();
    }
    let p = raw::dcp::Profile::load(std::path::Path::new(path)).map(Arc::new);
    if p.is_none() {
        eprintln!("camera profile not readable: {path}");
    }
    cache.lock().ok()?.insert(path.to_string(), p.clone());
    p
}

/// Contrast for finished images: slope in log space around middle gray, softly
/// limited towards white (hue is kept: all channels with the same factor).
#[inline]
fn contrast_finished(v: [f32; 3], c: f32, lum: [f32; 3]) -> [f32; 3] {
    if c == 0.0 {
        return v;
    }
    let y = (lum[0] * v[0] + lum[1] * v[1] + lum[2] * v[2]).max(1e-6);
    let g = raw::tone::MID_GRAY;
    let k = 1.0 + 0.6 * c;
    let mut y2 = g * (y / g).powf(k);
    // do not steepen beyond 1
    if y2 > 1.0 && y <= 1.0 {
        y2 = 1.0 - (1.0 - y).max(0.0) * 0.5;
    }
    let f = y2 / y;
    [v[0] * f, v[1] * f, v[2] * f]
}

/// Saturation (uniform) and vibrance (stronger for muted colors) as in [`develop`] (linear, around
/// the Rec. 709 luminance).
#[inline]
fn presence(rgb: [f32; 3], saturation: f32, vibrance: f32) -> [f32; 3] {
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    let (mx, mn) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    let current = if mx > 1e-6 { ((mx - mn) / mx).clamp(0.0, 1.0) } else { 0.0 };
    let v = if vibrance > 0.0 { vibrance * (1.0 - current).powi(2) } else { vibrance * current };
    let k = ((1.0 + saturation) * (1.0 + v)).max(0.0);
    [y + (rgb[0] - y) * k, y + (rgb[1] - y) * k, y + (rgb[2] - y) * k]
}

/// Output without look stage: `px` (w×h) with luminance map `mask` (same size).
pub fn output(doc: &RawDoc, px: &[[f32; 3]], w: usize, h: usize, mask: &[f32], s: &Settings, geo: Geo) -> Rgb16 {
    let p = Params::new(doc, s);
    if s.film.is_active() {
        let scene = scene_linear(&p, px, w, h, mask);
        match develop::film::develop(&scene, &s.film, geo) {
            Ok(img) => return img,
            Err(e) => eprintln!("film simulation: {e}"),
        }
    }
    let _t = crate::timings::Stage::new(if w * h > 4_000_000 { "CPU Ausgabe (Ton/Farbe)" } else { "CPU Ausgabe (Vorschau)" });
    let mut data = vec![0u16; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let src = &px[y * w..(y + 1) * w];
        let mk = &mask[y * w..(y + 1) * w];
        for (x, o) in row.chunks_exact_mut(3).enumerate() {
            let e = p.pixel(src[x], mk[x]);
            for c in 0..3 {
                o[c] = (e[c].clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            }
        }
    });
    Rgb16 { width: w, height: h, data }
}

/// Scene-linear sRGB (middle gray 0.184) for the film simulation.
pub fn scene_linear(p: &Params, px: &[[f32; 3]], w: usize, h: usize, mask: &[f32]) -> Linear {
    let mut data = vec![0f32; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for (x, o) in row.chunks_exact_mut(3).enumerate() {
            let i = y * w + x;
            let v = p.scene(px[i], mask[i]);
            let s = color::mulv(&p.ws_srgb, v);
            // middle gray of the sigmoid curve (0.1845) ≈ 0.184 of the film simulation
            o.copy_from_slice(&[s[0].max(0.0), s[1].max(0.0), s[2].max(0.0)]);
        }
    });
    Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None }
}

/// With film intensity (blend against the output without film), then texture and look stage.
/// Do dehaze or the noise brake need the CPU preparation step ([`prepare`])?
pub fn needs_prepare(s: &Settings) -> bool {
    s.dehaze != 0.0 || brake_active(s) || develop::tone_eq::needed(s)
}

/// Is the noise brake effective for these settings (any brightening by exposure, shadows or whites)?
pub fn brake_active(s: &Settings) -> bool {
    s.noise_brake.exponents().is_some() && (s.exposure > 0.0 || s.shadow > 0.0 || s.raw.whites > 0.0)
}

/// Dehaze and noise brake on the linear, white-balanced camera values before the output stage.
/// `haze`: analysis of the whole image (cropped for regions) so that crops match the full view; `None` → computed
/// for `px` (whole image). The noise brake lifts the edge-preserving base layer with the full brightening of the
/// pixel but colour detail not at all and luminance detail only partly (see `develop::lift::Brake`).
/// `base`: cached brake base layer of `px` (computed here if `None`).
/// Then the tone equalizer: exposure per brightness zone (`toneq`: mask of the whole image, resampled to `px`; `None` →
/// computed for `px` as a whole image), applied as a factor on all three channels before the output stage, i.e. right
/// after exposure in the scene-linear chain.
#[allow(clippy::too_many_arguments)]
pub fn prepare(doc: &RawDoc, px: &[[f32; 3]], w: usize, h: usize, mask: &[f32], s: &Settings, haze: Option<&develop::dehaze::HazeField>, toneq: Option<&develop::tone_eq::Mask>, base: Option<Vec<f32>>) -> Option<Vec<[f32; 3]>> {
    if !needs_prepare(s) {
        return None;
    }
    let mut out: Vec<[f32; 3]> = px.to_vec();
    let mut base = base;
    if s.dehaze != 0.0 {
        let own;
        let field = match haze {
            Some(f) => f,
            None => {
                let flat: Vec<f32> = px.iter().flatten().copied().collect();
                own = develop::dehaze::HazeField::analyze(&flat, w, h, 1.0);
                &own
            }
        };
        if let Some(dz) = field.at(s.dehaze) {
            out.par_iter_mut().enumerate().for_each(|(k, p)| *p = dz.pixel(k, *p));
            // the cached base layer follows the same per-pixel affine dehaze step
            if let Some(b) = base.as_mut() {
                b.par_chunks_mut(3).enumerate().for_each(|(k, q)| {
                    let v = dz.pixel(k, [q[0], q[1], q[2]]);
                    q.copy_from_slice(&v);
                });
            }
        }
    }
    if let Some((le, ce)) = s.noise_brake.exponents().filter(|_| brake_active(s)) {
        let p = Params::new(doc, s);
        let brake = match base {
            Some(b) if b.len() == w * h * 3 => develop::lift::Brake::from_base(b, le, ce),
            _ => {
                let flat: Vec<f32> = out.iter().flatten().copied().collect();
                develop::lift::Brake::new(&flat, w, h, le, ce)
            }
        };
        let exposure = s.exposure as f32;
        out.par_iter_mut().enumerate().for_each(|(k, v)| {
            let g = p.brightening_ev(*v, mask[k], exposure).exp2();
            if g > 1.0 {
                // pre-shrink the detail so that the following uniform gain g amplifies it only by g^exp
                let q = brake.apply(*v, brake.base(k), g);
                *v = q.map(|c| c / g);
            }
        });
    }
    if let Some(m) = toneq_mask(doc, &out, w, h, s, toneq) {
        let t = develop::tone_eq::Apply::new(&s.tone_eq, &m, doc.base_ev_for(s) + s.exposure as f32, false);
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let g = t.gain(x, y, *v);
                *v = v.map(|c| c * g);
            }
        });
    }
    Some(out)
}

/// Tone equalizer mask for `px` (`w × h`): the one handed in, else computed for `px` as a whole image. `None` when the
/// settings do not need it.
fn toneq_mask(doc: &RawDoc, px: &[[f32; 3]], w: usize, h: usize, s: &Settings, toneq: Option<&develop::tone_eq::Mask>) -> Option<develop::tone_eq::Mask> {
    let _ = doc;
    if !develop::tone_eq::needed(s) {
        return None;
    }
    let flat;
    let data: &[f32] = match toneq {
        Some(m) if m.grid().fits(&s.tone_eq) && (m.width, m.height) == (w, h) => &[],
        _ => {
            flat = px.iter().flatten().copied().collect::<Vec<f32>>();
            &flat
        }
    };
    Some(develop::tone_eq::mask_for(toneq, data, w, h, 1.0, &s.tone_eq))
}

/// "Show mask" of the tone equalizer: the zones as grey steps (black = −8 EV and below, white = 0 EV), without output
/// and look stage.
fn toneq_mask_image(doc: &RawDoc, px: &[[f32; 3]], w: usize, h: usize, s: &Settings, toneq: Option<&develop::tone_eq::Mask>) -> Option<Rgb16> {
    if !s.tone_eq_mask {
        return None;
    }
    let m = toneq_mask(doc, px, w, h, s, toneq)?;
    let t = develop::tone_eq::Apply::new(&s.tone_eq, &m, doc.base_ev_for(s) + s.exposure as f32, true);
    let mut data = vec![0u16; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let v = (t.mask_grey(x, y, px[y * w + x]) * 65535.0 + 0.5) as u16;
            row[x * 3..x * 3 + 3].fill(v);
        }
    });
    Some(Rgb16 { width: w, height: h, data })
}

pub fn render_finished(doc: &RawDoc, px: &[[f32; 3]], w: usize, h: usize, mask: &[f32], s: &Settings, geo: Geo) -> Rgb16 {
    render_finished_haze(doc, px, w, h, mask, s, geo, None, None, None)
}

/// Like [`render_finished`] with the dehaze analysis of the whole image (cropped for regions).
#[allow(clippy::too_many_arguments)]
pub fn render_finished_haze(doc: &RawDoc, px: &[[f32; 3]], w: usize, h: usize, mask: &[f32], s: &Settings, geo: Geo, haze: Option<&develop::dehaze::HazeField>, toneq: Option<&develop::tone_eq::Mask>, base: Option<Vec<f32>>) -> Rgb16 {
    if let Some(img) = toneq_mask_image(doc, px, w, h, s, toneq) {
        return img;
    }
    let prepared = prepare(doc, px, w, h, mask, s, haze, toneq, base);
    let px: &[[f32; 3]] = prepared.as_deref().unwrap_or(px);
    let log = std::env::var_os("GRAINEO_LOG").is_some() && w * h > 4_000_000;
    let t = std::time::Instant::now();
    let mut img = output(doc, px, w, h, mask, s, geo);
    if log {
        eprintln!("render {w}×{h}: output {:.0?}", t.elapsed());
    }
    if s.film.is_active() && s.profile_amount < 1.0 {
        let mut n = s.clone();
        n.film.active = false;
        n.profile_amount = 1.0;
        let base = output(doc, px, w, h, mask, &n, geo);
        develop::profile::blend(&mut img, &base, s.profile_amount.clamp(0.0, 1.0));
    }
    // X-Trans: false colors at fine edges (full resolution only; the preview bins the pixels)
    if doc.info.sensor == raw::Sensor::XTrans && s.raw.false_color > 0.0 && full_resolution(geo) {
        crate::false_color::suppress(&mut img, s.raw.false_color);
    }
    if s.raw.texture != 0.0 {
        structure(&mut img, s.raw.texture, geo.scale);
    }
    let t = std::time::Instant::now();
    develop::look::apply(&mut img, s, geo);
    crate::timings::lap("Look-Stufe (CPU)", t.elapsed());
    if log {
        eprintln!("render {w}×{h}: look stage {:.0?}", t.elapsed());
    }
    img
}

/// Output pixel = image pixel (full resolution or a crop of it): not the downscaled preview (scale > 1), whose binned pixels
/// have no demosaicing false colors, and not an enlarged crop (scale < 1).
pub fn full_resolution(geo: Geo) -> bool {
    (0.999..=1.001).contains(&geo.scale)
}

/// Texture (imported from Lightroom's "Texture"): boost or smooth fine detail around radius ≈ 3 image pixels,
/// on the luminance (channels in proportion), edges are not inflated (limiting).
pub fn structure(img: &mut Rgb16, amount: f32, scale: f32) {
    let (w, h) = (img.width, img.height);
    if w < 8 || h < 8 {
        return;
    }
    let l: Vec<f32> = img.data.par_chunks(3).map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 65535.0).collect();
    let r = ((2.5 / scale.max(1.0)).round() as usize).max(1);
    let b = box_blur(&box_blur(&l, w, h, r), w, h, r);
    let k = amount.clamp(-1.0, 1.0) * if amount > 0.0 { 1.2 } else { 0.8 };
    img.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = y * w + x;
            let li = l[i];
            if li <= 1e-5 {
                continue;
            }
            let d = (li - b[i]).clamp(-0.08, 0.08);
            let t = (li + k * d).max(0.0);
            let f = (t / li).clamp(0.0, 3.0);
            for c in 0..3 {
                let v = &mut row[x * 3 + c];
                *v = (*v as f32 * f).clamp(0.0, 65535.0) as u16;
            }
        }
    });
}

/// Box filter (horizontal + vertical), radius `r`.
pub fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        let s = &src[y * w..(y + 1) * w];
        let mut acc = 0f32;
        let n = (2 * r + 1) as f32;
        for i in 0..=2 * r {
            acc += s[i.saturating_sub(r).min(w - 1)];
        }
        for x in 0..w {
            o[x] = acc / n;
            let add = s[(x + r + 1).min(w - 1)];
            let sub = s[x.saturating_sub(r)];
            acc += add - sub;
        }
    });
    let mut out = vec![0f32; w * h];
    // vertical: column-wise over row blocks
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        let n = (b - a + 1) as f32;
        for yy in a..=b {
            let s = &tmp[yy * w..(yy + 1) * w];
            for (v, &t) in o.iter_mut().zip(s) {
                *v += t;
            }
        }
        for v in o.iter_mut() {
            *v /= n;
        }
    });
    out
}

/// The blacks slider's curve, shared with the local corrections (same numbers for global and masked blacks).
pub use masks::{blacks_ev, BLACKS_OFFSET};

#[cfg(test)]
mod blacks_tests {
    /// The masks crate has no `raw` dependency: its middle gray must stay the same.
    #[test]
    fn middle_gray_shared() {
        assert_eq!(masks::MID_GRAY, raw::tone::MID_GRAY);
    }
}
