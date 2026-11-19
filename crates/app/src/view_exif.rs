//! Header data (capture time, camera, lens, exposure, pixel size) for the photos of a viewed folder. Their rows start
//! without it (listing a folder reads no file contents); it is read here in the background only when something needs
//! it: an info line of the grid that shows such a field (`cell_info.rs`), or the order by capture time chosen by the user (not the default order alone).
//!
//! - One thread, one file after the other (no wave of parallel accesses on a network drive), with the platform-
//!   independent header reader (`library::exif::read_header`: 64–192 KiB per file for most formats).
//! - The tiles on screen first, then the rest: for the info lines [`AHEAD`] photos ahead with priority Background
//!   (the tiles on screen with Utility); for the order by capture time the whole folder with Utility, because the
//!   list waits for it (with Background a local folder of 450 RAWs took 7–19 s while its previews were made, with
//!   one file costing about 1 ms on its own).
//! - The results go into the rows shown (never into the library: viewing a folder writes nothing) and into a cache by
//!   path, so a folder chosen again in the tree has its values at once. Taken over at most every [`APPLY_EVERY`].
//! - Order by capture time: the list is sorted again once, when the whole folder has been read.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use library::db::Row;
use library::exif::Capture;

use super::{LibraryView, SortOrder, Source, VIEW_FOLDER};

/// Results are taken into the rows at most this often (each take may copy the rows once).
pub const APPLY_EVERY: Duration = Duration::from_millis(250);
/// For the info lines alone: photos read ahead of the last one on screen.
pub const AHEAD: usize = 120;

struct Job {
    path: PathBuf,
    format: String,
    visible: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<VecDeque<Job>>,
    results: Mutex<Vec<(PathBuf, Option<Capture>)>>,
    running: AtomicBool,
}

#[derive(Default)]
pub(super) struct ViewExif {
    /// read so far, by path (`None`: nothing readable)
    cache: HashMap<PathBuf, Option<Capture>>,
    shared: Arc<Shared>,
    /// what the queue was built for (on-screen tiles, list, needs)
    signature: u64,
    last_apply: Option<Instant>,
    /// capture times arrived since the list was last sorted
    resort: bool,
    /// measurement: start of the current pass and files read in it
    pass: Option<(Instant, usize)>,
}

/// Fill the fields of `z` that are still empty from `c`.
pub(super) fn fill(z: &mut Row, c: &Capture) {
    z.capture = z.capture.take().or_else(|| c.capture.clone());
    z.camera = z.camera.take().or_else(|| c.camera.clone());
    z.lens = z.lens.take().or_else(|| c.lens.clone());
    z.aperture = z.aperture.or(c.aperture);
    z.focal = z.focal.or(c.focal);
    z.time = z.time.or(c.time);
    z.iso = z.iso.or(c.iso);
    z.width = z.width.or(c.width.map(i64::from));
    z.height = z.height.or(c.height.map(i64::from));
}

/// The order in which files are read: `on_screen` first, then the list from the last one on screen on (`ahead`
/// photos; `None`: all, then those before). Each path once, never one already read.
pub(super) fn read_order(on_screen: &[usize], visible: &[usize], ahead: Option<usize>, path: &dyn Fn(usize) -> Option<PathBuf>, done: &dyn Fn(&PathBuf) -> bool) -> Vec<(usize, bool)> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    let mut push = |i: usize, vis: bool, out: &mut Vec<(usize, bool)>| {
        if let Some(p) = path(i) {
            if !done(&p) && seen.insert(p) {
                out.push((i, vis));
            }
        }
    };
    for &i in on_screen {
        push(i, true, &mut out);
    }
    let last = on_screen.last().and_then(|l| visible.iter().position(|v| v == l)).map_or(0, |p| p + 1);
    let after = &visible[last.min(visible.len())..];
    match ahead {
        Some(n) => after.iter().take(n).for_each(|&i| push(i, false, &mut out)),
        None => {
            after.iter().chain(&visible[..last.min(visible.len())]).for_each(|&i| push(i, false, &mut out));
        }
    }
    out
}

impl LibraryView {
    /// Every frame: take what was read, build the queue anew when the tiles on screen or the needs changed, start
    /// the reader. Does nothing while no folder is viewed or nothing needs the data.
    pub(super) fn view_exif_poll(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.viewed.as_ref().map(|v| v.0.clone()) else {
            if !self.view_exif.cache.is_empty() {
                self.view_exif = ViewExif::default();
            }
            return;
        };
        self.view_exif_take(&dir);
        let browsing = matches!(self.source, Source::Browse(_));
        let by_time = browsing && self.sort_order == SortOrder::CaptureTime && self.view_sort_chosen;
        let lines = crate::cell_info::needs_exif();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (by_time, lines, browsing, &dir, self.list_version, &self.on_screen).hash(&mut h);
        let sig = h.finish();
        if sig != self.view_exif.signature {
            self.view_exif.signature = sig;
            let jobs = if browsing && (by_time || lines) { self.view_exif_jobs(&dir, if by_time { None } else { Some(AHEAD) }, by_time) } else { VecDeque::new() };
            *self.view_exif.shared.queue.lock().unwrap_or_else(|e| e.into_inner()) = jobs;
        }
        self.view_exif_start(ctx);
        // the whole folder read: sort once by the capture times found
        let shared = &self.view_exif.shared;
        let idle = !shared.running.load(Ordering::SeqCst) && shared.queue.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && shared.results.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
        if idle {
            if let Some((t, n)) = self.view_exif.pass.take() {
                crate::log(&format!("view folder: header data of {n} files read in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0));
            }
        }
        if idle && by_time && std::mem::take(&mut self.view_exif.resort) {
            let active_shown = self.active.is_some_and(|a| self.on_screen.contains(&a));
            self.search_job.forget_capture_rank();
            self.refilter = true;
            self.to_active = active_shown;
        }
    }

    fn view_exif_jobs(&self, dir: &std::path::Path, ahead: Option<usize>, urgent: bool) -> VecDeque<Job> {
        let on_screen: Vec<usize> = self.on_screen.iter().filter_map(|b| self.index.get(b).copied()).collect();
        let path = |i: usize| self.rows.get(i).filter(|z| z.folder == VIEW_FOLDER).map(|z| dir.join(&z.name));
        let cache = &self.view_exif.cache;
        read_order(&on_screen, &self.visible, ahead, &path, &|p| cache.contains_key(p))
            .into_iter()
            .map(|(i, visible)| Job { path: dir.join(&self.rows[i].name), format: self.rows[i].format.clone(), visible: visible || urgent })
            .collect()
    }

    fn view_exif_start(&mut self, ctx: &egui::Context) {
        let shared = self.view_exif.shared.clone();
        if shared.queue.lock().unwrap_or_else(|e| e.into_inner()).is_empty() || shared.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.view_exif.pass.get_or_insert((Instant::now(), 0));
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("ansehen-exif".into()).spawn(move || {
            let mut priority = None;
            let mut last_wake = Instant::now();
            loop {
                let job = shared.queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                let Some(job) = job else { break };
                let want = if job.visible { library::platform::Priority::Utility } else { library::platform::Priority::Background };
                if priority != Some(want) {
                    library::platform::set_priority(want);
                    priority = Some(want);
                }
                let c = library::exif::read_header(&job.path, &job.format);
                shared.results.lock().unwrap_or_else(|e| e.into_inner()).push((job.path, c));
                if last_wake.elapsed() >= APPLY_EVERY {
                    ctx.request_repaint();
                    last_wake = Instant::now();
                }
            }
            shared.running.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
        if spawned.is_err() {
            self.view_exif.shared.running.store(false, Ordering::SeqCst);
        }
    }

    /// Take the results read so far into the rows (at most every [`APPLY_EVERY`], the last ones at once).
    fn view_exif_take(&mut self, dir: &std::path::Path) {
        let running = self.view_exif.shared.running.load(Ordering::SeqCst);
        if running && self.view_exif.last_apply.is_some_and(|t| t.elapsed() < APPLY_EVERY) {
            return;
        }
        let got = std::mem::take(&mut *self.view_exif.shared.results.lock().unwrap_or_else(|e| e.into_inner()));
        if got.is_empty() {
            return;
        }
        self.view_exif.last_apply = Some(Instant::now());
        if let Some(p) = &mut self.view_exif.pass {
            p.1 += got.len();
        }
        for (p, c) in got {
            self.view_exif.cache.insert(p, c);
        }
        self.view_exif_fill(dir);
    }

    /// Fill the viewed rows (shown and kept) from the cache. Called after results arrived and when a folder's rows were
    /// made anew (tree, refresh).
    pub(super) fn view_exif_fill(&mut self, dir: &std::path::Path) {
        let cache = &self.view_exif.cache;
        if cache.is_empty() {
            return;
        }
        let pending: Vec<usize> = self.rows.iter().enumerate().filter(|(_, z)| z.folder == VIEW_FOLDER).filter(|(_, z)| cache.get(&dir.join(&z.name)).is_some_and(|c| c.is_some()) && z.width.is_none() && z.capture.is_none() && z.camera.is_none()).map(|(i, _)| i).collect();
        if pending.is_empty() {
            return;
        }
        let mut times = false;
        let rows = Arc::make_mut(&mut self.rows);
        for i in pending {
            if let Some(Some(c)) = cache.get(&dir.join(&rows[i].name)) {
                times |= rows[i].capture.is_none() && c.capture.is_some();
                fill(&mut rows[i], c);
            }
        }
        if let Some((_, kept)) = &mut self.viewed {
            for z in kept.iter_mut() {
                if let Some(Some(c)) = cache.get(&dir.join(&z.name)) {
                    fill(z, c);
                }
            }
        }
        self.view_exif.resort |= times;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_only_empty_fields() {
        let mut z = Row { camera: Some("Own".into()), ..Row::default() };
        let c = Capture { camera: Some("Other".into()), capture: Some("2024-01-02T03:04:05".into()), focal: Some(50.0), iso: Some(200), width: Some(10), height: Some(20), ..Capture::default() };
        fill(&mut z, &c);
        assert_eq!(z.camera.as_deref(), Some("Own"));
        assert_eq!(z.capture.as_deref(), Some("2024-01-02T03:04:05"));
        assert_eq!((z.focal, z.iso, z.width, z.height), (Some(50.0), Some(200), Some(10), Some(20)));
    }

    #[test]
    fn order_screen_first_then_ahead_or_all() {
        let visible: Vec<usize> = (0..10).collect();
        let path = |i: usize| Some(PathBuf::from(format!("/f/{}", i % 9))); // 9 and 0 share a path
        let none = |_: &PathBuf| false;
        // on screen 4, 5: then two ahead
        assert_eq!(read_order(&[4, 5], &visible, Some(2), &path, &none), vec![(4, true), (5, true), (6, false), (7, false)]);
        // all: after the screen to the end, then the start (0 has the path of 9: read once)
        let all: Vec<usize> = read_order(&[4, 5], &visible, None, &path, &none).into_iter().map(|x| x.0).collect();
        assert_eq!(all, vec![4, 5, 6, 7, 8, 9, 1, 2, 3]);
        // read ones are left out
        let done = |p: &PathBuf| p == &PathBuf::from("/f/6");
        assert_eq!(read_order(&[4], &visible, Some(2), &path, &done), vec![(4, true), (5, false)]);
        // nothing on screen yet: from the start
        assert_eq!(read_order(&[], &visible, Some(3), &path, &none), vec![(0, false), (1, false), (2, false)]);
        // rows that are not part of the viewed folder have no path
        let only_even = |i: usize| (i % 2 == 0).then(|| PathBuf::from(format!("/f/{i}")));
        assert_eq!(read_order(&[1, 2], &visible, Some(3), &only_even, &none), vec![(2, true), (4, false)]);
    }

    /// Little-endian TIFF block: camera model in IFD0; capture time, exposure time, aperture, ISO and focal length in
    /// the EXIF IFD.
    fn tiff(model: &str, date: &str, focal: u32, f10: u32, denom: u32, iso: u16) -> Vec<u8> {
        type E = (u16, u16, u32, Vec<u8>);
        let ascii = |tag: u16, t: &str| -> E {
            let mut b = t.as_bytes().to_vec();
            b.push(0);
            (tag, 2, b.len() as u32, b)
        };
        let rational = |tag: u16, n: u32, d: u32| -> E { (tag, 5, 1, [n.to_le_bytes(), d.to_le_bytes()].concat()) };
        let ifd0_n = 2usize;
        let exif: Vec<E> = vec![rational(0x829A, 1, denom), rational(0x829D, f10, 10), (0x8827, 3, 1, iso.to_le_bytes().to_vec()), ascii(0x9003, date), rational(0x920A, focal, 1)];
        let exif_at = 8 + 2 + ifd0_n * 12 + 4;
        let ifd0: Vec<E> = vec![ascii(0x0110, model), (0x8769, 4, 1, (exif_at as u32).to_le_bytes().to_vec())];
        let mut data_at = (exif_at + 2 + exif.len() * 12 + 4) as u32;
        let mut data: Vec<u8> = Vec::new();
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        for list in [&ifd0, &exif] {
            out.extend((list.len() as u16).to_le_bytes());
            for (tag, ty, count, bytes) in list.iter() {
                out.extend(tag.to_le_bytes());
                out.extend(ty.to_le_bytes());
                out.extend(count.to_le_bytes());
                if bytes.len() <= 4 {
                    let mut v = bytes.clone();
                    v.resize(4, 0);
                    out.extend(v);
                } else {
                    out.extend(data_at.to_le_bytes());
                    data.extend(bytes);
                    data_at += bytes.len() as u32;
                }
            }
            out.extend(0u32.to_le_bytes());
        }
        out.extend(data);
        out
    }

    fn jpeg(p: &std::path::Path, exif: &[u8]) {
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut plain).encode(&[128u8; 64], 8, 8, image::ExtendedColorType::L8).unwrap();
        let mut out = plain[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(((2 + 6 + exif.len()) as u16).to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend(exif);
        out.extend(&plain[2..]);
        std::fs::write(p, out).unwrap();
    }

    /// The default order (by capture time) does not read the headers of a viewed folder; only choosing it does.
    #[test]
    fn default_order_reads_nothing_until_chosen() {
        use crate::cell_info::Field;
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let root = std::env::temp_dir().join(format!("graineo-view-exif-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        jpeg(&root.join("a.jpg"), &tiff("Canon EOS RP", "2020:01:01 10:00:00", 65, 40, 125, 400));
        jpeg(&root.join("b.jpg"), &tiff("Canon EOS RP", "2024:06:01 10:00:00", 35, 28, 60, 100));
        crate::cell_info::set_for_test(Some([Field::Off, Field::Off]));
        let ctx = egui::Context::default();
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&root, None);
        assert!(b.sort_order == SortOrder::CaptureTime, "the default order");
        for _ in 0..5 {
            b.view_exif_poll(&ctx);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(b.view_exif.cache.is_empty() && b.rows.iter().all(|z| z.camera.is_none()), "nothing read for the default order");
        b.view_sort_chosen = true;
        let t0 = Instant::now();
        while b.view_exif.cache.len() < 2 && t0.elapsed() < Duration::from_secs(10) {
            b.view_exif_poll(&ctx);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(b.view_exif.cache.len(), 2, "chosen: the whole folder is read");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A viewed folder: with info lines on, the header data is read in the background and fills the rows (shown and
    /// kept), the lines show it, the cells are taller by the lines; the order by capture time follows once all is read;
    /// a folder chosen again has its values at once. Lines off and another order: nothing is read.
    #[test]
    fn viewed_folder_gets_header_data_on_demand() {
        use crate::cell_info::Field;
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
        let _lang = crate::menu::i18n::locale::TestLanguage::set(crate::menu::i18n::Language::De);
        let root = std::env::temp_dir().join(format!("graineo-view-exif-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        jpeg(&root.join("a.jpg"), &tiff("Canon EOS RP", "2020:01:01 10:00:00", 65, 40, 125, 400));
        jpeg(&root.join("b.jpg"), &tiff("Canon EOS RP", "2024:06:01 10:00:00", 35, 28, 60, 100));
        jpeg(&root.join("c.jpg"), &tiff("Canon EOS RP", "2022:03:01 10:00:00", 50, 56, 250, 200));
        image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(root.join("d.png")).unwrap();
        crate::cell_info::set_for_test(Some([Field::Exposure, Field::Camera]));
        let ctx = egui::Context::default();
        crate::lr::apply_theme(&ctx);
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&root, None);
        b.centre_width = 600.0;
        b.cell_size = 290.0;
        b.sort_order = SortOrder::CaptureTime;
        b.view_sort_chosen = true;
        b.refilter = true;
        b.apply_filter();
        let names = |b: &LibraryView| b.visible.iter().map(|&i| b.rows[i].name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&b), vec!["a.jpg", "b.jpg", "c.jpg", "d.png"], "without capture times: by name");
        assert!(b.rows.iter().filter(|z| z.folder == VIEW_FOLDER).all(|z| z.camera.is_none()));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(620.0, 1400.0));
        let mut t = 0.0;
        let mut frame = |b: &mut LibraryView| {
            t += 0.05;
            let out = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), time: Some(t), ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let mut actions = Vec::new();
                    b.raster(ui, &mut actions);
                });
            });
            b.view_exif_poll(&ctx);
            b.apply_filter();
            out
        };
        let t0 = Instant::now();
        let mut out = frame(&mut b);
        while (b.view_exif.shared.running.load(Ordering::SeqCst) || b.rows.iter().any(|z| z.folder == VIEW_FOLDER && z.name.ends_with(".jpg") && z.camera.is_none()) || b.refilter) && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
            out = frame(&mut b);
        }
        out = frame(&mut b);
        let a = b.rows.iter().find(|z| z.name == "a.jpg").unwrap();
        assert_eq!((a.camera.as_deref(), a.focal, a.aperture, a.iso), (Some("Canon EOS RP"), Some(65.0), Some(4.0), Some(400)));
        assert_eq!(a.capture.as_deref(), Some("2020-01-01T10:00:00"));
        assert!(b.viewed.as_ref().unwrap().1.iter().find(|z| z.name == "a.jpg").unwrap().camera.is_some(), "the kept rows too");
        assert_eq!(names(&b), vec!["b.jpg", "c.jpg", "a.jpg", "d.png"], "sorted once by the capture times read");
        let texts: Vec<String> = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect();
        assert!(texts.iter().any(|t| t == "65 mm · f/4,0 · 1/125 s · ISO 400"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Canon EOS RP"), "{texts:?}");
        // cell height: square plus two lines
        let (_, r) = b.test_cells[0];
        assert!((r.height() - r.width() - crate::cell_info::extra_height(2)).abs() < 0.01, "{r:?}");
        // the png has no shooting data: tried once, its lines stay empty
        assert!(b.view_exif.cache.get(&root.join("d.png")).is_some_and(|c| c.as_ref().is_none_or(|c| c.camera.is_none())));
        // the folder chosen again: values at once from the cache, nothing read again
        let read = b.view_exif.cache.len();
        b.view_select("");
        assert!(b.rows.iter().find(|z| z.name == "b.jpg").unwrap().camera.is_some());
        assert_eq!(b.view_exif.cache.len(), read);
        // lines off: square cells again; with the order by name nothing is read for a new folder
        crate::cell_info::set_for_test(Some([Field::Off, Field::Off]));
        b.sort_order = SortOrder::Name;
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        jpeg(&other.join("x.jpg"), &tiff("X", "2021:01:01 10:00:00", 20, 20, 30, 800));
        b.browse_folder(&other, None);
        b.refilter = true;
        for _ in 0..5 {
            frame(&mut b);
        }
        let (_, r) = b.test_cells[0];
        assert!((r.height() - r.width()).abs() < 0.01, "{r:?}");
        assert!(!b.view_exif.cache.contains_key(&other.join("x.jpg")), "nothing needs the data: not read");
        assert!(b.rows.iter().find(|z| z.name == "x.jpg").unwrap().camera.is_none());
        crate::cell_info::set_for_test(None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
