//! Display color management (macOS).
//!
//! The UI is sRGB (sRGB gamma). With the float surface (see `photo_display.rs`) wgpu tags the layer as extended
//! sRGB itself; the 8-bit surface (Windows, `GRAINEO_SURFACE=8`) is handled as follows.
//! eframe/egui-wgpu configures the surface with `SurfaceColorSpace::Auto` → `Srgb`, and
//! wgpu-hal (Metal) sets `CAMetalLayer.colorspace = nil` for it. A layer without a color space
//! is **not** color-managed by macOS: the values go unchanged into the color space of the
//! display. On a P3 display (MacBook Pro), sRGB colors therefore appear
//! oversaturated.
//!
//! Remedy: we tag the `CAMetalLayer` with `kCGColorSpaceSRGB`; the window server then converts
//! sRGB → display profile (like Safari or Preview). Because wgpu resets the color space to `nil`
//! on every reconfiguration (resize), [`apply`] checks it every
//! frame (two Objective-C calls, practically free) and sets it again if needed.
//!
//! The computed pixels stay untouched: only how macOS shows the finished
//! framebuffer values on screen changes (export, histogram, color picker stay the same).

use std::sync::OnceLock;

/// How the framebuffer content is interpreted on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Content is sRGB, macOS converts to the monitor profile (always, no setting).
    Srgb,
    /// No color management (old behaviour). Only for measurements via the
    /// environment variable `GRAINEO_DISPLAY=native`, not in the UI.
    Native,
}

fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| match crate::env::var("GRAINEO_DISPLAY").as_deref() {
        Ok("native") => Mode::Native,
        _ => Mode::Srgb,
    })
}

/// Call every frame from `App::ui`: sets the color space of the window's Metal layer.
pub fn apply(frame: &eframe::Frame) {
    #[cfg(target_os = "macos")]
    mac::apply(frame, mode());
    #[cfg(not(target_os = "macos"))]
    let _ = (frame, mode());
}

/// Every frame: the window asks for EDR (brightness above SDR white) only while an HDR photo is shown. wgpu turns
/// EDR on for the extended float surface; colours beyond sRGB need no EDR (extended sRGB below 0 / above 1 in
/// colour, values up to white). Logs the state of the window layer on change (format, colour space, EDR).
pub fn set_edr(frame: &eframe::Frame, on: bool) {
    #[cfg(target_os = "macos")]
    mac::set_edr(frame, on);
    #[cfg(not(target_os = "macos"))]
    let _ = (frame, on);
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Mode;
    use objc2::rc::Retained;
    use objc2::msg_send;
    use objc2_core_graphics::{kCGColorSpaceSRGB, CGColorSpace};
    use objc2_foundation::NSObject;
    use objc2_quartz_core::{CALayer, CAMetalLayer};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// Finds the `CAMetalLayer` that wgpu (via raw-window-metal) attached to the view:
    /// either the root layer itself or a direct sublayer.
    fn metal_layer(frame: &eframe::Frame) -> Option<Retained<CAMetalLayer>> {
        let handle = frame.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
        // SAFETY: AppKit handle from winit, valid NSView; we are on the main thread (App::ui).
        let view: &NSObject = unsafe { h.ns_view.cast().as_ref() };
        let root: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
        let root = root?;
        if let Ok(m) = root.clone().downcast::<CAMetalLayer>() {
            return Some(m);
        }
        let subs = unsafe { root.sublayers() }?;
        subs.iter().find_map(|l| l.downcast::<CAMetalLayer>().ok())
    }

    pub fn set_edr(frame: &eframe::Frame, on: bool) {
        use std::sync::Mutex;
        static SHOWN: Mutex<Option<String>> = Mutex::new(None);
        let Some(layer) = metal_layer(frame) else { return };
        if layer.wantsExtendedDynamicRangeContent() != on {
            layer.setWantsExtendedDynamicRangeContent(on);
        }
        // what the window layer really is (checked, not assumed)
        let name = layer.colorspace().and_then(|c| CGColorSpace::name(Some(&c))).map(|n| n.to_string()).unwrap_or_else(|| "none".into());
        let state = format!("pixel format {}, colour space {name}, EDR {}", { let f: usize = unsafe { msg_send![&*layer, pixelFormat] }; f }, layer.wantsExtendedDynamicRangeContent());
        if let Ok(mut g) = SHOWN.lock() {
            if g.as_deref() != Some(state.as_str()) {
                crate::log(&format!("display layer: {state}"));
                *g = Some(state);
            }
        }
    }

    pub fn apply(frame: &eframe::Frame, mode: Mode) {
        let Some(layer) = metal_layer(frame) else { return };
        let has = layer.colorspace().is_some();
        match mode {
            Mode::Srgb if !has => {
                // SAFETY: kCGColorSpaceSRGB is a valid CoreGraphics constant (since 10.5).
                let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
                layer.setColorspace(cs.as_deref());
            }
            Mode::Native if has => layer.setColorspace(None),
            _ => {}
        }
    }
}
