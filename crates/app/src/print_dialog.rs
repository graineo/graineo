//! The system's print dialogs. macOS: page setup ("Drucker …") as a sheet on the window (`NSPageLayout`), printing
//! of the finished PDF through PDFKit (`PDFDocument.printOperation(for:scalingMode:autoRotate:)` without scaling and
//! without turning, so the millimetres of the layout stay exact), also as a sheet: neither blocks drawing.
//! Windows and Linux: no own print dialog yet; the PDF is opened in the system's viewer and printed from there.
//!
//! Safety switch: test runs (a cargo test binary, or `GRAINEO_DATA_DIR` set) never open a print dialog unless
//! `GRAINEO_PRINT_DIALOG=1`; `GRAINEO_PRINT_TO=<file.pdf>` replaces the dialog by a copy of the print PDF.

use std::path::Path;

use print::template::{Margins, Template};

/// What the page setup returned.
#[derive(Clone, Debug, PartialEq)]
pub struct PageSetup {
    /// paper in the chosen orientation, mm
    pub paper_mm: (f64, f64),
    pub landscape: bool,
    /// edges the printer cannot print on this paper
    pub unprintable: Margins,
    pub printer: String,
    pub paper_name: String,
}

/// May the system's print dialogs be opened? Never in test runs (see the module documentation).
pub fn dialogs_allowed() -> bool {
    if cfg!(test) {
        return false;
    }
    crate::env::var_os("GRAINEO_DATA_DIR").is_none() || crate::env::var("GRAINEO_PRINT_DIALOG").as_deref() == Ok("1")
}

/// Is there a page setup on this system?
pub fn page_setup_available() -> bool {
    cfg!(target_os = "macos")
}

/// Open the page setup for the template's paper (sheet on the window). false: not possible here.
pub fn begin_page_setup(t: &Template) -> bool {
    if !dialogs_allowed() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        mac::begin_page_setup(t)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = t;
        false
    }
}

/// While a page setup is open: `None`; once it closed: its result (once).
pub fn poll_page_setup() -> Option<PageSetup> {
    #[cfg(target_os = "macos")]
    {
        mac::poll_page_setup()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Is a page setup sheet open?
pub fn page_setup_open() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::page_setup_open()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Hand the finished PDF to the system's print dialog. macOS: sheet on the window, the PDF is read into memory first
/// (the file may go afterwards). Elsewhere: opened in the system's viewer.
pub fn print_pdf(pdf: &Path, t: &Template, title: &str) -> Result<(), String> {
    if !dialogs_allowed() {
        return Err("print dialog not allowed in test runs".into());
    }
    #[cfg(target_os = "macos")]
    {
        mac::print_pdf(pdf, t, title)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (t, title);
        crate::system::open(pdf);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::PageSetup;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::msg_send;
    use objc2_core_foundation::{CGRect, CGSize};
    use objc2_foundation::{NSData, NSString};
    use print::template::{Margins, Orientation, Template};
    use std::cell::{Cell, RefCell};
    use std::path::Path;

    #[link(name = "PDFKit", kind = "framework")]
    extern "C" {}

    const MM_PER_PT: f64 = 25.4 / 72.0;

    thread_local! {
        /// the print settings of this session (page setup and printing share them, main thread only)
        static INFO: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
        static SETUP_OPEN: Cell<bool> = const { Cell::new(false) };
    }

    fn class(name: &std::ffi::CStr) -> Option<&'static AnyClass> {
        AnyClass::get(name)
    }

    /// The app's window (the sheet attaches to it).
    fn window() -> Option<Retained<AnyObject>> {
        let app = class(c"NSApplication")?;
        // SAFETY: AppKit on the main thread (called from App::ui)
        unsafe {
            let shared: Option<Retained<AnyObject>> = msg_send![app, sharedApplication];
            let shared = shared?;
            let w: Option<Retained<AnyObject>> = msg_send![&*shared, keyWindow];
            w.or_else(|| msg_send![&*shared, mainWindow])
        }
    }

    fn info() -> Option<Retained<AnyObject>> {
        INFO.with(|i| {
            let mut i = i.borrow_mut();
            if i.is_none() {
                let cls = class(c"NSPrintInfo")?;
                // SAFETY: AppKit on the main thread; `copy` returns an owned object
                let shared: Option<Retained<AnyObject>> = unsafe { msg_send![cls, sharedPrintInfo] };
                let copy: Option<Retained<AnyObject>> = unsafe { msg_send![&*shared?, copy] };
                *i = copy;
            }
            i.clone()
        })
    }

    /// Paper, orientation and zero margins of the template into the print settings.
    fn apply_template(info: &AnyObject, t: &Template) {
        let (w, h) = t.paper.size_mm();
        let size = CGSize { width: w / MM_PER_PT, height: h / MM_PER_PT };
        let orientation: isize = if t.paper.orientation == Orientation::Landscape { 1 } else { 0 };
        // SAFETY: NSPrintInfo setters on the main thread
        unsafe {
            let _: () = msg_send![info, setOrientation: orientation];
            let _: () = msg_send![info, setPaperSize: size];
            let _: () = msg_send![info, setLeftMargin: 0.0f64];
            let _: () = msg_send![info, setRightMargin: 0.0f64];
            let _: () = msg_send![info, setTopMargin: 0.0f64];
            let _: () = msg_send![info, setBottomMargin: 0.0f64];
            let _: () = msg_send![info, setHorizontallyCentered: Bool::NO];
            let _: () = msg_send![info, setVerticallyCentered: Bool::NO];
        }
    }

    fn string(o: Option<Retained<AnyObject>>) -> String {
        o.and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default()
    }

    fn read(info: &AnyObject) -> PageSetup {
        // SAFETY: NSPrintInfo getters on the main thread
        unsafe {
            let size: CGSize = msg_send![info, paperSize];
            let bounds: CGRect = msg_send![info, imageablePageBounds];
            let orientation: isize = msg_send![info, orientation];
            let printer: Option<Retained<AnyObject>> = msg_send![info, printer];
            let printer_name = string(printer.and_then(|p| msg_send![&*p, name]));
            let paper_name = string(msg_send![info, paperName]);
            let (pw, ph) = (size.width * MM_PER_PT, size.height * MM_PER_PT);
            // Cocoa: origin bottom left
            let left = bounds.origin.x * MM_PER_PT;
            let bottom = bounds.origin.y * MM_PER_PT;
            let right = pw - left - bounds.size.width * MM_PER_PT;
            let top = ph - bottom - bounds.size.height * MM_PER_PT;
            let m = |v: f64| (v.max(0.0) * 100.0).round() / 100.0;
            PageSetup { paper_mm: (pw, ph), landscape: orientation == 1, unprintable: Margins { top: m(top), bottom: m(bottom), left: m(left), right: m(right) }, printer: printer_name, paper_name }
        }
    }

    pub fn begin_page_setup(t: &Template) -> bool {
        let (Some(info), Some(win)) = (info(), window()) else { return false };
        apply_template(&info, t);
        let Some(cls) = class(c"NSPageLayout") else { return false };
        // SAFETY: AppKit on the main thread; the sheet runs modal to the window only, drawing goes on
        unsafe {
            let pl: Option<Retained<AnyObject>> = msg_send![cls, pageLayout];
            let Some(pl) = pl else { return false };
            let none: Option<Sel> = None;
            let _: () = msg_send![&*pl, beginSheetWithPrintInfo: &*info, modalForWindow: &*win, delegate: std::ptr::null::<AnyObject>(), didEndSelector: none, contextInfo: std::ptr::null_mut::<std::ffi::c_void>()];
        }
        SETUP_OPEN.with(|o| o.set(true));
        true
    }

    pub fn page_setup_open() -> bool {
        SETUP_OPEN.with(|o| o.get())
    }

    pub fn poll_page_setup() -> Option<PageSetup> {
        if !SETUP_OPEN.with(|o| o.get()) {
            return None;
        }
        let win = window();
        // SAFETY: AppKit on the main thread
        let sheet: Option<Retained<AnyObject>> = win.as_ref().and_then(|w| unsafe { msg_send![&**w, attachedSheet] });
        if sheet.is_some() {
            return None;
        }
        SETUP_OPEN.with(|o| o.set(false));
        info().map(|i| read(&i))
    }

    pub fn print_pdf(pdf: &Path, t: &Template, title: &str) -> Result<(), String> {
        let bytes = std::fs::read(pdf).map_err(|e| e.to_string())?;
        let info = info().ok_or("NSPrintInfo")?;
        let win = window().ok_or("no window for the print dialog")?;
        apply_template(&info, t);
        let cls = class(c"PDFDocument").ok_or("PDFKit not available")?;
        let data = NSData::with_bytes(&bytes);
        drop(bytes);
        // SAFETY: AppKit/PDFKit on the main thread; the operation runs as a sheet (no blocking of the drawing)
        unsafe {
            let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
            let doc: Option<Retained<AnyObject>> = msg_send![alloc, initWithData: &*data];
            let doc = doc.ok_or("PDF not readable")?;
            // kPDFPrintPageScaleNone = 0: 100 %, autoRotate NO: the page as laid out
            let op: Option<Retained<AnyObject>> = msg_send![&*doc, printOperationForPrintInfo: &*info, scalingMode: 0isize, autoRotate: Bool::NO];
            let op = op.ok_or("no print operation")?;
            let _: () = msg_send![&*op, setShowsPrintPanel: Bool::YES];
            let _: () = msg_send![&*op, setShowsProgressPanel: Bool::YES];
            let _: () = msg_send![&*op, setJobTitle: &*NSString::from_str(title)];
            let none: Option<Sel> = None;
            let _: () = msg_send![&*op, runOperationModalForWindow: &*win, delegate: std::ptr::null::<AnyObject>(), didRunSelector: none, contextInfo: std::ptr::null_mut::<std::ffi::c_void>()];
        }
        Ok(())
    }
}
