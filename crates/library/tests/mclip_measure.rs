//! Measurement of the text models of the semantic search on a text→image test set (XTD10 format:
//! `cap/<lang>.txt` with one caption per line, `cap/names.txt` with the image names, images in
//! `images/`). Not part of the normal test run (models and data are large and not in the repo):
//!
//! ```sh
//! GRAINEO_XTD=… GRAINEO_CLIP_DIR=…/0_CLIPModel GRAINEO_STD_DIR=… GRAINEO_MCLIP_DIR=… \
//!   cargo test --release -p library --test mclip_measure -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

use library::semantic::{ImageEncoder, TextEncoder};
use library::xlmr::MclipEncoder;

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(k).unwrap_or_else(|| panic!("{k} fehlt")))
}

fn max_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    // macOS: bytes
    u.ru_maxrss as f64 / 1e6
}

fn metrics(q: &[Vec<f32>], img: &[Vec<f32>]) -> String {
    let mut ranks: Vec<usize> = q
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let dot = |x: &Vec<f32>| x.iter().zip(v).map(|(a, b)| a * b).sum::<f32>();
            let own = dot(&img[i]);
            img.iter().filter(|x| dot(x) > own).count() + 1
        })
        .collect();
    let n = ranks.len() as f64;
    let r = |k: usize| 100.0 * ranks.iter().filter(|&&x| x <= k).count() as f64 / n;
    let mean = ranks.iter().sum::<usize>() as f64 / n;
    let (r1, r5, r10) = (r(1), r(5), r(10));
    ranks.sort();
    format!("R@1 {r1:.1} | R@5 {r5:.1} | R@10 {r10:.1} | MedR {} | MeanR {mean:.1}", ranks[ranks.len() / 2])
}

#[test]
#[ignore]
fn xtd10() {
    let xtd = env("GRAINEO_XTD");
    let cap = |l: &str| -> Vec<String> { std::fs::read_to_string(xtd.join("cap").join(format!("{l}.txt"))).unwrap().split('\n').take(1000).map(str::to_string).collect() };
    let names = cap("names");
    // image vectors (cached next to the data)
    let cache = xtd.join("image-vectors-rust.bin");
    let img: Vec<Vec<f32>> = if let Ok(b) = std::fs::read(&cache) {
        b.chunks_exact(2048).map(|c| c.chunks_exact(4).map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]])).collect()).collect()
    } else {
        let enc = ImageEncoder::load(&env("GRAINEO_CLIP_DIR")).unwrap();
        let t = Instant::now();
        let v: Vec<Vec<f32>> = names.iter().map(|n| enc.embed(&image::open(xtd.join("images").join(n)).unwrap().to_rgb8())).collect();
        eprintln!("Image vectors: {:?} per image", t.elapsed() / 1000);
        std::fs::write(&cache, v.iter().flatten().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        v
    };
    assert_eq!(img.len(), 1000);
    let langs = ["de", "en", "fr", "es", "jp"];

    let std_enc = TextEncoder::load(&env("GRAINEO_STD_DIR")).unwrap();
    let mut standard = std::collections::HashMap::new();
    for l in langs {
        let t = Instant::now();
        let q: Vec<Vec<f32>> = cap(l).iter().map(|s| std_enc.embed(s)).collect();
        eprintln!("Standard   {l}: {}  ({:?} je Anfrage)", metrics(&q, &img), t.elapsed() / 1000);
        standard.insert(l, q);
    }
    drop(std_enc);

    let rss0 = max_rss_mb();
    let t = Instant::now();
    let dir = env("GRAINEO_MCLIP_DIR");
    let m = match std::env::var_os("GRAINEO_MCLIP_FILE") {
        Some(f) => MclipEncoder::load_files(library::xlmr::Unigram::load(&dir.join("tokenizer.json")).unwrap(), &dir.join(f)).unwrap(),
        None => MclipEncoder::load(&dir).unwrap(),
    };
    eprintln!("M-CLIP loaded in {:?}, peak memory {:.0} → {:.0} MB", t.elapsed(), rss0, max_rss_mb());
    let mut mclip = std::collections::HashMap::new();
    for l in langs {
        let c = cap(l);
        let t = Instant::now();
        let q: Vec<Vec<f32>> = c.iter().map(|s| m.embed(s)).collect();
        let single = t.elapsed() / 1000;
        eprintln!("M-CLIP     {l}: {}  ({single:?} je Anfrage)", metrics(&q, &img));
        mclip.insert(l, q);
    }
    // batch throughput (as for the AI descriptions in the background)
    let c = cap("de");
    let refs: Vec<&str> = c.iter().map(String::as_str).collect();
    let t = Instant::now();
    let b: Vec<Vec<f32>> = refs.chunks(8).flat_map(|ch| m.embed_batch(ch)).collect();
    eprintln!("M-CLIP batch of 8: {:?} per sentence; same as single: {}", t.elapsed() / 1000, b.iter().zip(&mclip["de"]).all(|(a, c)| a.iter().zip(c).map(|(x, y)| x * y).sum::<f32>() > 0.99999));
    // single query, warm, short German search words
    let words = ["Hund im Schnee", "Kind auf Schaukel", "Sonnenuntergang am Meer", "Mann mit Hut", "Katze auf dem Sofa"];
    let t = Instant::now();
    for _ in 0..6 {
        for w in words {
            std::hint::black_box(m.embed(w));
        }
    }
    eprintln!("M-CLIP short search words: {:?} per query; peak memory {:.0} MB", t.elapsed() / 30, max_rss_mb());

    // description vectors from the other model: German query (M-CLIP) against English texts
    // embedded with the standard model, compared with both sides in one model
    eprintln!("Text→Text (de-Anfrage gegen en-Texte als „Beschreibungen“):");
    eprintln!("  Standard → Standard: {}", metrics(&standard["de"], &standard["en"]));
    eprintln!("  M-CLIP   → M-CLIP:   {}", metrics(&mclip["de"], &mclip["en"]));
    eprintln!("  M-CLIP   → Standard: {}", metrics(&mclip["de"], &standard["en"]));
}

/// Load time, query time and memory of one model alone (run on its own, so the peak memory belongs
/// to it): `GRAINEO_MEASURE_MODEL=std|mclip`.
#[test]
#[ignore]
fn time_and_memory() {
    let which = std::env::var("GRAINEO_MEASURE_MODEL").unwrap_or_else(|_| "mclip".into());
    let rss0 = max_rss_mb();
    let t = Instant::now();
    let enc: Box<dyn Fn(&str) -> Vec<f32>> = if which == "std" {
        let e = TextEncoder::load(&env("GRAINEO_STD_DIR")).unwrap();
        Box::new(move |s| e.embed(s))
    } else {
        let e = MclipEncoder::load(&env("GRAINEO_MCLIP_DIR")).unwrap();
        Box::new(move |s| e.embed(s))
    };
    let load = t.elapsed();
    let words = ["Hund im Schnee", "Kind auf Schaukel", "Sonnenuntergang am Meer", "Mann mit Hut", "Katze auf dem Sofa", "Hochzeit im Garten"];
    enc(words[0]);
    let mut times = Vec::new();
    for _ in 0..5 {
        for w in words {
            let t = Instant::now();
            std::hint::black_box(enc(w));
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(f64::total_cmp);
    let long = "Hund, Schnee, Winter, Wald. Ein brauner Hund läuft durch tiefen Schnee zwischen Nadelbäumen, im Hintergrund ein verschneiter Weg. Typische Winterszene mit weichem Licht.";
    let t = Instant::now();
    for _ in 0..5 {
        std::hint::black_box(enc(long));
    }
    eprintln!(
        "{which}: loaded in {load:?}; query median {:.0} ms (min {:.0}, max {:.0}); description ~40 tokens {:.0} ms; peak memory {:.0} → {:.0} MB",
        times[times.len() / 2],
        times[0],
        times[times.len() - 1],
        t.elapsed().as_secs_f64() * 200.0,
        rss0,
        max_rss_mb()
    );
}
