//! Colour proof of the display (soft proof): limits only what the screen shows to a colour space (sRGB, Display P3,
//! Adobe RGB, ProPhoto) and optionally marks the pixels outside it (gamut warning). Computing, editing and export
//! stay unchanged; switching never starts a computation – egui's shader applies it as a uniform to every photo
//! texture (see [`crate::photo_display`]): fitted, zoomed, loupe, navigator, comparison. 8-bit photos (thumbnails,
//! grid, film, monochrome) are sRGB already and lie inside every proof space, so there the proof changes nothing.
//!
//! The choice applies to the session (not per photo); the start value comes from the setting `proofStart`.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

use graineo_color::Space;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proof {
    /// no limit (default): everything the display can show
    Full,
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
}

impl Proof {
    pub const ALL: [Proof; 5] = [Proof::Full, Proof::Srgb, Proof::DisplayP3, Proof::AdobeRgb, Proof::ProPhoto];

    /// Colour space of the proof (`None`: full gamut).
    pub fn space(self) -> Option<Space> {
        match self {
            Proof::Full => None,
            Proof::Srgb => Some(Space::Srgb),
            Proof::DisplayP3 => Some(Space::DisplayP3),
            Proof::AdobeRgb => Some(Space::AdobeRgb),
            Proof::ProPhoto => Some(Space::ProPhotoRgb),
        }
    }

    /// Name as shown (colour space names stay untranslated).
    pub fn name(self) -> &'static str {
        match self {
            Proof::Full => crate::tr("Voller Umfang"),
            Proof::Srgb => "sRGB",
            Proof::DisplayP3 => "Display P3",
            Proof::AdobeRgb => "Adobe RGB",
            Proof::ProPhoto => "ProPhoto",
        }
    }

    /// Key in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Proof::Full => "full",
            Proof::Srgb => "srgb",
            Proof::DisplayP3 => "displayP3",
            Proof::AdobeRgb => "adobeRgb",
            Proof::ProPhoto => "proPhoto",
        }
    }

    fn from_key(k: &str) -> Option<Proof> {
        Proof::ALL.into_iter().find(|p| p.key() == k)
    }

    fn index(self) -> u8 {
        Proof::ALL.iter().position(|p| *p == self).unwrap_or(0) as u8
    }

    fn from_index(i: u8) -> Proof {
        Proof::ALL.get(i as usize).copied().unwrap_or(Proof::Full)
    }
}

struct State {
    current: AtomicU8,
    /// last chosen proof other than "full" (target of ⌘Y and of the warning without a proof)
    last: AtomicU8,
    warning: AtomicBool,
    /// counts every change (the display layer redraws on a new value)
    generation: AtomicU64,
}

fn state() -> &'static State {
    static S: OnceLock<State> = OnceLock::new();
    S.get_or_init(|| {
        let start = start();
        State { current: AtomicU8::new(start.index()), last: AtomicU8::new(if start == Proof::Full { Proof::Srgb.index() } else { start.index() }), warning: AtomicBool::new(false), generation: AtomicU64::new(1) }
    })
}

/// Start proof from the settings (default: full gamut); test hook `GRAINEO_PROOF=<key>[,warning]`.
pub fn start() -> Proof {
    if let Ok(v) = crate::env::var("GRAINEO_PROOF") {
        return v.split(',').next().and_then(Proof::from_key).unwrap_or(Proof::Full);
    }
    if cfg!(test) {
        return Proof::Full;
    }
    library::settings::value("proofStart").as_str().and_then(Proof::from_key).unwrap_or(Proof::Full)
}

/// Setting: proof at start (saved; the current session keeps its choice).
pub fn set_start(p: Proof) {
    library::settings::set("proofStart", serde_json::json!(p.key()));
}

pub fn current() -> Proof {
    Proof::from_index(state().current.load(Ordering::Relaxed))
}

pub fn warning() -> bool {
    let s = state();
    // test hook: warning switched on from the start
    static TEST: OnceLock<()> = OnceLock::new();
    TEST.get_or_init(|| {
        if crate::env::var("GRAINEO_PROOF").is_ok_and(|v| v.split(',').any(|p| p == "warning")) {
            s.warning.store(true, Ordering::Relaxed);
        }
    });
    s.warning.load(Ordering::Relaxed)
}

/// Change counter (a new value means: draw the display again, nothing to compute).
pub fn generation() -> u64 {
    let _ = warning();
    state().generation.load(Ordering::Relaxed)
}

fn bump() {
    state().generation.fetch_add(1, Ordering::Relaxed);
}

pub fn set(p: Proof) {
    let s = state();
    s.current.store(p.index(), Ordering::Relaxed);
    if p != Proof::Full {
        s.last.store(p.index(), Ordering::Relaxed);
    }
    bump();
}

/// ⌘Y: between full gamut and the last chosen proof.
pub fn toggle() {
    if current() == Proof::Full {
        set(Proof::from_index(state().last.load(Ordering::Relaxed)));
    } else {
        set(Proof::Full);
    }
}

/// ⇧⌘Y: gamut warning on/off.
pub fn toggle_warning() {
    let on = !warning();
    state().warning.store(on, Ordering::Relaxed);
    bump();
}

/// Space the warning marks against: the proof, or without a proof the last chosen one.
pub fn warning_space() -> Option<Space> {
    if !warning() {
        return None;
    }
    match current() {
        Proof::Full => Proof::from_index(state().last.load(Ordering::Relaxed)).space(),
        p => p.space(),
    }
}

/// Short note over the image while a proof or the warning is active (`None`: nothing active).
pub fn note() -> Option<String> {
    let p = current();
    match (p, warning()) {
        (Proof::Full, false) => None,
        (Proof::Full, true) => {
            let last = Proof::from_index(state().last.load(Ordering::Relaxed));
            Some(crate::trf!("Warnung: {space}", space = last.name()))
        }
        (p, false) => Some(crate::trf!("Proof: {space}", space = p.name())),
        (p, true) => Some(crate::trf!("Proof: {space} + Warnung", space = p.name())),
    }
}

/// Uniform data of the display layer: matrices layer space → proof space and back (rows as vec4), flags and the
/// warning colour (in the layer space). `layer`: colour space of the layer's linear values; `hdr`: values above 1
/// are brightness, not colour (only the lower bound limits).
pub fn uniform(layer: Space, hdr: bool) -> [f32; 32] {
    let mut u = [0f32; 32];
    let limit = current().space();
    let warn = warning_space();
    // one target space: the proof; without a proof the warning's space (only marked, not limited)
    let Some(target) = limit.or(warn) else { return u };
    let (Ok(to), Ok(back)) = (graineo_color::matrix(layer, target), graineo_color::matrix(target, layer)) else { return u };
    for r in 0..3 {
        for c in 0..3 {
            u[r * 4 + c] = to[r][c] as f32;
            u[12 + r * 4 + c] = back[r][c] as f32;
        }
    }
    u[24] = limit.is_some() as u8 as f32;
    u[25] = warn.is_some() as u8 as f32;
    u[26] = if hdr { 0.0 } else { 1.0 };
    // warning colour: bright magenta (sRGB 1, 0, 1), rarely a real image colour and visible on dark and light parts
    let m = graineo_color::matrix(Space::Srgb, layer).unwrap_or([[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]);
    for r in 0..3 {
        u[28 + r] = (m[r][0] + m[r][2]) as f32;
    }
    u[31] = 1.0;
    u
}

/// The display state that decides the drawing (redraw on change).
pub fn display_key() -> (u8, bool) {
    (current().index(), warning())
}

/// Tests that change the (global) proof state run one after another.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// ⌘Y toggles between full gamut and the last chosen proof; the warning has its own switch; uniform matrices.
    #[test]
    fn toggle_and_uniform() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set(Proof::AdobeRgb);
        toggle();
        assert_eq!(current(), Proof::Full);
        toggle();
        assert_eq!(current(), Proof::AdobeRgb);
        let w = warning();
        toggle_warning();
        assert_eq!(warning(), !w);
        toggle_warning();
        // without proof and warning the uniform is neutral (flags 0); with sRGB proof on a P3 layer the matrices are
        // inverse to each other (one test: the state is global)
        set(Proof::Full);
        assert_eq!(uniform(Space::DisplayP3, false)[24], 0.0);
        set(Proof::Srgb);
        let u = uniform(Space::DisplayP3, false);
        assert_eq!(u[24], 1.0);
        for r in 0..3 {
            for c in 0..3 {
                let v: f32 = (0..3).map(|k| u[12 + r * 4 + k] * u[k * 4 + c]).sum();
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-4);
            }
        }
        set(Proof::Full);
    }

    /// In the running app: menu commands of the proof (⌘Y, ⇧⌘Y, a space) draw only – no job is sent.
    #[test]
    fn switching_in_the_app_sends_no_job() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use eframe::egui::{self, Pos2, Rect, Vec2};
        use eframe::App as _;
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-proof-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(1200, 800, image::Rgb([200, 40, 30])).save(folder.join("rot.png")).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = crate::App::new(&cc);
        a.open_folder_inner(folder.clone());
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        let mut clock = 0.0;
        let mut frame = |a: &mut crate::App| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0))), ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
            a.handle_results(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        // settle: until no job is running any more and a few frames in a row send nothing. Quiet frames alone were not
        // enough: opening the photo could still be running (slow under parallel tests), and its result then started the
        // render jobs in the middle of the switching below
        let mut quiet = 0;
        while quiet < 8 {
            let before = crate::jobs::SENT.with(|c| c.get());
            frame(&mut a);
            quiet = if crate::jobs::SENT.with(|c| c.get()) == before && a.worker.running() == 0 { quiet + 1 } else { 0 };
            assert!(std::time::Instant::now() < deadline, "app does not settle");
        }
        let before = crate::jobs::SENT.with(|c| c.get());
        let gen = generation();
        for cmd in [crate::menu::Cmd::ProofToggle, crate::menu::Cmd::GamutWarning, crate::menu::Cmd::ProofAdobe, crate::menu::Cmd::GamutWarning, crate::menu::Cmd::ProofFull] {
            a.run_cmd(cmd, &ctx);
            frame(&mut a);
        }
        assert!(generation() > gen);
        assert_eq!(current(), Proof::Full);
        assert_eq!(crate::jobs::SENT.with(|c| c.get()), before, "switching the proof sent a job");
        let _ = std::fs::remove_dir_all(folder);
    }
}
