//! Play videos in single image view. The picture is delivered by Apple's player (AVFoundation) as a layer of its own over
//! the image area – like the HDR layer, without Apple's own controls, so that keyboard and mouse stay with
//! graineo. Play/pause, timeline and sound are drawn by graineo itself below it (the same controls
//! when other systems later get their own player). Videos from Apple Photos that are only in iCloud
//! are fetched into the cache beforehand by [`crate::apple_photos_ui::original`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use eframe::egui;

use crate::lr;

/// Height of the control bar below the video
pub const BAR: f32 = 40.0;

/// What the single image view wants to show this frame: video in this rectangle (points).
pub struct View {
    pub rect: egui::Rect,
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Player {
    path: Option<PathBuf>,
    load: Option<Receiver<Result<PathBuf, String>>>,
    error: Option<String>,
    #[cfg(target_os = "macos")]
    mac: Option<mac::VideoPlayer>,
    /// Layer is in place (for the placeholder below)
    ready: bool,
    muted: bool,
}

impl Player {
    /// Is a video of this path loaded and ready to play?
    pub fn ready_for(&self, path: &Path) -> bool {
        self.ready && self.path.as_deref() == Some(path)
    }

    /// Text for the single image view while the video is not in place.
    pub fn hint(&self) -> Option<String> {
        if let Some(f) = &self.error {
            return Some(f.clone());
        }
        (self.load.is_some()).then(|| crate::tr("Video wird geladen …").to_string())
    }

    /// Control bar below the video (in egui). Returns: needs repaint (playing).
    pub fn bar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        #[cfg(target_os = "macos")]
        {
            let Some(s) = self.mac.as_ref().filter(|_| self.ready) else { return };
            let (pos, length, playing) = (s.position(), s.length(), s.playing());
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(12.0, 6.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            if symbol_button(&mut ui, if playing { Symbol::Pause } else { Symbol::Start }) {
                self.toggle_on();
            }
            ui.label(egui::RichText::new(library::video::duration_text(pos)).monospace().color(lr::TEXT()));
            let rest = ui.available_width() - 120.0;
            let mut p = pos;
            ui.spacing_mut().slider_width = rest.max(60.0);
            let r = crate::lr::slider_add(&mut ui, egui::Slider::new(&mut p, 0.0..=length.max(0.01)).show_value(false));
            if r.changed() {
                if let Some(s) = &self.mac {
                    s.jump(p);
                }
            }
            ui.label(egui::RichText::new(library::video::duration_text(length)).monospace().color(lr::TEXT_DIM()));
            if ui.add_sized([64.0, 24.0], egui::Button::new(if self.muted { crate::tr("Ton aus") } else { crate::tr("Ton an") })).clicked() {
                self.muted = !self.muted;
                if let Some(s) = &self.mac {
                    s.muted(self.muted);
                }
            }
            if playing {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (ui, rect);
        }
    }

    /// Play/pause (space bar).
    pub fn toggle_on(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(s) = &self.mac {
            if s.playing() {
                s.pause();
            } else {
                // at the end: from the beginning
                if s.position() >= s.length() - 0.05 {
                    s.jump(0.0);
                }
                s.play();
            }
        }
    }

    /// Call at the end of every frame: show, move or hide the layer.
    pub fn draw(&mut self, frame: &eframe::Frame, ctx: &egui::Context, view: Option<View>) {
        let Some(a) = view else {
            #[cfg(target_os = "macos")]
            if let Some(s) = &self.mac {
                s.hide_now();
            }
            return;
        };
        // another video: pause the old one, fetch the new one (from iCloud if necessary)
        if self.path.as_deref() != Some(a.path.as_path()) {
            #[cfg(target_os = "macos")]
            if let Some(s) = &self.mac {
                s.pause();
                s.hide_now();
            }
            self.path = Some(a.path.clone());
            self.ready = false;
            self.error = None;
            let (tx, rx) = channel();
            let p = a.path.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                let r = if library::apple_photos::in_photos_library(&p) { crate::apple_photos_ui::original(&p) } else { Ok(p) };
                let _ = tx.send(r);
                c.request_repaint();
            });
            self.load = Some(rx);
        }
        if let Some(rx) = &self.load {
            match rx.try_recv() {
                Ok(Ok(file)) => {
                    self.load = None;
                    #[cfg(target_os = "macos")]
                    {
                        if self.mac.is_none() {
                            self.mac = mac::VideoPlayer::new(frame);
                        }
                        match &self.mac {
                            Some(s) => {
                                s.open(&file, self.muted);
                                self.ready = true;
                            }
                            None => self.error = Some(crate::tr("Video lässt sich hier nicht abspielen").to_string()),
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = file;
                        self.error = Some(crate::tr("Video lässt sich hier nicht abspielen").to_string());
                    }
                }
                Ok(Err(e)) => {
                    self.load = None;
                    self.error = Some(e);
                }
                Err(_) => return,
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(s) = self.mac.as_ref().filter(|_| self.ready) {
            s.show(a.rect);
        }
        let _ = frame;
    }
}

#[derive(Clone, Copy)]
enum Symbol {
    Start,
    Pause,
}

/// Button with a drawn icon (the font has no playback glyphs).
fn symbol_button(ui: &mut egui::Ui, s: Symbol) -> bool {
    let r = ui.add_sized([34.0, 24.0], egui::Button::new(""));
    let c = r.rect.center();
    let color = lr::TEXT();
    match s {
        Symbol::Start => {
            ui.painter().add(egui::Shape::convex_polygon(vec![c + egui::vec2(-4.0, -6.0), c + egui::vec2(6.0, 0.0), c + egui::vec2(-4.0, 6.0)], color, egui::Stroke::NONE));
        }
        Symbol::Pause => {
            for dx in [-3.5, 3.5] {
                ui.painter().rect_filled(egui::Rect::from_center_size(c + egui::vec2(dx, 0.0), egui::vec2(3.0, 12.0)), 0.0, color);
            }
        }
    }
    r.clicked()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;

    use objc2::encode::{Encode, Encoding};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    use objc2_quartz_core::CALayer;

    #[link(name = "AVFoundation", kind = "framework")]
    #[link(name = "CoreMedia", kind = "framework")]
    extern "C" {
        fn CMTimeMakeWithSeconds(seconds: f64, timescale: i32) -> CMTime;
        fn CMTimeGetSeconds(t: CMTime) -> f64;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    // SAFETY: layout as CoreMedia's CMTime ({?=qiIq})
    unsafe impl Encode for CMTime {
        const ENCODING: Encoding = Encoding::Struct("?", &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING]);
    }

    fn seconds(t: CMTime) -> f64 {
        // invalid/indefinite times (flags without "valid") as 0
        if t.flags & 1 == 0 {
            return 0.0;
        }
        let s = unsafe { CMTimeGetSeconds(t) };
        if s.is_finite() { s } else { 0.0 }
    }

    /// Changes to the layer without animation, effective immediately (see hdr_anzeige).
    fn without_animation(f: impl FnOnce()) {
        unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
        }
        f();
        unsafe {
            let _: () = msg_send![class!(CATransaction), commit];
            let _: () = msg_send![class!(CATransaction), flush];
        }
    }

    pub struct VideoPlayer {
        root: Retained<CALayer>,
        layer: Retained<CALayer>,
        player: Retained<AnyObject>,
    }

    impl VideoPlayer {
        pub fn new(frame: &eframe::Frame) -> Option<VideoPlayer> {
            use objc2_foundation::NSObject;
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let handle = frame.window_handle().ok()?;
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
            // SAFETY: AppKit handle from winit, valid NSView; main thread
            let view: &NSObject = unsafe { h.ns_view.cast().as_ref() };
            let root: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
            let root = root?;
            unsafe {
                let player: Retained<AnyObject> = msg_send![class!(AVPlayer), new];
                // AVPlayerLayer is a CALayer subclass
                let layer: Retained<CALayer> = msg_send![class!(AVPlayerLayer), playerLayerWithPlayer: &*player];
                layer.setHidden(true);
                layer.setZPosition(11.0);
                root.addSublayer(&layer);
                Some(VideoPlayer { root, layer, player })
            }
        }

        pub fn open(&self, file: &Path, muted: bool) {
            unsafe {
                let p = NSString::from_str(&file.to_string_lossy());
                let url: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*p];
                let item: Retained<AnyObject> = msg_send![class!(AVPlayerItem), playerItemWithURL: &*url];
                let _: () = msg_send![&*self.player, replaceCurrentItemWithPlayerItem: &*item];
                let _: () = msg_send![&*self.player, setMuted: Bool::new(muted)];
            }
        }

        pub fn show(&self, r: egui::Rect) {
            use objc2_core_foundation::{CGPoint, CGRect, CGSize};
            let b = self.root.bounds();
            let y = if self.root.isGeometryFlipped() { r.top() as f64 } else { b.size.height - r.bottom() as f64 };
            let target_frame = CGRect { origin: CGPoint { x: r.left() as f64, y }, size: CGSize { width: r.width() as f64, height: r.height() as f64 } };
            if self.layer.isHidden() || self.layer.frame() != target_frame {
                without_animation(|| {
                    self.layer.setFrame(target_frame);
                    self.layer.setHidden(false);
                });
            }
        }

        pub fn hide_now(&self) {
            if !self.layer.isHidden() {
                self.pause();
                without_animation(|| self.layer.setHidden(true));
            }
        }

        pub fn play(&self) {
            unsafe {
                let _: () = msg_send![&*self.player, play];
            }
        }

        pub fn pause(&self) {
            unsafe {
                let _: () = msg_send![&*self.player, pause];
            }
        }

        pub fn playing(&self) -> bool {
            let r: f32 = unsafe { msg_send![&*self.player, rate] };
            r != 0.0
        }

        pub fn muted(&self, an: bool) {
            unsafe {
                let _: () = msg_send![&*self.player, setMuted: Bool::new(an)];
            }
        }

        pub fn position(&self) -> f64 {
            let t: CMTime = unsafe { msg_send![&*self.player, currentTime] };
            seconds(t)
        }

        pub fn length(&self) -> f64 {
            unsafe {
                let item: Option<Retained<AnyObject>> = msg_send![&*self.player, currentItem];
                let Some(item) = item else { return 0.0 };
                let t: CMTime = msg_send![&*item, duration];
                seconds(t)
            }
        }

        pub fn jump(&self, s: f64) {
            unsafe {
                let t = CMTimeMakeWithSeconds(s, 600);
                let null = CMTimeMakeWithSeconds(0.0, 600);
                let _: () = msg_send![&*self.player, seekToTime: t, toleranceBefore: null, toleranceAfter: null];
            }
        }
    }
}
