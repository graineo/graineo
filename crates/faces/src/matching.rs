//! Compare features: similarities (dot product of normalized vectors) of many queries against many
//! references, compute core via Accelerate; plus helpers for grouping.

#[cfg(target_os = "macos")]
#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn cblas_sgemm(order: i32, ta: i32, tb: i32, m: i32, n: i32, k: i32, alpha: f32, a: *const f32, lda: i32, b: *const f32, ldb: i32, beta: f32, c: *mut f32, ldc: i32);
}

/// C[n × m] = A[n × d] · B[m × d]ᵀ, all row-major.
pub fn similarities(a: &[f32], n: usize, b: &[f32], m: usize, d: usize) -> Vec<f32> {
    assert_eq!(a.len(), n * d);
    assert_eq!(b.len(), m * d);
    let mut c = vec![0f32; n * m];
    if n == 0 || m == 0 {
        return c;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        cblas_sgemm(101, 111, 112, n as i32, m as i32, d as i32, 1.0, a.as_ptr(), d as i32, b.as_ptr(), d as i32, 0.0, c.as_mut_ptr(), m as i32);
    }
    #[cfg(not(target_os = "macos"))]
    for i in 0..n {
        for j in 0..m {
            c[i * m + j] = (0..d).map(|k| a[i * d + k] * b[j * d + k]).sum();
        }
    }
    c
}

/// Union-find for groups.
pub struct Groups {
    parent: Vec<usize>,
}

impl Groups {
    pub fn new(n: usize) -> Groups {
        Groups { parent: (0..n).collect() }
    }
    pub fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }
    pub fn merge(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra.max(rb)] = ra.min(rb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_products() {
        let a = [1.0, 0.0, 0.0, 1.0];
        let b = [1.0, 0.0, 0.6, 0.8, 0.0, 1.0];
        let c = similarities(&a, 2, &b, 3, 2);
        assert_eq!(c, vec![1.0, 0.6, 0.0, 0.0, 0.8, 1.0]);
    }

    #[test]
    fn groups_merge() {
        let mut g = Groups::new(5);
        g.merge(1, 3);
        g.merge(3, 4);
        assert_eq!(g.find(4), g.find(1));
        assert_ne!(g.find(0), g.find(1));
    }
}
