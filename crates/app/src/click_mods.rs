//! Modifier keys of a click (⌘, ⇧, ⌥) as they were when the mouse button went down.
//!
//! egui keeps one modifier state per frame: the last one of all events the frame collected. When a frame takes
//! longer (busy library, memory pressure), the release of the button and the release of ⌘ or ⇧ land in the same
//! frame, and the click was taken as a plain click: ⌘-click and ⇧-click in the grid then chose a single photo.
//! Here a click counts with the keys held at the press – stamped on the press event – together with the keys held
//! now. On macOS the press event also gets the keys the system reports as held, in case the window missed a
//! change of the modifier keys.
use eframe::egui;

fn key() -> egui::Id {
    egui::Id::unique("click-mods-press")
}

fn union(a: egui::Modifiers, b: egui::Modifiers) -> egui::Modifiers {
    egui::Modifiers { alt: a.alt || b.alt, ctrl: a.ctrl || b.ctrl, shift: a.shift || b.shift, mac_cmd: a.mac_cmd || b.mac_cmd, command: a.command || b.command }
}

/// `raw_input_hook`: press events of the primary button carry the modifier keys the system reports as held (macOS).
pub fn complete(raw: &mut egui::RawInput) {
    if !raw.events.iter().any(|e| matches!(e, egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: true, .. })) {
        return;
    }
    let held = system_modifiers();
    if held.is_none() {
        return;
    }
    for e in &mut raw.events {
        if let egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: true, modifiers, .. } = e {
            *modifiers = union(*modifiers, held);
        }
    }
}

/// The modifier keys for a click or drag with the primary button in this frame: those held at the last press of the
/// button (also when they were let go before this frame saw the release), together with those held now.
pub fn at_press(ctx: &egui::Context) -> egui::Modifiers {
    let (now, press) = ctx.input(|i| {
        let press = i.events.iter().rev().find_map(|e| match e {
            egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: true, modifiers, .. } => Some(*modifiers),
            _ => None,
        });
        (i.modifiers, press)
    });
    let press = match press {
        Some(m) => {
            ctx.data_mut(|d| d.insert_temp(key(), m));
            m
        }
        None => ctx.data(|d| d.get_temp::<egui::Modifiers>(key())).unwrap_or_default(),
    };
    union(now, press)
}

/// Modifier keys held now as the system reports them (only the read, nothing is changed).
fn system_modifiers() -> egui::Modifiers {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::{msg_send, runtime::AnyClass};
        let Some(class) = AnyClass::get(c"NSEvent") else { return egui::Modifiers::NONE };
        let flags: usize = msg_send![class, modifierFlags];
        let (shift, ctrl, alt, cmd) = (flags & (1 << 17) != 0, flags & (1 << 18) != 0, flags & (1 << 19) != 0, flags & (1 << 20) != 0);
        return egui::Modifiers { alt, ctrl, shift, mac_cmd: cmd, command: cmd };
    }
    #[cfg(not(target_os = "macos"))]
    egui::Modifiers::NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Press with ⌘, then the release of the button and of ⌘ in one frame: the click still counts as ⌘-click.
    #[test]
    fn modifiers_of_the_press_outlast_a_late_frame() {
        let ctx = egui::Context::default();
        let pos = egui::pos2(10.0, 10.0);
        let cmd = egui::Modifiers::COMMAND;
        let mut seen = Vec::new();
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput { events, ..Default::default() };
            let _ = crate::ui_test::run(&ctx, input, |ui| seen.push(at_press(ui.ctx())));
        };
        frame(vec![egui::Event::ModifiersChanged(cmd), egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: cmd }]);
        frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: cmd }, egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
        // a later plain press: no ⌘ any more
        frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        assert!(seen[0].command && seen[1].command, "press and late release count with ⌘: {seen:?}");
        assert!(!seen[2].command, "the next plain press is plain");
    }
}
