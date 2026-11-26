//! Hostile input: values and sizes from the file are checked, mutated files end without panic or hang.

use super::*;

fn sdr_jpeg(w: usize, h: usize) -> Vec<u8> {
    let px: Vec<u8> = (0..w * h).flat_map(|i| {
        let v = (i % w * 255 / w) as u8;
        [v, v, v]
    }).collect();
    let mut o = Vec::new();
    jpeg_encoder::Encoder::new(&mut o, 90).encode(&px, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).unwrap();
    o
}

/// Values read from a file are checked: XMP accepts "NaN"/"inf" as numbers, the ISO block absurd fractions (1.5.6).
#[test]
fn rejects_non_finite_and_absurd_metadata() {
    let m = Metadata::luminance(0.0, 2.0);
    let x = xmp_gain_map(&m);
    assert!(xmp_decode(&x).is_some());
    for (from, to) in [("hdrgm:GainMapMax=\"2\"", "hdrgm:GainMapMax=\"NaN\""), ("hdrgm:HDRCapacityMax=\"2\"", "hdrgm:HDRCapacityMax=\"inf\""), ("hdrgm:GainMapMin=\"0\"", "hdrgm:GainMapMin=\"-1e30\"")] {
        assert!(x.contains(from), "{from}");
        assert!(xmp_decode(&x.replace(from, to)).is_none(), "{to}");
    }
    // ISO: max = (2^31 − 1) / 1 stops
    let mut b = iso_encode(&m);
    let max_at = 5 + 16 + 8; // version and flags, two headrooms, min
    b[max_at..max_at + 4].copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
    b[max_at + 4..max_at + 8].copy_from_slice(&1u32.to_be_bytes());
    assert!(iso_decode(&b).is_none());
}

/// A gain map image whose header claims 65535×65535 pixels is refused before decoding (no 4 GB allocation).
#[test]
fn refuses_huge_map() {
    let primary = sdr_jpeg(64, 48);
    let map = encode_map(&vec![1.0f32; 32 * 24], 32, 24, &Metadata::luminance(0.0, 1.0), 90).unwrap();
    let mut file = assemble(&primary, &map).unwrap();
    assert!(read(&file).is_some());
    let start = parse(&file).unwrap().map.start;
    // SOF0 of the map: FF C0, length (2), precision (1), height (2), width (2)
    let sof = start + file[start..].windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    file[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let t = std::time::Instant::now();
    assert!(read(&file).is_none());
    assert!(t.elapsed().as_secs() < 2);
}

/// Mutation fuzzing of the reader and of `assemble` with files written by this crate (5000 variants by default,
/// `GAINMAP_FUZZ_CASES` for more). Every case must end without panic within 10 s.
#[test]
fn fuzz_reader_and_assemble() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let cases: usize = std::env::var("GAINMAP_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(5000);
    let primary = sdr_jpeg(48, 32);
    let mut m3 = Metadata::luminance(-0.5, 2.0);
    m3.channels = 3;
    m3.max = [1.0, 1.5, 2.0];
    let seeds: Vec<Vec<u8>> = [Metadata::luminance(0.0, 2.0), m3]
        .iter()
        .map(|m| assemble(&primary, &encode_map(&vec![0.7f32; 24 * 16], 24, 16, m, 90).unwrap()).unwrap())
        .collect();
    let done = Arc::new(AtomicUsize::new(0));
    let d = done.clone();
    let worker = std::thread::spawn(move || {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for i in 0..cases {
            let mut f = seeds[i % seeds.len()].clone();
            for _ in 0..1 + rnd() % 8 {
                let at = rnd() as usize % f.len();
                match rnd() % 7 {
                    0 => f[at] ^= 1 << (rnd() % 8),
                    1 => f[at] = 0xFF,
                    2 => f[at] = rnd() as u8,
                    3 if at + 4 <= f.len() => f[at..at + 4].copy_from_slice(&[[0xFF; 4], [0; 4], [0x7F, 0xFF, 0xFF, 0xFF], [0, 0, 0, 1]][rnd() as usize % 4]),
                    4 => f.truncate(at.max(2)),
                    5 => {
                        let n = (rnd() % 64) as usize;
                        let src = rnd() as usize % f.len();
                        let chunk: Vec<u8> = f[src..(src + n).min(f.len())].to_vec();
                        f.splice(at..at, chunk);
                    }
                    _ => {
                        let n = (rnd() % 32) as usize;
                        f.drain(at..(at + n).min(f.len()));
                    }
                }
                if f.len() < 4 {
                    break;
                }
            }
            let _ = parse(&f);
            let _ = read(&f);
            let _ = image_end(&f, 0);
            // the export path with a mutated primary
            let _ = assemble(&f, &seeds[0][..64]);
            d.store(i + 1, Ordering::SeqCst);
        }
    });
    let mut last = (0, std::time::Instant::now());
    while !worker.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let n = done.load(Ordering::SeqCst);
        if n != last.0 {
            last = (n, std::time::Instant::now());
        }
        assert!(last.1.elapsed().as_secs() < 10, "case {n} runs longer than 10 s");
    }
    worker.join().expect("no panic");
    assert_eq!(done.load(Ordering::SeqCst), cases);
}
