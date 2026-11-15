//! Benchmark of the disk cache of the SPP base:
//! `cargo run --release -p develop --features spp-gpu --example spp_cache_bench -- FILE.X3F`
//! Preferably with `GRAINEO_CACHE_DIR=<tmp>` and `GRAINEO_LOG=1`.
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn fnv(d: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for v in d {
        for b in v.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("file"));
    let key = develop::Settings::default().spp_key(&path);
    let cancel = AtomicBool::new(false);
    develop::diskcache::clear();
    let t = Instant::now();
    let a = develop::spp::working_key(&path, &key, true, &cancel).unwrap();
    println!("recompute 1: {:.0?} (GPU {})  hash {:016x}", t.elapsed(), develop::spp::last_used_gpu(), fnv(&a.data));
    let t = Instant::now();
    let b = develop::spp::working_key(&path, &key, true, &cancel).unwrap();
    println!("recompute 2: {:.0?}  hash {:016x}", t.elapsed(), fnv(&b.data));
    // cache miss: computes and stores in the background
    let t = Instant::now();
    let c = develop::diskcache::working_key(&path, &key, true, &cancel).unwrap();
    println!("cache miss: {:.0?}  hash {:016x}", t.elapsed(), fnv(&c.data));
    let mut last = (0, 0);
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let s = develop::diskcache::stats();
        if s.0 > 0 && s == last {
            break;
        }
        last = s;
    }
    let raw_l = a.detail.as_ref().map_or(0, |d| d.l.len() * 2);
    println!("cache: {:.1} MB for {} image(s), raw {:.1} MB (of which detail image {:.1} MB)", last.0 as f64 / 1e6, last.1, (a.data.len() * 4 + raw_l) as f64 / 1e6, raw_l as f64 / 1e6);
    for i in 0..3 {
        let t = Instant::now();
        let d = develop::diskcache::working_key(&path, &key, true, &cancel).unwrap();
        let el = t.elapsed();
        let same = d.width == a.width && d.height == a.height && d.data.iter().zip(&c.data).all(|(x, y)| x.to_bits() == y.to_bits());
        // detail image "L" for the sharpness slider must be stored too
        let same = same && match (&d.detail, &c.detail) {
            (Some(x), Some(y)) => x.l == y.l && x.base.to_bits() == y.base.to_bits() && x.scale.to_bits() == y.scale.to_bits(),
            _ => false,
        };
        println!("cache hit {}: {:.0?}  hash {:016x}  bit-identical to stored: {same}", i + 1, el, fnv(&d.data));
    }
}
