//! Time of the detail stage (full resolution), repeated: `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test gpu_time -- --ignored --nocapture`

use std::time::Instant;

#[test]
#[ignore]
fn timings() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let doc = rawdev::RawDoc::open(std::path::Path::new(&p), &|| true).unwrap().unwrap();
    let mut s = develop::Settings::default();
    s.raw.nr_luma = 30.0;
    let _ = doc.full_base().unwrap();
    for path_kind in ["GPU", "CPU"] {
        if path_kind == "CPU" {
            pools::set_gpu_allowed(false); // `GRAINEO_GPU` is read only once per process
        }
        let mut z = Vec::new();
        for _ in 0..4 {
            let t = Instant::now();
            let _ = doc.full_stage_fresh(&s).unwrap();
            z.push(t.elapsed().as_millis());
        }
        println!("{path_kind}: {z:?} ms");
    }
}
