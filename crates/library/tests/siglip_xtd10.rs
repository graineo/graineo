//! Text → image retrieval on XTD10 (1000 COCO photos, one caption per language) with graineo's own
//! implementation of each search stage. Only runs when the data is there:
//!
//! XTD_DIR=<folder with cap/{de,en,es,fr,jp,names}.txt and ex/XTD10_dataset/*.jpg>
//! XTD_STAGE=clip | <SigLIP model folder>
//! CLIP_IMAGE=<clip-ViT-B-32/0_CLIPModel folder>, CLIP_TEXT=<clip-ViT-B-32-multilingual-v1 folder> (for `clip`)
//!
//! Prints R@1/5/10, median and mean rank per language and the times per photo and per query.

use std::path::PathBuf;
use std::time::Instant;

fn metrics(img: &[Vec<f32>], txt: &[Vec<f32>]) -> (f64, f64, f64, f64, f64) {
    let n = img.len();
    let mut ranks: Vec<usize> = (0..n)
        .map(|q| {
            let s: Vec<f32> = img.iter().map(|v| v.iter().zip(&txt[q]).map(|(a, b)| a * b).sum()).collect();
            1 + s.iter().filter(|&&x| x > s[q]).count()
        })
        .collect();
    let r = |k: usize| 100.0 * ranks.iter().filter(|&&x| x <= k).count() as f64 / n as f64;
    let (r1, r5, r10) = (r(1), r(5), r(10));
    let mean = ranks.iter().sum::<usize>() as f64 / n as f64;
    ranks.sort_unstable();
    (r1, r5, r10, ranks[n / 2] as f64, mean)
}

#[test]
fn xtd10() {
    let (Some(dir), Some(stage)) = (std::env::var_os("XTD_DIR").map(PathBuf::from), std::env::var("XTD_STAGE").ok()) else { return };
    let names: Vec<String> = std::fs::read_to_string(dir.join("cap/names.txt")).unwrap().split_whitespace().map(String::from).collect();
    let limit: usize = std::env::var("XTD_N").ok().and_then(|v| v.parse().ok()).unwrap_or(1000);
    let names = &names[..limit];
    let langs = ["de", "en", "es", "fr", "jp"];
    let caps: Vec<Vec<String>> = langs.iter().map(|l| std::fs::read_to_string(dir.join(format!("cap/{l}.txt"))).unwrap().split('\n').take(limit).map(String::from).collect()).collect();
    // photos are decoded batch by batch (1000 decoded photos would take ~1 GB); decoding time counted separately
    let decode_total = std::cell::Cell::new(0.0f64);
    let load_photos = |ns: &[String]| -> Vec<image::RgbImage> {
        let t = Instant::now();
        let v = ns.iter().map(|n| image::open(dir.join("ex/XTD10_dataset").join(n)).unwrap().to_rgb8()).collect();
        decode_total.set(decode_total.get() + t.elapsed().as_secs_f64() * 1000.0);
        v
    };
    let (img, embed_ms, txt, query_ms, load_ms): (Vec<Vec<f32>>, f64, Vec<Vec<Vec<f32>>>, f64, f64);
    if stage == "clip" {
        let t = Instant::now();
        let ie = library::semantic::ImageEncoder::load(&PathBuf::from(std::env::var("CLIP_IMAGE").unwrap())).unwrap();
        let te = library::semantic::TextEncoder::load(&PathBuf::from(std::env::var("CLIP_TEXT").unwrap())).unwrap();
        load_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        img = names.chunks(4).flat_map(|c| load_photos(c).iter().map(|p| ie.embed(p)).collect::<Vec<_>>()).collect();
        embed_ms = (t.elapsed().as_secs_f64() * 1000.0 - decode_total.get()) / limit as f64;
        let t = Instant::now();
        txt = caps.iter().map(|c| c.iter().map(|s| te.embed(s)).collect()).collect();
        query_ms = t.elapsed().as_secs_f64() * 1000.0 / (5 * limit) as f64;
    } else {
        let m = PathBuf::from(&stage);
        // one tower at a time (as in the app: reading photos and searching never need both)
        let t = Instant::now();
        let it = library::siglip::ImageTower::load(&m).unwrap();
        let image_load = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        let mut v = Vec::new();
        for c in names.chunks(4) {
            let ph = load_photos(c);
            let refs: Vec<&image::RgbImage> = ph.iter().collect();
            v.extend(it.embed(&refs));
        }
        img = v;
        embed_ms = (t.elapsed().as_secs_f64() * 1000.0 - decode_total.get()) / limit as f64;
        drop(it);
        let t = Instant::now();
        let tt = library::siglip::TextTower::load(&m).unwrap();
        load_ms = image_load + t.elapsed().as_secs_f64() * 1000.0;
        // time of single queries as in the app (one sentence at a time), on 50 sentences
        let t = Instant::now();
        for s in caps[0].iter().take(50) {
            tt.embed_one(s).unwrap();
        }
        query_ms = t.elapsed().as_secs_f64() * 1000.0 / 50.0;
        // all captions in batches (same vectors, faster)
        txt = caps
            .iter()
            .map(|c| c.chunks(32).flat_map(|b| tt.embed(&b.iter().map(|s| s.as_str()).collect::<Vec<_>>()).unwrap()).collect())
            .collect();
    }
    let decode_ms = decode_total.get() / limit as f64;
    println!("stage {stage}\nload {load_ms:.0} ms · decode {decode_ms:.1} ms/photo · image {embed_ms:.1} ms/photo · query {query_ms:.1} ms");
    // relevance values as in the search (z value against the distribution over all photos): how
    // the threshold of the strictness slider behaves with this model. Floor for strictness 0.3 at
    // N photos as in `similar::limits`: 2.8 + 1.6·t + 0.5·max(√(2 ln N) − 3.12, −0.5)
    let n = img.len() as f32;
    let floor = 2.8 + 1.6 * 0.3 + 0.5 * ((2.0 * n.ln()).sqrt() - 3.12).max(-0.5);
    println!("| Language | z searched photo (median) | searched photo above threshold {floor:.2} | Photos above threshold per query (mean) |");
    for (l, t) in langs.iter().zip(&txt) {
        let mut zs = Vec::new();
        let (mut hit, mut above) = (0usize, 0usize);
        for (q, tv) in t.iter().enumerate() {
            let s: Vec<f32> = img.iter().map(|v| v.iter().zip(tv).map(|(a, b)| a * b).sum()).collect();
            let mean = s.iter().sum::<f32>() / n;
            let sd = (s.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n).sqrt().max(1e-4);
            let z = (s[q] - mean) / sd;
            zs.push(z);
            hit += usize::from(z >= floor);
            above += s.iter().filter(|&&x| (x - mean) / sd >= floor).count();
        }
        zs.sort_by(f32::total_cmp);
        println!("| {l} | {:.2} | {:.1} % | {:.1} |", zs[zs.len() / 2], 100.0 * hit as f32 / t.len() as f32, above as f32 / t.len() as f32);
    }
    println!("| Sprache | R@1 | R@5 | R@10 | MedR | MeanR |");
    for (l, t) in langs.iter().zip(&txt) {
        let (a, b, c, d, e) = metrics(&img, t);
        println!("| {l} | {a:.1} | {b:.1} | {c:.1} | {d} | {e:.1} |");
    }
}
