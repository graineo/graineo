//! graineo: parity measurement against the Python original (spektrafilm).
//!
//! `paritaet <data_dir> <film> <print> <params.json|-> <in.f64> <width> <height> <out_prefix> [cpu|gpu]`
//!
//! Reads a little-endian f64 RGB image, runs the calibrated pipeline and writes
//! `<out_prefix>.rgb_out.f64`. With the CPU backend the intermediate taps are written
//! too (`log_e_film`, `cmy_film`, `cmy_print`) through the pipeline's dump hooks.
//! `params.json` patches `RuntimeParams` (only the fields present).
use spektrafilm_core::params::RuntimeParams;
use spektrafilm_core::pipeline::Pipeline;
use spektrafilm_core::profile;
use spektrafilm_gpu::ComputeBackend;
use spektrafilm_math::image::ImageBuf;
use spektrafilm_math::precision::from_f64;
use std::path::Path;

fn merge(a: &mut serde_json::Value, b: serde_json::Value) {
    match (a, b) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (a, b) => *a = b,
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 9 {
        eprintln!("usage: paritaet <data_dir> <film> <print> <params.json|-> <in.f64> <w> <h> <out_prefix> [cpu|gpu]");
        std::process::exit(2);
    }
    let dir = Path::new(&a[1]);
    let mut params = RuntimeParams::default();
    if a[4] != "-" {
        let patch: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&a[4]).unwrap()).unwrap();
        let mut base = serde_json::to_value(&params).unwrap();
        merge(&mut base, patch);
        params = serde_json::from_value(base).unwrap();
    }
    let (w, h): (u32, u32) = (a[6].parse().unwrap(), a[7].parse().unwrap());
    let bytes = std::fs::read(&a[5]).unwrap();
    let data = bytes.chunks_exact(8).map(|c| from_f64(f64::from_le_bytes(c.try_into().unwrap()))).collect();
    let image = ImageBuf::from_data(w, h, data);
    let out = &a[8];
    let gpu = a.get(9).is_some_and(|b| b == "gpu");
    let film = profile::load_profile_by_name(dir, &a[2]).unwrap();
    let print = profile::load_profile_by_name(dir, &a[3]).unwrap();
    // SAFETY: single-threaded at this point; the dump hooks read these variables.
    unsafe {
        std::env::set_var("SPEKTRAFILM_DUMP_FILM_LOG_RAW", format!("{out}.log_e_film.f64"));
        std::env::set_var("SPEKTRAFILM_DUMP_FILM_DENSITY", format!("{out}.cmy_film.f64"));
        std::env::set_var("SPEKTRAFILM_DUMP_PRINT_DENSITY", format!("{out}.cmy_print.f64"));
    }
    let pipe = Pipeline::new_with_spectral(film, print, params, dir).unwrap();
    let cpu = spektrafilm_gpu::cpu_backend::CpuBackend;
    let gpu_backend = if gpu { spektrafilm_gpu::wgpu_backend::WgpuBackend::new() } else { None };
    let backend: &dyn ComputeBackend = match gpu_backend.as_ref() {
        Some(g) => g,
        None => {
            if gpu {
                eprintln!("no GPU, computing on the CPU");
            }
            &cpu
        }
    };
    let t = std::time::Instant::now();
    let res = pipe.process(image, backend);
    eprintln!("{}: {:.1?}", backend.name(), t.elapsed());
    let bytes: Vec<u8> = res.data.iter().flat_map(|&v| (v as f64).to_le_bytes()).collect();
    std::fs::write(format!("{out}.rgb_out.f64"), bytes).unwrap();
}
