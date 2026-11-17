//! Print job: renders every page of a print layout in full quality and writes it as PDF, JPEG or TIFF. Each photo is
//! developed once through the export's rendering ([`crate::jobs::render_for_output`]), cut and scaled to its cells
//! (Lanczos3, also upwards), output-sharpened and placed into the page raster, which is written band by band
//! ([`print::output`]). One photo at a time (memory, fans), on its own thread with the export's priority; the
//! interface only polls the progress and can cancel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use develop::{Rgb16, Settings};
use eframe::egui;
use print::layout::{layout, PhotoSpec};
use print::raster::{prepare_cell, targets, CellImage, PageRaster};
use print::template::{Output, PageSpace, Template};
use print::Image16;

/// A photo of the job.
#[derive(Clone)]
pub(crate) struct JobPhoto {
    pub path: PathBuf,
    pub settings: Settings,
    pub image: i64,
    /// shown name (messages)
    pub name: String,
    /// size and caption as laid out in the preview (the size is corrected after developing)
    pub spec: PhotoSpec,
    /// key in the library's preview cache (draft mode)
    pub preview_key: Option<u64>,
}

/// Where the pages go.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Destination {
    /// a file chosen by the user (PDF: all pages; JPEG/TIFF: one file per page, further pages get "-2", "-3" …)
    File(PathBuf),
    /// a PDF in the temp folder, handed to the system's print dialog afterwards
    Printer(PathBuf),
}

pub(crate) struct Request {
    pub template: Template,
    pub photos: Vec<JobPhoto>,
    pub unprintable: Option<print::template::Margins>,
    pub page_info: String,
    pub destination: Destination,
    pub previews: Option<Arc<library::preview::Cache>>,
}

pub(crate) enum Update {
    /// page `page` (from 1) of `pages` is being prepared: photo `photo` of `photos` on it
    Progress { page: usize, pages: usize },
    Done(Result<Outcome, String>),
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub files: Vec<PathBuf>,
    pub pages: usize,
    pub seconds: f64,
    /// for the printer: the PDF to hand to the print dialog
    pub printer_pdf: Option<PathBuf>,
    /// the chosen file existed already: it stays untouched, the page went to a free name ("name-2.pdf")
    pub kept_existing: Option<PathBuf>,
}

/// A running job.
pub(crate) struct Running {
    rx: Receiver<Update>,
    pub cancel: Arc<AtomicBool>,
    /// last progress: (page, pages)
    pub progress: (usize, usize),
}

impl Running {
    /// New messages; `Some` once the job is over.
    pub fn poll(&mut self) -> Option<Result<Outcome, String>> {
        for u in self.rx.try_iter() {
            match u {
                Update::Progress { page, pages } => self.progress = (page, pages),
                Update::Done(r) => return Some(r),
            }
        }
        None
    }
}

/// Start the job on its own thread.
pub(crate) fn start(req: Request, ctx: egui::Context) -> Running {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let pages = layout(&req.template, &req.photos.iter().map(|p| p.spec.clone()).collect::<Vec<_>>(), req.unprintable, &req.page_info).pages.len();
    std::thread::Builder::new()
        .name("print".into())
        .spawn(move || {
            library::platform::set_priority(library::platform::Priority::UserInitiated);
            let send = |u: Update| {
                let _ = tx.send(u);
                ctx.request_repaint();
            };
            let r = run(req, &c, &|page, pages| send(Update::Progress { page, pages }));
            send(Update::Done(r));
        })
        .expect("print thread");
    Running { rx, cancel, progress: (0, pages) }
}

/// Colour space of the page.
pub(crate) fn space(t: &Template) -> graineo_color::Space {
    match t.job.color.space {
        PageSpace::AdobeRgb => graineo_color::Space::AdobeRgb,
        PageSpace::Srgb => graineo_color::Space::Srgb,
    }
}

fn cancelled() -> String {
    crate::tr("abgebrochen").into()
}

/// The whole job (also called directly by the tests).
pub(crate) fn run(req: Request, cancel: &AtomicBool, progress: &dyn Fn(usize, usize)) -> Result<Outcome, String> {
    let t0 = std::time::Instant::now();
    let t = &req.template;
    let ppi = t.job.effective_ppi() as f64;
    let bits16 = t.job.effective_bits16();
    let space = space(t);
    let icc = space.icc().map_err(|e| format!("ICC: {e}"))?;
    let mut specs: Vec<PhotoSpec> = req.photos.iter().map(|p| p.spec.clone()).collect();
    let first = layout(t, &specs, req.unprintable, &req.page_info);
    let pages = first.pages.len();
    if pages == 0 {
        return Err(crate::tr("Keine Seite zu drucken").into());
    }
    // a page that is too large for the chosen resolution would exhaust the memory: refuse it before anything is rendered
    if let Err(e) = print::raster::check_size(&first, ppi) {
        return Err(crate::trf!(
            "Seite zu groß zum Drucken: {w} × {h} Pixel bei {ppi} ppi, höchstens {max} MP und {edge} Pixel je Seite. Bitte kleineres Papier oder weniger ppi wählen.",
            w = e.width,
            h = e.height,
            ppi = ppi.round(),
            max = print::raster::MAX_PAGE_PIXELS / 1_000_000,
            edge = print::raster::MAX_EDGE
        ));
    }
    let output = match &req.destination {
        Destination::Printer(_) => Output::Pdf,
        Destination::File(_) => match t.job.output {
            Output::Printer => Output::Pdf,
            o => o,
        },
    };
    let target = match &req.destination {
        Destination::File(p) | Destination::Printer(p) => p.clone(),
    };
    // never into an Apple Photos library or another program's package
    library::write_target::check(&target).map_err(|e| crate::menu::i18n::tr_msg(&e).into_owned())?;
    // never over an existing file (as the export): the chosen file may be an original or another photo, even when the
    // save dialog asked "Replace?". A file actually written under the chosen name gets a free one ("name-2.pdf").
    let writes_target = matches!(req.destination, Destination::File(_)) && (output == Output::Pdf || pages == 1);
    let chosen = target.clone();
    let target = if writes_target { crate::jobs::free_target_name(&target)? } else { target };
    let kept_existing = (target != chosen).then_some(chosen);
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| crate::trf!("Zielordner: {e}", e = e))?;
    }
    let db = library::db::Db::standard().ok();
    let mut files = Vec::new();
    let mut pdf: Option<print::output::PdfWriter<std::io::BufWriter<std::fs::File>>> = None;
    let pdf_side = target.with_file_name(format!(".{}.part", target.file_name().unwrap_or_default().to_string_lossy()));
    if output == Output::Pdf {
        let f = std::fs::File::create(&pdf_side).map_err(|e| crate::trf!("Zieldatei: {e}", e = e))?;
        pdf = Some(print::output::PdfWriter::new(std::io::BufWriter::new(f), &icc).map_err(|e| e.to_string())?);
    }
    let result = (|| -> Result<(), String> {
        for page in 0..pages {
            if cancel.load(Ordering::SeqCst) {
                return Err(cancelled());
            }
            progress(page + 1, pages);
            let cells = page_cells(&req, &mut specs, page, ppi, space, cancel, db.as_ref())?;
            let l = layout(t, &specs, req.unprintable, &req.page_info);
            let raster = PageRaster::new(&l, page, ppi, cells);
            match output {
                Output::Pdf | Output::Printer => {
                    pdf.as_mut().expect("pdf writer").page(&raster, l.width_mm, l.height_mm, bits16).map_err(|e| e.to_string())?;
                }
                Output::Jpeg | Output::Tiff => {
                    let path = page_file(&target, page, pages)?;
                    let ppi_u = ppi.round() as u32;
                    print::output::atomic_file(&path, |w| match output {
                        Output::Jpeg => print::output::write_jpeg(w, &raster, &icc, ppi_u, t.job.jpeg_quality),
                        _ => print::output::write_tiff(w, &raster, &icc, ppi_u, bits16),
                    })
                    .map_err(|e| crate::trf!("Zieldatei: {e}", e = e))?;
                    files.push(path);
                }
            }
        }
        Ok(())
    })();
    if let Some(w) = pdf {
        let finished = result.as_ref().ok().map(|_| w.finish().map_err(|e| e.to_string()).and_then(|b| {
            let f = b.into_inner().map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())
        }));
        match finished {
            Some(Ok(())) => {
                std::fs::rename(&pdf_side, &target).map_err(|e| crate::trf!("Zieldatei: {e}", e = e))?;
                files.push(target.clone());
            }
            Some(Err(e)) => {
                let _ = std::fs::remove_file(&pdf_side);
                return Err(e);
            }
            None => {
                let _ = std::fs::remove_file(&pdf_side);
            }
        }
    }
    result?;
    let printer_pdf = matches!(req.destination, Destination::Printer(_)).then(|| target.clone());
    Ok(Outcome { files, pages, seconds: t0.elapsed().as_secs_f64(), printer_pdf, kept_existing })
}

/// File of page `page` for JPEG/TIFF: the chosen name for a single page (already made free by [`run`]), otherwise
/// "name-1", "name-2" … (never over an existing file).
fn page_file(target: &Path, page: usize, pages: usize) -> Result<PathBuf, String> {
    if pages == 1 {
        return Ok(target.to_path_buf());
    }
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = target.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    crate::jobs::free_target_name(&target.with_file_name(format!("{stem}-{}{ext}", page + 1)))
}

/// The photos of page `page`, scaled to their cells. Each photo is developed once (also with several cells on the
/// page); its true size replaces the estimate in `specs`, so fitting uses the developed image's shape.
fn page_cells(req: &Request, specs: &mut [PhotoSpec], page: usize, ppi: f64, space: graineo_color::Space, cancel: &AtomicBool, db: Option<&library::db::Db>) -> Result<Vec<CellImage>, String> {
    let t = &req.template;
    let photos_on_page: Vec<usize> = {
        let l = layout(t, specs, req.unprintable, &req.page_info);
        let mut v: Vec<usize> = l.pages[page].photos.iter().map(|p| p.photo).collect();
        v.dedup();
        v
    };
    let mut cells = Vec::new();
    for i in photos_on_page {
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        let p = &req.photos[i];
        let src = if t.job.draft { draft_source(req, p, space)? } else { full_source(p, t, space, db)? };
        specs[i].width = src.width as u32;
        specs[i].height = src.height as u32;
        let l = layout(t, specs, req.unprintable, &req.page_info);
        for (k, rect) in targets(&l, page, ppi) {
            let placed = &l.pages[page].photos[k];
            if placed.photo != i {
                continue;
            }
            let (w, h) = ((rect[2] - rect[0]).max(1) as usize, (rect[3] - rect[1]).max(1) as usize);
            let mut img = prepare_cell(&src, placed, w, h, &lanczos);
            if !t.job.draft {
                print::sharpen::apply(&mut img, ppi, t.job.sharpen);
            }
            cells.push(CellImage { rect, img });
        }
    }
    Ok(cells)
}

fn lanczos(img: &Image16, w: usize, h: usize) -> Image16 {
    let src = Rgb16 { width: img.width, height: img.height, data: img.data.clone() };
    let r = develop::resize::lanczos3_cpu(&src, w, h);
    Image16 { width: r.width, height: r.height, data: r.data }
}

/// The photo developed in full resolution through the export's rendering.
fn full_source(p: &JobPhoto, t: &Template, space: graineo_color::Space, db: Option<&library::db::Db>) -> Result<Image16, String> {
    let mut s = p.settings.clone();
    s.output_size = None;
    crate::tone_eq_ui::without_mask(&mut s);
    let fail = |e: String| crate::trf!("{name}: {e}", name = p.name, e = e);
    let src = crate::jobs::output_source(&p.path, s, p.image, db, t.page.darkroom_border, false).map_err(fail)?;
    let img = crate::jobs::render_for_output(&src, develop::export::Engine::Own, space).map_err(fail)?;
    if img.width == 0 || img.height == 0 || img.data.len() != img.width * img.height * 3 {
        return Err(fail(crate::tr("leeres Bild").into()));
    }
    Ok(Image16 { width: img.width, height: img.height, data: img.data })
}

/// Draft: the library preview (developed look, sRGB) instead of the full development.
fn draft_source(req: &Request, p: &JobPhoto, space: graineo_color::Space) -> Result<Image16, String> {
    let cache = req.previews.as_ref();
    let img = p.preview_key.and_then(|k| cache.and_then(|c| [library::preview::large(), library::preview::small()].into_iter().find_map(|lv| c.read(k, lv))));
    let Some(img) = img else {
        return Err(crate::trf!("{name}: keine Vorschau für den Entwurf", name = p.name));
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data: Vec<u16> = img.into_raw().into_iter().map(|v| v as u16 * 257).collect();
    let data = graineo_color::transcode_srgb16(&data, space)?;
    Ok(Image16 { width: w, height: h, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use print::template::builtin;

    fn photos(dir: &Path) -> Vec<JobPhoto> {
        [(1800u32, 1200u32), (1200, 1800), (1500, 1500)]
            .into_iter()
            .enumerate()
            .map(|(n, (w, h))| {
                let path = dir.join(format!("photo{n}.png"));
                image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 60 + 60 * n as u8])).save(&path).unwrap();
                JobPhoto { path, settings: Settings::default(), image: 0, name: format!("photo{n}.png"), spec: PhotoSpec { width: w, height: h, caption: format!("photo{n}.png"), offset: [0.0; 2] }, preview_key: None }
            })
            .collect()
    }

    fn pdfinfo(path: &Path) -> Option<String> {
        let out = std::process::Command::new("pdfinfo").arg(path).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Full path without a printer: three PNGs developed in full, A4 contact sheet and A3+ single pages as PDF,
    /// JPEG and TIFF; page size in pt, pixel size, profile and resolution checked.
    #[test]
    fn print_to_files() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-print-job-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ph = photos(&dir);
        let never = AtomicBool::new(false);

        // contact sheet A4, PDF 16 bit, 150 ppi (quick)
        let mut t = builtin().into_iter().find(|t| t.name == "Kontaktabzug A4 5 × 6").unwrap();
        t.job.ppi = 150;
        t.job.bits16 = true;
        t.job.output = Output::Pdf;
        let pdf = dir.join("sheet.pdf");
        let r = run(Request { template: t.clone(), photos: ph.clone(), unprintable: None, page_info: "test".into(), destination: Destination::File(pdf.clone()), previews: None }, &never, &|_, _| {}).unwrap();
        assert_eq!((r.pages, r.files.len()), (1, 1));
        let bytes = std::fs::read(&pdf).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/MediaBox [0 0 595.2756 841.8898]"));
        assert!(text.contains("/Width 1240 /Height 1754"), "A4 at 150 ppi");
        assert!(text.contains("/BitsPerComponent 16"));
        let adobe = graineo_color::Space::AdobeRgb.icc().unwrap();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &adobe).unwrap();
        let packed = z.finish().unwrap();
        assert!(bytes.windows(64).any(|w| w == &packed[..64]), "Adobe RGB profile embedded");
        if let Some(info) = pdfinfo(&pdf) {
            assert!(info.lines().any(|l| l.starts_with("Pages:") && l.trim_end().ends_with(" 1")), "{info}");
            assert!(info.contains("595.276 x 841.89 pts"), "{info}");
        }

        // A3+ one photo per page, three pages, TIFF 8 bit at 100 ppi, sRGB
        let mut t = builtin().into_iter().find(|t| t.name == "1 Foto, A3+, 15 mm Rand").unwrap();
        t.job.ppi = 100;
        t.job.output = Output::Tiff;
        t.job.color.space = PageSpace::Srgb;
        let r = run(Request { template: t.clone(), photos: ph.clone(), unprintable: None, page_info: String::new(), destination: Destination::File(dir.join("a3.tif")), previews: None }, &never, &|_, _| {}).unwrap();
        assert_eq!(r.files.len(), 3);
        assert!(r.files[0].ends_with("a3-1.tif") && r.files[2].ends_with("a3-3.tif"));
        let img = image::open(&r.files[1]).unwrap();
        assert_eq!((img.width(), img.height()), (print::layout::px(329.0, 100.0) as u32, print::layout::px(483.0, 100.0) as u32));

        // JPEG: single page keeps the chosen name
        t.job.output = Output::Jpeg;
        let r = run(Request { template: t.clone(), photos: ph[..1].to_vec(), unprintable: None, page_info: String::new(), destination: Destination::File(dir.join("one.jpg")), previews: None }, &never, &|_, _| {}).unwrap();
        assert_eq!(r.files, vec![dir.join("one.jpg")]);
        assert_eq!(r.kept_existing, None);

        // an existing file (here a photo in its folder) is never replaced, neither by JPEG nor by PDF: the page gets a
        // free name and the file stays byte for byte
        let original = dir.join("IMG_0001.jpg");
        std::fs::write(&original, b"camera original").unwrap();
        let r = run(Request { template: t.clone(), photos: ph[..1].to_vec(), unprintable: None, page_info: String::new(), destination: Destination::File(original.clone()), previews: None }, &never, &|_, _| {}).unwrap();
        assert_eq!(r.files, vec![dir.join("IMG_0001-2.jpg")]);
        assert_eq!(r.kept_existing.as_deref(), Some(original.as_path()));
        assert_eq!(std::fs::read(&original).unwrap(), b"camera original");
        let mut tp = t.clone();
        tp.job.output = Output::Pdf;
        let r = run(Request { template: tp, photos: ph[..1].to_vec(), unprintable: None, page_info: String::new(), destination: Destination::File(pdf.clone()), previews: None }, &never, &|_, _| {}).unwrap();
        assert_eq!(r.files, vec![dir.join("sheet-2.pdf")]);
        assert_eq!(std::fs::read(&pdf).unwrap(), bytes, "earlier PDF unchanged");

        // cancelled: no file, no side file left behind
        let stop = AtomicBool::new(true);
        let e = run(Request { template: builtin()[0].clone(), photos: ph.clone(), unprintable: None, page_info: String::new(), destination: Destination::File(dir.join("never.pdf")), previews: None }, &stop, &|_, _| {}).unwrap_err();
        assert_eq!(e, cancelled());
        assert!(!dir.join("never.pdf").exists() && !dir.join(".never.pdf.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The photo lands on its exact pixels: a 100 × 100 mm cell at 300 ppi is 1181 pixels wide, and the developed
    /// photo fills it edge to edge (no white seam, no overhang).
    #[test]
    fn cell_is_exact_in_millimetres() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-print-mm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("square.png");
        image::RgbImage::from_pixel(1000, 1000, image::Rgb([20, 20, 20])).save(&path).unwrap();
        let mut t = builtin()[0].clone();
        t.paper = print::template::Paper { id: "custom".into(), width_mm: 150.0, height_mm: 150.0, orientation: print::template::Orientation::Portrait };
        t.margins_mm = print::template::Margins::all(25.0);
        t.job.output = Output::Tiff;
        t.job.sharpen.amount = print::template::SharpenAmount::Off;
        t.job.color.space = PageSpace::Srgb;
        let p = JobPhoto { path, settings: Settings::default(), image: 0, name: "square".into(), spec: PhotoSpec { width: 1000, height: 1000, caption: String::new(), offset: [0.0; 2] }, preview_key: None };
        let out = dir.join("mm.tif");
        run(Request { template: t, photos: vec![p], unprintable: None, page_info: String::new(), destination: Destination::File(out.clone()), previews: None }, &AtomicBool::new(false), &|_, _| {}).unwrap();
        let img = image::open(&out).unwrap().to_rgb8();
        let (x0, x1) = (print::layout::px(25.0, 300.0) as u32, print::layout::px(125.0, 300.0) as u32);
        assert_eq!(x1 - x0, 1181);
        let y = img.height() / 2;
        assert_eq!(img.get_pixel(x0 - 1, y).0, [255; 3]);
        assert!(img.get_pixel(x0, y).0[0] < 40);
        assert!(img.get_pixel(x1 - 1, y).0[0] < 40);
        assert_eq!(img.get_pixel(x1, y).0, [255; 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Measurement (not a test): `GRAINEO_PRINT_MEASURE_DIR` with photos → A3+ at 360 ppi, one photo per page, PDF.
    #[test]
    #[ignore]
    fn measure_print() {
        let Some(dir) = crate::env::var_os("GRAINEO_PRINT_MEASURE_DIR") else { return };
        let dir = PathBuf::from(dir);
        let mut photos = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = e.path();
            if !crate::image_kind::of(&path).editable() {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            photos.push(JobPhoto { path, settings: Settings::default(), image: 0, name: name.clone(), spec: PhotoSpec { width: 3000, height: 2000, caption: name, offset: [0.0; 2] }, preview_key: None });
        }
        photos.sort_by(|a, b| a.name.cmp(&b.name));
        let mut t = builtin().into_iter().find(|t| t.name == "1 Foto, A3+, 15 mm Rand").unwrap();
        t.job.ppi = 360;
        t.job.output = Output::Pdf;
        t.page.photo_info.mode = print::template::InfoMode::Filename;
        let out = std::env::temp_dir().join("graineo-print-measure.pdf");
        let t0 = std::time::Instant::now();
        let r = run(Request { template: t, photos, unprintable: None, page_info: String::new(), destination: Destination::File(out.clone()), previews: None }, &AtomicBool::new(false), &|p, n| eprintln!("MEASURE page {p}/{n} after {:.1?}", t0.elapsed())).unwrap();
        eprintln!("MEASURE {} pages in {:.1} s → {}", r.pages, r.seconds, out.display());
    }
}
