//! Writer for text 3D LUTs (`.cube`, red varies fastest): one place for every export
//! (film in the darkroom, any look in "Film & Look").
use std::fmt::Write;

/// Grid of `n`³ input colors (0…1), red fastest, then green, then blue.
pub fn grid(n: usize) -> Vec<[f32; 3]> {
    let m = (n - 1) as f32;
    let mut v = Vec::with_capacity(n * n * n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                v.push([r, g, b].map(|x| x as f32 / m));
            }
        }
    }
    v
}

/// `.cube` text: `# Created by graineo`, the comments, `TITLE`, size, domain 0…1 and the rows.
/// `values` = output of [`grid`]`(n)` in the same order.
pub fn write(title: &str, comments: &[&str], n: usize, values: &[[f32; 3]]) -> Result<String, String> {
    if values.len() != n * n * n {
        return Err("LUT hat die falsche Zahl von Werten".into());
    }
    let title = title.replace(['"', '\r', '\n'], " ");
    let mut t = String::with_capacity(values.len() * 28 + 400);
    let _ = writeln!(t, "# Created by graineo");
    for c in comments {
        let _ = writeln!(t, "# {}", c.replace(['\r', '\n'], " "));
    }
    let _ = writeln!(t, "TITLE \"{title}\"\nLUT_3D_SIZE {n}\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0");
    for p in values {
        let _ = writeln!(t, "{:.8} {:.8} {:.8}", p[0], p[1], p[2]);
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    #[test]
    fn grid_order_and_text() {
        let g = super::grid(3);
        assert_eq!(g[1], [0.5, 0.0, 0.0]);
        assert_eq!(g[3], [0.0, 0.5, 0.0]);
        assert_eq!(g[9], [0.0, 0.0, 0.5]);
        let t = super::write("a\"b", &["x"], 3, &g).unwrap();
        assert!(t.starts_with("# Created by graineo\n# x\nTITLE \"a b\"\nLUT_3D_SIZE 3"));
        assert!(super::write("t", &[], 3, &g[..5]).is_err());
    }
}
