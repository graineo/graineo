//! Series and stacks: photos of the same subject shown as one tile.
//!
//! - **Series** (`kind = 'series'`, `origin = 'auto'`): detected in the background per capture day and camera – taken
//!   shortly one after the other *and* showing the same subject. Time alone bundles far too much (a walk through a
//!   town), image content alone too (the same church in the morning and in the evening), so the time chain is the
//!   pre-selection and the similarity the condition.
//! - **Exposure bracket** (`kind = 'bracket'`): a series whose photos differ mainly in exposure.
//! - **Stack** (`kind = 'manual'`): bundled by the user (or taken over from Lightroom, `origin = 'lightroom'`).
//!
//! As soon as the user changes an automatic series (takes a photo out, chooses the cover, reviews it), its origin
//! becomes `'user'`; the background grouping never rebuilds such stacks. Photos the user split apart are remembered
//! in `stack_never` and never grouped together again.
//!
//! Similarity comes from the CLIP image vectors when there are some (`image_vector`), otherwise from an own small
//! image signature ([`signature`]: difference hash + 8×8 Lab thumbnail of the grid preview, no model needed). Without
//! either, only very short time chains (≤ [`STRICT_GAP`] s) count as a series.
//!
//! The library is the main store: stacks are kept only in `library.sqlite`, never in sidecars.

use std::collections::{BTreeMap, HashMap, HashSet};

use image::RgbImage;
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Db;

type R<T> = Result<T, String>;

fn e(x: rusqlite::Error) -> String {
    x.to_string()
}

/// Version of the grouping: a new version regroups all automatic series that were not touched by the user.
/// 2: exposure brackets without aperture data (manual lenses) are recognized.
/// 3: frames of the same second in file name order (not import order); brackets whose aperture moves with the
/// exposure time (program shift, e.g. Panasonic GX7) are brackets; a bracket set by hand on a tripod (aperture and
/// ISO fixed, steps ≥ 0.7 EV) may take up to [`MANUAL_BRACKET_GAP`] s per frame.
/// 4: a series never spans folders, a Sigma SFD file (X3I, which holds a bracket itself) never joins single frames;
/// the cover of an exposure bracket is its middle exposure.
pub const ALGO: i64 = 4;
/// Version of the image signature (`image_quality.signature`).
pub const SIGNATURE_VERSION: i64 = 1;
/// Length of an image signature in bytes: 8 bytes difference hash + 64 cells × Lab (3 bytes).
pub const SIGNATURE_LEN: usize = 8 + 64 * 3;
/// Larger groups are split at their largest similarity gap: they are hardly ever a single decision.
pub const MAX_MEMBERS: usize = 60;
/// Without any similarity information: only photos at most this many seconds apart form a (provisional) series.
pub const STRICT_GAP: i64 = 2;
/// RAW and camera JPEG of the same shot: at most this far apart (seconds), see `crate::pairs`.
const PAIR_GAP: i64 = 1;

// ───────────────────────────────────────────── image signature

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn lab_f(t: f32) -> f32 {
    if t > 0.008856 {
        t.cbrt()
    } else {
        7.787 * t + 16.0 / 116.0
    }
}

/// sRGB (0..1) → CIE Lab (D65).
fn lab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let (r, g, b) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let (fx, fy, fz) = (lab_f(x), lab_f(y), lab_f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Image signature of an (upright) preview: a 64-bit difference hash of a 9×8 grey image (bit = left cell darker
/// than its right neighbour) followed by an 8×8 Lab thumbnail (L × 2.55, a + 128, b + 128, one byte each).
/// Costs well under a millisecond for a 512-pixel preview.
pub fn signature(img: &RgbImage) -> Vec<u8> {
    let (w, h) = (img.width().max(1) as usize, img.height().max(1) as usize);
    let mut grey = [0f64; 72];
    let mut grey_n = [0u32; 72];
    let mut col = [[0f64; 3]; 64];
    let mut col_n = [0u32; 64];
    for (x, y, p) in img.enumerate_pixels() {
        let (x, y) = (x as usize, y as usize);
        let [r, g, b] = p.0;
        let v = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
        let gi = (y * 8 / h) * 9 + x * 9 / w;
        grey[gi] += v;
        grey_n[gi] += 1;
        let ci = (y * 8 / h) * 8 + x * 8 / w;
        col[ci][0] += r as f64;
        col[ci][1] += g as f64;
        col[ci][2] += b as f64;
        col_n[ci] += 1;
    }
    let mut out = Vec::with_capacity(SIGNATURE_LEN);
    let mut hash = 0u64;
    for row in 0..8 {
        for c in 0..8 {
            let i = row * 9 + c;
            let a = grey[i] / grey_n[i].max(1) as f64;
            let b = grey[i + 1] / grey_n[i + 1].max(1) as f64;
            if a < b {
                hash |= 1 << (row * 8 + c);
            }
        }
    }
    out.extend_from_slice(&hash.to_le_bytes());
    for (c, n) in col.iter().zip(col_n) {
        let n = n.max(1) as f64 * 255.0;
        let [l, a, b] = lab((c[0] / n) as f32, (c[1] / n) as f32, (c[2] / n) as f32);
        out.push((l * 2.55).round().clamp(0.0, 255.0) as u8);
        out.push((a + 128.0).round().clamp(0.0, 255.0) as u8);
        out.push((b + 128.0).round().clamp(0.0, 255.0) as u8);
    }
    out
}

/// Distance parts of two signatures: differing hash bits (0..64) and mean Lab distance of the cells.
fn signature_distance(a: &[u8], b: &[u8]) -> Option<(u32, f32)> {
    if a.len() != SIGNATURE_LEN || b.len() != SIGNATURE_LEN {
        return None;
    }
    let ha = u64::from_le_bytes(a[..8].try_into().ok()?);
    let hb = u64::from_le_bytes(b[..8].try_into().ok()?);
    let bits = (ha ^ hb).count_ones();
    let mut sum = 0f32;
    for (p, q) in a[8..].chunks_exact(3).zip(b[8..].chunks_exact(3)) {
        let dl = (p[0] as f32 - q[0] as f32) / 2.55;
        let da = p[1] as f32 - q[1] as f32;
        let db = p[2] as f32 - q[2] as f32;
        sum += (dl * dl + da * da + db * db).sqrt();
    }
    Some((bits, sum / 64.0))
}

/// Similarity of two image signatures, 0 (unrelated) … 1 (same picture). Burst frames typically score above 0.8,
/// variants of the same scene (other framing) around 0.6–0.8, different subjects below 0.4.
pub fn signature_similarity(a: &[u8], b: &[u8]) -> Option<f32> {
    let (bits, d) = signature_distance(a, b)?;
    let s = 1.0 - 0.6 * (bits as f32 / 32.0).min(1.0) - 0.4 * (d / 30.0).min(1.0);
    Some(s.clamp(0.0, 1.0))
}

/// Rough offset of `b` against `a` in thumbnail cells (x, y as a share of the image width/height, −0.25 … 0.25):
/// the shift of the 8×8 lightness grid with the smallest mean difference. Used to align zoomed crops of slightly
/// shifted burst frames.
pub fn signature_shift(a: &[u8], b: &[u8]) -> Option<(f32, f32)> {
    if a.len() != SIGNATURE_LEN || b.len() != SIGNATURE_LEN {
        return None;
    }
    let l = |s: &[u8], x: i32, y: i32| s[8 + ((y * 8 + x) * 3) as usize] as f32;
    let mut best = (f32::MAX, 0i32, 0i32);
    for dy in -2..=2i32 {
        for dx in -2..=2i32 {
            let (mut sum, mut n) = (0f32, 0u32);
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let (x2, y2) = (x + dx, y + dy);
                    if (0..8).contains(&x2) && (0..8).contains(&y2) {
                        sum += (l(a, x, y) - l(b, x2, y2)).abs();
                        n += 1;
                    }
                }
            }
            // a slight preference for no shift: noise must not move the crop
            let cost = sum / n.max(1) as f32 + (dx.abs() + dy.abs()) as f32 * 0.5;
            if cost < best.0 {
                best = (cost, dx, dy);
            }
        }
    }
    Some((best.1 as f32 / 8.0, best.2 as f32 / 8.0))
}

// ───────────────────────────────────────────── grouping (pure, testable)

/// What a similarity value came from (the thresholds differ).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// CLIP ViT-B/32 image vectors (cosine)
    Clip,
    /// own image signature ([`signature_similarity`])
    Signature,
    /// no similarity known: time only, strict
    Time,
}

impl Basis {
    pub fn name(self) -> &'static str {
        match self {
            Basis::Clip => "clip",
            Basis::Signature => "signature",
            Basis::Time => "time",
        }
    }
}

/// Thresholds of the grouping for one kind of similarity.
#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    /// photos at most this many seconds apart may form a series …
    pub gap: i64,
    /// … or up to this many when they are very similar (calm subject, tripod)
    pub gap_long: i64,
    pub long_sim: f32,
    /// similarity to the previous photo below this: subject changed
    pub join: f32,
    /// similarity to the first photo of the group below this: the series wandered off (slow pan)
    pub drift: f32,
}

pub fn thresholds(b: Basis) -> Thresholds {
    match b {
        // duplicates start at 0.97; variants of a scene lie between about 0.88 and 0.97
        Basis::Clip => Thresholds { gap: 10, gap_long: 30, long_sim: 0.93, join: 0.88, drift: 0.82 },
        Basis::Signature => Thresholds { gap: 10, gap_long: 30, long_sim: 0.80, join: 0.60, drift: 0.50 },
        Basis::Time => Thresholds { gap: STRICT_GAP, gap_long: STRICT_GAP, long_sim: 1.0, join: 0.0, drift: 0.0 },
    }
}

/// One photo (original) to group.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub image: i64,
    pub file: i64,
    pub folder: i64,
    /// file name without extension, lower case (RAW+JPEG pairs)
    pub stem: String,
    pub raw: bool,
    pub camera: String,
    /// capture time in seconds
    pub secs: i64,
    pub aperture: Option<f64>,
    pub exposure: Option<f64>,
    pub iso: Option<i64>,
    /// already rated by the user (picked / stars): preferred as cover of a new series
    pub rank: i32,
    /// a Sigma SFD file (X3I): it holds a whole bracket itself and is never grouped with single frames
    pub sfd: bool,
}

impl Item {
    /// Exposure value at ISO 100 (log2(N²/t) − log2(ISO/100)); `None` without the shooting data.
    pub fn ev(&self) -> Option<f64> {
        // without an aperture (a manual or adapted lens writes none) the exposure still counts: the aperture of a
        // bracket is fixed, so the steps between its frames come from the time and ISO alone
        let (n, t) = (self.aperture.unwrap_or(1.0), self.exposure?);
        if n <= 0.0 || t <= 0.0 {
            return None;
        }
        let iso = self.iso.unwrap_or(100).max(1) as f64;
        Some((n * n / t).log2() - (iso / 100.0).log2())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Series,
    Bracket,
    Manual,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Series => "series",
            Kind::Bracket => "bracket",
            Kind::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Kind {
        match s {
            "bracket" => Kind::Bracket,
            "manual" => Kind::Manual,
            _ => Kind::Series,
        }
    }
}

/// Why photos form a series (stored as JSON in `stack.reason`, shown as an explanation).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reason {
    pub count: usize,
    /// seconds from the first to the last photo
    pub span_s: i64,
    /// largest gap between two photos
    pub gap_max_s: i64,
    pub sim_min: Option<f32>,
    pub sim_mean: Option<f32>,
    /// exposure bracket: number of distinct exposure steps
    pub ev_steps: Option<u32>,
    /// exposure bracket: range in EV
    pub ev_range: Option<f32>,
    pub model: String,
}

impl Reason {
    pub fn to_json(&self) -> String {
        let r = |v: Option<f32>| v.map(|x| (x * 1000.0).round() / 1000.0);
        serde_json::json!({
            "count": self.count, "span_s": self.span_s, "gap_max_s": self.gap_max_s,
            "sim_min": r(self.sim_min), "sim_mean": r(self.sim_mean),
            "ev_steps": self.ev_steps, "ev_range": r(self.ev_range), "model": self.model,
        })
        .to_string()
    }
    pub fn from_json(s: &str) -> Reason {
        let v: serde_json::Value = serde_json::from_str(s).unwrap_or_default();
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
        Reason {
            count: v.get("count").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            span_s: v.get("span_s").and_then(|x| x.as_i64()).unwrap_or(0),
            gap_max_s: v.get("gap_max_s").and_then(|x| x.as_i64()).unwrap_or(0),
            sim_min: f("sim_min"),
            sim_mean: f("sim_mean"),
            ev_steps: v.get("ev_steps").and_then(|x| x.as_u64()).map(|x| x as u32),
            ev_range: f("ev_range"),
            model: v.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        }
    }
}

/// A detected group: images in capture order (RAW before its camera JPEG).
#[derive(Clone, Debug)]
pub struct Group {
    pub images: Vec<i64>,
    pub kind: Kind,
    pub reason: Reason,
    /// image shown on top (preferably one the user already rated)
    pub cover: i64,
}

/// One link of a chain: a photo, or a RAW with its camera JPEG (counts as one picture).
struct Link {
    items: Vec<usize>,
    /// representative item (the RAW of a pair) for similarities
    rep: usize,
    secs: i64,
}

/// Similarity between two files: value and what it came from.
pub type SimFn<'a> = dyn Fn(i64, i64) -> Option<(f32, Basis)> + 'a;

/// Group the photos of one capture day (any cameras; grouped per camera and folder: a series never spans two
/// folders, and a Sigma SFD file never joins single frames). `sim(file_a, file_b)` gives the similarity of two files
/// if known; `never` holds image pairs (smaller id first) that must not be grouped.
pub fn group(items: &[Item], sim: &SimFn, never: &HashSet<(i64, i64)>) -> Vec<Group> {
    let mut by_camera: BTreeMap<(&str, i64, bool), Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        by_camera.entry((it.camera.as_str(), it.folder, it.sfd)).or_default().push(i);
    }
    let mut out = Vec::new();
    for (_, mut idx) in by_camera {
        // frames of the same second (no sub-seconds stored) in file name order: the order of the camera, not of the
        // import (the image ids of an import follow whichever file was read first)
        idx.sort_by(|&a, &b| {
            let (x, y) = (&items[a], &items[b]);
            (x.secs, &x.stem, !x.raw, x.image).cmp(&(y.secs, &y.stem, !y.raw, y.image))
        });
        let links = links(items, &idx);
        for chain in chains(items, &links, sim, never) {
            for part in split_large(items, &links, chain, sim) {
                if part.len() >= 2 {
                    out.push(make_group(items, &links, &part, sim));
                }
            }
        }
    }
    out
}

/// Pair RAW and camera JPEG of the same shot (same folder and name, at most [`PAIR_GAP`] s apart) into one link.
fn links(items: &[Item], idx: &[usize]) -> Vec<Link> {
    let mut used = vec![false; idx.len()];
    let mut out = Vec::new();
    for a in 0..idx.len() {
        if used[a] {
            continue;
        }
        used[a] = true;
        let mut link = Link { items: vec![idx[a]], rep: idx[a], secs: items[idx[a]].secs };
        for b in a + 1..idx.len() {
            let (x, y) = (&items[idx[a]], &items[idx[b]]);
            if y.secs - x.secs > PAIR_GAP {
                break;
            }
            if !used[b] && x.folder == y.folder && x.stem == y.stem && x.raw != y.raw {
                used[b] = true;
                link.items.push(idx[b]);
                if y.raw {
                    link.rep = idx[b];
                }
            }
        }
        out.push(link);
    }
    out
}

fn never_with(items: &[Item], links: &[Link], group: &[usize], cand: &Link, never: &HashSet<(i64, i64)>) -> bool {
    if never.is_empty() {
        return false;
    }
    group.iter().flat_map(|&l| links[l].items.iter()).any(|&a| {
        cand.items.iter().any(|&b| {
            let (x, y) = (items[a].image, items[b].image);
            never.contains(&(x.min(y), x.max(y)))
        })
    })
}

/// How much lower the neighbour similarity may be for two frames of an exposure bracket and still join.
const BRACKET_SIM_SLACK: f32 = 0.25;

/// Seconds between two frames of a bracket set by hand (time dial turned between the shots, tripod, Canon 5D Mark II
/// "garten": 3–6 s): only with aperture and ISO fixed and steps of at least 0.7 EV.
const MANUAL_BRACKET_GAP: i64 = 6;

/// Two shots one after the other of a bracket set by hand: same aperture (known) and ISO, exposure at least 0.7 EV
/// apart, at most [`MANUAL_BRACKET_GAP`] s between them.
fn manual_bracket_step(a: &Item, b: &Item, gap: i64) -> bool {
    if gap > MANUAL_BRACKET_GAP || a.aperture.is_none() || !same_aperture(a.aperture, b.aperture) || a.iso != b.iso {
        return false;
    }
    matches!((a.ev(), b.ev()), (Some(ea), Some(eb)) if (ea - eb).abs() >= 0.7)
}

/// Two shots one after the other of an exposure bracket: same camera and aperture (or an aperture moving with the
/// exposure time, see [`bracket_apertures`]), exposure at least 0.7 EV apart, at most 3 s between them (a bracket set
/// by hand: [`manual_bracket_step`]).
fn bracket_neighbours(a: &Item, b: &Item, gap: i64) -> bool {
    if a.camera != b.camera {
        return false;
    }
    if gap > 3 {
        return manual_bracket_step(a, b, gap);
    }
    match (bracket_apertures(a, b), a.ev(), b.ev()) {
        (true, Some(ea), Some(eb)) => (ea - eb).abs() >= 0.7,
        _ => false,
    }
}

/// Time chains split where the subject changes or drifts away (indices into `links`).
fn chains(items: &[Item], links: &[Link], sim: &SimFn, never: &HashSet<(i64, i64)>) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (li, l) in links.iter().enumerate() {
        let joins = match cur.last() {
            None => false,
            Some(&prev) => {
                let p = &links[prev];
                let gap = l.secs - p.secs;
                let (pf, lf) = (items[p.rep].file, items[l.rep].file);
                let ok = match sim(pf, lf) {
                    Some((v, b)) => {
                        let t = thresholds(b);
                        let in_time = gap <= t.gap || (gap <= t.gap_long && v >= t.long_sim);
                        let first = items[links[cur[0]].rep].file;
                        let drift_ok = cur.len() < 2 || sim(first, lf).is_none_or(|(d, db)| db != b || d >= t.drift);
                        // wide exposure brackets (±3 EV): the darkest and the brightest frame look different, the
                        // similarity of neighbours alone would cut the bracket into pairs
                        let wide = bracket_neighbours(&items[p.rep], &items[l.rep], gap) && v >= t.join - BRACKET_SIM_SLACK;
                        (in_time && v >= t.join && drift_ok) || wide
                    }
                    None => gap <= STRICT_GAP,
                };
                ok && !never_with(items, links, &cur, l, never)
            }
        };
        if !joins && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(li);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Pieces with more links than this are only cut in their middle half, so that a day-long uniform chain (a
/// time-lapse of 20 000 photos) splits in O(n log n) instead of peeling off one photo per step.
const SPLIT_FULL_SEARCH: usize = 1024;

/// Split chains above [`MAX_MEMBERS`] pictures at their weakest neighbour similarity (else the largest time gap,
/// else nearest the middle). Iterative with an explicit work list: no recursion depth, whatever the chain length.
fn split_large(items: &[Item], links: &[Link], chain: Vec<usize>, sim: &SimFn) -> Vec<Vec<usize>> {
    let n = chain.len();
    if n < 2 {
        return vec![chain];
    }
    // cum[k] = pictures in chain[..k]
    let mut cum = Vec::with_capacity(n + 1);
    cum.push(0usize);
    for &l in &chain {
        cum.push(cum.last().unwrap() + links[l].items.len());
    }
    if cum[n] <= MAX_MEMBERS {
        return vec![chain];
    }
    // neighbour similarity and gap in front of link k (index 0 unused); each similarity is asked once
    let mut s = vec![1.0f32; n];
    let mut gap = vec![0i64; n];
    for k in 1..n {
        let (a, b) = (&links[chain[k - 1]], &links[chain[k]]);
        let v = sim(items[a.rep].file, items[b.rep].file).map_or(1.0, |x| x.0);
        s[k] = if v.is_nan() { 1.0 } else { v };
        gap[k] = b.secs - a.secs;
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    // ranges [lo, hi) of `chain`, processed in order (the stack holds the later ones on top reversed)
    let mut work = vec![(0usize, n)];
    while let Some((lo, hi)) = work.pop() {
        if hi - lo < 2 || cum[hi] - cum[lo] <= MAX_MEMBERS {
            out.push(chain[lo..hi].to_vec());
            continue;
        }
        let total = cum[hi] - cum[lo];
        let mid2 = cum[lo] * 2 + total; // twice the middle, to stay in integers
        let restrict = hi - lo > SPLIT_FULL_SEARCH;
        let candidate = |k: usize| !restrict || (cum[k] - cum[lo] >= total / 4 && cum[hi] - cum[k] >= total / 4);
        let mut cut = 0usize;
        let mut best = (f32::MAX, i64::MIN, usize::MAX);
        for k in lo + 1..hi {
            if !candidate(k) {
                continue;
            }
            let dist = (cum[k] * 2).abs_diff(mid2);
            let better = s[k] < best.0
                || (s[k] == best.0 && (gap[k] > best.1 || (gap[k] == best.1 && dist < best.2)));
            if better {
                best = (s[k], gap[k], dist);
                cut = k;
            }
        }
        if cut == 0 {
            // no candidate in the middle half (huge links): cut in the middle by link count
            cut = lo + (hi - lo) / 2;
        }
        work.push((cut, hi));
        work.push((lo, cut));
    }
    out
}

fn make_group(items: &[Item], links: &[Link], part: &[usize], sim: &SimFn) -> Group {
    let images: Vec<i64> = part.iter().flat_map(|&l| links[l].items.iter().map(|&i| items[i].image)).collect();
    let mut sims = Vec::new();
    let mut model = Basis::Time;
    let mut gap_max = 0;
    for k in 1..part.len() {
        let (a, b) = (&links[part[k - 1]], &links[part[k]]);
        gap_max = gap_max.max(b.secs - a.secs);
        if let Some((v, basis)) = sim(items[a.rep].file, items[b.rep].file) {
            sims.push(v);
            if model == Basis::Time || basis == Basis::Clip {
                model = basis;
            }
        }
    }
    let span = links[*part.last().unwrap()].secs - links[part[0]].secs;
    let mut reason = Reason {
        count: images.len(),
        span_s: span,
        gap_max_s: gap_max,
        sim_min: sims.iter().copied().reduce(f32::min),
        sim_mean: (!sims.is_empty()).then(|| sims.iter().sum::<f32>() / sims.len() as f32),
        model: model.name().to_string(),
        ..Reason::default()
    };
    let reps: Vec<&Item> = part.iter().map(|&l| &items[links[l].rep]).collect();
    let kind = match bracket(&reps, gap_max) {
        Some((steps, range)) => {
            reason.ev_steps = Some(steps);
            reason.ev_range = Some(range);
            Kind::Bracket
        }
        None => Kind::Series,
    };
    // cover: the photo the user already rated best, otherwise the middle exposure of a bracket (its brightest or
    // darkest frame says little about the scene), otherwise the first one
    let middle = || {
        if kind != Kind::Bracket {
            return None;
        }
        let mut by_ev: Vec<(f64, i64)> = reps.iter().filter_map(|it| it.ev().map(|ev| (ev, it.image))).collect();
        if by_ev.len() != reps.len() {
            return None;
        }
        by_ev.sort_by(|a, b| a.0.total_cmp(&b.0));
        Some(by_ev[(by_ev.len() - 1) / 2].1)
    };
    let cover = part
        .iter()
        .flat_map(|&l| links[l].items.iter())
        .map(|&i| &items[i])
        .max_by_key(|it| (it.rank, std::cmp::Reverse(it.secs)))
        .filter(|it| it.rank > 0)
        .map(|it| it.image)
        .or_else(middle)
        .unwrap_or(images[0]);
    Group { images, kind, reason, cover }
}

/// Same aperture: both known and equal, or both unknown (a lens without electronic contacts writes none; the frames
/// of one burst share it).
fn same_aperture(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() <= 0.05,
        (None, None) => true,
        _ => false,
    }
}

/// Apertures of two neighbouring frames of a bracket: the same, or both known and the exposure time changing as well
/// (a bracket in program mode shifts aperture and time together: Panasonic GX7, f/3.2 1/400 → f/2.8 1/250 →
/// f/2.2 1/200). A change of the aperture alone (time fixed) is no bracket.
fn bracket_apertures(a: &Item, b: &Item) -> bool {
    if same_aperture(a.aperture, b.aperture) {
        return true;
    }
    match (a.aperture, b.aperture, a.exposure, b.exposure) {
        (Some(_), Some(_), Some(ta), Some(tb)) if ta > 0.0 && tb > 0.0 => (ta / tb).log2().abs() >= 0.2,
        _ => false,
    }
}

/// Exposure bracket: aperture fixed (or moving with the time, [`bracket_apertures`]), every photo at least 0.3 EV
/// away from the one before, at least two distinct steps spanning ≥ 0.6 EV, all gaps ≤ 2 s (a bracket set by hand:
/// every step a [`manual_bracket_step`]). Returns (steps, range in EV).
fn bracket(reps: &[&Item], gap_max: i64) -> Option<(u32, f32)> {
    if reps.len() < 2 {
        return None;
    }
    if gap_max > 2 && !reps.windows(2).all(|w| manual_bracket_step(w[0], w[1], (w[1].secs - w[0].secs).abs())) {
        return None;
    }
    if reps.windows(2).any(|w| !bracket_apertures(w[0], w[1])) {
        return None;
    }
    let mut evs = Vec::new();
    for r in reps {
        evs.push(r.ev()?);
    }
    if evs.windows(2).any(|w| (w[1] - w[0]).abs() < 0.3) {
        return None;
    }
    let mut steps: Vec<f64> = Vec::new();
    for v in &evs {
        if !steps.iter().any(|s| (s - v).abs() < 0.3) {
            steps.push(*v);
        }
    }
    let lo = evs.iter().copied().fold(f64::MAX, f64::min);
    let hi = evs.iter().copied().fold(f64::MIN, f64::max);
    (steps.len() >= 2 && hi - lo >= 0.6).then_some((steps.len() as u32, (hi - lo) as f32))
}

// ───────────────────────────────────────────── grouping in the library

/// Result of a grouping run.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub days: usize,
    pub regrouped_days: usize,
    pub stacks_created: usize,
    pub stacks_removed: usize,
    pub stacks_kept: usize,
    /// days left for later: shooting data or previews of their photos are not there yet ([`regroup_ready`])
    pub waiting_days: usize,
    pub ms: u128,
}

/// Day and camera of the photos to group (originals with capture time, not in a fixed stack).
struct DayItems {
    items: Vec<Item>,
    /// files with a signature / with a CLIP vector (part of the fingerprint)
    sig: HashSet<i64>,
    vec: HashSet<i64>,
    /// some photo's shooting data has not been read yet (`file.exif = 0`): grouping now would split brackets
    unread: bool,
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= *b as u64;
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

fn fingerprint(d: &DayItems) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut v: Vec<&Item> = d.items.iter().collect();
    v.sort_by_key(|i| i.image);
    for i in v {
        fnv(&mut h, &i.image.to_le_bytes());
        fnv(&mut h, &i.secs.to_le_bytes());
        fnv(&mut h, &i.folder.to_le_bytes());
        fnv(&mut h, i.camera.as_bytes());
        fnv(&mut h, &[d.sig.contains(&i.file) as u8, d.vec.contains(&i.file) as u8]);
        // the shooting data is read after the import, often after a first grouping: once it is there the day is
        // grouped again (an exposure bracket is recognized only with it; without, it fell apart into small series)
        fnv(&mut h, &[i.aperture.is_some() as u8, i.exposure.is_some() as u8]);
    }
    fnv(&mut h, &ALGO.to_le_bytes());
    h as i64
}

/// Images in stacks the grouping must not touch (anything except untouched automatic series).
fn fixed_images(conn: &Connection) -> R<HashSet<i64>> {
    let mut st = conn.prepare("SELECT si.image FROM stack_image si JOIN stack s ON s.id = si.stack WHERE s.origin <> 'auto' OR s.reviewed IS NOT NULL OR si.pinned = 1").map_err(e)?;
    let v = st.query_map([], |r| r.get::<_, i64>(0)).map_err(e)?.collect::<Result<HashSet<_>, _>>().map_err(e);
    v
}

fn load_days(conn: &Connection) -> R<BTreeMap<String, DayItems>> {
    let fixed = fixed_images(conn)?;
    let mut st = conn
        .prepare(
            "SELECT b.id, d.id, d.folder, d.name, d.format, coalesce(d.camera, ''), d.capture_time, d.aperture, d.time, d.iso,
                    b.stars + 10 * (b.flag = 1), q.signature IS NOT NULL, v.file IS NOT NULL, d.exif = 0 AND d.status = 0
             FROM image b JOIN file d ON d.id = b.file
             LEFT JOIN image_quality q ON q.file = d.id
             LEFT JOIN image_vector v ON v.file = d.id
             WHERE b.copy IS NULL AND d.status NOT IN (3, 4) AND d.capture_time IS NOT NULL AND d.format <> 'VIDEO'",
        )
        .map_err(e)?;
    let mut days: BTreeMap<String, DayItems> = BTreeMap::new();
    let mut rows = st.query([]).map_err(e)?;
    while let Some(r) = rows.next().map_err(e)? {
        let image: i64 = r.get(0).map_err(e)?;
        if fixed.contains(&image) {
            continue;
        }
        let capture: String = r.get(6).map_err(e)?;
        let Some(secs) = crate::pairs::seconds(&capture) else { continue };
        let name: String = r.get(3).map_err(e)?;
        let format: String = r.get(4).map_err(e)?;
        let file: i64 = r.get(1).map_err(e)?;
        let it = Item {
            image,
            file,
            folder: r.get(2).map_err(e)?,
            stem: crate::pairs::stem(&name),
            raw: crate::is_raw(&format),
            sfd: format == "X3I",
            camera: r.get(5).map_err(e)?,
            secs,
            aperture: r.get(7).map_err(e)?,
            exposure: r.get(8).map_err(e)?,
            iso: r.get(9).map_err(e)?,
            rank: r.get::<_, i64>(10).map_err(e)? as i32,
        };
        let d = days.entry(capture[..10].to_string()).or_insert_with(|| DayItems { items: Vec::new(), sig: HashSet::new(), vec: HashSet::new(), unread: false });
        d.unread |= r.get::<_, bool>(13).map_err(e)?;
        if r.get::<_, bool>(11).map_err(e)? {
            d.sig.insert(file);
        }
        if r.get::<_, bool>(12).map_err(e)? {
            d.vec.insert(file);
        }
        d.items.push(it);
    }
    Ok(days)
}

fn blob_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Signatures and CLIP vectors of the given files (zero vectors – failed computations – left out).
fn load_similarity_data(conn: &Connection, files: &[i64]) -> R<(HashMap<i64, Vec<u8>>, HashMap<i64, Vec<f32>>)> {
    let mut sigs = HashMap::new();
    let mut vecs = HashMap::new();
    let mut qs = conn.prepare_cached("SELECT signature FROM image_quality WHERE file = ?1 AND signature IS NOT NULL").map_err(e)?;
    let mut qv = conn.prepare_cached("SELECT vec FROM image_vector WHERE file = ?1").map_err(e)?;
    for &f in files {
        if let Some(s) = qs.query_row([f], |r| r.get::<_, Vec<u8>>(0)).optional().map_err(e)? {
            if s.len() == SIGNATURE_LEN {
                sigs.insert(f, s);
            }
        }
        if let Some(b) = qv.query_row([f], |r| r.get::<_, Vec<u8>>(0)).optional().map_err(e)? {
            let v = blob_f32(&b);
            if v.len() == 512 && v.iter().any(|x| x.abs() > 1e-6) {
                vecs.insert(f, v);
            }
        }
    }
    Ok((sigs, vecs))
}

/// Do neighbouring photos of `files` (in this order) look like frames of one exposure bracket? Each pair is compared
/// with the similarity the series use (CLIP or image signature), with the slack of wide brackets (the darkest and
/// the brightest frame look different). `None`: no similarity known for any pair (no signatures yet) – nothing
/// speaks against it. Reads a few rows, no image is decoded.
pub fn bracket_alike(conn: &Connection, files: &[i64]) -> R<Option<bool>> {
    let (sigs, vecs) = load_similarity_data(conn, files)?;
    let sim = similarity_from(&sigs, &vecs);
    let mut known = false;
    for w in files.windows(2) {
        if let Some((v, b)) = sim(w[0], w[1]) {
            known = true;
            if v < thresholds(b).join - BRACKET_SIM_SLACK {
                return Ok(Some(false));
            }
        }
    }
    Ok(known.then_some(true))
}

/// Similarity function over loaded data: CLIP when both files have a vector, otherwise the signature.
pub fn similarity_from<'a>(sigs: &'a HashMap<i64, Vec<u8>>, vecs: &'a HashMap<i64, Vec<f32>>) -> impl Fn(i64, i64) -> Option<(f32, Basis)> + 'a {
    move |a, b| {
        if let (Some(x), Some(y)) = (vecs.get(&a), vecs.get(&b)) {
            return Some((x.iter().zip(y).map(|(p, q)| p * q).sum(), Basis::Clip));
        }
        signature_similarity(sigs.get(&a)?, sigs.get(&b)?).map(|s| (s, Basis::Signature))
    }
}

fn never_pairs(conn: &Connection, images: &[i64]) -> R<HashSet<(i64, i64)>> {
    let set: HashSet<i64> = images.iter().copied().collect();
    let mut out = HashSet::new();
    let mut st = conn.prepare_cached("SELECT a, b FROM stack_never WHERE a = ?1 OR b = ?1").map_err(e)?;
    for &i in images {
        for p in st.query_map([i], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(e)? {
            let (a, b) = p.map_err(e)?;
            if set.contains(&a) && set.contains(&b) {
                out.insert((a.min(b), a.max(b)));
            }
        }
    }
    Ok(out)
}

/// How new automatic series start: collapsed or open (setting "stackCollapse": "reviewed" (default: open until
/// reviewed), "always", "never").
pub fn new_series_collapsed() -> bool {
    crate::settings::value("stackCollapse").as_str() == Some("always")
}

/// Write the groups of one day: untouched automatic stacks of the day (or containing its photos) that match a new
/// group exactly are kept as they are (collapsed state, id); the others are replaced. One short transaction.
fn write_day(conn: &Connection, day: &str, groups: &[Group], images: &HashSet<i64>, print: i64, stats: &mut Stats) -> R<()> {
    let tx = conn.unchecked_transaction().map_err(e)?;
    // automatic stacks of this day or holding one of its photos (time corrected from another day)
    let mut old: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut st = tx.prepare_cached("SELECT id FROM stack WHERE origin = 'auto' AND reviewed IS NULL AND day = ?1").map_err(e)?;
        for id in st.query_map([day], |r| r.get::<_, i64>(0)).map_err(e)? {
            old.insert(id.map_err(e)?, Vec::new());
        }
        let mut sm = tx.prepare_cached("SELECT s.id FROM stack_image si JOIN stack s ON s.id = si.stack WHERE si.image = ?1 AND s.origin = 'auto' AND s.reviewed IS NULL").map_err(e)?;
        for &i in images {
            if let Some(id) = sm.query_row([i], |r| r.get::<_, i64>(0)).optional().map_err(e)? {
                old.entry(id).or_default();
            }
        }
        let mut mem = tx.prepare_cached("SELECT image FROM stack_image WHERE stack = ?1 ORDER BY position").map_err(e)?;
        for (id, m) in old.iter_mut() {
            *m = mem.query_map([*id], |r| r.get::<_, i64>(0)).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
        }
    }
    let mut by_members: HashMap<Vec<i64>, i64> = old.iter().map(|(id, m)| (m.clone(), *id)).collect();
    let collapsed = new_series_collapsed();
    let now = crate::now();
    for g in groups {
        if let Some(id) = by_members.remove(&g.images) {
            // same photos: keep the stack (its collapsed state); refresh the explanation, and the cover while the
            // grouping chose it (a new version picks another one: the middle exposure of a bracket)
            tx.execute(
                "UPDATE stack SET reason = ?2, kind = ?3, day = ?4, algo = ?5, cover = CASE WHEN coalesce(cover_by, 'auto') = 'auto' THEN ?6 ELSE cover END WHERE id = ?1",
                params![id, g.reason.to_json(), g.kind.as_str(), day, ALGO, g.cover],
            )
            .map_err(e)?;
            old.remove(&id);
            stats.stacks_kept += 1;
            continue;
        }
        // members leave their previous automatic stack first (one stack per photo)
        for id in g.images.iter() {
            tx.execute("DELETE FROM stack_image WHERE image = ?1 AND stack IN (SELECT id FROM stack WHERE origin = 'auto' AND reviewed IS NULL)", [id]).map_err(e)?;
        }
        tx.execute(
            "INSERT INTO stack(kind, origin, cover, cover_by, collapsed, reason, algo, day, created) VALUES(?1, 'auto', ?2, 'auto', ?3, ?4, ?5, ?6, ?7)",
            params![g.kind.as_str(), g.cover, collapsed, g.reason.to_json(), ALGO, day, now],
        )
        .map_err(e)?;
        let id = tx.last_insert_rowid();
        let mut ins = tx.prepare_cached("INSERT OR REPLACE INTO stack_image(image, stack, position) VALUES(?1, ?2, ?3)").map_err(e)?;
        for (p, img) in g.images.iter().enumerate() {
            ins.execute(params![img, id, p as i64 + 1]).map_err(e)?;
        }
        stats.stacks_created += 1;
    }
    for id in old.keys() {
        tx.execute("DELETE FROM stack WHERE id = ?1", [id]).map_err(e)?;
        stats.stacks_removed += 1;
    }
    tx.execute(
        "INSERT INTO stack_day_state(day, fingerprint, algo) VALUES(?1, ?2, ?3) ON CONFLICT(day) DO UPDATE SET fingerprint = excluded.fingerprint, algo = excluded.algo",
        params![day, print, ALGO],
    )
    .map_err(e)?;
    tx.commit().map_err(e)
}

/// Members of stacks the library shows: photos whose file is not hidden for good (status 4, deleted or hidden in
/// Apple Photos, never shown in the library).
const SHOWN_MEMBERS: &str = "SELECT si.stack, si.image FROM stack_image si JOIN image b ON b.id = si.image JOIN file d ON d.id = b.file WHERE d.status <> 4";

/// Remove stacks with fewer than two shown photos (photos deleted, taken out, hidden for good). Any origin, own
/// stacks too: a stack of one photo has no cover, order or members to fold – it would only put a stack mark on a
/// single photo (⌘G and the Lightroom import refuse fewer than two photos for the same reason).
pub fn remove_broken(conn: &Connection) -> R<usize> {
    conn.execute(&format!("DELETE FROM stack WHERE id NOT IN (SELECT stack FROM ({SHOWN_MEMBERS}) GROUP BY stack HAVING count(*) >= 2)"), []).map_err(e)
}

/// Group the library: only days whose photos changed since the last run (or all with `force`), one short
/// transaction per day. `cancel` is asked between days. Fixed stacks (user, Lightroom, reviewed) are never changed.
pub fn regroup(db: &Db, force: bool, cancel: &dyn Fn() -> bool) -> R<Stats> {
    regroup_with_progress(db, force, cancel, &|_, _| {})
}

/// [`regroup`] that tells its progress: capture days looked at, days in total.
pub fn regroup_with_progress(db: &Db, force: bool, cancel: &dyn Fn() -> bool, progress: &dyn Fn(usize, usize)) -> R<Stats> {
    regroup_ready(db, force, cancel, progress, None)
}

/// [`regroup_with_progress`] that leaves days for later whose photos are not complete yet: shooting data not read
/// (`file.exif = 0`) or a file in `waiting` (its preview, and so its signature, is not there yet). Grouped without
/// them, a bracket fell apart into pieces (QA 1.5.0-test.5: garden 2 + 2 + 2, sunset 4 + 3 for two minutes after an
/// import). `None`: group every day as it is (the caller gave up waiting).
pub fn regroup_ready(db: &Db, force: bool, cancel: &dyn Fn() -> bool, progress: &dyn Fn(usize, usize), waiting: Option<&HashSet<i64>>) -> R<Stats> {
    let t = std::time::Instant::now();
    let conn = &db.conn;
    let days = load_days(conn)?;
    let mut stats = Stats { days: days.len(), ..Stats::default() };
    progress(0, days.len());
    let known: HashMap<String, (i64, i64)> = {
        let mut st = conn.prepare("SELECT day, fingerprint, algo FROM stack_day_state").map_err(e)?;
        let v = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
        v
    };
    for (k, (day, d)) in days.iter().enumerate() {
        if cancel() {
            break;
        }
        progress(k, days.len());
        let print = fingerprint(d);
        if !force && known.get(day) == Some(&(print, ALGO)) {
            continue;
        }
        if let Some(w) = waiting {
            if d.unread || d.items.iter().any(|i| w.contains(&i.file)) {
                stats.waiting_days += 1;
                continue;
            }
        }
        let files: Vec<i64> = d.items.iter().map(|i| i.file).collect();
        let images: Vec<i64> = d.items.iter().map(|i| i.image).collect();
        let (sigs, vecs) = load_similarity_data(conn, &files)?;
        let never = never_pairs(conn, &images)?;
        let sim = similarity_from(&sigs, &vecs);
        let groups = group(&d.items, &sim, &never);
        write_day(conn, day, &groups, &images.iter().copied().collect(), print, &mut stats)?;
        stats.regrouped_days += 1;
    }
    // days without photos any more: their automatic stacks go
    if !cancel() {
        for day in known.keys().filter(|d| !days.contains_key(*d)) {
            conn.execute("DELETE FROM stack WHERE origin = 'auto' AND reviewed IS NULL AND day = ?1", [day]).map_err(e)?;
            conn.execute("DELETE FROM stack_day_state WHERE day = ?1", [day]).map_err(e)?;
        }
        stats.stacks_removed += remove_broken(conn)?;
    }
    stats.ms = t.elapsed().as_millis();
    Ok(stats)
}

/// Forget the grouping state of the days of these images, so the next run groups them again (after the user
/// split or dissolved a stack).
pub fn invalidate_days_of(conn: &Connection, images: &[i64]) -> R<()> {
    let mut st = conn.prepare_cached("DELETE FROM stack_day_state WHERE day = (SELECT substr(d.capture_time, 1, 10) FROM image b JOIN file d ON d.id = b.file WHERE b.id = ?1)").map_err(e)?;
    for i in images {
        st.execute([i]).map_err(e)?;
    }
    Ok(())
}

// ───────────────────────────────────────────── signatures (background)

/// A file that needs an image signature (the preview key is built from path, size, time and orientation).
#[derive(Clone, Debug)]
pub struct SignatureTask {
    pub file: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub orientation: Option<i64>,
}

/// Files that need an image signature: originals with capture time (series candidates) without one, newest first.
pub fn files_without_signature(db: &Db) -> Vec<SignatureTask> {
    let Ok(mut st) = db.conn.prepare(
        "SELECT d.id, s.path || o.path || d.name, coalesce(d.size, 0), coalesce(d.mtime, 0), d.orientation
         FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
         LEFT JOIN image_quality q ON q.file = d.id
         WHERE d.capture_time IS NOT NULL AND d.status NOT IN (3, 4) AND d.format <> 'VIDEO' AND q.signature IS NULL
         ORDER BY d.capture_time DESC",
    ) else {
        return Vec::new();
    };
    st.query_map([], |r| Ok(SignatureTask { file: r.get(0)?, path: r.get(1)?, size: r.get(2)?, mtime: r.get(3)?, orientation: r.get(4)? }))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// Store the image signature of a file.
pub fn save_signature(db: &Db, file: i64, sig: &[u8]) -> R<()> {
    db.conn
        .execute(
            "INSERT INTO image_quality(file, version, signature, computed) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(file) DO UPDATE SET signature = excluded.signature, computed = excluded.computed",
            params![file, SIGNATURE_VERSION, sig, crate::now()],
        )
        .map(|_| ())
        .map_err(e)
}

// ───────────────────────────────────────────── reading stacks (grid)

/// A stack as the grid needs it.
#[derive(Clone, Debug)]
pub struct Stack {
    pub id: i64,
    pub kind: Kind,
    pub origin: String,
    pub cover: Option<i64>,
    pub cover_by_user: bool,
    pub collapsed: bool,
    pub reviewed: Option<i64>,
    pub reason: Reason,
    pub day: Option<String>,
    /// images in stack order
    pub images: Vec<i64>,
}

impl Stack {
    /// Automatic and not yet touched by the user ("Serie" in the interface); else a "Stapel".
    pub fn is_series(&self) -> bool {
        self.kind != Kind::Manual
    }
    /// The photo on top: the cover if it is still a member, otherwise the first one.
    pub fn top(&self) -> i64 {
        self.cover.filter(|c| self.images.contains(c)).unwrap_or(self.images[0])
    }
}

/// All stacks with their members (one query each; ~25,000 rows for a large library, a few milliseconds).
pub fn load(conn: &Connection) -> R<Vec<Stack>> {
    // automatic series of an older grouping version are not shown: the next run regroups them (shown until then,
    // they put photos together the current rules keep apart, QA 1.5.0-test.5: an X3I with the frames of another folder)
    let mut st = conn
        .prepare(&format!("SELECT id, kind, origin, cover, cover_by, collapsed, reviewed, reason, day FROM stack WHERE NOT (origin = 'auto' AND reviewed IS NULL AND algo < {ALGO})"))
        .map_err(e)?;
    let mut stacks: HashMap<i64, Stack> = st
        .query_map([], |r| {
            Ok(Stack {
                id: r.get(0)?,
                kind: Kind::parse(&r.get::<_, String>(1)?),
                origin: r.get(2)?,
                cover: r.get(3)?,
                cover_by_user: r.get::<_, Option<String>>(4)?.as_deref() == Some("user"),
                collapsed: r.get::<_, i64>(5)? != 0,
                reviewed: r.get(6)?,
                reason: r.get::<_, Option<String>>(7)?.map(|s| Reason::from_json(&s)).unwrap_or_default(),
                day: r.get(8)?,
                images: Vec::new(),
            })
        })
        .map_err(e)?
        .map(|s| s.map(|s| (s.id, s)))
        .collect::<Result<_, _>>()
        .map_err(e)?;
    // members hidden for good are left out: the stack is what the library can show (fewer than two: no stack)
    let mut st = conn.prepare(&format!("{SHOWN_MEMBERS} ORDER BY si.stack, si.position")).map_err(e)?;
    let mut rows = st.query([]).map_err(e)?;
    while let Some(r) = rows.next().map_err(e)? {
        if let Some(s) = stacks.get_mut(&r.get::<_, i64>(0).map_err(e)?) {
            s.images.push(r.get(1).map_err(e)?);
        }
    }
    let mut v: Vec<Stack> = stacks.into_values().filter(|s| s.images.len() >= 2).collect();
    v.sort_by_key(|s| s.id);
    Ok(v)
}

/// One stack by id.
pub fn get(conn: &Connection, id: i64) -> R<Option<Stack>> {
    let s = conn
        .query_row("SELECT id, kind, origin, cover, cover_by, collapsed, reviewed, reason, day FROM stack WHERE id = ?1", [id], |r| {
            Ok(Stack {
                id: r.get(0)?,
                kind: Kind::parse(&r.get::<_, String>(1)?),
                origin: r.get(2)?,
                cover: r.get(3)?,
                cover_by_user: r.get::<_, Option<String>>(4)?.as_deref() == Some("user"),
                collapsed: r.get::<_, i64>(5)? != 0,
                reviewed: r.get(6)?,
                reason: r.get::<_, Option<String>>(7)?.map(|s| Reason::from_json(&s)).unwrap_or_default(),
                day: r.get(8)?,
                images: Vec::new(),
            })
        })
        .optional()
        .map_err(e)?;
    let Some(mut s) = s else { return Ok(None) };
    let mut st = conn.prepare_cached("SELECT image FROM stack_image WHERE stack = ?1 ORDER BY position").map_err(e)?;
    s.images = st.query_map([id], |r| r.get(0)).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
    Ok(Some(s))
}

/// Stack of an image, if any.
pub fn stack_of(conn: &Connection, image: i64) -> R<Option<i64>> {
    conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [image], |r| r.get(0)).optional().map_err(e)
}

/// Number of series not reviewed yet (left panel "Serien · 86 offen").
pub fn open_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM stack s WHERE s.reviewed IS NULL AND s.kind <> 'manual' AND (SELECT count(*) FROM stack_image si WHERE si.stack = s.id) >= 2",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

// ───────────────────────────────────────────── changes by the user

/// State of one stack before a change, to undo it exactly (⌘Z).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// stacks as they were (row values and members with position and pin); `None` member list = did not exist
    stacks: Vec<(i64, Option<StackRow>)>,
    /// images whose membership the change touched, with their stack before (None = in no stack)
    members: Vec<(i64, Option<(i64, i64, i64)>)>,
    /// never pairs added by the change
    never_added: Vec<(i64, i64)>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StackRow {
    kind: String,
    origin: String,
    cover: Option<i64>,
    cover_by: Option<String>,
    collapsed: i64,
    reviewed: Option<i64>,
    reason: Option<String>,
    algo: i64,
    day: Option<String>,
    lr_id: Option<i64>,
    created: i64,
}

fn read_row(conn: &Connection, id: i64) -> R<Option<StackRow>> {
    conn.query_row("SELECT kind, origin, cover, cover_by, collapsed, reviewed, reason, algo, day, lr_id, created FROM stack WHERE id = ?1", [id], |r| {
        Ok(StackRow {
            kind: r.get(0)?,
            origin: r.get(1)?,
            cover: r.get(2)?,
            cover_by: r.get(3)?,
            collapsed: r.get(4)?,
            reviewed: r.get(5)?,
            reason: r.get(6)?,
            algo: r.get(7)?,
            day: r.get(8)?,
            lr_id: r.get(9)?,
            created: r.get(10)?,
        })
    })
    .optional()
    .map_err(e)
}

fn membership(conn: &Connection, image: i64) -> R<Option<(i64, i64, i64)>> {
    conn.query_row("SELECT stack, position, pinned FROM stack_image WHERE image = ?1", [image], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(e)
}

fn members_of(conn: &Connection, stack: i64) -> R<Vec<i64>> {
    let mut st = conn.prepare_cached("SELECT image FROM stack_image WHERE stack = ?1 ORDER BY position").map_err(e)?;
    let v = st.query_map([stack], |r| r.get(0)).map_err(e)?.collect::<Result<_, _>>().map_err(e);
    v
}

/// Record the state of the given stacks and images before a change.
fn snapshot(conn: &Connection, stacks: &[i64], images: &[i64]) -> R<Snapshot> {
    let mut s = Snapshot { stacks: Vec::new(), members: Vec::new(), never_added: Vec::new() };
    let mut imgs: Vec<i64> = images.to_vec();
    for &id in stacks {
        s.stacks.push((id, read_row(conn, id)?));
        imgs.extend(members_of(conn, id)?);
    }
    imgs.sort_unstable();
    imgs.dedup();
    for i in imgs {
        s.members.push((i, membership(conn, i)?));
    }
    Ok(s)
}

/// Undo a change: stacks, memberships and added never pairs as before. Stacks created by the change (not in the
/// snapshot) are removed by the caller through [`Change::created`].
///
/// Stack ids can be used again (`INTEGER PRIMARY KEY`): a stack is only taken as "the same" when its creation time
/// and kind match the snapshot. A stack that got the id meanwhile (e.g. from the background grouping) is never
/// changed or deleted: the old stack comes back under a new id, its photos follow it.
pub fn restore(db: &Db, c: &Change) -> R<()> {
    let tx = begin(db)?;
    restore_in(&tx, c)?;
    tx.commit().map_err(e)
}

/// [`restore`] inside a transaction the caller holds (and commits).
pub fn restore_in(tx: &Connection, c: &Change) -> R<()> {
    let same = |id: i64, created: Option<i64>, kind: Option<&str>| -> R<bool> {
        Ok(read_row(tx, id)?.is_some_and(|r| created.is_none_or(|t| r.created == t) && kind.is_none_or(|k| r.kind == k)))
    };
    for (id, created) in c.created.iter().zip(c.created_at.iter().map(Some).chain(std::iter::repeat(None))) {
        // only the stack this change made (created by the user; an id taken again by the grouping stays)
        if same(*id, created.copied(), None)? {
            tx.execute("DELETE FROM stack WHERE id = ?1 AND origin = 'user'", [id]).map_err(e)?;
        }
    }
    let mut moved: HashMap<i64, i64> = HashMap::new();
    for (id, row) in &c.before.stacks {
        match row {
            Some(r) => {
                let taken = read_row(&tx, *id)?.is_some_and(|now| now.created != r.created || now.kind != r.kind);
                if taken {
                    tx.execute(
                        "INSERT INTO stack(kind, origin, cover, cover_by, collapsed, reviewed, reason, algo, day, lr_id, created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                        params![r.kind, r.origin, r.cover, r.cover_by, r.collapsed, r.reviewed, r.reason, r.algo, r.day, r.lr_id.filter(|l| !lr_id_used(&tx, *l)), r.created],
                    )
                    .map_err(e)?;
                    moved.insert(*id, tx.last_insert_rowid());
                    continue;
                }
                tx.execute(
                    "INSERT INTO stack(id, kind, origin, cover, cover_by, collapsed, reviewed, reason, algo, day, lr_id, created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                     ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, origin=excluded.origin, cover=excluded.cover, cover_by=excluded.cover_by, collapsed=excluded.collapsed,
                       reviewed=excluded.reviewed, reason=excluded.reason, algo=excluded.algo, day=excluded.day, lr_id=excluded.lr_id, created=excluded.created",
                    params![id, r.kind, r.origin, r.cover, r.cover_by, r.collapsed, r.reviewed, r.reason, r.algo, r.day, r.lr_id, r.created],
                )
                .map_err(e)?;
            }
            None => {
                // did not exist before: a stack with this id now is another one (never deleted here)
            }
        }
    }
    for (img, m) in &c.before.members {
        tx.execute("DELETE FROM stack_image WHERE image = ?1", [img]).map_err(e)?;
        if let Some((st, pos, pin)) = m {
            let st = moved.get(st).unwrap_or(st);
            tx.execute("INSERT INTO stack_image(image, stack, position, pinned) SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM stack WHERE id = ?2)", params![img, st, pos, pin]).map_err(e)?;
        }
    }
    for (a, b) in &c.before.never_added {
        tx.execute("DELETE FROM stack_never WHERE a = ?1 AND b = ?2", params![a, b]).map_err(e)?;
    }
    Ok(())
}

/// A change to stacks: what is needed to undo it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Change {
    before: Snapshot,
    /// stacks the change created
    pub created: Vec<i64>,
    /// their creation times (parallel to `created`): ⌘Z deletes only these stacks, not one that got the id later
    created_at: Vec<i64>,
}

impl Change {
    fn new(before: Snapshot) -> Self {
        Change { before, created: Vec::new(), created_at: Vec::new() }
    }
    fn with_created(before: Snapshot, conn: &Connection, ids: &[i64]) -> R<Self> {
        let mut c = Change::new(before);
        for &id in ids {
            c.created.push(id);
            c.created_at.push(read_row(conn, id)?.map_or(0, |r| r.created));
        }
        Ok(c)
    }
}

/// A short write transaction that takes the write lock at once: the snapshot for ⌘Z and the change see the same state.
fn begin(db: &Db) -> R<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)
}

fn lr_id_used(conn: &Connection, lr_id: i64) -> bool {
    conn.query_row("SELECT EXISTS (SELECT 1 FROM stack WHERE lr_id = ?1)", [lr_id], |r| r.get(0)).unwrap_or(true)
}

fn add_never(tx: &Connection, a: &[i64], b: &[i64], added: &mut Vec<(i64, i64)>) -> R<()> {
    let mut st = tx.prepare_cached("INSERT OR IGNORE INTO stack_never(a, b) VALUES(?1, ?2)").map_err(e)?;
    for &x in a {
        for &y in b {
            if x != y {
                let p = (x.min(y), x.max(y));
                if st.execute(params![p.0, p.1]).map_err(e)? > 0 {
                    added.push(p);
                }
            }
        }
    }
    Ok(())
}

/// The stack becomes the user's: the background grouping and a new Lightroom import leave it alone from now on.
fn make_user(tx: &Connection, stack: i64) -> R<()> {
    tx.execute("UPDATE stack SET origin = 'user' WHERE id = ?1 AND origin IN ('auto', 'lightroom')", [stack]).map_err(e)?;
    Ok(())
}

fn renumber(tx: &Connection, stack: i64) -> R<()> {
    let m = members_of(tx, stack)?;
    let mut st = tx.prepare_cached("UPDATE stack_image SET position = ?2 WHERE image = ?1").map_err(e)?;
    for (p, i) in m.iter().enumerate() {
        st.execute(params![i, p as i64 + 1]).map_err(e)?;
    }
    Ok(())
}

/// Bundle photos into one own stack (⌘G); `cover` on top, the others in the given order. Photos leave their
/// previous stacks. Returns the change (for ⌘Z) and the id of the new stack.
pub fn create(db: &Db, images: &[i64], cover: i64) -> R<(Change, i64)> {
    let tx = begin(db)?;
    let r = create_in(&tx, images, cover)?;
    tx.commit().map_err(e)?;
    Ok(r)
}

/// [`create`] inside a transaction the caller holds (and commits).
pub fn create_in(tx: &Connection, images: &[i64], cover: i64) -> R<(Change, i64)> {
    if images.len() < 2 {
        return Err("mindestens zwei Fotos".into());
    }
    let olds: Vec<i64> = images.iter().filter_map(|&i| stack_of(tx, i).ok().flatten()).collect::<HashSet<_>>().into_iter().collect();
    let before = snapshot(tx, &olds, images)?;
    let mut order: Vec<i64> = vec![cover];
    order.extend(images.iter().copied().filter(|&i| i != cover));
    for i in &order {
        tx.execute("DELETE FROM stack_image WHERE image = ?1", [i]).map_err(e)?;
    }
    for &o in &olds {
        make_user(tx, o)?;
        renumber(tx, o)?;
    }
    tx.execute(
        "INSERT INTO stack(kind, origin, cover, cover_by, collapsed, algo, day, created) VALUES('manual', 'user', ?1, 'user', 1, ?2, NULL, ?3)",
        params![cover, ALGO, crate::now()],
    )
    .map_err(e)?;
    let id = tx.last_insert_rowid();
    for (p, i) in order.iter().enumerate() {
        tx.execute("INSERT INTO stack_image(image, stack, position, pinned) VALUES(?1, ?2, ?3, 1)", params![i, id, p as i64 + 1]).map_err(e)?;
    }
    remove_broken(tx)?;
    let change = Change::with_created(before, tx, &[id])?;
    Ok((change, id))
}

/// Put `image` on top of an existing stack as its cover (a merged photo joining the bracket of its sources). The stack
/// keeps its kind (a detected series or bracket stays one) and becomes the user's, so the background grouping leaves
/// it alone; `image` leaves its previous stack. Returns the change (for ⌘Z).
pub fn join(db: &Db, stack: i64, image: i64) -> R<Change> {
    let tx = begin(db)?;
    let c = join_in(&tx, stack, image)?;
    tx.commit().map_err(e)?;
    Ok(c)
}

/// [`join`] inside a transaction the caller holds (and commits).
pub fn join_in(tx: &Connection, stack: i64, image: i64) -> R<Change> {
    if read_row(tx, stack)?.is_none() {
        return Err("Stapel fehlt".into());
    }
    let old = stack_of(tx, image)?.filter(|o| *o != stack);
    let stacks: Vec<i64> = std::iter::once(stack).chain(old).collect();
    let before = snapshot(tx, &stacks, &[image])?;
    tx.execute("DELETE FROM stack_image WHERE image = ?1", [image]).map_err(e)?;
    if let Some(o) = old {
        make_user(tx, o)?;
        renumber(tx, o)?;
    }
    make_user(tx, stack)?;
    tx.execute("UPDATE stack_image SET position = position + 1 WHERE stack = ?1", [stack]).map_err(e)?;
    tx.execute("INSERT INTO stack_image(image, stack, position, pinned) VALUES(?1, ?2, 1, 1)", params![image, stack]).map_err(e)?;
    tx.execute("UPDATE stack SET cover = ?2, cover_by = 'user' WHERE id = ?1", params![stack, image]).map_err(e)?;
    remove_broken(tx)?;
    let change = Change::with_created(before, tx, &[])?;
    Ok(change)
}

/// Dissolve a stack (⇧⌘G): its photos stand alone again and are never grouped together automatically.
pub fn dissolve(db: &Db, stack: i64) -> R<Change> {
    let tx = begin(db)?;
    let mut before = snapshot(&tx, &[stack], &[])?;
    let m = members_of(&tx, stack)?;
    // at most the first 200 photos remember each other (a stack of 200 already gives ~20,000 pairs)
    let k = &m[..m.len().min(200)];
    add_never(&tx, k, k, &mut before.never_added)?;
    tx.execute("DELETE FROM stack WHERE id = ?1", [stack]).map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(Change::new(before))
}

/// Take one photo out of its stack; it is never grouped with the others again.
pub fn take_out(db: &Db, image: i64) -> R<Option<Change>> {
    let tx = begin(db)?;
    let Some(stack) = stack_of(&tx, image)? else { return Ok(None) };
    let mut before = snapshot(&tx, &[stack], &[image])?;
    let rest: Vec<i64> = members_of(&tx, stack)?.into_iter().filter(|&i| i != image).collect();
    add_never(&tx, &[image], &rest, &mut before.never_added)?;
    tx.execute("DELETE FROM stack_image WHERE image = ?1", [image]).map_err(e)?;
    tx.execute("UPDATE stack SET cover = NULL, cover_by = NULL WHERE id = ?1 AND cover = ?2", params![stack, image]).map_err(e)?;
    make_user(&tx, stack)?;
    renumber(&tx, stack)?;
    remove_broken(&tx)?;
    tx.commit().map_err(e)?;
    Ok(Some(Change::new(before)))
}

/// Split a stack before `image`: it and the photos after it form a stack of their own; the two parts are never
/// grouped together again.
pub fn split_at(db: &Db, image: i64) -> R<Option<Change>> {
    let tx = begin(db)?;
    let Some(stack) = stack_of(&tx, image)? else { return Ok(None) };
    let m = members_of(&tx, stack)?;
    let Some(k) = m.iter().position(|&i| i == image) else { return Ok(None) };
    if k == 0 {
        return Ok(None);
    }
    let mut before = snapshot(&tx, &[stack], &[])?;
    let (a, b) = m.split_at(k);
    let row = read_row(&tx, stack)?.ok_or("stack missing")?;
    add_never(&tx, &a[..a.len().min(100)], &b[..b.len().min(100)], &mut before.never_added)?;
    make_user(&tx, stack)?;
    tx.execute(
        "INSERT INTO stack(kind, origin, cover, cover_by, collapsed, reason, algo, day, created) VALUES(?1, 'user', ?2, 'auto', ?3, NULL, ?4, ?5, ?6)",
        params![row.kind, b[0], row.collapsed, ALGO, row.day, crate::now()],
    )
    .map_err(e)?;
    let id = tx.last_insert_rowid();
    for i in b {
        tx.execute("UPDATE stack_image SET stack = ?2 WHERE image = ?1", params![i, id]).map_err(e)?;
    }
    if row.cover.is_some_and(|c| b.contains(&c)) {
        tx.execute("UPDATE stack SET cover = ?2, cover_by = 'auto' WHERE id = ?1", params![stack, a[0]]).map_err(e)?;
    }
    renumber(&tx, stack)?;
    renumber(&tx, id)?;
    remove_broken(&tx)?;
    let change = Change::with_created(before, &tx, &[id])?;
    tx.commit().map_err(e)?;
    Ok(Some(change))
}

/// Choose the cover (the photo on top). The stack becomes the user's.
pub fn set_cover(db: &Db, stack: i64, image: i64) -> R<Change> {
    let tx = begin(db)?;
    let before = snapshot(&tx, &[stack], &[])?;
    tx.execute("UPDATE stack SET cover = ?2, cover_by = 'user' WHERE id = ?1", params![stack, image]).map_err(e)?;
    make_user(&tx, stack)?;
    tx.commit().map_err(e)?;
    Ok(Change::new(before))
}

/// Collapse or open one stack (no change of ownership: an automatic series stays automatic).
pub fn set_collapsed(db: &Db, stack: i64, collapsed: bool) -> R<()> {
    db.conn.execute("UPDATE stack SET collapsed = ?2 WHERE id = ?1", params![stack, collapsed]).map(|_| ()).map_err(e)
}

/// Collapse or open all stacks (⇧S).
pub fn set_all_collapsed(db: &Db, collapsed: bool) -> R<()> {
    db.conn.execute("UPDATE stack SET collapsed = ?1", [collapsed]).map(|_| ()).map_err(e)
}

/// Apply the setting "stackCollapse" to the existing stacks: "always" collapses all, "never" opens all,
/// "reviewed" (default) collapses the reviewed and own stacks and opens the others.
pub fn apply_collapse_setting(db: &Db, mode: &str) -> R<()> {
    let sql = match mode {
        "always" => "UPDATE stack SET collapsed = 1",
        "never" => "UPDATE stack SET collapsed = 0",
        _ => "UPDATE stack SET collapsed = (reviewed IS NOT NULL OR kind = 'manual')",
    };
    db.conn.execute(sql, []).map(|_| ()).map_err(e)
}

/// Mark a series as reviewed (it becomes the user's); with the default setting it then collapses.
pub fn mark_reviewed(db: &Db, stack: i64) -> R<Change> {
    let collapse = crate::settings::value("stackCollapse").as_str() != Some("never");
    let tx = begin(db)?;
    let before = snapshot(&tx, &[stack], &[])?;
    tx.execute("UPDATE stack SET reviewed = ?2, collapsed = CASE WHEN ?3 THEN 1 ELSE collapsed END WHERE id = ?1", params![stack, crate::now(), collapse]).map_err(e)?;
    make_user(&tx, stack)?;
    tx.commit().map_err(e)?;
    Ok(Change::new(before))
}

// ───────────────────────────────────────────── Lightroom

/// Take over folder stacks from a Lightroom catalog: (catalog stack id, collapsed, our images with the top one first).
/// A stack already taken over is updated unless the user changed it in graineo (then its origin is 'user'); photos
/// in an own stack stay there, photos in an untouched automatic series move into the Lightroom stack. Returns the
/// number of stacks created or updated.
/// All or nothing: an error rolls back every stack of this call (own savepoint inside the caller's transaction).
pub fn import_lightroom(conn: &Connection, stacks: &[(i64, bool, Vec<i64>)]) -> R<usize> {
    conn.execute_batch("SAVEPOINT import_lightroom_stacks").map_err(e)?;
    match import_lightroom_in(conn, stacks) {
        Ok(n) => {
            conn.execute_batch("RELEASE import_lightroom_stacks").map_err(e)?;
            Ok(n)
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK TO import_lightroom_stacks; RELEASE import_lightroom_stacks");
            Err(err)
        }
    }
}

fn import_lightroom_in(conn: &Connection, stacks: &[(i64, bool, Vec<i64>)]) -> R<usize> {
    let mut n = 0;
    for (lr_id, collapsed, images) in stacks {
        let existing: Option<(i64, String)> = conn.query_row("SELECT id, origin FROM stack WHERE lr_id = ?1", [lr_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(e)?;
        if existing.as_ref().is_some_and(|(_, o)| o != "lightroom") {
            continue;
        }
        // photos in a stack of the user (or another Lightroom stack) stay where they are
        let mut members = Vec::new();
        for &img in images {
            let other: Option<(i64, String)> = conn
                .query_row("SELECT s.id, s.origin FROM stack_image si JOIN stack s ON s.id = si.stack WHERE si.image = ?1", [img], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .map_err(e)?;
            match other {
                Some((id, _)) if existing.as_ref().is_some_and(|x| x.0 == id) => members.push(img),
                Some((_, o)) if o == "auto" => members.push(img),
                Some(_) => {}
                None => members.push(img),
            }
        }
        if members.len() < 2 {
            continue;
        }
        let id = match existing {
            Some((id, _)) => {
                conn.execute("DELETE FROM stack_image WHERE stack = ?1", [id]).map_err(e)?;
                conn.execute("UPDATE stack SET cover = ?2, collapsed = ?3 WHERE id = ?1", params![id, members[0], collapsed]).map_err(e)?;
                id
            }
            None => {
                conn.execute(
                    "INSERT INTO stack(kind, origin, cover, cover_by, collapsed, algo, day, lr_id, created) VALUES('manual', 'lightroom', ?1, 'user', ?2, ?3, NULL, ?4, ?5)",
                    params![members[0], collapsed, ALGO, lr_id, crate::now()],
                )
                .map_err(e)?;
                conn.last_insert_rowid()
            }
        };
        for (p, img) in members.iter().enumerate() {
            conn.execute("DELETE FROM stack_image WHERE image = ?1", [img]).map_err(e)?;
            conn.execute("INSERT INTO stack_image(image, stack, position, pinned) VALUES(?1, ?2, ?3, 1)", params![img, id, p as i64 + 1]).map_err(e)?;
        }
        n += 1;
    }
    remove_broken(conn)?;
    Ok(n)
}

// ───────────────────────────────────────────── statistics (graineo-dev series)

/// Size distribution of the stacks: (stacks, photos in them, 2–3, 4–9, 10–29, ≥ 30, largest).
pub fn distribution(conn: &Connection) -> R<(usize, usize, [usize; 4], usize)> {
    let mut st = conn.prepare("SELECT count(*) FROM stack_image GROUP BY stack HAVING count(*) >= 2").map_err(e)?;
    let sizes: Vec<usize> = st.query_map([], |r| r.get::<_, i64>(0)).map_err(e)?.map(|x| x.map(|n| n as usize)).collect::<Result<_, _>>().map_err(e)?;
    let mut b = [0usize; 4];
    for &n in &sizes {
        b[match n {
            0..=3 => 0,
            4..=9 => 1,
            10..=29 => 2,
            _ => 3,
        }] += 1;
    }
    Ok((sizes.len(), sizes.iter().sum(), b, sizes.iter().copied().max().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(image: i64, secs: i64) -> Item {
        Item { image, file: image, folder: 1, stem: format!("img_{image}"), raw: false, camera: "C".into(), secs, ..Item::default() }
    }

    fn no_sim(_: i64, _: i64) -> Option<(f32, Basis)> {
        None
    }

    fn sets(g: &[Group]) -> Vec<Vec<i64>> {
        g.iter().map(|g| g.images.clone()).collect()
    }

    #[test]
    fn without_similarity_only_strict_time_chains() {
        let items = vec![it(1, 0), it(2, 1), it(3, 3), it(4, 20), it(5, 21)];
        let g = group(&items, &no_sim, &HashSet::new());
        // 1–2 (1 s), 2–3 (2 s), then 17 s gap, 4–5 (1 s)
        assert_eq!(sets(&g), vec![vec![1, 2, 3], vec![4, 5]]);
        assert!(g.iter().all(|g| g.reason.model == "time"));
    }

    #[test]
    fn subject_change_splits_a_time_chain() {
        let items: Vec<Item> = (1..=6).map(|i| it(i, i * 2)).collect();
        // 1–3 one subject, 4–6 another (all within 10 s gaps)
        let sim = |a: i64, b: i64| Some((if (a <= 3) == (b <= 3) { 0.95 } else { 0.5 }, Basis::Clip));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(sets(&g), vec![vec![1, 2, 3], vec![4, 5, 6]]);
        assert_eq!(g[0].reason.count, 3);
        assert_eq!(g[0].reason.span_s, 4);
        assert!((g[0].reason.sim_min.unwrap() - 0.95).abs() < 1e-6);
    }

    #[test]
    fn long_gap_only_when_very_similar() {
        let items = vec![it(1, 0), it(2, 25), it(3, 50)];
        let very = |_: i64, _: i64| Some((0.95, Basis::Clip));
        assert_eq!(sets(&group(&items, &very, &HashSet::new())), vec![vec![1, 2, 3]]);
        let fair = |_: i64, _: i64| Some((0.90, Basis::Clip));
        assert!(group(&items, &fair, &HashSet::new()).is_empty());
    }

    #[test]
    fn drift_ends_a_slow_pan() {
        let items: Vec<Item> = (1..=5).map(|i| it(i, i)).collect();
        // neighbours similar (0.9), but far from the first photo after three steps
        let sim = |a: i64, b: i64| Some((if (a - b).abs() <= 1 { 0.9 } else if (a - b).abs() <= 2 { 0.85 } else { 0.7 }, Basis::Clip));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(sets(&g), vec![vec![1, 2, 3], vec![4, 5]]);
    }

    #[test]
    fn cameras_and_never_pairs_are_respected() {
        let mut items = vec![it(1, 0), it(2, 1), it(3, 2), it(4, 3)];
        items[1].camera = "D".into();
        items[3].camera = "D".into();
        let g = group(&items, &no_sim, &HashSet::new());
        assert_eq!(sets(&g), vec![vec![1, 3], vec![2, 4]]);
        let never: HashSet<(i64, i64)> = [(1, 3)].into_iter().collect();
        let g = group(&items, &no_sim, &never);
        assert_eq!(sets(&g), vec![vec![2, 4]]);
    }

    #[test]
    fn raw_and_jpeg_count_as_one_picture() {
        let mut a = it(1, 0);
        a.stem = "dsc_1".into();
        a.raw = true;
        let mut b = it(2, 0);
        b.stem = "dsc_1".into();
        // a lone pair is no series
        assert!(group(&[a.clone(), b.clone()], &no_sim, &HashSet::new()).is_empty());
        let g = group(&[b, a, it(3, 1)], &no_sim, &HashSet::new());
        assert_eq!(sets(&g), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn large_groups_are_split_at_the_weakest_link() {
        let items: Vec<Item> = (1..=80).map(|i| it(i, i)).collect();
        let sim = |a: i64, b: i64| Some((if a.min(b) == 40 && a.max(b) == 41 { 0.89 } else { 0.97 }, Basis::Clip));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].images.len(), 40);
        assert_eq!(g[1].images.len(), 40);
    }

    #[test]
    fn uniform_chain_just_above_the_limit_splits_in_the_middle() {
        // no signatures yet, one photo per second: 61 equal links used to give one group of the last 60
        let items: Vec<Item> = (1..=61).map(|i| it(i, i)).collect();
        let g = group(&items, &no_sim, &HashSet::new());
        let sizes: Vec<usize> = g.iter().map(|g| g.images.len()).collect();
        assert_eq!(sizes, vec![30, 31]);
    }

    #[test]
    fn day_long_time_lapse_without_signatures_does_not_overflow() {
        // 20 000 intervalometer frames 1 s apart: once a stack overflow (one link split off per recursion level)
        let items: Vec<Item> = (1..=20_000).map(|i| it(i, i)).collect();
        let run = move || {
            let t = std::time::Instant::now();
            let g = group(&items, &no_sim, &HashSet::new());
            (g, t.elapsed())
        };
        // the series thread runs with the default 2 MiB stack
        let (g, took) = std::thread::Builder::new().stack_size(2 << 20).spawn(run).unwrap().join().unwrap();
        assert!(took.as_secs() < 10, "took {took:?}");
        assert!(g.iter().all(|g| (2..=MAX_MEMBERS).contains(&g.images.len())));
        assert_eq!(g.iter().map(|g| g.images.len()).sum::<usize>(), 20_000);
        // capture order is kept and nothing is lost or doubled
        let all: Vec<i64> = g.iter().flat_map(|g| g.images.iter().copied()).collect();
        assert!(all.windows(2).all(|w| w[0] < w[1]));
        // balanced cuts: no tiny leftovers
        assert!(g.iter().all(|g| g.images.len() >= MAX_MEMBERS / 3), "{:?}", g.iter().map(|g| g.images.len()).min());
    }

    #[test]
    fn identical_signatures_split_evenly() {
        let items: Vec<Item> = (1..=5000).map(|i| it(i, i * 2)).collect();
        let same = |_: i64, _: i64| Some((1.0, Basis::Signature));
        let g = group(&items, &same, &HashSet::new());
        assert_eq!(g.iter().map(|g| g.images.len()).sum::<usize>(), 5000);
        assert!(g.iter().all(|g| (MAX_MEMBERS / 3..=MAX_MEMBERS).contains(&g.images.len())));
    }

    #[test]
    fn exposure_bracket_is_recognized() {
        let mk = |i: i64, t: f64| Item { aperture: Some(8.0), exposure: Some(t), iso: Some(100), ..it(i, i / 2) };
        let items = vec![mk(1, 1.0 / 250.0), mk(2, 1.0 / 60.0), mk(3, 1.0 / 1000.0)];
        let sim = |_: i64, _: i64| Some((0.95, Basis::Clip));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].kind, Kind::Bracket);
        assert_eq!(g[0].reason.ev_steps, Some(3));
        // a burst with the same exposure is a series
        let items = vec![mk(1, 1.0 / 250.0), mk(2, 1.0 / 250.0), mk(3, 1.0 / 250.0)];
        assert_eq!(group(&items, &sim, &HashSet::new())[0].kind, Kind::Series);
    }

    /// QA 1.5.0-test.5: the cover of a bracket is its middle exposure, not the brightest or darkest frame (garden
    /// bracket: the first frame was nearly white).
    #[test]
    fn bracket_cover_is_the_middle_exposure() {
        let mk = |i: i64, t: f64| Item { aperture: Some(8.0), exposure: Some(t), iso: Some(100), ..it(i, i / 2) };
        let sim = |_: i64, _: i64| Some((0.95, Basis::Clip));
        let items = vec![mk(1, 1.0), mk(2, 1.0 / 4.0), mk(3, 1.0 / 15.0), mk(4, 1.0 / 60.0), mk(5, 1.0 / 250.0)];
        assert_eq!(group(&items, &sim, &HashSet::new())[0].cover, 3);
        // a photo the user rated still wins
        let mut items = items;
        items[4].rank = 3;
        assert_eq!(group(&items, &sim, &HashSet::new())[0].cover, 5);
    }

    /// QA 1.5.0-test.5: a Sigma SFD file (X3I, folder 7) and the single frames taken from it (X3F, folder 8) formed
    /// one series of eight across two folders. A series stays in its folder, and an X3I never joins single frames.
    #[test]
    fn series_stay_in_their_folder_and_sfd_alone() {
        let sim = |_: i64, _: i64| Some((0.95, Basis::Clip));
        let mut items: Vec<Item> = (1..=4).map(|i| it(i, i)).collect();
        items[2].folder = 2;
        items[3].folder = 2;
        let g = group(&items, &sim, &HashSet::new());
        let mut v: Vec<Vec<i64>> = g.iter().map(|g| g.images.clone()).collect();
        v.sort();
        assert_eq!(v, [vec![1, 2], vec![3, 4]]);
        // one folder: the X3I stays out of the frames' series
        let mut items: Vec<Item> = (1..=4).map(|i| it(i, i)).collect();
        items[0].sfd = true;
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].images, [2, 3, 4]);
    }

    /// A wide bracket (seven frames, 1 EV apart) stays one group although the extreme frames look different;
    /// 
    #[test]
    fn wide_bracket_stays_together() {
        let mk = |i: i64, t: f64| Item { aperture: Some(4.0), exposure: Some(t), iso: Some(100), ..it(i, i / 2) };
        let ts = [1.0 / 4000.0, 1.0 / 2000.0, 1.0 / 1000.0, 1.0 / 500.0, 1.0 / 250.0, 1.0 / 125.0, 1.0 / 60.0];
        let items: Vec<Item> = ts.iter().enumerate().map(|(i, t)| mk(i as i64 + 1, *t)).collect();
        // neighbours of a wide bracket: similarity 0.75 (below the join threshold 0.88 of the content model)
        let sim = |_: i64, _: i64| Some((0.75, Basis::Clip));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].kind, Kind::Bracket);
    }

    /// Shooting data that arrives after a first grouping (read in the background after the import) changes the
    /// day's fingerprint, so the day is grouped again and a bracket no longer stays split into small series.
    #[test]
    fn shooting_data_arriving_later_regroups_the_day() {
        let day = |exp: Option<f64>| DayItems { items: (1..=7).map(|i| Item { aperture: exp.map(|_| 16.0), exposure: exp, ..it(i, 0) }).collect(), sig: HashSet::new(), vec: HashSet::new(), unread: false };
        assert_ne!(fingerprint(&day(None)), fingerprint(&day(Some(0.5))));
        // with the data, seven frames of one second (1 EV apart, neighbours alike, the ends not) are one bracket
        let ts = [1.0 / 60.0, 1.0 / 15.0, 1.0 / 30.0, 1.0 / 8.0, 0.25, 0.5, 1.0];
        let items: Vec<Item> = ts.iter().enumerate().map(|(i, t)| Item { aperture: Some(16.0), exposure: Some(*t), iso: Some(100), ..it(i as i64 + 1, i as i64 / 5) }).collect();
        let sim = |a: i64, b: i64| Some((if a.abs_diff(b) <= 1 { 0.65 } else { 0.4 }, Basis::Signature));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(g.len(), 1, "{:?}", g.iter().map(|g| g.images.len()).collect::<Vec<_>>());
        assert_eq!(g[0].kind, Kind::Bracket);
    }

    /// A handheld bracket from a lens without aperture data (Pentax K50, 2 EV steps): the frames look different
    /// (similarity below the join threshold) and still are one bracket.
    #[test]
    fn bracket_without_aperture_data_stays_together() {
        let ts = [1.0 / 500.0, 1.0 / 125.0, 1.0 / 30.0];
        let items: Vec<Item> = ts.iter().enumerate().map(|(i, t)| Item { aperture: None, exposure: Some(*t), iso: Some(100), ..it(i as i64 + 1, 0) }).collect();
        let sim = |a: i64, b: i64| Some((if a.min(b) == 1 { 0.45 } else { 0.62 }, Basis::Signature));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(g.len(), 1, "{:?}", g.iter().map(|g| g.images.len()).collect::<Vec<_>>());
        assert_eq!(g[0].kind, Kind::Bracket);
        // photos with and without aperture data are no bracket
        assert!(!same_aperture(Some(4.0), None));
    }

    /// The free bracket "3-fenster-gx7-3er" (Panasonic GX7, three RW2 of one second, imported as _02, _03, _01): the
    /// camera shifts aperture and time together (f/3.2 1/400, f/2.8 1/250, f/2.2 1/200, ≈ 1 EV apart). Measured
    /// similarities of the real files (signature / CLIP). One bracket in file name order, whatever the import order.
    #[test]
    fn program_shift_bracket_of_one_second_is_one_bracket_in_name_order() {
        let gx7 = |image: i64, n: u8, aperture: f64, time: f64| Item {
            image,
            file: image,
            folder: 1,
            stem: format!("fenster-gx7-3er_0{n}"),
            raw: true,
            camera: "Panasonic DMC-GX7".into(),
            secs: 100,
            aperture: Some(aperture),
            exposure: Some(time),
            iso: Some(200),
            rank: 0,
            sfd: false,
        };
        let items = vec![gx7(107, 2, 2.8, 0.004), gx7(108, 3, 2.2, 0.005), gx7(109, 1, 3.2, 0.0025)];
        let pair = |a: i64, b: i64| (a.min(b), a.max(b));
        for (basis, v) in [(Basis::Signature, [0.904f32, 0.779, 0.871]), (Basis::Clip, [0.944, 0.921, 0.981])] {
            let sim = move |a: i64, b: i64| {
                Some((
                    match pair(a, b) {
                        (107, 109) => v[0],
                        (108, 109) => v[1],
                        _ => v[2],
                    },
                    basis,
                ))
            };
            let g = group(&items, &sim, &HashSet::new());
            assert_eq!(sets(&g), vec![vec![109, 107, 108]], "{basis:?}");
            assert_eq!(g[0].kind, Kind::Bracket, "{basis:?}");
            assert_eq!(g[0].reason.ev_steps, Some(3));
            // every import order gives the same group
            for order in [[0, 1, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
                let shuffled: Vec<Item> = order.iter().map(|&k| items[k].clone()).collect();
                assert_eq!(sets(&group(&shuffled, &sim, &HashSet::new())), vec![vec![109, 107, 108]]);
            }
        }
        // a change of the aperture alone (time fixed) is no bracket
        let fixed: Vec<Item> = [(1, 2.8), (2, 4.0), (3, 5.6)].iter().map(|&(n, a)| gx7(n as i64, n, a, 0.004)).collect();
        assert_eq!(group(&fixed, &|_, _| Some((0.95, Basis::Clip)), &HashSet::new())[0].kind, Kind::Series);
    }

    /// "2-garten-5d2-6er" (Canon 5D Mark II on a tripod, f/18, ISO 400, 1 s … 1/1250 s in 2 EV steps, 3–6 s apart,
    /// the time dial turned by hand): one bracket of six. Measured CLIP similarities of the real files; before, the
    /// first frame (6 s before the next, similarity 0.83 < 0.88) stayed alone and the rest was a series.
    #[test]
    fn bracket_set_by_hand_stays_together() {
        let shot = |image: i64, secs: i64, time: f64| Item { aperture: Some(18.0), exposure: Some(time), iso: Some(400), raw: true, camera: "Canon EOS 5D Mark II".into(), ..it(image, secs) };
        let items = vec![shot(1, 0, 1.0), shot(2, 6, 0.2), shot(3, 9, 0.05), shot(4, 12, 0.0125), shot(5, 17, 0.003125), shot(6, 21, 0.0008)];
        let near = [0.83f32, 0.875, 0.974, 0.96, 0.951];
        let sim = move |a: i64, b: i64| Some((if a.abs_diff(b) == 1 { near[(a.min(b) - 1) as usize] } else { 0.7 }, Basis::Clip));
        let g = group(&items, &sim, &HashSet::new());
        assert_eq!(sets(&g), vec![vec![1, 2, 3, 4, 5, 6]]);
        assert_eq!(g[0].kind, Kind::Bracket);
        assert_eq!(g[0].reason.ev_steps, Some(6));
        // the same frames with the ISO changing are no bracket set by hand
        let mut other = items.clone();
        other[1].iso = Some(800);
        assert_ne!(group(&other, &sim, &HashSet::new())[0].images.len(), 6);
        // a burst 6 s apart with the same exposure stays a plain series rule (not joined by the bracket slack)
        let same: Vec<Item> = (1..=3).map(|i| shot(i, i * 6, 0.01)).collect();
        assert!(group(&same, &|_, _| Some((0.80, Basis::Clip)), &HashSet::new()).is_empty());
    }

    #[test]
    fn cover_prefers_a_rated_photo() {
        let mut items = vec![it(1, 0), it(2, 1), it(3, 2)];
        items[1].rank = 13;
        let g = group(&items, &no_sim, &HashSet::new());
        assert_eq!(g[0].cover, 2);
        let g = group(&[it(1, 0), it(2, 1)], &no_sim, &HashSet::new());
        assert_eq!(g[0].cover, 1);
    }

    fn pattern(seed: u32, shift: u32) -> RgbImage {
        RgbImage::from_fn(256, 192, |x, y| {
            let x = x + shift;
            let v = match seed {
                0 => ((x / 32 + y / 32) % 2) as u8 * 200 + 20,
                1 => ((x * 255) / 256) as u8,
                _ => (((x as f32 / 20.0).sin() * (y as f32 / 15.0).cos() * 120.0) + 128.0) as u8,
            };
            image::Rgb([v, v / 2 + (seed * 40) as u8, 255 - v])
        })
    }

    #[test]
    fn signature_tells_near_identical_from_different() {
        let a = signature(&pattern(2, 0));
        let a2 = signature(&pattern(2, 3));
        let b = signature(&pattern(1, 0));
        let c = signature(&pattern(0, 0));
        assert_eq!(a.len(), SIGNATURE_LEN);
        assert!((signature_similarity(&a, &a).unwrap() - 1.0).abs() < 1e-6);
        let near = signature_similarity(&a, &a2).unwrap();
        let far = signature_similarity(&a, &b).unwrap().max(signature_similarity(&a, &c).unwrap());
        assert!(near > 0.8, "near {near}");
        assert!(far < 0.6, "far {far}");
        assert!(signature_similarity(&a, &[1, 2]).is_none());
    }

    #[test]
    fn signature_shift_finds_an_offset() {
        let img = pattern(2, 0);
        let moved = RgbImage::from_fn(256, 192, |x, y| *img.get_pixel(x.saturating_sub(64).min(255), y));
        let (dx, dy) = signature_shift(&signature(&img), &signature(&moved)).unwrap();
        assert!(dx > 0.0, "dx {dx}");
        assert_eq!(dy, 0.0);
    }

    // ── in the library

    fn test_db(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("graineo-series-{}-{}.sqlite", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        let db = Db::open(&p).unwrap();
        db.conn.execute_batch("INSERT INTO root_folder(id, path, name, origin) VALUES(1,'/x/','x','own'); INSERT INTO folder(id, root, path) VALUES(1,1,'');").unwrap();
        db
    }

    fn add(db: &Db, id: i64, time: &str) {
        db.conn
            .execute("INSERT INTO file(id, folder, name, format, camera, capture_time) VALUES(?1, 1, ?2, 'JPG', 'Cam', ?3)", params![id, format!("IMG_{id}.JPG"), time])
            .unwrap();
        db.conn.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
    }

    fn stacks(db: &Db) -> Vec<Vec<i64>> {
        let mut v: Vec<Vec<i64>> = load(&db.conn).unwrap().into_iter().map(|s| s.images).collect();
        v.sort();
        v
    }

    fn never_cancel() -> bool {
        false
    }

    /// QA 1.5.0-test.5: right after an import brackets showed in pieces until the next start (shooting data and
    /// previews arrive in the background). A day is grouped once its photos are complete, in the same session.
    #[test]
    fn days_wait_for_shooting_data_and_previews() {
        let db = test_db("waiting");
        for i in 1..=3 {
            add(&db, i, &format!("2024-05-11T14:00:0{i}"));
        }
        let none = HashSet::new();
        let s = regroup_ready(&db, false, &never_cancel, &|_, _| {}, Some(&none)).unwrap();
        assert_eq!((s.waiting_days, s.regrouped_days), (1, 0), "shooting data not read yet");
        assert!(stacks(&db).is_empty());
        db.conn.execute("UPDATE file SET exif = 1", []).unwrap();
        let waiting: HashSet<i64> = [2].into_iter().collect();
        let s = regroup_ready(&db, false, &never_cancel, &|_, _| {}, Some(&waiting)).unwrap();
        assert_eq!(s.waiting_days, 1, "a preview is missing");
        assert!(stacks(&db).is_empty());
        let s = regroup_ready(&db, false, &never_cancel, &|_, _| {}, Some(&none)).unwrap();
        assert_eq!((s.waiting_days, s.regrouped_days), (0, 1));
        assert_eq!(stacks(&db), vec![vec![1, 2, 3]]);
        // a series of an older grouping version is not shown until the next run regroups it
        db.conn.execute("UPDATE stack SET algo = ?1", [ALGO - 1]).unwrap();
        assert!(stacks(&db).is_empty());
    }

    #[test]
    fn regroup_writes_keeps_and_respects_fixed_stacks() {
        let db = test_db("regroup");
        for (i, t) in [(1, "2024-05-11T14:03:10"), (2, "2024-05-11T14:03:11"), (3, "2024-05-11T14:03:12"), (4, "2024-05-11T15:00:00"), (5, "2024-05-12T09:00:00"), (6, "2024-05-12T09:00:01")] {
            add(&db, i, t);
        }
        let s = regroup(&db, false, &never_cancel).unwrap();
        assert_eq!(s.days, 2);
        assert_eq!(s.regrouped_days, 2);
        assert_eq!(stacks(&db), vec![vec![1, 2, 3], vec![5, 6]]);
        let first = load(&db.conn).unwrap();
        // unchanged days are skipped; ids stay
        let s = regroup(&db, false, &never_cancel).unwrap();
        assert_eq!(s.regrouped_days, 0);
        let s = regroup(&db, true, &never_cancel).unwrap();
        assert_eq!(s.stacks_kept, 2);
        assert_eq!(load(&db.conn).unwrap().iter().map(|s| s.id).collect::<Vec<_>>(), first.iter().map(|s| s.id).collect::<Vec<_>>());
        // a new photo joins the series on the next run
        add(&db, 7, "2024-05-12T09:00:02");
        regroup(&db, false, &never_cancel).unwrap();
        assert_eq!(stacks(&db), vec![vec![1, 2, 3], vec![5, 6, 7]]);
        // the user takes 3 out: never regrouped with 1 and 2, the stack is fixed now
        let id = stack_of(&db.conn, 1).unwrap().unwrap();
        take_out(&db, 3).unwrap();
        regroup(&db, true, &never_cancel).unwrap();
        assert_eq!(stacks(&db), vec![vec![1, 2], vec![5, 6, 7]]);
        assert_eq!(get(&db.conn, id).unwrap().unwrap().origin, "user");
        // deleting photos removes stacks with fewer than two members
        db.conn.execute("DELETE FROM image WHERE id IN (6, 7)", []).unwrap();
        regroup(&db, false, &never_cancel).unwrap();
        assert_eq!(stacks(&db), vec![vec![1, 2]]);
    }

    /// Found in 1.4.2 ("1/1" on a tile): a stack with only one photo left that the library shows is no stack –
    /// neither when loading nor after the next grouping run; own stacks too.
    #[test]
    fn stacks_with_one_shown_photo_are_dissolved() {
        let db = test_db("one-left");
        for i in 1..=6 {
            add(&db, i, &format!("2024-05-11T14:0{i}:00"));
        }
        let (_, own) = create(&db, &[1, 2], 1).unwrap();
        let (_, hidden) = create(&db, &[3, 4], 3).unwrap();
        let (_, kept) = create(&db, &[5, 6], 5).unwrap();
        // photo 2 leaves the library (cascade: its member row goes), photo 4 is hidden for good (Apple Photos)
        db.conn.execute("DELETE FROM image WHERE id = 2", []).unwrap();
        db.conn.execute("UPDATE file SET status = 4 WHERE id = 4", []).unwrap();
        // loading already leaves both out
        assert_eq!(stacks(&db), vec![vec![5, 6]]);
        // the next run removes them from the database
        assert_eq!(remove_broken(&db.conn).unwrap(), 2);
        assert!(get(&db.conn, own).unwrap().is_none());
        assert!(get(&db.conn, hidden).unwrap().is_none());
        assert!(get(&db.conn, kept).unwrap().is_some());
        // a missing file (status 1) is still a photo of the library: its stack stays
        db.conn.execute("UPDATE file SET status = 1 WHERE id = 6", []).unwrap();
        assert_eq!(remove_broken(&db.conn).unwrap(), 0);
        assert_eq!(stacks(&db), vec![vec![5, 6]]);
    }

    #[test]
    fn lightroom_stacks_are_taken_over_and_own_changes_kept() {
        let db = test_db("lightroom");
        for i in 1..=4 {
            add(&db, i, &format!("2024-05-11T14:0{i}:00"));
        }
        let n = import_lightroom(&db.conn, &[(77, true, vec![3, 1]), (78, false, vec![4])]).unwrap();
        assert_eq!(n, 1, "a stack of one photo is no stack");
        let id = stack_of(&db.conn, 1).unwrap().unwrap();
        let s = get(&db.conn, id).unwrap().unwrap();
        assert_eq!((s.origin.as_str(), s.top(), s.collapsed, s.kind), ("lightroom", 3, true, Kind::Manual));
        // the grouping leaves it alone
        regroup(&db, true, &never_cancel).unwrap();
        assert_eq!(get(&db.conn, id).unwrap().unwrap().images, vec![3, 1]);
        // a new import updates it …
        import_lightroom(&db.conn, &[(77, false, vec![1, 3, 2])]).unwrap();
        assert_eq!(get(&db.conn, id).unwrap().unwrap().images, vec![1, 3, 2]);
        // … unless the user changed it in graineo
        set_cover(&db, id, 2).unwrap();
        import_lightroom(&db.conn, &[(77, false, vec![1, 3])]).unwrap();
        let s = get(&db.conn, id).unwrap().unwrap();
        assert_eq!((s.origin.as_str(), s.images.len(), s.top()), ("user", 3, 2));
    }

    #[test]
    fn user_changes_can_be_undone() {
        let db = test_db("undo");
        for i in 1..=4 {
            add(&db, i, &format!("2024-05-11T14:03:1{i}"));
        }
        regroup(&db, false, &never_cancel).unwrap();
        let before = stacks(&db);
        let id = stack_of(&db.conn, 1).unwrap().unwrap();
        let c = split_at(&db, 3).unwrap().unwrap();
        assert_eq!(stacks(&db), vec![vec![1, 2], vec![3, 4]]);
        restore(&db, &c).unwrap();
        assert_eq!(stacks(&db), before);
        assert_eq!(get(&db.conn, id).unwrap().unwrap().origin, "auto");
        let never: i64 = db.conn.query_row("SELECT count(*) FROM stack_never", [], |r| r.get(0)).unwrap();
        assert_eq!(never, 0);
        // cover and review
        let c1 = set_cover(&db, id, 3).unwrap();
        assert_eq!(get(&db.conn, id).unwrap().unwrap().top(), 3);
        let c2 = mark_reviewed(&db, id).unwrap();
        assert!(get(&db.conn, id).unwrap().unwrap().reviewed.is_some());
        assert_eq!(open_count(&db.conn), 0);
        restore(&db, &c2).unwrap();
        restore(&db, &c1).unwrap();
        let s = get(&db.conn, id).unwrap().unwrap();
        assert_eq!((s.top(), s.reviewed, s.origin.as_str()), (1, None, "auto"));
        // own stack and dissolving
        let (c3, own) = create(&db, &[2, 4], 4).unwrap();
        assert_eq!(get(&db.conn, own).unwrap().unwrap().images, vec![4, 2]);
        assert_eq!(stacks(&db), vec![vec![1, 3], vec![4, 2]]);
        restore(&db, &c3).unwrap();
        assert_eq!(stacks(&db), before);
        let c4 = dissolve(&db, id).unwrap();
        assert!(stacks(&db).is_empty());
        regroup(&db, true, &never_cancel).unwrap();
        assert!(stacks(&db).is_empty(), "dissolved photos are not grouped again");
        restore(&db, &c4).unwrap();
        assert_eq!(stacks(&db), before);
    }

    /// ⌘Z of "dissolve" when the background grouping has used the id again meanwhile: the other stack stays as it
    /// is, the old one comes back under a new id.
    #[test]
    fn undo_never_overwrites_a_stack_that_reused_the_id() {
        let db = test_db("undo-reuse");
        for i in 1..=6 {
            add(&db, i, &format!("2024-05-11T14:03:1{i}"));
        }
        let (_, id) = create(&db, &[1, 2, 3], 1).unwrap();
        let c = dissolve(&db, id).unwrap();
        db.conn.execute("INSERT INTO stack(id, kind, origin, collapsed, algo, created) VALUES(?1, 'series', 'auto', 1, 1, 5)", [id]).unwrap();
        for (p, i) in [5, 6].iter().enumerate() {
            db.conn.execute("INSERT INTO stack_image(image, stack, position) VALUES(?1, ?2, ?3)", params![i, id, p as i64 + 1]).unwrap();
        }
        restore(&db, &c).unwrap();
        let other = get(&db.conn, id).unwrap().unwrap();
        assert_eq!((other.images, other.origin.as_str()), (vec![5, 6], "auto"));
        let back = stack_of(&db.conn, 1).unwrap().unwrap();
        assert_ne!(back, id);
        assert_eq!(get(&db.conn, back).unwrap().unwrap().images, vec![1, 2, 3]);
        // ⌘Z of "create" when the created stack is gone and its id taken by the grouping: that stack stays
        let (c, own) = create(&db, &[3, 4], 3).unwrap();
        db.conn.execute("DELETE FROM stack WHERE id = ?1", [own]).unwrap();
        db.conn.execute("INSERT INTO stack(id, kind, origin, collapsed, algo, created) VALUES(?1, 'series', 'auto', 1, 1, 5)", [own]).unwrap();
        restore(&db, &c).unwrap();
        assert!(get(&db.conn, own).unwrap().is_some(), "not deleted");
    }

    /// A failing Lightroom stack import changes no stack (all or nothing).
    #[test]
    fn failed_lightroom_stack_import_changes_nothing() {
        let db = test_db("lightroom-fail");
        for i in 1..=4 {
            add(&db, i, &format!("2024-05-11T14:0{i}:00"));
        }
        import_lightroom(&db.conn, &[(77, true, vec![1, 2])]).unwrap();
        let before = stacks(&db);
        // the second stack names a photo the library does not have: the insert fails
        assert!(import_lightroom(&db.conn, &[(77, true, vec![2, 1, 3]), (78, true, vec![4, 999])]).is_err());
        assert_eq!(stacks(&db), before);
        assert_eq!(db.conn.query_row("SELECT count(*) FROM stack WHERE lr_id = 78", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    /// Frames of one bracket (the same picture darker and brighter) look alike; another subject does not; without
    /// signatures nothing is known.
    #[test]
    fn bracket_alike_compares_neighbours() {
        let db = test_db("bracket-alike");
        for id in 1..=5 {
            add(&db, id, "2026-01-01T10:00:00");
        }
        let scaled = |f: f32| {
            let img = pattern(2, 0);
            RgbImage::from_fn(img.width(), img.height(), |x, y| {
                let p = img.get_pixel(x, y);
                image::Rgb([(p[0] as f32 * f).min(255.0) as u8, (p[1] as f32 * f).min(255.0) as u8, (p[2] as f32 * f).min(255.0) as u8])
            })
        };
        save_signature(&db, 1, &signature(&scaled(0.5))).unwrap();
        save_signature(&db, 2, &signature(&scaled(1.0))).unwrap();
        save_signature(&db, 3, &signature(&scaled(1.6))).unwrap();
        save_signature(&db, 4, &signature(&pattern(0, 0))).unwrap();
        assert_eq!(bracket_alike(&db.conn, &[1, 2, 3]).unwrap(), Some(true), "a bracket of one picture");
        assert_eq!(bracket_alike(&db.conn, &[1, 2, 4]).unwrap(), Some(false), "another subject in it");
        assert_eq!(bracket_alike(&db.conn, &[5, 1]).unwrap(), None, "no signature: nothing known");
    }
}
