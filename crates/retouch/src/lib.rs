//! CPU reference kernel for linear RGB, without color space conversion or clamping to 0…1.
//! The image and the source region stay unchanged. All coordinates are pixel coordinates.
//! Masks/sources need a one-pixel margin inside the image (Dirichlet boundary condition).
//! Own implementation of the discrete Poisson equation with conjugate gradients;
//! no third-party code adopted.
use rayon::prelude::*;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}

/// Packed RGBRGB… in one shared linear color space, also HDR/negative values.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    width: usize,
    height: usize,
    data: &'a [f32],
}
impl<'a> Image<'a> {
    pub fn new(width: usize, height: usize, data: &'a [f32]) -> Result<Self> {
        if width == 0
            || height == 0
            || width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(data.len())
        {
            return Err(Error("Bildabmessungen passen nicht zum RGB-Puffer"));
        }
        if !data.par_iter().all(|v| v.is_finite()) {
            return Err(Error("Bild enthält NaN oder Unendlich"));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    fn at(&self, x: usize, y: usize, c: usize) -> f64 {
        self.data[(y * self.width + x) * 3 + c] as f64
    }
}

/// Dense, local opacity mask. Zero values are holes and are preserved bit-exactly.
#[derive(Clone, Debug)]
pub struct Mask {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    weights: Vec<f32>,
}
impl Mask {
    pub fn new(x: usize, y: usize, width: usize, height: usize, weights: Vec<f32>) -> Result<Self> {
        if width == 0
            || height == 0
            || width.checked_mul(height) != Some(weights.len())
            || x.checked_add(width).is_none()
            || y.checked_add(height).is_none()
            || weights
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err(Error("Ungültige Maske"));
        }
        Ok(Self {
            x,
            y,
            width,
            height,
            weights,
        })
    }
    /// Circle at a pixel center; hardness=0: fully soft, 1: hard edge.
    pub fn circle(center: [f32; 2], radius: f32, hardness: f32) -> Result<Self> {
        Self::brush(&[center], radius, hardness)
    }
    /// Continuous brush stroke as a union of capsules, without gaps at large spacings.
    pub fn brush(points: &[[f32; 2]], radius: f32, hardness: f32) -> Result<Self> {
        if points.is_empty()
            || !radius.is_finite()
            || radius <= 0.0
            || !hardness.is_finite()
            || !(0.0..=1.0).contains(&hardness)
            || points.iter().flatten().any(|p| !p.is_finite() || *p < 0.0)
        {
            return Err(Error("Ungültiger Pinselzug"));
        }
        let mut lo = [f32::INFINITY; 2];
        let mut hi = [0.0f32; 2];
        for p in points {
            for c in 0..2 {
                lo[c] = lo[c].min(p[c]);
                hi[c] = hi[c].max(p[c]);
            }
        }
        // Allocation budget keeps malformed geometry from triggering huge allocations.
        let x = (lo[0] - radius).max(0.0).floor() as usize;
        let y = (lo[1] - radius).max(0.0).floor() as usize;
        let w = (hi[0] + radius).ceil() as usize;
        let h = (hi[1] + radius).ceil() as usize;
        let width = w
            .checked_sub(x)
            .and_then(|n| n.checked_add(1))
            .ok_or(Error("Maske zu groß"))?;
        let height = h
            .checked_sub(y)
            .and_then(|n| n.checked_add(1))
            .ok_or(Error("Maske zu groß"))?;
        let n = width
            .checked_mul(height)
            .filter(|&n| n <= 64_000_000)
            .ok_or(Error("Maske zu groß"))?;
        let weights = (0..n)
            .into_par_iter()
            .map(|i| {
                let p = [(x + i % width) as f32, (y + i / width) as f32];
                let distance = if points.len() == 1 {
                    (p[0] - points[0][0]).hypot(p[1] - points[0][1])
                } else {
                    points
                        .windows(2)
                        .map(|s| {
                            let v = [s[1][0] - s[0][0], s[1][1] - s[0][1]];
                            let t = (((p[0] - s[0][0]) * v[0] + (p[1] - s[0][1]) * v[1])
                                / (v[0] * v[0] + v[1] * v[1]).max(f32::MIN_POSITIVE))
                            .clamp(0.0, 1.0);
                            (p[0] - s[0][0] - t * v[0]).hypot(p[1] - s[0][1] - t * v[1])
                        })
                        .fold(f32::INFINITY, f32::min)
                };
                if distance >= radius {
                    0.0
                } else if distance <= radius * hardness {
                    1.0
                } else {
                    let t = (radius - distance) / (radius * (1.0 - hardness));
                    t * t * (3.0 - 2.0 * t)
                }
            })
            .collect();
        Self::new(x, y, width, height, weights)
    }
    pub fn bounds(&self) -> [usize; 4] {
        [self.x, self.y, self.width, self.height]
    }
    pub fn weights(&self) -> &[f32] {
        &self.weights
    }
    fn validate(&self, image: Image<'_>, source: Source) -> Result<()> {
        let sx = self.x as i128 + source.dx as i128;
        let sy = self.y as i128 + source.dy as i128;
        if self.x < 1
            || self.y < 1
            || self.x + self.width >= image.width
            || self.y + self.height >= image.height
            || sx < 1
            || sy < 1
            || sx + self.width as i128 >= image.width as i128
            || sy + self.height as i128 >= image.height as i128
        {
            return Err(Error(
                "Maske oder Quelle hat keinen vollständigen Rand im Bild",
            ));
        }
        Ok(())
    }
}
/// Integer source offset relative to the target mask. Overlap allowed for manual selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub dx: i32,
    pub dy: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Clone,
    Heal,
}
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub method: Method,
    pub max_iterations: usize,
    pub tolerance: f64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            method: Method::Heal,
            max_iterations: 1000,
            tolerance: 1e-7,
        }
    }
}
/// Error of the linear system relative to the right-hand side, maximum per channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct Report {
    pub iterations: usize,
    pub relative_residual: f64,
    pub converged: bool,
}
/// Local patch as shared CPU/GPU output. Only `apply` copies the whole image.
pub struct Patch {
    bounds: [usize; 4],
    rgb: Vec<f32>,
    pub report: Report,
    image_size: [usize; 2],
}
impl Patch {
    pub fn bounds(&self) -> [usize; 4] {
        self.bounds
    }
    pub fn rgb(&self) -> &[f32] {
        &self.rgb
    }
    pub fn apply(&self, image: Image<'_>) -> Result<Vec<f32>> {
        if self.image_size != [image.width, image.height] {
            return Err(Error("Patch passt nicht zum Bild"));
        }
        let [x, y, w, h] = self.bounds;
        let mut out = image.data.to_vec();
        out[y * image.width * 3..(y + h) * image.width * 3]
            .par_chunks_mut(image.width * 3)
            .enumerate()
            .for_each(|(row, dst)| {
                dst[x * 3..(x + w) * 3].copy_from_slice(&self.rgb[row * w * 3..(row + 1) * w * 3]);
            });
        Ok(out)
    }
}

/// Computes only the mask region. No mutation, no global state, no file access.
pub fn compute(image: Image<'_>, mask: &Mask, source: Source, options: Options) -> Result<Patch> {
    mask.validate(image, source)?;
    if !options.tolerance.is_finite() || options.tolerance <= 0.0 || options.max_iterations == 0 {
        return Err(Error("Ungültige Löserparameter"));
    }
    let [x, y, w, h] = mask.bounds();
    let n = w * h;
    let mut report = Report {
        converged: true,
        ..Report::default()
    };
    let mut correction = vec![[0.0f64; 3]; n];
    if options.method == Method::Heal {
        // Solve A d = b for d = result-source; harmonic correction preserves the source Laplacian.
        let active: Vec<usize> = (0..n).filter(|&i| mask.weights[i] > 0.0).collect();
        let mut map = vec![usize::MAX; n];
        for (k, &i) in active.iter().enumerate() {
            map[i] = k;
        }
        let mut neighbors = vec![[usize::MAX; 4]; active.len()];
        let mut rhs = vec![[0.0; 3]; active.len()];
        for (k, &i) in active.iter().enumerate() {
            let px = x + i % w;
            let py = y + i / w;
            for (j, (nx, ny)) in [(px - 1, py), (px + 1, py), (px, py - 1), (px, py + 1)]
                .into_iter()
                .enumerate()
            {
                let other = if nx >= x && nx < x + w && ny >= y && ny < y + h {
                    map[(ny - y) * w + nx - x]
                } else {
                    usize::MAX
                };
                neighbors[k][j] = other;
                if other == usize::MAX {
                    for (c, value) in rhs[k].iter_mut().enumerate() {
                        *value += image.at(nx, ny, c)
                            - image.at(
                                (nx as i64 + source.dx as i64) as usize,
                                (ny as i64 + source.dy as i64) as usize,
                                c,
                            );
                    }
                }
            }
        }
        for c in 0..3 {
            let b: Vec<f64> = rhs.iter().map(|v| v[c]).collect();
            let (solution, iterations, residual) = solve(&neighbors, &b, options);
            report.iterations = report.iterations.max(iterations);
            report.relative_residual = report.relative_residual.max(residual);
            report.converged &= residual <= options.tolerance;
            for (k, &i) in active.iter().enumerate() {
                correction[i][c] = solution[k];
            }
        }
    }
    let mut rgb = vec![0.0; n * 3];
    rgb.par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(row, dst)| {
            for col in 0..w {
                let i = row * w + col;
                let a = mask.weights[i] as f64;
                for c in 0..3 {
                    let original = image.at(x + col, y + row, c);
                    // Preserve signed zero and all untouched samples bit-for-bit.
                    dst[col * 3 + c] = if a == 0.0 {
                        image.data[((y + row) * image.width + x + col) * 3 + c]
                    } else {
                        let s = image.at(
                            (x as i64 + col as i64 + source.dx as i64) as usize,
                            (y as i64 + row as i64 + source.dy as i64) as usize,
                            c,
                        );
                        (original * (1.0 - a) + (s + correction[i][c]) * a) as f32
                    };
                }
            }
        });
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(Error("Retusche überschreitet den f32-Wertebereich"));
    }
    Ok(Patch {
        bounds: mask.bounds(),
        rgb,
        report,
        image_size: [image.width, image.height],
    })
}
fn solve(neighbors: &[[usize; 4]], b: &[f64], options: Options) -> (Vec<f64>, usize, f64) {
    let mut x = vec![0.0; b.len()];
    let mut r = b.to_vec();
    let mut p = r.clone();
    let mut ap = vec![0.0; b.len()];
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
    let initial = dot(b, b);
    if initial == 0.0 {
        return (x, 0, 0.0);
    }
    let mut rr = initial;
    for iteration in 1..=options.max_iterations {
        let multiply = |i: usize| {
            4.0 * p[i]
                - neighbors[i]
                    .iter()
                    .filter(|&&j| j != usize::MAX)
                    .map(|&j| p[j])
                    .sum::<f64>()
        };
        if b.len() >= 32768 {
            ap.par_iter_mut()
                .enumerate()
                .for_each(|(i, a)| *a = multiply(i));
        } else {
            for (i, a) in ap.iter_mut().enumerate() {
                *a = multiply(i);
            }
        }
        let alpha = rr / dot(&p, &ap);
        for i in 0..b.len() {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        let next = dot(&r, &r);
        let residual = (next / initial).sqrt();
        if residual <= options.tolerance || iteration == options.max_iterations {
            let actual = neighbors
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let ax = 4.0 * x[i]
                        - row
                            .iter()
                            .filter(|&&j| j != usize::MAX)
                            .map(|&j| x[j])
                            .sum::<f64>();
                    (b[i] - ax).powi(2)
                })
                .sum::<f64>();
            return (x, iteration, (actual / initial).sqrt());
        }
        let beta = next / rr;
        for i in 0..p.len() {
            p[i] = r[i] + beta * p[i];
        }
        rr = next;
    }
    unreachable!()
}

#[derive(Clone, Copy, Debug)]
pub struct Search {
    pub radius: u32,
    pub step: u32,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            radius: 256,
            step: 8,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Match {
    pub source: Source,
    pub mean_squared_error: f64,
    pub candidates: usize,
}
/// Deterministic grid search: RGB error on the unmasked 1-pixel border.
/// Returns a suggestion, no guarantee of semantically matching texture.
pub fn find_source(image: Image<'_>, mask: &Mask, search: Search) -> Result<Match> {
    mask.validate(image, Source { dx: 0, dy: 0 })?;
    if search.step == 0 || search.radius == 0 || search.radius > 4096 {
        return Err(Error("Ungültiges Suchraster (Radius 1…4096)"));
    }
    let [x, y, w, h] = mask.bounds();
    let mut ring = Vec::with_capacity(2 * (w + h) + 4);
    for px in x - 1..=x + w {
        ring.push((px, y - 1));
        ring.push((px, y + h));
    }
    for py in y..y + h {
        ring.push((x - 1, py));
        ring.push((x + w, py));
    }
    // Bound cost for long brush masks. Fixed sampling order is reproducible.
    let stride = ring.len().div_ceil(512).max(1);
    let ring: Vec<_> = ring.into_iter().step_by(stride).collect();
    let axis = (2 * search.radius as usize) / search.step as usize + 1;
    if axis * axis > 65_536 {
        return Err(Error("Suchraster hat mehr als 65536 Kandidaten"));
    }
    let r = search.radius as i32;
    let step = search.step as usize;
    let offsets: Vec<_> = (-r..=r)
        .step_by(step)
        .flat_map(|dy| (-r..=r).step_by(step).map(move |dx| Source { dx, dy }))
        .filter(|s| {
            (s.dx.unsigned_abs() as usize > w + 1 || s.dy.unsigned_abs() as usize > h + 1)
                && mask.validate(image, *s).is_ok()
        })
        .collect();
    let best = offsets
        .par_iter()
        .enumerate()
        .map(|(index, &source)| {
            let mut score = 0.0;
            for &(px, py) in &ring {
                for c in 0..3 {
                    let d = image.at(px, py, c)
                        - image.at(
                            (px as i64 + source.dx as i64) as usize,
                            (py as i64 + source.dy as i64) as usize,
                            c,
                        );
                    score += d * d;
                }
            }
            (score / (ring.len() * 3) as f64, index, source)
        })
        .reduce_with(|a, b| {
            if a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).is_le() {
                a
            } else {
                b
            }
        })
        .ok_or(Error("Keine überlappungsfreie Quelle im Suchbereich"))?;
    Ok(Match {
        source: best.2,
        mean_squared_error: best.0,
        candidates: offsets.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conjugate_gradient_solves_known_system() {
        // [4 -1; -1 4] [1,2] = [2,7]
        let neighbors = [
            [1, usize::MAX, usize::MAX, usize::MAX],
            [0, usize::MAX, usize::MAX, usize::MAX],
        ];
        let (x, _, residual) = solve(&neighbors, &[2., 7.], Options::default());
        assert!((x[0] - 1.).abs() < 1e-12 && (x[1] - 2.).abs() < 1e-12);
        assert!(residual < 1e-12);
    }
}
