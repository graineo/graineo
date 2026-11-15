//! Short zoom transition; it changes only the visible crop, not the development.
use eframe::egui::{self, Vec2};

/// Save the previous view once and restore it when the mosaic is switched off.
pub fn mosaic_pose(on: bool, previous: &mut Option<ViewPose>, current: ViewPose, target: f32) -> ViewPose {
    if on { previous.take().unwrap_or(current) } else {
        *previous = Some(current);
        (target, current.1 * (target / current.0.max(1e-6)))
    }
}
pub fn maximum_factor(sensor: bool, darkroom: bool) -> f32 {
    if sensor { 32.0 } else if darkroom { 12.0 } else { 4.0 }
}
pub fn preset_steps(sensor: bool) -> &'static [f32] {
    if sensor { &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0] } else { &[1.0, 2.0, 4.0] }
}

pub const DURATION: f64 = 0.180;
pub type ViewPose = (f32, Vec2);
/// Image generation, fitted image size, visible area, comparison view.
pub type Frame = (usize, Vec2, Vec2, bool);
#[derive(Default)]
pub struct Transition {
    run: Option<Run>,
}
struct Run {
    start: f64,
    from: ViewPose,
    to: ViewPose,
    view_frame: Frame,
}
impl Transition {
    pub fn cancel(&mut self) {
        self.run = None;
    }
    pub fn active(&self) -> bool {
        self.run.is_some()
    }
    pub fn start(
        &mut self,
        now: f64,
        from: ViewPose,
        to: ViewPose,
        view_frame: Frame,
        reduced: bool,
    ) -> ViewPose {
        self.cancel();
        if reduced || (from.0 - to.0).abs() < 1e-5 && (from.1 - to.1).length_sq() < 1e-5 {
            return to;
        }
        self.run = Some(Run {
            start: now,
            from,
            to,
            view_frame,
        });
        from
    }
    pub fn step(&mut self, now: f64, view_frame: Frame, direct_gesture: bool) -> Option<ViewPose> {
        let run = self.run.as_ref()?;
        if run.view_frame != view_frame || direct_gesture {
            self.cancel();
            return None;
        }
        let t = ((now - run.start) / DURATION).clamp(0.0, 1.0) as f32;
        if t <= 0.0 {
            return Some(run.from);
        }
        if t >= 1.0 {
            let result = run.to;
            self.cancel();
            return Some(result);
        }
        // Cubic smoothstep: gentle start and stop, without overshoot.
        let u = t * t * (3.0 - 2.0 * t);
        let z = (run.from.0.ln() + (run.to.0.ln() - run.from.0.ln()) * u).exp();
        // Pan follows the same magnification. This keeps the clicked image point
        // under the pointer as long as the edge clamping allows it.
        let f = if (run.to.0 - run.from.0).abs() > 1e-5 {
            (z - run.from.0) / (run.to.0 - run.from.0)
        } else {
            u
        };
        Some((z, run.from.1 + (run.to.1 - run.from.1) * f))
    }
}
/// Up to this zoom the photo counts as fitted (everywhere: `zoom > 1.01` = zoomed in).
pub const FIT_SNAP: f32 = 1.01;

/// Fitted means fitted: a tiny trackpad swipe or pinch can leave the photo a fraction above "fit"
/// (and then a few pixels can be panned). Once no gesture and no zoom animation runs, such a state goes back to
/// exactly fit without offset. During a gesture nothing changes, zooming in from fit passes through these values.
/// Returns whether it changed something.
pub fn settle_fit(zoom: &mut f32, pan: &mut Vec2, busy: bool) -> bool {
    if busy || *zoom > FIT_SNAP || (*zoom == 1.0 && *pan == Vec2::ZERO) {
        return false;
    }
    *zoom = 1.0;
    *pan = Vec2::ZERO;
    true
}

/// Calm time after the last gesture event before [`settle_fit`] acts (trackpad events can pause for a frame or two
/// within one slow swipe).
const SETTLE_AFTER: f64 = 0.3;

/// Is a gesture or zoom animation going on, or did one end less than [`SETTLE_AFTER`] ago (see [`settle_fit`])?
pub fn view_busy(ctx: &egui::Context, transition: &Transition) -> bool {
    let id = egui::Id::unique("zoom-last-gesture");
    let now = ctx.input(|i| i.time);
    if transition.active() || direct_gesture(ctx, true) {
        ctx.data_mut(|d| d.insert_temp(id, now));
        return true;
    }
    let last = ctx.data(|d| d.get_temp::<f64>(id)).unwrap_or(f64::NEG_INFINITY);
    if now - last < SETTLE_AFTER {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(SETTLE_AFTER));
        return true;
    }
    false
}

pub fn direct_gesture(ctx: &egui::Context, enabled: bool) -> bool {
    !enabled
        || ctx.input(|i| {
            i.pointer.any_down()
                || i.zoom_delta() != 1.0
                || i.raw
                    .events
                    .iter()
                    .any(|e| matches!(e, egui::Event::MouseWheel { .. }))
                || i.smooth_scroll_delta != Vec2::ZERO
        })
}
/// Where the scroll input of this frame comes from. A trackpad (and its momentum) reports points,
/// a real mouse wheel lines (egui `MouseWheelUnit::Point` vs `Line`). ⌘ + scrolling is left out: egui
/// turns it into `zoom_delta`. `None`: no scroll event in this frame (the last known source applies,
/// because egui smooths wheel steps over several frames).
pub fn scroll_source(ctx: &egui::Context) -> Option<bool> {
    ctx.input(|i| source_of(&i.raw.events))
}

fn source_of(events: &[egui::Event]) -> Option<bool> {
    events.iter().rev().find_map(|e| match e {
        egui::Event::MouseWheel { unit, source, modifiers, .. } if !modifiers.command => Some(match source {
            egui::MouseWheelSource::Trackpad | egui::MouseWheelSource::Momentum => true,
            egui::MouseWheelSource::Wheel => false,
            _ => *unit == egui::MouseWheelUnit::Point,
        }),
        _ => None,
    })
}

/// What a scroll/pinch does in the single image of Develop (and the darkroom):
/// - pinch (and ⌘ + scrolling, which egui reports as `zoom_delta`): zoom steplessly around the fingers;
/// - real mouse wheel: zoom as before;
/// - trackpad two-finger swipe: pan as soon as the photo is no longer fitted; fitted, it zooms as before.
///
/// Returns (zoom factor, pan delta). Never anything else: panning/zooming only moves the view.
pub fn develop_gesture(pinch: f32, swipe: Vec2, trackpad: bool, alt: bool, zoomed: bool) -> (f32, Vec2) {
    let mut factor = pinch;
    if swipe == Vec2::ZERO {
        return (factor, Vec2::ZERO);
    }
    if trackpad && zoomed && !alt {
        return (factor, swipe);
    }
    factor *= 1.0 + (swipe.y + swipe.x) * if alt { 0.002 } else { 0.004 };
    (factor, Vec2::ZERO)
}

/// The macOS default is only read, never changed.
pub fn reduced_motion() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::{
            msg_send,
            runtime::{AnyClass, AnyObject},
        };
        let Some(class) = AnyClass::get(c"NSWorkspace") else {
            return false;
        };
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        return !workspace.is_null()
            && msg_send![workspace, accessibilityDisplayShouldReduceMotion];
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Reproducible window captures without mouse/keyboard control. Only active in scratch.
/// GRAINEO_ZOOM_DEMO=1: zoom in from 4 s, out from 6 s; results via the screenshot helper.
pub fn demo(app: &mut crate::App, ctx: &egui::Context) {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none()
        || crate::env::var("GRAINEO_ZOOM_DEMO").as_deref() != Ok("1")
    {
        return;
    }
    let id = egui::Id::unique("zoom-demo");
    let stage = ctx.data(|d| d.get_temp::<u8>(id).unwrap_or(0));
    let now = app.now();
    if app.current.as_ref().is_some_and(|c| c.texture.is_some())
        && app.image_rect.is_some()
        && app.fit_size.min_elem() > 0.0
        && app.loading.is_none()
        && app.one_to_one > 1.0
        && ((stage == 0 && now >= 4.0) || (stage == 1 && now >= 6.0))
    {
        let to = if stage == 0 {
            let z = app.one_to_one.max(1.0);
            let rel = app.view_size * egui::vec2(0.1, -0.1);
            (z, rel - rel * z)
        } else {
            (1.0, Vec2::ZERO)
        };
        let view_frame = (
            app.open_generation
                .load(std::sync::atomic::Ordering::SeqCst),
            app.fit_size,
            app.view_size,
            app.compare,
        );
        (app.zoom, app.pan) = app.navigation.zoom.start(
            now,
            (app.zoom, app.pan),
            to,
            view_frame,
            reduced_motion(),
        );
        ctx.data_mut(|d| d.insert_temp(id, stage + 1));
        eprintln!(
            "ZOOM_DEMO start={now:.6} target={:.6} duration={DURATION}",
            to.0
        );
    }
    if now < 6.3 {
        ctx.request_repaint_after(std::time::Duration::from_millis(5));
    }
}
pub fn log_lines(ctx: &egui::Context, now: f64, view_pose: ViewPose) {
    if crate::env::var("GRAINEO_ZOOM_DEMO").as_deref() == Ok("1")
        && crate::env::var_os("GRAINEO_DATA_DIR").is_some()
    {
        eprintln!(
            "ZOOM_FRAME {now:.6} {:.6} {:.6} {:.6}",
            view_pose.0, view_pose.1.x, view_pose.1.y
        );
    }
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mosaic_saves_and_restores_the_original_view_after_manual_zoom() {
        let mut previous = None;
        let original = (2.0, Vec2::new(20.0, -40.0));
        let entered = mosaic_pose(false, &mut previous, original, 16.0);
        assert_eq!(entered, (16.0, Vec2::new(160.0, -320.0)));
        let manual = (32.0, Vec2::new(100.0, 70.0));
        assert_eq!(mosaic_pose(true, &mut previous, manual, 16.0), original);
        assert_eq!(previous, None);
    }
    #[test]
    fn sensor_zoom_limit_and_steps_are_isolated() {
        assert_eq!(maximum_factor(true, false), 32.0);
        assert_eq!(maximum_factor(false, false), 4.0);
        assert_eq!(maximum_factor(false, true), 12.0);
        assert_eq!(preset_steps(true), &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0]);
        assert_eq!(preset_steps(false), &[1.0, 2.0, 4.0]);
    }
    fn view_frame() -> Frame {
        (1, Vec2::new(900.0, 600.0), Vec2::new(1000.0, 700.0), false)
    }
    fn wheel(unit: egui::MouseWheelUnit, source: egui::MouseWheelSource, command: bool) -> egui::Event {
        egui::Event::MouseWheel { unit, delta: Vec2::new(0.0, 3.0), phase: egui::TouchPhase::Move, source, modifiers: egui::Modifiers { command, mac_cmd: command, ..Default::default() } }
    }
    #[test]
    fn trackpad_and_mouse_wheel_are_told_apart() {
        use egui::{MouseWheelSource as S, MouseWheelUnit as U};
        assert_eq!(source_of(&[]), None);
        assert_eq!(source_of(&[wheel(U::Point, S::Trackpad, false)]), Some(true));
        assert_eq!(source_of(&[wheel(U::Point, S::Momentum, false)]), Some(true));
        assert_eq!(source_of(&[wheel(U::Line, S::Wheel, false)]), Some(false));
        assert_eq!(source_of(&[wheel(U::Line, S::Unknown, false)]), Some(false));
        assert_eq!(source_of(&[wheel(U::Point, S::Unknown, false)]), Some(true));
        // ⌘ + scrolling becomes zoom_delta in egui: it decides nothing here
        assert_eq!(source_of(&[wheel(U::Point, S::Trackpad, true)]), None);
    }
    #[test]
    fn develop_gestures_as_agreed() {
        let swipe = Vec2::new(0.0, 20.0);
        // trackpad, zoomed in: pan, no zoom
        assert_eq!(develop_gesture(1.0, swipe, true, false, true), (1.0, swipe));
        // trackpad, fitted: zooms as before
        let (f, p) = develop_gesture(1.0, swipe, true, false, false);
        assert!(f > 1.0 && p == Vec2::ZERO);
        // real mouse wheel: always zooms, also when zoomed in
        let (f, p) = develop_gesture(1.0, swipe, false, false, true);
        assert!(f > 1.0 && p == Vec2::ZERO);
        // pinch alone: zoom by exactly its factor
        assert_eq!(develop_gesture(1.25, Vec2::ZERO, true, false, true), (1.25, Vec2::ZERO));
        // ⌥ + swipe zooms (finer)
        let (f, p) = develop_gesture(1.0, swipe, true, true, true);
        assert!(f > 1.0 && f < 1.0 + 20.0 * 0.004 && p == Vec2::ZERO);
    }
    #[test]
    fn endpoints_and_monotonicity_without_overshoot() {
        let mut a = Transition::default();
        let from = (1.0, Vec2::ZERO);
        let to = (3.25, Vec2::new(-250.0, 120.0));
        assert_eq!(a.start(2.0, from, to, view_frame(), false), from);
        let mut prev = 1.0;
        for i in 0..180 {
            let v = a.step(2.0 + i as f64 / 1000.0, view_frame(), false).unwrap();
            assert!(v.0 >= prev - 1e-6 && v.0 <= to.0 + 1e-6);
            prev = v.0;
        }
        assert_eq!(a.step(2.0 + DURATION, view_frame(), false), Some(to));
        assert!(!a.active());
        a.start(3.0, to, from, view_frame(), false);
        assert_eq!(a.step(3.0 + DURATION, view_frame(), false), Some(from));
    }
    #[test]
    fn image_point_stays_fixed_at_60_and_120_hz() {
        let rel = Vec2::new(130.0, -80.0);
        let target = 4.0;
        let mut max = 0.0f32;
        for fps in [60.0, 120.0] {
            let mut a = Transition::default();
            a.start(
                0.0,
                (1.0, Vec2::ZERO),
                (target, rel - rel * target),
                view_frame(),
                false,
            );
            for f in 0..=(DURATION * fps).ceil() as usize {
                if let Some((z, p)) = a.step(f as f64 / fps, view_frame(), false) {
                    max = max.max((rel * z + p - rel).length());
                }
            }
        }
        assert!(max < 0.001, "position error {max} points");
    }
    #[test]
    fn image_change_layout_and_direct_gestures_stop_immediately() {
        for (frame, gesture) in [
            ((2, view_frame().1, view_frame().2, false), false),
            ((1, Vec2::splat(10.0), view_frame().2, false), false),
            (view_frame(), true),
        ] {
            let mut a = Transition::default();
            a.start(
                0.0,
                (1.0, Vec2::ZERO),
                (4.0, Vec2::splat(20.0)),
                view_frame(),
                false,
            );
            assert!(a.step(0.05, frame, gesture).is_none());
            assert!(!a.active());
        }
    }
    #[test]
    fn reduced_motion_and_new_target_do_not_animate() {
        let mut a = Transition::default();
        let target = (4.0, Vec2::splat(100.0));
        assert_eq!(
            a.start(0.0, (1.0, Vec2::ZERO), target, view_frame(), true),
            target
        );
        assert!(!a.active());
        a.start(0.0, (1.0, Vec2::ZERO), target, view_frame(), false);
        let current = a.step(0.05, view_frame(), false).unwrap();
        assert_eq!(
            a.start(0.05, current, (1.0, Vec2::ZERO), view_frame(), false),
            current
        );
        assert_eq!(a.step(0.05, view_frame(), false), Some(current));
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::App;
    use eframe::App as _;
    #[test]
    fn whole_window_click_double_click_and_drag() {
        window_gestures(false);
    }
    #[test]
    fn profile_browser_must_not_block_click_zoom_and_drag() {
        window_gestures(true);
    }
    fn window_gestures(profile_browser: bool) {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!(
            "graineo-zoom-regression-{}-{profile_browser}",
            std::process::id()
        ));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(1800, 1200, image::Rgb([110, 80, 60]))
            .save(folder.join("sample.png"))
            .unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(folder);
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        a.profile_browser = profile_browser;
        let frame = |a: &mut App, time, events| {
            let mut input = egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1000.0),
                )),
                events,
                ..Default::default()
            };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        frame(&mut a, 10.0, vec![]);
        frame(&mut a, 10.5, vec![]);
        frame(&mut a, 11.0, vec![]);
        let p = a.image_rect.expect("image in the whole window").center() + egui::vec2(25.0, -15.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(
            &mut a,
            12.0,
            vec![egui::Event::PointerMoved(p), button(p, true)],
        );
        frame(&mut a, 12.02, vec![button(p, false)]);
        frame(&mut a, 12.321, vec![]);
        frame(&mut a, 12.55, vec![]);
        assert!(
            a.zoom > 1.1,
            "Klick im ganzen Fenster: zoom={}, frei={}, raster={}",
            a.zoom,
            a.image_click_free(&ctx),
            a.in_grid()
        );
        let before = a.pan;
        let q = p + egui::vec2(40.0, 25.0);
        frame(&mut a, 13.0, vec![button(p, true)]);
        frame(&mut a, 13.04, vec![egui::Event::PointerMoved(q)]);
        assert!(
            (a.pan - before - egui::vec2(40.0, 25.0)).length() < 1.0,
            "Ziehen: {:?} -> {:?}",
            before,
            a.pan
        );
        frame(&mut a, 13.06, vec![button(q, false)]);
        frame(&mut a, 13.5, vec![]);
        assert!(a.zoom > 1.1, "dragging must not trigger a click zoom");
        frame(&mut a, 14.0, vec![button(q, true)]);
        frame(&mut a, 14.02, vec![button(q, false)]);
        frame(&mut a, 14.321, vec![]);
        frame(&mut a, 14.55, vec![]);
        assert_eq!(a.zoom, 1.0, "a second single click must fit again");
        frame(&mut a, 15.0, vec![button(q, true)]);
        frame(&mut a, 15.02, vec![button(q, false)]);
        frame(&mut a, 15.321, vec![]);
        frame(&mut a, 15.55, vec![]);
        let z = a.zoom;
        frame(&mut a, 16.0, vec![egui::Event::Zoom(1.15)]);
        assert!(a.zoom > z, "trackpad zoom must respond directly");
        let pan = a.pan;
        frame(
            &mut a,
            16.2,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Trackpad,
                delta: egui::vec2(20.0, 10.0),
                modifiers: Default::default(),
            }],
        );
        assert!(
            (a.pan - pan).length() > 0.1,
            "Zwei-Finger-Wischen muss den Ausschnitt verschieben"
        );
        for n in 1..=25 {
            frame(&mut a, 16.2 + n as f64 * 0.03, vec![]);
        }
        let z = a.zoom;
        a.wb_picker = true;
        frame(&mut a, 17.2, vec![button(q, true)]);
        frame(&mut a, 17.22, vec![button(q, false)]);
        frame(&mut a, 17.55, vec![]);
        a.wb_picker = false;
        assert_eq!(
            a.zoom, z,
            "Die Pipette muss den Klick vor dem Zoom erhalten"
        );
        frame(&mut a, 18.0, vec![button(q, true)]);
        frame(&mut a, 18.02, vec![button(q, false)]);
        frame(&mut a, 18.1, vec![button(q, true)]);
        frame(&mut a, 18.12, vec![button(q, false)]);
        assert!(
            a.in_grid(),
            "double click in the whole window must lead to the grid"
        );
    }
}

#[cfg(test)]
mod settle_tests {
    use super::*;

    #[test]
    fn a_tiny_zoom_goes_back_to_fit_only_at_rest() {
        let (mut z, mut p) = (1.004, Vec2::new(3.0, -1.0));
        assert!(!settle_fit(&mut z, &mut p, true), "during the gesture nothing changes");
        assert_eq!(z, 1.004);
        assert!(settle_fit(&mut z, &mut p, false));
        assert_eq!((z, p), (1.0, Vec2::ZERO));
        let (mut z, mut p) = (1.0, Vec2::new(0.4, 0.0));
        assert!(settle_fit(&mut z, &mut p, false), "fitted with an offset: offset gone");
        assert_eq!(p, Vec2::ZERO);
        let (mut z, mut p) = (1.5, Vec2::new(30.0, 0.0));
        assert!(!settle_fit(&mut z, &mut p, false), "really zoomed in: untouched");
        assert_eq!((z, p), (1.5, Vec2::new(30.0, 0.0)));
    }
}
