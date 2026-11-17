//! Operating mode for slower computers (Settings → Develop → Performance): "Automatisch" (default), "Genau",
//! "Sparsam".
//!
//! A mode changes only **when** and **in which order** work is done, never the result: how long the app waits on a
//! photo before it computes Sigma's exact base, whether it computes the next photo ahead, how soon the full resolution
//! of other RAWs is prepared, how many opened photos stay in memory. The exact view always follows when the user stays
//! with a photo, at once when zooming, on noise sliders and for every export; the fitted view shows the spinner with
//! its tooltip ("wird verfeinert …") until then. Every photo is computed once per settings state as before.
//!
//! "Automatisch" picks "Genau" or "Sparsam":
//! - from the app's own measured job times (Sigma base, full resolution of other RAWs) as a factor against the
//!   reference computer ([`Tuning::reference`]), median and hysteresis, the first GPU job of a session, cache hits and
//!   jobs under heavy load from other programs left out ([`next_tier`]);
//! - before the first measurements from the hardware (performance cores, processor kind) as a start value;
//! - temporarily "Sparsam" on battery, in Low Power Mode, when the computer is hot or memory is short; the measured
//!   state stays as it was.
//!
//! All numbers live in [`Tuning`] (provisional until the reports of slower computers arrive); the setting
//! `performanceTuning` (JSON, same field names) overrides single values without a new build. The measured state is
//! kept in the setting `performanceProfile` (hardware, compute-path version, the last factors) – only on this
//! computer, nothing is sent anywhere.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Setting keys.
const SETTING_MODE: &str = "performanceMode";
const SETTING_PROFILE: &str = "performanceProfile";
const SETTING_TUNING: &str = "performanceTuning";

/// What the user chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Auto,
    Exact,
    Saving,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::Auto, Choice::Exact, Choice::Saving];

    fn key(self) -> &'static str {
        match self {
            Choice::Auto => "auto",
            Choice::Exact => "exact",
            Choice::Saving => "saving",
        }
    }

    fn from_key(k: &str) -> Option<Choice> {
        Choice::ALL.into_iter().find(|c| c.key() == k)
    }

    /// Name shown in the settings (translated).
    pub fn name(self) -> &'static str {
        match self {
            Choice::Auto => crate::tr("Automatisch"),
            Choice::Exact => crate::tr("Genau"),
            Choice::Saving => crate::tr("Sparsam"),
        }
    }
}

/// How the app computes right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Exact,
    Saving,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Exact => crate::tr("Genau"),
            Tier::Saving => crate::tr("Sparsam"),
        }
    }
    fn key(self) -> &'static str {
        match self {
            Tier::Exact => "exact",
            Tier::Saving => "saving",
        }
    }
}

/// What a tier means for the planning of the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Sigma: start the exact base together with opening the photo (otherwise after it appeared)
    pub early_base: bool,
    /// Sigma: dwell after the photo appeared before its exact base starts (s); cut short by zoom and noise sliders
    pub base_dwell: f64,
    /// Sigma: compute the base of the next photo ahead
    pub base_ahead: bool,
    /// other RAWs: dwell before the full resolution is prepared ahead (s); zooming prepares it at once
    pub full_dwell: f64,
    /// opened photos kept in memory besides the one on screen
    pub cache_limit: usize,
    /// preload the photo behind as well (otherwise only the next one in the direction of travel)
    pub neighbours_behind: bool,
}

/// Why the app computes the way it does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cause {
    /// the user chose the mode
    Chosen,
    /// start value from the hardware: no measurement yet
    Hardware { factor: f64 },
    /// measured: median factor of the last jobs
    Measured { factor: f64, samples: usize },
    /// temporary (only "Automatisch")
    Battery,
    LowPower,
    Thermal,
    Memory,
}

impl Cause {
    /// Reason in words (translated), for the settings, the status line and the spinner's tooltip.
    pub fn text(self) -> String {
        match self {
            Cause::Chosen => crate::tr("von Hand gewählt").to_string(),
            Cause::Hardware { factor } => crate::trf!("Startwert nach der Hardware (Rechenfaktor ≈ {f}); noch keine Messung", f = factor_text(factor)),
            Cause::Measured { factor, samples } => crate::trf!("gemessen: Rechenfaktor {f} aus {n} Rechnungen", f = factor_text(factor), n = samples),
            Cause::Battery => crate::tr("vorübergehend: Akkubetrieb").to_string(),
            Cause::LowPower => crate::tr("vorübergehend: Stromsparmodus").to_string(),
            Cause::Thermal => crate::tr("vorübergehend: Rechner ist heiß").to_string(),
            Cause::Memory => crate::tr("vorübergehend: Arbeitsspeicher knapp").to_string(),
        }
    }

    /// Same in English (performance report, log).
    pub fn english(self) -> String {
        match self {
            Cause::Chosen => "chosen by the user".into(),
            Cause::Hardware { factor } => format!("start value from the hardware (factor ≈ {factor:.1}), no measurement yet"),
            Cause::Measured { factor, samples } => format!("measured: factor {factor:.2} from {samples} jobs"),
            Cause::Battery => "temporary: on battery".into(),
            Cause::LowPower => "temporary: low power mode".into(),
            Cause::Thermal => "temporary: computer is hot".into(),
            Cause::Memory => "temporary: memory is short".into(),
        }
    }
}

fn factor_text(f: f64) -> String {
    crate::menu::i18n::format::decimal(crate::menu::i18n::language(), f, 1)
}

/// The numbers behind the automatic choice. **Provisional:** the references come from one computer (MacBook Pro M1
/// Pro, 10 cores, 16 GB; performance report of 16.11.2026, warm medians, light load from other programs); the slow
/// Intel Mac and the Windows computer are not measured yet. Their reports set the thresholds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tuning {
    /// Reference seconds per megapixel on the reference computer: Merrill base GPU/CPU, Quattro base GPU/CPU,
    /// full resolution Bayer GPU/CPU, X-Trans GPU/CPU.
    pub reference: [f64; 8],
    /// "Sparsam" when at least `enter_count` of the last `enter_window` factors are above this (computer this much
    /// slower than the reference)
    pub enter: f64,
    pub enter_count: usize,
    pub enter_window: usize,
    /// back to "Genau" when the last `leave_window` factors all stay below this
    pub leave: f64,
    pub leave_window: usize,
    /// factors needed before the measurement replaces the hardware start value
    pub min_samples: usize,
    /// a job counts only when other programs kept less than this share of the processor busy meanwhile
    pub max_foreign_load: f64,
    /// start value: performance cores of the reference computer, and the factor for x86 processors
    pub reference_cores: f64,
    pub x86_factor: f64,
    /// plans of the two tiers
    pub exact_base_dwell: f64,
    pub saving_base_dwell: f64,
    pub exact_full_dwell: f64,
    pub saving_full_dwell: f64,
    pub exact_cache: usize,
    pub saving_cache: usize,
    /// computers with at most this much memory keep only `saving_cache` photos in memory, in every tier
    pub small_memory_gb: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            // 16.11.2026 report, M1 Pro (seconds / MP): DP3M0004 14.8 MP base 1.08 s GPU, 1.51 s CPU; SDQ_0812
            // 19.6 MP base 2.38 s GPU, 2.26 s CPU; IMG_0361 26 MP full resolution 0.74 s GPU, 1.50 s CPU;
            // DSCF0804 24 MP 1.36 s GPU, 2.80 s CPU
            reference: [1.08 / 14.8, 1.51 / 14.8, 2.38 / 19.6, 2.26 / 19.6, 0.74 / 26.0, 1.50 / 26.0, 1.36 / 24.0, 2.80 / 24.0],
            // a Merrill base then takes > 2.7 s on the GPU (> 3.8 s on the processor)
            enter: 2.5,
            enter_count: 3,
            enter_window: 5,
            leave: 1.8,
            leave_window: 8,
            min_samples: 3,
            max_foreign_load: 0.4,
            reference_cores: 8.0,
            x86_factor: 1.5,
            exact_base_dwell: 0.0,
            saving_base_dwell: 2.0,
            exact_full_dwell: crate::browse::FULL_DWELL,
            saving_full_dwell: 4.0,
            exact_cache: 4,
            saving_cache: 2,
            small_memory_gb: 8.5,
        }
    }
}

/// Measured kinds of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    /// Sigma's exact base (`quattro`: Quattro, otherwise Merrill)
    SigmaBase { quattro: bool },
    /// full resolution of another RAW
    RawFull { xtrans: bool },
}

impl Work {
    fn reference(self, gpu: bool, t: &Tuning) -> f64 {
        let i = match self {
            Work::SigmaBase { quattro: false } => 0,
            Work::SigmaBase { quattro: true } => 2,
            Work::RawFull { xtrans: false } => 4,
            Work::RawFull { xtrans: true } => 6,
        };
        t.reference[i + usize::from(!gpu)]
    }
    fn slot(self) -> usize {
        match self {
            Work::SigmaBase { quattro: false } => 0,
            Work::SigmaBase { quattro: true } => 1,
            Work::RawFull { xtrans: false } => 2,
            Work::RawFull { xtrans: true } => 3,
        }
    }
    fn english(self) -> &'static str {
        match self {
            Work::SigmaBase { quattro: false } => "Merrill base",
            Work::SigmaBase { quattro: true } => "Quattro base",
            Work::RawFull { xtrans: false } => "Bayer full resolution",
            Work::RawFull { xtrans: true } => "X-Trans full resolution",
        }
    }
}

/// Stored in the setting `performanceProfile`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Profile {
    hardware: String,
    compute_path: u32,
    /// last factors, oldest first
    factors: Vec<f64>,
    /// measured tier (`None`: still the hardware start value)
    tier: Option<Tier>,
}

/// One measured job, as the report lists it.
#[derive(Clone, Debug)]
pub struct Sample {
    pub work: &'static str,
    pub megapixels: f64,
    pub seconds: f64,
    pub gpu: bool,
    /// `None`: left out (with the reason)
    pub factor: Option<f64>,
    pub skipped: Option<&'static str>,
}

#[derive(Default)]
struct State {
    choice: Option<Choice>,
    tuning: Tuning,
    profile: Profile,
    hardware_factor: f64,
    /// temporary cause from the system state (only applied with "Automatisch")
    temporary: Option<Cause>,
    /// GPU jobs seen per kind of work in this session (the first one sets up shaders and pipelines)
    gpu_seen: [bool; 4],
    /// the last measured jobs, for the report
    recent: VecDeque<Sample>,
    /// effective state last announced
    announced: Option<(Tier, CauseKind)>,
}

/// Cause without its numbers (a new measurement does not count as a change).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CauseKind {
    Chosen,
    Hardware,
    Measured,
    Battery,
    LowPower,
    Thermal,
    Memory,
}

fn kind(c: Cause) -> CauseKind {
    match c {
        Cause::Chosen => CauseKind::Chosen,
        Cause::Hardware { .. } => CauseKind::Hardware,
        Cause::Measured { .. } => CauseKind::Measured,
        Cause::Battery => CauseKind::Battery,
        Cause::LowPower => CauseKind::LowPower,
        Cause::Thermal => CauseKind::Thermal,
        Cause::Memory => CauseKind::Memory,
    }
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(load())).lock().unwrap_or_else(|e| e.into_inner())
}

/// Effective tier for [`plan`] (0 = not decided yet, 1 = exact, 2 = saving) – read every frame without a lock.
static TIER: AtomicU8 = AtomicU8::new(0);
/// Small memory (cache limit), decided once.
static SMALL_MEMORY: AtomicU8 = AtomicU8::new(0);
/// The effective tier changed while the app runs: the window shows it once.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// Power state as last seen by the watcher: 1 = Low Power Mode, 2 = on battery (without Low Power Mode), 0 = mains.
static POWER: AtomicU8 = AtomicU8::new(0);

/// How the computer is powered right now (for quiet background jobs that pause or slow down to save energy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Mains,
    Battery,
    LowPower,
}

/// The power state the watcher saw last (cheap: one atomic read; `Mains` before the first look).
pub fn power() -> Power {
    match POWER.load(Ordering::Relaxed) {
        1 => Power::LowPower,
        2 => Power::Battery,
        _ => Power::Mains,
    }
}

fn test_run() -> bool {
    crate::env::var_os("GRAINEO_DATA_DIR").is_some()
}

/// Hardware id: processor, cores, memory (no serial number, no name). New hardware → measurements start over.
fn hardware_id() -> String {
    let cpu = hardware_cpu();
    let mem = pools::physical_memory().map_or(0, |m| (m as f64 / 1e9).round() as u64);
    format!("{cpu}|{}|{}|{mem}", library::platform::info::physical_cores().unwrap_or(0), std::env::consts::ARCH)
}

fn hardware_cpu() -> String {
    static CPU: OnceLock<String> = OnceLock::new();
    CPU.get_or_init(library::platform::info::cpu_name).clone()
}

/// Start value from the hardware: how much slower than the reference computer it is likely to be (pure, tests).
pub fn hardware_factor(performance_cores: Option<usize>, x86: bool, t: &Tuning) -> f64 {
    let cores = performance_cores.unwrap_or(4).max(1) as f64;
    let f = t.reference_cores / cores;
    if x86 {
        f * t.x86_factor
    } else {
        f
    }
}

fn load() -> State {
    let mut tuning = Tuning::default();
    let over = library::settings::value(SETTING_TUNING);
    if over.is_object() {
        if let Ok(t) = serde_json::from_value::<Tuning>(over) {
            tuning = t;
        }
    }
    let choice = crate::env::var("GRAINEO_PERF_MODE")
        .ok()
        .filter(|_| test_run())
        .and_then(|v| Choice::from_key(&v))
        .or_else(|| library::settings::value(SETTING_MODE).as_str().and_then(Choice::from_key));
    let mut profile: Profile = serde_json::from_value(library::settings::value(SETTING_PROFILE)).unwrap_or_default();
    let hw = hardware_id();
    let path = develop::diskcache::COMPUTE_PATH_VERSION;
    if profile.hardware != hw || profile.compute_path != path {
        // other computer or other compute path: measure anew
        profile = Profile { hardware: hw, compute_path: path, ..Default::default() };
    }
    let hardware_factor = crate::env::var("GRAINEO_PERF_HW_FACTOR")
        .ok()
        .filter(|_| test_run())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| hardware_factor(library::platform::info::performance_cores(), cfg!(target_arch = "x86_64"), &tuning));
    let small = pools::physical_memory().is_some_and(|m| (m as f64) <= tuning.small_memory_gb * 1e9);
    SMALL_MEMORY.store(if small { 2 } else { 1 }, Ordering::Relaxed);
    let s = State { choice, tuning, profile, hardware_factor, ..Default::default() };
    let (tier, _) = effective(&s);
    TIER.store(if tier == Tier::Saving { 2 } else { 1 }, Ordering::Relaxed);
    s
}

/// Tier and cause from the state.
fn effective(s: &State) -> (Tier, Cause) {
    match s.choice.unwrap_or(Choice::Auto) {
        Choice::Exact => (Tier::Exact, Cause::Chosen),
        Choice::Saving => (Tier::Saving, Cause::Chosen),
        Choice::Auto => {
            if let Some(c) = s.temporary {
                return (Tier::Saving, c);
            }
            measured(&s.profile, s.hardware_factor, &s.tuning)
        }
    }
}

/// Tier from the measurements, or the hardware start value while there are too few (pure, tests).
fn measured(p: &Profile, hardware: f64, t: &Tuning) -> (Tier, Cause) {
    match p.tier {
        Some(tier) => (tier, Cause::Measured { factor: median(&p.factors).unwrap_or(hardware), samples: p.factors.len() }),
        None => (if hardware >= t.enter { Tier::Saving } else { Tier::Exact }, Cause::Hardware { factor: hardware }),
    }
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 })
}

/// Measured tier after a new factor (pure, tests): `current` is `None` while the hardware start value holds.
/// - With fewer than `min_samples` factors nothing is measured yet.
/// - The first decision takes the median of the factors.
/// - Then "Genau" → "Sparsam" when `enter_count` of the last `enter_window` factors exceed `enter`; "Sparsam" →
///   "Genau" when the last `leave_window` factors all stay below `leave`. One slow job never switches.
pub fn next_tier(current: Option<Tier>, factors: &[f64], t: &Tuning) -> Option<Tier> {
    if factors.len() < t.min_samples {
        return current;
    }
    let Some(cur) = current else {
        return Some(if median(factors).unwrap_or(0.0) >= t.enter { Tier::Saving } else { Tier::Exact });
    };
    let last = |n: usize| &factors[factors.len().saturating_sub(n)..];
    match cur {
        Tier::Exact => (last(t.enter_window).iter().filter(|&&f| f > t.enter).count() >= t.enter_count).then_some(Tier::Saving).or(Some(Tier::Exact)),
        Tier::Saving => {
            let w = last(t.leave_window);
            (w.len() >= t.leave_window && w.iter().all(|&f| f < t.leave)).then_some(Tier::Exact).or(Some(Tier::Saving))
        }
    }
}

/// Set up: read the setting and the stored measurements, watch the system state in the background.
pub fn init() {
    let (tier, cause) = {
        let mut s = state();
        let e = effective(&s);
        s.announced = Some((e.0, kind(e.1)));
        e
    };
    crate::log(&format!("operating mode: {} ({})", tier.key(), cause.english()));
    if cfg!(test) {
        return;
    }
    let _ = std::thread::Builder::new().name("system-state".into()).spawn(watch_system);
}

/// What the planning should do now (cheap: one atomic read).
pub fn plan() -> Plan {
    let tier = match TIER.load(Ordering::Relaxed) {
        2 => Tier::Saving,
        1 => Tier::Exact,
        _ => effective(&state()).0,
    };
    plan_for(tier, SMALL_MEMORY.load(Ordering::Relaxed) == 2, &tuning())
}

fn tuning() -> Tuning {
    static T: OnceLock<Tuning> = OnceLock::new();
    T.get_or_init(|| state().tuning.clone()).clone()
}

/// The plan of a tier (pure, tests).
pub fn plan_for(tier: Tier, small_memory: bool, t: &Tuning) -> Plan {
    let saving = tier == Tier::Saving;
    Plan {
        early_base: !saving,
        base_dwell: if saving { t.saving_base_dwell } else { t.exact_base_dwell },
        base_ahead: !saving,
        full_dwell: if saving { t.saving_full_dwell } else { t.exact_full_dwell },
        cache_limit: if saving || small_memory { t.saving_cache } else { t.exact_cache },
        neighbours_behind: !saving,
    }
}

/// Tier the app computes in now, as a word for logs ("exact", "saving").
pub fn tier_key() -> &'static str {
    match TIER.load(Ordering::Relaxed) {
        2 => "saving",
        _ => "exact",
    }
}

/// Mode, tier, cause for the settings and the report.
#[derive(Clone, Debug)]
pub struct Status {
    pub choice: Choice,
    pub tier: Tier,
    pub cause: Cause,
    pub hardware_factor: f64,
    /// measured factors kept (oldest first)
    pub factors: Vec<f64>,
}

pub fn status() -> Status {
    let s = state();
    let (tier, cause) = effective(&s);
    Status { choice: s.choice.unwrap_or(Choice::Auto), tier, cause, hardware_factor: s.hardware_factor, factors: s.profile.factors.clone() }
}

pub fn choice() -> Choice {
    state().choice.unwrap_or(Choice::Auto)
}

/// The user chose a mode (settings window). Takes effect with the next photo.
pub fn set_choice(c: Choice) {
    let mut s = state();
    s.choice = Some(c);
    library::settings::set(SETTING_MODE, json!(c.key()));
    update(&mut s, false);
}

/// Measurements start over (settings window).
pub fn reset_measurements() {
    let mut s = state();
    s.profile.factors.clear();
    s.profile.tier = None;
    s.recent.clear();
    save(&s);
    update(&mut s, false);
}

/// The effective tier after a change of state; `announce`: tell the window (a change the user did not make).
fn update(s: &mut State, announce: bool) {
    let (tier, cause) = effective(s);
    TIER.store(if tier == Tier::Saving { 2 } else { 1 }, Ordering::Relaxed);
    let now = (tier, kind(cause));
    if s.announced != Some(now) {
        let tier_changed = s.announced.is_none_or(|(t, _)| t != tier);
        s.announced = Some(now);
        crate::log(&format!("operating mode now {} ({})", tier.key(), cause.english()));
        if announce && tier_changed {
            CHANGED.store(true, Ordering::Relaxed);
        }
    }
}

/// Did the automatic choice switch since the last call? The window then shows the new state once in the status line.
pub fn take_change() -> Option<Status> {
    CHANGED.swap(false, Ordering::Relaxed).then(status)
}

/// Line for the status bar after an automatic switch (translated).
pub fn change_text(st: &Status) -> String {
    crate::trf!("Betriebsart jetzt {t} ({c}) · Einstellungen → Entwickeln → Leistung", t = st.tier.name(), c = st.cause.text())
}

fn save(s: &State) {
    if let Ok(v) = serde_json::to_value(&s.profile) {
        library::settings::set(SETTING_PROFILE, v);
    }
}

/// Load of the computer while a job ran: processor ticks of the whole system and of the own process at the start.
pub struct Probe {
    start: std::time::Instant,
    ticks: Option<(u64, u64)>,
    own: Option<f64>,
}

impl Probe {
    pub fn start() -> Probe {
        Probe { start: std::time::Instant::now(), ticks: library::platform::info::system_cpu_ticks(), own: library::platform::cpu_seconds() }
    }

    /// Seconds since the start.
    pub fn seconds(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Share of the whole processor other programs kept busy meanwhile (system busy minus the own process).
    pub fn foreign_load(&self) -> Option<f64> {
        let wall = self.seconds();
        let busy = self.ticks.zip(library::platform::info::system_cpu_ticks()).and_then(|(a, b)| library::platform::info::busy_share(a, b))?;
        let own = self.own.zip(library::platform::cpu_seconds()).map(|(a, b)| b - a)?;
        let logical = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        (wall > 0.0).then(|| (busy - own / (wall * logical)).clamp(0.0, 1.0))
    }
}

/// Test hook `GRAINEO_PERF_SLOW=<factor>` (only with `GRAINEO_DATA_DIR`): a measured job takes this much longer (the
/// job waits the difference), to try the automatic choice without a slow computer.
pub fn simulate_slow(seconds: f64) {
    if !test_run() {
        return;
    }
    if let Some(f) = crate::env::var("GRAINEO_PERF_SLOW").ok().and_then(|v| v.parse::<f64>().ok()).filter(|f| *f > 1.0) {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds * (f - 1.0)));
    }
}

/// A measured job finished (worker thread): `seconds` of computing (without waiting at a gate); `skip` names why the
/// job is not comparable (cache hit, part of the work done before). Updates the measured tier.
pub fn record(work: Work, megapixels: f64, seconds: f64, gpu: bool, skip: Option<&'static str>, foreign_load: Option<f64>) {
    if megapixels <= 0.1 || seconds <= 0.0 {
        return;
    }
    let mut s = state();
    let first_gpu = gpu && !std::mem::replace(&mut s.gpu_seen[work.slot()], true);
    let skipped = if skip.is_some() {
        skip
    } else if first_gpu {
        Some("first GPU job of the session")
    } else if foreign_load.is_some_and(|l| l > s.tuning.max_foreign_load) {
        Some("other programs busy")
    } else {
        None
    };
    let factor = seconds / (work.reference(gpu, &s.tuning) * megapixels);
    s.recent.push_back(Sample { work: work.english(), megapixels, seconds, gpu, factor: skipped.is_none().then_some(factor), skipped });
    while s.recent.len() > 12 {
        s.recent.pop_front();
    }
    crate::log(&format!(
        "operating mode: {} {:.1} MP in {:.2} s ({}), factor {:.2}{}",
        work.english(),
        megapixels,
        seconds,
        if gpu { "GPU" } else { "processor" },
        factor,
        skipped.map(|r| format!(", not counted: {r}")).unwrap_or_default()
    ));
    if skipped.is_some() {
        return;
    }
    s.profile.factors.push(factor);
    let keep = s.tuning.enter_window.max(s.tuning.leave_window).max(s.tuning.min_samples).max(10);
    while s.profile.factors.len() > keep {
        s.profile.factors.remove(0);
    }
    s.profile.tier = next_tier(s.profile.tier, &s.profile.factors, &s.tuning);
    save(&s);
    update(&mut s, true);
}

/// Temporary cause from the system state (pure, tests): Low Power Mode, heat, short memory, battery – in this order.
pub fn temporary_cause(on_battery: bool, low_power: bool, thermal: Option<&str>, pressure: library::platform::Pressure) -> Option<Cause> {
    if low_power {
        Some(Cause::LowPower)
    } else if matches!(thermal, Some("serious" | "critical")) {
        Some(Cause::Thermal)
    } else if pressure != library::platform::Pressure::Normal {
        Some(Cause::Memory)
    } else if on_battery {
        Some(Cause::Battery)
    } else {
        None
    }
}

/// The temporary cause to apply: the one seen now, else the last one seen within `hold` (pure, tests).
fn held(seen: Option<Cause>, last: &mut Option<(Cause, std::time::Instant)>, now: std::time::Instant, hold: std::time::Duration) -> Option<Cause> {
    if let Some(c) = seen {
        *last = Some((c, now));
        return Some(c);
    }
    match *last {
        Some((c, t)) if now.duration_since(t) < hold => Some(c),
        _ => {
            *last = None;
            None
        }
    }
}

/// Watches power, heat and memory (background thread, lowest priority): memory every 5 s, power and heat every 30 s.
fn watch_system() {
    library::platform::set_priority(library::platform::Priority::Background);
    let fake = crate::env::var("GRAINEO_PERF_FAKE").ok().filter(|_| test_run()).unwrap_or_default();
    let mut power = (false, false, None::<String>);
    let mut n = 0u64;
    // a temporary cause stays at least this long after it was last seen (memory pressure comes and goes within
    // seconds; the mode must not flip back and forth, each flip would be announced)
    let hold = std::time::Duration::from_secs(60);
    let mut last_seen: Option<(Cause, std::time::Instant)> = None;
    loop {
        if n % 6 == 0 {
            let (p, low, thermal) = library::platform::info::power_state();
            power = (p.source == library::platform::info::PowerSource::Battery, low.unwrap_or(false), thermal);
        }
        n += 1;
        let pressure = library::platform::memory_pressure();
        let battery = power.0 || fake.contains("battery");
        let low = power.1 || fake.contains("lowpower");
        POWER.store(if low { 1 } else if battery { 2 } else { 0 }, Ordering::Relaxed);
        let thermal = if fake.contains("thermal") { Some("serious") } else { power.2.as_deref() };
        let pressure = if fake.contains("pressure") { library::platform::Pressure::Warn } else { pressure };
        let now = std::time::Instant::now();
        let seen = temporary_cause(battery, low, thermal, pressure);
        let cause = held(seen, &mut last_seen, now, hold);
        {
            let mut s = state();
            if s.temporary != cause {
                s.temporary = cause;
                update(&mut s, true);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

/// Lines for the performance report (English).
pub fn report() -> Vec<(String, String)> {
    let st = status();
    let s = state();
    let mut v = vec![
        ("mode chosen".to_string(), st.choice.key().to_string()),
        ("computing now".to_string(), format!("{} ({})", st.tier.key(), st.cause.english())),
        ("hardware start factor".to_string(), format!("{:.2}", st.hardware_factor)),
        ("measured factors".to_string(), if st.factors.is_empty() { "none yet".into() } else { st.factors.iter().map(|f| format!("{f:.2}")).collect::<Vec<_>>().join(" ") }),
        (
            "thresholds".to_string(),
            format!("saving above {:.1} ({} of {}), exact below {:.1} (last {}), provisional", s.tuning.enter, s.tuning.enter_count, s.tuning.enter_window, s.tuning.leave, s.tuning.leave_window),
        ),
    ];
    for r in &s.recent {
        v.push((
            "job".into(),
            format!("{} {:.1} MP {:.2} s {}{}", r.work, r.megapixels, r.seconds, if r.gpu { "GPU" } else { "CPU" }, match (r.factor, r.skipped) {
                (Some(f), _) => format!(", factor {f:.2}"),
                (None, Some(why)) => format!(", not counted ({why})"),
                _ => String::new(),
            }),
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Tuning {
        Tuning::default()
    }

    #[test]
    fn reference_computer_stays_exact() {
        // the measured medians of the reference computer itself give factor 1
        let t = t();
        let merrill = 1.08 / (Work::SigmaBase { quattro: false }.reference(true, &t) * 14.8);
        assert!((merrill - 1.0).abs() < 1e-9);
        assert_eq!(next_tier(None, &[1.0, 1.1, 0.9], &t), Some(Tier::Exact));
        // the hardware start value of an M1 Pro (8 performance cores, arm): exact
        assert_eq!(hardware_factor(Some(8), false, &t), 1.0);
        assert_eq!(measured(&Profile::default(), 1.0, &t).0, Tier::Exact);
    }

    #[test]
    fn slow_intel_starts_saving() {
        // MacBook Pro 2015: i7-4870HQ, 4 cores, x86 → 3.0 ≥ 2.5
        let t = t();
        let f = hardware_factor(Some(4), true, &t);
        assert!(f >= t.enter);
        assert_eq!(measured(&Profile::default(), f, &t), (Tier::Saving, Cause::Hardware { factor: f }));
        // an 8-core x86 desktop starts exact
        assert!(hardware_factor(Some(8), true, &t) < t.enter);
    }

    #[test]
    fn too_few_samples_keep_the_start_value() {
        let t = t();
        assert_eq!(next_tier(None, &[5.0, 5.0], &t), None);
        assert_eq!(next_tier(None, &[5.0, 5.0, 5.0], &t), Some(Tier::Saving));
    }

    #[test]
    fn one_slow_job_does_not_switch() {
        let t = t();
        let mut f = vec![1.0; 6];
        f.push(9.0);
        assert_eq!(next_tier(Some(Tier::Exact), &f, &t), Some(Tier::Exact));
        f.push(9.0);
        assert_eq!(next_tier(Some(Tier::Exact), &f, &t), Some(Tier::Exact));
        // three of the last five
        f.push(9.0);
        assert_eq!(next_tier(Some(Tier::Exact), &f, &t), Some(Tier::Saving));
    }

    #[test]
    fn back_to_exact_only_after_a_long_quiet_run() {
        let t = t();
        let mut f = vec![4.0; 5];
        for _ in 0..7 {
            f.push(1.0);
            assert_eq!(next_tier(Some(Tier::Saving), &f, &t), Some(Tier::Saving));
        }
        f.push(1.0);
        assert_eq!(next_tier(Some(Tier::Saving), &f, &t), Some(Tier::Exact));
        // in between (factor 2: neither fast nor slow) nothing changes either way
        let mid = vec![2.0; 10];
        assert_eq!(next_tier(Some(Tier::Saving), &mid, &t), Some(Tier::Saving));
        assert_eq!(next_tier(Some(Tier::Exact), &mid, &t), Some(Tier::Exact));
    }

    #[test]
    fn temporary_causes() {
        use library::platform::Pressure;
        assert_eq!(temporary_cause(false, false, Some("nominal"), Pressure::Normal), None);
        assert_eq!(temporary_cause(true, false, Some("nominal"), Pressure::Normal), Some(Cause::Battery));
        assert_eq!(temporary_cause(true, true, Some("nominal"), Pressure::Normal), Some(Cause::LowPower));
        assert_eq!(temporary_cause(false, false, Some("serious"), Pressure::Normal), Some(Cause::Thermal));
        assert_eq!(temporary_cause(false, false, Some("fair"), Pressure::Warn), Some(Cause::Memory));
    }

    #[test]
    fn temporary_cause_is_held_for_a_while() {
        let t0 = std::time::Instant::now();
        let hold = std::time::Duration::from_secs(60);
        let mut last = None;
        assert_eq!(held(Some(Cause::Memory), &mut last, t0, hold), Some(Cause::Memory));
        // gone after 5 s: still held
        assert_eq!(held(None, &mut last, t0 + std::time::Duration::from_secs(5), hold), Some(Cause::Memory));
        // gone for longer than the hold: ends
        assert_eq!(held(None, &mut last, t0 + std::time::Duration::from_secs(61), hold), None);
        assert_eq!(held(None, &mut last, t0 + std::time::Duration::from_secs(62), hold), None);
    }

    #[test]
    fn choice_wins_over_measurement_and_system_state() {
        let mut s = State { tuning: t(), hardware_factor: 1.0, ..Default::default() };
        s.temporary = Some(Cause::Battery);
        assert_eq!(effective(&s), (Tier::Saving, Cause::Battery));
        s.choice = Some(Choice::Exact);
        assert_eq!(effective(&s), (Tier::Exact, Cause::Chosen));
        s.choice = Some(Choice::Saving);
        s.temporary = None;
        assert_eq!(effective(&s), (Tier::Saving, Cause::Chosen));
    }

    #[test]
    fn plans_differ_only_in_timing_and_memory() {
        let t = t();
        let e = plan_for(Tier::Exact, false, &t);
        let s = plan_for(Tier::Saving, false, &t);
        assert!(e.early_base && e.base_ahead && e.neighbours_behind && e.cache_limit == 4);
        assert!(!s.early_base && !s.base_ahead && !s.neighbours_behind && s.cache_limit == 2);
        assert!(s.base_dwell > e.base_dwell && s.full_dwell > e.full_dwell);
        // small memory: fewer photos kept, also when exact
        assert_eq!(plan_for(Tier::Exact, true, &t).cache_limit, 2);
    }

    #[test]
    fn tuning_overrides_single_values() {
        let t: Tuning = serde_json::from_value(json!({"enter": 3.5})).unwrap();
        assert_eq!(t.enter, 3.5);
        assert_eq!(t.leave, Tuning::default().leave);
        let p: Profile = serde_json::from_value(json!({"factors": [1.0, 2.0], "tier": "saving"})).unwrap();
        assert_eq!(p.tier, Some(Tier::Saving));
    }
}
