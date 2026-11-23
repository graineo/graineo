//! `graineo-dev black-bias FILE... [--gpu]`: black-level bias of the demosaic in the deepest shadows.
//!
//! The demosaic must keep the local mean of the mosaic: in a flat dark area the mean of each output channel equals the
//! mean of the photosites of that colour. Clipping negative noise around black breaks this and lifts the shadows (more
//! in red/blue, which have fewer photosites and a larger white-balance gain). Measured per file on the white-balanced,
//! normalised mosaic (as the development sees it, without highlight reconstruction):
//! - optically black sensor margins (if the decoder knows them): a real dark frame;
//! - the darkest 3 % of 36×36 blocks of the image area.
//!
//! Reported per channel: mosaic mean (truth), demosaic mean, bias (absolute, in units of the photosite noise σ and
//! relative to the mean), share of output values that are exactly 0, and NaN/Inf in the whole output.

use anyhow::{anyhow, Result};
use raw::development::{demosaic_mosaic, Demosaic};
use raw::{RawFile, Sensor};

const BS: usize = 36;

struct Acc {
    mos: [f64; 3],
    mos2: [f64; 3],
    nm: [f64; 3],
    dem: [f64; 3],
    zero: [f64; 3],
    n: f64,
}

impl Acc {
    fn new() -> Self {
        Acc { mos: [0.0; 3], mos2: [0.0; 3], nm: [0.0; 3], dem: [0.0; 3], zero: [0.0; 3], n: 0.0 }
    }

    fn add(&mut self, m: &[f32], rgb: &[[f32; 3]], w: usize, pat: &[[u8; 6]; 6], x0: usize, y0: usize, x1: usize, y1: usize) {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y * w + x;
                let c = pat[y % 6][x % 6] as usize;
                let v = m[i] as f64;
                self.mos[c] += v;
                self.mos2[c] += v * v;
                self.nm[c] += 1.0;
                for k in 0..3 {
                    self.dem[k] += rgb[i][k] as f64;
                    self.zero[k] += (rgb[i][k] == 0.0) as u8 as f64;
                }
                self.n += 1.0;
            }
        }
    }

    fn report(&self, label: &str) {
        if self.n == 0.0 {
            return;
        }
        println!("  {label}: {} px", self.n as u64);
        for (k, name) in ["R", "G", "B"].iter().enumerate() {
            let mm = self.mos[k] / self.nm[k].max(1.0);
            let sd = (self.mos2[k] / self.nm[k].max(1.0) - mm * mm).max(0.0).sqrt();
            let dm = self.dem[k] / self.n;
            let b = dm - mm;
            println!(
                "    {name}: mosaic {:+.6} demosaic {:+.6} bias {:+.6} = {:+.3} σ (σ {:.6}), rel {:+.1} %, zeros {:.2} %",
                mm,
                dm,
                b,
                b / sd.max(1e-12),
                sd,
                100.0 * b / mm.abs().max(1e-12),
                100.0 * self.zero[k] / self.n
            );
        }
    }
}

pub fn run(args: &[String]) -> Result<()> {
    let gpu = args.iter().any(|a| a == "--gpu");
    for f in args.iter().filter(|a| !a.starts_with("--")) {
        let rf = RawFile::open(f).map_err(|e| anyhow!("{f}: {e}"))?;
        let i = rf.info.clone();
        if !matches!(i.sensor, Sensor::Bayer | Sensor::XTrans) {
            println!("{f}: no Bayer/X-Trans mosaic, skipped");
            continue;
        }
        let (mut m, pat) = rf.mosaic_normalized().map_err(|e| anyhow!("{e}"))?;
        let (w, h) = (i.width, i.height);
        let wb = i.wb.unwrap_or([1.0; 3]);
        for (y, row) in m.chunks_mut(w).enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                *v *= wb[pat[y % 6][x % 6] as usize];
            }
        }
        let d = if i.sensor == Sensor::Bayer { Demosaic::Rcd } else { Demosaic::Markesteijn3 };
        let t = std::time::Instant::now();
        let rgb = if gpu && i.sensor == Sensor::XTrans {
            rawdev::gpu::xtrans::markesteijn(&m, w, h, pat).ok_or_else(|| anyhow!("no GPU"))?
        } else {
            demosaic_mosaic(&m, w, h, pat, d, i.sensor)
        };
        let dt = t.elapsed();
        let bad = rgb.iter().flatten().filter(|v| !v.is_finite()).count();
        let neg_in = m.iter().filter(|v| **v < 0.0).count();
        let neg_out = rgb.iter().flatten().filter(|v| **v < 0.0).count();
        println!(
            "{f}: {} {} ISO {:?}, {w}×{h}, {:?}{}, {:.0?}; mosaic < 0: {:.2} %, output < 0: {:.2} %, NaN/Inf: {bad}",
            i.maker,
            i.model,
            i.exif.iso,
            d,
            if gpu && i.sensor == Sensor::XTrans { " (GPU)" } else { "" },
            dt,
            100.0 * neg_in as f64 / m.len() as f64,
            100.0 * neg_out as f64 / (3 * rgb.len()) as f64
        );
        // optically black margins (keep 12 px away from their edges: the demosaic reaches across)
        let mut ob = Acc::new();
        for r in rf.black_margins() {
            if r.w > 30 && r.h > 30 && r.x + r.w <= w && r.y + r.h <= h {
                ob.add(&m, &rgb, w, &pat, r.x + 12, r.y + 12, r.x + r.w - 12, r.y + r.h - 12);
            }
        }
        ob.report("optical black");
        // darkest 3 % of the blocks in the image area (green mosaic mean)
        let r = i.crop.or(i.active).unwrap_or(raw::Region { x: 0, y: 0, w, h });
        let (bx0, by0) = (r.x.div_ceil(6) * 6 + 12, r.y.div_ceil(6) * 6 + 12);
        let (nbx, nby) = ((r.x + r.w).saturating_sub(bx0 + 12) / BS, (r.y + r.h).saturating_sub(by0 + 12) / BS);
        let mut blocks: Vec<(f64, usize, usize)> = Vec::new();
        for by in 0..nby {
            for bx in 0..nbx {
                let (x0, y0) = (bx0 + bx * BS, by0 + by * BS);
                let (mut s, mut n) = (0f64, 0f64);
                for y in y0..y0 + BS {
                    for x in x0..x0 + BS {
                        if pat[y % 6][x % 6] == 1 {
                            s += m[y * w + x] as f64;
                            n += 1.0;
                        }
                    }
                }
                blocks.push((s / n, x0, y0));
            }
        }
        blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let take = (blocks.len() * 3 / 100).max(1).min(blocks.len());
        let mut dark = Acc::new();
        for &(_, x0, y0) in &blocks[..take] {
            dark.add(&m, &rgb, w, &pat, x0, y0, x0 + BS, y0 + BS);
        }
        dark.report(&format!("darkest 3 % blocks ({take} of {})", blocks.len()));
        // second band: blocks between 3 and 10 % (low shadows)
        let take2 = (blocks.len() / 10).max(take + 1).min(blocks.len());
        let mut low = Acc::new();
        for &(_, x0, y0) in &blocks[take..take2] {
            low.add(&m, &rgb, w, &pat, x0, y0, x0 + BS, y0 + BS);
        }
        low.report("blocks 3–10 %");
    }
    Ok(())
}
