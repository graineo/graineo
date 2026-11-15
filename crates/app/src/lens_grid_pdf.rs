//! Grid template to print for measuring a lens (assistant "Objektiv hinzufügen"): a checkerboard over the whole
//! page with marks in the middle of each edge (to line up the camera), the square size and a 100 mm check line (to
//! see that the printer did not scale). Own, minimal PDF output (one page, vector paths, one standard font), no
//! extra dependency and no outside data.

use std::fmt::Write as _;

/// Paper size of the template.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paper {
    A4,
    A3,
}

/// Orientation of the page (the board fills it either way).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    fn name(self) -> &'static str {
        match self {
            Orientation::Portrait => "portrait",
            Orientation::Landscape => "landscape",
        }
    }
}

impl Paper {
    pub fn name(self) -> &'static str {
        match self {
            Paper::A4 => "A4",
            Paper::A3 => "A3",
        }
    }
    /// width, height in mm in the given orientation
    fn size_mm(self, o: Orientation) -> (f64, f64) {
        let (w, h) = match self {
            Paper::A4 => (210.0, 297.0),
            Paper::A3 => (297.0, 420.0),
        };
        if o == Orientation::Landscape { (h, w) } else { (w, h) }
    }
    /// edge of one square in mm
    pub fn square_mm(self) -> f64 {
        match self {
            Paper::A4 => 15.0,
            Paper::A3 => 20.0,
        }
    }
}

fn pt(mm: f64) -> f64 {
    mm * 72.0 / 25.4
}

/// The page content (PDF drawing commands, origin bottom left, unit pt).
fn content(paper: Paper, o: Orientation) -> String {
    let (w, h) = paper.size_mm(o);
    let q = paper.square_mm();
    // board: as many squares as fit with 12 mm margin at the sides and 24 mm below (legend)
    let (margin, below) = (12.0, 24.0);
    let nx = ((w - 2.0 * margin) / q).floor() as usize;
    let ny = ((h - margin - below) / q).floor() as usize;
    let (bw, bh) = (nx as f64 * q, ny as f64 * q);
    let x0 = (w - bw) / 2.0;
    let y0 = below + (h - margin - below - bh) / 2.0;
    let mut c = String::new();
    c.push_str("0 g\n");
    for j in 0..ny {
        for i in 0..nx {
            if (i + j) % 2 == 0 {
                let _ = writeln!(c, "{:.3} {:.3} {:.3} {:.3} re", pt(x0 + i as f64 * q), pt(y0 + j as f64 * q), pt(q), pt(q));
            }
        }
    }
    c.push_str("f\n");
    // frame around the board and marks in the middle of each edge (outside the board)
    let _ = writeln!(c, "0.5 w 0 G {:.3} {:.3} {:.3} {:.3} re S", pt(x0), pt(y0), pt(bw), pt(bh));
    let (cx, cy, m) = (x0 + bw / 2.0, y0 + bh / 2.0, 6.0);
    for (a, b) in [
        ((cx, y0 - m), (cx, y0 - 1.0)),
        ((cx, y0 + bh + 1.0), (cx, y0 + bh + m)),
        ((x0 - m, cy), (x0 - 1.0, cy)),
        ((x0 + bw + 1.0, cy), (x0 + bw + m, cy)),
    ] {
        let _ = writeln!(c, "1 w {:.3} {:.3} m {:.3} {:.3} l S", pt(a.0), pt(a.1), pt(b.0), pt(b.1));
    }
    // 100 mm check line with a tick every 10 mm
    let (lx, ly) = (x0, 10.0);
    let _ = writeln!(c, "0.7 w {:.3} {:.3} m {:.3} {:.3} l S", pt(lx), pt(ly), pt(lx + 100.0), pt(ly));
    for k in 0..=10 {
        let t = if k % 5 == 0 { 3.0 } else { 1.5 };
        let x = lx + k as f64 * 10.0;
        let _ = writeln!(c, "0.5 w {:.3} {:.3} m {:.3} {:.3} l S", pt(x), pt(ly), pt(x), pt(ly + t));
    }
    // legend (language neutral: numbers and units only)
    let legend = format!("100 mm  |  {} mm  |  {}  |  100 %  |  graineo", q as u32, paper.name());
    let _ = writeln!(c, "BT /F1 8 Tf {:.3} {:.3} Td ({}) Tj ET", pt(lx), pt(ly + 5.0), legend);
    c
}

/// The whole PDF file.
pub fn pdf(paper: Paper, o: Orientation) -> Vec<u8> {
    let (w, h) = paper.size_mm(o);
    let body = content(paper, o);
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>", pt(w), pt(h)),
        format!("<< /Length {} >>\nstream\n{}endstream", body.len(), body),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{}\nendobj\n", i + 1, o);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for o in offsets {
        let _ = writeln!(out, "{o:010} 00000 n ");
    }
    let _ = write!(out, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", objects.len() + 1, xref);
    out.into_bytes()
}

/// Write the template to the temporary folder and open it (Preview prints it). Returns the path or the error.
pub fn write_and_open(paper: Paper, o: Orientation) -> Result<std::path::PathBuf, String> {
    let p = std::env::temp_dir().join(format!("graineo-Grid-{}-{}.pdf", paper.name(), o.name()));
    std::fs::write(&p, pdf(paper, o)).map_err(|e| e.to_string())?;
    crate::system::open(&p);
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_has_a_valid_cross_reference() {
        for (paper, o) in [Paper::A4, Paper::A3].into_iter().flat_map(|p| [(p, Orientation::Portrait), (p, Orientation::Landscape)]) {
            let b = pdf(paper, o);
            let s = String::from_utf8(b).unwrap();
            assert!(s.starts_with("%PDF-1.4\n") && s.ends_with("%%EOF\n"));
            // every entry of the cross reference points at "n 0 obj"
            let xref_at: usize = s.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
            assert!(s[xref_at..].starts_with("xref\n"));
            for (k, line) in s[xref_at..].lines().skip(3).take(5).enumerate() {
                let off: usize = line[..10].parse().unwrap();
                assert!(s[off..].starts_with(&format!("{} 0 obj", k + 1)), "object {}", k + 1);
            }
            // look at it: `GRAINEO_PDF_OUT=<folder> cargo test -p graineo lens_grid_pdf`
            if let Some(d) = std::env::var_os("GRAINEO_PDF_OUT") {
                std::fs::write(std::path::Path::new(&d).join(format!("grid-{}-{}.pdf", paper.name(), o.name())), s.as_bytes()).unwrap();
            }
            // stream length matches
            let c = content(paper, o);
            let (w, h) = paper.size_mm(o);
            assert!(s.contains(&format!("/MediaBox [0 0 {:.3} {:.3}]", pt(w), pt(h))));
            assert_eq!(w > h, o == Orientation::Landscape);
            assert!(s.contains(&format!("/Length {} >>", c.len())));
        }
    }

    #[test]
    fn board_fits_on_the_page() {
        for (paper, o) in [Paper::A4, Paper::A3].into_iter().flat_map(|p| [(p, Orientation::Portrait), (p, Orientation::Landscape)]) {
            let (w, h) = paper.size_mm(o);
            for line in content(paper, o).lines().filter(|l| l.ends_with(" re")) {
                let v: Vec<f64> = line.split(' ').take(4).map(|x| x.parse().unwrap()).collect();
                assert!(v[0] >= 0.0 && v[1] >= 0.0 && v[0] + v[2] <= pt(w) + 0.01 && v[1] + v[3] <= pt(h) + 0.01);
            }
        }
    }
}
