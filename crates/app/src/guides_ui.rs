//! "Lines" tool in Crop & Straighten: up to four guide lines (or one quadrilateral) drawn in the image determine the
//! perspective (see `develop::guides`). While a line is dragged the settings stay as they were (the image does not move
//! under the pointer); on release the new guides are committed as one step (⌘Z).

use develop::geometry::Frame;
use develop::guides::{Guides, Line, Role, MAX_LINES};
use develop::upright::Segment;
use egui::Color32;

/// Grip radius of an end point or corner (screen points).
const GRIP: f32 = 9.0;
/// Snap distance with ⇧ (screen points).
const SNAP: f32 = 12.0;
/// Side of the loupe (points) and its magnification relative to the view.
const LOUPE: f32 = 128.0;
const LOUPE_ZOOM: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// New line from `a` (upright pixels) to the pointer
    New { a: [f32; 2] },
    /// End point `end` (0/1) of line `line`
    End { line: usize, end: usize },
    /// Corner of the quadrilateral
    Corner { i: usize },
}

/// State of the tool (not saved).
pub struct GuidesUi {
    /// Guides when crop mode was opened (Esc restores them)
    pub saved: Option<Guides>,
    drag: Option<Drag>,
    /// Current position of the dragged point (upright pixels)
    at: [f32; 2],
    /// Show the whole rectified image with the area outside the photo (zoomed out)
    pub margins: bool,
    /// Crop after a change: the whole rectified image instead of the largest rectangle
    pub whole: bool,
    /// Edit the quadrilateral instead of lines
    pub quad_mode: bool,
    /// Detected edges for snapping (photo index)
    segments: Option<(usize, Vec<Segment>)>,
    /// Short feedback below the list (text, time)
    pub msg: Option<(String, f64)>,
}

impl Default for GuidesUi {
    fn default() -> Self {
        Self { saved: None, drag: None, at: [0.0; 2], margins: true, whole: false, quad_mode: false, segments: None, msg: None }
    }
}

/// Mapping between the rectified image (where the crop lives) and the screen.
pub struct View<'a> {
    pub scr: &'a dyn Fn([f32; 2]) -> egui::Pos2,
    pub src: &'a dyn Fn(egui::Pos2) -> [f32; 2],
}

/// Default quadrilateral: a centred rectangle covering half the image.
pub fn default_quad() -> [[f32; 2]; 4] {
    [[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]]
}

fn dist_seg(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> (f32, egui::Pos2) {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    let q = a + ab * t;
    (q.distance(p), q)
}

fn line_color(role: Role) -> Color32 {
    match role {
        Role::Vertical => crate::lr::overlay(235),
        Role::Horizontal => crate::lr::BLUE(),
    }
}

impl GuidesUi {
    /// Margins view active for this frame? (only with the Lines tool and a guided correction)
    pub fn shows_margins(&self, frame: &Frame) -> bool {
        self.margins && frame.hom.is_some()
    }

    /// The view frame in crop mode: with margins shown, the bounding box of the whole rectified image.
    pub fn view_frame(&self, full: Frame) -> Frame {
        if !self.shows_margins(&full) {
            return full;
        }
        let q = full.valid_quad();
        let (ow, oh) = full.oriented();
        let (x0, x1) = q.iter().fold((0.0f32, ow), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        let (y0, y1) = q.iter().fold((0.0f32, oh), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        // bounded, so a degenerate correction can never zoom out without end
        let (x0, x1, y0, y1) = (x0.max(-ow), x1.min(2.0 * ow), y0.max(-oh), y1.min(2.0 * oh));
        Frame { cx: (x0 + x1) / 2.0, cy: (y0 + y1) / 2.0, w: x1 - x0, h: y1 - y0, ..full }
    }

    /// Grey area outside the photo inside the view (`frame`: current correction).
    pub fn paint_margins(&self, painter: &egui::Painter, view: &View, frame: &Frame, area: [egui::Pos2; 4]) {
        let q = frame.valid_quad().map(|p| (view.scr)(p));
        let fill = crate::lr::gray(0x20);
        let mut mesh = egui::Mesh::default();
        for i in 0..4 {
            let j = (i + 1) % 4;
            let base = mesh.vertices.len() as u32;
            for p in [area[i], area[j], q[j], q[i]] {
                mesh.colored_vertex(p, fill);
            }
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base, base + 2, base + 3);
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.add(egui::Shape::closed_line(q.to_vec(), egui::Stroke::new(1.0, crate::lr::overlay(120))));
    }

    /// Draws the guides and handles the pointer. Returns new guides to commit (on release or a click on a line).
    #[allow(clippy::too_many_arguments)]
    pub fn overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, view: &View, frame: &Frame, guides: &Guides, tex: egui::TextureId, full: [usize; 2], index: usize, segments: impl FnOnce() -> Vec<Segment>) -> Option<Guides> {
        let (ow, oh) = frame.oriented();
        let to_px = |p: [f32; 2]| [p[0] * ow, p[1] * oh];
        let to_n = |p: [f32; 2]| [p[0] / ow, p[1] / oh];
        // upright pixels ↔ screen through the current correction
        let scr_u = |p: [f32; 2]| (view.scr)(frame.ori_to_persp(p));
        let src_u = |q: egui::Pos2| frame.persp_to_ori((view.src)(q));
        let shift = ui.input(|i| i.modifiers.shift);
        if shift && self.drag.is_some() && self.segments.as_ref().is_none_or(|(i, _)| *i != index) {
            self.segments = Some((index, segments()));
        }
        let snap = |p: egui::Pos2, segs: &Option<(usize, Vec<Segment>)>| -> egui::Pos2 {
            let Some((_, segs)) = segs.as_ref().filter(|_| shift) else { return p };
            let mut best: Option<(f32, egui::Pos2)> = None;
            for s in segs {
                let (a, b) = (scr_u(s.p0), scr_u(s.p1));
                // an end point wins, otherwise the nearest point on the edge
                let cand = [(a.distance(p) * 0.7, a), (b.distance(p) * 0.7, b), dist_seg(p, a, b)];
                for (d, q) in cand {
                    if d < SNAP && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, q));
                    }
                }
            }
            best.map_or(p, |(_, q)| q)
        };
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());

        // grips under the pointer
        let quad = guides.quad.filter(|_| self.quad_mode);
        let hit_grip = |p: egui::Pos2| -> Option<Drag> {
            if self.quad_mode {
                let q = quad?;
                return (0..4).find(|&i| scr_u(to_px(q[i])).distance(p) < GRIP).map(|i| Drag::Corner { i });
            }
            for (k, l) in guides.lines.iter().enumerate() {
                for (e, pt) in [l.p0, l.p1].iter().enumerate() {
                    if scr_u(to_px(*pt)).distance(p) < GRIP {
                        return Some(Drag::End { line: k, end: e });
                    }
                }
            }
            None
        };
        let hit_line = |p: egui::Pos2| -> Option<usize> {
            if self.quad_mode {
                return None;
            }
            guides.lines.iter().position(|l| dist_seg(p, scr_u(to_px(l.p0)), scr_u(to_px(l.p1))).0 < 6.0)
        };
        let hovered_grip = pointer.filter(|_| self.drag.is_none()).and_then(hit_grip);

        let mut out = None;
        let now = ui.input(|i| i.time);
        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(o) = origin {
                match hit_grip(o) {
                    Some(d) => self.drag = Some(d),
                    None if self.quad_mode => {}
                    None if guides.lines.len() >= MAX_LINES => {
                        self.msg = Some((crate::tr("4 Linien gesetzt").to_string(), now));
                    }
                    None => {
                        let a = src_u(snap(o, &self.segments));
                        self.drag = Some(Drag::New { a });
                    }
                }
                self.at = src_u(snap(o, &self.segments));
            }
        } else if resp.clicked() {
            if let Some(k) = pointer.and_then(hit_line) {
                let mut g = guides.clone();
                g.lines[k].role = g.lines[k].role.toggled();
                out = Some(g);
            }
        }
        if let (Some(_), Some(p)) = (self.drag, pointer.filter(|_| resp.dragged() || resp.drag_stopped())) {
            self.at = src_u(snap(p, &self.segments));
        }

        // the guides as they would be with the drag applied (drawn), committed on release
        let mut shown = guides.clone();
        match self.drag {
            Some(Drag::End { line, end }) if line < shown.lines.len() => {
                let l = &mut shown.lines[line];
                if end == 0 { l.p0 = to_n(self.at) } else { l.p1 = to_n(self.at) }
            }
            Some(Drag::Corner { i }) => {
                if let Some(q) = shown.quad.as_mut() {
                    q[i] = to_n(self.at);
                }
            }
            _ => {}
        }
        let new_line = match self.drag {
            Some(Drag::New { a }) => {
                let b = self.at;
                ((b[0] - a[0]).hypot(b[1] - a[1]) > 0.02 * ow.max(oh)).then(|| Line { p0: to_n(a), p1: to_n(b), role: Role::by_angle(to_n(a), to_n(b), ow / oh) })
            }
            _ => None,
        };

        // drawing
        let halo = egui::Stroke::new(3.5, Color32::from_black_alpha(130));
        let draw_line = |l: &Line, hot: bool| {
            let (a, b) = (scr_u(to_px(l.p0)), scr_u(to_px(l.p1)));
            let c = line_color(l.role);
            painter.line_segment([a, b], halo);
            painter.line_segment([a, b], egui::Stroke::new(if hot { 2.5 } else { 1.5 }, c));
            for p in [a, b] {
                painter.circle_filled(p, 5.0, Color32::from_black_alpha(150));
                painter.circle_stroke(p, 4.0, egui::Stroke::new(1.5, c));
            }
            // role mark in the middle: V / H
            let m = a.lerp(b, 0.5);
            let t = if l.role == Role::Vertical { "V" } else { "H" };
            let g = painter.layout_no_wrap(t.to_string(), egui::FontId::proportional(11.0), c);
            let r = egui::Rect::from_center_size(m + egui::vec2(12.0, 0.0), g.size() + egui::vec2(6.0, 2.0));
            painter.rect_filled(r, 0.0, Color32::from_black_alpha(160));
            painter.galley(r.min + egui::vec2(3.0, 1.0), g, c);
        };
        if self.quad_mode {
            if let Some(q) = shown.quad {
                let pts = q.map(|p| scr_u(to_px(p)));
                painter.add(egui::Shape::closed_line(pts.to_vec(), halo));
                painter.add(egui::Shape::closed_line(pts.to_vec(), egui::Stroke::new(1.5, crate::lr::BLUE())));
                for (i, p) in pts.iter().enumerate() {
                    let hot = matches!(self.drag, Some(Drag::Corner { i: k }) if k == i) || hovered_grip == Some(Drag::Corner { i });
                    painter.rect_filled(egui::Rect::from_center_size(*p, egui::vec2(11.0, 11.0)), 0.0, Color32::from_black_alpha(150));
                    painter.rect_stroke(egui::Rect::from_center_size(*p, egui::vec2(if hot { 11.0 } else { 9.0 }, if hot { 11.0 } else { 9.0 })), 0.0, egui::Stroke::new(1.5, crate::lr::overlay(235)), egui::StrokeKind::Inside);
                }
            }
        } else {
            let hover_line = pointer.filter(|_| self.drag.is_none()).and_then(hit_line);
            for (k, l) in shown.lines.iter().enumerate() {
                draw_line(l, hover_line == Some(k) || matches!(self.drag, Some(Drag::End { line, .. }) if line == k));
            }
            if let Some(l) = &new_line {
                draw_line(l, true);
            }
        }
        if hovered_grip.is_some() || self.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(if pointer.and_then(hit_line).is_some() { egui::CursorIcon::Default } else { egui::CursorIcon::Crosshair });
        }

        // loupe in a fixed corner (away from the pointer) while dragging
        if self.drag.is_some() {
            if let Some(p) = pointer {
                let left = p.x > rect.center().x;
                let corner = if left { rect.left_top() + egui::vec2(12.0, 12.0) } else { rect.right_top() + egui::vec2(-12.0 - LOUPE, 12.0) };
                self.paint_loupe(painter, egui::Rect::from_min_size(corner, egui::vec2(LOUPE, LOUPE)), view, frame, tex, full);
            }
            ui.ctx().request_repaint();
        }

        if resp.drag_stopped() {
            match self.drag.take() {
                Some(Drag::New { .. }) => {
                    if let Some(l) = new_line {
                        let mut g = guides.clone();
                        g.quad = None;
                        g.lines.push(l);
                        out = Some(g);
                    }
                }
                Some(Drag::End { .. } | Drag::Corner { .. }) => out = Some(shown),
                None => {}
            }
        }
        out
    }

    /// Magnified view around the dragged point (rectified image, like the view).
    fn paint_loupe(&self, painter: &egui::Painter, r: egui::Rect, view: &View, frame: &Frame, tex: egui::TextureId, full: [usize; 2]) {
        let c = frame.ori_to_persp(self.at);
        // screen points per rectified pixel in the view
        let k = (view.scr)([c[0] + 1.0, c[1]]).distance((view.scr)(c)).max(1e-4);
        let px_per_pt = 1.0 / (k * LOUPE_ZOOM);
        painter.rect_filled(r.expand(2.0), 0.0, Color32::from_black_alpha(200));
        let n = 12;
        let mut mesh = egui::Mesh::with_texture(tex);
        for j in 0..=n {
            for i in 0..=n {
                let (tx, ty) = (i as f32 / n as f32, j as f32 / n as f32);
                let d = egui::vec2((tx - 0.5) * r.width(), (ty - 0.5) * r.height());
                let q = [c[0] + d.x * px_per_pt, c[1] + d.y * px_per_pt];
                let s = frame.ori_to_src(frame.persp_to_ori(q));
                mesh.vertices.push(egui::epaint::Vertex { pos: r.min + egui::vec2(tx * r.width(), ty * r.height()), uv: egui::pos2(s[0] / full[0] as f32, s[1] / full[1] as f32), color: Color32::WHITE });
            }
        }
        for j in 0..n {
            for i in 0..n {
                let a = (j * (n + 1) + i) as u32;
                let b = a + n as u32 + 1;
                mesh.add_triangle(a, a + 1, b + 1);
                mesh.add_triangle(a, b + 1, b);
            }
        }
        painter.with_clip_rect(r).add(egui::Shape::mesh(mesh));
        let m = r.center();
        let st = egui::Stroke::new(1.0, crate::lr::overlay(220));
        painter.line_segment([m - egui::vec2(10.0, 0.0), m + egui::vec2(10.0, 0.0)], st);
        painter.line_segment([m - egui::vec2(0.0, 10.0), m + egui::vec2(0.0, 10.0)], st);
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::overlay(160)), egui::StrokeKind::Outside);
    }
}

/// Test hook: `GRAINEO_GUIDES="v:x0,y0,x1,y1;h:…"` (normalised upright coordinates) or `"quad:x0,y0,…,x3,y3"`.
pub fn from_test_env(v: &str) -> Guides {
    let mut g = Guides::default();
    for part in v.split(';') {
        let (kind, nums) = part.split_once(':').unwrap_or(("", part));
        let n: Vec<f32> = nums.split(',').filter_map(|t| t.trim().parse().ok()).collect();
        match (kind.trim(), n.len()) {
            ("quad", 8) => g.quad = Some([[n[0], n[1]], [n[2], n[3]], [n[4], n[5]], [n[6], n[7]]]),
            ("v" | "h", 4) if g.lines.len() < MAX_LINES => g.lines.push(Line { p0: [n[0], n[1]], p1: [n[2], n[3]], role: if kind.trim() == "v" { Role::Vertical } else { Role::Horizontal } }),
            ("aspect", 1) => g.aspect = n[0],
            ("amount", 1) => g.amount = n[0],
            _ => {}
        }
    }
    g
}
