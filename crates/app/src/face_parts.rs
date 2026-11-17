//! Parts of people (macOS): face, facial skin, eyes, eyebrows, lips and the opening of the mouth, from the face
//! landmarks of the system's image analysis (Vision `VNDetectFaceLandmarksRequest`: contours of jaw line, eyes,
//! eyebrows, outer and inner lips) and its person segmentation. Vision gives no hair, body skin or clothing; those are
//! not offered (a face-parsing model would be needed, and the open ones are trained on data for non-commercial use
//! only).
//!
//! All parts of a photo come from one run (develop the mask space, turn it upright, landmarks and person mask, polygons
//! filled with 2 × 2 samples, soft edge), kept for the photo; pending leaves of any part are filled from it. On other
//! systems the parts are locked ([`available`]).
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use develop::Settings;
use masks::{Document, Mask, PersonPart, RasterData, RasterSource};

use crate::analysis_progress::{self as progress, Tool};

/// Landmark contours of one face in pixels of the analysed (upright) image, origin top left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Face {
    pub contour: Vec<[f32; 2]>,
    pub eyes: [Vec<[f32; 2]>; 2],
    pub brows: [Vec<[f32; 2]>; 2],
    pub outer_lips: Vec<[f32; 2]>,
    pub inner_lips: Vec<[f32; 2]>,
}

/// German name of a part (translated when shown).
pub fn label(p: PersonPart) -> &'static str {
    match p {
        PersonPart::Face => "Gesicht",
        PersonPart::Skin => "Gesichtshaut",
        PersonPart::Eyes => "Augen",
        PersonPart::Brows => "Augenbrauen",
        PersonPart::Lips => "Lippen",
        PersonPart::Mouth => "Zähne",
    }
}

/// Tooltip of a part's tile (German key).
pub fn tip(p: PersonPart) -> &'static str {
    match p {
        PersonPart::Face => "Gesichter vom Kinn bis zur Stirn (Gesichtslandmarken der Bildanalyse des Systems)",
        PersonPart::Skin => "Haut der Gesichter: ohne Augen, Brauen und Mund, nach der Farbe des Gesichts",
        PersonPart::Eyes => "Augen (Kontur aus den Gesichtslandmarken)",
        PersonPart::Brows => "Augenbrauen (aus den Gesichtslandmarken)",
        PersonPart::Lips => "Lippen ohne die Mundöffnung",
        PersonPart::Mouth => "Mundöffnung (Zähne), innere Lippenkontur",
    }
}

/// Can the parts be computed on this system? `Err`: why not (tooltip of the locked tile).
pub fn available() -> Result<(), &'static str> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err("Nur auf dem Mac (Gesichtslandmarken der Bildanalyse des Systems)")
    }
}

// ── geometry ────────────────────────────────────────────────────────────────────────────────────────────────────────

fn inside(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    let mut c = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
        j = i;
    }
    c
}

/// Coverage of a closed polygon (2 × 2 samples per pixel) added into `out` (maximum).
fn fill(out: &mut [f32], w: usize, h: usize, poly: &[[f32; 2]]) {
    if poly.len() < 3 {
        return;
    }
    let (x0, x1) = poly.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p[0]), a.1.max(p[0])));
    let (y0, y1) = poly.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p[1]), a.1.max(p[1])));
    for y in (y0.floor().max(0.0) as usize)..(y1.ceil().max(0.0) as usize).min(h) {
        for x in (x0.floor().max(0.0) as usize)..(x1.ceil().max(0.0) as usize).min(w) {
            let mut n = 0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                n += usize::from(inside(poly, x as f32 + dx, y as f32 + dy));
            }
            let v = n as f32 / 4.0;
            let o = &mut out[y * w + x];
            *o = o.max(v);
        }
    }
}

/// Convex hull (monotone chain), counter-clockwise in image coordinates.
fn hull(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut p: Vec<[f32; 2]> = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<[f32; 2]> = Vec::new();
    for &q in &p {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], q) <= 0.0 {
            lower.pop();
        }
        lower.push(q);
    }
    let mut upper: Vec<[f32; 2]> = Vec::new();
    for &q in p.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], q) <= 0.0 {
            upper.pop();
        }
        upper.push(q);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Pixels within `r` of the polyline (soft over one pixel), added into `out` (maximum).
fn stroke(out: &mut [f32], w: usize, h: usize, line: &[[f32; 2]], r: f32) {
    for seg in line.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let (x0, x1) = (a[0].min(b[0]) - r - 1.0, a[0].max(b[0]) + r + 1.0);
        let (y0, y1) = (a[1].min(b[1]) - r - 1.0, a[1].max(b[1]) + r + 1.0);
        let d = [b[0] - a[0], b[1] - a[1]];
        let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-6);
        for y in (y0.max(0.0) as usize)..(y1.max(0.0) as usize).min(h) {
            for x in (x0.max(0.0) as usize)..(x1.max(0.0) as usize).min(w) {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0);
                let dist = (p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1]);
                let v = (r + 0.5 - dist).clamp(0.0, 1.0);
                let o = &mut out[y * w + x];
                *o = o.max(v);
            }
        }
    }
}

/// Box blur of radius `r` (three passes ≈ Gaussian), for soft edges.
fn soften(d: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; w.max(h)];
    for _ in 0..3 {
        for y in 0..h {
            let row = &mut d[y * w..(y + 1) * w];
            tmp[..w].copy_from_slice(row);
            box_line(row, &tmp[..w], r);
        }
        let mut col = vec![0.0f32; h];
        for x in 0..w {
            for y in 0..h {
                col[y] = d[y * w + x];
            }
            tmp[..h].copy_from_slice(&col);
            box_line(&mut col, &tmp[..h], r);
            for y in 0..h {
                d[y * w + x] = col[y];
            }
        }
    }
}

fn box_line(out: &mut [f32], src: &[f32], r: usize) {
    let n = src.len() as isize;
    let at = |i: isize| src[i.clamp(0, n - 1) as usize];
    let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
    let k = (2 * r + 1) as f32;
    for i in 0..n {
        out[i as usize] = sum / k;
        sum += at(i + r as isize + 1) - at(i - r as isize);
    }
}

/// Height of a face from the eyebrows to the chin (pixels), at least 8.
fn face_height(f: &Face) -> f32 {
    let brow = f.brows.iter().flatten().map(|p| p[1]).fold(f32::MAX, f32::min);
    let chin = f.contour.iter().chain(f.outer_lips.iter()).map(|p| p[1]).fold(f32::MIN, f32::max);
    if brow < chin { (chin - brow).max(8.0) } else { 8.0 }
}

/// The face: hull of jaw line, eyebrows and those raised to the forehead (half the brow–chin height).
fn face_polygon(f: &Face) -> Vec<[f32; 2]> {
    let lift = 0.5 * face_height(f);
    let mut pts: Vec<[f32; 2]> = f.contour.clone();
    for b in f.brows.iter().flatten() {
        pts.push(*b);
        pts.push([b[0], b[1] - lift]);
    }
    pts.extend(f.eyes.iter().flatten());
    pts.extend(&f.outer_lips);
    hull(&pts)
}

/// Weights of `part` for the faces `faces` in an image of `w × h` (the analysed image `rgb`, linear sRGB in 0..1 for
/// the colour of the skin), cut to the person mask `person` where there is one.
pub fn part_weights(faces: &[Face], person: Option<&[f32]>, rgb: Option<&[f32]>, w: usize, h: usize, part: PersonPart) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for f in faces {
        let fh = face_height(f);
        let mut one = vec![0.0f32; w * h];
        match part {
            PersonPart::Face | PersonPart::Skin => fill(&mut one, w, h, &face_polygon(f)),
            PersonPart::Eyes => f.eyes.iter().for_each(|e| fill(&mut one, w, h, e)),
            PersonPart::Brows => {
                for b in &f.brows {
                    fill(&mut one, w, h, &hull(b));
                    stroke(&mut one, w, h, b, 0.04 * fh);
                }
            }
            PersonPart::Lips => {
                fill(&mut one, w, h, &f.outer_lips);
                let mut inner = vec![0.0f32; w * h];
                fill(&mut inner, w, h, &f.inner_lips);
                one.iter_mut().zip(&inner).for_each(|(o, i)| *o = (*o - i).max(0.0));
            }
            PersonPart::Mouth => fill(&mut one, w, h, &f.inner_lips),
        }
        if part == PersonPart::Skin {
            // without eyes, eyebrows and mouth (each with a margin of 4 % of the face height)
            let mut features = vec![0.0f32; w * h];
            f.eyes.iter().for_each(|e| fill(&mut features, w, h, e));
            f.eyes.iter().for_each(|e| stroke(&mut features, w, h, &[e.clone(), e.first().cloned().into_iter().collect()].concat(), 0.04 * fh));
            for b in &f.brows {
                fill(&mut features, w, h, &hull(b));
                stroke(&mut features, w, h, b, 0.06 * fh);
            }
            fill(&mut features, w, h, &f.outer_lips);
            stroke(&mut features, w, h, &[f.outer_lips.clone(), f.outer_lips.first().cloned().into_iter().collect()].concat(), 0.03 * fh);
            one.iter_mut().zip(&features).for_each(|(o, x)| *o = (*o - x).max(0.0));
            if let Some(rgb) = rgb {
                skin_colour(&mut one, rgb);
            }
        }
        // soft edge: 0.5 % of the face height, at least one pixel
        soften(&mut one, w, h, ((0.005 * fh).round() as usize).max(1));
        out.iter_mut().zip(&one).for_each(|(o, v)| *o = o.max(*v));
    }
    if let Some(p) = person {
        // the face is cut to the person (hair or background inside the hull is left out); the small parts stay
        if matches!(part, PersonPart::Face | PersonPart::Skin) && p.len() == out.len() {
            out.iter_mut().zip(p).for_each(|(o, q)| *o = o.min(*q));
        }
    }
    out
}

/// Keeps the skin of a face candidate `w` by its colour: Oklab distance to the median colour of the candidate pixels
/// (lightness half), full up to 0.06, gone at 0.14.
fn skin_colour(w: &mut [f32], rgb: &[f32]) {
    let mut labs: Vec<([f32; 3], usize)> = w.iter().enumerate().filter(|(_, v)| **v > 0.5).map(|(i, _)| (masks::oklab([rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]]), i)).collect();
    if labs.len() < 16 {
        return;
    }
    let median = |k: usize, l: &mut Vec<([f32; 3], usize)>| {
        l.sort_by(|a, b| a.0[k].total_cmp(&b.0[k]));
        l[l.len() / 2].0[k]
    };
    let m = [median(0, &mut labs), median(1, &mut labs), median(2, &mut labs)];
    for (i, v) in w.iter_mut().enumerate() {
        if *v > 0.0 {
            let d = masks::color_distance(masks::oklab([rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]]), m);
            *v *= 1.0 - ((d - 0.06) / 0.08).clamp(0.0, 1.0);
        }
    }
}

// ── computing for a photo ────────────────────────────────────────────────────────────────────────────────────────────

/// Rasters of all parts for one photo (`None` for a part: nothing found).
struct Found {
    path: PathBuf,
    ids: Vec<(PersonPart, Option<String>)>,
    faces: usize,
}

static JOB: Mutex<Option<(PathBuf, Option<Result<Found, String>>)>> = Mutex::new(None);
/// Name of this tool for [`crate::subject_ui::redo`].
pub const TOOL: &str = "parts";
/// The parts of the photos computed last.
static LAST: Mutex<Vec<Arc<Found>>> = Mutex::new(Vec::new());

fn last(path: &Path) -> Option<Arc<Found>> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().find(|f| f.path == path).cloned()
}

/// Is the computation of the parts running (any photo)?
pub fn busy() -> bool {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.1.is_none())
}

/// Is it running for this photo?
pub fn running(path: &Path) -> bool {
    JOB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.0 == path && j.1.is_none())
}

/// Forget the parts of a photo (compute again).
pub fn forget(path: &Path) {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| f.path != path);
    clear_message(path);
}

/// Last message for this photo.
static MESSAGE: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

pub fn message(path: &Path) -> Option<String> {
    MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|m| m.0 == path).map(|m| m.1.clone())
}

fn clear_message(path: &Path) {
    let mut g = MESSAGE.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|m| m.0 == path) {
        *g = None;
    }
}

/// Start computing the parts of the photo (background, one at a time).
pub fn start(ctx: &egui::Context, path: &Path, settings: &Settings) {
    {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.1.is_none()) {
            return;
        }
        *g = Some((path.to_path_buf(), None));
    }
    clear_message(path);
    let ticket = progress::begin(Tool::Parts, path, progress::DEVELOP);
    let (path, settings, ctx) = (path.to_path_buf(), settings.clone(), ctx.clone());
    let failed = path.clone();
    let work = move || {
        library::platform::set_priority(library::platform::Priority::UserInitiated);
        let r = crate::subject_ui::guarded("person parts", || compute(&path, &settings, &ticket));
        if let Err(e) = &r {
            crate::log(&format!("person parts {}: {e}", path.display()));
        }
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.0 == path) {
            j.1 = Some(r);
        }
        drop(ticket);
        ctx.request_repaint();
    };
    crate::subject_ui::spawn_job("person-parts", work, |e| {
        if let Some(j) = JOB.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|j| j.0 == failed) {
            j.1 = Some(Err(e));
        }
    });
}

#[cfg(target_os = "macos")]
fn landmarks(img: &image::RgbImage) -> Result<Vec<Face>, String> {
    let to = |v: &[(f32, f32)]| v.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>();
    Ok(faces::vision::face_shapes(img)?
        .into_iter()
        .filter(|f| !f.contour.is_empty() && !f.left_eye.is_empty())
        .map(|f| Face {
            contour: to(&f.contour),
            eyes: [to(&f.left_eye), to(&f.right_eye)],
            brows: [to(&f.left_brow), to(&f.right_brow)],
            outer_lips: to(&f.outer_lips),
            inner_lips: to(&f.inner_lips),
        })
        .collect())
}

#[cfg(not(target_os = "macos"))]
fn landmarks(_: &image::RgbImage) -> Result<Vec<Face>, String> {
    Err("person parts are only available on macOS".into())
}

/// Develop the photo in the mask space (without local corrections), find the faces upright and build all parts.
fn compute(path: &Path, settings: &Settings, ticket: &progress::Ticket) -> Result<Found, String> {
    let t = std::time::Instant::now();
    let s = Settings { local: None, ..settings.clone() };
    let (img, camera) = crate::large_preview::render_mask_space(path, &s)?;
    let (w, h) = (img.width, img.height);
    let side = crate::subject_ui::RASTER_SIDE;
    let scale = (side as f32 / w.max(h) as f32).min(1.0);
    let (mw, mh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    let rgb = image::RgbImage::from_raw(w as u32, h as u32, img.to_rgb8()).ok_or("image size")?;
    let small = image::imageops::resize(&rgb, mw, mh, image::imageops::FilterType::Triangle);
    let quarter = develop::geometry::Frame::full(mw as usize, mh as usize, develop::geometry::quarters(&Settings { rotate: settings.rotate, ..Settings::default() }, camera));
    let (uw, uh) = (quarter.w.round() as u32, quarter.h.round() as u32);
    let upright = image::RgbImage::from_fn(uw, uh, |x, y| {
        let p = quarter.to_src([(x as f32 + 0.5) / uw as f32, (y as f32 + 0.5) / uh as f32]);
        *small.get_pixel((p[0] as u32).min(mw - 1), (p[1] as u32).min(mh - 1))
    });
    let developed = t.elapsed().as_secs_f32();
    ticket.step(progress::FACES)?;
    let t = std::time::Instant::now();
    let found = landmarks(&upright)?;
    let person = if found.is_empty() || segment::available(segment::Kind::Person) == segment::Support::None {
        None
    } else {
        ticket.step(progress::PEOPLE)?;
        let p = segment::analyse(&upright, segment::Kind::Person)?;
        (!p.masks.is_empty()).then(|| p.union(&[]))
    };
    let lin: Vec<f32> = upright.as_raw().iter().map(|&v| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }).collect();
    let analysed = t.elapsed().as_secs_f32();
    let mut ids = Vec::new();
    for part in PersonPart::ALL {
        if found.is_empty() {
            ids.push((part, None));
            continue;
        }
        let up = part_weights(&found, person.as_deref(), Some(&lin), uw as usize, uh as usize, part);
        if !up.iter().any(|v| *v > 0.5) {
            ids.push((part, None));
            continue;
        }
        // back into the mask space
        let mut out = vec![0.0f32; (mw * mh) as usize];
        for y in 0..mh {
            for x in 0..mw {
                let u = quarter.from_src([x as f32 + 0.5, y as f32 + 0.5]);
                let (ux, uy) = (((u[0] * uw as f32) as u32).min(uw - 1), ((u[1] * uh as f32) as u32).min(uh - 1));
                out[(y * mw + x) as usize] = up[(uy * uw + ux) as usize];
            }
        }
        let data = Arc::new(RasterData::from_weights(mw, mh, &out));
        // stored before any leaf names it
        crate::subject_ui::store_now(&data, path, &serde_json::json!({"rule": "person_part", "part": format!("{part:?}").to_lowercase(), "file": path.file_name().map(|n| n.to_string_lossy().to_string()), "app": env!("CARGO_PKG_VERSION")}))?;
        let id = masks::raster::remember(data);
        ids.push((part, Some(id)));
    }
    crate::log(&format!("person parts {}: {} faces, develop {developed:.2} s, analysis {analysed:.2} s", path.display(), found.len()));
    Ok(Found { path: path.to_path_buf(), ids, faces: found.len() })
}

/// Fill pending leaves of person parts from the parts computed for this photo (and those named in `replace`, which are
/// computed again; a part not found this time leaves them as they were).
fn fill_leaves(local: &mut Option<Document>, f: &Found, replace: &[String]) {
    let Some(d) = local.as_mut() else { return };
    for l in &mut d.layers {
        l.mask.visit_mut(&mut |m| {
            if let Mask::Raster { raster, source: RasterSource::PersonPart { part }, .. } = m {
                if raster.is_empty() || replace.contains(raster) || masks::raster::resolve(raster).is_none() {
                    if let Some((_, Some(id))) = f.ids.iter().find(|(p, _)| p == part) {
                        *raster = id.clone();
                    }
                }
            }
        });
    }
    d.settle_version();
}

/// As with the subject: no new, unchanged layer that is only a part nothing was found for.
fn drop_untouched_pending(local: &mut Option<Document>) {
    if let Some(d) = local.as_mut() {
        d.layers.retain(|l| !(matches!(&l.mask, Mask::Raster { raster, source: RasterSource::PersonPart { .. }, .. } if raster.is_empty()) && crate::subject_ui::untouched(l)));
        d.settle_version();
        if d.layers.is_empty() {
            *local = None;
        }
    }
}

/// Every frame with the mask panel: take a finished computation into the pending leaves; `true` when the photo has
/// pending person parts that can be computed now (the caller starts [`start`]).
pub fn update(local: &mut Option<Document>, path: &Path) -> bool {
    let done = {
        let mut g = JOB.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|j| j.0 == path && j.1.is_some()) { g.take() } else { None }
    };
    if let Some((_, Some(r))) = done {
        let text = match r {
            Ok(f) => {
                let f = Arc::new(f);
                {
                    let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
                    l.retain(|x| x.path != path);
                    l.push(f.clone());
                    if l.len() > 4 {
                        l.remove(0);
                    }
                }
                let replace = crate::subject_ui::take_redo(path, TOOL);
                fill_leaves(local, &f, &replace);
                drop_untouched_pending(local);
                if f.faces == 0 { crate::tr("Kein Gesicht erkannt").to_string() } else { crate::trf!("{n} Gesichter gefunden", n = f.faces) }
            }
            Err(e) => {
                // computed again: the previous parts stay; a new, unchanged part that was never computed goes away
                crate::subject_ui::take_redo(path, TOOL);
                drop_untouched_pending(local);
                if progress::is_cancel(&e) { crate::tr(progress::CANCELLED).to_string() } else { crate::trf!("Berechnung fehlgeschlagen: {e}", e = e) }
            }
        };
        *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), text));
        return false;
    }
    let mut pending = false;
    if let Some(d) = local.as_ref() {
        for l in &d.layers {
            l.mask.visit(&mut |m| {
                if let Mask::Raster { raster, source: RasterSource::PersonPart { .. }, .. } = m {
                    pending |= raster.is_empty() || masks::raster::resolve(raster).is_none();
                }
            });
        }
    }
    if !pending {
        return false;
    }
    if let Some(f) = last(path) {
        fill_leaves(local, &f, &[]);
        return false;
    }
    let tried = message(path).is_some();
    !tried && !busy() && available().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic face: jaw line, eyes, brows, lips around (50, 60) in a 100 × 120 image.
    fn face() -> Face {
        let ring = |cx: f32, cy: f32, rx: f32, ry: f32, n: usize| (0..n).map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            [cx + rx * a.cos(), cy + ry * a.sin()]
        }).collect::<Vec<_>>();
        Face {
            contour: (0..=16).map(|i| {
                let a = std::f32::consts::PI * i as f32 / 16.0;
                [50.0 - 30.0 * a.cos(), 55.0 + 35.0 * a.sin()]
            }).collect(),
            eyes: [ring(38.0, 50.0, 6.0, 3.0, 8), ring(62.0, 50.0, 6.0, 3.0, 8)],
            brows: [vec![[30.0, 43.0], [36.0, 41.0], [44.0, 42.0]], vec![[56.0, 42.0], [64.0, 41.0], [70.0, 43.0]]],
            outer_lips: ring(50.0, 75.0, 10.0, 5.0, 12),
            inner_lips: ring(50.0, 75.0, 7.0, 2.5, 8),
        }
    }

    #[test]
    fn parts_land_where_the_landmarks_are() {
        let (w, h) = (100, 120);
        let f = [face()];
        let at = |part, x: usize, y: usize| part_weights(&f, None, None, w, h, part)[y * w + x];
        // face: cheek and forehead in, outside the hull out
        assert!(at(PersonPart::Face, 30, 65) > 0.9);
        assert!(at(PersonPart::Face, 50, 25) > 0.9, "forehead raised above the brows");
        assert!(at(PersonPart::Face, 5, 60) < 0.01 && at(PersonPart::Face, 50, 5) < 0.01);
        // eyes, brows, lips, mouth
        assert!(at(PersonPart::Eyes, 38, 50) > 0.9 && at(PersonPart::Eyes, 50, 50) < 0.1);
        assert!(at(PersonPart::Brows, 36, 41) > 0.7 && at(PersonPart::Brows, 36, 50) < 0.1);
        assert!(at(PersonPart::Lips, 50, 71) > 0.5 && at(PersonPart::Lips, 50, 75) < 0.3, "lips without the opening");
        assert!(at(PersonPart::Mouth, 50, 75) > 0.5 && at(PersonPart::Mouth, 50, 71) < 0.3);
        // skin: cheek in, eyes and mouth out
        assert!(at(PersonPart::Skin, 30, 65) > 0.9);
        assert!(at(PersonPart::Skin, 38, 50) < 0.1 && at(PersonPart::Skin, 50, 75) < 0.1);
    }

    #[test]
    fn the_face_is_cut_to_the_person_and_the_skin_to_its_colour() {
        let (w, h) = (100, 120);
        let f = [face()];
        // the person only on the left half
        let person: Vec<f32> = (0..w * h).map(|i| if i % w < 50 { 1.0 } else { 0.0 }).collect();
        let face = part_weights(&f, Some(&person), None, w, h, PersonPart::Face);
        assert!(face[65 * w + 30] > 0.9 && face[65 * w + 70] < 0.01);
        // a differently coloured patch (hair over the cheek) leaves the skin
        let rgb: Vec<f32> = (0..w * h).flat_map(|i| if i % w > 60 && i / w > 55 && i / w < 70 { [0.05, 0.04, 0.03] } else { [0.6, 0.4, 0.3] }).collect();
        let skin = part_weights(&f, None, Some(&rgb), w, h, PersonPart::Skin);
        assert!(skin[65 * w + 30] > 0.9 && skin[62 * w + 72] < 0.1);
        // no faces: nothing
        assert!(part_weights(&[], None, None, w, h, PersonPart::Face).iter().all(|v| *v == 0.0));
    }
}
