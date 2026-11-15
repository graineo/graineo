//! Creates a DNG file with an X-Trans color filter (6×6) from a real, demosaiced image – so that the
//! X-Trans path (Markesteijn, GPU) can run all the way through the app even though no X-Trans RAW is available.
//! The file is NOT a real photo from a Fuji camera: color matrix and name are borrowed, the mosaic is created
//! from RGB (so without the sensor noise and aliasing properties of a real X-Trans sensor).
//!
//! `GRAINEO_BENCH_RAW=<source> GRAINEO_XT_OFF=<target.DNG> cargo test -p rawdev --release --test xtrans_dng -- --ignored --nocapture`

const XT: [[u8; 6]; 6] = [[1, 2, 1, 1, 0, 1], [0, 1, 0, 2, 1, 2], [1, 2, 1, 1, 0, 1], [1, 0, 1, 1, 2, 1], [2, 1, 2, 0, 1, 0], [1, 0, 1, 1, 2, 1]];

fn tag(buf: &mut Vec<u8>, extra: &mut Vec<u8>, extra_start: u32, id: u16, type_name: u16, count: u32, data: &[u8]) {
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&type_name.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    if data.len() <= 4 {
        let mut v = [0u8; 4];
        v[..data.len()].copy_from_slice(data);
        buf.extend_from_slice(&v);
    } else {
        let off = extra_start + extra.len() as u32;
        buf.extend_from_slice(&off.to_le_bytes());
        extra.extend_from_slice(data);
        if extra.len() % 2 == 1 {
            extra.push(0);
        }
    }
}

#[test]
#[ignore]
fn make_xtrans_dng() {
    let (Ok(source), Ok(dest)) = (std::env::var("GRAINEO_BENCH_RAW"), std::env::var("GRAINEO_XT_OFF")) else { return };
    let doc = rawdev::RawDoc::open(std::path::Path::new(&source), &|| true).unwrap().unwrap();
    let st = doc.full_base().unwrap();
    let (w, h) = (st.w, st.h);
    // even dimensions (6×6 pattern fits, borders don't matter)
    let wb = doc.info.wb.unwrap_or([1.0; 3]);
    let mut data = Vec::with_capacity(w * h * 2);
    for y in 0..h {
        for x in 0..w {
            let c = XT[y % 6][x % 6] as usize;
            // the base image is white balanced: back to sensor values, the pipeline multiplies again
            let v = (st.px[y * w + x][c] / wb[c]).clamp(0.0, 1.0);
            data.extend_from_slice(&((v * 65000.0) as u16).to_le_bytes());
        }
    }
    let mut ifd = Vec::new();
    let mut extra = Vec::new();
    let n_tags: u16 = 20;
    let extra_start = 8 + 2 + n_tags as u32 * 12 + 4;
    let image_start = extra_start + 2048; // room for extra data
    let rat = |v: f64| -> [u8; 8] {
        let n = (v * 10000.0).round() as i32;
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&n.to_le_bytes());
        b[4..].copy_from_slice(&10000i32.to_le_bytes());
        b
    };
    let ur = |v: f64| -> [u8; 8] {
        let n = (v * 100000.0).round() as u32;
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&n.to_le_bytes());
        b[4..].copy_from_slice(&100000u32.to_le_bytes());
        b
    };
    let long = |v: u32| v.to_le_bytes().to_vec();
    let mut t = |id: u16, type_name: u16, count: u32, d: &[u8]| tag(&mut ifd, &mut extra, extra_start, id, type_name, count, d);
    t(254, 4, 1, &long(0));
    t(256, 4, 1, &long(w as u32));
    t(257, 4, 1, &long(h as u32));
    t(258, 3, 1, &16u16.to_le_bytes());
    t(259, 3, 1, &1u16.to_le_bytes());
    t(262, 3, 1, &32803u16.to_le_bytes());
    t(271, 2, 9, b"FUJIFILM\0");
    t(272, 2, 12, b"X-Pro2 synt\0");
    t(273, 4, 1, &long(image_start));
    t(274, 3, 1, &1u16.to_le_bytes());
    t(277, 3, 1, &1u16.to_le_bytes());
    t(278, 4, 1, &long(h as u32));
    t(279, 4, 1, &long(data.len() as u32));
    t(284, 3, 1, &1u16.to_le_bytes());
    t(33421, 3, 2, &[6, 0, 6, 0]);
    t(33422, 1, 36, &XT.iter().flatten().copied().collect::<Vec<u8>>());
    t(50706, 1, 4, &[1, 4, 0, 0]);
    // ColorMatrix1 (XYZ → camera), D65: values of the X100 (borrowed)
    let m = [1.2161, -0.4457, -0.1069, -0.5034, 1.2874, 0.2400, -0.0795, 0.1724, 0.6904];
    let mb: Vec<u8> = m.iter().flat_map(|v| rat(*v)).collect();
    t(50721, 10, 9, &mb);
    let nb: Vec<u8> = [1.0 / wb[0] as f64 * wb[1] as f64, 1.0, 1.0 / wb[2] as f64 * wb[1] as f64].iter().flat_map(|v| ur(*v)).collect();
    t(50728, 5, 3, &nb);
    t(50778, 3, 1, &21u16.to_le_bytes());
    assert!(extra.len() < 2048);
    let mut f = Vec::new();
    f.extend_from_slice(b"II*\0");
    f.extend_from_slice(&8u32.to_le_bytes());
    f.extend_from_slice(&n_tags.to_le_bytes());
    // entries must be sorted by tag number: already built in ascending order here
    f.extend_from_slice(&ifd);
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(&extra);
    f.resize(image_start as usize, 0);
    f.extend_from_slice(&data);
    std::fs::write(&dest, f).unwrap();
    println!("written: {dest} ({w}×{h})");
}
