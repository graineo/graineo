//! `graineo-dev skin-chart`: runs the test chart of `color-chart` (ColorChecker, gray wedge, saturation rows, 8 skin
//! tones) through the look stage with the skin-tone sliders (`develop::skin`) at their extremes and prints ΔE00 per
//! patch against the untouched chart. Non-skin patches should stay at ΔE00 ≈ 0.

use anyhow::Result;
use develop::look::Geo;
use develop::{Rgb16, Settings};

use crate::color_chart::{de00, lab, patches, PatchGroup};

fn run_patches(rgb: &[[u8; 3]], s: &Settings) -> Vec<[f64; 3]> {
    let mut img = Rgb16 { width: rgb.len(), height: 1, data: rgb.iter().flat_map(|p| p.map(|v| v as u16 * 257)).collect() };
    develop::look::apply(&mut img, s, Geo::full(rgb.len(), 1));
    img.data.chunks(3).map(|c| [c[0], c[1], c[2]].map(|v| v as f64 / 257.0)).collect()
}

pub fn run(_args: &[String]) -> Result<()> {
    let ps = patches();
    let rgb: Vec<[u8; 3]> = ps.iter().map(|p| p.rgb).collect();
    let base = run_patches(&rgb, &Settings::default());
    let cases = [("hue -1", -1.0, 0.0), ("hue +1", 1.0, 0.0), ("sat -1", 0.0, -1.0), ("sat +1", 0.0, 1.0)];
    let out: Vec<Vec<[f64; 3]>> = cases.iter().map(|&(_, h, sa)| run_patches(&rgb, &Settings { skin_hue: h, skin_sat: sa, ..Default::default() })).collect();
    print!("{:<16} {:<6}", "patch", "group");
    for (n, ..) in cases {
        print!(" {n:>8}");
    }
    println!();
    let mut max_other = [0.0f64; 4];
    let mut mean_skin = [0.0f64; 4];
    let mut n_skin = 0;
    for (i, p) in ps.iter().enumerate() {
        let skin = matches!(p.group, PatchGroup::Skin) || p.name.ends_with("skin");
        print!("{:<16} {:<6}", p.name, if skin { "skin" } else { p.group.name() });
        for k in 0..cases.len() {
            let d = de00(lab(base[i]), lab(out[k][i]));
            print!(" {d:>8.2}");
            if skin {
                mean_skin[k] += d;
            } else {
                max_other[k] = max_other[k].max(d);
            }
        }
        n_skin += skin as usize;
        println!();
    }
    print!("{:<23}", "mean ΔE00 skin");
    for v in mean_skin {
        print!(" {:>8.2}", v / n_skin as f64);
    }
    println!();
    print!("{:<23}", "max ΔE00 non-skin");
    for v in max_other {
        print!(" {v:>8.2}");
    }
    println!();
    Ok(())
}
