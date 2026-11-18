//! Neural denoising as an optional stage of the RAW development (`RawAdjust::ai_denoise`, off by default).
//!
//! Where it sits: after the demosaic at full resolution and before the classic noise reduction, sharpening and lens
//! correction ([`crate::doc::RawDoc::full_base_for`]). The network (RawNIND UtNet2, linear variant, see
//! `raw::ai_denoise`) sees the white-balanced stage converted to linear Rec.2020 and gives it back in the stage's space.
//!
//! Computed **once per photo at full resolution** and kept with the document ("compute the whole image once"): the
//! strength is a mix with the original (cheap, no recomputation), the preview is the downscaled full-resolution result,
//! zoom and export use the same stage, so preview and export agree. While it computes, the preview shows the photo
//! without it and [`status`] reports progress and the expected time; the app renders again when [`generation`]
//! changes. The classic noise reduction after it works on the residual noise (profile scaled by `1 − strength`, at
//! least a quarter, see [`residual`]).
//!
//! Only for RAWs graineo decodes itself (Bayer, X-Trans, linear DNG), never for finished images, the system RAW engine
//! or Sigma's Foveon path. The weights are a download module (`library::module::AI_DENOISE`), never part of graineo.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use develop::Settings;

use crate::doc::{RawDoc, Stage};

/// Mix of 0 … 100 % in whole percent (the cache key of the mixed stages).
pub fn percent(s: &Settings) -> u32 {
    (s.raw.ai_denoise.clamp(0.0, 1.0) * 100.0).round() as u32
}

/// Strength the switch sets when it is turned on.
pub const DEFAULT_STRENGTH: f32 = 1.0;

/// Is the network installed (Settings → Modules)? Looked up at most every two seconds (asked on every render).
pub fn module_ready() -> bool {
    static LAST: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = *g {
        if t.elapsed().as_secs_f32() < 2.0 {
            return v;
        }
    }
    let v = library::module::status(&library::module::AI_DENOISE).0 == library::module::Status::Installed;
    *g = Some((Instant::now(), v));
    v
}

/// The stage's state kept with one document.
#[derive(Default)]
pub(crate) struct Caches {
    /// denoised full resolution at full strength (no residual noise profile)
    full: Mutex<Option<Arc<Stage>>>,
    /// held while it is computed
    busy: Mutex<()>,
    /// a background computation was started ([`prepare`])
    started: AtomicBool,
    /// last mix at full resolution (percent, stage)
    mix: Mutex<Option<(u32, Arc<Stage>)>>,
    /// last preview of a mix (percent, stage)
    preview: Mutex<Option<(u32, Arc<Stage>)>>,
    /// counts [`Caches::release`]: a computation started before a release stops at its next tile
    epoch: AtomicU64,
}

impl Caches {
    pub(crate) fn ready(&self) -> bool {
        self.full.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Switching photos: give the memory back (the next visit computes again).
    pub(crate) fn release(&self) {
        *self.full.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.mix.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.preview.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.started.store(false, Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }
}

/// Can this document use the stage at all (a RAW decoded by graineo)?
pub fn applicable(doc: &RawDoc) -> bool {
    !doc.finished && doc.apple.is_none() && matches!(doc.info.sensor, raw::Sensor::Bayer | raw::Sensor::XTrans | raw::Sensor::Linear)
}

/// The loaded network, shared by all documents (≈ 31 MB of weights).
fn net() -> Result<Arc<raw::ai_denoise::Net>, String> {
    static NET: OnceLock<Mutex<Option<Arc<raw::ai_denoise::Net>>>> = OnceLock::new();
    let m = NET.get_or_init(Default::default);
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = g.as_ref() {
        return Ok(n.clone());
    }
    if !module_ready() {
        return Err("module ai-denoise not installed".into());
    }
    let path = library::module::folder(&library::module::AI_DENOISE).join(library::module::AI_DENOISE_MODEL);
    let n = Arc::new(raw::ai_denoise::Net::load(&path)?);
    *g = Some(n.clone());
    Ok(n)
}

/// Threads of the computation: the matrix products are bound by the matrix units, more threads only add heat
/// (measured on an M1 Pro: 4 threads as fast as 10).
fn pool() -> &'static rayon::ThreadPool {
    static P: OnceLock<rayon::ThreadPool> = OnceLock::new();
    P.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(4).thread_name(|i| format!("ai-denoise-{i}")).build().expect("thread pool"))
}

/// Progress of the computation running now (one at a time).
#[derive(Clone, Debug)]
pub struct Status {
    pub path: std::path::PathBuf,
    /// tiles done and in all
    pub done: usize,
    pub total: usize,
    pub started: Instant,
    /// when the tile now running began
    pub tile_started: Instant,
    /// seconds per tile: measured in this computation, before its first tile from the last one (`None`: unknown)
    pub per_tile: Option<f32>,
    id: u64,
}

static ID: AtomicU64 = AtomicU64::new(1);

impl Status {
    /// Share done, 0 … 1: the finished tiles plus the expected part of the running one (so the display keeps moving
    /// during a long tile; never ahead of the next tile).
    pub fn fraction(&self) -> f32 {
        let total = self.total.max(1) as f32;
        let running = match self.per_tile {
            Some(t) if t > 0.0 && self.done < self.total => (self.tile_started.elapsed().as_secs_f32() / t).min(0.95),
            _ => 0.0,
        };
        ((self.done as f32 + running) / total).clamp(0.0, 1.0)
    }

    /// Seconds still to go (`None` while nothing is known about the speed).
    pub fn remaining_secs(&self) -> Option<f32> {
        let t = self.per_tile?;
        Some(((1.0 - self.fraction()) * self.total.max(1) as f32 * t).max(0.0))
    }
}

/// A computation that ended: photo, seconds, whether it finished (`false`: cancelled or failed).
#[derive(Clone, Debug, PartialEq)]
pub struct Finished {
    pub path: std::path::PathBuf,
    pub seconds: f32,
    pub complete: bool,
    /// counts up with every computation that ended
    pub seq: u64,
    /// when it ended
    pub at: Instant,
}

static FINISHED: Mutex<Option<Finished>> = Mutex::new(None);
/// Seconds per tile of the last computation (estimate before the first tile of the next one).
static LAST_PER_TILE: Mutex<Option<f32>> = Mutex::new(None);

/// The computation that ended last.
pub fn last_finished() -> Option<Finished> {
    FINISHED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The computations running now (usually one; a Sigma base and a RAW of another photo can overlap).
static STATUS: Mutex<Vec<Status>> = Mutex::new(Vec::new());
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Last error per path (shown in the panel instead of a silent "off").
static ERROR: Mutex<Option<(std::path::PathBuf, String)>> = Mutex::new(None);

/// The computation of this photo, if one runs.
pub fn status_for(path: &std::path::Path) -> Option<Status> {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|s| s.path == path).cloned()
}

/// A running computation, that of `prefer` first (the photo on screen).
pub fn status(prefer: Option<&std::path::Path>) -> Option<Status> {
    let g = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    prefer.and_then(|p| g.iter().find(|s| s.path == p)).or_else(|| g.first()).cloned()
}

/// Counter of finished computations: when it changes, previews of photos with the stage are rendered again.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Why the stage failed for this photo (last attempt).
pub fn error_for(path: &std::path::Path) -> Option<String> {
    ERROR.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(p, _)| p == path).map(|(_, e)| e.clone())
}

/// Network input from the stage: white-balanced camera RGB → linear Rec.2020 (scene white stays white), and back.
fn matrices(doc: &RawDoc) -> Result<(raw::color::M3, raw::color::M3), String> {
    let ws = raw::color::rec2020_to_xyz_d65();
    let to = raw::color::mul(&raw::color::xyz_d50_to(&ws), &doc.cam_xyz_shot);
    let from = raw::color::inv(&to).ok_or("camera matrix not invertible")?;
    Ok((to, from))
}

/// The denoised full resolution at full strength: computed once per document (a second caller waits for the first).
fn full(doc: &RawDoc, base: &Stage) -> Result<Arc<Stage>, String> {
    let c = &doc.ai;
    if let Some(st) = c.full.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return Ok(st.clone());
    }
    let _busy = c.busy.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(st) = c.full.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return Ok(st.clone());
    }
    let r = compute(doc, base);
    // a cancelled computation (photo switched) is no error: the next visit computes again
    if let Err(e) = r.as_ref() {
        if e != "cancelled" {
            eprintln!("ai denoise {}: {e}", doc.path.display());
            *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some((doc.path.clone(), e.clone()));
        }
    }
    let st = match r {
        Ok(st) => Arc::new(st),
        Err(e) => {
            GENERATION.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
    };
    *c.full.lock().unwrap_or_else(|e| e.into_inner()) = Some(st.clone());
    GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(st)
}

/// Full resolution of the raw stage with `p` percent of the denoised image (computes it when needed). A failure (model
/// damaged, out of memory) falls back to the stage without it; [`error_for`] says why.
pub(crate) fn base(doc: &RawDoc, p: u32) -> Result<Arc<Stage>, String> {
    let orig = doc.full_base()?;
    let den = match full(doc, &orig) {
        Ok(d) => d,
        Err(_) => return Ok(orig),
    };
    if p >= 100 && residual(p) == 0.0 {
        return Ok(den);
    }
    let mut g = doc.ai.mix.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, st)) = g.as_ref().filter(|(k, _)| *k == p) {
        let _ = k;
        return Ok(st.clone());
    }
    *g = None;
    let st = Arc::new(mix(&orig, &den, p, orig.profile.as_ref()));
    *g = Some((p, st.clone()));
    Ok(st)
}

/// Preview of the mix: the full-resolution result downscaled to the preview size (so preview and export agree), with
/// the preview's noise profile scaled to the residual noise. `None` while the full resolution is not computed.
pub(crate) fn preview(doc: &RawDoc, p: u32) -> Option<Arc<Stage>> {
    if !doc.ai.ready() {
        return None;
    }
    if let Some((_, st)) = doc.ai.preview.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(k, _)| *k == p) {
        return Some(st.clone());
    }
    let full = base(doc, p).ok()?;
    let pv = &doc.preview;
    let px = raw::development::downscale(&full.px, full.w, full.h, pv.w, pv.h);
    let k = residual(p);
    let profile = pv.profile.as_ref().filter(|_| k >= 0.05).map(|pr| pr.scaled(k));
    let st = Arc::new(Stage::new(px, pv.w, pv.h, profile));
    *doc.ai.preview.lock().unwrap_or_else(|e| e.into_inner()) = Some((p, st.clone()));
    Some(st)
}

fn compute(doc: &RawDoc, base: &Stage) -> Result<Stage, String> {
    let (to, from) = matrices(doc)?;
    let epoch = doc.ai.epoch.load(Ordering::Relaxed);
    let keep_going = || doc.ai.epoch.load(Ordering::Relaxed) == epoch;
    let t = Instant::now();
    let px = run(&doc.path, &base.px, base.w, base.h, &to, &from, &keep_going)?;
    if std::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("ai denoise {} ({}×{}, ISO {:?}): {:.1} s", doc.path.display(), base.w, base.h, doc.info.exif.iso, t.elapsed().as_secs_f32());
    }
    crate::timings::lap("CPU AI denoise", t.elapsed());
    // at full strength no noise is left for the classic noise reduction
    Ok(Stage::new(px, base.w, base.h, None))
}

/// The network over a whole image with the progress shown in [`status`] (one computation at a time).
fn run(path: &std::path::Path, px: &[[f32; 3]], w: usize, h: usize, to: &raw::color::M3, from: &raw::color::M3, keep_going: &(dyn Fn() -> bool + Sync)) -> Result<Vec<[f32; 3]>, String> {
    let net = net()?;
    let started = Instant::now();
    let per_tile = *LAST_PER_TILE.lock().unwrap_or_else(|e| e.into_inner());
    let id = ID.fetch_add(1, Ordering::Relaxed);
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).push(Status { path: path.to_path_buf(), done: 0, total: 1, started, tile_started: started, per_tile, id });
    let progress = |done: usize, total: usize| {
        if let Some(s) = STATUS.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|s| s.id == id) {
            s.done = done;
            s.total = total;
            s.tile_started = Instant::now();
            if done > 0 {
                s.per_tile = Some(s.started.elapsed().as_secs_f32() / done as f32);
            }
        }
    };
    let r = pool().install(|| raw::ai_denoise::denoise(&net, px, w, h, to, from, &progress, keep_going));
    let last = {
        let mut g = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        let i = g.iter().position(|s| s.id == id);
        i.map(|i| g.remove(i))
    };
    let complete = matches!(r, Ok(Some(_)));
    if std::env::var_os("GRAINEO_LOG").is_some() && !complete {
        eprintln!("ai denoise {}: stopped after {:.1} s ({})", path.display(), started.elapsed().as_secs_f32(), if r.is_ok() { "cancelled" } else { "failed" });
    }
    if complete {
        if let Some(t) = last.and_then(|s| s.per_tile) {
            *LAST_PER_TILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
        }
    }
    let mut f = FINISHED.lock().unwrap_or_else(|e| e.into_inner());
    let seq = f.as_ref().map_or(1, |x| x.seq + 1);
    *f = Some(Finished { path: path.to_path_buf(), seconds: started.elapsed().as_secs_f32(), complete, seq, at: Instant::now() });
    drop(f);
    r?.ok_or_else(|| "cancelled".to_string())
}

/// Sigma photos (`develop::ai_denoise`): the network on Sigma's base after its noise reduction. The counts are linear
/// RGB with sRGB primaries after Sigma's colour matrix, white = `white`; converted to linear Rec.2020 with white = 1.
/// Bright point lights and areas near clipping keep the base ([`raw::ai_denoise::protect_highlights`]).
fn sigma(data: &mut [f32], w: usize, h: usize, white: f32, path: &std::path::Path, cancel: &AtomicBool) -> Result<(), String> {
    if data.len() != w * h * 3 {
        return Err("base size".into());
    }
    let srgb_to_2020 = raw::color::mul(&raw::color::inv(&raw::color::rec2020_to_xyz_d65()).ok_or("matrix")?, &raw::color::srgb_to_xyz_d65());
    let to = srgb_to_2020.map(|r| r.map(|v| v / white));
    // inverted before scaling (the scaled matrix's determinant is below the inversion's threshold)
    let from = raw::color::inv(&srgb_to_2020).ok_or("matrix")?.map(|r| r.map(|v| v * white));
    let px: Vec<[f32; 3]> = data.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
    let t = Instant::now();
    let keep_going = || !cancel.load(Ordering::Relaxed);
    let mut den = run(path, &px, w, h, &to, &from, &keep_going)?;
    if std::env::var("GRAINEO_AI_PROTECT").as_deref() != Ok("0") {
        raw::ai_denoise::protect_highlights(&px, &mut den, w, h, &to);
    }
    for (o, d) in data.chunks_exact_mut(3).zip(&den) {
        o.copy_from_slice(d);
    }
    if std::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("ai denoise (Sigma) {} ({w}×{h}): {:.1} s", path.display(), t.elapsed().as_secs_f32());
    }
    Ok(())
}

/// Make the network available to Sigma's development (`develop::ai_denoise`); call once at program start.
pub fn install_sigma() {
    develop::ai_denoise::install(sigma, module_ready);
}

/// Mix of the denoised and the original stage with `p` percent of the denoised one; the noise profile is scaled to the
/// remaining noise (none left at full strength).
pub(crate) fn mix(orig: &Stage, den: &Stage, p: u32, profile: Option<&raw::noise::Profile>) -> Stage {
    use rayon::prelude::*;
    let a = p.min(100) as f32 / 100.0;
    let px: Vec<[f32; 3]> = if p >= 100 {
        den.px.clone()
    } else {
        orig.px.par_iter().zip(den.px.par_iter()).map(|(o, d)| [o[0] + a * (d[0] - o[0]), o[1] + a * (d[1] - o[1]), o[2] + a * (d[2] - o[2])]).collect()
    };
    let k = residual(p);
    Stage::new(px, orig.w, orig.h, profile.filter(|_| k >= 0.05).map(|pr| pr.scaled(k)))
}

/// Noise the classic noise reduction after the stage assumes, as a share of the original: what the mix leaves, but at
/// least [`RESIDUAL_FLOOR`] – the network's result still carries some low-frequency colour mottle in smooth areas,
/// which the classic colour noise reduction removes when it sees it as a quarter of the original noise (measured:
/// +0.5 … +2.7 dB, low-frequency chroma noise down to a third, detail unchanged). `GRAINEO_AI_RESIDUAL=<k>` overrides
/// the floor (measurements; 0 = the network alone at full strength).
fn residual(p: u32) -> f32 {
    static FLOOR: OnceLock<f32> = OnceLock::new();
    let floor = *FLOOR.get_or_init(|| std::env::var("GRAINEO_AI_RESIDUAL").ok().and_then(|v| v.parse().ok()).unwrap_or(RESIDUAL_FLOOR));
    (1.0 - p.min(100) as f32 / 100.0).max(floor)
}

/// See [`residual`].
pub const RESIDUAL_FLOOR: f32 = 0.25;

/// Start the computation for this document in the background (the develop view does this for the photo on screen; a
/// thumbnail or a library preview never starts it). Nothing happens when it is done, running, or not wanted.
pub fn prepare(doc: &Arc<RawDoc>, s: &Settings) {
    if percent(s) == 0 || !applicable(doc) || !module_ready() || doc.ai.ready() || error_for(&doc.path).is_some() {
        return;
    }
    if doc.ai.started.swap(true, Ordering::Relaxed) {
        return;
    }
    let d = doc.clone();
    let spawned = std::thread::Builder::new().name("ai-denoise".into()).spawn(move || {
        if let Ok(base) = d.full_base() {
            let _ = full(&d, &base);
        }
    });
    if spawned.is_err() {
        doc.ai.started.store(false, Ordering::Relaxed);
    }
}

/// Is the denoised full resolution of this document there (preview with the stage possible)?
pub fn ready(doc: &RawDoc) -> bool {
    doc.ai.ready()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_moves_within_a_tile_and_estimates_the_rest() {
        let now = Instant::now();
        let s = Status { path: "x".into(), done: 2, total: 10, started: now - std::time::Duration::from_secs(4), tile_started: now - std::time::Duration::from_secs(1), per_tile: Some(2.0), id: 0 };
        // two tiles done, half of the third: 25 %, 7.5 tiles of 2 s to go
        assert!((s.fraction() - 0.25).abs() < 0.01, "{}", s.fraction());
        assert!((s.remaining_secs().unwrap() - 15.0).abs() < 0.3);
        // a tile that takes longer than expected never runs ahead of the next one
        let slow = Status { tile_started: now - std::time::Duration::from_secs(10), ..s.clone() };
        assert!(slow.fraction() < 0.3);
        // nothing known about the speed: no time, no movement within the tile
        let first = Status { done: 0, per_tile: None, ..s };
        assert_eq!(first.fraction(), 0.0);
        assert!(first.remaining_secs().is_none());
    }

    #[test]
    fn mix_blends_and_scales_the_noise() {
        let orig = Stage::new(vec![[1.0, 2.0, 3.0]; 4], 2, 2, None);
        let den = Stage::new(vec![[0.0, 0.0, 0.0]; 4], 2, 2, None);
        let m = mix(&orig, &den, 25, None);
        assert_eq!(m.px[0], [0.75, 1.5, 2.25]);
        let full = mix(&orig, &den, 100, None);
        assert_eq!(full.px[3], [0.0; 3]);
        let p = raw::noise::Profile { model: raw::noise::NoiseModel { a: [4e-4; 3], b: [1e-5; 3] }, sigma: [[1.0; raw::noise::LAYERS_C]; 3] };
        let half = mix(&orig, &den, 50, Some(&p));
        let q = half.profile.unwrap();
        assert!((q.model.a[0] - 1e-4).abs() < 1e-9 && (q.model.b[0] - 2.5e-6).abs() < 1e-12);
        // full strength: the classic noise reduction still sees a quarter of the original noise
        let q = mix(&orig, &den, 100, Some(&p)).profile.unwrap();
        let k2 = RESIDUAL_FLOOR * RESIDUAL_FLOOR;
        assert!((q.model.a[0] - 4e-4 * k2).abs() < 1e-9);
        assert_eq!(residual(90), RESIDUAL_FLOOR);
        assert_eq!(residual(50), 0.5);
    }
}
