//! Text stored in the files (IPTC, EXIF comment, XMP: title, description, keywords, location,
//! creator …) – for search only. Read once per file in the background (`file.text_stand = 1`);
//! nothing in the files is changed.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::params;

use crate::db::Db;

/// All text fields of a file, deduplicated and separated by spaces.
pub fn read(path: &Path, format: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    #[cfg(target_os = "macos")]
    if format != "X3F" && format != "X3I" && format != "VIDEO" {
        parts.extend(crate::imageio::text_fields(path));
    }
    // embedded XMP (JPEG) and sidecar (also next to RAWs)
    if format == "JPG" {
        if let Some(x) = crate::xmp::jpeg_xmp(path) {
            parts.extend(crate::xmp::text_fields(&x));
        }
    }
    if let Some(x) = crate::xmp::text(&crate::xmp::path_to(path)) {
        parts.extend(crate::xmp::text_fields(&x));
    }
    let mut seen = HashSet::new();
    parts.retain(|t| !filler_text(t) && seen.insert(crate::search::fold_text(t)));
    parts.join(" ")
}

/// Texts that cameras and programs insert by themselves ("OLYMPUS DIGITAL CAMERA") and that describe nothing.
fn filler_text(t: &str) -> bool {
    let k = t.trim().to_lowercase();
    k.is_empty()
        || k.len() < 2
        || k.contains("digital camera")
        || k.contains("exif_jpeg_picture")
        || k.contains("(c) by")
        || k.starts_with("created with")
        || ["sony dsc", "dsc", "canon", "nikon", "fujifilm", "pentax", "samsung", "apple", "ricoh", "panasonic", "leica", "casio", "olympus", "kodak"].contains(&k.as_str())
}

/// Fill in text fields of files that lack them (at most `max`). Returns: processed.
pub fn backfill(db: &Db, max: usize, cancel: &AtomicBool) -> Result<usize, String> {
    let f = |e: rusqlite::Error| e.to_string();
    // files not readable at an earlier run (network drive asleep): tried again once per program start
    static RETRY: std::sync::Once = std::sync::Once::new();
    RETRY.call_once(|| {
        let _ = db.conn.execute("UPDATE file SET text_read = 0 WHERE text_read = -1", []);
    });
    let open_set: Vec<(i64, String, String)> = {
        let mut st = db
            .conn
            .prepare(
                "SELECT d.id, s.path || o.path || d.name, d.format FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root
                 WHERE d.text_read = 0 AND d.status = 0 AND d.format <> 'VIDEO' LIMIT ?1",
            )
            .map_err(f)?;
        let r = st.query_map([max as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        r
    };
    let mut n = 0;
    for chunk in open_set.chunks(64) {
        let read_result: Vec<(i64, String, bool)> = chunk.iter().map(|(id, p, fm)| (*id, read(Path::new(p), fm), std::fs::File::open(p).is_ok())).collect();
        let tx = db.conn.unchecked_transaction().map_err(f)?;
        for (id, t, readable) in read_result {
            // not readable right now: -1 (again at the next start) instead of "read, no text"
            let state = if t.is_empty() && !readable { -1 } else { 1 };
            tx.execute("UPDATE file SET text_read=?3, textmeta=?2 WHERE id=?1", params![id, Some(t).filter(|t| !t.is_empty()), state]).map_err(f)?;
            n += 1;
        }
        tx.commit().map_err(f)?;
        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_fields_and_jpeg_header() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description photoshop:City="Oia" photoshop:Country="Griechenland">
            <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Sonnenuntergang</rdf:li></rdf:Alt></dc:title>
            <dc:subject><rdf:Bag><rdf:li>Santorini</rdf:li><rdf:li>Meer</rdf:li></rdf:Bag></dc:subject>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let t = crate::xmp::text_fields(xmp);
        for w in ["Sonnenuntergang", "Santorini", "Meer", "Oia", "Griechenland"] {
            assert!(t.iter().any(|x| x == w), "{w} missing in {t:?}");
        }
        // JPEG with XMP in the APP1 segment
        let mut j = vec![0xff, 0xd8];
        let payload_len: Vec<u8> = [&b"http://ns.adobe.com/xap/1.0/\0"[..], xmp.as_bytes()].concat();
        j.extend([0xff, 0xe1]);
        j.extend(((payload_len.len() + 2) as u16).to_be_bytes());
        j.extend(payload_len);
        j.extend([0xff, 0xda, 0, 2]);
        let p = std::env::temp_dir().join(format!("graineo-textmeta-{}.jpg", std::process::id()));
        std::fs::write(&p, &j).unwrap();
        let x = crate::xmp::jpeg_xmp(&p).expect("XMP found");
        assert!(crate::xmp::text_fields(&x).iter().any(|w| w == "Santorini"));
        let all = read(&p, "JPG");
        assert!(all.contains("Sonnenuntergang") && all.contains("Oia"));
        std::fs::remove_file(p).unwrap();
    }
}
