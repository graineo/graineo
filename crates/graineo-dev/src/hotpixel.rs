//! `graineo-dev hotpixel <list.txt> <output.tsv> [--serial S] [--max N]`: finds pixels that are brighter than
//! their 3×3 neighbourhood in many dark areas of many X3F files of the same camera (raw planes, read only).
//! Output: per plane and position the number of files in which the pixel was conspicuous and the number of files in
//! which it lay in a dark area at all.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write;

pub fn run(args: &[String]) -> Result<()> {
    let list = args.first().context("list of X3F files missing")?;
    let out = args.get(1).context("Ausgabedatei fehlt")?;
    let mut serial: Option<String> = None;
    let mut max = usize::MAX;
    let mut it = args.iter().skip(2);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--serial" => serial = it.next().cloned(),
            "--max" => max = it.next().context("--max N")?.parse()?,
            _ => {}
        }
    }
    let files: Vec<String> = std::fs::read_to_string(list)?.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    // key (plane, x, y) → (conspicuous, dark)
    let mut hits: HashMap<(u8, u32, u32), (u32, u32)> = HashMap::new();
    let mut dark_count: Vec<u32> = Vec::new();
    let mut used = 0usize;
    let mut size = (0usize, 0usize);
    for f in &files {
        if used >= max {
            break;
        }
        let Ok(x3f) = x3f::X3fFile::open(f) else { continue };
        if let Some(s) = &serial {
            let ser = x3f.properties().ok().and_then(|pl| pl.pairs.iter().find(|(k, _)| k == "CAMSERIAL").map(|(_, v)| v.trim().to_string()));
            if ser.as_deref() != Some(s.as_str()) {
                continue;
            }
        }
        let Ok(img) = x3f.decode_raw() else { continue };
        let (w, h) = (img.columns, img.rows);
        if size == (0, 0) {
            size = (w, h);
            dark_count = vec![0; w * h];
        }
        if size != (w, h) {
            continue;
        }
        used += 1;
        for (c, p) in img.planes.iter().enumerate() {
            for y in 40..h - 40 {
                for x in 70..w - 160 {
                    let v = p[y * w + x] as i32;
                    let mut nb = [0i32; 8];
                    let mut k = 0;
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            nb[k] = p[(y as i32 + dy) as usize * w + (x as i32 + dx) as usize] as i32;
                            k += 1;
                        }
                    }
                    nb.sort_unstable();
                    let med = (nb[3] + nb[4]) / 2;
                    // dark surroundings only (raw codes incl. black ≈ 16–32): hot pixels show there
                    if med > 400 {
                        continue;
                    }
                    if c == 0 {
                        dark_count[y * w + x] += 1;
                    }
                    let spread = (nb[6] - nb[1]).max(4);
                    if v - med > (3 * spread).max(24) && nb[7] - med < spread * 2 {
                        let e = hits.entry((c as u8, x as u32, y as u32)).or_insert((0, 0));
                        e.0 += 1;
                    }
                }
            }
        }
        eprintln!("{used}: {f}");
    }
    let mut rows: Vec<_> = hits.into_iter().filter(|(_, v)| v.0 >= 3).collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let mut o = std::io::BufWriter::new(std::fs::File::create(out)?);
    writeln!(o, "schicht\tx\ty\tauffaellig\tdunkel\tdateien={used}")?;
    for ((c, x, y), (n, _)) in &rows {
        let d = dark_count[*y as usize * size.0 + *x as usize];
        writeln!(o, "{c}\t{x}\t{y}\t{n}\t{d}")?;
    }
    eprintln!("{} candidates (≥ 3 files) from {used} files", rows.len());
    Ok(())
}
