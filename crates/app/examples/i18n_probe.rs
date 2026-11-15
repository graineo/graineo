//! Small measurement harness; no photo or library access.
// This harness intentionally exercises only the catalog subset of the production API.
#[allow(dead_code, unused_imports)]
#[path = "../src/i18n/mod.rs"]
mod i18n;
#[allow(dead_code)]
#[path = "../src/env.rs"]
mod env;
fn main() {
    let start = std::time::Instant::now();
    let catalogs = i18n::catalogs();
    println!("catalog_load_us={}", start.elapsed().as_micros());
    let start = std::time::Instant::now();
    for _ in 0..10_000 {
        std::hint::black_box(catalogs.photos(i18n::Language::En, 2));
    }
    println!("format_10000_us={}", start.elapsed().as_micros());
    println!(
        "{} / {}",
        catalogs.photos(i18n::Language::De, 2),
        catalogs.photos(i18n::Language::En, 2)
    );
}
