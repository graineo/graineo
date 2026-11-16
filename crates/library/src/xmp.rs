//! XMP sidecars (`NAME.xmp` next to the image). Since 08.11.2026 the library is the main store ([`crate::store`]);
//! the sidecar is an optional copy for other programs (setting `writeSidecars`) and is read into the library once.
//!
//! Only our fields are written; everything else is preserved character for character
//! (e.g. `crs:` develop settings from Lightroom/Camera Raw):
//! - `xmp:Rating` (0–5), `xmp:Label` (label name as in German-language Lightroom: Rot, Gelb, Grün, Blau, Lila),
//! - `dc:subject` (keywords, leaves) and `lr:hierarchicalSubject` ("Orte|Italien|Rom"),
//! - own namespace `graineo:` (English names): `Flag` (pick flag, which Lightroom does not write to XMP), `RatingTime`/`KeywordsTime`
//!   (timestamp per field group), `VirtualCopies` (JSON) and `DevelopSettings` (JSON).
//!
//! File name: RAW → `NAME.xmp`. Other formats likewise, unless there is a
//! same-named RAW in the folder (RAW+JPEG as two images) – then `NAME.JPG.xmp`, so that the
//! two do not share the same file. Originals are never modified.

use std::path::{Path, PathBuf};

/// Own XMP namespace (English field names, prefix `graineo`).
pub const NS: &str = crate::XMP_NS;
const LABEL_NAMES: [&str; 6] = ["", "Rot", "Gelb", "Grün", "Blau", "Lila"];

/// A named face (MWG region): box normalized 0…1 to the upright image, origin top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub b: f32,
    pub h: f32,
}

/// Content of our fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    /// named faces for `mwg-rs:Regions` (empty: existing regions of the file stay untouched)
    pub regions: Vec<Region>,
    /// dimensions of the upright image in pixels (for `AppliedToDimensions`)
    pub region_dims: Option<(u32, u32)>,
    /// the image's faces have been evaluated: without named faces our previously written regions are dropped too
    pub clear_regions: bool,
    pub stars: u8,
    pub flag: i8,
    pub label: u8,
    /// keywords as path ("A|B|C"); leaves for `dc:subject` are derived from them
    pub keywords: Vec<String>,
    /// Unix seconds of the last change per group (`None` = unknown)
    pub rating_time: Option<i64>,
    pub keyword_time: Option<i64>,
    /// virtual copies (JSON list, see [`crate::sidecar::VirtualCopy`])
    pub copies_json: Option<String>,
    /// when reading: the file contains rating fields or keywords (foreign XMP without these fields
    /// does not overwrite the library with "empty")
    pub has_rating: bool,
    pub has_keywords: bool,
    /// corrected capture time (new, original) as 'YYYY-MM-DDThh:mm:ss': `exif:DateTimeOriginal`
    /// (which Lightroom reads too) and `graineo:OriginalDateTime` (for restoring). `None`: no correction.
    pub time_correction: Option<(String, String)>,
    /// copyright given in graineo (`dc:rights`); `None`: a `dc:rights` already in the file stays untouched
    pub copyright: Option<String>,
}

/// May graineo write a sidecar (XMP) at `p`? Test and development runs must never write next to the person's photos
/// (a test run once overwrote `DP2M5418.xmp` in the camera folder it had opened):
/// - `GRAINEO_NO_XMP=1`: never; `GRAINEO_NO_XMP=0`: everywhere (a test that wants to see the files).
/// - otherwise, with an own data folder (`GRAINEO_DATA_DIR`, required for every test run): only inside that folder or
///   in the system's temporary folder (where tests put their copies).
/// - otherwise (the installed app): always.
///
/// A sidecar not written is skipped quietly; the library keeps everything anyway.
pub fn writes_allowed(p: &Path) -> bool {
    allowed_by(std::env::var("GRAINEO_NO_XMP").ok().as_deref(), std::env::var_os("GRAINEO_DATA_DIR").as_deref().map(Path::new), p)
}

fn allowed_by(no_xmp: Option<&str>, data: Option<&Path>, p: &Path) -> bool {
    match no_xmp.map(str::trim) {
        Some("1") => return false,
        Some("0") => return true,
        _ => {}
    }
    let Some(data) = data.filter(|d| !d.as_os_str().is_empty()) else { return true };
    // the file may not exist yet: its folder decides (symlinks resolved, e.g. /var → /private/var on macOS)
    let canon = |q: &Path| -> PathBuf {
        // the nearest existing folder resolved, the rest appended
        let mut rest = Vec::new();
        let mut at = q;
        loop {
            if let Ok(c) = std::fs::canonicalize(at) {
                return rest.iter().rev().fold(c, |p: PathBuf, n| p.join(n));
            }
            match (at.parent(), at.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name.to_os_string());
                    at = parent;
                }
                _ => return q.to_path_buf(),
            }
        }
    };
    // comparison only: without the Windows `\\?\` prefix on both sides (a folder that does not exist yet has none)
    let folder = sysdirs::simplify(&canon(p.parent().unwrap_or(p)));
    [data.to_path_buf(), std::env::temp_dir()].iter().any(|allowed| folder.starts_with(sysdirs::simplify(&canon(allowed))))
}

/// XMP path of an image. `raw_beside`: there is a same-named RAW in the folder (only relevant for non-RAW).
pub fn path(image: &Path, format: &str, raw_beside: bool) -> PathBuf {
    // photos from Apple Photos: sidecar in the own data folder, never in the library
    if let Some(uuid) = crate::apple_photos::uuid_from_path(image) {
        return crate::apple_photos::sidecar_folder().join(format!("{uuid}.xmp"));
    }
    if !crate::is_raw(format) && raw_beside {
        let mut n = image.file_name().unwrap_or_default().to_os_string();
        n.push(".xmp");
        image.with_file_name(n)
    } else {
        image.with_extension("xmp")
    }
}

/// XMP path of a file as the library uses it: the format comes from the extension, and
/// a same-named RAW in the folder is detected by looking into the folder (only needed for non-RAW).
pub fn path_to(p: &Path) -> PathBuf {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let format = crate::format_from_extension(name).unwrap_or("DNG");
    let raw_beside = !crate::is_raw(format)
        && p.parent().and_then(|d| std::fs::read_dir(d).ok()).is_some_and(|rd| {
            let root = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            rd.filter_map(|e| e.ok()).any(|e| {
                let q = e.path();
                q.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase() == root)
                    && q.file_name().and_then(|n| n.to_str()).and_then(crate::format_from_extension).is_some_and(crate::is_raw)
            })
        });
    path(p, format, raw_beside)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Result of reading.
#[derive(Debug, PartialEq)]
pub enum ReadResult {
    Missing,
    /// present, but not readable XMP
    Broken,
    Ok(Values),
}

/// Attribute value `name="…"` in the text (first occurrence).
fn attr(text: &str, name: &str) -> Option<String> {
    for q in ['"', '\''] {
        let pattern = format!("{name}={q}");
        let mut from = 0;
        while let Some(i) = text[from..].find(&pattern) {
            let i = from + i;
            // the name must stand on its own (e.g. not "crs:xmp:Rating")
            let prev_char = text[..i].chars().last();
            if prev_char.is_some_and(|c| !c.is_whitespace()) {
                from = i + pattern.len();
                continue;
            }
            let rest = &text[i + pattern.len()..];
            return rest.find(q).map(|e| unescape(&rest[..e]));
        }
    }
    None
}

/// Content of an element `<name>…</name>`.
fn element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let a = text.find(&format!("<{name}>")).or_else(|| text.find(&format!("<{name} ")))?;
    let start = a + text[a..].find('>')? + 1;
    let e = text[start..].find(&format!("</{name}>"))? + start;
    Some(&text[start..e])
}

fn li_texts(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(i) = rest.find("<rdf:li") {
        let Some(s) = rest[i..].find('>') else { break };
        let start = i + s + 1;
        let Some(e) = rest[start..].find("</rdf:li>") else { break };
        out.push(unescape(rest[start..start + e].trim()));
        rest = &rest[start + e..];
    }
    out
}

/// Read a value as attribute or element.
fn field(text: &str, name: &str) -> Option<String> {
    attr(text, name).or_else(|| element(text, name).map(|s| unescape(s.trim())))
}

/// Read an own field (`Flag`, `RatingTime` …).
fn own_field(text: &str, name: &str) -> Option<String> {
    field(text, &format!("{}:{name}", crate::XMP_PREFIX))
}

/// Own element.
fn own_element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    element(text, &format!("{}:{name}", crate::XMP_PREFIX))
}

pub fn read(p: &Path) -> ReadResult {
    let Ok(text) = std::fs::read_to_string(p) else {
        return if p.exists() { ReadResult::Broken } else { ReadResult::Missing };
    };
    match read_text(&text) {
        Some(w) => ReadResult::Ok(w),
        None => ReadResult::Broken,
    }
}

/// Our fields from the text of an XMP file; `None` = not readable XMP.
pub fn read_text(text: &str) -> Option<Values> {
    if !text.contains("rdf:Description") || !text.contains("</x:xmpmeta>") && !text.contains("</rdf:RDF>") {
        return None;
    }
    let mut w = Values::default();
    w.has_rating = ["xmp:Rating", "xmp:Label"].iter().any(|f| field(&text, f).is_some()) || own_field(&text, "Flag").is_some();
    w.has_keywords = element(&text, "dc:subject").is_some() || element(&text, "lr:hierarchicalSubject").is_some() || own_field(&text, "KeywordsTime").is_some();
    let rating = field(&text, "xmp:Rating").and_then(|r| r.trim().parse::<f64>().ok()).unwrap_or(0.0);
    w.stars = rating.clamp(0.0, 5.0) as u8;
    if rating < 0.0 {
        w.flag = -1; // XMP convention: −1 = rejected
    }
    if let Some(l) = field(&text, "xmp:Label") {
        w.label = crate::label::from_lightroom(&l).unwrap_or(0);
    }
    if let Some(m) = own_field(&text, "Flag").and_then(|m| m.trim().parse::<i8>().ok()) {
        w.flag = m.clamp(-1, 1);
    }
    w.rating_time = own_field(&text, "RatingTime").and_then(|v| v.trim().parse().ok());
    w.keyword_time = own_field(&text, "KeywordsTime").and_then(|v| v.trim().parse().ok());
    w.copies_json = own_element(&text, "VirtualCopies").map(|s| unescape(s.trim())).filter(|s| !s.is_empty());
    // only our correction counts (recognizable by the remembered original time)
    if let (Some(orig), Some(new)) = (own_field(&text, "OriginalDateTime"), field(&text, "exif:DateTimeOriginal")) {
        let short = |s: &str| s.trim().chars().take(19).collect::<String>();
        if short(&new).len() == 19 && short(&orig).len() == 19 {
            w.time_correction = Some((short(&new), short(&orig)));
        }
    }
    let here = element(&text, "lr:hierarchicalSubject").map(li_texts).unwrap_or_default();
    let flat = element(&text, "dc:subject").map(li_texts).unwrap_or_default();
    // hierarchical takes precedence; flat keywords without hierarchy in addition. Lightroom (and graineo) repeat
    // every parent term in `dc:subject`: a flat entry that is any segment of a path is part of that path, not an
    // extra top-level keyword
    let segments: std::collections::HashSet<String> = here.iter().flat_map(|h| h.split('|')).map(|s| s.trim().to_lowercase()).collect();
    let mut sw: Vec<String> = here.clone();
    for f in flat {
        let key = f.trim().to_lowercase();
        if !key.is_empty() && !segments.contains(&key) && !sw.iter().any(|s| s.to_lowercase() == key) {
            sw.push(f);
        }
    }
    w.keywords = sw;
    Some(w)
}

// when writing, our fields are replaced
const OUR_ATTRS: [&str; 6] = ["graineo:Regions", "xmp:Rating", "xmp:Label", "graineo:Flag", "graineo:RatingTime", "graineo:KeywordsTime"];
const OUR_ELEMS: [&str; 8] = ["dc:subject", "lr:hierarchicalSubject", "xmp:Rating", "xmp:Label", "graineo:Flag", "graineo:VirtualCopies", "graineo:RatingTime", "graineo:KeywordsTime"];

fn without_attr(tag: &str, name: &str) -> String {
    let mut t = tag.to_string();
    for q in ['"', '\''] {
        let pattern = format!("{name}={q}");
        while let Some(i) = t.find(&pattern) {
            let Some(e) = t[i + pattern.len()..].find(q) else { break };
            let end = i + pattern.len() + e + 1;
            // remove including leading whitespace
            let mut start = i;
            while start > 0 && t.as_bytes()[start - 1].is_ascii_whitespace() {
                start -= 1;
            }
            t.replace_range(start..end, "");
        }
    }
    t
}

fn without_element(body: &str, name: &str) -> String {
    let mut b = body.to_string();
    loop {
        let a = b.find(&format!("<{name}>")).or_else(|| b.find(&format!("<{name} ")));
        let Some(a) = a else { break };
        let closing = format!("</{name}>");
        let Some(e) = b[a..].find(&closing) else { break };
        let mut start = a;
        while start > 0 && b.as_bytes()[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        b.replace_range(start..a + e + closing.len(), "");
    }
    b
}

fn new_file() -> String {
    "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"graineo\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\">\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n".to_string()
}

/// Position of the `</rdf:Description>` that closes the outer description (nested ones, e.g. in regions, are skipped).
fn matching_end(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(r) = text[i..].find("<") {
        let a = i + r;
        let rest = &text[a..];
        if rest.starts_with("</rdf:Description>") {
            if depth == 0 {
                return Some(a);
            }
            depth -= 1;
        } else if rest.starts_with("<rdf:Description") {
            let end = a + text[a..].find('>')?;
            if text.as_bytes()[end - 1] != b'/' {
                depth += 1;
            }
        }
        i = a + 1;
    }
    None
}

/// Insert our fields into the XMP text (everything else stays).
pub fn insert_into(old: &str, w: &Values) -> Option<String> {
    let d = old.find("<rdf:Description")?;
    let day_end = d + old[d..].find('>')?;
    let itself = old.as_bytes()[day_end - 1] == b'/';
    let mut tag = old[d..if itself { day_end - 1 } else { day_end }].to_string();
    // the marker shows that the file's regions come from us (foreign ones always stay)
    let own_regions = tag.contains("graineo:Regions=");
    // capture time: only touch it if we correct it or the file already carries our correction
    // (an exif:DateTimeOriginal written by Lightroom without our marker stays)
    let own_time = tag.contains("graineo:OriginalDateTime=") || old.contains("<graineo:OriginalDateTime>");
    if own_time || w.time_correction.is_some() {
        tag = without_attr(&without_attr(&tag, "exif:DateTimeOriginal"), "graineo:OriginalDateTime");
    }
    if let Some((new, orig)) = &w.time_correction {
        if !old.contains("xmlns:exif=") && !tag.contains("xmlns:exif=") {
            tag.push_str("\n    xmlns:exif=\"http://ns.adobe.com/exif/1.0/\"");
        }
        tag.push_str(&format!("\n   exif:DateTimeOriginal=\"{}\"\n   graineo:OriginalDateTime=\"{}\"", esc(new), esc(orig)));
    }
    for a in OUR_ATTRS {
        tag = without_attr(&tag, a);
    }
    for (p, uri) in [("xmp", "http://ns.adobe.com/xap/1.0/"), ("dc", "http://purl.org/dc/elements/1.1/"), ("lr", "http://ns.adobe.com/lightroom/1.0/"), (crate::XMP_PREFIX, NS)] {
        if !old.contains(&format!("xmlns:{p}=")) {
            tag.push_str(&format!("\n    xmlns:{p}=\"{uri}\""));
        }
    }
    tag.push_str(&format!("\n   xmp:Rating=\"{}\"", if w.flag == -1 && w.stars == 0 { 0 } else { w.stars }));
    if w.label > 0 {
        tag.push_str(&format!("\n   xmp:Label=\"{}\"", LABEL_NAMES[w.label as usize % 6]));
    }
    tag.push_str(&format!("\n   graineo:Flag=\"{}\"", w.flag));
    if let Some(t) = w.rating_time {
        tag.push_str(&format!("\n   graineo:RatingTime=\"{t}\""));
    }
    if let Some(t) = w.keyword_time {
        tag.push_str(&format!("\n   graineo:KeywordsTime=\"{t}\""));
    }
    let (body, rest) = if itself {
        (String::new(), &old[day_end + 1..])
    } else {
        let e = day_end + 1 + matching_end(&old[day_end + 1..])?;
        (old[day_end + 1..e].to_string(), &old[e + "</rdf:Description>".len()..])
    };
    let mut body = body;
    for e in OUR_ELEMS {
        body = without_element(&body, e);
    }
    if own_time || w.time_correction.is_some() {
        body = without_element(&without_element(&body, "exif:DateTimeOriginal"), "graineo:OriginalDateTime");
    }
    // faces: only replace if we have some to write (otherwise foreign regions stay untouched)
    let write_regions = !w.regions.is_empty() && w.region_dims.is_some();
    let remove_regions = w.regions.is_empty() && w.clear_regions && own_regions;
    if remove_regions {
        body = without_element(&body, "mwg-rs:Regions");
    }
    if write_regions || (own_regions && !remove_regions) {
        tag.push_str("\n   graineo:Regions=\"1\"");
    }
    if write_regions {
        body = without_element(&body, "mwg-rs:Regions");
        for (p, uri) in [("mwg-rs", "http://www.metadataworkinggroup.com/schemas/regions/"), ("stDim", "http://ns.adobe.com/xap/1.0/sType/Dimensions#"), ("stArea", "http://ns.adobe.com/xmp/sType/Area#")] {
            if !old.contains(&format!("xmlns:{p}=")) && !tag.contains(&format!("xmlns:{p}=")) {
                tag.push_str(&format!("\n    xmlns:{p}=\"{uri}\""));
            }
        }
    }
    let mut new = String::new();
    if !w.keywords.is_empty() {
        // like Lightroom: the leaves first, then every parent term, each once (ignoring case); reading takes the
        // parents back as segments of the paths, not as extra keywords
        let mut leaves: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let segs: Vec<Vec<&str>> = w.keywords.iter().map(|s| s.split('|').collect()).collect();
        let last = segs.iter().filter_map(|p| p.last().copied());
        let parents = segs.iter().flat_map(|p| p[..p.len().saturating_sub(1)].iter().copied());
        for b in last.chain(parents) {
            if !b.trim().is_empty() && seen.insert(b.trim().to_lowercase()) {
                leaves.push(b.to_string());
            }
        }
        new.push_str("\n   <dc:subject>\n    <rdf:Bag>\n");
        for b in &leaves {
            new.push_str(&format!("     <rdf:li>{}</rdf:li>\n", esc(b)));
        }
        new.push_str("    </rdf:Bag>\n   </dc:subject>\n   <lr:hierarchicalSubject>\n    <rdf:Bag>\n");
        for s in &w.keywords {
            new.push_str(&format!("     <rdf:li>{}</rdf:li>\n", esc(s)));
        }
        new.push_str("    </rdf:Bag>\n   </lr:hierarchicalSubject>");
    }
    if let Some(c) = w.copyright.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        body = without_element(&body, "dc:rights");
        new.push_str(&format!("\n   <dc:rights>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </dc:rights>", esc(c)));
    }
    if let Some(k) = &w.copies_json {
        new.push_str(&format!("\n   <graineo:VirtualCopies>{}</graineo:VirtualCopies>", esc(k)));
    }
    if write_regions {
        let (bw, bh) = w.region_dims.unwrap_or((0, 0));
        new.push_str(&format!("\n   <mwg-rs:Regions rdf:parseType=\"Resource\">\n    <mwg-rs:AppliedToDimensions stDim:w=\"{bw}\" stDim:h=\"{bh}\" stDim:unit=\"pixel\"/>\n    <mwg-rs:RegionList>\n     <rdf:Bag>\n"));
        for r in &w.regions {
            // MWG: center and size, normalized
            new.push_str(&format!(
                "      <rdf:li>\n       <rdf:Description mwg-rs:Name=\"{}\" mwg-rs:Type=\"Face\">\n        <mwg-rs:Area stArea:x=\"{:.6}\" stArea:y=\"{:.6}\" stArea:w=\"{:.6}\" stArea:h=\"{:.6}\" stArea:unit=\"normalized\"/>\n       </rdf:Description>\n      </rdf:li>\n",
                esc(&r.name),
                (r.x + r.b / 2.0).clamp(0.0, 1.0),
                (r.y + r.h / 2.0).clamp(0.0, 1.0),
                r.b.clamp(0.0, 1.0),
                r.h.clamp(0.0, 1.0)
            ));
        }
        new.push_str("     </rdf:Bag>\n    </mwg-rs:RegionList>\n   </mwg-rs:Regions>");
    }
    let body = format!("{}{new}\n  ", body.trim_end());
    Some(format!("{}{tag}>{body}</rdf:Description>{rest}", &old[..d]))
}

/// Write (atomically). Foreign file: replace only our fields. Broken file: first back it up as
/// `.NAME.xmp.broken-<time>`, then create it anew. Returns: error text.
pub fn write(p: &Path, w: &Values) -> Result<(), String> {
    if !writes_allowed(p) {
        return Ok(());
    }
    let old = match std::fs::read(p) {
        // not UTF-8 is content (damaged or foreign): set aside below like an unreadable packet
        Ok(t) => String::from_utf8(t).ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // rights, I/O or a network hiccup: the file is fine, only not readable right now – never set it aside and
        // replace it with only our fields (before 0.108 it went to `.defekt-…` here)
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let text = match old.as_deref().and_then(|a| insert_into(a, w)) {
        Some(t) => t,
        None => {
            if p.exists() {
                // not readable: lose nothing
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                let safe = p.with_file_name(format!(".{name}.defekt-{}", crate::now()));
                std::fs::rename(p, &safe).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            insert_into(&new_file(), w).ok_or("Vorlage")?
        }
    };
    crate::files::write_atomic(p, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
}

/// The title (`dc:title`, first non-empty language alternative) of an XMP packet; `None` if missing or empty.
pub fn title(text: &str) -> Option<String> {
    let content = element(text, "dc:title")?;
    let t = li_texts(content).into_iter().find(|t| !t.trim().is_empty()).unwrap_or_else(|| unescape(content.trim()));
    Some(t.trim().to_string()).filter(|t| !t.is_empty() && !t.contains('<'))
}

/// Develop settings (JSON, English field names) from `graineo:DevelopSettings`.
/// `Err` = file present but not readable (then never overwrite).
pub fn read_develop(p: &Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    if !text.contains("rdf:Description") {
        return Err(format!("{} ist kein lesbares XMP", p.display()));
    }
    Ok(own_element(&text, "DevelopSettings").map(|s| unescape(s.trim())).filter(|s| !s.is_empty()))
}

/// All descriptive text fields of an XMP packet for search: title, description, keywords,
/// location (city, state, country, sublocation), headline, instructions, comment. Creator, rights and source
/// stay out (they are in almost every photo and would flood every name search).
pub fn text_fields(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in ["dc:title", "dc:description", "dc:subject", "exif:UserComment"] {
        if let Some(content) = element(text, name) {
            let li = li_texts(content);
            if li.is_empty() {
                out.push(unescape(content.trim()));
            } else {
                out.extend(li);
            }
        }
    }
    for name in ["photoshop:Headline", "photoshop:City", "photoshop:State", "photoshop:Country", "photoshop:Instructions", "Iptc4xmpCore:Location"] {
        if let Some(v) = field(text, name) {
            out.push(v);
        }
    }
    out.retain(|t| !t.trim().is_empty() && !t.contains('<'));
    out
}

/// XMP packet from the head of a JPEG file (APP1). Only the segments before the image are read.
pub fn jpeg_xmp(p: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::io::BufReader::new(std::fs::File::open(p).ok()?);
    let mut header = [0u8; 2];
    f.read_exact(&mut header).ok()?;
    if header != [0xff, 0xd8] {
        return None;
    }
    const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    loop {
        let mut m = [0u8; 2];
        f.read_exact(&mut m).ok()?;
        if m[0] != 0xff {
            return None;
        }
        if m[1] == 0xff {
            f.seek(SeekFrom::Current(-1)).ok()?;
            continue;
        }
        // image data begins / end / markers without length
        if m[1] == 0xda || m[1] == 0xd9 {
            return None;
        }
        if (0xd0..=0xd7).contains(&m[1]) || m[1] == 0x01 {
            continue;
        }
        let mut l = [0u8; 2];
        f.read_exact(&mut l).ok()?;
        let len = u16::from_be_bytes(l) as usize;
        if len < 2 {
            return None;
        }
        let payload_len = len - 2;
        if m[1] == 0xe1 && payload_len > XMP_SIGNATURE.len() {
            let mut buf = vec![0u8; payload_len];
            f.read_exact(&mut buf).ok()?;
            if buf.starts_with(XMP_SIGNATURE) {
                return Some(String::from_utf8_lossy(&buf[XMP_SIGNATURE.len()..]).into_owned());
            }
        } else {
            f.seek(SeekFrom::Current(payload_len as i64)).ok()?;
        }
    }
}

/// Whole text of the XMP file (for the `crs:` fields of other programs), if present.
pub fn text(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().filter(|t| t.contains("rdf:Description"))
}

/// Write develop settings into `graineo:DevelopSettings` (`None` = remove). Everything
/// else (including foreign `crs:` fields) is preserved character for character; atomic. An
/// existing but unreadable file is not touched.
pub fn write_develop(p: &Path, json: Option<&str>) -> Result<(), String> {
    if !writes_allowed(p) {
        return Ok(());
    }
    let old = match std::fs::read_to_string(p) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    if old.is_none() && json.is_none() {
        return Ok(());
    }
    let base = old.clone().unwrap_or_else(new_file);
    let text = develop_into(&base, json).map_err(|e| format!("{}: {e}", p.display()))?;
    crate::files::write_atomic(p, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
}

/// Text of an XMP packet `base` with `graineo:DevelopSettings` set to `json` (`None` = removed), see [`write_develop`].
pub fn develop_into(base: &str, json: Option<&str>) -> Result<String, String> {
    let d = base.find("<rdf:Description").ok_or_else(|| "kein lesbares XMP – nicht überschrieben".to_string())?;
    let day_end = d + base[d..].find('>').ok_or("XMP unvollständig")?;
    let itself = base.as_bytes()[day_end - 1] == b'/';
    let mut tag = base[d..if itself { day_end - 1 } else { day_end }].to_string();
    if !base.contains("xmlns:graineo=") {
        tag.push_str(&format!("\n    xmlns:graineo=\"{NS}\""));
    }
    let (body, rest) = if itself {
        (String::new(), &base[day_end + 1..])
    } else {
        // the matching end, not the first one: named faces are nested descriptions (mwg-rs regions), the settings
        // landed inside the first face before 0.108
        let e = day_end + 1 + matching_end(&base[day_end + 1..]).ok_or("XMP unvollständig")?;
        (base[day_end + 1..e].to_string(), &base[e + "</rdf:Description>".len()..])
    };
    let mut body = without_element(&body, "graineo:DevelopSettings");
    if let Some(j) = json {
        body = format!("{}\n   <graineo:DevelopSettings>{}</graineo:DevelopSettings>", body.trim_end(), esc(j));
    }
    let body = format!("{}\n  ", body.trim_end());
    let text = format!("{}{tag}>{body}</rdf:Description>{rest}", &base[..d]);
    Ok(text)
}

/// Develop settings (`graineo:DevelopSettings`) from the text of an XMP file.
pub fn develop_from_text(text: &str) -> Option<String> {
    own_element(&text, "DevelopSettings").map(|s| unescape(s.trim())).filter(|s| !s.is_empty())
}

/// Is the text a readable XMP packet (as [`read_text`] demands)?
pub fn readable(text: &str) -> bool {
    text.contains("rdf:Description") && (text.contains("</x:xmpmeta>") || text.contains("</rdf:RDF>"))
}

/// Whole sidecar text from the library's state: our fields (`w`), the own develop settings and the Lightroom data
/// put into `old` (everything else of it stays character for character) or into a new packet. `Err`: `old` is
/// not a readable XMP.
pub fn compose(old: Option<&str>, w: &Values, develop: Option<&str>, lr: &Lightroom) -> Result<String, String> {
    let base = match old {
        Some(t) if readable(t) => t.to_string(),
        Some(_) => return Err("kein lesbares XMP".into()),
        None => new_file(),
    };
    let t = insert_into(&base, w).ok_or("kein lesbares XMP")?;
    let t = develop_into(&t, develop)?;
    lightroom_into(&t, lr, None)
}

/// Develop data taken over once from a Lightroom catalog ([`crate::lr_handover`]), kept in the
/// sidecar so that nothing has to read the catalog later:
/// - `graineo:LightroomDevelop`: the catalog's settings text, unchanged (`s = { Exposure2012 = 0.35, … }`,
///   including crop, masks, spot removal); it is interpreted on every opening, so later improvements of
///   the mapping apply to it as well,
/// - `graineo:LightroomOrientation`: the image's orientation in Lightroom (`AB`, `BC` … including a rotation by hand),
/// - `graineo:LightroomEdited="1"`: Lightroom marks the image as edited (finished images: the grid shows the edit),
/// - `graineo:LightroomFile`: name of the photo the data belongs to (a JPEG next to a RAW that is not there may
///   share the RAW's sidecar; it must not take over the RAW's settings).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lightroom {
    pub develop: Option<String>,
    pub orientation: Option<String>,
    pub edited: bool,
    pub file: Option<String>,
}

impl Lightroom {
    pub fn is_empty(&self) -> bool {
        self.develop.is_none() && self.orientation.is_none() && !self.edited
    }

    /// Does the data belong to the photo `image` (or name no photo)?
    pub fn belongs_to(&self, image: &Path) -> bool {
        let norm = |s: &str| crate::nfc(s).to_lowercase();
        let name = image.file_name().map(|n| norm(&n.to_string_lossy()));
        self.file.as_deref().is_none_or(|f| Some(norm(f)) == name)
    }

    /// The data, if it belongs to the photo `image` ([`Self::belongs_to`]); else empty.
    pub fn for_image(self, image: &Path) -> Lightroom {
        if self.belongs_to(image) {
            self
        } else {
            Lightroom::default()
        }
    }
}

/// Lightroom data from the text of an XMP file (see [`Lightroom`]).
pub fn lightroom_from_text(text: &str) -> Lightroom {
    let prefix = crate::XMP_PREFIX;
    Lightroom {
        develop: element(text, &format!("{prefix}:LightroomDevelop")).map(|s| unescape(s.trim())).filter(|s| !s.is_empty()),
        orientation: field(text, &format!("{prefix}:LightroomOrientation")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        edited: field(text, &format!("{prefix}:LightroomEdited")).is_some_and(|v| v.trim() == "1"),
        file: field(text, &format!("{prefix}:LightroomFile")).filter(|s| !s.trim().is_empty()),
    }
}

/// Lightroom data of the sidecar `p` (empty if the file is missing or carries none).
pub fn lightroom(p: &Path) -> Lightroom {
    text(p).map(|t| lightroom_from_text(&t)).unwrap_or_default()
}

/// Write the Lightroom data (see [`Lightroom`]) and, if given, the virtual copies (`graineo:VirtualCopies`, JSON)
/// into the sidecar. Everything else stays character for character (ratings, keywords, own develop settings,
/// foreign `crs:` fields); atomic. An existing but unreadable file is not touched. The modification time of
/// an existing file is kept: the library compares it with its own timestamps for ratings and keywords,
/// which this write does not change.
pub fn write_lightroom(p: &Path, lr: &Lightroom, copies_json: Option<&str>) -> Result<(), String> {
    if !writes_allowed(p) {
        return Ok(());
    }
    let old = match std::fs::read_to_string(p) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let old_mtime = old.as_ref().and_then(|_| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok());
    if let Some(t) = &old {
        if !t.contains("rdf:Description") || !t.contains("</x:xmpmeta>") && !t.contains("</rdf:RDF>") {
            return Err(format!("{} ist kein lesbares XMP – nicht überschrieben", p.display()));
        }
    }
    let base = old.unwrap_or_else(new_file);
    let text = lightroom_into(&base, lr, copies_json).map_err(|e| format!("{}: {e}", p.display()))?;
    crate::files::write_atomic(p, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))?;
    if let Some(t) = old_mtime {
        let _ = std::fs::File::options().write(true).open(p).and_then(|f| f.set_modified(t));
    }
    Ok(())
}

/// Text of an XMP packet `base` with the Lightroom data `lr` (and, if given, the virtual copies), see [`write_lightroom`].
pub fn lightroom_into(base: &str, lr: &Lightroom, copies_json: Option<&str>) -> Result<String, String> {
    let d = base.find("<rdf:Description").ok_or_else(|| "kein lesbares XMP – nicht überschrieben".to_string())?;
    let day_end = d + base[d..].find('>').ok_or("XMP unvollständig")?;
    let itself = base.as_bytes()[day_end - 1] == b'/';
    let mut tag = base[d..if itself { day_end - 1 } else { day_end }].to_string();
    for a in ["graineo:LightroomOrientation", "graineo:LightroomEdited", "graineo:LightroomFile"] {
        tag = without_attr(&tag, a);
    }
    if !base.contains("xmlns:graineo=") {
        tag.push_str(&format!("\n    xmlns:graineo=\"{NS}\""));
    }
    if let Some(o) = &lr.orientation {
        tag.push_str(&format!("\n   graineo:LightroomOrientation=\"{}\"", esc(o)));
    }
    if lr.edited {
        tag.push_str("\n   graineo:LightroomEdited=\"1\"");
    }
    if let Some(f) = &lr.file {
        tag.push_str(&format!("\n   graineo:LightroomFile=\"{}\"", esc(f)));
    }
    let (body, rest) = if itself {
        (String::new(), &base[day_end + 1..])
    } else {
        let e = day_end + 1 + matching_end(&base[day_end + 1..]).ok_or("XMP unvollständig")?;
        (base[day_end + 1..e].to_string(), &base[e + "</rdf:Description>".len()..])
    };
    let mut body = without_element(&body, "graineo:LightroomDevelop");
    if copies_json.is_some() {
        body = without_element(&body, "graineo:VirtualCopies");
    }
    let mut body = body.trim_end().to_string();
    if let Some(k) = copies_json {
        body.push_str(&format!("\n   <graineo:VirtualCopies>{}</graineo:VirtualCopies>", esc(k)));
    }
    if let Some(t) = &lr.develop {
        body.push_str(&format!("\n   <graineo:LightroomDevelop>{}</graineo:LightroomDevelop>", esc(t)));
    }
    let body = format!("{body}\n  ");
    let text = format!("{}{tag}>{body}</rdf:Description>{rest}", &base[..d]);
    Ok(text)
}

#[cfg(test)]
mod tests {

    /// Develop settings go into the outer description, also when named faces (nested descriptions) come first.
    #[test]
    fn develop_settings_beside_nested_faces() {
        let base = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"><mwg-rs:Regions><rdf:Bag><rdf:li><rdf:Description mwg-rs:Name="Lotte"></rdf:Description></rdf:li></rdf:Bag></mwg-rs:Regions></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let t = develop_into(base, Some("{}")).unwrap();
        let face_end = t.find("mwg-rs:Name=\"Lotte\"></rdf:Description>").expect("face description intact");
        let dev = t.find("<graineo:DevelopSettings>").unwrap();
        assert!(dev > face_end, "settings after the face, in the outer description: {t}");
        assert_eq!(develop_from_text(&t).as_deref(), Some("{}"));
        assert_eq!(t.matches("</rdf:Description>").count(), 2);
    }
    use super::*;

    /// A sidecar as Lightroom Classic writes it with "Export containing keywords" (default): `dc:subject` holds the
    /// leaves *and* every parent term, `lr:hierarchicalSubject` the full paths. Artificial file, written here.
    const LIGHTROOM_KEYWORDS: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0-c000 1.000000, 0000/00/00-00:00:00        ">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
   xmp:Rating="3">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>Hund</rdf:li>
     <rdf:li>Italien</rdf:li>
     <rdf:li>Labrador</rdf:li>
     <rdf:li>Orte</rdf:li>
     <rdf:li>Rom</rdf:li>
     <rdf:li>Sommer</rdf:li>
     <rdf:li>Tiere</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>Orte|Italien|Rom</rdf:li>
     <rdf:li>Tiere|Hund|Labrador</rdf:li>
     <rdf:li>Sommer</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    /// Parent terms Lightroom repeats in `dc:subject` are not taken as extra top-level keywords; a flat keyword
    /// that is no segment of any path stays.
    #[test]
    fn lightroom_parents_in_dc_subject_are_no_extra_roots() {
        let w = read_text(LIGHTROOM_KEYWORDS).unwrap();
        let mut got = w.keywords.clone();
        got.sort();
        assert_eq!(got, vec!["Orte|Italien|Rom", "Sommer", "Tiere|Hund|Labrador"], "{:?}", w.keywords);
        // a flat keyword beside the paths (another program, no hierarchy) is kept
        let extra = LIGHTROOM_KEYWORDS.replace("<rdf:li>Tiere</rdf:li>", "<rdf:li>Tiere</rdf:li>\n     <rdf:li>Strand</rdf:li>");
        assert!(read_text(&extra).unwrap().keywords.contains(&"Strand".to_string()));
        // written by graineo: leaves and parents in dc:subject, each once (two "Paris" in different branches),
        // and read back to exactly the same paths
        let dir = std::env::temp_dir().join(format!("xmp-kw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xmp");
        let paths = vec!["Orte|Frankreich|Paris".to_string(), "Personen|Paris".to_string(), "AC/DC".to_string()];
        let w = Values { keywords: paths.clone(), has_keywords: true, ..Default::default() };
        write(&p, &w).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let subject = element(&text, "dc:subject").map(li_texts).unwrap();
        assert_eq!(subject, vec!["Paris", "AC/DC", "Orte", "Frankreich", "Personen"]);
        let mut back = read_text(&text).unwrap().keywords;
        back.sort();
        let mut want = paths;
        want.sort();
        assert_eq!(back, want);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lightroom_data_round_trip_keeps_everything_else() {
        let dir = std::env::temp_dir().join(format!("xmp-lr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xmp");
        std::fs::write(&p, LR).unwrap();
        write_develop(&p, Some("{\"exposure\":0.5}")).unwrap();
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let text = "s = { Exposure2012 = 0.35,\nLook = { Name = \"A & <B>\" },\nToneCurvePV2012 = { 0, 0, 255, 255 } }";
        let lr = Lightroom { develop: Some(text.into()), orientation: Some("BC".into()), edited: true, file: Some("A.cr3".into()) };
        assert!(lr.belongs_to(Path::new("/x/a.CR3")) && !lr.belongs_to(Path::new("/x/a.JPG")));
        assert!(lr.clone().for_image(Path::new("/x/a.JPG")).is_empty());
        write_lightroom(&p, &lr, Some("[{\"name\":\"Kopie 1\",\"lightroomDevelop\":\"s = { }\"}]")).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before, "modification time kept");
        assert_eq!(lightroom(&p), lr);
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("crs:Exposure2012=\"+0.35\"") && t.contains("<crs:ToneCurvePV2012>") && t.contains("xmp:Rating=\"3\""), "{t}");
        assert_eq!(read_develop(&p).unwrap().as_deref(), Some("{\"exposure\":0.5}"));
        let ReadResult::Ok(w) = read(&p) else { panic!() };
        assert_eq!(w.stars, 3);
        assert!(w.copies_json.unwrap().contains("Kopie 1"));
        // writing again replaces, never doubles
        write_lightroom(&p, &Lightroom { orientation: Some("AB".into()), ..lr.clone() }, None).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("<graineo:LightroomDevelop>").count(), 1);
        assert_eq!(t.matches("graineo:LightroomOrientation=").count(), 1);
        assert_eq!(t.matches("<graineo:VirtualCopies>").count(), 1, "copies stay when not given");
        assert_eq!(lightroom(&p).orientation.as_deref(), Some("AB"));
        // saving the rating or the development leaves the Lightroom data in place
        write(&p, &Values { stars: 1, ..Default::default() }).unwrap();
        write_develop(&p, Some("{}")).unwrap();
        assert_eq!(lightroom(&p).develop.as_deref(), Some(text));
        // new file
        let q = dir.join("neu.xmp");
        write_lightroom(&q, &lr, None).unwrap();
        assert_eq!(lightroom(&q), lr);
        assert!(matches!(read(&q), ReadResult::Ok(_)));
        // unreadable file: untouched
        let k = dir.join("kaputt.xmp");
        std::fs::write(&k, "kaputt").unwrap();
        assert!(write_lightroom(&k, &lr, None).is_err());
        assert_eq!(std::fs::read_to_string(&k).unwrap(), "kaputt");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const LR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   xmp:Rating="3"
   xmp:Label="Grün"
   crs:Exposure2012="+0.35"
   crs:Contrast2012="+12">
   <crs:ToneCurvePV2012>
    <rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn faces_are_written_and_replaced_as_mwg_regions() {
        let base = Values { stars: 3, regions: vec![Region { name: "Anna & Co".into(), x: 0.1, y: 0.2, b: 0.2, h: 0.3 }], region_dims: Some((6000, 4000)), ..Default::default() };
        let t = insert_into(&new_file(), &base).unwrap();
        assert!(t.contains("mwg-rs:Name=\"Anna &amp; Co\"") && t.contains("mwg-rs:Type=\"Face\""), "{t}");
        assert!(t.contains("stArea:x=\"0.200000\"") && t.contains("stArea:y=\"0.350000\""), "center instead of corner: {t}");
        assert!(t.contains("stDim:w=\"6000\"") && t.contains("xmlns:mwg-rs=") && t.contains("xmlns:stArea="), "{t}");
        // a second write replaces the regions; exactly one region list remains
        let new = Values { regions: vec![Region { name: "Ben".into(), x: 0.5, y: 0.5, b: 0.1, h: 0.1 }], ..base.clone() };
        let t2 = insert_into(&t, &new).unwrap();
        assert_eq!(t2.matches("<mwg-rs:Regions").count(), 1, "{t2}");
        assert!(t2.contains("mwg-rs:Name=\"Ben\"") && !t2.contains("Anna"), "{t2}");
        // without own faces, existing regions stay untouched
        let t3 = insert_into(&t2, &Values { stars: 1, ..Default::default() }).unwrap();
        // copyright: written as dc:rights, replaced on change, a foreign one stays when we have none
        let c1 = insert_into(&t3, &Values { stars: 1, copyright: Some("© 2026 A & B".into()), ..Default::default() }).unwrap();
        assert!(c1.contains("<rdf:li xml:lang=\"x-default\">© 2026 A &amp; B</rdf:li>"), "{c1}");
        let c2 = insert_into(&c1, &Values { stars: 1, copyright: Some("© 2027".into()), ..Default::default() }).unwrap();
        assert_eq!(c2.matches("<dc:rights>").count(), 1);
        assert!(c2.contains("© 2027") && !c2.contains("© 2026"));
        assert!(insert_into(&c2, &Values { stars: 1, ..Default::default() }).unwrap().contains("© 2027"));
        assert!(t3.contains("mwg-rs:Name=\"Ben\""), "{t3}");
        // once the faces are evaluated and none is named any more, our regions are dropped
        let t4 = insert_into(&t3, &Values { stars: 1, clear_regions: true, ..Default::default() }).unwrap();
        assert!(!t4.contains("mwg-rs:Regions") && !t4.contains("Ben") && !t4.contains("graineo:Regions"), "{t4}");
        // foreign regions (without our marker) stay even then
        let foreign = t3.replace("graineo:Regions=\"1\"", "");
        let t5 = insert_into(&foreign, &Values { stars: 1, clear_regions: true, ..Default::default() }).unwrap();
        assert!(t5.contains("mwg-rs:Name=\"Ben\""), "{t5}");
    }

    #[test]
    fn foreign_fields_remain() {
        let w = Values { stars: 5, flag: 1, label: 2, keywords: vec!["Orte|Italien|Rom".into(), "Anna".into()], rating_time: Some(100), keyword_time: Some(200), copies_json: Some("[{\"name\":\"Kopie 1\"}]".into()), ..Default::default() };
        let t = insert_into(LR, &w).unwrap();
        assert!(t.contains("crs:Exposure2012=\"+0.35\"") && t.contains("crs:Contrast2012=\"+12\"") && t.contains("<crs:ToneCurvePV2012>"));
        assert_eq!(t.matches("xmp:Rating=").count(), 1);
        let dir = std::env::temp_dir().join(format!("xmp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xmp");
        std::fs::write(&p, &t).unwrap();
        let ReadResult::Ok(r) = read(&p) else { panic!() };
        assert_eq!(r, Values { has_rating: true, has_keywords: true, ..w.clone() });
        // writing twice: stable
        write(&p, &w).unwrap();
        let t2 = std::fs::read_to_string(&p).unwrap();
        write(&p, &w).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), t2);
        // without keywords: elements disappear, crs stays
        write(&p, &Values { stars: 1, ..Default::default() }).unwrap();
        let t3 = std::fs::read_to_string(&p).unwrap();
        assert!(!t3.contains("dc:subject") && t3.contains("crs:ToneCurvePV2012"));
        // a broken file is backed up, not lost
        std::fs::write(&p, "kaputt").unwrap();
        assert_eq!(read(&p), ReadResult::Broken);
        write(&p, &w).unwrap();
        assert!(matches!(read(&p), ReadResult::Ok(_)));
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".defekt-")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn develop_next_to_crs() {
        let dir = std::env::temp_dir().join(format!("xmp-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.xmp");
        std::fs::write(&p, LR).unwrap();
        write_develop(&p, Some("{\"exposure\":0.5,\"a\":\"<&>\"}")).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("crs:Exposure2012=\"+0.35\"") && t.contains("<crs:ToneCurvePV2012>"));
        assert_eq!(read_develop(&p).unwrap().as_deref(), Some("{\"exposure\":0.5,\"a\":\"<&>\"}"));
        // writing the rating leaves the development in place
        write(&p, &Values { stars: 2, ..Default::default() }).unwrap();
        assert!(read_develop(&p).unwrap().is_some());
        write_develop(&p, Some("{}")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap().matches("DevelopSettings>").count(), 2);
        write_develop(&p, None).unwrap();
        assert_eq!(read_develop(&p).unwrap(), None);
        // new file
        let q = dir.join("neu.xmp");
        write_develop(&q, Some("{}")).unwrap();
        assert!(matches!(read(&q), ReadResult::Ok(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lightroom_label_and_reject() {
        let dir = std::env::temp_dir().join(format!("xmp-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("b.xmp");
        std::fs::write(&p, LR.replace("xmp:Rating=\"3\"", "xmp:Rating=\"-1\"")).unwrap();
        let ReadResult::Ok(r) = read(&p) else { panic!() };
        assert_eq!((r.stars, r.flag, r.label), (0, -1, 3));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(path(Path::new("/a/IMG_1.CR3"), "CR3", false), PathBuf::from("/a/IMG_1.xmp"));
        assert_eq!(path(Path::new("/a/IMG_1.JPG"), "JPG", true), PathBuf::from("/a/IMG_1.JPG.xmp"));
        assert_eq!(path(Path::new("/a/IMG_1.JPG"), "JPG", false), PathBuf::from("/a/IMG_1.xmp"));
    }

    #[test]
    fn own_fields_are_replaced_foreign_ones_stay() {
        let old = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:graineo="https://graineo.github.io/ns/1.0/"
   xmp:Rating="4"
   graineo:Flag="1"
   graineo:RatingTime="111"
   crs:Exposure2012="+0.35">
   <graineo:VirtualCopies>[{&quot;name&quot;:&quot;K1&quot;}]</graineo:VirtualCopies>
   <graineo:DevelopSettings>{&quot;a&quot;:1}</graineo:DevelopSettings>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        let dir = std::env::temp_dir().join(format!("xmp-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xmp");
        std::fs::write(&p, old).unwrap();
        let ReadResult::Ok(w) = read(&p) else { panic!() };
        assert_eq!((w.stars, w.flag, w.rating_time), (4, 1, Some(111)));
        assert_eq!(w.copies_json.as_deref(), Some("[{\"name\":\"K1\"}]"));
        assert_eq!(read_develop(&p).unwrap().as_deref(), Some("{\"a\":1}"));
        // saving the rating: our fields are replaced, foreign ones and the development stay
        write(&p, &Values { stars: 2, flag: -1, rating_time: Some(222), copies_json: w.copies_json.clone(), ..Default::default() }).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("graineo:Flag=\"-1\"") && t.contains("graineo:RatingTime=\"222\"") && t.contains("<graineo:VirtualCopies>"));
        assert_eq!(t.matches("graineo:Flag=").count(), 1);
        assert_eq!(t.matches("xmlns:graineo=\"https://graineo.github.io/ns/1.0/\"").count(), 1, "{t}");
        assert!(t.contains("<graineo:DevelopSettings>{&quot;a&quot;:1}</graineo:DevelopSettings>"), "{t}");
        assert!(t.contains("crs:Exposure2012=\"+0.35\"") && t.contains("<graineo:DevelopSettings>"));
        let ReadResult::Ok(w2) = read(&p) else { panic!() };
        assert_eq!((w2.stars, w2.flag, w2.rating_time), (2, -1, Some(222)));
        write_develop(&p, Some("{\"a\":2}")).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("<graineo:DevelopSettings>") && t.contains("crs:Exposure2012=\"+0.35\"") && t.matches("graineo:DevelopSettings>").count() == 2);
        assert_eq!(read_develop(&p).unwrap().as_deref(), Some("{\"a\":2}"));
        // rewriting is stable (the order of the elements follows the last writer)
        let values = Values { stars: 2, flag: -1, rating_time: Some(222), copies_json: w.copies_json.clone(), ..Default::default() };
        write(&p, &values).unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        write(&p, &values).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
        assert_eq!(read_develop(&p).unwrap().as_deref(), Some("{\"a\":2}"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A test run (own data folder) never writes a sidecar next to the person's photos; inside its data
    /// folder and the temporary folder it may. `GRAINEO_NO_XMP=1` stops every sidecar, `=0` allows all; the installed
    /// app (no data folder) writes as before.
    #[test]
    fn test_runs_write_no_sidecars_next_to_the_photos() {
        let data = std::env::temp_dir().join("graineo-xmp-gate");
        let home_photo = Path::new("/Users/someone/Pictures/2024/DSC_0001.xmp");
        assert!(!allowed_by(None, Some(&data), home_photo));
        assert!(allowed_by(None, Some(&data), &data.join("photos/a.xmp")));
        assert!(allowed_by(None, Some(&data), &std::env::temp_dir().join("x/a.xmp")));
        assert!(allowed_by(None, None, home_photo));
        assert!(!allowed_by(Some("1"), None, &data.join("a.xmp")));
        assert!(allowed_by(Some("0"), Some(&data), home_photo));
    }
}