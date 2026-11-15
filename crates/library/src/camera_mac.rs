//! Cameras connected by USB without a drive (PTP/MTP) through ImageCaptureCore (macOS): device browser, session,
//! catalog, thumbnails, partial reads and downloads as an import [`Source`]. Used by the import window of the app and
//! by `graineo-dev camera` (diagnosis with a real camera).
//!
//! The browser and its delegate live on the main thread; requests are started on the main queue and answered by
//! blocks on any queue, the helper threads wait for them. `GRAINEO_CAMERA_LOG=1` writes every step to stderr.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use crate::import_source::{self as isrc, Source, SourceFile};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, Message};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSData, NSDate, NSDictionary, NSError, NSNumber, NSProgress, NSString, NSURL};
use objc2_image_capture_core::{
    ICCameraDevice, ICCameraDeviceDelegate, ICCameraFile, ICCameraItem, ICDevice, ICDeviceBrowser, ICDeviceBrowserDelegate, ICDeviceDelegate, ICDeviceLocationTypeMask, ICDeviceTypeMask, ICDownloadsDirectoryURL,
    ICImageSourceThumbnailMaxPixelSize, ICSaveAsFilename, ICTransportTypeMassStorage,
};

use crate::import_source::{CamState, Row};

/// Every step to stderr (`GRAINEO_CAMERA_LOG=1`; the diagnosis command switches it on).
pub fn verbose() -> bool {
    std::env::var_os("GRAINEO_CAMERA_LOG").is_some()
}

fn log(m: &str) {
    if verbose() {
        let th = std::thread::current();
        let name = th.name().map(str::to_string).unwrap_or_else(|| if objc2::MainThreadMarker::new().is_some() { "main".into() } else { "?".into() });
        eprintln!("kamera {:>8.3} [{name}] {m}", started().elapsed().as_secs_f64());
    }
}

fn started() -> &'static Instant {
    static T: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T.get_or_init(Instant::now)
}

/// Let the camera fetch thumbnails and metadata of every file itself after the catalog (`GRAINEO_CAMERA_AUTOTHUMBS=1`;
/// normally off: thousands of requests, the window asks for the visible ones).
fn auto_items() -> bool {
    std::env::var_os("GRAINEO_CAMERA_AUTOTHUMBS").is_some()
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    fn CFRunLoopRunInMode(mode: *const std::ffi::c_void, seconds: f64, return_after_source_handled: u8) -> i32;
}

/// Run the main run loop for `secs` (a command-line program without a window: the browser's messages and the requests
/// started on the main queue are handled there). Only on the main thread.
pub fn pump(secs: f64) {
    // SAFETY: plain CoreFoundation call with the constant default mode
    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, secs, 0) };
}

/// ImageCaptureCore objects handed to the helper threads. There they are only used through the block requests
/// (Apple delivers the answers on any queue) and plain property reads; the requests are started on the main queue.
struct Obj<T>(T);
// SAFETY: see above – the objects are reference counted atomically, the requests go through the main queue
unsafe impl<T> Send for Obj<T> {}
unsafe impl<T> Sync for Obj<T> {}

struct Entry {
    id: String,
    name: String,
    device: Option<Obj<Retained<ICCameraDevice>>>,
    state: CamState,
    /// chosen as source: the session is wanted (and opened again when the camera comes back)
    wanted: bool,
    catalog: Vec<SourceFile>,
    files: HashMap<String, Obj<Retained<ICCameraFile>>>,
    /// the camera reported its catalog complete for the open session (only then does the list count)
    catalog_done: bool,
    /// files the camera announced (`didAddItems`) before its catalog was complete: taken in as well, in case
    /// `mediaFiles` is still empty at that moment
    announced: Vec<(SourceFile, Obj<Retained<ICCameraFile>>)>,
    /// an empty complete list: when it was last read again, and how often
    empty_check: Option<(Instant, u32)>,
    /// close and open the session again ("Neu einlesen")
    reopen: bool,
    /// automatic reopenings for an empty list (at most one per choice)
    auto_reopened: bool,
    /// the storages were asked for (empty list: is there a memory card at all?)
    storages_asked: bool,
}

impl Entry {
    fn new(id: String, name: String, device: Option<Obj<Retained<ICCameraDevice>>>, state: CamState, wanted: bool) -> Entry {
        Entry { id, name, device, state, wanted, catalog: Vec::new(), files: HashMap::new(), catalog_done: false, announced: Vec::new(), empty_check: None, reopen: false, auto_reopened: false, storages_asked: false }
    }

    /// Forget everything of the session (closed, pulled).
    fn reset(&mut self) {
        self.catalog.clear();
        self.files.clear();
        self.catalog_done = false;
        self.announced.clear();
        self.empty_check = None;
        self.storages_asked = false;
    }
}

pub struct Shared {
    entries: Mutex<Vec<Entry>>,
    /// files whose thumbnail was asked for: the camera fetches only these (answering "no" for a file to
    /// `shouldGetThumbnailOfItem` also refuses an explicit request, error -20098; "yes" for all fetches every thumbnail)
    thumbs_wanted: Mutex<std::collections::HashSet<usize>>,
    /// draw the window again (state changed)
    repaint: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn repaint(&self) {
        (self.repaint)();
    }

    fn with<R>(&self, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
        f(&mut self.entries.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

pub struct Ivars {
    shared: Arc<Shared>,
}

define_class!(
    // SAFETY: NSObject has no special requirements for subclassing; no Drop, the ivars are plain Rust data
    #[unsafe(super(NSObject))]
    #[name = "GraineoImportCameraDelegate"]
    #[ivars = Ivars]
    pub struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl ICDeviceBrowserDelegate for Delegate {
        #[unsafe(method(deviceBrowser:didAddDevice:moreComing:))]
        fn did_add_device(&self, _browser: &ICDeviceBrowser, device: &ICDevice, _more: bool) {
            self.added(device);
        }

        #[unsafe(method(deviceBrowser:didRemoveDevice:moreGoing:))]
        fn did_remove_device(&self, _browser: &ICDeviceBrowser, device: &ICDevice, _more: bool) {
            self.removed(device);
        }
    }

    unsafe impl ICDeviceDelegate for Delegate {
        #[unsafe(method(device:didCloseSessionWithError:))]
        fn did_close(&self, device: &ICDevice, _error: Option<&NSError>) {
            let id = device_id(device);
            log("Sitzung geschlossen");
            let reopen = self.ivars().shared.with(|v| v.iter_mut().find(|e| e.id == id).map(|e| std::mem::take(&mut e.reopen)).unwrap_or(false));
            if reopen {
                self.ivars().shared.with(|v| {
                    if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                        e.reset();
                    }
                });
                self.open_session(&id);
                return;
            }
            self.ivars().shared.with(|v| {
                if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                    e.reset();
                    if e.device.is_some() {
                        e.state = CamState::Found;
                    }
                }
            });
            self.ivars().shared.repaint();
        }

        #[unsafe(method(didRemoveDevice:))]
        fn did_remove(&self, device: &ICDevice) {
            self.removed(device);
        }

        #[unsafe(method(device:didOpenSessionWithError:))]
        fn did_open(&self, device: &ICDevice, error: Option<&NSError>) {
            let id = device_id(device);
            let restricted = camera_of(device).is_some_and(|c| unsafe { c.isAccessRestrictedAppleDevice() || c.isLocked() });
            log(&format!("Sitzung geöffnet: {}", error.map(|e| e.localizedDescription().to_string()).unwrap_or_else(|| "ok".into())));
            let state = match error {
                None => CamState::Catalog(0),
                Some(_) if restricted => CamState::Restricted,
                Some(e) => CamState::Failed(e.localizedDescription().to_string()),
            };
            self.ivars().shared.with(|v| {
                if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                    e.state = state;
                }
            });
            self.ivars().shared.repaint();
        }

        #[unsafe(method(device:didEncounterError:))]
        fn did_encounter_error(&self, device: &ICDevice, error: Option<&NSError>) {
            log(&format!("import: Kamera {}: {}", device_id(device), error.map(|e| e.localizedDescription().to_string()).unwrap_or_default()));
        }
    }

    unsafe impl ICCameraDeviceDelegate for Delegate {
        #[unsafe(method(cameraDevice:didAddItems:))]
        fn did_add_items(&self, camera: &ICCameraDevice, items: &NSArray<ICCameraItem>) {
            // photos taken while connected: only once the catalog is complete (before, the catalog brings them)
            let id = device_id(camera);
            let ready = self.ivars().shared.with(|v| v.iter().any(|e| e.id == id && matches!(e.state, CamState::Ready(_))));
            log(&format!("didAddItems: {} (Katalog fertig: {ready})", items.count()));
            let new = files_of(items);
            self.ivars().shared.with(|v| {
                if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                    if ready {
                        add_files(e, new);
                        e.state = CamState::Ready(e.catalog.len());
                    } else {
                        // kept until the catalog is complete (its list may still be empty then)
                        e.announced.extend(new);
                    }
                }
            });
            self.ivars().shared.repaint();
        }

        #[unsafe(method(cameraDevice:didRemoveItems:))]
        fn did_remove_items(&self, camera: &ICCameraDevice, items: &NSArray<ICCameraItem>) {
            let id = device_id(camera);
            let gone: Vec<String> = files_of(items).into_iter().map(|(f, _)| f.rel).collect();
            self.ivars().shared.with(|v| {
                if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                    e.catalog.retain(|f| !gone.contains(&f.rel));
                    for g in &gone {
                        e.files.remove(g);
                    }
                    if matches!(e.state, CamState::Ready(_)) {
                        e.state = CamState::Ready(e.catalog.len());
                    }
                }
            });
        }

        #[unsafe(method(cameraDevice:didReceiveThumbnail:forItem:error:))]
        fn did_receive_thumbnail(&self, _camera: &ICCameraDevice, _thumbnail: Option<&CGImage>, _item: &ICCameraItem, _error: Option<&NSError>) {}

        #[unsafe(method(cameraDevice:didReceiveMetadata:forItem:error:))]
        fn did_receive_metadata(&self, _camera: &ICCameraDevice, _metadata: Option<&NSDictionary>, _item: &ICCameraItem, _error: Option<&NSError>) {}

        #[unsafe(method(cameraDevice:didRenameItems:))]
        fn did_rename_items(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}

        #[unsafe(method(cameraDeviceDidChangeCapability:))]
        fn did_change_capability(&self, _camera: &ICCameraDevice) {}

        #[unsafe(method(cameraDevice:didReceivePTPEvent:))]
        fn did_receive_ptp_event(&self, _camera: &ICCameraDevice, _event: &NSData) {}

        #[unsafe(method(deviceDidBecomeReadyWithCompleteContentCatalog:))]
        fn catalog_complete(&self, camera: &ICCameraDevice) {
            log("Katalog vollständig");
            let id = device_id(camera);
            self.ivars().shared.with(|v| {
                if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                    e.catalog_done = true;
                }
            });
            self.fill_catalog(camera);
        }

        #[unsafe(method(cameraDeviceDidRemoveAccessRestriction:))]
        fn restriction_removed(&self, device: &ICDevice) {
            let id = device_id(device);
            let reopen = self.ivars().shared.with(|v| v.iter().any(|e| e.id == id && e.wanted && matches!(e.state, CamState::Restricted | CamState::Failed(_) | CamState::Found)));
            if reopen {
                self.open_session(&id);
            }
        }

        #[unsafe(method(cameraDeviceDidEnableAccessRestriction:))]
        fn restriction_enabled(&self, device: &ICDevice) {
            let id = device_id(device);
            self.ivars().shared.with(|v| {
                if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                    e.state = CamState::Restricted;
                }
            });
            self.ivars().shared.repaint();
        }

        // the camera would otherwise send a thumbnail and the metadata of every file right after the catalog
        // (thousands of requests); the window asks for the visible ones itself
        #[unsafe(method(cameraDevice:shouldGetThumbnailOfItem:))]
        fn should_get_thumbnail(&self, _camera: &ICCameraDevice, item: &ICCameraItem) -> bool {
            let key = item as *const ICCameraItem as usize;
            auto_items() || self.ivars().shared.thumbs_wanted.lock().unwrap_or_else(|e| e.into_inner()).contains(&key)
        }

        #[unsafe(method(cameraDevice:shouldGetMetadataOfItem:))]
        fn should_get_metadata(&self, _camera: &ICCameraDevice, _item: &ICCameraItem) -> bool {
            auto_items()
        }
    }
);

impl Delegate {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { shared });
        // SAFETY: plain NSObject init
        unsafe { msg_send![super(this), init] }
    }

    fn added(&self, device: &ICDevice) {
        let Some(camera) = camera_of(device) else { return };
        // cameras that mount as a drive appear as a card
        let mass_storage = unsafe { camera.transportType() }.is_some_and(|t| &*t == unsafe { ICTransportTypeMassStorage }) || unsafe { camera.mountPoint() }.is_some();
        if mass_storage {
            return;
        }
        let id = device_id(device);
        let name = unsafe { device.name() }.map(|n| n.to_string()).unwrap_or_else(|| "Kamera".to_string());
        let restricted = unsafe { camera.isAccessRestrictedAppleDevice() };
        let wanted = self.ivars().shared.with(|v| {
            let state = if restricted { CamState::Restricted } else { CamState::Found };
            match v.iter_mut().find(|e| e.id == id) {
                Some(e) => {
                    e.device = Some(Obj(camera.retain()));
                    e.name = name.clone();
                    e.state = state;
                    e.wanted
                }
                None => {
                    v.push(Entry::new(id.clone(), name, Some(Obj(camera.retain())), state, false));
                    false
                }
            }
        });
        log(&format!("import: Kamera gefunden ({id})"));
        if wanted {
            self.open_session(&id);
        }
        self.ivars().shared.repaint();
    }

    fn removed(&self, device: &ICDevice) {
        let id = device_id(device);
        self.ivars().shared.with(|v| {
            if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                e.device = None;
                e.reset();
                e.state = CamState::Gone;
            }
            // a camera nobody chose simply disappears; a chosen one stays as "getrennt" (an import waits for it)
            v.retain(|e| e.wanted || e.device.is_some());
        });
        self.ivars().shared.repaint();
    }

    /// The complete list of files of a camera (after the catalog or when it is chosen again).
    fn fill_catalog(&self, camera: &ICCameraDevice) {
        let id = device_id(camera);
        let all = unsafe { camera.mediaFiles() };
        let files = all.as_ref().map(|a| files_of(a)).unwrap_or_default();
        log(&format!("mediaFiles: {} Einträge, davon {} Fotos/Videos", all.map(|a| a.count()).unwrap_or(0), files.len()));
        self.ivars().shared.with(|v| {
            if let Some(e) = v.iter_mut().find(|e| e.id == id) {
                e.catalog.clear();
                e.files.clear();
                add_files(e, files);
                // what the camera announced before (the same objects are skipped)
                let announced = std::mem::take(&mut e.announced);
                let before = e.catalog.len();
                add_files(e, announced);
                if e.catalog.len() > before {
                    log(&format!("{} angekündigte Dateien fehlten in mediaFiles", e.catalog.len() - before));
                }
                e.state = CamState::Ready(e.catalog.len());
                // an empty list is read again a few times (see `Browser::rows`)
                e.empty_check = e.catalog.is_empty().then(|| (Instant::now(), 0));
            }
        });
        self.ivars().shared.repaint();
    }

    /// Open the session of camera `id` (delegate: this object). Outside the lock: the camera may answer right away.
    fn open_session(&self, id: &str) {
        let device = self.ivars().shared.with(|v| {
            let e = v.iter_mut().find(|e| e.id == id)?;
            e.wanted = true;
            let d = e.device.as_ref()?.0.clone();
            if !matches!(e.state, CamState::Ready(_)) {
                e.state = CamState::Opening;
            }
            Some((d, e.catalog_done))
        });
        let Some((d, done)) = device else { return };
        unsafe {
            d.setDelegate(Some(ProtocolObject::from_ref(self)));
            if d.hasOpenSession() {
                // already open (chosen again): the list counts only once the camera said it is complete (otherwise it follows)
                if done {
                    self.fill_catalog(&d);
                }
            } else {
                d.requestOpenSession();
            }
        }
        self.ivars().shared.repaint();
    }
}

fn camera_of(device: &ICDevice) -> Option<&ICCameraDevice> {
    let any: &AnyObject = device;
    any.downcast_ref::<ICCameraDevice>()
}

/// Identifier of a camera, the same after reconnecting: serial number, otherwise name and USB ids.
fn device_id(device: &ICDevice) -> String {
    unsafe {
        let serial = device.serialNumberString().map(|s| s.to_string()).filter(|s| !s.trim().is_empty());
        let id = serial.unwrap_or_else(|| format!("{}-{:04x}{:04x}", device.name().map(|n| n.to_string()).unwrap_or_default(), device.usbVendorID(), device.usbProductID()));
        isrc::device_key(id.trim())
    }
}

/// Local time of an `NSDate` as `YYYY-MM-DDTHH:MM:SS`.
fn stamp(d: &NSDate) -> String {
    let z = crate::platform::local_time(d.timeIntervalSince1970() as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", z.year, z.month, z.tag, z.hour, z.minute, z.second)
}

/// Path of an item on the camera (folders from the top, `/` separated).
fn rel_of(item: &ICCameraItem) -> Option<String> {
    unsafe {
        let mut parts = vec![item.name()?.to_string().replace('/', "_")];
        let mut folder = item.parentFolder();
        for _ in 0..32 {
            let Some(f) = folder else { break };
            if let Some(n) = f.name() {
                parts.push(n.to_string().replace('/', "_"));
            }
            folder = f.parentFolder();
        }
        parts.reverse();
        Some(parts.join("/"))
    }
}

/// Files (photos and videos the app knows) among `items`.
fn files_of(items: &NSArray<ICCameraItem>) -> Vec<(SourceFile, Obj<Retained<ICCameraFile>>)> {
    let mut out = Vec::new();
    for item in items.iter() {
        let Ok(file) = item.downcast::<ICCameraFile>() else { continue };
        let Some(rel) = rel_of(&file) else { continue };
        if crate::format_from_extension(&rel).is_none() {
            continue;
        }
        let size = unsafe { file.fileSize() }.max(0) as u64;
        let created = unsafe { file.exifCreationDate().or_else(|| file.creationDate()) }.map(|d| stamp(&d));
        out.push((SourceFile { rel, size, created }, Obj(file)));
    }
    out
}

fn add_files(e: &mut Entry, files: Vec<(SourceFile, Obj<Retained<ICCameraFile>>)>) {
    // the camera may announce files again that the catalog already has: those are the same objects
    let known: std::collections::HashSet<*const ICCameraFile> = e.files.values().map(|f| Retained::as_ptr(&f.0)).collect();
    for (mut f, o) in files {
        if known.contains(&Retained::as_ptr(&o.0)) {
            continue;
        }
        // two cards in one camera can hold the same path: the second one gets its own folder in front
        let base = f.rel.clone();
        let mut n = 2;
        while e.files.contains_key(&f.rel) {
            f.rel = format!("{n}/{base}");
            n += 1;
        }
        e.files.insert(f.rel.clone(), o);
        e.catalog.push(f);
    }
}

/// The device browser of the open import window (stopped when the window closes).
pub struct Browser {
    browser: Retained<ICDeviceBrowser>,
    delegate: Retained<Delegate>,
    shared: Arc<Shared>,
}

impl Browser {
    pub fn start(repaint: Box<dyn Fn() + Send + Sync>) -> Option<Browser> {
        // the browser and its delegate belong to the main thread (the window is drawn there)
        objc2::MainThreadMarker::new()?;
        let shared = Arc::new(Shared { entries: Mutex::new(Vec::new()), thumbs_wanted: Mutex::default(), repaint });
        let delegate = Delegate::new(shared.clone());
        unsafe {
            let browser = ICDeviceBrowser::new();
            browser.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            browser.setBrowsedDeviceTypeMask(ICDeviceTypeMask(ICDeviceTypeMask::Camera.0 | ICDeviceLocationTypeMask::Local.0));
            browser.start();
            log("import: Kamerasuche gestartet");
            Some(Browser { browser, delegate, shared })
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        // an empty complete list (seen after reconnecting or waking up): read it again every 2 s, after 10 tries
        // close and open the session once more; files that arrive later come in by `didAddItems` anyway
        let (again, reopen): (Vec<Retained<ICCameraDevice>>, Vec<String>) = self.shared.with(|v| {
            let (mut again, mut reopen) = (Vec::new(), Vec::new());
            for e in v.iter_mut() {
                let (Some((t, n)), Some(d), CamState::Ready(0)) = (e.empty_check, &e.device, &e.state) else { continue };
                if !e.storages_asked {
                    // which storages does the camera offer at all? (an info storage instead of the card, see `InfoOnly`)
                    e.storages_asked = true;
                    let (shared, id, dev) = (self.shared.clone(), e.id.clone(), Obj(d.0.clone()));
                    std::thread::Builder::new()
                        .name("kamera-ablagen".into())
                        .spawn(move || {
                            let r = storages_of(&dev, &shared, &id);
                            log(&format!("Ablagen: {r:?}"));
                            if let Ok(list) = r {
                                if !list.is_empty() && !list.iter().any(|s| s.holds_photos()) {
                                    let what = list.iter().map(|s| if s.description.is_empty() { format!("{:08x}", s.id) } else { s.description.clone() }).collect::<Vec<_>>().join(", ");
                                    shared.with(|v| {
                                        if let Some(e) = v.iter_mut().find(|e| e.id == id && e.state == CamState::Ready(0)) {
                                            e.state = CamState::InfoOnly(what);
                                            e.empty_check = None;
                                        }
                                    });
                                    shared.repaint();
                                }
                            }
                        })
                        .ok();
                }
                if t.elapsed() < Duration::from_secs(2) {
                    continue;
                }
                if n < 10 {
                    e.empty_check = Some((Instant::now(), n + 1));
                    again.push(d.0.clone());
                } else {
                    e.empty_check = None;
                    if !e.auto_reopened {
                        e.auto_reopened = true;
                        reopen.push(e.id.clone());
                    }
                }
            }
            (again, reopen)
        });
        for d in again {
            log("leere Dateiliste: neu lesen");
            self.delegate.fill_catalog(&d);
        }
        for id in reopen {
            log("leere Dateiliste: Sitzung neu öffnen");
            self.reload(&id);
        }
        self.shared.with(|v| {
            for e in v.iter_mut() {
                // how far the camera has got with its list (it does not report every step itself)
                if let (CamState::Opening | CamState::Catalog(_), Some(d)) = (&e.state, &e.device) {
                    unsafe {
                        if d.0.hasOpenSession() {
                            e.state = CamState::Catalog(d.0.contentCatalogPercentCompleted().min(99) as u32);
                        }
                    }
                }
            }
            v.iter().map(|e| Row { id: e.id.clone(), name: e.name.clone(), state: e.state.clone() }).collect()
        })
    }

    /// The storages of camera `id` as a job for a helper thread (it waits for the camera's answer; the main thread
    /// must keep running).
    pub fn storages_job(&self, id: &str) -> Option<Box<dyn FnOnce() -> Result<Vec<isrc::Storage>, String> + Send>> {
        let id = isrc::device_key(id);
        let dev = self.shared.with(|v| v.iter().find(|e| e.id == id).and_then(|e| e.device.as_ref().map(|d| Obj(d.0.clone()))))?;
        let shared = self.shared.clone();
        Some(Box::new(move || storages_of(&dev, &shared, &id)))
    }

    /// Everything ImageCaptureCore says about the camera `id` (diagnosis): properties and the folder tree with counts.
    pub fn describe(&self, id: &str) -> Vec<String> {
        let id = isrc::device_key(id);
        let Some(d) = self.shared.with(|v| v.iter().find(|e| e.id == id).and_then(|e| e.device.as_ref().map(|d| d.0.clone()))) else { return vec!["kein Gerät".into()] };
        let mut out = Vec::new();
        unsafe {
            out.push(format!(
                "Sitzung offen {} · Katalog {} % · gesperrt {} · Apple-Sperre {} · Darstellung {} · Transport {:?} · Ort {:?} · Akku {}",
                d.hasOpenSession(),
                d.contentCatalogPercentCompleted(),
                d.isLocked(),
                d.isAccessRestrictedAppleDevice(),
                d.mediaPresentation().0,
                d.transportType().map(|t| t.to_string()),
                d.locationDescription().map(|t| t.to_string()),
                d.batteryLevel()
            ));
            out.push(format!("Fähigkeiten: {:?}", d.capabilities().iter().map(|c| c.to_string()).collect::<Vec<_>>()));
            out.push(format!("mediaFiles: {}", d.mediaFiles().map(|a| a.count()).unwrap_or(0)));
            unsafe fn walk(items: &NSArray<ICCameraItem>, depth: usize, out: &mut Vec<String>, files: &mut usize) {
                for item in items.iter() {
                    match item.downcast::<objc2_image_capture_core::ICCameraFolder>() {
                        Ok(f) => {
                            let inner = f.contents();
                            out.push(format!("{}{}/ ({} Einträge)", "  ".repeat(depth), f.name().map(|n| n.to_string()).unwrap_or_default(), inner.as_ref().map(|a| a.count()).unwrap_or(0)));
                            if let Some(a) = inner {
                                if depth < 6 {
                                    walk(&a, depth + 1, out, files);
                                }
                            }
                        }
                        Err(item) => {
                            *files += 1;
                            if *files <= 5 {
                                out.push(format!("{}{} · {:?} · temporär {}", "  ".repeat(depth), item.name().map(|n| n.to_string()).unwrap_or_default(), item.UTI().map(|u| u.to_string()), item.isInTemporaryStore()));
                            }
                        }
                    }
                }
            }
            let mut files = 0;
            match d.contents() {
                Some(c) => {
                    out.push(format!("contents: {} Einträge oben", c.count()));
                    walk(&c, 1, &mut out, &mut files);
                }
                None => out.push("contents: keine".into()),
            }
            out.push(format!("Dateien in contents: {files}"));
        }
        out
    }

    /// Read the camera again ("Neu einlesen"): close the session and open it again (the list is read anew).
    pub fn reload(&self, id: &str) {
        let id = isrc::device_key(id);
        let device = self.shared.with(|v| {
            let e = v.iter_mut().find(|e| e.id == id)?;
            e.wanted = true;
            e.reset();
            let d = e.device.as_ref()?.0.clone();
            e.state = CamState::Opening;
            e.reopen = unsafe { d.hasOpenSession() };
            Some((d, e.reopen))
        });
        self.shared.repaint();
        match device {
            Some((d, true)) => {
                log("Neu einlesen: Sitzung schließen");
                unsafe { d.requestCloseSession() };
            }
            Some((_, false)) => self.delegate.open_session(&id),
            None => {}
        }
    }

    pub fn open(&self, id: &str) -> Arc<dyn Source> {
        let id = isrc::device_key(id);
        let known = self.shared.with(|v| {
            let known = v.iter().any(|e| e.id == id);
            if !known {
                // not connected yet (e.g. "import the rest" after a crash): opened as soon as it appears
                v.push(Entry::new(id.clone(), "Kamera".to_string(), None, CamState::Gone, true));
            }
            known
        });
        if known {
            self.delegate.open_session(&id);
        }
        let name = self.shared.with(|v| v.iter().find(|e| e.id == id).map(|e| e.name.clone())).unwrap_or_default();
        let s: Arc<dyn Source> = Arc::new(CameraSource { id, name, shared: self.shared.clone() });
        isrc::register(s.clone());
        s
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        unsafe {
            self.browser.stop();
            self.browser.setDelegate(None);
        }
        let entries = self.shared.with(std::mem::take);
        for e in entries {
            isrc::unregister(&e.id);
            if let Some(d) = e.device {
                unsafe {
                    d.0.setDelegate(None);
                    if d.0.hasOpenSession() {
                        d.0.requestCloseSession();
                    }
                }
            }
        }
        log("import: Kamerasuche beendet");
    }
}

/// A camera as import source.
struct CameraSource {
    id: String,
    name: String,
    shared: Arc<Shared>,
}

/// Send a PTP command (operation `code`, parameters) on the main queue and wait for the data it returns.
fn ptp(dev: &Obj<Retained<ICCameraDevice>>, shared: &Shared, id: &str, code: u16, params: &[u32]) -> Result<Vec<u8>, String> {
    let mut cmd: Vec<u8> = Vec::with_capacity(12 + 4 * params.len());
    cmd.extend(((12 + 4 * params.len()) as u32).to_le_bytes());
    cmd.extend(1u16.to_le_bytes()); // command container
    cmd.extend(code.to_le_bytes());
    cmd.extend(0u32.to_le_bytes()); // transaction (set by the system)
    for p in params {
        cmd.extend(p.to_le_bytes());
    }
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    let dev = Obj(dev.0.clone());
    DispatchQueue::main().exec_async(move || {
        let dev = dev;
        let command = NSData::with_bytes(&cmd);
        let block = RcBlock::new(move |data: std::ptr::NonNull<NSData>, _response: std::ptr::NonNull<NSData>, err: *mut NSError| {
            let r = match error_text(err) {
                Some(e) => Err(e),
                // SAFETY: the system hands a valid NSData
                None => Ok(unsafe { data.as_ref() }.to_vec()),
            };
            let _ = tx.send(r);
        });
        unsafe { dev.0.requestSendPTPCommand_outData_completion(&command, None, &block) };
    });
    let t = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Kamera hat nicht geantwortet".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if t.elapsed() > Duration::from_secs(10) || !shared.with(|v| v.iter().any(|e| e.id == id && e.device.is_some())) {
                    return Err("Kamera antwortet nicht".into());
                }
            }
        }
    }
}

/// The storages the camera offers (PTP `GetStorageIDs` and `GetStorageInfo`), read only.
fn storages_of(dev: &Obj<Retained<ICCameraDevice>>, shared: &Shared, id: &str) -> Result<Vec<isrc::Storage>, String> {
    let ids = isrc::ptp_u32_array(&ptp(dev, shared, id, 0x1004, &[])?);
    let mut out = Vec::new();
    for s in ids {
        let d = ptp(dev, shared, id, 0x1005, &[s])?;
        out.push(isrc::ptp_storage_info(s, &d).ok_or_else(|| format!("Ablage {s:08x}: unlesbare Antwort ({} Bytes)", d.len()))?);
    }
    Ok(out)
}

fn error_text(e: *mut NSError) -> Option<String> {
    // SAFETY: the pointer is an NSError or null
    unsafe { e.as_ref() }.map(|e| e.localizedDescription().to_string())
}

impl CameraSource {
    fn file(&self, rel: &str) -> Result<Obj<Retained<ICCameraFile>>, String> {
        self.shared
            .with(|v| {
                let e = v.iter().find(|e| e.id == self.id && matches!(e.state, CamState::Ready(_)))?;
                e.files.get(rel).map(|f| Obj(f.0.clone()))
            })
            .ok_or_else(|| if self.connected() { format!("{rel}: nicht mehr auf der Kamera") } else { "Kamera nicht verbunden".to_string() })
    }

    /// Start a request on the main queue and wait for its answer; gives up when the camera goes away or does not
    /// answer within `limit`. `tick` runs while waiting (download progress).
    fn ask<R: Send + 'static>(&self, what: &str, rel: &str, limit: Duration, start: impl FnOnce(&ICCameraFile, mpsc::Sender<Result<R, String>>) + Send + 'static, tick: &mut dyn FnMut()) -> Result<R, String> {
        let file = self.file(rel)?;
        let (tx, rx) = mpsc::channel();
        let what = format!("{what} {rel}");
        log(&format!("{what}: angefordert"));
        let w2 = what.clone();
        DispatchQueue::main().exec_async(move || {
            let file = file;
            log(&format!("{w2}: an die Kamera"));
            start(&file.0, tx);
        });
        let t = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(r) => {
                    log(&format!("{what}: Antwort nach {:.2} s: {}", t.elapsed().as_secs_f64(), match &r { Ok(_) => "ok".to_string(), Err(e) => e.clone() }));
                    return r;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Kamera hat nicht geantwortet".to_string()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    tick();
                    if !self.connected() {
                        return Err("Verbindung zur Kamera unterbrochen".to_string());
                    }
                    if t.elapsed() > limit {
                        return Err("Kamera antwortet nicht".to_string());
                    }
                }
            }
        }
    }

    /// EXIF orientation of a file and its size as stored (for thumbnails that come unturned).
    fn orientation(&self, rel: &str) -> (u32, i64, i64) {
        self.file(rel).map(|f| unsafe { (f.0.orientation().0 as u32, f.0.width() as i64, f.0.height() as i64) }).unwrap_or((1, 0, 0))
    }
}

impl Source for CameraSource {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn connected(&self) -> bool {
        self.shared.with(|v| v.iter().any(|e| e.id == self.id && e.device.is_some() && matches!(e.state, CamState::Ready(_))))
    }
    fn list(&self) -> Result<Vec<SourceFile>, String> {
        self.shared
            .with(|v| v.iter().find(|e| e.id == self.id && matches!(e.state, CamState::Ready(_))).map(|e| e.catalog.clone()))
            .ok_or_else(|| "Kamera nicht bereit".to_string())
    }
    fn read_range(&self, rel: &str, offset: u64, len: u64) -> Result<Vec<u8>, String> {
        self.ask(
            "Lesen",
            rel,
            Duration::from_secs(60),
            move |file, tx| {
                let block = RcBlock::new(move |data: *mut NSData, err: *mut NSError| {
                    // SAFETY: the pointer is an NSData or null
                    let r = match (unsafe { data.as_ref() }, error_text(err)) {
                        (_, Some(e)) => Err(e),
                        (Some(d), None) => Ok(d.to_vec()),
                        (None, None) => Err("Kamera hat keine Daten geliefert".to_string()),
                    };
                    let _ = tx.send(r);
                });
                unsafe { file.requestReadDataAtOffset_length_completion(offset as i64, len as i64, &block) };
            },
            &mut || {},
        )
    }
    fn download(&self, rel: &str, dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String> {
        let dir_url = NSURL::from_directory_path(dir).ok_or_else(|| format!("{}: kein Ordner", dir.display()))?;
        let (dir_url, name_s) = (Obj(dir_url), Obj(NSString::from_str(name)));
        let progress: Arc<Mutex<Option<Obj<Retained<NSProgress>>>>> = Arc::default();
        let p2 = progress.clone();
        let saved = self.ask(
            "Laden",
            rel,
            // a large video over a slow cable may take long; a pulled cable ends it right away
            Duration::from_secs(6 * 3600),
            move |file, tx| {
                let (dir_url, name_s) = (dir_url, name_s);
                let keys: [&NSString; 2] = unsafe { [ICDownloadsDirectoryURL, ICSaveAsFilename] };
                let values: [&AnyObject; 2] = [&dir_url.0, &name_s.0];
                let options = NSDictionary::from_slices(&keys, &values);
                let block = RcBlock::new(move |saved: *mut NSString, err: *mut NSError| {
                    // SAFETY: the pointer is an NSString or null
                    let r = match (unsafe { saved.as_ref() }, error_text(err)) {
                        (_, Some(e)) => Err(e),
                        (Some(s), None) => Ok(s.to_string()),
                        (None, None) => Err("Kamera hat die Datei nicht geliefert".to_string()),
                    };
                    let _ = tx.send(r);
                });
                let p = unsafe { file.requestDownloadWithOptions_completion(Some(&options), &block) };
                *p2.lock().unwrap_or_else(|e| e.into_inner()) = p.map(Obj);
            },
            &mut || {
                if let Some(p) = &*progress.lock().unwrap_or_else(|e| e.into_inner()) {
                    let done = p.0.completedUnitCount();
                    if done > 0 {
                        bytes(done as u64);
                    }
                }
            },
        )?;
        let saved = if saved.is_empty() { name.to_string() } else { saved };
        let p = dir.join(Path::new(&saved).file_name().unwrap_or_default());
        if p.is_file() {
            Ok(p)
        } else {
            Err(format!("{}: {}", p.display(), "nicht angekommen"))
        }
    }
    fn thumbnail(&self, rel: &str, edge: u32) -> Option<image::RgbImage> {
        let shared = self.shared.clone();
        let jpeg = self
            .ask(
                "Miniatur",
                rel,
                Duration::from_secs(30),
                move |file, tx| {
                    // allow the camera to fetch this one thumbnail (see `thumbs_wanted`)
                    let key = file as *const ICCameraFile as usize;
                    shared.thumbs_wanted.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
                    let num = NSNumber::numberWithUnsignedInteger(edge as usize);
                    let keys: [&NSString; 1] = unsafe { [ICImageSourceThumbnailMaxPixelSize] };
                    let values: [&AnyObject; 1] = [&num];
                    let options = NSDictionary::from_slices(&keys, &values);
                    let block = RcBlock::new(move |data: *mut NSData, err: *mut NSError| {
                        // SAFETY: the pointer is an NSData or null
                        let r = match (unsafe { data.as_ref() }, error_text(err)) {
                            (Some(d), None) => Ok(d.to_vec()),
                            (_, e) => Err(e.unwrap_or_default()),
                        };
                        let _ = tx.send(r);
                    });
                    unsafe { file.requestThumbnailDataWithOptions_completion(Some(&options), &block) };
                },
                &mut || {},
            )
            .ok()?;
        let img = image::load_from_memory(&jpeg).ok()?.to_rgb8();
        Some(upright(img, self.orientation(rel)))
    }
}

/// Turn a thumbnail upright when it still has the orientation of the sensor: for a turned photo (EXIF 6/8) such a
/// thumbnail has the same aspect as the stored image.
fn upright(img: image::RgbImage, (o, w, h): (u32, i64, i64)) -> image::RgbImage {
    let unturned = w > 0 && h > 0 && ((img.width() >= img.height()) == (w >= h));
    match o {
        6 if unturned => image::imageops::rotate90(&img),
        8 if unturned => image::imageops::rotate270(&img),
        _ => img,
    }
}

#[allow(dead_code)]
fn _unused(_: &NSObject) {}
