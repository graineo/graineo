//! `graineo-dev export-scene RAW OUTDIR`: export a RAW (or merged photo path) as linear DNG (32-bit float), OpenEXR
//! (half and float) and TIFF 32 bit with the scene-linear path of the app, open the DNG again and compare it with the
//! original's stage (round trip). Prints one line of numbers per check.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// Statistics of `log2(b / a)` over the pixels where `a` is above `floor`: (median, 95 % quantile of the absolute value, count).
fn log_ratio(a: &[[f32; 3]], b: &[[f32; 3]], floor: f32) -> (f32, f32, usize) {
    let mut d: Vec<f32> = Vec::new();
    for (p, q) in a.iter().zip(b) {
        if p[1] > floor && q[1] > 0.0 {
            d.push((q[1] / p[1]).log2());
        }
    }
    if d.is_empty() {
        return (f32::NAN, f32::NAN, 0);
    }
    d.sort_by(|x, y| x.total_cmp(y));
    let med = d[d.len() / 2];
    let mut ad: Vec<f32> = d.iter().map(|v| v.abs()).collect();
    ad.sort_by(|x, y| x.total_cmp(y));
    (med, ad[ad.len() * 95 / 100], d.len())
}

pub fn run(args: &[String]) -> Result<()> {
    let src = PathBuf::from(args.first().context("RAW missing")?);
    let out = PathBuf::from(args.get(1).context("OUTDIR missing")?);
    std::fs::create_dir_all(&out)?;
    let doc = rawdev::RawDoc::open(&src, &|| true).map_err(|e| anyhow::anyhow!(e))?.context("cancelled")?;
    let s = develop::Settings::default();
    let opts = develop::export::Options { dng_float: true, ..Default::default() };
    let stem = src.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "x".into());
    let t = std::time::Instant::now();
    let dng = out.join(format!("{stem}.dng"));
    rawdev::export::export_scene_opts(&doc, &s, develop::export::Format::Dng, &dng, &opts).map_err(|e| anyhow::anyhow!(e))?;
    let t_dng = t.elapsed().as_secs_f32();
    for (name, f) in [("half.exr", develop::export::Format::Exr { half: true }), ("float.exr", develop::export::Format::Exr { half: false }), ("linear.tif", develop::export::Format::TiffFloat)] {
        let p = out.join(format!("{stem}-{name}"));
        rawdev::export::export_scene_opts(&doc, &s, f, &p, &opts).map_err(|e| anyhow::anyhow!(e))?;
        println!("{} {} bytes", p.display(), std::fs::metadata(&p)?.len());
    }
    println!("{} {} bytes ({t_dng:.1} s)", dng.display(), std::fs::metadata(&dng)?.len());
    // round trip: the DNG opened by the app
    let back = rawdev::RawDoc::open(&dng, &|| true).map_err(|e| anyhow::anyhow!(e))?;
    let Some(back) = back else { bail!("DNG not opened") };
    let (a, b) = (&doc.preview, &back.preview);
    println!("sizes: original stage {}×{}, DNG stage {}×{}", a.w, a.h, b.w, b.h);
    if (a.w, a.h) == (b.w, b.h) {
        // block means (16 × 16): the two stages differ in noise and in the demosaic of single pixels, not in the image
        let blocks = |v: &[[f32; 3]]| -> Vec<[f32; 3]> {
            let (bw, bh) = (a.w / 16, a.h / 16);
            let mut o = vec![[0f32; 3]; bw * bh];
            for by in 0..bh {
                for bx in 0..bw {
                    let mut acc = [0f32; 3];
                    for y in 0..16 {
                        for x in 0..16 {
                            let p = v[(by * 16 + y) * a.w + bx * 16 + x];
                            for c in 0..3 {
                                acc[c] += p[c] / 256.0;
                            }
                        }
                    }
                    o[by * bw + bx] = acc;
                }
            }
            o
        };
        // position check: the p95 of the block deviation for the DNG stage shifted by whole pixels
        for (dx, dy) in [(0isize, 0isize), (1, 0), (-1, 0), (0, 1), (0, -1), (2, 0), (-2, 0), (0, 2), (0, -2)] {
            let (w, h) = (a.w as isize, a.h as isize);
            let mut shifted = vec![[0f32; 3]; a.px.len()];
            for y in 0..h {
                for x in 0..w {
                    let (sx, sy) = ((x + dx).clamp(0, w - 1), (y + dy).clamp(0, h - 1));
                    shifted[(y * w + x) as usize] = b.px[(sy * w + sx) as usize];
                }
            }
            let (m, q, _) = log_ratio(&blocks(&a.px), &blocks(&shifted), 0.02);
            println!("  shift ({dx:+},{dy:+}): median {m:+.4} EV, p95 {q:.4} EV");
        }
        let (ba, bb) = (blocks(&a.px), blocks(&b.px));
        let (med, p95, n) = log_ratio(&ba, &bb, 0.02);
        // by brightness of the original block (green): where does the deviation sit?
        for (lo, hi) in [(0.02f32, 0.05f32), (0.05, 0.2), (0.2, 0.6), (0.6, 100.0)] {
            let pa: Vec<[f32; 3]> = ba.iter().zip(&bb).filter(|(p, _)| p[1] >= lo && p[1] < hi).map(|(p, _)| *p).collect();
            let pb: Vec<[f32; 3]> = ba.iter().zip(&bb).filter(|(p, _)| p[1] >= lo && p[1] < hi).map(|(_, q)| *q).collect();
            let (m, q, c) = log_ratio(&pa, &pb, 0.0);
            println!("  block level {lo:.2}–{hi:.2}: {c} blocks, median {m:+.4} EV, p95 {q:.4} EV");
        }
        // the colour channels: ratio of the means of each channel over all blocks
        let mean = |v: &[[f32; 3]], c: usize| v.iter().map(|p| p[c] as f64).sum::<f64>() / v.len().max(1) as f64;
        println!("  channel means original {:.4} {:.4} {:.4} · DNG {:.4} {:.4} {:.4}", mean(&ba, 0), mean(&ba, 1), mean(&ba, 2), mean(&bb, 0), mean(&bb, 1), mean(&bb, 2));
        let peak = |v: &[[f32; 3]]| v.iter().map(|p| p[1]).fold(0f32, f32::max);
        println!("round trip DNG: median {med:+.4} EV, p95 |dev| {p95:.4} EV over {n} px; peak {:.3} → {:.3}", peak(&a.px), peak(&b.px));
        // full resolution: the DNG exported once more as TIFF against the first TIFF (the same development of both files)
        let tif_back = out.join(format!("{stem}-fromdng.tif"));
        rawdev::export::export_scene_opts(&back, &s, develop::export::Format::TiffFloat, &tif_back, &opts).map_err(|e| anyhow::anyhow!(e))?;
        let read = |p: &std::path::Path| -> Result<(usize, usize, Vec<f32>)> {
            let mut d = tiff::decoder::Decoder::new(std::io::BufReader::new(std::fs::File::open(p)?))?.with_limits(tiff::decoder::Limits::unlimited());
            let (w, h) = d.dimensions()?;
            match d.read_image()? {
                tiff::decoder::DecodingResult::F32(v) => Ok((w as usize, h as usize, v)),
                _ => bail!("TIFF is not float"),
            }
        };
        let (w1, h1, t1) = read(&out.join(format!("{stem}-linear.tif")))?;
        let (w2, h2, t2) = read(&tif_back)?;
        if (w1, h1) == (w2, h2) {
            for bs in [1usize, 16] {
                let (bw, bh) = (w1 / bs, h1 / bs);
                let blocks = |t: &[f32]| -> Vec<[f32; 3]> {
                    let mut o = vec![[0f32; 3]; bw * bh];
                    for by in 0..bh {
                        for bx in 0..bw {
                            let mut acc = [0f32; 3];
                            for y in 0..bs {
                                for x in 0..bs {
                                    let i = ((by * bs + y) * w1 + bx * bs + x) * 3;
                                    for c in 0..3 {
                                        acc[c] += t[i + c] / (bs * bs) as f32;
                                    }
                                }
                            }
                            o[by * bw + bx] = acc;
                        }
                    }
                    o
                };
                let (m, q, n) = log_ratio(&blocks(&t1), &blocks(&t2), 0.02);
                println!("full resolution, blocks of {bs}×{bs}: median {m:+.4} EV, p95 {q:.4} EV over {n} blocks");
            }
        } else {
            println!("full resolution: sizes differ {w1}×{h1} vs {w2}×{h2}");
        }
    } else {
        let mean = |v: &[[f32; 3]]| v.iter().map(|p| p[1] as f64).sum::<f64>() / v.len().max(1) as f64;
        println!("round trip DNG (sizes differ): mean green {:.5} → {:.5}", mean(&a.px), mean(&b.px));
    }
    Ok(())
}
