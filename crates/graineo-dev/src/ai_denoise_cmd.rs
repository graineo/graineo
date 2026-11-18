//! `graineo-dev ai-denoise …`: checks and measurements of the neural denoising (`raw::ai_denoise`).
//!
//!   ai-denoise tile MODEL.onnx IN.f32 OUT.f32   one 3×512×512 tile (planar f32, little endian) through the network,
//!                                              for the comparison with a reference runtime
//!   ai-denoise bench MODEL.onnx [N]              time per 512 tile (N runs) and the extrapolation to 24 MP
//!   ai-denoise install                           install the module like Settings → Modules (only with
//!                                              GRAINEO_DATA_DIR; GRAINEO_MODULE_SOURCE = folder with the package)

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

pub fn run(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    if sub == "install" {
        if std::env::var_os("GRAINEO_DATA_DIR").is_none() {
            bail!("only with GRAINEO_DATA_DIR (test data folder)");
        }
        let d = &library::module::AI_DENOISE;
        let t = Instant::now();
        library::module::install(d, &|a, b, f| eprint!("\r{a}/{b} {f}          "), &std::sync::atomic::AtomicBool::new(false)).map_err(|e| anyhow!(e))?;
        library::module::verify(d).map_err(|e| anyhow!(e))?;
        println!("\nINSTALLED\t{}\t{:.1} s", library::module::folder(d).display(), t.elapsed().as_secs_f64());
        return Ok(());
    }
    let model = args.get(1).context("model file missing")?;
    let t = Instant::now();
    let net = raw::ai_denoise::Net::load(Path::new(model)).map_err(|e| anyhow!(e))?;
    println!("LOAD\t{:.2} s", t.elapsed().as_secs_f64());
    let n = raw::ai_denoise::TILE;
    match sub {
        "tile" => {
            let input = std::fs::read(args.get(2).context("input missing")?)?;
            let x: Vec<f32> = input.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            if x.len() != 3 * n * n {
                bail!("input must hold 3×{n}×{n} values");
            }
            let t = Instant::now();
            let y = net.run(&x, n, n).map_err(|e| anyhow!(e))?;
            println!("RUN\t{:.3} s", t.elapsed().as_secs_f64());
            let bytes: Vec<u8> = y.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(args.get(3).context("output missing")?, bytes)?;
        }
        "bench" => {
            let runs: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(3);
            let x: Vec<f32> = (0..3 * n * n).map(|i| 0.2 + 0.05 * ((i * 2654435761usize) % 1000) as f32 / 1000.0).collect();
            let _ = net.run(&x, n, n).map_err(|e| anyhow!(e))?;
            let t = Instant::now();
            for _ in 0..runs {
                let _ = net.run(&x, n, n).map_err(|e| anyhow!(e))?;
            }
            let per = t.elapsed().as_secs_f64() / runs as f64;
            let step = (n - 2 * raw::ai_denoise::OVERLAP) as f64;
            let tiles_24mp = (6000.0f64 / step).ceil() * (4000.0f64 / step).ceil();
            println!("TILE\t{per:.3} s\t24MP ({tiles_24mp} tiles)\t{:.1} s", per * tiles_24mp);
        }
        _ => bail!("ai-denoise tile|bench MODEL …"),
    }
    Ok(())
}
