//! Real macOS menu (File, Edit, Photo, View, Window, Help), with
//! SF Symbols in front of the items. The shortcuts come from the central table ([`crate::shortcuts`]).
//!
//! Only commands with ⌘ get a real menu shortcut (macOS intercepts them, egui then does not see
//! them – no double triggering). Single keys like R or P only appear in the title so that the
//! menu does not swallow them when typing in text fields.

#[path = "i18n/mod.rs"]
pub(crate) mod i18n;

use i18n::Text;
#[cfg(any(target_os = "macos", windows))]
use i18n::Language;
#[cfg(any(target_os = "macos", windows))]
use std::cell::{Cell, RefCell};
#[cfg(any(target_os = "macos", windows))]
use std::collections::HashMap;
#[cfg(any(target_os = "macos", windows))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(target_os = "macos", windows))]
use std::sync::mpsc::{channel, Receiver, Sender};
#[cfg(any(target_os = "macos", windows))]
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;
#[cfg(any(target_os = "macos", windows))]
use muda::accelerator::{Accelerator, Code, Modifiers};
#[cfg(any(target_os = "macos", windows))]
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

use crate::shortcuts::{self, Trigger};

/// Menu commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    Settings,
    /// ⌘Q: through the window's close request, so pending library writes are waited for visibly (quit.rs)
    Quit,
    OpenFolder,
    Import,
    Lightroom,
    Export,
    /// ⌘P: the print room with the selection (`print_room.rs`)
    Print,
    ContactPrint,
    LensRoom,
    /// window "Kamera-Looks" (`camera_looks_ui.rs`)
    CameraLooks,
    VirtualCopy,
    /// "HDR zusammenfügen …" with the selection or the stack (hdr_ui.rs)
    MergeHdr,
    /// "Metadaten neu einlesen …": shooting data of the selection again from the files (metadata_reread_ui.rs)
    RereadMetadata,
    /// series and stacks (grid): review (N), group the selection (⌘G), dissolve (⇧⌘G), expand/collapse all (⇧S)
    StackReview,
    StackGroup,
    StackDissolve,
    StackToggleAll,
    DeleteRejected,
    /// ⌘⌫: the selected photos to the trash (with confirmation), also in a collection
    TrashSelected,
    /// take the selected photos out of the collection shown (⌫ there; no menu shortcut, ⌫ must reach text fields)
    RemoveFromCollection,
    Undo,
    Redo,
    /// ⌘C: the selected photos (developed JPEG or the original file) to the clipboard (`photo_copy.rs`)
    CopyPhoto,
    CopySettings,
    PasteSettings,
    /// ⌥⇧⌘V: settings of the main photo onto the other selected photos (`settings_group.rs`)
    TransferSettings,
    ResetSettings,
    AutoTone,
    ApplyToAll,
    Crop,
    RotateLeft,
    RotateRight,
    Pick,
    Reject,
    Unflag,
    Grid,
    JumpToSelection,
    Loupe,
    Develop,
    Sensor,
    Compare,
    Before,
    CameraJpeg,
    Clipping,
    Info,
    Panels,
    Lights,
    FullPreview,
    Loupe100,
    Shortcuts,
    ShowLogs,
    ReportBug,
    PerfReport,
    /// Help → Support graineo …: opens the support page in the browser (`support.rs`; hidden while no address is set)
    Support,
    About,
    CheckUpdates,
    TabInfo,
    TabEdit,
    ShowInFinder,
    ShowInLibrary,
    SelectNone,
    SelectAll,
    NewCollection,
    NewSmartCollection,
    NewCollectionSet,
    ManageImports,
    /// dialog "Stichwörter ordnen …" (library)
    ArrangeKeywords,
    ProofFull,
    ProofSrgb,
    ProofP3,
    ProofAdobe,
    ProofProPhoto,
    /// ⌘Y: full gamut ↔ last chosen proof
    ProofToggle,
    /// ⇧⌘Y: gamut warning
    GamutWarning,
    /// View → "Navigator beim Zoomen" (check mark, `navigator.rs`)
    Navigator,
    /// UI size one step up / down / back to 100 % (prefs.rs; only in the in-window menu on Linux and as ⌘+ / ⌘− / ⌘0)
    UiLarger,
    UiSmaller,
    UiReset,
}

impl Cmd {
    /// The proof a menu entry chooses (`None`: not a proof entry).
    pub fn proof(self) -> Option<crate::proof::Proof> {
        use crate::proof::Proof;
        Some(match self {
            Cmd::ProofFull => Proof::Full,
            Cmd::ProofSrgb => Proof::Srgb,
            Cmd::ProofP3 => Proof::DisplayP3,
            Cmd::ProofAdobe => Proof::AdobeRgb,
            Cmd::ProofProPhoto => Proof::ProPhoto,
            _ => return None,
        })
    }
}

/// Linux: no native menu bar (muda would pull in the GTK3 bindings). Instead the same menu as on Windows
/// is drawn by egui in a fixed bar at the top of the window ([`AppMenu::bar`]); the shortcuts run through
/// `shortcut_without_menu` as on Windows, the bar only shows them (Ctrl as on Windows).
#[cfg(not(any(target_os = "macos", windows)))]
pub struct AppMenu {
    clicked: std::cell::RefCell<Vec<Cmd>>,
}

/// One entry of the in-window menu (Linux).
#[cfg(not(any(target_os = "macos", windows)))]
enum Entry {
    Action(Cmd),
    /// with a check mark (colour proof, gamut warning, navigator)
    Check(Cmd),
    Separator,
    Sub(Text, &'static [Entry]),
    Minimize,
    Maximize,
}

/// The menu tree of the in-window menu: the same entries and order as the Windows menu bar (`AppMenu::install`).
#[cfg(not(any(target_os = "macos", windows)))]
const LINUX_MENU: &[(Text, &[Entry])] = {
    use Cmd::*;
    use Entry::{Action as A, Check as C, Separator as S};
    &[
        (Text::NativeFile, &[A(Import), A(Lightroom), A(OpenFolder), S, A(Export), A(Print), A(ContactPrint), S, A(ShowInFinder), A(ShowInLibrary), S, A(RemoveFromCollection), A(TrashSelected), A(DeleteRejected), S, A(Quit)]),
        (Text::NativeEdit, &[A(Undo), A(Redo), S, A(SelectAll), A(SelectNone), S, A(CopyPhoto), S, A(CopySettings), A(PasteSettings), A(TransferSettings), A(ApplyToAll), A(ResetSettings), S, A(Settings)]),
        (Text::NativeLibrary, &[A(NewCollection), A(NewSmartCollection), A(NewCollectionSet), S, A(ManageImports), A(ArrangeKeywords)]),
        (Text::NativePhoto, &[A(AutoTone), S, A(Crop), A(RotateLeft), A(RotateRight), S, A(VirtualCopy), A(MergeHdr), A(RereadMetadata), S, A(StackReview), A(StackGroup), A(StackDissolve), A(StackToggleAll), S, A(LensRoom), A(CameraLooks), S, A(Pick), A(Reject), A(Unflag)]),
        (
            Text::NativeView,
            &[
                A(Grid), A(Develop), A(Sensor), S, A(TabInfo), A(TabEdit), S, A(Compare), A(Before), A(CameraJpeg), S, A(Clipping), A(Info), A(Loupe100), C(Navigator), A(JumpToSelection), S,
                Entry::Sub(Text::MenuProof, &[C(ProofFull), C(ProofSrgb), C(ProofP3), C(ProofAdobe), C(ProofProPhoto), S, A(ProofToggle), C(GamutWarning)]),
                S, A(Panels), A(Lights), A(FullPreview), S, A(UiLarger), A(UiSmaller), A(UiReset),
            ],
        ),
        (Text::NativeWindow, &[Entry::Minimize, Entry::Maximize]),
        (Text::NativeHelp, &[A(Shortcuts), S, A(ReportBug), A(PerfReport), A(ShowLogs), S, A(Support), S, A(CheckUpdates), A(About)]),
    ]
};

/// Height of the in-window menu bar (Linux), fixed so nothing below it moves.
#[cfg(not(any(target_os = "macos", windows)))]
pub const BAR_H: f32 = 24.0;

#[cfg(not(any(target_os = "macos", windows)))]
impl AppMenu {
    pub fn install(_ctx: &egui::Context, _hwnd: Option<isize>) -> Option<Self> {
        i18n::set_logger(crate::log);
        Some(Self { clicked: std::cell::RefCell::new(Vec::new()) })
    }
    pub fn poll(&self) -> Vec<Cmd> {
        std::mem::take(&mut *self.clicked.borrow_mut())
    }

    /// Label and shortcut text of a menu command (Linux).
    fn label(cmd: Cmd) -> (String, String) {
        let shortcut = |id: &str| shortcuts::find(id).map(|k| shortcuts::display_of(&k.trigger)).unwrap_or_default();
        match cmd {
            Cmd::UiLarger => (crate::tr("Oberfläche vergrößern").to_string(), shortcut("ui_larger")),
            Cmd::UiSmaller => (crate::tr("Oberfläche verkleinern").to_string(), shortcut("ui_smaller")),
            Cmd::UiReset => (crate::tr("Oberfläche auf 100 %").to_string(), shortcut("ui_reset")),
            _ => match ACTIONS.iter().find(|a| a.0 == cmd) {
                Some((_, key, id, _)) => (i18n::text(*key).to_string(), id.map(shortcut).unwrap_or_default()),
                None => (format!("{cmd:?}"), String::new()),
            },
        }
    }

    fn checked(cmd: Cmd) -> bool {
        match cmd {
            Cmd::GamutWarning => crate::proof::warning(),
            Cmd::Navigator => crate::navigator::enabled(),
            c => c.proof() == Some(crate::proof::current()),
        }
    }

    fn entries(&self, ui: &mut egui::Ui, entries: &[Entry]) {
        for e in entries {
            match e {
                Entry::Separator => {
                    ui.separator();
                }
                Entry::Action(cmd) | Entry::Check(cmd) => {
                    if *cmd == Cmd::Support && !crate::support::enabled() {
                        continue;
                    }
                    let (title, keys) = Self::label(*cmd);
                    let button = if matches!(e, Entry::Check(_)) { egui::Button::selectable(Self::checked(*cmd), title) } else { egui::Button::new(title) };
                    if ui.add(button.shortcut_text(keys)).clicked() {
                        self.clicked.borrow_mut().push(*cmd);
                        ui.ctx().request_repaint();
                    }
                }
                Entry::Sub(key, sub) => {
                    ui.menu_button(i18n::text(*key), |ui| self.entries(ui, sub));
                }
                Entry::Minimize => {
                    if ui.button(i18n::text(Text::NativeMinimize)).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                }
                Entry::Maximize => {
                    if ui.button(i18n::text(Text::NativeZoom)).clicked() {
                        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
                    }
                }
            }
        }
    }

    /// The menu bar in the window: a fixed strip at the very top (same height always, no layout jumps).
    pub fn bar(&self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(crate::lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(6, 0));
        egui::Panel::top("menu-bar").show_separator_line(false).resizable(false).exact_size(BAR_H).frame(frame).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for (key, entries) in LINUX_MENU {
                    ui.menu_button(i18n::text(*key), |ui| {
                        ui.set_min_width(260.0);
                        self.entries(ui, entries);
                    });
                }
            });
        });
    }
}

#[cfg(any(target_os = "macos", windows))]
pub struct AppMenu {
    _menu: Menu,
    rx: Receiver<Cmd>,
    labels: Vec<Label>,
    /// entries with a check mark (colour proof, gamut warning, navigator) and the state last shown in them
    checks: Vec<(muda::CheckMenuItem, Cmd)>,
    checks_shown: Cell<Option<(u8, bool, bool)>>,
    language: Cell<Language>,
    route: u64,
    ctx: egui::Context,
}

#[cfg(any(target_os = "macos", windows))]
/// egui key → muda code (only what appears in the menu).
fn code(k: egui::Key) -> Option<Code> {
    use egui::Key as K;
    Some(match k {
        K::A => Code::KeyA,
        K::C => Code::KeyC,
        K::D => Code::KeyD,
        K::E => Code::KeyE,
        K::F => Code::KeyF,
        K::G => Code::KeyG,
        K::I => Code::KeyI,
        K::N => Code::KeyN,
        K::O => Code::KeyO,
        K::P => Code::KeyP,
        K::Q => Code::KeyQ,
        K::R => Code::KeyR,
        K::U => Code::KeyU,
        K::V => Code::KeyV,
        K::Y => Code::KeyY,
        K::Z => Code::KeyZ,
        K::Comma => Code::Comma,
        K::Slash => Code::Slash,
        K::OpenBracket => Code::BracketLeft,
        K::CloseBracket => Code::BracketRight,
        K::Backspace => Code::Backspace,
        K::Quote => Code::Quote,
        _ => return None,
    })
}

#[cfg(any(target_os = "macos", windows))]
/// Title and accelerator from the table: with ⌘ a real shortcut, otherwise the key in the title.
fn from_table(title: &str, id: Option<&str>) -> (String, Option<Accelerator>) {
    // muda reads "&" as a mnemonic marker and drops it ("Freistellen & Ausrichten"); "&&" shows one "&"
    let escaped = title.replace('&', "&&");
    let title = escaped.as_str();
    let Some(k) = id.and_then(shortcuts::find) else {
        return (title.to_string(), None);
    };
    match k.trigger {
        Trigger::KeyPress(m, key) if m.cmd => {
            let mut mods = Modifiers::SUPER;
            if m.shift {
                mods |= Modifiers::SHIFT;
            }
            if m.alt {
                mods |= Modifiers::ALT;
            }
            match code(key) {
                Some(c) => (title.to_string(), Some(Accelerator::new(Some(mods), c))),
                None => (
                    format!("{title}   {}", shortcuts::display_of(&k.trigger)),
                    None,
                ),
            }
        }
        // macOS: keys without ⌘ stand in the shortcut column like real shortcuts (see `single_key_equivalents`)
        Trigger::KeyPress(_, key) if cfg!(target_os = "macos") && key_equivalent(key).is_some() => (title.to_string(), None),
        _ => (
            format!("{title}   {}", shortcuts::display_of(&k.trigger)),
            None,
        ),
    }
}

#[cfg(any(target_os = "macos", windows))]
/// Character of a key as macOS key equivalent (keys without ⌘ that only show in the menu).
fn key_equivalent(k: egui::Key) -> Option<String> {
    use egui::Key as K;
    Some(match k {
        K::Enter => "\r".into(),
        K::Tab => "\t".into(),
        K::Space => " ".into(),
        K::Escape => "\u{1b}".into(),
        K::Backslash => "\\".into(),
        K::Backspace => "\u{8}".into(),
        _ => {
            let n = format!("{k:?}");
            if n.len() == 1 && n.chars().all(|c| c.is_ascii_alphabetic()) {
                n.to_lowercase()
            } else if let Some(d) = n.strip_prefix("Num").filter(|d| d.len() == 1) {
                d.to_string()
            } else {
                return None;
            }
        }
    })
}

#[cfg(any(target_os = "macos", windows))]
/// Single-key shortcut of a menu entry (no ⌘): key equivalent and modifier mask (⇧ = 1 << 17, ⌥ = 1 << 19).
fn single_key(id: Option<&str>) -> Option<(String, u64)> {
    let k = shortcuts::find(id?)?;
    match k.trigger {
        Trigger::KeyPress(m, key) if !m.cmd => Some((key_equivalent(key)?, if m.shift { 1 << 17 } else { 0 } | if m.alt { 1 << 19 } else { 0 })),
        _ => None,
    }
}

/// Menu shortcuts that egui received (Windows: menu bar without its own key handling; Mac: keys AppKit did not
/// match): query the ⌘ shortcuts of the menu commands directly (⌘ = Ctrl on Windows). Shortcuts with more
/// modifiers first so that ⇧⌘Z does not count as ⌘Z. During text input the keys belong to the text field.
pub fn shortcut_without_menu(ctx: &egui::Context) -> Vec<Cmd> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    let mut list: Vec<(Cmd, egui::KeyboardShortcut, u8)> = ACTIONS
        .iter()
        .filter_map(|(cmd, _, id, _)| match &shortcuts::find((*id)?)?.trigger {
            Trigger::KeyPress(m, key) if m.cmd => {
                let key = *key;
                let mut mods = egui::Modifiers::COMMAND;
                if m.shift {
                    mods = mods | egui::Modifiers::SHIFT;
                }
                if m.alt {
                    mods = mods | egui::Modifiers::ALT;
                }
                Some((*cmd, egui::KeyboardShortcut::new(mods, key), m.shift as u8 + m.alt as u8))
            }
            _ => None,
        })
        .collect();
    list.sort_by_key(|e| std::cmp::Reverse(e.2));
    let mut cmds: Vec<Cmd> = list.into_iter().filter(|(_, k, _)| ctx.input_mut(|i| consume_exact(i, k))).map(|(c, _, _)| c).collect();
    // ⌘C (Ctrl+C) arrives as egui's copy event, not as a key: "Copy photo", unless a label text is selected
    if !crate::photo_copy::text_selected(ctx) && ctx.input_mut(take_copy_event) {
        cmds.push(Cmd::CopyPhoto);
    }
    cmds
}

/// Remove egui's copy event of this frame (⌘C / Ctrl+C); true if there was one.
fn take_copy_event(i: &mut egui::InputState) -> bool {
    let n = i.events.len();
    i.events.retain(|e| !matches!(e, egui::Event::Copy));
    i.events.len() != n
}

/// Take a shortcut only when the modifiers match exactly: egui's own matching ignores an extra ⌥ or ⇧, so ⌥⌘R (no longer
/// bound) would otherwise run ⌘R (turn left).
fn consume_exact(i: &mut egui::InputState, k: &egui::KeyboardShortcut) -> bool {
    let exact = i.events.iter().any(|e| match e {
        egui::Event::Key { key, pressed: true, modifiers, .. } => {
            *key == k.logical_key && modifiers.command == k.modifiers.command && modifiers.shift == k.modifiers.shift && modifiers.alt == k.modifiers.alt
        }
        _ => false,
    });
    exact && i.consume_shortcut(k)
}

/// Stable action specification: translated text never identifies an action or a symbol.
const ACTIONS: &[(Cmd, Text, Option<&str>, Option<&str>)] = &[
    (Cmd::About, Text::MenuAbout, None, Some("info.circle")),
    (Cmd::Quit, Text::NativeQuit, Some("quit"), Some("power")),
    (Cmd::CheckUpdates, Text::MenuCheckUpdates, None, Some("arrow.down.circle")),
    (
        Cmd::Settings,
        Text::MenuSettings,
        Some("settings"),
        Some("gearshape"),
    ),
    (
        Cmd::OpenFolder,
        Text::MenuOpenFolder,
        Some("open_folder"),
        Some("folder"),
    ),
    (
        Cmd::Import,
        Text::MenuImport,
        Some("import"),
        Some("square.and.arrow.down"),
    ),
    (Cmd::Lightroom, Text::MenuLightroom, None, Some("arrow.down.doc")),
    (
        Cmd::Export,
        Text::MenuExport,
        Some("export"),
        Some("square.and.arrow.up"),
    ),
    (Cmd::Print, Text::MenuPrint, Some("print"), Some("printer")),
    (Cmd::ContactPrint, Text::MenuContactPrint, None, Some("film")),
    (Cmd::LensRoom, Text::MenuLensRoom, None, Some("camera.aperture")),
    (Cmd::CameraLooks, Text::MenuCameraLooks, None, Some("camera.filters")),
    (
        Cmd::ShowInFinder,
        Text::MenuShowInFinder,
        Some("show_in_finder"),
        Some("magnifyingglass"),
    ),
    (
        Cmd::ShowInLibrary,
        Text::MenuShowInLibrary,
        Some("show_in_library_folder"),
        Some("folder.badge.gearshape"),
    ),
    (
        Cmd::RemoveFromCollection,
        Text::MenuRemoveFromCollection,
        None,
        Some("rectangle.stack.badge.minus"),
    ),
    (
        Cmd::TrashSelected,
        Text::MenuTrashSelected,
        Some("trash_selected"),
        Some("trash"),
    ),
    (
        Cmd::DeleteRejected,
        Text::MenuDeleteRejected,
        Some("trash_rejected"),
        Some("trash"),
    ),
    (
        Cmd::Undo,
        Text::MenuUndo,
        Some("undo"),
        Some("arrow.uturn.backward"),
    ),
    (
        Cmd::Redo,
        Text::MenuRedo,
        Some("redo"),
        Some("arrow.uturn.forward"),
    ),
    (
        Cmd::SelectAll,
        Text::NativeSelectAll,
        Some("select_all"),
        Some("checkmark.circle"),
    ),
    (
        Cmd::SelectNone,
        Text::MenuSelectNone,
        Some("select_none"),
        Some("circle.dashed"),
    ),
    (
        Cmd::CopyPhoto,
        Text::MenuCopyPhoto,
        Some("copy_photo"),
        Some("doc.on.doc"),
    ),
    (
        Cmd::CopySettings,
        Text::MenuCopySettings,
        Some("copy_settings"),
        Some("slider.horizontal.3"),
    ),
    (
        Cmd::PasteSettings,
        Text::MenuPasteSettings,
        Some("paste_settings"),
        Some("slider.horizontal.below.rectangle"),
    ),
    (
        Cmd::TransferSettings,
        Text::MenuTransferSettings,
        Some("sync_settings"),
        Some("square.on.square"),
    ),
    (
        Cmd::ApplyToAll,
        Text::MenuApplyToAll,
        None,
        Some("square.stack.3d.up"),
    ),
    (
        Cmd::ResetSettings,
        Text::MenuResetSettings,
        Some("reset_all"),
        Some("arrow.counterclockwise"),
    ),
    (
        Cmd::AutoTone,
        Text::MenuAutoTone,
        Some("auto_tone"),
        Some("wand.and.stars"),
    ),
    (Cmd::Crop, Text::MenuCrop, Some("crop"), Some("crop")),
    (
        Cmd::RotateLeft,
        Text::MenuRotateLeft,
        Some("rotate_left"),
        Some("rotate.left"),
    ),
    (
        Cmd::RotateRight,
        Text::MenuRotateRight,
        Some("rotate_right"),
        Some("rotate.right"),
    ),
    (
        Cmd::VirtualCopy,
        Text::MenuVirtualCopy,
        Some("virtual_copy"),
        Some("plus.square.on.square"),
    ),
    (Cmd::MergeHdr, Text::MenuMergeHdr, Some("merge_hdr"), Some("camera.aperture")),
    (Cmd::RereadMetadata, Text::MenuRereadMetadata, None, Some("arrow.clockwise.circle")),
    (Cmd::StackReview, Text::MenuStackReview, Some("stack_review"), Some("square.stack")),
    (Cmd::StackGroup, Text::MenuStackGroup, Some("stack_group"), Some("square.stack.3d.down.right")),
    (Cmd::StackDissolve, Text::MenuStackDissolve, Some("stack_dissolve"), Some("square.stack.3d.up.slash")),
    (Cmd::StackToggleAll, Text::MenuStackToggleAll, Some("stack_toggle_all"), Some("rectangle.expand.vertical")),
    (Cmd::Pick, Text::MenuPick, Some("pick"), Some("checkmark.circle.fill")),
    (
        Cmd::Reject,
        Text::MenuReject,
        Some("reject"),
        Some("xmark.circle.fill"),
    ),
    (
        Cmd::Unflag,
        Text::MenuUnflag,
        Some("unflag"),
        Some("circle.dashed"),
    ),
    (
        Cmd::Grid,
        Text::MenuGrid,
        Some("grid"),
        Some("square.grid.3x3"),
    ),
    (
        Cmd::Develop,
        Text::MenuDevelop,
        Some("single_photo"),
        Some("photo"),
    ),
    (Cmd::Sensor, Text::MenuSensor, None, Some("camera.aperture")),
    (
        Cmd::TabInfo,
        Text::MenuTabInfo,
        Some("tab_info"),
        Some("info.square"),
    ),
    (
        Cmd::TabEdit,
        Text::MenuTabEdit,
        Some("tab_edit"),
        Some("slider.horizontal.3"),
    ),
    (
        Cmd::Compare,
        Text::MenuCompare,
        Some("compare"),
        Some("rectangle.split.2x1"),
    ),
    (
        Cmd::Before,
        Text::MenuBefore,
        Some("before"),
        Some("clock.arrow.circlepath"),
    ),
    (
        Cmd::CameraJpeg,
        Text::MenuCameraJpeg,
        Some("camera_jpeg"),
        Some("camera"),
    ),
    (
        Cmd::Clipping,
        Text::MenuClipping,
        Some("clipping"),
        Some("exclamationmark.triangle"),
    ),
    (
        Cmd::Info,
        Text::MenuInfo,
        Some("info_overlay"),
        Some("text.below.photo"),
    ),
    (
        Cmd::Loupe100,
        Text::MenuLoupe100,
        Some("loupe"),
        Some("magnifyingglass.circle"),
    ),
    (
        Cmd::Panels,
        Text::MenuPanels,
        Some("side_panels"),
        Some("sidebar.left"),
    ),
    (
        Cmd::Lights,
        Text::MenuLights,
        Some("lights_out"),
        Some("lightbulb"),
    ),
    (
        Cmd::FullPreview,
        Text::MenuFullPreview,
        Some("full_screen"),
        Some("arrow.up.left.and.arrow.down.right"),
    ),
    (
        Cmd::Shortcuts,
        Text::MenuShortcuts,
        Some("overview"),
        Some("keyboard"),
    ),
    (Cmd::ShowLogs, Text::MenuShowLogs, None, Some("doc.text.magnifyingglass")),
    (Cmd::ReportBug, Text::MenuReportBug, None, Some("exclamationmark.bubble")),
    (Cmd::PerfReport, Text::MenuPerfReport, None, Some("speedometer")),
    (Cmd::Support, Text::MenuSupport, None, Some("heart")),
    (Cmd::NewCollection, Text::MenuNewCollection, Some("new_collection"), Some("rectangle.stack.badge.plus")),
    (Cmd::NewSmartCollection, Text::MenuNewSmartCollection, None, Some("gearshape.2")),
    (Cmd::NewCollectionSet, Text::MenuNewCollectionSet, None, Some("folder.badge.plus")),
    (Cmd::JumpToSelection, Text::MenuJumpToSelection, Some("jump_to_selection"), Some("scope")),
    (Cmd::ManageImports, Text::MenuManageImports, None, Some("tray.full")),
    (Cmd::ArrangeKeywords, Text::MenuArrangeKeywords, Some("arrange_keywords"), Some("tag")),
    (Cmd::ProofFull, Text::MenuProofFull, None, None),
    (Cmd::ProofSrgb, Text::MenuProofSrgb, None, None),
    (Cmd::ProofP3, Text::MenuProofP3, None, None),
    (Cmd::ProofAdobe, Text::MenuProofAdobe, None, None),
    (Cmd::ProofProPhoto, Text::MenuProofProPhoto, None, None),
    (Cmd::ProofToggle, Text::MenuProofToggle, Some("soft_proof"), Some("circle.lefthalf.filled")),
    (Cmd::GamutWarning, Text::MenuGamutWarning, Some("gamut_warning"), Some("exclamationmark.triangle.fill")),
    (Cmd::Navigator, Text::MenuNavigator, None, None),
];

#[cfg(any(target_os = "macos", windows))]
enum Label {
    Action(MenuItem, Text, Option<&'static str>),
    Check(muda::CheckMenuItem, Text, Option<&'static str>),
    Native(PredefinedMenuItem, Text),
    Group(Submenu, Text),
}
#[cfg(any(target_os = "macos", windows))]
impl Label {
    fn apply(&self, language: Language) {
        match self {
            Self::Action(item, key, shortcut) => {
                item.set_text(from_table(i18n::text_in(language, *key), *shortcut).0)
            }
            Self::Check(item, key, shortcut) => {
                item.set_text(from_table(i18n::text_in(language, *key), *shortcut).0)
            }
            Self::Native(item, key) => item.set_text(i18n::text_in(language, *key)),
            Self::Group(item, key) => item.set_text(i18n::text_in(language, *key)),
        }
    }
}
#[cfg(any(target_os = "macos", windows))]
struct Route {
    generation: u64,
    ids: HashMap<muda::MenuId, Cmd>,
    tx: Sender<Cmd>,
    ctx: egui::Context,
}
#[cfg(any(target_os = "macos", windows))]
type Router = Arc<Mutex<Option<Route>>>;
#[cfg(any(target_os = "macos", windows))]
static ROUTER: OnceLock<Router> = OnceLock::new();
#[cfg(any(target_os = "macos", windows))]
fn deliver(router: &Router, id: &muda::MenuId) {
    let target = router
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|r| r.ids.get(id).map(|cmd| (*cmd, r.tx.clone(), r.ctx.clone())));
    if let Some((cmd, tx, ctx)) = target {
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var_os("GRAINEO_NATIVE_KEYS").is_some() {
            crate::log(&format!("menu delivers: {cmd:?}"));
        }
        let _ = tx.send(cmd);
        ctx.request_repaint();
    }
}
#[cfg(any(target_os = "macos", windows))]
fn connect(ctx: &egui::Context, tx: Sender<Cmd>, ids: HashMap<muda::MenuId, Cmd>) -> u64 {
    let router = ROUTER.get_or_init(|| {
        let router: Router = Arc::new(Mutex::new(None));
        let receiver = router.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| deliver(&receiver, &event.id)));
        router
    });
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let generation = NEXT.fetch_add(1, Ordering::Relaxed) + 1;
    *router.lock().unwrap_or_else(|e| e.into_inner()) = Some(Route {
        generation,
        ids,
        tx,
        ctx: ctx.clone(),
    });
    generation
}
#[cfg(any(target_os = "macos", windows))]
impl Drop for AppMenu {
    fn drop(&mut self) {
        if let Some(router) = ROUTER.get() {
            let mut route = router.lock().unwrap_or_else(|e| e.into_inner());
            if route.as_ref().is_some_and(|r| r.generation == self.route) {
                *route = None;
            }
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
impl AppMenu {
    /// Builds the menu and attaches it to the macOS app (main thread, after the window is set up).
    /// `hwnd`: Windows window handle (HWND) – there the menu bar hangs on the window, not on the app.
    pub fn install(ctx: &egui::Context, hwnd: Option<isize>) -> Option<Self> {
        i18n::set_logger(crate::log);
        if !cfg!(target_os = "macos") && hwnd.is_none() {
            crate::log("menu bar: no window handle, menu missing");
            return None;
        }
        let language = i18n::language();
        let labels = RefCell::new(Vec::new());
        let mut icons = HashMap::new();
        let mut single_keys: HashMap<muda::MenuId, (String, u64)> = HashMap::new();
        let (tx, rx) = channel();
        let mut items: Vec<(muda::MenuId, Cmd)> = Vec::new();
        let checks: RefCell<Vec<(muda::CheckMenuItem, Cmd)>> = RefCell::new(Vec::new());
        let check_ids: RefCell<Vec<(muda::MenuId, Cmd)>> = RefCell::new(Vec::new());
        // entries with a check mark: colour proof (one of five) and gamut warning
        let check = |cmd: Cmd| -> muda::CheckMenuItem {
            let (_, key, shortcut, _) = ACTIONS.iter().find(|a| a.0 == cmd).expect("static menu action");
            let (title, accelerator) = from_table(i18n::text_in(language, *key), *shortcut);
            let item = muda::CheckMenuItem::with_id(format!("graineo:{}", key.id()), title, true, false, accelerator);
            labels.borrow_mut().push(Label::Check(item.clone(), *key, *shortcut));
            checks.borrow_mut().push((item.clone(), cmd));
            check_ids.borrow_mut().push((item.id().clone(), cmd));
            item
        };
        let mut item = |cmd: Cmd| -> MenuItem {
            let (_, key, shortcut, symbol) = ACTIONS
                .iter()
                .find(|a| a.0 == cmd)
                .expect("static menu action");
            let (title, accelerator) = from_table(i18n::text_in(language, *key), *shortcut);
            let item =
                MenuItem::with_id(format!("graineo:{}", key.id()), title, true, accelerator);
            if let Some(symbol) = symbol {
                icons.insert(item.id().clone(), *symbol);
            }
            if let Some(k) = single_key(*shortcut) {
                single_keys.insert(item.id().clone(), k);
            }
            labels
                .borrow_mut()
                .push(Label::Action(item.clone(), *key, *shortcut));
            items.push((item.id().clone(), cmd));
            item
        };
        let native = |item: PredefinedMenuItem, key: Text| -> PredefinedMenuItem {
            labels.borrow_mut().push(Label::Native(item.clone(), key));
            item
        };

        // macOS: the app menu (About, Updates, Settings, Services, Hide, Quit). Windows has none: About and Updates go to
        // Help, Settings to Edit, Exit to the end of File – and no Mac-only entries (Services, Hide …).
        let mac = cfg!(target_os = "macos");
        let app = if mac {
            let app = Submenu::with_items(
                i18n::text_in(language, Text::NativeApp),
                true,
                &[
                    &item(Cmd::About),
                    &item(Cmd::CheckUpdates),
                    &PredefinedMenuItem::separator(),
                    &item(Cmd::Settings),
                    &PredefinedMenuItem::separator(),
                    &native(
                        PredefinedMenuItem::services(Some(i18n::text_in(
                            language,
                            Text::NativeServices,
                        ))),
                        Text::NativeServices,
                    ),
                    &PredefinedMenuItem::separator(),
                    &native(
                        PredefinedMenuItem::hide(Some(i18n::text_in(language, Text::NativeHide))),
                        Text::NativeHide,
                    ),
                    &native(
                        PredefinedMenuItem::hide_others(Some(i18n::text_in(
                            language,
                            Text::NativeHideOthers,
                        ))),
                        Text::NativeHideOthers,
                    ),
                    &native(
                        PredefinedMenuItem::show_all(Some(i18n::text_in(
                            language,
                            Text::NativeShowAll,
                        ))),
                        Text::NativeShowAll,
                    ),
                    &PredefinedMenuItem::separator(),
                    &item(Cmd::Quit),
                ],
            )
            .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
            labels
                .borrow_mut()
                .push(Label::Group(app.clone(), Text::NativeApp));
            Some(app)
        } else {
            None
        };
        // the way in first: import photos, import from Lightroom, then only view a folder
        let mut file_items: Vec<Box<dyn muda::IsMenuItem>> = vec![
            Box::new(item(Cmd::Import)),
            Box::new(item(Cmd::Lightroom)),
            Box::new(item(Cmd::OpenFolder)),
            Box::new(PredefinedMenuItem::separator()),
            Box::new(item(Cmd::Export)),
            Box::new(item(Cmd::Print)),
            Box::new(item(Cmd::ContactPrint)),
            Box::new(PredefinedMenuItem::separator()),
            Box::new(item(Cmd::ShowInFinder)),
            Box::new(item(Cmd::ShowInLibrary)),
            Box::new(PredefinedMenuItem::separator()),
            Box::new(item(Cmd::RemoveFromCollection)),
            Box::new(item(Cmd::TrashSelected)),
            Box::new(item(Cmd::DeleteRejected)),
        ];
        if !mac {
            file_items.push(Box::new(PredefinedMenuItem::separator()));
            file_items.push(Box::new(item(Cmd::Quit)));
        }
        let file_refs: Vec<&dyn muda::IsMenuItem> = file_items.iter().map(|b| b.as_ref()).collect();
        let file = Submenu::with_items(i18n::text_in(language, Text::NativeFile), true, &file_refs)
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        labels
            .borrow_mut()
            .push(Label::Group(file.clone(), Text::NativeFile));
        let edit = Submenu::with_items(
            i18n::text_in(language, Text::NativeEdit),
            true,
            &[
                &item(Cmd::Undo),
                &item(Cmd::Redo),
                &PredefinedMenuItem::separator(),
                // No cut/copy/paste from the system: these items intercept ⌘X, ⌘C and ⌘V and send them to
                // a responder the window does not have; text fields (search, import) then received nothing. Without them the
                // keys go directly to the text fields.
                &item(Cmd::SelectAll),
                &item(Cmd::SelectNone),
                &PredefinedMenuItem::separator(),
                &item(Cmd::CopyPhoto),
                &PredefinedMenuItem::separator(),
                &item(Cmd::CopySettings),
                &item(Cmd::PasteSettings),
                &item(Cmd::TransferSettings),
                &item(Cmd::ApplyToAll),
                &item(Cmd::ResetSettings),
            ],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        if !mac {
            let _ = edit.append_items(&[&PredefinedMenuItem::separator(), &item(Cmd::Settings)]);
        }
        labels
            .borrow_mut()
            .push(Label::Group(edit.clone(), Text::NativeEdit));
        let library_menu = Submenu::with_items(
            i18n::text_in(language, Text::NativeLibrary),
            true,
            &[
                &item(Cmd::NewCollection),
                &item(Cmd::NewSmartCollection),
                &item(Cmd::NewCollectionSet),
                &PredefinedMenuItem::separator(),
                &item(Cmd::ManageImports),
                &item(Cmd::ArrangeKeywords),
            ],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        labels
            .borrow_mut()
            .push(Label::Group(library_menu.clone(), Text::NativeLibrary));
        let photo = Submenu::with_items(
            i18n::text_in(language, Text::NativePhoto),
            true,
            &[
                &item(Cmd::AutoTone),
                &PredefinedMenuItem::separator(),
                &item(Cmd::Crop),
                &item(Cmd::RotateLeft),
                &item(Cmd::RotateRight),
                &PredefinedMenuItem::separator(),
                &item(Cmd::VirtualCopy),
                &item(Cmd::MergeHdr),
                &item(Cmd::RereadMetadata),
                &PredefinedMenuItem::separator(),
                &item(Cmd::StackReview),
                &item(Cmd::StackGroup),
                &item(Cmd::StackDissolve),
                &item(Cmd::StackToggleAll),
                &PredefinedMenuItem::separator(),
                &item(Cmd::LensRoom),
                &item(Cmd::CameraLooks),
                &PredefinedMenuItem::separator(),
                &item(Cmd::Pick),
                &item(Cmd::Reject),
                &item(Cmd::Unflag),
            ],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        labels
            .borrow_mut()
            .push(Label::Group(photo.clone(), Text::NativePhoto));
        // View → Colour proof: the space as a choice with a check mark, ⌘Y and ⇧⌘Y below
        let proof_menu = Submenu::with_items(
            i18n::text_in(language, Text::MenuProof),
            true,
            &[
                &check(Cmd::ProofFull),
                &check(Cmd::ProofSrgb),
                &check(Cmd::ProofP3),
                &check(Cmd::ProofAdobe),
                &check(Cmd::ProofProPhoto),
                &PredefinedMenuItem::separator(),
                &item(Cmd::ProofToggle),
                &check(Cmd::GamutWarning),
            ],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        labels.borrow_mut().push(Label::Group(proof_menu.clone(), Text::MenuProof));
        let view = Submenu::with_items(
            i18n::text_in(language, Text::NativeView),
            true,
            &[
                &item(Cmd::Grid),
                &item(Cmd::Develop),
                &item(Cmd::Sensor),
                &PredefinedMenuItem::separator(),
                &item(Cmd::TabInfo),
                &item(Cmd::TabEdit),
                &PredefinedMenuItem::separator(),
                &item(Cmd::Compare),
                &item(Cmd::Before),
                &item(Cmd::CameraJpeg),
                &PredefinedMenuItem::separator(),
                &item(Cmd::Clipping),
                &item(Cmd::Info),
                &item(Cmd::Loupe100),
                &check(Cmd::Navigator),
                &item(Cmd::JumpToSelection),
                &PredefinedMenuItem::separator(),
                &proof_menu,
                &PredefinedMenuItem::separator(),
                &item(Cmd::Panels),
                &item(Cmd::Lights),
                &item(Cmd::FullPreview),
            ],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        labels
            .borrow_mut()
            .push(Label::Group(view.clone(), Text::NativeView));
        let window = Submenu::with_items(
            i18n::text_in(language, Text::NativeWindow),
            true,
            &[
                &native(
                    PredefinedMenuItem::minimize(Some(i18n::text_in(
                        language,
                        Text::NativeMinimize,
                    ))),
                    Text::NativeMinimize,
                ),
                &native(
                    PredefinedMenuItem::maximize(Some(i18n::text_in(language, Text::NativeZoom))),
                    Text::NativeZoom,
                ),
            ],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        // full screen and "bring all to front" exist only on the Mac
        if mac {
            let _ = window.append_items(&[
                &PredefinedMenuItem::separator(),
                &native(
                    PredefinedMenuItem::fullscreen(Some(i18n::text_in(
                        language,
                        Text::NativeFullscreen,
                    ))),
                    Text::NativeFullscreen,
                ),
                &PredefinedMenuItem::separator(),
                &native(
                    PredefinedMenuItem::bring_all_to_front(Some(i18n::text_in(language, Text::NativeBringAllToFront))),
                    Text::NativeBringAllToFront,
                ),
            ]);
        }
        labels
            .borrow_mut()
            .push(Label::Group(window.clone(), Text::NativeWindow));
        let help = Submenu::with_items(
            i18n::text_in(language, Text::NativeHelp),
            true,
            &[&item(Cmd::Shortcuts), &PredefinedMenuItem::separator(), &item(Cmd::ReportBug), &item(Cmd::PerfReport), &item(Cmd::ShowLogs)],
        )
        .map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        // quiet entry, only once a support address is set (support.rs)
        if crate::support::enabled() {
            let _ = help.append_items(&[&PredefinedMenuItem::separator(), &item(Cmd::Support)]);
        }
        if !mac {
            let _ = help.append_items(&[&PredefinedMenuItem::separator(), &item(Cmd::CheckUpdates), &item(Cmd::About)]);
        }
        labels
            .borrow_mut()
            .push(Label::Group(help.clone(), Text::NativeHelp));

        let menu = Menu::new();
        for s in app.iter().chain([&file, &edit, &library_menu, &photo, &view, &window, &help]) {
            menu.append(s).map_err(|e| crate::log(&format!("menu bar: {e}"))).ok()?;
        }
        #[cfg(target_os = "macos")]
        {
            let _ = hwnd;
            menu.init_for_nsapp();
            window.set_as_windows_menu_for_nsapp();
            help.set_as_help_menu_for_nsapp();
        }
        // Windows: menu bar on the window, dark like the app. The shortcuts run through egui there
        // (`shortcut_without_menu`), the bar only displays them.
        #[cfg(windows)]
        unsafe {
            match menu.init_for_hwnd_with_theme(hwnd?, muda::MenuTheme::Dark) {
                Ok(()) => crate::log("menu bar attached to the window"),
                Err(e) => {
                    crate::log(&format!("menu bar failed: {e}"));
                    return None;
                }
            }
        }
        // Symbols are resolved by item identity, independent of localized labels.
        for label in labels.borrow().iter() {
            if let Label::Native(item, key) = label {
                let sf = match key {
                    Text::NativeCut => Some("scissors"),
                    Text::NativeCopy => Some("doc.on.doc"),
                    Text::NativePaste => Some("doc.on.clipboard"),
                    Text::NativeSelectAll => Some("checkmark.circle"),
                    Text::NativeMinimize => Some("minus.square"),
                    Text::NativeZoom => Some("plus.square"),
                    Text::NativeFullscreen => Some("rectangle.inset.filled"),
                    Text::NativeBringAllToFront => Some("macwindow.on.rectangle"),
                    // app menu: every entry with a symbol, as the others
                    Text::NativeServices => Some("ellipsis.circle"),
                    Text::NativeHide => Some("eye.slash"),
                    Text::NativeHideOthers => Some("rectangle.on.rectangle.slash"),
                    Text::NativeShowAll => Some("eye"),
                    Text::NativeQuit => Some("power"),
                    _ => None,
                };
                if let Some(sf) = sf {
                    icons.insert(item.id().clone(), sf);
                }
            }
        }
        set_symbols(&menu, &icons, &single_keys);
        let ids = items.iter().cloned().chain(check_ids.into_inner()).collect();
        let route = connect(ctx, tx, ids);
        Some(Self {
            _menu: menu,
            rx,
            labels: labels.into_inner(),
            checks: checks.into_inner(),
            checks_shown: Cell::new(None),
            language: Cell::new(language),
            route,
            ctx: ctx.clone(),
        })
    }

    /// Only Linux draws the menu in the window; here the system shows it.
    pub fn bar(&self, _ui: &mut egui::Ui) {}

    pub fn poll(&self) -> Vec<Cmd> {
        let test_changed = i18n::locale::test_frame(&self.ctx);
        let language = i18n::language();
        // Own-window screenshot aid: About does not require loading a full photo in Grid.
        let about_id = egui::Id::unique("i18n-native-about-opened");
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some()
            && crate::env::var("GRAINEO_I18N_TEST_ABOUT").as_deref() == Ok("1")
            && !self
                .ctx
                .data(|d| d.get_temp::<bool>(about_id).unwrap_or(false))
        {
            self.ctx.data_mut(|d| d.insert_temp(about_id, true));
            if let Some(router) = ROUTER.get() {
                deliver(router, &muda::MenuId::new("graineo:menu-about"));
            }
        }
        let changed = self.language.replace(language) != language;
        if changed {
            // Retain native items, IDs, accelerators and the one event handler.
            for label in &self.labels {
                label.apply(language);
            }
        }
        if test_changed && crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
            let failures = self
                .labels
                .iter()
                .filter(|label| match label {
                    Label::Action(item, key, shortcut) => {
                        // muda removes mnemonic markers on macOS, as it did before localization.
                        item.text()
                            != from_table(i18n::text_in(language, *key), *shortcut)
                                .0
                                .replace("&&", "&")
                    }
                    Label::Check(item, key, shortcut) => {
                        item.text()
                            != from_table(i18n::text_in(language, *key), *shortcut)
                                .0
                                .replace("&&", "&")
                    }
                    Label::Native(item, key) => item.text() != i18n::text_in(language, *key),
                    Label::Group(item, key) => item.text() != i18n::text_in(language, *key),
                })
                .count();
            // Exercise the live route after each DE/EN/DE stage; no synthetic OS keystroke.
            if let Some(router) = ROUTER.get() {
                deliver(router, &muda::MenuId::new("graineo:menu-settings"));
            }
            let commands: Vec<_> = self.rx.try_iter().collect();
            let settings = commands.iter().filter(|cmd| **cmd == Cmd::Settings).count();
            eprintln!("i18n-native: language={language:?} labels={} failures={failures} settings-events={settings}",self.labels.len());
            return commands;
        }
        let commands: Vec<Cmd> = self.rx.try_iter().collect();
        self.sync_checks(commands.iter().any(|c| c.proof().is_some() || matches!(c, Cmd::GamutWarning | Cmd::Navigator)));
        commands
    }

    /// Check marks of the colour proof and the gamut warning follow the state (also after ⌘Y, ⇧⌘Y and the
    /// settings); `force`: a check entry was clicked (the system toggled its mark by itself).
    pub fn sync_checks(&self, force: bool) {
        let (proof, warning) = crate::proof::display_key();
        let key = (proof, warning, crate::navigator::enabled());
        if !force && self.checks_shown.get() == Some(key) {
            return;
        }
        let current = crate::proof::current();
        for (item, cmd) in &self.checks {
            let on = match cmd {
                Cmd::GamutWarning => crate::proof::warning(),
                Cmd::Navigator => crate::navigator::enabled(),
                c => c.proof() == Some(current),
            };
            item.set_checked(on);
        }
        self.checks_shown.set(if force { None } else { Some(key) });
    }
}

/// SF Symbols by stable Rust item identity and matching NSMenu position, never by title.
#[cfg(target_os = "macos")]
fn set_symbols(menu: &Menu, icons: &HashMap<muda::MenuId, &'static str>, keys: &HashMap<muda::MenuId, (String, u64)>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    unsafe fn walk(
        native: *mut AnyObject,
        items: &[muda::MenuItemKind],
        icons: &HashMap<muda::MenuId, &'static str>,
        keys: &HashMap<muda::MenuId, (String, u64)>,
        found: &mut Vec<(usize, String, u64)>,
    ) {
        if native.is_null() {
            return;
        }
        let count: isize = msg_send![native, numberOfItems];
        for (index, item) in items.iter().enumerate().take(count.max(0) as usize) {
            let native_item: *mut AnyObject = msg_send![native,itemAtIndex:index as isize];
            if native_item.is_null() {
                continue;
            }
            if let Some(symbol) = icons.get(item.id()) {
                if let Some(class) = AnyClass::get(c"NSImage") {
                    let name = NSString::from_str(symbol);
                    let image: *mut AnyObject = msg_send![class,imageWithSystemSymbolName:&*name,accessibilityDescription:std::ptr::null::<NSString>()];
                    if !image.is_null() {
                        let _: () = msg_send![native_item,setImage:image];
                    }
                }
            }
            if let Some((k, mask)) = keys.get(item.id()) {
                found.push((native_item as usize, k.clone(), *mask));
            }
            if let muda::MenuItemKind::Submenu(sub) = item {
                let child: *mut AnyObject = msg_send![native_item, submenu];
                walk(child, &sub.items(), icons, keys, found);
            }
        }
    }
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    unsafe {
        let app: *mut AnyObject = msg_send![class, sharedApplication];
        let main: *mut AnyObject = msg_send![app, mainMenu];
        let mut found = Vec::new();
        walk(main, &menu.items(), icons, keys, &mut found);
        single_key_equivalents(found);
    }
}
#[cfg(windows)]
fn set_symbols(_: &Menu, _: &HashMap<muda::MenuId, &'static str>, _: &HashMap<muda::MenuId, (String, u64)>) {}

/// Keys without ⌘ (P, X, R, G …) appear right-aligned in the shortcut column like real shortcuts – but only while
/// the menu bar is open. Closed, the items have no key equivalent, so the keys reach the window (text fields,
/// tools that use the same key differently) exactly as before.
#[cfg(target_os = "macos")]
fn single_key_equivalents(items: Vec<(usize, String, u64)>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    use std::ptr::NonNull;
    static ITEMS: Mutex<Vec<(usize, String, u64)>> = Mutex::new(Vec::new());
    static OBSERVING: std::sync::Once = std::sync::Once::new();
    *ITEMS.lock().unwrap_or_else(|e| e.into_inner()) = items;
    fn apply(open: bool) {
        let items = ITEMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for (ptr, key, mask) in items {
            let item = ptr as *mut AnyObject;
            let text = NSString::from_str(if open { key.as_str() } else { "" });
            unsafe {
                let _: () = msg_send![item, setKeyEquivalent: &*text];
                let _: () = msg_send![item, setKeyEquivalentModifierMask: if open { mask as usize } else { 0usize }];
            }
        }
    }
    OBSERVING.call_once(|| unsafe {
        let Some(class) = AnyClass::get(c"NSNotificationCenter") else { return };
        let center: *mut AnyObject = msg_send![class, defaultCenter];
        if center.is_null() {
            return;
        }
        for (name, open) in [("NSMenuDidBeginTrackingNotification", true), ("NSMenuDidEndTrackingNotification", false)] {
            let n = NSString::from_str(name);
            let block = block2::RcBlock::new(move |_note: NonNull<AnyObject>| apply(open));
            let _observer: *mut AnyObject = msg_send![center, addObserverForName: &*n, object: std::ptr::null::<AnyObject>(), queue: std::ptr::null::<AnyObject>(), usingBlock: &*block];
            // the observer stays for the lifetime of the app
            std::mem::forget(block);
        }
    });
}

#[cfg(any(target_os = "macos", windows))]
#[cfg(test)]
mod localization_tests {
    use super::*;
    #[test]
    fn action_ids_shortcuts_and_symbols_survive_language_changes() {
        let mut ids = std::collections::HashSet::new();
        for (cmd, key, shortcut, symbol) in ACTIONS {
            assert!(ids.insert(key.id()));
            for language in [Language::De, Language::En, Language::Ja, Language::Fr, Language::Es, Language::De] {
                let title = i18n::text_in(language, *key);
                assert!(!title.is_empty());
                assert!(!from_table(title, *shortcut).0.is_empty());
                let original: serde_json::Value =
                    serde_json::from_str(include_str!("../tests/snapshots/menu-de.json")).unwrap();
                let row = original
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["action"] == format!("{cmd:?}"))
                    .unwrap();
                assert_eq!(key.id(), row["text_id"].as_str().unwrap());
                assert_eq!(
                    i18n::text_in(Language::De, *key),
                    row["de"].as_str().unwrap()
                );
                assert_eq!(*shortcut, row["shortcut"].as_str());
                assert_eq!(*symbol, row["symbol"].as_str());
                assert_eq!(
                    from_table(i18n::text_in(Language::De, *key), *shortcut).1,
                    from_table(i18n::text_in(language, *key), *shortcut).1
                );
            }
        }
    }
    /// Menu shortcuts that reach egui run once as a menu command and are consumed, so the other key handlers
    /// (grid, single view) no longer see them; ⇧ combinations do not count as the plain shortcut.
    #[test]
    fn menu_keys_in_egui_run_once_and_are_consumed() {
        let ctx = egui::Context::default();
        let key = |k: egui::Key, modifiers: egui::Modifiers| egui::Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers };
        let cases = [
            (key(egui::Key::Z, egui::Modifiers::COMMAND), vec![Cmd::Undo]),
            (key(egui::Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT), vec![Cmd::Redo]),
            (key(egui::Key::I, egui::Modifiers::COMMAND), vec![Cmd::TabInfo]),
            (key(egui::Key::I, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT), vec![Cmd::Import]),
            (key(egui::Key::Quote, egui::Modifiers::COMMAND), vec![Cmd::VirtualCopy]),
            // series and stacks: ⌘G bundles, ⇧⌘G dissolves (menu Foto); N and ⇧S stay keys of the grid
            (key(egui::Key::G, egui::Modifiers::COMMAND), vec![Cmd::StackGroup]),
            (key(egui::Key::G, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT), vec![Cmd::StackDissolve]),
            (key(egui::Key::N, egui::Modifiers::NONE), vec![]),
            (key(egui::Key::S, egui::Modifiers::SHIFT), vec![]),
            (key(egui::Key::F, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT), vec![Cmd::ShowInFinder]),
            (key(egui::Key::Z, egui::Modifiers::NONE), vec![]),
            // an extra ⌥ does not run the shorter shortcut: ⌥⌘R is not ⌘R (turn left)
            (key(egui::Key::R, egui::Modifiers::COMMAND | egui::Modifiers::ALT), vec![]),
            (key(egui::Key::R, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT), vec![Cmd::RotateRight]),
            (key(egui::Key::R, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT | egui::Modifiers::ALT), vec![Cmd::ResetSettings]),
            // ⇧⌘C stays "copy settings" (a key event); ⌘C comes as egui's copy event (below)
            (key(egui::Key::C, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT), vec![Cmd::CopySettings]),
        ];
        for (event, expected) in cases {
            let mut got = Vec::new();
            let mut left = true;
            let _ = crate::ui_test::run(&ctx, egui::RawInput { events: vec![event.clone()], ..Default::default() }, |ui| {
                got = shortcut_without_menu(ui.ctx());
                left = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })));
            });
            assert_eq!(got, expected, "{event:?}");
            assert_eq!(left, expected.is_empty(), "consumed: {event:?}");
        }
        // ⌘C reaches egui as a copy event: "Copy photo", consumed (no text field has the focus)
        let mut got = Vec::new();
        let mut left = true;
        let _ = crate::ui_test::run(&ctx, egui::RawInput { events: vec![egui::Event::Copy], ..Default::default() }, |ui| {
            got = shortcut_without_menu(ui.ctx());
            left = ui.input(|i| i.events.contains(&egui::Event::Copy));
        });
        assert_eq!(got, vec![Cmd::CopyPhoto]);
        assert!(!left);
    }

    #[test]
    fn router_delivers_once_after_rebinding_and_ignores_old_ids() {
        let ctx = egui::Context::default();
        let (tx, rx) = channel();
        let id = muda::MenuId::new("graineo:menu-export");
        let router: Router = Arc::new(Mutex::new(Some(Route {
            generation: 1,
            ids: HashMap::from([(id.clone(), Cmd::Export)]),
            tx,
            ctx: ctx.clone(),
        })));
        for _language in [Language::De, Language::En, Language::De] {
            deliver(&router, &id);
            assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![Cmd::Export]);
        }
        let (tx2, rx2) = channel();
        *router.lock().unwrap() = Some(Route {
            generation: 2,
            ids: HashMap::from([(id.clone(), Cmd::Export)]),
            tx: tx2,
            ctx,
        });
        deliver(&router, &id);
        assert_eq!(rx2.try_iter().collect::<Vec<_>>(), vec![Cmd::Export]);
        assert!(rx.try_recv().is_err());
        deliver(&router, &muda::MenuId::new("stale"));
        assert!(rx2.try_recv().is_err());
    }
}
