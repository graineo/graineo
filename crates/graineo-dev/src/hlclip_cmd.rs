//! `graineo-dev hlclip FILE…`: how much highlight information of a mosaic RAW lies above 1 after the as-shot white
//! balance although the photosite itself is below sensor saturation (red/blue with a white-balance gain > 1).
//!
//! Per file one line: camera, WB, share of red/blue photosites with `v·wb > 1` and `v < 0.987` (unclipped on the sensor),
//! share of sensor-clipped photosites per colour, and the share of 2×2 cells (≈ image area) that contain such a photosite.

use anyhow::{anyhow, Result};

pub fn run(args: &[String]) -> Result<()> {
    // `--mask FILE OUT.png`: 8-bit mask in crop size, 255 where a photosite of the 2×2 cell exceeds 1 after WB
    if args.first().map(String::as_str) == Some("--mask") {
        return mask(&args[1], &args[2]);
    }
    if args.first().map(String::as_str) == Some("--base") {
        let rf = raw::RawFile::open(&args[1]).map_err(|e| anyhow!("{e}"))?;
        let b = rf.basis(raw::development::Demosaic::Auto, true, false, raw::development::Size::Full).map_err(|e| anyhow!("{e}"))?;
        let mut mx = [0f32; 3];
        let mut over = [0usize; 3];
        for p in &b.px {
            for c in 0..3 {
                mx[c] = mx[c].max(p[c]);
                over[c] += (p[c] > 1.0) as usize;
            }
        }
        let n = b.px.len() as f64;
        if let Some(out) = args.get(2) {
            // raw f32 dump: width, height (u32 LE), then RGB f32 LE
            let mut v = Vec::with_capacity(8 + b.px.len() * 12);
            v.extend((b.width as u32).to_le_bytes());
            v.extend((b.height as u32).to_le_bytes());
            v.extend(b.px.iter().flat_map(|p| p.iter().flat_map(|c| c.to_le_bytes())));
            std::fs::write(out, v)?;
        }
        println!("base max {:?} over1% {:.3} {:.3} {:.3} clippedPhotosites {}", mx, 100.0 * over[0] as f64 / n, 100.0 * over[1] as f64 / n, 100.0 * over[2] as f64 / n, b.clipped_frac);
        return Ok(());
    }
    let files = args;
    println!("FILE\tCAMERA\tSENSOR\tWB_R\tWB_B\tlostR%\tlostB%\tlostArea%\tlostAbove1.2%\tclipR%\tclipG%\tclipB%\tmaxR\tmaxG\tmaxB");
    for f in files {
        if let Err(e) = one(f) {
            println!("{f}\tERROR\t{e}");
        }
    }
    Ok(())
}

fn one(file: &str) -> Result<()> {
    let rf = raw::RawFile::open(file).map_err(|e| anyhow!("{e}"))?;
    let i = &rf.info;
    let name = std::path::Path::new(file).file_name().unwrap_or_default().to_string_lossy().to_string();
    let cam = raw::camera_display(&i.maker, &i.model);
    let (norm, pat) = match rf.mosaic_normalized() {
        Ok(x) => x,
        Err(e) => {
            println!("{name}\t{cam}\t{:?}\t-\t-\tno mosaic: {e}", i.sensor);
            return Ok(());
        }
    };
    let wb = i.wb.unwrap_or([1.0; 3]);
    let (w, h) = (i.width, i.height);
    let r = i.crop.or(i.active).unwrap_or(raw::Region { x: 0, y: 0, w, h });
    let mut n = [0usize; 3];
    let mut lost = [0usize; 3];
    let mut lost12 = 0usize;
    let mut clip = [0usize; 3];
    let mut max = [0f32; 3];
    let (cw, ch) = (r.w / 2 + 1, r.h / 2 + 1);
    let mut cell = vec![false; cw * ch];
    for y in r.y..(r.y + r.h).min(h) {
        for x in r.x..(r.x + r.w).min(w) {
            let c = pat[y % 6][x % 6] as usize;
            let v = norm[y * w + x];
            n[c] += 1;
            max[c] = max[c].max(v);
            if v >= 0.987 {
                clip[c] += 1;
            } else if v * wb[c] > 1.0 {
                lost[c] += 1;
                if v * wb[c] > 1.2 {
                    lost12 += 1;
                }
                cell[((y - r.y) / 2) * cw + (x - r.x) / 2] = true;
            }
        }
    }
    let pc = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    let area = cell.iter().filter(|b| **b).count();
    println!(
        "{name}\t{cam}\t{:?}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}",
        i.sensor,
        wb[0],
        wb[2],
        pc(lost[0], n[0]),
        pc(lost[2], n[2]),
        pc(area, (r.w / 2) * (r.h / 2)),
        pc(lost12, n[0] + n[2]),
        pc(clip[0], n[0]),
        pc(clip[1], n[1]),
        pc(clip[2], n[2]),
        max[0],
        max[1],
        max[2]
    );
    Ok(())
}

fn mask(file: &str, out: &str) -> Result<()> {
    let rf = raw::RawFile::open(file).map_err(|e| anyhow!("{e}"))?;
    let i = &rf.info;
    let (norm, pat) = rf.mosaic_normalized().map_err(|e| anyhow!("{e}"))?;
    let wb = i.wb.unwrap_or([1.0; 3]);
    let (w, h) = (i.width, i.height);
    let r = i.crop.or(i.active).unwrap_or(raw::Region { x: 0, y: 0, w, h });
    let mut m = image::GrayImage::new(r.w as u32, r.h as u32);
    for y in r.y..(r.y + r.h).min(h) {
        for x in r.x..(r.x + r.w).min(w) {
            let c = pat[y % 6][x % 6] as usize;
            if norm[y * w + x] * wb[c] >= 0.98 {
                let (cx, cy) = ((x - r.x) & !1, (y - r.y) & !1);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    if cx + dx < r.w && cy + dy < r.h {
                        m.put_pixel((cx + dx) as u32, (cy + dy) as u32, image::Luma([255]));
                    }
                }
            }
        }
    }
    m.save(out)?;
    Ok(())
}
