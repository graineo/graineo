//! The open merged photo takes new pixels in place: a merge job delivers its preview, then its full resolution
//! (`hdr_ui::Event::Pixels`); a background thread builds the new document from the shown one (camera data, looks and
//! orientation stay) and the app swaps it in. The photo is never opened again: zoom, pan and the picture on screen stay
//! until the new picture is rendered, the sharp crop at 100 % stays until the new one is there (no jump to "fit", no
//! blurry moment).

use std::sync::{Arc, Mutex};

use crate::Settings;

/// A new document of a merged photo, ready to be swapped in.
struct Ready {
    file: i64,
    key: String,
    loaded: crate::jobs::Loaded,
    with_full: bool,
}

static READY: Mutex<Vec<Ready>> = Mutex::new(Vec::new());
/// Documents being built now (new pixels of the open photo).
static BUILDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The last computation started by itself for the photo shown: (file, key of the document shown, key computed).
static LAST_AUTO: Mutex<Option<((i64, String, String), std::time::Instant)>> = Mutex::new(None);

/// The user asked for a result of `file` (a change, "Neu berechnen"): the next automatic step counts anew.
pub(crate) fn asked(file: i64) {
    LAST_AUTO.lock().unwrap_or_else(|e| e.into_inner()).take_if(|l| l.0 .0 == file);
}
/// A full resolution that arrived before the preview of its result was swapped in.
static WAITING_FULL: Mutex<Option<(i64, String, Arc<hdr::Merged>, rawdev::merged::Meta)>> = Mutex::new(None);

/// Times of the swaps for the log (from the change to the picture in place).
static STARTED: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);

/// A change of the recipe was sent (log of the time until its pixels are in place).
pub(crate) fn note_change(key: &str) {
    *STARTED.lock().unwrap_or_else(|e| e.into_inner()) = Some((key.to_string(), std::time::Instant::now()));
}

fn since_change(key: &str) -> Option<f64> {
    STARTED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(k, _)| k == key).map(|(_, t)| t.elapsed().as_secs_f64() * 1000.0)
}

/// A message of the tool for the status (taken by the app in the next frame).
static SAID: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn say(text: &str) {
    *SAID.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_string());
}

pub(crate) fn take_said() -> Option<String> {
    SAID.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// "Quellen zeigen" from the filmstrip or the single image: the grid shows the opened stack.
static SHOW_GRID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn show_grid() {
    SHOW_GRID.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The hint after a merge was created: file and when it came.
static CREATED: Mutex<Option<(i64, f64)>> = Mutex::new(None);

/// How long the hint stays (seconds).
const HINT_SECS: f64 = 10.0;

/// A newly created merge is done: the hint "HDR erstellt · Rückgängig · Quellen zeigen".
pub(crate) fn created_done(file: i64, now: f64) {
    *CREATED.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, now));
}

/// The mark "HDR" on a picture of a merged photo (grid and filmstrip), at the bottom left of `r`.
pub(crate) fn paint_mark(p: &egui::Painter, r: egui::Rect) {
    let g = p.layout_no_wrap("HDR".into(), egui::FontId::proportional(9.5), egui::Color32::from_gray(0x16));
    let box_ = egui::Rect::from_min_size(r.left_bottom() + egui::vec2(4.0, -g.size().y - 6.0), g.size() + egui::vec2(8.0, 2.0));
    p.rect_filled(box_, 1.0, egui::Color32::from_gray(0xd8));
    p.galley(box_.min + egui::vec2(4.0, 1.0), g, egui::Color32::from_gray(0x16));
}

/// Test: a new session (nothing waiting to be swapped in, no hint, no message).
#[cfg(test)]
pub(crate) fn test_new_session() {
    READY.lock().unwrap_or_else(|e| e.into_inner()).clear();
    *LAST_AUTO.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *WAITING_FULL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *STARTED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SAID.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *CREATED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    SHOW_GRID.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Test: documents built and not swapped in yet.
#[cfg(test)]
pub(crate) fn test_waiting() -> usize {
    READY.lock().unwrap_or_else(|e| e.into_inner()).len()
}

impl crate::App {
    /// The merged photo shown now (`file`) and its document.
    pub(crate) fn shown_merged(&self) -> Option<(usize, Arc<rawdev::RawDoc>)> {
        let c = self.current.as_ref()?;
        let doc = c.loaded.previews.foreign.clone()?;
        doc.merged.as_ref()?;
        Some((c.index, doc))
    }

    /// New pixels of a merged photo from its job: build the new document in the background.
    pub(crate) fn merge_pixels(&mut self, file: i64, key: String, preview: Option<Arc<hdr::Merged>>, full: Option<Arc<hdr::Merged>>, meta: rawdev::merged::Meta) {
        // only the newest change counts
        let Some(src) = crate::hdr_ui::wanted(file).filter(|s| s.key == key) else { return };
        let Some((index, doc)) = self.shown_merged() else { return };
        let Some(m) = doc.merged.as_ref().filter(|m| m.source.file == file) else { return };
        if preview.is_none() && (m.source.key != key || m.pending) {
            // the full resolution came before the preview of its result is in place: it follows that one. Also when
            // the photo shown is this result still standing in with its reference frame (a cached result hands over
            // preview and full resolution in one go): a document built from it would keep the reference's preview and
            // count as the merge
            if let Some(f) = full {
                *WAITING_FULL.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, key, f, meta));
            }
            return;
        }
        let path = self.current.as_ref().map(|c| c.loaded.path.clone()).unwrap_or_default();
        let settings: Settings = self.entries[index].settings.clone();
        let with_full = full.is_some();
        BUILDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // (counted down whatever happens to the thread: a panic must not leave the photo "being built" for ever –
        // nothing would be computed for it by itself any more)
        struct Building;
        impl Drop for Building {
            fn drop(&mut self) {
                BUILDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        crate::hdr_ui::spawn(move || {
            let building = Building;
            let t = std::time::Instant::now();
            let new = doc.with_merged(preview.as_deref(), full.as_deref(), src, meta);
            let loaded = crate::raw_app::loaded_from_doc(new, &path, &settings, &|| true, t, std::time::Duration::ZERO);
            READY.lock().unwrap_or_else(|e| e.into_inner()).push(Ready { file, key, loaded, with_full });
            drop(building);
            crate::log(&format!("hdr: new document {} in {:.0} ms", if with_full { "with full resolution" } else { "preview" }, t.elapsed().as_secs_f64() * 1000.0));
        });
    }

    /// Each frame: swap in a document built for the open merged photo; keep the merge of the open photo in memory;
    /// a merged photo that is not computed yet (it shows its reference) is computed now.
    pub(crate) fn merge_doc_step(&mut self, ctx: &egui::Context) {
        self.created_hint(ctx);
        if SHOW_GRID.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.to_grid();
        }
        // frames and base stay in memory only while the tool is open (released on leaving it or another photo); how
        // much, by the operating mode
        let tool_open = self.tool_rail_active() == crate::workspace::ToolTab::Merge && self.show_right && !self.in_grid();
        let live = self.shown_merged().filter(|_| tool_open).and_then(|(_, d)| d.merged.as_ref().map(|m| m.source.file));
        rawdev::merged::set_memory_limit(if crate::perf_mode::tier_key() == "saving" { 512 << 20 } else { 2 << 30 });
        {
            static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
            let l = live.unwrap_or(-1);
            if LAST.swap(l, std::sync::atomic::Ordering::Relaxed) != l {
                crate::log(&format!("hdr: frames and base kept for {l} (released before: {:.0} MB)", rawdev::merged::live_bytes() as f64 / 1e6));
            }
        }
        rawdev::merged::set_live(live);
        let ready: Vec<Ready> = std::mem::take(&mut *READY.lock().unwrap_or_else(|e| e.into_inner()));
        for r in ready {
            let still = crate::hdr_ui::wanted(r.file).is_some_and(|s| s.key == r.key) && self.shown_merged().is_some_and(|(_, d)| d.merged.as_ref().is_some_and(|m| m.source.file == r.file));
            if !still {
                continue;
            }
            self.swap_loaded(r.loaded);
            if let Some(ms) = since_change(&r.key) {
                crate::log(&format!("hdr: {} in place {ms:.0} ms after the change", if r.with_full { "full resolution" } else { "preview" }));
            }
            if !r.with_full {
                let waiting = WAITING_FULL.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some((file, key, full, meta)) = waiting.filter(|w| w.0 == r.file && w.1 == r.key) {
                    self.merge_pixels(file, key, None, Some(full), meta);
                }
            }
        }
        // the tool is open: the frames are read ahead, so the first change merges without reading the RAWs
        if let Some((_, doc)) = self.shown_merged() {
            if self.tool_rail_active() == crate::workspace::ToolTab::Merge && self.show_right {
                if let Some(m) = doc.merged.as_ref().filter(|m| !m.pending) {
                    rawdev::merged::warm(&m.source);
                }
            }
        }
        // The photo shown is not the result it should show and nothing is on its way: it is computed now. Either its
        // merge is not there yet (the reference stands in: cache gone, a new merge), or the document is an older one
        // (kept from before a change that was computed while another photo was shown) – then the result wanted is
        // put in its place (from the cache when it is there). Never a loop: a result that failed is not started again
        // by itself (the tool says why and offers "Neu berechnen"), and the same step is taken again after two seconds
        // at the earliest.
        if let Some((_, doc)) = self.shown_merged() {
            let m = doc.merged.as_ref().expect("merged");
            let file = m.source.file;
            let idle = !rawdev::merged::jobs().iter().any(|j| j.file == file) && !crate::hdr_ui::job_alive(file) && BUILDING.load(std::sync::atomic::Ordering::SeqCst) == 0 && READY.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
            let info = self.merge_info();
            // a result that failed for a missing source is tried again once every source is back
            if info.as_ref().is_some_and(|i| i.missing.is_empty()) && crate::hdr_ui::sources_back(file) {
                asked(file);
            }
            let stored = info.and_then(|i| i.stored_key);
            let target = match crate::hdr_ui::wanted(file) {
                Some(w) if w.key != m.source.key || m.pending => Some(w),
                Some(_) => None,
                // nothing is wanted (this session did not change the photo, or its last change ended without a result):
                // the stored recipe counts. A document of an older recipe follows it – unless the user cancelled that
                // result or it failed: then the tool says "Nicht aktuell" and offers "Neu berechnen".
                None if stored.as_ref().is_some_and(|k| *k == m.source.key) => m.pending.then(|| m.source.clone()),
                None if idle && stored.as_ref().is_some_and(|k| !crate::hdr_ui::stopped(file, k)) => self.current.as_ref().and_then(|c| rawdev::merged::lookup(&c.loaded.path)).filter(|s| s.unreadable.is_none()),
                None => None,
            };
            if let Some(src) = target.filter(|_| idle) {
                let step = (file, m.source.key.clone(), src.key.clone());
                // (the same step again only after a while: its pixels may have arrived while the photo was not shown –
                // a result that cannot be computed is remembered as failed and not tried at all)
                let again = LAST_AUTO.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|l| l.0 == step && crate::hdr_ui::clock::since(l.1) < std::time::Duration::from_secs(2));
                let possible = rawdev::merged::cached(&src.key) || src.missing().is_empty();
                if !again && possible && !crate::hdr_ui::stopped(file, &src.key) {
                    *LAST_AUTO.lock().unwrap_or_else(|e| e.into_inner()) = Some((step, crate::hdr_ui::clock::now()));
                    let name = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.name.clone()).unwrap_or_default();
                    crate::hdr_ui::run_update(src, name);
                }
            }
        }
    }

    /// The hint after a merge was created (bottom right, like the update card): "HDR erstellt", "Rückgängig",
    /// "Quellen zeigen"; it goes after a few seconds.
    pub(crate) fn created_hint(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let Some((file, at)) = *CREATED.lock().unwrap_or_else(|e| e.into_inner()) else { return };
        if now - at > HINT_SECS || ctx.memory(|m| m.top_modal_layer()).is_some() {
            if now - at > HINT_SECS {
                *CREATED.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let mut undo = false;
        let mut sources = false;
        let mut close = false;
        egui::Area::new(egui::Id::new("hdr-created-hint")).order(egui::Order::Foreground).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -self.message_bottom())).show(ctx, |ui| {
            egui::Frame::new().fill(egui::Color32::from_rgb(0x26, 0x26, 0x26)).stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(0x44))).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.label(egui::RichText::new(crate::tr("HDR erstellt")).font(crate::lr::semibold(12.5)).color(egui::Color32::from_gray(0xee)));
                    undo = ui.add(egui::Button::new(crate::tr("Rückgängig")).corner_radius(0.0)).clicked();
                    sources = ui.add(egui::Button::new(crate::tr("Quellen zeigen")).corner_radius(0.0)).clicked();
                    let (xr, x) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                    crate::lr::paint_icon(ui.painter(), xr.shrink(3.0), crate::lr::Icon::X, if x.hovered() { egui::Color32::WHITE } else { crate::lr::TEXT_DIM });
                    crate::lr::hover_tip(ui, &x, crate::tr("Hinweis schließen"));
                    close = x.clicked();
                });
            });
        });
        if undo || sources || close {
            *CREATED.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        if undo {
            let a = self.lib.undo_merge(file);
            self.library_actions(a);
        }
        if sources {
            let image = self.entries.iter().find(|e| e.file == file).map(|e| e.image);
            if let Some(image) = image {
                let mut a = Vec::new();
                self.lib.show_sources(image, &mut a);
                self.library_actions(a);
            }
        }
    }

    /// Put a new document of the open photo in place without opening it again: the picture, zoom and pan stay; the
    /// preview is rendered again, the sharp crop is kept on screen until its successor is there.
    fn swap_loaded(&mut self, loaded: crate::jobs::Loaded) {
        let Some(c) = self.current.as_mut() else { return };
        if c.loaded.path != loaded.path {
            return;
        }
        c.loaded = loaded;
        // the sharp image of the old pixels stays below until the new one is rendered (same settings, see main.rs)
        if let Some(d) = c.detail.take() {
            c.detail_prev = Some(d);
        }
        c.detail_grain = None;
        c.detail_hover = None;
        c.detail_left = None;
        c.clip_tex = None;
        // whatever was computed from the old document is not wanted any more (its results are discarded by origin)
        self.full_job = None;
        self.detail_pending = false;
        self.edit_generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.panes.invalidate();
        self.display.clear();
        self.rendered_for = None;
        // the picture kept as "the development seen last" is of the old pixels (the same settings give another
        // picture now): the next render stores it anew
        self.developed_stored = None;
    }
}
