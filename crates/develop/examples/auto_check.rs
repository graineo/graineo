//! Check tool for "Auto" (tone): develops each image once with default values
//! and once with the automatically determined sliders and places both side by side on
//! contact sheets (two pairs per row: left before, right after). The table on stdout
//! lists values, metrics and computation time per image in sheet order.
//!
//! `cargo run --release -p develop --example auto_check -- OUTPUT_FOLDER FILE|FOLDER … [--max N] [--spp]`
//!
//! `--spp`: like the app, with Sigma's base (from the disk cache or newly computed, slow).

use std::path::{Path, PathBuf};

use develop::session::Preview;
use develop::{auto, export, geometry, Rgb16, Session, Settings};

const TILE: usize = 420;
const PAIRS_PER_ROW: usize = 2;
const ROWS_PER_SHEET: usize = 5;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut max = usize::MAX;
    let mut use_spp = false;
    let mut out_dir = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--max" => max = it.next().and_then(|v| v.parse().ok()).unwrap_or(max),
            "--spp" => use_spp = true,
            _ if out_dir.is_none() => out_dir = Some(PathBuf::from(a)),
            _ => {
                let p = PathBuf::from(a);
                if p.is_dir() {
                    let mut v: Vec<PathBuf> = std::fs::read_dir(&p)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f")))
                        .collect();
                    v.sort();
                    files.extend(v);
                } else {
                    files.push(p);
                }
            }
        }
    }
    let Some(out_dir) = out_dir else {
        eprintln!("Usage: auto_check OUTPUT_FOLDER FILE|FOLDER … [--max N] [--spp]");
        std::process::exit(2);
    };
    files.truncate(max);
    std::fs::create_dir_all(&out_dir).expect("output folder");

    let mut tiles: Vec<(Rgb16, Rgb16)> = Vec::new();
    let mut sheet = 0;
    println!("{:>3} {:<34} {:>5} | {:>5} {:>5} {:>5} {:>5} {:>4} | key {:>4}→{:<4} p99 {:>4}→{:<4} clip% {:>4}→{:<4} dark% {:>4}→{:<4} | ms", "#", "File", "ISO", "Exp", "Con", "High", "Shad", "Fill", "", "", "", "", "", "", "", "");
    for (n, path) in files.iter().enumerate() {
        let mut session = match Session::open(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                continue;
            }
        };
        let base = Settings::default();
        let preview = if use_spp && Session::uses_spp(&base) {
            match session.ensure_spp(&base) {
                Ok(_) => {
                    let m = session.spp_matrix().unwrap();
                    Preview::from_spp(&session.spp_base().unwrap(), m, 2400)
                }
                Err(e) => {
                    eprintln!("{}: SPP base: {e}", path.display());
                    continue;
                }
            }
        } else {
            let (lin, f) = session.preview_linear(2400);
            Preview::developed(lin, f, &session.cal, &base)
        };
        let cal = session.cal.clone();
        let t = std::time::Instant::now();
        let Some((a, [s0, s1])) = auto::auto_tone_traced(&preview, &cal, &base) else {
            eprintln!("{}: no analysis", path.display());
            continue;
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let name = path.file_name().unwrap().to_string_lossy();
        println!(
            "{:>3} {:<34} {:>5} | {:>+5.2} {:>+5.2} {:>+5.2} {:>+5.2} {:>+4.1} | key {:.2}→{:.2} p99 {:.2}→{:.2} clip% {:>4.1}→{:<4.1} dark% {:>4.1}→{:<4.1} | {ms:.0}",
            n,
            name,
            cal.iso,
            a.exposure,
            a.contrast,
            a.highlight,
            a.shadow,
            a.fill_light,
            s0.key,
            s1.key,
            s0.p99,
            s1.p99,
            s0.clip * 100.0,
            s1.clip * 100.0,
            s0.dark * 100.0,
            s1.dark * 100.0
        );
        let mut after = base.clone();
        a.apply_to(&mut after);
        let before = tile(geometry::apply(preview.render(&cal, &base), &base, cal.orientation));
        let after = tile(geometry::apply(preview.render(&cal, &after), &after, cal.orientation));
        tiles.push((before, after));
        if tiles.len() == PAIRS_PER_ROW * ROWS_PER_SHEET {
            write_sheet(&out_dir, sheet, &tiles);
            sheet += 1;
            tiles.clear();
        }
    }
    if !tiles.is_empty() {
        write_sheet(&out_dir, sheet, &tiles);
    }
}

/// Downscale to at most TILE px (block mean, then nearest neighbour).
fn tile(img: Rgb16) -> Rgb16 {
    let f = img.width.max(img.height).div_ceil(TILE).max(1);
    let (w, h) = (img.width / f, img.height / f);
    let mut data = vec![0u16; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0u32;
                for yy in 0..f {
                    for xx in 0..f {
                        acc += img.data[((y * f + yy) * img.width + x * f + xx) * 3 + c] as u32;
                    }
                }
                data[(y * w + x) * 3 + c] = (acc / (f * f) as u32) as u16;
            }
        }
    }
    Rgb16 { width: w, height: h, data }
}

fn write_sheet(dir: &Path, index: usize, tiles: &[(Rgb16, Rgb16)]) {
    let gap = 8;
    let cell = TILE + gap;
    let rows = tiles.len().div_ceil(PAIRS_PER_ROW);
    let (w, h) = (PAIRS_PER_ROW * 2 * cell + gap * PAIRS_PER_ROW, rows * cell + gap);
    let mut data = vec![0x2a2au16; w * h * 3];
    for (i, (a, b)) in tiles.iter().enumerate() {
        let (r, c) = (i / PAIRS_PER_ROW, i % PAIRS_PER_ROW);
        for (k, img) in [a, b].into_iter().enumerate() {
            let x0 = gap + c * (2 * cell + gap) + k * cell + (TILE - img.width) / 2;
            let y0 = gap + r * cell + (TILE - img.height) / 2;
            for y in 0..img.height {
                let dst = ((y0 + y) * w + x0) * 3;
                data[dst..dst + img.width * 3].copy_from_slice(&img.data[y * img.width * 3..(y + 1) * img.width * 3]);
            }
        }
    }
    let path = dir.join(format!("auto_{index:02}.jpg"));
    export::write_jpeg(&Rgb16 { width: w, height: h, data }, &path, 88).expect("write JPEG");
    eprintln!("→ {}", path.display());
}
