//! Matrix product for the convolutions: Apple Accelerate (`cblas_sgemm`) on macOS, `matrixmultiply` (SIMD)
//! elsewhere.

#[cfg(target_os = "macos")]
#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn cblas_sgemm(order: i32, ta: i32, tb: i32, m: i32, n: i32, k: i32, alpha: f32, a: *const f32, lda: i32, b: *const f32, ldb: i32, beta: f32, c: *mut f32, ldc: i32);
}

/// C[m×n] = A[m×k] · B[k×n] (all row-major); `c` is overwritten.
pub fn matmul_into(a: &[f32], b: &[f32], c: &mut [f32], m: usize, k: usize, n: usize) {
    assert_eq!(a.len(), m * k);
    assert_eq!(b.len(), k * n);
    assert_eq!(c.len(), m * n);
    #[cfg(target_os = "macos")]
    if !plain_forced() {
        unsafe {
            cblas_sgemm(101, 111, 111, m as i32, n as i32, k as i32, 1.0, a.as_ptr(), k as i32, b.as_ptr(), n as i32, 0.0, c.as_mut_ptr(), n as i32);
        }
        return;
    }
    portable(a, b, c, m, k, n);
}

/// Portable product through `matrixmultiply` (SIMD kernels for x86-64 and ARM, selected at run time).
/// `GRAINEO_FACE_LOOP_GEMM` uses the simple loop [`plain`] instead (for measurements).
fn portable(a: &[f32], b: &[f32], c: &mut [f32], m: usize, k: usize, n: usize) {
    static LOOP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOOP.get_or_init(|| std::env::var_os("GRAINEO_FACE_LOOP_GEMM").is_some()) {
        return plain(a, b, c, m, k, n);
    }
    // SAFETY: the slices have exactly the sizes asserted in matmul_into; strides describe row-major layouts
    unsafe {
        matrixmultiply::sgemm(m, k, n, 1.0, a.as_ptr(), k as isize, 1, b.as_ptr(), n as isize, 1, 0.0, c.as_mut_ptr(), n as isize, 1);
    }
}

/// `GRAINEO_FACE_PLAIN_GEMM`: use the portable product on macOS too (for measurements, as on Windows/Linux).
#[cfg(target_os = "macos")]
fn plain_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| std::env::var_os("GRAINEO_FACE_PLAIN_GEMM").is_some())
}

/// Portable product: for each row of A, the rows of B are added with their factor (the inner loop runs over
/// contiguous memory and is vectorised by the compiler). Blocked over `k` so that the touched rows of B stay in the cache.
fn plain(a: &[f32], b: &[f32], c: &mut [f32], m: usize, k: usize, n: usize) {
    c.fill(0.0);
    const KB: usize = 64;
    const NB: usize = 1024;
    for n0 in (0..n).step_by(NB) {
        let n1 = (n0 + NB).min(n);
        for k0 in (0..k).step_by(KB) {
            let k1 = (k0 + KB).min(k);
            for i in 0..m {
                let row = &mut c[i * n + n0..i * n + n1];
                for p in k0..k1 {
                    let x = a[i * k + p];
                    if x == 0.0 {
                        continue;
                    }
                    let brow = &b[p * n + n0..p * n + n1];
                    for (o, v) in row.iter_mut().zip(brow) {
                        *o += x * v;
                    }
                }
            }
        }
    }
}

/// C[m×n] = A[m×k] · B[k×n], new buffer.
pub fn matmul(a: &[f32], b: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
    let mut c = vec![0f32; m * n];
    matmul_into(a, b, &mut c, m, k, n);
    c
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_matches_naive() {
        let (m, k, n) = (7, 131, 1500);
        let a: Vec<f32> = (0..m * k).map(|i| ((i * 37 % 101) as f32 - 50.0) / 25.0).collect();
        let b: Vec<f32> = (0..k * n).map(|i| ((i * 13 % 97) as f32 - 48.0) / 30.0).collect();
        let mut c = vec![0f32; m * n];
        super::plain(&a, &b, &mut c, m, k, n);
        let mut d = vec![0f32; m * n];
        super::portable(&a, &b, &mut d, m, k, n);
        let accelerated = super::matmul(&a, &b, m, k, n);
        for i in 0..m {
            for j in 0..n {
                let s: f32 = (0..k).map(|p| a[i * k + p] * b[p * n + j]).sum();
                assert!((c[i * n + j] - s).abs() < 1e-3, "{i},{j}: {} vs {s}", c[i * n + j]);
                assert!((accelerated[i * n + j] - s).abs() < 1e-3);
                assert!((d[i * n + j] - s).abs() < 1e-3);
            }
        }
    }
}
