//! `graineo-dev camera-color --list PAIRS.tsv [--sat F1,F2,…] [--camera-look] [--no-ev-match]`:
//! color distance of the app's development of a RAW to the camera's own rendering of the same shot
//! (camera JPEG or the JPEG preview embedded in the RAW), as distributions over many photos.
//!
//! `PAIRS.tsv`: one line per shot, `group<TAB>split<TAB>RAW<TAB>REFERENCE` with REFERENCE a JPEG path or
//! `embedded`; `group` (e.g. camera and picture style) and `split` (e.g. `train`/`holdout`) only label
//! the results. Lines starting with `#` are skipped.
//!
//! Per shot: the app's preview development (default settings: look "Neutral", i.e. the standard
//! development with its per-camera corrections; `--camera-look`: the bundled look of the shot's picture
//! style, shots without one are skipped) is compared with the reference on the grid of
//! `rawapp --compare` (common crop, area average to a 400 px long edge, ΔE2000 per cell, reference as
//! sRGB). Unless `--no-ev-match`, the exposure is first matched on the midtones of the reference
//! (two passes), so global brightness differences do not count as color error. `--sat` additionally
//! renders the shot with these values of the per-camera saturation factor
//! (`raw::development::camera_saturation`, set through its test hook); variant `code` is the value the
//! code uses. Shots are processed one after another (the hook is process-wide). `--label NAME` names the
//! `code` variant differently (to compare runs with other test hooks, e.g. `GRAINEO_CAMERA_LOOK`).
//!
//! `--fit-look OUT`: fit the per-camera hue table (`raw::development::camera_look`, 18 nodes at 10°, 30° …
//! in Oklab, hue shift and chroma factor) from the `train` lines only, run with `GRAINEO_CAMERA_LOOK=off`
//! (the table acts after the tone curve, so it is fitted on the development without it). Per node:
//! mean hue difference and geometric mean chroma ratio (reference / ours) of the cells around it
//! (linear weights by our hue, chroma > 0.04, 0.25 < L < 0.95, outliers beyond ±30° or a ratio outside
//! 0.5 … 2 dropped), shrunk towards no change by n / (n + 20 000), smoothed 1-2-1 over the neighbours.
//! Written as 18 lines "hue shift chroma factor" (the format of `GRAINEO_CAMERA_LOOK=<file>`).
//!
//! Output (stdout, tab-separated):
//! - `IMG group split file variant ev mean median p90 dL dC dL_shadows dL_mid dL_highlights`
//! - `POOL group split variant images median p90 mean mean_of_images dC dL`: all cells of all shots pooled
//! - `SECTOR group split variant hue_from n chroma_ratio dhue`: per 20° hue sector of the reference
//!   (cells with C* > 10 and 20 < L* < 90): geometric mean of C*ours / C*reference and mean hue difference
//!   in degrees (ours − reference)

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use develop::Settings;
use image::RgbImage;

use crate::raw_compare;

/// Width of a histogram bin in ΔE00.
const BIN: f32 = 0.02;
const BINS: usize = 4000;

/// Pooled ΔE00 distribution (histogram) plus chroma/lightness sums and per-sector hue statistics.
#[derive(Clone)]
pub struct Pool {
    hist: Vec<u64>,
    n: u64,
    sum: f64,
    sum_dc: f64,
    sum_dl: f64,
    image_means: Vec<f64>,
    /// per 20° sector: n, sum ln(C ours / C ref), sum Δh
    sectors: [(u64, f64, f64); 18],
}

impl Default for Pool {
    fn default() -> Self {
        Pool { hist: vec![0; BINS + 1], n: 0, sum: 0.0, sum_dc: 0.0, sum_dl: 0.0, image_means: Vec::new(), sectors: [(0, 0.0, 0.0); 18] }
    }
}

impl Pool {
    fn add(&mut self, cells: &[([f32; 3], [f32; 3])], de: &[f32]) {
        for ((a, b), d) in cells.iter().zip(de) {
            self.hist[((d / BIN) as usize).min(BINS)] += 1;
            self.n += 1;
            self.sum += *d as f64;
            let (ca, cb) = (a[1].hypot(a[2]), b[1].hypot(b[2]));
            self.sum_dc += (ca - cb) as f64;
            self.sum_dl += (a[0] - b[0]) as f64;
            if cb > 10.0 && b[0] > 20.0 && b[0] < 90.0 && ca > 0.5 {
                let hb = b[2].atan2(b[1]).to_degrees().rem_euclid(360.0);
                let ha = a[2].atan2(a[1]).to_degrees().rem_euclid(360.0);
                let dh = (ha - hb + 540.0).rem_euclid(360.0) - 180.0;
                let s = &mut self.sectors[((hb / 20.0) as usize).min(17)];
                s.0 += 1;
                s.1 += (ca / cb).ln() as f64;
                s.2 += dh as f64;
            }
        }
        if !de.is_empty() {
            self.image_means.push(de.iter().map(|v| *v as f64).sum::<f64>() / de.len() as f64);
        }
    }

    /// ΔE00 at quantile `q` (0 … 1), bin center.
    pub fn quantile(&self, q: f64) -> f32 {
        if self.n == 0 {
            return f32::NAN;
        }
        let target = (q * self.n as f64).ceil().max(1.0) as u64;
        let mut acc = 0u64;
        for (i, c) in self.hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return (i as f32 + 0.5) * BIN;
            }
        }
        BINS as f32 * BIN
    }
}

/// ΔE00 per cell.
pub fn cell_de(cells: &[([f32; 3], [f32; 3])]) -> Vec<f32> {
    cells.iter().map(|(a, b)| raw_compare::de2000(*a, *b)).collect()
}

/// Mean log2 luminance ratio ours/reference over the reference midtones (L* 30 … 70); None with fewer than 30 cells.
pub fn midtone_ev(cells: &[([f32; 3], [f32; 3])]) -> Option<f32> {
    let y = |l: f32| ((l + 16.0) / 116.0).powi(3);
    let v: Vec<f32> = cells.iter().filter(|(_, b)| b[0] > 30.0 && b[0] < 70.0).map(|(a, b)| (y(a[0]) / y(b[0])).log2()).collect();
    (v.len() >= 30).then(|| v.iter().sum::<f32>() / v.len() as f32)
}

fn quantile_sorted(v: &[f32], q: f64) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v[(((q * v.len() as f64).ceil() as usize).max(1) - 1).min(v.len() - 1)]
}

/// Accumulator for `--fit-look`: per node (weight, Σ w·Δhue, Σ w·ln chroma ratio).
#[derive(Default)]
pub struct LookFit {
    nodes: [(f64, f64, f64); 18],
    cells: u64,
}

/// CIELab (D65) → Oklab.
fn lab_to_oklab(l: [f32; 3]) -> [f32; 3] {
    let w = raw::color::xy_to_xyz(raw::color::D65_XY);
    let fy = (l[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + l[1] / 500.0, fy - l[2] / 200.0);
    let inv = |t: f32| if t > 0.206_893 { t * t * t } else { (t - 16.0 / 116.0) / 7.787 };
    let xyz = [inv(fx) * w[0], inv(fy) * w[1], inv(fz) * w[2]];
    let m = raw::color::inv(&raw::color::srgb_to_xyz_d65()).expect("sRGB");
    raw::development::srgb_lin_to_oklab(raw::color::mulv(&m, xyz))
}

impl LookFit {
    pub fn add(&mut self, cells: &[([f32; 3], [f32; 3])]) {
        for (a, b) in cells {
            let (oa, ob) = (lab_to_oklab(*a), lab_to_oklab(*b));
            let (ca, cb) = (oa[1].hypot(oa[2]), ob[1].hypot(ob[2]));
            if ca < 0.04 || cb < 0.04 || ob[0] < 0.25 || ob[0] > 0.95 {
                continue;
            }
            let ha = oa[2].atan2(oa[1]).to_degrees().rem_euclid(360.0);
            let hb = ob[2].atan2(ob[1]).to_degrees().rem_euclid(360.0);
            let dh = (hb - ha + 540.0).rem_euclid(360.0) - 180.0;
            let r = cb / ca;
            if dh.abs() > 30.0 || !(0.5..=2.0).contains(&r) {
                continue;
            }
            // same interpolation as the look: nodes at 10°, 30° …
            let t = (ha - 10.0).rem_euclid(360.0) / 20.0;
            let i = t as usize % 18;
            let f = (t - t.floor()) as f64;
            for (k, w) in [(i, 1.0 - f), ((i + 1) % 18, f)] {
                let n = &mut self.nodes[k];
                n.0 += w;
                n.1 += w * dh as f64;
                n.2 += w * (r as f64).ln();
            }
            self.cells += 1;
        }
    }

    pub fn look(&self) -> raw::development::Look {
        const N0: f64 = 20_000.0;
        let raw: Vec<(f64, f64)> = self
            .nodes
            .iter()
            .map(|(w, h, c)| if *w > 0.0 { let s = w / (w + N0); (s * h / w, s * c / w) } else { (0.0, 0.0) })
            .collect();
        std::array::from_fn(|k| {
            let (p, q, n) = (raw[(k + 17) % 18], raw[k], raw[(k + 1) % 18]);
            let h = (p.0 + 2.0 * q.0 + n.0) / 4.0;
            let c = ((p.1 + 2.0 * q.1 + n.1) / 4.0).exp();
            [h as f32, c as f32]
        })
    }
}

struct Line {
    group: String,
    split: String,
    raw: String,
    reference: String,
}

fn read_list(path: &str) -> Result<Vec<Line>> {
    let mut out = Vec::new();
    for l in std::fs::read_to_string(path)?.lines() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let t: Vec<&str> = l.split('\t').collect();
        anyhow::ensure!(t.len() >= 4, "line needs group, split, RAW, reference: {l}");
        out.push(Line { group: t[0].into(), split: t[1].into(), raw: t[2].into(), reference: t[3].into() });
    }
    Ok(out)
}

fn render(doc: &rawdev::RawDoc, st: &rawdev::Stage, s: &Settings) -> Result<RgbImage> {
    let img = doc.render_stage_gpu(st, s).unwrap_or_else(|| doc.render_stage(st, s));
    let img = develop::geometry::apply(img, s, doc.orientation);
    RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).context("image")
}

pub fn run(args: &[String]) -> Result<()> {
    let has_flag = |n: &str| args.iter().any(|a| a == n);
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let list = read_list(&value("--list").context("--list PAIRS.tsv missing")?)?;
    let factors: Vec<f32> = value("--sat").map(|v| v.split(',').filter_map(|f| f.trim().parse().ok()).collect()).unwrap_or_default();
    let camera_look = has_flag("--camera-look");
    let label = value("--label").unwrap_or_else(|| "code".into());
    let fit_out = value("--fit-look");
    let mut fit = LookFit::default();
    let ev_match = !has_flag("--no-ev-match");
    const HOOK: &str = "GRAINEO_CAMERA_SATURATION";
    anyhow::ensure!(std::env::var_os(HOOK).is_none(), "{HOOK} must not be set (the variants set it)");
    let mut variants: Vec<(String, Option<f32>)> = vec![(label.clone(), None)];
    variants.extend(factors.iter().map(|f| (format!("{f:.3}"), Some(*f))));
    let mut pools: BTreeMap<(String, String, String), Pool> = BTreeMap::new();
    for line in &list {
        let doc = match rawdev::RawDoc::open(Path::new(&line.raw), &|| true) {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                println!("SKIP\t{}\t{e}", line.raw);
                continue;
            }
        };
        let mut s = Settings::default();
        if camera_look {
            match &doc.auto_look {
                Some(l) => s.raw.look = l.clone(),
                None => {
                    println!("SKIP\t{}\tno bundled look for this picture style", line.raw);
                    continue;
                }
            }
        }
        let embedded = line.reference == "embedded";
        let reference = if embedded {
            match raw::RawFile::open(&line.raw).map_err(|e| anyhow!("{e}"))?.preview() {
                Some(p) => p,
                None => {
                    println!("SKIP\t{}\tno embedded preview", line.raw);
                    continue;
                }
            }
        } else {
            image::open(&line.reference).with_context(|| line.reference.clone())?.to_rgb8()
        };
        let st = doc.preview_stage(&s);
        let first = render(&doc, &st, &s)?;
        let (reference, _) = raw_compare::reference_oriented(reference, (first.width(), first.height()), doc.orientation, embedded);
        if ev_match {
            let mut img = first;
            for _ in 0..2 {
                let Some(ev) = midtone_ev(&raw_compare::lab_cells(&img, &reference, false)) else { break };
                s.exposure = (s.exposure - ev as f64).clamp(-3.0, 3.0);
                img = render(&doc, &st, &s)?;
            }
        }
        let name = Path::new(&line.raw).file_name().unwrap_or_default().to_string_lossy().to_string();
        for (vname, f) in &variants {
            match f {
                Some(f) => std::env::set_var(HOOK, f.to_string()),
                None => std::env::remove_var(HOOK),
            }
            let img = render(&doc, &st, &s)?;
            let cells = raw_compare::lab_cells(&img, &reference, false);
            let de = cell_de(&cells);
            if f.is_none() && fit_out.is_some() && line.split == "train" {
                fit.add(&cells);
            }
            let mut sorted = de.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let n = de.len().max(1) as f32;
            let mean = de.iter().sum::<f32>() / n;
            let dl = cells.iter().map(|(a, b)| a[0] - b[0]).sum::<f32>() / n;
            let dc = cells.iter().map(|(a, b)| a[1].hypot(a[2]) - b[1].hypot(b[2])).sum::<f32>() / n;
            let zone = |lo: f32, hi: f32| {
                let v: Vec<f32> = cells.iter().filter(|(_, b)| b[0] >= lo && b[0] < hi).map(|(a, b)| a[0] - b[0]).collect();
                if v.is_empty() {
                    f32::NAN
                } else {
                    v.iter().sum::<f32>() / v.len() as f32
                }
            };
            println!(
                "IMG\t{}\t{}\t{name}\t{vname}\t{:+.3}\t{mean:.3}\t{:.3}\t{:.3}\t{dl:+.2}\t{dc:+.2}\t{:+.2}\t{:+.2}\t{:+.2}",
                line.group,
                line.split,
                s.exposure,
                quantile_sorted(&sorted, 0.5),
                quantile_sorted(&sorted, 0.9),
                zone(-1.0, 30.0),
                zone(30.0, 70.0),
                zone(70.0, 101.0)
            );
            pools.entry((line.group.clone(), line.split.clone(), vname.clone())).or_default().add(&cells, &de);
        }
        std::env::remove_var(HOOK);
    }
    if let Some(out) = &fit_out {
        let look = fit.look();
        let text: String = look.iter().map(|[h, c]| format!("{h:.2} {c:.4}\n")).collect();
        std::fs::write(out, format!("# hue table fitted by graineo-dev camera-color from {} cells\n{text}", fit.cells))?;
        for (k, [h, c]) in look.iter().enumerate() {
            println!("FIT\t{}\t{:.0}\t{h:+.2}\t{c:.4}", 10 + 20 * k, fit.nodes[k].0);
        }
    }
    for ((g, sp, v), p) in &pools {
        let n = p.n.max(1) as f64;
        let mut im = p.image_means.clone();
        im.sort_by(|a, b| a.total_cmp(b));
        println!(
            "POOL\t{g}\t{sp}\t{v}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:+.2}\t{:+.2}",
            p.image_means.len(),
            p.quantile(0.5),
            p.quantile(0.9),
            p.sum / n,
            im.iter().sum::<f64>() / im.len().max(1) as f64,
            p.sum_dc / n,
            p.sum_dl / n
        );
        for (k, (c, lr, dh)) in p.sectors.iter().enumerate() {
            if *c > 0 {
                println!("SECTOR\t{g}\t{sp}\t{v}\t{}\t{c}\t{:.3}\t{:+.2}", k * 20, (lr / *c as f64).exp(), dh / *c as f64);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patchwork(w: u32, h: u32, sat: f32) -> RgbImage {
        // colored patches around a gray of 120; `sat` scales the distance from gray
        RgbImage::from_fn(w, h, |x, y| {
            let base = [[200.0, 60.0, 50.0], [60.0, 170.0, 70.0], [50.0, 80.0, 200.0], [210.0, 190.0, 60.0]][(((x / 40) + (y / 40)) % 4) as usize];
            image::Rgb(base.map(|v: f32| (120.0 + sat * (v - 120.0)).round().clamp(0.0, 255.0) as u8))
        })
    }

    #[test]
    fn identical_images_have_no_distance() {
        let a = patchwork(320, 200, 1.0);
        let cells = raw_compare::lab_cells(&a, &a, false);
        let de = cell_de(&cells);
        assert!(de.iter().all(|d| *d < 1e-3));
        let mut p = Pool::default();
        p.add(&cells, &de);
        assert!(p.quantile(0.5) <= BIN && p.quantile(0.9) <= BIN);
        assert!(midtone_ev(&cells).unwrap().abs() < 1e-4);
    }

    #[test]
    fn stronger_colors_show_as_chroma_ratio_above_one() {
        let (a, b) = (patchwork(320, 200, 1.2), patchwork(320, 200, 1.0));
        let cells = raw_compare::lab_cells(&a, &b, false);
        let de = cell_de(&cells);
        let mut p = Pool::default();
        p.add(&cells, &de);
        let used: Vec<_> = p.sectors.iter().filter(|s| s.0 > 0).collect();
        assert!(!used.is_empty());
        for s in used {
            assert!((s.1 / s.0 as f64).exp() > 1.05, "chroma ratio {}", (s.1 / s.0 as f64).exp());
        }
        assert!(p.sum_dc > 0.0);
        assert!(p.quantile(0.9) >= p.quantile(0.5) && p.quantile(0.5) > 1.0);
    }

    #[test]
    fn fitted_look_moves_towards_the_reference() {
        let (a, b) = (patchwork(320, 200, 0.8), patchwork(320, 200, 1.0));
        let cells = raw_compare::lab_cells(&a, &b, false);
        let mut f = LookFit::default();
        for _ in 0..20 {
            f.add(&cells);
        }
        assert!(f.cells > 0);
        let l = f.look();
        // our colors are weaker: the chroma factor is above one where there are cells, hue changes small
        assert!(l.iter().any(|n| n[1] > 1.05), "{l:?}");
        assert!(l.iter().all(|n| n[1] > 0.99 && n[0].abs() < 10.0), "{l:?}");
        // gray maps to gray in Oklab
        let g = lab_to_oklab([50.0, 0.0, 0.0]);
        assert!(g[1].abs() < 1e-3 && g[2].abs() < 1e-3);
    }

    #[test]
    fn quantiles_of_a_known_distribution() {
        let mut p = Pool::default();
        let cells = vec![([50.0, 0.0, 0.0], [50.0, 0.0, 0.0]); 100];
        let de: Vec<f32> = (0..100).map(|i| i as f32 * 0.1).collect();
        p.add(&cells, &de);
        assert!((p.quantile(0.5) - 4.9).abs() < 0.03, "{}", p.quantile(0.5));
        assert!((p.quantile(0.9) - 8.9).abs() < 0.03, "{}", p.quantile(0.9));
    }
}
