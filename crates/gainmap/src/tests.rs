use super::*;

fn sdr_jpeg(w: usize, h: usize) -> Vec<u8> {
    // gradient from black to white with a bright block (clipped highlight)
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let v = if x > w * 3 / 4 && y < h / 2 { 255 } else { (x * 255 / w) as u8 };
            px.extend_from_slice(&[v, v, v]);
        }
    }
    let mut o = Vec::new();
    jpeg_encoder::Encoder::new(&mut o, 90).encode(&px, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).unwrap();
    o
}

#[test]
fn iso_round_trip() {
    let mut m = Metadata::luminance(0.0, 2.25);
    m.offset_hdr = [0.02; 3];
    let back = iso_decode(&iso_encode(&m)).unwrap();
    for c in 0..3 {
        assert!((back.max[c] - 2.25).abs() < 1e-6);
        assert!((back.offset_hdr[c] - 0.02).abs() < 1e-6);
    }
    assert_eq!(back.channels, 1);
    assert!((back.alternate_headroom - 2.25).abs() < 1e-6);
    // primary image carries only the version: no metadata
    assert!(iso_decode(&[0, 0, 0, 0]).is_none());
    // three channels, negative minimum
    let mut m3 = Metadata::luminance(-0.5, 1.0);
    m3.channels = 3;
    m3.max = [1.0, 1.5, 2.0];
    let b3 = iso_decode(&iso_encode(&m3)).unwrap();
    assert_eq!(b3.channels, 3);
    assert!((b3.min[1] + 0.5).abs() < 1e-6 && (b3.max[2] - 2.0).abs() < 1e-6);
}

#[test]
fn xmp_round_trip() {
    let m = Metadata::luminance(0.0, 1.75);
    let back = xmp_decode(&xmp_gain_map(&m)).unwrap();
    assert!((back.max[0] - 1.75).abs() < 1e-6);
    assert!((back.offset_sdr[0] - OFFSET).abs() < 1e-6);
    assert!((back.alternate_headroom - 1.75).abs() < 1e-6);
    assert!(!back.base_is_hdr);
}

#[test]
fn container_round_trip_keeps_primary() {
    let primary = sdr_jpeg(64, 48);
    let g: Vec<f32> = (0..32 * 24).map(|i| (i % 32) as f32 / 31.0 * 2.0).collect();
    let meta = Metadata::luminance(0.0, 2.0);
    let map = encode_map(&g, 32, 24, &meta, 90).unwrap();
    let file = assemble(&primary, &map).unwrap();
    // the primary image decodes to the same pixels (entropy-coded data untouched)
    let a = jpeg_decoder::Decoder::new(&primary[..]).decode().unwrap();
    let b = jpeg_decoder::Decoder::new(&file[..]).decode().unwrap();
    assert_eq!(a, b);
    let p = parse(&file).unwrap();
    assert!(p.iso);
    assert_eq!(&file[p.map.clone()], &map[..]);
    let m = read(&file).unwrap();
    assert_eq!((m.w, m.h, m.channels), (32, 24, 1));
    let err = m.log2.iter().zip(&g).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 0.05, "map error {err}");
    // rewriting replaces the old map instead of stacking a second one
    let again = assemble(&file[..p.map.start], &map).unwrap();
    assert_eq!(again.len(), file.len());
}

#[test]
fn xmp_of_export_is_kept() {
    // primary with an existing XMP packet (title of the export)
    let primary = sdr_jpeg(16, 16);
    let old = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>Test</dc:title></rdf:Description></rdf:RDF></x:xmpmeta>";
    let mut with = primary[..2].to_vec();
    with.extend(segment(0xE1, XMP_NS, old.as_bytes()).unwrap());
    with.extend_from_slice(&primary[2..]);
    let map = encode_map(&[1.0; 64], 8, 8, &Metadata::luminance(0.0, 1.0), 90).unwrap();
    let file = assemble(&with, &map).unwrap();
    let (segs, _) = header(&file).unwrap();
    let xmps: Vec<&str> = segs.iter().filter(|s| s.marker == 0xE1 && s.data.starts_with(XMP_NS)).map(|s| std::str::from_utf8(&s.data[XMP_NS.len()..]).unwrap()).collect();
    assert_eq!(xmps.len(), 1);
    assert!(xmps[0].contains("<dc:title>Test</dc:title>") && xmps[0].contains("hdrgm:Version=\"1.0\"") && xmps[0].contains(&format!("Item:Length=\"{}\"", map.len())));
    assert!(read(&file).is_some());
}

#[test]
fn gain_only_in_highlights_and_capped() {
    // midtones: no gain even if the scene is brighter
    assert_eq!(target_gain(0.18, 0.5, 2.0), 0.0);
    // SDR white with scene 3 stops above: close to, never above the headroom
    let g = target_gain(1.0, 8.0, 2.0);
    assert!(g > 1.5 && g < 2.0, "{g}");
    // scene not above SDR: nothing
    assert_eq!(target_gain(0.9, 0.8, 2.0), 0.0);
    assert_eq!(target_gain(1.0, 8.0, 0.0), 0.0);
}

#[test]
fn add_to_jpeg_writes_map_for_bright_scene() {
    let primary = sdr_jpeg(64, 48);
    // scene: the white block is 3 stops over white, the rest equals the SDR
    let scene = |w: usize, h: usize| {
        let (sdr, _, _) = {
            let px = jpeg_decoder::Decoder::new(&primary[..]).decode().unwrap();
            sdr_luminance_half(&px, 64, 48)
        };
        Some(sdr.iter().enumerate().map(|(i, &y)| if i % w > w * 3 / 4 && i / w < h / 2 { 8.0 } else { y }).collect())
    };
    let (file, info) = add_to_jpeg(&primary, &scene, 2.0, 90).unwrap().unwrap();
    assert_eq!((info.w, info.h), (32, 24));
    assert!(info.max > 1.4 && info.max <= 2.0, "{}", info.max);
    let m = read(&file).unwrap();
    // bright block gets the gain, dark corner none
    assert!(m.log2[2 * 32 + 30] > 1.2);
    assert!(m.log2[20 * 32 + 1] < 0.05);
    // nothing bright: no map
    let flat = |w: usize, h: usize| Some(vec![0.1; w * h]);
    assert!(add_to_jpeg(&primary, &flat, 2.0, 90).unwrap().is_none());
}
