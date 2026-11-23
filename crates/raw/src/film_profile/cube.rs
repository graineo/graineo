//! Own reader for text LUTs (`.cube`, red varies fastest): 3D table, 1D shaper before it, or only a 1D curve.
//! No LUT data is shipped.
//!
//! Conventions: `LUT_1D_SIZE n` = n rows (one value per channel) per-channel curve, linear interpolation;
//! `LUT_3D_SIZE n` = n³ rows. With both tables the rows of the 1D curve come first, then the 3D table; the curve
//! is applied first. Input range: `LUT_1D_INPUT_RANGE` / `LUT_3D_INPUT_RANGE` for the table concerned, otherwise
//! `DOMAIN_MIN/MAX`. With both tables `DOMAIN_*` belongs to the curve and the 3D table expects the curve's output
//! in 0…1 (unless it has its own `LUT_3D_INPUT_RANGE`). Unknown keywords are ignored.
use super::{manager, FilmProfile, Look, Source};
use std::{fs::File, io::Read, path::Path};

struct Cube {
    /// 3D edge length (0 = no 3D table)
    n: usize,
    min: [f32; 3],
    max: [f32; 3],
    values: Vec<[f32; 3]>,
    /// 1D curve (empty = none), per channel over `smin`…`smax`
    shaper: Vec<[f32; 3]>,
    smin: [f32; 3],
    smax: [f32; 3],
}
fn parse_triple(s: &str) -> Result<[f32; 3], String> {
    let v: Vec<f32> = s
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| "Ungültiger LUT-Wert")?;
    if v.iter().any(|x| !x.is_finite() || x.abs() > 1e6) {
        return Err("LUT-Werte müssen endlich sein".into());
    }
    match v[..] {
        [a, b, c] => Ok([a, b, c]),
        // some 1D files carry one value per row
        [a] => Ok([a; 3]),
        _ => Err("Ungültiger LUT-Wert".into()),
    }
}
fn parse_range(s: &str) -> Result<([f32; 3], [f32; 3]), String> {
    let v: Vec<f32> = s
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| "Ungültiger Eingabebereich")?;
    if v.len() != 2 || !v.iter().all(|x| x.is_finite()) || v[1] - v[0] <= 1e-12 {
        return Err("Ungültiger Eingabebereich".into());
    }
    Ok(([v[0]; 3], [v[1]; 3]))
}
fn parse(text: &str) -> Result<Cube, String> {
    let (mut n, mut n1) = (0usize, 0usize);
    let mut shaper = Vec::new();
    let mut values = Vec::new();
    let mut started = false;
    let (mut dmin, mut dmax) = (None, None);
    let (mut r1, mut r3) = (None, None);
    for line in text.lines() {
        let line = line
            .split('#')
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let is_data = key.parse::<f32>().is_ok();
        if !is_data && started {
            return Err("Ungültiger LUT-Wert".into());
        }
        match key {
            _ if is_data => {
                if n == 0 && n1 == 0 {
                    return Err("LUT-Größe fehlt (LUT_3D_SIZE oder LUT_1D_SIZE)".into());
                }
                let v = parse_triple(line)?;
                started = true;
                if shaper.len() < n1 {
                    shaper.push(v);
                } else if values.len() < n * n * n {
                    values.push(v);
                } else {
                    return Err("Zu viele LUT-Zeilen".into());
                }
            }
            "LUT_3D_SIZE" if n == 0 => {
                let size = rest
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "Ungültige LUT-Größe")?;
                if !(17..=65).contains(&size) {
                    return Err("3D-LUTs brauchen 17 bis 65 Stützstellen".into());
                }
                n = size;
            }
            "LUT_1D_SIZE" if n1 == 0 => {
                n1 = rest.trim().parse().map_err(|_| "Ungültiger 1D-Shaper")?;
                if !(2..=65536).contains(&n1) {
                    return Err("1D-Shaper ist zu groß".into());
                }
            }
            "DOMAIN_MIN" => dmin = Some(parse_triple(rest)?),
            "DOMAIN_MAX" => dmax = Some(parse_triple(rest)?),
            "LUT_1D_INPUT_RANGE" => r1 = Some(parse_range(rest)?),
            "LUT_3D_INPUT_RANGE" => r3 = Some(parse_range(rest)?),
            // TITLE, LUT_IN_VIDEO_RANGE … and anything unknown before the data
            _ => {}
        }
    }
    if n == 0 && n1 == 0 {
        return Err("LUT-Größe fehlt (LUT_3D_SIZE oder LUT_1D_SIZE)".into());
    }
    if shaper.len() != n1 {
        return Err("1D-LUT ist unvollständig".into());
    }
    if values.len() != n * n * n {
        return Err("3D-LUT ist unvollständig".into());
    }
    let domain = (dmin.unwrap_or([0.0; 3]), dmax.unwrap_or([1.0; 3]));
    let (smin, smax) = r1.unwrap_or(domain);
    let (min, max) = r3.unwrap_or(if n1 > 0 { ([0.0; 3], [1.0; 3]) } else { domain });
    for (a, b) in [(smin, smax), (min, max)] {
        if (0..3).any(|c| !a[c].is_finite() || !b[c].is_finite() || b[c] - a[c] <= 1e-12) {
            return Err("Ungültiger LUT-Eingabebereich".into());
        }
    }
    Ok(Cube {
        n,
        min,
        max,
        values,
        shaper,
        smin,
        smax,
    })
}
impl Cube {
    fn curve(&self, c: [f32; 3]) -> [f32; 3] {
        if self.shaper.is_empty() {
            return c;
        }
        let m = (self.shaper.len() - 1) as f32;
        std::array::from_fn(|k| {
            let p = ((c[k] - self.smin[k]) / (self.smax[k] - self.smin[k])).clamp(0.0, 1.0) * m;
            let i = (p as usize).min(self.shaper.len() - 2);
            let f = p - i as f32;
            self.shaper[i][k] * (1.0 - f) + self.shaper[i + 1][k] * f
        })
    }
    fn pixel(&self, c: [f32; 3]) -> [f32; 3] {
        let c = self.curve(c);
        if self.n == 0 {
            return c;
        }
        let m = (self.n - 1) as f32;
        let p = std::array::from_fn::<_, 3, _>(|k| {
            ((c[k] - self.min[k]) / (self.max[k] - self.min[k])).clamp(0.0, 1.0) * m
        });
        let i = p.map(|v| (v as usize).min(self.n - 2));
        let f = std::array::from_fn::<_, 3, _>(|k| p[k] - i[k] as f32);
        let mut out = [0.0; 3];
        for (r, wr) in [(0, 1.0 - f[0]), (1, f[0])] {
            for (g, wg) in [(0, 1.0 - f[1]), (1, f[1])] {
                for (b, wb) in [(0, 1.0 - f[2]), (1, f[2])] {
                    let v = self.values[(i[0] + r) + self.n * ((i[1] + g) + self.n * (i[2] + b))];
                    for k in 0..3 {
                        out[k] += wr * wg * wb * v[k];
                    }
                }
            }
        }
        out
    }
}
/// The table of a `.cube` text as differences on its own grid (or 65³ for a curve or a non-default domain), the form
/// [`Look::from_table`] takes: (edge length, Δ per node, r slow, b fast).
pub fn table(text: &str) -> Result<(usize, Vec<[f32; 3]>), String> {
    let cube = parse(text)?;
    let n = if cube.n > 0 && cube.shaper.is_empty() && cube.min == [0.0; 3] && cube.max == [1.0; 3] {
        cube.n
    } else {
        65
    };
    let mut d = Vec::with_capacity(n * n * n);
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                let c = [r, g, b].map(|x| x as f32 / (n - 1) as f32);
                let v = cube.pixel(c);
                d.push(std::array::from_fn(|k| v[k] - c[k]));
            }
        }
    }
    Ok((n, d))
}

/// Is the text a complete, valid LUT (every row present and readable)?
pub(super) fn check(text: &str) -> Result<(), String> {
    parse(text).map(|_| ())
}

pub(super) fn look(text: &str, f: &FilmProfile) -> Result<Look, String> {
    let cube = parse(text)?;
    // Native grid preserves ordinary 3D LUTs exactly. A curve or a non-default domain is baked onto a 65³ grid.
    let n = if cube.n > 0 && cube.shaper.is_empty() && cube.min == [0.0; 3] && cube.max == [1.0; 3] {
        cube.n
    } else {
        65
    };
    let mut d = Vec::with_capacity(n * n * n);
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                let c = [r, g, b].map(|x| x as f32 / (n - 1) as f32);
                let v = cube.pixel(c);
                d.push(std::array::from_fn(|k| v[k] - c[k]));
            }
        }
    }
    Look::from_table(
        &format!("film:{}", f.uuid),
        r#"{"source":"cube","colorSpace":"sRGB"}"#,
        n,
        d,
    )
    .ok_or("LUT lässt sich nicht umrechnen".into())
}
pub(super) fn profile(path: &Path) -> Option<FilmProfile> {
    let mut head = String::new();
    File::open(path)
        .ok()?
        .take(6000)
        .read_to_string(&mut head)
        .ok()?;
    let size = |key: &str| {
        head.lines().find_map(|l| {
            l.trim()
                .strip_prefix(key)
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
    };
    let (n3, n1) = (size("LUT_3D_SIZE"), size("LUT_1D_SIZE"));
    if n3.is_none() && n1.is_none() {
        return None;
    }
    if n3.is_some_and(|n| !(17..=65).contains(&n)) || n1.is_some_and(|n| !(2..=65536).contains(&n)) {
        return None;
    }
    let manifest = manager::read_limited(&path.with_extension("json"), 64 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let id = manifest
        .as_ref()
        .and_then(|m| m["id"].as_str())
        .filter(|u| {
            u.starts_with("CUBE-") && u.len() == 37 && u[5..].bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .or_else(|| {
            Some(format!(
                "CUBE-{}",
                super::md5_hex(&super::md5(
                    &manager::read_limited(path, manager::MAX_FILE).ok()?
                ))
                .to_uppercase()
            ))
        })?;
    Some(FilmProfile {
        uuid: id,
        name: path
            .file_stem()?
            .to_string_lossy()
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect(),
        group: "Lookup-Tabellen".into(),
        path: path.into(),
        source: Source::Automatic,
        available: true,
        strength: true,
        additional: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(shaper: bool) -> String {
        let mut s = String::from("TITLE \"Ignored title\"\nLUT_3D_SIZE 17\n");
        if shaper {
            s.push_str("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n");
        }
        for b in 0..17 {
            for g in 0..17 {
                for r in 0..17 {
                    s.push_str(&format!(
                        "{} {} {}\n",
                        b as f32 / 16.0,
                        g as f32 / 16.0,
                        r as f32 / 16.0
                    ));
                }
            }
        }
        s
    }
    #[test]
    fn axes_identity_shaper_and_intermediate_values() {
        for shaper in [false, true] {
            let c = parse(&text(shaper)).unwrap();
            let y = c.pixel([0.13, 0.47, 0.82]);
            assert!(
                (y[0] - 0.82).abs() < 1e-6
                    && (y[1] - 0.47).abs() < 1e-6
                    && (y[2] - 0.13).abs() < 1e-6
            );
        }
    }
    #[test]
    fn domain_inputs_and_broken_tables() {
        let t = text(false).replace(
            "LUT_3D_SIZE 17",
            "LUT_3D_SIZE 17\nDOMAIN_MIN 0.25 0.25 0.25\nDOMAIN_MAX 0.75 0.75 0.75",
        );
        let c = parse(&t).unwrap();
        assert_eq!(c.pixel([0.25, 0.5, 0.75]), [1.0, 0.5, 0.0]);
        assert!(parse("LUT_1D_SIZE 17\n0 0 0").is_err());
        assert!(parse(&text(false).replace("LUT_3D_SIZE 17", "LUT_3D_SIZE 999999")).is_err());
        assert!(parse(&(text(false) + "1 1 1\n")).is_err());
        assert!(parse(&text(false).replace("0 0 0\n", "NaN 0 0\n")).is_err());
        assert!(
            parse(&t.replace("DOMAIN_MAX 0.75 0.75 0.75", "DOMAIN_MAX 0.25 0.75 0.75")).is_err()
        );
        assert!(parse("LUT_3D_SIZE 17\n0 0 0").is_err());
    }

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5)
    }
    /// 3D table as a swap of channels (r<->b) with the 1D curve `x²` in front: output = (b², g², r²)
    fn gamma_text(extra: &str) -> String {
        let mut s = format!("# comment\n\nTITLE \"x\"\n{extra}LUT_1D_SIZE 5\nLUT_3D_SIZE 17\n\n");
        for i in 0..5 {
            let v = (i as f32 / 4.0).powi(2);
            s.push_str(&format!("{v} {v} {v}\n"));
        }
        for b in 0..17 {
            for g in 0..17 {
                for r in 0..17 {
                    s.push_str(&format!("{} {} {}\n", b as f32 / 16.0, g as f32 / 16.0, r as f32 / 16.0));
                }
            }
        }
        s
    }
    #[test]
    fn shaper_runs_before_3d_table() {
        let c = parse(&gamma_text("")).unwrap();
        // curve points exactly on the nodes: 0.5 -> 0.25 (linear between 0.25 -> 0.0625 and 0.5 -> 0.25)
        let y = c.pixel([0.5, 1.0, 0.0]);
        assert!(near(y, [0.0, 1.0, 0.25]), "{y:?}");
        // between nodes the curve is linear, then 17 nodes of the identity-like swap
        let y = c.pixel([0.375, 0.375, 0.375]);
        assert!(near(y, [0.15625; 3]), "{y:?}"); // (0.0625 + 0.25) / 2
    }
    #[test]
    fn shaper_only_applies_the_curve() {
        let mut t = String::from("TITLE \"curve\"\nLUT_1D_SIZE 3\n0 0 0\n0.8 0.5 0.2\n1 1 1\n");
        let c = parse(&t).unwrap();
        assert_eq!(c.n, 0);
        assert!(near(c.pixel([0.5, 0.5, 0.5]), [0.8, 0.5, 0.2]));
        assert!(near(c.pixel([0.25, 0.25, 0.25]), [0.4, 0.25, 0.1]));
        // input range
        t = t.replace("LUT_1D_SIZE 3", "LUT_1D_SIZE 3\nLUT_1D_INPUT_RANGE 0.0 2.0");
        assert!(near(parse(&t).unwrap().pixel([1.0; 3]), [0.8, 0.5, 0.2]));
        // one value per row
        assert!(parse("LUT_1D_SIZE 2\n0\n1\n").is_ok());
        // baked into a Look (65³), reproduces the curve at a node of the grid
        let f = FilmProfile { uuid: "CUBE-X".into(), name: "x".into(), group: String::new(), path: Default::default(), source: Source::Automatic, available: true, strength: true, additional: Vec::new() };
        let l = look(&t.replace("LUT_1D_INPUT_RANGE 0.0 2.0\n", ""), &f).unwrap();
        assert!(near(l.pixel([0.5, 0.5, 0.5]), [0.8, 0.5, 0.2]));
    }
    #[test]
    fn broken_one_dimensional_files_say_what_is_wrong() {
        assert_eq!(parse("LUT_1D_SIZE 3\n0 0 0\n1 1 1\n").err().unwrap(), "1D-LUT ist unvollständig");
        assert_eq!(parse("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n1 1 1\n").err().unwrap(), "Zu viele LUT-Zeilen");
        assert!(parse("0 0 0\n1 1 1\n").err().unwrap().contains("LUT-Größe fehlt"));
        assert!(parse("TITLE \"x\"\n").err().unwrap().contains("LUT-Größe fehlt"));
        assert!(parse("LUT_1D_SIZE 2\n0 0 0\n1 1 x\n").is_err());
        assert!(parse("LUT_1D_SIZE 2\n0 0 0\n1 1\n").is_err());
        assert!(parse("LUT_1D_SIZE 1\n0 0 0\n").is_err());
        assert!(parse("LUT_1D_SIZE 2\nLUT_1D_INPUT_RANGE 1 1\n0 0 0\n1 1 1\n").is_err());
        // keyword after the data
        assert!(parse("LUT_1D_SIZE 2\n0 0 0\n1 1 1\nDOMAIN_MAX 2 2 2\n").is_err());
    }

    /// Export a look as .cube, read it back and compare with the direct table (synthetic look, no real data).
    #[test]
    fn exported_look_reproduces_the_table() {
        let smooth = |c: [f32; 3]| {
            // smooth, non-trivial color change (warm shift, S curve, channel mixing)
            let s = |x: f32| x + 0.08 * (6.2832 * x).sin() * 0.5;
            [s(c[0]) * 0.95 + 0.05 * c[1] + 0.03, s(c[1]) * 0.9 + 0.08 * c[2], s(c[2]) * 0.85 + 0.1 * c[0] * c[1]]
        };
        for (nodes, tol) in [(17usize, 1e-5f32), (33, 1e-5), (65, 0.004)] {
            let m = (nodes - 1) as f32;
            let mut d = Vec::new();
            for r in 0..nodes {
                for g in 0..nodes {
                    for b in 0..nodes {
                        let c = [r, g, b].map(|x| x as f32 / m);
                        let o = smooth(c).map(|v| v.clamp(0.0, 1.0));
                        d.push([o[0] - c[0], o[1] - c[1], o[2] - c[2]]);
                    }
                }
            }
            let look = Look::from_table("synthetic", "{}", nodes, d).unwrap();
            for amount in [1.0f32, 0.6] {
                let text = look.cube_text("Synthetic \"look\"", amount).unwrap();
                assert!(text.starts_with("# Created by graineo\n") && text.contains("TITLE \"Synthetic  look \""));
                let cube = parse(&text).unwrap();
                assert_eq!(cube.n, 33);
                let (mut worst, mut sum, mut count) = (0.0f32, 0.0f32, 0);
                for i in 0..2000u32 {
                    // deterministic pseudo-random points
                    let h = |k: u32| ((i.wrapping_mul(2654435761).wrapping_add(k.wrapping_mul(40503)) >> 7) % 10007) as f32 / 10006.0;
                    let c = [h(1), h(2), h(3)];
                    let o = look.pixel(c);
                    let want: [f32; 3] = std::array::from_fn(|k| c[k] + amount * (o[k] - c[k]));
                    let got = cube.pixel(c);
                    for k in 0..3 {
                        let e = (got[k] - want[k]).abs();
                        worst = worst.max(e);
                        sum += e;
                        count += 1;
                    }
                }
                eprintln!("look export {nodes}^3 amount {amount}: max {:.5} ({:.2} of 255 levels), mean {:.6}", worst, worst * 255.0, sum / count as f32);
                assert!(worst <= tol, "{nodes} {amount}: {worst}");
            }
        }
    }
}
