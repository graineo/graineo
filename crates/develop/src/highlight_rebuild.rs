//! Rebuild of a clipped Foveon layer from the layers that are still valid at the same pixels (opt-in, see
//! [`enabled`]).
//!
//! Where one layer of a Foveon sensor saturates while another one still measures (bright coloured surfaces: the top
//! layer of a cyan label, the top layer of a red petal), the clipped layer is flat at its clip level: the structure
//! of the surface is lost and the colour drifts. This module restores the clipped layer **in the gradient domain**:
//!
//! 1. Mask `M`: pixels whose target layer lies at or above [`NEAR_CLIP`] of its clip level while at least one guide
//!    layer is still below its own. Pixels where every layer is clipped are left alone (no colour is measurable there; the existing
//!    neutral treatment handles them). This is a hard case split by the validity of the layers.
//! 2. Guide `G`: one linear combination of the valid guide layers, fitted **once for the whole image** by least
//!    squares on bright, unclipped pixels. No per-pixel or neighbourhood ratios.
//! 3. Inside `M` the target layer takes the gradients of `G` (weighted by how reliable the guide still is) and the
//!    measured values on the border of `M` (Dirichlet): the Poisson equation `Δu = div(ρ∇G)` in `M`, solved by
//!    conjugate gradients per connected area of `M` (only that area is held in memory), for large areas preconditioned
//!    by one V-cycle of an aggregation multigrid (2 × 2 blocks, Galerkin coarse graphs, damped Jacobi). [`Variant::Log`]
//!    solves the same equation for `ln u` with `ln G`. The measured value is a lower bound, and with the white balance
//!    given ([`rebuild_layer_with`]) so is grey at the level of guide layers that near their own clip.
//! 4. The rebuilt values reach above the clip level. A soft knee compresses the brightness of the rebuilt areas and
//!    a seam of [`SEAM`] pixels around them into the range up to the clip level, by one factor for all three layers
//!    (the colour of the pixel stays), so the later stages, which cap at the common white point, keep the structure.
//!    Pixels with a clipped guide layer keep their values. Pixels away from the rebuilt areas are not touched.
//!
//! The weight of each pixel in the rebuilt areas (1 inside, falling off over the seam) is returned for the sliders
//! "Lichter-Farbe" and "Lichter-Struktur", which act later on the colour grid and the top-layer detail without
//! solving again.
//!
//! Sources: P. Pérez, M. Gangnet, A. Blake, "Poisson Image Editing", ACM SIGGRAPH 2003 (guided interpolation with
//! Dirichlet border); D. Rouf, C. Lau, W. Heidrich, "Gradient domain color restoration of clipped highlights",
//! CVPR Workshops 2012 (gradients of unclipped channels guide a clipped one); M. Masood, J. Zhou, M. Tappen,
//! "Automatic correction of saturated regions in photographs using cross-channel correlation", 2009; the "guided
//! laplacians" highlight reconstruction of darktable (A. Pierre, GPL-3.0) follows the same idea. Solver:
//! P. Vaněk, J. Mandel, M. Brezina, "Algebraic multigrid by smoothed aggregation", Computing 56, 1996; Y. Notay,
//! "An aggregation-based algebraic multigrid method", ETNA 37, 2010 (plain aggregation as a preconditioner).

use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Environment switch for comparisons: `GRAINEO_FOVEON_HL=1` (or any value but `0`/`off`) turns the rebuild on,
/// `linear` with [`Variant::Linear`], `0` off, for Merrill and Quattro alike. Without it the settings decide.
pub const ENV: &str = "GRAINEO_FOVEON_HL";

/// Settings per camera class. Both start off here: the app sets them from its preferences (Merrill on, Quattro off by
/// default), while `graineo-dev` and `scripts/check.sh` never do, so the reference checks keep the path without the
/// rebuild.
static SETTING_MERRILL: AtomicBool = AtomicBool::new(false);
static SETTING_QUATTRO: AtomicBool = AtomicBool::new(false);

/// Rebuilds running right now and the start of the oldest (status line of the app while a heavily clipped photo
/// opens).
static RUNNING: std::sync::Mutex<(usize, Option<std::time::Instant>)> = std::sync::Mutex::new((0, None));

struct Running;

impl Running {
    fn start() -> Self {
        let mut r = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        r.0 += 1;
        r.1.get_or_insert_with(std::time::Instant::now);
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut r = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        r.0 = r.0.saturating_sub(1);
        if r.0 == 0 {
            r.1 = None;
        }
    }
}

/// How long a rebuild has been running (`None` when none runs).
pub fn running_for() -> Option<std::time::Duration> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).1.map(|t| t.elapsed())
}

/// Setting "Merrill: Lichter rekonstruieren" (preferences).
pub fn set_enabled_merrill(on: bool) {
    SETTING_MERRILL.store(on, Ordering::Relaxed);
}

/// Setting "Quattro: Lichter rekonstruieren" (preferences).
pub fn set_enabled_quattro(on: bool) {
    SETTING_QUATTRO.store(on, Ordering::Relaxed);
}

fn env_switch() -> Option<bool> {
    match std::env::var(ENV) {
        Ok(v) if v == "0" || v == "off" => Some(false),
        Ok(v) if !v.is_empty() => Some(true),
        _ => None,
    }
}

/// Whether the rebuild runs for Merrill photos: the environment switch wins over the setting.
pub fn enabled_merrill() -> bool {
    env_switch().unwrap_or_else(|| SETTING_MERRILL.load(Ordering::Relaxed))
}

/// Whether the rebuild runs for Quattro photos (dp, sd, sd H): the environment switch wins over the setting.
pub fn enabled_quattro() -> bool {
    env_switch().unwrap_or_else(|| SETTING_QUATTRO.load(Ordering::Relaxed))
}

/// [`enabled_quattro`] for the prepared camera classes (Quattro, Classic; Classic never rebuilds), otherwise
/// [`enabled_merrill`].
pub fn enabled_for(prepared: bool) -> bool {
    if prepared { enabled_quattro() } else { enabled_merrill() }
}

/// Mark of the switch state for the disk cache: the value of [`ENV`], or `setting`.
pub fn tag() -> String {
    std::env::var(ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "setting".into())
}

/// Solver variant ([`ENV`]): logarithms (default) or linear values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Linear,
    Log,
}

/// Variant chosen by [`ENV`] (`linear`), [`Variant::Log`] otherwise (measured better on sd Quattro H labels: the
/// small print stays legible where the linear fill flattens it).
pub fn variant() -> Variant {
    match std::env::var(ENV) {
        Ok(v) if v == "linear" => Variant::Linear,
        _ => Variant::Log,
    }
}

/// Share of the saturation level from which a value counts as clipped.
pub const CLIP_SHARE: f32 = 0.97;
/// Share of the clip level from which the target counts as clipped for the mask (step 1).
pub const NEAR_CLIP: f32 = 0.95;
/// Share of the clip level where the soft knee of step 4 starts.
pub const KNEE: f32 = 0.75;
/// Width of the seam around the rebuilt areas (pixels) over which the knee and the slider weight fade out.
pub const SEAM: usize = 6;
/// Fit samples: target between these shares of its clip level (bright, but measured).
const FIT_RANGE: (f32, f32) = (0.4, 0.9);
/// Share of a guide layer's clip level from which its gradients count less (soft reliability).
const RELIABLE_UNTIL: f32 = 0.85;
/// Conjugate-gradient tolerance (relative residual).
const CG_TOL: f64 = 1e-4;
/// Areas from this many pixels run the solver's steps in parallel.
const PARALLEL_FROM: usize = 65_536;

/// Result of [`rebuild_layer`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Stats {
    /// Pixels rebuilt (size of `M`).
    pub rebuilt: usize,
    /// Pixels where every layer is clipped (left alone).
    pub all_clipped: usize,
    /// Connected areas of `M`.
    pub areas: usize,
    /// Fitted guide weights (one per layer, 0 for the target).
    pub weights: [f32; 3],
    /// Largest number of solver iterations of an area.
    pub iterations: usize,
    /// Areas whose solver stopped at its iteration limit (their result is used as it is).
    pub unconverged: usize,
}

/// Outcome of [`rebuild_layer`]: statistics and the weight of the rebuilt areas.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub stats: Stats,
    /// Pixels of the rebuilt areas and their seam with their weight (1 in `M`, falling to 0 over [`SEAM`]), sorted
    /// by index.
    pub weight: Vec<(u32, f32)>,
}

/// Rebuilds layer `target` of `layers` (each `w × h`, any linear unit) where it is clipped and another layer is not.
/// `sat`: saturation level per layer in the units of `layers`. Returns `None` (nothing changed) when no pixel
/// qualifies or the fit has too few samples.
pub fn rebuild_layer(layers: &mut [Vec<f32>; 3], w: usize, h: usize, sat: [f32; 3], target: usize, variant: Variant) -> Option<Outcome> {
    rebuild_layer_with(layers, w, h, sat, target, variant, true, None)
}

/// [`rebuild_layer`]; `knee`: compress the rebuilt brightness into the clip level (step 4). Without it the rebuilt
/// values stay above the clip level (for stages that take values above it, Merrill's float frontend).
/// `neutral`: the layer values of a grey surface (relative); with it, a rebuilt pixel whose guide layers near their
/// own clip level does not fall below grey at the guides' level ([`neutral_floor`]).
#[allow(clippy::too_many_arguments)]
pub fn rebuild_layer_with(layers: &mut [Vec<f32>; 3], w: usize, h: usize, sat: [f32; 3], target: usize, variant: Variant, knee: bool, neutral: Option<[f32; 3]>) -> Option<Outcome> {
    let n = w * h;
    if target > 2 || w < 3 || h < 3 || layers.iter().any(|l| l.len() < n) || sat.iter().any(|s| !(*s > 0.0)) {
        return None;
    }
    let clip = sat.map(|s| s * CLIP_SHARE);
    let guides: [usize; 2] = match target {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    // 1. mask: the target at or just below its clip level (the level is a low percentile of the marked values: the
    // band just below it is clipped as well, softly; left out, the edge of a white surface stayed yellow)
    let near = NEAR_CLIP;
    let mask: Vec<bool> = (0..n)
        .into_par_iter()
        .map(|i| layers[target][i] >= near * clip[target] && guides.iter().any(|&g| layers[g][i] < clip[g]))
        .collect();
    let rebuilt = mask.par_iter().filter(|m| **m).count();
    if rebuilt == 0 {
        return None;
    }
    let _running = Running::start();
    let all_clipped = (0..n).into_par_iter().filter(|&i| layers.iter().zip(clip).all(|(l, c)| l[i] >= c)).count();
    // 2. one global fit target ≈ a·g0 + b·g1 (normal equations, 2 × 2)
    let [g0, g1] = guides;
    let sums = (0..n)
        .into_par_iter()
        .fold(
            || [0f64; 6],
            |mut s, i| {
                let t = layers[target][i];
                if t >= FIT_RANGE.0 * clip[target] && t <= FIT_RANGE.1 * clip[target] && layers[g0][i] < clip[g0] && layers[g1][i] < clip[g1] {
                    let (a, b, t) = (layers[g0][i] as f64, layers[g1][i] as f64, t as f64);
                    s[0] += a * a;
                    s[1] += a * b;
                    s[2] += b * b;
                    s[3] += a * t;
                    s[4] += b * t;
                    s[5] += 1.0;
                }
                s
            },
        )
        .reduce(|| [0f64; 6], |a, b| std::array::from_fn(|k| a[k] + b[k]));
    let [s00, s01, s11, s0t, s1t, m] = sums;
    if m < 64.0 {
        return None;
    }
    let det = s00 * s11 - s01 * s01;
    let (mut wa, mut wb) = if det.abs() > 1e-9 * (s00 * s11).max(1e-30) {
        ((s0t * s11 - s1t * s01) / det, (s1t * s00 - s0t * s01) / det)
    } else {
        (if s00 > 0.0 { s0t / s00 } else { 0.0 }, 0.0)
    };
    // a negative weight turns the gradients of that layer around: then the better single fit
    if wa < 0.0 || wb < 0.0 {
        let (ea, eb) = (if s00 > 0.0 { s0t * s0t / s00 } else { 0.0 }, if s11 > 0.0 { s1t * s1t / s11 } else { 0.0 });
        (wa, wb) = if ea >= eb { (s0t / s00, 0.0) } else { (0.0, s1t / s11) };
    }
    let single = [if s00 > 0.0 { s0t / s00 } else { 0.0 }, if s11 > 0.0 { s1t / s11 } else { 0.0 }];
    // 3. connected areas of the mask (4-neighbourhood), solved one by one
    let areas = components(&mask, w, h);
    let src: &[Vec<f32>; 3] = layers;
    let results: Vec<(Vec<(usize, f32)>, usize, bool)> = areas
        .par_iter()
        .map(|a| solve_area(src, w, h, &mask, a, target, guides, clip, (wa, wb), single, variant))
        .collect();
    let mut stats = Stats { rebuilt, all_clipped, areas: areas.len(), ..Default::default() };
    for (vals, it, conv) in &results {
        stats.iterations = stats.iterations.max(*it);
        stats.unconverged += usize::from(!conv);
        for &(i, v) in vals {
            // the measured value is a lower bound, and grey at the level of guide layers near their own clip
            let floor = neutral.map_or(0.0, |n| neutral_floor(layers, i, target, guides, clip, n));
            layers[target][i] = v.max(layers[target][i]).max(floor);
        }
    }
    drop(results);
    // 4. weight over the areas and their seam, and the knee of the brightness there
    let weight = seam_weight(&mask, w, h);
    let peak = weight.iter().filter(|(_, wt)| *wt >= 1.0).map(|&(i, _)| layers[target][i as usize]).fold(clip[target], f32::max);
    let k0 = KNEE * clip[target];
    if knee && peak > clip[target] {
        let span_in = peak - k0;
        let span_out = clip[target] - k0;
        for &(i, wt) in &weight {
            let i = i as usize;
            let v = layers[target][i];
            // a pixel with a clipped guide layer keeps its values: the guide is a lower bound (white detail within a
            // coloured surface stays light), and lowering only the target turned such pixels (every layer clipped:
            // specular highlights in the seam) yellow
            if v > k0 && guides.iter().all(|&g| layers[g][i] < clip[g]) {
                let out = k0 + span_out * knee_curve((v - k0) / span_in, span_in / span_out);
                // the knee as it would apply without the seam, faded over the seam; one factor for all layers
                let f = 1.0 + wt * (out / v - 1.0);
                for l in layers.iter_mut() {
                    l[i] *= f;
                }
            }
        }
    }
    let mut weights = [0f32; 3];
    weights[g0] = wa as f32;
    weights[g1] = wb as f32;
    stats.weights = weights;
    Some(Outcome { stats, weight })
}

/// Share of a guide layer's clip level from which the grey floor of [`neutral_floor`] starts, and where it is full.
const NEUTRAL_FLOOR: (f32, f32) = (0.6, 0.9);

/// Lower bound of a rebuilt value from the guide layers that are bright themselves: the target value of a grey
/// surface at the guide's level, faded in as the guide nears its own clip level. Where every layer is that bright, a
/// small specular highlight or the edge of a white surface is white: the gradient fill and the image-wide fit (taken
/// from the coloured surfaces of the photo) left the target below grey there, and the pixel turned yellow.
fn neutral_floor(l: &[Vec<f32>; 3], i: usize, target: usize, guides: [usize; 2], clip: [f32; 3], neutral: [f32; 3]) -> f32 {
    guides
        .iter()
        .filter(|&&g| neutral[g] > 0.0)
        .map(|&g| {
            let v = l[g][i].min(clip[g]);
            let t = ((v / clip[g] - NEUTRAL_FLOOR.0) / (NEUTRAL_FLOOR.1 - NEUTRAL_FLOOR.0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t) * v / neutral[g] * neutral[target]
        })
        .fold(0.0, f32::max)
}

/// A connected area of the mask: its pixels and bounding box (inclusive).
struct Area {
    pixels: Vec<usize>,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

/// Connected areas (4-neighbourhood) of `mask`, by flood fill.
fn components(mask: &[bool], w: usize, h: usize) -> Vec<Area> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for s in 0..mask.len() {
        if !mask[s] || seen[s] {
            continue;
        }
        seen[s] = true;
        stack.push(s);
        let mut a = Area { pixels: Vec::new(), x0: usize::MAX, y0: usize::MAX, x1: 0, y1: 0 };
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            a.pixels.push(i);
            a.x0 = a.x0.min(x);
            a.x1 = a.x1.max(x);
            a.y0 = a.y0.min(y);
            a.y1 = a.y1.max(y);
            for j in [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)].into_iter().flatten() {
                if mask[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        out.push(a);
    }
    out
}

/// Solves one area (step 3). Returns the new target values of its pixels, the iterations and whether the solver
/// converged.
#[allow(clippy::too_many_arguments)]
fn solve_area(
    l: &[Vec<f32>; 3],
    w: usize,
    h: usize,
    mask: &[bool],
    a: &Area,
    target: usize,
    guides: [usize; 2],
    clip: [f32; 3],
    fit: (f64, f64),
    single: [f64; 2],
    variant: Variant,
) -> (Vec<(usize, f32)>, usize, bool) {
    let [g0, g1] = guides;
    let valid = |c: usize, i: usize| l[c][i] < clip[c];
    let lift = |v: f64| match variant {
        Variant::Linear => v,
        Variant::Log => v.max(1e-6).ln(),
    };
    let guide_at = |i: usize| -> f64 {
        let v = match (valid(g0, i), valid(g1, i)) {
            (true, true) => fit.0 * l[g0][i] as f64 + fit.1 * l[g1][i] as f64,
            (true, false) => single[0] * l[g0][i] as f64,
            (false, true) => single[1] * l[g1][i] as f64,
            (false, false) => l[target][i] as f64,
        };
        lift(v)
    };
    // soft reliability of the guide: its gradients fade out as the guide layers themselves near their clip level
    let reliability = |i: usize| -> f64 {
        guides
            .iter()
            .filter(|&&g| valid(g, i))
            .map(|&g| {
                let t = ((l[g][i] / clip[g] - RELIABLE_UNTIL) / (1.0 - RELIABLE_UNTIL)).clamp(0.0, 1.0) as f64;
                1.0 - t * t * (3.0 - 2.0 * t)
            })
            .fold(0.0, f64::max)
    };
    // index of each pixel of the area in a local box with a margin of one pixel
    let (bx0, by0) = (a.x0.saturating_sub(1), a.y0.saturating_sub(1));
    let (bx1, by1) = ((a.x1 + 1).min(w - 1), (a.y1 + 1).min(h - 1));
    let bw = bx1 - bx0 + 1;
    let local = |i: usize| (i / w - by0) * bw + (i % w - bx0);
    let mut index = vec![u32::MAX; bw * (by1 - by0 + 1)];
    for (k, &i) in a.pixels.iter().enumerate() {
        index[local(i)] = k as u32;
    }
    let neighbours = |i: usize| -> [Option<usize>; 4] {
        let (x, y) = (i % w, i / w);
        [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)]
    };
    let cells = &a.pixels;
    let guide: Vec<f64> = cells.iter().map(|&i| guide_at(i)).collect();
    let rel: Vec<f64> = cells.iter().map(|&i| reliability(i)).collect();
    // Σ_{n ∈ N(i)} (u_i − u_n) = Σ ρ (G_i − G_n); neighbours outside the image are left out (Neumann), neighbours
    // outside M are known (Dirichlet, measured values)
    let mut rhs = vec![0f64; cells.len()];
    let (mut off, mut cnt) = (0f64, 0usize);
    for (k, &i) in cells.iter().enumerate() {
        for j in neighbours(i).into_iter().flatten() {
            let kj = index[local(j)];
            let (gj, rj) = if kj != u32::MAX { (guide[kj as usize], rel[kj as usize]) } else { (guide_at(j), reliability(j)) };
            rhs[k] += rel[k].min(rj) * (guide[k] - gj);
            if kj == u32::MAX {
                debug_assert!(!mask[j]);
                let t = lift(l[target][j] as f64);
                rhs[k] += t;
                off += t - gj;
                cnt += 1;
            }
        }
    }
    let off = if cnt > 0 { off / cnt as f64 } else { lift(clip[target] as f64) - guide[0] };
    // the operator as a graph: one row per pixel of the area, its neighbours inside the image on the diagonal, the
    // neighbours inside the area off the diagonal
    let graph = Graph::of_area(cells, w, h, |j| index[local(j)]);
    // start: the guide shifted to the mean offset on the border
    let mut u: Vec<f64> = guide.iter().map(|g| g + off).collect();
    // conjugate gradients converge on a Laplacian in a number of steps of the order of the area's width; a coarse
    // correction first (aggregated 2 × 2 blocks, recursively) removes the smooth part of the error those steps spend
    // their time on
    let span = (a.x1 - a.x0).max(a.y1 - a.y0) + 1;
    let coords: Vec<(u32, u32)> = cells.iter().map(|&i| ((i % w) as u32, (i / w) as u32)).collect();
    let (iterations, converged) = if cells.len() >= COARSE_FROM && std::env::var_os("GRAINEO_HL_COARSE").map_or(true, |v| v != "0") {
        let mg = Multigrid::build(graph, &coords);
        preconditioned_cg(&mg, &rhs, &mut u, 4 * span + 100, CG_TOL)
    } else {
        conjugate_gradients(&graph, &rhs, &mut u, 4 * span + 100, CG_TOL)
    };
    let out = cells
        .iter()
        .zip(&u)
        .filter(|(_, v)| v.is_finite())
        .map(|(&i, &v)| {
            let v = match variant {
                Variant::Linear => v,
                Variant::Log => v.exp(),
            };
            (i, v as f32)
        })
        .collect();
    (out, iterations, converged)
}

/// Weight of the mask and its seam: 1 in the mask, smoothstep down to 0 at [`SEAM`] + 1 pixels (chessboard
/// distance), sorted by index.
fn seam_weight(mask: &[bool], w: usize, h: usize) -> Vec<(u32, f32)> {
    // distance to the mask by the two passes of a chamfer transform (capped)
    let cap = (SEAM + 1) as u16;
    let mut d: Vec<u16> = mask.iter().map(|m| if *m { 0 } else { cap }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 1);
            }
            if y > 0 {
                v = v.min(d[i - w] + 1);
                if x > 0 {
                    v = v.min(d[i - w - 1] + 1);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + 1);
                }
            }
            d[i] = v.min(cap);
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + 1);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + 1);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + 1);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + 1);
                }
            }
            d[i] = v.min(cap);
        }
    }
    d.iter()
        .enumerate()
        .filter(|(_, v)| **v < cap)
        .map(|(i, &v)| {
            let t = 1.0 - v as f32 / cap as f32;
            (i as u32, t * t * (3.0 - 2.0 * t))
        })
        .collect()
}

/// Monotone map of `[0, 1]` onto `[0, 1]`: `1 − (1 − x)^ratio`, slope `ratio` at 0 (in layer units 1: unchanged near
/// the knee start) when `ratio` (input span / output span) ≥ 1.
fn knee_curve(x: f32, ratio: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if ratio <= 1.0 {
        return x * ratio;
    }
    1.0 - (1.0 - x).powf(ratio)
}

/// Areas from this many pixels get the coarse correction before the conjugate gradients.
const COARSE_FROM: usize = 4096;
/// Coarsest level of the correction (pixels): solved by conjugate gradients alone.
const COARSE_MIN: usize = 1024;
/// Tolerance of the coarse levels (relative residual): only a start for the level above.
const COARSE_TOL: f64 = 1e-3;

/// Symmetric positive definite sparse matrix of a weighted graph Laplacian with a Dirichlet part: row `k` is
/// `diag[k]·x[k] − Σ weight·x[neighbour]` over its entries `start[k]..start[k + 1]`.
struct Graph {
    diag: Vec<f64>,
    start: Vec<u32>,
    adj: Vec<u32>,
    weight: Vec<f64>,
}

impl Graph {
    /// The 4-neighbourhood Laplacian of an area: a neighbour inside the image adds 1 to the diagonal (outside the
    /// image: left out, Neumann); one inside the area (`slot` ≠ `u32::MAX`) is an entry with weight 1.
    fn of_area(cells: &[usize], w: usize, h: usize, slot: impl Fn(usize) -> u32) -> Self {
        let mut g = Graph { diag: Vec::with_capacity(cells.len()), start: Vec::with_capacity(cells.len() + 1), adj: Vec::with_capacity(4 * cells.len()), weight: Vec::new() };
        g.start.push(0);
        for &i in cells {
            let (x, y) = (i % w, i / w);
            let mut d = 0.0;
            for j in [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)].into_iter().flatten() {
                d += 1.0;
                let k = slot(j);
                if k != u32::MAX {
                    g.adj.push(k);
                }
            }
            g.diag.push(d);
            g.start.push(g.adj.len() as u32);
        }
        g.weight = vec![1.0; g.adj.len()];
        g
    }

    fn len(&self) -> usize {
        self.diag.len()
    }

    fn row(&self, k: usize, x: &[f64]) -> f64 {
        let mut s = self.diag[k] * x[k];
        for e in self.start[k] as usize..self.start[k + 1] as usize {
            s -= self.weight[e] * x[self.adj[e] as usize];
        }
        s
    }

    fn apply(&self, x: &[f64], out: &mut [f64]) {
        if self.len() >= PARALLEL_FROM {
            out.par_iter_mut().enumerate().for_each(|(k, o)| *o = self.row(k, x));
        } else {
            for (k, o) in out.iter_mut().enumerate() {
                *o = self.row(k, x);
            }
        }
    }

    /// Galerkin coarsening by aggregation: the nodes of each 2 × 2 block of `coords` form one coarse node
    /// (`Pᵀ A P` with the piecewise constant `P`). Returns the coarse graph, its coordinates and the coarse node of
    /// each node.
    fn coarsen(&self, coords: &[(u32, u32)]) -> (Graph, Vec<(u32, u32)>, Vec<u32>) {
        let n = self.len();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for &(x, y) in coords {
            (x0, y0, x1, y1) = (x0.min(x / 2), y0.min(y / 2), x1.max(x / 2), y1.max(y / 2));
        }
        let bw = (x1 - x0 + 1) as usize;
        let mut slot = vec![u32::MAX; bw * (y1 - y0 + 1) as usize];
        let mut parent = vec![0u32; n];
        let mut ccoords = Vec::new();
        for (k, &(x, y)) in coords.iter().enumerate() {
            let b = (y / 2 - y0) as usize * bw + (x / 2 - x0) as usize;
            if slot[b] == u32::MAX {
                slot[b] = ccoords.len() as u32;
                ccoords.push((x / 2, y / 2));
            }
            parent[k] = slot[b];
        }
        drop(slot);
        let m = ccoords.len();
        // members of each coarse node (counting sort)
        let mut first = vec![0u32; m + 1];
        for &p in &parent {
            first[p as usize + 1] += 1;
        }
        for c in 0..m {
            first[c + 1] += first[c];
        }
        let mut fill = first.clone();
        let mut members = vec![0u32; n];
        for (k, &p) in parent.iter().enumerate() {
            members[fill[p as usize] as usize] = k as u32;
            fill[p as usize] += 1;
        }
        let rows: Vec<(f64, Vec<(u32, f64)>)> = (0..m)
            .into_par_iter()
            .map(|c| {
                let mut d = 0.0;
                let mut row: Vec<(u32, f64)> = Vec::new();
                for &k in &members[first[c] as usize..first[c + 1] as usize] {
                    let k = k as usize;
                    d += self.diag[k];
                    for e in self.start[k] as usize..self.start[k + 1] as usize {
                        let pj = parent[self.adj[e] as usize];
                        if pj as usize == c {
                            d -= self.weight[e];
                        } else if let Some(r) = row.iter_mut().find(|r| r.0 == pj) {
                            r.1 += self.weight[e];
                        } else {
                            row.push((pj, self.weight[e]));
                        }
                    }
                }
                (d, row)
            })
            .collect();
        let mut g = Graph { diag: Vec::with_capacity(m), start: Vec::with_capacity(m + 1), adj: Vec::new(), weight: Vec::new() };
        g.start.push(0);
        for (d, row) in rows {
            g.diag.push(d);
            for (j, wt) in row {
                g.adj.push(j);
                g.weight.push(wt);
            }
            g.start.push(g.adj.len() as u32);
        }
        (g, ccoords, parent)
    }
}

/// Damping of the Jacobi smoother of [`Multigrid`].
const JACOBI_OMEGA: f64 = 0.67;
/// Jacobi sweeps before and after the coarse correction of each level.
const SMOOTH_STEPS: usize = 1;

/// Hierarchy of aggregated graphs ([`Graph::coarsen`]) used as the preconditioner of [`preconditioned_cg`]: one
/// V-cycle with damped Jacobi smoothing per step (aggregation multigrid; Vaněk, Mandel, Brezina 1996; Notay 2010).
struct Multigrid {
    levels: Vec<Graph>,
    /// `parents[l][k]`: node of level `l + 1` that holds node `k` of level `l`.
    parents: Vec<Vec<u32>>,
    /// `members[l]`: the nodes of level `l` held by each node of level `l + 1` (offsets, nodes).
    members: Vec<(Vec<u32>, Vec<u32>)>,
    /// Span of the coarsest level (iteration limit of its solve).
    coarsest_span: usize,
}

impl Multigrid {
    fn build(fine: Graph, coords: &[(u32, u32)]) -> Self {
        let mut levels = vec![fine];
        let mut parents = Vec::new();
        let mut coords = coords.to_vec();
        while levels.last().map_or(0, Graph::len) >= COARSE_MIN {
            let (g, c, p) = levels.last().unwrap().coarsen(&coords);
            if g.len() * 2 > levels.last().unwrap().len() {
                break;
            }
            levels.push(g);
            parents.push(p);
            coords = c;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for &(x, y) in &coords {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let members = parents
            .iter()
            .zip(&levels[1..])
            .map(|(p, g)| {
                let m = g.len();
                let mut first = vec![0u32; m + 1];
                for &c in p {
                    first[c as usize + 1] += 1;
                }
                for c in 0..m {
                    first[c + 1] += first[c];
                }
                let mut fill = first.clone();
                let mut list = vec![0u32; p.len()];
                for (k, &c) in p.iter().enumerate() {
                    list[fill[c as usize] as usize] = k as u32;
                    fill[c as usize] += 1;
                }
                (first, list)
            })
            .collect();
        Multigrid { levels, parents, members, coarsest_span: (x1.saturating_sub(x0)).max(y1.saturating_sub(y0)) as usize + 1 }
    }

    /// `z ≈ A⁻¹ r` by one V-cycle from level `l`.
    fn vcycle(&self, l: usize, r: &[f64], z: &mut [f64]) {
        let g = &self.levels[l];
        let n = g.len();
        z.iter_mut().for_each(|v| *v = 0.0);
        if l + 1 == self.levels.len() {
            conjugate_gradients(g, r, z, 4 * self.coarsest_span + 100, COARSE_TOL);
            return;
        }
        let par = n >= PARALLEL_FROM;
        let mut az = vec![0f64; n];
        let smooth = |z: &mut [f64], az: &mut [f64]| {
            g.apply(z, az);
            let step = |(k, zk): (usize, &mut f64)| *zk += JACOBI_OMEGA * (r[k] - az[k]) / g.diag[k];
            if par {
                z.par_iter_mut().enumerate().for_each(step);
            } else {
                z.iter_mut().enumerate().for_each(step);
            }
        };
        for _ in 0..SMOOTH_STEPS {
            smooth(z, &mut az);
        }
        g.apply(z, &mut az);
        let parent = &self.parents[l];
        let m = self.levels[l + 1].len();
        let (first, members) = &self.members[l];
        let gather = |c: usize| members[first[c] as usize..first[c + 1] as usize].iter().map(|&k| r[k as usize] - az[k as usize]).sum::<f64>();
        let rc: Vec<f64> = if par { (0..m).into_par_iter().map(gather).collect() } else { (0..m).map(gather).collect() };
        let mut ec = vec![0f64; m];
        self.vcycle(l + 1, &rc, &mut ec);
        if par {
            z.par_iter_mut().enumerate().for_each(|(k, zk)| *zk += ec[parent[k] as usize]);
        } else {
            for k in 0..n {
                z[k] += ec[parent[k] as usize];
            }
        }
        for _ in 0..SMOOTH_STEPS {
            smooth(z, &mut az);
        }
    }
}

/// [`conjugate_gradients`] preconditioned by one V-cycle of `mg` per step (flexible form: the coarsest solve is not
/// exactly linear). Same stopping rule on the true residual.
fn preconditioned_cg(mg: &Multigrid, b: &[f64], u: &mut [f64], max_iter: usize, tol: f64) -> (usize, bool) {
    let g = &mg.levels[0];
    let n = b.len();
    let par = n >= PARALLEL_FROM;
    let mut r = vec![0f64; n];
    g.apply(u, &mut r);
    for k in 0..n {
        r[k] = b[k] - r[k];
    }
    let bb = dot(b, b).max(1e-30);
    let mut z = vec![0f64; n];
    mg.vcycle(0, &r, &mut z);
    let mut p = z.clone();
    let mut ap = vec![0f64; n];
    let mut rz = dot(&r, &z);
    let mut rr = dot(&r, &r);
    let mut z_old = vec![0f64; n];
    for it in 0..max_iter {
        if rr / bb < tol * tol {
            return (it, true);
        }
        g.apply(&p, &mut ap);
        let pap = dot(&p, &ap);
        if !(pap > 0.0) {
            return (it, false);
        }
        let alpha = rz / pap;
        if par {
            u.par_iter_mut().zip(r.par_iter_mut()).zip(p.par_iter().zip(ap.par_iter())).for_each(|((uk, rk), (pk, apk))| {
                *uk += alpha * pk;
                *rk -= alpha * apk;
            });
        } else {
            for k in 0..n {
                u[k] += alpha * p[k];
                r[k] -= alpha * ap[k];
            }
        }
        rr = dot(&r, &r);
        std::mem::swap(&mut z, &mut z_old);
        mg.vcycle(0, &r, &mut z);
        // Conjugate-gradient β update: β = rᵀ(z − z_old) / (r_oldᵀ z_old)
        let rz_new = dot(&r, &z);
        let r_zold = dot(&r, &z_old);
        let beta = ((rz_new - r_zold) / rz).max(0.0);
        rz = rz_new;
        if par {
            p.par_iter_mut().zip(z.par_iter()).for_each(|(pk, zk)| *pk = zk + beta * *pk);
        } else {
            for k in 0..n {
                p[k] = z[k] + beta * p[k];
            }
        }
    }
    (max_iter, rr / bb < tol * tol)
}

/// Solves `A u = b` for the symmetric positive definite `graph`, starting from `u`, at most `max_iter` steps.
/// Returns the iterations and whether the residual reached `tol` (relative to `b`).
fn conjugate_gradients(graph: &Graph, b: &[f64], u: &mut [f64], max_iter: usize, tol: f64) -> (usize, bool) {
    let n = b.len();
    let par = n >= PARALLEL_FROM;
    let mut r = vec![0f64; n];
    graph.apply(u, &mut r);
    for k in 0..n {
        r[k] = b[k] - r[k];
    }
    let mut p = r.clone();
    let mut ap = vec![0f64; n];
    let mut rr: f64 = dot(&r, &r);
    let bb: f64 = dot(b, b).max(1e-30);
    for it in 0..max_iter {
        if rr / bb < tol * tol {
            return (it, true);
        }
        graph.apply(&p, &mut ap);
        let pap: f64 = dot(&p, &ap);
        if pap <= 0.0 {
            return (it, false);
        }
        let alpha = rr / pap;
        let rr_new: f64 = if par {
            u.par_iter_mut()
                .zip(r.par_iter_mut())
                .zip(p.par_iter().zip(ap.par_iter()))
                .map(|((uk, rk), (pk, apk))| {
                    *uk += alpha * pk;
                    *rk -= alpha * apk;
                    *rk * *rk
                })
                .sum()
        } else {
            let mut s = 0.0;
            for k in 0..n {
                u[k] += alpha * p[k];
                r[k] -= alpha * ap[k];
                s += r[k] * r[k];
            }
            s
        };
        let beta = rr_new / rr;
        rr = rr_new;
        if par {
            p.par_iter_mut().zip(r.par_iter()).for_each(|(pk, rk)| *pk = rk + beta * *pk);
        } else {
            for k in 0..n {
                p[k] = r[k] + beta * p[k];
            }
        }
    }
    (max_iter, rr / bb < tol * tol)
}

/// Dot product (parallel for long vectors).
fn dot(a: &[f64], b: &[f64]) -> f64 {
    if a.len() >= PARALLEL_FROM {
        a.par_iter().zip(b.par_iter()).map(|(x, y)| x * y).sum()
    } else {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }
}

/// Slider "Lichter-Farbe" (0 … 1, 0.5 = as rebuilt): chroma factor in the rebuilt areas, 0 = neutral, 1 = as rebuilt,
/// at most [`COLOUR_MAX`] (above 1 only clear colours gain, see the colour step of the Quattro grid).
pub fn colour_factor(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 { 2.0 * v } else { 1.0 + (COLOUR_MAX - 1.0) * 2.0 * (v - 0.5) }
}

/// Largest chroma factor of "Lichter-Farbe" (slider 100).
pub const COLOUR_MAX: f32 = 1.6;

/// Slider "Lichter-Struktur" (0 … 1, 0.5 = as rebuilt): factor of the top-layer detail in the rebuilt areas, 0 =
/// smooth, 1 = as rebuilt, [`STRUCTURE_MAX`] at 100 (the rebuilt top layer carries little fine detail of its own, a
/// factor of 2 hardly showed).
pub fn structure_factor(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 { 2.0 * v } else { 1.0 + (STRUCTURE_MAX - 1.0) * 2.0 * (v - 0.5) }
}

/// Largest detail factor of "Lichter-Struktur" (slider 100).
pub const STRUCTURE_MAX: f32 = 3.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A cyan-like scene: top = 2·middle = 4·bottom with a textured bright patch; the top layer clips in the patch.
    fn scene(w: usize, h: usize) -> ([Vec<f32>; 3], Vec<f32>) {
        let mut truth = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - w as f32 / 2.0, y as f32 - h as f32 / 2.0);
                let base = 0.3 + 0.9 * (-(dx * dx + dy * dy) / (w as f32 * w as f32 / 16.0)).exp();
                let texture = if (x / 3 + y / 3) % 2 == 0 { 0.05 } else { 0.0 };
                truth[y * w + x] = base + texture;
            }
        }
        let layers = [truth.iter().map(|t| t / 4.0).collect(), truth.iter().map(|t| t / 2.0).collect(), truth.iter().map(|t| t.min(1.0)).collect()];
        (layers, truth)
    }

    #[test]
    fn nothing_clipped_changes_nothing() {
        let (mut l, _) = scene(32, 32);
        for v in l[2].iter_mut() {
            *v *= 0.5;
        }
        let before = l.clone();
        assert!(rebuild_layer(&mut l, 32, 32, [1.0; 3], 2, Variant::Linear).is_none());
        assert_eq!(l, before);
    }

    #[test]
    fn clipped_top_gets_structure_back() {
        for variant in [Variant::Linear, Variant::Log] {
            let (w, h) = (48, 48);
            let (mut l, truth) = scene(w, h);
            let o = rebuild_layer(&mut l, w, h, [10.0, 10.0, 1.0 / CLIP_SHARE], 2, variant).expect("rebuild");
            let s = &o.stats;
            assert!(s.rebuilt > 0 && s.all_clipped == 0 && s.unconverged == 0, "{s:?}");
            // the fit finds top ≈ 4·bottom or 2·middle (any mix of both)
            assert!((s.weights[0] / 4.0 + s.weights[1] / 2.0 - 1.0).abs() < 0.05, "{:?}", s.weights);
            // structure: inside the clipped patch neighbouring values differ again, in the order of the truth
            let c = (h / 2) * w + w / 2;
            let (a, b) = (l[2][c], l[2][c + 3]);
            assert!((a - b).abs() > 1e-3);
            assert_eq!(a > b, truth[c] > truth[c + 3]);
            // the knee keeps everything at or below the clip level and the ratio of the layers (colour)
            assert!(l[2].iter().all(|v| *v <= 1.0 + 1e-4));
            assert!((l[2][c] / l[1][c] - 2.0).abs() < 0.2, "{} {}", l[2][c], l[1][c]);
            // the weight covers the mask and fades out
            assert!(o.weight.iter().any(|(_, wt)| *wt >= 1.0) && o.weight.iter().any(|(_, wt)| *wt < 1.0));
        }
    }

    #[test]
    fn far_pixels_are_untouched() {
        let (w, h) = (64, 64);
        let (mut l, _) = scene(w, h);
        let before = l.clone();
        let o = rebuild_layer(&mut l, w, h, [10.0, 10.0, 1.0 / CLIP_SHARE], 2, Variant::Linear).expect("rebuild");
        let touched: std::collections::HashSet<u32> = o.weight.iter().map(|(i, _)| *i).collect();
        for i in 0..w * h {
            if !touched.contains(&(i as u32)) {
                for c in 0..3 {
                    assert_eq!(l[c][i], before[c][i]);
                }
            }
        }
    }

    #[test]
    fn all_layers_clipped_is_left_alone() {
        let (w, h) = (16, 16);
        let mut l = [vec![1.0f32; w * h], vec![1.0f32; w * h], vec![1.0f32; w * h]];
        for i in 0..100 {
            l[0][i] = 0.1;
            l[1][i] = 0.2;
            l[2][i] = 0.6;
        }
        let before = l.clone();
        assert!(rebuild_layer(&mut l, w, h, [1.0 / CLIP_SHARE; 3], 2, Variant::Linear).is_none());
        assert_eq!(l, before);
    }

    #[test]
    fn slider_factors() {
        assert_eq!(colour_factor(0.5), 1.0);
        assert_eq!(structure_factor(0.5), 1.0);
        assert_eq!(colour_factor(0.0), 0.0);
        assert_eq!(structure_factor(1.0), STRUCTURE_MAX);
        assert_eq!(colour_factor(1.0), COLOUR_MAX);
    }
}
