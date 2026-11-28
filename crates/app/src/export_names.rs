//! File names of an export: a pattern with placeholders, turned into one safe file name per photo.
//!
//! The pattern is plain text with placeholders in braces; text outside braces is kept as typed:
//! `{name}` original file name without extension, `{mono}` "_mono" for black and white, `{n}` / `{n:3}` running
//! number (with leading zeros), `{date}` / `{date:YYYYMMDD}` capture date (YYYY YY MM DD hh mm ss), `{camera}`,
//! `{title}`. A placeholder without a value (no title, no capture date) leaves no doubled separator behind.
//! The default pattern gives the names of earlier versions: `IMG_0001.jpg`, `IMG_0001_mono.jpg`.
//! A typed subfolder (`Subfolder::Custom`) takes the same placeholders, "/" separates nested folders.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Pattern of earlier versions: original name, "_mono" for black and white.
pub const DEFAULT_PATTERN: &str = "{name}{mono}";
/// Capture date format of `{date}` without a format of its own.
pub const DEFAULT_DATE: &str = "YYYY-MM-DD";
/// Longest file name stem in bytes (leaves room for "-2", format suffix and extension within the usual 255).
pub const MAX_STEM: usize = 200;
/// Characters that count as separators next to an empty placeholder.
const SEPARATORS: &[char] = &[' ', '_', '-', '.'];

/// What happens when the file name is taken in the target folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Collision {
    /// "Name-2.jpg": nothing is overwritten (the behaviour of earlier versions)
    #[default]
    Number,
    /// the existing file is replaced (it goes to the trash); asked before the export
    Overwrite,
    /// the photo is not exported
    Skip,
}

/// Subfolders inside the export folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Subfolder {
    #[default]
    None,
    /// `YYYY/YYYY-MM-DD` of the capture date (photos without a date: directly in the folder)
    Date,
    /// name of the folder the original lies in
    Folder,
    /// typed text ([`Naming::subfolder_text`]) with the placeholders of the pattern, "/" for nested folders
    Custom,
}

/// The naming rules of an export (stored as the last used ones and in export templates).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Naming {
    pub pattern: String,
    /// first value of `{n}`
    pub start: u32,
    pub subfolder: Subfolder,
    pub collision: Collision,
    /// text of [`Subfolder::Custom`]
    #[serde(skip_serializing_if = "String::is_empty")]
    pub subfolder_text: String,
    /// target: the folder of each original instead of the chosen export folder
    #[serde(skip_serializing_if = "is_false")]
    pub beside: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for Naming {
    fn default() -> Self {
        Naming { pattern: DEFAULT_PATTERN.into(), start: 1, subfolder: Subfolder::None, collision: Collision::Number, subfolder_text: String::new(), beside: false }
    }
}

impl Naming {
    /// Do the rules use `{key…}` (pattern or typed subfolder)?
    pub fn uses(&self, key: &str) -> bool {
        let token = format!("{{{key}");
        self.pattern.contains(&token) || (self.subfolder == Subfolder::Custom && self.subfolder_text.contains(&token))
    }
}

/// What a photo offers for its name.
#[derive(Clone, Debug, Default)]
pub struct Info {
    /// original file name without extension
    pub stem: String,
    pub mono: bool,
    /// capture time `YYYY-MM-DDThh:mm:ss` (local, as in EXIF)
    pub capture: Option<String>,
    pub camera: Option<String>,
    pub title: Option<String>,
    /// name of the folder of the original
    pub folder: Option<String>,
    /// folder of the original (target of [`Naming::beside`])
    pub source_dir: Option<PathBuf>,
}

/// The name of one photo: `base` as the pattern gives it, `unique` with "-2", "-3" … where several photos of the same
/// export would get the same name. Both relative to the export folder (subfolders included), without extension.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub base: PathBuf,
    pub unique: PathBuf,
}

/// All names of an export.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub items: Vec<Planned>,
    /// photos whose name another photo of the same export has already
    pub duplicates: usize,
}

/// Names of a running export, per item (relative stem without extension; empty: names as in earlier versions).
#[derive(Clone, Debug, Default)]
pub struct JobNames {
    pub names: Vec<PathBuf>,
    pub collision: Collision,
}

impl JobNames {
    /// The names the job writes: as planned; with "add a number" the job numbers on its own (a duplicate gets the
    /// next free name once the first file is written), the other modes keep the duplicates apart beforehand.
    pub fn from_plan(plan: &Plan, collision: Collision) -> JobNames {
        let names = plan.items.iter().map(|p| if collision == Collision::Number { p.base.clone() } else { p.unique.clone() }).collect();
        JobNames { names, collision }
    }

    /// Relative stem of item `i`, if planned.
    pub fn get(&self, i: usize) -> Option<&PathBuf> {
        self.names.get(i)
    }
}

enum Segment {
    Text(String),
    Value(String),
}

/// File name stem of `info` with `pattern`; `number` is the value of `{n}`. Safe as a file name on macOS and Windows.
pub fn render(pattern: &str, info: &Info, number: u32) -> String {
    let name = sanitize(&fill(pattern, info, number));
    if name.is_empty() {
        let fallback = sanitize(&info.stem);
        if fallback.is_empty() { "image".into() } else { fallback }
    } else {
        name
    }
}

/// `pattern` with its placeholders filled in, not yet made safe.
fn fill(pattern: &str, info: &Info, number: u32) -> String {
    let mut out = String::new();
    // trailing separators of `out` that came from typed text (dropped next to an empty placeholder)
    let mut text_tail = 0usize;
    let mut after_empty = false;
    for seg in segments(pattern, info, number) {
        match seg {
            Segment::Text(t) => {
                let t = if after_empty && (out.is_empty() || text_tail > 0) { t.trim_start_matches(SEPARATORS).to_string() } else { t };
                if t.is_empty() {
                    continue;
                }
                let tail = t.len() - t.trim_end_matches(SEPARATORS).len();
                text_tail = if tail == t.len() { text_tail + tail } else { tail };
                out.push_str(&t);
                after_empty = false;
            }
            Segment::Value(v) if v.is_empty() => after_empty = true,
            Segment::Value(v) => {
                out.push_str(&v);
                text_tail = 0;
                after_empty = false;
            }
        }
    }
    if after_empty {
        out.truncate(out.len() - text_tail.min(out.len()));
    }
    out
}

/// A typed subfolder: placeholders filled in, "/" (or "\\") separates nested folders, every part made safe for macOS
/// and Windows; empty parts, "." and ".." are dropped (the folder never leaves the export folder).
pub fn render_folder(text: &str, info: &Info, number: u32) -> PathBuf {
    let mut path = PathBuf::new();
    for part in text.split(['/', '\\']) {
        let name = sanitize(&fill(part, info, number));
        if !name.is_empty() && name.chars().any(|c| c != '.' && c != '_') {
            path.push(name);
        }
    }
    path
}

fn segments(pattern: &str, info: &Info, number: u32) -> Vec<Segment> {
    let mut list = Vec::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else { break };
        let token = &rest[open + 1..open + len];
        match value(token, info, number) {
            Some(v) => {
                if open > 0 {
                    list.push(Segment::Text(rest[..open].to_string()));
                }
                list.push(Segment::Value(v));
            }
            // unknown placeholder: kept as typed
            None => list.push(Segment::Text(rest[..open + len + 1].to_string())),
        }
        rest = &rest[open + len + 1..];
    }
    if !rest.is_empty() {
        list.push(Segment::Text(rest.to_string()));
    }
    list
}

fn value(token: &str, info: &Info, number: u32) -> Option<String> {
    let (key, arg) = match token.split_once(':') {
        Some((k, a)) => (k.trim(), Some(a)),
        None => (token.trim(), None),
    };
    let clean = |v: &Option<String>| v.as_deref().map(|s| s.trim().to_string()).unwrap_or_default();
    Some(match key {
        "name" => info.stem.clone(),
        "mono" => if info.mono { "_mono".into() } else { String::new() },
        "n" => {
            let digits = arg.and_then(|a| a.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 9);
            format!("{number:0digits$}")
        }
        "date" => info.capture.as_deref().and_then(|c| format_date(c, arg.unwrap_or(DEFAULT_DATE))).unwrap_or_default(),
        "camera" => clean(&info.camera),
        "title" => clean(&info.title),
        _ => return None,
    })
}

/// `capture` (`YYYY-MM-DDThh:mm:ss`, also with a space) in `format`: YYYY YY MM DD hh mm ss, everything else as typed.
pub fn format_date(capture: &str, format: &str) -> Option<String> {
    let b = capture.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<&str> { capture.get(r.clone()).filter(|s| s.bytes().all(|c| c.is_ascii_digit())) };
    let (y, mo, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let (h, mi, s) = (digits(11..13).unwrap_or("00"), digits(14..16).unwrap_or("00"), digits(17..19).unwrap_or("00"));
    let mut out = String::new();
    let mut rest = format;
    while !rest.is_empty() {
        let (part, len) = if rest.starts_with("YYYY") {
            (y, 4)
        } else if rest.starts_with("YY") {
            (&y[2..], 2)
        } else if rest.starts_with("MM") {
            (mo, 2)
        } else if rest.starts_with("DD") {
            (d, 2)
        } else if rest.starts_with("hh") {
            (h, 2)
        } else if rest.starts_with("mm") {
            (mi, 2)
        } else if rest.starts_with("ss") {
            (s, 2)
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        };
        out.push_str(part);
        rest = &rest[len..];
    }
    Some(out)
}

/// A name that is valid on macOS and Windows: no path separators or characters Windows forbids (replaced by "_"),
/// no control characters, no leading dot (hidden file), no trailing dot or space, no reserved device name, at most
/// [`MAX_STEM`] bytes.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    s = s.trim().to_string();
    if s.len() > MAX_STEM {
        let mut end = MAX_STEM;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    let s = s.trim_end_matches(['.', ' ']).to_string();
    let s = match s.strip_prefix('.') {
        Some(r) => format!("_{r}"),
        None => s,
    };
    let device = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL") || (device.len() == 4 && (device.starts_with("COM") || device.starts_with("LPT")) && device.as_bytes()[3].is_ascii_digit() && device.as_bytes()[3] != b'0');
    if !reserved {
        return s;
    }
    // the prefix must not push a name of full length beyond the limit (and a cut must not end in a dot or space)
    let mut s = format!("_{s}");
    if s.len() > MAX_STEM {
        let mut end = MAX_STEM;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s = s.trim_end_matches(['.', ' ']).to_string();
    }
    s
}

/// Subfolder of a photo (relative, empty: directly in the export folder).
pub fn subfolder(rule: Subfolder, info: &Info) -> PathBuf {
    match rule {
        Subfolder::None => PathBuf::new(),
        Subfolder::Date => match info.capture.as_deref().and_then(|c| Some((format_date(c, "YYYY")?, format_date(c, "YYYY-MM-DD")?))) {
            Some((y, d)) => PathBuf::from(y).join(d),
            None => PathBuf::new(),
        },
        Subfolder::Folder => match info.folder.as_deref().map(sanitize).filter(|f| !f.is_empty()) {
            Some(f) => PathBuf::from(f),
            None => PathBuf::new(),
        },
        // needs the text and the number: [`folder_of`]
        Subfolder::Custom => PathBuf::new(),
    }
}

/// Folder of photo `info` with the rules of `naming`: relative to the export folder, or – with [`Naming::beside`] – the
/// original's folder (absolute) plus the subfolder.
pub fn folder_of(naming: &Naming, info: &Info, number: u32) -> PathBuf {
    let sub = match naming.subfolder {
        Subfolder::Custom => render_folder(&naming.subfolder_text, info, number),
        rule => subfolder(rule, info),
    };
    match (naming.beside, &info.source_dir) {
        (true, Some(dir)) => dir.join(sub),
        _ => sub,
    }
}

/// Names of all photos of an export, in order (`{n}` counts from `naming.start`). `extension(i)`: what follows the stem
/// of photo `i` (".jpg", "_linear.tif" …); photos with the same name and extension (regardless of case: the Mac's
/// file system does not tell them apart) are counted as duplicates and get "-2", "-3" … in `unique`.
pub fn plan(naming: &Naming, infos: &[Info], extension: impl Fn(usize) -> String) -> Plan {
    let mut taken = std::collections::HashSet::new();
    let mut duplicates = 0;
    let items = infos
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let number = naming.start.saturating_add(i as u32);
            let base = folder_of(naming, info, number).join(render(&naming.pattern, info, number));
            let ext = extension(i);
            let key = |p: &PathBuf| format!("{}{ext}", p.to_string_lossy()).to_lowercase();
            let mut unique = base.clone();
            if !taken.insert(key(&base)) {
                duplicates += 1;
                let stem = base.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                for k in 2.. {
                    let candidate = base.with_file_name(format!("{stem}-{k}"));
                    if taken.insert(key(&candidate)) {
                        unique = candidate;
                        break;
                    }
                }
            }
            Planned { base, unique }
        })
        .collect();
    Plan { items, duplicates }
}

/// Digits of the first `{n}` placeholder (`None`: the pattern has no number).
pub fn number_digits(pattern: &str) -> Option<usize> {
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let len = rest[open..].find('}')?;
        let token = &rest[open + 1..open + len];
        match token.split_once(':') {
            Some(("n", d)) => return Some(d.trim().parse::<usize>().unwrap_or(1).clamp(1, 9)),
            None if token == "n" => return Some(1),
            _ => {}
        }
        rest = &rest[open + len + 1..];
    }
    None
}

/// `pattern` with every number placeholder set to `digits` digits.
pub fn with_number_digits(pattern: &str, digits: usize) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else { break };
        let token = &rest[open + 1..open + len];
        out.push_str(&rest[..open]);
        if token == "n" || token.split_once(':').is_some_and(|(k, _)| k == "n") {
            out.push_str(&format!("{{n:{}}}", digits.clamp(1, 9)));
        } else {
            out.push_str(&rest[open..open + len + 1]);
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info {
        Info { stem: "IMG_0001".into(), mono: false, capture: Some("2026-07-14T09:05:03".into()), camera: Some("Canon EOS RP".into()), title: Some("Strand".into()), folder: Some("Urlaub 2026".into()), source_dir: None }
    }

    #[test]
    fn default_pattern_gives_the_names_of_earlier_versions() {
        assert_eq!(render(DEFAULT_PATTERN, &info(), 1), "IMG_0001");
        assert_eq!(render(DEFAULT_PATTERN, &Info { mono: true, ..info() }, 1), "IMG_0001_mono");
        assert_eq!(Naming::default().pattern, DEFAULT_PATTERN);
        assert_eq!(Naming::default().collision, Collision::Number);
    }

    #[test]
    fn placeholders() {
        let i = info();
        assert_eq!(render("{name}_{title}", &i, 1), "IMG_0001_Strand");
        assert_eq!(render("{date}_{n:3}", &i, 7), "2026-07-14_007");
        assert_eq!(render("{date:YYYYMMDD}-{date:hhmmss}", &i, 1), "20260714-090503");
        assert_eq!(render("{date:DD.MM.YY}", &i, 1), "14.07.26");
        assert_eq!(render("{camera} {n}", &i, 12), "Canon EOS RP 12");
        assert_eq!(render("Urlaub {n:2}", &i, 3), "Urlaub 03");
        // unknown placeholders stay as typed, text around them too
        assert_eq!(render("{name}{foo}x", &i, 1), "IMG_0001{foo}x");
    }

    #[test]
    fn missing_values_leave_no_doubled_separators() {
        let bare = Info { stem: "DSC_0042".into(), ..Default::default() };
        assert_eq!(render("{name}_{title}", &bare, 1), "DSC_0042");
        assert_eq!(render("{title}_{name}", &bare, 1), "DSC_0042");
        assert_eq!(render("{name}_{title}_{n:3}", &bare, 5), "DSC_0042_005");
        assert_eq!(render("{name} - {camera} - {date}", &bare, 1), "DSC_0042");
        assert_eq!(render("{date}_{camera}_{name}", &bare, 1), "DSC_0042");
        // a separator belonging to the original name stays
        let trailing = Info { stem: "scan_".into(), ..Default::default() };
        assert_eq!(render("{name}{mono}", &trailing, 1), "scan_");
        // nothing left: the original name
        assert_eq!(render("{title}", &bare, 1), "DSC_0042");
        assert_eq!(render("", &Info::default(), 1), "image");
    }

    #[test]
    fn forbidden_characters_and_length() {
        let i = Info { title: Some("Ja/Nein: \"Was?\" <1|2>*\\".into()), ..info() };
        assert_eq!(render("{title}", &i, 1), "Ja_Nein_ _Was__ _1_2___");
        assert_eq!(sanitize(".hidden"), "_hidden");
        assert_eq!(sanitize("end. . "), "end");
        assert_eq!(sanitize("tab\there"), "tab_here");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("com1.x"), "_com1.x");
        assert_eq!(sanitize("COM0"), "COM0");
        assert_eq!(sanitize("CONTRA"), "CONTRA");
        let long = "ä".repeat(150);
        let s = sanitize(&long);
        assert!(s.len() <= MAX_STEM && s.chars().all(|c| c == 'ä'), "{}", s.len());
    }

    #[test]
    fn numbering_and_duplicates_across_photos() {
        let naming = Naming { pattern: "Reise_{n:3}".into(), start: 9, ..Default::default() };
        let infos = vec![info(), info(), info()];
        let p = plan(&naming, &infos, |_| ".jpg".into());
        let names: Vec<_> = p.items.iter().map(|x| x.unique.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Reise_009", "Reise_010", "Reise_011"]);
        assert_eq!(p.duplicates, 0);
        // same original name from two folders, a third differing only in case
        let infos = vec![info(), info(), Info { stem: "img_0001".into(), ..info() }, Info { stem: "IMG_0002".into(), ..info() }];
        let p = plan(&Naming::default(), &infos, |_| ".jpg".into());
        let unique: Vec<_> = p.items.iter().map(|x| x.unique.to_string_lossy().into_owned()).collect();
        assert_eq!(unique, ["IMG_0001", "IMG_0001-2", "img_0001-3", "IMG_0002"]);
        assert_eq!(p.duplicates, 2);
        assert_eq!(p.items[1].base, PathBuf::from("IMG_0001"));
        // different extensions do not collide (originals)
        let p = plan(&Naming::default(), &[info(), info()], |i| if i == 0 { ".CR3".into() } else { ".JPG".into() });
        assert_eq!(p.duplicates, 0);
        // "add a number" leaves the numbering to the job, the other modes take the unique names
        let p = plan(&Naming::default(), &[info(), info()], |_| ".jpg".into());
        assert_eq!(JobNames::from_plan(&p, Collision::Number).names, [PathBuf::from("IMG_0001"), PathBuf::from("IMG_0001")]);
        assert_eq!(JobNames::from_plan(&p, Collision::Skip).names, [PathBuf::from("IMG_0001"), PathBuf::from("IMG_0001-2")]);
    }

    #[test]
    fn custom_subfolder_and_beside() {
        let i = Info { stem: "IMG_1".into(), capture: Some("2026-07-14T10:11:12".into()), camera: Some("DP2 Merrill".into()), source_dir: Some(PathBuf::from("/Fotos/Urlaub")), ..Default::default() };
        assert_eq!(render_folder("Export/{date:YYYY}/{camera}", &i, 1), PathBuf::from("Export/2026/DP2 Merrill"));
        assert_eq!(render_folder("{date}", &i, 1), PathBuf::from("2026-07-14"));
        // forbidden characters, "..", empty parts and placeholders without a value
        assert_eq!(render_folder("../a:b//c*?/{title}/.", &i, 1), PathBuf::from("a_b/c__"));
        assert_eq!(render_folder("web\\klein ", &i, 1), PathBuf::from("web/klein"));
        assert_eq!(render_folder("CON", &i, 1), PathBuf::from("_CON"));
        assert_eq!(render_folder("", &i, 1), PathBuf::new());
        let mut n = Naming { subfolder: Subfolder::Custom, subfolder_text: "Export {date:YYYYMMDD}".into(), ..Default::default() };
        assert!(n.uses("date") && !n.uses("title"));
        let p = plan(&n, std::slice::from_ref(&i), |_| ".jpg".into());
        assert_eq!(p.items[0].base, PathBuf::from("Export 20260714/IMG_1"));
        n.beside = true;
        let other = Info { source_dir: Some(PathBuf::from("/Fotos/Heim")), ..i.clone() };
        let p = plan(&n, &[i.clone(), other], |_| ".jpg".into());
        assert_eq!(p.items[0].base, PathBuf::from("/Fotos/Urlaub/Export 20260714/IMG_1"));
        assert_eq!(p.items[1].base, PathBuf::from("/Fotos/Heim/Export 20260714/IMG_1"));
        assert_eq!(p.duplicates, 0, "same name in two folders is no duplicate");
        // stored only when used: old files and old readers stay as they were
        let j = serde_json::to_string(&n).unwrap();
        assert!(j.contains(r#""subfolder":"custom""#) && j.contains(r#""subfolderText":"Export {date:YYYYMMDD}""#) && j.contains(r#""beside":true"#), "{j}");
        assert_eq!(serde_json::from_str::<Naming>(&j).unwrap(), n);
    }

    /// Fuzz: typed subfolders and patterns as a hand-edited settings file or template could carry them (traversal,
    /// absolute paths, drive letters, reserved names, control characters, very long and Unicode text) never leave the
    /// export folder or the original's folder and give only names that are safe on macOS and Windows.
    #[test]
    fn typed_subfolder_fuzz_stays_inside() {
        use std::path::Component;
        let long = "x".repeat(300);
        let pieces: Vec<&str> = vec![
            "..", ".", "/", "\\", "//", "C:", "C:\\", "\\\\server\\share", "~", "CON", "nul.txt", "LPT1", "com9 ", " ", "\u{0}", "\n", "\u{202e}", "é", "e\u{301}", "日本", "😀", "a",
            "{date}", "{date:YYYY}", "{camera}", "{title}", "{n:3}", "{", "}", "{{", "..\\..", "/etc/passwd", "...", ". .", "a.", &long,
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let base = PathBuf::from("/Fotos/Urlaub");
        for case in 0..8000u32 {
            let n = 1 + (rnd() % 8) as usize;
            let text: String = (0..n).map(|_| pieces[(rnd() % pieces.len() as u64) as usize]).collect();
            let i = Info { stem: "IMG_1".into(), capture: Some("2026-07-14T10:11:12".into()), camera: Some(text.clone()), title: Some(text.clone()), folder: Some(text.clone()), source_dir: Some(base.clone()), ..Default::default() };
            let sub = render_folder(&text, &i, case);
            for c in sub.components() {
                let Component::Normal(name) = c else { panic!("case {case}: {text:?} → {sub:?} has {c:?}") };
                let name = name.to_string_lossy();
                assert!(!name.is_empty() && !name.contains(['/', '\\', ':']) && sanitize(&name) == name, "case {case}: {text:?} → part {name:?}");
            }
            let n = Naming { pattern: text.clone(), subfolder: Subfolder::Custom, subfolder_text: text.clone(), beside: case % 2 == 0, ..Default::default() };
            let p = plan(&n, std::slice::from_ref(&i), |_| ".jpg".into());
            let out = &p.items[0].base;
            if n.beside {
                assert!(out.starts_with(&base) && out != &base, "case {case}: {out:?}");
                assert!(out.strip_prefix(&base).unwrap().components().all(|c| matches!(c, Component::Normal(_))), "case {case}: {out:?}");
            } else {
                assert!(out.is_relative() && out.components().all(|c| matches!(c, Component::Normal(_))), "case {case}: {out:?}");
            }
        }
    }

    #[test]
    fn subfolders() {
        let i = info();
        assert_eq!(subfolder(Subfolder::Date, &i), PathBuf::from("2026/2026-07-14"));
        assert_eq!(subfolder(Subfolder::Folder, &i), PathBuf::from("Urlaub 2026"));
        assert_eq!(subfolder(Subfolder::Folder, &Info { folder: Some("a:b".into()), ..i.clone() }), PathBuf::from("a_b"));
        assert_eq!(subfolder(Subfolder::Date, &Info::default()), PathBuf::new());
        let naming = Naming { subfolder: Subfolder::Date, ..Default::default() };
        assert_eq!(plan(&naming, &[i], |_| ".jpg".into()).items[0].unique, PathBuf::from("2026/2026-07-14/IMG_0001"));
    }

    #[test]
    fn number_digits_are_read_and_set() {
        assert_eq!(number_digits("{name}"), None);
        assert_eq!(number_digits("{name}_{n}"), Some(1));
        assert_eq!(number_digits("{n:4}_{name}"), Some(4));
        assert_eq!(with_number_digits("{name}_{n}_{n:2}", 3), "{name}_{n:3}_{n:3}");
        assert_eq!(with_number_digits("{date:YYYY}", 3), "{date:YYYY}");
    }

    #[test]
    fn dates() {
        assert_eq!(format_date("2026-07-14 09:05:03", "YYYY-MM-DD_hhmmss").as_deref(), Some("2026-07-14_090503"));
        assert_eq!(format_date("2026-07-14", "YYYYMMDD-hh").as_deref(), Some("20260714-00"));
        assert_eq!(format_date("garbage", "YYYY"), None);
    }

    #[test]
    fn naming_json_keys_are_english_and_broken_values_fall_back() {
        let n = Naming { pattern: "{n:3}".into(), start: 5, subfolder: Subfolder::Date, collision: Collision::Skip, ..Default::default() };
        let j = serde_json::to_string(&n).unwrap();
        assert_eq!(j, r#"{"pattern":"{n:3}","start":5,"subfolder":"date","collision":"skip"}"#);
        assert_eq!(serde_json::from_str::<Naming>(&j).unwrap(), n);
        assert_eq!(serde_json::from_str::<Naming>(r#"{"pattern":"x"}"#).unwrap().start, 1);
    }
}
