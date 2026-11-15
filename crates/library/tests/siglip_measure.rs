//! Comparison of the SigLIP 2 reimplementation with the Python reference (only when the reference
//! data is there; otherwise the tests return at once).
//!
//! SIGLIP_REF=<folder with tokens.json, text_emb.npy, pixels.npy, image_emb.npy …>
//! SIGLIP_MODEL=<model folder with model.safetensors, tokenizer.model>
//! SIGLIP_IMAGES=<folder of the reference photos> (optional: end-to-end check with own preprocessing)

use std::path::PathBuf;

fn env(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).map(PathBuf::from)
}

/// Minimal .npy reader (little-endian f32, C order).
fn npy(p: &std::path::Path) -> (Vec<usize>, Vec<f32>) {
    let b = std::fs::read(p).unwrap();
    assert_eq!(&b[..6], b"\x93NUMPY");
    let hl = u16::from_le_bytes([b[8], b[9]]) as usize;
    let h = std::str::from_utf8(&b[10..10 + hl]).unwrap();
    assert!(h.contains("'<f4'") && h.contains("'fortran_order': False"), "{h}");
    let s = &h[h.find("'shape': (").unwrap() + 10..];
    let shape: Vec<usize> = s[..s.find(')').unwrap()].split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let data = b[10 + hl..].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    (shape, data)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    d / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}

#[test]
fn tokenizer_matches_reference() {
    let (Some(r), Some(m)) = (env("SIGLIP_REF"), env("SIGLIP_MODEL")) else { return };
    let sp = library::sentencepiece::SentencePiece::load(&m.join("tokenizer.model")).unwrap();
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.join("tokens.json")).unwrap()).unwrap();
    let sents = j["sents"].as_array().unwrap();
    let raw = j["raw"].as_array().unwrap();
    let mut bad = 0;
    for (s, ids) in sents.iter().zip(raw) {
        let s = s.as_str().unwrap().to_lowercase();
        let want: Vec<u32> = ids.as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect();
        let got = sp.encode(&s);
        if got != want {
            bad += 1;
            if bad <= 10 {
                eprintln!("ABWEICHUNG {s:?}\n  rust {got:?}\n  ref  {want:?}");
            }
        }
    }
    eprintln!("tokenizer: {} sentences, {bad} deviations", sents.len());
    assert_eq!(bad, 0);
}

#[test]
fn text_tower_matches_reference() {
    let (Some(r), Some(m)) = (env("SIGLIP_REF"), env("SIGLIP_MODEL")) else { return };
    let t = std::time::Instant::now();
    let tower = library::siglip::TextTower::load(&m).unwrap();
    eprintln!("Text-Turm geladen in {:.2} s", t.elapsed().as_secs_f32());
    // padded ids exactly as the reference processor
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.join("tokens.json")).unwrap()).unwrap();
    for (s, ids) in j["sents"].as_array().unwrap().iter().zip(j["ids"].as_array().unwrap()).take(400) {
        let want: Vec<u32> = ids.as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect();
        assert_eq!(tower.ids(s.as_str().unwrap()), want, "{s}");
    }
    let sents: Vec<String> = serde_json::from_str(&std::fs::read_to_string(r.join("text_sents.json")).unwrap()).unwrap();
    let (shape, want) = npy(&r.join("text_emb.npy"));
    let refs: Vec<&str> = sents.iter().map(|s| s.as_str()).collect();
    let t = std::time::Instant::now();
    let got = tower.embed(&refs).unwrap();
    eprintln!("{} sentences in {:.0} ms", refs.len(), t.elapsed().as_secs_f64() * 1000.0);
    let t = std::time::Instant::now();
    let _ = tower.embed_one("ein Hund am Strand").unwrap();
    eprintln!("one query: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    let mut min = 1.0f32;
    for (i, g) in got.iter().enumerate() {
        min = min.min(cosine(g, &want[i * shape[1]..(i + 1) * shape[1]]));
    }
    eprintln!("Text: kleinster Kosinus {min:.6}");
    assert!(min > 0.999, "{min}");
}

#[test]
fn image_tower_matches_reference() {
    let (Some(r), Some(m)) = (env("SIGLIP_REF"), env("SIGLIP_MODEL")) else { return };
    let t = std::time::Instant::now();
    let tower = library::siglip::ImageTower::load(&m).unwrap();
    eprintln!("image tower loaded in {:.2} s", t.elapsed().as_secs_f32());
    let (ps, px) = npy(&r.join("pixels.npy"));
    let (es, want) = npy(&r.join("image_emb.npy"));
    let per = ps[1] * ps[2] * ps[3];
    let prepared: Vec<Vec<f32>> = px.chunks(per).map(|c| c.to_vec()).collect();
    let t = std::time::Instant::now();
    let got = tower.embed_prepared(&prepared);
    eprintln!("{} images in {:.0} ms", prepared.len(), t.elapsed().as_secs_f64() * 1000.0);
    let mut min = 1.0f32;
    for (i, g) in got.iter().enumerate() {
        min = min.min(cosine(g, &want[i * es[1]..(i + 1) * es[1]]));
    }
    eprintln!("Image (same pixels): smallest cosine {min:.6}");
    assert!(min > 0.999, "{min}");
    // end to end with own preprocessing
    if let Some(dir) = env("SIGLIP_IMAGES") {
        let names: Vec<String> = serde_json::from_str(&std::fs::read_to_string(r.join("image_names.json")).unwrap()).unwrap();
        let imgs: Vec<image::RgbImage> = names.iter().map(|n| image::open(dir.join(n)).unwrap().to_rgb8()).collect();
        let mut pmin = 1.0f32;
        for (i, im) in imgs.iter().enumerate() {
            pmin = pmin.min(cosine(&tower.prepare(im), &px[i * per..(i + 1) * per]));
        }
        let refs: Vec<&image::RgbImage> = imgs.iter().collect();
        let got = tower.embed(&refs);
        let mut min = 1.0f32;
        for (i, g) in got.iter().enumerate() {
            min = min.min(cosine(g, &want[i * es[1]..(i + 1) * es[1]]));
        }
        eprintln!("Preprocessing: smallest cosine of the pixels {pmin:.6}; image end to end: {min:.6}");
        assert!(min > 0.995, "{min}");
    }
}
