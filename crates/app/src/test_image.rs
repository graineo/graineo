//! Test hook: screenshots of the app's own window only, taken by the app itself
//! (egui `ViewportCommand::Screenshot`) – without simulating mouse/keyboard and without
//! screen recording permission.
//!
//! `GRAINEO_SCREENSHOT="a.png@6,b.png@12"`: save `a.png` after 6 s, `b.png` after 12 s.
//! `GRAINEO_QUIT=seconds`: quit the app afterwards.

use std::sync::Mutex;

use eframe::egui;

struct Snapshot {
    start: std::time::Instant,
    tasks: Vec<(String, f64)>,
    waiting: Option<String>,
}

static STATE: Mutex<Option<Snapshot>> = Mutex::new(None);

pub fn step(ctx: &egui::Context) {
    let Ok(spec) = crate::env::var("GRAINEO_SCREENSHOT") else {
        finish(ctx);
        return;
    };
    let mut g = STATE.lock().unwrap();
    let st = g.get_or_insert_with(|| Snapshot {
        start: std::time::Instant::now(),
        tasks: spec.split(',').filter_map(|t| t.rsplit_once('@').and_then(|(p, s)| Some((p.trim().to_string(), s.trim().parse().ok()?)))).collect(),
        waiting: None,
    });
    let t = st.start.elapsed().as_secs_f64();
    if st.waiting.is_none() {
        if let Some(pos) = st.tasks.iter().position(|(_, s)| t >= *s) {
            let (p, _) = st.tasks.remove(pos);
            st.waiting = Some(p.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                let [w, h] = img.size;
                let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Some(b) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                    match b.save(&p) {
                        Ok(()) => crate::log(&format!("testbild saved: {p} ({w}×{h})")),
                        Err(e) => crate::log(&format!("testbild {p}: {e}")),
                    }
                }
                crate::photo_display::capture_note(&p);
                if let Some(st) = STATE.lock().unwrap().as_mut() {
                    st.waiting = None;
                }
            })));
        }
    }
    if !st.tasks.is_empty() || st.waiting.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    drop(g);
    finish(ctx);
}

fn finish(ctx: &egui::Context) {
    static START: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    if let Some(s) = crate::env::var("GRAINEO_QUIT").ok().and_then(|v| v.parse::<f64>().ok()) {
        let start = *START.lock().unwrap().get_or_insert_with(std::time::Instant::now);
        if start.elapsed().as_secs_f64() > s {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// Test hook: take a screenshot to `p` right away (e.g. from a test matrix).
pub fn add(ctx: &egui::Context, p: String) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
        let [w, h] = img.size;
        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        if let Some(b) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
            match b.save(&p) {
                Ok(()) => crate::log(&format!("testbild saved: {p} ({w}×{h})")),
                Err(e) => crate::log(&format!("testbild {p}: {e}")),
            }
        }
    })));
}

/// Test hook `GRAINEO_KEYS="Space@6,Escape@9,cmd+U@12"` (only with `GRAINEO_DATA_DIR`): presses these keys (egui names,
/// optionally with `cmd+`/`shift+`/`alt+` prefixes) once each after the given seconds, as if typed – for screenshots of keyboard paths. Called before the keys
/// are evaluated in the frame.
pub fn keys(ctx: &egui::Context) {
    static KEYS: Mutex<Option<(std::time::Instant, Vec<(egui::Key, egui::Modifiers, f64)>)>> = Mutex::new(None);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let Ok(spec) = crate::env::var("GRAINEO_KEYS") else { return };
    let mut g = KEYS.lock().unwrap();
    let (start, list) = g.get_or_insert_with(|| {
        let parse = |k: &str| {
            let mut m = egui::Modifiers::NONE;
            let mut name = k.trim();
            while let Some((pre, rest)) = name.split_once('+') {
                match pre.to_ascii_lowercase().as_str() {
                    "cmd" => m = m | egui::Modifiers::COMMAND,
                    "shift" => m = m | egui::Modifiers::SHIFT,
                    "alt" => m = m | egui::Modifiers::ALT,
                    _ => return None,
                }
                name = rest;
            }
            Some((egui::Key::from_name(name)?, m))
        };
        let list = spec.split(',').filter_map(|t| t.rsplit_once('@').and_then(|(k, s)| { let (key, m) = parse(k)?; Some((key, m, s.trim().parse().ok()?)) })).collect();
        (std::time::Instant::now(), list)
    });
    let t = start.elapsed().as_secs_f64();
    while let Some(pos) = list.iter().position(|(_, _, s)| t >= *s) {
        let (key, modifiers, _) = list.remove(pos);
        crate::log(&format!("test keys: {modifiers:?} {key:?} after {t:.1} s"));
        ctx.input_mut(|i| {
            i.modifiers = modifiers;
            i.events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers });
        });
    }
    if !list.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// A file dropped by the test hook (`drop:`).
#[derive(Debug)]
struct TestDrop(std::path::PathBuf);

impl egui::DroppedFile for TestDrop {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Test hook `GRAINEO_INPUT="focus:library-keyword-input@5,text:beach@6,key:Enter@7,click:820:400@8"` (only with
/// `GRAINEO_DATA_DIR`): real input events in the raw input (like typing and clicking, through every filter of the app):
/// `focus:<id>` focuses the text field with `egui::Id::unique(<id>)`, `text:` types text, `key:` presses a key (egui name, `shift+`/`alt+`/`cmd+` in front hold those),
/// `rclick:x:y` right-clicks there (context menu), `click:x:y` moves the pointer there and clicks (press, release in the next frame), `drag:x1:y1:x2:y2` presses at
/// (x1, y1), moves the pointer to (x2, y2) in ten steps (one per frame) and releases there, like dragging with the mouse.
/// `press:x:y`, `move:x:y` and `release:x:y` do single steps of it (for slow drags with pauses).
/// `mods:cmd`, `mods:shift`, `mods:cmd+shift` and `mods:none` hold or let go modifier keys (for ⌘/⇧-clicks and frames).
/// `drop:<path>|<path>` drops files or folders on the window.
/// The window counts as active meanwhile. Called from `raw_input_hook`.
pub fn input(ctx: &egui::Context, raw: &mut egui::RawInput) {
    /// start, scheduled entries, events still to send (one batch per frame)
    type Pending = (std::time::Instant, Vec<(String, f64)>, std::collections::VecDeque<Vec<egui::Event>>);
    static INPUT: Mutex<Option<Pending>> = Mutex::new(None);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let Ok(spec) = crate::env::var("GRAINEO_INPUT") else { return };
    let mut g = INPUT.lock().unwrap();
    let (start, list, queued) = g.get_or_insert_with(|| {
        let list = spec.split(',').filter_map(|t| t.rsplit_once('@').and_then(|(k, s)| Some((k.trim().to_string(), s.trim().parse().ok()?)))).collect();
        (std::time::Instant::now(), list, Default::default())
    });
    if let Some(events) = queued.pop_front() {
        raw.events.extend(events);
        // a press or drag in progress: no new entry in the same frame
        raw.focused = true;
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        return;
    }
    // the test window counts as the active window even when another program is in front (text fields
    // only show their focus, and react like it, in the active window)
    raw.focused = true;
    let t = start.elapsed().as_secs_f64();
    // one event per frame, in order
    if let Some(pos) = list.iter().position(|(_, s)| t >= *s) {
        let (what, _) = list.remove(pos);
        crate::log(&format!("test input: {what} after {t:.1} s"));
        match what.split_once(':') {
            Some(("focus", id)) => ctx.memory_mut(|m| m.request_focus(egui::Id::unique(id))),
            // files dropped on the window, separated by "|" (as from the Finder)
            Some(("drop", paths)) => raw.dropped_files.extend(paths.split('|').map(|p| std::sync::Arc::new(TestDrop(p.into())) as egui::DroppedFileHandle)),
            // "␣" stands for a space (the list is trimmed)
            Some(("text", s)) => raw.events.push(egui::Event::Text(s.replace('␣', " "))),
            Some(("mods", m)) => {
                let (command, shift) = (m.contains("cmd"), m.contains("shift"));
                raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers { shift, mac_cmd: command && cfg!(target_os = "macos"), command, ..Default::default() }));
            }
            Some(("key", k)) => {
                // "shift+Tab", "cmd+shift+H": with ⌘/⇧ held for this key
                let mut k = k;
                let mut modifiers = egui::Modifiers::NONE;
                loop {
                    if let Some(rest) = k.strip_prefix("shift+") {
                        modifiers.shift = true;
                        k = rest;
                    } else if let Some(rest) = k.strip_prefix("alt+") {
                        modifiers.alt = true;
                        k = rest;
                    } else if let Some(rest) = k.strip_prefix("cmd+") {
                        modifiers.command = true;
                        modifiers.mac_cmd = cfg!(target_os = "macos");
                        k = rest;
                    } else {
                        break;
                    }
                }
                if let Some(key) = egui::Key::from_name(k) {
                    raw.events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers });
                    raw.events.push(egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers });
                }
            }
            Some(("click", xy)) => {
                if let Some((x, y)) = xy.split_once(':').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    let pos = egui::pos2(x, y);
                    raw.events.push(egui::Event::PointerMoved(pos));
                    raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                    queued.push_back(vec![release(pos)]);
                }
            }
            Some(("rclick", xy)) => {
                // right click (context menu): press now, release in the next frame
                if let Some((x, y)) = xy.split_once(':').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    let pos = egui::pos2(x, y);
                    raw.events.push(egui::Event::PointerMoved(pos));
                    raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE });
                    queued.push_back(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: false, modifiers: egui::Modifiers::NONE }]);
                }
            }
            Some((kind @ ("press" | "move" | "release"), xy)) => {
                if let Some((x, y)) = xy.split_once(':').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    let pos = egui::pos2(x, y);
                    raw.events.push(egui::Event::PointerMoved(pos));
                    match kind {
                        "press" => raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }),
                        "release" => raw.events.push(release(pos)),
                        _ => {}
                    }
                }
            }
            Some(("drag", xy)) => {
                let n: Vec<f32> = xy.split(':').filter_map(|v| v.parse().ok()).collect();
                if let [x1, y1, x2, y2] = n[..] {
                    let (a, b) = (egui::pos2(x1, y1), egui::pos2(x2, y2));
                    raw.events.push(egui::Event::PointerMoved(a));
                    raw.events.push(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                    const STEPS: usize = 10;
                    for k in 1..=STEPS {
                        queued.push_back(vec![egui::Event::PointerMoved(a.lerp(b, k as f32 / STEPS as f32))]);
                    }
                    queued.push_back(vec![release(b)]);
                }
            }
            _ => {}
        }
    }
    if !list.is_empty() || !queued.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn release(pos: egui::Pos2) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }
}

/// Test hook `GRAINEO_NATIVE_KEYS="cmd+q@8,cmd+shift+z@5"` (only with `GRAINEO_DATA_DIR`, macOS): posts a real
/// key event (down and up) into the app's own event queue the given seconds after the first frame. It runs through
/// AppKit like a typed key – menu key equivalents (⌘Q, ⌘Z …) first, then the window and egui – so it checks the menu
/// shortcuts without remote-controlling the computer: the events never leave this process. A helper thread posts
/// them (allowed from any thread), so they also arrive while the window is covered and egui paints nothing.
pub fn native_keys(ctx: &egui::Context) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Ok(spec) = crate::env::var("GRAINEO_NATIVE_KEYS") else { return };
    let mut list: Vec<(String, f64)> = spec.split(',').filter_map(|t| t.rsplit_once('@').and_then(|(k, s)| Some((k.trim().to_lowercase(), s.trim().parse().ok()?)))).collect();
    list.sort_by(|a, b| a.1.total_cmp(&b.1));
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        for (combo, at) in list {
            if let Some(wait) = std::time::Duration::from_secs_f64(at).checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
            crate::log(&format!("test native key: {combo} after {:.1} s", start.elapsed().as_secs_f64()));
            post_native_key(&combo);
            ctx.request_repaint();
        }
    });
}

/// With `GRAINEO_NATIVE_KEYS`: log which pressed keys reach egui (to see whether the menu took them first).
pub fn log_keys(raw: &egui::RawInput) {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || crate::env::var_os("GRAINEO_NATIVE_KEYS").is_none() {
        return;
    }
    for e in &raw.events {
        if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
            crate::log(&format!("test native key: egui gets {key:?} cmd={} shift={} alt={}", modifiers.command, modifiers.shift, modifiers.alt));
        }
    }
}

#[cfg(target_os = "macos")]
fn post_native_key(combo: &str) {
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::msg_send;
    use objc2_core_foundation::CGPoint;
    use objc2_foundation::NSString;
    // "menu": list the key equivalents AppKit really has in the menu bar (diagnosis)
    if combo == "menu" {
        unsafe {
            let Some(app_class) = AnyClass::get(c"NSApplication") else { return };
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            let main: *mut AnyObject = msg_send![app, mainMenu];
            if main.is_null() {
                crate::log("test menu: no main menu");
                return;
            }
            let n: isize = msg_send![main, numberOfItems];
            for i in 0..n {
                let top: *mut AnyObject = msg_send![main, itemAtIndex: i];
                let sub: *mut AnyObject = msg_send![top, submenu];
                if sub.is_null() {
                    continue;
                }
                let m: isize = msg_send![sub, numberOfItems];
                for j in 0..m {
                    let item: *mut AnyObject = msg_send![sub, itemAtIndex: j];
                    let title: *mut NSString = msg_send![item, title];
                    let key: *mut NSString = msg_send![item, keyEquivalent];
                    let mask: usize = msg_send![item, keyEquivalentModifierMask];
                    let enabled: Bool = msg_send![item, isEnabled];
                    let (title, key) = ((*title).to_string(), (*key).to_string());
                    if !key.is_empty() {
                        crate::log(&format!("test menu: {title:?} key={:?} mask={mask:#x} enabled={}", key, enabled.as_bool()));
                    }
                }
            }
        }
        return;
    }
    // "close" = the window's red close button, "terminate" = Quit from the Dock (both like the user does them)
    if combo == "close" || combo == "terminate" {
        unsafe {
            let Some(app_class) = AnyClass::get(c"NSApplication") else { return };
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            if combo == "terminate" {
                let _: () = msg_send![app, performSelectorOnMainThread: objc2::sel!(terminate:), withObject: std::ptr::null_mut::<AnyObject>(), waitUntilDone: Bool::NO];
            } else {
                let windows: *mut AnyObject = msg_send![app, windows];
                let window: *mut AnyObject = if windows.is_null() { std::ptr::null_mut() } else { msg_send![windows, firstObject] };
                if !window.is_null() {
                    let _: () = msg_send![window, performSelectorOnMainThread: objc2::sel!(performClose:), withObject: std::ptr::null_mut::<AnyObject>(), waitUntilDone: Bool::NO];
                }
            }
        }
        return;
    }
    let mut flags: usize = 0;
    let mut key = "";
    for part in combo.split('+') {
        match part {
            "cmd" => flags |= 1 << 20,
            "shift" => flags |= 1 << 17,
            "ctrl" => flags |= 1 << 18,
            "alt" => flags |= 1 << 19,
            k => key = k,
        }
    }
    // ANSI virtual key codes (characters decide the menu match, the code the physical key)
    const CODES: &[(&str, u16)] = &[
        ("a", 0), ("s", 1), ("d", 2), ("f", 3), ("h", 4), ("g", 5), ("z", 6), ("x", 7), ("c", 8), ("v", 9), ("b", 11), ("q", 12),
        ("w", 13), ("e", 14), ("r", 15), ("y", 16), ("t", 17), ("1", 18), ("2", 19), ("3", 20), ("4", 21), ("6", 22), ("5", 23),
        ("=", 24), ("9", 25), ("7", 26), ("-", 27), ("8", 28), ("0", 29), ("]", 30), ("o", 31), ("u", 32), ("[", 33), ("i", 34),
        ("p", 35), ("l", 37), ("j", 38), ("'", 39), ("k", 40), ("\\", 42), (",", 43), ("/", 44), ("n", 45), ("m", 46), (".", 47),
        ("backspace", 51), ("escape", 53),
    ];
    let Some(&(_, code)) = CODES.iter().find(|(k, _)| *k == key) else {
        crate::log(&format!("test native key: unknown key {key}"));
        return;
    };
    let chars = match key {
        "backspace" => "\u{7f}".to_string(),
        "escape" => "\u{1b}".to_string(),
        k => k.to_string(),
    };
    let shown = if flags & (1 << 17) != 0 { chars.to_uppercase() } else { chars.clone() };
    unsafe {
        let (Some(app_class), Some(event_class)) = (AnyClass::get(c"NSApplication"), AnyClass::get(c"NSEvent")) else { return };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        let windows: *mut AnyObject = msg_send![app, windows];
        let window: *mut AnyObject = if windows.is_null() { std::ptr::null_mut() } else { msg_send![windows, firstObject] };
        let number: isize = if window.is_null() { 0 } else { msg_send![window, windowNumber] };
        // the window takes the key focus inside the app (as when the user works in it) without activating the
        // app: the event then goes through the key window first, like a typed key
        if !window.is_null() && crate::env::var_os("GRAINEO_NATIVE_KEYWINDOW").is_some() {
            let _: () = msg_send![window, makeKeyWindow];
            let key: *mut AnyObject = msg_send![app, keyWindow];
            crate::log(&format!("test native key: key window {}", if key.is_null() { "none" } else { "set" }));
        }
        let characters = NSString::from_str(&shown);
        // like a typed key: "ignoring modifiers" still contains the effect of ⇧ (⌘⇧Z → "Z")
        let plain = NSString::from_str(&shown);
        for kind in [10usize, 11usize] {
            // 10 = key down, 11 = key up
            let event: *mut AnyObject = msg_send![event_class, keyEventWithType: kind, location: CGPoint::new(0.0, 0.0), modifierFlags: flags, timestamp: 0.0f64, windowNumber: number, context: std::ptr::null_mut::<AnyObject>(), characters: &*characters, charactersIgnoringModifiers: &*plain, isARepeat: Bool::NO, keyCode: code];
            if !event.is_null() {
                let _: () = msg_send![app, postEvent: event, atStart: Bool::NO];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn post_native_key(_combo: &str) {}
