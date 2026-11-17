//! graineo-dev – inspection tool for X3F files.
//!
//! ```text
//! graineo-dev info    <file.X3F>               sections, properties, CAMF overview
//! graineo-dev camf    <file.X3F> [name …]      print CAMF entries (all or selected)
//! graineo-dev raw     <file.X3F> <out.tif>     RAW planes as 16-bit TIFF (linear, unprocessed)
//! graineo-dev raw     <file.CR3|CR2|RAF|DNG|ARW> [out.jpg] [--info] [--max PX] [--rawler] [--phase1] [--compare REF.jpg]
//!                                         non-Sigma RAW (rawler): info, simple development,
//!                                         comparison with the camera preview
//! graineo-dev preview <file.X3F> <out.jpg>     extract the embedded camera JPEG
//! graineo-dev develop <file.X3F> <out.jpg|tif|dng> [options]   develop (see develop_cmd)
//! graineo-dev x3fuse  <file.X3F> <out.tif> [--denoiser=nlm|bm3d|aus] [--luma] [--no-chroma] [--strength=0.1..3] [--backend=cpu|gpu|auto]
//!                                         test bench: X3Fuse's own path (noise reduction on the sensor data
//!                                         before the color conversion), not our development
//! graineo-dev compare <file.X3F> [folder]      compare against x3fuse-core (SPP); GRAINEO_COMPARE_OWN=1:
//!                                         own pipeline only, GRAINEO_COMPARE_COUNTS=1: SPP base + output stage
//! graineo-dev spphash <file.X3F>                fingerprint of the SPP base
//! graineo-dev film    <file.X3F> <out.jpg|tif> --film <id> […]   film simulation (spektrafilm),
//!                                         `graineo-dev film --list` lists films and papers
//! graineo-dev sppa    <file.X3F> [out.json]    settings saved by SPP (read only) and
//!                                         the graineo settings derived from them
//! graineo-dev qa <output_folder> <files|folders …>   quality check: compare the SPP path, own pipeline and
//!                                         camera JPEG → report.html, result.json
//! graineo-dev noise-measure <a.tif> [b …] [--crop x,y,w,h]   noise, colour blotches, banding, shadow tint
//! graineo-dev sensor <RAW> [--kind …] [--crop x,y,w,h] [--sheet] --output <folder>
//!                                         sensor analyses (Bayer/X-Trans): PNGs + metrics.json
//! ```
//!
//! Further commands: `apple-photos`, `lens`, `masks`, `lr-masks`, `retouch`, `color-chart`, `grain`,
//! `style`, `camera-color`, `apple-raw`, `learn-look`, `look-pair`, `look-image`, `rawapp`, `psnr`, `sppgpu`, `sppsharp`, `bench`, `stages`, `ca`,
//! `defringe`, `sharpen-study`, `color-export`, `wide-stat`, `auto-data`, `lr-compare`, `lr-calibrate`,
//! `lr-samples`, `camera-samples`. The German command names of earlier versions (`lens`, `masks`,
//! `farbtafel`, `grain`, `stil`, `sensorblick`, …) are still accepted as hidden aliases.

use std::fs::File;
use std::io::BufWriter;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tiff::encoder::{colortype, TiffEncoder};
use x3f::{CamfEntry, MatrixData, X3fFile};

mod classic_output;
mod scene_cmd;
mod learn_look_cmd;
mod samples_look;
mod film_shelf;
mod merrill_new;
mod color_quality;
mod pairs_bench;
mod series_cmd;
mod search_bench;
mod places_cmd;
mod semantic_search;
mod content_keywords_cmd;
mod color_chart;
mod film;
mod analog;
mod geo_compare;
mod grain_measure;
mod lenscheck;
mod baseline_compare;
mod perf;
mod gpu_b;
mod lens_measure;
mod noise_measure;
mod hotpixel;
mod looks;
mod camera_color;
mod focus_cmd;
mod lr_handover;
mod gpu_measure;
mod qa;
mod bench_data;

/// Lightroom catalog for the measuring tools (read only): `GRAINEO_LR_CATALOG`, else the usual location on this Mac.
/// The app itself never reads a catalog outside the import.
pub(crate) fn dev_catalog() -> std::path::PathBuf {
    std::env::var_os("GRAINEO_LR_CATALOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| sysdirs::home().unwrap_or_default().join("Pictures/Lightroom/Lightroom Catalog.lrcat"))
}
mod rawapp;
mod apple_raw_cmd;
mod color_chart_raw;
mod gpu_compare;
mod rawtest;
mod raw_compare;
mod rcps_compare;
mod sensor_cmd;
mod update_cmd;
#[cfg(target_os = "macos")]
mod camera;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("apple-photos")) {
        if args.get(1).map(String::as_str) == Some("--sync") {
            return apple_photos_sync_cmd();
        }
        return apple_photos_cmd(args.get(1).map(String::as_str));
    }
    // sync of a test library with times and system calls, see `sync_cmd`
    // series and stacks: grouping measured on an own synthetic library / statistics of a library copy
    if args.first().map(String::as_str) == Some("series-measure") {
        return series_cmd::measure(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("series-demo") {
        return series_cmd::demo(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("series-stats") {
        return series_cmd::stats(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("sync") {
        return sync_cmd(&args[1..]);
    }
    // camera import with a real camera (ImageCaptureCore): `camera [--load] [--auto]`, see camera.rs
    #[cfg(target_os = "macos")]
    if args.first().map(String::as_str) == Some("camera") {
        return camera::run(&args[1..]);
    }
    // release signing for the update check (update-key needs no file argument)
    if let Some(c) = args.first().filter(|c| c.starts_with("update-")) {
        return update_cmd::run(c, &args[1..]);
    }
    // noise, colour blotches, banding and shadow tint of developed images, see noise_measure.rs
    // measurement of the semantic search (relevance, strictness, German vs. English), see semantic_search.rs
    if args.first().map(String::as_str) == Some("semantic-search") {
        return semantic_search::run(&args[1..]);
    }
    // offline place names: build the data file / spot checks, see places_cmd.rs
    if args.first().map(String::as_str) == Some("places-build") {
        return places_cmd::build(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("places-lookup") {
        return places_cmd::lookup(&args[1..]);
    }
    // time of the library search per query and term, see search_bench.rs
    if args.first().map(String::as_str) == Some("search-measure") {
        return search_bench::run(&args[1..]);
    }
    // keyword suggestions from the picture content: ranking, threshold sweep, hits per photo, see content_keywords_cmd.rs
    if args.first().map(String::as_str) == Some("content-keywords") {
        return content_keywords_cmd::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("semantic-search-fill") {
        return semantic_search::fill(&args[1..]);
    }
    // test data and machine facts for the start page benchmarks, see bench_data.rs
    if args.first().map(String::as_str) == Some("bench-data") {
        return bench_data::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("hotpixel") {
        return hotpixel::run(&args[1..]);
    }
    // GPU vs. CPU path of the Foveon development: time, deviation, memory, see gpu_measure.rs
    if args.first().map(String::as_str) == Some("gpu-measure") {
        return gpu_measure::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("noise-measure") {
        return noise_measure::run(&args[1..]);
    }
    // handover of the Lightroom develop data into the sidecars: check run and before/after comparison, see lr_handover.rs
    if matches!(args.first().map(String::as_str), Some("lr-handover-dry-run")) {
        return lr_handover::dry_run(&args[1..]);
    }
    if matches!(args.first().map(String::as_str), Some("lr-handover-check")) {
        return lr_handover::check(&args[1..]);
    }
    let (cmd, file) = match (args.first(), args.get(1)) {
        (Some(c), Some(f)) => (c.as_str(), f),
        _ => bail!("usage: graineo-dev <command> <file> …\ncommands: info camf raw preview develop x3fuse compare spphash sppa sppgpu sppsharp bench stages ca defringe psnr film qa sensor rawapp lens masks lr-masks retouch color-chart grain style camera-color learn-look samples-look look-pair look-image sharpen-study color-export wide-stat auto-data lr-compare lr-calibrate lr-samples camera-samples apple-photos update-key update-sign update-verify update-manifest"),
    };
    if cmd == "perf" { return perf::run(&args[1..]); }
    if cmd == "scene-depth" { return scene_cmd::depth(&args[1..]); }
    if cmd == "scene-depth-compare" { return scene_cmd::depth_compare(&args[1..]); }
    if cmd == "scene-sky" { return scene_cmd::sky(&args[1..]); }
    if cmd == "mask-refine" { return scene_cmd::mask_refine(&args[1..]); }
    if cmd == "scene-segments" { return scene_cmd::segments(&args[1..]); }
    if cmd == "mask-ref" { return scene_cmd::mask_ref(&args[1..]); }
    if cmd == "gpub" { return gpu_b::run(&args[1..]); }
    if cmd == "perf-reference" { return baseline_compare::reference(&args[1..]); }
    if matches!(cmd, "lens") { return lens_measure::run(&args[1..]); }
    if matches!(cmd, "masks") { return { #[path = "masks.rs"] mod masks_cmd; masks_cmd::run(file, &args[2..]) }; }
    if matches!(cmd, "lr-masks") { return { #[path = "lr_masks.rs"] mod lr_masks_cmd; lr_masks_cmd::run(file, &args[2..]) }; }
    if matches!(cmd, "retouch") { return { #[path = "retouch.rs"] mod retouch_cmd; retouch_cmd::run(file, &args[2..]) }; }
    if cmd == "qa" {
        anyhow::ensure!(args.len() > 2, "usage: graineo-dev qa <output_folder> <files|folders …>");
        return qa::run(file, &args[2..]);
    }
    // film simulation (spektrafilm), see film.rs; `film --list` without an image file
    if cmd == "film" {
        return film::run(&args[1..]);
    }
    // time per film of the small film previews (darkroom shelf, "Film & Look"), see film_shelf.rs
    if cmd == "film-shelf" {
        return film_shelf::run(&args[1..]);
    }
    // black-and-white film from the Foveon layers: scene dump, layer weights, see analog.rs
    if cmd == "analog" {
        return analog::run(&args[1..]);
    }
    // yardstick for color film profiles (test chart, ΔE00 of two profiles), see color_chart.rs
    if matches!(cmd, "color-chart") {
        return color_chart::run(&args[1..]);
    }
    // measure the graininess of the film simulation, see grain_measure.rs
    if matches!(cmd, "grain") {
        return grain_measure::run(&args[1..]);
    }
    // measure camera looks (picture style from the makernote, RAW/JPEG pairs), see looks.rs
    // focus areas the camera wrote into the file (normalised), see focus_cmd.rs
    if cmd == "focus" {
        return focus_cmd::run(&args[1..]);
    }
    if matches!(cmd, "style") {
        return looks::style(&args[1..]);
    }
    // time of reading the picture styles of many RAWs (window "Kamera-Looks"), see looks.rs
    if matches!(cmd, "style-scan") {
        return looks::style_scan(&args[1..]);
    }
    // color distance to the camera's own JPEG rendering over many RAW/JPEG pairs, see camera_color.rs
    if cmd == "camera-color" {
        return camera_color::run(&args[1..]);
    }
    // learn camera looks from RAW + camera JPEG pairs (same code as the app), see learn_look_cmd.rs
    if cmd == "learn-look" {
        return learn_look_cmd::run(&args[1..]);
    }
    // starting look from sample files of other photographers (e.g. raw.pixls.us), see samples_look.rs
    if cmd == "samples-look" {
        return samples_look::run(&args[1..]);
    }
    if matches!(cmd, "look-pair") {
        return looks::look_pair(&args[1..]);
    }
    if matches!(cmd, "look-image") {
        return looks::look_image(&args[1..]);
    }
    // non-Sigma RAWs (CR2/CR3/RAF/DNG/ARW …) via crates/raw (rawler), see rawtest.rs
    // non-Sigma RAWs as in the app (crates/rawdev), with Lightroom starting values, see rawapp.rs
    // sensor analyses on the mosaic (crates/sensor_view), see sensor_cmd.rs
    if matches!(cmd, "sensor") {
        return sensor_cmd::run(file, &args[2..]);
    }
    if cmd == "sharpen-study" {return color_quality::study(file);}
    if cmd == "color-export" {return color_quality::export(file,&args[2..]);}
    if cmd == "wide-stat" {return wide_stat(&args[1..]);}
    if matches!(cmd, "auto-data") {return auto_data(&args[1..]);}
    if matches!(cmd, "lr-compare") {return lr_compare(&args[1..]);}
    if matches!(cmd, "lr-calibrate") {return lr_calibrate(&args[1..]);}
    if matches!(cmd, "lr-samples") {return lr_samples(&args[1..]);}
    if matches!(cmd, "camera-samples") {return camera_samples(&args[1..]);}
    // Removing RAW+JPEG pairs into the Trash, measured (own test files only), see pairs_bench.rs
    if cmd == "pairs-measure" {
        return pairs_bench::run(&args[1..]);
    }
    if cmd == "classic-output" { return classic_output::run(file, &args[2..]); }
    // GPU against processor path (time, ΔE00, tile edges), see gpu_compare.rs
    if cmd == "gpu-compare" {
        return gpu_compare::run(&args[1..]);
    }
    // color fidelity against a ColorChecker in the shot (WB on patch 22, exposure on patch 20), see color_chart_raw.rs
    if cmd == "color-chart-raw" {
        return color_chart_raw::run(file, &args[2..]);
    }
    if cmd == "rawapp" {
        return rawapp::run(file, &args[2..]);
    }
    // the system RAW engine of macOS (Core Image) as in the app's comparison view, see apple_raw_cmd.rs
    if cmd == "apple-raw" {
        return apple_raw_cmd::run(file, &args[2..]);
    }
    // the system RAW engine as decoding stage of a development (Apple path): times, exposure against graineo
    if cmd == "apple-stage" {
        return apple_raw_cmd::stage(file, &args[2..]);
    }
    if cmd == "raw" && !file.to_ascii_lowercase().ends_with(".x3f") {
        return rawtest::run(file, &args[2..]);
    }
    if cmd == "sppa" {
        return sppa(file, args.get(2).map(String::as_str));
    }
    if cmd == "psnr" {
        return psnr(file, args.get(2).context("second TIFF file missing")?);
    }
    let t = Instant::now();
    let x3f = X3fFile::open(file).with_context(|| format!("opening {file}"))?;
    match cmd {
        "info" => info(&x3f)?,
        // code index (row, column) at a bit position of an intact TRUE plane: `codeat <X3F> <plane> <bit>`
        "codeat" => {
            let plane: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0);
            let bit: u64 = args.get(3).and_then(|a| a.parse().ok()).context("bit position missing")?;
            let (e, h) = x3f.raw_entry()?;
            let body = &x3f.section(e)[x3f::ImageHeader::SIZE..];
            let t = x3f::raw::TrueLayout::parse(body)?;
            let huff = x3f::huffman::TrueHuffman::new(&t.pairs)?;
            let src = &t.data[t.starts[plane]..t.starts[plane] + t.plane_size[plane] as usize];
            let mut br = x3f::bits::BitReader::new(src);
            let mut k = 0usize;
            while br.bit_position() < bit {
                huff.decode_diff(&mut br);
                k += 1;
            }
            println!("bit {} → code {k} (row {} col {}), exact {}", br.bit_position(), k / h.columns as usize, k % h.columns as usize, br.bit_position() == bit);
        }
        // per-row map of one damaged TRUE plane: row start (file offset), invalid codes, values out of range
        "rawmap" => {
            let plane: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0);
            let (e, h) = x3f.raw_entry()?;
            let body = &x3f.section(e)[x3f::ImageHeader::SIZE..];
            let t = x3f::raw::TrueLayout::parse(body)?;
            let huff = x3f::huffman::TrueHuffman::new(&t.pairs)?;
            let src = &t.data[t.starts[plane]..t.starts[plane] + t.plane_size[plane] as usize];
            let src_off = e.offset + x3f::ImageHeader::SIZE + (t.data.as_ptr() as usize - body.as_ptr() as usize) + t.starts[plane];
            println!("plane {plane}: file offset {src_off}, {} bytes", src.len());
            let mut br = x3f::bits::BitReader::new(src);
            let mut row_start = [[t.seeds[plane] as i32; 2]; 2];
            let (rows, cols) = (h.rows as usize, h.columns as usize);
            for row in 0..rows {
                let pos = br.bit_position();
                let (mut bad, mut oor) = (0, 0);
                let mut acc = [0i32; 2];
                for col in 0..cols {
                    if !huff.next_is_valid(&mut br) {
                        bad += 1;
                    }
                    let d = huff.decode_diff(&mut br);
                    let prev = if col < 2 { row_start[row & 1][col & 1] } else { acc[col & 1] };
                    let v = prev + d;
                    acc[col & 1] = v;
                    if col < 2 {
                        row_start[row & 1][col & 1] = v;
                    }
                    if !(0..=4095).contains(&v) {
                        oor += 1;
                    }
                }
                if bad > 0 || oor > 0 || row % 200 == 0 || (row > 900 && row < 960) {
                    println!("row {row}: bit {pos} (file byte {}) invalid {bad} oor {oor} start {:?}", src_off as u64 + pos / 8, row_start);
                }
            }
        }
        // rescue damaged RAW data: `rescue <X3F> <out.tif> [intact.X3F]` (with an intact original of the same
        // file: comparison of the rescued rows, for tests with artificially damaged copies)
        "rescue" => {
            let t = Instant::now();
            let (img, r) = x3f.decode_raw_rescue()?;
            println!("rescued {:.1} % of the rows in {:.2?}", r.rescued_fraction() * 100.0, t.elapsed());
            for c in 0..3 {
                println!("plane {c}: lost rows {:?}", r.lost[c]);
            }
            if let Some(orig) = args.get(3) {
                let o = X3fFile::open(orig)?.decode_raw()?;
                for c in 0..3 {
                    let (mut exact, mut off, mut max_dev) = (0, 0, 0i32);
                    for row in (0..img.rows).filter(|&row| !r.is_lost(c, row)) {
                        let a = &img.planes[c][row * img.columns..(row + 1) * img.columns];
                        let b = &o.planes[c][row * img.columns..(row + 1) * img.columns];
                        let dev = a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
                        max_dev = max_dev.max(dev);
                        if dev == 0 { exact += 1 } else { off += 1 }
                    }
                    println!("plane {c}: kept rows identical {exact}, deviating {off} (max |Δ| {max_dev})");
                }
            }
            let data = img.interleaved();
            let mut enc = TiffEncoder::new(BufWriter::new(File::create(args.get(2).context("output file missing")?)?))?;
            enc.write_image::<colortype::RGB16>(img.columns as u32, img.rows as u32, &data)?;
        }
        // quality of the filling of lost planes: `fillcheck <damaged X3F> <intact X3F>` → mean |Δ| in counts and
        // relative to the signal for the estimated pixels (GRAINEO_FILL=learn,tile,intercept to compare)
        "fillcheck" => {
            let (mut img, r) = x3f.decode_raw_rescue()?;
            let orig = X3fFile::open(args.get(2).context("intact X3F missing")?)?;
            let o = orig.decode_raw()?;
            let cal = develop::calib::Calibration::read(&orig, &o)?;
            let d = develop::damaged::fill(&mut img, &r, &cal);
            println!("{d:?}");
            let [x0, y0, x1, y1] = cal.active;
            let (mut sum, mut sig, mut n) = (0f64, 0f64, 0usize);
            for row in y0..=y1 {
                for c in 0..3 {
                    if !r.is_lost(c, row) || (0..3).all(|p| r.is_lost(p, row)) {
                        continue;
                    }
                    for col in x0..=x1 {
                        let i = row * img.columns + col;
                        sum += (img.planes[c][i] as f64 - o.planes[c][i] as f64).abs();
                        sig += o.planes[c][i] as f64 - cal.black[c];
                        n += 1;
                    }
                }
            }
            println!("estimated pixels {n}: mean |Δ| {:.1} counts ({:.1} % of the signal)", sum / n.max(1) as f64, 100.0 * sum / sig.max(1.0));
        }
        // damage check of the TRUE planes (invalid codes, values outside 12 bits, stream length)
        "rawcheck" => {
            let (e, h) = x3f.raw_entry()?;
            println!("RAW {}×{} type {} format {}", h.columns, h.rows, h.image_type, h.format);
            if !h.is_merrill_raw() && !(h.is_classic_raw() && h.format == 30) {
                println!("verdict: {:?}", x3f.integrity());
                return Ok(());
            }
            let checks = x3f::RawImage::check_merrill(&x3f.section(e)[x3f::ImageHeader::SIZE..], h)?;
            for (c, k) in checks.iter().enumerate() {
                println!(
                    "plane {c}: {} · invalid codes {} (first row {:?}) · out of range {} (first row {:?}) · bits used {} of {}",
                    if k.is_clean() { "clean" } else { "DAMAGED" },
                    k.invalid_codes,
                    k.first_invalid_row,
                    k.out_of_range,
                    k.first_out_of_range_row,
                    k.bits_used,
                    k.bits_available
                );
            }
            println!("verdict: {:?}", x3f.integrity());
        }
        "camf" => camf(&x3f, &args[2..])?,
        // Sigma's Classic chain to the linear codes: `classic-kette <X3F> [WB] [Farbmodus]`
        // (stages with GRAINEO_CLASSIC_DUMP=<dir> as f32 files)
        "classic-chain" => {
            let camf = x3f.camf()?;
            let props = x3f.properties()?;
            let wb = args.get(2).cloned().unwrap_or_else(|| props.get("WB_DESC").unwrap_or("Auto").trim().to_string());
            let mode = args.get(3).cloned().unwrap_or_else(|| props.get("CM_DESC").unwrap_or("Standard").trim().to_string());
            let t0 = Instant::now();
            match develop::classic_new::run(&x3f, &camf, &props, &wb, &mode, x3f.header_rotation().unwrap_or(0) as u32) {
                Ok(c) => println!("{}×{} crop {:?} in {:.2?}", c.width, c.height, c.crop, t0.elapsed()),
                Err(e) => println!("not available: {e}"),
            }
        }
        "raw" => raw(&x3f, args.get(2).context("output file missing")?)?,
        "preview" => {
            let out = args.get(2).context("output file missing")?;
            match x3f.preview_jpeg() {
                Some(jpg) => std::fs::write(out, jpg)?,
                // SD9/SD10: uncompressed preview only (type 2, format 3)
                None => {
                    let (w, h, rgb) = x3f::read_embedded_plain(file, true).context("no embedded preview")?;
                    image::RgbImage::from_raw(w, h, rgb).context("preview incomplete")?.save(out)?;
                }
            }
        }
        "compare" => compare(file, args.get(2).map(String::as_str))?,
        // fingerprint of the SPP base (checks that optimizations stay bit-identical)
        "spphash" => {
            let t = Instant::now();
            let c = develop::spp::working(std::path::Path::new(file), None).map_err(anyhow::Error::msg)?;
            let mut h: u64 = 0xcbf29ce484222325;
            for v in &c.data {
                for b in v.to_bits().to_le_bytes() {
                    h = (h ^ b as u64).wrapping_mul(0x100000001b3);
                }
            }
            println!("{h:016x} ({}×{}, {:.2?})", c.width, c.height, t.elapsed());
        }
        // CPU vs. GPU noise reduction: times, PSNR, deviations
        // optional: `sppgpu <X3F> <luma> <chroma>` with SPP noise sliders 0…1
        "sppgpu" => sppgpu(file, &args[2..])?,
        // Test bench: finished image CPU against GPU without the RCPS lock (not exact, never in the app)
        "rcps-compare" => rcps_compare::run(file, &args[2..])?,
        // sharpness slider: check the detail image and incremental resharpening against x3fuse-core
        "sppsharp" => sppsharp(file)?,
        "bench" => {
            for _ in 0..3 {
                let t0 = Instant::now();
                let session = develop::Session::open(file)?;
                let t1 = t0.elapsed();
                let s = develop::Settings::default();
                let (small, f) = session.preview_linear(2000);
                let p = develop::session::Preview::developed(small, f, &session.cal, &s);
                let t2 = t0.elapsed();
                let img = p.render(&session.cal, &s);
                eprintln!("open {:.0?} · preview {:.0?} · render {:.0?} ({}×{})", t1, t2 - t1, t0.elapsed() - t2, img.width, img.height);
            }
        }
        // pipeline inspection: all stages and difference images as JPEG into a folder
        // optional `--look`: with clarity and vignette (checks the look difference image)
        "stages" => stages_cmd(file, args.get(2).context("output folder missing")?, args.get(3).is_some_and(|a| a == "--look"))?,
        // alternative Merrill parts: fit of their parameters against the default form
        "merrill-new-fit" => merrill_new::fit(&args[1..])?,
        "fill-ref" => merrill_new::fill_ref(&args[1..])?,
        "develop" => develop_cmd(file, args.get(2).context("output file missing")?, &args[3..])?,
        "x3fuse" => x3fuse_cmd(file, args.get(2).context("output file missing")?, &args[3..])?,
        // color fringes: timing, crops before/after (see `defringe_cmd`)
        // lens correction (CA as in SPP): measurement before/after, crops
        "ca" => lenscheck::run(file, args.get(2).context("output folder missing")?, args.get(3).map(String::as_str))?,
        "defringe" => defringe_cmd(file, args.get(2).context("output prefix missing")?, &args[3..])?,
        _ => bail!("unknown command {cmd}"),
    }
    eprintln!("done in {:.2?}", t.elapsed());
    Ok(())
}

/// `sppsharp <X3F>`: checks the sharpness slider.
/// 1. Base without sharpening (`X3F_SPP_TEST=sharpen=0`) + `sharpen_px(L, base)` must give the
///    normal SPP base bit-identically (detail image and computation exact).
/// 2. For s ≠ 0: incremental (base + Δ, as SPP does when the slider is moved) against
///    single-step (`sharpen = amt(s)`, as SPP does when developing fresh with s).
fn sppsharp(file: &str) -> Result<()> {
    use develop::sharpness::{amount, sharpen_px};
    let path = std::path::Path::new(file);
    let key = develop::SppKey { mode: None, nr: (2, 2), ..Default::default() };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let run = |amt: Option<f32>| -> Result<develop::Linear> {
        match amt {
            Some(a) => std::env::set_var("X3F_SPP_TEST", format!("sharpen={a:e}")),
            None => std::env::remove_var("X3F_SPP_TEST"),
        }
        let r = develop::spp::working_key(path, &key, false, &cancel).map_err(anyhow::Error::msg);
        std::env::remove_var("X3F_SPP_TEST");
        r
    };
    let base = run(None)?;
    let d = base.detail.clone().context("no detail image")?;
    println!("sharpness base = {}", d.base);
    let raw = run(Some(0.0))?;
    let rd = raw.detail.clone().context("no detail image")?;
    println!("detail image independent of amount: {}", rd.l == d.l);
    let mut diff = 0usize;
    for (i, p) in raw.data.chunks_exact(3).enumerate() {
        let q = sharpen_px([p[0], p[1], p[2]], d.l[i], f64::from(amount(0.0, d.base, 1.0)));
        let b = &base.data[i * 3..i * 3 + 3];
        diff += (0..3).filter(|&c| q[c].to_bits() != b[c].to_bits()).count();
    }
    println!("unsharpened + Sharpen(amt(0)) vs. base: {diff} differing values (0 = bit-identical)");
    for s in [-2.0f32, -1.0, -0.5, 0.5, 1.0, 2.0] {
        let one = run(Some(amount(f64::from(s), d.base, 1.0)))?;
        let delta = d.delta(s);
        let (amt0, amt) = d.amounts(s).unwrap();
        let mut changed = 0usize;
        // [incremental, reversed+single-step]: (differing, max, sum of squared errors)
        let mut st = [(0usize, 0f64, 0f64); 2];
        for (i, p) in base.data.chunks_exact(3).enumerate() {
            let p = [p[0], p[1], p[2]];
            let qs = [sharpen_px(p, d.l[i], delta), develop::sharpness::resharpen_px(p, d.l[i], amt0, amt)];
            for c in 0..3 {
                if (f64::from(qs[1][c]) - f64::from(p[c])).abs() > 0.5 {
                    changed += 1;
                }
                for (k, q) in qs.iter().enumerate() {
                    let e = f64::from(q[c]) - f64::from(one.data[i * 3 + c]);
                    if q[c].to_bits() != one.data[i * 3 + c].to_bits() {
                        st[k].0 += 1;
                    }
                    st[k].1 = st[k].1.max(e.abs());
                    st[k].2 += e * e;
                }
            }
        }
        let n = base.data.len() as f64;
        let fmt = |(k, max, se): (usize, f64, f64)| {
            format!("{:.3} % differing, max {max:.3}, PSNR {:.1} dB", 100.0 * k as f64 / n, 10.0 * (32767f64.powi(2) / (se / n)).log10())
        };
        println!("s = {s:+.1}: amt {amt:.4} (Δ {delta:+.4}), changed values {:.1} %", 100.0 * changed as f64 / n);
        println!("    vs. single-step – incremental: {} | reversed+single-step: {}", fmt(st[0]), fmt(st[1]));
    }
    // GPU version: detail image vs. CPU
    let gpu = develop::spp::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?;
    if develop::spp::last_used_gpu() {
        let g = gpu.detail.clone().context("no detail image (GPU)")?;
        let (mut n, mut max) = (0usize, 0i32);
        for (a, b) in g.l.iter().zip(&d.l) {
            let e = (i32::from(*a) - i32::from(*b)).abs();
            n += usize::from(e != 0);
            max = max.max(e);
        }
        println!("GPU detail image: {:.3} % differing, max {max} (units ×4), base {}", 100.0 * n as f64 / d.l.len() as f64, g.base);
    } else {
        println!("GPU not used");
    }
    // speed of the output stage with and without the sharpness slider
    let session = develop::Session::open(path)?;
    let m = session.cal.mode(None).cam_to_srgb;
    let preview = develop::session::Preview::from_spp(
        &develop::session::SppBase::new(key.clone(), base.clone()),
        m,
        1600,
    );
    for v in [0.0f32, 1.0] {
        let s = develop::Settings { sigma_sharpness: v, ..Default::default() };
        let t = Instant::now();
        let n = 5;
        for _ in 0..n {
            std::hint::black_box(develop::render_counts(&base, &session.cal, &s, &m, None));
        }
        let full = t.elapsed() / n;
        let crop = base.crop(1500, 1000, 1200, 900);
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(develop::render_counts(&crop, &session.cal, &s, &m, None));
        }
        let region = t.elapsed() / n;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(preview.render(&session.cal, &s));
        }
        println!(
            "sharpness {v:+.1}: render_counts full {full:.1?}, crop 1200×900 {region:.1?}, preview {}×{} {:.1?}",
            preview.lin.width, preview.lin.height, t.elapsed() / n
        );
    }
    // layer mixer off/on (same output stage, just a different matrix)
    for l in [None, Some(develop::layers::INFRARED_NRG)] {
        let s = develop::Settings { layer_matrix: l, ..Default::default() };
        let n = 5;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(develop::render_counts(&base, &session.cal, &s, &m, None));
        }
        println!("layer mixer {}: render_counts full {:.1?}", if l.is_some() { "on " } else { "off" }, t.elapsed() / n);
    }
    Ok(())
}

/// `sppgpu <X3F>`: compute the SPP working image on CPU and GPU, print times and deviation.
fn sppgpu(file: &str, sliders: &[String]) -> Result<()> {
    let path = std::path::Path::new(file);
    // values 0…1; negative (e.g. −0.25) = level "Off"
    let step = |i: usize| -> Result<i8> {
        let v: f64 = sliders.get(i).map_or(Ok(0.5), |s| s.parse())?;
        Ok((v.clamp(develop::NR_OFF, 1.0) * 4.0).round() as i8)
    };
    let key = develop::SppKey { mode: None, nr: (step(0)?, step(1)?), ..Default::default() };
    eprintln!("noise slider levels (luminance, color): {:?}", key.nr);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let t = Instant::now();
    match develop::spp::gpu_adapter() {
        Ok(name) => eprintln!("GPU: {name} (device + shaders: {:.0?})", t.elapsed()),
        Err(e) => eprintln!("no GPU: {e}"),
    }
    let t = Instant::now();
    let cpu = develop::spp::working_key(path, &key, false, &cancel).map_err(anyhow::Error::msg)?;
    let t_cpu = t.elapsed();
    let mut gpu = None;
    let mut times = Vec::new();
    for _ in 0..4 {
        let t = Instant::now();
        // `GRAINEO_SPP_COMPARE_NUDGE=…`: instead of the GPU, a second CPU run with this
        // X3F_SPP_TEST value (sensitivity test).
        gpu = Some(match std::env::var("GRAINEO_SPP_COMPARE_NUDGE") {
            Ok(spec) => {
                let old = std::env::var("X3F_SPP_TEST").ok();
                std::env::set_var("X3F_SPP_TEST", spec);
                let r = develop::spp::working_key(path, &key, false, &cancel);
                match old {
                    Some(v) => std::env::set_var("X3F_SPP_TEST", v),
                    None => std::env::remove_var("X3F_SPP_TEST"),
                }
                r
            }
            Err(_) => develop::spp::working_key(path, &key, true, &cancel),
        }
        .map_err(anyhow::Error::msg)?);
        times.push(t.elapsed());
    }
    let gpu = gpu.unwrap();
    println!(
        "CPU total {t_cpu:.2?} · GPU total {:.2?} (runs: {:.2?}) · GPU used: {}",
        times.iter().min().unwrap(), times, develop::spp::last_used_gpu()
    );
    anyhow::ensure!(cpu.width == gpu.width && cpu.height == gpu.height, "sizes differ");
    let peak = cpu.data.iter().cloned().fold(0f32, f32::max) as f64;
    let (mut se, mut sae, mut max, mut at, mut se_g) = (0f64, 0f64, 0f64, 0, 0f64);
    let w = cpu.width;
    let mut col = vec![0f64; w];
    let mut row = vec![0f64; cpu.height];
    for (i, (a, b)) in cpu.data.iter().zip(&gpu.data).enumerate() {
        let d = (*a as f64 - *b as f64).abs();
        se += d * d;
        sae += d;
        if d > max {
            max = d;
            at = i / 3;
        }
        col[(i / 3) % w] += d;
        row[i / 3 / w] += d;
        // gamma-encoded comparison (≈ rendered image, peak = 1).
        let g = |v: f32| (v.max(0.0) as f64 / peak).powf(1.0 / 2.2);
        let dg = g(*a) - g(*b);
        se_g += dg * dg;
    }
    let n = cpu.data.len() as f64;
    println!(
        "counts: PSNR {:.1} dB (peak {peak:.0}), mean |Δ| {:.4}, max |Δ| {max:.2} at ({}, {}) – CPU {:?} GPU {:?}",
        10.0 * (peak * peak / (se / n)).log10(),
        sae / n,
        at % w,
        at / w,
        cpu.pixel(at % w, at / w),
        gpu.pixel(at % w, at / w),
    );
    println!("gamma 1/2.2 encoded: PSNR {:.1} dB", 10.0 * (1.0 / (se_g / n)).log10());
    // systematic offset (mean of the signed deviation, relative).
    let (mut sd, mut sc) = (0f64, 0f64);
    for (a, b) in cpu.data.iter().zip(&gpu.data) {
        sd += *b as f64 - *a as f64;
        sc += *a as f64;
    }
    println!("mean deviation GPU−CPU: {:+.2e} (relative {:+.2e})", sd / n, sd / sc);
    // tile edge check: columns/rows with the largest mean deviation.
    let top = |v: &[f64], len: usize| {
        let mut idx: Vec<usize> = (0..v.len()).collect();
        idx.sort_by(|a, b| v[*b].total_cmp(&v[*a]));
        let mean = v.iter().sum::<f64>() / v.len() as f64 / (len * 3) as f64;
        let list: Vec<String> = idx[..6].iter().map(|&i| format!("{i}:{:.4}", v[i] / (len * 3) as f64)).collect();
        format!("mean {mean:.4}, largest {}", list.join(" "))
    };
    if std::env::var_os("GRAINEO_SPP_DIFFMAP").is_some() {
        // coarse map of the mean |Δ| in 32×24 blocks.
        let (bw, bh) = (cpu.width.div_ceil(32), cpu.height.div_ceil(24));
        for by in 0..24 {
            let mut line = String::new();
            for bx in 0..32 {
                let (mut sum, mut cnt) = (0f64, 0f64);
                for y in by * bh..((by + 1) * bh).min(cpu.height) {
                    for x in bx * bw..((bx + 1) * bw).min(w) {
                        for c in 0..3 {
                            let i = (y * w + x) * 3 + c;
                            sum += (cpu.data[i] as f64 - gpu.data[i] as f64).abs();
                            cnt += 1.0;
                        }
                    }
                }
                let m = sum / cnt.max(1.0);
                line.push(if m < 0.001 { '.' } else if m < 0.01 { '1' } else if m < 0.1 { '2' } else if m < 1.0 { '3' } else { '#' });
            }
            println!("  {line}");
        }
    }
    if let Some(prefix) = std::env::var_os("GRAINEO_SPP_DIFF_PPM") {
        // images for visual comparison: CPU and GPU in display gamma, |Δ| amplified ×40
        let prefix = prefix.to_string_lossy().into_owned();
        let h = cpu.height;
        let enc = |v: f32| ((v.max(0.0) as f64 / peak).powf(1.0 / 2.2) * 255.0).round().min(255.0) as u8;
        let write = |name: &str, f: &dyn Fn(usize) -> u8| -> Result<()> {
            let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
            bytes.extend((0..cpu.data.len()).map(f));
            std::fs::write(format!("{prefix}-{name}.ppm"), bytes)?;
            Ok(())
        };
        write("cpu", &|i| enc(cpu.data[i]))?;
        write("gpu", &|i| enc(gpu.data[i]))?;
        write("diff", &|i| (((cpu.data[i] as f64 - gpu.data[i] as f64).abs() * 40.0).min(255.0)) as u8)?;
    }
    println!("columns mean |Δ|: {}", top(&col, cpu.height));
    println!("rows    mean |Δ|: {}", top(&row, w));
    Ok(())
}

/// `x3fuse <out.tif> [--denoiser=nlm|bm3d|off] [--luma] [--no-chroma] [--strength=0.1..3] [--backend=cpu|gpu|auto]`
/// Test bench: X3Fuse's own path. There, noise reduction (NLM or BM3D) runs as stage 5 on the
/// normalized sensor data, before highlight handling, color conversion (ColorDQ) and tone curve. Output:
/// sRGB TIFF. This is neither the SPP path nor our development; only for comparing where the noise is removed.
fn x3fuse_cmd(file: &str, out: &str, flags: &[String]) -> Result<()> {
    use x3f_core::{ColorEncoding, DenoiseAlgorithm, DenoiseBackend, DenoiseOptions, OutputFormat, ProcessOptions};
    let mut opts = ProcessOptions { color_encoding: ColorEncoding::Srgb, ..ProcessOptions::default() };
    let mut d = DenoiseOptions::DEFAULT;
    for a in flags {
        match a.split_once('=') {
            None if a == "--luma" => d.luminance = true,
            None if a == "--no-chroma" => d.chroma = false,
            Some(("--denoiser", "nlm")) => d.algorithm = DenoiseAlgorithm::Nlm,
            Some(("--denoiser", "bm3d")) => d.algorithm = DenoiseAlgorithm::Bm3d,
            Some(("--denoiser", "off")) => opts.denoise_intensity = 0,
            Some(("--strength", v)) => d.strength = v.parse().with_context(|| format!("number expected: {a}"))?,
            Some(("--backend", "cpu")) => opts.denoise_backend = DenoiseBackend::Cpu,
            Some(("--backend", "gpu")) => opts.denoise_backend = DenoiseBackend::Gpu,
            Some(("--backend", "auto")) => opts.denoise_backend = DenoiseBackend::Auto,
            // Sigma's exact Merrill path (SPP noise reduction, native output stage) and its output space
            None if a == "--spp" => {
                opts.spp_denoise = true;
                opts.rgb_chroma_lut = Some(false);
            }
            Some(("--color", "adobe")) => opts.color_encoding = ColorEncoding::AdobeRgb,
            Some(("--color", "prophoto")) => opts.color_encoding = ColorEncoding::ProPhotoRgb,
            _ => bail!("unknown option {a}"),
        }
    }
    opts.denoise_options = d;
    let _ = std::fs::remove_file(out);
    let t = Instant::now();
    x3f_core::convert_file(std::path::Path::new(file), std::path::Path::new(out), OutputFormat::Tiff, &opts, &std::sync::atomic::AtomicBool::new(false), |_| {}).map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("x3fuse path done in {:.2?}: {out} (denoise strength {}, {:?}, luma {}, chroma {})", t.elapsed(), if opts.denoise_intensity == 0 { "off".to_string() } else { format!("{}", d.strength) }, d.algorithm, d.luminance, d.chroma);
    Ok(())
}

/// Metadata of an exported JPEG/JPEG XL/TIFF/PNG as in the app (same function, without the library's personal fields).
pub(crate) fn export_metadata(out: &std::path::Path, original: &std::path::Path, format: develop::export::Format, opts: &develop::export::Options, shot: Option<library::export_meta::Shot>) {
    use develop::export::{Format, Metadata};
    if opts.metadata == Metadata::None || !matches!(format, Format::Jpeg { .. } | Format::Jxl { .. } | Format::Tiff16 | Format::Png16) {
        return;
    }
    let f = library::imageio::ExportFields { personal: opts.metadata == Metadata::All, gps: opts.gps, shot, ..Default::default() };
    match library::export_meta::write(out, original, &f) {
        Ok(r) => eprintln!("metadata: {r}"),
        Err(e) => eprintln!("metadata not written: {e}"),
    }
}

/// `develop <out.jpg|out.jxl|out.tif|out.dng> [--mono] [--ev=N] [--contrast=N] [--shadow=N] [--highlight=N]
/// [--mode=Portrait] [--mix=r,g,b] [--nr=off] [--luma=0..1] [--chroma=0..1] [--fill=-2..2]
/// [--layer=0|1|2] [--rawrgb] [--cbm3d=strength[,y]] [--einstellungen=FILE.json] [--baseline] [--metadata=none|shooting|all] [--gps] [--jxl=1..100|lossless|lossless8]`
fn develop_cmd(file: &str, out: &str, flags: &[String]) -> Result<()> {
    // whole-image export: binning of the sd Quattro at high ISO as in the camera maker's software
    develop::quattro::enable_binning(true);
    let mut s = develop::Settings::default();
    let mut engine = develop::export::Engine::Own;
    let mut profiles = false;
    let mut opts = develop::export::Options::default();
    let mut space = graineo_color::Space::Srgb;
    let mut forced = None;
    for a in flags {
        let num = |v: &str| -> Result<f64> { v.parse().with_context(|| format!("number expected: {a}")) };
        match a.split_once('=') {
            None if a == "--mono" => s.monochrome = true,
            // initial values as in the app for a photo without own settings (SPP/camera values in the X3F)
            None if a == "--baseline" => s = develop::sppa::baseline(file).unwrap_or_default(),
            // colour even if the camera was set to black and white (after --baseline)
            None if a == "--color" => s.monochrome = false,
            // settings as JSON (as in the sidecar); later options override individual values
            Some(("--settings", v)) => s = serde_json::from_str(&std::fs::read_to_string(v)?)?,
            None if a == "--rawrgb" => s.view = develop::View::RawComposite,
            None if a == "--spp" => engine = develop::export::Engine::Spp,
            None if a == "--no-binning" => s.binning = develop::Binning::Off,
            None if a == "--binning" => s.binning = develop::Binning::On,
            None if a == "--profiles" => profiles = true,
            None if a == "--float" => opts.dng_float = true,
            None if a == "--embed" => opts.dng_embed_original = true,
            Some(("--space", v)) => space = match v { "p3" => graineo_color::Space::DisplayP3, "adobe" => graineo_color::Space::AdobeRgb, "prophoto" => graineo_color::Space::ProPhotoRgb, _ => graineo_color::Space::Srgb },
            Some(("--compress", v)) => opts.tiff_compression = match v { "deflate" | "zip" => develop::tiffw::Compression::Deflate, "lzw" => develop::tiffw::Compression::Lzw, _ => develop::tiffw::Compression::None },
            Some(("--linear", v)) => opts.linear_space = match v { "acescg" => graineo_color::LinearSpace::AcesCg, "prophoto" => graineo_color::LinearSpace::ProPhoto, _ => graineo_color::LinearSpace::Rec2020 },
            Some(("--format", v)) => forced = Some(match v { "png" => develop::export::Format::Png16, "tiff-float" => develop::export::Format::TiffFloat, "exr" => develop::export::Format::Exr { half: true }, "exr-float" => develop::export::Format::Exr { half: false }, "layers" => develop::export::Format::RawLayers, _ => bail!("--format=png|tiff-float|exr|exr-float|layers") }),
            // JPEG XL (out.jxl): quality 1–100, "lossless" (16 bit) or "lossless8"
            Some(("--jxl", v)) => forced = Some(match v {
                "lossless" => develop::export::Format::Jxl { quality: 92, lossless: true, bits16: true },
                "lossless8" => develop::export::Format::Jxl { quality: 92, lossless: true, bits16: false },
                q => develop::export::Format::Jxl { quality: q.parse::<u8>().ok().filter(|q| (1..=100).contains(q)).context("--jxl=1..100|lossless|lossless8")?, lossless: false, bits16: true },
            }),
            // metadata of exported JPEG/TIFF/PNG: none | shooting (default) | all; --gps takes over the location
            Some(("--metadata", v)) => opts.metadata = develop::export::Metadata::parse(v).context("--metadata=none|shooting|all")?,
            None if a == "--gps" => opts.gps = true,
            Some(("--layer", v)) => s.view = develop::View::Layer(num(v)? as usize),
            Some(("--ev", v)) => s.exposure = num(v)?,
            // white-balance multipliers on top of the as-shot balance (as the pipette), e.g. after --baseline
            Some(("--wb-mult", v)) => {
                let w: Vec<f32> = v.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(w.len() == 3, "--wb-mult=r,g,b");
                s.wb_mult = [w[0], w[1], w[2]];
            }
            Some(("--contrast", v)) => s.contrast = num(v)?,
            Some(("--shadow", v)) => s.shadow = num(v)?,
            Some(("--highlight", v)) => s.highlight = num(v)?,
            Some(("--mode", v)) => s.color_mode = Some(v.to_string()),
            Some(("--fill", v)) => s.fill_light = num(v)? as f32,
            // alternative Merrill parts (test switch, see x3f_core::merrill_new): --new=sharpness,highlights,…
            Some(("--new", v)) => std::env::set_var(develop::merrill_new::VAR, v),
            Some(("--sharpness", v)) => s.sigma_sharpness = num(v)? as f32,
            Some(("--hc", v)) => s.highlight_neutralize = num(v)? as f32,
            // SPP fringe band: --saum=gruen|magenta:hue,range,amount
            Some(("--saum", v)) => {
                let (band, w) = v.split_once(':').context("--saum=gruen|magenta:hue,range,amount")?;
                let w: Vec<f32> = w.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(w.len() == 3, "--saum=gruen|magenta:hue,range,amount");
                let b = Some(develop::spp_defringe::Band { hue: w[0], range: w[1], amount: w[2] });
                if band == "gruen" { s.spp_defringe.green = b } else { s.spp_defringe.magenta = b }
            }
            // CA: --ca=profil[=amount] or --ca=manuell=red,blue
            Some(("--ca", v)) => match v.split_once('=') {
                None if v == "profile" => s.ca_mode = develop::lens::CaMode::Profile,
                Some(("profile", a)) => (s.ca_mode, s.ca_profile) = (develop::lens::CaMode::Profile, a.parse()?),
                Some(("manual", rb)) => {
                    let (r, b) = rb.split_once(',').context("--ca=manuell=red,blue")?;
                    (s.ca_mode, s.ca_red, s.ca_blue) = (develop::lens::CaMode::Manual, r.parse()?, b.parse()?);
                }
                _ => bail!("--ca=profil[=amount] | manuell=red,blue"),
            },
            Some(("--nr", "0" | "off")) => s.denoise = false,
            Some(("--luma", v)) => s.nr_luma = num(v)?,
            Some(("--chroma", v)) => s.nr_chroma = num(v)?,
            // prototype color BM3D on the SPP base: --cbm3d=strength[,y]
            Some(("--cbm3d", v)) => {
                std::env::set_var("GRAINEO_CHROMA_BM3D", v);
                std::env::set_var("GRAINEO_NO_DISK_CACHE", "1");
            }
            // tone curve: --curve=x:y,x:y,… (RGB), --curve-r/-g/-b for the channels
            Some((k @ ("--curve" | "--curve-r" | "--curve-g" | "--curve-b"), v)) => {
                let pts = v
                    .split(',')
                    .map(|p| {
                        let (x, y) = p.split_once(':').context("point as x:y")?;
                        Ok([x.parse()?, y.parse()?])
                    })
                    .collect::<Result<Vec<[f32; 2]>>>()?;
                let i = ["--curve", "--curve-r", "--curve-g", "--curve-b"].iter().position(|n| *n == k).unwrap();
                *s.tone_curve.channel_mut(i) = pts;
            }
            Some(("--mix", v)) => {
                let w: Vec<f32> = v.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(w.len() == 3, "--mix=r,g,b");
                s.mix = [w[0], w[1], w[2]];
            }
            // layer mixer: preset (identity|nrg|swap) or 9 values row by row R,G,B
            Some(("--layers", v)) => {
                s.layer_matrix = match v {
                    "identity" => Some(develop::layers::IDENTITY),
                    "nrg" => Some(develop::layers::INFRARED_NRG),
                    "swap" => Some(develop::layers::SWAP_RB),
                    _ => {
                        let w: Vec<f32> = v.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
                        anyhow::ensure!(w.len() == 9, "--layers=identity|nrg|swap|9 values");
                        Some(std::array::from_fn(|r| std::array::from_fn(|c| w[r * 3 + c])))
                    }
                };
            }
            _ => bail!("unknown option {a}"),
        }
    }
    let path = std::path::Path::new(out);
    let mut format = develop::export::Format::from_path(path).context("unknown output format (jpg, jxl, tif, png, dng)")?;
    if profiles && format == develop::export::Format::Dng {
        format = develop::export::Format::DngProfiles;
    }
    if let Some(f) = forced {
        format = f;
    }
    let _ = std::fs::remove_file(path);
    let t = Instant::now();
    let mut session = develop::Session::open_for(file, &s)?;
    let cal = &session.cal;
    eprintln!(
        "{} ({:?}, count factor {:.4}, psat {}, color space {}) · WB {} · color mode {} · black {:.1}/{:.1}/{:.1} · spatial gain {} · opened in {:.2?}",
        cal.camera, cal.model, cal.count_scale, cal.pixel_saturation,
        if cal.output_to_srgb.is_some() { "Adobe RGB" } else { "sRGB" },
        cal.wb_name, cal.mode(s.color_mode.as_deref()).name, cal.black[0], cal.black[1], cal.black[2],
        if cal.spatial.is_some() { "yes" } else { "no" }, t.elapsed()
    );
    let spp = develop::export::color::export(&mut session, &s, format, engine, path, space, &opts)?;
    export_metadata(path, std::path::Path::new(file), format, &opts, None);
    let (quattro, classic) = (session.cal.is_quattro(), session.cal.is_classic());
    eprintln!(
        "Pipeline: {}",
        match (spp, quattro, classic) {
            (true, true, _) => "Quattro base (x3fuse-core processing)",
            (true, _, true) => "Classic base (x3fuse-core processing)",
            (true, ..) => "x3fuse-core (Sigma base)",
            (false, ..) => "own",
        }
    );
    // measurements with a warm disk cache: wait for the background store (`GRAINEO_CACHE_WAIT=1`)
    if std::env::var_os("GRAINEO_CACHE_WAIT").is_some() {
        develop::diskcache::wait_for_memory(std::time::Duration::from_secs(120));
    }
    Ok(())
}

/// Compares our development (default settings) with the Sigma base from x3fuse-core.
/// Optional: folder into which both images are written as JPEG.
fn compare(file: &str, out_dir: Option<&str>) -> Result<()> {
    let path = std::path::Path::new(file);
    let t = Instant::now();
    let reference = develop::spp::render(path, None).map_err(anyhow::Error::msg)?;
    let t_ref = t.elapsed();
    let t = Instant::now();
    let mut session = develop::Session::open(path)?;
    let mut s = develop::Settings::default();
    if std::env::var_os("GRAINEO_COMPARE_NONR").is_some() {
        s.denoise = false;
    }
    // GRAINEO_COMPARE_OWN: only our own pipeline (otherwise render_full uses the SPP base
    // as soon as x3fuse-core supports the file)
    let ours = if std::env::var_os("GRAINEO_COMPARE_OWN").is_some() {
        session.render_full_own(&s)
    } else if std::env::var_os("GRAINEO_COMPARE_COUNTS").is_some() {
        let counts = develop::spp::working(path, None).map_err(anyhow::Error::msg)?;
        let m = session.cal.mode(None).cam_to_srgb;
        develop::render_counts(&counts, &session.cal, &s, &m, None)
    } else {
        session.render_full(&s)
    };
    let t_ours = t.elapsed();
    anyhow::ensure!(
        (ours.width, ours.height) == (reference.width, reference.height),
        "sizes differ: {}×{} vs {}×{}",
        ours.width, ours.height, reference.width, reference.height
    );
    // deviation in 8-bit steps per channel and by brightness of the reference
    let mut se = 0f64;
    let mut bias = [0f64; 3];
    let mut zone = [[0f64; 4]; 5];
    for (a, b) in ours.data.chunks_exact(3).zip(reference.data.chunks_exact(3)) {
        let lum = (b[0] as f64 * 0.2126 + b[1] as f64 * 0.7152 + b[2] as f64 * 0.0722) / 65535.0;
        let z = ((lum * 5.0) as usize).min(4);
        for c in 0..3 {
            let d = (a[c] as f64 - b[c] as f64) / 257.0;
            se += d * d;
            bias[c] += d;
            zone[z][c] += d;
        }
        zone[z][3] += 1.0;
    }
    let n = (ours.data.len() / 3) as f64;
    let mse = se / (n * 3.0);
    println!("reference (x3fuse-core SPP) {t_ref:.2?} · ours {t_ours:.2?}");
    println!("PSNR {:.2} dB · mean deviation R {:+.2} G {:+.2} B {:+.2} (8-bit steps)",
        10.0 * (255.0f64 * 255.0 / mse).log10(), bias[0] / n, bias[1] / n, bias[2] / n);
    for (i, z) in zone.iter().enumerate() {
        if z[3] > 0.0 {
            println!("  brightness {:>3}–{:>3} %: {:>5.1} % of pixels · R {:+6.2} G {:+6.2} B {:+6.2}",
                i * 20, i * 20 + 20, z[3] / n * 100.0, z[0] / z[3], z[1] / z[3], z[2] / z[3]);
        }
    }
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir)?;
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        develop::export::write_jpeg(&ours, &std::path::Path::new(dir).join(format!("{stem}_ours.jpg")), 90)?;
        develop::export::write_jpeg(&reference, &std::path::Path::new(dir).join(format!("{stem}_spp.jpg")), 90)?;
    }
    Ok(())
}

fn read_tiff16(path: &str) -> Result<(u32, u32, Vec<u16>)> {
    let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(File::open(path)?))?;
    let (w, h) = dec.dimensions()?;
    match dec.read_image()? {
        tiff::decoder::DecodingResult::U16(v) => Ok((w, h, v)),
        _ => bail!("{path}: not a 16-bit TIFF"),
    }
}

/// PSNR and mean deviation of two 16-bit RGB TIFFs (8-bit steps).
fn psnr(a: &str, b: &str) -> Result<()> {
    let (wa, ha, da) = read_tiff16(a)?;
    let (wb, hb, db) = read_tiff16(b)?;
    anyhow::ensure!((wa, ha) == (wb, hb), "sizes differ: {wa}×{ha} vs {wb}×{hb}");
    let (mut se, mut bias) = (0f64, [0f64; 3]);
    for (i, (x, y)) in da.iter().zip(&db).enumerate() {
        let d = (*x as f64 - *y as f64) / 257.0;
        se += d * d;
        bias[i % 3] += d;
    }
    let n = da.len() as f64;
    println!(
        "PSNR {:.2} dB · mean deviation R {:+.2} G {:+.2} B {:+.2}",
        10.0 * (255.0f64 * 255.0 / (se / n)).log10(),
        bias[0] / n * 3.0,
        bias[1] / n * 3.0,
        bias[2] / n * 3.0
    );
    Ok(())
}

/// Show settings saved by SPP (reads only the header, directory, SPPA, PROP).
fn sppa(file: &str, out: Option<&str>) -> Result<()> {
    let Some(a) = x3f::sppa::read_path(file) else {
        bail!("{file}: no readable X3F header");
    };
    println!("{a:#?}");
    match develop::sppa::to_settings(&a) {
        Some(s) => {
            let json = serde_json::to_string_pretty(&s)?;
            println!("settings for graineo:\n{json}");
            if let Some(out) = out {
                std::fs::write(out, json)?;
            }
        }
        None => println!("No settings saved by SPP (camera values only)."),
    }
    Ok(())
}

fn info(x3f: &X3fFile) -> Result<()> {
    println!("X3F version {:#x}", x3f.version);
    for e in &x3f.entries {
        let kind = String::from_utf8_lossy(&e.kind);
        match x3f.image_header(e) {
            Ok(h) => println!(
                "  {kind} @ {:>9}  {:>9} B  type={} format={} {}×{}",
                e.offset, e.length, h.image_type, h.format, h.columns, h.rows
            ),
            Err(_) => println!("  {kind} @ {:>9}  {:>9} B", e.offset, e.length),
        }
    }
    if let Ok(props) = x3f.properties() {
        println!("\nproperties:");
        for (k, v) in &props.pairs {
            println!("  {k:<20} {v}");
        }
    }
    let camf = x3f.camf()?;
    println!("\nCAMF: {} entries", camf.order.len());
    Ok(())
}

fn camf(x3f: &X3fFile, names: &[String]) -> Result<()> {
    let camf = x3f.camf()?;
    // GRAINEO_CAMF_ALL=1: all values instead of the first 12
    let take = if std::env::var_os("GRAINEO_CAMF_ALL").is_some() { usize::MAX } else { 12 };
    let selected: Vec<&String> = if names.is_empty() {
        camf.order.iter().collect()
    } else {
        names.iter().collect()
    };
    for name in selected {
        match camf.entries.get(name.as_str()) {
            None => println!("{name}: <missing>"),
            Some(CamfEntry::Text(t)) => println!("{name} (Text): {t:?}"),
            Some(CamfEntry::Properties(p)) => {
                println!("{name} (properties):");
                for (k, v) in p {
                    println!("    {k} = {v}");
                }
            }
            Some(CamfEntry::Matrix(m)) => {
                let dims: Vec<String> = m.dims.iter().map(|(n, s)| format!("{n}={s}")).collect();
                let preview: Vec<String> = match &m.data {
                    MatrixData::Float(v) => v.iter().take(take).map(|x| format!("{x:.6}")).collect(),
                    MatrixData::Int(v) => v.iter().take(take).map(|x| x.to_string()).collect(),
                    MatrixData::UInt(v) => v.iter().take(take).map(|x| x.to_string()).collect(),
                };
                let more = if m.len() > take { " …" } else { "" };
                println!(
                    "{name} (matrix type {}, [{}], {} values): {}{more}",
                    m.element_type,
                    dims.join(", "),
                    m.len(),
                    preview.join(" ")
                );
            }
        }
    }
    Ok(())
}

fn raw(x3f: &X3fFile, out: &str) -> Result<()> {
    let t = Instant::now();
    let img = x3f.decode_raw()?;
    eprintln!("RAW decoded: {}×{} in {:.2?}", img.columns, img.rows, t.elapsed());
    let data = img.interleaved();
    let max = data.iter().copied().max().unwrap_or(0);
    let min = data.iter().copied().min().unwrap_or(0);
    eprintln!("value range {min}..{max}");
    for (c, p) in img.planes.iter().enumerate() {
        let mut v = p.clone();
        v.sort_unstable();
        let q = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
        let top = v[v.len() - 1];
        let at_top = v.iter().rev().take_while(|&&x| x + 8 >= top).count();
        if let Some(r) = x3f.camf().ok().and_then(|cm| cm.matrix("DarkShieldTop").ok().map(|m| m.as_f64())).filter(|r| r.len() == 4) {
            let (mut sum, mut n) = (0f64, 0f64);
            for y in r[1] as usize..=r[3] as usize {
                for x in r[0] as usize..=r[2] as usize {
                    sum += p[y * img.columns + x] as f64;
                    n += 1.0;
                }
            }
            eprintln!("plane {c}: DarkShieldTop mean {:.1}", sum / n);
        }
        eprintln!("plane {c}: p1 {} p50 {} p99 {} p99.9 {} max {top} (≥ max−8: {at_top})", q(0.01), q(0.5), q(0.99), q(0.999));
    }
    let mut enc = TiffEncoder::new(BufWriter::new(File::create(out)?))?;
    enc.write_image::<colortype::RGB16>(img.columns as u32, img.rows as u32, &data)?;
    Ok(())
}

/// `defringe <X3F> <prefix> [--purple=0..1] [--green=0..1] [--region=x,y,w,h]… [--top=n] [--zoom=n]`:
/// measures the fringe removal at full resolution and in the preview, checks that the detail
/// (render_region) matches the full image, and writes crops before | after as
/// 8-bit TIFF (`<prefix>-<n>.tif`). Without `--region` the `n` tiles with the strongest
/// effect are chosen.
fn defringe_cmd(file: &str, prefix: &str, flags: &[String]) -> Result<()> {
    let mut s = develop::Settings::default();
    let (mut regions, mut top, mut zoom) = (Vec::<[usize; 4]>::new(), 4usize, 2usize);
    for a in flags {
        let num = |v: &str| -> Result<f64> { v.parse().with_context(|| format!("number expected: {a}")) };
        match a.split_once('=') {
            Some(("--purple", v)) => s.defringe_purple_amount = num(v)? as f32,
            Some(("--green", v)) => s.defringe_green_amount = num(v)? as f32,
            Some(("--purple-hue", v)) | Some(("--green-hue", v)) => {
                let w: Vec<f32> = v.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(w.len() == 2, "hue as from,to");
                if a.starts_with("--purple") { s.defringe_purple_hue = [w[0], w[1]] } else { s.defringe_green_hue = [w[0], w[1]] }
            }
            Some(("--region", v)) => {
                let w: Vec<usize> = v.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(w.len() == 4, "--region=x,y,w,h");
                regions.push([w[0], w[1], w[2], w[3]]);
            }
            Some(("--top", v)) => top = num(v)? as usize,
            Some(("--zoom", v)) => zoom = (num(v)? as usize).max(1),
            _ => bail!("unknown option {a}"),
        }
    }
    let d = develop::Settings::default();
    let mut session = develop::Session::open(file)?;
    let before = session.render_full(&d);
    let (w, h) = (before.width, before.height);
    let geo = develop::look::Geo::full(w, h);
    let cp = |i: &develop::Rgb16| develop::Rgb16 { width: i.width, height: i.height, data: i.data.clone() };
    let mut times = Vec::new();
    let mut after = cp(&before);
    for _ in 0..3 {
        after = cp(&before);
        let t = Instant::now();
        develop::defringe::apply(&mut after, &s, geo);
        times.push(t.elapsed());
    }
    let changed = before.data.chunks(3).zip(after.data.chunks(3)).filter(|(a, b)| a != b).count();
    eprintln!("full resolution {w}×{h}: {:.1?} / {:.1?} / {:.1?}, changed {:.3} % of pixels", times[0], times[1], times[2], 100.0 * changed as f64 / (w * h) as f64);
    // preview (2000 px) from the SPP base
    if let (Some(base), Some(m)) = (session.spp_base(), session.spp_matrix()) {
        let p = develop::session::Preview::from_spp(&base, m, 2000);
        let img = p.render(&session.cal, &d);
        let geo_p = develop::look::Geo { full_w: w, full_h: h, off_x: 0, off_y: 0, scale: p.factor as f32, camera: 0 };
        let mut best = f64::MAX;
        for _ in 0..5 {
            let mut c = cp(&img);
            let t = Instant::now();
            develop::defringe::apply(&mut c, &s, geo_p);
            best = best.min(t.elapsed().as_secs_f64());
        }
        eprintln!("preview {}×{}: {:.2} ms", img.width, img.height, best * 1e3);
    }
    if regions.is_empty() {
        // tiles with the largest sum of colorfulness change
        let tile = 200usize;
        let mut scores = Vec::new();
        for ty in 0..h / tile {
            for tx in 0..w / tile {
                let mut sum = 0u64;
                for y in ty * tile..(ty + 1) * tile {
                    for x in tx * tile..(tx + 1) * tile {
                        let i = (y * w + x) * 3;
                        let ch = |p: &[u16]| (*p.iter().max().unwrap() - *p.iter().min().unwrap()) as i64;
                        sum += (ch(&before.data[i..i + 3]) - ch(&after.data[i..i + 3])).max(0) as u64;
                    }
                }
                scores.push((sum, tx * tile, ty * tile));
            }
        }
        scores.sort_by(|a, b| b.0.cmp(&a.0));
        for &(_, x, y) in scores.iter().take(top) {
            regions.push([x, y, tile, tile]);
        }
    }
    for (n, &[x, y, rw, rh]) in regions.iter().enumerate() {
        let (rw, rh) = (rw.min(w - x), rh.min(h - y));
        // crop as in the loupe: must match the full image except for the border (radius)
        let reg = session.render_region(&s, [x, y, rw, rh]);
        let mut maxd = 0u16;
        let m = 20;
        for yy in m..rh.saturating_sub(m) {
            for xx in m..rw.saturating_sub(m) {
                for c in 0..3 {
                    let a = after.data[((y + yy) * w + x + xx) * 3 + c];
                    let b = reg.data[(yy * rw + xx) * 3 + c];
                    maxd = maxd.max(a.abs_diff(b));
                }
            }
        }
        let (ow, oh) = (rw * zoom * 2 + 4, rh * zoom);
        let mut out = vec![40u8; ow * oh * 3];
        for (k, img) in [&before, &after].iter().enumerate() {
            for oy in 0..oh {
                for ox in 0..rw * zoom {
                    let i = ((y + oy / zoom) * w + x + ox / zoom) * 3;
                    let o = (oy * ow + k * (rw * zoom + 4) + ox) * 3;
                    for c in 0..3 {
                        out[o + c] = (img.data[i + c] >> 8) as u8;
                    }
                }
            }
        }
        let path = format!("{prefix}-{n}.tif");
        let mut enc = TiffEncoder::new(BufWriter::new(File::create(&path)?))?;
        enc.write_image::<colortype::RGB8>(ow as u32, oh as u32, &out)?;
        eprintln!("{path}: crop {x},{y} {rw}×{rh}, crop↔full image max. deviation (without {m} px border) {maxd}");
    }
    Ok(())
}

/// Write all stages of the pipeline inspection (preview size as in the app) and their difference images
/// as JPEG, with run time per image.
fn stages_cmd(file: &str, dir: &str, look: bool) -> Result<()> {
    use develop::inspect::{Sources, Stage};
    let mut session = develop::Session::open(file)?;
    let mut s = develop::Settings::default();
    if look {
        s.clarity = 0.5;
        s.vignette = -0.5;
    }
    let (small, f) = session.preview_linear(2400);
    let sensor = develop::session::Preview::plain(small.clone(), f);
    let developed = develop::session::Preview::developed(small, f, &session.cal, &s);
    let spp = session.ensure_spp(&s).is_ok().then(|| {
        let m = session.spp_matrix().unwrap();
        develop::session::Preview::from_spp(&session.spp_base().unwrap(), m, 2400)
    });
    let raw = session.raw_shared();
    let src = Sources { raw: &raw, sensor: &sensor, developed: &developed, spp: spp.as_ref() };
    std::fs::create_dir_all(dir)?;
    for (i, stage) in Stage::ALL.into_iter().enumerate() {
        for diff in [false, true] {
            if diff && stage.diff_label().is_none() {
                continue;
            }
            let t = Instant::now();
            let st = develop::Settings { view: develop::View::Stage { stage, diff, gain: 3 }, ..s.clone() };
            let img = develop::inspect::render(&src, &session.cal, &st);
            let ms = t.elapsed();
            let out = format!("{dir}/{}{}.jpg", i + 1, if diff { "d" } else { "" });
            image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).context("image size")?.save(&out)?;
            println!("{out}: {}×{} in {ms:.0?}", img.width, img.height);
        }
    }
    Ok(())
}

/// `wide-stat FILE…`: share of preview pixels that, computed as Display P3, lie outside sRGB (measure of the gain from the
/// wide display), and compute time of the wide preview.
fn wide_stat(files: &[String]) -> Result<()> {
    use graineo_color::Space;
    for f in files {
        if f.to_ascii_lowercase().ends_with(".x3f") {
            wide_stat_sigma(f)?;
            continue;
        }
        let Some(doc) = rawdev::RawDoc::open(std::path::Path::new(f), &|| true).map_err(|e| anyhow::anyhow!("{f}: {e}"))? else { continue };
        let mut s = develop::Settings::default();
        s.raw.lens = false;
        let t = Instant::now();
        let Some(b) = rawdev::color::render_preview_wide(&doc, &s, Space::DisplayP3).map_err(|e| anyhow::anyhow!("{f}: {e}"))? else { continue };
        let ms = t.elapsed().as_millis();
        let srgb = graineo_color::convert_linear(&b.rgb, Space::DisplayP3, Space::Srgb).map_err(|e| anyhow::anyhow!("{e}"))?;
        let n = srgb.len() / 3;
        let out = srgb.chunks_exact(3).filter(|p| p.iter().any(|v| *v < -0.002 || *v > 1.002)).count();
        let strong = srgb.chunks_exact(3).filter(|p| p.iter().any(|v| *v < -0.05 || *v > 1.05)).count();
        println!("{f}: {}×{}, outside sRGB {:.2} % (clearly, above 5 %: {:.2} %), {ms} ms", b.width, b.height, out as f64 * 100.0 / n as f64, strong as f64 * 100.0 / n as f64);
    }
    Ok(())
}

/// `wide-stat` for a Sigma file: the display preview (longer edge 2400) from Sigma's exact base and from the own pipeline,
/// share of pixels outside sRGB as Display P3, compute time of the sRGB and of the wide version (median of 5).
fn wide_stat_sigma(f: &str) -> Result<()> {
    use graineo_color::Space;
    let session = develop::Session::open(std::path::Path::new(f)).map_err(|e| anyhow::anyhow!("{f}: {e}"))?;
    let s = develop::Settings::default();
    let cal = session.cal.clone();
    let base = session.ensure_spp(&s).map_err(|e| anyhow::anyhow!("{f}: {e}"))?;
    let spp = develop::session::Preview::from_spp(&base, base.matrix(&cal), 2400);
    let (lin, factor) = session.preview_linear(2400);
    let own = develop::session::Preview::developed(lin, factor, &cal, &s);
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    for (name, p) in [("SPP base", &spp), ("own pipeline", &own)] {
        let mut t_sdr = Vec::new();
        let mut t_wide = Vec::new();
        let mut last = None;
        for _ in 0..5 {
            let t = Instant::now();
            let _ = p.render(&cal, &s);
            t_sdr.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let w = p.render_wide(&cal, &s).map_err(|e| anyhow::anyhow!("{e}"))?.context("no wide version")?;
            let linear: Vec<f32> = w.data.iter().map(|&v| graineo_color::srgb_decode(v)).collect();
            let p3 = graineo_color::convert_linear(&linear, Space::Srgb, Space::DisplayP3).map_err(|e| anyhow::anyhow!("{e}"))?;
            t_wide.push(t.elapsed().as_secs_f64() * 1000.0);
            last = Some((w.width, w.height, p3));
        }
        let (w, h, p3) = last.unwrap();
        let srgb = graineo_color::convert_linear(&p3, Space::DisplayP3, Space::Srgb).map_err(|e| anyhow::anyhow!("{e}"))?;
        let n = srgb.len() / 3;
        let out = srgb.chunks_exact(3).filter(|p| p.iter().any(|v| *v < -0.002 || *v > 1.002)).count();
        let strong = srgb.chunks_exact(3).filter(|p| p.iter().any(|v| *v < -0.05 || *v > 1.05)).count();
        let p3_out = p3.chunks_exact(3).filter(|p| p.iter().any(|v| *v < -0.002 || *v > 1.002)).count();
        println!(
            "{f} [{name}]: {w}×{h}, outside sRGB {:.2} % (clearly, above 5 %: {:.2} %; outside P3 too: {:.2} %), sRGB preview {:.0} ms, wide {:.0} ms (median of 5)",
            out as f64 * 100.0 / n as f64,
            strong as f64 * 100.0 / n as f64,
            p3_out as f64 * 100.0 / n as f64,
            median(t_sdr),
            median(t_wide)
        );
    }
    Ok(())
}

/// `auto-data DATA.json OUT.json [count]`: for photos with known Lightroom auto values (from the catalog history, read only), compute the
/// image features and the result of our automatic tone (⌘U) – basis for comparison and tuning.
fn auto_data(args: &[String]) -> Result<()> {
    use rayon::prelude::*;
    let data: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(args.first().context("DATA.json missing")?)?)?;
    let out = args.get(1).context("OUT.json missing")?;
    let max: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let outcome: Vec<serde_json::Value> = pool.install(|| {
        data
            .par_iter()
            .take(max)
            .filter(|d| d["da"].as_bool().unwrap_or(false))
            .filter_map(|d| {
                let nas = d["nas"].as_str()?;
                let doc = rawdev::RawDoc::open(std::path::Path::new(nas), &|| true).ok()??;
                let f = doc.auto_features()?;
                let mut s = develop::Settings::default();
                doc.auto_tone(&mut s);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 50 == 0 {
                    eprintln!("{n} done");
                }
                Some(serde_json::json!({"file": d["file"], "ext": d["ext"], "step": d["step"], "lr": d["values"], "ours": {"exposure": s.exposure, "highlight": s.highlight, "shadow": s.shadow}, "f": f}))
            })
            .collect()
    });
    std::fs::write(out, serde_json::to_string(&outcome)?)?;
    println!("{} photos evaluated", outcome.len());
    Ok(())
}

/// `lr-compare DATA.json OUT.json [count]`: for photos with a RAW (NAS, read only) and a reference export (`jpg`), compute the development with the
/// starting values from Lightroom as in the app and compare with the export (ΔE00, midtone exposure, L*, chroma).
fn lr_compare(args: &[String]) -> Result<()> {
    use rayon::prelude::*;
    let data: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(args.first().context("DATA.json missing")?)?)?;
    let out = args.get(1).context("OUT.json missing")?;
    let max: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let catalog = crate::dev_catalog();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let outcome: Vec<serde_json::Value> = pool.install(|| {
        data
            .par_iter()
            .take(max)
            .filter_map(|d| {
                let nas = d["nas"].as_str()?;
                let mut doc = rawdev::RawDoc::open(std::path::Path::new(nas), &|| true).ok()??;
                doc.logical_path = std::path::PathBuf::from(d["lr_path"].as_str()?);
                rawdev::lightroom::apply_orientation_from_catalog(&mut doc, &catalog);
                let s = rawdev::lightroom::initial_settings_from_catalog(&doc, &catalog).unwrap_or_default();
                let img = develop::geometry::apply(doc.render_preview(&s), &s, doc.orientation);
                let own = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8())?;
                let reference = image::open(d["jpg"].as_str()?).ok()?.to_rgb8();
                let e = raw_compare::compare_images(&own, &reference, false);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 50 == 0 {
                    eprintln!("{n} done");
                }
                Some(serde_json::json!({"file": d["file"], "ext": d["ext"], "model": doc.info.model, "dr": doc.info.fuji_dr, "iso": doc.info.exif.iso, "base_ev": doc.base_ev, "strength": d["strength"], "de": e.de, "zone": e.de_zone, "dl_zone": e.dl_zone, "ev_mitten": e.ev_mid, "ev": e.ev, "dl": e.dl, "da": e.da, "db": e.db, "dc": e.dc,
                    "sliders": {"exposure": s.exposure, "contrast": s.contrast, "highlight": s.highlight, "shadow": s.shadow, "whites": s.raw.whites, "blacks": s.raw.blacks, "clarity": s.clarity, "vibrance": s.vibrance, "saturation": s.saturation, "look": s.raw.look}}))
            })
            .collect()
    });
    std::fs::write(out, serde_json::to_string(&outcome)?)?;
    println!("{} photos compared", outcome.len());
    Ok(())
}

/// Factors and exponents used to adjust the sliders imported from Lightroom before rendering: highlights, shadows, whites,
/// blacks, contrast, exposure (factor), exposure (offset in EV), clarity, highlights exponent, shadows exponent.
const K_NAMES: [&str; 10] = ["highlight×", "shadow×", "whites×", "blacks×", "contrast×", "exposure×", "exposure+", "clarity×", "highlight^", "shadow^"];
const K_START: [f64; 10] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0];

fn pot(v: f64, k: f64, p: f64) -> f64 {
    v.signum() * k * v.abs().powf(p)
}

fn apply_k(s: &develop::Settings, k: &[f64; 10]) -> develop::Settings {
    let mut s = s.clone();
    s.highlight = pot(s.highlight, k[0], k[8]);
    s.shadow = pot(s.shadow, k[1], k[9]);
    s.raw.whites = (s.raw.whites as f64 * k[2]) as f32;
    s.raw.blacks = (s.raw.blacks as f64 * k[3]) as f32;
    s.contrast *= k[4];
    s.exposure = s.exposure * k[5] + k[6];
    s.clarity = (s.clarity as f64 * k[7]) as f32;
    s
}

struct CalibrationSample {
    doc: rawdev::RawDoc,
    start: develop::Settings,
    reference: image::RgbImage,
    /// 0 unedited, 1 light, 2 medium, 3 heavy
    class: usize,
}

fn class_of(strength: f64) -> usize {
    match strength {
        s if s < 1.0 => 0,
        s if s < 30.0 => 1,
        s if s < 80.0 => 2,
        _ => 3,
    }
}

fn load_calibration(d: &serde_json::Value, catalog: &std::path::Path) -> Option<CalibrationSample> {
    let mut doc = rawdev::RawDoc::open(std::path::Path::new(d["nas"].as_str()?), &|| true).ok()??;
    doc.logical_path = std::path::PathBuf::from(d["lr_path"].as_str()?);
    rawdev::lightroom::apply_orientation_from_catalog(&mut doc, catalog);
    let start = rawdev::lightroom::initial_settings_from_catalog(&doc, catalog).unwrap_or_default();
    let r = image::open(d["jpg"].as_str()?).ok()?.to_rgb8();
    let k = 900.0 / r.width().max(r.height()) as f32;
    let reference = if k < 1.0 { image::imageops::resize(&r, (r.width() as f32 * k) as u32, (r.height() as f32 * k) as u32, image::imageops::FilterType::Triangle) } else { r };
    let _ = doc.preview_stage(&start);
    Some(CalibrationSample { doc, start, reference, class: class_of(d["strength"].as_f64().unwrap_or(0.0)) })
}

/// Mean ΔE per class (unedited, light, medium, heavy).
fn calibration_score(samples: &[CalibrationSample], k: &[f64; 10]) -> [f64; 4] {
    use rayon::prelude::*;
    let r: Vec<(usize, f64)> = samples
        .par_iter()
        .map(|c| {
            let s = apply_k(&c.start, k);
            let img = develop::geometry::apply(c.doc.render_preview(&s), &s, c.doc.orientation);
            let own = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).unwrap();
            (c.class, raw_compare::compare_images(&own, &c.reference, false).de as f64)
        })
        .collect();
    std::array::from_fn(|class_idx| {
        let v: Vec<f64> = r.iter().filter(|(c, d)| *c == class_idx && d.is_finite()).map(|(_, d)| *d).collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    })
}

fn calibration_objective(w: &[f64; 4]) -> f64 {
    // edited classes count fully, unedited ones half (they must not get worse)
    (w[1] + w[2] + w[3] + 0.5 * w[0]) / 3.5
}

/// `lr-calibrate DATA.json [photos per class]`: searches factors and exponents for the sliders imported from Lightroom that bring the
/// development closer to the reference exports (ΔE00). Trains on one half of the photos and checks on the other.
/// Classes by strength of editing: unedited, light, medium, heavy.
fn lr_calibrate(args: &[String]) -> Result<()> {
    use rayon::prelude::*;
    let data: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(args.first().context("DATA.json missing")?)?)?;
    let per_class: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    let start_k: Vec<f64> = std::env::var("GRAINEO_CALIB_START").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    let catalog = crate::dev_catalog();
    let (mut train, mut test) = (Vec::new(), Vec::new());
    for class_idx in 0..4 {
        let l: Vec<&serde_json::Value> = data.iter().filter(|d| class_of(d["strength"].as_f64().unwrap_or(0.0)) == class_idx).collect();
        for (i, d) in l.iter().enumerate() {
            if i % 2 == 0 && train.iter().filter(|x: &&&serde_json::Value| class_of(x["strength"].as_f64().unwrap_or(0.0)) == class_idx).count() < per_class { train.push(*d) }
            if i % 2 == 1 && test.iter().filter(|x: &&&serde_json::Value| class_of(x["strength"].as_f64().unwrap_or(0.0)) == class_idx).count() < per_class { test.push(*d) }
        }
    }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let load = |l: &[&serde_json::Value]| -> Vec<CalibrationSample> { pool.install(|| l.par_iter().filter_map(|d| load_calibration(d, &catalog)).collect()) };
    eprintln!("loading {} training photos …", train.len());
    let samples = load(&train);
    let mut k = K_START;
    for (i, v) in start_k.iter().enumerate().take(10) {
        k[i] = *v;
    }
    let mut best_w = pool.install(|| calibration_score(&samples, &k));
    let mut best = calibration_objective(&best_w);
    eprintln!("start: ΔE unedited {:.2}, light {:.2}, medium {:.2}, heavy {:.2}", best_w[0], best_w[1], best_w[2], best_w[3]);
    let mut step = [0.25, 0.25, 0.25, 0.25, 0.25, 0.05, 0.05, 0.5, 0.15, 0.15];
    for round_no in 0..4 {
        for i in 0..10 {
            if i == 3 || i == 5 || i == 6 {
                continue; // blacks barely matter, exposure stays as it is (unedited photos must not shift)
            }
            for direction in [1.0, -1.0] {
                loop {
                    let mut k2 = k;
                    k2[i] *= 1.0 + direction * step[i];
                    let w = pool.install(|| calibration_score(&samples, &k2));
                    if calibration_objective(&w) < best - 1e-4 {
                        k = k2;
                        best = calibration_objective(&w);
                        best_w = w;
                        eprintln!("  round {round_no}: {} → {:.3}   ΔE unedited {:.2}, light {:.2}, medium {:.2}, heavy {:.2}", K_NAMES[i], k[i], w[0], w[1], w[2], w[3]);
                    } else {
                        break;
                    }
                }
            }
            step[i] *= 0.6;
        }
    }
    println!("training: ΔE unedited {:.2}, light {:.2}, medium {:.2}, heavy {:.2}", best_w[0], best_w[1], best_w[2], best_w[3]);
    for (n, v) in K_NAMES.iter().zip(k) {
        println!("  {n}: {v:.3}");
    }
    drop(samples);
    eprintln!("loading {} test photos …", test.len());
    let check = load(&test);
    let before = pool.install(|| calibration_score(&check, &K_START));
    let after = pool.install(|| calibration_score(&check, &k));
    println!(
        "check on {} other photos (before → after): unedited {:.2} → {:.2}, light {:.2} → {:.2}, medium {:.2} → {:.2}, heavy {:.2} → {:.2}",
        check.len(), before[0], after[0], before[1], after[1], before[2], after[2], before[3], after[3]
    );
    Ok(())
}

/// `lr-samples DATA.json OUT.json [count]`: like `lr-compare`, but writes Lab pairs (ours, reference) on a grid per photo.
fn lr_samples(args: &[String]) -> Result<()> {
    use rayon::prelude::*;
    let data: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(args.first().context("DATA.json missing")?)?)?;
    let out = args.get(1).context("OUT.json missing")?;
    let max: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let catalog = crate::dev_catalog();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let outcome: Vec<serde_json::Value> = pool.install(|| {
        data
            .par_iter()
            .take(max)
            .filter_map(|d| {
                let mut doc = rawdev::RawDoc::open(std::path::Path::new(d["nas"].as_str()?), &|| true).ok()??;
                doc.logical_path = std::path::PathBuf::from(d["lr_path"].as_str()?);
                rawdev::lightroom::apply_orientation_from_catalog(&mut doc, &catalog);
                let s = rawdev::lightroom::initial_settings_from_catalog(&doc, &catalog).unwrap_or_default();
                let img = develop::geometry::apply(doc.render_preview(&s), &s, doc.orientation);
                let own = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8())?;
                let reference = image::open(d["jpg"].as_str()?).ok()?.to_rgb8();
                let p: Vec<f32> = raw_compare::samples(&own, &reference, false).into_iter().flatten().collect();
                Some(serde_json::json!({"file": d["file"], "ext": d["ext"], "model": doc.info.model, "strength": d["strength"], "p": p}))
            })
            .collect()
    });
    std::fs::write(out, serde_json::to_string(&outcome)?)?;
    println!("{} photos", outcome.len());
    Ok(())
}

/// Mean midtone brightness deviation (EV, ours/reference) from Lab pairs; midtones = reference L* between 30 and 70.
fn midtone_ev(p: &[[f32; 6]]) -> Option<f32> {
    let y = |l: f32| ((l + 16.0) / 116.0).powi(3);
    let (mut sum, mut n) = (0.0f32, 0u32);
    for q in p.iter().filter(|q| q[3] > 30.0 && q[3] < 70.0) {
        sum += (y(q[0]) / y(q[3])).log2();
        n += 1;
    }
    (n >= 30).then(|| sum / n as f32)
}

/// `camera-samples DATA.json OUT.json [count]`: midtone brightness of our development against the reference export and against the
/// embedded camera JPEG (unedited photos only). Checks whether the model value can be derived from the camera JPEG.
fn camera_samples(args: &[String]) -> Result<()> {
    use rayon::prelude::*;
    let data: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(args.first().context("DATA.json missing")?)?)?;
    let out = args.get(1).context("OUT.json missing")?;
    let max: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let catalog = crate::dev_catalog();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let outcome: Vec<serde_json::Value> = pool.install(|| {
        let selection: Vec<&serde_json::Value> = data.iter().filter(|d| d["strength"].as_f64().unwrap_or(1.0) < 1.0).take(max).collect();
        selection
            .par_iter()
            .filter_map(|d| {
                let path = std::path::Path::new(d["nas"].as_str()?);
                let mut doc = rawdev::RawDoc::open(path, &|| true).ok()??;
                doc.logical_path = std::path::PathBuf::from(d["lr_path"].as_str()?);
                rawdev::lightroom::apply_orientation_from_catalog(&mut doc, &catalog);
                let s = rawdev::lightroom::initial_settings_from_catalog(&doc, &catalog).unwrap_or_default();
                let img = develop::geometry::apply(doc.render_preview(&s), &s, doc.orientation);
                let own = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8())?;
                let lr = image::open(d["jpg"].as_str()?).ok()?.to_rgb8();
                let mut cam = raw::RawFile::open(path).ok()?.preview()?;
                for _ in 0..doc.orientation % 4 {
                    cam = image::imageops::rotate90(&cam);
                }
                let ev_lr = midtone_ev(&raw_compare::samples(&own, &lr, false))?;
                let ev_cam = midtone_ev(&raw_compare::samples(&own, &cam, false))?;
                Some(serde_json::json!({"file": d["file"], "model": doc.info.model, "iso": doc.info.exif.iso, "dr": doc.info.fuji_dr,
                    "ev_lr": ev_lr, "ev_camera": ev_cam, "camera_px": [cam.width(), cam.height()]}))
            })
            .collect()
    });
    std::fs::write(out, serde_json::to_string(&outcome)?)?;
    println!("{} photos", outcome.len());
    Ok(())
}

/// `graineo-dev sync [--fast] [--folder <path>]`: sync the library of a **test folder** (`GRAINEO_DATA_DIR`
/// required) once and print time, processor time and the counted system calls. `--folder` adds the folder as an own
/// root first.
fn sync_cmd(args: &[String]) -> Result<()> {
    anyhow::ensure!(std::env::var_os("GRAINEO_DATA_DIR").is_some(), "only with GRAINEO_DATA_DIR (test folder)");
    let db = library::db::Db::standard().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if let Some(i) = args.iter().position(|a| a == "--folder") {
        let p = args.get(i + 1).context("--folder <path>")?;
        let t = Instant::now();
        let (_, b) = library::reconcile::add_root(&db, std::path::Path::new(p), &|_| {}).map_err(anyhow::Error::msg)?;
        println!("added: {} new photos in {:.2} s", b.newly_added, t.elapsed().as_secs_f32());
    }
    let depth = if args.iter().any(|a| a == "--fast") { library::reconcile::Depth::Quick } else { library::reconcile::Depth::Full };
    let cpu = || {
        #[cfg(unix)]
        {
            // SAFETY: getrusage only fills the given struct
            let mut u: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
            let s = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
            s(u.ru_utime) + s(u.ru_stime)
        }
        #[cfg(not(unix))]
        0.0
    };
    let (t, c) = (Instant::now(), cpu());
    let b = library::reconcile::run(&db, None, &library::reconcile::Options { depth, write_now: Some(false), progress: &|_| {}, count: &|_, _| {}, pause: &|| {} }).map_err(anyhow::Error::msg)?;
    let (wall, proc_s) = (t.elapsed().as_secs_f64(), cpu() - c);
    println!(
        "{:?}: {:.3} s, CPU {:.3} s ({:.0} %) · files {} · present {} · new {} · missing {} · folders read {} · unchanged {} · stat {}",
        depth,
        wall,
        proc_s,
        100.0 * proc_s / wall.max(1e-9),
        b.files_checked,
        b.existing,
        b.newly_added,
        b.missing,
        b.folders_listed,
        b.folders_unchanged,
        b.files_stat
    );
    Ok(())
}

/// `graineo-dev apple-photos --sync`: import photos and named faces into the library of a **test folder**
/// (`GRAINEO_DATA_DIR` required) and measure the times. The library is only read.
fn apple_photos_sync_cmd() -> Result<()> {
    anyhow::ensure!(std::env::var_os("GRAINEO_DATA_DIR").is_some(), "only with GRAINEO_DATA_DIR (test folder)");
    let m = library::apple_photos::standard().context("no Apple Photos library found")?;
    let db = library::db::Db::standard().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let t = Instant::now();
    let b = library::apple_photos::reconcile(&db, &m).map_err(anyhow::Error::msg)?;
    println!("photos: {} total, {} new, {} removed, {} collections ({:.1} s)", b.total, b.new, b.removed, b.albums, t.elapsed().as_secs_f32());
    let t = Instant::now();
    let g = library::apple_photos::match_faces(&db, &m).map_err(anyhow::Error::msg)?;
    println!("faces: {g:?} ({:.1} s)", t.elapsed().as_secs_f32());
    Ok(())
}

/// `graineo-dev apple-photos [library]`: analyze the Photos library read-only (count, local, albums).
fn apple_photos_cmd(path: Option<&str>) -> Result<()> {
    let m = path.map(std::path::PathBuf::from).or_else(library::apple_photos::standard).context("no Apple Photos library found")?;
    let t = Instant::now();
    let (assets, album_list) = library::apple_photos::read(&m).map_err(anyhow::Error::msg)?;
    let local = assets.iter().filter(|a| a.original(&m).is_file()).count();
    let preview = assets.iter().filter(|a| a.preview(&m).is_some()).count();
    let videos = assets.iter().filter(|a| a.video).count();
    println!("{} entries ({} videos), originals local {}, previews local {}, {} albums, {:.1} s", assets.len(), videos, local, preview, album_list.len(), t.elapsed().as_secs_f32());
    let mut cameras: std::collections::BTreeMap<String, usize> = Default::default();
    for a in &assets {
        *cameras.entry(a.camera.clone().unwrap_or_else(|| "–".into())).or_default() += 1;
    }
    let mut k: Vec<_> = cameras.into_iter().collect();
    k.sort_by(|a, b| b.1.cmp(&a.1));
    for (n, c) in k.iter().take(6) {
        println!("  {c:6}  {n}");
    }
    for a in assets.iter().take(3) {
        println!("  {} {:?} {:?}", a.uuid, a.original_name, a.capture);
    }
    Ok(())
}
