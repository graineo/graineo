//! Times per stage at full resolution: `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test timings -- --ignored --nocapture`

use std::time::Instant;

#[test]
#[ignore]
fn stages() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let p = std::path::PathBuf::from(p);
    let t = Instant::now();
    let f = raw::RawFile::open(&p).unwrap();
    println!("open: {:.0?}", t.elapsed());
    let t = Instant::now();
    let b = f.basis(raw::development::Demosaic::Auto, true, true, raw::development::Size::Full).unwrap();
    println!("base full {}×{}: {:.0?} {:?}", b.width, b.height, t.elapsed(), b.timings);
    let t = Instant::now();
    let doc = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    println!("RawDoc::open (preview): {:.0?}", t.elapsed());
    let s = develop::Settings::default();
    let t = Instant::now();
    let _vb = doc.full_base().unwrap();
    println!("voll_basis: {:.0?}", t.elapsed());
    let t = Instant::now();
    let st = doc.full_stage(&s).unwrap();
    println!("voll_stufe (NR/sharpening/lens): {:.0?}", t.elapsed());
    let t = Instant::now();
    let _m = st.mask(doc.lum());
    println!("mask: {:.0?}", t.elapsed());
    let t = Instant::now();
    let _img = doc.render_stage(&st, &s);
    println!("render_stufe (tone/color/look): {:.0?}", t.elapsed());
    let mut s2 = s.clone();
    s2.exposure += 0.3;
    let t = Instant::now();
    let _img = doc.render_stage(&st, &s2);
    println!("render_stufe after slider move: {:.0?}", t.elapsed());
}
