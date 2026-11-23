//! "Copy photo" (⌘C, menu Edit): the selected photos onto the system clipboard, to paste them into Mail, Messages,
//! a document or the Finder. "Copy settings" stays ⇧⌘C.
//!
//! What lands on the clipboard (macOS, one pasteboard item per photo, as the Finder does it for files):
//! - **developed photos** (RAW, X3F, an edited JPEG …): what graineo shows – a JPEG of the development, long edge
//!   [`LEVEL`] px, in a temporary folder named after the photo (`DP2M5418.jpg`); the item carries the file reference
//!   (Mail, Messages and the Finder take it as an attachment or a file with this name) and the JPEG data itself
//!   (apps that only take image data, e.g. Preview "New from Clipboard"). The open photo in Develop takes the view on
//!   screen as it is (no new development, immediate).
//! - **unedited JPEG/HEIC/PNG/TIFF and videos**: the original file itself (reference only, like ⌘C in the Finder).
//!
//! Why so: a RAW file is of no use to the person receiving the mail, and the clipboard should hold what the photo
//! looks like in graineo; the file reference gives every app a real file name and the choice of attachment or
//! inline image. Developing runs in a helper thread (the user waits for it, so not at the lowest priority); the
//! status line shows the progress; a new ⌘C replaces a copy still being prepared.
//!
//! In text fields ⌘C copies text (the menu entry forwards the key). Windows: the image of the first photo goes onto
//! the clipboard as an image (egui), no file references.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::image_kind::ImageKind;
use crate::App;

/// Long edge of a developed copy (px): sharp on screens and in mails, small enough to send.
pub const LEVEL: u32 = 2048;
/// At most this many photos at once (each one may need a development).
pub const MAX: usize = 100;
const QUALITY: u8 = 90;

/// The developed view last shown in Develop: (image ID, virtual copy, width, height, RGB).
type View = (i64, Option<String>, usize, usize, Arc<Vec<u8>>);
static VIEW: Mutex<Option<View>> = Mutex::new(None);

/// The current copy job (a newer one cancels it) and its state.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static STATE: Mutex<Option<Progress>> = Mutex::new(None);

#[derive(Clone)]
enum Progress {
    Working { generation: u64, done: usize, total: usize },
    Finished { generation: u64, items: Vec<Ready>, skipped: Vec<String> },
}

/// One photo ready for the clipboard.
#[derive(Clone)]
struct Ready {
    /// file the item refers to (the original or the developed JPEG)
    file: PathBuf,
    /// JPEG data of a developed copy (also as image data on the clipboard)
    jpeg: Option<Arc<Vec<u8>>>,
}

/// Where a photo comes from.
enum Source {
    /// the original file as it is
    Original(PathBuf),
    /// the view on screen (Develop)
    View { name: String, width: usize, height: usize, rgb: Arc<Vec<u8>> },
    /// develop anew: path, settings, own settings stored?, file name
    Develop { path: PathBuf, settings: develop::Settings, stored: bool, name: String },
}

/// Remember the developed view of the open photo (called whenever a new one is shown; one copy of the pixels).
/// Returns the shared pixels (the displays at the top of the trays are computed from them, see `histogram_pin`).
pub fn remember_view(image: i64, copy: Option<String>, width: usize, height: usize, rgb: &[u8]) -> Arc<Vec<u8>> {
    let px = Arc::new(rgb.to_vec());
    *VIEW.lock().unwrap() = Some((image, copy, width, height, px.clone()));
    px
}

/// The remembered view of photo `image` (virtual copy `copy`): width, height, pixels.
pub fn view_of(image: i64, copy: Option<&str>) -> Option<(usize, usize, Arc<Vec<u8>>)> {
    let v = VIEW.lock().unwrap();
    let (i, c, w, h, px) = v.as_ref()?;
    (*i == image && c.as_deref() == copy).then(|| (*w, *h, px.clone()))
}

/// Is a label text selected (then ⌘C copies that text)?
pub fn text_selected(ctx: &egui::Context) -> bool {
    ctx.with_plugin::<egui::text_selection::LabelSelectionState, _>(|s| s.has_selection()).unwrap_or(false)
}

/// File name of the copy: name of the original without extension, a virtual copy with its name in brackets.
fn copy_name(name: &str, copy: Option<&str>) -> String {
    let stem = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let stem: String = stem.chars().map(|c| if matches!(c, '/' | ':' | '\\') { '_' } else { c }).collect();
    match copy {
        Some(c) if !c.is_empty() => format!("{stem} ({c})"),
        _ => stem,
    }
}

/// Unique file names within one copy ("A.jpg", "A-2.jpg" …).
fn unique(name: &str, used: &mut std::collections::HashSet<String>) -> String {
    let mut n = name.to_string();
    let mut k = 2;
    while !used.insert(n.to_lowercase()) {
        n = format!("{name}-{k}");
        k += 1;
    }
    n
}

/// Temporary folder of the copies; folders of earlier copies are removed (only the app's own files).
fn folder(generation: u64) -> PathBuf {
    // tests run in parallel processes (several test runs, the installed app) and share the system temp folder:
    // a folder per process keeps them from deleting each other's copies
    let base = if cfg!(test) { std::env::temp_dir().join(format!("graineo Clipboard test {}", std::process::id())) } else { std::env::temp_dir().join("graineo Clipboard") };
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            if e.file_name().to_str().and_then(|n| n.parse::<u64>().ok()).is_some_and(|g| g < generation) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    base.join(generation.to_string())
}

fn encode(img: &image::RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, QUALITY).encode_image(img).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Develop a photo for the clipboard (helper thread).
pub(crate) fn develop_one(path: &std::path::Path, settings: &develop::Settings, stored: bool) -> Result<image::RgbImage, String> {
    // a merged photo (no file of its own): its result, merged first when it is not in the cache – a copy is an output,
    // never the reference frame standing in
    if (!path.exists() || library::merge::has_merge_name(path)) && rawdev::merged::lookup(path).is_some() {
        let mut doc = rawdev::RawDoc::open_for_output(path, settings, &|| true)?.ok_or("abgebrochen")?;
        rawdev::lightroom::apply_orientation(&mut doc);
        let s = if stored { settings.clone() } else { rawdev::lightroom::initial_settings(&doc).unwrap_or_default() };
        return crate::large_preview::render_doc(&doc, &s, LEVEL);
    }
    match crate::image_kind::of(path) {
        // an unedited RAW looks as on opening (Lightroom initial values, otherwise the defaults)
        ImageKind::Raw if !stored => {
            let mut doc = rawdev::RawDoc::open(path, &|| true)?.ok_or("abgebrochen")?;
            rawdev::lightroom::apply_orientation(&mut doc);
            let s = rawdev::lightroom::initial_settings(&doc).unwrap_or_default();
            crate::large_preview::render_doc(&doc, &s, LEVEL)
        }
        _ => crate::large_preview::render(path, settings, LEVEL),
    }
}

/// Prepare all photos (helper thread); stops when a newer copy started.
fn prepare(generation: u64, sources: Vec<Source>, ctx: egui::Context) {
    let total = sources.len();
    let dir = folder(generation);
    let mut used = std::collections::HashSet::new();
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    let current = || GENERATION.load(Ordering::SeqCst) == generation;
    for (k, src) in sources.into_iter().enumerate() {
        if !current() {
            return;
        }
        *STATE.lock().unwrap() = Some(Progress::Working { generation, done: k, total });
        ctx.request_repaint();
        let (name, img) = match src {
            Source::Original(p) => {
                items.push(Ready { file: p, jpeg: None });
                continue;
            }
            Source::View { name, width, height, rgb } => (name, image::RgbImage::from_raw(width as u32, height as u32, rgb.to_vec()).ok_or_else(|| "Bildgröße passt nicht".to_string())),
            Source::Develop { path, settings, stored, name } => (name, develop_one(&path, &settings, stored)),
        };
        let written = img.and_then(|img| encode(&img)).and_then(|jpeg| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let file = dir.join(format!("{}.jpg", unique(&name, &mut used)));
            std::fs::write(&file, &jpeg).map_err(|e| e.to_string())?;
            Ok(Ready { file, jpeg: Some(Arc::new(jpeg)) })
        });
        match written {
            Ok(r) => items.push(r),
            Err(e) => {
                crate::log(&format!("copy photo {name}: {e}"));
                skipped.push(name);
            }
        }
    }
    if current() {
        *STATE.lock().unwrap() = Some(Progress::Finished { generation, items, skipped });
        ctx.request_repaint();
    }
}

/// Put the prepared photos onto the clipboard (UI thread).
#[cfg(target_os = "macos")]
fn to_clipboard(_ctx: &egui::Context, items: &[Ready]) -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::{NSArray, NSString};
    unsafe {
        let pb: Retained<AnyObject> = msg_send![class!(NSPasteboard), generalPasteboard];
        let _: isize = msg_send![&*pb, clearContents];
        let file_url = NSString::from_str("public.file-url");
        let jpeg_type = NSString::from_str("public.jpeg");
        let mut list: Vec<Retained<AnyObject>> = Vec::new();
        for it in items {
            let item: Retained<AnyObject> = msg_send![class!(NSPasteboardItem), new];
            let path = NSString::from_str(&it.file.to_string_lossy());
            let url: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*path];
            let text: Retained<NSString> = msg_send![&*url, absoluteString];
            let _: Bool = msg_send![&*item, setString: &*text, forType: &*file_url];
            if let Some(jpeg) = &it.jpeg {
                let data: Retained<AnyObject> = msg_send![class!(NSData), dataWithBytes: jpeg.as_ptr() as *const std::ffi::c_void, length: jpeg.len()];
                let _: Bool = msg_send![&*item, setData: &*data, forType: &*jpeg_type];
            }
            list.push(item);
        }
        let array = NSArray::from_retained_slice(&list);
        let ok: Bool = msg_send![&*pb, writeObjects: &*array];
        ok.as_bool()
    }
}

/// Windows and others: the image of the first photo as an image (no file references there).
#[cfg(not(target_os = "macos"))]
fn to_clipboard(ctx: &egui::Context, items: &[Ready]) -> bool {
    let Some(first) = items.first() else { return false };
    let img = match &first.jpeg {
        Some(j) => image::load_from_memory_with_format(j, image::ImageFormat::Jpeg).ok(),
        None => image::open(&first.file).ok().map(|i| i.thumbnail(LEVEL, LEVEL)),
    };
    let Some(img) = img.map(|i| i.to_rgba8()) else { return false };
    let size = [img.width() as usize, img.height() as usize];
    ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()));
    true
}

impl App {
    /// ⌘C / menu "Copy photo". In a text field ⌘C copies its text (the menu took the key, it is handed on).
    pub(crate) fn copy_photo_command(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || text_selected(ctx) {
            ctx.input_mut(|i| i.events.push(egui::Event::Copy));
            return;
        }
        let targets = self.selection();
        if targets.is_empty() {
            self.status = crate::tr("Zum Kopieren ein Foto auswählen").to_string();
            return;
        }
        if targets.len() > MAX {
            self.status = crate::trf!("Höchstens {n} Fotos auf einmal kopieren ({k} ausgewählt)", n = MAX, k = targets.len());
            return;
        }
        let open = self.current.as_ref().map(|c| c.index).filter(|_| !self.in_grid());
        let view = VIEW.lock().unwrap().clone();
        let mut sources = Vec::new();
        let mut unreachable = 0;
        for i in targets {
            self.ensure_entry(i);
            let e = &self.entries[i];
            if e.state != 0 {
                unreachable += 1;
                continue;
            }
            let name = copy_name(&e.name, e.copy.as_deref());
            // the open photo in Develop: the view on screen, if it is this photo's
            if open == Some(i) {
                if let Some((_, _, w, h, rgb)) = view.as_ref().filter(|v| v.0 == e.image && v.1 == e.copy) {
                    sources.push(Source::View { name, width: *w, height: *h, rgb: rgb.clone() });
                    continue;
                }
            }
            let kind = crate::image_kind::of(&e.path);
            let as_is = kind == ImageKind::Other || (kind == ImageKind::Done && !e.stored && e.copy.is_none());
            if as_is {
                sources.push(Source::Original(e.path.clone()));
            } else {
                sources.push(Source::Develop { path: e.path.clone(), settings: e.settings.clone(), stored: e.stored, name });
            }
        }
        if sources.is_empty() {
            self.status = crate::tr("Original nicht erreichbar – nichts kopiert").to_string();
            return;
        }
        if unreachable > 0 {
            crate::log(&format!("copy photo: {unreachable} not reachable, left out"));
        }
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let total = sources.len();
        *STATE.lock().unwrap() = Some(Progress::Working { generation, done: 0, total });
        let c = ctx.clone();
        let started = std::thread::Builder::new().name("copy-photo".into()).spawn(move || {
            library::platform::set_priority(library::platform::Priority::UserInitiated);
            prepare(generation, sources, c);
        });
        if started.is_err() {
            *STATE.lock().unwrap() = None;
        }
        self.photo_copy_frame(ctx);
    }

    /// Every frame: progress in the status line, the finished copy onto the clipboard.
    pub(crate) fn photo_copy_frame(&mut self, ctx: &egui::Context) {
        let state = STATE.lock().unwrap().clone();
        match state {
            None => {}
            Some(Progress::Working { generation, done, total }) => {
                if generation == GENERATION.load(Ordering::SeqCst) && total > 1 {
                    self.status = crate::trf!("Fotos werden für die Zwischenablage vorbereitet … {a} von {n}", a = done + 1, n = total);
                } else if generation == GENERATION.load(Ordering::SeqCst) {
                    self.status = crate::tr("Foto wird für die Zwischenablage vorbereitet …").to_string();
                }
            }
            Some(Progress::Finished { generation, items, skipped }) => {
                *STATE.lock().unwrap() = None;
                if generation != GENERATION.load(Ordering::SeqCst) {
                    return;
                }
                let ok = !items.is_empty() && to_clipboard(ctx, &items);
                let mut t = match (ok, items.len()) {
                    (false, _) => crate::tr("Foto nicht kopiert").to_string(),
                    (true, 1) => crate::trf!("{a} in der Zwischenablage", a = items[0].file.file_name().and_then(|n| n.to_str()).unwrap_or("")),
                    (true, n) => crate::trf!("{n} Fotos in der Zwischenablage", n = n),
                };
                if !cfg!(target_os = "macos") && items.len() > 1 {
                    t = crate::tr("Erstes Foto als Bild in der Zwischenablage (mehrere Fotos nur auf dem Mac)").to_string();
                }
                if !skipped.is_empty() {
                    t = crate::trf!("{t}; nicht kopiert: {k}", t = t, k = skipped.join(", "));
                }
                self.status = t;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the global job state (`GENERATION`, `STATE`): one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn names_without_extension_and_unique() {
        assert_eq!(copy_name("DP2M5418.X3F", None), "DP2M5418");
        assert_eq!(copy_name("IMG_1.CR2", Some("Kopie 1")), "IMG_1 (Kopie 1)");
        assert_eq!(copy_name("a:b.jpg", None), "a_b");
        let mut used = std::collections::HashSet::new();
        assert_eq!(unique("A", &mut used), "A");
        assert_eq!(unique("a", &mut used), "a-2");
        assert_eq!(unique("A", &mut used), "A-3");
    }

    /// A finished copy with a developed view: JPEG next to the name, data on the item.
    #[test]
    fn view_becomes_a_named_jpeg() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let rgb = Arc::new(vec![128u8; 4 * 3 * 3]);
        prepare(generation, vec![Source::View { name: "Test".into(), width: 4, height: 3, rgb }], egui::Context::default());
        let state = STATE.lock().unwrap().take();
        let Some(Progress::Finished { items, skipped, .. }) = state else { panic!("not finished") };
        assert!(skipped.is_empty());
        assert_eq!(items[0].file.file_name().unwrap(), "Test.jpg");
        assert!(items[0].file.is_file());
        assert!(items[0].jpeg.as_ref().is_some_and(|j| j.starts_with(&[0xFF, 0xD8])));
        let _ = std::fs::remove_dir_all(items[0].file.parent().unwrap().parent().unwrap());
    }
}
