//! Focus area of the camera over the image in single view (toggle ⇧F, toolbar).
//!
//! The areas come from the file's metadata ([`raw::focus`]), read in the background once per file and kept in memory
//! (nothing is stored in the library yet). They are normalised in the unrotated image; the quarter turns of the frame
//! (camera orientation + rotation by the user) bring them upright, then the same way as the face frames
//! ([`crate::face_frames`]): perspective, fine angle and crop via [`Frame`]. Lens distortion is not taken into account.
//!
//! Drawing: the area the camera reports as focused in strong green, areas that were only selected dashed; corners
//! instead of a full frame so the subject stays visible. A point without size (Fujifilm) is a small circle with ticks.
use develop::geometry::Frame;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use raw::focus::{FocusField, FocusInfo, FocusSource};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// Colour of the frames: green like a camera's focus confirmation (not gold, which marks the main button).
pub const GREEN: Color32 = Color32::from_rgb(72, 214, 96);
/// Selected but not reported as focused.
const GREEN_DIM: Color32 = Color32::from_rgb(150, 225, 160);

#[derive(Clone)]
enum Slot {
    Loading,
    Done(Option<Arc<FocusInfo>>),
}

/// Read results per file (also failures, so a file is read only once per session).
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const CACHE_MAX: usize = 512;

/// Focus data of a file: `None` while it is being read (a background thread is started on the first request).
fn lookup(ctx: &egui::Context, path: &Path) -> Option<Option<Arc<FocusInfo>>> {
    let mut c = CACHE.lock().unwrap();
    match c.get(path) {
        Some(Slot::Done(i)) => return Some(i.clone()),
        Some(Slot::Loading) => return None,
        None => {}
    }
    if c.len() >= CACHE_MAX {
        c.retain(|_, s| matches!(s, Slot::Loading));
    }
    c.insert(path.to_path_buf(), Slot::Loading);
    drop(c);
    let (p, ctx) = (path.to_path_buf(), ctx.clone());
    let spawned = std::thread::Builder::new().name("focus-read".into()).spawn(move || {
        let t = std::time::Instant::now();
        let info = raw::focus::read(&p).map(Arc::new);
        // the file name only, never the folders (the log is meant to be shareable)
        let name = p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        crate::log(&format!("focus {name}: {} field(s) in {:.0?}", info.as_ref().map_or(0, |i| i.fields.len()), t.elapsed()));
        CACHE.lock().unwrap().insert(p, Slot::Done(info));
        ctx.request_repaint();
    });
    if spawned.is_err() {
        CACHE.lock().unwrap().insert(path.to_path_buf(), Slot::Done(None));
    }
    None
}

thread_local! {
    /// Last drawn frame: file, centre of the main area in the crop (0 … 1), screen rectangles of the areas with
    /// their screen centres
    static LAST: RefCell<Option<(PathBuf, Option<[f32; 2]>, Vec<(Rect, Pos2)>, f64)>> = const { RefCell::new(None) };
    /// When the status text of a file was first shown (it fades after a few seconds)
    static STATUS_SINCE: RefCell<Option<(PathBuf, f64)>> = const { RefCell::new(None) };
}

/// Normalised point of the unrotated image → point in the crop (0 … 1).
fn to_crop(p: [f32; 2], frame: &Frame) -> [f32; 2] {
    let (ow, oh) = frame.oriented();
    let d = crate::face_frames::rotate_quarters(p, frame.q);
    frame.from_ori(frame.ori_to_persp([d[0] * ow, d[1] * oh]))
}

fn to_screen(p: [f32; 2], rect: Rect, frame: &Frame) -> Pos2 {
    let u = to_crop(p, frame);
    Pos2::new(rect.min.x + u[0] * rect.width(), rect.min.y + u[1] * rect.height())
}

/// Screen rectangle of an area (enclosing its four corners); `rect` is the whole crop on screen.
fn screen_rect(g: &FocusField, rect: Rect, frame: &Frame) -> Rect {
    let [cx, cy, w, h] = g.rect;
    let corners = [[cx - w / 2.0, cy - h / 2.0], [cx + w / 2.0, cy - h / 2.0], [cx + w / 2.0, cy + h / 2.0], [cx - w / 2.0, cy + h / 2.0]].map(|p| to_screen(p, rect, frame));
    Rect::from_points(&corners)
}

/// The area that counts for zooming: the first one reported as focused, else the first selected one.
fn main_field(info: &FocusInfo) -> Option<&FocusField> {
    info.fields.iter().find(|g| g.achieved).or_else(|| info.fields.iter().find(|g| g.selected))
}

fn tooltip(g: &FocusField, info: &FocusInfo) -> String {
    let what = match g.source {
        FocusSource::Canon if g.achieved => crate::tr("Fokusfeld der Kamera: scharf gemeldet"),
        FocusSource::Canon => crate::tr("Fokusfeld der Kamera: gewählt"),
        FocusSource::Sony => crate::tr("Fokusstelle der Kamera"),
        FocusSource::Fujifilm if info.focus_warning => crate::tr("Fokuspunkt der Kamera: Kamera meldet unscharf"),
        FocusSource::Fujifilm => crate::tr("Fokuspunkt der Kamera"),
        FocusSource::Exif => crate::tr("Motivbereich laut Kamera"),
    };
    format!("{what}\n{}", crate::trf!("Klick oder {k}: 100 % an dieser Stelle", k = crate::shortcuts::display("zoom")))
}

/// Corner marks of `r` (polylines), each `len` long.
fn corners(r: Rect, len: f32) -> [[Pos2; 3]; 4] {
    let (a, b) = (r.min, r.max);
    [
        [Pos2::new(a.x, a.y + len), a, Pos2::new(a.x + len, a.y)],
        [Pos2::new(b.x - len, a.y), Pos2::new(b.x, a.y), Pos2::new(b.x, a.y + len)],
        [Pos2::new(b.x, b.y - len), b, Pos2::new(b.x - len, b.y)],
        [Pos2::new(a.x + len, b.y), Pos2::new(a.x, b.y), Pos2::new(a.x, b.y - len)],
    ]
}

fn line(painter: &egui::Painter, pts: &[Pos2], strong: bool) {
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(if strong { 4.0 } else { 3.0 }, Color32::from_black_alpha(130))));
    if strong {
        painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(2.0, GREEN)));
    } else {
        painter.extend(egui::Shape::dashed_line(pts, Stroke::new(1.5, GREEN_DIM), 3.0, 2.5));
    }
}

/// Paints one area; returns its screen rectangle (for clicks and the tooltip).
fn paint_field(painter: &egui::Painter, g: &FocusField, rect: Rect, frame: &Frame) -> (Rect, Pos2) {
    let strong = g.achieved;
    if g.is_point() {
        let c = to_screen([g.rect[0], g.rect[1]], rect, frame);
        let r = 9.0;
        let ring: Vec<Pos2> = (0..=40).map(|k| c + egui::Vec2::angled(k as f32 / 40.0 * std::f32::consts::TAU) * r).collect();
        line(painter, &ring, strong);
        for d in [egui::vec2(1.0, 0.0), egui::vec2(-1.0, 0.0), egui::vec2(0.0, 1.0), egui::vec2(0.0, -1.0)] {
            line(painter, &[c + d * (r + 2.0), c + d * (r + 7.0)], strong);
        }
        return (Rect::from_center_size(c, egui::vec2(2.0 * r + 14.0, 2.0 * r + 14.0)), c);
    }
    let mut s = screen_rect(g, rect, frame);
    // never smaller than a clear mark on screen
    let min = 16.0;
    if s.width() < min || s.height() < min {
        s = Rect::from_center_size(s.center(), egui::vec2(s.width().max(min), s.height().max(min)));
    }
    let len = (s.width().min(s.height()) * 0.28).clamp(5.0, 18.0);
    for c in corners(s, len) {
        line(painter, &c, strong);
    }
    (s, s.center())
}

/// Draws the focus areas of `path` over the image (`rect`: whole crop on screen, `avail`: visible area). Returns a
/// short status text for a badge (no data, manual focus, outside the crop) during the first seconds on a photo.
pub fn draw(ui: &mut egui::Ui, avail: Rect, rect: Rect, frame: &Frame, path: &Path) -> Option<String> {
    let painter = ui.painter().with_clip_rect(avail);
    let now = ui.input(|i| i.time);
    let mut drawn = Vec::new();
    let mut target = None;
    let status = match lookup(ui.ctx(), path) {
        None => None,
        Some(None) => Some(crate::tr("Kein Fokusfeld in dieser Datei").to_string()),
        Some(Some(info)) if info.manual => Some(crate::tr("Manuell fokussiert – kein Fokusfeld").to_string()),
        Some(Some(info)) => {
            let main = main_field(&info).copied();
            let mut outside = 0;
            let shown: Vec<&FocusField> = info.fields.iter().filter(|g| g.achieved || g.selected).collect();
            for (k, g) in shown.iter().enumerate() {
                let u = to_crop([g.rect[0], g.rect[1]], frame);
                if !(0.0..=1.0).contains(&u[0]) || !(0.0..=1.0).contains(&u[1]) {
                    outside += 1;
                    continue;
                }
                let (r, c) = paint_field(&painter, g, rect, frame);
                if Some(**g) == main {
                    target = Some(u);
                }
                let resp = ui.interact(r.expand(4.0).intersect(avail), egui::Id::new(("focus-field", k)), egui::Sense::hover());
                crate::lr::hover_tip(ui, &resp, &tooltip(g, &info));
                drawn.push((r.expand(4.0), c));
            }
            (outside > 0 && outside == shown.len()).then(|| crate::tr("Fokusfeld liegt außerhalb des Ausschnitts").to_string())
        }
    };
    LAST.with(|l| *l.borrow_mut() = Some((path.to_path_buf(), target, drawn, now)));
    let status = status?;
    // the badge stays for a few seconds per photo
    let since = STATUS_SINCE.with(|s| {
        let mut s = s.borrow_mut();
        match s.as_ref() {
            Some((p, t)) if p == path => *t,
            _ => {
                *s = Some((path.to_path_buf(), now));
                now
            }
        }
    });
    if now - since < 4.0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(4.05 - (now - since)));
        Some(status)
    } else {
        None
    }
}

/// Forget the badge timer (switching the display on again shows the status anew).
pub fn reset_status() {
    STATUS_SINCE.with(|s| *s.borrow_mut() = None);
}

/// Centre of the main area of `path` in the crop (0 … 1), as last drawn – the target for Z.
pub fn target(path: &Path, now: f64) -> Option<[f32; 2]> {
    LAST.with(|l| l.borrow().as_ref().filter(|(p, _, _, t)| p == path && now - t < 1.0).and_then(|(_, u, _, _)| *u))
}

/// Screen centre of the area under `p` (last drawn frame): a click there zooms to the area's centre.
pub fn hit(p: Pos2, now: f64) -> Option<Pos2> {
    LAST.with(|l| l.borrow().as_ref().filter(|(_, _, _, t)| now - t < 1.0).and_then(|(_, _, d, _)| d.iter().find(|(r, _)| r.contains(p)).map(|(_, c)| *c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(cx: f32, cy: f32) -> FocusField {
        FocusField { rect: [cx, cy, 0.1, 0.1], achieved: true, selected: true, source: FocusSource::Canon }
    }

    #[test]
    fn follows_camera_and_user_rotation() {
        // sensor 6000 × 4000, area top left (0.1, 0.2); a quarter turn clockwise (camera or user) puts it top right
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 600.0));
        for q in 0..4u8 {
            let frame = Frame::full(6000, 4000, q);
            let r = if q % 2 == 1 { rect } else { Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 400.0)) };
            let c = screen_rect(&field(0.1, 0.2), r, &frame).center();
            let want = match q {
                0 => [0.1, 0.2],
                1 => [0.8, 0.1],
                2 => [0.9, 0.8],
                _ => [0.2, 0.9],
            };
            assert!((c.x / r.width() - want[0]).abs() < 1e-3 && (c.y / r.height() - want[1]).abs() < 1e-3, "q={q}: {c:?}");
        }
    }

    #[test]
    fn crop_moves_the_area_and_outside_is_outside() {
        // crop: right half of a 6000 × 4000 image
        let mut frame = Frame::full(6000, 4000, 0);
        frame.cx = 4500.0;
        frame.w = 3000.0;
        let u = to_crop([0.75, 0.5], &frame);
        assert!((u[0] - 0.5).abs() < 1e-4 && (u[1] - 0.5).abs() < 1e-4);
        assert!(to_crop([0.2, 0.5], &frame)[0] < 0.0);
    }

    #[test]
    fn main_field_prefers_focused() {
        let a = FocusField { achieved: false, ..field(0.1, 0.1) };
        let b = field(0.6, 0.6);
        let info = FocusInfo { orientation: 1, fields: vec![a, b], manual: false, focus_warning: false };
        assert_eq!(main_field(&info), Some(&b));
    }
}
