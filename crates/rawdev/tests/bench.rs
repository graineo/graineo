//! Comparison measurement CPU ↔ GPU, full resolution, several files: alternating (A/B/C/A/B/C …), so that
//! machine load affects all variants equally. Per file and variant: one warm-up run (shader compilation,
//! file cache), then N runs (default 7); output median (minimum – maximum) in ms as a Markdown table.
//!
//! Usage: `GRAINEO_BENCH_FILES=a.CR3,b.DNG cargo test -p rawdev --release --test bench -- --ignored --nocapture`
//! (`GRAINEO_BENCH_RUNS=7`). Each run reopens the image (nothing cached): open = `RawDoc::open` (decoding +
//! 2.8 MP preview), then `render_full` (base, detail stage, output stage). "Total" = open + `render_full`,
//! measured with one clock around both calls; the stages come from the collector ([`rawdev::timings`]) in
//! extra runs with stage timing (the GPU is then submitted after each stage – a bit slower than
//! the real run). "Rest" = total − open − sum of stages (copying, allocation, not measured individually).

use std::collections::BTreeMap;
use std::time::Instant;

struct Variant {
    name: &'static str,
    /// switch "Grafikkarte verwenden" for this variant. `GRAINEO_GPU` cannot do that: `pools::gpu_allowed` reads it only
    /// once per process, so before this switch the "CPU" column actually computed on the GPU.
    gpu: bool,
    /// (variable, value) – None removes it
    env: &'static [(&'static str, Option<&'static str>)],
}

const VARIANTS: &[Variant] = &[
    Variant { name: "CPU", gpu: false, env: &[("GRAINEO_GPU_FUSION", None), ("GRAINEO_GPU_DEMOSAIC", None)] },
    Variant { name: "GPU 0.8.1", gpu: true, env: &[("GRAINEO_GPU_FUSION", Some("0")), ("GRAINEO_GPU_DEMOSAIC", Some("0"))] },
    Variant { name: "GPU new", gpu: true, env: &[("GRAINEO_GPU_FUSION", None), ("GRAINEO_GPU_DEMOSAIC", None)] },
];

fn apply_variant(v: &Variant) {
    pools::set_gpu_allowed(v.gpu);
    for (k, w) in v.env {
        unsafe {
            match w {
                Some(w) => std::env::set_var(k, w),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn last() -> String {
    let mut a = [0f64; 3];
    let n = unsafe { libc_getloadavg(a.as_mut_ptr(), 3) };
    if n >= 1 {
        format!("{:.1}/{:.1}/{:.1}", a[0], a[1], a[2]).replace('.', ",")
    } else {
        "?".into()
    }
}

extern "C" {
    #[link_name = "getloadavg"]
    fn libc_getloadavg(a: *mut f64, n: i32) -> i32;
}

#[derive(Default)]
struct Run {
    total: f64,
    open_ms: f64,
    preview: f64,
    stages: Vec<(String, f64)>,
}

fn single_run(p: &std::path::Path, s: &develop::Settings, stages: bool) -> Run {
    rawdev::gpu::set_stage_timing(stages);
    let t = Instant::now();
    let doc = rawdev::RawDoc::open(p, &|| true).unwrap().unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    // 2.8 MP preview (detail stage of the preview + output) – separate from the full-resolution stages
    let t = Instant::now();
    let _v = doc.render_preview(s);
    let preview = t.elapsed().as_secs_f64() * 1000.0;
    rawdev::timings::collect(true);
    let t = Instant::now();
    let _img = doc.render_full(s).unwrap();
    let full = t.elapsed().as_secs_f64() * 1000.0;
    let stage_list = rawdev::timings::take_all();
    rawdev::timings::collect(false);
    Run { total: open_ms + full, open_ms, preview, stages: stage_list }
}

fn stat(v: &mut Vec<f64>) -> String {
    if v.is_empty() {
        return "–".into();
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = v[v.len() / 2];
    format!("{:.0} ({:.0}–{:.0})", med, v[0], v[v.len() - 1])
}

/// stage name → table row
fn table_row(n: &str) -> (usize, &'static str) {
    match n {
        "CPU normieren+WB" => (10, "Base: normalize + white balance (CPU)"),
        "CPU Rauschmodell" => (11, "Base: noise model (CPU)"),
        "CPU Lichter" => (12, "Base: reconstruct highlights (CPU)"),
        "CPU Demosaik" => (13, "Base: demosaic (CPU)"),
        "GPU Demosaik" => (14, "Base: demosaic (GPU)"),
        "CPU Rauschminderung" | "GPU Rauschminderung" => (20, "Noise reduction"),
        "CPU Schärfe" | "GPU Schärfe" => (21, "Base sharpening"),
        "CPU Objektiv" | "GPU Objektiv" => (22, "Lens correction"),
        "CPU Helligkeitskarte" | "GPU Helligkeitskarte" => (30, "Luminance map (shadows/highlights)"),
        "CPU Ausgabe (Ton/Farbe)" | "GPU Ausgabe" => (31, "Output (matrix, tone, color, look table)"),
        "Look-Stufe (CPU)" => (32, "Look stage (curve/HSL/…, CPU)"),
        "CPU Falschfarben" | "GPU Falschfarben gesamt" => (33, "False color suppression (X-Trans)"),
        "GPU Hochladen" => (40, "GPU: upload"),
        "GPU Zurücklesen" => (41, "GPU: readback"),
        "GPU Rechnen (Detail + Ausgabe, ein Auftrag)" | "GPU Rechnen (Detail, ein Auftrag)" => (42, "GPU: compute as one job (not measurable per stage)"),
        _ => (99, "other"),
    }
}

#[test]
#[ignore]
fn compare() {
    let Ok(files) = std::env::var("GRAINEO_BENCH_FILES") else { return };
    let n: usize = std::env::var("GRAINEO_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(7);
    let g = rawdev::gpu::gpu().map(|g| g.name.clone()).unwrap_or_else(|| "no GPU".into());
    println!("GPU: {g}; runs per variant: {n} (after one warm-up run); the variant changes in every round\n");
    for file in files.split(',') {
        let p = std::path::PathBuf::from(file);
        let mut s = develop::Settings::default();
        s.raw.lens = true;
        // noise reduction: luminance 30 (color 25 is the default); `GRAINEO_BENCH_NR_LUMA=0` for the default
        s.raw.nr_luma = std::env::var("GRAINEO_BENCH_NR_LUMA").ok().and_then(|v| v.parse().ok()).unwrap_or(30.0);
        let last_before = last();
        let t_file = Instant::now();
        // warm-up
        for v in VARIANTS {
            apply_variant(v);
            let _ = single_run(&p, &s, false);
        }
        let mut raw_runs: Vec<Vec<Run>> = (0..VARIANTS.len()).map(|_| Vec::new()).collect();
        let mut raw_stage_runs: Vec<Vec<Run>> = (0..VARIANTS.len()).map(|_| Vec::new()).collect();
        for _ in 0..n {
            for (i, v) in VARIANTS.iter().enumerate() {
                apply_variant(v);
                raw_runs[i].push(single_run(&p, &s, false));
                raw_stage_runs[i].push(single_run(&p, &s, true));
            }
        }
        for v in VARIANTS {
            for (k, _) in v.env {
                unsafe { std::env::remove_var(k) };
            }
        }
        pools::set_gpu_allowed(true);
        let info = {
            let d = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
            format!("{} {} · {}×{} ({:.1} MP)", d.info.maker, d.info.model, d.full_size[0], d.full_size[1], (d.full_size[0] * d.full_size[1]) as f64 / 1e6).replace('.', ",")
        };
        println!("### {info} ({})", p.file_name().unwrap().to_string_lossy());
        println!("load (1/5/15 min) before the runs {} · after the runs {} · {:.0} s\n", last_before, last(), t_file.elapsed().as_secs_f64());
        print!("| Stage (ms: median (min–max)) |");
        for v in VARIANTS {
            print!(" {} |", v.name);
        }
        println!();
        print!("|---|");
        for _ in VARIANTS {
            print!("---|");
        }
        println!();
        // stages from the stage runs, per variant and row (multiple entries of one run added up)
        let mut rows: BTreeMap<(usize, &'static str), Vec<Vec<f64>>> = BTreeMap::new();
        for (i, l) in raw_stage_runs.iter().enumerate() {
            for run in l {
                let mut per_row: BTreeMap<(usize, &'static str), f64> = BTreeMap::new();
                for (nm, ms) in &run.stages {
                    *per_row.entry(table_row(nm)).or_default() += ms;
                }
                for (k, ms) in per_row {
                    rows.entry(k).or_insert_with(|| vec![Vec::new(); VARIANTS.len()])[i].push(ms);
                }
            }
        }
        let row = |name: &str, vals: Vec<Vec<f64>>| {
            let mut vals = vals;
            print!("| {name} |");
            for v in vals.iter_mut() {
                print!(" {} |", stat(v));
            }
            println!();
        };
        row("Open (decoding + 2.8 MP preview base)", (0..VARIANTS.len()).map(|i| raw_stage_runs[i].iter().map(|l| l.open_ms).collect()).collect());
        for ((_, name), v) in rows {
            row(name, v);
        }
        // sum and rest per stage run (consistent: total of the stage run = open + sum + rest)
        let sum: Vec<Vec<f64>> = (0..VARIANTS.len()).map(|i| raw_stage_runs[i].iter().map(|l| l.open_ms + l.stages.iter().map(|x| x.1).sum::<f64>()).collect()).collect();
        row("**Sum open + stages** (stage run)", sum);
        let rest: Vec<Vec<f64>> = (0..VARIANTS.len()).map(|i| raw_stage_runs[i].iter().map(|l| l.total - l.open_ms - l.stages.iter().map(|x| x.1).sum::<f64>()).collect()).collect();
        row("Rest (copying/allocation, not measured individually)", rest);
        row("**Total open → finished 16-bit pixels** (stage run)", (0..VARIANTS.len()).map(|i| raw_stage_runs[i].iter().map(|l| l.total).collect()).collect());
        row("**Total open → finished 16-bit pixels** (normal run, without stage timing)", (0..VARIANTS.len()).map(|i| raw_runs[i].iter().map(|l| l.total).collect()).collect());
        row("Develop 2.8 MP preview (after opening)", (0..VARIANTS.len()).map(|i| raw_runs[i].iter().map(|l| l.preview).collect()).collect());
        row("Open + 2.8 MP preview (until the image is visible)", (0..VARIANTS.len()).map(|i| raw_runs[i].iter().map(|l| l.open_ms + l.preview).collect()).collect());
        println!();
    }
}
