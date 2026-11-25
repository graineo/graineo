//! Appearance of the interface (Settings → Interface): System, Dark, Medium, Light.
//! The palette itself lives in `lr.rs` ([`lr::Theme`], [`lr::gray`]); this module keeps the
//! choice, saves it and follows the operating system for "System".

use crate::lr;
use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Appearance {
    System,
    Dark,
    Medium,
    Light,
}

impl Appearance {
    pub const ALL: [Appearance; 4] = [Appearance::System, Appearance::Dark, Appearance::Medium, Appearance::Light];

    fn key(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Dark => "dark",
            Appearance::Medium => "medium",
            Appearance::Light => "light",
        }
    }

    fn from_key(k: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == k)
    }

    pub fn label(self) -> String {
        // own keys: "Dunkel"/"Mittel"/"Hell" alone are used elsewhere with other meanings
        let (de, key) = match self {
            Appearance::System => ("Wie das System", "Wie das System"),
            Appearance::Dark => ("Dunkel", "Dunkel (Erscheinungsbild)"),
            Appearance::Medium => ("Mittel", "Mittel (Erscheinungsbild)"),
            Appearance::Light => ("Hell", "Hell (Erscheinungsbild)"),
        };
        crate::menu::i18n::german_or(de, crate::tr(key)).to_string()
    }
}

static CHOICE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub fn appearance() -> Appearance {
    Appearance::ALL[CHOICE.load(std::sync::atomic::Ordering::Relaxed) as usize % 4]
}

fn file() -> Option<std::path::PathBuf> {
    crate::app_file("interface-theme.txt")
}

/// Load the saved appearance (at startup). Default: Dark. `GRAINEO_THEME=dark|medium|light|system`
/// overrides it for test runs (screenshots) without saving.
pub fn load(ctx: &egui::Context) {
    let saved = std::env::var("GRAINEO_THEME").ok().or_else(|| file().and_then(|p| std::fs::read_to_string(p).ok()));
    let a = saved.as_deref().map(str::trim).and_then(Appearance::from_key).unwrap_or(Appearance::Dark);
    set(ctx, a);
}

fn set(ctx: &egui::Context, a: Appearance) {
    CHOICE.store(Appearance::ALL.iter().position(|x| *x == a).unwrap_or(1) as u8, std::sync::atomic::Ordering::Relaxed);
    lr::set_theme(ctx, resolve(ctx, a));
}

fn resolve(ctx: &egui::Context, a: Appearance) -> lr::Theme {
    match a {
        Appearance::Dark => lr::Theme::Dark,
        Appearance::Medium => lr::Theme::Medium,
        Appearance::Light => lr::Theme::Light,
        Appearance::System => match ctx.system_theme() {
            Some(egui::Theme::Light) => lr::Theme::Light,
            _ => lr::Theme::Dark,
        },
    }
}

/// Choose and save an appearance.
pub fn choose(ctx: &egui::Context, a: Appearance) {
    if a == appearance() {
        return;
    }
    set(ctx, a);
    if let Some(p) = file() {
        let _ = std::fs::write(p, format!("{}\n", a.key()));
    }
}

/// Once per frame: with "System", switch along with the operating system.
pub fn follow_system(ctx: &egui::Context) {
    if appearance() == Appearance::System {
        lr::set_theme(ctx, resolve(ctx, Appearance::System));
    }
}

/// Selection for the settings page (Settings → Interface → Appearance).
pub fn combo(ui: &mut egui::Ui, ctx: &egui::Context) {
    let mut a = appearance();
    egui::ComboBox::from_id_salt("prefs-ui-theme").selected_text(a.label()).width(150.0).show_ui(ui, |ui| {
        for v in Appearance::ALL {
            ui.selectable_value(&mut a, v, v.label());
        }
    });
    choose(ctx, a);
}
