//! Neural denoising as an optional stage **after** Sigma's noise reduction (every Foveon generation; `SppKey::ai`, off
//! by default). It runs on the finished base: for Quattro and Classic the RGB image after the preparation (lower layers
//! upsampled with the top layer as guide) and after binning, so its size is that of the developed image and the
//! export. The base of the settings ([`crate::diskcache::working_key`]) is then a mix of Sigma's base and the same
//! base run through the network: the full-strength result is computed once per photo and base key (kept in memory
//! and in the disk cache like any base), every other strength is only a mix of the two.
//!
//! The network itself lives outside this crate (`rawdev::ai_denoise`, which installs it with [`install`]); without
//! it the stage leaves the base unchanged. With the stage off nothing here runs, the base is bit-identical.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use crate::{Linear, SppKey};

/// The network on a base in place: interleaved RGB counts (`w × h`, linear, white = `white`). Reports progress and
/// stops early when `cancel` is set (then `Err`).
pub type Denoiser = fn(data: &mut [f32], w: usize, h: usize, white: f32, path: &std::path::Path, cancel: &AtomicBool) -> Result<(), String>;

static HOOK: OnceLock<Denoiser> = OnceLock::new();
static READY: OnceLock<fn() -> bool> = OnceLock::new();

/// Install the network (once, at program start); `ready` tells whether it can run now (model installed).
pub fn install(f: Denoiser, ready: fn() -> bool) {
    let _ = HOOK.set(f);
    let _ = READY.set(ready);
}

/// Can the stage run (network installed and ready)? Without it the key never asks for it ([`SppKey::ai`] = 0).
pub fn installed() -> bool {
    HOOK.get().is_some() && READY.get().is_some_and(|r| r())
}

/// Last full-strength result (path, key text of the base, counts): changing the strength right after the
/// computation mixes from memory instead of computing again while the disk cache is still being written.
static LAST: Mutex<Option<(std::path::PathBuf, String, Arc<Linear>)>> = Mutex::new(None);

/// The base for `key` with `key.ai > 0`: Sigma's base (`key.ai = 0`, from the disk cache or computed) mixed with the
/// denoised one (`key.ai = 100`). `base_for` returns the base of another key (the caller's cached path).
pub(crate) fn working(path: &std::path::Path, key: &SppKey, cancel: &AtomicBool, base_for: &dyn Fn(&SppKey) -> Result<Linear, String>) -> Result<Linear, String> {
    let plain_key = SppKey { ai: 0, ..key.clone() };
    if key.ai >= 100 {
        let mut lin = base_for(&plain_key)?;
        let Some(f) = HOOK.get() else { return Ok(lin) };
        let white = if key.prepared { 65535.0 } else { 32766.0 };
        f(&mut lin.data, lin.width, lin.height, white, path, cancel)?;
        *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), format!("{plain_key:?}"), Arc::new(lin.clone())));
        return Ok(lin);
    }
    let orig = base_for(&plain_key)?;
    let full_key = SppKey { ai: 100, ..key.clone() };
    let remembered = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(p, k, _)| p == path && *k == format!("{plain_key:?}")).map(|e| e.2.clone());
    let den = match remembered {
        Some(d) => d,
        None => Arc::new(base_for(&full_key)?),
    };
    Ok(mix(orig, &den, key.ai))
}

/// `orig` with `p` percent of `den` (same size); the detail image for the sharpness slider stays the original's.
pub(crate) fn mix(mut orig: Linear, den: &Linear, p: u8) -> Linear {
    use rayon::prelude::*;
    if den.data.len() != orig.data.len() {
        return orig;
    }
    let a = f32::from(p.min(100)) / 100.0;
    orig.data.par_iter_mut().zip(den.data.par_iter()).for_each(|(o, d)| *o += a * (d - *o));
    orig
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_blends() {
        let lin = |v: f32| Linear { width: 2, height: 1, data: vec![v; 6], detail: None, haze: None, toneq: None, mono: false };
        let m = mix(lin(4.0), &lin(0.0), 25);
        assert!(m.data.iter().all(|v| (*v - 3.0).abs() < 1e-6));
        assert_eq!(mix(lin(4.0), &lin(0.0), 100).data, vec![0.0; 6]);
    }
}
