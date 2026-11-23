//! `graineo-dev hdr FRAME... [--reference N] [--no-align] [--ghost off|standard|fine] [--threads N] --out OUT(.tif|.f32)`: merge an exposure
//! bracket with the app's merge (crate `hdr`) and write the linear result (float32 RGB, white-balanced camera RGB of the
//! reference, reference crop; `--fusion`: exposure fusion instead of the linear result) for the HDR test bench; prints times, exposure factors and peak memory.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn peak_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    if cfg!(target_os = "macos") {
        u.ru_maxrss as f64 / 1048576.0
    } else {
        u.ru_maxrss as f64 / 1024.0
    }
}

pub fn run(args: &[String]) -> Result<()> {
    let mut frames: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut settings = hdr::Settings::default();
    let mut threads = 4usize;
    let mut preview = false;
    // X-Trans demosaic on the GPU as in the app (`--gpu`)
    let mut gpu = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--reference" => settings.reference = it.next().and_then(|v| v.parse().ok()),
            "--no-align" => settings.align = false,
            "--threads" => threads = it.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            "--preview" => preview = true,
            "--gpu" => gpu = true,
            // memory budget in GB as the app sets it from the computer's memory (merge works in bands when needed)
            "--budget-gb" => {
                if let Some(g) = it.next().and_then(|v| v.parse::<f64>().ok()) {
                    hdr::merge::set_memory_budget((g * 1e9) as u64);
                }
            }
            "--fusion" => settings.output = hdr::fusion::Output::Fusion,
            // fusion strength 0 … 1 (default 1, the full fusion)
            "--fusion-strength" => settings.fusion_strength = it.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--ghost" => {
                settings.ghost = match it.next().map(|v| v.as_str()) {
                    Some("off") => hdr::ghost::Level::Off,
                    Some("fine") => hdr::ghost::Level::Fine,
                    _ => hdr::ghost::Level::Standard,
                }
            }
            _ => frames.push(PathBuf::from(a)),
        }
    }
    let out = out.context("--out missing")?;
    if frames.len() < 2 {
        bail!("at least two frames needed");
    }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    pool.install(|| -> Result<()> {
        let cancel = AtomicBool::new(false);
        let progress = |_: f32| {};
        let ctl = hdr::Control { cancel: &cancel, progress: &progress };
        let t0 = Instant::now();
        let mut plan = hdr::analyze(&frames, &settings, &ctl).map_err(|e| anyhow::anyhow!("{e}"))?;
        let t_an = t0.elapsed().as_secs_f64();
        let s = plan.summary();
        let info = plan.reference_info().clone();
        let wb = plan.wb;
        let t1 = Instant::now();
        let g = hdr::ghost::detect(&plan);
        let (g_area, g_holes) = (g.area, g.holes);
        drop(g);
        let hook = move |m: &[f32], w: usize, h: usize, pat: [[u8; 6]; 6], sensor: raw::Sensor| -> Option<Vec<[f32; 3]>> {
            if gpu && sensor == raw::Sensor::XTrans {
                rawdev::gpu::xtrans::markesteijn(m, w, h, pat)
            } else {
                None
            }
        };
        let m = if preview { hdr::merge::preview(&plan, 2048, &ctl) } else { hdr::merge::full_with(&mut plan, &ctl, Some(&hook)) }.map_err(|e| anyhow::anyhow!("{e}"))?;
        let t_merge = t1.elapsed().as_secs_f64();
        drop(plan);
        let t2 = Instant::now();
        let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "tif" | "tiff" => {
                let f = std::io::BufWriter::new(std::fs::File::create(&out)?);
                let mut enc = tiff::encoder::TiffEncoder::new(f)?;
                let flat: Vec<f32> = m.px.iter().flatten().copied().collect();
                enc.write_image::<tiff::encoder::colortype::RGB32Float>(m.w as u32, m.h as u32, &flat)?;
            }
            _ => {
                let mut bytes = Vec::with_capacity(m.w * m.h * 12);
                for p in &m.px {
                    for v in p {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                }
                std::fs::write(&out, bytes)?;
            }
        }
        let t_write = t2.elapsed().as_secs_f64();
        let total = t0.elapsed().as_secs_f64();
        let json = serde_json::json!({
            "w": m.w, "h": m.h, "crop": null, "wb": wb, "ref_index": s.reference,
            "ref": frames[s.reference].file_name().map(|n| n.to_string_lossy().to_string()),
            "kind": if settings.output == hdr::fusion::Output::Fusion { "fusion" } else { "linear" }, "ev": s.ev, "shift_px": s.shift, "warnings": s.warnings,
            "noise": {"a": m.noise.a, "b": m.noise.b}, "peak": m.peak(),
            "timings": {"analyze": t_an, "merge": t_merge, "write": t_write}, "total": total,
            "peak_rss_mb": peak_rss_mb(), "threads": threads, "camera": format!("{} {}", info.maker, info.model),
            "algo": hdr::ALGO_VERSION,
            "ghost": {"level": settings.ghost, "motion_area": g_area, "holes": g_holes},
        });
        std::fs::write(format!("{}.json", out.display()), serde_json::to_string_pretty(&json)?)?;
        eprintln!(
            "hdr: {}×{} ref {} EV {:?} shift {:?} px, analyse {t_an:.2} s, merge {t_merge:.2} s, total {total:.2} s, peak {:.0} MB, warnings {:?}",
            m.w,
            m.h,
            s.reference,
            s.ev.iter().map(|v| (v * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
            s.shift.iter().map(|v| (v * 100.0).round() / 100.0).collect::<Vec<_>>(),
            peak_rss_mb(),
            s.warnings
        );
        Ok(())
    })
}

/// `graineo-dev hdr-library FOLDER`: add FOLDER to the library of `GRAINEO_DATA_DIR` (test libraries for the HDR
/// dialog; refuses to run without `GRAINEO_DATA_DIR`, so the real library is never touched).
pub fn library(args: &[String]) -> Result<()> {
    if std::env::var_os("GRAINEO_DATA_DIR").is_none() {
        bail!("GRAINEO_DATA_DIR must point to a test folder");
    }
    let folder = PathBuf::from(args.first().context("FOLDER missing")?);
    let db = library::db::Db::standard().map_err(|e| anyhow::anyhow!("{e}"))?;
    let (root, report) = library::reconcile::add_root(&db, &folder, &|_| {}).map_err(|e| anyhow::anyhow!("{e}"))?;
    let n = library::exif::backfill(&db, 100_000, &AtomicBool::new(false)).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("root {root}: {}, shooting data {n}", report.json());
    Ok(())
}
