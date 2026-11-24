//! Window geometry check (macOS): the drawing area must match the visible content area of the window.
//! Logs the geometry whenever it changes (view, metal layer, content layout rect, style mask, egui's screen).
//!
//! The window server and the drawing surface can disagree after a change of the window size that the window does
//! not report (full screen ↔ window, zoom, tiling): the surface keeps the old size and the top of the app lies under
//! the title bar (user report 22.11.2026). When the sizes stay apart, the window is nudged by one point and back,
//! which makes the window report its size again and the surface follow.

#[cfg(target_os = "macos")]
pub fn check(frame: &eframe::Frame, ctx: &eframe::egui::Context) {
    mac::check(frame, ctx);
}

#[cfg(not(target_os = "macos"))]
pub fn check(_frame: &eframe::Frame, _ctx: &eframe::egui::Context) {}

/// Do the surface (`drawable`, pixels), egui's screen (`screen`, pixels) and the visible content area (`visible`,
/// pixels) agree (within rounding)?
pub fn sizes_agree(drawable: [f64; 2], screen: [f64; 2], visible: [f64; 2]) -> bool {
    let close = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() <= 2.0 && (a[1] - b[1]).abs() <= 2.0;
    close(drawable, visible) && close(screen, visible)
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2_core_foundation::{CGRect, CGSize};
    use objc2_foundation::NSObject;
    use objc2_quartz_core::{CALayer, CAMetalLayer};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::sync::Mutex;

    pub fn check(frame: &eframe::Frame, ctx: &eframe::egui::Context) {
        use eframe::egui::{vec2, ViewportCommand};
        static LAST: Mutex<String> = Mutex::new(String::new());
        // frames in a row with disagreeing sizes, and the step of a running nudge
        static APART: Mutex<(u32, u8)> = Mutex::new((0, 0));
        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        // SAFETY: AppKit handle from winit, valid NSView; main thread (App::ui).
        let view: &NSObject = unsafe { h.ns_view.cast().as_ref() };
        let vframe: CGRect = unsafe { msg_send![view, frame] };
        let window: Option<Retained<NSObject>> = unsafe { msg_send![view, window] };
        let Some(window) = window else { return };
        let layout: CGRect = unsafe { msg_send![&*window, contentLayoutRect] };
        let wframe: CGRect = unsafe { msg_send![&*window, frame] };
        let mask: usize = unsafe { msg_send![&*window, styleMask] };
        let root: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
        let (lframe, drawable) = match root {
            Some(root) => {
                let metal = root.clone().downcast::<CAMetalLayer>().ok().or_else(|| unsafe { root.sublayers() }.and_then(|s| s.iter().find_map(|l| l.downcast::<CAMetalLayer>().ok())));
                match metal {
                    Some(m) => {
                        let f: CGRect = unsafe { msg_send![&*m, frame] };
                        let d: CGSize = unsafe { msg_send![&*m, drawableSize] };
                        (f, d)
                    }
                    None => (CGRect::default(), CGSize::default()),
                }
            }
            None => (CGRect::default(), CGSize::default()),
        };
        // test hook (`skew` in GRAINEO_WINDOW_TEST): the surface reported 100 px too tall until the nudge
        let drawable = if super::SKEW.load(std::sync::atomic::Ordering::Relaxed) { CGSize { width: drawable.width, height: drawable.height + 100.0 } } else { drawable };
        let scale: f64 = unsafe { msg_send![&*window, backingScaleFactor] };
        let ppp = ctx.pixels_per_point() as f64;
        let screen = ctx.content_rect().size();
        let visible = [layout.size.width * scale, layout.size.height * scale];
        let agree = super::sizes_agree([drawable.width, drawable.height], [screen.x as f64 * ppp, screen.y as f64 * ppp], visible);
        let live = unsafe { msg_send![view, inLiveResize] };
        if let Ok(mut a) = APART.lock() {
            match a.1 {
                1 => {
                    // second half of the nudge: back to the visible size
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(layout.size.width as f32, layout.size.height as f32)));
                    *a = (0, 2);
                }
                _ if agree || live || drawable.width == 0.0 => *a = (0, 0),
                _ => {
                    a.0 += 1;
                    // a few frames apart (a size change on its way settles by itself), not during a full-screen
                    // change and at most once in a row (the nudge itself counts again only after it has settled)
                    let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    if a.0 >= 10 && a.1 == 0 && !full {
                        crate::log(&format!(
                            "window geometry: surface {:.0}x{:.0}, screen {:.0}x{:.0} px, visible {:.0}x{:.0} px apart – window nudged",
                            drawable.width, drawable.height, screen.x as f64 * ppp, screen.y as f64 * ppp, visible[0], visible[1]
                        ));
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(layout.size.width as f32, layout.size.height as f32 + 1.0)));
                        super::SKEW.store(false, std::sync::atomic::Ordering::Relaxed);
                        *a = (0, 1);
                    } else if a.1 == 2 && a.0 >= 120 {
                        // the nudge did not help: try again only after a while (no loop of resizes)
                        a.1 = 0;
                    }
                    ctx.request_repaint();
                }
            }
        }
        let r = |c: CGRect| format!("{:.0},{:.0} {:.0}x{:.0}", c.origin.x, c.origin.y, c.size.width, c.size.height);
        let state = format!(
            "window {} mask {mask:#x} | view {} | content layout {} | layer {} drawable {:.0}x{:.0} | egui {:.0}x{:.0} pt",
            r(wframe),
            r(vframe),
            r(layout),
            r(lframe),
            drawable.width,
            drawable.height,
            screen.x,
            screen.y
        );
        if let Ok(mut g) = LAST.lock() {
            if *g != state {
                crate::log(&format!("window geometry: {state}"));
                *g = state;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_agree_within_rounding_only() {
        assert!(super::sizes_agree([2560.0, 1334.0], [2560.0, 1333.5], [2560.0, 1334.0]));
        // surface still at the full-screen size while the window has its title bar again
        assert!(!super::sizes_agree([2560.0, 1440.0], [2560.0, 1334.0], [2560.0, 1334.0]));
        // egui lays out for a taller screen than is visible
        assert!(!super::sizes_agree([2560.0, 1334.0], [2560.0, 1390.0], [2560.0, 1334.0]));
    }
}

static SKEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test hook (only with GRAINEO_DATA_DIR): GRAINEO_WINDOW_TEST="fs@3;window@7;unmax@10;max@13" sends the window
/// commands full screen on/off and maximise off/on at the given seconds (each once); `skew` pretends a surface of the
/// wrong size (the check must nudge the window).
pub fn test_step(ctx: &eframe::egui::Context) {
    use eframe::egui::ViewportCommand as C;
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let Ok(plan) = crate::env::var("GRAINEO_WINDOW_TEST") else { return };
    static DONE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let now = ctx.input(|i| i.time);
    let done = DONE.load(std::sync::atomic::Ordering::Relaxed);
    for (n, step) in plan.split(';').enumerate().skip(done) {
        let Some((what, at)) = step.split_once('@') else { return };
        if now < at.trim().parse::<f64>().unwrap_or(f64::MAX) {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        let cmd = match what.trim() {
            "fs" => C::Fullscreen(true),
            "window" => C::Fullscreen(false),
            "max" => C::Maximized(true),
            "unmax" => C::Maximized(false),
            "skew" => {
                SKEW.store(true, std::sync::atomic::Ordering::Relaxed);
                crate::log("window test: skew");
                DONE.store(n + 1, std::sync::atomic::Ordering::Relaxed);
                ctx.request_repaint();
                return;
            }
            _ => return,
        };
        crate::log(&format!("window test: {what}"));
        ctx.send_viewport_cmd(cmd);
        DONE.store(n + 1, std::sync::atomic::Ordering::Relaxed);
        ctx.request_repaint();
        return;
    }
}
