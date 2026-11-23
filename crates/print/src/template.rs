//! Print templates: everything that describes a print job except the photos. Stored as JSON files in
//! `<data_dir>/print-templates/<slug>.json` (English keys, format id and version for later migrations); the
//! built-in templates live in the program ([`builtin`]) and are copied when the user saves one as their own.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Format id of a template file.
pub const FORMAT: &str = "graineo-print-template";
/// Current version of the template format.
pub const VERSION: u32 = 1;
/// Folder of the user's templates inside the data folder.
pub const FOLDER: &str = "print-templates";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub format: String,
    pub version: u32,
    /// shown name; for built-in templates the (German) translation key
    pub name: String,
    pub style: Style,
    pub paper: Paper,
    /// printer and paper name the template was made for (informational; the page setup decides)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printer: Option<PrinterRef>,
    pub margins_mm: Margins,
    pub grid: Grid,
    pub image: ImageOptions,
    pub page: PageOptions,
    pub job: JobOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Style {
    /// rows × columns of equal cells (single photo, contact sheet)
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paper {
    /// id of a known size ([`PAPERS`]) or "custom"
    pub id: String,
    /// size in portrait orientation (width ≤ height)
    pub width_mm: f64,
    pub height_mm: f64,
    pub orientation: Orientation,
}

impl Paper {
    /// Paper of a known size.
    pub fn known(id: &str, orientation: Orientation) -> Option<Paper> {
        PAPERS.iter().find(|p| p.id == id).map(|p| Paper { id: p.id.into(), width_mm: p.width_mm, height_mm: p.height_mm, orientation })
    }

    /// Page size in the chosen orientation (width, height) in mm.
    pub fn size_mm(&self) -> (f64, f64) {
        let (a, b) = (self.width_mm.min(self.height_mm), self.width_mm.max(self.height_mm));
        match self.orientation {
            Orientation::Portrait => (a, b),
            Orientation::Landscape => (b, a),
        }
    }
}

/// A paper size the program knows.
#[derive(Clone, Copy, Debug)]
pub struct PaperSize {
    pub id: &'static str,
    /// shown name (not translated: sizes are international)
    pub name: &'static str,
    pub width_mm: f64,
    pub height_mm: f64,
}

/// Known paper sizes (portrait). Photo sizes are the inch formats sold as "10 × 15" and "13 × 18".
pub const PAPERS: &[PaperSize] = &[
    PaperSize { id: "iso-a4", name: "A4", width_mm: 210.0, height_mm: 297.0 },
    PaperSize { id: "iso-a3", name: "A3", width_mm: 297.0, height_mm: 420.0 },
    PaperSize { id: "super-a3", name: "A3+", width_mm: 329.0, height_mm: 483.0 },
    PaperSize { id: "photo-4x6", name: "10 × 15", width_mm: 101.6, height_mm: 152.4 },
    PaperSize { id: "photo-5x7", name: "13 × 18", width_mm: 127.0, height_mm: 177.8 },
    PaperSize { id: "us-letter", name: "Letter", width_mm: 215.9, height_mm: 279.4 },
];

/// Id of a free paper size.
pub const CUSTOM_PAPER: &str = "custom";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrinterRef {
    pub name: String,
    #[serde(default)]
    pub paper_name: String,
}

/// Margins (or unprintable edges) in mm.
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Margins {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

impl Margins {
    pub fn all(v: f64) -> Margins {
        Margins { top: v, bottom: v, left: v, right: v }
    }
    /// Per side the larger of the two.
    pub fn max(self, o: Margins) -> Margins {
        Margins { top: self.top.max(o.top), bottom: self.bottom.max(o.bottom), left: self.left.max(o.left), right: self.right.max(o.right) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grid {
    pub rows: u32,
    pub columns: u32,
    /// gap between cells: [horizontal (between columns), vertical (between rows)]
    pub gap_mm: [f64; 2],
    /// cell size [width, height]; 0 or larger than possible = as large as possible
    pub cell_mm: [f64; 2],
}

/// Most rows or columns of a grid.
pub const MAX_CELLS: u32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ink {
    Black,
    White,
    Gray,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub on: bool,
    pub width_pt: f64,
    pub color: Ink,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageOptions {
    /// fill the cell (crop the overhang) instead of fitting the whole photo
    pub fill: bool,
    /// turn a photo by 90° when it then becomes larger in its cell
    pub rotate_to_fit: bool,
    /// one photo per page, in every cell
    pub repeat_one_per_page: bool,
    pub stroke: Stroke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Background {
    White,
    Black,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InfoMode {
    Off,
    Filename,
    Title,
    Date,
    Exposure,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoInfo {
    pub mode: InfoMode,
    pub size_pt: f64,
    /// pattern for [`InfoMode::Custom`] with `{filename} {title} {date} {camera} {exposure}`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOptions {
    pub background: Background,
    pub page_numbers: bool,
    pub page_info: bool,
    pub crop_marks: bool,
    /// guides in the preview (margins, cells, sizes); never printed
    #[serde(default = "yes")]
    pub guides: bool,
    pub photo_info: PhotoInfo,
    /// print the darkroom border/film border with the photo (otherwise the page layout makes the border)
    pub darkroom_border: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Output {
    Printer,
    Pdf,
    Jpeg,
    Tiff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SharpenAmount {
    Off,
    Low,
    Standard,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Media {
    Glossy,
    Matte,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sharpen {
    pub amount: SharpenAmount,
    pub media: Media,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorMode {
    /// the page in an RGB space with embedded profile; the system/driver converts to printer and paper
    PrinterManaged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageSpace {
    AdobeRgb,
    Srgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorOptions {
    pub mode: ColorMode,
    pub space: PageSpace,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOptions {
    pub output: Output,
    /// quick: library previews instead of full development, 150 ppi, no sharpening, 8 bit
    pub draft: bool,
    pub ppi: u32,
    pub sharpen: Sharpen,
    /// 16 bits per channel (PDF/TIFF)
    pub bits16: bool,
    pub color: ColorOptions,
    #[serde(default = "jpeg_quality")]
    pub jpeg_quality: u8,
}

fn jpeg_quality() -> u8 {
    92
}

/// Print resolutions offered (pixels per inch on paper).
pub const PPI_CHOICES: &[u32] = &[240, 300, 360, 600, 720];
/// Resolution of the draft mode.
pub const DRAFT_PPI: u32 = 150;

impl JobOptions {
    /// The resolution actually used (draft: [`DRAFT_PPI`]).
    pub fn effective_ppi(&self) -> u32 {
        if self.draft {
            DRAFT_PPI
        } else {
            self.ppi.clamp(72, 1440)
        }
    }
    /// 16 bit actually used (only PDF/TIFF, never in draft mode).
    pub fn effective_bits16(&self) -> bool {
        self.bits16 && !self.draft && matches!(self.output, Output::Pdf | Output::Tiff | Output::Printer)
    }
}

impl Default for Template {
    fn default() -> Self {
        builtin()[0].clone()
    }
}

impl Template {
    fn base(name: &str, paper: &str, orientation: Orientation, margin: f64, rows: u32, columns: u32, gap: f64) -> Template {
        Template {
            format: FORMAT.into(),
            version: VERSION,
            name: name.into(),
            style: Style::Grid,
            paper: Paper::known(paper, orientation).expect("known paper"),
            printer: None,
            margins_mm: Margins::all(margin),
            grid: Grid { rows, columns, gap_mm: [gap, gap], cell_mm: [0.0, 0.0] },
            image: ImageOptions { fill: false, rotate_to_fit: true, repeat_one_per_page: false, stroke: Stroke { on: false, width_pt: 0.5, color: Ink::Black } },
            page: PageOptions {
                background: Background::White,
                page_numbers: false,
                page_info: false,
                crop_marks: false,
                guides: true,
                photo_info: PhotoInfo { mode: InfoMode::Off, size_pt: 8.0, text: String::new() },
                darkroom_border: false,
            },
            job: JobOptions {
                output: Output::Printer,
                draft: false,
                ppi: 300,
                sharpen: Sharpen { amount: SharpenAmount::Standard, media: Media::Glossy },
                bits16: false,
                color: ColorOptions { mode: ColorMode::PrinterManaged, space: PageSpace::AdobeRgb },
                jpeg_quality: 92,
            },
        }
    }

    /// Several photos per sheet, each cell fitted to its photo (not contact sheets with upright frames): the
    /// templates whose sheet orientation follows the photos.
    pub fn is_multi_photo_sheet(&self) -> bool {
        self.grid.rows * self.grid.columns > 1 && self.image.rotate_to_fit
    }

    /// Switch the sheet orientation and divide the cells anew: rows and columns, gaps and a fixed cell size swap,
    /// so a 2 × 3 grid on a portrait sheet becomes 3 × 2 on a landscape sheet. No change if already as asked.
    pub fn set_orientation(&mut self, o: Orientation) {
        if self.paper.orientation == o {
            return;
        }
        self.paper.orientation = o;
        self.swap_grid_axes();
    }

    /// Rows and columns, both gaps and both cell sizes trade places (the orientation was switched).
    pub fn swap_grid_axes(&mut self) {
        std::mem::swap(&mut self.grid.rows, &mut self.grid.columns);
        self.grid.gap_mm.swap(0, 1);
        self.grid.cell_mm.swap(0, 1);
    }

    /// Read a template file (JSON). Unknown format or newer version: error.
    pub fn from_json(text: &str) -> Result<Template, String> {
        let t: Template = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if t.format != FORMAT {
            return Err(format!("not a print template ({})", t.format));
        }
        if t.version > VERSION {
            return Err(format!("print template version {} is newer than this program ({VERSION})", t.version));
        }
        Ok(t.sanitized())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("template serialises")
    }

    /// Values brought into their ranges (a hand-edited file never breaks the layout).
    pub fn sanitized(mut self) -> Template {
        let pos = |v: f64, lo: f64, hi: f64| if v.is_finite() { v.clamp(lo, hi) } else { lo };
        self.paper.width_mm = pos(self.paper.width_mm, 20.0, 2000.0);
        self.paper.height_mm = pos(self.paper.height_mm, 20.0, 2000.0);
        for m in [&mut self.margins_mm.top, &mut self.margins_mm.bottom, &mut self.margins_mm.left, &mut self.margins_mm.right] {
            *m = pos(*m, 0.0, 500.0);
        }
        self.grid.rows = self.grid.rows.clamp(1, MAX_CELLS);
        self.grid.columns = self.grid.columns.clamp(1, MAX_CELLS);
        for g in self.grid.gap_mm.iter_mut() {
            *g = pos(*g, 0.0, 500.0);
        }
        for c in self.grid.cell_mm.iter_mut() {
            *c = pos(*c, 0.0, 2000.0);
        }
        self.image.stroke.width_pt = pos(self.image.stroke.width_pt, 0.25, 10.0);
        self.page.photo_info.size_pt = pos(self.page.photo_info.size_pt, 6.0, 16.0);
        self.job.ppi = self.job.ppi.clamp(72, 1440);
        self.job.jpeg_quality = self.job.jpeg_quality.clamp(10, 100);
        self
    }
}

/// The sheet orientation the photos ask for, from their aspect ratios (width / height): mostly landscape → landscape,
/// mostly portrait → portrait. Squares count for neither; a tie or no photo: no suggestion.
pub fn suggest_orientation(aspects: impl IntoIterator<Item = f64>) -> Option<Orientation> {
    let (mut land, mut port) = (0usize, 0usize);
    for a in aspects {
        if a > 1.02 {
            land += 1;
        } else if a > 0.0 && a < 0.98 {
            port += 1;
        }
    }
    match land.cmp(&port) {
        std::cmp::Ordering::Greater => Some(Orientation::Landscape),
        std::cmp::Ordering::Less => Some(Orientation::Portrait),
        std::cmp::Ordering::Equal => None,
    }
}

/// The built-in templates (names are translation keys of the interface).
pub fn builtin() -> Vec<Template> {
    use Orientation::*;
    let mut v = Vec::new();
    v.push(Template::base("1 Foto, A4, 10 mm Rand", "iso-a4", Portrait, 10.0, 1, 1, 0.0));
    v.push(Template::base("1 Foto, A3+, 15 mm Rand", "super-a3", Portrait, 15.0, 1, 1, 0.0));
    v.push(Template::base("1 Foto, A3, 15 mm Rand", "iso-a3", Portrait, 15.0, 1, 1, 0.0));
    let mut t = Template::base("Randlos 10 × 15", "photo-4x6", Landscape, 0.0, 1, 1, 0.0);
    t.image.fill = true;
    v.push(t);
    v.push(Template::base("13 × 18, 5 mm Rand", "photo-5x7", Portrait, 5.0, 1, 1, 0.0));
    v.push(Template::base("2 Fotos, A4", "iso-a4", Portrait, 10.0, 2, 1, 6.0));
    v.push(Template::base("4 Fotos, A4", "iso-a4", Portrait, 10.0, 2, 2, 6.0));
    let mut t = Template::base("Kontaktabzug A4 5 × 6", "iso-a4", Portrait, 10.0, 6, 5, 3.0);
    t.margins_mm.bottom = 15.0;
    t.page.photo_info = PhotoInfo { mode: InfoMode::Filename, size_pt: 6.0, text: String::new() };
    t.page.page_numbers = true;
    t.page.page_info = true;
    t.job.sharpen.amount = SharpenAmount::Low;
    // a contact sheet shows every frame upright, as taken
    t.image.rotate_to_fit = false;
    v.push(t);
    let mut t = Template::base("Kontaktabzug A3+ 6 × 8", "super-a3", Portrait, 12.0, 8, 6, 3.0);
    t.margins_mm.bottom = 18.0;
    t.page.photo_info = PhotoInfo { mode: InfoMode::Filename, size_pt: 7.0, text: String::new() };
    t.page.page_numbers = true;
    t.page.page_info = true;
    t.job.sharpen.amount = SharpenAmount::Low;
    // a contact sheet shows every frame upright, as taken
    t.image.rotate_to_fit = false;
    v.push(t);
    let mut t = Template::base("Filmstreifen A4 (Kontaktabzug)", "iso-a4", Portrait, 10.0, 7, 6, 1.5);
    t.margins_mm.bottom = 15.0;
    t.page.background = Background::Black;
    t.page.photo_info = PhotoInfo { mode: InfoMode::Filename, size_pt: 6.0, text: String::new() };
    t.page.page_numbers = true;
    t.job.sharpen.amount = SharpenAmount::Low;
    // a contact sheet shows every frame upright, as taken
    t.image.rotate_to_fit = false;
    v.push(t);
    v
}

/// Name of the built-in contact sheet in film strips (darkroom "Drucken …").
pub const FILM_STRIP_TEMPLATE: &str = "Filmstreifen A4 (Kontaktabzug)";

/// File name of a template name: lower case, letters/digits, everything else "-".
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'ä' => "ae".into(),
            'ö' => "oe".into(),
            'ü' => "ue".into(),
            'ß' => "ss".into(),
            c if c.is_alphanumeric() => c.to_string(),
            _ => "-".into(),
        };
        if c == "-" && (s.is_empty() || s.ends_with('-')) {
            continue;
        }
        s.push_str(&c);
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "template".into()
    } else {
        s
    }
}

/// Folder of the user's templates.
pub fn folder(data_dir: &Path) -> PathBuf {
    data_dir.join(FOLDER)
}

/// The user's templates (sorted by name). Unreadable files are skipped and reported in the second list.
pub fn load_all(data_dir: &Path) -> (Vec<(PathBuf, Template)>, Vec<(PathBuf, String)>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let Ok(rd) = std::fs::read_dir(folder(data_dir)) else { return (ok, bad) };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| Template::from_json(&t)) {
                Ok(t) => ok.push((p, t)),
                Err(e) => bad.push((p, e)),
            }
        }
    }
    ok.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()));
    (ok, bad)
}

/// The own template file a template named `name` would be saved to, if it exists already (same name or a name with
/// the same file name, e.g. "Druck A" and "druck-a"): [`save`] replaces it, so the caller asks first.
pub fn existing(data_dir: &Path, name: &str) -> Option<PathBuf> {
    let path = folder(data_dir).join(format!("{}.json", slug(name)));
    std::fs::symlink_metadata(&path).is_ok().then_some(path)
}

/// Save an own template under its name. Written to a side file first and then renamed, so a crash never leaves half
/// a template. An existing file of the same name is replaced ("update"): check [`existing`] first.
pub fn save(data_dir: &Path, t: &Template) -> std::io::Result<PathBuf> {
    let dir = folder(data_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", slug(&t.name)));
    let side = dir.join(format!(".{}.json.part", slug(&t.name)));
    std::fs::write(&side, t.to_json())?;
    std::fs::rename(&side, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_keys_are_english() {
        for t in builtin() {
            let j = t.to_json();
            assert_eq!(Template::from_json(&j).unwrap(), t);
        }
        let j = builtin()[7].to_json();
        for key in ["\"format\": \"graineo-print-template\"", "\"marginsMm\"", "\"gapMm\"", "\"cellMm\"", "\"rotateToFit\"", "\"repeatOnePerPage\"", "\"photoInfo\"", "\"darkroomBorder\"", "\"printerManaged\"", "\"adobeRgb\""] {
            assert!(j.contains(key), "{key} missing in {j}");
        }
    }

    #[test]
    fn foreign_or_newer_files_are_refused_and_values_clamped() {
        let mut t = builtin()[0].clone();
        t.format = "other".into();
        assert!(Template::from_json(&t.to_json()).is_err());
        let mut t = builtin()[0].clone();
        t.version = VERSION + 1;
        assert!(Template::from_json(&t.to_json()).is_err());
        let mut t = builtin()[0].clone();
        t.grid.rows = 99;
        t.margins_mm.left = -3.0;
        t.page.photo_info.size_pt = 40.0;
        let t = Template::from_json(&t.to_json()).unwrap();
        assert_eq!((t.grid.rows, t.margins_mm.left, t.page.photo_info.size_pt), (MAX_CELLS, 0.0, 16.0));
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Kontaktabzug A4 5 × 6"), "kontaktabzug-a4-5-6");
        assert_eq!(slug("Größe / Übersicht"), "groesse-uebersicht");
        assert_eq!(slug("  "), "template");
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("graineo-print-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut t = builtin()[1].clone();
        t.name = "Mein A3+".into();
        assert_eq!(existing(&dir, &t.name), None);
        let p = save(&dir, &t).unwrap();
        assert!(p.ends_with("print-templates/mein-a3.json"));
        // the same name and a name with the same file name are found (the room asks before replacing)
        assert_eq!(existing(&dir, "Mein A3+").as_deref(), Some(p.as_path()));
        assert_eq!(existing(&dir, "mein a3").as_deref(), Some(p.as_path()));
        assert_eq!(existing(&dir, "Mein A4"), None);
        std::fs::write(folder(&dir).join("broken.json"), "{").unwrap();
        let (ok, bad) = load_all(&dir);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].1, t);
        assert_eq!(bad.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orientation_swaps_cells() {
        let mut t = builtin().into_iter().find(|t| t.name == "2 Fotos, A4").unwrap();
        assert_eq!((t.grid.rows, t.grid.columns), (2, 1));
        t.set_orientation(Orientation::Landscape);
        assert_eq!((t.grid.rows, t.grid.columns), (1, 2));
        t.set_orientation(Orientation::Landscape);
        assert_eq!((t.grid.rows, t.grid.columns), (1, 2));
        t.set_orientation(Orientation::Portrait);
        assert_eq!((t.grid.rows, t.grid.columns), (2, 1));
        let mut q = builtin().into_iter().find(|t| t.name == "4 Fotos, A4").unwrap();
        q.set_orientation(Orientation::Landscape);
        assert_eq!((q.grid.rows, q.grid.columns), (2, 2));
        assert_eq!(q.paper.size_mm(), (297.0, 210.0));
        assert!(q.is_multi_photo_sheet());
        assert!(!builtin().into_iter().find(|t| t.name.starts_with("Kontaktabzug A4")).unwrap().is_multi_photo_sheet());
        assert!(!builtin()[0].is_multi_photo_sheet());
    }

    #[test]
    fn orientation_suggestion() {
        use Orientation::*;
        assert_eq!(suggest_orientation([1.5, 1.5, 0.67]), Some(Landscape));
        assert_eq!(suggest_orientation([0.67, 0.75, 1.5]), Some(Portrait));
        assert_eq!(suggest_orientation([1.5, 0.67]), None);
        assert_eq!(suggest_orientation([1.0, 1.0]), None);
        assert_eq!(suggest_orientation(Vec::<f64>::new()), None);
    }

    #[test]
    fn paper_orientation() {
        let p = Paper::known("super-a3", Orientation::Landscape).unwrap();
        assert_eq!(p.size_mm(), (483.0, 329.0));
    }
}
