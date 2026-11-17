//! Text and JSON of the performance report (see `perf_measure`): an English, readable text file (system, a table per
//! photo and stage with cold/warm/median/min/max, notes, the last lines of the timing log) and a JSON file beside it
//! for evaluation. Both are anonymised: no user or computer name, no path, no file name.

use crate::perf_measure::{Photo, Report};

/// Statistics of the runs of one stage (milliseconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// first run
    pub cold: f64,
    /// second run
    pub warm: Option<f64>,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub runs: usize,
}

/// Statistics of a series; an empty series gives NaN everywhere.
pub fn stats(v: &[f64]) -> Stats {
    if v.is_empty() {
        return Stats { cold: f64::NAN, warm: None, median: f64::NAN, min: f64::NAN, max: f64::NAN, runs: 0 };
    }
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    s.sort_by(f64::total_cmp);
    let median = if s.is_empty() {
        f64::NAN
    } else if s.len() % 2 == 1 {
        s[s.len() / 2]
    } else {
        (s[s.len() / 2 - 1] + s[s.len() / 2]) / 2.0
    };
    Stats { cold: v[0], warm: v.get(1).copied(), median, min: s.first().copied().unwrap_or(f64::NAN), max: s.last().copied().unwrap_or(f64::NAN), runs: v.len() }
}

/// Milliseconds for the table: "1234", "12.3", "0.42", "-".
pub fn ms(v: f64) -> String {
    if !v.is_finite() {
        "-".into()
    } else if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// "16.0 GB", "512 MB".
pub fn bytes(b: u64) -> String {
    let gb = b as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", b as f64 / 1_048_576.0)
    }
}

/// What must never be in a report: the user's home folder, user and computer names, the measured photos' paths,
/// folder and file names. A last safety net – the report is built without them in the first place – and the
/// cleaner of error messages (which may name a file).
#[derive(Clone, Default)]
pub struct Privacy {
    /// whole paths (photos, their folders, home), longest first
    paths: Vec<String>,
    /// names replaced as whole words (user, computer, file names and stems), longest first
    words: Vec<String>,
    redact: crate::bug_report::Redact,
}

impl Privacy {
    /// From the photos picked (files or the files of a folder) and this computer's names.
    pub fn new(photos: &[std::path::PathBuf], folder: Option<&std::path::Path>) -> Privacy {
        let home = sysdirs::home().map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string()).unwrap_or_default();
        let mut own: Vec<String> = Vec::new();
        if let Some(u) = std::path::Path::new(&home).file_name() {
            own.push(u.to_string_lossy().into_owned());
        }
        for v in ["USER", "USERNAME", "COMPUTERNAME", "HOSTNAME"] {
            if let Ok(u) = std::env::var(v) {
                own.push(u);
            }
        }
        Privacy::with(&home, own, photos, folder)
    }

    /// Testable form: `home`, own names and the photos.
    pub fn with(home: &str, own: Vec<String>, photos: &[std::path::PathBuf], folder: Option<&std::path::Path>) -> Privacy {
        let mut paths: Vec<String> = Vec::new();
        let mut words = own.clone();
        for p in photos.iter().map(|p| p.as_path()).chain(folder) {
            let s = p.to_string_lossy().to_string();
            paths.push(s.clone());
            paths.push(s.replace('\\', "/"));
            if let Some(parent) = p.parent() {
                let ps = parent.to_string_lossy().to_string();
                if ps.len() > 1 {
                    paths.push(ps);
                }
            }
            if let Some(n) = p.file_name() {
                words.push(n.to_string_lossy().into_owned());
            }
            if let Some(n) = p.file_stem() {
                words.push(n.to_string_lossy().into_owned());
            }
        }
        if home.len() > 1 {
            paths.push(home.to_string());
        }
        paths.retain(|p| p.len() > 1);
        paths.sort_by_key(|p| std::cmp::Reverse(p.len()));
        paths.dedup();
        words.retain(|w| w.chars().count() >= 3);
        words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
        words.dedup();
        Privacy { paths, words, redact: crate::bug_report::Redact { home: home.to_string(), own, names: Vec::new() } }
    }

    /// An error message or note from the development code: paths and file names out.
    pub fn message(&self, m: &str) -> String {
        let mut s = m.to_string();
        for p in &self.paths {
            s = s.replace(p.as_str(), "‹path›");
        }
        self.scrub(&self.redact.line(&s))
    }

    /// A free value (drive, switches, log lines …): every path and name replaced.
    pub fn scrub(&self, t: &str) -> String {
        let mut s = self.scrub_paths(t);
        for w in &self.words {
            s = replace_word(&s, w, "‹…›");
        }
        s
    }

    /// A value of fixed vocabulary (camera and family names, stage labels, processor) or the whole report as a last
    /// safety net: only paths replaced. Names are replaced in the free values only – as whole words over the whole
    /// text they hit keys and fixed words too (a folder "SIGMA" or user "max" turned "SIGMA DP2 Merrill" and `max_ms`
    /// into "‹…›").
    pub fn scrub_paths(&self, t: &str) -> String {
        let mut s = t.to_string();
        for p in &self.paths {
            s = s.replace(p.as_str(), "‹path›");
        }
        s
    }

    /// Every string value of a JSON document scrubbed, keys never: values under `fixed` keys only of paths
    /// ([`Self::scrub_paths`]), all others of paths and names ([`Self::scrub`]).
    fn scrub_json(&self, v: &mut serde_json::Value, fixed: bool) {
        match v {
            serde_json::Value::String(t) => *t = if fixed { self.scrub_paths(t) } else { self.scrub(t) },
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| self.scrub_json(x, fixed)),
            serde_json::Value::Object(o) => o.iter_mut().for_each(|(k, x)| self.scrub_json(x, JSON_FIXED.contains(&k.as_str()))),
            _ => {}
        }
    }
}

/// Keys of the JSON report whose values are fixed vocabulary (no names of the user's files can be in them).
const JSON_FIXED: &[&str] = &["format", "version", "target", "created", "os", "model", "cpu", "power_source", "thermal", "name", "backend", "device_type", "driver", "driver_info", "used_for_other_formats", "foveon", "pool", "family", "camera", "id", "label", "path"];

/// Replace `word` where it stands as a whole word (case as given; names are compared exactly).
fn replace_word(s: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(pos) = s[i..].find(word) {
        let a = i + pos;
        let b = a + word.len();
        let before = s[..a].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = s[b..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        out.push_str(&s[i..a]);
        out.push_str(if before && after { with } else { &s[a..b] });
        i = b;
    }
    out.push_str(&s[i..]);
    out
}

fn row(t: &mut String, k: &str, v: &str) {
    t.push_str(&format!("  {k:<22} {v}\n"));
}

fn system_rows(t: &mut String, r: &Report, privacy: &Privacy) {
    let s = &r.system_before;
    row(t, "Operating system", &s.os);
    if let Some(m) = &s.model {
        row(t, "Computer model", m);
    }
    let mut cpu = s.cpu.clone();
    if let Some(p) = s.physical_cores {
        cpu.push_str(&format!(" · {p} cores"));
        if let Some((perf, eff)) = s.core_kinds {
            cpu.push_str(&format!(" ({perf} performance, {eff} efficiency)"));
        }
    }
    cpu.push_str(&format!(" · {} logical", s.logical_cores));
    row(t, "Processor", &cpu);
    let mem = match (s.memory_total, s.memory_available) {
        (Some(a), Some(f)) => format!("{} total · {} available at start", bytes(a), bytes(f)),
        (Some(a), None) => format!("{} total", bytes(a)),
        _ => "unknown".into(),
    };
    row(t, "Memory", &mem);
    let g = &r.graphics;
    if g.adapters.is_empty() {
        row(t, "Graphics", "no adapter found");
    }
    for (i, a) in g.adapters.iter().enumerate() {
        let used = g.raw_in_use.as_deref() == Some(a.name.as_str());
        let driver = [a.driver.as_str(), a.driver_info.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join(" ");
        let v = format!("{} · {} · {}{}{}", a.name, a.backend, a.device_type, if driver.is_empty() { String::new() } else { format!(" · driver {driver}") }, if used { " [used]" } else { "" });
        row(t, if i == 0 { "Graphics" } else { "" }, &v);
    }
    row(t, "Foveon GPU", &g.foveon);
    row(t, "GPU setting", if g.setting_on { "on" } else { "off" });
    let d = &r.display;
    let mon = d.monitor.map(|m| format!(" · monitor {:.0} × {:.0} points ({:.0} × {:.0} pixels)", m[0], m[1], m[0] * d.scale, m[1] * d.scale)).unwrap_or_default();
    row(t, "Display", &format!("scale {:.2}{mon} · window {:.0} × {:.0} points", d.scale, d.window[0], d.window[1]));
    let power = |s: &library::platform::info::SystemInfo| {
        let src = match s.power.source {
            library::platform::info::PowerSource::Ac => "mains power",
            library::platform::info::PowerSource::Battery => "battery",
            library::platform::info::PowerSource::Unknown => "unknown",
        };
        let bat = s.power.battery_percent.map(|p| format!(" · battery {p} %")).unwrap_or_default();
        format!("{src}{bat}")
    };
    row(t, "Power", &format!("{} (at the end: {})", power(s), power(&r.system_after)));
    let lpm = |v: Option<bool>| v.map_or("not readable", |b| if b { "on" } else { "off" });
    row(t, "Power saving mode", &format!("{} (at the end: {})", lpm(s.low_power_mode), lpm(r.system_after.low_power_mode)));
    let th = |v: &Option<String>| v.clone().unwrap_or_else(|| "not readable".into());
    row(t, "Thermal state", &format!("{} (at the end: {})", th(&s.thermal), th(&r.system_after.thermal)));
    let load = s.load_average.map(|l| format!("load {:.1} / {:.1} / {:.1} · ", l[0], l[1], l[2])).unwrap_or_default();
    let busy = r.busy_before.map_or("-".into(), |b| format!("{:.0} %", b * 100.0));
    row(t, "System load at start", &format!("{load}processor busy {busy} (1 s before the measurement)"));
    row(t, "App caches on", &privacy.scrub(&r.caches_drive));
    let th = r.threads.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ");
    row(t, "Thread pools", &th);
    for (k, v) in &r.mode {
        row(t, &format!("Operating mode: {k}"), &privacy.scrub(v));
    }
    row(t, "Switches", &if r.switches.is_empty() { "none".to_string() } else { privacy.scrub(&r.switches.join(", ")) });
}

fn photo_head(p: &Photo, privacy: &Privacy) -> String {
    let mut parts = vec![p.family.clone()];
    if !p.camera.is_empty() {
        parts.push(p.camera.clone());
    }
    if !p.format.is_empty() {
        parts.push(p.format.clone());
    }
    if p.size[0] > 0 {
        parts.push(format!("{} × {} ({:.1} MP)", p.size[0], p.size[1], (p.size[0] * p.size[1]) as f64 / 1e6));
    }
    if let Some(i) = p.iso {
        parts.push(format!("ISO {i}"));
    }
    parts.push(bytes(p.bytes));
    parts.push(format!("on {}", privacy.scrub(&p.drive)));
    format!("Photo {}: {}", p.index, parts.join(" · "))
}

/// The text file.
pub fn text(r: &Report, privacy: &Privacy) -> String {
    let mut t = String::new();
    t.push_str("graineo performance report\n==========================\n\n");
    t.push_str(&format!("Created {} · graineo {} ({}) · measurement took {:.0} s{}\n", r.created, r.version, r.target, r.seconds, if r.cancelled { " · CANCELLED" } else { "" }));
    t.push_str("This file stays on this computer until you send it yourself. It contains no user, computer or file\nnames and no paths.\n\n");
    t.push_str("SYSTEM\n");
    system_rows(&mut t, r, privacy);
    t.push_str("\nPHOTOS\n");
    for p in &r.photos {
        t.push_str(&format!("  {}\n", photo_head(p, privacy)));
    }
    if r.photos.is_empty() {
        t.push_str("  none\n");
    }
    t.push_str(&format!(
        "\nTIMES in milliseconds · {} runs per stage: cold = 1st run, warm = 2nd run, median/min/max over all runs ·\ncores = processor time of the app / wall time (average cores busy) · path GPU→CPU = asked for the GPU, ran on the\nprocessor\n",
        crate::perf_measure::RUNS
    ));
    for p in &r.photos {
        t.push_str(&format!("\n{}\n", photo_head(p, privacy)));
        t.push_str(&format!("  {:<58} {:<8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}\n", "Stage", "Path", "Cold", "Warm", "Median", "Min", "Max", "Cores"));
        for s in &p.stages {
            let st = stats(&s.ms);
            let cores = if s.cores.is_empty() { "-".to_string() } else { format!("{:.1}", stats(&s.cores).median) };
            t.push_str(&format!("  {:<58} {:<8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}\n", s.label, s.path, ms(st.cold), st.warm.map_or("-".into(), ms), ms(st.median), ms(st.min), ms(st.max), cores));
        }
        // sums the user feels: first picture and exact view
        let sum = |ids: &[&str]| -> Option<f64> {
            let v: Vec<f64> = ids.iter().filter_map(|id| p.stages.iter().find(|s| s.id == *id).map(|s| stats(&s.ms).median)).collect();
            (v.len() == ids.len()).then(|| v.iter().sum())
        };
        let first = sum(&["open", "first_linear", "first_develop", "first_render"]).or_else(|| sum(&["open", "first_detail", "first_render"]));
        if let Some(f) = first {
            t.push_str(&format!("  {:<58} {:<8} {:>8}\n", "= until the first picture (median sum)", "", ms(f)));
        }
        for (id, name) in [("base_gpu", "= until the exact fitted view, GPU (median sum)"), ("base_cpu", "= until the exact fitted view, processor (median sum)")] {
            if let (Some(f), Some(b), Some(v)) = (first, sum(&[id]), sum(&["fitted"])) {
                t.push_str(&format!("  {:<58} {:<8} {:>8}\n", name, "", ms(f + b + v)));
            }
        }
        if !p.details.is_empty() {
            t.push_str("  finer stages reported by the development code (median):\n");
            for (k, v) in &p.details {
                t.push_str(&format!("    {:<72} {:>8}\n", privacy.scrub(k), ms(stats(v).median)));
            }
        }
        let mut res = Vec::new();
        if let Some(m) = p.peak_memory {
            res.push(format!("peak memory of the app {}", bytes(m)));
        }
        if let Some(n) = p.max_threads {
            res.push(format!("threads up to {n}"));
        }
        if let Some(b) = p.system_busy {
            res.push(format!("whole processor busy {:.0} %", b * 100.0));
        }
        res.push(format!("{:.0} s for this photo", p.seconds));
        t.push_str(&format!("  resources: {}\n", res.join(" · ")));
        if let Some(e) = &p.error {
            t.push_str(&format!("  ERROR: {}\n", privacy.message(e)));
        }
        for n in &p.notes {
            t.push_str(&format!("  note: {}\n", privacy.message(n)));
        }
    }
    if !r.notes.is_empty() {
        t.push_str("\nNOTES\n");
        for n in &r.notes {
            t.push_str(&format!("  - {}\n", privacy.message(n)));
        }
    }
    t.push_str(&format!("\nTIMING LOG (last {} lines of normal use{})\n", r.timing_log.len(), if r.timing_log_on { "" } else { "; the log is switched off in the settings" }));
    for l in &r.timing_log {
        t.push_str(&format!("  {}\n", privacy.scrub(l)));
    }
    if r.timing_log.is_empty() {
        t.push_str("  none yet\n");
    }
    privacy.scrub_paths(&t)
}

/// The JSON file (same content, machine-readable).
pub fn json(r: &Report, privacy: &Privacy) -> String {
    use serde_json::json;
    let s = &r.system_before;
    let sys = |s: &library::platform::info::SystemInfo| {
        json!({
            "os": s.os, "model": s.model, "cpu": s.cpu, "physical_cores": s.physical_cores, "logical_cores": s.logical_cores,
            "performance_efficiency_cores": s.core_kinds.map(|(a, b)| [a, b]), "memory_total": s.memory_total, "memory_available": s.memory_available,
            "power_source": format!("{:?}", s.power.source), "battery_percent": s.power.battery_percent, "low_power_mode": s.low_power_mode,
            "thermal": s.thermal, "load_average": s.load_average,
        })
    };
    let photos: Vec<serde_json::Value> = r
        .photos
        .iter()
        .map(|p| {
            let stages: Vec<serde_json::Value> = p
                .stages
                .iter()
                .map(|st| {
                    let x = stats(&st.ms);
                    json!({"id": st.id, "label": st.label, "path": st.path, "ms": st.ms, "cores": st.cores, "cold_ms": x.cold, "warm_ms": x.warm, "median_ms": x.median, "min_ms": x.min, "max_ms": x.max})
                })
                .collect();
            json!({
                "index": p.index, "family": p.family, "camera": p.camera, "format": p.format, "size": p.size, "iso": p.iso, "bytes": p.bytes, "drive": p.drive,
                "stages": stages, "details": p.details.iter().map(|(k, v)| json!({"stage": k, "ms": v, "median_ms": stats(v).median})).collect::<Vec<_>>(),
                "notes": p.notes.iter().map(|n| privacy.message(n)).collect::<Vec<_>>(), "error": p.error.as_ref().map(|e| privacy.message(e)),
                "peak_memory": p.peak_memory, "max_threads": p.max_threads, "system_busy": p.system_busy, "seconds": p.seconds,
            })
        })
        .collect();
    let g = &r.graphics;
    let doc = json!({
        "format": "graineo-performance-report", "format_version": 1, "created": r.created, "version": r.version, "target": r.target,
        "runs_per_stage": crate::perf_measure::RUNS, "seconds": r.seconds, "cancelled": r.cancelled,
        "system": sys(s), "system_at_end": sys(&r.system_after), "busy_before": r.busy_before,
        "display": {"scale": r.display.scale, "monitor_points": r.display.monitor, "window_points": r.display.window},
        "graphics": {
            "adapters": g.adapters.iter().map(|a| json!({"name": a.name, "backend": a.backend, "device_type": a.device_type, "driver": a.driver, "driver_info": a.driver_info})).collect::<Vec<_>>(),
            "used_for_other_formats": g.raw_in_use, "foveon": g.foveon,
            "setting_on": g.setting_on, "available": g.available,
        },
        "threads": r.threads.iter().map(|(k, n)| json!({"pool": k, "threads": n})).collect::<Vec<_>>(),
        "operating_mode": r.mode.iter().map(|(k, v)| json!({"item": k, "value": privacy.scrub(v)})).collect::<Vec<_>>(),
        "switches": r.switches, "caches_drive": r.caches_drive, "photos": photos,
        "notes": r.notes.iter().map(|n| privacy.message(n)).collect::<Vec<_>>(),
        "timing_log_on": r.timing_log_on, "timing_log": r.timing_log,
    });
    let mut doc = doc;
    privacy.scrub_json(&mut doc, false);
    privacy.scrub_paths(&serde_json::to_string_pretty(&doc).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf_measure::{Photo, Stage};

    #[test]
    fn median_min_max_cold_warm() {
        let s = stats(&[900.0, 300.0, 310.0]);
        assert_eq!((s.cold, s.warm, s.median, s.min, s.max, s.runs), (900.0, Some(300.0), 310.0, 300.0, 900.0, 3));
        let s = stats(&[4.0, 2.0]);
        assert_eq!((s.median, s.warm), (3.0, Some(2.0)));
        let s = stats(&[5.0]);
        assert_eq!((s.median, s.warm), (5.0, None));
        assert!(stats(&[]).median.is_nan());
        assert_eq!(ms(1234.4), "1234");
        assert_eq!(ms(12.34), "12.3");
        assert_eq!(ms(0.4249), "0.42");
        assert_eq!(ms(f64::NAN), "-");
        assert_eq!(bytes(16 * 1_073_741_824), "16.0 GB");
        assert_eq!(bytes(512 * 1_048_576), "512 MB");
    }

    fn sample(privacy_paths: &[std::path::PathBuf]) -> Report {
        let stage = |id: &str, label: &str, path: &str, ms: Vec<f64>| Stage { id: id.into(), label: label.into(), path: path.into(), cores: vec![3.5; ms.len()], ms };
        let p = Photo {
            index: 1,
            family: "Foveon Merrill".into(),
            camera: "SIGMA DP2 Merrill".into(),
            format: "X3F".into(),
            size: [4704, 3136],
            iso: Some(100),
            bytes: 48_000_000,
            drive: "SSD, internal".into(),
            stages: vec![
                stage("read", "read file", "disk", vec![14.0, 5.0, 5.2]),
                stage("open", "open (app path)", "CPU", vec![180.0, 140.0, 150.0]),
                stage("first_linear", "first picture: linearise to 2400 px", "CPU", vec![90.0, 80.0, 85.0]),
                stage("first_develop", "first picture: own pipeline (noise reduction)", "CPU", vec![320.0, 300.0, 310.0]),
                stage("first_render", "first picture: output", "CPU", vec![20.0, 15.0, 16.0]),
                stage("base_gpu", "exact base, GPU", "GPU→CPU", vec![2900.0, 2100.0, 2000.0]),
                stage("base_cpu", "exact base, processor", "CPU", vec![2500.0, 2400.0, 2450.0]),
                stage("fitted", "fitted view from the exact base (2400 px + output)", "CPU", vec![140.0, 120.0, 125.0]),
            ],
            // an error message from the development code naming the file
            error: Some(format!("{}: Bild nicht lesbar", privacy_paths[0].display())),
            notes: vec!["exact base, GPU: asked for the GPU, computed on the processor.".into()],
            ..Default::default()
        };
        Report {
            version: "1.2.0".into(),
            target: "macos aarch64".into(),
            created: "2026-11-16 15:30 (UTC+01:00)".into(),
            system_before: library::platform::info::SystemInfo { os: "macOS 26.5".into(), cpu: "Apple M1 Pro".into(), logical_cores: 10, memory_total: Some(16 << 30), ..Default::default() },
            photos: vec![p],
            switches: vec![crate::perf_measure::switch_text("GRAINEO_DATA_DIR", &privacy_paths[0].to_string_lossy())],
            timing_log: vec!["2026-11-16T09:05:07+01:00 v=1.2.0 family=Merrill iso=<=200 first_ms=432 exact_ms=1290 path=gpu system_cpu=35% app_mem_mb=1200".into()],
            ..Default::default()
        }
    }

    /// Paths, folder, file and user names never reach the report, also not through error messages (macOS and
    /// Windows paths).
    #[test]
    fn report_has_no_paths_or_names() {
        for (home, photo, folder) in [
            ("/Users/x", "/Users/x/Pictures/Urlaub Kreta/DP2M5418.X3F", "/Users/x/Pictures/Urlaub Kreta"),
            (r"C:\Users\x", r"C:\Users\x\Pictures\Urlaub Kreta\DP2M5418.X3F", r"C:\Users\x\Pictures\Urlaub Kreta"),
        ] {
            let photos = vec![std::path::PathBuf::from(photo)];
            let privacy = Privacy::with(home, vec!["x".into(), "Annas-MacBook".into()], &photos, Some(std::path::Path::new(folder)));
            let r = sample(&photos);
            for out in [text(&r, &privacy), json(&r, &privacy)] {
                for bad in ["Users", "Urlaub", "Kreta", "DP2M5418", "Pictures", "Annas-MacBook", "C:\\", "/Users"] {
                    assert!(!out.contains(bad), "{bad} in\n{out}");
                }
                assert!(out.contains("SIGMA DP2 Merrill") && out.contains("exact base, GPU"));
            }
        }
    }

    /// Names are replaced in values only: a folder "SIGMA" or user "max" leaves camera and family names, table heads
    /// and JSON keys whole, while the photo's own name still never reaches the report.
    #[test]
    fn names_do_not_damage_keys_or_camera_names() {
        let photos = vec![std::path::PathBuf::from("/Users/x/SIGMA/DP2M5418.X3F")];
        let privacy = Privacy::with("/Users/x", vec!["max".into()], &photos, Some(std::path::Path::new("/Users/x/SIGMA")));
        let mut r = sample(&photos);
        r.timing_log.push("note for max about SIGMA".into());
        let t = text(&r, &privacy);
        let j = json(&r, &privacy);
        assert!(t.contains("SIGMA DP2 Merrill") && t.contains("median/min/max") && t.contains("Max"), "{t}");
        assert!(j.contains("\"max_ms\"") && j.contains("SIGMA DP2 Merrill"), "{j}");
        assert!(serde_json::from_str::<serde_json::Value>(&j).is_ok());
        for out in [&t, &j] {
            assert!(!out.contains("DP2M5418") && !out.contains("/Users") && !out.contains("note for max"), "{out}");
        }
    }

    #[test]
    fn text_has_table_and_sums() {
        let photos = vec![std::path::PathBuf::from("/Users/x/a/DP2M5418.X3F")];
        let t = text(&sample(&photos), &Privacy::with("/Users/x", vec![], &photos, None));
        assert!(t.contains("Photo 1: Foveon Merrill · SIGMA DP2 Merrill · X3F · 4704 × 3136 (14.8 MP) · ISO 100"), "{t}");
        // cold, warm, median, min, max of the GPU base
        assert!(t.lines().any(|l| l.contains("exact base, GPU") && l.contains("GPU→CPU") && l.contains("2900") && l.contains("2100") && l.contains("2000") && l.contains("3.5")), "{t}");
        // first picture = 150 + 85 + 310 + 16
        assert!(t.lines().any(|l| l.contains("until the first picture") && l.trim_end().ends_with("561")), "{t}");
        // exact view on the processor = 561 + 2450 + 125
        assert!(t.lines().any(|l| l.contains("exact fitted view, processor") && l.trim_end().ends_with("3136")), "{t}");
        assert!(t.contains("TIMING LOG (last 1 lines") && t.contains("family=Merrill"));
        assert!(t.contains("GRAINEO_DATA_DIR (set)"));
    }
}
